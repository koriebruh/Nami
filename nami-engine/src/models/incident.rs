use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

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
