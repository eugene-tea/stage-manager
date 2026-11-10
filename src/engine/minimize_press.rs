//! A minimize caught before macOS starts it (`[windows] minimize_to_strip`): the yellow
//! button pressed, or ⌘M, on a window Stage Manager manages. Apple's Stage Manager shows no
//! genie into the Dock (recorded 11-09 at 60 fps): the window flies straight from its frame
//! into a new card at the top of the strip, as a stage leaving in a switch does. Ours does the
//! same where it hears of the minimize first, in its event tap
//! ([`crate::platform::minimize_tap`]); this module is what the tap decides, pure.
//!
//! - **The yellow button.** A left press near the top-left corner of a managed window
//!   ([`Minimizable::may_hit`], rectangle tests only) is hit-tested through Accessibility at
//!   the press; when the element there is the minimize button of a managed window, enabled
//!   ([`judge_press`]), the press is swallowed, its drags too, and the release acts only when
//!   it lands on the same button, as the button's own tracking does: a press dragged off does
//!   nothing ([`ButtonGate`]). Every other press passes untouched.
//! - **⌘M** with no other modifier ([`is_cmd_m`]), when the frontmost app's focused window
//!   is managed, has an enabled minimize button and can be minimized (`AXMinimized`
//!   settable): swallowed, and the window goes to the strip ([`judge_key`]). A key held down
//!   repeats nothing. In any other window (a panel, a dialog, an app's own window kind with
//!   no minimize button) ⌘M is the app's.
//! - Anything not caught here (Window ▸ Minimize from the menu, a title-bar double-click,
//!   an app's own call, an answer that did not come in time) is macOS's minimize, genie and
//!   all, after which the window comes back to the strip as before ([`super::minimize`]).

use std::collections::HashMap;

use crate::engine::geometry::{Point, Rect};
use crate::engine::sliver::{DockBar, Listed};

/// The part of a window's frame, from its top-left corner, where its title-bar buttons may
/// be (points): the standard title bar's at about 8–62 × 4–18, a unified toolbar's lower
/// (Safari, Finder, 26's taller toolbars, an inset Electron title bar at about 20,20). Only
/// a press in here is hit-tested; Accessibility decides the rest.
pub const BUTTON_ZONE_W: f64 = 160.0;
/// See [`BUTTON_ZONE_W`].
pub const BUTTON_ZONE_H: f64 = 80.0;

/// The `M` key's virtual key code (ANSI): the key ⌘M is read by when the event carries no
/// character.
pub const KEY_M: u16 = 46;

/// The windows a minimize may be caught on, as the WM thread last published them: managed,
/// standard, shown in their stage on a shown Desktop, not pinned, with their frames
/// (global, top-left). `on`: `[windows] minimize_to_strip`, Stage Manager arranging (not
/// standing down, no logout coming, Accessibility on) and no stage change frozen (full
/// screen, a Desktop change, a locked screen).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Minimizable {
    /// Catching minimizes at all.
    pub on: bool,
    /// Window id → frame.
    pub windows: HashMap<u32, Rect>,
    /// The Dock's bar: its display-sized window is clear away from it ([`under_press`]).
    pub dock: DockBar,
}

impl Minimizable {
    /// Whether a press at `at` may be on a managed window's title-bar buttons: inside the
    /// top-left [`BUTTON_ZONE_W`] × [`BUTTON_ZONE_H`] of one of the windows (rectangle tests
    /// only: the tap asks nothing for any other press).
    pub fn may_hit(&self, at: Point) -> bool {
        self.on
            && self.windows.values().any(|f| {
                let zone = Rect::new(f.x, f.y, BUTTON_ZONE_W.min(f.w), BUTTON_ZONE_H.min(f.h));
                zone.contains(at)
            })
    }

    /// Whether window `id` is one a minimize is caught on.
    pub fn has(&self, id: u32) -> bool {
        self.on && self.windows.contains_key(&id)
    }
}

