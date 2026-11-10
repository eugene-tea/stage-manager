//! The event tap that catches a minimize before macOS's genie (`[windows]
//! minimize_to_strip`; what it decides: [`crate::engine::minimize_press`]).
//!
//! Apple's Stage Manager plays no genie into the Dock: the window flies straight into a new
//! card at the top of the strip (recorded 11-09). Stage Manager hears of macOS's own
//! minimize only once the genie is over (`AXWindowMiniaturized`), so the two ways a person
//! minimizes most are caught here first:
//!
//! - **The yellow button.** A left press inside the top-left corner of a window the WM thread
//!   published ([`publish`]: managed, shown, standard) is hit-tested through Accessibility at
//!   the press ([`probe_button`]: the system-wide element at a short timeout, then one
//!   message for the element's subrole, enabled state, window and frame). On a managed
//!   window's enabled minimize button the press is swallowed, with its drags and its
//!   release; a release on the button sends the window to the strip ([`MinimizeAsk`] to the
//!   WM thread), one elsewhere does nothing, as the button's own tracking.
//! - **⌘M** (no other modifier): the frontmost app's focused window is asked for
//!   ([`probe_focused`]); a managed one with an enabled minimize button and a settable
//!   `AXMinimized` has its ⌘M swallowed and goes to the strip. Every other ⌘M is the app's.
//!
//! An active (filtering) `CGEventTap` at the session level, head-inserted, on a thread of its
//! own with its own run loop, as the inert-slivers tap ([`super::sliver_tap`]). A press away
//! from every published corner (nearly all) passes after a few rectangle tests; only a press
//! in a corner, or ⌘M, asks Accessibility, each call at [`AX_SECONDS`]: an app that does not
//! answer in time gets its event, and macOS's minimize (the genie, then the strip as before)
//! is what happens. What the callback decided is noted in memory and logged by the WM thread
//! at its next tick ([`refresh`]). A tap switched off by a timeout is switched on again at
//! once; one switched off for user input (secure input) stays off until secure input ends.
//!
//! Not caught: Window ▸ Minimize from the menu bar, a double-click on the title bar set to
//! minimize, an app's own minimize, and ⌥⌘M / ⌥-click (every window of the app).

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopCommonModes};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};

use super::ax::{AxElement, names};
use super::wm_thread::{WmCommand, WmHandle};
use crate::engine::geometry::Point;
use crate::engine::minimize_press::{
    ButtonGate, ButtonHit, Focused, Gate, KeyGate, KeyPress, Minimizable, Pass, Pressed, is_cmd_m, judge_key, judge_press,
    plain_click,
};

/// The Accessibility budget of each call the callback makes (seconds): an app this slow to
/// answer gets its event and macOS's minimize plays.
pub const AX_SECONDS: f32 = 0.1;

/// How a minimize was caught.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// The yellow button.
    Button,
    /// ⌘M.
    Key,
}

impl By {
    /// For the log.
    pub fn name(self) -> &'static str {
        match self {
            By::Button => "the minimize button",
            By::Key => "Cmd-M",
        }
    }
}

/// A minimize the tap swallowed, for the WM thread: the window goes to the strip, or, when
/// Stage Manager cannot take it now, is minimized as macOS would ([`Self::as_macos`]): the
/// person's minimize is never lost.
#[derive(Debug, Clone)]
pub struct MinimizeAsk {
    /// The window (`CGWindowID`).
    pub window: u32,
    /// Its Accessibility element, as the tap read it.
    pub element: AxElement,
    /// Which way.
    pub by: By,
    /// When the tap swallowed it.
    pub at: Instant,
}

