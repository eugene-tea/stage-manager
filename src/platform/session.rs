//! Can the app see the Desktop at all? While the screen is locked (or the session is not on
//! the console: another user's, fast user switching) Accessibility lists no window for any
//! app, and a Desktop recognized then would be an empty one: stages built from it, saved
//! Desktops pruned, the windows that show after the unlock filed as new. Nothing is
//! recognized, built or saved while the app is blind ([`blindness`]); the controller waits
//! for the unlock (polled: [`screen_locked`] is cheap) and looks again.
//!
//! Two signs, either enough:
//! - the login session says so: `CGSessionCopyCurrentDictionary`'s
//!   `CGSSessionScreenIsLocked` set, or `kCGSSessionOnConsoleKey` false;
//! - Accessibility lists none of the windows the window server shows on screen for the apps
//!   we observe, while it shows at least [`BLIND_MIN_WINDOWS`] real ones (an empty Desktop,
//!   or one with a stray helper window, is never taken for blindness). At an arrival, after
//!   the launch, only windows the model already knows count ([`arrival_silent`]).
//!
//! [`window_session`] names the window server session (boot and login) for `state.json`:
//! window ids saved in another session are not trusted, and a file from another session
//! starts a restore. Tests simulate a new session with `STAGE_MANAGER_SESSION=<text>`,
//! honoured only with `STAGE_MANAGER_TEST` set.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFString, CFType};
use objc2_core_graphics::CGSessionCopyCurrentDictionary;

use super::windows::CgWindow;

/// The window server must show at least this many real windows of observed apps on screen,
/// Accessibility none of them, for Accessibility to count as blind.
pub const BLIND_MIN_WINDOWS: usize = 3;

/// A window smaller than this (either side, points) is not counted as a real window: apps
/// keep tiny or invisible helper windows at layer 0 that Accessibility never lists.
const REAL_WINDOW_MIN: f64 = 50.0;

/// Why the app cannot see the Desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blind {
    /// The screen is locked, or the login session is not on the console.
    Locked,
    /// Accessibility lists none of the windows the window server shows.
    AxSilent,
}

impl Blind {
    /// Its name for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::Locked => "the screen is locked",
            Self::AxSilent => "Accessibility lists none of the windows on screen",
        }
    }
}

/// The verdict from what is known: `locked` ([`screen_locked`]), the real windows the window
/// server shows on screen for observed apps (`server`), and how many of them Accessibility
/// lists (`ax`).
pub fn blindness(locked: bool, server: usize, ax: usize) -> Option<Blind> {
    if locked {
        Some(Blind::Locked)
    } else if ax == 0 && server >= BLIND_MIN_WINDOWS {
        Some(Blind::AxSilent)
    } else {
        None
    }
}

/// At an arrival (the screen unlocked; [`screen_locked`] is asked first): Accessibility is
/// silent only when the window server shows windows the model already knows (`known`, counted
/// by [`counts`]) and Accessibility lists none of them (`listed`). Windows the model has
/// never seen say nothing: an app whose windows Accessibility does not list would otherwise
/// hold its Desktop for the whole wait at every visit.
pub fn arrival_silent(known: usize, listed: usize) -> bool {
    known > 0 && listed == 0
}

/// What a launch does about building its stages ([`wait_step`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The Desktop can be seen: build.
    Build,
    /// Wait (blind since this instant; nothing built, nothing saved).
    Wait(Instant),
    /// Accessibility has listed nothing for `max_silent`: build from what it lists.
    GiveUp,
}

/// The launch's step at `now`: `blind` as [`blindness`] says now, `waiting` the blindness
/// waited on so far and since when. A locked screen is waited out however long it lasts;
/// Accessibility listing nothing at most `max_silent` (it may simply not list some app's
/// windows). A new reason starts the wait afresh.
pub fn wait_step(blind: Option<Blind>, waiting: Option<(Blind, Instant)>, now: Instant, max_silent: Duration) -> Step {
    let Some(blind) = blind else { return Step::Build };
    let since = match waiting {
        Some((was, t)) if was == blind => t,
        _ => now,
    };
    if blind == Blind::AxSilent && now.duration_since(since) >= max_silent {
        Step::GiveUp
    } else {
        Step::Wait(since)
    }
}

