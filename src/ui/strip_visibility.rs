//! When the strip shows, pure: the user's toggle, Apple's "Show recent apps" (`AutoHide`),
//! auto-hide while an active window overlaps the strip, and the left-edge reveal.
//!
//! The strip feeds this with the pointer position (polled every ~60 ms only while
//! [`Decision::poll`] says so) and the current time; it answers whether the strip shows.

use std::time::{Duration, Instant};

use crate::engine::geometry::{Point, Rect};

/// The pointer must rest this long at the left edge to reveal the strip.
pub const EDGE_DWELL: Duration = Duration::from_millis(250);
/// A revealed strip hides this long after the pointer leaves it.
pub const LEAVE_DELAY: Duration = Duration::from_millis(500);
/// The left edge zone, in points from the strip's left edge.
pub const EDGE_ZONE: f64 = 2.0;
/// The pointer poll period while polling is needed.
pub const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// What drives visibility right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The user hid the strip (menu or hotkey).
    Off,
    /// Shown, nothing in the way.
    Shown,
    /// Hidden until the pointer rests at the left edge (Apple's `AutoHide`, or an active
    /// window overlapping the strip area).
    EdgeOnly,
}

impl Mode {
    /// The mode for a snapshot's flags and whether an active window overlaps the strip.
    pub fn of(user_visible: bool, hide_recent_apps: bool, overlapped: bool) -> Self {
        if !user_visible {
            Self::Off
        } else if hide_recent_apps || overlapped {
            Self::EdgeOnly
        } else {
            Self::Shown
        }
    }
}

/// Whether any of `frames` intersects the strip area `strip` (global coordinates).
pub fn overlaps(frames: &[Rect], strip: &Rect) -> bool {
    frames.iter().any(|f| f.intersect(strip).is_some_and(|i| i.w > 0.5 && i.h > 0.5))
}

/// The answer for one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    /// Show the strip.
    pub shown: bool,
    /// Keep polling the pointer.
    pub poll: bool,
}

/// Edge-reveal state.
#[derive(Clone, Debug, Default)]
pub struct Visibility {
    dwell_since: Option<Instant>,
    left_since: Option<Instant>,
    revealed: bool,
}

