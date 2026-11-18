//! Where the posted routes press: a visible point of a strip thumbnail ([`visible_point`]),
//! a window's title bar ([`title_point`]) and the strip's column to drop it on
//! ([`strip_drop`]). Pure geometry, top-left global coordinates.
//!
//! A thumbnail partly under its app icon (66x82 at x = -7) or a window in front still shows a
//! part of its own; a click there takes that window alone (a Shift-click adds only the window
//! clicked). A thumbnail behind another of its item takes no click at all, whatever part of
//! it shows (the eighth probe, 11-17): the mirror asks no point of it
//! (`mirror::item_fronts`); it is clicked once the window in front of it has gone in and it
//! is its item's front (the job's docs).
//!
//! **Only the drawn card takes a click.** The window list lists a thumbnail at the bounding box
//! of its turned plane (the strip's perspective, [`crate::engine::perspective`]: a stage's
//! listed boxes turned back give planes of its windows' own aspect within 1.5 %, live 11-17;
//! our strip's model matches Apple's boxes within 1.5 pt), and WindowManager's click target
//! over it has those bounds; a click in the box outside the drawn plane falls through to the
//! wallpaper, whose click reveals the desktop (probe 7, 11-17), or is lost (a press at 0.9 pt
//! inside the model's top edge, at its rounded corner, took nothing, live 11-17). So a press
//! goes [`INSIDE`] deep in the thumbnail's plane, [`CLEAR`] off every plane in front of it as
//! drawn under the pointer ([`HOVER`]), and off the part of each app icon that is drawn
//! ([`icon_drawn`]); a box no plane fits has no point (it waits, then gives up).

use crate::engine::geometry::{Point, Rect};
use crate::engine::model::Display;
use crate::engine::perspective::{self, View};

/// A point this close to an edge of the screen is not taken (the window server rounds bounds
/// to whole points; the pointer lands on a pixel).
const MARGIN: f64 = 2.0;

/// How deep in its thumbnail's drawn plane a press goes: the plane's rounded corners (radius 8,
/// [cards] corner_radius) cut 2.3 pt in along a corner's diagonal, the list rounds to whole
/// points, a press shrinks the card to 0.987 about its middle, and a press 0.9 pt inside the
/// model's edge took nothing (live 11-17).
pub const INSIDE: f64 = 6.0;

/// How far off what is drawn over it a press stays.
pub const CLEAR: f64 = 3.0;

/// How much a thumbnail in front grows under the pointer, about its own middle (Apple's,
/// measured 11-09: [cards] hover_scale's default). A posted press lands before any growth
/// shows, but the planes in front are counted grown all the same: a press 5 pt left of the
/// front window's plane at rest (0.9 pt off it grown) took nothing (live 11-17).
pub const HOVER: f64 = 1.11;

/// The part of an app icon window (66x82) that is drawn: the icon image (38 pt, [icons] size,
/// about 31 pt of it opaque) at the window's middle across (its middle 33 pt right of the
/// window's left, as the card's left edge plus [icons] center_dx is, measured), its middle
/// between 33 and 41 pt below the window's top (the live run of 11-17: the presses picked
/// reproduce only with the icon window's top 33 to 41 pt above the middle our model draws it
/// at), and 3 pt round it. The rest of the window is clear: a click there goes to the card
/// under it, as one outside the card's drawn plane goes through its click target.
pub fn icon_drawn(icon: Rect) -> Rect {
    const IMAGE: f64 = 38.0;
    const ROUND: f64 = 3.0;
    let w = IMAGE + 2.0 * ROUND;
    let top = icon.y + 33.0 - IMAGE / 2.0 - ROUND;
    let bottom = icon.y + 41.0 + IMAGE / 2.0 + ROUND;
    Rect::new(icon.x + (icon.w - w) / 2.0, top, w, bottom - top)
}

/// What is listed in front of a strip thumbnail, by how it is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Over {
    /// Another strip thumbnail, or its click target: its turned plane.
    Thumb(Rect),
    /// An app icon window of WindowManager's: [`icon_drawn`].
    Icon(Rect),
    /// Any other window: all of its bounds.
    Window(Rect),
}

impl Over {
    /// Its listed bounds.
    pub fn bounds(&self) -> Rect {
        match *self {
            Over::Thumb(r) | Over::Icon(r) | Over::Window(r) => r,
        }
    }

