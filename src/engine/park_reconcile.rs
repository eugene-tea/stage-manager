//! A parked window the screen contradicts, decided every tick (the Fable review of 10-29:
//! L1, K2, S5 and E7 are one problem, the model's `parked` flag and the screen disagreeing,
//! reconciled until then only by a Space arrival's `match_screen`).
//!
//! The model has a window parked; the window server lists it on the shown Desktop outside
//! the park zone (an app that re-centred its own window, one restoring its saved bounds, a
//! window macOS pushed back after a Dock or display change, the guardian of a crashed copy
//! moving it out a moment after this copy parked it), or the `Park` that should have put it
//! there failed (a busy app, one mid-launch). Each such window has a [`Stray`] record, and
//! [`step`] says what to do with it at a tick:
//!
//! - **Nothing our own action still moves is judged.** A window inside our park's echo
//!   window ([`Facts::echo`]: a park, an unminimize to the strip, a transition's raise), or
//!   one the window server lists somewhere else than at the last tick, waits
//!   ([`Verdict::Wait`]): where it is now says nothing yet. Live 10-29: ⌘M parked a window
//!   and un-minimized it there; macOS's un-minimize focused it and set it down out of the
//!   zone, and the tick adopted it into the active stage as "focused", so the card it had
//!   just been given went away again.
//! - **The user's choice wins.** A window the user moved while parked (a drag with the
//!   button down), or one the user focused since our last park of it (a click on it, ⌘`,
//!   the app's own menu; [`Facts::focused`], never macOS's or our own focus), is adopted
//!   as shown in the active stage where it is ([`Verdict::Adopt`]).
//! - **Otherwise it is parked again**, after it has been listed at the same place out of
//!   the zone on [`SETTLE_TICKS`] ticks in a row (one listing can lie: a Space slide's
//!   first frame, a window on its way), with a backoff between tries ([`backoff`]) and at
//!   most [`MAX_TRIES`] tries; then the reconciler gives up on it and says so once
//!   ([`Verdict::GiveUp`]), rather than moving a window its app keeps moving back forever.
//! - A window listed in the zone is settled ([`Verdict::Settled`]): forget it.
//!
//! The caller skips whole ticks when the list cannot be trusted (a Space change settling, a
//! Desktop slide, Mission Control, a transition, an arrangement made ahead, a drag), and a
//! window whose app is busy.

use std::time::{Duration, Instant};

use crate::engine::geometry::Rect;

/// Ticks in a row a window is listed at one place out of the zone before it is parked again.
pub const SETTLE_TICKS: u32 = 2;

/// Listed bounds this far apart (a point: the window server rounds) are the same place.
const SAME_PLACE: f64 = 1.0;

/// Parks tried before the reconciler gives a window up.
pub const MAX_TRIES: u32 = 4;

/// Never again (a stray is forgotten once settled or adopted; a given-up one stays so).
const BACKOFF: [Duration; 4] = [Duration::ZERO, Duration::from_millis(500), Duration::from_secs(1), Duration::from_secs(2)];

/// How long after park `tries` the next park may go: at once, then half a second, a second,
/// two seconds.
pub fn backoff(tries: u32) -> Duration {
    BACKOFF[(tries as usize).min(BACKOFF.len() - 1)]
}

/// Where the window server lists a stray at a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// On screen, in the park zone: parked in fact.
    InZone,
    /// On screen, out of the park zone.
    OutOfZone,
    /// Not on screen (its app hidden, another Desktop, closing).
    NotListed,
}

/// What a tick knows of a stray.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Facts {
    /// Where the window server lists it.
    pub seen: Seen,
    /// Its listed bounds when [`Seen::OutOfZone`] (else anything): a window listed somewhere
    /// else than at the last tick is still moving.
    pub bounds: Rect,
    /// It is the focused window, and the user made it so since our last park of it (a click
    /// on it, ⌘`, the app's own menu). macOS's focus at an un-minimize, or the echo of our
    /// own, is not the user's choice.
    pub focused: bool,
    /// The user moved it while the model had it parked (a drag).
    pub user_moved: bool,
    /// Its app did not answer lately ([`crate::engine::busy_apps`]): nothing is asked of it.
    pub busy: bool,
    /// Our own park (or raise, or un-minimize) went out a moment ago and may still be
    /// landing: nothing about the window is judged yet.
    pub echo: bool,
}

