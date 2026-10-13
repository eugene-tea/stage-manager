//! Safety: no window stays parked. A parked window that never comes back is the worst bug
//! this app can have, so there are seven layers (PLAN.md, "Safety"):
//!
//! 1. **Registry**: every parked window's AX element and (clamped) restore frame, kept in sync
//!    by the controller before and after each batch of actions.
//! 2. **Quit** (menu or `ctl quit`) unparks through the WM thread, falling back to the registry.
//! 3. **Panic hook**: outside the WM thread's guarded sections a panic unparks from the
//!    registry (`try_lock`; a recovery scan when it is busy) and exits.
//! 4. **Signals**: SIGTERM, SIGINT and SIGHUP go through a self-pipe to a watcher thread that
//!    quits cleanly; SIGABRT gets a best-effort unpark in the handler itself.
//! 5. **Guardian** ([`super::guardian`]): `stage-manager --guardian <pid>`, spawned at launch,
//!    runs the recovery scan when the app dies without its clean-exit marker, then waits for
//!    every window still parked on a Space not shown and brings each back once its Space
//!    shows. It learns which windows to wait for from the parked file this module keeps
//!    beside the registry ([`paths::parked_file`]).
//! 6. **Recovery scan** (launch, guardian, panic fallback): every window of every allowed app
//!    whose frame is stranded in the park zone goes back to its `state.json` frame or is
//!    centered ([`crate::engine::recovery`]).
//! 7. **Clamping**: every restore frame is clamped onto the visible display first.
//!
//! Quit counts as clean only when, after unparking, the window server lists no window of an
//! allowed app in the park zone on any Space ([`stranded_allowed`]).

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use objc2_app_kit::NSRunningApplication;

use super::ax::{AxElement, names};
use super::{desktop, logging, observe, paths, windows};
use crate::engine::geometry::Rect;
use crate::engine::park::Corner;
use crate::engine::persist::StateFile;
use crate::engine::recovery::{self, LiveWindow, ParkedFile, Registered, ScanDisplays};
use crate::logbook::counters::{self, Count};

/// Assumed menu bar height when the visible frame is unknown (guardian).
const MENU_BAR_GUESS: f64 = 38.0;

/// A window a safety path must move back (parked, or fitted clear of the strip): its
/// element and the frame that brings it back.
#[derive(Clone, Debug)]
pub struct ParkedEntry {
    /// The window's AX element.
    pub element: AxElement,
    /// Where it goes: the restore frame clamped onto the visible display, or a fitted
    /// window's exact pre-fit frame.
    pub restore: Rect,
    /// A child window (dialog, sheet, panel): some refuse a resize or a move; the recovery
    /// scan covers them.
    pub child: bool,
    /// A shown window we fitted clear of the strip (not parked): `restore` is its exact
    /// pre-fit frame.
    pub fitted: bool,
}

/// Set once Quit is requested: the WM thread stops long batches and answers Quit first.
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Marks that the app is quitting (any thread).
pub fn request_quit() {
    QUIT_REQUESTED.store(true, Ordering::SeqCst);
}

/// True once Quit is requested.
pub fn quit_requested() -> bool {
    QUIT_REQUESTED.load(Ordering::SeqCst)
}

static REGISTRY: Mutex<BTreeMap<u32, ParkedEntry>> = Mutex::new(BTreeMap::new());

/// The main display's visible frame as last reported (for scans in this process).
static VISIBLE: Mutex<Option<Rect>> = Mutex::new(None);

/// Every park corner used this run (the guardian's: those the app's parked file names), so a
/// window parked at a corner the arrangement no longer picks still counts as parked.
static CORNERS: Mutex<BTreeSet<Corner>> = Mutex::new(BTreeSet::new());

fn corners_used() -> Vec<Corner> {
    CORNERS.lock().map(|c| c.iter().copied().collect()).unwrap_or_default()
}

thread_local! {
    /// True inside a [`guarded`] section: a panic there is caught and recovered from.
    static GUARDED: Cell<bool> = const { Cell::new(false) };
}

