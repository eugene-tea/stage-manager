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
//! - **The shape.** A window's corner is close to Core Animation's continuous corner (Apple's
//!   "squircle", reaching 1.53 radii along each edge), 16 pt on a titled window; a circular
//!   arc of the same radius sits up to half a point off it. The curve macOS's shadow follows
//!   has a longer tail than Core Animation's: up to a seventh of a point further in between
//!   0.84 and 1.44 radii along the edge ([`window_inset`], fitted 11-01 to the dark hairline,
//!   which is nearly opaque and shows every tenth of a point).
//! - **The hairline** ([`Hairline`]) is drawn as a band just outside that curve, not as a
//!   grown, filled shape: its alpha at a pixel is the band's profile across the edge at the
//!   pixel's centre, softened by a Gaussian that is near nothing where the edge runs along
//!   the pixel grid (the sides: one sharp pixel) and widest where it runs at 45 degrees, the
//!   band a little wider there too (fitted to the corners as macOS draws them: within about
//!   15 alpha levels in the dark appearance, against 126 for a filled shape grown by a
//!   spread). The inner rim follows the same curve and softening ([`render_rim`]).
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

/// macOS's hairline around a window: a band from `s_in` to `s_out` points out from the
/// window's edge (its curve: [`window_inset`]), `s_out45` wider where the edge runs at 45
/// degrees, at `opacity`, its two sides softened by a Gaussian of `aa` points where the edge
/// runs along the pixel grid, up to `aa45` more at 45 degrees (combined as the root of the
/// sum of squares); the angle weight is `|sin 2θ|` to the power [`LINE_ANGLE_POWER`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hairline {
    pub s_in: f64,
    pub s_out: f64,
    pub s_out45: f64,
    pub aa: f64,
    pub aa45: f64,
    pub opacity: f64,
}

impl Hairline {
    /// No hairline.
    pub const NONE: Hairline = Hairline { s_in: 0.0, s_out: 0.0, s_out45: 0.0, aa: 0.0, aa45: 0.0, opacity: 0.0 };

    /// How far past the edge it can reach (points).
    pub fn reach(&self) -> f64 {
        if self.opacity <= 0.0 { 0.0 } else { self.s_out + self.s_out45 + 4.0 * (self.aa + self.aa45) }
    }
}

/// The power of `|sin 2θ|` that weighs the hairline's softening and widening by the edge's
/// angle (fitted: it is near its full 45-degree amount a few degrees off the grid already).
pub const LINE_ANGLE_POWER: f64 = 0.30;

/// The softening of the inner rim's sides where the edge runs along the pixel grid (points):
/// sharp there (its pixels read exactly on the sides), its hairline's `aa45` at 45 degrees.
pub const RIM_AA: f64 = 0.02;

