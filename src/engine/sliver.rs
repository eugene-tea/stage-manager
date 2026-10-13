//! Inert slivers (`[windows] inert_slivers`): a press on what a parked window keeps on screen
//! does nothing, as with Apple's Stage Manager, whose parked windows cannot be grabbed.
//!
//! macOS keeps a strip of every parked window on screen (one point wide, 32 to 66 points of
//! title bar tall, [`super::park`]); a press there used to reach the app, so a stray click in
//! the corner focused, dragged or resized a parked window. The platform's event tap asks this
//! module, for every mouse press:
//!
//! 1. **Cheap first** ([`Shield::may_hit`]): only a press inside the corner zone of the park
//!    corner ([`corner_zone`]) can hit a sliver. Everywhere else (nearly every press) the
//!    answer is "pass" after a few rectangle tests, without asking the window server.
//! 2. **Then exact** ([`topmost_at`]): the topmost on-screen window under the press, our own
//!    windows (the strip's panels, the motion overlay), transparent ones and the Dock's
//!    window away from its bar ([`DockBar`]) skipped, each window's invisible resize border
//!    ([`GRAB_MARGIN`]) counted as its own. Only when that window is one the model
//!    counts as parked is the press swallowed: a window of the active stage over the corner,
//!    the Dock's bar, a menu bar extra or the wallpaper keep their presses.
//! 3. **The whole gesture** ([`PressGate`]): a swallowed press swallows its drags and its
//!    release too, button by button, so no app sees half a click.

use std::collections::HashSet;

use super::geometry::{Point, Rect};
use super::overview::DOCK_LAYER;
use super::park::{Corner, MACOS_KEEPS_TITLE, PARK_MARGIN};

/// How far above the visible frame's bottom (or below its top, for a top corner) the zone
/// reaches: the tallest title strip macOS was seen to keep (66 pt, Telegram) with room.
pub const ZONE_REACH: f64 = 2.0 * PARK_MARGIN + MACOS_KEEPS_TITLE;

/// Where slivers of windows parked in `corner` of a display (`frame`, `visible`) can be
/// pressed: [`PARK_MARGIN`] points in from the corner's side; at a bottom corner from
/// [`ZONE_REACH`] points above the visible frame's bottom on to the display's edge (a bottom
/// Dock sits under it); at a top corner the whole side, since macOS keeps a window parked
/// there below the menu bar at its full height.
pub fn corner_zone(frame: Rect, visible: Rect, corner: Corner) -> Rect {
    let v = if visible.is_degenerate() { frame } else { visible };
    let x = if corner.right() { frame.max_x() - PARK_MARGIN } else { frame.x };
    if corner.bottom() {
        let top = v.max_y() - ZONE_REACH;
        Rect::new(x, top, PARK_MARGIN, frame.max_y() - top)
    } else {
        Rect::new(x, frame.y, PARK_MARGIN, frame.h)
    }
}

/// The zones of every display in use (`displays`: each one's frame, visible frame and park
/// corner), one per display and corner, without repeats.
pub fn zones(displays: impl IntoIterator<Item = (Rect, Rect, Corner)>) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    for (frame, visible, corner) in displays {
        let z = corner_zone(frame, visible, corner);
        if !out.contains(&z) {
            out.push(z);
        }
    }
    out
}

/// The topmost window (our own and transparent ones seen through) that covers the whole of
/// `zone` and is not parked: while it is there, every press in the zone is its, so the tap
/// passes them all; one that lets clicks through (an overlay) leaves the slivers under it
/// pressable. Worth a note in the log.
pub fn zone_owner(front_to_back: &[Listed], zone: Rect, own_pid: i32, shield: &Shield) -> Option<Listed> {
    front_to_back
        .iter()
        .copied()
        .filter(|w| w.pid != own_pid && w.alpha > 0.0 && !shield.dock.clear_over(w, |bar| bar.contains_rect(&zone)))
        .find(|w| w.bounds.intersect(&zone).is_some())
        .filter(|w| !shield.parked.contains(&w.id) && w.bounds.contains_rect(&zone))
}

