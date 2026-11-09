//! When the strip shows, pure: the user's toggle, Apple's "Show recent apps" (`AutoHide`),
//! auto-hide while an active window reaches the screen's left edge ([`hides_strip`]), and the
//! left-edge reveal.
//!
//! The strip feeds this with the pointer position and the current time; it answers whether the
//! strip shows. The pointer is polled (every ~60 ms) only while [`Decision::poll`] says so: a
//! clock runs (the rest at the edge, the delay after leaving) or the strip is revealed (the
//! pointer is then over the strip's own window, which the global monitor never hears).
//! Otherwise the global monitor's mouse moves wake it ([`Visibility::wakes`]): a 60 ms timer
//! ran for as long as any window covered the strip (the review's U6).
//!
//! Extensions (Apple has neither): the **edge tab** ([`Visibility::set_tab`]), a slim tab
//! at the left edge shown while a window hides the strip, reveals it on hover with the same
//! dwell as the edge and at once on click ([`Visibility::reveal_now`]); and the launch
//! **announce** ([`Visibility::announce`]) shows a covered strip for a few seconds.

use std::time::{Duration, Instant};

use crate::config::RevealStyle;
use crate::engine::geometry::{Point, Rect};

/// Timings and the edge zone (`[reveal]` in config).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    /// The pointer must rest this long at the left edge to reveal the strip.
    pub edge_dwell: Duration,
    /// A revealed strip hides this long after the pointer leaves it.
    pub leave_delay: Duration,
    /// The left edge zone, in points from the strip's left edge.
    pub edge_zone: f64,
}

impl Timing {
    /// From the `[reveal]` keys.
    pub fn from_style(r: &RevealStyle) -> Self {
        Self {
            edge_dwell: Duration::from_millis(r.delay_ms),
            leave_delay: Duration::from_millis(r.hide_delay_ms),
            edge_zone: r.edge_width,
        }
    }
}

impl Default for Timing {
    fn default() -> Self {
        Self::from_style(&crate::config::StripStyle::default().reveal)
    }
}

/// What drives visibility right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The user hid the strip (menu or hotkey).
    Off,
    /// Shown, nothing in the way.
    Shown,
    /// Hidden until the pointer rests at the left edge (Apple's `AutoHide`, or an active
    /// window at the screen's left edge, [`hides_strip`]).
    EdgeOnly,
}

impl Mode {
    /// The mode for a snapshot's flags and whether an active window hides the strip
    /// ([`hides_strip`]).
    pub fn of(user_visible: bool, hide_recent_apps: bool, covered: bool) -> Self {
        if !user_visible {
            Self::Off
        } else if hide_recent_apps || covered {
            Self::EdgeOnly
        } else {
            Self::Shown
        }
    }
}

/// How close to the screen's left edge (the strip's left edge) a window's left edge must come
/// for the strip to slide away, in points. Apple's strip stays up under a window that covers
/// the cards and goes only once the window covers the cards' left edge too: recorded 11-09 on
/// a Studio Display, it stayed up with a dragged window's left edge at 52 pt and slid away at
/// 15 pt, its cards starting 16 pt from the edge. 20 pt is the cards' edge and a few points
/// (the exact point between 16 and 52 pt was not measured). The owner's eye of 11-05 put it at
/// about 50 pt, which the recordings showed too early; before that, ours hid at any overlap of
/// the column.
pub const COVER_EDGE: f64 = 20.0;

/// What may hold the strip above windows ([`lifted`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lift {
    /// `[strip] level = "above-windows"`.
    pub always: bool,
    /// A card of the strip is being dragged.
    pub card_drag: bool,
    /// A switch is starting or running while the strip is up (the leaving windows still
    /// cover it until their snapshots land).
    pub transition: bool,
    /// The edge reveals the strip over a window that covers it.
    pub edge_reveal: bool,
    /// A window is being dragged with the pointer over the strip column.
    pub window_drag_over: bool,
}

/// Whether the strip goes above windows. A window dragged over it is not a reason: Apple's
/// cards stay under the dragged window (recorded 11-09); its card-sized picture follows the
/// pointer above everything instead, and the drop into the strip works as before.
pub fn lifted(l: Lift) -> bool {
    l.always || l.card_drag || l.transition || l.edge_reveal
}

