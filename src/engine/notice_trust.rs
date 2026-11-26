//! Whether the window server's notices can be trusted to bring the looks forward now (round
//! looks-forward).
//!
//! The notices that bring a look ([`super::server_notice::Kind::brings_look`]: Mission
//! Control's or App Exposé's open and close, a Desktop change's end) are the window server's
//! own, heard under every window engine of ours. They come with a check and a fallback, as
//! the Desktops' reads do ([`super::desk_reads`]). A notice is a trigger for a look, never a
//! fact, so one that does not come changes nothing: the poll's look finds what it would have
//! told. What a trigger can do wrong is cost: a look for nothing. So **working** means all
//! of:
//!
//! 1. the calls are there and the registration took (the platform's part: else the notices
//!    are off for the run);
//! 2. they come ([`Trust::fact`]): Mission Control's open or close found with no notice
//!    within [`NEAR`] before it is a miss, and [`MISSES`] in a row make them silent. Nothing
//!    rests for that. The public triggers (Mission Control's key heard by the key tap, the
//!    Dock's notice) take over from then on, and give way again at the first open or close a
//!    notice told;
//! 3. they tell something ([`Trust::notice`]): [`FRUITLESS`] notices in a row with no fact
//!    between them (an overview's open or close, a Space change) rest them for [`OFF_FOR`]:
//!    none brings a look meanwhile, the polls and the public triggers carry on. After the
//!    rest they are heard again on trial ([`State::Probing`]): the first open or close a
//!    notice told makes them working again, [`PROBE_FRUITLESS`] more for nothing rest them
//!    again, so notices that mean something else on a later macOS never cost more than a few
//!    looks every ten minutes.
//!
//! What the check cannot see: a notice that comes for some of the ways to open Mission
//! Control only (a gesture, a hot corner). Those opens are found by the poll, as before.

use std::time::{Duration, Instant};

/// A notice this long before a fact was found told of it.
pub const NEAR: Duration = Duration::from_secs(2);
/// Opens or closes in a row found with no notice that make the notices silent.
pub const MISSES: u32 = 3;
/// Notices in a row with no fact between them that rest the notices.
pub const FRUITLESS: u32 = 20;
/// The same on trial after a rest.
pub const PROBE_FRUITLESS: u32 = 3;
/// How long the notices rest before they are tried again.
pub const OFF_FOR: Duration = Duration::from_secs(10 * 60);

/// Where the notices stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// They bring the looks; the public triggers rest.
    Working,
    /// None came for the last opens and closes: the public triggers bring the looks (a notice
    /// that does come still brings its own).
    Silent,
    /// They rest until then: none brings a look.
    Resting(Instant),
    /// The rest is over: heard again on trial.
    Probing,
}

/// What a notice or a fact changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The notices count as silent from now.
    Silent,
    /// The notices rest from now: the first rest of a run is worth a warning.
    Rested {
        /// The first rest of this run.
        first: bool,
    },
    /// The notices work again.
    Back,
}

/// The notices' standing in this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trust {
    state: State,
    misses: u32,
    fruitless: u32,
    last_notice: Option<Instant>,
    last_miss: Option<Instant>,
    /// Opens and closes a notice told, those found with none, notices that came with no
    /// fact since the one before, and rests begun.
    pub told: u64,
    /// See [`Self::told`].
    pub missed: u64,
    /// See [`Self::told`].
    pub for_nothing: u64,
    /// See [`Self::told`].
    pub rests: u64,
}

impl Default for Trust {
    fn default() -> Self {
        Self::new()
    }
}

impl Trust {
    /// Working, nothing heard yet.
    pub const fn new() -> Self {
        Self { state: State::Working, misses: 0, fruitless: 0, last_notice: None, last_miss: None, told: 0, missed: 0, for_nothing: 0, rests: 0 }
    }

    /// Where the notices stand at `now` (a rest that is over becomes [`State::Probing`]).
    pub fn state(&mut self, now: Instant) -> State {
        if let State::Resting(until) = self.state
            && now >= until
        {
            self.state = State::Probing;
            self.fruitless = 0;
        }
        self.state
    }

