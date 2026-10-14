//! Replacing a file so that a crash, a kernel panic or a power cut leaves either the old
//! contents or the new ones, never an empty or half-written file: the bytes go to a temporary
//! file beside it, which is flushed to disk before it is renamed over the old one, and the
//! directory is flushed after the rename (the review of 1.1.0, Grok M1).

use std::fs::File;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// How hard the data is pushed to disk before the rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flush {
    /// `F_FULLFSYNC` (Rust's `sync_all` on macOS): through the drive's own cache. For files
    /// written now and then (`state.json`, debounced).
    Full,
    /// `fsync(2)`: to the drive, not through its cache; cheap enough for a file rewritten at
    /// every batch of window moves (the parked file, on the WM thread).
    Fast,
}

/// The temporary file `path` is written through: `<name>.tmp` beside it.
pub fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Replaces `path` with `bytes` (see the module docs). On an error the old file is left as it
/// was and the temporary file is removed.
pub fn replace(path: &Path, bytes: &[u8], flush: Flush) -> std::io::Result<()> {
    let tmp = temp_path(path);
    let result = write_flushed(&tmp, bytes, flush).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    // The rename itself lives in the directory: flush it too. Best effort (some file
    // systems refuse to sync a directory); the data is already on disk either way.
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && let Ok(d) = File::open(dir)
    {
        // SAFETY: fsync on a descriptor we own for the duration of the call.
        unsafe { libc::fsync(d.as_raw_fd()) };
    }
    Ok(())
}

fn write_flushed(tmp: &Path, bytes: &[u8], flush: Flush) -> std::io::Result<()> {
    let mut f = File::create(tmp)?;
    f.write_all(bytes)?;
    match flush {
        Flush::Full => f.sync_all(),
        Flush::Fast => {
            // SAFETY: fsync on a descriptor `f` owns for the duration of the call.
            if unsafe { libc::fsync(f.as_raw_fd()) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sm-durable-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_default();
        dir
    }

    #[test]
    fn replaces_the_file_and_leaves_no_temporary_file() {
        let dir = scratch("ok");
        let path = dir.join("state.json");
        std::fs::write(&path, b"old").unwrap_or_default();
        for flush in [Flush::Full, Flush::Fast] {
            assert!(replace(&path, b"new", flush).is_ok());
            assert_eq!(std::fs::read(&path).unwrap_or_default(), b"new");
            assert!(!temp_path(&path).exists());
        }
        assert_eq!(temp_path(&path), dir.join("state.json.tmp"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_write_keeps_the_old_file() {
        let dir = scratch("fail");
        let path = dir.join("state.json");
        std::fs::write(&path, b"old").unwrap_or_default();
        // The temporary file's name taken by a directory: the write fails before the rename.
        std::fs::create_dir_all(temp_path(&path)).unwrap_or_default();
        assert!(replace(&path, b"new", Flush::Full).is_err());
        assert_eq!(std::fs::read(&path).unwrap_or_default(), b"old");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
