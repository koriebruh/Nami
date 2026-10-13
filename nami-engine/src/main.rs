use nami_engine::{config::Config, db, email, state::AppState, routes};
use axum::Router;
use time::Duration as TimeDuration;
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
    let app = Router::new()
        .merge(routes::health::router())
        .merge(routes::auth::router())
        .merge(routes::projects::router())
        .layer(session_layer)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .unwrap();
    tracing::info!("listening on port {}", port);
    axum::serve(listener, app).await.unwrap();
}
