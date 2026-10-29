//! The corner patch through macOS's Show Desktop (round show-desktop-patch; the owner's
//! report of 10-29: with "Click wallpaper to reveal desktop: Always", the click that brings
//! the windows back showed the parked windows' slivers for a moment).
//!
//! The patch used to hide for the whole of Show Desktop (10-13: shown through it, it sat
//! over a pushed window's edge, a lighter block in the corner), and came back only when a
//! once-a-second look saw Show Desktop's own window gone, after macOS had slid the windows
//! back: the slivers were bare until then (his log: 1.2 s).
//!
//! - **With a still of the desktop** ([`super::patch_still`]) the patch shows what the
//!   desktop shows there, icons and widgets included, so it has no reason of its own to hide:
//!   it is planned through Show Desktop as at any time ([`gate`]). The rules that keep it
//!   from showing over a window still hold there: it shows only while ordered just above the
//!   parked windows (every shown window reaching it, pushed aside or not, listed in front of
//!   it), or where no shown window reaches it ([`super::corner_patch::visibility`]), so a
//!   pushed window's edge in the corner (10-13) is never under it.
//! - **With the file's picture** (no Screen Recording) it hides through Show Desktop as
//!   before, and comes back at the first sign of the close ([`CloseWatch`]).
//! - **Where macOS puts the panel** ([`panel_place`]): its own listing (shown or hidden by
//!   its alpha, the panel stays ordered in) tells whether macOS leaves it where it was put,
//!   slides it wholly off the display with the windows (it comes back with them), or pushes
//!   it aside with a part still on the display, where it would show the wrong part of the
//!   desktop: then it hides until it is back in place.
//! - **The close** ([`CloseWatch`]): Show Desktop's own window going, or, sooner, the first
//!   look at the window list that shows the Desktop's windows moving again once they had come
//!   to rest (the slide back).
//! - **Looked at on the fast tick** ([`fast_look`]) while Show Desktop is listed or expected
//!   from a wallpaper click and the patch is on: the close is seen within 20 ms, not at the
//!   next once-a-second look.
//!
//! Mission Control and App Exposé keep their rule (the panel is `Transient`: macOS hides it
//! while they show and shows it with the windows, so a patch shown when one opens is left as
//! it is), and so does a Space change (each Desktop's panel slides with it).

use std::time::{Duration, Instant};

use super::geometry::Rect;

/// The patch's reason to stay off while Show Desktop is up (the file's picture) or Mission
/// Control shows: the same words as before this round (the scripted checks read them).
pub const OVERVIEW_UP: &str = "an overview is up";

/// The patch's reason to stay off while macOS has pushed its panel aside with a part still on
/// the display ([`PanelPlace::Moved`]).
pub const MOVED_ASIDE: &str = "macOS moved the patch aside";

/// Movement under this (points, any edge) is no movement.
pub const STIR: f64 = 1.0;

/// Show Desktop's windows are at rest once no look has seen one move for this long: the
/// push aside has ended, and the next move is the slide back.
pub const AT_REST: Duration = Duration::from_millis(120);

/// A close seen from the windows' movement alone holds this long while Show Desktop's own
/// window stays listed; then it is taken for a window moving by itself, and the windows are
/// watched for rest again.
pub const CLOSE_MAX: Duration = Duration::from_millis(1500);

/// Where the window server lists the patch's panel during Show Desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelPlace {
    /// Not known: the panel is not listed, or was never put anywhere.
    Unknown,
    /// Where the app put it (within [`STIR`]).
    InPlace,
    /// Moved wholly off the display: nothing of it shows; it comes back with the windows.
    Away,
    /// Moved, with a part still on the display: it would show the wrong part of the desktop.
    Moved,
}

impl PanelPlace {
    /// Its name for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "not listed",
            Self::InPlace => "left in place",
            Self::Away => "slid off the display",
            Self::Moved => "pushed aside, partly on the display",
        }
    }
}

