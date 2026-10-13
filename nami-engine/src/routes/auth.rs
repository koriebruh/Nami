use axum::{routing::{get, post}, Router, Json, extract::{State, Query, Path}};
use oauth2::{CsrfToken, Scope, AuthorizationCode, TokenResponse};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_sessions::Session;
use crate::{state::AppState, error::AppError, auth, models};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/verify-email", get(verify_email))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
        .route("/auth/totp/setup", post(totp_setup))
        .route("/auth/totp/enable", post(totp_enable))
        // axum 0.8 uses `{param}` path syntax (not the brief's `:param`, which is
        // the pre-0.8 axum/matchit syntax and panics at router-build time here).
        .route("/auth/oauth/{provider}/start", get(oauth_start))
        .route("/auth/oauth/{provider}/callback", get(oauth_callback))
}

// Routes behind the per-IP rate limiter (`SmartIpKeyExtractor`-based governor
// in `main.rs`/`router::build_router`). `/auth/totp/verify` guards a 6-digit
// code held by someone who already has a stolen password — exactly the case
// 2FA exists to stop a brute force on — and `/auth/password/forgot`/`/reset`
// can otherwise be used to mail-bomb an address or brute-force a reset token
// unlimited. Per-IP rate limiting is the full scope of this fix; a
// persistent per-user TOTP attempt-lockout system is a larger, separate
// feature left out of scope here.
pub fn rate_limited_router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/totp/verify", post(totp_verify))
        .route("/auth/password/forgot", post(forgot_password))
        .route("/auth/password/reset", post(reset_password))
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

    // Don't propagate a mailer error with `?` here: that would turn into a
    // 400 response, distinguishable from the 200 generic response below for
    // an already-registered email — a status-code oracle for account
    // enumeration. Log and still return the generic success response.
    if let Err(e) = state
        .mailer
        .send(
            &user.email,
            "Verify your Nami account",
            format!("Verify your email: /auth/verify-email?token={raw_token}"),
        )
        .await
    {
        tracing::error!(error = %e, "failed to send verification email");
    }

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

    // Checked only after the password is confirmed correct, so this doesn't
    // become a new enumeration oracle (an attacker without the right
    // password still just gets the generic 401 above). The spec requires
    // login to stay blocked until the email is verified; this is the one
    // check every test in the repo seeded around (every fixture user sets
    // `email_verified_at = now()`), so it was never previously enforced here.
    if user.email_verified_at.is_none() {
        return Err(AppError::Validation(
            "please verify your email before logging in".into(),
        ));
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

    if auth_at < user.password_changed_at {
        // `.flush()` not `.delete()`: delete only removes the server-side row
        // and leaves a stale cookie on the client until its Max-Age expires.
        session.flush().await.ok();
        return Err(AppError::Unauthorized);
    }

    Ok(Json(json!({ "id": user.id, "email": user.email, "global_role": user.global_role })))
}

/// Same staleness check as `me`: a session authenticated before the user's
/// last password reset must not be usable, here either — otherwise a reset
/// doesn't actually invalidate old sessions for endpoints that only call
/// this helper instead of `me` (e.g. TOTP setup/enable).
async fn current_user_id(session: &Session, pool: &sqlx::PgPool) -> Result<uuid::Uuid, AppError> {
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
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::Unauthorized)?;

    if auth_at < user.password_changed_at {
        // `.flush()` not `.delete()`: same reasoning as `me` above.
        session.flush().await.ok();
        return Err(AppError::Unauthorized);
    }

    Ok(user_id)
}

async fn totp_setup(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    let user_id = current_user_id(&session, &state.db).await?;
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
    let user_id = current_user_id(&session, &state.db).await?;
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

#[derive(Deserialize)]
struct ForgotPasswordRequest {
    email: String,
}

async fn forgot_password(
    State(state): State<AppState>,
    Json(payload): Json<ForgotPasswordRequest>,
) -> Result<Json<Value>, AppError> {
    let generic_response = Json(json!({ "message": "if this email exists, a reset link was sent" }));

    let Some(user) = models::user::find_by_email(&state.db, &payload.email).await? else {
        return Ok(generic_response);
    };

    let (raw_token, token_hash) = auth::token::new_opaque_token();
    models::token::insert_reset_token(&state.db, user.id, &token_hash).await?;

    // Same anti-enumeration reasoning as `register`: don't let a mailer
    // failure propagate into a distinguishable error status.
    if let Err(e) = state
        .mailer
        .send(&user.email, "Reset your Nami password", format!("Reset: /auth/password/reset?token={raw_token}"))
        .await
    {
        tracing::error!(error = %e, "failed to send password reset email");
    }

    Ok(generic_response)
}

#[derive(Deserialize)]
struct ResetPasswordRequest {
    token: String,
    new_password: String,
}

async fn reset_password(
    State(state): State<AppState>,
    Json(payload): Json<ResetPasswordRequest>,
) -> Result<Json<Value>, AppError> {
    let token_hash = auth::token::hash_token(&payload.token);
    let user_id = models::token::consume_valid_reset_token(&state.db, &token_hash)
        .await?
        .ok_or_else(|| AppError::Validation("invalid or expired token".into()))?;

    let new_hash = auth::password::hash_password(&payload.new_password)?;
    models::user::update_password(&state.db, user_id, &new_hash).await?;

    Ok(Json(json!({ "message": "password reset" })))
}

async fn oauth_start(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    session: Session,
) -> Result<axum::response::Redirect, AppError> {
    let provider = auth::oauth::Provider::parse(&provider)
        .ok_or_else(|| AppError::Validation("unknown provider".into()))?;
    let client = provider.client(&state.config);

    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("email".to_string()))
        .url();

    session
        .insert("oauth_csrf", csrf_token.secret().clone())
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;

    Ok(axum::response::Redirect::to(auth_url.as_str()))
}

