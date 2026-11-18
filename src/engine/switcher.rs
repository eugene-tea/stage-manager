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

use std::time::{Duration, Instant};

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

/// How long after the Dock's switcher list is gone with no key heard for it the switcher is
/// taken as let go ([`Switcher::gone_due`]). Live 11-15: the list goes 1 ms after Escape's
/// key down, the monitor's Escape comes later; ⌘'s release likewise.
pub const GONE_WAIT: Duration = Duration::from_millis(150);

/// The switcher as the keys show it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Switcher {
    /// The switcher is up, with its selection this far from the app in front (`None`: lost).
    open: Option<Option<i32>>,
    /// The Dock's list went at this time with the switcher still up for the keys: Escape or
    /// ⌘'s release is on its way ([`Switcher::list_gone`]).
    gone: Option<Instant>,
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

    /// The switcher shows although its Tab was not heard (the Dock said so): up, its
    /// selection not known from the keys.
    pub fn open_unfollowed(&mut self) {
        if self.open.is_none() {
            self.open = Some(None);
        }
    }

    /// Forgets the switcher (⌘ found let go without its key event): true when it was up.
    pub fn reset(&mut self) -> bool {
        self.gone = None;
        self.open.take().is_some()
    }

    /// The Dock's switcher list went at `at` (`AXUIElementDestroyed`) while the keys still have
    /// it up: nothing is decided now, the key on its way (Escape: cancelled; ⌘ up: released)
    /// decides, else [`Self::gone_due`] after [`GONE_WAIT`].
    pub fn list_gone(&mut self, at: Instant) {
        if self.open.is_some() && self.gone.is_none() {
            self.gone = Some(at);
        }
    }

    /// True while a list gone waits for its key.
    pub fn waits_gone(&self) -> bool {
        self.gone.is_some()
    }

    /// The list gone [`GONE_WAIT`] ago with no key since: the switcher is taken as let go now
    /// ([`Heard::Released`]); else nothing.
    pub fn gone_due(&mut self, now: Instant) -> Heard {
        match self.gone {
            Some(at) if now.saturating_duration_since(at) >= GONE_WAIT => {
                self.gone = None;
                match self.open.take() {
                    Some(steps) => Heard::Released { steps },
                    None => Heard::Nothing,
                }
            }
            _ => Heard::Nothing,
        }
    }

    /// Follows one key event.
    pub fn hear(&mut self, key: Key) -> Heard {
        let heard = self.hear_key(key);
        if self.open.is_none() {
            self.gone = None;
        }
        heard
    }

    fn hear_key(&mut self, key: Key) -> Heard {
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

/// After the switcher closes (⌘ let go, or its list gone), a window put home for it stays only
/// when its app is activated within this long of the later of the two. macOS activates the
/// app 3-12 ms after ⌘'s release (live 11-15). Escape is never heard by the input monitor
/// while the switcher shows (the switcher takes it), so the keys cannot tell a cancel from a
/// release: the activation does.
pub const ACTIVATION_WAIT: Duration = Duration::from_millis(200);

/// A window put home for the switcher, as the switcher's end and its app's activation leave
/// it ([`LetGo::step`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LetGo {
    /// The later of ⌘'s release heard (on the app it was put home for) and the Dock's list
    /// gone.
    pub at: Option<Instant>,
    /// Its app was activated since it was put home.
    pub activated: bool,
}

/// What becomes of a window put home for the switcher, at a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LetGoStep {
    /// The switcher is up, or its app's activation may still come.
    Keep,
    /// No activation of its app came within [`ACTIVATION_WAIT`]: the switcher was cancelled
    /// (Escape, a click outside it), or let go on another app.
    NoActivation,
    /// Its app was activated, but its stage did not come in front within the Dock's wait.
    NoSwitch,
}

impl LetGo {
    /// ⌘ let go on its app, or the Dock's list gone, at `at`: the later counts.
    pub fn let_go(&mut self, at: Instant) {
        self.at = Some(self.at.map_or(at, |t| t.max(at)));
    }

