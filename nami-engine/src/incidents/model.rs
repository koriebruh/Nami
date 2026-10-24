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
    pub log_alert_id: Option<Uuid>,
    /// Name of the monitor, heartbeat or log alert the incident belongs to.
    pub source_name: Option<String>,
    pub title: String,
    pub cause: Option<String>,
    /// Output of the heartbeat ping that opened the incident (sanitized).
    pub details: Option<String>,
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
        "select i.id, i.project_id, i.monitor_id, i.heartbeat_id, i.log_alert_id,
                coalesce(m.name, h.name, la.name) as source_name, i.title, i.cause, i.details,
                i.status, i.started_at, i.resolved_at
         from incidents i
         left join monitors m on m.id = i.monitor_id
         left join heartbeats h on h.id = i.heartbeat_id
         left join log_alerts la on la.id = i.log_alert_id
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

/// Log-alert counterpart of [`open_for_monitor`], guarded by
/// `incidents_one_open_per_log_alert`. Never reopens a resolved incident
/// (unlike [`open_for_heartbeat`]). Called only through
/// `logs::alert::open_alert_incident`.
pub async fn open_for_log_alert(
    conn: &mut PgConnection,
    project_id: Uuid,
    log_alert_id: Uuid,
    title: &str,
    cause: Option<&str>,
    started_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "insert into incidents (project_id, log_alert_id, title, cause, started_at)
         values ($1, $2, $3, $4, $5)
         on conflict (log_alert_id) where status = 'open' do nothing
         returning id",
    )
    .bind(project_id)
    .bind(log_alert_id)
    .bind(title)
    .bind(cause)
    .bind(started_at)
    .fetch_optional(&mut *conn)
    .await
}

/// Log-alert counterpart of [`resolve_for_monitor`]. Called only through
/// `logs::alert::resolve_alert_incident`.
pub async fn resolve_for_log_alert(
    conn: &mut PgConnection,
    log_alert_id: Uuid,
    resolved_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "update incidents set status = 'resolved', resolved_at = $2
         where log_alert_id = $1 and status = 'open'
         returning id",
    )
    .bind(log_alert_id)
    .bind(resolved_at)
    .fetch_optional(&mut *conn)
    .await
}

/// A heartbeat incident resolved less than this long ago is reopened, not
/// followed by a new one, when the heartbeat goes down again (see
/// [`open_for_heartbeat`]). Same length as the alert flap window
/// (`uptime::alert_throttle::ALERT_FLAP_WINDOW`).
pub const HEARTBEAT_REOPEN_WINDOW_SECS: i32 = 5 * 60;

/// Heartbeat counterpart of [`open_for_monitor`], guarded by
/// `incidents_one_open_per_heartbeat`, with flap control: when the
/// heartbeat's latest incident was resolved less than
/// [`HEARTBEAT_REOPEN_WINDOW_SECS`] before `started_at`, that incident is
/// reopened (status open, `resolved_at` cleared, title and cause replaced,
/// details cleared for the caller to attach the new ones; its id and
/// `started_at` stay) and its id returned, instead of a new row. A job that
/// fails and succeeds in turn (`/fail`, then `/0`, as fast as the ping rate
/// limit allows) therefore keeps one incident row, however long it flaps,
/// and its downtime counts from the first failure until it stays up. Called
/// under the heartbeat's row lock (the ping and the sweep both hold it).
pub async fn open_for_heartbeat(
    conn: &mut PgConnection,
    project_id: Uuid,
    heartbeat_id: Uuid,
    title: &str,
    cause: Option<&str>,
    started_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    let reopened: Option<Uuid> = sqlx::query_scalar(
        "update incidents i
         set status = 'open', resolved_at = null, title = $2, cause = $3, details = null
         from (select id, status, resolved_at from incidents
               where heartbeat_id = $1
               order by started_at desc
               limit 1) latest
         where i.id = latest.id
           and latest.status = 'resolved'
           and latest.resolved_at > $4 - make_interval(secs => $5)
         returning i.id",
    )
    .bind(heartbeat_id)
    .bind(title)
    .bind(cause)
    .bind(started_at)
    .bind(HEARTBEAT_REOPEN_WINDOW_SECS)
    .fetch_optional(&mut *conn)
    .await?;
    if reopened.is_some() {
        return Ok(reopened);
    }
    sqlx::query_scalar(
        "insert into incidents (project_id, heartbeat_id, title, cause, started_at)
         values ($1, $2, $3, $4, $5)
         on conflict (heartbeat_id) where status = 'open' do nothing
         returning id",
    )
    .bind(project_id)
    .bind(heartbeat_id)
    .bind(title)
    .bind(cause)
    .bind(started_at)
    .fetch_optional(&mut *conn)
    .await
}

/// Heartbeat counterpart of [`resolve_for_monitor`].
pub async fn resolve_for_heartbeat(
    conn: &mut PgConnection,
    heartbeat_id: Uuid,
    resolved_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "update incidents set status = 'resolved', resolved_at = $2
         where heartbeat_id = $1 and status = 'open'
         returning id",
    )
    .bind(heartbeat_id)
    .bind(resolved_at)
    .fetch_optional(&mut *conn)
    .await
}

/// Stores the output of the ping that opened (or reopened) the incident.
pub async fn attach_details(
    conn: &mut PgConnection,
    incident_id: Uuid,
    details: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("update incidents set details = $2 where id = $1")
        .bind(incident_id)
        .bind(details)
        .execute(&mut *conn)
        .await?;
    Ok(())
}
