//! Observation on the WM thread: which apps are observed, which windows are manageable, and
//! the AX notification handlers that keep both current.
//!
//! A *manageable* window is an AX element with role `AXWindow`, a `CGWindowID`, not
//! fullscreen, on layer 0. Subrole `AXStandardWindow` is [`WindowKind::Standard`]; any other
//! subrole is [`WindowKind::Child`], except a window that is or was minimized, which is
//! standard whatever its subrole says ([`kind_of`]). Windows are deduplicated by window id.
//!
//! The [`Observer`] only tracks; it reports what changed as [`WinEvent`]s, which the
//! controller drains with [`Observer::take_events`] and turns into engine operations.
//!
//! **Spaces.** `AXWindows` lists only the current Space's windows for most apps (measured
//! 10-12 on macOS 27: Safari, Mail, VS Code, Fork, Terminal, Chrome, Messages, Telegram);
//! some apps list every Space's. So the windows of another Space are found when the user
//! goes there ([`Observer::rescan_all`] on a Space change), and a tracked window that goes
//! to another Space stays tracked (no destroy notification comes). Whether a window is on
//! the current Space is the window server's on-screen flag, read by the controller.
//!
//! **Closed while its app did not answer** (round 8M, [`crate::engine::vanished`]): an app that
//! stops answering Accessibility sends no destroyed notification for a window the user closes
//! (Home, 10-21). About once a second ([`Observer::sweep_closed`]) the tracked windows are
//! looked up in the window server's complete list (every Desktop, minimized and hidden
//! windows included); one missing from it on two looks apart is reported destroyed.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::Instant;

use objc2_application_services::{AXObserver, AXUIElement};
use objc2_core_foundation::CFString;
use serde::Serialize;
use serde_json::json;

use super::apps::{AppEvent, AppInfo};
use super::ax::{AxElement, AxError, AxObserver, AxResult, names, notifications as n};
use super::logging;
use super::native_settings::RawSettings;
use super::windows::{self, CgWindow};
use crate::engine::busy_apps::Reply;
use crate::engine::geometry::Rect;
use crate::engine::vanished::Vanished;

/// How often an app whose AX is not ready yet (just launched) is retried, one per retry pass.
const MAX_ATTEMPTS: u32 = 10;

/// Test only (round 8M): with `STAGE_MANAGER_TEST` set too, any value ignores Accessibility's
/// destroyed notifications, as an app that does not answer sends none (Home, 10-21), so a
/// scripted check sees a closed window leave through [`Observer::sweep_closed`] alone.
pub const DROP_DESTROYED_ENV: &str = "STAGE_MANAGER_DROP_AX_DESTROYED";

fn drop_destroyed() -> bool {
    static DROP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DROP.get_or_init(|| std::env::var_os("STAGE_MANAGER_TEST").is_some() && std::env::var_os(DROP_DESTROYED_ENV).is_some())
}

/// Standard window or child (sheet, dialog, panel, popover…).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowKind {
    Standard,
    Child,
}

impl WindowKind {
    /// Classifies an `AXSubrole`.
    pub fn from_subrole(subrole: &str) -> Self {
        if subrole == names::SUBROLE_STANDARD { Self::Standard } else { Self::Child }
    }
}

/// A window's kind from its subrole and whether it is (or was) minimized. Only a standard
/// window can be minimized (a sheet, dialog or panel has no minimize button), so a minimized
/// one is standard whatever its subrole says: measured 10-15 on macOS 27, a window reopened
/// minimized reads `AXDialog` while in the Dock and may still read it during the un-minimize
/// animation, when it is placed. A window never minimized is classified by its subrole.
pub fn kind_of(subrole: &str, minimized: bool) -> WindowKind {
    if minimized { WindowKind::Standard } else { WindowKind::from_subrole(subrole) }
}

/// A manageable window being observed.
#[derive(Clone, Debug)]
pub struct TrackedWindow {
    pub id: u32,
    pub pid: i32,
    pub element: AxElement,
    pub subrole: String,
    pub kind: WindowKind,
    pub title: String,
    pub frame: Rect,
    /// Minimized when first read, or since (`AXWindowMiniaturized`); never cleared: a window
    /// once minimized is a standard window ([`kind_of`]).
    pub minimized: bool,
}

/// What changed, for the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinEvent {
    /// Observation started; every window found so far is tracked (no `Found` for them).
    Started,
    /// Observation stopped (Accessibility revoked); every window was dropped.
    Stopped,
    /// A window appeared (created, or found when its app became observable).
    Found(u32),
    /// A window is gone (destroyed, or its app quit).
    Destroyed(u32),
    /// A window moved or resized; its tracked frame is current.
    Moved(u32),
    /// A window's title changed; its tracked title is current.
    Retitled(u32),
    /// A window got focus.
    Focused(u32),
    /// An observed app became frontmost (NSWorkspace; its notice's instant:
    /// [`Observer::activated_at`]).
    AppActivated(i32),
    /// An observed app was hidden (NSWorkspace).
    AppHidden(i32),
    /// An observed app was unhidden (NSWorkspace).
    AppUnhidden(i32),
    /// An app quit (NSWorkspace; its windows' `Destroyed` come with it).
    AppTerminated(i32),
    /// A window was minimized (⌘M, the yellow button).
    Minimized(u32),
}

struct ObservedApp {
    element: AxElement,
    observer: AxObserver,
}

