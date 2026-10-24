//! Settling a solar frame near a frame change (the corner patch, [`super::wallpaper`]).
//!
//! Without Location Services the sun is placed at the time zone's place, and every place
//! within [`super::wallpaper::SOLAR_SLACK_DEG`] of it must agree on the frame. Near a frame
//! change they do not, and the frame is one of a few candidates
//! ([`super::wallpaper::solar_candidates`]); on a dusk-heavy schedule that is hours a day.
//! The patch still draws only from the picture's file, exact or off; two ways settle which
//! frame macOS shows:
//!
//! 1. **Alike** ([`alike`]): every candidate renders the patch's block the same within
//!    [`ALIKE_LEVELS`] in every cell of a [`CELL_PT`] grid ([`Thumb`]): any of them is exact.
//!    Pure, no capture.
//! 2. **One still** ([`best_match`]): otherwise, a still of the desktop's block (the picture
//!    and what lies on it, never a window), taken only where window captures already work,
//!    is compared with each candidate's render on the same grid, both drawn in the display's
//!    colour space. Desktop icons are masked out ([`mask`]). The best must be clearly close
//!    ([`MATCH_LEVELS`]) and clearly better than the next ([`MATCH_RATIO`], [`MATCH_GAP`]);
//!    no clear match is off. The still picks the frame only: what is drawn is the file's.
//!
//! [`next`] decides from what is known which way the frame settles now, when a still is
//! due (at most one in flight, again only after [`RECHECK`]), and why it is off.

use std::time::Duration;

use super::geometry::{Rect, Size};

/// The side of a comparison cell (points): fine enough to see a shape move, coarse enough
/// that resampling (the window server's against Core Graphics') averages out.
pub const CELL_PT: f64 = 4.0;

/// Candidates look alike when no cell's colour differs by more than this (0-255, per
/// channel) between any two: below what the eye tells apart on a smooth gradient.
pub const ALIKE_LEVELS: f64 = 3.0;

/// A still matches a candidate when their cells differ by at most this on average (0-255,
/// per channel): the window server's own scaling and compositing against the patch's render
/// of the same frame stays well under it; another frame's light lies far above.
pub const MATCH_LEVELS: f64 = 6.0;

/// The next candidate must differ from the still at least this many times the best one's
/// difference ...
pub const MATCH_RATIO: f64 = 2.0;

/// ... and by at least this many levels more (0-255): two near frames are never told apart
/// by noise.
pub const MATCH_GAP: f64 = 3.0;

/// The least share of the block's cells compared (the rest under desktop icons).
pub const MIN_COVERAGE: f64 = 0.25;

/// A still's cell with alpha below this is see-through (the picture's own window not in it).
pub const OPAQUE: u8 = 250;

/// How often a still settles the frame again while the candidates differ.
pub const RECHECK: Duration = Duration::from_secs(super::wallpaper::SOLAR_RECHECK as u64);

/// A still answer is awaited this long (the last frame it settled is kept meanwhile).
pub const STALL: Duration = Duration::from_secs(5);

/// A picture of a block as mean colours on a [`CELL_PT`] grid.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumb {
    /// Cells across.
    pub cols: usize,
    /// Cells down.
    pub rows: usize,
    /// Each cell's mean red, green and blue (0-255, premultiplied), row after row.
    pub rgb: Vec<[f32; 3]>,
    /// Each cell's least alpha.
    pub alpha: Vec<u8>,
}

impl Thumb {
    /// The grid for a block of `size` points: whole cells, at least one each way.
    pub fn grid(size: Size) -> (usize, usize) {
        let n = |v: f64| if v.is_finite() { ((v / CELL_PT).floor() as usize).max(1) } else { 1 };
        (n(size.w), n(size.h))
    }