    /// True while a notice brings its look: always but during a rest.
    pub fn brings_looks(&mut self, now: Instant) -> bool {
        !matches!(self.state(now), State::Resting(_))
    }

    /// True while the notices are trusted to bring every look: the public triggers rest.
    pub fn works(&mut self, now: Instant) -> bool {
        self.state(now) == State::Working
    }

    /// A notice that brings a look came at `now` (not asked while resting).
    pub fn notice(&mut self, now: Instant) -> Option<Change> {
        let state = self.state(now);
        if matches!(state, State::Resting(_)) {
            return None;
        }
        self.last_notice = Some(now);
        // The fact it tells of was found a moment before it (the poll's look fell between
        // the two): told after all.
        if self.last_miss.take().is_some_and(|t| now.saturating_duration_since(t) < NEAR) {
            self.missed = self.missed.saturating_sub(1);
            self.told += 1;
            self.misses = 0;
            self.fruitless = 0;
            return (std::mem::replace(&mut self.state, State::Working) != State::Working).then_some(Change::Back);
        }
        self.fruitless += 1;
        if self.fruitless > 1 {
            self.for_nothing += 1;
        }
        let limit = if state == State::Probing { PROBE_FRUITLESS } else { FRUITLESS };
        if self.fruitless < limit {
            return None;
        }
        self.fruitless = 0;
        self.state = State::Resting(now + OFF_FOR);
        self.rests += 1;
        Some(Change::Rested { first: self.rests == 1 })
    }

    /// A fact a notice may tell of was found at `now`: a Space change, or (`expected`) one a
    /// notice is known to come for, Mission Control's or App Exposé's open or close.
    pub fn fact(&mut self, expected: bool, now: Instant) -> Option<Change> {
        let state = self.state(now);
        // Whatever came before it had something to tell.
        self.fruitless = 0;
        if !expected || matches!(state, State::Resting(_)) {
            return None;
        }
        if self.last_notice.is_some_and(|t| now.saturating_duration_since(t) <= NEAR) {
            self.told += 1;
            self.misses = 0;
            self.last_miss = None;
            return (std::mem::replace(&mut self.state, State::Working) != State::Working).then_some(Change::Back);
        }
        self.missed += 1;
        self.misses += 1;
        self.last_miss = Some(now);
        if state == State::Working && self.misses >= MISSES {
            self.state = State::Silent;
            return Some(Change::Silent);
        }
        None
    }

