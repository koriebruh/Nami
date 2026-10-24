//! One notification request (or email) per channel kind. The destination is
//! checked again before every request (R9), the client is picked by the
//! channel's reach, webhooks are signed (R29), and each outcome is classified
//! for the retry policy (R12). Error text is a fixed vocabulary (R11).

use std::time::Duration;

use chrono::Utc;
use reqwest::{StatusCode, header};
use uuid::Uuid;

use crate::{
    core::{config::Config, email::Mailer, state::AppState},
    incidents::{
        channel::model::{self as channel, ChannelConfig, OpenedChannel, Owner},
        format::{self, IncidentSummary, Message, NotifyEvent},
        outbound::{self, OutboundPolicy, Reach, UrlRule},
    },
    projects::model as projects,
    uptime::checker::DESTINATION_NOT_ALLOWED,
};

/// The longest wait a `Retry-After` from a 429 may ask for.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);
/// What a channel whose secrets no longer open records (R10). It is never
/// sent anywhere.
pub const UNDECRYPTABLE: &str =
    "channel configuration could not be decrypted (was NAMI_ENCRYPTION_KEY changed?)";
/// What a failed email records (R11): the SMTP reply may name the recipient.
pub const EMAIL_NOT_DELIVERED: &str = "email not delivered";

/// Why a send failed, and whether trying again can help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendError {
    /// From the R11 vocabulary: `HTTP 404`, `timeout`, `connection failed`,
    /// `destination not allowed`, `email not delivered`, ... Never a URL,
    /// address, body or token.
    pub message: String,
    pub retry: Retry,
}

/// The retry class of a failure (R12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Retrying cannot help (a 4xx, a refused destination).
    Never,
    /// Retry after the backoff, or after `Some(wait)` when that is longer
    /// (a 429's `Retry-After`, at most [`MAX_RETRY_AFTER`]).
    After(Option<Duration>),
}

impl SendError {
    pub(crate) fn never(message: &str) -> Self {
        SendError { message: message.to_string(), retry: Retry::Never }
    }

    fn retry(message: String) -> Self {
        SendError { message, retry: Retry::After(None) }
    }
}

/// The settings a send reads from `Config`.
#[derive(Debug, Clone)]
pub struct Senders {
    pub telegram_base: String,
    pub policy: OutboundPolicy,
}

impl Senders {
    /// Built per kick, so a test can change the config between kicks.
    pub fn from_config(config: &Config) -> Senders {
        Senders {
            telegram_base: config.telegram_api_base_url.clone(),
            policy: config.outbound_policy,
        }
    }
}

/// One request (or one email) to `channel`. Checks the destination again
/// (`check_url_for_send` with the channel's reach and `s.policy`; a refusal is
/// `destination not allowed`, never retried), then sends to the URL that check
/// returns with `outbound::public_client()` for `Reach::PublicOnly` and
/// `outbound::client()` otherwise. A webhook carries `X-Nami-Timestamp`,
/// `X-Nami-Signature: sha256=...` over `{timestamp}.{body}`, `X-Nami-Delivery:
/// <delivery_id>` and `X-Nami-Event`. 2xx is success; the body of any response
/// is never read.
pub async fn send_to_channel(
    s: &Senders,
    mailer: &Mailer,
    channel: &OpenedChannel,
    message: &Message,
    delivery_id: Uuid,
) -> Result<(), SendError> {
    let to_json = |value: serde_json::Value| value.to_string().into_bytes();
    let body = match &channel.config {
        ChannelConfig::Email { address } => return send_email(mailer, address, message).await,
        ChannelConfig::Webhook { .. } => format::webhook_body(message),
        ChannelConfig::Slack { .. } => to_json(format::slack(message)),
        ChannelConfig::Teams { .. } => to_json(format::teams(message)),
        ChannelConfig::Discord { .. } => to_json(format::discord(message)),
        ChannelConfig::Telegram { chat_id, .. } => to_json(format::telegram(message, chat_id)),
    };
    let (raw, rule) = match &channel.config {
        ChannelConfig::Telegram { bot_token, .. } => (
            format!("{}/bot{bot_token}/sendMessage", s.telegram_base.trim_end_matches('/')),
            UrlRule::HttpOrHttps,
        ),
        config => {
            let (url, rule) = config
                .destination()
                .expect("every kind but email (returned above) and Telegram has a URL");
            (url.to_string(), rule)
        }
    };
    let reach = channel.config.reach(&channel.owner);
    // Also the only guard for IP literals: reqwest never passes them to the resolver.
    let url = outbound::check_url_for_send(&raw, rule, reach, s.policy)
        .map_err(|_| SendError::never(DESTINATION_NOT_ALLOWED))?;
    let client = match reach {
        Reach::PublicOnly => outbound::public_client(),
        Reach::MonitorPolicy => outbound::client(),
    };
    let mut request = client.post(url).header(header::CONTENT_TYPE, "application/json");
    if let ChannelConfig::Webhook { .. } = channel.config {
        let secret = channel.signing_secret.as_deref().ok_or_else(|| SendError::never(UNDECRYPTABLE))?;
        // Taken per attempt, so a retry is signed afresh (R29).
        let timestamp = Utc::now().timestamp();
        request = request
            .header("x-nami-timestamp", timestamp.to_string())
            .header("x-nami-signature", format::signature(secret, timestamp, &body))
            .header("x-nami-delivery", delivery_id.to_string())
            .header("x-nami-event", message.event.webhook_name());
    }
    // With no idle pool, dropping the response unread closes the connection:
    // nothing of the body is ever read or kept.
    let response = request
        .body(body)
        .send()
        .await
        .map_err(|e| SendError::retry(outbound::describe_error(&e)))?;
    classify(response.status(), retry_after(response.headers()))
}