/// Where the panel is (`listed`: its bounds as listed; `placed`: where the app last put it)
/// relative to `display`.
pub fn panel_place(listed: Option<Rect>, placed: Option<Rect>, display: Rect) -> PanelPlace {
    let (Some(l), Some(p)) = (listed, placed) else { return PanelPlace::Unknown };
    if !moved(&l, &p) {
        PanelPlace::InPlace
    } else if l.intersect(&display).is_none() {
        PanelPlace::Away
    } else {
        PanelPlace::Moved
    }
}

/// Whether `b` differs from `a` by more than [`STIR`] on any edge.
fn moved(a: &Rect, b: &Rect) -> bool {
    (a.x - b.x).abs() > STIR || (a.y - b.y).abs() > STIR || (a.max_x() - b.max_x()).abs() > STIR || (a.max_y() - b.max_y()).abs() > STIR
}

/// What the patch's plan knows of an overview ([`gate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevealFacts {
    /// Mission Control or App Exposé shows (or carries through its exit).
    pub mission_control: bool,
    /// Show Desktop is listed, or expected from a wallpaper click.
    pub show_desktop: bool,
    /// Show Desktop is closing ([`CloseWatch`]).
    pub closing: bool,
    /// A still of the desktop stands for the corner now
    /// ([`super::patch_still::Still::Ready`]).
    pub still: bool,
    /// Where the panel is listed ([`panel_place`]).
    pub panel: PanelPlace,
}

/// Whether the patch is planned as usual, or why an overview keeps it off. Mission Control
/// and App Exposé: off (a patch shown when one opened is left to macOS before any plan).
/// Show Desktop: off while macOS has pushed the panel aside onto the display; otherwise
/// planned with a still of the desktop, and with the file's picture only once it closes.
pub fn gate(f: &RevealFacts) -> Result<(), &'static str> {
    if f.mission_control {
        return Err(OVERVIEW_UP);
    }
    if !f.show_desktop {
        return Ok(());
    }
    if f.panel == PanelPlace::Moved {
        return Err(MOVED_ASIDE);
    }
    if f.still || f.closing {
        return Ok(());
    }
    Err(OVERVIEW_UP)
}

/// Whether the WM thread looks at the window list on its fast tick: while Show Desktop is
/// listed or expected (`show_desktop`) and the patch is on with windows managed
/// (`patch_on`), so its close is seen within a fast tick.
pub fn fast_look(show_desktop: bool, patch_on: bool) -> bool {
    show_desktop && patch_on
}

/// What a look said about the close ([`CloseWatch::see`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closing {
    /// Not closing (or Show Desktop not listed).
    No,
    /// This look is the first sign: the windows move again after coming to rest.
    Began,
    /// Closing since an earlier look.
    Yes,
}

impl Closing {
    /// True when closing.
    pub fn is(self) -> bool {
        self != Self::No
    }
}

/// Watches the Desktop's windows while Show Desktop is listed: after the push aside, they
/// rest; the first look that sees one move again is the slide back.
#[derive(Debug, Clone, Default)]
pub struct CloseWatch {
    /// The windows (id, bounds as listed) at the last look, and when.
    last: Option<(Vec<(u32, Rect)>, Instant)>,
    /// Since when no look has seen a window move.
    rest_since: Option<Instant>,
    /// When the close was seen.
    closing: Option<Instant>,
}