/// What reading a window taken for a child again makes of it ([`Observer::recheck_kind`]).
/// A standard window is never asked (one read off screen was read right). A window that was
/// minimized (`was_minimized`: when first read, or since) or is minimized now
/// (`minimized_now`) is standard whatever its subrole ([`kind_of`]); any other is classified
/// by the subrole read now. `Some((subrole, kind))` with the subrole read now (`None` when it
/// did not answer) and the kind; `None` when the kind stays (standard, or no answer).
pub fn reread_kind(
    kind: WindowKind,
    was_minimized: bool,
    minimized_now: impl FnOnce() -> AxResult<bool>,
    read: impl FnOnce() -> AxResult<String>,
) -> Option<(Option<String>, WindowKind)> {
    if kind == WindowKind::Standard {
        return None;
    }
    let subrole = read().ok();
    if was_minimized {
        return Some((subrole, WindowKind::Standard));
    }
    if subrole.as_deref() == Some(names::SUBROLE_STANDARD) {
        return Some((subrole, WindowKind::Standard));
    }
    // Still a child's subrole, or no answer: a window minimized now is standard all the same.
    if minimized_now().unwrap_or(false) {
        return Some((subrole, WindowKind::Standard));
    }
    let subrole = subrole?;
    let kind = kind_of(&subrole, false);
    Some((Some(subrole), kind))
}

/// The `STAGE_MANAGER_ONLY` filter: a set of bundle ids, or `None` for every app.
pub fn parse_only(value: Option<&str>) -> Option<HashSet<String>> {
    let set: HashSet<String> =
        value?.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    if set.is_empty() { None } else { Some(set) }
}

/// The `STAGE_MANAGER_ONLY` filter from the environment.
pub fn only_from_env() -> Option<HashSet<String>> {
    parse_only(std::env::var("STAGE_MANAGER_ONLY").ok().as_deref())
}

/// An app's `AXWindows` answer: its windows, and whether to read the list again later.
/// No value is an app without windows; any other error (busy, not ready) is retried.
pub fn windows_or_retry<T>(result: Result<Vec<T>, AxError>) -> (Vec<T>, bool) {
    match result {
        Ok(w) => (w, false),
        Err(e) if e == AxError::NO_VALUE => (Vec::new(), false),
        Err(_) => (Vec::new(), true),
    }
}

/// Whether an app may be observed and managed: not us, not excluded, a real pid, and on the
/// `only` list when there is one.
pub fn app_allowed(only: Option<&HashSet<String>>, own_pid: i32, pid: i32, bundle_id: Option<&str>) -> bool {
    if pid <= 0 || pid == own_pid {
        return false;
    }
    match bundle_id {
        Some(b) if windows::is_excluded_bundle(b) => false,
        Some(b) => only.is_none_or(|only| only.contains(b)),
        None => only.is_none(),
    }
}

/// Everything the WM thread knows about apps and windows.
pub struct Observer {
    own_pid: i32,
    only: Option<HashSet<String>>,
    accessibility: bool,
    observing: bool,
    settings: RawSettings,
    apps: BTreeMap<i32, AppInfo>,
    observed: HashMap<i32, ObservedApp>,
    windows: BTreeMap<u32, TrackedWindow>,
    pending: HashMap<i32, u32>,
    /// Apps with a window the window server did not list yet (just created): rescanned on
    /// the next retry passes, counting attempts.
    recheck: HashMap<i32, u32>,
    events: Vec<WinEvent>,
    /// When the main thread got each app's last activation notice.
    activated_at: HashMap<i32, Instant>,
    /// When the last regular app launched (any app, managed or not): the restore window's
    /// settling waits for launches to stop.
    last_launch: Option<Instant>,
    /// Tracked windows the window server no longer lists ([`Self::sweep_closed`], 8M).
    vanished: Vanished,
}

/// The AX callback type, installed by the WM thread.
pub type Callback =
    unsafe extern "C-unwind" fn(NonNull<AXObserver>, NonNull<AXUIElement>, NonNull<CFString>, *mut c_void);

fn refcon(pid: i32) -> usize {
    pid as usize
}

/// The pid carried in an AX callback's `refcon`.
pub fn pid_of_refcon(refcon: *mut c_void) -> i32 {
    refcon as usize as i32
}

impl Observer {
    /// A fresh observer for this process, filtered by `only`.
    pub fn new(own_pid: i32, only: Option<HashSet<String>>) -> Self {
        Self {
            own_pid,
            only,
            accessibility: false,
            observing: false,
            settings: RawSettings::new(),
            apps: BTreeMap::new(),
            observed: HashMap::new(),
            windows: BTreeMap::new(),
            pending: HashMap::new(),
            recheck: HashMap::new(),
            events: Vec::new(),
            activated_at: HashMap::new(),
            last_launch: None,
            vanished: Vanished::default(),
        }
    }

    /// The events since the last call.
    pub fn take_events(&mut self) -> Vec<WinEvent> {
        std::mem::take(&mut self.events)
    }

    /// True while windows are observed.
    pub fn observing(&self) -> bool {
        self.observing
    }

    /// True when Accessibility is granted.
    pub fn accessibility(&self) -> bool {
        self.accessibility
    }

    /// The tracked windows by id.
    pub fn windows(&self) -> &BTreeMap<u32, TrackedWindow> {
        &self.windows
    }

    /// One tracked window.
    pub fn window(&self, id: u32) -> Option<&TrackedWindow> {
        self.windows.get(&id)
    }

    /// When the main thread got app `pid`'s last activation notice (NSWorkspace).
    pub fn activated_at(&self, pid: i32) -> Option<Instant> {
        self.activated_at.get(&pid).copied()
    }

