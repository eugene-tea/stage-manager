//! ⌘Tab's app switcher, followed from the keys (round cmd-tab for the Spaces engine's lend;
//! every engine since round arranged-ahead, for the Desktop arranged ahead: [`plan_ahead`]).
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
//!   release ([`Act::Release`]), which then waits for it ([`activation_cap`],
//!   [`crate::engine::lend_wait`]).
//!
//! The input monitor never hears Escape while the switcher shows, nor always ⌘'s release;
//! the platform's HID key tap hears both, and a mouse-down, while the switcher is up, and
//! feeds them here as the same [`Key`]s.
//!
//! The platform prefers the switcher's own selection, read from the Dock's Accessibility at
//! these key events, over [`predict`]; the decision is the same either way.
//!
//! **The Desktop arranged ahead** (round arranged-ahead; audit engine-uniformity 11-25, H5).
//! An app with no window on the Desktop shown and one on another makes macOS slide there at
//! ⌘'s release, and Apple's Stage Manager slides that Desktop in with the app's stage in
//! front. While the switcher shows the app is known, so that Desktop is arranged then, as at
//! a Dock press ([`crate::engine::model::Model::dock_arrival`] says whether macOS will
//! slide), under every engine: [`plan_ahead`] arranges for the app selected
//! ([`AheadAct::Arrange`]), undoes it when the selection moves on, at Escape or when the
//! keys lose the switcher ([`AheadAct::Undo`]), and lets it stand at ⌘'s release on that app
//! ([`AheadAct::Stand`]: the Space change confirms it, as a Dock click's).
//!
//! *When.* At the first Tab's guess, corrected by the Dock's list, as the lend: a quick ⌘Tab
//! (⌘ let go before the Dock lists the switcher, ~180 ms after the Tab) is arranged from
//! [`predict`] alone, a browsed one from the Dock's reading from then on. Under the Spaces
//! engine it has to be before the release: macOS picks the Desktop 3-12 ms after it, by the
//! windows it counts there then. Under the Corner engine the hidden window is on its Desktop
//! already, and the earlier start is its moves' (through Accessibility, each as slow as its
//! app). Arranging only once the Dock confirmed, or at the release, would leave a quick
//! ⌘Tab as it was: in the logs of 11-18 to 11-24, 220 of 668 first Tabs had no reading of
//! the Dock's list after them (let go before it), and where both the guess and the list
//! named an app (442) they agreed in 435.
//! A wrong guess costs its moves twice, on a Desktop nobody sees: corrected by the Dock's
//! list while the switcher shows; let go before that, undone by what macOS does next (the
//! other app's activation, or the Space change that lists another Desktop).
//!
//! *Its end must be heard.* A cancelled switcher looks like a released one to the input
//! monitor (it never hears Escape). Only with the key tap hearing is a release known for one:
//! without it nothing is arranged ahead, and a switcher that closes with no key heard for its
//! end has its arrangement undone, so the Desktop is arranged on arrival, as before.

use std::time::{Duration, Instant};

use super::lend_wait::Cap;
use super::margin::{self, End};
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
/// key down, the monitor's Escape comes later; ⌘'s release likewise. The key tap hears both
/// as they happen (round r2-lends), so this is the fallback for a tap macOS refused or keeps
/// off, and the log says so when it ends a switcher.
pub const GONE_WAIT: Duration = Duration::from_millis(150);

/// How long the tick finds ⌘ let go, with no key told for it, before a key tap that hears is
/// taken to have missed the release ([`unheard_release`]). Live 11-25: a tick 3 ms after ⌘'s
/// release found it let go before the tap's own word of it was handled, and the Desktop
/// arranged ahead was undone for a release that could not be told from a cancel.
pub const UNHEARD_WAIT: Duration = Duration::from_millis(100);

