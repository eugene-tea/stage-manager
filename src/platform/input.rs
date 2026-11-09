//! Global input: one `NSEvent` global monitor for left mouse down/up, right and other mouse
//! down and key down in other apps, the live modifier and button state, and the Shift sugar's
//! trigger record.
//!
//! - The monitor (main thread) reports every left mouse-down (wallpaper clicks, the start of
//!   a window drag), left mouse-up (the end of a window drag), right or other button press
//!   (a user action: it ends the login's settling, nothing more) and key-down, each with the
//!   Shift and Command flags the user held. Global monitors never see our own windows'
//!   events.
//! - [`Triggers`] remembers the latest of those user actions, so a window or an activation
//!   that arrives a little later can tell whether the user held Shift when they caused it
//!   ("hold Shift to bring things here"). A trigger with Command held is the app switcher
//!   (⌘Tab, ⌘⇧Tab): Shift there means "cycle backward", never "bring here".
//! - Test support: synthetic clicks and drags posted only where every point is over our own
//!   windows or a window of an app on the `STAGE_MANAGER_ONLY` list. A synthetic drag stops,
//!   and says so, when the user's own mouse or trackpad lets the button go under it
//!   ([`drag`]).

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
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
use crate::ui::strip_model;
use crate::engine::geometry::{Point, Rect};

/// What the user did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// Left mouse button pressed.
    MouseDown,
    /// Left mouse button released.
    MouseUp,
    /// The right or another (middle, side) mouse button pressed: a user action, nothing
    /// else (no drag, no wallpaper or Dock click).
    OtherMouseDown,
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
    /// Option or Control held (a Dock click with either is no plain activation).
    pub other_modifiers: bool,
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

/// Calls `sink` (main thread) for every left mouse down/up, right or other mouse down and key
/// down in another app, and `moved` with the pointer (global, top-left) for every mouse move
/// over another app's window or the desktop.
pub fn monitor(mtm: MainThreadMarker, sink: impl Fn(InputEvent) + 'static, moved: impl Fn(Point) + 'static) -> Option<InputMonitor> {
    let block = RcBlock::new(move |event: NonNull<NSEvent>| {
        // SAFETY: AppKit passes a live event for the duration of the block.
        let event = unsafe { event.as_ref() };
        if event.r#type() == NSEventType::MouseMoved {
            let at = NSEvent::mouseLocation();
            moved(point_to_top_left(Point::new(at.x, at.y), primary_height(mtm)));
            return;
        }
        let Some(kind) = kind_of(event.r#type()) else { return };
        if kind == InputKind::MouseUp {
            LEFT_UPS.fetch_add(1, Ordering::Relaxed);
        }
        let flags = event.modifierFlags();
        let at = NSEvent::mouseLocation();
        // clickCount and keyCode raise on the other kind of event: ask each only its own.
        let (clicks, key) = match kind {
            InputKind::KeyDown => (0, Some(event.keyCode())),
            InputKind::MouseDown | InputKind::MouseUp | InputKind::OtherMouseDown => (event.clickCount() as i64, None),
        };
        sink(InputEvent {
            kind,
            at: point_to_top_left(Point::new(at.x, at.y), primary_height(mtm)),
            shift: flags.contains(NSEventModifierFlags::Shift),
            cmd: flags.contains(NSEventModifierFlags::Command),
            other_modifiers: flags.intersects(NSEventModifierFlags::Option | NSEventModifierFlags::Control),
            clicks,
            key,
        });
    });
    let mask = NSEventMask::LeftMouseDown
        | NSEventMask::LeftMouseUp
        | NSEventMask::RightMouseDown
        | NSEventMask::OtherMouseDown
        | NSEventMask::KeyDown
        | NSEventMask::MouseMoved;
    let token = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &block)?;
    Some(InputMonitor { token })
}

/// True when the monitor is to be made again: Accessibility was just granted (`trusted`, not
/// `was_trusted`). A global monitor hears key-downs only from a trusted process, and one made
/// before the grant may stay deaf to them until a relaunch (the review's K9).
pub fn monitor_again(was_trusted: bool, trusted: bool) -> bool {
    trusted && !was_trusted
}