#[derive(Deserialize)]
struct OAuthCallbackQuery {
    code: String,
    state: String,
}

// Google's OIDC userinfo endpoint returns the subject identifier as `sub`
// (there is no `id` field).
#[derive(serde::Deserialize)]
struct GoogleUserInfo {
    sub: String,
    email: String,
}

// GitHub's `GET /user` returns `id` as a JSON integer, not a string, and
// `email` is nullable (many users don't have a public email set).
#[derive(serde::Deserialize)]
struct GitHubUserInfo {
    id: u64,
    email: Option<String>,
}

async fn oauth_callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<OAuthCallbackQuery>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    let provider = auth::oauth::Provider::parse(&provider)
        .ok_or_else(|| AppError::Validation("unknown provider".into()))?;

    let expected_csrf: String = session
        .get("oauth_csrf")
        .await
        .map_err(|_| AppError::Validation("session error".into()))?
        .ok_or_else(|| AppError::Validation("missing csrf state".into()))?;
    if expected_csrf != query.state {
        return Err(AppError::Validation("csrf mismatch".into()));
    }

    // Remove the stored CSRF value as soon as it's validated (matched),
    // regardless of what happens afterward in this function (token exchange
    // or userinfo fetch failing, etc). Previously this was only removed on
    // the full success path at the end, so a failed token exchange left the
    // CSRF token in the session indefinitely, letting it be reused if a
    // later `/token` call succeeded.
    session.remove::<String>("oauth_csrf").await.ok();

    let client = provider.client(&state.config);
    // oauth2's `reqwest` feature vendors its own reqwest dependency (0.12.x), a
    // different major version from this crate's own `reqwest` (0.13.x, used below
    // for the userinfo call) — the two `Client` types are distinct, so the token
    // exchange specifically needs `oauth2::reqwest::Client`, which is the only
    // type implementing oauth2's `AsyncHttpClient` trait.
    let oauth_http_client = oauth2::reqwest::Client::new();
    let token = client
        .exchange_code(AuthorizationCode::new(query.code))
        .request_async(&oauth_http_client)
        .await
        .map_err(|_| AppError::Validation("token exchange failed".into()))?;

    let userinfo_response = reqwest::Client::new()
        .get(provider.userinfo_url(&state.config))
        .bearer_auth(token.access_token().secret())
        .send()
        .await
        .map_err(|_| AppError::Validation("userinfo request failed".into()))?;

    // Each provider's userinfo endpoint returns a different shape; parse
    // with the matching struct and normalize to a (provider_user_id, email)
    // pair before calling `find_or_create_oauth_user`.
    let (provider_user_id, email) = match provider {
        auth::oauth::Provider::Google => {
            let info: GoogleUserInfo = userinfo_response
                .json()
                .await
                .map_err(|_| AppError::Validation("invalid userinfo response".into()))?;
            (info.sub, info.email)
        }
        auth::oauth::Provider::Github => {
            let info: GitHubUserInfo = userinfo_response
                .json()
                .await
                .map_err(|_| AppError::Validation("invalid userinfo response".into()))?;
            let email = info.email.ok_or_else(|| {
                AppError::Validation(
                    "GitHub account has no public email; please make an email public on GitHub or register with email/password instead"
                        .into(),
                )
            })?;
            (info.id.to_string(), email)
        }
    };

    let user = models::user::find_or_create_oauth_user(
        &state.db,
        provider.as_str(),
        &provider_user_id,
        &email,
    )
    .await?;

    // Rotate the session ID before promoting to a fully authenticated session,
    // mirroring `login`'s and `totp_verify`'s success paths. Without this, a
    // session ID that existed before the OAuth round-trip (e.g. attacker-fixed)
    // could be reused as the post-auth session.
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
