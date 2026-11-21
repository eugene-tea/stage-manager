//! `probe-spaces dock-notices [--rounds N]` (probe 1, the review's R4): does the Dock announce
//! Mission Control, App Exposé and Show Desktop, and does it come before the window list shows
//! them?
//!
//! An Accessibility observer on the Dock's application element registers the names public
//! window managers register there (yabai's `AXExposeShowAllWindows`,
//! `AXExposeShowFrontWindows`, `AXExposeShowDesktop`, `AXExposeExit`), each notice stamped in
//! its callback on the main run loop, which turns all along (keys go out from a thread of
//! their own). A second thread reads the on-screen window list every 2 ms for what
//! [`crate::engine::overview`] looks for today (WindowManager's window over the display at
//! layer 19 for Mission Control and App Exposé, 18 for Show Desktop; the Dock's cover at layer
//! 20) and stamps each change.
//!
//! Triggers, each closed by the same key and, in other rounds, by Escape, `--rounds` times
//! (default 3): Mission Control's shortcut (Keyboard Shortcuts, Control-↑ by default), the
//! keyboard's Mission Control key (key code 160), App Exposé's shortcut (with whichever app is
//! frontmost: the script brings a fixture in front) and Show Desktop's shortcut. A shortcut
//! turned off is skipped and said. One that does not open in 3 s is noted and not closed. A
//! close that does not take in 4 s is noted and the trigger posted again to close it.
//!
//! Hot corners are not read (the Dock's settings are read through Accessibility only) and the
//! pointer is never moved: a hot corner can only fire if the pointer already rests there.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use objc2_application_services::{AXObserver, AXUIElement};
use objc2_core_foundation::CFString;

use super::{Args, Ctx, cell, main_frame, median, ms, off_main, post_plain_stamped, post_stamped, pump, pump_until, sight_of, table};
use crate::engine::mission_control::MISSION_CONTROL_KEY;
use crate::engine::overview::Overview;
use crate::platform::ax::{AxElement, AxObserver};
use crate::platform::space_keys::{ESCAPE_KEY, Expose, SpaceKey};
use crate::platform::{dock, permissions, windows};

/// The Dock's names, as public window managers register them on its application element.
const NAMES: [&str; 4] = ["AXExposeShowAllWindows", "AXExposeShowFrontWindows", "AXExposeShowDesktop", "AXExposeExit"];
const EXIT: &str = "AXExposeExit";

/// The window list's read interval.
const EVERY: Duration = Duration::from_millis(2);
/// How long a trigger may take to show, a close to take, and the settles around them.
const OPEN_LIMIT: Duration = Duration::from_secs(3);
const CLOSE_LIMIT: Duration = Duration::from_secs(4);
const SHOWN_HOLD: Duration = Duration::from_millis(1200);
const LATE_TAIL: Duration = Duration::from_millis(1000);
const REST: Duration = Duration::from_millis(800);

/// What one read of the list shows (bits).
const WM_MC: u8 = 1;
const WM_SD: u8 = 2;
const DOCK: u8 = 4;

/// Every notice heard: when, which.
static HEARD: Mutex<Vec<(Instant, String)>> = Mutex::new(Vec::new());

unsafe extern "C-unwind" fn on_notice(_: NonNull<AXObserver>, _: NonNull<AXUIElement>, name: NonNull<CFString>, _: *mut c_void) {
    let at = Instant::now();
    // SAFETY: Accessibility hands a live string for the length of the callback.
    let name = unsafe { name.as_ref() }.to_string();
    if let Ok(mut v) = HEARD.lock() {
        v.push((at, name));
    }
}

fn heard() -> Vec<(Instant, String)> {
    HEARD.lock().map(|v| v.clone()).unwrap_or_default()
}

/// How a trigger opens.
#[derive(Clone, Copy)]
enum Open {
    Shortcut(Expose),
    McKey,
}

/// How a round closes.
#[derive(Clone, Copy, PartialEq)]
enum Close {
    Same,
    Escape,
}

struct Trigger {
    label: &'static str,
    open: Open,
    /// The bit of the list that says it shows.
    shows: u8,
}

