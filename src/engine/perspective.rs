//! The strip's perspective, pure: a window's thumbnail is a plane turned by `tilt` about its
//! left edge (the right side away from the viewer), seen through one eye `distance` points in
//! front of the screen at `eye`. Every far edge is shorter by `D / (D + u sin tilt)` and drawn
//! toward the eye. Our strip draws with it (`ui::strip_layout`, measured on Apple's: the
//! measurement record is `docs/UX-SPEC.md`, "Strip geometry"), and the Apple mode reads Apple's
//! thumbnails with it ([`View::quad_of_bounds`]: the window list lists a thumbnail at the bounding
//! box of its turned plane). Top-left coordinates, `y` grows down.

use crate::engine::geometry::{Point, Rect};

/// The two displays the automatic tilt and perspective were measured on: (visible width,
/// tilt in degrees, eye distance in points).
pub const MEASURED_PERSPECTIVE: [(f64, f64, f64); 2] = [(1800.0, 44.2, 1346.0), (2560.0, 47.9, 2260.0)];

/// The eye's horizontal place right of the visible frame's left edge (`[cards]
/// perspective_x`'s default, Apple's measured).
pub const EYE_X: f64 = 15.0;

/// Apple's tilt (degrees) and eye distance (points) for a visible width: linear between the
/// two measured displays, held at the nearer one outside them.
pub fn auto_perspective(visible_width: f64) -> (f64, f64) {
    let [(w0, t0, d0), (w1, t1, d1)] = MEASURED_PERSPECTIVE;
    let t = if visible_width.is_finite() { ((visible_width - w0) / (w1 - w0)).clamp(0.0, 1.0) } else { 1.0 };
    (t0 + (t1 - t0) * t, d0 + (d1 - d0) * t)
}

/// One perspective: the turn, the eye's distance and its place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// The turn, radians.
    pub tilt: f64,
    /// The eye's distance in front of the screen (at least a point).
    pub distance: f64,
    /// The eye's place on the screen.
    pub eye: Point,
}

impl View {
    /// Apple's view on a display whose visible frame is `visible` and whose full frame's
    /// vertical middle is `middle` (global coordinates): [`auto_perspective`], the eye
    /// [`EYE_X`] in from the visible frame's left edge at the display's middle height.
    pub fn apples(visible: Rect, middle: f64) -> Self {
        let (tilt, distance) = auto_perspective(visible.w);
        Self { tilt: tilt.to_radians(), distance, eye: Point::new(visible.x + EYE_X, middle) }
    }

    /// How much the far edge of a plane `u` wide shrinks (`D / (D + u sin tilt)`).
    pub fn far_ratio(&self, u: f64) -> f64 {
        let d = self.distance.max(1.0);
        d / (d + u * self.tilt.sin())
    }

    /// The four corners of a plane whose left edge runs from (`x`, `top`) down `v`, `u` wide
    /// before the turn: top-left, top-right, bottom-right, bottom-left.
    pub fn project(&self, x: f64, top: f64, u: f64, v: f64) -> [Point; 4] {
        let f = self.far_ratio(u);
        let rx = self.eye.x + (x + u * self.tilt.cos() - self.eye.x) * f;
        let ry = |y: f64| self.eye.y + (y - self.eye.y) * f;
        [Point::new(x, top), Point::new(rx, ry(top)), Point::new(rx, ry(top + v)), Point::new(x, top + v)]
    }

    /// The plane before the turn (its left edge's top-left corner, its width and height)
    /// whose turned bounding box is `bounds`, as the window list lists a strip thumbnail: its
    /// left edge at the box's left, its far edge at the box's right; the box's top is the left
    /// edge's top when that lies above the eye (the far edge drawn lower), else the far
    /// edge's; its bottom likewise. `None` when no plane fits (a box too wide for any turned
    /// plane at its place, or empty).
    pub fn plane_of(&self, bounds: Rect) -> Option<Rect> {
        if !bounds.is_finite() || bounds.w <= 0.0 || bounds.h <= 0.0 {
            return None;
        }
        let x = bounds.x;
        // The far edge moves right as the plane widens: the width before the turn by halving.
        let reach = |u: f64| self.eye.x + (x + u * self.tilt.cos() - self.eye.x) * self.far_ratio(u) - x;
        let (mut lo, mut hi) = (0.0_f64, 8.0 * self.distance.max(1.0));
        if reach(hi) < bounds.w || reach(lo) > bounds.w {
            return None;
        }
        for _ in 0..60 {
            let mid = (lo + hi) / 2.0;
            if reach(mid) < bounds.w { lo = mid } else { hi = mid }
        }
        let u = (lo + hi) / 2.0;
        let f = self.far_ratio(u);
        let ey = self.eye.y;
        let unshrunk = |y: f64| ey + (y - ey) / f;
        let top = if bounds.y <= ey { bounds.y } else { unshrunk(bounds.y) };
        let bottom = if bounds.max_y() >= ey { bounds.max_y() } else { unshrunk(bounds.max_y()) };
        (bottom > top).then(|| Rect::new(x, top, u, bottom - top))
    }

    /// The turned plane whose bounding box is `bounds` ([`View::plane_of`]), its corners as
    /// [`View::project`] gives them.
    pub fn quad_of_bounds(&self, bounds: Rect) -> Option<[Point; 4]> {
        self.plane_of(bounds).map(|p| self.project(p.x, p.y, p.w, p.h))
    }
}

