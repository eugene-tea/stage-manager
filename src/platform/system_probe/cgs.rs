//! `probe-spaces cgs BUNDLE-A BUNDLE-B BUNDLE-C [--rounds N]` (probe 5, round r2-probes2, the
//! review's R3 and R4): does the window server tell its clients when Mission Control opens or
//! closes, the Desktop changes, or a window changes Space?
//!
//! Both of SkyLight's notice registrations are made for every type 0..=2000:
//! `SLSRegisterConnectionNotifyProc(cid, proc, type, context)` with the callback
//! `(type, data, size_t length, context, cid)` (yabai's src/misc/extern.h), and
//! `SLSRegisterNotifyProc(proc, type, context)` with `(type, data, unsigned int length,
//! context)` (CGSInternal's CGSEvent.h, `CGSRegisterNotifyProc`, MIT). Each notice is stamped in
//! its callback. Then `SLSRequestNotificationsForWindows(cid, ids, count)` (yabai's extern.h;
//! yabai's src/event_loop.c calls it for the windows it tracks) asks for the per-window notices
//! of every fixture window. The process finishes launching and turns as `[NSApp run]` does
//! ([`super::app_pump`]): yabai hears its connection notices that way, and a bare run loop
//! heard none (`probe-spaces sources`, 11-14).
//!
//! Setup: Alpha's one window (BUNDLE-A) in front on the Desktop shown; Bravo's (BUNDLE-B)
//! moved to the next Desktop of the main screen (as `space-flip` does); Charlie (BUNDLE-C) with
//! one window more than there are rounds (one is closed per round). Each round, every action
//! once, in this order, each followed by its 1.5 s window, 0.5 s counted nowhere, and a 3 s
//! rest (nothing done):
//!
//! - Mission Control opened by its shortcut, then closed by it; App Exposé likewise; Show
//!   Desktop likewise (a shortcut turned off is skipped and said);
//! - the Desktop key to the next Desktop and back; Bravo opened by its path (an activation
//!   that slides) and Alpha (back) (both need a second Desktop);
//! - Alpha's window into full screen and out; minimized and back (Accessibility);
//! - one of Charlie's windows closed (its close button);
//! - Alpha's window into a classic Space the probe made (the bridged add, as the launch probe,
//!   `spaces/probe.rs`, does with its panel) and back onto its Desktop.
//!
//! After each pair the probe gets back to the start (Mission Control closed, the start Desktop
//! shown, Alpha out of full screen, not minimized, on its Desktop) or stops. A type heard in
//! any rest (or the 3 s before the first action) is noise. Per action it prints the other
//! types heard in its windows: count, rounds with one (of the rounds the action ran), the
//! median ms from the action to the first, the payload's length and whether a word names a
//! fixture window or a Space id. Verdict lines for Mission Control's open and close, a Desktop
//! change (both Desktop keys) and a window's Space membership change (into the probe's Space
//! and back): "an event exists: type N, in 3/3, M ms after" or "none". Cross-checks from the
//! same run: `activeSpaceDidChangeNotification` (as the app observes it) and the Dock's
//! `AXExpose…` notices (as `dock-notices` registers them).
//!
//! Moves only fixture windows; the probe's Space is recorded in `/tmp/sm-spaces-probe/spaces.txt`
//! before Alpha goes into it (`probe-spaces cleanup` ends it), and is ended at the end. A
//! classic Space ends with the process that made it anyway, its windows back on the Desktop.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use objc2_application_services::{AXObserver, AXUIElement};
use objc2_core_foundation::CFString;

use super::space_flip::{full_screen, open_path, put_on};
use super::{
    Args, Ctx, app_off_main as off_main, app_pump as pump, app_pump_until as pump_until, fixture, listen_as_app, main_entry, main_frame, median,
    ms, post_plain_stamped, post_stamped, sight_of, table,
};
use crate::engine::geometry::Rect;
use crate::platform::ax::{AxElement, AxObserver};
use crate::platform::space_keys::{Direction, ESCAPE_KEY, Expose, SpaceKey};
use crate::platform::spaces::ffi::{ALL_SPACES_MASK, Route, SKYLIGHT, Sky};
use crate::platform::{apps, dock, permissions, windows};

/// The types registered, both ways.
const TYPES: std::ops::RangeInclusive<u32> = 0..=2000;
/// How long after an action its notices count, then a gap counted nowhere, then the rest.
const WINDOW: Duration = Duration::from_millis(1500);
const GAP: Duration = Duration::from_millis(500);
const REST: Duration = Duration::from_secs(3);
/// How long the recovery waits for each step.
const BACK_LIMIT: Duration = Duration::from_secs(4);
/// Notices kept at most (a type that floods stops being kept, and is said).
const KEEP_MAX: usize = 400_000;
/// Words of a payload kept.
const WORDS: usize = 8;

/// One window-server notice: when, its type, which registration, its payload.
#[derive(Clone, Debug)]
struct Notice {
    at: Instant,
    kind: Kind,
    len: u32,
    words: Vec<u32>,
}

/// A notice type and the registration it came through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Kind {
    kind: u32,
    /// `SLSRegisterNotifyProc`'s (the process-wide one), else the connection's.
    plain: bool,
}

impl Kind {
    fn describe(self) -> String {
        format!("{}{}", self.kind, if self.plain { " (plain)" } else { "" })
    }
}

static HEARD: Mutex<Vec<Notice>> = Mutex::new(Vec::new());
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// `activeSpaceDidChangeNotification`s and the Dock's notices heard.
static SPACE_NOTICES: Mutex<Vec<Instant>> = Mutex::new(Vec::new());
static DOCK_NOTICES: Mutex<Vec<(Instant, String)>> = Mutex::new(Vec::new());

