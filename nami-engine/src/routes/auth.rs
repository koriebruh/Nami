use axum::{routing::{get, post}, Router, Json, extract::{State, Query}};
use serde::Deserialize;
use serde_json::{json, Value};
use crate::{state::AppState, error::AppError, auth, models};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/verify-email", get(verify_email))
}

#[derive(Deserialize)]
struct RegisterRequest {
    email: String,
    password: String,
}

async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterRequest>,
) -> Result<Json<Value>, AppError> {
    let generic_response = Json(json!({
        "message": "if this email is new, a verification link has been sent"
    }));

    if models::user::find_by_email(&state.db, &payload.email).await?.is_some() {
        return Ok(generic_response);
    }

    let password_hash = auth::password::hash_password(&payload.password)?;
    let user = models::user::insert_user(&state.db, &payload.email, &password_hash).await?;

    let (raw_token, token_hash) = auth::token::new_opaque_token();
    models::token::insert_verification_token(&state.db, user.id, &token_hash).await?;

    state
        .mailer
        .send(
            &user.email,
            "Verify your Nami account",
            format!("Verify your email: /auth/verify-email?token={raw_token}"),
        )
        .await?;

    Ok(generic_response)
}

#[derive(Deserialize)]
struct VerifyEmailQuery {
    token: String,
}

async fn verify_email(
    State(state): State<AppState>,
    Query(query): Query<VerifyEmailQuery>,
) -> Result<Json<Value>, AppError> {
    let token_hash = auth::token::hash_token(&query.token);
    let user_id = models::token::consume_valid_verification_token(&state.db, &token_hash)
        .await?
        .ok_or_else(|| AppError::Validation("invalid or expired token".into()))?;

    models::user::mark_email_verified(&state.db, user_id).await?;
    Ok(Json(json!({ "message": "email verified" })))
}
