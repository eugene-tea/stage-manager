//! After macOS quit the app for a logout, restart or shutdown: whether the logout is
//! cancelled after all, and Stage Manager opened again (round logout-cancel-relaunch).
//!
//! A logout that an app cancels (an unsaved document, the user's Cancel) stops where it is:
//! the apps already quit stay quit, ours among them, and every window it had brought back for
//! the logout stays on screen in one pile, with no stages. Apple's Stage Manager lives in the
//! window server and never meets this. So the app's quit at a logout writes its clean-exit
//! marker as [`LOGOUT_MARK`], and its guardian, which outlives it, waits a while instead of
//! exiting ([`Watch`]): a cancel signal (`loginwindow`'s `com.apple.logoutCancelled`, its
//! notify(3) name, or a test's own note) opens the app again, which restores the stages saved
//! at the logout's head start in the same session, by window id. A logout that goes on ends
//! the session and the guardian with it (its stop signal: nothing is opened); none of
//! [`WATCH`] passes with no cancel, or the app runs again already: nothing is opened either.
//!
//! A quit at a logout that left windows away (an app not answering, a window gone: no
//! clean-exit marker) says so in the parked file instead, and the guardian listens for the
//! cancel from the exit on, while its recovery watch brings those windows back
//! ([`after_exit`]): a cancel heard during the watch opens the app at once (the new copy
//! adopts the windows, or takes the rest of the watch, as after any crash), and once the
//! watch is done with no cancel yet, the guardian waits as after a clean quit (round
//! logout-cancel, 1.8.x).
//!
//! The guardian is no app: `loginwindow` never waits for it, and the app's own quit is as
//! quick as before.

use std::time::{Duration, Instant};

use super::stand_down::Signal;

/// How long the guardian waits for a cancel after the app quit for a logout, counted from the
/// quit or the latest logout signal since. An app asking about unsaved changes may wait on its
/// dialog for minutes; a logout that goes on ends the guardian long before (the session ends).
pub const WATCH: Duration = Duration::from_secs(600);

/// The clean-exit marker's text after a quit at a logout ([`quit_marker`]).
pub const LOGOUT_MARK: &str = "logout";

/// The clean-exit marker's text after any other clean quit.
pub const CLEAN_MARK: &str = "clean";

/// What a clean exit leaves in its marker: [`LOGOUT_MARK`] when the app quit for a logout
/// (`after_head_start`: the quit came while the logout's head start held, or `logout_reason`:
/// the system's quit said logout, restart or shutdown) and not by the user's own Quit
/// (`user_quit`: the menu's Quit, which a cancel must never undo).
pub fn quit_marker(after_head_start: bool, logout_reason: bool, user_quit: bool) -> &'static str {
    if (after_head_start || logout_reason) && !user_quit { LOGOUT_MARK } else { CLEAN_MARK }
}

/// Whether a clean-exit marker's text says the app quit for a logout.
pub fn quit_for_logout(marker: &str) -> bool {
    marker.trim() == LOGOUT_MARK
}

/// What the guardian does once the app it watches has exited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AfterExit {
    /// Run the recovery scan and watch: no clean-exit marker, not every window came back.
    pub recover: bool,
    /// Listen for the logout's cancel from the exit on (through the recovery watch, then
    /// [`WATCH`] as after a clean quit): the app quit for a logout.
    pub listen: bool,
}

/// The guardian's plan after the exit: `marker`, the clean-exit marker's text (`None`: no
/// marker, an unclean exit); `parked_logout`, whether the parked file says the quit was for a
/// logout (an unclean quit at a logout writes it there, [`quit_marker`] deciding as for the
/// marker; a crash never does).
pub fn after_exit(marker: Option<&str>, parked_logout: bool) -> AfterExit {
    match marker {
        Some(text) => AfterExit { recover: false, listen: quit_for_logout(text) },
        None => AfterExit { recover: true, listen: parked_logout },
    }
}

/// `keyAEQuitReason` values (AERegistry.h) that mean a logout, restart or shutdown: `logo`,
/// `rlgo` (log out, with and without its dialog), `rrst`, `rest` (restart), `rsdn`, `shut`
/// (shut down).
pub const LOGOUT_REASONS: [[u8; 4]; 6] = [*b"logo", *b"rlgo", *b"rrst", *b"rest", *b"rsdn", *b"shut"];

/// Whether the quit event's reason (`keyAEQuitReason`, a FourCharCode) is a logout, restart or
/// shutdown.
pub fn logout_reason(code: u32) -> bool {
    LOGOUT_REASONS.contains(&code.to_be_bytes())
}

