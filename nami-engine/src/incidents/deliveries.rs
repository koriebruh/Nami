//! The durable notification queue: deliveries are queued once per (incident,
//! round, target), claimed under the storm caps (R13), sent outside any
//! transaction, recorded in the incident timeline, and retried with backoff
//! up to [`MAX_ATTEMPTS`] times (R12). A step notification whose incident is
//! no longer open is cancelled (R15).

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::{
    core::{error::AppError, state::AppState},
    incidents::{
        channel::model as channel,
        format::{IncidentSummary, Message, NotifyEvent},
        lifecycle::{self, EventKind},
        send::{self, Retry, SendError, Senders, UNDECRYPTABLE},
    },
};

/// Attempts per delivery (the table's `attempts` check).
pub const MAX_ATTEMPTS: i32 = 5;
/// How long a claimed delivery is held before another claim may take it.
pub const LEASE: Duration = Duration::from_secs(60);
/// Most deliveries one channel receives per [`RATE_WINDOW`].
pub const CHANNEL_PER_MINUTE: i64 = 30;
/// Most implicit emails one user receives per [`RATE_WINDOW`].
pub const EMAIL_PER_USER_PER_MINUTE: i64 = 20;
/// The window of the per-minute caps, and how long an over-cap delivery waits.
pub const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Most deliveries one drain claims.
pub const DRAIN_BATCH: i64 = 50;
/// Most sends one drain runs at once.
pub const MAX_SENDS_IN_FLIGHT: usize = 10;
/// Most chars of `last_error` and of a delivery's timeline event.
pub const MAX_RECORDED_CHARS: usize = 200;
/// What an email delivery to a user without a verified address records.
pub const NO_VERIFIED_EMAIL: &str = "no verified email";
const FIRST_BACKOFF: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(3600);

/// 30 s × 4^(attempt-1), capped at 1 h (attempt is 1-based, the one that just failed).
pub fn backoff(attempt: i32) -> Duration {
    let exponent = attempt.clamp(1, 6) as u32 - 1;
    (FIRST_BACKOFF * 4u32.pow(exponent)).min(MAX_BACKOFF)
}

/// What a delivery tells its target (`notification_deliveries.event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryEvent {
    Triggered,
    Resolved,
}

impl DeliveryEvent {
    fn as_str(self) -> &'static str {
        match self {
            DeliveryEvent::Triggered => "triggered",
            DeliveryEvent::Resolved => "resolved",
        }
    }
}

/// One delivery to queue. `round` names the escalation pass and step (or
/// `resolved`); a target gets at most one delivery per incident and round.
pub struct NewDelivery {
    pub incident_id: Uuid,
    pub event: DeliveryEvent,
    pub round: String,
    pub target: Target,
}

/// Who a delivery goes to: a user's implicit (verified) email, or a channel
/// (`user_id` = the owner of a personal channel, checked for membership).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    Email { user_id: Uuid },
    Channel { channel_id: Uuid, user_id: Option<Uuid> },
}

impl Target {
    /// `email:{user}` or `channel:{channel}`, the deduplication key.
    pub fn key(&self) -> String {
        match self {
            Target::Email { user_id } => format!("email:{user_id}"),
            Target::Channel { channel_id, .. } => format!("channel:{channel_id}"),
        }
    }
}

