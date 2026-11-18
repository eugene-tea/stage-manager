//! `stage-manager probe-spaces sources …`: how early each source tells of a newly opened
//! app's first window, against the moment the window server first lists it on screen (round
//! sm-placenew). Reads only: no window is moved, so any app may be measured.
//!
//! Each round opens the app (`open -n`), then turns the main run loop every ~0.25 ms and
//! stamps, from the `open`:
//! - **launch**: the app's pid first in `NSWorkspace`'s running apps (the app's own launch
//!   notice comes the same way);
//! - **listed**: each new layer-0 window of that pid first in the window server's complete
//!   list (on screen or not), **desktop**: first on the Desktop shown
//!   (`SLSCopySpacesForWindows`), **onscreen**: first listed on screen;
//! - **ax_answer**: the app's first Accessibility answer (an observer's window-created
//!   registration, tried every 2 ms from the launch), **ax_listed**: the window first in its
//!   `AXWindows` (read at the answer and every 2 ms after), **ax_created**: its window-created
//!   notice;
//! - **cgs**: the window server's notices (`SLSRegisterConnectionNotifyProc` and
//!   `SLSRegisterNotifyProc`, every event type in `--events A-B`, default 700-1700) whose
//!   payload names the window id, each with its type.
//!
//! `--hold-ms MS` (a fixture app only) holds each new window in a Space of its own from its
//! first listing and lets it out MS after its app ordered it in, with every other one
//! ordered in by then, one by one: front first as the complete list orders them, or back
//! first (`--release-reverse`); the round reports that order (`held_order`), the order let
//! out (`release_order`) and their order on screen after (`out_order`), front to back.
//!
//! `sources APP-PATH BUNDLE [--rounds N] [--events A-B] [--hold-ms MS] [--release-reverse] [--args …]`: APP-PATH is passed to
//! `open -n` (an app bundle path, or `-a Name`); every round quits the app after (terminate,
//! then force after 3 s). One JSON line per round, then a summary.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::process::ExitCode;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSRunningApplication};
use objc2_application_services::{AXObserver, AXUIElement};
use objc2_core_foundation::{CFRunLoop, CFString, kCFRunLoopDefaultMode};
use objc2_foundation::{MainThreadMarker, NSString};
use serde_json::{Value, json};

use super::ax::{AxElement, AxObserver, notifications};
use super::spaces::ffi::{ALL_SPACES_MASK, Route, SKYLIGHT, Sky};
use super::windows;

/// One window server notice: when, its type, its payload's first 32-bit words.
struct Notice {
    at: Instant,
    kind: u32,
    words: Vec<u32>,
}

/// Every window server notice heard (bounded), filled by the callbacks on the main thread.
static NOTICES: Mutex<Vec<Notice>> = Mutex::new(Vec::new());
const NOTICES_MAX: usize = 400_000;

thread_local! {
    /// Window-created notices heard through Accessibility: (when, window id).
    static AX_CREATED: RefCell<Vec<(Instant, u32)>> = const { RefCell::new(Vec::new()) };
}

fn note(kind: u32, data: *mut c_void, len: usize) {
    let n = (len / 4).min(8);
    let mut words = Vec::with_capacity(n);
    if !data.is_null() {
        for i in 0..n {
            // SAFETY: the window server hands `len` bytes at `data`; `i * 4 + 4 <= len`. Read
            // unaligned: the payload's alignment is not documented.
            words.push(unsafe { std::ptr::read_unaligned((data as *const u8).add(i * 4).cast::<u32>()) });
        }
    }
    if let Ok(mut v) = NOTICES.lock()
        && v.len() < NOTICES_MAX
    {
        v.push(Notice { at: Instant::now(), kind, words });
    }
}

extern "C" fn on_connection_notice(kind: u32, data: *mut c_void, len: usize, _ctx: *mut c_void, _cid: i32) {
    note(kind, data, len);
}

extern "C" fn on_notice(kind: u32, data: *mut c_void, len: usize, _ctx: *mut c_void) {
    note(kind | 0x8000_0000, data, len);
}

unsafe extern "C-unwind" fn on_ax(_: NonNull<AXObserver>, element: NonNull<AXUIElement>, _: NonNull<CFString>, _: *mut c_void) {
    // SAFETY: Accessibility hands a live element for the length of the callback.
    let e = unsafe { AxElement::from_borrowed(element) };
    let id = e.window_id_within(0.05).unwrap_or(0);
    AX_CREATED.with(|v| v.borrow_mut().push((Instant::now(), id)));
}