/// What an event the monitor sees is to us (`None`: not reported).
fn kind_of(t: NSEventType) -> Option<InputKind> {
    match t {
        NSEventType::LeftMouseDown => Some(InputKind::MouseDown),
        NSEventType::LeftMouseUp => Some(InputKind::MouseUp),
        NSEventType::RightMouseDown | NSEventType::OtherMouseDown => Some(InputKind::OtherMouseDown),
        NSEventType::KeyDown => Some(InputKind::KeyDown),
        _ => None,
    }
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

/// Since the last keyboard, mouse or trackpad event in this login session (any thread).
pub fn since_last_input() -> Duration {
    // kCGAnyInputEventType is all ones.
    let secs = CGEventSource::seconds_since_last_event_type(CGEventSourceStateID::CombinedSessionState, CGEventType(u32::MAX));
    Duration::try_from_secs_f64(secs).unwrap_or(Duration::ZERO)
}

/// Since the user last pressed a mouse button in this login session (any thread): unlike
/// [`since_last_input`], a pointer move, a scroll or a key does not count.
pub fn since_last_click() -> Duration {
    [CGEventType::LeftMouseDown, CGEventType::RightMouseDown, CGEventType::OtherMouseDown]
        .into_iter()
        .map(|kind| CGEventSource::seconds_since_last_event_type(CGEventSourceStateID::CombinedSessionState, kind))
        .filter_map(|secs| Duration::try_from_secs_f64(secs).ok())
        .min()
        .unwrap_or(Duration::MAX)
}

/// Since the user last pressed a mouse button or a key in this login session (any thread):
/// a pointer move or a scroll does not count.
pub fn since_last_press() -> Duration {
    let key = CGEventSource::seconds_since_last_event_type(CGEventSourceStateID::CombinedSessionState, CGEventType::KeyDown);
    let key = Duration::try_from_secs_f64(key).unwrap_or(Duration::MAX);
    since_last_click().min(key)
}

/// Left mouse-ups the monitor has seen (another app's, any source): a synthetic drag posts
/// none until its own release, so one seen while it runs is the user's ([`drag`]).
static LEFT_UPS: AtomicU64 = AtomicU64::new(0);

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

    /// When the latest user action came (a mouse down or up, a key down in another app).
    pub fn last_at(&self) -> Option<Instant> {
        self.last.map(|(at, _)| at)
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

/// Checks that the topmost window at every point belongs to one of `pids` (for a `press`,
/// the first point's strip pane counts only on a card).
pub fn path_is_safe(path: &[Point], pids: &[i32], press: bool) -> Result<(), String> {
    let list = windows::list_windows();
    let own = std::process::id() as i32;
    // Every screen's strip: off the main one's cards, another screen's pane read as
    // click-through and its card's press was refused (round displays-live-1).
    let cards: Vec<Rect> = strip_model::every_published_card().into_iter().map(|(_, r)| r).collect();
    // The Dock draws its bar in one window as large as its display, clear and click-through
    // away from the bar (engine::sliver::DockBar; with auto-hide on, seen 11-05 at 2560x1440):
    // it counts only on the bar's own frame, read through Accessibility, and wherever that
    // frame is unknown. macOS's click-through overlays (the screenshot tool's, Notification
    // Center's full-screen layer: desktop::seen_through) never count.
    let dock = std::cell::OnceCell::new();
    let dock_bar = || *dock.get_or_init(|| super::dock::dock_pid().map(|pid| (pid, super::dock::bar_frame(pid))));
    let through = |w: &windows::CgWindow, p: Point| {
        let clear_dock = w.layer == super::dock::DOCK_LAYER
            && dock_bar().is_some_and(|(pid, bar)| pid == w.pid && bar.is_some_and(|b| !b.contains(p)));
        clear_dock || super::desktop::seen_through(w)
    };
    path_refusal(&list, path, pids, press, own, &cards, &through).map_or(Ok(()), Err)
}

/// Why a synthetic input along `path` is refused over `list` (front to back), or `None`
/// when every point is safe ([`path_is_safe`]): `own` our pid, `cards` every screen's cards,
/// `through` true for another process's window seen through at a point (the Dock's away
/// from its bar, macOS's click-through overlays; asked only of a window under the point).
///
/// After a drag's press the bare desktop (no window above the desktop level) is safe: the
/// moves and the release go to the window pressed, and nothing of anyone's lies there. A
/// card dragged onto another screen crosses that screen's desktop to its middle (11-09:
/// the behave phase's drag was refused unposted at the first point past the strip).
fn path_refusal(
    list: &[windows::CgWindow],
    path: &[Point],
    pids: &[i32],
    press: bool,
    own: i32,
    cards: &[Rect],
    through: &dyn Fn(&windows::CgWindow, Point) -> bool,
) -> Option<String> {
    for (i, p) in path.iter().enumerate() {
        // Our own strip sits below normal windows (layer -1): it counts where nothing covers
        // it, and for the press only on a card: off the cards its pane lets clicks through
        // (R1-ui 8), so what lies below must be safe (a drag stays with the window it was
        // pressed in). Our click-through windows above the floating level (card ghosts, the
        // motion overlay) never take a click, so they never count.
        let on_card = !press || i > 0 || cards.iter().any(|r| r.contains(*p));
        let top = list.iter().find(|w| {
            let click_through = w.pid == own && (w.layer > CLICKABLE_LEVEL || (w.bounds.w > TAB_MAX_WIDTH && !on_card));
            w.onscreen
                && !click_through
                && (w.layer >= 0 || pids.contains(&w.pid))
                && w.alpha > 0.0
                && w.bounds.contains(*p)
                && (w.pid == own || pids.contains(&w.pid) || !through(w, *p))
        });
        match top {
            Some(w) if pids.contains(&w.pid) => {}
            Some(w) => {
                return Some(format!(
                    "the topmost window at {:.0},{:.0} is not ours or a fixture's (pid {}) at layer {}, {}",
                    p.x, p.y, w.pid, w.layer, w.bounds
                ));
            }
            None if press && i > 0 => {}
            None => {
                // Say where our own windows are: the strip panel missing from the screen there
                // (ordered out, on another Space, elsewhere) is the usual story.
                let ours: Vec<String> = list
                    .iter()
                    .filter(|w| w.pid == own)
                    .map(|w| format!("layer {} {} {}", w.layer, w.bounds, if w.onscreen { "on screen" } else { "off screen" }))
                    .collect();
                return Some(format!("no window at {:.0},{:.0} (our windows: {})", p.x, p.y, ours.join("; ")));
            }
        }
    }
    None
}

/// The highest level of our own windows that take clicks (the strip lifted, the edge tab).
const CLICKABLE_LEVEL: i64 = 3;

/// Our windows at most this wide are the edge tab (it takes every click on it); wider ones
/// are strip panes (they take clicks on a card only).
const TAB_MAX_WIDTH: f64 = 12.0;

/// Moves the pointer to `p` before a synthetic press: a person's pointer is on the card
/// before the click, and the strip's pane takes the mouse only once the pointer is on a card
/// (it follows the pointer's moves).
fn move_first(p: Point, shift: bool) -> Result<(), String> {
    post_mouse(CGEventType::MouseMoved, p, shift)?;
    std::thread::sleep(Duration::from_millis(80));
    Ok(())
}

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
    post_mouse_clicks(kind, p, shift, 1)
}

/// A mouse event at `p`, the `clicks`th click of a multiple click.
fn post_mouse_clicks(kind: CGEventType, p: Point, shift: bool, clicks: i64) -> Result<(), String> {
    let at = CGPoint { x: p.x, y: p.y };
    let event = CGEvent::new_mouse_event(None, kind, at, CGMouseButton::Left).ok_or("cannot create a mouse event")?;
    CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventClickState, clicks);
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
    path_is_safe(&[p], &test_pids(), false)?;
    if let Ok(mut home) = HOVER_HOME.lock()
        && home.is_none()
    {
        *home = pointer_now().map(|c| (c.x, c.y));
    }
    post_mouse(CGEventType::MouseMoved, p, false)?;
    std::thread::sleep(rest);
    Ok(())
}

