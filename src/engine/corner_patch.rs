//! The wallpaper corner patch (`[windows] corner_patch`, on by default: Apple's corner is clean): a
//! small opaque panel over the park corner that shows the desktop picture under the parked
//! windows' slivers, drawn from the picture's file ([`super::wallpaper`]; no screen
//! capture), so the corner looks empty as Apple's does. This module is its geometry and
//! when it may show.
//!
//! - **Where** ([`patch_rect`]): the parked windows' slivers as the window server lists them,
//!   each grown by the shadow's reach ([`DEFAULT_REACH`], `[windows] corner_patch_reach`),
//!   on the display. A park or unpark that shrinks it a little keeps the rectangle in use
//!   ([`steady_rect`]), so the panel does not move for every sliver that comes and goes.
//! - **Ordering.** The patch is a normal-level window ordered just above the topmost parked
//!   window under it (`orderWindow:relativeTo:` with another app's window number). Whether
//!   it sits right is checked afresh at every plan against the window server's list
//!   ([`ordered_right`]), once the panel is on screen ordered against the window wanted now
//!   ([`order_step`]): an activation raising a parked window, or a hide and show, can undo
//!   it at any time. Ordered right, every shown window stays above the patch on its own; the
//!   patch may show whenever slivers are there. Not known yet (never shown, hidden, or a
//!   new window to order against), it shows ordered against that window and is checked at
//!   the next plan. Found wrong after being right, it is ordered again (unknown again).
//!   An order that cannot be right whatever macOS does (a shown window reaching the patch
//!   sits below the parked window: a stage switch's windows just parked are in front of the
//!   one just shown) is known from the list before any show ([`would_be_right`]): the patch
//!   takes the fallback without showing over that window for a tick.
//! - **Found not ordered right** (the fallback; macOS did not honour the order): the patch
//!   sits above every normal window there, so it shows only while no shown window reaches
//!   its rectangle, no window is being dragged and no switch is running ([`visibility`]).
//! - **Desktop items.** The patch draws the picture only: a Finder desktop icon under it
//!   (read through Accessibility, [`desktop_icons`], [`item_reach`]) keeps it off, and so
//!   does not knowing: no good read yet, more icons than are read ([`MAX_ICONS`]), or a tree
//!   that is not the one expected. A read that fails keeps the last good one. Widgets'
//!   places are not public; with widgets shown on the desktop the patch would cover one in
//!   the corner (logged).
//! - Never during Mission Control, App Exposé or Show Desktop (its panel is `Transient`, and
//!   the overview watch hides it), on a full-screen app's Space, nor once the app stands
//!   down (nothing parked).

use std::collections::HashSet;

use super::geometry::Rect;

/// How far (points) a window's shadow reaches past its frame: an active window's shadow is
/// the largest (unmeasured on the owner's displays; to be confirmed by the live check).
pub const DEFAULT_REACH: f64 = 60.0;

/// The patch's rectangle: every sliver (`slivers`: parked windows' bounds as listed) that
/// shows on `display`, grown by `reach` on every side, rounded out to whole points (so the
/// picture's crop, whole pixels, is exactly the panel's size), kept on the display. `None`
/// when no sliver shows.
pub fn patch_rect(slivers: &[Rect], display: Rect, reach: f64) -> Option<Rect> {
    let reach = if reach.is_finite() { reach.clamp(0.0, 400.0) } else { DEFAULT_REACH };
    let mut out: Option<Rect> = None;
    for s in slivers.iter().filter_map(|s| s.intersect(&display)) {
        let grown = Rect::new(s.x - reach, s.y - reach, s.w + 2.0 * reach, s.h + 2.0 * reach);
        out = Some(match out {
            None => grown,
            Some(u) => union(u, grown),
        });
    }
    out.and_then(|r| r.intersect(&display)).map(whole_points).and_then(|r| r.intersect(&display))
}

/// `r` grown to whole points on every side.
fn whole_points(r: Rect) -> Rect {
    let (x, y) = (r.x.floor(), r.y.floor());
    Rect::new(x, y, r.max_x().ceil() - x, r.max_y().ceil() - y)
}

