//! The daily log, pure Rust (the tracing layer that feeds it lives in
//! `platform::logging`).
//!
//! One file per local day, `YYYY-MM-DD.log`, shared by the app, its guardian and one-shot
//! commands. Every line starts with the local time, its UTC offset and the process:
//!
//! ```text
//! 2026-10-12 15:04:05.123 +02:00 app[6612] INFO  platform::controller: new window id=4711 …
//! ```
//!
//! The level column also holds `COUNT` (the day's counters, [`counters`]) and `MARK` (a
//! problem mark: a delimited block with a state dump and the last seconds of detailed lines).

pub mod counters;
pub mod retention;
pub mod ring;
pub mod summary;
pub mod time;
pub mod titles;
pub mod writer;

use time::Stamp;

/// The message that starts every `COUNT` line.
pub const COUNT_MESSAGE: &str = "counts";

/// The start of a problem mark's first line; the note follows.
pub const MARK_BEGIN: &str = "===== problem mark: ";

/// A problem mark's last line.
pub const MARK_END: &str = "===== end of problem mark =====";

/// The note of a mark made without one.
pub const NO_NOTE: &str = "(no note)";

/// Part of the line written once a day's file reaches its cap.
pub const CAP_NOTICE: &str = "this day's log reached its cap; from now on only warnings, errors, marks and counts are written";

/// Part of the guardian's line after an app exit without the clean-exit marker.
pub const UNCLEAN_EXIT: &str = "the app exited without a clean exit";

/// A line's start: `<stamp> <process>[<pid>] <LEVEL> ` (the level padded to five).
pub fn prefix(stamp: &Stamp, process: &str, pid: u32, level: &str) -> String {
    format!("{stamp} {process}[{pid}] {level:<5} ")
}

/// A log target without the crate's own prefix (`stage_manager::platform::x` → `platform::x`).
pub fn short_target(target: &str) -> &str {
    target.strip_prefix("stage_manager::").unwrap_or(target)
}

/// A note made safe for one line: control characters become spaces, at most 200 characters.
pub fn clean_note(note: &str) -> String {
    let one_line: String = note.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    one_line.trim().chars().take(200).collect()
}

/// A problem mark: the delimited block written into the day's log. `sections` are
/// `(name, text)` pairs (a JSON dump each); `recent` are the last seconds of detailed log
/// lines (each ending in a newline), indented with `  | ` so they are never read as lines of
/// their own.
pub fn mark_block(begin: &str, end: &str, note: &str, sections: &[(&str, String)], recent: &[String]) -> String {
    let note = if note.is_empty() { NO_NOTE.to_string() } else { note.to_string() };
    let mut out = format!("{begin}{MARK_BEGIN}{note} =====\n");
    for (name, text) in sections {
        out.push_str(&format!("--- {name}\n{}\n", text.trim_end()));
    }
    out.push_str(&format!("--- the last {} detailed lines\n", recent.len()));
    for line in recent {
        out.push_str("  | ");
        out.push_str(line.trim_end_matches('\n'));
        out.push('\n');
    }
    out.push_str(&format!("{end}{MARK_END}\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::time::Date;
    use super::*;

    fn stamp() -> Stamp {
        Stamp {
            date: Date::new(2026, 10, 12).expect("valid"),
            hour: 15,
            minute: 4,
            second: 5,
            millis: 123,
            offset_minutes: 120,
        }
    }

    #[test]
    fn lines_start_with_time_offset_and_process() {
        assert_eq!(prefix(&stamp(), "app", 42, "INFO"), "2026-10-12 15:04:05.123 +02:00 app[42] INFO  ");
        assert_eq!(prefix(&stamp(), "guardian", 7, "ERROR"), "2026-10-12 15:04:05.123 +02:00 guardian[7] ERROR ");
        assert_eq!(short_target("stage_manager::platform::ctl"), "platform::ctl");
        assert_eq!(short_target("app"), "app");
    }

    #[test]
    fn a_mark_is_a_block_the_summary_finds_once() {
        let begin = prefix(&stamp(), "app", 42, "MARK");
        let recent = vec![format!("{}platform::x: a warning inside\n", prefix(&stamp(), "app", 42, "WARN"))];
        let block = mark_block(&begin, &begin, &clean_note("strip\ngone "), &[("state", "{\n  \"ok\": true\n}".into())], &recent);
        assert!(block.starts_with("2026-10-12 15:04:05.123 +02:00 app[42] MARK  ===== problem mark: strip gone =====\n"));
        assert!(block.contains("--- state\n{\n  \"ok\": true\n}\n--- the last 1 detailed lines\n  | 2026-10-12"));
        assert!(block.ends_with("MARK  ===== end of problem mark =====\n"));
        let s = summary::summarize(None, &block);
        assert_eq!(s.marks.len(), 1);
        assert_eq!(s.marks[0].note, "strip gone");
        assert_eq!(s.warnings, 0, "ring lines are not counted again");
    }
}
