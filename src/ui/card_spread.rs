//! Apple's spread of a card of several apps, pure: where each window goes on the card before
//! the turn, from the windows' sizes and the side of the front window each sits on.
//!
//! Measured 11-16 on the laptop (1800 × 1169 pt) through the window list, which lists each of
//! Apple's strip pictures at its bounds: six layouts of two and three fixture windows of
//! different apps (`docs/UX-SPEC.md`, "A card's look"). Apple does not draw the windows' real
//! layout: two windows 856 pt apart came out 27 pt apart, a small window centred in a big one
//! came out beside it. It steps them sideways by a share of their widths:
//!
//! - **Side.** The front window anchors the card; each other window goes to the side of it
//!   its middle lies on, on the screen (level: the left, as before).
//! - **Step.** Of two windows side by side, the right one's left edge sits `step` of the left
//!   one's width right of the left one's, plus `narrow` of how much narrower the right one is
//!   ([`pair_step`]): equal windows overlap by three quarters before the turn (the turn
//!   narrows each to about 0.6 of that, so the one behind shows about 40 % of itself), a small
//!   window comes out beside a big one.
//! - **Third.** A third window on the second's side sits `third` of its own step from the
//!   front window: between the two, behind both.
//! - **Height.** The windows are lined up `line` of the way down each, the second raised
//!   `raise`, the third `raise_third`; one scale for all (sizes relative to each other kept),
//!   chosen so the card's windows span `height` top to bottom, never below `min_scale` of the
//!   plain scale, never wider than the widest card.

use crate::engine::geometry::{Rect, Size};

/// The spread's numbers (`[cards] spread_*`), lengths already in strip points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spread {
    pub step: f64,
    pub narrow: f64,
    pub third: f64,
    pub line: f64,
    pub raise: f64,
    pub raise_third: f64,
    pub height: f64,
    pub min_scale: f64,
}

impl Spread {
    /// Apple's, as measured 11-16 (the `[cards]` defaults).
    pub const APPLE: Spread =
        Spread { step: 0.25, narrow: 0.65, third: 0.65, line: 0.85, raise: 12.0, raise_third: 19.0, height: 102.0, min_scale: 0.875 };
}

/// How far right of a window `left` wide the left edge of the window right of it, `right`
/// wide, sits: `step` of the left one's width plus `narrow` of how much narrower the right one
/// is.
pub fn pair_step(left: f64, right: f64, p: &Spread) -> f64 {
    p.step * left + p.narrow * (left - right).max(0.0)
}

/// Each window's left edge relative to the front window's, at the plain scale: `widths` and
/// `sides` front window first (`sides[k]` -1 left of the front window, +1 right; the front
/// window's own is not read). A fourth window or more (not measured) takes the third's place
/// rule.
pub fn offsets(widths: &[f64], sides: &[f64], p: &Spread) -> Vec<f64> {
    let Some(&front) = widths.first() else { return Vec::new() };
    let side = |k: usize| if sides.get(k).copied().unwrap_or(-1.0) > 0.0 { 1.0 } else { -1.0 };
    (0..widths.len())
        .map(|k| {
            if k == 0 {
                return 0.0;
            }
            let s = side(k);
            let step = if s < 0.0 { pair_step(widths[k], front, p) } else { pair_step(front, widths[k], p) };
            let share = if k == 1 || s != side(1) { 1.0 } else { p.third };
            s * step * share
        })
        .collect()
}

/// How far up the window `rank` places behind the front one is raised: 0 for the front
/// window, `raise`, `raise_third`, then on by the same step (not measured).
pub fn raise(rank: usize, p: &Spread) -> f64 {
    match rank {
        0 => 0.0,
        1 => p.raise,
        _ => p.raise_third + (rank - 2) as f64 * (p.raise_third - p.raise),
    }
}

/// The span, top to bottom, of windows `heights` tall (front first) scaled by `s` and lined up
/// as [`arrange`] lines them up.
fn span(heights: &[f64], s: f64, p: &Spread) -> f64 {
    let above = heights.iter().enumerate().map(|(k, h)| p.line * h * s + raise(k, p)).fold(f64::NEG_INFINITY, f64::max);
    let below = heights.iter().enumerate().map(|(k, h)| (1.0 - p.line) * h * s - raise(k, p)).fold(f64::NEG_INFINITY, f64::max);
    above + below
}

/// The one scale of a spread card, on top of the plain one: its windows `heights` tall at the
/// plain scale span `height`, at least `min_scale`, at most `cap` (the widest card).
pub fn scale(heights: &[f64], p: &Spread, cap: f64) -> f64 {
    if heights.is_empty() {
        return 1.0;
    }
    let (mut lo, mut hi) = (1e-3, 100.0);
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if span(heights, mid, p) < p.height { lo = mid } else { hi = mid }
    }
    lo.max(p.min_scale).min(cap.max(p.min_scale))
}

