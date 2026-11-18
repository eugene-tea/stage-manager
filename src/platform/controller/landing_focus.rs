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
//!
//! The owner's apps do activate for that early focus (11-15, review build, five card clicks
//! of 28: Mail and VS Code activated at the unveil, their key window still flying), and with
//! the key window on no Desktop macOS went to the app's window on another Desktop (06:42:57:
//! VS Code's window 255 on Desktop 1, the Space changed ~330 ms after the activation). So
//! when the key window flies and its app has a window elsewhere, the unveil leaves its
//! focus out ([`LandingFocus::defer`]) and only the focus after the landing runs, owed even
//! when no stretch landed.
//!
//! With the focus owed, the check of the batch's order ([`super::raise_order`]) waits for it
//! too ([`LandingFocus::hold_order_check`]): the key's app comes in front only with that
//! focus, so before it the check finds the windows of the other apps in front of the key's
//! and raises the key's again, holding the WM thread while it watches, and the focus with
//! it (11-16 09:23:16, VS Code's 49841 and Terminal's 214: Terminal in front for ~160 ms
//! after the landing, until the check's raise; the focus at 16.824, 170 ms after it).

use std::collections::HashSet;
use std::time::Instant;

use crate::engine::model::Action;

/// The unveil's raises of the windows still flying (`flying`), and of the children raised
/// right after each (`child`), left out of `actions` when the key's focus is owed: the
/// raises left out, in order. The key's raise reaches a window on no Desktop and does
/// nothing, the others' do: macOS keeps one order of all windows, the hidden ones too, and
/// the window behind came in front of the key's, which then landed under it (11-16
/// 09:47:03: VS Code's 49841 above Terminal's 214 in that order before the switch, Terminal
/// above after the landing until VS Code's app came in front). Left out, they land in the
/// order their last showing left, and the focus after the landing brings the key's app in
/// front.
pub(super) fn without_flying_raises(actions: Vec<Action>, flying: &HashSet<u32>, child: impl Fn(u32) -> bool) -> (Vec<Action>, Vec<u32>) {
    let mut dropped = Vec::new();
    let mut after_dropped = false;
    let kept = actions
        .into_iter()
        .filter(|a| {
            let Action::Raise { id } = a else { return true };
            let drop = flying.contains(&id.0) || (after_dropped && child(id.0));
            after_dropped = drop;
            if drop {
                dropped.push(id.0);
            }
            !drop
        })
        .collect();
    (kept, dropped)
}

