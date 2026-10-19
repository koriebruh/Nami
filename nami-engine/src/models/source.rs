//! Log sources: a project's named ingestion endpoints, each with its own
//! ingest token. A token is 32 random bytes, base64url without padding (43
//! characters), stored twice: as its SHA-256 (hex, `token_hash`), which the
//! ingest paths look sources up by, and sealed with AES-256-GCM
//! (`token_enc`, associated data [`token_context`], which names the column
//! and the source) so members can see it again. Neither column is a field of
//! [`Source`], so no response can carry the stored values by accident. The
//! plaintext token appears only in [`SourceWithToken`] (create and rotate)
//! and in the reveal route's reply, and `SourceWithToken` never prints it.

use std::fmt;

use base64::Engine;
use chrono::{DateTime, Utc};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::validate_name;
use crate::{auth::token::hash_token, crypto::Cipher, error::AppError};

pub const PLATFORMS: [&str; 5] = ["http", "syslog", "vector", "docker", "other"];
pub const DEFAULT_PLATFORM: &str = "http";
pub const DEFAULT_RETENTION_DAYS: i32 = 30;
pub const MAX_RETENTION_DAYS: i32 = 365;
/// The smallest daily byte quota a source may have (1 MiB).
pub const MIN_DAILY_BYTE_QUOTA: i64 = 1024 * 1024;
/// The column part of the AES-GCM associated data of `sources.token_enc`.
/// What is actually used is [`token_context`], which adds the source id.
pub const TOKEN_CONTEXT: &str = "sources.token";
/// A generated token is 32 random bytes in base64url without padding: 43
/// characters.
pub const TOKEN_LEN: usize = 43;
/// What `Debug` prints in place of an ingest token.
const REDACTED: &str = "<redacted>";

/// The AES-GCM associated data of one source's sealed token:
/// `sources.token:{id}`. Binding the id to the ciphertext means a sealed
/// value copied onto another row (by anyone who can write the table) does not
/// open there, so the reveal route cannot be turned into an oracle for the
/// other source's token.
pub fn token_context(source_id: Uuid) -> String {
    format!("{TOKEN_CONTEXT}:{source_id}")
}

/// A source as every API response shows it: never its token.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct Source {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub platform: String,
    pub retention_days: i32,
    pub daily_byte_quota: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A source with its plaintext token: the response to create and rotate,
/// which only members and admins can call. `Debug` leaves the token out, so
/// it cannot reach a log line through `{:?}`.
#[derive(Clone, PartialEq, Serialize)]
pub struct SourceWithToken {
    #[serde(flatten)]
    pub source: Source,
    pub token: String,
}

impl fmt::Debug for SourceWithToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Exhaustive, so that a new field cannot be left out by accident.
        let SourceWithToken { source, token: _ } = self;
        f.debug_struct("SourceWithToken")
            .field("source", source)
            .field("token", &REDACTED)
            .finish()
    }
}

/// The slice of a source the ingest paths need, found by token.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct IngestSource {
    pub id: Uuid,
    pub project_id: Uuid,
    pub retention_days: i32,
    pub daily_byte_quota: Option<i64>,
}

/// The user-editable fields of a source.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceConfig {
    pub name: String,
    pub platform: String,
    pub retention_days: i32,
    pub daily_byte_quota: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSourceRequest {
    pub name: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    #[serde(default = "default_retention_days")]
    pub retention_days: i32,
    #[serde(default)]
    pub daily_byte_quota: Option<i64>,
}

fn default_platform() -> String {
    DEFAULT_PLATFORM.to_string()
}

fn default_retention_days() -> i32 {
    DEFAULT_RETENTION_DAYS
}

impl CreateSourceRequest {
    pub fn into_config(self) -> SourceConfig {
        SourceConfig {
            name: self.name,
            platform: self.platform,
            retention_days: self.retention_days,
            daily_byte_quota: self.daily_byte_quota,
        }
    }
}