/// A synthetic pointer move to `p` on a screen edge, no button (test only): it shows an
/// auto-hidden Dock there. Moves only: nothing is clicked or dragged. [`pointer_back`] puts
/// the pointer where it was.
pub fn reveal_edge(p: Point, rest: Duration) -> Result<(), String> {
    if let Ok(mut home) = HOVER_HOME.lock()
        && home.is_none()
    {
        *home = pointer_now().map(|c| (c.x, c.y));
    }
    post_mouse(CGEventType::MouseMoved, p, false)?;
    std::thread::sleep(rest);
    Ok(())
}

/// Puts the pointer back where it was before [`reveal_edge`] or [`hover`] moved it.
pub fn pointer_back() {
    if let Some(home) = HOVER_HOME.lock().ok().and_then(|mut h| h.take()) {
        let _ = post_mouse(CGEventType::MouseMoved, Point::new(home.0, home.1), false);
    }
}

/// A synthetic click at `p`, only where the topmost window is ours or a fixture's. The
/// pointer goes back where it was.
pub fn click(p: Point, shift: bool) -> Result<(), String> {
    path_is_safe(&[p], &test_pids(), true)?;
    let back = pointer_home();
    move_first(p, shift)?;
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
///
/// The user's own mouse or trackpad may end it: the window server takes its release
/// mid-path as the drag's end (the monitor hears a left mouse-up), and the window stays where
/// the pointer was then (11-06, a live run with the owner at his Mac: Fixture 5 left at
/// x -39, short of the strip, and the drop's checks failed). Then nothing more is posted,
/// the pointer is left to him, and the error starts with [`DRAG_INTERRUPTED`].
pub fn drag(waypoints: &[Point], shift: bool, hold: Duration) -> Result<(), String> {
    let (Some(&first), Some(&last)) = (waypoints.first(), waypoints.last()) else {
        return Err("a drag needs points".into());
    };
    let mut path = vec![first];
    for pair in waypoints.windows(2) {
        path.extend(segment(pair[0], pair[1], 4.0).into_iter().skip(1));
    }
    path_is_safe(&path, &test_pids(), true)?;
    let back = pointer_home();
    move_first(first, shift)?;
    post_mouse(CGEventType::LeftMouseDown, first, shift)?;
    std::thread::sleep(Duration::from_millis(120));
    let ups = LEFT_UPS.load(Ordering::Relaxed);
    let interrupted = |posted: usize| {
        let why = drag_interrupted(ups, LEFT_UPS.load(Ordering::Relaxed), left_button_down())?;
        if shift {
            release_shift();
        }
        Some(Err(format!("{DRAG_INTERRUPTED}: {why} after {posted} of {} points; nothing more posted", path.len())))
    };
    for (i, p) in path.iter().enumerate().skip(1) {
        if let Some(e) = interrupted(i) {
            return e;
        }
        post_mouse(CGEventType::LeftMouseDragged, *p, shift)?;
        std::thread::sleep(Duration::from_millis(8));
    }
    std::thread::sleep(hold);
    if let Some(e) = interrupted(path.len()) {
        return e;
    }
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

/// The start of a synthetic drag's error when the user's input ended it (the live checks
/// look for it).
pub const DRAG_INTERRUPTED: &str = "the drag was interrupted by the user's own input";

/// Why a synthetic drag holding the button is no longer one (`None`: it still is): a left
/// mouse-up seen since its press (`ups_at_press`, `ups_now`: [`LEFT_UPS`]), or the button
/// up in the session's state.
fn drag_interrupted(ups_at_press: u64, ups_now: u64, button_down: bool) -> Option<&'static str> {
    if ups_now != ups_at_press {
        Some("a left mouse-up came")
    } else if !button_down {
        Some("the left button is up")
    } else {
        None
    }
}

/// Whether only the desktop lies at `p` in `list` (front to back): the topmost window there,
/// our click-through ones (the corner patch, the strip's pane off its cards, the overlay) and
/// transparent ones seen through, is a desktop-level window of another process (Finder's
/// icons, the wallpaper), or the window macOS's Show Desktop puts over the revealed desktop
/// (WindowManager's, [`crate::engine::overview::SHOW_DESKTOP_LAYER`]: a click there brings
/// the windows back). macOS's click-through overlays (`through`: [`super::desktop::seen_through`])
/// are seen through as well. The test's desktop clicks ([`desktop_click`], [`desktop_drag`])
/// go only there.
fn desktop_at(list: &[windows::CgWindow], p: Point, own: i32, through: &dyn Fn(&windows::CgWindow) -> bool) -> Result<(), String> {
    let top = list.iter().find(|w| {
        let click_through = w.pid == own && (w.layer > CLICKABLE_LEVEL || w.bounds.w > TAB_MAX_WIDTH);
        w.onscreen && !click_through && w.alpha > 0.0 && w.bounds.contains(p) && (w.pid == own || !through(w))
    });
    match top {
        Some(w) if w.layer < 0 && w.pid != own => Ok(()),
        Some(w) if w.owner == "WindowManager" && w.layer == crate::engine::overview::SHOW_DESKTOP_LAYER => Ok(()),
        Some(w) => Err(format!("the topmost window at {:.0},{:.0} is not the desktop (pid {}, layer {})", p.x, p.y, w.pid, w.layer)),
        None => Err(format!("no desktop window at {:.0},{:.0}", p.x, p.y)),
    }
}

/// A synthetic click (or double click) at `p` on the desktop itself (test only: a fixture's
/// desktop icon under the corner patch), only where nothing but the desktop lies
/// ([`desktop_at`]). The pointer goes back where it was.
pub fn desktop_click(p: Point, double: bool) -> Result<(), String> {
    let own = std::process::id() as i32;
    desktop_at(&windows::list_windows(), p, own, &super::desktop::seen_through)?;
    let back = pointer_home();
    move_first(p, false)?;
    for clicks in 1..=if double { 2 } else { 1 } {
        for kind in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
            post_mouse_clicks(kind, p, false, clicks)?;
            std::thread::sleep(Duration::from_millis(40));
        }
    }
    if let Some(back) = back {
        let _ = CGWarpMouseCursorPosition(back);
    }
    Ok(())
}

