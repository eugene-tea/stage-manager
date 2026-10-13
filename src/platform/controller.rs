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
//! - **Safety**: the parked-window registry is synced around every batch. Once Quit is
//!   requested ([`safety::quit_requested`]) a batch skips its remaining parks, raises and
//!   focus, so Quit is answered soon, but never a `SetFrame`: the model already counts those
//!   windows as shown, so no unpark would bring them back. A window the model counts as
//!   parked keeps its registry entry (element and frame) while the observer has lost it
//!   (Accessibility revoked, an element destroyed while ordered out): the registry and the
//!   guardian's parked file never forget a parked window, and its actions go through that
//!   element. A restore (`SetFrame`) that fails (a hung app, Accessibility revoked) keeps
//!   the window registered at the frame it was asked to take, and it is retried about once
//!   a second until the window server lists it on screen out of the park zone (or it is
//!   gone, or parked again); Quit retries it too.
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
//!   [`StripEvent::CardDropped`], one dropped onto another card as
//!   [`StripEvent::CardMerged`] (`[strip] merge_on_drop`).
//! - **Spaces**: every Space (Desktop) has its own stages and strip, as Apple's does. A
//!   window is given to the engine only once the window server lists it on screen (so its
//!   Space is the shown one); until then it waits in `unplaced` (another Space, minimized,
//!   app hidden). On a Space change ([`Controller::on_space_changed`]) every app's window
//!   list is read again and the engine recognizes the Space by the windows on screen
//!   ([`Model::arrive`]). About once a second the window server's list is checked: windows
//!   waiting that now show are new windows; windows of other Spaces on screen were moved
//!   here; windows of this Space gone from the screen (app not hidden, not minimized) left
//!   it. On a full-screen app's Space there is no strip and no stage changes; a Space change
//!   during the full-screen animation waits for it to settle ([`crate::engine::fullscreen`]).
//!   A Dock click, ⌘Tab or activation that lands on a window waiting in a stage of another
//!   Desktop: that stage is arranged at the Dock click itself (before the slide), else at the
//!   Space change notice, before the Desktop is recognized; the arrival is never animated
//!   ([`arrival`], [`super::dock`]).
//! - **Wallpaper clicks**: a click on the wallpaper reveals the desktop: ours with "Click
//!   wallpaper to reveal desktop" set to Only in Stage Manager, macOS's own with Always (the
//!   default, also when the key is unset), never both ([`wallpaper_click`]); a double click
//!   is one click. macOS hands the desktop to Finder, so Finder's activation and its main
//!   window's focus follow the click; that echo never switches stages or unparks a window
//!   ([`DesktopEcho`]), as with Apple's. A deliberate Finder activation (Dock click, ⌘Tab,
//!   its card) still brings its stage.
//! - **Shift sugar** (`[sugar] shift_brings_here`): Shift held at the user's triggering
//!   action ([`Triggers`]) brings windows here: a newly opened app's first window joins the
//!   current stage, and an activation (Dock click) of an app in another stage brings its
//!   windows here. The app switcher (Command held) never counts.
//! - **Transitions** (`[animation]`): a batch that parks shown windows or brings parked ones
//!   back is announced to the strip first ([`strip_model::Transition`]); once the strip has
//!   drawn snapshots over them, outgoing windows park behind theirs, and incoming windows wait
//!   (in the safety registry at their arrival frames) until the strip's growing snapshots
//!   cover their frames ([`StripEvent::Unveil`]), or a click or another event arrives, or
//!   their backstop passes (the expected unveil at the configured speed plus a margin,
//!   1.5 to 3 s: `motion_math::unveil_backstop`). The batch's focus does not wait: the keyboard goes to the new
//!   stage at once, as Apple's does (`split_covered`).
//! - **Space settle gate**: while a Space change waits out a full-screen animation, nothing
//!   changes stages: window events, strip input, hotkeys, wallpaper clicks and ctl stage
//!   commands are held off (`Controller::frozen`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
use super::{desktop, logging, paths, windows};
use crate::config::{Config, ConfigStore, FileAction, FitMode, HotkeyCommand, HotkeyConfig, StripStyle};
use crate::engine::desktop_echo::{Cause, DesktopEcho};
use crate::engine::displays::on_other_display;
use crate::engine::focus_replay::{FocusReplay, Wanted};
use crate::engine::park::choose_corner;
use crate::engine::fullscreen::{self, Seen, Settle, Verdict};
use crate::engine::overview::{self, FOREIGN_SETTLE, Overview, OverviewWatch, Persisting, Sighting};
use crate::engine::geometry::{Point, Rect};
use crate::engine::model::{
    Action, Display, DisplayId, Home, Model, Observation, SpaceId, StageId, Target, WindowId, WindowMeta, WindowRole,
};
use crate::engine::stage_area::{FitOptions, capacity_for, stage_inset, strip_width};
use crate::engine::persist::{LaunchHints, StateFile};
use crate::engine::rules::{Decision, RuleAction};
use crate::engine::settings::{NativeSettings, PrefValue};
use crate::logbook::counters::{self, Count};
use crate::ui::strip_model::{self, CardSnapshot, CardWindow, StripDrop, StripEvent, StripSnapshot, Transition};
use crate::ui::{motion_math, transition_plan};

mod arrival;

/// The windows read at a card's press serve its click this long (a press held longer reads
/// them again at the release).
const PRESSED_OBS_MAX: Duration = Duration::from_secs(1);

/// How long focus and activation events for windows and apps we just touched are ignored
/// by default (`[windows] focus_echo_ms`).
pub const ECHO_WINDOW: Duration = Duration::from_millis(700);

/// Quiet time before `state.json` is written after a change.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// A busy stream of changes is still saved at least this often.
const SAVE_MAX_DELAY: Duration = Duration::from_secs(3);

/// How often config.toml's modification time is checked.
const CONFIG_POLL: Duration = Duration::from_secs(1);

/// Longest wait for the strip to draw a transition's snapshots before windows move anyway.
const COVER_TIMEOUT: Duration = Duration::from_millis(150);

/// How often the window server's list is checked for windows changing Spaces.
const SPACE_POLL: Duration = Duration::from_secs(1);

/// No such check this soon after a Space change (the list may still be the old Space's).
const SPACE_SETTLE: Duration = Duration::from_secs(1);

/// A window counts as moved to or gone from the shown Space after this many checks in a row.
const SPACE_CHECKS: u32 = 2;

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
        /// `[strip] capture`: window pictures on or off.
        capture: bool,
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

/// How long an app we made frontmost may take to report its activation and still have it
/// count as ours ([`Echoes::is_app_echo`]): a busy app (Electron under load, an app answering
/// Accessibility at the 0.25 s timeout) can report it well after [`ECHO_WINDOW`].
pub const LATE_ACTIVATION_MAX: Duration = Duration::from_secs(5);

/// Recently touched windows and apps, whose focus and activation events are our echoes.
///
/// An activation is ours when it comes within the echo window of our `AXFrontmost`, or when
/// it is the first activation reported for an app we made frontmost and the user has not
/// switched apps since (a mouse-down or ⌘ key press the global monitor saw, or an activation
/// of another app that was not ours): a late echo after two quick switches must never pull
/// the user back to the first stage. Card clicks and hotkeys are not seen by the monitor,
/// so they keep a late echo ours.
#[derive(Debug)]
pub struct Echoes {
    windows: HashMap<u32, Instant>,
    apps: HashMap<i32, Instant>,
    /// Apps we made frontmost whose activation has not been reported yet.
    awaiting: HashMap<i32, Instant>,
    window: Duration,
}

impl Default for Echoes {
    fn default() -> Self {
        Self { windows: HashMap::new(), apps: HashMap::new(), awaiting: HashMap::new(), window: ECHO_WINDOW }
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
        self.awaiting.insert(pid, now);
    }

    /// True when a focus event for `id` is probably our own.
    pub fn is_window_echo(&self, id: u32, now: Instant) -> bool {
        self.windows.get(&id).is_some_and(|t| now.duration_since(*t) < self.window)
    }

    /// True when an activation of `pid` is probably our own: within the echo window, or
    /// the first activation reported since we made it frontmost with no user input between.
    /// Call it once per reported activation: it consumes the awaited one.
    pub fn is_app_echo(&mut self, pid: i32, now: Instant) -> bool {
        let late = self.awaiting.remove(&pid).is_some_and(|t| now.saturating_duration_since(t) < LATE_ACTIVATION_MAX);
        late || self.apps.get(&pid).is_some_and(|t| now.duration_since(*t) < self.window)
    }

    /// The user may have switched apps (a click, a ⌘ key press, another app activated on
    /// purpose): an activation reported from now on beyond the echo window is theirs, not a
    /// late echo.
    pub fn user_switched(&mut self) {
        self.awaiting.clear();
    }

    /// Forgets old entries.
    pub fn prune(&mut self, now: Instant) {
        self.windows.retain(|_, t| now.duration_since(*t) < self.window);
        self.apps.retain(|_, t| now.duration_since(*t) < self.window);
        self.awaiting.retain(|_, t| now.saturating_duration_since(*t) < LATE_ACTIVATION_MAX);
    }
}

/// Interprets raw `com.apple.WindowManager` values.
pub fn native_settings(raw: &RawSettings) -> NativeSettings {
    let values: HashMap<String, PrefValue> = raw.iter().map(|(k, v)| (k.clone(), PrefValue::Int(*v))).collect();
    NativeSettings::from_values(&values)
}

/// True when our wallpaper-click reveal should run: macOS's own "Click wallpaper to reveal
/// desktop" set to Always (`EnableStandardClickToShowDesktop` 1 or absent, Apple's default)
/// already moves every window aside outside Apple's Stage Manager, and both at once would
/// fight. Only "Only in Stage Manager" leaves the click to us.
pub fn wallpaper_reveal_enabled(s: &NativeSettings) -> bool {
    s.click_wallpaper_reveals_desktop && !s.macos_reveals_desktop_outside_stage_manager
}

/// What a click on the wallpaper does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperClick {
    /// Our reveal: the active stage goes to the strip, or the revealed stage comes back.
    Toggle,
    /// macOS reveals the desktop itself ("Always"): we move nothing.
    LeftToMacOs,
    /// The second click of a double click (or a later one): the first click already did
    /// what the click does; toggling again would bring the stage straight back.
    SameClick,
}

/// What a wallpaper click with macOS click count `clicks` does under settings `s`.
pub fn wallpaper_click(s: &NativeSettings, clicks: i64) -> WallpaperClick {
    if !wallpaper_reveal_enabled(s) {
        WallpaperClick::LeftToMacOs
    } else if clicks > 1 {
        WallpaperClick::SameClick
    } else {
        WallpaperClick::Toggle
    }
}

/// Splits a batch whose incoming windows wait behind their growing snapshots: what runs now
/// and what runs when the strip unveils them. Parks, the other frames and the batch's focus
/// run now: the keyboard goes to the new stage at once, as Apple's does (the app activates
/// first; its window, still in the park zone behind its snapshot, arrives a moment later,
/// as after a Dock click). The arriving frames, the raises and the focus again (after the
/// raises, so the key window ends in front) wait.
fn split_covered(actions: Vec<Action>, arriving: &HashSet<u32>) -> (Vec<Action>, Vec<Action>) {
    let focus: Vec<Action> = actions.iter().copied().filter(|a| matches!(a, Action::Focus { .. })).collect();
    let (later, mut now): (Vec<Action>, Vec<Action>) = actions.into_iter().partition(|a| match a {
        Action::SetFrame { id, .. } => arriving.contains(&id.0),
        Action::Raise { .. } | Action::Focus { .. } => true,
        Action::Park { .. } | Action::StateChanged => false,
    });
    now.extend(focus);
    (now, later)
}

/// True when a move of a shown window while the left button is down is the user dragging
/// it: the press was on the window (`frame`, its frame at the press, with a margin for the
/// resize edge) and the pointer has moved since. An app moving its own window while the
/// user holds the button elsewhere is no drag.
fn drag_plausible(press_at: Point, frame: Option<Rect>, pointer: Option<Point>) -> bool {
    const EDGE: f64 = 8.0;
    const MOVED: f64 = 2.0;
    let on_window = frame.is_some_and(|f| Rect::new(f.x - EDGE, f.y - EDGE, f.w + 2.0 * EDGE, f.h + 2.0 * EDGE).contains(press_at));
    let moved = pointer.is_none_or(|p| (p.x - press_at.x).hypot(p.y - press_at.y) >= MOVED);
    on_window && moved
}

/// The left button went down in another app: where, and the restore and live frames of the
/// windows shown then, so a window drag that follows knows where the window was before and
/// whether the press was on it.
#[derive(Debug, Default)]
struct Press {
    at: Point,
    frames: HashMap<u32, Rect>,
    live: HashMap<u32, Rect>,
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
    /// Where the button went down: the point of the window the pointer holds.
    grab: Point,
}

/// Incoming windows of a transition, held parked until their snapshots cover their frames.
#[derive(Debug)]
struct Pending {
    /// The transition.
    id: u64,
    /// Their `SetFrame`s, then the batch's raises and focus, in order.
    actions: Vec<Action>,
    since: Instant,
    /// They come back this long after `since` even when the strip never unveils them.
    max: Duration,
    /// Registry entries bringing them to their arrival frames if anything goes wrong.
    entries: BTreeMap<u32, ParkedEntry>,
    /// Their arrival frames: a click there during the wait was meant for them.
    frames: Vec<Rect>,
}

/// A window whose restore (`SetFrame`) failed: the model counts it shown, but it may still sit
/// in the park zone. It keeps its registry entry until the window server lists it on screen
/// out of the park zone (the review's S1), and the restore is retried meanwhile.
#[derive(Debug)]
struct Unrestored {
    /// The registry entry: element and the frame it was asked to take.
    entry: ParkedEntry,
    /// Retries so far (the first failure is logged at warn, later ones at debug).
    tries: u32,
}

/// What to do with an [`Unrestored`] window at a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnrestoredStep {
    /// Back on screen out of the park zone, gone, or parked again in the model's eyes (its
    /// own registry entry covers it): forget it.
    Done,
    /// On screen and still in the park zone: move it again.
    Retry,
    /// Not on screen (another Space, its app hidden): keep it registered and wait.
    Wait,
}

