//! The strip's size on a display and the stage area beside it, pure.
//!
//! - **Strip scale.** UX-SPEC's strip numbers were measured on a 5K display (visible frame
//!   2560 × 1410 pt). Apple draws smaller cards on smaller screens, so every strip length
//!   (column width, card box, gaps, icons) is multiplied by [`strip_scale`]: the visible
//!   frame's area relative to the 5K one, `(w / 2560) × (h / 1410)`, kept within
//!   [`MIN_STRIP_SCALE`]..1. On a 1800 × 1130 pt laptop that is 0.56: a column of about
//!   113 pt instead of 200 pt. Capacity follows from the scaled card pitch
//!   ([`capacity_for`]).
//! - **Stage area.** Apple keeps the stage's windows clear of the strip. The stage area is
//!   the visible frame minus the strip column and a [`STRIP_CLEARANCE`] margin
//!   ([`stage_inset`]). When we place a window that overlaps the column, [`fit_frame`] moves
//!   it right just enough; a window too wide for the area is also narrowed to fit when
//!   shrinking is on (`[stages] fit_windows`), else it stays put.

use super::geometry::{Rect, Size};
use super::model::{CAPACITY_RANGE, CARD_PITCH, STRIP_MARGIN};

/// The visible frame UX-SPEC's strip numbers were measured on (5K, menu bar 30 pt).
pub const REFERENCE_VISIBLE: Size = Size::new(2560.0, 1410.0);

/// The strip column's width at the reference size (Apple's strip occupies about 0–200 pt).
pub const REFERENCE_STRIP_WIDTH: f64 = 200.0;

/// The smallest strip scale: cards stay legible (a card box of about 85 × 89 pt).
pub const MIN_STRIP_SCALE: f64 = 0.5;

/// Room kept between the strip column and the stage area, in points.
pub const STRIP_CLEARANCE: f64 = 16.0;

/// The factor applied to every strip length on a display whose visible frame is `visible`:
/// the area ratio to [`REFERENCE_VISIBLE`], within [`MIN_STRIP_SCALE`]..1.
pub fn strip_scale(visible: Size) -> f64 {
    let k = (visible.w / REFERENCE_VISIBLE.w) * (visible.h / REFERENCE_VISIBLE.h);
    if k.is_finite() { k.clamp(MIN_STRIP_SCALE, 1.0) } else { 1.0 }
}

/// The strip column's width on a display whose visible frame is `visible`.
pub fn strip_width(visible: Size) -> f64 {
    (REFERENCE_STRIP_WIDTH * strip_scale(visible)).round()
}

/// How far the stage area starts right of the visible frame's left edge: the strip column
/// plus [`STRIP_CLEARANCE`].
pub fn stage_inset(visible: Size) -> f64 {
    strip_width(visible) + STRIP_CLEARANCE
}

/// How many cards fit on a display whose visible frame is `visible` (Apple: 6 at 5K): the
/// visible height less the scaled top and bottom margins, over the scaled card pitch.
pub fn capacity_for(visible: Size) -> usize {
    let (lo, hi) = CAPACITY_RANGE;
    let k = strip_scale(visible);
    if !visible.h.is_finite() {
        return lo;
    }
    let n = ((visible.h - 2.0 * STRIP_MARGIN * k) / (CARD_PITCH * k)).floor();
    if n.is_nan() || n < lo as f64 { lo } else { (n as usize).min(hi) }
}

/// How the model keeps placed windows clear of the strip.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FitOptions {
    /// The stage area's left inset from the visible frame ([`stage_inset`]); 0 turns fitting
    /// off (the strip is hidden by the user or by Apple's "Show recent apps" being off).
    pub inset: f64,
    /// Narrow a window too wide for the stage area (`[stages] fit_windows`); else such a
    /// window stays where it is.
    pub shrink: bool,
}

impl FitOptions {
    /// True when placed windows are kept clear of the strip.
    pub fn active(&self) -> bool {
        self.inset > 0.0 && self.inset.is_finite()
    }
}

/// The stage area: `visible` minus `inset` points on the left.
pub fn stage_area(visible: Rect, inset: f64) -> Rect {
    let inset = inset.clamp(0.0, visible.w.max(0.0));
    Rect::new(visible.x + inset, visible.y, visible.w - inset, visible.h)
}

