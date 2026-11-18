//! A window of another app dragged toward the strip, pure: when the strip takes it (its
//! card-sized picture follows the pointer and a release drops it into the strip), and when
//! the dragged window itself hides the strip.
//!
//! **Apple's, recorded 11-09 on a Studio Display** (`07-drag-window-into-strip`,
//! `08-window-over-strip-hides`, `15-drag-window-onto-card`, `10-window-near-edge-hides`):
//!
//! - The window turns into a card by where the **pointer** is, not by the window's frame: a
//!   window dragged low toward the strip, held 240 pt right of its left edge, started to
//!   shrink with the pointer about 290 pt from the screen's edge (its left edge then about
//!   55 pt from it), well past the strip's cards; a window dragged over the cards to 48 pt
//!   with the pointer about 450 pt out stayed a window.
//! - The dragged window hides the strip as any window of the stage does, once its left edge
//!   comes within [`strip_visibility::COVER_EDGE`]: a window held about 390 pt right of its
//!   left edge, dragged onto a card, hid the strip before the pointer got that close, and no
//!   card ever showed, with the pointer at last about 95 pt from the edge.
//! - Once the window is a card it covers nothing: the strip stays up under it.
//!
//! So the card shows while the pointer is within [`DROP_REACH`] of the screen's edge and the
//! strip is up; a window that hides the strip before the pointer gets there keeps it hidden
//! and is no drop. Ours keeps the window at its size under a card-sized picture (Stage
//! Manager cannot resize a window the user holds), so the window's frame while it is dragged
//! is worked out from the pointer ([`live_frame`]).
//!
//! The reach has a margin on the way out ([`DROP_LEAVE`]): a pointer wavering at its edge
//! does not show and hide the card at every move (the owner, 11-18: the strip's
//! `drop_over` flipped every one to four seconds during drags).

use crate::engine::geometry::{Point, Rect};
use crate::ui::strip_visibility;

/// How far from the strip's left edge (the screen's) the pointer takes a dragged window into
/// the strip, in points at the strip's length scale 1: Apple's window started to turn into a
/// card with the pointer about 290 pt from the edge (recorded 11-09, a 2560 pt wide screen;
/// the frame before, about 304 pt, showed it whole). Never less than the strip's column.
pub const DROP_REACH: f64 = 290.0;

/// How far past the reach the pointer goes before a window taken in leaves the strip again,
/// in points at the strip's length scale 1.
pub const DROP_LEAVE: f64 = 24.0;

/// The dragged window's frame now: `pre` (its frame when the button went down at `grab`)
/// moved with the pointer, now at `pointer` (all global, top-left origin).
pub fn live_frame(pre: Rect, grab: Point, pointer: Point) -> Rect {
    Rect::new(pre.x + pointer.x - grab.x, pre.y + pointer.y - grab.y, pre.w, pre.h)
}

/// Where the pointer holds the dragged window, as shares of its width and height (`pre`, its
/// frame when the button went down at `grab`): its card is held at the same shares
/// ([`crate::ui::motion_math::Plane::under_pointer`]), as Apple's card keeps the point the
/// pointer holds (recorded 11-09: a window held at 0.24 of its width by its title bar became
/// a card with the pointer 0.26 across it, a few points below its top). A window with no
/// size is held at its title bar's middle.
pub fn grab_share(pre: Rect, grab: Point) -> (f64, f64) {
    if pre.w > 1.0 && pre.h > 1.0 { ((grab.x - pre.x) / pre.w, (grab.y - pre.y) / pre.h) } else { (0.5, 0.05) }
}

/// The area where the pointer takes a dragged window into the strip whose column is `column`
/// (global): the column's height, from its left edge to [`DROP_REACH`] (times the strip's
/// length scale `k`), or the column's width where that is wider, plus [`DROP_LEAVE`] when
/// the window is already taken in (`over`).
pub fn reach(column: &Rect, k: f64, over: bool) -> Rect {
    let k = if k.is_finite() && k > 0.0 { k } else { 1.0 };
    let w = column.w.max(DROP_REACH * k) + if over { DROP_LEAVE * k } else { 0.0 };
    Rect::new(column.x, column.y, w, column.h)
}

