//! Notification messages per channel kind (email, Slack, Discord, Telegram,
//! Teams, webhook), with every piece of user text escaped for the channel it
//! goes to, and the HMAC signature of webhook bodies (R29). Pure: no I/O.
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;

use crate::incidents::model::MAX_TEXT_BYTES;
use crate::uptime::{checker::is_unsafe_in_text, heartbeat::sanitize_output};

/// Chars of a cause a chat message carries; longer causes are cut and end in `…`.
pub const MAX_CHAT_CAUSE_CHARS: usize = 1000;
/// Discord's cap on a message's `content`.
pub const MAX_DISCORD_CHARS: usize = 2000;
/// Telegram's cap on a message's `text`.
pub const MAX_TELEGRAM_CHARS: usize = 4096;

/// What a notification is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyEvent {
    Triggered,
    Resolved,
    Test,
}

impl NotifyEvent {
    /// The `event` field of a webhook body.
    pub fn webhook_name(self) -> &'static str {
        match self {
            NotifyEvent::Triggered => "incident.triggered",
            NotifyEvent::Resolved => "incident.resolved",
            NotifyEvent::Test => "test",
        }
    }
}

/// The incident fields a notification shows. Text fields are untrusted.
#[derive(Debug, Clone)]
pub struct IncidentSummary {
    pub id: Uuid,
    pub title: String,
    pub cause: Option<String>,
    pub status: String,
    pub source: String,
    pub severity: String,
    pub started_at: DateTime<Utc>,
    /// `incidents.details` (heartbeat output, R26): email only.
    pub details: Option<String>,
    /// R30: the latest `resolved` event's reason ("alert disabled"), `Resolved` messages only.
    pub resolution_reason: Option<String>,
}

/// The recipient's own single-use acknowledge/resolve links (email only, R16).
#[derive(Debug, Clone)]
pub struct ActionLinks {
    pub acknowledge: String,
    pub resolve: String,
}

/// One notification, before it is formatted for a channel. `incident_url`
/// is built from ids under the operator's `UI_BASE_URL`, so it is trusted.
#[derive(Debug, Clone)]
pub struct Message {
    pub event: NotifyEvent,
    pub incident: IncidentSummary,
    pub project_id: Uuid,
    pub project_name: String,
    pub incident_url: String,
    pub actions: Option<ActionLinks>,
}

/// Text as one line: every char that could start a line of its own or
/// reorder how the text reads becomes a space.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if is_unsafe_in_text(c) { ' ' } else { c })
        .collect()
}

/// A cause keeps its line breaks and loses every other unsafe char.
fn clean_cause(s: &str) -> String {
    s.chars()
        .filter(|&c| c == '\n' || !is_unsafe_in_text(c))
        .collect()
}

/// At most `max` chars; a cut text ends in `…` (counted in `max`).
fn cut_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// At most `max` bytes, cut at a char boundary.
fn cut_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

/// `s` escaped with `escape`, cut on the raw text so the escaped result is at
/// most `max` chars (ending in `…` when cut). Cutting the escaped output
/// instead could split an entity or leave a dangling backslash.
fn escape_within(s: &str, escape: fn(&str) -> String, max: usize) -> String {
    let full = escape(s);
    if full.chars().count() <= max {
        return full;
    }
    let mut out = String::new();
    let mut used = 0;
    let mut buf = [0u8; 4];
    for c in s.chars() {
        let piece = escape(c.encode_utf8(&mut buf));
        let n = piece.chars().count();
        if used + n + 1 > max {
            break;
        }
        out.push_str(&piece);
        used += n;
    }
    if max > 0 {
        out.push('…');
    }
    out
}

/// Message text cleaned once: one-line fields, the cause with its line
/// breaks, and the chat-length cause.
struct Parts {
    headline: String,
    title: String,
    project: String,
    severity: String,
    status: String,
    reason: Option<String>,
    cause: Option<String>,
    chat_cause: Option<String>,
}

fn parts(m: &Message) -> Parts {
    let i = &m.incident;
    let title = one_line(&i.title);
    let severity = one_line(&i.severity);
    let headline = match m.event {
        NotifyEvent::Triggered => format!("[{severity}] {title}"),
        NotifyEvent::Resolved => format!("Resolved: {title}"),
        NotifyEvent::Test => "Test notification from Nami".to_string(),
    };
    let reason = match m.event {
        NotifyEvent::Resolved => i.resolution_reason.as_deref().map(one_line),
        _ => None,
    };
    let cause = i
        .cause
        .as_deref()
        .map(|c| clean_cause(cut_bytes(c, MAX_TEXT_BYTES)))
        .filter(|c| !c.is_empty());
    let chat_cause = cause.as_deref().map(|c| cut_chars(c, MAX_CHAT_CAUSE_CHARS));
    Parts {
        headline,
        title,
        project: one_line(&m.project_name),
        severity,
        status: one_line(&i.status),
        reason,
        cause,
        chat_cause,
    }
}

