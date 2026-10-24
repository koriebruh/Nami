use crate::{auth::user, core::error::AppError, core::state::AppState};
use axum::{extract::FromRequestParts, http::request::Parts};
use tower_sessions::Session;

/// The three roles, used both as `users.global_role` and
/// `project_members.project_role`. Ordered by rank: `Viewer < Member < Admin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Member,
    Admin,
}

impl Role {
    /// The role stored as `text` in Postgres, or `None` for any other string,
    /// so a caller fails closed.
    pub fn parse(value: &str) -> Option<Role> {
        match value {
            "admin" => Some(Role::Admin),
            "member" => Some(Role::Member),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Member => "member",
            Role::Viewer => "viewer",
        }
    }
}

pub struct CurrentUser {
    pub id: uuid::Uuid,
    pub email: String,
    pub global_role: Role,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::Unauthorized)?;

        user_from_session(state, &session).await
    }
}

/// The user `session` belongs to, or `Unauthorized`. Shared by the
/// `CurrentUser` extractor and long-lived responses (the log live tail)
/// that re-check their session while they stream.
pub async fn user_from_session(
    state: &AppState,
    session: &Session,
) -> Result<CurrentUser, AppError> {
    let user_id: uuid::Uuid = session
        .get("user_id")
        .await
        .map_err(|_| AppError::Unauthorized)?
        .ok_or(AppError::Unauthorized)?;

    // Same staleness check as `auth::routes::me`/`current_user_id`: a
    // session authenticated before the user's last password change must
    // not be usable to identify the current user here either — this
    // extractor is the shared mechanism every RBAC-guarded route uses,
    // so without this check a session invalidated by a password reset
    // would still work for every route except `/auth/me`.
    let auth_at: chrono::DateTime<chrono::Utc> = session
        .get("auth_at")
        .await
        .map_err(|_| AppError::Unauthorized)?
        .ok_or(AppError::Unauthorized)?;

    let user: user::User = sqlx::query_as("select * from users where id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)?;

    if auth_at < user.password_changed_at {
        // `.flush()` not `.delete()`: delete only removes the server-side
        // row and leaves a stale cookie on the client until its Max-Age
        // (same reasoning as `logout` in `auth::routes`). Flush does both.
        session.flush().await.ok();
        return Err(AppError::Unauthorized);
    }

    // The column's check constraint allows only the three roles; anything
    // else fails closed.
    let global_role = Role::parse(&user.global_role).ok_or(AppError::Unauthorized)?;
    Ok(CurrentUser {
        id: user.id,
        email: user.email,
        global_role,
    })
}

pub fn require_global_role(user: &CurrentUser, min: Role) -> Result<(), AppError> {
    if user.global_role >= min {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

/// Project-scoped counterpart to `require_global_role`. A global admin
/// bypasses the membership check (as `list_projects_for_user` does).
/// Otherwise the caller needs a `project_members` row for this project whose
/// role ranks at or above `min`; an unrecognized stored role fails closed.
pub async fn require_project_role(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    project_id: uuid::Uuid,
    min: Role,
) -> Result<(), AppError> {
    if user.global_role == Role::Admin {
        return Ok(());
    }

    let row: Option<(String,)> = sqlx::query_as(
        "select project_role from project_members where project_id = $1 and user_id = $2",
    )
    .bind(project_id)
    .bind(user.id)
    .fetch_optional(pool)
    .await?;

    match row.and_then(|(role,)| Role::parse(&role)) {
        Some(role) if role >= min => Ok(()),
        _ => Err(AppError::Forbidden),
    }
}

/// What a caller who may read a project may see of its secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectReader {
    /// A member of the project or above (a global admin counts): sees
    /// everything, ping tokens and monitor URLs included.
    Member,
    /// A viewer: reads, without the secrets (no ping tokens, monitor URLs with
    /// their credentials and query values masked).
    Viewer,
}

/// Checks that the caller may read `project_id` (viewer or above; 403
/// otherwise) and says which kind of reader they are.
pub async fn require_project_reader(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    project_id: uuid::Uuid,
) -> Result<ProjectReader, AppError> {
    match require_project_role(pool, user, project_id, Role::Member).await {
        Ok(()) => Ok(ProjectReader::Member),
        Err(AppError::Forbidden) => {
            require_project_role(pool, user, project_id, Role::Viewer).await?;
            Ok(ProjectReader::Viewer)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_with_role(role: Role) -> CurrentUser {
        CurrentUser {
            id: uuid::Uuid::nil(),
            email: "test@example.com".to_string(),
            global_role: role,
        }
    }

    #[test]
    fn an_unrecognized_stored_role_does_not_parse() {
        assert_eq!(Role::parse("superadmin"), None);
        assert_eq!(Role::parse("Admin"), None);
    }

    #[test]
    fn roles_round_trip_through_their_stored_text() {
        for role in [Role::Viewer, Role::Member, Role::Admin] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
    }

    #[test]
    fn known_roles_rank_correctly() {
        let admin = user_with_role(Role::Admin);
        let member = user_with_role(Role::Member);
        let viewer = user_with_role(Role::Viewer);

        assert!(require_global_role(&admin, Role::Admin).is_ok());
        assert!(require_global_role(&admin, Role::Viewer).is_ok());
        assert!(require_global_role(&member, Role::Admin).is_err());
        assert!(require_global_role(&member, Role::Member).is_ok());
        assert!(require_global_role(&viewer, Role::Member).is_err());
        assert!(require_global_role(&viewer, Role::Viewer).is_ok());
    }
}