/// A FourCharCode as text, for the log (`"none"` for 0, hex when not printable).
pub fn four_cc(code: u32) -> String {
    let b = code.to_be_bytes();
    match code {
        0 => "none".into(),
        _ if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') => b.iter().map(|c| *c as char).collect(),
        _ => format!("{code:#010x}"),
    }
}

/// What the guardian heard since its last look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
    /// Nothing.
    Nothing,
    /// A logout signal.
    Signal(Signal),
    /// A stop signal (SIGTERM, SIGHUP, SIGINT): the session ends, the logout goes on.
    Stopped,
}

/// Why the watch ended with nothing opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The session ends: the logout goes on.
    LogoutWentOn,
    /// No cancel within [`WATCH`].
    TimedOut,
    /// Cancelled, but Stage Manager runs already (opened by hand meanwhile).
    AlreadyRunning,
}

impl End {
    /// Plain words, for the log.
    pub fn words(self) -> &'static str {
        match self {
            End::LogoutWentOn => "the logout goes on (the session ends): nothing is opened",
            End::TimedOut => "no cancel came in time: nothing is opened",
            End::AlreadyRunning => "the logout was cancelled and Stage Manager runs already: nothing is opened",
        }
    }
}

/// What the guardian does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Keep waiting.
    Wait,
    /// The logout was cancelled: open Stage Manager again, then end.
    Relaunch,
    /// End with nothing opened.
    End(End),
}

/// The watch after a quit at a logout (module docs).
#[derive(Clone, Copy, Debug)]
pub struct Watch {
    /// The quit, or the latest logout signal since: [`WATCH`] counts from it.
    last: Instant,
}

impl Watch {
    /// A watch from the app's exit at `at`.
    pub fn new(at: Instant) -> Self {
        Watch { last: at }
    }