impl MinimizeAsk {
    /// Minimizes the window as macOS would have (`AXMinimized`): its genie into the Dock,
    /// then, with `[windows] minimize_to_strip`, back to the strip as any minimize Stage
    /// Manager hears of after it.
    pub fn as_macos(&self, why: &str) {
        let r = self.element.bounded(super::wm_thread::AX_TIMEOUT_SECONDS, |e| e.set_minimized(true));
        match r {
            Ok(()) => tracing::info!(id = self.window, by = self.by.name(), why, "a minimize caught before macOS's genie goes to macOS after all"),
            Err(e) => tracing::warn!(id = self.window, by = self.by.name(), why, "cannot minimize the window as macOS would: {e}"),
        }
    }
}

static SET: Mutex<Option<Minimizable>> = Mutex::new(None);
/// The WM thread arranges now (not standing down for Apple's Stage Manager, no logout
/// coming, Accessibility on).
static ADMITTED: AtomicBool = AtomicBool::new(false);
static WM: OnceLock<WmHandle> = OnceLock::new();
/// Only the tap thread uses it.
static GATE: Mutex<ButtonGate<AxElement>> = Mutex::new(ButtonGate::new());
static PORT: AtomicPtr<CFMachPort> = AtomicPtr::new(std::ptr::null_mut());
static STARTED: AtomicBool = AtomicBool::new(false);
/// macOS switched the tap off for user input: it stays off until secure input ends.
static OFF_BY_INPUT: AtomicBool = AtomicBool::new(false);
/// When the WM tick last switched the tap on after user input.
static BACK_ON_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// What the callback decided, for the WM thread to log ([`refresh`]); bounded.
static NOTES: Mutex<Vec<Note>> = Mutex::new(Vec::new());
const NOTES_MAX: usize = 64;

/// The tap holds a left press it swallowed (on a minimize button), its release not come yet:
/// the session's button state never saw that press ([`holding_press`]).
static HOLDING: AtomicBool = AtomicBool::new(false);

/// Whether the tap holds a left press it swallowed, its release not come yet. The session's
/// button state reads such a press as up (the window server never got it); a synthetic drag's
/// guard against the user's own release counts the button down while this holds
/// ([`super::input::drag`]). A real release ends it, swallowed or not.
pub fn holding_press() -> bool {
    HOLDING.load(Ordering::Relaxed)
}

/// The WM thread's handle: swallowed minimizes go there.
pub fn set_handle(h: WmHandle) {
    let _ = WM.set(h);
}

/// Whether the WM thread arranges now ([`ADMITTED`]); the tap passes everything while not.
/// Starts the tap once windows are published and arranged.
pub fn admit(on: bool) {
    ADMITTED.store(on, Ordering::Relaxed);
    if on && SET.lock().is_ok_and(|s| s.as_ref().is_some_and(|s| s.on)) {
        start();
    }
}

/// Hands the tap the windows a minimize is caught on. Cheap; call after every change. Starts
/// the tap the first time it is on while the WM thread arranges ([`admit`]).
pub fn publish(set: Minimizable) {
    let on = set.on;
    if let Ok(mut s) = SET.lock()
        && s.as_ref() != Some(&set)
    {
        *s = Some(set);
    }
    if on && ADMITTED.load(Ordering::Relaxed) {
        start();
    }
}

