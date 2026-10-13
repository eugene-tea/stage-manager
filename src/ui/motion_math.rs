//! Motion math, pure: the spring's progress over time, the planes snapshots move between,
//! where a drop lands in the column, and the snapshot cache's bookkeeping.
//!
//! - **Spring.** Apple moves windows on a spring that starts at rest one frame after the
//!   mouse-up: mass 1, stiffness 272, damping 33 (critically damped, ω ≈ 16.5 rad/s: 50 % at
//!   ~100 ms, 90 % at ~235 ms, 99 % at ~400 ms from its start; fitted 10-12 to 37 windows of the
//!   laptop session, `docs/UX-SPEC.md`, "Transitions"); cards glide at ω ≈ 17.0 and the strip
//!   slides at ω ≈ 16.8. [`spring_progress`] gives the fraction of the way covered after `t`
//!   seconds for any mass, stiffness and damping; [`time_to_progress`] when a fraction is first
//!   reached (the moment an incoming window may take its place behind its growing snapshot,
//!   `[animation] unpark_at`). `[animation] speed` scales every spring ([`Spring::at_speed`]:
//!   the same curve, ω times the speed) and duration; [`switch_spring`], [`glide_spring`] and
//!   [`slide_spring`] are the style's springs with it applied.
//! - **Planes.** A snapshot is a plane turned about its left edge, like a strip card: at a
//!   window's frame it is flat and full size; on a card it is the card's thumbnail, turned by
//!   the strip's tilt and seen from the strip's eye. Moving between the two on one spring
//!   (position of the left edge's middle, size, turn, corner radius) is the switch animation.
//! - **Drop spot.** A card or a window released over the column goes into the gap between the
//!   cards whose middles are above and below the pointer. A band around each card's middle
//!   (`[drag] merge_zone` of its height, for a card dragged with `[strip] merge_on_drop`)
//!   means "onto this card": a merge. A window dropped on the strip has no band.

use std::collections::HashMap;

use crate::config::StripStyle;
use crate::engine::geometry::{Point, Rect, Size};

/// Spring parameters (`[animation]` or `[motion]`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub stiffness: f64,
    pub damping: f64,
    pub mass: f64,
}

impl Spring {
    /// Apple's windows in a switch, measured (`[animation]`'s default).
    pub const APPLE: Spring = Spring { stiffness: 272.0, damping: 33.0, mass: 1.0 };

    /// The same motion `speed` times as fast: ω times `speed`, the same damping ratio
    /// (stiffness times speed², damping times speed). A speed of 0 or less leaves it as it is
    /// (nothing animates then; the callers jump instead).
    pub fn at_speed(self, speed: f64) -> Spring {
        if speed <= 0.0 || speed == 1.0 {
            return self;
        }
        Spring { stiffness: self.stiffness * speed * speed, damping: self.damping * speed, mass: self.mass }
    }
}

/// The windows' spring in a switch, the desktop reveal, ⌘M and drops (`[animation]`), at the
/// style's speed.
pub fn switch_spring(style: &StripStyle) -> Spring {
    let a = &style.animation;
    Spring { stiffness: a.spring_stiffness, damping: a.spring_damping, mass: a.spring_mass }.at_speed(a.speed())
}

/// The cards' glide to new slots (`[motion] spring_*`), at the style's speed.
pub fn glide_spring(style: &StripStyle) -> Spring {
    let m = &style.motion;
    Spring { stiffness: m.spring_stiffness, damping: m.spring_damping, mass: m.spring_mass }.at_speed(style.animation.speed())
}

/// The strip's slide out and back in (`[motion] slide_*`), at the style's speed.
pub fn slide_spring(style: &StripStyle) -> Spring {
    let m = &style.motion;
    Spring { stiffness: m.slide_stiffness, damping: m.slide_damping, mass: m.slide_mass }.at_speed(style.animation.speed())
}

/// The fraction of the way a spring released at rest has covered after `t` seconds (0 at the
/// start, 1 at the target; above 1 while an underdamped spring overshoots).
pub fn spring_progress(s: Spring, t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    let m = s.mass.max(1e-6);
    let k = s.stiffness.max(1e-6);
    let w0 = (k / m).sqrt();
    let zeta = s.damping.max(0.0) / (2.0 * (k * m).sqrt());
    let x = if (zeta - 1.0).abs() < 1e-3 {
        (1.0 + w0 * t) * (-w0 * t).exp()
    } else if zeta < 1.0 {
        let wd = w0 * (1.0 - zeta * zeta).sqrt();
        (-zeta * w0 * t).exp() * ((wd * t).cos() + zeta * w0 / wd * (wd * t).sin())
    } else {
        let r = (zeta * zeta - 1.0).sqrt();
        let (r1, r2) = (-w0 * (zeta - r), -w0 * (zeta + r));
        (r2 * (r1 * t).exp() - r1 * (r2 * t).exp()) / (r2 - r1)
    };
    1.0 - x
}

