use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::{monitor_group, nullable, validate_name, with_unchanged_values};
use crate::error::AppError;

/// The longest a heartbeat's period, or its grace, may be: 31 days.
pub const MAX_SECONDS: i32 = 31 * 24 * 60 * 60;

/// What `Debug` prints in place of a ping token.
const REDACTED: &str = "<redacted>";

/// A heartbeat row. Not `Serialize` on purpose: the API returns a
/// [`HeartbeatView`], which carries the ping token only for callers who may
/// use it. `Debug` leaves the token out for the same reason.
#[derive(Clone, PartialEq, sqlx::FromRow)]
pub struct Heartbeat {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    /// The ping secret (`GET|POST /api/v1/heartbeat/{token}`). Stored in the
    /// clear, unlike auth tokens, because members keep seeing the ping URL.
    pub token: String,
    pub period_seconds: i32,
    pub grace_seconds: i32,
    pub paused: bool,
    pub current_status: String,
    pub last_ping_at: Option<DateTime<Utc>>,
    /// When it was last resumed from a pause (`None`: never). The sweep counts
    /// from the latest of this, `last_ping_at`, `created_at` and the end of the
    /// last engine outage its deadline fell into (`outage_anchor_at`, a column
    /// only the sweep reads, see `uptime::heartbeat::note_worker_alive`).
    pub resumed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// A `monitor_groups` row of the same project, or ungrouped.
    pub group_id: Option<Uuid>,
}

impl fmt::Debug for Heartbeat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Exhaustive, so that a new field cannot be left out by accident.
        let Heartbeat {
            id,
            project_id,
            name,
            token: _,
            period_seconds,
            grace_seconds,
            paused,
            current_status,
            last_ping_at,
            resumed_at,
            created_at,
            group_id,
        } = self;
        f.debug_struct("Heartbeat")
            .field("id", id)
            .field("project_id", project_id)
            .field("name", name)
            .field("token", &REDACTED)
            .field("period_seconds", period_seconds)
            .field("grace_seconds", grace_seconds)
            .field("paused", paused)
            .field("current_status", current_status)
            .field("last_ping_at", last_ping_at)
            .field("resumed_at", resumed_at)
            .field("created_at", created_at)
            .field("group_id", group_id)
            .finish()
    }
}

impl Heartbeat {
    /// The API's shape of this heartbeat. `with_token` says whether the
    /// caller may see the ping token (see [`HeartbeatView::token`]);
    /// `in_maintenance` whether a project-wide maintenance window covers it now.
    pub fn into_view(self, with_token: bool, in_maintenance: bool) -> HeartbeatView {
        let Heartbeat {
            id,
            project_id,
            name,
            token,
            period_seconds,
            grace_seconds,
            paused,
            current_status,
            last_ping_at,
            resumed_at: _,
            created_at,
            group_id,
        } = self;
        HeartbeatView {
            id,
            project_id,
            name,
            token: with_token.then_some(token),
            period_seconds,
            grace_seconds,
            paused,
            current_status,
            last_ping_at,
            created_at,
            in_maintenance,
            group_id,
        }
    }
}

/// A heartbeat as the API returns it.
#[derive(Clone, PartialEq, Serialize)]
pub struct HeartbeatView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    /// The ping secret: `null` unless the caller is a member of the project or
    /// above. Whoever holds it can ping, and a ping resolves an incident or
    /// hides a dead job, so viewers (who may only read) never get it.
    pub token: Option<String>,
    pub period_seconds: i32,
    pub grace_seconds: i32,
    pub paused: bool,
    pub current_status: String,
    pub last_ping_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// Whether a project-wide maintenance window covers it right now: pings
    /// are recorded but its state is frozen (see `uptime::heartbeat`).
    pub in_maintenance: bool,
    /// The project's monitor group this heartbeat is listed under, if any.
    pub group_id: Option<Uuid>,
}