/// macOS refused the tap once (noted once a run; tried again at each start).
static REFUSED_NOTED: AtomicBool = AtomicBool::new(false);
/// When the tap was last tried: a refused tap is tried again at most every [`RETRY_EVERY`].
static TRIED_AT: Mutex<Option<Instant>> = Mutex::new(None);
const RETRY_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// Starts the tap thread once (no-op afterwards). A tap that cannot be made (no
/// Accessibility yet) is tried again at the next start.
fn start() {
    if STARTED.load(Ordering::SeqCst) {
        return;
    }
    let now = Instant::now();
    if let Ok(mut t) = TRIED_AT.lock() {
        if t.is_some_and(|t| now.saturating_duration_since(t) < RETRY_EVERY) {
            return;
        }
        *t = Some(now);
    }
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("minimize-tap".into()).spawn(|| {
        if !run() {
            STARTED.store(false, Ordering::SeqCst);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("minimize: cannot start the tap thread: {e}");
        STARTED.store(false, Ordering::SeqCst);
    }
}

fn mask(types: &[CGEventType]) -> CGEventMask {
    types.iter().fold(0, |m, t| m | (1u64 << t.0))
}

/// Makes the tap and runs its run loop; false when the tap could not be made.
fn run() -> bool {
    let events = mask(&[CGEventType::LeftMouseDown, CGEventType::LeftMouseDragged, CGEventType::LeftMouseUp, CGEventType::KeyDown]);
    // SAFETY: valid enum values; the callback has the documented signature and uses no user
    // info.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            events,
            Some(callback),
            std::ptr::null_mut(),
        )
    };
    let Some(port) = port else {
        if !REFUSED_NOTED.swap(true, Ordering::Relaxed) {
            tracing::warn!("minimize: macOS refused the event tap (Accessibility?); a minimize plays macOS's genie first (tried again every 5 s)");
        }
        return false;
    };
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
        tracing::warn!("minimize: no run loop source for the tap");
        return false;
    };
    let Some(rl) = CFRunLoop::current() else { return false };
    // SAFETY: the common-modes constant is a valid static.
    rl.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    PORT.store((&*port as *const CFMachPort).cast_mut(), Ordering::SeqCst);
    CGEvent::tap_enable(&port, true);
    tracing::info!("minimize: the event tap runs (the yellow button and Cmd-M go straight to the strip)");
    CFRunLoop::run();
    PORT.store(std::ptr::null_mut(), Ordering::SeqCst);
    drop(port);
    true
}

/// What the callback decided, logged later by the WM thread.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Note {
    /// A press near a managed window's corner, left to the app, and why.
    PressPassed { at: Point, why: Pass, ms: u64 },
    /// A press on a minimize button released elsewhere: nothing happened.
    DraggedOff,
    /// ⌘M left to the app, and why.
    KeyPassed { why: Pass, ms: u64 },
    /// A swallowed minimize the WM thread could not be told of: minimized as macOS would.
    NoWm { id: u32 },
    /// macOS switched the tap off for a timeout; it is on again.
    OnAgain,
    /// macOS switched the tap off for user input; it stays off while secure input is on.
    OffByInput,
}

/// Keeps `n` for the WM thread's next tick (dropped when the list is busy or full).
fn note(n: Note) {
    if let Ok(mut notes) = NOTES.try_lock()
        && notes.len() < NOTES_MAX
    {
        notes.push(n);
    }
}

/// From the WM thread's tick: switches a tap that macOS switched off for user input on again
/// once secure input is over, then logs what the callback noted since the last tick.
pub fn refresh() {
    let now = Instant::now();
    let last = BACK_ON_AT.lock().map(|t| *t).unwrap_or(None);
    if super::sliver_tap::back_on(OFF_BY_INPUT.load(Ordering::Relaxed), last, now, super::sliver_tap::secure_input_on) {
        let port = PORT.load(Ordering::SeqCst);
        // SAFETY: the port is kept alive by the tap thread while its run loop runs (stored
        // before it starts, cleared after it ends; it runs for the life of the process).
        if let Some(port) = unsafe { port.as_ref() } {
            CGEvent::tap_enable(port, true);
            OFF_BY_INPUT.store(false, Ordering::Relaxed);
            if let Ok(mut t) = BACK_ON_AT.lock() {
                *t = Some(now);
            }
            tracing::info!("minimize: secure input is over; the event tap is on again");
        }
    }
    let notes = NOTES.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default();
    for n in notes {
        match n {
            Note::PressPassed { at, why, ms } => {
                tracing::debug!(x = at.x, y = at.y, ?why, ms, "minimize: a press near a window's buttons left to the app")
            }
            Note::DraggedOff => tracing::debug!("minimize: the minimize button pressed and released off it: nothing, as the button's own"),
            Note::KeyPassed { why, ms } => tracing::debug!(?why, ms, "minimize: Cmd-M left to the app"),
            Note::NoWm { id } => tracing::warn!(id, "minimize: a caught minimize found no WM thread; minimized as macOS would"),
            Note::OnAgain => tracing::debug!("minimize: the tap was switched off by macOS (a timeout); on again"),
            Note::OffByInput => tracing::info!("minimize: macOS switched the tap off for user input (secure input); a minimize plays macOS's genie until it is over"),
        }
    }
}

