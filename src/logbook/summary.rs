//! Reading a day's log back: `stage-manager log summary [YYYY-MM-DD]`.
//!
//! The summary is short enough to paste into a chat: the day's counts (the last `COUNT`
//! line of every app run, added up), the runs and unclean exits, every problem mark with its
//! line number, every warning and error, deduplicated (numbers folded) with how often and
//! when, and the day's last restore after a restart (its one line, no titles).

use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

use regex::Regex;

use super::counters::{ALL, Count, Counts};
use super::time::Date;
use super::{CAP_NOTICE, COUNT_MESSAGE, MARK_BEGIN, UNCLEAN_EXIT};

/// Most kinds of warnings and errors listed.
const MAX_PROBLEMS: usize = 30;

/// A problem's text is cut to this many characters.
const MAX_TEXT: usize = 150;

/// One log line's parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line<'a> {
    /// `HH:MM:SS.mmm`.
    pub time: &'a str,
    /// `app`, `guardian`, `ctl`, …
    pub process: &'a str,
    /// The process id.
    pub pid: u32,
    /// `TRACE` … `ERROR`, `MARK` or `COUNT`.
    pub level: &'a str,
    /// The rest: target and message, or a mark's or count's text.
    pub text: &'a str,
}

/// The restore's one line ([`crate::engine::restore::Summary`]) in a log line's text.
fn restore_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"restore: \d+ windows? into \d+ stages? on \d+ Desktops? in \d+ s; \d+ pending; \d+ by Apple's rule(; \d+ at the corner)?(; \d+ from other Desktops)?")
            .unwrap_or_else(|e| panic!("bad restore pattern: {e}"))
    })
}

fn line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\d{4}-\d\d-\d\d (\d\d:\d\d:\d\d\.\d{3}) [+-]\d\d:\d\d ([a-z-]+)\[(\d+)\] ([A-Z]+) +(.*)$")
            .unwrap_or_else(|e| panic!("bad line pattern: {e}"))
    })
}

/// Parses one line; `None` for anything that is not a log line (a mark's dump, say).
pub fn parse_line(line: &str) -> Option<Line<'_>> {
    let c = line_regex().captures(line)?;
    let get = |i| c.get(i).map(|m| m.as_str());
    Some(Line { time: get(1)?, process: get(2)?, pid: get(3)?.parse().ok()?, level: get(4)?, text: get(5)? })
}

/// A warning or error, with how often it came.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// `WARN` or `ERROR`.
    pub level: String,
    /// The process that logged it.
    pub process: String,
    /// The text with numbers folded to `#`.
    pub text: String,
    /// How often.
    pub count: usize,
    /// `HH:MM` of the first.
    pub first: String,
    /// `HH:MM` of the last.
    pub last: String,
}

/// A problem mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkRef {
    /// `HH:MM:SS`.
    pub time: String,
    /// What the user wrote, or empty.
    pub note: String,
    /// Its line in the file (1-based).
    pub line: usize,
}

/// A day's log, summed up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// The day.
    pub date: Option<Date>,
    /// Lines in the file.
    pub lines: usize,
    /// Bytes in the file.
    pub bytes: usize,
    /// The app's process ids, in order of their first line.
    pub app_runs: Vec<u32>,
    /// Exits the guardian found unclean.
    pub unclean_exits: usize,
    /// The counts of every app run (the last `COUNT` line of each), added up.
    pub counts: Counts,
    /// Runs that wrote counts.
    pub counted_runs: usize,
    /// Warning lines.
    pub warnings: usize,
    /// Error lines.
    pub errors: usize,
    /// Warnings and errors, deduplicated: errors first, then the most frequent.
    pub problems: Vec<Problem>,
    /// Problem marks, in order.
    pub marks: Vec<MarkRef>,
    /// The day's file reached its cap.
    pub capped: bool,
    /// The last restore line of the day (`HH:MM:SS`, the line from "restore: " on).
    pub last_restore: Option<(String, String)>,
}

