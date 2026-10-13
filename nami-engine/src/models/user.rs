use sqlx::PgPool;
use uuid::Uuid;
use chrono::{DateTime, Utc};
use crate::error::AppError;

#[derive(Debug, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub password_hash: Option<String>,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub global_role: String,
    pub totp_secret: Option<String>,
    pub totp_enabled: bool,
    pub password_changed_at: DateTime<Utc>,
}

pub async fn find_by_email(pool: &PgPool, email: &str) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>("select * from users where email = $1")
        .bind(email)
        .fetch_optional(pool)
        .await
}

pub async fn insert_user(
    pool: &PgPool,
    email: &str,
    password_hash: &str,
) -> Result<User, sqlx::Error> {
    // First-registered-user-becomes-admin bootstrap: this project is
    // explicitly single-org (no multi-tenancy), so there's otherwise no way
    // to ever create the first project (`POST /projects` requires global
    // `admin`, and `global_role` defaults to `member`). A count-then-insert
    // (not a single atomic statement) is an accepted, extremely
    // low-probability race for this one-time bootstrap event only — not
    // worth wrapping in a transaction for this fix.
    let is_first_user = sqlx::query_scalar::<_, i64>("select count(*) from users")
        .fetch_one(pool)
        .await?
        == 0;
    let global_role = if is_first_user { "admin" } else { "member" };

    sqlx::query_as::<_, User>(
        "insert into users (email, password_hash, global_role) values ($1, $2, $3) returning *",
    )
    .bind(email)
    .bind(password_hash)
    .bind(global_role)
    .fetch_one(pool)
    .await
}

pub async fn mark_email_verified(pool: &PgPool, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("update users set email_verified_at = now() where id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_password(pool: &PgPool, user_id: Uuid, new_hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update users set password_hash = $1, password_changed_at = now() where id = $2",
    )
    .bind(new_hash)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn find_or_create_oauth_user(
    pool: &PgPool,
    provider: &str,
    provider_user_id: &str,
    email: &str,
) -> Result<User, AppError> {
    if let Some((user_id,)) = sqlx::query_as::<_, (Uuid,)>(
        "select user_id from oauth_identities where provider = $1 and provider_user_id = $2",
    )
    .bind(provider)
    .bind(provider_user_id)
    .fetch_optional(pool)
    .await?
    {
        return Ok(sqlx::query_as::<_, User>("select * from users where id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await?);
    }

    let mut tx = pool.begin().await?;

    // Same first-registered-user-becomes-admin bootstrap as `insert_user`
    // (the password-registration path). Without it, a deployment whose very
    // first user signs in via OAuth instead of email/password ends up with no
    // global admin at all, so `POST /projects` is 403 for everyone forever
    // with no in-app recovery. Counted inside this transaction, so it can't
    // observe a half-written user row from a concurrent OAuth signup.
    let is_first_user = sqlx::query_scalar::<_, i64>("select count(*) from users")
        .fetch_one(&mut *tx)
        .await?
        == 0;
    let global_role = if is_first_user { "admin" } else { "member" };

    let user: User = sqlx::query_as(
        "insert into users (email, email_verified_at, global_role) values ($1, now(), $2) returning *",
    )
    .bind(email)
    .bind(global_role)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        // A pre-existing password-based account with this email hits the
        // `users.email` unique constraint. That's an expected conflict, not
        // a server bug — surface it as 409 instead of letting it bubble up
        // as an opaque 500 via `AppError::Database`.
        if e.as_database_error()
            .is_some_and(|db_err| db_err.is_unique_violation())
        {
            AppError::Conflict(
                "an account with this email already exists — sign in with your password instead"
                    .into(),
            )
        } else {
            AppError::Database(e)
        }
    })?;

    sqlx::query(
        "insert into oauth_identities (user_id, provider, provider_user_id) values ($1, $2, $3)",
    )
    .bind(user.id)
    .bind(provider)
    .bind(provider_user_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(user)
}
