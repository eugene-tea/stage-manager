//! The wallpaper corner patch (`[windows] corner_patch`, a prototype, off by default): a
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
//!   ([`ordered_right`]): an activation raising a parked window, or a hide and show, can undo
//!   it at any time. Ordered right, every shown window stays above the patch on its own; the
//!   patch may show whenever slivers are there. Found wrong after being right, it is ordered
//!   again.
//! - **Not ordered right** (the fallback): the patch sits above every normal window there,
//!   so it shows only while no shown window reaches its rectangle, no window is being
//!   dragged and no switch is running ([`visibility`]).
//! - **Desktop items.** The patch draws the picture only: a Finder desktop icon under it
//!   (read through Accessibility, [`item_reach`]) keeps it off. Widgets' places are not
//!   public; with widgets shown on the desktop the patch would cover one in the corner (logged).
//! - Never during Mission Control, App Exposé or Show Desktop (its panel is `Transient`, and
//!   the overview watch hides it), on a full-screen app's Space, nor once the app stands
//!   down (nothing parked).

use std::collections::HashSet;

use super::geometry::Rect;

/// How far (points) a window's shadow reaches past its frame: an active window's shadow is
/// the largest (unmeasured on the owner's displays; to be confirmed by the live check).
pub const DEFAULT_REACH: f64 = 60.0;

/// The patch's rectangle: every sliver (`slivers`: parked windows' bounds as listed) that
/// shows on `display`, grown by `reach` on every side, kept on the display. `None` when no
/// sliver shows.
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
    out.and_then(|r| r.intersect(&display))
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
}

/// True when the patch (`patch`, at `rect`) sits above every parked window (`parked`) that
/// reaches its rectangle and below every other normal window that does: what ordering it
/// relative to the topmost parked window should give. False when the patch is not listed.
pub fn ordered_right(front_to_back: &[Listed], patch: u32, rect: Rect, parked: &HashSet<u32>) -> bool {
    let Some(at) = front_to_back.iter().position(|w| w.id == patch) else { return false };
    front_to_back.iter().enumerate().filter(|(_, w)| w.id != patch && w.layer == 0 && w.bounds.intersect(&rect).is_some()).all(
        |(i, w)| if parked.contains(&w.id) { i > at } else { i < at },
    )
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
    /// The patch is ordered right now ([`ordered_right`] on the current list); `None` before
    /// the patch exists.
    pub ordered: Option<bool>,
    /// A full-screen app's Space shows.
    pub fullscreen: bool,
    /// Shown (not parked) normal windows on screen, any app's.
    pub shown: Vec<Rect>,
    /// A window is being dragged, or the left button is down.
    pub dragging: bool,
    /// A switch's transition is running.
    pub transition: bool,
    /// Finder's desktop icons with their labels ([`item_reach`]), as last read.
    pub items: Vec<Rect>,
}

/// How far past a desktop icon's image (Accessibility's `AXImage` frame) its label and
/// selection reach: sideways, above, below (two lines of label).
pub const ITEM_REACH: (f64, f64, f64) = (36.0, 6.0, 44.0);

/// A desktop icon's image frame grown by its label ([`ITEM_REACH`]).
pub fn item_reach(icon: Rect) -> Rect {
    let (side, above, below) = ITEM_REACH;
    Rect::new(icon.x - side, icon.y - above, icon.w + 2.0 * side, icon.h + above + below)
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
    if f.items.iter().any(|i| i.intersect(&rect).is_some()) {
        return Err("a desktop icon lies under the corner");
    }
    if f.ordered == Some(true) {
        return Ok(rect);
    }
    // Above every normal window: only where none reaches, and never while windows move.
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
        let shown = Listed { id: 1, layer: 0, bounds: Rect::new(2000.0, 1000.0, 600.0, 400.0) };
        let patch = Listed { id: 9, layer: 0, bounds: rect };
        let p1 = Listed { id: 7, layer: 0, bounds: sliver(52.0) };
        let p2 = Listed { id: 8, layer: 0, bounds: sliver(32.0) };
        let elsewhere = Listed { id: 2, layer: 0, bounds: Rect::new(0.0, 0.0, 300.0, 300.0) };
        let dock = Listed { id: 3, layer: 20, bounds: Rect::new(0.0, 1364.0, 2560.0, 76.0) };
        let parked: HashSet<u32> = [7, 8].into();
        assert!(ordered_right(&[dock, shown, patch, p1, elsewhere, p2], 9, rect, &parked));
        assert!(!ordered_right(&[p1, patch, p2], 9, rect, &parked), "a sliver above the patch");
        assert!(!ordered_right(&[patch, shown, p1], 9, rect, &parked), "the patch over a shown window");
        assert!(!ordered_right(&[shown, p1], 9, rect, &parked), "not listed");
    }

    fn facts() -> PatchFacts {
        PatchFacts { on: true, rect: Some(Rect::new(2440.0, 1240.0, 120.0, 200.0)), ..PatchFacts::default() }
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
        let f = PatchFacts { ordered: Some(true), items: vec![item_reach(icon)], ..facts() };
        assert_eq!(visibility(&f), Err("a desktop icon lies under the corner"));
        let away = Rect::new(2000.0, 100.0, 48.0, 48.0);
        assert!(visibility(&PatchFacts { items: vec![item_reach(away)], ..f }).is_ok());
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
    fn unordered_it_shows_only_with_the_corner_clear_and_still() {
        for ordered in [None, Some(false)] {
            let f = PatchFacts { ordered, ..facts() };
            assert!(visibility(&f).is_ok());
            assert!(visibility(&PatchFacts { dragging: true, ..f.clone() }).is_err());
            assert!(visibility(&PatchFacts { transition: true, ..f.clone() }).is_err());
            assert!(visibility(&PatchFacts { shown: vec![Rect::new(2000.0, 1000.0, 600.0, 400.0)], ..f.clone() }).is_err());
            assert!(visibility(&PatchFacts { shown: vec![Rect::new(0.0, 0.0, 600.0, 400.0)], ..f }).is_ok(), "a window elsewhere");
        }
    }
}
