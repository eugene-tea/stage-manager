//! The landing, pure (1.9, round sm-landing): when a switch's incoming window may take over
//! from its flying picture, the Space transform that draws the window at the picture's pose,
//! and where each picture still flying goes in the window order.
//!
//! - **The flat drawing.** The Spaces engine can draw a live window through its Space's
//!   transform (S4: a full 2D affine per window, about the window's own top-left corner: a
//!   point `q` of the window, from its corner `o`, is drawn at `o + M⁻¹(q − t)`). Apple's path
//!   turns the picture in depth, which draws a trapezoid ([`Drawn`]: its far side shorter); the
//!   nearest flat drawing is the upright parallelogram with both sides at their mean height,
//!   each centred on its own middle ([`flat`]), whose corners miss the trapezoid's by a
//!   quarter of the sides' height difference ([`flat_miss`]). Mid-flight that is up to 35 pt
//!   on an 1800 pt window; near the end, as the turn goes, it falls under a point.
//! - **The handover** ([`handover_time`]): the first moment after which the flat drawing stays
//!   within a tolerance of the picture's pose for good. From there the real window flies the
//!   rest of the path in a Space of its own ([`space_matrix`] each frame), with its own
//!   shadow, at its own place in the window order, and the picture goes. [`miss_along`]
//!   reports the miss over the whole path (logs, tests).
//! - **The order** ([`anchors`]): with real windows in place among the incoming ones (a window
//!   already shown, or one that took over from its picture), each picture still flying must
//!   pass under the real windows that end in front of it and over those that end behind it:
//!   it goes just under the nearest real window in front of it in the stage's order, or above
//!   every window when none is ([`side_of`]: a panel of its own only where the overlay's one
//!   place is wrong for it).
//! - **The variants** ([`Landing`]): a test switch picks today's landing, the order alone, or
//!   the real window's last stretch at one of three handover tolerances, for the owner's eye
//!   (`STAGE_MANAGER_LANDING`, honoured only with `STAGE_MANAGER_ONLY`; no config key).

use crate::engine::geometry::{Point, Rect};
use crate::ui::motion_math::{Drawn, Eye, Path, Spring, spring_progress};


/// A Space transform in `CGAffineTransform`'s order: `a, b, c, d, tx, ty` (a point `(x, y)`
/// maps to `(a·x + c·y + tx, b·x + d·y + ty)`).
pub type Affine = [f64; 6];

/// The identity transform: the window drawn at its frame.
pub const IDENTITY: Affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// The upright parallelogram nearest to `d`: both sides at their mean height, each centred on
/// its own middle (the flat drawing a 2D transform can give).
pub fn flat(d: &Drawn) -> Drawn {
    let h = ((d.left_bottom - d.left_top) + (d.right_bottom - d.right_top)) / 2.0;
    let (l, r) = ((d.left_top + d.left_bottom) / 2.0, (d.right_top + d.right_bottom) / 2.0);
    Drawn { left: d.left, left_top: l - h / 2.0, left_bottom: l + h / 2.0, right: d.right, right_top: r - h / 2.0, right_bottom: r + h / 2.0 }
}

/// How far the flat drawing's corners are from `d`'s (points): a quarter of the difference of
/// the sides' heights (each side's top and bottom move half of its half).
pub fn flat_miss(d: &Drawn) -> f64 {
    ((d.left_bottom - d.left_top) - (d.right_bottom - d.right_top)).abs() / 4.0
}