/// The check of a batch's order ([`super::Controller::settle_raise_order`]) held for the
/// focus a landing owes: the batch's raises in the order sent, its focus, the switch's start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OrderCheck {
    pub(super) raises: Vec<u32>,
    pub(super) focus: Option<u32>,
    pub(super) since: Instant,
}

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
    /// The unveil left the focus out ([`Self::defer`]): it runs once the last stretch ended,
    /// landed or not.
    owed: bool,
    /// The batch's order check, held for the owed focus ([`Self::hold_order_check`]).
    order_check: Option<OrderCheck>,
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
        (!flying.is_empty()).then_some(Self { switch, key, flying, landed: false, owed: false, order_check: None })
    }

    /// The window the switch focuses.
    pub(super) fn key(&self) -> u32 {
        self.key
    }

    /// The unveil leaves the focus out: it runs only after the stretches, whatever their end.
    pub(super) fn defer(&mut self) {
        self.owed = true;
    }

    /// True when the focus of window `key` waits for the stretches' end ([`Self::defer`]):
    /// nothing else focuses it before.
    pub(super) fn owes(&self, key: u32) -> bool {
        self.owed && self.key == key
    }

    /// The batch's order check `check`: given back to run now, or kept (None) when the focus
    /// is owed, to run right after it ([`Self::take_order_check`]).
    pub(super) fn hold_order_check(&mut self, check: OrderCheck) -> Option<OrderCheck> {
        if !self.owed {
            return Some(check);
        }
        self.order_check = Some(check);
        None
    }

    /// The batch's windows whose stretch has not ended yet.
    pub(super) fn flying(&self) -> &HashSet<u32> {
        &self.flying
    }

    /// The order check held for the owed focus, once ([`Self::hold_order_check`]).
    pub(super) fn take_order_check(&mut self) -> Option<OrderCheck> {
        self.order_check.take()
    }

    /// Window `window`'s stretch in transition `switch` ended, on its Desktop when `landed`.
    pub(super) fn ended(&mut self, switch: u64, window: u32, landed: bool) -> AfterEnd {
        if switch != self.switch || !self.flying.remove(&window) {
            return AfterEnd::Waiting;
        }
        self.landed |= landed;
        match (self.flying.is_empty(), self.landed || self.owed) {
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

    /// The owner's card click of 11-15 06:42:57: VS Code's 240 and Terminal's 214 fly, VS Code
    /// has a window on Desktop 1; the unveil leaves the focus out, and the focus runs once
    /// both ended, even when neither landed (each shown by the unveil instead).
    #[test]
    fn a_focus_left_out_at_the_unveil_runs_after_the_last_stretch_whatever_its_end() {
        let stretching: HashSet<u32> = [214, 240].into();
        let mut f = LandingFocus::after_unveil(37, Some(240), [214, 240], &stretching).expect("both fly");
        assert!(!f.owes(240), "not left out yet");
        f.defer();
        assert!(f.owes(240) && !f.owes(214), "only the key's focus is owed");
        assert_eq!(f.key(), 240);
        assert_eq!(f.ended(37, 214, false), AfterEnd::Waiting);
        assert_eq!(f.ended(37, 240, false), AfterEnd::Refocus(240), "owed though none landed");
    }

    /// The owner's card click of 11-16 09:23:15.831: VS Code's 49841 (the key, in front) and
    /// Terminal's 214 fly, the focus owed. The batch's order check waits for that focus; with
    /// the focus run at the unveil, it runs at once as before.
    #[test]
    fn the_order_check_waits_for_an_owed_focus() {
        let stretching: HashSet<u32> = [214, 49841].into();
        let check = OrderCheck { raises: vec![214, 49841], focus: Some(49841), since: Instant::now() };
        let mut f = LandingFocus::after_unveil(6, Some(49841), [214, 49841], &stretching).expect("both fly");
        assert_eq!(f.hold_order_check(check.clone()), Some(check.clone()), "the focus ran at the unveil: the check runs now");
        assert_eq!(f.take_order_check(), None);
        f.defer();
        assert_eq!(f.hold_order_check(check.clone()), None, "held for the owed focus");
        assert_eq!(f.ended(6, 214, true), AfterEnd::Waiting);
        assert_eq!(f.ended(6, 49841, true), AfterEnd::Refocus(49841));
        assert_eq!(f.take_order_check(), Some(check), "run after that focus");
        assert_eq!(f.take_order_check(), None, "once");
    }

    /// The same switch's unveil: Terminal's 214 and VS Code's 49841 fly, the focus owed; their
    /// raises are left out, a sheet raised after Terminal's with them, a window shown at once
    /// (no stretch) keeps its raise, and so do the frames.
    #[test]
    fn an_owed_focus_leaves_out_the_raises_of_the_windows_still_flying() {
        use crate::engine::model::WindowId as W;
        use crate::engine::geometry::Rect;
        let frame = Rect::new(0.0, 0.0, 100.0, 100.0);
        let batch = vec![
            Action::SetFrame { id: W(214), frame },
            Action::Raise { id: W(7) },
            Action::Raise { id: W(214) },
            Action::Raise { id: W(900) },
            Action::Raise { id: W(49841) },
            Action::Raise { id: W(901) },
        ];
        let flying: HashSet<u32> = [214, 49841].into();
        let child = |id: u32| id >= 900;
        let (kept, dropped) = without_flying_raises(batch, &flying, child);
        assert_eq!(dropped, [214, 900, 49841, 901]);
        assert_eq!(kept, [Action::SetFrame { id: W(214), frame }, Action::Raise { id: W(7) }]);
        // A child after a window that does not fly keeps its raise.
        let (kept, dropped) = without_flying_raises(vec![Action::Raise { id: W(7) }, Action::Raise { id: W(900) }], &flying, child);
        assert!(dropped.is_empty());
        assert_eq!(kept.len(), 2);
    }
}
