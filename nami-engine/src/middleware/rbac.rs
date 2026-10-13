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
            session.delete().await.ok();
            return Err(AppError::Unauthorized);
        }

        Ok(CurrentUser { id: user.id, email: user.email, global_role: user.global_role })
    }
}

pub fn require_global_role(user: &CurrentUser, min: &str) -> Result<(), AppError> {
    let rank = |role: &str| match role {
        "admin" => Some(2),
        "member" => Some(1),
        "viewer" => Some(0),
        _ => None,
    };

    match (rank(&user.global_role), rank(min)) {
        (Some(user_rank), Some(min_rank)) if user_rank >= min_rank => Ok(()),
        _ => Err(AppError::Forbidden),
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
