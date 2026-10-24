//! Log alerts as stored: their settings, validation and CRUD.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    core::validate::validate_label,
    logs::view::saved_query::{LogQuery, MAX_QUERY_BYTES},
};

/// Most log alerts a project may hold (bounds the evaluation load).
pub const MAX_LOG_ALERTS_PER_PROJECT: i64 = 100;
/// Largest threshold (count) an alert may compare with.
pub const MAX_THRESHOLD: i64 = 1_000_000_000_000;
/// Allowed `window_seconds` (1 min to 24 h).
pub const WINDOW_RANGE: std::ops::RangeInclusive<i32> = 60..=86_400;
/// Allowed `interval_seconds` (1 min to 24 h).
pub const INTERVAL_RANGE: std::ops::RangeInclusive<i32> = 60..=86_400;

/// How the count compares with the threshold; Postgres `text`
/// (`log_alerts_comparison_known`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    /// Fires when `count > threshold`.
    Gt,
    /// Fires when `count < threshold`.
    Lt,
}

impl Comparison {
    /// The comparison named `value`, or a message for a 400.
    pub fn parse(value: &str) -> Result<Comparison, String> {
        match value {
            "gt" => Ok(Comparison::Gt),
            "lt" => Ok(Comparison::Lt),
            _ => Err("comparison must be gt or lt".to_string()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Comparison::Gt => "gt",
            Comparison::Lt => "lt",
        }
    }
}

impl TryFrom<String> for Comparison {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Comparison::parse(&value)
    }
}

/// An alert's state; Postgres `text` (`log_alerts_state_known`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    Ok,
    Alerting,
}

