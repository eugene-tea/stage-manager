//! One Stage Manager at a time (the review of 1.1.0, Fable M1 and Grok High 1): two managers
//! would build models from the same windows and park each other's shown stage.
//!
//! The claim is made at launch, before the WM thread, the guardian or any observer starts
//! ([`claim`]): an exclusive `flock` on a lock file in the config directory
//! ([`paths::lock_file`], not `$TMPDIR`, which macOS prunes), holding its owner's pid, held
//! for the life of the process (the kernel lets go of it at any exit, a crash included; the
//! descriptor is close-on-exec, so the guardian never holds it), and a `ping` on the socket
//! (and at an earlier version's place, [`paths::legacy_socket_path`]) for a copy that takes
//! no lock. The socket is then bound before anything is managed; failing to bind it ends the
//! launch too. What a launch does is decided by [`decide`]:
//!
//! - a copy answers `ping`: hand off (it shows its Settings, unless it started moments ago:
//!   two launches at login, [`HANDOFF_QUIET`]) and leave;
//! - a copy answers that it is quitting (bringing its windows back): wait for as long as it
//!   takes, then manage (a relaunch during a quit must not leave nothing running);
//! - the lock is held by a process that does not answer: wait while it lives if it answered
//!   before (a copy quitting removes its socket last, right before it exits); one that never
//!   answered at all (starting and not bound yet, or hung) is waited for [`LOCK_WAIT`], then
//!   left alone;
//! - otherwise (the lock is ours, or no lock can be had at all and nothing answers): manage.
//!
//! An error reply to `ping` is not an answer.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{ctl, paths};

/// How long a launch waits for a copy holding the lock that never answered (starting, hung).
pub const LOCK_WAIT: Duration = Duration::from_secs(5);

/// How often the lock is tried again while waiting.
const LOCK_RETRY: Duration = Duration::from_millis(100);

/// How long `ping` may take: a running copy slow to answer must not get a second manager
/// beside it (no copy running answers "refused" at once).
const PING_TIMEOUT: Duration = Duration::from_secs(2);

/// A running copy that started less than this long ago is not asked to show its Settings on a
/// hand-off: two launches at login (the login item and another launcher) race, and neither is
/// the user asking for the window.
pub const HANDOFF_QUIET: Duration = Duration::from_secs(10);

/// The lock held for the life of the process.
static LOCK: OnceLock<File> = OnceLock::new();

/// The lock file ([`paths::lock_file`]).
pub fn lock_path() -> PathBuf {
    paths::lock_file()
}

/// What trying the lock found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    /// Ours now.
    Held,
    /// Another process holds it.
    Busy,
    /// The file cannot be opened or locked (a read-only directory, say): the socket decides.
    Unavailable,
}

/// What a `ping` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    /// A copy runs and answers (`ok`, not quitting).
    Answers,
    /// A copy answers that it is quitting.
    Quitting,
    /// Nothing answers, or the reply is an error or unreadable.
    Silent,
}

/// A reply to `ping`, read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    pub peer: Peer,
    /// The answering copy's pid.
    pub pid: Option<u32>,
    /// How long ago the answering copy started (earlier versions do not say).
    pub uptime: Option<Duration>,
}

impl Pong {
    /// No answer.
    pub const SILENT: Pong = Pong { peer: Peer::Silent, pid: None, uptime: None };

    /// Reads a `ping` reply: `"ok": true` answers (or quits, with `"quitting": true`); an
    /// error reply or anything else is [`Peer::Silent`].
    pub fn parse(reply: &str) -> Pong {
        let Ok(v) = serde_json::from_str::<Value>(reply) else { return Self::SILENT };
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            return Self::SILENT;
        }
        let quitting = v.get("quitting").and_then(Value::as_bool) == Some(true);
        Pong {
            peer: if quitting { Peer::Quitting } else { Peer::Answers },
            pid: v.get("pid").and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok()),
            uptime: v.get("uptime_ms").and_then(Value::as_u64).map(Duration::from_millis),
        }
    }
}

/// What is known of the lock's holder while the lock is busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Holder {
    /// A copy answered `ping` during this claim (it was quitting).
    pub answered: bool,
    /// Its pid (in the lock file, else from a `ping`) names a live process.
    pub alive: bool,
}

/// What a launch does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// Manage windows: this is the one copy.
    Manage,
    /// Another copy answers: ask it to show its Settings, then leave.
    HandOff,
    /// Another copy is quitting, or holds the lock without answering: try again shortly.
    Wait,
    /// Another copy held the lock without ever answering for all of [`LOCK_WAIT`]: leave.
    Leave,
}

/// The launch decision (module docs): `lock` as tried, what `ping` found, what is known of
/// the lock's holder, and whether [`LOCK_WAIT`] is over.
pub fn decide(lock: Lock, peer: Peer, holder: Holder, waited_out: bool) -> Launch {
    match (lock, peer) {
        (_, Peer::Answers) => Launch::HandOff,
        (_, Peer::Quitting) => Launch::Wait,
        (Lock::Busy, Peer::Silent) if holder.answered && holder.alive => Launch::Wait,
        (Lock::Busy, Peer::Silent) if waited_out => Launch::Leave,
        (Lock::Busy, Peer::Silent) => Launch::Wait,
        (Lock::Held | Lock::Unavailable, Peer::Silent) => Launch::Manage,
    }
}

