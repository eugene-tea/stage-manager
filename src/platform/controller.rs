//! The controller: owns the engine's [`Model`] on the WM thread, feeds it what the
//! [`Observer`] sees and executes the [`Action`]s it returns through Accessibility.
//!
//! - **Observations** for the engine: live frames through AX for every window the model
//!   shows, z-order from `CGWindowListCopyWindowInfo`, the focused window from the frontmost
//!   app's `AXFocusedWindow`.
//! - **Actions**: `SetFrame` = size, position, size; `Park` = position into the park zone;
//!   `Raise` = `AXRaise`; `Focus` = raise, `AXMain`, then the app's `AXFrontmost`.
//! - **Echoes**: focus and activation events for windows and apps we just touched are our
//!   own actions coming back late; they are dropped for [`ECHO_WINDOW`] so a stale focus
//!   never bounces us back to the stage we left.
//! - **Safety**: the parked-window registry is synced around every batch. A batch stops
//!   between actions once Quit is requested ([`safety::quit_requested`]), so Quit is
//!   answered within one action's time.
//! - **Stage area**: while the strip shows, placed windows are kept clear of it (moved
//!   right, narrowed when too wide unless `[stages] fit_windows = false`); their pre-fit
//!   frames come back on quit ([`FitOptions`]).
//! - After every state change: the strip snapshot ([`strip_model::present`]) and a debounced
//!   `state.json` save.
//! - **Apple's defaults**: the first start groups windows one stage per app; new windows
//!   follow the `native` rule action; app activation switches to the app's stage.
//! - **Drag and drop**: a managed window dragged by its title bar while the left button is
//!   down (AX move events between a global mouse-down and mouse-up) is announced to the strip,
//!   which shows a drop target; released over the strip, it gets a stage of its own and keeps
//!   its pre-drag frame. A card dragged out of the strip arrives as
//!   [`StripEvent::CardDropped`].
//! - **Shift sugar** (`[sugar] shift_brings_here`): Shift held at the user's triggering
//!   action ([`Triggers`]) brings windows here: a newly opened app's first window joins the
//!   current stage, and an activation (Dock click) of an app in another stage brings its
//!   windows here. The app switcher (Command held) never counts.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use super::ax::AxElement;
use super::ctl::Command;
use super::input::{self, InputEvent, InputKind, Modifiers, Triggers};
use super::native_settings::RawSettings;
use super::observe::{Observer, WinEvent, WindowKind};
use super::safety::{self, ParkedEntry};
use super::{desktop, paths, windows};
use crate::config::{Config, ConfigStore, FileAction, FitMode, HotkeyCommand, HotkeyConfig, StripStyle};
use crate::engine::geometry::{Point, Rect};
use crate::engine::model::{
    Action, Display, DisplayId, Home, Model, Observation, StageId, Target, WindowId, WindowMeta, WindowRole,
};
use crate::engine::stage_area::{FitOptions, capacity_for, stage_inset, strip_width};
use crate::engine::persist::{LaunchHints, STATE_VERSION, StateFile, rematch};
use crate::engine::rules::{Decision, RuleAction};
use crate::engine::settings::{NativeSettings, PrefValue};
use crate::ui::strip_model::{self, CardSnapshot, CardWindow, StripEvent, StripSnapshot};

/// How long focus and activation events for windows and apps we just touched are ignored
/// by default (`[windows] focus_echo_ms`).
pub const ECHO_WINDOW: Duration = Duration::from_millis(700);

/// Quiet time before `state.json` is written after a change.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// A busy stream of changes is still saved at least this often.
const SAVE_MAX_DELAY: Duration = Duration::from_secs(3);

/// How often config.toml's modification time is checked.
const CONFIG_POLL: Duration = Duration::from_secs(1);

/// Updates for the main thread (menu and hotkeys).
#[derive(Clone, Debug, PartialEq)]
pub enum UiUpdate {
    /// The config in force: its load error (the last good config stays) and its hotkeys.
    Config {
        /// The load error, if the file is bad.
        error: Option<String>,
        /// Hotkeys to register.
        hotkeys: HotkeyConfig,
        /// The strip's thumbnail refresh interval.
        thumbnail_interval_ms: u64,
    },
    /// The strip's Show/Hide state.
    StripVisible(bool),
}

type UiSink = Box<dyn Fn(UiUpdate) + Send + Sync>;

fn ui_sink() -> &'static Mutex<Option<UiSink>> {
    static S: OnceLock<Mutex<Option<UiSink>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// Registers the main thread's receiver of [`UiUpdate`]s (it must hop to the main thread).
pub fn set_ui_sink(f: impl Fn(UiUpdate) + Send + Sync + 'static) {
    if let Ok(mut s) = ui_sink().lock() {
        *s = Some(Box::new(f));
    }
}

fn notify_ui(update: UiUpdate) {
    if let Ok(s) = ui_sink().lock()
        && let Some(f) = s.as_ref()
    {
        f(update);
    }
}

/// Recently touched windows and apps, whose focus and activation events are our echoes.
#[derive(Debug)]
pub struct Echoes {
    windows: HashMap<u32, Instant>,
    apps: HashMap<i32, Instant>,
    window: Duration,
}

impl Default for Echoes {
    fn default() -> Self {
        Self { windows: HashMap::new(), apps: HashMap::new(), window: ECHO_WINDOW }
    }
}

impl Echoes {
    /// Sets how long an echo lasts.
    pub fn set_window(&mut self, window: Duration) {
        self.window = window;
    }

    /// We just moved, raised or focused this window.
    pub fn touch_window(&mut self, id: u32, now: Instant) {
        self.windows.insert(id, now);
    }

    /// We just made this app frontmost.
    pub fn touch_app(&mut self, pid: i32, now: Instant) {
        self.apps.insert(pid, now);
    }

    /// True when a focus event for `id` is probably our own.
    pub fn is_window_echo(&self, id: u32, now: Instant) -> bool {
        self.windows.get(&id).is_some_and(|t| now.duration_since(*t) < self.window)
    }

