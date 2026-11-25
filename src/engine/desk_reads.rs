//! Whether the window server's Desktop reads can be trusted now (round corner-desktops).
//!
//! The reads (the listing of every screen's Desktops, a window's Spaces:
//! `platform/desk_reads.rs`) decide which Desktop shows and where each window is, under every
//! window engine. They are the window server's own calls, so each comes with a check and a
//! fallback: the guess from the windows on screen, as before the reads.
//!
//! **Working** means all of:
//!
//! 1. the calls are there (SkyLight loads, the three symbols resolve): else the reads are
//!    off for the run;
//! 2. a read answers: every managed screen has an entry naming a Space shown (the
//!    controller's `match_screens`, at every pass: a pass with none goes by the guess);
//! 3. what is read agrees with a public fact ([`cross_check`]): of the windows the window
//!    list shows on a screen, at least one is on the Space the listing says that screen
//!    shows. A screen with no window says nothing.
//!
//! The third is looked at while nothing moves (no Space change, no overview, no drag), every
//! [`CHECK_EVERY`], at the next look after a Space change's notice the listing answered with
//! no change ([`Health::suspect`]), and at every look once one disagreed. [`STRIKES`] disagreements in a row
//! rest the reads for [`OFF_FOR`] ([`Health`]): everything goes by the guess meanwhile. After
//! the rest they are read for the check alone ([`State::Probing`]) and decide again only once
//! a look agreed, so a listing that stays wrong (after a macOS update) never decides again.
//!
//! What the check cannot see: a listing that names the wrong Desktop while no window is on
//! screen.

use std::time::{Duration, Instant};

/// Disagreements in a row that rest the reads.
pub const STRIKES: u32 = 3;
/// How long the reads rest before they are tried again.
pub const OFF_FOR: Duration = Duration::from_secs(10 * 60);
/// How often the reads are checked while they agree.
pub const CHECK_EVERY: Duration = Duration::from_secs(10);

/// One screen at a look: the Space the listing says it shows, and the Spaces the window
/// server lists each window on screen there on (one entry a window).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Screen {
    /// The listing's "Current Space" for the screen.
    pub current: u64,
    /// Each window the window list shows on the screen: the Spaces it is on.
    pub windows: Vec<Vec<u64>>,
}

/// What a look found ([`cross_check`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every screen with a window has one on the Space the listing shows there.
    Agree,
    /// No window on any screen: nothing to compare.
    Nothing,
    /// A screen's windows are all elsewhere by the reads (why).
    Disagree(&'static str),
}

/// Why a look disagreed: no window on screen has a Space at all.
pub const NO_SPACES: &str = "no window on screen is listed on any Space";
/// Why a look disagreed: the windows on screen are all on other Spaces than the one shown.
pub const ELSEWHERE: &str = "no window on screen is on the Space the listing shows";

/// The reads against the window list (module docs, 3).
pub fn cross_check(screens: &[Screen]) -> Verdict {
    let mut seen = false;
    for s in screens.iter().filter(|s| !s.windows.is_empty()) {
        if s.windows.iter().any(|on| on.contains(&s.current)) {
            seen = true;
        } else if s.windows.iter().all(Vec::is_empty) {
            return Verdict::Disagree(NO_SPACES);
        } else {
            return Verdict::Disagree(ELSEWHERE);
        }
    }
    if seen { Verdict::Agree } else { Verdict::Nothing }
}

/// Where the reads stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// They decide.
    Working,
    /// They rest until then: the guess decides.
    Resting(Instant),
    /// The rest is over: read for the check alone until a look agrees.
    Probing,
}

/// What a look changed ([`Health::looked`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The reads rest from now (why): the first rest of a run is worth a warning.
    Rested { why: &'static str, first: bool },
    /// The reads decide again after a rest.
    Back,
}

/// The reads' health in this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    state: State,
    strikes: u32,
    checked: Option<Instant>,
    /// Looks that agreed, that disagreed, and rests begun.
    pub agreed: u64,
    /// See [`Self::agreed`].
    pub disagreed: u64,
    /// See [`Self::agreed`].
    pub rests: u64,
    /// Why the last look that disagreed did.
    pub last_why: Option<&'static str>,
}

impl Default for Health {
    fn default() -> Self {
        Self::new()
    }
}

impl Health {
    /// Working, nothing checked yet.
    pub const fn new() -> Self {
        Self { state: State::Working, strikes: 0, checked: None, agreed: 0, disagreed: 0, rests: 0, last_why: None }
    }

    /// Where the reads stand at `now` (a rest that is over becomes [`State::Probing`]).
    pub fn state(&mut self, now: Instant) -> State {
        if let State::Resting(until) = self.state
            && now >= until
        {
            self.state = State::Probing;
            self.strikes = 0;
        }
        self.state
    }

    /// True while the reads decide.
    pub fn decides(&mut self, now: Instant) -> bool {
        self.state(now) == State::Working
    }

    /// True when a look is due at `now`: never while resting; at every look while probing or
    /// after a disagreement; else every [`CHECK_EVERY`].
    pub fn due(&mut self, now: Instant) -> bool {
        match self.state(now) {
            State::Resting(_) => false,
            State::Probing => true,
            State::Working => self.strikes > 0 || self.checked.is_none_or(|t| now.saturating_duration_since(t) >= CHECK_EVERY),
        }
    }

    /// Something the listing said looks doubtful (a Space change's notice it answered with
    /// no change): the next look checks, whatever [`CHECK_EVERY`] says.
    pub fn suspect(&mut self) {
        self.checked = None;
    }

