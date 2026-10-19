//! Incident notifications. `notify_transition` is the single entry point:
//! the Incident Management sub-project replaces its body (escalation
//! policies, webhook/Slack/Telegram/Discord channels) without touching the
//! worker.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{checker::is_unsafe_in_text, heartbeat::sanitize_output};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    Down,
    Recovered,
}

/// What changed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Monitor(Uuid),
    Heartbeat(Uuid),
}

/// A state change worth telling people about: something went down (an
/// incident opened) or came back up (its incident resolved).
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub kind: TransitionKind,
    pub subject: Subject,
    pub project_id: Uuid,
    pub name: String,
    pub cause: Option<String>,
    /// Output of the heartbeat ping that opened the incident (already
    /// sanitized); only heartbeat failure transitions carry it.
    pub details: Option<String>,
    pub at: DateTime<Utc>,
}

/// Emails every member of the transition's project. Failures are logged and
/// swallowed: alert delivery must never abort the worker or undo the state
/// change that triggered it.
///
/// A failed send is logged once per transition, with the monitor and the kind
/// of transition but without the recipient's address: `Mailer::send` has
/// already logged the cause itself, and an address is personal data that has
/// no business in a log.
pub async fn notify_transition(state: &AppState, transition: &Transition) {
    let recipients: Vec<String> = match sqlx::query_scalar(
        "select u.email from project_members pm
         join users u on u.id = pm.user_id
         where pm.project_id = $1
         order by u.email",
    )
    .bind(transition.project_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(recipients) => recipients,
        Err(e) => {
            tracing::error!(
                error = %e,
                kind = ?transition.kind,
                subject = ?transition.subject,
                project_id = %transition.project_id,
                "failed to load alert recipients"
            );
            return;
        }
    };
    let (subject, body) = render(&state.config.ui_base_url, transition);
    let mut failed = 0;
    for recipient in &recipients {
        if state
            .mailer
            .send(recipient, &subject, body.clone())
            .await
            .is_err()
        {
            failed += 1;
        }
    }
    if failed > 0 {
        tracing::error!(
            kind = ?transition.kind,
            subject = ?transition.subject,
            failed,
            recipients = recipients.len(),
            "alert email not delivered to every recipient"
        );
    }
}

/// Text a monitored server can influence (a check's error message) as one
/// line of plain text: every control character (NUL, CR, LF, ESC, DEL, ...),
/// line or paragraph separator and bidirectional control becomes a space (see
/// [`is_unsafe_in_text`]) and everything else is kept. Postgres text cannot
/// hold NUL, a line break would let the text start lines of its own in the
/// email body, and a direction control would make it read as something else.
/// The checker already cleans its errors this way; this covers a cause that
/// reaches a transition any other way.
pub fn sanitize_cause(cause: &str) -> String {
    cause
        .chars()
        .map(|c| if is_unsafe_in_text(c) { ' ' } else { c })
        .collect()
}