fn note(kind: u32, plain: bool, data: *mut c_void, len: usize) {
    let at = Instant::now();
    let n = (len / 4).min(WORDS);
    let mut words = Vec::with_capacity(n);
    if !data.is_null() {
        for i in 0..n {
            // SAFETY: the window server hands `len` bytes at `data`; `i * 4 + 4 <= len`. Read
            // unaligned: the payload's alignment is not documented.
            words.push(unsafe { std::ptr::read_unaligned((data as *const u8).add(i * 4).cast::<u32>()) });
        }
    }
    if let Ok(mut v) = HEARD.lock() {
        if v.len() < KEEP_MAX {
            v.push(Notice { at, kind: Kind { kind, plain }, len: u32::try_from(len).unwrap_or(u32::MAX), words });
        } else {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

extern "C" fn on_connection_notice(kind: u32, data: *mut c_void, len: usize, _ctx: *mut c_void, _cid: i32) {
    note(kind, false, data, len);
}

extern "C" fn on_notice(kind: u32, data: *mut c_void, len: u32, _ctx: *mut c_void) {
    note(kind, true, data, len as usize);
}

unsafe extern "C-unwind" fn on_dock(_: NonNull<AXObserver>, _: NonNull<AXUIElement>, name: NonNull<CFString>, _: *mut c_void) {
    let at = Instant::now();
    // SAFETY: Accessibility hands a live string for the length of the callback.
    let name = unsafe { name.as_ref() }.to_string();
    if let Ok(mut v) = DOCK_NOTICES.lock() {
        v.push((at, name));
    }
}

type ConnectionProc = extern "C" fn(u32, *mut c_void, usize, *mut c_void, i32);
type PlainProc = extern "C" fn(u32, *mut c_void, u32, *mut c_void);
type FnRegisterConnection = unsafe extern "C" fn(i32, ConnectionProc, u32, *mut c_void) -> i32;
type FnRegister = unsafe extern "C" fn(PlainProc, u32, *mut c_void) -> i32;
type FnRequestForWindows = unsafe extern "C" fn(i32, *const u32, i32) -> i32;

/// SkyLight's three notice calls, each `None` when not exported.
struct Calls {
    connection: Option<FnRegisterConnection>,
    plain: Option<FnRegister>,
    request: Option<FnRequestForWindows>,
}

fn calls() -> Calls {
    // SAFETY: a system framework path; RTLD_NOW resolves everything at load.
    let h = unsafe { libc::dlopen(SKYLIGHT.as_ptr(), libc::RTLD_NOW) };
    if h.is_null() {
        return Calls { connection: None, plain: None, request: None };
    }
    let mut names = Vec::new();
    // SAFETY (each load): the signatures the module docs name, from yabai's extern.h and
    // CGSInternal's CGSEvent.h.
    Calls {
        connection: unsafe { crate::platform::spaces::ffi::load(h, c"SLSRegisterConnectionNotifyProc", &mut names) },
        plain: unsafe { crate::platform::spaces::ffi::load(h, c"SLSRegisterNotifyProc", &mut names) },
        request: unsafe { crate::platform::spaces::ffi::load(h, c"SLSRequestNotificationsForWindows", &mut names) },
    }
}

/// Registers both callbacks for every type of [`TYPES`]; how many registrations returned 0.
fn register(c: &Calls, cid: i32) -> (usize, usize) {
    let (mut a, mut b) = (0, 0);
    for kind in TYPES {
        if let Some(f) = c.connection {
            // SAFETY: a callback of the stated type that lives as long as the process.
            a += usize::from(unsafe { f(cid, on_connection_notice, kind, std::ptr::null_mut()) } == 0);
        }
        if let Some(f) = c.plain {
            // SAFETY: as above.
            b += usize::from(unsafe { f(on_notice, kind, std::ptr::null_mut()) } == 0);
        }
    }
    (a, b)
}

/// One action as done: its label, when it was caused, and why it did not run.
#[derive(Clone, Debug)]
struct Act {
    label: &'static str,
    cause: Instant,
    failed: Option<String>,
}

/// A stretch with nothing done.
#[derive(Clone, Copy, Debug)]
struct Rest {
    from: Instant,
    to: Instant,
}

/// The fixture windows and where they belong.
struct Scene {
    a_pid: i32,
    a: u32,
    a_path: String,
    c_pid: i32,
    /// Charlie's windows still open, the next to close first.
    c_left: Vec<u32>,
    start: u64,
    main_id: String,
    display: Rect,
}

/// The actions' labels, in the order they run (pairs: the second undoes the first).
const MC_OPEN: &str = "Mission Control opened (shortcut)";
const MC_CLOSE: &str = "Mission Control closed (shortcut)";
const AE_OPEN: &str = "App Exposé opened (shortcut)";
const AE_CLOSE: &str = "App Exposé closed (shortcut)";
const SD_OPEN: &str = "Show Desktop opened (shortcut)";
const SD_CLOSE: &str = "Show Desktop closed (shortcut)";
const KEY_TO: &str = "Desktop key to the next Desktop";
const KEY_BACK: &str = "Desktop key back";
const SLIDE_TO: &str = "Bravo opened (an activation that slides)";
const SLIDE_BACK: &str = "Alpha opened (slides back)";
const FULL_IN: &str = "Alpha into full screen";
const FULL_OUT: &str = "Alpha out of full screen";
const MIN_IN: &str = "Alpha minimized";
const MIN_OUT: &str = "Alpha back from the Dock";
const CLOSED: &str = "a Charlie window closed";
const SPACE_IN: &str = "Alpha into the probe's Space";
const SPACE_OUT: &str = "Alpha back onto its Desktop";
const LABELS: [&str; 17] =
    [MC_OPEN, MC_CLOSE, AE_OPEN, AE_CLOSE, SD_OPEN, SD_CLOSE, KEY_TO, KEY_BACK, SLIDE_TO, SLIDE_BACK, FULL_IN, FULL_OUT, MIN_IN, MIN_OUT, CLOSED, SPACE_IN, SPACE_OUT];

/// What the run keeps: the actions, the rests, the Space ids seen.
#[derive(Default)]
struct Log {
    acts: Vec<Act>,
    rests: Vec<Rest>,
    spaces: BTreeSet<u64>,
}

impl Log {
    /// Does `f` off the main thread, then its window, the gap and a rest.
    fn act(&mut self, sky: &Sky, label: &'static str, f: &(dyn Fn() -> Result<Instant, String> + Sync)) -> bool {
        match off_main(f) {
            Ok(cause) => {
                pump((cause + WINDOW + GAP).saturating_duration_since(Instant::now()));
                self.note_spaces(sky);
                self.rest();
                self.acts.push(Act { label, cause, failed: None });
                true
            }
            Err(e) => {
                self.failed(label, e);
                false
            }
        }
    }

    fn failed(&mut self, label: &'static str, why: String) {
        self.acts.push(Act { label, cause: Instant::now(), failed: Some(why) });
        pump(GAP);
    }

    fn rest(&mut self) {
        let from = Instant::now();
        pump(REST);
        self.rests.push(Rest { from, to: Instant::now() });
    }

    fn note_spaces(&mut self, sky: &Sky) {
        for screen in sky.displays() {
            self.spaces.insert(screen.current);
            self.spaces.extend(screen.every());
        }
    }
}

fn ax_window(pid: i32, id: u32) -> Result<AxElement, String> {
    AxElement::application(pid)
        .windows()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|e| e.window_id().ok() == Some(id))
        .ok_or_else(|| "the fixture window is not reachable through Accessibility".to_string())
}

impl Scene {
    fn main_space(&self, sky: &Sky) -> u64 {
        sky.displays().iter().find(|d| d.screen == self.main_id).map_or(0, |d| d.current)
    }

    fn overview_shows(&self) -> bool {
        let s = sight_of(&windows::on_screen(), self.display);
        s.overview.is_some()
    }

    fn a_onscreen(&self) -> bool {
        windows::describe(&[self.a]).first().is_some_and(|w| w.onscreen)
    }

    fn a_full(&self) -> bool {
        windows::describe(&[self.a]).first().is_some_and(|w| w.onscreen && crate::engine::fullscreen::covers_display(w.bounds, self.display))
    }

    /// Back at the start: nothing of macOS's showing, the start Desktop shown, Alpha out of
    /// full screen, not minimized, on its Desktop and on screen. True when it is.
    fn home(&self, sky: &Sky, back_key: Option<&SpaceKey>, probe_space: Option<u64>) -> bool {
        let at_home = || {
            !self.overview_shows() && self.main_space(sky) == self.start && self.a_onscreen() && !self.a_full() && !sky.spaces_of(&[self.a]).iter().any(|s| Some(*s) == probe_space)
        };
        if at_home() {
            return true;
        }
        if self.overview_shows() {
            let _ = off_main(|| post_plain_stamped(ESCAPE_KEY));
            pump_until(BACK_LIMIT, || !self.overview_shows());
        }
        if probe_space.is_some_and(|p| sky.spaces_of(&[self.a]).contains(&p)) {
            let _ = sky.add_remove_from(Route::Bridged, self.start, &[self.a], ALL_SPACES_MASK);
            pump(Duration::from_millis(300));
        }
        if self.a_full() {
            let _ = off_main(|| full_screen(self.a_pid, self.a, false));
            pump_until(BACK_LIMIT, || !self.a_full());
        }
        if ax_window(self.a_pid, self.a).and_then(|w| w.is_minimized().map_err(|e| e.to_string())).unwrap_or(false) {
            let _ = off_main(|| ax_window(self.a_pid, self.a).and_then(|w| w.set_minimized(false).map_err(|e| e.to_string())));
            pump_until(BACK_LIMIT, || self.a_onscreen());
        }
        if self.main_space(sky) != self.start {
            let _ = off_main(|| open_path(&self.a_path));
            pump_until(BACK_LIMIT, || self.main_space(sky) == self.start);
        }
        if self.main_space(sky) != self.start
            && let Some(k) = back_key
        {
            let _ = off_main(|| post_stamped(k));
            pump_until(BACK_LIMIT, || self.main_space(sky) == self.start);
        }
        pump(Duration::from_millis(500));
        at_home()
    }
}

pub(super) fn run(ctx: &Ctx, args: &Args) -> Result<(), String> {
    let usage = "cgs BUNDLE-A BUNDLE-B BUNDLE-C [--rounds N]";
    let (bundle_a, bundle_b, bundle_c) = (args.bundle(0, usage)?, args.bundle(1, usage)?, args.bundle(2, usage)?);
    if !permissions::accessibility_trusted() {
        return Err("no Accessibility for this process (launch it through `open` from the dev app)".into());
    }
    listen_as_app(ctx);
    let sky = &ctx.sky;
    if sky.copy_managed.is_none() || sky.create.is_none() {
        return Err("SLSCopyManagedDisplaySpaces or SLSSpaceCreate is missing on this macOS".into());
    }
    let c = calls();
    if c.connection.is_none() && c.plain.is_none() {
        return Err("neither SLSRegisterConnectionNotifyProc nor SLSRegisterNotifyProc is exported on this macOS".into());
    }
    let display = main_frame().ok_or("no main display")?;
    let displays = sky.displays();
    let main = main_entry(&displays).ok_or("no entry of SLSCopyManagedDisplaySpaces is the main screen's")?;
    let (main_id, start) = (displays[main].screen.clone(), displays[main].current);
    let desktops: Vec<u64> = displays[main].desks.iter().map(|d| d.id).collect();
    let at = desktops.iter().position(|d| *d == start).ok_or("the main screen shows no Desktop (a full-screen app's Space?): show a Desktop first")?;
    let next = if at + 1 < desktops.len() {
        Some((desktops[at + 1], Direction::Right))
    } else if at > 0 {
        Some((desktops[at - 1], Direction::Left))
    } else {
        None
    };
    let on_screen = |bundle: &str| -> Result<(i32, Vec<u32>), String> {
        let (pid, list) = fixture(sky, bundle);
        let pid = pid.ok_or(format!("{bundle} is not running"))?;
        let ids: Vec<u32> = list.iter().filter(|w| w.onscreen).map(|w| w.id).collect();
        if ids.is_empty() {
            return Err(format!("{bundle} has no window on screen"));
        }
        Ok((pid, ids))
    };
    let (a_pid, a_ids) = on_screen(bundle_a)?;
    let (b_pid, b_ids) = on_screen(bundle_b)?;
    let (c_pid, c_ids) = on_screen(bundle_c)?;
    let (a, b) = (a_ids[0], b_ids[0]);
    let a_path = super::app_path(a_pid).ok_or(format!("{bundle_a}'s app path is unknown"))?;
    let b_path = super::app_path(b_pid).ok_or(format!("{bundle_b}'s app path is unknown"))?;
    println!("== cgs: the window server's notices to this process, every type {}..{} both ways ==", TYPES.start(), TYPES.end());
    let (conn_ok, plain_ok) = register(&c, sky.cid);
    let n = TYPES.count();
    println!(
        "  registered: SLSRegisterConnectionNotifyProc {conn_ok} of {n} types{}; SLSRegisterNotifyProc {plain_ok} of {n}{}",
        if c.connection.is_none() { " (not exported)" } else { "" },
        if c.plain.is_none() { " (not exported)" } else { "" }
    );
    let fixture_ids: Vec<u32> = a_ids.iter().chain(&b_ids).chain(&c_ids).copied().collect();
    let asked = c.request.map(|f| {
        let count = i32::try_from(fixture_ids.len()).unwrap_or(0);
        // SAFETY: `fixture_ids` holds `count` window ids and outlives the call.
        unsafe { f(sky.cid, fixture_ids.as_ptr(), count) }
    });
    println!(
        "  SLSRequestNotificationsForWindows for the {} fixture windows: {}",
        fixture_ids.len(),
        asked.map_or("not exported".into(), |r| format!("returned {r}"))
    );
    let _space_observer = apps::observe_space_changes(|| {
        if let Ok(mut v) = SPACE_NOTICES.lock() {
            v.push(Instant::now());
        }
    });
    let dock_observer = dock::dock_pid().and_then(|pid| {
        let o = AxObserver::new(pid, on_dock).ok()?;
        let app = AxElement::application(pid);
        let ok = ["AXExposeShowAllWindows", "AXExposeShowFrontWindows", "AXExposeShowDesktop", "AXExposeExit"].iter().filter(|n| o.add(&app, n, 0).is_ok()).count();
        Some((o, ok))
    });
    println!("  the Dock's AXExpose notices registered (Accessibility, cross-check): {}", dock_observer.as_ref().map_or("no observer on the Dock".into(), |(_, k)| format!("{k} of 4")));
    println!("  the main screen has {} Desktops; the probe starts on number {}", desktops.len(), at + 1);
    let moved = next.is_some_and(|(sid, _)| put_on(sky, &[b], sid));
    match next {
        Some(_) => println!("  Bravo's window moved to the other Desktop: {}", if moved { "yes" } else { "NO (the Desktop key and activation actions are skipped)" }),
        None => println!("  SKIPPED the Desktop keys and the activations: the main screen has one Desktop (add one in Mission Control and run again)"),
    }
    let _ = std::fs::create_dir_all(super::super::PROBE_DIR);
    let probe_space = match sky.create(Route::Classic, false) {
        Ok(0) | Err(_) => None,
        Ok(sid) => {
            super::super::record(sid, Route::Classic);
            Some(sid)
        }
    };
    println!("  the probe's classic Space: {}", if probe_space.is_some() { "made" } else { "NOT made (the Space move is skipped)" });
    let mut scene = Scene { a_pid, a, a_path, c_pid, c_left: c_ids.clone(), start, main_id, display };
    // Charlie's last window stays: its app keeps a window.
    let closable = scene.c_left.len().saturating_sub(1);
    let expose = |e: Expose| SpaceKey::read_expose(e);
    for (label, e) in [("Mission Control", Expose::MissionControl), ("App Exposé", Expose::AppWindows), ("Show Desktop", Expose::ShowDesktop)] {
        let k = expose(e);
        println!("  {label}'s shortcut: {} ({})", k.describe(), if k.enabled { "on" } else { "OFF, skipped" });
    }
    let keys = next.map(|(_, d)| (SpaceKey::read(d), SpaceKey::read(if d == Direction::Right { Direction::Left } else { Direction::Right })));
    let back_key = keys.as_ref().map(|k| &k.1);
    let mut log = Log::default();
    log.note_spaces(sky);
    pump(Duration::from_millis(300));
    // The rest before the first action.
    log.rest();
    let mut stopped = false;
    for round in 0..args.rounds {
        for (open_label, close_label, e) in [(MC_OPEN, MC_CLOSE, Expose::MissionControl), (AE_OPEN, AE_CLOSE, Expose::AppWindows), (SD_OPEN, SD_CLOSE, Expose::ShowDesktop)] {
            let k = expose(e);
            if stopped || !k.enabled {
                continue;
            }
            if log.act(sky, open_label, &|| post_stamped(&k)) && scene.overview_shows() {
                log.act(sky, close_label, &|| post_stamped(&k));
            } else {
                log.failed(close_label, "the open did not show".into());
            }
            stopped |= !scene.home(sky, back_key, probe_space);
        }
        if let (true, Some((to, back)), Some((sid, _))) = (moved && !stopped, keys.as_ref(), next) {
            if log.act(sky, KEY_TO, &|| post_stamped(to)) && scene.main_space(sky) == sid {
                log.act(sky, KEY_BACK, &|| post_stamped(back));
            } else {
                log.failed(KEY_BACK, "the key did not change the Desktop".into());
            }
            stopped |= !scene.home(sky, back_key, probe_space);
            if !stopped {
                if log.act(sky, SLIDE_TO, &|| open_path(&b_path)) && scene.main_space(sky) == sid {
                    log.act(sky, SLIDE_BACK, &|| open_path(&scene.a_path));
                } else {
                    log.failed(SLIDE_BACK, "the activation did not change the Desktop".into());
                }
                stopped |= !scene.home(sky, back_key, probe_space);
            }
        }
        if !stopped {
            if log.act(sky, FULL_IN, &|| full_screen(a_pid, a, true)) && scene.a_full() {
                log.act(sky, FULL_OUT, &|| full_screen(a_pid, a, false));
            } else {
                log.failed(FULL_OUT, "the window did not go full screen".into());
            }
            stopped |= !scene.home(sky, back_key, probe_space);
        }
        if !stopped {
            let minimize = |on: bool| -> Result<Instant, String> {
                let w = ax_window(a_pid, a)?;
                let at = Instant::now();
                w.set_minimized(on).map_err(|e| format!("minimize: {e}"))?;
                Ok(at)
            };
            if log.act(sky, MIN_IN, &|| minimize(true)) && !scene.a_onscreen() {
                log.act(sky, MIN_OUT, &|| minimize(false));
            } else {
                log.failed(MIN_OUT, "the window did not go to the Dock".into());
            }
            stopped |= !scene.home(sky, back_key, probe_space);
        }
        if !stopped && round < closable {
            let id = scene.c_left.remove(0);
            let c_pid = scene.c_pid;
            log.act(sky, CLOSED, &|| {
                let w = ax_window(c_pid, id)?;
                let at = Instant::now();
                w.press_close_button().map_err(|e| format!("close: {e}"))?;
                Ok(at)
            });
        }
        if let (false, Some(space)) = (stopped, probe_space) {
            let into = || -> Result<Instant, String> {
                let at = Instant::now();
                sky.add_remove(Route::Bridged, space, &[a])?;
                Ok(at)
            };
            let back = || -> Result<Instant, String> {
                let at = Instant::now();
                sky.add_remove_from(Route::Bridged, start, &[a], ALL_SPACES_MASK)?;
                Ok(at)
            };
            if log.act(sky, SPACE_IN, &into) && sky.spaces_of(&[a]) == [space] {
                log.act(sky, SPACE_OUT, &back);
            } else {
                log.failed(SPACE_OUT, "the window did not go into the probe's Space".into());
            }
            stopped |= !scene.home(sky, back_key, probe_space);
        }
        if stopped {
            println!("  STOPPED in round {}: the probe could not get back to the start (the start Desktop, Alpha on it and out of full screen)", round + 1);
            break;
        }
    }
    // Put back: Alpha on its Desktop, the probe's Space ended, Bravo home.
    let home = scene.home(sky, back_key, probe_space);
    let ended = probe_space.is_none_or(|sid| {
        let _ = sky.destroy(Route::Classic, sid);
        pump(Duration::from_millis(200));
        let gone = !sky.all_spaces(ALL_SPACES_MASK).contains(&sid);
        if gone {
            super::super::unrecord(sid);
        }
        gone
    });
    let back = if moved { put_on(sky, &[b], start) } else { true };
    drop(dock_observer);
    println!(
        "  after: at the start: {}; the probe's Space ended: {}; Bravo's window back on the start Desktop: {}",
        yes(home),
        yes(ended),
        yes(back)
    );
    let heard = HEARD.lock().map(|v| v.clone()).unwrap_or_default();
    let dropped = DROPPED.load(Ordering::Relaxed);
    let space_notices = SPACE_NOTICES.lock().map(|v| v.clone()).unwrap_or_default();
    let dock_notices = DOCK_NOTICES.lock().map(|v| v.clone()).unwrap_or_default();
    let mut ids = Ids { windows: fixture_ids, spaces: log.spaces.clone() };
    ids.spaces.extend(probe_space);
    report(&log, &heard, dropped, &ids, &space_notices, &dock_notices);
    if log.acts.iter().all(|a| a.failed.is_some()) {
        return Err("no action could run".into());
    }
    Ok(())
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "NO" }
}

/// The ids a payload word may name.
struct Ids {
    windows: Vec<u32>,
    spaces: BTreeSet<u64>,
}

impl Ids {
    fn names_window(&self, n: &Notice) -> bool {
        n.words.iter().any(|w| self.windows.contains(w))
    }

    fn names_space(&self, n: &Notice) -> bool {
        n.words.windows(2).any(|p| self.spaces.contains(&(u64::from(p[0]) | (u64::from(p[1]) << 32)))) || n.words.iter().any(|w| *w != 0 && self.spaces.contains(&u64::from(*w)))
    }
}

/// One type's line for one action.
#[derive(Debug, Default, PartialEq)]
struct Row {
    count: usize,
    rounds: usize,
    first: Vec<f64>,
    window: bool,
    space: bool,
    lens: BTreeSet<u32>,
}

/// Every type heard in a rest: (notices, rests with one).
fn noise(heard: &[Notice], rests: &[Rest]) -> BTreeMap<Kind, (usize, usize)> {
    let mut out: BTreeMap<Kind, (usize, usize)> = BTreeMap::new();
    for r in rests {
        let mut seen = BTreeSet::new();
        for n in heard.iter().filter(|n| n.at >= r.from && n.at <= r.to) {
            let e = out.entry(n.kind).or_default();
            e.0 += 1;
            if seen.insert(n.kind) {
                e.1 += 1;
            }
        }
    }
    out
}

/// The types not noise heard within [`WINDOW`] after each run of `label`, and how many runs.
fn rows_for(label: &str, acts: &[Act], heard: &[Notice], noisy: &BTreeMap<Kind, (usize, usize)>, ids: &Ids) -> (BTreeMap<Kind, Row>, usize) {
    let mine: Vec<&Act> = acts.iter().filter(|a| a.label == label && a.failed.is_none()).collect();
    let mut out: BTreeMap<Kind, Row> = BTreeMap::new();
    for a in &mine {
        let end = a.cause + WINDOW;
        let mut first: BTreeMap<Kind, Instant> = BTreeMap::new();
        for n in heard.iter().filter(|n| n.at >= a.cause && n.at <= end && !noisy.contains_key(&n.kind)) {
            let r = out.entry(n.kind).or_default();
            r.count += 1;
            r.window |= ids.names_window(n);
            r.space |= ids.names_space(n);
            r.lens.insert(n.len);
            first.entry(n.kind).or_insert(n.at);
        }
        for (k, at) in first {
            if let Some(r) = out.get_mut(&k) {
                r.rounds += 1;
                r.first.push(ms(a.cause, at));
            }
        }
    }
    (out, mine.len())
}

/// The verdict words for `rows` of an action run `runs` times: the types in every run.
fn exists(rows: &[(&BTreeMap<Kind, Row>, usize)], heard_any: bool) -> String {
    if !heard_any {
        return "not measured (no notice of any type was heard in the whole run)".into();
    }
    if rows.iter().any(|(_, runs)| *runs == 0) {
        return "not measured (the action did not run)".into();
    }
    // A type in every run of every action named, by its median first ms over them all.
    let Some((first, _)) = rows.first() else { return "not measured".into() };
    let mut found: Vec<(Kind, f64, usize)> = first
        .keys()
        .filter(|k| rows.iter().all(|(r, runs)| r.get(k).is_some_and(|row| row.rounds == *runs)))
        .map(|k| {
            let all: Vec<f64> = rows.iter().flat_map(|(r, _)| r[k].first.iter().copied()).collect();
            (*k, median(&all).unwrap_or(0.0), rows.iter().map(|(_, runs)| runs).sum())
        })
        .collect();
    found.sort_by(|x, y| x.1.total_cmp(&y.1));
    let Some((k, at, runs)) = found.first() else { return "none".into() };
    let also: Vec<String> = found[1..].iter().map(|(k, at, _)| format!("{} ({at:.0} ms)", k.describe())).collect();
    format!(
        "an event exists: type {}{}, in {runs}/{runs}, {at:.0} ms after{}",
        k.describe(),
        name_of(k.kind).map_or(String::new(), |n| format!(" ({n})")),
        if also.is_empty() { String::new() } else { format!("; also {}", also.join(", ")) }
    )
}

fn report(log: &Log, heard: &[Notice], dropped: u64, ids: &Ids, space_notices: &[Instant], dock_notices: &[(Instant, String)]) {
    let noisy = noise(heard, &log.rests);
    println!(
        "\n  heard {} notices in the whole run ({} types){}; {} rests of {} s",
        heard.len(),
        heard.iter().map(|n| n.kind).collect::<BTreeSet<_>>().len(),
        if dropped > 0 { format!(", {dropped} more not kept") } else { String::new() },
        log.rests.len(),
        REST.as_secs()
    );
    for label in LABELS {
        let failed: Vec<&str> = log.acts.iter().filter(|a| a.label == label).filter_map(|a| a.failed.as_deref()).collect();
        let (rows, runs) = rows_for(label, &log.acts, heard, &noisy, ids);
        let mut lines: Vec<(&Kind, &Row)> = rows.iter().collect();
        lines.sort_by(|x, y| median(&x.1.first).unwrap_or(0.0).total_cmp(&median(&y.1.first).unwrap_or(0.0)));
        println!(
            "\n  {label}: ran {runs} times{}{}",
            if failed.is_empty() { String::new() } else { format!(", not {} ({})", failed.len(), failed.join("; ")) },
            if runs > 0 && rows.is_empty() { "; nothing but noise in its windows" } else { "" }
        );
        if rows.is_empty() {
            continue;
        }
        let cells: Vec<Vec<String>> = lines
            .iter()
            .map(|(k, r)| {
                vec![
                    k.describe(),
                    r.count.to_string(),
                    format!("{}/{runs}{}", r.rounds, if r.rounds == runs { " all" } else { "" }),
                    super::cell(median(&r.first)),
                    r.lens.iter().map(u32::to_string).collect::<Vec<_>>().join(","),
                    [(r.window, "window"), (r.space, "Space")].iter().filter(|(b, _)| *b).map(|(_, s)| *s).collect::<Vec<_>>().join(" "),
                    name_of(k.kind).unwrap_or("").into(),
                ]
            })
            .collect();
        table(&["type", "heard", "rounds", "first ms (median)", "payload bytes", "payload names", "public name (older macOS)"], &cells);
    }
    println!("\n  noise: the types heard in the rests (nothing done)");
    if noisy.is_empty() {
        println!("  none");
    } else {
        let cells: Vec<Vec<String>> = noisy
            .iter()
            .map(|(k, (count, rests))| vec![k.describe(), count.to_string(), format!("{rests}/{}", log.rests.len()), name_of(k.kind).unwrap_or("").into()])
            .collect();
        table(&["type", "heard", "rests with it", "public name (older macOS)"], &cells);
    }
    // The per-window types at a move between Spaces (the payload names a fixture window).
    for label in [SPACE_IN, SPACE_OUT] {
        let (rows, _) = rows_for(label, &log.acts, heard, &noisy, ids);
        let per_window: Vec<String> = rows.iter().filter(|(_, r)| r.window).map(|(k, r)| format!("{} ({}/{} rounds)", k.describe(), r.rounds, log.acts.iter().filter(|a| a.label == label && a.failed.is_none()).count())).collect();
        println!("  per-window types at \"{label}\": {}", if per_window.is_empty() { "none".into() } else { per_window.join(", ") });
    }
    // The cross-checks.
    let changes = [KEY_TO, KEY_BACK, SLIDE_TO, SLIDE_BACK, FULL_IN, FULL_OUT];
    let runs_of = |labels: &[&str]| log.acts.iter().filter(|a| labels.contains(&a.label) && a.failed.is_none()).count();
    let with = |labels: &[&str], stamps: &[Instant]| {
        log.acts.iter().filter(|a| labels.contains(&a.label) && a.failed.is_none() && stamps.iter().any(|t| *t >= a.cause && *t <= a.cause + WINDOW)).count()
    };
    println!(
        "  cross-check: activeSpaceDidChangeNotification heard {} times; within {} ms after {} of {} Desktop changes (keys, activations, full screen)",
        space_notices.len(),
        WINDOW.as_millis(),
        with(&changes, space_notices),
        runs_of(&changes)
    );
    let opens = [MC_OPEN, AE_OPEN, SD_OPEN];
    let dock_at: Vec<Instant> = dock_notices.iter().map(|(t, _)| *t).collect();
    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, n) in dock_notices {
        *names.entry(n.as_str()).or_default() += 1;
    }
    println!(
        "  cross-check: the Dock's AXExpose notices heard {} times ({}); within {} ms after {} of {} opens",
        dock_notices.len(),
        if names.is_empty() { "none".into() } else { names.iter().map(|(n, c)| format!("{n} {c}")).collect::<Vec<_>>().join(", ") },
        WINDOW.as_millis(),
        with(&opens, &dock_at),
        runs_of(&opens)
    );
    println!();
    let heard_any = !heard.is_empty();
    if !heard_any {
        println!("  NOTE: no notice of any type came through either registration in the whole run: the listening itself did not work, so every verdict below is void");
    }
    let r = |label: &str| rows_for(label, &log.acts, heard, &noisy, ids);
    let (mc_open, mc_close) = (r(MC_OPEN), r(MC_CLOSE));
    let (key_to, key_back) = (r(KEY_TO), r(KEY_BACK));
    let (space_in, space_out) = (r(SPACE_IN), r(SPACE_OUT));
    println!("VERDICT Mission Control's open (its shortcut): {}", exists(&[(&mc_open.0, mc_open.1)], heard_any));
    println!("VERDICT Mission Control's close (its shortcut): {}", exists(&[(&mc_close.0, mc_close.1)], heard_any));
    println!("VERDICT a Desktop change (the Desktop key there and back): {}", exists(&[(&key_to.0, key_to.1), (&key_back.0, key_back.1)], heard_any));
    println!("VERDICT a window's Space membership change (into the probe's Space and back): {}", exists(&[(&space_in.0, space_in.1), (&space_out.0, space_out.1)], heard_any));
}