    /// The state's word for `ctl state`, with the seconds a rest still lasts.
    pub fn word(&mut self, now: Instant) -> (&'static str, Option<u64>) {
        match self.state(now) {
            State::Working => ("working", None),
            State::Silent => ("silent", None),
            State::Resting(until) => ("resting", Some(until.saturating_duration_since(now).as_secs())),
            State::Probing => ("probing", None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn opens_and_closes_with_no_notice_make_them_silent_and_one_told_brings_them_back() {
        let t0 = Instant::now();
        let mut t = Trust::new();
        assert!(t.works(t0) && t.brings_looks(t0), "private first: trusted until found wanting");
        // Told: the notice 40 ms before the look that found it open.
        assert_eq!(t.notice(at(t0, 0)), None);
        assert_eq!(t.fact(true, at(t0, 40)), None);
        // Two with none, one told between: the count starts again.
        assert_eq!(t.fact(true, at(t0, 10_000)), None);
        assert_eq!(t.fact(true, at(t0, 13_000)), None);
        assert_eq!(t.notice(at(t0, 20_000)), None);
        assert_eq!(t.fact(true, at(t0, 20_600)), None, "a close, its notice 0.6 s before");
        assert_eq!(t.fact(true, at(t0, 30_000)), None);
        assert_eq!(t.fact(true, at(t0, 33_000)), None);
        assert!(t.works(at(t0, 33_000)), "two in a row: still trusted");
        // A Space change is no open or close: it counts for nothing here.
        assert_eq!(t.fact(false, at(t0, 35_000)), None);
        assert_eq!(t.fact(true, at(t0, 40_000)), Some(Change::Silent));
        assert!(!t.works(at(t0, 40_000)) && t.brings_looks(at(t0, 40_000)), "silent: the public triggers, and a notice that comes still looks");
        assert_eq!(t.word(at(t0, 40_000)), ("silent", None));
        assert_eq!(t.fact(true, at(t0, 50_000)), None, "said once");
        // One comes with its fact: working again.
        assert_eq!(t.notice(at(t0, 60_000)), None);
        assert_eq!(t.fact(true, at(t0, 60_030)), Some(Change::Back));
        assert!(t.works(at(t0, 60_030)));
        assert_eq!((t.told, t.missed), (3, 6));
    }

    #[test]
    fn a_notice_just_after_the_polls_look_found_it_told_of_it_after_all() {
        let t0 = Instant::now();
        let mut t = Trust::new();
        assert_eq!(t.fact(true, at(t0, 0)), None);
        assert_eq!(t.fact(true, at(t0, 5_000)), None);
        // The third found by the poll 10 ms before its notice.
        assert_eq!(t.fact(true, at(t0, 9_000)), Some(Change::Silent));
        assert_eq!(t.notice(at(t0, 9_010)), Some(Change::Back));
        assert!(t.works(at(t0, 9_010)));
        assert_eq!((t.told, t.missed), (1, 2));
        // And an old miss is not taken back by a notice long after.
        assert_eq!(t.fact(true, at(t0, 20_000)), None);
        assert_eq!(t.notice(at(t0, 30_000)), None);
        assert_eq!((t.told, t.missed), (1, 3));
    }

    #[test]
    fn notices_for_nothing_rest_them_and_a_trial_follows() {
        let t0 = Instant::now();
        let mut t = Trust::new();
        // A person going through Desktops: each notice with its Space change. Never a rest.
        for i in 0..100 {
            assert_eq!(t.notice(at(t0, i * 300)), None);
            assert_eq!(t.fact(false, at(t0, i * 300 + 5)), None);
        }
        assert!(t.works(at(t0, 30_000)));
        // Then notices with nothing to find.
        let from = 40_000;
        for i in 0..u64::from(FRUITLESS - 1) {
            assert_eq!(t.notice(at(t0, from + i * 50)), None);
        }
        let rested = from + u64::from(FRUITLESS) * 50;
        assert_eq!(t.notice(at(t0, rested)), Some(Change::Rested { first: true }));
        assert!(!t.brings_looks(at(t0, rested + 1)) && !t.works(at(t0, rested + 1)));
        assert_eq!(t.notice(at(t0, rested + 10)), None, "resting: not counted");
        assert_eq!(t.word(at(t0, rested + 60_000)), ("resting", Some(540)));
        // An open found meanwhile says nothing of them.
        assert_eq!(t.fact(true, at(t0, rested + 70_000)), None);
        // The rest over: on trial, the public triggers still on.
        let back = at(t0, rested) + OFF_FOR;
        assert!(t.brings_looks(back) && !t.works(back));
        assert_eq!(t.word(back), ("probing", None));
        // Still for nothing: a few rest them again, quietly.
        for i in 0..u64::from(PROBE_FRUITLESS - 1) {
            assert_eq!(t.notice(back + Duration::from_millis(i)), None);
        }
        assert_eq!(t.notice(back + Duration::from_millis(10)), Some(Change::Rested { first: false }));
        // The next trial: one tells of an open, and they work again.
        let again = back + Duration::from_millis(10) + OFF_FOR;
        assert_eq!(t.notice(again), None);
        assert_eq!(t.fact(true, again + Duration::from_millis(30)), Some(Change::Back));
        assert!(t.works(again + Duration::from_millis(30)));
        assert_eq!(t.rests, 2);
    }
}
