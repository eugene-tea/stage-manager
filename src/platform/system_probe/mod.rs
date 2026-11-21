//! `stage-manager probe-spaces dock-notices|space-flip|names|indicator|cgs …`: five live
//! probes the rework waits on (rounds r0-probes and r2-probes2, 11-21; the review's R3 and R4).
//! Each answers one question the window server or the Dock can answer and nothing in the repo
//! has measured:
//!
//! - [`dock_notices`]: does the Dock announce Mission Control, App Exposé and Show Desktop
//!   (`AXExpose…` notices), and how soon against the window list?
//! - [`space_flip`]: when does the window server's "Current Space" flip during a slide,
//!   against `activeSpaceDidChangeNotification` and the window list?
//! - [`names`]: what the window server calls the screens and the windows' Desktops, and what
//!   one `SLSCopyManagedDisplaySpaces` costs (read only).
//! - [`indicator`]: which capture lights the purple screen-recording indicator (may raise
//!   macOS's screen-capture alert: run only with the owner at the Mac).
//! - [`cgs`]: does the window server tell this process of Mission Control, a Desktop change
//!   or a window's move between Spaces (`SLSRegisterConnectionNotifyProc`,
//!   `SLSRegisterNotifyProc`, every type 0..2000)?
//!
//! They are reached through `scripts/probe_system.py` (`scripts/test-stages.sh probe-system`),
//! which opens the fixture windows, launches this binary from the dev app through
//! LaunchServices (`open -n -W … --stdout FILE --args probe-spaces …`) so it has the app's
//! Accessibility and Screen Recording, streams what it prints, and quits the fixtures after.
//! Run from a terminal, a probe has the terminal's approvals instead.
//!
//! Each prints plain tables and `VERDICT` lines a reader can act on (never JSON), to the
//! terminal only: nothing is written to the repo. The last line is `RESULT measured` (exit
//! 0) or `RESULT could not measure: …` (exit 1); a usage error exits 2. Only fixture windows
//! (`dev.eugene.stagemanager.fixture…`) are moved, resized or hidden, and each probe puts them
//! back. Private window-server calls go through [`crate::platform::spaces::ffi`] (signatures from public
//! sources, named there) and, for the two capture calls, the signatures
//! [`crate::platform::capture_fast`] names.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask, NSRunningApplication};
use objc2_core_graphics::CGMainDisplayID;
use objc2_foundation::{MainThreadMarker, NSDate, NSDefaultRunLoopMode};

use super::{fixture_pids, fixture_windows, pump};
use crate::engine::geometry::Rect;
use crate::engine::overview::{self, Sight, Sighting};
use crate::platform::park_probe::FIXTURE_PREFIX;
use crate::platform::space_keys::{self, SpaceKey};
use crate::platform::spaces::ffi::{ScreenDesks, Sky};
use crate::platform::{desktop, desktop_counts, windows};

mod cgs;
mod dock_notices;
mod indicator;
mod names;
mod space_flip;

/// What every probe gets: the main thread's marker and SkyLight's calls.
pub(super) struct Ctx {
    pub(super) mtm: MainThreadMarker,
    pub(super) sky: Sky,
}

/// The arguments the five take (each reads the ones it needs).
#[derive(Debug, Default)]
pub(super) struct Args {
    /// Fixture bundle ids, in order.
    pub(super) bundles: Vec<String>,
    /// Rounds per case (`--rounds N`, default 3).
    pub(super) rounds: usize,
    /// A window set to "All Desktops" by hand (`--window ID`, `names`).
    pub(super) window: Option<u32>,
    /// A fixture whose first window is on every Desktop (`--sticky BUNDLE`, `names`).
    pub(super) sticky: Option<String>,
    /// How many calls the cost is measured over (`--calls N`, `names`, default 2000).
    pub(super) calls: usize,
    /// Seconds each capture kind runs and rests (`--run S`, `--rest S`, `indicator`).
    pub(super) run_s: f64,
    pub(super) rest_s: f64,
    /// `indicator`: the by-eye pass for every kind, not only those the list cannot tell.
    pub(super) by_eye: bool,
}

