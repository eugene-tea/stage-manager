//! Well-known file locations: ctl socket, log file, config and state files, clean-exit
//! markers.
//!
//! `STAGE_MANAGER_HOME=<dir>` moves all of them into `<dir>` (test runs use a scratch
//! directory, so they never touch the real config, state, socket or log). Without it:
//! config and state in `~/.config/stage-manager`, the log in `~/Library/Logs/Stage Manager`,
//! the socket in the per-user temporary directory.

use std::ffi::CStr;
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

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `$STAGE_MANAGER_HOME`, else `~/Library/Logs/Stage Manager`.
pub fn log_dir() -> PathBuf {
    custom_home().unwrap_or_else(|| home().join("Library/Logs/Stage Manager"))
}

/// `stage-manager.log` in [`log_dir`].
pub fn log_file() -> PathBuf {
    log_dir().join("stage-manager.log")
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

/// The clean-exit marker of the app process `pid`: written right before a clean exit, after
/// every window is back onscreen; the guardian then exits without a recovery scan.
pub fn clean_exit_marker(pid: u32) -> PathBuf {
    config_dir().join(format!(".clean-exit-{pid}"))
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
        assert!(log_file().ends_with("stage-manager.log"));
        assert!(config_file().ends_with("config.toml"));
        assert!(state_file().ends_with("state.json"));
        assert!(clean_exit_marker(42).ends_with(".clean-exit-42"));
    }
}
