//! Logging: one file per local day, `~/Library/Logs/Stage Manager/YYYY-MM-DD.log` (see
//! [`crate::logbook`] for the line format), shared by the app, its guardian and one-shot
//! commands, each line tagged with its process.
//!
//! - **Level:** `[log] level` (default `debug`: every decision the app makes, no per-move or
//!   per-frame lines), live on config reload; `RUST_LOG` overrides it.
//! - **Ring:** the last [`RING_KEEP`] of lines at `trace` stay in memory for a problem mark
//!   ([`write_mark`]), whatever the file's level.
//! - **Limits:** a day's file past `[log] max_mb_per_day` takes only warnings, errors, marks
//!   and counts; files older than `[log] days` and the oldest past `[log] max_mb_total` are
//!   deleted at launch and every hour (the app only).
//! - **Counts:** the app writes its counters ([`crate::logbook::counters`]) every hour, at
//!   the day's rollover (into the day that ends) and at quit.
//!
//! Nothing here ever logs through `tracing` while it holds the writer: a failure to write
//! goes to stderr once.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

use super::paths;
use crate::config::{Config, LogConfig};
use crate::logbook::counters::{self, Count};
use crate::logbook::retention::{self, LEGACY_NAMES, LogFile};
use crate::logbook::ring::Ring;
use crate::logbook::summary;
use crate::logbook::time::{Date, Stamp};
use crate::logbook::writer::DayWriter;
use crate::logbook::{self, CAP_NOTICE, COUNT_MESSAGE};

/// How long the ring keeps lines.
pub const RING_KEEP: Duration = Duration::from_secs(30);

/// Most lines in the ring.
const RING_MAX: usize = 5000;

/// What the ring keeps.
const RING_DIRECTIVES: &str = "info,stage_manager=trace";

/// Which process writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Process {
    /// The menu bar app: counts, the hourly ticker, pruning, stderr when it is a terminal.
    App,
    /// The guardian.
    Guardian,
    /// A one-shot command (`ctl`, `mark`, the strip demo).
    Tool {
        /// Its tag in the log.
        name: &'static str,
        /// Also print lines on stderr when it is a terminal.
        echo: bool,
    },
}

impl Process {
    fn tag(self) -> &'static str {
        match self {
            Process::App => "app",
            Process::Guardian => "guardian",
            Process::Tool { name, .. } => name,
        }
    }
}

struct Book {
    tag: &'static str,
    pid: u32,
    writer: Mutex<DayWriter>,
    /// The file's filter (the config's level, or `RUST_LOG`).
    file: RwLock<Targets>,
    /// `RUST_LOG` is set: the config's level is ignored.
    env_override: bool,
    /// The config's level is `debug` or `trace` (the menu's Mark a Problem shows).
    verbose: AtomicBool,
    ring_filter: Targets,
    ring: Mutex<Ring>,
    max_day: AtomicU64,
    max_total: AtomicU64,
    days: AtomicU32,
    /// This process writes `COUNT` lines.
    counting: bool,
    echo: bool,
    /// `HH:MM` the counts start from (launch, or the day's start).
    since: Mutex<String>,
    write_failed: AtomicBool,
}

static BOOK: OnceLock<Book> = OnceLock::new();

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The local time now (UTC when it cannot be read).
pub fn now() -> Stamp {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    local(d.as_secs() as i64, d.subsec_millis())
}

/// The local time of `secs` since the epoch.
fn local(secs: i64, millis: u32) -> Stamp {
    let t = secs as libc::time_t;
    // SAFETY: `tm` is plain data; all-zero is a valid value for localtime_r to overwrite.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call; localtime_r is thread-safe.
    let ok = !unsafe { libc::localtime_r(&t, &mut tm) }.is_null();
    let date = ok.then(|| Date::new(tm.tm_year + 1900, (tm.tm_mon + 1) as u32, tm.tm_mday as u32)).flatten();
    match date {
        Some(date) => Stamp {
            date,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
            millis,
            offset_minutes: (tm.tm_gmtoff / 60) as i32,
        },
        None => Stamp::utc(secs, millis),
    }
}

