//! Channel types, the per-kind destination rules (R8, R9), sealing (R10) and
//! the SQL.

use std::{fmt, str::FromStr, sync::LazyLock};

use base64::Engine;
use chrono::{DateTime, Utc};
use rand::{RngCore, rngs::OsRng};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    core::crypto::{Cipher, DecryptError},
    core::error::AppError,
    core::validate::has_invisible_or_bidi,
    incidents::outbound::{self, DISCORD, Reach, SLACK, UrlRule},
};

/// Every channel kind.
pub const KINDS: [&str; 6] = ["email", "webhook", "slack", "teams", "telegram", "discord"];
/// The kinds a personal channel may be (no email, R8).
pub const PERSONAL_KINDS: [&str; 5] = ["webhook", "slack", "teams", "telegram", "discord"];
/// Most channels one project may hold.
pub const MAX_PROJECT_CHANNELS: i64 = 50;
/// Most personal channels one user may hold.
pub const MAX_PERSONAL_CHANNELS: i64 = 10;
/// A channel name's length in chars (the column's check).
pub const MAX_NAME_CHARS: usize = 100;
/// The longest email address a channel may hold, in bytes.
pub const MAX_ADDRESS_BYTES: usize = 254;
/// AAD prefix of `config_enc` (see [`config_context`]).
pub const CONFIG_CONTEXT: &str = "notification_channels.config";
/// AAD prefix of `signing_secret_enc` (see [`signing_secret_context`]).
pub const SIGNING_SECRET_CONTEXT: &str = "notification_channels.signing_secret";

/// The AAD `config_enc` of channel `id` is sealed under.
pub fn config_context(id: Uuid) -> String {
    format!("{CONFIG_CONTEXT}:{id}")
}

/// The AAD `signing_secret_enc` of channel `id` is sealed under.
pub fn signing_secret_context(id: Uuid) -> String {
    format!("{SIGNING_SECRET_CONTEXT}:{id}")
}

// The bot token becomes part of the Telegram request path: `[0-9]`, never
// `\d`, which also matches non-ASCII digits.
static TELEGRAM_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9]{1,20}:[A-Za-z0-9_-]{30,64}$").expect("a constant regex")
});
static TELEGRAM_CHAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:-?[0-9]{1,20}|@[A-Za-z0-9_]{5,32})$").expect("a constant regex")
});

/// A channel's destination and secrets. Serialised as
/// `{"kind": "...", "config": {...}}`; sealed whole into `config_enc`.
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", content = "config", rename_all = "lowercase", deny_unknown_fields)]
pub enum ChannelConfig {
    Email { address: String },
    Webhook { url: String },
    Slack { webhook_url: String },
    Teams { webhook_url: String },
    Telegram { bot_token: String, chat_id: String },
    Discord { webhook_url: String },
}

impl fmt::Debug for ChannelConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelConfig")
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

/// Who a channel belongs to: a project (admin-managed) or one user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Project(Uuid),
    User(Uuid),
}

impl Owner {
    /// `(project_id, user_id)`, one of them `None`.
    fn ids(&self) -> (Option<Uuid>, Option<Uuid>) {
        match *self {
            Owner::Project(id) => (Some(id), None),
            Owner::User(id) => (None, Some(id)),
        }
    }
}

