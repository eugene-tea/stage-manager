//! Motion math, pure: the spring's progress over time, the planes snapshots move between,
//! where a drop lands in the column, and the snapshot cache's bookkeeping.
//!
//! - **Spring.** Apple moves windows on a spring that starts at rest one frame after the
//!   mouse-up: mass 1, stiffness 272, damping 33 (critically damped, ω ≈ 16.5 rad/s: 50 % at
//!   ~100 ms, 90 % at ~235 ms, 99 % at ~400 ms from its start; fitted 10-12 to 37 windows of the
//!   laptop session; the record: `docs/UX-SPEC.md` as of commit 20665a3, "Transitions"); cards glide at ω ≈ 17.0 and the strip
//!   slides at ω ≈ 16.8. [`spring_progress`] gives the fraction of the way covered after `t`
//!   seconds for any mass, stiffness and damping; [`time_to_progress`] when a fraction is first
//!   reached, [`settle_time`] when it stays within a tolerance for good. `[animation] speed`
//!   scales every spring ([`Spring::at_speed`]: the same curve, ω times the speed) and
//!   duration; [`switch_spring`], [`glide_spring`] and [`slide_spring`] are the style's springs
//!   with it applied.
//! - **Unveil.** An incoming window takes its place behind its growing snapshot when the
//!   snapshot as shown is within [`UNVEIL_TOLERANCE`] of the window on every edge
//!   ([`unveil_time`]): by distance, from the transition's own start and end rectangles
//!   ([`travel`]), plus the [`PRESENTATION_LAG`] by which the snapshot on screen trails the
//!   clock; `[animation] unpark_at` is the floor. A fixed fraction placed the window 1 % of
//!   the travel short, and a frame or three before the snapshot showed there: a 25–31 pt jump
//!   on a window growing from its card (the owner's recording, 11-01). [`unveil_backstop`] is
//!   how long the controller holds incoming windows for the strip's unveil at that speed.
//!   A snapshot's spring runs until it rests within [`END_TOLERANCE`] ([`spring_duration`]),
//!   not Core Animation's settling time, which stopped it a point short and jumped.
//! - **Handover.** An incoming snapshot keeps its shadow until the window server shows its
//!   window at its frame ([`shows_at`]), then fades it from a frame on ([`shown_handover`]),
//!   never before a presentation lag past the unveil, and lands once it has handed over
//!   ([`shown_lands`]). Both shadows for a few frames read about 2 levels darker; a shadow
//!   faded before its window showed read 16 lighter for 80 ms (11-01).
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

/// How close the snapshot as shown is to its window, on every edge, when the window takes
/// its place behind it (points).
pub const UNVEIL_TOLERANCE: f64 = 1.0;
/// How far what is on screen trails the clock when the window takes its place (seconds): the
/// snapshot's frames in flight to the display, against the window placed through the
/// Accessibility API. About three frames at 60 Hz, measured from the owner's recording
/// (11-01: the window showed where the snapshot had been 50 ms before). Not scaled by the
/// display's refresh: the window's own path (the app, the window server) is not frame-bound,
/// and too long a lag only leaves the snapshot over its window a moment longer, still within
/// the tolerance.
pub const PRESENTATION_LAG: f64 = 0.05;
/// One display frame at 60 Hz (seconds).
pub const FRAME: f64 = 1.0 / 60.0;
/// How long an incoming snapshot's shadow takes to fade once its window shows (seconds):
/// three frames, both shadows meanwhile (darker by a little, never lighter).
pub const SHOWN_FADE: f64 = PRESENTATION_LAG;

/// Whether the window server shows a window at `frame`: listed on screen (`onscreen`; a
/// hidden app's window is not) with its `bounds` within [`UNVEIL_TOLERANCE`] of the frame on
/// every edge (a parked one sits in the corner).
pub fn shows_at(onscreen: bool, bounds: Rect, frame: Rect) -> bool {
    onscreen && travel(bounds, frame) <= UNVEIL_TOLERANCE
}

