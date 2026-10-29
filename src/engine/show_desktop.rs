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
//!   desktop: then it hides until it is back in place. As `Transient` it was pushed aside
//!   (the owner's log of 10-29: 3 to 10 pt); the panel is `Stationary` since part 2
//!   ([`crate::ui::panels::corner_patch`]: "unaffected by Exposé", like the desktop
//!   picture), and this stays as the fallback in case macOS moves it anyway.
//! - **What it covers** (part 2, [`PushedCover`]): Show Desktop pushes the parked windows
//!   too, from the corner partly into view (10-29: a window's rounded corner, about 90x130
//!   pt, with its shadow). While it is up the patch covers what of them is listed on the
//!   display, grown by the shadow's reach ([`pushed_rect`]), within the still's block (no
//!   still is taken while an overview shows: the block taken before is what there is), and
//!   only grows. It shrinks back to the slivers' rectangle at the first look that lists every
//!   parked window back at its park frame, never before.
//! - **The close** ([`CloseWatch`]): Show Desktop's own window going, or, sooner, the first
//!   look at the window list that shows the Desktop's windows moving again once they had come
//!   to rest (the slide back).
//! - **Looked at on the fast tick** ([`fast_look`]) while Show Desktop is listed or expected
//!   from a wallpaper click, or its cover held, and the patch is on: the close is seen within
//!   20 ms, not at the next once-a-second look, and the cover let go as soon as the windows
//!   are back.
//!
//! Mission Control and App Exposé keep their rule: a patch shown when one opens is left as it
//! is (the panel is `Stationary` since part 2, so it stays drawn in place like the desktop
//! picture under their windows, where `Transient` had macOS hide it; either way it is there
//! the moment they close), and so does a Space change (each Desktop's panel slides with it:
//! `Stationary` is the Exposé group's choice, the Spaces group stays empty).

use std::collections::{HashMap, HashSet};
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

/// Where the panel is (`listed`: its bounds as listed; `placed`: where the app last put it;
/// `asked`: where the last plan asked for it, which the main thread may have put it at just
/// before publishing `placed`) relative to `display`.
pub fn panel_place(listed: Option<Rect>, placed: Option<Rect>, asked: Option<Rect>, display: Rect) -> PanelPlace {
    let (Some(l), Some(p)) = (listed, placed) else { return PanelPlace::Unknown };
    if !moved(&l, &p) || asked.is_some_and(|a| !moved(&l, &a)) {
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
/// listed or expected, or the patch's cover is held past it ([`PushedCover::holding`])
/// (`show_desktop`), and the patch is on with windows managed (`patch_on`), so its close is
/// seen within a fast tick, and the cover let go at the first look with the windows back.
pub fn fast_look(show_desktop: bool, patch_on: bool) -> bool {
    show_desktop && patch_on
}

/// A held cover is let go this long after Show Desktop stopped being listed or expected even
/// if a parked window is not listed back at its park frame (macOS put it a little elsewhere).
pub const HOLD_AFTER: Duration = Duration::from_secs(2);

/// The patch's rectangle while Show Desktop is up: what of each parked window (`parts`, its
/// bounds as listed, wherever Show Desktop put it) shows on `display`, grown by `reach` (its
/// shadow; [`super::corner_patch::patch_rect`]), each kept within `cap` (the still's block),
/// together. `None` when nothing of them shows within the cap.
pub fn pushed_rect(parts: &[Rect], display: Rect, reach: f64, cap: Rect) -> Option<Rect> {
    let area = cap.intersect(&display)?;
    parts
        .iter()
        .filter_map(|p| super::corner_patch::patch_rect(std::slice::from_ref(p), display, reach))
        .filter_map(|r| r.intersect(&area))
        .reduce(|a, b| a.union(&b))
}

/// What one plan knows for [`PushedCover::see`].
#[derive(Debug, Clone)]
pub struct CoverLook<'a> {
    /// Show Desktop is listed or expected ([`super::overview::OverviewWatch::show_desktop_up`]).
    pub up: bool,
    /// This Desktop's parked windows as listed (normal layer), wherever they are.
    pub parked: &'a [(u32, Rect)],
    /// Those of them listed in the park zone.
    pub in_zone: &'a HashSet<u32>,
    /// The patch's rectangle in use before (this Desktop's).
    pub in_use: Option<Rect>,
    /// The main display.
    pub display: Rect,
    /// `[windows] corner_patch_reach`.
    pub reach: f64,
    /// What the patch may cover: the park corner's square within the still's block.
    pub cap: Rect,
    /// This look's time.
    pub now: Instant,
}

/// The patch's rectangle as [`PushedCover::see`] decides it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cover {
    /// The slivers' rectangle, as at any time.
    Usual,
    /// The rectangle held through Show Desktop.
    Held(Rect),
    /// The slivers' rectangle again, from this look on: taken as it is (not kept at the bigger
    /// one in use, [`super::corner_patch::steady_rect`]).
    Released,
}