impl Facts {
    /// Listed out of the zone at `bounds`, nothing else known.
    pub fn out_at(bounds: Rect) -> Self {
        Self { seen: Seen::OutOfZone, bounds, focused: false, user_moved: false, busy: false, echo: false }
    }
}

/// What to do with a stray at a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Listed in the zone: parked in fact; forget it.
    Settled,
    /// The user's: it joins the active stage as shown, where it is.
    Adopt,
    /// Park it again.
    Park,
    /// Nothing this tick.
    Wait,
    /// Tried enough: given up, to be said once.
    GiveUp,
}

/// One window the screen contradicts.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stray {
    /// Ticks in a row it was listed out of the zone at the same place.
    pub seen_out: u32,
    /// Where it was listed at the last tick (out of the zone).
    pub at: Option<Rect>,
    /// Parks sent by the reconciler.
    pub tries: u32,
    /// The next park may go from here.
    pub next: Option<Instant>,
    /// The reconciler gave it up (said once).
    pub gave_up: bool,
    /// The user moved it while parked (noted by the caller from a move with the button down).
    pub user_moved: bool,
    /// Since when it is known (its park failed, or it was first seen out of the zone).
    pub since: Option<Instant>,
}

impl Stray {
    /// A window whose park just failed: tried once already (the batch's), so the next try
    /// waits a backoff, not a tick.
    pub fn park_failed(now: Instant) -> Self {
        Self { seen_out: SETTLE_TICKS, tries: 1, next: Some(now + backoff(1)), since: Some(now), ..Self::default() }
    }
}

/// True when listed bounds `a` and `b` are the same place ([`SAME_PLACE`]).
fn same_place(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= SAME_PLACE && (a.y - b.y).abs() <= SAME_PLACE && (a.w - b.w).abs() <= SAME_PLACE && (a.h - b.h).abs() <= SAME_PLACE
}

/// The verdict for stray `s` with `f` at `now`, with `s` brought up to date.
pub fn step(s: &mut Stray, f: Facts, now: Instant) -> Verdict {
    if s.since.is_none() {
        s.since = Some(now);
    }
    match f.seen {
        Seen::InZone => return Verdict::Settled,
        Seen::NotListed => {
            s.seen_out = 0;
            s.at = None;
            return Verdict::Wait;
        }
        Seen::OutOfZone => {
            // Listed somewhere else than last time: still moving; the count starts again.
            let moved = s.at.is_some_and(|at| !same_place(at, f.bounds));
            s.at = Some(f.bounds);
            s.seen_out = if moved { 1 } else { s.seen_out.saturating_add(1) };
        }
    }
    if f.user_moved || s.user_moved {
        return Verdict::Adopt;
    }
    if f.echo {
        // Our own action may still be landing: where the window is, and who focused it, say
        // nothing yet.
        return Verdict::Wait;
    }
    if f.focused {
        return Verdict::Adopt;
    }
    if f.busy || s.gave_up || s.seen_out < SETTLE_TICKS || s.next.is_some_and(|t| now < t) {
        return Verdict::Wait;
    }
    if s.tries >= MAX_TRIES {
        s.gave_up = true;
        return Verdict::GiveUp;
    }
    s.tries += 1;
    s.next = Some(now + backoff(s.tries));
    Verdict::Park
}

#[cfg(test)]
mod tests {
    use super::*;

    const MID: Rect = Rect::new(1020.0, 539.0, 520.0, 392.0);
    const OUT: Facts = Facts { seen: Seen::OutOfZone, bounds: MID, focused: false, user_moved: false, busy: false, echo: false };

