//! UTC timestamps for things that get named after when they happened.
//!
//! Binding snapshots and install records are both directories named by time,
//! and both need that name to sort lexicographically, survive a shell, and be
//! readable a year later. That is a small enough job to do here rather than
//! taking a date-time crate for it.
//!
//! UTC only, deliberately. A snapshot directory named in local time changes
//! its apparent order twice a year, and a folder full of `02-30` twice over is
//! worse than one that never matches the wall clock.

use std::time::{SystemTime, UNIX_EPOCH};

/// A civil date and time, UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Utc {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl Utc {
    pub fn now() -> Self {
        Self::from(SystemTime::now())
    }

    /// Times before 1970 are not something this application can produce, so
    /// they clamp to the epoch rather than complicating every caller.
    pub fn from(time: SystemTime) -> Self {
        let secs = time
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self::from_unix(secs)
    }

    pub fn from_unix(secs: i64) -> Self {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Utc {
            year,
            month,
            day,
            hour: (rem / 3600) as u32,
            minute: (rem % 3600 / 60) as u32,
            second: (rem % 60) as u32,
        }
    }

    /// `2026-08-24T14-30-05Z` — safe as a directory name, and sorts by time.
    ///
    /// ISO-8601 proper would put colons in the time. They are legal on Linux
    /// but need quoting in a shell and are rejected outright if a snapshot is
    /// ever copied to a Windows drive, which for a folder of backups is the
    /// wrong trade.
    pub fn stamp(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// `2026-08-24 14:30:05 UTC` — for showing a person, not for naming files.
    pub fn readable(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// Seconds since the Unix epoch.
    pub fn to_unix(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }

    /// How long ago this was, from now. `None` if it is in the future.
    pub fn ago(&self) -> Option<std::time::Duration> {
        let seconds = Utc::now().to_unix() - self.to_unix();
        (seconds >= 0).then(|| std::time::Duration::from_secs(seconds as u64))
    }

    /// Parse an ISO-8601 instant like `2026-08-25T03:46:25.053Z`.
    ///
    /// Only the shape logs actually write: a date, a `T`, a time, optional
    /// fractional seconds, and an optional `Z`. Offsets other than UTC are not
    /// accepted rather than being silently misread — a timestamp an hour out is
    /// worse than one that failed to parse.
    pub fn parse_iso8601(s: &str) -> Option<Self> {
        let s = s.trim().strip_suffix('Z').unwrap_or(s);
        let (date, time) = s.split_once('T')?;
        let time = time.split_once('.').map_or(time, |(head, _)| head);
        if time.contains('+') || time.matches('-').count() > 0 {
            return None;
        }

        let mut date = date.splitn(3, '-');
        let mut time = time.splitn(3, ':');
        let utc = Utc {
            year: date.next()?.parse().ok()?,
            month: date.next()?.parse().ok()?,
            day: date.next()?.parse().ok()?,
            hour: time.next()?.parse().ok()?,
            minute: time.next()?.parse().ok()?,
            second: time.next()?.parse().ok()?,
        };
        utc.plausible().then_some(utc)
    }

    fn plausible(&self) -> bool {
        (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
    }

    /// Parse a stamp produced by [`Utc::stamp`]. Anything else returns `None`.
    pub fn parse_stamp(s: &str) -> Option<Self> {
        let s = s.strip_suffix('Z')?;
        let (date, time) = s.split_once('T')?;
        let mut date = date.splitn(3, '-');
        let mut time = time.splitn(3, '-');
        let utc = Utc {
            year: date.next()?.parse().ok()?,
            month: date.next()?.parse().ok()?,
            day: date.next()?.parse().ok()?,
            hour: time.next()?.parse().ok()?,
            minute: time.next()?.parse().ok()?,
            second: time.next()?.parse().ok()?,
        };
        utc.plausible().then_some(utc)
    }
}

/// Days since the Unix epoch to a civil date.
///
/// Howard Hinnant's `civil_from_days`, which is exact for the whole range of
/// `i64` and has no table and no leap-year special cases beyond the shifted
/// era arithmetic. Worth keeping verbatim rather than "simplifying".
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// A civil date to days since the Unix epoch — the inverse of
/// [`civil_from_days`], and Hinnant's `days_from_civil`. Kept verbatim for the
/// same reason.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = i64::from(month);
    let d = i64::from(day);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_where_it_should_be() {
        let t = Utc::from_unix(0);
        assert_eq!(t.stamp(), "1970-01-01T00-00-00Z");
    }

    #[test]
    fn a_known_instant_matches_date_minus_u() {
        // $ date -u -d @1756044605  ->  Sun Aug 24 14:10:05 UTC 2025
        let t = Utc::from_unix(1_756_044_605);
        assert_eq!(t.stamp(), "2025-08-24T14-10-05Z");
        assert_eq!(t.readable(), "2025-08-24 14:10:05 UTC");
    }

    #[test]
    fn leap_days_are_not_special_cased_wrongly() {
        // 2024-02-29, a leap day in a leap century-rule year.
        assert_eq!(
            Utc::from_unix(1_709_164_800).stamp(),
            "2024-02-29T00-00-00Z"
        );
        // 2000-02-29 — divisible by 400, so a leap year despite the century.
        assert_eq!(Utc::from_unix(951_782_400).stamp(), "2000-02-29T00-00-00Z");
        // 1900 was not a leap year: 1900-03-01 follows 1900-02-28.
        assert_eq!(
            Utc::from_unix(-2_203_891_200).stamp(),
            "1900-03-01T00-00-00Z"
        );
    }

    #[test]
    fn stamps_sort_in_time_order() {
        let mut stamps = [
            Utc::from_unix(1_756_044_605).stamp(),
            Utc::from_unix(1_000_000_000).stamp(),
            Utc::from_unix(1_756_044_604).stamp(),
        ];
        stamps.sort();
        assert_eq!(
            stamps,
            [
                "2001-09-09T01-46-40Z",
                "2025-08-24T14-10-04Z",
                "2025-08-24T14-10-05Z",
            ]
        );
    }

    #[test]
    fn a_stamp_survives_a_round_trip() {
        let t = Utc::from_unix(1_756_044_605);
        assert_eq!(Utc::parse_stamp(&t.stamp()), Some(t));
    }

    #[test]
    fn a_civil_date_survives_a_trip_to_unix_and_back() {
        for secs in [
            0i64,
            1_756_044_605,
            951_782_400,
            1_709_164_800,
            -2_203_891_200,
        ] {
            assert_eq!(Utc::from_unix(secs).to_unix(), secs, "at {secs}");
        }
    }

    #[test]
    fn a_log_timestamp_parses() {
        // The shape Project Airports writes, fractional seconds and all.
        let t = Utc::parse_iso8601("2026-08-25T03:46:25.053Z").expect("parses");
        assert_eq!(t.stamp(), "2026-08-25T03-46-25Z");
        // And without the fraction or the Z.
        assert_eq!(
            Utc::parse_iso8601("2026-08-25T03:46:25"),
            Some(t),
            "the fraction and the Z are both optional"
        );
    }

    #[test]
    fn a_timestamp_in_another_zone_is_refused_rather_than_misread() {
        // An hour out is worse than a parse failure, because it looks fine.
        assert_eq!(Utc::parse_iso8601("2026-08-25T03:46:25+01:00"), None);
        assert_eq!(Utc::parse_iso8601("2026-08-25T03:46:25-05:00"), None);
        assert_eq!(Utc::parse_iso8601("nonsense"), None);
    }

    #[test]
    fn a_directory_that_is_not_a_stamp_is_not_mistaken_for_one() {
        assert_eq!(Utc::parse_stamp("bindings"), None);
        assert_eq!(Utc::parse_stamp("2026-08-24T14-30-05"), None);
        assert_eq!(Utc::parse_stamp("2026-13-24T14-30-05Z"), None);
        assert_eq!(Utc::parse_stamp("2026-08-24T25-30-05Z"), None);
    }
}