/// The bounding box of a quadrilateral.
pub fn quad_bounds(q: &[Point; 4]) -> Rect {
    let (x0, x1) = (q.iter().map(|p| p.x).fold(f64::INFINITY, f64::min), q.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max));
    let (y0, y1) = (q.iter().map(|p| p.y).fold(f64::INFINITY, f64::min), q.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max));
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// True when `p` lies in the convex quadrilateral `q` (its corners in order, either way
/// round), its edges included.
pub fn in_quad(q: &[Point; 4], p: Point) -> bool {
    let sides: Vec<f64> = (0..4)
        .map(|i| {
            let (a, b) = (q[i], q[(i + 1) % 4]);
            (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
        })
        .collect();
    sides.iter().all(|&c| c >= 0.0) || sides.iter().all(|&c| c <= 0.0)
}

/// How deep `p` lies in the convex quadrilateral `q` (its corners in order, either way round):
/// inside, its distance to the nearest edge; outside, minus the farthest it lies beyond any
/// one edge's line, which is never more than its true distance (so a point taken as clear of
/// a quadrilateral by this is at least that clear). A degenerate `q` is nowhere.
pub fn quad_depth(q: &[Point; 4], p: Point) -> f64 {
    // Which way round: the signed area.
    let area: f64 = (0..4).map(|i| q[i].x * q[(i + 1) % 4].y - q[(i + 1) % 4].x * q[i].y).sum();
    if area.abs() < 1e-9 || !area.is_finite() {
        return f64::NEG_INFINITY;
    }
    let turn = area.signum();
    (0..4)
        .map(|i| {
            let (a, b) = (q[i], q[(i + 1) % 4]);
            let len = (b.x - a.x).hypot(b.y - a.y);
            if len < 1e-9 {
                return f64::INFINITY;
            }
            turn * ((b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)) / len
        })
        .fold(f64::INFINITY, f64::min)
}

/// `q` grown by `s` about the middle of its bounding box (a strip thumbnail under the pointer:
/// each window grows about its own middle, measured 11-09).
pub fn grown(q: &[Point; 4], s: f64) -> [Point; 4] {
    let c = quad_bounds(q).center();
    q.map(|v| Point::new(c.x + (v.x - c.x) * s, c.y + (v.y - c.y) * s))
}

/// A rectangle as a quadrilateral, corners in the order [`View::project`] gives them.
pub fn rect_quad(r: Rect) -> [Point; 4] {
    [Point::new(r.x, r.y), Point::new(r.max_x(), r.y), Point::new(r.max_x(), r.max_y()), Point::new(r.x, r.max_y())]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 5K display: visible 0,30 2560 × 1410 in 2560 × 1440.
    fn five_k() -> View {
        View::apples(Rect::new(0.0, 30.0, 2560.0, 1410.0), 720.0)
    }

    #[test]
    fn a_box_gives_back_the_plane_it_bounds() {
        let v = five_k();
        // Above the eye, across it, below it.
        for (top, h) in [(266.0, 97.0), (660.0, 120.0), (900.0, 97.0)] {
            let q = v.project(16.0, top, 118.0, h);
            let b = quad_bounds(&q);
            let back = v.quad_of_bounds(b).expect("a plane fits");
            for (a, z) in q.iter().zip(back.iter()) {
                assert!((a.x - z.x).abs() < 1e-6 && (a.y - z.y).abs() < 1e-6, "{q:?} vs {back:?}");
            }
        }
    }

    #[test]
    fn apples_listed_thumbnails_read_as_their_windows_aspect() {
        // The live run of 11-17 (fixture windows 520 × 424 on the 5K display): a stage alone
        // listed 76 × 110 at 16,266 and 76 × 105 at 16,412; one of two apps' three windows
        // 58 × 84 at 35,266. Turned back, each is a plane of the window's own aspect (1.23)
        // within 1.5 %: the list's box is the drawn plane's bounding box.
        let v = five_k();
        for b in [Rect::new(16.0, 266.0, 76.0, 110.0), Rect::new(35.0, 266.0, 58.0, 84.0), Rect::new(16.0, 412.0, 76.0, 105.0)] {
            let p = v.plane_of(b).expect("a plane fits");
            let aspect = p.w / p.h;
            assert!((aspect / (520.0 / 424.0) - 1.0).abs() < 0.015, "{b:?}: {aspect}");
        }
    }

    #[test]
    fn depth_is_the_distance_inside_and_no_more_than_it_outside() {
        let q = rect_quad(Rect::new(0.0, 0.0, 10.0, 20.0));
        assert!((quad_depth(&q, Point::new(3.0, 10.0)) - 3.0).abs() < 1e-9);
        assert!((quad_depth(&q, Point::new(-2.0, 10.0)) + 2.0).abs() < 1e-9);
        // Off a corner: 3 and 4 beyond two edges, 5 away; read as 4 away.
        assert!((quad_depth(&q, Point::new(13.0, 24.0)) + 4.0).abs() < 1e-9);
        let back: [Point; 4] = [q[3], q[2], q[1], q[0]];
        assert!((quad_depth(&back, Point::new(3.0, 10.0)) - 3.0).abs() < 1e-9, "either way round");
        assert!(in_quad(&q, Point::new(3.0, 10.0)) && !in_quad(&q, Point::new(-1.0, 10.0)));
    }

    #[test]
    fn a_box_no_plane_fits_is_none() {
        let v = five_k();
        assert_eq!(v.quad_of_bounds(Rect::new(16.0, 266.0, 0.0, 110.0)), None);
        assert_eq!(v.quad_of_bounds(Rect::new(16.0, 266.0, 5000.0, 110.0)), None, "wider than any turned plane");
    }
}
