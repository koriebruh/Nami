//! Who is on call when (R20). Pure: no I/O.

use chrono::{
    DateTime, Days, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc,
};
use chrono_tz::Tz;
use serde::Serialize;
use uuid::Uuid;

use crate::incidents::timewindow::ValidWindow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationLength {
    Daily,
    Weekly,
    /// 1..=8760 (validated by the caller; 0 counts as 1, never a hang).
    Hours(u32),
}

#[derive(Debug, Clone)]
pub struct Rotation {
    pub timezone: Tz,
    pub start_date: NaiveDate,
    pub handoff: NaiveTime,
    pub length: RotationLength,
    pub participants: Vec<Uuid>,
    pub restrictions: Vec<ValidWindow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Override {
    pub id: Uuid,
    pub user_id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Shift {
    pub user_id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub override_id: Option<Uuid>,
}

pub const MAX_SHIFT_RANGE: chrono::Duration = chrono::Duration::days(70);

/// Ambiguous (fall back): the earlier instant. Nonexistent (spring forward):
/// the first valid minute after the requested local time.
pub fn local_to_utc(tz: Tz, local: NaiveDateTime) -> DateTime<Utc> {
    let mut probe = local;
    for _ in 0..=24 * 60 {
        match tz.from_local_datetime(&probe) {
            LocalResult::Single(t) => return t.with_timezone(&Utc),
            LocalResult::Ambiguous(earlier, _) => return earlier.with_timezone(&Utc),
            LocalResult::None => probe += TimeDelta::minutes(1),
        }
    }
    local.and_utc() // unreachable for real zones; never panic
}

fn add_days(t: NaiveDateTime, days: i64) -> Option<NaiveDateTime> {
    let d = Days::new(days.unsigned_abs());
    if days >= 0 { t.checked_add_days(d) } else { t.checked_sub_days(d) }
}

/// Start of shift `k`: daily/weekly are calendar steps of the local handoff,
/// custom hours are absolute time from the first handoff.
pub fn shift_start(r: &Rotation, k: i64) -> DateTime<Utc> {
    let epoch_local = r.start_date.and_time(r.handoff);
    let local_days = |n: i64| add_days(epoch_local, n).map(|t| local_to_utc(r.timezone, t));
    match r.length {
        RotationLength::Daily => local_days(k),
        RotationLength::Weekly => local_days(k.saturating_mul(7)),
        RotationLength::Hours(h) => TimeDelta::try_hours(i64::from(h.max(1)).saturating_mul(k))
            .and_then(|d| local_to_utc(r.timezone, epoch_local).checked_add_signed(d)),
    }
    .unwrap_or(DateTime::<Utc>::MAX_UTC) // only for absurd k
}

/// Index of the rotation shift containing `at`; None before the first shift.
pub fn shift_index(r: &Rotation, at: DateTime<Utc>) -> Option<i64> {
    let epoch = shift_start(r, 0);
    if at < epoch {
        return None;
    }
    let nominal = match r.length {
        RotationLength::Daily => 86_400,
        RotationLength::Weekly => 604_800,
        RotationLength::Hours(h) => i64::from(h.max(1)) * 3600,
    };
    let mut k = (at - epoch).num_seconds() / nominal;
    while k > 0 && shift_start(r, k) > at {
        k -= 1; // DST made the estimate overshoot
    }
    while shift_start(r, k + 1) <= at {
        k += 1; // ...or undershoot
    }
    Some(k)
}

/// The rotation's user at `at`: nobody without participants, before the
/// first shift, or outside every restriction window (when there are any).
pub fn rotation_user_at(r: &Rotation, at: DateTime<Utc>) -> Option<Uuid> {
    if r.participants.is_empty()
        || (!r.restrictions.is_empty() && !r.restrictions.iter().any(|w| w.covers(at)))
    {
        return None;
    }
    let k = shift_index(r, at)?;
    let n = r.participants.len() as i64;
    Some(r.participants[k.rem_euclid(n) as usize])
}

/// (user, override id): the newest override covering `at` wins (ties: the
/// larger id), regardless of restrictions; otherwise the rotation.
pub fn on_call_at(
    r: &Rotation,
    overrides: &[Override],
    at: DateTime<Utc>,
) -> Option<(Uuid, Option<Uuid>)> {
    overrides
        .iter()
        .filter(|o| o.starts_at <= at && at < o.ends_at)
        .max_by_key(|o| (o.created_at, o.id))
        .map(|o| (o.user_id, Some(o.id)))
        .or_else(|| rotation_user_at(r, at).map(|u| (u, None)))
}

/// Merged timeline in [from, to); panics never, clamps to MAX_SHIFT_RANGE.
/// Samples `from`, every whole UTC minute and every override edge in range.
pub fn shifts(
    r: &Rotation,
    overrides: &[Override],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<Shift> {
    let to = to.min(from.checked_add_signed(MAX_SHIFT_RANGE).unwrap_or(to));
    if from >= to {
        return Vec::new();
    }
    let first_minute = (from.timestamp().div_euclid(60) + 1) * 60;
    let mut points: Vec<DateTime<Utc>> = std::iter::once(from)
        .chain(
            (first_minute..)
                .step_by(60)
                .map_while(|s| DateTime::from_timestamp(s, 0).filter(|t| *t < to)),
        )
        .chain(
            overrides
                .iter()
                .flat_map(|o| [o.starts_at, o.ends_at])
                .filter(|t| from < *t && *t < to),
        )
        .collect();
    points.sort_unstable();
    points.dedup();

    let mut out: Vec<Shift> = Vec::new();
    let mut prev = None;
    for at in points {
        let current = on_call_at(r, overrides, at);
        if current == prev {
            continue;
        }
        if prev.is_some()
            && let Some(last) = out.last_mut()
        {
            last.ends_at = at;
        }
        if let Some((user_id, override_id)) = current {
            out.push(Shift { user_id, starts_at: at, ends_at: to, override_id });
        }
        prev = current;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incidents::timewindow::TimeWindow;

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }
    fn users(n: usize) -> Vec<Uuid> {
        (0..n).map(|i| Uuid::from_u128(i as u128 + 1)).collect()
    }

    fn rotation(
        tz: &str,
        start: &str,
        handoff: &str,
        length: RotationLength,
        n: usize,
    ) -> Rotation {
        Rotation {
            timezone: tz.parse().unwrap(),
            start_date: start.parse().unwrap(),
            handoff: NaiveTime::parse_from_str(handoff, "%H:%M").unwrap(),
            length,
            participants: users(n),
            restrictions: vec![],
        }
    }

    #[test]
    fn daily_rotation_cycles_through_participants_at_the_handoff() {
        let r = rotation("UTC", "2026-10-19", "09:00", RotationLength::Daily, 3);
        let u = users(3);
        let before = utc("2026-10-19T08:59:59Z");
        assert_eq!(rotation_user_at(&r, before), None, "before the first shift");
        assert_eq!(rotation_user_at(&r, utc("2026-10-19T09:00:00Z")), Some(u[0]));
        assert_eq!(rotation_user_at(&r, utc("2026-10-20T08:59:59Z")), Some(u[0]));
        assert_eq!(rotation_user_at(&r, utc("2026-10-20T09:00:00Z")), Some(u[1]));
        assert_eq!(rotation_user_at(&r, utc("2026-10-22T09:00:00Z")), Some(u[0]), "wraps around");
    }

    #[test]
    fn weekly_handoff_keeps_local_time_across_spring_forward() {
        // Europe/Berlin springs forward on Sunday 2026-03-29 (02:00 -> 03:00).
        let r = rotation("Europe/Berlin", "2026-03-23", "09:00", RotationLength::Weekly, 2);
        assert_eq!(shift_start(&r, 0), utc("2026-03-23T08:00:00Z"), "09:00 CET");
        assert_eq!(shift_start(&r, 1), utc("2026-03-30T07:00:00Z"), "09:00 CEST");
        let u = users(2);
        assert_eq!(rotation_user_at(&r, utc("2026-03-30T06:59:00Z")), Some(u[0]));
        assert_eq!(rotation_user_at(&r, utc("2026-03-30T07:00:00Z")), Some(u[1]));
    }

    #[test]
    fn a_handoff_inside_the_spring_forward_gap_happens_at_the_first_valid_instant() {
        let r = rotation("Europe/Berlin", "2026-03-28", "02:30", RotationLength::Daily, 2);
        assert_eq!(shift_start(&r, 0), utc("2026-03-28T01:30:00Z"), "02:30 CET the day before");
        assert_eq!(
            shift_start(&r, 1),
            utc("2026-03-29T01:00:00Z"),
            "02:30 does not exist: 03:00 CEST"
        );
        assert_eq!(shift_start(&r, 2), utc("2026-03-30T00:30:00Z"), "02:30 CEST");
    }

    #[test]
    fn fall_back_repeats_no_shift() {
        // Europe/Berlin falls back on Sunday 2026-10-25 (03:00 -> 02:00): 02:30 happens twice.
        let r = rotation("Europe/Berlin", "2026-10-24", "02:30", RotationLength::Daily, 2);
        assert_eq!(shift_start(&r, 1), utc("2026-10-25T00:30:00Z"), "the earlier 02:30 (CEST)");
        assert_eq!(shift_start(&r, 2), utc("2026-10-26T01:30:00Z"));
        let u = users(2);
        assert_eq!(
            rotation_user_at(&r, utc("2026-10-25T01:30:00Z")),
            Some(u[1]),
            "the repeated 02:30 is mid-shift"
        );
        let s = shifts(&r, &[], utc("2026-10-24T00:00:00Z"), utc("2026-10-27T00:00:00Z"));
        assert_eq!(
            s.iter().map(|s| s.starts_at).collect::<Vec<_>>(),
            [utc("2026-10-24T00:30:00Z"), utc("2026-10-25T00:30:00Z"), utc("2026-10-26T01:30:00Z")],
            "nobody before the epoch; a 25-hour shift on the fall-back day"
        );
    }

    #[test]
    fn custom_hour_rotations_are_absolute_time() {
        let r = rotation("Europe/Berlin", "2026-03-28", "20:00", RotationLength::Hours(12), 2);
        assert_eq!(shift_start(&r, 0), utc("2026-03-28T19:00:00Z"));
        assert_eq!(
            shift_start(&r, 1),
            utc("2026-03-29T07:00:00Z"),
            "12 real hours, not 08:00 local"
        );
    }

    #[test]
    fn restrictions_blank_the_rotation_outside_working_hours_but_overrides_still_apply() {
        let tz: Tz = "UTC".parse().unwrap();
        let mut r = rotation("UTC", "2026-10-19", "00:00", RotationLength::Weekly, 1);
        r.restrictions = vec![
            TimeWindow { weekdays: vec![1, 2, 3, 4, 5], from: "09:00".into(), to: "17:00".into() }
                .validate(tz)
                .unwrap(),
        ];
        assert_eq!(rotation_user_at(&r, utc("2026-10-19T10:00:00Z")), Some(users(1)[0]));
        assert_eq!(rotation_user_at(&r, utc("2026-10-19T18:00:00Z")), None);
        assert_eq!(rotation_user_at(&r, utc("2026-10-24T10:00:00Z")), None, "Saturday");
        let stand_in = Uuid::from_u128(99);
        let o = Override {
            id: Uuid::from_u128(500),
            user_id: stand_in,
            starts_at: utc("2026-10-19T17:00:00Z"),
            ends_at: utc("2026-10-19T20:00:00Z"),
            created_at: utc("2026-10-18T00:00:00Z"),
        };
        assert_eq!(
            on_call_at(&r, &[o.clone()], utc("2026-10-19T18:00:00Z")),
            Some((stand_in, Some(o.id)))
        );
    }

    #[test]
    fn the_newest_override_wins_on_overlap() {
        let r = rotation("UTC", "2026-10-19", "09:00", RotationLength::Daily, 1);
        let (a, b) = (Uuid::from_u128(10), Uuid::from_u128(11));
        let older = Override {
            id: Uuid::from_u128(1),
            user_id: a,
            starts_at: utc("2026-10-19T10:00:00Z"),
            ends_at: utc("2026-10-19T14:00:00Z"),
            created_at: utc("2026-10-01T00:00:00Z"),
        };
        let newer = Override {
            id: Uuid::from_u128(2),
            user_id: b,
            starts_at: utc("2026-10-19T12:00:00Z"),
            ends_at: utc("2026-10-19T13:00:00Z"),
            created_at: utc("2026-10-02T00:00:00Z"),
        };
        let all = [older.clone(), newer.clone()];
        assert_eq!(on_call_at(&r, &all, utc("2026-10-19T11:00:00Z")).unwrap().0, a);
        assert_eq!(on_call_at(&r, &all, utc("2026-10-19T12:30:00Z")).unwrap().0, b);
        assert_eq!(on_call_at(&r, &all, utc("2026-10-19T13:30:00Z")).unwrap().0, a);
        let s = shifts(&r, &all, utc("2026-10-19T09:00:00Z"), utc("2026-10-19T15:00:00Z"));
        let users_in_order: Vec<Uuid> = s.iter().map(|s| s.user_id).collect();
        assert_eq!(users_in_order, [users(1)[0], a, b, a, users(1)[0]]);
        assert_eq!(s[2].starts_at, utc("2026-10-19T12:00:00Z"));
        assert_eq!(s[2].ends_at, utc("2026-10-19T13:00:00Z"));
    }

    #[test]
    fn shifts_merge_runs_and_are_clamped_to_70_days() {
        let r = rotation("UTC", "2026-10-19", "09:00", RotationLength::Daily, 1);
        let s = shifts(&r, &[], utc("2026-10-19T09:00:00Z"), utc("2027-10-19T09:00:00Z"));
        assert_eq!(s.len(), 1, "one participant: one merged shift");
        assert_eq!(s[0].ends_at, utc("2026-10-19T09:00:00Z") + MAX_SHIFT_RANGE);
        assert!(
            shifts(
                &rotation("UTC", "2026-10-19", "09:00", RotationLength::Daily, 0),
                &[],
                utc("2026-10-19T00:00:00Z"),
                utc("2026-10-20T00:00:00Z")
            )
            .is_empty(),
            "no participants: nobody"
        );
    }

    #[test]
    fn shift_index_agrees_with_shift_start_for_a_year_of_daily_shifts_in_a_dst_zone() {
        let r = rotation("America/Santiago", "2026-01-01", "00:00", RotationLength::Daily, 7);
        for k in 0..400 {
            let start = shift_start(&r, k);
            assert_eq!(shift_index(&r, start), Some(k), "k={k}");
            assert_eq!(
                shift_index(&r, start - chrono::Duration::seconds(1)),
                if k == 0 { None } else { Some(k - 1) }
            );
        }
    }
}
