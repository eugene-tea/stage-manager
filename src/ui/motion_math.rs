//! Motion math, pure: the spring's progress over time, the planes snapshots move between,
//! where a drop lands in the column, and the snapshot cache's bookkeeping.
//!
//! - **Springs.** Apple moves windows on one critically damped spring that starts at rest,
//!   both ways from the same moment: stiffness 289, damping 34 (ω = 17 rad/s: 50 % at ~99 ms,
//!   90 % at ~229 ms, 99 % at ~390 ms), fitted 11-09 to the four corners of a window growing
//!   out of its card and of one shrinking into its card, frame by frame on a 5K screen at 60
//!   fps, together with the path below ([`Path`]): the fixture's 1100 pt window, then its
//!   1800 pt one, three runs each way, which read ω 16.8 growing and 17.3 shrinking. Cards
//!   glide at ω ≈ 17.0, the strip slides at ω ≈ 16.8 (Apple's slide-in read ω 16.3 on 11-09),
//!   and a card under the pointer grows on ω ≈ 16.6 ([`hover_spring`]). [`spring_progress`]
//!   gives the fraction of the way covered after `t` seconds for any mass, stiffness and
//!   damping; [`time_to_progress`] when a fraction is first reached, [`settle_time`] when it
//!   stays within a tolerance for good. `[animation] speed` scales every spring
//!   ([`Spring::at_speed`]: the same curve, ω times the speed) and duration;
//!   [`switch_spring`], [`shrink_spring`], [`glide_spring`] and [`slide_spring`] are the
//!   style's springs with it applied.
//! - **Flight.** Apple's snapshot in flight is its window's picture, never stretched, turned
//!   about its vertical middle and seen through an eye in front of the display's middle,
//!   [`FLIGHT_DISTANCE`] of its width away (3000 pt on the 5K display; [`Flight`]): the
//!   1800 × 1100 pt window, its corners tracked frame by frame to a tenth of a point, fits
//!   that rigid plane within 0.16 pt rms growing and 0.43 shrinking, where the strip's eye
//!   (15, 720 at 2260 pt) misses by 6.1 and 4.1 (read through it, the same frames are up to
//!   14 % wider than the window). Seen so, its card is the window far back
//!   (0.12 of its size: 21,700 pt behind the screen), turned 76°.
//! - **Path.** Its size (its depth), turn and height run on the spring together; its
//!   sideways place runs ahead toward the window's end ([`Path::at`]). Growing, it is out over
//!   the desktop while still small and far back, then comes forward to the viewer; shrinking,
//!   it shrinks back in place first, then slides into the card. Apple scales the sideways
//!   place about a point left of the screen ([`SCALE_ORIGIN`]), which makes the lead depend on
//!   the window's place and the card's size ([`Path::sideways_lead`]): 0.75 of p·(1 − p) for
//!   the 1800 pt window from slot 5, 0.3 for the 1100 pt one, both from that one origin
//!   (each set alone fits it within −200 to −228 pt). Against Apple's corners, on 289/34:
//!   5.3 pt rms (22 at worst) growing and 6.0 (17) shrinking for the 1800 pt window, 6.1 (24)
//!   and 5.3 (17) for the 1100 pt one, where the motion-path model before it (the strip's
//!   eye; the middle's x and the width 0.33 ahead, the turn 0.19 ahead, the height on the
//!   spring: it stretched the window up to 16 % wide while it flew) read 26.5 (118) and 27.2
//!   (135) on the 1800 pt window (3.4 and 3.5 on the 1100 pt one it was fitted to).
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
//!   window at its frame ([`shows_at`]), then drops it at once, in one step: the window and
//!   the change reach the display on about the same compositor frame. A shadow dropped before
//!   its window showed read 16 levels lighter for 80 ms (11-01); one faded over three frames
//!   from a presentation lag past the unveil overlapped the window's own for four frames, up
//!   to 12 levels darker within 45 pt of the edge (11-09, the owner's "shadow wobble" as the
//!   window lands). Measured 11-09 at 60 fps (VS Code, Safari, Finder, frames on the log's
//!   clock): dropped in a step when the window is listed, no frame lighter and at most one
//!   frame of both, in three landings of five none.
//! - **Planes.** Callers give a snapshot as a plane turned about its left edge, like a strip
//!   card, seen through the strip's eye: at a window's frame it is flat and full size; on a
//!   card it is the card's thumbnail, turned by the strip's tilt. In flight it is the pose
//!   the flight's eye draws exactly as the strip's eye draws the plane ([`Plane::pose`], from
//!   the drawn trapezoid, [`Drawn`]), so it leaves and lands on a card with no change on
//!   screen, and is read back the same way ([`Pose::plane`]).
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
    /// Apple's windows growing out of their card, measured 11-09 with the path
    /// (`[animation] spring_*`).
    pub const APPLE: Spring = Spring { stiffness: 289.0, damping: 34.0, mass: 1.0 };
    /// Apple's windows shrinking into their card (`[animation] shrink_*`): the same spring,
    /// measured the same way.
    pub const APPLE_SHRINK: Spring = Spring { stiffness: 289.0, damping: 34.0, mass: 1.0 };

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

/// The spring of windows growing out of the strip: a switch's incoming windows, the desktop
/// coming back, a card dropped in the middle (`[animation] spring_*`), at the style's speed.
pub fn switch_spring(style: &StripStyle) -> Spring {
    let a = &style.animation;
    Spring { stiffness: a.spring_stiffness, damping: a.spring_damping, mass: a.spring_mass }.at_speed(a.speed())
}

/// The spring of windows shrinking into the strip: a switch's outgoing windows, the desktop
/// reveal, ⌘M, a window dropped on the strip (`[animation] shrink_*`), at the style's speed.
pub fn shrink_spring(style: &StripStyle) -> Spring {
    let a = &style.animation;
    Spring { stiffness: a.shrink_stiffness, damping: a.shrink_damping, mass: a.shrink_mass }.at_speed(a.speed())
}

/// Where a critically damped spring from rest is 99 % of the way, in units of 1/ω:
/// (1 + x)·e^(-x) = 0.01.
const CRITICAL_99: f64 = 6.6384;

