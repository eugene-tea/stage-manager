//! A close of Mission Control by its key is not a pick (round mc-key; the Spaces engine).
//!
//! Mission Control closes three ways: a click on a window (a pick: macOS activates the
//! window's app and the activation's switch brings its stage in front, round mc-pick), a click
//! elsewhere, or a key: its shortcut again, the keyboard's Mission Control key, or Escape.
//! At a close by key macOS may still bring an app in front: 11-20 11:16:36 the owner closed
//! it by its key from the stage in front and Terminal, whose window was out for Mission
//! Control, came in front; ours took that activation for a pick and switched to Terminal's
//! stage. Nothing was picked.
//!
//! So an activation (or a focus) that comes within [`KEY_COUNTS`] after the key that closed
//! Mission Control, with no click since that key, is not a pick ([`verdict`]): no stage
//! switch, the windows put home go back with the others, and the stage in front gets its own
//! key window's focus back once Mission Control's own window is gone. A click while Mission
//! Control shows (a mouse-down) is a pick as before. Once an app's activation was found no
//! pick, its later notices of the same close are not one either ([`CloseWatch::refused`]):
//! its window's focus follows its activation by tens of ms.
//!
//! Which app macOS brings in front at such a close (the one under the pointer, the top one
//! put home, or none) is not measured: `scripts/probe_apple.py mission-close-key` measures it
//! with Apple's Stage Manager and with ours. Any answer is covered: with none, nothing comes
//! to decide; with one, it is no pick. What the measurement may change is
//! [`APPLE_SWITCHES_AT_CLOSE_KEY`]: should Apple's bring that app's stage in front, the
//! activation is a pick after all (Apple's way stays the default), and [`KEY_COUNTS`], should
//! the activation come later than it allows.

use std::time::{Duration, Instant};

use super::mission_control::{Keys, Mods, trigger};
use super::model::Pid;

/// Escape's virtual key code: it closes Mission Control and App Exposé.
pub const ESCAPE_KEY: u16 = 53;

/// An activation or a focus this long after the key that closed Mission Control (no click
/// since) is macOS's, not a pick: Mission Control's close plays about half a second (its
/// own window gone 0.53 s after a pick, `scripts/probe_apple.py mission-pick`, 11-19), and the
/// owner's unwanted activation came while it still showed (11-20 11:16:36).
pub const KEY_COUNTS: Duration = Duration::from_millis(1000);

/// Whether Apple's Stage Manager brings in front the stage of the app macOS activates at a
/// close by key. Not measured: false, the stage in front stays (as Apple's keeps the stage in
/// front when nothing is picked). `mission-close-key` settles it: true makes such an
/// activation a pick again, as Apple's would have it.
pub const APPLE_SWITCHES_AT_CLOSE_KEY: bool = false;

/// The key that closes Mission Control or App Exposé while one shows: its own shortcuts
/// ([`trigger`]: the user's, the keyboard's Mission Control key) or Escape (with no ⌘, ⌥ or
/// ⌃: ⌘Escape and the like are other shortcuts).
pub fn closes(keys: &Keys, keycode: u16, mods: Mods) -> bool {
    (keycode == ESCAPE_KEY && mods == Mods::default()) || trigger(keys, keycode, mods).is_some()
}

/// What Mission Control's close came with: the key that closed it, a click while it showed,
/// and the app whose activation at this close was found no pick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CloseWatch {
    key: Option<Instant>,
    click: Option<Instant>,
    refused: Option<Pid>,
}

impl CloseWatch {
    /// Mission Control shows anew: nothing of the last one counts.
    pub fn opened(&mut self) {
        *self = Self::default();
    }

    /// A key that closes it ([`closes`]) pressed at `now` while it shows.
    pub fn key(&mut self, now: Instant) {
        self.key = Some(now);
    }

    /// A mouse-down at `now` while it shows: what follows is a pick.
    pub fn click(&mut self, now: Instant) {
        self.click = Some(now);
    }

    /// True when Mission Control was closed by its key within [`KEY_COUNTS`] of `now`, no
    /// click after that key.
    pub fn closed_by_key(&self, now: Instant) -> bool {
        let Some(key) = self.key else { return false };
        now.saturating_duration_since(key) < KEY_COUNTS && self.click.is_none_or(|c| c < key)
    }

    /// The app found no pick at this close, if any.
    pub fn refused(&self) -> Option<Pid> {
        self.refused
    }

    /// App `pid`'s activation was found no pick: its later notices of this close are not one.
    pub fn refuse(&mut self, pid: Pid) {
        self.refused = Some(pid);
    }

    /// The close is over (every window put home is back): nothing counts any more.
    pub fn over(&mut self) {
        *self = Self::default();
    }
}

/// What an activation (or focus) of `pid` while windows are out for Mission Control is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A pick: its switch as before (round mc-pick).
    Pick,
    /// Not a pick: no switch; the stage in front keeps the focus it had (its key window
    /// focused again once Mission Control's own window is gone). `own_front`: the app's own
    /// stage is in front (it gets that stage's focus back), else it has no stage in front
    /// (the stage in front stays, as Apple's for an activation without a pick).
    NoPick { own_front: bool },
}

