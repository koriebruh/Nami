use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Extension, Json, Router,
    body::{Body, HttpBody},
    extract::{Path, Query, Request, State, rejection::PathRejection},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{
    error::AppError,
    logs::rate_limit::{InFlight, TokenBuckets},
    middleware::rbac::{CurrentUser, ProjectReader, require_project_reader, require_project_role},
    models::{
        heartbeat::{
            self, CreateHeartbeatRequest, Heartbeat, HeartbeatView, PatchHeartbeatRequest,
        },
        maintenance, monitor_group,
    },
    routes::monitors::RangeQuery,
    state::AppState,
    uptime::{
        alerts,
        availability::{self, Availability},
        heartbeat::{
            MAX_OUTPUT_BYTES, Ping, PingKind, SUCCESS_OUTPUT_MAX, is_well_formed_token,
            lookup_token, parse_signal, record_signal, sanitize_output_capped,
        },
        pings::{self, HeartbeatPingRow},
    },
};

// Same RBAC as monitors: viewer reads, member creates/edits (and rotates the
// ping token), admin deletes. Viewers read heartbeats without their ping token.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/heartbeats",
            get(list_heartbeats).post(create_heartbeat),
        )
        .route(
            "/projects/{id}/heartbeats/{heartbeat_id}",
            get(get_heartbeat)
                .patch(patch_heartbeat)
                .delete(delete_heartbeat),
        )
        .route(
            "/projects/{id}/heartbeats/{heartbeat_id}/rotate-token",
            post(rotate_heartbeat_token),
        )
        .route(
            "/projects/{id}/heartbeats/{heartbeat_id}/availability",
            get(get_availability),
        )
        .route(
            "/projects/{id}/heartbeats/{heartbeat_id}/pings",
            get(list_pings),
        )
}

/// `GET .../pings?limit=`: the default when `limit` is absent. A `limit`
/// outside `MIN_PINGS_LIMIT..=MAX_PINGS_LIMIT` is clamped into it.
pub const DEFAULT_PINGS_LIMIT: u32 = 50;
const MIN_PINGS_LIMIT: u32 = 1;
const MAX_PINGS_LIMIT: u32 = 100;

/// How long a ping waits for ClickHouse to store its row. The ping itself
/// (state, incident, alert) is already committed: a slow or failing
/// ClickHouse only loses the log row.
const PING_LOG_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
struct PingsQuery {
    limit: Option<u32>,
}

async fn get_availability(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
    Query(range): Query<RangeQuery>,
) -> Result<Json<Availability>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    availability::check_range(range.from, range.to).map_err(AppError::Validation)?;
    availability::for_heartbeat(
        &state.db,
        project_id,
        heartbeat_id,
        range.from,
        range.to,
        Utc::now(),
    )
    .await?
    .map(Json)
    .ok_or(AppError::NotFound)
}

async fn list_pings(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<PingsQuery>,
) -> Result<Json<Vec<HeartbeatPingRow>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    heartbeat::find_in_project(&state.db, project_id, heartbeat_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_PINGS_LIMIT)
        .clamp(MIN_PINGS_LIMIT, MAX_PINGS_LIMIT);
    Ok(Json(
        pings::recent_pings(&state.clickhouse, project_id, heartbeat_id, limit).await?,
    ))
}

/// How many pings may be handled at once. The endpoint is public and needs no
/// session, and every well-formed token costs database work: a ping that finds
/// every place taken is answered at once with `503` and `Retry-After: 1`
/// (cron jobs retry, e.g. `curl --retry 3`) instead of waiting for a pooled
/// connection, so a flood of pings can never starve the API or the worker of
/// the pool they share.
pub const MAX_PINGS_IN_FLIGHT: usize = 4;