    /// What of it covers, as a quadrilateral: a thumbnail's plane grown by [`HOVER`] (its box
    /// when no plane fits it: all of it), an icon's drawn part, a window's bounds.
    fn drawn(&self, view: &View) -> [Point; 4] {
        match *self {
            Over::Thumb(r) => view.from_bounds(r).map_or_else(|| perspective::rect_quad(r), |q| perspective::grown(&q, HOVER)),
            Over::Icon(r) => perspective::rect_quad(icon_drawn(r)),
            Over::Window(r) => perspective::rect_quad(r),
        }
    }
}

/// The grid a thumbnail's plane is searched on, in points.
const STEP: f64 = 1.0;

/// A point of strip thumbnail `thumb` (its listed bounds) on display `screen` to press: in its
/// drawn plane by [`INSIDE`], off what `over` (everything listed in front of it) draws there by
/// [`CLEAR`], and on the screen; of the candidates, the one with the most room to spare (of
/// equals, the nearest the plane's middle). `None` when no such point is there, or no plane
/// fits the box.
pub fn visible_point(thumb: Rect, over: &[Over], screen: &Display) -> Option<Point> {
    let view = View::apples(screen.visible, screen.frame.center().y);
    let own = view.from_bounds(thumb)?;
    let area = perspective::quad_bounds(&own).intersect(&screen.frame)?;
    let screen_q = perspective::rect_quad(screen.frame);
    let near = Rect::new(area.x - CLEAR, area.y - CLEAR, area.w + 2.0 * CLEAR, area.h + 2.0 * CLEAR);
    let covers: Vec<[Point; 4]> = over
        .iter()
        .map(|o| o.drawn(&view))
        .filter(|q| perspective::quad_bounds(q).intersect(&near).is_some())
        .collect();
    let mid = perspective::quad_bounds(&own).center();
    let mut best: Option<(f64, Point)> = None;
    let (nx, ny) = ((area.w / STEP).floor() as i64, (area.h / STEP).floor() as i64);
    for i in 0..=nx {
        for j in 0..=ny {
            let p = Point::new(area.x + i as f64 * STEP, area.y + j as f64 * STEP);
            let mut room = (perspective::quad_depth(&own, p) - INSIDE).min(perspective::quad_depth(&screen_q, p) - MARGIN);
            if room < 0.0 {
                continue;
            }
            for q in &covers {
                room = room.min(-perspective::quad_depth(q, p) - CLEAR);
            }
            if room < 0.0 {
                continue;
            }
            let nearer = |q: Point| (p.x - mid.x).hypot(p.y - mid.y) < (q.x - mid.x).hypot(q.y - mid.y);
            if best.is_none_or(|(r, q)| room > r + 0.01 || ((room - r).abs() <= 0.01 && nearer(q))) {
                best = Some((room, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// How far below a window's top edge its title bar is grabbed (the drag that worked in the
/// first probe of 11-16: 14 points, within a standard title bar's 28).
pub const TITLE_GRAB: f64 = 14.0;

/// How far in from a window's left edge no grab is taken (the traffic lights).
const BUTTONS: f64 = 80.0;

/// Where window `frame`'s title bar is grabbed for the drag: [`TITLE_GRAB`] below its top,
/// across at its middle, as the drag that worked in the first probe of 11-16. Probe 6 (a safe
/// point for unified toolbars and Electron) is still open: the live check's fixtures have
/// plain title bars.
pub fn title_point(frame: Rect) -> Point {
    let x = frame.center().x.max(frame.x + BUTTONS.min(frame.w / 2.0));
    Point::new(x, frame.y + TITLE_GRAB)
}

/// Where window `frame`'s title bar shows a part of its own to grab, `over` being every
/// window listed in front of it (the drag's guard takes the topmost window at the grab point,
/// live 11-16: three fixture windows at the same frame, the one to drag under another, the
/// gesture refused twice): [`title_point`] when nothing covers it, else the free point along
/// the title bar nearest it, clear of the traffic lights, within `screen`. `None` when the
/// title bar is covered all along: the window is raised first.
pub fn title_free(frame: Rect, over: &[Rect], screen: Rect) -> Option<Point> {
    let want = title_point(frame);
    let y = want.y;
    if y < screen.y + MARGIN || y > screen.max_y() - MARGIN {
        return None;
    }
    let free = |x: f64| {
        let p = Point::new(x, y);
        screen.contains(p) && !over.iter().any(|o| Rect::new(o.x - MARGIN, o.y - MARGIN, o.w + 2.0 * MARGIN, o.h + 2.0 * MARGIN).contains(p))
    };
    if free(want.x) {
        return Some(want);
    }
    let lo = frame.x + BUTTONS.min(frame.w / 2.0);
    let hi = frame.max_x() - 16.0;
    let mut xs: Vec<f64> = (0..=16).map(|i| lo + (hi - lo) * f64::from(i) / 16.0).collect();
    for o in over {
        xs.extend([o.x - MARGIN - 0.5, o.max_x() + MARGIN + 0.5]);
    }
    xs.into_iter()
        .filter(|x| (lo..=hi).contains(x) && free(*x))
        .min_by(|a, b| (a - want.x).abs().total_cmp(&(b - want.x).abs()))
        .map(|x| Point::new(x, y))
}

/// How far in from the screen's left edge a window is dropped onto a hidden strip (the third
/// probe's release at x = 30, 11-16: the window went into a stage of its own with the strip
/// covered).
pub const DROP_X: f64 = 30.0;

/// Where a dragged window is dropped onto screen `screen`'s strip, `strip` being the strip's
/// items as the mirror read them (empty, or hidden off the left edge): the middle of the
/// items when they show (the drag that worked in the first probe of 11-16 dropped in the
/// strip's column at the screen's middle height, x = 80, y = 600 of 1440, among the items;
/// one dropped at the title bar's height, near the top-left corner above them, did nothing,
/// live 11-16), else [`DROP_X`] in at the visible frame's middle height.
pub fn strip_drop(screen: &Display, strip: &[Rect]) -> Point {
    let v = screen.visible;
    let shown: Vec<Rect> = strip.iter().filter(|r| r.max_x() > screen.frame.x).copied().collect();
    let p = match shown.split_first() {
        Some((first, rest)) => {
            let all = rest.iter().fold(*first, |u, r| u.union(r));
            Point::new(all.center().x.clamp(screen.frame.x + DROP_X, screen.frame.x + 120.0), all.center().y)
        }
        None => Point::new(screen.frame.x + DROP_X, v.center().y),
    };
    Point::new(p.x, p.y.clamp(v.y + 40.0, (v.max_y() - 40.0).max(v.y + 40.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testkit::display;

    const SCREEN: Rect = Rect::new(0.0, 0.0, 1440.0, 900.0);

    /// The 5K display of the live run of 11-17: 2560 × 1440, visible from y = 30.
    fn five_k() -> Display {
        Display { frame: Rect::new(0.0, 0.0, 2560.0, 1440.0), visible: Rect::new(0.0, 30.0, 2560.0, 1410.0), ..display() }
    }

    /// The thumbnail's drawn plane (the model's, turned back from its box).
    fn plane(thumb: Rect, d: &Display) -> [Point; 4] {
        View::apples(d.visible, d.frame.center().y).from_bounds(thumb).expect("a plane fits")
    }

    /// `p` lies `INSIDE` deep in `thumb`'s plane and `CLEAR` off what `over` draws.
    fn safe(p: Point, thumb: Rect, over: &[Over], d: &Display) -> bool {
        let view = View::apples(d.visible, d.frame.center().y);
        perspective::quad_depth(&plane(thumb, d), p) >= INSIDE - 1e-9 && over.iter().all(|o| -perspective::quad_depth(&o.drawn(&view), p) >= CLEAR - 1e-9)
    }

    #[test]
    fn a_thumbnail_alone_is_pressed_deep_in_its_plane() {
        let d = five_k();
        // A stage of one 520 × 424 window, above the display's middle and below it.
        for t in [Rect::new(16.0, 266.0, 76.0, 110.0), Rect::new(16.0, 900.0, 76.0, 110.0)] {
            let p = visible_point(t, &[], &d).expect("a point");
            assert!(safe(p, t, &[], &d), "{p:?}");
            let q = plane(t, &d);
            assert!(perspective::quad_depth(&q, p) > 30.0, "the deepest point: {p:?}");
        }
    }

    #[test]
    fn a_cards_corner_regions_are_never_chosen() {
        let d = five_k();
        let t = Rect::new(35.0, 266.0, 58.0, 84.0);
        let q = plane(t, &d);
        // The box's corners outside the drawn plane (above the middle the far edge is drawn
        // lower: the top-right and bottom-left of the box are empty) are no part of the card.
        assert!(!perspective::in_quad(&q, Point::new(t.max_x() - 3.0, t.y + 3.0)));
        assert!(!perspective::in_quad(&q, Point::new(t.x + 3.0, t.max_y() - 3.0)));
        // Everything covered but one corner of the box after another: no point there.
        let corners = [(t.x, t.y), (t.max_x() - 14.0, t.y), (t.max_x() - 14.0, t.max_y() - 14.0), (t.x, t.max_y() - 14.0)];
        for (cx, cy) in corners {
            let hole = Rect::new(cx, cy, 14.0, 14.0);
            // Four windows round the hole.
            let over = [
                Over::Window(Rect::new(0.0, 0.0, 200.0, hole.y)),
                Over::Window(Rect::new(0.0, hole.max_y(), 200.0, 400.0)),
                Over::Window(Rect::new(0.0, 0.0, hole.x, 400.0)),
                Over::Window(Rect::new(hole.max_x(), 0.0, 200.0, 400.0)),
            ];
            if let Some(p) = visible_point(t, &over, &d) {
                assert!(safe(p, t, &over, &d), "corner {cx},{cy}: {p:?}");
            }
        }
        // The live run's press (84.25, 278): 0.9 pt inside the top edge near its rounded
        // corner, took nothing. Under the stage's other two windows it is not taken again.
        let over = [Over::Thumb(Rect::new(41.0, 289.0, 58.0, 84.0)), Over::Thumb(Rect::new(15.0, 276.0, 59.0, 84.0))];
        assert!(perspective::quad_depth(&q, Point::new(84.25, 278.0)) < INSIDE);
        if let Some(p) = visible_point(t, &over, &d) {
            assert!(safe(p, t, &over, &d), "{p:?}");
            assert!((p.x - 84.25).abs() > 1.0 || (p.y - 278.0).abs() > 4.0, "{p:?}");
        }
    }

    #[test]
    fn the_back_thumbnail_of_a_stage_of_two_is_pressed_clear_of_the_front_one_grown() {
        let d = five_k();
        // The live run of 11-17: Bravo's two windows alone in an item, the back one pressed
        // at (27, 307.75), 5 pt left of the front one's plane at rest, took nothing.
        let back = Rect::new(16.0, 266.0, 76.0, 110.0);
        let front = Over::Thumb(Rect::new(32.0, 266.0, 75.0, 110.0));
        let icon = Over::Icon(Rect::new(-7.0, 313.0, 66.0, 82.0));
        let over = [front, front, icon];
        assert!(!safe(Point::new(27.0, 307.75), back, &over, &d), "the press that missed is no safe point");
        if let Some(p) = visible_point(back, &over, &d) {
            assert!(safe(p, back, &over, &d), "{p:?}");
            assert!(p.x < 27.0, "left of the front window as it grows: {p:?}");
        }
        // Two apps' windows: the back one shows above the other two.
        let t = Rect::new(35.0, 266.0, 58.0, 84.0);
        let q = plane(t, &d);
        let p = visible_point(t, &[], &d).expect("alone: a point");
        assert!(perspective::quad_depth(&q, p) >= INSIDE);
    }

    #[test]
    fn a_thumbnail_half_under_its_icon_is_pressed_beside_the_drawn_icon() {
        let d = five_k();
        // A stage alone low in the strip: its top half and its right part under windows in
        // front, its lower left under its icon window (66 × 82 at x = -7), whose middle only
        // is drawn: the part that shows lies in the icon window, right of the drawn icon.
        let t = Rect::new(16.0, 900.0, 76.0, 110.0);
        let icon = Rect::new(-7.0, 940.0, 66.0, 82.0);
        let over = [Over::Window(Rect::new(-100.0, 0.0, 400.0, 945.0)), Over::Window(Rect::new(62.0, 0.0, 400.0, 1440.0)), Over::Icon(icon)];
        let p = visible_point(t, &over, &d).expect("beside the drawn icon");
        assert!(safe(p, t, &over, &d), "{p:?}");
        assert!(icon.contains(p) && !icon_drawn(icon).contains(p), "in the icon window's clear part: {p:?}");
        // Counted whole, as before, the icon window would leave no point.
        assert_eq!(visible_point(t, &[over[0], over[1], Over::Window(icon)], &d), None);
    }

    #[test]
    fn a_thumbnail_wholly_under_drawn_parts_has_no_point() {
        let d = five_k();
        // A small window's thumbnail (37 × 103) at the strip's foot (the owner's smoke test of
        // 11-17, Calculator's): its top under the item above's thumbnail as it grows, the rest
        // under its icon as drawn (its middle 10.5 pt above the card's bottom, as our model
        // draws it, 37 pt below the icon window's top).
        let t = Rect::new(15.0, 1200.0, 37.0, 103.0);
        let icon = Rect::new(-7.0, 1303.0 - 10.5 - 37.0, 66.0, 82.0);
        let above = Over::Thumb(Rect::new(16.0, 1160.0, 76.0, 110.0));
        assert_eq!(visible_point(t, &[above, Over::Icon(icon)], &d), None);
        // Fully covered, or off screen, or a box no plane fits: none.
        assert_eq!(visible_point(Rect::new(15.0, 300.0, 80.0, 100.0), &[Over::Window(Rect::new(10.0, 295.0, 100.0, 120.0))], &d), None);
        assert_eq!(visible_point(Rect::new(-258.0, 300.0, 80.0, 100.0), &[], &d), None, "the strip hidden");
        assert_eq!(visible_point(Rect::new(15.0, 300.0, 4000.0, 100.0), &[], &d), None, "no plane fits");
    }

    #[test]
    fn a_sliver_too_thin_to_press_safely_is_no_point() {
        let d = five_k();
        let back = Rect::new(15.0, 300.0, 80.0, 100.0);
        let front = Rect::new(22.0, 300.0, 80.0, 100.0);
        assert_eq!(visible_point(back, &[Over::Thumb(front)], &d), None, "7 points showing: no press that sure");
    }

    #[test]
    fn the_title_bar_is_grabbed_at_its_middle_and_dropped_among_the_strips_items() {
        let f = Rect::new(300.0, 120.0, 700.0, 500.0);
        let p = title_point(f);
        assert_eq!(p, Point::new(f.center().x, f.y + 14.0), "the probe's grab");
        let narrow = title_point(Rect::new(300.0, 120.0, 120.0, 300.0));
        assert_eq!(narrow.x, 360.0, "a narrow window: its middle");
        let d = display();
        // Items shown: their middle, in the strip's column, never the grab's height.
        let items = [Rect::new(-7.0, 300.0, 170.0, 180.0), Rect::new(-7.0, 700.0, 170.0, 180.0)];
        let drop = strip_drop(&d, &items);
        assert!((drop.x - 78.0).abs() < 1.0 && (drop.y - 590.0).abs() < 1.0, "{drop:?}");
        // None shown (hidden off the left edge, or none): in from the edge, mid-height.
        let hidden = [Rect::new(-258.0, 300.0, 79.0, 104.0)];
        for strip in [&hidden[..], &[]] {
            let drop = strip_drop(&d, strip);
            assert_eq!(drop, Point::new(d.frame.x + DROP_X, d.visible.center().y));
        }
    }

    #[test]
    fn a_title_bar_under_another_window_is_grabbed_where_it_shows_or_not_at_all() {
        let f = Rect::new(80.0, 87.0, 520.0, 392.0);
        assert_eq!(title_free(f, &[], SCREEN), Some(title_point(f)), "nothing over it: the usual point");
        assert_eq!(title_free(f, &[f], SCREEN), None, "the live run of 11-16: another window at the same frame covers it all");
        // A window over its left half: grabbed right of it, clear of the middle.
        let left = Rect::new(60.0, 60.0, 320.0, 300.0);
        let p = title_free(f, &[left], SCREEN).expect("its right part shows");
        assert!(p.x > left.max_x() + MARGIN && p.x <= f.max_x() - 16.0, "{p:?}");
        assert!(p.x - left.max_x() < 10.0, "the free point nearest the middle: {p:?}");
        assert_eq!(p.y, f.y + TITLE_GRAB);
        // A window lower down leaves the title bar free.
        assert_eq!(title_free(f, &[Rect::new(80.0, 200.0, 520.0, 300.0)], SCREEN), Some(title_point(f)));
    }
}