/// A window on screen at a press, as the window server lists it (front to back).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listed {
    /// `CGWindowID`.
    pub id: u32,
    /// Owning process.
    pub pid: i32,
    /// Bounds, top-left origin.
    pub bounds: Rect,
    /// Alpha (0: transparent, click-through in practice).
    pub alpha: f64,
    /// Window layer; normal app windows are at 0, the Dock's at [`DOCK_LAYER`].
    pub layer: i64,
}

/// The Dock as the hit test needs it. The Dock draws its bar in one window at
/// [`DOCK_LAYER`] as large as its whole display (seen 10-13 on a laptop: 1800×1169), clear and
/// click-through away from the bar, so the window list cannot tell the bar from the rest:
/// the bar's own frames come from Accessibility, read on the WM thread.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DockBar {
    /// The Dock's pid, when it runs.
    pub pid: Option<i32>,
    /// The bar's frames (the Dock's `AXList` children), top-left origin; empty when unknown.
    pub bars: Vec<Rect>,
}

impl DockBar {
    /// True when `w` is the Dock's window and clear where `on_bar` finds no bar: seen
    /// through, as clicks go through it there.
    fn clear_over(&self, w: &Listed, on_bar: impl Fn(Rect) -> bool) -> bool {
        Some(w.pid) == self.pid && w.layer == DOCK_LAYER && !self.bars.iter().any(|b| on_bar(*b))
    }
}

/// The topmost window of `front_to_back` under `at`, our own (`own_pid`) and transparent
/// windows skipped, and the Dock's away from its bar.
pub fn topmost_at(front_to_back: &[Listed], at: Point, own_pid: i32, dock: &DockBar) -> Option<Listed> {
    front_to_back
        .iter()
        .copied()
        .find(|w| w.pid != own_pid && w.alpha > 0.0 && contains(w.bounds, at) && !dock.clear_over(w, |b| contains(b, at)))
}

/// How far outside its frame a window takes a press: macOS's invisible resize border (seen
/// 10-13: a press 3.1 pt left of a parked window's 1 pt sliver grabbed its left edge, and a
/// double-click and drag pulled it out). Wide on purpose: the exposed part is the window's
/// rounded top-left corner, whose diagonal hot spot reaches farther than an edge's, and a
/// press in this band costs at most a wallpaper click inside the corner zone.
pub const GRAB_MARGIN: f64 = 12.0;

/// The resize border counted for a window not parked: the measured edge (3.1 pt) with room.
/// Narrower than [`GRAB_MARGIN`], so a shown window ending a few points short of the corner
/// does not take a press on a sliver.
pub const OTHER_GRAB_MARGIN: f64 = 4.0;

/// The frontmost window of `front_to_back` whose frame grown by its resize border
/// ([`GRAB_MARGIN`] parked, [`OTHER_GRAB_MARGIN`] else) holds `at`,
/// skipping what [`topmost_at`] skips: the window server hit-tests front to back with the
/// resize border counted, so a front window's border beats a back window's content.
fn grabbing_at(front_to_back: &[Listed], at: Point, own_pid: i32, dock: &DockBar, parked: &HashSet<u32>) -> Option<Listed> {
    front_to_back.iter().copied().find(|w| {
        let m = if parked.contains(&w.id) { GRAB_MARGIN } else { OTHER_GRAB_MARGIN };
        let grown = Rect::new(w.bounds.x - m, w.bounds.y - m, w.bounds.w + 2.0 * m, w.bounds.h + 2.0 * m);
        w.pid != own_pid && w.alpha > 0.0 && contains(grown, at) && !dock.clear_over(w, |b| contains(b, at))
    })
}

/// Half-open containment, as the window server hit-tests: the right and bottom edges are
/// outside.
fn contains(r: Rect, p: Point) -> bool {
    p.x >= r.x && p.x < r.max_x() && p.y >= r.y && p.y < r.max_y()
}

/// What the event tap knows, published by the WM thread after every change: whether the
/// shield is on, the corner zones, and the windows the model counts as parked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shield {
    /// `[windows] inert_slivers`, and the app managing windows.
    pub on: bool,
    /// The corner zones of every display with a parked window ([`zones`]).
    pub zones: Vec<Rect>,
    /// Windows parked now (every Space: the shown one's are the ones on screen).
    pub parked: HashSet<u32>,
    /// The Dock and its bar.
    pub dock: DockBar,
}

