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
//! - **The user's choice wins.** The focused window, or one the user moved while parked, is
//!   adopted as shown in the active stage where it is ([`Verdict::Adopt`]).
//! - **Otherwise it is parked again**, after it has been seen out of the zone on
//!   [`SETTLE_TICKS`] ticks in a row (one listing can lie: a Space slide's first frame, a
//!   window on its way), with a backoff between tries ([`backoff`]) and at most
//!   [`MAX_TRIES`] tries; then the reconciler gives up on it and says so once
//!   ([`Verdict::GiveUp`]), rather than moving a window its app keeps moving back forever.
//! - A window listed in the zone is settled ([`Verdict::Settled`]): forget it.
//!
//! The caller skips whole ticks when the list cannot be trusted (a Space change settling, a
//! Desktop slide, Mission Control, a transition, a drag), and a window while our own park
//! may still be landing (its echo) or its app is busy.

use std::time::{Duration, Instant};

/// Ticks in a row a window is listed out of the zone before it is parked again.
pub const SETTLE_TICKS: u32 = 2;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// Where the window server lists it.
    pub seen: Seen,
    /// It is the focused window.
    pub focused: bool,
    /// The user moved it while the model had it parked (a drag).
    pub user_moved: bool,
    /// Its app did not answer lately ([`crate::engine::busy_apps`]): nothing is asked of it.
    pub busy: bool,
    /// Our own park went out a moment ago and may still be landing.
    pub echo: bool,
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stray {
    /// Ticks in a row it was listed out of the zone.
    pub seen_out: u32,
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

/// The verdict for stray `s` with `f` at `now`, with `s` brought up to date.
pub fn step(s: &mut Stray, f: Facts, now: Instant) -> Verdict {
    if s.since.is_none() {
        s.since = Some(now);
    }
    match f.seen {
        Seen::InZone => return Verdict::Settled,
        Seen::NotListed => {
            s.seen_out = 0;
            return Verdict::Wait;
        }
        Seen::OutOfZone => s.seen_out = s.seen_out.saturating_add(1),
    }
    if f.focused || f.user_moved || s.user_moved {
        return Verdict::Adopt;
    }
    if f.echo || f.busy || s.gave_up || s.seen_out < SETTLE_TICKS || s.next.is_some_and(|t| now < t) {
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

    const OUT: Facts = Facts { seen: Seen::OutOfZone, focused: false, user_moved: false, busy: false, echo: false };

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

    /// The user's choice wins: the focused window, or one they moved, is adopted at once.
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
        let mut s = Stray { seen_out: SETTLE_TICKS, ..Stray::default() };
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
