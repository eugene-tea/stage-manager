//! The wallpaper corner patch (`[windows] corner_patch`, a prototype, off by default): a
//! small panel over the park corner that shows what lies under the parked windows' slivers
//! (the desktop picture, Finder's desktop icons, desktop widgets), captured live, so the
//! corner looks empty as Apple's does. This module is its geometry and when it may show.
//!
//! - **Where** ([`patch_rect`]): the parked windows' slivers as the window server lists them,
//!   each grown by the shadow's reach ([`DEFAULT_REACH`], `[windows] corner_patch_reach`),
//!   on the display.
//! - **Ordering.** The patch is a normal-level window ordered just above the topmost parked
//!   window under it (`orderWindow:relativeTo:` with another app's window number). Whether
//!   macOS honours that across processes is checked after every ordering against the window
//!   server's list ([`ordered_right`]). Ordered right, every shown window stays above the
//!   patch on its own; the patch may show whenever slivers are there.
//! - **Not ordered right** (the fallback): the patch sits above every normal window there,
//!   so it shows only while no shown window reaches its rectangle, no window is being
//!   dragged and no switch is running ([`visibility`]).
//! - Never during Mission Control, App Exposé or Show Desktop (its panel is `Transient`, and
//!   the overview watch hides it), nor once the app stands down (nothing parked).

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
    /// `[windows] corner_patch`, the app managing windows and captures allowed.
    pub on: bool,
    /// The patch's rectangle ([`patch_rect`]).
    pub rect: Option<Rect>,
    /// Mission Control, App Exposé or Show Desktop is up (or settling).
    pub overview: bool,
    /// The last ordering was found right ([`ordered_right`]); `None` before any check.
    pub ordered: Option<bool>,
    /// Shown (not parked) normal windows on screen, any app's.
    pub shown: Vec<Rect>,
    /// A window is being dragged, or the left button is down.
    pub dragging: bool,
    /// A switch's transition is running.
    pub transition: bool,
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