/// Whether any of `frames` (the active stage's shown windows) hides the strip whose area is
/// `strip` (global coordinates): it reaches into the strip's area and its left edge is within
/// [`COVER_EDGE`] of the strip's left edge (or past it). A window over the cards but further
/// right leaves the strip shown, under the window.
pub fn hides_strip(frames: &[Rect], strip: &Rect) -> bool {
    frames.iter().any(|f| f.x <= strip.x + COVER_EDGE && f.intersect(strip).is_some_and(|i| i.w > 0.5 && i.h > 0.5))
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
    /// The last step was [`Mode::Shown`]: a strip in plain view that a window now covers
    /// stays revealed while the pointer is on it.
    was_shown: bool,
    /// Shown regardless of the pointer until then (the launch announce).
    hold_until: Option<Instant>,
    /// The edge tab's area (global), when it shows: resting on it counts as the edge.
    tab: Option<Rect>,
    /// The left edge of the strip's screen (global x): the edge zone starts there, so a
    /// pointer on a screen further left is never at this strip's edge (the review's
    /// S2). `None` until the strip says (unbounded).
    screen_left: Option<f64>,
    /// Delays and the edge zone.
    timing: Timing,
}

impl Visibility {
    /// The edge tab's area while it shows (global), or `None`.
    pub fn set_tab(&mut self, tab: Option<Rect>) {
        self.tab = tab;
    }

    /// The left edge of the strip's screen (global x; [`Self::at_edge`]).
    pub fn set_screen_left(&mut self, x: f64) {
        self.screen_left = Some(x);
    }

    /// True when `p` (global) is in the left edge zone by the strip's rows: from the strip's
    /// screen's left edge to [`Timing::edge_zone`] past the strip's left edge (the Dock on the
    /// left lies between them). A pointer on a screen to the left is not.
    fn at_edge(&self, p: Point, strip: &Rect) -> bool {
        p.x <= strip.x + self.timing.edge_zone && self.screen_left.is_none_or(|l| p.x >= l) && p.y >= strip.y && p.y <= strip.max_y()
    }

    /// Sets the delays and the edge zone (config reload).
    pub fn set_timing(&mut self, timing: Timing) {
        self.timing = timing;
    }

    /// Reveals the strip now (a click on the edge tab); it hides again once the pointer has
    /// been away from it for the leave delay.
    pub fn reveal_now(&mut self) {
        self.revealed = true;
        self.dwell_since = None;
        self.left_since = None;
    }

    /// Shows the strip until `now + for_how_long` whatever the pointer does, then hides it as
    /// a revealed strip does.
    pub fn announce(&mut self, now: Instant, for_how_long: Duration) {
        self.reveal_now();
        self.hold_until = Some(now + for_how_long);
    }

    /// Drops the carry-over of a strip in plain view ([`Self::step`]): only a switch the strip
    /// itself started (a card click) keeps it under the pointer; a window resized to the
    /// screen's left edge or a keyboard switch hides it, as Apple's does.
    pub fn forget_shown(&mut self) {
        self.was_shown = false;
    }

    /// True when the pointer at `p` (global) starts something while hidden at the edge: it is
    /// at the left edge by the strip's rows, or on the edge tab. Between those, moves need no
    /// look.
    pub fn wakes(&self, p: Point, strip: &Rect) -> bool {
        self.at_edge(p, strip) || self.tab.is_some_and(|t| t.contains(p))
    }

