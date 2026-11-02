//! A window shadow's math, pure (round ghost-shadow): the model of macOS's window shadow, its
//! nine-part image, and the handover's fade. Drawn by [`crate::ui::window_shadow`].
//!
//! - **The model.** macOS's window shadow, measured on the owner's Mac (11-01, `stage-manager
//!   probe-shadow --profile`, Studio Display at 2x), is two parts laid over one another: a
//!   hairline rim just outside the window (a sharp line half a point out) and a soft drop
//!   moved down. The key window's drop is about 2.5 times as wide as an inactive window's.
//!   Each part is the window's shape grown by a spread and moved down by an offset, blurred by
//!   a Gaussian ([`ShadowPart`]); fitted to the falloff along every side, the model is within
//!   a few alpha levels (out of 255) of the measured one there.
//! - **Light and dark.** macOS draws a window's shadow by the window's own appearance (a
//!   window drawn in the light appearance on a dark system keeps the light shadow, measured
//!   with the probe's own windows set to each). In the dark one the drop has the same reach
//!   and offset but is nearly twice as dark, and the rim is nearly opaque (a fifth to a third
//!   in the light one): [`WindowShadow::ACTIVE_DARK`], [`WindowShadow::INACTIVE_DARK`].
//! - **The inner rim.** macOS also draws a thin white line just inside the window's edge
//!   (over its content, following its corner), with the shadow, not with the window's own
//!   pixels: a capture that ignores the shadow lacks it ([`InnerRim`]). It reads on a dark
//!   window (a dark-appearance one above all, a point wide there), not on a white one.
//! - **The shape.** A window's corner is Core Animation's continuous corner (Apple's
//!   "squircle", reaching 1.53 radii along each edge), 16 pt on a titled window; a circular
//!   arc of the same radius sits up to half a point off it, which puts the rim a pixel off.
//!   [`corner_inset`] follows the continuous curve.
//! - **The image.** [`render`] draws a shadow once as a nine-part image: the window's shape
//!   just large enough that its middle row and column lie past every corner's reach, so
//!   the middle stretches to any window and the corners stay as drawn.
//! - **The handover.** The real window's shadow appears (or goes) in one frame at a moment
//!   known within a frame or two; where the snapshot's shadow shows over it they add up
//!   darker, where neither shows it is lighter. No fade removes that step, it can only
//!   spread it: [`handover`] centres the snapshot's fade on the expected moment, which
//!   halves the worst error there ([`handover_error`]).

use std::sync::OnceLock;

/// One part of a window shadow: the window's shape grown by `spread`, moved down by
/// `offset_y`, blurred by a Gaussian whose sigma is `blur`, at `opacity`. Points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowPart {
    pub blur: f64,
    pub opacity: f64,
    pub offset_y: f64,
    pub spread: f64,
}

impl ShadowPart {
    /// No part.
    pub const NONE: ShadowPart = ShadowPart { blur: 0.0, opacity: 0.0, offset_y: 0.0, spread: 0.0 };
}

/// The white line macOS draws just inside a window's edge, over its content, with its
/// shadow: `width` points wide, white at `white` (0-1) over the content, following the
/// window's corner. Drawn on a snapshot over its picture ([`crate::ui::window_shadow`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InnerRim {
    pub width: f64,
    pub white: f64,
}

impl InnerRim {
    /// No inner rim.
    pub const NONE: InnerRim = InnerRim { width: 0.0, white: 0.0 };

    /// True when it draws nothing.
    pub fn is_none(&self) -> bool {
        self.width <= 0.0 || self.white <= 0.0
    }
}

/// A window shadow: its parts, laid over one another (source over), under the window, and
/// the inner rim over it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowShadow {
    pub parts: [ShadowPart; 2],
    pub rim: InnerRim,
}

/// The corner radius of a titled window on the owner's Mac (points, continuous), measured
/// from its picture: used when a snapshot's own corner cannot be read.
pub const DEFAULT_CORNER: f64 = 16.0;

/// How far a continuous corner reaches along each edge, in radii.
pub const CORNER_REACH: f64 = 1.528_665;

