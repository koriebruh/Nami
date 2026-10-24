use axum::{
    Json, Router,
    extract::{Path, RawQuery, State},
    http::StatusCode,
    routing::{get, post, put},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{CurrentUser, Role, require_project_role},
    core::state::AppState,
    core::validate::{validate_line, validate_text},
    incidents::lifecycle::{self, IncidentSource, OpenIncident, Reopen},
    incidents::model::{
        self as incident, DEFAULT_LIST_LIMIT, IncidentDetail, IncidentEvent, IncidentListItem,
        ListFilter, MAX_LIST_LIMIT, MAX_POSTMORTEM_BYTES, MAX_TEXT_BYTES, MAX_TITLE_CHARS, SOURCES,
        STATUSES,
    },
    incidents::severity::model as severity,
    logs::query::{last, query_pairs, values},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/incidents",
            get(list_incidents).post(create_incident),
        )
        .route(
            "/projects/{id}/incidents/{incident_id}",
            get(get_incident).patch(patch_incident),
        )
        .route(
            "/projects/{id}/incidents/{incident_id}/acknowledge",
            post(acknowledge),
        )
        .route("/projects/{id}/incidents/{incident_id}/resolve", post(resolve))
        .route("/projects/{id}/incidents/{incident_id}/reopen", post(reopen))
        .route(
            "/projects/{id}/incidents/{incident_id}/comments",
            post(add_comment),
        )
        .route(
            "/projects/{id}/incidents/{incident_id}/postmortem",
            put(set_postmortem),
        )
}

