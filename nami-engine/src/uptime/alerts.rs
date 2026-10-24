//! Incident notifications. `notify_transition` is the single entry point the
//! uptime worker, the heartbeat endpoint and the log alert worker call; the
//! incident and its notifications are written by `incidents::lifecycle` in
//! the transaction that changed the source's state.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::checker::is_unsafe_in_text;
use crate::core::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    Down,
    Recovered,
}

/// What changed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subject {
    Monitor(Uuid),
    Heartbeat(Uuid),
    LogAlert(Uuid),
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

/// Incidents are opened and resolved, and their notifications queued, inside
/// the transaction that changed the source's state (`incidents::lifecycle`).
/// This only wakes the incidents worker so they go out now instead of at its
/// next tick (`incidents::worker::kick`, bounded and never failing).
pub async fn notify_transition(state: &AppState, _transition: &Transition) {
    crate::incidents::worker::kick(state).await;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
