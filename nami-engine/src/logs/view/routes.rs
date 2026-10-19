//! Saved log views: viewers read, members create, edit and delete.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{CurrentUser, require_project_role},
    core::state::AppState,
    core::validate::validate_label,
    logs::view::model::{self as log_view, CreatedView, LogView, MAX_VIEWS_PER_PROJECT},
    logs::view::saved_query::{self, ViewQuery},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateViewRequest {
    pub name: String,
    pub query: ViewQuery,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchViewRequest {
    pub name: Option<String>,
    pub query: Option<ViewQuery>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/log-views",
            get(list_views).post(create_view),
        )
        .route(
            "/projects/{id}/log-views/{view_id}",
            get(get_view).patch(patch_view).delete(delete_view),
        )
}

async fn checked_query(
    state: &AppState,
    project_id: Uuid,
    query: &ViewQuery,
) -> Result<(), AppError> {
    query.validate().map_err(AppError::Validation)?;
    saved_query::check_sources(&state.db, project_id, &query.source_ids).await
}

async fn list_views(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<LogView>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    Ok(Json(
        log_view::list_for_project(&state.db, project_id).await?,
    ))
}

async fn create_view(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateViewRequest>,
) -> Result<(StatusCode, Json<LogView>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let name = validate_label("name", &payload.name).map_err(AppError::Validation)?;
    checked_query(&state, project_id, &payload.query).await?;
    match log_view::create(&state.db, project_id, &name, &payload.query, user.id).await? {
        CreatedView::Created(view) => Ok((StatusCode::CREATED, Json(view))),
        CreatedView::ProjectFull => Err(AppError::Conflict(format!(
            "a project may have at most {MAX_VIEWS_PER_PROJECT} saved views"
        ))),
        CreatedView::NameTaken => Err(AppError::Conflict(
            "a view with this name already exists".to_string(),
        )),
        // Global admins bypass membership: the project may not exist.
        CreatedView::NoProject => Err(AppError::NotFound),
    }
}

async fn get_view(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, view_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<LogView>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    log_view::get(&state.db, project_id, view_id)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
}

async fn patch_view(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, view_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchViewRequest>,
) -> Result<Json<LogView>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let name = payload
        .name
        .as_deref()
        .map(|n| validate_label("name", n))
        .transpose()
        .map_err(AppError::Validation)?;
    if let Some(query) = &payload.query {
        checked_query(&state, project_id, query).await?;
    }
    log_view::update(
        &state.db,
        project_id,
        view_id,
        name.as_deref(),
        payload.query.as_ref(),
    )
    .await?
    .map(Json)
    .ok_or(AppError::NotFound)
}

async fn delete_view(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, view_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    if log_view::delete(&state.db, project_id, view_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}
