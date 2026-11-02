//! Which appearance macOS draws a window's shadow in, read from the shadow itself, pure
//! (round window-appearance): the classification of a sampled shadow and the per-window
//! cache. Captured by [`crate::platform::window_look`], used by the snapshots' shadows
//! ([`crate::ui::window_shadow`]).
//!
//! - **Why the window's own.** macOS draws a window's shadow by the window's appearance, which
//!   follows the system's unless the app sets one: a per-app override, an app that forces
//!   the light or the dark one, or Messages, which draws its whole window dark on a light
//!   system when the conversation shown has a dark chat background (11-02: white sidebar
//!   text on a purple background, its shadow the dark model's). No setting reads all of
//!   those; the shadow does, as it is what a snapshot must match.
//! - **The evidence.** A small capture of the window with its shadow (ScreenCaptureKit, the
//!   shadow not ignored) over a clear background: the shadow is the alpha past the window.
//!   ScreenCaptureKit fits the window and its shadow into the size asked (measured 11-02 on
//!   the probe's window: 800 x 532 pt asked at 1600 x 1064 px came back with the window
//!   1470 px wide, 1.84 px a point), so the window is found in the picture by its opaque
//!   middle row and column ([`profiles`]) and the scale read from it.
//! - **The classification** ([`classify`]): the alpha past the middle of the left, right and
//!   bottom sides against the model's four shadows (light or dark, key or inactive,
//!   [`WindowShadow`]), each averaged over the pixel's span and fitted over the edge's
//!   sub-pixel place. The dark drop is nearly twice the light one at the same reach, the key
//!   one about two and a half times as wide as an inactive one, so the four are far apart
//!   (measured 11-02 at 0.46 px a point, an inactive window 4 to 6 pt below its bottom
//!   edge: light 36, dark 71 out of 255). Taken only when the best fit is close and the
//!   other appearance's best clearly worse; else there is no evidence.
//! - **The cache** ([`LookCache`]): per window, the last reading, read again [`REFRESH`]
//!   after it while the window is captured, and at once after the system's appearance
//!   changes (a reading from before is not used).

use std::collections::HashMap;

use super::shadow_math::{Band, WindowShadow, phi};

/// The appearance a window's shadow is drawn in (`dark`), and whether it was a key window's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub dark: bool,
    pub key: bool,
}

impl Look {
    /// "dark" or "light", "active" or "inactive".
    pub fn words(&self) -> (&'static str, &'static str) {
        (if self.dark { "dark" } else { "light" }, if self.key { "active" } else { "inactive" })
    }
}

/// A side of the window, along whose middle the shadow is sampled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Bottom,
}

/// The shadow's alpha past the middle of one side: `alpha[0]` the first pixel past the last
/// opaque one of the window, outward; `px_per_pt` the picture's scale.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub side: Side,
    pub alpha: Vec<u8>,
    pub px_per_pt: f64,
}

/// A classification: the look that fits best, its error (RMS, 8-bit levels), and the best
/// error of the other appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub look: Look,
    pub rms: f64,
    pub other: f64,
}

/// The four shadows a window can have.
pub const MODELS: [(Look, WindowShadow); 4] = [
    (Look { dark: false, key: false }, WindowShadow::INACTIVE),
    (Look { dark: false, key: true }, WindowShadow::ACTIVE),
    (Look { dark: true, key: false }, WindowShadow::INACTIVE_DARK),
    (Look { dark: true, key: true }, WindowShadow::ACTIVE_DARK),
];

/// How far past the edge the profile is compared (points): the dark key drop's 2/255 reach
/// below is 63.5; past 40 every model is a few levels at most.
pub const MAX_DISTANCE: f64 = 40.0;
/// The largest error (RMS, 8-bit levels) of a fit taken as evidence (measured 11-02 as the
/// captures take them: 0.7 light, 0.9 dark, 2.0 a dark key window; no shadow at all fits
/// the light inactive one at 10).
pub const MAX_RMS: f64 = 6.0;
/// The other appearance's best error must be at least this many times the fit's (measured:
/// 8.3 against 0.7, 11.1 against 0.9, 24.1 against 2.0)...
pub const MARGIN: f64 = 2.5;
/// ...and at least this many levels more.
pub const MARGIN_LEVELS: f64 = 4.0;
/// The pixels past the last opaque one not compared.
pub const SKIP: usize = 2;
/// The fewest pixels compared.
pub const MIN_SAMPLES: usize = 6;
/// A window's opaque pixels (alpha at least this) are the window, not its shadow: the
/// dark hairline is 0.95 opaque at most (243 at 2x), and only along a pixel row.
pub const OPAQUE: u8 = 250;

