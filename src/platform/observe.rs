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
//!
//! **Quit without a notice** (round missed-quit, [`crate::engine::gone`]): Home quit on 10-23
//! (and earlier days) and no "app terminated" ever reached us. The pids of apps with tracked
//! windows (about once a second) and of apps whose Accessibility calls failed or timed out
//! (each pass) are tested ([`Observer::reap_gone`]); a process that no longer exists goes the
//! way of a termination notice. A termination notice for a pid we do not know is logged, and
//! taken for the known apps of its bundle id whose process is gone.
//!
//! **Dead handles** (round stale-handles, [`crate::engine::stale_handle`]): a window's element
//! can go stale while the window lives on (Music on 11-04: every call through it answered
//! `kAXErrorInvalidUIElement` from the first, and no destroyed notification ever came for it).
//! The controller looks such a window up afresh and notes, on `&self` paths, a fresh element
//! ([`Observer::note_fresh`], used at once through [`Observer::element`]) or the window
//! closed ([`Observer::note_closed`]); both are applied at the next [`Observer::take_events`]:
//! the fresh element replaces the tracked one (its notifications moved over), and a closed
//! window is dropped and reported destroyed, so if it comes back it is a new window. A focus
//! naming a new element for a tracked window whose handle is dead takes it at once.
//!
//! **Apps that do not answer yet** (round login-observe, [`crate::engine::observe_retry`]): an
//! app is observed on the WM thread only once it answers. One that does not (just launched,
//! or reopened at the login and still busy) is asked on a thread of its own
//! ([`super::observe_probe`]), every 150 ms while the login settles, every second after; its
//! answer ([`Observer::on_probe`]) observes it then. At the start every app is asked at once,
//! in parallel and for a bounded time ([`START_WAIT`]); those that answer are observed before
//! [`WinEvent::Started`], as before. An app the launch's recovery scan has not read yet
//! ([`super::launch_scan`]) waits for it: observed, or asked, once its scan is done
//! ([`Observer::scan_done`]). Each app observed logs, at debug, the ms since it was
//! first known (listed at our start, launched, or activated) and how many windows the window
//! server listed for it then; each window placed logs the ms since it was found
//! ([`TrackedWindow::found_at`]).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use objc2_application_services::{AXObserver, AXUIElement};
use objc2_core_foundation::CFString;
use serde::Serialize;
use serde_json::json;

use super::apps::{AppEvent, AppInfo};
use super::ax::{AxElement, AxError, AxObserver, AxResult, names, notifications as n};
use super::logging;
use super::native_settings::RawSettings;
use super::observe_probe::{self, Note, Notify, Probes};
use super::windows::{self, CgWindow};
use crate::engine::busy_apps::Reply;
use crate::engine::geometry::Rect;
use crate::engine::gone::{self, GoneCheck};
use crate::engine::observe_retry::{Next, Retry};
use crate::engine::vanished::Vanished;

/// How often an app with a window the window server did not list yet is rescanned, one per
/// retry pass (about once a second).
const RECHECK_ATTEMPTS: u32 = 10;

/// At observation's start every app is asked at once, in parallel; the start waits this long
/// at most for their answers (a miss takes the 0.25 s messaging timeout, so every answer that
/// can come is in). Those that do not answer are asked on their own threads.
pub const START_WAIT: Duration = Duration::from_millis(300);

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
    /// When it was first tracked: a window's ms from found to placed are logged from it.
    pub found_at: Instant,
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
    /// An observed app became frontmost (NSWorkspace), and when the main thread got the notice
    /// (also [`Observer::activated_at`]): a late one may be stale
    /// ([`crate::engine::stale_activation`]).
    AppActivated(i32, Instant),
    /// An observed app posted its own activation (AXApplicationActivated): unlike the
    /// NSWorkspace notice ([`WinEvent::AppActivated`]), it comes once the app has taken it,
    /// about when its windows are redrawn in their active look.
    AppTookActivation(i32),
    /// An observed app was hidden (NSWorkspace).
    AppHidden(i32),
    /// An observed app was unhidden (NSWorkspace).
    AppUnhidden(i32),
    /// An app quit (NSWorkspace; its windows' `Destroyed` come with it).
    AppTerminated(i32),
    /// A window was minimized (⌘M, the yellow button, Window → Minimize, its app's own
    /// code): posted once macOS's genie has put it in the Dock.
    Minimized(u32),
    /// A window's un-minimize is over: AppKit has set it down (`AXWindowDeminiaturized`).
    Deminiaturized(u32),
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

/// A registration answered `CannotComplete` after waiting this long timed out (half the WM
/// thread's 0.25 s messaging timeout): the app is not answering. One answered so sooner was
/// refused by an app that does answer.
pub const STALLED: Duration = Duration::from_millis(125);

