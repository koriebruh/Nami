//! Heartbeat state machine: pings (`record_ping`), the overdue sweep
//! (`sweep_overdue`) and the worker's liveness record, which keeps an outage
//! of the engine from counting against the jobs (`note_worker_alive`). Same
//! transitions as monitors: going down opens an incident, a ping on a down
//! heartbeat resolves it.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::alerts::{Subject, Transition, TransitionKind};
use crate::models::incident;

#[derive(Debug, PartialEq)]
pub enum PingOutcome {
    UnknownToken,
    Recorded { transition: Option<Transition> },
}

/// The longest string that can be a token (they are 64 hex characters, see
/// `models::heartbeat::insert_heartbeat`).
const MAX_TOKEN_LEN: usize = 128;

/// Whether `token` could be one: printable ASCII, not absurdly long. Anything
/// else cannot match a row, so [`record_ping`] answers "unknown" without a
/// query. Besides saving the round trip, this keeps a NUL byte (Postgres
/// refuses it in text, which would be a 500 for whoever sent it) an ordinary
/// unknown token.
fn could_be_a_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_TOKEN_LEN
        && token.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Records a ping in one transaction, under the heartbeat's row lock so a
/// ping and a concurrent sweep apply one after the other.
///
/// An unknown token costs one plain indexed lookup and no transaction: only a
/// token that exists gets the locking transaction, which looks it up again
/// (so a heartbeat deleted, or its token rotated, in between is unknown too:
/// once a rotation has committed, the old token records nothing).
///
/// Paused heartbeats keep accepting pings — `last_ping_at` stays fresh — but
/// their state is frozen: no status change, no incident change, no alert.
pub async fn record_ping(pool: &PgPool, token: &str) -> Result<PingOutcome, sqlx::Error> {
    if !could_be_a_token(token) {
        return Ok(PingOutcome::UnknownToken);
    }
    let known: bool =
        sqlx::query_scalar("select exists (select 1 from heartbeats where token = $1)")
            .bind(token)
            .fetch_one(pool)
            .await?;
    if !known {
        return Ok(PingOutcome::UnknownToken);
    }
    let mut tx = pool.begin().await?;
    let row: Option<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
        "select id, project_id, name, current_status, paused from heartbeats where token = $1 for update",
    )
    .bind(token)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, project_id, name, status, paused)) = row else {
        return Ok(PingOutcome::UnknownToken);
    };

    if paused {
        sqlx::query("update heartbeats set last_ping_at = now() where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(PingOutcome::Recorded { transition: None });
    }

    let pinged_at: DateTime<Utc> = sqlx::query_scalar(
        "update heartbeats set last_ping_at = now(), current_status = 'up' where id = $1 returning last_ping_at",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let transition = if status == "down" {
        incident::resolve_for_heartbeat(&mut tx, id, pinged_at).await?;
        Some(Transition {
            kind: TransitionKind::Recovered,
            subject: Subject::Heartbeat(id),
            project_id,
            name,
            cause: None,
            at: pinged_at,
        })
    } else {
        None
    };
    tx.commit().await?;
    Ok(PingOutcome::Recorded { transition })
}

/// When a heartbeat is due: its period and grace after its anchor. The anchor
/// is the latest of its last ping, its last resume from a pause, the end of the
/// last outage its deadline fell into (see [`note_worker_alive`]) and its
/// creation (a missing ping, resume or outage counts as the creation). The
/// interval is computed in float8 so a huge period plus grace cannot overflow
/// int4 and break the sweep for every heartbeat. One text, used by the sweep's
/// filter and ordering and by the outage catch-up below.
macro_rules! due_at {
    () => {
        "greatest(coalesce(last_ping_at, created_at), coalesce(resumed_at, created_at),
                  coalesce(outage_anchor_at, created_at))
         + make_interval(secs => period_seconds::float8 + grace_seconds::float8)"
    };
}

