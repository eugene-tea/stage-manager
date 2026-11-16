//! The gate: whether a posted step of the Apple mode may go now ([`gate`]). Posted input
//! never fights the person: no posted step while a mouse button is down, ⌘, ⌥ or ⌃ is held
//! (a hotkey's own modifiers would turn the click into another gesture: the step waits for
//! their release), the pointer moved in the last [`POINTER_STILL`], a key came in the last
//! [`KEY_IDLE`], the screen is locked or Mission Control shows; and never mid-motion (a click
//! while Apple's animates is ignored, or late in the motion switches back to the stage
//! leaving: a probe of 11-16), nor while a gesture of ours is still in flight.
//!
//! A Shift the person holds is used as it is (a probe of 11-16): the click then goes with no
//! Shift key event of ours ([`Go::own_shift`]).
//!
//! A held-back step waits, bounded ([`Waiting`]): [`GATE_WAIT`] for the person's input,
//! [`STRIP_WAIT`] for a covered strip to show (the owner's rule, 11-16: wait until it shows,
//! give up after a few seconds and log it). Never retried blindly.

use std::time::{Duration, Instant};

use serde::Serialize;

use super::desk::InputNow;

/// The pointer still this long before a posted step.
pub const POINTER_STILL: Duration = Duration::from_millis(50);

/// No key event for this long before a posted step.
pub const KEY_IDLE: Duration = Duration::from_millis(150);

/// How long a step waits for the person's input to let it go, then it is given up.
pub const GATE_WAIT: Duration = Duration::from_millis(1500);

/// How long a Shift-click waits for a covered strip to show, then it is given up.
pub const STRIP_WAIT: Duration = Duration::from_secs(4);

/// Why a step is held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    /// A mouse button is down.
    ButtonDown,
    /// ⌘, ⌥ or ⌃ is held.
    CommandKeys,
    /// The pointer moved in the last [`POINTER_STILL`].
    PointerMoving,
    /// A key came in the last [`KEY_IDLE`].
    Typing,
    /// The screen is locked.
    Locked,
    /// Mission Control, App Exposé or Show Desktop shows.
    Overview,
    /// Apple's motion runs (or a step of ours began less than a settle ago).
    InMotion,
    /// A gesture of ours is still in flight.
    InFlight,
    /// Apple's strip is covered (a window over it, or its recent apps hidden).
    StripCovered,
    /// The thumbnail to click shows no part of its own.
    NoPoint,
    /// A full-screen app's Space shows on the screen: no step runs there.
    FullScreen,
}

impl Hold {
    /// Its words for the log and `ctl state`.
    pub fn why(self) -> &'static str {
        match self {
            Self::ButtonDown => "a mouse button is down",
            Self::CommandKeys => "a modifier key (command, option or control) is held",
            Self::PointerMoving => "the pointer moves",
            Self::Typing => "a key was just pressed",
            Self::Locked => "the screen is locked",
            Self::Overview => "Mission Control or Show Desktop shows",
            Self::InMotion => "Apple's Stage Manager is moving windows",
            Self::InFlight => "a gesture of ours is in flight",
            Self::StripCovered => "Apple's strip is covered",
            Self::NoPoint => "the thumbnail shows no part of its own",
            Self::FullScreen => "a full-screen app's Space shows",
        }
    }

    /// How long a step waits on this before it is given up: the strip's own wait for the
    /// strip; the settle and a gesture in flight are the mirror's (bounded by the job); the
    /// person's input [`GATE_WAIT`].
    pub fn limit(self) -> Option<Duration> {
        match self {
            Self::StripCovered | Self::NoPoint | Self::FullScreen => Some(STRIP_WAIT),
            Self::InMotion | Self::InFlight => None,
            _ => Some(GATE_WAIT),
        }
    }
}

/// A posted step may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Go {
    /// Post a Shift key-down and key-up of ours around a Shift-click: false when the person
    /// holds Shift (used as it is).
    pub own_shift: bool,
}

/// Whether a posted step may go now: `input` the person's, `settled` the mirror's, `in_flight`
/// a gesture of ours not ended yet.
pub fn gate(input: &InputNow, settled: bool, in_flight: bool) -> Result<Go, Hold> {
    person(input)?;
    if in_flight {
        return Err(Hold::InFlight);
    }
    if !settled {
        return Err(Hold::InMotion);
    }
    Ok(Go { own_shift: !input.shift })
}

/// Whether a focus may go now (it moves no pointer, posts nothing): only while no button is
/// down (a focus then would end the person's drag elsewhere), no gesture of ours is in flight
/// and the screen is unlocked.
pub fn focus_gate(input: &InputNow, in_flight: bool) -> Result<(), Hold> {
    if input.locked {
        return Err(Hold::Locked);
    }
    if input.button_down {
        return Err(Hold::ButtonDown);
    }
    if in_flight {
        return Err(Hold::InFlight);
    }
    Ok(())
}