/// The Space transform that draws the window at `frame` (global, top-left points) as `d`
/// flattened ([`flat`]; `d` in the same coordinates): `M` is the inverse of the flat drawing's
/// map from the window's own points, `t` brings its corner from `frame`'s to the drawing's.
/// A degenerate drawing (no width or height) gives the identity.
pub fn space_matrix(d: &Drawn, frame: Rect) -> Affine {
    let f = flat(d);
    let (w, h) = (frame.w, frame.h);
    let p = (f.right - f.left) / w.max(1e-9);
    let r = (f.left_bottom - f.left_top) / h.max(1e-9);
    if w <= 0.0 || h <= 0.0 || p.abs() < 1e-9 || r.abs() < 1e-9 {
        return IDENTITY;
    }
    let s = (f.right_top - f.left_top) / w;
    // M = A⁻¹ for A = [[p, 0], [s, r]] (column vectors: x' = p·u, y' = s·u + r·v).
    let (a, b, c, dd) = (1.0 / p, -s / (p * r), 0.0, 1.0 / r);
    // t = −M (corner − o).
    let (cx, cy) = (f.left - frame.x, f.left_top - frame.y);
    [a, b, c, dd, -(a * cx + c * cy), -(b * cx + dd * cy)]
}

#[cfg(test)]
/// Where transform `m` draws the window at `frame`: its top-left, top-right, bottom-left and
/// bottom-right corners (`o + M⁻¹(q − t)`, the mapping S4 pinned).
pub fn drawn_by(m: Affine, frame: Rect) -> [Point; 4] {
    let [a, b, c, d, tx, ty] = m;
    let det = a * d - b * c;
    let inv = |x: f64, y: f64| {
        let (x, y) = (x - tx, y - ty);
        ((d * x - c * y) / det, (-b * x + a * y) / det)
    };
    let at = |u: f64, v: f64| {
        let (x, y) = inv(u, v);
        Point::new(frame.x + x, frame.y + y)
    };
    [at(0.0, 0.0), at(frame.w, 0.0), at(0.0, frame.h), at(frame.w, frame.h)]
}

/// How finely [`miss_along`] and [`handover_time`] look along the path (seconds): a 240 Hz
/// step, finer than any display's frame.
pub const LOOK_STEP: f64 = 1.0 / 240.0;

/// A picture's flight as the overlay plays it: its path, its spring, how long it runs
/// (seconds) and the eye it is seen through (the flight's, in the overlay's coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flown {
    pub path: Path,
    pub spring: Spring,
    pub duration: f64,
    pub eye: Eye,
}

impl Flown {
    /// The pose `t` seconds into the flight, drawn ([`Path::at`] on the spring; the end pose
    /// itself from `duration` on).
    pub fn drawn_at(&self, t: f64) -> Drawn {
        let pose = if t >= self.duration { self.path.to } else { self.path.at(spring_progress(self.spring, t)) };
        pose.drawn(self.eye)
    }
}

/// The flat drawing's miss along flight `f`: `(seconds, points)` every [`LOOK_STEP`], the
/// last at its end (the window's frame: no miss).
pub fn miss_along(f: &Flown) -> Vec<(f64, f64)> {
    let n = ((f.duration.max(0.0) / LOOK_STEP).ceil() as usize).max(1);
    (0..=n)
        .map(|i| {
            let t = f.duration * i as f64 / n as f64;
            (t, flat_miss(&f.drawn_at(t)))
        })
        .collect()
}

/// When the real window may take over from its picture on flight `f` (seconds after the
/// flight starts): the first moment after which the flat drawing stays within `tolerance`
/// points of the pose for good ([`miss_along`]). `None` when that leaves less than
/// `min_left` seconds of flight (nothing to fly: the picture lands as before).
pub fn handover_time(f: &Flown, tolerance: f64, min_left: f64) -> Option<f64> {
    let along = miss_along(f);
    let at = along.iter().rposition(|(_, m)| *m > tolerance).map_or(0.0, |i| along.get(i + 1).map_or(f.duration, |(t, _)| *t));
    (f.duration - at >= min_left).then_some(at)
}

