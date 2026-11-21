//! A lend's wait: a prediction acted on ahead of macOS, ended by events, time only as its cap
//! (review 11-21, R2 and rule 3: a wait names what ends it).
//!
//! Under the Spaces engine a window in a stage Space is on no Desktop for macOS, so before an
//! activation macOS has not made yet the window is put on the Desktop shown ("lent", parked
//! out of sight, [`super::lend_park`]): at a Dock press (the click activates its app at the
//! release), while ⌘Tab's switcher selects an app (activated at ⌘'s release). The lend
//! predicts that activation. [`LendWait`] is what ends it:
//!
//! - **Confirmed** by its app's activation ([`LendWait::activation`] for that app) and the
//!   switch showing its stage ([`LendWait::shown`]): the window is the switch's from then on.
//! - **Refuted** by another app's activation stamped after the wait began (the user went
//!   elsewhere, or the switcher was let go on another app), or by a word of the caller's
//!   ([`LendWait::refute`]: a press that was no click, the next press, Escape heard).
//! - **Capped**: with neither past its cap ([`LendWait::step`]), it ends and says so. The
//!   window waits parked off the screen meanwhile, so a long cap costs nothing seen; a short
//!   one took the window back before a busy app's activation, and macOS slid to another
//!   Desktop, the very thing the lend is for.
//!
//! A wait may be held before it begins (⌘Tab's switcher up: the keys decide, no cap runs) and
//! begun later ([`LendWait::begin`]). Each end is told once, and noted with
//! [`margin::wait`] by its cap's name (met, refuted, cap). Nothing here reads a clock.

use std::time::{Duration, Instant};

use super::margin::{self, End};
use super::model::Pid;

/// A wait's cap: its constant's name (for [`margin::wait`]) and its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    /// The constant's name.
    pub name: &'static str,
    /// How long the wait may last.
    pub limit: Duration,
}

/// How a lend's wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Its app was activated and the switch shows its stage: the window is the switch's.
    Met,
    /// An event said its activation will not come (`why`, for the log): back now.
    Refuted(&'static str),
    /// Neither came within the cap: back now.
    Cap,
}

impl Outcome {
    /// As [`margin`] counts it.
    pub fn end(self) -> End {
        match self {
            Outcome::Met => End::Met,
            Outcome::Refuted(_) => End::Refuted,
            Outcome::Cap => End::Cap,
        }
    }
}

/// A lend's wait for its app's activation and switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LendWait {
    /// The app whose activation the lend predicts.
    pid: Pid,
    /// When the wait began, and its cap (`None`: held, not begun).
    from: Option<(Instant, Cap)>,
    /// Its app's activation came.
    activated: bool,
    /// Its end was told.
    ended: bool,
}

impl LendWait {
    /// A wait for app `pid` held, not begun (⌘Tab's switcher up): no cap runs, no other
    /// app's activation refutes it, until [`Self::begin`].
    pub fn held(pid: Pid) -> Self {
        Self { pid, from: None, activated: false, ended: false }
    }

    /// A wait for app `pid` begun at `at` with `cap` (a Dock press's lend).
    pub fn begun(pid: Pid, at: Instant, cap: Cap) -> Self {
        Self { pid, from: Some((at, cap)), activated: false, ended: false }
    }

    /// Begins the wait at `at` with `cap`, or again: the later start and the longer cap count
    /// (⌘'s release and the Dock's list gone, in either order).
    pub fn begin(&mut self, at: Instant, cap: Cap) {
        self.from = Some(match self.from {
            None => (at, cap),
            Some((t, c)) => (t.max(at), if cap.limit > c.limit { cap } else { c }),
        });
    }

    /// The app the lend is for.
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// True once the wait began.
    pub fn is_begun(&self) -> bool {
        self.from.is_some()
    }

    /// True once its app's activation came.
    pub fn is_activated(&self) -> bool {
        self.activated
    }

    /// The cap running, once begun.
    pub fn cap(&self) -> Option<Cap> {
        self.from.map(|(_, c)| c)
    }

    /// App `pid` activated, its notice stamped `at` (when macOS sent it): its own app's
    /// activation is the first half of the confirmation (the switch shows it next,
    /// [`Self::shown`]), and ends nothing. Another app's, once the wait began and stamped at
    /// or after its start, refutes it: an activation from before is no answer to it.
    pub fn activation(&mut self, pid: Pid, at: Instant, now: Instant) -> Option<Outcome> {
        if pid == self.pid {
            self.activated = true;
            return None;
        }
        match self.from {
            Some((from, _)) if at >= from => self.end(Outcome::Refuted("another app was activated"), now),
            _ => None,
        }
    }

