//! Logging: `tracing-subscriber` filtered by `RUST_LOG` (default
//! `info,stage_manager=debug`), written to stderr and to
//! `~/Library/Logs/Stage Manager/stage-manager.log`.

use std::fs::OpenOptions;
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use super::paths;

/// The filter used when `RUST_LOG` is unset or invalid.
pub const DEFAULT_FILTER: &str = "info,stage_manager=debug";

/// Logs above this size are moved to `stage-manager.log.1` at launch.
const ROTATE_BYTES: u64 = 5 * 1024 * 1024;

/// Installs the global subscriber. Falls back to stderr only when the log file cannot be
/// opened.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let stderr = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    let file = open_log_file();
    let registry = tracing_subscriber::registry().with(filter).with(stderr);
    match file {
        Ok(f) => {
            let layer = tracing_subscriber::fmt::layer().with_ansi(false).with_writer(Mutex::new(f));
            let _ = registry.with(layer).try_init();
        }
        Err(e) => {
            let _ = registry.try_init();
            tracing::warn!("log file unavailable ({e}); logging to stderr only");
        }
    }
}

fn open_log_file() -> std::io::Result<std::fs::File> {
    std::fs::create_dir_all(paths::log_dir())?;
    let path = paths::log_file();
    if std::fs::metadata(&path).map(|m| m.len() > ROTATE_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    OpenOptions::new().create(true).append(true).open(path)
}