#[derive(Serialize)]
struct IncidentPage {
    incidents: Vec<IncidentListItem>,
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateIncidentRequest {
    title: String,
    description: Option<String>,
    severity_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchIncidentRequest {
    severity_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentRequest {
    body: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PostmortemRequest {
    postmortem: Option<String>,
}

/// `status` (repeatable; `unresolved` = open + acknowledged), `source`
/// (repeatable), `monitor_id`, `heartbeat_id`, `log_alert_id`, `limit`
/// (1-100) and `cursor`. Other keys are ignored.
fn parse_list_query(raw: Option<&str>) -> Result<ListFilter, String> {
    let pairs = query_pairs(raw);
    let mut filter = ListFilter {
        limit: DEFAULT_LIST_LIMIT,
        ..ListFilter::default()
    };
    for status in values(&pairs, "status") {
        let expanded: &[&str] = match status {
            "unresolved" => &["open", "acknowledged"],
            s if STATUSES.contains(&s) => &[s],
            _ => return Err("status must be open, acknowledged, resolved or unresolved".into()),
        };
        for s in expanded {
            if !filter.statuses.iter().any(|known| known == s) {
                filter.statuses.push((*s).to_string());
            }
        }
    }
    for source in values(&pairs, "source") {
        if !SOURCES.contains(&source) {
            return Err(format!("source must be one of {}", SOURCES.join(", ")));
        }
        if !filter.sources.iter().any(|known| known == source) {
            filter.sources.push(source.to_string());
        }
    }
    let id = |key: &str| -> Result<Option<Uuid>, String> {
        last(&pairs, key)
            .filter(|v| !v.is_empty())
            .map(|v| Uuid::parse_str(v).map_err(|_| format!("{key} must be a UUID")))
            .transpose()
    };
    filter.monitor_id = id("monitor_id")?;
    filter.heartbeat_id = id("heartbeat_id")?;
    filter.log_alert_id = id("log_alert_id")?;
    if let Some(limit) = last(&pairs, "limit").filter(|v| !v.is_empty()) {
        filter.limit = limit
            .parse()
            .ok()
            .filter(|n| (1..=MAX_LIST_LIMIT).contains(n))
            .ok_or_else(|| format!("limit must be between 1 and {MAX_LIST_LIMIT}"))?;
    }
    if let Some(cursor) = last(&pairs, "cursor").filter(|v| !v.is_empty()) {
        filter.before = Some(incident::decode_cursor(cursor).ok_or("cursor is invalid")?);
    }
    Ok(filter)
}

async fn list_incidents(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Json<IncidentPage>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    let mut filter = parse_list_query(raw.as_deref()).map_err(AppError::Validation)?;
    let limit = filter.limit;
    // One row more than the page tells whether another page follows.
    filter.limit += 1;
    let mut incidents = incident::list(&state.db, project_id, &filter).await?;
    let next_cursor = if incidents.len() as i64 > limit {
        incidents.truncate(limit as usize);
        incidents.last().map(incident::encode_cursor)
    } else {
        None
    };
    Ok(Json(IncidentPage {
        incidents,
        next_cursor,
    }))
}

async fn create_incident(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(req): Json<CreateIncidentRequest>,
) -> Result<(StatusCode, Json<IncidentDetail>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let title = validate_line("title", &req.title, MAX_TITLE_CHARS).map_err(AppError::Validation)?;
    let description = req.description.filter(|d| !d.trim().is_empty());
    if let Some(description) = &description {
        validate_text("description", description, MAX_TEXT_BYTES).map_err(AppError::Validation)?;
    }
    let mut tx = state.db.begin().await?;
    if let Some(severity_id) = req.severity_id
        && !severity::belongs_to(&mut *tx, project_id, severity_id).await?
    {
        return Err(AppError::NotFound);
    }
    let id = lifecycle::open(
        &mut tx,
        &OpenIncident {
            project_id,
            source: IncidentSource::Manual {
                created_by: user.id,
            },
            title: &title,
            cause: description.as_deref(),
            severity_id: req.severity_id,
            metadata: None,
            started_at: Utc::now(),
        },
    )
    .await?
    .expect("a manual incident has no conflict target, so the insert always returns its id");
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(load_detail(&state, project_id, id).await?),
    ))
}

async fn get_incident(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, incident_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<IncidentDetail>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    Ok(Json(load_detail(&state, project_id, incident_id).await?))
}

async fn acknowledge(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, incident_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<IncidentDetail>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let mut tx = begin_locked(&state, project_id, incident_id).await?;
    if !lifecycle::acknowledge(&mut tx, incident_id, user.id, Utc::now()).await? {
        return Err(AppError::Conflict("incident is not open".into()));
    }
    tx.commit().await?;
    Ok(Json(load_detail(&state, project_id, incident_id).await?))
}

async fn resolve(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, incident_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<IncidentDetail>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let mut tx = begin_locked(&state, project_id, incident_id).await?;
    if !lifecycle::resolve(&mut tx, incident_id, Some(user.id), Utc::now()).await? {
        return Err(AppError::Conflict("incident is already resolved".into()));
    }
    tx.commit().await?;
    Ok(Json(load_detail(&state, project_id, incident_id).await?))
}

async fn reopen(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, incident_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<IncidentDetail>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let mut tx = begin_locked(&state, project_id, incident_id).await?;
    match lifecycle::reopen(&mut tx, incident_id, user.id, Utc::now()).await? {
        Reopen::Reopened => {}
        Reopen::NotResolved => return Err(AppError::Conflict("incident is not resolved".into())),
        Reopen::SourceHasUnresolvedIncident => {
            return Err(AppError::Conflict(
                "this source already has an unresolved incident".into(),
            ));
        }
        Reopen::NotFound => return Err(AppError::NotFound),
    }
    tx.commit().await?;
    Ok(Json(load_detail(&state, project_id, incident_id).await?))
}

async fn add_comment(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, incident_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<CommentRequest>,
) -> Result<(StatusCode, Json<IncidentEvent>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    if req.body.trim().is_empty() {
        return Err(AppError::Validation("comment is required".into()));
    }
    validate_text("comment", &req.body, MAX_TEXT_BYTES).map_err(AppError::Validation)?;
    let mut tx = begin_locked(&state, project_id, incident_id).await?;
    let event_id = lifecycle::comment(&mut tx, incident_id, user.id, &req.body).await?;
    tx.commit().await?;
    let event = incident::event(&state.db, event_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, Json(event)))
}

async fn set_postmortem(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, incident_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<PostmortemRequest>,
) -> Result<Json<IncidentDetail>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let postmortem = req.postmortem.filter(|p| !p.trim().is_empty());
    if let Some(postmortem) = &postmortem {
        validate_text("postmortem", postmortem, MAX_POSTMORTEM_BYTES)
            .map_err(AppError::Validation)?;
    }
    let mut tx = begin_locked(&state, project_id, incident_id).await?;
    incident::set_postmortem(&mut tx, incident_id, user.id, postmortem.as_deref()).await?;
    tx.commit().await?;
    Ok(Json(load_detail(&state, project_id, incident_id).await?))
}

async fn patch_incident(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, incident_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<PatchIncidentRequest>,
) -> Result<Json<IncidentDetail>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let mut tx = begin_locked(&state, project_id, incident_id).await?;
    if !incident::set_severity(&mut tx, project_id, incident_id, user.id, req.severity_id).await? {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;
    Ok(Json(load_detail(&state, project_id, incident_id).await?))
}

/// A transaction holding the project's incident row lock; 404 when the
/// project has no such incident.
async fn begin_locked(
    state: &AppState,
    project_id: Uuid,
    incident_id: Uuid,
) -> Result<Transaction<'static, Postgres>, AppError> {
    let mut tx = state.db.begin().await?;
    let conn: &mut PgConnection = &mut tx;
    if !incident::lock(conn, project_id, incident_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(tx)
}

async fn load_detail(
    state: &AppState,
    project_id: Uuid,
    incident_id: Uuid,
) -> Result<IncidentDetail, AppError> {
    incident::detail(&state.db, project_id, incident_id)
        .await?
        .ok_or(AppError::NotFound)
}
