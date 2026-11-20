//! A listen-only event tap at the HID level that hears Mission Control's keys (round
//! mc-key; the Spaces engine, `[windows] mission_control_stages` not `"off"`).
//!
//! The global input monitor ([`super::input::monitor`]) never heard one: macOS takes its
//! symbolic hotkeys (Mission Control's ⌃↑, App Exposé's ⌃↓, the keyboard's Mission Control
//! key) in the window server, before any app gets the key-down, and a global `NSEvent`
//! monitor sees only what is sent to an app. No `mission-control: trigger seen` line was ever
//! logged (the owner's logs, every day to 11-20), so Mission Control was found by the tick, up
//! to 2 s late at rest (11-20 11:16:34 `found_within_ms=2140`), and `"key"` mode and the parks
//! at the key never ran. A tap at `kCGHIDEventTap` sees the events as they enter the window
//! server, before the hotkeys are taken, and sees those posted there too (`ctl debug-expose`).
//!
//! It only listens (`kCGEventTapOptionListenOnly`): nothing is held, swallowed or posted,
//! and the session's input never waits on it. It needs the Accessibility permission the app
//! has already (no new prompt: Input Monitoring is never asked for). The callback forwards to
//! the WM thread ([`WmCommand::MissionInput`]) only:
//!
//! - a key-down of one of Mission Control's shortcuts as the WM thread published them
//!   ([`publish`]: the user's own, read from Keyboard Shortcuts) or of the Mission Control key
//!   ([`crate::engine::mission_control::MISSION_CONTROL_KEY`]), never a repeat;
//! - while Mission Control may show ([`publish`]'s `armed`), Escape and mouse-downs: what
//!   closed it ([`crate::engine::mission_close`]).
//!
//! Every other key is dropped in the callback, unread past its code and modifiers, and
//! nothing typed is logged: the log names the shortcuts' key codes only. System-defined
//! events (media keys) are counted, nothing more, for `ctl state` (`mission_control.tap`):
//! a Mission Control key that comes as one shows there.
//!
//! On only while wanted ([`TapSwitch`]): the Spaces engine arranging with the mode not off.
//! macOS switches a tap off for secure input (a password field) and for a timeout; the tick
//! switches it on again once secure input is over, a timeout's at once. macOS also sends the
//! callback its "disabled by user input" notice when we switch the tap off ourselves: one that
//! comes while the tap is not wanted is ours, and is not taken for secure input.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopCommonModes};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use serde_json::{Value, json};

use super::tap_switch::TapSwitch;
use super::wm_thread::{WmCommand, WmHandle};
use crate::engine::mission_close;
use crate::engine::mission_control::{Keys, MISSION_CONTROL_KEY, Mods, trigger};

/// What the tap heard, for the WM thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// One of Mission Control's keys (its shortcuts, the Mission Control key, Escape while it
    /// may show): the key code and the modifiers held.
    Key {
        /// The virtual key code.
        code: u16,
        /// ⌃, ⌥ and ⌘ held.
        mods: Mods,
    },
    /// A mouse button pressed while Mission Control may show.
    Click,
}

/// One thing heard, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeardAt {
    /// What.
    pub what: Heard,
    /// When the callback got it.
    pub at: Instant,
}

/// The system-defined event type (`NX_SYSDEFINED`: media and special keys).
const SYSTEM_DEFINED: CGEventType = CGEventType(14);

static WM: OnceLock<WmHandle> = OnceLock::new();
static PORT: AtomicPtr<CFMachPort> = AtomicPtr::new(std::ptr::null_mut());
static STARTED: AtomicBool = AtomicBool::new(false);
static SWITCH: TapSwitch = TapSwitch::new();
/// The WM thread arranges (not standing down, no logout coming, Accessibility on).
static ADMITTED: AtomicBool = AtomicBool::new(false);
/// The Spaces engine runs with the mode not off ([`publish`]).
static WANTED_BY_ENGINE: AtomicBool = AtomicBool::new(false);
/// Mission Control may show: Escape and presses are forwarded.
static ARMED: AtomicBool = AtomicBool::new(false);
/// Mission Control's shortcuts as the WM thread last read them.
static KEYS: Mutex<Option<Keys>> = Mutex::new(None);
/// macOS switched the tap off for user input: it stays off until secure input ends.
static OFF_BY_INPUT: AtomicBool = AtomicBool::new(false);
static BACK_ON_AT: Mutex<Option<Instant>> = Mutex::new(None);
static REFUSED_NOTED: AtomicBool = AtomicBool::new(false);
static TRIED_AT: Mutex<Option<Instant>> = Mutex::new(None);
const RETRY_EVERY: Duration = Duration::from_secs(5);