impl ChannelConfig {
    /// The kind, as stored in `notification_channels.kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            ChannelConfig::Email { .. } => "email",
            ChannelConfig::Webhook { .. } => "webhook",
            ChannelConfig::Slack { .. } => "slack",
            ChannelConfig::Teams { .. } => "teams",
            ChannelConfig::Telegram { .. } => "telegram",
            ChannelConfig::Discord { .. } => "discord",
        }
    }

    /// The URL a send goes to and its rule; `None` for email and Telegram.
    fn destination(&self) -> Option<(&str, UrlRule)> {
        match self {
            ChannelConfig::Webhook { url } => Some((url, UrlRule::HttpOrHttps)),
            ChannelConfig::Slack { webhook_url } => Some((webhook_url, SLACK)),
            ChannelConfig::Teams { webhook_url } => Some((webhook_url, UrlRule::Https)),
            ChannelConfig::Discord { webhook_url } => Some((webhook_url, DISCORD)),
            ChannelConfig::Email { .. } | ChannelConfig::Telegram { .. } => None,
        }
    }

    /// The R8/R9 rules for this owner, always strict: no personal email, the
    /// URL rule of the kind under [`Self::reach`], the Telegram token and chat
    /// shapes, a bare email address. The error is a 400 message.
    pub fn validate(&self, owner: &Owner) -> Result<(), String> {
        if let Some((url, rule)) = self.destination() {
            return outbound::check_url(url, rule, self.reach(owner)).map(drop);
        }
        match self {
            ChannelConfig::Email { .. } if matches!(owner, Owner::User(_)) => Err(
                "personal channels cannot be email: notifications already go to your verified address"
                    .into(),
            ),
            ChannelConfig::Email { address } => {
                // lettre accepts a quoted local part with tabs and any
                // non-ASCII char; the address reaches SMTP headers and the UI.
                if address.len() > MAX_ADDRESS_BYTES
                    || address.chars().any(char::is_control)
                    || has_invisible_or_bidi(address)
                    || lettre::Address::from_str(address).is_err()
                {
                    Err("address must be a valid email address".into())
                } else {
                    Ok(())
                }
            }
            ChannelConfig::Telegram { bot_token, chat_id } => {
                if !TELEGRAM_TOKEN.is_match(bot_token) {
                    Err("bot_token is not a Telegram bot token".into())
                } else if !TELEGRAM_CHAT.is_match(chat_id) {
                    Err("chat_id must be a numeric chat id or an @channel name".into())
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    /// The client and literal policy a send uses: public-only for every
    /// personal channel and every Slack, Discord and Teams channel; the
    /// monitors' policy for project webhooks (and Telegram, whose base URL is
    /// operator config).
    pub fn reach(&self, owner: &Owner) -> Reach {
        match (self, owner) {
            (ChannelConfig::Slack { .. } | ChannelConfig::Teams { .. } | ChannelConfig::Discord { .. }, _)
            | (ChannelConfig::Webhook { .. }, Owner::User(_)) => Reach::PublicOnly,
            _ => Reach::MonitorPolicy,
        }
    }

    /// What the UI may show (R10), never secret: a webhook's scheme, host and
    /// port, a chat host, `Telegram chat <id>` or the email address.
    pub fn summary(&self) -> String {
        let parsed = self.destination().and_then(|(url, _)| reqwest::Url::parse(url).ok());
        match (self, parsed) {
            (ChannelConfig::Email { address }, _) => address.clone(),
            (ChannelConfig::Telegram { chat_id, .. }, _) => format!("Telegram chat {chat_id}"),
            (ChannelConfig::Webhook { .. }, Some(url)) => url.origin().ascii_serialization(),
            (_, Some(url)) => url.host_str().unwrap_or(self.kind()).to_string(),
            (_, None) => self.kind().to_string(),
        }
    }
}

/// A channel as the API returns it: no config, no secret.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Channel {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub name: String,
    pub kind: String,
    pub summary: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The create response: the webhook signing secret appears here only.
#[derive(Serialize)]
pub struct CreatedChannel {
    #[serde(flatten)]
    pub channel: Channel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_secret: Option<String>,
}

/// The outcome of [`insert`].
pub enum Inserted {
    Created(CreatedChannel),
    /// The owner already holds its maximum of channels.
    Full,
    /// The project does not exist (a global admin may name any id).
    NoOwner,
}

// sqlx takes only `&'static str` SQL, so the column list is a macro.
macro_rules! columns {
    () => {
        "id, project_id, user_id, name, kind, summary, enabled, created_at, updated_at"
    };
}

/// Seals `config` (and, for a webhook, a fresh signing secret: 32 random
/// bytes, base64url, sealed as that 43-char string) and inserts the channel.
/// The owner's row is locked for the count and the insert, so concurrent
/// creates cannot pass the cap together. Does not run
/// [`ChannelConfig::validate`]: routes do, so tests can point a channel at
/// wiremock.
pub async fn insert(
    pool: &PgPool,
    cipher: &Cipher,
    owner: Owner,
    name: &str,
    config: &ChannelConfig,
    enabled: bool,
    created_by: Uuid,
) -> Result<Inserted, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let (project_id, user_id, limit) = match owner {
        Owner::Project(id) => (Some(id), None, MAX_PROJECT_CHANNELS),
        Owner::User(id) => (None, Some(id), MAX_PERSONAL_CHANNELS),
    };
    let locked: Option<Uuid> = match owner {
        Owner::Project(id) => {
            sqlx::query_scalar("select id from projects where id = $1 for no key update")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
        }
        Owner::User(id) => {
            sqlx::query_scalar("select id from users where id = $1 for no key update")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
        }
    };
    if locked.is_none() {
        return Ok(Inserted::NoOwner);
    }
    let count: i64 = sqlx::query_scalar(
        "select count(*) from notification_channels where project_id = $1 or user_id = $2",
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    if count >= limit {
        return Ok(Inserted::Full);
    }

    let id = Uuid::now_v7();
    let config_enc = seal_config(cipher, id, config);
    let signing_secret = matches!(config, ChannelConfig::Webhook { .. }).then(|| {
        let mut bytes = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(&mut *bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes[..])
    });
    let signing_secret_enc = signing_secret
        .as_ref()
        .map(|secret| cipher.encrypt(&signing_secret_context(id), secret.as_bytes()));
    let channel: Channel = sqlx::query_as(concat!(
        "insert into notification_channels
            (id, project_id, user_id, name, kind, config_enc, signing_secret_enc, summary, enabled, created_by)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) returning ",
        columns!()
    ))
    .bind(id)
    .bind(project_id)
    .bind(user_id)
    .bind(name)
    .bind(config.kind())
    .bind(config_enc)
    .bind(signing_secret_enc)
    .bind(config.summary())
    .bind(enabled)
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Inserted::Created(CreatedChannel { channel, signing_secret }))
}

fn seal_config(cipher: &Cipher, id: Uuid, config: &ChannelConfig) -> Vec<u8> {
    let json = Zeroizing::new(
        serde_json::to_vec(config).expect("a ChannelConfig of plain strings always serialises"),
    );
    cipher.encrypt(&config_context(id), &json)
}

/// The owner's channels, by name.
pub async fn list(pool: &PgPool, owner: &Owner) -> Result<Vec<Channel>, sqlx::Error> {
    let (project_id, user_id) = owner.ids();
    sqlx::query_as(concat!(
        "select ",
        columns!(),
        " from notification_channels where project_id = $1 or user_id = $2 order by name, id"
    ))
    .bind(project_id)
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// The owner's channel `id`; `None` for another owner's.
pub async fn find(pool: &PgPool, owner: &Owner, id: Uuid) -> Result<Option<Channel>, sqlx::Error> {
    let (project_id, user_id) = owner.ids();
    sqlx::query_as(concat!(
        "select ",
        columns!(),
        " from notification_channels where id = $1 and (project_id = $2 or user_id = $3)"
    ))
    .bind(id)
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// Fields a PATCH changes; `None` keeps the stored value. A config replaces
/// the old one whole and keeps the signing secret.
pub struct ChannelPatch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub config: Option<ChannelConfig>,
}

impl fmt::Debug for ChannelPatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelPatch")
            .field("config_kind", &self.config.as_ref().map(ChannelConfig::kind))
            .finish_non_exhaustive()
    }
}

