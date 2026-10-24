//! Weekly time windows (`{weekdays, from, to}` in a zone) for schedule
//! restrictions and escalation step conditions (R19).

use chrono::{DateTime, NaiveTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::uptime::maintenance::model::{Schedule, weekday_from_iso};

/// A window as the API carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeWindow {
    pub weekdays: Vec<u8>,
    pub from: String,
    pub to: String,
}

/// A window that passed [`TimeWindow::validate`].
#[derive(Debug, Clone, PartialEq)]
pub struct ValidWindow {
    schedule: Schedule,
}

impl TimeWindow {
    /// weekdays: ISO 1-7, non-empty, each once; from/to: "HH:MM", 00:00-23:59.
    /// A `from` after `to` spans midnight, `from == to` is a full 24 hours.
    pub fn validate(&self, timezone: Tz) -> Result<ValidWindow, String> {
        let mut weekdays = self.weekdays.clone();
        weekdays.sort_unstable();
        let days = weekdays
            .iter()
            .map(|d| weekday_from_iso(i16::from(*d)))
            .collect::<Option<Vec<_>>>()
            .filter(|days| !days.is_empty() && weekdays.windows(2).all(|w| w[0] != w[1]))
            .ok_or("weekdays must list ISO weekdays, 1 (Monday) to 7 (Sunday), each once")?;
        Ok(ValidWindow {
            schedule: Schedule::Weekly {
                days,
                start: parse_hhmm("from", &self.from)?,
                end: parse_hhmm("to", &self.to)?,
                timezone,
            },
        })
    }
}

impl ValidWindow {
    /// Wall-clock semantics of `maintenance::model::Schedule::covers`.
    pub fn covers(&self, at: DateTime<Utc>) -> bool {
        self.schedule.covers(at)
    }
}

/// The IANA zone `name` spells (case-sensitive).
pub fn parse_timezone(name: &str) -> Result<Tz, String> {
    name.parse().map_err(|_| format!("unknown time zone {name:?}"))
}

/// Exactly "HH:MM": chrono alone would also take "9:00" or " 9:00".
fn parse_hhmm(field: &str, s: &str) -> Result<NaiveTime, String> {
    let b = s.as_bytes();
    let shaped =
        b.len() == 5 && b[2] == b':' && [0, 1, 3, 4].iter().all(|&i| b[i].is_ascii_digit());
    shaped
        .then(|| NaiveTime::parse_from_str(s, "%H:%M").ok())
        .flatten()
        .ok_or_else(|| format!("{field} must be a time of day as HH:MM"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn windows_validate_days_times_and_zones() {
        let tz: Tz = "Europe/Berlin".parse().unwrap();
        assert!(
            TimeWindow { weekdays: vec![1, 2, 3, 4, 5], from: "09:00".into(), to: "17:00".into() }
                .validate(tz)
                .is_ok()
        );
        for bad in [
            TimeWindow { weekdays: vec![], from: "09:00".into(), to: "17:00".into() },
            TimeWindow { weekdays: vec![0], from: "09:00".into(), to: "17:00".into() },
            TimeWindow { weekdays: vec![8], from: "09:00".into(), to: "17:00".into() },
            TimeWindow { weekdays: vec![1, 1], from: "09:00".into(), to: "17:00".into() },
            TimeWindow { weekdays: vec![1], from: "9:00".into(), to: "17:00".into() },
            TimeWindow { weekdays: vec![1], from: " 9:00".into(), to: "17:00".into() },
            TimeWindow { weekdays: vec![1], from: "24:00".into(), to: "17:00".into() },
            TimeWindow { weekdays: vec![1], from: "09:00".into(), to: "17:60".into() },
            TimeWindow { weekdays: vec![1], from: "09:00".into(), to: "17:00:00".into() },
        ] {
            assert!(bad.validate(tz).is_err(), "{bad:?}");
        }
        assert!(parse_timezone("Mars/Olympus").is_err());
        assert_eq!(parse_timezone("Asia/Jakarta"), Ok(chrono_tz::Asia::Jakarta));
    }

    #[test]
    fn a_window_from_after_to_spans_midnight_into_the_next_day() {
        let tz: Tz = "Asia/Jakarta".parse().unwrap(); // UTC+7, no DST
        let night = TimeWindow { weekdays: vec![5], from: "22:00".into(), to: "06:00".into() }
            .validate(tz)
            .unwrap();
        assert!(night.covers(utc("2026-10-23T15:30:00Z")), "Friday 22:30 local");
        assert!(night.covers(utc("2026-10-23T22:59:00Z")), "Saturday 05:59 local, carried over");
        assert!(!night.covers(utc("2026-10-23T23:00:00Z")), "Saturday 06:00 local");
        assert!(!night.covers(utc("2026-10-24T15:30:00Z")), "Saturday 22:30 local is not listed");
    }

    #[test]
    fn windows_follow_the_wall_clock_across_dst() {
        let tz: Tz = "America/New_York".parse().unwrap();
        let mornings = TimeWindow { weekdays: vec![7], from: "09:00".into(), to: "10:00".into() }
            .validate(tz)
            .unwrap();
        // 2026-03-08 is the spring-forward Sunday: 09:00 EDT = 13:00Z (the day before it was 14:00Z).
        assert!(mornings.covers(utc("2026-03-08T13:30:00Z")));
        assert!(!mornings.covers(utc("2026-03-08T14:30:00Z")));
        // 2026-11-01 is the fall-back Sunday: 09:00 EST = 14:00Z.
        assert!(mornings.covers(utc("2026-11-01T14:30:00Z")));
        assert!(!mornings.covers(utc("2026-11-01T13:30:00Z")));
    }
}