/// The tick found ⌘ let go with the switcher up and no key told for it: whether that counts
/// as the release now. `seen` is when a tick first found it so for this switcher (`None`:
/// this is the first), `tap_hears` whether the key tap hears. A tap that hears tells the
/// release itself a few ms after it, so the tick waits [`UNHEARD_WAIT`] for its word; with
/// no tap hearing there is nothing to wait for.
pub fn unheard_release(seen: Option<Instant>, tap_hears: bool, now: Instant) -> bool {
    !tap_hears || seen.is_some_and(|at| now.saturating_duration_since(at) >= UNHEARD_WAIT)
}

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
                margin::wait("GONE_WAIT", now.saturating_duration_since(at), GONE_WAIT, End::Cap);
                self.gone = None;
                match self.open.take() {
                    Some(steps) => Heard::Released { steps },
                    None => Heard::Nothing,
                }
            }
            _ => Heard::Nothing,
        }
    }

    /// [`Self::hear`] at `now`: a key that ends a list gone's wait notes it met
    /// ([`GONE_WAIT`]).
    pub fn hear_at(&mut self, key: Key, now: Instant) -> Heard {
        let waited = self.gone;
        let heard = self.hear(key);
        if let Some(at) = waited
            && self.gone.is_none()
        {
            margin::wait("GONE_WAIT", now.saturating_duration_since(at), GONE_WAIT, End::Met);
        }
        heard
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

/// After ⌘'s release heard on the app a window was put home for (or the Dock's list gone),
/// that app's activation is waited for at most this long ([`activation_cap`]); its activation
/// and switch confirm the lend, another app's activation refutes it, Escape heard took it back
/// already ([`crate::engine::lend_wait`]). macOS activates the app 3-12 ms after ⌘'s release
/// (live 11-15), later on a busy Mac. The cap is long because waiting costs nothing seen: the
/// window waits parked off the screen (a sliver at its display's corner at most) and the key
/// tap hears Escape and ⌘'s release while the switcher shows, so silence no longer stands for
/// a cancel. It ends only a release whose activation never comes (the app hung or quit). The
/// 200 ms wait it replaces took the window back before a busy app's activation, and macOS slid
/// to the app's other Desktop: the slide the lend is for.
pub const ACTIVATION_WAIT: Duration = Duration::from_millis(3000);

/// [`ACTIVATION_WAIT`] once the switcher went with no reading of its end ([`Choice::Gone`]:
/// a mouse pick dismissed it, with ⌘ still held): the click's activation comes at the mouse's
/// release. Counted from the click heard by the key tap, else from the list gone (the later
/// of it and ⌘'s release). As long, for the same reason.
pub const UNREAD_WAIT: Duration = Duration::from_millis(3000);

/// The wait of [`ACTIVATION_WAIT`] when the key tap does not hear (macOS refused it, or keeps
/// it off for secure input) or the release was not heard at all (taken after [`GONE_WAIT`],
/// or from the live modifiers): Escape is then never heard (the switcher takes it from the
/// input monitor), and a missing activation is all that tells a cancel, so the wait stays
/// short, as before the tap.
pub const SILENT_ACTIVATION_WAIT: Duration = Duration::from_millis(200);

/// [`UNREAD_WAIT`] in the same case as [`SILENT_ACTIVATION_WAIT`].
pub const SILENT_UNREAD_WAIT: Duration = Duration::from_millis(400);

/// The cap for a window put home for the switcher once it closed: `unread` after a mouse pick
/// ([`UNREAD_WAIT`]), `heard` when the key tap hears and the release (or the click) was
/// heard, else the short ones of the time before the tap.
pub fn activation_cap(unread: bool, heard: bool) -> Cap {
    match (unread, heard) {
        (false, true) => Cap { name: "ACTIVATION_WAIT", limit: ACTIVATION_WAIT },
        (true, true) => Cap { name: "UNREAD_WAIT", limit: UNREAD_WAIT },
        (false, false) => Cap { name: "SILENT_ACTIVATION_WAIT", limit: SILENT_ACTIVATION_WAIT },
        (true, false) => Cap { name: "SILENT_UNREAD_WAIT", limit: SILENT_UNREAD_WAIT },
    }
}

/// What the Dock's Accessibility showed at a selection step, for [`choose`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    /// The switcher's list shows (its selection named or not).
    Shown,
    /// No switcher list in the Dock.
    Gone,
    /// The Dock left the call unanswered.
    Unanswered,
}

