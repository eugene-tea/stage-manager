//! Focus and activations that land before the Desktop they are for is recognized: held, then
//! replayed once it is.
//!
//! A Dock click, ⌘Tab or an activation whose window lives on another Desktop makes macOS
//! show that Desktop with the window focused. Apple's Stage Manager shows it with **that
//! window's stage** active. The events come in no fixed order: on the owner's Mac (10-12,
//! VS Code with a window on each Desktop) the window's focus arrived ~40 ms before the Space
//! change notice and ~0.7 s before the Desktop was recognized, when acting on it was
//! impossible (a stage of a Desktop not shown never switches: acting there would make macOS
//! switch Spaces), and it was lost: the Desktop came up with its previous stage.
//!
//! So a focus or activation that cannot act yet (its window's Desktop is not the one shown,
//! the window is not known yet, or a Space change or full-screen animation is settling) is
//! **held** here ([`FocusReplay::hold`]); when a Desktop is recognized, and when a wait ends,
//! the controller asks Accessibility for the focused window **now** and hands both to
//! [`crate::engine::model::Model::bring_focus_forward`], which switches to the stage the
//! user went there for. A held event older than [`FOCUS_REPLAY_MAX`] is dropped (it no
//! longer says what the user is doing), and any act of the user's on the Desktop shown
//! (a card click, a hotkey, a focus that acted) clears it.

use std::time::{Duration, Instant};

use super::model::{Pid, WindowId};

/// A held focus or activation is replayed at most this long after it came: a Space change
/// recognized within ~0.1–0.7 s, Mission Control's exit and its settle (~0.8 s), a
/// full-screen animation's settle (at most 2 s).
pub const FOCUS_REPLAY_MAX: Duration = Duration::from_secs(3);

/// What the user went to another Desktop for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    /// macOS focused this window (of app `pid`).
    Window {
        /// The window.
        id: WindowId,
        /// Its app.
        pid: Pid,
    },
    /// This app became frontmost (no window focus came with it yet).
    App(Pid),
}

impl Wanted {
    /// The app it is about.
    pub fn pid(self) -> Pid {
        match self {
            Wanted::Window { pid, .. } | Wanted::App(pid) => pid,
        }
    }
}

/// The last focus or activation that could not act yet, and when it came.
#[derive(Debug, Clone, Copy, Default)]
pub struct FocusReplay {
    held: Option<(Wanted, Instant)>,
}

impl FocusReplay {
    /// Holds `wanted` (the newest wins), except that an activation of the app whose window
    /// focus is held does not replace that window (macOS reports both, in either order).
    pub fn hold(&mut self, wanted: Wanted, now: Instant) {
        if let (Wanted::App(pid), Some((held @ Wanted::Window { .. }, at))) = (wanted, self.held)
            && held.pid() == pid
            && now.duration_since(at) < FOCUS_REPLAY_MAX
        {
            return;
        }
        self.held = Some((wanted, now));
    }

    /// The held event, if it is recent enough; it is gone from here either way.
    pub fn take(&mut self, now: Instant) -> Option<Wanted> {
        self.held.take().filter(|(_, at)| now.duration_since(*at) < FOCUS_REPLAY_MAX).map(|(w, _)| w)
    }

    /// The held event, left held.
    pub fn peek(&self, now: Instant) -> Option<Wanted> {
        self.held.filter(|(_, at)| now.duration_since(*at) < FOCUS_REPLAY_MAX).map(|(w, _)| w)
    }

    /// Forgets it (the user acted on the Desktop shown).
    pub fn clear(&mut self) {
        self.held = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn the_newest_wins_and_an_old_one_is_dropped() {
        let t0 = Instant::now();
        let mut r = FocusReplay::default();
        assert_eq!(r.take(t0), None);
        r.hold(Wanted::Window { id: WindowId(1), pid: 10 }, t0);
        r.hold(Wanted::Window { id: WindowId(2), pid: 20 }, ms(t0, 100));
        assert_eq!(r.peek(ms(t0, 200)), Some(Wanted::Window { id: WindowId(2), pid: 20 }));
        assert_eq!(r.take(ms(t0, 200)), Some(Wanted::Window { id: WindowId(2), pid: 20 }));
        assert_eq!(r.take(ms(t0, 300)), None, "taken once");
        r.hold(Wanted::App(30), t0);
        assert_eq!(r.take(t0 + FOCUS_REPLAY_MAX), None, "too old");
        r.hold(Wanted::App(30), t0);
        r.clear();
        assert_eq!(r.take(t0), None);
    }

    #[test]
    fn an_activation_never_replaces_its_own_windows_focus() {
        let t0 = Instant::now();
        let mut r = FocusReplay::default();
        let win = Wanted::Window { id: WindowId(31310), pid: 37564 };
        // Focus first, the activation after (the owner's VS Code log), or the other way round.
        r.hold(win, t0);
        r.hold(Wanted::App(37564), ms(t0, 40));
        assert_eq!(r.take(ms(t0, 700)), Some(win));
        r.hold(Wanted::App(37564), t0);
        r.hold(win, ms(t0, 40));
        assert_eq!(r.take(ms(t0, 700)), Some(win));
        // Another app's activation does replace it.
        r.hold(win, t0);
        r.hold(Wanted::App(41), ms(t0, 40));
        assert_eq!(r.take(ms(t0, 100)), Some(Wanted::App(41)));
    }
}
