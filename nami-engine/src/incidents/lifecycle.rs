//! The incident lifecycle: open -> acknowledged -> resolved, and reopen.
//! Every state change goes through this module, which writes it and its
//! timeline event (`incident_events`) on the caller's connection, so both
//! land in the caller's transaction.

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

/// What an incident belongs to. Each source other than `Manual` has at most
/// one unresolved incident (the `incidents_one_unresolved_per_*` indexes).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IncidentSource {
    Monitor(Uuid),
    Heartbeat(Uuid),
    LogAlert(Uuid),
    Manual { created_by: Uuid },
}

impl IncidentSource {
    /// The `incidents.source` value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Monitor(_) => "monitor",
            Self::Heartbeat(_) => "heartbeat",
            Self::LogAlert(_) => "log_alert",
            Self::Manual { .. } => "manual",
        }
    }

    /// `(monitor_id, heartbeat_id, log_alert_id)`, with only this source's
    /// column set. A query matching `monitor_id = $1 or heartbeat_id = $2 or
    /// log_alert_id = $3` then finds this source's incidents, and none for
    /// `Manual`.
    fn keys(&self) -> (Option<Uuid>, Option<Uuid>, Option<Uuid>) {
        match *self {
            Self::Monitor(id) => (Some(id), None, None),
            Self::Heartbeat(id) => (None, Some(id), None),
            Self::LogAlert(id) => (None, None, Some(id)),
            Self::Manual { .. } => (None, None, None),
        }
    }
}

/// The kind of a timeline event (`incident_events.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Opened,
    Acknowledged,
    Resolved,
    Reopened,
    Escalated,
    NotificationSent,
    NotificationFailed,
    Comment,
    PostmortemUpdated,
    IntegrationEvent,
    StepSkipped,
    EscalationFallback,
    EscalationExhausted,
    SeverityChanged,
    MetadataUpdated,
    TicketCreated,
}

impl EventKind {
    /// The stored, snake_case name, e.g. `"notification_sent"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Acknowledged => "acknowledged",
            Self::Resolved => "resolved",
            Self::Reopened => "reopened",
            Self::Escalated => "escalated",
            Self::NotificationSent => "notification_sent",
            Self::NotificationFailed => "notification_failed",
            Self::Comment => "comment",
            Self::PostmortemUpdated => "postmortem_updated",
            Self::IntegrationEvent => "integration_event",
            Self::StepSkipped => "step_skipped",
            Self::EscalationFallback => "escalation_fallback",
            Self::EscalationExhausted => "escalation_exhausted",
            Self::SeverityChanged => "severity_changed",
            Self::MetadataUpdated => "metadata_updated",
            Self::TicketCreated => "ticket_created",
        }
    }
}

/// A request to open an incident.
#[derive(Debug, Clone)]
pub struct OpenIncident<'a> {
    pub project_id: Uuid,
    pub source: IncidentSource,
    pub title: &'a str,
    pub cause: Option<&'a str>,
    /// `None` or an id of another project: the project's default severity.
    pub severity_id: Option<Uuid>,
    pub metadata: Option<&'a serde_json::Map<String, serde_json::Value>>,
    pub started_at: DateTime<Utc>,
}

/// The outcome of [`reopen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reopen {
    Reopened,
    /// The incident is open or acknowledged.
    NotResolved,
    /// Another incident of the same source is unresolved.
    SourceHasUnresolvedIncident,
    NotFound,
}

// One statement per source, because each names its own conflict target. $5
// is the requested severity, kept only if it belongs to the project.
const OPEN_MONITOR: &str = "insert into incidents
        (project_id, monitor_id, source, title, cause, severity_id, metadata, started_at)
    values ($1, $2, 'monitor', $3, $4,
        coalesce((select id from incident_severities where id = $5 and project_id = $1),
                 (select id from incident_severities where project_id = $1 and is_default)),
        coalesce($6, '{}'::jsonb), $7)
    on conflict (monitor_id) where status <> 'resolved' do nothing
    returning id";
const OPEN_HEARTBEAT: &str = "insert into incidents
        (project_id, heartbeat_id, source, title, cause, severity_id, metadata, started_at)
    values ($1, $2, 'heartbeat', $3, $4,
        coalesce((select id from incident_severities where id = $5 and project_id = $1),
                 (select id from incident_severities where project_id = $1 and is_default)),
        coalesce($6, '{}'::jsonb), $7)
    on conflict (heartbeat_id) where status <> 'resolved' do nothing
    returning id";