/// Whether the window dragged with the pointer at `pointer` is taken into the strip (its
/// card shows, a release there drops it), given whether it was at the last step (`over`),
/// the strip's column `column` (global), its length scale `k` and the window's frame now
/// (`dragged`, [`live_frame`]; `None` when not known). Before it is taken in, a window that
/// hides the strip ([`strip_visibility::hides_strip`]) is not; once in, its frame no longer
/// counts (Apple's window is a card by then).
pub fn taken_in(over: bool, pointer: Point, column: &Rect, k: f64, dragged: Option<Rect>) -> bool {
    if !reach(column, k, over).contains(pointer) {
        return false;
    }
    over || !dragged.is_some_and(|f| hides_strip_alone(f, column))
}

/// Whether the dragged window (`dragged`, its frame now) hides the strip whose column is
/// `column`: only while it is not taken in (`over`); once taken in, it shows as a card.
pub fn hides(over: bool, column: &Rect, dragged: Option<Rect>) -> bool {
    !over && dragged.is_some_and(|f| hides_strip_alone(f, column))
}

fn hides_strip_alone(frame: Rect, column: &Rect) -> bool {
    strip_visibility::hides_strip(std::slice::from_ref(&frame), column)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2560 x 1410 pt screen's strip column (the menu bar above it), 200 pt wide.
    const COLUMN: Rect = Rect { x: 0.0, y: 30.0, w: 200.0, h: 1410.0 };

    /// A 1000 x 700 window held at its title bar `grab_dx` right of its left edge, with the
    /// pointer at `x`, 1100 pt down: (the pointer, the window's frame now).
    fn held(grab_dx: f64, x: f64) -> (Point, Rect) {
        let pre = Rect::new(1200.0, 1080.0, 1000.0, 700.0);
        let grab = Point::new(pre.x + grab_dx, pre.y + 10.0);
        let p = Point::new(x, 1090.0);
        (p, live_frame(pre, grab, p))
    }

    #[test]
    fn the_window_moves_with_the_pointer() {
        let pre = Rect::new(400.0, 300.0, 800.0, 600.0);
        let f = live_frame(pre, Point::new(500.0, 310.0), Point::new(260.0, 410.0));
        assert_eq!(f, Rect::new(160.0, 400.0, 800.0, 600.0));
    }

    /// Apple's 07: held 240 pt in, the window became a card with the pointer about 290 pt
    /// out, its left edge about 50 pt from the screen's edge, past the column.
    #[test]
    fn the_card_shows_with_the_pointer_where_apples_did() {
        let (p, f) = held(240.0, 289.0);
        assert!(f.x > strip_visibility::COVER_EDGE);
        assert!(taken_in(false, p, &COLUMN, 1.0, Some(f)), "about 290 pt out, as Apple's");
        let (p, f) = held(240.0, 150.0);
        assert!(taken_in(true, p, &COLUMN, 1.0, Some(f)), "taken in, deeper in it stays, its window under its card");
    }

    /// Apple's 08: the pointer about 450 pt out keeps the window whole, though the window
    /// covers the cards; and well out, nothing at all.
    #[test]
    fn no_card_with_the_pointer_far_out() {
        let (p, f) = held(400.0, 450.0);
        assert!(!taken_in(false, p, &COLUMN, 1.0, Some(f)));
        let (p, f) = held(240.0, 1000.0);
        assert!(!taken_in(false, p, &COLUMN, 1.0, Some(f)));
        assert!(!taken_in(true, p, &COLUMN, 1.0, Some(f)), "taken in before, it leaves");
    }

    /// Apple's 15 and the owner's 11-18 drag: a window whose left edge is at the screen's
    /// edge hides the strip, and the pointer inside the strip's reach (or its column) shows
    /// no card.
    #[test]
    fn a_window_at_the_edge_shows_no_card_wherever_the_pointer_is() {
        for x in [95.0, 180.0, 250.0, 289.0] {
            let (p, f) = held(390.0, x);
            assert!(f.x <= strip_visibility::COVER_EDGE);
            assert!(!taken_in(false, p, &COLUMN, 1.0, Some(f)), "pointer at {x}");
            assert!(hides(false, &COLUMN, Some(f)), "the strip hides under it at {x}");
        }
        // A window near the edge held near its left: the pointer in the column, still no card.
        let pre = Rect::new(5.0, 160.0, 512.0, 347.0);
        let grab = Point::new(173.0, 170.0);
        let p = Point::new(176.0, 200.0);
        assert!(!taken_in(false, p, &COLUMN, 1.0, Some(live_frame(pre, grab, p))));
    }

    /// Once taken in, the window shows as a card and hides nothing, even with its frame at
    /// the edge (Apple's strip stayed up under the card).
    #[test]
    fn a_window_taken_in_hides_nothing() {
        let (p, f) = held(240.0, 100.0);
        assert!(f.x < 0.0);
        assert!(!hides(true, &COLUMN, Some(f)));
        assert!(taken_in(true, p, &COLUMN, 1.0, Some(f)));
    }

    /// The pointer wavering at the reach's edge: in at 290, it stays in until past 314, and
    /// comes back only within 290 again.
    #[test]
    fn the_edge_of_the_reach_does_not_flap() {
        let mut over = false;
        let mut flips = 0;
        for x in [300.0, 289.0, 295.0, 289.0, 305.0, 292.0, 310.0, 288.0] {
            let (p, f) = held(240.0, x);
            let now = taken_in(over, p, &COLUMN, 1.0, Some(f));
            flips += usize::from(now != over);
            over = now;
        }
        assert_eq!(flips, 1, "in once, never out");
        let (p, f) = held(240.0, 315.0);
        assert!(!taken_in(true, p, &COLUMN, 1.0, Some(f)), "out past the margin");
        let (p, f) = held(240.0, 300.0);
        assert!(!taken_in(false, p, &COLUMN, 1.0, Some(f)), "back in only within the reach");
    }

    /// Placed as Apple's (07): the card keeps the point the pointer holds, the pointer a
    /// quarter of the way across it and on its title bar, not off to one side.
    #[test]
    fn the_card_is_held_where_the_window_was() {
        use crate::engine::geometry::Size;
        use crate::ui::motion_math::Plane;
        let pre = Rect::new(1408.0, 1110.0, 998.0, 700.0);
        let grab = Point::new(pre.x + 240.0, pre.y + 8.0);
        let share = grab_share(pre, grab);
        assert!((share.0 - 0.24).abs() < 0.01 && share.1 < 0.02, "{share:?}");
        let p = Point::new(150.0, 1170.0);
        let size = Size::new(138.0, 97.0);
        let card = Plane::under_pointer(p, size, share, 0.0, 4.0);
        let left = card.left_mid.x;
        let top = card.left_mid.y - size.h / 2.0;
        assert!((p.x - left - 0.24 * size.w).abs() < 0.5, "the pointer {} pt right of the card's left edge", p.x - left);
        assert!(p.y - top >= 0.0 && p.y - top < 3.0, "on its title bar: {} pt below its top", p.y - top);
        assert_eq!(grab_share(Rect::new(0.0, 0.0, 0.0, 0.0), grab), (0.5, 0.05));
    }

    /// The reach scales with the strip and is never narrower than its column.
    #[test]
    fn the_reach_follows_the_strips_scale_and_column() {
        assert_eq!(reach(&COLUMN, 1.0, false).w, 290.0);
        assert_eq!(reach(&COLUMN, 0.5, false).w, 200.0, "the column, wider than 145");
        assert_eq!(reach(&COLUMN, 2.0, true).w, 628.0);
        assert_eq!(reach(&COLUMN, f64::NAN, false).w, 290.0);
        let r = reach(&COLUMN, 1.0, false);
        assert!(!r.contains(Point::new(100.0, 20.0)), "not over the menu bar");
        assert!(!r.contains(Point::new(-10.0, 700.0)), "not on a screen to the left");
    }

    /// Unknown frame: the pointer decides alone.
    #[test]
    fn with_no_frame_the_pointer_decides() {
        assert!(taken_in(false, Point::new(120.0, 700.0), &COLUMN, 1.0, None));
        assert!(!hides(false, &COLUMN, None));
    }
}
