//! The event tap that makes parked windows' slivers inert (`[windows] inert_slivers`,
//! [`crate::engine::sliver`]).
//!
//! An active (filtering) `CGEventTap` at the session level, head-inserted, for mouse presses,
//! drags and releases of every button, on a thread of its own with its own run loop: the WM
//! thread waits on Accessibility calls and the main thread draws animations, and a tap whose
//! callback is late is switched off by macOS. Filtering mouse events needs Accessibility,
//! which the app holds; without it the tap cannot be made and slivers stay as they were.
//!
//! The callback only does rectangle tests on what the WM thread publishes, read with a
//! `try_lock`, never waiting: the corner zones and the parked windows ([`publish`]), and the
//! on-screen windows near the zones, read every WM tick ([`refresh`], [`Seen`]). A press
//! outside every zone (nearly all) passes after a few rectangle tests; one inside a zone is
//! judged on the published list while it is fresh, and passes when it is not (the WM thread
//! busy). Nothing in the callback asks the window server or writes the log: what it decided
//! is noted in memory and logged by the WM thread at its next tick. A swallowed press
//! swallows its drags and its release ([`PressGate`]). A tap macOS switched off for a
//! timeout is switched on again at once; one switched off by user input (secure input, a
//! password field) stays off for the rest of the run: slivers are pressable again, the safe
//! way to fail. A window not parked that covers a whole zone takes every press there (the
//! tap passes them); if it lets clicks through (an overlay), the slivers under it stay
//! pressable: noted in the log once ([`zone_owner`]).

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopCommonModes};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy, CGEventType,
};

use super::windows;
use crate::engine::geometry::{Point, Rect};
use crate::engine::sliver::{Button, Listed, Press, PressGate, Seen, Shield, near_zones, zone_owner};

static SHIELD: Mutex<Option<Shield>> = Mutex::new(None);
/// The on-screen windows near the zones, as the WM thread last read them ([`refresh`]).
static SEEN: Mutex<Option<Seen>> = Mutex::new(None);
/// What the callback decided, for the WM thread to log ([`refresh`]); bounded.
static NOTES: Mutex<Vec<Note>> = Mutex::new(Vec::new());
/// Most notes kept between two WM ticks.
const NOTES_MAX: usize = 64;
/// macOS switched the tap off for user input: it stays off (noted once).
static OFF_BY_INPUT: AtomicBool = AtomicBool::new(false);
/// Only the tap thread uses it.
static GATE: Mutex<PressGate> = Mutex::new(PressGate::new());
static PORT: AtomicPtr<CFMachPort> = AtomicPtr::new(std::ptr::null_mut());
static STARTED: AtomicBool = AtomicBool::new(false);
/// A window owning a whole zone was noted in the log (once a run).
static OWNER_NOTED: AtomicBool = AtomicBool::new(false);

/// Hands the tap what it needs to judge a press: whether the shield is on, the corner zones
/// and the parked windows. Cheap; call after every change. Starts the tap the first time the
/// shield is on.
pub fn publish(shield: Shield) {
    let on = shield.on;
    if let Ok(mut s) = SHIELD.lock() {
        *s = Some(shield);
    }
    if on {
        start();
    }
}

/// Starts the tap thread once (no-op afterwards). A tap that cannot be made (no
/// Accessibility yet) is tried again at the next start.
fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("sliver-tap".into()).spawn(|| {
        if !run() {
            STARTED.store(false, Ordering::SeqCst);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("inert slivers: cannot start the tap thread: {e}");
        STARTED.store(false, Ordering::SeqCst);
    }
}

fn mask(types: &[CGEventType]) -> CGEventMask {
    types.iter().fold(0, |m, t| m | (1u64 << t.0))
}

/// Makes the tap and runs its run loop; false when the tap could not be made.
fn run() -> bool {
    let events = mask(&[
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::OtherMouseDragged,
    ]);
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
        tracing::warn!("inert slivers: macOS refused the event tap (Accessibility?); parked windows stay pressable");
        return false;
    };
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
        tracing::warn!("inert slivers: no run loop source for the tap");
        return false;
    };
    let Some(rl) = CFRunLoop::current() else { return false };
    // SAFETY: the common-modes constant is a valid static.
    rl.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    PORT.store((&*port as *const CFMachPort).cast_mut(), Ordering::SeqCst);
    CGEvent::tap_enable(&port, true);
    tracing::info!("inert slivers: the event tap runs");
    CFRunLoop::run();
    PORT.store(std::ptr::null_mut(), Ordering::SeqCst);
    drop(port);
    true
}

