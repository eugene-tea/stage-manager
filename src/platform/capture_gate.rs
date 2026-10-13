//! When window captures may run: one probe before many, and a long rest after a refusal.
//!
//! macOS may answer a ScreenCaptureKit capture with an alert ("… is requesting to bypass the
//! system private window picker …"), and every capture issued while it is undecided can raise
//! another. So the first capture of a process (and the first after a refusal) goes alone, a
//! probe; only once one image came back do rounds issue every capture at once. A capture
//! refused as user-declined stops captures for a backoff that doubles up to
//! [`BACKOFF_MAX`]; the user asking for screen access (the menu) retries at once.
//!
//! Pure state, no clocks of its own: every call takes `now`, so the rules are unit tested.
//! The capturer keeps one gate for the whole process ([`super::capture`]).

use std::time::{Duration, Instant};

/// The first rest after a refusal.
pub const BACKOFF_MIN: Duration = Duration::from_secs(10 * 60);
/// The longest rest between two tries after refusals.
pub const BACKOFF_MAX: Duration = Duration::from_secs(2 * 60 * 60);

/// What a capture round may issue now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permit {
    /// Nothing: a probe is out, or captures rest after a refusal.
    Wait,
    /// One capture, alone: the probe.
    Probe,
    /// Every due capture.
    All,
}

/// How a round went, for [`Gate::finished`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// At least one image came back.
    Captured,
    /// macOS refused as user-declined (not permitted).
    Declined,
    /// Nothing came back for another reason (window gone, timeout).
    Inconclusive,
}

/// The gate's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// No capture has run in this process yet.
    Untried,
    /// A lone probe is out.
    Probing,
    /// Captures work.
    Open,
    /// Refused; the next probe may go at `retry_at`.
    Refused { retry_at: Instant },
}

/// What a finished round changed, so the caller logs each change once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Nothing worth a line.
    None,
    /// Captures were refused (first refusal since they last worked); rest for this long.
    Refused(Duration),
    /// Refused again after a retry; rest for this long.
    RefusedAgain(Duration),
    /// Captures work again after a refusal.
    Recovered,
}

/// One process's capture gate.
#[derive(Clone, Debug)]
pub struct Gate {
    state: State,
    /// The rest the next refusal starts.
    backoff: Duration,
    /// A refusal happened since captures last worked.
    refused_before: bool,
}

impl Default for Gate {
    fn default() -> Self {
        Self { state: State::Untried, backoff: BACKOFF_MIN, refused_before: false }
    }
}

impl Gate {
    /// The current state.
    pub fn state(&self) -> State {
        self.state
    }

    /// True while captures rest after a refusal.
    pub fn refused(&self) -> bool {
        matches!(self.state, State::Refused { .. })
    }

    /// What a round may issue now. A probe handed out stays out until [`Gate::finished`].
    pub fn permit(&mut self, now: Instant) -> Permit {
        match self.state {
            State::Open => Permit::All,
            State::Probing => Permit::Wait,
            State::Refused { retry_at } if now < retry_at => Permit::Wait,
            State::Untried | State::Refused { .. } => {
                self.state = State::Probing;
                Permit::Probe
            }
        }
    }

    /// A round (probe or full) ended with `outcome`.
    pub fn finished(&mut self, outcome: Outcome, now: Instant) -> Change {
        match outcome {
            Outcome::Captured => {
                let was = std::mem::replace(&mut self.state, State::Open);
                self.backoff = BACKOFF_MIN;
                let recovered = self.refused_before && was != State::Open;
                self.refused_before = false;
                if recovered { Change::Recovered } else { Change::None }
            }
            Outcome::Declined => {
                let rest = self.backoff;
                self.state = State::Refused { retry_at: now + rest };
                self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
                if std::mem::replace(&mut self.refused_before, true) {
                    Change::RefusedAgain(rest)
                } else {
                    Change::Refused(rest)
                }
            }
            Outcome::Inconclusive => {
                // A probe that told nothing frees the slot for the next round's probe.
                if self.state == State::Probing {
                    self.state = if self.refused_before { State::Refused { retry_at: now } } else { State::Untried };
                }
                Change::None
            }
        }
    }

    /// The user asked for screen access (a menu click): a refused gate probes at once.
    pub fn user_asked(&mut self, now: Instant) {
        if let State::Refused { retry_at } = &mut self.state {
            *retry_at = (*retry_at).min(now);
        }
    }