/// `Ok(None)` = not the owner's channel. A config of another kind is a
/// `Validation` error: the kind of a channel never changes. The config is
/// sealed again only when one is given.
pub async fn update(
    pool: &PgPool,
    cipher: &Cipher,
    owner: &Owner,
    id: Uuid,
    patch: ChannelPatch,
) -> Result<Option<Channel>, AppError> {
    let (project_id, user_id) = owner.ids();
    let mut sealed = None;
    if let Some(config) = &patch.config {
        let Some(stored) = find(pool, owner, id).await? else {
            return Ok(None);
        };
        if stored.kind != config.kind() {
            return Err(AppError::Validation("the kind of a channel cannot change".into()));
        }
        sealed = Some((seal_config(cipher, id, config), config.summary()));
    }
    let (config_enc, summary) = sealed.unzip();
    Ok(sqlx::query_as(concat!(
        "update notification_channels
         set name = coalesce($4, name), enabled = coalesce($5, enabled),
             config_enc = coalesce($6, config_enc), summary = coalesce($7, summary),
             updated_at = now()
         where id = $1 and (project_id = $2 or user_id = $3) returning ",
        columns!()
    ))
    .bind(id)
    .bind(project_id)
    .bind(user_id)
    .bind(patch.name)
    .bind(patch.enabled)
    .bind(config_enc)
    .bind(summary)
    .fetch_optional(pool)
    .await?)
}

/// The outcome of [`delete`].
pub enum Deleted {
    Deleted,
    NotFound,
    /// An escalation step targets the channel (returned once escalation
    /// policies exist, Task 8).
    InUse { policy: String },
}

/// Deletes the owner's channel `id`; another owner's is `NotFound`.
pub async fn delete(pool: &PgPool, owner: &Owner, id: Uuid) -> Result<Deleted, sqlx::Error> {
    let (project_id, user_id) = owner.ids();
    let result = sqlx::query(
        "delete from notification_channels where id = $1 and (project_id = $2 or user_id = $3)",
    )
    .bind(id)
    .bind(project_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(if result.rows_affected() > 0 {
        Deleted::Deleted
    } else {
        Deleted::NotFound
    })
}

/// Everything a sender needs, opened.
pub struct OpenedChannel {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub owner: Owner,
    pub enabled: bool,
    pub config: ChannelConfig,
    pub signing_secret: Option<Zeroizing<Vec<u8>>>,
}

impl fmt::Debug for OpenedChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenedChannel")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

#[derive(sqlx::FromRow)]
struct SealedRow {
    id: Uuid,
    project_id: Option<Uuid>,
    user_id: Option<Uuid>,
    name: String,
    kind: String,
    enabled: bool,
    config_enc: Vec<u8>,
    signing_secret_enc: Option<Vec<u8>>,
}

/// Channel `id` with its config and signing secret opened. `None` when it
/// does not exist; `Some(Err(DecryptError))` when its secrets no longer open
/// under the current key (or do not match the row's kind).
pub async fn open_for_send(
    pool: &PgPool,
    cipher: &Cipher,
    id: Uuid,
) -> Result<Option<Result<OpenedChannel, DecryptError>>, sqlx::Error> {
    let row: Option<SealedRow> = sqlx::query_as(
        "select id, project_id, user_id, name, kind, enabled, config_enc, signing_secret_enc
         from notification_channels where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| open_row(cipher, row)))
}

