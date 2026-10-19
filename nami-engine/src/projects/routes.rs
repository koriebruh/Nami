use crate::{
    core::error::AppError,
    core::middleware::rbac::{CurrentUser, require_global_role, require_project_role},
    core::state::AppState,
    projects::model as project,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::post,
};
use serde::Deserialize;

// axum 0.8 requires `{param}` path syntax, not the brief's pre-0.8 `:param`
// syntax (panics at router-build time otherwise) — same issue as Task 10's
// OAuth routes (`src/auth/routes.rs`).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", post(create_project).get(list_projects))
        .route("/projects/{id}", axum::routing::get(get_project))
        .route("/projects/{id}/members", post(add_member))
        .route(
            "/projects/{id}/members/{user_id}",
            axum::routing::delete(delete_member),
        )
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
) -> Result<Json<project::Project>, AppError> {
    require_global_role(&user, "admin")?;
    let project = project::insert_project(&state.db, &payload.name, &payload.slug, user.id).await?;
    Ok(Json(project))
}

async fn list_projects(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<project::Project>>, AppError> {
    let projects =
        project::list_projects_for_user(&state.db, user.id, user.global_role == "admin").await?;
    Ok(Json(projects))
}

async fn get_project(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<project::Project>, AppError> {
    require_project_role(&state.db, &user, id, "viewer").await?;
    let project = project::find_by_id(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
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

    if !matches!(payload.project_role.as_str(), "admin" | "member" | "viewer") {
        return Err(AppError::Validation("invalid project_role".into()));
    }

    project::upsert_member(&state.db, id, payload.user_id, &payload.project_role)
        .await
        .map_err(|e| match &e {
            // Postgres SQLSTATE 23503 = foreign_key_violation: project_id or
            // user_id doesn't exist. The DB correctly refuses the write; map
            // it to 404 instead of letting it surface as a raw 500.
            sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => {
                AppError::NotFound
            }
            _ => AppError::Database(e),
        })?;
    Ok(Json(serde_json::json!({ "added": true })))
}

async fn delete_member(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, member_id)): Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_project_role(&state.db, &user, id, "admin").await?;
    project::remove_member(&state.db, id, member_id).await?;
    Ok(Json(serde_json::json!({ "removed": true })))
}
