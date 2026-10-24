use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::incidents::lifecycle::{self, EventKind, IncidentSource, OpenIncident};

/// Longest incident title, in characters (one line).
pub const MAX_TITLE_CHARS: usize = 255;
/// Longest incident description (`cause`) and comment, in bytes.
pub const MAX_TEXT_BYTES: usize = 10_240;
/// Longest postmortem, in bytes (`incidents_postmortem_size`).
pub const MAX_POSTMORTEM_BYTES: usize = 102_400;
/// Page size of the incident list when the request names none.
pub const DEFAULT_LIST_LIMIT: i64 = 50;
/// Largest page size of the incident list.
pub const MAX_LIST_LIMIT: i64 = 100;
/// Most timeline events a detail carries (the latest ones).
pub const MAX_DETAIL_EVENTS: i64 = 1_000;

/// The `incidents.status` values.
pub const STATUSES: &[&str] = &["open", "acknowledged", "resolved"];
/// The `incidents.source` values (`incidents_source_known`).
pub const SOURCES: &[&str] = &[
    "monitor",
    "heartbeat",
    "log_alert",
    "metric_alert",
    "manual",
    "integration",
    "report_form",
];

// Columns and joins of `IncidentListItem`, shared by the list and the detail.
// A macro, because sqlx takes SQL as one literal.
macro_rules! item_columns {
    () => {
        "i.id, i.project_id, i.monitor_id, i.heartbeat_id, i.log_alert_id, i.source,
         coalesce(m.name, h.name, la.name) as source_name, i.title, i.cause, i.details,
         i.status, s.name as severity, s.rank as severity_rank, i.started_at,
         i.acknowledged_at, ack.email as acknowledged_by_email, i.resolved_at"
    };
}
macro_rules! item_joins {
    () => {
        "incidents i
         left join monitors m on m.id = i.monitor_id
         left join heartbeats h on h.id = i.heartbeat_id
         left join log_alerts la on la.id = i.log_alert_id
         left join incident_severities s on s.id = i.severity_id
         left join users ack on ack.id = i.acknowledged_by"
    };
}
macro_rules! event_select {
    () => {
        "select e.id, e.kind, e.actor_id, u.email as actor_email, e.message, e.metadata,
                e.created_at
         from incident_events e
         left join users u on u.id = e.actor_id"
    };
}

/// One row of `GET /projects/{id}/incidents`.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct IncidentListItem {
    pub id: Uuid,
    pub project_id: Uuid,
    pub monitor_id: Option<Uuid>,
    pub heartbeat_id: Option<Uuid>,
    pub log_alert_id: Option<Uuid>,
    /// One of [`SOURCES`].
    pub source: String,
    /// Name of the monitor, heartbeat or log alert the incident belongs to.
    pub source_name: Option<String>,
    pub title: String,
    pub cause: Option<String>,
    /// Output of the heartbeat ping that opened the incident (sanitized);
    /// text, never markup.
    pub details: Option<String>,
    /// One of [`STATUSES`].
    pub status: String,
    pub severity: Option<String>,
    pub severity_rank: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub acknowledged_by_email: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// Filters of the incident list. An empty `statuses` or `sources` matches
/// every value; `None` ids match every incident.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub statuses: Vec<String>,
    pub sources: Vec<String>,
    pub monitor_id: Option<Uuid>,
    pub heartbeat_id: Option<Uuid>,
    pub log_alert_id: Option<Uuid>,
    /// Only incidents after this one in the list order (a decoded cursor).
    pub before: Option<(DateTime<Utc>, Uuid)>,
    pub limit: i64,
}

/// The project's incidents matching `filter`, newest first (`started_at`,
/// then `id`, both descending), at most `filter.limit` rows.
pub async fn list(
    pool: &PgPool,
    project_id: Uuid,
    filter: &ListFilter,
) -> Result<Vec<IncidentListItem>, sqlx::Error> {
    let (before_at, before_id) = filter.before.unzip();
    sqlx::query_as(concat!(
        "select ",
        item_columns!(),
        " from ",
        item_joins!(),
        " where i.project_id = $1
           and (cardinality($2::text[]) = 0 or i.status = any($2))
           and (cardinality($3::text[]) = 0 or i.source = any($3))
           and ($4::uuid is null or i.monitor_id = $4)
           and ($5::uuid is null or i.heartbeat_id = $5)
           and ($6::uuid is null or i.log_alert_id = $6)
           and ($7::timestamptz is null or (i.started_at, i.id) < ($7, $8::uuid))
         order by i.started_at desc, i.id desc
         limit $9"
    ))
    .bind(project_id)
    .bind(&filter.statuses)
    .bind(&filter.sources)
    .bind(filter.monitor_id)
    .bind(filter.heartbeat_id)
    .bind(filter.log_alert_id)
    .bind(before_at)
    .bind(before_id)
    .bind(filter.limit)
    .fetch_all(pool)
    .await
}

/// The opaque list cursor after `item`: base64url of
/// `<started_at RFC 3339, microseconds>|<id>`.
pub fn encode_cursor(item: &IncidentListItem) -> String {
    let raw = format!(
        "{}|{}",
        item.started_at.to_rfc3339_opts(SecondsFormat::Micros, true),
        item.id
    );
    URL_SAFE_NO_PAD.encode(raw)
}