/// A window shadow: its hairline and parts, laid over one another (source over), under the
/// window, and the inner rim over it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowShadow {
    pub line: Hairline,
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
        line: Hairline { s_in: 0.074, s_out: 0.188, s_out45: 0.027, aa: 0.171, aa45: 0.202, opacity: 1.0 },
        parts: [ShadowPart::NONE, ShadowPart { blur: 19.25, opacity: 0.395, offset_y: 17.9, spread: 0.65 }],
        rim: InnerRim { width: 0.5, white: 0.10 },
    };
    /// An inactive window's shadow in the light appearance, fitted to the owner's Mac
    /// (11-01): at 2/255 it reaches 15.5 pt left and right, 8.5 above and 23 below the middle
    /// of each side; the inner rim as the key window's.
    pub const INACTIVE: WindowShadow = WindowShadow {
        line: Hairline { s_in: 0.067, s_out: 0.180, s_out45: 0.021, aa: 0.187, aa45: 0.196, opacity: 1.0 },
        parts: [ShadowPart::NONE, ShadowPart { blur: 7.8, opacity: 0.215, offset_y: 7.0, spread: 0.2 }],
        rim: InnerRim { width: 0.5, white: 0.10 },
    };
    /// The key window's shadow in the dark appearance, fitted to the owner's Mac (11-01): at
    /// 2/255 it reaches 46 pt left and right, 29 above and 63.5 below the middle of each
    /// side; the hairline 0.95 opaque, the drop 0.35 just past it (light: 0.37, 0.22); the
    /// inner rim a point wide, white at a tenth.
    pub const ACTIVE_DARK: WindowShadow = WindowShadow {
        line: Hairline { s_in: -0.030, s_out: 0.354, s_out45: 0.106, aa: 0.075, aa45: 0.228, opacity: 1.0 },
        parts: [ShadowPart::NONE, ShadowPart { blur: 19.25, opacity: 0.68, offset_y: 17.85, spread: 0.95 }],
        rim: InnerRim { width: 1.0, white: 0.10 },
    };
    /// An inactive window's shadow in the dark appearance, fitted to the owner's Mac (11-01):
    /// at 2/255 it reaches 17.5 pt left and right, 10 above and 25.5 below the middle of each
    /// side; the hairline 0.87 opaque, the drop 0.23 just past it (light: 0.29, 0.12); the
    /// inner rim a point wide, white at 0.08.
    pub const INACTIVE_DARK: WindowShadow = WindowShadow {
        line: Hairline { s_in: -0.030, s_out: 0.354, s_out45: 0.106, aa: 0.075, aa45: 0.228, opacity: 0.891 },
        parts: [ShadowPart::NONE, ShadowPart { blur: 7.37, opacity: 0.425, offset_y: 6.8, spread: 0.9 }],
        rim: InnerRim { width: 1.0, white: 0.08 },
    };

    /// No shadow.
    pub const NONE: WindowShadow = WindowShadow { line: Hairline::NONE, parts: [ShadowPart::NONE; 2], rim: InnerRim::NONE };

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
            line: Hairline::NONE,
            parts: [ShadowPart { blur: radius.max(0.0) / 2.0, opacity: opacity.clamp(0.0, 1.0), offset_y, spread: 0.0 }, ShadowPart::NONE],
            rim: InnerRim::NONE,
        }
    }

    /// How far past the window (points, every side) the shadow can reach: 3.5 sigmas of the
    /// widest part plus its offset and spread, in whole points, plus one.
    pub fn extent(&self) -> f64 {
        self.parts.iter().filter(|p| p.opacity > 0.0).map(|p| 3.5 * p.blur + p.offset_y.abs() + p.spread.max(0.0)).fold(self.line.reach(), f64::max).ceil() + 1.0
    }

    /// The same shadow with every part's opacity times `k`.
    pub fn scaled(&self, k: f64) -> WindowShadow {
        let mut s = *self;
        for p in &mut s.parts {
            p.opacity = (p.opacity * k).clamp(0.0, 1.0);
        }
        s.line.opacity = (s.line.opacity * k).clamp(0.0, 1.0);
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

/// The tail macOS's window corner has past Core Animation's continuous one: up to `.0` radii
/// further in, between `.1` and `.2` radii along the edge, as `sin²` (fitted 11-01).
pub const CORNER_TAIL: (f64, f64, f64) = (0.0039, 0.836, 1.439);

/// How far in from the side macOS's window corner of radius `r` is at `dy` from the top or
/// bottom edge: [`corner_inset`] and its [`CORNER_TAIL`].
pub fn window_inset(r: f64, dy: f64) -> f64 {
    let base = corner_inset(r, dy);
    if r <= 0.0 {
        return base;
    }
    let (a, u0, u1) = CORNER_TAIL;
    let u = dy / r;
    if u <= u0 || u >= u1 {
        return base;
    }
    base + a * r * (std::f64::consts::PI * (u - u0) / (u1 - u0)).sin().powi(2)
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
    let inset = window_inset(r, (y - y0).min(y1 - y));
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

/// A band along a window's edge: from `s_in` to `s_out` points out (negative: in), `s_out45`
/// wider at 45 degrees, softened by `aa` points on the grid and `aa45` more at 45 degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub s_in: f64,
    pub s_out: f64,
    pub s_out45: f64,
    pub aa: f64,
    pub aa45: f64,
}

impl Band {
    /// The band's coverage (0-1) at signed distance `sd` (points, positive outside) from the
    /// edge, where the edge's angle weight is `s2` (`|sin 2θ|`).
    pub fn at(&self, sd: f64, s2: f64) -> f64 {
        let k = s2.clamp(0.0, 1.0).powf(LINE_ANGLE_POWER);
        let sigma = (self.aa * self.aa + (self.aa45 * k).powi(2)).sqrt().max(1e-4);
        let out = self.s_out + self.s_out45 * k;
        (phi((out - sd) / sigma) - phi((self.s_in - sd) / sigma)).clamp(0.0, 1.0)
    }
}

/// The corner curve of radius `r` as points `(u, v)` from the frame's corner (inward along
/// each edge), from the diagonal out along both branches, a fiftieth of a point apart.
fn corner_points(r: f64) -> Vec<(f64, f64)> {
    let reach = CORNER_REACH * r;
    // Where the curve meets the diagonal.
    let (mut lo, mut hi) = (0.0, reach);
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if window_inset(r, mid) > mid { lo = mid } else { hi = mid }
    }
    let n = ((reach - lo) / 0.02).ceil().max(1.0) as usize;
    let mut out = Vec::with_capacity(2 * n + 2);
    for i in 0..=n {
        let v = lo + (reach - lo) * i as f64 / n as f64;
        let u = window_inset(r, v);
        out.push((u, v));
        out.push((v, u));
    }
    out
}

