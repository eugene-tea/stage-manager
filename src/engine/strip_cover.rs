//! Which strip cards anyone can see, pure: the gate for their thumbnails' captures and for a
//! card's hover.
//!
//! The strip sits below windows (Apple's), so a window of the stage in front over its column
//! hides the cards under it. A window reaching the column's left edge hides the whole strip
//! (`ui::strip_visibility::hides_strip`): it slides off the screen and asks no picture at all.
//! A window over the cards but further right leaves the strip shown, under the window:
//! - The pointer on that window above a card is on the window, not on the card ([`on_card`]):
//!   until 1.9.1 the card grew unseen under the window and took its thumbnails and, ahead of
//!   a switch, its windows' full-size pictures, at every pass of the pointer.
//! - A card wholly under windows asks for no thumbnails ([`wants_pictures`]): not for a stage
//!   leaving for its card, nor every `[strip] thumbnail_interval_ms` when one is set. With
//!   the cards at their default place (`[cards] x` 15, left of the 20 pt that hides the
//!   strip) some of each card shows past any window that leaves the strip up, so this counts
//!   with the cards further right, or a strip on a full-screen Space (the pane left on the
//!   Desktop behind it, out of sight).
//!
//! A card leaving the cover asks again: the strip's capturer takes a window newly asked for at
//! once, so a card coming into view (the window moved off it, the edge revealing the strip
//! above the window, a switch, another Desktop) gets one round of fresh pictures as it shows,
//! and keeps its last one meanwhile.
//!
//! Only the windows the strip knows cover it (the stage in front's and the pinned ones on
//! screen, `active_frames`); another app's window over a card counts for nothing, so the card
//! asks as before. A card is seen while any part of it shows: no card is left stale that
//! someone could see.

use crate::engine::geometry::{Point, Rect};

/// Slivers thinner than this (points) count as covered: a window's edge a fraction of a
/// point off the card's.
const SLIVER: f64 = 0.5;

/// How the strip is placed now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    /// The strip is on the screen (not slid off: hidden by the user, for the Desktop, by a
    /// window at its edge, by "Show recent apps" off, or on a full-screen Space).
    pub shown: bool,
    /// The strip is above windows (lifted: the edge revealing it over a window, a card
    /// dragged, a switch under way, or `[strip] level` above): nothing covers it.
    pub above_windows: bool,
}

/// True when some part of `card` (global, top-left) shows past `windows` (the frames of the
/// windows over the strip): it is not wholly under their union.
pub fn shows(card: &Rect, windows: &[Rect]) -> bool {
    if card.w < SLIVER || card.h < SLIVER {
        return false;
    }
    let over: Vec<Rect> = windows.iter().filter_map(|w| w.intersect(card)).collect();
    if over.is_empty() {
        return true;
    }
    if over.iter().any(|o| o.contains_rect(card)) {
        return false;
    }
    // Cut the card along every window edge inside it: each cell is wholly in or out of each
    // window, so the card shows if one cell of some size lies under no window.
    let mut xs: Vec<f64> = over.iter().flat_map(|o| [o.x, o.max_x()]).chain([card.x, card.max_x()]).collect();
    let mut ys: Vec<f64> = over.iter().flat_map(|o| [o.y, o.max_y()]).chain([card.y, card.max_y()]).collect();
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);
    xs.dedup();
    ys.dedup();
    xs.windows(2).filter(|x| x[1] - x[0] >= SLIVER).any(|x| {
        ys.windows(2).filter(|y| y[1] - y[0] >= SLIVER).any(|y| {
            let mid = Point::new((x[0] + x[1]) / 2.0, (y[0] + y[1]) / 2.0);
            !over.iter().any(|o| o.contains(mid))
        })
    })
}

/// True when the thumbnails of the card at `card` (global, top-left) are worth taking now: the
/// strip is shown, and above windows or with some of the card past `windows`.
pub fn wants_pictures(view: View, card: &Rect, windows: &[Rect]) -> bool {
    view.shown && (view.above_windows || shows(card, windows))
}