    /// When the last regular app launched, if one did since this observer started.
    pub fn last_launch(&self) -> Option<Instant> {
        self.last_launch
    }

    /// The bundle id of a running app, if known.
    pub fn bundle_id(&self, pid: i32) -> Option<&str> {
        self.apps.get(&pid).and_then(|a| a.bundle_id.as_deref())
    }

    /// The running apps with bundle id `bundle` (the Dock's item for an app).
    pub fn pids_with_bundle(&self, bundle: &str) -> Vec<i32> {
        self.apps.values().filter(|a| a.bundle_id.as_deref() == Some(bundle)).map(|a| a.pid).collect()
    }

    /// Re-reads a tracked window's frame through AX, keeping the result.
    pub fn refresh_frame(&mut self, id: u32) -> Option<Rect> {
        let w = self.windows.get_mut(&id)?;
        if let Ok(f) = w.element.frame() {
            w.frame = f;
        }
        Some(w.frame)
    }

    /// Reads the kind of a tracked window taken for a child again, keeping the answer: a
    /// window first read off screen may have answered for its state then (measured 10-15, macOS
    /// 27: a window reopened minimized reads `AXDialog`, `AXStandardWindow` once it shows, and
    /// may read `AXDialog` still during the un-minimize animation), so a window that waited off
    /// screen is read again when it is placed; one minimized then or now is standard
    /// ([`reread_kind`]). True when its kind changed.
    pub fn recheck_kind(&mut self, id: u32) -> bool {
        let Some(w) = self.windows.get_mut(&id) else { return false };
        let element = &w.element;
        let Some((subrole, kind)) = reread_kind(w.kind, w.minimized, || element.is_minimized(), || element.subrole())
        else {
            return false;
        };
        let changed = kind != w.kind;
        let subrole = subrole.unwrap_or_else(|| w.subrole.clone());
        if changed {
            tracing::info!(id, pid = w.pid, from = %w.subrole, to = %subrole, minimized = w.minimized, "window read again once on screen: now a standard window (first read off screen)");
        }
        w.subrole = subrole;
        w.kind = kind;
        changed
    }

    /// True when app `pid` is observed (allowed and answering, not ours).
    pub fn observes(&self, pid: i32) -> bool {
        self.observed.contains_key(&pid)
    }

    /// The `STAGE_MANAGER_ONLY` filter in force.
    pub fn only(&self) -> Option<&HashSet<String>> {
        self.only.as_ref()
    }

    /// Stores the latest `com.apple.WindowManager` values (for `ctl dump`).
    pub fn set_settings(&mut self, settings: RawSettings) {
        self.settings = settings;
    }

    /// The full list of running regular apps.
    pub fn set_apps(&mut self, list: Vec<AppInfo>, callback: Callback) {
        self.apps = list.into_iter().map(|a| (a.pid, a)).collect();
        tracing::debug!("{} regular apps running", self.apps.len());
        if self.observing {
            self.observe_all(callback);
        }
    }

    /// Accessibility trust changed: start or stop observation.
    pub fn set_accessibility(&mut self, trusted: bool, callback: Callback) {
        self.accessibility = trusted;
        if trusted && !self.observing {
            self.start(callback);
        } else if !trusted && self.observing {
            tracing::warn!("Accessibility revoked; observation stopped");
            self.stop();
        }
    }

    fn allowed(&self, app: &AppInfo) -> bool {
        app_allowed(self.only.as_ref(), self.own_pid, app.pid, app.bundle_id.as_deref())
    }

    fn bundle_of(&self, pid: i32) -> &str {
        self.apps.get(&pid).map_or("?", AppInfo::bundle_or_unknown)
    }

    fn start(&mut self, callback: Callback) {
        self.observing = true;
        tracing::info!("Accessibility granted; observation starting");
        let list = windows::list_windows();
        let z = windows::z_order(&list);
        let cg: HashMap<u32, CgWindow> = list.into_iter().map(|w| (w.id, w)).collect();
        let mut found = Vec::new();
        let pids: Vec<i32> = self.apps.values().filter(|a| self.allowed(a)).map(|a| a.pid).collect();
        for pid in pids {
            found.extend(self.observe_app_or_queue(pid, &cg, callback));
        }
        found.sort_by_key(|id| z.get(id).copied().unwrap_or(usize::MAX));
        for id in &found {
            if let Some(w) = self.windows.get(id) {
                let zi = z.get(id).map_or_else(|| "-".to_string(), |z| z.to_string());
                tracing::info!(
                    "window id={} pid={} bundle={} title={} frame={} z={} subrole={}",
                    w.id, w.pid, self.bundle_of(w.pid), logging::title(&w.title), w.frame, zi, w.subrole
                );
            }
        }
        tracing::info!("observing {} apps, {} manageable windows", self.observed.len(), self.windows.len());
        self.events.push(WinEvent::Started);
    }

    /// Drops every observer and window.
    pub fn stop(&mut self) {
        let was = self.observing;
        self.observing = false;
        self.observed.clear();
        self.windows.clear();
        self.pending.clear();
        self.recheck.clear();
        if was {
            self.events.push(WinEvent::Stopped);
        }
    }

    fn observe_all(&mut self, callback: Callback) {
        let cg = cg_lookup();
        let pids: Vec<i32> = self
            .apps
            .values()
            .filter(|a| self.allowed(a) && !self.observed.contains_key(&a.pid))
            .map(|a| a.pid)
            .collect();
        for pid in pids {
            for id in self.observe_app_or_queue(pid, &cg, callback) {
                self.found(id, "window found");
            }
        }
    }

