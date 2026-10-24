use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{StatusCode, header},
    routing::{get, post},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::Map;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{CurrentUser, Role, require_project_role},
    core::state::AppState,
    logs::ingest::{
        normalize, pipeline,
        pipeline::Outcome,
        store,
        usage::{self, DailyUsage},
    },
    logs::source::model::{
        self as source, CreateSourceRequest, PatchSourceRequest, Source, SourceWithToken,
    },
};

/// Sent with every response that carries a token (create, reveal, rotate):
/// no browser, proxy or BFF cache may keep it.
const NO_STORE: NoStore = [(header::CACHE_CONTROL, "no-store")];
type NoStore = [(header::HeaderName, &'static str); 1];

// Viewers read sources but never their tokens; members create, edit, reveal
// and rotate; admins delete. Viewers read a source's pipeline rules; members
// replace them and preview them.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/sources",
            get(list_sources).post(create_source),
        )
        .route(
            "/projects/{id}/sources/{source_id}",
            get(get_source).patch(patch_source).delete(delete_source),
        )
        .route(
            "/projects/{id}/sources/{source_id}/token",
            get(reveal_token),
        )
        .route(
            "/projects/{id}/sources/{source_id}/rotate",
            post(rotate_token),
        )
        .route(
            "/projects/{id}/sources/{source_id}/usage",
            get(source_usage),
        )
        .route(
            "/projects/{id}/sources/{source_id}/pipeline",
            get(get_pipeline).put(put_pipeline),
        )
        .route(
            "/projects/{id}/sources/{source_id}/pipeline/test",
            post(test_pipeline),
        )
}

async fn list_sources(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<Source>>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    Ok(Json(source::list_for_project(&state.db, project_id).await?))
}

async fn create_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateSourceRequest>,
) -> Result<(StatusCode, NoStore, Json<SourceWithToken>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let config = source::validate(payload.into_config()).map_err(AppError::Validation)?;
    let max = state.config.max_sources_per_project;
    let created =
        match source::create_source(&state.db, &state.cipher, project_id, &config, max).await? {
            source::CreatedSource::Created(created) => created,
            source::CreatedSource::ProjectFull => {
                return Err(AppError::Conflict(format!(
                    "a project may have at most {max} sources (MAX_SOURCES_PER_PROJECT)"
                )));
            }
            // Global admins bypass membership: the project may not exist.
            source::CreatedSource::NoProject => return Err(AppError::NotFound),
        };
    Ok((StatusCode::CREATED, NO_STORE, Json(created)))
}

async fn get_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Source>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    let found = source::find_in_project(&state.db, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(found))
}

async fn patch_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchSourceRequest>,
) -> Result<Json<Source>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let updated = source::update_in_project(&state.db, project_id, source_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated))
}

async fn delete_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    if !source::delete_in_project(&state.db, project_id, source_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}