type ConnectionProc = extern "C" fn(u32, *mut c_void, usize, *mut c_void, i32);
type FnRequestForWindows = unsafe extern "C" fn(i32, *const u32, i32) -> i32;
type Proc = extern "C" fn(u32, *mut c_void, usize, *mut c_void);
type FnRegisterConnection = unsafe extern "C" fn(i32, ConnectionProc, u32, *mut c_void) -> i32;
type FnRegister = unsafe extern "C" fn(Proc, u32, *mut c_void) -> i32;

/// Registers both notice callbacks for every type in `range`; how many registrations took.
fn register(cid: i32, range: std::ops::RangeInclusive<u32>) -> (usize, usize) {
    // SAFETY: a system framework path; RTLD_NOW resolves everything at load.
    let h = unsafe { libc::dlopen(SKYLIGHT.as_ptr(), libc::RTLD_NOW) };
    if h.is_null() {
        return (0, 0);
    }
    let mut names = Vec::new();
    // SAFETY: the signatures as yabai's extern.h gives them (see the spike notes).
    let conn: Option<FnRegisterConnection> = unsafe { super::spaces::ffi::load(h, c"SLSRegisterConnectionNotifyProc", &mut names) };
    // SAFETY: as above.
    let plain: Option<FnRegister> = unsafe { super::spaces::ffi::load(h, c"SLSRegisterNotifyProc", &mut names) };
    let (mut a, mut b) = (0, 0);
    for kind in range {
        if let Some(f) = conn {
            // SAFETY: a callback of the stated type that lives as long as the process.
            a += usize::from(unsafe { f(cid, on_connection_notice, kind, std::ptr::null_mut()) } == 0);
        }
        if let Some(f) = plain {
            // SAFETY: as above.
            b += usize::from(unsafe { f(on_notice, kind, std::ptr::null_mut()) } == 0);
        }
    }
    (a, b)
}

/// `notices --ids ID,… [--events A-B] [--for MS]` (round sm-tick): registers both notice
/// callbacks for every type in the range (default 700-1700), asks the window server for the
/// notices of windows `ids` (`SLSRequestNotificationsForWindows(cid, ids, count)`, as yabai's
/// extern.h gives it; S3b registered without it and heard nothing at a launch), then turns the
/// main run loop for MS (default 30000) while the windows are closed, moved, minimized or
/// ordered out by hand, and prints every notice heard: when (ms from the request), its type
/// (`0x8000_0000` set: the plain registration's), its first words, and whether a word names one
/// of the windows. Moves nothing.
pub fn notices(args: &[String]) -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else { return ExitCode::from(4) };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    let Ok(sky) = Sky::open() else { return ExitCode::from(3) };
    let (mut ids, mut range, mut wait) = (Vec::<u32>::new(), 700u32..=1700u32, Duration::from_secs(30));
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--ids" => ids = it.next().map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default(),
            "--events" => {
                if let Some((x, y)) = it.next().and_then(|s| s.split_once('-')) {
                    range = x.parse().unwrap_or(700)..=y.parse().unwrap_or(1700);
                }
            }
            "--for" => wait = it.next().and_then(|s| s.parse().ok()).map_or(wait, Duration::from_millis),
            _ => {}
        }
    }
    if ids.is_empty() {
        println!("{}", json!({ "ok": false, "error": "usage: probe-spaces notices --ids ID,… [--events A-B] [--for MS]" }));
        return ExitCode::from(2);
    }
    let registered = register(sky.cid, range.clone());
    // SAFETY: a system framework path; RTLD_NOW resolves everything at load.
    let h = unsafe { libc::dlopen(SKYLIGHT.as_ptr(), libc::RTLD_NOW) };
    let mut names = Vec::new();
    // SAFETY: the signature as yabai's extern.h gives it: (cid, window ids, count) -> CGError.
    let request: Option<FnRequestForWindows> = if h.is_null() { None } else { unsafe { super::spaces::ffi::load(h, c"SLSRequestNotificationsForWindows", &mut names) } };
    let Ok(count) = i32::try_from(ids.len()) else { return ExitCode::from(2) };
    // SAFETY: `ids` holds `count` window ids and outlives the call.
    let asked = request.map(|f| unsafe { f(sky.cid, ids.as_ptr(), count) });
    let t0 = Instant::now();
    println!(
        "{}",
        json!({ "event": "asked", "ids": ids, "connection": registered.0, "plain": registered.1, "range": [range.start(), range.end()], "request": asked })
    );
    let mut printed = 0;
    while t0.elapsed() < wait {
        pump(Duration::from_millis(50));
        let heard: Vec<(Instant, u32, Vec<u32>)> =
            NOTICES.lock().map(|v| v[printed..].iter().map(|n| (n.at, n.kind, n.words.clone())).collect()).unwrap_or_default();
        printed += heard.len();
        for (at, kind, words) in heard {
            let names_window = words.iter().find(|w| ids.contains(w));
            println!("{}", json!({ "event": "notice", "ms": ms(t0, Some(at)), "type": kind, "words": words, "window": names_window }));
        }
    }
    println!("{}", json!({ "ok": true, "event": "done", "heard": printed }));
    ExitCode::SUCCESS
}