/// The real windows on screen in `list` that `include` takes (at launch: those of a process
/// we observe, not ours; at an arrival: those the model knows too): `(server, ax)` for
/// [`blindness`] or [`arrival_silent`], `ax` counting those `tracked` says Accessibility
/// lists.
pub fn counts(list: &[CgWindow], include: impl Fn(&CgWindow) -> bool, tracked: impl Fn(u32) -> bool) -> (usize, usize) {
    let real: Vec<&CgWindow> = list
        .iter()
        .filter(|w| w.onscreen && w.layer == 0 && w.alpha > 0.0)
        .filter(|w| w.bounds.w >= REAL_WINDOW_MIN && w.bounds.h >= REAL_WINDOW_MIN)
        .filter(|w| include(w))
        .collect();
    let ax = real.iter().filter(|w| tracked(w.id)).count();
    (real.len(), ax)
}

/// True while the screen is locked or this login session is not the one on the console.
/// `false` when the session cannot be read (never blocks the app on an unknown).
pub fn screen_locked() -> bool {
    let Some(dict) = CGSessionCopyCurrentDictionary() else {
        return false;
    };
    // SAFETY: the session dictionary maps CFString keys to CF values (checked below).
    let dict: &CFDictionary<CFString, CFType> = unsafe { &*(&*dict as *const CFDictionary).cast() };
    let flag = |key: &'static str| -> Option<bool> {
        let value = dict.get(&CFString::from_static_str(key))?;
        if let Some(b) = value.downcast_ref::<CFBoolean>() {
            return Some(b.as_bool());
        }
        value.downcast_ref::<CFNumber>().and_then(|n| n.as_i64()).map(|n| n != 0)
    };
    flag("CGSSessionScreenIsLocked").unwrap_or(false) || flag("kCGSSessionOnConsoleKey") == Some(false)
}

/// The window server session this process runs in, as a string kept in `state.json`
/// ([`crate::engine::persist::StateFile::session`]): the boot (`kern.bootsessionuuid`, stable
/// for the boot; `kern.boottime` in whole seconds when it cannot be read: macOS moves the
/// boot time when the clock is set) and the login session (`CGSessionCopyCurrentDictionary`'s
/// `kCGSSessionIDKey` and `kCGSSessionAuditIDKey`). Window ids are only unique within one: after a reboot or a new
/// login they start over. Read once (it cannot change while the process lives); `None` when
/// neither the boot time nor the session can be read.
pub fn window_session() -> Option<&'static str> {
    static SESSION: OnceLock<Option<String>> = OnceLock::new();
    SESSION.get_or_init(read_window_session).as_deref()
}