    /// True when an activation of `pid` is probably our own.
    pub fn is_app_echo(&self, pid: i32, now: Instant) -> bool {
        self.apps.get(&pid).is_some_and(|t| now.duration_since(*t) < self.window)
    }

    /// Forgets old entries.
    pub fn prune(&mut self, now: Instant) {
        self.windows.retain(|_, t| now.duration_since(*t) < self.window);
        self.apps.retain(|_, t| now.duration_since(*t) < self.window);
    }
}

/// Interprets raw `com.apple.WindowManager` values.
pub fn native_settings(raw: &RawSettings) -> NativeSettings {
    let values: HashMap<String, PrefValue> = raw.iter().map(|(k, v)| (k.clone(), PrefValue::Int(*v))).collect();
    NativeSettings::from_values(&values)
}

/// True when our wallpaper-click reveal should run: macOS's own "Click wallpaper to reveal
/// desktop" set to Always (`EnableStandardClickToShowDesktop = 1`) already moves every
/// window aside outside Apple's Stage Manager, and both at once would fight.
pub fn wallpaper_reveal_enabled(s: &NativeSettings) -> bool {
    s.click_wallpaper_reveals_desktop && !s.macos_reveals_desktop_outside_stage_manager
}

/// The left button went down in another app: the restore frames of the windows shown then,
/// so a window drag that follows knows where the window was before.
#[derive(Debug, Default)]
struct Press {
    frames: HashMap<u32, Rect>,
}

/// A managed window being dragged by the user.
#[derive(Debug, Clone, Copy)]
struct WindowDrag {
    id: u32,
    pid: i32,
    /// Its frame before the drag.
    pre: Rect,
    /// False when the drag resizes the window (its size changed); only moves can be dropped.
    moving: bool,
}

/// The engine plus everything around it on the WM thread.
pub struct Controller {
    model: Option<Model>,
    /// The strip's look, shared with every snapshot.
    style: Arc<StripStyle>,
    store: ConfigStore,
    config_error: Option<String>,
    config_mtime: Option<SystemTime>,
    config_checked: Instant,
    settings: NativeSettings,
    displays: Vec<Display>,
    strip_visible: bool,
    echoes: Echoes,
    dirty: Option<(Instant, Instant)>,
    strip_dirty: bool,
    triggers: Triggers,
    press: Option<Press>,
    drag: Option<WindowDrag>,
    /// The first snapshot after the launch's stages are built asks the strip to show itself
    /// for a moment even when a window covers it (once per launch).
    announce: bool,
}

/// How placed windows are kept clear of the strip on a display with `visible` frame: never
/// by default (Apple leaves every window's frame alone); with `[windows] fit` only while the
/// strip shows (the user's toggle on, Apple's "Show recent apps" on).
pub fn fit_options(visible: Rect, strip_visible: bool, settings: &NativeSettings, config: &Config) -> FitOptions {
    let on = config.windows.fit != FitMode::Off && strip_visible && !settings.hide_recent_apps;
    FitOptions {
        inset: if on { stage_inset(visible.size(), &config.style, config.windows.fit_margin) } else { 0.0 },
        shrink: config.windows.fit == FitMode::Narrow,
    }
}

/// The strip's card count on a display with `visible` frame.
pub fn strip_capacity(visible: Rect, config: &Config) -> usize {
    capacity_for(visible.size(), &config.strip.capacity, &config.style)
}

impl Controller {
    /// Loads config (writing the default file on first run) and tells the main thread.
    pub fn new() -> Self {
        let path = paths::config_file();
        let (store, result) = ConfigStore::open(path.clone());
        let config_error = match result {
            Ok((warnings, action)) => {
                for w in warnings {
                    tracing::warn!("config: {w}");
                }
                match action {
                    FileAction::Created => tracing::info!("config: wrote the commented default file {}", path.display()),
                    FileAction::Upgraded => tracing::info!(
                        "config: {} was an unedited default file of an earlier version; replaced it with today's commented one",
                        path.display()
                    ),
                    FileAction::Read => {}
                }
                tracing::info!("config loaded from {}", path.display());
                None
            }
            Err(e) => {
                tracing::error!("config: {e}; using the built-in defaults");
                Some(e.to_string())
            }
        };
        let style = Arc::new(store.config().style.clone());
        let mut echoes = Echoes::default();
        echoes.set_window(Duration::from_millis(store.config().windows.focus_echo_ms));
        let this = Self {
            style,
            model: None,
            config_mtime: mtime(&path),
            store,
            config_error,
            config_checked: Instant::now(),
            settings: NativeSettings::default(),
            displays: Vec::new(),
            strip_visible: true,
            echoes,
            dirty: None,
            strip_dirty: false,
            triggers: Triggers::default(),
            press: None,
            drag: None,
            announce: false,
        };
        this.notify_config();
        this
    }

    fn notify_config(&self) {
        let config = self.store.config();
        notify_ui(UiUpdate::Config {
            error: self.config_error.clone(),
            hotkeys: config.hotkeys.clone(),
            thumbnail_interval_ms: config.strip.thumbnail_interval_ms,
        });
    }

    /// True once the model exists (observation started).
    pub fn managing(&self) -> bool {
        self.model.is_some()
    }

    // ----- observation -------------------------------------------------------------------

    fn main_display_id(&self) -> DisplayId {
        self.model
            .as_ref()
            .map(|m| m.main_display().id)
            .or(self.displays.first().map(|d| d.id))
            .unwrap_or(DisplayId(0))
    }

    /// The engine's view of a tracked window. A child's parent is the frontmost standard
    /// window of the same app (AX does not say which window a dialog or panel belongs to).
    fn meta(&self, ob: &Observer, id: u32, z: &HashMap<u32, usize>) -> Option<WindowMeta> {
        let w = ob.window(id)?;
        let role = match w.kind {
            WindowKind::Standard => WindowRole::Standard,
            WindowKind::Child => {
                let parent = ob
                    .windows()
                    .values()
                    .filter(|o| o.pid == w.pid && o.id != w.id && o.kind == WindowKind::Standard)
                    .min_by_key(|o| (z.get(&o.id).copied().unwrap_or(usize::MAX), std::cmp::Reverse(o.id)))
                    .map(|o| WindowId(o.id));
                WindowRole::Child { parent }
            }
        };
        Some(WindowMeta {
            id: WindowId(id),
            pid: w.pid,
            bundle_id: ob.bundle_id(w.pid).unwrap_or_default().to_string(),
            title: w.title.clone(),
            role,
            frame: w.frame,
            display: self.main_display_id(),
        })
    }

