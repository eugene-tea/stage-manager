//! Global input: one `NSEvent` global monitor for left mouse down/up and key down in other
//! apps, the live modifier and button state, and the Shift sugar's trigger record.
//!
//! - The monitor (main thread) reports every left mouse-down (wallpaper clicks, the start of
//!   a window drag), left mouse-up (the end of a window drag) and key-down, each with the
//!   Shift and Command flags the user held. Global monitors never see our own windows'
//!   events.
//! - [`Triggers`] remembers the latest of those user actions, so a window or an activation
//!   that arrives a little later can tell whether the user held Shift when they caused it
//!   ("hold Shift to bring things here"). A trigger with Command held is the app switcher
//!   (⌘Tab, ⌘⇧Tab): Shift there means "cycle backward", never "bring here".
//! - Test support: synthetic clicks and drags posted only where every point is over our own
//!   windows or a window of an app on the `STAGE_MANAGER_ONLY` list.

use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSRunningApplication};
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType,
    CGMouseButton, CGWarpMouseCursorPosition,
};
use objc2_foundation::{MainThreadMarker, NSString};

use super::desktop::{point_to_top_left, primary_height};
use super::windows;
use crate::engine::geometry::Point;

/// What the user did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// Left mouse button pressed.
    MouseDown,
    /// Left mouse button released.
    MouseUp,
    /// A key pressed.
    KeyDown,
}

/// One user action in another app, global top-left coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputEvent {
    /// Which action.
    pub kind: InputKind,
    /// Where the pointer was.
    pub at: Point,
    /// Shift held.
    pub shift: bool,
    /// Command held.
    pub cmd: bool,
    /// For a mouse event, macOS's click count: 2 for the second click of a double click
    /// (within the user's double-click interval, near the first), 1 for a single click; 0
    /// for a key.
    pub clicks: i64,
    /// For a key-down, its virtual key code ([`KEY_TAB`], …); `None` for a mouse event.
    pub key: Option<u16>,
}

/// The Tab key's virtual key code.
pub const KEY_TAB: u16 = 48;
/// The ` key's virtual key code (⌘` cycles an app's windows).
pub const KEY_GRAVE: u16 = 50;

impl InputEvent {
    /// A key-down of an app or window switcher, ⌘Tab or ⌘`: the next activation may switch
    /// stages, so fresh snapshots for its animation are worth taking. Other ⌘ shortcuts
    /// (⌘C, ⌘S, ⌘→ held down) are not.
    pub fn is_switcher_key(&self) -> bool {
        self.kind == InputKind::KeyDown && self.cmd && matches!(self.key, Some(KEY_TAB | KEY_GRAVE))
    }
}

/// Keeps the global monitor alive; dropping it removes the monitor.
pub struct InputMonitor {
    token: Retained<AnyObject>,
}

impl Drop for InputMonitor {
    fn drop(&mut self) {
        // SAFETY: the token came from addGlobalMonitorForEventsMatchingMask.
        unsafe { NSEvent::removeMonitor(&self.token) };
    }
}

/// Calls `sink` (main thread) for every left mouse down/up and key down in another app.
pub fn monitor(mtm: MainThreadMarker, sink: impl Fn(InputEvent) + 'static) -> Option<InputMonitor> {
    let block = RcBlock::new(move |event: NonNull<NSEvent>| {
        // SAFETY: AppKit passes a live event for the duration of the block.
        let event = unsafe { event.as_ref() };
        let kind = match event.r#type() {
            NSEventType::LeftMouseDown => InputKind::MouseDown,
            NSEventType::LeftMouseUp => InputKind::MouseUp,
            NSEventType::KeyDown => InputKind::KeyDown,
            _ => return,
        };
        let flags = event.modifierFlags();
        let at = NSEvent::mouseLocation();
        // clickCount and keyCode raise on the other kind of event: ask each only its own.
        let (clicks, key) = match kind {
            InputKind::KeyDown => (0, Some(event.keyCode())),
            InputKind::MouseDown | InputKind::MouseUp => (event.clickCount() as i64, None),
        };
        sink(InputEvent {
            kind,
            at: point_to_top_left(Point::new(at.x, at.y), primary_height(mtm)),
            shift: flags.contains(NSEventModifierFlags::Shift),
            cmd: flags.contains(NSEventModifierFlags::Command),
            clicks,
            key,
        });
    });
    let mask = NSEventMask::LeftMouseDown | NSEventMask::LeftMouseUp | NSEventMask::KeyDown;
    let token = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &block)?;
    Some(InputMonitor { token })
}

