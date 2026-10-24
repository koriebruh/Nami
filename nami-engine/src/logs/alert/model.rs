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
pub const MAX_THRESHOLD: i64 = 1_000_000_000_000;
pub const WINDOW_RANGE: std::ops::RangeInclusive<i32> = 60..=86_400;
pub const INTERVAL_RANGE: std::ops::RangeInclusive<i32> = 60..=86_400;

#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct LogAlert {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub query: sqlx::types::Json<LogQuery>,
    pub threshold: i64,
    /// `gt` or `lt`.
    pub comparison: String,
    pub window_seconds: i32,
    pub interval_seconds: i32,
    pub enabled: bool,
    /// `ok` or `alerting`.
    pub state: String,
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
}

/// Everything a user sets on an alert.
#[derive(Debug, Clone, PartialEq)]
pub struct LogAlertConfig {
    pub name: String,
    pub query: LogQuery,
    pub threshold: i64,
    pub comparison: String,
    pub window_seconds: i32,
    pub interval_seconds: i32,
    pub enabled: bool,
}

/// The config with its name trimmed, or the first problem as a message for
/// a 400. The query is validated exactly like search and capped at
/// [`MAX_QUERY_BYTES`]; whether its sources belong to the project is
/// checked by the caller (`saved_query::check_sources`).
pub fn validate(config: LogAlertConfig) -> Result<LogAlertConfig, String> {
    let name = validate_label("name", &config.name)?;
    if !matches!(config.comparison.as_str(), "gt" | "lt") {
        return Err("comparison must be gt or lt".to_string());
    }
    if !(0..=MAX_THRESHOLD).contains(&config.threshold) {
        return Err(format!("threshold must be between 0 and {MAX_THRESHOLD}"));
    }
    if config.comparison == "lt" && config.threshold < 1 {
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
         next_evaluation_at, created_by, created_at, updated_at"
    };
}

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
                                 interval_seconds, enabled, created_by)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning ",
        columns!(),
        ""
    ))
    .bind(project_id)
    .bind(&config.name)
    .bind(sqlx::types::Json(&config.query))
    .bind(config.threshold)
    .bind(&config.comparison)
    .bind(config.window_seconds)
    .bind(config.interval_seconds)
    .bind(config.enabled)
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(CreatedAlert::Created(alert))
}

/// Writes the whole (already merged and validated) config in a transaction
/// under the row lock, bumps `updated_at` (an evaluation already running
/// is then dropped) and makes the alert due at once. Disabling an
/// `alerting` alert sets it `ok` and resolves its incident without a
/// notification. `Ok(None)` = no such alert in the project.
pub async fn update(
    pool: &PgPool,
    project_id: Uuid,
    alert_id: Uuid,
    config: &LogAlertConfig,
) -> Result<Option<LogAlert>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let state: Option<String> =
        sqlx::query_scalar("select state from log_alerts where id = $1 and project_id = $2 for update")
            .bind(alert_id)
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(state) = state else {
        return Ok(None);
    };
    let alert: LogAlert = sqlx::query_as(concat!(
        "update log_alerts
         set name = $3, query = $4, threshold = $5, comparison = $6, window_seconds = $7,
             interval_seconds = $8, enabled = $9,
             state = case when $9 then state else 'ok' end,
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
    .bind(&config.comparison)
    .bind(config.window_seconds)
    .bind(config.interval_seconds)
    .bind(config.enabled)
    .fetch_one(&mut *tx)
    .await?;
    if !config.enabled && state == "alerting" {
        super::resolve_alert_incident(&mut tx, alert_id, project_id, &alert.name, Utc::now(), false)
            .await?;
    }
    tx.commit().await?;
    Ok(Some(alert))
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
            comparison: "gt".to_string(),
            window_seconds: *WINDOW_RANGE.start(),
            interval_seconds: *INTERVAL_RANGE.start(),
            enabled: true,
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
        assert!(validate(LogAlertConfig { comparison: "lt".into(), threshold: 1, ..config() }).is_ok());
        for bad in [
            LogAlertConfig { threshold: MAX_THRESHOLD + 1, ..config() },
            LogAlertConfig { threshold: -1, ..config() },
            LogAlertConfig { comparison: "lt".into(), ..config() },
            LogAlertConfig { window_seconds: WINDOW_RANGE.start() - 1, ..config() },
            LogAlertConfig { window_seconds: WINDOW_RANGE.end() + 1, ..config() },
            LogAlertConfig { interval_seconds: INTERVAL_RANGE.start() - 1, ..config() },
            LogAlertConfig { interval_seconds: INTERVAL_RANGE.end() + 1, ..config() },
            LogAlertConfig { comparison: "eq".into(), ..config() },
            LogAlertConfig { name: String::new(), ..config() },
            LogAlertConfig {
                query: LogQuery { levels: vec!["loud".into()], ..LogQuery::default() },
                ..config()
            },
        ] {
            assert!(validate(bad.clone()).is_err(), "{bad:?}");
        }
    }
}