/// Inserts, ignoring a duplicate (incident, round, target). Returns how many rows were new.
pub async fn enqueue(conn: &mut PgConnection, deliveries: &[NewDelivery]) -> Result<u64, sqlx::Error> {
    let mut inserted = 0;
    // ponytail: one statement per row; a step queues at most 100 (R13).
    for delivery in deliveries {
        let (user_id, channel_id) = match delivery.target {
            Target::Email { user_id } => (Some(user_id), None),
            Target::Channel { channel_id, user_id } => (user_id, Some(channel_id)),
        };
        inserted += sqlx::query(
            "insert into notification_deliveries (incident_id, event, round, target_key, user_id, channel_id)
             values ($1, $2, $3, $4, $5, $6)
             on conflict on constraint notification_deliveries_target_unique do nothing",
        )
        .bind(delivery.incident_id)
        .bind(delivery.event.as_str())
        .bind(&delivery.round)
        .bind(delivery.target.key())
        .bind(user_id)
        .bind(channel_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    }
    Ok(inserted)
}

/// Claims due deliveries and sends them, at most [`MAX_SENDS_IN_FLIGHT`] at
/// once, and returns when every send is recorded. Returns how many were
/// attempted.
pub async fn drain(state: &AppState, senders: &Senders) -> Result<usize, AppError> {
    let claimed = claim(&state.db).await?;
    let attempted = claimed.len();
    let mut sends = JoinSet::new();
    for row in claimed {
        if sends.len() >= MAX_SENDS_IN_FLIGHT {
            log_panic(sends.join_next().await);
        }
        let (state, senders) = (state.clone(), senders.clone());
        sends.spawn(async move {
            let id = row.id;
            if let Err(e) = deliver(&state, &senders, row).await {
                tracing::error!(delivery_id = %id, error = %e, "notification delivery could not be recorded");
            }
        });
    }
    while let Some(ended) = sends.join_next().await {
        log_panic(Some(ended));
    }
    Ok(attempted)
}

fn log_panic(ended: Option<Result<(), tokio::task::JoinError>>) {
    if let Some(Err(e)) = ended {
        tracing::error!(error = %e, "notification send task panicked");
    }
}

/// A claimed delivery; `attempts` counts this attempt.
#[derive(sqlx::FromRow)]
struct Claimed {
    id: Uuid,
    incident_id: Uuid,
    event: String,
    user_id: Option<Uuid>,
    channel_id: Option<Uuid>,
    attempts: i32,
}

// Poison rows (R12): a lease that expired after the last attempt (the process
// died mid-send) would otherwise be reclaimed past the attempts check, which
// would fail the whole claim statement and wedge the queue.
const FAIL_INTERRUPTED: &str = "update notification_deliveries
    set status = 'failed', locked_until = null, last_error = 'delivery interrupted'
    where status = 'sending' and locked_until < now() and attempts >= $1";

// The storm caps (R13): each candidate is ranked within its bucket (a channel,
// or a user's implicit email) after what the bucket sent in the last window
// plus what is leased now. Over-cap rows are deferred in the same statement,
// without spending an attempt, so a busy channel never starves the others.
// $1 = DRAIN_BATCH, $2 = CHANNEL_PER_MINUTE, $3 = EMAIL_PER_USER_PER_MINUTE,
// $4 = LEASE, $5 = RATE_WINDOW (seconds), $6 = MAX_ATTEMPTS.
const CLAIM: &str = "with candidates as (
    select id, channel_id, user_id, next_attempt_at,
           case when channel_id is not null then 'c:' || channel_id::text else 'u:' || user_id::text end as bucket
    from notification_deliveries
    where ((status = 'pending' and next_attempt_at <= now())
        or (status = 'sending' and locked_until < now()))
      and attempts < $6
    order by next_attempt_at, id
    limit $1
    for update skip locked
), recent as (
    select case when channel_id is not null then 'c:' || channel_id::text else 'u:' || user_id::text end as bucket,
           count(*) as used
    from notification_deliveries
    where ((status = 'sent' and sent_at > now() - make_interval(secs => $5))
        or (status = 'sending' and locked_until >= now()))
      and (channel_id in (select channel_id from candidates where channel_id is not null)
        or (channel_id is null and user_id in (select user_id from candidates where channel_id is null)))
    group by 1
), ranked as (
    select c.id,
           coalesce(r.used, 0) + row_number() over (partition by c.bucket order by c.next_attempt_at, c.id) as nth,
           case when c.channel_id is not null then $2::bigint else $3::bigint end as cap
    from candidates c left join recent r on r.bucket = c.bucket
), deferred as (
    update notification_deliveries d
    set status = 'pending', locked_until = null, next_attempt_at = now() + make_interval(secs => $5)
    from ranked r where d.id = r.id and r.nth > r.cap
    returning d.id
)
update notification_deliveries d
set status = 'sending', locked_until = now() + make_interval(secs => $4), attempts = d.attempts + 1
from ranked r
where d.id = r.id and r.nth <= r.cap
returning d.id, d.incident_id, d.event, d.user_id, d.channel_id, d.attempts";

