//! The incident lifecycle: open -> acknowledged -> resolved, and reopen.
//! Every state change goes through this module, which writes it and its
//! timeline event (`incident_events`) on the caller's connection, so both
//! land in the caller's transaction.

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use crate::{core::validate::is_invisible_or_bidi_char, uptime::checker::is_unsafe_in_text};

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
    /// `None` or an id of another project: the project's default policy.
    pub escalation_policy_id: Option<Uuid>,
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

// Restarts escalation at the first step of a policy, due that step's delay
// after $3: the requested policy $2 (none requested: the incident's own) if
// it is still one of the project's, else the project's default.
const ARM: &str = "update incidents i set
        escalation_policy_id = p.id, escalation_step = 0, escalation_repeat = 0,
        escalation_on_fallback = false, escalation_notified = 0, escalation_legacy = false,
        next_escalation_at = $3 + make_interval(mins => s.delay_minutes)
    from escalation_policies p
    join escalation_steps s on s.policy_id = p.id and s.position = 0
    where i.id = $1
      and p.id = coalesce(
          (select id from escalation_policies
           where id = coalesce($2, i.escalation_policy_id) and project_id = i.project_id),
          (select id from escalation_policies where project_id = i.project_id and is_default))";

/// Arms escalation (see `ARM`) from `base`.
async fn arm(
    conn: &mut PgConnection,
    incident_id: Uuid,
    requested_policy: Option<Uuid>,
    base: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(ARM)
        .bind(incident_id)
        .bind(requested_policy)
        .bind(base)
        .execute(conn)
        .await?;
    Ok(())
}

/// Opens an incident unless the source already has an unresolved one (then
/// `None`). Records an `opened` event (a manual incident's names its
/// creator) and arms escalation on the requested policy (else the project's
/// default) from `started_at`, in the same transaction.
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
        arm(conn, id, req.escalation_policy_id, req.started_at).await?;
    }
    Ok(id)
}

