//! The escalation tick: claims incidents whose next step is due, resolves the
//! step's targets to recipients (R7), queues their deliveries, and moves the
//! cursor with the pure planner (`plan::after_step`, R14).

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{
    core::{error::AppError, state::AppState},
    incidents::{
        deliveries::{self, DeliveryEvent, NewDelivery, Target},
        escalation::{
            model::StepCondition,
            plan::{self, Cursor, Exhausted, Next, PolicyShape},
        },
        lifecycle::{self, EventKind},
        oncall::{model::load_rotation, rotation::on_call_at},
    },
};

/// Most incidents one tick escalates.
pub const ESCALATION_BATCH: usize = 50;
/// Most steps (run, skipped, or past a shortened policy's end) one tick runs
/// for one incident; the rest wait for the next tick.
pub const MAX_STEPS_PER_TICK: usize = 20;
/// Most recipients one step notifies.
pub const MAX_RECIPIENTS_PER_STEP: usize = 100;
/// Most deliveries one incident ever queues through its steps.
pub const MAX_DELIVERIES_PER_INCIDENT: i64 = 500;
/// A step overdue by more than this runs now, and the policy's clock restarts
/// from now (R14): an outage never replays missed steps as a burst.
const MAX_LAG: Duration = Duration::seconds(60);

/// An incident whose next step is due, locked by its transaction.
#[derive(sqlx::FromRow)]
struct Due {
    id: Uuid,
    project_id: Uuid,
    escalation_policy_id: Option<Uuid>,
    escalation_generation: i32,
    escalation_step: i32,
    escalation_repeat: i32,
    escalation_on_fallback: bool,
    escalation_notified: i32,
    next_escalation_at: DateTime<Utc>,
    now: DateTime<Utc>,
}

// $1 = the incidents this tick already took (done, or failed).
const CLAIM: &str = "select id, project_id, escalation_policy_id, escalation_generation,
        escalation_step, escalation_repeat, escalation_on_fallback, escalation_notified,
        next_escalation_at, now() as now
    from incidents
    where status = 'open' and next_escalation_at <= now() and not (id = any($1))
    order by next_escalation_at
    limit 1
    for update skip locked";

/// Runs every due step of up to [`ESCALATION_BATCH`] incidents, one
/// transaction each, and returns how many steps ran or were skipped. An
/// incident whose escalation fails is logged and left for the next tick, so
/// it never blocks the others; only a failure to claim is an `Err`.
pub async fn tick(state: &AppState) -> Result<usize, AppError> {
    let mut taken: Vec<Uuid> = Vec::with_capacity(ESCALATION_BATCH);
    let mut steps = 0;
    while taken.len() < ESCALATION_BATCH {
        let mut tx = state.db.begin().await?;
        let due: Option<Due> = sqlx::query_as(CLAIM).bind(&taken).fetch_optional(&mut *tx).await?;
        let Some(due) = due else {
            break;
        };
        taken.push(due.id);
        let escalated = match escalate(&mut tx, &due).await {
            Ok(ran) => tx.commit().await.map(|()| ran),
            Err(e) => Err(e),
        };
        match escalated {
            Ok(ran) => steps += ran,
            Err(e) => tracing::error!(incident_id = %due.id, error = %e, "escalation failed"),
        }
    }
    Ok(steps)
}

/// A policy as the tick walks it.
struct Policy {
    shape: PolicyShape,
    name: String,
    fallback_policy_id: Option<Uuid>,
    /// `(step id, condition)` in order, parallel to `shape.delays_minutes`.
    steps: Vec<(Uuid, Option<StepCondition>)>,
}

#[derive(sqlx::FromRow)]
struct StepRow {
    name: String,
    repeat_count: i32,
    fallback_policy_id: Option<Uuid>,
    step_id: Uuid,
    delay_minutes: i32,
    condition: Option<sqlx::types::Json<StepCondition>>,
}

