//! Saved explorer views of a project's logs.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{core::error::AppError, logs::view::saved_query::ViewQuery};

/// Most saved views a project may hold.
pub const MAX_VIEWS_PER_PROJECT: i64 = 200;

#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct LogView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub query: sqlx::types::Json<ViewQuery>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Short-lived return value; boxing would change the interface later tasks use.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum CreatedView {
    Created(LogView),
    ProjectFull,
    NameTaken,
    NoProject,
}

// sqlx takes only `&'static str` SQL, so the column list is a macro.
macro_rules! columns {
    () => {
        "id, project_id, name, query, created_by, created_at, updated_at"
    };
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|code| code == "23505")
}

pub async fn list_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<LogView>, sqlx::Error> {
    sqlx::query_as(concat!(
        "select ",
        columns!(),
        " from log_views where project_id = $1 order by name"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await
}

pub async fn get(
    pool: &PgPool,
    project_id: Uuid,
    view_id: Uuid,
) -> Result<Option<LogView>, sqlx::Error> {
    sqlx::query_as(concat!(
        "select ",
        columns!(),
        " from log_views where id = $1 and project_id = $2"
    ))
    .bind(view_id)
    .bind(project_id)
    .fetch_optional(pool)
    .await
}

/// The project row is locked for the count and the insert, so concurrent
/// creates cannot both pass the cap.
pub async fn create(
    pool: &PgPool,
    project_id: Uuid,
    name: &str,
    query: &ViewQuery,
    created_by: Uuid,
) -> Result<CreatedView, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let locked: Option<Uuid> =
        sqlx::query_scalar("select id from projects where id = $1 for no key update")
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    if locked.is_none() {
        return Ok(CreatedView::NoProject);
    }
    let count: i64 = sqlx::query_scalar("select count(*) from log_views where project_id = $1")
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_VIEWS_PER_PROJECT {
        return Ok(CreatedView::ProjectFull);
    }
    let inserted = sqlx::query_as(concat!(
        "insert into log_views (project_id, name, query, created_by)
         values ($1, $2, $3, $4) returning ",
        columns!(),
        ""
    ))
    .bind(project_id)
    .bind(name)
    .bind(sqlx::types::Json(query))
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await;
    match inserted {
        Ok(view) => {
            tx.commit().await?;
            Ok(CreatedView::Created(view))
        }
        Err(e) if is_unique_violation(&e) => Ok(CreatedView::NameTaken),
        Err(e) => Err(e),
    }
}

/// `Ok(None)` = no such view in the project; `Err(Conflict)` = name taken.
pub async fn update(
    pool: &PgPool,
    project_id: Uuid,
    view_id: Uuid,
    name: Option<&str>,
    query: Option<&ViewQuery>,
) -> Result<Option<LogView>, AppError> {
    let result = sqlx::query_as(concat!(
        "update log_views set name = coalesce($3, name), query = coalesce($4, query),
                updated_at = now()
         where id = $1 and project_id = $2 returning ",
        columns!(),
        ""
    ))
    .bind(view_id)
    .bind(project_id)
    .bind(name)
    .bind(query.map(sqlx::types::Json))
    .fetch_optional(pool)
    .await;
    match result {
        Ok(view) => Ok(view),
        Err(e) if is_unique_violation(&e) => Err(AppError::Conflict(
            "a view with this name already exists".to_string(),
        )),
        Err(e) => Err(e.into()),
    }
}

pub async fn delete(pool: &PgPool, project_id: Uuid, view_id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from log_views where id = $1 and project_id = $2")
        .bind(view_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