/// The hover's spring: critically damped, 99 % of the way after `seconds` (`[cards] hover_ms`
/// at the style's speed; Apple's card grows on ω ≈ 16.6, 99 % at ~400 ms, its windows turning
/// solid on the same curve, measured 11-09). `None` for no time (it jumps).
pub fn hover_spring(seconds: f64) -> Option<Spring> {
    (seconds > 0.0).then(|| {
        let w = CRITICAL_99 / seconds;
        Spring { stiffness: w * w, damping: 2.0 * w, mass: 1.0 }
    })
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
/// Whether the window server shows a window at `frame`: listed on screen (`onscreen`; a
/// hidden app's window is not) with its `bounds` within [`UNVEIL_TOLERANCE`] of the frame on
/// every edge (a parked one sits in the corner).
pub fn shows_at(onscreen: bool, bounds: Rect, frame: Rect) -> bool {
    onscreen && travel(bounds, frame) <= UNVEIL_TOLERANCE
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

/// A perspective: the eye (overlay coordinates) and its distance in front of the screen
/// (points).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Eye {
    pub at: Point,
    pub distance: f64,
}

/// How a turned rectangle is drawn on the screen: its left edge's x, that edge's top and
/// bottom, its right edge's x, that edge's top and bottom (a trapezoid whose sides stay
/// upright: every turn is about a vertical axis).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drawn {
    pub left: f64,
    pub left_top: f64,
    pub left_bottom: f64,
    pub right: f64,
    pub right_top: f64,
    pub right_bottom: f64,
}

impl Drawn {
    /// The six numbers in their order.
    pub fn array(&self) -> [f64; 6] {
        [self.left, self.left_top, self.left_bottom, self.right, self.right_top, self.right_bottom]
    }

    /// The bounding box.
    pub fn bounds(&self) -> Rect {
        let (x0, x1) = (self.left.min(self.right), self.left.max(self.right));
        let (y0, y1) = (self.left_top.min(self.right_top), self.left_bottom.max(self.right_bottom));
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// How far in front of the screen Apple's snapshots are seen from in flight, in display widths
/// (3000 pt on a 2560 pt wide display; measured 11-09, see the module notes, "Flight").
pub const FLIGHT_DISTANCE: f64 = 1.17;
/// Where, in display widths from the display's left edge, Apple's flight scales a snapshot's
/// sideways place about (−205 pt on a 2560 pt wide display: left of the screen; module notes,
/// "Path").
pub const SCALE_ORIGIN: f64 = -0.08;

/// What a snapshot is seen through in flight: Apple's eye, at the display's middle
/// ([`FLIGHT_DISTANCE`] away), and the sideways origin of its path ([`SCALE_ORIGIN`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flight {
    pub eye: Eye,
    /// The x its path scales the sideways place about ([`Path::sideways_lead`]).
    pub origin_x: f64,
}

impl Flight {
    /// The flight over a display `size` points large whose left edge is at `left` (overlay
    /// coordinates), its eye level with the strip's eye at `eye_y` (Apple's are both at the
    /// display's vertical middle; one level keeps a card's look exact through either).
    pub fn over(left: f64, size: Size, eye_y: f64) -> Self {
        let w = size.w.max(1.0);
        Self { eye: Eye { at: Point::new(left + w / 2.0, eye_y), distance: FLIGHT_DISTANCE * w }, origin_x: left + SCALE_ORIGIN * w }
    }
}

/// A snapshot's pose in flight: a flat rectangle turned about its vertical middle line, which
/// lies in the screen's plane, seen through the flight's eye ([`Flight`]): its middle, its size
/// before the turn, the turn (radians, the right side away) and its corner radius, in the
/// overlay's coordinates. A pose farther back is the same pose smaller: nearer the eye along
/// its rays, everything shrinks by one factor and nothing changes on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub middle: Point,
    pub size: Size,
    pub tilt: f64,
    pub corner: f64,
}

impl Plane {
    /// How the plane is drawn through `eye`: its left edge in the screen's plane, its far side
    /// `w · sin tilt` behind it.
    pub fn drawn(&self, eye: Eye) -> Drawn {
        let d = eye.distance.max(1.0);
        let (top, bottom) = (self.left_mid.y - self.size.h / 2.0, self.left_mid.y + self.size.h / 2.0);
        let f = d / (d + self.size.w * self.tilt.sin()).max(1.0);
        let far = self.left_mid.x + self.size.w * self.tilt.cos();
        Drawn {
            left: self.left_mid.x,
            left_top: top,
            left_bottom: bottom,
            right: eye.at.x + (far - eye.at.x) * f,
            right_top: eye.at.y + (top - eye.at.y) * f,
            right_bottom: eye.at.y + (bottom - eye.at.y) * f,
        }
    }

    /// The plane turned about its left edge that `eye` draws as `d` (the left edge as drawn;
    /// the far side from its height and place); its corner `corner`.
    pub fn from_drawn(d: Drawn, eye: Eye, corner: f64) -> Self {
        let dist = eye.distance.max(1.0);
        let hl = (d.left_bottom - d.left_top).max(1e-9);
        let f = ((d.right_bottom - d.right_top) / hl).max(1e-9);
        let w_sin = dist * (1.0 / f - 1.0);
        let w_cos = eye.at.x + (d.right - eye.at.x) / f - d.left;
        Self {
            left_mid: Point::new(d.left, (d.left_top + d.left_bottom) / 2.0),
            size: Size::new(w_sin.hypot(w_cos), hl),
            tilt: w_sin.atan2(w_cos),
            corner,
        }
    }

    /// The pose that `flight` draws exactly as `strip` draws this plane ([`Pose::from_drawn`]):
    /// a card's thumbnail, seen through the strip's eye, becomes a snapshot seen through the
    /// flight's, with no change on screen.
    pub fn pose(&self, strip: Eye, flight: Eye) -> Pose {
        let mut pose = Pose::from_drawn(self.drawn(strip), flight, 0.0);
        pose.corner = self.corner * pose.size.h / self.size.h.max(1e-9);
        pose
    }
}

impl Pose {
    /// How the pose is drawn through `eye`: each side moved along the eye's rays by its depth
    /// (`± w/2 · sin tilt`).
    pub fn drawn(&self, eye: Eye) -> Drawn {
        let d = eye.distance.max(1.0);
        let half = self.size.w / 2.0;
        let side = |u: f64| {
            let s = d / (d + u * self.tilt.sin()).max(1.0);
            let x = eye.at.x + (self.middle.x + u * self.tilt.cos() - eye.at.x) * s;
            let y = |v: f64| eye.at.y + (self.middle.y + v - eye.at.y) * s;
            (x, y(-self.size.h / 2.0), y(self.size.h / 2.0))
        };
        let (l, r) = (side(-half), side(half));
        Drawn { left: l.0, left_top: l.1, left_bottom: l.2, right: r.0, right_top: r.1, right_bottom: r.2 }
    }

    /// The pose that `eye` draws as `d` (closed form: the sides' heights give the depth
    /// between them, so the turn and the size; their places give the middle); its corner
    /// `corner`.
    pub fn from_drawn(d: Drawn, eye: Eye, corner: f64) -> Self {
        let dist = eye.distance.max(1.0);
        let (hl, hr) = (d.left_bottom - d.left_top, d.right_bottom - d.right_top);
        let depth = dist * (hl - hr) / (hl + hr).max(1e-9);
        let (sl, sr) = (dist / (dist - depth).max(1.0), dist / (dist + depth).max(1.0));
        let left = eye.at.x + (d.left - eye.at.x) / sl;
        let right = eye.at.x + (d.right - eye.at.x) / sr;
        let across = (right - left) / 2.0;
        Self {
            middle: Point::new((left + right) / 2.0, eye.at.y + ((d.left_top + d.left_bottom) / 2.0 - eye.at.y) / sl),
            size: Size::new(2.0 * depth.hypot(across), hl / sl),
            tilt: depth.atan2(across),
            corner,
        }
    }