/// One round's stamps.
struct Round {
    trigger: &'static str,
    close: Close,
    posted: Instant,
    closed: Option<Instant>,
    end: Instant,
    note: String,
}

fn post_open(open: Open) -> Result<Instant, String> {
    match open {
        Open::Shortcut(e) => post_stamped(&SpaceKey::read_expose(e)),
        Open::McKey => post_plain_stamped(MISSION_CONTROL_KEY),
    }
}

/// Reads the list every [`EVERY`] until `stop`: each change of what it shows, and how many reads.
fn sample(stop: &AtomicBool, latest: &AtomicU8, display: crate::engine::geometry::Rect) -> (Vec<(Instant, u8)>, u64, f64) {
    let (mut changes, mut reads, t0) = (Vec::new(), 0u64, Instant::now());
    let mut last: Option<u8> = None;
    while !stop.load(Ordering::Relaxed) {
        let next = Instant::now() + EVERY;
        let list = windows::on_screen();
        let at = Instant::now();
        let sight = sight_of(&list, display);
        let bits = match sight.overview {
            Some(Overview::MissionControl) => WM_MC,
            Some(Overview::ShowDesktop) => WM_SD,
            None => 0,
        } | if sight.dock_cover { DOCK } else { 0 };
        reads += 1;
        if last != Some(bits) {
            changes.push((at, bits));
            last = Some(bits);
        }
        latest.store(bits, Ordering::Relaxed);
        if let Some(d) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }
    let every = if reads > 0 { t0.elapsed().as_secs_f64() * 1e3 / reads as f64 } else { 0.0 };
    (changes, reads, every)
}

pub(super) fn run(_ctx: &Ctx, args: &Args) -> Result<(), String> {
    if !permissions::accessibility_trusted() {
        return Err("no Accessibility for this process (launch it through `open` from the dev app)".into());
    }
    let display = main_frame().ok_or("no main display")?;
    let dock_pid = dock::dock_pid().ok_or("the Dock is not running")?;
    let observer = AxObserver::new(dock_pid, on_notice).map_err(|e| format!("no observer on the Dock: {e}"))?;
    let app = AxElement::application(dock_pid);
    let registered: Vec<(&str, Result<(), String>)> = NAMES.iter().map(|n| (*n, observer.add(&app, n, 0).map_err(|e| e.to_string()))).collect();
    println!("== dock-notices: the Dock's AXExpose notices against the window list (read every {} ms) ==", EVERY.as_millis());
    for (n, r) in &registered {
        println!("  registered {n} on the Dock (pid {dock_pid}): {}", r.as_ref().map_or_else(|e| format!("refused, {e}"), |_| "ok".into()));
    }
    println!("  hot corners: not read (the Dock's settings are read through Accessibility only); the pointer is never moved");
    let triggers = [
        Trigger { label: "Mission Control shortcut", open: Open::Shortcut(Expose::MissionControl), shows: WM_MC },
        Trigger { label: "Mission Control key (code 160)", open: Open::McKey, shows: WM_MC },
        Trigger { label: "App Exposé shortcut", open: Open::Shortcut(Expose::AppWindows), shows: WM_MC },
        Trigger { label: "Show Desktop shortcut", open: Open::Shortcut(Expose::ShowDesktop), shows: WM_SD },
    ];
    for t in &triggers {
        if let Open::Shortcut(e) = t.open {
            let k = SpaceKey::read_expose(e);
            println!("  {}: {} ({}{})", t.label, k.describe(), if k.enabled { "on" } else { "OFF, skipped" }, if k.from_settings { ", from Keyboard Shortcuts" } else { ", the macOS default" });
        }
    }
    let stop = AtomicBool::new(false);
    let latest = AtomicU8::new(0);
    let (rounds, (changes, reads, every)) = std::thread::scope(|s| {
        let sampler = s.spawn(|| sample(&stop, &latest, display));
        pump(Duration::from_millis(300));
        let mut rounds = Vec::new();
        for t in &triggers {
            if let Open::Shortcut(e) = t.open
                && !SpaceKey::read_expose(e).enabled
            {
                continue;
            }
            for close in [Close::Same, Close::Escape] {
                for _ in 0..args.rounds {
                    rounds.push(round(t, close, &latest));
                }
            }
        }
        // Whatever happened, nothing of macOS's is left showing.
        if latest.load(Ordering::Relaxed) & (WM_MC | WM_SD) != 0 {
            let _ = off_main(|| post_plain_stamped(ESCAPE_KEY));
            pump_until(CLOSE_LIMIT, || latest.load(Ordering::Relaxed) & (WM_MC | WM_SD) == 0);
        }
        stop.store(true, Ordering::Relaxed);
        let out = sampler.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
        (rounds, out)
    });
    drop(observer);
    println!("  the list was read {reads} times, every {every:.1} ms on average");
    report(&triggers, &rounds, &changes, &heard());
    Ok(())
}