/// Whether a left press is a plain click, the only kind caught: ⌥-click on the yellow button
/// minimizes every window of the app, ⇧-click plays the genie slowly and ⌃-click is a
/// secondary click: those are macOS's (⌘ changes nothing on the button).
pub fn plain_click(shift: bool, option: bool, control: bool) -> bool {
    !shift && !option && !control
}

/// What lies topmost under a press near a managed window's corner, from the window server's
/// on-screen list ([`under_press`]): only a managed window there is hit-tested, and through
/// its own app's element. Stage Manager's own windows are never asked through Accessibility
/// (an Accessibility call into our own process from the tap answers only when our main thread
/// gets to it, while every input event of the session waits on the tap).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Under {
    /// A window a minimize is caught on: hit-test its app.
    Managed {
        /// Its `CGWindowID`.
        id: u32,
        /// Its app.
        pid: i32,
    },
    /// One of Stage Manager's own windows takes the press (a card, the edge tab, Settings).
    Ours(Listed),
    /// Another window (`None`: none at all).
    Other(Option<Listed>),
}

/// The highest window layer of ours that takes clicks (the strip lifted, the edge tab):
/// above it our windows are click-through (a dragged card's ghost, the motion overlay raised).
pub const OWN_CLICKABLE_LEVEL: i64 = 3;
/// The motion overlay's layer (`NSFloatingWindowLevel - 1`): click-through.
pub const OWN_OVERLAY_LEVEL: i64 = 2;
/// Our windows at most this wide are the edge tab (it takes every click); wider ones at the
/// strip's levels are strip panes (they take clicks on a card only).
pub const OWN_TAB_MAX_WIDTH: f64 = 12.0;

/// What takes a press at `at`: the first window of `list` (front to back, on screen) under
/// the point that a click there does not go through, as [`crate::platform::desktop::is_wallpaper_click`]
/// decides what lies under a point: transparent and desktop-level windows are seen through,
/// and so is every window `through` says a click passes (the Dock's display-sized window away
/// from its bar, [`Minimizable::dock`]; macOS's click-through overlays: the screenshot tool's,
/// Notification Center's full-screen one, whose banners the platform checks after). Of ours
/// (`own`: our pid), the motion overlay and the windows above our clickable levels are
/// click-through, and a strip pane takes a press only on one of its `cards` (global); every
/// other window of ours takes it.
pub fn under_press(list: &[Listed], at: Point, own: i32, cards: &[Rect], set: &Minimizable, through: impl Fn(&Listed) -> bool) -> Under {
    for w in list {
        if !w.bounds.contains(at) || w.alpha <= 0.0 || (w.layer < 0 && w.pid != own) {
            continue;
        }
        if w.pid == own {
            let clear = w.layer > OWN_CLICKABLE_LEVEL || w.layer == OWN_OVERLAY_LEVEL;
            let pane = w.bounds.w > OWN_TAB_MAX_WIDTH && (w.layer == OWN_CLICKABLE_LEVEL || w.layer < 0);
            if clear || (pane && !cards.iter().any(|c| c.contains(at))) {
                continue;
            }
            return Under::Ours(*w);
        }
        if set.dock.seen_through(w.pid, w.layer, w.bounds, at) || through(w) {
            continue;
        }
        return if set.has(w.id) { Under::Managed { id: w.id, pid: w.pid } } else { Under::Other(Some(*w)) };
    }
    Under::Other(None)
}

/// What Accessibility says of the element under a press.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ButtonHit {
    /// The element is a window's minimize button (`AXMinimizeButton`).
    pub minimize_button: bool,
    /// It is enabled (`AXEnabled`; unknown: enabled).
    pub enabled: bool,
    /// The window it belongs to (`AXWindow`, its `CGWindowID`).
    pub window: Option<u32>,
    /// Its frame (global, top-left): the release must land inside it.
    pub frame: Option<Rect>,
}

