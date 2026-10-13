//! Incident notifications. `notify_transition` is the single entry point:
//! the Incident Management sub-project replaces its body (escalation
//! policies, webhook/Slack/Telegram/Discord channels) without touching the
//! worker.

use chrono::{DateTime, Utc};
use uuid::Uuid;

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
    pub at: DateTime<Utc>,
}

/// Emails every member of the transition's project. Failures are logged and
/// swallowed: alert delivery must never abort the worker or undo the state
/// change that triggered it.
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
            tracing::error!(error = %e, project_id = %transition.project_id, "failed to load alert recipients");
            return;
        }
    };
    let (subject, body) = render(&state.config.ui_base_url, transition);
    for recipient in recipients {
        if let Err(e) = state.mailer.send(&recipient, &subject, body.clone()).await {
            tracing::error!(error = ?e, %recipient, "failed to send alert email");
        }
    }
}

/// Subject and plain-text body for a transition's email.
pub fn render(ui_base_url: &str, transition: &Transition) -> (String, String) {
    // Names are validated to contain no control characters; stripping them
    // again here keeps a header-injection attempt out of the subject even
    // for rows written some other way.
    let name: String = transition
        .name
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let base = ui_base_url.trim_end_matches('/');
    let link = match transition.subject {
        Subject::Monitor(id) => format!("{base}/projects/{}/monitors/{id}", transition.project_id),
    };
    let at = transition.at.format("%Y-%m-%d %H:%M:%S UTC");
    match transition.kind {
        TransitionKind::Down => (
            format!("[Nami] {name} is DOWN"),
            format!(
                "{name} is DOWN.\n\nCause: {}\nSince: {at}\n\nDetails: {link}\n",
                transition.cause.as_deref().unwrap_or("unknown")
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

    /// The cause is partly chosen by the monitored server. It is only ever
    /// text inside a plain-text body: nothing rewrites the subject, the link
    /// or the rest of the message, and nothing is turned into markup.
    #[test]
    fn a_hostile_cause_stays_plain_text_inside_the_body() {
        let hostile = "x\r\nSubject: pwned\r\nBcc: evil@evil.test\r\n\r\n<script>alert(1)</script>";
        let mut down = transition(TransitionKind::Down, "Checkout API");
        down.cause = Some(hostile.to_string());

        let (subject, body) = render("http://ui.test", &down);

        assert_eq!(subject, "[Nami] Checkout API is DOWN");
        assert!(body.contains(&format!("Cause: {hostile}\n")), "{body:?}");
        let nil = Uuid::nil();
        assert!(
            body.ends_with(&format!(
                "\nDetails: http://ui.test/projects/{nil}/monitors/{nil}\n"
            )),
            "the link is the last thing in the body and is built from ids only: {body:?}"
        );
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