impl WindowShadow {
    /// The key (active) window's shadow in the light appearance, fitted to the owner's Mac
    /// (11-01): at 2/255 it reaches 45 pt left and right, 24.5 above and 62 below the middle
    /// of each side; the inner rim half a point, white at a tenth.
    pub const ACTIVE: WindowShadow = WindowShadow {
        parts: [
            ShadowPart { blur: 0.25, opacity: 0.315, offset_y: 0.0, spread: 0.35 },
            ShadowPart { blur: 19.25, opacity: 0.395, offset_y: 17.9, spread: 0.65 },
        ],
        rim: InnerRim { width: 0.5, white: 0.10 },
    };
    /// An inactive window's shadow in the light appearance, fitted to the owner's Mac
    /// (11-01): at 2/255 it reaches 15.5 pt left and right, 8.5 above and 23 below the middle
    /// of each side; the inner rim as the key window's.
    pub const INACTIVE: WindowShadow = WindowShadow {
        parts: [
            ShadowPart { blur: 0.25, opacity: 0.22, offset_y: 0.0, spread: 0.5 },
            ShadowPart { blur: 7.8, opacity: 0.215, offset_y: 7.0, spread: 0.2 },
        ],
        rim: InnerRim { width: 0.5, white: 0.10 },
    };
    /// The key window's shadow in the dark appearance, fitted to the owner's Mac (11-01): at
    /// 2/255 it reaches 46 pt left and right, 29 above and 63.5 below the middle of each
    /// side; the hairline 0.95 opaque, the drop 0.35 just past it (light: 0.37, 0.22); the
    /// inner rim a point wide, white at a tenth.
    pub const ACTIVE_DARK: WindowShadow = WindowShadow {
        parts: [
            ShadowPart { blur: 0.03, opacity: 0.93, offset_y: 0.0, spread: 0.52 },
            ShadowPart { blur: 19.25, opacity: 0.68, offset_y: 17.85, spread: 0.95 },
        ],
        rim: InnerRim { width: 1.0, white: 0.10 },
    };
    /// An inactive window's shadow in the dark appearance, fitted to the owner's Mac (11-01):
    /// at 2/255 it reaches 17.5 pt left and right, 10 above and 25.5 below the middle of each
    /// side; the hairline 0.87 opaque, the drop 0.23 just past it (light: 0.29, 0.12); the
    /// inner rim a point wide, white at 0.08.
    pub const INACTIVE_DARK: WindowShadow = WindowShadow {
        parts: [
            ShadowPart { blur: 0.03, opacity: 0.83, offset_y: 0.0, spread: 0.52 },
            ShadowPart { blur: 7.37, opacity: 0.425, offset_y: 6.8, spread: 0.9 },
        ],
        rim: InnerRim { width: 1.0, white: 0.08 },
    };

    /// No shadow.
    pub const NONE: WindowShadow = WindowShadow { parts: [ShadowPart::NONE; 2], rim: InnerRim::NONE };

    /// macOS's shadow for a window drawn in the dark appearance (`dark`) or the light one,
    /// key (`key`) or not.
    pub fn of_window(key: bool, dark: bool) -> WindowShadow {
        match (key, dark) {
            (true, false) => Self::ACTIVE,
            (false, false) => Self::INACTIVE,
            (true, true) => Self::ACTIVE_DARK,
            (false, true) => Self::INACTIVE_DARK,
        }
    }

    /// One Core Animation-like shadow (`[animation] shadow = "custom"`): blur `radius` (a
    /// sigma of half of it), `opacity`, moved down `offset_y`; no rim.
    pub fn custom(radius: f64, opacity: f64, offset_y: f64) -> WindowShadow {
        WindowShadow {
            parts: [ShadowPart { blur: radius.max(0.0) / 2.0, opacity: opacity.clamp(0.0, 1.0), offset_y, spread: 0.0 }, ShadowPart::NONE],
            rim: InnerRim::NONE,
        }
    }

    /// How far past the window (points, every side) the shadow can reach: 3.5 sigmas of the
    /// widest part plus its offset and spread, in whole points, plus one.
    pub fn extent(&self) -> f64 {
        self.parts.iter().filter(|p| p.opacity > 0.0).map(|p| 3.5 * p.blur + p.offset_y.abs() + p.spread.max(0.0)).fold(0.0, f64::max).ceil() + 1.0
    }

    /// The same shadow with every part's opacity times `k`.
    pub fn scaled(&self, k: f64) -> WindowShadow {
        let mut s = *self;
        for p in &mut s.parts {
            p.opacity = (p.opacity * k).clamp(0.0, 1.0);
        }
        s.rim.white = (s.rim.white * k).clamp(0.0, 1.0);
        s
    }
}

