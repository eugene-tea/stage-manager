//! When an app that does not answer Accessibility yet is asked again (round login-observe).
//!
//! The owner's restart on 11-06 09:30: a dozen apps macOS reopened did not answer Accessibility for
//! several seconds after Stage Manager started. Each try to observe one cost the WM thread
//! three registrations at the 0.25 s messaging timeout (0.75 s), one app after another, once a
//! second: the WM thread sat 3.3 s behind its notices, and a window found (Terminal's) waited
//! 2.7 s behind the rest of the pass before it went to its stage, long enough to be clicked.
//!
//! Now an app that does not answer is asked on a thread of its own
//! ([`crate::platform::observe_probe`]); the WM thread observes it once it answers. This
//! module is the schedule each of those threads keeps:
//!
//! - **While the login settles** (the restore window open): asked again [`FAST_EVERY`] after
//!   a miss. 150 ms keeps the time from "it answers" to "its windows are in their stages"
//!   under the ~250 ms it takes to see a window and click it; 100 ms would ask half as often
//!   again to gain 50 ms at most, and a miss that waits out the timeout takes 250 ms anyway.
//! - **Otherwise** (the restore window ended, or none): [`SLOW_EVERY`], as before.
//! - **Given up** [`GIVE_UP_AFTER`] after the first try (it was ten tries about a second
//!   apart; at the 11-06 login Fork answered after 29 s). An activation tries again.
//!
//! The next ask counts from the end of the miss, so an app that waits out the timeout is asked
//! about every 0.4 s while the login settles, one that fails at once every 150 ms.

use std::time::{Duration, Instant};

/// After a miss while the login settles (the restore window open).
pub const FAST_EVERY: Duration = Duration::from_millis(150);

/// After a miss otherwise.
pub const SLOW_EVERY: Duration = Duration::from_secs(1);

/// An app that has not answered this long after its first try is given up.
pub const GIVE_UP_AFTER: Duration = Duration::from_secs(60);

/// One app's retries: when it was first tried and how often it missed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    first: Instant,
    misses: u32,
}

/// What follows a miss.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Ask again at this instant.
    At(Instant),
    /// Stop asking: it did not answer for [`GIVE_UP_AFTER`].
    GiveUp,
}

impl Retry {
    /// An app first tried at `now`.
    pub fn new(now: Instant) -> Self {
        Self { first: now, misses: 0 }
    }

    /// When it was first tried.
    pub fn first(&self) -> Instant {
        self.first
    }

    /// How often it missed so far.
    pub fn misses(&self) -> u32 {
        self.misses
    }

    /// It missed at `now` (the ask came back then); `fast` while the login settles.
    pub fn missed(&mut self, now: Instant, fast: bool) -> Next {
        self.misses += 1;
        if now.saturating_duration_since(self.first) >= GIVE_UP_AFTER {
            return Next::GiveUp;
        }
        Next::At(now + if fast { FAST_EVERY } else { SLOW_EVERY })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_miss_while_the_login_settles_is_asked_again_soon() {
        let t0 = Instant::now();
        let mut r = Retry::new(t0);
        assert_eq!(r.missed(t0, true), Next::At(t0 + FAST_EVERY));
        // A miss that waited out the timeout counts from its end.
        let late = t0 + Duration::from_millis(400);
        assert_eq!(r.missed(late, true), Next::At(late + FAST_EVERY));
        assert_eq!(r.misses(), 2);
        assert_eq!(r.first(), t0);
    }

    #[test]
    fn once_the_restore_window_ends_the_cadence_is_a_second() {
        let t0 = Instant::now();
        let mut r = Retry::new(t0);
        assert_eq!(r.missed(t0, true), Next::At(t0 + FAST_EVERY));
        let t1 = t0 + Duration::from_secs(3);
        assert_eq!(r.missed(t1, false), Next::At(t1 + SLOW_EVERY));
    }

    #[test]
    fn an_app_is_given_up_a_minute_after_its_first_try_whatever_the_cadence() {
        let t0 = Instant::now();
        let mut fast = Retry::new(t0);
        let mut slow = Retry::new(t0);
        // Fork answered 29 s into the 11-06 login: still asked then.
        let t29 = t0 + Duration::from_secs(29);
        assert!(matches!(fast.missed(t29, true), Next::At(_)));
        assert!(matches!(slow.missed(t29, false), Next::At(_)));
        let end = t0 + GIVE_UP_AFTER;
        assert_eq!(fast.missed(end, true), Next::GiveUp);
        assert_eq!(slow.missed(end, false), Next::GiveUp);
    }

    #[test]
    fn the_fast_cadence_asks_often_and_the_slow_one_as_before() {
        // About six times a second at the most while the login settles; once a second after.
        assert!(FAST_EVERY >= Duration::from_millis(100) && FAST_EVERY <= Duration::from_millis(200));
        assert_eq!(SLOW_EVERY, Duration::from_secs(1));
    }
}