impl CloseWatch {
    /// Takes a look at `now`: `listed`, Show Desktop listed; `windows`, the Desktop's windows
    /// with their bounds as listed. Windows that come or go do not count as moving.
    pub fn see(&mut self, listed: bool, windows: Vec<(u32, Rect)>, now: Instant) -> Closing {
        if !listed {
            *self = Self::default();
            return Closing::No;
        }
        let prev = self.last.replace((windows, now));
        if let Some(t) = self.closing {
            if now.saturating_duration_since(t) < CLOSE_MAX {
                return Closing::Yes;
            }
            // Show Desktop's window stayed: a window moved by itself. Watch for rest again.
            self.closing = None;
            self.rest_since = None;
        }
        let Some((before, at)) = prev else { return Closing::No };
        let now_listed = self.last.as_ref().map_or(&[][..], |(w, _)| w.as_slice());
        let stirred = now_listed.iter().any(|(id, b)| before.iter().any(|(i, a)| i == id && moved(a, b)));
        if !stirred {
            self.rest_since.get_or_insert(at);
            return Closing::No;
        }
        if self.rest_since.is_some_and(|t| now.saturating_duration_since(t) >= AT_REST) {
            self.closing = Some(now);
            self.rest_since = None;
            return Closing::Began;
        }
        // Still moving: the push aside.
        self.rest_since = None;
        Closing::No
    }

    /// True while closing (as of the last look).
    pub fn closing(&self) -> bool {
        self.closing.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    const PATCH: Rect = Rect::new(2499.0, 1328.0, 61.0, 112.0);

    fn facts() -> RevealFacts {
        RevealFacts { mission_control: false, show_desktop: true, closing: false, still: false, panel: PanelPlace::InPlace }
    }

    #[test]
    fn the_panel_is_found_in_place_slid_away_or_pushed_aside() {
        assert_eq!(panel_place(Some(PATCH), Some(PATCH), MAIN), PanelPlace::InPlace);
        let nudged = Rect::new(PATCH.x + 0.5, PATCH.y, PATCH.w, PATCH.h);
        assert_eq!(panel_place(Some(nudged), Some(PATCH), MAIN), PanelPlace::InPlace, "half a point is no move");
        let off = Rect::new(2600.0, 1328.0, 61.0, 112.0);
        assert_eq!(panel_place(Some(off), Some(PATCH), MAIN), PanelPlace::Away);
        // Show Desktop leaves ~20 pt of a pushed window on the display (his log: x=2538).
        let edge = Rect::new(2540.0, 1328.0, 61.0, 112.0);
        assert_eq!(panel_place(Some(edge), Some(PATCH), MAIN), PanelPlace::Moved);
        assert_eq!(panel_place(None, Some(PATCH), MAIN), PanelPlace::Unknown, "not listed");
        assert_eq!(panel_place(Some(PATCH), None, MAIN), PanelPlace::Unknown, "never put anywhere");
    }

    #[test]
    fn with_a_still_the_patch_is_planned_through_show_desktop() {
        let still = RevealFacts { still: true, ..facts() };
        assert_eq!(gate(&still), Ok(()), "up, in place");
        assert_eq!(gate(&RevealFacts { panel: PanelPlace::Away, ..still }), Ok(()), "slid off: it comes back with the windows");
        assert_eq!(gate(&RevealFacts { panel: PanelPlace::Unknown, ..still }), Ok(()));
        assert_eq!(gate(&RevealFacts { closing: true, ..still }), Ok(()), "closing");
        assert_eq!(gate(&RevealFacts { panel: PanelPlace::Moved, ..still }), Err(MOVED_ASIDE), "never the wrong part of the desktop");
        assert_eq!(gate(&RevealFacts { panel: PanelPlace::Moved, closing: true, ..still }), Err(MOVED_ASIDE), "not even while it slides back");
    }

    #[test]
    fn with_the_file_the_patch_hides_until_the_close() {
        assert_eq!(gate(&facts()), Err(OVERVIEW_UP), "up: hidden as before");
        assert_eq!(gate(&RevealFacts { panel: PanelPlace::Away, ..facts() }), Err(OVERVIEW_UP));
        assert_eq!(gate(&RevealFacts { closing: true, ..facts() }), Ok(()), "the first sign of the close");
        assert_eq!(gate(&RevealFacts { closing: true, panel: PanelPlace::Moved, ..facts() }), Err(MOVED_ASIDE));
    }

    #[test]
    fn mission_control_and_no_overview_keep_their_rules() {
        let none = RevealFacts { show_desktop: false, ..facts() };
        assert_eq!(gate(&none), Ok(()));
        assert_eq!(gate(&RevealFacts { panel: PanelPlace::Moved, ..none }), Ok(()), "the place counts only during Show Desktop");
        let mc = RevealFacts { mission_control: true, show_desktop: false, still: true, ..facts() };
        assert_eq!(gate(&mc), Err(OVERVIEW_UP), "Mission Control: as before, still or not");
    }

    #[test]
    fn the_fast_tick_looks_only_while_show_desktop_is_up_and_the_patch_is_on() {
        assert!(fast_look(true, true));
        assert!(!fast_look(true, false), "the patch off: nothing to bring back");
        assert!(!fast_look(false, true));
        assert!(!fast_look(false, false));
    }

    fn at(x: f64) -> Vec<(u32, Rect)> {
        vec![(1, Rect::new(x, 300.0, 800.0, 600.0)), (2, Rect::new(2540.0, 1400.0, 800.0, 600.0))]
    }

    #[test]
    fn the_close_is_the_first_move_after_the_windows_rest() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut w = CloseWatch::default();
        // The push aside, still moving when Show Desktop is first listed.
        assert_eq!(w.see(true, at(400.0), ms(0)), Closing::No);
        assert_eq!(w.see(true, at(-200.0), ms(20)), Closing::No, "the push");
        assert_eq!(w.see(true, at(-780.0), ms(40)), Closing::No, "the push ends");
        for n in 3..10 {
            assert_eq!(w.see(true, at(-780.0), ms(n * 20)), Closing::No, "at rest");
        }
        assert!(!w.closing());
        assert_eq!(w.see(true, at(-700.0), ms(200)), Closing::Began, "the slide back: the first look");
        assert_eq!(w.see(true, at(-300.0), ms(220)), Closing::Yes);
        assert!(w.closing());
        // Show Desktop's window goes: all over.
        assert_eq!(w.see(false, at(400.0), ms(600)), Closing::No);
        assert!(!w.closing());
    }

