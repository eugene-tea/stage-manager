//! Well-known file locations: ctl socket, log file, config file.

use std::ffi::CStr;
use std::path::PathBuf;

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

/// The ctl socket: `$TMPDIR/stage-manager.sock`.
pub fn socket_path() -> PathBuf {
    user_temp_dir().join("stage-manager.sock")
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `~/Library/Logs/Stage Manager`.
pub fn log_dir() -> PathBuf {
    home().join("Library/Logs/Stage Manager")
}

/// `~/Library/Logs/Stage Manager/stage-manager.log`.
pub fn log_file() -> PathBuf {
    log_dir().join("stage-manager.log")
}

// ---------------------------------------------------------------------------------------
// INTEGRATOR: config values hard-coded here until `src/config.rs` (branch `engine`) owns
// them. Replace `config_file` and `CONFIG_PLACEHOLDER` with the config module's path and
// commented defaults.
// ---------------------------------------------------------------------------------------

/// `~/.config/stage-manager/config.toml`.
pub fn config_file() -> PathBuf {
    home().join(".config/stage-manager/config.toml")
}

/// Written when Open Config finds no config file. INTEGRATOR: replace with the commented
/// defaults from `config.rs`.
pub const CONFIG_PLACEHOLDER: &str = "\
# Stage Manager configuration.
#
# macOS settings (System Settings > Desktop & Dock > Stage Manager) stay the source of
# truth for everything they cover; this file holds only what macOS lacks: new-window rules,
# placement, hotkeys, named stages, pinning, persistence.
#
# The commented defaults arrive with the engine milestone.
";

/// Creates the config file with [`CONFIG_PLACEHOLDER`] when it is missing; returns its path.
pub fn ensure_config_file() -> std::io::Result<PathBuf> {
    let path = config_file();
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, CONFIG_PLACEHOLDER)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_lives_in_the_user_temp_dir() {
        let p = socket_path();
        assert_eq!(p.file_name().and_then(|s| s.to_str()), Some("stage-manager.sock"));
        assert!(p.starts_with(user_temp_dir()));
    }

    #[test]
    fn log_and_config_paths_are_named() {
        assert!(log_file().ends_with("Library/Logs/Stage Manager/stage-manager.log"));
        assert!(config_file().ends_with(".config/stage-manager/config.toml"));
    }
}
