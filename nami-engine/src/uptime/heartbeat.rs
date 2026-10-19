//! Heartbeat state machine: pings (`record_signal`, `record_ping`), the overdue sweep
//! (`sweep_overdue`) and the worker's liveness record, which keeps an outage
//! of the engine from counting against the jobs (`note_worker_alive`). Same
//! transitions as monitors: going down opens an incident, a ping on a down
//! heartbeat resolves it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::alerts::{Subject, Transition, TransitionKind};
use crate::models::{incident, maintenance};

#[derive(Debug, PartialEq)]
pub enum PingOutcome {
    UnknownToken,
    Recorded { transition: Option<Transition> },
}

/// A ping token's length: 32 random bytes, hex-encoded (see
/// `models::heartbeat::insert_heartbeat`).
pub const TOKEN_LEN: usize = 64;

/// Whether `token` has a token's exact shape: [`TOKEN_LEN`] lowercase hex
/// characters. Anything else cannot match a row, so it is answered "unknown"
/// without touching the database. Besides saving the round trip (the ping
/// endpoint is public), this keeps a NUL byte (Postgres refuses it in text,
/// which would be a 500 for whoever sent it) an ordinary unknown token.
pub fn is_well_formed_token(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// What a ping says about the job: it ran fine, or it failed.
///
/// Serialized in lowercase (`success` / `fail`): the ping log stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PingKind {
    Success,
    Fail,
}

/// One ping: its kind, the exit code its path carried (if any) and the body
/// the job sent with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ping {
    pub kind: PingKind,
    pub exit_code: Option<i32>,
    pub output: String,
}

impl Ping {
    /// A plain success ping (`/{token}`): no exit code, no output.
    pub fn success() -> Ping {
        Ping {
            kind: PingKind::Success,
            exit_code: None,
            output: String::new(),
        }
    }
}

/// Reads the path segment after the token: `fail` is a failure without an
/// exit code; an `i32` is an exit code, `0` a success and anything else a
/// failure. Every other text (`FAIL`, `1.5`, a number beyond `i32`) is `None`.
pub fn parse_signal(segment: &str) -> Option<(PingKind, Option<i32>)> {
    if segment == "fail" {
        return Some((PingKind::Fail, None));
    }
    // `str::parse` accepts a leading `+`; nothing a cron job prints, but
    // harmless: `+1` is exit code 1.
    let code: i32 = segment.parse().ok()?;
    if code == 0 {
        Some((PingKind::Success, Some(0)))
    } else {
        Some((PingKind::Fail, Some(code)))
    }
}

/// The incident cause of a failure ping.
pub fn failure_cause(exit_code: Option<i32>) -> String {
    match exit_code {
        Some(code) => format!("Heartbeat reported failure (exit code {code})"),
        None => "Heartbeat reported failure".to_string(),
    }
}

/// The id of the heartbeat that holds `token`, in one plain indexed lookup
/// (no transaction, no lock). Text that cannot be a token is `None` without a
/// query.
pub async fn lookup_token(pool: &PgPool, token: &str) -> Result<Option<Uuid>, sqlx::Error> {
    if !is_well_formed_token(token) {
        return Ok(None);
    }
    sqlx::query_scalar("select id from heartbeats where token = $1")
        .bind(token)
        .fetch_optional(pool)
        .await
}

/// A ping [`record_signal`] recorded: whose it was, and the state change it
/// made, if any.
#[derive(Debug, PartialEq)]
pub struct Recorded {
    pub heartbeat_id: Uuid,
    pub project_id: Uuid,
    pub transition: Option<Transition>,
}

