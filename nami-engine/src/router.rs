//! Single source of truth for assembling the HTTP router, so the real
//! session-layer settings (`Expiry::OnInactivity` + `with_always_save(true)`,
//! both the outcome of a Critical fix in an earlier task) and the governor's
//! rate-limit config/`error_handler` (outcome of a later Critical fix) are
//! defined exactly once and exercised by `main.rs` AND every test, instead of
//! each of seven test files re-building an ad hoc router that quietly drifts
//! from production. `main.rs` calls `build_router` instead of assembling the
//! router inline; test files' `test_app()`/`build_app()` helpers do too.

use axum::{
    extract::{ConnectInfo, Request},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use time::Duration as TimeDuration;
use tower_governor::{governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor, GovernorLayer};
use tower_sessions::{cookie::SameSite, Expiry, SessionManagerLayer, SessionStore};

use crate::{error::AppError, routes, state::AppState};

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
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .merge(routes::health::router())
        .merge(routes::auth::router())
        .merge(rate_limited_with_governor(routes::auth::rate_limited_router()))
        .merge(routes::projects::router())
        .merge(routes::monitors::router())
        .merge(routes::incidents::router())
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
