//! `ctl debug-cmd-tab` (test only, round cmd-tab): a posted ⌘Tab, and what answered it.
//!
//! Two questions for a run on the owner's Mac, which code alone cannot settle:
//!
//! 1. Does the Dock show the switcher's selection to Accessibility, and how soon after the
//!    Tab? Each sample reads it as the controller does ([`switcher_ax::read`]); the report
//!    also holds two dumps of the Dock's children ([`switcher_ax::dump`]): before the ⌘ goes
//!    down, and with the switcher up.
//! 2. What reaches the input monitor while the switcher shows (⌘, each Tab, Escape, ⌘'s
//!    release), and how long macOS takes from ⌘'s release to a Space change: the controller
//!    notes what it heard and did into the report ([`note`]) with the time since the probe
//!    began. For comparison, an event tap at the session level made for the probe's length
//!    only (active, so Accessibility suffices; every event passed on unchanged, nothing
//!    swallowed) notes the key events it sees (`tap`): if the global monitor misses the
//!    switcher's keys, the report tells whether a tap would see them.
//!
//! The keys go out on a thread of their own at the HID tap, as a person's do (the WM thread
//! is free to hear them): ⌘ down; Tab `tabs` times; held `hold`; then, for a cancel, Escape
//! before ⌘'s release, so nothing is activated; for a release, ⌘ let go, the app selected
//! activated. A release goes out only when the app selected is a `STAGE_MANAGER_ONLY` app:
//! the Dock's answer at the last sample, else the controller's prediction made before the
//! keys went out (`expected_test`); otherwise Escape goes first. The report stays readable
//! (`ctl debug-cmd-tab result`) until the next probe.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy,
    CGEventType,
};

use serde_json::{Value, json};

use super::space_keys::{COMMAND, COMMAND_KEY, ESCAPE_KEY, post_key};
use super::switcher_ax;
use crate::engine::switcher::KEY_TAB;

/// What a probe posts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    /// ⌘ let go on the selection (an activation); else Escape first (none).
    pub release: bool,
    /// How many Tabs.
    pub tabs: u8,
    /// From the last Tab's release to Escape or ⌘'s release.
    pub hold: Duration,
    /// The app the controller predicts for `tabs` Tabs is a `STAGE_MANAGER_ONLY` app.
    pub expected_test: bool,
}

/// The longest hold a probe takes.
pub const HOLD_MAX: Duration = Duration::from_millis(5000);

/// After its last key the probe waits this long for the activation and the Space change
/// before it says it is done.
const SETTLE: Duration = Duration::from_millis(2500);

/// When the Dock is read after the last Tab (ms), within the hold.
const SAMPLES_MS: [u64; 7] = [15, 40, 80, 150, 250, 400, 700];

#[derive(Debug)]
struct Report {
    started: Instant,
    request: Request,
    events: Vec<Value>,
    samples: Vec<Value>,
    dump_before: Value,
    dump_up: Option<Value>,
    released_ms: Option<u64>,
    outcome: Option<String>,
    done: bool,
}

static REPORT: Mutex<Option<Report>> = Mutex::new(None);

fn ms_since(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

/// True while a probe's keys are going out (its ⌘ is posted, not a person's).
pub fn posting() -> bool {
    REPORT.lock().ok().is_some_and(|r| r.as_ref().is_some_and(|r| !r.done))
}

/// Notes what the controller heard or did (`what`, of `kind`) into the running probe's
/// report, with the time since it began; nothing when no probe runs.
pub fn note(kind: &str, what: impl Into<String>) {
    let Ok(mut guard) = REPORT.lock() else { return };
    let Some(r) = guard.as_mut().filter(|r| !r.done || r.started.elapsed() < SETTLE * 2) else { return };
    r.events.push(json!({ "ms": ms_since(r.started), "kind": kind, "what": what.into() }));
}

/// The last probe's report.
pub fn result() -> Value {
    let Ok(guard) = REPORT.lock() else { return json!({ "ok": false, "error": "the report is not readable" }) };
    let Some(r) = guard.as_ref() else { return json!({ "ok": false, "error": "no probe ran" }) };
    json!({
        "ok": true,
        "done": r.done,
        "release": r.request.release,
        "tabs": r.request.tabs,
        "hold_ms": r.request.hold.as_millis() as u64,
        "released_ms": r.released_ms,
        "outcome": r.outcome,
        "events": r.events,
        "samples": r.samples,
        "dump_before": r.dump_before,
        "dump_up": r.dump_up,
    })
}

fn with_report(f: impl FnOnce(&mut Report)) {
    if let Ok(mut guard) = REPORT.lock()
        && let Some(r) = guard.as_mut()
    {
        f(r);
    }
}

/// Starts a probe (`dock`: the Dock's pid; `allowed`: whether a pid is a
/// `STAGE_MANAGER_ONLY` app). Refused while another probe's keys are going out.
pub fn start(req: Request, dock: i32, allowed: impl Fn(i32) -> bool + Send + 'static) -> Result<(), String> {
    if posting() {
        return Err("a probe is still running".into());
    }
    let req = Request { hold: req.hold.min(HOLD_MAX), tabs: req.tabs.clamp(1, 9), ..req };
    std::thread::Builder::new()
        .name("test-cmd-tab".into())
        .spawn(move || run(req, dock, allowed))
        .map(|_| ())
        .map_err(|e| format!("no thread for the probe: {e}"))
}

/// The probe's own event tap, for its length (`length`): every key down and up and every
/// modifier change it sees is noted (`tap`); every event goes on unchanged.
fn watch_with_tap(length: Duration) {
    let types = [CGEventType::KeyDown, CGEventType::KeyUp, CGEventType::FlagsChanged];
    let events: CGEventMask = types.iter().fold(0, |m, t| m | (1u64 << t.0));
    // SAFETY: valid enum values; the callback has the documented signature and uses no user
    // info.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            events,
            Some(tap_callback),
            std::ptr::null_mut(),
        )
    };
    let Some(port) = port else {
        note("tap", "macOS refused the probe's event tap");
        return;
    };
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else { return };
    let Some(rl) = CFRunLoop::current() else { return };
    // SAFETY: the framework's own constant.
    let mode = unsafe { kCFRunLoopDefaultMode };
    rl.add_source(Some(&source), mode);
    CGEvent::tap_enable(&port, true);
    CFRunLoop::run_in_mode(mode, length.as_secs_f64(), false);
    CGEvent::tap_enable(&port, false);
    CFMachPort::invalidate(&port);
}