const OPEN_LOG_ALERT: &str = "insert into incidents
        (project_id, log_alert_id, source, title, cause, severity_id, metadata, started_at)
    values ($1, $2, 'log_alert', $3, $4,
        coalesce((select id from incident_severities where id = $5 and project_id = $1),
                 (select id from incident_severities where project_id = $1 and is_default)),
        coalesce($6, '{}'::jsonb), $7)
    on conflict (log_alert_id) where status <> 'resolved' do nothing
    returning id";
const OPEN_MANUAL: &str = "insert into incidents
        (project_id, created_by, source, title, cause, severity_id, metadata, started_at)
    values ($1, $2, 'manual', $3, $4,
        coalesce((select id from incident_severities where id = $5 and project_id = $1),
                 (select id from incident_severities where project_id = $1 and is_default)),
        coalesce($6, '{}'::jsonb), $7)
    returning id";

/// Opens an incident unless the source already has an unresolved one (then
/// `None`). Records an `opened` event (a manual incident's names its
/// creator) in the same transaction.
pub async fn open(
    conn: &mut PgConnection,
    req: &OpenIncident<'_>,
) -> Result<Option<Uuid>, sqlx::Error> {
    let (sql, key, actor) = match req.source {
        IncidentSource::Monitor(id) => (OPEN_MONITOR, id, None),
        IncidentSource::Heartbeat(id) => (OPEN_HEARTBEAT, id, None),
        IncidentSource::LogAlert(id) => (OPEN_LOG_ALERT, id, None),
        IncidentSource::Manual { created_by } => (OPEN_MANUAL, created_by, Some(created_by)),
    };
    let id: Option<Uuid> = sqlx::query_scalar(sql)
        .bind(req.project_id)
        .bind(key)
        .bind(req.title)
        .bind(req.cause)
        .bind(req.severity_id)
        .bind(req.metadata.map(sqlx::types::Json))
        .bind(req.started_at)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(id) = id {
        record_event(conn, id, EventKind::Opened, actor, None, json!({})).await?;
    }
    Ok(id)
}

/// open -> acknowledged, by `actor` at `at`. `false` when it was not open.
pub async fn acknowledge(
    conn: &mut PgConnection,
    incident_id: Uuid,
    actor: Uuid,
    at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "update incidents set status = 'acknowledged', acknowledged_at = $3, acknowledged_by = $2
         where id = $1 and status = 'open'",
    )
    .bind(incident_id)
    .bind(actor)
    .bind(at)
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1;
    if done {
        record_event(conn, incident_id, EventKind::Acknowledged, Some(actor), None, json!({}))
            .await?;
    }
    Ok(done)
}

/// open|acknowledged -> resolved at `at`. `actor: None` = automatic.
/// `false` when it was already resolved (or does not exist).
pub async fn resolve(
    conn: &mut PgConnection,
    incident_id: Uuid,
    actor: Option<Uuid>,
    at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "update incidents set status = 'resolved', resolved_at = $3, resolved_by = $2
         where id = $1 and status <> 'resolved'",
    )
    .bind(incident_id)
    .bind(actor)
    .bind(at)
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1;
    if done {
        record_event(conn, incident_id, EventKind::Resolved, actor, None, json!({})).await?;
    }
    Ok(done)
}

/// Resolves the source's unresolved incident, automatically (no resolver).
/// Returns its id, or `None` when it has none (always for `Manual`).
pub async fn resolve_source(
    conn: &mut PgConnection,
    source: &IncidentSource,
    at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    let (monitor, heartbeat, log_alert) = source.keys();
    let id: Option<Uuid> = sqlx::query_scalar(
        "select id from incidents
         where (monitor_id = $1 or heartbeat_id = $2 or log_alert_id = $3)
           and status <> 'resolved'
         for update",
    )
    .bind(monitor)
    .bind(heartbeat)
    .bind(log_alert)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(id) = id else {
        return Ok(None);
    };
    Ok(resolve(conn, id, None, at).await?.then_some(id))
}

