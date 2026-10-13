use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{CurrentUser, require_project_role},
    models::monitor::{self, CreateMonitorRequest, Monitor, PatchMonitorRequest},
    state::AppState,
    uptime::store::{self, MonitorStats, UptimeCheckRow},
};

// RBAC mirrors routes/projects.rs: viewer reads, member creates/edits,
// admin deletes. Authorization runs before any lookup, so a caller without
// access gets 403 whether or not the monitor exists.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/monitors",
            get(list_monitors).post(create_monitor),
        )
        .route(
            "/projects/{id}/monitors/{monitor_id}",
            get(get_monitor).patch(patch_monitor).delete(delete_monitor),
        )
        .route(
            "/projects/{id}/monitors/{monitor_id}/checks",
            get(list_checks),
        )
        .route("/projects/{id}/monitors/{monitor_id}/stats", get(get_stats))
}

/// A monitor plus its 24-hour uptime (`null` when it has no checks in the
/// window, or when ClickHouse is unavailable).
#[derive(Serialize)]
struct MonitorSummary {
    #[serde(flatten)]
    monitor: Monitor,
    uptime_24h: Option<f64>,
}

async fn list_monitors(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<MonitorSummary>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let monitors = monitor::list_for_project(&state.db, project_id).await?;
    // Uptime decorates this list; current status comes from Postgres. If
    // ClickHouse is down, still list every monitor, with uptime unknown.
    let uptime = match store::uptime_24h_by_monitor(&state.clickhouse, project_id, Utc::now()).await
    {
        Ok(uptime) => uptime,
        Err(e) => {
            tracing::warn!(error = %e, %project_id, "24h uptime unavailable; listing monitors without it");
            HashMap::new()
        }
    };
    Ok(Json(
        monitors
            .into_iter()
            .map(|monitor| {
                let uptime_24h = uptime.get(&monitor.id).copied();
                MonitorSummary {
                    monitor,
                    uptime_24h,
                }
            })
            .collect(),
    ))
}

async fn create_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateMonitorRequest>,
) -> Result<(StatusCode, Json<Monitor>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let config = monitor::validate(payload.into_config()).map_err(AppError::Validation)?;
    let created = monitor::insert_monitor(&state.db, project_id, &config)
        .await
        .map_err(|e| match &e {
            // A global admin bypasses membership, so the project itself may
            // not exist: SQLSTATE 23503 (foreign_key_violation) -> 404.
            sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => {
                AppError::NotFound
            }
            _ => AppError::Database(e),
        })?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Monitor>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let found = monitor::find_in_project(&state.db, project_id, monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(found))
}

async fn patch_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchMonitorRequest>,
) -> Result<Json<Monitor>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let updated = monitor::update_in_project(&state.db, project_id, monitor_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated))
}

async fn delete_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !monitor::delete_in_project(&state.db, project_id, monitor_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[derive(Deserialize)]
struct ChecksQuery {
    limit: Option<u32>,
}

async fn list_checks(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<ChecksQuery>,
) -> Result<Json<Vec<UptimeCheckRow>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    monitor::find_in_project(&state.db, project_id, monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    Ok(Json(
        store::recent_checks(&state.clickhouse, project_id, monitor_id, limit).await?,
    ))
}

async fn get_stats(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<MonitorStats>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    monitor::find_in_project(&state.db, project_id, monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(
        store::monitor_stats(&state.clickhouse, project_id, monitor_id, Utc::now()).await?,
    ))
}