/// A PATCH field that can be cleared: absent = keep, `null` = clear, a value =
/// set. Private, so that Logs does not depend on another sub-project's
/// helper of the same purpose.
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Omitted fields are kept; `daily_byte_quota: null` removes the quota.
#[derive(Debug, Default, Deserialize)]
pub struct PatchSourceRequest {
    pub name: Option<String>,
    pub platform: Option<String>,
    pub retention_days: Option<i32>,
    #[serde(default, deserialize_with = "nullable")]
    pub daily_byte_quota: Option<Option<i64>>,
}

impl PatchSourceRequest {
    pub fn apply_to(self, base: SourceConfig) -> SourceConfig {
        SourceConfig {
            name: self.name.unwrap_or(base.name),
            platform: self.platform.unwrap_or(base.platform),
            retention_days: self.retention_days.unwrap_or(base.retention_days),
            daily_byte_quota: self.daily_byte_quota.unwrap_or(base.daily_byte_quota),
        }
    }
}

pub fn validate(mut c: SourceConfig) -> Result<SourceConfig, String> {
    c.name = validate_name(&c.name)?;
    if !PLATFORMS.contains(&c.platform.as_str()) {
        return Err(format!("platform must be one of {}", PLATFORMS.join(", ")));
    }
    if !(1..=MAX_RETENTION_DAYS).contains(&c.retention_days) {
        return Err(format!(
            "retention_days must be between 1 and {MAX_RETENTION_DAYS}"
        ));
    }
    if matches!(c.daily_byte_quota, Some(quota) if quota < MIN_DAILY_BYTE_QUOTA) {
        return Err(format!(
            "daily_byte_quota must be at least {MIN_DAILY_BYTE_QUOTA} bytes (1 MiB), or null"
        ));
    }
    Ok(c)
}

/// A fresh ingest token: 32 bytes from the OS RNG, base64url, no padding.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Whether `token` has the shape of a generated one: exactly [`TOKEN_LEN`]
/// characters of the base64url alphabet. Anything else is no source's token,
/// so it is never looked up ([`find_by_token`]), and the public ingest paths
/// refuse it before they spend a database query on it.
pub fn is_well_formed_token(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub async fn insert_source(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    c: &SourceConfig,
) -> Result<SourceWithToken, sqlx::Error> {
    insert_source_in(pool, cipher, project_id, c).await
}

async fn insert_source_in<'e, E: sqlx::PgExecutor<'e>>(
    executor: E,
    cipher: &Cipher,
    project_id: Uuid,
    c: &SourceConfig,
) -> Result<SourceWithToken, sqlx::Error> {
    // The id is chosen here, not by the database, because the sealed token is
    // bound to it.
    let id = Uuid::new_v4();
    let token = new_token();
    let source: Source = sqlx::query_as(
        "insert into sources (id, project_id, name, platform, retention_days, daily_byte_quota, token_hash, token_enc)
         values ($1, $2, $3, $4, $5, $6, $7, $8)
         returning *",
    )
    .bind(id)
    .bind(project_id)
    .bind(&c.name)
    .bind(&c.platform)
    .bind(c.retention_days)
    .bind(c.daily_byte_quota)
    .bind(hash_token(&token))
    .bind(cipher.encrypt(&token_context(id), token.as_bytes()))
    .fetch_one(executor)
    .await?;
    Ok(SourceWithToken { source, token })
}

/// What [`create_source`] did.
pub enum CreatedSource {
    Created(SourceWithToken),
    /// The project already has `max_sources` sources; nothing was created.
    ProjectFull,
    /// There is no such project.
    NoProject,
}

/// [`insert_source`] for the API: refused once the project has `max_sources`
/// sources (`MAX_SOURCES_PER_PROJECT`). Creations in one project take turns
/// on the project's row (`for no key update`, which still lets other tables'
/// foreign keys reference it), so concurrent requests cannot pass the cap
/// together.
pub async fn create_source(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    c: &SourceConfig,
    max_sources: i64,
) -> Result<CreatedSource, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let project: Option<(Uuid,)> =
        sqlx::query_as("select id from projects where id = $1 for no key update")
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    if project.is_none() {
        return Ok(CreatedSource::NoProject);
    }
    let (count,): (i64,) = sqlx::query_as("select count(*) from sources where project_id = $1")
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= max_sources {
        return Ok(CreatedSource::ProjectFull);
    }
    let created = insert_source_in(&mut *tx, cipher, project_id, c).await?;
    tx.commit().await?;
    Ok(CreatedSource::Created(created))
}