/// One policy of the project with its steps; `None`: no such policy.
async fn load_policy(
    conn: &mut PgConnection,
    project_id: Uuid,
    policy_id: Uuid,
) -> Result<Option<Policy>, sqlx::Error> {
    let rows: Vec<StepRow> = sqlx::query_as(
        "select p.name, p.repeat_count, p.fallback_policy_id,
                s.id as step_id, s.delay_minutes, s.condition
         from escalation_policies p join escalation_steps s on s.policy_id = p.id
         where p.id = $1 and p.project_id = $2
         order by s.position",
    )
    .bind(policy_id)
    .bind(project_id)
    .fetch_all(conn)
    .await?;
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(Policy {
        shape: PolicyShape {
            id: policy_id,
            delays_minutes: rows
                .iter()
                .map(|r| u32::try_from(r.delay_minutes).unwrap_or(0))
                .collect(),
            repeat_count: u32::try_from(first.repeat_count).unwrap_or(0),
        },
        name: first.name.clone(),
        fallback_policy_id: first.fallback_policy_id,
        steps: rows.into_iter().map(|r| (r.step_id, r.condition.map(|c| c.0))).collect(),
    }))
}

/// Runs the incident's due steps, at most [`MAX_STEPS_PER_TICK`], and saves
/// where its escalation stands. Returns how many steps ran or were skipped.
async fn escalate(conn: &mut PgConnection, due: &Due) -> Result<usize, sqlx::Error> {
    let own = match due.escalation_policy_id {
        Some(id) => load_policy(conn, due.project_id, id).await?,
        None => None,
    };
    let fallback = match own.as_ref().and_then(|p| p.fallback_policy_id) {
        Some(id) => load_policy(conn, due.project_id, id).await?,
        None => None,
    };
    let mut cursor = Cursor {
        step: usize::try_from(due.escalation_step).unwrap_or(0),
        repeat: u32::try_from(due.escalation_repeat).unwrap_or(0),
        on_fallback: due.escalation_on_fallback,
        notified_in_pass: u32::try_from(due.escalation_notified).unwrap_or(0),
    };
    let mut scheduled = due.next_escalation_at;
    let mut ran = 0;
    loop {
        let active = if cursor.on_fallback { fallback.as_ref() } else { own.as_ref() };
        let Some(active) = active else {
            exhaust(conn, due.id, Exhausted::NoPolicy).await?;
            return Ok(ran);
        };
        ran += 1;
        let ran_at = scheduled.max(due.now - MAX_LAG);
        // A cursor past the last step (the policy was shortened) ends the pass.
        let newly = match active.steps.get(cursor.step) {
            Some((step_id, condition)) => {
                run_step(conn, due, active, *step_id, condition.as_ref(), cursor).await?
            }
            None => 0,
        };
        let fallback_shape = fallback.as_ref().map(|f| &f.shape);
        let next = plan::after_step(&active.shape, fallback_shape, cursor, ran_at, newly);
        let (next, at) = match next {
            Next::Step { cursor, at } => (cursor, at),
            Next::Exhausted(reason) => {
                exhaust(conn, due.id, reason).await?;
                return Ok(ran);
            }
        };
        if next.on_fallback && !cursor.on_fallback {
            let name = fallback.as_ref().map_or("", |f| f.name.as_str());
            let why = if cursor.notified_in_pass.saturating_add(newly) == 0 {
                "No one could be notified"
            } else {
                "No one acknowledged"
            };
            let message = format!("{why}: continuing with {name}");
            let kind = EventKind::EscalationFallback;
            lifecycle::record_event(conn, due.id, kind, None, Some(&message), json!({})).await?;
        }
        cursor = next;
        scheduled = at;
        if at > due.now || ran >= MAX_STEPS_PER_TICK {
            save(conn, due.id, &cursor, at).await?;
            return Ok(ran);
        }
    }
}