/// What the activation (or focus) of app `pid` at `now` is, `watch` what this close came
/// with: no pick when Mission Control was closed by its key and no click came
/// ([`CloseWatch::closed_by_key`]), or when `pid` was found no pick already at this close.
/// `own_front`: `pid` has a window in the stage in front; `apple_switches`:
/// [`APPLE_SWITCHES_AT_CLOSE_KEY`] (a parameter for the tests).
pub fn verdict(watch: &CloseWatch, pid: Pid, now: Instant, own_front: bool, apple_switches: bool) -> Verdict {
    let by_key = watch.closed_by_key(now) || watch.refused() == Some(pid);
    if !by_key || (apple_switches && !own_front) {
        Verdict::Pick
    } else {
        Verdict::NoPick { own_front }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::mission_control::{MISSION_CONTROL_KEY, Shortcut};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn keys() -> Keys {
        let ctrl = Mods { control: true, ..Mods::default() };
        Keys {
            mission_control: Shortcut { keycode: 126, mods: ctrl, enabled: true },
            app_windows: Shortcut { keycode: 125, mods: ctrl, enabled: true },
        }
    }

    #[test]
    fn its_shortcuts_the_keyboards_key_and_escape_close_it() {
        let k = keys();
        let ctrl = Mods { control: true, ..Mods::default() };
        assert!(closes(&k, 126, ctrl), "⌃↑ again");
        assert!(closes(&k, 125, ctrl), "App Exposé's ⌃↓");
        assert!(closes(&k, MISSION_CONTROL_KEY, Mods::default()), "the Mission Control key (F3)");
        assert!(closes(&k, ESCAPE_KEY, Mods::default()), "Escape");
        assert!(!closes(&k, ESCAPE_KEY, Mods { command: true, ..Mods::default() }), "⌘Escape is another shortcut");
        assert!(!closes(&k, 126, Mods::default()), "a plain arrow moves the selection");
        assert!(!closes(&k, 36, Mods::default()), "Return");
    }

    /// 11-20 11:16:36: closed by its key, Terminal (a hidden stage's app, its window out)
    /// activated while it still showed: no pick, the stage in front stays.
    #[test]
    fn an_activation_after_the_closing_key_is_no_pick() {
        let t0 = Instant::now();
        let mut w = CloseWatch::default();
        w.key(t0);
        assert_eq!(verdict(&w, 798, t0 + ms(300), false, false), Verdict::NoPick { own_front: false });
        assert_eq!(verdict(&w, 798, t0 + ms(300), true, false), Verdict::NoPick { own_front: true }, "its own stage in front: its focus back");
        assert_eq!(verdict(&w, 798, t0 + KEY_COUNTS, false, false), Verdict::Pick, "long after the key: macOS's own again");
    }

    #[test]
    fn a_click_is_a_pick_as_before() {
        let t0 = Instant::now();
        let mut w = CloseWatch::default();
        assert_eq!(verdict(&w, 7, t0, false, false), Verdict::Pick, "no key: a pick");
        w.click(t0);
        assert_eq!(verdict(&w, 7, t0 + ms(150), false, false), Verdict::Pick, "a click, no key");
        // A key after a click (the click missed, or closed nothing): the key counts.
        w.key(t0 + ms(200));
        assert!(w.closed_by_key(t0 + ms(300)));
        // A click after the key: a pick.
        w.click(t0 + ms(250));
        assert_eq!(verdict(&w, 7, t0 + ms(300), false, false), Verdict::Pick);
    }

    #[test]
    fn an_app_found_no_pick_stays_no_pick_for_the_close() {
        let t0 = Instant::now();
        let mut w = CloseWatch::default();
        w.key(t0);
        assert_eq!(verdict(&w, 798, t0 + ms(200), false, false), Verdict::NoPick { own_front: false });
        w.refuse(798);
        // Its window's focus comes later than the key's time: still no pick.
        assert_eq!(verdict(&w, 798, t0 + ms(1500), false, false), Verdict::NoPick { own_front: false });
        assert_eq!(verdict(&w, 5, t0 + ms(1500), false, false), Verdict::Pick, "another app's later activation is its own");
        w.over();
        assert_eq!(verdict(&w, 798, t0 + ms(1600), false, false), Verdict::Pick, "the close over");
        w.key(t0);
        w.opened();
        assert!(!w.closed_by_key(t0 + ms(10)), "opened again: the last close's key counts no more");
    }

    /// `mission-close-key` may find that Apple's brings the activated app's stage in front:
    /// then the activation is a pick, unless its stage is in front already.
    #[test]
    fn apples_switch_at_a_close_by_key_would_make_it_a_pick() {
        let t0 = Instant::now();
        let mut w = CloseWatch::default();
        w.key(t0);
        assert_eq!(verdict(&w, 9, t0 + ms(100), false, true), Verdict::Pick);
        assert_eq!(verdict(&w, 9, t0 + ms(100), true, true), Verdict::NoPick { own_front: true });
        assert_eq!(verdict(&w, 9, t0 + ms(100), false, APPLE_SWITCHES_AT_CLOSE_KEY), Verdict::NoPick { own_front: false }, "unmeasured: the stage in front stays");
    }
}