    /// From 32-bit pixels, B, G, R, A in memory (`row` bytes a row, `w` x `h`), of a block of
    /// `size` points: each cell's pixels averaged. `None` for a buffer too small or a block
    /// with fewer pixels than cells.
    pub fn from_bgra(bytes: &[u8], w: usize, h: usize, row: usize, size: Size) -> Option<Thumb> {
        let (cols, rows) = Self::grid(size);
        if w < cols || h < rows || row < w * 4 || bytes.len() < row * (h - 1) + w * 4 {
            return None;
        }
        let edge = |i: usize, n: usize, px: usize| (i * px + n / 2) / n;
        let mut rgb = Vec::with_capacity(cols * rows);
        let mut alpha = Vec::with_capacity(cols * rows);
        for r in 0..rows {
            let (y0, y1) = (edge(r, rows, h), edge(r + 1, rows, h).max(edge(r, rows, h) + 1));
            for c in 0..cols {
                let (x0, x1) = (edge(c, cols, w), edge(c + 1, cols, w).max(edge(c, cols, w) + 1));
                let (mut sum, mut least, mut n) = ([0u64; 3], u8::MAX, 0u64);
                for y in y0..y1.min(h) {
                    let line = &bytes[y * row..y * row + w * 4];
                    for p in line[x0 * 4..x1.min(w) * 4].as_chunks::<4>().0 {
                        sum[0] += u64::from(p[2]);
                        sum[1] += u64::from(p[1]);
                        sum[2] += u64::from(p[0]);
                        least = least.min(p[3]);
                        n += 1;
                    }
                }
                let n = n.max(1) as f32;
                rgb.push([sum[0] as f32 / n, sum[1] as f32 / n, sum[2] as f32 / n]);
                alpha.push(least);
            }
        }
        Some(Thumb { cols, rows, rgb, alpha })
    }

    fn same_grid(&self, other: &Thumb) -> bool {
        self.cols == other.cols && self.rows == other.rows && self.rgb.len() == other.rgb.len()
    }
}

/// The largest difference of any cell's channel between `a` and `b` (0-255); `None` for
/// different grids.
pub fn max_diff(a: &Thumb, b: &Thumb) -> Option<f64> {
    a.same_grid(b).then(|| {
        a.rgb.iter().zip(&b.rgb).flat_map(|(p, q)| (0..3).map(move |i| f64::from((p[i] - q[i]).abs()))).fold(0.0, f64::max)
    })
}

/// The mean difference of the cells `mask` keeps between `a` and `b` (0-255, per channel);
/// `None` for different grids or no cell kept.
pub fn mean_diff(a: &Thumb, b: &Thumb, mask: &[bool]) -> Option<f64> {
    if !a.same_grid(b) || mask.len() != a.rgb.len() {
        return None;
    }
    let (mut sum, mut n) = (0.0, 0usize);
    for ((p, q), keep) in a.rgb.iter().zip(&b.rgb).zip(mask) {
        if *keep {
            sum += (0..3).map(|i| f64::from((p[i] - q[i]).abs())).sum::<f64>();
            n += 3;
        }
    }
    (n > 0).then(|| sum / n as f64)
}

/// Whether every candidate's render looks the same ([`ALIKE_LEVELS`] in every cell): then
/// any of them is the frame macOS shows, as far as the patch's block can tell. False for
/// fewer than two or grids that differ.
pub fn alike(thumbs: &[&Thumb]) -> bool {
    thumbs.len() >= 2 && thumbs.iter().enumerate().all(|(i, a)| thumbs[i + 1..].iter().all(|b| max_diff(a, b).is_some_and(|d| d <= ALIKE_LEVELS)))
}

/// The cells of a `cols` x `rows` grid over `region` (points) compared: those no desktop icon
/// (`icons`, grown by their labels, same coordinates) meets.
pub fn mask(cols: usize, rows: usize, region: Rect, icons: &[Rect]) -> Vec<bool> {
    let (cw, ch) = (region.w / cols.max(1) as f64, region.h / rows.max(1) as f64);
    let mut out = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            let cell = Rect::new(region.x + c as f64 * cw, region.y + r as f64 * ch, cw, ch);
            out.push(!icons.iter().any(|i| i.intersect(&cell).is_some()));
        }
    }
    out
}

/// A still matched one candidate ([`best_match`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matched {
    /// The candidate frame.
    pub index: usize,
    /// Its difference from the still (0-255).
    pub best: f64,
    /// The next candidate's.
    pub next: f64,
}

