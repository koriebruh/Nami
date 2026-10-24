use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{CurrentUser, Role, require_project_role},
    core::state::AppState,
    incidents::escalation::model::{
        self as policy, Deleted, MAX_POLICIES_PER_PROJECT, PolicyInput, PolicyView, Saved,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/escalation-policies",
            get(list_policies).post(create_policy),
        )
        .route(
            "/projects/{id}/escalation-policies/{policy_id}",
            get(get_policy).put(replace_policy).delete(delete_policy),
        )
        .route(
            "/projects/{id}/escalation-policies/{policy_id}/default",
            post(make_default),
        )
}

fn saved(result: Saved) -> Result<Json<PolicyView>, AppError> {
    match result {
        Saved::Saved(view) => Ok(Json(view)),
        Saved::NotFound => Err(AppError::NotFound),
        Saved::Full => Err(AppError::Conflict(format!(
            "policy limit reached ({MAX_POLICIES_PER_PROJECT} per project)"
        ))),
        Saved::NameTaken => Err(AppError::Conflict(
            "a policy with this name already exists".into(),
        )),
        Saved::Invalid(message) => Err(AppError::Validation(message)),
    }
}

async fn list_policies(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<PolicyView>>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    Ok(Json(policy::list(&state.db, project_id).await?))
}

async fn get_policy(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<PolicyView>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    policy::get(&state.db, project_id, id).await?.map(Json).ok_or(AppError::NotFound)
}

async fn create_policy(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(input): Json<PolicyInput>,
) -> Result<(StatusCode, Json<PolicyView>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    policy::validate(&input).map_err(AppError::Validation)?;
    let view = saved(policy::create(&state.db, project_id, &input).await?)?;
    Ok((StatusCode::CREATED, view))
}

async fn replace_policy(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
    Json(input): Json<PolicyInput>,
) -> Result<Json<PolicyView>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    policy::validate(&input).map_err(AppError::Validation)?;
    saved(policy::replace(&state.db, project_id, id, &input).await?)
}

async fn make_default(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<PolicyView>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    if !policy::make_default(&state.db, project_id, id).await? {
        return Err(AppError::NotFound);
    }
    policy::get(&state.db, project_id, id).await?.map(Json).ok_or(AppError::NotFound)
}

async fn delete_policy(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    match policy::delete(&state.db, project_id, id).await? {
        Deleted::Deleted => Ok(Json(json!({ "deleted": true }))),
        Deleted::NotFound => Err(AppError::NotFound),
        Deleted::IsDefault => Err(AppError::Conflict(
            "the default policy cannot be deleted; make another policy the default first".into(),
        )),
    }
}
