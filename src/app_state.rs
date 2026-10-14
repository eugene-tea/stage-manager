//! What the app remembers about itself between launches (not configuration): whether the
//! welcome was shown, whether the login item was set up once, the last update check and a
//! skipped version. `app-state.json` beside `config.toml` (so `STAGE_MANAGER_HOME` moves it
//! too). A missing or unreadable file is a first launch.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The remembered facts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// The welcome window was shown once (the first launch after install).
    pub welcomed: bool,
    /// The default login item was registered (or deliberately not) on an installed copy's
    /// first launch; from then on only the user's choice changes it.
    pub login_item_set_up: bool,
    /// The last automatic or manual update check that reached GitHub (Unix seconds).
    pub last_update_check: Option<u64>,
    /// "Skip This Version": automatic checks stay quiet about this version and older.
    pub skipped_version: Option<String>,
}

impl AppState {
    /// Reads `path`; defaults when it is missing or unreadable.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Writes `path` atomically (a temporary file renamed over it; its directory created),
    /// readable by the user only ([`crate::private_files`]), logging a failure: a crash
    /// mid-write never leaves half a file.
    pub fn save(&self, path: &Path) {
        let tmp = path.with_extension("json.tmp");
        let result = path
            .parent()
            .map_or(Ok(()), crate::private_files::ensure_dir)
            .and_then(|_| serde_json::to_string_pretty(self).map_err(std::io::Error::other))
            .and_then(|text| crate::private_files::write(&tmp, (text + "\n").as_bytes()))
            .and_then(|_| std::fs::rename(&tmp, path));
        if let Err(e) = result {
            tracing::warn!("cannot save {}: {e}", path.display());
        }
    }
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_broken_file_is_a_first_launch_and_a_save_reads_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("app-state.json");
        assert_eq!(AppState::load(&path), AppState::default());
        std::fs::write(&path, "{ nope").expect("writes");
        assert_eq!(AppState::load(&path), AppState::default());
        let s = AppState { welcomed: true, login_item_set_up: true, last_update_check: Some(42), skipped_version: Some("1.2.0".into()) };
        s.save(&path);
        assert_eq!(AppState::load(&path), s);
        assert!(!path.with_extension("json.tmp").exists(), "written through a temporary file");
        assert_eq!(crate::private_files::mode(&path).expect("mode"), 0o600, "readable by the user only");
        std::fs::write(&path, "{\"welcomed\": true}").expect("writes");
        assert_eq!(AppState::load(&path), AppState { welcomed: true, ..AppState::default() }, "missing fields default");
    }
}