pub async fn list_for_project(pool: &PgPool, project_id: Uuid) -> Result<Vec<Source>, sqlx::Error> {
    sqlx::query_as("select * from sources where project_id = $1 order by name, id")
        .bind(project_id)
        .fetch_all(pool)
        .await
}

/// Scoped by both ids: another project's source is not found (404).
pub async fn find_in_project(
    pool: &PgPool,
    project_id: Uuid,
    source_id: Uuid,
) -> Result<Option<Source>, sqlx::Error> {
    sqlx::query_as("select * from sources where id = $1 and project_id = $2")
        .bind(source_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// Merge-validate-write under the row's lock; bumps `updated_at`.
///
/// Every write to `sources` moves `updated_at` forward and never back
/// (`greatest(clock_timestamp(), updated_at + 1 µs)`, R-P2-6): the ingest
/// paths key their compiled pipeline cache on it, and `now()` (the
/// transaction start) could be older than a pipeline save that committed
/// while this one waited on the row lock.
pub async fn update_in_project(
    pool: &PgPool,
    project_id: Uuid,
    source_id: Uuid,
    patch: PatchSourceRequest,
) -> Result<Option<Source>, AppError> {
    let mut tx = pool.begin().await?;
    let current: Option<Source> =
        sqlx::query_as("select * from sources where id = $1 and project_id = $2 for update")
            .bind(source_id)
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let base = SourceConfig {
        name: current.name,
        platform: current.platform,
        retention_days: current.retention_days,
        daily_byte_quota: current.daily_byte_quota,
    };
    let c = validate(patch.apply_to(base)).map_err(AppError::Validation)?;
    let updated: Source = sqlx::query_as(
        "update sources
         set name = $3, platform = $4, retention_days = $5, daily_byte_quota = $6,
             updated_at = greatest(clock_timestamp(), updated_at + interval '1 microsecond')
         where id = $1 and project_id = $2
         returning *",
    )
    .bind(source_id)
    .bind(project_id)
    .bind(&c.name)
    .bind(&c.platform)
    .bind(c.retention_days)
    .bind(c.daily_byte_quota)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(updated))
}

/// The stored pipeline rules of one source of the project and its
/// `updated_at`; `None` when the project has no such source.
pub async fn get_pipeline(
    pool: &PgPool,
    project_id: Uuid,
    source_id: Uuid,
) -> Result<Option<(serde_json::Value, DateTime<Utc>)>, sqlx::Error> {
    sqlx::query_as("select pipeline, updated_at from sources where id = $1 and project_id = $2")
        .bind(source_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// Replaces the rules (already validated by `logs::pipeline::compile`) and
/// bumps `updated_at`; `None` when the project has no such source.
pub async fn set_pipeline(
    pool: &PgPool,
    project_id: Uuid,
    source_id: Uuid,
    rules: &serde_json::Value,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    // clock_timestamp(), not now(): two writes in one transaction would
    // otherwise share an updated_at and the cache could keep the older rules.
    // The `greatest` keeps it growing even if the clock steps back.
    sqlx::query_scalar(
        "update sources
         set pipeline = $3,
             updated_at = greatest(clock_timestamp(), updated_at + interval '1 microsecond')
         where id = $1 and project_id = $2
         returning updated_at",
    )
    .bind(source_id)
    .bind(project_id)
    .bind(rules)
    .fetch_optional(pool)
    .await
}

/// The ingest view of one source of the project (the pipeline preview
/// builds rows with it).
pub async fn ingest_source(
    pool: &PgPool,
    project_id: Uuid,
    source_id: Uuid,
) -> Result<Option<IngestSource>, sqlx::Error> {
    sqlx::query_as(
        "select id, project_id, retention_days, daily_byte_quota from sources where id = $1 and project_id = $2",
    )
    .bind(source_id)
    .bind(project_id)
    .fetch_optional(pool)
    .await
}

/// Deleting a source stops its ingestion at once; its rows age out by TTL.
pub async fn delete_in_project(
    pool: &PgPool,
    project_id: Uuid,
    source_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from sources where id = $1 and project_id = $2")
        .bind(source_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// The plaintext token, for members and admins. A token sealed under
/// another key (NAMI_ENCRYPTION_KEY changed) or for another row cannot be
/// shown, but ingestion keeps working — it only needs the hash — and rotating
/// issues a new one.
pub async fn reveal_token(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    source_id: Uuid,
) -> Result<Option<String>, AppError> {
    let sealed: Option<Vec<u8>> =
        sqlx::query_scalar("select token_enc from sources where id = $1 and project_id = $2")
            .bind(source_id)
            .bind(project_id)
            .fetch_optional(pool)
            .await?;
    let Some(sealed) = sealed else {
        return Ok(None);
    };
    let undecryptable = || {
        AppError::Conflict(
            "the stored token could not be decrypted (was NAMI_ENCRYPTION_KEY changed?); rotate it to get a new one"
                .to_string(),
        )
    };
    let plaintext = cipher
        .decrypt(&token_context(source_id), &sealed)
        .map_err(|_| undecryptable())?;
    String::from_utf8(plaintext)
        .map(Some)
        .map_err(|_| undecryptable())
}

/// Replaces the token: the old one stops working as soon as this commits.
pub async fn rotate_token(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    source_id: Uuid,
) -> Result<Option<SourceWithToken>, sqlx::Error> {
    let token = new_token();
    let source: Option<Source> = sqlx::query_as(
        "update sources set token_hash = $3, token_enc = $4,
         updated_at = greatest(clock_timestamp(), updated_at + interval '1 microsecond')
         where id = $1 and project_id = $2
         returning *",
    )
    .bind(source_id)
    .bind(project_id)
    .bind(hash_token(&token))
    .bind(cipher.encrypt(&token_context(source_id), token.as_bytes()))
    .fetch_optional(pool)
    .await?;
    Ok(source.map(|source| SourceWithToken { source, token }))
}

/// The source a presented ingest token belongs to, if any. A token that is
/// not [`is_well_formed_token`] belongs to nobody, and costs no query.
pub async fn find_by_token(
    pool: &PgPool,
    token: &str,
) -> Result<Option<IngestSource>, sqlx::Error> {
    if !is_well_formed_token(token) {
        return Ok(None);
    }
    sqlx::query_as(
        "select id, project_id, retention_days, daily_byte_quota from sources where token_hash = $1",
    )
    .bind(hash_token(token))
    .fetch_optional(pool)
    .await
}

/// The sources whose token hash is in `hashes`, as (token hash, source): one
/// query for a whole batch of syslog frames. Hashes of unknown tokens simply
/// have no entry.
pub async fn find_by_token_hashes(
    pool: &PgPool,
    hashes: &[String],
) -> Result<Vec<(String, IngestSource)>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        token_hash: String,
        #[sqlx(flatten)]
        source: IngestSource,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "select token_hash, id, project_id, retention_days, daily_byte_quota from sources where token_hash = any($1)",
    )
    .bind(hashes)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.token_hash, row.source))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SourceConfig {
        SourceConfig {
            name: "api".to_string(),
            platform: "http".to_string(),
            retention_days: 30,
            daily_byte_quota: None,
        }
    }

    fn patch(json: &str) -> PatchSourceRequest {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn tokens_are_43_base64url_characters_and_never_repeat() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 43);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{a}"
        );
        assert_ne!(a, b);
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&a)
            .unwrap();
        assert_eq!(bytes.len(), 32);
        assert!(is_well_formed_token(&a) && is_well_formed_token(&b));
    }

    #[test]
    fn only_the_exact_shape_of_a_generated_token_is_well_formed() {
        let token = "A".repeat(TOKEN_LEN);
        assert!(is_well_formed_token(&token));
        assert!(is_well_formed_token(&format!(
            "-_{}",
            "z9".repeat(20) + "Z"
        )));
        for bad in [
            String::new(),
            "A".repeat(TOKEN_LEN - 1),
            "A".repeat(TOKEN_LEN + 1),
            "A".repeat(10_000),
            format!("{}+", "A".repeat(TOKEN_LEN - 1)),
            format!("{}/", "A".repeat(TOKEN_LEN - 1)),
            format!("{}=", "A".repeat(TOKEN_LEN - 1)),
            format!("{} ", "A".repeat(TOKEN_LEN - 1)),
            format!("{}\n", "A".repeat(TOKEN_LEN - 1)),
            format!("{}\0", "A".repeat(TOKEN_LEN - 1)),
            // 43 bytes, 42 characters: the length is in bytes.
            format!("{}é", "A".repeat(TOKEN_LEN - 2)),
        ] {
            assert!(!is_well_formed_token(&bad), "{bad:?}");
        }
    }

    #[test]
    fn validate_accepts_the_bounds() {
        for retention_days in [1, 365] {
            assert!(
                validate(SourceConfig {
                    retention_days,
                    ..config()
                })
                .is_ok()
            );
        }
        for platform in PLATFORMS {
            assert!(
                validate(SourceConfig {
                    platform: platform.to_string(),
                    ..config()
                })
                .is_ok()
            );
        }
        let quota = SourceConfig {
            daily_byte_quota: Some(MIN_DAILY_BYTE_QUOTA),
            ..config()
        };
        assert_eq!(validate(quota).unwrap().daily_byte_quota, Some(1_048_576));
    }

    #[test]
    fn validate_trims_the_name() {
        assert_eq!(
            validate(SourceConfig {
                name: "  api  ".to_string(),
                ..config()
            })
            .unwrap()
            .name,
            "api"
        );
    }

    #[test]
    fn patch_requests_tell_an_absent_field_from_null_and_from_a_value() {
        let base = SourceConfig {
            daily_byte_quota: Some(5_000_000),
            ..config()
        };
        assert_eq!(patch("{}").daily_byte_quota, None);
        assert_eq!(
            patch("{}").apply_to(base.clone()).daily_byte_quota,
            Some(5_000_000),
            "absent keeps the quota"
        );
        assert_eq!(
            patch(r#"{"daily_byte_quota": null}"#).daily_byte_quota,
            Some(None)
        );
        assert_eq!(
            patch(r#"{"daily_byte_quota": null}"#)
                .apply_to(base.clone())
                .daily_byte_quota,
            None,
            "null clears it"
        );
        assert_eq!(
            patch(r#"{"daily_byte_quota": 2000000}"#)
                .apply_to(base)
                .daily_byte_quota,
            Some(2_000_000)
        );
    }

    #[test]
    fn the_token_context_names_the_column_and_the_source() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(token_context(a), format!("sources.token:{a}"));
        assert_ne!(token_context(a), token_context(b));

        let cipher = Cipher::new(&crate::crypto::TEST_KEY);
        let sealed = cipher.encrypt(&token_context(a), b"token");
        assert_eq!(
            cipher.decrypt(&token_context(a), &sealed).unwrap(),
            b"token"
        );
        assert!(cipher.decrypt(&token_context(b), &sealed).is_err());
        assert!(cipher.decrypt(TOKEN_CONTEXT, &sealed).is_err());
    }

    #[test]
    fn debug_output_never_shows_the_token() {
        let now = Utc::now();
        let created = SourceWithToken {
            source: Source {
                id: Uuid::new_v4(),
                project_id: Uuid::new_v4(),
                name: "api".to_string(),
                platform: "http".to_string(),
                retention_days: 30,
                daily_byte_quota: None,
                created_at: now,
                updated_at: now,
            },
            token: new_token(),
        };
        for shown in [format!("{created:?}"), format!("{created:#?}")] {
            assert!(!shown.contains(&created.token), "{shown}");
            assert!(shown.contains(REDACTED), "{shown}");
            assert!(shown.contains("api"), "the rest is still shown: {shown}");
        }
    }
}