/// Lets the main run loop turn for up to `d` (notices and Accessibility callbacks land then).
fn pump(d: Duration) {
    // SAFETY: the framework's own constant.
    let mode = unsafe { kCFRunLoopDefaultMode };
    CFRunLoop::run_in_mode(mode, d.as_secs_f64(), false);
}

fn ms(t0: Instant, t: Option<Instant>) -> Value {
    t.map_or(Value::Null, |t| json!((t.saturating_duration_since(t0).as_secs_f64() * 1e4).round() / 10.0))
}

fn pids_of(bundle: &str) -> Vec<i32> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle)).iter().map(|a| a.processIdentifier()).collect()
}

/// One new window's stamps.
#[derive(Default)]
struct Seen {
    listed: Option<Instant>,
    listed_onscreen: bool,
    bounds: String,
    desktop: Option<Instant>,
    onscreen: Option<Instant>,
    ax_listed: Option<Instant>,
    /// Each change of the listed bounds while off screen: (when, bounds).
    shapes: Vec<(Instant, String)>,
    /// `--hold-ms`: when it went into the waiting Space, whether the read back said so,
    /// when its bounds first were a real size there (ordered in), when it was let out, and
    /// how many samples listed it on screen while held.
    held: Option<Instant>,
    held_ok: bool,
    ordered: Option<Instant>,
    released: Option<Instant>,
    on_while_held: u32,
}

/// Runs `sources` (see the module docs).
pub fn run(args: &[String]) -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else { return ExitCode::from(4) };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    // The window server's notices reach a process that has finished launching.
    app.finishLaunching();
    let Ok(sky) = Sky::open() else { return ExitCode::from(3) };
    let mut it = args.iter();
    let (Some(target), Some(bundle)) = (it.next().cloned(), it.next().cloned()) else {
        println!("{}", json!({ "ok": false, "error": "usage: probe-spaces sources APP-PATH BUNDLE [--rounds N] [--events A-B] [--args …]" }));
        return ExitCode::from(2);
    };
    let (mut rounds, mut range, mut extra, mut hold, mut reverse) = (5usize, 700u32..=1700u32, Vec::new(), None, false);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--rounds" => rounds = it.next().and_then(|s| s.parse().ok()).unwrap_or(5),
            "--events" => {
                if let Some((x, y)) = it.next().and_then(|s| s.split_once('-')) {
                    range = x.parse().unwrap_or(700)..=y.parse().unwrap_or(1700);
                }
            }
            "--hold-ms" => hold = it.next().and_then(|s| s.parse().ok()).map(Duration::from_millis),
            "--release-reverse" => reverse = true,
            "--args" => extra.extend(it.by_ref().cloned()),
            _ => {}
        }
    }
    if hold.is_some() && !bundle.starts_with(super::park_probe::FIXTURE_PREFIX) {
        println!("{}", json!({ "ok": false, "error": "--hold-ms moves windows: a fixture app's only" }));
        return ExitCode::from(2);
    }
    let registered = register(sky.cid, range.clone());
    println!("{}", json!({ "event": "registered", "connection": registered.0, "plain": registered.1, "range": [range.start(), range.end()] }));
    let mut out = Vec::new();
    for round in 0..rounds {
        let r = one_round(&sky, &target, &bundle, &extra, hold, reverse);
        println!("{}", json!({ "event": "round", "round": round, "result": r }));
        out.push(r);
        pump(Duration::from_millis(800));
    }
    println!("{}", json!({ "ok": true, "event": "done", "rounds": out.len() }));
    ExitCode::SUCCESS
}