    /// Observes `pid`; on failure queues a retry. Returns newly tracked window ids.
    fn observe_app_or_queue(&mut self, pid: i32, cg: &HashMap<u32, CgWindow>, callback: Callback) -> Vec<u32> {
        match self.observe_app(pid, cg, callback) {
            Ok(ids) => {
                self.pending.remove(&pid);
                ids
            }
            Err(e) => {
                let attempts = self.pending.entry(pid).or_insert(0);
                *attempts += 1;
                tracing::debug!(pid, attempt = *attempts, "cannot observe app yet: {e}");
                if *attempts >= MAX_ATTEMPTS {
                    tracing::warn!(pid, bundle = self.bundle_of(pid), "giving up observing app: {e}");
                    self.pending.remove(&pid);
                }
                Vec::new()
            }
        }
    }

    fn observe_app(&mut self, pid: i32, cg: &HashMap<u32, CgWindow>, callback: Callback) -> Result<Vec<u32>, AxError> {
        if self.observed.contains_key(&pid) {
            return Ok(Vec::new());
        }
        let element = AxElement::application(pid);
        let observer = AxObserver::new(pid, callback)?;
        let mut registered = 0;
        let mut last_err = None;
        for name in n::APP_LEVEL {
            match observer.add(&element, name, refcon(pid)) {
                Ok(()) => registered += 1,
                Err(e) => last_err = Some(e),
            }
        }
        if registered == 0 {
            return Err(last_err.unwrap_or(AxError::Value));
        }
        let (ax_windows, retry) = windows_or_retry(element.windows());
        if retry {
            // Busy (CannotComplete at the timeout) or not ready: its windows would stay
            // unmanaged until it is activated; the retry passes read the list again.
            tracing::debug!(pid, "AXWindows failed; rescanning the app on the next retry passes");
            self.recheck.entry(pid).or_insert(0);
        }
        self.observed.insert(pid, ObservedApp { element, observer });
        tracing::debug!(pid, bundle = self.bundle_of(pid), "observing app");
        let mut ids: Vec<u32> = ax_windows.into_iter().filter_map(|w| self.track_window(pid, w, cg)).collect();
        // Creation order, so rules see windows in the order the app opened them.
        ids.sort_unstable();
        Ok(ids)
    }

    fn unobserve_app(&mut self, pid: i32) {
        self.pending.remove(&pid);
        self.recheck.remove(&pid);
        if self.observed.remove(&pid).is_some() {
            let gone: Vec<u32> = self.windows.values().filter(|w| w.pid == pid).map(|w| w.id).collect();
            for id in &gone {
                self.windows.remove(id);
                self.events.push(WinEvent::Destroyed(*id));
            }
            tracing::debug!(pid, dropped = gone.len(), "stopped observing app");
        }
    }

    /// Retries apps whose AX was not ready, and rescans apps with windows the window server
    /// did not list yet (called about once a second).
    pub fn retry_pending(&mut self, callback: Callback) {
        if !self.observing {
            return;
        }
        let recheck: Vec<i32> = self.recheck.keys().copied().collect();
        for pid in recheck {
            let attempts = self.recheck.entry(pid).or_insert(0);
            *attempts += 1;
            if *attempts > MAX_ATTEMPTS {
                self.recheck.remove(&pid);
                continue;
            }
            self.rescan_app(pid);
        }
        if self.pending.is_empty() {
            return;
        }
        let cg = cg_lookup();
        let pids: Vec<i32> = self.pending.keys().copied().collect();
        for pid in pids {
            for id in self.observe_app_or_queue(pid, &cg, callback) {
                self.found(id, "window found");
            }
        }
    }

    /// Windows closed while their app did not answer (round 8M, [`crate::engine::vanished`]):
    /// each tracked window of an app still observed and running that the window server's
    /// complete list ([`windows::list_windows`]: `kCGWindowListOptionAll`, every Desktop,
    /// minimized and hidden windows included) misses on two looks at least
    /// [`crate::engine::vanished::LOOK_APART`] apart is dropped and reported destroyed, as a
    /// destroyed notification would have (its stage drops it; an empty stage has no card).
    /// Logged at info, once per window. Called about once a second.
    pub fn sweep_closed(&mut self) {
        if !self.observing || self.windows.is_empty() {
            return;
        }
        let list = windows::list_windows();
        // Nothing on screen at all: the window server answered nothing (or the session is not
        // the console's): no observation.
        let blind = !list.iter().any(|w| w.onscreen);
        let listed: HashSet<u32> = list.iter().map(|w| w.id).collect();
        let alive = |pid: i32| {
            // SAFETY: signal 0 only checks that the process exists; nothing is sent.
            unsafe { libc::kill(pid, 0) == 0 }
        };
        let tracked: Vec<u32> =
            self.windows.values().filter(|w| self.observed.contains_key(&w.pid) && alive(w.pid)).map(|w| w.id).collect();
        for id in self.vanished.look(Instant::now(), tracked, &listed, blind) {
            let Some(w) = self.windows.remove(&id) else { continue };
            if let Some(app) = self.observed.get(&w.pid) {
                for name in n::WINDOW_LEVEL {
                    let _ = app.observer.remove(&w.element, name);
                }
            }
            tracing::info!(
                id,
                pid = w.pid,
                bundle = self.bundle_of(w.pid),
                "window closed while its app did not answer: the window server no longer lists it on any Desktop and no destroyed notification came; dropped"
            );
            self.events.push(WinEvent::Destroyed(id));
        }
    }

