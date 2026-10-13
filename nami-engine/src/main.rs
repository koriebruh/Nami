use nami_engine::{clickhouse::ClickHouse, config::Config, db, email, router, state::AppState};
use std::net::SocketAddr;
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

    let clickhouse = ClickHouse::new(
        &config.clickhouse_url,
        &config.clickhouse_user,
        &config.clickhouse_password,
        &config.clickhouse_database,
    );
    clickhouse
        .migrate()
        .await
        .expect("failed to run ClickHouse migrations");

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

    // Session-layer settings (Expiry::OnInactivity + with_always_save(true))
    // and the rate limiter's governor config/error_handler are defined once
    // in `nami_engine::router` and shared with every test, so a regression
    // removing either can't silently go untested again.
    let session_layer = router::configure_session_layer(session_store, config.cookie_secure);

    let port = config.port;
    let state = AppState {
        db: pool,
        mailer: email::Mailer::new(&config.smtp_host, config.smtp_port),
        clickhouse,
        config,
    };

    let app = router::build_router(state).layer(session_layer);

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
