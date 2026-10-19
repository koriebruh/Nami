use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{CurrentUser, require_project_role},
    core::state::AppState,
    uptime::group::model::{
        self as monitor_group, CreateGroupRequest, MonitorGroup, PatchGroupRequest,
    },
};

// Same RBAC as monitors: viewer reads, member creates/edits, admin deletes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/monitor-groups",
            get(list_groups).post(create_group),
        )
        .route(
            "/projects/{id}/monitor-groups/{group_id}",
            get(get_group).patch(patch_group).delete(delete_group),
        )
}

/// Ordered by position, then name.
async fn list_groups(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<MonitorGroup>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    Ok(Json(
        monitor_group::list_for_project(&state.db, project_id).await?,
    ))
}

async fn create_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateGroupRequest>,
) -> Result<(StatusCode, Json<MonitorGroup>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let (name, position) =
        monitor_group::validate(&payload.name, payload.position).map_err(AppError::Validation)?;
    // Global admins skip membership: a project that does not exist is a 404.
    let created = monitor_group::insert_group(&state.db, project_id, &name, position).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, group_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<MonitorGroup>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let group = monitor_group::find_in_project(&state.db, project_id, group_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(group))
}

async fn patch_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, group_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchGroupRequest>,
) -> Result<Json<MonitorGroup>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let group = monitor_group::update_in_project(&state.db, project_id, group_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(group))
}

async fn delete_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, group_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !monitor_group::delete_in_project(&state.db, project_id, group_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}
