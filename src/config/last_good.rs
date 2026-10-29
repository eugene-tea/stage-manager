//! The last good config: a copy of `config.toml` as it last loaded, kept beside it
//! ([`FILE`]), so a file that fails at launch keeps the user's settings, as a failed reload
//! does (review C1).
//!
//! The WM thread's [`super::ConfigStore`] writes the copy after every successful load and
//! reload; every process that reads the file at launch (the store, the shell, the log)
//! falls back the same way ([`read_in_force`], [`fallback`]): the copy when it parses, else
//! the built-in defaults with update checks off ([`safe_defaults`]), since a file that does
//! not parse may be one that turned them off.

use std::path::{Path, PathBuf};

use super::Config;

/// The copy's file name, beside `config.toml` (hidden: it is the app's, not the user's).
pub const FILE: &str = ".config-last-good.toml";

/// Where the last good config used at a failed launch came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The copy of the file as it last loaded.
    LastGood,
    /// No copy that parses: the built-in defaults with update checks off.
    SafeDefaults,
}

impl Source {
    /// How a log line names it.
    pub fn describe(self) -> &'static str {
        match self {
            Source::LastGood => "the last good config",
            Source::SafeDefaults => "the built-in defaults with update checks off",
        }
    }
}

/// The copy's path for the config file at `config`.
pub fn path_for(config: &Path) -> PathBuf {
    config.with_file_name(FILE)
}

/// Keeps `text` (a file that just loaded) as the last good copy of `config`: written
/// owner-only and atomically (a temporary file renamed over it), and only when it changed.
pub fn keep(config: &Path, text: &str) {
    let path = path_for(config);
    if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return;
    }
    let tmp = path.with_extension("toml.saving");
    let written = crate::private_files::write(&tmp, text.as_bytes()).and_then(|_| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!("config: cannot keep the last good copy: {e}");
    }
}

/// The built-in defaults with update checks (and unattended installs) off: what runs when
/// nothing parses.
pub fn safe_defaults() -> Config {
    let mut config = Config::default();
    config.app.check_for_updates = false;
    config.app.install_updates = false;
    config
}

/// What runs when the file at `config` does not load: its last good copy, else
/// [`safe_defaults`].
pub fn fallback(config: &Path) -> (Config, Source) {
    let copy = std::fs::read_to_string(path_for(config)).ok().and_then(|t| Config::parse(&t).ok());
    match copy {
        Some(p) => (p.config, Source::LastGood),
        None => (safe_defaults(), Source::SafeDefaults),
    }
}

/// The config in force for a process that only reads the file at launch: its parse, else
/// [`fallback`]. Only a readable empty file means the plain defaults.
pub fn read_in_force(config: &Path) -> Config {
    match std::fs::read_to_string(config).ok().and_then(|t| Config::parse(&t).ok()) {
        Some(p) => p.config,
        None => fallback(config).0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_copy_is_owner_only_and_rewritten_only_when_it_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config.toml");
        keep(&config, "[app]\ncheck_for_updates = false\n");
        let copy = path_for(&config);
        assert_eq!(crate::private_files::mode(&copy).expect("mode"), 0o600);
        let before = std::fs::metadata(&copy).and_then(|m| m.modified()).expect("mtime");
        std::thread::sleep(std::time::Duration::from_millis(20));
        keep(&config, "[app]\ncheck_for_updates = false\n");
        assert_eq!(std::fs::metadata(&copy).and_then(|m| m.modified()).expect("mtime"), before, "unchanged, not written");
        assert!(!copy.with_extension("toml.saving").exists());
    }

    #[test]
    fn nothing_that_parses_means_the_defaults_with_update_checks_off() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[app\nbroken").expect("write");
        let (c, source) = fallback(&config);
        assert_eq!(source, Source::SafeDefaults);
        assert!(!c.app.check_for_updates && !c.app.install_updates);
        assert_eq!(read_in_force(&config), c);
        std::fs::write(path_for(&config), "also [broken").expect("write");
        assert_eq!(fallback(&config).1, Source::SafeDefaults);
    }
}
