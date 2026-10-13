use axum::{routing::{get, post}, Router, Json, extract::{State, Query}};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_sessions::Session;
use crate::{state::AppState, error::AppError, auth, models};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/verify-email", get(verify_email))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
        .route("/auth/totp/setup", post(totp_setup))
        .route("/auth/totp/enable", post(totp_enable))
        .route("/auth/totp/verify", post(totp_verify))
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

#[derive(Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

async fn login(
    State(state): State<AppState>,
    session: Session,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<Value>, AppError> {
    let user = models::user::find_by_email(&state.db, &payload.email)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let password_hash = user.password_hash.as_deref().ok_or(AppError::Unauthorized)?;
    if !auth::password::verify_password(&payload.password, password_hash)? {
        return Err(AppError::Unauthorized);
    }

    if user.totp_enabled {
        // Flush any prior authenticated session before stashing the pending
        // TOTP state, so a stale `user_id` from an earlier login can't
        // survive into this "awaiting 2FA" state.
        session
            .flush()
            .await
            .map_err(|_| AppError::Validation("session error".into()))?;
        session
            .insert("pending_user_id", user.id)
            .await
            .map_err(|_| AppError::Validation("session error".into()))?;
        return Ok(Json(json!({ "requires_totp": true })));
    }

    // Rotate the session ID on authentication to prevent session fixation
    // (a session ID set before login, e.g. by an attacker, must not be
    // reusable as the authenticated session).
    session
        .cycle_id()
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;

    session
        .insert("user_id", user.id)
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;
    session
        .insert("auth_at", chrono::Utc::now())
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;

    Ok(Json(json!({ "authenticated": true })))
}

async fn logout(session: Session) -> Result<Json<Value>, AppError> {
    // `.flush()` (not `.delete()`) clears the session server-side AND marks
    // it so the middleware removes the client's cookie, instead of leaving a
    // stale (now-useless) cookie around until its Max-Age.
    session
        .flush()
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;
    Ok(Json(json!({ "message": "logged out" })))
}

async fn me(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    let user_id: uuid::Uuid = session
        .get("user_id")
        .await
        .map_err(|_| AppError::Validation("session error".into()))?
        .ok_or(AppError::Unauthorized)?;

    let auth_at: chrono::DateTime<chrono::Utc> = session
        .get("auth_at")
        .await
        .map_err(|_| AppError::Validation("session error".into()))?
        .ok_or(AppError::Unauthorized)?;

    let user: models::user::User = sqlx::query_as("select * from users where id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)?;

    // Task 9 adds `password_changed_at`; until then this check is a no-op.
    let _ = auth_at;

    Ok(Json(json!({ "id": user.id, "email": user.email, "global_role": user.global_role })))
}

async fn current_user_id(session: &Session) -> Result<uuid::Uuid, AppError> {
    session
        .get("user_id")
        .await
        .map_err(|_| AppError::Validation("session error".into()))?
        .ok_or(AppError::Unauthorized)
}

async fn totp_setup(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    let user_id = current_user_id(&session).await?;
    let user: models::user::User = sqlx::query_as("select * from users where id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;

    let secret = auth::totp::generate_secret();
    sqlx::query("update users set totp_secret = $1, totp_enabled = false where id = $2")
        .bind(&secret)
        .bind(user_id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({ "provisioning_uri": auth::totp::provisioning_uri(&secret, &user.email) })))
}

#[derive(Deserialize)]
struct TotpCodeRequest {
    code: String,
}

async fn totp_enable(
    State(state): State<AppState>,
    session: Session,
    Json(payload): Json<TotpCodeRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = current_user_id(&session).await?;
    let user: models::user::User = sqlx::query_as("select * from users where id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;
    let secret = user.totp_secret.ok_or_else(|| AppError::Validation("run setup first".into()))?;

    if !auth::totp::verify_code(&secret, &user.email, &payload.code) {
        return Err(AppError::Validation("invalid code".into()));
    }

    sqlx::query("update users set totp_enabled = true where id = $1")
        .bind(user_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "enabled": true })))
}

async fn totp_verify(
    State(state): State<AppState>,
    session: Session,
    Json(payload): Json<TotpCodeRequest>,
) -> Result<Json<Value>, AppError> {
    let pending_user_id: uuid::Uuid = session
        .get("pending_user_id")
        .await
        .map_err(|_| AppError::Validation("session error".into()))?
        .ok_or(AppError::Unauthorized)?;

    let user: models::user::User = sqlx::query_as("select * from users where id = $1")
        .bind(pending_user_id)
        .fetch_one(&state.db)
        .await?;
    let secret = user.totp_secret.ok_or(AppError::Unauthorized)?;

    if !auth::totp::verify_code(&secret, &user.email, &payload.code) {
        return Err(AppError::Unauthorized);
    }

    session.remove::<uuid::Uuid>("pending_user_id").await.ok();

    // Rotate the session ID before promoting this "pending 2FA" session to a
    // fully authenticated one, mirroring `login`'s non-TOTP success path.
    // Without this, a session ID that existed before TOTP verification
    // (e.g. attacker-fixed) could be reused as the post-auth session.
    session
        .cycle_id()
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;

    session
        .insert("user_id", user.id)
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;
    session
        .insert("auth_at", chrono::Utc::now())
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;

    Ok(Json(json!({ "authenticated": true })))
}
