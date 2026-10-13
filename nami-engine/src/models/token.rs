use sqlx::PgPool;
use uuid::Uuid;
use chrono::{Duration, Utc};

pub async fn insert_verification_token(
    pool: &PgPool,
    user_id: Uuid,
    token_hash: &str,
) -> Result<(), sqlx::Error> {
    let expires_at = Utc::now() + Duration::hours(24);
    sqlx::query(
        "insert into email_verification_tokens (user_id, token_hash, expires_at) values ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(token_hash)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Returns the user_id if the token is valid and unconsumed, and marks it consumed.
pub async fn consume_valid_verification_token(
    pool: &PgPool,
    token_hash: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select id, user_id from email_verification_tokens
         where token_hash = $1 and expires_at > now() and consumed_at is null",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;

    if let Some((token_id, user_id)) = row {
        sqlx::query("update email_verification_tokens set consumed_at = now() where id = $1")
            .bind(token_id)
            .execute(pool)
            .await?;
        Ok(Some(user_id))
    } else {
        Ok(None)
    }
}
