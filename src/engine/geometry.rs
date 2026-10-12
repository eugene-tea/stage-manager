//! Plain geometry in global display coordinates.
//!
//! Coordinates follow the Accessibility and CoreGraphics convention: the origin is the
//! top-left corner of the main display and `y` grows downward. All values are points (`f64`).

use serde::{Deserialize, Serialize};

/// A point in global coordinates (top-left origin, `y` grows down).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point {
    /// Horizontal position.
    pub x: f64,
    /// Vertical position (grows downward).
    pub y: f64,
}

impl Point {
    /// Builds a point.
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// A width and height in points.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Size {
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Size {
    /// Builds a size.
    pub const fn new(w: f64, h: f64) -> Self {
        Self { w, h }
    }
}

/// An axis-aligned rectangle: `(x, y)` is the top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// Builds a rectangle from its top-left corner and size.
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Builds a rectangle from an origin and a size.
    pub const fn from_parts(origin: Point, size: Size) -> Self {
        Self::new(origin.x, origin.y, size.w, size.h)
    }

    /// The top-left corner.
    pub const fn origin(&self) -> Point {
        Point::new(self.x, self.y)
    }

    /// The width and height.
    pub const fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }

    /// The right edge (`x + w`).
    pub fn max_x(&self) -> f64 {
        self.x + self.w
    }

    /// The bottom edge (`y + h`).
    pub fn max_y(&self) -> f64 {
        self.y + self.h
    }

    /// The center point.
    pub fn center(&self) -> Point {
        Point::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// The area; zero for degenerate rectangles.
    pub fn area(&self) -> f64 {
        if self.is_degenerate() {
            0.0
        } else {
            self.w * self.h
        }
    }

    /// True when any value is not finite or the width or height is not positive.
    pub fn is_degenerate(&self) -> bool {
        !(self.x.is_finite() && self.y.is_finite() && self.w.is_finite() && self.h.is_finite())
            || self.w <= 0.0
            || self.h <= 0.0
    }

    /// True when `p` lies inside (left and top edges inclusive, right and bottom exclusive).
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.max_x() && p.y >= self.y && p.y < self.max_y()
    }

    /// True when `other` lies entirely inside `self`.
    pub fn contains_rect(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.max_x() <= self.max_x()
            && other.max_y() <= self.max_y()
    }

    /// The overlap of two rectangles, or `None` when they do not overlap with positive area.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let max_x = self.max_x().min(other.max_x());
        let max_y = self.max_y().min(other.max_y());
        (max_x > x && max_y > y).then(|| Rect::new(x, y, max_x - x, max_y - y))
    }

    /// The same rectangle moved by `(dx, dy)`.
    pub fn translate(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// The same size with the top-left corner at `origin`.
    pub fn with_origin(&self, origin: Point) -> Rect {
        Rect::new(origin.x, origin.y, self.w, self.h)
    }

    /// The same size centered on `center`.
    pub fn centered_on(&self, center: Point) -> Rect {
        Rect::new(
            center.x - self.w / 2.0,
            center.y - self.h / 2.0,
            self.w,
            self.h,
        )
    }
}

impl std::fmt::Display for Rect {
    /// `x,y wxh`, rounded to whole points (for logs).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.0},{:.0} {:.0}x{:.0}", self.x, self.y, self.w, self.h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_and_center() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert_eq!(r.max_x(), 110.0);
        assert_eq!(r.max_y(), 70.0);
        assert_eq!(r.center(), Point::new(60.0, 45.0));
        assert_eq!(r.area(), 5000.0);
    }

    #[test]
    fn intersect_overlapping_and_disjoint() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(50.0, 60.0, 100.0, 100.0);
        assert_eq!(a.intersect(&b), Some(Rect::new(50.0, 60.0, 50.0, 40.0)));
        let c = Rect::new(100.0, 0.0, 10.0, 10.0);
        assert_eq!(a.intersect(&c), None, "touching edges do not overlap");
    }

    #[test]
    fn contains_point_and_rect() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(a.contains(Point::new(0.0, 0.0)));
        assert!(!a.contains(Point::new(100.0, 50.0)));
        assert!(a.contains_rect(&Rect::new(10.0, 10.0, 90.0, 90.0)));
        assert!(!a.contains_rect(&Rect::new(10.0, 10.0, 91.0, 90.0)));
    }

    #[test]
    fn translate_and_center_on() {
        let r = Rect::new(0.0, 0.0, 10.0, 20.0);
        assert_eq!(r.translate(5.0, -5.0), Rect::new(5.0, -5.0, 10.0, 20.0));
        assert_eq!(
            r.centered_on(Point::new(50.0, 50.0)),
            Rect::new(45.0, 40.0, 10.0, 20.0)
        );
    }

    #[test]
    fn formats_for_logs() {
        assert_eq!(Rect::new(10.0, 20.5, 300.0, 200.0).to_string(), "10,20 300x200");
    }

    #[test]
    fn degenerate_rects() {
        assert!(Rect::new(0.0, 0.0, 0.0, 10.0).is_degenerate());
        assert!(Rect::new(f64::NAN, 0.0, 10.0, 10.0).is_degenerate());
        assert!(!Rect::new(-5.0, -5.0, 1.0, 1.0).is_degenerate());
        assert_eq!(Rect::new(0.0, 0.0, -1.0, 10.0).area(), 0.0);
    }
}