/// The position an [`encode_cursor`] string names, or `None` when it is not one.
pub fn decode_cursor(cursor: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let raw = String::from_utf8(URL_SAFE_NO_PAD.decode(cursor).ok()?).ok()?;
    let (at, id) = raw.split_once('|')?;
    Some((
        DateTime::parse_from_rfc3339(at).ok()?.with_timezone(&Utc),
        Uuid::parse_str(id).ok()?,
    ))
}

/// One timeline event of an incident.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct IncidentEvent {
    pub id: i64,
    pub kind: String,
    pub actor_id: Option<Uuid>,
    pub actor_email: Option<String>,
    pub message: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// One incident with its timeline (`GET /projects/{id}/incidents/{incident_id}`).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct IncidentDetail {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub item: IncidentListItem,
    pub severity_id: Option<Uuid>,
    pub metadata: serde_json::Value,
    pub postmortem: Option<String>,
    pub resolved_by_email: Option<String>,
    pub created_by_email: Option<String>,
    /// The latest [`MAX_DETAIL_EVENTS`] events, oldest first.
    #[sqlx(skip)]
    pub events: Vec<IncidentEvent>,
}

/// The project's incident with its timeline, or `None` when the project has
/// no such incident.
pub async fn detail(
    pool: &PgPool,
    project_id: Uuid,
    incident_id: Uuid,
) -> Result<Option<IncidentDetail>, sqlx::Error> {
    let detail: Option<IncidentDetail> = sqlx::query_as(concat!(
        "select ",
        item_columns!(),
        ", i.severity_id, i.metadata, i.postmortem,
           rb.email as resolved_by_email, cb.email as created_by_email
         from ",
        item_joins!(),
        " left join users rb on rb.id = i.resolved_by
          left join users cb on cb.id = i.created_by
         where i.project_id = $1 and i.id = $2"
    ))
    .bind(project_id)
    .bind(incident_id)
    .fetch_optional(pool)
    .await?;
    let Some(mut detail) = detail else {
        return Ok(None);
    };
    detail.events = sqlx::query_as(concat!(
        "select * from (",
        event_select!(),
        " where e.incident_id = $1 order by e.id desc limit $2) latest
         order by id"
    ))
    .bind(incident_id)
    .bind(MAX_DETAIL_EVENTS)
    .fetch_all(pool)
    .await?;
    Ok(Some(detail))
}

/// One timeline event by id.
pub async fn event(pool: &PgPool, event_id: i64) -> Result<Option<IncidentEvent>, sqlx::Error> {
    sqlx::query_as(concat!(event_select!(), " where e.id = $1"))
        .bind(event_id)
        .fetch_optional(pool)
        .await
}