/// The chat second line: `project · severity · status[ · Reason: reason]`,
/// each value escaped with `escape`.
fn summary_line(p: &Parts, escape: fn(&str) -> String) -> String {
    let mut line = format!(
        "{} · {} · {}",
        escape(&p.project),
        escape(&p.severity),
        escape(&p.status)
    );
    if let Some(reason) = &p.reason {
        line.push_str(" · Reason: ");
        line.push_str(&escape(reason));
    }
    line
}

/// (subject, plain-text body). The body is sent as `text/plain`, so markup is
/// inert; every line after a cause's first is indented by two spaces, so user
/// text never starts a line and cannot fake a `View:` or action link.
pub fn email(m: &Message) -> (String, String) {
    let p = parts(m);
    let subject = match m.event {
        NotifyEvent::Triggered => format!("[Nami] [{}] {}", p.severity, p.title),
        NotifyEvent::Resolved => format!("[Nami] Resolved: {}", p.title),
        NotifyEvent::Test => "[Nami] Test notification".to_string(),
    };
    let mut body = format!(
        "{}\n\nProject: {}\nSeverity: {}\nStatus: {}\n",
        p.headline, p.project, p.severity, p.status
    );
    if let Some(reason) = &p.reason {
        body.push_str(&format!("Reason: {reason}\n"));
    }
    body.push_str(&format!(
        "Started: {}\nCause: {}\n",
        m.incident.started_at.format("%Y-%m-%d %H:%M:%S UTC"),
        p.cause.as_deref().unwrap_or("unknown").replace('\n', "\n  ")
    ));
    if let Some(details) = m.incident.details.as_deref().filter(|d| !d.is_empty()) {
        body.push_str("Output:\n");
        for line in sanitize_output(details.as_bytes()).lines() {
            body.push_str("> ");
            body.push_str(line);
            body.push('\n');
        }
    }
    body.push_str(&format!("\nView: {}\n", m.incident_url));
    if let Some(a) = &m.actions {
        body.push_str(&format!(
            "\nAcknowledge: {}\nResolve: {}\n",
            a.acknowledge, a.resolve
        ));
    }
    (subject, body)
}

fn mrkdwn(text: String) -> Value {
    json!({"type": "section", "text": {"type": "mrkdwn", "text": text}})
}

/// A Slack incoming-webhook payload. The only `<url|…>` link is the incident.
pub fn slack(m: &Message) -> Value {
    let p = parts(m);
    let headline = escape_slack(&p.headline);
    let mut blocks = vec![mrkdwn(format!(
        "*{headline}*\n{}",
        summary_line(&p, escape_slack)
    ))];
    if let Some(cause) = &p.chat_cause {
        blocks.push(mrkdwn(escape_slack(cause)));
    }
    blocks.push(mrkdwn(format!("<{}|View incident>", m.incident_url)));
    json!({
        "text": headline,
        "unfurl_links": false,
        "unfurl_media": false,
        "blocks": blocks,
    })
}

/// A Discord webhook payload that parses no mentions, at most
/// [`MAX_DISCORD_CHARS`] chars, the incident link always kept.
pub fn discord(m: &Message) -> Value {
    let p = parts(m);
    let mut content = format!(
        "{}\n{}\n",
        escape_discord(&p.headline),
        summary_line(&p, escape_discord)
    );
    if let Some(cause) = &p.chat_cause {
        let room = MAX_DISCORD_CHARS
            .saturating_sub(content.chars().count() + m.incident_url.chars().count() + 1);
        let cause = escape_within(cause, escape_discord, room);
        if !cause.is_empty() {
            content.push_str(&cause);
            content.push('\n');
        }
    }
    content.push_str(&m.incident_url);
    json!({"content": cut_chars(&content, MAX_DISCORD_CHARS), "allowed_mentions": {"parse": []}})
}