/// Why a press, or ⌘M, is left to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// Not on a minimize button (the close or zoom button, a toolbar, the title).
    NotTheButton,
    /// The button or the window's minimize is disabled, or the window cannot be minimized.
    Disabled,
    /// The window is not one Stage Manager catches a minimize on (unmanaged, a panel or a
    /// sheet, pinned, parked, another app's of the list).
    NotManaged,
    /// Accessibility gave no window or no frame in time.
    Unknown,
    /// One of Stage Manager's own windows takes the press: never asked through Accessibility.
    Ours,
    /// Another window than a managed one is topmost there.
    Covered,
}

/// The press is on the enabled minimize button of `under` (the managed window topmost
/// there, [`under_press`]), a window in `set`: that window and the button's frame; else why
/// not.
pub fn judge_press(hit: &ButtonHit, set: &Minimizable, under: u32) -> Result<(u32, Rect), Pass> {
    if !hit.minimize_button {
        return Err(Pass::NotTheButton);
    }
    if !hit.enabled {
        return Err(Pass::Disabled);
    }
    let (Some(window), Some(frame)) = (hit.window, hit.frame) else { return Err(Pass::Unknown) };
    if frame.is_degenerate() {
        return Err(Pass::Unknown);
    }
    if !set.has(window) || window != under {
        return Err(Pass::NotManaged);
    }
    Ok((window, frame))
}

/// A left press on a minimize button, held: its window, the button's frame and what the
/// platform keeps with it (`H`: the window's Accessibility element).
#[derive(Debug, Clone, PartialEq)]
pub struct Pressed<H> {
    /// The window.
    pub window: u32,
    /// The button's frame.
    pub button: Rect,
    /// The platform's handle on the window.
    pub handle: H,
}

/// What a left-button event does under the gate.
#[derive(Debug, Clone, PartialEq)]
pub enum Gate<H> {
    /// It reaches the app.
    Pass,
    /// Swallowed; nothing happens.
    Swallow,
    /// Swallowed, and the button acts: the window goes to the strip.
    Minimize(Pressed<H>),
}

/// The left button's press, drag and release around the minimize button, as the button's own
/// tracking would take them: the press swallowed when [`judge_press`] says so, every drag
/// after it swallowed, and the release swallowed, acting only when it lands on the button.
#[derive(Debug, Clone, PartialEq)]
pub struct ButtonGate<H> {
    held: Option<Pressed<H>>,
}

impl<H> Default for ButtonGate<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H> ButtonGate<H> {
    /// No press held.
    pub const fn new() -> Self {
        Self { held: None }
    }
}

impl<H: Clone> ButtonGate<H> {
    /// A left press: `judge` (asked only now) says whether it is on the minimize button of a
    /// window caught. A press while one is held (its release never came to the tap) starts
    /// afresh.
    pub fn down(&mut self, judge: impl FnOnce() -> Option<Pressed<H>>) -> Gate<H> {
        self.held = judge();
        if self.held.is_some() { Gate::Swallow } else { Gate::Pass }
    }

    /// A left drag: swallowed while a press is held.
    pub fn dragged(&self) -> Gate<H> {
        if self.held.is_some() { Gate::Swallow } else { Gate::Pass }
    }

    /// The left release at `at`: swallowed when a press is held, acting when it lands on the
    /// button (dragged off and released elsewhere: nothing, as the button's own).
    pub fn up(&mut self, at: Point) -> Gate<H> {
        match self.held.take() {
            Some(p) if p.button.contains(at) => Gate::Minimize(p),
            Some(_) => Gate::Swallow,
            None => Gate::Pass,
        }
    }

    /// Whether a press is held.
    pub fn holding(&self) -> bool {
        self.held.is_some()
    }
}

/// A key-down as the tap reads it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KeyPress {
    /// Its virtual key code.
    pub keycode: u16,
    /// The characters it types, the keyboard layout's (empty: none read).
    pub chars: String,
    /// Command held.
    pub cmd: bool,
    /// Shift held.
    pub shift: bool,
    /// Option held.
    pub option: bool,
    /// Control held.
    pub control: bool,
    /// An auto-repeat of a key held down.
    pub repeat: bool,
}