/// Modifier keys held right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Command.
    pub cmd: bool,
}

/// The modifiers held now (any thread).
pub fn live_modifiers() -> Modifiers {
    let flags = CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState);
    Modifiers { shift: flags.contains(CGEventFlags::MaskShift), cmd: flags.contains(CGEventFlags::MaskCommand) }
}

/// Where the pointer is now, global top-left coordinates (any thread).
pub fn pointer() -> Option<Point> {
    pointer_now().map(|p| Point::new(p.x, p.y))
}

/// True while the left mouse button is down (any thread).
pub fn left_button_down() -> bool {
    CGEventSource::button_state(CGEventSourceStateID::CombinedSessionState, CGMouseButton::Left)
}

/// How long after a Shift-held trigger a newly opened app's first window still counts.
pub const LAUNCH_WINDOW: Duration = Duration::from_secs(10);
/// How long after a Shift-held trigger an activation or another app's new window counts.
pub const ACTIVATION_WINDOW: Duration = Duration::from_millis(2000);

/// The latest user action, for the Shift sugar.
#[derive(Clone, Copy, Debug, Default)]
pub struct Triggers {
    last: Option<(Instant, Modifiers)>,
    used: bool,
}

impl Triggers {
    /// Records a user action (every mouse down/up and key down in another app).
    pub fn record(&mut self, now: Instant, mods: Modifiers) {
        self.last = Some((now, mods));
        self.used = false;
    }

    /// True when Shift says "bring it here" for something happening `now`: the latest user
    /// action, at most `within` ago and not used yet, held Shift without Command; or, when
    /// no recent action says otherwise, Shift is held `live` (without Command). A recent
    /// action with Command held (the app switcher) always means no.
    pub fn shift_here(&self, now: Instant, within: Duration, live: Modifiers) -> bool {
        match self.last {
            Some((at, mods)) if now.saturating_duration_since(at) <= within => {
                if mods.cmd || live.cmd || self.used {
                    return false;
                }
                mods.shift || live.shift
            }
            _ => live.shift && !live.cmd,
        }
    }

    /// Records the release of the latest press: it is the same user action, so a trigger
    /// already used stays used (a Shift-click's mouse-up must not re-arm the Shift sugar for
    /// the next app that activates on its own; review R1-ui 10).
    pub fn record_release(&mut self, now: Instant, mods: Modifiers) {
        let used = self.used;
        self.record(now, mods);
        self.used = used;
    }

    /// The latest trigger did its job; it does not count again.
    pub fn consume(&mut self) {
        self.used = true;
    }
}

// ---------------------------------------------------------------------------------------
// Test support

/// Pids whose windows synthetic test input may touch: ours plus the apps on the
/// `STAGE_MANAGER_ONLY` list. Without that list only our own.
pub fn test_pids() -> Vec<i32> {
    let mut pids = vec![std::process::id() as i32];
    if let Some(only) = super::observe::only_from_env() {
        for bundle in only {
            let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(&bundle));
            pids.extend(apps.iter().map(|a| a.processIdentifier()));
        }
    }
    pids
}

/// Checks that the topmost window at every point belongs to one of `pids`.
pub fn path_is_safe(path: &[Point], pids: &[i32]) -> Result<(), String> {
    let list = windows::list_windows();
    let own = std::process::id() as i32;
    for p in path {
        // Our own strip sits below normal windows (layer -1): it counts where nothing covers
        // it. Our click-through windows above the floating level (card ghosts, the motion
        // overlay) never take a click, so they never count.
        let top = list.iter().find(|w| {
            let click_through = w.pid == own && w.layer > CLICKABLE_LEVEL;
            w.onscreen && !click_through && (w.layer >= 0 || pids.contains(&w.pid)) && w.alpha > 0.0 && w.bounds.contains(*p)
        });
        match top {
            Some(w) if pids.contains(&w.pid) => {}
            Some(w) => return Err(format!("the topmost window at {:.0},{:.0} is not ours or a fixture's (pid {})", p.x, p.y, w.pid)),
            None => {
                // Say where our own windows are: the strip panel missing from the screen there
                // (ordered out, on another Space, elsewhere) is the usual story.
                let ours: Vec<String> = list
                    .iter()
                    .filter(|w| w.pid == own)
                    .map(|w| format!("layer {} {} {}", w.layer, w.bounds, if w.onscreen { "on screen" } else { "off screen" }))
                    .collect();
                return Err(format!("no window at {:.0},{:.0} (our windows: {})", p.x, p.y, ours.join("; ")));
            }
        }
    }
    Ok(())
}