    /// What `heard` at `now` leads to. `running` is asked at a cancel only: whether Stage
    /// Manager runs again already.
    pub fn step(&mut self, heard: Heard, now: Instant, running: impl FnOnce() -> bool) -> Next {
        match heard {
            Heard::Stopped => Next::End(End::LogoutWentOn),
            Heard::Signal(Signal::Cancelled) if running() => Next::End(End::AlreadyRunning),
            Heard::Signal(Signal::Cancelled) => Next::Relaunch,
            Heard::Signal(Signal::Coming) => {
                if now > self.last {
                    self.last = now;
                }
                Next::Wait
            }
            Heard::Nothing if now.saturating_duration_since(self.last) >= WATCH => Next::End(End::TimedOut),
            Heard::Nothing => Next::Wait,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn a_cancel_opens_the_app_again() {
        let t = Instant::now();
        let mut w = Watch::new(t);
        assert_eq!(w.step(Heard::Nothing, t + 5 * S, || unreachable!("asked at a cancel only")), Next::Wait);
        assert_eq!(w.step(Heard::Signal(Signal::Cancelled), t + 40 * S, || false), Next::Relaunch);
    }

    #[test]
    fn a_logout_that_goes_on_opens_nothing() {
        let t = Instant::now();
        let mut w = Watch::new(t);
        assert_eq!(w.step(Heard::Signal(Signal::Coming), t + S, || true), Next::Wait, "logoutContinued after the quit");
        assert_eq!(w.step(Heard::Stopped, t + 2 * S, || false), Next::End(End::LogoutWentOn));
    }

    #[test]
    fn no_cancel_in_time_opens_nothing() {
        let t = Instant::now();
        let mut w = Watch::new(t);
        assert_eq!(w.step(Heard::Nothing, t + WATCH - S, || false), Next::Wait);
        assert_eq!(w.step(Heard::Nothing, t + WATCH, || false), Next::End(End::TimedOut));
    }

    #[test]
    fn the_wait_counts_from_the_latest_signal() {
        let t = Instant::now();
        let mut w = Watch::new(t);
        assert_eq!(w.step(Heard::Signal(Signal::Coming), t + 60 * S, || false), Next::Wait);
        assert_eq!(w.step(Heard::Nothing, t + WATCH, || false), Next::Wait, "the logout still under way");
        assert_eq!(w.step(Heard::Signal(Signal::Coming), t + 30 * S, || false), Next::Wait, "an older signal moves nothing back");
        assert_eq!(w.step(Heard::Nothing, t + 60 * S + WATCH, || false), Next::End(End::TimedOut));
    }

    #[test]
    fn a_cancel_with_the_app_running_opens_nothing() {
        let t = Instant::now();
        let mut w = Watch::new(t);
        assert_eq!(w.step(Heard::Signal(Signal::Cancelled), t + S, || true), Next::End(End::AlreadyRunning));
    }

    #[test]
    fn the_marker_says_logout_only_for_the_systems_quit_at_a_logout() {
        assert_eq!(quit_marker(true, false, false), LOGOUT_MARK, "the quit after the head start");
        assert_eq!(quit_marker(false, true, false), LOGOUT_MARK, "no early signal; the quit event says logout");
        assert_eq!(quit_marker(true, true, true), CLEAN_MARK, "the user's own Quit during a logout is never undone");
        assert_eq!(quit_marker(false, false, false), CLEAN_MARK, "any other quit");
        assert!(quit_for_logout("logout\n") && !quit_for_logout("clean\n") && !quit_for_logout(""));
    }

    /// The guardian end to end, as decisions: the exit (`marker`, `parked_logout`), what it
    /// hears while its recovery watch runs (`during`, only when it recovers), then after it
    /// (`after`). Whether it opened the app, and when (`true`: during the recovery watch).
    fn guardian(marker: Option<&str>, parked_logout: bool, during: &[Heard], after: &[Heard]) -> Option<bool> {
        let t = Instant::now();
        let plan = after_exit(marker, parked_logout);
        let mut watch = plan.listen.then(|| Watch::new(t));
        let hear = |heard: &[Heard], at: u32, watch: &mut Option<Watch>| -> bool {
            for (i, h) in heard.iter().enumerate() {
                let Some(w) = watch.as_mut() else { return false };
                match w.step(*h, t + S * (at + i as u32), || false) {
                    Next::Wait => {}
                    Next::Relaunch => {
                        *watch = None;
                        return true;
                    }
                    Next::End(_) => *watch = None,
                }
            }
            false
        };
        if plan.recover && hear(during, 1, &mut watch) {
            return Some(true);
        }
        // The recovery watch done (or none): the clean quit's wait, if still listening.
        hear(after, 100, &mut watch).then_some(false)
    }

    const CANCEL: Heard = Heard::Signal(Signal::Cancelled);
    const COMING: Heard = Heard::Signal(Signal::Coming);

    #[test]
    fn what_the_guardian_does_after_the_exit() {
        assert_eq!(after_exit(Some("clean\n"), false), AfterExit { recover: false, listen: false });
        assert_eq!(after_exit(Some("logout\n"), false), AfterExit { recover: false, listen: true });
        assert_eq!(after_exit(None, false), AfterExit { recover: true, listen: false }, "a crash, or an unclean user's Quit");
        assert_eq!(after_exit(None, true), AfterExit { recover: true, listen: true }, "an unclean quit at a logout");
        assert_eq!(after_exit(Some("clean\n"), true), AfterExit { recover: false, listen: false }, "the marker decides a clean exit");
    }

    /// Clean or unclean quit, at a logout or not, cancelled or not (round logout-cancel).
    #[test]
    fn a_cancel_reopens_after_a_clean_or_an_unclean_quit_at_a_logout() {
        let (clean, logout) = (Some("clean\n"), Some("logout\n"));
        // Clean quits.
        assert_eq!(guardian(logout, false, &[], &[CANCEL]), Some(false), "clean, logout, cancel: opened");
        assert_eq!(guardian(logout, false, &[], &[COMING, Heard::Stopped]), None, "clean, logout, it goes on");
        assert_eq!(guardian(clean, false, &[], &[CANCEL]), None, "clean, no logout: nothing listens");
        // Unclean quits at a logout: a cancel during the recovery watch opens at once; one
        // after it, as after a clean quit.
        assert_eq!(guardian(None, true, &[CANCEL], &[]), Some(true), "unclean, logout, cancel during the watch");
        assert_eq!(guardian(None, true, &[Heard::Nothing, COMING], &[CANCEL]), Some(false), "unclean, logout, cancel after the watch");
        assert_eq!(guardian(None, true, &[COMING], &[Heard::Stopped, CANCEL]), None, "unclean, logout, it goes on");
        assert_eq!(guardian(None, true, &[Heard::Stopped], &[CANCEL]), None, "the session ends during the watch: nothing after");
        // Unclean, no logout (a crash, the user's Quit): recovery only, a cancel is nobody's.
        assert_eq!(guardian(None, false, &[CANCEL], &[CANCEL]), None, "unclean, no logout");
    }

    #[test]
    fn quit_reasons() {
        let code = |s: &[u8; 4]| u32::from_be_bytes(*s);
        for r in LOGOUT_REASONS {
            assert!(logout_reason(u32::from_be_bytes(r)));
        }
        assert!(!logout_reason(0) && !logout_reason(code(b"quit")));
        assert_eq!(four_cc(code(b"rlgo")), "rlgo");
        assert_eq!(four_cc(0), "none");
        assert_eq!(four_cc(0x0102_0304), "0x01020304");
    }
}
