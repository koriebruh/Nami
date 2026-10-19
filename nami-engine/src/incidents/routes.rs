use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{CurrentUser, require_project_role},
    core::state::AppState,
    incidents::model::{self as incident, IncidentListItem},
};

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/incidents", get(list_incidents))
}

#[derive(Deserialize)]
struct IncidentsQuery {
    status: Option<String>,
    monitor_id: Option<Uuid>,
}

async fn list_incidents(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Query(query): Query<IncidentsQuery>,
) -> Result<Json<Vec<IncidentListItem>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    if let Some(status) = query.status.as_deref()
        && !matches!(status, "open" | "resolved")
    {
        return Err(AppError::Validation(
            "status must be open or resolved".into(),
        ));
    }
    let incidents = incident::list_for_project(
        &state.db,
        project_id,
        query.status.as_deref(),
        query.monitor_id,
    )
    .await?;
    Ok(Json(incidents))
}