/// The step for an [`Unrestored`] window: `model_parked` is `None` when the model no longer
/// has the window, `listed` is `None` when the window server lists it nowhere, else whether
/// it is on screen and whether its frame lies in the park zone.
fn unrestored_step(model_parked: Option<bool>, listed: Option<(bool, bool)>) -> UnrestoredStep {
    match (model_parked, listed) {
        (None | Some(true), _) | (_, None) => UnrestoredStep::Done,
        (Some(false), Some((true, false))) => UnrestoredStep::Done,
        (Some(false), Some((true, true))) => UnrestoredStep::Retry,
        (Some(false), Some((false, _))) => UnrestoredStep::Wait,
    }
}

/// After a wallpaper click left to macOS's reveal, how long a check finding every window on
/// screen waits before the reveal counts as over (macOS slides them away over a moment).
const MACOS_REVEAL_SETTLE: Duration = Duration::from_secs(3);

/// How often unrestored windows are checked and retried.
const UNRESTORED_RETRY: Duration = Duration::from_secs(1);

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
    /// After the last batch: the windows the model had parked and every window's stage, so
    /// a transition knows which windows arrive and from which card.
    before: transition_plan::Before,
    /// Incoming windows waiting behind their growing snapshots.
    pending: Option<Pending>,
    next_transition: u64,
    /// The transition whose outgoing windows just parked, for the next snapshot.
    parked_note: Option<u64>,
    /// The transition whose incoming windows are in place, with their frames read back, for
    /// the next snapshot.
    arrived_note: Option<(u64, Vec<(u32, Rect)>)>,
    /// Tracked windows not given to the engine yet: not on screen when found (another
    /// Space, minimized, app hidden). Offered once they show.
    unplaced: BTreeSet<u32>,
    /// The shown Space is a full-screen app's: no strip, no stage changes.
    fullscreen: bool,
    /// A Space change waits for a full-screen animation to settle ([`Settle`]): nothing is
    /// counted and no stage changes until it has.
    space_pending: bool,
    /// Waits out full-screen animations.
    settle: Settle,
    /// When the last Space change was handled.
    space_changed_at: Option<Instant>,
    /// Space changes handled since launch (`ctl space`: a test's switch shows as a step).
    space_changes: u64,
    /// When the window server's list was last checked for Space changes.
    space_checked: Instant,
    /// Managed windows of other Spaces on screen: counted once the same ones show at two
    /// checks a settle apart ([`Persisting`]).
    foreign: Persisting,
    /// Mission Control, App Exposé and Show Desktop: no Space is inferred from the window
    /// list while one shows and for a settle after ([`OverviewWatch`]).
    overview: OverviewWatch,
    /// A notified Space change waits for an overview to close (said once in the log).
    arrival_held: bool,
    /// The windows as read when a card was pressed ([`StripEvent::CardPressed`]): the click
    /// that follows uses them instead of reading every frame again through AX after the
    /// release. Any event, batch, Space change, config reload, retried restore or window
    /// left for another display forgets them.
    pressed_obs: Option<(Instant, Observation)>,
    /// A focus or activation that came before its Desktop was recognized (its window on a
    /// Desktop not shown, not known yet, or a Space change settling): replayed on arrival
    /// ([`FocusReplay`], [`Model::bring_focus_forward`]).
    focus_replay: FocusReplay,
    /// The held window's Desktop, arranged before it was recognized ([`arrival`]).
    ahead: Option<arrival::AheadSent>,
    /// A left mouse-down on an application item of the Dock, until its mouse-up.
    dock_press: Option<super::dock::DockPress<arrival::Prediction>>,
    /// The Dock's pid, while it runs.
    dock_pid: Option<i32>,
    /// While set, a batch leaves the strip snapshot for later (`strip_dirty`): an arrival
    /// hands the strip one snapshot, in its final order.
    defer_present: bool,
    /// Managed windows of the shown Space missing from the screen: checks in a row.
    absent: HashMap<u32, u32>,
    /// Managed windows showing mostly on another display than the main one: checks in a
    /// row (left alone there, the guide).
    elsewhere: HashMap<u32, u32>,
    /// Each Space's active stage at its last strip snapshot (the day's log counts a switch
    /// when a Space's stage changed since: a click, or the stage its arrival brought).
    seen_active: HashMap<u64, Option<u64>>,
    /// The stages at the last strip snapshot (`None` before the first).
    seen_stages: Option<BTreeSet<u64>>,
    /// Finder's activation and focus that follow a wallpaper click.
    desktop_echo: DesktopEcho,
    /// `[strip] hide_while_desktop_shown`: a wallpaper click hid the strip, until the
    /// reveal ends or any window activation.
    strip_hidden_for_desktop: bool,
    /// A parked window focused while the left button is down (grabbed by its sliver): its
    /// stage comes once the button is up ([`Model::parked_window_dropped`]).
    focus_after_press: Option<u32>,
    /// Windows whose restore failed, kept registered until seen back ([`Unrestored`]).
    unrestored: BTreeMap<u32, Unrestored>,
    /// When the unrestored windows were last checked.
    unrestored_checked: Instant,
    /// When a wallpaper click was left to macOS's own reveal (Show Desktop: every window slides
    /// aside): until its windows are back, a check never counts them gone, so the reveal is
    /// never read as a Space change (review R2 V8). Ends with a click off the wallpaper or a
    /// Space change.
    macos_reveal_at: Option<Instant>,
    /// Shown windows (the active stage's, pinned ones) the window server did not show at the
    /// last strip snapshot (app hidden, minimized): left out of the strip's overlap test;
    /// a change seen by the once-a-second check sends a new snapshot.
    active_off_screen: BTreeSet<u32>,
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
            before: transition_plan::Before::default(),
            pending: None,
            next_transition: 0,
            parked_note: None,
            arrived_note: None,
            unplaced: BTreeSet::new(),
            fullscreen: false,
            space_pending: false,
            settle: Settle::default(),
            space_changed_at: None,
            space_changes: 0,
            space_checked: Instant::now(),
            foreign: Persisting::default(),
            overview: OverviewWatch::default(),
            arrival_held: false,
            pressed_obs: None,
            focus_replay: FocusReplay::default(),
            ahead: None,
            dock_press: None,
            dock_pid: None,
            defer_present: false,
            absent: HashMap::new(),
            elsewhere: HashMap::new(),
            seen_active: HashMap::new(),
            seen_stages: None,
            desktop_echo: DesktopEcho::default(),
            strip_hidden_for_desktop: false,
            focus_after_press: None,
            unrestored: BTreeMap::new(),
            unrestored_checked: Instant::now(),
            active_off_screen: BTreeSet::new(),
            macos_reveal_at: None,
        };
        logging::apply_config(&this.store.config().log);
        this.notify_config();
        this
    }

    fn notify_config(&self) {
        let config = self.store.config();
        notify_ui(UiUpdate::Config {
            error: self.config_error.clone(),
            hotkeys: config.hotkeys.clone(),
            thumbnail_interval_ms: config.strip.thumbnail_interval_ms,
            capture: config.strip.capture,
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

    /// Live frames of every shown managed window of the shown Space, the z-order and the
    /// focused window.
    fn observe(&self, ob: &mut Observer) -> Observation {
        let Some(m) = &self.model else { return Observation::default() };
        let space = m.current_space(m.main_display().id);
        let list = windows::list_windows();
        let z_order: Vec<WindowId> = list
            .iter()
            .filter(|w| w.onscreen && w.layer == 0)
            .map(|w| WindowId(w.id))
            .filter(|id| m.window(*id).is_some())
            .collect();
        let shown: Vec<u32> =
            m.windows().filter(|w| !w.parked && (w.space == space || w.sticky)).map(|w| w.meta.id.0).collect();
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
    /// put them back (a fitted window's exact pre-fit frame). A window the observer no longer
    /// tracks keeps the element of its current registry entry.
    fn parked_entries(&self, ob: &Observer) -> BTreeMap<u32, ParkedEntry> {
        let Some(m) = &self.model else { return BTreeMap::new() };
        let mut entries: BTreeMap<u32, ParkedEntry> = m
            .safety_targets()
            .into_iter()
            .filter_map(|(id, restore)| {
                let fitted = m.window(id).is_some_and(|w| !w.parked && w.original.is_some());
                match ob.window(id.0) {
                    Some(w) => {
                        let child = w.kind == WindowKind::Child;
                        Some((id.0, ParkedEntry { element: w.element.clone(), restore, child, fitted }))
                    }
                    None => {
                        let kept = safety::registry_entry(id.0)?;
                        tracing::debug!(id = id.0, "not tracked now; its registry entry is kept");
                        Some((id.0, ParkedEntry { restore, fitted, ..kept }))
                    }
                }
            })
            .collect();
        if let Some(p) = &self.pending {
            for (id, e) in &p.entries {
                entries.entry(*id).or_insert_with(|| e.clone());
            }
        }
        for (id, u) in &self.unrestored {
            entries.entry(*id).or_insert_with(|| u.entry.clone());
        }
        entries
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

    /// Executes actions in order; returns how many AX calls failed. Once Quit is requested
    /// the rest of the batch runs only its `SetFrame`s: unpark-all covers every window the
    /// model counts as parked, whether or not its park was carried out, but a window the
    /// model already counts as shown would otherwise stay parked (the review's S2).
    fn execute(&mut self, ob: &Observer, actions: Vec<Action>) -> usize {
        self.execute_batch(ob, actions, true)
    }

    fn execute_batch(&mut self, ob: &Observer, actions: Vec<Action>, interruptible: bool) -> usize {
        if actions.is_empty() {
            return 0;
        }
        self.pressed_obs = None;
        let parked = self.parked_entries(ob);
        safety::registry_add(parked.clone());
        let now = Instant::now();
        let mut failed = 0;
        let mut changed = false;
        let mut interrupted = false;
        let total = actions.len();
        for (done, action) in actions.into_iter().enumerate() {
            if interruptible && !interrupted && action != Action::StateChanged && safety::quit_requested() {
                tracing::info!(done, total, "quit requested: the rest of this batch runs only its window restores");
                changed = true;
                interrupted = true;
            }
            if interrupted && !runs_after_quit(&action) {
                continue;
            }
            let id = match action {
                Action::SetFrame { id, .. } | Action::Park { id, .. } | Action::Raise { id } | Action::Focus { id } => id.0,
                Action::StateChanged => {
                    changed = true;
                    continue;
                }
            };
            let Some(w) = ob.window(id) else {
                // Lost by the observer (Accessibility revoked, say): a move still goes through
                // the registry's element; a restore that cannot is a failure.
                let kept = matches!(action, Action::SetFrame { .. } | Action::Park { .. })
                    .then(|| safety::registry_entry(id))
                    .flatten();
                let result = match (action, &kept) {
                    (Action::SetFrame { frame, .. }, Some(e)) => Some(e.element.set_frame(frame)),
                    (Action::Park { frame, .. }, Some(e)) => Some(e.element.set_position(frame.origin())),
                    _ => None,
                };
                match result {
                    Some(Ok(())) => tracing::debug!(id, ?action, "done through the registry's element"),
                    Some(Err(e)) => {
                        failed += 1;
                        tracing::warn!(id, ?action, "action through the registry's element failed: {e}");
                        if let (Action::SetFrame { frame, .. }, Some(kept)) = (action, kept) {
                            self.restore_failed(id, ParkedEntry { restore: frame, fitted: false, ..kept });
                        }
                    }
                    None if matches!(action, Action::SetFrame { .. }) => {
                        failed += 1;
                        tracing::warn!(id, ?action, "cannot restore an untracked window");
                    }
                    None => tracing::debug!(id, ?action, "action for an untracked window skipped"),
                }
                continue;
            };
            self.echoes.touch_window(id, now);
            let result = match action {
                Action::SetFrame { frame, .. } => w.element.set_frame(frame),
                Action::Park { frame, .. } => w.element.set_position(frame.origin()),
                Action::Raise { .. } => w.element.raise(),
                Action::Focus { .. } => {
                    // Raise and make it main first, then bring only the app forward (never
                    // all its windows): with its main window on the shown Space, macOS has
                    // no reason to switch Spaces. The engine never focuses another Space's.
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
                if let Action::SetFrame { frame, .. } = action {
                    let child = w.kind == WindowKind::Child;
                    self.restore_failed(id, ParkedEntry { element: w.element.clone(), restore: frame, child, fitted: false });
                }
            } else {
                tracing::trace!(id, ?action, "done");
            }
        }
        if interrupted {
            // Never drop an entry on the way out: whatever was registered stays registered.
            safety::registry_add(self.parked_entries(ob));
        } else {
            safety::registry_replace(self.parked_entries(ob));
        }
        self.remember();
        if changed {
            self.mark_dirty();
            if self.defer_present {
                self.strip_dirty = true;
            } else {
                self.present();
            }
        }
        failed
    }

    /// True when the shown windows the window server leaves out (`shown` is its on-screen
    /// set) differ from those at the last strip snapshot.
    fn active_shown_changed(&self, shown: &HashSet<u32>) -> bool {
        let Some(m) = &self.model else { return false };
        let Some(active) = m.main_active_stage() else { return !self.active_off_screen.is_empty() };
        let space = m.current_space(m.main_display().id);
        let off: BTreeSet<u32> = m
            .windows()
            .filter(|w| {
                !w.parked
                    && ((w.home == Home::Pinned && (w.space == space || w.sticky)) || w.home == Home::Stage(active))
            })
            .map(|w| w.meta.id.0)
            .filter(|id| !shown.contains(id))
            .collect();
        off != self.active_off_screen
    }

    /// Keeps a window whose `SetFrame` failed registered (with the frame it was asked to
    /// take) until it is seen back out of the park zone; [`Self::retry_unrestored`] retries
    /// it. Never lets the batch's registry replace drop a window whose restore failed (S1).
    fn restore_failed(&mut self, id: u32, entry: ParkedEntry) {
        tracing::warn!(id, restore = %entry.restore, "restore failed: the window stays registered and is retried until it is seen back");
        self.unrestored.insert(id, Unrestored { entry, tries: 0 });
    }

    /// Checks windows whose restore failed against the window server, about once a second:
    /// seen on screen out of the park zone, gone, or parked again in the model → forgotten;
    /// still in the park zone → moved again; off screen → kept registered. With `force`
    /// (quit) every one is retried at once, whatever the list says.
    fn retry_unrestored(&mut self, ob: &Observer, force: bool) {
        if self.unrestored.is_empty() {
            return;
        }
        let now = Instant::now();
        if !force && now.duration_since(self.unrestored_checked) < UNRESTORED_RETRY {
            return;
        }
        self.unrestored_checked = now;
        let list = windows::list_windows();
        let before = self.unrestored.len();
        let ids: Vec<u32> = self.unrestored.keys().copied().collect();
        for id in ids {
            let model_parked = self.model.as_ref().and_then(|m| m.window(WindowId(id))).map(|w| w.parked);
            let parks = |f: Rect| self.model.as_ref().is_some_and(|m| m.main_display().parks(f));
            let listed = list.iter().find(|w| w.id == id).map(|w| (w.onscreen, parks(w.bounds)));
            let step = if force && model_parked == Some(false) && listed.is_some() {
                UnrestoredStep::Retry
            } else {
                unrestored_step(model_parked, listed)
            };
            match step {
                UnrestoredStep::Done => {
                    tracing::info!(id, ?model_parked, ?listed, "a window whose restore failed is accounted for");
                    self.unrestored.remove(&id);
                }
                UnrestoredStep::Wait => {}
                UnrestoredStep::Retry => {
                    let Some(u) = self.unrestored.get_mut(&id) else { continue };
                    u.tries += 1;
                    let element = ob.window(id).map(|w| w.element.clone()).unwrap_or_else(|| u.entry.element.clone());
                    match element.set_frame(u.entry.restore) {
                        Ok(()) => {
                            tracing::info!(id, tries = u.tries, restore = %u.entry.restore, "a failed restore went through on a retry");
                            // A window moved: a click pressed before reads the frames again.
                            self.pressed_obs = None;
                            if force {
                                self.unrestored.remove(&id);
                            }
                        }
                        Err(e) if u.tries == 1 || force => tracing::warn!(id, tries = u.tries, "a failed restore failed again: {e}"),
                        Err(e) => tracing::debug!(id, tries = u.tries, "a failed restore failed again: {e}"),
                    }
                }
            }
        }
        if self.unrestored.len() != before {
            safety::registry_replace(self.parked_entries(ob));
        }
    }

    /// Notes which windows are parked, where every window lives and the strip's slots, for
    /// the next transition.
    fn remember(&mut self) {
        let Some(m) = &self.model else { return };
        self.before = transition_plan::Before::of(m);
    }

    /// The windows a batch moves on or off the screen, if any, with the card slot each one
    /// leaves into or arrives from (the engine's, [`transition_plan::plan`]).
    fn transition_for(&mut self, ob: &Observer, actions: &[Action]) -> Option<Transition> {
        let m = self.model.as_ref()?;
        let t = transition_plan::plan(m, actions, &self.before, |id| ob.window(id).map(|t| t.frame), self.next_transition + 1)?;
        self.next_transition = t.id;
        for g in &t.outgoing {
            tracing::debug!(id = t.id, window = g.window.id, stage = ?g.stage, to_slot = ?g.slot, from = %g.window.frame, "transition: outgoing");
        }
        for g in &t.incoming {
            tracing::debug!(id = t.id, window = g.window.id, stage = ?g.stage, from_slot = ?g.slot, to = %g.window.frame, "transition: incoming");
        }
        tracing::debug!(
            id = t.id,
            before = ?transition_plan::order(&self.before.slots),
            after = ?transition_plan::order(&transition_plan::slots_of(m)),
            "transition: strip order"
        );
        Some(t)
    }

    /// Executes a batch the user caused, animated (`[animation]`): the strip first draws
    /// snapshots over the windows that leave and grows snapshots of those that arrive; the
    /// leaving windows park behind theirs, the arriving ones wait until the strip unveils
    /// them. Without the strip's cover the batch runs at once.
    fn execute_animated(&mut self, ob: &Observer, actions: Vec<Action>) -> usize {
        self.flush_pending(ob);
        let anim = self.store.config().style.animation.clone();
        if !anim.animates() || !self.strip_visible {
            return self.execute(ob, actions);
        }
        let Some(t) = self.transition_for(ob, &actions) else { return self.execute(ob, actions) };
        let id = t.id;
        let arriving: HashSet<u32> = t.incoming.iter().map(|w| w.window.id).collect();
        let frames: Vec<Rect> = t.incoming.iter().map(|w| w.window.frame).collect();
        let started = Instant::now();
        self.present_with(Some(Transition { sent: Some(started), ..t }));
        let cover = strip_model::wait_cover(id, COVER_TIMEOUT);
        tracing::debug!(id, ?cover, waited_ms = started.elapsed().as_millis() as u64, "transition covered");
        if cover.outgoing || cover.incoming {
            std::thread::sleep(Duration::from_millis(anim.cover_ms));
        }
        if cover.outgoing {
            self.parked_note = Some(id);
        }
        if !cover.incoming {
            let failed = self.execute(ob, actions);
            self.note_parked();
            return failed;
        }
        let (now, later) = split_covered(actions, &arriving);
        if let Some(Action::Focus { id: key }) = now.iter().rev().find(|a| matches!(a, Action::Focus { .. })) {
            tracing::debug!(id, key = key.0, "the keyboard goes to the new stage now; its windows arrive behind their snapshots");
        }
        let entries = later
            .iter()
            .filter_map(|a| match *a {
                Action::SetFrame { id, frame } => {
                    let w = ob.window(id.0)?;
                    let child = w.kind == WindowKind::Child;
                    Some((id.0, ParkedEntry { element: w.element.clone(), restore: frame, child, fitted: false }))
                }
                _ => None,
            })
            .collect();
        let max = motion_math::unveil_backstop(&self.store.config().style);
        self.pending = Some(Pending { id, actions: later, since: Instant::now(), max, entries, frames });
        let failed = self.execute(ob, now);
        self.note_parked();
        failed
    }

    /// Tells the strip a transition's outgoing windows have parked, if no snapshot did yet.
    fn note_parked(&mut self) {
        if self.parked_note.is_some() {
            self.present();
        }
    }

    /// Brings held incoming windows to their frames now (their snapshots cover them, or other
    /// input arrived). Never interrupted: these windows are not parked in the model's eyes.
    fn flush_pending(&mut self, ob: &Observer) {
        if let Some(p) = self.pending.take() {
            tracing::trace!(id = p.id, held_ms = p.since.elapsed().as_millis() as u64, "incoming windows unveiled");
            let asked: Vec<(u32, Rect)> = p
                .actions
                .iter()
                .filter_map(|a| match *a {
                    Action::SetFrame { id, frame } => Some((id.0, frame)),
                    _ => None,
                })
                .collect();
            self.execute_batch(ob, p.actions, false);
            // Where the windows really are now: their snapshots land exactly there.
            let mut arrived = Vec::new();
            for (id, frame) in asked {
                let Some(real) = ob.window(id).and_then(|w| w.element.frame().ok()) else { continue };
                let off = (real.x - frame.x).abs().max((real.y - frame.y).abs()).max((real.w - frame.w).abs()).max((real.h - frame.h).abs());
                tracing::debug!(id = p.id, window = id, asked = %frame, real = %real, off, "transition: incoming window in place");
                arrived.push((id, real));
            }
            if !arrived.is_empty() {
                self.arrived_note = Some((p.id, arrived));
                self.present();
            }
        }
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
        self.present_with(None);
    }

    /// Counts and logs what changed since the last strip snapshot: the active stage of the
    /// Space the strip shows (a switch, whatever caused it, since that Space's last
    /// snapshot) and stages made.
    fn note_changes(&mut self, space: SpaceId) {
        let Some(m) = &self.model else { return };
        let active = m.active_stage_in(space);
        if let Some(&before) = self.seen_active.get(&space.0)
            && before != active.map(|a| a.0)
            && let Some(to) = active
        {
            counters::bump(Count::Switch);
            let from = before.and_then(|b| m.stage_number(StageId(b)));
            tracing::info!(from, to = m.stage_number(to), stage = to.0, "stage switch");
        }
        let known: HashSet<u64> = m.spaces().iter().map(|s| s.id.0).collect();
        self.seen_active.retain(|s, _| known.contains(s));
        self.seen_active.insert(space.0, active.map(|a| a.0));
        let stages: BTreeSet<u64> = m.stages().iter().map(|s| s.id.0).collect();
        if let Some(seen) = &self.seen_stages {
            for id in stages.difference(seen) {
                counters::bump(Count::NewStage);
                tracing::info!(stage = id, number = m.stage_number(StageId(*id)), "new stage");
            }
        }
        self.seen_stages = Some(stages);
    }

    /// The Space the strip shows: the one shown, or while a Space change settles, the
    /// Desktop arranged ahead for it (its strip slides in arranged, [`arrival`]).
    fn strip_space(&self) -> Option<SpaceId> {
        let m = self.model.as_ref()?;
        let ahead = self.space_pending.then(|| self.ahead_space()).flatten();
        Some(ahead.unwrap_or_else(|| m.current_space(m.main_display().id)))
    }

    fn present_with(&mut self, transition: Option<Transition>) {
        self.strip_dirty = false;
        let Some(space) = self.strip_space() else { return };
        self.note_changes(space);
        let Some(m) = &self.model else { return };
        let current = space == m.current_space(m.main_display().id);
        let active = m.active_stage_in(space);
        let cards = cards_in(m, space);
        let dragged = self.drag.filter(|d| d.moving);
        // Only windows the window server shows: a hidden app's (⌘H) or a minimized window
        // never keeps the strip auto-hidden (R1-ui 2).
        let on = on_screen(&windows::list_windows());
        let (active_frames, off): (Vec<_>, Vec<_>) = m
            .windows()
            .filter(|w| {
                !w.parked
                    && ((w.home == Home::Pinned && (w.space == space || w.sticky))
                        || active.is_some_and(|a| w.home == Home::Stage(a)))
            })
            .filter(|w| dragged.is_none_or(|d| d.id != w.meta.id.0))
            .partition(|w| on.contains(&w.meta.id.0));
        let active_frames = active_frames.into_iter().map(|w| w.meta.frame).collect();
        self.active_off_screen = off.into_iter().map(|w| w.meta.id.0).collect();
        let active_windows = active
            .and_then(|a| m.stage(a))
            .map(|s| {
                s.windows
                    .iter()
                    .filter_map(|id| m.window(*id))
                    .filter(|w| !w.parked)
                    .map(|w| CardWindow { id: w.meta.id.0, pid: w.meta.pid, frame: w.meta.frame })
                    .collect()
            })
            .unwrap_or_default();
        strip_model::present(StripSnapshot {
            cards,
            active_stage: active.map(|s| s.0),
            hide_recent_apps: m.strip_auto_hides(),
            user_visible: self.strip_visible && !(current && self.strip_hidden_for_desktop && m.desktop_revealed()),
            style: self.style.clone(),
            active_frames,
            window_drag: dragged.map(|d| CardWindow { id: d.id, pid: d.pid, frame: d.pre }),
            window_drag_grab: dragged.map(|d| d.grab),
            announce: std::mem::take(&mut self.announce),
            active_windows,
            transition,
            parked: self.parked_note.take(),
            arrived: self.arrived_note.take(),
            space: space.0,
            fullscreen: current && self.fullscreen,
            away: false,
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
        // Something changed since a card was pressed: the click reads the windows afresh.
        self.pressed_obs = None;
        // Any other event brings windows held behind a transition's snapshots in at once;
        // moves, retitles and the echoes of our own focus (sent before the windows arrive,
        // so the keyboard goes to the new stage at once) leave the animation alone.
        if !matches!(
            event,
            WinEvent::Moved(_) | WinEvent::Retitled(_) | WinEvent::Focused(_) | WinEvent::AppActivated(_)
        ) {
            self.flush_pending(ob);
        }
        match event {
            WinEvent::Started => self.on_started(ob),
            WinEvent::Stopped => tracing::warn!("observation stopped; windows stay where they are"),
            WinEvent::Found(id) => self.on_found(ob, id),
            WinEvent::Destroyed(id) => {
                self.unplaced.remove(&id);
                self.absent.remove(&id);
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
                if self.unplaced.contains(&id) {
                    self.on_found(ob, id);
                }
                if self.echoes.is_window_echo(id, now) && !self.focus_beats_echo(ob, id, now) {
                    tracing::debug!(id, "focus echo of our own action ignored");
                    return;
                }
                let bundle = ob.window(id).and_then(|w| ob.bundle_id(w.pid));
                if self.desktop_echo.classify(bundle, now) == Cause::WallpaperEcho {
                    tracing::debug!(id, "Finder window focus after a wallpaper click: ignored (the desktop is Finder's)");
                    return;
                }
                self.flush_pending(ob);
                self.show_strip_after_desktop("a window was focused");
                let pid = ob.window(id).map(|w| w.pid);
                let unknown = self.model.as_ref().is_some_and(|m| m.window(WindowId(id)).is_none());
                let elsewhere = self.model.as_ref().is_some_and(|m| m.waits_on_another_space(WindowId(id)));
                if let Some(pid) = pid
                    && (self.frozen() || elsewhere || (unknown && self.unplaced.contains(&id)))
                {
                    // Its Desktop is not recognized yet (a Dock click or ⌘Tab to a window of
                    // another Desktop, often before the Space change notice): replayed once
                    // it is, never dropped.
                    let predicted = self.predicted_space(id);
                    tracing::debug!(id, pid, settling = self.space_pending, elsewhere, unknown, predicted, "focus held until its Desktop is recognized");
                    self.focus_replay.hold(Wanted::Window { id: WindowId(id), pid }, now);
                    self.after_hold(ob, now);
                    return;
                }
                if self.frozen() {
                    return;
                }
                self.forget_held(ob);
                if self.model.as_ref().and_then(|m| m.window(WindowId(id))).is_some_and(|w| w.parked)
                    && input::left_button_down()
                {
                    // Grabbed by its sliver: nothing moves under the user's drag; its stage
                    // comes once the button is up (Apple's parked windows cannot be grabbed).
                    tracing::debug!(id, "a parked window was grabbed; its stage comes when the button is up");
                    self.focus_after_press = Some(id);
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
                    self.execute_animated(ob, actions);
                }
            }
            WinEvent::Minimized(id) => self.on_minimized(ob, id),
            WinEvent::AppActivated(pid) => {
                let waiting: Vec<u32> =
                    self.unplaced.iter().copied().filter(|id| ob.window(*id).is_some_and(|w| w.pid == pid)).collect();
                for id in waiting {
                    self.on_found(ob, id);
                }
                if self.echoes.is_app_echo(pid, now) {
                    tracing::debug!(pid, "activation echo of our own action ignored");
                    return;
                }
                if self.desktop_echo.classify(ob.bundle_id(pid), now) == Cause::WallpaperEcho {
                    tracing::debug!(pid, "Finder activation after a wallpaper click: ignored (the desktop is Finder's)");
                    return;
                }
                // The user switched apps: a late echo of an earlier focus of ours is no
                // longer one.
                self.echoes.user_switched();
                self.flush_pending(ob);
                self.show_strip_after_desktop("an app was activated");
                if self.frozen() {
                    tracing::debug!(pid, "activation held until the Desktop is recognized");
                    self.focus_replay.hold(Wanted::App(pid), now);
                    self.after_hold(ob, now);
                    return;
                }
                let obs = self.observe(ob);
                if self.bring_here_if_shift(ob, pid, &obs, now) {
                    self.forget_held(ob);
                    return;
                }
                let present = self.app_present(pid);
                if let Some(m) = self.model.as_mut() {
                    let actions = m.app_activated(pid, &obs);
                    if actions.is_empty() && !present {
                        // Nothing to show here: macOS may be on its way to the app's window
                        // on another Desktop (its focus may have come first and be held).
                        self.focus_replay.hold(Wanted::App(pid), now);
                        self.after_hold(ob, now);
                    } else if actions.is_empty() && self.held_is_of(pid, now) {
                        // The app's own activation, its window's focus held (either order):
                        // that focus still says where the user goes (R5 2).
                        tracing::debug!(pid, "activation of the app whose window focus is held; the focus stays held");
                    } else {
                        if !actions.is_empty() {
                            tracing::info!(pid, "activated app lives in another stage; switching");
                        }
                        self.forget_held(ob);
                    }
                    self.execute_animated(ob, actions);
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
        if !self.store.config().windows.minimize_to_strip || self.frozen() {
            return;
        }
        let obs = self.observe(ob);
        let Some(m) = self.model.as_mut() else { return };
        let actions = m.minimize_to_strip(WindowId(id), &obs);
        if actions.is_empty() {
            return;
        }
        tracing::info!(id, "window minimized: it goes to the strip instead of the Dock");
        self.execute_animated(ob, actions);
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

    /// True when app `pid` has a standard window on the current stage (or pinned on this
    /// Space): its activation switches nothing, so Shift has nothing to change (a Shift-click
    /// inside one of its windows, to extend a selection, must not pull in its other windows).
    fn app_present(&self, pid: i32) -> bool {
        self.model.as_ref().is_some_and(|m| {
            let active = m.main_active_stage();
            let space = m.current_space(m.main_display().id);
            m.windows().any(|w| {
                w.meta.pid == pid
                    && w.meta.role == WindowRole::Standard
                    && ((w.home == Home::Pinned && (w.space == space || w.sticky))
                        || active.is_some_and(|a| w.home == Home::Stage(a)))
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
        self.execute_animated(ob, actions);
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
        if !drag_plausible(press.at, press.live.get(&id).copied(), input::pointer()) {
            tracing::trace!(id, "a move while the button is down elsewhere: the app moved its window, no drag");
            return;
        }
        let pre = press.frames.get(&id).copied().unwrap_or(w.restore);
        let Some(now_frame) = ob.window(id).map(|t| t.frame) else { return };
        let moving = (now_frame.w - pre.w).abs() < 1.0 && (now_frame.h - pre.h).abs() < 1.0;
        self.drag = Some(WindowDrag { id, pid: w.meta.pid, pre, moving, grab: press.at });
        if moving {
            tracing::info!(id, "window drag started");
            self.present();
        }
    }

    /// Global input in another app (mouse down/up, key down). `drop` tells, for a mouse-up
    /// over the visible strip, which slot a dragged window goes to.
    pub fn input(&mut self, ob: &mut Observer, event: InputEvent, drop: Option<StripDrop>) {
        let now = Instant::now();
        let mods = Modifiers { shift: event.shift, cmd: event.cmd };
        if event.kind == InputKind::MouseUp {
            self.triggers.record_release(now, mods);
        } else {
            self.triggers.record(now, mods);
        }
        match event.kind {
            InputKind::MouseDown => {
                tracing::debug!(x = event.at.x, y = event.at.y, "mouse down in another app");
                // One window list for the press: the Dock's bar and the wallpaper are looked
                // up in it. A press on a Dock item: its click may take the user to another
                // Desktop.
                let list = if self.model.is_some() && !self.frozen() { windows::list_windows() } else { Vec::new() };
                self.dock_pressed(ob, &list, event.at, now);
                // A click where an arriving window is about to be was meant for it: it
                // arrives now, and the click is no wallpaper click.
                let for_arriving = self.pending.as_ref().is_some_and(|p| p.frames.iter().any(|f| f.contains(event.at)));
                self.flush_pending(ob);
                self.end_drag();
                self.echoes.user_switched();
                self.press = self.model.as_ref().map(|m| Press {
                    at: event.at,
                    frames: m.windows().filter(|w| !w.parked).map(|w| (w.meta.id.0, w.restore)).collect(),
                    live: m.windows().filter(|w| !w.parked).map(|w| (w.meta.id.0, w.meta.frame)).collect(),
                });
                let wallpaper = if for_arriving {
                    tracing::debug!("click during a transition, on an arriving window");
                    false
                } else {
                    self.mouse_down(ob, &list, event.at, event.clicks)
                };
                if !wallpaper && self.desktop_echo.user_acted() {
                    tracing::debug!("a click off the wallpaper: Finder's next activation is deliberate");
                }
                if !wallpaper {
                    self.macos_reveal_at = None;
                }
            }
            InputKind::MouseUp => {
                let modifiers = event.cmd || event.shift || event.other_modifiers;
                let _ = self.dock_released(ob, event.at, modifiers, now);
                self.press = None;
                self.focus_after_press_done(ob);
                let Some(drag) = self.drag.take() else { return };
                if let Some(slot) = drop.filter(|_| drag.moving) {
                    counters::bump(Count::Drop);
                    tracing::info!(id = drag.id, before = ?slot.before, "window dropped on the strip: a stage of its own");
                    let obs = self.observe(ob);
                    if let Some(m) = self.model.as_mut() {
                        let actions = m.drop_window_on_strip(WindowId(drag.id), drag.pre, slot.before.map(StageId), &obs);
                        self.execute_animated(ob, actions);
                    }
                } else if drag.moving {
                    tracing::debug!(id = drag.id, "window drag ended in the main area");
                }
                self.present();
            }
            InputKind::KeyDown => {
                // Typing never cuts a switch's animation short: the keyboard is already the
                // new stage's (its focus ran before the windows arrived).
                if event.cmd {
                    self.echoes.user_switched();
                }
                if self.desktop_echo.user_acted() {
                    tracing::debug!("a key press after a wallpaper click: Finder's next activation is deliberate");
                }
            }
        }
    }

    /// The button is up after a parked window was grabbed by its sliver: its stage comes now,
    /// the window where the user left it when that is outside the park zone.
    fn focus_after_press_done(&mut self, ob: &mut Observer) {
        let Some(id) = self.focus_after_press.take() else { return };
        if self.space_pending {
            // A Space change is settling: the grab waits for it (the tick tries again), or the
            // window would sit on screen counted as parked until the next arrival parks it
            // again (review R2 L1).
            self.focus_after_press = Some(id);
            return;
        }
        if self.frozen() {
            return;
        }
        let Some(frame) = ob.refresh_frame(id) else { return };
        let obs = self.observe(ob);
        let Some(m) = self.model.as_mut() else { return };
        let actions = m.parked_window_dropped(WindowId(id), frame, &obs);
        if !actions.is_empty() {
            tracing::info!(id, "a parked window was let go; switching to its stage");
        }
        self.execute_animated(ob, actions);
    }

    fn end_drag(&mut self) {
        if self.drag.take().is_some_and(|d| d.moving) {
            self.present();
        }
    }

    /// Observation started: build the model from the windows on screen and `state.json`
    /// (launch, after the recovery scan), or add newly seen windows when it restarts.
    /// Windows not on screen (other Spaces, minimized, hidden apps) wait in `unplaced`.
    fn on_started(&mut self, ob: &mut Observer) {
        if self.model.is_some() {
            let new: Vec<u32> = ob
                .windows()
                .keys()
                .copied()
                .filter(|id| self.model.as_ref().is_some_and(|m| m.window(WindowId(*id)).is_none()))
                .collect();
            if self.frozen() {
                for id in new {
                    self.on_found(ob, id);
                }
                return;
            }
            // Accessibility came back (R1-platform 9): recognize the Space again from the
            // windows on screen, which also brings back a window the model counts as shown
            // but finds parked, and parks one it counts as parked but finds shown.
            tracing::info!(new = new.len(), "observation started again; recognizing the Space and checking every window");
            self.unplaced.extend(new);
            if let Some(main) = self.model.as_ref().map(|m| *m.main_display()) {
                let list = windows::list_windows();
                self.arrive_here(ob, &list, main);
            }
            return;
        }
        let displays = if self.displays.is_empty() { fallback_displays() } else { self.displays.clone() };
        let config = self.store.config().clone();
        let capacity = strip_capacity(displays[0].visible, &config);
        let mut options = config.model_options(self.settings, capacity);
        options.fit = fit_options(displays[0].visible, self.strip_visible, &self.settings, &config);
        let main = displays[0];
        let mut model = Model::new(displays, options);
        let list = windows::list_windows();
        // Windows on another display are left alone; they wait until they show on the main one.
        let shown = on_main_screen(&list, main.frame);
        let z = windows::z_order(&list);
        let candidates: Vec<WindowMeta> = ob
            .windows()
            .keys()
            .filter_map(|id| self.meta(ob, *id, &z))
            .filter(|m| self.allowed_by_rules(m))
            .collect();
        let (live, waiting): (Vec<WindowMeta>, Vec<WindowMeta>) =
            candidates.into_iter().partition(|m| shown.contains(&m.id.0));
        self.unplaced = waiting.iter().map(|m| m.id.0).collect();
        let state = Self::load_state();
        let first_start = state.is_none();
        tracing::info!(
            live = live.len(),
            off_screen = self.unplaced.len(),
            saved_spaces = state.as_ref().map_or(0, |s| s.spaces.len()),
            "building stages"
        );
        if let Some(state) = state {
            model.restore_spaces(state.spaces);
        }
        if main.frame.w > 0.0 && self.screen_seen(ob, &list, main.frame) == Seen::FullScreen {
            self.fullscreen = true;
            tracing::info!("launched on a full-screen app's Space: no strip until a Desktop shows");
        }
        let hints = launch_hints(&list);
        let actions = if self.fullscreen { Vec::new() } else { model.arrive(main.id, &live, &[], &hints) };
        if first_start {
            tracing::info!(stages = model.stages().len(), "first start: one stage per app, as Apple's Stage Manager does");
        }
        if self.fullscreen {
            self.unplaced.extend(live.iter().map(|m| m.id.0));
        }
        self.model = Some(model);
        self.announce = config.style.reveal.launch_reveal;
        self.execute(ob, actions);
        if !self.fullscreen {
            self.pin_floats(ob, &live);
        }
        self.present();
        self.mark_dirty();
    }

    /// True unless a rule says to ignore the window (children always count).
    fn allowed_by_rules(&self, m: &WindowMeta) -> bool {
        m.role != WindowRole::Standard || self.store.config().rules.decide(&m.bundle_id, &m.title).action != RuleAction::Ignore
    }

    /// Windows just placed by an arrival whose rule says `float`: pinned.
    fn pin_floats(&mut self, ob: &mut Observer, placed: &[WindowMeta]) {
        for meta in placed.iter().filter(|m| m.role == WindowRole::Standard) {
            let float = self.store.config().rules.decide(&meta.bundle_id, &meta.title).action == RuleAction::Float;
            let pinned = self.model.as_ref().and_then(|m| m.window(meta.id)).is_none_or(|w| w.home == Home::Pinned);
            if float && !pinned {
                let obs = self.observe(ob);
                if let Some(m) = self.model.as_mut() {
                    let actions = m.toggle_pin(meta.id, &obs);
                    self.execute(ob, actions);
                }
            }
        }
    }

    /// No stage changes now: a full-screen app's Space shows, or a Space change waits for a
    /// full-screen animation to settle.
    fn frozen(&self) -> bool {
        self.fullscreen || self.space_pending
    }

    /// Why stage commands are refused now, if they are ([`Self::frozen`]).
    fn frozen_reason(&self) -> Option<&'static str> {
        if self.space_pending {
            Some("a Space change is settling")
        } else if self.fullscreen {
            Some("a full-screen app's Space shows")
        } else {
            None
        }
    }

    /// What the window list shows on `display` ([`Seen`]): a full-screen app's Space when a
    /// big window on screen says it is in full screen (asked through Accessibility: the
    /// observer's element when we track it, else its app's window list); an animation when a
    /// window covers the whole display without that; else a Desktop.
    fn screen_seen(&self, ob: &Observer, list: &[windows::CgWindow], display: Rect) -> Seen {
        let big: Vec<&windows::CgWindow> = list
            .iter()
            .filter(|w| w.onscreen && w.layer == 0 && fullscreen::could_be_fullscreen(w.bounds, display))
            .collect();
        let covering = big.iter().filter(|w| fullscreen::covers_display(w.bounds, display)).count();
        let mut apps: HashMap<i32, Vec<AxElement>> = HashMap::new();
        let full = big.iter().any(|w| match ob.window(w.id) {
            Some(t) => t.element.is_fullscreen().unwrap_or(false),
            None => apps
                .entry(w.pid)
                .or_insert_with(|| AxElement::application(w.pid).windows().unwrap_or_default())
                .iter()
                .any(|e| e.window_id().ok() == Some(w.id) && e.is_fullscreen().unwrap_or(false)),
        });
        Seen::of(covering, full)
    }

    /// Looks for Mission Control, App Exposé or Show Desktop in the window list `list`
    /// (macOS's own windows covering the main display `display`, [`overview::overview_in`])
    /// and says in the log when one shows, closes, or is listed too long to be trusted.
    /// True while Space inference waits ([`OverviewWatch::freezes`]).
    fn look_for_overview(&mut self, list: &[windows::CgWindow], display: Rect, now: Instant) -> bool {
        let sightings = list
            .iter()
            .map(|w| Sighting { owner: &w.owner, layer: w.layer, bounds: w.bounds, onscreen: w.onscreen });
        match self.overview.see(overview::overview_in(sightings, display), now) {
            overview::Change::None => {}
            overview::Change::Began(kind) => {
                tracing::info!("{} shows: no Space is inferred from the window list until it closes", kind.name());
            }
            overview::Change::Ended(kind) => {
                tracing::info!("{} closed: Space inference resumes after a settle", kind.name());
            }
            overview::Change::Stuck(kind) => {
                tracing::warn!(
                    "{} has been listed for {} s without a break: taken for something else; Space inference resumes",
                    kind.name(),
                    overview::OVERVIEW_STUCK.as_secs()
                );
            }
        }
        let frozen = self.overview.freezes(now);
        if frozen {
            // What was counted before it showed is looked at afresh once it has closed.
            self.foreign.clear();
            self.absent.clear();
            self.elsewhere.clear();
        }
        frozen
    }

    /// True when the window list disagrees with the Space the engine thinks is shown (a
    /// Space change under way, its notification not handled yet): a known window of another
    /// Space is on screen, or none of this Space's windows is while one of them is missing
    /// for no visible reason.
    fn space_in_doubt(&self, ob: &Observer, shown: &HashSet<u32>) -> bool {
        let Some(m) = &self.model else { return false };
        let space = m.current_space(m.main_display().id);
        let standard = |w: &&crate::engine::model::WindowState| w.meta.role == WindowRole::Standard && !w.sticky;
        if m.windows().filter(standard).any(|w| w.space != space && shown.contains(&w.meta.id.0)) {
            return true;
        }
        let here: Vec<WindowId> = m.windows().filter(standard).filter(|w| w.space == space).map(|w| w.meta.id).collect();
        if here.is_empty() || here.iter().any(|id| shown.contains(&id.0)) {
            return false;
        }
        let absent = self.absent_windows(ob, shown);
        here.iter().any(|id| absent.contains(id))
    }

    /// Managed standard windows missing from the screen for no reason we can see: their app
    /// is not hidden, they are not minimized and not in full screen (asked through AX; a
    /// window in full screen is on a Space of its own and still belongs to its Desktop).
    /// Evidence that the Space they live on is not the one shown.
    fn absent_windows(&self, ob: &Observer, shown: &HashSet<u32>) -> Vec<WindowId> {
        let Some(m) = &self.model else { return Vec::new() };
        let mut hidden: HashMap<i32, bool> = HashMap::new();
        m.windows()
            .filter(|w| w.meta.role == WindowRole::Standard && !w.sticky && !shown.contains(&w.meta.id.0))
            .filter(|w| ob.window(w.meta.id.0).is_some())
            .filter(|w| !*hidden.entry(w.meta.pid).or_insert_with(|| ob.app_hidden_now(w.meta.pid)))
            .filter(|w| !ob.is_minimized_now(w.meta.id.0))
            .filter(|w| !ob.is_fullscreen_now(w.meta.id.0))
            .map(|w| w.meta.id)
            .collect()
    }

    /// The user went to another Space (`activeSpaceDidChangeNotification`): read every app's
    /// window list again, then (once any full-screen animation has settled,
    /// [`Self::settle_space`]) recognize the Space by the windows on screen and show its
    /// stages and strip. On a full-screen app's Space: no strip, nothing changes.
    pub fn on_space_changed(&mut self, ob: &mut Observer) {
        // Windows still held behind a switch's snapshots belong to the Space being left:
        // bring them in before anything is counted for the new one.
        self.flush_pending(ob);
        if self.model.is_none() {
            return;
        }
        let now = Instant::now();
        self.macos_reveal_at = None;
        self.space_changes += 1;
        counters::bump(Count::SpaceChange);
        tracing::debug!(changes = self.space_changes, "Space change notified");
        self.space_changed_at = Some(now);
        self.space_checked = now;
        self.pressed_obs = None;
        // A focus held for the Desktop being recognized counts from this change: a change
        // inferred from the window list comes seconds after the focus (R3 4).
        self.focus_replay.touch(now);
        self.foreign.clear();
        self.absent.clear();
        self.arrival_held = false;
        // The held window's Desktop slides in: its stage goes out now, before every app's
        // window list is read again (~60 ms) and the Desktop recognized.
        self.arrange_at_notice(ob, now);
        self.space_pending = true;
        if self.ahead.is_some() {
            // Its strip too, in its final order, while the Desktop slides in.
            self.present();
        }
        let found = ob.rescan_all();
        self.unplaced.extend(found);
        self.settle_space(ob);
    }

    /// A Space change waiting to be handled: a full-screen app's Space, a Desktop (arrive),
    /// or still a full-screen animation (looked at again on the next tick).
    fn settle_space(&mut self, ob: &mut Observer) {
        let Some(m) = self.model.as_ref() else { return };
        let main = *m.main_display();
        let list = windows::list_windows();
        let now = Instant::now();
        // Clicked in Mission Control (or switched there with the keyboard), the Desktop shows
        // once Mission Control has closed: until then the list holds its layout (App Exposé
        // even other Desktops' windows). The notification stays authoritative; the arrival
        // waits for a list that shows the Desktop.
        self.look_for_overview(&list, main.frame, now);
        if self.overview.holds_arrival(now) {
            if !self.arrival_held {
                self.arrival_held = true;
                tracing::info!(
                    "Space changed while {} shows; the Desktop is recognized once it has closed",
                    self.overview.showing().map_or("an overview", Overview::name)
                );
            }
            return;
        }
        self.arrival_held = false;
        let was_waiting = self.settle.waiting(now);
        match self.settle.step(self.screen_seen(ob, &list, main.frame), now) {
            Verdict::Wait => {
                if !was_waiting {
                    tracing::info!("Space changed during a full-screen animation; waiting for it to settle");
                }
            }
            Verdict::FullScreen => {
                self.space_pending = false;
                self.undo_ahead(ob, "a full-screen app's Space came");
                if !self.fullscreen {
                    counters::bump(Count::FullScreen);
                    tracing::info!("Space changed: a full-screen app's Space (no strip)");
                }
                self.fullscreen = true;
                self.present();
            }
            Verdict::Desktop => {
                self.space_pending = false;
                self.fullscreen = false;
                self.space_changed_at = Some(now);
                // A sliver grab let go while the change settled runs first, so the arrival
                // finds the window where the user dropped it, already unparked (R2 L1).
                if self.focus_after_press.is_some() && !input::left_button_down() {
                    self.focus_after_press_done(ob);
                }
                self.arrive_here(ob, &list, main);
            }
        }
    }

    /// A Desktop shows: the engine recognizes it by the windows on screen ([`Model::arrive`]),
    /// its stages and strip come back.
    fn arrive_here(&mut self, ob: &mut Observer, list: &[windows::CgWindow], main: Display) {
        let Some(m) = self.model.as_ref() else { return };
        let before = m.current_space(main.id);
        let shown = on_main_screen(list, main.frame);
        let z = windows::z_order(list);
        // Known windows' frames read fresh: the engine matches the screen to the model on
        // arrival (a parked window found shown is parked again, a shown one found parked is
        // brought back), so a frame missed by a notification must not mislead it.
        let known: Vec<u32> = ob
            .windows()
            .keys()
            .copied()
            .filter(|id| shown.contains(id) && m.window(WindowId(*id)).is_some())
            .collect();
        for id in known {
            ob.refresh_frame(id);
        }
        let Some(m) = self.model.as_ref() else { return };
        let live: Vec<WindowMeta> = ob
            .windows()
            .keys()
            .copied()
            .filter(|id| shown.contains(id))
            .filter(|id| self.unplaced.contains(id) || m.window(WindowId(*id)).is_some())
            .filter_map(|id| self.meta(ob, id, &z))
            .filter(|meta| self.allowed_by_rules(meta))
            .collect();
        let arrivals: Vec<WindowMeta> = live.iter().filter(|meta| m.window(meta.id).is_none()).cloned().collect();
        for meta in &live {
            self.unplaced.remove(&meta.id.0);
        }
        let hints = launch_hints(list);
        // Absent means off every screen: a window on another display is not missing.
        let absent = self.absent_windows(ob, &on_screen(list));
        let Some(m) = self.model.as_mut() else { return };
        let mut actions = m.arrive(main.id, &live, &absent, &hints);
        let after = m.current_space(main.id);
        tracing::info!(from = before.0, to = after.0, on_screen = live.len(), new = arrivals.len(), "Space changed");
        // A Desktop arranged ahead for the window it was shown for: confirmed or undone.
        let (more, predicted) = self.settle_ahead(ob, after, Instant::now());
        actions.extend(more);
        // One batch and one strip snapshot for the arrival: the strip of the Desktop shown
        // takes over in its final order, never first with the stage left active.
        self.defer_present = true;
        self.execute(ob, actions);
        self.pin_floats(ob, &arrivals);
        // Arriving through an app (Dock, ⌘Tab, an activation) whose window waits in a stage
        // here: show it, as Apple's does, at once (the Desktop's slide is the motion).
        self.bring_focus_forward(ob, "arrival", predicted);
        self.defer_present = false;
        self.present();
        self.mark_dirty();
    }

    /// The window the user went to this Desktop for (or a wait ended): AX is asked for the
    /// focused window now, and the focus or activation held meanwhile is replayed
    /// ([`Model::bring_focus_forward`]); its stage comes at once, the Desktop's slide having
    /// just shown it (Apple's shows the Desktop with that stage already active).
    /// `predicted`: the window a Desktop arranged ahead came for, counted as the focused one
    /// (Accessibility may still name the previous app's window, in the stage parked ahead).
    fn bring_focus_forward(&mut self, ob: &mut Observer, why: &str, predicted: Option<WindowId>) {
        let now = Instant::now();
        let held_at = self.focus_replay.came(now);
        let held = self.focus_replay.take(now);
        let mut obs = self.observe(ob);
        let ax_answer = obs.focused;
        if predicted.is_some() {
            obs.focused = predicted;
        }
        // The frontmost app's focused window asked now; when the system-wide answer fails
        // (an app busy with the Space change times out), the held app's own.
        if obs.focused.is_none()
            && let Some(pid) = held.map(Wanted::pid)
        {
            obs.focused = AxElement::application(pid).focused_window().ok().and_then(|w| w.window_id().ok()).map(WindowId);
        }
        let since_input_ms = self.since_input_ms(now);
        let Some(m) = self.model.as_mut() else { return };
        tracing::debug!(
            why,
            ax_focused = ?obs.focused.map(|f| f.0),
            ax_answer = ?ax_answer.map(|f| f.0),
            predicted = ?predicted.map(|f| f.0),
            ?held,
            "the window the Desktop was shown for"
        );
        let actions = m.bring_focus_forward(obs.focused, held, &obs);
        if !actions.is_empty() {
            tracing::info!(
                why,
                ax_focused = ?obs.focused.map(|f| f.0),
                ?held,
                instant = true,
                since_focus_ms = held_at.map(|t| now.saturating_duration_since(t).as_millis() as u64),
                since_input_ms,
                "the focused window waits in a stage of this Space; switching"
            );
        }
        self.execute(ob, actions);
    }

    /// About once a second: windows waiting that now show on screen are new windows; known
    /// windows of other Spaces on screen were moved here; windows of the shown Space gone from
    /// the screen (their app not hidden, not minimized) left it. A change between a Desktop
    /// and a full-screen Space the notification missed is handled as a Space change.
    fn check_spaces(&mut self, ob: &mut Observer) {
        let now = Instant::now();
        if now.duration_since(self.space_checked) < SPACE_POLL
            || self.space_changed_at.is_some_and(|t| now.duration_since(t) < SPACE_SETTLE)
        {
            return;
        }
        self.space_checked = now;
        let Some(m) = self.model.as_ref() else { return };
        let main = *m.main_display();
        let list = windows::list_windows();
        // Mission Control, App Exposé, Show Desktop: the list shows their layout (other
        // Desktops' windows, windows slid aside), not the Desktop. Nothing is inferred while
        // one shows and for a settle after; a Desktop clicked there comes with its
        // notification.
        if self.look_for_overview(&list, main.frame, now) {
            return;
        }
        if !self.fullscreen
            && self.model.as_ref().is_some_and(|m| m.held_focus_acts_now(self.focus_replay.peek(now)))
        {
            if input::left_button_down() {
                // A drag goes on (a sliver grab, a card): nothing switches under it; the
                // focus stays held and is looked at again on the next check (R3 8).
                tracing::debug!("a held focus waits for the mouse button to come up");
            } else {
                // A focus held while a settle froze things, with no Space change after it:
                // its window is on this Desktop after all. (One whose Desktop is not shown
                // stays held for its arrival: the check can fall between its focus and the
                // Space change.)
                self.bring_focus_forward(ob, "a wait ended", None);
            }
        }
        // A change between a Desktop and a full-screen Space the notification missed is
        // handled as a Space change; during a full-screen animation nothing is counted.
        match self.settle.step(self.screen_seen(ob, &list, main.frame), now) {
            Verdict::Wait => return,
            Verdict::FullScreen if !self.fullscreen => {
                self.on_space_changed(ob);
                return;
            }
            Verdict::Desktop if self.fullscreen => {
                self.on_space_changed(ob);
                return;
            }
            _ => {}
        }
        if self.fullscreen {
            return;
        }
        let shown = on_screen(&list);
        let shown_main = on_main_screen(&list, main.frame);
        if self.active_shown_changed(&shown) {
            tracing::debug!("a shown window was hidden or came back: the strip looks again");
            self.strip_dirty = true;
        }
        self.unplaced.retain(|id| ob.window(*id).is_some());
        let showing: Vec<u32> = self.unplaced.iter().copied().filter(|id| shown_main.contains(id)).collect();
        for id in showing {
            self.on_found(ob, id);
        }
        self.leave_other_displays(ob, &shown, &shown_main);

        let Some(m) = self.model.as_ref() else { return };
        let space = m.current_space(main.id);
        let standard = |w: &&crate::engine::model::WindowState| w.meta.role == WindowRole::Standard && !w.sticky;
        // The same windows of other Spaces on screen at two checks a settle apart (never one
        // listing): a Space change went unseen, the arrival guessed wrong, or a window was
        // moved here. Arrive again.
        let mut foreign: Vec<u32> = m
            .windows()
            .filter(standard)
            .filter(|w| w.space != space && shown.contains(&w.meta.id.0))
            .map(|w| w.meta.id.0)
            .collect();
        foreign.sort_unstable();
        if !foreign.is_empty() {
            // Maybe a Space change the notification missed: a focus held for it stays held
            // through the looks that confirm it (R3 4).
            self.focus_replay.touch(now);
        }
        if self.foreign.observe(foreign.clone(), now, FOREIGN_SETTLE) {
            tracing::info!(windows = ?foreign, "windows of another Space are on screen; recognizing the Space again");
            self.on_space_changed(ob);
            return;
        }

        let here = m.windows().filter(standard).filter(|w| w.space == space).count();
        let here_shown =
            m.windows().filter(standard).filter(|w| w.space == space && shown.contains(&w.meta.id.0)).count();
        let missing: Vec<(u32, i32)> = m
            .windows()
            .filter(standard)
            .filter(|w| w.space == space && !shown.contains(&w.meta.id.0))
            .filter(|w| !self.echoes.is_window_echo(w.meta.id.0, now))
            .map(|w| (w.meta.id.0, w.meta.pid))
            .collect();
        // In full screen, a window is on a Space of its own but still this Desktop's: it is
        // not gone (it comes back here when it leaves full screen).
        let missing: Vec<u32> = missing
            .into_iter()
            .filter(|&(id, pid)| {
                ob.window(id).is_some() && !ob.app_hidden_now(pid) && !ob.is_minimized_now(id) && !ob.is_fullscreen_now(id)
            })
            .map(|(id, _)| id)
            .collect();
        if let Some(at) = self.macos_reveal_at {
            if !missing.is_empty() {
                // macOS's own reveal slid the windows aside: they are not gone.
                tracing::debug!(missing = missing.len(), "macOS reveals the desktop: its windows off screen are not counted");
                self.absent.clear();
                return;
            }
            if now.duration_since(at) >= MACOS_REVEAL_SETTLE {
                self.macos_reveal_at = None;
            }
        }
        self.absent.retain(|id, _| missing.contains(id));
        let mut gone = Vec::new();
        for &id in &missing {
            let n = self.absent.entry(id).or_insert(0);
            *n += 1;
            if *n >= SPACE_CHECKS {
                gone.push(id);
            }
        }
        if gone.is_empty() {
            return;
        }
        // Most (or all) of this Space's windows gone at once: another Space shows (the
        // notification was missed) or the screen shows nothing of ours for a moment (display
        // asleep, the login window); recognize it rather than forgetting them.
        if missed_space_change(here, here_shown, gone.len()) {
            if pane_on_screen(&strip_model::panel_status(), space.0, &list) {
                // This Desktop's own strip pane is still on screen, so this Desktop still
                // shows: the windows were moved to another Desktop by hand (Mission Control),
                // not left behind by a Space change (review R2 V7: a one-window Desktop kept
                // its id).
                tracing::info!(missing = gone.len(), of = here, "windows left this Desktop while its strip still shows: they moved to another Desktop");
            } else {
                tracing::info!(missing = gone.len(), of = here, "most windows of this Space are gone from the screen; recognizing the Space again");
                self.on_space_changed(ob);
                return;
            }
        }
        // Forget only a window the window server still lists (on another Space): one not
        // listed at all is closing (its destroy notification follows) or the list is blind.
        let listed: HashSet<u32> = list.iter().map(|w| w.id).collect();
        for id in gone.into_iter().filter(|id| listed.contains(id)) {
            self.absent.remove(&id);
            let Some(m) = self.model.as_mut() else { return };
            tracing::info!(id, "window left this Space (not on screen, not minimized, app not hidden)");
            let (actions, forgotten) = m.window_left_space(WindowId(id));
            self.execute(ob, actions);
            self.unplaced.extend(forgotten.into_iter().map(|w| w.0).filter(|w| ob.window(*w).is_some()));
        }
    }

    /// Managed windows (shown, standard) that show mostly on another display in two checks in
    /// a row are let go: other displays are left alone (docs/GUIDE.md). They wait, unmanaged, and
    /// come back as new windows once they show on the main display.
    fn leave_other_displays(&mut self, ob: &Observer, shown: &HashSet<u32>, shown_main: &HashSet<u32>) {
        let Some(m) = self.model.as_ref() else { return };
        let away: Vec<u32> = m
            .windows()
            .filter(|w| w.meta.role == WindowRole::Standard && !w.parked)
            .map(|w| w.meta.id.0)
            .filter(|id| shown.contains(id) && !shown_main.contains(id))
            .collect();
        self.elsewhere.retain(|id, _| away.contains(id));
        let mut leave = Vec::new();
        for id in away {
            let n = self.elsewhere.entry(id).or_insert(0);
            *n += 1;
            if *n >= SPACE_CHECKS {
                leave.push(id);
            }
        }
        if !leave.is_empty() {
            // What a card press read no longer holds (the model forgets these windows).
            self.pressed_obs = None;
        }
        for id in leave {
            self.elsewhere.remove(&id);
            let Some(m) = self.model.as_mut() else { return };
            tracing::info!(id, "window moved to another display; left alone there until it comes back");
            let (actions, forgotten) = m.window_left_space(WindowId(id));
            self.execute(ob, actions);
            self.unplaced.extend(forgotten.into_iter().map(|w| w.0).filter(|w| ob.window(*w).is_some()));
        }
    }

    /// A window appeared after launch: the rules decide where it goes. One that is not on
    /// screen (another Space, minimized, app hidden; or any window while a full-screen app's
    /// Space shows) waits in `unplaced` until it shows.
    fn on_found(&mut self, ob: &mut Observer, id: u32) {
        if self.model.as_ref().is_none_or(|m| m.window(WindowId(id)).is_some()) {
            return;
        }
        self.flush_pending(ob);
        let list = windows::list_windows();
        let main = self.model.as_ref().map_or(Rect::default(), |m| m.main_display().frame);
        let shown = on_main_screen(&list, main);
        if self.frozen() || !shown.contains(&id) {
            if self.unplaced.insert(id) {
                tracing::debug!(id, "window not on screen; it is placed once it shows");
            }
            return;
        }
        if self.look_for_overview(&list, main, Instant::now()) {
            // App Exposé lists other Desktops' windows on screen: placed once it has closed
            // (the once-a-second check offers it again).
            if self.unplaced.insert(id) {
                tracing::debug!(id, "Mission Control, App Exposé or Show Desktop shows; the window is placed once it has closed");
            }
            return;
        }
        let display = self.model.as_ref().map(|m| m.main_display().frame);
        if let Some(display) = display
            && self.settle.step(self.screen_seen(ob, &list, display), Instant::now()) != Verdict::Desktop
        {
            // Most likely the full-screen animation's stand-in window (it goes away once the
            // animation is over), or a window on a full-screen Space not handled yet.
            if self.unplaced.insert(id) {
                tracing::debug!(id, "a full-screen animation or Space shows; the window is placed once a Desktop shows");
            }
            return;
        }
        if self.space_in_doubt(ob, &on_screen(&list)) {
            // Its notification came before the Space change's: placing it now would put it
            // (and maybe a stage switch, a focus) on the Space being left.
            if self.unplaced.insert(id) {
                tracing::debug!(id, "the window list disagrees with the Space shown; the window is placed on arrival");
            }
            return;
        }
        self.unplaced.remove(&id);
        let z = windows::z_order(&list);
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
        counters::bump(Count::NewWindow);
        tracing::info!(id, action = %decision.action, placement = %decision.placement, "new window");
        let obs = self.observe(ob);
        if let Some(m) = self.model.as_mut() {
            let actions = m.add_window(meta, &decision, &obs);
            self.execute_animated(ob, actions);
        }
    }

    // ----- commands ----------------------------------------------------------------------

    /// The main display changed (or was first reported).
    pub fn set_displays(&mut self, ob: &mut Observer, displays: Vec<Display>) {
        self.flush_pending(ob);
        let Some(main) = displays.first().copied() else { return };
        safety::set_main_display(main.visible, main.corner);
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

    /// A left mouse-down in another app at `p`, the `clicks`th click of a multiple click:
    /// on the wallpaper it reveals the desktop ([`wallpaper_click`]: ours only with "Only in
    /// Stage Manager"; a double click is one click). True when it was a wallpaper click,
    /// whoever reveals: Finder's activation and focus that follow are its echo
    /// ([`DesktopEcho`]) either way.
    pub fn mouse_down(&mut self, ob: &mut Observer, list: &[windows::CgWindow], p: Point, clicks: i64) -> bool {
        if self.model.is_none() || self.frozen() {
            return false;
        }
        if self.model.as_ref().is_some_and(|m| !m.main_display().frame.contains(p)) {
            return false;
        }
        // Our own windows (the strip below windows, the motion overlay above them) never
        // receive other apps' clicks, so they never hide the wallpaper.
        let own = std::process::id() as i32;
        let list: Vec<_> = list.iter().filter(|w| w.pid != own).cloned().collect();
        if !desktop::is_wallpaper_click(&list, p) {
            return false;
        }
        self.desktop_echo.wallpaper_clicked(Instant::now());
        match wallpaper_click(&self.settings, clicks) {
            WallpaperClick::Toggle => {
                tracing::info!(x = p.x, y = p.y, "wallpaper click");
                self.reveal(ob, true);
            }
            WallpaperClick::LeftToMacOs => {
                self.macos_reveal_at = Some(Instant::now());
                tracing::info!(
                    x = p.x,
                    y = p.y,
                    "wallpaper click: macOS reveals the desktop itself (Click wallpaper to reveal desktop: Always)"
                )
            }
            WallpaperClick::SameClick => {
                tracing::info!(x = p.x, y = p.y, clicks, "wallpaper double click: one click, nothing toggles again")
            }
        }
        true
    }

    /// Reveals the desktop, or brings the revealed stage back (a second click). With
    /// `[strip] hide_while_desktop_shown` the strip hides while the desktop shows, and the
    /// windows move without the strip's animation (there is no card to grow from or shrink
    /// into).
    fn reveal(&mut self, ob: &mut Observer, animated: bool) -> usize {
        let obs = self.observe(ob);
        let hide = self.store.config().strip.hide_while_desktop_shown;
        let hidden_before = self.strip_hidden_for_desktop && self.model.as_ref().is_some_and(Model::desktop_revealed);
        let Some(m) = self.model.as_mut() else { return 0 };
        let actions = m.reveal_desktop(&obs);
        let revealed = m.desktop_revealed();
        let n = actions.len();
        self.strip_hidden_for_desktop = hide && revealed;
        if self.strip_hidden_for_desktop {
            tracing::debug!("the desktop shows: the strip hides ([strip] hide_while_desktop_shown)");
        }
        if animated && !hidden_before && !self.strip_hidden_for_desktop {
            self.execute_animated(ob, actions);
        } else {
            self.execute(ob, actions);
        }
        if hidden_before != self.strip_hidden_for_desktop {
            self.present();
        }
        n
    }

    /// `[strip] hide_while_desktop_shown`: any window activation brings the hidden strip
    /// back (the reveal's own Finder echo does not count).
    fn show_strip_after_desktop(&mut self, why: &str) {
        if std::mem::take(&mut self.strip_hidden_for_desktop) {
            tracing::debug!(why, "the strip comes back after the desktop reveal");
            self.present();
        }
    }

    /// A hotkey was pressed.
    pub fn hotkey(&mut self, ob: &mut Observer, command: HotkeyCommand) {
        tracing::info!(?command, "hotkey");
        self.flush_pending(ob);
        if self.frozen() && command != HotkeyCommand::ToggleStrip {
            // Refused: nothing acted, so a focus held (and a Desktop arranged for it) stays.
            tracing::debug!(settling = self.space_pending, "a full-screen app's Space shows or a Space change is settling; the hotkey does nothing");
            return;
        }
        self.forget_held(ob);
        let _ = match command {
            HotkeyCommand::SwitchTo(n) => self.switch(ob, n, true),
            HotkeyCommand::SendTo(n) => self.send(ob, None, Target::Number(n), true),
            HotkeyCommand::NewStage => self.send(ob, None, Target::New, true),
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
        if let StripEvent::CardPressed { stage } = event {
            // Read the windows now, while the button is down (~100 ms before the release),
            // so the click's motion starts without waiting on AX. Not while a transition's
            // windows are still on their way (their frames are not the ones to save).
            self.pressed_obs = (self.pending.is_none() && !self.frozen() && self.model.is_some()).then(|| (Instant::now(), self.observe(ob)));
            tracing::trace!(stage, read = self.pressed_obs.is_some(), "card pressed");
            return;
        }
        if let StripEvent::Unveil { id } = event {
            if self.pending.as_ref().is_some_and(|p| p.id == id) {
                self.flush_pending(ob);
            }
            return;
        }
        self.flush_pending(ob);
        // The strip shows a reorder or a merge before the engine has decided: when the engine
        // refuses it (or does nothing), a snapshot puts the strip back (review R2 V4).
        let local_edit = matches!(event, StripEvent::CardMoved { .. } | StripEvent::CardMerged { .. });
        if self.frozen() {
            // Refused: nothing acted, so a focus held (and a Desktop arranged for it) stays.
            tracing::debug!(settling = self.space_pending, "a full-screen app's Space shows or a Space change is settling; the strip's input does nothing");
            if local_edit {
                self.present();
            }
            return;
        }
        self.forget_held(ob);
        if let StripEvent::CardMoved { stage, before } = event {
            let Some(m) = self.model.as_mut() else { return };
            counters::bump(Count::Reorder);
            tracing::info!(stage, ?before, "card moved to another slot");
            let actions = m.move_card(StageId(stage), before.map(StageId));
            let changed = actions.contains(&Action::StateChanged);
            self.execute(ob, actions);
            if !changed {
                self.present();
            }
            return;
        }
        let pressed = self.pressed_obs.take().filter(|(at, _)| at.elapsed() < PRESSED_OBS_MAX).map(|(_, obs)| obs);
        let obs = match pressed {
            Some(obs) if matches!(event, StripEvent::CardClicked { .. } | StripEvent::IconClicked { .. }) => obs,
            _ => self.observe(ob),
        };
        let Some(m) = self.model.as_mut() else { return };
        let actions = match event {
            StripEvent::CardClicked { stage, shift } => {
                counters::bump(Count::CardClick);
                tracing::info!(stage, number = m.stage_number(StageId(stage)), shift, "card clicked");
                if shift { m.merge_into_active(StageId(stage), &obs) } else { m.switch_to(StageId(stage), &obs) }
            }
            StripEvent::CardDropped { stage, x, y } => {
                counters::bump(Count::Drop);
                tracing::info!(stage, x, y, "card dropped in the main area: its windows join the current stage");
                m.merge_into_active_at(StageId(stage), Point::new(x, y), &obs)
            }
            StripEvent::IconClicked { stage, pid } => {
                counters::bump(Count::CardClick);
                tracing::info!(stage, pid, "card icon clicked: the stage, with every window of the app");
                m.show_app_everywhere(StageId(stage), pid, &obs)
            }
            StripEvent::CardMerged { stage, into } => {
                counters::bump(Count::Drop);
                tracing::info!(
                    stage,
                    into,
                    number = m.stage_number(StageId(stage)),
                    into_number = m.stage_number(StageId(into)),
                    "card dropped on a card: the stages merge"
                );
                m.merge_stages(StageId(stage), StageId(into), &obs)
            }
            StripEvent::CardMoved { .. } | StripEvent::Unveil { .. } | StripEvent::CardPressed { .. } => Vec::new(),
        };
        let changed = actions.contains(&Action::StateChanged);
        self.execute_animated(ob, actions);
        if local_edit && !changed {
            tracing::debug!("the engine did not take the strip's edit; the strip shows the engine's cards again");
            self.present();
        }
    }

    /// Runs a batch animated (user input) or at once (scripted ctl commands).
    fn run_batch(&mut self, ob: &Observer, actions: Vec<Action>, animated: bool) -> usize {
        if animated { self.execute_animated(ob, actions) } else { self.execute(ob, actions) }
    }

    fn switch(&mut self, ob: &mut Observer, n: usize, animated: bool) -> Result<usize, String> {
        let obs = self.observe(ob);
        let m = self.model.as_mut().ok_or_else(not_managing)?;
        let space = m.current_space(m.main_display().id);
        if m.stage_by_number(space, n).is_none() {
            return Err(format!("there is no stage {n}"));
        }
        let actions = m.switch_to_number(n, &obs);
        let count = actions.len();
        self.run_batch(ob, actions, animated);
        Ok(count)
    }

    fn send(&mut self, ob: &mut Observer, id: Option<u32>, target: Target, animated: bool) -> Result<usize, String> {
        let obs = self.observe(ob);
        let id = id.map(WindowId).or(obs.focused).ok_or("no focused window")?;
        let m = self.model.as_mut().ok_or_else(not_managing)?;
        if m.window(id).is_none() {
            return Err(format!("window {} is not managed", id.0));
        }
        let actions = m.send_window(id, target, &obs);
        let count = actions.len();
        self.run_batch(ob, actions, animated);
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
        let space = m.current_space(m.main_display().id);
        let stage = m.stage_by_number(space, n).ok_or(format!("there is no stage {n}"))?;
        let actions = m.merge_into_active(stage, &obs);
        let count = actions.len();
        self.execute(ob, actions);
        Ok(count)
    }

    /// Brings every parked window back onscreen (quit, safety). Returns how many AX calls
    /// failed; the registry is empty afterwards when none did.
    pub fn unpark_all(&mut self, ob: &Observer) -> usize {
        self.flush_pending(ob);
        // Windows whose restore failed earlier: the model counts them shown, so its unpark
        // has nothing for them (S1).
        self.retry_unrestored(ob, true);
        let Some(m) = self.model.as_mut() else { return self.unrestored.len() };
        let actions = m.unpark_all();
        let n = actions.len();
        let failed = self.execute_batch(ob, actions, false);
        safety::registry_replace(self.parked_entries(ob));
        tracing::info!(windows = n, failed, unrestored = self.unrestored.len(), "unparked every window");
        failed + self.unrestored.len()
    }

    /// Re-reads config.toml and applies it; a bad file keeps the last good config.
    pub fn reload_config(&mut self, ob: &mut Observer) {
        self.flush_pending(ob);
        self.config_mtime = mtime(self.store.path());
        match self.store.reload() {
            Ok(warnings) => {
                for w in warnings {
                    tracing::warn!("config: {w}");
                }
                tracing::info!("config reloaded");
                self.config_error = None;
                // The fit and the strip may change: a pressed card's click reads afresh.
                self.pressed_obs = None;
                let config = self.store.config().clone();
                logging::apply_config(&config.log);
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
        if let Some((id, max)) = self.pending.as_ref().filter(|p| now.duration_since(p.since) >= p.max).map(|p| (p.id, p.max)) {
            tracing::warn!(id, max_ms = max.as_millis() as u64, "the strip never unveiled a transition's windows; they come back now");
            self.flush_pending(ob);
        }
        self.echoes.prune(now);
        if !safety::quit_requested() {
            self.retry_unrestored(ob, false);
        }
        if self.focus_after_press.is_some() && !input::left_button_down() {
            // The button came up without a mouse-up the monitor saw.
            self.focus_after_press_done(ob);
        }
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
        if self.model.is_some() && !safety::quit_requested() {
            self.expire_ahead(ob, now);
            if self.space_pending {
                self.settle_space(ob);
            } else {
                self.check_spaces(ob);
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
        self.flush_pending(ob);
        let acts = matches!(
            cmd,
            Command::Switch(_) | Command::Send(..) | Command::New(_) | Command::Pin(_) | Command::Merge(_) | Command::Reveal
        );
        if acts && let Some(why) = self.frozen_reason() {
            return json!({ "ok": false, "error": why });
        }
        // Without Accessibility no window moves: a stage change now would leave the model out
        // of step with the screen, its parked windows unregistered (review R2 L2), as the
        // wm thread's gate does for the user's input.
        if acts && self.managing() && !ob.observing() {
            return json!({ "ok": false, "error": "Accessibility is off" });
        }
        match cmd {
            Command::State => self.state_json(ob),
            Command::Space => match &self.model {
                Some(m) => json!({
                    "ok": true,
                    "space": m.current_space(m.main_display().id).0,
                    "fullscreen": self.fullscreen,
                    "settling": self.space_pending,
                    "changes": self.space_changes,
                    "overview": self.overview.showing().map(Overview::name),
                    "inference_frozen": self.overview.freezes(Instant::now()),
                }),
                None => json!({ "ok": false, "error": not_managing() }),
            },
            Command::DebugFullscreen { id, on } => {
                if std::env::var_os("STAGE_MANAGER_TEST").is_none() {
                    return json!({ "ok": false, "error": "debug-fullscreen needs STAGE_MANAGER_TEST=1" });
                }
                match test_fullscreen(ob, id, on) {
                    Ok(reply) => reply,
                    Err(e) => json!({ "ok": false, "error": e }),
                }
            }
            Command::DebugClose { id } => {
                if std::env::var_os("STAGE_MANAGER_TEST").is_none() {
                    return json!({ "ok": false, "error": "debug-close needs STAGE_MANAGER_TEST=1" });
                }
                match test_window(ob, id, "debug-close").and_then(|e| e.press_close_button().map_err(|e| e.to_string())) {
                    Ok(()) => {
                        tracing::info!(id, "debug-close");
                        json!({ "ok": true })
                    }
                    Err(e) => json!({ "ok": false, "error": e }),
                }
            }
            Command::Dump => {
                let mut dump = ob.dump();
                dump["spaces"] = self.spaces_json();
                dump
            }
            Command::Switch(n) => done(self.switch(ob, n, false)),
            Command::Send(n, id) => done(self.send(ob, id, Target::Number(n), false)),
            Command::New(id) => done(self.send(ob, id, Target::New, false)),
            Command::Pin(id) => done(self.pin(ob, id)),
            Command::Merge(n) => done(self.merge(ob, n)),
            Command::Reveal => done(if self.model.is_some() { Ok(self.reveal(ob, false)) } else { Err(not_managing()) }),
            Command::UnparkAll => {
                let failed = self.unpark_all(ob);
                json!({ "ok": failed == 0, "failed": failed })
            }
            Command::Reload => {
                self.reload_config(ob);
                json!({ "ok": self.config_error.is_none(), "error": self.config_error })
            }
            Command::DebugDockClick { bundle } => {
                if std::env::var_os("STAGE_MANAGER_TEST").is_none() {
                    return json!({ "ok": false, "error": "debug-dock-click needs STAGE_MANAGER_TEST=1" });
                }
                self.debug_dock_click(ob, &bundle)
            }
            Command::DebugFrame { id, frame, only } => {
                if std::env::var_os("STAGE_MANAGER_TEST").is_none() {
                    return json!({ "ok": false, "error": "debug-frame needs STAGE_MANAGER_TEST=1" });
                }
                let shown = self.model.as_ref().and_then(|m| m.window(WindowId(id))).is_some_and(|w| !w.parked);
                if only {
                    // The frame and nothing else: no raise or focus, which would take macOS
                    // to the window's Desktop. The probe of moves on a Desktop not shown.
                    let Some(w) = ob.window(id).filter(|_| shown) else {
                        return json!({ "ok": false, "error": format!("window {id} is not a managed window shown in its stage") });
                    };
                    self.echoes.touch_window(id, Instant::now());
                    let space = self.model.as_ref().and_then(|m| m.space_of(WindowId(id))).map(|s| s.0);
                    let on_shown = self.model.as_ref().is_some_and(|m| !m.waits_on_another_space(WindowId(id)));
                    return match w.element.set_frame(frame) {
                        Ok(()) => {
                            let read = w.element.frame().ok();
                            tracing::info!(id, %frame, ?read, space, on_shown, "debug-frame only");
                            json!({ "ok": true, "space": space, "on_shown_space": on_shown, "ax_frame": read })
                        }
                        Err(e) => json!({ "ok": false, "error": e.to_string(), "space": space, "on_shown_space": on_shown }),
                    };
                }
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
            Command::Ping | Command::Quit | Command::Settings | Command::Mark(_) | Command::DebugPanic | Command::DebugMenuQuit | Command::DebugClick { .. }
            | Command::DebugHover { .. }
            | Command::DebugDrag { .. }
            | Command::DebugStrip
            | Command::DebugOverlay { .. }
            | Command::DebugSpace { .. }
            | Command::DebugExpose { .. } => {
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
                    "space": s.space.0,
                    "number": number(s.id),
                    "name": s.name,
                    "active": m.active_stage_in(s.space) == Some(s.id),
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
                    "space": w.space.0,
                    "sticky": w.sticky,
                })
            })
            .collect();
        json!({
            "ok": true,
            "space": m.current_space(m.main_display().id).0,
            "fullscreen": self.fullscreen,
            "unplaced": self.unplaced.iter().collect::<Vec<_>>(),
            "active": active.and_then(number),
            "strip": m.strip().iter().map(|c| c.number).collect::<Vec<_>>(),
            "strip_capacity": m.options().strip.capacity,
            "stage_inset": m.options().fit.inset,
            "strip_width": strip_width(m.main_display().visible.size(), &self.style),
            "fit_windows": m.options().fit.shrink,
            "fit": format!("{:?}", self.store.config().windows.fit).to_lowercase(),
            "strip_visible": self.strip_visible,
            "desktop_revealed": m.desktop_revealed(),
            "strip_hidden_for_desktop": self.strip_hidden_for_desktop && m.desktop_revealed(),
            "window_drag": self.drag.filter(|d| d.moving).map(|d| d.id),
            "animation": self.store.config().style.animation.animates(),
            "animation_speed": self.store.config().style.animation.speed(),
            "capture": super::capture::status(),
            "strip_cards": strip_model::published_cards()
                .into_iter()
                .map(|(stage, r)| json!({ "number": m.stage_number(StageId(stage)), "frame": r }))
                .collect::<Vec<_>>(),
            "strip_level": format!("{:?}", self.style.level),
            "last_transition": strip_model::last_transition(),
            "strip_panel": strip_model::panel_status(),
            "strip_panels": strip_model::pane_statuses(),
            "display": { "frame": m.main_display().frame, "visible": m.main_display().visible },
            "config_path": self.store.path(),
            "config_error": self.config_error,
            "registry": safety::registry_len(),
            "stages": stages,
            "windows": windows,
        })
    }
}

impl Controller {
    /// The Spaces the engine knows, for `ctl dump`: synthetic id, display, whether shown,
    /// the active stage and every stage id (no titles).
    pub fn spaces_json(&self) -> Value {
        let Some(m) = &self.model else { return json!(null) };
        let list: Vec<Value> = m
            .spaces()
            .iter()
            .map(|s| {
                json!({
                    "id": s.id.0,
                    "display": s.display.0,
                    "current": m.current_space(s.display) == s.id,
                    "active_stage": m.active_stage_in(s.id).map(|a| a.0),
                    "stages": m.stages_in(s.id).map(|st| st.id.0).collect::<Vec<_>>(),
                    "windows": m.windows().filter(|w| w.space == s.id).count(),
                })
            })
            .collect();
        json!({
            "fullscreen": self.fullscreen,
            "settling": self.space_pending,
            "unplaced_windows": self.unplaced.len(),
            "saved_spaces_not_visited": m.unvisited_saved_spaces(),
            "list": list,
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

/// Ids of the layer-0 windows the window server lists on screen: the shown Space's (parked
/// ones included), never another Space's.
/// True for an action a batch still runs once Quit is requested: a `SetFrame` (the model
/// already counts its window as shown, so no unpark would bring it back; S2). Parks, raises
/// and focus are skipped: unpark-all covers every window the model counts as parked.
fn runs_after_quit(action: &Action) -> bool {
    matches!(action, Action::SetFrame { .. })
}

/// True when this Space's windows missing from the screen mean another Space shows (a Space
/// change the notification missed) rather than single windows leaving: none of its windows
/// is on screen any more (a one-window Desktop too, the review's S5), or at least two and at
/// least half of them are gone. `here` counts the Space's standard windows, `here_shown`
/// those on screen, `gone` those missing for no reason in consecutive checks.
fn missed_space_change(here: usize, here_shown: usize, gone: usize) -> bool {
    gone > 0 && (here_shown == 0 || (gone >= 2 && gone * 2 >= here))
}

/// True when the strip's pane (`pane`, as the strip last published it) belongs to `space`
/// and the window server lists it on screen in `list`: a pane lives on one Desktop, so that
/// Desktop shows.
fn pane_on_screen(pane: &strip_model::PanelStatus, space: u64, list: &[windows::CgWindow]) -> bool {
    pane.window != 0 && pane.space == space && list.iter().any(|w| w.id == pane.window && w.onscreen)
}

fn on_screen(list: &[windows::CgWindow]) -> HashSet<u32> {
    list.iter().filter(|w| w.onscreen && w.layer == 0).map(|w| w.id).collect()
}

/// The strip's cards of `space`, in slot order.
fn cards_in(m: &Model, space: SpaceId) -> Vec<CardSnapshot> {
    m.strip_in(space)
        .into_iter()
        .map(|c| CardSnapshot {
            stage: c.stage.0,
            number: c.number,
            name: c.name,
            windows: c
                .windows
                .iter()
                .filter_map(|id| m.window(*id))
                .map(|w| CardWindow { id: w.meta.id.0, pid: w.meta.pid, frame: w.restore })
                .collect(),
            apps: c.apps,
        })
        .collect()
}

/// The windows on screen on the main display: [`on_screen`] minus those that show mostly on
/// another display ([`on_other_display`]), which are left alone (docs/GUIDE.md: stages live on the
/// main display only). `main` is the main display's frame.
fn on_main_screen(list: &[windows::CgWindow], main: Rect) -> HashSet<u32> {
    let others: Vec<Rect> = desktop::cg_display_bounds().into_iter().skip(1).collect();
    list.iter()
        .filter(|w| w.onscreen && w.layer == 0)
        .filter(|w| others.is_empty() || !on_other_display(w.bounds, main, &others))
        .map(|w| w.id)
        .collect()
}

/// Recency hints for windows placed on arrival: the window list's z-order and the frontmost
/// app.
fn launch_hints(list: &[windows::CgWindow]) -> LaunchHints {
    LaunchHints {
        z_order: list.iter().filter(|w| w.onscreen && w.layer == 0).map(|w| WindowId(w.id)).collect(),
        frontmost: AxElement::system_wide().focused_application().ok().and_then(|a| a.pid().ok()),
    }
}

/// Test only (`ctl debug-fullscreen`): puts window `id` in or out of full screen through
/// Accessibility. Only a window of an app on the `STAGE_MANAGER_ONLY` list (a fixture) is
/// touched; without that list, or for any other window, it refuses. Sets `AXFullScreen`; a
/// window that refuses the attribute gets its full-screen button pressed instead. A window
/// already where asked is left alone.
fn test_fullscreen(ob: &Observer, id: u32, on: bool) -> Result<Value, String> {
    let element = test_window(ob, id, "debug-fullscreen")?;
    let before = element.is_fullscreen().map_err(|e| e.to_string())?;
    if before == on {
        return Ok(json!({ "ok": true, "fullscreen_before": before, "changed": false }));
    }
    let via = match element.set_fullscreen(on) {
        Ok(()) => "AXFullScreen",
        Err(e) => {
            tracing::info!(id, "debug-fullscreen: AXFullScreen refused ({e}); pressing the full-screen button");
            element.press_fullscreen_button().map_err(|b| format!("AXFullScreen: {e}; full-screen button: {b}"))?;
            "AXFullScreenButton"
        }
    };
    tracing::info!(id, on, via, "debug-fullscreen");
    Ok(json!({ "ok": true, "fullscreen_before": before, "changed": true, "via": via }))
}

/// A standard window `id` a test may act on through Accessibility: one of an app on the
/// `STAGE_MANAGER_ONLY` list, never ours; any other is refused (`what` names the command).
fn test_window(ob: &Observer, id: u32, what: &str) -> Result<AxElement, String> {
    let only = ob.only().ok_or(format!("{what} needs STAGE_MANAGER_ONLY (fixture apps only)"))?;
    let cg = windows::list_windows().into_iter().find(|w| w.id == id).ok_or(format!("no window {id}"))?;
    let bundle = ob.bundle_id(cg.pid).map(str::to_string).unwrap_or_default();
    if cg.pid == std::process::id() as i32 || !only.contains(&bundle) {
        return Err(format!("window {id} is not a STAGE_MANAGER_ONLY app's window; refused"));
    }
    let element = match ob.window(id) {
        Some(t) => t.element.clone(),
        None => AxElement::application(cg.pid)
            .windows()
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.window_id().ok() == Some(id))
            .ok_or(format!("window {id} is not reachable through Accessibility (its Space is not shown?)"))?,
    };
    if element.subrole().ok().as_deref() != Some(super::ax::names::SUBROLE_STANDARD) {
        return Err(format!("window {id} is not a standard window"));
    }
    Ok(element)
}

/// The focused window: the frontmost app's `AXFocusedWindow`, as a `CGWindowID`.
pub fn focused_window() -> Option<u32> {
    AxElement::system_wide().focused_application().ok()?.focused_window().ok()?.window_id().ok()
}

/// The main display from CoreGraphics when AppKit has not reported one yet.
fn fallback_displays() -> Vec<Display> {
    let mut bounds = desktop::cg_display_bounds();
    let main = if bounds.is_empty() { Rect::new(0.0, 0.0, 1440.0, 900.0) } else { bounds.remove(0) };
    let menu = 30.0;
    let visible = Rect::new(main.x, main.y + menu, main.w, main.h - menu);
    vec![Display {
        id: DisplayId(objc2_core_graphics::CGMainDisplayID()),
        frame: main,
        visible,
        corner: choose_corner(main, visible, &bounds).corner,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quit_mid_batch_still_brings_back_what_the_model_shows() {
        let id = WindowId(3);
        let frame = Rect::new(10.0, 40.0, 300.0, 200.0);
        assert!(runs_after_quit(&Action::SetFrame { id, frame }));
        assert!(!runs_after_quit(&Action::Park { id, frame }));
        assert!(!runs_after_quit(&Action::Raise { id }));
        assert!(!runs_after_quit(&Action::Focus { id }));
        assert!(!runs_after_quit(&Action::StateChanged));
    }

    #[test]
    fn a_failed_restore_stays_registered_until_the_window_is_seen_back() {
        use UnrestoredStep::*;
        // Still in the park zone on screen: moved again, never dropped.
        assert_eq!(unrestored_step(Some(false), Some((true, true))), Retry);
        // Off screen (another Space, app hidden): kept, waiting.
        assert_eq!(unrestored_step(Some(false), Some((false, true))), Wait);
        assert_eq!(unrestored_step(Some(false), Some((false, false))), Wait);
        // Seen back out of the zone, closed, or parked again by the model: done.
        assert_eq!(unrestored_step(Some(false), Some((true, false))), Done);
        assert_eq!(unrestored_step(Some(false), None), Done);
        assert_eq!(unrestored_step(None, Some((true, true))), Done);
        assert_eq!(unrestored_step(Some(true), Some((true, true))), Done);
    }

    #[test]
    fn a_desktop_whose_own_strip_pane_shows_is_still_the_desktop_shown() {
        let win = |id: u32, onscreen: bool| windows::CgWindow {
            id,
            pid: 1,
            owner: String::new(),
            name: None,
            layer: 0,
            bounds: Rect::new(0.0, 0.0, 200.0, 900.0),
            onscreen,
            alpha: 1.0,
        };
        let pane = strip_model::PanelStatus { space: 7, window: 40, ..Default::default() };
        // R2 V7: the only window moved to another Desktop by hand; this Desktop's pane shows.
        assert!(pane_on_screen(&pane, 7, &[win(40, true), win(41, false)]));
        // A Space change: the pane stays on the Desktop left, off screen.
        assert!(!pane_on_screen(&pane, 7, &[win(40, false)]));
        // Another Space's pane, an unknown window, nothing listed (screen asleep).
        assert!(!pane_on_screen(&pane, 8, &[win(40, true)]));
        assert!(!pane_on_screen(&strip_model::PanelStatus { window: 0, ..pane }, 7, &[win(0, true)]));
        assert!(!pane_on_screen(&pane, 7, &[]));
    }

    #[test]
    fn a_desktop_with_nothing_left_on_screen_is_recognized_again_not_forgotten() {
        // S5 / R1-platform 12: one window, gone from the screen: another Space shows.
        assert!(missed_space_change(1, 0, 1));
        // Two of three gone, one still shown: a missed Space change by the majority rule.
        assert!(missed_space_change(3, 1, 2));
        // Every window gone at once.
        assert!(missed_space_change(5, 0, 5));
        // One of three left this Space while the others stay: forgotten one by one.
        assert!(!missed_space_change(3, 2, 1));
        // One of five gone, even with a second one missing but not yet twice: not a change.
        assert!(!missed_space_change(5, 3, 1));
        assert!(!missed_space_change(1, 0, 0), "nothing gone, nothing to decide");
    }

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
        assert!(
            !wallpaper_reveal_enabled(&NativeSettings::default()),
            "absent = Always (Apple's default): macOS reveals on its own, ours never runs alongside"
        );
        let his: RawSettings = [("AppWindowGroupingBehavior".to_string(), 1), ("GloballyEnabled".to_string(), 0)].into();
        assert!(!wallpaper_reveal_enabled(&native_settings(&his)), "his Mac: the key is absent");
        let always: RawSettings = [("EnableStandardClickToShowDesktop".to_string(), 1)].into();
        assert!(!wallpaper_reveal_enabled(&native_settings(&always)), "macOS reveals on its own");
        let only_sm: RawSettings = [("EnableStandardClickToShowDesktop".to_string(), 0)].into();
        assert!(wallpaper_reveal_enabled(&native_settings(&only_sm)));
    }

    #[test]
    fn a_double_click_on_the_wallpaper_is_one_click() {
        let only_sm: RawSettings = [("EnableStandardClickToShowDesktop".to_string(), 0)].into();
        let ours = native_settings(&only_sm);
        assert_eq!(wallpaper_click(&ours, 1), WallpaperClick::Toggle);
        assert_eq!(wallpaper_click(&ours, 2), WallpaperClick::SameClick, "two clicks 40 ms apart: no second toggle");
        assert_eq!(wallpaper_click(&ours, 3), WallpaperClick::SameClick);
        assert_eq!(wallpaper_click(&ours, 0), WallpaperClick::Toggle, "count unknown: a click of its own");
        let default = NativeSettings::default();
        assert_eq!(wallpaper_click(&default, 1), WallpaperClick::LeftToMacOs);
        assert_eq!(wallpaper_click(&default, 2), WallpaperClick::LeftToMacOs);
    }

    #[test]
    fn a_double_click_reveals_and_stays_revealed() {
        // The model and the controller's decision, wired as `mouse_down` wires them: his log
        // of 10-12 had two clicks 40 ms apart reveal and bring the stage straight back.
        use crate::engine::model::{ModelOptions, Observation};
        use crate::engine::rules::{Decision, Placement, RuleAction};
        use crate::engine::testkit::{meta, model_with};
        let only_sm: RawSettings = [("EnableStandardClickToShowDesktop".to_string(), 0)].into();
        let settings = native_settings(&only_sm);
        let mut m = model_with(ModelOptions::default());
        let window = meta(1, 10, "com.example.a");
        let obs = Observation { frames: [(window.id, window.frame)].into(), z_order: vec![window.id], focused: Some(window.id) };
        m.add_window(window.clone(), &Decision { action: RuleAction::New, placement: Placement::AsIs }, &obs);
        for clicks in [1, 2] {
            if wallpaper_click(&settings, clicks) == WallpaperClick::Toggle {
                m.reveal_desktop(&obs);
            }
        }
        assert!(m.desktop_revealed(), "the second click of the double click did not bring the stage back");
        if wallpaper_click(&settings, 1) == WallpaperClick::Toggle {
            m.reveal_desktop(&Observation::default());
        }
        assert!(!m.desktop_revealed(), "a later single click does");
    }

    fn ids(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .map(|a| match a {
                Action::SetFrame { id, .. } => format!("frame {}", id.0),
                Action::Park { id, .. } => format!("park {}", id.0),
                Action::Raise { id } => format!("raise {}", id.0),
                Action::Focus { id } => format!("focus {}", id.0),
                Action::StateChanged => "state".to_string(),
            })
            .collect()
    }

    #[test]
    fn a_covered_switch_focuses_the_new_stage_at_once() {
        let r = Rect::new(100.0, 100.0, 400.0, 300.0);
        let batch = vec![
            Action::Park { id: WindowId(1), frame: r },
            Action::Park { id: WindowId(2), frame: r },
            Action::SetFrame { id: WindowId(4), frame: r },
            Action::SetFrame { id: WindowId(3), frame: r },
            Action::Raise { id: WindowId(4) },
            Action::Raise { id: WindowId(3) },
            Action::Focus { id: WindowId(3) },
            Action::StateChanged,
        ];
        let arriving: HashSet<u32> = [3, 4].into();
        let (now, later) = split_covered(batch, &arriving);
        assert_eq!(ids(&now), ["park 1", "park 2", "state", "focus 3"], "the keyboard goes to the new stage with the parks");
        assert_eq!(
            ids(&later),
            ["frame 4", "frame 3", "raise 4", "raise 3", "focus 3"],
            "the windows arrive behind their snapshots, the key window in front last"
        );
        // A batch without a focus focuses nothing early.
        let (now, later) = split_covered(vec![Action::SetFrame { id: WindowId(3), frame: r }], &arriving);
        assert!(now.is_empty());
        assert_eq!(ids(&later), ["frame 3"]);
    }

    #[test]
    fn a_late_activation_after_a_second_quick_switch_is_still_ours() {
        let (slack, mail) = (10, 20);
        let mut e = Echoes::default();
        let t0 = Instant::now();
        e.touch_app(slack, t0);
        e.touch_app(mail, t0 + Duration::from_millis(400));
        assert!(e.is_app_echo(mail, t0 + Duration::from_millis(450)));
        // Slack, busy, reports its activation 800 ms after our focus: past the echo window.
        assert!(e.is_app_echo(slack, t0 + Duration::from_millis(800)), "late, but the one we asked for");
        assert!(!e.is_app_echo(slack, t0 + Duration::from_millis(900)), "a second one is not");
    }

    #[test]
    fn a_user_switch_ends_a_pending_late_echo() {
        let slack = 10;
        let mut e = Echoes::default();
        let t0 = Instant::now();
        e.touch_app(slack, t0);
        e.user_switched();
        assert!(!e.is_app_echo(slack, t0 + Duration::from_millis(900)), "a Dock click or ⌘Tab to it is the user's");
        assert!(e.is_app_echo(slack, t0 + Duration::from_millis(100)), "still within the echo window");
        // Never awaited forever: an app that was already frontmost never reports one.
        e.touch_app(slack, t0);
        e.prune(t0 + LATE_ACTIVATION_MAX);
        assert!(!e.is_app_echo(slack, t0 + LATE_ACTIVATION_MAX));
    }

    #[test]
    fn only_a_press_on_the_window_and_a_moving_pointer_start_a_drag() {
        let frame = Some(Rect::new(100.0, 100.0, 400.0, 300.0));
        let title_bar = Point::new(200.0, 110.0);
        assert!(drag_plausible(title_bar, frame, Some(Point::new(230.0, 120.0))));
        assert!(drag_plausible(Point::new(96.0, 200.0), frame, Some(Point::new(80.0, 200.0))), "the resize edge");
        assert!(
            !drag_plausible(Point::new(900.0, 700.0), frame, Some(Point::new(930.0, 720.0))),
            "the button is down in another window: the app moved its own window"
        );
        assert!(!drag_plausible(title_bar, frame, Some(title_bar)), "the pointer has not moved");
        assert!(!drag_plausible(title_bar, None, Some(Point::new(230.0, 120.0))), "not shown at the press");
    }
}