/// The Space transforms that fly the window at `frame` (global) along flight `f` (in overlay
/// coordinates whose top-left corner is `origin`, global) from `from` seconds to its end, one
/// every `step` seconds, as `(seconds after the flight's start, transform)`; the last is the
/// identity at the flight's end.
pub fn stretch(f: &Flown, origin: Point, frame: Rect, from: f64, step: f64) -> Vec<(f64, Affine)> {
    let step = step.max(1e-3);
    let n = (((f.duration - from).max(0.0)) / step).ceil() as usize;
    let mut out: Vec<(f64, Affine)> = (0..n)
        .map(|i| {
            let t = from + step * i as f64;
            let d = f.drawn_at(t);
            let global = Drawn {
                left: d.left + origin.x,
                left_top: d.left_top + origin.y,
                left_bottom: d.left_bottom + origin.y,
                right: d.right + origin.x,
                right_top: d.right_top + origin.y,
                right_bottom: d.right_bottom + origin.y,
            };
            (t, space_matrix(&global, frame))
        })
        .collect();
    out.push((f.duration.max(from), IDENTITY));
    out
}

/// Each of `pictures`' anchor: the nearest window of `real` (on screen at its place: shown
/// already, or flying itself) in front of it in `order` (the stage's windows, front to back):
/// the picture passes just under that window. `None`: no real window ends in front of it, or
/// its place is not known (not in `order`): above every window, as before.
pub fn anchors(order: &[u32], real: &[u32], pictures: &[u32]) -> Vec<(u32, Option<u32>)> {
    pictures
        .iter()
        .map(|p| {
            let anchor = order.iter().position(|w| w == p).and_then(|i| order[..i].iter().rev().find(|w| real.contains(w)).copied());
            (*p, anchor)
        })
        .collect()
}

/// Where a picture anchored at `anchor` goes when the overlay itself is under `main` (a window
/// already in place, or `None`: above every window): `None` in the overlay (its place is
/// right for it), else a panel of its own just under that window.
pub fn side_of(anchor: Option<u32>, main: Option<u32>) -> Option<u32> {
    anchor.filter(|a| Some(*a) != main)
}

/// How a switch's incoming windows land: the test switch's variants (see the module notes).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Landing {
    /// Today's: every picture in the one overlay, landing over its window.
    Pictures,
    /// Today's, each picture ordered by where its window ends ([`anchors`]).
    Ordered,
    /// The real window flies the last stretch where the flat miss stays within `tolerance`
    /// points (the Spaces engine; ordered as [`Landing::Ordered`]).
    Live { tolerance: f64 },
}

/// Set by [`Landing::open_for_review`].
static REVIEW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The handover tolerance of `live` (points): a fifth of a point at 2x is under half a pixel.
pub const LIVE_TOLERANCE: f64 = 1.0;
/// `live-early`: a larger miss, a longer stretch.
pub const LIVE_EARLY_TOLERANCE: f64 = 4.0;
/// `live-late`: a smaller miss, a shorter stretch.
pub const LIVE_LATE_TOLERANCE: f64 = 0.25;

/// The environment variable that picks a variant (only with `STAGE_MANAGER_ONLY` or under
/// `[windows] engine = "spaces"`: [`Landing::open_for_review`]).
pub const LANDING_ENV: &str = "STAGE_MANAGER_LANDING";