/// open -> acknowledged, by `actor` at `at`, which stops escalation. `false`
/// when it was not open.
pub async fn acknowledge(
    conn: &mut PgConnection,
    incident_id: Uuid,
    actor: Uuid,
    at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "update incidents set status = 'acknowledged', acknowledged_at = $3, acknowledged_by = $2,
                next_escalation_at = null
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

/// Most chars of a resolution reason (R30).
pub const MAX_REASON_CHARS: usize = 200;

/// open|acknowledged -> resolved at `at`, which stops escalation. `actor:
/// None` = automatic. Queues a `resolved` notice for every target the
/// current escalation generation has notified (`QUEUE_RESOLVED`; for a
/// legacy incident, every verified member) and cancels the step
/// notifications still queued. `false` when it was already resolved (or does
/// not exist).
pub async fn resolve(
    conn: &mut PgConnection,
    incident_id: Uuid,
    actor: Option<Uuid>,
    at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    resolve_with_reason(conn, incident_id, actor, at, None).await
}

// One `resolved` notice per target with a sent step notification in the
// current generation ($2): its rounds start `g{generation}p`, and the `p`
// keeps g1 from matching g10.
const QUEUE_RESOLVED: &str = "insert into notification_deliveries
        (incident_id, event, round, target_key, user_id, channel_id)
    select distinct incident_id, 'resolved', 'resolved:g' || $2, target_key, user_id, channel_id
    from notification_deliveries
    where incident_id = $1 and event = 'triggered' and status = 'sent'
      and round like 'g' || $2 || 'p%'
    on conflict on constraint notification_deliveries_target_unique do nothing";

// A legacy incident (unresolved at deploy, R6) was emailed to every member by
// the old path, which recorded no deliveries: each verified member is told.
const QUEUE_RESOLVED_LEGACY: &str = "insert into notification_deliveries
        (incident_id, event, round, target_key, user_id)
    select i.id, 'resolved', 'resolved:g' || $2, 'email:' || m.user_id, m.user_id
    from incidents i
    join project_members m on m.project_id = i.project_id
    join users u on u.id = m.user_id and u.email_verified_at is not null
    where i.id = $1
    on conflict on constraint notification_deliveries_target_unique do nothing";

// Step notifications still waiting (a retry backoff, a storm cap deferral) are
// cancelled now, not when claimed: a reopen makes the incident open again, and
// the drain would then send this generation's page next to the new one's.
const CANCEL_QUEUED_PAGES: &str = "update notification_deliveries set status = 'cancelled'
    where incident_id = $1 and event = 'triggered' and status = 'pending'";

/// [`resolve`], with the `resolved` event carrying `reason` as its message
/// and `metadata.reason` (R30).
async fn resolve_with_reason(
    conn: &mut PgConnection,
    incident_id: Uuid,
    actor: Option<Uuid>,
    at: DateTime<Utc>,
    reason: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let resolved: Option<(i32, bool)> = sqlx::query_as(
        "update incidents set status = 'resolved', resolved_at = $3, resolved_by = $2,
                next_escalation_at = null
         where id = $1 and status <> 'resolved'
         returning escalation_generation, escalation_legacy",
    )
    .bind(incident_id)
    .bind(actor)
    .bind(at)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((generation, legacy)) = resolved else {
        return Ok(false);
    };
    let metadata = reason.map_or_else(|| json!({}), |r| json!({ "reason": r }));
    record_event(conn, incident_id, EventKind::Resolved, actor, reason, metadata).await?;
    sqlx::query(if legacy { QUEUE_RESOLVED_LEGACY } else { QUEUE_RESOLVED })
        .bind(incident_id)
        .bind(generation)
        .execute(&mut *conn)
        .await?;
    sqlx::query(CANCEL_QUEUED_PAGES).bind(incident_id).execute(&mut *conn).await?;
    Ok(true)
}

/// Resolves the source's unresolved incident, automatically (no resolver).
/// Returns its id, or `None` when it has none (always for `Manual`).
pub async fn resolve_source(
    conn: &mut PgConnection,
    source: &IncidentSource,
    at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    resolve_source_inner(conn, source, at, None).await
}

/// R17/R30: [`resolve_source`] with a reason, cut to [`MAX_REASON_CHARS`]
/// with every unsafe char (control, invisible, text direction) made a
/// space. Queues the normal `resolved` notices, which carry the reason.
pub async fn resolve_source_with_reason(
    conn: &mut PgConnection,
    source: &IncidentSource,
    at: DateTime<Utc>,
    reason: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    resolve_source_inner(conn, source, at, Some(&clean_reason(reason))).await
}

/// At most [`MAX_REASON_CHARS`], one line, every unsafe char a space.
fn clean_reason(reason: &str) -> String {
    reason
        .chars()
        .take(MAX_REASON_CHARS)
        .map(|c| if is_unsafe_in_text(c) || is_invisible_or_bidi_char(c) { ' ' } else { c })
        .collect()
}

async fn resolve_source_inner(
    conn: &mut PgConnection,
    source: &IncidentSource,
    at: DateTime<Utc>,
    reason: Option<&str>,
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
    Ok(resolve_with_reason(conn, id, None, at, reason).await?.then_some(id))
}

/// resolved -> open, by `actor`: the resolution and the acknowledgement are
/// cleared, and escalation restarts from `at` at the first step of the
/// incident's own policy (the project's default if it was deleted), in a new
/// generation. Refused while another incident of the same source is
/// unresolved.
pub async fn reopen(
    conn: &mut PgConnection,
    incident_id: Uuid,
    actor: Uuid,
    at: DateTime<Utc>,
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
                acknowledged_at = null, acknowledged_by = null,
                escalation_generation = escalation_generation + 1
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
    arm(conn, incident_id, None, at).await?;
    Ok(Reopen::Reopened)
}

/// Automatic reopen: the source's latest incident, if it was resolved less
/// than `window_secs` before `at`, goes back to `open` with the new title and
/// cause (details, resolver and acknowledgement cleared; id and `started_at`
/// kept) and gets a `reopened` event with no actor; escalation restarts from
/// `at` as for [`reopen`]. Returns its id, or `None` when there is no such
/// incident or the source already has an unresolved one.
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
             title = $4, cause = $5, details = null,
             escalation_generation = i.escalation_generation + 1
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
        arm(conn, id, None, at).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_is_one_line_and_cut_at_its_char_limit() {
        let at = "é".repeat(MAX_REASON_CHARS);
        assert_eq!(clean_reason(&at), at);
        assert_eq!(clean_reason(&format!("{at}x")), at);
        assert_eq!(clean_reason("alert\ndisabled\u{202E}!"), "alert disabled !");
    }
}