/// The model's alpha (0-1) `d` points past the middle of `side` of a window large enough
/// that its other sides are past the blur (every titled window): the hairline, then each
/// part's Gaussian edge, laid over one another.
pub fn side_alpha(shadow: &WindowShadow, side: Side, d: f64) -> f64 {
    let l = &shadow.line;
    let line = if l.opacity > 0.0 {
        let band = Band { s_in: l.s_in, s_out: l.s_out, s_out45: l.s_out45, aa: l.aa, aa45: l.aa45 };
        (l.opacity * band.at(d, 0.0)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut clear = 1.0 - line;
    for p in shadow.parts.iter().filter(|p| p.opacity > 0.0) {
        let reach = match side {
            Side::Bottom => p.spread + p.offset_y,
            Side::Left | Side::Right => p.spread,
        };
        let a = if p.blur <= 1e-6 { f64::from(u8::from(d <= reach)) } else { phi((reach - d) / p.blur) };
        clear *= 1.0 - (p.opacity * a).clamp(0.0, 1.0);
    }
    1.0 - clear
}

/// The error (RMS, 8-bit levels) of `shadow` against `profiles`, at the edge's best
/// sub-pixel place, and how many pixels were compared. The first [`SKIP`] pixels out (the
/// window's edge, the hairline and the drop in one; a picture scaled down smears the edge
/// over two, measured 11-02: 229 then 118 past a dark key window at 0.41 px a point) are
/// left out.
fn error(shadow: &WindowShadow, profiles: &[Profile]) -> (f64, usize) {
    let mut best = (f64::INFINITY, 0);
    // Where the window's edge lies (pixels from the start of the first pixel out).
    for e in [-0.5, -0.25, 0.0, 0.25, 0.5, 0.75, 1.0] {
        let (mut sum, mut n) = (0.0, 0usize);
        for p in profiles {
            let k = p.px_per_pt.max(1e-3);
            for (i, &a) in p.alpha.iter().enumerate().skip(SKIP) {
                let (from, to) = ((i as f64 - e) / k, (i as f64 + 1.0 - e) / k);
                if from > MAX_DISTANCE {
                    break;
                }
                // The pixel's mean over its span (four points).
                let model: f64 = (0..4).map(|j| side_alpha(shadow, p.side, from + (to - from) * (j as f64 + 0.5) / 4.0)).sum::<f64>() / 4.0;
                sum += (f64::from(a) - 255.0 * model).powi(2);
                n += 1;
            }
        }
        if n > 0 {
            let rms = (sum / n as f64).sqrt();
            if rms < best.0 {
                best = (rms, n);
            }
        }
    }
    best
}

/// The look of the shadow sampled in `profiles`, when the evidence is clear (see the module
/// docs); `None` otherwise (too few pixels, no model close, or both appearances close).
pub fn classify(profiles: &[Profile]) -> Option<Fit> {
    let fits: Vec<(Look, f64, usize)> = MODELS.iter().map(|(look, s)| {
        let (rms, n) = error(s, profiles);
        (*look, rms, n)
    }).collect();
    let (look, rms, n) = fits.iter().copied().min_by(|a, b| a.1.total_cmp(&b.1))?;
    let other = fits.iter().filter(|f| f.0.dark != look.dark).map(|f| f.1).fold(f64::INFINITY, f64::min);
    let clear = n >= MIN_SAMPLES && rms <= MAX_RMS && other >= MARGIN * rms && other - rms >= MARGIN_LEVELS;
    clear.then_some(Fit { look, rms, other })
}

/// The profiles of a picture of a window with its shadow (`alpha`, `w` x `h`, top row first)
/// whose frame is `frame` (points): the window found as the opaque run through the middle
/// row and column, its scale read from it (both ways alike within 3 %, else `None`: the
/// middle is not opaque, or not the window's), then the alpha past the middle of the left,
/// right and bottom sides.
pub fn profiles(alpha: &[u8], w: usize, h: usize, frame: (f64, f64)) -> Option<Vec<Profile>> {
    if w < 8 || h < 8 || alpha.len() < w * h || frame.0 < 1.0 || frame.1 < 1.0 {
        return None;
    }
    let at = |x: usize, y: usize| alpha[y * w + x];
    let (cx, cy) = (w / 2, h / 2);
    if at(cx, cy) < OPAQUE {
        return None;
    }
    let run = |start: usize, len: usize, f: &dyn Fn(usize) -> u8| -> (usize, usize) {
        let (mut lo, mut hi) = (start, start);
        while lo > 0 && f(lo - 1) >= OPAQUE {
            lo -= 1;
        }
        while hi + 1 < len && f(hi + 1) >= OPAQUE {
            hi += 1;
        }
        (lo, hi)
    };
    let (left, right) = run(cx, w, &|x| at(x, cy));
    let (top, bottom) = run(cy, h, &|y| at(cx, y));
    let (kx, ky) = ((right - left + 1) as f64 / frame.0, (bottom - top + 1) as f64 / frame.1);
    if (kx - ky).abs() > 0.03 * kx.max(ky) {
        return None;
    }
    // The window touching the picture's edge: no shadow in it on that side.
    if left == 0 || right + 1 >= w || bottom + 1 >= h {
        return None;
    }
    Some(vec![
        Profile { side: Side::Left, alpha: (0..left).rev().map(|x| at(x, cy)).collect(), px_per_pt: kx },
        Profile { side: Side::Right, alpha: (right + 1..w).map(|x| at(x, cy)).collect(), px_per_pt: kx },
        Profile { side: Side::Bottom, alpha: (bottom + 1..h).map(|y| at(cx, y)).collect(), px_per_pt: ky },
    ])
}

/// The size (pixels) the evidence's picture is asked at for a window whose content is `w`
/// x `h` points: half a pixel a point, at most [`PROBE_SIDE`] on the longer side, so the
/// shadow's drop spans a few pixels at least (an inactive one's sigma is 7.4 pt).
pub fn probe_size(w: f64, h: f64) -> (usize, usize) {
    let k = PROBE_PX_PER_PT.min(PROBE_SIDE / w.max(h).max(1.0));
    (((w * k).round() as usize).max(16), ((h * k).round() as usize).max(16))
}

/// Pixels a point the evidence's picture is asked at.
pub const PROBE_PX_PER_PT: f64 = 0.5;
/// The longest side of the evidence's picture (pixels).
pub const PROBE_SIDE: f64 = 512.0;

/// A window's reading is taken again this long after the last one (seconds), while the
/// window is captured: an app may change its window's appearance on its own (Messages, a
/// conversation with another background).
pub const REFRESH: f64 = 20.0;

/// Windows remembered before the cache starts over.
const CAP: usize = 1024;

/// One window's last reading.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Entry {
    /// What it read (`None`: no clear evidence).
    look: Option<Look>,
    /// When (seconds, the caller's clock).
    at: f64,
    /// The system appearance's generation it was taken in.
    generation: u64,
}

/// Each window's look as its shadow last showed it.
#[derive(Debug, Clone, Default)]
pub struct LookCache {
    entries: HashMap<u32, Entry>,
    generation: u64,
}

impl LookCache {
    /// The system's appearance changed: every reading before it is stale (a window that
    /// follows the system has changed with it).
    pub fn appearance_changed(&mut self) {
        self.generation += 1;
    }

    /// The current generation (handed to [`Self::record`] with a reading taken now).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// True when window `id` is to be read at `now`: never read, read before the last
    /// appearance change, or [`REFRESH`] ago.
    pub fn due(&self, id: u32, now: f64) -> bool {
        self.entries.get(&id).is_none_or(|e| e.generation != self.generation || now - e.at >= REFRESH)
    }

    /// Records window `id`'s reading taken in `generation` at `now`; a reading from before
    /// the last appearance change is dropped. No evidence keeps the last look of this
    /// generation (it is not read again before [`REFRESH`]). True when the look changed.
    pub fn record(&mut self, id: u32, generation: u64, look: Option<Look>, now: f64) -> bool {
        if generation != self.generation {
            return false;
        }
        if self.entries.len() >= CAP && !self.entries.contains_key(&id) {
            self.entries.clear();
        }
        let before = self.entries.get(&id).filter(|e| e.generation == generation).and_then(|e| e.look);
        let kept = look.or(before);
        self.entries.insert(id, Entry { look: kept, at: now, generation });
        kept != before
    }

    /// Window `id`'s look and how long ago it was read (seconds), when read since the last
    /// appearance change.
    pub fn look(&self, id: u32, now: f64) -> Option<(Look, f64)> {
        let e = self.entries.get(&id).filter(|e| e.generation == self.generation)?;
        e.look.map(|l| (l, (now - e.at).max(0.0)))
    }

    /// Forgets window `id` (closed).
    pub fn forget(&mut self, id: u32) {
        self.entries.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile of `shadow` past `side` at `k` px a point, the edge `e` px into the first
    /// pixel, each pixel the model's mean over it, rounded.
    fn sampled(shadow: &WindowShadow, side: Side, k: f64, e: f64, n: usize) -> Profile {
        let alpha = (0..n)
            .map(|i| {
                let (from, to) = (((i as f64 - e) / k).max(0.0), (i as f64 + 1.0 - e) / k);
                let m: f64 = (0..16).map(|j| side_alpha(shadow, side, from + (to - from) * (j as f64 + 0.5) / 16.0)).sum::<f64>() / 16.0;
                (255.0 * m).round() as u8
            })
            .collect();
        Profile { side, alpha, px_per_pt: k }
    }

    fn all_sides(shadow: &WindowShadow, k: f64, e: f64) -> Vec<Profile> {
        [Side::Left, Side::Right, Side::Bottom].into_iter().map(|s| sampled(shadow, s, k, e, 40)).collect()
    }

    /// Each of the four shadows, sampled at the probe's scale and at a display's, is read as
    /// itself, wherever the edge falls in its pixel.
    #[test]
    fn each_shadow_reads_as_its_own_look() {
        for (look, shadow) in MODELS {
            for k in [0.46, 0.5, 1.84] {
                for e in [0.1, 0.4, 0.8] {
                    let fit = classify(&all_sides(&shadow, k, e)).unwrap_or_else(|| panic!("{look:?} at {k} px/pt, edge {e}: no evidence"));
                    assert_eq!(fit.look.dark, look.dark, "{look:?} at {k}, {e}: {fit:?}");
                }
            }
        }
    }

    /// Measured 11-02 (ScreenCaptureKit, shadows on, the probe's own inactive window, 0.46
    /// px a point): the bottom and left profiles in each appearance read as that appearance.
    #[test]
    fn the_measured_profiles_read_as_their_appearance() {
        let k = 244.0 / 532.0;
        let light = [
            Profile { side: Side::Bottom, alpha: vec![101, 29, 36, 28, 22, 16, 11, 7, 5, 3, 2, 1, 0, 0], px_per_pt: k },
            Profile { side: Side::Left, alpha: vec![120, 23, 16, 12, 7, 5, 3, 2, 1, 0, 0], px_per_pt: k },
        ];
        let dark = [
            Profile { side: Side::Bottom, alpha: vec![155, 70, 71, 56, 44, 33, 23, 15, 9, 5, 3, 2, 0, 0], px_per_pt: k },
            Profile { side: Side::Left, alpha: vec![158, 48, 33, 24, 15, 10, 6, 3, 1, 0, 0], px_per_pt: k },
        ];
        let l = classify(&light).expect("the light one is clear");
        assert!(!l.look.dark && !l.look.key, "{l:?}");
        let d = classify(&dark).expect("the dark one is clear");
        assert!(d.look.dark && !d.look.key, "{d:?}");
        // The probe's dark key panel, as the captures take it (0.41 px a point).
        let k = 0.41125;
        let dark_key = [
            Profile { side: Side::Left, alpha: vec![229, 118, 69, 69, 59, 51, 44, 38, 31, 26, 22, 17, 14, 11, 9, 7], px_per_pt: k },
            Profile { side: Side::Right, alpha: vec![229, 118, 69, 69, 59, 52, 44, 38, 32, 26, 22, 18, 14, 11, 9, 7], px_per_pt: k },
            Profile { side: Side::Bottom, alpha: vec![212, 133, 134, 126, 117, 110, 101, 93, 84, 76, 67, 59, 52, 44, 38, 31], px_per_pt: k },
        ];
        let dk = classify(&dark_key).expect("the dark key one is clear");
        assert!(dk.look.dark && dk.look.key, "{dk:?}");
    }

    /// No shadow, or one unlike any model, is no evidence.
    #[test]
    fn no_shadow_is_no_evidence() {
        let none: Vec<Profile> = [Side::Left, Side::Right, Side::Bottom].into_iter().map(|side| Profile { side, alpha: vec![0; 30], px_per_pt: 0.5 }).collect();
        assert_eq!(classify(&none), None);
        let flat: Vec<Profile> = [Side::Left, Side::Bottom].into_iter().map(|side| Profile { side, alpha: vec![200; 30], px_per_pt: 0.5 }).collect();
        assert_eq!(classify(&flat), None);
        assert_eq!(classify(&[]), None);
    }

    /// A picture: the window opaque at its place, the shadow past it; the profiles start at
    /// the first pixel out and carry the scale.
    #[test]
    fn the_window_is_found_in_its_picture() {
        let (w, h) = (60usize, 50usize);
        // The window 40 x 30 px at (8, 6): 80 x 60 pt at half a pixel a point.
        let alpha: Vec<u8> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if (8..48).contains(&x) && (6..36).contains(&y) { 255 } else if y >= 36 { 90 - (y - 36) as u8 } else { 40 }
            })
            .collect();
        let p = profiles(&alpha, w, h, (80.0, 60.0)).expect("found");
        assert_eq!(p.len(), 3);
        assert_eq!((p[0].side, p[0].alpha.len(), p[0].px_per_pt), (Side::Left, 8, 0.5));
        assert_eq!(p[1].alpha.len(), 12);
        assert_eq!((p[2].side, p[2].alpha[0], p[2].alpha[1]), (Side::Bottom, 90, 89));
        // The frame's shape unlike the opaque run: not the window.
        assert!(profiles(&alpha, w, h, (80.0, 30.0)).is_none());
        // A see-through middle: nothing found.
        let mut holed = alpha.clone();
        holed[(h / 2) * w + w / 2] = 0;
        assert!(profiles(&holed, w, h, (80.0, 60.0)).is_none());
    }

    #[test]
    fn the_probe_is_small() {
        assert_eq!(probe_size(800.0, 532.0), (400, 266));
        let (w, h) = probe_size(2560.0, 1400.0);
        assert!(w == 512 && h == 280, "{w} x {h}");
    }

    /// A reading is used until the system's appearance changes, read again after
    /// [`REFRESH`]; no evidence keeps the look; a reading from before a change is dropped.
    #[test]
    fn the_cache_follows_readings_and_the_system() {
        let mut c = LookCache::default();
        let dark = Look { dark: true, key: false };
        assert!(c.due(7, 0.0));
        let g = c.generation();
        assert!(c.record(7, g, Some(dark), 1.0), "a new look");
        assert!(!c.due(7, 5.0));
        assert_eq!(c.look(7, 5.0), Some((dark, 4.0)));
        assert!(c.due(7, 1.0 + REFRESH));
        assert!(!c.record(7, g, None, 30.0), "no evidence keeps the look");
        assert_eq!(c.look(7, 30.0).map(|l| l.0), Some(dark));
        assert!(!c.due(7, 31.0), "and is not read again at once");
        c.appearance_changed();
        assert_eq!(c.look(7, 31.0), None, "stale after a change of appearance");
        assert!(c.due(7, 31.0));
        assert!(!c.record(7, g, Some(dark), 31.0), "a reading from before the change is dropped");
        assert_eq!(c.look(7, 31.0), None);
        assert!(!c.record(7, c.generation(), None, 32.0) && c.look(7, 32.0).is_none(), "no evidence, nothing kept from before");
        assert!(c.record(7, c.generation(), Some(Look { dark: false, key: true }), 33.0));
        c.forget(7);
        assert!(c.due(7, 34.0));
    }
}