/// A cover held through one Show Desktop.
#[derive(Debug, Clone, Copy)]
struct Holding {
    /// The rectangle, grown at every look.
    rect: Option<Rect>,
    /// A parked window was seen away from its park frame.
    left: bool,
    /// When Show Desktop stopped being up.
    ended: Option<Instant>,
}

/// The patch's rectangle through Show Desktop (part 2): the slivers' rectangle grown to what
/// macOS pushes of the parked windows into view, until they are back.
#[derive(Debug, Clone, Default)]
pub struct PushedCover {
    /// The parked windows' frames as listed at rest in the zone outside Show Desktop: their
    /// park frames.
    home: HashMap<u32, Rect>,
    /// The parked windows as listed at the last look.
    last: HashMap<u32, Rect>,
    /// The cover held through this Show Desktop.
    held: Option<Holding>,
    /// Let go while Show Desktop is still up (its window lingers): nothing more is held until
    /// it is over.
    done: bool,
}

impl PushedCover {
    /// Takes a look ([`CoverLook`]) and says which rectangle the patch takes.
    pub fn see(&mut self, l: &CoverLook) -> Cover {
        // Back: listed at its park frame; one parked while Show Desktop was up has none known,
        // and is back once in the zone and still since the last look.
        let back = |id: &u32, b: &Rect| match self.home.get(id) {
            Some(h) => !moved(h, b),
            None => l.in_zone.contains(id) && self.last.get(id).is_some_and(|a| !moved(a, b)),
        };
        let all_back = l.parked.iter().all(|(id, b)| back(id, b));
        self.last = l.parked.iter().copied().collect();
        if self.done {
            if !l.up {
                self.done = false;
                self.rest(l);
            }
            return Cover::Usual;
        }
        let h = match self.held.as_mut() {
            Some(h) => h,
            None if !l.up => {
                self.rest(l);
                return Cover::Usual;
            }
            None => self.held.insert(Holding { rect: l.in_use, left: false, ended: None }),
        };
        h.left |= !all_back;
        if l.up {
            h.ended = None;
            if h.left && all_back {
                self.held = None;
                self.done = true;
                return Cover::Released;
            }
        } else {
            let ended = *h.ended.get_or_insert(l.now);
            if !h.left {
                // Nothing was pushed (macOS showed no Show Desktop): the rectangle never grew.
                self.held = None;
                self.rest(l);
                return Cover::Usual;
            }
            if all_back || l.now.saturating_duration_since(ended) >= HOLD_AFTER {
                self.held = None;
                self.rest(l);
                return Cover::Released;
            }
        }
        let parts: Vec<Rect> = l.parked.iter().map(|(_, b)| *b).collect();
        let pushed = pushed_rect(&parts, l.display, l.reach, l.cap);
        h.rect = match (h.rect, pushed) {
            (Some(a), Some(b)) => Some(a.union(&b)),
            (a, b) => a.or(b),
        };
        h.rect.map_or(Cover::Usual, Cover::Held)
    }

    /// True while a cover is held (the fast tick looks on until it is let go).
    pub fn holding(&self) -> bool {
        self.held.is_some()
    }

