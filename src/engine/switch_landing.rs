//! What lands a switch's held windows, and what becomes of its pictures still in flight.
//!
//! A switch's incoming windows wait off screen while their pictures grow from the cards; the
//! strip's unveil brings them to their frames under the pictures, which then hand over. Some
//! things land them earlier ([`Landing`]): a click, an activation, a focus, a new switch of
//! ours, anything else that needs the windows where the model has them, and a Space change.
//!
//! The rule ([`pictures_on_landing`]): once a switch's windows land, no picture of that switch
//! grows or shrinks over them. Its pictures fly on only where nothing is shown under them yet:
//! the unveil itself (they are at their windows), the backstop, and a new switch of ours,
//! whose transition takes the overlay over at once, its pictures drawn over the same windows.
//!
//! On the Desktop shown, every other landing finishes the motion at once
//! ([`Pictures::Finish`]): each incoming picture on its window's frame, each outgoing one on its
//! card, handing over as the windows show and fading as at an unveil. Cut away instead, the
//! pictures would leave a hole where a window is not drawn yet (windows show 30-70 ms after
//! they are placed). The owner's log of 11-19 has them: a click at 08:31:36 (switch #9, a
//! Dock click on Safari 615 ms into the motion: the window at its frame 27 ms before its
//! picture's unveil) and at 09:12:35 (#18, a click on the arriving window 582 ms in: 60 ms);
//! an activation at 07:45:06 (#6, ⌘Tab back to Safari 587 ms in: Claude's window at its frame
//! under its picture until the next switch took the overlay 73 ms later), at 07:19:46 (#65,
//! Fork activated 243 ms in, during Mission Control: VS Code's two windows at their frames
//! and their pictures growing over them for 400 ms) and at 12:22:52 (#31, 4 ms before the
//! unveil: nothing seen); any other event at 07:43:19 (#4, a window of Safari shown: 330 ms).
//!
//! A Space change is the one where the pictures go with the landing ([`Pictures::Go`]).
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
//!
//! Mission Control closing during a switch lands nothing early: its windows go back while the
//! switch runs and only delay the landing the unveil asked for (11-19 07:42:02, 07:42:07,
//! 07:57:01 and 09:12:39: unveiled, the windows at their frames 30-70 ms later, the pictures
//! standing still at their frames meanwhile).

/// What brought a switch's held windows to their frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// The strip unveiled them: their pictures are within a point of their frames.
    Unveil,
    /// The strip never unveiled them within the backstop.
    Backstop,
    /// A click of the owner's, anywhere: on a window arriving (it lands as the switch meant,
    /// with its focus) or elsewhere (the switch left behind).
    Click,
    /// An activation, a focus the owner caused, a click on the strip: the switch left behind.
    Superseded,
    /// A new switch of ours: the switch left behind, and the new one's transition takes the
    /// overlay over at once, its pictures drawn over the windows it moves.
    NewSwitch,
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
    /// The motion ends now: every incoming picture on its window's frame, every outgoing one
    /// on its card, handing over as the windows show; the switch lands as after an unveil.
    Finish,
    /// They go at once, with the switch's outgoing pictures and its held cards: the switch
    /// is over.
    Go,
}

