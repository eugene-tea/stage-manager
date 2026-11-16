//! The Apple mode's posted gestures ([`start`]): a Shift-click on a strip thumbnail and a drag
//! of a title bar onto the strip, each on a thread of its own (never a sleep on the WM
//! thread), one at a time. How the gesture ended is taken with [`take_landed`].
//!
//! - **The Shift press** (probes of 11-16): a real Shift key-down about 12 ms before (none
//!   when the person holds Shift: theirs is used as it is), then each Shift-click of the press
//!   [`CLICK_GAP`] apart, at its point with no hold between down and up, the pointer warped
//!   back at once (away 2-15 ms, about a frame), Shift up about 12 ms after the last. Flags on
//!   the click alone switch instead of adding: the key events are what Apple's reads. Each
//!   click checks its point first; one whose point is no longer WindowManager's is left for
//!   the next settle, and the person's own move or press ends the press there.
//! - **The pointer back**: the posted events wait in the event queue, so a warp right after
//!   posting can land before them and the click then leaves the pointer at its point (live
//!   11-16: after a merge of three, the pointer stayed at the second click's point). The warp
//!   back waits until the pointer reads at the point posted (at most [`POINTER_WAIT`]), and
//!   is checked and made again once if the pointer is not home.
//! - **The drag**: the left button pressed on the title bar, moved in steps to the strip's
//!   column, held there [`DRAG_HOLD`], released; the pointer warped back. The person's own
//!   input ends it at once (their release, the button up, the pointer moved from where the
//!   drag put it): nothing more is posted, the button goes up where the pointer is, the
//!   pointer is left to the person, and [`Landed::Interrupted`] tells the WM thread to set the
//!   window's frame back.
//!
//! **Guards.** Each gesture checks the window list itself just before it posts: a click goes
//! only where the topmost window is WindowManager's (a strip item's stand-in), a drag only
//! where the topmost window is the window dragged. Nothing is posted while a mouse button is
//! down. A [`Gesture`] guard ends whatever is held if its thread unwinds, and
//! [`end_in_flight`] (the engine's bring-all-back: quit, a signal, the panic hook) ends a
//! gesture still in flight from outside: Shift up, the button up, the pointer back.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton,
    CGWarpMouseCursorPosition,
};

use crate::engine::apple::desk::{CLICK_GAP, Landed, Posted, Press};
use crate::engine::geometry::Point;
use crate::platform::{input, windows};

/// Between the Shift key-down and the click, and between the click and Shift up (measured).
pub const SHIFT_LEAD: Duration = Duration::from_millis(12);

/// The drag's press before it moves (the window server takes it as a press on the title bar).
pub const DRAG_PRESS: Duration = Duration::from_millis(60);

/// The drag's step along its path, in points, and the pause after each.
const DRAG_STEP: f64 = 24.0;
const DRAG_STEP_PAUSE: Duration = Duration::from_millis(6);

/// How long the drag holds over the strip before the release (the probe held 1.2 s with the
/// strip covered; the shortest Apple's accepts is still to measure, probe 6).
pub const DRAG_HOLD: Duration = Duration::from_millis(300);

/// The pointer this far from where the drag last put it is the person's own move.
const MOVED: f64 = 4.0;

/// How long the warp back waits for the posted events to have moved the pointer, and then for
/// the warp to show.
pub const POINTER_WAIT: Duration = Duration::from_millis(20);

/// Input events heard this soon after a gesture of ours ended are its own echoes (the global
/// monitor hears posted events), never the person's.
pub const ECHO: Duration = Duration::from_millis(300);

/// Shift's virtual key code.
const SHIFT_KEY: u16 = 56;

/// A gesture is in flight.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// How the last gesture ended, until taken.
static LANDED: Mutex<Option<Landed>> = Mutex::new(None);

/// When the last gesture ended.
static ENDED: Mutex<Option<Instant>> = Mutex::new(None);

/// What the gesture in flight holds: Shift down, the button down, where the pointer goes back.
static HELD: Mutex<Held> = Mutex::new(Held { shift: false, button: false, home: None });

#[derive(Debug, Clone, Copy)]
struct Held {
    shift: bool,
    button: bool,
    home: Option<(f64, f64)>,
}