/// The person's side of the gate.
fn person(input: &InputNow) -> Result<(), Hold> {
    if input.locked {
        return Err(Hold::Locked);
    }
    if input.overview {
        return Err(Hold::Overview);
    }
    if input.button_down {
        return Err(Hold::ButtonDown);
    }
    if input.command_keys {
        return Err(Hold::CommandKeys);
    }
    if (input.pointer_still_ms as u128) < POINTER_STILL.as_millis() {
        return Err(Hold::PointerMoving);
    }
    if (input.key_idle_ms as u128) < KEY_IDLE.as_millis() {
        return Err(Hold::Typing);
    }
    Ok(())
}

/// A held-back step's wait: since when it waits on what. A change of reason keeps the clock
/// (the step has waited all along); the wait ends when the step goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Waiting {
    since: Option<Instant>,
    /// What it waits on now.
    pub on: Option<Hold>,
}

impl Waiting {
    /// Held back by `hold` at `now`: `true` once it has waited longer than `hold`'s limit
    /// (the step is given up).
    pub fn hold(&mut self, hold: Hold, now: Instant) -> bool {
        let since = *self.since.get_or_insert(now);
        self.on = Some(hold);
        hold.limit().is_some_and(|limit| now.saturating_duration_since(since) > limit)
    }

    /// How long it has waited.
    pub fn waited(&self, now: Instant) -> Duration {
        self.since.map_or(Duration::ZERO, |s| now.saturating_duration_since(s))
    }

    /// The step went: the wait ends.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_person_idle_and_the_mirror_settled_lets_a_step_go_with_a_shift_of_ours() {
        assert_eq!(gate(&InputNow::idle(), true, false), Ok(Go { own_shift: true }));
    }

    #[test]
    fn every_refusal_holds_a_step_back() {
        let idle = InputNow::idle();
        let cases = [
            (InputNow { button_down: true, ..idle }, Hold::ButtonDown),
            (InputNow { command_keys: true, ..idle }, Hold::CommandKeys),
            (InputNow { pointer_still_ms: 20, ..idle }, Hold::PointerMoving),
            (InputNow { key_idle_ms: 100, ..idle }, Hold::Typing),
            (InputNow { locked: true, ..idle }, Hold::Locked),
            (InputNow { overview: true, ..idle }, Hold::Overview),
        ];
        for (input, hold) in cases {
            assert_eq!(gate(&input, true, false), Err(hold), "{input:?}");
        }
        assert_eq!(gate(&idle, false, false), Err(Hold::InMotion), "never mid-motion");
        assert_eq!(gate(&idle, true, true), Err(Hold::InFlight), "one gesture at a time");
        assert_eq!(gate(&InputNow { pointer_still_ms: 50, key_idle_ms: 150, ..idle }, true, false), Ok(Go { own_shift: true }), "the bounds themselves pass");
    }

    #[test]
    fn a_shift_the_person_holds_is_used_as_it_is() {
        assert_eq!(gate(&InputNow { shift: true, ..InputNow::idle() }, true, false), Ok(Go { own_shift: false }));
    }

    #[test]
    fn a_focus_waits_only_for_the_button_a_gesture_and_the_lock() {
        let idle = InputNow::idle();
        assert_eq!(focus_gate(&InputNow { pointer_still_ms: 0, key_idle_ms: 0, command_keys: true, ..idle }, false), Ok(()), "a hotkey's own keys do not hold a focus");
        assert_eq!(focus_gate(&InputNow { button_down: true, ..idle }, false), Err(Hold::ButtonDown));
        assert_eq!(focus_gate(&idle, true), Err(Hold::InFlight));
        assert_eq!(focus_gate(&InputNow { locked: true, ..idle }, false), Err(Hold::Locked));
    }

    #[test]
    fn a_wait_is_bounded_by_its_reason_and_keeps_its_clock_across_reasons() {
        let t0 = Instant::now();
        let mut w = Waiting::default();
        assert!(!w.hold(Hold::ButtonDown, t0));
        assert!(!w.hold(Hold::Typing, t0 + Duration::from_millis(1400)));
        assert!(w.hold(Hold::Typing, t0 + Duration::from_millis(1600)), "1.5 s of the person's input: given up");
        let mut w = Waiting::default();
        assert!(!w.hold(Hold::StripCovered, t0));
        assert!(!w.hold(Hold::StripCovered, t0 + Duration::from_secs(3)), "a covered strip is waited a few seconds");
        assert!(w.hold(Hold::StripCovered, t0 + Duration::from_millis(4100)));
        let mut w = Waiting::default();
        assert!(!w.hold(Hold::InMotion, t0 + Duration::from_secs(60)), "the settle is the mirror's, bounded by the job");
        w.clear();
        assert_eq!(w.waited(t0), Duration::ZERO);
    }
}
