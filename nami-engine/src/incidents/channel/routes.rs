//! Project channels (viewers list, admins manage) and personal channels (any
//! logged-in user, own channels only; another user's id is a 404).

use std::fmt;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{CurrentUser, Role, require_project_role},
    core::state::AppState,
    core::validate::validate_name,
    incidents::channel::model::{
        self as channel, Channel, ChannelConfig, ChannelPatch, CreatedChannel, Deleted, Inserted,
        KINDS, MAX_NAME_CHARS, MAX_PERSONAL_CHANNELS, MAX_PROJECT_CHANNELS, Owner,
    },
    incidents::send,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateChannelRequest {
    pub name: String,
    pub kind: String,
    pub config: Value,
    pub enabled: Option<bool>,
}

impl fmt::Debug for CreateChannelRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateChannelRequest")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// `kind` and `config` come together: a config replaces the stored one whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchChannelRequest {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub kind: Option<String>,
    pub config: Option<Value>,
}

impl fmt::Debug for PatchChannelRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PatchChannelRequest")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/notification-channels",
            get(list_project).post(create_project),
        )
        .route(
            "/projects/{id}/notification-channels/{channel_id}",
            patch(patch_project).delete(delete_project),
        )
        .route(
            "/account/notification-channels",
            get(list_personal).post(create_personal),
        )
        .route(
            "/account/notification-channels/{channel_id}",
            patch(patch_personal).delete(delete_personal),
        )
        .route(
            "/projects/{id}/notification-channels/{channel_id}/test",
            post(test_project),
        )
        .route(
            "/account/notification-channels/{channel_id}/test",
            post(test_personal),
        )
}

fn checked_name(raw: &str) -> Result<String, AppError> {
    let name = validate_name(raw).map_err(AppError::Validation)?;
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(AppError::Validation(format!(
            "name must be at most {MAX_NAME_CHARS} characters"
        )));
    }
    Ok(name)
}

/// The config of a `kind` channel, validated for `owner` (R8, R9).
fn checked_config(owner: &Owner, kind: &str, config: Value) -> Result<ChannelConfig, AppError> {
    if !KINDS.contains(&kind) {
        return Err(AppError::Validation("unknown channel kind".into()));
    }
    // The serde error is not returned: it can quote the submitted value.
    let config: ChannelConfig = serde_json::from_value(json!({ "kind": kind, "config": config }))
        .map_err(|_| AppError::Validation(format!("invalid config for a {kind} channel")))?;
    config.validate(owner).map_err(AppError::Validation)?;
    Ok(config)
}

async fn list(state: &AppState, owner: Owner) -> Result<Json<Vec<Channel>>, AppError> {
    Ok(Json(channel::list(&state.db, &owner).await?))
}

async fn create(
    state: &AppState,
    user: &CurrentUser,
    owner: Owner,
    payload: CreateChannelRequest,
) -> Result<(StatusCode, Json<CreatedChannel>), AppError> {
    let name = checked_name(&payload.name)?;
    let config = checked_config(&owner, &payload.kind, payload.config)?;
    let enabled = payload.enabled.unwrap_or(true);
    match channel::insert(&state.db, &state.cipher, owner, &name, &config, enabled, user.id).await? {
        Inserted::Created(created) => Ok((StatusCode::CREATED, Json(created))),
        Inserted::Full => Err(AppError::Conflict(match owner {
            Owner::Project(_) => format!("channel limit reached ({MAX_PROJECT_CHANNELS} per project)"),
            Owner::User(_) => format!("channel limit reached ({MAX_PERSONAL_CHANNELS} per user)"),
        })),
        Inserted::NoOwner => Err(AppError::NotFound),
    }
}

async fn update(
    state: &AppState,
    owner: Owner,
    id: Uuid,
    payload: PatchChannelRequest,
) -> Result<Json<Channel>, AppError> {
    let name = payload.name.as_deref().map(checked_name).transpose()?;
    let config = match (payload.kind, payload.config) {
        (Some(kind), Some(config)) => Some(checked_config(&owner, &kind, config)?),
        (None, None) => None,
        _ => {
            return Err(AppError::Validation("kind and config must be sent together".into()));
        }
    };
    let patch = ChannelPatch { name, enabled: payload.enabled, config };
    channel::update(&state.db, &state.cipher, &owner, id, patch)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
}

async fn remove(state: &AppState, owner: Owner, id: Uuid) -> Result<StatusCode, AppError> {
    match channel::delete(&state.db, &owner, id).await? {
        Deleted::Deleted => Ok(StatusCode::NO_CONTENT),
        Deleted::NotFound => Err(AppError::NotFound),
        Deleted::InUse { policy } => Err(AppError::Conflict(format!(
            "channel is used by escalation policy \"{policy}\""
        ))),
    }
}

/// "Send test": 200 `{"ok": true}`, 502 `{"error": "<what failed>"}`, or 429
/// within `MIN_TEST_INTERVAL` of the last test. Disabled channels too.
async fn test(state: &AppState, owner: Owner, id: Uuid) -> Result<(StatusCode, Json<Value>), AppError> {
    match channel::take_test_slot(&state.db, &owner, id).await? {
        None => return Err(AppError::NotFound),
        Some(false) => {
            return Ok((
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({ "error": "wait a few seconds before testing again" })),
            ));
        }
        Some(true) => {}
    }
    Ok(match send::send_test(state, id).await? {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": e.message }))),
    })
}

async fn list_project(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<Channel>>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    list(&state, Owner::Project(project_id)).await
}

async fn create_project(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateChannelRequest>,
) -> Result<(StatusCode, Json<CreatedChannel>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    create(&state, &user, Owner::Project(project_id), payload).await
}

async fn patch_project(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchChannelRequest>,
) -> Result<Json<Channel>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    update(&state, Owner::Project(project_id), id, payload).await
}

async fn delete_project(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    remove(&state, Owner::Project(project_id), id).await
}

async fn test_project(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    test(&state, Owner::Project(project_id), id).await
}

async fn list_personal(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<Channel>>, AppError> {
    list(&state, Owner::User(user.id)).await
}

async fn create_personal(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(payload): Json<CreateChannelRequest>,
) -> Result<(StatusCode, Json<CreatedChannel>), AppError> {
    create(&state, &user, Owner::User(user.id), payload).await
}

async fn patch_personal(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(payload): Json<PatchChannelRequest>,
) -> Result<Json<Channel>, AppError> {
    update(&state, Owner::User(user.id), id, payload).await
}

async fn delete_personal(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    remove(&state, Owner::User(user.id), id).await
}

async fn test_personal(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    test(&state, Owner::User(user.id), id).await
}
