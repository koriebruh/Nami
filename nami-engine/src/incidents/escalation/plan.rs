//! The escalation planner (pure): where the cursor goes after a step ran.

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

/// The shortest time between the end of a pass and the start of the next
/// (a repeat, or the fallback after the last repeat), whatever the delays.
pub const MIN_REPEAT_GAP: Duration = Duration::minutes(5);

/// What the planner needs of a policy: its steps' delays, in order, and how
/// often the whole policy repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyShape {
    pub id: Uuid,
    pub delays_minutes: Vec<u32>,
    pub repeat_count: u32,
}

/// Where an incident's escalation stands: the next step to run, the pass,
/// whether it walks the fallback policy, and how many recipients the current
/// pass has notified so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub step: usize,
    pub repeat: u32,
    pub on_fallback: bool,
    pub notified_in_pass: u32,
}

/// Why escalation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exhausted {
    /// A whole pass notified nobody and there is no fallback left.
    NobodyNotified,
    /// Every repeat ran and there is no fallback left.
    RepeatsDone,
    /// The incident has no policy (it was deleted).
    NoPolicy,
}

/// The planner's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Step { cursor: Cursor, at: DateTime<Utc> },
    Exhausted(Exhausted),
}

/// The step after the one that just ran. `active` is the policy the cursor is
/// walking (the incident's own, or its fallback when `cursor.on_fallback`);
/// `fallback` is the incident's policy's fallback (ignored when already on
/// it: one hop). `ran_at` is the step's scheduled time (R14), so a slow tick
/// does not drift the policy.
///
/// A pass that notified nobody moves to the fallback at once (or ends). A
/// pass that notified someone repeats, then moves to the fallback, each no
/// sooner than [`MIN_REPEAT_GAP`] after `ran_at`. Every walk ends: the
/// fallback is entered at most once and each pass is finite.
pub fn after_step(
    active: &PolicyShape,
    fallback: Option<&PolicyShape>,
    cursor: Cursor,
    ran_at: DateTime<Utc>,
    newly_notified: u32,
) -> Next {
    let notified = cursor.notified_in_pass.saturating_add(newly_notified);
    let minutes = |m: u32| Duration::minutes(i64::from(m));
    if let Some(&delay) = active.delays_minutes.get(cursor.step + 1) {
        return Next::Step {
            cursor: Cursor { step: cursor.step + 1, notified_in_pass: notified, ..cursor },
            at: ran_at + minutes(delay),
        };
    }
    // The last step of a pass ran.
    let fallback = fallback.filter(|f| !cursor.on_fallback && !f.delays_minutes.is_empty());
    let to_fallback = |f: &PolicyShape, gap: Duration| Next::Step {
        cursor: Cursor { step: 0, repeat: 0, on_fallback: true, notified_in_pass: 0 },
        at: ran_at + minutes(f.delays_minutes[0]).max(gap),
    };
    if notified == 0 {
        return match fallback {
            Some(f) => to_fallback(f, Duration::zero()),
            None => Next::Exhausted(Exhausted::NobodyNotified),
        };
    }
    if cursor.repeat < active.repeat_count {
        let first = active.delays_minutes.first().copied().unwrap_or(0);
        return Next::Step {
            cursor: Cursor { step: 0, repeat: cursor.repeat + 1, notified_in_pass: 0, ..cursor },
            at: ran_at + minutes(first).max(MIN_REPEAT_GAP),
        };
    }
    match fallback {
        Some(f) => to_fallback(f, MIN_REPEAT_GAP),
        None => Next::Exhausted(Exhausted::RepeatsDone),
    }
}

