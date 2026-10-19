use axum::{
    Json, Router,
    extract::{Path, RawQuery, State},
    routing::get,
};
use chrono::Utc;
use uuid::Uuid;

use crate::{
    error::AppError,
    logs::{
        query,
        search::{self, Histogram, SearchPage},
    },
    middleware::rbac::{CurrentUser, require_project_role},
    state::AppState,
};

// Viewers and up. The project is always the one in the path: a source_id
// of another project simply matches nothing.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/logs", get(search_logs))
        .route("/projects/{id}/logs/histogram", get(log_histogram))
}

/// `GET /projects/{id}/logs?from&to&source_id*&level*&q&in_fields&filter*
/// &trace_id&span_id&pattern_hash*&limit&cursor` (`*` = repeatable).
async fn search_logs(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SearchPage>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let params = query::parse_search(&query::query_pairs(raw.as_deref()), Utc::now())
        .map_err(AppError::Validation)?;
    Ok(Json(
        search::search(&state.clickhouse, project_id, &params).await?,
    ))
}

/// The same filters and window as search (no cursor/limit).
async fn log_histogram(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Histogram>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let pairs = query::query_pairs(raw.as_deref());
    let window = query::parse_window(&pairs, Utc::now()).map_err(AppError::Validation)?;
    let filters = query::parse_filters(&pairs).map_err(AppError::Validation)?;
    Ok(Json(
        search::histogram(&state.clickhouse, project_id, &window, &filters).await?,
    ))
}
