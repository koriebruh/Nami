//! Flap control for alert emails: at most one DOWN email and one recovery
//! email per subject (monitor or heartbeat) per [`ALERT_FLAP_WINDOW`], while
//! keeping the members' picture current (see the restart caveat below).
//!
//! The rules, per subject, in [`AlertThrottle::decide`]:
//!
//! - **Down**: sent when no DOWN email went out for the subject within the
//!   window. Otherwise it is held back as *pending* (the latest one wins).
//! - **Recovered**: if a DOWN is pending, the members were never told about
//!   that outage, so both are dropped (the pending one is cancelled and the
//!   recovery suppressed): their last email already says UP, which is true
//!   again. Otherwise the recovery is sent, unless the last email sent was
//!   already a recovery (nothing to correct).
//! - A pending DOWN still pending once the window since the last DOWN email
//!   has passed is sent then ([`AlertThrottle::due`] and
//!   [`AlertThrottle::confirm_deferred`], driven by `alerts::run_deferred_alerts`,
//!   which every engine instance runs, worker or not), after checking that
//!   the subject is still down, unpaused and outside maintenance.
//!
//! So a subject that flaps without pause costs each member at most one
//! DOWN + recovery pair per window, and a subject that stays down is
//! reported at most one window late, as long as the process that held the
//! DOWN back keeps running.
//!
//! State is in memory, per engine process, like the ping rate limits. A
//! restart forgets it: the first transition after a restart is sent, and a
//! DOWN held back at the moment of a restart is lost (the subject shows as
//! down in the UI, but no email goes out for that outage).

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use super::alerts::{Subject, Transition, TransitionKind};

/// The flap window: at most one DOWN email and one recovery email per
/// subject within it.
pub const ALERT_FLAP_WINDOW: Duration = Duration::from_secs(5 * 60);

/// What to do with one transition's email.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Send,
    /// Held back or dropped; see the module docs.
    Suppress,
}

#[derive(Debug)]
struct Entry {
    /// The kind of the last email sent for the subject.
    last_sent: Option<TransitionKind>,
    /// When the last DOWN email went out.
    last_down_sent: Option<Instant>,
    /// A DOWN held back by the window, still to be sent if the subject stays
    /// down.
    pending: Option<Transition>,
    /// Last time the entry changed, for pruning.
    touched: Instant,
}

/// Per-subject alert state; see the module docs.
#[derive(Debug, Default)]
pub struct AlertThrottle {
    entries: Mutex<HashMap<Subject, Entry>>,
}

/// Entries kept at most before idle ones (nothing pending, untouched for a
/// window) are dropped. A dropped entry behaves like a fresh subject.
const PRUNE_ABOVE: usize = 10_000;