/// True when the pointer at `p` (global, top-left) is on the strip, not on one of `windows`
/// over it: always while the strip is above windows.
pub fn on_card(view: View, p: Point, windows: &[Rect]) -> bool {
    view.shown && (view.above_windows || !windows.iter().any(|w| w.contains(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BELOW: View = View { shown: true, above_windows: false };
    const ABOVE: View = View { shown: true, above_windows: true };
    const HIDDEN: View = View { shown: false, above_windows: false };

    fn card() -> Rect {
        Rect::new(16.0, 200.0, 180.0, 120.0)
    }

    #[test]
    fn a_card_with_no_window_over_it_shows() {
        assert!(shows(&card(), &[]));
        assert!(shows(&card(), &[Rect::new(300.0, 0.0, 800.0, 900.0)]), "a window right of the column");
        assert!(shows(&card(), &[Rect::new(196.0, 0.0, 800.0, 900.0)]), "a window touching the card's right edge");
    }

    #[test]
    fn a_card_wholly_under_one_window_or_several_does_not_show() {
        assert!(!shows(&card(), &[Rect::new(10.0, 100.0, 900.0, 800.0)]));
        // Two windows, each over half of it, together over all of it.
        let left = Rect::new(0.0, 0.0, 110.0, 900.0);
        let right = Rect::new(100.0, 150.0, 900.0, 300.0);
        assert!(!shows(&card(), &[left, right]));
        // A window's edge a fraction of a point short of the card's: still covered.
        assert!(!shows(&card(), &[Rect::new(16.3, 200.0, 180.0, 120.0)]));
    }

    #[test]
    fn a_card_partly_out_from_under_its_windows_shows() {
        assert!(shows(&card(), &[Rect::new(60.0, 0.0, 900.0, 900.0)]), "its left part shows");
        assert!(shows(&card(), &[Rect::new(0.0, 0.0, 900.0, 260.0)]), "its lower half shows");
        // Two windows leaving a gap between them.
        let top = Rect::new(0.0, 0.0, 900.0, 250.0);
        let bottom = Rect::new(0.0, 270.0, 900.0, 600.0);
        assert!(shows(&card(), &[top, bottom]), "a 20 pt band shows");
        // The gap a corner only: three windows over all but the card's bottom-right corner.
        let a = Rect::new(0.0, 0.0, 900.0, 300.0);
        let b = Rect::new(0.0, 0.0, 150.0, 900.0);
        assert!(shows(&card(), &[a, b]), "its bottom-right corner shows");
        assert!(!shows(&card(), &[a, b, Rect::new(140.0, 290.0, 100.0, 100.0)]), "the corner covered too");
    }

    #[test]
    fn pictures_are_wanted_only_for_cards_someone_can_see() {
        let over = [Rect::new(10.0, 100.0, 900.0, 800.0)];
        assert!(!wants_pictures(BELOW, &card(), &over), "under a window, the strip below windows");
        assert!(wants_pictures(ABOVE, &card(), &over), "the edge reveals the strip above the window");
        assert!(wants_pictures(BELOW, &card(), &[]), "nothing over it");
        assert!(!wants_pictures(HIDDEN, &card(), &[]), "the strip off the screen");
        assert!(!wants_pictures(View { shown: false, above_windows: true }, &card(), &[]));
    }

    #[test]
    fn the_pointer_over_a_window_above_a_card_is_on_the_window() {
        let over = [Rect::new(60.0, 0.0, 900.0, 900.0)];
        assert!(!on_card(BELOW, Point::new(100.0, 250.0), &over), "on the window over the card");
        assert!(on_card(BELOW, Point::new(30.0, 250.0), &over), "on the card's part left of the window");
        assert!(on_card(ABOVE, Point::new(100.0, 250.0), &over), "the strip above the window");
        assert!(!on_card(HIDDEN, Point::new(30.0, 250.0), &[]));
    }
}
