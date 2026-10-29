//! Safety: no window stays parked. A parked window that never comes back is the worst bug
//! this app can have, so there are seven layers (PLAN.md, "Safety"):
//!
//! 1. **Registry**: every parked window's AX element and (clamped) restore frame, kept in sync
//!    by the controller before and after each batch of actions.
//! 2. **Quit** (menu or `ctl quit`) unparks through the WM thread, falling back to the registry.
//! 3. **Panic hook**: outside the WM thread's guarded sections a panic unparks from the
//!    registry (`try_lock`; a recovery scan when it is busy) and exits.
//! 4. **Signals**: SIGTERM, SIGINT and SIGHUP go through a self-pipe to a watcher thread that
//!    quits cleanly; SIGABRT wakes the watcher for a short best-effort unpark, the handler
//!    itself doing only async-signal-safe things (a write, atomics, a bounded sleep).
//! 5. **Guardian** ([`super::guardian`]): `stage-manager --guardian <pid>`, spawned at launch,
//!    runs the recovery scan when the app dies without its clean-exit marker, then waits for
//!    every window still parked on a Space not shown and brings each back once its Space
//!    shows. It learns which windows to wait for from the parked file this module keeps
//!    beside the registry ([`paths::parked_file`]).
//! 6. **Recovery scan** (launch, guardian, panic fallback): every window of every allowed app
//!    whose frame is stranded in the park zone goes back to the frame a crashed copy
//!    registered for it (its parked file), else its `state.json` frame, else is centered
//!    ([`crate::engine::recovery`]).
//! 7. **Clamping**: every restore frame is clamped onto the visible display first.
//!
//! Quit counts as clean only when, after unparking, the window server lists no window of an
//! allowed app in the park zone on any Space ([`stranded_allowed`]). Every unpark path at
//! quit (the WM thread's, the registry's fallback, the signal watcher's) and a stand-down
//! ends with [`leftovers_back`]: each window still listed there is brought back, known to
//! this run or not (its registry frame, its `state.json` frame, else centered); one that
//! cannot be reached leaves the exit unclean, so the guardian waits for it (6X). The unpark
//! saves the stages and hands the check the frames it restored, so a window its app moved
//! late goes back to that frame, not an older one (S1). A move the
//! registry reports done is never taken for proof. An unpark's restores retry a busy app
//! quickly ([`set_frame_retrying`]), always within a deadline: the leftovers check never
//! takes more than [`leftovers::LEFTOVERS_MAX`], the registry's retries share
//! [`leftovers::REGISTRY_RETRY_BUDGET`]. Only windows we know ([`Ours`]) count as parked
//! when a thin sliver of them shows ([`Known`]). A hidden tab (8J, [`hidden_tabs`]) is never
//! waited for: macOS keeps it off screen at its last frame, out of Accessibility's reach,
//! until its tab is selected, when it takes its group's frame.
//!
//! **Taken windows** (round 8N, [`crate::engine::handoff`]): an old guardian still waiting when
//! this app runs hands its watch over ([`take_watch`]). The windows it took join the parked
//! file (so this app's guardian waits for them after a crash), count as ours at the quit's
//! leftovers check (brought back to their taken frame), and are watched on the WM thread
//! ([`super::handoff::TakenWatch`]) until each is back, gone, or in the model. A quit is clean
//! only once none is left ([`taken_len`]).
//!
//! **Hidden apps** (`[windows] hide_parked_apps`): every app we hid is recorded beside the
//! registry ([`set_hidden`]) and in the parked file, and every path that unparks unhides it
//! too ([`unhide_ours`]): quit, the panic hook, the signals and, from the parked file, the
//! guardian. An app is unhidden only while its pid still runs the same bundle id; an app the
//! user hid is never recorded, so never unhidden.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use objc2_app_kit::NSRunningApplication;

use super::ax::{AxElement, AxError, AxResult, names};
use super::{desktop, hidden_tabs, logging, observe, paths, windows};
use crate::engine::geometry::Rect;
use crate::engine::hidden_tab::Candidate;
use crate::engine::leftovers::{self, LEFTOVER_BUDGET, LEFTOVERS_MAX, REGISTRY_RETRY_BUDGET, RESTORE_RETRY, Remembered, UNPARK_POLL, UNPARK_SETTLE};
use crate::engine::park::Corner;
use crate::engine::persist::StateFile;
use crate::engine::recovery::{self, HiddenApp, Known, LiveWindow, ParkedFile, Registered, ScanDisplays};
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
    /// A shown window we fitted clear of the strip (not parked): its fitted frame (where we
    /// put it); `restore` is its exact pre-fit frame.
    pub fitted: Option<Rect>,
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

/// Windows taken from an old guardian (round 8N): no AX element yet (most sit on a Desktop
/// not shown), only what the guardian knew. Locked after [`REGISTRY`] when both are.
static TAKEN: Mutex<BTreeMap<u32, Registered>> = Mutex::new(BTreeMap::new());

fn lock_taken() -> MutexGuard<'static, BTreeMap<u32, Registered>> {
    TAKEN.lock().unwrap_or_else(|p| p.into_inner())
}

/// Takes an old guardian's watch ([`crate::engine::handoff::take`]): its windows join the
/// taken ones and the parked file, its park corners this run's. Returns every offered id
/// (all held now) and how many were new.
pub fn take_watch(windows: &[Registered], corners: &[Corner]) -> (Vec<u32>, usize) {
    let taken = crate::engine::handoff::take(&mut lock_taken(), windows);
    if let Ok(mut c) = CORNERS.lock() {
        c.extend(corners.iter().copied());
    }
    let listed = registered(&lock_registry());
    write_parked_file(&listed);
    taken
}

/// The windows taken from an old guardian, still waited for.
pub fn taken() -> Vec<Registered> {
    lock_taken().values().copied().collect()
}

/// How many windows taken from an old guardian are still waited for.
pub fn taken_len() -> usize {
    lock_taken().len()
}

/// The ids of the windows taken from an old guardian.
pub fn taken_ids() -> BTreeSet<u32> {
    lock_taken().keys().copied().collect()
}

/// A taken window's entry.
fn taken_entry(id: u32) -> Option<Registered> {
    lock_taken().get(&id).copied()
}

/// Windows the latest leftovers check left in the park zone that were not handed over (taken
/// from an old guardian): those keep a quit's error.
static LEFT_NOT_TAKEN: AtomicUsize = AtomicUsize::new(0);