/// When an incoming snapshot's shadow starts to fade (seconds from now), its window seen on
/// screen at its frame by the window server now, `since_unveil` seconds after the unveil: a
/// frame on, as the window reaches the display at the compositor's next frame and the fade,
/// sent now, at its next one too (the two share the compositor's lag; a frame of both shadows
/// rather than one of neither); never before a [`PRESENTATION_LAG`] past the unveil, the
/// least the window took to show in the owner's recordings.
pub fn shown_handover(since_unveil: f64) -> f64 {
    FRAME.max(PRESENTATION_LAG - since_unveil)
}

/// How long from now the snapshot of a window seen on screen now ([`shown_handover`]) may
/// land (seconds): once its shadow has handed over.
pub fn shown_lands(since_unveil: f64) -> f64 {
    shown_handover(since_unveil) + SHOWN_FADE
}

/// The longest travel the unveil waits for (points): more than any display's diagonal.
const MAX_TRAVEL: f64 = 5000.0;
/// The unveil comes at least this long before the backstop (seconds).
const BACKSTOP_LEAD: f64 = 0.25;

/// How far from its end a snapshot's spring may stop (points): a tenth of a point, a fifth of
/// a pixel at 2x, so its last step is never seen.
pub const END_TOLERANCE: f64 = 0.1;

/// How long a spring animation of `travel` points runs (seconds): until the spring stays
/// within [`END_TOLERANCE`] of its end for good, so it comes to rest on the curve. Core
/// Animation's own `settlingDuration` (0.6 s for Apple's spring) stops it 0.055 % short and
/// puts it at its end in one frame: 1.1 pt on a 2000 pt travel, a snap and a halt in the
/// owner's recording (11-01). Nothing to travel: as short as the search allows.
pub fn spring_duration(s: Spring, travel: f64) -> f64 {
    if travel <= END_TOLERANCE {
        return STEP;
    }
    settle_time(s, END_TOLERANCE / travel.min(MAX_TRAVEL))
}

/// The farthest any edge of `start` is from the same edge of `end` (points).
pub fn travel(start: Rect, end: Rect) -> f64 {
    [start.x - end.x, start.y - end.y, start.max_x() - end.max_x(), start.max_y() - end.max_y()]
        .into_iter()
        .fold(0.0, |m, d| m.max(d.abs()))
}

/// When the strip unveils a switch's incoming windows whose snapshots travel `travel` points
/// at most (seconds after the motion starts): once the switch spring stays within
/// [`UNVEIL_TOLERANCE`] of the end for good, and not before `[animation] unpark_at` of the
/// way, each as shown on screen ([`PRESENTATION_LAG`] later); never past the backstop less a
/// lead ([`unveil_backstop`]).
pub fn unveil_time(style: &StripStyle, travel: f64) -> f64 {
    unveil_at(style, travel).min(unveil_backstop(style).as_secs_f64() - BACKSTOP_LEAD)
}

/// [`unveil_time`] before the backstop's bound.
fn unveil_at(style: &StripStyle, travel: f64) -> f64 {
    let s = switch_spring(style);
    let floor = time_to_progress(s, style.animation.unpark_at);
    let near = if travel > UNVEIL_TOLERANCE { settle_time(s, UNVEIL_TOLERANCE / travel.min(MAX_TRAVEL)) } else { 0.0 };
    floor.max(near) + PRESENTATION_LAG
}

/// The latest the strip unveils, whatever the travel (seconds).
pub fn unveil_latest(style: &StripStyle) -> f64 {
    unveil_time(style, MAX_TRAVEL)
}

/// The shortest backstop for incoming windows the strip never unveils.
pub const UNVEIL_BACKSTOP_MIN: std::time::Duration = std::time::Duration::from_millis(1500);
/// The longest: a slow spring (a low speed, a soft spring) never holds windows longer.
pub const UNVEIL_BACKSTOP_MAX: std::time::Duration = std::time::Duration::from_millis(3000);
/// How long after the latest unveil the backstop waits.
const UNVEIL_MARGIN: f64 = 0.5;