/// Folds numbers to `#` and lists of them to `[#…]`, so repeats of one problem with other
/// window ids count as one.
pub fn fold(text: &str) -> String {
    static DIGITS: OnceLock<Regex> = OnceLock::new();
    static LISTS: OnceLock<Regex> = OnceLock::new();
    let digits = DIGITS.get_or_init(|| Regex::new(r"\d+(\.\d+)?").unwrap_or_else(|e| panic!("{e}")));
    let lists = LISTS.get_or_init(|| Regex::new(r"\[#(, #)+\]").unwrap_or_else(|e| panic!("{e}")));
    let folded = digits.replace_all(text, "#");
    let folded = lists.replace_all(&folded, "[#…]");
    let mut out: String = folded.chars().take(MAX_TEXT).collect();
    if folded.chars().count() > MAX_TEXT {
        out.push('…');
    }
    out
}

/// Sums up a day's log text.
pub fn summarize(date: Option<Date>, text: &str) -> Summary {
    let mut s = Summary { date, bytes: text.len(), ..Summary::default() };
    let mut last_counts: Vec<(u32, Counts)> = Vec::new();
    let mut index: HashMap<(String, String, String), usize> = HashMap::new();
    for (n, raw) in text.lines().enumerate() {
        s.lines += 1;
        let Some(line) = parse_line(raw) else { continue };
        if line.process == "app" && !s.app_runs.contains(&line.pid) {
            s.app_runs.push(line.pid);
        }
        match line.level {
            "COUNT" if line.text.starts_with(COUNT_MESSAGE) => {
                let counts = Counts::parse_fields(line.text);
                match last_counts.iter_mut().find(|(pid, _)| *pid == line.pid) {
                    Some(slot) => slot.1 = counts,
                    None => last_counts.push((line.pid, counts)),
                }
            }
            "INFO" => {
                if let Some(m) = restore_regex().find(line.text) {
                    s.last_restore = Some((line.time[..8].to_string(), m.as_str().to_string()));
                }
            }
            "MARK" => {
                if let Some(rest) = line.text.strip_prefix(MARK_BEGIN) {
                    let note = rest.trim_end_matches('=').trim();
                    let note = if note == super::NO_NOTE { "" } else { note };
                    s.marks.push(MarkRef { time: line.time[..8].to_string(), note: note.to_string(), line: n + 1 });
                }
            }
            "WARN" | "ERROR" => {
                if line.level == "WARN" {
                    s.warnings += 1;
                } else {
                    s.errors += 1;
                }
                if line.text.contains(CAP_NOTICE) {
                    s.capped = true;
                }
                if line.text.contains(UNCLEAN_EXIT) {
                    s.unclean_exits += 1;
                }
                let key = (line.level.to_string(), line.process.to_string(), fold(line.text));
                let hhmm = line.time[..5].to_string();
                match index.get(&key) {
                    Some(&i) => {
                        s.problems[i].count += 1;
                        s.problems[i].last = hhmm;
                    }
                    None => {
                        index.insert(key.clone(), s.problems.len());
                        s.problems.push(Problem {
                            level: key.0,
                            process: key.1,
                            text: key.2,
                            count: 1,
                            first: hhmm.clone(),
                            last: hhmm,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    s.counted_runs = last_counts.len();
    for (_, c) in &last_counts {
        s.counts.add(c);
    }
    s.problems.sort_by(|a, b| (b.level == "ERROR").cmp(&(a.level == "ERROR")).then(b.count.cmp(&a.count)));
    s
}

fn size(bytes: usize) -> String {
    if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / f64::from(1 << 20))
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let day = self.date.map(|d| d.to_string()).unwrap_or_else(|| "(no date)".into());
        write!(f, "Stage Manager log {day}: {} lines, {}", self.lines, size(self.bytes))?;
        if self.capped {
            write!(f, " (reached the day's cap: later debug lines were dropped)")?;
        }
        writeln!(f)?;
        let pids: Vec<String> = self.app_runs.iter().map(u32::to_string).collect();
        writeln!(
            f,
            "runs: {} (pids {}); unclean exits: {}",
            self.app_runs.len(),
            if pids.is_empty() { "-".into() } else { pids.join(", ") },
            self.unclean_exits
        )?;
        let counts: Vec<String> = ALL
            .iter()
            .filter(|c| !matches!(c, Count::Warning | Count::Error))
            .map(|c| format!("{} {}", c.label(), self.counts.get(*c)))
            .collect();
        writeln!(f, "counts ({}): {}", plural(self.counted_runs, "run", "runs"), counts.join(", "))?;
        if let Some((time, line)) = &self.last_restore {
            writeln!(f, "last restore after a restart: {time} {line}")?;
        }
        writeln!(f, "problem marks: {}", self.marks.len())?;
        for (i, m) in self.marks.iter().enumerate() {
            let note = if m.note.is_empty() { "(no note)".to_string() } else { format!("\"{}\"", m.note) };
            writeln!(f, "  #{} {} {note} (line {})", i + 1, m.time, m.line)?;
        }
        writeln!(
            f,
            "warnings and errors: {}, {} ({})",
            plural(self.warnings, "warning", "warnings"),
            plural(self.errors, "error", "errors"),
            plural(self.problems.len(), "kind", "kinds")
        )?;
        for p in self.problems.iter().take(MAX_PROBLEMS) {
            let when = if p.first == p.last { p.first.clone() } else { format!("{}-{}", p.first, p.last) };
            writeln!(f, "  {:<5} {:>4}x {:<11} {:<8} {}", p.level, p.count, when, p.process, p.text)?;
        }
        if self.problems.len() > MAX_PROBLEMS {
            writeln!(f, "  … and {} more kinds", self.problems.len() - MAX_PROBLEMS)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: &str = "\
2026-10-12 09:00:00.001 +02:00 app[100] INFO  app: Stage Manager starting version=\"0.1.0\" pid=100
2026-10-12 09:00:00.002 +02:00 guardian[101] INFO  platform::guardian: guardian watching parent=100
2026-10-12 09:10:00.000 +02:00 app[100] WARN  platform::wm_thread: windows still parked after unparking (another Space?) windows=[4551, 4550]
2026-10-12 10:00:00.000 +02:00 app[100] COUNT counts kind=hour since=09:00 switches=3 card_clicks=2 captures_ok=40 warnings=1
2026-10-12 10:30:00.000 +02:00 app[100] MARK  ===== problem mark: the strip vanished =====
{
  \"state\": \"…\"
}
  | 2026-10-12 10:29:59.000 +02:00 app[100] ERROR platform::x: inside the ring, not counted
2026-10-12 10:30:00.010 +02:00 app[100] MARK  ===== end of problem mark =====
2026-10-12 11:00:00.000 +02:00 app[100] COUNT counts kind=quit since=09:00 switches=5 card_clicks=2 captures_ok=60 warnings=2
2026-10-12 11:00:01.000 +02:00 app[100] WARN  platform::wm_thread: windows still parked after unparking (another Space?) windows=[4600, 4601, 4602]
2026-10-12 12:00:00.000 +02:00 guardian[201] WARN  platform::guardian: guardian: the app exited without a clean exit; running the recovery scan parent=200
2026-10-12 12:00:00.500 +02:00 app[300] ERROR platform::safety: recovery: cannot move window: AX error -25204
2026-10-12 13:00:00.000 +02:00 app[300] MARK  ===== problem mark: (no note) =====
2026-10-12 13:00:00.001 +02:00 app[300] MARK  ===== end of problem mark =====
2026-10-12 14:00:00.000 +02:00 app[300] COUNT counts kind=hour since=12:00 switches=1 new_windows=4
2026-10-12 23:59:00.000 +02:00 ctl[400] INFO  ctl: the app is not reachable
";

    #[test]
    fn parses_log_lines_and_skips_anything_else() {
        let l = parse_line("2026-10-12 09:10:00.000 +02:00 app[100] WARN  platform::x: hello world").expect("line");
        assert_eq!((l.time, l.process, l.pid, l.level, l.text), ("09:10:00.000", "app", 100, "WARN", "platform::x: hello world"));
        let c = parse_line("2026-10-12 10:00:00.000 -07:00 app[1] COUNT counts kind=hour").expect("count");
        assert_eq!(c.level, "COUNT");
        assert!(parse_line("  | 2026-10-12 09:10:00.000 +02:00 app[100] WARN  x").is_none());
        assert!(parse_line("{").is_none());
        assert!(parse_line("2026-10-12T09:10:00.000Z  WARN stage_manager::x: old format").is_none());
    }

    #[test]
    fn folds_numbers_and_id_lists() {
        assert_eq!(fold("windows=[4551, 4550] at 1.5 s"), "windows=[#…] at # s");
        assert_eq!(fold("id=12"), "id=#");
        assert_eq!(fold(&"x".repeat(400)).chars().count(), MAX_TEXT + 1);
    }

    #[test]
    fn sums_counts_per_run_and_dedupes_problems() {
        let s = summarize(Date::new(2026, 10, 12), DAY);
        assert_eq!(s.app_runs, [100, 300]);
        assert_eq!(s.counted_runs, 2);
        // The last COUNT line of run 100 (quit) plus run 300's hour line.
        assert_eq!(s.counts.get(Count::Switch), 6);
        assert_eq!(s.counts.get(Count::CaptureOk), 60);
        assert_eq!(s.counts.get(Count::NewWindow), 4);
        assert_eq!((s.warnings, s.errors, s.unclean_exits), (3, 1, 1));
        assert_eq!(s.problems.len(), 3);
        assert_eq!(s.problems[0].level, "ERROR");
        let parked = &s.problems[1];
        assert_eq!((parked.count, parked.first.as_str(), parked.last.as_str()), (2, "09:10", "11:00"));
        assert_eq!(s.marks.len(), 2);
        assert_eq!(s.marks[0], MarkRef { time: "10:30:00".into(), note: "the strip vanished".into(), line: 5 });
        assert_eq!(s.marks[1].note, "");
        assert!(!s.capped);
    }

    #[test]
    fn the_printout_is_short_and_names_everything() {
        let text = summarize(Date::new(2026, 10, 12), DAY).to_string();
        assert!(text.starts_with("Stage Manager log 2026-10-12: 18 lines"));
        assert!(text.contains("runs: 2 (pids 100, 300); unclean exits: 1"));
        assert!(text.contains("switches 6, card clicks 2"));
        assert!(text.contains("#1 10:30:00 \"the strip vanished\" (line 5)"));
        assert!(text.contains("#2 13:00:00 (no note)"));
        assert!(text.contains("3 warnings, 1 error (3 kinds)"));
        assert!(text.contains("WARN     2x 09:10-11:00 app      platform::wm_thread: windows still parked"));
        assert!(text.lines().count() <= 12, "{text}");
    }

    #[test]
    fn the_last_restore_line_is_repeated() {
        let line = crate::engine::restore::Summary { windows: 9, stages: 4, desktops: 2, seconds: 41, pending: 2, by_apple: 1, corner: 0, pooled: 3 };
        let one = crate::engine::restore::Summary { windows: 1, stages: 1, desktops: 1, ..Default::default() };
        let text = format!(
            "2026-10-12 08:01:00.000 +02:00 app[1] INFO  platform::controller: {one}\n\
             2026-10-12 08:02:11.000 +02:00 app[1] INFO  platform::controller: restore: the restore window ends why=\"apps went quiet\"\n\
             2026-10-12 08:02:11.001 +02:00 app[1] INFO  platform::controller: {line}\n"
        );
        let s = summarize(Date::new(2026, 10, 12), &text);
        assert_eq!(s.last_restore, Some(("08:02:11".into(), line.to_string())));
        assert!(s.to_string().contains("last restore after a restart: 08:02:11 restore: 9 windows into 4 stages on 2 Desktops in 41 s; 2 pending; 1 by Apple's rule"));
        assert!(!summarize(None, DAY).to_string().contains("last restore"), "a day without one says nothing");
    }

    #[test]
    fn a_capped_day_says_so() {
        let text = format!("2026-10-12 09:00:00.000 +02:00 app[1] WARN  logbook: {CAP_NOTICE} (100 MB)\n");
        assert!(summarize(None, &text).capped);
    }
}
