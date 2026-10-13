use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

pub async fn insert_project(pool: &PgPool, name: &str, slug: &str) -> Result<Project, sqlx::Error> {
    sqlx::query_as("insert into projects (name, slug) values ($1, $2) returning id, name, slug")
        .bind(name)
        .bind(slug)
        .fetch_one(pool)
        .await
}

pub async fn list_projects_for_user(pool: &PgPool, user_id: Uuid, is_global_admin: bool) -> Result<Vec<Project>, sqlx::Error> {
    if is_global_admin {
        sqlx::query_as("select id, name, slug from projects").fetch_all(pool).await
    } else {
        sqlx::query_as(
            "select p.id, p.name, p.slug from projects p
             join project_members pm on pm.project_id = p.id
             where pm.user_id = $1",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
    }
}