    /// A look's verdict at `now`: what it changed, if anything.
    pub fn looked(&mut self, verdict: Verdict, now: Instant) -> Option<Change> {
        let state = self.state(now);
        if matches!(state, State::Resting(_)) {
            return None;
        }
        self.checked = Some(now);
        match verdict {
            Verdict::Nothing => None,
            Verdict::Agree => {
                self.agreed += 1;
                self.strikes = 0;
                (std::mem::replace(&mut self.state, State::Working) == State::Probing).then_some(Change::Back)
            }
            Verdict::Disagree(why) => {
                self.disagreed += 1;
                self.last_why = Some(why);
                self.strikes += 1;
                if state == State::Working && self.strikes < STRIKES {
                    return None;
                }
                // Probing, one is enough: the listing is still not to be trusted.
                self.strikes = 0;
                self.state = State::Resting(now + OFF_FOR);
                self.rests += 1;
                Some(Change::Rested { why, first: self.rests == 1 })
            }
        }
    }

    /// The state's word for `ctl state`, with the seconds a rest still lasts.
    pub fn word(&mut self, now: Instant) -> (&'static str, Option<u64>) {
        match self.state(now) {
            State::Working => ("working", None),
            State::Resting(until) => ("resting", Some(until.saturating_duration_since(now).as_secs())),
            State::Probing => ("probing", None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(current: u64, windows: &[&[u64]]) -> Screen {
        Screen { current, windows: windows.iter().map(|w| w.to_vec()).collect() }
    }

    #[test]
    fn a_screen_agrees_when_one_window_on_it_is_on_the_space_shown() {
        // A window in the park corner of the Desktop shown, one of a Desktop sliding away.
        assert_eq!(cross_check(&[screen(4, &[&[4], &[1]])]), Verdict::Agree);
        // On every Desktop.
        assert_eq!(cross_check(&[screen(4, &[&[1, 4]])]), Verdict::Agree);
        // No window anywhere, or none on the one screen: nothing to say.
        assert_eq!(cross_check(&[]), Verdict::Nothing);
        assert_eq!(cross_check(&[screen(4, &[])]), Verdict::Nothing);
        assert_eq!(cross_check(&[screen(4, &[]), screen(9, &[&[9]])]), Verdict::Agree);
        // The listing kept naming Desktop 1 while Desktop 4's windows show.
        assert_eq!(cross_check(&[screen(1, &[&[4], &[4]])]), Verdict::Disagree(ELSEWHERE));
        // One screen right, the other wrong.
        assert_eq!(cross_check(&[screen(4, &[&[4]]), screen(9, &[&[8]])]), Verdict::Disagree(ELSEWHERE));
        // The per-window read answers nothing.
        assert_eq!(cross_check(&[screen(1, &[&[], &[]])]), Verdict::Disagree(NO_SPACES));
    }

    #[test]
    fn three_disagreements_in_a_row_rest_the_reads_and_one_agreement_brings_them_back() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut h = Health::new();
        assert!(h.decides(t0) && h.due(t0), "the first look is due at once");
        assert_eq!(h.looked(Verdict::Agree, t0), None);
        assert!(!h.due(at(5)) && h.due(at(10)), "then every CHECK_EVERY");
        h.suspect();
        assert!(h.due(at(5)), "or at the next look after something doubtful");
        assert_eq!(h.looked(Verdict::Agree, at(5)), None);
        assert!(!h.due(at(9)));
        // Two strikes, an agreement between: the count starts again.
        assert!(h.due(at(15)));
        assert_eq!(h.looked(Verdict::Disagree(ELSEWHERE), at(15)), None);
        assert!(h.due(at(16)), "looked at again at the next look");
        assert_eq!(h.looked(Verdict::Disagree(ELSEWHERE), at(16)), None);
        assert_eq!(h.looked(Verdict::Agree, at(17)), None);
        assert_eq!(h.looked(Verdict::Disagree(ELSEWHERE), at(22)), None);
        // A look with no window on screen neither counts nor clears.
        assert_eq!(h.looked(Verdict::Nothing, at(23)), None);
        assert_eq!(h.looked(Verdict::Disagree(ELSEWHERE), at(24)), None);
        assert!(h.decides(at(24)), "two in a row: still deciding");
        assert_eq!(h.looked(Verdict::Disagree(NO_SPACES), at(25)), Some(Change::Rested { why: NO_SPACES, first: true }));
        assert!(!h.decides(at(26)) && !h.due(at(26)), "resting: the guess decides, nothing is read");
        assert_eq!(h.word(at(85)), ("resting", Some(540)));
        // The rest over: read for the check alone, not deciding.
        let back = at(25) + OFF_FOR;
        assert!(!h.decides(back) && h.due(back));
        assert_eq!(h.word(back), ("probing", None));
        assert_eq!(h.looked(Verdict::Nothing, back), None);
        assert!(!h.decides(back), "nothing on screen proves nothing");
        // Still wrong: one look rests it again, quietly.
        assert_eq!(h.looked(Verdict::Disagree(ELSEWHERE), back), Some(Change::Rested { why: ELSEWHERE, first: false }));
        let again = back + OFF_FOR;
        assert_eq!(h.looked(Verdict::Agree, again), Some(Change::Back));
        assert!(h.decides(again));
        assert_eq!((h.agreed, h.disagreed, h.rests), (4, 6, 2));
    }
}