    /// Live frames of every shown managed window, the z-order and the focused window.
    fn observe(&self, ob: &mut Observer) -> Observation {
        let Some(m) = &self.model else { return Observation::default() };
        let list = windows::list_windows();
        let z_order: Vec<WindowId> = list
            .iter()
            .filter(|w| w.onscreen && w.layer == 0)
            .map(|w| WindowId(w.id))
            .filter(|id| m.window(*id).is_some())
            .collect();
        let shown: Vec<u32> = m.windows().filter(|w| !w.parked).map(|w| w.meta.id.0).collect();
        let mut frames = HashMap::new();
        for id in shown {
            if let Some(f) = ob.refresh_frame(id) {
                frames.insert(WindowId(id), f);
            }
        }
        Observation { frames, z_order, focused: focused_window().map(WindowId) }
    }

    // ----- executing actions -------------------------------------------------------------

    /// Registry entries: parked windows and windows we fitted, with where safety paths
    /// put them back (a fitted window's exact pre-fit frame).
    fn parked_entries(&self, ob: &Observer) -> BTreeMap<u32, ParkedEntry> {
        let Some(m) = &self.model else { return BTreeMap::new() };
        m.safety_targets()
            .into_iter()
            .filter_map(|(id, restore)| {
                let w = ob.window(id.0)?;
                let child = w.kind == WindowKind::Child;
                Some((id.0, ParkedEntry { element: w.element.clone(), restore, child }))
            })
            .collect()
    }

    /// Keeps the model's stage-area fitting in step with the strip, Apple's settings and
    /// config.
    fn sync_fit(&mut self) {
        let (strip_visible, settings) = (self.strip_visible, self.settings);
        let config = self.store.config();
        if let Some(m) = self.model.as_mut() {
            let fit = fit_options(m.main_display().visible, strip_visible, &settings, config);
            m.set_fit(fit);
        }
    }

    /// Executes actions in order; returns how many AX calls failed. Stops between actions
    /// once Quit is requested (the model's state stays safe: unpark-all covers every window
    /// it counts as parked, whether or not its park was carried out).
    fn execute(&mut self, ob: &Observer, actions: Vec<Action>) -> usize {
        self.execute_batch(ob, actions, true)
    }

    fn execute_batch(&mut self, ob: &Observer, actions: Vec<Action>, interruptible: bool) -> usize {
        if actions.is_empty() {
            return 0;
        }
        let parked = self.parked_entries(ob);
        safety::registry_add(parked.clone());
        let now = Instant::now();
        let mut failed = 0;
        let mut changed = false;
        let total = actions.len();
        for (done, action) in actions.into_iter().enumerate() {
            if interruptible && action != Action::StateChanged && safety::quit_requested() {
                tracing::info!(done, total, "quit requested: the rest of this batch is skipped");
                changed = true;
                break;
            }
            let id = match action {
                Action::SetFrame { id, .. } | Action::Park { id, .. } | Action::Raise { id } | Action::Focus { id } => id.0,
                Action::StateChanged => {
                    changed = true;
                    continue;
                }
            };
            let Some(w) = ob.window(id) else {
                tracing::debug!(id, ?action, "action for an untracked window skipped");
                continue;
            };
            self.echoes.touch_window(id, now);
            let result = match action {
                Action::SetFrame { frame, .. } => w.element.set_frame(frame),
                Action::Park { frame, .. } => w.element.set_position(frame.origin()),
                Action::Raise { .. } => w.element.raise(),
                Action::Focus { .. } => {
                    self.echoes.touch_app(w.pid, now);
                    let _ = w.element.raise();
                    let main = w.element.set_main(true);
                    let front = AxElement::application(w.pid).set_frontmost(true);
                    main.and(front)
                }
                Action::StateChanged => Ok(()),
            };
            if let Err(e) = result {
                failed += 1;
                tracing::warn!(id, ?action, "action failed: {e}");
            } else {
                tracing::trace!(id, ?action, "done");
            }
        }
        safety::registry_replace(parked);
        if changed {
            self.mark_dirty();
            self.present();
        }
        failed
    }

    /// After a caught panic: make sure every window the model has parked is in the registry
    /// (entries already there stay, since a batch may have stopped halfway).
    pub fn resync_registry(&self, ob: &Observer) {
        safety::registry_add(self.parked_entries(ob));
    }

    fn mark_dirty(&mut self) {
        let now = Instant::now();
        self.dirty = Some((self.dirty.map_or(now, |d| d.0), now));
    }

    // ----- strip -------------------------------------------------------------------------

    /// Builds the strip snapshot and hands it to the strip.
    pub fn present(&mut self) {
        self.strip_dirty = false;
        let Some(m) = &self.model else { return };
        let active = m.main_active_stage();
        let cards = m
            .strip()
            .into_iter()
            .map(|c| CardSnapshot {
                stage: c.stage.0,
                number: c.number,
                name: c.name,
                windows: c
                    .windows
                    .iter()
                    .filter_map(|id| m.window(*id))
                    .take(3)
                    .map(|w| CardWindow { id: w.meta.id.0, pid: w.meta.pid, frame: w.restore })
                    .collect(),
                apps: c.apps,
            })
            .collect();
        let dragged = self.drag.filter(|d| d.moving);
        let active_frames = m
            .windows()
            .filter(|w| !w.parked && (w.home == Home::Pinned || active.is_some_and(|a| w.home == Home::Stage(a))))
            .filter(|w| dragged.is_none_or(|d| d.id != w.meta.id.0))
            .map(|w| w.meta.frame)
            .collect();
        strip_model::present(StripSnapshot {
            cards,
            active_stage: active.map(|s| s.0),
            hide_recent_apps: m.strip_auto_hides(),
            user_visible: self.strip_visible,
            style: self.style.clone(),
            active_frames,
            window_drag: dragged.map(|d| CardWindow { id: d.id, pid: d.pid, frame: d.pre }),
            announce: std::mem::take(&mut self.announce),
        });
    }

