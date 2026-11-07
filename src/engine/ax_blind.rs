//! Accessibility blind for the whole session (his Mac, 11-06): WindowServer hung on wake and
//! macOS's watchdog logged him out; after the login Accessibility listed no window of any app
//! (every app answered, with no windows; System Events counted none either), while the window
//! server showed them all. Only another logout fixes it. Stage Manager can do nothing then: it
//! says so in its menu and keeps every saved stage, and once Accessibility lists windows again
//! (in the same run) the restore runs as at a launch after a restart.
//!
//! **The line** ([`Facts::counts`], held [`HOLD`]): Accessibility is trusted, the window server
//! shows at least [`MIN_WINDOWS`] real windows of regular apps on screen, of at least
//! [`MIN_APPS`] apps that answer Accessibility, and Accessibility lists no window at all. Not
//! counted: [`LOGIN_GRACE`] from the login (apps still starting), the screen locked, Mission
//! Control or a full-screen Space showing (the platform's `overview`). One app quiet is not it
//! (another app's windows are listed). Any exception resets the hold. Once declared, only a
//! window listed again ends it (or Accessibility no longer trusted: the menu asks for it then).
//!
//! **The restore while it sees nothing** ([`Watch::sees_nothing`], [`Watch::user_ends_restore`]):
//! from the first silent look (no grace, no hold), and while the window server shows windows
//! of regular apps (answering yet or not) before Accessibility has listed any in this run, a
//! click or a key press does not end the restore window: one would end it on nothing (11-06 15:40:07.6, 0.2 s after it opened: the two windows waiting on the Desktop
//! shown lost their saved stages a minute later). Its own ends (apps quiet, five minutes) and
//! the give-ups of waiting windows hold too while it sees nothing.
//!
//! Pure: the platform feeds the facts about once a second.

use std::time::{Duration, Instant};

/// How long the facts must hold before Accessibility counts as blind.
pub const HOLD: Duration = Duration::from_secs(30);

/// Not counted this soon after the login: apps are still starting and answering.
pub const LOGIN_GRACE: Duration = Duration::from_secs(30);

/// The window server must show at least this many real windows of regular apps on screen.
pub const MIN_WINDOWS: usize = 3;

/// ...of at least this many apps that answer Accessibility (one app quiet is that app's).
pub const MIN_APPS: usize = 2;

/// What the platform sees at one look.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Facts {
    /// Accessibility is trusted (`AXIsProcessTrusted`).
    pub trusted: bool,
    /// The screen is locked (or the session is not on the console).
    pub locked: bool,
    /// Mission Control, App Exposé or Show Desktop shows, or a full-screen Space does.
    pub overview: bool,
    /// Since the login, when known.
    pub since_login: Option<Duration>,
    /// Real windows of observed (regular, answering) apps the window server shows on screen.
    pub shown: usize,
    /// Real windows on screen of every regular app that would be observed, answering
    /// Accessibility yet or not (at least `shown`).
    pub regular: usize,
    /// The apps of those windows that answer Accessibility (no error, the window list read).
    pub answering: usize,
    /// Windows Accessibility lists, of any app, on screen or not.
    pub listed: usize,
}

impl Facts {
    /// Accessibility trusted and answering, the window server showing windows of several apps,
    /// Accessibility listing none: silent now, exceptions aside.
    pub fn silent(&self) -> bool {
        self.trusted && self.listed == 0 && self.shown >= MIN_WINDOWS && self.answering >= MIN_APPS
    }

    /// [`Facts::silent`] and none of the exceptions: the look counts towards [`HOLD`].
    pub fn counts(&self) -> bool {
        self.silent() && !self.locked && !self.overview && self.since_login.is_none_or(|d| d >= LOGIN_GRACE)
    }
}

/// A change of the verdict, for the log, the menu and the restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Blind from now (the facts held since `held` ago).
    Began {
        /// How long the facts held.
        held: Duration,
    },
    /// No longer blind.
    Ended {
        /// How long it lasted from its start (the hold's first look).
        lasted: Duration,
        /// A window is listed again: the restore runs again ([`Watch`]'s module docs). False
        /// when Accessibility is no longer trusted (nothing can be seen; the menu asks).
        recovered: bool,
    },
}

/// The verdict over time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Watch {
    /// The first look of the current hold.
    held_since: Option<Instant>,
    /// Blind since (the hold's first look), once declared.
    blind_since: Option<Instant>,
    /// The last look was silent ([`Facts::silent`], the screen not locked).
    silent: bool,
    /// Accessibility has listed a window in this run.
    listed_once: bool,
    /// The last look: the window server showed windows of regular apps, Accessibility listed
    /// none (apps not answering yet, or blind).
    unlisted: bool,
}