impl Visibility {
    /// Advances the state with the pointer (global, top-left origin; `None` when unknown)
    /// and the strip area `strip` (global).
    pub fn step(&mut self, mode: Mode, pointer: Option<Point>, strip: &Rect, now: Instant) -> Decision {
        match mode {
            Mode::Off | Mode::Shown => {
                *self = Self::default();
                Decision { shown: mode == Mode::Shown, poll: false }
            }
            Mode::EdgeOnly => {
                let in_rows = |p: &Point| p.y >= strip.y && p.y <= strip.max_y();
                if self.revealed {
                    let inside = pointer.is_some_and(|p| strip.contains(p) || (p.x <= strip.x + EDGE_ZONE && in_rows(&p)));
                    if inside {
                        self.left_since = None;
                    } else {
                        let since = *self.left_since.get_or_insert(now);
                        if now.duration_since(since) >= LEAVE_DELAY {
                            *self = Self::default();
                        }
                    }
                } else {
                    let at_edge = pointer.is_some_and(|p| p.x <= strip.x + EDGE_ZONE && in_rows(&p));
                    if at_edge {
                        let since = *self.dwell_since.get_or_insert(now);
                        if now.duration_since(since) >= EDGE_DWELL {
                            self.revealed = true;
                            self.dwell_since = None;
                            self.left_since = None;
                        }
                    } else {
                        self.dwell_since = None;
                    }
                }
                Decision { shown: self.revealed, poll: true }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRIP: Rect = Rect::new(0.0, 30.0, 200.0, 1410.0);

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn modes_follow_the_flags() {
        assert_eq!(Mode::of(false, false, false), Mode::Off);
        assert_eq!(Mode::of(false, true, true), Mode::Off);
        assert_eq!(Mode::of(true, false, false), Mode::Shown);
        assert_eq!(Mode::of(true, true, false), Mode::EdgeOnly);
        assert_eq!(Mode::of(true, false, true), Mode::EdgeOnly);
    }

    #[test]
    fn overlap_needs_a_real_intersection() {
        assert!(overlaps(&[Rect::new(150.0, 100.0, 500.0, 400.0)], &STRIP));
        assert!(!overlaps(&[Rect::new(200.0, 100.0, 500.0, 400.0)], &STRIP));
        assert!(!overlaps(&[], &STRIP));
    }

    #[test]
    fn shown_and_off_never_poll() {
        let mut v = Visibility::default();
        let t = Instant::now();
        assert_eq!(v.step(Mode::Shown, None, &STRIP, t), Decision { shown: true, poll: false });
        assert_eq!(v.step(Mode::Off, None, &STRIP, t), Decision { shown: false, poll: false });
    }

    #[test]
    fn the_edge_reveals_after_a_rest_and_hides_after_leaving() {
        let mut v = Visibility::default();
        let t = Instant::now();
        let edge = Some(Point::new(1.0, 500.0));
        let inside = Some(Point::new(120.0, 500.0));
        let away = Some(Point::new(900.0, 500.0));
        assert!(!v.step(Mode::EdgeOnly, away, &STRIP, t).shown);
        assert!(!v.step(Mode::EdgeOnly, edge, &STRIP, ms(t, 0)).shown);
        assert!(!v.step(Mode::EdgeOnly, edge, &STRIP, ms(t, 200)).shown);
        assert!(v.step(Mode::EdgeOnly, edge, &STRIP, ms(t, 260)).shown);
        // Moving into the strip keeps it.
        assert!(v.step(Mode::EdgeOnly, inside, &STRIP, ms(t, 2000)).shown);
        // Leaving: still shown before the delay, hidden after it.
        assert!(v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 2100)).shown);
        assert!(v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 2500)).shown);
        assert!(!v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 2610)).shown);
    }

    #[test]
    fn passing_the_edge_quickly_does_not_reveal() {
        let mut v = Visibility::default();
        let t = Instant::now();
        assert!(!v.step(Mode::EdgeOnly, Some(Point::new(0.0, 500.0)), &STRIP, t).shown);
        assert!(!v.step(Mode::EdgeOnly, Some(Point::new(40.0, 500.0)), &STRIP, ms(t, 120)).shown);
        assert!(!v.step(Mode::EdgeOnly, Some(Point::new(0.0, 500.0)), &STRIP, ms(t, 300)).shown);
        assert!(!v.step(Mode::EdgeOnly, Some(Point::new(0.0, 500.0)), &STRIP, ms(t, 500)).shown);
        assert!(v.step(Mode::EdgeOnly, Some(Point::new(0.0, 500.0)), &STRIP, ms(t, 560)).shown);
        // The edge outside the strip's rows (the menu bar) does not count.
        let mut v = Visibility::default();
        assert!(!v.step(Mode::EdgeOnly, Some(Point::new(0.0, 5.0)), &STRIP, t).shown);
        assert!(!v.step(Mode::EdgeOnly, Some(Point::new(0.0, 5.0)), &STRIP, ms(t, 400)).shown);
    }

    #[test]
    fn a_window_going_away_shows_the_strip_and_resets_the_reveal() {
        let mut v = Visibility::default();
        let t = Instant::now();
        let edge = Some(Point::new(0.0, 500.0));
        v.step(Mode::EdgeOnly, edge, &STRIP, t);
        assert!(v.step(Mode::EdgeOnly, edge, &STRIP, ms(t, 300)).shown);
        assert!(v.step(Mode::Shown, edge, &STRIP, ms(t, 400)).shown);
        // A window overlaps again while the pointer is away: hidden at once.
        assert!(!v.step(Mode::EdgeOnly, Some(Point::new(900.0, 500.0)), &STRIP, ms(t, 500)).shown);
    }
}