/// What public sources call a type (CGSInternal's CGSEvent.h, MIT, from older macOS; yabai's
/// src/mission_control.c for 1204, 1327, 1328 and 1202). The numbers may mean something else
/// on macOS 27: printed as a lead, never as a finding.
fn name_of(kind: u32) -> Option<&'static str> {
    Some(match kind {
        100 => "display will reconfigure",
        101 => "display did reconfigure",
        102 => "display will sleep",
        103 => "display did wake",
        116 => "display full-screen app running",
        117 => "display full-screen app done",
        118 => "display reconfigure happened",
        400 => "foreground process switched",
        401 => "special key pressed",
        402 => "foreground switch requested but redundant",
        700 => "special key event",
        710..=738 => "an input event",
        750 => "app is unresponsive",
        751 => "app is no longer unresponsive",
        760..=766 => "symbolic hot keys",
        800 => "window is obscured",
        801 => "window is unobscured",
        802 => "window is ordered in",
        803 => "window is ordered out",
        804 => "window is terminated (yabai: window destroyed)",
        805 => "window is changing screens",
        806 => "window did move",
        807 => "window did resize",
        808 => "window did change order (yabai: window ordered)",
        809 => "window geometry did change",
        811 => "window did create",
        815 => "window is visible",
        816 => "window is invisible",
        904 => "connection windows became visible",
        905 => "connection windows became occluded",
        906 => "connection window modifications started",
        907 => "connection window modifications stopped",
        912 => "window became visible",
        913 => "window became occluded",
        1000 => "server window did create",
        1001 => "server window will terminate",
        1002 => "server window order did change",
        1003 => "server window did terminate",
        1202 => "yabai: the ⌘Tab switcher",
        1204 => "yabai: Mission Control entered (before macOS 12)",
        1205 => "window was moved by the Dock",
        1207 => "window was resized by the Dock",
        1208 => "window did become managed by the Dock",
        1306 => "packages: workspaces disabled",
        1307 => "packages: workspaces enabled",
        1308 => "packages: status bar Space changed",
        1327 => "yabai: Space created",
        1328 => "yabai: Space destroyed",
        1400 => "workspace will change",
        1401 => "workspace did change",
        1402 => "workspace window is viewable",
        1403 => "workspace window is not viewable",
        1404 => "workspace window did move",
        1405 => "workspace prefs did change",
        1411 => "workspaces window drag did start",
        1412 => "workspaces window drag did end",
        1413 => "workspaces window drag will end",
        1414 => "workspaces show Space for process",
        1415 => "window ordered in on non-current managed Spaces only",
        1416 => "window ordered out on non-current managed Spaces",
        1500..=1506 => "session console or remote",
        1550..=1555 => "session create or destroy",
        1700 => "transition did finish",
        1800..=1804 => "display server change",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(at: Instant, kind: u32, words: Vec<u32>) -> Notice {
        Notice { at, kind: Kind { kind, plain: false }, len: (words.len() * 4) as u32, words }
    }

    #[test]
    fn a_type_of_the_rests_is_noise_and_the_rest_counts_per_round() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let ids = Ids { windows: vec![42], spaces: [7u64].into_iter().collect() };
        let rests = [Rest { from: at(0), to: at(3000) }];
        let acts = vec![
            Act { label: MC_OPEN, cause: at(4000), failed: None },
            Act { label: MC_OPEN, cause: at(9000), failed: None },
            Act { label: MC_OPEN, cause: at(14000), failed: Some("off".into()) },
        ];
        let heard = vec![
            n(at(100), 808, vec![1]),
            n(at(4040), 1204, vec![]),
            n(at(4050), 808, vec![42]),
            n(at(4060), 1401, vec![7, 0]),
            n(at(9030), 1204, vec![]),
            n(at(9031), 1204, vec![]),
            n(at(9600), 1300, vec![42]),
            // After its window: counted for no action.
            n(at(5600), 1205, vec![]),
        ];
        let noisy = noise(&heard, &rests);
        assert_eq!(noisy.get(&Kind { kind: 808, plain: false }), Some(&(1, 1)));
        let (rows, runs) = rows_for(MC_OPEN, &acts, &heard, &noisy, &ids);
        assert_eq!(runs, 2);
        assert!(!rows.contains_key(&Kind { kind: 808, plain: false }), "noise is left out");
        assert!(!rows.contains_key(&Kind { kind: 1205, plain: false }), "outside every window");
        let mc = &rows[&Kind { kind: 1204, plain: false }];
        assert_eq!((mc.count, mc.rounds, mc.first.clone()), (3, 2, vec![40.0, 30.0]));
        assert!(rows[&Kind { kind: 1401, plain: false }].space);
        assert!(rows[&Kind { kind: 1300, plain: false }].window);
        let v = exists(&[(&rows, runs)], true);
        assert!(v.starts_with("an event exists: type 1204 (yabai: Mission Control entered (before macOS 12)), in 2/2, 40 ms after"), "{v}");
        assert!(!v.contains("also"), "1401 and 1300 came in one round of two: {v}");
    }

    #[test]
    fn verdicts_say_none_or_not_measured() {
        let empty: BTreeMap<Kind, Row> = BTreeMap::new();
        assert_eq!(exists(&[(&empty, 3)], true), "none");
        assert!(exists(&[(&empty, 0)], true).starts_with("not measured"));
        assert!(exists(&[(&empty, 3)], false).starts_with("not measured"));
        // Both legs must have it in every run.
        let mut a = BTreeMap::new();
        a.insert(Kind { kind: 1401, plain: true }, Row { count: 3, rounds: 3, first: vec![10.0, 12.0, 14.0], ..Default::default() });
        let mut b = BTreeMap::new();
        b.insert(Kind { kind: 1401, plain: true }, Row { count: 2, rounds: 2, first: vec![10.0, 12.0], ..Default::default() });
        assert_eq!(exists(&[(&a, 3), (&b, 3)], true), "none");
        assert!(exists(&[(&a, 3), (&a, 3)], true).starts_with("an event exists: type 1401 (plain) (workspace did change), in 6/6, 12 ms after"));
    }

    #[test]
    fn a_space_id_is_read_from_two_words() {
        let ids = Ids { windows: vec![], spaces: [(5u64 << 32) | 3].into_iter().collect() };
        assert!(ids.names_space(&n(Instant::now(), 1, vec![9, 3, 5])));
        assert!(!ids.names_space(&n(Instant::now(), 1, vec![3])));
    }
}
