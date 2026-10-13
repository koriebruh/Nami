use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{CurrentUser, require_project_role},
    models::monitor::{self, CreateMonitorRequest, Monitor, PatchMonitorRequest},
    state::AppState,
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
}

async fn list_monitors(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<Monitor>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    Ok(Json(
        monitor::list_for_project(&state.db, project_id).await?,
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