/// How many windows the latest leftovers check left that were not handed over.
pub fn left_not_taken() -> usize {
    LEFT_NOT_TAKEN.load(Ordering::SeqCst)
}

/// Set by a quit whose only windows not back are handed-over ones still away: passed on to
/// this app's guardian, a warning rather than an error.
static PASSED_ON: AtomicBool = AtomicBool::new(false);

/// Notes whether this quit passes handed-over windows on (and nothing else is left).
pub fn set_passed_on(on: bool) {
    PASSED_ON.store(on, Ordering::SeqCst);
}

/// True when this quit passes handed-over windows on and nothing else is left.
pub fn passed_on() -> bool {
    PASSED_ON.load(Ordering::SeqCst)
}

/// Lets go of taken windows (back, gone, or in the model now).
pub fn taken_forget(ids: &[u32]) {
    if ids.is_empty() {
        return;
    }
    let changed = {
        let mut taken = lock_taken();
        let before = taken.len();
        taken.retain(|id, _| !ids.contains(id));
        taken.len() != before
    };
    if changed {
        let listed = registered(&lock_registry());
        write_parked_file(&listed);
    }
}

/// Windows a stand-down could not bring back (another Space's): owned by no controller, kept
/// in the registry through every [`registry_replace`] until they are back
/// ([`crate::engine::stand_down`]).
static HELD: Mutex<BTreeMap<u32, ParkedEntry>> = Mutex::new(BTreeMap::new());

fn lock_held() -> MutexGuard<'static, BTreeMap<u32, ParkedEntry>> {
    HELD.lock().unwrap_or_else(|p| p.into_inner())
}

/// Apps we hid, by pid, with their bundle ids.
static HIDDEN: Mutex<BTreeMap<i32, String>> = Mutex::new(BTreeMap::new());

fn hidden_now() -> Vec<HiddenApp> {
    HIDDEN.lock().map(|h| h.iter().map(|(&pid, b)| HiddenApp { pid, bundle: b.clone() }).collect()).unwrap_or_default()
}

/// Records exactly the apps we hid now (pid, bundle id), for every unpark path and the
/// guardian.
pub fn set_hidden(apps: BTreeMap<i32, String>) {
    let changed = HIDDEN.lock().map(|mut h| std::mem::replace(&mut *h, apps) != *h).unwrap_or(false);
    if changed {
        let listed = registered(&lock_registry());
        write_parked_file(&listed);
    }
}

/// Unhides every app we hid that still runs under the same bundle id, and forgets them;
/// returns how many were asked. Never waits on the lock longer than a try.
pub fn unhide_ours(reason: &str) -> usize {
    let apps = match HIDDEN.try_lock() {
        Ok(mut h) => std::mem::take(&mut *h),
        Err(_) => return 0,
    };
    let n = unhide_apps(reason, apps.into_iter().map(|(pid, bundle)| HiddenApp { pid, bundle }));
    let listed = registered(&lock_registry());
    write_parked_file(&listed);
    n
}

/// Unhides each app still running under its recorded bundle id (the guardian's, from the
/// parked file, too); returns how many were asked.
pub fn unhide_apps(reason: &str, apps: impl IntoIterator<Item = HiddenApp>) -> usize {
    let mut n = 0;
    for app in apps {
        let running = NSRunningApplication::runningApplicationWithProcessIdentifier(app.pid);
        let same = running.as_ref().and_then(|r| r.bundleIdentifier()).is_some_and(|b| b.to_string() == app.bundle);
        match running {
            Some(r) if same => {
                let ok = r.unhide();
                n += 1;
                tracing::info!(reason, pid = app.pid, bundle = app.bundle, ok, "safety: an app we hid is unhidden");
            }
            _ => tracing::debug!(reason, pid = app.pid, bundle = app.bundle, "safety: an app we hid is gone (or its pid is another app's)"),
        }
    }
    n
}

/// The main display's visible frame as last reported (for scans in this process).
static VISIBLE: Mutex<Option<Rect>> = Mutex::new(None);

/// Every park corner used this run (the guardian's: those the app's parked file names), so a
/// window parked at a corner the arrangement no longer picks still counts as parked.
static CORNERS: Mutex<BTreeSet<Corner>> = Mutex::new(BTreeSet::new());

