//! Which log files to delete: days older than the kept number of days, then the oldest days
//! until the folder fits its total cap. Today's file is never deleted.

use super::time::Date;

/// A log file in the log directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFile {
    /// Its file name.
    pub name: String,
    /// Its day: from a `YYYY-MM-DD.log` name, or the day it was last written for older names.
    pub date: Date,
    /// Its size.
    pub bytes: u64,
}

/// File names written by earlier versions (one file, rotated once), deleted by age like the
/// day files.
pub const LEGACY_NAMES: &[&str] = &["stage-manager.log", "stage-manager.log.1"];

/// The names to delete so that only the last `days` days (today included) stay and the
/// files left add up to at most `max_total` bytes (oldest first; never today's file).
pub fn to_delete(files: &[LogFile], today: Date, days: u32, max_total: u64) -> Vec<String> {
    let first_kept = today.add_days(-(i64::from(days.max(1)) - 1));
    let mut doomed: Vec<String> = Vec::new();
    let mut kept: Vec<&LogFile> = Vec::new();
    for f in files {
        if f.date < first_kept {
            doomed.push(f.name.clone());
        } else {
            kept.push(f);
        }
    }
    kept.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.name.cmp(&b.name)));
    let mut total: u64 = kept.iter().map(|f| f.bytes).sum();
    for f in kept {
        if total <= max_total {
            break;
        }
        if f.date >= today {
            continue;
        }
        total -= f.bytes;
        doomed.push(f.name.clone());
    }
    doomed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, day: u32, mb: u64) -> LogFile {
        LogFile { name: name.into(), date: Date::new(2026, 10, day).expect("valid"), bytes: mb << 20 }
    }

    fn today() -> Date {
        Date::new(2026, 10, 20).expect("valid")
    }

    #[test]
    fn days_older_than_the_kept_number_go() {
        let files = [file("a", 6, 1), file("b", 7, 1), file("c", 19, 1), file("d", 20, 1)];
        // 14 days kept: the 7th to the 20th.
        assert_eq!(to_delete(&files, today(), 14, 500 << 20), ["a"]);
        assert_eq!(to_delete(&files, today(), 1, 500 << 20), ["a", "b", "c"]);
        // 0 means 1: today's file always stays.
        assert_eq!(to_delete(&files, today(), 0, 500 << 20), ["a", "b", "c"]);
    }

    #[test]
    fn the_total_cap_takes_the_oldest_days_first_but_never_today() {
        let files = [file("d18", 18, 200), file("d20", 20, 300), file("d17", 17, 100), file("d19", 19, 50)];
        assert_eq!(to_delete(&files, today(), 14, 500 << 20), ["d17", "d18"]);
        assert!(to_delete(&files, today(), 14, 650 << 20).is_empty());
        // Today's alone is over the cap: everything else goes, today's stays.
        assert_eq!(to_delete(&files, today(), 14, 100 << 20), ["d17", "d18", "d19"]);
    }

    #[test]
    fn a_file_dated_after_today_is_kept_by_age_and_counted() {
        let files = [file("future", 25, 400), file("d19", 19, 200)];
        assert_eq!(to_delete(&files, today(), 14, 500 << 20), ["d19"]);
    }
}