impl Watch {
    /// One look at `now`; the change it makes, if any.
    pub fn feed(&mut self, facts: &Facts, now: Instant) -> Option<Change> {
        self.silent = facts.silent() && !facts.locked;
        self.unlisted = facts.listed == 0 && facts.regular.max(facts.shown) >= MIN_WINDOWS;
        if facts.listed > 0 {
            self.listed_once = true;
            return self.end(now, true);
        }
        if !facts.trusted {
            return self.end(now, false);
        }
        if self.blind_since.is_some() {
            // Only a window listed ends it: a lock, Mission Control or fewer windows shown
            // say nothing about Accessibility.
            return None;
        }
        if !facts.counts() {
            self.held_since = None;
            return None;
        }
        let since = *self.held_since.get_or_insert(now);
        let held = now.saturating_duration_since(since);
        if held < HOLD {
            return None;
        }
        self.blind_since = Some(since);
        Some(Change::Began { held })
    }

    /// A window is listed (the observer found one between looks): [`Watch::feed`] with a
    /// listed window, the rest unknown.
    pub fn listed(&mut self, now: Instant) -> Option<Change> {
        self.silent = false;
        self.unlisted = false;
        self.listed_once = true;
        self.end(now, true)
    }

    fn end(&mut self, now: Instant, recovered: bool) -> Option<Change> {
        self.held_since = None;
        let since = self.blind_since.take()?;
        Some(Change::Ended { lasted: now.saturating_duration_since(since), recovered })
    }

    /// True while Accessibility counts as blind.
    pub fn blind(&self) -> bool {
        self.blind_since.is_some()
    }

    /// Since when it is blind (the hold's first look).
    pub fn since(&self) -> Option<Instant> {
        self.blind_since
    }

    /// True while the app sees nothing it could restore into: blind, or the last look silent
    /// (no hold, no grace). The restore window's own ends and the give-ups of waiting windows
    /// hold meanwhile.
    pub fn sees_nothing(&self) -> bool {
        self.blind() || self.silent
    }

    /// True when what a save says was looked for: Accessibility has listed a window in this
    /// run and is not blind now (else [`crate::engine::persist::StateFile::not_looked_for`]).
    pub fn looked(&self) -> bool {
        self.listed_once && !self.blind()
    }

    /// True when a click or a key press may end the restore window: the app sees windows, and
    /// Accessibility has listed one in this run or the window server shows none (an empty
    /// screen); a click on a screen it cannot see would end the restore on nothing.
    pub fn user_ends_restore(&self) -> bool {
        !self.sees_nothing() && (self.listed_once || !self.unlisted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// His Mac at 15:40 (11-06): 15 windows of 14 apps on screen, every app answering, none
    /// listed; logged in long before.
    fn blind_mac() -> Facts {
        Facts { trusted: true, locked: false, overview: false, since_login: Some(Duration::from_secs(2200)), shown: 15, regular: 15, answering: 14, listed: 0 }
    }

    fn at(t0: Instant, secs: u64) -> Instant {
        t0 + Duration::from_secs(secs)
    }

    /// Feeds `facts` once a second from `from` to `to` (inclusive); the first change and when.
    fn run(w: &mut Watch, facts: Facts, t0: Instant, from: u64, to: u64) -> Option<(u64, Change)> {
        (from..=to).find_map(|s| w.feed(&facts, at(t0, s)).map(|c| (s, c)))
    }

    #[test]
    fn accessibility_listing_nothing_for_thirty_seconds_is_blind() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        assert_eq!(run(&mut w, blind_mac(), t0, 0, 29), None, "held 29 s: not yet");
        assert!(!w.blind() && w.sees_nothing(), "silent already: the restore holds");
        assert_eq!(w.feed(&blind_mac(), at(t0, 30)), Some(Change::Began { held: HOLD }));
        assert!(w.blind() && w.since() == Some(t0) && !w.looked(), "a save says nothing was looked for");
        assert_eq!(run(&mut w, blind_mac(), t0, 31, 600), None, "said once");
    }

    #[test]
    fn a_window_listed_again_ends_it_at_once() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        run(&mut w, blind_mac(), t0, 0, 30);
        assert!(w.blind());
        let back = Facts { listed: 1, ..blind_mac() };
        assert_eq!(w.feed(&back, at(t0, 400)), Some(Change::Ended { lasted: Duration::from_secs(400), recovered: true }));
        assert!(!w.blind() && !w.sees_nothing() && w.user_ends_restore() && w.looked());
        assert_eq!(w.feed(&back, at(t0, 401)), None);
        // The observer finding a window between looks ends it the same way.
        let mut w = Watch::default();
        run(&mut w, blind_mac(), t0, 0, 30);
        assert_eq!(w.listed(at(t0, 45)), Some(Change::Ended { lasted: Duration::from_secs(45), recovered: true }));
        assert_eq!(w.listed(at(t0, 46)), None);
    }

