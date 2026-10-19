use std::sync::{Arc, atomic::AtomicBool};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, RawQuery, State},
    http::{HeaderValue, header, header::HeaderName},
    response::{
        IntoResponse, Response,
        sse::{KeepAlive, Sse},
    },
    routing::get,
};
use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower_sessions::Session;
use uuid::Uuid;

use crate::{
    download::{AbortableBody, instance_busy, user_busy},
    error::AppError,
    logs::{
        export::{self, ExportFormat},
        patterns, query,
        search::{self, Histogram, LogEntry, QueryPermit, SearchPage},
        slots::SlotRefused,
        tail,
    },
    middleware::rbac::{CurrentUser, require_project_role},
    models::source,
    state::AppState,
};

// Viewers and up. The project is always the one in the path: a source_id
// of another project (or of none) simply matches nothing, in search,
// histogram, patterns, tail and export alike.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/logs", get(search_logs))
        .route("/projects/{id}/logs/histogram", get(log_histogram))
        .route("/projects/{id}/logs/patterns", get(log_patterns))
        .route("/projects/{id}/logs/tail", get(tail_logs))
        .route("/projects/{id}/logs/export", get(export_logs))
        .route("/projects/{id}/logs/{log_id}", get(log_entry))
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
    // Held until the response is built, or dropped with the handler when the
    // client goes away (ClickHouse cancels the query then).
    let _slot = QueryPermit::acquire(&state.logs, user.id)?;
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
    let _slot = QueryPermit::acquire(&state.logs, user.id)?;
    Ok(Json(
        search::histogram(&state.clickhouse, project_id, &window, &filters).await?,
    ))
}

/// `GET /projects/{id}/logs/patterns?from&to&<search filters>`: the top
/// patterns of the window (viewer). Holds one of the user's query slots.
async fn log_patterns(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Json<serde_json::Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let pairs = query::query_pairs(raw.as_deref());
    let window = query::parse_window(&pairs, Utc::now()).map_err(AppError::Validation)?;
    let filters = query::parse_filters(&pairs).map_err(AppError::Validation)?;
    let _slot = QueryPermit::acquire(&state.logs, user.id)?;
    let rows = patterns::top_patterns(&state.clickhouse, project_id, &window, &filters).await?;
    Ok(Json(serde_json::json!({ "patterns": rows })))
}

/// `GET /projects/{id}/logs/{log_id}?dt=<RFC 3339>`: one whole row (list
/// rows cut long messages and leave out large fields, see
/// `search::ENTRY_COLUMNS`). `dt` is the row's own, as listed. 404 when the
/// project has no such row (a row of another project included).
async fn log_entry(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, log_id)): Path<(Uuid, Uuid)>,
    RawQuery(raw): RawQuery,
) -> Result<Json<LogEntry>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let pairs = query::query_pairs(raw.as_deref());
    let dt = pairs
        .iter()
        .find(|(key, _)| key == "dt")
        .and_then(|(_, value)| DateTime::parse_from_rfc3339(value).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .ok_or_else(|| AppError::Validation("dt must be an RFC 3339 timestamp".to_string()))?;
    let _slot = state
        .logs
        .entry_slots
        .acquire(user.id, search::MAX_ENTRY_READS_PER_USER)?;
    search::entry(&state.clickhouse, project_id, log_id, dt)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
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

/// `GET /projects/{id}/logs/export?format=csv|ndjson&from&to&<search
/// filters>&columns*`: the matching rows as a streamed download, newest
/// first, at most `ExportLimits::rows` (see `logs::export`). Viewer.
///
/// Everything that can fail before the file starts is answered as JSON
/// (400 parameters, 429 the user's 2 exports already running, 503 every
/// export slot of the instance taken, both with `Retry-After: 5`, 400/500
/// ClickHouse refusing the query); after that, only the file itself can say
/// it stopped early.
async fn export_logs(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Response, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let params = export::parse_export(&query::query_pairs(raw.as_deref()), Utc::now())
        .map_err(AppError::Validation)?;
    // Names for the CSV `source` column. A `source_id` filter naming a
    // source that is not one of the project's simply matches nothing, as in
    // search: the project in the WHERE is always the path's.
    let sources = source::names_for_project(&state.db, project_id).await?;
    // Moved into the streaming task below: held exactly as long as the file
    // streams (at most the deadline), and given back on every early return.
    let permit = match state.logs.export_slots.acquire_capped(
        user.id,
        export::MAX_EXPORTS_PER_USER,
        export::MAX_EXPORTS_PER_INSTANCE,
    ) {
        Ok(permit) => permit,
        Err(SlotRefused::User) => return Ok(user_busy(export::MAX_EXPORTS_PER_USER)),
        Err(SlotRefused::Total) => return Ok(instance_busy()),
    };
    let limits = state.logs.export_limits;
    let sql = export::export_sql(project_id, &params, limits.rows);
    let rows = state
        .clickhouse
        .stream_rows_with(sql.text(), &sql.params(), &[], limits.line_bytes)
        .await
        .map_err(search::user_error)?;

    let (content_type, name) = match params.format {
        ExportFormat::Csv => (
            "text/csv; charset=utf-8",
            export::file_name(params.format, params.window.to),
        ),
        ExportFormat::Ndjson => (
            "application/x-ndjson",
            export::file_name(params.format, params.window.to),
        ),
    };
    let (tx, rx) = mpsc::channel(export::EXPORT_CHANNEL_CHUNKS);
    let aborted = Arc::new(AtomicBool::new(false));
    let task_aborted = aborted.clone();
    tokio::spawn(async move {
        let _permit = permit;
        export::stream(rows, params, sources, limits, tx, task_aborted).await;
    });
    let mut response = Response::new(Body::from_stream(AbortableBody::new(rx, aborted)));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .expect("the file name is ASCII"),
    );
    headers.insert(
        HeaderName::from_static(export::EXPORT_LIMIT_HEADER),
        HeaderValue::from(limits.rows),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    Ok(response)
}
