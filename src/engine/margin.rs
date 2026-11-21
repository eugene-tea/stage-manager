//! How close each time value comes to deciding the other way (review 11-21, rule 5): every
//! decision made by a time window ("within X of Y") and every wait's end is noted here, by
//! the constant's name, so a log tells per Mac which values ever come near their limit and
//! which waits end by their cap (docs/DEVELOPMENT.md, "Logs").
//!
//! - [`note`]: a time-window decision, its elapsed time against its limit, and whether it was
//!   judged inside.
//! - [`wait`]: a wait's end, met (the state it waits for read back), refuted (an event said
//!   no) or ended by its cap.
//! - [`predated`]: a notice whose receipt stamp is older than the action it is compared with
//!   (judged as at the action: age zero).
//!
//! One process-wide table, behind a mutex, keyed by `&'static str`: a call allocates only
//! the first time a name is seen. A decision within 0.5 to 2 times its limit logs one debug
//! line, once per run of such decisions with one verdict (a predicate asked on every tick or
//! every move while its time runs out logs as it comes close and as it crosses, not at each
//! call); a wait ended by its cap logs one; [`take_period`] gives the lines of the period
//! since its last call (the hourly and quit counts write them), [`rows`] the whole table
//! (`ctl state`'s `margins`). Nothing here reads a clock.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;

/// How a wait ended ([`wait`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// What it waited for was read back.
    Met,
    /// An event said it will not come.
    Refuted,
    /// Its cap ran out first.
    Cap,
}

/// A decision this close to its limit (as a share of it), inside, is a close call.
pub const CLOSE_INSIDE: f64 = 0.5;
/// A decision up to this share of its limit, outside, is a near miss.
pub const JUST_OUTSIDE: f64 = 2.0;

/// One name's counts. The shares are `elapsed / limit`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Row {
    /// The limit last noted, in milliseconds (a configurable one may change).
    pub limit_ms: u64,
    /// Decisions noted.
    pub decisions: u64,
    /// The largest share judged inside.
    pub closest_inside: Option<f64>,
    /// The smallest share judged outside.
    pub closest_outside: Option<f64>,
    /// Inside within 0.5 to 1 of the limit.
    pub close_inside: u64,
    /// Outside within 1 to 2 of the limit.
    pub just_outside: u64,
    /// Waits ended.
    pub waits: u64,
    /// Waits ended by their cap.
    pub capped: u64,
    /// The longest wait, in milliseconds.
    pub longest_wait_ms: u64,
    /// Notices stamped before the action they were compared with.
    pub predated: u64,
}

impl Row {
    fn decide(&mut self, share: f64, inside: bool) {
        self.decisions += 1;
        if inside {
            self.closest_inside = Some(self.closest_inside.map_or(share, |c| c.max(share)));
            if (CLOSE_INSIDE..=1.0).contains(&share) {
                self.close_inside += 1;
            }
        } else {
            self.closest_outside = Some(self.closest_outside.map_or(share, |c| c.min(share)));
            if (1.0..JUST_OUTSIDE).contains(&share) {
                self.just_outside += 1;
            }
        }
    }

    fn ended(&mut self, elapsed: Duration, how: End) {
        self.waits += 1;
        self.longest_wait_ms = self.longest_wait_ms.max(ms(elapsed));
        if how == End::Cap {
            self.capped += 1;
        }
    }

    /// True when the row has something worth a summary line: a close call, a near miss, a
    /// wait ended by its cap or a notice older than its action.
    pub fn notable(&self) -> bool {
        self.close_inside + self.just_outside + self.capped + self.predated > 0
    }

    /// The row as a summary line's fields (`log summary` reads them back, [`parse_line`]).
    pub fn line(&self, name: &str) -> String {
        let share = |s: Option<f64>| s.map_or_else(|| "-".to_string(), |s| format!("{s:.2}"));
        format!(
            "name={name} limit_ms={} decisions={} closest_inside={} closest_outside={} close_inside={} just_outside={} waits={} capped={} longest_wait_ms={} predated={}",
            self.limit_ms,
            self.decisions,
            share(self.closest_inside),
            share(self.closest_outside),
            self.close_inside,
            self.just_outside,
            self.waits,
            self.capped,
            self.longest_wait_ms,
            self.predated
        )
    }