    /// Tracks `element` if it is a manageable window not tracked yet; returns its id.
    fn track_window(&mut self, pid: i32, element: AxElement, cg: &HashMap<u32, CgWindow>) -> Option<u32> {
        let id = element.window_id().ok()?;
        if self.windows.contains_key(&id) {
            return None;
        }
        if element.role().ok()? != names::ROLE_WINDOW {
            return None;
        }
        if element.is_fullscreen().unwrap_or(false) {
            tracing::debug!(id, pid, "skipping fullscreen window");
            return None;
        }
        let Some(cg_window) = cg.get(&id) else {
            tracing::debug!(id, pid, "window not listed by the window server yet; rechecking soon");
            self.recheck.entry(pid).or_insert(0);
            return None;
        };
        if cg_window.layer != 0 {
            tracing::debug!(id, pid, "skipping window not on layer 0");
            return None;
        }
        let subrole = element.subrole().unwrap_or_default();
        let minimized = element.is_minimized().unwrap_or(false);
        let kind = kind_of(&subrole, minimized);
        if kind != WindowKind::from_subrole(&subrole) {
            tracing::info!(id, pid, subrole = %subrole, "window first read minimized: a standard window whatever its subrole");
        }
        let window = TrackedWindow {
            id,
            pid,
            kind,
            subrole,
            title: element.title().unwrap_or_default(),
            frame: element.frame().unwrap_or(cg_window.bounds),
            minimized,
            element,
        };
        if let Some(app) = self.observed.get(&pid) {
            for name in n::WINDOW_LEVEL {
                if let Err(e) = app.observer.add(&window.element, name, refcon(pid)) {
                    tracing::debug!(id, pid, "cannot register {name}: {e}");
                }
            }
        }
        self.windows.insert(id, window);
        Some(id)
    }

    fn find_window(&self, pid: i32, element: &AxElement) -> Option<u32> {
        self.windows.values().find(|w| w.pid == pid && &w.element == element).map(|w| w.id)
    }

    fn found(&mut self, id: u32, what: &str) {
        self.log_window(what, id);
        self.events.push(WinEvent::Found(id));
    }

    fn log_window(&self, what: &str, id: u32) {
        if let Some(w) = self.windows.get(&id) {
            tracing::info!(
                "{what} id={} pid={} bundle={} title={} frame={} subrole={}",
                w.id, w.pid, self.bundle_of(w.pid), logging::title(&w.title), w.frame, w.subrole
            );
        }
    }

    /// Re-reads an app's window list and tracks windows that appeared without a
    /// notification (or before the window server knew them).
    fn rescan_app(&mut self, pid: i32) {
        let cg = cg_lookup();
        for id in self.rescan_app_quietly(pid, &cg) {
            self.found(id, "window found");
        }
    }

    /// Tracks the windows `pid` lists now that are not tracked yet, in creation order,
    /// without reporting them.
    fn rescan_app_quietly(&mut self, pid: i32, cg: &HashMap<u32, CgWindow>) -> Vec<u32> {
        let Some(app) = self.observed.get(&pid) else { return Vec::new() };
        let Ok(list) = app.element.windows() else { return Vec::new() };
        let mut ids: Vec<u32> = list.into_iter().filter_map(|w| self.track_window(pid, w, cg)).collect();
        ids.sort_unstable();
        ids
    }

    /// After a Space change: re-reads every observed app's window list (most apps list only
    /// the current Space's windows) and returns the windows tracked for the first time,
    /// without reporting them as [`WinEvent::Found`]: the controller places them as the
    /// Space's own.
    pub fn rescan_all(&mut self) -> Vec<u32> {
        if !self.observing {
            return Vec::new();
        }
        let cg = cg_lookup();
        let pids: Vec<i32> = self.observed.keys().copied().collect();
        let mut out = Vec::new();
        for pid in pids {
            for id in self.rescan_app_quietly(pid, &cg) {
                self.log_window("window found on arrival", id);
                out.push(id);
            }
        }
        out.sort_unstable();
        out
    }

    /// Whether the window is minimized now, asked through AX ([`Reply`]: a timeout and any
    /// other "cannot tell" kept apart, [`crate::engine::busy_apps`]). A window without the
    /// attribute, or not tracked, is not minimized. Callers ask "is it off screen for a
    /// reason?", and an unknown answer must never count a window as gone from its Space (the
    /// review's L4).
    pub fn minimized_reply(&self, id: u32) -> Reply {
        self.windows.get(&id).map_or(Reply::No, |w| reply(w.element.is_minimized()))
    }

    /// Whether the window is in full screen now (on its own Space; asked through AX, which
    /// still reaches a tracked window off the shown Space for the apps measured).
    pub fn fullscreen_reply(&self, id: u32) -> Reply {
        self.windows.get(&id).map_or(Reply::No, |w| reply(w.element.is_fullscreen()))
    }

    /// Whether app `pid` is hidden now (⌘H; asked through AX).
    pub fn app_hidden_reply(&self, pid: i32) -> Reply {
        self.observed.get(&pid).map_or(Reply::No, |a| reply(a.element.is_hidden()))
    }

