//! The day's log file: `YYYY-MM-DD.log` in the log directory, opened for appending (the app,
//! its guardian and one-shot commands share it; each record is one `write`), switched to the
//! next day's file when a record carries a new date, and capped: once the file reaches the
//! day's limit only important records (warnings, errors, marks, counts) are written, and the
//! first record dropped writes one notice instead.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::time::Date;

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
    /// The day this process wrote its cap notice for.
    noticed: Option<Date>,
}

impl DayWriter {
    /// A writer for files in `dir` (created on the first write).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, open: None, noticed: None }
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

    /// Appends `text` (whole lines) to `date`'s file. When the file has reached `max_bytes`,
    /// only `important` text goes in; the first other text dropped that day writes
    /// `notice()` in its place (once per day in this process).
    pub fn write(
        &mut self,
        date: Date,
        text: &str,
        important: bool,
        max_bytes: u64,
        notice: impl FnOnce() -> String,
    ) -> io::Result<Written> {
        let full = self.size(date) >= max_bytes;
        if full && !important {
            if self.noticed == Some(date) {
                return Ok(Written::Dropped);
            }
            self.noticed = Some(date);
            let text = notice();
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
        assert_eq!(w.write(day(12), "a\n", false, 1000, String::new).ok(), Some(Written::Yes));
        assert_eq!(w.day(), Some(day(12)));
        w.write(day(12), "b\n", false, 1000, String::new).expect("write");
        w.write(day(13), "c\n", false, 1000, String::new).expect("write");
        assert_eq!(w.day(), Some(day(13)));
        let read = |d| std::fs::read_to_string(dir.path().join("logs").join(Date::file_name(d))).unwrap_or_default();
        assert_eq!(read(day(12)), "a\nb\n");
        assert_eq!(read(day(13)), "c\n");
    }

    #[test]
    fn a_full_day_keeps_only_important_lines_and_says_so_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut w = DayWriter::new(dir.path().to_path_buf());
        let notice = || "NOTICE\n".to_string();
        assert_eq!(w.write(day(12), "0123456789\n", false, 10, notice).ok(), Some(Written::Yes));
        assert_eq!(w.write(day(12), "debug\n", false, 10, notice).ok(), Some(Written::Notice));
        assert_eq!(w.write(day(12), "debug\n", false, 10, notice).ok(), Some(Written::Dropped));
        assert_eq!(w.write(day(12), "WARN\n", true, 10, notice).ok(), Some(Written::Yes));
        // The next day starts empty.
        assert_eq!(w.write(day(13), "fresh\n", false, 10, notice).ok(), Some(Written::Yes));
        let text = std::fs::read_to_string(dir.path().join("2026-10-12.log")).unwrap_or_default();
        assert_eq!(text, "0123456789\nNOTICE\nWARN\n");
    }

    #[test]
    fn two_writers_share_one_file_and_see_each_others_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut a = DayWriter::new(dir.path().to_path_buf());
        let mut b = DayWriter::new(dir.path().to_path_buf());
        a.write(day(12), "app\n", false, 100, String::new).expect("write");
        b.write(day(12), "guardian\n", false, 100, String::new).expect("write");
        a.write(day(12), "app again\n", false, 100, String::new).expect("write");
        assert_eq!(a.size(day(12)), "app\nguardian\napp again\n".len() as u64);
        let text = std::fs::read_to_string(a.path(day(12))).unwrap_or_default();
        assert_eq!(text, "app\nguardian\napp again\n");
    }
}