    /// Adds `other` (a later period of the same name) to this one.
    pub fn add(&mut self, other: &Row) {
        self.limit_ms = other.limit_ms;
        self.decisions += other.decisions;
        self.closest_inside = max_opt(self.closest_inside, other.closest_inside);
        self.closest_outside = min_opt(self.closest_outside, other.closest_outside);
        self.close_inside += other.close_inside;
        self.just_outside += other.just_outside;
        self.waits += other.waits;
        self.capped += other.capped;
        self.longest_wait_ms = self.longest_wait_ms.max(other.longest_wait_ms);
        self.predated += other.predated;
    }
}

fn max_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn min_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Parses a summary line's fields ([`Row::line`]) back: the name and its row.
pub fn parse_line(text: &str) -> Option<(String, Row)> {
    let mut name = None;
    let mut row = Row::default();
    for field in text.split_whitespace() {
        let Some((key, value)) = field.split_once('=') else { continue };
        let n = || value.parse::<u64>().ok();
        let share = || value.parse::<f64>().ok();
        match key {
            "name" => name = Some(value.to_string()),
            "limit_ms" => row.limit_ms = n()?,
            "decisions" => row.decisions = n()?,
            "closest_inside" => row.closest_inside = share(),
            "closest_outside" => row.closest_outside = share(),
            "close_inside" => row.close_inside = n()?,
            "just_outside" => row.just_outside = n()?,
            "waits" => row.waits = n()?,
            "capped" => row.capped = n()?,
            "longest_wait_ms" => row.longest_wait_ms = n()?,
            "predated" => row.predated = n()?,
            _ => {}
        }
    }
    Some((name?, row))
}

/// The whole run's table and the period's (since the last [`take_period`]).
#[derive(Default)]
struct Table {
    total: BTreeMap<&'static str, Row>,
    period: BTreeMap<&'static str, Row>,
    /// Per name, the verdict of the last decision noted within 0.5 to 2 of its limit, while
    /// the decisions since stayed there: a repeat logs no line.
    /// `None` once a decision fell outside that band (kept, not removed: no allocation).
    band: BTreeMap<&'static str, Option<bool>>,
}

static TABLE: Mutex<Table> = Mutex::new(Table { total: BTreeMap::new(), period: BTreeMap::new(), band: BTreeMap::new() });

fn table() -> MutexGuard<'static, Table> {
    // A panic while holding it leaves counts only: they stay usable.
    TABLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// `elapsed` as a share of `limit` (a zero limit: 0 when nothing elapsed, else far outside).
pub fn share(elapsed: Duration, limit: Duration) -> f64 {
    if limit.is_zero() {
        return if elapsed.is_zero() { 0.0 } else { f64::INFINITY };
    }
    elapsed.as_secs_f64() / limit.as_secs_f64()
}

/// Applies `f` to `name`'s row in both tables.
fn with_row(name: &'static str, f: impl Fn(&mut Row)) {
    let mut t = table();
    f(t.total.entry(name).or_default());
    f(t.period.entry(name).or_default());
}

/// True when a decision of `name` with `share` and `inside` is worth a debug line: within
/// 0.5 to 2 of its limit, and not the same verdict as the decision before it there.
fn worth_a_line(name: &'static str, share: f64, inside: bool) -> bool {
    let mut t = table();
    let last = t.band.entry(name).or_default();
    if !(CLOSE_INSIDE..JUST_OUTSIDE).contains(&share) {
        *last = None;
        return false;
    }
    last.replace(inside) != Some(inside)
}

/// A decision by time window `name`: `elapsed` against `limit`, judged `inside` (the caller's
/// own verdict: `<` or `<=`, and any other condition, are its). Returns `inside`, so a check
/// can be noted where it is made. Within 0.5 to 2 of the limit: one debug line.
pub fn note(name: &'static str, elapsed: Duration, limit: Duration, inside: bool) -> bool {
    #[cfg(test)]
    test::seen(test::Seen { name, elapsed, limit, inside: Some(inside), ended: None });
    let share = share(elapsed, limit);
    let limit_ms = ms(limit);
    with_row(name, |r| {
        r.limit_ms = limit_ms;
        r.decide(share, inside);
    });
    if worth_a_line(name, share, inside) {
        tracing::debug!("margin: name={name} elapsed_ms={} limit_ms={limit_ms} inside={inside}", ms(elapsed));
    }
    inside
}

