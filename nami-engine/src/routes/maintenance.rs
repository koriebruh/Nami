use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{CurrentUser, require_project_role},
    models::maintenance::{
        self, CreateWindowRequest, PatchWindowRequest, WindowConfig, WindowView,
    },
    state::AppState,
};

// Same RBAC as monitors: viewer reads, member creates/edits, admin deletes.
// `require_project_role` runs before any lookup, so a caller without access
// gets 403 whether or not the window exists. A window or monitor id from
// another project is a 404.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/maintenance-windows",
            get(list_windows).post(create_window),
        )
        .route(
            "/projects/{id}/maintenance-windows/{window_id}",
            get(get_window).patch(patch_window).delete(delete_window),
        )
}

async fn list_windows(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<WindowView>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let now = Utc::now();
    let windows = maintenance::list_for_project(&state.db, project_id).await?;
    Ok(Json(
        windows
            .into_iter()
            .map(|w| WindowView::new(w, now))
            .collect(),
    ))
}

async fn create_window(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateWindowRequest>,
) -> Result<(StatusCode, Json<WindowView>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let window =
        maintenance::validate(WindowConfig::from(payload)).map_err(AppError::Validation)?;
    let created = maintenance::insert_window(&state.db, project_id, &window).await?;
    Ok((
        StatusCode::CREATED,
        Json(WindowView::new(created, Utc::now())),
    ))
}

async fn get_window(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, window_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<WindowView>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let window = maintenance::find_in_project(&state.db, project_id, window_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(WindowView::new(window, Utc::now())))
}

async fn patch_window(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, window_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchWindowRequest>,
) -> Result<Json<WindowView>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let window = maintenance::update_in_project(&state.db, project_id, window_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(WindowView::new(window, Utc::now())))
}

async fn delete_window(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, window_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !maintenance::delete_in_project(&state.db, project_id, window_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}
