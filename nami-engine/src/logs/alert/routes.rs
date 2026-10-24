//! Log alerts: viewers read and preview, members create and edit, admins
//! delete (as monitors).

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{CurrentUser, Role, require_project_role},
    core::state::AppState,
    logs::alert::{
        count_matches,
        model::{
            self as log_alert, Comparison, CreatedAlert, LogAlert, LogAlertConfig, LogAlertPatch,
            MAX_LOG_ALERTS_PER_PROJECT, UpdatedAlert, WINDOW_RANGE,
        },
    },
    logs::query::{Window, search::QueryPermit},
    logs::view::saved_query::{self, LogQuery},
};

fn yes() -> bool {
    true
}

// Numbers and the comparison are taken as plain values so an out-of-range
// one is a 400 (`validate`, `Comparison::parse`) instead of failing to
// deserialize (422).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateLogAlertRequest {
    pub name: String,
    #[serde(default)]
    pub query: LogQuery,
    pub threshold: i64,
    pub comparison: String,
    pub window_seconds: i32,
    pub interval_seconds: i32,
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// Every field optional: merged with the stored alert, then validated whole.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchLogAlertRequest {
    pub name: Option<String>,
    pub query: Option<LogQuery>,
    pub threshold: Option<i64>,
    pub comparison: Option<String>,
    pub window_seconds: Option<i32>,
    pub interval_seconds: Option<i32>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    #[serde(default)]
    pub query: LogQuery,
    pub window_seconds: i32,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/log-alerts",
            get(list_alerts).post(create_alert),
        )
        .route("/projects/{id}/log-alerts/preview", post(preview))
        .route(
            "/projects/{id}/log-alerts/{alert_id}",
            get(get_alert).patch(patch_alert).delete(delete_alert),
        )
}

/// The validated config, its sources checked against the project.
async fn checked(
    state: &AppState,
    project_id: Uuid,
    config: LogAlertConfig,
) -> Result<LogAlertConfig, AppError> {
    let config = log_alert::validate(config).map_err(AppError::Validation)?;
    saved_query::check_sources(&state.db, project_id, &config.query.source_ids).await?;
    Ok(config)
}

async fn list_alerts(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<LogAlert>>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    Ok(Json(
        log_alert::list_for_project(&state.db, project_id).await?,
    ))
}

async fn create_alert(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateLogAlertRequest>,
) -> Result<(StatusCode, Json<LogAlert>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let config = checked(
        &state,
        project_id,
        LogAlertConfig {
            name: payload.name,
            query: payload.query,
            threshold: payload.threshold,
            comparison: Comparison::parse(&payload.comparison).map_err(AppError::Validation)?,
            window_seconds: payload.window_seconds,
            interval_seconds: payload.interval_seconds,
            enabled: payload.enabled,
        },
    )
    .await?;
    match log_alert::create(&state.db, project_id, &config, user.id).await? {
        CreatedAlert::Created(alert) => Ok((StatusCode::CREATED, Json(alert))),
        CreatedAlert::ProjectFull => Err(AppError::Conflict(format!(
            "a project may have at most {MAX_LOG_ALERTS_PER_PROJECT} log alerts"
        ))),
        // Global admins bypass membership: the project may not exist.
        CreatedAlert::NoProject => Err(AppError::NotFound),
    }
}

async fn get_alert(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, alert_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<LogAlert>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    log_alert::get(&state.db, project_id, alert_id)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
}

/// Merged with the stored alert under its row lock ([`log_alert::update`]).
/// Only the sources the PATCH itself names are checked against the project:
/// a stored source deleted since does not block other edits.
async fn patch_alert(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, alert_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchLogAlertRequest>,
) -> Result<Json<LogAlert>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let comparison = payload
        .comparison
        .as_deref()
        .map(Comparison::parse)
        .transpose()
        .map_err(AppError::Validation)?;
    if let Some(query) = &payload.query {
        // Shape first: it caps `source_ids` before they reach Postgres.
        query.to_filters().map_err(AppError::Validation)?;
        saved_query::check_sources(&state.db, project_id, &query.source_ids).await?;
    }
    let patch = LogAlertPatch {
        name: payload.name,
        query: payload.query,
        threshold: payload.threshold,
        comparison,
        window_seconds: payload.window_seconds,
        interval_seconds: payload.interval_seconds,
        enabled: payload.enabled,
    };
    match log_alert::update(&state.db, project_id, alert_id, patch).await? {
        UpdatedAlert::Updated(alert) => Ok(Json(alert)),
        UpdatedAlert::NotFound => Err(AppError::NotFound),
        UpdatedAlert::Invalid(message) => Err(AppError::Validation(message)),
    }
}

async fn delete_alert(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, alert_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    if log_alert::delete(&state.db, project_id, alert_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

/// `POST /projects/{id}/log-alerts/preview`: what the alert's count would be
/// right now, `{count, from, to}`. Holds one of the user's query slots.
async fn preview(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<PreviewRequest>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    if !WINDOW_RANGE.contains(&payload.window_seconds) {
        return Err(AppError::Validation(format!(
            "window_seconds must be between {} and {}",
            WINDOW_RANGE.start(),
            WINDOW_RANGE.end()
        )));
    }
    let filters = payload.query.to_filters().map_err(AppError::Validation)?;
    saved_query::check_sources(&state.db, project_id, &payload.query.source_ids).await?;
    let _slot = QueryPermit::acquire(&state.logs, user.id)?;
    let to = Utc::now();
    let window = Window {
        from: to - chrono::Duration::seconds(payload.window_seconds.into()),
        to,
    };
    let count = count_matches(&state.clickhouse, project_id, &filters, &window).await?;
    Ok(Json(
        json!({ "count": count, "from": window.from, "to": window.to }),
    ))
}
