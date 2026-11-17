//! The strip brought back at the left edge, pure: its cards flat, and the turn coming back as
//! it hides (Apple's, recorded 11-16 on a 2560 × 1440 pt display, three rounds, the cards'
//! bounds read every 8 to 30 ms: `scripts/measure-edge-reveal.py`).
//!
//! - **In.** While a window over its column hides the strip, Apple's cards wait off the
//!   screen turned as in the strip (their left edges at −260 pt). Once the pointer rests at
//!   the left edge they turn flat at once, still off the screen, facing the viewer at the size
//!   the strip draws them before the turn (a 900 × 668 pt window 130 × 97 pt, its own
//!   proportions), and slide in flat on the strip's slide spring (ω 15.7 to 16.2 read; ours
//!   16.8), their places down the column unchanged. Ours draws the strip with a flat
//!   [`Metrics`](crate::ui::strip_layout::Metrics) (no turn) from that moment.
//! - **Out.** When it hides again, each card slides out turning back as it goes: the
//!   window's pose, seen through the flight's eye ([`crate::ui::motion_math::Flight`]: the
//!   eye in front of the display's middle that every switch's snapshot is seen through), goes
//!   from the flat card to the turned card off the screen, its size, turn and height on the
//!   slide's spring, its sideways place trailing it by [`HIDE_SIDEWAYS_TRAIL`] of `p (1 − p)`
//!   ([`hide_pose`]). Seen so, the card first widens (a 130 pt card 139 to 140 pt wide while
//!   it starts to turn, as its far side goes back toward the eye's middle) and then narrows
//!   to the turned card, 94 pt. That draws the recorded cards within 0.6 pt rms (2.6 at worst)
//!   at their best progress; a turn about the left edge on the slide seen through the
//!   strip's own eye misses them by 7.2 pt rms (18 at worst).
//! - **Drawn.** The strip draws each card's planes through its own eye, slid in 3D; a pose
//!   seen through the flight's eye is a trapezoid with upright sides, which one plane turned
//!   about its left edge draws exactly through the strip's eye ([`Pose::plane`]). So the hide
//!   is a keyframed transform on each plane ([`hide_keyframes`], [`layer_parts`]) under the
//!   strip's usual slide, ending on the turned plane the strip draws at rest.

use crate::engine::geometry::{Point, Size};
use crate::ui::motion_math::{Eye, Plane, Pose, Spring, spring_progress};

/// How far the pose's sideways place trails the slide's spring as a revealed strip hides, as
/// the `k` of `p − k·p(1 − p)` ([`hide_pose`]): fitted to the three cards of two recorded
/// hides (0.26 to 0.33 by fit; size and turn on the spring itself).
pub const HIDE_SIDEWAYS_TRAIL: f64 = 0.3;

/// How many poses a second the hide is drawn from (Core Animation goes straight between
/// them; as the switch's flight).
pub const HIDE_SAMPLES_PER_SECOND: f64 = 120.0;

fn lerp(a: f64, b: f64, p: f64) -> f64 {
    a + (b - a) * p
}

/// Whether the cards turn flat as the strip's visibility changes from `was_shown` to `shown`:
/// only a strip coming into view, brought back at the edge (`by_edge`: the pointer's rest, the
/// edge tab, the launch announce; not a window dragged over it), with `[reveal] flat` on. The
/// strip turns them flat before its slide starts, both in one commit.
pub fn turns_flat(was_shown: bool, shown: bool, by_edge: bool, flat_on: bool) -> bool {
    !was_shown && shown && by_edge && flat_on
}

/// The pose of a revealed card's window when the hide's spring has covered `p` of the way,
/// from `flat` (the card as revealed) to `turned` (the card off the screen, turned), both
/// seen through the flight's eye: size, turn and height on the spring, the sideways place
/// [`HIDE_SIDEWAYS_TRAIL`] behind it.
pub fn hide_pose(flat: Pose, turned: Pose, p: f64) -> Pose {
    let side = p - HIDE_SIDEWAYS_TRAIL * p * (1.0 - p);
    Pose {
        middle: Point::new(lerp(flat.middle.x, turned.middle.x, side), lerp(flat.middle.y, turned.middle.y, p)),
        size: Size::new(lerp(flat.size.w, turned.size.w, p), lerp(flat.size.h, turned.size.h, p)),
        tilt: lerp(flat.tilt, turned.tilt, p),
        corner: lerp(flat.corner, turned.corner, p),
    }
}