/// Runs one step at `due.now`: records it skipped when its condition does
/// not cover now, else queues its recipients (within the incident's cap).
/// Returns how many recipients were newly queued.
async fn run_step(
    conn: &mut PgConnection,
    due: &Due,
    policy: &Policy,
    step_id: Uuid,
    condition: Option<&StepCondition>,
    cursor: Cursor,
) -> Result<u32, sqlx::Error> {
    let n = cursor.step + 1;
    // A condition that no longer parses (a zone dropped from the tz database)
    // pages rather than silences.
    if let Some(c) = condition
        && c.window().is_ok_and(|w| !w.covers(due.now))
    {
        let days = weekdays(&c.weekdays);
        let message = format!("Step {n} skipped: outside {days} {}-{} {}", c.from, c.to, c.timezone);
        let kind = EventKind::StepSkipped;
        lifecycle::record_event(conn, due.id, kind, None, Some(&message), json!({})).await?;
        return Ok(0);
    }
    let mut targets = recipients(conn, due.project_id, step_id, due.now).await?;
    let existing: i64 =
        sqlx::query_scalar("select count(*) from notification_deliveries where incident_id = $1")
            .bind(due.id)
            .fetch_one(&mut *conn)
            .await?;
    let room = usize::try_from(MAX_DELIVERIES_PER_INCIDENT - existing).unwrap_or(0);
    if targets.len() > room {
        targets.truncate(room);
        note_cap(conn, due.id).await?;
    }
    let round = plan::round(due.escalation_generation, &cursor);
    let queued: Vec<NewDelivery> = targets
        .into_iter()
        .map(|target| NewDelivery {
            incident_id: due.id,
            event: DeliveryEvent::Triggered,
            round: round.clone(),
            target,
        })
        .collect();
    let newly = u32::try_from(deliveries::enqueue(conn, &queued).await?).unwrap_or(u32::MAX);
    let message = format!("Step {n} of {}: {newly} recipient(s)", policy.name);
    let metadata = json!({
        "step": n, "repeat": cursor.repeat, "fallback": cursor.on_fallback, "recipients": newly
    });
    let kind = EventKind::Escalated;
    lifecycle::record_event(conn, due.id, kind, None, Some(&message), metadata).await?;
    Ok(newly)
}

/// "Mon,Tue" for ISO weekdays.
fn weekdays(days: &[u8]) -> String {
    const NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let names: Vec<&str> = days
        .iter()
        .filter_map(|&d| NAMES.get(usize::from(d).wrapping_sub(1)).copied())
        .collect();
    names.join(",")
}

/// Records, once per incident, that its delivery cap was reached.
async fn note_cap(conn: &mut PgConnection, incident_id: Uuid) -> Result<(), sqlx::Error> {
    let noted: bool = sqlx::query_scalar(
        "select exists (select 1 from incident_events
                        where incident_id = $1 and kind = 'notification_failed'
                          and metadata->>'cap' = 'true')",
    )
    .bind(incident_id)
    .fetch_one(&mut *conn)
    .await?;
    if !noted {
        let message =
            format!("notification cap reached ({MAX_DELIVERIES_PER_INCIDENT} per incident)");
        let kind = EventKind::NotificationFailed;
        lifecycle::record_event(conn, incident_id, kind, None, Some(&message), json!({"cap": true}))
            .await?;
    }
    Ok(())
}

/// Writes the cursor and when its step is due.
async fn save(
    conn: &mut PgConnection,
    incident_id: Uuid,
    cursor: &Cursor,
    at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update incidents set escalation_step = $2, escalation_repeat = $3, escalation_on_fallback = $4,
                escalation_notified = $5, next_escalation_at = $6
         where id = $1",
    )
    .bind(incident_id)
    .bind(i32::try_from(cursor.step).unwrap_or(i32::MAX))
    .bind(i32::try_from(cursor.repeat).unwrap_or(i32::MAX))
    .bind(cursor.on_fallback)
    .bind(i32::try_from(cursor.notified_in_pass).unwrap_or(i32::MAX))
    .bind(at)
    .execute(conn)
    .await?;
    Ok(())
}