/// The highest level of our own windows that take clicks (the strip lifted, the edge tab).
const CLICKABLE_LEVEL: i64 = 3;

/// Points from `a` to `b` at most `step` apart, both ends included.
pub fn segment(a: Point, b: Point, step: f64) -> Vec<Point> {
    let d = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
    let n = (d / step.max(0.5)).ceil().max(1.0) as usize;
    (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
        })
        .collect()
}

fn post_mouse(kind: CGEventType, p: Point, shift: bool) -> Result<(), String> {
    let at = CGPoint { x: p.x, y: p.y };
    let event = CGEvent::new_mouse_event(None, kind, at, CGMouseButton::Left).ok_or("cannot create a mouse event")?;
    CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventClickState, 1);
    if shift {
        CGEvent::set_flags(Some(&event), CGEventFlags::MaskShift);
    }
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

/// Posting events with the Shift flag leaves the session's modifier state saying Shift is
/// down; a flag-free Shift key-up clears it (otherwise every later check would see Shift).
fn release_shift() {
    const SHIFT_KEY: u16 = 56;
    if let Some(up) = CGEvent::new_keyboard_event(None, SHIFT_KEY, false) {
        CGEvent::set_flags(Some(&up), CGEventFlags::empty());
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&up));
    }
}

fn pointer_now() -> Option<CGPoint> {
    CGEvent::new(None).map(|e| CGEvent::location(Some(&e)))
}

/// Where the pointer was before a synthetic hover moved it: the next click or drag puts it
/// back there.
static HOVER_HOME: std::sync::Mutex<Option<(f64, f64)>> = std::sync::Mutex::new(None);

/// Where a synthetic click or drag puts the pointer back: before a hover, else where it is.
fn pointer_home() -> Option<CGPoint> {
    let hovered = HOVER_HOME.lock().ok().and_then(|mut h| h.take());
    hovered.map(|(x, y)| CGPoint { x, y }).or_else(pointer_now)
}

/// A synthetic pointer move to `p` (only where the topmost window is ours or a fixture's),
/// resting there for `rest` (the strip's left-edge reveal). The pointer stays; the next
/// synthetic click or drag puts it back where it was before.
pub fn hover(p: Point, rest: Duration) -> Result<(), String> {
    path_is_safe(&[p], &test_pids())?;
    if let Ok(mut home) = HOVER_HOME.lock()
        && home.is_none()
    {
        *home = pointer_now().map(|c| (c.x, c.y));
    }
    post_mouse(CGEventType::MouseMoved, p, false)?;
    std::thread::sleep(rest);
    Ok(())
}

/// A synthetic click at `p`, only where the topmost window is ours or a fixture's. The
/// pointer goes back where it was.
pub fn click(p: Point, shift: bool) -> Result<(), String> {
    path_is_safe(&[p], &test_pids())?;
    let back = pointer_home();
    for kind in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
        post_mouse(kind, p, shift)?;
        std::thread::sleep(Duration::from_millis(60));
    }
    if shift {
        release_shift();
    }
    if let Some(back) = back {
        let _ = CGWarpMouseCursorPosition(back);
    }
    Ok(())
}