    /// The step at `now`: `up` while the keys still have the switcher up, `lent_at` when it was
    /// put home (the base when the switcher closed unseen), `lend_wait` the Dock press's wait
    /// for the switch once activated. Its stage in front, or the model showing it, is the
    /// caller's to see first.
    pub fn step(&self, up: bool, now: Instant, lent_at: Instant, lend_wait: Duration) -> LetGoStep {
        let base = match self.at {
            Some(t) => t,
            None if up => return LetGoStep::Keep,
            None => lent_at,
        };
        let since = now.saturating_duration_since(base);
        match self.activated {
            true if since >= lend_wait => LetGoStep::NoSwitch,
            false if since >= ACTIVATION_WAIT => LetGoStep::NoActivation,
            _ => LetGoStep::Keep,
        }
    }
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
    /// ⌘ let go on the app it was put home for: it stays home only if that app's activation
    /// comes within [`ACTIVATION_WAIT`] ([`LetGo`]); its switch takes it then.
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
        s.open_unfollowed();
        assert!(s.is_open() && s.steps().is_none(), "the Dock shows it, its Tab unheard");
        assert_eq!(s.hear(Key::Command(false)), Heard::Released { steps: None });
    }

    /// Live 11-15: Escape's key down at 454 ms, the Dock's list gone at 455 ms, the monitor's
    /// Escape after. The list gone decides nothing: the Escape that follows cancels.
    #[test]
    fn the_list_gone_waits_for_the_key_on_its_way() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut s = Switcher::default();
        s.hear(tab(false));
        s.list_gone(ms(455));
        assert!(s.waits_gone() && s.is_open());
        assert_eq!(s.gone_due(ms(470)), Heard::Nothing, "too soon: the key is on its way");
        assert_eq!(s.hear(down(KEY_ESCAPE)), Heard::Cancelled, "Escape decides");
        assert!(!s.waits_gone());
        assert_eq!(s.gone_due(ms(700)), Heard::Nothing, "nothing left to decide");
        // ⌘ up after the list went: released as before.
        let mut s = Switcher::default();
        s.hear(tab(false));
        s.list_gone(ms(0));
        assert_eq!(s.hear(Key::Command(false)), Heard::Released { steps: Some(1) });
        assert!(!s.waits_gone());
        // No key at all: released after the wait.
        let mut s = Switcher::default();
        s.hear(tab(false));
        s.hear(tab(false));
        s.list_gone(ms(0));
        s.list_gone(ms(100));
        assert_eq!(s.gone_due(ms(149)), Heard::Nothing);
        assert_eq!(s.gone_due(ms(150)), Heard::Released { steps: Some(2) }, "the first notice's time counts");
        assert!(!s.is_open() && !s.waits_gone());
        // A list gone with the switcher down (another Dock element) waits for nothing.
        let mut s = Switcher::default();
        s.list_gone(ms(0));
        assert!(!s.waits_gone());
    }

    /// Live 11-15 (rerun): Escape is never heard; the list goes at +300 ms, ⌘ up is heard at
    /// +387 ms as a release on the app put home for. No activation: back at +587 ms.
    #[test]
    fn escape_order_no_activation_takes_the_window_back() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let lend_wait = Duration::from_millis(1000);
        let mut s = Switcher::default();
        s.hear(tab(false));
        let w = WindowId(5);
        assert_eq!(plan(Heard::Selected { steps: Some(1), first: true }, Some(40), None, |_| Some(w)), vec![Act::Lend { pid: 40, window: w }]);
        let mut lent = LetGo::default();
        assert_eq!(lent.step(s.is_open(), ms(250), t0, lend_wait), LetGoStep::Keep, "the switcher up");
        s.list_gone(ms(300));
        lent.let_go(ms(300));
        let heard = s.hear(Key::Command(false));
        assert_eq!(heard, Heard::Released { steps: Some(1) }, "Escape unheard: ⌘ up looks like a release");
        assert_eq!(plan(heard, Some(40), Some((40, w)), |_| None), vec![Act::Release { window: w }]);
        lent.let_go(ms(387));
        assert_eq!(lent.at, Some(ms(387)), "the later of the two");
        assert_eq!(lent.step(false, ms(500), t0, lend_wait), LetGoStep::Keep, "the activation may still come");
        assert_eq!(lent.step(false, ms(586), t0, lend_wait), LetGoStep::Keep);
        assert_eq!(lent.step(false, ms(587), t0, lend_wait), LetGoStep::NoActivation, "no activation came: cancelled");
        // ⌘ held after Escape: the list gone alone starts the wait.
        let mut held = LetGo::default();
        held.let_go(ms(300));
        assert_eq!(held.step(true, ms(499), t0, lend_wait), LetGoStep::Keep);
        assert_eq!(held.step(true, ms(500), t0, lend_wait), LetGoStep::NoActivation);
        // The switcher closed unseen: from when it was put home.
        assert_eq!(LetGo::default().step(false, ms(200), t0, lend_wait), LetGoStep::NoActivation);
    }

    /// The release order: ⌘ up, the activation 10 ms later: kept until its switch (the caller
    /// forgets it once its stage is in front), back only if no switch came in the Dock's wait.
    #[test]
    fn release_order_with_the_activation_keeps_the_window() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let lend_wait = Duration::from_millis(1000);
        let mut lent = LetGo::default();
        lent.let_go(ms(400));
        lent.activated = true; // +410 ms
        lent.let_go(ms(401)); // the list gone after ⌘ up
        assert_eq!(lent.step(false, ms(700), t0, lend_wait), LetGoStep::Keep, "activated: past the activation wait, kept");
        assert_eq!(lent.step(false, ms(1401), t0, lend_wait), LetGoStep::NoSwitch, "activated, but no switch in the Dock's wait");
        // The activation before ⌘ up is heard (both orders reach the WM thread).
        let mut early = LetGo { at: None, activated: true };
        assert_eq!(early.step(true, ms(900), t0, lend_wait), LetGoStep::Keep);
        early.let_go(ms(410));
        assert_eq!(early.step(false, ms(700), t0, lend_wait), LetGoStep::Keep);
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
