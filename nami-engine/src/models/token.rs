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
///
/// Uses a single atomic `UPDATE ... RETURNING` (instead of a SELECT followed by
/// a separate UPDATE) so the row is locked and consumed in one statement —
/// two concurrent requests racing on the same token can no longer both pass
/// a staleness-free read before either write lands.
pub async fn consume_valid_verification_token(
    pool: &PgPool,
    token_hash: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "update email_verification_tokens set consumed_at = now()
         where token_hash = $1 and expires_at > now() and consumed_at is null
         returning user_id",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(user_id,)| user_id))
}

pub async fn insert_reset_token(pool: &PgPool, user_id: Uuid, token_hash: &str) -> Result<(), sqlx::Error> {
    let expires_at = Utc::now() + Duration::hours(1);
    sqlx::query(
        "insert into password_reset_tokens (user_id, token_hash, expires_at) values ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(token_hash)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Returns the user_id if the token is valid and unconsumed, and marks it consumed.
///
/// Same atomic `UPDATE ... RETURNING` pattern as `consume_valid_verification_token`
/// above — a single statement so the token can't be consumed twice by two
/// concurrent requests racing on the same token.
pub async fn consume_valid_reset_token(pool: &PgPool, token_hash: &str) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "update password_reset_tokens set consumed_at = now()
         where token_hash = $1 and expires_at > now() and consumed_at is null
         returning user_id",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(user_id,)| user_id))
}
