use axum::{
    Json, Router,
    extract::{Path, RawQuery, State},
    http::{HeaderValue, header::HeaderName},
    response::{
        IntoResponse, Response,
        sse::{KeepAlive, Sse},
    },
    routing::get,
};
use chrono::Utc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower_sessions::Session;
use uuid::Uuid;

use crate::{
    error::AppError,
    logs::{
        query,
        search::{self, Histogram, SearchPage},
        tail,
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
        .route("/projects/{id}/logs/tail", get(tail_logs))
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

/// Server-Sent Events of new rows matching the search filters (no window,
/// cursor or limit); see `logs::tail`. At most `MAX_TAILS_PER_USER` streams
/// per user and `MAX_TAILS_GLOBAL` in all, else 429.
async fn tail_logs(
    State(state): State<AppState>,
    user: CurrentUser,
    session: Session,
    Path(project_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let filters =
        query::parse_filters(&query::query_pairs(raw.as_deref())).map_err(AppError::Validation)?;
    let permit = tail::TailPermit::acquire(&state.logs, user.id)?;
    let keep_alive = state.logs.tail.keep_alive;
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        // The slot is held exactly as long as the stream is.
        let _permit = permit;
        tail::run(state, session, project_id, filters, tx).await;
    });
    let mut response = Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::new().interval(keep_alive))
        .into_response();
    // Tell reverse proxies (nginx) not to buffer the stream.
    response.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    Ok(response)
}
