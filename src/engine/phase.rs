//! The controller's Desktop and transition axis as one explicit state ([`Phase`]), fed with
//! events by the platform ([`Tracker::feed`]).
//!
//! What the main display shows, as far as the controller knows, is one of: a Desktop
//! recognized and steady; a Desktop with a switch's incoming windows still held behind their
//! snapshots; a Space change notified but not recognized yet (and what the recognition waits
//! for); a full-screen app's Space. Every hold that depends on it (no stage changes, no plan
//! for the corner patch, no still of the corner, the strip's order, the reconciler's guard)
//! asks the tracker, and every transition is made here and logged once at debug. Before this
//! the moments were spread over flags and clocks of the controller that each hold read in its
//! own way, and the incident notes were mostly "two of them were true at once".
//!
//! Two measured clocks live beside the phase, because no event says when they end:
//!
//! - **the settle after a Space change** ([`SPACE_SETTLE`]): the window server's list may
//!   still be the old Space's for a moment after the arrival, so nothing is inferred from it;
//! - **the Desktop's slide** as the corner patch's plan reads it from the list (the parked
//!   windows shifted sideways together before the Space change's notice): the plan stays as
//!   it was for at most [`super::corner_patch::SLIDE_HOLD`].
//!
//! Mission Control, App Exposé and Show Desktop stay their own machine
//! ([`super::overview::OverviewWatch`]): one can show in any phase (over a steady Desktop, over
//! a Space change clicked inside it, over a full-screen Space). So does a Desktop arranged
//! ahead of its arrival (the platform's `arrival`): it lives through the Space change it
//! predicts, by design.

use std::fmt;
use std::time::{Duration, Instant};

/// No Space is inferred from the window server's list this soon after a Space change (the
/// list may still be the old Space's).
pub const SPACE_SETTLE: Duration = Duration::from_secs(1);

/// A switch whose incoming windows wait behind their snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switch {
    /// The transition's id.
    pub id: u64,
    /// When it was sent.
    pub since: Instant,
    /// Its windows come back this long after `since` even when the strip never unveils them.
    pub max: Duration,
}

impl Switch {
    /// True once the strip has had its time to unveil the windows.
    pub fn overdue(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) >= self.max
    }
}

/// What a Space change's recognition waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// Mission Control, App Exposé or Show Desktop shows (or just closed): the list shows
    /// its layout, not the Desktop.
    Overview,
    /// A full-screen animation runs ([`super::fullscreen::Settle`]).
    Animation,
    /// Accessibility lists none of the known windows on screen yet.
    Silence,
    /// The list shows two Desktops at once (one still sliding).
    TwoDesktops,
}

impl Hold {
    /// For the log.
    pub fn name(self) -> &'static str {
        match self {
            Hold::Overview => "an overview",
            Hold::Animation => "a full-screen animation",
            Hold::Silence => "Accessibility's silence",
            Hold::TwoDesktops => "two Desktops listed",
        }
    }
}

/// What the main display shows, as the controller knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// A Desktop shows, recognized: stages change, the strip shows.
    Steady,
    /// A Desktop shows; a switch's incoming windows wait behind their snapshots.
    Switching(Switch),
    /// A Space change was notified (or presumed) at `since`; the Space shown is not
    /// recognized yet. `from_fullscreen`: a full-screen app's Space showed before (still no
    /// strip until a Desktop is recognized). `hold`: what the recognition last waited for.
    Changing { since: Instant, from_fullscreen: bool, hold: Option<Hold> },
    /// A full-screen app's Space shows: no strip, no stage changes.
    FullScreen,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Phase::Steady => f.write_str("steady"),
            Phase::Switching(s) => write!(f, "switching #{}", s.id),
            Phase::Changing { from_fullscreen, hold, .. } => {
                f.write_str("changing")?;
                if *from_fullscreen {
                    f.write_str(" from full screen")?;
                }
                if let Some(h) = hold {
                    write!(f, " (waiting for {})", h.name())?;
                }
                Ok(())
            }
            Phase::FullScreen => f.write_str("full screen"),
        }
    }
}