/// Whose word a selection step takes ([`choose`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The switcher's own selection, read from the Dock.
    Dock(Pid),
    /// The keys and the order of activations ([`predict`]; `None`: not known).
    Guess(Option<Pid>),
    /// The Dock gave no reading at its own notice: the last reading stands, nothing moves.
    Keep,
    /// The Dock's list, seen this switcher, is gone: the last reading stands, and the window
    /// put home waits for its app's activation ([`UNREAD_WAIT`]). A click
    /// in the switcher dismisses it with ⌘ still held; the keys' steps know nothing of the
    /// mouse (hovering an icon moves the selection), so a guess from them would take the
    /// window back as the click activates its app (live 11-18).
    Gone,
}

impl Choice {
    /// For the log.
    pub fn name(self) -> &'static str {
        match self {
            Choice::Dock(_) => "the Dock's reading",
            Choice::Guess(_) => "the guess from the keys",
            Choice::Keep => "the last reading stands: the Dock gave none",
            Choice::Gone => "the last reading stands: the Dock's list is gone",
        }
    }
}

/// Whose word a selection step takes: `docked` the app the Dock's read names, `list` what it
/// showed, `listed` the Dock's list was seen shown since the first Tab, `by_key` the step
/// came from a key (else from a Dock notice), `predicted` the keys' guess. Before the Dock's
/// list first shows (~180 ms after the first Tab) the keys are all there is; once it showed,
/// a guess never stands in for a reading, except after a key with the Dock silent.
pub fn choose(docked: Option<Pid>, list: List, listed: bool, by_key: bool, predicted: Option<Pid>) -> Choice {
    match docked {
        Some(pid) => Choice::Dock(pid),
        None if !listed => Choice::Guess(predicted),
        None if list == List::Gone => Choice::Gone,
        None if by_key => Choice::Guess(predicted),
        None => Choice::Keep,
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
    /// ⌘ let go on the app it was put home for: it stays home for that app's activation and
    /// switch, at most [`ACTIVATION_WAIT`] ([`crate::engine::lend_wait::LendWait`]).
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

/// What becomes of the Desktop arranged ahead for the switcher's selection ([`plan_ahead`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AheadAct {
    /// App `pid` is selected and macOS will slide to another Desktop for its `window` at ⌘'s
    /// release: that Desktop is arranged now.
    Arrange {
        /// The app selected.
        pid: Pid,
        /// The window macOS will show.
        window: WindowId,
    },
    /// The arrangement made for the switcher is undone, on its Desktop not shown.
    Undo {
        /// Why, for the log.
        why: &'static str,
    },
    /// ⌘ let go on the app it was arranged for: it stands for the Space change, the window
    /// held as its focus would be.
    Stand {
        /// The window.
        window: WindowId,
    },
}

/// The acts on the Desktop arranged ahead for `heard`, with `selected` the app the switcher
/// selects now (as for [`plan`]), `arranged` the app and window a Desktop stands arranged for
/// while this switcher is up, `end_heard` whether its end was told by a key a hearing tap or
/// the monitor could not mistake (a release taken from the Dock's list gone, or from the
/// live modifiers, may be a cancel), and `slides_to` the window macOS will slide to another
/// Desktop for when ⌘ is let go on an app (`None`: it stays here, or nothing says).
pub fn plan_ahead(
    heard: Heard,
    selected: Option<Pid>,
    arranged: Option<(Pid, WindowId)>,
    end_heard: bool,
    slides_to: impl FnOnce(Pid) -> Option<WindowId>,
) -> Vec<AheadAct> {
    let undo = |why| arranged.map(|_| AheadAct::Undo { why });
    match heard {
        Heard::Nothing => Vec::new(),
        Heard::Selected { .. } => {
            if selected.is_some() && arranged.map(|(pid, _)| pid) == selected {
                return Vec::new();
            }
            let mut acts: Vec<AheadAct> = undo("the switcher's selection moved on").into_iter().collect();
            if let Some(pid) = selected
                && let Some(window) = slides_to(pid)
            {
                acts.push(AheadAct::Arrange { pid, window });
            }
            acts
        }
        Heard::Cancelled => undo("the switcher was cancelled").into_iter().collect(),
        Heard::Lost => undo("the switcher was acted on in a way the keys do not follow").into_iter().collect(),
        Heard::Released { .. } => match arranged {
            Some((pid, window)) if selected == Some(pid) && end_heard => vec![AheadAct::Stand { window }],
            Some((pid, _)) if selected == Some(pid) => undo("the switcher closed with no key heard for its end: it may have been cancelled").into_iter().collect(),
            _ => undo("⌘ was let go on another app").into_iter().collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::lend_wait::{LendWait, Outcome};

    fn tab(shift: bool) -> Key {
        Key::Down { code: KEY_TAB, cmd: true, shift }
    }

    fn down(code: u16) -> Key {
        Key::Down { code, cmd: true, shift: false }
    }

    #[test]
    fn a_release_the_tick_finds_waits_for_a_hearing_taps_word() {
        let t = Instant::now();
        assert!(unheard_release(None, false, t), "no tap hears: the tick's word is all there is");
        assert!(!unheard_release(None, true, t), "the first tick that finds it: the tap's word may be on its way");
        assert!(!unheard_release(Some(t), true, t + Duration::from_millis(99)));
        assert!(unheard_release(Some(t), true, t + UNHEARD_WAIT), "the tap missed it");
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

    /// Live 11-15 (rerun), before the key tap: Escape is never heard; the list goes at +300 ms,
    /// ⌘ up is heard at +387 ms as a release on the app put home for. No activation: back
    /// after the short wait of a tap that does not hear (the silent fallback).
    #[test]
    fn escape_unheard_no_activation_takes_the_window_back_after_the_silent_wait() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut s = Switcher::default();
        s.hear(tab(false));
        let w = WindowId(5);
        assert_eq!(plan(Heard::Selected { steps: Some(1), first: true }, Some(40), None, |_| Some(w)), vec![Act::Lend { pid: 40, window: w }]);
        let mut lent = LendWait::held(40);
        assert_eq!(lent.step(ms(250)), None, "the switcher up: no cap runs");
        s.list_gone(ms(300));
        lent.begin(ms(300), activation_cap(false, false));
        let heard = s.hear_at(Key::Command(false), ms(387));
        assert_eq!(heard, Heard::Released { steps: Some(1) }, "Escape unheard: ⌘ up looks like a release");
        assert_eq!(plan(heard, Some(40), Some((40, w)), |_| None), vec![Act::Release { window: w }]);
        lent.begin(ms(387), activation_cap(false, false));
        assert_eq!(lent.step(ms(586)), None, "the activation may still come");
        assert_eq!(lent.step(ms(587)), Some(Outcome::Cap), "no activation came: taken for a cancel");
    }

    /// With the key tap: Escape heard while the switcher shows cancels at once; a release
    /// heard waits for the activation, its cap long (the window waits parked off the screen).
    #[test]
    fn with_the_key_tap_escape_cancels_at_once_and_a_release_waits_long() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let w = WindowId(5);
        // Escape at the tap (⌘ held): cancelled, back now; the list gone after it decides nothing.
        let mut s = Switcher::default();
        s.hear(tab(false));
        let heard = s.hear_at(Key::Down { code: KEY_ESCAPE, cmd: true, shift: false }, ms(300));
        assert_eq!(heard, Heard::Cancelled);
        assert_eq!(plan(heard, Some(40), Some((40, w)), |_| None), vec![Act::TakeBack { window: w, why: "the switcher was cancelled" }]);
        s.list_gone(ms(301));
        assert!(!s.waits_gone(), "closed for the keys: nothing waits");
        // ⌘ up at the tap: released; a slow activation 1.5 s later still keeps it.
        let mut s = Switcher::default();
        s.hear(tab(false));
        assert_eq!(s.hear_at(Key::Command(false), ms(400)), Heard::Released { steps: Some(1) });
        let mut lent = LendWait::held(40);
        lent.begin(ms(400), activation_cap(false, true));
        assert_eq!(lent.step(ms(1000)), None, "past the old 200 ms: kept");
        assert_eq!(lent.activation(40, ms(1900), ms(1900)), None);
        assert_eq!(lent.shown(ms(1910)), Some(Outcome::Met));
        // Released on the app put home for, then another app activated: back at once.
        let mut lent = LendWait::held(40);
        lent.begin(ms(400), activation_cap(false, true));
        assert!(matches!(lent.activation(77, ms(420), ms(420)), Some(Outcome::Refuted(_))));
        // No activation at all: the cap says so.
        let mut lent = LendWait::held(40);
        lent.begin(ms(400), activation_cap(false, true));
        assert_eq!(lent.step(ms(400) + ACTIVATION_WAIT), Some(Outcome::Cap));
    }

    /// The release order: ⌘ up, the activation 10 ms later: kept until its switch (the caller
    /// forgets it once its stage is in front), back only if no switch came within the cap.
    #[test]
    fn release_order_with_the_activation_keeps_the_window() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut lent = LendWait::held(40);
        lent.begin(ms(400), activation_cap(false, true));
        lent.activation(40, ms(410), ms(410));
        lent.begin(ms(401), activation_cap(false, true)); // the list gone after ⌘ up
        assert_eq!(lent.step(ms(700)), None, "activated: kept");
        assert_eq!(lent.step(ms(410) + ACTIVATION_WAIT), Some(Outcome::Cap), "activated, but no switch within the cap");
        // The activation before ⌘ up is heard (both orders reach the WM thread).
        let mut early = LendWait::held(40);
        early.activation(40, ms(405), ms(405));
        assert_eq!(early.step(ms(900)), None, "held: no cap");
        early.begin(ms(410), activation_cap(false, true));
        assert_eq!(early.shown(ms(420)), Some(Outcome::Met));
    }

    #[test]
    fn a_guess_never_stands_in_for_the_docks_reading_once_its_list_showed() {
        use List::*;
        assert_eq!(choose(Some(7), Shown, false, true, Some(9)), Choice::Dock(7), "a reading wins");
        assert_eq!(choose(Some(7), Shown, true, false, Some(9)), Choice::Dock(7));
        assert_eq!(choose(None, Gone, false, true, Some(9)), Choice::Guess(Some(9)), "the first Tab, before the list shows");
        assert_eq!(choose(None, Gone, false, false, None), Choice::Guess(None));
        assert_eq!(choose(None, Gone, true, false, Some(9)), Choice::Gone, "a notice after a mouse pick: the list is gone");
        assert_eq!(choose(None, Gone, true, true, Some(9)), Choice::Gone);
        assert_eq!(choose(None, Unanswered, true, false, Some(9)), Choice::Keep, "the Dock silent at its own notice");
        assert_eq!(choose(None, Shown, true, false, Some(9)), Choice::Keep, "a selection that names no app");
        assert_eq!(choose(None, Unanswered, true, true, Some(9)), Choice::Guess(Some(9)), "a key moved it, the Dock silent");
    }

    /// Live 11-18: ⌘ held, VS Code read from the Dock and its window put home; a click on it
    /// dismissed the switcher, the next Dock notice found no list and the keys guessed Mail
    /// (steps 1: the mouse moves nothing for them). The guess took the window back as the
    /// click activated VS Code, and macOS slid away and back.
    #[test]
    fn a_mouse_pick_keeps_the_window_put_home_for_its_activation() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let (code, mail, w) = (757, 765, WindowId(3077));
        let sel = Heard::Selected { steps: Some(1), first: false };
        // A reading from the Dock that moves on: back at once, as before.
        assert_eq!(
            choose(Some(mail), List::Shown, true, false, Some(mail)),
            Choice::Dock(mail)
        );
        assert_eq!(plan(sel, Some(mail), Some((code, w)), |_| None), vec![Act::TakeBack { window: w, why: "the switcher's selection moved on" }]);
        // The list gone: the guess is no selection; the last reading (VS Code) stands, and the
        // window waits for its app's activation.
        assert_eq!(choose(None, List::Gone, true, false, Some(mail)), Choice::Gone);
        let mut go = LendWait::held(code);
        go.begin(ms(0), activation_cap(true, false));
        assert_eq!(go.step(ms(33)), None);
        // VS Code activated by the click (+33 ms): kept for its switch.
        let mut kept = go;
        kept.activation(code, ms(33), ms(33));
        assert_eq!(kept.step(ms(300)), None, "its switch takes it");
        assert_eq!(kept.shown(ms(320)), Some(Outcome::Met));
        // ⌘'s release (taken after GONE_WAIT) still names VS Code: released, not taken back.
        assert_eq!(plan(Heard::Released { steps: Some(1) }, Some(code), Some((code, w)), |_| None), vec![Act::Release { window: w }]);
        // No activation of VS Code (a click elsewhere), the tap not hearing: back after the
        // silent cap; from ⌘'s release, the later.
        assert_eq!(go.step(ms(399)), None, "past SILENT_ACTIVATION_WAIT, inside the cap");
        let mut later = go;
        later.begin(ms(150), activation_cap(false, false));
        assert_eq!(later.step(ms(549)), None, "from ⌘'s release, the later");
        assert_eq!(later.step(ms(550)), Some(Outcome::Cap));
        assert_eq!(go.step(ms(400)), Some(Outcome::Cap), "the cap: back");
        // The click heard by the key tap: the long cap, from the click.
        let mut tapped = LendWait::held(code);
        tapped.begin(ms(0), activation_cap(true, true));
        assert_eq!(tapped.step(ms(1500)), None, "a slow activation at the mouse's release");
        assert_eq!(tapped.step(ms(0) + UNREAD_WAIT), Some(Outcome::Cap));
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

    #[test]
    fn the_desktop_of_the_app_selected_is_arranged_ahead_and_undone_when_the_selection_moves_on() {
        let w = WindowId;
        // App 40's window 5 waits on another Desktop; app 20 has a window here.
        let slides = |pid: Pid| (pid == 40).then_some(w(5));
        let first = Heard::Selected { steps: Some(1), first: true };
        let next = Heard::Selected { steps: Some(2), first: false };
        assert_eq!(plan_ahead(first, Some(40), None, true, slides), vec![AheadAct::Arrange { pid: 40, window: w(5) }], "at the first Tab's guess");
        assert_eq!(plan_ahead(next, Some(40), Some((40, w(5))), true, slides), vec![], "the Dock's list agrees: it stands as it is");
        assert_eq!(plan_ahead(first, Some(20), None, true, slides), vec![], "an app macOS activates here: nothing");
        assert_eq!(plan_ahead(first, None, None, true, slides), vec![], "no guess: nothing");
        assert_eq!(
            plan_ahead(next, Some(20), Some((40, w(5))), true, slides),
            vec![AheadAct::Undo { why: "the switcher's selection moved on" }],
            "the Dock's list names another app, or another Tab: undone"
        );
        // The guess was app 20 (nothing arranged); the Dock's list says app 40.
        assert_eq!(plan_ahead(next, Some(40), None, true, slides), vec![AheadAct::Arrange { pid: 40, window: w(5) }]);
        // Two apps on other Desktops in a row: one undone, the next arranged.
        let both = |pid: Pid| Some(w(pid as u32));
        assert_eq!(
            plan_ahead(next, Some(41), Some((40, w(40))), true, both),
            vec![AheadAct::Undo { why: "the switcher's selection moved on" }, AheadAct::Arrange { pid: 41, window: w(41) }]
        );
        assert_eq!(plan_ahead(Heard::Cancelled, Some(40), Some((40, w(5))), true, slides), vec![AheadAct::Undo { why: "the switcher was cancelled" }]);
        assert!(matches!(plan_ahead(Heard::Lost, Some(40), Some((40, w(5))), true, slides)[..], [AheadAct::Undo { .. }]));
        assert_eq!(plan_ahead(Heard::Cancelled, Some(40), None, true, slides), vec![], "nothing arranged, nothing to undo");
        assert_eq!(plan_ahead(Heard::Nothing, Some(40), Some((40, w(5))), true, slides), vec![]);
    }

    #[test]
    fn at_the_release_the_arrangement_stands_only_when_the_release_was_heard_for_one() {
        let w = WindowId(5);
        let released = Heard::Released { steps: Some(1) };
        let none = |_: Pid| None;
        assert_eq!(plan_ahead(released, Some(40), Some((40, w)), true, none), vec![AheadAct::Stand { window: w }], "⌘ let go on its app");
        assert_eq!(
            plan_ahead(released, Some(20), Some((40, w)), true, none),
            vec![AheadAct::Undo { why: "⌘ was let go on another app" }]
        );
        assert!(matches!(plan_ahead(released, None, Some((40, w)), true, none)[..], [AheadAct::Undo { .. }]), "the selection unknown");
        // A release taken from the Dock's list gone, the key tap not hearing: Escape looks
        // the same, so it is undone and the Desktop is arranged on arrival, as before.
        assert!(matches!(plan_ahead(released, Some(40), Some((40, w)), false, none)[..], [AheadAct::Undo { .. }]));
        assert_eq!(plan_ahead(released, Some(40), None, true, |_| Some(w)), vec![], "nothing is arranged at the release itself");
    }
}