fn press_of(t: CGEventType) -> Option<Press> {
    Some(match t {
        CGEventType::LeftMouseDown => Press::Down(Button::Left),
        CGEventType::RightMouseDown => Press::Down(Button::Right),
        CGEventType::OtherMouseDown => Press::Down(Button::Other),
        CGEventType::LeftMouseDragged => Press::Dragged(Button::Left),
        CGEventType::RightMouseDragged => Press::Dragged(Button::Right),
        CGEventType::OtherMouseDragged => Press::Dragged(Button::Other),
        CGEventType::LeftMouseUp => Press::Up(Button::Left),
        CGEventType::RightMouseUp => Press::Up(Button::Right),
        CGEventType::OtherMouseUp => Press::Up(Button::Other),
        _ => return None,
    })
}

/// The window server's on-screen windows, front to back, as the hit test needs them.
fn listed() -> Vec<Listed> {
    windows::on_screen().into_iter().map(|w| Listed { id: w.id, pid: w.pid, bounds: w.bounds, alpha: w.alpha, layer: w.layer }).collect()
}

/// What the callback decided, logged later by the WM thread.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Note {
    /// A press on a parked window's sliver, swallowed.
    Swallowed { id: u32, at: Point },
    /// A press in a zone, not on a sliver: passed.
    Passed { at: Point },
    /// A press in a zone passed unjudged: the published list was missing or too old.
    Unjudged { at: Point, age_ms: Option<u64> },
    /// A window not parked covers the whole zone of a passed press (once a run).
    ZoneOwner { owner: Listed, zone: Rect },
    /// macOS switched the tap off for a timeout; it is on again.
    OnAgain,
    /// macOS switched the tap off for user input; it stays off.
    OffByInput,
}

/// Keeps `note` for the WM thread's next tick (dropped when the list is busy or full: notes
/// never hold up a press).
fn note(n: Note) {
    if let Ok(mut notes) = NOTES.try_lock()
        && notes.len() < NOTES_MAX
    {
        notes.push(n);
    }
}

/// What the tap does after macOS switched it off with `kind`: on again only after a timeout
/// (its callback was late); user input (secure input) means macOS wants it off.
fn on_again_after(kind: CGEventType) -> bool {
    kind == CGEventType::TapDisabledByTimeout
}

/// From the WM thread's tick: reads the on-screen windows near the zones for the callback
/// (only while the shield is on, a window is parked and the tap runs), then logs what the
/// callback noted since the last tick.
pub fn refresh() {
    let zones = match SHIELD.lock() {
        Ok(s) => s.as_ref().filter(|s| s.on && !s.parked.is_empty()).map(|s| s.zones.clone()),
        Err(_) => None,
    };
    let seen = zones.filter(|_| STARTED.load(Ordering::SeqCst)).map(|zones| Seen { at: Instant::now(), windows: near_zones(listed(), &zones) });
    if let Ok(mut s) = SEEN.lock() {
        *s = seen;
    }
    let notes = NOTES.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default();
    for n in notes {
        log_note(n);
    }
}

fn log_note(n: Note) {
    match n {
        Note::Swallowed { id, at } => tracing::info!(id, x = at.x, y = at.y, "inert slivers: a press on a parked window's sliver swallowed"),
        Note::Passed { at } => tracing::debug!(x = at.x, y = at.y, "inert slivers: a press in the corner, not on a sliver: passed"),
        Note::Unjudged { at, age_ms } => tracing::debug!(x = at.x, y = at.y, ?age_ms, "inert slivers: a press in the corner passed: no fresh window list"),
        Note::ZoneOwner { owner, zone } => tracing::info!(
            id = owner.id,
            pid = owner.pid,
            bounds = %owner.bounds,
            zone = %zone,
            "inert slivers: a window not parked covers the whole corner zone; presses there are its (one that lets clicks through leaves the slivers under it pressable). Noted once"
        ),
        Note::OnAgain => tracing::debug!("inert slivers: the tap was switched off by macOS (a timeout); on again"),
        Note::OffByInput => tracing::info!(
            "inert slivers: macOS switched the tap off for user input (secure input); it stays off, parked windows' edges are pressable until the next launch"
        ),
    }
}

