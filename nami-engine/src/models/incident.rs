use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// One row of `GET /projects/{id}/incidents`.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct IncidentListItem {
    pub id: Uuid,
    pub project_id: Uuid,
    pub monitor_id: Option<Uuid>,
    pub heartbeat_id: Option<Uuid>,
    /// Name of the monitor the incident belongs to. Heartbeat incidents
    /// arrive in Task 6; until then `heartbeat_id` is always null and every
    /// incident has a monitor.
    pub source_name: Option<String>,
    pub title: String,
    pub cause: Option<String>,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// Newest first, capped at 200. `status` and `monitor_id` are optional
/// filters (`None` = no filter).
pub async fn list_for_project(
    pool: &PgPool,
    project_id: Uuid,
    status: Option<&str>,
    monitor_id: Option<Uuid>,
) -> Result<Vec<IncidentListItem>, sqlx::Error> {
    sqlx::query_as(
        "select i.id, i.project_id, i.monitor_id, null::uuid as heartbeat_id,
                m.name as source_name, i.title, i.cause, i.status, i.started_at, i.resolved_at
         from incidents i
         left join monitors m on m.id = i.monitor_id
         where i.project_id = $1
           and ($2::text is null or i.status = $2)
           and ($3::uuid is null or i.monitor_id = $3)
         order by i.started_at desc
         limit 200",
    )
    .bind(project_id)
    .bind(status)
    .bind(monitor_id)
    .fetch_all(pool)
    .await
}

/// Opens an incident for the monitor unless one is already open (the
/// `incidents_one_open_per_monitor` index turns a duplicate into a no-op).
/// Returns the new incident's id, or `None` if one was already open.
pub async fn open_for_monitor(
    conn: &mut PgConnection,
    project_id: Uuid,
    monitor_id: Uuid,
    title: &str,
    cause: Option<&str>,
    started_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "insert into incidents (project_id, monitor_id, title, cause, started_at)
         values ($1, $2, $3, $4, $5)
         on conflict (monitor_id) where status = 'open' do nothing
         returning id",
    )
    .bind(project_id)
    .bind(monitor_id)
    .bind(title)
    .bind(cause)
    .bind(started_at)
    .fetch_optional(&mut *conn)
    .await
}

/// Resolves the monitor's open incident, if there is one. Returns its id.
pub async fn resolve_for_monitor(
    conn: &mut PgConnection,
    monitor_id: Uuid,
    resolved_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "update incidents set status = 'resolved', resolved_at = $2
         where monitor_id = $1 and status = 'open'
         returning id",
    )
    .bind(monitor_id)
    .bind(resolved_at)
    .fetch_optional(&mut *conn)
    .await
}