/// The longest motion considered (seconds).
const HORIZON: f64 = 10.0;
/// The time step of the searches below (seconds).
const STEP: f64 = 0.001;

/// When the spring first covers `fraction` of the way (seconds); [`HORIZON`] if never.
pub fn time_to_progress(s: Spring, fraction: f64) -> f64 {
    let mut t = 0.0;
    while t < HORIZON {
        if spring_progress(s, t) >= fraction {
            return t;
        }
        t += STEP;
    }
    HORIZON
}

/// When the spring stays within `tolerance` of its target for good (seconds).
pub fn settle_time(s: Spring, tolerance: f64) -> f64 {
    let mut last_out = 0.0;
    let mut t = 0.0;
    while t < HORIZON {
        if (1.0 - spring_progress(s, t)).abs() > tolerance {
            last_out = t;
        }
        t += STEP;
    }
    (last_out + STEP).min(HORIZON)
}

/// A snapshot's plane: its left edge's middle, its size before the turn, the turn about the
/// left edge (radians, the right side away) and its corner radius. Coordinates are the
/// overlay's (top-left origin).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub left_mid: Point,
    pub size: Size,
    pub tilt: f64,
    pub corner: f64,
}

impl Plane {
    /// A window's plane: flat, at its frame `r` (global), in an overlay whose top-left corner
    /// is at `origin` (global).
    pub fn window(r: Rect, origin: Point, corner: f64) -> Self {
        Self { left_mid: Point::new(r.x - origin.x, r.y + r.h / 2.0 - origin.y), size: r.size(), tilt: 0.0, corner }
    }

    /// A card thumbnail's plane: its left edge from (`x`, `top`) down `size.h` (overlay
    /// coordinates), turned by `tilt`.
    pub fn thumb(x: f64, top: f64, size: Size, tilt: f64, corner: f64) -> Self {
        Self { left_mid: Point::new(x, top + size.h / 2.0), size, tilt, corner }
    }

    /// A card-sized plane held under the pointer `p` at fractions `grab` of its drawn width and
    /// height (a dragged window shown as a card).
    pub fn under_pointer(p: Point, size: Size, grab: (f64, f64), tilt: f64, corner: f64) -> Self {
        let drawn_w = size.w * tilt.cos();
        Self {
            left_mid: Point::new(p.x - grab.0.clamp(0.0, 1.0) * drawn_w, p.y - (grab.1.clamp(0.0, 1.0) - 0.5) * size.h),
            size,
            tilt,
            corner,
        }
    }

    /// The same plane moved by (`dx`, `dy`).
    pub fn shifted(self, dx: f64, dy: f64) -> Self {
        Self { left_mid: Point::new(self.left_mid.x + dx, self.left_mid.y + dy), ..self }
    }
}

/// Where something released over the strip column goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropSpot {
    /// Into the gap above card `n` (`n` = the count: below the last card).
    Gap(usize),
    /// Onto card `n` (a merge; only with a merge band).
    OnCard(usize),
}

/// The spot for a release at height `y` over a column whose cards span `bands` (top, bottom),
/// top to bottom. `merge_band` is the fraction of a card's height around its middle that
/// counts as onto the card (0: never).
pub fn drop_spot(bands: &[(f64, f64)], y: f64, merge_band: f64) -> DropSpot {
    if merge_band > 0.0 {
        for (i, (top, bottom)) in bands.iter().enumerate() {
            let middle = (top + bottom) / 2.0;
            if (y - middle).abs() <= (bottom - top) * merge_band / 2.0 {
                return DropSpot::OnCard(i);
            }
        }
    }
    DropSpot::Gap(bands.iter().filter(|(top, bottom)| (top + bottom) / 2.0 < y).count())
}

/// One kept snapshot.
#[derive(Clone, Debug)]
struct Kept<T> {
    image: T,
    bytes: usize,
    taken: u64,
}

/// Full-size window snapshots under a memory budget: the oldest go first.
#[derive(Clone, Debug)]
pub struct SnapshotCache<T> {
    kept: HashMap<u32, Kept<T>>,
    budget: usize,
    clock: u64,
}