    /// L1: a model-parked window listed mid-screen is parked again after two ticks, then with
    /// a backoff, and given up after four tries, said once.
    #[test]
    fn a_window_listed_out_of_the_zone_is_parked_again_with_backoff_then_given_up() {
        let t0 = Instant::now();
        let mut s = Stray::default();
        assert_eq!(step(&mut s, OUT, t0), Verdict::Wait, "one listing can lie");
        assert_eq!(step(&mut s, OUT, t0 + Duration::from_millis(250)), Verdict::Park, "seen twice: parked again");
        assert_eq!(s.tries, 1);
        assert_eq!(step(&mut s, OUT, t0 + Duration::from_millis(500)), Verdict::Wait, "the backoff");
        assert_eq!(step(&mut s, OUT, t0 + Duration::from_millis(800)), Verdict::Park, "half a second later: again");
        assert_eq!(step(&mut s, OUT, t0 + Duration::from_millis(1900)), Verdict::Park, "a second later");
        assert_eq!(step(&mut s, OUT, t0 + Duration::from_millis(4000)), Verdict::Park, "two seconds later: the fourth");
        assert_eq!(step(&mut s, OUT, t0 + Duration::from_millis(7000)), Verdict::GiveUp, "tried enough");
        assert!(s.gave_up);
        assert_eq!(step(&mut s, OUT, t0 + Duration::from_millis(8000)), Verdict::Wait, "said once; nothing more");
        assert_eq!(step(&mut s, Facts { seen: Seen::InZone, ..OUT }, t0 + Duration::from_secs(9)), Verdict::Settled, "back in the zone: forgotten");
    }

