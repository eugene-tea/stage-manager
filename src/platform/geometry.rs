//! Plain geometry values shared by the platform layer (top-left origin, points).

use serde::Serialize;

/// A point in global display coordinates (top-left origin, as AX and CGWindowList use).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// A size in points.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Size {
    pub w: f64,
    pub h: f64,
}

/// A rectangle in global display coordinates (top-left origin).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    /// Builds a rectangle from an origin and a size.
    pub fn from_parts(origin: Point, size: Size) -> Self {
        Self { x: origin.x, y: origin.y, w: size.w, h: size.h }
    }

    /// The rectangle's origin.
    pub fn origin(&self) -> Point {
        Point { x: self.x, y: self.y }
    }

    /// The rectangle's size.
    pub fn size(&self) -> Size {
        Size { w: self.w, h: self.h }
    }
}

impl std::fmt::Display for Rect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.0},{:.0} {:.0}x{:.0}", self.x, self.y, self.w, self.h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_round_trips_parts_and_formats() {
        let r = Rect::from_parts(Point { x: 10.0, y: 20.5 }, Size { w: 300.0, h: 200.0 });
        assert_eq!(r.origin(), Point { x: 10.0, y: 20.5 });
        assert_eq!(r.size(), Size { w: 300.0, h: 200.0 });
        assert_eq!(r.to_string(), "10,20 300x200");
    }
}