/// The plane the strip's eye draws a hiding card's window as at `p` ([`hide_pose`]): `flat`
/// is the card as revealed, `turned` the card where the hide ends (both as drawn, the slide
/// included), seen through `strip`; the pose between them through `flight`. Exactly `flat`
/// at 0 and `turned` at 1.
pub fn hide_plane(flat: Plane, turned: Plane, strip: Eye, flight: Eye, p: f64) -> Plane {
    if p <= 0.0 {
        return flat;
    }
    if p >= 1.0 {
        return turned;
    }
    let pose = hide_pose(flat.pose(strip, flight), turned.pose(strip, flight), p);
    let mut plane = pose.plane(flight, strip);
    plane.corner = lerp(flat.corner, turned.corner, p);
    plane
}

/// The planes a revealed card's window is drawn as while the strip hides, in the slid space's
/// own coordinates (the strip's slide, a move in 3D before the perspective, adds `dx`), as
/// (fraction of `duration`, plane), evenly spaced at [`HIDE_SAMPLES_PER_SECOND`] or finer.
/// `base` is the window's plane as laid out on its card, flat; `tilt` the strip's turn; the
/// slide goes from `from_dx` to `to_dx` on `s` over `duration` seconds. The first is `base`,
/// the last `base` turned by `tilt`: what the strip draws at rest, hidden.
pub fn hide_keyframes(base: Plane, tilt: f64, from_dx: f64, to_dx: f64, eyes: (Eye, Eye), s: Spring, duration: f64) -> Vec<(f64, Plane)> {
    let (strip, flight) = eyes;
    let flat = Plane { tilt: 0.0, ..base }.shifted(from_dx, 0.0);
    let turned = Plane { tilt, ..base }.shifted(to_dx, 0.0);
    let n = ((duration.max(0.0) * HIDE_SAMPLES_PER_SECOND).ceil() as usize).clamp(1, 2400);
    (0..=n)
        .map(|i| {
            let f = i as f64 / n as f64;
            if i == n {
                return (f, Plane { tilt, ..base });
            }
            let p = spring_progress(s, f * duration);
            let dx = lerp(from_dx, to_dx, p);
            (f, hide_plane(flat, turned, strip, flight, p).shifted(-dx, 0.0))
        })
        .collect()
}

/// How a plane layer laid out as `base` (its bounds `base.size`, anchored at its left edge's
/// middle at `base.left_mid`) is transformed to draw as `target`, in the order applied: a
/// scale (across, down), a turn about its left edge (radians), then a move (`dx`, `dy`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerParts {
    pub scale: (f64, f64),
    pub tilt: f64,
    pub shift: (f64, f64),
}