    /// The plane turned about its left edge that `strip` draws exactly as `flight` draws this
    /// pose ([`Plane::pose`] the other way).
    pub fn plane(&self, flight: Eye, strip: Eye) -> Plane {
        let mut plane = Plane::from_drawn(self.drawn(flight), strip, 0.0);
        plane.corner = self.corner * plane.size.h / self.size.h.max(1e-9);
        plane
    }
}

/// The most a flight's middle runs ahead of the spring sideways, as the `k` of
/// `p + k·p(1 − p)` ([`Path::sideways_lead`]): at 1 it reaches its end with the spring and
/// never passes it.
pub const MAX_LEAD: f64 = 1.0;

/// Which end of a flight is a window's frame: the path leans toward it ([`Path`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toward {
    /// A window grows out of a card (or a dropped card's ghost) to its frame.
    Window,
    /// A window shrinks from its frame into a card.
    Card,
    /// Neither end is a window: a straight path on the spring (a dragged window's card
    /// dropped into its new card).
    Straight,
}

impl Toward {
    fn lean(self) -> f64 {
        match self {
            Toward::Window => 1.0,
            Toward::Card => -1.0,
            Toward::Straight => 0.0,
        }
    }
}

/// A snapshot's flight from one pose to another on a spring, along Apple's path (module notes,
/// "Path"), seen through the flight's eye; `origin_x` is the flight's ([`Flight::origin_x`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Path {
    pub from: Pose,
    pub to: Pose,
    pub toward: Toward,
    pub origin_x: f64,
}

impl Path {
    /// How far ahead of the spring the middle moves sideways toward the window's end: the `k`
    /// of `p + k·p(1 − p)`. Apple's sideways place is scaled about `origin_x` with the
    /// snapshot while the place it is scaled from moves on the spring, so the card's end
    /// counts as the card at the window's size, scaled out about the origin, and the lead is
    /// how far that lies past the window's middle: `(1 − s)·(X − x_w)/(x_w − x_c)`, the card's
    /// scale `s` against the window, `X = o + (x_c − o)/s`. Kept within 0..[`MAX_LEAD`]: never
    /// behind the spring, never past the end. 0 on a straight path.
    pub fn sideways_lead(&self) -> f64 {
        let (card, window) = match self.toward {
            Toward::Window => (self.from, self.to),
            Toward::Card => (self.to, self.from),
            Toward::Straight => return 0.0,
        };
        let s = card.size.h / window.size.h.max(1e-9);
        let span = window.middle.x - card.middle.x;
        if s <= 0.0 || span.abs() < 1.0 {
            return 0.0;
        }
        let scaled_out = self.origin_x + (card.middle.x - self.origin_x) / s;
        ((1.0 - s) * (scaled_out - window.middle.x) / span).clamp(0.0, MAX_LEAD)
    }

    /// The pose when the spring has covered `p` of the way: the size, the turn, the middle's
    /// height and the corner on the spring (the snapshot keeps its window's proportions:
    /// Apple's never stretches); the middle's sideways place [`Self::sideways_lead`] of
    /// `p (1 − p)` toward the window's end.
    pub fn at(&self, p: f64) -> Pose {
        let side = p + self.sideways_lead() * p * (1.0 - p) * self.toward.lean();
        let lerp = |a: f64, b: f64, u: f64| a + (b - a) * u;
        let (a, b) = (self.from, self.to);
        Pose {
            middle: Point::new(lerp(a.middle.x, b.middle.x, side), lerp(a.middle.y, b.middle.y, p)),
            size: Size::new(lerp(a.size.w, b.size.w, p), lerp(a.size.h, b.size.h, p)),
            tilt: lerp(a.tilt, b.tilt, p),
            corner: lerp(a.corner, b.corner, p),
        }
    }

    /// How much farther than the spring's own way any part of the pose can still be from its
    /// end: shrinking into a card, the sideways place trails the spring by up to
    /// [`MAX_LEAD`] of what is left; growing, every part is at least as far along.
    pub fn trail(toward: Toward) -> f64 {
        1.0 + (-toward.lean()).max(0.0) * MAX_LEAD
    }
}

/// How many poses a second a flight is drawn from ([`flight_samples`]): Core Animation goes
/// straight between them, every 8 ms, finer than a 60 or 120 Hz display's frame.
pub const FLIGHT_SAMPLES_PER_SECOND: f64 = 120.0;

/// The poses `path` passes on spring `s` over `duration` seconds, as (fraction of the
/// duration, pose), evenly spaced at [`FLIGHT_SAMPLES_PER_SECOND`] or finer: the first is
/// `path.from`, the last exactly `path.to` (the spring rests within [`END_TOLERANCE`] there
/// when `duration` is [`path_duration`]).
pub fn flight_samples(path: &Path, s: Spring, duration: f64) -> Vec<(f64, Pose)> {
    let n = ((duration.max(0.0) * FLIGHT_SAMPLES_PER_SECOND).ceil() as usize).clamp(1, 2400);
    (0..=n)
        .map(|i| {
            let f = i as f64 / n as f64;
            let pose = if i == n { path.to } else { path.at(spring_progress(s, f * duration)) };
            (f, pose)
        })
        .collect()
}

/// How long a snapshot's flight of `travel` points along a path toward `toward` runs on `s`
/// (seconds): [`spring_duration`], with a trailing part's longer way in.
pub fn path_duration(s: Spring, travel: f64, toward: Toward) -> f64 {
    spring_duration(s, travel * Path::trail(toward))
}