/// The test-only session override: `STAGE_MANAGER_SESSION`, honoured only with
/// `STAGE_MANAGER_TEST` set (a restart simulated without restarting the Mac).
fn session_override(test: Option<std::ffi::OsString>, session: Option<String>) -> Option<String> {
    test?;
    session.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn read_window_session() -> Option<String> {
    if let Some(s) = session_override(std::env::var_os("STAGE_MANAGER_TEST"), std::env::var("STAGE_MANAGER_SESSION").ok()) {
        tracing::info!(session = %s, "window server session from STAGE_MANAGER_SESSION (test)");
        return Some(s);
    }
    let boot = boot_uuid().or_else(|| boot_time().map(|s| format!("t{s}")));
    let ids = CGSessionCopyCurrentDictionary().map(|dict| {
        // SAFETY: the session dictionary maps CFString keys to CF values (checked below).
        let dict: &CFDictionary<CFString, CFType> = unsafe { &*(&*dict as *const CFDictionary).cast() };
        let value = |key: &'static str| dict.get(&CFString::from_static_str(key));
        let number = |key: &'static str| -> Option<i64> { value(key)?.downcast_ref::<CFNumber>().and_then(|n| n.as_i64()) };
        // The login session: its UUID where macOS lists one (seen on macOS 26, which lists no
        // kCGSSessionIDKey), else the session id.
        let id = value("CGSSessionUniqueSessionUUID")
            .and_then(|v| v.downcast_ref::<CFString>().map(|s| s.to_string()))
            .or_else(|| number("kCGSSessionIDKey").map(|n| n.to_string()));
        (id, number("kCGSSessionAuditIDKey").or_else(|| number("kSCSecuritySessionID")))
    });
    let (id, audit) = ids.unwrap_or((None, None));
    let key = session_key(boot.as_deref(), id.as_deref(), audit);
    match &key {
        Some(k) if !k.contains('?') => tracing::debug!(session = %k, "window server session"),
        _ => tracing::warn!(session = ?key, "window server session only partly readable: saved window ids may be dropped at the next launch"),
    }
    key
}

/// The session string from its parts; `None` when the boot time and the session id are
/// both unknown (nothing to tell one session from another).
pub fn session_key(boot: Option<&str>, id: Option<&str>, audit: Option<i64>) -> Option<String> {
    if boot.is_none() && id.is_none() {
        return None;
    }
    let audit = audit.map_or_else(|| "?".to_string(), |v| v.to_string());
    Some(format!("boot {} session {} audit {audit}", boot.unwrap_or("?"), id.unwrap_or("?")))
}

/// `kern.bootsessionuuid`: a UUID for this boot.
fn boot_uuid() -> Option<String> {
    let mut buf = [0u8; 64];
    let mut size = buf.len();
    // SAFETY: the sysctl writes a NUL-terminated string into our buffer of `size` bytes.
    let rc = unsafe {
        libc::sysctlbyname(c"kern.bootsessionuuid".as_ptr(), buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0)
    };
    if rc != 0 || size == 0 {
        return None;
    }
    let s = std::str::from_utf8(&buf[..size]).ok()?.trim_end_matches('\0').trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// `kern.boottime` in whole seconds.
fn boot_time() -> Option<i64> {
    let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
    let mut size = std::mem::size_of::<libc::timeval>();
    // SAFETY: `kern.boottime` is a `struct timeval`; the buffer and its size are ours.
    let rc = unsafe {
        libc::sysctlbyname(c"kern.boottime".as_ptr(), (&raw mut tv).cast(), &mut size, std::ptr::null_mut(), 0)
    };
    (rc == 0 && size == std::mem::size_of::<libc::timeval>() && tv.tv_sec > 0).then_some(tv.tv_sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Rect;

    fn win(id: u32, pid: i32, w: f64, onscreen: bool) -> CgWindow {
        CgWindow {
            id,
            pid,
            owner: String::new(),
            name: None,
            layer: 0,
            bounds: Rect::new(100.0, 100.0, w, w),
            onscreen,
            alpha: 1.0,
        }
    }

    #[test]
    fn a_locked_screen_is_blind_whatever_accessibility_says() {
        assert_eq!(blindness(true, 0, 0), Some(Blind::Locked));
        assert_eq!(blindness(true, 18, 18), Some(Blind::Locked));
    }

    #[test]
    fn accessibility_listing_nothing_while_windows_show_is_blind() {
        // The 10-13 launches: the window server showed his windows, Accessibility none.
        assert_eq!(blindness(false, 7, 0), Some(Blind::AxSilent));
        assert_eq!(blindness(false, BLIND_MIN_WINDOWS, 0), Some(Blind::AxSilent));
        // One window listed: Accessibility works.
        assert_eq!(blindness(false, 7, 1), None);
    }

    #[test]
    fn an_empty_desktop_or_a_stray_window_is_not_blindness() {
        assert_eq!(blindness(false, 0, 0), None);
        assert_eq!(blindness(false, BLIND_MIN_WINDOWS - 1, 0), None);
    }

    #[test]
    fn counts_take_real_windows_on_screen_of_observed_apps_only() {
        let list = vec![
            win(1, 10, 800.0, true),
            win(2, 10, 800.0, true),
            win(3, 20, 800.0, true),
            win(4, 10, 800.0, false), // another Desktop
            win(5, 10, 10.0, true),   // a helper window
            win(6, 99, 800.0, true),  // an app we do not observe (STAGE_MANAGER_ONLY, ours)
        ];
        let observed = |w: &CgWindow| w.pid == 10 || w.pid == 20;
        assert_eq!(counts(&list, observed, |_| false), (3, 0));
        assert_eq!(counts(&list, observed, |id| id == 3), (3, 1));
        let (server, ax) = counts(&list, observed, |_| false);
        assert_eq!(blindness(false, server, ax), Some(Blind::AxSilent));
    }

    #[test]
    fn an_arrival_is_held_for_silence_only_over_windows_the_model_knows() {
        let list = vec![
            win(1, 10, 800.0, true), // known
            win(2, 10, 800.0, true), // known
            win(3, 20, 800.0, true), // never seen: an app whose windows Accessibility does not list
            win(4, 20, 800.0, true),
            win(5, 20, 800.0, true),
        ];
        let known = |w: &CgWindow| w.id <= 2;
        // Accessibility lists the known windows, not the app it never listed: no wait (the
        // whole-screen count would have held this Desktop 10 s at every visit).
        let (k, l) = counts(&list, known, |id| id <= 2);
        assert!(!arrival_silent(k, l));
        // It lists none of the known ones (just unlocked, still waking): wait.
        let (k, l) = counts(&list, known, |_| false);
        assert_eq!((k, l), (2, 0));
        assert!(arrival_silent(k, l));
        // A Desktop of windows never seen: never held.
        let (k, l) = counts(&list, |_| false, |_| false);
        assert!(!arrival_silent(k, l));
        // One known window listed is enough.
        assert!(!arrival_silent(2, 1));
    }

    #[test]
    fn a_launch_waits_out_a_locked_screen_however_long_and_silence_for_a_while() {
        let t0 = Instant::now();
        let max = Duration::from_secs(10);
        assert_eq!(wait_step(None, None, t0, max), Step::Build);
        assert_eq!(wait_step(Some(Blind::Locked), None, t0, max), Step::Wait(t0));
        let hour = t0 + Duration::from_secs(3600);
        assert_eq!(wait_step(Some(Blind::Locked), Some((Blind::Locked, t0)), hour, max), Step::Wait(t0), "locked: never given up");
        assert_eq!(wait_step(None, Some((Blind::Locked, t0)), hour, max), Step::Build, "unlocked: build");
        let t9 = t0 + Duration::from_secs(9);
        assert_eq!(wait_step(Some(Blind::AxSilent), Some((Blind::AxSilent, t0)), t9, max), Step::Wait(t0));
        assert_eq!(wait_step(Some(Blind::AxSilent), Some((Blind::AxSilent, t0)), t0 + max, max), Step::GiveUp);
        // Unlocked, then Accessibility still silent: its own wait starts at the unlock.
        assert_eq!(wait_step(Some(Blind::AxSilent), Some((Blind::Locked, t0)), hour, max), Step::Wait(hour));
    }

    #[test]
    fn reading_the_real_session_does_not_panic() {
        let _ = screen_locked();
        assert_eq!(window_session(), window_session(), "read once");
    }

    #[test]
    fn the_session_override_needs_the_test_switch() {
        let on = Some(std::ffi::OsString::from("1"));
        assert_eq!(session_override(on.clone(), Some("restore B".into())), Some("restore B".to_string()));
        assert_eq!(session_override(None, Some("restore B".into())), None, "not outside tests");
        assert_eq!(session_override(on.clone(), Some("  ".into())), None);
        assert_eq!(session_override(on, None), None);
    }

    #[test]
    fn the_session_key_tells_boots_and_logins_apart() {
        let a = session_key(Some("2EA11F65-D66D-496B-94D1-84260EF970A2"), Some("257"), Some(100_001));
        assert_eq!(a.as_deref(), Some("boot 2EA11F65-D66D-496B-94D1-84260EF970A2 session 257 audit 100001"));
        assert_ne!(a, session_key(Some("7C1B0A11-0000-4000-8000-000000000001"), Some("257"), Some(100_001)), "a reboot");
        assert_ne!(a, session_key(Some("2EA11F65-D66D-496B-94D1-84260EF970A2"), Some("257"), Some(100_007)), "a new login");
        assert_eq!(session_key(None, None, Some(1)), None, "nothing to tell sessions apart");
        assert!(session_key(Some("t1"), None, None).is_some());
    }
}