/// The windows published, when a press at `at` may be on one's buttons (cloned out of the
/// lock: nothing waits on Accessibility holding it).
fn set_for_press(at: Point) -> Option<Minimizable> {
    if !ADMITTED.load(Ordering::Relaxed) {
        return None;
    }
    let s = SET.try_lock().ok()?;
    s.as_ref().filter(|s| s.may_hit(at)).cloned()
}

/// The set published, for ⌘M.
fn set_for_key() -> Option<Minimizable> {
    if !ADMITTED.load(Ordering::Relaxed) {
        return None;
    }
    let s = SET.try_lock().ok()?;
    s.as_ref().filter(|s| s.on).cloned()
}

/// What Accessibility says of the element at `at`, with the window element it belongs to.
fn probe_button(at: Point) -> (ButtonHit, Option<AxElement>) {
    let hit = super::desktop::with_system_wide(AX_SECONDS, |system| system.element_at(at));
    let Ok(element) = hit else { return (ButtonHit::default(), None) };
    let _ = element.set_messaging_timeout(AX_SECONDS);
    let Ok(facts) = element.button_facts() else { return (ButtonHit::default(), None) };
    let minimize_button = facts.subrole.as_deref() == Some(names::MINIMIZE_BUTTON);
    let window = if minimize_button {
        facts.window.and_then(|w| w.window_id_within(AX_SECONDS).ok().map(|id| (id, w)))
    } else {
        None
    };
    let hit = ButtonHit {
        minimize_button,
        enabled: facts.enabled.unwrap_or(true),
        window: window.as_ref().map(|(id, _)| *id),
        frame: facts.frame,
    };
    (hit, window.map(|(_, w)| w))
}

/// What Accessibility says of the frontmost app's focused window, with its element.
fn probe_focused() -> (Focused, Option<AxElement>) {
    let app = super::desktop::with_system_wide(AX_SECONDS, |system| system.focused_application());
    let Ok(app) = app else { return (Focused::default(), None) };
    if app.pid().is_ok_and(|p| p == std::process::id() as i32) {
        // Our own windows (Settings) are never managed.
        return (Focused::default(), None);
    }
    let window = app.bounded(AX_SECONDS, |a| a.focused_window());
    let Ok(window) = window else { return (Focused::default(), None) };
    let _ = window.set_messaging_timeout(AX_SECONDS);
    let Ok(id) = window.window_id() else { return (Focused::default(), None) };
    let button = window.minimize_button().is_ok_and(|b| {
        let _ = b.set_messaging_timeout(AX_SECONDS);
        b.is_enabled().unwrap_or(false)
    });
    let settable = button && window.is_settable(names::MINIMIZED).unwrap_or(false);
    (Focused { window: Some(id), button, settable }, Some(window))
}

/// Hands a swallowed minimize to the WM thread; without one, minimizes as macOS would.
fn ask(window: u32, element: AxElement, by: By) {
    let ask = MinimizeAsk { window, element, by, at: Instant::now() };
    let sent = WM.get().is_some_and(|wm| wm.send(WmCommand::Minimize(ask.clone())));
    if !sent {
        note(Note::NoWm { id: window });
        ask.as_macos("no WM thread");
    }
}