/// The candidate (`(frame, render)`) a still of the same block matches, its cells kept by
/// `mask`: the best clearly close ([`MATCH_LEVELS`]) and clearly better than the next
/// ([`MATCH_RATIO`], [`MATCH_GAP`]); why not otherwise.
pub fn best_match(still: &Thumb, candidates: &[(usize, &Thumb)], mask: &[bool]) -> Result<Matched, &'static str> {
    if candidates.len() < 2 {
        return Err("fewer than two candidate frames to tell apart");
    }
    if mask.len() != still.rgb.len() {
        return Err("the desktop's still is not of the candidates' block");
    }
    let kept = mask.iter().filter(|k| **k).count();
    if (kept as f64) < MIN_COVERAGE * mask.len() as f64 {
        return Err("desktop icons cover most of the corner");
    }
    if still.alpha.iter().zip(mask).any(|(a, k)| *k && *a < OPAQUE) {
        return Err("the desktop's still does not hold the picture there");
    }
    let mut scored: Vec<(usize, f64)> = Vec::with_capacity(candidates.len());
    for (index, t) in candidates {
        scored.push((*index, mean_diff(still, t, mask).ok_or("the desktop's still is not of the candidates' block")?));
    }
    scored.sort_by(|a, b| a.1.total_cmp(&b.1));
    let ((index, best), next) = (scored[0], scored[1].1);
    if best > MATCH_LEVELS {
        return Err("the desktop's still matches none of the candidate frames");
    }
    if next < (best * MATCH_RATIO).max(best + MATCH_GAP) {
        return Err("the desktop's still matches two candidate frames nearly alike");
    }
    Ok(Matched { index, best, next })
}

/// Which way a frame was settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    /// The candidates look alike ([`alike`]).
    Alike,
    /// A still of the desktop matched it ([`best_match`]).
    Still,
}

/// What is known for [`next`].
#[derive(Debug, Clone)]
pub struct Facts<'a> {
    /// The candidate frames (two or more), the time zone's own first.
    pub candidates: &'a [usize],
    /// Whether their renders look alike; `None` while they render.
    pub alike: Option<bool>,
    /// The last still for this picture: the frame it matched or why none, and its age.
    pub last_still: Option<(Result<usize, &'static str>, Duration)>,
    /// A still is in flight.
    pub in_flight: bool,
    /// Whether a still may be taken now (the capture gate, the screen, the icons), why not.
    pub may_take: Result<(), &'static str>,
}

/// What to do ([`next`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Render the candidates (hidden meanwhile).
    Render,
    /// Draw this frame, settled this way; take a new still too when `take`.
    Draw {
        /// The frame.
        index: usize,
        /// How it was settled.
        way: Way,
        /// A recheck is due.
        take: bool,
    },
    /// Off (hidden), and why; take a still when `take`.
    Off {
        /// Why.
        why: &'static str,
        /// A still is due.
        take: bool,
    },
}

