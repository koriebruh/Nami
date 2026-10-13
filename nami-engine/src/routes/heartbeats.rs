use axum::{
    Json, Router,
    extract::{Path, State, rejection::PathRejection},
    http::StatusCode,
    routing::get,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{CurrentUser, require_project_role},
    models::heartbeat::{self, CreateHeartbeatRequest, Heartbeat, PatchHeartbeatRequest},
    state::AppState,
    uptime::{
        alerts,
        heartbeat::{PingOutcome, record_ping},
    },
};

// Same RBAC as monitors: viewer reads, member creates/edits, admin deletes.
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
}

/// The public ping endpoint. No session: the token in the path is the
/// secret, and cron jobs call the engine directly.
pub fn public_router() -> Router<AppState> {
    Router::new().route("/api/v1/heartbeat/{token}", get(ping).post(ping))
}

async fn list_heartbeats(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<Heartbeat>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    Ok(Json(
        heartbeat::list_for_project(&state.db, project_id).await?,
    ))
}

async fn create_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateHeartbeatRequest>,
) -> Result<(StatusCode, Json<Heartbeat>), AppError> {
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
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Heartbeat>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let found = heartbeat::find_in_project(&state.db, project_id, heartbeat_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(found))
}

async fn patch_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchHeartbeatRequest>,
) -> Result<Json<Heartbeat>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let updated = heartbeat::update_in_project(&state.db, project_id, heartbeat_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated))
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