impl AlertThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `transition`'s email goes out now, judged at `now`; records
    /// the decision.
    pub fn decide(&self, transition: &Transition, now: Instant) -> Decision {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.len() > PRUNE_ABOVE {
            entries.retain(|_, entry| {
                entry.pending.is_some()
                    || now.saturating_duration_since(entry.touched) < ALERT_FLAP_WINDOW
            });
        }
        let entry = entries.entry(transition.subject).or_insert(Entry {
            last_sent: None,
            last_down_sent: None,
            pending: None,
            touched: now,
        });
        entry.touched = now;
        match transition.kind {
            TransitionKind::Down => {
                let in_window = entry
                    .last_down_sent
                    .is_some_and(|sent| now.saturating_duration_since(sent) < ALERT_FLAP_WINDOW);
                if in_window {
                    entry.pending = Some(transition.clone());
                    Decision::Suppress
                } else {
                    entry.pending = None;
                    entry.last_down_sent = Some(now);
                    entry.last_sent = Some(TransitionKind::Down);
                    Decision::Send
                }
            }
            TransitionKind::Recovered => {
                if entry.pending.take().is_some() {
                    return Decision::Suppress;
                }
                if entry.last_sent == Some(TransitionKind::Recovered) {
                    return Decision::Suppress;
                }
                entry.last_sent = Some(TransitionKind::Recovered);
                Decision::Send
            }
        }
    }

    /// The pending DOWN transitions whose window has passed by `now`. They
    /// stay pending until [`Self::confirm_deferred`].
    pub fn due(&self, now: Instant) -> Vec<Transition> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .values()
            .filter(|entry| {
                entry
                    .last_down_sent
                    .is_none_or(|sent| now.saturating_duration_since(sent) >= ALERT_FLAP_WINDOW)
            })
            .filter_map(|entry| entry.pending.clone())
            .collect()
    }

    /// Settles a pending DOWN that [`Self::due`] returned: when `still_down`
    /// and it is still pending (no recovery cancelled it meanwhile), it is
    /// recorded as sent and `true` comes back, so the caller sends it.
    /// Otherwise it is dropped and `false` comes back.
    pub fn confirm_deferred(
        &self,
        transition: &Transition,
        still_down: bool,
        now: Instant,
    ) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = entries.get_mut(&transition.subject) else {
            return false;
        };
        if entry.pending.as_ref() != Some(transition) {
            return false;
        }
        entry.pending = None;
        entry.touched = now;
        if !still_down {
            return false;
        }
        entry.last_down_sent = Some(now);
        entry.last_sent = Some(TransitionKind::Down);
        true
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;

    fn transition(kind: TransitionKind, subject: Subject) -> Transition {
        Transition {
            kind,
            subject,
            project_id: Uuid::nil(),
            name: "job".into(),
            cause: None,
            details: None,
            at: Utc::now(),
        }
    }

    const DOWN: TransitionKind = TransitionKind::Down;
    const UP: TransitionKind = TransitionKind::Recovered;

    #[test]
    fn a_flapping_subject_gets_one_pair_per_window() {
        let throttle = AlertThrottle::new();
        let subject = Subject::Heartbeat(Uuid::new_v4());
        let start = Instant::now();
        let mut sent = Vec::new();
        // 20 down/up cycles, one second apart.
        for i in 0..40u64 {
            let kind = if i % 2 == 0 { DOWN } else { UP };
            let now = start + Duration::from_secs(i);
            if throttle.decide(&transition(kind, subject), now) == Decision::Send {
                sent.push(kind);
            }
        }
        assert_eq!(sent, vec![DOWN, UP]);
        // Ended up: nothing is left to send.
        assert!(throttle.due(start + ALERT_FLAP_WINDOW * 2).is_empty());
    }

    #[test]
    fn a_new_window_sends_a_new_pair() {
        let throttle = AlertThrottle::new();
        let subject = Subject::Monitor(Uuid::new_v4());
        let start = Instant::now();
        assert_eq!(
            throttle.decide(&transition(DOWN, subject), start),
            Decision::Send
        );
        assert_eq!(
            throttle.decide(&transition(UP, subject), start + Duration::from_secs(60)),
            Decision::Send
        );
        let later = start + ALERT_FLAP_WINDOW + Duration::from_secs(1);
        assert_eq!(
            throttle.decide(&transition(DOWN, subject), later),
            Decision::Send
        );
        assert_eq!(
            throttle.decide(&transition(UP, subject), later),
            Decision::Send
        );
    }

    #[test]
    fn a_subject_that_stays_down_is_reported_once_the_window_ends() {
        let throttle = AlertThrottle::new();
        let subject = Subject::Heartbeat(Uuid::new_v4());
        let start = Instant::now();
        throttle.decide(&transition(DOWN, subject), start);
        throttle.decide(&transition(UP, subject), start + Duration::from_secs(1));
        let again = transition(DOWN, subject);
        assert_eq!(
            throttle.decide(&again, start + Duration::from_secs(2)),
            Decision::Suppress
        );
        assert!(
            throttle.due(start + Duration::from_secs(60)).is_empty(),
            "not yet"
        );
        let end = start + ALERT_FLAP_WINDOW;
        assert_eq!(throttle.due(end), vec![again.clone()]);
        assert!(throttle.confirm_deferred(&again, true, end));
        assert!(
            throttle.due(end + ALERT_FLAP_WINDOW).is_empty(),
            "sent once"
        );
        // Its recovery is sent: the last email said DOWN.
        assert_eq!(
            throttle.decide(&transition(UP, subject), end + Duration::from_secs(1)),
            Decision::Send
        );
    }

    #[test]
    fn a_deferred_down_is_dropped_when_the_subject_is_no_longer_down() {
        let throttle = AlertThrottle::new();
        let subject = Subject::Heartbeat(Uuid::new_v4());
        let start = Instant::now();
        throttle.decide(&transition(DOWN, subject), start);
        throttle.decide(&transition(UP, subject), start);
        let again = transition(DOWN, subject);
        throttle.decide(&again, start);
        let end = start + ALERT_FLAP_WINDOW;
        assert!(!throttle.confirm_deferred(&again, false, end));
        assert!(throttle.due(end).is_empty());
    }

    #[test]
    fn a_recovery_between_due_and_confirm_cancels_the_deferred_down() {
        let throttle = AlertThrottle::new();
        let subject = Subject::Heartbeat(Uuid::new_v4());
        let start = Instant::now();
        throttle.decide(&transition(DOWN, subject), start);
        throttle.decide(&transition(UP, subject), start);
        let again = transition(DOWN, subject);
        throttle.decide(&again, start);
        let end = start + ALERT_FLAP_WINDOW;
        assert_eq!(throttle.due(end).len(), 1);
        assert_eq!(
            throttle.decide(&transition(UP, subject), end),
            Decision::Suppress
        );
        assert!(!throttle.confirm_deferred(&again, true, end));
    }

    #[test]
    fn subjects_are_throttled_independently() {
        let throttle = AlertThrottle::new();
        let now = Instant::now();
        for _ in 0..3 {
            let subject = Subject::Heartbeat(Uuid::new_v4());
            assert_eq!(
                throttle.decide(&transition(DOWN, subject), now),
                Decision::Send
            );
        }
    }

    #[test]
    fn a_first_recovery_after_a_restart_is_sent() {
        let throttle = AlertThrottle::new();
        let subject = Subject::Monitor(Uuid::new_v4());
        assert_eq!(
            throttle.decide(&transition(UP, subject), Instant::now()),
            Decision::Send
        );
    }
}