/// What happened, as the platform tells it ([`Tracker::feed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The stages were built on a full-screen app's Space.
    LaunchedFullScreen,
    /// A Space change was notified (`activeSpaceDidChangeNotification`), or is presumed
    /// (observation started again while the screen was locked).
    SpaceChanged,
    /// The recognition of the Space waits ([`Hold`]).
    Held(Hold),
    /// The Space change settled on a full-screen app's Space.
    FullScreenCame,
    /// The Space change settled on a Desktop, recognized now.
    Arrived,
    /// The screen unlocked: the list is looked at afresh after a settle (a Space change
    /// waiting counts from now).
    Unlocked,
    /// A switch was sent to the strip; its incoming windows wait behind their snapshots.
    SwitchSent { id: u64, max: Duration },
    /// The switch's windows are in place (unveiled, or brought in by other input).
    SwitchDone,
}

/// The phase, the clocks beside it, and the transitions between them.
#[derive(Debug, Clone, Copy)]
pub struct Tracker {
    phase: Phase,
    /// When the last Space change was handled: the arrival, or the full-screen Space's
    /// notice. `None` before any.
    changed_at: Option<Instant>,
    /// Since when the corner patch's plan reads the Desktop sliding in the window server's
    /// list; `None` while it does not.
    slide_since: Option<Instant>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self { phase: Phase::Steady, changed_at: None, slide_since: None }
    }
}

impl Tracker {
    /// Takes `event` at `now`. A change of phase (or of what a Space change waits for) is
    /// logged once at debug; an event that makes no sense in the phase is logged at warn and
    /// taken as well as it can be, so the controller never sticks.
    pub fn feed(&mut self, event: Event, now: Instant) {
        let was = self.phase;
        self.phase = match (event, was) {
            (Event::LaunchedFullScreen, _) => Phase::FullScreen,
            (Event::SpaceChanged, p) => {
                if let Phase::Switching(s) = p {
                    tracing::warn!(id = s.id, "phase: a Space change came while a switch's windows were held; they count as in place");
                }
                Phase::Changing { since: now, from_fullscreen: p.shows_fullscreen(), hold: None }
            }
            (Event::Held(h), Phase::Changing { since, from_fullscreen, .. }) => Phase::Changing { since, from_fullscreen, hold: Some(h) },
            (Event::Held(h), p) => {
                tracing::warn!(hold = h.name(), phase = %p, "phase: a wait without a Space change");
                p
            }
            (Event::FullScreenCame, Phase::Changing { since, .. }) => {
                self.changed_at = Some(since);
                Phase::FullScreen
            }
            (Event::FullScreenCame, p) => {
                tracing::warn!(phase = %p, "phase: a full-screen Space came without a Space change");
                Phase::FullScreen
            }
            (Event::Arrived, Phase::Changing { .. }) => {
                self.changed_at = Some(now);
                Phase::Steady
            }
            (Event::Arrived, p) => {
                tracing::warn!(phase = %p, "phase: a Desktop arrived without a Space change");
                self.changed_at = Some(now);
                Phase::Steady
            }
            (Event::Unlocked, Phase::Changing { from_fullscreen, hold, .. }) => Phase::Changing { since: now, from_fullscreen, hold },
            (Event::Unlocked, p) => {
                self.changed_at = Some(now);
                p
            }
            (Event::SwitchSent { id, max }, p) => {
                match p {
                    Phase::Steady => {}
                    Phase::Switching(s) => tracing::warn!(id, held = s.id, "phase: a switch sent over another's held windows"),
                    other => tracing::warn!(id, phase = %other, "phase: a switch sent while no Desktop is recognized"),
                }
                Phase::Switching(Switch { id, since: now, max })
            }
            (Event::SwitchDone, Phase::Switching(_)) => Phase::Steady,
            (Event::SwitchDone, p) => p,
        };
        if self.phase != was {
            tracing::debug!(from = %was, to = %self.phase, "phase");
        }
    }

    /// The phase now.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// True while a full-screen app's Space shows, or a Space change from one has not
    /// settled on a Desktop yet: no strip, no stage changes.
    pub fn fullscreen(&self) -> bool {
        self.phase.shows_fullscreen()
    }

    /// True while a Space change waits to be recognized.
    pub fn changing(&self) -> bool {
        matches!(self.phase, Phase::Changing { .. })
    }

    /// What the Space change's recognition last waited for, while one waits.
    pub fn hold(&self) -> Option<Hold> {
        match self.phase {
            Phase::Changing { hold, .. } => hold,
            _ => None,
        }
    }