/// Mission Control's keys heard (forwarded), the last one's code, system-defined events
/// seen, Escape and presses forwarded: `ctl state`, the live checks.
static KEYS_HEARD: AtomicU64 = AtomicU64::new(0);
static LAST_KEY: AtomicU32 = AtomicU32::new(u32::MAX);
static SYSTEM_EVENTS: AtomicU64 = AtomicU64::new(0);
static CLOSERS_HEARD: AtomicU64 = AtomicU64::new(0);
static CLICKS_HEARD: AtomicU64 = AtomicU64::new(0);
/// What the callback noted for the WM thread's log ([`refresh`]).
static NOTED_ON_AGAIN: AtomicBool = AtomicBool::new(false);
static NOTED_OFF_BY_INPUT: AtomicBool = AtomicBool::new(false);

/// The WM thread's handle: what is heard goes there.
pub fn set_handle(h: WmHandle) {
    let _ = WM.set(h);
}

/// Whether the WM thread arranges now ([`ADMITTED`]).
pub fn admit(on: bool) {
    ADMITTED.store(on, Ordering::Relaxed);
    sync();
}

/// From the engine's tick: `wanted` (the Spaces engine, the mode not off), the shortcuts read
/// from the user's settings, and whether Mission Control may show now (`armed`: it is seen,
/// or windows are out for it).
pub fn publish(wanted: bool, keys: Option<Keys>, armed: bool) {
    WANTED_BY_ENGINE.store(wanted, Ordering::Relaxed);
    ARMED.store(wanted && armed, Ordering::Relaxed);
    if let Ok(mut k) = KEYS.lock()
        && *k != keys
    {
        *k = keys;
    }
    sync();
}

/// True when the tap has something to listen for: the WM thread arranges, the engine wants it.
fn wanted() -> bool {
    ADMITTED.load(Ordering::Relaxed) && WANTED_BY_ENGINE.load(Ordering::Relaxed)
}

fn sync() {
    let on = wanted();
    SWITCH.set_wanted(on);
    if on {
        start();
    } else {
        OFF_BY_INPUT.store(false, Ordering::Relaxed);
    }
    SWITCH.apply(port(), false, true);
}

/// Whether the tap is on now.
pub fn tap_on() -> bool {
    SWITCH.enabled()
}

/// True while macOS keeps the tap off for secure input: the tick stays brisk and switches it
/// on again once secure input is over ([`refresh`]).
pub fn off_for_secure_input() -> bool {
    OFF_BY_INPUT.load(Ordering::Relaxed) && SWITCH.wanted()
}

/// `ctl state`'s view: on, how much it heard, the last key's code.
pub fn status() -> Value {
    let last = LAST_KEY.load(Ordering::Relaxed);
    json!({
        "on": tap_on(),
        "running": !PORT.load(Ordering::SeqCst).is_null(),
        "armed": ARMED.load(Ordering::Relaxed),
        "keys_heard": KEYS_HEARD.load(Ordering::Relaxed),
        "last_key": (last != u32::MAX).then_some(last),
        "closers_heard": CLOSERS_HEARD.load(Ordering::Relaxed),
        "clicks_heard": CLICKS_HEARD.load(Ordering::Relaxed),
        "system_defined_seen": SYSTEM_EVENTS.load(Ordering::Relaxed),
        "off_for_secure_input": off_for_secure_input(),
    })
}

fn port() -> Option<&'static CFMachPort> {
    let port = PORT.load(Ordering::SeqCst);
    // SAFETY: the port is kept alive by the tap thread while its run loop runs (stored before
    // it starts, cleared after it ends; it runs for the life of the process).
    unsafe { port.as_ref() }
}