/// Marks up to `limit` overdue heartbeats as down, the most overdue first, and
/// opens their incidents, in one transaction. A heartbeat is overdue when it
/// is unpaused, not down yet, and past its due time (`due_at!`).
///
/// The heartbeats are claimed with `for update skip locked`, and the
/// `current_status <> 'down'` guard is re-checked under each row's lock. A
/// heartbeat another transaction holds (a ping, an edit, a concurrent sweep) is
/// skipped, not waited for: the next sweep takes it, and no sweep can be stuck
/// behind one row, delaying every other heartbeat. Two concurrent sweeps take
/// different heartbeats, so none is flipped (or alerted on) twice.
pub async fn sweep_overdue(pool: &PgPool, limit: i64) -> Result<Vec<Transition>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let overdue: Vec<(Uuid, Uuid, String, i32, i32)> = sqlx::query_as(concat!(
        "update heartbeats set current_status = 'down'
         where id in (
             select id from heartbeats
             where not paused
               and current_status <> 'down'
               and ",
        due_at!(),
        " < now()
             order by ",
        due_at!(),
        "
             limit $1
             for update skip locked
         )
         returning id, project_id, name, period_seconds, grace_seconds"
    ))
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    // now() is the transaction's start time: the same instant the UPDATE used.
    let now: DateTime<Utc> = sqlx::query_scalar("select now()")
        .fetch_one(&mut *tx)
        .await?;

    let mut transitions = Vec::new();
    for (id, project_id, name, period, grace) in overdue {
        let cause = format!("no ping received within {period}s (+{grace}s grace)");
        let opened = incident::open_for_heartbeat(
            &mut tx,
            project_id,
            id,
            &format!("{name} missed its heartbeat"),
            Some(&cause),
            now,
        )
        .await?;
        if opened.is_some() {
            transitions.push(Transition {
                kind: TransitionKind::Down,
                subject: Subject::Heartbeat(id),
                project_id,
                name,
                cause: Some(cause),
                at: now,
            });
        }
    }
    tx.commit().await?;
    Ok(transitions)
}

/// A gap this long between two records of the worker being alive is an outage
/// even though the process kept running: the database was unreachable (or the
/// worker stuck), and pings, which need the database, failed all along. The
/// worker records its liveness every few seconds (`worker::SWEEP_INTERVAL`).
pub const OUTAGE_GAP: std::time::Duration = std::time::Duration::from_secs(30);

/// An outage the worker made up for: no ping could be received from `from`,
/// the last moment the worker was known to be alive, to `to`, when it was back.
#[derive(Debug, Clone, PartialEq)]
pub struct Outage {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// How many heartbeats had their deadline inside it and got a fresh period.
    pub extended: u64,
}

/// Records that the worker is alive now, first making up for an outage since
/// the previous record, in one transaction (the record's row lock keeps two
/// workers from doing it at once). The worker calls it before every sweep,
/// with `starting` true until the first call after its start succeeds.
///
/// The time since the previous record counts as an outage when the worker is
/// starting, or when it is longer than [`OUTAGE_GAP`]. A heartbeat whose
/// deadline fell inside the outage could not have been pinged on time, so it
/// gets a full period from the end of the outage (`outage_anchor_at`, which
/// later restarts keep). Every other heartbeat is judged as usual: one that was
/// overdue before the outage began goes down at the next sweep, however often
/// the engine restarts, and one due later keeps its deadline. With no previous
/// record (the first start ever) there is nothing to make up for.
///
/// All times are the database's, like the pings' and the heartbeats' own.
pub async fn note_worker_alive(
    pool: &PgPool,
    starting: bool,
) -> Result<Option<Outage>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let last_alive: Option<DateTime<Utc>> =
        sqlx::query_scalar("select last_alive_at from worker_state where id = 1 for update")
            .fetch_optional(&mut *tx)
            .await?;
    let now: DateTime<Utc> = sqlx::query_scalar("select now()")
        .fetch_one(&mut *tx)
        .await?;
    let outage = match last_alive {
        Some(from) if from < now && (starting || is_outage_gap(now - from)) => {
            let extended = sqlx::query(concat!(
                "update heartbeats set outage_anchor_at = $2
                 where not paused
                   and current_status <> 'down'
                   and ",
                due_at!(),
                " > $1
                   and ",
                due_at!(),
                " <= $2"
            ))
            .bind(from)
            .bind(now)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            Some(Outage {
                from,
                to: now,
                extended,
            })
        }
        _ => None,
    };
    sqlx::query(
        "insert into worker_state (id, last_alive_at) values (1, $1)
         on conflict (id) do update set last_alive_at = excluded.last_alive_at",
    )
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(outage)
}

fn is_outage_gap(gap: chrono::TimeDelta) -> bool {
    gap.to_std().is_ok_and(|gap| gap > OUTAGE_GAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_could_be_tokens() {
        for _ in 0..50 {
            let (token, _hash) = crate::auth::token::new_opaque_token();
            assert_eq!(token.len(), 64);
            assert!(could_be_a_token(&token), "{token}");
        }
        assert!(could_be_a_token("not-a-real-token"));
    }

    #[test]
    fn the_length_limit_is_exact() {
        assert!(could_be_a_token(&"a".repeat(MAX_TOKEN_LEN)));
        assert!(!could_be_a_token(&"a".repeat(MAX_TOKEN_LEN + 1)));
        assert!(!could_be_a_token(""));
    }

    #[test]
    fn nul_whitespace_control_and_non_ascii_text_cannot_be_a_token() {
        for text in [
            "a\0b",
            "\0",
            "a b",
            " ",
            "\t",
            "a\nb",
            "a\r\nb",
            "\u{7f}",
            "\u{20ac}",
            "caf\u{e9}",
            "a\u{200b}b",
        ] {
            assert!(!could_be_a_token(text), "{text:?}");
        }
    }
}