    /// At rest outside Show Desktop: the parked windows in the zone are at their park frames.
    fn rest(&mut self, l: &CoverLook) {
        self.home = l.parked.iter().filter(|(id, _)| l.in_zone.contains(id)).copied().collect();
    }
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
        assert_eq!(panel_place(Some(PATCH), Some(PATCH), None, MAIN), PanelPlace::InPlace);
        let nudged = Rect::new(PATCH.x + 0.5, PATCH.y, PATCH.w, PATCH.h);
        assert_eq!(panel_place(Some(nudged), Some(PATCH), None, MAIN), PanelPlace::InPlace, "half a point is no move");
        let off = Rect::new(2600.0, 1328.0, 61.0, 112.0);
        assert_eq!(panel_place(Some(off), Some(PATCH), None, MAIN), PanelPlace::Away);
        // Show Desktop leaves ~20 pt of a pushed window on the display (his log: x=2538).
        let edge = Rect::new(2540.0, 1328.0, 61.0, 112.0);
        assert_eq!(panel_place(Some(edge), Some(PATCH), None, MAIN), PanelPlace::Moved);
        assert_eq!(panel_place(None, Some(PATCH), None, MAIN), PanelPlace::Unknown, "not listed");
        assert_eq!(panel_place(Some(PATCH), None, None, MAIN), PanelPlace::Unknown, "never put anywhere");
        // Put at a new rectangle the plan asked for, before the main thread said so.
        let grown = Rect::new(2380.0, 1200.0, 180.0, 240.0);
        assert_eq!(panel_place(Some(grown), Some(PATCH), Some(grown), MAIN), PanelPlace::InPlace);
        assert_eq!(panel_place(Some(edge), Some(PATCH), Some(grown), MAIN), PanelPlace::Moved);
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

    /// The still's block at the corner (`desktop_shot::BLOCK`).
    const CAP: Rect = Rect::new(2240.0, 1120.0, 320.0, 320.0);
    /// A parked window at its park frame: a one-point sliver 52 pt tall (his 10-29 corner).
    const PARKED: Rect = Rect::new(2559.0, 1388.0, 1500.0, 1100.0);
    /// The same window as Show Desktop pushes it partly into view (his 10-29 report: about
    /// 90x130 pt of its corner).
    const PUSHED: Rect = Rect::new(2470.0, 1310.0, 1500.0, 1100.0);

    #[test]
    fn the_pushed_parts_are_covered_with_their_shadow_within_the_still() {
        let r = pushed_rect(&[PUSHED], MAIN, 60.0, CAP).expect("a part shows");
        assert_eq!(r, Rect::new(2410.0, 1250.0, 150.0, 190.0), "90x130 grown by 60, kept on the display");
        // Two windows: one rectangle holding both.
        let other = Rect::new(2500.0, 1360.0, 900.0, 700.0);
        let both = pushed_rect(&[PUSHED, other], MAIN, 60.0, CAP).expect("parts");
        assert!(both.contains_rect(&r) && both.contains_rect(&Rect::new(2440.0, 1300.0, 120.0, 140.0)), "{both:?}");
        // Pushed further than the still's block holds: only the block is covered.
        let far = Rect::new(2100.0, 1000.0, 1500.0, 1100.0);
        assert_eq!(pushed_rect(&[far], MAIN, 60.0, CAP), Some(CAP));
        // A part wholly outside the block (a window listed mid-screen) adds nothing.
        let mid = Rect::new(800.0, 400.0, 600.0, 400.0);
        assert_eq!(pushed_rect(&[mid, PUSHED], MAIN, 60.0, CAP), Some(r));
        assert_eq!(pushed_rect(&[mid], MAIN, 60.0, CAP), None);
        // Off the display: nothing.
        assert_eq!(pushed_rect(&[Rect::new(2600.0, 1300.0, 500.0, 500.0)], MAIN, 60.0, CAP), None);
    }

    fn look<'a>(up: bool, parked: &'a [(u32, Rect)], in_zone: &'a HashSet<u32>, now: Instant) -> CoverLook<'a> {
        CoverLook { up, parked, in_zone, in_use: Some(PATCH), display: MAIN, reach: 60.0, cap: CAP, now }
    }