/// One round: the trigger posted, held while it shows, closed, the tail heard (module docs).
fn round(t: &Trigger, close: Close, latest: &AtomicU8) -> Round {
    let shows = |bits: u8| bits & (WM_MC | WM_SD) != 0;
    let mut note = String::new();
    let heard_before = heard().len();
    let posted = match off_main(|| post_open(t.open)) {
        Ok(at) => at,
        Err(e) => return Round { trigger: t.label, close, posted: Instant::now(), closed: None, end: Instant::now(), note: e },
    };
    let opened = pump_until(OPEN_LIMIT, || latest.load(Ordering::Relaxed) & t.shows != 0);
    let notice = heard().len() > heard_before;
    if !opened && !notice {
        pump(REST);
        return Round { trigger: t.label, close, posted, closed: None, end: Instant::now(), note: "did not open in 3 s".into() };
    }
    if !opened {
        note.push_str("a notice but nothing in the list; ");
    }
    pump(SHOWN_HOLD);
    let posted_close = off_main(|| match close {
        Close::Same => post_open(t.open),
        Close::Escape => post_plain_stamped(ESCAPE_KEY),
    });
    let closed = posted_close.as_ref().ok().copied();
    if let Err(e) = posted_close {
        note.push_str(&e);
    }
    if !pump_until(CLOSE_LIMIT, || !shows(latest.load(Ordering::Relaxed))) {
        note.push_str(if close == Close::Escape { "Escape did not close it, closed by the trigger; " } else { "did not close in 4 s, posted again; " });
        let _ = off_main(|| post_open(t.open));
        pump_until(CLOSE_LIMIT, || !shows(latest.load(Ordering::Relaxed)));
    }
    pump(LATE_TAIL);
    let end = Instant::now();
    pump(REST);
    Round { trigger: t.label, close, posted, closed, end, note: note.trim_end_matches("; ").to_string() }
}

/// What one round measured (ms from its post or its close).
#[derive(Default)]
struct Measured {
    open_notice: Option<(String, f64)>,
    listed: Option<f64>,
    exit: Option<f64>,
    wm_gone: Option<f64>,
    dock_gone: Option<f64>,
    covered: bool,
    others: Vec<String>,
}

fn measure(r: &Round, changes: &[(Instant, u8)], notices: &[(Instant, String)]) -> Measured {
    let mut m = Measured::default();
    let open_end = r.closed.unwrap_or(r.end);
    let in_open = |at: Instant| at >= r.posted && at < open_end;
    m.open_notice = notices.iter().find(|(at, n)| in_open(*at) && n != EXIT).map(|(at, n)| (short(n), ms(r.posted, *at)));
    m.listed = changes.iter().find(|(at, b)| in_open(*at) && b & (WM_MC | WM_SD) != 0).map(|(at, _)| ms(r.posted, *at));
    m.covered = changes.iter().any(|(at, b)| *at >= r.posted && *at <= r.end && b & DOCK != 0);
    for (at, n) in notices.iter().filter(|(at, _)| *at >= r.posted && *at <= r.end) {
        let (base, tag) = match r.closed {
            Some(c) if *at >= c => (c, "c+"),
            _ => (r.posted, "+"),
        };
        m.others.push(format!("{}@{tag}{:.0}", short(n), ms(base, *at)));
    }
    if let Some(c) = r.closed {
        let after = |at: Instant| at >= c && at <= r.end;
        m.exit = notices.iter().find(|(at, n)| after(*at) && n == EXIT).map(|(at, _)| ms(c, *at));
        m.wm_gone = changes.iter().find(|(at, b)| after(*at) && b & (WM_MC | WM_SD) == 0).map(|(at, _)| ms(c, *at));
        if m.covered {
            m.dock_gone = changes.iter().find(|(at, b)| after(*at) && b & DOCK == 0 && b & (WM_MC | WM_SD) == 0).map(|(at, _)| ms(c, *at));
        }
    }
    m
}