unsafe extern "C-unwind" fn tap_callback(_proxy: CGEventTapProxy, kind: CGEventType, event: NonNull<CGEvent>, _info: *mut c_void) -> *mut CGEvent {
    if matches!(kind, CGEventType::KeyDown | CGEventType::KeyUp | CGEventType::FlagsChanged) {
        // SAFETY: the event is live for the callback's duration.
        let ev = unsafe { event.as_ref() };
        let code = CGEvent::integer_value_field(Some(ev), CGEventField::KeyboardEventKeycode);
        let cmd = CGEvent::flags(Some(ev)).contains(CGEventFlags::MaskCommand);
        let what = match kind {
            CGEventType::KeyDown => "down",
            CGEventType::KeyUp => "up",
            _ => "modifiers",
        };
        note("tap", format!("{what} key={code} cmd={cmd}"));
    }
    event.as_ptr()
}

fn run(req: Request, dock: i32, allowed: impl Fn(i32) -> bool) {
    // The tap first: it is on before the ⌘ goes down (the dump below takes longer than it).
    let length = Duration::from_millis(30 + 60 * u64::from(req.tabs) + 300) + req.hold + Duration::from_millis(800);
    let _ = std::thread::Builder::new().name("test-cmd-tab-tap".into()).spawn(move || watch_with_tap(length));
    let dump_before = switcher_ax::dump(dock, 3);
    std::thread::sleep(Duration::from_millis(30));
    if let Ok(mut guard) = REPORT.lock() {
        *guard = Some(Report {
            started: Instant::now(),
            request: req,
            events: Vec::new(),
            samples: Vec::new(),
            dump_before,
            dump_up: None,
            released_ms: None,
            outcome: None,
            done: false,
        });
    }
    let posted = |what: &str| note("posted", what);
    let last_pid = std::cell::Cell::new(None);
    let sample = |label: String| {
        let t = Instant::now();
        let read = switcher_ax::read(dock);
        let took = t.elapsed().as_micros() as u64;
        if let switcher_ax::Read::Shown { .. } = read {
            last_pid.set(read.pid());
        }
        let (items, index, matched) = match &read {
            switcher_ax::Read::Shown { items, selected } => {
                (Some(*items), selected.as_ref().and_then(|s| s.index), selected.as_ref().map(|s| s.matched))
            }
            _ => (None, None, None),
        };
        let shown = matches!(read, switcher_ax::Read::Shown { .. });
        with_report(|r| {
            r.samples.push(json!({
                "label": label, "ms": ms_since(r.started), "read": read.name(), "items": items, "index": index,
                "pid": read.pid(), "matched": matched, "took_us": took,
            }));
        });
        shown
    };
    let key = |code: u16, flags: u64| {
        let r = post_key(code, true, flags).and_then(|()| {
            std::thread::sleep(Duration::from_millis(10));
            post_key(code, false, flags)
        });
        if let Err(e) = r {
            tracing::warn!("test cmd-tab: {e}");
        }
    };
    let _ = post_key(COMMAND_KEY, true, COMMAND);
    posted("⌘ down");
    std::thread::sleep(Duration::from_millis(30));
    for i in 1..=req.tabs {
        key(KEY_TAB, COMMAND);
        posted(&format!("Tab {i}"));
        sample(format!("Tab {i}"));
        if i < req.tabs {
            std::thread::sleep(Duration::from_millis(40));
        }
    }
    let last_tab = Instant::now();
    let mut dumped = false;
    for off in SAMPLES_MS {
        let at = Duration::from_millis(off);
        if at >= req.hold {
            break;
        }
        std::thread::sleep(at.saturating_sub(last_tab.elapsed()));
        let shown = sample(format!("+{off} ms"));
        if !dumped && (shown || off >= 150) {
            dumped = true;
            let d = switcher_ax::dump(dock, 12);
            with_report(|r| r.dump_up = Some(d));
        }
    }
    std::thread::sleep(req.hold.saturating_sub(last_tab.elapsed()));
    let last_pid = last_pid.get();
    let safe = match last_pid {
        Some(pid) => allowed(pid),
        None => req.expected_test,
    };
    if !req.release || !safe {
        key(ESCAPE_KEY, COMMAND);
        posted("Escape");
        std::thread::sleep(Duration::from_millis(80));
        sample("after Escape".into());
        if req.release {
            with_report(|r| r.outcome = Some(format!("cancelled: the selection (pid {last_pid:?}) is not a test app")));
        }
    }
    let _ = post_key(COMMAND_KEY, false, 0);
    posted("⌘ up");
    with_report(|r| {
        if req.release && safe {
            r.released_ms = Some(ms_since(r.started));
            r.outcome.get_or_insert_with(|| "released".into());
        } else {
            r.outcome.get_or_insert_with(|| "cancelled".into());
        }
    });
    std::thread::sleep(SETTLE);
    with_report(|r| r.done = true);
}
