//! Heartbeat state machine: pings (`record_ping`) and the overdue sweep
//! (`sweep_overdue`). Same transitions as monitors: going down opens an
//! incident, a ping on a down heartbeat resolves it.

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
/// Paused heartbeats keep accepting pings — `last_ping_at` stays fresh, so
/// resuming does not immediately trip the sweep — but their state is frozen:
/// no status change, no incident change, no alert.
pub async fn record_ping(pool: &PgPool, token: &str) -> Result<PingOutcome, sqlx::Error> {
    if !could_be_a_token(token) {
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

/// Marks every unpaused heartbeat whose last ping (or creation, if it never
/// pinged) is older than `period + grace` as down and opens its incident,
/// in one transaction. The `current_status <> 'down'` guard is re-checked
/// under each row's lock, so two concurrent sweeps cannot both flip (and
/// alert on) the same heartbeat. The interval is computed in float8 so a
/// huge period plus grace cannot overflow int4 and break the sweep for
/// every heartbeat.
pub async fn sweep_overdue(pool: &PgPool) -> Result<Vec<Transition>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let overdue: Vec<(Uuid, Uuid, String, i32, i32)> = sqlx::query_as(
        "update heartbeats set current_status = 'down'
         where not paused
           and current_status <> 'down'
           and coalesce(last_ping_at, created_at)
               + make_interval(secs => period_seconds::float8 + grace_seconds::float8) < now()
         returning id, project_id, name, period_seconds, grace_seconds",
    )
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