fn lock_registry() -> MutexGuard<'static, BTreeMap<u32, ParkedEntry>> {
    REGISTRY.lock().unwrap_or_else(|p| p.into_inner())
}

/// Adds entries (before a batch: windows about to be parked are covered at once).
pub fn registry_add(entries: impl IntoIterator<Item = (u32, ParkedEntry)>) {
    let listed = {
        let mut guard = lock_registry();
        guard.extend(entries);
        registered(&guard)
    };
    write_parked_file(&listed);
}

/// Replaces the registry with exactly `entries` (after a batch).
pub fn registry_replace(entries: BTreeMap<u32, ParkedEntry>) {
    let listed = {
        let mut guard = lock_registry();
        *guard = entries;
        registered(&guard)
    };
    write_parked_file(&listed);
}

/// The registry as the guardian needs it: ids, pids and restore frames.
fn registered(map: &BTreeMap<u32, ParkedEntry>) -> Vec<Registered> {
    map.iter()
        .map(|(&id, e)| Registered {
            id,
            pid: e.element.pid().unwrap_or(-1),
            restore: e.restore,
            child: e.child,
            fitted: e.fitted,
        })
        .collect()
}

/// What the parked file holds now (only rewritten when it changes).
static PARKED_WRITTEN: Mutex<Option<ParkedFile>> = Mutex::new(None);

/// Keeps this process's parked file ([`paths::parked_file`]) equal to the registry, with the
/// main display's visible frame, written atomically; removed when the registry is empty.
fn write_parked_file(listed: &[Registered]) {
    let file = ParkedFile { visible: visible_frame(), windows: listed.to_vec(), corners: corners_used() };
    let Ok(mut written) = PARKED_WRITTEN.lock() else { return };
    if written.as_ref() == Some(&file) {
        return;
    }
    let path = paths::parked_file(std::process::id());
    let result = if listed.is_empty() {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        let tmp = path.with_extension("json.tmp");
        serde_json::to_vec(&file)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(&tmp, bytes))
            .and_then(|()| std::fs::rename(&tmp, &path))
    };
    match result {
        Ok(()) => *written = Some(file),
        Err(e) => tracing::warn!("cannot write {}: {e}", path.display()),
    }
}