/// The file of `date` in the log directory.
pub fn day_file(date: Date) -> PathBuf {
    paths::log_dir().join(date.file_name())
}

fn read_config() -> LogConfig {
    std::fs::read_to_string(paths::config_file())
        .ok()
        .and_then(|t| Config::parse(&t).ok())
        .map(|p| p.config.log)
        .unwrap_or_default()
}

fn targets(directives: &str) -> Targets {
    directives.parse().unwrap_or_else(|_| Targets::new().with_default(LevelFilter::INFO))
}

const MB: u64 = 1 << 20;

/// Installs the global subscriber for `process`: the day's file, the ring, and (the app)
/// counting, pruning and the hourly ticker.
pub fn init(process: Process) {
    let config = read_config();
    let env = std::env::var("RUST_LOG").ok().filter(|v| !v.trim().is_empty());
    let env_filter = env.as_deref().and_then(|v| v.parse::<Targets>().ok());
    let file = env_filter.clone().unwrap_or_else(|| targets(config.level.directives()));
    let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let echo = tty
        && match process {
            Process::App => true,
            Process::Guardian => false,
            Process::Tool { echo, .. } => echo,
        };
    let mut book = Book::new(process, paths::log_dir(), &config, file);
    book.env_override = env_filter.is_some();
    book.echo = echo;
    if BOOK.set(book).is_err() {
        return;
    }
    let _ = tracing_subscriber::registry().with(BookLayer).try_init();
    if env.is_some() && env_filter.is_none() {
        tracing::warn!("RUST_LOG={env:?} is not a list of target=level directives; using [log] level");
    }
    if process == Process::App {
        tracing::info!(
            dir = %paths::log_dir().display(),
            level = env.as_deref().unwrap_or(config.level.directives()),
            days = config.days,
            max_mb_per_day = config.max_mb_per_day,
            max_mb_total = config.max_mb_total,
            "daily log"
        );
        prune(now().date);
        spawn_ticker();
    }
}

fn verbose(config: &LogConfig) -> bool {
    matches!(config.level, crate::config::LogLevel::Debug | crate::config::LogLevel::Trace)
}

/// Applies a reloaded `[log]` section: level (unless `RUST_LOG` is set) and limits.
pub fn apply_config(config: &LogConfig) {
    let Some(b) = BOOK.get() else { return };
    if !b.env_override
        && let Ok(mut f) = b.file.write()
    {
        *f = targets(config.level.directives());
    }
    b.verbose.store(verbose(config), Ordering::Relaxed);
    b.max_day.store(config.max_mb_per_day.saturating_mul(MB), Ordering::Relaxed);
    b.max_total.store(config.max_mb_total.saturating_mul(MB), Ordering::Relaxed);
    b.days.store(config.days, Ordering::Relaxed);
}

/// True while the log is verbose (`[log] level` debug or trace, the trial default): the
/// status menu shows Mark a Problem.
pub fn verbose_now() -> bool {
    BOOK.get().is_some_and(|b| b.verbose.load(Ordering::Relaxed))
}

impl Book {
    fn new(process: Process, dir: PathBuf, config: &LogConfig, file: Targets) -> Self {
        Self {
            tag: process.tag(),
            pid: std::process::id(),
            writer: Mutex::new(DayWriter::new(dir)),
            file: RwLock::new(file),
            env_override: false,
            verbose: AtomicBool::new(verbose(config)),
            ring_filter: targets(RING_DIRECTIVES),
            ring: Mutex::new(Ring::new(RING_KEEP, RING_MAX)),
            max_day: AtomicU64::new(config.max_mb_per_day.saturating_mul(MB)),
            max_total: AtomicU64::new(config.max_mb_total.saturating_mul(MB)),
            days: AtomicU32::new(config.days),
            counting: process == Process::App,
            echo: false,
            since: Mutex::new(now().hhmm()),
            write_failed: AtomicBool::new(false),
        }
    }