impl Landing {
    /// The variant `word` names: `pictures`, `ordered`, `live`, `live-early`, `live-late`.
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word.trim() {
            "pictures" => Self::Pictures,
            "ordered" => Self::Ordered,
            "live" => Self::Live { tolerance: LIVE_TOLERANCE },
            "live-early" => Self::Live { tolerance: LIVE_EARLY_TOLERANCE },
            "live-late" => Self::Live { tolerance: LIVE_LATE_TOLERANCE },
            _ => return None,
        })
    }

    /// The variant asked for (`only`: `STAGE_MANAGER_ONLY`, or a stand-in under
    /// `[windows] engine = "spaces"`; `word`: [`LANDING_ENV`]): honoured only in a test or
    /// review run; else today's.
    pub fn from(only: Option<&str>, word: Option<&str>) -> Self {
        if only.is_none_or(|o| o.trim().is_empty()) {
            return Self::Pictures;
        }
        word.and_then(Self::parse).unwrap_or(Self::Pictures)
    }

    /// Opens [`LANDING_ENV`] in a run that is no test run: the launch calls it before the
    /// first [`Self::current`], true under `[windows] engine = "spaces"` (the review build of
    /// 11-14 asked for the landing variants with the Spaces engine).
    pub fn open_for_review(open: bool) {
        REVIEW.store(open, std::sync::atomic::Ordering::Relaxed);
    }

    /// The variant of this process's environment (read once).
    pub fn current() -> Self {
        static L: std::sync::OnceLock<Landing> = std::sync::OnceLock::new();
        *L.get_or_init(|| {
            let only = std::env::var("STAGE_MANAGER_ONLY").ok();
            let review = REVIEW.load(std::sync::atomic::Ordering::Relaxed);
            let gate = only.or_else(|| review.then(|| "spaces review".to_string()));
            Self::from(gate.as_deref(), std::env::var(LANDING_ENV).ok().as_deref())
        })
    }

    /// True when pictures are ordered by where their windows end.
    pub fn ordered(self) -> bool {
        !matches!(self, Self::Pictures)
    }

    /// The handover tolerance when real windows fly the last stretch.
    pub fn live(self) -> Option<f64> {
        match self {
            Self::Live { tolerance } => Some(tolerance),
            _ => None,
        }
    }

    /// Its name, for logs and `ctl state`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Pictures => "pictures",
            Self::Ordered => "ordered",
            Self::Live { tolerance } if tolerance >= LIVE_EARLY_TOLERANCE => "live-early",
            Self::Live { tolerance } if tolerance <= LIVE_LATE_TOLERANCE => "live-late",
            Self::Live { .. } => "live",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Size;
    use crate::ui::motion_math::{Flight, Plane, Pose, Toward, path_duration, travel};

    /// The strip's eye and the flight over a 2560 x 1440 pt screen, as motion_math's tests.
    const EYE: Eye = Eye { at: Point::new(15.0, 720.0), distance: 2260.0 };
    fn flight() -> Flight {
        Flight::over(0.0, Size::new(2560.0, 1440.0), 720.0)
    }

    /// A window of `w` x `h` at (`x`, `y`) growing out of a card in slot 5 (as the 11-09
    /// recordings' blue card), on Apple's spring: its flight and its frame.
    fn growing(x: f64, y: f64, w: f64, h: f64) -> (Flown, Rect) {
        let f = flight();
        let card = Plane { left_mid: Point::new(11.0, 935.9 + 107.44 / 2.0), size: Size::new(162.68, 107.44), tilt: 0.8995, corner: 0.0 };
        let frame = Rect::new(x, y, w, h);
        let window = Plane::window(frame, Point::new(0.0, 0.0), 0.0);
        let path = Path { from: card.pose(EYE, f.eye), to: window.pose(EYE, f.eye), toward: Toward::Window, origin_x: f.origin_x };
        let unturned = |p: &Plane| Rect::new(p.left_mid.x, p.left_mid.y - p.size.h / 2.0, p.size.w, p.size.h);
        let duration = path_duration(Spring::APPLE, travel(unturned(&card), frame), Toward::Window);
        (Flown { path, spring: Spring::APPLE, duration, eye: f.eye }, frame)
    }

    fn near(a: Point, b: Point, tol: f64) -> bool {
        (a.x - b.x).abs() <= tol && (a.y - b.y).abs() <= tol
    }

    /// The transform draws the window as the flat drawing, by S4's mapping: corners in place
    /// for the identity, a shear for a side higher than the other, a scale and a move for a
    /// window drawn smaller elsewhere.
    #[test]
    fn the_space_transform_draws_the_flat_drawing() {
        let frame = Rect::new(300.0, 130.0, 1800.0, 1100.0);
        let at_frame = Drawn { left: 300.0, left_top: 130.0, left_bottom: 1230.0, right: 2100.0, right_top: 130.0, right_bottom: 1230.0 };
        let m = space_matrix(&at_frame, frame);
        assert!(m.iter().zip(IDENTITY).all(|(a, b)| (a - b).abs() < 1e-9), "{m:?}");
        // Half size, moved, the right side 60 pt higher (flat: no turn).
        let d = Drawn { left: 500.0, left_top: 400.0, left_bottom: 950.0, right: 1400.0, right_top: 340.0, right_bottom: 890.0 };
        let m = space_matrix(&d, frame);
        let [tl, tr, bl, br] = drawn_by(m, frame);
        for (got, want) in [(tl, (500.0, 400.0)), (tr, (1400.0, 340.0)), (bl, (500.0, 950.0)), (br, (1400.0, 890.0))] {
            assert!(near(got, Point::new(want.0, want.1), 1e-6), "{got:?} vs {want:?}");
        }
        assert_eq!(flat_miss(&d), 0.0, "a parallelogram is drawn exactly");
        // S4's still: b = 0.3 draws the right side of an 800 pt window 240 pt higher.
        let small = Rect::new(100.0, 100.0, 800.0, 500.0);
        let [tl, tr, ..] = drawn_by([1.0, 0.3, 0.0, 1.0, 0.0, 0.0], small);
        assert!((tl.y - tr.y - 240.0).abs() < 1e-9);
        // A trapezoid: its sides at their mean height, the miss a quarter of the difference.
        let t = Drawn { left: 500.0, left_top: 400.0, left_bottom: 1000.0, right: 1400.0, right_top: 450.0, right_bottom: 950.0 };
        assert!((flat_miss(&t) - 25.0).abs() < 1e-9);
        let [tl, tr, bl, br] = drawn_by(space_matrix(&t, frame), frame);
        assert!(near(tl, Point::new(500.0, 425.0), 1e-6) && near(bl, Point::new(500.0, 975.0), 1e-6), "{tl:?} {bl:?}");
        assert!(near(tr, Point::new(1400.0, 425.0), 1e-6) && near(br, Point::new(1400.0, 975.0), 1e-6), "{tr:?} {br:?}");
    }

    /// Along Apple's path the flat miss is large mid-flight and falls under a point near the
    /// end: the handover comes late in the flight, with a stretch left to fly, earlier for a
    /// larger tolerance, and the transforms from there end on the identity.
    #[test]
    fn the_window_takes_over_where_the_flat_miss_stays_small() {
        let (fl, frame) = growing(380.0, 150.0, 1800.0, 1100.0);
        let along = miss_along(&fl);
        let worst = along.iter().fold(0.0_f64, |m, (_, x)| m.max(*x));
        assert!((30.0..42.0).contains(&worst), "an 1800 pt window misses up to about 35 pt mid-flight: {worst:.1}");
        assert_eq!(along.last().map(|a| a.1), Some(0.0), "none at the frame");
        let at = |tol: f64| handover_time(&fl, tol, 0.05).expect("a stretch to fly");
        let (early, default, late) = (at(LIVE_EARLY_TOLERANCE), at(LIVE_TOLERANCE), at(LIVE_LATE_TOLERANCE));
        // Measured on this path (0.74 s of flight): 0.35 s (28 pt left, 6 pt a 60 Hz frame),
        // 0.45 s (7 pt left, 1.5 a frame), 0.54 s (1.7 pt left, 0.4 a frame).
        assert!(early < default && default < late, "{early:.3} {default:.3} {late:.3}");
        assert!((0.3..0.4).contains(&early) && (0.4..0.5).contains(&default) && (0.5..0.6).contains(&late), "{early:.3} {default:.3} {late:.3} of {:.3}", fl.duration);
        assert!(along.iter().filter(|(t, _)| *t >= default).all(|(_, m)| *m <= LIVE_TOLERANCE), "within the tolerance for good");
        // A small window misses less, so takes over sooner.
        let (small, _) = growing(900.0, 500.0, 700.0, 450.0);
        let small_at = handover_time(&small, LIVE_TOLERANCE, 0.05).expect("a stretch");
        assert!(small_at < default, "{small_at:.3} vs {default:.3}");
        // Nothing left to fly: no handover.
        assert_eq!(handover_time(&fl, LIVE_LATE_TOLERANCE, fl.duration), None);
        let steps = stretch(&fl, Point::new(0.0, 0.0), frame, default, 1.0 / 120.0);
        assert_eq!(steps.last().map(|s| s.1), Some(IDENTITY));
        assert!((steps[0].0 - default).abs() < 1e-9 && steps.windows(2).all(|w| w[1].0 > w[0].0));
        // Each step draws the window where the picture is, within the tolerance.
        for (t, m) in &steps[..steps.len() - 1] {
            let d = fl.drawn_at(*t);
            let [tl, tr, bl, br] = drawn_by(*m, frame);
            let tol = LIVE_TOLERANCE + 1e-6;
            assert!(
                near(tl, Point::new(d.left, d.left_top), tol) && near(tr, Point::new(d.right, d.right_top), tol) && near(bl, Point::new(d.left, d.left_bottom), tol) && near(br, Point::new(d.right, d.right_bottom), tol),
                "at {t:.3}: {tl:?} {tr:?} {bl:?} {br:?} vs {d:?}"
            );
        }
        // In an overlay placed elsewhere the transform follows its origin.
        let moved = stretch(&fl, Point::new(0.0, 25.0), Rect::new(380.0, 175.0, 1800.0, 1100.0), default, 1.0 / 120.0);
        assert_eq!(moved.len(), steps.len());
        assert!(moved.iter().zip(&steps).all(|(a, b)| a.1.iter().zip(b.1).all(|(x, y)| (x - y).abs() < 1e-9)), "the same transform about the window's own corner");
        // A pose turned the other way misses as much.
        let p = fl.path.at(0.5);
        let mirrored = Pose { tilt: -p.tilt, ..p };
        assert!((flat_miss(&p.drawn(fl.eye)) - flat_miss(&mirrored.drawn(fl.eye))).abs() < 0.5);
    }

    /// Each picture goes under the nearest real window in front of it; with none, above every
    /// window; a picture whose place is not known stays in the overlay.
    #[test]
    fn pictures_are_ordered_by_where_their_windows_end() {
        // The stage front to back: 1, 2 (in place), 3, 4 (flying itself), 5.
        let order = [1, 2, 3, 4, 5];
        let a = anchors(&order, &[2, 4], &[1, 3, 5, 9]);
        assert_eq!(a, vec![(1, None), (3, Some(2)), (5, Some(4)), (9, None)]);
        // The overlay under window 2 (Transition::under): the pictures behind it stay in it.
        assert_eq!(side_of(Some(2), Some(2)), None);
        assert_eq!(side_of(Some(4), Some(2)), Some(4), "behind a deeper real window: a panel under it");
        assert_eq!(side_of(None, None), None);
        assert_eq!(side_of(Some(2), None), Some(2), "the big window in place in front of a picture flying over every window: under it");
        assert!(anchors(&[], &[2], &[1]).iter().all(|(_, a)| a.is_none()), "no order known: as before");
    }

    #[test]
    fn the_variants_are_a_test_switch() {
        assert_eq!(Landing::from(None, Some("live")), Landing::Pictures, "honoured only in a test run");
        assert_eq!(Landing::from(Some(" "), Some("live")), Landing::Pictures);
        assert_eq!(Landing::from(Some("fixture"), None), Landing::Pictures);
        assert_eq!(Landing::from(Some("fixture"), Some("nonsense")), Landing::Pictures);
        assert_eq!(Landing::from(Some("fixture"), Some("ordered")), Landing::Ordered);
        for word in ["pictures", "ordered", "live", "live-early", "live-late"] {
            let l = Landing::from(Some("fixture"), Some(word));
            assert_eq!(l.name(), word);
        }
        assert!(!Landing::Pictures.ordered() && Landing::Ordered.ordered() && Landing::Live { tolerance: 1.0 }.ordered());
        assert_eq!(Landing::parse("live-late").and_then(Landing::live), Some(LIVE_LATE_TOLERANCE));
        assert_eq!(Landing::Ordered.live(), None);
    }
}