    /// Handles one AX notification for app `pid`.
    pub fn on_notification(&mut self, pid: i32, element: AxElement, name: &str) {
        if !self.observing {
            return;
        }
        match name {
            n::WINDOW_CREATED => {
                let cg = cg_lookup();
                if let Some(id) = self.track_window(pid, element, &cg) {
                    self.found(id, "window created");
                }
            }
            n::FOCUSED_WINDOW_CHANGED => {
                let id = self.find_window(pid, &element).or_else(|| {
                    let cg = cg_lookup();
                    let id = self.track_window(pid, element.clone(), &cg);
                    if let Some(id) = id {
                        self.found(id, "window found");
                    }
                    id
                });
                match id {
                    Some(id) => {
                        tracing::debug!(id, pid, bundle = self.bundle_of(pid), "window focused");
                        self.events.push(WinEvent::Focused(id));
                    }
                    None => tracing::debug!(pid, "focus moved to an unmanaged window"),
                }
            }
            n::APPLICATION_ACTIVATED => {
                tracing::debug!(pid, bundle = self.bundle_of(pid), "AX application activated");
                self.rescan_app(pid);
            }
            n::ELEMENT_DESTROYED => {
                if drop_destroyed() {
                    tracing::debug!(pid, "test: a destroyed notification ignored ({DROP_DESTROYED_ENV})");
                    return;
                }
                if let Some(id) = self.find_window(pid, &element) {
                    if let (Some(w), Some(app)) = (self.windows.remove(&id), self.observed.get(&pid)) {
                        for name in n::WINDOW_LEVEL {
                            let _ = app.observer.remove(&w.element, name);
                        }
                    }
                    tracing::info!(id, pid, bundle = self.bundle_of(pid), "window destroyed");
                    self.events.push(WinEvent::Destroyed(id));
                }
            }
            n::WINDOW_MOVED | n::WINDOW_RESIZED => {
                if let Some(id) = self.find_window(pid, &element) {
                    let frame = element.frame();
                    if let (Some(w), Ok(frame)) = (self.windows.get_mut(&id), frame) {
                        w.frame = frame;
                        let what = if name == n::WINDOW_MOVED { "moved" } else { "resized" };
                        tracing::trace!(id, pid, "window {what} to {frame}");
                        self.events.push(WinEvent::Moved(id));
                    }
                }
            }
            n::TITLE_CHANGED => {
                if let Some(id) = self.find_window(pid, &element)
                    && let Some(w) = self.windows.get_mut(&id)
                    && let Ok(title) = w.element.title()
                    && title != w.title
                {
                    w.title = title;
                    self.events.push(WinEvent::Retitled(id));
                }
            }
            n::WINDOW_MINIATURIZED => {
                if let Some(w) = self.find_window(pid, &element).and_then(|id| self.windows.get_mut(&id)) {
                    w.minimized = true;
                    tracing::debug!(id = w.id, pid, "window minimized");
                    let id = w.id;
                    self.events.push(WinEvent::Minimized(id));
                }
            }
            other => tracing::debug!(pid, "unhandled AX notification {other}"),
        }
    }

    /// Handles an NSWorkspace event.
    pub fn on_app_event(&mut self, event: AppEvent, callback: Callback) {
        match event {
            AppEvent::Launched(app) => {
                self.log_app("app launched", &app);
                self.last_launch = Some(Instant::now());
                let pid = app.pid;
                let allowed = self.allowed(&app);
                self.apps.insert(pid, app);
                if self.observing && allowed {
                    let cg = cg_lookup();
                    for id in self.observe_app_or_queue(pid, &cg, callback) {
                        self.found(id, "window found");
                    }
                }
            }
            AppEvent::Terminated(app) => {
                self.activated_at.remove(&app.pid);
                let known = self.apps.remove(&app.pid).is_some();
                if known {
                    self.log_app("app terminated", &app);
                }
                self.unobserve_app(app.pid);
                if self.observing {
                    self.events.push(WinEvent::AppTerminated(app.pid));
                }
            }
            AppEvent::Activated(app, at) => {
                let queued_ms = at.elapsed().as_millis() as u64;
                self.log_app_queued("app activated", &app, queued_ms);
                let pid = app.pid;
                self.activated_at.insert(pid, at);
                let allowed = self.allowed(&app);
                self.apps.entry(pid).or_insert(app);
                if self.observing && allowed {
                    if !self.observed.contains_key(&pid) {
                        let cg = cg_lookup();
                        for id in self.observe_app_or_queue(pid, &cg, callback) {
                            self.found(id, "window found");
                        }
                    }
                    self.events.push(WinEvent::AppActivated(pid));
                }
            }
            AppEvent::Hidden(app) => {
                self.log_app("app hidden", &app);
                if self.observing && self.allowed(&app) {
                    self.events.push(WinEvent::AppHidden(app.pid));
                }
            }
            AppEvent::Unhidden(app) => {
                self.log_app("app unhidden", &app);
                if self.observing && self.allowed(&app) {
                    self.events.push(WinEvent::AppUnhidden(app.pid));
                }
            }
        }
    }

    fn log_app_queued(&self, what: &str, app: &AppInfo, queued_ms: u64) {
        let name = app.name.as_deref().unwrap_or("?");
        if self.allowed(app) {
            tracing::info!(pid = app.pid, bundle = app.bundle_or_unknown(), name, queued_ms, "{what}");
        } else {
            tracing::debug!(pid = app.pid, bundle = app.bundle_or_unknown(), name, queued_ms, "{what} (not observed)");
        }
    }

    fn log_app(&self, what: &str, app: &AppInfo) {
        let name = app.name.as_deref().unwrap_or("?");
        if self.allowed(app) {
            tracing::info!(pid = app.pid, bundle = app.bundle_or_unknown(), name, "{what}");
        } else {
            tracing::debug!(pid = app.pid, bundle = app.bundle_or_unknown(), name, "{what} (not observed)");
        }
    }