/// The standard normal's cumulative distribution (Abramowitz and Stegun 7.1.26 on erf, error
/// under 1.5e-7).
pub fn phi(x: f64) -> f64 {
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t * (0.254_829_592 + t * (-0.284_496_736 + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-z * z).exp();
    if x >= 0.0 { 0.5 * (1.0 + erf) } else { 0.5 * (1.0 - erf) }
}

/// The continuous corner of radius 1 as `(dy, inset)` pairs, `dy` rising from the edge: three
/// cubic Béziers (a transition in from the edge, the arc, a transition out), as Core
/// Animation draws `kCACornerCurveContinuous`.
fn corner_table() -> &'static [(f64, f64)] {
    static TABLE: OnceLock<Vec<(f64, f64)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        // (inset from the vertical edge, distance from the horizontal edge), in radii.
        const SEGMENTS: [[(f64, f64); 4]; 3] = [
            [(1.528_665, 0.0), (1.088_493, 0.0), (0.868_407, 0.0), (0.631_494, 0.074_911)],
            [(0.631_494, 0.074_911), (0.372_824, 0.169_057), (0.169_057, 0.372_824), (0.074_911, 0.631_494)],
            [(0.074_911, 0.631_494), (0.0, 0.868_407), (0.0, 1.088_493), (0.0, 1.528_665)],
        ];
        let mut out = Vec::with_capacity(3 * 129);
        for seg in SEGMENTS {
            for i in 0..=128 {
                let t = i as f64 / 128.0;
                let b = [(1.0 - t).powi(3), 3.0 * (1.0 - t).powi(2) * t, 3.0 * (1.0 - t) * t * t, t.powi(3)];
                let x: f64 = (0..4).map(|k| b[k] * seg[k].0).sum();
                let y: f64 = (0..4).map(|k| b[k] * seg[k].1).sum();
                out.push((y, x));
            }
        }
        out
    })
}

/// How far in from the side a continuous corner of radius `r` is at `dy` from the top or
/// bottom edge (0 past [`CORNER_REACH`] radii).
pub fn corner_inset(r: f64, dy: f64) -> f64 {
    if r <= 0.0 || dy >= CORNER_REACH * r {
        return 0.0;
    }
    let u = (dy / r).max(0.0);
    let table = corner_table();
    let i = table.partition_point(|(y, _)| *y < u);
    if i == 0 {
        return table[0].1 * r;
    }
    let ((y0, x0), (y1, x1)) = (table[i - 1], table[i.min(table.len() - 1)]);
    let k = if y1 > y0 { (u - y0) / (y1 - y0) } else { 0.0 };
    (x0 + k * (x1 - x0)) * r
}

/// How far in along the diagonal a continuous corner of radius 1 is (its arc's middle).
pub const DIAGONAL_INSET: f64 = 0.291_5;

/// The left and right edges of the shape `[x0, x1] x [y0, y1]` with continuous corners of
/// radius `r` at height `y`; `None` outside it.
fn row_span(x0: f64, x1: f64, y0: f64, y1: f64, r: f64, y: f64) -> Option<(f64, f64)> {
    if y < y0 || y > y1 {
        return None;
    }
    let r = r.min((x1 - x0) / (2.0 * CORNER_REACH)).min((y1 - y0) / (2.0 * CORNER_REACH)).max(0.0);
    let inset = corner_inset(r, (y - y0).min(y1 - y));
    Some((x0 + inset, x1 - inset))
}

/// Sub-rows sampled down each pixel row.
const DOWN: usize = 4;

/// One part's alpha (0-1, before its opacity) over a `w` x `h` pixel grid (`scale` pixels a
/// point), the window at `frame` (x, y, w, h, points) with corner radius `corner`: its shape
/// grown and moved as the part says, blurred. Across, each row of the shape is one span,
/// blurred by the normal distribution; down, a numerical Gaussian, over sub-rows for a blur
/// under two pixels (the rim), over pixel rows otherwise.
fn part_alpha(part: &ShadowPart, frame: (f64, f64, f64, f64), corner: f64, w: usize, h: usize, scale: f64) -> Vec<f32> {
    let (fx, fy, fw, fh) = frame;
    let s = part.spread;
    let (x0, x1, y0, y1) = (fx - s, fx + fw + s, fy - s + part.offset_y, fy + fh + s + part.offset_y);
    let r = (corner + s).max(0.0);
    let sigma = part.blur * scale;
    let fine = sigma < 2.0;
    let rows = if fine { h * DOWN } else { h };
    let mut across = vec![0f32; w * rows];
    for y in 0..h {
        for sub in 0..DOWN {
            let yp = (y as f64 + (sub as f64 + 0.5) / DOWN as f64) / scale;
            let Some((l, rr)) = row_span(x0, x1, y0, y1, r, yp) else { continue };
            let (l, rr) = (l * scale, rr * scale);
            let (row, k) = if fine { ((y * DOWN + sub) * w, 1.0) } else { (y * w, 1.0 / DOWN as f64) };
            for x in 0..w {
                let c = x as f64 + 0.5;
                let v = if sigma <= 1e-3 { (rr.min(c + 0.5) - l.max(c - 0.5)).clamp(0.0, 1.0) } else { phi((rr - c) / sigma) - phi((l - c) / sigma) };
                across[row + x] += (v * k) as f32;
            }
        }
    }
    let sigma_rows = if fine { sigma * DOWN as f64 } else { sigma };
    let blurred = if sigma_rows <= 1e-3 { across } else { blur_down(&across, w, rows, sigma_rows) };
    if !fine {
        return blurred;
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for sub in 0..DOWN {
            let from = (y * DOWN + sub) * w;
            for x in 0..w {
                out[y * w + x] += blurred[from + x] / DOWN as f32;
            }
        }
    }
    out
}