/// The pictures after `landing` ([`Pictures`]): none grows or shrinks over windows already at
/// their frames. They fly on at the unveil and the backstop (nothing landed under them before
/// they reach their windows) and at a new switch of ours (its transition takes them over at
/// once); every other landing on the Desktop shown finishes them where they were headed; a
/// Space change, which lands the windows on a Desktop out of sight, takes them away.
pub fn pictures_on_landing(landing: Landing) -> Pictures {
    match landing {
        Landing::Unveil | Landing::Backstop | Landing::NewSwitch => Pictures::FlyOn,
        Landing::Click | Landing::Superseded | Landing::Other => Pictures::Finish,
        Landing::SpaceChange => Pictures::Go,
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

    /// Only a Space change takes the pictures away.
    #[test]
    fn only_a_space_change_takes_the_pictures() {
        for l in [Landing::Unveil, Landing::Backstop, Landing::Click, Landing::Superseded, Landing::NewSwitch, Landing::Other] {
            assert_ne!(pictures_on_landing(l), Pictures::Go, "{l:?}");
        }
    }

    /// 11-19 08:31:35.744, switch #9 (a Dock click on Safari brings its stage): a second Dock
    /// click on Safari at +615 ms, before the unveil (~+643 ms), lands the window. The motion
    /// finishes there: no picture grows the last stretch over the window at its frame.
    #[test]
    fn a_dock_click_mid_switch_finishes_its_pictures() {
        let t0 = Instant::now();
        let mut phase = Tracker::default();
        phase.feed(Event::SwitchSent { id: 9, max: BACKSTOP }, t0);
        phase.feed(Event::SwitchDone, ms(t0, 615));
        assert_eq!(phase.phase(), Phase::Steady);
        assert_eq!(pictures_on_landing(Landing::Click), Pictures::Finish);
    }

    /// 11-19 09:12:34.663, switch #18: a click where the arriving window will be, at +582 ms,
    /// lands it as the switch meant (its focus kept); its picture, 60 ms short of its unveil,
    /// finishes on it.
    #[test]
    fn a_click_on_the_arriving_window_finishes_its_picture() {
        let t0 = Instant::now();
        let mut phase = Tracker::default();
        phase.feed(Event::SwitchSent { id: 18, max: BACKSTOP }, t0);
        phase.feed(Event::SwitchDone, ms(t0, 582));
        assert!(phase.switching().is_none());
        assert_eq!(pictures_on_landing(Landing::Click), Pictures::Finish);
    }

    /// 11-19 07:19:46.570, switch #65 (VS Code's stage, during Mission Control): Fork
    /// activated at +243 ms. VS Code's two windows landed at once while their pictures had
    /// 400 ms still to grow; now they finish on the windows. 07:45:06.159, switch #6 (⌘Tab to
    /// Claude): ⌘Tab back to Safari at +587 ms lands Claude's window, and the switch to
    /// Safari's stage (#7) is sent 70 ms later: the pictures of #6 finish first, and #7's
    /// transition takes the overlay over as it starts.
    #[test]
    fn an_activation_mid_switch_finishes_its_pictures() {
        for (id, at, next) in [(65, 243, None), (6, 587, Some(7))] {
            let t0 = Instant::now();
            let mut phase = Tracker::default();
            phase.feed(Event::SwitchSent { id, max: BACKSTOP }, t0);
            phase.feed(Event::SwitchDone, ms(t0, at));
            assert_eq!(pictures_on_landing(Landing::Superseded), Pictures::Finish, "#{id}");
            if let Some(next) = next {
                phase.feed(Event::SwitchSent { id: next, max: BACKSTOP }, ms(t0, at + 70));
                assert_eq!(phase.switching().map(|s| s.id), Some(next));
            }
        }
    }

    /// A new switch of ours supersedes the running one (a card clicked mid-switch through
    /// its batch): the held windows land and the new transition, sent in the same pass, takes
    /// the overlay over; the pictures fly on until it does.
    #[test]
    fn a_new_switch_takes_the_pictures_over() {
        let t0 = Instant::now();
        let mut phase = Tracker::default();
        phase.feed(Event::SwitchSent { id: 31, max: BACKSTOP }, t0);
        phase.feed(Event::SwitchDone, ms(t0, 300));
        phase.feed(Event::SwitchSent { id: 32, max: BACKSTOP }, ms(t0, 310));
        assert_eq!(pictures_on_landing(Landing::NewSwitch), Pictures::FlyOn);
        assert_eq!(phase.switching().map(|s| s.id), Some(32));
    }

    /// 11-19 07:42:01.321, switch #2: Mission Control closed at +662 ms, just after the
    /// unveil (+648 ms); its windows put back first, the controller came to the unveil's
    /// landing at +714 ms. A late landing, never an early one: the pictures stood at their
    /// frames and fly on.
    #[test]
    fn mission_controls_put_back_only_delays_the_unveils_landing() {
        let t0 = Instant::now();
        let mut phase = Tracker::default();
        phase.feed(Event::SwitchSent { id: 2, max: BACKSTOP }, t0);
        assert!(phase.switching().is_some_and(|s| !s.overdue(ms(t0, 714))));
        phase.feed(Event::SwitchDone, ms(t0, 714));
        assert_eq!(pictures_on_landing(Landing::Unveil), Pictures::FlyOn);
    }
}
