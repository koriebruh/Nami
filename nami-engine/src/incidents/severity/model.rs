use serde::Serialize;
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

/// One row of `incident_severities`. `rank` 1 is the most severe.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Severity {
    pub id: Uuid,
    pub name: String,
    pub rank: i32,
    pub is_default: bool,
}

/// The project's severities, most severe first.
pub async fn list_for_project(pool: &PgPool, project_id: Uuid) -> Result<Vec<Severity>, sqlx::Error> {
    sqlx::query_as(
        "select id, name, rank, is_default from incident_severities
         where project_id = $1 order by rank",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// Whether `severity_id` is one of the project's severities.
pub async fn belongs_to<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    severity_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists (select 1 from incident_severities where id = $1 and project_id = $2)",
    )
    .bind(severity_id)
    .bind(project_id)
    .fetch_one(executor)
    .await
}
