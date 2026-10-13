use axum::{routing::post, Router, Json, extract::{State, Path}};
use serde::Deserialize;
use crate::{state::AppState, error::AppError, models, middleware::rbac::{CurrentUser, require_global_role, require_project_role}};

// axum 0.8 requires `{param}` path syntax, not the brief's pre-0.8 `:param`
// syntax (panics at router-build time otherwise) — same issue as Task 10's
// OAuth routes (`src/routes/auth.rs`).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", post(create_project).get(list_projects))
        .route("/projects/{id}", axum::routing::get(get_project))
        .route("/projects/{id}/members", post(add_member))
        .route("/projects/{id}/members/{user_id}", axum::routing::delete(delete_member))
}

#[derive(Deserialize)]
struct CreateProjectRequest {
    name: String,
    slug: String,
}

async fn create_project(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(payload): Json<CreateProjectRequest>,
) -> Result<Json<models::project::Project>, AppError> {
    require_global_role(&user, "admin")?;
    let project = models::project::insert_project(&state.db, &payload.name, &payload.slug).await?;
    Ok(Json(project))
}

async fn list_projects(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<models::project::Project>>, AppError> {
    let projects = models::project::list_projects_for_user(&state.db, user.id, user.global_role == "admin").await?;
    Ok(Json(projects))
}

async fn get_project(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<models::project::Project>, AppError> {
    require_project_role(&state.db, &user, id, "viewer").await?;
    let project = models::project::find_by_id(&state.db, id).await?.ok_or(AppError::NotFound)?;
    Ok(Json(project))
}

#[derive(Deserialize)]
struct AddMemberRequest {
    user_id: uuid::Uuid,
    project_role: String,
}

async fn add_member(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<uuid::Uuid>,
    Json(payload): Json<AddMemberRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_project_role(&state.db, &user, id, "admin").await?;
    models::project::upsert_member(&state.db, id, payload.user_id, &payload.project_role).await?;
    Ok(Json(serde_json::json!({ "added": true })))
}

async fn delete_member(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, member_id)): Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_project_role(&state.db, &user, id, "admin").await?;
    models::project::remove_member(&state.db, id, member_id).await?;
    Ok(Json(serde_json::json!({ "removed": true })))
}