/// `grid` (`w` wide, `rows` high) blurred down by a Gaussian of `sigma` rows, 3.5 sigmas each
/// way, zero past the edges.
fn blur_down(grid: &[f32], w: usize, rows: usize, sigma: f64) -> Vec<f32> {
    let reach = (3.5 * sigma).ceil() as isize;
    let kernel: Vec<f32> = (-reach..=reach).map(|i| (-(i as f64).powi(2) / (2.0 * sigma * sigma)).exp() as f32).collect();
    let total: f32 = kernel.iter().sum();
    let mut out = vec![0f32; w * rows];
    for y in 0..rows as isize {
        let row = y as usize * w;
        for (k, kv) in kernel.iter().enumerate() {
            let src = y + k as isize - reach;
            if src < 0 || src >= rows as isize {
                continue;
            }
            let from = src as usize * w;
            let kv = kv / total;
            for (o, g) in out[row..row + w].iter_mut().zip(&grid[from..from + w]) {
                *o += g * kv;
            }
        }
    }
    out
}

/// The shadow's alpha (0-1) over a `w` x `h` pixel grid, the window at `frame` (points).
pub fn shadow_alpha(shadow: &WindowShadow, frame: (f64, f64, f64, f64), corner: f64, w: usize, h: usize, scale: f64) -> Vec<f32> {
    let mut clear = vec![1f32; w * h];
    for part in shadow.parts.iter().filter(|p| p.opacity > 0.0) {
        let a = part_alpha(part, frame, corner, w, h, scale);
        for (c, v) in clear.iter_mut().zip(a) {
            *c *= 1.0 - (part.opacity as f32 * v).clamp(0.0, 1.0);
        }
    }
    clear.into_iter().map(|c| 1.0 - c).collect()
}

/// A shadow drawn as a nine-part image: premultiplied black BGRA, `w` x `h` pixels (top row
/// first) at `scale` pixels a point, the window's rectangle `extent` points in from every edge,
/// `slice` (x, y, w, h in the unit square) the middle part that stretches.
#[derive(Debug, Clone)]
pub struct NinePart {
    pub bytes: Vec<u8>,
    pub w: usize,
    pub h: usize,
    pub scale: f64,
    pub extent: f64,
    pub slice: [f64; 4],
}

/// Draws `shadow` around a window with continuous corners of radius `corner`, at `scale`
/// pixels a point, as a nine-part image: the window's shape just large enough that its middle
/// row and column lie past every corner's reach, plus one point that stretches.
pub fn render(shadow: &WindowShadow, corner: f64, scale: f64) -> NinePart {
    let extent = shadow.extent();
    let core = 2.0 * (CORNER_REACH * corner.max(0.0) + extent).ceil() + 1.0;
    let side = core + 2.0 * extent;
    let n = (side * scale).round() as usize;
    let alpha = shadow_alpha(shadow, (extent, extent, core, core), corner, n, n, scale);
    let mut bytes = Vec::with_capacity(n * n * 4);
    for a in alpha {
        bytes.extend_from_slice(&[0, 0, 0, (a * 255.0).round().clamp(0.0, 255.0) as u8]);
    }
    // The middle point stretches (the image is symmetric about it both ways but for the
    // drop's offset, which the middle row is past).
    let mid = (side / 2.0 - 0.5) / side;
    let one = 1.0 / side;
    NinePart { bytes, w: n, h: n, scale, extent, slice: [mid, mid, one, one] }
}

/// The alpha (0-1) a nine-part image shows at `(x, y)` points from the window's top-left,
/// stretched around a window of `size` points as Core Animation stretches it (the middle
/// slice stretched, the rest at its size). For tests and the probe.
pub fn nine_part_at(img: &NinePart, size: (f64, f64), x: f64, y: f64) -> f64 {
    let side = img.w as f64 / img.scale;
    let half = side / 2.0;
    let (lw, lh) = (size.0 + 2.0 * img.extent, size.1 + 2.0 * img.extent);
    let map = |v: f64, len: f64| -> f64 {
        if v < half {
            v
        } else if v > len - half {
            side - (len - v)
        } else {
            half
        }
    };
    let (u, v) = (map(x + img.extent, lw), map(y + img.extent, lh));
    let (px, py) = ((u * img.scale).floor() as isize, (v * img.scale).floor() as isize);
    if px < 0 || py < 0 || px as usize >= img.w || py as usize >= img.h {
        return 0.0;
    }
    f64::from(img.bytes[(py as usize * img.w + px as usize) * 4 + 3]) / 255.0
}

