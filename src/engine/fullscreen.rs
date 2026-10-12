//! Is the shown Space a Desktop or a full-screen app's? Steadied over macOS's full-screen
//! animation.
//!
//! Entering or leaving full screen, macOS shows a short-lived window covering the whole
//! display (no title, subrole `AXUnknown`) while the app's window animates, and the Space
//! change notification comes in the middle of it: the full-screen window may not report
//! `AXFullScreen` yet, or the Desktop's windows may not be on screen yet. Taken at face
//! value, that moment looks like a new Desktop holding one window (measured 10-12: new
//! synthetic Space ids minted on every toggle, the Desktop's id lost, its stages merged).
//!
//! So the platform classifies what it sees ([`Seen`]): a window it knows is full screen
//! means a full-screen Space; a window covering the whole display (menu bar included)
//! without one means a transition; anything else is a Desktop. [`Settle`] waits out a
//! transition, at most [`SETTLE_MAX`] (a borderless window covering the display that is not
//! full screen is an ordinary window on a Desktop).

use std::time::{Duration, Instant};

use super::geometry::Rect;

/// The longest a transition is waited out.
pub const SETTLE_MAX: Duration = Duration::from_secs(2);

/// What the window list shows right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// A Desktop: no window covers the display.
    Desktop,
    /// A window on screen is in full screen.
    FullScreen,
    /// A window covers the whole display but none is in full screen (yet): an animation.
    Transition,
}

impl Seen {
    /// From the number of on-screen windows covering the display and whether one of the
    /// windows on screen is in full screen.
    pub fn of(covering: usize, fullscreen: bool) -> Self {
        if fullscreen {
            Self::FullScreen
        } else if covering > 0 {
            Self::Transition
        } else {
            Self::Desktop
        }
    }
}

/// What to do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A Desktop shows: recognize it by its windows.
    Desktop,
    /// A full-screen app's Space shows: no strip, nothing changes.
    FullScreen,
    /// An animation runs: count nothing yet, look again shortly.
    Wait,
}

/// Waits out full-screen transitions.
#[derive(Debug, Clone, Copy, Default)]
pub struct Settle {
    since: Option<Instant>,
}

impl Settle {
    /// The verdict for what is seen at `now`: a transition is waited out for at most
    /// [`SETTLE_MAX`] from when it was first seen, then taken for a Desktop.
    pub fn step(&mut self, seen: Seen, now: Instant) -> Verdict {
        match seen {
            Seen::Desktop => {
                self.since = None;
                Verdict::Desktop
            }
            Seen::FullScreen => {
                self.since = None;
                Verdict::FullScreen
            }
            Seen::Transition => {
                let since = *self.since.get_or_insert(now);
                if now.duration_since(since) < SETTLE_MAX { Verdict::Wait } else { Verdict::Desktop }
            }
        }
    }

    /// True while a transition is being waited out.
    pub fn waiting(&self, now: Instant) -> bool {
        self.since.is_some_and(|s| now.duration_since(s) < SETTLE_MAX)
    }
}

/// True when `bounds` covers the whole `display` (within a point): a full-screen window,
/// or the animation's stand-in.
pub fn covers_display(bounds: Rect, display: Rect) -> bool {
    (bounds.x - display.x).abs() < 1.0
        && (bounds.y - display.y).abs() < 1.0
        && (bounds.w - display.w).abs() < 1.0
        && (bounds.h - display.h).abs() < 1.0
}

/// True when `bounds` is big enough to be a full-screen window worth asking Accessibility
/// about (most of the display; some displays keep a band at the top).
pub fn could_be_fullscreen(bounds: Rect, display: Rect) -> bool {
    bounds.w >= display.w * 0.9 && bounds.h >= display.h * 0.85
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);

    #[test]
    fn what_is_seen() {
        assert_eq!(Seen::of(0, false), Seen::Desktop);
        assert_eq!(Seen::of(1, false), Seen::Transition);
        assert_eq!(Seen::of(1, true), Seen::FullScreen);
        assert_eq!(Seen::of(0, true), Seen::FullScreen, "a full-screen window below a top band");
    }

    #[test]
    fn entering_full_screen_waits_for_the_animation() {
        // The measured sequence: the stand-in covers the display, the Space changes, then
        // the app's window reports full screen.
        let t0 = Instant::now();
        let mut s = Settle::default();
        assert_eq!(s.step(Seen::Transition, t0), Verdict::Wait);
        assert!(s.waiting(t0 + Duration::from_millis(500)));
        assert_eq!(s.step(Seen::Transition, t0 + Duration::from_millis(500)), Verdict::Wait);
        assert_eq!(s.step(Seen::FullScreen, t0 + Duration::from_millis(1000)), Verdict::FullScreen);
        assert!(!s.waiting(t0 + Duration::from_millis(1000)));
    }

    #[test]
    fn leaving_full_screen_waits_for_the_desktop() {
        let t0 = Instant::now();
        let mut s = Settle::default();
        assert_eq!(s.step(Seen::FullScreen, t0), Verdict::FullScreen);
        assert_eq!(s.step(Seen::Transition, t0 + Duration::from_millis(100)), Verdict::Wait);
        assert_eq!(s.step(Seen::Desktop, t0 + Duration::from_millis(700)), Verdict::Desktop);
    }

    #[test]
    fn a_window_that_covers_the_display_but_stays_is_a_desktop_window() {
        let t0 = Instant::now();
        let mut s = Settle::default();
        assert_eq!(s.step(Seen::Transition, t0), Verdict::Wait);
        assert_eq!(s.step(Seen::Transition, t0 + SETTLE_MAX), Verdict::Desktop);
        assert_eq!(s.step(Seen::Transition, t0 + SETTLE_MAX * 3), Verdict::Desktop, "no new wait while it stays");
        assert!(!s.waiting(t0 + SETTLE_MAX * 3));
    }

    #[test]
    fn display_cover() {
        assert!(covers_display(Rect::new(0.0, 0.0, 2560.0, 1440.0), D));
        assert!(covers_display(Rect::new(0.4, -0.3, 2560.2, 1440.0), D));
        assert!(!covers_display(Rect::new(0.0, 30.0, 2560.0, 1410.0), D), "a zoomed window below the menu bar");
        assert!(could_be_fullscreen(Rect::new(0.0, 32.0, 2560.0, 1408.0), D));
        assert!(!could_be_fullscreen(Rect::new(80.0, 78.0, 520.0, 392.0), D));
    }
}