/// The largest ping body read (`curl --data-binary`, the job's output); a
/// larger one is `413`, whether it declares its length or not.
pub const MAX_PING_BODY_BYTES: usize = 1024 * 1024;
/// The steady rate of pings one heartbeat may send, per second.
pub const PINGS_PER_SECOND: f64 = 1.0;
/// How many pings one heartbeat may send at once before [`PINGS_PER_SECOND`]
/// applies; beyond that, `429` with `Retry-After: 1`.
pub const PING_BURST: f64 = 10.0;
/// How many ping bodies are read at once across the endpoint (one per
/// heartbeat at most); a body that finds every place taken is `503`.
pub const MAX_PING_BODIES_IN_FLIGHT: usize = 16;
/// How long a ping body may take to arrive; a slower one is `408`.
pub const PING_BODY_TIMEOUT: Duration = Duration::from_secs(10);
/// Past this many rate-limit buckets, the full ones (which behave like
/// missing ones) are dropped before a ping takes a token.
const PRUNE_BUCKETS_ABOVE: usize = 10_000;

/// Everything that bounds the work pings cause: the database places (see
/// [`MAX_PINGS_IN_FLIGHT`]), the per-heartbeat token buckets, one body read
/// per heartbeat at a time and [`MAX_PING_BODIES_IN_FLIGHT`] body reads in
/// all.
#[derive(Clone)]
struct PingGuards {
    slots: Arc<Semaphore>,
    limits: Arc<TokenBuckets>,
    bodies_per_heartbeat: Arc<InFlight>,
    body_slots: Arc<Semaphore>,
}

/// The public ping endpoint, served on the public listener only (see
/// `router::build_public_router`). No session: the token in the path is the
/// secret, and cron jobs call it directly. `/{token}` is a success ping,
/// `/{token}/fail` a failure and `/{token}/{exit_code}` either (`0` is a
/// success). Each database step of a ping must hold one of `ping_slots`'
/// places; `ping_limits` holds the per-heartbeat token buckets. Every reply
/// says `Cache-Control: no-store`: a ping is a GET with a side effect, which
/// no cache may answer or keep.
pub fn public_router(
    ping_slots: Arc<Semaphore>,
    ping_limits: Arc<TokenBuckets>,
) -> Router<AppState> {
    let guards = PingGuards {
        slots: ping_slots,
        limits: ping_limits,
        bodies_per_heartbeat: Arc::new(InFlight::new(1)),
        body_slots: Arc::new(Semaphore::new(MAX_PING_BODIES_IN_FLIGHT)),
    };
    Router::new()
        .route("/api/v1/heartbeat/{token}", get(ping).post(ping))
        .route(
            "/api/v1/heartbeat/{token}/{signal}",
            get(ping_signal).post(ping_signal),
        )
        .layer(Extension(guards))
        .layer(middleware::from_fn(no_store))
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Checks that the caller may read the heartbeats of `project_id` and says
/// whether they may also see the ping tokens: only a member of the project or
/// above may (a global admin counts). Whoever holds a token can ping, and a
/// ping can resolve an incident or hide a dead job, so a viewer, who may only
/// read, must not get one.
async fn read_access(
    state: &AppState,
    user: &CurrentUser,
    project_id: Uuid,
) -> Result<bool, AppError> {
    let reader = require_project_reader(&state.db, user, project_id).await?;
    Ok(reader == ProjectReader::Member)
}

/// Whether a project-wide maintenance window covers the heartbeats of
/// `project_id` now. One query, whether for one heartbeat or a whole list:
/// every heartbeat of a project shares the answer.
async fn in_maintenance(state: &AppState, project_id: Uuid) -> Result<bool, AppError> {
    Ok(maintenance::heartbeat_in_maintenance(&state.db, project_id, Utc::now()).await?)
}

/// `heartbeat` as a member sees it (with its token), maintenance looked up.
async fn member_view(state: &AppState, heartbeat: Heartbeat) -> Result<HeartbeatView, AppError> {
    let in_maintenance = in_maintenance(state, heartbeat.project_id).await?;
    Ok(heartbeat.into_view(true, in_maintenance))
}

async fn list_heartbeats(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<HeartbeatView>>, AppError> {
    let with_token = read_access(&state, &user, project_id).await?;
    let heartbeats = heartbeat::list_for_project(&state.db, project_id).await?;
    let in_maintenance = in_maintenance(&state, project_id).await?;
    Ok(Json(
        heartbeats
            .into_iter()
            .map(|heartbeat| heartbeat.into_view(with_token, in_maintenance))
            .collect(),
    ))
}

async fn create_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateHeartbeatRequest>,
) -> Result<(StatusCode, Json<HeartbeatView>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let config = heartbeat::validate(payload.into_config()).map_err(AppError::Validation)?;
    // A group of another project is a 404, like any foreign id.
    if let Some(group_id) = config.group_id
        && !monitor_group::belongs_to(&state.db, project_id, group_id).await?
    {
        return Err(AppError::NotFound);
    }
    let created = heartbeat::insert_heartbeat(&state.db, project_id, &config)
        .await
        .map_err(|e| match &e {
            // Global admins bypass membership: the project may not exist.
            sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => {
                AppError::NotFound
            }
            _ => AppError::Database(e),
        })?;
    Ok((
        StatusCode::CREATED,
        Json(member_view(&state, created).await?),
    ))
}

async fn get_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<HeartbeatView>, AppError> {
    let with_token = read_access(&state, &user, project_id).await?;
    let found = heartbeat::find_in_project(&state.db, project_id, heartbeat_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let in_maintenance = in_maintenance(&state, project_id).await?;
    Ok(Json(found.into_view(with_token, in_maintenance)))
}

async fn patch_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchHeartbeatRequest>,
) -> Result<Json<HeartbeatView>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let updated = heartbeat::update_in_project(&state.db, project_id, heartbeat_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(member_view(&state, updated).await?))
}