impl Args {
    fn parse(words: &[String]) -> Result<Self, String> {
        let mut a = Args { rounds: 3, calls: 2000, run_s: 5.0, rest_s: 5.0, ..Default::default() };
        let mut it = words.iter();
        let num = |v: Option<&String>, what: &str| v.and_then(|s| s.parse::<f64>().ok()).ok_or(format!("{what} takes a number"));
        while let Some(w) = it.next() {
            match w.as_str() {
                "--rounds" => a.rounds = num(it.next(), "--rounds")?.clamp(1.0, 50.0) as usize,
                "--window" => a.window = Some(num(it.next(), "--window")? as u32),
                "--sticky" => a.sticky = Some(it.next().ok_or("--sticky takes a fixture bundle id")?.clone()),
                "--calls" => a.calls = num(it.next(), "--calls")?.clamp(10.0, 100_000.0) as usize,
                "--run" => a.run_s = num(it.next(), "--run")?.clamp(1.0, 60.0),
                "--rest" => a.rest_s = num(it.next(), "--rest")?.clamp(1.0, 60.0),
                "--by-eye" => a.by_eye = true,
                b => a.bundles.push(b.to_string()),
            }
        }
        if let Some(b) = a.bundles.iter().chain(a.sticky.iter()).find(|b| !b.starts_with(FIXTURE_PREFIX)) {
            return Err(format!("{b}: the probes act on fixture apps only ({FIXTURE_PREFIX}…)"));
        }
        Ok(a)
    }

    /// The `i`th bundle id, or a usage error naming `what`.
    pub(super) fn bundle(&self, i: usize, what: &str) -> Result<&str, String> {
        self.bundles.get(i).map(String::as_str).ok_or(format!("usage: {what}"))
    }
}

/// Runs `cmd` when it is one of the five (module docs); `None` for any other subcommand.
pub(super) fn run(cmd: &str, rest: &[String]) -> Option<ExitCode> {
    type Probe = fn(&Ctx, &Args) -> Result<(), String>;
    let probe: Probe = match cmd {
        "dock-notices" => dock_notices::run,
        "space-flip" => space_flip::run,
        "names" => names::run,
        "indicator" => indicator::run,
        "cgs" => cgs::run,
        _ => return None,
    };
    let args = match Args::parse(rest) {
        Ok(a) => a,
        Err(e) => {
            println!("usage error: {e}");
            return Some(ExitCode::from(2));
        }
    };
    let Some(mtm) = MainThreadMarker::new() else { return Some(finish(Err("not on the main thread".into()))) };
    // AppKit loaded as an accessory app (no Dock icon, no menu bar): the bridged Space calls,
    // NSWorkspace's notices and NSScreen need it.
    NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let sky = match Sky::open() {
        Ok(s) => s,
        Err(e) => return Some(finish(Err(e))),
    };
    Some(finish(probe(&Ctx { mtm, sky }, &args)))
}

/// The last line and the exit code (module docs).
fn finish(r: Result<(), String>) -> ExitCode {
    match r {
        Ok(()) => {
            println!("RESULT measured");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("RESULT could not measure: {e}");
            ExitCode::FAILURE
        }
    }
}

// ----- printing -----------------------------------------------------------------------------

/// Prints a table: a header row, a rule, the rows, columns padded to their widest cell.
pub(super) fn table(head: &[&str], rows: &[Vec<String>]) {
    let mut width: Vec<usize> = head.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            if i < width.len() {
                width[i] = width[i].max(c.chars().count());
            }
        }
    }
    let line = |cells: Vec<String>| {
        let padded: Vec<String> = cells.iter().enumerate().map(|(i, c)| format!("{c:<w$}", w = width.get(i).copied().unwrap_or(0))).collect();
        println!("  {}", padded.join("  ").trim_end());
    };
    line(head.iter().map(|h| h.to_string()).collect());
    line(width.iter().map(|w| "-".repeat(*w)).collect());
    for r in rows {
        line(r.clone());
    }
}

/// Milliseconds from `from` to `to` (negative when `to` is earlier), to 0.1 ms.
pub(super) fn ms(from: Instant, to: Instant) -> f64 {
    let d = if to >= from { to.duration_since(from).as_secs_f64() } else { -from.duration_since(to).as_secs_f64() };
    (d * 1e4).round() / 10.0
}

/// A table cell: whole milliseconds, or `-` for nothing seen.
pub(super) fn cell(v: Option<f64>) -> String {
    v.map_or("-".into(), |v| format!("{v:.0}"))
}

/// The median of `v` (`None` when empty).
pub(super) fn median(v: &[f64]) -> Option<f64> {
    quantile(v, 0.5)
}

