//! Monitor groups: named, ordered buckets for a project's monitors
//! (`monitors.group_id`). The monitors list stays flat; clients group it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::{ProjectQuota, validate_name};
use crate::error::AppError;

pub const MAX_POSITION: i32 = 1_000_000;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct MonitorGroup {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    /// Sort key: groups list by position, then name.
    pub position: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
    #[serde(default)]
    pub position: i32,
}

#[derive(Debug, Default, Deserialize)]
pub struct PatchGroupRequest {
    pub name: Option<String>,
    pub position: Option<i32>,
}

/// The validated (name, position) of a group.
pub fn validate(name: &str, position: i32) -> Result<(String, i32), String> {
    let name = validate_name(name)?;
    if !(0..=MAX_POSITION).contains(&position) {
        return Err(format!("position must be between 0 and {MAX_POSITION}"));
    }
    Ok((name, position))
}

/// Stores a group, unless the project already holds
/// [`ProjectQuota::MAX_ROWS`] (a 400); a project that does not exist is a 404.
pub async fn insert_group(
    pool: &PgPool,
    project_id: Uuid,
    name: &str,
    position: i32,
) -> Result<MonitorGroup, AppError> {
    let mut tx = pool.begin().await?;
    ProjectQuota::MonitorGroups
        .check(&mut tx, project_id)
        .await?;
    let group = sqlx::query_as(
        "insert into monitor_groups (project_id, name, position) values ($1, $2, $3) returning *",
    )
    .bind(project_id)
    .bind(name)
    .bind(position)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(group)
}

pub async fn list_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<MonitorGroup>, sqlx::Error> {
    sqlx::query_as("select * from monitor_groups where project_id = $1 order by position, name, id")
        .bind(project_id)
        .fetch_all(pool)
        .await
}

/// Scoped by both ids: another project's group is not found (404).
pub async fn find_in_project(
    pool: &PgPool,
    project_id: Uuid,
    group_id: Uuid,
) -> Result<Option<MonitorGroup>, sqlx::Error> {
    sqlx::query_as("select * from monitor_groups where id = $1 and project_id = $2")
        .bind(group_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// True when `group_id` is a group of `project_id`.
pub async fn belongs_to<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    group_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from monitor_groups where id = $1 and project_id = $2)",
    )
    .bind(group_id)
    .bind(project_id)
    .fetch_one(executor)
    .await
}

/// Merge-validate-write under the row's lock.
pub async fn update_in_project(
    pool: &PgPool,
    project_id: Uuid,
    group_id: Uuid,
    patch: PatchGroupRequest,
) -> Result<Option<MonitorGroup>, AppError> {
    let mut tx = pool.begin().await?;
    let current: Option<MonitorGroup> =
        sqlx::query_as("select * from monitor_groups where id = $1 and project_id = $2 for update")
            .bind(group_id)
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let (name, position) = validate(
        patch.name.as_deref().unwrap_or(&current.name),
        patch.position.unwrap_or(current.position),
    )
    .map_err(AppError::Validation)?;
    let updated: MonitorGroup = sqlx::query_as(
        "update monitor_groups set name = $3, position = $4 where id = $1 and project_id = $2 returning *",
    )
    .bind(group_id)
    .bind(project_id)
    .bind(&name)
    .bind(position)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(updated))
}

/// Deleting a group ungroups its monitors (`on delete set null (group_id)`).
pub async fn delete_in_project(
    pool: &PgPool,
    project_id: Uuid,
    group_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from monitor_groups where id = $1 and project_id = $2")
        .bind(group_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