/// Whether a press at `at` lands on a sliver: rectangle tests on the published shield and
/// window list only.
fn on_sliver(at: Point) -> bool {
    // The cheap test under the lock; the shield is copied out only for a press in a zone.
    let shield = match SHIELD.try_lock() {
        Ok(s) => s.as_ref().filter(|s| s.may_hit(at)).cloned(),
        Err(_) => return false,
    };
    let Some(shield) = shield else { return false };
    let Ok(seen) = SEEN.try_lock() else {
        note(Note::Unjudged { at, age_ms: None });
        return false;
    };
    let now = Instant::now();
    let Some(list) = seen.as_ref().and_then(|s| s.fresh(now)) else {
        let age_ms = seen.as_ref().map(|s| now.saturating_duration_since(s.at).as_millis() as u64);
        note(Note::Unjudged { at, age_ms });
        return false;
    };
    let own = std::process::id() as i32;
    let hit = shield.hits(list, at, own);
    match hit {
        Some(id) => note(Note::Swallowed { id, at }),
        None => {
            note(Note::Passed { at });
            note_zone_owner(&shield, list, at, own);
        }
    }
    hit.is_some()
}

/// Notes once a run a window not parked that covers the whole zone of a passed press.
fn note_zone_owner(shield: &Shield, list: &[Listed], at: Point, own: i32) {
    if OWNER_NOTED.load(Ordering::Relaxed) {
        return;
    }
    let owner = shield
        .zones
        .iter()
        .filter(|z| at.x >= z.x && at.x < z.max_x() && at.y >= z.y && at.y < z.max_y())
        .find_map(|z| zone_owner(list, *z, own, shield).map(|w| (*z, w)));
    if let Some((zone, owner)) = owner
        && !OWNER_NOTED.swap(true, Ordering::Relaxed)
    {
        note(Note::ZoneOwner { owner, zone });
    }
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    _info: *mut c_void,
) -> *mut CGEvent {
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        if !on_again_after(kind) {
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
    let Some(press) = press_of(kind) else { return event.as_ptr() };
    // SAFETY: the event is live for the callback's duration.
    let loc = CGEvent::location(Some(unsafe { event.as_ref() }));
    let at = Point::new(loc.x, loc.y);
    let swallow = std::panic::catch_unwind(|| {
        let Ok(mut gate) = GATE.try_lock() else { return false };
        gate.swallow(press, || on_sliver(at))
    })
    .unwrap_or(false);
    if swallow { std::ptr::null_mut() } else { event.as_ptr() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Rect;
    use crate::engine::park::Corner;
    use crate::engine::sliver::corner_zone;

    #[test]
    fn only_a_timeout_switches_the_tap_on_again() {
        assert!(on_again_after(CGEventType::TapDisabledByTimeout));
        assert!(!on_again_after(CGEventType::TapDisabledByUserInput), "secure input: macOS wants it off");
    }

    #[test]
    fn notes_are_bounded_and_never_wait() {
        let before = NOTES.lock().map(|n| n.len()).unwrap_or(0);
        for _ in 0..(NOTES_MAX + 10) {
            note(Note::OnAgain);
        }
        let notes = NOTES.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default();
        assert_eq!(notes.len(), NOTES_MAX.max(before));
        let held = NOTES.lock().expect("lock");
        note(Note::OnAgain);
        assert!(held.is_empty(), "a busy list drops the note instead of waiting");
    }

    /// What a press costs: the cheap test (every press) and the window list read (now on the
    /// WM thread, every tick). Reads the window server's list, moves nothing:
    /// `cargo test --lib sliver_tap -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads the live window list; run by hand for the timings"]
    fn what_a_press_costs() {
        let main = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let visible = Rect::new(0.0, 30.0, 2560.0, 1410.0);
        let shield = Shield {
            on: true,
            zones: vec![corner_zone(main, visible, Corner::BottomRight)],
            parked: (1..40).collect(),
            dock: Default::default(),
        };
        let n = 100_000;
        let started = Instant::now();
        let mut hits = 0;
        for i in 0..n {
            let lock = SHIELD.try_lock().map(|g| g.is_some()).unwrap_or(false);
            if shield.may_hit(Point::new((i % 2560) as f64, (i % 1440) as f64)) || lock {
                hits += 1;
            }
        }
        let cheap_ns = started.elapsed().as_nanos() / n as u128;
        let started = Instant::now();
        let rounds = 50;
        let mut count = 0;
        for _ in 0..rounds {
            count += listed().len();
        }
        let list_us = started.elapsed().as_micros() / rounds;
        println!("cheap test: {cheap_ns} ns a press ({hits} in zone); window list: {list_us} µs a read ({} windows)", count / rounds as usize);
    }
}