/// Replaces the ping token; the old one stops working at once. The reply is
/// the heartbeat with its new token, so the caller can show the new ping URL.
async fn rotate_heartbeat_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<HeartbeatView>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let rotated = heartbeat::rotate_token(&state.db, project_id, heartbeat_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(member_view(&state, rotated).await?))
}

async fn delete_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !heartbeat::delete_in_project(&state.db, project_id, heartbeat_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}

async fn ping(
    State(state): State<AppState>,
    Extension(guards): Extension<PingGuards>,
    token: Result<Path<String>, PathRejection>,
    request: Request,
) -> Result<Response, AppError> {
    // A path segment that is not even valid UTF-8 is as unknown a token as
    // any other, and costs no database work.
    let Ok(Path(token)) = token else {
        return Err(AppError::NotFound);
    };
    handle_ping(state, guards, token, None, request).await
}

async fn ping_signal(
    State(state): State<AppState>,
    Extension(guards): Extension<PingGuards>,
    path: Result<Path<(String, String)>, PathRejection>,
    request: Request,
) -> Result<Response, AppError> {
    let Ok(Path((token, signal))) = path else {
        return Err(AppError::NotFound);
    };
    handle_ping(state, guards, token, Some(signal), request).await
}

/// One ping, every step returning before the next (see the plan's "Path order
/// on pings"): token shape (404) → suffix (400) → declared length (413) →
/// token lookup under a database place (503, 404) → the heartbeat's rate
/// limit (429) → the body, one per heartbeat and [`MAX_PING_BODIES_IN_FLIGHT`]
/// in all, at most [`MAX_PING_BODY_BYTES`] within [`PING_BODY_TIMEOUT`]
/// (429, 503, 413, 408) → the record under a database place again (503).
///
/// No body is read before its token is known and its rate limit passed, so
/// nobody without a token can make the endpoint buffer anything, and no
/// database place is held while a body arrives, so a slow body (one token
/// holder's) cannot block other heartbeats' pings.
async fn handle_ping(
    state: AppState,
    guards: PingGuards,
    token: String,
    signal: Option<String>,
    request: Request,
) -> Result<Response, AppError> {
    if !is_well_formed_token(&token) {
        return Err(AppError::NotFound);
    }
    let (kind, exit_code) = match signal.as_deref().map(parse_signal) {
        None => (PingKind::Success, None),
        Some(Some(parsed)) => parsed,
        Some(None) => {
            return Err(AppError::Validation(
                "the last path segment must be fail or an exit code".to_string(),
            ));
        }
    };
    let (parts, body) = request.into_parts();
    let declared = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > MAX_PING_BODY_BYTES as u64) {
        return Ok(body_too_large());
    }

    let heartbeat_id = {
        let Ok(_slot) = guards.slots.try_acquire() else {
            return Ok(busy());
        };
        lookup_token(&state.db, &token)
            .await?
            .ok_or(AppError::NotFound)?
    };

    if guards.limits.len() > PRUNE_BUCKETS_ABOVE {
        guards.limits.prune_full(Instant::now());
    }
    if !guards.limits.try_acquire(heartbeat_id) {
        return Ok(too_many());
    }

    // A success ping keeps little output (nothing attaches it to an incident);
    // a failure ping keeps the full cap.
    let output_cap = match kind {
        PingKind::Success => SUCCESS_OUTPUT_MAX,
        PingKind::Fail => MAX_OUTPUT_BYTES,
    };
    let mut output = String::new();
    if body.size_hint().exact() != Some(0) {
        let Some(_reading) = guards.bodies_per_heartbeat.try_enter(heartbeat_id) else {
            return Ok(too_many());
        };
        let Ok(_body_slot) = guards.body_slots.try_acquire() else {
            return Ok(busy());
        };
        match read_body(body).await {
            Ok(bytes) => output = sanitize_output_capped(&bytes, output_cap),
            Err(reply) => return Ok(*reply),
        }
    }

    let (ping, recorded) = {
        let Ok(_slot) = guards.slots.try_acquire() else {
            return Ok(busy());
        };
        let ping = Ping {
            kind,
            exit_code,
            output,
        };
        let recorded = record_signal(&state.db, &token, &ping)
            .await?
            .ok_or(AppError::NotFound)?;
        (ping, recorded)
    };
    let row = HeartbeatPingRow {
        heartbeat_id: recorded.heartbeat_id,
        project_id: recorded.project_id,
        received_at: recorded.received_at,
        kind: ping.kind,
        exit_code: ping.exit_code,
        output: ping.output,
    };
    if let Some(transition) = recorded.transition {
        // A cron job is waiting on this response: deliver the alert in the
        // background.
        let state = state.clone();
        tokio::spawn(async move { alerts::notify_transition(&state, &transition).await });
    }
    // After the record slot is dropped, so a slow ClickHouse never holds one.
    match tokio::time::timeout(
        PING_LOG_TIMEOUT,
        pings::insert_ping(&state.clickhouse, &row),
    )
    .await
    {
        Ok(Ok(())) => {}
        // Never the token: only ids.
        Ok(Err(e)) => {
            tracing::warn!(error = %e, heartbeat_id = %row.heartbeat_id, "heartbeat ping not recorded in ClickHouse");
        }
        Err(_) => {
            tracing::warn!(heartbeat_id = %row.heartbeat_id, "heartbeat ping not recorded in ClickHouse (timeout)");
        }
    }
    Ok(Json(json!({ "ok": true })).into_response())
}