/// How much bigger (points, on any side) than needed the rectangle in use may stay.
pub const STEADY_SLACK: f64 = 48.0;

/// The rectangle to use: the one in use (`prev`) while it still covers `next` and is at most
/// [`STEADY_SLACK`] bigger on every side (a sliver that came and went), else `next`.
pub fn steady_rect(prev: Option<Rect>, next: Rect) -> Rect {
    match prev {
        Some(p)
            if p.contains_rect(&next)
                && next.x - p.x <= STEADY_SLACK
                && next.y - p.y <= STEADY_SLACK
                && p.max_x() - next.max_x() <= STEADY_SLACK
                && p.max_y() - next.max_y() <= STEADY_SLACK =>
        {
            p
        }
        _ => next,
    }
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect::new(x, y, a.max_x().max(b.max_x()) - x, a.max_y().max(b.max_y()) - y)
}

/// One window of the window server's on-screen list, front to back, as [`ordered_right`]
/// needs it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listed {
    /// `CGWindowID`.
    pub id: u32,
    /// Window layer (0: normal windows).
    pub layer: i64,
    /// Bounds, top-left origin.
    pub bounds: Rect,
    /// Drawn (alpha above 0) and another app's (this app's own windows never count).
    pub drawn: bool,
}

impl Listed {
    /// Whether the window counts for the patch at `rect`: a normal window, drawn, another
    /// app's, reaching the rectangle. The one filter for [`ordered_right`],
    /// [`would_be_right`] and [`PatchFacts::shown`] (with the parked ones left out).
    pub fn counts(&self, rect: Rect) -> bool {
        self.layer == 0 && self.drawn && self.bounds.intersect(&rect).is_some()
    }
}

/// True when the patch (`patch`, at `rect`) sits above every parked window (`parked`) that
/// reaches its rectangle and below every other window that does ([`Listed::counts`]): what
/// ordering it relative to the topmost parked window should give. False when the patch is
/// not listed.
pub fn ordered_right(front_to_back: &[Listed], patch: u32, rect: Rect, parked: &HashSet<u32>) -> bool {
    let Some(at) = front_to_back.iter().position(|w| w.id == patch) else { return false };
    front_to_back.iter().enumerate().filter(|(_, w)| w.id != patch && w.counts(rect)).all(
        |(i, w)| if parked.contains(&w.id) { i > at } else { i < at },
    )
}

/// Whether ordering the patch (`patch`, at `rect`) just above `above` could be right: the
/// list with the patch taken out and put back just above `above`, checked by
/// [`ordered_right`]. False when a shown window reaching the rectangle sits below `above`
/// (or a parked one above it), or `above` is not listed: then no order against `above` is
/// right, and the patch must not show to find out. Only whether macOS honours the order
/// across apps is left for a live show to tell.
pub fn would_be_right(front_to_back: &[Listed], patch: u32, above: u32, rect: Rect, parked: &HashSet<u32>) -> bool {
    let mut list: Vec<Listed> = front_to_back.iter().filter(|w| w.id != patch).copied().collect();
    let Some(at) = list.iter().position(|w| w.id == above) else { return false };
    list.insert(at, Listed { id: patch, layer: 0, bounds: rect, drawn: true });
    ordered_right(&list, patch, rect, parked)
}

/// What the WM thread remembers of the patch's order between plans.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OrderState {
    /// The last check's answer.
    pub last: Option<bool>,
    /// The parked window the patch was found not ordered above after being ordered against
    /// it (and ordered anew, where it had been right): macOS did not honour that order.
    pub refused: Option<u32>,
}

/// One plan's knowledge of the order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderStep {
    /// For [`PatchFacts::ordered`].
    pub ordered: Option<bool>,
    /// Order the patch again (it was right and is not any more).
    pub reorder: bool,
}

