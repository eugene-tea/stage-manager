//! Where the posted routes press: a visible point of a strip thumbnail ([`visible_point`]),
//! a window's title bar ([`title_point`]) and the strip's column to drop it on
//! ([`strip_drop`]). Pure geometry, top-left global coordinates.
//!
//! A thumbnail partly under another in its item (a stage of two: the front one over the back
//! one, a live capture of 11-16: 85x97 at x = 32 over 87x97 at x = 15) or under its app icon
//! (66x82 at x = -7) still shows a part of its own; a click there takes that window alone
//! (a Shift-click adds only the window clicked). One fully under the others has no point of
//! its own: the routes click front to back, each click taking the front one away and
//! uncovering the next.

use crate::engine::geometry::{Point, Rect};
use crate::engine::model::Display;

/// A point this close to an edge of what covers it, or of the thumbnail, is not taken (the
/// window server rounds bounds to whole points; the pointer lands on a pixel).
const MARGIN: f64 = 2.0;

/// A visible point of `thumb` (its listed bounds) that none of `over` (every window listed
/// in front of it, and the app icons) covers and that lies within `screen`: of the candidates,
/// the one farthest from any edge (of equals, the nearest its middle). `None` when no part of
/// it shows.
pub fn visible_point(thumb: Rect, over: &[Rect], screen: Rect) -> Option<Point> {
    let area = thumb.intersect(&screen)?;
    let (lo_x, hi_x, lo_y, hi_y) = (area.x + MARGIN, area.max_x() - MARGIN, area.y + MARGIN, area.max_y() - MARGIN);
    if hi_x < lo_x || hi_y < lo_y {
        return None;
    }
    let over: Vec<Rect> = over.iter().filter(|o| o.intersect(&area).is_some()).copied().collect();
    // Candidate lines: a grid over the thumbnail, its centre, and both sides of every edge of
    // what covers it (the strips that show).
    let mut xs: Vec<f64> = (0..=8).map(|i| lo_x + (hi_x - lo_x) * f64::from(i) / 8.0).collect();
    let mut ys: Vec<f64> = (0..=8).map(|i| lo_y + (hi_y - lo_y) * f64::from(i) / 8.0).collect();
    for o in &over {
        xs.extend([o.x - MARGIN - 0.5, o.max_x() + MARGIN + 0.5]);
        ys.extend([o.y - MARGIN - 0.5, o.max_y() + MARGIN + 0.5]);
    }
    let mid = area.center();
    let mut best: Option<(f64, Point)> = None;
    for &x in xs.iter().filter(|x| (lo_x..=hi_x).contains(*x)) {
        for &y in ys.iter().filter(|y| (lo_y..=hi_y).contains(*y)) {
            let p = Point::new(x, y);
            if over.iter().any(|o| o.contains(p)) {
                continue;
            }
            let clear = clearance(p, area, &over);
            // The clearest; of equals, the nearest the thumbnail's middle.
            let nearer = |q: Point| (p.x - mid.x).hypot(p.y - mid.y) < (q.x - mid.x).hypot(q.y - mid.y);
            if clear >= MARGIN && best.is_none_or(|(c, q)| clear > c + 0.01 || ((clear - c).abs() <= 0.01 && nearer(q))) {
                best = Some((clear, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// How far `p` is from the nearest edge of `area` (inside) and of every rectangle of `over`
/// (outside).
fn clearance(p: Point, area: Rect, over: &[Rect]) -> f64 {
    let inside = (p.x - area.x).min(area.max_x() - p.x).min(p.y - area.y).min(area.max_y() - p.y);
    over.iter().map(|o| outside(p, *o)).fold(inside, f64::min)
}

/// How far `p` is outside rectangle `r` (0 inside).
fn outside(p: Point, r: Rect) -> f64 {
    let dx = (r.x - p.x).max(p.x - r.max_x()).max(0.0);
    let dy = (r.y - p.y).max(p.y - r.max_y()).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

/// How far below a window's top edge its title bar is grabbed: within the title bar of a
/// standard window (28 points) and the top strip of a unified toolbar, above its controls.
pub const TITLE_GRAB: f64 = 8.0;

/// Where window `frame`'s title bar is grabbed for the drag: [`TITLE_GRAB`] below its top,
/// a third of the way across from its left (clear of the traffic lights at the left, and of
/// the middle, where a title bar of an app's own draws a search field). Probe 6 (a safe point
/// for unified toolbars and Electron) is still open: the live check's fixtures have plain
/// title bars.
pub fn title_point(frame: Rect) -> Point {
    let x = frame.x + (frame.w / 3.0).max(90.0_f64.min(frame.w / 2.0));
    Point::new(x, frame.y + TITLE_GRAB)
}

/// How far in from the screen's left edge a window is dropped onto the strip (the probe's
/// release at x = 30, 11-16: the window went into a stage of its own with the strip shown or
/// covered).
pub const DROP_X: f64 = 30.0;

/// Where a window grabbed at `from` is dropped onto screen `screen`'s strip: [`DROP_X`] in
/// from its left edge, at the grab's height kept within the visible frame.
pub fn strip_drop(screen: &Display, from: Point) -> Point {
    let v = screen.visible;
    let y = from.y.clamp(v.y + 40.0, (v.max_y() - 40.0).max(v.y + 40.0));
    Point::new(screen.frame.x + DROP_X, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testkit::display;

    const SCREEN: Rect = Rect::new(0.0, 0.0, 1440.0, 900.0);

    #[test]
    fn a_thumbnail_alone_is_pressed_near_its_middle() {
        let t = Rect::new(15.0, 300.0, 87.0, 116.0);
        let p = visible_point(t, &[], SCREEN).expect("a point");
        assert!(t.contains(p));
        assert!((p.x - t.center().x).abs() < 6.0 && (p.y - t.center().y).abs() < 8.0, "{p:?}");
    }

    #[test]
    fn the_back_thumbnail_of_a_stage_of_two_is_pressed_where_it_shows() {
        // The live capture of 11-16: Alpha's two thumbnails and the item's icon.
        let back = Rect::new(15.0, 575.0, 87.0, 97.0);
        let front = Rect::new(32.0, 575.0, 85.0, 97.0);
        let icon = Rect::new(-7.0, 628.0, 66.0, 82.0);
        let p = visible_point(back, &[front, front, icon], SCREEN).expect("a part of its own shows");
        assert!(back.contains(p) && !front.contains(p) && !icon.contains(p), "{p:?}");
        assert!(p.x < 32.0 - MARGIN && p.y < 628.0 - MARGIN);
    }

    #[test]
    fn a_thumbnail_fully_covered_or_off_screen_has_no_point() {
        let t = Rect::new(15.0, 300.0, 80.0, 100.0);
        assert_eq!(visible_point(t, &[Rect::new(10.0, 295.0, 100.0, 120.0)], SCREEN), None);
        assert_eq!(visible_point(Rect::new(-258.0, 300.0, 80.0, 100.0), &[], SCREEN), None, "the strip hidden");
    }

    #[test]
    fn a_sliver_too_thin_to_press_safely_is_no_point() {
        let back = Rect::new(15.0, 300.0, 80.0, 100.0);
        let front = Rect::new(18.0, 300.0, 80.0, 100.0);
        assert_eq!(visible_point(back, &[front], SCREEN), None, "3 points showing: no press that sure");
    }

    #[test]
    fn the_title_bar_is_grabbed_near_the_top_clear_of_the_buttons_and_dropped_on_the_strip() {
        let f = Rect::new(300.0, 120.0, 700.0, 500.0);
        let p = title_point(f);
        assert!(p.y > f.y && p.y < f.y + 20.0);
        assert!(p.x >= f.x + 90.0 && p.x < f.center().x);
        let narrow = title_point(Rect::new(300.0, 120.0, 120.0, 300.0));
        assert!(narrow.x <= 360.0 && narrow.x > 300.0, "a narrow window: its middle at most");
        let d = display();
        let drop = strip_drop(&d, p);
        assert_eq!(drop.x, d.frame.x + DROP_X);
        assert_eq!(drop.y, p.y);
        assert!(strip_drop(&d, Point::new(400.0, 2.0)).y >= d.visible.y + 40.0, "kept off the menu bar");
    }
}