/// A left-button event under the gate (`flags`: the modifiers held): true to swallow it.
fn on_mouse(kind: CGEventType, at: Point, flags: CGEventFlags) -> bool {
    let Ok(mut gate) = GATE.try_lock() else { return false };
    let g = match kind {
        CGEventType::LeftMouseDown => gate.down(|| {
            if !plain_click(flags.contains(CGEventFlags::MaskShift), flags.contains(CGEventFlags::MaskAlternate), flags.contains(CGEventFlags::MaskControl)) {
                return None;
            }
            let set = set_for_press(at)?;
            let started = Instant::now();
            let (hit, element) = probe_button(at);
            match (judge_press(&hit, &set), element) {
                (Ok((window, button)), Some(handle)) => Some(Pressed { window, button, handle }),
                (Ok(_), None) => None,
                (Err(why), _) => {
                    note(Note::PressPassed { at, why, ms: started.elapsed().as_millis() as u64 });
                    None
                }
            }
        }),
        CGEventType::LeftMouseDragged => gate.dragged(),
        CGEventType::LeftMouseUp => {
            let held = gate.holding();
            let g = gate.up(at);
            if held && g == Gate::Swallow {
                note(Note::DraggedOff);
            }
            g
        }
        _ => Gate::Pass,
    };
    HOLDING.store(gate.holding(), Ordering::Relaxed);
    match g {
        Gate::Pass => false,
        Gate::Swallow => true,
        Gate::Minimize(p) => {
            ask(p.window, p.handle, By::Button);
            true
        }
    }
}

/// A key-down: true to swallow it.
fn on_key(event: &CGEvent) -> bool {
    let k = key_press(event);
    if !is_cmd_m(&k) {
        return false;
    }
    let Some(set) = set_for_key() else { return false };
    let started = Instant::now();
    let (focused, element) = probe_focused();
    match (judge_key(&k, &focused, &set), element) {
        (KeyGate::Minimize(window), Some(e)) => {
            ask(window, e, By::Key);
            true
        }
        (KeyGate::Swallow, _) => true,
        (KeyGate::Pass(why), _) => {
            note(Note::KeyPassed { why, ms: started.elapsed().as_millis() as u64 });
            false
        }
        (KeyGate::Minimize(_), None) => false,
    }
}

/// The key-down as [`is_cmd_m`] reads it.
fn key_press(event: &CGEvent) -> KeyPress {
    let flags = CGEvent::flags(Some(event));
    let keycode = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode) as u16;
    let repeat = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventAutorepeat) != 0;
    let mut buf = [0u16; 4];
    let mut n = 0;
    // SAFETY: `n` and `buf` are valid out pointers; at most `buf.len()` units are written.
    unsafe { CGEvent::keyboard_get_unicode_string(Some(event), buf.len() as _, &mut n, buf.as_mut_ptr()) };
    let chars = String::from_utf16_lossy(&buf[..(n as usize).min(buf.len())]);
    KeyPress {
        keycode,
        chars,
        cmd: flags.contains(CGEventFlags::MaskCommand),
        shift: flags.contains(CGEventFlags::MaskShift),
        option: flags.contains(CGEventFlags::MaskAlternate),
        control: flags.contains(CGEventFlags::MaskControl),
        repeat,
    }
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    _info: *mut c_void,
) -> *mut CGEvent {
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        if kind == CGEventType::TapDisabledByUserInput {
            if !OFF_BY_INPUT.swap(true, Ordering::Relaxed) {
                note(Note::OffByInput);
            }
            return event.as_ptr();
        }
        let port = PORT.load(Ordering::SeqCst);
        // SAFETY: the port is kept alive by the tap thread while it runs (stored before the
        // run loop starts, cleared after it ends).
        if let Some(port) = unsafe { port.as_ref() } {
            CGEvent::tap_enable(port, true);
            note(Note::OnAgain);
        }
        return event.as_ptr();
    }
    // SAFETY: the event is live for the callback's duration.
    let ev = unsafe { event.as_ref() };
    let swallow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if kind == CGEventType::KeyDown {
            on_key(ev)
        } else {
            let loc = CGEvent::location(Some(ev));
            on_mouse(kind, Point::new(loc.x, loc.y), CGEvent::flags(Some(ev)))
        }
    }))
    .unwrap_or(false);
    if swallow { std::ptr::null_mut() } else { event.as_ptr() }
}
