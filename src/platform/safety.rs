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
//! 5. **Guardian**: `stage-manager --guardian <pid>`, spawned at launch, waits for the app to
//!    exit (kqueue `NOTE_EXIT`) and runs the recovery scan unless the app wrote its clean-exit
//!    marker.
//! 6. **Recovery scan** (launch, guardian, panic fallback): every window of every allowed app
//!    whose frame is stranded in the park zone goes back to its `state.json` frame or is
//!    centered ([`crate::engine::recovery`]).
//! 7. **Clamping**: every restore frame is clamped onto the visible display first.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::panic::AssertUnwindSafe;
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use objc2_app_kit::NSRunningApplication;

use super::ax::{AxElement, names};
use super::{desktop, observe, paths, windows};
use crate::engine::geometry::Rect;
use crate::engine::persist::StateFile;
use crate::engine::recovery::{self, LiveWindow, ScanDisplays};

/// The AX messaging timeout used by the guardian and emergency paths.
const AX_TIMEOUT_SECONDS: f32 = 0.25;

/// Assumed menu bar height when the visible frame is unknown (guardian).
const MENU_BAR_GUESS: f64 = 38.0;

/// A parked window: its element and the frame that brings it back.
#[derive(Clone, Debug)]
pub struct ParkedEntry {
    /// The window's AX element.
    pub element: AxElement,
    /// The restore frame, already clamped onto the visible display.
    pub restore: Rect,
}

static REGISTRY: Mutex<BTreeMap<u32, ParkedEntry>> = Mutex::new(BTreeMap::new());

/// The main display's visible frame as last reported (for scans in this process).
static VISIBLE: Mutex<Option<Rect>> = Mutex::new(None);

thread_local! {
    /// True inside a [`guarded`] section: a panic there is caught and recovered from.
    static GUARDED: Cell<bool> = const { Cell::new(false) };
}

fn lock_registry() -> MutexGuard<'static, BTreeMap<u32, ParkedEntry>> {
    REGISTRY.lock().unwrap_or_else(|p| p.into_inner())
}

/// Adds entries (before a batch: windows about to be parked are covered at once).
pub fn registry_add(entries: impl IntoIterator<Item = (u32, ParkedEntry)>) {
    lock_registry().extend(entries);
}

/// Replaces the registry with exactly `entries` (after a batch).
pub fn registry_replace(entries: BTreeMap<u32, ParkedEntry>) {
    *lock_registry() = entries;
}

/// How many windows the registry holds.
pub fn registry_len() -> usize {
    lock_registry().len()
}

/// Remembers the main display's visible frame for clamping in scans.
pub fn set_visible_frame(visible: Rect) {
    if let Ok(mut v) = VISIBLE.lock() {
        *v = Some(visible);
    }
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
    let entries = std::mem::take(&mut *guard);
    for (id, entry) in entries {
        report.checked += 1;
        match entry.element.set_frame(entry.restore) {
            Ok(()) => report.restored += 1,
            Err(e) => {
                tracing::warn!(id, "safety: cannot restore window: {e}");
                report.failed += 1;
                guard.insert(id, entry);
            }
        }
    }
    Some(report)
}

/// The last line of defence in this process: the registry, else a recovery scan. True when
/// nothing failed.
pub fn emergency_unpark(reason: &str) -> bool {
    match unpark_registry(Duration::from_millis(200)) {
        Some(r) if r.failed == 0 => {
            tracing::warn!(reason, restored = r.restored, "safety: registry unparked");
            true
        }
        other => {
            tracing::warn!(reason, ?other, "safety: registry unavailable or incomplete; running the recovery scan");
            recovery_scan(reason).failed == 0
        }
    }
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
        tracing::error!("panic: {info}");
        default(info);
        if GUARDED.try_with(Cell::get).unwrap_or(false) {
            return;
        }
        emergency_unpark("panic");
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

/// Writes this process's clean-exit marker (every window is back onscreen).
pub fn mark_clean_exit() {
    let path = paths::clean_exit_marker(std::process::id());
    if let Err(e) = std::fs::write(&path, b"clean\n") {
        tracing::warn!("cannot write {}: {e}", path.display());
    }
}

/// Spawns the guardian for this process. It runs in its own session, so it outlives the app
/// and its process group.
pub fn spawn_guardian() -> Option<u32> {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("cannot find our own executable for the guardian: {e}");
            return None;
        }
    };
    let mut cmd = Command::new(exe);
    cmd.arg("--guardian")
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches nothing shared with the parent.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            tracing::info!(pid, "guardian started");
            let _ = std::thread::Builder::new().name("guardian-reaper".into()).spawn(move || {
                match child.wait() {
                    Ok(status) => tracing::warn!(?status, "guardian exited while the app runs"),
                    Err(e) => tracing::warn!("guardian wait failed: {e}"),
                }
            });
            Some(pid)
        }
        Err(e) => {
            tracing::error!("cannot start the guardian: {e}");
            None
        }
    }
}

