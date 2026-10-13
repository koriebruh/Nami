use crate::{error::AppError, state::AppState};
use axum::{Json, Router, routing::get};
use serde_json::json;

/// `/health` on the private API: the engine is up and its database answers.
pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

/// `/health` on the public listener: says only that the listener is up. It
/// never touches the database, so asking costs an anonymous caller's flood
/// nothing that the API or the worker need.
pub fn public_router() -> Router<AppState> {
    Router::new().route("/health", get(|| async { Json(json!({ "status": "ok" })) }))
}

async fn health(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    sqlx::query("SELECT 1").execute(&state.db).await?;
    Ok(Json(json!({ "status": "ok" })))
}