impl<T> SnapshotCache<T> {
    /// An empty cache holding at most `budget` bytes.
    pub fn new(budget: usize) -> Self {
        Self { kept: HashMap::new(), budget, clock: 0 }
    }

    /// Changes the budget (config reload).
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.trim();
    }

    /// Keeps `image` (`bytes` large) as window `id`'s newest snapshot.
    pub fn insert(&mut self, id: u32, image: T, bytes: usize) {
        self.clock += 1;
        self.kept.insert(id, Kept { image, bytes, taken: self.clock });
        self.trim();
    }

    /// Window `id`'s newest snapshot.
    pub fn get(&self, id: u32) -> Option<&T> {
        self.kept.get(&id).map(|k| &k.image)
    }

    /// Forgets every window `keep` refuses (closed, or no longer shown anywhere).
    pub fn retain(&mut self, keep: impl Fn(u32) -> bool) {
        self.kept.retain(|id, _| keep(*id));
    }

    /// Bytes held.
    pub fn bytes(&self) -> usize {
        self.kept.values().map(|k| k.bytes).sum()
    }

    /// How many snapshots are held.
    pub fn len(&self) -> usize {
        self.kept.len()
    }

    /// True when nothing is held.
    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }

    fn trim(&mut self) {
        while self.bytes() > self.budget {
            let Some(oldest) = self.kept.iter().min_by_key(|(_, k)| k.taken).map(|(id, _)| *id) else { break };
            self.kept.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apple's switch, measured (10-12, the laptop session at ~120 Hz): the median progress
    /// of 37 incoming windows (size, from card to frame) at these ms after the mouse-up. The
    /// motion starts one frame (~11 ms) after the mouse-up.
    const MEASURED: [(f64, f64); 8] =
        [(50.0, 0.132), (100.0, 0.401), (150.0, 0.657), (200.0, 0.815), (250.0, 0.914), (300.0, 0.959), (400.0, 0.991), (500.0, 0.998)];
    const START_MS: f64 = 11.0;

    #[test]
    fn apples_spring_matches_the_measured_curve() {
        let s = Spring::APPLE;
        for (ms, measured) in MEASURED {
            let ours = spring_progress(s, (ms - START_MS) / 1000.0);
            assert!((ours - measured).abs() < 0.035, "{ms} ms after the mouse-up: ours {ours:.3}, Apple's {measured}");
        }
        let t99 = time_to_progress(s, 0.99);
        assert!((0.38..=0.42).contains(&t99), "99 % at {t99}");
        let settle = settle_time(s, 0.001);
        assert!((0.45..0.65).contains(&settle), "settled at {settle}");
        // The spring before 10-12 (240/31, ω 15.5) lagged Apple's: 99 % ~30 ms later.
        let old = Spring { stiffness: 240.0, damping: 31.0, mass: 1.0 };
        assert!(time_to_progress(old, 0.99) - t99 > 0.02);
    }

    #[test]
    fn speed_scales_every_spring_on_the_same_curve() {
        let s = Spring::APPLE;
        let t = time_to_progress(s, 0.9);
        let twice = s.at_speed(2.0);
        assert!((time_to_progress(twice, 0.9) - t / 2.0).abs() < 0.002, "twice as fast");
        let half = s.at_speed(0.5);
        assert!((time_to_progress(half, 0.9) - t * 2.0).abs() < 0.004, "half as fast");
        assert!((0..1000).all(|i| spring_progress(twice, i as f64 / 1000.0) <= 1.0 + 1e-9), "still no overshoot");
        assert_eq!(s.at_speed(1.0), s);
        assert_eq!(s.at_speed(0.0), s, "speed 0 is the callers' to handle (no animation)");
        // The style's springs: each its own section, all at [animation] speed.
        let mut style = StripStyle::default();
        assert_eq!(switch_spring(&style), Spring::APPLE, "the default is Apple's");
        let glide = glide_spring(&style);
        let slide = slide_spring(&style);
        let omega = |s: Spring| (s.stiffness / s.mass).sqrt();
        assert!((omega(glide) - 17.0).abs() < 0.1 && (omega(slide) - 16.8).abs() < 0.1, "{} {}", omega(glide), omega(slide));
        style.animation.speed = 2.0;
        assert!((omega(switch_spring(&style)) - 2.0 * omega(Spring::APPLE)).abs() < 1e-9);
        assert!((omega(glide_spring(&style)) - 2.0 * omega(glide)).abs() < 1e-9);
        assert!((omega(slide_spring(&style)) - 2.0 * omega(slide)).abs() < 1e-9);
        assert!((style.animation.seconds(210) - 0.105).abs() < 1e-9, "durations too");
        style.animation.speed = 0.1;
        assert_eq!(style.animation.speed(), crate::config::MIN_SPEED, "the slowest is 0.25");
        style.animation.speed = 0.0;
        assert!(!style.animation.animates() && style.animation.seconds(210) == 0.0, "0 is no animation");
        style.animation.speed = 1.0;
        style.animation.enabled = false;
        assert!(!style.animation.animates() && style.animation.seconds(210) == 0.0, "enabled = false is no animation");
    }

    #[test]
    fn the_spring_never_overshoots_when_critically_damped_and_does_when_soft() {
        let s = Spring::APPLE;
        assert!((0..2000).all(|i| spring_progress(s, i as f64 / 1000.0) <= 1.0 + 1e-9));
        let soft = Spring { damping: 8.0, ..s };
        assert!((0..2000).any(|i| spring_progress(soft, i as f64 / 1000.0) > 1.05));
        let stiff = Spring { damping: 80.0, ..s };
        assert!(time_to_progress(stiff, 0.9) > time_to_progress(s, 0.9), "overdamped is slower");
        assert_eq!(spring_progress(s, 0.0), 0.0);
    }

    #[test]
    fn planes_put_a_window_at_its_frame_and_a_dragged_card_under_the_pointer() {
        let p = Plane::window(Rect::new(100.0, 80.0, 600.0, 400.0), Point::new(0.0, 0.0), 0.0);
        assert_eq!(p.left_mid, Point::new(100.0, 280.0));
        assert_eq!(p.size, Size::new(600.0, 400.0));
        let on_second = Plane::window(Rect::new(1900.0, 80.0, 600.0, 400.0), Point::new(1800.0, 0.0), 0.0);
        assert_eq!(on_second.left_mid.x, 100.0);
        let t = Plane::thumb(15.0, 300.0, Size::new(150.0, 100.0), 0.8, 4.0);
        assert_eq!(t.left_mid, Point::new(15.0, 350.0));
        let g = Plane::under_pointer(Point::new(100.0, 500.0), Size::new(160.0, 100.0), (0.5, 0.1), 0.0, 4.0);
        assert_eq!(g.left_mid, Point::new(20.0, 540.0), "the grab point stays under the pointer");
        assert_eq!(g.shifted(5.0, -5.0).left_mid, Point::new(25.0, 535.0));
    }

    #[test]
    fn a_drop_goes_into_the_gap_under_the_pointer() {
        let bands = [(100.0, 200.0), (230.0, 330.0), (360.0, 460.0)];
        assert_eq!(drop_spot(&bands, 50.0, 0.0), DropSpot::Gap(0), "above the first card");
        assert_eq!(drop_spot(&bands, 160.0, 0.0), DropSpot::Gap(1), "lower half of the first card");
        assert_eq!(drop_spot(&bands, 140.0, 0.0), DropSpot::Gap(0), "upper half of the first card");
        assert_eq!(drop_spot(&bands, 215.0, 0.0), DropSpot::Gap(1), "between the first two");
        assert_eq!(drop_spot(&bands, 900.0, 0.0), DropSpot::Gap(3), "below the last");
        assert_eq!(drop_spot(&[], 900.0, 0.0), DropSpot::Gap(0), "an empty strip");
        assert_eq!(drop_spot(&bands, 285.0, 0.4), DropSpot::OnCard(1), "a merge band, when there is one");
        assert_eq!(drop_spot(&bands, 240.0, 0.4), DropSpot::Gap(1));
    }

    #[test]
    fn the_cache_keeps_the_newest_snapshots_within_its_budget() {
        let mut c: SnapshotCache<&str> = SnapshotCache::new(100);
        c.insert(1, "a", 40);
        c.insert(2, "b", 40);
        assert_eq!((c.len(), c.bytes()), (2, 80));
        c.insert(1, "a2", 40);
        assert_eq!(c.get(1), Some(&"a2"), "a newer snapshot replaces the older");
        c.insert(3, "c", 40);
        assert_eq!(c.get(2), None, "the oldest went first");
        assert!(c.get(1).is_some() && c.get(3).is_some());
        c.retain(|id| id != 3);
        assert_eq!(c.len(), 1);
        c.set_budget(10);
        assert!(c.is_empty());
    }
}