fn one_round(sky: &Sky, target: &str, bundle: &str, extra: &[String], hold: Option<Duration>, reverse: bool) -> Value {
    // A classic Space (it ends with this process) where `--hold-ms` keeps new windows.
    let waiting = match hold {
        Some(_) => match sky.create(Route::Classic, false) {
            Ok(sid) if sid != 0 => Some(sid),
            _ => return json!({ "error": "no Space made" }),
        },
        None => None,
    };
    let before_pids: HashSet<i32> = pids_of(bundle).into_iter().collect();
    let before: HashSet<u32> = windows::every_window().iter().map(|w| w.id).collect();
    if let Ok(mut v) = NOTICES.lock() {
        v.clear();
    }
    AX_CREATED.with(|v| v.borrow_mut().clear());
    let (desktop_space, _) = sky.current();
    let mut cmd = std::process::Command::new("open");
    cmd.arg("-n");
    if let Some(name) = target.strip_prefix("-a ") {
        cmd.args(["-a", name]);
    } else {
        cmd.arg(target);
    }
    if !extra.is_empty() {
        cmd.arg("--args").args(extra);
    }
    let t0 = Instant::now();
    let Ok(mut child) = cmd.spawn() else { return json!({ "error": "open failed" }) };
    let mut pid: Option<i32> = None;
    let mut launch: Option<Instant> = None;
    let mut observer: Option<AxObserver> = None;
    let mut ax_answer: Option<Instant> = None;
    let mut ax_tries = 0u32;
    let mut last_ax = t0 - Duration::from_secs(1);
    let mut last_pids = t0 - Duration::from_secs(1);
    let mut seen: HashMap<u32, Seen> = HashMap::new();
    let mut first_onscreen: Option<Instant> = None;
    let mut samples = 0u64;
    // `--hold-ms`: the held windows ordered in, in the complete list's order, at the last
    // sample before any was let out; and the order they were let out in.
    let mut held_order: Vec<u32> = Vec::new();
    let mut release_order: Vec<u32> = Vec::new();
    let mut list_us = 0u128;
    let mut ids_us = 0u128;
    let limit = t0 + Duration::from_secs(10);
    loop {
        let now = Instant::now();
        if now > limit || first_onscreen.is_some_and(|t| now > t + Duration::from_millis(600)) {
            break;
        }
        if hold.is_some() && now > t0 + Duration::from_millis(4500) {
            break;
        }
        let _ = child.try_wait();
        if pid.is_none() && now.duration_since(last_pids) >= Duration::from_millis(1) {
            last_pids = now;
            if let Some(p) = pids_of(bundle).into_iter().find(|p| !before_pids.contains(p)) {
                pid = Some(p);
                launch = Some(Instant::now());
            }
        }
        if let Some(p) = pid
            && now.duration_since(last_ax) >= Duration::from_millis(2)
        {
            last_ax = now;
            let app = AxElement::application(p);
            let _ = app.set_messaging_timeout(0.02);
            if ax_answer.is_none() {
                ax_tries += 1;
                if observer.is_none() {
                    observer = AxObserver::new(p, on_ax).ok();
                }
                if let Some(o) = &observer
                    && o.add(&app, notifications::WINDOW_CREATED, 1).is_ok()
                {
                    ax_answer = Some(Instant::now());
                }
            }
            if ax_answer.is_some()
                && (seen.is_empty() || seen.values().any(|s| s.ax_listed.is_none()))
                && let Ok(list) = app.windows()
            {
                let at = Instant::now();
                for w in list {
                    if let Ok(id) = w.window_id_within(0.02)
                        && !before.contains(&id)
                    {
                        seen.entry(id).or_default().ax_listed.get_or_insert(at);
                    }
                }
            }
        }
        if let Some(p) = pid {
            let q0 = Instant::now();
            let _ = windows::every_window_id();
            ids_us += q0.elapsed().as_micros();
            let q = Instant::now();
            let all = windows::every_window();
            let at = Instant::now();
            list_us += at.duration_since(q).as_micros();
            samples += 1;
            let mut due: Vec<u32> = Vec::new();
            if release_order.is_empty() {
                let order: Vec<u32> = all
                    .iter()
                    .filter(|w| seen.get(&w.id).is_some_and(|s| s.held.is_some() && s.ordered.is_some() && s.released.is_none()))
                    .map(|w| w.id)
                    .collect();
                if !order.is_empty() {
                    held_order = order;
                }
            }
            for w in all.iter().filter(|w| w.pid == p && w.layer == 0 && !before.contains(&w.id)) {
                let s = seen.entry(w.id).or_default();
                if s.listed.is_none() {
                    s.listed = Some(at);
                    s.listed_onscreen = w.onscreen;
                    s.bounds = format!("{}", w.bounds);
                }
                let shape = format!("{}", w.bounds);
                if s.onscreen.is_none() && s.shapes.last().is_none_or(|(_, b)| *b != shape) {
                    s.shapes.push((at, shape));
                }
                if w.onscreen && s.onscreen.is_none() {
                    s.onscreen = Some(at);
                    first_onscreen.get_or_insert(at);
                }
                if let (Some(sid), Some(h)) = (waiting, hold) {
                    let real = w.bounds.w > 0.0 && w.bounds.h > 0.0;
                    if s.held.is_none() && !w.onscreen && sky.spaces_of(&[w.id]).is_empty() {
                        let _ = sky.add_remove_from(Route::Bridged, sid, &[w.id], ALL_SPACES_MASK);
                        s.held = Some(Instant::now());
                        s.held_ok = sky.spaces_of(&[w.id]) == [sid];
                    }
                    if s.held.is_some() && s.released.is_none() {
                        if w.onscreen {
                            s.on_while_held += 1;
                        }
                        if real && s.ordered.is_none() {
                            s.ordered = Some(at);
                        }
                        if s.ordered.is_some_and(|o| at >= o + h) || at > t0 + Duration::from_secs(4) {
                            due.push(w.id);
                        }
                    }
                }
                if s.desktop.is_none() && sky.spaces_of(&[w.id]).contains(&desktop_space) {
                    s.desktop = Some(Instant::now());
                }
            }
            // Let out together with every other one ordered in, one by one, in the list's
            // order (front first) or, `--release-reverse`, back first: does the order they
            // come out in change their order on screen?
            if !due.is_empty() {
                due = all
                    .iter()
                    .filter(|w| due.contains(&w.id) || seen.get(&w.id).is_some_and(|s| s.held.is_some() && s.ordered.is_some() && s.released.is_none()))
                    .map(|w| w.id)
                    .collect();
            }
            if reverse {
                due.reverse();
            }
            for id in due {
                let _ = sky.add_remove_from(Route::Bridged, desktop_space, &[id], ALL_SPACES_MASK);
                if let Some(s) = seen.get_mut(&id) {
                    s.released = Some(Instant::now());
                }
                release_order.push(id);
            }
        }
        pump(Duration::from_micros(250));
    }
    // The let-out windows' order on screen now, front to back.
    let out_order: Vec<u32> = windows::on_screen().iter().filter(|w| release_order.contains(&w.id)).map(|w| w.id).collect();
    // Windows still held (never ordered in: helpers) let out; do they show then?
    let mut helpers_shown = 0;
    if let Some(sid) = waiting {
        let left: Vec<u32> = seen.iter().filter(|(_, s)| s.held.is_some() && s.released.is_none()).map(|(id, _)| *id).collect();
        if !left.is_empty() {
            let _ = sky.add_remove_from(Route::Bridged, desktop_space, &left, ALL_SPACES_MASK);
            for id in &left {
                if let Some(s) = seen.get_mut(id) {
                    s.released = Some(Instant::now());
                }
            }
            let end = Instant::now() + Duration::from_millis(300);
            while Instant::now() < end {
                helpers_shown = helpers_shown.max(windows::describe(&left).iter().filter(|w| w.onscreen).count());
                pump(Duration::from_millis(2));
            }
        }
        let _ = sky.destroy(Route::Classic, sid);
    }
    let ax_created: Vec<(Instant, u32)> = AX_CREATED.with(|v| v.borrow().clone());
    let notices = NOTICES.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
    // Notice types per count (every notice heard in the round, for the record).
    let mut kinds: HashMap<u32, usize> = HashMap::new();
    for n in &notices {
        *kinds.entry(n.kind).or_default() += 1;
    }
    let mut kinds: Vec<(u32, usize)> = kinds.into_iter().collect();
    kinds.sort();
    let wins: Vec<Value> = seen
        .iter()
        .filter(|(_, s)| s.listed.is_some())
        .map(|(id, s)| {
            let cgs: Vec<Value> = notices
                .iter()
                .filter(|n| n.words.contains(id))
                .take(24)
                .map(|n| json!([n.kind & 0x7fff_ffff, n.kind & 0x8000_0000 != 0, ms(t0, Some(n.at)), n.words]))
                .collect();
            let created = ax_created.iter().find(|(_, w)| w == id).map(|(t, _)| *t);
            json!({
                "id": id, "bounds": s.bounds, "listed": ms(t0, s.listed), "listed_onscreen": s.listed_onscreen,
                "desktop": ms(t0, s.desktop), "onscreen": ms(t0, s.onscreen), "ax_listed": ms(t0, s.ax_listed),
                "ax_created": ms(t0, created), "cgs": cgs,
                "shapes": s.shapes.iter().map(|(t, b)| json!([ms(t0, Some(*t)), b])).collect::<Vec<_>>(),
                "held": ms(t0, s.held), "held_ok": s.held_ok, "ordered": ms(t0, s.ordered), "released": ms(t0, s.released), "on_while_held": s.on_while_held,
            })
        })
        .collect();
    // Quit the app: terminate, then force.
    if let Some(p) = pid
        && let Some(a) = NSRunningApplication::runningApplicationWithProcessIdentifier(p)
    {
        a.terminate();
        let end = Instant::now() + Duration::from_secs(3);
        while !a.isTerminated() && Instant::now() < end {
            pump(Duration::from_millis(20));
        }
        if !a.isTerminated() {
            a.forceTerminate();
        }
    }
    let _ = child.wait();
    drop(observer);
    json!({
        "pid": pid, "launch": ms(t0, launch), "ax_answer": ms(t0, ax_answer), "ax_tries": ax_tries,
        "held_order": held_order, "release_order": release_order, "out_order": out_order,
        "first_onscreen": ms(t0, first_onscreen), "samples": samples, "list_us_mean": list_us / u128::from(samples.max(1)), "ids_us_mean": ids_us / u128::from(samples.max(1)), "helpers_shown": helpers_shown, "windows": wins,
        "ax_created_all": ax_created.iter().map(|(t, w)| json!([ms(t0, Some(*t)), w])).collect::<Vec<_>>(),
        "notice_kinds": kinds.iter().map(|(k, n)| json!([k & 0x7fff_ffff, k & 0x8000_0000 != 0, n])).collect::<Vec<_>>(),
    })
}