fn short(name: &str) -> String {
    name.trim_start_matches("AXExpose").to_string()
}

fn report(triggers: &[Trigger], rounds: &[Round], changes: &[(Instant, u8)], notices: &[(Instant, String)]) {
    let measured: Vec<Measured> = rounds.iter().map(|r| measure(r, changes, notices)).collect();
    let rows: Vec<Vec<String>> = rounds
        .iter()
        .zip(&measured)
        .map(|(r, m)| {
            vec![
                r.trigger.to_string(),
                if r.close == Close::Same { "same key".into() } else { "Escape".into() },
                m.open_notice.as_ref().map_or("-".into(), |n| n.0.clone()),
                cell(m.open_notice.as_ref().map(|n| n.1)),
                cell(m.listed),
                cell(m.exit),
                cell(m.wm_gone),
                if m.covered { cell(m.dock_gone) } else { "no cover".into() },
                m.others.join(" "),
                r.note.clone(),
            ]
        })
        .collect();
    println!("\n  ms from the post (open) and from the closing key (close); notices: +ms from the post, c+ms from the close");
    table(
        &["trigger", "close", "notice at open", "post→notice", "post→list", "close→Exit", "close→WM gone", "close→cover gone", "notices", "note"],
        &rows,
    );
    println!();
    let mut opens_event = Vec::new();
    let mut closes_event = Vec::new();
    for t in triggers {
        let mine: Vec<&Measured> = rounds.iter().zip(&measured).filter(|(r, _)| r.trigger == t.label).map(|(_, m)| m).collect();
        let opened: Vec<&&Measured> = mine.iter().filter(|m| m.listed.is_some() || m.open_notice.is_some()).collect();
        if mine.is_empty() {
            continue;
        }
        if opened.is_empty() {
            println!("VERDICT {}: never opened in {} rounds: nothing to tell", t.label, mine.len());
            continue;
        }
        let with_notice: Vec<&&&Measured> = opened.iter().filter(|m| m.open_notice.is_some()).collect();
        let leads: Vec<f64> = with_notice.iter().filter_map(|m| Some(m.listed? - m.open_notice.as_ref()?.1)).collect();
        let names: Vec<String> = {
            let mut n: Vec<String> = with_notice.iter().filter_map(|m| m.open_notice.as_ref().map(|n| n.0.clone())).collect();
            n.sort();
            n.dedup();
            n
        };
        let notice_ms: Vec<f64> = with_notice.iter().filter_map(|m| m.open_notice.as_ref().map(|n| n.1)).collect();
        let list_ms: Vec<f64> = opened.iter().filter_map(|m| m.listed).collect();
        let open_line = if with_notice.len() == opened.len() {
            opens_event.push(true);
            format!(
                "open IS an event: {} in {}/{} rounds, median {} ms after the post; the list shows it at a median {} ms; the notice {}",
                names.join("/"),
                with_notice.len(),
                opened.len(),
                cell(median(&notice_ms)),
                cell(median(&list_ms)),
                lead_words(&leads, "the list")
            )
        } else if with_notice.is_empty() {
            opens_event.push(false);
            format!("open is NOT an event: no notice in {} rounds that opened; the list shows it at a median {} ms", opened.len(), cell(median(&list_ms)))
        } else {
            opens_event.push(false);
            format!("open is NOT a reliable event: a notice in {}/{} rounds only ({})", with_notice.len(), opened.len(), names.join("/"))
        };
        println!("VERDICT {}: {open_line}", t.label);
        let closed: Vec<&&Measured> = opened.iter().filter(|m| m.wm_gone.is_some()).copied().collect();
        let exits: Vec<f64> = closed.iter().filter_map(|m| m.exit).collect();
        let gone: Vec<f64> = closed.iter().filter_map(|m| m.wm_gone).collect();
        let covers: Vec<f64> = closed.iter().filter_map(|m| m.dock_gone).collect();
        let exit_vs_cover: Vec<f64> = closed.iter().filter_map(|m| Some(m.dock_gone? - m.exit?)).collect();
        let exit_vs_wm: Vec<f64> = closed.iter().filter_map(|m| Some(m.wm_gone? - m.exit?)).collect();
        let close_line = if !closed.is_empty() && exits.len() == closed.len() {
            closes_event.push(true);
            format!(
                "close IS an event: AXExposeExit in {}/{} closes, median {} ms after the key; WindowManager's window gone at {} ms, the Dock's cover at {} ms; Exit {} WindowManager's going, {} the cover's",
                exits.len(),
                closed.len(),
                cell(median(&exits)),
                cell(median(&gone)),
                cell(median(&covers)),
                lead_words(&exit_vs_wm, "it"),
                lead_words(&exit_vs_cover, "it")
            )
        } else {
            closes_event.push(false);
            format!(
                "close is NOT {}an event: AXExposeExit in {}/{} closes; WindowManager's window gone at a median {} ms, the Dock's cover at {} ms",
                if exits.is_empty() { "" } else { "a reliable " },
                exits.len(),
                closed.len(),
                cell(median(&gone)),
                cell(median(&covers))
            )
        };
        println!("VERDICT {}: {close_line}", t.label);
    }
    let all = |v: &[bool]| !v.is_empty() && v.iter().all(|b| *b);
    println!(
        "VERDICT overall: open {} an event for every trigger; close {} an event for every trigger (see the lines above for which comes first)",
        if all(&opens_event) { "can be" } else { "cannot be" },
        if all(&closes_event) { "can be" } else { "cannot be" }
    );
}