impl Shield {
    /// The cheap test: a press at `at` may hit a sliver.
    pub fn may_hit(&self, at: Point) -> bool {
        self.on && !self.parked.is_empty() && self.zones.iter().any(|z| contains(*z, at))
    }

    /// The exact test, given the window server's on-screen list at the press: the frontmost
    /// window whose frame or resize border ([`GRAB_MARGIN`]) holds the press is parked. A
    /// non-resizable parked window in front then takes a press within the margin over a
    /// window behind: rarer and milder than the parked window's edge grabbed.
    pub fn hits(&self, front_to_back: &[Listed], at: Point, own_pid: i32) -> Option<u32> {
        if !self.may_hit(at) {
            return None;
        }
        grabbing_at(front_to_back, at, own_pid, &self.dock, &self.parked)
            .map(|w| w.id)
            .filter(|id| self.parked.contains(id))
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Other,
}

impl Button {
    fn index(self) -> usize {
        match self {
            Button::Left => 0,
            Button::Right => 1,
            Button::Other => 2,
        }
    }
}

/// A mouse event the tap sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Down(Button),
    Dragged(Button),
    Up(Button),
}

/// Swallows whole gestures: from a press on a sliver to its release, per button.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PressGate {
    swallowing: [bool; 3],
}

impl PressGate {
    /// No gesture swallowed.
    pub const fn new() -> Self {
        Self { swallowing: [false; 3] }
    }

    /// Whether to swallow `event`. `on_sliver` is asked only for a press (it may cost a
    /// window list read); drags and releases follow their press.
    pub fn swallow(&mut self, event: Press, on_sliver: impl FnOnce() -> bool) -> bool {
        match event {
            Press::Down(b) => {
                let s = on_sliver();
                self.swallowing[b.index()] = s;
                s
            }
            Press::Dragged(b) => self.swallowing[b.index()],
            Press::Up(b) => std::mem::take(&mut self.swallowing[b.index()]),
        }
    }

