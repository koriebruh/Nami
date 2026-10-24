//! The link endpoints (R16). No session: the token is the credential, sent in
//! a JSON body (never the URL, so it stays out of request logs). Mounted
//! behind a per-IP governor of their own (`core::router`).

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    core::{error::AppError, state::AppState},
    incidents::{
        action::{self, Inspected, Refused},
        worker,
    },
};

/// Requests one client address may send at once.
pub const BURST: u32 = 20;
/// Seconds until one more request is allowed.
pub const REPLENISH_SECONDS: u64 = 3;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/incident-actions/inspect", post(inspect))
        .route("/incident-actions/confirm", post(confirm))
}

/// Holds a bearer token, so it has no `Debug`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenRequest {
    token: String,
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        match self {
            Refused::Invalid => AppError::Validation("this link is invalid or has expired".into()).into_response(),
            Refused::NoAccess => (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "you no longer have access to this incident" })),
            )
                .into_response(),
            Refused::Used => AppError::Gone("this link was already used".into()).into_response(),
            Refused::AlreadyDone(status) => {
                AppError::Conflict(format!("the incident is already {status}")).into_response()
            }
            Refused::Database(e) => AppError::Database(e).into_response(),
        }
    }
}

/// `POST /incident-actions/inspect {token}`: what the link would do. Never mutates.
async fn inspect(State(state): State<AppState>, Json(req): Json<TokenRequest>) -> Result<Json<Inspected>, Refused> {
    Ok(Json(action::inspect(&state.db, &state.cipher, &req.token, Utc::now()).await?))
}

/// `POST /incident-actions/confirm {token}`: does it, once, then kicks the
/// worker (a resolve queues notices).
async fn confirm(State(state): State<AppState>, Json(req): Json<TokenRequest>) -> Result<Json<Value>, Refused> {
    let status = action::confirm(&state.db, &state.cipher, &req.token, Utc::now()).await?;
    worker::kick(&state).await;
    Ok(Json(json!({ "status": status })))
}
