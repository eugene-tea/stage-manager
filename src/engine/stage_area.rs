//! The strip's size on a display and the stage area beside it, pure.
//!
//! - **Strip lengths.** Apple draws the strip with the same lengths on every display (the
//!   card x, gaps and icons measured on a 1800 pt laptop and a 5K display match; a card's
//!   size follows its window, see `ui::strip_layout`). Every strip length is multiplied by
//!   [`length_scale`]: `[strip] scale` (1 by default), times [`strip_scale`] when
//!   `[strip] scale_to_display` asks to shrink strips on small screens.
//! - **Capacity** comes from the visible height ([`capacity_for`], `[strip] capacity*`).
//! - **Stage area.** Apple leaves windows where they are, even over the strip (the strip
//!   then hides). With `[windows] fit` on, the stage area is the visible frame minus the
//!   strip column and a margin ([`stage_inset`]); when we place a window that overlaps the
//!   column, [`fit_frame`] moves it right just enough, and with `"narrow"` also narrows a
//!   window too wide for the area.

use super::geometry::{Rect, Size};
use crate::config::{CapacityRule, StripStyle};

/// The visible frame the area-based scale is relative to (5K, menu bar 30 pt).
pub const REFERENCE_VISIBLE: Size = Size::new(2560.0, 1410.0);

/// The smallest area-based scale: cards stay legible.
pub const MIN_STRIP_SCALE: f64 = 0.5;

/// The area-based factor `(w / 2560) × (h / 1410)` within [`MIN_STRIP_SCALE`]..1, used only
/// with `[strip] scale_to_display`.
pub fn strip_scale(visible: Size) -> f64 {
    let k = (visible.w / REFERENCE_VISIBLE.w) * (visible.h / REFERENCE_VISIBLE.h);
    if k.is_finite() { k.clamp(MIN_STRIP_SCALE, 1.0) } else { 1.0 }
}

/// The factor applied to every strip length on a display whose visible frame is `visible`.
pub fn length_scale(visible: Size, style: &StripStyle) -> f64 {
    let display = if style.scale_to_display { strip_scale(visible) } else { 1.0 };
    style.scale * display
}

/// The strip column's width on a display whose visible frame is `visible`.
pub fn strip_width(visible: Size, style: &StripStyle) -> f64 {
    (style.width * length_scale(visible, style)).round()
}

/// How far the stage area starts right of the visible frame's left edge: the strip column
/// plus `margin`.
pub fn stage_inset(visible: Size, style: &StripStyle, margin: f64) -> f64 {
    strip_width(visible, style) + margin
}

/// How many cards fit on a display whose visible frame is `visible`.
pub fn capacity_for(visible: Size, rule: &CapacityRule, style: &StripStyle) -> usize {
    rule.for_height(visible.h, length_scale(visible, style))
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
    fn the_strip_has_apples_size_everywhere_unless_asked() {
        let style = StripStyle::default();
        let rule = crate::config::Config::default().strip.capacity;
        assert_eq!(length_scale(LAPTOP, &style), 1.0);
        assert_eq!(strip_width(LAPTOP, &style), strip_width(FIVE_K, &style));
        assert_eq!(strip_width(FIVE_K, &style), 200.0);
        assert_eq!(capacity_for(LAPTOP, &rule, &style), 5, "Apple's five at 1130 pt");
        assert_eq!(capacity_for(FIVE_K, &rule, &style), 6, "Apple's six at 1410 pt");
        assert_eq!(stage_inset(FIVE_K, &style, 16.0), 216.0);
        // Round 4A's display scaling, now an option.
        let scaled = StripStyle { scale_to_display: true, ..StripStyle::default() };
        let w = strip_width(LAPTOP, &scaled);
        assert!((105.0..=125.0).contains(&w), "about 113 pt on a laptop, got {w}");
        assert_eq!(strip_width(FIVE_K, &scaled), 200.0);
        assert_eq!(strip_scale(Size::new(3840.0, 2135.0)), 1.0);
        assert_eq!(strip_scale(Size::new(1024.0, 700.0)), MIN_STRIP_SCALE);
        assert_eq!(strip_scale(Size::new(f64::NAN, 1.0)), 1.0);
        let big = StripStyle { scale: 1.5, ..StripStyle::default() };
        assert_eq!(strip_width(FIVE_K, &big), 300.0);
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