    /// The switch whose incoming windows wait behind their snapshots, while one does.
    pub fn switching(&self) -> Option<Switch> {
        match self.phase {
            Phase::Switching(s) => Some(s),
            _ => None,
        }
    }

    /// When the last Space change was notified (while one waits) or handled.
    pub fn changed_at(&self) -> Option<Instant> {
        match self.phase {
            Phase::Changing { since, .. } => Some(since),
            _ => self.changed_at,
        }
    }

    /// Since the last Space change was handled (`None`: none yet, or one waits now).
    pub fn since_change(&self, now: Instant) -> Option<Duration> {
        self.changed_at().map(|t| now.saturating_duration_since(t))
    }

    /// True while the window server's list is not the shown Space's for sure: a Space change
    /// waits, or one settled less than [`SPACE_SETTLE`] ago.
    pub fn space_settling(&self, now: Instant) -> bool {
        self.changing() || self.since_change(now).is_some_and(|d| d < SPACE_SETTLE)
    }

    /// The corner patch's plan read the list at `now` and found the Desktop sliding
    /// (`slides`) or not.
    pub fn slide_seen(&mut self, slides: bool, now: Instant) {
        self.slide_since = if slides { Some(self.slide_since.unwrap_or(now)) } else { None };
    }

    /// For how long the plan has read the Desktop sliding (`None`: it does not).
    pub fn sliding_for(&self, now: Instant) -> Option<Duration> {
        self.slide_since.map(|t| now.saturating_duration_since(t))
    }

    /// True while the plan reads the Desktop sliding.
    pub fn sliding(&self) -> bool {
        self.slide_since.is_some()
    }

    /// The plan is not made (the patch is off): nothing is read sliding.
    pub fn forget_slide(&mut self) {
        self.slide_since = None;
    }
}

impl Phase {
    /// True on a full-screen Space and while a Space change from one has not settled.
    fn shows_fullscreen(self) -> bool {
        matches!(self, Phase::FullScreen | Phase::Changing { from_fullscreen: true, .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn a_space_change_settles_on_a_desktop_and_the_list_settles_after() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        assert_eq!(t.phase(), Phase::Steady);
        assert!(!t.space_settling(t0), "no change yet: nothing settles");
        t.feed(Event::SpaceChanged, t0);
        assert!(t.changing());
        assert!(!t.fullscreen());
        assert_eq!(t.changed_at(), Some(t0), "the notice's time while the change waits");
        t.feed(Event::Held(Hold::Overview), ms(t0, 50));
        assert_eq!(t.hold(), Some(Hold::Overview));
        t.feed(Event::Held(Hold::Animation), ms(t0, 300));
        assert_eq!(t.hold(), Some(Hold::Animation));
        t.feed(Event::Arrived, ms(t0, 900));
        assert_eq!(t.phase(), Phase::Steady);
        assert_eq!(t.hold(), None);
        assert_eq!(t.changed_at(), Some(ms(t0, 900)), "the arrival's time once handled");
        assert!(t.space_settling(ms(t0, 1800)));
        assert!(!t.space_settling(ms(t0, 900) + SPACE_SETTLE));
        assert_eq!(t.since_change(ms(t0, 1000)), Some(Duration::from_millis(100)));
    }

    #[test]
    fn a_full_screen_space_keeps_the_notice_time_and_holds_through_the_change_back() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.feed(Event::SpaceChanged, t0);
        t.feed(Event::FullScreenCame, ms(t0, 400));
        assert_eq!(t.phase(), Phase::FullScreen);
        assert!(t.fullscreen());
        assert!(!t.changing());
        assert_eq!(t.changed_at(), Some(t0), "the notice's time, as the change was handled then");
        // Leaving full screen: no strip until a Desktop is recognized.
        t.feed(Event::SpaceChanged, ms(t0, 5000));
        assert!(t.fullscreen() && t.changing());
        assert_eq!(t.phase(), Phase::Changing { since: ms(t0, 5000), from_fullscreen: true, hold: None });
        t.feed(Event::Arrived, ms(t0, 5600));
        assert!(!t.fullscreen());
        assert_eq!(t.phase(), Phase::Steady);
    }

    #[test]
    fn an_unlock_restarts_the_settle_and_a_waiting_change_counts_from_it() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.feed(Event::Unlocked, t0);
        assert_eq!(t.phase(), Phase::Steady);
        assert_eq!(t.changed_at(), Some(t0));
        assert!(t.space_settling(ms(t0, 500)));
        t.feed(Event::SpaceChanged, ms(t0, 2000));
        t.feed(Event::Held(Hold::Overview), ms(t0, 2100));
        t.feed(Event::Unlocked, ms(t0, 3000));
        assert_eq!(t.phase(), Phase::Changing { since: ms(t0, 3000), from_fullscreen: false, hold: Some(Hold::Overview) });
    }