/// The windows of a spread card, front first, before the turn: `plain` their plain sizes,
/// `sides` as for [`offsets`], none grown wider than `max_width`. The leftmost left edge and
/// the highest top are at 0.
pub fn arrange(plain: &[Size], sides: &[f64], p: &Spread, max_width: f64) -> Vec<Rect> {
    if plain.is_empty() {
        return Vec::new();
    }
    let widest = plain.iter().map(|s| s.w).fold(0.0, f64::max);
    let cap = if widest > 0.0 { (max_width / widest).max(1.0) } else { 1.0 };
    let heights: Vec<f64> = plain.iter().map(|s| s.h).collect();
    let s = scale(&heights, p, cap);
    let widths: Vec<f64> = plain.iter().map(|z| z.w).collect();
    let xs = offsets(&widths, sides, p);
    let line = heights.iter().enumerate().map(|(k, h)| p.line * h * s + raise(k, p)).fold(f64::NEG_INFINITY, f64::max);
    let rects: Vec<Rect> = plain
        .iter()
        .enumerate()
        .map(|(k, z)| Rect::new(xs[k] * s, line - raise(k, p) - p.line * z.h * s, z.w * s, z.h * s))
        .collect();
    let x0 = rects.iter().map(|r| r.x).fold(f64::INFINITY, f64::min);
    let y0 = rects.iter().map(|r| r.y).fold(f64::INFINITY, f64::min);
    rects.iter().map(|r| r.translate(-x0, -y0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Spread = Spread::APPLE;

    #[test]
    fn equal_windows_step_a_quarter_and_a_narrower_one_comes_out_further() {
        assert_eq!(pair_step(100.0, 100.0, &P), 25.0);
        // A window half as wide right of a big one: a quarter of the big one, and 0.65 of the
        // difference (Apple's small window centred in a big one, 11-16: beside it).
        assert!((pair_step(130.0, 65.0, &P) - (32.5 + 42.25)).abs() < 1e-9);
        // A wider window on the right steps only the quarter of the left one.
        assert_eq!(pair_step(50.0, 170.0, &P), 12.5);
    }

    #[test]
    fn each_window_goes_to_its_side_and_a_third_sits_between() {
        let x = offsets(&[100.0, 100.0], &[0.0, -1.0], &P);
        assert_eq!(x, vec![0.0, -25.0], "behind, on the left");
        let x = offsets(&[100.0, 100.0], &[0.0, 1.0], &P);
        assert_eq!(x, vec![0.0, 25.0], "behind, on the right");
        // Two on the left: the second its full step out, the third 0.65 of its own.
        let x = offsets(&[76.0, 116.0, 116.0], &[0.0, -1.0, -1.0], &P);
        let full = pair_step(116.0, 76.0, &P);
        assert!((x[1] + full).abs() < 1e-9 && (x[2] + 0.65 * full).abs() < 1e-9, "{x:?}");
        // One on each side: each its full step (not measured).
        let x = offsets(&[100.0, 100.0, 100.0], &[0.0, -1.0, 1.0], &P);
        assert_eq!(x, vec![0.0, -25.0, 25.0]);
    }

    #[test]
    fn the_card_is_scaled_to_its_height_but_big_windows_not_below_the_floor() {
        // Two 90 pt windows, the one behind 12 up: 102 pt, as measured at the plain scale.
        assert!((scale(&[90.0, 90.0], &P, 10.0) - 1.0).abs() < 1e-6);
        // Small ones grow, big ones shrink to the floor at most.
        assert!(scale(&[60.0, 60.0], &P, 10.0) > 1.4);
        assert_eq!(scale(&[300.0, 300.0], &P, 10.0), 0.875);
        // Never wider than the widest card.
        assert_eq!(scale(&[20.0, 20.0], &P, 2.0), 2.0);
    }

    #[test]
    fn arranged_windows_start_at_the_corner_the_front_one_lowest() {
        let plain = [Size::new(100.0, 90.0), Size::new(100.0, 90.0)];
        let r = arrange(&plain, &[0.0, -1.0], &P, 256.0);
        assert!(r[1].x == 0.0 && (r[0].x - 25.0).abs() < 1e-6, "{r:?}");
        assert!((r[1].y).abs() < 1e-9 && (r[0].y - 12.0).abs() < 1e-6, "{r:?}");
        assert!((r[0].h - 90.0).abs() < 1e-6);
        assert!(arrange(&[], &[], &P, 256.0).is_empty());
    }
}