/// The email `message` renders, to `to`. Any failure is
/// `email not delivered`, retried.
pub async fn send_email(mailer: &Mailer, to: &str, message: &Message) -> Result<(), SendError> {
    let (subject, body) = format::email(message);
    mailer
        .send(to, &subject, body)
        .await
        .map_err(|_| SendError::retry(EMAIL_NOT_DELIVERED.to_string()))
}

/// "Send test": a `NotifyEvent::Test` message to channel `id`, now (not
/// queued), enabled or not. The outer error is the database's.
pub async fn send_test(state: &AppState, id: Uuid) -> Result<Result<(), SendError>, sqlx::Error> {
    let channel = match channel::open_for_send(&state.db, &state.cipher, id).await? {
        Some(Ok(channel)) => channel,
        Some(Err(_)) => return Ok(Err(SendError::never(UNDECRYPTABLE))),
        None => return Ok(Err(SendError::never("channel not found"))),
    };
    let base = state.config.ui_base_url.trim_end_matches('/');
    let (project_id, project_name, url) = match channel.owner {
        Owner::Project(project) => (
            project,
            projects::find_by_id(&state.db, project).await?.map(|p| p.name).unwrap_or_default(),
            format!("{base}/projects/{project}/settings/notifications"),
        ),
        Owner::User(_) => (Uuid::nil(), "Personal channel".to_string(), format!("{base}/account/notifications")),
    };
    let message = Message {
        event: NotifyEvent::Test,
        incident: IncidentSummary {
            id: Uuid::nil(),
            title: "Test notification".into(),
            cause: Some("This channel works. No incident is open.".into()),
            status: "test".into(),
            source: "test".into(),
            severity: "test".into(),
            started_at: Utc::now(),
            details: None,
            resolution_reason: None,
        },
        project_id,
        project_name,
        incident_url: url,
        actions: None,
    };
    Ok(send_to_channel(&Senders::from_config(&state.config), &state.mailer, &channel, &message, Uuid::now_v7()).await)
}

/// R12: 2xx is success; 429 waits for `Retry-After` (at most an hour); 408
/// and 5xx are retried; anything else (other 4xx, an unfollowed 3xx) is not.
fn classify(status: StatusCode, retry_after: Option<Duration>) -> Result<(), SendError> {
    if status.is_success() {
        return Ok(());
    }
    let retry = match status.as_u16() {
        429 => Retry::After(retry_after.map(|wait| wait.min(MAX_RETRY_AFTER))),
        408 | 500..=599 => Retry::After(None),
        _ => Retry::Never,
    };
    Err(SendError { message: format!("HTTP {}", status.as_u16()), retry })
}

/// `Retry-After` in seconds (the HTTP-date form is ignored).
fn retry_after(headers: &header::HeaderMap) -> Option<Duration> {
    let seconds = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim().parse().ok()?;
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_are_classified_for_retry() {
        let class = |code: u16, wait: Option<u64>| {
            classify(StatusCode::from_u16(code).unwrap(), wait.map(Duration::from_secs)).map_err(|e| e.retry)
        };
        assert_eq!(class(204, None), Ok(()));
        assert_eq!(class(429, Some(7200)), Err(Retry::After(Some(MAX_RETRY_AFTER))));
        assert_eq!(class(429, Some(90)), Err(Retry::After(Some(Duration::from_secs(90)))));
        assert_eq!(class(429, None), Err(Retry::After(None)));
        assert_eq!(class(503, None), Err(Retry::After(None)));
        assert_eq!(class(408, None), Err(Retry::After(None)));
        assert_eq!(class(404, None), Err(Retry::Never));
        assert_eq!(class(302, None), Err(Retry::Never));
        assert_eq!(
            classify(StatusCode::BAD_GATEWAY, None).unwrap_err().message,
            "HTTP 502"
        );
    }

    #[test]
    fn retry_after_is_read_in_seconds_only() {
        let mut headers = header::HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert(header::RETRY_AFTER, "120".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(120)));
        headers.insert(header::RETRY_AFTER, "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap());
        assert_eq!(retry_after(&headers), None);
    }
}
