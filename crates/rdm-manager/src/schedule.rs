//! Queue schedules: did a daily `HH:MM` on chosen weekdays pass between two
//! checks? Times are local wall-clock times in the system time zone; DST
//! gaps and folds are resolved by jiff's "compatible" rule: a time skipped
//! by spring-forward fires shifted by the gap (02:30 → 03:30), and a time
//! repeated by fall-back fires once, at its first occurrence.

use jiff::civil::{Date, Time};
use jiff::{SignedDuration, Zoned};

/// A start that was missed by more than this (computer asleep, rdm not
/// running) is skipped rather than surprising the user hours later.
pub(crate) const MAX_LATENESS: SignedDuration = SignedDuration::from_hours(2);

/// `HH:MM` (24-hour) → hour and minute.
pub(crate) fn parse_clock(s: &str) -> Option<(i8, i8)> {
    let (h, m) = s.trim().split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let h: i8 = h.parse().ok()?;
    let m: i8 = m.parse().ok()?;
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some((h, m))
}

/// Whether the daily time `clock` on `days` (ISO weekdays; empty = all)
/// occurred in `(from, to]`, at most [`MAX_LATENESS`] before `to`.
pub(crate) fn crossed(clock: (i8, i8), days: &[u8], from: &Zoned, to: &Zoned) -> bool {
    if to <= from {
        return false;
    }
    let tz = to.time_zone().clone();
    let Ok(time) = Time::new(clock.0, clock.1, 0, 0) else {
        return false;
    };
    let mut date: Date = from.date();
    let last = to.date();
    // A handful of days at most; long sleeps are cut off by MAX_LATENESS.
    for _ in 0..8 {
        let weekday = date.weekday().to_monday_one_offset() as u8;
        if (days.is_empty() || days.contains(&weekday))
            && let Ok(at) = date.to_datetime(time).to_zoned(tz.clone())
            && &at > from
            && &at <= to
            && to.duration_since(&at) <= MAX_LATENESS
        {
            return true;
        }
        if date >= last {
            break;
        }
        match date.tomorrow() {
            Ok(d) => date = d,
            Err(_) => break,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> Zoned {
        s.parse().unwrap()
    }

    #[test]
    fn clocks() {
        assert_eq!(parse_clock("07:30"), Some((7, 30)));
        assert_eq!(parse_clock("7:05"), Some((7, 5)));
        assert_eq!(parse_clock("23:59"), Some((23, 59)));
        for bad in ["24:00", "12:60", "12:5", "noon", "", "12", "-1:00"] {
            assert_eq!(parse_clock(bad), None, "{bad}");
        }
    }

    #[test]
    fn fires_once_when_the_time_passes() {
        let a = at("2026-10-12T01:59:50[Europe/London]");
        let b = at("2026-10-12T02:00:05[Europe/London]");
        assert!(crossed((2, 0), &[], &a, &b));
        // The next check must not fire again.
        let c = at("2026-10-12T02:00:20[Europe/London]");
        assert!(!crossed((2, 0), &[], &b, &c));
        // Exactly at the boundary belongs to the later window.
        let exact = at("2026-10-12T02:00:00[Europe/London]");
        assert!(crossed((2, 0), &[], &a, &exact));
        assert!(!crossed((2, 0), &[], &exact, &c));
    }

    #[test]
    fn respects_weekdays() {
        // 2026-10-12 is a Monday.
        let a = at("2026-10-12T06:59:00[UTC]");
        let b = at("2026-10-12T07:01:00[UTC]");
        assert!(crossed((7, 0), &[1], &a, &b));
        assert!(!crossed((7, 0), &[2, 3, 4, 5], &a, &b));
        // Across midnight into Tuesday.
        let c = at("2026-10-12T23:59:00[UTC]");
        let d = at("2026-10-13T00:01:00[UTC]");
        assert!(crossed((0, 0), &[2], &c, &d));
        assert!(!crossed((0, 0), &[1], &c, &d));
    }

    #[test]
    fn long_sleeps_skip_stale_starts() {
        let a = at("2026-10-12T06:00:00[UTC]");
        let late = at("2026-10-12T10:00:00[UTC]");
        assert!(
            !crossed((7, 0), &[], &a, &late),
            "four hours late is skipped"
        );
        let slightly = at("2026-10-12T08:30:00[UTC]");
        assert!(crossed((7, 0), &[], &a, &slightly));
    }

    #[test]
    fn dst_gap_and_fold() {
        // US spring forward: 02:30 does not exist on 2026-03-08 and maps
        // to 03:30 EDT.
        let a = at("2026-03-08T01:59:00[America/New_York]");
        let early = at("2026-03-08T03:01:00[America/New_York]");
        let b = at("2026-03-08T03:31:00[America/New_York]");
        assert!(!crossed((2, 30), &[], &a, &early));
        assert!(crossed((2, 30), &[], &early, &b));
        // Fall back: 01:30 happens twice on 2026-11-01; fire only once.
        let c = at("2026-11-01T01:00:00-04:00[America/New_York]");
        let d = at("2026-11-01T01:45:00-04:00[America/New_York]");
        let e = at("2026-11-01T01:45:00-05:00[America/New_York]");
        assert!(crossed((1, 30), &[], &c, &d));
        assert!(!crossed((1, 30), &[], &d, &e));
    }
}