    /// Live 10-29, the parity phase's ⌘M: the window is parked and un-minimized there; macOS
    /// focuses it and sets it down out of the zone (listed growing for 200 ms, then still).
    /// Inside our echo nothing is judged, its focus included; past it the window, still
    /// focused by macOS and not by the user, is parked again, not adopted.
    #[test]
    fn a_window_our_unminimize_focused_out_of_the_zone_is_parked_again_not_adopted() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut s = Stray::default();
        let growing = Rect::new(1794.0, 1431.0, 70.0, 84.0);
        let grown = Rect::new(1794.0, 1278.0, 529.0, 237.0);
        let down = Rect::new(2336.0, 1317.0, 823.0, 433.0);
        // The un-minimize animation, inside the echo: the AX focused window is already ours.
        assert_eq!(step(&mut s, Facts { bounds: growing, echo: true, ..OUT }, ms(100)), Verdict::Wait);
        assert_eq!(step(&mut s, Facts { bounds: grown, echo: true, ..OUT }, ms(350)), Verdict::Wait);
        assert_eq!(step(&mut s, Facts { bounds: down, echo: true, ..OUT }, ms(600)), Verdict::Wait, "the echo: not judged");
        assert_eq!(s.seen_out, 1, "a window listed elsewhere each tick starts its count again");
        // Past the echo: focused by macOS (the caller says: not the user's), set down out of
        // the zone for two ticks running.
        assert_eq!(step(&mut s, Facts { bounds: down, ..OUT }, ms(850)), Verdict::Park, "parked again, never adopted");
        assert_eq!(s.tries, 1);
        assert_eq!(step(&mut s, Facts { seen: Seen::InZone, ..OUT }, ms(1100)), Verdict::Settled, "its card stays");
    }

    /// A window listed somewhere else each tick (an app sliding it, a Space slide's last
    /// frames) is never parked under the move; once it rests, two ticks later it is.
    #[test]
    fn a_window_still_moving_waits_until_it_rests() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut s = Stray::default();
        for i in 0..5u64 {
            let at = Rect::new(100.0 + 40.0 * i as f64, 100.0, 500.0, 400.0);
            assert_eq!(step(&mut s, Facts { bounds: at, ..OUT }, ms(250 * i)), Verdict::Wait, "tick {i}: moving");
            assert_eq!(s.seen_out, 1);
        }
        let rest = Rect::new(300.0, 100.0, 500.0, 400.0);
        assert_eq!(step(&mut s, Facts { bounds: rest, ..OUT }, ms(1250)), Verdict::Wait, "at rest once");
        let rounded = Rect::new(300.5, 100.0, 500.0, 400.0);
        assert_eq!(step(&mut s, Facts { bounds: rounded, ..OUT }, ms(1500)), Verdict::Park, "a point off is the same place");
        // Not listed for a tick (its app hidden): the count and the place start over.
        let mut s = Stray::default();
        assert_eq!(step(&mut s, OUT, ms(0)), Verdict::Wait);
        assert_eq!(step(&mut s, Facts { seen: Seen::NotListed, ..OUT }, ms(250)), Verdict::Wait);
        assert_eq!(s.at, None);
        assert_eq!(step(&mut s, OUT, ms(500)), Verdict::Wait, "seen once since");
    }

    /// The user's choice wins: a window they focused since our park, or one they moved, is
    /// adopted at once; inside our echo even the user's focus waits (it cannot be told from
    /// macOS's yet).
    #[test]
    fn the_focused_or_user_moved_window_is_adopted_at_once() {
        let t0 = Instant::now();
        let mut s = Stray::default();
        assert_eq!(step(&mut s, Facts { focused: true, ..OUT }, t0), Verdict::Adopt);
        let mut s = Stray::default();
        assert_eq!(step(&mut s, Facts { user_moved: true, ..OUT }, t0), Verdict::Adopt);
        let mut s = Stray { user_moved: true, ..Stray::default() };
        assert_eq!(step(&mut s, OUT, t0), Verdict::Adopt, "noted by the caller from a drag");
        let mut s = Stray { gave_up: true, ..Stray::default() };
        assert_eq!(step(&mut s, Facts { focused: true, ..OUT }, t0), Verdict::Adopt, "even one given up");
        let mut s = Stray::default();
        assert_eq!(step(&mut s, Facts { focused: true, echo: true, ..OUT }, t0), Verdict::Wait, "inside our echo: not yet");
        assert_eq!(step(&mut s, Facts { user_moved: true, echo: true, ..OUT }, t0), Verdict::Adopt, "a drag is the user's, echo or not");
    }

    /// K2: a failed park is tried again after a backoff, not at the next tick, and settles
    /// once the app applied the move late.
    #[test]
    fn a_failed_park_is_retried_after_a_backoff() {
        let t0 = Instant::now();
        let mut s = Stray::park_failed(t0);
        assert_eq!(step(&mut s, OUT, t0 + Duration::from_millis(250)), Verdict::Wait, "the batch's try counts");
        assert_eq!(step(&mut s, OUT, t0 + Duration::from_millis(600)), Verdict::Park);
        assert_eq!(s.tries, 2);
        assert_eq!(step(&mut s, Facts { seen: Seen::InZone, ..OUT }, t0 + Duration::from_millis(900)), Verdict::Settled);
        let mut s = Stray::park_failed(t0);
        assert_eq!(step(&mut s, Facts { seen: Seen::NotListed, ..OUT }, t0 + Duration::from_secs(1)), Verdict::Wait, "not on screen: nothing to do");
    }

    /// Our own park landing late, or a busy app, is not fought.
    #[test]
    fn an_echo_or_a_busy_app_waits() {
        let t0 = Instant::now();
        let mut s = Stray { seen_out: SETTLE_TICKS, at: Some(MID), ..Stray::default() };
        assert_eq!(step(&mut s, Facts { echo: true, ..OUT }, t0), Verdict::Wait);
        assert_eq!(step(&mut s, Facts { busy: true, ..OUT }, t0), Verdict::Wait);
        assert_eq!(s.tries, 0, "no try spent");
        assert_eq!(step(&mut s, OUT, t0), Verdict::Park);
    }

    #[test]
    fn the_backoff_grows_and_caps() {
        assert_eq!(backoff(0), Duration::ZERO);
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(2), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(2));
        assert_eq!(backoff(9), Duration::from_secs(2));
    }
}
