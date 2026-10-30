//! The corner patch drawn from a still of the desktop (round desktop-still-patch, review
//! PARKING.md "Medium term"): where Screen Recording is granted and window captures work, the
//! patch's picture is a ScreenCaptureKit still of the corner block showing only the
//! desktop-level windows (the wallpaper, Finder's icons, widgets;
//! [`crate::platform::desktop_shot`]), so it matches the screen by construction. The
//! exactness gate of the file's picture ([`super::wallpaper::decide`], the icons under the
//! corner in [`super::corner_patch::visibility`]) falls away while a still stands for the
//! corner; the file's picture is the fallback without one.
//!
//! - **Which picture** ([`source`]): a still that stands for the corner now
//!   ([`Still::Ready`]) first; while none does but one is on its way ([`Still::Coming`]: a
//!   Desktop arrived, an icon moved under the corner, the picture changed) the panel keeps
//!   what it shows, if a still drew it there, for at most [`HOLD_MAX`]; otherwise the file's
//!   picture, only where no desktop icon lies under the corner, as before.
//! - **When a still may be taken** ([`may_take`]): never while a switch runs, an overview is
//!   up (Mission Control, App Exposé, Show Desktop), a Space change is being recognized or a
//!   Desktop slides, nor within [`SETTLE`] of a Space change or of the still being dropped
//!   (the slide settles); on a full-screen Space not at all. Parked windows and our own
//!   panel are never in a still: it includes only desktop-level windows of other processes
//!   ([`crate::platform::capture::desktop_level`]).
//! - **When a new one is due** ([`crate::platform::desktop_shot::due`]): a Desktop arrived,
//!   the icons in the corner changed, a bigger block, [`AGAIN_AFTER_TOUCH`] after a press on
//!   the desktop or a release in the corner (a selection, a dropped icon), [`FADE`] after the
//!   wallpaper changed or the frame the clock predicts is due, else every 10 s while slivers
//!   show.
//!
//! Pure: the WM thread and the patch's panel ask it with what they know.

use std::time::Duration;

/// The longest a shown patch keeps its picture while a new still is on its way: past it, the
/// file's picture where it may stand, else the slivers (a still that does not come).
pub const HOLD_MAX: Duration = Duration::from_millis(1500);

/// No still within this of a Space change or of the still being dropped (an appearance or
/// display change): the Desktop's slide and the window server settle first.
pub const SETTLE: Duration = Duration::from_millis(400);

/// How long after the wallpaper changed, or after the frame the clock predicts is due, a
/// still is taken again: macOS crossfades from one picture or frame to the next over about a
/// second, and a still in between holds a blend.
pub const FADE: Duration = Duration::from_millis(1500);

/// How long after a press on the desktop or a release in the corner a still is taken again:
/// an icon selected, deselected or dropped there redraws by then.
pub const AGAIN_AFTER_TOUCH: Duration = Duration::from_millis(250);

/// The still of the desktop for the corner patch, as found at a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Still {
    /// One stands for the corner now (exact for this Desktop, display, icons and block, fresh,
    /// opaque): its serial.
    Ready(u64),
    /// None stands now, and one may be taken shortly: the patch holds what a still drew.
    Coming,
    /// None stands and none is coming: the file's picture, as without Screen Recording.
    #[default]
    Absent,
}

/// Why no still is taken now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    /// Passes shortly (a Space change settling, a switch running): one is still coming.
    Wait(&'static str),
    /// Not now at all (captures off, no Screen Recording, a full-screen Space, the screen
    /// locked).
    No(&'static str),
}

impl Block {
    /// The reason, for the log.
    pub fn why(self) -> &'static str {
        match self {
            Block::Wait(w) | Block::No(w) => w,
        }
    }
}

/// What the WM thread knows when a still may be taken ([`may_take`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TakeFacts {
    /// A switch's transition runs (its motion on the overlay, windows held in the corner).
    pub switching: bool,
    /// Mission Control, App Exposé or Show Desktop is up, or a wallpaper click's reveal holds.
    pub overview: bool,
    /// A Space change is being recognized.
    pub space_pending: bool,
    /// The window server lists the parked windows slid aside: a Desktop slides.
    pub slide: bool,
    /// A full-screen app's Space shows.
    pub fullscreen: bool,
    /// Since the last Space change (`None`: none in this run).
    pub since_space_change: Option<Duration>,
    /// Since the still was last dropped (a Space, appearance, display or picture change).
    pub since_dropped: Option<Duration>,
}

/// Whether a still of the corner may be taken now, from what the WM thread knows; the
/// capture gate, Screen Recording and the screen's lock are the platform's
/// ([`crate::platform::desktop_shot`]).
pub fn may_take(f: &TakeFacts) -> Result<(), Block> {
    if f.fullscreen {
        return Err(Block::No("a full-screen Space shows"));
    }
    if f.overview {
        return Err(Block::Wait("an overview is up"));
    }
    if f.space_pending {
        return Err(Block::Wait("a Space change is being recognized"));
    }
    if f.slide {
        return Err(Block::Wait("the Desktop slides"));
    }
    if f.since_space_change.is_some_and(|d| d < SETTLE) {
        return Err(Block::Wait("a Desktop just arrived"));
    }
    if f.since_dropped.is_some_and(|d| d < SETTLE) {
        return Err(Block::Wait("the desktop just changed"));
    }
    if f.switching {
        return Err(Block::Wait("a switch runs"));
    }
    Ok(())
}

/// The still for the patch: `ready`, the serial of one that stands for the corner now; else
/// coming while one may be taken (`take`: now, or after a short wait) and none has stood for
/// less than [`HOLD_MAX`] (`missing_for`).
pub fn state(ready: Option<u64>, take: Result<(), Block>, missing_for: Duration) -> Still {
    if let Some(serial) = ready {
        return Still::Ready(serial);
    }
    match take {
        Ok(()) | Err(Block::Wait(_)) if missing_for < HOLD_MAX => Still::Coming,
        _ => Still::Absent,
    }
}