impl AlertState {
    /// The state stored as `value`, or an error for any other text.
    pub fn parse(value: &str) -> Result<AlertState, String> {
        match value {
            "ok" => Ok(AlertState::Ok),
            "alerting" => Ok(AlertState::Alerting),
            _ => Err(format!("unknown log alert state {value:?}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AlertState::Ok => "ok",
            AlertState::Alerting => "alerting",
        }
    }

    /// The state after an evaluation (R-P2-18): a project-wide maintenance
    /// window (`held`) keeps it, otherwise it follows the condition.
    pub fn next(self, met: bool, held: bool) -> AlertState {
        match (held, met) {
            (true, _) => self,
            (false, true) => AlertState::Alerting,
            (false, false) => AlertState::Ok,
        }
    }
}

impl TryFrom<String> for AlertState {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        AlertState::parse(&value)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct LogAlert {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub query: sqlx::types::Json<LogQuery>,
    pub threshold: i64,
    #[sqlx(try_from = "String")]
    pub comparison: Comparison,
    pub window_seconds: i32,
    pub interval_seconds: i32,
    pub enabled: bool,
    #[sqlx(try_from = "String")]
    pub state: AlertState,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub last_count: Option<i64>,
    /// Why the last evaluation failed (a fixed, user-safe text); `None`
    /// after a success.
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub next_evaluation_at: DateTime<Utc>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The escalation policy of its incidents; `None` is the project default.
    pub escalation_policy_id: Option<Uuid>,
    /// The severity of its incidents; `None` is the project default.
    pub severity_id: Option<Uuid>,
}

/// Everything a user sets on an alert.
#[derive(Debug, Clone, PartialEq)]
pub struct LogAlertConfig {
    pub name: String,
    pub query: LogQuery,
    pub threshold: i64,
    pub comparison: Comparison,
    pub window_seconds: i32,
    pub interval_seconds: i32,
    pub enabled: bool,
    pub escalation_policy_id: Option<Uuid>,
    pub severity_id: Option<Uuid>,
}

/// The config with its name trimmed, or the first problem as a message for
/// a 400. The query is validated exactly like search and capped at
/// [`MAX_QUERY_BYTES`]; whether its sources belong to the project is
/// checked by the caller (`saved_query::check_sources`).
pub fn validate(config: LogAlertConfig) -> Result<LogAlertConfig, String> {
    let name = validate_label("name", &config.name)?;
    if !(0..=MAX_THRESHOLD).contains(&config.threshold) {
        return Err(format!("threshold must be between 0 and {MAX_THRESHOLD}"));
    }
    if config.comparison == Comparison::Lt && config.threshold < 1 {
        return Err("lt needs a threshold of at least 1".to_string());
    }
    if !WINDOW_RANGE.contains(&config.window_seconds) {
        return Err(format!(
            "window_seconds must be between {} and {}",
            WINDOW_RANGE.start(),
            WINDOW_RANGE.end()
        ));
    }
    if !INTERVAL_RANGE.contains(&config.interval_seconds) {
        return Err(format!(
            "interval_seconds must be between {} and {}",
            INTERVAL_RANGE.start(),
            INTERVAL_RANGE.end()
        ));
    }
    config.query.to_filters()?;
    match serde_json::to_vec(&config.query) {
        Ok(bytes) if bytes.len() <= MAX_QUERY_BYTES => Ok(LogAlertConfig { name, ..config }),
        _ => Err(format!("query must be at most {MAX_QUERY_BYTES} bytes")),
    }
}

/// What [`create`] did: created, refused at [`MAX_LOG_ALERTS_PER_PROJECT`]
/// (409), or the project does not exist (404).
// Short-lived return value; boxing would change the interface callers use.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum CreatedAlert {
    Created(LogAlert),
    ProjectFull,
    NoProject,
}

// sqlx takes only `&'static str` SQL, so the column list is a macro.
macro_rules! columns {
    () => {
        "id, project_id, name, query, threshold, comparison, window_seconds, interval_seconds,
         enabled, state, last_evaluated_at, last_count, last_error, last_error_at,
         next_evaluation_at, created_by, created_at, updated_at, escalation_policy_id, severity_id"
    };
}

/// Every alert of the project, by name.
pub async fn list_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<LogAlert>, sqlx::Error> {
    sqlx::query_as(concat!(
        "select ",
        columns!(),
        " from log_alerts where project_id = $1 order by name, id"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// The alert, or `None` when it is not in the project.
pub async fn get(
    pool: &PgPool,
    project_id: Uuid,
    alert_id: Uuid,
) -> Result<Option<LogAlert>, sqlx::Error> {
    sqlx::query_as(concat!(
        "select ",
        columns!(),
        " from log_alerts where id = $1 and project_id = $2"
    ))
    .bind(alert_id)
    .bind(project_id)
    .fetch_optional(pool)
    .await
}

/// The project row is locked for the count and the insert, so concurrent
/// creates cannot both pass [`MAX_LOG_ALERTS_PER_PROJECT`]. A new alert is
/// due at once.
pub async fn create(
    pool: &PgPool,
    project_id: Uuid,
    config: &LogAlertConfig,
    created_by: Uuid,
) -> Result<CreatedAlert, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let locked: Option<Uuid> =
        sqlx::query_scalar("select id from projects where id = $1 for no key update")
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    if locked.is_none() {
        return Ok(CreatedAlert::NoProject);
    }
    let count: i64 = sqlx::query_scalar("select count(*) from log_alerts where project_id = $1")
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_LOG_ALERTS_PER_PROJECT {
        return Ok(CreatedAlert::ProjectFull);
    }
    let alert = sqlx::query_as(concat!(
        "insert into log_alerts (project_id, name, query, threshold, comparison, window_seconds,
                                 interval_seconds, enabled, created_by, escalation_policy_id, severity_id)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) returning ",
        columns!(),
        ""
    ))
    .bind(project_id)
    .bind(&config.name)
    .bind(sqlx::types::Json(&config.query))
    .bind(config.threshold)
    .bind(config.comparison.as_str())
    .bind(config.window_seconds)
    .bind(config.interval_seconds)
    .bind(config.enabled)
    .bind(created_by)
    .bind(config.escalation_policy_id)
    .bind(config.severity_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(CreatedAlert::Created(alert))
}

/// A PATCH: `None` keeps the stored value.
#[derive(Debug)]
pub struct LogAlertPatch {
    pub name: Option<String>,
    pub query: Option<LogQuery>,
    pub threshold: Option<i64>,
    pub comparison: Option<Comparison>,
    pub window_seconds: Option<i32>,
    pub interval_seconds: Option<i32>,
    pub enabled: Option<bool>,
    /// `Some(None)` returns to the project's default.
    pub escalation_policy_id: Option<Option<Uuid>>,
    pub severity_id: Option<Option<Uuid>>,
}

impl LogAlertPatch {
    /// The stored alert's config with the patched fields replaced.
    fn merge(self, stored: LogAlert) -> LogAlertConfig {
        LogAlertConfig {
            name: self.name.unwrap_or(stored.name),
            query: self.query.unwrap_or(stored.query.0),
            threshold: self.threshold.unwrap_or(stored.threshold),
            comparison: self.comparison.unwrap_or(stored.comparison),
            window_seconds: self.window_seconds.unwrap_or(stored.window_seconds),
            interval_seconds: self.interval_seconds.unwrap_or(stored.interval_seconds),
            enabled: self.enabled.unwrap_or(stored.enabled),
            escalation_policy_id: self.escalation_policy_id.unwrap_or(stored.escalation_policy_id),
            severity_id: self.severity_id.unwrap_or(stored.severity_id),
        }
    }
}

/// What [`update`] did.
// Short-lived return value; boxing would change the interface callers use.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum UpdatedAlert {
    Updated(LogAlert),
    /// No such alert in the project (404).
    NotFound,
    /// The merged config is invalid: the message for a 400 ([`validate`]).
    Invalid(String),
}

/// Reads the alert under its row lock, merges `patch` into it, validates
/// the result and writes it, all in one transaction, so two PATCHes of
/// different fields never undo each other. Bumps `updated_at` (an
/// evaluation already running is then dropped) and makes the alert due at
/// once. Disabling an `alerting` alert sets it `ok` and resolves its
/// incident with the reason `alert disabled` (R17; the caller wakes the
/// incidents worker). Whether the patch's sources, policy and severity
/// belong to the project is checked by the caller.
pub async fn update(
    pool: &PgPool,
    project_id: Uuid,
    alert_id: Uuid,
    patch: LogAlertPatch,
) -> Result<UpdatedAlert, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let stored: Option<LogAlert> = sqlx::query_as(concat!(
        "select ",
        columns!(),
        " from log_alerts where id = $1 and project_id = $2 for update"
    ))
    .bind(alert_id)
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(stored) = stored else {
        return Ok(UpdatedAlert::NotFound);
    };
    let was = stored.state;
    let config = match validate(patch.merge(stored)) {
        Ok(config) => config,
        Err(message) => return Ok(UpdatedAlert::Invalid(message)),
    };
    let alert: LogAlert = sqlx::query_as(concat!(
        "update log_alerts
         set name = $3, query = $4, threshold = $5, comparison = $6, window_seconds = $7,
             interval_seconds = $8, enabled = $9,
             state = case when $9 then state else 'ok' end,
             escalation_policy_id = $10, severity_id = $11,
             updated_at = now(), next_evaluation_at = now()
         where id = $1 and project_id = $2 returning ",
        columns!(),
        ""
    ))
    .bind(alert_id)
    .bind(project_id)
    .bind(&config.name)
    .bind(sqlx::types::Json(&config.query))
    .bind(config.threshold)
    .bind(config.comparison.as_str())
    .bind(config.window_seconds)
    .bind(config.interval_seconds)
    .bind(config.enabled)
    .bind(config.escalation_policy_id)
    .bind(config.severity_id)
    .fetch_one(&mut *tx)
    .await?;
    if !config.enabled && was == AlertState::Alerting {
        super::resolve_alert_incident(&mut tx, alert_id, project_id, &alert.name, Utc::now(), false)
            .await?;
    }
    tx.commit().await?;
    Ok(UpdatedAlert::Updated(alert))
}

/// Deletes the alert and, by cascade, its incidents. `false` = no such
/// alert in the project.
pub async fn delete(pool: &PgPool, project_id: Uuid, alert_id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from log_alerts where id = $1 and project_id = $2")
        .bind(alert_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LogAlertConfig {
        LogAlertConfig {
            name: " Errors ".to_string(),
            query: LogQuery::default(),
            threshold: 0,
            comparison: Comparison::Gt,
            window_seconds: *WINDOW_RANGE.start(),
            interval_seconds: *INTERVAL_RANGE.start(),
            enabled: true,
            escalation_policy_id: None,
            severity_id: None,
        }
    }

    #[test]
    fn every_bound_holds_at_its_limit_and_fails_past_it() {
        assert_eq!(validate(config()).unwrap().name, "Errors");
        let at_the_top = LogAlertConfig {
            threshold: MAX_THRESHOLD,
            window_seconds: *WINDOW_RANGE.end(),
            interval_seconds: *INTERVAL_RANGE.end(),
            ..config()
        };
        assert!(validate(at_the_top).is_ok());
        assert!(validate(LogAlertConfig { comparison: Comparison::Lt, threshold: 1, ..config() }).is_ok());
        for bad in [
            LogAlertConfig { threshold: MAX_THRESHOLD + 1, ..config() },
            LogAlertConfig { threshold: -1, ..config() },
            LogAlertConfig { comparison: Comparison::Lt, ..config() },
            LogAlertConfig { window_seconds: WINDOW_RANGE.start() - 1, ..config() },
            LogAlertConfig { window_seconds: WINDOW_RANGE.end() + 1, ..config() },
            LogAlertConfig { interval_seconds: INTERVAL_RANGE.start() - 1, ..config() },
            LogAlertConfig { interval_seconds: INTERVAL_RANGE.end() + 1, ..config() },
            LogAlertConfig { name: String::new(), ..config() },
            LogAlertConfig {
                query: LogQuery { levels: vec!["loud".into()], ..LogQuery::default() },
                ..config()
            },
        ] {
            assert!(validate(bad.clone()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn only_known_comparisons_and_states_parse() {
        assert_eq!(Comparison::parse("gt"), Ok(Comparison::Gt));
        assert_eq!(Comparison::parse("lt"), Ok(Comparison::Lt));
        assert!(Comparison::parse("eq").is_err());
        assert_eq!(AlertState::parse("alerting"), Ok(AlertState::Alerting));
        assert!(AlertState::parse("paused").is_err());
    }

    #[test]
    fn maintenance_holds_the_state_and_otherwise_the_condition_decides() {
        use AlertState::{Alerting, Ok as Fine};
        assert_eq!(Fine.next(true, false), Alerting);
        assert_eq!(Alerting.next(false, false), Fine);
        assert_eq!(Fine.next(true, true), Fine);
        assert_eq!(Alerting.next(false, true), Alerting);
    }
}