impl fmt::Debug for HeartbeatView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let HeartbeatView {
            id,
            project_id,
            name,
            token,
            period_seconds,
            grace_seconds,
            paused,
            current_status,
            last_ping_at,
            created_at,
            in_maintenance,
            group_id,
        } = self;
        f.debug_struct("HeartbeatView")
            .field("id", id)
            .field("project_id", project_id)
            .field("name", name)
            .field("token", &token.as_ref().map(|_| REDACTED))
            .field("period_seconds", period_seconds)
            .field("grace_seconds", grace_seconds)
            .field("paused", paused)
            .field("current_status", current_status)
            .field("last_ping_at", last_ping_at)
            .field("created_at", created_at)
            .field("in_maintenance", in_maintenance)
            .field("group_id", group_id)
            .finish()
    }
}

/// The user-editable fields of a heartbeat.
#[derive(Debug, Clone, PartialEq)]
pub struct HeartbeatConfig {
    pub name: String,
    pub period_seconds: i32,
    pub grace_seconds: i32,
    pub paused: bool,
    pub group_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct CreateHeartbeatRequest {
    pub name: String,
    pub period_seconds: i32,
    #[serde(default = "default_grace_seconds")]
    pub grace_seconds: i32,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub group_id: Option<Uuid>,
}

fn default_grace_seconds() -> i32 {
    60
}

impl CreateHeartbeatRequest {
    pub fn into_config(self) -> HeartbeatConfig {
        HeartbeatConfig {
            name: self.name,
            period_seconds: self.period_seconds,
            grace_seconds: self.grace_seconds,
            paused: self.paused,
            group_id: self.group_id,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct PatchHeartbeatRequest {
    pub name: Option<String>,
    pub period_seconds: Option<i32>,
    pub grace_seconds: Option<i32>,
    pub paused: Option<bool>,
    /// Omitted keeps the group, `null` ungroups, a value regroups.
    #[serde(default, deserialize_with = "nullable")]
    pub group_id: Option<Option<Uuid>>,
}

impl PatchHeartbeatRequest {
    pub fn apply_to(self, base: HeartbeatConfig) -> HeartbeatConfig {
        HeartbeatConfig {
            name: self.name.unwrap_or(base.name),
            period_seconds: self.period_seconds.unwrap_or(base.period_seconds),
            grace_seconds: self.grace_seconds.unwrap_or(base.grace_seconds),
            paused: self.paused.unwrap_or(base.paused),
            group_id: self.group_id.unwrap_or(base.group_id),
        }
    }
}

pub fn validate(mut c: HeartbeatConfig) -> Result<HeartbeatConfig, String> {
    c.name = validate_name(&c.name)?;
    if c.period_seconds < 30 {
        return Err("period_seconds must be at least 30".into());
    }
    if c.period_seconds > MAX_SECONDS {
        return Err(format!(
            "period_seconds must be at most {MAX_SECONDS} (31 days)"
        ));
    }
    if c.grace_seconds < 0 {
        return Err("grace_seconds must not be negative".into());
    }
    if c.grace_seconds > MAX_SECONDS {
        return Err(format!(
            "grace_seconds must be at most {MAX_SECONDS} (31 days)"
        ));
    }
    Ok(c)
}

/// A new ping token: 32 random bytes, hex-encoded (the same generator as auth
/// tokens, which draws from the OS-seeded CSPRNG).
fn new_token() -> String {
    let (token, _hash) = crate::auth::token::new_opaque_token();
    token
}

pub async fn insert_heartbeat(
    pool: &PgPool,
    project_id: Uuid,
    c: &HeartbeatConfig,
) -> Result<Heartbeat, sqlx::Error> {
    sqlx::query_as(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds, paused, group_id)
         values ($1, $2, $3, $4, $5, $6, $7)
         returning *",
    )
    .bind(project_id)
    .bind(&c.name)
    .bind(new_token())
    .bind(c.period_seconds)
    .bind(c.grace_seconds)
    .bind(c.paused)
    .bind(c.group_id)
    .fetch_one(pool)
    .await
}

pub async fn list_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<Heartbeat>, sqlx::Error> {
    sqlx::query_as("select * from heartbeats where project_id = $1 order by name, id")
        .bind(project_id)
        .fetch_all(pool)
        .await
}

/// Scoped by BOTH ids, like `monitor::find_in_project`.
pub async fn find_in_project(
    pool: &PgPool,
    project_id: Uuid,
    heartbeat_id: Uuid,
) -> Result<Option<Heartbeat>, sqlx::Error> {
    sqlx::query_as("select * from heartbeats where id = $1 and project_id = $2")
        .bind(heartbeat_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// Merge-validate-write under a row lock, like `monitor::update_in_project`.
/// Changing `paused` from true to false records the resume (`resumed_at`), so
/// that the sweep gives the job a fresh period instead of judging it by pings
/// it could not send while paused.
pub async fn update_in_project(
    pool: &PgPool,
    project_id: Uuid,
    heartbeat_id: Uuid,
    patch: PatchHeartbeatRequest,
) -> Result<Option<Heartbeat>, AppError> {
    let mut tx = pool.begin().await?;
    let current: Option<Heartbeat> =
        sqlx::query_as("select * from heartbeats where id = $1 and project_id = $2 for update")
            .bind(heartbeat_id)
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let was_paused = current.paused;
    let previous_group = current.group_id;
    let stored = vec![current.name.clone()];
    let base = HeartbeatConfig {
        name: current.name,
        period_seconds: current.period_seconds,
        grace_seconds: current.grace_seconds,
        paused: current.paused,
        group_id: current.group_id,
    };
    let config = with_unchanged_values(stored, || validate(patch.apply_to(base)))
        .map_err(AppError::Validation)?;
    if let Some(group_id) = config.group_id.filter(|g| Some(*g) != previous_group) {
        // A group of another project is a 404, like any foreign id.
        if !monitor_group::belongs_to(&mut *tx, project_id, group_id).await? {
            return Err(AppError::NotFound);
        }
    }
    let resumed = was_paused && !config.paused;
    let updated: Heartbeat = sqlx::query_as(
        "update heartbeats
         set name = $3, period_seconds = $4, grace_seconds = $5, paused = $6,
             resumed_at = case when $7 then now() else resumed_at end,
             group_id = $8
         where id = $1 and project_id = $2
         returning *",
    )
    .bind(heartbeat_id)
    .bind(project_id)
    .bind(&config.name)
    .bind(config.period_seconds)
    .bind(config.grace_seconds)
    .bind(config.paused)
    .bind(resumed)
    .bind(config.group_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        // The group was deleted between the check and the write.
        sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => {
            AppError::NotFound
        }
        _ => AppError::Database(e),
    })?;
    tx.commit().await?;
    Ok(Some(updated))
}

/// Gives the heartbeat a new ping token; the old one stops working at once
/// (pings look the token up, and re-check it under the row lock). Scoped by
/// BOTH ids, like [`find_in_project`]: `None` when there is no such heartbeat
/// in this project.
pub async fn rotate_token(
    pool: &PgPool,
    project_id: Uuid,
    heartbeat_id: Uuid,
) -> Result<Option<Heartbeat>, sqlx::Error> {
    sqlx::query_as("update heartbeats set token = $3 where id = $1 and project_id = $2 returning *")
        .bind(heartbeat_id)
        .bind(project_id)
        .bind(new_token())
        .fetch_optional(pool)
        .await
}

pub async fn delete_in_project(
    pool: &PgPool,
    project_id: Uuid,
    heartbeat_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from heartbeats where id = $1 and project_id = $2")
        .bind(heartbeat_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef-the-ping-secret";

    fn heartbeat() -> Heartbeat {
        Heartbeat {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            name: "Nightly backup".to_string(),
            token: TOKEN.to_string(),
            period_seconds: 60,
            grace_seconds: 60,
            paused: false,
            current_status: "pending".to_string(),
            last_ping_at: None,
            resumed_at: None,
            created_at: DateTime::UNIX_EPOCH,
            group_id: None,
        }
    }

    fn config(period_seconds: i32, grace_seconds: i32) -> HeartbeatConfig {
        HeartbeatConfig {
            name: "Nightly backup".to_string(),
            period_seconds,
            grace_seconds,
            paused: false,
            group_id: None,
        }
    }

    #[test]
    fn debug_output_never_contains_the_token() {
        let row = heartbeat();
        let rendered = [format!("{row:?}"), format!("{row:#?}")];
        for text in rendered {
            assert!(!text.contains(TOKEN), "{text}");
            assert!(
                text.contains("<redacted>") && text.contains("Nightly backup"),
                "{text}"
            );
        }
        let view = format!("{:?}", row.into_view(true, false));
        assert!(!view.contains(TOKEN), "{view}");
        assert!(view.contains("Some(\"<redacted>\")"), "{view}");
        assert!(format!("{:?}", heartbeat().into_view(false, false)).contains("token: None"));
        // And inside other types that derive Debug.
        let held = (heartbeat(), Some(heartbeat()), vec![heartbeat()]);
        assert!(!format!("{held:?}").contains(TOKEN));
    }

    #[test]
    fn the_view_carries_the_token_only_when_asked() {
        let with = serde_json::to_value(heartbeat().into_view(true, false)).unwrap();
        let without = serde_json::to_value(heartbeat().into_view(false, false)).unwrap();
        assert_eq!(with["token"], TOKEN);
        assert!(
            without.get("token").is_some_and(|token| token.is_null()),
            "{without}"
        );
        assert!(!without.to_string().contains(TOKEN));
        // Nothing but the token differs, and the internal clock is not exposed.
        let mut with_token_removed = with.clone();
        with_token_removed["token"] = serde_json::Value::Null;
        assert_eq!(with_token_removed, without);
        assert!(with.get("resumed_at").is_none());
    }

    #[test]
    fn the_view_carries_the_group_id() {
        let none = serde_json::to_value(heartbeat().into_view(false, false)).unwrap();
        assert!(none.get("group_id").is_some_and(|g| g.is_null()), "{none}");
        let group = Uuid::from_u128(7);
        let mut row = heartbeat();
        row.group_id = Some(group);
        let some = serde_json::to_value(row.into_view(false, false)).unwrap();
        assert_eq!(some["group_id"], group.to_string());
    }

    #[test]
    fn a_patch_keeps_sets_or_clears_the_group() {
        let group = Uuid::from_u128(7);
        let base = HeartbeatConfig {
            group_id: Some(group),
            ..config(60, 60)
        };
        let keep: PatchHeartbeatRequest = serde_json::from_str(r#"{"name":"x"}"#).unwrap();
        assert_eq!(keep.apply_to(base.clone()).group_id, Some(group));
        let clear: PatchHeartbeatRequest = serde_json::from_str(r#"{"group_id":null}"#).unwrap();
        assert_eq!(clear.apply_to(base).group_id, None);
        let set: PatchHeartbeatRequest =
            serde_json::from_value(serde_json::json!({"group_id": group})).unwrap();
        assert_eq!(set.apply_to(config(60, 60)).group_id, Some(group));
    }

    #[test]
    fn period_has_limits_at_both_ends() {
        assert!(validate(config(30, 0)).is_ok());
        assert!(validate(config(MAX_SECONDS, 0)).is_ok());
        assert_eq!(
            validate(config(29, 0)).unwrap_err(),
            "period_seconds must be at least 30"
        );
        assert_eq!(
            validate(config(MAX_SECONDS + 1, 0)).unwrap_err(),
            "period_seconds must be at most 2678400 (31 days)"
        );
        assert!(validate(config(i32::MAX, 0)).is_err());
    }

    #[test]
    fn grace_has_limits_at_both_ends() {
        assert!(validate(config(60, 0)).is_ok());
        assert!(validate(config(60, MAX_SECONDS)).is_ok());
        assert_eq!(
            validate(config(60, -1)).unwrap_err(),
            "grace_seconds must not be negative"
        );
        assert_eq!(
            validate(config(60, MAX_SECONDS + 1)).unwrap_err(),
            "grace_seconds must be at most 2678400 (31 days)"
        );
        assert!(validate(config(60, i32::MAX)).is_err());
    }

    #[test]
    fn the_limit_is_31_days() {
        assert_eq!(MAX_SECONDS, 2_678_400);
    }
}