    /// The switch shows it (its stage in front, or the model shows the window): met.
    pub fn shown(&mut self, now: Instant) -> Option<Outcome> {
        self.end(Outcome::Met, now)
    }

    /// An event the caller has said refutes it (`why`, for the log).
    pub fn refute(&mut self, why: &'static str, now: Instant) -> Option<Outcome> {
        self.end(Outcome::Refuted(why), now)
    }

    /// The look at `now`: past its cap with no end yet, it ends by the cap.
    pub fn step(&mut self, now: Instant) -> Option<Outcome> {
        match self.from {
            Some((from, cap)) if now.saturating_duration_since(from) >= cap.limit => self.end(Outcome::Cap, now),
            _ => None,
        }
    }

    /// Ends the wait `how`, once: noted by its cap's name when it had begun.
    fn end(&mut self, how: Outcome, now: Instant) -> Option<Outcome> {
        if self.ended {
            return None;
        }
        self.ended = true;
        if let Some((from, cap)) = self.from {
            margin::wait(cap.name, now.saturating_duration_since(from), cap.limit, how.end());
        }
        Some(how)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: Cap = Cap { name: "TEST_LEND_CAP", limit: Duration::from_secs(5) };

    fn clock() -> impl Fn(u64) -> Instant {
        let t0 = Instant::now();
        move |ms| t0 + Duration::from_millis(ms)
    }

    /// A busy Mac: the click's activation 1.5 s after the lend (the old 1 s wait had taken the
    /// window back by then, and macOS slid away). Kept; the switch shows it: met.
    #[test]
    fn a_slow_activation_keeps_the_lend() {
        let ms = clock();
        let mut w = LendWait::begun(40, ms(0), CAP);
        assert_eq!(w.step(ms(1000)), None, "the old wait's end: nothing");
        assert_eq!(w.step(ms(1499)), None);
        assert_eq!(w.activation(40, ms(1500), ms(1510)), None, "its own app: half the answer");
        assert!(w.is_activated());
        assert_eq!(w.step(ms(1515)), None, "the switch not seen yet");
        assert_eq!(w.shown(ms(1520)), Some(Outcome::Met));
        assert_eq!(w.step(ms(9000)), None, "told once");
        assert_eq!(w.shown(ms(9000)), None);
    }

    #[test]
    fn another_apps_activation_ends_it_at_once() {
        let ms = clock();
        let mut w = LendWait::begun(40, ms(100), CAP);
        assert_eq!(w.activation(77, ms(90), ms(130)), None, "stamped before the lend: no answer to it");
        assert_eq!(w.activation(77, ms(140), ms(150)), Some(Outcome::Refuted("another app was activated")));
        assert_eq!(w.activation(40, ms(160), ms(170)), None, "ended already");
        assert_eq!(w.step(ms(9000)), None);
        // Its own app activated first, then another: still ended at once.
        let mut w = LendWait::begun(40, ms(0), CAP);
        w.activation(40, ms(20), ms(20));
        assert!(matches!(w.activation(77, ms(30), ms(30)), Some(Outcome::Refuted(_))));
    }

    #[test]
    fn the_cap_ends_it_and_says_so() {
        let ms = clock();
        let mut w = LendWait::begun(40, ms(0), CAP);
        assert_eq!(w.step(ms(4999)), None);
        assert_eq!(w.step(ms(5000)), Some(Outcome::Cap));
        assert_eq!(Outcome::Cap.end(), End::Cap);
        assert_eq!(w.step(ms(5250)), None, "told once");
        assert_eq!(w.refute("late", ms(5300)), None);
    }

    #[test]
    fn a_held_wait_runs_no_cap_and_hears_no_other_app_until_begun() {
        let ms = clock();
        let mut w = LendWait::held(40);
        assert!(!w.is_begun());
        assert_eq!(w.step(ms(60_000)), None, "⌘Tab's switcher up: the keys decide");
        assert_eq!(w.activation(77, ms(100), ms(100)), None, "an activation while the switcher shows");
        let short = Cap { name: "TEST_SHORT", limit: Duration::from_millis(200) };
        w.begin(ms(1000), short);
        w.begin(ms(900), CAP);
        assert_eq!(w.cap(), Some(CAP), "the longer cap");
        assert_eq!(w.step(ms(5899)), None, "from the later start");
        assert_eq!(w.step(ms(6000)), Some(Outcome::Cap));
    }

    #[test]
    fn a_word_from_the_caller_refutes_it() {
        let ms = clock();
        let mut w = LendWait::held(40);
        assert_eq!(w.refute("the switcher was cancelled", ms(10)), Some(Outcome::Refuted("the switcher was cancelled")));
        assert_eq!(Outcome::Refuted("x").end(), End::Refuted);
        assert_eq!(Outcome::Met.end(), End::Met);
    }
}