/// Subject and plain-text body for a transition's email.
pub fn render(ui_base_url: &str, transition: &Transition) -> (String, String) {
    // Names are validated to contain no control, separator or direction
    // characters; stripping them again here keeps a header-injection attempt
    // out of the subject even for rows written some other way.
    let name: String = transition
        .name
        .chars()
        .filter(|c| !is_unsafe_in_text(*c))
        .collect();
    let base = ui_base_url.trim_end_matches('/');
    let link = match transition.subject {
        Subject::Monitor(id) => format!("{base}/projects/{}/monitors/{id}", transition.project_id),
        Subject::Heartbeat(id) => {
            format!("{base}/projects/{}/heartbeats/{id}", transition.project_id)
        }
    };
    let at = transition.at.format("%Y-%m-%d %H:%M:%S UTC");
    // Quoted line by line, so no line of a job's output can start a
    // header-looking line of its own. Sanitized again for a Transition built
    // any other way.
    let output = match transition.details.as_deref() {
        Some(details) if !details.is_empty() => {
            let mut quoted = String::from("\nOutput:\n");
            for line in sanitize_output(details.as_bytes()).lines() {
                quoted.push_str("> ");
                quoted.push_str(line);
                quoted.push('\n');
            }
            quoted
        }
        _ => String::new(),
    };
    match transition.kind {
        TransitionKind::Down => (
            format!("[Nami] {name} is DOWN"),
            format!(
                "{name} is DOWN.\n\nCause: {}\nSince: {at}\n{output}\nDetails: {link}\n",
                // Already one line when it comes from `apply_outcome`; the
                // same filter here covers a Transition built any other way.
                transition
                    .cause
                    .as_deref()
                    .map(sanitize_cause)
                    .as_deref()
                    .unwrap_or("unknown")
            ),
        ),
        TransitionKind::Recovered => (
            format!("[Nami] {name} is back UP"),
            format!("{name} is back UP.\n\nRecovered at: {at}\n\nDetails: {link}\n"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transition(kind: TransitionKind, name: &str) -> Transition {
        Transition {
            kind,
            subject: Subject::Monitor(Uuid::nil()),
            project_id: Uuid::nil(),
            name: name.to_string(),
            cause: Some("expected status 200-399, got 503".to_string()),
            details: None,
            at: DateTime::from_timestamp(1_792_000_000, 0).unwrap(),
        }
    }

    #[test]
    fn down_email_names_the_cause_time_and_link() {
        let (subject, body) = render(
            "http://ui.test/",
            &transition(TransitionKind::Down, "Checkout API"),
        );
        assert_eq!(subject, "[Nami] Checkout API is DOWN");
        assert!(
            body.contains("Cause: expected status 200-399, got 503"),
            "{body}"
        );
        assert!(body.contains("Since: 2026-10-14 17:46:40 UTC"), "{body}");
        let nil = Uuid::nil();
        assert!(
            body.contains(&format!("http://ui.test/projects/{nil}/monitors/{nil}")),
            "{body}"
        );
    }

    #[test]
    fn recovery_email_has_its_own_subject() {
        let (subject, body) = render(
            "http://ui.test",
            &transition(TransitionKind::Recovered, "Checkout API"),
        );
        assert_eq!(subject, "[Nami] Checkout API is back UP");
        assert!(
            body.contains("Recovered at: 2026-10-14 17:46:40 UTC"),
            "{body}"
        );
    }

    #[test]
    fn heartbeat_emails_link_to_the_heartbeat_page() {
        let heartbeat = Transition {
            subject: Subject::Heartbeat(Uuid::nil()),
            ..transition(TransitionKind::Down, "Nightly backup")
        };
        let (subject, body) = render("http://ui.test", &heartbeat);
        assert_eq!(subject, "[Nami] Nightly backup is DOWN");
        let nil = Uuid::nil();
        assert!(
            body.contains(&format!("http://ui.test/projects/{nil}/heartbeats/{nil}")),
            "{body}"
        );
    }

    #[test]
    fn heartbeat_output_is_quoted_line_by_line_between_since_and_details() {
        let down = Transition {
            subject: Subject::Heartbeat(Uuid::nil()),
            cause: Some("Heartbeat reported failure (exit code 2)".into()),
            details: Some("<script>alert(1)</script>\nBcc: x@evil.test\n\tdisk full".into()),
            ..transition(TransitionKind::Down, "Nightly backup")
        };
        let (subject, body) = render("http://ui.test", &down);
        assert_eq!(subject, "[Nami] Nightly backup is DOWN");
        assert!(body.contains(
            "Since: 2026-10-14 17:46:40 UTC\n\nOutput:\n> <script>alert(1)</script>\n> Bcc: x@evil.test\n> \tdisk full\n\nDetails: "
        ), "{body:?}");
        assert!(!body.lines().any(|l| l.starts_with("Bcc:")), "{body:?}");
    }

    #[test]
    fn details_are_sanitized_again_when_rendered() {
        let down = Transition {
            details: Some("a\rb\u{1b}c".into()),
            ..transition(TransitionKind::Down, "X")
        };
        let (_, body) = render("http://ui.test", &down);
        assert!(body.contains("Output:\n> abc\n"), "{body:?}");
    }

    #[test]
    fn control_characters_never_reach_the_subject() {
        let (subject, _) = render(
            "http://ui.test",
            &transition(TransitionKind::Down, "API\r\nBcc: x@evil.test"),
        );
        assert_eq!(subject, "[Nami] APIBcc: x@evil.test is DOWN");
    }

    #[test]
    fn control_characters_never_reach_the_first_line_of_the_body() {
        let (_, body) = render(
            "http://ui.test",
            &transition(TransitionKind::Down, "API\r\nBcc: x@evil.test"),
        );
        assert!(
            body.starts_with("APIBcc: x@evil.test is DOWN.\n\nCause:"),
            "{body:?}"
        );
    }

    #[test]
    fn a_missing_cause_reads_unknown() {
        let mut down = transition(TransitionKind::Down, "Checkout API");
        down.cause = None;
        let (_, body) = render("http://ui.test", &down);
        assert!(body.contains("Cause: unknown\n"), "{body}");
    }

    /// The cause is partly chosen by the monitored server. It reaches the
    /// email as one line of plain text: nothing in it can start a line of its
    /// own, rewrite the subject or the link, or turn into markup.
    #[test]
    fn a_hostile_cause_becomes_one_line_of_plain_text() {
        let hostile = "x\r\nSubject: pwned\r\nBcc: evil@evil.test\r\n\r\n<script>alert(1)</script>";
        let mut down = transition(TransitionKind::Down, "Checkout API");
        down.cause = Some(hostile.to_string());

        let (subject, body) = render("http://ui.test", &down);

        assert_eq!(subject, "[Nami] Checkout API is DOWN");
        assert!(
            body.contains(
                "Cause: x  Subject: pwned  Bcc: evil@evil.test    <script>alert(1)</script>\nSince:"
            ),
            "{body:?}"
        );
        let nil = Uuid::nil();
        assert!(
            body.ends_with(&format!(
                "\nDetails: http://ui.test/projects/{nil}/monitors/{nil}\n"
            )),
            "the link is the last thing in the body and is built from ids only: {body:?}"
        );
        assert_eq!(body.lines().count(), 6, "{body:?}");
    }

    #[test]
    fn every_control_character_in_a_cause_becomes_a_space() {
        let controls: String = ('\u{0}'..='\u{1f}').chain('\u{7f}'..='\u{9f}').collect();
        assert_eq!(
            sanitize_cause(&format!("a{controls}b")),
            format!("a{}b", " ".repeat(controls.chars().count()))
        );
        assert_eq!(sanitize_cause("a\0b\r\nc\u{1b}[31md"), "a b  c [31md");
    }

    #[test]
    fn separators_and_direction_controls_in_a_cause_become_spaces() {
        assert_eq!(
            sanitize_cause("a\u{2028}b\u{2029}c\u{202E}d\u{2066}e\u{200E}f"),
            "a b c d e f"
        );
        let (subject, body) = render(
            "http://ui.test",
            &Transition {
                cause: Some("evil\u{2028}Bcc: x@evil.test".to_string()),
                ..transition(TransitionKind::Down, "API\u{202E}gnp.exe\u{2029}")
            },
        );
        assert_eq!(subject, "[Nami] APIgnp.exe is DOWN");
        assert!(body.contains("Cause: evil Bcc: x@evil.test\n"), "{body:?}");
    }

    #[test]
    fn printable_text_is_left_alone() {
        for text in [
            "",
            "connection refused",
            "expected status 200-399, got 503",
            "certificate not valid for name \"a.test\"; only valid for DnsName(\"b.test\")",
            "gagal: koneksi ditolak \u{2026} 日本語 \u{1f980}",
        ] {
            assert_eq!(sanitize_cause(text), text);
        }
    }

    #[test]
    fn trailing_slashes_on_the_ui_base_url_never_double_up() {
        let (_, body) = render(
            "http://ui.test///",
            &transition(TransitionKind::Recovered, "Checkout API"),
        );
        let nil = Uuid::nil();
        assert!(
            body.contains(&format!(
                "Details: http://ui.test/projects/{nil}/monitors/{nil}\n"
            )),
            "{body}"
        );
    }
}
