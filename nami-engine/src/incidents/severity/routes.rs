use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{CurrentUser, Role, require_project_role},
    core::state::AppState,
    incidents::severity::model::{self as severity, Severity},
};

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/severities", get(list_severities))
}

async fn list_severities(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<Severity>>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    Ok(Json(severity::list_for_project(&state.db, project_id).await?))
}