    #[test]
    fn the_exceptions_never_start_it_and_reset_the_hold() {
        let t0 = Instant::now();
        let cases = [
            ("one app quiet: the others list theirs", Facts { listed: 4, ..blind_mac() }),
            ("one app showing windows", Facts { answering: 1, ..blind_mac() }),
            ("apps not answering", Facts { answering: 0, shown: 0, ..blind_mac() }),
            ("a stray window or two", Facts { shown: MIN_WINDOWS - 1, ..blind_mac() }),
            ("the screen locked", Facts { locked: true, ..blind_mac() }),
            ("Mission Control or full screen", Facts { overview: true, ..blind_mac() }),
            ("the login's first seconds", Facts { since_login: Some(Duration::from_secs(5)), ..blind_mac() }),
            ("not trusted", Facts { trusted: false, ..blind_mac() }),
        ];
        for (why, facts) in cases {
            let mut w = Watch::default();
            assert_eq!(run(&mut w, facts, t0, 0, 3600), None, "{why}");
            // 20 s silent, one look of the exception, 20 s silent: the hold started again.
            let mut w = Watch::default();
            assert_eq!(run(&mut w, blind_mac(), t0, 0, 20), None);
            assert_eq!(w.feed(&facts, at(t0, 21)), None, "{why}");
            assert_eq!(run(&mut w, blind_mac(), t0, 22, 51), None, "{why}: the hold starts again");
            assert_eq!(w.feed(&blind_mac(), at(t0, 52)), Some(Change::Began { held: HOLD }), "{why}");
        }
        // The login's grace: counted from 30 s after the login, declared 30 s later.
        let mut w = Watch::default();
        let login = |s: u64| Facts { since_login: Some(Duration::from_secs(s)), ..blind_mac() };
        let first = (0..120).find_map(|s| w.feed(&login(s), at(t0, s)).map(|_| s));
        assert_eq!(first, Some(60));
    }

    #[test]
    fn once_blind_only_a_listed_window_or_the_permission_ends_it() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        run(&mut w, blind_mac(), t0, 0, 30);
        for facts in [
            Facts { locked: true, ..blind_mac() },
            Facts { overview: true, ..blind_mac() },
            Facts { shown: 0, answering: 0, ..blind_mac() },
        ] {
            assert_eq!(w.feed(&facts, at(t0, 60)), None);
            assert!(w.blind());
        }
        assert_eq!(w.feed(&Facts { trusted: false, ..blind_mac() }, at(t0, 90)), Some(Change::Ended { lasted: Duration::from_secs(90), recovered: false }));
    }

    #[test]
    fn a_click_ends_the_restore_only_once_accessibility_has_listed_a_window() {
        let t0 = Instant::now();
        // 15:40:07.6: the stages built from nothing, a click 0.2 s after: kept open.
        let mut w = Watch::default();
        w.feed(&blind_mac(), t0);
        assert!(!w.user_ends_restore() && w.sees_nothing());
        run(&mut w, blind_mac(), t0, 1, 30);
        assert!(!w.user_ends_restore(), "blind");
        // At the login's first seconds, apps not answering yet: not silent, but the window
        // server shows windows nothing has listed yet.
        let mut w = Watch::default();
        w.feed(&Facts { shown: 0, answering: 0, ..blind_mac() }, t0);
        assert!(!w.sees_nothing() && !w.user_ends_restore());
        // An empty screen after a login (nothing shown, nothing listed): a click ends it, as
        // ever.
        let mut w = Watch::default();
        w.feed(&Facts { shown: 0, regular: 0, answering: 0, ..blind_mac() }, t0);
        assert!(!w.sees_nothing() && w.user_ends_restore());
        // A normal launch: windows listed, a click ends the restore.
        let mut w = Watch::default();
        w.feed(&Facts { listed: 9, ..blind_mac() }, t0);
        assert!(w.user_ends_restore());
        // Silent later on (no hold yet): the restore's ends wait, the click too.
        w.feed(&blind_mac(), at(t0, 5));
        assert!(w.sees_nothing() && !w.user_ends_restore());
        // A locked screen is not silence (the lock has its own hold).
        w.feed(&Facts { locked: true, ..blind_mac() }, at(t0, 6));
        assert!(!w.sees_nothing());
    }
}