/// How long incoming windows may wait for the strip's unveil before they come back anyway:
/// the latest unveil (the longest travel) plus a margin, kept within
/// [`UNVEIL_BACKSTOP_MIN`]..[`UNVEIL_BACKSTOP_MAX`].
pub fn unveil_backstop(style: &StripStyle) -> std::time::Duration {
    let at = std::time::Duration::from_secs_f64((unveil_at(style, MAX_TRAVEL) + UNVEIL_MARGIN).clamp(0.0, HORIZON));
    at.clamp(UNVEIL_BACKSTOP_MIN, UNVEIL_BACKSTOP_MAX)
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

    /// The windows a snapshot is held of.
    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.kept.keys().copied()
    }

    fn trim(&mut self) {
        while self.bytes() > self.budget {
            let Some(oldest) = self.kept.iter().min_by_key(|(_, k)| k.taken).map(|(id, _)| *id) else { break };
            self.kept.remove(&oldest);
        }
    }
}

/// Where a card's content starts a glide so the card is drawn exactly where it is now, when
/// its hover scale `s` (`[cards] hover_scale`, about the middle of its slot) moves with the
/// slot from `c_old` to `c_new`; `now` is the content's offset drawn now. The scale moves
/// every point by `(1 - s)·(c - x)`: with the centre moved and the offset kept, a hovered
/// card dropped `d` from its slot would jump `(s - 1)·d` further away before it glides back,
/// past the slot it is headed for (round 9C).
pub fn rescaled_start(now: Point, s: f64, c_old: Point, c_new: Point) -> Point {
    if s == 1.0 || s <= 0.0 {
        return now;
    }
    let k = (1.0 - s) / s;
    Point::new(now.x + k * (c_old.x - c_new.x), now.y + k * (c_old.y - c_new.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 11-01: the snapshot's shadow faded 50 ms before the unveil and its window showed a
    /// frame after the fade ended: 16 levels lighter for ~80 ms. Now it fades once the window
    /// server shows the window, never before: never lighter than the window's own shadow.
    #[test]
    fn the_shadow_hands_over_once_the_window_shows() {
        let frame = Rect::new(813.0, 215.0, 1217.0, 790.0);
        assert!(shows_at(true, Rect::new(813.4, 215.0, 1217.0, 790.0), frame));
        assert!(!shows_at(true, Rect::new(2559.0, 1408.0, 1217.0, 790.0), frame), "parked in the corner");
        assert!(!shows_at(false, frame, frame), "a hidden app's window, in place");
        for since in [-0.01, 0.0, 0.006, 0.03, 0.2] {
            let begin = shown_handover(since);
            assert!(begin >= FRAME - 1e-12, "a frame on at least: {since} {begin}");
            assert!(since + begin >= PRESENTATION_LAG - 1e-12, "not before a lag past the unveil: {since} {begin}");
            assert!((shown_lands(since) - begin - SHOWN_FADE).abs() < 1e-12);
            // The window shows now (0 on the shared clock); the alpha of a point of its shadow.
            for a in [0.1, 0.4, 0.8] {
                for i in 0..40 {
                    let t = i as f64 * 0.005;
                    let e = crate::ui::shadow_math::handover_error(a, t, 0.0, begin, SHOWN_FADE);
                    assert!(e >= -1e-12, "lighter than the window's own at {t}: {e}");
                }
            }
        }
    }

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
    fn the_unveil_backstop_comes_after_the_unveil_at_every_speed() {
        let mut style = StripStyle::default();
        for speed in [crate::config::MIN_SPEED, 0.26, 0.3, 0.5, 1.0, 2.0, 4.0] {
            style.animation.speed = speed;
            let latest = unveil_latest(&style);
            let backstop = unveil_backstop(&style).as_secs_f64();
            assert!(backstop >= latest + BACKSTOP_LEAD - 1e-9, "speed {speed}: unveil by {latest:.3} s, backstop at {backstop:.3} s");
            assert!(backstop >= UNVEIL_BACKSTOP_MIN.as_secs_f64() && backstop <= UNVEIL_BACKSTOP_MAX.as_secs_f64());
            for travel in [0.0, 10.0, 1200.0, 1e6] {
                assert!(unveil_time(&style, travel) <= latest + 1e-9, "speed {speed}, {travel} pt");
            }
        }
        // The slowest speed unveils after 1.5 s (the constant cap before R3 fired first).
        style.animation.speed = crate::config::MIN_SPEED;
        assert!(unveil_time(&style, 1200.0) > 1.5, "{}", unveil_time(&style, 1200.0));
        // A spring that never gets there is still bounded, and unveils before the backstop.
        style.animation.speed = 1.0;
        style.animation.spring_stiffness = 0.5;
        assert_eq!(unveil_backstop(&style), UNVEIL_BACKSTOP_MAX);
        assert!((unveil_time(&style, 1200.0) - (UNVEIL_BACKSTOP_MAX.as_secs_f64() - BACKSTOP_LEAD)).abs() < 1e-9);
        style.animation.spring_stiffness = 1e6;
        assert_eq!(unveil_backstop(&style), UNVEIL_BACKSTOP_MIN);
    }

    /// The snapshot on screen at the unveil: where the spring had it a presentation lag before.
    fn shown_gap(style: &StripStyle, travel: f64) -> f64 {
        let t = unveil_time(style, travel) - PRESENTATION_LAG;
        (1.0 - spring_progress(switch_spring(style), t)) * travel
    }

    #[test]
    fn the_window_takes_its_place_within_a_point_of_its_snapshot_as_shown() {
        let style = StripStyle::default();
        // A window growing from its card across the screen (the owner's 11-01 recording: a
        // 1200 pt edge travel jumped 25–31 pt at 99 %, the snapshot shown 50 ms late).
        let long = unveil_time(&style, 1200.0);
        assert!(shown_gap(&style, 1200.0) <= UNVEIL_TOLERANCE + 1e-6, "{}", shown_gap(&style, 1200.0));
        assert!((0.60..0.65).contains(&long), "a long travel unveils at {long:.3} s");
        let old = time_to_progress(Spring::APPLE, 0.99);
        let old_gap = (1.0 - spring_progress(Spring::APPLE, old - PRESENTATION_LAG)) * 1200.0;
        assert!(old_gap > 20.0, "the 99 % unveil jumped {old_gap:.1} pt");
        // A short one waits for the floor (99 % as shown), not for the distance.
        let short = unveil_time(&style, 40.0);
        assert!((short - (time_to_progress(Spring::APPLE, 0.99) + PRESENTATION_LAG)).abs() < 1e-9, "{short}");
        assert!(shown_gap(&style, 40.0) <= UNVEIL_TOLERANCE);
        assert!(short < long);
        // Longer travels wait longer, each within the tolerance.
        let mut last = 0.0;
        for travel in [100.0, 300.0, 600.0, 1200.0, 2400.0, 4000.0] {
            let t = unveil_time(&style, travel);
            assert!(t >= last, "{travel} pt at {t:.3} s");
            assert!(shown_gap(&style, travel) <= UNVEIL_TOLERANCE + 1e-6, "{travel} pt: {}", shown_gap(&style, travel));
            last = t;
        }
        // Nothing to travel: the floor still holds.
        assert_eq!(unveil_time(&style, 0.0), short);
    }

    #[test]
    fn a_slow_speed_unveils_by_distance_too_and_a_floor_can_wait_longer() {
        let mut style = StripStyle::default();
        style.animation.speed = 0.5;
        let t = unveil_time(&style, 1200.0);
        // Twice the spring's time, the presentation lag unscaled.
        let at_one = unveil_time(&StripStyle::default(), 1200.0);
        assert!((t - (2.0 * (at_one - PRESENTATION_LAG) + PRESENTATION_LAG)).abs() < 0.003, "{t} vs {at_one}");
        assert!(shown_gap(&style, 1200.0) <= UNVEIL_TOLERANCE + 1e-6);
        // unpark_at is a floor: set near 1 it waits past the distance's moment.
        style.animation.speed = 1.0;
        style.animation.unpark_at = 0.99999;
        assert!(unveil_time(&style, 300.0) > unveil_time(&StripStyle::default(), 300.0) + 0.1);
        // An overshooting spring waits until it stays within the tolerance.
        let mut soft = StripStyle::default();
        soft.animation.spring_damping = 12.0;
        let t = unveil_time(&soft, 1200.0) - PRESENTATION_LAG;
        let s = switch_spring(&soft);
        assert!((0..2000).all(|i| ((1.0 - spring_progress(s, t + i as f64 / 1000.0)) * 1200.0).abs() <= UNVEIL_TOLERANCE + 1e-6));
    }

    /// 11-01, switch 1: the snapshot moved 0–2 px a frame, then 3 px in one frame, then
    /// stopped dead. Core Animation's settling duration for Apple's spring is 0.6 s, where
    /// 0.055 % of the way is left: 1.1 pt of a 1999 pt travel. Ours runs on until a tenth of a
    /// point is left, its last frame's motion under a frame's motion before it.
    #[test]
    fn a_snapshot_s_spring_runs_until_its_last_step_is_unseen() {
        let s = Spring::APPLE;
        let ca_settle = 0.6;
        let left_at_ca = (1.0 - spring_progress(s, ca_settle)) * 1999.0;
        assert!((1.0..1.2).contains(&left_at_ca), "Core Animation's cut leaves {left_at_ca:.2} pt");
        let d = spring_duration(s, 1999.0);
        let left = (1.0 - spring_progress(s, d)) * 1999.0;
        assert!(left <= END_TOLERANCE + 1e-9, "{left}");
        assert!((0.7..0.8).contains(&d), "{d}");
        // Its last frames move a fraction of a pixel at 2x, so the stop is not seen; at Core
        // Animation's cut the last frame still moved a third of a point, then jumped 1.1.
        let frame = 1.0 / 60.0;
        let moved = |t: f64| (spring_progress(s, t) - spring_progress(s, t - frame)) * 1999.0;
        assert!(moved(d) < 0.05, "the last frame moved {:.3} pt", moved(d));
        assert!(moved(ca_settle) > 0.2 && left_at_ca > 3.0 * moved(ca_settle), "{}", moved(ca_settle));
        // Shorter travels stop sooner, never before Core Animation's for a long one.
        assert!(spring_duration(s, 100.0) < d && spring_duration(s, 100.0) > 0.4);
        assert_eq!(spring_duration(s, 0.0), STEP);
        // It ends before the snapshot lands (the unveil plus 120 ms).
        let style = StripStyle::default();
        assert!(d <= unveil_time(&style, 1999.0) + 0.12, "{d}");
        assert!(spring_duration(s, 1e6) < HORIZON);
    }

    #[test]
    fn travel_is_the_farthest_edge() {
        let end = Rect::new(100.0, 100.0, 800.0, 600.0);
        assert_eq!(travel(end, end), 0.0);
        // A card at the left growing to the window: the right edge travels farthest.
        assert_eq!(travel(Rect::new(20.0, 300.0, 120.0, 90.0), end), 760.0);
        assert_eq!(travel(Rect::new(100.0, 90.0, 800.0, 600.0), end), 10.0);
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
        assert_eq!(c.ids().collect::<Vec<_>>(), [1]);
        c.set_budget(10);
        assert!(c.is_empty());
    }

    #[test]
    fn a_hovered_card_whose_slot_moves_is_drawn_where_it_was() {
        // Content point x drawn under a scale s about c: c + s·(x - c).
        let drawn = |x: f64, s: f64, c: f64| c + s * (x - c);
        let (built, s) = (100.0, 1.06);
        // Dropped 30 pt below its slot: the scale was about the card where it followed the
        // pointer, and now is about the slot.
        let (c_old, c_new, now) = (160.0, 130.0, 30.0);
        let before = drawn(built + 50.0 + now, s, c_old);
        let from = rescaled_start(Point::new(0.0, now), s, Point::new(0.0, c_old), Point::new(0.0, c_new));
        let after = drawn(built + 50.0 + from.y, s, c_new);
        assert!((before - after).abs() < 1e-9, "{before} vs {after}");
        // Without it the card would jump (s - 1)·30 = 1.8 pt past where it was, away from the slot.
        let unscaled = drawn(built + 50.0 + now, s, c_new);
        assert!((unscaled - before - 1.8).abs() < 1e-9);
        // At rest size nothing changes.
        assert_eq!(rescaled_start(Point::new(3.0, 4.0), 1.0, Point::new(0.0, 9.0), Point::new(0.0, 0.0)), Point::new(3.0, 4.0));
    }
}