/// A Telegram `sendMessage` body in HTML parse mode, at most
/// [`MAX_TELEGRAM_CHARS`] chars, the incident link always kept.
pub fn telegram(m: &Message, chat_id: &str) -> Value {
    let p = parts(m);
    let link = format!(
        "<a href=\"{}\">View incident</a>",
        escape_telegram(&m.incident_url)
    );
    let mut text = format!(
        "<b>{}</b>\n{}\n",
        escape_telegram(&p.headline),
        summary_line(&p, escape_telegram)
    );
    if let Some(cause) = &p.chat_cause {
        let room =
            MAX_TELEGRAM_CHARS.saturating_sub(text.chars().count() + link.chars().count() + 1);
        let cause = escape_within(cause, escape_telegram, room);
        if !cause.is_empty() {
            text.push_str(&cause);
            text.push('\n');
        }
    }
    text.push_str(&link);
    json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
        "disable_web_page_preview": true,
    })
}

/// A Teams workflow payload: an Adaptive Card v1.4 whose user text sits only
/// in `TextBlock`s and whose only link is an `Action.OpenUrl` to the incident.
pub fn teams(m: &Message) -> Value {
    let p = parts(m);
    let mut body = vec![
        json!({"type": "TextBlock", "text": escape_teams(&p.headline), "weight": "bolder", "wrap": true}),
        json!({"type": "TextBlock", "text": summary_line(&p, escape_teams), "wrap": true}),
    ];
    if let Some(cause) = &p.chat_cause {
        body.push(json!({"type": "TextBlock", "text": escape_teams(cause), "wrap": true}));
    }
    json!({
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": {
                "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                "type": "AdaptiveCard",
                "version": "1.4",
                "body": body,
                "actions": [{"type": "Action.OpenUrl", "title": "View incident", "url": m.incident_url}],
            },
        }],
    })
}

/// The webhook body, serialised once: these bytes are both sent and signed.
/// `{event, incident:{id,title,cause,status,source,severity,started_at,url}, project:{id,name}}`.
pub fn webhook_body(m: &Message) -> Vec<u8> {
    let p = parts(m);
    let i = &m.incident;
    let body = json!({
        "event": m.event.webhook_name(),
        "incident": {
            "id": i.id,
            "title": p.title,
            "cause": p.cause,
            "status": p.status,
            "source": one_line(&i.source),
            "severity": p.severity,
            "started_at": i.started_at,
            "url": m.incident_url,
        },
        "project": {"id": m.project_id, "name": p.project},
    });
    serde_json::to_vec(&body).expect("a JSON value always serialises")
}

