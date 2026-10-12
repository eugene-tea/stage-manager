//! Placement math for new windows: center and cascade.
//!
//! Placement never resizes a window unless it is larger than the visible frame. There is no
//! tiling or snapping: these only choose an origin.

use super::geometry::{Rect, Size};

/// Default cascade offset in points: enough to leave the edges of windows behind exposed.
pub const CASCADE_OFFSET: f64 = 32.0;

/// Inset from the visible top-left corner used when a cascade wraps.
pub const CASCADE_WRAP_INSET: f64 = 16.0;

fn fit(size: Size, visible: Rect) -> Size {
    Size::new(size.w.min(visible.w), size.h.min(visible.h))
}

/// `frame` at its current size (shrunk only if larger than `visible`), centered in `visible`.
/// The origin is rounded to whole points.
pub fn center(frame: Rect, visible: Rect) -> Rect {
    let size = fit(frame.size(), visible);
    let x = (visible.x + (visible.w - size.w) / 2.0).round();
    let y = (visible.y + (visible.h - size.h) / 2.0).round();
    Rect::new(x, y, size.w, size.h)
}

/// Cascade `frame` from the frontmost window `front`.
///
/// Places the window at `front`'s origin plus `(offset, offset)`. When that would overflow the
/// right or bottom edge of `visible`, wraps to the visible top-left plus
/// [`CASCADE_WRAP_INSET`]. With no front window the frame keeps its origin. The size changes
/// only when it exceeds `visible`; a frame that still overflows after wrapping is pulled
/// inside.
pub fn cascade(frame: Rect, front: Option<Rect>, visible: Rect, offset: f64) -> Rect {
    let size = fit(frame.size(), visible);
    let Some(front) = front else {
        return Rect::new(frame.x, frame.y, size.w, size.h);
    };
    let mut x = front.x + offset;
    let mut y = front.y + offset;
    if x + size.w > visible.max_x() || y + size.h > visible.max_y() {
        x = visible.x + CASCADE_WRAP_INSET;
        y = visible.y + CASCADE_WRAP_INSET;
    }
    x = x.min(visible.max_x() - size.w).max(visible.x);
    y = y.min(visible.max_y() - size.h).max(visible.y);
    Rect::new(x, y, size.w, size.h)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VISIBLE: Rect = Rect::new(0.0, 25.0, 1440.0, 875.0);

    #[test]
    fn center_keeps_size() {
        let f = center(Rect::new(5.0, 5.0, 400.0, 300.0), VISIBLE);
        assert_eq!(f, Rect::new(520.0, 313.0, 400.0, 300.0));
    }

    #[test]
    fn center_shrinks_only_when_too_big() {
        let f = center(Rect::new(0.0, 0.0, 2000.0, 300.0), VISIBLE);
        assert_eq!(f, Rect::new(0.0, 313.0, 1440.0, 300.0));
    }

    #[test]
    fn center_on_offset_display() {
        let vis = Rect::new(1440.0, -100.0, 1000.0, 800.0);
        let f = center(Rect::new(0.0, 0.0, 200.0, 200.0), vis);
        assert_eq!(f, Rect::new(1840.0, 200.0, 200.0, 200.0));
    }

    #[test]
    fn cascade_offsets_from_front() {
        let front = Rect::new(100.0, 100.0, 600.0, 400.0);
        let f = cascade(Rect::new(0.0, 0.0, 500.0, 300.0), Some(front), VISIBLE, CASCADE_OFFSET);
        assert_eq!(f, Rect::new(132.0, 132.0, 500.0, 300.0));
    }

    #[test]
    fn cascade_without_front_keeps_origin() {
        let f = cascade(Rect::new(70.0, 80.0, 500.0, 300.0), None, VISIBLE, CASCADE_OFFSET);
        assert_eq!(f, Rect::new(70.0, 80.0, 500.0, 300.0));
    }

    #[test]
    fn cascade_wraps_on_right_overflow() {
        let front = Rect::new(1000.0, 100.0, 400.0, 300.0);
        let f = cascade(Rect::new(0.0, 0.0, 420.0, 300.0), Some(front), VISIBLE, CASCADE_OFFSET);
        assert_eq!(f, Rect::new(16.0, 41.0, 420.0, 300.0));
    }

    #[test]
    fn cascade_wraps_on_bottom_overflow() {
        let front = Rect::new(100.0, 600.0, 400.0, 300.0);
        let f = cascade(Rect::new(0.0, 0.0, 400.0, 300.0), Some(front), VISIBLE, CASCADE_OFFSET);
        assert_eq!(f, Rect::new(16.0, 41.0, 400.0, 300.0));
    }

    #[test]
    fn cascade_exact_fit_does_not_wrap() {
        // Right edge lands exactly on the visible edge.
        let front = Rect::new(908.0, 100.0, 400.0, 300.0);
        let f = cascade(Rect::new(0.0, 0.0, 500.0, 300.0), Some(front), VISIBLE, CASCADE_OFFSET);
        assert_eq!(f.x, 940.0);
        assert_eq!(f.max_x(), VISIBLE.max_x());
    }

    #[test]
    fn cascade_huge_window_is_shrunk_and_pinned_inside() {
        let front = Rect::new(100.0, 100.0, 400.0, 300.0);
        let f = cascade(Rect::new(0.0, 0.0, 5000.0, 5000.0), Some(front), VISIBLE, CASCADE_OFFSET);
        assert_eq!(f, VISIBLE);
    }

    #[test]
    fn cascade_nearly_full_size_wraps_then_clamps() {
        let front = Rect::new(0.0, 25.0, 400.0, 300.0);
        let f = cascade(Rect::new(0.0, 0.0, 1430.0, 870.0), Some(front), VISIBLE, CASCADE_OFFSET);
        assert_eq!(f, Rect::new(10.0, 30.0, 1430.0, 870.0));
        assert!(VISIBLE.contains_rect(&f));
    }

    #[test]
    fn repeated_cascade_walks_then_wraps() {
        let mut front = Rect::new(16.0, 41.0, 600.0, 500.0);
        let mut wrapped = false;
        for _ in 0..20 {
            let next = cascade(Rect::new(0.0, 0.0, 600.0, 500.0), Some(front), VISIBLE, CASCADE_OFFSET);
            assert!(VISIBLE.contains_rect(&next));
            if next.x < front.x {
                wrapped = true;
            }
            front = next;
        }
        assert!(wrapped);
    }
}