/// What the app process `pid` left in its parked file: its registered windows and the main
/// display's visible frame; empty when it has none or the file cannot be read.
pub fn read_parked_file(pid: u32) -> ParkedFile {
    let path = paths::parked_file(pid);
    match std::fs::read(&path) {
        Ok(bytes) => ParkedFile::from_json(&bytes).unwrap_or_else(|e| {
            tracing::warn!("cannot read {}: {e}", path.display());
            ParkedFile::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ParkedFile::default(),
        Err(e) => {
            tracing::warn!("cannot read {}: {e}", path.display());
            ParkedFile::default()
        }
    }
}

/// The registry's entry for a window, if it has one.
pub fn registry_entry(id: u32) -> Option<ParkedEntry> {
    lock_registry().get(&id).cloned()
}

/// How many windows the registry holds.
pub fn registry_len() -> usize {
    lock_registry().len()
}

/// Remembers the main display's visible frame and park corner for the park test and
/// clamping in scans, and writes them to the parked file for the guardian (when windows are
/// registered). Corners add up over the run: windows of other Desktops may still sit at an
/// earlier one.
pub fn set_main_display(visible: Rect, corner: Corner) {
    let changed = match VISIBLE.lock() {
        Ok(mut v) => v.replace(visible) != Some(visible),
        Err(_) => false,
    };
    let new_corner = CORNERS.lock().is_ok_and(|mut c| c.insert(corner));
    if changed || new_corner {
        let listed = registered(&lock_registry());
        write_parked_file(&listed);
    }
}

/// Takes the main display's visible frame without writing a parked file (the guardian, from
/// the app's parked file).
pub fn assume_visible_frame(visible: Rect) {
    if let Ok(mut v) = VISIBLE.lock() {
        *v = Some(visible);
    }
}

/// Takes the park corners the app used without writing a parked file (the guardian, from
/// the app's parked file).
pub fn assume_corners(corners: &[Corner]) {
    if let Ok(mut c) = CORNERS.lock() {
        c.extend(corners.iter().copied());
    }
}

fn visible_frame() -> Option<Rect> {
    VISIBLE.lock().ok().and_then(|v| *v)
}

/// The outcome of an unpark or a scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Windows looked at.
    pub checked: usize,
    /// Windows moved back onscreen.
    pub restored: usize,
    /// Windows that could not be moved.
    pub failed: usize,
}

/// The registry entries [`unpark_registry`] could not restore: id, frame target, error.
static LAST_FAILURES: Mutex<Vec<(u32, bool, String)>> = Mutex::new(Vec::new());

/// Moves every registry window back to its restore frame. Waits up to `wait` for the lock;
/// `None` when it stayed busy. Entries that were restored leave the registry.
pub fn unpark_registry(wait: Duration) -> Option<Report> {
    let deadline = Instant::now() + wait;
    let mut guard = loop {
        match REGISTRY.try_lock() {
            Ok(g) => break g,
            Err(TryLockError::Poisoned(p)) => break p.into_inner(),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(TryLockError::WouldBlock) => return None,
        }
    };
    let mut report = Report::default();
    let mut failures = Vec::new();
    let entries = std::mem::take(&mut *guard);
    for (id, entry) in entries {
        report.checked += 1;
        match entry.element.set_frame(entry.restore) {
            Ok(()) => report.restored += 1,
            Err(e) => {
                // Logged by the caller once it knows whether the recovery scan covered it.
                tracing::debug!(id, child = entry.child, "safety: cannot restore window: {e}");
                failures.push((id, entry.child, e.to_string()));
                report.failed += 1;
                guard.insert(id, entry);
            }
        }
    }
    if let Ok(mut f) = LAST_FAILURES.lock() {
        *f = failures;
    }
    Some(report)
}

/// The last line of defence in this process: the registry, else a recovery scan. True when
/// nothing failed.
///
/// A registry entry that could not be moved (a fixed-size dialog, say) is not a failure when
/// the recovery scan leaves no window stranded: it is logged at debug only.
pub fn emergency_unpark(reason: &str) -> bool {
    match unpark_registry(Duration::from_millis(200)) {
        Some(r) if r.failed == 0 => {
            if r.restored > 0 {
                counters::bump(Count::Recovery);
            }
            tracing::warn!(reason, restored = r.restored, "safety: registry unparked");
            true
        }
        Some(r) => {
            tracing::info!(reason, restored = r.restored, failed = r.failed, "safety: registry unparked; running the recovery scan for the rest");
            let scan = recovery_scan(reason);
            let failures = LAST_FAILURES.lock().map(|f| f.clone()).unwrap_or_default();
            let stranded = stranded_ids();
            let mut ok = scan.failed == 0;
            for (id, child, error) in failures {
                if stranded.contains(&id) {
                    ok = false;
                    tracing::warn!(reason, id, child, "safety: window still parked after the scan: {error}");
                } else {
                    tracing::debug!(reason, id, child, "safety: the registry could not move it ({error}), but it is not parked");
                }
            }
            ok
        }
        None => {
            tracing::warn!(reason, "safety: registry busy; running the recovery scan");
            recovery_scan(reason).failed == 0
        }
    }
}

/// Window ids (of every app) that sit stranded in the main display's park zone now.
fn stranded_ids() -> Vec<u32> {
    let displays = scan_displays();
    windows::list_windows()
        .iter()
        .filter(|w| w.layer == 0 && recovery::is_stranded(w.bounds, &displays))
        .map(|w| w.id)
        .collect()
}

/// Windows of allowed apps (`STAGE_MANAGER_ONLY` honored, our own process and the system's
/// excluded apps skipped) the window server lists in the main display's park zone, on any
/// Space: windows on other Spaces are listed too, off screen, with their frames.
pub fn stranded_allowed() -> Vec<windows::CgWindow> {
    let only = observe::only_from_env();
    let own = std::process::id() as i32;
    let displays = scan_displays();
    let mut bundles: std::collections::HashMap<i32, Option<String>> = std::collections::HashMap::new();
    windows::list_windows()
        .into_iter()
        .filter(|w| w.layer == 0 && recovery::is_stranded(w.bounds, &displays))
        .filter(|w| {
            let bundle = bundles.entry(w.pid).or_insert_with(|| bundle_id_of(w.pid));
            observe::app_allowed(only.as_ref(), own, w.pid, bundle.as_deref())
        })
        .collect()
}

/// Runs `f`, catching a panic (logged). Inside, the panic hook does not unpark or exit: the
/// caller recovers instead.
pub fn guarded<R>(what: &str, f: impl FnOnce() -> R) -> Option<R> {
    let previous = GUARDED.with(|g| g.replace(true));
    let result = std::panic::catch_unwind(AssertUnwindSafe(f));
    GUARDED.with(|g| g.set(previous));
    match result {
        Ok(v) => Some(v),
        Err(_) => {
            // The panic hook counted it.
            tracing::error!("panic in {what}; recovered");
            None
        }
    }
}

/// Installs the panic hook: log, then (outside guarded sections) unpark everything and exit
/// with status 101.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        counters::bump(Count::Panic);
        tracing::error!("panic: {info}");
        default(info);
        if GUARDED.try_with(Cell::get).unwrap_or(false) {
            return;
        }
        emergency_unpark("panic");
        // A capture in flight at exit makes macOS alert about screen access.
        super::capture::quiesce(std::time::Duration::from_secs(1));
        logging::write_counts("panic");
        std::process::exit(101);
    }));
}

