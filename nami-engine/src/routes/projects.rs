use axum::{routing::post, Router, Json, extract::State};
use serde::Deserialize;
use crate::{state::AppState, error::AppError, models, middleware::rbac::{CurrentUser, require_global_role}};

// This task's routes are both bare `/projects` (no path params), so the
// `:id`-vs-`{id}` axum 0.8 path-syntax issue flagged in prior tasks doesn't
// apply here. Flagging for awareness: any future path-parameterized project
// route (e.g. `/projects/{id}`) must use the `{id}` syntax, not `:id`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", post(create_project).get(list_projects))
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