/// When a flight toward `toward` stays within `tolerance` of its end (a fraction of the way)
/// for good on `s` (seconds): [`settle_time`], with a trailing part's longer way in.
pub fn path_settle_time(s: Spring, tolerance: f64, toward: Toward) -> f64 {
    settle_time(s, tolerance / Path::trail(toward))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The shadow hands over once the window server shows the window (`shows_at`), in one
    /// step: from then on the shadow on screen is the window's own, never darker or lighter
    /// (11-01: dropped 50 ms early, 16 levels lighter for ~80 ms; 11-09: faded over three
    /// frames once shown, both shadows for four frames, up to 12 levels darker).
    #[test]
    fn the_shadow_hands_over_once_the_window_shows() {
        let frame = Rect::new(813.0, 215.0, 1217.0, 790.0);
        assert!(shows_at(true, Rect::new(813.4, 215.0, 1217.0, 790.0), frame));
        assert!(!shows_at(true, Rect::new(2559.0, 1408.0, 1217.0, 790.0), frame), "parked in the corner");
        assert!(!shows_at(false, frame, frame), "a hidden app's window, in place");
        for a in [0.1, 0.4, 0.8] {
            for i in 1..40 {
                let t = i as f64 * 0.005;
                let e = crate::ui::shadow_math::handover_error(a, t, 0.0, 0.0, 0.0);
                assert!(e.abs() < 1e-12, "the window's own shadow alone at {t}: {e}");
            }
        }
    }

    /// The 5K screen's strip eye, where the overlay covers the main display (global = overlay).
    const EYE: Eye = Eye { at: Point::new(15.0, 720.0), distance: 2260.0 };

    /// The flight over the 5K screen: its eye in front of the middle, 3000 pt away; its
    /// sideways origin 205 pt left of the screen.
    fn flight() -> Flight {
        Flight::over(0.0, Size::new(2560.0, 1440.0), 720.0)
    }

    /// Apple's switch, measured 11-09 (5K screen, 60 fps, the fixture windows), as each frame
    /// draws the window: ms after the fitted onset (about a frame before the first moving
    /// frame), then its left edge's x, that edge's top and bottom, its right edge's x, that
    /// edge's top and bottom (points; the top from the colour's top and the title bar's share
    /// of the height). The blue window growing out of its card (hovered, slot 5) to
    /// 759, 258.5, 1100 × 748.5.
    const GROWN: [(f64, [f64; 6]); 30] = [
        (16.0, [51.0, 912.3, 1041.9, 171.0, 901.9, 1024.3]),
        (32.7, [139.0, 860.6, 1042.9, 319.0, 849.2, 1018.7]),
        (49.3, [243.0, 793.5, 1044.1, 507.0, 786.9, 1013.7]),
        (66.0, [343.0, 721.1, 1045.1, 703.0, 722.7, 1009.5]),
        (82.7, [427.0, 652.0, 1046.2, 887.0, 661.0, 1006.7]),
        (99.3, [499.0, 587.6, 1045.2, 1055.0, 603.4, 1005.7]),
        (116.0, [551.1, 529.4, 1043.2, 1199.0, 551.0, 1005.9]),
        (132.7, [595.0, 478.7, 1040.6, 1323.0, 505.0, 1006.1]),
        (149.3, [627.0, 436.3, 1037.7, 1427.0, 463.7, 1006.4]),
        (166.0, [655.0, 400.6, 1034.2, 1511.0, 428.5, 1006.8]),
        (182.7, [675.0, 371.9, 1030.7, 1578.1, 397.6, 1007.1]),
        (199.3, [691.0, 348.8, 1026.9, 1635.0, 371.8, 1007.8]),
        (216.0, [703.0, 329.9, 1024.4, 1683.0, 350.3, 1007.8]),
        (232.7, [715.0, 314.4, 1021.6, 1719.0, 332.9, 1008.1]),
        (249.3, [723.0, 302.5, 1019.2, 1747.0, 318.0, 1008.5]),
        (266.0, [731.0, 293.2, 1016.9, 1771.0, 306.0, 1009.0]),
        (282.7, [741.4, 285.9, 1015.7, 1787.0, 295.4, 1008.8]),
        (299.3, [743.0, 279.5, 1011.3, 1803.0, 288.4, 1010.9]),
        (316.0, [747.0, 274.5, 1011.0, 1815.0, 281.9, 1011.0]),
        (332.7, [747.0, 270.0, 1011.0, 1823.0, 277.7, 1011.0]),
        (349.3, [751.0, 268.3, 1011.2, 1831.0, 272.8, 1010.6]),
        (366.0, [755.0, 267.1, 1011.6, 1835.9, 267.1, 1009.8]),
        (382.7, [755.0, 261.5, 1012.5, 1843.0, 269.2, 1007.7]),
        (399.3, [755.0, 262.8, 1012.6, 1847.0, 263.1, 1007.0]),
        (416.0, [760.9, 262.8, 1012.8, 1847.0, 263.1, 1006.6]),
        (432.7, [759.0, 258.1, 1012.8, 1851.0, 263.9, 1006.2]),
        (449.3, [759.0, 258.7, 1012.0, 1851.0, 259.0, 1005.9]),
        (466.0, [759.0, 258.9, 1007.0, 1855.0, 258.9, 1007.0]),
        (482.7, [759.0, 258.9, 1007.0, 1855.0, 258.9, 1007.0]),
        (499.3, [759.0, 258.9, 1007.0, 1855.0, 258.9, 1007.0]),
    ];
    /// The same switch's other way: the green window (1299, 599, 1000 × 692) shrinking into
    /// its place in the combined card, measured the same way (NaN: an incoming snapshot
    /// passed over that edge).
    const SHRUNK: [(f64, [f64; 6]); 29] = [
        (18.2, [1275.0, 605.9, 1285.2, 2239.0, 612.9, 1277.8]),
        (34.8, [1211.0, 632.2, 1272.1, 2091.0, 637.3, 1249.1]),
        (51.5, [1127.0, 668.1, 1251.0, 1899.0, 669.8, 1214.7]),
        (68.2, [1031.0, 702.9, 1225.0, 1687.0, 705.0, 1180.3]),
        (84.8, [923.0, 738.6, 1196.9, 1475.0, 734.0, 1149.5]),
        (101.5, [815.0, 764.3, 1170.0, 1277.0, 766.6, 1122.5]),
        (118.2, [711.0, NAN, 1144.2, 1095.0, NAN, 1100.8]),
        (134.8, [611.0, NAN, NAN, 935.0, NAN, NAN]),
        (151.5, [523.0, NAN, NAN, 795.0, NAN, NAN]),
        (168.2, [443.0, NAN, NAN, 675.0, NAN, NAN]),
        (184.8, [375.0, 875.8, 1077.4, 579.0, 862.8, 1048.4]),
        (201.5, [315.0, 886.2, 1067.7, 495.0, 873.9, 1041.4]),
        (218.2, [267.0, 897.0, 1057.8, 423.0, 883.7, 1036.6]),
        (234.8, [227.0, 904.3, 1052.2, 367.0, 892.2, 1031.9]),
        (251.5, [195.0, 911.0, 1046.4, 323.0, 899.4, 1027.9]),
        (268.2, [167.0, 915.5, 1043.1, 283.0, 906.4, 1024.4]),
        (284.8, [143.0, 920.6, 1040.0, 255.0, 909.8, 1021.5]),
        (301.5, [124.5, 924.4, 1034.5, 227.0, 913.8, 1020.8]),
        (318.2, [111.0, 927.7, 1033.8, 207.0, 916.4, 1018.9]),
        (334.8, [99.0, 928.4, 1031.4, 191.0, 920.8, 1017.4]),
        (351.5, [91.0, 931.6, 1029.4, 179.0, 921.1, 1016.9]),
        (368.2, [83.0, 932.9, 1028.7, 171.0, 923.1, 1015.8]),
        (384.8, [75.0, 932.9, 1029.0, 163.0, 925.2, 1014.5]),
        (401.5, [68.3, 934.8, 1027.0, 155.0, 925.4, 1013.7]),
        (418.2, [66.9, 936.3, 1026.5, 151.0, 925.7, 1013.5]),
        (434.8, [63.0, 937.0, 1025.2, 147.0, 926.4, 1013.8]),
        (451.5, [63.0, 936.7, 1025.5, 143.0, 928.1, 1013.1]),
        (468.2, [59.0, 936.7, 1025.5, 139.0, 929.1, 1013.1]),
        (484.8, [59.0, 936.7, 1025.4, 139.0, 929.1, 1012.9]),
    ];
    const NAN: f64 = f64::NAN;

    /// Apple's switch recorded for its depth (11-09, `capture-11-09/depth`, the second of
    /// three identical runs each way): the fixture's 1800 × 1100 pt blue window at 300, 130
    /// and its card in slot 5, each frame's blue rectangle as drawn (its corners tracked to a
    /// tenth of a point: colour edges fitted with lines), ms after the last still frame, in
    /// [`Drawn`]'s order. Growing out of its card, from [`GROWN_1800_FROM`].
    const GROWN_1800_FROM: [f64; 6] = [10.7, 889.8, 1028.0, 148.8, 878.6, 1007.6];
    const WINDOW_1800: [f64; 6] = [300.7, 130.3, 1229.7, 2100.7, 130.3, 1229.7];
    const GROWN_1800: [(f64, [f64; 6]); 25] = [
        (16.7, [45.2, 868.1, 1035.9, 220.9, 855.8, 1010.7]),
        (33.3, [120.1, 801.9, 1055.5, 388.8, 793.0, 1018.5]),
        (50.0, [202.8, 733.8, 1080.8, 599.8, 731.9, 1030.0]),
        (66.7, [272.5, 652.5, 1108.3, 818.9, 663.6, 1044.9]),
        (83.3, [320.5, 571.8, 1135.6, 1028.3, 598.3, 1061.7]),
        (100.0, [346.8, 496.1, 1160.1, 1216.1, 537.0, 1079.9]),
        (116.7, [356.8, 428.1, 1180.6, 1379.9, 480.2, 1098.5]),
        (133.3, [354.8, 369.4, 1196.7, 1517.7, 428.2, 1116.8]),
        (150.0, [348.5, 320.4, 1208.5, 1632.6, 381.4, 1133.9]),
        (166.7, [338.8, 280.4, 1216.9, 1727.7, 340.1, 1149.4]),
        (183.3, [330.5, 248.3, 1222.5, 1802.9, 304.0, 1163.2]),
        (200.0, [322.8, 222.9, 1226.2, 1864.2, 273.0, 1175.1]),
        (216.7, [317.2, 202.8, 1228.4, 1913.4, 246.8, 1185.2]),
        (233.3, [312.4, 187.1, 1229.7, 1952.1, 224.8, 1193.7]),
        (250.0, [309.3, 174.8, 1230.4, 1983.7, 206.6, 1200.7]),
        (266.7, [306.8, 165.2, 1230.7, 2007.8, 191.6, 1206.6]),
        (283.3, [304.6, 157.6, 1230.9, 2026.9, 179.4, 1211.1]),
        (300.0, [304.0, 151.7, 1231.0, 2042.6, 169.4, 1215.0]),
        (316.7, [303.6, 147.1, 1230.8, 2054.8, 161.4, 1218.2]),
        (333.3, [301.9, 143.4, 1230.9, 2064.2, 154.9, 1220.4]),
        (350.0, [301.6, 140.5, 1230.7, 2072.1, 149.8, 1222.5]),
        (366.7, [301.5, 138.3, 1230.7, 2077.8, 145.6, 1224.0]),
        (383.3, [301.3, 136.5, 1230.5, 2082.9, 142.3, 1225.4]),
        (400.0, [301.2, 135.1, 1230.5, 2086.0, 139.7, 1226.2]),
        (416.7, [301.2, 134.0, 1230.3, 2090.0, 137.6, 1227.3]),
    ];
    /// The same window shrinking into its card ([`SHRUNK_1800_TO`]); 100–133 ms are missing,
    /// the incoming window over its edges.
    const SHRUNK_1800_TO: [f64; 6] = [11.0, 889.7, 1027.8, 149.3, 878.5, 1007.5];
    const SHRUNK_1800: [(f64, [f64; 6]); 22] = [
        (16.7, [320.4, 193.7, 1225.0, 1941.0, 231.7, 1188.5]),
        (33.3, [344.0, 260.6, 1213.6, 1781.6, 315.2, 1154.8]),
        (50.0, [364.6, 339.1, 1195.4, 1600.0, 397.1, 1122.9]),
        (66.7, [372.7, 419.9, 1173.1, 1412.0, 471.0, 1095.8]),
        (83.3, [364.9, 496.1, 1150.0, 1229.9, 535.5, 1074.1]),
        (150.0, [240.4, 714.1, 1078.8, 666.9, 714.8, 1027.6]),
        (166.7, [205.7, 748.6, 1067.8, 571.1, 744.1, 1022.1]),
        (183.3, [175.6, 768.5, 1058.5, 490.9, 764.1, 1018.0]),
        (200.0, [147.1, 793.1, 1051.4, 424.5, 785.3, 1014.7]),
        (216.7, [124.5, 812.8, 1045.4, 370.5, 803.6, 1012.1]),
        (233.3, [104.0, 831.0, 1040.9, 325.3, 817.9, 1010.3]),
        (250.0, [87.6, 845.2, 1038.4, 288.6, 830.3, 1008.3]),
        (266.7, [72.6, 851.1, 1034.1, 260.0, 845.8, 1007.7]),
        (283.3, [61.7, 866.6, 1031.4, 236.8, 850.2, 1006.9]),
        (300.0, [53.2, 868.3, 1029.4, 216.7, 858.6, 1006.2]),
        (316.7, [45.3, 873.6, 1028.0, 202.5, 863.7, 1005.5]),
        (333.3, [39.6, 877.5, 1026.8, 189.7, 867.9, 1005.4]),
        (350.0, [34.4, 880.6, 1026.4, 181.2, 870.6, 1005.2]),
        (366.7, [29.6, 882.8, 1026.4, 173.5, 872.9, 1005.4]),
        (383.3, [26.5, 884.5, 1026.2, 168.1, 874.6, 1005.6]),
        (400.0, [23.6, 885.8, 1026.4, 163.2, 875.6, 1005.9]),
        (416.7, [21.1, 886.9, 1026.5, 160.1, 876.3, 1006.1]),
    ];

    fn drawn(a: [f64; 6]) -> Drawn {
        Drawn { left: a[0], left_top: a[1], left_bottom: a[2], right: a[3], right_top: a[4], right_bottom: a[5] }
    }

    /// The blue card as drawn (hovered) and its window; the green window and its place in the
    /// combined card (the 1100 pt recordings): as planes turned about their left edge (solved
    /// from the drawn trapezoids through [`EYE`]).
    fn grow_ends() -> (Plane, Plane) {
        (plane(11.0, 935.9, 162.68, 107.44, 0.8995), plane(759.0, 258.5, 1100.0, 748.5, 0.0))
    }
    fn shrink_ends() -> (Plane, Plane) {
        (plane(1299.0, 599.0, 1000.0, 692.0, 0.0), plane(55.0, 937.64, 118.7, 86.52, 0.8258))
    }
    fn plane(x: f64, top: f64, w: f64, h: f64, tilt: f64) -> Plane {
        Plane { left_mid: Point::new(x, top + h / 2.0), size: Size::new(w, h), tilt, corner: 0.0 }
    }

    /// Root mean square and worst miss of `model` against `measured` (points; NaN skipped).
    fn misses(measured: &[(f64, [f64; 6])], model: impl Fn(f64) -> [f64; 6]) -> (f64, f64) {
        let (mut sum, mut n, mut worst) = (0.0, 0.0, 0.0_f64);
        for (ms, m) in measured {
            let d = model(ms / 1000.0);
            for (a, b) in m.iter().zip(d) {
                if a.is_nan() {
                    continue;
                }
                sum += (a - b) * (a - b);
                n += 1.0;
                worst = worst.max((a - b).abs());
            }
        }
        ((sum / n).sqrt(), worst)
    }

    /// The flight from plane `a` to plane `b` (given through the strip's eye) toward
    /// `toward`, as drawn `t` seconds in.
    fn flown(a: Plane, b: Plane, toward: Toward, s: Spring, t: f64) -> [f64; 6] {
        let f = flight();
        flown_poses(a.pose(EYE, f.eye), b.pose(EYE, f.eye), toward, s, t)
    }

    /// The flight between two poses (through the flight's eye), as drawn `t` seconds in.
    fn flown_poses(a: Pose, b: Pose, toward: Toward, s: Spring, t: f64) -> [f64; 6] {
        let f = flight();
        let path = Path { from: a, to: b, toward, origin_x: f.origin_x };
        path.at(spring_progress(s, t)).drawn(f.eye).array()
    }

    /// The flight between two drawn ends (Apple's stills), as drawn `t` seconds in.
    fn flown_drawn(a: [f64; 6], b: [f64; 6], toward: Toward, s: Spring, t: f64) -> [f64; 6] {
        let e = flight().eye;
        flown_poses(Pose::from_drawn(drawn(a), e, 0.0), Pose::from_drawn(drawn(b), e, 0.0), toward, s, t)
    }

    /// The motion-path round's model (11-09, before this one), `t` seconds in: through the
    /// strip's eye, the middle's x and the width 0.33 and the turn 0.19 of `p (1 − p)` ahead
    /// toward the window, the height and the middle's height on the spring.
    fn motion_path(a: Plane, b: Plane, toward: Toward, s: Spring, t: f64) -> [f64; 6] {
        let (from, to) = (Pose::from_drawn(a.drawn(EYE), EYE, 0.0), Pose::from_drawn(b.drawn(EYE), EYE, 0.0));
        let p = spring_progress(s, t);
        let bend = p * (1.0 - p) * toward.lean();
        let (side, flat) = (p + 0.33 * bend, p + 0.19 * bend);
        let lerp = |x: f64, y: f64, u: f64| x + (y - x) * u;
        let pose = Pose {
            middle: Point::new(lerp(from.middle.x, to.middle.x, side), lerp(from.middle.y, to.middle.y, p)),
            size: Size::new(lerp(from.size.w, to.size.w, side), lerp(from.size.h, to.size.h, p)),
            tilt: lerp(from.tilt, to.tilt, flat),
            corner: 0.0,
        };
        pose.drawn(EYE).array()
    }

    /// A plane turned about its left edge moved straight on one spring (every snapshot until
    /// the motion-path round), `t` seconds in.
    fn straight(a: Plane, b: Plane, s: Spring, t: f64) -> [f64; 6] {
        let p = spring_progress(s, t);
        let lerp = |x: f64, y: f64| x + (y - x) * p;
        let q = Plane {
            left_mid: Point::new(lerp(a.left_mid.x, b.left_mid.x), lerp(a.left_mid.y, b.left_mid.y)),
            size: Size::new(lerp(a.size.w, b.size.w), lerp(a.size.h, b.size.h)),
            tilt: lerp(a.tilt, b.tilt),
            corner: 0.0,
        };
        q.drawn(EYE).array()
    }

    /// [`misses`] with the model's own onset: the best within 40 ms of the table's (each
    /// model is judged at its best start; the 1800 pt tables count from the last still frame,
    /// and Apple's shrinking flight starts a frame before it shows).
    fn best_misses(measured: &[(f64, [f64; 6])], model: impl Fn(f64) -> [f64; 6]) -> (f64, f64) {
        (-80..=80).map(|i| misses(measured, |t| model(t + i as f64 * 0.0005))).fold((f64::MAX, 0.0), |best, m| if m.0 < best.0 { m } else { best })
    }

    /// Apple's snapshot never stretches, seen through the flight's eye: every frame of the
    /// 1800 × 1100 pt window at least 300 pt tall (smaller ones are too few pixels for its
    /// turn), read as a pose there, keeps the window's proportions within 1.5 %; through the
    /// strip's eye the same frames read far apart.
    #[test]
    fn apples_snapshot_is_its_window_seen_through_the_flight_eye() {
        let aspect = |d: [f64; 6], eye: Eye| {
            let p = Pose::from_drawn(drawn(d), eye, 0.0);
            p.size.w / p.size.h / (1800.0 / 1100.0)
        };
        let frames: Vec<[f64; 6]> = GROWN_1800.iter().chain(SHRUNK_1800.iter()).map(|r| r.1).filter(|d| d[2] - d[1] >= 300.0).collect();
        assert!(frames.len() >= 28, "{}", frames.len());
        let f = flight().eye;
        for d in &frames {
            let a = aspect(*d, f);
            assert!((a - 1.0).abs() < 0.015, "{a:.3} for {d:?}");
        }
        let through_strip: Vec<f64> = frames.iter().map(|d| aspect(*d, EYE)).collect();
        let (lo, hi) = through_strip.iter().fold((f64::MAX, 0.0_f64), |(lo, hi), a| (lo.min(*a), hi.max(*a)));
        assert!(hi - lo > 0.1, "the strip's eye: {lo:.3}..{hi:.3}");
        // The card is the window far back and turned: 0.12 of its height, 76°.
        let card = Pose::from_drawn(drawn(GROWN_1800_FROM), f, 0.0);
        assert!((card.size.h / 1100.0 - 0.121).abs() < 0.003 && (card.tilt.to_degrees() - 75.8).abs() < 1.0, "{card:?}");
    }

    #[test]
    fn apples_path_matches_the_measured_corners() {
        let s = Spring::APPLE;
        assert_eq!(s, Spring::APPLE_SHRINK, "one spring both ways");
        // The 1800 pt window, recorded for this.
        let (rms, worst) = best_misses(&GROWN_1800, |t| flown_drawn(GROWN_1800_FROM, WINDOW_1800, Toward::Window, s, t));
        assert!(rms < NEW_GROWN.0 && worst < NEW_GROWN.1, "growing: {rms:.2} pt rms, {worst:.1} at worst");
        let (rms, worst) = best_misses(&SHRUNK_1800, |t| flown_drawn(WINDOW_1800, SHRUNK_1800_TO, Toward::Card, s, t));
        assert!(rms < NEW_SHRUNK.0 && worst < NEW_SHRUNK.1, "shrinking: {rms:.2} pt rms, {worst:.1} at worst");
        // The 1100 pt window, the one origin's other place.
        let (a, b) = grow_ends();
        let (rms, worst) = best_misses(&GROWN, |t| flown(a, b, Toward::Window, s, t));
        assert!(rms < OLD_GROWN.0 && worst < OLD_GROWN.1, "1100 pt growing: {rms:.2} pt rms, {worst:.1} at worst");
        let (c, d) = shrink_ends();
        let (rms, worst) = best_misses(&SHRUNK, |t| flown(c, d, Toward::Card, s, t));
        assert!(rms < OLD_SHRUNK.0 && worst < OLD_SHRUNK.1, "1100 pt shrinking: {rms:.2} pt rms, {worst:.1} at worst");
        // The motion-path model before this one fitted the 1100 pt window, and misses the
        // 1800 pt one by far: it stretched the snapshot wide early and saw it from the strip.
        let strip = |x: [f64; 6]| Plane::from_drawn(drawn(x), EYE, 0.0);
        let (rms, _) = best_misses(&GROWN, |t| motion_path(a, b, Toward::Window, s, t));
        assert!(rms < 3.5, "motion-path on its own recording: {rms:.2}");
        let (rms, worst) = best_misses(&GROWN_1800, |t| motion_path(strip(GROWN_1800_FROM), strip(WINDOW_1800), Toward::Window, s, t));
        assert!(rms > 15.0 && worst > 80.0, "motion-path growing 1800: {rms:.2}, {worst:.1}");
        let (rms, worst) = best_misses(&SHRUNK_1800, |t| motion_path(strip(WINDOW_1800), strip(SHRUNK_1800_TO), Toward::Card, s, t));
        assert!(rms > 15.0 && worst > 80.0, "motion-path shrinking 1800: {rms:.2}, {worst:.1}");
        // A straight path, or the new one without its sideways lead, are far off too.
        let (rms, _) = best_misses(&GROWN_1800, |t| straight(strip(GROWN_1800_FROM), strip(WINDOW_1800), s, t));
        assert!(rms > 15.0, "straight: {rms:.2}");
        let (rms, _) = best_misses(&GROWN_1800, |t| flown_drawn(GROWN_1800_FROM, WINDOW_1800, Toward::Straight, s, t));
        assert!(rms > 15.0, "no lead: {rms:.2}");
        // Critically damped: no overshoot, 99 % at about 390 ms.
        assert!((0..2000).all(|i| spring_progress(s, i as f64 / 1000.0) <= 1.0 + 1e-9));
        assert!((0.38..0.40).contains(&time_to_progress(s, 0.99)));
    }

    /// Each fit's bound (rms, worst; points).
    const NEW_GROWN: (f64, f64) = (5.5, 23.0);
    const NEW_SHRUNK: (f64, f64) = (6.2, 17.5);
    const OLD_GROWN: (f64, f64) = (6.3, 25.0);
    const OLD_SHRUNK: (f64, f64) = (5.5, 18.0);

    #[test]
    fn a_pose_looks_exactly_like_its_plane() {
        let f = flight().eye;
        for p in [
            plane(11.0, 935.9, 162.68, 107.44, 0.8995),
            plane(55.0, 937.6, 118.7, 86.5, 0.8258),
            plane(759.0, 258.5, 1100.0, 748.5, 0.0),
            plane(300.0, 100.0, 900.0, 600.0, 0.3),
        ] {
            let pose = p.pose(EYE, f);
            let (a, b) = (p.drawn(EYE).array(), pose.drawn(f).array());
            assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9), "{a:?} vs {b:?}");
            let back = pose.plane(f, EYE);
            assert!((back.left_mid.x - p.left_mid.x).abs() < 1e-9 && (back.size.w - p.size.w).abs() < 1e-9 && (back.left_mid.y - p.left_mid.y).abs() < 1e-9);
            assert!((back.tilt - p.tilt).abs() < 1e-9);
            if p.tilt == 0.0 {
                assert!((pose.size.w - p.size.w).abs() < 1e-9 && pose.tilt.abs() < 1e-12, "a flat plane is its own pose");
            } else {
                assert!(pose.tilt > p.tilt, "a card seen from the display's middle is turned farther");
            }
        }
        // One eye both ways: the pose turned about the middle, the same look.
        let p = plane(300.0, 100.0, 900.0, 600.0, 0.3);
        let same = p.pose(EYE, EYE);
        assert!(p.drawn(EYE).array().iter().zip(same.drawn(EYE).array()).all(|(x, y)| (x - y).abs() < 1e-9));
        assert!(same.size.w < p.size.w, "its middle moved into the screen: nearer, so smaller");
    }

    #[test]
    fn the_path_leans_toward_the_window_and_ends_on_both_poses() {
        let (a, b) = grow_ends();
        let f = flight();
        let path = Path { from: a.pose(EYE, f.eye), to: b.pose(EYE, f.eye), toward: Toward::Window, origin_x: f.origin_x };
        assert_eq!(path.at(0.0), path.from);
        assert_eq!(path.at(1.0), path.to);
        let lead = path.sideways_lead();
        assert!((0.2..0.4).contains(&lead), "the 1100 pt window: {lead}");
        let big = Path {
            from: Pose::from_drawn(drawn(GROWN_1800_FROM), f.eye, 0.0),
            to: Pose::from_drawn(drawn(WINDOW_1800), f.eye, 0.0),
            ..path
        };
        assert!((0.65..0.85).contains(&big.sideways_lead()), "the 1800 pt window: {}", big.sideways_lead());
        let half = path.at(0.5);
        let mid = |x: f64, y: f64| (x + y) / 2.0;
        assert!(half.middle.x > mid(path.from.middle.x, path.to.middle.x) + 50.0, "out sideways early");
        assert!((half.size.w - mid(path.from.size.w, path.to.size.w)).abs() < 1e-9, "the size on the spring");
        assert!((half.size.h - mid(path.from.size.h, path.to.size.h)).abs() < 1e-9);
        assert!((half.tilt - mid(path.from.tilt, path.to.tilt)).abs() < 1e-9, "the turn on the spring");
        assert!((half.middle.y - mid(path.from.middle.y, path.to.middle.y)).abs() < 1e-9, "up on the spring");
        // Into a card it trails sideways: the same curve the other way; nothing overshoots.
        let back = Path { from: path.to, to: path.from, toward: Toward::Card, origin_x: f.origin_x };
        assert!((back.sideways_lead() - lead).abs() < 1e-12, "one curve both ways");
        assert!((back.at(0.3).middle.x - path.at(0.7).middle.x).abs() < 1e-9);
        for i in 0..=100 {
            let p = big.at(i as f64 / 100.0);
            assert!(p.middle.x <= big.to.middle.x + 1e-9 && p.size.w <= big.to.size.w + 1e-9 && p.tilt >= -1e-12);
        }
        // Never past the end, however far the window is from its card.
        let near = Path { to: Pose { middle: Point::new(400.0, 600.0), ..big.to }, ..big };
        assert_eq!(near.sideways_lead(), MAX_LEAD);
        assert!((0..=100).all(|i| near.at(i as f64 / 100.0).middle.x <= 400.0 + 1e-9));
        let straight = Path { toward: Toward::Straight, ..path };
        assert_eq!(straight.sideways_lead(), 0.0);
        assert!((straight.at(0.5).middle.x - mid(path.from.middle.x, path.to.middle.x)).abs() < 1e-9);
        // A trailing flight runs longer and settles later than its spring alone.
        let s = Spring::APPLE;
        assert!(path_duration(s, 1500.0, Toward::Card) > spring_duration(s, 1500.0));
        assert_eq!(path_duration(s, 1500.0, Toward::Window), spring_duration(s, 1500.0));
        assert!(path_settle_time(s, 0.002, Toward::Card) > settle_time(s, 0.002));
        // Within that time every part is within the end tolerance of its end.
        let travel = (back.from.middle.x - back.to.middle.x).abs();
        let d = path_duration(s, travel, Toward::Card);
        let at = back.at(spring_progress(s, d));
        assert!((at.middle.x - back.to.middle.x).abs() <= END_TOLERANCE + 1e-9, "{}", (at.middle.x - back.to.middle.x).abs());
    }

    #[test]
    fn a_flight_is_drawn_from_poses_a_frame_apart_and_ends_exactly() {
        let (a, b) = grow_ends();
        let f = flight();
        let path = Path { from: a.pose(EYE, f.eye), to: b.pose(EYE, f.eye), toward: Toward::Window, origin_x: f.origin_x };
        let s = Spring::APPLE;
        let d = path_duration(s, 1750.0, Toward::Window);
        let samples = flight_samples(&path, s, d);
        assert_eq!(samples.first().map(|x| x.1), Some(path.from));
        assert_eq!(samples.last().map(|x| x.1), Some(path.to));
        assert!(samples.len() as f64 >= d * FLIGHT_SAMPLES_PER_SECOND);
        assert!(samples.windows(2).all(|w| w[1].0 > w[0].0 && (w[1].0 - w[0].0) * d <= 1.0 / FLIGHT_SAMPLES_PER_SECOND + 1e-9));
        // The step before the last is within the end tolerance: no jump at the end.
        let before = samples[samples.len() - 2].1;
        assert!((before.middle.x - path.to.middle.x).abs() < 0.2 && (before.size.w - path.to.size.w).abs() < 0.2, "{before:?}");
        assert_eq!(flight_samples(&path, s, 0.0).len(), 2, "nothing to fly: from and to");
    }

    /// The unveil waits for the snapshot as drawn: on Apple's path, through the flight's eye,
    /// each drawn edge is within the tolerance of the window by the unveil (less the lag), for
    /// both recorded windows.
    #[test]
    fn the_unveil_holds_on_apples_path() {
        let style = StripStyle::default();
        let (a, b) = grow_ends();
        let big = (Plane::from_drawn(drawn(GROWN_1800_FROM), EYE, 0.0), Plane::from_drawn(drawn(WINDOW_1800), EYE, 0.0));
        for (a, b) in [(a, b), big] {
            let start = a.drawn(EYE).bounds();
            let end = Rect::new(b.left_mid.x, b.left_mid.y - b.size.h / 2.0, b.size.w, b.size.h);
            let t = unveil_time(&style, travel(start, end)) - PRESENTATION_LAG;
            let w = [end.x, end.y, end.max_y(), end.max_x(), end.y, end.max_y()];
            for i in 0..400 {
                let shown = flown(a, b, Toward::Window, switch_spring(&style), t + i as f64 / 1000.0);
                let off = shown.iter().zip(w).fold(0.0_f64, |m, (x, y)| m.max((x - y).abs()));
                assert!(off <= UNVEIL_TOLERANCE, "{off:.2} pt off {i} ms after the unveil");
            }
        }
    }

    /// Apple's hover, measured 11-09: the card's top edge and its windows' opacity (turning
    /// solid) at these ms after the pointer came onto it, both fitted best by a critically
    /// damped spring of ω ≈ 16.7 (an ease-out of ~285 ms missed by twice as much).
    #[test]
    fn the_hover_spring_reaches_99_percent_in_hover_ms() {
        let s = hover_spring(0.4).expect("a spring");
        assert!((time_to_progress(s, 0.99) - 0.4).abs() < 0.002);
        let w = (s.stiffness / s.mass).sqrt();
        assert!((w - 16.6).abs() < 0.1, "ω {w}");
        assert!((s.damping - 2.0 * w).abs() < 1e-9, "critically damped");
        assert!((time_to_progress(s, 0.5) - 0.1).abs() < 0.005, "half way at a quarter of it");
        assert_eq!(hover_spring(0.0), None);
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
        assert_eq!(shrink_spring(&style), Spring::APPLE_SHRINK, "both ways");
        let glide = glide_spring(&style);
        let slide = slide_spring(&style);
        let omega = |s: Spring| (s.stiffness / s.mass).sqrt();
        assert!((omega(glide) - 17.0).abs() < 0.1 && (omega(slide) - 16.8).abs() < 0.1, "{} {}", omega(glide), omega(slide));
        style.animation.speed = 2.0;
        assert!((omega(switch_spring(&style)) - 2.0 * omega(Spring::APPLE)).abs() < 1e-9);
        assert!((omega(shrink_spring(&style)) - 2.0 * omega(Spring::APPLE_SHRINK)).abs() < 1e-9);
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
        assert!((0.58..0.63).contains(&long), "a long travel unveils at {long:.3} s");
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
        // The spring of the time (272/33).
        let s = Spring { stiffness: 272.0, damping: 33.0, mass: 1.0 };
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
        // A growing window's snapshot ends before it lands (the unveil plus 120 ms).
        let style = StripStyle::default();
        let grown = spring_duration(Spring::APPLE, 1999.0);
        assert!(grown <= unveil_time(&style, 1999.0) + 0.12, "{grown}");
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
}
