use axum::{routing::get, Router, Json};
use serde_json::json;
use crate::{state::AppState, error::AppError};

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    sqlx::query("SELECT 1").execute(&state.db).await?;
    Ok(Json(json!({ "status": "ok" })))
}