/// Records a ping in one transaction, under the heartbeat's row lock so a
/// ping and a concurrent sweep apply one after the other. `None` when no
/// heartbeat holds `token` under the lock (a heartbeat deleted, or its token
/// rotated, since the caller looked it up: once a rotation has committed, the
/// old token records nothing).
///
/// A success ping marks the heartbeat up and resolves its incident if it was
/// down. A failure ping on a pending or up heartbeat marks it down at once and
/// opens an incident (title `{name} reported a failure`, cause
/// [`failure_cause`]); on a heartbeat already down it only moves
/// `last_ping_at` (the open incident keeps the cause that opened it).
///
/// Paused heartbeats, and heartbeats a project-wide maintenance window covers
/// (judged at the transaction's `now()`, the instant `last_ping_at` records),
/// keep accepting pings — `last_ping_at` stays fresh, so resuming, or the
/// window ending, does not immediately trip the sweep — but their state is
/// frozen: no status change, no incident change, no alert, failure pings
/// included.
pub async fn record_signal(
    pool: &PgPool,
    token: &str,
    ping: &Ping,
) -> Result<Option<Recorded>, sqlx::Error> {
    if !is_well_formed_token(token) {
        return Ok(None);
    }
    let mut tx = pool.begin().await?;
    let row: Option<(Uuid, Uuid, String, String, bool, DateTime<Utc>)> = sqlx::query_as(
        "select id, project_id, name, current_status, paused, now()
         from heartbeats where token = $1 for update",
    )
    .bind(token)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, project_id, name, status, paused, now)) = row else {
        return Ok(None);
    };

    let frozen = paused || maintenance::heartbeat_in_maintenance(&mut *tx, project_id, now).await?;
    let transition = match (frozen, ping.kind) {
        (true, _) => {
            touch(&mut tx, id).await?;
            None
        }
        (false, PingKind::Fail) if status == "down" => {
            touch(&mut tx, id).await?;
            None
        }
        (false, PingKind::Success) => {
            let pinged_at: DateTime<Utc> = sqlx::query_scalar(
                "update heartbeats set last_ping_at = now(), current_status = 'up' where id = $1 returning last_ping_at",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            if status == "down" {
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
            }
        }
        (false, PingKind::Fail) => {
            sqlx::query(
                "update heartbeats set last_ping_at = now(), current_status = 'down' where id = $1",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
            let cause = failure_cause(ping.exit_code);
            let opened = incident::open_for_heartbeat(
                &mut tx,
                project_id,
                id,
                &format!("{name} reported a failure"),
                Some(&cause),
                now,
            )
            .await?;
            opened.map(|_| Transition {
                kind: TransitionKind::Down,
                subject: Subject::Heartbeat(id),
                project_id,
                name,
                cause: Some(cause),
                at: now,
            })
        }
    };
    tx.commit().await?;
    Ok(Some(Recorded {
        heartbeat_id: id,
        project_id,
        transition,
    }))
}

/// Records that a ping arrived and changes nothing else.
async fn touch(tx: &mut sqlx::PgConnection, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("update heartbeats set last_ping_at = now() where id = $1")
        .bind(id)
        .execute(tx)
        .await?;
    Ok(())
}

/// Records a plain success ping (see [`record_signal`]).
///
/// An unknown token costs one plain indexed lookup ([`lookup_token`]) and no
/// transaction: only a token that exists gets the locking transaction, which
/// looks it up again.
pub async fn record_ping(pool: &PgPool, token: &str) -> Result<PingOutcome, sqlx::Error> {
    if lookup_token(pool, token).await?.is_none() {
        return Ok(PingOutcome::UnknownToken);
    }
    Ok(match record_signal(pool, token, &Ping::success()).await? {
        None => PingOutcome::UnknownToken,
        Some(recorded) => PingOutcome::Recorded {
            transition: recorded.transition,
        },
    })
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
///
/// Heartbeats of a project inside a project-wide maintenance window (judged at
/// the transaction's `now()`, the instant the UPDATE compares against) never go
/// down, and their deadline does not run out during the window: each sweep
/// moves the anchor of those that are overdue to `now()` (`outage_anchor_at`,
/// the same anchor an engine outage sets), so after the window a heartbeat
/// gets a full period and grace before it can go down. One still on time is
/// left alone and keeps its deadline. The re-anchoring skips locked rows like
/// the flip does; a row skipped is re-anchored by the next sweep.
pub async fn sweep_overdue(pool: &PgPool, limit: i64) -> Result<Vec<Transition>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // now() is the transaction's start time: the same instant the UPDATE uses.
    let now: DateTime<Utc> = sqlx::query_scalar("select now()")
        .fetch_one(&mut *tx)
        .await?;
    let in_maintenance =
        maintenance::projects_with_heartbeats_in_maintenance(&mut *tx, now).await?;
    if !in_maintenance.is_empty() {
        sqlx::query(concat!(
            "update heartbeats set outage_anchor_at = now()
             where id in (
                 select id from heartbeats
                 where project_id = any($1)
                   and not paused
                   and current_status <> 'down'
                   and ",
            due_at!(),
            " < now()
                 for update skip locked
             )"
        ))
        .bind(&in_maintenance)
        .execute(&mut *tx)
        .await?;
    }
    let overdue: Vec<(Uuid, Uuid, String, i32, i32)> = sqlx::query_as(concat!(
        "update heartbeats set current_status = 'down'
         where id in (
             select id from heartbeats
             where not paused
               and current_status <> 'down'
               and project_id <> all($2)
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
    .bind(&in_maintenance)
    .fetch_all(&mut *tx)
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
    fn generated_tokens_are_well_formed() {
        for _ in 0..50 {
            let (token, _hash) = crate::auth::token::new_opaque_token();
            assert_eq!(token.len(), TOKEN_LEN);
            assert!(is_well_formed_token(&token), "{token}");
        }
        assert!(is_well_formed_token(&"0123456789abcdef".repeat(4)));
    }

    #[test]
    fn the_length_is_exact() {
        assert!(is_well_formed_token(&"a".repeat(TOKEN_LEN)));
        assert!(!is_well_formed_token(&"a".repeat(TOKEN_LEN - 1)));
        assert!(!is_well_formed_token(&"a".repeat(TOKEN_LEN + 1)));
        assert!(!is_well_formed_token(""));
    }

    #[test]
    fn only_lowercase_hex_digits_are_accepted() {
        for bad in [
            'A', 'F', 'g', 'z', '-', '_', '.', '/', '%', ' ', '\0', '\n', '\u{7f}', '\u{e9}',
        ] {
            let token = format!("{}{bad}", "a".repeat(TOKEN_LEN - 1));
            assert!(!is_well_formed_token(&token), "{bad:?}");
        }
        assert!(!is_well_formed_token("not-a-real-token"));
        assert!(
            !is_well_formed_token(&"A".repeat(TOKEN_LEN)),
            "uppercase hex"
        );
        // 64 bytes but fewer characters: never a token either.
        assert!(!is_well_formed_token(&"\u{e9}".repeat(TOKEN_LEN / 2)));
    }

    #[test]
    fn signals_parse_to_kind_and_exit_code() {
        assert_eq!(parse_signal("fail"), Some((PingKind::Fail, None)));
        assert_eq!(parse_signal("0"), Some((PingKind::Success, Some(0))));
        assert_eq!(parse_signal("1"), Some((PingKind::Fail, Some(1))));
        assert_eq!(parse_signal("137"), Some((PingKind::Fail, Some(137))));
        assert_eq!(parse_signal("-1"), Some((PingKind::Fail, Some(-1))));
        assert_eq!(
            parse_signal("2147483647"),
            Some((PingKind::Fail, Some(i32::MAX)))
        );
        for bad in [
            "",
            "FAIL",
            "Fail",
            "fail ",
            "1.5",
            "2147483648",
            "0x1",
            "abc",
            "1e3",
            "/",
        ] {
            assert_eq!(parse_signal(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn failure_causes_name_the_exit_code_when_there_is_one() {
        assert_eq!(
            failure_cause(Some(2)),
            "Heartbeat reported failure (exit code 2)"
        );
        assert_eq!(failure_cause(None), "Heartbeat reported failure");
    }

    #[test]
    fn ping_kinds_serialize_in_lowercase() {
        assert_eq!(serde_json::to_string(&PingKind::Fail).unwrap(), "\"fail\"");
        assert_eq!(
            serde_json::from_str::<PingKind>("\"success\"").unwrap(),
            PingKind::Success
        );
        assert_eq!(Ping::success().exit_code, None);
    }
}