/// A synthetic drag on the desktop from `from` to `to` (test only: a fixture's desktop icon
/// dragged out of the corner), only when every point it passes lies on the desktop itself
/// ([`desktop_at`], checked before anything is posted). The pointer goes back where it was.
pub fn desktop_drag(from: Point, to: Point) -> Result<(), String> {
    let own = std::process::id() as i32;
    let list = windows::list_windows();
    let path = segment(from, to, 4.0);
    for p in &path {
        desktop_at(&list, *p, own, &super::desktop::seen_through)?;
    }
    let back = pointer_home();
    move_first(from, false)?;
    post_mouse(CGEventType::LeftMouseDown, from, false)?;
    std::thread::sleep(Duration::from_millis(150));
    for p in path.iter().skip(1) {
        post_mouse(CGEventType::LeftMouseDragged, *p, false)?;
        std::thread::sleep(Duration::from_millis(8));
    }
    std::thread::sleep(Duration::from_millis(200));
    post_mouse(CGEventType::LeftMouseUp, to, false)?;
    std::thread::sleep(Duration::from_millis(80));
    if let Some(back) = back {
        let _ = CGWarpMouseCursorPosition(back);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_desktop_click_goes_only_where_nothing_but_the_desktop_lies() {
        use crate::engine::geometry::Rect;
        let own = 7;
        let w = |pid: i32, layer: i64, bounds: Rect| windows::CgWindow { id: 1, pid, owner: String::new(), name: None, layer, bounds, onscreen: true, alpha: 1.0 };
        let screen = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let icons = w(300, -2_147_483_603, screen);
        let patch = w(own, 0, Rect::new(2440.0, 1240.0, 120.0, 200.0));
        let app = w(55, 0, Rect::new(100.0, 100.0, 800.0, 600.0));
        let tab = w(own, 0, Rect::new(0.0, 600.0, 8.0, 80.0));
        let list = [app.clone(), tab, patch, icons.clone()];
        let no_overlay = |_: &windows::CgWindow| false;
        assert_eq!(desktop_at(&list, Point::new(2490.0, 1380.0), own, &no_overlay), Ok(()), "under the patch: the icon");
        assert!(desktop_at(&list, Point::new(200.0, 200.0), own, &no_overlay).is_err(), "an app's window");
        assert!(desktop_at(&list, Point::new(4.0, 620.0), own, &no_overlay).is_err(), "our edge tab takes clicks");
        assert!(desktop_at(std::slice::from_ref(&app), Point::new(2490.0, 1380.0), own, &no_overlay).is_err(), "nothing listed there");
        // Show Desktop's own window over the revealed desktop: a click there brings the
        // windows back.
        let mut reveal = w(400, crate::engine::overview::SHOW_DESKTOP_LAYER, screen);
        reveal.owner = "WindowManager".into();
        assert_eq!(desktop_at(&[reveal.clone(), icons.clone()], Point::new(1200.0, 700.0), own, &no_overlay), Ok(()));
        let mut other = reveal;
        other.owner = "Dock".into();
        assert!(desktop_at(&[other, icons], Point::new(1200.0, 700.0), own, &no_overlay).is_err(), "only WindowManager's");
    }

    #[test]
    fn a_drag_may_cross_the_bare_desktop_after_its_press() {
        let (own, fixture, owner) = (7, 70, 55);
        let w = |pid: i32, layer: i64, bounds: Rect| windows::CgWindow { id: 1, pid, owner: String::new(), name: None, layer, bounds, onscreen: true, alpha: 1.0 };
        // The 5K and the laptop left of it, a card on the 5K's strip, the laptop's desktop
        // bare, an owner's window low on the laptop.
        let strip = w(own, -1, Rect::new(0.0, 30.0, 220.0, 1410.0));
        let wallpaper = w(300, -2_147_483_624, Rect::new(-1800.0, 271.0, 1800.0, 1169.0));
        let mail = w(owner, 0, Rect::new(-1700.0, 1000.0, 800.0, 400.0));
        let list = [mail, strip, wallpaper];
        let card = [Rect::new(15.0, 686.0, 88.0, 97.0)];
        let no_dock = |_: &windows::CgWindow, _: Point| false;
        let pids = [own, fixture];
        let path = |to: Point| {
            let mut p = vec![Point::new(55.0, 739.0), Point::new(55.0, 751.0)];
            p.extend(segment(Point::new(55.0, 751.0), to, 4.0).into_iter().skip(1));
            p
        };
        let across = path(Point::new(-900.0, 687.0));
        assert_eq!(path_refusal(&list, &across, &pids, true, own, &card, &no_dock), None, "the card onto the laptop's middle");
        let onto_mail = path(Point::new(-1300.0, 1200.0));
        assert!(path_refusal(&list, &onto_mail, &pids, true, own, &card, &no_dock).is_some_and(|e| e.contains("not ours")), "an owner's window still refuses");
        let off_card = [Point::new(-900.0, 687.0), Point::new(-880.0, 687.0)];
        assert!(path_refusal(&list, &off_card, &pids, true, own, &card, &no_dock).is_some(), "the press itself is never on the bare desktop");
        assert!(path_refusal(&list, &across[1..], &pids, false, own, &card, &no_dock).is_some(), "a hover never is either");
    }

    /// 11-09 14:45: Notification Center's click-through window over the whole main screen
    /// (layer 23) refused every card click of the suite. Seen through now; one of its banners,
    /// or an unknown app's full-screen window, still refuses.
    #[test]
    fn notification_centers_full_screen_layer_is_looked_through_a_banner_is_not() {
        use crate::platform::desktop::{NOTIFICATION_CENTER_UI, Owner, click_through_overlay};
        let (own, fixture, nc_pid) = (7, 70, 894);
        let main = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let w = |pid: i32, layer: i64, bounds: Rect| windows::CgWindow { id: 1, pid, owner: String::new(), name: None, layer, bounds, onscreen: true, alpha: 1.0 };
        // Notification Center's windows, its identity as `desktop::seen_through` reads it.
        let nc = |bounds: Rect| windows::CgWindow { owner: "Notification Center".into(), ..w(nc_pid, 23, bounds) };
        let through = |c: &windows::CgWindow| {
            let bundle = (c.pid == nc_pid).then_some(NOTIFICATION_CENTER_UI);
            click_through_overlay(c.layer, c.bounds, &Owner { name: &c.owner, bundle, exe: None }, &[main])
        };
        let on_path = |c: &windows::CgWindow, _: Point| through(c);
        let strip = w(own, -1, Rect::new(0.0, 30.0, 220.0, 1410.0));
        let fix = w(fixture, 0, Rect::new(400.0, 78.0, 600.0, 432.0));
        let card = [Rect::new(15.0, 686.0, 88.0, 97.0)];
        let pids = [own, fixture];
        let click = [Point::new(66.0, 739.0)];
        let list = [nc(main), strip.clone(), fix.clone()];
        assert_eq!(path_refusal(&list, &click, &pids, true, own, &card, &on_path), None, "a card click under the full-screen layer");
        let into_fixture = [Point::new(700.0, 300.0)];
        assert_eq!(path_refusal(&list, &into_fixture, &pids, true, own, &card, &on_path), None, "a fixture's window under it");
        // A banner at the top right still refuses a click on it.
        let banner = Rect::new(2190.0, 38.0, 360.0, 76.0);
        let list = [nc(banner), nc(main), strip.clone(), fix];
        let on_banner = [Point::new(2300.0, 70.0)];
        let refused = path_refusal(&list, &on_banner, &pids, true, own, &card, &on_path);
        assert!(refused.is_some_and(|e| e.contains("not ours or a fixture's (pid 894)")), "the banner's click");
        assert_eq!(path_refusal(&list, &click, &pids, true, own, &card, &on_path), None, "away from the banner");
        // An unknown app's full-screen window refuses.
        let stranger = windows::CgWindow { owner: "Overlay".into(), ..w(55, 23, main) };
        assert!(path_refusal(&[stranger.clone(), strip], &click, &pids, true, own, &card, &on_path).is_some(), "an unknown overlay");
        // The test's desktop clicks look through it to the desktop, not through a stranger's.
        let icons = w(300, -2_147_483_603, main);
        assert_eq!(desktop_at(&[nc(main), icons.clone()], Point::new(2362.0, 480.0), own, &through), Ok(()));
        assert!(desktop_at(&[stranger, icons], Point::new(2362.0, 480.0), own, &through).is_err());
    }

    const NONE: Modifiers = Modifiers { shift: false, cmd: false };
    const SHIFT: Modifiers = Modifiers { shift: true, cmd: false };
    const CMD: Modifiers = Modifiers { shift: false, cmd: true };
    const CMD_SHIFT: Modifiers = Modifiers { shift: true, cmd: true };

    /// K9: the monitor is made again only when Accessibility is granted.
    #[test]
    fn the_monitor_is_made_again_at_the_grant() {
        assert!(monitor_again(false, true));
        assert!(!monitor_again(true, true));
        assert!(!monitor_again(true, false), "revoked: nothing to gain");
        assert!(!monitor_again(false, false));
    }

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
        InputEvent { kind, at: Point::new(0.0, 0.0), shift: false, cmd, other_modifiers: false, clicks, key }
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
    fn a_right_or_middle_click_is_a_user_action() {
        assert_eq!(kind_of(NSEventType::RightMouseDown), Some(InputKind::OtherMouseDown));
        assert_eq!(kind_of(NSEventType::OtherMouseDown), Some(InputKind::OtherMouseDown));
        assert_eq!(kind_of(NSEventType::LeftMouseDown), Some(InputKind::MouseDown));
        assert_eq!(kind_of(NSEventType::LeftMouseUp), Some(InputKind::MouseUp));
        assert_eq!(kind_of(NSEventType::KeyDown), Some(InputKind::KeyDown));
        assert_eq!(kind_of(NSEventType::RightMouseUp), None, "a release is not reported");
        assert_eq!(kind_of(NSEventType::MouseMoved), None, "moves go to the strip");
    }

    #[test]
    fn a_release_under_a_synthetic_drag_interrupts_it() {
        assert_eq!(drag_interrupted(4, 4, true), None, "held, no release: it goes on");
        assert!(drag_interrupted(4, 5, true).is_some(), "a release seen (the user's), the session's button down again");
        assert!(drag_interrupted(4, 4, false).is_some(), "the button up (a release the monitor missed)");
    }

    #[test]
    fn segments_cover_the_path() {
        let s = segment(Point::new(0.0, 0.0), Point::new(10.0, 0.0), 4.0);
        assert_eq!(s.first(), Some(&Point::new(0.0, 0.0)));
        assert_eq!(s.last(), Some(&Point::new(10.0, 0.0)));
        assert!(s.windows(2).all(|p| (p[1].x - p[0].x) <= 4.0));
    }
}
