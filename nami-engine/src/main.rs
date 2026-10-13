use nami_engine::{config::Config, db, email, error::AppError, state::AppState, routes};
use axum::{response::IntoResponse, Router};
use std::net::SocketAddr;
use std::sync::Arc;
use time::Duration as TimeDuration;
use tower_governor::{governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor, GovernorLayer};
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::PostgresStore;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let config = Config::from_env();
    let pool = db::connect_pool(&config.database_url)
        .await
        .expect("failed to connect to database");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("failed to run migrations");

    // tower-sessions-sqlx-store 0.15.0 pins sqlx ^0.8, which is incompatible
    // (different crate version = different `PgPool` type) with this crate's
    // sqlx 0.9.0 pool. We use a dedicated sqlx-0.8 pool (aliased as
    // `sqlx-session` in Cargo.toml) solely for the session store, pointed at
    // the same database.
    let session_pool = sqlx_session::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database_url)
        .await
        .expect("failed to connect session pool");
    let session_store = PostgresStore::new(session_pool);
    session_store
        .migrate()
        .await
        .expect("failed to migrate session store");

    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(false) // set true once served over HTTPS in production
        .with_expiry(Expiry::OnInactivity(TimeDuration::hours(24)))
        // tower-sessions defaults to `always_save = false`, so a read-only
        // request (e.g. /auth/me) never re-persists the session and the
        // "inactivity" expiry becomes a fixed window from the last write,
        // not a real sliding expiry. Force a save on every request instead.
        .with_always_save(true);

    let port = config.port;
    let state = AppState {
        db: pool,
        mailer: email::Mailer::new(&config.smtp_host, config.smtp_port),
        config,
    };
    // `SmartIpKeyExtractor`, not `GlobalKeyExtractor`: the latter is a single
    // deployment-wide bucket, so any one client sending 6 requests locks out
    // login/registration for every user — the opposite of what a rate
    // limiter is for. `SmartIpKeyExtractor` checks `X-Forwarded-For` ->
    // `X-Real-IP` -> `Forwarded`, then falls back to the TCP peer address via
    // `ConnectInfo` (populated below via `into_make_service_with_connect_info`).
    // Today, with no upstream setting `X-Forwarded-For`, this falls back to
    // the peer address, which is always nami-ui's own IP (this is a BFF) —
    // one bucket, the same as `GlobalKeyExtractor` for legitimate traffic,
    // but NOT exploitable the same way: an external attacker can't forge the
    // TCP peer IP, so they can't use this to lock out every other user. Once
    // nami-ui forwards the real end-user IP via `X-Forwarded-For` (a later
    // task), this becomes genuinely per-end-user with no further changes here.
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(SmartIpKeyExtractor)
            .per_second(60)
            .burst_size(5)
            .finish()
            .expect("failed to build rate limit config"),
    );

    let app = Router::new()
        .merge(routes::health::router())
        .merge(routes::auth::router())
        .merge(
            routes::auth::rate_limited_router().layer(
                GovernorLayer::new(governor_conf)
                    // Default error body is tower_governor's own plain text
                    // ("Too Many Requests! Wait for Ns"), which doesn't match
                    // this service's `{"error": "..."}` JSON shape and would
                    // break nami-ui's response parser. Route it through the
                    // same `AppError` the rest of the service uses.
                    .error_handler(|_err| AppError::RateLimited.into_response()),
            ),
        )
        .merge(routes::projects::router())
        .layer(session_layer)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .unwrap();
    tracing::info!("listening on port {}", port);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}