/// A window's corner radius from its picture: `inset` points along the diagonal from the
/// corner to the first half-opaque pixel, a continuous corner's arc being [`DIAGONAL_INSET`]
/// radii in there. Bounded to 0..=40 pt.
pub fn corner_from_inset(inset: f64) -> f64 {
    (inset / DIAGONAL_INSET).clamp(0.0, 40.0)
}

/// When the snapshot's shadow fades at a handover, around the moment `at` (seconds from now
/// on the snapshot's clock) the real window's shadow is expected to change on screen: from
/// `spread / 2` before it to `spread / 2` after, linear, halfway at `at`; never before now.
/// Returns `(begin, duration)`.
pub fn handover(at: f64, spread: f64) -> (f64, f64) {
    let begin = at - spread / 2.0;
    if begin >= 0.0 { (begin, spread.max(0.0)) } else { (0.0, (at + spread / 2.0).max(0.0)) }
}

/// The snapshot's shadow (0-1 of full) at time `t` of a fade from 1 to 0 that begins at
/// `begin` and lasts `duration`, linear.
pub fn fade_out(t: f64, begin: f64, duration: f64) -> f64 {
    if t <= begin {
        1.0
    } else if duration <= 0.0 || t >= begin + duration {
        0.0
    } else {
        1.0 - (t - begin) / duration
    }
}

/// How far the shadow on screen is from the window's own (alpha `a` at a point) at time `t`
/// of an incoming handover, when the window's shadow shows from `shown` on and the snapshot's
/// fades as [`fade_out`]: positive is darker. Before the window shows, the snapshot's `f a`
/// alone; after, `f a` over the window's `a`.
pub fn handover_error(a: f64, t: f64, shown: f64, begin: f64, duration: f64) -> f64 {
    let f = fade_out(t, begin, duration) * a;
    if t >= shown { f + a * (1.0 - f) - a } else { f - a }
}

/// How the inner rim, laid out at the window's `window` size (points) so its corner and width
/// are the window's, is scaled to a plane of `plane` size: as the snapshot's picture is, so
/// the rim stays on the picture's edge and corner all the way (`(1, 1)` at the window).
pub fn rim_scale(plane: (f64, f64), window: (f64, f64)) -> (f64, f64) {
    let ratio = |p: f64, w: f64| if w > 0.0 { (p / w).max(0.0) } else { 1.0 };
    (ratio(plane.0, window.0), ratio(plane.1, window.1))
}