/// The `q` quantile of `v` (nearest rank; `None` when empty).
pub(super) fn quantile(v: &[f64], q: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    Some(s[((s.len() - 1) as f64 * q).round() as usize])
}

/// The largest of `v` (`None` when empty).
pub(super) fn worst(v: &[f64]) -> Option<f64> {
    v.iter().copied().reduce(f64::max)
}

// ----- acting while the run loop turns ------------------------------------------------------

/// Runs `f` on a thread of its own while the main run loop turns (so notices this process
/// hears are stamped as they come, never held behind a key's pauses or an Accessibility call).
pub(super) fn off_main<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        let h = s.spawn(f);
        while !h.is_finished() {
            pump(Duration::from_millis(1));
        }
        h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

/// Turns the run loop until `done` holds or `limit` passes; true when it held.
pub(super) fn pump_until(limit: Duration, done: impl FnMut() -> bool) -> bool {
    pump_until_by(pump, limit, done)
}

fn pump_until_by(turn: fn(Duration), limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        turn(Duration::from_millis(1));
    }
}

// ----- listening as an app -------------------------------------------------------------------
//
// Round r2-probes2 (11-21): the first space-flip run heard no
// `activeSpaceDidChangeNotification` in 24 of 24 legs, while the app hears it. The app runs
// `[NSApp run]`; the probes only turned the CFRunLoop (`CFRunLoopRunInMode`) and never
// finished launching. AppKit reads the window server's messages to this process's
// connection (its events, and the connection notices behind AppKit's own notices, such as
// the Space change) when it fetches events (`nextEventMatchingMask:…`), which `run` does and
// a bare run loop does not; the same gap would explain why `probe-spaces sources` heard no
// `SLSRegisterConnectionNotifyProc` notice at all (11-14). yabai, which hears those notices,
// also ends in `[NSApp run]` (src/yabai.c). So the probes that listen finish launching and
// turn the main thread as `run` does: AppKit's events fetched and sent, the run loop turning
// in the default mode meanwhile (main-queue blocks, Accessibility observers).

/// Finishes AppKit's launch for a probe that listens (`finishLaunching`, as `run` would).
pub(super) fn listen_as_app(ctx: &Ctx) {
    NSApplication::sharedApplication(ctx.mtm).finishLaunching();
}

/// Turns the main thread for `d` as `[NSApp run]` does: each AppKit event fetched and sent,
/// the run loop turning in the default mode while it waits (module notes above).
pub(super) fn app_pump(d: Duration) {
    let Some(mtm) = MainThreadMarker::new() else {
        pump(d);
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let end = Instant::now() + d;
    let until = NSDate::dateWithTimeIntervalSinceNow(d.as_secs_f64());
    // SAFETY: the framework's own constant.
    let mode = unsafe { NSDefaultRunLoopMode };
    loop {
        let sent = objc2::rc::autoreleasepool(|_| match app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, Some(&until), mode, true) {
            Some(e) => {
                app.sendEvent(&e);
                true
            }
            None => false,
        });
        if !sent || Instant::now() >= end {
            return;
        }
    }
}

/// [`off_main`] with the main thread turned by [`app_pump`].
pub(super) fn app_off_main<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        let h = s.spawn(f);
        while !h.is_finished() {
            app_pump(Duration::from_millis(1));
        }
        h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

/// [`pump_until`] with the main thread turned by [`app_pump`].
pub(super) fn app_pump_until(limit: Duration, done: impl FnMut() -> bool) -> bool {
    pump_until_by(app_pump, limit, done)
}

/// The Control modifier bit and the left Control key's code (as [`space_keys::post`] posts them).
const CONTROL: u64 = 0x4_0000;
const CONTROL_KEY: u16 = 59;
/// The pause between a shortcut's events (as [`space_keys::post`]).
const KEY_PAUSE: Duration = Duration::from_millis(30);

/// Posts `key` as [`space_keys::post`] does (Control down, the key down and up, Control up)
/// and returns when its key-down went out: macOS acts on the key-down.
pub(super) fn post_stamped(key: &SpaceKey) -> Result<Instant, String> {
    if !key.enabled {
        return Err(format!("the shortcut {} is turned off in Keyboard Shortcuts", key.describe()));
    }
    let control = key.modifiers & CONTROL != 0;
    if control {
        space_keys::post_key(CONTROL_KEY, true, CONTROL)?;
        std::thread::sleep(KEY_PAUSE);
    }
    let at = Instant::now();
    let r = space_keys::post_key(key.keycode, true, key.modifiers).and_then(|_| {
        std::thread::sleep(KEY_PAUSE);
        space_keys::post_key(key.keycode, false, key.modifiers)
    });
    std::thread::sleep(KEY_PAUSE);
    if control {
        space_keys::post_key(CONTROL_KEY, false, 0)?;
    }
    r.map(|_| at)
}