/// Whether `k` is ⌘M with no other modifier (Caps Lock and Fn aside): by the character the
/// layout gives, else by the `M` key's code. ⌘⇧M, ⌥⌘M and ⌃⌘M are other shortcuts.
pub fn is_cmd_m(k: &KeyPress) -> bool {
    let m = if k.chars.is_empty() { k.keycode == KEY_M } else { k.chars.eq_ignore_ascii_case("m") };
    m && k.cmd && !k.shift && !k.option && !k.control
}

/// What Accessibility says of the frontmost app's focused window at ⌘M.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Focused {
    /// Its `CGWindowID` (`None`: no focused window, or no answer in time).
    pub window: Option<u32>,
    /// It has a minimize button (`AXMinimizeButton`), and it is enabled.
    pub button: bool,
    /// Its `AXMinimized` is settable.
    pub settable: bool,
}

/// What ⌘M does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyGate {
    /// It reaches the app.
    Pass(Pass),
    /// Swallowed; nothing happens (a repeat of a ⌘M already acted on).
    Swallow,
    /// Swallowed, and the window goes to the strip.
    Minimize(u32),
}

/// The tap's answer to a key-down `k` already known to be ⌘M ([`is_cmd_m`]), from the
/// focused window's facts (`focused`, asked only for ⌘M) and the windows caught.
pub fn judge_key(k: &KeyPress, focused: &Focused, set: &Minimizable) -> KeyGate {
    let Some(window) = focused.window else { return KeyGate::Pass(Pass::Unknown) };
    if !set.has(window) {
        return KeyGate::Pass(Pass::NotManaged);
    }
    if !focused.button || !focused.settable {
        return KeyGate::Pass(Pass::Disabled);
    }
    if k.repeat { KeyGate::Swallow } else { KeyGate::Minimize(window) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture window as on the owner's 5K, and its minimize button (a standard title bar).
    const FRAME: Rect = Rect::new(639.0, 300.0, 900.0, 632.0);
    const BUTTON: Rect = Rect::new(667.0, 305.0, 14.0, 16.0);

    fn set() -> Minimizable {
        Minimizable { on: true, windows: HashMap::from([(7, FRAME)]), dock: DockBar::default() }
    }

    fn hit(window: u32) -> ButtonHit {
        ButtonHit { minimize_button: true, enabled: true, window: Some(window), frame: Some(BUTTON) }
    }

    fn press(window: u32) -> Option<Pressed<u32>> {
        judge_press(&hit(window), &set(), window).ok().map(|(window, button)| Pressed { window, button, handle: window })
    }

    #[test]
    fn only_a_press_near_a_managed_windows_top_left_corner_is_asked_about() {
        let s = set();
        assert!(s.may_hit(BUTTON.center()), "the yellow button");
        assert!(s.may_hit(Point::new(FRAME.x + 30.0, FRAME.y + 40.0)), "a toolbar's lower buttons");
        assert!(!s.may_hit(Point::new(FRAME.x + 400.0, FRAME.y + 10.0)), "the title");
        assert!(!s.may_hit(Point::new(FRAME.x + 30.0, FRAME.y + 300.0)), "the window's content");
        assert!(!s.may_hit(Point::new(FRAME.x - 5.0, FRAME.y + 10.0)), "beside the window");
        assert!(!Minimizable { on: false, ..set() }.may_hit(BUTTON.center()), "off: nothing asked");
        assert!(!Minimizable::default().may_hit(BUTTON.center()), "no window published");
        // A small window: the zone never reaches past it.
        let small = Minimizable { on: true, windows: HashMap::from([(8, Rect::new(0.0, 0.0, 100.0, 50.0))]), ..Minimizable::default() };
        assert!(!small.may_hit(Point::new(120.0, 10.0)) && small.may_hit(Point::new(30.0, 10.0)));
    }

    #[test]
    fn only_a_plain_click_is_caught() {
        assert!(plain_click(false, false, false));
        assert!(!plain_click(false, true, false), "⌥-click: every window of the app, macOS's");
        assert!(!plain_click(true, false, false), "⇧-click: the slow genie, macOS's");
        assert!(!plain_click(false, false, true), "⌃-click: a secondary click");
    }

    #[test]
    fn a_press_on_a_managed_windows_minimize_button_is_caught() {
        assert_eq!(judge_press(&hit(7), &set(), 7), Ok((7, BUTTON)));
        assert_eq!(judge_press(&hit(7), &set(), 8), Err(Pass::NotManaged), "its app answered for another of its windows");
    }

    #[test]
    fn every_other_press_passes() {
        let s = set();
        let close = ButtonHit { minimize_button: false, ..hit(7) };
        assert_eq!(judge_press(&close, &s, 7), Err(Pass::NotTheButton), "the close or zoom button, a toolbar");
        assert_eq!(judge_press(&ButtonHit { enabled: false, ..hit(7) }, &s, 7), Err(Pass::Disabled), "a disabled button");
        assert_eq!(judge_press(&hit(9), &s, 7), Err(Pass::NotManaged), "an unmanaged window's button");
        assert_eq!(judge_press(&ButtonHit { window: None, ..hit(7) }, &s, 7), Err(Pass::Unknown), "no window read");
        assert_eq!(judge_press(&ButtonHit { frame: None, ..hit(7) }, &s, 7), Err(Pass::Unknown), "no frame read");
        assert_eq!(judge_press(&ButtonHit::default(), &s, 7), Err(Pass::NotTheButton), "no answer in time");
        assert_eq!(judge_press(&hit(7), &Minimizable { on: false, ..s }, 7), Err(Pass::NotManaged), "off");
    }

    /// A panel's or a sheet's minimize button (a utility window, a document sheet's parent
    /// is the window): only the windows the WM thread publishes, standard ones in a stage,
    /// are caught; a panel is never published.
    #[test]
    fn a_panels_button_passes() {
        let panel = 12;
        assert_eq!(judge_press(&hit(panel), &set(), panel), Err(Pass::NotManaged));
    }

    #[test]
    fn a_click_on_the_button_minimizes_on_its_release() {
        let mut g = ButtonGate::default();
        assert_eq!(g.down(|| press(7)), Gate::Swallow, "the press never reaches the app");
        assert_eq!(g.dragged(), Gate::Swallow, "a wobble on the button");
        let up = g.up(BUTTON.center());
        assert!(matches!(up, Gate::Minimize(Pressed { window: 7, .. })), "{up:?}");
        assert!(!g.holding());
    }

    #[test]
    fn a_press_dragged_off_the_button_does_nothing() {
        let mut g = ButtonGate::default();
        g.down(|| press(7));
        assert_eq!(g.dragged(), Gate::Swallow, "its drags are swallowed too: the window never moves under it");
        assert_eq!(g.up(Point::new(BUTTON.max_x() + 20.0, BUTTON.y)), Gate::Swallow, "released off the button");
        assert_eq!(g.up(BUTTON.center()), Gate::Pass, "nothing held any more");
    }

    #[test]
    fn presses_elsewhere_pass_with_their_drags_and_releases() {
        let mut g = ButtonGate::<u32>::default();
        assert_eq!(g.down(|| None), Gate::Pass);
        assert_eq!(g.dragged(), Gate::Pass, "a window dragged by its title bar");
        assert_eq!(g.up(BUTTON.center()), Gate::Pass, "a release over the button after a press elsewhere");
        // A release the tap never saw: the next press starts afresh.
        g.down(|| press(7));
        assert_eq!(g.down(|| None), Gate::Pass);
        assert!(!g.holding());
    }

    fn listed(id: u32, pid: i32, bounds: Rect, layer: i64) -> Listed {
        Listed { id, pid, bounds, alpha: 1.0, layer }
    }

    fn none(_: &Listed) -> bool {
        false
    }

    const OWN: i32 = 500;

    /// A press on a card of our strip, lifted to the floating level over a managed window's
    /// corner, is ours: our own process is never asked through Accessibility.
    #[test]
    fn a_press_our_own_window_takes_is_never_hit_tested() {
        let s = set();
        let at = BUTTON.center();
        let card = Rect::new(at.x - 20.0, at.y - 20.0, 120.0, 90.0);
        let pane = listed(90, OWN, Rect::new(0.0, 0.0, 700.0, 1440.0), OWN_CLICKABLE_LEVEL);
        let window = listed(7, 42, FRAME, 0);
        assert_eq!(under_press(&[pane, window], at, OWN, &[card], &s, none), Under::Ours(pane), "a card of the lifted strip");
        assert_eq!(under_press(&[pane, window], at, OWN, &[], &s, none), Under::Managed { id: 7, pid: 42 }, "the pane off its cards lets the press through");
        let below = listed(90, OWN, pane.bounds, -1);
        assert_eq!(under_press(&[window, below], at, OWN, &[card], &s, none), Under::Managed { id: 7, pid: 42 }, "the strip below the window");
        let overlay = listed(91, OWN, Rect::new(0.0, 0.0, 2560.0, 1440.0), OWN_OVERLAY_LEVEL);
        let ghost = listed(92, OWN, Rect::new(600.0, 280.0, 900.0, 640.0), 4);
        assert_eq!(under_press(&[ghost, overlay, window], at, OWN, &[], &s, none), Under::Managed { id: 7, pid: 42 }, "click-through: a ghost, the overlay");
        let settings = listed(93, OWN, Rect::new(600.0, 280.0, 700.0, 500.0), 0);
        assert_eq!(under_press(&[settings, window], at, OWN, &[], &s, none), Under::Ours(settings), "our Settings window over its corner");
        let tab = listed(94, OWN, Rect::new(at.x - 4.0, 0.0, 8.0, 1440.0), OWN_CLICKABLE_LEVEL);
        assert_eq!(under_press(&[tab, window], at, OWN, &[], &s, none), Under::Ours(tab), "the edge tab");
    }

    #[test]
    fn only_a_managed_window_topmost_there_is_hit_tested() {
        let s = set();
        let at = BUTTON.center();
        let window = listed(7, 42, FRAME, 0);
        let other = listed(9, 43, Rect::new(600.0, 290.0, 300.0, 200.0), 0);
        assert_eq!(under_press(&[other, window], at, OWN, &[], &s, none), Under::Other(Some(other)), "another app's window over its corner");
        let clear = Listed { alpha: 0.0, ..other };
        assert_eq!(under_press(&[clear, window], at, OWN, &[], &s, none), Under::Managed { id: 7, pid: 42 }, "a transparent one is seen through");
        let desktop = listed(1, 44, Rect::new(0.0, 0.0, 2560.0, 1440.0), -2_147_483_602);
        assert_eq!(under_press(&[desktop], at, OWN, &[], &s, none), Under::Other(None), "only the desktop");
        assert_eq!(under_press(&[listed(8, 42, FRAME, 0)], at, OWN, &[], &s, none), Under::Other(Some(listed(8, 42, FRAME, 0))), "a window not published");
        assert_eq!(under_press(&[], at, OWN, &[], &s, none), Under::Other(None), "nothing listed");
    }

    /// Live 11-09 (parity, the yellow button on Bravo 1 at 420,98): the press was passed as
    /// covered, the genie played. Above the window at its button lay what a click goes
    /// through: our motion overlay, the Dock's display-sized window away from its bar, and
    /// macOS's click-through overlays.
    #[test]
    fn what_a_click_goes_through_never_covers_the_button() {
        let at = Point::new(459.0, 114.0);
        let frame = Rect::new(420.0, 98.0, 640.0, 452.0);
        let screen = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let mut s = Minimizable { on: true, windows: HashMap::from([(7, frame)]), dock: DockBar::default() };
        let window = listed(7, 42, frame, 0);
        let overlay = listed(91, OWN, screen, OWN_OVERLAY_LEVEL);
        let dock = listed(5, 818, screen, crate::engine::overview::DOCK_LAYER);
        let nc = listed(6, 819, screen, 23);
        let managed = Under::Managed { id: 7, pid: 42 };
        assert_eq!(under_press(&[overlay, window], at, OWN, &[], &s, none), managed, "our overlay above it");
        // The Dock's window: seen through once its bar is known (away from the bar).
        assert_eq!(under_press(&[dock, overlay, window], at, OWN, &[], &s, none), Under::Other(Some(dock)), "the bar unknown: it counts, as for a wallpaper click");
        s.dock = DockBar { pid: Some(818), bars: vec![Rect::new(493.0, 1356.0, 1574.0, 84.0)] };
        assert_eq!(under_press(&[dock, overlay, window], at, OWN, &[], &s, none), managed, "away from the Dock's bar");
        // macOS's click-through overlays, as the platform tells them.
        assert_eq!(under_press(&[nc, dock, overlay, window], at, OWN, &[], &s, |w| w.id == 6), managed, "Notification Center's full-screen window");
        assert_eq!(under_press(&[nc, window], at, OWN, &[], &s, none), Under::Other(Some(nc)), "an overlay the platform does not see through");
    }

    fn key(chars: &str) -> KeyPress {
        KeyPress { keycode: KEY_M, chars: chars.into(), cmd: true, ..KeyPress::default() }
    }

    #[test]
    fn cmd_m_is_command_and_m_alone() {
        assert!(is_cmd_m(&key("m")));
        assert!(is_cmd_m(&key("M")), "Caps Lock on");
        assert!(is_cmd_m(&key("")), "no characters read: the M key");
        assert!(!is_cmd_m(&KeyPress { keycode: KEY_M, chars: ",".into(), cmd: true, ..KeyPress::default() }), "AZERTY's key there types ,");
        assert!(is_cmd_m(&KeyPress { keycode: 41, chars: "m".into(), cmd: true, ..KeyPress::default() }), "AZERTY's M key");
        assert!(!is_cmd_m(&KeyPress { cmd: false, ..key("m") }), "M alone");
        assert!(!is_cmd_m(&KeyPress { shift: true, ..key("m") }), "⌘⇧M");
        assert!(!is_cmd_m(&KeyPress { option: true, ..key("m") }), "⌥⌘M (Minimize All)");
        assert!(!is_cmd_m(&KeyPress { control: true, ..key("m") }), "⌃⌘M");
        assert!(!is_cmd_m(&key("n")), "⌘N");
    }

    #[test]
    fn cmd_m_on_a_managed_minimizable_window_is_caught() {
        let f = Focused { window: Some(7), button: true, settable: true };
        assert_eq!(judge_key(&key("m"), &f, &set()), KeyGate::Minimize(7));
        assert_eq!(judge_key(&KeyPress { repeat: true, ..key("m") }, &f, &set()), KeyGate::Swallow, "held down: nothing more");
    }

    #[test]
    fn cmd_m_elsewhere_is_the_apps() {
        let s = set();
        let ok = Focused { window: Some(7), button: true, settable: true };
        assert_eq!(judge_key(&key("m"), &Focused { window: Some(9), ..ok }, &s), KeyGate::Pass(Pass::NotManaged), "an unmanaged window");
        assert_eq!(judge_key(&key("m"), &Focused { button: false, ..ok }, &s), KeyGate::Pass(Pass::Disabled), "no minimize button");
        assert_eq!(judge_key(&key("m"), &Focused { settable: false, ..ok }, &s), KeyGate::Pass(Pass::Disabled), "not minimizable");
        assert_eq!(judge_key(&key("m"), &Focused::default(), &s), KeyGate::Pass(Pass::Unknown), "no focused window or no answer");
        assert_eq!(judge_key(&key("m"), &ok, &Minimizable { on: false, ..s }), KeyGate::Pass(Pass::NotManaged), "off");
        // A repeat on a window not caught passes too.
        assert_eq!(judge_key(&KeyPress { repeat: true, ..key("m") }, &Focused { window: Some(9), ..ok }, &set()), KeyGate::Pass(Pass::NotManaged));
    }
}