/// Locks the project's incident row for the rest of the caller's
/// transaction. `false` when the project has no such incident.
pub async fn lock(
    conn: &mut PgConnection,
    project_id: Uuid,
    incident_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let found: Option<Uuid> =
        sqlx::query_scalar("select id from incidents where id = $1 and project_id = $2 for update")
            .bind(incident_id)
            .bind(project_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(found.is_some())
}

/// Replaces the postmortem (`None` clears it) and records a
/// `postmortem_updated` event by `actor`, without the text. The caller checks
/// the text against [`MAX_POSTMORTEM_BYTES`].
pub async fn set_postmortem(
    conn: &mut PgConnection,
    incident_id: Uuid,
    actor: Uuid,
    postmortem: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("update incidents set postmortem = $2 where id = $1")
        .bind(incident_id)
        .bind(postmortem)
        .execute(&mut *conn)
        .await?;
    lifecycle::record_event(
        conn,
        incident_id,
        EventKind::PostmortemUpdated,
        Some(actor),
        None,
        json!({}),
    )
    .await?;
    Ok(())
}

/// Sets the incident's severity and records a `severity_changed` event by
/// `actor` carrying `{"from": <name>, "to": <name>}`. Setting the current
/// severity again changes nothing. `false` when `severity_id` is not one of
/// the project's severities.
pub async fn set_severity(
    conn: &mut PgConnection,
    project_id: Uuid,
    incident_id: Uuid,
    actor: Uuid,
    severity_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let names: Option<(Option<String>, String)> = sqlx::query_as(
        "select (select name from incident_severities where id = i.severity_id), s.name
         from incidents i
         join incident_severities s on s.id = $2 and s.project_id = $3
         where i.id = $1",
    )
    .bind(incident_id)
    .bind(severity_id)
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((from, to)) = names else {
        return Ok(false);
    };
    if from.as_deref() == Some(to.as_str()) {
        return Ok(true);
    }
    sqlx::query("update incidents set severity_id = $2 where id = $1")
        .bind(incident_id)
        .bind(severity_id)
        .execute(&mut *conn)
        .await?;
    lifecycle::record_event(
        conn,
        incident_id,
        EventKind::SeverityChanged,
        Some(actor),
        None,
        json!({"from": from, "to": to}),
    )
    .await?;
    Ok(true)
}

/// Opens an incident for the monitor unless it already has an unresolved
/// one (`lifecycle::open`). Returns the new incident's id, or `None`.
pub async fn open_for_monitor(
    conn: &mut PgConnection,
    project_id: Uuid,
    monitor_id: Uuid,
    title: &str,
    cause: Option<&str>,
    started_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    lifecycle::open(
        conn,
        &OpenIncident {
            project_id,
            source: IncidentSource::Monitor(monitor_id),
            title,
            cause,
            severity_id: None,
            metadata: None,
            started_at,
        },
    )
    .await
}

/// Resolves the monitor's unresolved incident, if there is one. Returns its id.
pub async fn resolve_for_monitor(
    conn: &mut PgConnection,
    monitor_id: Uuid,
    resolved_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    lifecycle::resolve_source(conn, &IncidentSource::Monitor(monitor_id), resolved_at).await
}

/// Log-alert counterpart of [`open_for_monitor`]. Never reopens a resolved
/// incident (unlike [`open_for_heartbeat`]). Called only through
/// `logs::alert::open_alert_incident`.
pub async fn open_for_log_alert(
    conn: &mut PgConnection,
    project_id: Uuid,
    log_alert_id: Uuid,
    title: &str,
    cause: Option<&str>,
    started_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    lifecycle::open(
        conn,
        &OpenIncident {
            project_id,
            source: IncidentSource::LogAlert(log_alert_id),
            title,
            cause,
            severity_id: None,
            metadata: None,
            started_at,
        },
    )
    .await
}

/// Log-alert counterpart of [`resolve_for_monitor`]. Called only through
/// `logs::alert::resolve_alert_incident`.
pub async fn resolve_for_log_alert(
    conn: &mut PgConnection,
    log_alert_id: Uuid,
    resolved_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    lifecycle::resolve_source(conn, &IncidentSource::LogAlert(log_alert_id), resolved_at).await
}

/// A heartbeat incident resolved less than this long ago is reopened, not
/// followed by a new one, when the heartbeat goes down again (see
/// [`open_for_heartbeat`]). Same length as the alert flap window
/// (`uptime::alert_throttle::ALERT_FLAP_WINDOW`).
pub const HEARTBEAT_REOPEN_WINDOW_SECS: i32 = 5 * 60;

/// Heartbeat counterpart of [`open_for_monitor`], with flap control: when the
/// heartbeat's latest incident was resolved less than
/// [`HEARTBEAT_REOPEN_WINDOW_SECS`] before `started_at`, that incident is
/// reopened (`lifecycle::reopen_source_within`: status open, title and cause
/// replaced, details cleared for the caller to attach the new ones; its id
/// and `started_at` stay) and its id returned, instead of a new row. A job
/// that fails and succeeds in turn (`/fail`, then `/0`, as fast as the ping
/// rate limit allows) therefore keeps one incident row, however long it
/// flaps, and its downtime counts from the first failure until it stays up.
/// Called under the heartbeat's row lock (the ping and the sweep both hold it).
pub async fn open_for_heartbeat(
    conn: &mut PgConnection,
    project_id: Uuid,
    heartbeat_id: Uuid,
    title: &str,
    cause: Option<&str>,
    started_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    let source = IncidentSource::Heartbeat(heartbeat_id);
    let reopened = lifecycle::reopen_source_within(
        conn,
        &source,
        HEARTBEAT_REOPEN_WINDOW_SECS,
        title,
        cause,
        started_at,
    )
    .await?;
    if reopened.is_some() {
        return Ok(reopened);
    }
    lifecycle::open(
        conn,
        &OpenIncident {
            project_id,
            source,
            title,
            cause,
            severity_id: None,
            metadata: None,
            started_at,
        },
    )
    .await
}

/// Heartbeat counterpart of [`resolve_for_monitor`].
pub async fn resolve_for_heartbeat(
    conn: &mut PgConnection,
    heartbeat_id: Uuid,
    resolved_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    lifecycle::resolve_source(conn, &IncidentSource::Heartbeat(heartbeat_id), resolved_at).await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_round_trips_and_garbage_is_refused() {
        let at = DateTime::parse_from_rfc3339("2026-10-19T00:00:00.123456Z")
            .unwrap()
            .with_timezone(&Utc);
        let id = Uuid::now_v7();
        let item = IncidentListItem {
            id,
            project_id: Uuid::nil(),
            monitor_id: None,
            heartbeat_id: None,
            log_alert_id: None,
            source: "manual".into(),
            source_name: None,
            title: "t".into(),
            cause: None,
            details: None,
            status: "open".into(),
            severity: None,
            severity_rank: None,
            started_at: at,
            acknowledged_at: None,
            acknowledged_by_email: None,
            resolved_at: None,
        };
        assert_eq!(decode_cursor(&encode_cursor(&item)), Some((at, id)));
        for bad in ["", "%%%", "bm90LWEtY3Vyc29y", &URL_SAFE_NO_PAD.encode("2026-10-19|x")] {
            assert_eq!(decode_cursor(bad), None, "{bad:?}");
        }
    }
}
