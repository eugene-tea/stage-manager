//! One Stage Manager at a time (the review of 1.1.0, Fable M1 and Grok High 1): two managers
//! would build models from the same windows and park each other's shown stage.
//!
//! The claim is made at launch, before the WM thread, the guardian or any observer starts
//! ([`claim`]): an exclusive `flock` on a lock file beside the ctl socket
//! ([`lock_path`]), held for the life of the process (the kernel lets go of it at any exit,
//! a crash included; the descriptor is close-on-exec, so the guardian never holds it), and a
//! `ping` on the socket for a copy that takes no lock (an earlier version). The socket is
//! then bound before anything is managed; failing to bind it ends the launch too. What a
//! launch does is decided by [`decide`]:
//!
//! - a copy answers `ping`: hand off (it shows its Settings) and leave;
//! - the lock is held by another process that does not answer: wait a moment (a copy quitting
//!   has removed its socket and still unparks; one starting has not bound it yet), then leave;
//! - otherwise (the lock is ours, or no lock can be had at all and nothing answers): manage.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::{ctl, paths};

/// How long a launch waits for a copy holding the lock without answering (quitting, starting).
pub const LOCK_WAIT: Duration = Duration::from_secs(5);

/// How often the lock is tried again while waiting.
const LOCK_RETRY: Duration = Duration::from_millis(100);

/// How long `ping` may take: a running copy slow to answer must not get a second manager
/// beside it (no copy running answers "refused" at once).
const PING_TIMEOUT: Duration = Duration::from_secs(2);

/// The lock held for the life of the process.
static LOCK: OnceLock<File> = OnceLock::new();

/// The lock file: beside the ctl socket (`stage-manager.lock`; per `STAGE_MANAGER_HOME`).
pub fn lock_path() -> PathBuf {
    paths::socket_path().with_extension("lock")
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

/// What a launch does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// Manage windows: this is the one copy.
    Manage,
    /// Another copy answers: ask it to show its Settings, then leave.
    HandOff,
    /// Another copy holds the lock without answering: try again shortly.
    Wait,
    /// Another copy held the lock without answering for all of [`LOCK_WAIT`]: leave.
    Leave,
}

/// The launch decision (module docs): `lock` as tried, whether a copy `answers` `ping`, and
/// whether the wait for a silent lock holder is over.
pub fn decide(lock: Lock, answers: bool, waited_out: bool) -> Launch {
    match (lock, answers) {
        (_, true) => Launch::HandOff,
        (Lock::Busy, false) if waited_out => Launch::Leave,
        (Lock::Busy, false) => Launch::Wait,
        (Lock::Held | Lock::Unavailable, false) => Launch::Manage,
    }
}

fn try_lock() -> (Lock, Option<File>) {
    let path = lock_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
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

/// Claims the single instance (module docs). `Manage` with the lock kept for the process's
/// life; `HandOff` after asking the running copy to show its Settings; `Leave` when a copy
/// holds the lock and never answered. Runs before anything is managed.
pub fn claim() -> Launch {
    let start = Instant::now();
    let mut said_wait = false;
    loop {
        let (lock, file) = try_lock();
        let answers = ctl::request("ping", PING_TIMEOUT).is_ok();
        let launch = decide(lock, answers, start.elapsed() >= LOCK_WAIT);
        match launch {
            Launch::Manage => {
                if let Some(f) = file {
                    let _ = LOCK.set(f);
                }
                return launch;
            }
            Launch::HandOff => {
                // Opened again (`open -n`, a terminal): the running one shows its Settings, the
                // way back when the menu bar icon is hidden. (A Finder or Spotlight launch
                // never gets here: macOS hands it to the running app as a reopen.)
                let shown = ctl::request("settings", PING_TIMEOUT).is_ok();
                tracing::info!(shown, "Stage Manager is already running; asked it to show Settings");
                return launch;
            }
            Launch::Wait => {
                if !said_wait {
                    tracing::info!("another Stage Manager holds the lock and does not answer yet (quitting or starting); waiting");
                    said_wait = true;
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

    #[test]
    fn one_copy_manages() {
        assert_eq!(decide(Lock::Held, false, false), Launch::Manage, "the lock is ours, nothing answers");
        assert_eq!(decide(Lock::Unavailable, false, false), Launch::Manage, "no lock to be had: the socket decides");
    }

    #[test]
    fn a_copy_that_answers_gets_the_hand_off_lock_or_not() {
        for lock in [Lock::Held, Lock::Busy, Lock::Unavailable] {
            assert_eq!(decide(lock, true, false), Launch::HandOff, "{lock:?}");
        }
        // An earlier version takes no lock: ours, but it answers.
        assert_eq!(decide(Lock::Held, true, true), Launch::HandOff);
    }

    /// Fable M1: a running copy slow to answer (stopped under a debugger, beach-balling, its
    /// abort handler stuck) never gets a second manager beside it.
    #[test]
    fn a_silent_lock_holder_is_waited_for_then_left_alone() {
        assert_eq!(decide(Lock::Busy, false, false), Launch::Wait);
        assert_eq!(decide(Lock::Busy, false, true), Launch::Leave);
    }

    #[test]
    fn the_lock_file_sits_beside_the_socket() {
        let lock = lock_path();
        assert_eq!(lock.parent(), paths::socket_path().parent());
        assert_eq!(lock.extension().and_then(|e| e.to_str()), Some("lock"));
    }
}