/// Registers an app's notifications ([`n::APP_LEVEL`]) through `add` (its answer, and how
/// long it took), in order. The ones the app refused (it lacks them, or says it cannot), when
/// at least one was taken; an error when none was, when the first answered `CannotComplete`,
/// or at any that timed out ([`STALLED`]), the rest not asked: the app stopped answering, so
/// it is asked off the WM thread until it answers and registered whole then
/// ([`Observer::missed`]). Each registration that times out holds the WM thread for the
/// messaging timeout: at the 11-10 login an app that answered its first one and none after
/// held it 0.9 s, and the activation notices behind it waited 2.3 s.
pub fn register_app_level(mut add: impl FnMut(&'static str) -> (AxResult<()>, Duration)) -> Result<Vec<(&'static str, AxError)>, AxError> {
    let mut refused = Vec::new();
    for (i, name) in n::APP_LEVEL.into_iter().enumerate() {
        match add(name) {
            (Ok(()), _) => {}
            (Err(e), took) if e == AxError::CANNOT_COMPLETE && (i == refused.len() || took >= STALLED) => return Err(e),
            (Err(e), _) => refused.push((name, e)),
        }
    }
    match refused.last() {
        Some(&(_, e)) if refused.len() == n::APP_LEVEL.len() => Err(e),
        _ => Ok(refused),
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
    /// Apps that did not answer yet, with their retries ([`super::observe_probe`] asks them).
    pending: HashMap<i32, Retry>,
    /// The threads asking the pending apps.
    probes: Probes,
    /// Apps the launch's recovery scan has not read yet, since when ([`Self::hold_for_scan`]):
    /// neither observed nor asked until it has.
    scan_held: HashMap<i32, Instant>,
    /// Held apps observation wanted meanwhile: observed or asked once their scan is done.
    scan_wanted: HashSet<i32>,
    /// When each running app was first known, and how: the measure logged when it is observed.
    known: HashMap<i32, (Instant, &'static str)>,
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
    /// Apps whose Accessibility calls failed or timed out: their process is tested at the next
    /// pass ([`Self::reap_gone`]). A cell: the failures are seen on `&self` paths.
    gone: RefCell<GoneCheck>,
    /// Dead handles judged on `&self` paths, applied at the next [`Self::take_events`]
    /// (module docs).
    stale: RefCell<StaleNotes>,
}

/// What the controller found of windows whose handle died ([`Observer::note_fresh`],
/// [`Observer::note_closed`]), and the test knob's poisoned handles.
#[derive(Default)]
struct StaleNotes {
    /// Fresh elements for windows whose tracked one died.
    fresh: HashMap<u32, AxElement>,
    /// Windows judged closed.
    closed: Vec<u32>,
    /// Test only (`ctl debug-stale-handle`): handles taken for dead until a fresh one comes.
    poisoned: HashSet<u32>,
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
            probes: Probes::new(observe_probe::ask_ax),
            scan_held: HashMap::new(),
            scan_wanted: HashSet::new(),
            known: HashMap::new(),
            recheck: HashMap::new(),
            events: Vec::new(),
            activated_at: HashMap::new(),
            last_launch: None,
            vanished: Vanished::default(),
            gone: RefCell::new(GoneCheck::default()),
            stale: RefCell::new(StaleNotes::default()),
        }
    }

    /// The events since the last call (dead handles noted since are applied first: a window
    /// judged closed is among them as destroyed).
    pub fn take_events(&mut self) -> Vec<WinEvent> {
        self.apply_stale();
        std::mem::take(&mut self.events)
    }

    /// Window `id`'s element: a fresh one noted for it ([`Self::note_fresh`]), else the
    /// tracked one.
    pub fn element(&self, id: u32) -> Option<AxElement> {
        if let Some(e) = self.stale.borrow().fresh.get(&id) {
            return Some(e.clone());
        }
        self.windows.get(&id).map(|w| w.element.clone())
    }

    /// Window `id`'s handle was found dead and its app lists it afresh: `element` is its
    /// handle from now on (applied to the tracked window at the next [`Self::take_events`]).
    pub fn note_fresh(&self, id: u32, element: AxElement) {
        let mut stale = self.stale.borrow_mut();
        stale.poisoned.remove(&id);
        stale.fresh.insert(id, element);
    }

    /// Window `id`'s handle was found dead and the window judged closed: it is dropped and
    /// reported destroyed at the next [`Self::take_events`].
    pub fn note_closed(&self, id: u32) {
        let mut stale = self.stale.borrow_mut();
        stale.fresh.remove(&id);
        if !stale.closed.contains(&id) {
            stale.closed.push(id);
        }
    }

    /// True when window `id` was judged closed and is not dropped yet.
    pub fn noted_closed(&self, id: u32) -> bool {
        self.stale.borrow().closed.contains(&id)
    }

    /// Test only (`ctl debug-stale-handle`, `STAGE_MANAGER_TEST`): window `id`'s handle is
    /// taken for dead ([`Self::poisoned`]) until a fresh one is noted. False when the window
    /// is not tracked.
    pub fn poison(&mut self, id: u32) -> bool {
        if !self.windows.contains_key(&id) {
            return false;
        }
        let mut stale = self.stale.borrow_mut();
        stale.fresh.remove(&id);
        stale.poisoned.insert(id);
        true
    }

    /// True while window `id`'s handle is poisoned by the test knob ([`Self::poison`]).
    pub fn poisoned(&self, id: u32) -> bool {
        self.stale.borrow().poisoned.contains(&id)
    }

    /// Whether window `id`'s handle is dead now: a cheap read through it answers
    /// `kAXErrorInvalidUIElement` (or the test knob poisoned it). Any other failure (a busy
    /// app) is not taken for dead; the app is a suspect then ([`Self::suspect`]).
    pub fn handle_dead(&self, id: u32) -> bool {
        if self.poisoned(id) {
            return true;
        }
        let Some(w) = self.windows.get(&id) else { return false };
        let Some(element) = self.element(id) else { return false };
        match element.role() {
            Ok(_) => false,
            Err(e) if e == AxError::INVALID_ELEMENT => true,
            Err(_) => {
                self.suspect(w.pid);
                false
            }
        }
    }

    /// Window `id`'s tracked element becomes `element` (a fresh one for a dead one), its
    /// window notifications moved over. Nothing when it is the same element.
    fn replace_element(&mut self, id: u32, element: AxElement) {
        let Some(w) = self.windows.get_mut(&id) else { return };
        if w.element == element {
            return;
        }
        if let Some(app) = self.observed.get(&w.pid) {
            for name in n::WINDOW_LEVEL {
                let _ = app.observer.remove(&w.element, name);
                if let Err(e) = app.observer.add(&element, name, refcon(w.pid)) {
                    tracing::debug!(id, pid = w.pid, "a fresh handle: cannot register {name}: {e}");
                }
            }
        }
        w.element = element;
        tracing::debug!(id, pid = w.pid, "a fresh handle replaces the dead one");
    }

    /// A focus named `element`, which matches no tracked element: when it is a tracked
    /// window's (same window id) whose handle is dead, it is that window's handle from now on
    /// (Music on 11-04 focused its window through a new element 3 s after ours died). The
    /// window's id when it was.
    fn refocus_dead(&mut self, pid: i32, element: &AxElement) -> Option<u32> {
        let id = element.window_id().ok()?;
        let tracked = self.windows.get(&id).filter(|w| w.pid == pid)?;
        let dead = matches!(tracked.element.role(), Err(e) if e == AxError::INVALID_ELEMENT);
        if !dead {
            return None;
        }
        tracing::info!(id, pid, bundle = self.bundle_of(pid), "a window whose handle died is focused through a new one: its new handle is kept");
        self.stale.borrow_mut().poisoned.remove(&id);
        self.replace_element(id, element.clone());
        Some(id)
    }

    /// Applies the dead handles noted since the last call (module docs).
    fn apply_stale(&mut self) {
        let (fresh, closed) = {
            let mut stale = self.stale.borrow_mut();
            if stale.fresh.is_empty() && stale.closed.is_empty() {
                return;
            }
            (std::mem::take(&mut stale.fresh), std::mem::take(&mut stale.closed))
        };
        for (id, element) in fresh {
            self.replace_element(id, element);
        }
        for id in closed {
            self.stale.borrow_mut().poisoned.remove(&id);
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
                "a window whose handle died is gone: let go (its app lists it no more and the window server shows it off screen, or not at all)"
            );
            self.events.push(WinEvent::Destroyed(id));
        }
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

    /// True once the running apps are known (the first list came).
    pub fn knows_apps(&self) -> bool {
        !self.apps.is_empty()
    }

    /// The running apps with bundle id `bundle` (the Dock's item for an app).
    pub fn pids_with_bundle(&self, bundle: &str) -> Vec<i32> {
        self.apps.values().filter(|a| a.bundle_id.as_deref() == Some(bundle)).map(|a| a.pid).collect()
    }

    /// Re-reads a tracked window's frame through AX, keeping the result; the tracked frame
    /// when the window gives none. One message (`AXPosition` and `AXSize` together,
    /// [`AxElement::role_and_frame`]), not a round trip for each: the review's K4, every
    /// shown window read twice on each focus event, card press, new window and hotkey.
    pub fn refresh_frame(&mut self, id: u32) -> Option<Rect> {
        self.read_frame(id).map(|(f, _)| f)
    }

    /// Takes `frames` (window id, frame), an observation's as decided against the window
    /// server's list, as the tracked frames of the windows it names: a window of an app that
    /// posts no move notice is then tracked where it is, and a switch's outgoing picture
    /// starts there, not where it was before the user moved it (the review's switching
    /// 5). Unknown windows are skipped.
    pub fn note_frames(&mut self, frames: impl IntoIterator<Item = (u32, Rect)>) {
        for (id, frame) in frames {
            if let Some(w) = self.windows.get_mut(&id)
                && w.frame != frame
            {
                tracing::trace!(id, from = %w.frame, to = %frame, "tracked frame taken from the observation");
                w.frame = frame;
            }
        }
    }

    /// [`Self::refresh_frame`], saying whether the frame was read now (`true`) or is the
    /// tracked one because the read failed (a busy app's timeout).
    pub fn read_frame(&mut self, id: u32) -> Option<(Rect, bool)> {
        let w = self.windows.get_mut(&id)?;
        let fresh = match w.element.role_and_frame() {
            Ok((_, Some(f))) => {
                w.frame = f;
                true
            }
            _ => false,
        };
        Some((w.frame, fresh))
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

    /// True when app `pid` is a running regular app that would be observed (allowed, not
    /// ours), answering yet or not.
    pub fn wants(&self, pid: i32) -> bool {
        self.apps.get(&pid).is_some_and(|a| self.allowed(a))
    }

    /// True when app `pid` answers Accessibility: observed, and its window list read with no
    /// error (an app whose `AXWindows` failed is read again on the retry passes).
    pub fn answers(&self, pid: i32) -> bool {
        self.observed.contains_key(&pid) && !self.recheck.contains_key(&pid)
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
        let now = Instant::now();
        self.known.retain(|pid, _| self.apps.contains_key(pid));
        for &pid in self.apps.keys() {
            self.known.entry(pid).or_insert((now, "listed at our start"));
        }
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
        let (held, pids): (Vec<i32>, Vec<i32>) =
            self.apps.values().filter(|a| self.allowed(a)).map(|a| a.pid).partition(|p| self.scan_held.contains_key(p));
        // An app the launch's recovery scan has not read yet is observed once it has.
        let held_n = held.len();
        self.scan_wanted.extend(held);
        // Every app asked at once, for a bounded time: one that does not answer yet costs the
        // start nothing more and is asked on its own thread (round login-observe).
        let asked = Instant::now();
        let answered = observe_probe::ask_once(&pids, self.probes.ask(), START_WAIT);
        let waiting = pids.len() - answered.len();
        tracing::debug!(apps = pids.len(), answered = answered.len(), waiting, held = held_n, took_ms = asked.elapsed().as_millis() as u64, "apps asked at once at observation's start");
        for pid in pids {
            if answered.contains(&pid) {
                found.extend(self.observe_app_or_queue(pid, &cg, callback));
            } else {
                self.queue(pid, "did not answer at observation's start", true);
            }
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
        self.probes.stop_all();
        // Still held (a scan still reads them), but no longer wanted: a new start wants them.
        self.scan_wanted.clear();
        self.recheck.clear();
        self.gone.borrow_mut().clear();
        if was {
            self.events.push(WinEvent::Stopped);
        }
    }

    /// Apps listed now that are neither observed nor waited for: each is asked on a thread
    /// of its own and observed once it answers ([`Self::queue`]).
    fn observe_all(&mut self, _callback: Callback) {
        let pids: Vec<i32> = self
            .apps
            .values()
            .filter(|a| self.allowed(a) && !self.observed.contains_key(&a.pid) && !self.pending.contains_key(&a.pid))
            .map(|a| a.pid)
            .collect();
        for pid in pids {
            self.queue(pid, "listed", false);
        }
    }

    /// Apps `pids` are read by the launch's recovery scan ([`super::launch_scan`]): none is
    /// observed or asked until [`Self::scan_done`] says its windows are back.
    pub fn hold_for_scan(&mut self, pids: impl IntoIterator<Item = i32>) {
        let now = Instant::now();
        for pid in pids {
            self.scan_held.entry(pid).or_insert(now);
        }
    }

    /// True (and wanted once its scan is done) while app `pid` waits for the launch scan.
    fn held_for_scan(&mut self, pid: i32) -> bool {
        if !self.scan_held.contains_key(&pid) {
            return false;
        }
        self.scan_wanted.insert(pid);
        true
    }

    /// The launch scan read app `pid` (`answered`: it answered; else it did not, or the read
    /// was given up): wanted meanwhile, it is observed now, or asked off the WM thread on the
    /// schedule when it did not answer. Logs, at debug, how long the scan held it.
    pub fn scan_done(&mut self, pid: i32, answered: bool, callback: Callback) {
        let Some(since) = self.scan_held.remove(&pid) else { return };
        let wanted = self.scan_wanted.remove(&pid) && self.observing;
        tracing::debug!(pid, bundle = self.bundle_of(pid), answered, wanted, held_ms = since.elapsed().as_millis() as u64, "launch scan done for the app");
        if !wanted {
            return;
        }
        if answered {
            let cg = cg_lookup();
            for id in self.observe_app_or_queue(pid, &cg, callback) {
                self.found(id, "window found");
            }
        } else {
            self.queue(pid, "did not answer the launch scan", true);
        }
    }

    /// The apps held for the launch scan since before `now - max` (a read that takes that
    /// long: its app is observed without it; its windows still come back when it ends).
    pub fn scan_overdue(&self, now: Instant, max: Duration) -> Vec<i32> {
        self.scan_held.iter().filter(|(_, since)| now.duration_since(**since) >= max).map(|(pid, _)| *pid).collect()
    }

    /// Where the probes' notes go (the WM thread's inbox); until set, apps that do not answer
    /// are retried on the WM thread's once-a-second pass ([`Self::retry_pending`]).
    pub fn set_probe_notify(&mut self, notify: Notify) {
        self.probes.set_notify(notify);
    }

    /// Test only: the question the probes ask, standing in for apps.
    #[cfg(test)]
    fn set_probe_ask(&mut self, ask: observe_probe::Ask) {
        self.probes = Probes::new(ask);
    }

    /// The fast retry cadence on while the login settles (the restore window open), off
    /// after; logged when it changes.
    pub fn set_fast_retry(&mut self, fast: bool) {
        if self.probes.set_fast(fast) {
            let waiting = self.pending.len();
            if fast {
                tracing::debug!(waiting, "apps that do not answer yet: asked every 150 ms while the login settles");
            } else {
                tracing::debug!(waiting, "apps that do not answer yet: asked once a second (the restore window ended)");
            }
        }
    }

    /// App `pid` is waited for from now: asked on a thread of its own, at once or, when it
    /// has just `missed` an ask, on the schedule ([`crate::engine::observe_retry`]); nothing
    /// on the WM thread waits for it.
    fn queue(&mut self, pid: i32, why: &str, missed: bool) {
        if self.held_for_scan(pid) {
            return;
        }
        let now = Instant::now();
        let fast = self.probes.fast();
        let retry = self.pending.entry(pid).or_insert_with(|| Retry::new(now));
        let due = if missed {
            match retry.missed(now, fast) {
                Next::At(due) => due,
                Next::GiveUp => return self.give_up(pid, why),
            }
        } else {
            now
        };
        let retry = *retry;
        if self.probes.start(pid, retry, due) {
            tracing::debug!(pid, bundle = self.bundle_of(pid), why, "an app to observe: asked off the WM thread until it answers");
        }
    }

    /// App `pid` did not answer the WM thread (`e`): asked on its own thread on the schedule,
    /// or given up.
    fn missed(&mut self, pid: i32, e: AxError) {
        let now = Instant::now();
        let fast = self.probes.fast();
        let retry = self.pending.entry(pid).or_insert_with(|| Retry::new(now));
        match retry.missed(now, fast) {
            Next::At(due) => {
                let retry = *retry;
                tracing::debug!(pid, bundle = self.bundle_of(pid), misses = retry.misses(), "cannot observe app yet: {e}; asked again off the WM thread");
                self.probes.start(pid, retry, due);
            }
            Next::GiveUp => self.give_up(pid, &format!("{e}")),
        }
    }

    /// App `pid` is no longer waited for: it did not answer for a minute (`why`).
    fn give_up(&mut self, pid: i32, why: &str) {
        self.probes.stop(pid);
        if let Some(retry) = self.pending.remove(&pid) {
            let waited_s = retry.first().elapsed().as_secs();
            tracing::warn!(pid, bundle = self.bundle_of(pid), misses = retry.misses(), waited_s, "giving up observing app: {why} (an activation tries again)");
        }
    }

    /// A probe's note ([`super::observe_probe`]): an app that answers now is observed (its
    /// windows found), one given up or gone is no longer waited for. A note of a probe stopped
    /// before it came is dropped: the app's running probe, if any, goes on.
    pub fn on_probe(&mut self, note: Note, callback: Callback) {
        let pid = note.pid();
        if !self.probes.current(&note) {
            tracing::debug!(pid, ?note, "a stopped probe's note: dropped");
            return;
        }
        self.probes.stop(pid);
        if !self.observing || !self.pending.contains_key(&pid) || self.observed.contains_key(&pid) {
            self.pending.remove(&pid);
            return;
        }
        match note {
            Note::Answered { retry, .. } => {
                if let Some(r) = self.pending.get_mut(&pid) {
                    *r = retry;
                }
                let cg = cg_lookup();
                for id in self.observe_app_or_queue(pid, &cg, callback) {
                    self.found(id, "window found");
                }
            }
            Note::GaveUp { retry, .. } => {
                if let Some(r) = self.pending.get_mut(&pid) {
                    *r = retry;
                }
                self.give_up(pid, "it did not answer");
            }
            // Its quit is handled by the notice, or found by the next reap ([`Self::reap_gone`]).
            Note::Gone { .. } => {
                self.pending.remove(&pid);
                self.suspect(pid);
            }
        }
    }

    /// Observes `pid`; on failure it is asked again off the WM thread. Returns newly tracked
    /// window ids. Logs, once observed, the ms since the app was first known.
    fn observe_app_or_queue(&mut self, pid: i32, cg: &HashMap<u32, CgWindow>, callback: Callback) -> Vec<u32> {
        if self.observed.contains_key(&pid) || self.held_for_scan(pid) {
            return Vec::new();
        }
        match self.observe_app(pid, cg, callback) {
            Ok(ids) => {
                self.probes.stop(pid);
                let misses = self.pending.remove(&pid).map_or(0, |r| r.misses());
                let (since, known) = self.known.get(&pid).copied().unwrap_or((Instant::now(), "unknown"));
                let listed = cg.values().filter(|w| w.pid == pid && w.layer == 0).count();
                tracing::debug!(
                    pid,
                    bundle = self.bundle_of(pid),
                    known,
                    since_ms = since.elapsed().as_millis() as u64,
                    misses,
                    listed,
                    windows = ids.len(),
                    "observing app"
                );
                ids
            }
            Err(e) => {
                self.missed(pid, e);
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
        // A timeout drops `observer` here, and with it what was registered so far.
        let refused = register_app_level(|name| {
            let asked = Instant::now();
            (observer.add(&element, name, refcon(pid)), asked.elapsed())
        })?;
        for (name, e) in refused {
            tracing::debug!(pid, bundle = self.bundle_of(pid), "observing the app without {name}: {e}");
        }
        let (ax_windows, retry) = windows_or_retry(element.windows());
        if retry {
            // Busy (CannotComplete at the timeout) or not ready: its windows would stay
            // unmanaged until it is activated; the retry passes read the list again.
            tracing::debug!(pid, "AXWindows failed; rescanning the app on the next retry passes");
            self.recheck.entry(pid).or_insert(0);
        }
        self.observed.insert(pid, ObservedApp { element, observer });
        let mut ids: Vec<u32> = ax_windows.into_iter().filter_map(|w| self.track_window(pid, w, cg)).collect();
        // Creation order, so rules see windows in the order the app opened them.
        ids.sort_unstable();
        Ok(ids)
    }

    fn unobserve_app(&mut self, pid: i32) {
        self.pending.remove(&pid);
        self.scan_wanted.remove(&pid);
        self.probes.stop(pid);
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

    /// Rescans apps with windows the window server did not list yet, and retries here the
    /// apps waited for that no probe asks (its thread could not start; called about once a
    /// second). The probes ask the rest ([`super::observe_probe`]).
    pub fn retry_pending(&mut self, callback: Callback) {
        if !self.observing {
            return;
        }
        let recheck: Vec<i32> = self.recheck.keys().copied().collect();
        for pid in recheck {
            let attempts = self.recheck.entry(pid).or_insert(0);
            *attempts += 1;
            if *attempts > RECHECK_ATTEMPTS {
                self.recheck.remove(&pid);
                continue;
            }
            self.rescan_app(pid);
        }
        let pids: Vec<i32> = self.pending.keys().copied().filter(|p| !self.probes.running(*p)).collect();
        if pids.is_empty() {
            return;
        }
        let cg = cg_lookup();
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
            found_at: Instant::now(),
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
        self.windows.get(&id).map_or(Reply::No, |w| self.noted(w.pid, reply(w.element.is_minimized())))
    }

    /// Whether the window is in full screen now (on its own Space; asked through AX, which
    /// still reaches a tracked window off the shown Space for the apps measured).
    pub fn fullscreen_reply(&self, id: u32) -> Reply {
        self.windows.get(&id).map_or(Reply::No, |w| self.noted(w.pid, reply(w.element.is_fullscreen())))
    }

    /// Whether app `pid` is hidden now (⌘H; asked through AX).
    pub fn app_hidden_reply(&self, pid: i32) -> Reply {
        self.observed.get(&pid).map_or(Reply::No, |a| self.noted(pid, reply(a.element.is_hidden())))
    }

    /// `answer` from app `pid`: a timeout or a "cannot tell" makes it a suspect
    /// ([`Self::suspect`]).
    fn noted(&self, pid: i32, answer: Reply) -> Reply {
        if matches!(answer, Reply::TimedOut | Reply::CannotTell) {
            self.suspect(pid);
        }
        answer
    }

    /// An Accessibility call to app `pid` failed or timed out: whether its process still
    /// exists is tested at the next pass ([`Self::reap_gone`]).
    pub fn suspect(&self, pid: i32) {
        self.gone.borrow_mut().suspect(pid);
    }

    /// Apps gone without a termination notice (round missed-quit, [`crate::engine::gone`]):
    /// tests the pids of apps whose Accessibility calls failed or timed out since the last
    /// pass, and on a `full` pass (about once a second) the pids of apps with tracked windows;
    /// a process that no longer exists is handled as its termination notice would be (its
    /// windows reported destroyed, then the app terminated), logged at info. Returns how many.
    pub fn reap_gone(&mut self, full: bool) -> usize {
        if !self.observing {
            self.gone.borrow_mut().clear();
            return 0;
        }
        let window_pids: Vec<i32> = self.windows.values().map(|w| w.pid).collect();
        let candidates = self.gone.borrow_mut().candidates(window_pids, full, |p| {
            self.apps.contains_key(&p) || self.observed.contains_key(&p) || self.pending.contains_key(&p)
        });
        let dead = gone::gone(candidates, process_gone);
        for &pid in &dead {
            let app = self.apps.get(&pid).cloned().unwrap_or(AppInfo { pid, bundle_id: None, name: None });
            tracing::info!(
                pid,
                bundle = app.bundle_or_unknown(),
                name = app.name.as_deref().unwrap_or("?"),
                "app gone without a termination notice: its process no longer exists; handled as its quit"
            );
            self.terminated(app);
        }
        dead.len()
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
                let id = self.find_window(pid, &element).or_else(|| self.refocus_dead(pid, &element)).or_else(|| {
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
                self.events.push(WinEvent::AppTookActivation(pid));
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
            n::WINDOW_DEMINIATURIZED => {
                if let Some(id) = self.find_window(pid, &element) {
                    tracing::debug!(id, pid, "window un-minimized");
                    self.events.push(WinEvent::Deminiaturized(id));
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
                let now = Instant::now();
                self.last_launch = Some(now);
                let pid = app.pid;
                let allowed = self.allowed(&app);
                self.apps.insert(pid, app);
                self.known.entry(pid).or_insert((now, "launched"));
                // Asked off the WM thread, observed once it answers (a launching app rarely
                // answers at its notice; one that does is observed within a few ms).
                if self.observing && allowed && !self.observed.contains_key(&pid) && !self.pending.contains_key(&pid) {
                    self.queue(pid, "launched", false);
                }
            }
            AppEvent::Terminated(app) => {
                let known = self.apps.contains_key(&app.pid) || self.observed.contains_key(&app.pid);
                if known {
                    self.log_app("app terminated", &app);
                    self.terminated(app);
                    return;
                }
                // A notice for a pid we never knew (any app quitting: the notice covers every
                // activation policy). Logged, so a missed quit tells which side lost it; the
                // known apps of its bundle id whose process is gone quit with it.
                tracing::debug!(pid = app.pid, bundle = app.bundle_or_unknown(), "app terminated: a pid not known");
                if let Some(bundle) = app.bundle_id.as_deref() {
                    for pid in self.pids_with_bundle(bundle) {
                        if process_gone(pid) {
                            let info = self.apps.get(&pid).cloned().unwrap_or(AppInfo { pid, ..app.clone() });
                            tracing::info!(pid, notice_pid = app.pid, bundle, "app terminated: the notice named another pid of its bundle id; this one is gone too");
                            self.terminated(info);
                        }
                    }
                }
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
                self.known.entry(pid).or_insert((at, "activated"));
                if self.observing && allowed {
                    // One waited for is asked by its probe: the WM thread does not wait for it.
                    // Any other (given up, or never known) is tried at once, as before.
                    if !self.observed.contains_key(&pid) && !self.pending.contains_key(&pid) {
                        let cg = cg_lookup();
                        for id in self.observe_app_or_queue(pid, &cg, callback) {
                            self.found(id, "window found");
                        }
                    }
                    self.events.push(WinEvent::AppActivated(pid, at));
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

    /// App `app` quit (a termination notice, or its process found gone): forgotten, its
    /// windows reported destroyed, then [`WinEvent::AppTerminated`].
    fn terminated(&mut self, app: AppInfo) {
        self.activated_at.remove(&app.pid);
        self.known.remove(&app.pid);
        self.apps.remove(&app.pid);
        self.unobserve_app(app.pid);
        if self.observing {
            self.events.push(WinEvent::AppTerminated(app.pid));
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

/// True when process `pid` no longer exists: the kernel answers "no such process" (a process
/// of another user answers "not permitted": it exists).
fn process_gone(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists; nothing is sent.
    let r = unsafe { libc::kill(pid, 0) };
    r == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
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

    unsafe extern "C-unwind" fn no_callback(_: NonNull<AXObserver>, _: NonNull<AXUIElement>, _: NonNull<CFString>, _: *mut c_void) {}

    /// The 11-10 login: an app answered the first registration and then none (each waited out
    /// the messaging timeout on the WM thread). The first timeout ends the try, wherever it
    /// comes; the app is registered whole once it answers.
    #[test]
    fn an_app_that_stops_answering_between_registrations_is_not_waited_for() {
        const TIMEOUT: Duration = Duration::from_millis(250);
        const AT_ONCE: Duration = Duration::from_millis(2);
        let asked = RefCell::new(Vec::new());
        let stalls_after_the_first = |name: &'static str| {
            asked.borrow_mut().push(name);
            if asked.borrow().len() == 1 { (Ok(()), AT_ONCE) } else { (Err(AxError::CANNOT_COMPLETE), TIMEOUT) }
        };
        assert_eq!(register_app_level(stalls_after_the_first), Err(AxError::CANNOT_COMPLETE));
        assert_eq!(asked.borrow().len(), 2, "the third is not asked: it would wait out the timeout too");
        // Not answering at all: one ask, however fast the failure, as before.
        for took in [TIMEOUT, AT_ONCE] {
            let mut n = 0;
            let silent = |_: &'static str| {
                n += 1;
                (Err(AxError::CANNOT_COMPLETE), took)
            };
            assert_eq!(register_app_level(silent), Err(AxError::CANNOT_COMPLETE));
            assert_eq!(n, 1);
        }
        // An app that answers and lacks one notification (or says at once it cannot) is
        // observed without it, and the caller told.
        for e in [AxError::Api(-25207), AxError::CANNOT_COMPLETE] {
            let lacks_one = |name: &'static str| if name == n::APPLICATION_ACTIVATED { (Err(e), AT_ONCE) } else { (Ok(()), AT_ONCE) };
            assert_eq!(register_app_level(lacks_one), Ok(vec![(n::APPLICATION_ACTIVATED, e)]));
        }
        assert_eq!(register_app_level(|_| (Ok(()), AT_ONCE)), Ok(Vec::new()));
        // One that takes none is not observed.
        assert_eq!(register_app_level(|_| (Err(AxError::Api(-25207)), AT_ONCE)), Err(AxError::Api(-25207)));
    }

    /// The pid of a process that has exited (a child run and waited for).
    fn a_gone_pid() -> i32 {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().expect("runs /usr/bin/true");
        let pid = child.id() as i32;
        child.wait().expect("waits for it");
        pid
    }

    /// An observer that observes, knowing `apps` (none of them observed through AX).
    fn observing(apps: Vec<AppInfo>) -> Observer {
        let mut ob = Observer::new(1, None);
        ob.set_apps(apps, no_callback);
        ob.observing = true;
        ob
    }

    #[test]
    fn a_suspect_whose_process_is_gone_quits_once_and_a_live_one_stays() {
        // 10-23: Home quit and no termination notice came; its calls failed.
        let gone = a_gone_pid();
        let me = std::process::id() as i32;
        let home = AppInfo { pid: gone, bundle_id: Some("com.example.home".into()), name: Some("Home".into()) };
        let busy = AppInfo { pid: me, bundle_id: Some("com.example.busy".into()), name: Some("Busy".into()) };
        let mut ob = observing(vec![home, busy]);
        ob.suspect(gone);
        ob.suspect(me);
        assert_eq!(ob.reap_gone(false), 1);
        assert_eq!(ob.take_events(), vec![WinEvent::AppTerminated(gone)]);
        assert!(ob.bundle_id(gone).is_none(), "forgotten");
        assert_eq!(ob.bundle_id(me), Some("com.example.busy"), "a live app stays");
        ob.suspect(gone);
        assert_eq!(ob.reap_gone(true), 0, "handled once");
        assert!(ob.take_events().is_empty());
    }

    #[test]
    fn nothing_is_reaped_while_not_observing() {
        let gone = a_gone_pid();
        let mut ob = Observer::new(1, None);
        ob.set_apps(vec![AppInfo { pid: gone, bundle_id: None, name: None }], no_callback);
        ob.suspect(gone);
        assert_eq!(ob.reap_gone(true), 0);
        assert!(ob.take_events().is_empty());
    }

    #[test]
    fn a_notice_for_an_unknown_pid_takes_the_gone_apps_of_its_bundle() {
        let gone = a_gone_pid();
        let me = std::process::id() as i32;
        let home = AppInfo { pid: gone, bundle_id: Some("com.example.home".into()), name: Some("Home".into()) };
        let other = AppInfo { pid: me, bundle_id: Some("com.example.home".into()), name: Some("Home".into()) };
        let mut ob = observing(vec![home.clone(), other]);
        let notice = AppInfo { pid: 999_999, ..home };
        ob.on_app_event(AppEvent::Terminated(notice), no_callback);
        assert_eq!(ob.take_events(), vec![WinEvent::AppTerminated(gone), WinEvent::AppTerminated(999_999)]);
        assert_eq!(ob.pids_with_bundle("com.example.home"), vec![me], "the live one of the bundle stays");
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

    /// Round login-observe: apps standing in for the 11-06 login's. The stuck one waits out
    /// the 0.25 s timeout at every ask and never answers; the other answers at once.
    const STUCK: i32 = 5001;
    const READY: i32 = 5002;

    fn stand_in(pid: i32) -> observe_probe::Answer {
        if pid == STUCK {
            std::thread::sleep(Duration::from_millis(250));
            return observe_probe::Answer::NotYet;
        }
        observe_probe::Answer::Answers
    }

    /// An observing observer whose probes ask [`stand_in`], their notes on a channel.
    fn probing() -> (Observer, std::sync::mpsc::Receiver<Note>) {
        let mut ob = observing(Vec::new());
        ob.set_probe_ask(stand_in);
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        ob.set_probe_notify(std::sync::Arc::new(move |note| {
            let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(note);
        }));
        (ob, rx)
    }

    #[test]
    fn a_launch_of_an_app_that_does_not_answer_holds_up_neither_the_wm_thread_nor_another_app() {
        let (mut ob, notes) = probing();
        let t0 = Instant::now();
        ob.on_app_event(AppEvent::Launched(app(STUCK, Some("com.example.stuck"))), no_callback);
        ob.on_app_event(AppEvent::Launched(app(READY, Some("com.example.ready"))), no_callback);
        let took = t0.elapsed();
        assert!(took < Duration::from_millis(50), "the notices are handled at once ({took:?})");
        assert!(ob.pending.contains_key(&STUCK) && ob.pending.contains_key(&READY));
        // The ready app's answer comes while the stuck one's first ask still waits.
        let note = notes.recv_timeout(Duration::from_millis(200)).expect("the ready app's answer");
        assert!(matches!(note, Note::Answered { pid: READY, .. }), "{note:?}");
        assert!(t0.elapsed() < Duration::from_millis(200));
        assert!(ob.probes.running(STUCK), "the stuck one is still asked");
        // A second launch notice for it starts no second probe and costs nothing.
        ob.on_app_event(AppEvent::Launched(app(STUCK, Some("com.example.stuck"))), no_callback);
        assert!(notes.recv_timeout(Duration::from_millis(400)).is_err(), "the stuck app sends nothing");
        ob.stop();
        assert!(ob.pending.is_empty() && !ob.probes.running(STUCK));
    }

    #[test]
    fn an_activation_of_an_app_waited_for_does_not_ask_it_on_the_wm_thread() {
        let (mut ob, _notes) = probing();
        ob.on_app_event(AppEvent::Launched(app(STUCK, Some("com.example.stuck"))), no_callback);
        let t0 = Instant::now();
        ob.on_app_event(AppEvent::Activated(app(STUCK, Some("com.example.stuck")), Instant::now()), no_callback);
        // Before, Slack's activation at the 11-06 login came 3.3 s late behind such tries.
        assert!(t0.elapsed() < Duration::from_millis(50));
        assert_eq!(ob.take_events(), vec![WinEvent::AppActivated(STUCK, ob.activated_at(STUCK).unwrap_or(t0))]);
        assert!(ob.pending.contains_key(&STUCK) && !ob.observes(STUCK));
        ob.stop();
    }

    /// The number of the probe asking `pid` now.
    fn probe_of(ob: &Observer, pid: i32) -> u64 {
        ob.probes.probe_of(pid).expect("a probe asks it")
    }

    #[test]
    fn a_probes_note_for_an_app_no_longer_waited_for_is_dropped() {
        let (mut ob, _notes) = probing();
        let now = Instant::now();
        // Given up: forgotten, an activation tries it again.
        ob.on_app_event(AppEvent::Launched(app(STUCK, Some("com.example.stuck"))), no_callback);
        let probe = probe_of(&ob, STUCK);
        ob.on_probe(Note::GaveUp { pid: STUCK, probe, retry: Retry::new(now) }, no_callback);
        assert!(!ob.pending.contains_key(&STUCK) && !ob.probes.running(STUCK));
        // A note for an app that quit meanwhile (not waited for): nothing observed.
        ob.on_probe(Note::Answered { pid: READY, probe: 1, retry: Retry::new(now) }, no_callback);
        assert!(!ob.observes(READY) && ob.take_events().is_empty());
        // Its process gone: no longer waited for, and a suspect for the next reap.
        ob.pending.insert(READY, Retry::new(now));
        ob.probes.start(READY, Retry::new(now), now + Duration::from_secs(60));
        let probe = probe_of(&ob, READY);
        ob.on_probe(Note::Gone { pid: READY, probe }, no_callback);
        assert!(!ob.pending.contains_key(&READY));
        ob.stop();
    }

    /// The 1.7 review: a probe stopped with observation (a logout's head start) may still send
    /// its note (it was mid-ask); after a cancelled logout observation starts again and a new
    /// probe asks the same app. The old probe's GaveUp stopped the new one and gave the app up:
    /// never observed until activated. Now it is dropped and the new probe goes on.
    #[test]
    fn an_old_probes_give_up_leaves_the_fresh_probe_asking() {
        let (mut ob, _notes) = probing();
        let now = Instant::now();
        ob.on_app_event(AppEvent::Launched(app(STUCK, Some("com.example.stuck"))), no_callback);
        let old = probe_of(&ob, STUCK);
        ob.stop();
        ob.observing = true;
        ob.on_app_event(AppEvent::Launched(app(STUCK, Some("com.example.stuck"))), no_callback);
        assert!(ob.probes.running(STUCK));
        ob.on_probe(Note::GaveUp { pid: STUCK, probe: old, retry: Retry::new(now) }, no_callback);
        assert!(ob.pending.contains_key(&STUCK), "still waited for");
        assert!(ob.probes.running(STUCK), "the fresh probe still asks");
        ob.stop();
    }

    /// L1: an app the launch's recovery scan has not read yet is neither asked nor observed
    /// (a window a crash left parked comes back first); one read is asked at once, whatever
    /// the apps still read; one that did not answer the scan is asked on the schedule.
    #[test]
    fn an_app_waits_for_its_own_launch_scan_only() {
        let (mut ob, notes) = probing();
        const LATER: i32 = 5003;
        ob.hold_for_scan([STUCK, READY, LATER]);
        ob.on_app_event(AppEvent::Launched(app(READY, Some("com.example.ready"))), no_callback);
        ob.on_app_event(AppEvent::Activated(app(STUCK, Some("com.example.stuck")), Instant::now()), no_callback);
        ob.on_app_event(AppEvent::Launched(app(LATER, Some("com.example.later"))), no_callback);
        assert!(ob.pending.is_empty() && !ob.probes.running(READY) && !ob.probes.running(STUCK), "held: nothing asked");
        assert!(!ob.observes(READY) && !ob.observes(STUCK));
        // The ready app's scan is done while the stuck one's still reads: asked now.
        let t0 = Instant::now();
        ob.scan_done(READY, false, no_callback);
        assert!(ob.probes.running(READY) && !ob.probes.running(STUCK));
        let note = notes.recv_timeout(Duration::from_millis(400)).expect("the ready app's answer");
        assert!(matches!(note, Note::Answered { pid: READY, .. }), "{note:?}");
        assert!(t0.elapsed() < Duration::from_millis(400));
        // A held app that quit is no longer wanted when its scan is done.
        ob.on_app_event(AppEvent::Terminated(app(LATER, Some("com.example.later"))), no_callback);
        ob.scan_done(LATER, false, no_callback);
        assert!(!ob.pending.contains_key(&LATER) && !ob.probes.running(LATER));
        // Overdue: a read holding its app too long is let go.
        let later = Instant::now() + Duration::from_secs(11);
        assert_eq!(ob.scan_overdue(later, Duration::from_secs(10)), vec![STUCK]);
        ob.scan_done(STUCK, false, no_callback);
        assert!(ob.pending.contains_key(&STUCK), "asked on the schedule");
        assert!(ob.scan_overdue(later, Duration::from_secs(10)).is_empty());
        ob.stop();
    }

    /// A start of observation leaves the held apps to their scans; a stop forgets them as
    /// wanted, and a scan done while stopped observes nothing.
    #[test]
    fn a_held_app_waits_through_a_stop() {
        let (mut ob, _notes) = probing();
        ob.hold_for_scan([STUCK]);
        ob.on_app_event(AppEvent::Activated(app(STUCK, Some("com.example.stuck")), Instant::now()), no_callback);
        assert!(ob.scan_wanted.contains(&STUCK));
        ob.stop();
        assert!(ob.scan_wanted.is_empty() && ob.scan_held.contains_key(&STUCK));
        ob.scan_done(STUCK, false, no_callback);
        assert!(ob.pending.is_empty() && !ob.probes.running(STUCK) && ob.scan_held.is_empty());
    }

    #[test]
    fn the_cadence_follows_the_restore_window() {
        let (mut ob, _notes) = probing();
        assert!(ob.probes.fast(), "fast as observation starts (the login settles)");
        ob.set_fast_retry(false);
        assert!(!ob.probes.fast());
        ob.set_fast_retry(true);
        assert!(ob.probes.fast());
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
