//! Single source of truth for assembling the HTTP router, so the real
//! session-layer settings (`Expiry::OnInactivity` + `with_always_save(true)`,
//! both the outcome of a Critical fix in an earlier task) and the governor's
//! rate-limit config/`error_handler` (outcome of a later Critical fix) are
//! defined exactly once and exercised by `main.rs` AND every test, instead of
//! each of seven test files re-building an ad hoc router that quietly drifts
//! from production. `main.rs` calls `build_router` instead of assembling the
//! router inline; test files' `test_app()`/`build_app()` helpers do too.

use axum::{
    Json, Router,
    extract::{ConnectInfo, Request},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use time::Duration as TimeDuration;
use tokio::sync::Semaphore;
use tower_governor::{
    GovernorLayer, governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor,
};
use tower_sessions::{Expiry, SessionManagerLayer, SessionStore, cookie::SameSite};

use crate::{
    core::error::AppError, core::state::AppState, logs::rate_limit::TokenBuckets, routes,
    routes::logs_ingest::IngestLimits,
};

/// Wraps `rate_limited_router()` with the real governor config: per-IP
/// (`SmartIpKeyExtractor`, not `GlobalKeyExtractor` — see the historical
/// rationale previously inline in `main.rs`) at 60/s with a burst of 5, and
/// an `error_handler` that converts the response into this service's own
/// `{"error": "..."}` JSON shape instead of tower_governor's default plain
/// text (which nami-ui's BFF response parser can't consume).
fn rate_limited_with_governor(router: Router<AppState>) -> Router<AppState> {
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(SmartIpKeyExtractor)
            .per_second(60)
            .burst_size(5)
            .finish()
            .expect("failed to build rate limit config"),
    );

    router.layer(
        GovernorLayer::new(governor_conf)
            .error_handler(|_err| AppError::RateLimited.into_response()),
    )
}

/// Assembles the complete application router exactly as `main.rs` serves it
/// (all route groups merged, the rate-limited group behind the real governor
/// config above) — everything EXCEPT the session layer, since the session
/// *store* genuinely differs between production (`PostgresStore`) and tests
/// (`tower_sessions::MemoryStore`). Callers add the session layer via
/// `configure_session_layer` below, which is itself shared so the layer's
/// *settings* (as opposed to its store) are identical everywhere.
///
/// This is the private API (`PORT`): nami-ui is its only client, and it is
/// never exposed directly. Its login rate limit is keyed on the client address
/// nami-ui sends in `X-Forwarded-For`, which anyone reaching this port could
/// forge. What outside clients must reach goes on the public listener instead
/// ([`build_public_router`]).
///
/// Every state-changing request must carry a JSON body (see
/// [`require_json_body`]).
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .merge(crate::core::health::router())
        .merge(routes::auth::router())
        .merge(rate_limited_with_governor(
            routes::auth::rate_limited_router(),
        ))
        .merge(routes::projects::router())
        .merge(routes::sources::router())
        .merge(routes::monitors::router())
        .merge(routes::incidents::router())
        .merge(routes::heartbeats::router())
        .merge(routes::maintenance::router())
        .merge(routes::monitor_groups::router())
        .merge(routes::logs::router())
        .merge(routes::log_views::router())
        .layer(middleware::from_fn(require_json_body))
        .with_state(state)
}

/// Refuses a state-changing request (POST, PUT, PATCH) whose body is not
/// declared as JSON with `415`, before any handler, session check or rate
/// limiter sees it. Browsers send forms across sites without asking first
/// (`application/x-www-form-urlencoded`, `multipart/form-data`, `text/plain`),
/// so this keeps a form on another site from ever driving the API with a
/// visitor's session, also for actions that take no body: those send `{}`.
/// Reads and DELETE are left alone: they carry no body, and no browser sends
/// a cross-site DELETE without asking first.
async fn require_json_body(request: Request, next: Next) -> Response {
    let changes_state = matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH
    );
    if changes_state && !declares_json(request.headers()) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(json!({ "error": "send a JSON body (Content-Type: application/json)" })),
        )
            .into_response();
    }
    next.run(request).await
}

/// Whether `Content-Type` is JSON, the way axum's `Json` extractor judges it:
/// `application/json` or `application/<anything>+json`, parameters (such as
/// `charset`) and case ignored.
fn declares_json(headers: &HeaderMap) -> bool {
    let Some(content_type) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let essence = content_type.split(';').next().unwrap_or_default().trim();
    let Some((kind, subtype)) = essence.split_once('/') else {
        return false;
    };
    let subtype = subtype.to_ascii_lowercase();
    kind.eq_ignore_ascii_case("application") && (subtype == "json" || subtype.ends_with("+json"))
}

/// The public listener's router (`PUBLIC_PORT`): only what clients outside
/// reach without a session, namely the heartbeat ping endpoint, the log ingest
/// endpoint (later the OTLP and Sentry ones) and a `/health` that does not
/// touch the database. None of the API is served here, so the public port can
/// be exposed to cron jobs, log shippers and other machines without exposing
/// the API with it. Pings share [`routes::heartbeats::MAX_PINGS_IN_FLIGHT`]
/// places, ingest requests [`routes::logs_ingest::MAX_INGEST_IN_FLIGHT`]. The
/// JSON-only guard of the API (`require_json_body`) does not apply here:
/// ingest takes NDJSON and MessagePack, gzipped, and the token in the
/// `Authorization` header (never a cookie) is its credential.
pub fn build_public_router(state: AppState) -> Router {
    let ingest_limits = IngestLimits::for_pool(state.config.database_max_connections);
    build_public_router_with_limits(
        state,
        default_ping_slots(),
        default_ping_limits(),
        ingest_limits,
    )
}