/// Ends the incident's escalation and records why.
async fn exhaust(
    conn: &mut PgConnection,
    incident_id: Uuid,
    reason: Exhausted,
) -> Result<(), sqlx::Error> {
    sqlx::query("update incidents set next_escalation_at = null where id = $1")
        .bind(incident_id)
        .execute(&mut *conn)
        .await?;
    let message = match reason {
        Exhausted::NobodyNotified => concat!(
            "No one could be notified (nobody on call, no members or enabled channels) ",
            "and there is no fallback policy"
        ),
        Exhausted::RepeatsDone => {
            "Escalation finished: every step and repeat ran without an acknowledgement"
        }
        Exhausted::NoPolicy => "The escalation policy was deleted",
    };
    let kind = EventKind::EscalationExhausted;
    lifecycle::record_event(conn, incident_id, kind, None, Some(message), json!({})).await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct TargetRow {
    kind: String,
    user_id: Option<Uuid>,
    schedule_id: Option<Uuid>,
    channel_id: Option<Uuid>,
}

// $1 = project, $2 = the users the step names (directly or on call), $3 =
// every member, $4 = the project channels it names. Only project members
// are paged (R7), with their verified email and enabled personal channels.
const RECIPIENTS: &str = "select m.user_id, null::uuid as channel_id
    from project_members m join users u on u.id = m.user_id
    where m.project_id = $1 and ($3 or m.user_id = any($2)) and u.email_verified_at is not null
    union all
    select c.user_id, c.id
    from notification_channels c
    join project_members m on m.user_id = c.user_id and m.project_id = $1
    where c.enabled and ($3 or c.user_id = any($2))
    union all
    select null, c.id from notification_channels c
    where c.project_id = $1 and c.enabled and c.id = any($4)";

/// R7: the targets of one step at `at`, deduplicated, members only, capped.
/// A `user` target (also whoever a `schedule` puts on call at `at`, and each
/// member for `all_members`) yields their email when it is verified and
/// each enabled personal channel; a `project_channel` yields itself when
/// enabled. Ordered by `Target::key`, at most [`MAX_RECIPIENTS_PER_STEP`].
pub async fn recipients(
    conn: &mut PgConnection,
    project_id: Uuid,
    step_id: Uuid,
    at: DateTime<Utc>,
) -> Result<Vec<Target>, sqlx::Error> {
    let targets: Vec<TargetRow> = sqlx::query_as(
        "select kind, user_id, schedule_id, channel_id from escalation_step_targets
         where step_id = $1 order by position",
    )
    .bind(step_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut users = Vec::with_capacity(targets.len());
    let mut channels = Vec::new();
    let mut all_members = false;
    for target in targets {
        match (target.kind.as_str(), target.user_id, target.schedule_id, target.channel_id) {
            ("user", Some(user), _, _) => users.push(user),
            ("schedule", _, Some(schedule), _) => {
                if let Some((rotation, overrides)) = load_rotation(conn, schedule, at, at).await?
                    && let Some((user, _)) = on_call_at(&rotation, &overrides, at)
                {
                    users.push(user);
                }
            }
            ("all_members", ..) => all_members = true,
            ("project_channel", _, _, Some(channel)) => channels.push(channel),
            _ => {}
        }
    }
    let rows: Vec<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(RECIPIENTS)
        .bind(project_id)
        .bind(&users)
        .bind(all_members)
        .bind(&channels)
        .fetch_all(conn)
        .await?;
    Ok(dedup(rows.into_iter().filter_map(|row| match row {
        (user_id, Some(channel_id)) => Some(Target::Channel { channel_id, user_id }),
        (Some(user_id), None) => Some(Target::Email { user_id }),
        (None, None) => None,
    })))
}

/// One target per key, ordered by key, the first [`MAX_RECIPIENTS_PER_STEP`].
fn dedup(targets: impl IntoIterator<Item = Target>) -> Vec<Target> {
    let mut by_key = BTreeMap::new();
    for target in targets {
        by_key.entry(target.key()).or_insert(target);
    }
    by_key.into_values().take(MAX_RECIPIENTS_PER_STEP).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emails(n: usize) -> Vec<Target> {
        (0..n).map(|i| Target::Email { user_id: Uuid::from_u128(i as u128 + 1) }).collect()
    }

    #[test]
    fn recipients_are_deduplicated_by_key_and_ordered() {
        let user = Uuid::from_u128(7);
        let channel = Uuid::from_u128(9);
        let targets = dedup([
            Target::Email { user_id: user },
            Target::Channel { channel_id: channel, user_id: Some(user) },
            Target::Email { user_id: user },
            Target::Channel { channel_id: channel, user_id: None },
        ]);
        assert_eq!(
            targets,
            [
                Target::Channel { channel_id: channel, user_id: Some(user) },
                Target::Email { user_id: user }
            ]
        );
    }

    #[test]
    fn a_step_notifies_at_most_its_recipient_cap() {
        assert_eq!(dedup(emails(MAX_RECIPIENTS_PER_STEP)).len(), MAX_RECIPIENTS_PER_STEP);
        assert_eq!(dedup(emails(MAX_RECIPIENTS_PER_STEP + 1)).len(), MAX_RECIPIENTS_PER_STEP);
    }

    #[test]
    fn skipped_steps_name_their_weekdays() {
        assert_eq!(weekdays(&[1, 5, 7]), "Mon,Fri,Sun");
        assert_eq!(weekdays(&[0, 8]), "");
    }
}