/// Starts the tap thread once; a tap macOS refuses is tried again at most every
/// [`RETRY_EVERY`].
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
    let spawned = std::thread::Builder::new().name("mission-key-tap".into()).spawn(|| {
        if !run() {
            STARTED.store(false, Ordering::SeqCst);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("mission-control: cannot start the key tap's thread: {e}");
        STARTED.store(false, Ordering::SeqCst);
    }
}

fn mask(types: &[CGEventType]) -> CGEventMask {
    types.iter().fold(0, |m, t| m | (1u64 << t.0))
}

/// Makes the tap and runs its run loop; false when macOS refused the tap.
fn run() -> bool {
    let events = mask(&[CGEventType::KeyDown, CGEventType::LeftMouseDown, CGEventType::RightMouseDown, CGEventType::OtherMouseDown, SYSTEM_DEFINED]);
    // SAFETY: valid enum values; the callback has the documented signature and uses no user
    // info.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::TailAppendEventTap,
            CGEventTapOptions::ListenOnly,
            events,
            Some(callback),
            std::ptr::null_mut(),
        )
    };
    let Some(port) = port else {
        if !REFUSED_NOTED.swap(true, Ordering::Relaxed) {
            tracing::warn!("mission-control: macOS refused the key tap (Accessibility?); Mission Control is found by the tick (tried again every 5 s)");
        }
        return false;
    };
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
        tracing::warn!("mission-control: no run loop source for the key tap");
        return false;
    };
    let Some(rl) = CFRunLoop::current() else { return false };
    // SAFETY: the common-modes constant is a valid static.
    rl.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    PORT.store((&*port as *const CFMachPort).cast_mut(), Ordering::SeqCst);
    SWITCH.made(&port);
    tracing::info!(on = SWITCH.enabled(), "mission-control: the key tap runs (Mission Control's shortcuts, its key, and Escape while it shows)");
    CFRunLoop::run();
    PORT.store(std::ptr::null_mut(), Ordering::SeqCst);
    drop(port);
    true
}

/// From the WM thread's tick: a tap macOS switched off for secure input on again once it is
/// over; what the callback noted, logged.
pub fn refresh() {
    let now = Instant::now();
    let last = BACK_ON_AT.lock().map(|t| *t).unwrap_or(None);
    if SWITCH.wanted() && super::corner::sliver_tap::back_on(OFF_BY_INPUT.load(Ordering::Relaxed), last, now, super::corner::sliver_tap::secure_input_on)
        && let Some(port) = port()
    {
        CGEvent::tap_enable(port, true);
        OFF_BY_INPUT.store(false, Ordering::Relaxed);
        if let Ok(mut t) = BACK_ON_AT.lock() {
            *t = Some(now);
        }
        tracing::info!("mission-control: secure input is over; the key tap is on again");
    }
    if NOTED_ON_AGAIN.swap(false, Ordering::Relaxed) {
        tracing::debug!("mission-control: the key tap was switched off by macOS (a timeout); on again");
    }
    if NOTED_OFF_BY_INPUT.swap(false, Ordering::Relaxed) {
        tracing::info!("mission-control: macOS switched the key tap off for user input (secure input); its keys are not heard until it is over");
    }
}

/// What the callback forwards of a key-down with `code` and `mods` (not a repeat): one of
/// Mission Control's shortcuts (`keys`, when read) or its key, or, while `armed`, a key that
/// closes it ([`mission_close::closes`]). `None`: dropped, unread.
fn forwarded(code: u16, mods: Mods, keys: Option<&Keys>, armed: bool) -> Option<Heard> {
    let shortcut = code == MISSION_CONTROL_KEY || keys.is_some_and(|k| trigger(k, code, mods).is_some());
    let closer = armed && keys.map_or(code == mission_close::ESCAPE_KEY && mods == Mods::default(), |k| mission_close::closes(k, code, mods));
    (shortcut || closer).then_some(Heard::Key { code, mods })
}

fn send(what: Heard) {
    if let Some(wm) = WM.get() {
        wm.send(WmCommand::MissionInput(HeardAt { what, at: Instant::now() }));
    }
}

