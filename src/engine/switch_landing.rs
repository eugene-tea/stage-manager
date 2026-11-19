//! What lands a switch's held windows, and what becomes of its pictures still in flight.
//!
//! A switch's incoming windows wait off screen while their pictures grow from the cards; the
//! strip's unveil brings them to their frames under the pictures, which then hand over. Some
//! things land them earlier ([`Landing`]): a click, an activation or a new switch supersedes
//! the switch (a new transition takes the overlay over, or the pictures land on their windows
//! a moment later), and a Space change brings them in before anything is counted for the
//! Desktop shown next.
//!
//! A Space change is the one where the pictures go with the landing ([`pictures_on_landing`]).
//! macOS posts the notice once its slide is over, so the Desktop the switch played on is out
//! of sight already, the windows land there unseen, and the overlay (it belongs to the Space
//! it was shown on) keeps playing there. When macOS slides back to it at once, the Desktop
//! comes in with its windows already at their frames and the pictures still growing over
//! them: a zoom onto windows already there. The owner's log of 11-19 has it twice. At 13:13 a
//! link opened Safari: the switch to its stage began on Desktop 4, Safari focused its window
//! of Desktop 1 and macOS went there; the notice landed window 150 on Desktop 4 at .564, its
//! picture unveiled at .892, and macOS came back to Desktop 4 at .961 (the bounce f80a8b5
//! took away). At 13:25:31, after f80a8b5, ⌘Tab to Safari: macOS slid to Safari's window of
//! Desktop 1, the notice landed window 150 on Desktop 2 (its focus took macOS back), and
//! Desktop 2 slid in again with the window at its frame and its picture unveiling at .763,
//! 26 ms after the arrival.

/// What brought a switch's held windows to their frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// The strip unveiled them: their pictures are within a point of their frames.
    Unveil,
    /// The strip never unveiled them within the backstop.
    Backstop,
    /// A click, an activation, a focus the owner caused, or a new switch: the switch left
    /// behind.
    Superseded,
    /// A Space change was notified: the Desktop the switch played on has slid away.
    SpaceChange,
    /// Anything else that needs the windows where the model has them first (a window shown,
    /// a display change, a hotkey, a config reload, a ctl command).
    Other,
}

/// What becomes of the switch's pictures still in flight when its windows land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pictures {
    /// They fly on to their windows and hand over there (or a new transition takes the
    /// overlay over).
    FlyOn,
    /// They go at once, with the switch's outgoing pictures and its held cards: the switch
    /// is over.
    Go,
}

/// The pictures after `landing` ([`Pictures`]). Only a Space change takes them away: every
/// other landing happens on the Desktop shown, where the pictures still lead the eye to the
/// windows, but a Space change lands the windows on a Desktop out of sight, and if macOS
/// slides back to it the pictures would grow over windows already in place.
pub fn pictures_on_landing(landing: Landing) -> Pictures {
    match landing {
        Landing::SpaceChange => Pictures::Go,
        Landing::Unveil | Landing::Backstop | Landing::Superseded | Landing::Other => Pictures::FlyOn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::focus_elsewhere::{FocusOnSwitch, focus_on_switch};
    use crate::engine::phase::{Event, Phase, Tracker};
    use std::time::{Duration, Instant};

    const BACKSTOP: Duration = Duration::from_millis(1200);

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    /// 11-19 13:13:33, switch #22 (Mail to Safari's stage, Desktop 4), replayed with f80a8b5
    /// in: Safari's own focus of its window on Desktop 1 at +32 ms leaves the switch held; the
    /// Space change notice at +311 ms is what lands it (the controller brings the held windows
    /// in, then the phase changes), well before the unveil (~+650 ms), and the pictures go
    /// with it.
    #[test]
    fn a_space_change_mid_switch_lands_it_and_takes_its_pictures() {
        let t0 = Instant::now();
        let mut phase = Tracker::default();
        phase.feed(Event::SwitchSent { id: 22, max: BACKSTOP }, t0);
        // +32 ms: Safari focuses window 151 on Desktop 1 (not in the switch, held for its
        // Desktop, no click or key since the switch began).
        assert_eq!(focus_on_switch(false, true, false), FocusOnSwitch::LandWithoutFocus);
        assert_eq!(phase.switching().map(|s| s.id), Some(22), "the app's focus elsewhere does not land the switch");
        assert!(phase.switching().is_some_and(|s| !s.overdue(ms(t0, 311))), "nor does the backstop, at the notice");
        // +311 ms: the notice. The held windows land first (SwitchDone), then the change.
        phase.feed(Event::SwitchDone, ms(t0, 311));
        phase.feed(Event::SpaceChanged, ms(t0, 311));
        assert!(phase.changing());
        assert_eq!(pictures_on_landing(Landing::SpaceChange), Pictures::Go, "no picture left growing on the Desktop left");
    }

    /// 11-19 13:25:31, switch #3 (⌘Tab to Safari, after f80a8b5): no focus elsewhere at all;
    /// macOS's own slide to Safari's window of Desktop 1 ends in a notice 217 ms into the
    /// switch, and its landing takes the pictures away before Desktop 2 slides back in.
    #[test]
    fn a_cmd_tab_slide_mid_switch_takes_its_pictures() {
        let t0 = Instant::now();
        let mut phase = Tracker::default();
        phase.feed(Event::SwitchSent { id: 3, max: BACKSTOP }, t0);
        phase.feed(Event::SwitchDone, ms(t0, 217));
        phase.feed(Event::SpaceChanged, ms(t0, 217));
        phase.feed(Event::Arrived, ms(t0, 349));
        assert_eq!(phase.phase(), Phase::Steady);
        assert_eq!(pictures_on_landing(Landing::SpaceChange), Pictures::Go);
    }

    /// Every landing on the Desktop shown keeps the pictures flying to their windows.
    #[test]
    fn only_a_space_change_takes_the_pictures() {
        for l in [Landing::Unveil, Landing::Backstop, Landing::Superseded, Landing::Other] {
            assert_eq!(pictures_on_landing(l), Pictures::FlyOn, "{l:?}");
        }
    }
}