/// resolved -> open, by `actor`: the resolution and the acknowledgement are
/// cleared. Refused while another incident of the same source is unresolved.
/// `_at` is the reopen time (escalation will restart from it).
pub async fn reopen(
    conn: &mut PgConnection,
    incident_id: Uuid,
    actor: Uuid,
    _at: DateTime<Utc>,
) -> Result<Reopen, sqlx::Error> {
    let status: Option<String> =
        sqlx::query_scalar("select status from incidents where id = $1 for update")
            .bind(incident_id)
            .fetch_optional(&mut *conn)
            .await?;
    match status.as_deref() {
        None => return Ok(Reopen::NotFound),
        Some("resolved") => {}
        Some(_) => return Ok(Reopen::NotResolved),
    }
    // A savepoint: the unique index refuses the update when the source has
    // another unresolved incident, and the caller's transaction must outlive
    // that error.
    let mut savepoint = conn.begin().await?;
    let updated = sqlx::query(
        "update incidents set status = 'open', resolved_at = null, resolved_by = null,
                acknowledged_at = null, acknowledged_by = null
         where id = $1",
    )
    .bind(incident_id)
    .execute(&mut *savepoint)
    .await;
    match updated {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            savepoint.rollback().await?;
            return Ok(Reopen::SourceHasUnresolvedIncident);
        }
        updated => {
            updated?;
        }
    }
    savepoint.commit().await?;
    record_event(conn, incident_id, EventKind::Reopened, Some(actor), None, json!({})).await?;
    Ok(Reopen::Reopened)
}

/// Automatic reopen: the source's latest incident, if it was resolved less
/// than `window_secs` before `at`, goes back to `open` with the new title and
/// cause (details, resolver and acknowledgement cleared; id and `started_at`
/// kept) and gets a `reopened` event with no actor. Returns its id, or `None`
/// when there is no such incident or the source already has an unresolved one.
pub async fn reopen_source_within(
    conn: &mut PgConnection,
    source: &IncidentSource,
    window_secs: i32,
    title: &str,
    cause: Option<&str>,
    at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    let (monitor, heartbeat, log_alert) = source.keys();
    // `latest` and `not exists` read the statement's snapshot, so a manual
    // reopen committed meanwhile is invisible to them: `i.status` is
    // rechecked on the locked row, and the unique index (another incident of
    // the source reopened) is caught in a savepoint so the caller's
    // transaction survives it.
    let mut savepoint = conn.begin().await?;
    let reopened = sqlx::query_scalar::<_, Uuid>(
        "update incidents i
         set status = 'open', resolved_at = null, resolved_by = null,
             acknowledged_at = null, acknowledged_by = null,
             title = $4, cause = $5, details = null
         from (select id, status, resolved_at from incidents
               where monitor_id = $1 or heartbeat_id = $2 or log_alert_id = $3
               order by started_at desc
               limit 1) latest
         where i.id = latest.id
           and i.status = 'resolved'
           and latest.resolved_at > $6 - make_interval(secs => $7)
           and not exists (select 1 from incidents
                           where (monitor_id = $1 or heartbeat_id = $2 or log_alert_id = $3)
                             and status <> 'resolved')
         returning i.id",
    )
    .bind(monitor)
    .bind(heartbeat)
    .bind(log_alert)
    .bind(title)
    .bind(cause)
    .bind(at)
    .bind(window_secs)
    .fetch_optional(&mut *savepoint)
    .await;
    let reopened = match reopened {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            savepoint.rollback().await?;
            return Ok(None);
        }
        reopened => reopened?,
    };
    savepoint.commit().await?;
    if let Some(id) = reopened {
        record_event(conn, id, EventKind::Reopened, None, None, json!({})).await?;
    }
    Ok(reopened)
}

/// Adds `actor`'s comment to the timeline and returns the event id. The
/// caller checks the body's length and characters.
pub async fn comment(
    conn: &mut PgConnection,
    incident_id: Uuid,
    actor: Uuid,
    body: &str,
) -> Result<i64, sqlx::Error> {
    record_event(conn, incident_id, EventKind::Comment, Some(actor), Some(body), json!({})).await
}

/// Appends one timeline event and returns its id. `metadata` must be a JSON
/// object and `message` at most 10 KiB (both checked by the table).
pub async fn record_event(
    conn: &mut PgConnection,
    incident_id: Uuid,
    kind: EventKind,
    actor: Option<Uuid>,
    message: Option<&str>,
    metadata: serde_json::Value,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "insert into incident_events (incident_id, kind, actor_id, message, metadata)
         values ($1, $2, $3, $4, $5)
         returning id",
    )
    .bind(incident_id)
    .bind(kind.as_str())
    .bind(actor)
    .bind(message)
    .bind(metadata)
    .fetch_one(&mut *conn)
    .await
}