/// The shadow's amount (0 at a card, 1 at the window) for a plane `progress` of the way from
/// `from` to `to` on the switch's spring: the same curve as the plane's motion.
pub fn amount_at(from: f64, to: f64, progress: f64) -> f64 {
    (from + (to - from) * progress).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phi_is_the_normal_distribution() {
        assert!((phi(0.0) - 0.5).abs() < 1e-7);
        assert!((phi(1.0) - 0.841_344_746).abs() < 1e-6);
        assert!((phi(-1.96) - 0.024_997_9).abs() < 1e-6);
        assert!(phi(8.0) > 0.999_999 && phi(-8.0) < 1e-6);
    }

    /// The continuous corner as measured on a titled window (16 pt, 11-01): 11 pt in one point
    /// down, 8.5 at two, 5.5 at four, 2.5 at eight, half a point at fourteen, none at 24.5.
    #[test]
    fn the_continuous_corner_follows_the_measured_window() {
        for (dy, measured) in [(1.0, 11.0), (2.0, 8.5), (4.0, 5.5), (8.0, 2.5), (14.0, 0.5)] {
            let inset = corner_inset(16.0, dy);
            assert!((inset - measured).abs() <= 0.6, "at {dy}: {inset} vs {measured}");
        }
        assert_eq!(corner_inset(16.0, 24.5), 0.0);
        assert!((corner_inset(16.0, 0.0) - 16.0 * CORNER_REACH).abs() < 1e-9);
        assert!((corner_inset(1.0, DIAGONAL_INSET) - DIAGONAL_INSET).abs() < 2e-3, "the arc's middle is on the diagonal");
        assert!((corner_from_inset(DIAGONAL_INSET * 16.0) - 16.0).abs() < 1e-9);
        assert_eq!(corner_from_inset(100.0), 40.0);
    }

    #[test]
    fn a_rounded_row_narrows_at_the_corners() {
        assert_eq!(row_span(0.0, 100.0, 0.0, 50.0, 10.0, 25.0), Some((0.0, 100.0)));
        let (l, r) = row_span(0.0, 100.0, 0.0, 50.0, 10.0, 0.0).unwrap();
        assert!((l - 15.28665).abs() < 1e-6 && (r - 84.71335).abs() < 1e-6, "the top row is the straight edge only");
        assert_eq!(row_span(0.0, 100.0, 0.0, 50.0, 10.0, 60.0), None);
    }

    /// Far from the corners a side falls off as the Gaussian's tail: a part of sigma 4 at
    /// opacity 0.5, 7.5 pt past the edge, is 0.5 (1 - Φ(7.5 / 4)).
    #[test]
    fn a_side_falls_off_as_the_gaussian() {
        let s = WindowShadow { parts: [ShadowPart { blur: 4.0, opacity: 0.5, offset_y: 0.0, spread: 0.0 }, ShadowPart::NONE], rim: InnerRim::NONE };
        let (w, h) = (200, 200);
        let a = shadow_alpha(&s, (50.0, 50.0, 100.0, 100.0), 8.0, w, h, 1.0);
        let at = |x: usize, y: usize| f64::from(a[y * w + x]);
        let want = 0.5 * (1.0 - phi(7.5 / 4.0));
        assert!((at(157, 100) - want).abs() < 2e-3, "{} vs {want}", at(157, 100));
        assert!((at(42, 100) - want).abs() < 2e-3, "left mirrors right");
        assert!((at(100, 157) - want).abs() < 3e-3, "down as across: {}", at(100, 157));
        assert!(at(100, 100) > 0.49, "under the window: the whole opacity");
        assert!(at(5, 5) < 1e-4, "far away: nothing");
    }

    /// The rim: a sharp part half a point out is one pixel wide at 2x.
    #[test]
    fn a_sharp_rim_is_one_pixel_at_2x() {
        let s = WindowShadow { parts: [ShadowPart { blur: 0.25, opacity: 0.3, offset_y: 0.0, spread: 0.5 }, ShadowPart::NONE], rim: InnerRim::NONE };
        let (w, h) = (120, 120);
        let a = shadow_alpha(&s, (10.0, 10.0, 40.0, 40.0), 4.0, w, h, 2.0);
        let at = |x: usize, y: usize| f64::from(a[y * w + x]);
        assert!(at(19, 60) > 0.15, "the pixel just outside the left edge: {}", at(19, 60));
        assert!(at(17, 60) < 0.02, "two pixels out: {}", at(17, 60));
        assert!(at(60, 19) > 0.15 && at(60, 17) < 0.02, "the same above: {} {}", at(60, 19), at(60, 17));
    }

    #[test]
    fn an_offset_moves_the_shadow_down() {
        let s = WindowShadow { parts: [ShadowPart { blur: 3.0, opacity: 0.6, offset_y: 6.0, spread: 0.0 }, ShadowPart::NONE], rim: InnerRim::NONE };
        let (w, h) = (120, 120);
        let a = shadow_alpha(&s, (30.0, 30.0, 60.0, 60.0), 0.0, w, h, 1.0);
        let (above, below) = (a[25 * w + 60], a[94 * w + 60]);
        assert!(below > 3.0 * above, "below {below}, above {above}");
    }

    /// The fitted shadows reach where the probe measured them (2/255, points past the middle
    /// of each side): inactive 15.5 left, 8.5 above, 23 below; active 45, 24.5, 62; within
    /// three points (the measured tail flickers between 1 and 3 there).
    #[test]
    fn the_fitted_shadows_reach_as_measured() {
        let reach = |s: &WindowShadow| {
            let img = render(s, DEFAULT_CORNER, 2.0);
            let size = (800.0, 532.0);
            let past = |f: &dyn Fn(f64) -> (f64, f64)| (0..400).map(|i| i as f64 * 0.5 + 0.25).take_while(|d| { let (x, y) = f(*d); nine_part_at(&img, size, x, y) >= 2.0 / 255.0 }).last().unwrap_or(0.0) + 0.25;
            (past(&|d| (-d, 266.0)), past(&|d| (400.0, -d)), past(&|d| (400.0, 532.0 + d)))
        };
        let close = |got: (f64, f64, f64), want: (f64, f64, f64)| (got.0 - want.0).abs() <= 3.0 && (got.1 - want.1).abs() <= 3.0 && (got.2 - want.2).abs() <= 3.0;
        let inactive = reach(&WindowShadow::INACTIVE);
        assert!(close(inactive, (15.5, 8.5, 23.0)), "inactive {inactive:?}");
        let active = reach(&WindowShadow::ACTIVE);
        assert!(close(active, (45.0, 24.5, 62.0)), "active {active:?}");
    }

    /// Stretched to any size, the nine-part image shows the shadow drawn at that size within
    /// an alpha level, everywhere outside the window.
    #[test]
    fn the_nine_part_image_stretches_to_any_size() {
        let s = WindowShadow {
            parts: [ShadowPart { blur: 0.5, opacity: 0.3, offset_y: 0.0, spread: 0.5 }, ShadowPart { blur: 3.0, opacity: 0.4, offset_y: 3.0, spread: 0.0 }],
            rim: InnerRim::NONE,
        };
        let corner = 6.0;
        let img = render(&s, corner, 1.0);
        let (ww, wh) = (90.0, 70.0);
        let e = img.extent;
        let (w, h) = ((ww + 2.0 * e) as usize, (wh + 2.0 * e) as usize);
        let direct = shadow_alpha(&s, (e, e, ww, wh), corner, w, h, 1.0);
        let mut worst = 0.0f64;
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f64 + 0.5 - e, y as f64 + 0.5 - e);
                if px > 0.0 && px < ww && py > 0.0 && py < wh {
                    continue;
                }
                worst = worst.max((nine_part_at(&img, (ww, wh), px, py) - f64::from(direct[y * w + x])).abs());
            }
        }
        assert!(worst <= 1.5 / 255.0, "worst {}", worst * 255.0);
    }

    /// The dark shadows reach where the probe measured them (11-01, the system in the dark
    /// appearance): inactive 17.5 left, 10 above, 25.5 below; active 46, 29, 63.5; and they
    /// are darker than the light ones just past the hairline.
    #[test]
    fn the_dark_shadows_reach_as_measured() {
        let reach = |s: &WindowShadow| {
            let img = render(s, DEFAULT_CORNER, 2.0);
            let size = (800.0, 532.0);
            let past = |f: &dyn Fn(f64) -> (f64, f64)| (0..400).map(|i| i as f64 * 0.5 + 0.25).take_while(|d| { let (x, y) = f(*d); nine_part_at(&img, size, x, y) >= 2.0 / 255.0 }).last().unwrap_or(0.0) + 0.25;
            (past(&|d| (-d, 266.0)), past(&|d| (400.0, -d)), past(&|d| (400.0, 532.0 + d)))
        };
        let close = |got: (f64, f64, f64), want: (f64, f64, f64)| (got.0 - want.0).abs() <= 3.0 && (got.1 - want.1).abs() <= 3.0 && (got.2 - want.2).abs() <= 3.0;
        let inactive = reach(&WindowShadow::INACTIVE_DARK);
        assert!(close(inactive, (17.5, 10.0, 25.5)), "inactive {inactive:?}");
        let active = reach(&WindowShadow::ACTIVE_DARK);
        assert!(close(active, (46.0, 29.0, 63.5)), "active {active:?}");
        // Measured at 2x: the hairline (the first pixel out) and the drop (the next) left of
        // the middle of the left side.
        for (s, hairline, drop) in [(WindowShadow::ACTIVE_DARK, 243.0, 90.0), (WindowShadow::INACTIVE_DARK, 221.0, 58.0)] {
            let img = render(&s, DEFAULT_CORNER, 2.0);
            let at = |d: f64| nine_part_at(&img, (800.0, 532.0), -d, 266.0) * 255.0;
            assert!((at(0.25) - hairline).abs() <= 4.0, "hairline {} vs {hairline}", at(0.25));
            assert!((at(0.75) - drop).abs() <= 4.0, "drop {} vs {drop}", at(0.75));
        }
    }

    /// The shadow is picked by the window's appearance and focus; a custom one has no rim.
    #[test]
    fn the_shadow_follows_appearance_and_focus() {
        assert_eq!(WindowShadow::of_window(true, false), WindowShadow::ACTIVE);
        assert_eq!(WindowShadow::of_window(false, false), WindowShadow::INACTIVE);
        assert_eq!(WindowShadow::of_window(true, true), WindowShadow::ACTIVE_DARK);
        assert_eq!(WindowShadow::of_window(false, true), WindowShadow::INACTIVE_DARK);
        let (light, dark) = (WindowShadow::ACTIVE.parts[1], WindowShadow::ACTIVE_DARK.parts[1]);
        assert!(dark.opacity > 1.5 * light.opacity && (dark.blur - light.blur).abs() < 1.0, "the dark drop: as wide, darker");
        let rim = |key: bool, dark: bool| WindowShadow::of_window(key, dark).rim;
        assert!(rim(true, true).width > rim(true, false).width, "the dark rim is a point, the light one half");
        assert!(rim(false, true).white < rim(true, true).white, "an inactive window's dark rim is fainter");
        assert_eq!(rim(false, false), rim(true, false), "the light rim is the same on both");
        assert!(WindowShadow::custom(24.0, 0.35, 10.0).rim.is_none() && WindowShadow::NONE.rim.is_none());
    }

    /// The rim keeps to the picture: scaled with the plane, the window's own size at the
    /// window, a card's fraction of it on a card, never negative.
    #[test]
    fn the_rim_scales_with_the_plane() {
        assert_eq!(rim_scale((800.0, 500.0), (800.0, 500.0)), (1.0, 1.0));
        assert_eq!(rim_scale((200.0, 100.0), (800.0, 500.0)), (0.25, 0.2));
        assert_eq!(rim_scale((200.0, 100.0), (0.0, 500.0)), (1.0, 0.2), "no window size: as laid out");
        assert_eq!(rim_scale((-5.0, 100.0), (800.0, 500.0)).0, 0.0);
    }

    #[test]
    fn the_extent_covers_the_widest_part() {
        assert_eq!(WindowShadow::NONE.extent(), 1.0);
        let s = WindowShadow { parts: [ShadowPart { blur: 10.0, opacity: 0.4, offset_y: 6.0, spread: 1.0 }, ShadowPart::NONE], rim: InnerRim::NONE };
        assert_eq!(s.extent(), 43.0);
        assert_eq!(WindowShadow::custom(24.0, 0.35, 10.0).parts[0], ShadowPart { blur: 12.0, opacity: 0.35, offset_y: 10.0, spread: 0.0 });
        assert_eq!(WindowShadow::INACTIVE.scaled(0.0).parts[1].opacity, 0.0);
    }

    /// Card to window: the amount rides the plane's own spring, so it is the card's shadow
    /// alone where the motion starts, the window's alone where it settles, and in between it
    /// never jumps by more than the plane moves in a frame.
    #[test]
    fn the_amount_follows_the_motion() {
        use crate::ui::motion_math::{Spring, settle_time, spring_progress};
        let s = Spring::APPLE;
        let at = |t: f64| amount_at(0.0, 1.0, spring_progress(s, t));
        assert_eq!(at(0.0), 0.0);
        assert!(at(settle_time(s, 0.002)) > 0.998);
        let frame = 1.0 / 120.0;
        let (mut t, mut last) = (0.0, 0.0);
        while t < 1.0 {
            let a = at(t);
            let moved = spring_progress(s, t) - spring_progress(s, (t - frame).max(0.0));
            assert!(a >= last - 1e-12 && a - last <= moved + 1e-12, "at {t}: {last} to {a}");
            last = a;
            t += frame;
        }
        assert_eq!(amount_at(1.0, 0.0, 1.0), 0.0, "outgoing: the card's alone at the card");
        assert_eq!(amount_at(0.0, 1.0, 1.02), 1.0, "an overshoot never shows more than the window's");
    }

    /// The fade is continuous, whole before it begins and gone after it ends, so neither the
    /// unveil nor the landing after it changes the shadow in one frame.
    #[test]
    fn the_handover_fade_never_steps() {
        let (begin, duration) = handover(0.1, 0.06);
        assert!((begin - 0.07).abs() < 1e-12 && (duration - 0.06).abs() < 1e-12);
        assert_eq!(fade_out(0.0, begin, duration), 1.0);
        assert_eq!(fade_out(begin, begin, duration), 1.0);
        assert!((fade_out(0.1, begin, duration) - 0.5).abs() < 1e-9, "halfway at the expected moment");
        assert_eq!(fade_out(begin + duration, begin, duration), 0.0);
        let frame = 1.0 / 120.0;
        let (mut last, mut t) = (1.0, 0.0);
        while t < 0.3 {
            let f = fade_out(t, begin, duration);
            assert!(f <= last + 1e-12 && last - f <= frame / duration + 1e-9, "at {t}: {last} to {f}");
            last = f;
            t += frame;
        }
        assert_eq!(handover(0.02, 0.06), (0.0, 0.05), "never before now: it ends as late");
        assert_eq!(handover(0.0, 0.0), (0.0, 0.0));
    }

    /// Centred on the expected moment, the error is at most half a step when the window shows
    /// as expected (a darker `a (1 - a)` with the snapshot's shadow left on, a lighter `a` with
    /// it gone first), and three quarters of one when it shows within the middle half of the
    /// spread.
    #[test]
    fn the_handover_error_is_smallest_centred() {
        let a = 0.3;
        let (begin, duration) = handover(0.1, 0.06);
        let worst_for = |shown: f64| {
            let (mut worst, mut t) = (0.0f64, 0.0);
            while t < 0.3 {
                worst = worst.max(handover_error(a, t, shown, begin, duration).abs());
                t += 0.0005;
            }
            worst
        };
        assert!(worst_for(0.1) <= a / 2.0 + 1e-9, "{}", worst_for(0.1));
        let worst = (85..=115).map(|ms| worst_for(ms as f64 / 1000.0)).fold(0.0, f64::max);
        assert!(worst <= 0.75 * a + 1e-9, "worst {worst}");
        assert!(worst_for(0.0) >= a * (1.0 - a) - 1e-9, "shown before the fade: the whole darker step");
        assert_eq!(handover_error(a, 0.0, 0.1, begin, duration), 0.0);
        assert!(handover_error(a, 0.25, 0.1, begin, duration).abs() < 1e-12);
    }
}