    // ----- persistence -------------------------------------------------------------------

    /// Writes `state.json` now (atomically).
    pub fn save_now(&mut self) {
        self.dirty = None;
        let Some(m) = &self.model else { return };
        let path = paths::state_file();
        let text = m.to_state().to_json();
        let tmp = path.with_extension("json.tmp");
        let result = std::fs::create_dir_all(paths::config_dir())
            .and_then(|_| std::fs::write(&tmp, text))
            .and_then(|_| std::fs::rename(&tmp, &path));
        match result {
            Ok(()) => tracing::debug!("state saved to {}", path.display()),
            Err(e) => tracing::warn!("cannot save {}: {e}", path.display()),
        }
    }

    fn load_state() -> Option<StateFile> {
        let path = paths::state_file();
        let text = std::fs::read_to_string(&path).ok()?;
        match StateFile::from_json(&text) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!("{e}; starting without saved stages");
                None
            }
        }
    }

    // ----- events ------------------------------------------------------------------------

    /// Handles a batch of observer events. Runs of `Found` are taken in window-id order
    /// (creation order), so rules see windows in the order the app opened them.
    pub fn on_events(&mut self, ob: &mut Observer, events: Vec<WinEvent>) {
        let mut i = 0;
        while i < events.len() {
            if let WinEvent::Found(_) = events[i] {
                let mut run: Vec<u32> = Vec::new();
                while let Some(WinEvent::Found(id)) = events.get(i) {
                    run.push(*id);
                    i += 1;
                }
                run.sort_unstable();
                run.dedup();
                for id in run {
                    self.on_found(ob, id);
                }
                continue;
            }
            self.on_event(ob, events[i]);
            i += 1;
        }
    }

    fn on_event(&mut self, ob: &mut Observer, event: WinEvent) {
        let now = Instant::now();
        match event {
            WinEvent::Started => self.on_started(ob),
            WinEvent::Stopped => tracing::warn!("observation stopped; windows stay where they are"),
            WinEvent::Found(id) => self.on_found(ob, id),
            WinEvent::Destroyed(id) => {
                if let Some(m) = self.model.as_mut() {
                    let actions = m.remove_window(WindowId(id));
                    self.execute(ob, actions);
                }
            }
            WinEvent::Moved(id) => {
                self.maybe_start_drag(ob, id, now);
                let frame = ob.window(id).map(|w| w.frame);
                let ours = self.echoes.is_window_echo(id, now);
                if let (Some(m), Some(frame)) = (self.model.as_mut(), frame)
                    && m.window(WindowId(id)).is_some()
                {
                    if ours {
                        m.window_frame_settled(WindowId(id), frame);
                    } else {
                        m.window_frame_changed(WindowId(id), frame);
                    }
                    self.mark_dirty();
                    self.strip_dirty = true;
                }
            }
            WinEvent::Retitled(id) => {
                let title = ob.window(id).map(|w| w.title.clone());
                if let (Some(m), Some(title)) = (self.model.as_mut(), title)
                    && m.window_title_changed(WindowId(id), &title)
                {
                    self.mark_dirty();
                }
            }
            WinEvent::Focused(id) => {
                if self.echoes.is_window_echo(id, now) {
                    tracing::debug!(id, "focus echo of our own action ignored");
                    return;
                }
                let obs = self.observe(ob);
                if let Some(pid) = ob.window(id).map(|w| w.pid)
                    && self.window_elsewhere(id)
                    && self.bring_here_if_shift(ob, pid, &obs, now)
                {
                    return;
                }
                if let Some(m) = self.model.as_mut() {
                    let actions = m.window_focused(WindowId(id), &obs);
                    if !actions.is_empty() {
                        tracing::info!(id, "focused window is in another stage; switching");
                    }
                    self.execute(ob, actions);
                }
            }
            WinEvent::Minimized(id) => self.on_minimized(ob, id),
            WinEvent::AppActivated(pid) => {
                if self.echoes.is_app_echo(pid, now) {
                    tracing::debug!(pid, "activation echo of our own action ignored");
                    return;
                }
                let obs = self.observe(ob);
                if self.bring_here_if_shift(ob, pid, &obs, now) {
                    return;
                }
                if let Some(m) = self.model.as_mut() {
                    let actions = m.app_activated(pid, &obs);
                    if !actions.is_empty() {
                        tracing::info!(pid, "activated app lives in another stage; switching");
                    }
                    self.execute(ob, actions);
                }
            }
        }
    }

    /// How long before an activation a Shift press still counts (`[sugar]`).
    fn activation_window(&self) -> Duration {
        Duration::from_millis(self.store.config().sugar.activation_window_ms)
    }

    /// ⌘M on a managed window: with `[windows] minimize_to_strip` (Apple's behavior) the
    /// window goes to the strip as a card of its own instead of the Dock: it is parked, then
    /// un-minimized there (macOS may flash its restore animation toward the corner).
    fn on_minimized(&mut self, ob: &mut Observer, id: u32) {
        if !self.store.config().windows.minimize_to_strip {
            return;
        }
        let obs = self.observe(ob);
        let Some(m) = self.model.as_mut() else { return };
        let actions = m.minimize_to_strip(WindowId(id), &obs);
        if actions.is_empty() {
            return;
        }
        tracing::info!(id, "window minimized: it goes to the strip instead of the Dock");
        self.execute(ob, actions);
        if let Some(w) = ob.window(id) {
            let now = Instant::now();
            self.echoes.touch_window(id, now);
            self.echoes.touch_app(w.pid, now);
            if let Err(e) = w.element.set_minimized(false) {
                tracing::warn!(id, "cannot restore the minimized window: {e}");
            }
        }
    }

    /// True when window `id` sits in a stage that is not active.
    fn window_elsewhere(&self, id: u32) -> bool {
        self.model.as_ref().is_some_and(|m| {
            m.stage_of(WindowId(id)).is_some_and(|s| m.main_active_stage() != Some(s))
        })
    }

    /// True when app `pid` has a standard window on the current stage (or pinned): its
    /// activation switches nothing, so Shift has nothing to change (a Shift-click inside one
    /// of its windows, to extend a selection, must not pull in its other windows).
    fn app_present(&self, pid: i32) -> bool {
        self.model.as_ref().is_some_and(|m| {
            let active = m.main_active_stage();
            m.windows().any(|w| {
                w.meta.pid == pid
                    && w.meta.role == WindowRole::Standard
                    && (w.home == Home::Pinned || active.is_some_and(|a| w.home == Home::Stage(a)))
            })
        })
    }

    /// The Shift sugar for an activation that would switch stages: when Shift was held (not
    /// with Command), bring app `pid`'s windows into the current stage instead. True when it
    /// did.
    fn bring_here_if_shift(&mut self, ob: &Observer, pid: i32, obs: &Observation, now: Instant) -> bool {
        if !self.store.config().sugar.shift_brings_here
            || self.app_present(pid)
            || !self.triggers.shift_here(now, self.activation_window(), input::live_modifiers())
        {
            return false;
        }
        let Some(m) = self.model.as_mut() else { return false };
        let actions = m.bring_app_here(pid, obs);
        if actions.is_empty() {
            return false;
        }
        self.triggers.consume();
        tracing::info!(pid, "Shift held: the app's windows come to the current stage");
        self.execute(ob, actions);
        true
    }

    /// A move event while the left button is down after a mouse-down in another app: the
    /// user is dragging a shown managed window (our own moves are echoes and do not count).
    fn maybe_start_drag(&mut self, ob: &Observer, id: u32, now: Instant) {
        if self.drag.is_some() || self.echoes.is_window_echo(id, now) {
            return;
        }
        let (Some(press), Some(m)) = (&self.press, &self.model) else { return };
        let Some(w) = m.window(WindowId(id)) else { return };
        let shown = !w.parked
            && (w.home == Home::Pinned || m.main_active_stage().is_some_and(|a| w.home == Home::Stage(a)));
        if !shown || w.meta.role != WindowRole::Standard || !input::left_button_down() {
            return;
        }
        let pre = press.frames.get(&id).copied().unwrap_or(w.restore);
        let Some(now_frame) = ob.window(id).map(|t| t.frame) else { return };
        let moving = (now_frame.w - pre.w).abs() < 1.0 && (now_frame.h - pre.h).abs() < 1.0;
        self.drag = Some(WindowDrag { id, pid: w.meta.pid, pre, moving });
        if moving {
            tracing::info!(id, "window drag started");
            self.present();
        }
    }

    /// Global input in another app (mouse down/up, key down). `over_strip` tells, for a
    /// mouse-up, whether the pointer is over the visible strip.
    pub fn input(&mut self, ob: &mut Observer, event: InputEvent, over_strip: bool) {
        let now = Instant::now();
        self.triggers.record(now, Modifiers { shift: event.shift, cmd: event.cmd });
        match event.kind {
            InputKind::MouseDown => {
                self.end_drag();
                self.press = self.model.as_ref().map(|m| Press {
                    frames: m.windows().filter(|w| !w.parked).map(|w| (w.meta.id.0, w.restore)).collect(),
                });
                self.mouse_down(ob, event.at);
            }
            InputKind::MouseUp => {
                self.press = None;
                let Some(drag) = self.drag.take() else { return };
                if drag.moving && over_strip {
                    tracing::info!(id = drag.id, "window dropped on the strip: a stage of its own");
                    let obs = self.observe(ob);
                    if let Some(m) = self.model.as_mut() {
                        let actions = m.drop_window_on_strip(WindowId(drag.id), drag.pre, &obs);
                        self.execute(ob, actions);
                    }
                } else if drag.moving {
                    tracing::debug!(id = drag.id, "window drag ended in the main area");
                }
                self.present();
            }
            InputKind::KeyDown => {}
        }
    }

    fn end_drag(&mut self) {
        if self.drag.take().is_some_and(|d| d.moving) {
            self.present();
        }
    }

    fn z_map() -> HashMap<u32, usize> {
        windows::z_order(&windows::list_windows())
    }

    /// Observation started: build the model from the live windows and `state.json` (launch,
    /// after the recovery scan), or add newly seen windows when it restarts.
    fn on_started(&mut self, ob: &mut Observer) {
        if self.model.is_some() {
            let new: Vec<u32> = ob
                .windows()
                .keys()
                .copied()
                .filter(|id| self.model.as_ref().is_some_and(|m| m.window(WindowId(*id)).is_none()))
                .collect();
            for id in new {
                self.on_found(ob, id);
            }
            return;
        }
        let displays = if self.displays.is_empty() { fallback_displays() } else { self.displays.clone() };
        let config = self.store.config().clone();
        let capacity = strip_capacity(displays[0].visible, &config);
        let mut options = config.model_options(self.settings, capacity);
        options.fit = fit_options(displays[0].visible, self.strip_visible, &self.settings, &config);
        let mut model = Model::new(displays, options);
        let z = Self::z_map();
        let rules = &config.rules;
        let live: Vec<WindowMeta> = ob
            .windows()
            .keys()
            .filter_map(|id| self.meta(ob, *id, &z))
            .filter(|m| m.role != WindowRole::Standard || rules.decide(&m.bundle_id, &m.title).action != RuleAction::Ignore)
            .collect();
        let state = Self::load_state();
        let empty = StateFile { version: STATE_VERSION, stages: vec![], active_index: None, strip_slots: vec![] };
        let assignment = rematch(state.as_ref().unwrap_or(&empty), &live);
        tracing::info!(
            live = live.len(),
            saved_stages = state.as_ref().map_or(0, |s| s.stages.len()),
            unmatched = assignment.unmatched.len(),
            "building stages"
        );
        let z_order: Vec<WindowId> = windows::list_windows()
            .iter()
            .filter(|w| w.onscreen && w.layer == 0)
            .map(|w| WindowId(w.id))
            .collect();
        let frontmost = AxElement::system_wide().focused_application().ok().and_then(|a| a.pid().ok());
        let first_start = state.is_none();
        let actions = model.apply_launch(&live, &assignment, false, &LaunchHints { z_order, frontmost });
        if first_start {
            tracing::info!(stages = model.stages().len(), "first start: one stage per app, as Apple's Stage Manager does");
        }
        self.model = Some(model);
        self.announce = config.style.reveal.launch_reveal;
        self.execute(ob, actions);
        for id in &assignment.unmatched {
            let Some(meta) = live.iter().find(|m| m.id == *id) else { continue };
            if rules.decide(&meta.bundle_id, &meta.title).action == RuleAction::Float {
                let obs = self.observe(ob);
                if let Some(m) = self.model.as_mut() {
                    let actions = m.toggle_pin(*id, &obs);
                    self.execute(ob, actions);
                }
            }
        }
        self.present();
        self.mark_dirty();
    }

    /// A window appeared after launch: the rules decide where it goes.
    fn on_found(&mut self, ob: &mut Observer, id: u32) {
        if self.model.as_ref().is_none_or(|m| m.window(WindowId(id)).is_some()) {
            return;
        }
        let z = Self::z_map();
        let Some(mut meta) = self.meta(ob, id, &z) else { return };
        if let Some(title) = ob.window(id).and_then(|w| w.element.title().ok()) {
            meta.title = title;
        }
        let mut decision = match meta.role {
            WindowRole::Standard => self.store.config().rules.decide(&meta.bundle_id, &meta.title),
            WindowRole::Child { .. } => Decision::default(),
        };
        if meta.role == WindowRole::Standard
            && self.store.config().sugar.shift_brings_here
            && matches!(decision.action, RuleAction::Native | RuleAction::New | RuleAction::Stage(_))
        {
            let new_app = self
                .model
                .as_ref()
                .is_some_and(|m| !m.windows().any(|w| w.meta.pid == meta.pid && w.meta.role == WindowRole::Standard));
            let sugar = &self.store.config().sugar;
            let within = Duration::from_millis(if new_app { sugar.launch_window_ms } else { sugar.activation_window_ms });
            if self.triggers.shift_here(Instant::now(), within, input::live_modifiers()) {
                if new_app {
                    self.triggers.consume();
                }
                tracing::info!(id, "Shift held: the new window joins the current stage");
                decision.action = RuleAction::Current;
            }
        }
        if decision.action == RuleAction::Ignore {
            tracing::debug!(id, "rule: ignore");
            return;
        }
        tracing::info!(id, action = %decision.action, placement = %decision.placement, "new window");
        let obs = self.observe(ob);
        if let Some(m) = self.model.as_mut() {
            let actions = m.add_window(meta, &decision, &obs);
            self.execute(ob, actions);
        }
    }

    // ----- commands ----------------------------------------------------------------------

    /// The main display changed (or was first reported).
    pub fn set_displays(&mut self, ob: &mut Observer, displays: Vec<Display>) {
        let Some(main) = displays.first().copied() else { return };
        safety::set_visible_frame(main.visible);
        self.displays = displays.clone();
        if let Some(m) = self.model.as_mut() {
            let actions = m.set_displays(displays);
            m.set_strip_capacity(strip_capacity(main.visible, self.store.config()));
            self.sync_fit();
            self.execute(ob, actions);
        }
    }

    /// Fresh `com.apple.WindowManager` values.
    pub fn set_settings(&mut self, raw: &RawSettings) {
        let s = native_settings(raw);
        if s == self.settings {
            return;
        }
        self.settings = s;
        if let Some(m) = self.model.as_mut() {
            m.set_settings(s);
            self.sync_fit();
            self.present();
        }
    }

    /// A left mouse-down in another app at `p`: on the wallpaper it reveals the desktop.
    pub fn mouse_down(&mut self, ob: &mut Observer, p: Point) {
        if self.model.is_none() || !wallpaper_reveal_enabled(&self.settings) {
            return;
        }
        if self.model.as_ref().is_some_and(|m| !m.main_display().frame.contains(p)) {
            return;
        }
        if !desktop::is_wallpaper_click(&windows::list_windows(), p) {
            return;
        }
        tracing::info!(x = p.x, y = p.y, "wallpaper click");
        self.reveal(ob);
    }

    fn reveal(&mut self, ob: &mut Observer) -> usize {
        let obs = self.observe(ob);
        let Some(m) = self.model.as_mut() else { return 0 };
        let actions = m.reveal_desktop(&obs);
        let n = actions.len();
        self.execute(ob, actions);
        n
    }

    /// A hotkey was pressed.
    pub fn hotkey(&mut self, ob: &mut Observer, command: HotkeyCommand) {
        tracing::info!(?command, "hotkey");
        let _ = match command {
            HotkeyCommand::SwitchTo(n) => self.switch(ob, n),
            HotkeyCommand::SendTo(n) => self.send(ob, None, Target::Number(n)),
            HotkeyCommand::NewStage => self.send(ob, None, Target::New),
            HotkeyCommand::TogglePin => self.pin(ob, None),
            HotkeyCommand::ToggleStrip => {
                self.toggle_strip();
                Ok(0)
            }
        };
    }

    /// Shows or hides the strip (menu and hotkey).
    pub fn toggle_strip(&mut self) {
        self.strip_visible = !self.strip_visible;
        self.sync_fit();
        tracing::info!(visible = self.strip_visible, "strip toggled");
        notify_ui(UiUpdate::StripVisible(self.strip_visible));
        self.present();
    }

    /// Input from the strip.
    pub fn strip_event(&mut self, ob: &mut Observer, event: StripEvent) {
        let obs = self.observe(ob);
        let Some(m) = self.model.as_mut() else { return };
        let actions = match event {
            StripEvent::CardClicked { stage, shift: true } => m.merge_into_active(StageId(stage), &obs),
            StripEvent::CardClicked { stage, shift: false } => m.switch_to(StageId(stage), &obs),
            StripEvent::CardDropped { stage, x, y } => {
                tracing::info!(stage, x, y, "card dropped in the main area: its windows join the current stage");
                m.merge_into_active_at(StageId(stage), Point::new(x, y), &obs)
            }
            StripEvent::IconClicked { stage, pid } => {
                tracing::info!(stage, pid, "card icon clicked: the stage, with every window of the app");
                m.show_app_everywhere(StageId(stage), pid, &obs)
            }
        };
        self.execute(ob, actions);
    }

    fn switch(&mut self, ob: &mut Observer, n: usize) -> Result<usize, String> {
        let obs = self.observe(ob);
        let m = self.model.as_mut().ok_or_else(not_managing)?;
        if n > m.stages().len() {
            return Err(format!("there is no stage {n}"));
        }
        let actions = m.switch_to_number(n, &obs);
        let count = actions.len();
        self.execute(ob, actions);
        Ok(count)
    }

    fn send(&mut self, ob: &mut Observer, id: Option<u32>, target: Target) -> Result<usize, String> {
        let obs = self.observe(ob);
        let id = id.map(WindowId).or(obs.focused).ok_or("no focused window")?;
        let m = self.model.as_mut().ok_or_else(not_managing)?;
        if m.window(id).is_none() {
            return Err(format!("window {} is not managed", id.0));
        }
        let actions = m.send_window(id, target, &obs);
        let count = actions.len();
        self.execute(ob, actions);
        Ok(count)
    }

    fn pin(&mut self, ob: &mut Observer, id: Option<u32>) -> Result<usize, String> {
        let obs = self.observe(ob);
        let id = id.map(WindowId).or(obs.focused).ok_or("no focused window")?;
        let m = self.model.as_mut().ok_or_else(not_managing)?;
        if m.window(id).is_none() {
            return Err(format!("window {} is not managed", id.0));
        }
        let actions = m.toggle_pin(id, &obs);
        let count = actions.len();
        self.execute(ob, actions);
        Ok(count)
    }

    fn merge(&mut self, ob: &mut Observer, n: usize) -> Result<usize, String> {
        let obs = self.observe(ob);
        let m = self.model.as_mut().ok_or_else(not_managing)?;
        let stage = m.stages().get(n.wrapping_sub(1)).map(|s| s.id).ok_or(format!("there is no stage {n}"))?;
        let actions = m.merge_into_active(stage, &obs);
        let count = actions.len();
        self.execute(ob, actions);
        Ok(count)
    }

    /// Brings every parked window back onscreen (quit, safety). Returns how many AX calls
    /// failed; the registry is empty afterwards when none did.
    pub fn unpark_all(&mut self, ob: &Observer) -> usize {
        let Some(m) = self.model.as_mut() else { return 0 };
        let actions = m.unpark_all();
        let n = actions.len();
        let failed = self.execute_batch(ob, actions, false);
        tracing::info!(windows = n, failed, "unparked every window");
        failed
    }

    /// Re-reads config.toml and applies it; a bad file keeps the last good config.
    pub fn reload_config(&mut self, ob: &mut Observer) {
        self.config_mtime = mtime(self.store.path());
        match self.store.reload() {
            Ok(warnings) => {
                for w in warnings {
                    tracing::warn!("config: {w}");
                }
                tracing::info!("config reloaded");
                self.config_error = None;
                let config = self.store.config().clone();
                if *self.style != config.style {
                    self.style = Arc::new(config.style.clone());
                }
                self.echoes.set_window(Duration::from_millis(config.windows.focus_echo_ms));
                self.sync_fit();
                if let Some(m) = self.model.as_mut() {
                    let capacity = strip_capacity(m.main_display().visible, &config);
                    m.set_strip_options(config.strip_options(capacity));
                    let actions = m.set_named_stages(config.stages.names.clone());
                    self.execute(ob, actions);
                }
                self.present();
            }
            Err(e) => {
                tracing::error!("config: {e}; keeping the last good config");
                self.config_error = Some(e.to_string());
            }
        }
        self.notify_config();
    }

    /// Housekeeping, about every 250 ms: debounced save, config polling, strip refresh.
    pub fn tick(&mut self, ob: &mut Observer) {
        let now = Instant::now();
        self.echoes.prune(now);
        if let Some((first, last)) = self.dirty
            && (now.duration_since(last) >= SAVE_DEBOUNCE || now.duration_since(first) >= SAVE_MAX_DELAY)
        {
            self.save_now();
        }
        if now.duration_since(self.config_checked) >= CONFIG_POLL {
            self.config_checked = now;
            let current = mtime(self.store.path());
            if current.is_some() && current != self.config_mtime {
                tracing::info!("config.toml changed; reloading");
                self.reload_config(ob);
            }
        }
        if self.strip_dirty {
            self.present();
        }
    }

    // ----- ctl ---------------------------------------------------------------------------

    /// Answers a ctl command that acts on the engine.
    pub fn ctl(&mut self, ob: &mut Observer, cmd: Command) -> Value {
        let done = |r: Result<usize, String>| match r {
            Ok(actions) => json!({ "ok": true, "actions": actions }),
            Err(e) => json!({ "ok": false, "error": e }),
        };
        match cmd {
            Command::State => self.state_json(ob),
            Command::Dump => ob.dump(),
            Command::Switch(n) => done(self.switch(ob, n)),
            Command::Send(n, id) => done(self.send(ob, id, Target::Number(n))),
            Command::New(id) => done(self.send(ob, id, Target::New)),
            Command::Pin(id) => done(self.pin(ob, id)),
            Command::Merge(n) => done(self.merge(ob, n)),
            Command::Reveal => done(if self.model.is_some() { Ok(self.reveal(ob)) } else { Err(not_managing()) }),
            Command::UnparkAll => {
                let failed = self.unpark_all(ob);
                json!({ "ok": failed == 0, "failed": failed })
            }
            Command::Reload => {
                self.reload_config(ob);
                json!({ "ok": self.config_error.is_none(), "error": self.config_error })
            }
            Command::DebugFrame { id, frame } => {
                if std::env::var_os("STAGE_MANAGER_TEST").is_none() {
                    return json!({ "ok": false, "error": "debug-frame needs STAGE_MANAGER_TEST=1" });
                }
                let shown = self.model.as_ref().and_then(|m| m.window(WindowId(id))).is_some_and(|w| !w.parked);
                match ob.window(id) {
                    Some(w) if shown => {
                        // Placed and brought to the front, so a test's pointer path is over it.
                        let now = Instant::now();
                        self.echoes.touch_window(id, now);
                        self.echoes.touch_app(w.pid, now);
                        let result = w
                            .element
                            .set_frame(frame)
                            .and_then(|_| w.element.raise())
                            .and_then(|_| w.element.set_main(true))
                            .and_then(|_| AxElement::application(w.pid).set_frontmost(true));
                        match result {
                            Ok(()) => json!({ "ok": true }),
                            Err(e) => json!({ "ok": false, "error": e.to_string() }),
                        }
                    }
                    _ => json!({ "ok": false, "error": format!("window {id} is not a shown managed window") }),
                }
            }
            Command::Ping | Command::Quit | Command::DebugPanic | Command::DebugMenuQuit | Command::DebugClick { .. } | Command::DebugDrag { .. } => {
                json!({ "ok": false, "error": "not a WM command" })
            }
        }
    }

    /// Stages, windows, frames and parked flags as JSON (`ctl state`).
    pub fn state_json(&self, ob: &Observer) -> Value {
        let Some(m) = &self.model else {
            return json!({ "ok": false, "error": not_managing() });
        };
        let active = m.main_active_stage();
        let number = |s: StageId| m.stage_number(s);
        let stages: Vec<Value> = m
            .stages()
            .iter()
            .map(|s| {
                json!({
                    "id": s.id.0,
                    "number": number(s.id),
                    "name": s.name,
                    "active": Some(s.id) == active,
                    "key": s.key.map(|k| k.0),
                    "windows": s.windows.iter().map(|w| w.0).collect::<Vec<_>>(),
                })
            })
            .collect();
        let windows: Vec<Value> = m
            .windows()
            .map(|w| {
                let live = ob.window(w.meta.id.0).and_then(|t| t.element.frame().ok());
                json!({
                    "id": w.meta.id.0,
                    "pid": w.meta.pid,
                    "bundle_id": w.meta.bundle_id,
                    "title": w.meta.title,
                    "role": match w.meta.role { WindowRole::Standard => "standard", WindowRole::Child { .. } => "child" },
                    "parent": match w.meta.role { WindowRole::Child { parent } => parent.map(|p| p.0), WindowRole::Standard => None },
                    "home": match w.home { Home::Pinned => json!("pinned"), Home::Stage(s) => json!(number(s)) },
                    "restore": w.restore,
                    "original": w.original,
                    "live": live,
                    "parked": w.parked,
                })
            })
            .collect();
        json!({
            "ok": true,
            "active": active.and_then(number),
            "strip": m.strip().iter().map(|c| c.number).collect::<Vec<_>>(),
            "strip_capacity": m.options().strip.capacity,
            "stage_inset": m.options().fit.inset,
            "strip_width": strip_width(m.main_display().visible.size(), &self.style),
            "fit_windows": m.options().fit.shrink,
            "fit": format!("{:?}", self.store.config().windows.fit).to_lowercase(),
            "strip_visible": self.strip_visible,
            "window_drag": self.drag.filter(|d| d.moving).map(|d| d.id),
            "display": { "frame": m.main_display().frame, "visible": m.main_display().visible },
            "config_path": self.store.path(),
            "config_error": self.config_error,
            "registry": safety::registry_len(),
            "stages": stages,
            "windows": windows,
        })
    }
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}