/// One claim at a time across every engine instance (a transaction-level
/// advisory lock, held for milliseconds), so the caps count every lease.
async fn claim(pool: &PgPool) -> Result<Vec<Claimed>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('nami.notification_deliveries.claim'))")
        .execute(&mut *tx)
        .await?;
    sqlx::query(FAIL_INTERRUPTED).bind(MAX_ATTEMPTS).execute(&mut *tx).await?;
    let claimed = sqlx::query_as(CLAIM)
        .bind(DRAIN_BATCH)
        .bind(CHANNEL_PER_MINUTE)
        .bind(EMAIL_PER_USER_PER_MINUTE)
        .bind(LEASE.as_secs_f64())
        .bind(RATE_WINDOW.as_secs_f64())
        .bind(MAX_ATTEMPTS)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(claimed)
}

/// What a delivery needs besides its channel's secrets.
#[derive(sqlx::FromRow)]
struct Context {
    project_id: Uuid,
    project_name: String,
    title: String,
    cause: Option<String>,
    status: String,
    source: String,
    severity: String,
    started_at: DateTime<Utc>,
    details: Option<String>,
    resolution_reason: Option<String>,
    user_email: Option<String>,
    user_verified: bool,
    user_is_member: bool,
    channel_kind: Option<String>,
    channel_name: Option<String>,
    channel_enabled: Option<bool>,
}

// $1 = incident, $2 = user (or null), $3 = channel (or null). Membership is
// `project_members` only: a global admin who is not a member is not paged (R7).
const CONTEXT: &str = "select i.project_id, p.name as project_name, i.title, i.cause, i.status, i.source,
        coalesce(s.name, 'unknown') as severity, i.started_at, i.details,
        (select e.metadata->>'reason' from incident_events e
         where e.incident_id = i.id and e.kind = 'resolved' order by e.id desc limit 1) as resolution_reason,
        u.email as user_email, u.email_verified_at is not null as user_verified,
        exists (select 1 from project_members pm
                where pm.project_id = i.project_id and pm.user_id = $2) as user_is_member,
        c.kind as channel_kind, c.name as channel_name, c.enabled as channel_enabled
    from incidents i
    join projects p on p.id = i.project_id
    left join incident_severities s on s.id = i.severity_id
    left join users u on u.id = $2
    left join notification_channels c on c.id = $3
    where i.id = $1";

enum Outcome {
    /// Never sent: the incident is no longer open (a step notification), the
    /// user left the project, or the channel is gone or disabled.
    Cancelled,
    /// `label` names the target in the timeline: `Slack: <name>`, `Email: <address>`.
    Sent { label: String },
    Failed { label: String, error: SendError },
}

impl Outcome {
    fn of(label: String, result: Result<(), SendError>) -> Outcome {
        match result {
            Ok(()) => Outcome::Sent { label },
            Err(error) => Outcome::Failed { label, error },
        }
    }
}

async fn deliver(state: &AppState, senders: &Senders, row: Claimed) -> Result<(), sqlx::Error> {
    let outcome = attempt(state, senders, &row).await?;
    record(&state.db, &row, outcome).await
}