    /// Advances the state with the pointer (global, top-left origin; `None` when unknown)
    /// and the strip area `strip` (global).
    pub fn step(&mut self, mode: Mode, pointer: Option<Point>, strip: &Rect, now: Instant) -> Decision {
        match mode {
            Mode::Off | Mode::Shown => {
                *self = Self { tab: self.tab, timing: self.timing, screen_left: self.screen_left, was_shown: mode == Mode::Shown, ..Self::default() };
                Decision { shown: mode == Mode::Shown, poll: false }
            }
            Mode::EdgeOnly => {
                // Shown a moment ago with the pointer on it (a card click to a stage whose
                // window covers the strip): it stays until the pointer leaves, as after an
                // edge reveal (10-13: it slid away under the pointer, and the clicked
                // stage's window grew from its card already off screen).
                if std::mem::take(&mut self.was_shown) && pointer.is_some_and(|p| strip.contains(p)) {
                    self.revealed = true;
                    self.dwell_since = None;
                    self.left_since = None;
                }
                let on_tab = |p: &Point| self.tab.is_some_and(|t| t.contains(*p));
                let held = self.hold_until.is_some_and(|t| now < t);
                if !held {
                    self.hold_until = None;
                }
                if self.revealed {
                    let inside = pointer.is_some_and(|p| strip.contains(p) || self.at_edge(p, strip));
                    if inside || held {
                        self.left_since = None;
                    } else {
                        let since = *self.left_since.get_or_insert(now);
                        if now.duration_since(since) >= self.timing.leave_delay {
                            *self = Self { tab: self.tab, timing: self.timing, screen_left: self.screen_left, ..Self::default() };
                        }
                    }
                } else {
                    let at_edge = pointer.is_some_and(|p| self.at_edge(p, strip) || on_tab(&p));
                    if at_edge {
                        let since = *self.dwell_since.get_or_insert(now);
                        if now.duration_since(since) >= self.timing.edge_dwell {
                            self.revealed = true;
                            self.dwell_since = None;
                            self.left_since = None;
                        }
                    } else {
                        self.dwell_since = None;
                    }
                }
                Decision { shown: self.revealed, poll: self.revealed || self.dwell_since.is_some() }
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

    /// Apple's cards stay under a window dragged over them (11-09): the drag alone never lifts
    /// the strip; a card drag, a switch, the edge reveal and the config's level still do.
    #[test]
    fn a_window_dragged_over_the_strip_leaves_it_below_windows() {
        assert!(!lifted(Lift { window_drag_over: true, ..Lift::default() }));
        assert!(!lifted(Lift::default()));
        assert!(lifted(Lift { always: true, ..Lift::default() }));
        assert!(lifted(Lift { card_drag: true, ..Lift::default() }));
        assert!(lifted(Lift { transition: true, window_drag_over: true, ..Lift::default() }));
        assert!(lifted(Lift { edge_reveal: true, ..Lift::default() }));
    }

    #[test]
    fn modes_follow_the_flags() {
        assert_eq!(Mode::of(false, false, false), Mode::Off);
        assert_eq!(Mode::of(false, true, true), Mode::Off);
        assert_eq!(Mode::of(true, false, false), Mode::Shown);
        assert_eq!(Mode::of(true, true, false), Mode::EdgeOnly);
        assert_eq!(Mode::of(true, false, true), Mode::EdgeOnly);
    }

    /// Only a window that covers the cards' left edge hides the strip; one over the cards
    /// further right leaves it shown, as Apple's does (recorded 11-09: shown with the window's
    /// left edge at 52 pt, slid away at 15 pt; the cards start at 16 pt).
    #[test]
    fn only_a_window_at_the_left_edge_hides_the_strip() {
        // Every one covers the cards: a strip 320 pt wide (his Studio Display's cards end near
        // 308 pt).
        let wide = Rect::new(0.0, 30.0, 320.0, 1410.0);
        let at = |x: f64| Rect::new(x, 100.0, 900.0, 400.0);
        assert!(hides_strip(&[at(0.0)], &wide), "flush left");
        assert!(hides_strip(&[at(15.0)], &wide), "15 pt in: Apple's slid away");
        assert!(hides_strip(&[at(20.0)], &wide), "20 pt in: the threshold itself");
        assert!(!hides_strip(&[at(21.0)], &wide), "21 pt in: shown, under the window");
        assert!(!hides_strip(&[at(50.0)], &wide), "50 pt in: Apple's stayed up (52 pt)");
        assert!(!hides_strip(&[at(300.0)], &wide), "300 pt in, over the cards: shown");
        assert!(!hides_strip(&[at(320.0)], &wide), "clear of the column");
        assert!(hides_strip(&[at(-40.0)], &wide), "past the left edge");
        assert!(hides_strip(&[at(300.0), at(10.0)], &STRIP), "any one of the stage's windows");
        // The threshold counts from the screen's left edge (a screen further right).
        let right = Rect::new(1440.0, 30.0, 200.0, 1410.0);
        assert!(hides_strip(&[Rect::new(1455.0, 100.0, 900.0, 400.0)], &right));
        assert!(!hides_strip(&[Rect::new(1480.0, 100.0, 900.0, 400.0)], &right));
        // At the edge but not in the strip's area (another screen, above its rows): shown.
        assert!(!hides_strip(&[Rect::new(0.0, 1500.0, 900.0, 400.0)], &STRIP));
        assert!(!hides_strip(&[Rect::new(-950.0, 100.0, 900.0, 400.0)], &STRIP));
        assert!(!hides_strip(&[], &STRIP));
    }

    #[test]
    fn a_strip_in_view_that_a_window_covers_stays_under_the_pointer() {
        let mut v = Visibility::default();
        let t = Instant::now();
        let inside = Some(Point::new(120.0, 500.0));
        let away = Some(Point::new(900.0, 500.0));
        assert!(v.step(Mode::Shown, None, &STRIP, t).shown);
        assert!(v.step(Mode::EdgeOnly, inside, &STRIP, ms(t, 10)).shown, "the pointer on it: kept");
        assert!(v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 20)).shown, "leaving: the delay first");
        assert!(!v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 5000)).shown, "then hidden");
        // The pointer elsewhere when the window comes: hidden at once, as before.
        let mut v = Visibility::default();
        assert!(v.step(Mode::Shown, None, &STRIP, t).shown);
        assert!(!v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 10)).shown);
        // Only right after Shown: a later pass over the strip needs the edge.
        assert!(!v.step(Mode::EdgeOnly, inside, &STRIP, ms(t, 20)).shown);
        // Not a card click (a resize into the column, a keyboard switch): forgotten, hidden.
        let mut v = Visibility::default();
        assert!(v.step(Mode::Shown, None, &STRIP, t).shown);
        v.forget_shown();
        assert!(!v.step(Mode::EdgeOnly, inside, &STRIP, ms(t, 10)).shown);
    }

    #[test]
    fn shown_and_off_never_poll() {
        let mut v = Visibility::default();
        let t = Instant::now();
        assert_eq!(v.step(Mode::Shown, None, &STRIP, t), Decision { shown: true, poll: false });
        assert_eq!(v.step(Mode::Off, None, &STRIP, t), Decision { shown: false, poll: false });
    }

    /// U6: hidden at the edge with the pointer elsewhere nothing is polled (mouse moves wake
    /// it); a rest at the edge, a revealed strip and the delay after leaving it poll.
    #[test]
    fn hidden_at_the_edge_polls_only_while_a_clock_runs() {
        let mut v = Visibility::default();
        let t = Instant::now();
        let edge = Some(Point::new(1.0, 500.0));
        let away = Some(Point::new(900.0, 500.0));
        assert_eq!(v.step(Mode::EdgeOnly, away, &STRIP, t), Decision { shown: false, poll: false });
        assert!(!v.wakes(Point::new(900.0, 500.0), &STRIP));
        assert!(v.wakes(Point::new(1.0, 500.0), &STRIP));
        assert!(!v.wakes(Point::new(1.0, 5.0), &STRIP), "above the strip's rows");
        assert_eq!(v.step(Mode::EdgeOnly, edge, &STRIP, ms(t, 10)), Decision { shown: false, poll: true });
        assert_eq!(v.step(Mode::EdgeOnly, edge, &STRIP, ms(t, 400)), Decision { shown: true, poll: true });
        assert_eq!(v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 500)), Decision { shown: true, poll: true }, "leaving: the delay runs");
        assert_eq!(v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 5000)), Decision { shown: false, poll: false });
        // A rest begun and broken off: polled once more, then idle.
        assert!(v.step(Mode::EdgeOnly, edge, &STRIP, ms(t, 6000)).poll);
        assert!(!v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 6010)).poll);
        // The edge tab wakes it too.
        v.set_tab(Some(Rect::new(0.0, 400.0, 10.0, 72.0)));
        assert!(v.wakes(Point::new(8.0, 420.0), &STRIP));
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

    /// The review's S2: the laptop left of the main screen (global x below 0); a
    /// pointer on it was at the main strip's edge, revealed it and held it open.
    #[test]
    fn a_pointer_on_a_screen_to_the_left_is_not_at_the_strips_edge() {
        let mut v = Visibility::default();
        v.set_screen_left(0.0);
        let t = Instant::now();
        let laptop = Some(Point::new(-600.0, 500.0));
        let seam = Some(Point::new(-1.0, 500.0));
        let edge = Some(Point::new(0.0, 500.0));
        assert!(!v.wakes(Point::new(-600.0, 500.0), &STRIP) && !v.wakes(Point::new(-1.0, 500.0), &STRIP));
        assert!(v.wakes(Point::new(0.0, 500.0), &STRIP));
        assert!(!v.step(Mode::EdgeOnly, laptop, &STRIP, t).shown);
        assert!(!v.step(Mode::EdgeOnly, laptop, &STRIP, ms(t, 1000)).poll, "no rest begins there");
        assert!(!v.step(Mode::EdgeOnly, seam, &STRIP, ms(t, 2000)).shown);
        assert!(!v.step(Mode::EdgeOnly, seam, &STRIP, ms(t, 3000)).shown);
        // Revealed from its own edge, it hides once the pointer has gone onto the laptop.
        assert!(!v.step(Mode::EdgeOnly, edge, &STRIP, ms(t, 4000)).shown);
        assert!(v.step(Mode::EdgeOnly, edge, &STRIP, ms(t, 4300)).shown);
        assert!(v.step(Mode::EdgeOnly, laptop, &STRIP, ms(t, 4400)).shown, "the leave delay");
        assert!(!v.step(Mode::EdgeOnly, laptop, &STRIP, ms(t, 5000)).shown, "hidden: the laptop does not hold it");
        // The bound survives a mode change; the Dock on the left (between the screen's edge
        // and the strip) is still the edge.
        v.step(Mode::Shown, None, &STRIP, ms(t, 6000));
        let docked = Rect::new(70.0, 30.0, 200.0, 1410.0);
        assert!(!v.wakes(Point::new(-600.0, 500.0), &docked));
        assert!(v.wakes(Point::new(2.0, 500.0), &docked));
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
    fn the_edge_tab_reveals_on_a_rest_and_at_once_on_a_click() {
        let mut v = Visibility::default();
        let t = Instant::now();
        let tab = Rect::new(0.0, 700.0, 10.0, 72.0);
        v.set_tab(Some(tab));
        let on_tab = Some(Point::new(8.0, 730.0));
        assert!(!v.step(Mode::EdgeOnly, on_tab, &STRIP, t).shown);
        assert!(!v.step(Mode::EdgeOnly, on_tab, &STRIP, ms(t, 200)).shown);
        assert!(v.step(Mode::EdgeOnly, on_tab, &STRIP, ms(t, 260)).shown, "same dwell as the edge");
        let mut v = Visibility::default();
        v.set_tab(Some(tab));
        v.reveal_now();
        assert!(v.step(Mode::EdgeOnly, on_tab, &STRIP, t).shown, "a click shows it at once");
        let away = Some(Point::new(900.0, 500.0));
        assert!(v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 100)).shown);
        assert!(!v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 700)).shown, "and it hides after the pointer leaves");
    }

    #[test]
    fn the_announce_holds_the_strip_then_lets_auto_hide_resume() {
        let mut v = Visibility::default();
        let t = Instant::now();
        let away = Some(Point::new(900.0, 500.0));
        v.announce(t, Duration::from_secs(3));
        assert!(v.step(Mode::EdgeOnly, away, &STRIP, t).shown);
        assert!(v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 2900)).shown, "held while announced");
        assert!(v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 3100)).shown, "then the leave delay");
        assert!(!v.step(Mode::EdgeOnly, away, &STRIP, ms(t, 3700)).shown, "auto-hide resumes");
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