fn not_managing() -> String {
    "not managing windows yet (Accessibility missing or observation not started)".to_string()
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The focused window: the frontmost app's `AXFocusedWindow`, as a `CGWindowID`.
pub fn focused_window() -> Option<u32> {
    AxElement::system_wide().focused_application().ok()?.focused_window().ok()?.window_id().ok()
}

/// The main display from CoreGraphics when AppKit has not reported one yet.
fn fallback_displays() -> Vec<Display> {
    let main = desktop::cg_display_bounds().first().copied().unwrap_or(Rect::new(0.0, 0.0, 1440.0, 900.0));
    let menu = 30.0;
    vec![Display {
        id: DisplayId(objc2_core_graphics::CGMainDisplayID()),
        frame: main,
        visible: Rect::new(main.x, main.y + menu, main.w, main.h - menu),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echoes_expire() {
        let mut e = Echoes::default();
        let t0 = Instant::now();
        e.touch_window(7, t0);
        e.touch_app(42, t0);
        assert!(e.is_window_echo(7, t0 + Duration::from_millis(100)));
        assert!(e.is_app_echo(42, t0 + Duration::from_millis(100)));
        assert!(!e.is_window_echo(8, t0));
        assert!(!e.is_window_echo(7, t0 + ECHO_WINDOW));
        e.prune(t0 + ECHO_WINDOW);
        assert!(e.windows.is_empty() && e.apps.is_empty());
    }

    #[test]
    fn raw_settings_become_native_settings() {
        let raw: RawSettings = [("AutoHide".to_string(), 1), ("AppWindowGroupingBehavior".to_string(), 0)].into();
        let s = native_settings(&raw);
        assert!(s.hide_recent_apps);
        assert_eq!(s.app_windows, crate::engine::settings::AppWindows::OneAtATime);
    }

    #[test]
    fn wallpaper_reveal_honors_click_to_show_desktop() {
        assert!(wallpaper_reveal_enabled(&NativeSettings::default()));
        let always: RawSettings = [("EnableStandardClickToShowDesktop".to_string(), 1)].into();
        assert!(!wallpaper_reveal_enabled(&native_settings(&always)), "macOS reveals on its own");
        let only_sm: RawSettings = [("EnableStandardClickToShowDesktop".to_string(), 0)].into();
        assert!(wallpaper_reveal_enabled(&native_settings(&only_sm)));
    }
}