    /// The observed apps and windows as JSON (for `ctl dump`).
    pub fn dump(&self) -> serde_json::Value {
        let cg: Vec<CgWindow> = windows::list_windows();
        let z = windows::z_order(&cg);
        let mut wins: Vec<&TrackedWindow> = self.windows.values().collect();
        wins.sort_by_key(|w| z.get(&w.id).copied().unwrap_or(usize::MAX));
        let windows: Vec<_> = wins
            .iter()
            .map(|w| {
                json!({
                    "id": w.id,
                    "pid": w.pid,
                    "bundle_id": self.apps.get(&w.pid).and_then(|a| a.bundle_id.clone()),
                    "title": w.title,
                    "frame": w.frame,
                    "z": z.get(&w.id),
                    "subrole": w.subrole,
                    "kind": w.kind,
                    "minimized": w.minimized,
                })
            })
            .collect();
        let apps: Vec<_> = self
            .apps
            .values()
            .filter(|a| self.allowed(a))
            .map(|a| {
                json!({
                    "pid": a.pid,
                    "bundle_id": a.bundle_id,
                    "name": a.name,
                    "observed": self.observed.contains_key(&a.pid),
                })
            })
            .collect();
        let note = if !self.accessibility {
            Some("Accessibility not granted: windows are not observed")
        } else if !self.observing {
            Some("observation not started")
        } else {
            None
        };
        let mut only: Option<Vec<&String>> = self.only.as_ref().map(|s| s.iter().collect());
        if let Some(list) = only.as_mut() {
            list.sort();
        }
        json!({
            "ok": true,
            "accessibility": self.accessibility,
            "observing": self.observing,
            "note": note,
            "only": only,
            "macos_settings": self.settings,
            "apps": apps,
            "windows": windows,
        })
    }
}

fn cg_lookup() -> HashMap<u32, CgWindow> {
    windows::list_windows().into_iter().map(|w| (w.id, w)).collect()
}