/// [`build_public_router`] whose pings hold places of `ping_slots` (tests hold
/// its places to fill the endpoint up).
pub fn build_public_router_with_ping_slots(state: AppState, ping_slots: Arc<Semaphore>) -> Router {
    build_public_router_with_limits(
        state,
        ping_slots,
        default_ping_limits(),
        IngestLimits::default(),
    )
}

/// [`build_public_router`] whose pings hold places of `ping_slots` and take
/// tokens of `ping_limits` (tests drain a heartbeat's bucket, or lift the
/// limit for tests that ping one token often).
pub fn build_public_router_with_ping_limits(
    state: AppState,
    ping_slots: Arc<Semaphore>,
    ping_limits: Arc<TokenBuckets>,
) -> Router {
    build_public_router_with_limits(state, ping_slots, ping_limits, IngestLimits::default())
}

/// [`build_public_router`] with the given limits on the ingest endpoint
/// (tests fill its places up, or shorten how long it waits for a body).
pub fn build_public_router_with_ingest_limits(state: AppState, limits: IngestLimits) -> Router {
    build_public_router_with_limits(state, default_ping_slots(), default_ping_limits(), limits)
}

fn default_ping_slots() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(routes::heartbeats::MAX_PINGS_IN_FLIGHT))
}

/// One token bucket per heartbeat: [`routes::heartbeats::PINGS_PER_SECOND`],
/// burst [`routes::heartbeats::PING_BURST`].
fn default_ping_limits() -> Arc<TokenBuckets> {
    Arc::new(TokenBuckets::new(
        routes::heartbeats::PINGS_PER_SECOND,
        routes::heartbeats::PING_BURST,
    ))
}

fn build_public_router_with_limits(
    state: AppState,
    ping_slots: Arc<Semaphore>,
    ping_limits: Arc<TokenBuckets>,
    ingest_limits: IngestLimits,
) -> Router {
    Router::new()
        .merge(crate::core::health::public_router())
        .merge(routes::heartbeats::public_router(ping_slots, ping_limits))
        .merge(routes::logs_ingest::public_router(ingest_limits))
        .with_state(state)
}

/// Builds the `SessionManagerLayer` with the real settings every deployment
/// (and every test) must use: `Expiry::OnInactivity(24h)` for a sliding
/// session lifetime, `with_always_save(true)` (tower-sessions defaults to
/// `always_save = false`, which would make a read-only request like
/// `/auth/me` never re-persist the session, silently turning the "inactivity"
/// expiry into a fixed window from the last write instead of a real sliding
/// expiry), `with_secure(cookie_secure)` (`Config::cookie_secure`,
/// `COOKIE_SECURE` env var — true only once served over HTTPS), and
/// `with_same_site(SameSite::Lax)`.
///
/// Lax (not tower-sessions' own default of Strict) is a deliberate choice,
/// not an oversight: the OAuth login flow (`routes/auth.rs`'s
/// `oauth_callback`) requires the session holding `oauth_csrf` to survive a
/// cross-site-originated top-level GET redirect from Google/GitHub back to
/// this app — Strict blocks cookies on exactly that kind of navigation, Lax
/// does not. Declaring it explicitly here (instead of relying on nami-ui's
/// BFF proxy to coincidentally default to Lax when it relays the cookie,
/// which it did until this was made explicit) means a future change to
/// nami-ui's cookie-relay logic can't silently reopen this gap or silently
/// change the app's CSRF posture without anyone deciding to.
pub fn configure_session_layer<S>(store: S, cookie_secure: bool) -> SessionManagerLayer<S>
where
    S: SessionStore + Clone + Send + Sync + 'static,
{
    SessionManagerLayer::new(store)
        .with_secure(cookie_secure)
        .with_expiry(Expiry::OnInactivity(TimeDuration::hours(24)))
        .with_always_save(true)
        .with_same_site(SameSite::Lax)
}

async fn inject_fallback_connect_info(mut req: Request, next: Next) -> Response {
    req.extensions_mut().insert(ConnectInfo(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        0,
    )));
    next.run(req).await
}

/// Test-only convenience. Production (`main.rs`) serves via
/// `axum::serve(...).into_make_service_with_connect_info::<SocketAddr>()`, so
/// `SmartIpKeyExtractor` can always fall back to the real TCP peer address
/// when no `X-Forwarded-For`/`X-Real-IP`/`Forwarded` header is present.
/// Integration tests built on `tower::ServiceExt::oneshot` never go through
/// that `make_service` wrapper, so without this, a test request to a
/// rate-limited route with no forwarded-IP header would fail key extraction
/// entirely and get converted by the same `error_handler` into a spurious
/// 429/500 — not real rate-limiting, just an inability to identify any
/// client at all. This injects one fixed fallback address so extraction can
/// still succeed; tests that need to prove genuine per-IP isolation (see
/// `tests/rate_limit.rs`) must still set an explicit `X-Forwarded-For`
/// header, which `SmartIpKeyExtractor` checks first and which therefore
/// takes precedence over this fallback.
pub fn with_fallback_connect_info(router: Router) -> Router {
    router.layer(middleware::from_fn(inject_fallback_connect_info))
}