/// The order as this plan knows it: `above` the parked window to order against now,
/// `check` [`ordered_right`] when the panel is on screen ordered against `above` with the
/// current reorder count (`None` otherwise: not known). Right is right; wrong after right
/// orders it again (not known until checked again); wrong otherwise is macOS refusing that
/// order, remembered for `above` so an unknown order there falls back rather than showing
/// the patch over windows again and again.
pub fn order_step(state: &mut OrderState, above: Option<u32>, check: Option<bool>) -> OrderStep {
    let Some(above) = above else {
        state.last = None;
        return OrderStep { ordered: None, reorder: false };
    };
    if state.refused.is_some_and(|r| r != above) {
        state.refused = None;
    }
    match check {
        Some(true) => {
            *state = OrderState { last: Some(true), refused: None };
            OrderStep { ordered: Some(true), reorder: false }
        }
        Some(false) if state.last == Some(true) => {
            state.last = Some(false);
            OrderStep { ordered: None, reorder: true }
        }
        Some(false) => {
            *state = OrderState { last: Some(false), refused: Some(above) };
            OrderStep { ordered: Some(false), reorder: false }
        }
        None => OrderStep { ordered: (state.refused == Some(above)).then_some(false), reorder: false },
    }
}

/// What the patch's visibility depends on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PatchFacts {
    /// `[windows] corner_patch` and the app managing windows.
    pub on: bool,
    /// The patch's rectangle ([`patch_rect`]).
    pub rect: Option<Rect>,
    /// Mission Control, App Exposé or Show Desktop is up (or settling).
    pub overview: bool,
    /// The patch's order ([`order_step`]): right, found wrong, or `None` while not known
    /// (never shown, hidden, ordered against another window, ordered anew).
    pub ordered: Option<bool>,
    /// A parked window lies under the patch to order it against, and ordering just above it
    /// could be right ([`would_be_right`]).
    pub can_order: bool,
    /// A full-screen app's Space shows.
    pub fullscreen: bool,
    /// Shown (not parked) normal windows on screen, any app's.
    pub shown: Vec<Rect>,
    /// A window is being dragged, or the left button is down.
    pub dragging: bool,
    /// A switch's transition is running.
    pub transition: bool,
    /// Finder's desktop icons with their labels ([`item_reach`]), as last read well; `None`
    /// while they are not known (no good read yet, too many to read).
    pub items: Option<Vec<Rect>>,
}

/// How far past a desktop icon's image (Accessibility's `AXImage` frame) its label and
/// selection reach: sideways, above, below (two lines of label).
pub const ITEM_REACH: (f64, f64, f64) = (36.0, 6.0, 44.0);

/// A desktop icon's image frame grown by its label ([`ITEM_REACH`]).
pub fn item_reach(icon: Rect) -> Rect {
    let (side, above, below) = ITEM_REACH;
    Rect::new(icon.x - side, icon.y - above, icon.w + 2.0 * side, icon.h + above + below)
}

/// One element of Finder's Accessibility tree as read for [`desktop_icons`]: its role, its
/// frame (read for icons only) and its children (read only where the desktop's icons lie).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AxNode {
    /// `AXRole`.
    pub role: String,
    /// `AXPosition` and `AXSize`, where read.
    pub frame: Option<Rect>,
    /// The children read.
    pub children: Vec<AxNode>,
    /// How many children the element has (more than [`Self::children`] when not all were
    /// read).
    pub count: usize,
}

impl AxNode {
    /// An element with `role` and these children (all of them read).
    pub fn new(role: &str, children: Vec<AxNode>) -> Self {
        Self { role: role.into(), frame: None, count: children.len(), children }
    }

    /// An icon-like element with `role` at `frame`.
    pub fn leaf(role: &str, frame: Option<Rect>) -> Self {
        Self { role: role.into(), frame, children: Vec::new(), count: 0 }
    }
}

/// The most desktop icons read (one Accessibility message each); more keep the patch off.
pub const MAX_ICONS: usize = 200;

/// Accessibility roles in Finder's desktop tree.
pub mod roles {
    /// A display's desktop.
    pub const SCROLL_AREA: &str = "AXScrollArea";
    /// The desktop's icon view.
    pub const GROUP: &str = "AXGroup";
    /// An icon's image.
    pub const IMAGE: &str = "AXImage";
}