/// Whether a hand-off asks the running copy to show its Settings: not when it started less
/// than [`HANDOFF_QUIET`] ago (an earlier version, which does not say, is asked).
pub fn shows_settings(uptime: Option<Duration>) -> bool {
    uptime.is_none_or(|u| u >= HANDOFF_QUIET)
}

fn try_lock() -> (Lock, Option<File>) {
    let path = lock_path();
    if let Some(dir) = path.parent() {
        // Private from the first launch on (the socket and state.json live here too).
        let _ = crate::private_files::ensure_dir(dir);
    }
    let file = match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("cannot open the lock file {}: {e}; the ctl socket alone keeps a second copy out", path.display());
            return (Lock::Unavailable, None);
        }
    };
    // SAFETY: flock on a descriptor `file` owns for the duration of the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return (Lock::Held, Some(file));
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        (Lock::Busy, None)
    } else {
        tracing::warn!("cannot lock {}: {err}; the ctl socket alone keeps a second copy out", path.display());
        (Lock::Unavailable, None)
    }
}

/// Writes our pid into the lock we hold, for a launch waiting on it.
fn write_pid(file: &mut File) {
    let written = file.set_len(0).and_then(|()| file.seek(SeekFrom::Start(0))).and_then(|_| writeln!(file, "{}", std::process::id()));
    if let Err(e) = written {
        tracing::debug!("cannot write our pid into the lock file: {e}");
    }
}

/// The pid the lock's holder wrote.
fn holder_pid() -> Option<u32> {
    std::fs::read_to_string(lock_path()).ok().and_then(|s| s.trim().parse().ok())
}

