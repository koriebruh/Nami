use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{CurrentUser, require_project_role},
    models::source::{self, CreateSourceRequest, PatchSourceRequest, Source, SourceWithToken},
    state::AppState,
};

// Viewers read sources but never their tokens; members create, edit, reveal
// and rotate; admins delete.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/sources",
            get(list_sources).post(create_source),
        )
        .route(
            "/projects/{id}/sources/{source_id}",
            get(get_source).patch(patch_source).delete(delete_source),
        )
        .route(
            "/projects/{id}/sources/{source_id}/token",
            get(reveal_token),
        )
        .route(
            "/projects/{id}/sources/{source_id}/rotate",
            post(rotate_token),
        )
}

async fn list_sources(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<Source>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    Ok(Json(source::list_for_project(&state.db, project_id).await?))
}

async fn create_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateSourceRequest>,
) -> Result<(StatusCode, Json<SourceWithToken>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let config = source::validate(payload.into_config()).map_err(AppError::Validation)?;
    let created = source::insert_source(&state.db, &state.cipher, project_id, &config)
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

async fn get_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Source>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let found = source::find_in_project(&state.db, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(found))
}

async fn patch_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchSourceRequest>,
) -> Result<Json<Source>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let updated = source::update_in_project(&state.db, project_id, source_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated))
}

async fn delete_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !source::delete_in_project(&state.db, project_id, source_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}

async fn reveal_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let token = source::reveal_token(&state.db, &state.cipher, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(json!({ "token": token })))
}

async fn rotate_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<SourceWithToken>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let rotated = source::rotate_token(&state.db, &state.cipher, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(rotated))
}
