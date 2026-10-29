//! The day's log file: `YYYY-MM-DD.log` in the log directory, opened for appending (the app,
//! its guardian and one-shot commands share it; each record is one `write`), switched to the
//! next day's file when a record carries a new date, and capped: once the file reaches the
//! day's limit only warnings, errors, marks and counts are written, and once it reaches twice
//! the limit only marks and counts ([`Weight`]: a warning in a loop cannot fill the disk,
//! review C8); the first record dropped at each stage writes one notice instead.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::time::Date;

/// How much a record weighs against the day's cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Weight {
    /// An info, debug or trace line: dropped once the file reaches the cap.
    Detail,
    /// A warning or an error: dropped once the file reaches twice the cap.
    Warning,
    /// A mark or a count line: always written (each is made by hand or at most hourly).
    Always,
}

impl Weight {
    /// The size from which records of this weight are dropped, for a cap of `max_bytes`.
    fn limit(self, max_bytes: u64) -> u64 {
        match self {
            Weight::Detail => max_bytes,
            Weight::Warning => max_bytes.saturating_mul(2),
            Weight::Always => u64::MAX,
        }
    }
}

/// What [`DayWriter::write`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Written {
    /// The record is in the file.
    Yes,
    /// The day's file is full; the record was dropped.
    Dropped,
    /// The day's file is full; the record was dropped and the notice written in its place.
    Notice,
}

/// Appends records to the day's file.
#[derive(Debug)]
pub struct DayWriter {
    dir: PathBuf,
    open: Option<(Date, File)>,
    /// The day this process wrote its cap notice for, per weight cut ([`Weight::Detail`],
    /// [`Weight::Warning`]).
    noticed: [Option<Date>; 2],
}

impl DayWriter {
    /// A writer for files in `dir` (created on the first write).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, open: None, noticed: [None; 2] }
    }

    /// The log directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The day of the file open now.
    pub fn day(&self) -> Option<Date> {
        self.open.as_ref().map(|(d, _)| *d)
    }

    /// The file of `date`.
    pub fn path(&self, date: Date) -> PathBuf {
        self.dir.join(date.file_name())
    }

    /// The size of `date`'s file as it is now (every process's writes), 0 when missing.
    pub fn size(&mut self, date: Date) -> u64 {
        match self.file(date) {
            Ok(f) => f.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        }
    }

    fn file(&mut self, date: Date) -> io::Result<&mut File> {
        if self.day() != Some(date) {
            std::fs::create_dir_all(&self.dir)?;
            let mut options = OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                // Window titles may be in the log: readable by its owner only.
                options.mode(0o600);
            }
            let file = options.open(self.path(date))?;
            self.open = Some((date, file));
        }
        match self.open.as_mut() {
            Some((_, f)) => Ok(f),
            None => Err(io::Error::other("log file not open")),
        }
    }

    /// Appends `text` (whole lines) to `date`'s file. Text of `weight` past its limit for a
    /// cap of `max_bytes` ([`Weight`]) is dropped; the first text dropped that day at each
    /// weight writes `notice(weight)` in its place (once per day and weight in this process).
    pub fn write(
        &mut self,
        date: Date,
        text: &str,
        weight: Weight,
        max_bytes: u64,
        notice: impl FnOnce(Weight) -> String,
    ) -> io::Result<Written> {
        if self.size(date) >= weight.limit(max_bytes) {
            let slot = &mut self.noticed[usize::from(weight == Weight::Warning)];
            if *slot == Some(date) {
                return Ok(Written::Dropped);
            }
            *slot = Some(date);
            let text = notice(weight);
            self.file(date)?.write_all(text.as_bytes())?;
            return Ok(Written::Notice);
        }
        self.file(date)?.write_all(text.as_bytes())?;
        Ok(Written::Yes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(n: u32) -> Date {
        Date::new(2026, 10, n).expect("valid")
    }

    #[test]
    fn a_new_date_opens_the_next_days_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut w = DayWriter::new(dir.path().join("logs"));
        assert_eq!(w.write(day(12), "a\n", Weight::Detail, 1000, |_| String::new()).ok(), Some(Written::Yes));
        assert_eq!(w.day(), Some(day(12)));
        w.write(day(12), "b\n", Weight::Detail, 1000, |_| String::new()).expect("write");
        w.write(day(13), "c\n", Weight::Detail, 1000, |_| String::new()).expect("write");
        assert_eq!(w.day(), Some(day(13)));
        let read = |d| std::fs::read_to_string(dir.path().join("logs").join(Date::file_name(d))).unwrap_or_default();
        assert_eq!(read(day(12)), "a\nb\n");
        assert_eq!(read(day(13)), "c\n");
    }

    #[test]
    fn a_full_day_keeps_only_important_lines_and_says_so_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut w = DayWriter::new(dir.path().to_path_buf());
        let notice = |_| "NOTICE\n".to_string();
        assert_eq!(w.write(day(12), "0123456789\n", Weight::Detail, 10, notice).ok(), Some(Written::Yes));
        assert_eq!(w.write(day(12), "debug\n", Weight::Detail, 10, notice).ok(), Some(Written::Notice));
        assert_eq!(w.write(day(12), "debug\n", Weight::Detail, 10, notice).ok(), Some(Written::Dropped));
        assert_eq!(w.write(day(12), "WARN\n", Weight::Warning, 10, notice).ok(), Some(Written::Yes));
        // The next day starts empty.
        assert_eq!(w.write(day(13), "fresh\n", Weight::Detail, 10, notice).ok(), Some(Written::Yes));
        let text = std::fs::read_to_string(dir.path().join("2026-10-12.log")).unwrap_or_default();
        assert_eq!(text, "0123456789\nNOTICE\nWARN\n");
    }

    /// Review C8: a warning in a loop stops at twice the cap; marks and counts still go in.
    #[test]
    fn warnings_stop_at_twice_the_cap_and_marks_never() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut w = DayWriter::new(dir.path().to_path_buf());
        let notice = |weight: Weight| format!("NOTICE {weight:?}\n");
        for _ in 0..10 {
            w.write(day(12), "WARN 1234\n", Weight::Warning, 20, notice).expect("write");
        }
        assert_eq!(w.write(day(12), "MARK\n", Weight::Always, 20, notice).ok(), Some(Written::Yes));
        let text = std::fs::read_to_string(dir.path().join("2026-10-12.log")).unwrap_or_default();
        assert_eq!(text, "WARN 1234\n".repeat(4) + "NOTICE Warning\nMARK\n");
    }

    #[test]
    fn two_writers_share_one_file_and_see_each_others_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut a = DayWriter::new(dir.path().to_path_buf());
        let mut b = DayWriter::new(dir.path().to_path_buf());
        a.write(day(12), "app\n", Weight::Detail, 100, |_| String::new()).expect("write");
        b.write(day(12), "guardian\n", Weight::Detail, 100, |_| String::new()).expect("write");
        a.write(day(12), "app again\n", Weight::Detail, 100, |_| String::new()).expect("write");
        assert_eq!(a.size(day(12)), "app\nguardian\napp again\n".len() as u64);
        let text = std::fs::read_to_string(a.path(day(12))).unwrap_or_default();
        assert_eq!(text, "app\nguardian\napp again\n");
    }
}