async fn reveal_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<(NoStore, Json<Value>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let token = source::reveal_token(&state.db, &state.cipher, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((NO_STORE, Json(json!({ "token": token }))))
}

async fn rotate_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<(NoStore, Json<SourceWithToken>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let rotated = source::rotate_token(&state.db, &state.cipher, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((NO_STORE, Json(rotated)))
}

#[derive(Debug, Deserialize)]
struct UsageQuery {
    days: Option<i64>,
}

/// Daily ingestion counters for the last `days` days (default 30, at most
/// the 400 kept), oldest first and zero-filled. Counts flushed by every
/// engine instance, so the current day lags by up to ~10 s.
async fn source_usage(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
    query: Result<Query<UsageQuery>, QueryRejection>,
) -> Result<Json<Vec<DailyUsage>>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    source::find_in_project(&state.db, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    // A `days` that is not a number is the same kind of mistake as one out of
    // range, and answers in the API's own JSON shape.
    let invalid = || {
        AppError::Validation(format!(
            "days must be a whole number between 1 and {}",
            usage::RETENTION_DAYS
        ))
    };
    let days = query.map_err(|_| invalid())?.0.days.unwrap_or(30);
    if !(1..=usage::RETENTION_DAYS).contains(&days) {
        return Err(invalid());
    }
    Ok(Json(
        usage::daily_usage(&state.db, source_id, days, Utc::now().date_naive()).await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineBody {
    rules: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineTestBody {
    rules: Value,
    events: Vec<Value>,
}

fn pipeline_reply(rules: Value, updated_at: chrono::DateTime<Utc>) -> Json<Value> {
    Json(json!({ "rules": rules, "updated_at": updated_at }))
}

/// Runs CPU work (compiling regexes, applying rules) on a blocking thread,
/// never an async worker. A panic there is re-raised here.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value,
        Err(error) => std::panic::resume_unwind(error.into_panic()),
    }
}

/// Validates and compiles `rules` off the async workers and returns their
/// canonical form, which is what gets stored.
async fn validate_rules(rules: Value) -> Result<Value, AppError> {
    let parsed = blocking(move || {
        let parsed = pipeline::parse_rules(&rules)?;
        pipeline::compile(&parsed)?;
        Ok(parsed)
    })
    .await
    .map_err(|error: pipeline::PipelineError| AppError::Validation(error.to_string()))?;
    serde_json::to_value(&parsed)
        .map_err(|_| AppError::Validation("rules could not be stored".to_string()))
}

async fn get_pipeline(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    let (rules, updated_at) = source::get_pipeline(&state.db, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(pipeline_reply(rules, updated_at))
}

/// Replaces the source's rules as a whole. They are validated and compiled
/// first; nothing is stored when any rule is invalid (400 naming it).
async fn put_pipeline(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PipelineBody>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    // 404 for a source outside this project before any CPU is spent on the
    // rules, as the preview does.
    source::find_in_project(&state.db, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let rules = validate_rules(payload.rules).await?;
    let updated_at = source::set_pipeline(&state.db, project_id, source_id, &rules)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(pipeline_reply(rules, updated_at))
}

/// Runs `rules` (not necessarily the stored ones) over at most
/// [`pipeline::MAX_TEST_EVENTS`] sample events and shows what each would
/// become. Nothing is stored and no sample counter outlives the request.
async fn test_pipeline(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PipelineTestBody>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Member).await?;
    let source = source::ingest_source(&state.db, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if payload.events.len() > pipeline::MAX_TEST_EVENTS {
        return Err(AppError::Validation(format!(
            "at most {} sample events",
            pipeline::MAX_TEST_EVENTS
        )));
    }
    // Checked before any CPU work.
    let events: Vec<Map<String, Value>> = payload
        .events
        .into_iter()
        .enumerate()
        .map(|(index, event)| match event {
            Value::Object(object) => Ok(object),
            _ => Err(AppError::Validation(format!(
                "events[{index}] must be a JSON object"
            ))),
        })
        .collect::<Result<_, _>>()?;
    let rules = payload.rules;
    let results = blocking(move || -> Result<Vec<Value>, AppError> {
        let compiled = pipeline::parse_rules(&rules)
            .and_then(|parsed| pipeline::compile(&parsed))
            .map_err(|error| AppError::Validation(error.to_string()))?;
        let now = Utc::now();
        Ok(events
            .into_iter()
            .map(
                |object| match compiled.apply(normalize::from_json(object, now)) {
                    Outcome::Keep(event) => {
                        let row = store::build_row_at(event, &source, now);
                        let fields: Value = serde_json::from_str(&row.fields)
                            .unwrap_or_else(|_| Value::Object(Map::new()));
                        json!({
                            "outcome": "kept",
                            "event": {
                                "dt": row.dt,
                                "level": row.level,
                                "message": row.message,
                                "fields": fields,
                                "trace_id": row.trace_id,
                                "span_id": row.span_id,
                                "pattern": row.pattern,
                            },
                        })
                    }
                    Outcome::Dropped => json!({ "outcome": "dropped" }),
                    Outcome::SampledOut => json!({ "outcome": "sampled_out" }),
                },
            )
            .collect())
    })
    .await?;
    Ok(Json(json!({ "results": results })))
}
