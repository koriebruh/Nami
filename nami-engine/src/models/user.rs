use sqlx::PgPool;
use uuid::Uuid;
use chrono::{DateTime, Utc};

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
    sqlx::query_as::<_, User>(
        "insert into users (email, password_hash) values ($1, $2) returning *",
    )
    .bind(email)
    .bind(password_hash)
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
) -> Result<User, sqlx::Error> {
    if let Some((user_id,)) = sqlx::query_as::<_, (Uuid,)>(
        "select user_id from oauth_identities where provider = $1 and provider_user_id = $2",
    )
    .bind(provider)
    .bind(provider_user_id)
    .fetch_optional(pool)
    .await?
    {
        return sqlx::query_as::<_, User>("select * from users where id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await;
    }

    let mut tx = pool.begin().await?;
    let user: User = sqlx::query_as(
        "insert into users (email, email_verified_at) values ($1, now()) returning *",
    )
    .bind(email)
    .fetch_one(&mut *tx)
    .await?;

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