/// Which way the frame settles now. Alike candidates draw (the last still's frame when it
/// is one of them and fresh, else the first). Otherwise a still's frame stands for
/// [`RECHECK`], then a new still is taken; the old frame is kept while it is in flight, at
/// most [`STALL`] more. No still before [`RECHECK`] has passed since the last one.
pub fn next(f: &Facts) -> Step {
    let Some(alike) = f.alike else { return Step::Render };
    let age = f.last_still.as_ref().map(|(_, a)| *a);
    let fresh = |a: Duration| a < RECHECK + STALL;
    let stilled = f.last_still.as_ref().and_then(|(r, a)| r.ok().filter(|i| fresh(*a) && f.candidates.contains(i)));
    if alike {
        let index = stilled.unwrap_or(f.candidates[0]);
        return Step::Draw { index, way: Way::Alike, take: false };
    }
    let due = !f.in_flight && f.may_take.is_ok() && age.is_none_or(|a| a >= RECHECK);
    if let Some(index) = stilled {
        return Step::Draw { index, way: Way::Still, take: due };
    }
    let why = match (&f.last_still, f.may_take) {
        _ if due || f.in_flight => "its candidate frames differ here; a still of the desktop settles which",
        (_, Err(why)) => why,
        (Some((Err(why), _)), Ok(())) => why,
        (Some((Ok(_), _)), Ok(())) => "the frame the last still of the desktop matched is no candidate now",
        (None, Ok(())) => "its candidate frames differ here",
    };
    Step::Off { why, take: due }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::wallpaper::{SunFrame, solar_candidates};

    const BLOCK: Size = Size::new(480.0, 560.0);

    /// BGRA pixels at `scale` for [`BLOCK`], each pixel `f(x, y)` in points as [r, g, b].
    fn pixels(scale: f64, f: impl Fn(f64, f64) -> [u8; 3]) -> (Vec<u8>, usize, usize) {
        let (w, h) = ((BLOCK.w * scale) as usize, (BLOCK.h * scale) as usize);
        let mut b = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let [r, g, bl] = f(x as f64 / scale, y as f64 / scale);
                b.extend_from_slice(&[bl, g, r, 255]);
            }
        }
        (b, w, h)
    }

    fn thumb(scale: f64, f: impl Fn(f64, f64) -> [u8; 3]) -> Thumb {
        let (b, w, h) = pixels(scale, f);
        Thumb::from_bgra(&b, w, h, w * 4, BLOCK).expect("a thumb")
    }

    /// A dusk sky: a gradient whose brightness is `light`, with a hill's edge.
    fn sky(light: f64) -> impl Fn(f64, f64) -> [u8; 3] {
        move |x, y| {
            let v = (light * (1.0 - y / BLOCK.h * 0.5) * 200.0).clamp(0.0, 255.0) as u8;
            if y > 400.0 + x * 0.1 { [20, 30, 25] } else { [v, (f64::from(v) * 0.8) as u8, 120] }
        }
    }

    #[test]
    fn a_thumb_averages_cells_on_a_grid_in_points_whatever_the_scale() {
        let one = thumb(1.0, sky(0.7));
        let two = thumb(2.0, sky(0.7));
        assert_eq!((one.cols, one.rows), (120, 140));
        assert_eq!((two.cols, two.rows), (one.cols, one.rows));
        let all = vec![true; one.rgb.len()];
        assert!(mean_diff(&one, &two, &all).expect("same grid") < 0.5, "the same picture at 1x and 2x");
        assert!(two.alpha.iter().all(|a| *a == 255));
        // A buffer too small, or one row short.
        let (b, w, h) = pixels(1.0, sky(0.7));
        assert_eq!(Thumb::from_bgra(&b[..b.len() - 4], w, h, w * 4, BLOCK), None);
        assert_eq!(Thumb::from_bgra(&b, 10, 10, 40, BLOCK), None, "fewer pixels than cells");
        // Other grids do not compare.
        let small = Thumb::from_bgra(&b, w, h, w * 4, Size::new(240.0, 280.0)).expect("a thumb");
        assert_eq!(max_diff(&one, &small), None);
        assert_eq!(mean_diff(&one, &small, &vec![true; small.rgb.len()]), None);
    }

    #[test]
    fn candidates_alike_only_when_every_cell_agrees() {
        let a = thumb(1.0, sky(0.70));
        let b = thumb(1.0, sky(0.705));
        let c = thumb(1.0, sky(0.9));
        assert!(alike(&[&a, &b]), "a level apart: alike");
        assert!(!alike(&[&a, &b, &c]), "one brighter frame among them");
        assert!(!alike(&[&a]), "one is no choice");
        // One small shape that differs (a star, a light) is enough.
        let star = thumb(1.0, |x, y| if (300.0..308.0).contains(&x) && (100.0..108.0).contains(&y) { [255, 255, 255] } else { sky(0.70)(x, y) });
        assert!(max_diff(&a, &star).expect("same grid") > 100.0);
        assert!(mean_diff(&a, &star, &vec![true; a.rgb.len()]).expect("same grid") < 0.1, "a mean would miss it");
        assert!(!alike(&[&a, &star]));
    }

    #[test]
    fn a_still_matches_the_candidate_it_shows_clearly_or_none() {
        let dusk = thumb(2.0, sky(0.6));
        let night = thumb(2.0, sky(0.25));
        let all = vec![true; dusk.rgb.len()];
        let cands = [(3, &dusk), (1, &night)];
        // The window server's draw of the dusk frame: a level or two off, a pixel shifted.
        let still = thumb(2.0, |x, y| {
            let [r, g, b] = sky(0.6)(x + 0.5, y);
            [r.saturating_add(2), g.saturating_add(1), b]
        });
        let m = best_match(&still, &cands, &all).expect("dusk");
        assert_eq!(m.index, 3);
        assert!(m.best < 3.0 && m.next > 20.0, "{m:?}");
        assert_eq!(best_match(&still, &[(1, &night), (3, &dusk)], &all).map(|m| m.index), Ok(3), "order does not matter");
        // Halfway between: neither clearly.
        let between = thumb(2.0, sky(0.425));
        assert!(best_match(&between, &cands, &all).is_err());
        // Two candidates nearly alike: noise must not pick one.
        let near = thumb(2.0, sky(0.615));
        assert_eq!(best_match(&still, &[(3, &dusk), (4, &near)], &all), Err("the desktop's still matches two candidate frames nearly alike"));
        // Something else on the desktop there (a widget): no candidate.
        let widget = thumb(2.0, |x, y| if x > 100.0 && y < 300.0 { [240, 240, 240] } else { sky(0.6)(x, y) });
        assert_eq!(best_match(&widget, &cands, &all), Err("the desktop's still matches none of the candidate frames"));
        assert!(best_match(&still, &cands[..1], &all).is_err(), "one candidate");
    }

    #[test]
    fn icons_are_masked_out_and_a_see_through_still_never_matches() {
        let region = Rect::new(2080.0, 880.0, BLOCK.w, BLOCK.h);
        let dusk = thumb(2.0, sky(0.6));
        let night = thumb(2.0, sky(0.25));
        // An icon at the block's top-left, its label below it.
        let icon = Rect::new(2090.0, 890.0, 100.0, 120.0);
        let m = mask(dusk.cols, dusk.rows, region, &[icon]);
        assert_eq!(m.len(), dusk.rgb.len());
        let at = |x: f64, y: f64| ((y - region.y) / CELL_PT) as usize * dusk.cols + ((x - region.x) / CELL_PT) as usize;
        assert!(!m[at(2100.0, 900.0)] && m[at(2079.0 + BLOCK.w, 879.0 + BLOCK.h)] && m[0], "only the icon's cells are not compared");
        let with_icon = thumb(2.0, |x, y| if x < 112.0 && y < 132.0 { [250, 10, 10] } else { sky(0.6)(x, y) });
        assert_eq!(best_match(&with_icon, &[(3, &dusk), (1, &night)], &m).map(|m| m.index), Ok(3));
        // Icons over most of the block: off.
        let crowd: Vec<Rect> = (0..5).map(|i| Rect::new(2080.0, 880.0 + f64::from(i) * 110.0, 400.0, 100.0)).collect();
        let m = mask(dusk.cols, dusk.rows, region, &crowd);
        assert_eq!(best_match(&dusk, &[(3, &dusk), (1, &night)], &m), Err("desktop icons cover most of the corner"));
        // The still holds the icons only (the picture's own window not drawn): see-through.
        let (mut b, w, h) = pixels(1.0, sky(0.6));
        b[3] = 0;
        let clear = Thumb::from_bgra(&b, w, h, w * 4, BLOCK).expect("a thumb");
        let all = vec![true; clear.rgb.len()];
        assert_eq!(best_match(&clear, &[(3, &dusk), (1, &night)], &all), Err("the desktop's still does not hold the picture there"));
    }

    fn facts<'a>(c: &'a [usize], alike: Option<bool>) -> Facts<'a> {
        Facts { candidates: c, alike, last_still: None, in_flight: false, may_take: Ok(()) }
    }

    #[test]
    fn the_frame_settles_alike_by_a_still_or_not_at_all() {
        let c = [3, 4];
        assert_eq!(next(&facts(&c, None)), Step::Render, "nothing shows while the candidates render");
        assert_eq!(next(&facts(&c, Some(true))), Step::Draw { index: 3, way: Way::Alike, take: false }, "alike: no capture");
        // Differ: a still is taken, off meanwhile.
        let differ = facts(&c, Some(false));
        assert!(matches!(next(&differ), Step::Off { take: true, .. }));
        let flying = Facts { in_flight: true, ..differ.clone() };
        assert!(matches!(next(&flying), Step::Off { take: false, .. }), "at most one in flight");
        // The still matched 4: drawn for a minute, then rechecked, kept while the recheck flies.
        let s = |r: Result<usize, &'static str>, secs: u64| Some((r, Duration::from_secs(secs)));
        let settled = Facts { last_still: s(Ok(4), 10), ..differ.clone() };
        assert_eq!(next(&settled), Step::Draw { index: 4, way: Way::Still, take: false });
        let stale = Facts { last_still: s(Ok(4), 60), ..differ.clone() };
        assert_eq!(next(&stale), Step::Draw { index: 4, way: Way::Still, take: true }, "a recheck at SOLAR_RECHECK");
        let waiting = Facts { in_flight: true, ..stale.clone() };
        assert_eq!(next(&waiting), Step::Draw { index: 4, way: Way::Still, take: false });
        let lost = Facts { last_still: s(Ok(4), 66), in_flight: true, ..differ.clone() };
        assert!(matches!(next(&lost), Step::Off { take: false, .. }), "past the stall: off");
        // A still's frame that is no candidate now is not drawn; a new still waits its minute.
        let moved = Facts { last_still: s(Ok(2), 10), ..differ.clone() };
        assert_eq!(next(&moved), Step::Off { why: "the frame the last still of the desktop matched is no candidate now", take: false });
        // No clear match: off, and no new still before the minute.
        let unclear = Facts { last_still: s(Err("the desktop's still matches none of the candidate frames"), 20), ..differ.clone() };
        assert_eq!(next(&unclear), Step::Off { why: "the desktop's still matches none of the candidate frames", take: false });
        // No still may be taken (the gate closed, locked, asleep): off and why.
        let gated = Facts { may_take: Err("window captures are off"), ..differ.clone() };
        assert_eq!(next(&gated), Step::Off { why: "window captures are off", take: false });
        let gated_settled = Facts { may_take: Err("the screen is locked"), ..stale.clone() };
        assert_eq!(next(&gated_settled), Step::Draw { index: 4, way: Way::Still, take: false }, "the fresh frame stands; no recheck");
        // Alike candidates take the still's frame when it is one of them.
        let both = Facts { alike: Some(true), ..settled };
        assert_eq!(next(&both), Step::Draw { index: 4, way: Way::Alike, take: false });
    }

    /// A dusk-heavy schedule (Apple's solar pictures crowd frames about the horizon, both
    /// ways): over a day at the owner's place, the minutes off for want of an exact frame
    /// drop once candidates that render alike are drawn. Dusk frames come in runs of four
    /// that look the same in the corner (frames a few degrees apart differing only up the
    /// sky, out of the corner).
    #[test]
    fn a_day_on_a_dusk_heavy_schedule_is_off_far_less_when_alike_candidates_draw() {
        let mut suns = vec![SunFrame { altitude: 60.0, azimuth: 180.0, index: 0 }, SunFrame { altitude: -40.0, azimuth: 0.0, index: 1 }];
        let mut i = 2;
        for (az0, step) in [(95.0, 1.0), (265.0, -1.0)] {
            for k in 0..8 {
                let alt = -14.0 + f64::from(k) * 4.0;
                suns.push(SunFrame { altitude: alt, azimuth: az0 + step * alt, index: i });
                i += 1;
            }
        }
        // The look of each frame in the corner: dusk frames in runs of four (below and above
        // the horizon, each way), day and night alone.
        let look = |f: usize| if f < 2 { f as f64 * 0.9 } else { 0.2 + ((f - 2) / 4) as f64 * 0.15 };
        let thumbs: Vec<Thumb> = (0..i).map(|f| thumb(0.25, sky(look(f)))).collect();
        let la = (34.05, -118.24);
        let start = 1_792_738_800.0; // 2026-10-23 07:00 UTC, midnight in Los Angeles
        let (mut before, mut after) = (0, 0);
        for minute in 0..1440 {
            let c = solar_candidates(&suns, start + f64::from(minute) * 60.0, la).expect("frames");
            if c.len() > 1 {
                before += 1;
                let t: Vec<&Thumb> = c.iter().map(|f| &thumbs[*f]).collect();
                if alike(&t) {
                    assert!(c.iter().all(|f| look(*f) == look(c[0])), "alike only when they look the same: {c:?}");
                } else {
                    after += 1;
                }
            }
        }
        assert!(before > 120, "hours off by the clock alone: {before} min");
        // 10-23 at Los Angeles: 642 min before, 363 after; at least a quarter less.
        assert!(after * 4 < before * 3, "alike candidates draw: {before} min off before, {after} after");
    }
}