/// Every park corner used this run (or named by the guardian's parked file).
pub(crate) fn corners_used() -> Vec<Corner> {
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

/// Replaces the registry with `entries` (after a batch), plus the held windows (a
/// stand-down's) that `entries` does not take over.
pub fn registry_replace(entries: BTreeMap<u32, ParkedEntry>) {
    let listed = {
        let mut held = lock_held();
        // A held window the controller parks itself is its own again.
        held.retain(|id, _| !entries.contains_key(id));
        let merged = crate::engine::stand_down::merge_held(entries, &held);
        drop(held);
        let mut guard = lock_registry();
        *guard = merged;
        registered(&guard)
    };
    write_parked_file(&listed);
}

/// A stand-down's leftovers: every window still in the registry is held until it is back.
/// Returns how many are held.
pub fn hold_remaining() -> usize {
    let remaining = lock_registry().clone();
    let mut held = lock_held();
    held.extend(remaining);
    held.len()
}

/// How many windows are held.
pub fn held_len() -> usize {
    lock_held().len()
}

/// Tries every held window again (its Space may show now); those back leave the hold and
/// the registry. True when none is left.
pub fn retry_held() -> bool {
    let held = lock_held().clone();
    if held.is_empty() {
        return true;
    }
    let mut back = Vec::new();
    for (id, entry) in &held {
        match entry.element.set_frame(entry.restore) {
            Ok(()) => back.push(*id),
            Err(e) => tracing::debug!(id, "safety: a held window cannot come back yet: {e}"),
        }
    }
    if !back.is_empty() {
        counters::bump(Count::Recovery);
        tracing::info!(back = back.len(), left = held.len() - back.len(), "safety: held windows brought back");
        lock_held().retain(|id, _| !back.contains(id));
        let listed = {
            let mut guard = lock_registry();
            guard.retain(|id, _| !back.contains(id));
            registered(&guard)
        };
        write_parked_file(&listed);
    }
    lock_held().is_empty()
}

/// The registry as the guardian needs it: ids, pids and restore frames; then the windows
/// taken from an old guardian that the registry does not hold (round 8N).
fn registered(map: &BTreeMap<u32, ParkedEntry>) -> Vec<Registered> {
    let mut listed: Vec<Registered> = map
        .iter()
        .map(|(&id, e)| Registered {
            id,
            pid: e.element.pid().unwrap_or(-1),
            restore: e.restore,
            child: e.child,
            fitted: e.fitted.is_some(),
            fitted_at: e.fitted,
            unvouched: false,
        })
        .collect();
    listed.extend(lock_taken().values().filter(|r| !map.contains_key(&r.id)).copied());
    listed
}

/// What the parked file holds now (only rewritten when it changes).
static PARKED_WRITTEN: Mutex<Option<ParkedFile>> = Mutex::new(None);

/// Keeps this process's parked file ([`paths::parked_file`]) equal to the registry, with the
/// main display's visible frame, written atomically and flushed to disk before the rename
/// ([`super::durable`]); removed when the registry is empty.
fn write_parked_file(listed: &[Registered]) {
    let hidden = hidden_now();
    let empty = listed.is_empty() && hidden.is_empty();
    let file = ParkedFile { visible: visible_frame(), windows: listed.to_vec(), corners: corners_used(), hidden };
    let Ok(mut written) = PARKED_WRITTEN.lock() else { return };
    if written.as_ref() == Some(&file) {
        return;
    }
    let path = paths::parked_file(std::process::id());
    let result = if empty {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        serde_json::to_vec(&file)
            .map_err(std::io::Error::other)
            .and_then(|bytes| super::durable::replace(&path, &bytes, super::durable::Flush::Fast))
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

/// The ids the registry holds.
pub fn registry_ids() -> BTreeSet<u32> {
    lock_registry().keys().copied().collect()
}

/// Drops the entries of windows seen back (quit's leftovers, [`leftovers_back`]), taken ones
/// too.
fn registry_forget(ids: &[u32]) {
    if ids.is_empty() {
        return;
    }
    lock_held().retain(|id, _| !ids.contains(id));
    lock_taken().retain(|id, _| !ids.contains(id));
    let listed = {
        let mut guard = lock_registry();
        guard.retain(|id, _| !ids.contains(id));
        registered(&guard)
    };
    write_parked_file(&listed);
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
/// `None` when it stayed busy. Entries that were restored leave the registry. A busy app is
/// tried again quickly ([`set_frame_retrying`]) while `retry_budget` lasts, all windows
/// together; then each window left gets its one try (the review of 6X, L1).
pub fn unpark_registry(wait: Duration, retry_budget: Duration) -> Option<Report> {
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
    let retries_end = Instant::now() + retry_budget;
    for (id, entry) in entries {
        report.checked += 1;
        match set_frame_retrying(&entry.element, id, entry.restore, retries_end.saturating_duration_since(Instant::now())) {
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
    // Restored held windows are done.
    let still: Vec<u32> = guard.keys().copied().collect();
    drop(guard);
    lock_held().retain(|id, _| still.contains(id));
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
    unhide_ours(reason);
    match unpark_registry(Duration::from_millis(200), REGISTRY_RETRY_BUDGET) {
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
            let stranded = stranded_ids(&failures.iter().map(|f| f.0).collect());
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

/// Those of `registered` (windows we registered) that sit stranded in the main display's
/// park zone now.
fn stranded_ids(registered: &BTreeSet<u32>) -> Vec<u32> {
    let displays = scan_displays();
    windows::list_windows()
        .iter()
        .filter(|w| w.layer == 0 && registered.contains(&w.id) && recovery::is_stranded(w.bounds, &displays, Known::Registered))
        .map(|w| w.id)
        .collect()
}

/// Whose windows the safety paths know as ours (the review of 6X, L3): the registry's, the
/// windows the model manages, and the windows `state.json` names (this session's ids). The
/// park test counts a sliver only for these ([`Known`]); an app's other windows are any
/// app's, so a user's window of that app pushed off the corner is left where it is at a quit,
/// a stand-down or a logout (S9 of the Fable review of 10-29). A leftovers check that could not bring every
/// window back hands what it knew to the next try ([`Self::kept_for_retry`]): a stand-down
/// drops `state.json` and the model before it looks again (the review of 6X2, 2).
#[derive(Debug, Clone, Default)]
pub struct Ours {
    /// Windows we registered (parked, fitted), and windows an earlier check judged ours and
    /// could not bring back.
    pub registered: BTreeSet<u32>,
    /// Windows the model manages.
    pub managed: BTreeSet<u32>,
    /// `state.json` as it was read: the frames a leftover goes back to.
    pub state: Option<StateFile>,
    /// Registered windows the model let go because Accessibility reported them destroyed
    /// while their app still listed them in the window server (a tab no longer selected; the
    /// review of 8J): the only registered windows the hidden-tab test may judge
    /// ([`Self::candidate`]). A window let go for any other reason (it left its Space, its app
    /// was hidden) stays real.
    pub destroyed: BTreeSet<u32>,
    /// Windows the model manages and has not parked: they vouch for a
    /// [`Candidate::Destroyed`] window's same size, on any Desktop.
    pub shown_managed: BTreeSet<u32>,
    /// The frames the controller's latest unpark restored windows to (S1): a window still
    /// in the park zone at the check goes back there before any `state.json` frame (its
    /// registry entry left as done with the batch).
    pub unparked: BTreeMap<u32, Rect>,
}

impl Ours {
    /// The registry now (the windows taken from an old guardian too, but for unvouched ones:
    /// any app's to the park test, R4), `managed` (the model's windows, when the caller has
    /// them) and `state.json` (its window ids and frames).
    pub fn now(managed: BTreeSet<u32>) -> Self {
        let mut registered = registry_ids();
        registered.extend(lock_taken().values().filter(|r| !r.unvouched).map(|r| r.id));
        Self::from_parts(registered, managed, read_state("safety"))
    }

    /// What is known from the registry's ids, the model's windows and `state.json`.
    pub fn from_parts(registered: BTreeSet<u32>, managed: BTreeSet<u32>, state: Option<StateFile>) -> Self {
        Self { registered, managed, state, destroyed: BTreeSet::new(), shown_managed: BTreeSet::new(), unparked: BTreeMap::new() }
    }

    /// `self`, with the frames the controller's latest unpark restored windows to
    /// ([`Self::unparked`]).
    pub fn with_unparked(mut self, unparked: BTreeMap<u32, Rect>) -> Self {
        self.unparked = unparked;
        self
    }

    /// `self`, with what the hidden-tab test needs from the model (8J): the registered
    /// windows it let go as reported destroyed ([`Self::destroyed`]) and its windows not
    /// parked ([`Self::shown_managed`]).
    pub fn with_tab_signs(mut self, destroyed: BTreeSet<u32>, shown_managed: BTreeSet<u32>) -> Self {
        self.destroyed = destroyed;
        self.shown_managed = shown_managed;
        self
    }

    /// What the hidden-tab test may judge of window `id` (8J; [`Candidate`]): one the model
    /// manages, or one we registered (or an earlier check left) and the model did not let go
    /// as reported destroyed, is real; a registered one reported destroyed may be a tab its
    /// shown selected tab vouches for; one we never registered nor manage, only beside a
    /// window Accessibility lists now.
    pub fn candidate(&self, id: u32) -> Candidate {
        if self.managed.contains(&id) {
            Candidate::Real
        } else if self.registered.contains(&id) {
            if self.destroyed.contains(&id) { Candidate::Destroyed } else { Candidate::Real }
        } else {
            Candidate::Unknown
        }
    }

    /// What a check that left `left` in the park zone hands its next try: everything it knew,
    /// and each window left judged registered (it was ours, in the zone; a sliver of it still
    /// counts, whatever the registry, the model and `state.json` say by then).
    pub fn kept_for_retry(mut self, left: &[u32]) -> Self {
        self.registered.extend(left.iter().copied());
        self
    }

    /// `self` (what an earlier check knew) with what is known `now`: every window either
    /// knows; `self`'s `state.json` first (read before a stand-down dropped it), else `now`'s.
    pub fn merged(mut self, now: Ours) -> Self {
        self.registered.extend(now.registered);
        self.managed.extend(now.managed);
        // Reported destroyed stays true; what the model shows is `now`'s alone (an earlier
        // check's model may have parked those windows since, or be gone).
        self.destroyed.extend(now.destroyed);
        self.shown_managed = now.shown_managed;
        // A later unpark's frame is the fresher one.
        self.unparked.extend(now.unparked);
        if self.state.is_none() {
            self.state = now.state;
        }
        self
    }

    /// True when `state.json` (this window server session's: [`read_state`] keeps saved ids only
    /// then) names window `id`: a window an earlier run parked may be one of them (round 8M).
    pub fn saved(&self, id: u32) -> bool {
        self.state.as_ref().is_some_and(|s| s.all_stages().flat_map(|st| st.windows.iter()).any(|w| w.window == Some(id)))
    }

    /// What is known of window `id`: registered; managed (the model's, or named by
    /// `state.json`); else any app's, whatever its app (S9).
    pub fn known(&self, id: u32) -> Known {
        if self.registered.contains(&id) {
            Known::Registered
        } else if self.managed.contains(&id) || self.saved(id) {
            Known::Managed
        } else {
            Known::Unknown
        }
    }
}

/// Windows of allowed apps (`STAGE_MANAGER_ONLY` honored, our own process and the system's
/// excluded apps skipped) the window server lists in the main display's park zone, on any
/// Space: windows on other Spaces are listed too, off screen, with their frames. Only real
/// candidates count: layer 0, not transparent, the frame in a park zone as far as what is
/// known of it allows ([`recovery::is_stranded`], [`Ours`]: a sliver only for ours, no
/// helper's tiny window unless we registered it).
///
/// A hidden tab ([`hidden_tabs`], 8J) is never one: nothing can move it, and it takes its
/// group's frame once selected. Only by its own signature ([`Ours::candidate`]): a registered
/// one the model let go as reported destroyed leaves the registry.
///
/// Nor is another app's off-screen window we never parked (round 8M,
/// [`crate::engine::not_ours`]: never registered nor managed, not saved in this session's
/// `state.json`, off screen, its app answering and not listing it through Accessibility):
/// logged at debug, never "still parked", so a quit is not unclean for it and the guardian
/// never waits on it. A registered window always counts.
pub fn stranded_allowed(ours: &Ours) -> Vec<windows::CgWindow> {
    let only = observe::only_from_env();
    let own = std::process::id() as i32;
    let displays = scan_displays();
    let mut bundles: std::collections::HashMap<i32, Option<String>> = std::collections::HashMap::new();
    let list = windows::list_windows();
    let mut tabs = hidden_tabs::Tabs::new(&list);
    let mut registered_tabs = Vec::new();
    let left = list
        .into_iter()
        // A transparent window is no window we parked (helpers keep such windows anywhere).
        .filter(|w| w.layer == 0 && w.alpha > 0.0 && displays.parks(w.bounds))
        .filter(|w| {
            let bundle = bundles.entry(w.pid).or_insert_with(|| bundle_id_of(w.pid));
            observe::app_allowed(only.as_ref(), own, w.pid, bundle.as_deref())
                && recovery::is_stranded(w.bounds, &displays, ours.known(w.id))
        })
        .filter(|w| {
            let tab = tabs.is_hidden_tab(w, ours.candidate(w.id), |id| ours.shown_managed.contains(&id));
            if tab && ours.registered.contains(&w.id) {
                registered_tabs.push(w.id);
            }
            !tab && !tabs.is_not_ours(w, ours.candidate(w.id), ours.saved(w.id))
        })
        .collect();
    registry_forget(&registered_tabs);
    left
}

/// The apps (process ids) of allowed apps with a window listed now, on any Space: those a
/// display change's sweep scans ([`crate::engine::rescan`]).
pub fn scan_pids() -> Vec<i32> {
    let only = observe::only_from_env();
    let own = std::process::id() as i32;
    let mut pids: Vec<i32> = windows::list_windows().iter().filter(|w| w.layer == 0).map(|w| w.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    pids.retain(|&pid| observe::app_allowed(only.as_ref(), own, pid, bundle_id_of(pid).as_deref()));
    pids
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
/// Set by the signal watcher once its unpark for an abort is done.
static ABORT_UNPARKED: AtomicBool = AtomicBool::new(false);

/// How long the SIGABRT handler waits for the signal watcher's unpark before it lets the
/// abort finish (the guardian covers whatever is left).
const ABORT_WAIT: Duration = Duration::from_millis(1500);
/// The handler's wait step.
const ABORT_STEP: Duration = Duration::from_millis(10);

extern "C" fn on_signal(sig: libc::c_int) {
    let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = sig as u8;
        // SAFETY: write(2) is async-signal-safe; `byte` lives for the call.
        unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
    }
}

/// SIGABRT (a double panic, a C abort): async-signal-safe only (Fable M3, Grok High 3). It
/// never locks, allocates, logs or calls Accessibility, which could deadlock on a lock the
/// aborting thread holds and keep a dying process alive (answering `ping`, blocking every
/// relaunch, the guardian never woken). It wakes the signal watcher through the self-pipe,
/// waits a bounded moment for its unpark ([`abort_wait`]: atomics and `nanosleep` only),
/// then finishes the abort; the guardian takes over at the exit.
extern "C" fn on_abort(sig: libc::c_int) {
    if !ABORTING.swap(true, Ordering::SeqCst) && waits_for_watcher(SIGNAL_PIPE.load(Ordering::SeqCst)) {
        on_signal(sig);
        abort_wait(|| ABORT_UNPARKED.load(Ordering::SeqCst), ABORT_WAIT, ABORT_STEP, nanosleep);
    }
    // SAFETY: restoring the default action and re-raising is the documented way to finish
    // an abort from a handler; both are async-signal-safe.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// Whether an abort waits for the signal watcher: only when it runs (`pipe`, the watcher's
/// self-pipe, is set once it started and cleared if it stops; the review of 8A, L3).
/// Otherwise nothing would set the flag the handler waits for.
fn waits_for_watcher(pipe: libc::c_int) -> bool {
    pipe >= 0
}

/// Waits until `done` or `max` has passed, in `step`s through `sleep` (counted, never a
/// clock read: what a signal handler may do). True when `done`.
fn abort_wait(done: impl Fn() -> bool, max: Duration, step: Duration, mut sleep: impl FnMut(Duration)) -> bool {
    let steps = (max.as_nanos() / step.as_nanos().max(1)).max(1);
    for _ in 0..steps {
        if done() {
            return true;
        }
        sleep(step);
    }
    done()
}

/// `nanosleep(2)`, which is async-signal-safe (`std::thread::sleep` makes no such promise).
fn nanosleep(d: Duration) {
    let ts = libc::timespec { tv_sec: d.as_secs() as libc::time_t, tv_nsec: libc::c_long::from(d.subsec_nanos() as i32) };
    // SAFETY: a valid timespec; the remainder is not wanted.
    unsafe { libc::nanosleep(&ts, std::ptr::null_mut()) };
}

/// The signal watcher's side of an abort (an ordinary thread, so it may lock and log): the
/// registry's best-effort unpark, a short one; then the handler may finish the abort. A lock
/// held by the aborting thread only makes this wait out the handler's bound.
fn unpark_for_abort() {
    let r = unpark_registry(Duration::from_millis(100), Duration::ZERO);
    ABORT_UNPARKED.store(true, Ordering::SeqCst);
    tracing::error!(restored = r.map(|r| r.restored), failed = r.map(|r| r.failed), "SIGABRT: the registry unparked from the signal watcher; the guardian covers the rest");
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
/// `on_quit(signal)`; SIGABRT goes through the same pipe to the watcher's short unpark while
/// the handler waits a bounded moment ([`on_abort`]).
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
    std::thread::Builder::new().name("signals".into()).spawn(move || {
        loop {
            let mut byte = 0u8;
            // SAFETY: reading one byte into a valid buffer.
            let n = unsafe { libc::read(read_fd, (&mut byte as *mut u8).cast(), 1) };
            if n == 1 && i32::from(byte) == libc::SIGABRT {
                unpark_for_abort();
            } else if n == 1 {
                tracing::info!(signal = byte, "signal received; quitting");
                on_quit(i32::from(byte));
            } else if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            } else {
                break;
            }
        }
        // No watcher from here on: an abort no longer waits for one.
        SIGNAL_PIPE.store(-1, Ordering::SeqCst);
        tracing::warn!("the signal watcher stopped");
    })?;
    // Set once the watcher runs ([`waits_for_watcher`]).
    SIGNAL_PIPE.store(write_fd, Ordering::SeqCst);
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

/// Which windows a recovery scan looks at.
#[derive(Debug, Clone, Default)]
pub struct ScanScope {
    /// Only these apps' windows (`None`: every allowed app's).
    pub pids: Option<Vec<i32>>,
    /// Windows left alone: those we park on purpose (the registry's), while the app runs.
    pub skip: BTreeSet<u32>,
    /// A rescan's pass, every few seconds while an app does not answer (8M): the summary line
    /// at debug unless a window was brought back or could not be (the caller logs each app's
    /// episode of not answering once).
    pub quiet: bool,
    /// The windows a crashed copy registered (its parked file, [`crashed_registered`]), by
    /// window id: one still in the park zone goes back to its registered frame, the freshest
    /// and exact one, before any `state.json` frame (S4). Used only for a window of the very
    /// process that registered it.
    pub registered: BTreeMap<u32, Registered>,
}

/// The recovery scan: every window of every allowed app (`STAGE_MANAGER_ONLY` honored, our
/// own process and the system's excluded apps skipped) that is stranded in the park zone
/// goes back to its registered frame ([`ScanScope::registered`]), else its `state.json`
/// frame, else is centered.
pub fn recovery_scan(reason: &str) -> Report {
    recovery_scan_with(reason, &ScanScope::default()).0
}

/// [`recovery_scan`] over `scope`; also returns the apps that did not answer (their windows
/// could not be read: an app busy after a display change answers `CannotComplete`), so the
/// caller can scan them again.
pub fn recovery_scan_with(reason: &str, scope: &ScanScope) -> (Report, Vec<i32>) {
    let only = observe::only_from_env();
    let own = std::process::id() as i32;
    let mut pids: Vec<i32> = windows::list_windows().iter().filter(|w| w.layer == 0).map(|w| w.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    if let Some(wanted) = &scope.pids {
        pids.retain(|p| wanted.contains(p));
    }
    let state = read_state("recovery scan");
    let displays = scan_displays();
    let mut live = Vec::new();
    let mut elements = Vec::new();
    let mut unanswered = Vec::new();
    let mut registered = BTreeMap::new();
    for pid in pids {
        let bundle = bundle_id_of(pid);
        if !observe::app_allowed(only.as_ref(), own, pid, bundle.as_deref()) {
            continue;
        }
        let list = match AxElement::application(pid).windows() {
            Ok(list) => list,
            Err(e) => {
                if e != AxError::NO_VALUE {
                    tracing::debug!(reason, pid, bundle, "recovery scan: the app does not answer: {e}");
                    unanswered.push(pid);
                }
                continue;
            }
        };
        for w in list {
            if w.role().ok().as_deref() != Some(names::ROLE_WINDOW) {
                continue;
            }
            let id = w.window_id().ok();
            if id.is_some_and(|id| scope.skip.contains(&id)) {
                continue;
            }
            let Ok(frame) = w.frame() else { continue };
            // A window id names one window in this session; its process makes sure.
            if let Some(r) = id.and_then(|id| scope.registered.get(&id)).filter(|r| r.pid == pid) {
                registered.insert(r.id, r.restore);
            }
            live.push(LiveWindow { id, bundle_id: bundle.clone().unwrap_or_default(), title: w.title().unwrap_or_default(), frame });
            elements.push(w);
        }
    }
    let mut report = Report { checked: live.len(), ..Report::default() };
    for (i, frame, source) in recovery::plan_with(&live, &registered, state.as_ref(), &displays) {
        match elements[i].set_frame(frame) {
            Ok(()) => {
                report.restored += 1;
                tracing::warn!(reason, id = live[i].id, bundle = live[i].bundle_id, from = %live[i].frame, to = %frame, source = source.name(), "recovery: window brought back");
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
    if scope.quiet && report.restored == 0 && report.failed == 0 {
        tracing::debug!(reason, checked = report.checked, unanswered = ?unanswered, "recovery scan");
    } else if unanswered.is_empty() {
        tracing::info!(reason, checked = report.checked, restored = report.restored, failed = report.failed, "recovery scan");
    } else {
        tracing::info!(reason, checked = report.checked, restored = report.restored, failed = report.failed, unanswered = ?unanswered, "recovery scan (some apps did not answer)");
    }
    (report, unanswered)
}

/// The windows earlier copies of the app registered and left behind (their parked files, an
/// exit that was not clean: a crash, a kill), this process's own file aside: what the launch
/// scan brings a window still parked back to (S4). The window ids are checked against the
/// process that registered them by the scan ([`ScanScope::registered`]).
pub fn crashed_registered() -> BTreeMap<u32, Registered> {
    let own = std::process::id();
    let mut found = BTreeMap::new();
    let Ok(dir) = std::fs::read_dir(paths::config_dir()) else { return found };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(parked_file_pid).filter(|&p| p != own) else { continue };
        for r in read_parked_file(pid).windows {
            found.insert(r.id, r);
        }
    }
    found
}

/// The process id a parked file's name ([`paths::parked_file`]) stands for.
fn parked_file_pid(name: &str) -> Option<u32> {
    name.strip_prefix(".parked-")?.strip_suffix(".json")?.parse().ok()
}

/// The window ids this window server session's `state.json` names (none when it cannot be
/// read, or was written in another session): windows an earlier run may have parked (round 8M,
/// [`crate::engine::not_ours`]).
pub fn saved_window_ids() -> BTreeSet<u32> {
    read_state("saved window ids")
        .map(|s| s.all_stages().flat_map(|st| st.windows.iter()).filter_map(|w| w.window).collect())
        .unwrap_or_default()
}

/// `state.json`, when it can be read; its window ids only when it was written in this
/// window server session ([`StateFile::for_session`]: after a reboot or a new login an id
/// names another window), so the scans match by id first only when the id still means
/// the same window.
fn read_state(what: &str) -> Option<StateFile> {
    let text = std::fs::read_to_string(paths::state_file()).ok()?;
    match StateFile::from_json(&text) {
        Ok(s) => Some(s.for_session(super::session::window_session()).0),
        Err(e) => {
            tracing::warn!("{what}: {e}");
            None
        }
    }
}

/// Window `id` of `pid` through Accessibility; the reason when it cannot be reached.
pub(crate) fn ax_window(pid: i32, id: u32) -> Result<AxElement, String> {
    let list = AxElement::application(pid).windows().map_err(|e| format!("its app does not answer ({e})"))?;
    list.into_iter()
        .find(|w| w.window_id().ok() == Some(id))
        .ok_or_else(|| "its app does not list it through Accessibility (another Desktop's?)".to_string())
}

/// Moves window `id` (`element`) to `frame`; while its app is too busy to answer
/// (`CannotComplete`), tries again quickly and counts the move done once the window server
/// lists the window out of the park zone ([`leftovers::retry_quickly`], [`RESTORE_RETRY`]),
/// never past `left` (a caller's deadline: no retry, and no wait, that would end after it;
/// the first try is always made). For the restores of an unpark (quit, stand-down, the
/// registry's fallback, quit's leftovers): windows we parked, or one known to be parked.
pub fn set_frame_retrying(element: &AxElement, id: u32, frame: Rect, left: Duration) -> AxResult<()> {
    let start = Instant::now();
    let mut displays: Option<ScanDisplays> = None;
    let r = leftovers::retry_quickly(
        RESTORE_RETRY.within(left),
        || element.set_frame(frame),
        |e| *e == AxError::CANNOT_COMPLETE,
        || {
            let d = displays.get_or_insert_with(scan_displays);
            // Ours: still a sliver in the corner is not back.
            windows::describe(&[id]).first().is_some_and(|w| !recovery::is_stranded(w.bounds, d, Known::Registered))
        },
        || start.elapsed(),
        std::thread::sleep,
    );
    let ms = start.elapsed().as_millis() as u64;
    match &r.result {
        Ok(()) if r.landed_late => tracing::info!(id, tries = r.tries, ms, "a restore the app was too busy to answer went through late"),
        Ok(()) if r.tries > 1 => tracing::info!(id, tries = r.tries, ms, "a restore went through on a quick retry"),
        Err(e) if r.tries > 1 => tracing::debug!(id, tries = r.tries, ms, "a restore failed on every quick retry: {e}"),
        _ => {}
    }
    r.result
}

/// Every unpark path's last look (6X): reads the window server's list until no window of an
/// allowed app is left in the park zone, or [`UNPARK_SETTLE`] has passed; brings back each
/// one still there ([`bring_back_leftovers`]) and reads again. True when none is left; else
/// each one is logged and the exit must not count as clean (the guardian waits for them).
/// `managed`: the model's windows, when the caller has them ([`Ours`]). Never takes more
/// than [`leftovers::LEFTOVERS_MAX`]: the second read gets what is left.
pub fn leftovers_back(reason: &str, managed: BTreeSet<u32>) -> bool {
    leftovers_back_knowing(reason, Ours::now(managed)).is_none()
}

/// [`leftovers_back`] with what is known of the windows given (`ours`: an earlier check's,
/// [`Ours::merged`] with now). `None` when no window is left in the park zone; else what this
/// check knew, the windows left judged ours ([`Ours::kept_for_retry`]), for the next try:
/// a stand-down's retries pass it until each window is back (the review of 6X2, 2).
pub fn leftovers_back_knowing(reason: &str, ours: Ours) -> Option<Ours> {
    let start = Instant::now();
    let end = start + LEFTOVERS_MAX;
    let read = |max: Duration| {
        let began = Instant::now();
        leftovers::settle(|| stranded_allowed(&ours), max, UNPARK_POLL, || began.elapsed(), std::thread::sleep)
    };
    let left = read(UNPARK_SETTLE);
    if left.is_empty() {
        LEFT_NOT_TAKEN.store(0, Ordering::SeqCst);
        return None;
    }
    let seen: Vec<(u32, bool)> = left.iter().map(|w| (w.id, w.onscreen)).collect();
    tracing::warn!(reason, windows = ?seen, "windows still parked after unparking (id, on screen); bringing each back");
    // The second read keeps its full settle unless the first ran long.
    let budget_end = (Instant::now() + LEFTOVER_BUDGET).min(end.checked_sub(UNPARK_SETTLE).unwrap_or(end));
    let unreached = bring_back_leftovers(reason, &left, budget_end, &ours);
    let after = read(UNPARK_SETTLE.min(end.saturating_duration_since(Instant::now())));
    let took_ms = start.elapsed().as_millis() as u64;
    if after.is_empty() {
        tracing::info!(reason, back = left.len(), took_ms, "every window still parked after unparking is back");
        LEFT_NOT_TAKEN.store(0, Ordering::SeqCst);
        return None;
    }
    let ids: Vec<u32> = after.iter().map(|w| w.id).collect();
    let taken = taken_ids();
    let mine: Vec<u32> = ids.iter().copied().filter(|id| !taken.contains(id)).collect();
    LEFT_NOT_TAKEN.store(mine.len(), Ordering::SeqCst);
    if mine.is_empty() {
        // Only windows handed over by an old guardian (round 8N): passed on, never an error.
        tracing::info!(reason, windows = ?ids, took_ms, "handed-over windows still parked: passed on to the guardian");
    } else {
        tracing::error!(reason, windows = ?ids, unreached = ?unreached, took_ms, "windows still parked: the guardian brings each back once its Desktop shows or its app answers");
    }
    Some(ours.kept_for_retry(&ids))
}

/// Brings back every window in `left` (listed in the park zone after an unpark) before
/// `deadline`: to its registry frame, else the frame the unpark restored it to
/// ([`Ours::unparked`]), else the frame it was taken with from an old guardian, else its
/// `state.json` frame, else centered ([`leftovers::leftover_frame`]), through the
/// registry's element or the app's own list.
/// A window is tried only when its try fits in the time left ([`leftovers::leftover_fits`]),
/// and its quick retries stop at the deadline. Returns the ids it could not move (another
/// Desktop's, an app that does not answer, no time left).
pub fn bring_back_leftovers(reason: &str, left: &[windows::CgWindow], deadline: Instant, ours: &Ours) -> Vec<u32> {
    let displays = scan_displays();
    let state = ours.state.as_ref();
    let mut back = Vec::new();
    let mut unreached = Vec::new();
    for w in left {
        let bundle = bundle_id_of(w.pid);
        let entry = registry_entry(w.id);
        if !leftovers::leftover_fits(deadline.saturating_duration_since(Instant::now()), entry.is_some()) {
            tracing::warn!(reason, id = w.id, pid = w.pid, bundle, "a window still parked: no time left to bring it back");
            unreached.push(w.id);
            continue;
        }
        let element = match &entry {
            Some(e) => Ok(e.element.clone()),
            None => ax_window(w.pid, w.id),
        };
        let element = match element {
            Ok(e) => e,
            Err(why) => {
                tracing::warn!(reason, id = w.id, pid = w.pid, bundle, at = %w.bounds, onscreen = w.onscreen, "a window still parked cannot be reached: {why}");
                unreached.push(w.id);
                continue;
            }
        };
        let remembered = Remembered {
            registry: entry.as_ref().map(|e| e.restore),
            unparked: ours.unparked.get(&w.id).copied(),
            taken: taken_entry(w.id).map(|r| r.restore),
        };
        // The title only matters for a state.json match (no frame remembered).
        let title = if remembered.first().is_none() { element.title().unwrap_or_default() } else { String::new() };
        let live = LiveWindow { id: Some(w.id), bundle_id: bundle.clone().unwrap_or_default(), title, frame: w.bounds };
        let (frame, source) = leftovers::leftover_frame(remembered, &live, state, &displays);
        let known = ours.known(w.id);
        let mut result = set_frame_retrying(&element, w.id, frame, deadline.saturating_duration_since(Instant::now()));
        // The registry's element went stale (the window was reported destroyed while ordered
        // out, its app relaunched it): the app's own list may have it, when that and a try
        // still fit (the review of 6X2, 1).
        if entry.is_some()
            && result == Err(AxError::INVALID_ELEMENT)
            && leftovers::leftover_fits(deadline.saturating_duration_since(Instant::now()), false)
        {
            match ax_window(w.pid, w.id) {
                Ok(fresh) => {
                    tracing::info!(reason, id = w.id, pid = w.pid, "a window still parked: the registry's element is stale, trying the app's own");
                    result = set_frame_retrying(&fresh, w.id, frame, deadline.saturating_duration_since(Instant::now()));
                }
                Err(why) => tracing::debug!(reason, id = w.id, "a stale registry element's lookup: {why}"),
            }
        }
        match result {
            Ok(()) => {
                tracing::warn!(reason, id = w.id, pid = w.pid, bundle, ?known, from = %w.bounds, to = %frame, source = source.name(), "a window still parked after unparking is brought back");
                back.push(w.id);
            }
            Err(e) => {
                tracing::warn!(reason, id = w.id, pid = w.pid, bundle, at = %w.bounds, onscreen = w.onscreen, "a window still parked cannot be moved: {e}");
                unreached.push(w.id);
            }
        }
    }
    if !back.is_empty() {
        counters::bump(Count::Recovery);
    }
    registry_forget(&back);
    unreached
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

    /// Fable M2: a helper thread's body runs guarded, so its panic is caught on that thread
    /// (the panic hook neither unparks nor exits) and the feature degrades instead.
    #[test]
    fn a_guarded_thread_survives_its_panic() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let joined = std::thread::spawn(|| {
            let inside = guarded("a helper", || GUARDED.with(Cell::get));
            let after = guarded("a helper", || -> u32 { panic!("boom") });
            (inside, after, GUARDED.with(Cell::get))
        })
        .join();
        std::panic::set_hook(prev);
        assert_eq!(joined.ok(), Some((Some(true), None, false)));
    }

    /// The review of 8A, L3: no watcher, no wait.
    #[test]
    fn an_abort_waits_only_for_a_running_watcher() {
        assert!(!waits_for_watcher(-1), "never started, or stopped");
        assert!(waits_for_watcher(7));
    }

    /// Fable M3: the abort handler's wait is bounded, whether the watcher answers or not.
    #[test]
    fn the_abort_wait_is_bounded() {
        let slept = std::cell::Cell::new(Duration::ZERO);
        let sleep = |d: Duration| slept.set(slept.get() + d);
        assert!(!abort_wait(|| false, ABORT_WAIT, ABORT_STEP, sleep), "never done: gives up");
        assert_eq!(slept.get(), ABORT_WAIT, "after the bound, not later");
        slept.set(Duration::ZERO);
        let calls = std::cell::Cell::new(0);
        let done = || {
            calls.set(calls.get() + 1);
            calls.get() > 3
        };
        assert!(abort_wait(done, ABORT_WAIT, ABORT_STEP, sleep));
        assert_eq!(slept.get(), ABORT_STEP * 3, "returns as soon as the unpark is done");
        nanosleep(Duration::from_millis(1));
    }

    /// 8J and its review: what the hidden-tab test may judge. A window the model manages,
    /// or one we registered (or an earlier check left) is real, unless the model let it go
    /// as reported destroyed; one found in the park zone is unknown. Only the model's windows
    /// not parked vouch beyond Accessibility; saved ids never do.
    #[test]
    fn only_a_window_reported_destroyed_or_never_known_may_be_a_hidden_tab() {
        let ours = Ours::from_parts([1, 2].into(), [3, 6].into(), None).with_tab_signs([2].into(), [6].into());
        assert_eq!(ours.candidate(1), Candidate::Real, "registered and tracked (or let go as it left its Space)");
        assert_eq!(ours.candidate(2), Candidate::Destroyed, "registered, let go as reported destroyed");
        assert_eq!(ours.candidate(3), Candidate::Real, "managed");
        assert_eq!(ours.candidate(4), Candidate::Unknown, "found in the park zone");
        let kept = ours.clone().kept_for_retry(&[4]);
        assert_eq!(kept.candidate(4), Candidate::Real, "an earlier check's leftover stays real");
        let merged = ours.clone().merged(Ours::default());
        assert_eq!(merged.candidate(2), Candidate::Destroyed, "a merge keeps what was reported destroyed");
        assert!(merged.shown_managed.is_empty(), "what the model shows is now's alone");
        let merged = Ours::default().merged(ours);
        assert_eq!(merged.shown_managed, [6].into());
    }

    /// The review of 8J, High: a parked window the model let go because it left its Space
    /// (dragged to another Desktop in Mission Control) stays registered and not reported
    /// destroyed: real at the quit's check, whatever its same-size sibling says.
    #[test]
    fn a_parked_window_that_left_its_space_stays_real_at_the_quit() {
        let ours = Ours::from_parts([1].into(), [2].into(), None).with_tab_signs(BTreeSet::new(), [2].into());
        assert_eq!(ours.candidate(1), Candidate::Real);
    }

    /// S1 (the Fable review of 10-29): the frames an unpark restored reach the leftovers
    /// check, and a stand-down's reach the quit's retry after it; a later unpark's frame wins.
    #[test]
    fn the_unparks_frames_reach_the_leftovers_check() {
        let (a, b) = (Rect::new(10.0, 40.0, 300.0, 200.0), Rect::new(50.0, 60.0, 300.0, 200.0));
        let stand_down = Ours::default().with_unparked(BTreeMap::from([(1, a), (2, a)]));
        let quit = Ours::default().with_unparked(BTreeMap::from([(2, b)]));
        let kept = stand_down.kept_for_retry(&[1]).merged(quit);
        assert_eq!(kept.unparked, BTreeMap::from([(1, a), (2, b)]));
    }

    /// S9 (the Fable review of 10-29): at a quit, a stand-down or a logout a sliver counts as
    /// parked only for a window known by its id (registered, the model's, named by this
    /// session's `state.json`), no longer for any window of an app with one saved: the
    /// user's other window of that app, pushed off the corner, stays where they put it.
    #[test]
    fn a_sliver_is_ours_only_by_its_window_id() {
        use crate::engine::persist::{SavedSpace, SavedStage, SavedWindow};
        let saved = SavedWindow { bundle_id: "com.example.mail".into(), title: "Inbox".into(), window: Some(41), ..Default::default() };
        let state = StateFile::single(SavedSpace { stages: vec![SavedStage { windows: vec![saved], ..Default::default() }], ..Default::default() });
        let ours = Ours::from_parts([1].into(), [2].into(), Some(state));
        assert_eq!(ours.known(1), Known::Registered);
        assert_eq!(ours.known(2), Known::Managed);
        assert_eq!(ours.known(41), Known::Managed, "named by state.json");
        assert_eq!(ours.known(42), Known::Unknown, "another window of the same app");
        let d = ScanDisplays::new(Rect::new(0.0, 0.0, 1440.0, 900.0), Rect::new(0.0, 25.0, 1440.0, 875.0), vec![], [Corner::BottomRight]);
        let sliver = Rect::new(1439.0, 820.0, 600.0, 392.0);
        assert!(recovery::is_stranded(sliver, &d, ours.known(41)));
        assert!(!recovery::is_stranded(sliver, &d, ours.known(42)));
    }

    /// S4: a crashed copy's parked file is found by its name, this process's own aside.
    #[test]
    fn a_parked_files_name_says_whose_it_is() {
        let name = paths::parked_file(4242).file_name().and_then(|n| n.to_str().map(str::to_string)).unwrap_or_default();
        assert_eq!(parked_file_pid(&name), Some(4242));
        assert_eq!(parked_file_pid(".clean-exit-4242"), None);
        assert_eq!(parked_file_pid(".parked-x.json"), None);
        assert_eq!(parked_file_pid("state.json"), None);
    }

    #[test]
    fn empty_registry_unparks_nothing() {
        let r = unpark_registry(Duration::from_millis(10), Duration::ZERO).unwrap_or_default();
        assert_eq!(r.failed, 0);
    }
}