/// R29: "sha256=" + lowercase hex HMAC-SHA256(secret, "{timestamp}." + body).
pub fn signature(secret: &[u8], timestamp: i64, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes a key of any length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Lowercase hex HMAC-SHA256(secret, bytes); `signature` is built on it.
pub fn hmac_hex(secret: &[u8], bytes: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes a key of any length");
    mac.update(bytes);
    hex::encode(mac.finalize().into_bytes())
}

/// Slack mrkdwn: `&`, `<`, `>` as entities, so `<!channel>`, `<@U…>`, `<#C…>`
/// and `<http://x|y>` stay plain text.
pub fn escape_slack(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// Discord markdown: a backslash before each markdown, mention, link and
/// emoji char (`\ * _ ~ ` | > # - [ ] ( ) @ : <`).
pub fn escape_discord(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '#' | '-' | '[' | ']' | '(' | ')' | '@' | ':' | '<'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Telegram HTML: `&`, `<`, `>` and `"` (for the `href` attribute) as entities.
pub fn escape_telegram(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// Adaptive Card text: a backslash before each markdown char
/// (`\ * _ [ ] ( ) ` # - !`) and `&`, `<`, `>` as entities, so no link,
/// `<at>` mention or formatting can be injected. One pass per char, so an
/// entity is never escaped again.
pub fn escape_teams(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\\' | '*' | '_' | '[' | ']' | '(' | ')' | '`' | '#' | '-' | '!' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(title: &str, cause: Option<&str>) -> Message {
        Message {
            event: NotifyEvent::Triggered,
            incident: IncidentSummary {
                id: Uuid::nil(),
                title: title.into(),
                cause: cause.map(Into::into),
                status: "open".into(),
                source: "monitor".into(),
                severity: "critical".into(),
                started_at: DateTime::from_timestamp(1_792_000_000, 0).unwrap(),
                details: None,
                resolution_reason: None,
            },
            project_id: Uuid::nil(),
            project_name: "Shop".into(),
            incident_url: format!("http://ui.test/projects/{0}/incidents/{0}", Uuid::nil()),
            actions: None,
        }
    }

    const HOSTILE: &str = "<!channel> <@U123> @everyone @here <script>alert(1)</script> *bold* [x](http://evil.test) <at>ceo</at> &amp;";

    #[test]
    fn email_subject_and_body_carry_severity_and_links_on_single_lines() {
        let mut m = message("API\r\nBcc: x@evil.test", Some("refused"));
        m.actions = Some(ActionLinks {
            acknowledge: "http://ui.test/incidents/action?token=a".into(),
            resolve: "http://ui.test/incidents/action?token=r".into(),
        });
        let (subject, body) = email(&m);
        assert_eq!(subject, "[Nami] [critical] API  Bcc: x@evil.test");
        assert!(
            body.starts_with(
                "[critical] API  Bcc: x@evil.test\n\nProject: Shop\nSeverity: critical\n"
            ),
            "{body}"
        );
        assert!(
            body.contains("Started: 2026-10-14 17:46:40 UTC\nCause: refused\n"),
            "{body}"
        );
        assert!(
            body.contains(
                "\nAcknowledge: http://ui.test/incidents/action?token=a\nResolve: http://ui.test/incidents/action?token=r"
            ),
            "{body}"
        );
        let resolved = Message {
            event: NotifyEvent::Resolved,
            ..message("API", None)
        };
        assert_eq!(email(&resolved).0, "[Nami] Resolved: API");
    }

    #[test]
    fn a_cause_cannot_forge_a_link_line_and_job_output_is_quoted() {
        let mut m = message(
            "Nightly backup reported a failure",
            Some("boom\nAcknowledge: http://evil.test/x\r\nView: http://evil.test"),
        );
        m.incident.details =
            Some("<script>alert(1)</script>\nBcc: x@evil.test\n\u{1b}[31mdisk full".into());
        let (_, body) = email(&m);
        for line in body.lines() {
            assert!(
                !line.starts_with("Acknowledge: http://evil")
                    && !line.starts_with("View: http://evil"),
                "{body}"
            );
        }
        assert!(
            body.contains("Cause: boom\n  Acknowledge: http://evil.test/x"),
            "{body}"
        );
        assert!(
            body.contains(
                "Output:\n> <script>alert(1)</script>\n> Bcc: x@evil.test\n> [31mdisk full\n"
            ),
            "{body}"
        );
        assert!(
            body.find("Output:").unwrap() < body.find("\nView: ").unwrap(),
            "{body}"
        );
    }

    #[test]
    fn slack_text_cannot_mention_or_link() {
        let v = slack(&message(HOSTILE, Some(HOSTILE)));
        let text = v.to_string();
        for bad in ["<!channel>", "<@U123>", "<script>", "<http://evil"] {
            assert!(!text.contains(bad), "{bad} in {text}");
        }
        assert!(text.contains("&lt;!channel&gt;"));
        assert!(text.contains("&amp;amp;"));
        let links: Vec<_> = text.match_indices("<http").collect();
        assert_eq!(links.len(), 1, "only the incident link: {text}");
    }

    #[test]
    fn discord_never_parses_mentions_and_escapes_markdown() {
        let v = discord(&message(HOSTILE, None));
        assert_eq!(v["allowed_mentions"], serde_json::json!({"parse": []}));
        let content = v["content"].as_str().unwrap();
        assert!(content.contains("\\*bold\\*"), "{content}");
        assert!(content.contains("\\[x\\]\\(http\\:"), "{content}");
        assert!(content.contains("\\@everyone"), "{content}");
        assert!(content.chars().count() <= 2000);
        let long = discord(&message(&"x".repeat(300), Some(&"y".repeat(5000))));
        assert!(long["content"].as_str().unwrap().chars().count() <= 2000);
    }

    #[test]
    fn telegram_uses_html_with_everything_escaped() {
        let v = telegram(&message(HOSTILE, Some("a < b && c > d")), "-100");
        assert_eq!(v["parse_mode"], "HTML");
        assert_eq!(v["chat_id"], "-100");
        assert_eq!(v["disable_web_page_preview"], true);
        let text = v["text"].as_str().unwrap();
        assert!(!text.contains("<script>") && !text.contains("<at>"), "{text}");
        assert!(text.contains("a &lt; b &amp;&amp; c &gt; d"), "{text}");
        assert_eq!(text.matches("<a href=").count(), 1);
    }

    #[test]
    fn teams_is_an_adaptive_card_whose_only_link_is_the_incident() {
        let v = teams(&message(HOSTILE, Some(HOSTILE)));
        let card = &v["attachments"][0]["content"];
        assert_eq!(card["type"], "AdaptiveCard");
        assert_eq!(card["version"], "1.4");
        let blocks = card["body"].as_array().unwrap();
        assert!(blocks.iter().all(|b| b["type"] == "TextBlock"));
        let texts: String = blocks.iter().map(|b| b["text"].as_str().unwrap()).collect();
        for bad in ["<at>", "<script>", "[x](", "*bold*"] {
            assert!(!texts.contains(bad), "{bad} in {texts}");
        }
        assert!(texts.contains("\\[x\\]\\(http"));
        let actions = card["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["type"], "Action.OpenUrl");
        assert_eq!(
            actions[0]["url"],
            format!("http://ui.test/projects/{0}/incidents/{0}", Uuid::nil())
        );
    }

    #[test]
    fn every_escaper_handles_each_control_character() {
        assert_eq!(escape_slack("a&<>b"), "a&amp;&lt;&gt;b");
        assert_eq!(escape_telegram("a&<>\"b"), "a&amp;&lt;&gt;&quot;b");
        assert_eq!(
            escape_teams("*_[]()`#>-!&<>\\"),
            "\\*\\_\\[\\]\\(\\)\\`\\#&gt;\\-\\!&amp;&lt;&gt;\\\\"
        );
        assert_eq!(escape_discord("@here *x*"), "\\@here \\*x\\*");
    }

    #[test]
    fn webhook_bodies_are_signed_with_hmac_sha256_over_the_exact_bytes() {
        let body = webhook_body(&message("API down", Some("refused")));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["event"], "incident.triggered");
        assert_eq!(v["incident"]["severity"], "critical");
        assert_eq!(v["project"]["name"], "Shop");
        assert_eq!(v.as_object().unwrap().len(), 3);
        // RFC 4231 test case 2.
        assert_eq!(
            hmac_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // R29: the signed bytes are "{timestamp}.{body}", never the body alone.
        let mut signed = b"1792000000.".to_vec();
        signed.extend_from_slice(&body);
        assert_eq!(
            signature(b"k", 1_792_000_000, &body),
            format!("sha256={}", hmac_hex(b"k", &signed))
        );
        assert_ne!(
            signature(b"k", 1_792_000_000, &body),
            signature(b"k", 1_792_000_001, &body),
            "the timestamp is signed"
        );
        assert_ne!(
            signature(b"k", 1_792_000_000, &body),
            format!("sha256={}", hmac_hex(b"k", &body))
        );
    }

    #[test]
    fn a_resolution_reason_reaches_every_format_escaped() {
        let mut m = Message {
            event: NotifyEvent::Resolved,
            ..message("Log alert: Checkout errors", None)
        };
        m.incident.resolution_reason = Some("alert disabled".into());
        let (subject, body) = email(&m);
        assert_eq!(subject, "[Nami] Resolved: Log alert: Checkout errors");
        assert!(body.contains("Status: open\nReason: alert disabled\n"), "{body}");
        assert!(slack(&m).to_string().contains("Reason: alert disabled"));
        assert!(
            discord(&m)["content"]
                .as_str()
                .unwrap()
                .contains("Reason: alert disabled")
        );
        assert!(
            telegram(&m, "1")["text"]
                .as_str()
                .unwrap()
                .contains("Reason: alert disabled")
        );
        assert!(teams(&m).to_string().contains("Reason: alert disabled"));
        let v: serde_json::Value = serde_json::from_slice(&webhook_body(&m)).unwrap();
        assert!(
            v["incident"].get("reason").is_none(),
            "the webhook body stays as the spec fixes it"
        );
    }

    #[test]
    fn a_chat_cause_is_cut_at_its_limit_and_escaping_never_pushes_out_the_link() {
        let at = "y".repeat(MAX_CHAT_CAUSE_CHARS);
        let past = "y".repeat(MAX_CHAT_CAUSE_CHARS + 1);
        let block = |cause: &str| {
            teams(&message("API", Some(cause)))["attachments"][0]["content"]["body"][2]["text"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(block(&at), at);
        assert_eq!(block(&past), format!("{}…", "y".repeat(MAX_CHAT_CAUSE_CHARS - 1)));

        // Every char of this cause grows when escaped, past both caps.
        let m = message(&"&".repeat(255), Some(&"\"&<>".repeat(400)));
        let url = m.incident_url.clone();
        let text = telegram(&m, "1")["text"].as_str().unwrap().to_string();
        assert!(text.chars().count() <= MAX_TELEGRAM_CHARS, "{}", text.len());
        assert!(text.ends_with(&format!("<a href=\"{url}\">View incident</a>")));
        assert!(!text.contains("&am…") && !text.contains("&qu…"), "an entity was split");
        let m = message(&"*".repeat(255), Some(&"*@".repeat(1000)));
        let content = discord(&m)["content"].as_str().unwrap().to_string();
        assert!(content.chars().count() <= MAX_DISCORD_CHARS);
        assert!(content.ends_with(&format!("\n{url}")), "{content}");
        assert!(!content.contains("\\…"), "a backslash was left dangling");
    }
}