/// Posts one key with no modifiers (Escape, the Mission Control key); when its key-down went out.
pub(super) fn post_plain_stamped(code: u16) -> Result<Instant, String> {
    let at = Instant::now();
    space_keys::post_key(code, true, 0)?;
    std::thread::sleep(KEY_PAUSE);
    space_keys::post_key(code, false, 0)?;
    Ok(at)
}

// ----- the screens and the windows ------------------------------------------------------------

/// The main display's frame (top-left points), from CoreGraphics.
pub(super) fn main_frame() -> Option<Rect> {
    desktop::cg_display_bounds().first().copied()
}

/// The UUID of the main display (upper case), from ColorSync.
pub(super) fn main_uuid() -> Option<String> {
    desktop_counts::display_uuid(CGMainDisplayID())
}

/// Which entry of `SLSCopyManagedDisplaySpaces` is the main screen's: the one named `Main`,
/// else the one named by the main display's UUID.
pub(super) fn main_entry(list: &[ScreenDesks]) -> Option<usize> {
    let uuid = main_uuid();
    list.iter()
        .position(|d| d.screen == "Main")
        .or_else(|| uuid.and_then(|u| list.iter().position(|d| d.screen.eq_ignore_ascii_case(&u))))
}

/// What the window list shows over `display` now, as [`overview::overview_in`] tells it.
pub(super) fn sight_of(list: &[windows::CgWindow], display: Rect) -> Sight {
    overview::overview_in(list.iter().map(|w| Sighting { owner: &w.owner, layer: w.layer, bounds: w.bounds, onscreen: w.onscreen }), display)
}

/// A fixture's windows on some Space (ids, window-server order) and its first pid.
pub(super) fn fixture(sky: &Sky, bundle: &str) -> (Option<i32>, Vec<windows::CgWindow>) {
    let pids = fixture_pids(bundle);
    (pids.first().copied(), fixture_windows(sky, &pids))
}

/// The bundle path of the running app `pid` (to open it by path: `open -b` may launch
/// another tree's copy, scripts/test_stages.py `activate`).
pub(super) fn app_path(pid: i32) -> Option<String> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    Some(app.bundleURL()?.path()?.to_string())
}

/// True when `b` is within a point of `want` on every side.
pub(super) fn at_frame(b: Rect, want: Rect) -> bool {
    (b.x - want.x).abs() < 1.0 && (b.y - want.y).abs() < 1.0 && (b.w - want.w).abs() < 1.0 && (b.h - want.h).abs() < 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arguments_take_fixtures_only() {
        let w = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        let a = Args::parse(&w("dev.eugene.stagemanager.fixture.stages.a --rounds 5 --window 42")).expect("parses");
        assert_eq!((a.rounds, a.window, a.bundles.len()), (5, Some(42), 1));
        assert!(Args::parse(&w("com.apple.Safari")).is_err());
        assert!(Args::parse(&w("--sticky com.apple.Safari")).is_err());
        assert!(Args::parse(&w("--rounds x")).is_err());
    }

    #[test]
    fn milliseconds_are_signed_and_cells_round() {
        let t = Instant::now();
        let later = t + Duration::from_micros(12_340);
        assert_eq!(ms(t, later), 12.3);
        assert_eq!(ms(later, t), -12.3);
        assert_eq!(cell(Some(12.6)), "13");
        assert_eq!(cell(None), "-");
    }

    #[test]
    fn quantiles_take_the_nearest_rank() {
        let v = [5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(median(&v), Some(3.0));
        assert_eq!(quantile(&v, 1.0), Some(5.0));
        assert_eq!(worst(&v), Some(5.0));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn the_main_entry_is_main_or_its_uuid() {
        let entry = |screen: &str, current| ScreenDesks { screen: screen.into(), current, ..ScreenDesks::default() };
        let list = vec![entry("ABC", 1), entry("Main", 2)];
        assert_eq!(main_entry(&list), Some(1));
    }
}