/// The round label of a step (the deliveries' dedup key part, at most 32
/// chars): `"g{generation}p{0|1}r{repeat}s{step}"`, `p1` on the fallback.
pub fn round(generation: i32, cursor: &Cursor) -> String {
    format!(
        "g{generation}p{}r{}s{}",
        u8::from(cursor.on_fallback),
        cursor.repeat,
        cursor.step
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_792_000_000 + minutes * 60, 0).unwrap()
    }
    fn policy(delays: &[u32], repeat: u32) -> PolicyShape {
        PolicyShape {
            id: Uuid::from_u128(delays.len() as u128 * 100 + repeat as u128),
            delays_minutes: delays.to_vec(),
            repeat_count: repeat,
        }
    }
    fn cursor(step: usize, repeat: u32, on_fallback: bool, notified: u32) -> Cursor {
        Cursor { step, repeat, on_fallback, notified_in_pass: notified }
    }

    #[test]
    fn steps_follow_each_other_after_their_delay_from_the_scheduled_time() {
        let p = policy(&[0, 10, 30], 0);
        assert_eq!(
            after_step(&p, None, cursor(0, 0, false, 0), t(0), 2),
            Next::Step { cursor: cursor(1, 0, false, 2), at: t(10) }
        );
        assert_eq!(
            after_step(&p, None, cursor(1, 0, false, 2), t(10), 0),
            Next::Step { cursor: cursor(2, 0, false, 2), at: t(40) }
        );
    }

    #[test]
    fn a_pass_that_notified_someone_repeats_after_at_least_the_minimum_gap() {
        let p = policy(&[0, 10], 2);
        assert_eq!(
            after_step(&p, None, cursor(1, 0, false, 1), t(10), 0),
            Next::Step { cursor: cursor(0, 1, false, 0), at: t(15) },
            "delay 0 still waits MIN_REPEAT_GAP"
        );
        let slow = policy(&[20], 1);
        assert_eq!(
            after_step(&slow, None, cursor(0, 0, false, 0), t(20), 3),
            Next::Step { cursor: cursor(0, 1, false, 0), at: t(40) }
        );
        assert_eq!(
            after_step(&p, None, cursor(1, 2, false, 1), t(50), 0),
            Next::Exhausted(Exhausted::RepeatsDone)
        );
    }

    #[test]
    fn a_pass_that_notified_nobody_moves_to_the_fallback_at_once() {
        let p = policy(&[0, 5], 3);
        let f = policy(&[0], 0);
        assert_eq!(
            after_step(&p, Some(&f), cursor(1, 0, false, 0), t(5), 0),
            Next::Step { cursor: cursor(0, 0, true, 0), at: t(5) }
        );
        assert_eq!(
            after_step(&p, None, cursor(1, 0, false, 0), t(5), 0),
            Next::Exhausted(Exhausted::NobodyNotified)
        );
    }

    #[test]
    fn the_last_repeat_ending_unacknowledged_moves_to_the_fallback_after_the_gap() {
        let p = policy(&[0], 1);
        let f = policy(&[2], 0);
        assert_eq!(
            after_step(&p, Some(&f), cursor(0, 1, false, 0), t(30), 1),
            Next::Step { cursor: cursor(0, 0, true, 0), at: t(35) }
        );
    }

    #[test]
    fn a_fallback_is_followed_one_hop_only() {
        let f = policy(&[0], 0);
        let ff = policy(&[0], 0);
        assert_eq!(
            after_step(&f, Some(&ff), cursor(0, 0, true, 0), t(0), 0),
            Next::Exhausted(Exhausted::NobodyNotified)
        );
        assert_eq!(
            after_step(&f, Some(&ff), cursor(0, 0, true, 0), t(0), 1),
            Next::Exhausted(Exhausted::RepeatsDone)
        );
    }

    #[test]
    fn every_path_terminates_within_a_bounded_number_of_steps() {
        // Exhaustive over small policies: walking `after_step` from step 0 always reaches
        // Exhausted within (repeat+1) * steps * 2 + 2 calls, whatever gets notified.
        for steps in 1..=4usize {
            for repeat in 0..=3u32 {
                for notify_pattern in 0..16u32 {
                    let p = policy(&vec![0; steps], repeat);
                    let f = policy(&vec![1; steps], repeat);
                    let mut c = cursor(0, 0, false, 0);
                    let mut at = t(0);
                    let mut calls = 0;
                    loop {
                        calls += 1;
                        assert!(calls <= ((repeat as usize + 1) * steps * 2 + 2), "no termination");
                        let notified = (notify_pattern >> (calls % 4)) & 1;
                        let active = if c.on_fallback { &f } else { &p };
                        match after_step(active, Some(&f), c, at, notified) {
                            Next::Step { cursor, at: next } => {
                                assert!(next >= at);
                                c = cursor;
                                at = next;
                            }
                            Next::Exhausted(_) => break,
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rounds_are_short_and_distinct() {
        let a = round(0, &cursor(0, 0, false, 0));
        let b = round(0, &cursor(0, 0, true, 0));
        let c = round(1, &cursor(0, 0, false, 0));
        assert_eq!(a, "g0p0r0s0");
        assert!(a != b && a != c && b != c);
        assert!(round(i32::MAX, &cursor(19, 10, true, 0)).len() <= 32);
    }
}