/// [`note`] for the common "`elapsed < limit`".
pub fn within(name: &'static str, elapsed: Duration, limit: Duration) -> bool {
    note(name, elapsed, limit, elapsed < limit)
}

/// A wait `name` ended after `elapsed`, its cap `cap`, `how`. One ended by its cap logs one
/// debug line.
pub fn wait(name: &'static str, elapsed: Duration, cap: Duration, how: End) {
    #[cfg(test)]
    test::seen(test::Seen { name, elapsed, limit: cap, inside: None, ended: Some(how) });
    let cap_ms = ms(cap);
    with_row(name, |r| {
        r.limit_ms = cap_ms;
        r.ended(elapsed, how);
    });
    if how == End::Cap {
        tracing::debug!("margin: name={name} elapsed_ms={} limit_ms={cap_ms} inside=false ended=cap", ms(elapsed));
    }
}

/// A notice stamped before the action `name` compares it with: counted, and judged as at the
/// action (age zero) by the caller, as before stamps.
pub fn predated(name: &'static str) {
    with_row(name, |r| r.predated += 1);
}

/// The notice's age at `at` since the action at `since`, for window `name`: zero, and
/// counted [`predated`], when the notice was stamped before the action.
pub fn age(name: &'static str, at: std::time::Instant, since: std::time::Instant) -> Duration {
    if at < since {
        predated(name);
    }
    at.saturating_duration_since(since)
}

/// How long before a notice the last press came, for window `name`: the press `since` ago
/// as of now, the notice received `ago` before now. Zero, and counted [`predated`], when the
/// press came after the notice (a press between a notice and its handling counts as at it).
pub fn before(name: &'static str, since: Duration, ago: Duration) -> Duration {
    if since < ago {
        predated(name);
    }
    since.saturating_sub(ago)
}

/// Test only: what this thread noted, kept apart from the process's table (tests run side by
/// side): a scenario reads the time decisions its own run made, each with its verdict.
#[cfg(test)]
pub mod test {
    use super::End;
    use std::cell::RefCell;
    use std::time::Duration;

    /// One decision or one wait's end noted on this thread.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Seen {
        /// The constant's name.
        pub name: &'static str,
        /// What had passed.
        pub elapsed: Duration,
        /// Its limit or cap.
        pub limit: Duration,
        /// A decision's verdict (`None`: a wait's end).
        pub inside: Option<bool>,
        /// How a wait ended (`None`: a decision).
        pub ended: Option<End>,
    }

    thread_local! {
        static SEEN: RefCell<Option<Vec<Seen>>> = const { RefCell::new(None) };
    }

    /// Starts keeping what this thread notes (dropping what was kept).
    pub fn start() {
        SEEN.with(|s| *s.borrow_mut() = Some(Vec::new()));
    }

    /// What was noted since [`start`], in order; nothing is kept after.
    pub fn take() -> Vec<Seen> {
        SEEN.with(|s| s.borrow_mut().take().unwrap_or_default())
    }

    /// What was noted since [`start`], in order, still kept.
    pub fn so_far() -> Vec<Seen> {
        SEEN.with(|s| s.borrow().clone().unwrap_or_default())
    }

    pub(super) fn seen(seen: Seen) {
        SEEN.with(|s| {
            if let Some(kept) = s.borrow_mut().as_mut() {
                kept.push(seen);
            }
        });
    }
}

/// The whole run's table, by name.
pub fn rows() -> Vec<(&'static str, Row)> {
    table().total.iter().map(|(n, r)| (*n, *r)).collect()
}

/// `value` (a JSON object: `ctl state`) with the run's table under `margins`, by name.
pub fn with_table(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(o) = value.as_object_mut() {
        let table: serde_json::Map<String, serde_json::Value> =
            rows().into_iter().filter_map(|(n, r)| serde_json::to_value(r).ok().map(|v| (n.to_string(), v))).collect();
        o.insert("margins".into(), serde_json::Value::Object(table));
    }
    value
}

/// The summary lines of the period since the last call ([`Row::line`]), for the names with
/// something [`Row::notable`]; the period starts again. Empty when nothing came close.
/// Never waits for the table: a panic hook may write the counts while this thread holds it.
pub fn take_period() -> Vec<String> {
    let mut t = match TABLE.try_lock() {
        Ok(t) => t,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return Vec::new(),
    };
    let period = std::mem::take(&mut t.period);
    drop(t);
    period.iter().filter(|(_, r)| r.notable()).map(|(n, r)| r.line(n)).collect()
}