/// True while a gesture of ours is in flight.
pub fn in_flight() -> bool {
    IN_FLIGHT.load(Ordering::SeqCst)
}

/// How the last gesture ended, once it has (taken).
pub fn take_landed() -> Option<Landed> {
    LANDED.lock().ok().and_then(|mut l| l.take())
}

/// True when an input event heard at `now` may be a gesture of ours: one is in flight, or
/// ended less than [`ECHO`] ago.
pub fn echo(now: Instant) -> bool {
    in_flight() || ENDED.lock().ok().and_then(|e| *e).is_some_and(|t| now.saturating_duration_since(t) < ECHO)
}

/// Starts `posted` on a thread of its own (`manager`: WindowManager's pid, whose window a
/// click must land on). `Err` when one is in flight already or no thread can be made.
pub fn start(posted: Posted, manager: Option<i32>) -> Result<(), String> {
    if IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return Err("a gesture is in flight already".into());
    }
    if let Ok(mut l) = LANDED.lock() {
        *l = None;
    }
    let spawned = std::thread::Builder::new().name("apple-gesture".into()).spawn(move || {
        // A panic midway still lands (refused) and ends the flight: the guard has let go of
        // whatever was held as it unwound.
        let landed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut g = Gesture::begin();
            let landed = match &posted {
                Posted::Clicks { presses, own_shift } => shift_press(&mut g, presses, *own_shift, manager),
                &Posted::Drag { window, from, to, .. } => drag(&mut g, window, from, to),
            };
            g.finish();
            landed
        }))
        .unwrap_or_else(|_| Landed::Refused("the gesture's thread panicked".into()));
        match &landed {
            Landed::Posted => tracing::debug!(?posted, "apple mode: gesture posted"),
            Landed::Refused(why) => tracing::info!(why, "apple mode: gesture not posted"),
            Landed::Interrupted(why) => tracing::info!(why, "apple mode: the person's own input ended the gesture"),
        }
        if let Ok(mut l) = LANDED.lock() {
            *l = Some(landed);
        }
        if let Ok(mut e) = ENDED.lock() {
            *e = Some(Instant::now());
        }
        IN_FLIGHT.store(false, Ordering::SeqCst);
    });
    match spawned {
        Ok(_) => Ok(()),
        Err(e) => {
            IN_FLIGHT.store(false, Ordering::SeqCst);
            Err(format!("no thread for the gesture: {e}"))
        }
    }
}

/// Ends a gesture still in flight from outside its thread (quit, a signal, the panic hook):
/// Shift up, the button up, the pointer back. Nothing when none is.
pub fn end_in_flight(why: &str) {
    if !in_flight() {
        return;
    }
    let held = HELD.lock().map(|h| *h).unwrap_or(Held { shift: false, button: false, home: None });
    release(held);
    tracing::warn!(why, shift = held.shift, button = held.button, "apple mode: a gesture in flight ended");
}

/// What a gesture holds, kept in [`HELD`] for [`end_in_flight`]; whatever it still holds is
/// let go when it drops (its thread unwinding too).
struct Gesture {
    held: Held,
}

impl Gesture {
    fn begin() -> Self {
        let g = Self { held: Held { shift: false, button: false, home: pointer_now().map(|p| (p.x, p.y)) } };
        g.publish();
        g
    }

    fn publish(&self) {
        if let Ok(mut h) = HELD.lock() {
            *h = self.held;
        }
    }

    fn shift(&mut self, down: bool) {
        self.held.shift = down;
        self.publish();
    }

    fn button(&mut self, down: bool) {
        self.held.button = down;
        self.publish();
    }

    /// The pointer back where it was, once the events posted at `posted` have moved it there
    /// (a warp before them would be undone by them); checked, and warped again once.
    fn home_from(&self, posted: Point) {
        let Some((x, y)) = self.held.home else { return };
        pointer_at(posted, POINTER_WAIT);
        for _ in 0..2 {
            let _ = CGWarpMouseCursorPosition(CGPoint { x, y });
            if pointer_at(Point::new(x, y), POINTER_WAIT) {
                return;
            }
        }
        tracing::debug!(x, y, "apple mode: the pointer not read back home after the warp");
    }