    #[test]
    fn launched_on_a_full_screen_space() {
        let mut t = Tracker::default();
        t.feed(Event::LaunchedFullScreen, Instant::now());
        assert!(t.fullscreen());
        assert_eq!(t.changed_at(), None);
    }

    #[test]
    fn a_switch_holds_its_windows_until_done_or_overdue() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.feed(Event::SwitchSent { id: 7, max: Duration::from_millis(800) }, t0);
        let s = t.switching().expect("switching");
        assert_eq!((s.id, s.since), (7, t0));
        assert!(!s.overdue(ms(t0, 799)));
        assert!(s.overdue(ms(t0, 800)));
        assert!(!t.changing() && !t.fullscreen());
        t.feed(Event::SwitchDone, ms(t0, 500));
        assert_eq!(t.phase(), Phase::Steady);
        assert_eq!(t.switching(), None);
        t.feed(Event::SwitchDone, ms(t0, 600));
        assert_eq!(t.phase(), Phase::Steady, "a second done changes nothing");
    }

    #[test]
    fn a_space_change_over_a_switch_counts_its_windows_in_place() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.feed(Event::SwitchSent { id: 1, max: Duration::from_secs(1) }, t0);
        t.feed(Event::SpaceChanged, ms(t0, 100));
        assert!(t.changing());
        assert_eq!(t.switching(), None);
        t.feed(Event::SwitchDone, ms(t0, 200));
        assert!(t.changing(), "a late done does not end the change");
    }

    #[test]
    fn a_switch_sent_while_no_desktop_is_recognized_is_taken_and_told() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.feed(Event::LaunchedFullScreen, t0);
        t.feed(Event::SwitchSent { id: 3, max: Duration::from_secs(1) }, ms(t0, 10));
        assert!(t.switching().is_some(), "the batch ran: its windows must be unveiled");
        assert!(!t.fullscreen(), "the next check of the list finds the full-screen Space again");
    }

    #[test]
    fn a_hold_without_a_change_and_an_arrival_without_one_are_taken_as_well_as_they_can_be() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.feed(Event::Held(Hold::Silence), t0);
        assert_eq!(t.phase(), Phase::Steady);
        t.feed(Event::Arrived, ms(t0, 10));
        assert_eq!(t.phase(), Phase::Steady);
        assert_eq!(t.changed_at(), Some(ms(t0, 10)));
        t.feed(Event::FullScreenCame, ms(t0, 20));
        assert_eq!(t.phase(), Phase::FullScreen);
    }

    #[test]
    fn the_slide_is_measured_from_when_the_plan_first_read_it() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        assert!(!t.sliding());
        assert_eq!(t.sliding_for(t0), None);
        t.slide_seen(true, t0);
        t.slide_seen(true, ms(t0, 250));
        assert_eq!(t.sliding_for(ms(t0, 500)), Some(Duration::from_millis(500)));
        t.slide_seen(false, ms(t0, 750));
        assert!(!t.sliding());
        t.slide_seen(true, ms(t0, 1000));
        assert_eq!(t.sliding_for(ms(t0, 1000)), Some(Duration::ZERO), "a new slide counts afresh");
        t.forget_slide();
        assert!(!t.sliding());
    }

    #[test]
    fn phases_name_themselves() {
        let t0 = Instant::now();
        assert_eq!(Phase::Steady.to_string(), "steady");
        assert_eq!(Phase::Switching(Switch { id: 4, since: t0, max: Duration::ZERO }).to_string(), "switching #4");
        assert_eq!(Phase::Changing { since: t0, from_fullscreen: false, hold: None }.to_string(), "changing");
        assert_eq!(
            Phase::Changing { since: t0, from_fullscreen: true, hold: Some(Hold::Overview) }.to_string(),
            "changing from full screen (waiting for an overview)"
        );
        assert_eq!(Phase::FullScreen.to_string(), "full screen");
    }
}