/// An AX yes/no answer as a [`Reply`]: a missing attribute reads as no; the timeout (the app
/// did not answer: `CannotComplete`) and any other error kept apart.
fn reply(answer: AxResult<bool>) -> Reply {
    match answer {
        Ok(true) => Reply::Yes,
        Ok(false) => Reply::No,
        Err(e) if e == AxError::NO_VALUE || e == AxError::ATTRIBUTE_UNSUPPORTED => Reply::No,
        Err(e) if e == AxError::CANNOT_COMPLETE => {
            tracing::debug!("AX: the app did not answer in time; left alone a moment");
            Reply::TimedOut
        }
        Err(e) => {
            tracing::debug!("AX cannot tell ({e})");
            Reply::CannotTell
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round 8H, the freeze: once the logout's head start stops observation, an app's quit or
    /// activation reaches no controller (no stage switched, no card removed, nothing saved),
    /// while the running apps stay known for a cancelled logout's fresh start.
    #[test]
    fn a_stopped_observer_hands_on_no_app_event() {
        unsafe extern "C-unwind" fn never(_: NonNull<AXObserver>, _: NonNull<AXUIElement>, _: NonNull<CFString>, _: *mut c_void) {}
        let mail = AppInfo { pid: 4242, bundle_id: Some("com.example.mail".into()), name: Some("Mail".into()) };
        let terminal = AppInfo { pid: 4343, bundle_id: Some("com.example.terminal".into()), name: Some("Terminal".into()) };
        let mut ob = Observer::new(1, None);
        ob.set_apps(vec![mail.clone(), terminal.clone()], never);
        ob.stop();
        let _ = ob.take_events();
        ob.on_app_event(AppEvent::Terminated(mail.clone()), never);
        ob.on_app_event(AppEvent::Activated(terminal.clone(), Instant::now()), never);
        ob.on_app_event(AppEvent::Hidden(terminal.clone()), never);
        assert!(ob.take_events().is_empty(), "nothing for any controller");
        assert!(!ob.observing());
    }

    #[test]
    fn an_axwindows_error_is_retried_unless_there_is_no_value() {
        assert_eq!(windows_or_retry(Ok(vec![1, 2])), (vec![1, 2], false));
        assert_eq!(windows_or_retry::<u32>(Err(AxError::NO_VALUE)), (vec![], false), "no windows: nothing to retry");
        assert_eq!(windows_or_retry::<u32>(Err(AxError::CANNOT_COMPLETE)), (vec![], true), "busy at the timeout");
        assert_eq!(windows_or_retry::<u32>(Err(AxError::Value)), (vec![], true));
    }

    #[test]
    fn ax_answers_keep_a_timeout_apart_from_cannot_tell() {
        assert_eq!(reply(Ok(true)), Reply::Yes);
        assert_eq!(reply(Ok(false)), Reply::No);
        assert_eq!(reply(Err(AxError::NO_VALUE)), Reply::No);
        assert_eq!(reply(Err(AxError::ATTRIBUTE_UNSUPPORTED)), Reply::No);
        assert_eq!(reply(Err(AxError::CANNOT_COMPLETE)), Reply::TimedOut);
        assert_eq!(reply(Err(AxError::Api(-25202))), Reply::CannotTell);
    }

    #[test]
    fn only_filter_parses_comma_lists() {
        assert_eq!(parse_only(None), None);
        assert_eq!(parse_only(Some(" , ")), None);
        let set = parse_only(Some("a.b, c.d,,")).unwrap_or_default();
        assert!(set.contains("a.b") && set.contains("c.d") && set.len() == 2);
    }

    #[test]
    fn subroles_classify_standard_and_child() {
        assert_eq!(WindowKind::from_subrole("AXStandardWindow"), WindowKind::Standard);
        assert_eq!(WindowKind::from_subrole("AXDialog"), WindowKind::Child);
        assert_eq!(WindowKind::from_subrole(""), WindowKind::Child);
    }

    #[test]
    fn a_minimized_window_is_standard_whatever_its_subrole() {
        // The 10-15 live run: a window reopened minimized reads `AXDialog` in the Dock.
        assert_eq!(kind_of("AXDialog", true), WindowKind::Standard);
        assert_eq!(kind_of("", true), WindowKind::Standard);
        assert_eq!(kind_of("AXStandardWindow", true), WindowKind::Standard);
        // Never minimized: its subrole decides, so a real dialog stays a child.
        assert_eq!(kind_of("AXDialog", false), WindowKind::Child);
        assert_eq!(kind_of("AXFloatingWindow", false), WindowKind::Child);
        assert_eq!(kind_of("AXStandardWindow", false), WindowKind::Standard);
    }

    #[test]
    fn a_window_first_read_off_screen_is_read_again_when_it_shows() {
        let not_minimized = || Ok::<_, AxError>(false);
        // Alpha 2 (10-15): read `AXDialog` off screen, `AXStandardWindow` once shown.
        let shown = || Ok::<_, AxError>("AXStandardWindow".to_string());
        assert_eq!(
            reread_kind(WindowKind::Child, false, not_minimized, shown),
            Some((Some("AXStandardWindow".into()), WindowKind::Standard))
        );
        // A real dialog (never minimized) stays one.
        assert_eq!(
            reread_kind(WindowKind::Child, false, not_minimized, || Ok("AXDialog".into())),
            Some((Some("AXDialog".into()), WindowKind::Child))
        );
        // No answer (busy, gone): kept as it was.
        assert_eq!(reread_kind(WindowKind::Child, false, not_minimized, || Err(AxError::CANNOT_COMPLETE)), None);
        assert_eq!(
            reread_kind(WindowKind::Child, false, || Err(AxError::CANNOT_COMPLETE), || Ok("AXDialog".into())),
            Some((Some("AXDialog".into()), WindowKind::Child))
        );
        // A standard window is never asked again.
        let asked = std::cell::Cell::new(false);
        let read = || {
            asked.set(true);
            Ok("AXDialog".to_string())
        };
        let minimized = || {
            asked.set(true);
            Ok(false)
        };
        assert_eq!(reread_kind(WindowKind::Standard, false, minimized, read), None);
        assert!(!asked.get());
    }

    #[test]
    fn a_window_minimized_then_or_now_is_standard_when_read_again() {
        // Alpha 1 (10-15): un-minimized during the restore window, it still read `AXDialog`
        // mid-animation when placed. Once minimized, it is standard whatever it reads now.
        let dialog = || Ok::<_, AxError>("AXDialog".to_string());
        assert_eq!(
            reread_kind(WindowKind::Child, true, || Ok(false), dialog),
            Some((Some("AXDialog".into()), WindowKind::Standard))
        );
        assert_eq!(
            reread_kind(WindowKind::Child, true, || Ok(false), || Err(AxError::CANNOT_COMPLETE)),
            Some((None, WindowKind::Standard))
        );
        // Minimized now (not seen before): standard as well, even with no subrole answer.
        assert_eq!(
            reread_kind(WindowKind::Child, false, || Ok(true), dialog),
            Some((Some("AXDialog".into()), WindowKind::Standard))
        );
        assert_eq!(
            reread_kind(WindowKind::Child, false, || Ok(true), || Err(AxError::CANNOT_COMPLETE)),
            Some((None, WindowKind::Standard))
        );
    }

    fn app(pid: i32, bundle: Option<&str>) -> AppInfo {
        AppInfo { pid, bundle_id: bundle.map(String::from), name: None }
    }

    #[test]
    fn allowed_apps_respect_own_pid_exclusions_and_only() {
        let all = Observer::new(1, None);
        assert!(!all.allowed(&app(1, Some("x.y"))));
        assert!(!all.allowed(&app(2, Some("com.apple.dock"))));
        assert!(all.allowed(&app(2, Some("x.y"))));
        assert!(all.allowed(&app(2, None)));
        let only = Observer::new(1, parse_only(Some("dev.eugene.stagemanager.fixture")));
        assert!(only.allowed(&app(2, Some("dev.eugene.stagemanager.fixture"))));
        assert!(!only.allowed(&app(2, Some("x.y"))));
        assert!(!only.allowed(&app(2, None)));
    }

    #[test]
    fn apps_without_a_real_pid_are_skipped() {
        let all = Observer::new(1, None);
        assert!(!all.allowed(&app(-1, Some("com.example.preview"))));
        assert!(!all.allowed(&app(0, Some("com.example.preview"))));
    }

    #[test]
    fn dump_without_accessibility_says_so() {
        let mut s = Observer::new(1, None);
        s.apps.insert(5, app(5, Some("x.y")));
        let d = s.dump();
        assert_eq!(d["accessibility"], false);
        assert_eq!(d["observing"], false);
        assert!(d["note"].as_str().unwrap_or_default().contains("Accessibility not granted"));
        assert_eq!(d["apps"].as_array().map(Vec::len), Some(1));
        assert_eq!(d["windows"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn stop_reports_once() {
        let mut s = Observer::new(1, None);
        s.stop();
        assert!(s.take_events().is_empty());
        s.observing = true;
        s.stop();
        assert_eq!(s.take_events(), vec![WinEvent::Stopped]);
    }
}