/// The text a summary line carries before its fields (`log summary` looks for it).
pub const SUMMARY_PREFIX: &str = "margin summary:";

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_row_keeps_the_closest_calls_on_either_side() {
        let mut r = Row::default();
        r.decide(share(ms(100), ms(700)), true);
        r.decide(share(ms(650), ms(700)), true);
        r.decide(share(ms(900), ms(700)), false);
        r.decide(share(ms(3000), ms(700)), false);
        assert_eq!(r.decisions, 4);
        assert_eq!((r.close_inside, r.just_outside), (1, 1));
        assert!((r.closest_inside.unwrap_or(0.0) - 650.0 / 700.0).abs() < 1e-9);
        assert!((r.closest_outside.unwrap_or(0.0) - 900.0 / 700.0).abs() < 1e-9);
        assert!(r.notable());
        assert!(!Row { decisions: 3, closest_inside: Some(0.1), ..Row::default() }.notable(), "far from the limit: nothing to say");
    }

    #[test]
    fn waits_count_their_caps_and_the_longest() {
        let mut r = Row::default();
        r.ended(ms(40), End::Met);
        r.ended(ms(400), End::Cap);
        r.ended(ms(10), End::Refuted);
        assert_eq!((r.waits, r.capped, r.longest_wait_ms), (3, 1, 400));
    }

    #[test]
    fn a_line_reads_back_as_written() {
        let r = Row {
            limit_ms: 700,
            decisions: 9,
            closest_inside: Some(0.93),
            closest_outside: None,
            close_inside: 2,
            just_outside: 0,
            waits: 0,
            capped: 0,
            longest_wait_ms: 0,
            predated: 1,
        };
        let (name, back) = parse_line(&r.line("ECHO_WINDOW")).expect("parses");
        assert_eq!(name, "ECHO_WINDOW");
        assert_eq!(back, r);
    }

    #[test]
    fn periods_add_up() {
        let a = Row { limit_ms: 700, decisions: 2, closest_inside: Some(0.6), close_inside: 1, ..Row::default() };
        let b = Row { limit_ms: 700, decisions: 3, closest_inside: Some(0.9), closest_outside: Some(1.2), just_outside: 1, ..Row::default() };
        let mut sum = a;
        sum.add(&b);
        assert_eq!((sum.decisions, sum.close_inside, sum.just_outside), (5, 1, 1));
        assert_eq!((sum.closest_inside, sum.closest_outside), (Some(0.9), Some(1.2)));
    }

    #[test]
    fn a_run_of_close_calls_with_one_verdict_logs_once() {
        const NAME: &str = "TEST_MARGIN_BAND";
        assert!(worth_a_line(NAME, 0.6, true), "the first close call");
        assert!(!worth_a_line(NAME, 0.8, true), "a poll coming closer");
        assert!(worth_a_line(NAME, 1.1, false), "crossed");
        assert!(!worth_a_line(NAME, 1.5, false));
        assert!(!worth_a_line(NAME, 3.0, false), "far: no line, and the run ends");
        assert!(worth_a_line(NAME, 1.5, false), "a new run");
    }

    #[test]
    fn the_table_counts_by_name_and_a_period_starts_again() {
        // A name of this test's own: the table is the process's.
        const NAME: &str = "TEST_MARGIN_TABLE";
        assert!(note(NAME, ms(600), ms(700), true));
        assert!(!within(NAME, ms(800), ms(700)));
        wait(NAME, ms(150), ms(150), End::Cap);
        let at = std::time::Instant::now();
        assert_eq!(age(NAME, at, at + ms(5)), Duration::ZERO, "stamped before the action: age zero");
        let row = rows().into_iter().find(|(n, _)| *n == NAME).map(|(_, r)| r).expect("a row");
        assert_eq!((row.decisions, row.close_inside, row.just_outside, row.capped, row.predated), (2, 1, 1, 1, 1));
        let lines = take_period();
        assert!(lines.iter().any(|l| l.starts_with(&format!("name={NAME} "))));
        assert!(!take_period().iter().any(|l| l.starts_with(&format!("name={NAME} "))), "the period starts again");
        assert!(rows().iter().any(|(n, _)| *n == NAME), "the run's table keeps it");
    }
}
