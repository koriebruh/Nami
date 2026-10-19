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
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{
        CurrentUser, ProjectReader, require_project_reader, require_project_role,
    },
    core::state::AppState,
    logs::ingest::rate_limit::{InFlight, TokenBuckets},
    uptime::group::model as monitor_group,
    uptime::heartbeat::model::{
        self as heartbeat, CreateHeartbeatRequest, Heartbeat, HeartbeatView, PatchHeartbeatRequest,
    },
    uptime::maintenance::model as maintenance,
    uptime::{
        alerts,
        availability::{self, Availability, RangeQuery},
        heartbeat::pings::{self, HeartbeatPingRow},
        heartbeat::{
            MAX_OUTPUT_BYTES, OUTPUT_BUDGET_MARKER, OUTPUT_BUSY_MARKER, OUTPUT_LATE_MARKER,
            OUTPUT_UNREADABLE_MARKER, Ping, PingKind, SUCCESS_OUTPUT_MAX, is_output_marker,
            is_well_formed_token, lookup_token, parse_signal, record_signal,
            sanitize_output_capped,
        },
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

/// How many pings may do database work at once. The endpoint is public and
/// needs no session, and every well-formed token costs database work: a ping
/// waits at most [`PING_SLOT_WAIT`] for a place (cron fleets ping at the same
/// second, `:00`) and is then answered `503` with `Retry-After: 1`, so a flood
/// of pings costs latency, never an unbounded queue, and can never starve the
/// API or the worker of the pool they share.
pub const MAX_PINGS_IN_FLIGHT: usize = 4;
/// How long a ping waits for one of the [`MAX_PINGS_IN_FLIGHT`] places.
pub const PING_SLOT_WAIT: Duration = Duration::from_secs(2);

/// The largest ping body declared (`Content-Length`) that is accepted; a
/// larger declared one is `413`. An undeclared (chunked) body is read up to
/// this many bytes and the rest ignored.
pub const MAX_PING_BODY_BYTES: usize = 1024 * 1024;
/// The steady rate of pings one heartbeat may send, per second.
pub const PINGS_PER_SECOND: f64 = 1.0;
/// How many pings one heartbeat may send at once before [`PINGS_PER_SECOND`]
/// applies; beyond that, `429` with `Retry-After: 1`.
pub const PING_BURST: f64 = 10.0;
/// How many ping bodies are read at once across the endpoint (one per
/// heartbeat and [`MAX_PING_BODIES_PER_PROJECT`] per project at most).
pub const MAX_PING_BODIES_IN_FLIGHT: usize = 16;
/// How many ping bodies of one project are read at once, so one tenant
/// cannot hold every one of the [`MAX_PING_BODIES_IN_FLIGHT`] places.
pub const MAX_PING_BODIES_PER_PROJECT: usize = 2;
/// How long a ping body may take to arrive.
pub const PING_BODY_TIMEOUT: Duration = Duration::from_secs(10);
/// Output bytes one heartbeat may store per hour (refilled continuously, up
/// to this much at once); past it, pings are stored with
/// [`OUTPUT_BUDGET_MARKER`] in place of their output.
pub const OUTPUT_BYTES_PER_HOUR: usize = 1024 * 1024;
/// Past this many rate-limit buckets, the full ones (which behave like
/// missing ones) are dropped before a ping takes a token.
const PRUNE_BUCKETS_ABOVE: usize = 10_000;

/// Everything that bounds the work pings cause: the database places (see
/// [`MAX_PINGS_IN_FLIGHT`]), the per-heartbeat token buckets, one body read
/// per heartbeat at a time, [`MAX_PING_BODIES_PER_PROJECT`] per project and
/// [`MAX_PING_BODIES_IN_FLIGHT`] in all, and the per-heartbeat output budget
/// ([`OUTPUT_BYTES_PER_HOUR`]).
#[derive(Clone)]
struct PingGuards {
    slots: Arc<Semaphore>,
    limits: Arc<TokenBuckets>,
    bodies_per_heartbeat: Arc<InFlight>,
    bodies_per_project: Arc<InFlight>,
    body_slots: Arc<Semaphore>,
    output_budget: Arc<TokenBuckets>,
}

/// The public ping endpoint, served on the public listener only (see
/// `core::router::build_public_router`). No session: the token in the path is the
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
        bodies_per_project: Arc::new(InFlight::new(MAX_PING_BODIES_PER_PROJECT)),
        body_slots: Arc::new(Semaphore::new(MAX_PING_BODIES_IN_FLIGHT)),
        output_budget: Arc::new(output_budget()),
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
/// token lookup under a database place (503 after [`PING_SLOT_WAIT`], 404) →
/// the heartbeat's rate limit (429) → the body → the record under a database
/// place again (503 after [`PING_SLOT_WAIT`]).
///
/// The path alone carries the signal (success, failure, exit code), so the
/// body never decides whether a ping counts: a body that cannot be read (its
/// heartbeat already has one being read, its project
/// [`MAX_PING_BODIES_PER_PROJECT`], the endpoint [`MAX_PING_BODIES_IN_FLIGHT`];
/// or it does not arrive within [`PING_BODY_TIMEOUT`], or breaks off) is
/// dropped and the ping recorded with a fixed marker in place of its output.
/// Only a declared length over [`MAX_PING_BODY_BYTES`] is refused (413).
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
    // The declared length: the header, or the body's exact size (hyper's
    // incoming body takes it from `Content-Length`).
    let declared = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .or(body.size_hint().exact());
    if declared.is_some_and(|length| length > MAX_PING_BODY_BYTES as u64) {
        return Ok(body_too_large());
    }

    let (heartbeat_id, project_id) = {
        let Some(_slot) = ping_slot(&guards.slots).await else {
            return Ok(busy());
        };
        lookup_token(&state.db, &token)
            .await?
            .ok_or(AppError::NotFound)?
    };

    let now = Instant::now();
    if guards.limits.len() > PRUNE_BUCKETS_ABOVE {
        guards.limits.prune_full(now);
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
        output = match read_output(&guards, heartbeat_id, project_id, body).await {
            BodyRead::Read(bytes) => sanitize_output_capped(&bytes, output_cap),
            BodyRead::Busy => OUTPUT_BUSY_MARKER.to_string(),
            BodyRead::Late => OUTPUT_LATE_MARKER.to_string(),
            BodyRead::Broken => OUTPUT_UNREADABLE_MARKER.to_string(),
        };
        if guards.output_budget.len() > PRUNE_BUCKETS_ABOVE {
            guards.output_budget.prune_full(now);
        }
        output = charge_output(&guards.output_budget, heartbeat_id, output, Instant::now());
    }

    let (ping, recorded) = {
        let Some(_slot) = ping_slot(&guards.slots).await else {
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

/// `output` if the heartbeat's output budget (see [`OUTPUT_BYTES_PER_HOUR`])
/// still holds its length, which is then taken from it; otherwise
/// [`OUTPUT_BUDGET_MARKER`]. Empty output and the fixed markers cost nothing.
fn charge_output(
    budget: &TokenBuckets,
    heartbeat_id: Uuid,
    output: String,
    now: Instant,
) -> String {
    if output.is_empty()
        || is_output_marker(&output)
        || budget.try_take_at(heartbeat_id, output.len() as f64, now)
    {
        output
    } else {
        OUTPUT_BUDGET_MARKER.to_string()
    }
}

/// The per-heartbeat output budget: [`OUTPUT_BYTES_PER_HOUR`], refilled
/// continuously.
fn output_budget() -> TokenBuckets {
    TokenBuckets::new(
        OUTPUT_BYTES_PER_HOUR as f64 / 3600.0,
        OUTPUT_BYTES_PER_HOUR as f64,
    )
}

/// One of the database places, waiting at most [`PING_SLOT_WAIT`] for it.
async fn ping_slot(slots: &Semaphore) -> Option<tokio::sync::SemaphorePermit<'_>> {
    tokio::time::timeout(PING_SLOT_WAIT, slots.acquire())
        .await
        .ok()?
        .ok()
}

/// What became of a ping body.
enum BodyRead {
    /// Its first [`MAX_PING_BODY_BYTES`] bytes.
    Read(Vec<u8>),
    /// No reading place was free (heartbeat, project or endpoint).
    Busy,
    /// It did not arrive within [`PING_BODY_TIMEOUT`].
    Late,
    /// It broke off.
    Broken,
}

/// Reads a ping body under its places: one per heartbeat,
/// [`MAX_PING_BODIES_PER_PROJECT`] per project and
/// [`MAX_PING_BODIES_IN_FLIGHT`] in all, never waited for. At most
/// [`MAX_PING_BODY_BYTES`] are kept (the rest is not read) and the whole read
/// must end within [`PING_BODY_TIMEOUT`].
async fn read_output(
    guards: &PingGuards,
    heartbeat_id: Uuid,
    project_id: Uuid,
    body: Body,
) -> BodyRead {
    let Some(_heartbeat) = guards.bodies_per_heartbeat.try_enter(heartbeat_id) else {
        return BodyRead::Busy;
    };
    let Some(_project) = guards.bodies_per_project.try_enter(project_id) else {
        return BodyRead::Busy;
    };
    let Ok(_body_slot) = guards.body_slots.try_acquire() else {
        return BodyRead::Busy;
    };
    let read = async {
        let mut stream = body.into_data_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                return BodyRead::Broken;
            };
            let room = MAX_PING_BODY_BYTES - bytes.len();
            bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
            if bytes.len() >= MAX_PING_BODY_BYTES {
                break;
            }
        }
        BodyRead::Read(bytes)
    };
    tokio::time::timeout(PING_BODY_TIMEOUT, read)
        .await
        .unwrap_or(BodyRead::Late)
}

