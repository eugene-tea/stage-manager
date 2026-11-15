//! ⌘Tab's app switcher, followed from the keys (round cmd-tab, the Spaces engine).
//!
//! Under the Spaces engine a window in a stage Space is on no Desktop for macOS. With "When
//! switching to an application, switch to a Space with open windows for the application"
//! on, ⌘Tab to an app whose windows here all wait in stage Spaces, while it has a window on
//! another Desktop, takes macOS to that Desktop; the switch then brings the stage here and
//! macOS slides back (S3d gap 5). A Dock click has the same shape and is answered at its
//! press ([`crate::engine::model::Model::dock_home`]: the stage's window put home before the
//! release). ⌘Tab has no press, but the switcher shows while ⌘ is held and the app selected
//! is activated at ⌘'s release: while the switcher shows, the app it selects is known, and
//! its stage's window can be put home the same way before the release.
//!
//! - [`Switcher`] follows the switcher from the keys the input monitor hears: ⌘ pressed and
//!   let go, Tab and ⇧Tab, the arrows, Escape. Its answer ([`Heard`]) is how far the
//!   selection is from the app in front (`1`: the app used before it; negative: counted from
//!   the end, as ⌘⇧Tab starts), or that the switcher was cancelled, let go, or acted on in a
//!   way not followed (Q, H, ↑, ↓, ⌘`: the selection is no longer known from the keys).
//! - [`predict`] turns the steps into an app: the switcher lists the running apps most
//!   recently used first; the platform knows that order for the apps activated since it
//!   started ([`predict`]'s `order`), and nothing of the rest.
//! - [`plan`] says what becomes of the window put home for the switcher: put home for the
//!   app selected ([`Act::Lend`]), taken back when the selection moves on, the switcher is
//!   cancelled or the keys lose it ([`Act::TakeBack`]), kept for the activation at the
//!   release ([`Act::Release`]).
//!
//! The platform prefers the switcher's own selection, read from the Dock's Accessibility at
//! these key events, over [`predict`]; the decision is the same either way.

use super::model::{Pid, WindowId};

/// The Tab key's virtual key code.
pub const KEY_TAB: u16 = 48;
/// The ` key's virtual key code.
pub const KEY_GRAVE: u16 = 50;
/// Escape's virtual key code.
pub const KEY_ESCAPE: u16 = 53;
/// The left arrow's virtual key code.
pub const KEY_LEFT: u16 = 123;
/// The right arrow's virtual key code.
pub const KEY_RIGHT: u16 = 124;

/// A key event as the switcher is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// ⌘ pressed (`true`) or let go (`false`).
    Command(bool),
    /// A key pressed (auto-repeats included), with the modifiers held.
    Down {
        /// Its virtual key code.
        code: u16,
        /// ⌘ held.
        cmd: bool,
        /// ⇧ held.
        shift: bool,
    },
}

/// What a key did to the switcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// Nothing the switcher shows or does.
    Nothing,
    /// The switcher shows (`first`: it just came up) with its selection `steps` from the app
    /// in front; `None` once the keys lost it ([`Heard::Lost`]).
    Selected {
        /// From the app in front, in the switcher's order (negative: from the end).
        steps: Option<i32>,
        /// The switcher just came up.
        first: bool,
    },
    /// Escape: the switcher closed, nothing activated (⌘ may still be held).
    Cancelled,
    /// ⌘ let go with the switcher up: the app selected (`steps`, as in
    /// [`Heard::Selected`]) is activated now.
    Released {
        /// The selection at the release.
        steps: Option<i32>,
    },
    /// A key the switcher acts on in a way the keys do not follow (Q quits the app selected,
    /// H hides it, ↑ and ↓ show its windows, ⌘` moves the selection): the selection is not
    /// known from the keys until the switcher closes.
    Lost,
}

/// The switcher as the keys show it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Switcher {
    /// The switcher is up, with its selection this far from the app in front (`None`: lost).
    open: Option<Option<i32>>,
}