async fn attempt(state: &AppState, senders: &Senders, row: &Claimed) -> Result<Outcome, sqlx::Error> {
    let context: Option<Context> = sqlx::query_as(CONTEXT)
        .bind(row.incident_id)
        .bind(row.user_id)
        .bind(row.channel_id)
        .fetch_optional(&state.db)
        .await?;
    // A deleted incident takes its deliveries with it (cascade).
    let Some(c) = context else {
        return Ok(Outcome::Cancelled);
    };
    let event = match row.event.as_str() {
        "resolved" => NotifyEvent::Resolved,
        _ => NotifyEvent::Triggered,
    };
    if (event == NotifyEvent::Triggered && c.status != "open") || (row.user_id.is_some() && !c.user_is_member) {
        return Ok(Outcome::Cancelled);
    }
    let message = Message {
        event,
        incident: IncidentSummary {
            id: row.incident_id,
            title: c.title,
            cause: c.cause,
            status: c.status,
            source: c.source,
            severity: c.severity,
            started_at: c.started_at,
            details: c.details,
            resolution_reason: c.resolution_reason,
        },
        project_id: c.project_id,
        project_name: c.project_name,
        incident_url: format!(
            "{}/projects/{}/incidents/{}",
            state.config.ui_base_url.trim_end_matches('/'),
            c.project_id,
            row.incident_id
        ),
        actions: None,
    };

    let Some(channel_id) = row.channel_id else {
        let email = c.user_email.unwrap_or_default();
        let result = if c.user_verified {
            send::send_email(&state.mailer, &email, &message).await
        } else {
            Err(SendError::never(NO_VERIFIED_EMAIL))
        };
        return Ok(Outcome::of(format!("Email: {email}"), result));
    };
    let (Some(kind), Some(name), Some(true)) = (c.channel_kind, c.channel_name, c.channel_enabled) else {
        return Ok(Outcome::Cancelled);
    };
    let label = format!("{}: {name}", kind_label(&kind));
    let result = match channel::open_for_send(&state.db, &state.cipher, channel_id).await? {
        None => return Ok(Outcome::Cancelled),
        Some(Err(_)) => Err(SendError::never(UNDECRYPTABLE)),
        Some(Ok(opened)) => send::send_to_channel(senders, &state.mailer, &opened, &message, row.id).await,
    };
    Ok(Outcome::of(label, result))
}

/// How the timeline names a channel kind.
fn kind_label(kind: &str) -> &str {
    match kind {
        "email" => "Email",
        "webhook" => "Webhook",
        "slack" => "Slack",
        "teams" => "Teams",
        "telegram" => "Telegram",
        "discord" => "Discord",
        other => other,
    }
}

fn recorded(text: &str) -> String {
    text.chars().take(MAX_RECORDED_CHARS).collect()
}

// Only while this attempt still holds the row: a finisher whose lease was
// taken over by a later claim changes nothing.
const RECORD: &str = "update notification_deliveries
    set status = $3, last_error = $4, locked_until = null,
        sent_at = case when $3 = 'sent' then now() end,
        next_attempt_at = coalesce(now() + make_interval(secs => $5), next_attempt_at)
    where id = $1 and status = 'sending' and attempts = $2";

/// Writes the outcome and its timeline event in one transaction: `sent`, a
/// retry (`pending` after the backoff or a longer `Retry-After`), `failed`
/// (never retryable, or the last attempt) or `cancelled` (no event).
async fn record(pool: &PgPool, row: &Claimed, outcome: Outcome) -> Result<(), sqlx::Error> {
    let (status, error, retry_in, event) = match outcome {
        Outcome::Cancelled => ("cancelled", None, None, None),
        Outcome::Sent { label } => ("sent", None, None, Some((EventKind::NotificationSent, label))),
        Outcome::Failed { label, error } => {
            let retry_in = match error.retry {
                Retry::After(wait) if row.attempts < MAX_ATTEMPTS => {
                    Some(backoff(row.attempts).max(wait.unwrap_or_default()))
                }
                _ => None,
            };
            let status = if retry_in.is_some() { "pending" } else { "failed" };
            let event = (EventKind::NotificationFailed, format!("{label}: {}", error.message));
            (status, Some(recorded(&error.message)), retry_in, Some(event))
        }
    };
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(RECORD)
        .bind(row.id)
        .bind(row.attempts)
        .bind(status)
        .bind(error)
        .bind(retry_in.map(|wait| wait.as_secs_f64()))
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if let (1, Some((kind, message))) = (updated, event) {
        lifecycle::record_event(&mut tx, row.incident_id, kind, None, Some(&recorded(&message)), json!({}))
            .await?;
    }
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_30s_times_4_to_the_attempt_capped_at_an_hour() {
        let secs: Vec<u64> = (1..=6).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(secs, [30, 120, 480, 1920, 3600, 3600]);
    }

    #[test]
    fn targets_are_keyed_by_kind_and_id() {
        let id = Uuid::nil();
        assert_eq!(Target::Email { user_id: id }.key(), format!("email:{id}"));
        assert_eq!(
            Target::Channel { channel_id: id, user_id: Some(Uuid::max()) }.key(),
            format!("channel:{id}")
        );
    }
}