/// The signed distance (points, positive outside) from corner-local `(u, v)` (inward from
/// the frame's corner along each edge) to the window's edge with corners of radius `r`
/// (`points`: [`corner_points`]), and the edge's angle weight `|sin 2θ|` there.
fn corner_distance(r: f64, points: &[(f64, f64)], u: f64, v: f64) -> (f64, f64) {
    let reach = CORNER_REACH * r;
    let inside = u >= 0.0 && v >= 0.0 && u >= window_inset(r, v) && v >= window_inset(r, u);
    if u > reach || v > reach || r <= 0.0 {
        // Along a straight side.
        let d = if inside { -u.min(v) } else { (-u).max(-v) };
        return (d, 0.0);
    }
    let (mut best, mut at) = (f64::INFINITY, (0.0, 0.0));
    for &(pu, pv) in points {
        let d = (u - pu).powi(2) + (v - pv).powi(2);
        if d < best {
            best = d;
            at = (pu, pv);
        }
    }
    // The straight sides past the curve's ends.
    for (d, q) in [((u * u + (reach - v).max(0.0).powi(2)), (0.0, v.max(reach))), (((reach - u).max(0.0).powi(2) + v * v), (u.max(reach), 0.0))] {
        if d < best {
            best = d;
            at = q;
        }
    }
    let dist = best.sqrt();
    let s2 = if dist > 1e-6 { (2.0 * (u - at.0) * (v - at.1)).abs() / (dist * dist) } else { 0.0 };
    (if inside { -dist } else { dist }, s2)
}

/// `band`'s coverage (0-1) along the edge of the window at `frame` (points) with corners of
/// radius `corner`, over a `w` x `h` pixel grid at `scale`, at each pixel's centre.
pub fn band_alpha(band: &Band, frame: (f64, f64, f64, f64), corner: f64, w: usize, h: usize, scale: f64) -> Vec<f32> {
    let (fx, fy, fw, fh) = frame;
    let r = corner.min(fw / (2.0 * CORNER_REACH)).min(fh / (2.0 * CORNER_REACH)).max(0.0);
    let points = corner_points(r);
    let reach = CORNER_REACH * r;
    let far = band.s_out.max(0.0) + band.s_out45.max(0.0) + 5.0 * (band.aa + band.aa45) + 1.0;
    let near = (-band.s_in).max(0.0) + 5.0 * (band.aa + band.aa45) + 1.0;
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let py = (y as f64 + 0.5) / scale;
        let v = (py - fy).min(fy + fh - py);
        for x in 0..w {
            let px = (x as f64 + 0.5) / scale;
            let u = (px - fx).min(fx + fw - px);
            // Only near the edge: within the band's reach of the frame's sides, or of the
            // curve in a corner.
            let in_box = u <= reach && v <= reach;
            if !in_box {
                let d = if u >= 0.0 && v >= 0.0 { -u.min(v) } else { (-u).max(-v) };
                if d > far || d < -near {
                    continue;
                }
            } else {
                let (hu, hv) = (u - window_inset(r, v.max(0.0)), v - window_inset(r, u.max(0.0)));
                if hu.abs().min(hv.abs()) > 1.5 * far.max(near) && (hu > 0.0) == (hv > 0.0) {
                    continue;
                }
            }
            let (sd, s2) = corner_distance(r, &points, u, v);
            out[y * w + x] = band.at(sd, s2) as f32;
        }
    }
    out
}

