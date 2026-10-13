//! The event tap that makes parked windows' slivers inert (`[windows] inert_slivers`,
//! [`crate::engine::sliver`]).
//!
//! An active (filtering) `CGEventTap` at the session level, head-inserted, for mouse presses,
//! drags and releases of every button, on a thread of its own with its own run loop: the WM
//! thread waits on Accessibility calls and the main thread draws animations, and a tap whose
//! callback is late is switched off by macOS. Filtering mouse events needs Accessibility,
//! which the app holds; without it the tap cannot be made and slivers stay as they were.
//!
//! The callback reads what the WM thread publishes ([`publish`]: the corner zones and the
//! parked windows) with a `try_lock`, never waiting; a press outside every zone (nearly all)
//! passes after a few rectangle tests. Only a press inside a zone reads the window server's
//! on-screen list to find the topmost window there. A swallowed press swallows its drags and
//! its release ([`PressGate`]); each one is logged with what it cost. A tap macOS switched off
//! (a timeout, user input) is switched on again at once. A window not parked that covers a
//! whole zone takes every press there (the tap passes them); if it lets clicks through (an
//! overlay), the slivers under it stay pressable: noted in the log once ([`zone_owner`]).

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
use crate::engine::geometry::Point;
use crate::engine::sliver::{Button, Listed, Press, PressGate, Shield, zone_owner};

static SHIELD: Mutex<Option<Shield>> = Mutex::new(None);
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
    windows::on_screen().into_iter().map(|w| Listed { id: w.id, pid: w.pid, bounds: w.bounds, alpha: w.alpha }).collect()
}

/// Whether a press at `at` lands on a sliver, and logs the exact test (zone hits only).
fn on_sliver(at: Point) -> bool {
    // The cheap test under the lock; the shield is copied out only for a press in a zone.
    let shield = match SHIELD.try_lock() {
        Ok(s) => s.as_ref().filter(|s| s.may_hit(at)).cloned(),
        Err(_) => return false,
    };
    let Some(shield) = shield else { return false };
    let started = Instant::now();
    let own = std::process::id() as i32;
    let list = listed();
    let hit = shield.hits(&list, at, own);
    let took_us = started.elapsed().as_micros() as u64;
    match hit {
        Some(id) => tracing::info!(id, x = at.x, y = at.y, took_us, "inert slivers: a press on a parked window's sliver swallowed"),
        None => {
            tracing::debug!(x = at.x, y = at.y, took_us, "inert slivers: a press in the corner, not on a sliver: passed");
            note_zone_owner(&shield, &list, at, own);
        }
    }
    hit.is_some()
}

/// Logs once a run a window not parked that covers the whole zone of a passed press.
fn note_zone_owner(shield: &Shield, list: &[Listed], at: Point, own: i32) {
    if OWNER_NOTED.load(Ordering::Relaxed) {
        return;
    }
    let owner = shield
        .zones
        .iter()
        .filter(|z| at.x >= z.x && at.x < z.max_x() && at.y >= z.y && at.y < z.max_y())
        .find_map(|z| zone_owner(list, *z, own, &shield.parked).map(|w| (*z, w)));
    if let Some((zone, w)) = owner
        && !OWNER_NOTED.swap(true, Ordering::Relaxed)
    {
        tracing::info!(
            id = w.id,
            pid = w.pid,
            bounds = %w.bounds,
            zone = %zone,
            "inert slivers: a window not parked covers the whole corner zone; presses there are its (one that lets clicks through leaves the slivers under it pressable). Noted once"
        );
    }
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    _info: *mut c_void,
) -> *mut CGEvent {
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        let port = PORT.load(Ordering::SeqCst);
        // SAFETY: the port is kept alive by the tap thread while it runs (stored before the
        // run loop starts, cleared after it ends).
        if let Some(port) = unsafe { port.as_ref() } {
            CGEvent::tap_enable(port, true);
            tracing::debug!(?kind, "inert slivers: the tap was switched off by macOS; on again");
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

    /// What a press costs: the cheap test (every press) and the window list read (presses in
    /// the corner zone only). Reads the window server's list, moves nothing:
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