impl Switcher {
    /// True while the switcher is up.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The selection's steps while the switcher is up and the keys follow it.
    pub fn steps(&self) -> Option<i32> {
        self.open.flatten()
    }

    /// Forgets the switcher (⌘ found let go without its key event): true when it was up.
    pub fn reset(&mut self) -> bool {
        self.open.take().is_some()
    }

    /// Follows one key event.
    pub fn hear(&mut self, key: Key) -> Heard {
        match key {
            Key::Command(true) => Heard::Nothing,
            Key::Command(false) => match self.open.take() {
                Some(steps) => Heard::Released { steps },
                None => Heard::Nothing,
            },
            Key::Down { code, cmd, shift } => {
                let Some(steps) = self.open else {
                    if cmd && code == KEY_TAB {
                        let steps = if shift { -1 } else { 1 };
                        self.open = Some(Some(steps));
                        return Heard::Selected { steps: Some(steps), first: true };
                    }
                    return Heard::Nothing;
                };
                if !cmd {
                    // ⌘ is up although its own event was not heard: the release came first.
                    self.open = None;
                    return Heard::Released { steps };
                }
                let step = match code {
                    KEY_TAB if shift => Some(-1),
                    KEY_TAB => Some(1),
                    KEY_RIGHT => Some(1),
                    KEY_LEFT => Some(-1),
                    KEY_ESCAPE => {
                        self.open = None;
                        return Heard::Cancelled;
                    }
                    _ => None,
                };
                match step {
                    Some(d) => {
                        let steps = steps.map(|s| s + d);
                        self.open = Some(steps);
                        Heard::Selected { steps, first: false }
                    }
                    None if steps.is_some() => {
                        self.open = Some(None);
                        Heard::Lost
                    }
                    // Already lost: still lost.
                    None => Heard::Nothing,
                }
            }
        }
    }
}

/// The app the switcher selects `steps` from the app in front, when the platform knows it:
/// `order` is the running apps most recently activated first (the app in front first), as
/// far as the platform saw them activated; `running` how many apps the switcher lists (the
/// running apps with the regular activation policy). `None` for a selection past the apps
/// whose order is known (activated before the platform started, or never), or with fewer
/// than two apps.
pub fn predict(order: &[Pid], running: usize, steps: i32) -> Option<Pid> {
    let n = running.max(order.len());
    if n < 2 {
        return None;
    }
    let at = steps.rem_euclid(n as i32) as usize;
    order.get(at).copied()
}

/// What becomes of the window put home for the switcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// App `pid` is selected and its window `window` is put home now
    /// ([`crate::platform::window_engine::WindowEngine::lend_home`]).
    Lend {
        /// The app selected.
        pid: Pid,
        /// Its stage's window to put home.
        window: WindowId,
    },
    /// The window put home goes back into its stage Space, unshown.
    TakeBack {
        /// The window.
        window: WindowId,
        /// Why, for the log.
        why: &'static str,
    },
    /// ⌘ let go on the app it was put home for: it stays home for the activation, whose
    /// switch takes it (as after a Dock press), else it goes back after the usual wait.
    Release {
        /// The window.
        window: WindowId,
    },
}

