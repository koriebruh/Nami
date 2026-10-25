use std::{collections::HashMap, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{
        CurrentUser, ProjectReader, Role, require_project_reader, require_project_role,
    },
    core::state::AppState,
    incidents::model as incident,
    uptime::group::model as monitor_group,
    uptime::maintenance::model as maintenance,
    uptime::monitor::model::{
        self as monitor, CreateMonitorRequest, Monitor, MonitorView, PatchMonitorRequest,
    },
    uptime::{
        availability::{self, Availability, RangeQuery},
        import::{
            self, ImportReport, ImportRequest, MAX_IMPORT_BODY_BYTES, MAX_IMPORT_REQUEST_BYTES,
        },
        store::{self, MonitorStats, UptimeCheckRow},
    },
};

/// How long the monitor list waits for ClickHouse's uptime answer. Past it the
/// list is served without uptime, exactly as when ClickHouse returns an error,
/// instead of waiting for the HTTP client's own 30 s timeout.
const UPTIME_LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);

/// `GET .../checks?limit=`: the default when `limit` is absent. A `limit`
/// outside `MIN_CHECKS_LIMIT..=MAX_CHECKS_LIMIT` is clamped into it, not
/// rejected.
const DEFAULT_CHECKS_LIMIT: u32 = 50;
const MIN_CHECKS_LIMIT: u32 = 1;
const MAX_CHECKS_LIMIT: u32 = 500;

// RBAC mirrors projects/routes.rs: viewer reads, member creates/edits,
// admin deletes. Viewers read monitors with the secrets of their URL masked
// and their request body, username and udp payload hidden
// (`Monitor::for_reader`, flagged `redacted`); nobody
// ever reads a header value or the password: every monitor leaves here as a
// `MonitorView`. `require_project_role` runs before any database or
// ClickHouse lookup, so a caller without access gets 403 whether or not the
// monitor exists. Extractors run before the handler body, so a malformed path
// or query string (`?limit=abc`) is a 400 even for such a caller; that
// reveals nothing about the project. The same goes for the import body
// (`Json<ImportRequest>`, up to `MAX_IMPORT_REQUEST_BYTES`): a malformed one
// is a 400 (an unknown `format` a 422) before the role check.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/monitors",
            get(list_monitors).post(create_monitor),
        )
        .route(
            "/projects/{id}/monitors/import",
            post(import_monitors).layer(DefaultBodyLimit::max(MAX_IMPORT_REQUEST_BYTES)),
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
        .route(
            "/projects/{id}/monitors/{monitor_id}/availability",
            get(get_availability),
        )
        .route(
            "/projects/{id}/monitors/{monitor_id}/checks/export",
            crate::uptime::export::route(),
        )
}

/// `monitor` as a [`MonitorView`] for `reader`, with `in_maintenance` looked
/// up now (one query; a list looks the windows up once instead, see
/// `list_monitors`).
async fn view_one(
    state: &AppState,
    monitor: Monitor,
    reader: ProjectReader,
) -> Result<MonitorView, AppError> {
    let in_maintenance =
        maintenance::monitor_in_maintenance(&state.db, monitor.project_id, monitor.id, Utc::now())
            .await?;
    Ok(MonitorView::for_reader(monitor, reader, in_maintenance))
}

/// A monitor plus its 24-hour uptime (`null` when it has no checks in the
/// window, or when ClickHouse is unavailable).
#[derive(Serialize)]
struct MonitorSummary {
    #[serde(flatten)]
    monitor: MonitorView,
    uptime_24h: Option<f64>,
}

