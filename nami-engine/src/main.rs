use nami_engine::{config::Config, db, state::AppState, routes};
use axum::Router;

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

    let state = AppState { db: pool };
    let app = Router::new()
        .merge(routes::health::router())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port))
        .await
        .unwrap();
    tracing::info!("listening on port {}", config.port);
    axum::serve(listener, app).await.unwrap();
}