/// Reads a ping body of at most [`MAX_PING_BODY_BYTES`] within
/// [`PING_BODY_TIMEOUT`], or the reply that says why it was not.
async fn read_body(body: Body) -> Result<axum::body::Bytes, Box<Response>> {
    let read = tokio::time::timeout(
        PING_BODY_TIMEOUT,
        axum::body::to_bytes(body, MAX_PING_BODY_BYTES),
    )
    .await;
    let reply = match read {
        Ok(Ok(bytes)) => return Ok(bytes),
        Err(_) => (
            StatusCode::REQUEST_TIMEOUT,
            Json(json!({ "error": "ping body not received in time" })),
        )
            .into_response(),
        Ok(Err(error)) if is_length_limit(&error) => body_too_large(),
        Ok(Err(_)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "ping body could not be read" })),
        )
            .into_response(),
    };
    Err(Box::new(reply))
}

/// Whether a body error is the length cap of `to_bytes` (anywhere in its
/// source chain).
fn is_length_limit(error: &axum::Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = current {
        if error.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        current = error.source();
    }
    false
}

fn body_too_large() -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(json!({ "error": "ping body too large (max 1 MiB)" })),
    )
        .into_response()
}

/// The reply to a heartbeat over its rate limit, or with a body already being
/// read: try again in a second.
fn too_many() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "1")],
        Json(json!({ "error": "too many requests" })),
    )
        .into_response()
}

/// The reply to a ping that found every place taken: try again in a second.
fn busy() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "1")],
        Json(json!({ "error": "busy, retry shortly" })),
    )
        .into_response()
}