/// What a read of Finder's desktop found.
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopIcons {
    /// Every icon's image frame (global, top-left).
    Icons(Vec<Rect>),
    /// More than [`MAX_ICONS`]: not read; where they lie is unknown.
    TooMany(usize),
    /// The tree is not the one expected: unreadable, never "no icons".
    Unexpected(&'static str),
}

/// The desktop icons in Finder's application element's children (`app`), Finder's desktop
/// being on (`desktop_on`, `com.apple.finder` `CreateDesktop`) or off.
///
/// Expected: one `AXScrollArea` per display's desktop, holding an `AXGroup` (the icon
/// view; scroll bars are other children) whose `AXImage` children are the icons. With the
/// desktop on and no scroll area, a scroll area with no icon view, or an icon view with
/// children none of which is an image, the tree is unexpected. An empty icon view is an
/// empty desktop.
pub fn desktop_icons(desktop_on: bool, app: &[AxNode]) -> DesktopIcons {
    let areas: Vec<&AxNode> = app.iter().filter(|n| n.role == roles::SCROLL_AREA).collect();
    if areas.is_empty() {
        return if desktop_on { DesktopIcons::Unexpected("Finder lists no desktop") } else { DesktopIcons::Icons(Vec::new()) };
    }
    let mut icons = Vec::new();
    let mut total = 0;
    for area in areas {
        let views: Vec<&AxNode> = area.children.iter().filter(|n| n.role == roles::GROUP).collect();
        if views.is_empty() {
            return DesktopIcons::Unexpected("a desktop with no icon view");
        }
        for view in views {
            total += view.count;
            if total > MAX_ICONS || view.children.len() < view.count {
                return DesktopIcons::TooMany(total.max(MAX_ICONS + 1));
            }
            let images: Vec<&AxNode> = view.children.iter().filter(|n| n.role == roles::IMAGE).collect();
            if images.is_empty() && !view.children.is_empty() {
                return DesktopIcons::Unexpected("the desktop's icon view lists no icon images");
            }
            for image in images {
                match image.frame {
                    Some(f) if !f.is_degenerate() => icons.push(f),
                    Some(_) => return DesktopIcons::Unexpected("a desktop icon with an empty frame"),
                    None => return DesktopIcons::Unexpected("a desktop icon with no frame"),
                }
            }
        }
    }
    DesktopIcons::Icons(icons)
}

/// Whether Finder draws the desktop (`com.apple.finder` `CreateDesktop`, as text: absent is
/// on; `defaults write` without a type stores the text `false`).
pub fn finder_desktop_on(raw: Option<&str>) -> bool {
    !raw.is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"))
}

/// Whether the patch shows, and why not.
pub fn visibility(f: &PatchFacts) -> Result<Rect, &'static str> {
    if !f.on {
        return Err("off");
    }
    let Some(rect) = f.rect else { return Err("no sliver shows") };
    if f.overview {
        return Err("an overview is up");
    }
    if f.fullscreen {
        return Err("a full-screen Space shows");
    }
    let Some(items) = &f.items else { return Err("Finder's desktop icons are not known") };
    if items.iter().any(|i| i.intersect(&rect).is_some()) {
        return Err("a desktop icon lies under the corner");
    }
    match f.ordered {
        Some(true) => return Ok(rect),
        // Shown ordered against the parked window; checked at the next plan.
        None if f.can_order => return Ok(rect),
        _ => {}
    }
    // Found above every normal window (or nothing to order against): only where none
    // reaches, and never while windows move.
    if f.dragging {
        return Err("a window may be dragged");
    }
    if f.transition {
        return Err("a switch is running");
    }
    if f.shown.iter().any(|w| w.intersect(&rect).is_some()) {
        return Err("a shown window reaches the corner");
    }
    Ok(rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);

    fn sliver(kept: f64) -> Rect {
        Rect::new(2559.0, 1364.0 - kept, 1200.0, 900.0)
    }

    #[test]
    fn the_patch_covers_every_sliver_and_its_shadow_on_the_display() {
        let r = patch_rect(&[sliver(32.0), sliver(66.0)], MAIN, 60.0).expect("slivers show");
        for kept in [32.0, 66.0] {
            let seen = sliver(kept).intersect(&MAIN).expect("on screen");
            let shadow = Rect::new(seen.x - 60.0, seen.y - 60.0, seen.w + 60.0, seen.h + 60.0);
            assert!(r.contains_rect(&shadow.intersect(&MAIN).expect("on screen")), "{kept}: {r:?}");
        }
        assert!(MAIN.contains_rect(&r));
        assert_eq!(r.max_x(), MAIN.max_x());
        assert!(r.w <= 61.0 + 1.0, "only as wide as the shadow: {r:?}");
        assert_eq!(patch_rect(&[], MAIN, 60.0), None);
        assert_eq!(patch_rect(&[Rect::new(3000.0, 100.0, 50.0, 50.0)], MAIN, 60.0), None, "off the display");
    }

    #[test]
    fn ordered_right_means_above_the_parked_and_below_the_rest() {
        let rect = Rect::new(2440.0, 1240.0, 120.0, 200.0);
        let shown = Listed { id: 1, layer: 0, bounds: Rect::new(2000.0, 1000.0, 600.0, 400.0), drawn: true };
        let patch = Listed { id: 9, layer: 0, bounds: rect, drawn: true };
        let p1 = Listed { id: 7, layer: 0, bounds: sliver(52.0), drawn: true };
        let p2 = Listed { id: 8, layer: 0, bounds: sliver(32.0), drawn: true };
        let elsewhere = Listed { id: 2, layer: 0, bounds: Rect::new(0.0, 0.0, 300.0, 300.0), drawn: true };
        let dock = Listed { id: 3, layer: 20, bounds: Rect::new(0.0, 1364.0, 2560.0, 76.0), drawn: true };
        let parked: HashSet<u32> = [7, 8].into();
        assert!(ordered_right(&[dock, shown, patch, p1, elsewhere, p2], 9, rect, &parked));
        assert!(!ordered_right(&[p1, patch, p2], 9, rect, &parked), "a sliver above the patch");
        assert!(!ordered_right(&[patch, shown, p1], 9, rect, &parked), "the patch over a shown window");
        assert!(!ordered_right(&[shown, p1], 9, rect, &parked), "not listed");
    }

    #[test]
    fn windows_not_drawn_or_our_own_do_not_count_for_the_order() {
        // The same filter as the shown windows the fallback looks at: an invisible window
        // (alpha 0) or one of this app's own above or below the patch changes nothing.
        let rect = Rect::new(2440.0, 1240.0, 120.0, 200.0);
        let over = Rect::new(2000.0, 1000.0, 600.0, 400.0);
        let patch = Listed { id: 9, layer: 0, bounds: rect, drawn: true };
        let p1 = Listed { id: 7, layer: 0, bounds: sliver(52.0), drawn: true };
        let clear = Listed { id: 1, layer: 0, bounds: over, drawn: false };
        let parked: HashSet<u32> = [7].into();
        assert!(ordered_right(&[patch, clear, p1], 9, rect, &parked), "an undrawn window under the patch");
        assert!(would_be_right(&[clear, p1], 9, 7, rect, &parked));
        let drawn = Listed { drawn: true, ..clear };
        assert!(!ordered_right(&[patch, drawn, p1], 9, rect, &parked), "drawn, it counts");
        assert!(drawn.counts(rect) && !clear.counts(rect));
        assert!(!Listed { layer: 3, ..drawn }.counts(rect), "not a normal window");
    }

    #[test]
    fn an_order_that_cannot_be_right_is_known_before_any_show() {
        let rect = Rect::new(2440.0, 1240.0, 120.0, 200.0);
        let shown = Listed { id: 1, layer: 0, bounds: Rect::new(2000.0, 1000.0, 600.0, 400.0), drawn: true };
        let p1 = Listed { id: 7, layer: 0, bounds: sliver(52.0), drawn: true };
        let p2 = Listed { id: 8, layer: 0, bounds: sliver(32.0), drawn: true };
        let elsewhere = Listed { id: 2, layer: 0, bounds: Rect::new(0.0, 0.0, 300.0, 300.0), drawn: true };
        let parked: HashSet<u32> = [7, 8].into();
        // The shown window in front of the topmost parked one: just above 7 is right.
        assert!(would_be_right(&[shown, p1, elsewhere, p2], 9, 7, rect, &parked));
        // A stage switch: the windows just parked are in front, the one just shown behind
        // them. Just above 7 puts the patch over the shown window: never right.
        assert!(!would_be_right(&[p1, p2, shown], 9, 7, rect, &parked));
        assert!(!would_be_right(&[p1, shown, p2], 9, 7, rect, &parked));
        // A window behind the parked one that does not reach the patch changes nothing.
        assert!(would_be_right(&[p1, elsewhere, p2], 9, 7, rect, &parked));
        // The patch already listed somewhere (on screen): taken out first.
        let patch = Listed { id: 9, layer: 0, bounds: rect, drawn: true };
        assert!(would_be_right(&[patch, shown, p1, p2], 9, 7, rect, &parked), "wrong now, right once ordered");
        assert!(!would_be_right(&[shown, patch, p1, p2], 9, 8, rect, &parked), "above 8 leaves 7 over it");
        // `above` not listed: not right.
        assert!(!would_be_right(&[shown, p2], 9, 7, rect, &parked));
    }

    fn facts() -> PatchFacts {
        PatchFacts { on: true, rect: Some(Rect::new(2440.0, 1240.0, 120.0, 200.0)), items: Some(Vec::new()), ..PatchFacts::default() }
    }

    #[test]
    fn ordered_right_it_shows_whatever_windows_do() {
        let f = PatchFacts {
            ordered: Some(true),
            shown: vec![Rect::new(2000.0, 1000.0, 600.0, 400.0)],
            dragging: true,
            transition: true,
            ..facts()
        };
        assert!(visibility(&f).is_ok());
        assert!(visibility(&PatchFacts { overview: true, ..f.clone() }).is_err(), "never over Mission Control");
        assert!(visibility(&PatchFacts { on: false, ..f.clone() }).is_err());
        assert!(visibility(&PatchFacts { rect: None, ..f }).is_err());
    }

    #[test]
    fn a_desktop_icon_under_the_corner_keeps_it_off() {
        // An icon at the bottom of Finder's rightmost column, its label reaching the patch.
        let icon = Rect::new(2440.0 - 48.0 - 20.0, 1300.0, 48.0, 48.0);
        let f = PatchFacts { ordered: Some(true), items: Some(vec![item_reach(icon)]), ..facts() };
        assert_eq!(visibility(&f), Err("a desktop icon lies under the corner"));
        let away = Rect::new(2000.0, 100.0, 48.0, 48.0);
        assert!(visibility(&PatchFacts { items: Some(vec![item_reach(away)]), ..f.clone() }).is_ok());
        assert_eq!(visibility(&PatchFacts { items: None, ..f }), Err("Finder's desktop icons are not known"), "not known: off");
    }

    fn icon(x: f64, y: f64) -> AxNode {
        AxNode::leaf(roles::IMAGE, Some(Rect::new(x, y, 64.0, 64.0)))
    }

    fn scroll_bar() -> AxNode {
        AxNode::new("AXScrollBar", vec![AxNode::leaf("AXValueIndicator", None), AxNode::leaf("AXButton", None)])
    }

    fn desktop(view: Vec<AxNode>) -> AxNode {
        AxNode::new(roles::SCROLL_AREA, vec![AxNode::new(roles::GROUP, view), scroll_bar()])
    }

    #[test]
    fn finders_desktop_tree_gives_its_icon_images_only() {
        let menu = AxNode::new("AXMenuBar", vec![AxNode::leaf("AXMenuBarItem", None)]);
        let app = [menu.clone(), desktop(vec![icon(2400.0, 1300.0), icon(2400.0, 100.0), AxNode::leaf("AXStaticText", None)])];
        let got = desktop_icons(true, &app);
        assert_eq!(got, DesktopIcons::Icons(vec![Rect::new(2400.0, 1300.0, 64.0, 64.0), Rect::new(2400.0, 100.0, 64.0, 64.0)]), "scroll bar parts are no icons");
        // Two displays: both desktops' icons.
        let two = [desktop(vec![icon(10.0, 10.0)]), desktop(vec![icon(3000.0, 10.0)])];
        assert!(matches!(desktop_icons(true, &two), DesktopIcons::Icons(v) if v.len() == 2));
        // An empty desktop: no icons, known.
        assert_eq!(desktop_icons(true, &[desktop(Vec::new())]), DesktopIcons::Icons(Vec::new()));
        // Finder's desktop off: no scroll area is no icons.
        assert_eq!(desktop_icons(false, std::slice::from_ref(&menu)), DesktopIcons::Icons(Vec::new()));
    }

    #[test]
    fn an_unexpected_tree_is_unreadable_never_no_icons() {
        let menu = AxNode::new("AXMenuBar", Vec::new());
        assert!(matches!(desktop_icons(true, &[menu]), DesktopIcons::Unexpected(_)), "desktop on, no scroll area");
        let no_view = AxNode::new(roles::SCROLL_AREA, vec![scroll_bar()]);
        assert!(matches!(desktop_icons(true, &[no_view]), DesktopIcons::Unexpected(_)));
        let strange = desktop(vec![AxNode::leaf("AXButton", Some(Rect::new(0.0, 0.0, 10.0, 10.0)))]);
        assert!(matches!(desktop_icons(true, &[strange]), DesktopIcons::Unexpected(_)), "children, none an image");
        let frameless = desktop(vec![AxNode::leaf(roles::IMAGE, None)]);
        assert!(matches!(desktop_icons(true, &[frameless]), DesktopIcons::Unexpected(_)));
        let empty = desktop(vec![icon(10.0, 10.0), AxNode::leaf(roles::IMAGE, Some(Rect::new(2400.0, 1300.0, 0.0, 64.0)))]);
        assert!(matches!(desktop_icons(true, &[empty]), DesktopIcons::Unexpected(_)), "a zero-size icon frame is unknown, never dropped");
    }

    #[test]
    fn too_many_icons_are_not_read() {
        let crowded = AxNode { count: MAX_ICONS + 40, ..AxNode::new(roles::GROUP, Vec::new()) };
        let app = [AxNode::new(roles::SCROLL_AREA, vec![crowded])];
        assert_eq!(desktop_icons(true, &app), DesktopIcons::TooMany(MAX_ICONS + 40));
        // Two desktops past the cap together.
        let half: Vec<AxNode> = (0..MAX_ICONS / 2 + 1).map(|i| icon(i as f64, 0.0)).collect();
        assert!(matches!(desktop_icons(true, &[desktop(half.clone()), desktop(half)]), DesktopIcons::TooMany(_)));
    }

    #[test]
    fn finders_desktop_setting_reads_as_defaults_writes_it() {
        assert!(finder_desktop_on(None));
        assert!(finder_desktop_on(Some("1")));
        assert!(finder_desktop_on(Some("true")));
        assert!(!finder_desktop_on(Some("0")));
        assert!(!finder_desktop_on(Some("false")));
        assert!(!finder_desktop_on(Some("NO")));
    }

    #[test]
    fn the_patch_rectangle_is_whole_points() {
        let r = patch_rect(&[Rect::new(2559.4, 1331.25, 1200.0, 900.0)], MAIN, 60.5).expect("a sliver");
        assert_eq!(r, Rect::new(2498.0, 1270.0, 62.0, 170.0));
        for v in [r.x, r.y, r.w, r.h] {
            assert_eq!(v.fract(), 0.0, "{r:?}");
        }
        assert!(MAIN.contains_rect(&r));
    }

    #[test]
    fn never_on_a_full_screen_space() {
        for ordered in [None, Some(false), Some(true)] {
            let f = PatchFacts { ordered, fullscreen: true, ..facts() };
            assert_eq!(visibility(&f), Err("a full-screen Space shows"), "{ordered:?}");
        }
    }

    #[test]
    fn a_rect_that_barely_shrinks_stays_and_one_that_grows_moves() {
        let wide = patch_rect(&[sliver(32.0), sliver(52.0)], MAIN, 60.0).expect("slivers");
        let narrow = patch_rect(&[sliver(32.0)], MAIN, 60.0).expect("a sliver");
        assert_ne!(wide, narrow);
        // Safari's sliver goes (an unpark): the patch keeps its rectangle.
        assert_eq!(steady_rect(Some(wide), narrow), wide);
        // It comes back: the same rectangle again, the panel stays put.
        assert_eq!(steady_rect(Some(wide), wide), wide);
        // Fresh, or growing past the one in use: the new one.
        assert_eq!(steady_rect(None, narrow), narrow);
        assert_eq!(steady_rect(Some(narrow), wide), wide);
        // Shrinking a lot (most slivers gone): the new one.
        let small = Rect::new(2500.0, 1300.0, 60.0, 140.0);
        let big = Rect::new(2300.0, 1000.0, 260.0, 440.0);
        assert_eq!(steady_rect(Some(big), small), small);
    }

    #[test]
    fn an_unknown_order_shows_against_the_parked_window_whatever_windows_do() {
        // Never shown yet, a maximized window over the corner: shown, ordered against the
        // topmost parked window, checked at the next plan.
        let f = PatchFacts {
            ordered: None,
            can_order: true,
            shown: vec![Rect::new(0.0, 0.0, 2560.0, 1440.0)],
            dragging: true,
            transition: true,
            ..facts()
        };
        assert!(visibility(&f).is_ok());
        // Found wrong: the fallback, and the window keeps it off.
        assert_eq!(visibility(&PatchFacts { ordered: Some(false), ..f.clone() }), Err("a window may be dragged"));
        // Nothing to order against: the fallback.
        assert!(visibility(&PatchFacts { can_order: false, ..f.clone() }).is_err());
        // Overviews, full screen and icons still win.
        assert!(visibility(&PatchFacts { overview: true, ..f.clone() }).is_err());
        assert!(visibility(&PatchFacts { items: None, ..f }).is_err());
    }

    #[test]
    fn the_order_is_known_only_once_checked_against_the_window_wanted() {
        let mut st = OrderState::default();
        // Never shown: unknown, so it shows and is ordered against window 7.
        assert_eq!(order_step(&mut st, Some(7), None), OrderStep { ordered: None, reorder: false });
        // On screen against 7 and right.
        assert_eq!(order_step(&mut st, Some(7), Some(true)), OrderStep { ordered: Some(true), reorder: false });
        // A parked window raised over it: ordered again, unknown meanwhile (still shown).
        assert_eq!(order_step(&mut st, Some(7), Some(false)), OrderStep { ordered: None, reorder: true });
        assert_eq!(order_step(&mut st, Some(7), None), OrderStep { ordered: None, reorder: false }, "the reorder not applied yet");
        // Still wrong after the reorder: macOS refuses; the fallback from now on for 7.
        assert_eq!(order_step(&mut st, Some(7), Some(false)), OrderStep { ordered: Some(false), reorder: false });
        assert_eq!(order_step(&mut st, Some(7), None).ordered, Some(false), "hidden, refused: no new try over windows");
        // Another window to order against: unknown again, tried.
        assert_eq!(order_step(&mut st, Some(8), None).ordered, None);
        assert_eq!(st.refused, None);
        // Wrong at the first check against a window: refused at once.
        let mut st = OrderState::default();
        assert_eq!(order_step(&mut st, Some(9), Some(false)).ordered, Some(false));
        // Right later (it showed by the fallback, ordered against 9 all the same): known right.
        assert_eq!(order_step(&mut st, Some(9), Some(true)).ordered, Some(true));
        // No parked window under it: nothing to order against.
        assert_eq!(order_step(&mut st, None, None), OrderStep { ordered: None, reorder: false });
    }

    #[test]
    fn unordered_it_shows_only_with_the_corner_clear_and_still() {
        for (ordered, can_order) in [(None, false), (Some(false), true), (Some(false), false)] {
            let f = PatchFacts { ordered, can_order, ..facts() };
            assert!(visibility(&f).is_ok());
            assert!(visibility(&PatchFacts { dragging: true, ..f.clone() }).is_err());
            assert!(visibility(&PatchFacts { transition: true, ..f.clone() }).is_err());
            assert!(visibility(&PatchFacts { shown: vec![Rect::new(2000.0, 1000.0, 600.0, 400.0)], ..f.clone() }).is_err());
            assert!(visibility(&PatchFacts { shown: vec![Rect::new(0.0, 0.0, 600.0, 400.0)], ..f }).is_ok(), "a window elsewhere");
        }
    }
}
