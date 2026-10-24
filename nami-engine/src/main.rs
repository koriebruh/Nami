use nami_engine::{
    core::clickhouse::ClickHouse,
    core::config::Config,
    core::crypto::{Cipher, TEST_KEY},
    core::db,
    core::email,
    core::router,
    core::serve::{ServeLimits, serve},
    core::state::AppState,
    logs::LogsState,
};
use std::sync::Arc;
use tower_sessions_sqlx_store::PostgresStore;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let config = Config::from_env();
    // The test key is public (it lives in this repository), so secrets sealed
    // under it would not be secret: refuse to run on it. A plain `if` rather
    // than `assert_ne!`, which would print both keys.
    if config.encryption_key == TEST_KEY {
        panic!("NAMI_ENCRYPTION_KEY must not be the built-in test key");
    }
    let pool = db::connect_pool(
        &config.database_url,
        &db::PoolSettings::from_config(&config),
    )
    .await
    .expect("failed to connect to database");
    // On a connection of their own, without the pool's statement timeout.
    let connect_options: sqlx::postgres::PgConnectOptions = config
        .database_url
        .parse()
        .expect("DATABASE_URL must be a Postgres URL");
    db::run_migrations(&connect_options)
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
    // in `nami_engine::core::router` and shared with every test, so a regression
    // removing either can't silently go untested again.
    let session_layer = router::configure_session_layer(session_store, config.cookie_secure);

    let (port, public_port) = (config.port, config.public_port);
    let state = AppState {
        db: pool,
        mailer: email::Mailer::from_settings(&email::SmtpSettings::from_config(&config))
            .unwrap_or_else(|message| panic!("{message}")),
        clickhouse,
        cipher: Cipher::new(&config.encryption_key),
        logs: Arc::new(LogsState::default()),
        alert_throttle: Arc::new(nami_engine::uptime::alert_throttle::AlertThrottle::new()),
        config,
    };

    // Every instance sends the DOWN alerts its flap control held back, worker
    // or not: pings are throttled in whichever process received them.
    tokio::spawn(nami_engine::uptime::alerts::run_deferred_alerts(
        state.clone(),
        nami_engine::uptime::alerts::DEFERRED_ALERT_INTERVAL,
        std::time::Duration::ZERO,
    ));

    // Every instance that ingests logs adds its usage counters to Postgres.
    tokio::spawn(nami_engine::logs::ingest::usage::run_forever(state.clone()));

    // A public ingress like PUBLIC_PORT: off unless SYSLOG_ENABLED=true.
    if state.config.syslog_enabled {
        let syslog_config = nami_engine::logs::syslog::SyslogConfig::from_config(&state.config);
        let syslog = nami_engine::logs::syslog::spawn(state.clone(), syslog_config)
            .await
            .expect("failed to bind the syslog listeners (SYSLOG_TCP_PORT / SYSLOG_UDP_PORT)");
        tracing::info!(tcp = %syslog.tcp_addr, udp = %syslog.udp_addr, "syslog listeners started");
    }

    if state.config.uptime_worker_enabled {
        tokio::spawn(nami_engine::uptime::worker::run_forever(state.clone()));
        tracing::info!("uptime worker started");
        tokio::spawn(nami_engine::logs::alert::run_forever(state.clone()));
        tracing::info!("log alert worker started");
    } else {
        tracing::info!("uptime worker disabled (UPTIME_WORKER_ENABLED=false)");
    }

    let public_app = router::build_public_router(state.clone());
    let app = router::build_router(state).layer(session_layer);

    // Two listeners. The API (`PORT`) is private: nami-ui is its only client,
    // and it trusts the client address nami-ui forwards. The public listener
    // (`PUBLIC_PORT`) serves only the endpoints outside clients call without
    // a session (heartbeat pings and log ingest) and is the only HTTP port to
    // expose. Both have a header deadline and a connection cap (`serve`).
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .unwrap_or_else(|e| panic!("failed to bind PORT {port}: {e}"));
    let public_listener = tokio::net::TcpListener::bind(("0.0.0.0", public_port))
        .await
        .unwrap_or_else(|e| panic!("failed to bind PUBLIC_PORT {public_port}: {e}"));
    tracing::info!("API listening on port {port} (private: only nami-ui may reach it)");
    tracing::info!("public endpoints listening on port {public_port}");
    tokio::join!(
        serve(listener, app, ServeLimits::API),
        serve(public_listener, public_app, ServeLimits::PUBLIC),
    );
}
