//! Files only the user's own account may read: `state.json` and the parked file (window
//! titles and frames), `app-state.json`, and the folder that holds them (and the ctl socket
//! under `STAGE_MANAGER_HOME`), like the logs (`0600`, folders `0700`).
//!
//! A home folder macOS makes is not readable by other accounts, but one shared with the
//! `staff` group, or a folder moved elsewhere, would otherwise expose titles to every local
//! account.

use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// A private file's mode: the owner reads and writes.
pub const FILE_MODE: u32 = 0o600;

/// A private folder's mode: the owner only.
pub const DIR_MODE: u32 = 0o700;

/// Writes `bytes` to `path`, created (or truncated) readable by its owner only. A file that
/// was already there with a wider mode is narrowed before anything is written into it. For
/// the temporary file of an atomic write: the rename keeps the mode.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    create(path)?.write_all(bytes)
}

/// Creates (or truncates) `path` readable by its owner only, narrowing a wider file left
/// there; for writers that flush themselves (`platform::durable`).
pub fn create(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new().write(true).create(true).truncate(true).mode(FILE_MODE).open(path)?;
    narrow(&file)?;
    Ok(file)
}

/// Narrows an open file to [`FILE_MODE`] when its mode is wider (one left by an older
/// version, or created under another umask).
fn narrow(file: &File) -> io::Result<()> {
    let mode = file.metadata()?.permissions().mode() & 0o777;
    if mode & !FILE_MODE != 0 {
        file.set_permissions(Permissions::from_mode(FILE_MODE))?;
    }
    Ok(())
}

/// Creates `dir` (and its parents) and makes it [`DIR_MODE`] when it is open to others.
/// Parents are created as usual and never changed; a folder the user may not change (one
/// owned by another account, like `/tmp`) is left as it is.
pub fn ensure_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    if std::fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
        match std::fs::set_permissions(dir, Permissions::from_mode(DIR_MODE)) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {}
            other => other?,
        }
    }
    Ok(())
}

/// The mode bits of `path` (tests and checks).
pub fn mode(path: &Path) -> io::Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_private_file_is_owner_only_new_or_old() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json.tmp");
        write(&path, b"{}").expect("writes");
        assert_eq!(mode(&path).expect("mode"), 0o600);
        assert_eq!(std::fs::read(&path).expect("reads"), b"{}");
        // A file an older version left readable by everyone is narrowed, then replaced.
        std::fs::set_permissions(&path, Permissions::from_mode(0o644)).expect("chmod");
        write(&path, b"[1]").expect("writes");
        assert_eq!(mode(&path).expect("mode"), 0o600);
        assert_eq!(std::fs::read(&path).expect("reads"), b"[1]");
        // The atomic write's rename keeps it.
        let fin = dir.path().join("state.json");
        std::fs::rename(&path, &fin).expect("renames");
        assert_eq!(mode(&fin).expect("mode"), 0o600);
    }

    #[test]
    fn a_private_folder_is_made_or_narrowed_but_only_when_ours() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ours = dir.path().join("a/stage-manager");
        ensure_dir(&ours).expect("creates");
        assert_eq!(mode(&ours).expect("mode"), 0o700);
        std::fs::set_permissions(&ours, Permissions::from_mode(0o755)).expect("chmod");
        ensure_dir(&ours).expect("narrows");
        assert_eq!(mode(&ours).expect("mode"), 0o700);
        // Already private: nothing to do; a folder not ours (root's /tmp) is left alone.
        ensure_dir(&ours).expect("again");
        let tmp_before = mode(Path::new("/tmp")).expect("mode");
        let _ = ensure_dir(Path::new("/tmp"));
        assert_eq!(mode(Path::new("/tmp")).expect("mode"), tmp_before);
    }
}