/// Whether `pid` names a live process other than this one.
fn alive(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else { return false };
    if raw <= 0 || pid == std::process::id() {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists.
    let r = unsafe { libc::kill(raw, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// `ping` on the socket, then at an earlier version's place: the first answer, with the path
/// it came from.
fn ping() -> (Pong, PathBuf) {
    let mut places = vec![paths::socket_path()];
    let legacy = paths::legacy_socket_path();
    if legacy != places[0] {
        places.push(legacy);
    }
    for path in &places {
        let pong = ctl::request_at(path, "ping", PING_TIMEOUT).map_or(Pong::SILENT, |r| Pong::parse(&r));
        if pong.peer != Peer::Silent {
            return (pong, path.clone());
        }
    }
    (Pong::SILENT, places.swap_remove(0))
}

/// Claims the single instance (module docs). `Manage` with the lock kept for the process's
/// life; `HandOff` after asking the running copy to show its Settings; `Leave` when a copy
/// holds the lock and never answered. Runs before anything is managed.
pub fn claim() -> Launch {
    let start = Instant::now();
    let mut answered = false;
    let mut ping_pid: Option<u32> = None;
    let mut said: Option<Peer> = None;
    loop {
        let (lock, file) = try_lock();
        let (pong, place) = ping();
        answered |= pong.peer != Peer::Silent;
        ping_pid = pong.pid.or(ping_pid);
        let holder = match lock {
            Lock::Busy => Holder { answered, alive: holder_pid().or(ping_pid).is_some_and(alive) },
            Lock::Held | Lock::Unavailable => Holder::default(),
        };
        let launch = decide(lock, pong.peer, holder, start.elapsed() >= LOCK_WAIT);
        match launch {
            Launch::Manage => {
                if let Some(mut f) = file {
                    write_pid(&mut f);
                    let _ = LOCK.set(f);
                }
                if answered {
                    tracing::info!(waited_ms = start.elapsed().as_millis() as u64, "the Stage Manager that was quitting is gone; this one manages");
                }
                return launch;
            }
            Launch::HandOff => {
                // Opened again (`open -n`, a terminal): the running one shows its Settings, the
                // way back when the menu bar icon is hidden. (A Finder or Spotlight launch
                // never gets here: macOS hands it to the running app as a reopen.)
                if shows_settings(pong.uptime) {
                    let shown = ctl::request_at(&place, "settings", PING_TIMEOUT).is_ok();
                    tracing::info!(shown, "Stage Manager is already running; asked it to show Settings");
                } else {
                    tracing::info!(
                        uptime_ms = pong.uptime.map(|u| u.as_millis() as u64),
                        "Stage Manager is already running and started moments ago (two launches at login); leaving without showing its Settings"
                    );
                }
                return launch;
            }
            Launch::Wait => {
                if said != Some(pong.peer) {
                    match pong.peer {
                        Peer::Quitting => tracing::info!(pid = pong.pid, "another Stage Manager is quitting; waiting until it is gone"),
                        _ => tracing::info!("another Stage Manager holds the lock and does not answer (quitting or starting); waiting"),
                    }
                    said = Some(pong.peer);
                }
                std::thread::sleep(LOCK_RETRY);
            }
            Launch::Leave => {
                tracing::error!(
                    lock = %lock_path().display(),
                    "another Stage Manager holds the lock but does not answer; not starting a second one (quit it in Activity Monitor if it hangs)"
                );
                return launch;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: Holder = Holder { answered: true, alive: true };
    const SILENT_LIVE: Holder = Holder { answered: false, alive: true };
    const GONE: Holder = Holder { answered: true, alive: false };

    #[test]
    fn one_copy_manages() {
        assert_eq!(decide(Lock::Held, Peer::Silent, Holder::default(), false), Launch::Manage, "the lock is ours, nothing answers");
        assert_eq!(decide(Lock::Unavailable, Peer::Silent, Holder::default(), false), Launch::Manage, "no lock to be had: the socket decides");
    }

    #[test]
    fn a_copy_that_answers_gets_the_hand_off_lock_or_not() {
        for lock in [Lock::Held, Lock::Busy, Lock::Unavailable] {
            assert_eq!(decide(lock, Peer::Answers, Holder::default(), false), Launch::HandOff, "{lock:?}");
        }
        // An earlier version takes no lock: ours, but it answers.
        assert_eq!(decide(Lock::Held, Peer::Answers, Holder::default(), true), Launch::HandOff);
    }

    /// The review of 8A, M1: a relaunch while a copy quits waits for it (its quit can take
    /// longer than [`LOCK_WAIT`] with many windows), then manages; it never hands off to a
    /// copy about to exit.
    #[test]
    fn a_quitting_copy_is_waited_for_then_this_one_manages() {
        for lock in [Lock::Held, Lock::Busy, Lock::Unavailable] {
            assert_eq!(decide(lock, Peer::Quitting, Holder::default(), true), Launch::Wait, "{lock:?}, past the bound");
        }
        // It removed its socket and still holds the lock: waited for while it lives.
        assert_eq!(decide(Lock::Busy, Peer::Silent, LIVE, true), Launch::Wait);
        // Gone: the lock is free.
        assert_eq!(decide(Lock::Held, Peer::Silent, Holder::default(), true), Launch::Manage);
    }

    /// Fable M1: a running copy slow to answer (stopped under a debugger, beach-balling, its
    /// abort handler stuck) never gets a second manager beside it; one that never answered is
    /// waited for only [`LOCK_WAIT`].
    #[test]
    fn a_silent_lock_holder_is_waited_for_then_left_alone() {
        assert_eq!(decide(Lock::Busy, Peer::Silent, SILENT_LIVE, false), Launch::Wait);
        assert_eq!(decide(Lock::Busy, Peer::Silent, SILENT_LIVE, true), Launch::Leave, "never answered: bounded");
        assert_eq!(decide(Lock::Busy, Peer::Silent, GONE, true), Launch::Leave, "its pid is gone, the lock still busy");
    }

    #[test]
    fn ping_replies_are_read_and_errors_are_not_answers() {
        let p = Pong::parse(r#"{"ok":true,"pong":true,"pid":412,"uptime_ms":30000}"#);
        assert_eq!(p, Pong { peer: Peer::Answers, pid: Some(412), uptime: Some(Duration::from_secs(30)) });
        assert_eq!(Pong::parse(r#"{"ok":true,"pid":412,"quitting":true}"#).peer, Peer::Quitting);
        assert_eq!(Pong::parse(r#"{"ok":true,"quitting":false}"#).peer, Peer::Answers);
        assert_eq!(Pong::parse(r#"{"ok":true,"pong":true}"#), Pong { peer: Peer::Answers, pid: None, uptime: None }, "an earlier version");
        assert_eq!(Pong::parse(r#"{"ok":false,"error":"not allowed"}"#), Pong::SILENT);
        assert_eq!(Pong::parse(r#"{"pong":true}"#), Pong::SILENT);
        assert_eq!(Pong::parse("garbage"), Pong::SILENT);
    }

    /// The review of 8A, L2: two launches at login do not open the Settings.
    #[test]
    fn a_copy_started_moments_ago_shows_no_settings_on_a_hand_off() {
        assert!(!shows_settings(Some(Duration::from_secs(3))));
        assert!(shows_settings(Some(HANDOFF_QUIET)));
        assert!(shows_settings(Some(Duration::from_secs(3600))));
        assert!(shows_settings(None), "an earlier version does not say");
    }

    #[test]
    fn this_process_is_never_the_holder() {
        assert!(!alive(std::process::id()));
        assert!(!alive(0));
        // SAFETY: getppid has no preconditions.
        let parent = unsafe { libc::getppid() };
        assert!(alive(parent as u32), "the test runner's parent lives");
    }

    #[test]
    fn the_lock_file_sits_in_the_config_dir() {
        assert_eq!(lock_path(), paths::config_dir().join("stage-manager.lock"));
        assert!(!lock_path().starts_with(paths::user_temp_dir()) || paths::config_dir().starts_with(paths::user_temp_dir()));
    }
}