/// `stage-manager --guardian <pid>`: waits for `pid` to exit, then runs the recovery scan
/// unless it wrote its clean-exit marker.
pub fn run_guardian(args: &[String]) -> ExitCode {
    super::logging::init();
    let Some(parent) = args.first().and_then(|a| a.parse::<i32>().ok()).filter(|p| *p > 0) else {
        eprintln!("usage: stage-manager --guardian <pid>");
        return ExitCode::from(2);
    };
    if let Err(e) = AxElement::system_wide().set_messaging_timeout(AX_TIMEOUT_SECONDS) {
        tracing::debug!("guardian: cannot set the AX timeout: {e}");
    }
    tracing::debug!(parent, "guardian watching");
    wait_for_exit(parent);
    let marker = paths::clean_exit_marker(parent as u32);
    if marker.exists() {
        let _ = std::fs::remove_file(&marker);
        tracing::debug!(parent, "guardian: clean exit; nothing to do");
        return ExitCode::SUCCESS;
    }
    tracing::warn!(parent, "guardian: the app exited without a clean exit; running the recovery scan");
    let r = recovery_scan("guardian");
    tracing::warn!(checked = r.checked, restored = r.restored, failed = r.failed, "guardian: recovery scan done");
    ExitCode::SUCCESS
}

/// Blocks until `pid` exits (kqueue `EVFILT_PROC` / `NOTE_EXIT`); returns at once when it is
/// already gone. Falls back to polling when kqueue fails.
fn wait_for_exit(pid: i32) {
    // SAFETY: kqueue() takes no arguments.
    let kq = unsafe { libc::kqueue() };
    if kq >= 0 {
        // SAFETY: a zeroed kevent is valid; the fields are set below.
        let mut change: libc::kevent = unsafe { std::mem::zeroed() };
        change.ident = pid as usize;
        change.filter = libc::EVFILT_PROC;
        change.flags = libc::EV_ADD | libc::EV_ONESHOT;
        change.fflags = libc::NOTE_EXIT;
        // SAFETY: as above.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: one change in, room for one event out, no timeout.
            let n = unsafe { libc::kevent(kq, &change, 1, &mut event, 1, std::ptr::null()) };
            if n >= 0 {
                // A registration error comes back as an EV_ERROR event: ESRCH means the
                // process is already gone; anything else falls back to polling.
                let (flags, data) = (event.flags, event.data);
                if n == 1 && flags & libc::EV_ERROR != 0 && data != libc::ESRCH as isize {
                    tracing::warn!(error = data, "guardian: kevent registration failed; polling");
                    poll_for_exit(pid);
                }
                break;
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                if err.raw_os_error() != Some(libc::ESRCH) {
                    tracing::warn!("guardian: kevent failed ({err}); polling");
                    poll_for_exit(pid);
                }
                break;
            }
        }
        // SAFETY: closing our own descriptor.
        unsafe { libc::close(kq) };
    } else {
        poll_for_exit(pid);
    }
}

fn poll_for_exit(pid: i32) {
    // SAFETY: signal 0 only checks that the process exists.
    while unsafe { libc::kill(pid, 0) } == 0 {
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn bundle_id_of(pid: i32) -> Option<String> {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .and_then(|a| a.bundleIdentifier())
        .map(|s| s.to_string())
}

fn scan_displays() -> ScanDisplays {
    let mut bounds = desktop::cg_display_bounds();
    let main = bounds.remove(0);
    let visible = VISIBLE
        .lock()
        .ok()
        .and_then(|v| *v)
        .filter(|v| main.contains_rect(v))
        .unwrap_or_else(|| Rect::new(main.x, main.y + MENU_BAR_GUESS, main.w, main.h - MENU_BAR_GUESS));
    ScanDisplays { main, visible, others: bounds }
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