/// The acts for `heard`, with `selected` the app the switcher selects now (the Dock's answer,
/// else [`predict`]'s; `None`: not known), `lent` the app and window put home for the
/// switcher so far, and `home` the window to put home for an app, if any
/// ([`crate::engine::model::Model::dock_home`]: under the Corner engine never).
pub fn plan(heard: Heard, selected: Option<Pid>, lent: Option<(Pid, WindowId)>, home: impl FnOnce(Pid) -> Option<WindowId>) -> Vec<Act> {
    let back = |why| lent.map(|(_, window)| Act::TakeBack { window, why });
    match heard {
        Heard::Nothing => Vec::new(),
        Heard::Selected { .. } => {
            if selected.is_some() && lent.map(|(pid, _)| pid) == selected {
                return Vec::new();
            }
            let mut acts: Vec<Act> = back("the switcher's selection moved on").into_iter().collect();
            if let Some(pid) = selected
                && let Some(window) = home(pid)
            {
                acts.push(Act::Lend { pid, window });
            }
            acts
        }
        Heard::Cancelled => back("the switcher was cancelled").into_iter().collect(),
        Heard::Lost => back("the switcher was acted on in a way the keys do not follow").into_iter().collect(),
        Heard::Released { .. } => match lent {
            Some((pid, window)) if selected == Some(pid) => vec![Act::Release { window }],
            _ => back("⌘ was let go on another app").into_iter().collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(shift: bool) -> Key {
        Key::Down { code: KEY_TAB, cmd: true, shift }
    }

    fn down(code: u16) -> Key {
        Key::Down { code, cmd: true, shift: false }
    }

    #[test]
    fn tab_opens_the_switcher_on_the_app_used_before() {
        let mut s = Switcher::default();
        assert_eq!(s.hear(Key::Command(true)), Heard::Nothing, "⌘ alone shows nothing");
        assert_eq!(s.hear(tab(false)), Heard::Selected { steps: Some(1), first: true });
        assert!(s.is_open());
        assert_eq!(s.hear(tab(false)), Heard::Selected { steps: Some(2), first: false });
        assert_eq!(s.hear(tab(true)), Heard::Selected { steps: Some(1), first: false }, "⇧Tab goes back");
        assert_eq!(s.hear(down(KEY_RIGHT)), Heard::Selected { steps: Some(2), first: false });
        assert_eq!(s.hear(down(KEY_LEFT)), Heard::Selected { steps: Some(1), first: false });
        assert_eq!(s.hear(Key::Command(false)), Heard::Released { steps: Some(1) });
        assert!(!s.is_open());
        assert_eq!(s.hear(Key::Command(false)), Heard::Nothing, "a second release is nothing");
    }

    #[test]
    fn shift_tab_opens_on_the_last_app() {
        let mut s = Switcher::default();
        assert_eq!(s.hear(tab(true)), Heard::Selected { steps: Some(-1), first: true });
        assert_eq!(s.hear(Key::Command(false)), Heard::Released { steps: Some(-1) });
    }

    #[test]
    fn escape_cancels_and_a_new_tab_opens_again() {
        let mut s = Switcher::default();
        s.hear(tab(false));
        assert_eq!(s.hear(down(KEY_ESCAPE)), Heard::Cancelled);
        assert!(!s.is_open());
        assert_eq!(s.hear(Key::Command(false)), Heard::Nothing, "⌘ let go after Escape activates nothing");
        s.hear(Key::Command(true));
        s.hear(tab(false));
        assert_eq!(s.hear(down(KEY_ESCAPE)), Heard::Cancelled);
        assert_eq!(s.hear(tab(false)), Heard::Selected { steps: Some(1), first: true }, "⌘ still held: Tab opens it again");
    }

    #[test]
    fn keys_not_followed_lose_the_selection_until_the_switcher_closes() {
        let mut s = Switcher::default();
        s.hear(tab(false));
        assert_eq!(s.hear(down(12)), Heard::Lost, "Q quits the app selected");
        assert_eq!(s.hear(down(4)), Heard::Nothing, "H: already lost");
        assert_eq!(s.hear(tab(false)), Heard::Selected { steps: None, first: false }, "still up, selection unknown");
        assert_eq!(s.hear(Key::Command(false)), Heard::Released { steps: None });
        assert_eq!(s.hear(tab(false)), Heard::Selected { steps: Some(1), first: true }, "a new switcher is followed again");
    }

    #[test]
    fn a_key_without_command_means_command_was_let_go_unheard() {
        let mut s = Switcher::default();
        s.hear(tab(false));
        assert_eq!(s.hear(Key::Down { code: 0, cmd: false, shift: false }), Heard::Released { steps: Some(1) });
        assert!(!s.is_open());
    }

    #[test]
    fn plain_keys_and_command_shortcuts_are_nothing_to_it() {
        let mut s = Switcher::default();
        assert_eq!(s.hear(Key::Down { code: KEY_TAB, cmd: false, shift: false }), Heard::Nothing, "Tab alone");
        assert_eq!(s.hear(down(8)), Heard::Nothing, "⌘C");
        assert_eq!(s.hear(down(KEY_GRAVE)), Heard::Nothing, "⌘` with no switcher up");
        assert_eq!(s.hear(down(KEY_ESCAPE)), Heard::Nothing, "Escape with no switcher up");
        assert!(!s.is_open());
        assert!(!s.reset());
        s.hear(tab(false));
        assert!(s.reset(), "a switcher found closed by the live modifiers");
    }

    #[test]
    fn the_prediction_covers_only_apps_whose_order_is_known() {
        let order = [10, 20, 30];
        assert_eq!(predict(&order, 5, 1), Some(20), "one Tab: the app used before");
        assert_eq!(predict(&order, 5, 2), Some(30));
        assert_eq!(predict(&order, 5, 3), None, "past the apps seen activated");
        assert_eq!(predict(&order, 5, -1), None, "⌘⇧Tab: the last app, never activated since");
        assert_eq!(predict(&order, 3, -1), Some(30), "every app's order known: the last one");
        assert_eq!(predict(&order, 3, 3), Some(10), "around to the app in front");
        assert_eq!(predict(&order, 3, 4), Some(20));
        assert_eq!(predict(&[10], 1, 1), None, "one app: no switcher worth a prediction");
        assert_eq!(predict(&[], 4, 1), None);
    }

    #[test]
    fn the_plan_lends_takes_back_and_releases() {
        let w = WindowId;
        let home = |pid: Pid| (pid == 40).then_some(w(5));
        let sel = Heard::Selected { steps: Some(1), first: true };
        assert_eq!(plan(sel, Some(40), None, home), vec![Act::Lend { pid: 40, window: w(5) }]);
        assert_eq!(plan(sel, Some(40), Some((40, w(5))), home), vec![], "the same app still selected: kept");
        assert_eq!(
            plan(sel, Some(20), Some((40, w(5))), home),
            vec![Act::TakeBack { window: w(5), why: "the switcher's selection moved on" }],
            "moved on to an app with nothing to put home"
        );
        assert_eq!(plan(sel, Some(20), None, home), vec![], "nothing lent, nothing to lend");
        assert_eq!(
            plan(Heard::Selected { steps: None, first: false }, None, Some((40, w(5))), home),
            vec![Act::TakeBack { window: w(5), why: "the switcher's selection moved on" }],
            "selection unknown: back"
        );
        assert_eq!(plan(Heard::Cancelled, Some(40), Some((40, w(5))), home)[0], Act::TakeBack { window: w(5), why: "the switcher was cancelled" });
        assert!(matches!(plan(Heard::Lost, Some(40), Some((40, w(5))), home)[..], [Act::TakeBack { .. }]));
        assert_eq!(plan(Heard::Released { steps: Some(1) }, Some(40), Some((40, w(5))), home), vec![Act::Release { window: w(5) }]);
        assert!(matches!(plan(Heard::Released { steps: Some(2) }, Some(20), Some((40, w(5))), home)[..], [Act::TakeBack { .. }]));
        assert!(matches!(plan(Heard::Released { steps: None }, None, Some((40, w(5))), home)[..], [Act::TakeBack { .. }]));
        assert_eq!(plan(Heard::Released { steps: Some(1) }, Some(40), None, home), vec![], "a quick tap: nothing was lent");
        assert_eq!(plan(Heard::Nothing, Some(40), Some((40, w(5))), home), vec![]);
        // Under the Corner engine `home` answers nothing: no act at all.
        assert_eq!(plan(sel, Some(40), None, |_| None), vec![]);
    }
}