    /// True while a swallowed press of some button is still down.
    pub fn busy(&self) -> bool {
        self.swallowing.iter().any(|s| *s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Size;
    use crate::engine::park::park_frame_at;

    const MAIN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    const VISIBLE: Rect = Rect::new(0.0, 30.0, 2560.0, 1410.0);
    const OWN: i32 = 1;
    const DOCK: i32 = 40;
    /// The Dock's bar, brought up at the bottom (its window spans the whole display).
    const BAR: Rect = Rect::new(600.0, 1370.0, 1360.0, 70.0);

    /// Where macOS leaves a window parked bottom-right that keeps `kept` points of title
    /// (measured 10-13: Safari 52, Chrome 41, Telegram 66, most apps 32).
    fn sliver(id: u32, kept: f64) -> Listed {
        let asked = park_frame_at(Size::new(1200.0, 900.0), MAIN, Corner::BottomRight);
        Listed { id, pid: 100 + id as i32, bounds: Rect::new(asked.x, VISIBLE.max_y() - kept, 1200.0, 900.0), alpha: 1.0, layer: 0 }
    }

    fn shield(parked: &[u32]) -> Shield {
        Shield {
            on: true,
            zones: vec![corner_zone(MAIN, VISIBLE, Corner::BottomRight)],
            parked: parked.iter().copied().collect(),
            dock: DockBar { pid: Some(DOCK), bars: vec![BAR] },
        }
    }

    #[test]
    fn every_measured_sliver_lies_in_the_zone() {
        let zone = corner_zone(MAIN, VISIBLE, Corner::BottomRight);
        for kept in [32.0, 41.0, 52.0, 66.0] {
            let s = sliver(1, kept);
            let seen = s.bounds.intersect(&MAIN).expect("a sliver shows");
            assert!(zone.contains_rect(&seen), "{kept}: {seen:?} outside {zone:?}");
        }
    }

    #[test]
    fn a_press_on_a_sliver_is_swallowed_and_its_neighbours_pass() {
        let list = [sliver(7, 52.0), sliver(8, 32.0)];
        let s = shield(&[7, 8]);
        // On Safari's 52 pt strip, above the 32 pt one.
        assert_eq!(s.hits(&list, Point::new(2559.5, VISIBLE.max_y() - 45.0), OWN), Some(7));
        // Where both overlap, the front one (first listed) takes it.
        assert_eq!(s.hits(&list, Point::new(2559.5, VISIBLE.max_y() - 10.0), OWN), Some(7));
        // One point left of the slivers: their resize border; past it, the wallpaper.
        assert_eq!(s.hits(&list, Point::new(2558.5, VISIBLE.max_y() - 10.0), OWN), Some(7));
        assert_eq!(s.hits(&list, Point::new(2559.0 - GRAB_MARGIN - 1.0, VISIBLE.max_y() - 10.0), OWN), None);
    }

    #[test]
    fn a_press_on_a_slivers_resize_border_is_swallowed() {
        // 10-13: 3.1 pt left of the sliver, on no window: the parked window's resize border.
        let s = sliver(7, 52.0);
        let y = VISIBLE.max_y() - 20.0;
        let list = [s];
        assert_eq!(shield(&[7]).hits(&list, Point::new(s.bounds.x - 3.1, y), OWN), Some(7));
        assert_eq!(shield(&[7]).hits(&list, Point::new(s.bounds.x - GRAB_MARGIN + 0.5, y), OWN), Some(7));
        // Farther left: the wallpaper's.
        assert_eq!(shield(&[7]).hits(&list, Point::new(s.bounds.x - GRAB_MARGIN - 1.0, y), OWN), None);
        // A parked window in front: its border beats the content of a shown window behind.
        let shown = Listed { id: 3, pid: 50, bounds: Rect::new(1200.0, 900.0, s.bounds.x - 1200.0 - 2.0, 600.0), alpha: 1.0, layer: 0 };
        assert_eq!(shield(&[7]).hits(&[s, shown], Point::new(s.bounds.x - 3.1, y), OWN), Some(7));
        // A shown window in front of it keeps its content.
        assert_eq!(shield(&[7]).hits(&[shown, s], Point::new(s.bounds.x - 3.1, y), OWN), None);
        // A shown window in front ending 10 pt short of the sliver: a press on the sliver is
        // the sliver's.
        let short = Listed { bounds: Rect::new(1200.0, 900.0, s.bounds.x - 1200.0 - 10.0, 600.0), ..shown };
        assert_eq!(shield(&[7]).hits(&[short, s], Point::new(s.bounds.x + 0.5, y), OWN), Some(7));
        // A shown window's border in front of the sliver's takes it.
        let near = Listed { bounds: Rect::new(1200.0, 900.0, s.bounds.x - 1200.0 - 5.0, 600.0), ..shown };
        assert_eq!(shield(&[7]).hits(&[near, s], Point::new(s.bounds.x - 3.1, y), OWN), None);
    }

    #[test]
    fn a_shown_window_over_the_corner_keeps_its_press() {
        let shown = Listed { id: 3, pid: 50, bounds: Rect::new(1800.0, 900.0, 800.0, 600.0), alpha: 1.0, layer: 0 };
        let list = [shown, sliver(7, 52.0)];
        assert_eq!(shield(&[7]).hits(&list, Point::new(2559.5, VISIBLE.max_y() - 20.0), OWN), None);
    }

    #[test]
    fn the_dock_and_menu_bar_extras_over_the_corner_keep_their_presses() {
        // The Dock's window (its layer, listed first) spans the display; its bar reaching
        // into the corner keeps a press there.
        let dock = Listed { id: 2, pid: DOCK, bounds: MAIN, alpha: 1.0, layer: DOCK_LAYER };
        let list = [dock, sliver(7, 52.0)];
        let wide = Shield { dock: DockBar { pid: Some(DOCK), bars: vec![Rect::new(0.0, 1370.0, 2560.0, 70.0)] }, ..shield(&[7]) };
        assert_eq!(wide.hits(&list, Point::new(2559.5, VISIBLE.max_y() - 5.0), OWN), None);
        // A menu bar extra sits far from a bottom zone: the cheap test passes it.
        assert!(!shield(&[7]).may_hit(Point::new(2540.0, 10.0)));
    }

    #[test]
    fn the_docks_window_away_from_its_bar_is_seen_through() {
        // Seen 10-13 on a laptop: the auto-hiding Dock brought up lists one window as large
        // as the display; a press on a sliver beside the bar went to the parked window.
        let dock = Listed { id: 46, pid: DOCK, bounds: MAIN, alpha: 1.0, layer: DOCK_LAYER };
        let list = [dock, sliver(7, 52.0)];
        let at = Point::new(2559.5, VISIBLE.max_y() - 20.0);
        assert_eq!(shield(&[7]).hits(&list, at, OWN), Some(7));
        // Its bar unknown: still seen through (the bar rarely reaches the corner).
        let blind = Shield { dock: DockBar { pid: Some(DOCK), bars: Vec::new() }, ..shield(&[7]) };
        assert_eq!(blind.hits(&list, at, OWN), Some(7));
        // Another app's window at the Dock's layer, or the Dock at another layer (Mission
        // Control's, Launchpad's), is not.
        let other = Listed { pid: 41, ..dock };
        assert_eq!(shield(&[7]).hits(&[other, sliver(7, 52.0)], at, OWN), None);
        let launchpad = Listed { layer: 27, ..dock };
        assert_eq!(shield(&[7]).hits(&[launchpad, sliver(7, 52.0)], at, OWN), None);
        // Mission Control: WindowManager's window under the Dock's takes the press.
        let expose = Listed { id: 60, pid: 70, bounds: MAIN, alpha: 1.0, layer: 19 };
        assert_eq!(shield(&[7]).hits(&[dock, expose, sliver(7, 52.0)], at, OWN), None);
        // Nor does the Dock's window own the zone for the log's note.
        let zone = corner_zone(MAIN, VISIBLE, Corner::BottomRight);
        assert_eq!(zone_owner(&list, zone, OWN, &shield(&[7])), None);
    }

    #[test]
    fn a_quick_note_in_the_corner_keeps_its_press() {
        // 10-13, the hot corner set to Quick Note: its preview is a window of its own
        // (LinkedNotesUIService, 254×254 in the corner), not drawn in the Dock's window, so a
        // press on it passes with the Dock's window seen through.
        let dock = Listed { id: 46, pid: DOCK, bounds: MAIN, alpha: 1.0, layer: DOCK_LAYER };
        let note = Listed { id: 55, pid: 80, bounds: Rect::new(MAIN.max_x() - 254.0, MAIN.max_y() - 254.0, 254.0, 254.0), alpha: 1.0, layer: 25 };
        let at = Point::new(2559.5, VISIBLE.max_y() - 20.0);
        assert_eq!(shield(&[7]).hits(&[dock, note, sliver(7, 52.0)], at, OWN), None);
        assert_eq!(shield(&[7]).hits(&[note, dock, sliver(7, 52.0)], at, OWN), None);
    }

    #[test]
    fn our_own_and_transparent_windows_are_seen_through() {
        let overlay = Listed { id: 90, pid: OWN, bounds: MAIN, alpha: 1.0, layer: 0 };
        let clear = Listed { id: 91, pid: 60, bounds: MAIN, alpha: 0.0, layer: 0 };
        let list = [overlay, clear, sliver(7, 32.0)];
        assert_eq!(shield(&[7]).hits(&list, Point::new(2559.5, VISIBLE.max_y() - 5.0), OWN), Some(7));
    }

    #[test]
    fn a_window_not_parked_in_the_corner_keeps_its_press() {
        // An unmanaged window, or one the model shows, tucked into the corner.
        let list = [sliver(9, 32.0)];
        assert_eq!(shield(&[7]).hits(&list, Point::new(2559.5, VISIBLE.max_y() - 5.0), OWN), None);
    }

    #[test]
    fn off_or_with_nothing_parked_nothing_is_swallowed() {
        let list = [sliver(7, 32.0)];
        let at = Point::new(2559.5, VISIBLE.max_y() - 5.0);
        assert_eq!(Shield { on: false, ..shield(&[7]) }.hits(&list, at, OWN), None);
        assert_eq!(shield(&[]).hits(&list, at, OWN), None);
    }

    #[test]
    fn presses_away_from_the_corner_never_ask_the_window_server() {
        let s = shield(&[7]);
        for p in [Point::new(100.0, 100.0), Point::new(1280.0, 1400.0), Point::new(2559.0, 100.0), Point::new(2400.0, 1400.0)] {
            assert!(!s.may_hit(p), "{p:?}");
        }
        assert!(s.may_hit(Point::new(2559.5, 1430.0)));
    }

    #[test]
    fn every_corner_has_its_zone_at_its_side() {
        for corner in Corner::ALL {
            let zone = corner_zone(MAIN, VISIBLE, corner);
            let asked = park_frame_at(Size::new(900.0, 700.0), MAIN, corner);
            let y = if corner.bottom() { VISIBLE.max_y() - MACOS_KEEPS_TITLE } else { VISIBLE.y };
            let at = Rect::new(asked.x, y, asked.w, asked.h);
            let seen = at.intersect(&MAIN).expect("a sliver shows");
            assert!(zone.contains_rect(&seen), "{corner:?}: {seen:?} outside {zone:?}");
            assert!(zone.w <= PARK_MARGIN, "{corner:?}: a narrow zone {zone:?}");
            if corner.bottom() {
                assert!(zone.h <= MAIN.h / 4.0, "{corner:?}: a small zone {zone:?}");
            }
        }
    }

    #[test]
    fn each_display_in_use_has_its_zone_once() {
        let second = Rect::new(2560.0, 0.0, 1920.0, 1080.0);
        let z = zones([
            (MAIN, VISIBLE, Corner::BottomRight),
            (second, second, Corner::BottomLeft),
            (MAIN, VISIBLE, Corner::BottomRight),
        ]);
        assert_eq!(z.len(), 2, "{z:?}");
        assert!(z[1].x >= second.x && z[1].max_x() <= second.x + PARK_MARGIN, "the second display's own corner: {z:?}");
        let s = Shield { on: true, zones: z, parked: [7].into(), dock: DockBar::default() };
        assert!(s.may_hit(Point::new(2560.5, 1075.0)), "a press in the second display's corner is judged");
        assert!(s.may_hit(Point::new(2559.5, 1430.0)));
    }

    #[test]
    fn a_window_over_the_whole_zone_owns_it() {
        let zone = corner_zone(MAIN, VISIBLE, Corner::BottomRight);
        let parked = shield(&[7]);
        let overlay = Listed { id: 3, pid: 50, bounds: MAIN, alpha: 1.0, layer: 0 };
        assert_eq!(zone_owner(&[overlay, sliver(7, 52.0)], zone, OWN, &parked), Some(overlay));
        // Seen through: ours, transparent ones.
        let ours = Listed { pid: OWN, ..overlay };
        let clear = Listed { alpha: 0.0, ..overlay };
        assert_eq!(zone_owner(&[ours, clear, sliver(7, 52.0)], zone, OWN, &parked), None);
        // A window over part of it, or a sliver on top: nobody owns the zone.
        let part = Listed { id: 4, pid: 51, bounds: Rect::new(2000.0, 1300.0, 560.0, 100.0), alpha: 1.0, layer: 0 };
        assert_eq!(zone_owner(&[part, overlay], zone, OWN, &parked), None);
        assert_eq!(zone_owner(&[sliver(7, 52.0), overlay], zone, OWN, &parked), None);
        assert_eq!(zone_owner(&[], zone, OWN, &parked), None);
    }

    #[test]
    fn a_swallowed_press_takes_its_drags_and_release_and_nothing_else() {
        let mut g = PressGate::default();
        assert!(g.swallow(Press::Down(Button::Left), || true));
        assert!(g.busy());
        assert!(g.swallow(Press::Dragged(Button::Left), || unreachable!()));
        // Another button's gesture meanwhile is its own.
        assert!(!g.swallow(Press::Down(Button::Right), || false));
        assert!(!g.swallow(Press::Up(Button::Right), || unreachable!()));
        assert!(g.swallow(Press::Up(Button::Left), || unreachable!()));
        assert!(!g.busy());
        // The next gesture is judged afresh.
        assert!(!g.swallow(Press::Down(Button::Left), || false));
        assert!(!g.swallow(Press::Dragged(Button::Left), || unreachable!()));
        assert!(!g.swallow(Press::Up(Button::Left), || unreachable!()));
        // A release with no press seen passes.
        assert!(!g.swallow(Press::Up(Button::Other), || unreachable!()));
    }
}