fn body_too_large() -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(json!({ "error": "ping body too large (max 1 MiB)" })),
    )
        .into_response()
}

/// The reply to a heartbeat over its rate limit: try again in a second.
fn too_many() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "1")],
        Json(json!({ "error": "too many requests" })),
    )
        .into_response()
}

/// The reply to a ping that waited [`PING_SLOT_WAIT`] for a database place in
/// vain: try again in a second.
fn busy() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "1")],
        Json(json!({ "error": "busy, retry shortly" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 400 failure pings with 10 KiB of output each, one every 9 s (an hour,
    /// about 4 MiB sent), store at most the burst (1 MiB) plus one hour of
    /// refill (1 MiB); the rest get the marker. Sustained, that is 1 MiB per
    /// hour.
    #[test]
    fn an_hour_of_large_outputs_stores_about_the_budget() {
        let budget = output_budget();
        let heartbeat = Uuid::new_v4();
        let start = Instant::now();
        let mut stored = 0usize;
        let mut markers = 0;
        for n in 0..400u64 {
            let now = start + Duration::from_secs(n * 9);
            let output = charge_output(&budget, heartbeat, "x".repeat(MAX_OUTPUT_BYTES), now);
            if output == OUTPUT_BUDGET_MARKER {
                markers += 1;
            } else {
                stored += output.len();
            }
        }
        // The burst (1 MiB) plus at most one hour of refill (1 MiB).
        assert!(stored <= 2 * OUTPUT_BYTES_PER_HOUR, "{stored}");
        assert!(markers > 0);

        // All at once: exactly the burst.
        let burst = output_budget();
        let stored: usize = (0..200)
            .map(|_| charge_output(&burst, heartbeat, "x".repeat(MAX_OUTPUT_BYTES), start))
            .filter(|output| output != OUTPUT_BUDGET_MARKER)
            .map(|output| output.len())
            .sum();
        assert!(stored <= OUTPUT_BYTES_PER_HOUR, "{stored}");
        assert_eq!(
            stored,
            (OUTPUT_BYTES_PER_HOUR / MAX_OUTPUT_BYTES) * MAX_OUTPUT_BYTES
        );
    }

    #[test]
    fn budgets_are_per_heartbeat_and_markers_are_free() {
        let budget = output_budget();
        let now = Instant::now();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let whole = "x".repeat(OUTPUT_BYTES_PER_HOUR);
        assert_eq!(charge_output(&budget, a, whole.clone(), now), whole);
        assert_eq!(
            charge_output(&budget, a, "y".into(), now),
            OUTPUT_BUDGET_MARKER
        );
        assert_eq!(charge_output(&budget, a, String::new(), now), "");
        assert_eq!(
            charge_output(&budget, a, OUTPUT_BUSY_MARKER.into(), now),
            OUTPUT_BUSY_MARKER
        );
        assert_eq!(charge_output(&budget, b, "z".into(), now), "z");
    }
}