/// A synthetic left-button drag along `waypoints` (straight segments), only when every point
/// it passes is over our own windows or a fixture's (checked before anything is posted). The
/// button is held `hold` at the end before release; the pointer goes back where it was.
pub fn drag(waypoints: &[Point], shift: bool, hold: Duration) -> Result<(), String> {
    let (Some(&first), Some(&last)) = (waypoints.first(), waypoints.last()) else {
        return Err("a drag needs points".into());
    };
    let mut path = vec![first];
    for pair in waypoints.windows(2) {
        path.extend(segment(pair[0], pair[1], 4.0).into_iter().skip(1));
    }
    path_is_safe(&path, &test_pids())?;
    let back = pointer_home();
    post_mouse(CGEventType::LeftMouseDown, first, shift)?;
    std::thread::sleep(Duration::from_millis(120));
    for p in path.iter().skip(1) {
        post_mouse(CGEventType::LeftMouseDragged, *p, shift)?;
        std::thread::sleep(Duration::from_millis(8));
    }
    std::thread::sleep(hold);
    post_mouse(CGEventType::LeftMouseUp, last, shift)?;
    std::thread::sleep(Duration::from_millis(80));
    if shift {
        release_shift();
    }
    if let Some(back) = back {
        let _ = CGWarpMouseCursorPosition(back);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers { shift: false, cmd: false };
    const SHIFT: Modifiers = Modifiers { shift: true, cmd: false };
    const CMD: Modifiers = Modifiers { shift: false, cmd: true };
    const CMD_SHIFT: Modifiers = Modifiers { shift: true, cmd: true };

    #[test]
    fn the_release_of_a_used_shift_click_does_not_re_arm_it() {
        let t0 = Instant::now();
        let mut t = Triggers::default();
        t.record(t0, SHIFT); // Shift-click in the Dock: down
        assert!(t.shift_here(t0 + Duration::from_millis(50), ACTIVATION_WINDOW, NONE));
        t.consume(); // the app's windows came here
        t.record_release(t0 + Duration::from_millis(100), SHIFT); // and up
        assert!(
            !t.shift_here(t0 + Duration::from_millis(900), ACTIVATION_WINDOW, NONE),
            "another app activating on its own is not pulled here"
        );
        // A new press counts again.
        t.record(t0 + Duration::from_millis(1000), SHIFT);
        assert!(t.shift_here(t0 + Duration::from_millis(1050), ACTIVATION_WINDOW, NONE));
    }

    #[test]
    fn a_recent_shift_trigger_counts_once() {
        let t0 = Instant::now();
        let mut t = Triggers::default();
        assert!(!t.shift_here(t0, LAUNCH_WINDOW, NONE));
        t.record(t0, SHIFT);
        assert!(t.shift_here(t0 + Duration::from_secs(3), LAUNCH_WINDOW, NONE));
        assert!(!t.shift_here(t0 + Duration::from_secs(3), ACTIVATION_WINDOW, NONE), "too late for an activation");
        assert!(!t.shift_here(t0 + Duration::from_secs(11), LAUNCH_WINDOW, NONE), "too late for a launch");
        t.consume();
        assert!(!t.shift_here(t0 + Duration::from_secs(1), LAUNCH_WINDOW, NONE), "used");
    }

    #[test]
    fn a_later_trigger_without_shift_cancels() {
        let t0 = Instant::now();
        let mut t = Triggers::default();
        t.record(t0, SHIFT);
        t.record(t0 + Duration::from_millis(500), NONE);
        assert!(!t.shift_here(t0 + Duration::from_secs(1), LAUNCH_WINDOW, NONE));
        assert!(t.shift_here(t0 + Duration::from_secs(1), LAUNCH_WINDOW, SHIFT), "held now");
    }

    #[test]
    fn the_app_switcher_never_brings_here() {
        let t0 = Instant::now();
        let mut t = Triggers::default();
        // ⌘⇧Tab: the Tab key-down carries Command and Shift.
        t.record(t0, CMD_SHIFT);
        assert!(!t.shift_here(t0 + Duration::from_millis(300), ACTIVATION_WINDOW, SHIFT));
        // ⌘Tab released while Shift is still down.
        t.record(t0, CMD);
        assert!(!t.shift_here(t0 + Duration::from_millis(300), ACTIVATION_WINDOW, SHIFT));
        // Long after, the live state decides alone.
        assert!(t.shift_here(t0 + Duration::from_secs(30), ACTIVATION_WINDOW, SHIFT));
        assert!(!t.shift_here(t0 + Duration::from_secs(30), ACTIVATION_WINDOW, CMD_SHIFT));
    }

    fn event(kind: InputKind, cmd: bool, clicks: i64, key: Option<u16>) -> InputEvent {
        InputEvent { kind, at: Point::new(0.0, 0.0), shift: false, cmd, clicks, key }
    }

    #[test]
    fn only_the_switchers_count_as_switcher_keys() {
        assert!(event(InputKind::KeyDown, true, 0, Some(KEY_TAB)).is_switcher_key(), "⌘Tab");
        assert!(event(InputKind::KeyDown, true, 0, Some(KEY_GRAVE)).is_switcher_key(), "⌘`");
        assert!(!event(InputKind::KeyDown, false, 0, Some(KEY_TAB)).is_switcher_key(), "Tab alone");
        assert!(!event(InputKind::KeyDown, true, 0, Some(8)).is_switcher_key(), "⌘C");
        assert!(!event(InputKind::KeyDown, true, 0, Some(124)).is_switcher_key(), "⌘→");
        assert!(!event(InputKind::MouseDown, true, 1, None).is_switcher_key());
    }

    #[test]
    fn segments_cover_the_path() {
        let s = segment(Point::new(0.0, 0.0), Point::new(10.0, 0.0), 4.0);
        assert_eq!(s.first(), Some(&Point::new(0.0, 0.0)));
        assert_eq!(s.last(), Some(&Point::new(10.0, 0.0)));
        assert!(s.windows(2).all(|p| (p[1].x - p[0].x) <= 4.0));
    }
}