/// Where a window at `frame` goes to stay clear of the strip, given the stage `area`:
/// unchanged when its left edge is already inside the area; else moved right to the area's
/// left edge (never resized) when it fits; else, with `shrink`, also narrowed to the area's
/// width (height and `y` unchanged); else unchanged (too wide, left where it is).
pub fn fit_frame(frame: Rect, area: Rect, shrink: bool) -> Rect {
    const SLACK: f64 = 0.5;
    if frame.is_degenerate() || area.is_degenerate() || frame.x >= area.x - SLACK {
        return frame;
    }
    if frame.w <= area.w + SLACK {
        Rect::new(area.x, frame.y, frame.w, frame.h)
    } else if shrink {
        Rect::new(area.x, frame.y, area.w, frame.h)
    } else {
        frame
    }
}

/// True when two frames are the same within a point (AX rounds).
pub fn same_frame(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= 1.0 && (a.y - b.y).abs() <= 1.0 && same_size(a, b)
}

/// True when two frames have the same size within a point.
pub fn same_size(a: Rect, b: Rect) -> bool {
    (a.w - b.w).abs() <= 1.0 && (a.h - b.h).abs() <= 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAPTOP: Size = Size::new(1800.0, 1130.0);
    const FIVE_K: Size = Size::new(2560.0, 1410.0);

    #[test]
    fn the_strip_keeps_the_5k_size_and_narrows_on_a_laptop() {
        assert_eq!(strip_scale(FIVE_K), 1.0);
        assert_eq!(strip_width(FIVE_K), 200.0);
        assert_eq!(capacity_for(FIVE_K), 6, "Apple's six at 5K");
        let w = strip_width(LAPTOP);
        assert!((105.0..=125.0).contains(&w), "about 120 pt on a laptop, got {w}");
        assert!(w < strip_width(FIVE_K));
        assert!(capacity_for(LAPTOP) >= 3);
        // Larger screens never grow the strip past the measured size.
        assert_eq!(strip_scale(Size::new(3840.0, 2135.0)), 1.0);
        // A tiny screen keeps legible cards.
        assert_eq!(strip_scale(Size::new(1024.0, 700.0)), MIN_STRIP_SCALE);
        assert_eq!(strip_scale(Size::new(f64::NAN, 1.0)), 1.0);
        assert_eq!(capacity_for(Size::new(1800.0, f64::NAN)), 3);
        assert_eq!(stage_inset(FIVE_K), 216.0);
    }

    #[test]
    fn a_window_over_the_strip_moves_right_just_enough() {
        let visible = Rect::new(0.0, 39.0, 1800.0, 1130.0);
        let area = stage_area(visible, 130.0);
        assert_eq!(area, Rect::new(130.0, 39.0, 1670.0, 1130.0));
        let f = fit_frame(Rect::new(40.0, 100.0, 900.0, 600.0), area, true);
        assert_eq!(f, Rect::new(130.0, 100.0, 900.0, 600.0), "moved, never resized");
        // Already clear: untouched, even if it runs past the right edge.
        let clear = Rect::new(400.0, 100.0, 1600.0, 600.0);
        assert_eq!(fit_frame(clear, area, true), clear);
        // Exactly at the edge: untouched.
        let edge = Rect::new(130.0, 100.0, 900.0, 600.0);
        assert_eq!(fit_frame(edge, area, true), edge);
    }

    #[test]
    fn a_too_wide_window_is_narrowed_only_when_shrinking_is_on() {
        let area = stage_area(Rect::new(0.0, 39.0, 1800.0, 1130.0), 130.0);
        let wide = Rect::new(0.0, 60.0, 1750.0, 1000.0);
        assert_eq!(fit_frame(wide, area, true), Rect::new(130.0, 60.0, 1670.0, 1000.0));
        assert_eq!(fit_frame(wide, area, false), wide, "nudge-only leaves it put");
        // Exactly the area's width fits without shrinking.
        let exact = Rect::new(10.0, 60.0, 1670.0, 1000.0);
        assert_eq!(fit_frame(exact, area, false), Rect::new(130.0, 60.0, 1670.0, 1000.0));
    }

    #[test]
    fn degenerate_frames_and_off_options_do_nothing() {
        let area = stage_area(Rect::new(0.0, 0.0, 1000.0, 800.0), 100.0);
        let bad = Rect::new(0.0, 0.0, 0.0, 10.0);
        assert_eq!(fit_frame(bad, area, true), bad);
        assert!(!FitOptions::default().active());
        assert!(FitOptions { inset: 130.0, shrink: true }.active());
        assert_eq!(stage_area(Rect::new(0.0, 0.0, 100.0, 100.0), 500.0).w, 0.0);
    }
}