static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);
static ABORTING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(sig: libc::c_int) {
    let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = sig as u8;
        // SAFETY: write(2) is async-signal-safe; `byte` lives for the call.
        unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
    }
}

extern "C" fn on_abort(sig: libc::c_int) {
    if !ABORTING.swap(true, Ordering::SeqCst) {
        // Best effort only: not async-signal-safe, but the process is dying anyway and the
        // guardian covers whatever this misses.
        let _ = unpark_registry(Duration::from_millis(100));
    }
    // SAFETY: restoring the default action and re-raising is the documented way to finish
    // an abort from a handler.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

fn install_handler(sig: libc::c_int, handler: extern "C" fn(libc::c_int)) -> std::io::Result<()> {
    // SAFETY: a zeroed sigaction is a valid starting point; the handler has the plain
    // one-argument signature (no SA_SIGINFO).
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handler as usize;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: `action.sa_mask` is valid storage.
    unsafe { libc::sigemptyset(&mut action.sa_mask) };
    // SAFETY: valid signal number and action; the old action is not needed.
    if unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Routes SIGTERM, SIGINT and SIGHUP through a self-pipe to a watcher thread that calls
/// `on_quit(signal)`; installs the best-effort SIGABRT handler.
pub fn watch_signals(on_quit: impl Fn(i32) + Send + 'static) -> std::io::Result<()> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two descriptors.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    // SAFETY: plain fcntl calls on descriptors we own.
    unsafe {
        libc::fcntl(write_fd, libc::F_SETFL, libc::O_NONBLOCK);
        libc::fcntl(read_fd, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(write_fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    SIGNAL_PIPE.store(write_fd, Ordering::SeqCst);
    std::thread::Builder::new().name("signals".into()).spawn(move || {
        loop {
            let mut byte = 0u8;
            // SAFETY: reading one byte into a valid buffer.
            let n = unsafe { libc::read(read_fd, (&mut byte as *mut u8).cast(), 1) };
            if n == 1 {
                tracing::info!(signal = byte, "signal received; quitting");
                on_quit(i32::from(byte));
            } else if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            } else {
                break;
            }
        }
    })?;
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        install_handler(sig, on_signal)?;
    }
    install_handler(libc::SIGABRT, on_abort)?;
    Ok(())
}

/// Writes this process's clean-exit marker (every window is back onscreen) and removes its
/// parked file.
pub fn mark_clean_exit() {
    let _ = std::fs::remove_file(paths::parked_file(std::process::id()));
    let path = paths::clean_exit_marker(std::process::id());
    if let Err(e) = std::fs::write(&path, b"clean\n") {
        tracing::warn!("cannot write {}: {e}", path.display());
    }
}

/// The bundle id of a running process, when LaunchServices knows it.
pub(crate) fn bundle_id_of(pid: i32) -> Option<String> {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .and_then(|a| a.bundleIdentifier())
        .map(|s| s.to_string())
}

/// The main display (visible frame as last reported, else guessed) and the others.
pub(crate) fn scan_displays() -> ScanDisplays {
    let mut bounds = desktop::cg_display_bounds();
    let main = bounds.remove(0);
    let visible = visible_frame()
        .filter(|v| main.contains_rect(v))
        .unwrap_or_else(|| Rect::new(main.x, main.y + MENU_BAR_GUESS, main.w, main.h - MENU_BAR_GUESS));
    ScanDisplays::new(main, visible, bounds, corners_used())
}

/// The recovery scan: every window of every allowed app (`STAGE_MANAGER_ONLY` honored, our
/// own process and the system's excluded apps skipped) that is stranded in the park zone
/// goes back to its `state.json` frame, or is centered.
pub fn recovery_scan(reason: &str) -> Report {
    let only = observe::only_from_env();
    let own = std::process::id() as i32;
    let mut pids: Vec<i32> = windows::list_windows().iter().filter(|w| w.layer == 0).map(|w| w.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    let state = std::fs::read_to_string(paths::state_file()).ok().and_then(|t| match StateFile::from_json(&t) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("recovery scan: {e}");
            None
        }
    });
    let displays = scan_displays();
    let mut live = Vec::new();
    let mut elements = Vec::new();
    for pid in pids {
        let bundle = bundle_id_of(pid);
        if !observe::app_allowed(only.as_ref(), own, pid, bundle.as_deref()) {
            continue;
        }
        let Ok(list) = AxElement::application(pid).windows() else { continue };
        for w in list {
            if w.role().ok().as_deref() != Some(names::ROLE_WINDOW) {
                continue;
            }
            let Ok(frame) = w.frame() else { continue };
            live.push(LiveWindow { bundle_id: bundle.clone().unwrap_or_default(), title: w.title().unwrap_or_default(), frame });
            elements.push(w);
        }
    }
    let mut report = Report { checked: live.len(), ..Report::default() };
    for (i, frame) in recovery::plan(&live, state.as_ref(), &displays) {
        match elements[i].set_frame(frame) {
            Ok(()) => {
                report.restored += 1;
                tracing::warn!(reason, bundle = live[i].bundle_id, from = %live[i].frame, to = %frame, "recovery: window brought back");
            }
            Err(e) => {
                report.failed += 1;
                tracing::error!(reason, bundle = live[i].bundle_id, "recovery: cannot move window: {e}");
            }
        }
    }
    if report.restored > 0 {
        counters::bump(Count::Recovery);
    }
    tracing::info!(reason, checked = report.checked, restored = report.restored, failed = report.failed, "recovery scan");
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarded_catches_and_returns_none() {
        assert_eq!(guarded("test", || 5), Some(5));
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r: Option<()> = guarded("test", || panic!("boom"));
        std::panic::set_hook(prev);
        assert_eq!(r, None);
        assert!(!GUARDED.with(Cell::get), "flag reset after a panic");
    }

    #[test]
    fn empty_registry_unparks_nothing() {
        let r = unpark_registry(Duration::from_millis(10)).unwrap_or_default();
        assert_eq!(r.failed, 0);
    }
}