/// The [`LayerParts`] that draw a layer laid out as `base` as `target`.
pub fn layer_parts(base: &Plane, target: &Plane) -> LayerParts {
    LayerParts {
        scale: (target.size.w / base.size.w.max(1e-9), target.size.h / base.size.h.max(1e-9)),
        tilt: target.tilt,
        shift: (target.left_mid.x - base.left_mid.x, target.left_mid.y - base.left_mid.y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Rect;
    use crate::ui::motion_math::Flight;

    /// The recording's display: 2560 × 1440 pt, the strip's eye at (15, 720) 2260 pt away and
    /// its turn 47.9° (Apple's, measured on a 5K display), the flight's eye at the middle.
    fn eyes() -> (Eye, Eye) {
        let strip = Eye { at: Point::new(15.0, 720.0), distance: 2260.0 };
        (strip, Flight::over(0.0, Size::new(2560.0, 1440.0), 720.0).eye)
    }

    const TILT: f64 = 47.9 * std::f64::consts::PI / 180.0;

    /// The three fixture windows (900 × 668, 700 × 528, 1000 × 728 pt with their title bars)
    /// as the strip draws them before the turn, 97 pt tall, and their tops.
    fn cards() -> [(f64, f64); 3] {
        [(97.0 * 900.0 / 668.0, 885.0), (97.0 * 700.0 / 528.0, 759.0), (97.0 * 1000.0 / 728.0, 633.0)]
    }

    fn plane(x: f64, u: f64, top: f64, tilt: f64) -> Plane {
        Plane::thumb(x, top, Size::new(u, 97.0), tilt, 0.0)
    }

    /// Apple's hide, recorded 11-16: the three cards' bounds (x, y, w, h) at a few frames.
    const HIDE: [[[f64; 4]; 3]; 6] = [
        [[-40.0, 883.0, 139.0, 99.0], [-40.0, 758.0, 136.0, 98.0], [-40.0, 633.0, 141.0, 97.0]],
        [[-95.0, 881.0, 139.0, 101.0], [-95.0, 758.0, 137.0, 98.0], [-95.0, 633.0, 142.0, 97.0]],
        [[-143.0, 880.0, 133.0, 102.0], [-143.0, 758.0, 131.0, 98.0], [-143.0, 633.0, 136.0, 97.0]],
        [[-194.0, 879.0, 120.0, 103.0], [-194.0, 757.0, 118.0, 99.0], [-194.0, 633.0, 122.0, 97.0]],
        [[-225.0, 878.0, 109.0, 104.0], [-225.0, 757.0, 107.0, 99.0], [-225.0, 633.0, 111.0, 97.0]],
        [[-245.0, 878.0, 100.0, 104.0], [-245.0, 757.0, 99.0, 99.0], [-245.0, 633.0, 102.0, 97.0]],
    ];

    /// The worst miss of a frame's three cards drawn by `draw` at its best progress.
    fn best_miss(frame: &[[f64; 4]; 3], draw: impl Fn(f64, f64, f64) -> Rect) -> f64 {
        (0..=1000)
            .map(|i| {
                let p = i as f64 / 1000.0;
                cards()
                    .iter()
                    .zip(frame)
                    .map(|(&(u, top), b)| {
                        let r = draw(p, u, top);
                        [r.x - b[0], r.y - b[1], r.w - b[2], r.h - b[3]].iter().fold(0.0_f64, |m, e| m.max(e.abs()))
                    })
                    .fold(0.0_f64, f64::max)
            })
            .fold(f64::INFINITY, f64::min)
    }

    #[test]
    fn the_hide_draws_apples_recorded_cards() {
        let (strip, flight) = eyes();
        for frame in &HIDE {
            let ours = best_miss(frame, |p, u, top| hide_plane(plane(16.0, u, top, 0.0), plane(-260.0, u, top, TILT), strip, flight, p).drawn(strip).bounds());
            assert!(ours < 2.7, "{frame:?}: {ours}");
        }
        // A turn about the left edge on the slide, seen through the strip's eye, cannot widen
        // the card past its flat width: it misses the first frames by far more.
        let plain = best_miss(&HIDE[1], |p, u, top| plane(16.0 - 276.0 * p, u, top, TILT * p).drawn(strip).bounds());
        assert!(plain > 6.0, "{plain}");
    }

    #[test]
    fn the_card_widens_as_it_starts_to_turn_then_narrows() {
        let (strip, flight) = eyes();
        let (u, top) = cards()[0];
        let (flat, turned) = (plane(16.0, u, top, 0.0), plane(-260.0, u, top, TILT));
        let w = |p: f64| hide_plane(flat, turned, strip, flight, p).drawn(strip).bounds().w;
        assert!((w(0.0) - u).abs() < 1e-9);
        assert!(w(0.25) > u + 5.0, "{}", w(0.25));
        assert!((w(1.0) - turned.drawn(strip).bounds().w).abs() < 1e-9);
        assert!(w(1.0) < 96.0 && w(1.0) > 93.0, "Apple's turned card off the screen: 94 pt");
    }

    #[test]
    fn the_ends_are_the_flat_card_and_the_turned_one() {
        let (strip, flight) = eyes();
        let (flat, turned) = (plane(16.0, 130.0, 885.0, 0.0), plane(-271.0, 130.0, 885.0, TILT));
        assert_eq!(hide_plane(flat, turned, strip, flight, 0.0), flat);
        assert_eq!(hide_plane(flat, turned, strip, flight, 1.0), turned);
        // Just inside the ends the plane is next to them (no jump on the first or last frame).
        let near = hide_plane(flat, turned, strip, flight, 1e-6).drawn(strip).array();
        for (a, b) in near.iter().zip(flat.drawn(strip).array()) {
            assert!((a - b).abs() < 1e-3);
        }
        let near = hide_plane(flat, turned, strip, flight, 1.0 - 1e-6).drawn(strip).array();
        for (a, b) in near.iter().zip(turned.drawn(strip).array()) {
            assert!((a - b).abs() < 1e-3);
        }
    }

    #[test]
    fn the_keyframes_run_from_the_flat_card_to_the_turned_one_under_the_slide() {
        let (strip, flight) = eyes();
        let s = Spring { stiffness: 282.0, damping: 33.6, mass: 1.0 };
        let base = plane(15.0, 130.0, 885.0, 0.0);
        let keys = hide_keyframes(base, TILT, 0.0, -286.0, (strip, flight), s, 0.42);
        assert_eq!(keys.first().map(|k| k.1), Some(base));
        assert_eq!(keys.last().map(|k| k.1), Some(Plane { tilt: TILT, ..base }));
        assert!(keys.len() >= 50 && keys.windows(2).all(|w| w[1].0 > w[0].0));
        // Each, slid as the strip is at that moment, is the hide's plane.
        for &(f, k) in &keys[1..keys.len() - 1] {
            let p = spring_progress(s, f * 0.42);
            let want = hide_plane(base, Plane { tilt: TILT, ..base }.shifted(-286.0, 0.0), strip, flight, p);
            let got = k.shifted(-286.0 * p, 0.0);
            assert!((got.left_mid.x - want.left_mid.x).abs() < 1e-9 && (got.size.w - want.size.w).abs() < 1e-9 && (got.tilt - want.tilt).abs() < 1e-12);
        }
        // A hide that starts while the strip still slides in starts where it is drawn.
        let mid = hide_keyframes(base, TILT, -100.0, -286.0, (strip, flight), s, 0.42);
        assert_eq!(mid.first().map(|k| k.1), Some(base));
    }

    #[test]
    fn only_a_strip_coming_into_view_at_the_edge_turns_flat() {
        assert!(turns_flat(false, true, true, true));
        assert!(!turns_flat(true, true, true, true), "already in view (a card click's carry-over): stays turned");
        assert!(!turns_flat(false, true, false, true), "shown for a window dragged over it: turned");
        assert!(!turns_flat(false, true, true, false), "[reveal] flat = false");
        assert!(!turns_flat(true, false, true, true), "hiding");
    }

    #[test]
    fn layer_parts_take_the_laid_out_plane_to_the_target() {
        let base = plane(15.0, 130.0, 885.0, 0.0);
        let target = Plane { left_mid: Point::new(-40.0, 930.0), size: Size::new(150.0, 95.0), tilt: 0.3, corner: 0.0 };
        let l = layer_parts(&base, &target);
        assert!((base.size.w * l.scale.0 - target.size.w).abs() < 1e-9 && (base.size.h * l.scale.1 - target.size.h).abs() < 1e-9);
        assert_eq!(l.tilt, 0.3);
        assert!((base.left_mid.x + l.shift.0 - target.left_mid.x).abs() < 1e-9 && (base.left_mid.y + l.shift.1 - target.left_mid.y).abs() < 1e-9);
        assert_eq!(layer_parts(&base, &base), LayerParts { scale: (1.0, 1.0), tilt: 0.0, shift: (0.0, 0.0) });
    }
}