    /// True when the pointer is no longer where the gesture keeps it (home): the person moved
    /// it.
    fn moved_away(&self) -> bool {
        self.held.home.is_some_and(|(x, y)| pointer_now().is_some_and(|p| (p.x - x).abs() > MOVED || (p.y - y).abs() > MOVED))
    }

    /// Ended as planned: nothing held, nothing to put back on drop.
    fn finish(&mut self) {
        self.held = Held { shift: false, button: false, home: None };
        self.publish();
    }
}

impl Drop for Gesture {
    fn drop(&mut self) {
        if self.held.shift || self.held.button {
            release(self.held);
        }
        self.finish();
    }
}

/// Lets go of what `held` holds.
fn release(held: Held) {
    if held.button {
        let at = pointer_now().map(|p| Point::new(p.x, p.y)).unwrap_or_default();
        let _ = mouse(CGEventType::LeftMouseUp, at, CGEventFlags::empty());
    }
    if held.shift {
        key(SHIFT_KEY, false, CGEventFlags::empty());
    }
    if let Some((x, y)) = held.home {
        let _ = CGWarpMouseCursorPosition(CGPoint { x, y });
    }
}

/// The Shift press (module docs): every click of `presses` within one Shift press.
fn shift_press(g: &mut Gesture, presses: &[Press], own_shift: bool, manager: Option<i32>) -> Landed {
    let Some(manager) = manager else { return Landed::Refused("WindowManager is not running".into()) };
    let manager_at = |at: Point| top_is(at, |w| w.pid == manager && w.layer == 0);
    let Some(first) = presses.first() else { return Landed::Refused("no click to post".into()) };
    if let Err(why) = manager_at(first.at) {
        return Landed::Refused(why);
    }
    if any_button_down() {
        return Landed::Refused("a mouse button is down".into());
    }
    // A Shift the person holds is used as it is.
    let own = own_shift && !person_holds_shift();
    if own {
        key(SHIFT_KEY, true, CGEventFlags::MaskShift);
        g.shift(true);
        std::thread::sleep(SHIFT_LEAD);
    }
    let (mut posted, mut skipped, mut away_ms) = (0_u32, 0_u32, 0.0_f64);
    let mut ended: Option<&str> = None;
    let mut failed: Option<String> = None;
    for (k, p) in presses.iter().enumerate() {
        if k > 0 {
            std::thread::sleep(CLICK_GAP);
            if any_button_down() {
                ended = Some("a mouse button is down");
                break;
            }
            if g.moved_away() {
                // The person's own move: the pointer is theirs, the clicks left wait.
                g.held.home = None;
                g.publish();
                ended = Some("the pointer moved");
                break;
            }
            if manager_at(p.at).is_err() {
                skipped += 1;
                continue;
            }
        }
        let t0 = Instant::now();
        if let Err(e) = mouse(CGEventType::LeftMouseDown, p.at, CGEventFlags::MaskShift) {
            failed = Some(e);
            break;
        }
        g.button(true);
        let up = mouse(CGEventType::LeftMouseUp, p.at, CGEventFlags::MaskShift);
        g.button(up.is_err());
        g.home_from(p.at);
        away_ms = away_ms.max(t0.elapsed().as_secs_f64() * 1000.0);
        match up {
            Ok(()) => posted += 1,
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    std::thread::sleep(SHIFT_LEAD);
    if own {
        key(SHIFT_KEY, false, CGEventFlags::empty());
        g.shift(false);
    }
    tracing::debug!(posted, of = presses.len(), skipped, ended, away_ms, own_shift = own, "apple mode: Shift press posted");
    match (posted, failed) {
        (0, Some(e)) => Landed::Refused(e),
        (0, None) => Landed::Refused("no click of the press posted".into()),
        _ => Landed::Posted,
    }
}

/// The drag (module docs).
fn drag(g: &mut Gesture, window: u32, from: Point, to: Point) -> Landed {
    if let Err(why) = top_is(from, |w| w.id == window) {
        return Landed::Refused(why);
    }
    if any_button_down() {
        return Landed::Refused("a mouse button is down".into());
    }
    let path = input::segment(from, to, DRAG_STEP);
    if let Err(e) = mouse(CGEventType::LeftMouseDown, from, CGEventFlags::empty()) {
        return Landed::Refused(e);
    }
    g.button(true);
    std::thread::sleep(DRAG_PRESS);
    let ups = input::left_ups();
    let mut last = from;
    let person = |last: Point| -> Option<&'static str> {
        if input::left_ups() != ups {
            Some("a left mouse-up came")
        } else if !input::left_button_down() {
            Some("the left button is up")
        } else if pointer_now().is_some_and(|p| (p.x - last.x).abs() > MOVED || (p.y - last.y).abs() > MOVED) {
            Some("the pointer moved")
        } else {
            None
        }
    };
    for p in path.iter().skip(1) {
        if let Some(why) = person(last) {
            return interrupted(g, why);
        }
        let _ = mouse(CGEventType::LeftMouseDragged, *p, CGEventFlags::empty());
        last = *p;
        std::thread::sleep(DRAG_STEP_PAUSE);
    }
    let held = Instant::now();
    while held.elapsed() < DRAG_HOLD {
        std::thread::sleep(Duration::from_millis(30));
        if let Some(why) = person(last) {
            return interrupted(g, why);
        }
    }
    let up = mouse(CGEventType::LeftMouseUp, to, CGEventFlags::empty());
    g.button(up.is_err());
    g.home_from(to);
    match up {
        Ok(()) => Landed::Posted,
        Err(e) => Landed::Refused(e),
    }
}

/// The person took over mid-drag: the button up where the pointer is (unless they let it go
/// already), the pointer left to them.
fn interrupted(g: &mut Gesture, why: &'static str) -> Landed {
    if input::left_button_down() {
        let at = pointer_now().map(|p| Point::new(p.x, p.y)).unwrap_or_default();
        let _ = mouse(CGEventType::LeftMouseUp, at, CGEventFlags::empty());
    }
    g.button(false);
    g.held.home = None;
    g.publish();
    Landed::Interrupted(why.to_string())
}

/// `Ok` when the topmost window at `p` (on screen, at or above the windows' layer, not
/// clear) passes `ok`.
fn top_is(p: Point, ok: impl Fn(&windows::CgWindow) -> bool) -> Result<(), String> {
    let list = windows::on_screen();
    match list.iter().find(|w| w.onscreen && w.layer >= 0 && w.alpha > 0.0 && w.bounds.contains(p)) {
        Some(w) if ok(w) => Ok(()),
        Some(w) => Err(format!("the topmost window at {:.0},{:.0} is not the one meant (pid {}, layer {})", p.x, p.y, w.pid, w.layer)),
        None => Err(format!("no window at {:.0},{:.0}", p.x, p.y)),
    }
}

/// A mouse button is down, by the person or a posted event.
fn any_button_down() -> bool {
    [CGMouseButton::Left, CGMouseButton::Right, CGMouseButton::Center]
        .into_iter()
        .any(|b| CGEventSource::button_state(CGEventSourceStateID::CombinedSessionState, b))
}

/// The person holds Shift on their keyboard (the hardware's state, not a posted event's).
fn person_holds_shift() -> bool {
    CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState).contains(CGEventFlags::MaskShift)
}

fn pointer_now() -> Option<CGPoint> {
    CGEvent::new(None).map(|e| CGEvent::location(Some(&e)))
}

/// Waits up to `limit` for the pointer to read at `p` (within a point): true when it does.
fn pointer_at(p: Point, limit: Duration) -> bool {
    let t0 = Instant::now();
    loop {
        if pointer_now().is_some_and(|q| (q.x - p.x).abs() <= 1.0 && (q.y - p.y).abs() <= 1.0) {
            return true;
        }
        if t0.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_micros(500));
    }
}

/// A left-button mouse event of `kind` at `p` with `flags`, posted at the HID tap.
fn mouse(kind: CGEventType, p: Point, flags: CGEventFlags) -> Result<(), String> {
    let event = CGEvent::new_mouse_event(None, kind, CGPoint { x: p.x, y: p.y }, CGMouseButton::Left).ok_or("cannot create a mouse event")?;
    CGEvent::set_flags(Some(&event), flags);
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

/// A key-down (`down`) or key-up of `code` with `flags`, posted at the HID tap.
fn key(code: u16, down: bool, flags: CGEventFlags) {
    if let Some(event) = CGEvent::new_keyboard_event(None, code, down) {
        CGEvent::set_flags(Some(&event), flags);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
}