/// "comes N ms before X" / "after X" / "with X", from per-round differences (X minus the notice).
fn lead_words(diffs: &[f64], other: &str) -> String {
    let Some(m) = median(diffs) else { return format!("cannot be set against {other} (no pair)") };
    let before = diffs.iter().filter(|d| **d > 2.0).count();
    let after = diffs.iter().filter(|d| **d < -2.0).count();
    let with = diffs.len() - before - after;
    let order = if m > 2.0 {
        format!("comes a median {m:.0} ms before {other}")
    } else if m < -2.0 {
        format!("comes a median {:.0} ms after {other}", -m)
    } else {
        format!("comes with {other} (within 2 ms)")
    };
    format!("{order} ({before} before, {with} with, {after} after)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leads_say_which_comes_first() {
        assert!(lead_words(&[30.0, 40.0, 35.0], "the list").starts_with("comes a median 35 ms before the list (3 before"));
        assert!(lead_words(&[-10.0, -12.0], "it").starts_with("comes a median 12 ms after it") || lead_words(&[-10.0, -12.0], "it").starts_with("comes a median 10 ms after it"));
        assert!(lead_words(&[1.0, -1.0, 0.0], "x").starts_with("comes with x"));
        assert!(lead_words(&[], "x").contains("no pair"));
    }

    #[test]
    fn a_round_is_measured_from_its_post_and_its_close() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let r = Round { trigger: "t", close: Close::Escape, posted: at(0), closed: Some(at(2000)), end: at(4000), note: String::new() };
        let changes = vec![(at(0), 0), (at(45), WM_MC | DOCK), (at(2100), DOCK), (at(2600), 0)];
        let notices = vec![(at(20), "AXExposeShowAllWindows".to_string()), (at(2050), EXIT.to_string())];
        let m = measure(&r, &changes, &notices);
        assert_eq!(m.open_notice, Some(("ShowAllWindows".into(), 20.0)));
        assert_eq!((m.listed, m.exit, m.wm_gone, m.dock_gone), (Some(45.0), Some(50.0), Some(100.0), Some(600.0)));
        assert_eq!(m.others, vec!["ShowAllWindows@+20", "Exit@c+50"]);
    }
}
