//! Observation on the WM thread: which apps are observed, which windows are manageable, and
//! the AX notification handlers that keep both current.
//!
//! A *manageable* window is an AX element with role `AXWindow`, a `CGWindowID`, not
//! fullscreen, on layer 0. Subrole `AXStandardWindow` is [`WindowKind::Standard`]; any other
//! subrole is [`WindowKind::Child`]. Windows are deduplicated by window id.
//!
//! The [`Observer`] only tracks; it reports what changed as [`WinEvent`]s, which the
//! controller drains with [`Observer::take_events`] and turns into engine operations.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_application_services::{AXObserver, AXUIElement};
use objc2_core_foundation::CFString;
use serde::Serialize;
use serde_json::json;

use super::apps::{AppEvent, AppInfo};
use super::ax::{AxElement, AxError, AxObserver, names, notifications as n};
use super::native_settings::RawSettings;
use super::windows::{self, CgWindow};
use crate::engine::geometry::Rect;

/// How often an app whose AX is not ready yet (just launched) is retried, one per retry pass.
const MAX_ATTEMPTS: u32 = 10;

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
    /// An observed app became frontmost (NSWorkspace).
    AppActivated(i32),
}

struct ObservedApp {
    element: AxElement,
    observer: AxObserver,
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

    /// The bundle id of a running app, if known.
    pub fn bundle_id(&self, pid: i32) -> Option<&str> {
        self.apps.get(&pid).and_then(|a| a.bundle_id.as_deref())
    }

    /// Re-reads a tracked window's frame through AX, keeping the result.
    pub fn refresh_frame(&mut self, id: u32) -> Option<Rect> {
        let w = self.windows.get_mut(&id)?;
        if let Ok(f) = w.element.frame() {
            w.frame = f;
        }
        Some(w.frame)
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
                    "window id={} pid={} bundle={} title={:?} frame={} z={} subrole={}",
                    w.id, w.pid, self.bundle_of(w.pid), w.title, w.frame, zi, w.subrole
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
        let ax_windows = match element.windows() {
            Ok(w) => w,
            Err(e) if e == AxError::NO_VALUE => Vec::new(),
            Err(e) => {
                tracing::debug!(pid, "AXWindows failed: {e}");
                Vec::new()
            }
        };
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
        let window = TrackedWindow {
            id,
            pid,
            kind: WindowKind::from_subrole(&subrole),
            subrole,
            title: element.title().unwrap_or_default(),
            frame: element.frame().unwrap_or(cg_window.bounds),
            minimized: element.is_minimized().unwrap_or(false),
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
                "{what} id={} pid={} bundle={} title={:?} frame={} subrole={}",
                w.id, w.pid, self.bundle_of(w.pid), w.title, w.frame, w.subrole
            );
        }
    }

    /// Re-reads an app's window list and tracks windows that appeared without a
    /// notification (or before the window server knew them).
    fn rescan_app(&mut self, pid: i32) {
        let Some(app) = self.observed.get(&pid) else { return };
        let Ok(list) = app.element.windows() else { return };
        let cg = cg_lookup();
        let mut ids: Vec<u32> = list.into_iter().filter_map(|w| self.track_window(pid, w, &cg)).collect();
        ids.sort_unstable();
        for id in ids {
            self.found(id, "window found");
        }
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
                let known = self.apps.remove(&app.pid).is_some();
                if known {
                    self.log_app("app terminated", &app);
                }
                self.unobserve_app(app.pid);
            }
            AppEvent::Activated(app) => {
                self.log_app("app activated", &app);
                let pid = app.pid;
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

#[cfg(test)]
mod tests {
    use super::*;

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