fn open_row(cipher: &Cipher, row: SealedRow) -> Result<OpenedChannel, DecryptError> {
    let json = Zeroizing::new(cipher.decrypt(&config_context(row.id), &row.config_enc)?);
    let config: ChannelConfig = serde_json::from_slice(&json).map_err(|_| DecryptError)?;
    if config.kind() != row.kind {
        return Err(DecryptError);
    }
    let signing_secret = row
        .signing_secret_enc
        .map(|sealed| cipher.decrypt(&signing_secret_context(row.id), &sealed).map(Zeroizing::new))
        .transpose()?;
    let owner = match (row.project_id, row.user_id) {
        (Some(project), _) => Owner::Project(project),
        (None, Some(user)) => Owner::User(user),
        // The table's one-owner check rules this out.
        (None, None) => return Err(DecryptError),
    };
    Ok(OpenedChannel {
        id: row.id,
        kind: row.kind,
        name: row.name,
        owner,
        enabled: row.enabled,
        config,
        signing_secret,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_tokens_and_chat_ids_are_shaped() {
        let ok = ChannelConfig::Telegram {
            bot_token: format!("123456:{}", "A".repeat(35)),
            chat_id: "-1001234".into(),
        };
        assert!(ok.validate(&Owner::User(Uuid::nil())).is_ok());
        for (token, chat) in [
            ("123456:short", "1"),
            ("abc:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "1"),
            ("123456:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/../x", "1"),
            ("123456:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "@ab"),
            ("123456:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "1; drop"),
            ("\u{661}\u{662}:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "1"),
        ] {
            let c = ChannelConfig::Telegram { bot_token: token.into(), chat_id: chat.into() };
            assert!(c.validate(&Owner::User(Uuid::nil())).is_err(), "{token} {chat}");
        }
    }

    #[test]
    fn summaries_and_debug_output_never_show_secrets() {
        let slack = ChannelConfig::Slack {
            webhook_url: "https://hooks.slack.com/services/T0/B0/SECRET".into(),
        };
        assert_eq!(slack.summary(), "hooks.slack.com");
        assert!(!format!("{slack:?}").contains("SECRET"));
        let hook = ChannelConfig::Webhook { url: "https://ops.example:8443/in?key=SECRET".into() };
        assert_eq!(hook.summary(), "https://ops.example:8443");
        let tg = ChannelConfig::Telegram {
            bot_token: format!("1:{}", "S".repeat(35)),
            chat_id: "42".into(),
        };
        assert_eq!(tg.summary(), "Telegram chat 42");
        assert!(!format!("{tg:?}").contains("SSSS"));
    }

    #[test]
    fn the_contexts_name_the_column_and_the_row() {
        let id = Uuid::nil();
        assert_eq!(config_context(id), format!("notification_channels.config:{id}"));
        assert_eq!(
            signing_secret_context(id),
            format!("notification_channels.signing_secret:{id}")
        );
    }

    #[test]
    fn a_config_with_an_unknown_field_is_refused() {
        let extra = serde_json::json!({
            "kind": "slack",
            "config": {"webhook_url": "https://hooks.slack.com/services/T/B/x", "extra": 1}
        });
        assert!(serde_json::from_value::<ChannelConfig>(extra).is_err());
    }

    #[test]
    fn email_addresses_are_bare_and_bounded() {
        let project = Owner::Project(Uuid::nil());
        let labels = ["b", "c", "d", "e"].map(|c| c.repeat(60)).join(".");
        let at_limit = format!("a@{labels}.fff.test");
        assert_eq!(at_limit.len(), MAX_ADDRESS_BYTES);
        assert!(ChannelConfig::Email { address: at_limit.clone() }.validate(&project).is_ok());
        for bad in [
            format!("a{at_limit}"),
            "Ops <ops@b.test>".into(),
            "not an address".into(),
            "\"a\tb\"@x.test".into(),
            "a\u{202e}b@x.test".into(),
            "a\u{200b}@x.test".into(),
            "a@ex\u{200d}ample.test".into(),
        ] {
            assert!(ChannelConfig::Email { address: bad.clone() }.validate(&project).is_err(), "{bad}");
        }
    }
}