/// `probe-spaces overlap OLD-BUNDLE APP-PATH NEW-BUNDLE [--for MS] [--args …]`: the live
/// check of round sm-placenew. Opens the app (`open -n`, activated, as a person opens one)
/// while OLD-BUNDLE's windows show, and reads the on-screen window list every millisecond
/// for MS (default 2500): a sample with one of OLD-BUNDLE's windows (those on screen at the
/// start, listed on screen at the frame they had then) and a window of the new app (layer 0,
/// taller than 50 pt) both on screen is an overlap. Reads only; leaves the app running. One JSON line.
pub fn overlap(args: &[String]) -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else { return ExitCode::from(4) };
    let _ = NSApplication::sharedApplication(mtm);
    let mut it = args.iter();
    let (Some(old_bundle), Some(target), Some(bundle)) = (it.next().cloned(), it.next().cloned(), it.next().cloned()) else {
        println!("{}", json!({ "ok": false, "error": "usage: probe-spaces overlap OLD-BUNDLE APP-PATH NEW-BUNDLE [--for MS] [--args …]" }));
        return ExitCode::from(2);
    };
    let (mut span, mut extra) = (Duration::from_millis(2500), Vec::new());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--for" => span = it.next().and_then(|s| s.parse().ok()).map_or(span, Duration::from_millis),
            "--args" => extra.extend(it.by_ref().cloned()),
            _ => {}
        }
    }
    let old_pids = pids_of(&old_bundle);
    let before_pids: HashSet<i32> = pids_of(&bundle).into_iter().collect();
    // The old app's windows where they show now: one still listed on screen at that frame
    // shows (the Corner engine's hidden window is listed on screen too, in the park corner).
    let old: HashMap<u32, crate::engine::geometry::Rect> = windows::on_screen()
        .iter()
        .filter(|w| old_pids.contains(&w.pid) && w.layer == 0 && w.bounds.h > 50.0)
        .map(|w| (w.id, w.bounds))
        .collect();
    let at_frame = |a: crate::engine::geometry::Rect, b: crate::engine::geometry::Rect| {
        (a.x - b.x).abs() <= 2.0 && (a.y - b.y).abs() <= 2.0 && (a.w - b.w).abs() <= 2.0 && (a.h - b.h).abs() <= 2.0
    };
    if old.is_empty() {
        println!("{}", json!({ "ok": false, "error": "no window of the old app on screen" }));
        return ExitCode::from(1);
    }
    let mut cmd = std::process::Command::new("open");
    cmd.arg("-n").arg(&target);
    if !extra.is_empty() {
        cmd.arg("--args").args(&extra);
    }
    let t0 = Instant::now();
    let Ok(mut child) = cmd.spawn() else {
        println!("{}", json!({ "ok": false, "error": "open failed" }));
        return ExitCode::from(1);
    };
    let mut pid: Option<i32> = None;
    let mut last_pids = t0 - Duration::from_secs(1);
    let (mut samples, mut overlaps, mut max_gap) = (0u64, 0u64, Duration::ZERO);
    let (mut first_new, mut last_old, mut first_overlap, mut last_overlap): (Option<Instant>, Option<Instant>, Option<Instant>, Option<Instant>) =
        (None, None, None, None);
    let mut new_ids: Vec<u32> = Vec::new();
    let mut prev = t0;
    let mut due = t0;
    while t0.elapsed() < span {
        let _ = child.try_wait();
        if pid.is_none() && last_pids.elapsed() >= Duration::from_millis(5) {
            last_pids = Instant::now();
            pid = pids_of(&bundle).into_iter().find(|p| !before_pids.contains(p));
        }
        let list = windows::on_screen();
        let at = Instant::now();
        samples += 1;
        max_gap = max_gap.max(at.duration_since(prev));
        prev = at;
        let old_on = list.iter().any(|w| old.get(&w.id).is_some_and(|f| at_frame(*f, w.bounds)));
        let new_on: Vec<u32> =
            list.iter().filter(|w| pid.is_some_and(|p| w.pid == p) && w.layer == 0 && w.bounds.h > 50.0).map(|w| w.id).collect();
        if old_on {
            last_old = Some(at);
        }
        if !new_on.is_empty() {
            first_new.get_or_insert(at);
            for id in new_on.iter() {
                if !new_ids.contains(id) {
                    new_ids.push(*id);
                }
            }
            if old_on {
                overlaps += 1;
                first_overlap.get_or_insert(at);
                last_overlap = Some(at);
            }
        }
        due += Duration::from_millis(1);
        let now = Instant::now();
        if due > now {
            // The run loop turns meanwhile: the running apps' list is updated on it.
            pump(due - now);
        } else {
            due = now;
        }
    }
    let _ = child.wait();
    let span_ms = t0.elapsed().as_secs_f64() * 1e3;
    println!(
        "{}",
        json!({
            "ok": true, "pid": pid, "old": old.len(), "samples": samples, "span_ms": span_ms.round(),
            "mean_interval_ms": ((span_ms / samples.max(1) as f64) * 1000.0).round() / 1000.0,
            "max_gap_ms": (max_gap.as_secs_f64() * 1e4).round() / 10.0,
            "overlaps": overlaps, "first_overlap_ms": ms(t0, first_overlap), "last_overlap_ms": ms(t0, last_overlap),
            "overlap_ms": match (first_overlap, last_overlap) { (Some(a), Some(b)) => json!((b.duration_since(a).as_secs_f64() * 1e4).round() / 10.0), _ => Value::Null },
            "first_new_ms": ms(t0, first_new), "last_old_ms": ms(t0, last_old), "new_windows": new_ids,
        })
    );
    ExitCode::SUCCESS
}
