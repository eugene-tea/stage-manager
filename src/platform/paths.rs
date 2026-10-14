//! Well-known file locations: ctl socket, log file, config and state files, clean-exit
//! markers, the parked file for the guardian.
//!
//! `STAGE_MANAGER_HOME=<dir>` moves all of them into `<dir>` (test runs use a scratch
//! directory, so they never touch the real config, state, socket or log). Without it:
//! config and state in `~/.config/stage-manager`, the log in `~/Library/Logs/Stage Manager`,
//! the socket in the per-user temporary directory. `STAGE_MANAGER_LOG_DIR=<dir>` moves only
//! the log.

use std::ffi::{CStr, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::config::{self, HOME_ENV};

/// `sun_path` holds 104 bytes on macOS; longer socket paths are hashed into the temp dir.
const MAX_SOCKET_PATH: usize = 100;

/// The per-user temporary directory (`confstr(_CS_DARWIN_USER_TEMP_DIR)`, which is what
/// `$TMPDIR` normally points at). Using `confstr` first keeps the app (launched through
/// LaunchServices) and the `ctl` client (launched from any shell) on the same path.
pub fn user_temp_dir() -> PathBuf {
    let mut buf = [0 as libc::c_char; 1024];
    // SAFETY: `buf` is a valid writable buffer of the length we pass.
    let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr(), buf.len()) };
    if n > 0 && n <= buf.len() {
        // SAFETY: confstr wrote a NUL-terminated string that fits in `buf`.
        let s = unsafe { CStr::from_ptr(buf.as_ptr()) };
        if let Ok(s) = s.to_str() {
            return PathBuf::from(s);
        }
    }
    std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `$STAGE_MANAGER_HOME`, when set and not empty.
pub fn custom_home() -> Option<PathBuf> {
    std::env::var_os(HOME_ENV).filter(|d| !d.is_empty()).map(PathBuf::from)
}

/// The ctl socket: `$STAGE_MANAGER_HOME/stage-manager.sock`, else
/// `$TMPDIR/stage-manager.sock`.
pub fn socket_path() -> PathBuf {
    match custom_home() {
        Some(home) => socket_in(&home),
        None => user_temp_dir().join("stage-manager.sock"),
    }
}

/// The socket for a custom home; a path too long for `sun_path` becomes a short hashed name
/// in the temp dir (the same for the app and the client).
fn socket_in(home: &Path) -> PathBuf {
    let path = home.join("stage-manager.sock");
    if path.as_os_str().len() <= MAX_SOCKET_PATH {
        return path;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    home.hash(&mut h);
    user_temp_dir().join(format!("stage-manager-{:016x}.sock", h.finish()))
}

/// The home folder: `$HOME`, else the account's own (`getpwuid_r`), else the per-user
/// temporary folder; never a folder every account can write to (`/tmp`).
fn home() -> PathBuf {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from).or_else(account_home).unwrap_or_else(user_temp_dir)
}

/// The home folder of this process's account, from the user database.
fn account_home() -> Option<PathBuf> {
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: `passwd` is plain data; all-zero is a valid value for getpwuid_r to overwrite.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call and `buf` for the length we pass; getuid
    // cannot fail.
    let rc = unsafe { libc::getpwuid_r(libc::getuid(), &mut entry, buf.as_mut_ptr(), buf.len(), &mut found) };
    if rc != 0 || found.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r succeeded: `pw_dir` points at a NUL-terminated string inside `buf`.
    let dir = unsafe { CStr::from_ptr(entry.pw_dir) };
    Some(PathBuf::from(OsStr::from_bytes(dir.to_bytes()))).filter(|d| d.is_absolute())
}

/// The environment variable that moves the log directory alone.
pub const LOG_DIR_ENV: &str = "STAGE_MANAGER_LOG_DIR";

/// The daily logs' directory: `$STAGE_MANAGER_LOG_DIR`, else `$STAGE_MANAGER_HOME`, else
/// `~/Library/Logs/Stage Manager`.
pub fn log_dir() -> PathBuf {
    std::env::var_os(LOG_DIR_ENV)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(custom_home)
        .unwrap_or_else(|| home().join("Library/Logs/Stage Manager"))
}

/// The config and state directory (see [`config::config_dir`]).
pub fn config_dir() -> PathBuf {
    config::config_dir().unwrap_or_else(|| home().join(".config/stage-manager"))
}

/// `config.toml`.
pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

/// `state.json`.
pub fn state_file() -> PathBuf {
    config_dir().join("state.json")
}

/// `app-state.json`: what the app remembers about itself (see [`crate::app_state`]).
pub fn app_state_file() -> PathBuf {
    config_dir().join("app-state.json")
}

/// The clean-exit marker of the app process `pid`: written right before a clean exit, after
/// every window is back onscreen; the guardian then exits without a recovery scan.
pub fn clean_exit_marker(pid: u32) -> PathBuf {
    config_dir().join(format!(".clean-exit-{pid}"))
}

/// The parked file of the app process `pid`: the safety registry's windows (id, pid,
/// restore frame), rewritten whenever it changes, so the guardian knows exactly which windows
/// to wait for after a crash. Removed on a clean exit.
pub fn parked_file(pid: u32) -> PathBuf {
    config_dir().join(format!(".parked-{pid}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_lives_in_the_user_temp_dir_by_default() {
        if custom_home().is_none() {
            let p = socket_path();
            assert_eq!(p.file_name().and_then(|s| s.to_str()), Some("stage-manager.sock"));
            assert!(p.starts_with(user_temp_dir()));
        }
    }

    #[test]
    fn the_account_home_is_known_without_home_set() {
        let account = account_home().expect("the user database knows this account");
        assert!(account.is_absolute());
        assert!(!home().starts_with("/tmp"), "never the shared /tmp");
    }

    #[test]
    fn long_custom_homes_get_a_short_hashed_socket() {
        let short = socket_in(Path::new("/tmp/sm"));
        assert_eq!(short, PathBuf::from("/tmp/sm/stage-manager.sock"));
        let long = Path::new("/private/tmp").join("x".repeat(120));
        let p = socket_in(&long);
        assert!(p.as_os_str().len() <= MAX_SOCKET_PATH + 40);
        assert!(p.starts_with(user_temp_dir()));
        assert_eq!(p, socket_in(&long), "stable");
        assert_ne!(p, socket_in(&long.join("y")));
    }

    #[test]
    fn log_and_config_paths_are_named() {
        if std::env::var_os(LOG_DIR_ENV).is_none() && custom_home().is_none() {
            assert!(log_dir().ends_with("Library/Logs/Stage Manager"));
        }
        assert!(config_file().ends_with("config.toml"));
        assert!(state_file().ends_with("state.json"));
        assert!(app_state_file().ends_with("app-state.json"));
        assert!(clean_exit_marker(42).ends_with(".clean-exit-42"));
        assert!(parked_file(42).ends_with(".parked-42.json"));
    }
}