    fn wants(&self, meta: &Metadata<'_>) -> bool {
        self.ring_filter.would_enable(meta.target(), meta.level()) || self.file_wants(meta)
    }

    fn file_wants(&self, meta: &Metadata<'_>) -> bool {
        self.file.read().map(|f| f.would_enable(meta.target(), meta.level())).unwrap_or(true)
    }

    fn record(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let level = *meta.level();
        if level == Level::WARN {
            counters::bump(Count::Warning);
        } else if level == Level::ERROR {
            counters::bump(Count::Error);
        }
        let to_file = self.file_wants(meta);
        let to_ring = self.ring_filter.would_enable(meta.target(), meta.level());
        if !to_file && !to_ring {
            return;
        }
        let stamp = now();
        let mut line = logbook::prefix(&stamp, self.tag, self.pid, level.as_str());
        line.push_str(logbook::short_target(meta.target()));
        line.push_str(": ");
        let mut fields = Fields::default();
        event.record(&mut fields);
        line.push_str(&fields.message);
        line.push_str(&fields.rest);
        line.push('\n');
        if to_ring {
            lock(&self.ring).push(Instant::now(), line.clone());
        }
        if to_file {
            if self.echo {
                eprint!("{line}");
            }
            self.write(&stamp, &line, level <= Level::WARN);
        }
    }

    /// Writes `text` into the day's file (rolling over first when the day changed).
    fn write(&self, stamp: &Stamp, text: &str, important: bool) -> bool {
        let mut w = lock(&self.writer);
        self.roll_over(&mut w, stamp);
        let max = self.max_day.load(Ordering::Relaxed);
        let notice = || {
            format!("{}logbook: {CAP_NOTICE} ({} MB)\n", logbook::prefix(stamp, self.tag, self.pid, "WARN"), max / MB)
        };
        match w.write(stamp.date, text, important, max, notice) {
            Ok(_) => true,
            Err(e) => {
                if !self.write_failed.swap(true, Ordering::Relaxed) {
                    eprintln!("stage-manager: cannot write the log in {}: {e}", w.dir().display());
                }
                false
            }
        }
    }

    /// A new local day: the app writes the day's counts into the day that ends and starts
    /// counting again.
    fn roll_over(&self, w: &mut DayWriter, stamp: &Stamp) {
        let Some(old) = w.day() else { return };
        if old == stamp.date || !self.counting {
            return;
        }
        let counts = counters::global().take();
        let line = self.counts_line(stamp, "day", &counts.fields());
        let _ = w.write(old, &line, true, u64::MAX, String::new);
        *lock(&self.since) = "00:00".into();
    }

    fn counts_line(&self, stamp: &Stamp, kind: &str, fields: &str) -> String {
        let since = lock(&self.since).clone();
        format!("{}{COUNT_MESSAGE} kind={kind} since={since} {fields}\n", logbook::prefix(stamp, self.tag, self.pid, "COUNT"))
    }
}

/// Collects an event's message and its other fields (`key=value`, strings quoted).
#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }
}

struct BookLayer;

impl<S: Subscriber> tracing_subscriber::Layer<S> for BookLayer {
    fn register_callsite(&self, _meta: &'static Metadata<'static>) -> Interest {
        // The level changes live: ask every time.
        Interest::sometimes()
    }

    fn enabled(&self, meta: &Metadata<'_>, _cx: Context<'_, S>) -> bool {
        BOOK.get().is_some_and(|b| b.wants(meta))
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }

    fn on_event(&self, event: &Event<'_>, _cx: Context<'_, S>) {
        if let Some(b) = BOOK.get() {
            b.record(event);
        }
    }
}

/// Writes the app's counts (`kind`: `hour`, `quit`, `panic`) as one `COUNT` line.
pub fn write_counts(kind: &str) {
    let Some(b) = BOOK.get().filter(|b| b.counting) else { return };
    let stamp = now();
    // A rollover inside `write` resets the counters: take the snapshot after it.
    {
        let mut w = lock(&b.writer);
        b.roll_over(&mut w, &stamp);
    }
    let line = b.counts_line(&stamp, kind, &counters::global().snapshot().fields());
    b.write(&stamp, &line, true);
}

