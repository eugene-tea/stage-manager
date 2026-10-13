//! Local dates and timestamps for the log, without a calendar crate: the platform layer
//! reads the local time (`localtime_r`) into a [`Stamp`]; everything here is arithmetic.

use std::fmt;

/// A calendar date (proleptic Gregorian).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    /// The year, e.g. 2026.
    pub year: i32,
    /// 1..=12.
    pub month: u32,
    /// 1..=31.
    pub day: u32,
}

impl Date {
    /// A valid date, or `None`.
    pub fn new(year: i32, month: u32, day: u32) -> Option<Self> {
        let d = Self { year, month, day };
        (Self::from_days(d.days_since_epoch()) == d).then_some(d)
    }

    /// Parses `YYYY-MM-DD`.
    pub fn parse(text: &str) -> Option<Self> {
        let b = text.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let year = text.get(0..4)?.parse().ok()?;
        let month = text.get(5..7)?.parse().ok()?;
        let day = text.get(8..10)?.parse().ok()?;
        Self::new(year, month, day)
    }

    /// Days since 1970-01-01 (negative before it).
    pub fn days_since_epoch(self) -> i64 {
        // Howard Hinnant's days_from_civil.
        let y = i64::from(self.year) - i64::from(self.month <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.month);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// The date `days` after 1970-01-01.
    pub fn from_days(days: i64) -> Self {
        // Howard Hinnant's civil_from_days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
        Self { year, month, day }
    }

    /// This date moved by `days` (negative: earlier).
    pub fn add_days(self, days: i64) -> Self {
        Self::from_days(self.days_since_epoch() + days)
    }

    /// `YYYY-MM-DD.log`: the name of this day's log file.
    pub fn file_name(self) -> String {
        format!("{self}.log")
    }

    /// The date of a day's log file name (`YYYY-MM-DD.log`), or `None` for any other name.
    pub fn from_file_name(name: &str) -> Option<Self> {
        Self::parse(name.strip_suffix(".log")?)
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// A local wall-clock time with its UTC offset, to the millisecond.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// The local date.
    pub date: Date,
    /// 0..=23.
    pub hour: u32,
    /// 0..=59.
    pub minute: u32,
    /// 0..=60 (a leap second).
    pub second: u32,
    /// 0..=999.
    pub millis: u32,
    /// Local time minus UTC, in minutes (+120 in Central European Summer Time).
    pub offset_minutes: i32,
}

impl Stamp {
    /// A UTC stamp from seconds and milliseconds since the epoch (the fallback when the local
    /// time cannot be read).
    pub fn utc(secs: i64, millis: u32) -> Self {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400) as u32;
        Self {
            date: Date::from_days(days),
            hour: rem / 3600,
            minute: rem / 60 % 60,
            second: rem % 60,
            millis,
            offset_minutes: 0,
        }
    }

    /// `HH:MM:SS.mmm`.
    pub fn clock(&self) -> String {
        format!("{:02}:{:02}:{:02}.{:03}", self.hour, self.minute, self.second, self.millis)
    }

    /// `HH:MM`.
    pub fn hhmm(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// `+HH:MM` or `-HH:MM`.
    pub fn offset(&self) -> String {
        let sign = if self.offset_minutes < 0 { '-' } else { '+' };
        let m = self.offset_minutes.unsigned_abs();
        format!("{sign}{:02}:{:02}", m / 60, m % 60)
    }

    /// Milliseconds until the next full local hour (at least 1).
    pub fn millis_to_next_hour(&self) -> u64 {
        let into = u64::from(self.minute.min(59)) * 60_000 + u64::from(self.second.min(59)) * 1000 + u64::from(self.millis);
        (3_600_000 - into).max(1)
    }
}

impl fmt::Display for Stamp {
    /// `2026-10-12 15:04:05.123 +02:00`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.date, self.clock(), self.offset())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> Date {
        Date::new(y, m, day).expect("valid")
    }

    #[test]
    fn dates_round_trip_through_day_numbers() {
        assert_eq!(d(1970, 1, 1).days_since_epoch(), 0);
        assert_eq!(d(2000, 3, 1).days_since_epoch(), 11_017);
        for n in [-800_000, -1, 0, 1, 19_000, 20_738, 400_000] {
            assert_eq!(Date::from_days(n).days_since_epoch(), n);
        }
        assert_eq!(d(2026, 12, 31).add_days(1), d(2027, 1, 1));
        assert_eq!(d(2028, 3, 1).add_days(-1), d(2028, 2, 29));
        assert_eq!(d(2026, 10, 12).add_days(-14), d(2026, 9, 28));
    }

    #[test]
    fn parses_only_real_dates() {
        assert_eq!(Date::parse("2026-10-12"), Some(d(2026, 10, 12)));
        assert_eq!(Date::parse("2026-02-29"), None);
        assert_eq!(Date::parse("2028-02-29"), Some(d(2028, 2, 29)));
        assert_eq!(Date::parse("2026-13-01"), None);
        assert_eq!(Date::parse("2026-1-01"), None);
        assert_eq!(Date::parse("yesterday"), None);
        assert_eq!(Date::from_file_name("2026-10-12.log"), Some(d(2026, 10, 12)));
        assert_eq!(Date::from_file_name("stage-manager.log"), None);
        assert_eq!(Date::from_file_name("2026-10-12.log.1"), None);
        assert_eq!(d(2026, 1, 5).file_name(), "2026-01-05.log");
    }

    #[test]
    fn stamps_print_local_time_with_the_offset() {
        let s = Stamp { date: d(2026, 10, 12), hour: 15, minute: 4, second: 5, millis: 7, offset_minutes: 120 };
        assert_eq!(s.to_string(), "2026-10-12 15:04:05.007 +02:00");
        let s = Stamp { offset_minutes: -(7 * 60), ..s };
        assert_eq!(s.offset(), "-07:00");
        let s = Stamp { offset_minutes: 330, ..s };
        assert_eq!(s.offset(), "+05:30");
        assert_eq!(s.hhmm(), "15:04");
    }

    #[test]
    fn utc_fallback_and_next_hour() {
        let s = Stamp::utc(86_400 + 3_600 + 61, 250);
        assert_eq!(s.to_string(), "1970-01-02 01:01:01.250 +00:00");
        assert_eq!(s.millis_to_next_hour(), 3_600_000 - 61_250);
        let top = Stamp { minute: 0, second: 0, millis: 0, ..s };
        assert_eq!(top.millis_to_next_hour(), 3_600_000);
    }
}