fn on_key(event: &CGEvent) {
    if CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventAutorepeat) != 0 {
        return;
    }
    let code = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode) as u16;
    let flags = CGEvent::flags(Some(event));
    let mods = Mods {
        control: flags.contains(CGEventFlags::MaskControl),
        option: flags.contains(CGEventFlags::MaskAlternate),
        command: flags.contains(CGEventFlags::MaskCommand),
    };
    let armed = ARMED.load(Ordering::Relaxed);
    // Never waits: a busy lock drops only the shortcuts' lookup (the Mission Control key and
    // Escape still pass).
    let keys = KEYS.try_lock().ok().and_then(|k| *k);
    let Some(heard) = forwarded(code, mods, keys.as_ref(), armed) else { return };
    KEYS_HEARD.fetch_add(1, Ordering::Relaxed);
    LAST_KEY.store(u32::from(code), Ordering::Relaxed);
    if armed && keys.as_ref().map_or(code == mission_close::ESCAPE_KEY, |k| mission_close::closes(k, code, mods)) {
        CLOSERS_HEARD.fetch_add(1, Ordering::Relaxed);
    }
    send(heard);
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    _info: *mut c_void,
) -> *mut CGEvent {
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        // Our own switch-off comes as "by user input" too: only a wanted tap's is macOS's.
        if kind == CGEventType::TapDisabledByUserInput {
            if SWITCH.wanted() && !OFF_BY_INPUT.swap(true, Ordering::Relaxed) {
                NOTED_OFF_BY_INPUT.store(true, Ordering::Relaxed);
            }
        } else if let Some(port) = port()
            && SWITCH.on_again(port)
        {
            NOTED_ON_AGAIN.store(true, Ordering::Relaxed);
        }
        return event.as_ptr();
    }
    super::costs::bump(super::costs::Cost::TapEvent);
    // SAFETY: the event is live for the callback's duration.
    let ev = unsafe { event.as_ref() };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match kind {
        CGEventType::KeyDown => on_key(ev),
        SYSTEM_DEFINED => {
            SYSTEM_EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        _ if ARMED.load(Ordering::Relaxed) => {
            CLICKS_HEARD.fetch_add(1, Ordering::Relaxed);
            send(Heard::Click);
        }
        _ => {}
    }));
    // Listen only: the event goes on as it came.
    event.as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Keys {
        use crate::engine::mission_control::Shortcut;
        let ctrl = Mods { control: true, ..Mods::default() };
        Keys {
            mission_control: Shortcut { keycode: 126, mods: ctrl, enabled: true },
            app_windows: Shortcut { keycode: 125, mods: ctrl, enabled: true },
        }
    }

    #[test]
    fn only_mission_controls_keys_are_forwarded() {
        let k = keys();
        let ctrl = Mods { control: true, ..Mods::default() };
        let none = Mods::default();
        assert_eq!(forwarded(126, ctrl, Some(&k), false), Some(Heard::Key { code: 126, mods: ctrl }), "⌃↑");
        assert!(forwarded(125, ctrl, Some(&k), false).is_some(), "⌃↓");
        assert!(forwarded(MISSION_CONTROL_KEY, none, None, false).is_some(), "the Mission Control key, the shortcuts not read yet");
        assert_eq!(forwarded(0, none, Some(&k), false), None, "typing");
        assert_eq!(forwarded(0, ctrl, Some(&k), true), None, "⌃A");
        assert_eq!(forwarded(126, none, Some(&k), true), None, "an arrow moves Mission Control's selection");
        assert_eq!(forwarded(124, ctrl, Some(&k), false), None, "⌃→ switches Desktops");
        assert_eq!(forwarded(126, ctrl, None, false), None, "the shortcuts not read yet");
    }

    #[test]
    fn escape_only_while_mission_control_may_show() {
        let k = keys();
        let none = Mods::default();
        assert_eq!(forwarded(mission_close::ESCAPE_KEY, none, Some(&k), false), None, "typed in an app");
        assert!(forwarded(mission_close::ESCAPE_KEY, none, Some(&k), true).is_some());
        assert!(forwarded(mission_close::ESCAPE_KEY, none, None, true).is_some(), "the shortcuts not read yet");
        assert_eq!(forwarded(mission_close::ESCAPE_KEY, Mods { command: true, ..none }, Some(&k), true), None, "⌘Escape");
    }
}