/// Every hour: the counts and a prune; the first line after midnight rolls the day over.
fn spawn_ticker() {
    let spawned = std::thread::Builder::new().name("log-ticker".into()).spawn(|| {
        loop {
            // A little past the hour, so the stamp is on the new hour's side.
            std::thread::sleep(Duration::from_millis(now().millis_to_next_hour() + 250));
            write_counts("hour");
            prune(now().date);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("cannot start the log ticker: {e}");
    }
}

/// Deletes day files older than `[log] days` and the oldest past `[log] max_mb_total`
/// (never today's), plus the earlier versions' single log by age.
fn prune(today: Date) {
    let Some(b) = BOOK.get() else { return };
    let dir = paths::log_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else { continue };
        let date = match Date::from_file_name(&name) {
            Some(d) => d,
            None if LEGACY_NAMES.contains(&name.as_str()) => {
                let Some(secs) = meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()) else { continue };
                local(secs.as_secs() as i64, 0).date
            }
            None => continue,
        };
        files.push(LogFile { name, date, bytes: meta.len() });
    }
    let doomed = retention::to_delete(
        &files,
        today,
        b.days.load(Ordering::Relaxed),
        b.max_total.load(Ordering::Relaxed),
    );
    let mut removed = Vec::new();
    for name in doomed {
        match std::fs::remove_file(dir.join(&name)) {
            Ok(()) => removed.push(name),
            Err(e) => tracing::warn!("cannot delete old log {name}: {e}"),
        }
    }
    if !removed.is_empty() {
        tracing::info!(files = ?removed, "old logs deleted");
    }
}

/// Where a problem mark went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marked {
    /// The day's file.
    pub file: PathBuf,
    /// `HH:MM:SS`, local.
    pub at: String,
}

/// Writes a problem mark into the day's file: the note, `sections` (name, text: a JSON dump
/// each) and the ring's last [`RING_KEEP`] of detailed lines. Written whatever the cap.
pub fn write_mark(note: &str, sections: &[(&str, String)]) -> Result<Marked, String> {
    let b = BOOK.get().ok_or("logging is not set up")?;
    let stamp = now();
    let recent = lock(&b.ring).recent(Instant::now());
    let begin = logbook::prefix(&stamp, b.tag, b.pid, "MARK");
    let end = logbook::prefix(&now(), b.tag, b.pid, "MARK");
    let block = logbook::mark_block(&begin, &end, &logbook::clean_note(note), sections, &recent);
    if !b.write(&stamp, &block, true) {
        return Err(format!("cannot write the log in {}", paths::log_dir().display()));
    }
    Ok(Marked { file: day_file(stamp.date), at: stamp.clock()[..8].to_string() })
}