    #[test]
    fn a_move_too_soon_after_the_push_is_still_the_push() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut w = CloseWatch::default();
        assert_eq!(w.see(true, at(-780.0), ms(0)), Closing::No, "the first look has nothing to compare");
        assert_eq!(w.see(true, at(-780.0), ms(20)), Closing::No);
        assert_eq!(w.see(true, at(-790.0), ms(60)), Closing::No, "60 ms of rest is not rest");
        // A window that comes or goes is no move.
        let mut more = at(-790.0);
        more.push((3, Rect::new(10.0, 10.0, 100.0, 100.0)));
        assert_eq!(w.see(true, more, ms(80)), Closing::No);
        assert_eq!(w.see(true, at(-790.0), ms(250)), Closing::No);
        assert_eq!(w.see(true, at(-600.0), ms(260)), Closing::Began, "at rest from 60 ms (the look before the quiet ones) to 260");
    }

    #[test]
    fn a_window_moving_by_itself_closes_nothing_for_long() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut w = CloseWatch::default();
        w.see(true, at(-780.0), ms(0));
        w.see(true, at(-780.0), ms(200));
        assert_eq!(w.see(true, at(-770.0), ms(220)), Closing::Began);
        assert_eq!(w.see(true, at(-770.0), ms(1000)), Closing::Yes);
        // Show Desktop is still listed past CLOSE_MAX: not a close after all.
        assert_eq!(w.see(true, at(-770.0), ms(1800)), Closing::No);
        assert!(!w.closing());
        assert_eq!(w.see(true, at(-770.0), ms(2000)), Closing::No, "resting again");
        assert_eq!(w.see(true, at(-600.0), ms(2200)), Closing::Began, "the real slide back");
    }
}
