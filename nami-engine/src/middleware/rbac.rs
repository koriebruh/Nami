use axum::{extract::FromRequestParts, http::request::Parts};
use tower_sessions::Session;
use crate::{state::AppState, error::AppError, models};

pub struct CurrentUser {
    pub id: uuid::Uuid,
    pub email: String,
    pub global_role: String,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::Unauthorized)?;

        let user_id: uuid::Uuid = session
            .get("user_id")
            .await
            .map_err(|_| AppError::Unauthorized)?
            .ok_or(AppError::Unauthorized)?;

        // Same staleness check as `routes::auth::me`/`current_user_id`: a
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

        let user: models::user::User = sqlx::query_as("select * from users where id = $1")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::Unauthorized)?;

        if auth_at < user.password_changed_at {
            // `.flush()` not `.delete()`: delete only removes the server-side
            // row and leaves a stale cookie on the client until its Max-Age
            // (same reasoning as `logout` in `routes::auth`). Flush does both.
            session.flush().await.ok();
            return Err(AppError::Unauthorized);
        }

        Ok(CurrentUser { id: user.id, email: user.email, global_role: user.global_role })
    }
}

/// Shared by `require_global_role` and `require_project_role` (previously
/// duplicated verbatim in both). `None` on an unrecognized role string so
/// both callers fail closed rather than ranking an unknown role above `-1`.
fn role_rank(role: &str) -> Option<i32> {
    match role {
        "admin" => Some(2),
        "member" => Some(1),
        "viewer" => Some(0),
        _ => None,
    }
}

pub fn require_global_role(user: &CurrentUser, min: &str) -> Result<(), AppError> {
    match (role_rank(&user.global_role), role_rank(min)) {
        (Some(user_rank), Some(min_rank)) if user_rank >= min_rank => Ok(()),
        _ => Err(AppError::Forbidden),
    }
}

// Project-scoped counterpart to `require_global_role`. A global admin
// bypasses the membership check entirely (consistent with
// `list_projects_for_user`'s global-admin bypass). Otherwise the caller must
// hold a `project_members` row for this project, and its role must rank at
// or above `min`. Mirrors `require_global_role`'s fail-closed rank
// comparison (`Option<i32>`, `None` on an unrecognized role string) rather
// than the brief's original `-1`-sentinel version, which fails open when
// `min` itself is unrecognized (any known role ranks >= -1).
pub async fn require_project_role(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    project_id: uuid::Uuid,
    min: &str,
) -> Result<(), crate::error::AppError> {
    if user.global_role == "admin" {
        return Ok(());
    }

    let row: Option<(String,)> = sqlx::query_as(
        "select project_role from project_members where project_id = $1 and user_id = $2",
    )
    .bind(project_id)
    .bind(user.id)
    .fetch_optional(pool)
    .await?;

    match row {
        Some((role,)) => match (role_rank(&role), role_rank(min)) {
            (Some(user_rank), Some(min_rank)) if user_rank >= min_rank => Ok(()),
            _ => Err(crate::error::AppError::Forbidden),
        },
        None => Err(crate::error::AppError::Forbidden),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_with_role(role: &str) -> CurrentUser {
        CurrentUser {
            id: uuid::Uuid::nil(),
            email: "test@example.com".to_string(),
            global_role: role.to_string(),
        }
    }

    #[test]
    fn unrecognized_global_role_fails_closed_even_against_low_bar() {
        let user = user_with_role("superadmin");
        let result = require_global_role(&user, "viewer");
        assert!(matches!(result, Err(AppError::Forbidden)));
    }

    #[test]
    fn unrecognized_min_argument_fails_closed() {
        let user = user_with_role("admin");
        let result = require_global_role(&user, "Admin");
        assert!(matches!(result, Err(AppError::Forbidden)));
    }

    #[test]
    fn known_roles_still_rank_correctly() {
        let admin = user_with_role("admin");
        let member = user_with_role("member");
        let viewer = user_with_role("viewer");

        assert!(require_global_role(&admin, "admin").is_ok());
        assert!(require_global_role(&admin, "viewer").is_ok());
        assert!(require_global_role(&member, "admin").is_err());
        assert!(require_global_role(&member, "member").is_ok());
        assert!(require_global_role(&viewer, "member").is_err());
        assert!(require_global_role(&viewer, "viewer").is_ok());
    }
}