/// What the patch's panel draws ([`source`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Source {
    /// The still to draw (its serial).
    pub still: Option<u64>,
    /// A still is on its way: a panel a still drew at the same rectangle keeps its picture
    /// meanwhile.
    pub hold: bool,
    /// The file's picture may stand for the corner (no desktop icon under it, the icons
    /// known): the fallback when no still is drawn.
    pub file: bool,
}

/// What the patch draws for `still`, with `icons_clear` (Finder's icons known and none under
/// the corner: the file's picture would hide none).
pub fn source(still: Still, icons_clear: bool) -> Source {
    match still {
        Still::Ready(serial) => Source { still: Some(serial), hold: false, file: icons_clear },
        Still::Coming => Source { still: None, hold: true, file: icons_clear },
        Still::Absent => Source { still: None, hold: false, file: icons_clear },
    }
}

/// Whether a still stands for the corner now or is on its way: the gate of the file's
/// picture (the icons under the corner, [`super::corner_patch::visibility`]) does not keep
/// the patch off then.
pub fn stands(still: Still) -> bool {
    !matches!(still, Still::Absent)
}

/// Whether a panel drawn from a still on another Desktop may stay drawn while away (it
/// slides back in as it was left): the icons under it unchanged (`drawn`, `now`: those
/// meeting its rectangle; unknown now counts as unchanged, the arrival takes a new still at
/// once), the appearance unchanged, and the frame the clock predicted not yet due.
pub fn away_still_holds(drawn: Option<&[super::geometry::Rect]>, now: Option<&[super::geometry::Rect]>, dark_changed: bool, frame_due: bool) -> bool {
    if dark_changed || frame_due {
        return false;
    }
    match (drawn, now) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Rect;

    #[test]
    fn a_still_is_taken_only_when_nothing_moves_on_the_desktop() {
        let calm = TakeFacts { since_space_change: Some(Duration::from_secs(5)), since_dropped: Some(Duration::from_secs(5)), ..TakeFacts::default() };
        assert_eq!(may_take(&calm), Ok(()));
        assert_eq!(may_take(&TakeFacts::default()), Ok(()), "no change yet in this run");
        assert_eq!(may_take(&TakeFacts { switching: true, ..calm }), Err(Block::Wait("a switch runs")));
        assert_eq!(may_take(&TakeFacts { overview: true, ..calm }), Err(Block::Wait("an overview is up")));
        assert_eq!(may_take(&TakeFacts { space_pending: true, ..calm }), Err(Block::Wait("a Space change is being recognized")));
        assert_eq!(may_take(&TakeFacts { slide: true, ..calm }), Err(Block::Wait("the Desktop slides")));
        assert_eq!(may_take(&TakeFacts { fullscreen: true, ..calm }), Err(Block::No("a full-screen Space shows")));
        // A Space arrival: after the slide settles.
        let arrived = TakeFacts { since_space_change: Some(Duration::from_millis(100)), ..calm };
        assert_eq!(may_take(&arrived), Err(Block::Wait("a Desktop just arrived")));
        assert_eq!(may_take(&TakeFacts { since_space_change: Some(SETTLE), ..calm }), Ok(()));
        let dropped = TakeFacts { since_dropped: Some(Duration::from_millis(100)), ..calm };
        assert_eq!(may_take(&dropped), Err(Block::Wait("the desktop just changed")));
    }

    #[test]
    fn a_still_on_its_way_holds_the_patch_only_briefly() {
        let short = Duration::from_millis(300);
        assert_eq!(state(Some(7), Err(Block::No("off")), HOLD_MAX * 2), Still::Ready(7), "one stands: it is used");
        assert_eq!(state(None, Ok(()), short), Still::Coming);
        assert_eq!(state(None, Err(Block::Wait("a Desktop just arrived")), short), Still::Coming);
        assert_eq!(state(None, Err(Block::No("no Screen Recording access")), short), Still::Absent);
        assert_eq!(state(None, Ok(()), HOLD_MAX), Still::Absent, "a still that does not come");
    }

    #[test]
    fn the_patch_draws_the_still_first_then_holds_then_the_file() {
        assert_eq!(source(Still::Ready(3), false), Source { still: Some(3), hold: false, file: false }, "an icon under the corner: the still");
        assert_eq!(source(Still::Ready(3), true), Source { still: Some(3), hold: false, file: true });
        assert_eq!(source(Still::Coming, false), Source { still: None, hold: true, file: false });
        assert_eq!(source(Still::Absent, true), Source { still: None, hold: false, file: true }, "without Screen Recording: as before");
        assert!(stands(Still::Ready(1)) && stands(Still::Coming) && !stands(Still::Absent));
    }

    #[test]
    fn another_desktops_still_stays_drawn_while_its_desktop_is_unchanged() {
        let icon = [Rect::new(2440.0, 1300.0, 136.0, 114.0)];
        assert!(away_still_holds(Some(&icon), Some(&icon), false, false));
        assert!(away_still_holds(Some(&[]), Some(&[]), false, false));
        assert!(!away_still_holds(Some(&[]), Some(&icon), false, false), "an icon came under it");
        assert!(!away_still_holds(Some(&icon), Some(&[]), false, false), "the icon left");
        assert!(away_still_holds(Some(&icon), None, false, false), "not known now: kept, the arrival takes a new one");
        assert!(!away_still_holds(Some(&icon), Some(&icon), true, false), "the appearance changed");
        assert!(!away_still_holds(Some(&icon), Some(&icon), false, true), "the predicted frame is due");
    }
}
