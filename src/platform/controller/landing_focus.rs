//! A switch's focus run again once the windows that fly their last stretch have landed
//! (round S7b, the live landing under the Spaces engine).
//!
//! The unveil's focus reaches a window that flies its last stretch while it is still in its
//! landing Space, which is no Desktop: macOS takes the window's focus but never activates its
//! app for it (suite on 9af41d3, 11-14: every switch whose window flew left the incoming app
//! behind the one in front, no activation reported). Under the pictures landing the same
//! focus comes right after the show has put the window on its Desktop, and the activation
//! follows within 20 ms. So the switch remembers the windows of its batch that had a
//! stretch at the unveil; once each has told how it ended (landed, stopped, never taken
//! over), and at least one landed, the focus runs again with the window on its Desktop
//! ([`super::Controller::stretch_ended`]). A switch superseded, or the user's own switch
//! meanwhile, forgets it.

use std::collections::HashSet;

/// The focus a switch owes its windows that are still flying their last stretch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LandingFocus {
    /// The transition the stretches belong to.
    switch: u64,
    /// The window the switch focused.
    key: u32,
    /// The batch's windows whose stretch has not ended yet.
    flying: HashSet<u32>,
    /// One of them landed on its Desktop.
    landed: bool,
}

/// What a stretch's end leaves of a [`LandingFocus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AfterEnd {
    /// Not this switch's stretch, or others still fly: kept.
    Waiting,
    /// The last one ended and none landed (each window was shown by the unveil itself, onto
    /// its Desktop, before the focus): nothing more to do.
    Done,
    /// The last one ended and one landed: focus this window again.
    Refocus(u32),
}

impl LandingFocus {
    /// What switch `switch` owes after its unveil focused `key` with `asked` brought in: the
    /// ones among them that have a stretch now (`stretching`, read before the unveil's shows,
    /// which stop a stretch not taken over yet; that one then ends too). None without a
    /// focus or with no stretch among them.
    pub(super) fn after_unveil(switch: u64, key: Option<u32>, asked: impl IntoIterator<Item = u32>, stretching: &HashSet<u32>) -> Option<Self> {
        let key = key?;
        let flying: HashSet<u32> = asked.into_iter().filter(|id| stretching.contains(id)).collect();
        (!flying.is_empty()).then_some(Self { switch, key, flying, landed: false })
    }

    /// Window `window`'s stretch in transition `switch` ended, on its Desktop when `landed`.
    pub(super) fn ended(&mut self, switch: u64, window: u32, landed: bool) -> AfterEnd {
        if switch != self.switch || !self.flying.remove(&window) {
            return AfterEnd::Waiting;
        }
        self.landed |= landed;
        match (self.flying.is_empty(), self.landed) {
            (false, _) => AfterEnd::Waiting,
            (true, false) => AfterEnd::Done,
            (true, true) => AfterEnd::Refocus(self.key),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The suite's switch on 9af41d3 (11-14 21:02:14): one window, 45223, flies from 14.733
    /// and lands at 15.201, after the unveil's focus (15.121): its focus runs again then.
    #[test]
    fn the_focus_runs_again_once_the_flying_window_lands() {
        let stretching: HashSet<u32> = [45223].into();
        let mut f = LandingFocus::after_unveil(5, Some(45223), [45223], &stretching).expect("it flies");
        assert_eq!(f.ended(4, 45223, true), AfterEnd::Waiting, "another switch's note");
        assert_eq!(f.ended(5, 45223, true), AfterEnd::Refocus(45223));
    }

    /// Several windows fly: the focus waits for the last one, whichever is the key; one
    /// landing is enough.
    #[test]
    fn the_focus_waits_for_every_flying_window() {
        let stretching: HashSet<u32> = [1, 2, 9].into();
        let mut f = LandingFocus::after_unveil(7, Some(3), [1, 2, 3], &stretching).expect("two fly");
        assert_eq!(f.ended(7, 9, true), AfterEnd::Waiting, "not one of this batch");
        assert_eq!(f.ended(7, 1, true), AfterEnd::Waiting, "2 still flies");
        assert_eq!(f.ended(7, 1, true), AfterEnd::Waiting, "a second note of the same window");
        assert_eq!(f.ended(7, 2, false), AfterEnd::Refocus(3));
    }

    /// No stretch landed (a show at the unveil stopped each before it took over): the
    /// unveil's focus found the windows on their Desktop; nothing more.
    #[test]
    fn stretches_that_never_landed_owe_nothing() {
        let stretching: HashSet<u32> = [1].into();
        let mut f = LandingFocus::after_unveil(2, Some(1), [1], &stretching).expect("it was planned");
        assert_eq!(f.ended(2, 1, false), AfterEnd::Done);
        assert_eq!(LandingFocus::after_unveil(2, Some(1), [1], &HashSet::new()), None, "no stretch: the pictures landing");
        assert_eq!(LandingFocus::after_unveil(2, None, [1], &stretching), None, "no focus in the batch");
    }
}
