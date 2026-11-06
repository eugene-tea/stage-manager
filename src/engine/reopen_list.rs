//! Joining macOS's reopen list at a logout (round reopen-list, `[app] reopen_with_apps`).
//!
//! At login `loginwindow` relaunches the apps of its persistent-apps list about 3 s after
//! the login and holds the login items until every one of them has checked in (12-27 s on
//! his Mac, 11-06). The list holds the regular (Dock) apps running as the user logged out;
//! Stage Manager is an accessory app (`LSUIElement`), so it was never on it and started with
//! the login items, after the windows had shown. AppKit's header for
//! `disableRelaunchOnLogin` says an app "running at the time the user logged out" may be
//! relaunched unless it opted out; the accessory apps left off the list (ours, 1Password)
//! say the activation policy counts too.
//!
//! So at the logout's first signal the app turns regular (a Dock icon for a moment, no
//! window, not activated), and macOS lists it with the apps it reopens. A cancelled logout
//! turns it back: a cancel signal, or no quit within [`LOGOUT_WAIT`] of the latest signal or
//! app quit (the head start's own rule, [`crate::engine::stand_down::Logout::cancelled`]). The app
//! launches as an accessory app as ever (`LSUIElement` stays).
//!
//! [`ReopenSwitch`] is the pure rule; the shell carries out its [`Policy`].

use std::time::Instant;

use crate::engine::stand_down::{LOGOUT_WAIT, Signal};

/// The app's activation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// A regular app: in the Dock, and on the list of apps macOS reopens.
    Regular,
    /// An accessory app (`LSUIElement`), as the app always runs.
    Accessory,
}

/// Whether this copy may join the list: `[app] reopen_with_apps` on, launch at login on in
/// System Settings (`starts_at_login`: an app the user keeps from starting at login is not
/// brought back this way either), and not a test run (`test`: `STAGE_MANAGER_TEST` or
/// `STAGE_MANAGER_HOME`). The dev copy and builds never start at login, so never join.
pub fn allowed(key_on: bool, starts_at_login: bool, test: bool) -> bool {
    key_on && starts_at_login && !test
}

/// The switch through a logout: regular from the first "coming" signal while allowed, back
/// to accessory when the logout is cancelled.
#[derive(Debug, Default)]
pub struct ReopenSwitch {
    /// The latest signal of the logout that turned the app regular; `None`: accessory.
    regular_since: Option<Instant>,
}

impl ReopenSwitch {
    /// A logout signal at `at`; `allowed` ([`allowed`]) is asked at the first. Returns the
    /// policy to set, if it changes.
    pub fn signal(&mut self, signal: Signal, at: Instant, allowed: impl FnOnce() -> bool) -> Option<Policy> {
        match (signal, self.regular_since.is_some()) {
            (Signal::Coming, true) => {
                self.regular_since = Some(at);
                None
            }
            (Signal::Coming, false) if allowed() => {
                self.regular_since = Some(at);
                Some(Policy::Regular)
            }
            (Signal::Cancelled, true) => {
                self.regular_since = None;
                Some(Policy::Accessory)
            }
            _ => None,
        }
    }

    /// An app quit at `at`: while regular for a logout, the logout is still under way and the
    /// wait counts from it (as [`crate::engine::stand_down::Logout::app_quit`]).
    pub fn app_quit(&mut self, at: Instant) {
        if let Some(since) = self.regular_since.as_mut().filter(|s| at > **s) {
            *since = at;
        }
    }

    /// At `now`: no quit came within [`LOGOUT_WAIT`] of the latest signal or app quit, so the
    /// logout was cancelled without a word: back to accessory (once).
    pub fn tick(&mut self, now: Instant) -> Option<Policy> {
        let since = self.regular_since?;
        (now.saturating_duration_since(since) >= LOGOUT_WAIT).then(|| {
            self.regular_since = None;
            Policy::Accessory
        })
    }

    /// True while the app is regular for a logout.
    pub fn regular(&self) -> bool {
        self.regular_since.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn regular_at_the_logouts_first_signal_when_allowed() {
        let t = Instant::now();
        let mut s = ReopenSwitch::default();
        assert_eq!(s.signal(Signal::Coming, t, || true), Some(Policy::Regular));
        assert!(s.regular());
        let mut asked = false;
        assert_eq!(s.signal(Signal::Coming, t + Duration::from_millis(100), || { asked = true; true }), None, "a repeat");
        assert!(!asked, "asked once, at the first");
        assert_eq!(s.tick(t + Duration::from_secs(30)), None, "the quit comes within the wait");
    }

    #[test]
    fn back_to_accessory_when_the_logout_is_cancelled() {
        let t = Instant::now();
        let mut s = ReopenSwitch::default();
        s.signal(Signal::Coming, t, || true);
        assert_eq!(s.signal(Signal::Cancelled, t + Duration::from_secs(5), || true), Some(Policy::Accessory));
        assert!(!s.regular());
        assert_eq!(s.signal(Signal::Cancelled, t + Duration::from_secs(6), || true), None, "once");

        // Cancelled without a word: no quit within the wait of the latest signal.
        let mut s = ReopenSwitch::default();
        s.signal(Signal::Coming, t, || true);
        s.signal(Signal::Coming, t + Duration::from_secs(60), || true);
        assert_eq!(s.tick(t + LOGOUT_WAIT), None, "counted from the latest signal");
        assert_eq!(s.tick(t + Duration::from_secs(60) + LOGOUT_WAIT), Some(Policy::Accessory));
        assert_eq!(s.tick(t + Duration::from_secs(600)), None, "once");
        // And the next logout turns it regular again.
        assert_eq!(s.signal(Signal::Coming, t + Duration::from_secs(700), || true), Some(Policy::Regular));
    }

    /// The 1.7 review: a slow restart (apps saving, one asking about unsaved changes) keeps the
    /// app on the reopen list while apps still quit.
    #[test]
    fn apps_quitting_keep_the_logout_under_way() {
        let t = Instant::now();
        let mut s = ReopenSwitch::default();
        s.app_quit(t);
        assert!(!s.regular(), "no logout: an app quit is nothing");
        s.signal(Signal::Coming, t, || true);
        s.app_quit(t + Duration::from_secs(100));
        assert_eq!(s.tick(t + LOGOUT_WAIT), None, "an app quit 100 s in: still under way");
        assert_eq!(s.tick(t + Duration::from_secs(100) + LOGOUT_WAIT), Some(Policy::Accessory));
    }

    #[test]
    fn never_when_not_allowed() {
        let t = Instant::now();
        let mut s = ReopenSwitch::default();
        assert_eq!(s.signal(Signal::Coming, t, || false), None);
        assert_eq!(s.signal(Signal::Cancelled, t, || true), None, "nothing to turn back");
        assert_eq!(s.tick(t + LOGOUT_WAIT), None);
        assert!(!allowed(false, true, false), "the key off");
        assert!(!allowed(true, false, false), "launch at login off (or the dev copy, a build)");
        assert!(!allowed(true, true, true), "a test run");
        assert!(allowed(true, true, false));
    }
}
