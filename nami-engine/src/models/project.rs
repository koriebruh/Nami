use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

/// Creates a project AND seeds its creator as a `project_members` row at
/// `admin`, atomically (a single `pool.begin()` transaction, mirroring
/// `find_or_create_oauth_user`'s multi-statement-atomicity pattern in
/// `models/user.rs`). Without this, a freshly created project has zero
/// members — not even its creator — and since only a global admin (not a
/// project admin) could previously add the first member, a global admin who
/// isn't also made a member here would be locked out of managing their own
/// project's membership afterward.
pub async fn insert_project(
    pool: &PgPool,
    name: &str,
    slug: &str,
    created_by: Uuid,
) -> Result<Project, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let project: Project = sqlx::query_as(
        "insert into projects (name, slug, created_by) values ($1, $2, $3) returning id, name, slug",
    )
    .bind(name)
    .bind(slug)
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "insert into project_members (project_id, user_id, project_role) values ($1, $2, 'admin')",
    )
    .bind(project.id)
    .bind(created_by)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(project)
}

pub async fn list_projects_for_user(
    pool: &PgPool,
    user_id: Uuid,
    is_global_admin: bool,
) -> Result<Vec<Project>, sqlx::Error> {
    if is_global_admin {
        sqlx::query_as("select id, name, slug from projects")
            .fetch_all(pool)
            .await
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

pub async fn upsert_member(
    pool: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
    role: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into project_members (project_id, user_id, project_role) values ($1, $2, $3)
         on conflict (project_id, user_id) do update set project_role = excluded.project_role",
    )
    .bind(project_id)
    .bind(user_id)
    .bind(role)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn remove_member(
    pool: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from project_members where project_id = $1 and user_id = $2")
        .bind(project_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Project>, sqlx::Error> {
    sqlx::query_as("select id, name, slug from projects where id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}