/// `stage-manager log summary [DATE] | path [DATE]` (DATE: `YYYY-MM-DD`, `today`,
/// `yesterday`; default today): a day's summary, or its file's path.
pub fn command(args: &[String]) -> ExitCode {
    let usage = || {
        eprintln!("usage: stage-manager log summary [YYYY-MM-DD|today|yesterday] | path [YYYY-MM-DD|today|yesterday]");
        ExitCode::from(2)
    };
    let today = now().date;
    let date = match args.get(1).map(String::as_str) {
        None | Some("today") => Some(today),
        Some("yesterday") => Some(today.add_days(-1)),
        Some(d) => Date::parse(d),
    };
    let (Some(verb), Some(date)) = (args.first().map(String::as_str), date) else { return usage() };
    if args.len() > 2 {
        return usage();
    }
    let path = day_file(date);
    match verb {
        "path" => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        "summary" => match std::fs::read(&path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                print!("{}", summary::summarize(Some(date), &text));
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("no log for {date} at {}: {e}", path.display());
                ExitCode::from(1)
            }
        },
        _ => usage(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logbook::counters::Counts;

    #[test]
    fn local_time_matches_the_utc_clock_by_its_offset() {
        let secs = 1_791_810_245; // 2026-10-12 12:24:05 UTC
        let l = local(secs, 7);
        let utc = Stamp::utc(secs, 7);
        let minutes = |s: &Stamp| s.date.days_since_epoch() * 1440 + i64::from(s.hour * 60 + s.minute);
        assert_eq!(minutes(&l) - minutes(&utc), i64::from(l.offset_minutes));
        assert_eq!((l.second, l.millis), (utc.second, 7));
    }

    fn stamp(day: u32, hour: u32) -> Stamp {
        Stamp { date: Date::new(2026, 10, day).expect("valid"), hour, minute: 0, second: 1, millis: 0, offset_minutes: 120 }
    }

    #[test]
    fn the_app_rolls_over_at_a_new_date_with_the_days_counts_in_the_day_that_ends() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = LogConfig::default();
        let book = Book::new(Process::App, dir.path().to_path_buf(), &config, targets("info"));
        assert!(book.write(&stamp(12, 23), "before midnight\n", false));
        counters::bump(Count::Switch);
        counters::bump(Count::Switch);
        assert!(book.write(&stamp(13, 0), "after midnight\n", false));
        let read = |d: u32| std::fs::read_to_string(dir.path().join(format!("2026-10-{d}.log"))).unwrap_or_default();
        let old = read(12);
        assert!(old.starts_with("before midnight\n"), "{old}");
        let count = old.lines().nth(1).unwrap_or_default();
        assert!(count.contains("COUNT counts kind=day since="), "{count}");
        let line = summary::parse_line(count).expect("a log line");
        assert!(Counts::parse_fields(line.text).get(Count::Switch) >= 2, "{count}");
        assert_eq!(read(13), "after midnight\n");
        assert_eq!(*lock(&book.since), "00:00");
        // The guardian never writes counts.
        let guardian = Book::new(Process::Guardian, dir.path().to_path_buf(), &config, targets("info"));
        guardian.write(&stamp(13, 1), "g\n", false);
        guardian.write(&stamp(14, 0), "g2\n", false);
        assert_eq!(read(13), "after midnight\ng\n");
        assert_eq!(read(14), "g2\n");
    }

    #[test]
    fn a_full_day_keeps_warnings_and_says_so_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = LogConfig { max_mb_per_day: 1, ..LogConfig::default() };
        let book = Book::new(Process::Guardian, dir.path().to_path_buf(), &config, targets("info"));
        let big = "x".repeat(1 << 20) + "\n";
        book.write(&stamp(12, 9), &big, false);
        book.write(&stamp(12, 9), "debug 1\n", false);
        book.write(&stamp(12, 9), "debug 2\n", false);
        book.write(&stamp(12, 9), "a warning\n", true);
        let text = std::fs::read_to_string(dir.path().join("2026-10-12.log")).unwrap_or_default();
        let tail: Vec<&str> = text.lines().skip(1).collect();
        assert_eq!(tail.len(), 2, "{tail:?}");
        assert!(tail[0].contains(&format!("WARN  logbook: {CAP_NOTICE} (1 MB)")), "{}", tail[0]);
        assert_eq!(tail[1], "a warning");
        assert!(summary::summarize(None, &text).capped);
    }

    #[test]
    fn levels_map_to_directives_the_filter_understands() {
        let debug = targets(crate::config::LogLevel::Debug.directives());
        assert!(debug.would_enable("stage_manager::platform::controller", &Level::DEBUG));
        assert!(!debug.would_enable("stage_manager::platform::observe", &Level::TRACE));
        assert!(!debug.would_enable("other_crate", &Level::DEBUG));
        let info = targets(crate::config::LogLevel::Info.directives());
        assert!(!info.would_enable("stage_manager::x", &Level::DEBUG));
        let ring = targets(RING_DIRECTIVES);
        assert!(ring.would_enable("stage_manager::platform::observe", &Level::TRACE));
    }
}
