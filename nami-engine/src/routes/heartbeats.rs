use axum::{
    Json, Router,
    extract::{Path, Request, State, rejection::PathRejection},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{CurrentUser, require_project_role},
    models::heartbeat::{self, CreateHeartbeatRequest, HeartbeatView, PatchHeartbeatRequest},
    state::AppState,
    uptime::{
        alerts,
        heartbeat::{PingOutcome, record_ping},
    },
};

// Same RBAC as monitors: viewer reads, member creates/edits (and rotates the
// ping token), admin deletes. Viewers read heartbeats without their ping token.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/heartbeats",
            get(list_heartbeats).post(create_heartbeat),
        )
        .route(
            "/projects/{id}/heartbeats/{heartbeat_id}",
            get(get_heartbeat)
                .patch(patch_heartbeat)
                .delete(delete_heartbeat),
        )
        .route(
            "/projects/{id}/heartbeats/{heartbeat_id}/rotate-token",
            post(rotate_heartbeat_token),
        )
}

/// The public ping endpoint. No session: the token in the path is the
/// secret, and cron jobs call the engine directly. Every reply says
/// `Cache-Control: no-store`: a ping is a GET with a side effect, which no
/// cache may answer or keep.
pub fn public_router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/heartbeat/{token}", get(ping).post(ping))
        .layer(middleware::from_fn(no_store))
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Checks that the caller may read the heartbeats of `project_id` and says
/// whether they may also see the ping tokens: only a member of the project or
/// above may (a global admin counts). Whoever holds a token can ping, and a
/// ping can resolve an incident or hide a dead job, so a viewer, who may only
/// read, must not get one.
async fn read_access(
    state: &AppState,
    user: &CurrentUser,
    project_id: Uuid,
) -> Result<bool, AppError> {
    match require_project_role(&state.db, user, project_id, "member").await {
        Ok(()) => Ok(true),
        Err(AppError::Forbidden) => {
            require_project_role(&state.db, user, project_id, "viewer").await?;
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

async fn list_heartbeats(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<HeartbeatView>>, AppError> {
    let with_token = read_access(&state, &user, project_id).await?;
    let heartbeats = heartbeat::list_for_project(&state.db, project_id).await?;
    Ok(Json(
        heartbeats
            .into_iter()
            .map(|heartbeat| heartbeat.into_view(with_token))
            .collect(),
    ))
}

async fn create_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateHeartbeatRequest>,
) -> Result<(StatusCode, Json<HeartbeatView>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let config = heartbeat::validate(payload.into_config()).map_err(AppError::Validation)?;
    let created = heartbeat::insert_heartbeat(&state.db, project_id, &config)
        .await
        .map_err(|e| match &e {
            // Global admins bypass membership: the project may not exist.
            sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => {
                AppError::NotFound
            }
            _ => AppError::Database(e),
        })?;
    Ok((StatusCode::CREATED, Json(created.into_view(true))))
}

async fn get_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<HeartbeatView>, AppError> {
    let with_token = read_access(&state, &user, project_id).await?;
    let found = heartbeat::find_in_project(&state.db, project_id, heartbeat_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(found.into_view(with_token)))
}

async fn patch_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchHeartbeatRequest>,
) -> Result<Json<HeartbeatView>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let updated = heartbeat::update_in_project(&state.db, project_id, heartbeat_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated.into_view(true)))
}

/// Replaces the ping token; the old one stops working at once. The reply is
/// the heartbeat with its new token, so the caller can show the new ping URL.
async fn rotate_heartbeat_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<HeartbeatView>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let rotated = heartbeat::rotate_token(&state.db, project_id, heartbeat_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(rotated.into_view(true)))
}

async fn delete_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !heartbeat::delete_in_project(&state.db, project_id, heartbeat_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}

async fn ping(
    State(state): State<AppState>,
    token: Result<Path<String>, PathRejection>,
) -> Result<Json<Value>, AppError> {
    // A path segment that is not even valid UTF-8 is as unknown a token as any
    // other: the endpoint only ever answers 200 or 404 (bar a database failure).
    let Ok(Path(token)) = token else {
        return Err(AppError::NotFound);
    };
    match record_ping(&state.db, &token).await? {
        PingOutcome::UnknownToken => Err(AppError::NotFound),
        PingOutcome::Recorded { transition } => {
            if let Some(transition) = transition {
                // A cron job is waiting on this response: deliver the
                // recovery alert in the background.
                let state = state.clone();
                tokio::spawn(async move { alerts::notify_transition(&state, &transition).await });
            }
            Ok(Json(json!({ "ok": true })))
        }
    }
}