    #[test]
    fn the_cover_grows_through_show_desktop_and_shrinks_when_the_windows_are_back() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let zone: HashSet<u32> = [1].into();
        let none: HashSet<u32> = HashSet::new();
        let home = [(1, PARKED)];
        let pushed = [(1, PUSHED)];
        let halfway = [(1, Rect::new(2515.0, 1350.0, 1500.0, 1100.0))];
        let mut c = PushedCover::default();
        assert_eq!(c.see(&look(false, &home, &zone, ms(0))), Cover::Usual, "at rest: the slivers' rectangle");
        assert!(!c.holding());
        // The click: expected, nothing moved yet. The rectangle in use is held.
        assert_eq!(c.see(&look(true, &home, &zone, ms(10))), Cover::Held(PATCH));
        assert!(c.holding());
        // Pushed into view: the rectangle grows to cover it.
        let grown = PATCH.union(&Rect::new(2410.0, 1250.0, 150.0, 190.0));
        assert_eq!(c.see(&look(true, &pushed, &none, ms(200))), Cover::Held(grown));
        assert_eq!(c.see(&look(true, &pushed, &none, ms(900))), Cover::Held(grown), "at rest pushed");
        // The slide back: never smaller before the window is back.
        assert_eq!(c.see(&look(true, &halfway, &zone, ms(1000))), Cover::Held(grown));
        assert_eq!(c.see(&look(false, &halfway, &zone, ms(1020))), Cover::Held(grown), "Show Desktop's window gone first");
        // Back at its park frame: the slivers' rectangle, at once.
        assert_eq!(c.see(&look(false, &home, &zone, ms(1040))), Cover::Released);
        assert!(!c.holding());
        assert_eq!(c.see(&look(false, &home, &zone, ms(1060))), Cover::Usual);
    }

    #[test]
    fn a_cover_let_go_while_show_desktop_lingers_is_not_held_again() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let zone: HashSet<u32> = [1].into();
        let home = [(1, PARKED)];
        let pushed = [(1, PUSHED)];
        let mut c = PushedCover::default();
        c.see(&look(false, &home, &zone, ms(0)));
        c.see(&look(true, &home, &zone, ms(10)));
        assert!(matches!(c.see(&look(true, &pushed, &zone, ms(300))), Cover::Held(_)));
        assert_eq!(c.see(&look(true, &home, &zone, ms(1000))), Cover::Released, "back while its window is still listed");
        assert_eq!(c.see(&look(true, &home, &zone, ms(1100))), Cover::Usual);
        assert!(!c.holding());
        assert_eq!(c.see(&look(false, &home, &zone, ms(1300))), Cover::Usual);
        // The next Show Desktop holds again.
        assert_eq!(c.see(&look(true, &home, &zone, ms(2000))), Cover::Held(PATCH));
    }

    #[test]
    fn a_window_not_back_is_let_go_a_while_after_show_desktop_ends() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let zone: HashSet<u32> = [1].into();
        let home = [(1, PARKED)];
        let pushed = [(1, PUSHED)];
        // macOS puts it back a point lower, and it stays there.
        let near = [(1, Rect::new(2559.0, 1390.0, 1500.0, 1100.0))];
        let mut c = PushedCover::default();
        c.see(&look(false, &home, &zone, ms(0)));
        c.see(&look(true, &pushed, &zone, ms(300)));
        assert!(matches!(c.see(&look(false, &near, &zone, ms(1000))), Cover::Held(_)));
        assert!(matches!(c.see(&look(false, &near, &zone, ms(2999))), Cover::Held(_)));
        assert_eq!(c.see(&look(false, &near, &zone, ms(3000))), Cover::Released);
        // Its new place is its park frame from now on.
        c.see(&look(true, &near, &zone, ms(4000)));
        c.see(&look(true, &pushed, &zone, ms(4300)));
        assert_eq!(c.see(&look(false, &near, &zone, ms(4600))), Cover::Released);
    }

    #[test]
    fn nothing_pushed_nothing_held_and_a_window_parked_meanwhile_counts_once_still() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let zone: HashSet<u32> = [1].into();
        let home = [(1, PARKED)];
        let mut c = PushedCover::default();
        c.see(&look(false, &home, &zone, ms(0)));
        // A click macOS answered with nothing: the expectation runs out, the rectangle in use.
        assert_eq!(c.see(&look(true, &home, &zone, ms(10))), Cover::Held(PATCH));
        assert_eq!(c.see(&look(false, &home, &zone, ms(1500))), Cover::Usual);
        assert!(!c.holding());
        // A window parked while Show Desktop is up has no park frame known: back once in the
        // zone and still.
        let zone2: HashSet<u32> = [1, 2].into();
        let two = |b: Rect| [(1, PARKED), (2, b)];
        c.see(&look(true, &home, &zone, ms(2000)));
        let a = two(PUSHED);
        assert!(matches!(c.see(&look(true, &a, &zone, ms(2100))), Cover::Held(_)));
        let b = two(Rect::new(2559.0, 1400.0, 800.0, 600.0));
        assert!(matches!(c.see(&look(false, &b, &zone2, ms(2200))), Cover::Held(_)), "in the zone, moved since the last look");
        assert_eq!(c.see(&look(false, &b, &zone2, ms(2220))), Cover::Released, "still: back");
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