async fn list_monitors(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<MonitorSummary>>, AppError> {
    let reader = require_project_reader(&state.db, &user, project_id).await?;
    let monitors = monitor::list_for_project(&state.db, project_id).await?;
    // One query for the whole list, whatever its length.
    let windows = maintenance::active_windows(&state.db, project_id, Utc::now()).await?;
    // Uptime decorates this list; current status comes from Postgres. If
    // ClickHouse is down, erroring or too slow, still list every monitor,
    // with uptime unknown.
    let monitor_ids: Vec<Uuid> = monitors.iter().map(|monitor| monitor.id).collect();
    let lookup = tokio::time::timeout(
        UPTIME_LOOKUP_TIMEOUT,
        store::uptime_24h_by_monitor(&state.clickhouse, project_id, &monitor_ids, Utc::now()),
    )
    .await;
    let uptime = match lookup {
        Ok(Ok(uptime)) => uptime,
        Ok(Err(e)) => uptime_unavailable(project_id, &e),
        Err(elapsed) => uptime_unavailable(project_id, &elapsed),
    };
    Ok(Json(
        monitors
            .into_iter()
            .map(|monitor| {
                let uptime_24h = uptime.get(&monitor.id).copied();
                let in_maintenance = windows.covers_monitor(monitor.id);
                MonitorSummary {
                    monitor: MonitorView::for_reader(monitor, reader, in_maintenance),
                    uptime_24h,
                }
            })
            .collect(),
    ))
}

/// Logs why the uptime lookup failed (an error, or the deadline passing) and
/// returns no uptime, so the list is served without it either way.
fn uptime_unavailable(project_id: Uuid, reason: &dyn std::fmt::Display) -> HashMap<Uuid, f64> {
    tracing::warn!(error = %reason, %project_id, "24h uptime unavailable; listing monitors without it");
    HashMap::new()
}

async fn create_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateMonitorRequest>,
) -> Result<(StatusCode, Json<MonitorView>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let config = monitor::validate(payload.into_config()).map_err(AppError::Validation)?;
    if let Some(group_id) = config.group_id {
        // A group of another project is a 404, like any foreign id.
        if !monitor_group::belongs_to(&state.db, project_id, group_id).await? {
            return Err(AppError::NotFound);
        }
    }
    // So is a policy or severity of another project.
    if !incident::routing_belongs_to(
        &state.db,
        project_id,
        config.escalation_policy_id,
        config.severity_id,
    )
    .await?
    {
        return Err(AppError::NotFound);
    }
    let created = monitor::insert_monitor(&state.db, &state.cipher, project_id, &config)
        .await
        .map_err(|e| match &e {
            // A global admin bypasses membership, so the project itself may
            // not exist (or the group was just deleted): SQLSTATE 23503
            // (foreign_key_violation) -> 404.
            sqlx::Error::Database(db_err) if db_err.is_foreign_key_violation() => {
                AppError::NotFound
            }
            _ => AppError::Database(e),
        })?;
    Ok((
        StatusCode::CREATED,
        Json(view_one(&state, created, ProjectReader::Member).await?),
    ))
}

/// `POST .../monitors/import`: a URL list, Uptime Kuma backup or UptimeRobot
/// CSV, every entry created through the single-create path (see
/// `uptime::import::run_import`). 200 with one result row per entry.
async fn import_monitors(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(request): Json<ImportRequest>,
) -> Result<Response, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    if request.content.len() > MAX_IMPORT_BODY_BYTES {
        return Ok((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": format!(
                    "content is larger than {} MiB",
                    MAX_IMPORT_BODY_BYTES / (1024 * 1024)
                )
            })),
        )
            .into_response());
    }
    let report: ImportReport =
        import::run_import(&state.db, &state.cipher, project_id, request).await?;
    Ok(Json(report).into_response())
}

async fn get_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<MonitorView>, AppError> {
    let reader = require_project_reader(&state.db, &user, project_id).await?;
    let found = monitor::find_in_project(&state.db, project_id, monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(view_one(&state, found, reader).await?))
}

async fn patch_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchMonitorRequest>,
) -> Result<Json<MonitorView>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    // A policy or severity of another project is a 404, like any foreign id.
    let (policy, severity) = (
        payload.escalation_policy_id.flatten(),
        payload.severity_id.flatten(),
    );
    if !incident::routing_belongs_to(&state.db, project_id, policy, severity).await? {
        return Err(AppError::NotFound);
    }
    let updated =
        monitor::update_in_project(&state.db, &state.cipher, project_id, monitor_id, payload)
            .await?
            .ok_or(AppError::NotFound)?;
    Ok(Json(
        view_one(&state, updated, ProjectReader::Member).await?,
    ))
}

async fn delete_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
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
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    monitor::find_in_project(&state.db, project_id, monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_CHECKS_LIMIT)
        .clamp(MIN_CHECKS_LIMIT, MAX_CHECKS_LIMIT);
    Ok(Json(
        store::recent_checks(&state.clickhouse, project_id, monitor_id, limit).await?,
    ))
}

async fn get_stats(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<MonitorStats>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    monitor::find_in_project(&state.db, project_id, monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(
        store::monitor_stats(&state.clickhouse, project_id, monitor_id, Utc::now()).await?,
    ))
}

async fn get_availability(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
    Query(range): Query<RangeQuery>,
) -> Result<Json<Availability>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    availability::check_range(range.from, range.to).map_err(AppError::Validation)?;
    availability::for_monitor(
        &state.db,
        project_id,
        monitor_id,
        range.from,
        range.to,
        Utc::now(),
    )
    .await?
    .map(Json)
    .ok_or(AppError::NotFound)
}