/// The shadow's alpha (0-1) over a `w` x `h` pixel grid, the window at `frame` (points).
pub fn shadow_alpha(shadow: &WindowShadow, frame: (f64, f64, f64, f64), corner: f64, w: usize, h: usize, scale: f64) -> Vec<f32> {
    let mut clear = vec![1f32; w * h];
    let l = &shadow.line;
    if l.opacity > 0.0 {
        let band = Band { s_in: l.s_in, s_out: l.s_out, s_out45: l.s_out45, aa: l.aa, aa45: l.aa45 };
        let a = band_alpha(&band, frame, corner, w, h, scale);
        for (c, v) in clear.iter_mut().zip(a) {
            *c *= 1.0 - (l.opacity as f32 * v).clamp(0.0, 1.0);
        }
    }
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

/// Draws `shadow`'s inner rim over a window with corners of radius `corner`, at `scale`
/// pixels a point, as a nine-part image of the window itself (no extent): premultiplied
/// white, a band `width` points in from the edge on macOS's curve, softened as the
/// hairline is at 45 degrees ([`RIM_AA`] on the sides), at the rim's `white`.
pub fn render_rim(shadow: &WindowShadow, corner: f64, scale: f64) -> NinePart {
    let rim = &shadow.rim;
    let core = 2.0 * (CORNER_REACH * corner.max(0.0) + 2.0).ceil() + 1.0;
    let n = (core * scale).round() as usize;
    let band = Band { s_in: -rim.width, s_out: 0.0, s_out45: 0.0, aa: RIM_AA, aa45: shadow.line.aa45 };
    let alpha = band_alpha(&band, (0.0, 0.0, core, core), corner, n, n, scale);
    let mut bytes = Vec::with_capacity(n * n * 4);
    for a in alpha {
        let v = (f64::from(a) * rim.white * 255.0).round().clamp(0.0, 255.0) as u8;
        bytes.extend_from_slice(&[v, v, v, v]);
    }
    let mid = (core / 2.0 - 0.5) / core;
    let one = 1.0 / core;
    NinePart { bytes, w: n, h: n, scale, extent: 0.0, slice: [mid, mid, one, one] }
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

/// How far in along the diagonal (in radii of the curve its shadow follows) a window's own
/// picture is clear, counting partly clear pixels by how clear they are: 0.3008, measured
/// 11-01 (a titled window's picture 4.81 pt, its shadow's curve 15.98 pt; the picture's own
/// edge sits a little inside the curve, and reads 17.2 pt by [`corner_from_inset`]).
pub const PICTURE_DIAGONAL: f64 = 0.3008;

/// A window's corner radius (the curve its shadow follows) from the alpha of its picture's
/// pixels along the diagonal from a corner inward (`alphas`, 0-255), `scale` pixels a point:
/// the clear part summed to the first opaque pixel, over [`PICTURE_DIAGONAL`]. `None` when
/// no pixel along it is opaque. Bounded to 0..=40 pt.
pub fn corner_from_diagonal(alphas: &[u8], scale: f64) -> Option<f64> {
    let end = alphas.iter().position(|a| *a == 255)?;
    let clear: f64 = alphas[..end].iter().map(|a| 1.0 - f64::from(*a) / 255.0).sum();
    Some((clear / scale.max(1e-6) / PICTURE_DIAGONAL).clamp(0.0, 40.0))
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
        let s = WindowShadow { line: Hairline::NONE, parts: [ShadowPart { blur: 4.0, opacity: 0.5, offset_y: 0.0, spread: 0.0 }, ShadowPart::NONE], rim: InnerRim::NONE };
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
        let s = WindowShadow { line: Hairline::NONE, parts: [ShadowPart { blur: 0.25, opacity: 0.3, offset_y: 0.0, spread: 0.5 }, ShadowPart::NONE], rim: InnerRim::NONE };
        let (w, h) = (120, 120);
        let a = shadow_alpha(&s, (10.0, 10.0, 40.0, 40.0), 4.0, w, h, 2.0);
        let at = |x: usize, y: usize| f64::from(a[y * w + x]);
        assert!(at(19, 60) > 0.15, "the pixel just outside the left edge: {}", at(19, 60));
        assert!(at(17, 60) < 0.02, "two pixels out: {}", at(17, 60));
        assert!(at(60, 19) > 0.15 && at(60, 17) < 0.02, "the same above: {} {}", at(60, 19), at(60, 17));
    }

    #[test]
    fn an_offset_moves_the_shadow_down() {
        let s = WindowShadow { line: Hairline::NONE, parts: [ShadowPart { blur: 3.0, opacity: 0.6, offset_y: 6.0, spread: 0.0 }, ShadowPart::NONE], rim: InnerRim::NONE };
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
            line: Hairline::NONE,
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

    /// macOS's corner is the continuous one but for its longer tail: the same near the
    /// corner and past the reach, a little further in between (a seventh of a point at most
    /// on a titled window).
    #[test]
    fn the_window_corner_has_a_longer_tail() {
        for dy in [0.0, 4.0, 10.0, 13.0, 23.5, 24.5, 30.0] {
            assert!((window_inset(16.0, dy) - corner_inset(16.0, dy)).abs() < 1e-9, "at {dy}");
        }
        let extra = (140..230).map(|i| window_inset(16.0, i as f64 / 10.0) - corner_inset(16.0, i as f64 / 10.0)).fold(0.0, f64::max);
        assert!(extra > 0.05 && extra <= 0.0625 + 1e-9, "{extra}");
        let mut last = f64::INFINITY;
        for i in 0..250 {
            let v = window_inset(16.0, i as f64 / 10.0);
            assert!(v <= last + 1e-9, "the curve only comes closer to the side, at {}", i as f64 / 10.0);
            last = v;
        }
    }

    /// The corner as a snapshot reads it: a titled window's picture is clear for 9 pixels and
    /// 61 hundredths of the next along the diagonal at 2x (11-01): the 15.98 pt curve its
    /// shadow follows.
    #[test]
    fn the_corner_reads_from_the_clear_diagonal() {
        let mut diagonal = vec![0u8; 9];
        diagonal.extend_from_slice(&[99, 255, 255]);
        let r = corner_from_diagonal(&diagonal, 2.0).unwrap();
        assert!((r - 15.98).abs() < 0.03, "{r}");
        assert_eq!(corner_from_diagonal(&[0, 0, 128], 2.0), None, "never opaque: unread");
        assert_eq!(corner_from_diagonal(&[255], 2.0), Some(0.0));
    }

    /// The dark hairline on a side is one sharp pixel at 2x (0.915 of its opacity, under 3% in
    /// the next); along the corner's 45 degrees it is softer and wider, its peak lower.
    #[test]
    fn the_hairline_is_sharp_on_the_sides_and_soft_at_45_degrees() {
        let l = WindowShadow::ACTIVE_DARK.line;
        let band = Band { s_in: l.s_in, s_out: l.s_out, s_out45: l.s_out45, aa: l.aa, aa45: l.aa45 };
        assert!((band.at(0.25, 0.0) - 0.915).abs() < 0.01, "{}", band.at(0.25, 0.0));
        assert!(band.at(0.75, 0.0) < 0.03 && band.at(-0.25, 0.0) < 0.03);
        let peak45 = (0..100).map(|i| band.at(i as f64 / 100.0 - 0.2, 1.0)).fold(0.0, f64::max);
        let width = |s2: f64| (0..400).map(|i| band.at(i as f64 / 200.0 - 0.5, s2) / 200.0).sum::<f64>();
        assert!(peak45 < 0.97 && width(1.0) > width(0.0) * 1.2, "peak {peak45}, width {} vs {}", width(1.0), width(0.0));
        // On the image: the first pixel left of a side's middle, as measured (243 with the
        // drop under it), the next the drop's.
        let img = render(&WindowShadow::ACTIVE_DARK, DEFAULT_CORNER, 2.0);
        let at = |d: f64| nine_part_at(&img, (800.0, 532.0), -d, 266.0) * 255.0;
        assert!((at(0.25) - 243.0).abs() <= 4.0, "{}", at(0.25));
    }

    /// The inner rim, drawn on its own: a point of white at a tenth inside a dark window's
    /// side (two pixels at 2x), nothing past the edge or further in, and none in the corner's
    /// clear part.
    #[test]
    fn the_inner_rim_is_its_width_inside_the_edge() {
        let img = render_rim(&WindowShadow::ACTIVE_DARK, DEFAULT_CORNER, 2.0);
        assert_eq!(img.extent, 0.0);
        let side = img.w as f64 / img.scale;
        let at = |x: f64, y: f64| nine_part_at(&img, (side, side), x, y) * 255.0;
        let mid = side / 2.0;
        assert!((at(0.25, mid) - 25.5).abs() <= 1.5 && (at(0.75, mid) - 25.5).abs() <= 1.5, "{} {}", at(0.25, mid), at(0.75, mid));
        assert!(at(1.25, mid) < 1.5, "{}", at(1.25, mid));
        assert!(at(0.25, 0.25) < 0.5, "the corner's clear part");
        let light = render_rim(&WindowShadow::ACTIVE, DEFAULT_CORNER, 2.0);
        let at = |x: f64| nine_part_at(&light, (side, side), x, mid) * 255.0;
        assert!((at(0.25) - 25.5).abs() <= 1.5 && at(0.75) < 1.5, "light: half a point, {} {}", at(0.25), at(0.75));
    }

    #[test]
    fn the_extent_covers_the_widest_part() {
        assert_eq!(WindowShadow::NONE.extent(), 1.0);
        let s = WindowShadow { line: Hairline::NONE, parts: [ShadowPart { blur: 10.0, opacity: 0.4, offset_y: 6.0, spread: 1.0 }, ShadowPart::NONE], rim: InnerRim::NONE };
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