    /// Short name for logs and the ctl dump.
    pub fn describe(&self) -> &'static str {
        match self.state {
            State::Untried => "untried",
            State::Probing => "probing",
            State::Open => "open",
            State::Refused { .. } => "refused",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn the_first_capture_goes_alone() {
        let now = t0();
        let mut g = Gate::default();
        assert_eq!(g.permit(now), Permit::Probe);
        // While the probe is out, no other round (the other capturer) issues anything.
        assert_eq!(g.permit(now), Permit::Wait);
        assert_eq!(g.permit(now + Duration::from_secs(5)), Permit::Wait);
        assert_eq!(g.finished(Outcome::Captured, now), Change::None);
        assert_eq!(g.permit(now), Permit::All);
        assert_eq!(g.state(), State::Open);
    }

    #[test]
    fn a_refusal_stops_captures_for_the_backoff() {
        let now = t0();
        let mut g = Gate::default();
        assert_eq!(g.permit(now), Permit::Probe);
        assert_eq!(g.finished(Outcome::Declined, now), Change::Refused(BACKOFF_MIN));
        assert!(g.refused());
        assert_eq!(g.permit(now + Duration::from_secs(1)), Permit::Wait);
        assert_eq!(g.permit(now + BACKOFF_MIN - Duration::from_millis(1)), Permit::Wait);
        // After the rest: one probe, never a full round.
        assert_eq!(g.permit(now + BACKOFF_MIN), Permit::Probe);
        assert_eq!(g.permit(now + BACKOFF_MIN), Permit::Wait);
    }

    #[test]
    fn the_backoff_doubles_to_its_cap_and_resets_on_success() {
        let mut now = t0();
        let mut g = Gate::default();
        let mut rests = Vec::new();
        for _ in 0..6 {
            assert_eq!(g.permit(now), Permit::Probe);
            let rest = match g.finished(Outcome::Declined, now) {
                Change::Refused(d) | Change::RefusedAgain(d) => d,
                other => panic!("unexpected {other:?}"),
            };
            rests.push(rest.as_secs() / 60);
            now += rest;
        }
        assert_eq!(rests, vec![10, 20, 40, 80, 120, 120]);
        assert_eq!(g.permit(now), Permit::Probe);
        assert_eq!(g.finished(Outcome::Captured, now), Change::Recovered);
        assert_eq!(g.permit(now), Permit::All);
        assert_eq!(g.finished(Outcome::Declined, now), Change::Refused(BACKOFF_MIN));
    }

    #[test]
    fn a_refusal_is_reported_once_until_captures_work() {
        let now = t0();
        let mut g = Gate::default();
        g.permit(now);
        assert!(matches!(g.finished(Outcome::Declined, now), Change::Refused(_)));
        g.permit(now + BACKOFF_MIN);
        assert!(matches!(g.finished(Outcome::Declined, now + BACKOFF_MIN), Change::RefusedAgain(_)));
    }

    #[test]
    fn a_refusal_while_open_stops_every_round() {
        let now = t0();
        let mut g = Gate::default();
        g.permit(now);
        g.finished(Outcome::Captured, now);
        assert_eq!(g.permit(now), Permit::All);
        assert!(matches!(g.finished(Outcome::Declined, now), Change::Refused(_)));
        assert_eq!(g.permit(now), Permit::Wait);
    }

    #[test]
    fn an_inconclusive_probe_frees_the_slot() {
        let now = t0();
        let mut g = Gate::default();
        assert_eq!(g.permit(now), Permit::Probe);
        assert_eq!(g.finished(Outcome::Inconclusive, now), Change::None);
        assert_eq!(g.state(), State::Untried);
        assert_eq!(g.permit(now), Permit::Probe);
        // After a refusal an inconclusive retry probes again next round, without a new rest.
        g.finished(Outcome::Declined, now);
        let later = now + BACKOFF_MIN;
        assert_eq!(g.permit(later), Permit::Probe);
        g.finished(Outcome::Inconclusive, later);
        assert!(g.refused());
        assert_eq!(g.permit(later), Permit::Probe);
    }

    #[test]
    fn an_inconclusive_full_round_keeps_captures_open() {
        let now = t0();
        let mut g = Gate::default();
        g.permit(now);
        g.finished(Outcome::Captured, now);
        g.finished(Outcome::Inconclusive, now);
        assert_eq!(g.permit(now), Permit::All);
    }

    #[test]
    fn the_user_asking_retries_at_once() {
        let now = t0();
        let mut g = Gate::default();
        g.permit(now);
        g.finished(Outcome::Declined, now);
        let soon = now + Duration::from_secs(3);
        assert_eq!(g.permit(soon), Permit::Wait);
        g.user_asked(soon);
        assert_eq!(g.permit(soon), Permit::Probe);
        // Asking does nothing to a gate that is not refused.
        let mut open = Gate::default();
        open.user_asked(now);
        assert_eq!(open.state(), State::Untried);
    }
}
