use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::validate_name;
use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct Heartbeat {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    /// The ping secret (`GET|POST /api/v1/heartbeat/{token}`). Stored in the
    /// clear, unlike auth tokens, because the UI keeps showing the ping URL.
    pub token: String,
    pub period_seconds: i32,
    pub grace_seconds: i32,
    pub paused: bool,
    pub current_status: String,
    pub last_ping_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// The user-editable fields of a heartbeat.
#[derive(Debug, Clone, PartialEq)]
pub struct HeartbeatConfig {
    pub name: String,
    pub period_seconds: i32,
    pub grace_seconds: i32,
    pub paused: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateHeartbeatRequest {
    pub name: String,
    pub period_seconds: i32,
    #[serde(default = "default_grace_seconds")]
    pub grace_seconds: i32,
    #[serde(default)]
    pub paused: bool,
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
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct PatchHeartbeatRequest {
    pub name: Option<String>,
    pub period_seconds: Option<i32>,
    pub grace_seconds: Option<i32>,
    pub paused: Option<bool>,
}

impl PatchHeartbeatRequest {
    pub fn apply_to(self, base: HeartbeatConfig) -> HeartbeatConfig {
        HeartbeatConfig {
            name: self.name.unwrap_or(base.name),
            period_seconds: self.period_seconds.unwrap_or(base.period_seconds),
            grace_seconds: self.grace_seconds.unwrap_or(base.grace_seconds),
            paused: self.paused.unwrap_or(base.paused),
        }
    }
}

pub fn validate(mut c: HeartbeatConfig) -> Result<HeartbeatConfig, String> {
    c.name = validate_name(&c.name)?;
    if c.period_seconds < 30 {
        return Err("period_seconds must be at least 30".into());
    }
    if c.grace_seconds < 0 {
        return Err("grace_seconds must not be negative".into());
    }
    Ok(c)
}

pub async fn insert_heartbeat(
    pool: &PgPool,
    project_id: Uuid,
    c: &HeartbeatConfig,
) -> Result<Heartbeat, sqlx::Error> {
    // 32 random bytes, hex-encoded (the same generator as auth tokens).
    let (token, _hash) = crate::auth::token::new_opaque_token();
    sqlx::query_as(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds, paused)
         values ($1, $2, $3, $4, $5, $6)
         returning *",
    )
    .bind(project_id)
    .bind(&c.name)
    .bind(&token)
    .bind(c.period_seconds)
    .bind(c.grace_seconds)
    .bind(c.paused)
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
    let base = HeartbeatConfig {
        name: current.name,
        period_seconds: current.period_seconds,
        grace_seconds: current.grace_seconds,
        paused: current.paused,
    };
    let config = validate(patch.apply_to(base)).map_err(AppError::Validation)?;
    let updated: Heartbeat = sqlx::query_as(
        "update heartbeats set name = $3, period_seconds = $4, grace_seconds = $5, paused = $6
         where id = $1 and project_id = $2
         returning *",
    )
    .bind(heartbeat_id)
    .bind(project_id)
    .bind(&config.name)
    .bind(config.period_seconds)
    .bind(config.grace_seconds)
    .bind(config.paused)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(updated))
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
