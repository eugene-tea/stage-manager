//! The controller: owns the engine's [`Model`] on the WM thread, feeds it what the
//! [`Observer`] sees and executes the [`Action`]s it returns through Accessibility.
//!
//! - **Observations** for the engine: live frames through AX for every window the model
//!   shows, z-order from `CGWindowListCopyWindowInfo`, the focused window from the frontmost
//!   app's `AXFocusedWindow`.
//! - **Actions**: `SetFrame` = size, position, size; `Hide` = position into the park zone
//!   (the window engine's hide, [`WindowEngine::hide`]: the Corner engine's park frame);
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
//!   [`StripEvent::CardDropped`] (with a window id: that window of a combined card alone,
//!   [`Model::move_window_into_active_at`]), one dropped onto another card as
//!   [`StripEvent::CardMerged`] (`[strip] merge_on_drop`). A window of a combined card dragged
//!   off it alone and released in the column arrives as [`StripEvent::WindowToNewCard`] (a gap:
//!   [`Model::move_window_to_new_stage`]) or [`StripEvent::WindowToCard`] (onto a card:
//!   [`Model::move_window_to_stage`]).
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
//!   is one click. The reveal runs on the release, as Apple's: a still click shorter than
//!   [`desktop_click::CLICK_HOLD`] reveals, a drag or a long press reveals nothing; a press in
//!   a window's resize band, just outside its frame, is its edge, never a wallpaper click
//!   ([`desktop_click::reveals_on_release`], [`desktop_click::in_any_resize_band`]). macOS hands the desktop to Finder, so Finder's activation and its main
//!   window's focus follow the press (often while it is still down, about a second in);
//!   that echo, during any desktop press and just after its release, never switches stages
//!   or unparks a window ([`DesktopEcho`]), as with Apple's. A deliberate Finder activation (Dock click, ⌘Tab,
//!   its card) still brings its stage.
//! - **Shift sugar** (`[sugar] shift_brings_here`): Shift held at the user's triggering
//!   action ([`Triggers`]) brings windows here: a newly opened app's first window joins the
//!   current stage, and an activation (Dock click) of an app in another stage brings its
//!   windows here. The app switcher (Command held) never counts. With
//!   `[stages] new_apps_join_current` on, a newly opened app's first window joins the current
//!   stage without Shift, and Shift gives it a stage of its own (`Decision::for_new_window`).
//! - **Transitions** (`[animation]`): a batch that parks shown windows or brings parked ones
//!   back is announced to the strip first ([`strip_model::Transition`]); once the strip has
//!   drawn snapshots over them, outgoing windows park behind theirs (the whole motion starts
//!   then, both ways on one frame: [`Controller::note_parked`]), and incoming windows wait
//!   (in the safety registry at their arrival frames) until the strip's growing snapshots
//!   are within a point of their frames as shown ([`StripEvent::Unveil`],
//!   `motion_math::unveil_time`), or a click or another event arrives, or their backstop
//!   passes (the latest unveil at the configured speed plus a margin, 1.5 to 3 s:
//!   `motion_math::unveil_backstop`). The batch's focus does not wait: the keyboard goes to the new
//!   stage at once, as Apple's does (`split_covered`).
//! - **Raise order** (`raise_order`): after a batch that raised windows of two or more apps,
//!   the window server's order of them is read for a moment; windows that landed out of the
//!   order asked (an app's late reorder) are raised again, back to front, the key last.
//! - **Space settle gate**: while a Space change waits out a full-screen animation, nothing
//!   changes stages: window events, strip input, hotkeys, wallpaper clicks and ctl stage
//!   commands are held off (`Controller::frozen`).
//! - **The phase** ([`crate::engine::phase`]): what the main display shows, as one state
//!   fed at every transition (a Desktop steady, a switch's windows held, a Space change
//!   waiting and for what, a full-screen Space), with the settle after a Space change and
//!   the Desktop's slide as its clocks. Every hold above asks it; `ctl space` names it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use super::ax::{AxElement, AxError};
use super::ctl::Command;
use super::input::{self, InputEvent, InputKind, Modifiers, Triggers};
use super::minimize_tap::MinimizeAsk;
use super::native_settings::RawSettings;
use super::observe::{Observer, WinEvent, WindowKind};
use super::safety::{self, ParkedEntry};
use super::window_engine::{self, Motion, ShowHow, WindowEngine};
use super::{capture_pace, desktop, desktop_shot, enhanced_ui, hidden_tabs, logging, paths, session, windows};
use crate::config::{Config, ConfigStore, FileAction, FitMode, HotkeyCommand, HotkeyConfig, StripStyle};
use crate::engine::busy_apps::{BUSY_BACKOFF, BusyApps, Episode, OffScreen, Reply};
use self::corner::CornerUpkeep;
use self::corner::unrestored::LetGo;
use crate::engine::corner::park_reconcile::{self, Stray};
use crate::engine::corner::corner_patch::{CoverPicture, Refused, Staying, SwitchCover};
use crate::engine::desktop_click;
use crate::engine::desktop_echo::{Cause, DesktopEcho};
use crate::engine::displays::{display_kept, display_moves_now, display_of, off_every_display, off_screen_not_ours, on_other_display, returned_screens};
use crate::engine::early_start;
use crate::engine::focus_replay::{FocusReplay, Wanted};
use crate::engine::slide_hold::{self, SlideHold};
use crate::engine::stale_activation;
use crate::engine::stranded;
use crate::engine::hidden_tab::Candidate;
use crate::engine::leftovers;
use crate::engine::minimize;
use crate::engine::minimize_press::Minimizable;
use crate::engine::corner::park::{Corner, ParkRequest, choose_corner};
use crate::engine::recovery::{self, Known};
use crate::engine::corner::sliver;
use crate::engine::fullscreen::{self, Seen, Settle, Verdict};
use crate::engine::overview::{self, FOREIGN_SETTLE, Overview, OverviewWatch, Persisting, Sighting};
use crate::engine::phase::{self, Hold};
use crate::engine::show_desktop::{self, CloseWatch, Cover, CoverLook, PanelPlace, PushedCover, RevealFacts};
use crate::engine::geometry::{Point, Rect};
use crate::engine::model::{
    Action, Closing, Display, DisplayId, Home, Model, Observation, SpaceId, StageId, SwitchRefused, Target, WindowId, WindowMeta, WindowRole,
    WindowState,
};
use crate::engine::stage_area::{FitOptions, capacity_for, stage_inset, strip_width};
use crate::engine::persist::{LaunchHints, StateFile};
use crate::engine::restore::{self, AppQuiet, End, Restore, Settling, Summary};
use crate::engine::rules::{Decision, RuleAction, ShiftEffect};
use crate::engine::save_clock::SaveClock;
use crate::engine::settings::{NativeSettings, PrefValue};
use crate::logbook::counters::{self, Count};
use crate::ui::strip_model::{self, CardSnapshot, CardWindow, Minimized, StripDrop, StripEvent, StripSnapshot, Transition};
use crate::ui::{assistive, motion_math, transition_plan};

mod arrival;
mod ax_blind;
mod corner;
mod dock_order;
mod fallback;
mod landing_focus;
mod raise_order;
mod spaces;
mod stale;
mod switch_pictures;
mod switcher;
mod tabs;
#[cfg(test)]
mod tick_scenarios;
mod upkeep;

/// A focus this soon after a click or key press is the user's: the window is touched
/// ([`Model::touch_window`]).
const USER_FOCUS: Duration = Duration::from_millis(1000);
/// A stage switch this soon after a click or key press is the user's: its windows are
/// touched ([`Model::touch_stage`]).
const USER_SWITCH: Duration = Duration::from_millis(1500);

/// The windows read at a card's press serve its click this long (a press held longer reads
/// them again at the release).
const PRESSED_OBS_MAX: Duration = Duration::from_secs(1);

/// How long focus and activation events for windows and apps we just touched are ignored
/// by default (`[windows] focus_echo_ms`).
pub const ECHO_WINDOW: Duration = Duration::from_millis(700);

/// How long after our own batch showed a window an arrival of a Space change still counts it
/// as carried by our switch, not moved by the user ([`Model::set_slide_carried`]): macOS's
/// slide away for a switch's activation and back took ~70 ms and ~330 ms from the unveil
/// (11-15 07:33:48, 07:35:02); a second is ample and far below a user's move to another
/// Desktop and back.
const SLIDE_CARRY: Duration = Duration::from_millis(1500);

/// How long before its app's quit notice a destroyed window still counts as quit with it (an
/// app closes its windows as it quits, a moment before macOS says it terminated; round 8H).
const CLOSING_KEEP: Duration = Duration::from_secs(10);

/// Now, in Unix seconds (0 when the clock is before 1970).
fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// How often config.toml's modification time is checked.
const CONFIG_POLL: Duration = Duration::from_secs(1);

/// A window found before the window server lists it on screen (an app's new window, its AX
/// notice ahead of its first frame) is looked at on every fast tick for this long, then left
/// to the once-a-second check: waiting for that check left a newly opened app's window over
/// the stage shown for up to a second before its own stage came (11-13: 925 ms).
const FRESH_LOOK: Duration = Duration::from_millis(1500);

/// A window of an app launched this recently, found off screen, is looked at as a new one
/// ([`looks_soon`]): a heavy app's first window can show seconds after its launch notice.
const FRESH_APP: Duration = Duration::from_secs(10);

/// How long the Dock's bar as read for the inert-slivers tap is trusted ([`Controller::refresh_dock_bar`]).
const DOCK_BAR_FRESH: Duration = Duration::from_secs(2);
/// A window un-minimized on its way to the strip is parked again when its un-minimize ends
/// within this long ([`Controller::on_deminiaturized`]; measured 11-09: the notice 0.65 s
/// after the un-minimize was asked).
const UNMINIMIZE_MAX: Duration = Duration::from_secs(3);

/// How long Finder's desktop icons as read for the corner patch are trusted (a new
/// rectangle reads them at once).
const PATCH_ITEMS_FRESH: Duration = Duration::from_secs(5);

/// How soon a read of Finder's desktop icons that failed is tried again (the next tick),
/// and how soon after a mouse-up (a desktop icon may have been dropped) they are read.
const PATCH_ITEMS_RETRY: Duration = Duration::from_millis(250);

/// How long a press with no window under it waits for a read of Finder's desktop icons when
/// none is fresh ([`Controller::icons_for_press`]): whether it is on a desktop icon decides
/// whether it reveals the desktop.
const PRESS_ITEMS_WAIT: Duration = Duration::from_millis(60);

/// The longest wait between reads of Finder's desktop icons while they keep failing.
const PATCH_ITEMS_RETRY_MAX: Duration = Duration::from_secs(4);

/// How soon a read of Finder's desktop icons is tried again after `fails` failed reads in a
/// row: [`PATCH_ITEMS_RETRY`], doubled for each further one up to [`PATCH_ITEMS_RETRY_MAX`]
/// (a hung or absent Finder does not get a new thread four times a second).
fn items_retry(fails: u32) -> Duration {
    PATCH_ITEMS_RETRY.saturating_mul(1 << fails.saturating_sub(1).min(8)).min(PATCH_ITEMS_RETRY_MAX)
}

/// The earlier of a due time already set (a mouse-up's) and `at`.
fn sooner(due: Option<Instant>, at: Instant) -> Instant {
    due.map_or(at, |d| d.min(at))
}

/// Another screen's full-screen settle and overview watch, and what its once-a-second check
/// counts (round displays-2). The main screen's live on the controller (`settle`, `overview`,
/// `absent`) and feed the phase; each other screen has its own, so a full-screen app or an
/// overview on one screen never gates windows arriving on another, and a window left behind
/// on another screen's Desktop is let go as on the main one.
#[derive(Default)]
struct ScreenWatch {
    settle: Settle,
    overview: OverviewWatch,
    /// A full-screen app's Space shows there (logged once a change).
    fullscreen: bool,
    /// Windows of its Desktop missing from the screen for no visible reason, by the checks
    /// in a row ([`SPACE_CHECKS`]).
    absent: HashMap<u32, u32>,
}

impl ScreenWatch {
    /// True while this screen's checks wait on the tick ([`Controller::tick_waits`]), as the
    /// main screen's do: windows counted missing towards a second look, a full-screen
    /// transition waited out, an overview showing or just closed. Left out before (the review
    /// of 1.9, B-10), another screen's second look came a rest later each time.
    fn waits(&self, now: Instant) -> bool {
        !self.absent.is_empty() || self.settle.waiting(now) || self.overview.showing_or_expected(now) || self.overview.freezes(now)
    }
}

/// What another screen's window list shows ([`Controller::other_screen_seen`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OtherSeen {
    /// Mission Control, App Exposé or Show Desktop shows there (or just closed).
    Overview,
    /// A full-screen animation runs.
    Animation,
    /// A full-screen app's Space.
    FullScreen,
    /// A Desktop.
    Desktop,
}

/// Finder's desktop icons for the corner patch, read on a thread of their own
/// ([`super::desktop_items::start_read`]).
#[derive(Default)]
struct PatchItems {
    /// The icons (grown by their labels) of the last read that knew them; `None` before one,
    /// or after a read found too many to read: the patch stays off.
    known: Option<Vec<Rect>>,
    /// When `known` was read.
    known_at: Option<Instant>,
    /// The rectangle the last read was started for, per screen (a new rectangle on a screen
    /// starts a read; the icons read are every screen's).
    read_for: BTreeMap<u32, Rect>,
    /// When the next read is due (`None`: at once).
    due: Option<Instant>,
    /// The read running.
    reading: Option<std::sync::mpsc::Receiver<Option<crate::engine::corner::corner_patch::DesktopIcons>>>,
    /// Reads failed since the last good one (logged once a streak; spaces the retries,
    /// [`items_retry`]).
    fails: u32,
    /// What was last logged about a read that did not give icons.
    said: Option<String>,
}

/// Longest wait for the strip to draw a transition's snapshots before windows move anyway.
const COVER_TIMEOUT: Duration = Duration::from_millis(150);

/// How often the window server's list is checked for windows changing Spaces.
const SPACE_POLL: Duration = Duration::from_secs(1);

/// A window counts as moved to or gone from the shown Space after this many checks in a row.
const SPACE_CHECKS: u32 = 2;

/// For this long after the set of displays changed, a managed window found on another
/// display than the model has it on is macOS's doing (the lid opened: a screen that came
/// back gets the windows it had, at the frames they had there, a parked one at its old
/// corner; 11-05 16:27:31, two seconds after the screen), never the user's drag. The engine
/// takes both by where the window is listed ([`Model::windows_moved_to_display`]); the log
/// says which.
const DISPLAY_SETTLE: Duration = Duration::from_secs(10);

/// The window list right after a Space change notice may show two Desktops at once (the one
/// sliding out and the one sliding in, [`Model::shows_two_desktops`]): read again for at most
/// this long after the notice before the Desktop is recognized from it.
const MIXED_WAIT: Duration = Duration::from_millis(600);

/// Accessibility listing none of the windows on screen ([`session::Blind::AxSilent`]) holds a
/// launch's stages and a Space change's arrival at most this long (a locked screen holds
/// them until it unlocks).
const AX_SILENT_MAX: Duration = Duration::from_secs(10);

/// What an arrival is given ([`Controller::screen_view`]): the windows on the main screen
/// the engine knows or waits to place, and the managed windows missing for no visible reason.
type ScreenView = (Vec<WindowMeta>, Vec<WindowId>);

/// How often the login session is asked whether the screen is locked, and a launch waiting
/// for the Desktop to be seen looks again.
const LOCK_POLL: Duration = Duration::from_millis(500);

/// Updates for the main thread (menu and hotkeys).
#[derive(Clone, Debug, PartialEq)]
pub enum UiUpdate {
    /// The config in force: its load error (the last good config stays) and its hotkeys.
    Config {
        /// The load error, if the file is bad.
        error: Option<String>,
        /// The whole config in force (the last good one after an error): the shell takes
        /// this, never a read of its own, so both sides agree (review R2).
        config: Box<Config>,
        /// Hotkeys to register.
        hotkeys: HotkeyConfig,
        /// The strip's thumbnail refresh interval.
        thumbnail_interval_ms: u64,
        /// `[strip] capture`: window pictures on or off.
        capture: bool,
    },
    /// The strip's Show/Hide state.
    StripVisible(bool),
    /// The wallpaper corner patch's plan (`[windows] corner_patch`).
    CornerPatch(crate::ui::corner_patch::PatchPlan),
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
    /// The app whose activation was reported last (ours or not): frontmost as far as we know.
    /// A focus on it awaits no activation, since macOS reports none for an app already in
    /// front.
    front: Option<i32>,
    window: Duration,
}

impl Default for Echoes {
    fn default() -> Self {
        Self { windows: HashMap::new(), apps: HashMap::new(), awaiting: HashMap::new(), front: None, window: ECHO_WINDOW }
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

    /// We just made this app frontmost. Its activation is awaited only when another app was
    /// in front: a switch focuses its key window twice (at once, then again at the landing),
    /// and the second focus, after the activation was reported, left one awaited that never
    /// came, so macOS's own activation of that app up to 5 s later was taken for ours
    /// (11-09 suite: the front app quit, macOS activated the fixture 4.4 s after the
    /// landing, and its stage never came).
    pub fn touch_app(&mut self, pid: i32, now: Instant) {
        self.apps.insert(pid, now);
        if self.front != Some(pid) {
            self.awaiting.insert(pid, now);
        }
    }

    /// True when a focus event for `id` is probably our own.
    pub fn is_window_echo(&self, id: u32, now: Instant) -> bool {
        self.windows.get(&id).is_some_and(|t| now.duration_since(*t) < self.window)
    }

    /// True when an activation of `pid` is probably our own: within the echo window, or
    /// the first activation reported since we made it frontmost with no user input between.
    /// Call it once per reported activation: it consumes the awaited one.
    ///
    /// An awaited one it consumes also ends the awaits of the apps we made frontmost more
    /// than an echo window before it: our later switch made this app frontmost and macOS
    /// says so, so an older request is answered by now or never will be (11-14, the live
    /// landing: the fixture's activation never came, Alpha's switch 1.8 s later was reported,
    /// and the user's activation of the fixture 1.3 s after that was taken for the late
    /// echo). Quick switches keep theirs: within the echo window of each other the first
    /// app may still be answering ([`ECHO_WINDOW`]), and one switch focuses one window.
    pub fn is_app_echo(&mut self, pid: i32, now: Instant) -> bool {
        self.front = Some(pid);
        let asked = self.awaiting.remove(&pid);
        if let Some(at) = asked {
            let window = self.window;
            self.awaiting.retain(|_, t| at.saturating_duration_since(*t) < window);
        }
        let late = asked.is_some_and(|t| now.saturating_duration_since(t) < LATE_ACTIVATION_MAX);
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
/// raises, so the key window ends in front) wait. An activation's early focus
/// ([`Controller::focus_now`]) comes before all this and changes none of it.
///
/// The raises all wait, another app's windows' too, even after the early focus gave the
/// activated app the keyboard: raised now they would rise in the park corner, over the corner
/// patch, beside the cover drawn for the activated app's. An `AXRaise` of another app's window
/// reorders it without activating that app (10-27: Mail's window raised after Calendar's early
/// focus noted a focused-window change in Mail, no activation), and the batch raises the key
/// window and its children last ([`crate::engine::model::Model::show_stage`]), right before
/// the focus, which raises the key again: the activated window ends in front.
///
/// A focus on an arriving window the engine says must be shown first (`waits`,
/// [`WindowEngine::focus_waits_for_show`]: under the Spaces engine it is on no Desktop until
/// shown, and its app, with a window on another Desktop, activated now would take macOS there) runs only with the
/// arrivals.
fn split_covered(actions: Vec<Action>, arriving: &HashSet<u32>, waits: impl Fn(WindowId) -> bool) -> (Vec<Action>, Vec<Action>) {
    let focus: Vec<Action> = actions
        .iter()
        .copied()
        .filter(|a| matches!(a, Action::Focus { id } if !(arriving.contains(&id.0) && waits(*id))))
        .collect();
    let (later, mut now): (Vec<Action>, Vec<Action>) = actions.into_iter().partition(|a| match a {
        Action::SetFrame { id, .. } => arriving.contains(&id.0),
        Action::Raise { .. } | Action::Focus { .. } => true,
        Action::Hide { .. } | Action::StateChanged => false,
    });
    now.extend(focus);
    (now, later)
}

/// The windows of `waiting` (an app's windows waiting in `unplaced`) that a notice of that app
/// (its activation, a focus) offers to [`Controller::on_found`]: the ones the window server
/// lists on a managed screen now (`listed`, [`Controller::shown_managed`]), in order. One still
/// away (minimized, another Desktop's, a dialog its app keeps off screen) waits on untouched:
/// offered, it went back to wait only after landing a switch's held windows (11-09: our own
/// switch's activation of Messages and of Safari, each with such a window, brought the
/// incoming windows in 135 and 230 ms into the motion, before the echo check ran; the
/// snapshots unveiled at about 670 ms). The once-a-second check offers it once it shows.
fn offered_at_notice(waiting: impl IntoIterator<Item = u32>, listed: &HashSet<u32>) -> Vec<u32> {
    waiting.into_iter().filter(|id| listed.contains(id)).collect()
}

/// Whether ctl command `cmd` brings a running switch's held incoming windows to their frames
/// first. Only a command that changes something does; a question (`state`, `space`, `dump`,
/// `adopted`) leaves the motion alone: the scripted checks poll `state` while a switch runs,
/// and that poll brought the incoming windows to their frames 40-60 ms into the grow,
/// visible under the overlay (live 10-16, round 8L3).
///
/// The commands that still land it first: those that act on the stages or windows
/// (`switch`, `send`, `new`, `pin`, `merge`, `reveal`, `unpark-all`, `reload`) and the test
/// commands the WM thread runs (`debug-frame`, `debug-fullscreen`, `debug-dock-click`,
/// `debug-close`). `ping`, `mark`, `quit`, `settings` and the main thread's test commands
/// (`debug-strip`, `debug-overlay`, `debug-click`, `debug-hover`, `debug-drag`,
/// `debug-space`, `debug-expose`, ...) never reach the WM thread's `ctl`, so they leave the
/// motion alone too (`mark` asks `state` and `dump`).
fn ctl_lands_switch(cmd: &Command) -> bool {
    !matches!(cmd, Command::State | Command::Space | Command::Dump | Command::Adopted(_) | Command::DebugCmdTab(_) | Command::DebugSwitcher)
}

/// Gives window `element` of app `pid` the keyboard: raised and made main first, then only the
/// app brought forward (never all its windows): with its main window on the shown Space, macOS
/// has no reason to switch Spaces. The engine never focuses another Space's.
fn ax_focus(element: &AxElement, pid: i32) -> Result<(), AxError> {
    let _ = element.raise();
    let main = element.set_main(true);
    let front = AxElement::application(pid).set_frontmost(true);
    main.and(front)
}

/// The whole budget of an activation's early focus ([`Controller::focus_now`]): one step,
/// the window made main and its app frontmost, while the parks wait behind it under
/// snapshots that start to shrink 200 ms after they show (the strip motion's
/// `PARK_WAIT_MAX`). The rest of the focus (the raise, and all of it again) runs with the
/// parks, as for a card click.
const EARLY_FOCUS_BUDGET: Duration = Duration::from_millis(50);

/// Below this much of [`EARLY_FOCUS_BUDGET`] left, the early focus's second call is not made
/// (the app is busy: it goes with the parks).
const EARLY_FOCUS_MIN_CALL: Duration = Duration::from_millis(5);

/// An activation notice older than this is not the one that caused a switch (the timing
/// logged for the keyboard, [`ActivationRun::noticed`]).
const ACTIVATION_NOTICE_FRESH: Duration = Duration::from_secs(2);

/// How the animated batch an activation caused runs (⌘Tab, a Dock click, a focus such as a
/// click on a parked sliver, to an app in another stage; `[animation] activation_grows`).
#[derive(Debug, Clone, Default, PartialEq)]
struct ActivationRun {
    /// Off (6V), or on with a corner the overlay cannot cover ([`Self::fallback`]): the
    /// activated app's windows come to their frames first and at once, outside the
    /// transition (macOS has raised them in the park corner already).
    now: HashSet<u32>,
    /// On (the default, round 8L): the batch's focus when it is the activated app's window,
    /// made main and its app frontmost in one bounded step the moment the transition is sent,
    /// before the strip's answer and the parks: the keyboard is never left waiting for the
    /// motion. The windows grow from their card as a card click's do, waiting parked in the
    /// corner, which the overlay covers ([`crate::engine::corner::corner_patch::switch_cover`]).
    focus: Option<WindowId>,
    /// On: the activated app's arriving windows, brought at once as with it off (6V) when the
    /// corner cannot be covered for this switch (round 8L2: the patch off, an icon under the
    /// corner, its picture not ready...), so macOS's raise never shows them there.
    fallback: HashSet<u32>,
    /// When macOS's activation notice came, for the log's keyboard timing.
    noticed: Option<Instant>,
    /// Why the windows arrived at once instead of growing ([`Self::arrive_now`]), for the
    /// switch's log line (`at_once`): the checks tell a corner that could not be covered
    /// (icons there with captures off, say) from a grow that went wrong (8L3).
    at_once: Option<&'static str>,
    /// The windows [`Self::at_once`] names (a window that stays shown into the corner with
    /// no snapshot of it, 8L5), logged on the switch's line: the checks tell the owner's own
    /// window from a fixture's.
    at_once_windows: Vec<u32>,
}

impl ActivationRun {
    /// The corner cannot be covered: the activated app's windows arrive at once (6V), the
    /// focus with the parks.
    fn arrive_now(&mut self, why: &'static str) {
        self.now.extend(self.fallback.drain());
        self.focus = None;
        self.at_once = Some(why);
    }
}

/// The [`ActivationRun`] of `actions`, the switch an activation of app `pid` caused, with
/// `grows` (`[animation] activation_grows`); `pid_of` gives a window's app.
fn activation_run(grows: bool, pid: i32, actions: &[Action], pid_of: impl Fn(u32) -> Option<i32>) -> ActivationRun {
    let mine: HashSet<u32> = actions
        .iter()
        .filter_map(|a| match a {
            Action::SetFrame { id, .. } => Some(id.0),
            _ => None,
        })
        .filter(|id| pid_of(*id) == Some(pid))
        .collect();
    if grows {
        let focus = actions.iter().rev().find_map(|a| match a {
            Action::Focus { id } => Some(*id),
            _ => None,
        });
        let focus = focus.filter(|id| pid_of(id.0) == Some(pid));
        return ActivationRun { now: HashSet::new(), focus, fallback: mine, ..ActivationRun::default() };
    }
    ActivationRun { now: mine, ..ActivationRun::default() }
}

/// Logs once per reason (at debug) why an activation arrived at once instead of growing.
fn note_fallback(said: &mut HashSet<&'static str>, why: &'static str) {
    if said.insert(why) {
        tracing::debug!(why, "activation: the park corner cannot be covered; the activated windows arrive at once (noted once per reason)");
    }
}

/// The order a run of windows found together is placed in: creation (window id) order, but
/// each app's windows front to back in the slots its windows take in that order (`z`: the
/// window server's front-to-back place; unlisted windows after, oldest first). The first
/// window of a new app placed brings its stage in front and is raised and focused: taken
/// oldest first, an app that opened two windows at once had its older one raised over the
/// one it opened last, and kept it in front on every switch to the stage (11-09: Red 1 over
/// Red 2; Apple's shows Red 2, the window used last, in front).
fn found_order(mut run: Vec<u32>, pid: impl Fn(u32) -> Option<i32>, z: impl Fn(u32) -> Option<usize>) -> Vec<u32> {
    run.sort_unstable();
    run.dedup();
    let mut slots: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    for (i, &id) in run.iter().enumerate() {
        if let Some(p) = pid(id) {
            slots.entry(p).or_default().push(i);
        }
    }
    let mut out = run.clone();
    for at in slots.values().filter(|a| a.len() > 1) {
        let mut ids: Vec<u32> = at.iter().map(|&i| run[i]).collect();
        ids.sort_by_key(|&id| (z(id).unwrap_or(usize::MAX), id));
        for (&i, id) in at.iter().zip(ids) {
            out[i] = id;
        }
    }
    out
}

/// How long each Accessibility question of [`Controller::found_z`] may take.
const HELD_ORDER_BUDGET: f32 = 0.1;

/// Each of `held` (one app's windows held on their way in) with its rank front to back:
/// its place in `listed` (the app's `AXWindows`, front to back), the ones it does not list
/// after those, by id (creation order).
fn held_ranks(held: &[u32], listed: &[u32]) -> Vec<(u32, usize)> {
    let mut ranked: Vec<u32> = held.to_vec();
    ranked.sort_by_key(|id| (listed.iter().position(|l| l == id).unwrap_or(usize::MAX), *id));
    ranked.into_iter().enumerate().map(|(rank, id)| (id, rank)).collect()
}

/// Whether an app's activation is held, nothing switching for it now. `frozen`: no stage
/// change on the main screen ([`Controller::frozen`]: a full-screen app's Space there, a
/// Space change settling, the lock); `screens_frozen`: for each screen holding a window of
/// the app, that screen's own answer ([`Controller::frozen_on`]). Held while frozen, unless
/// the app's windows are all on screens that take input: a full-screen app on the main screen
/// stops nothing on the other (UX-SPEC, "Screens"), and macOS takes the user to no Desktop of
/// the main screen for an app with no window there. An app with no window placed is held as
/// before (its window may be on a Desktop not visited).
fn activation_held(frozen: bool, screens_frozen: impl IntoIterator<Item = bool>) -> bool {
    if !frozen {
        return false;
    }
    let mut placed = false;
    for screen_frozen in screens_frozen {
        if screen_frozen {
            return true;
        }
        placed = true;
    }
    !placed
}

/// Whether a left press reads the window server's list: Stage Manager manages windows
/// (`managing`) and something can act on the press, the Dock's prediction (not `frozen`: no
/// full-screen Space on the main screen, no Space change settling, no lock) or a wallpaper
/// click on the screen pressed (`pressed_frozen`: that screen's own answer,
/// [`Controller::frozen_on`]; `None`: no managed screen there). A full-screen app on the
/// main screen leaves the other screen's wallpaper click working, so its press reads the list
/// too: judged on a list not read, every press there was a wallpaper click.
fn press_reads_list(managing: bool, frozen: bool, pressed_frozen: Option<bool>) -> bool {
    managing && (!frozen || pressed_frozen == Some(false))
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

/// A wallpaper press whose reveal waits for the release, as Apple's: it runs only for a still,
/// short one ([`desktop_click::reveals_on_release`]); a drag or a long press reveals nothing.
#[derive(Debug, Clone, Copy)]
struct WallpaperPress {
    at: Point,
    screen: DisplayId,
    /// When it was pressed ([`InputEvent::stamp`]).
    stamp: f64,
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
    /// A window no stage holds (stranded, round stage-lost): taken into the stage in front
    /// when the drag ends, before the drop is done.
    adopt: bool,
}

/// Incoming windows of a transition, held parked until their snapshots cover their frames:
/// the batch [`Phase::Switching`](phase::Phase::Switching) holds (the phase keeps its clocks).
#[derive(Debug)]
struct Pending {
    /// The transition.
    id: u64,
    /// Their `SetFrame`s, then the batch's raises and focus, in order.
    actions: Vec<Action>,
    /// Registry entries bringing them to their arrival frames if anything goes wrong.
    entries: BTreeMap<u32, ParkedEntry>,
    /// Their arrival frames: a click there during the wait was meant for them.
    frames: Vec<Rect>,
    /// The switch's batch (round enhanced-ui): an app with its enhanced UI on keeps it off
    /// from the switch's first park to its landing, once; on again when this drops.
    _quiet: enhanced_ui::Batch,
}

/// How a batch of actions runs ([`Controller::execute_batch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Batch {
    /// What the user caused: stops between actions once Quit is requested, one try each.
    User,
    /// A switch's arrivals unveiled behind their snapshots: never interrupted (these windows
    /// are not parked in the model's eyes), one try each (M2).
    Unveil,
    /// An unpark (quit, stand-down): never interrupted, and a busy app is tried again
    /// quickly ([`safety::set_frame_retrying`]).
    Unpark,
}

impl Batch {
    /// Stops between actions once Quit is requested.
    fn interruptible(self) -> bool {
        self == Self::User
    }

    /// Tries a busy app's restore again quickly: only an unpark; a switch's arrivals keep
    /// the single try, so a busy app never holds a switch up (the review of 6X, M2).
    fn retries(self) -> bool {
        self == Self::Unpark
    }
}

/// The frames an unpark's batch restores windows to, by window id (S1).
fn unparked_frames(actions: &[Action]) -> BTreeMap<u32, Rect> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::SetFrame { id, frame } => Some((id.0, *frame)),
            _ => None,
        })
        .collect()
}

/// The name a `state.json` that cannot be read is set aside under, beside it.
const UNREAD_SUFFIX: &str = ".unread";

/// Reads the saved stages at `path` (`state.json`). A file that is there and cannot be read
/// (damaged, or written by a later version of the app: a downgrade) is set aside as
/// `state.json.unread`, replacing an older one, before anything saves over it: the first
/// save of this run would otherwise be the end of every stage in it. Logged, never shown
/// (restore is best effort and silent). `None` then, and when there is no file.
fn read_state_or_set_aside(path: &Path) -> Option<StateFile> {
    let why = match std::fs::read_to_string(path) {
        Ok(text) => match StateFile::from_json(&text) {
            Ok(s) => return Some(s),
            Err(e) => e,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => format!("state.json: {e}"),
    };
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(UNREAD_SUFFIX);
    let aside = path.with_file_name(name);
    match std::fs::rename(path, &aside) {
        Ok(()) => tracing::warn!("{why}; starting without saved stages; the file is kept as {}", aside.display()),
        Err(e) => tracing::warn!("{why}; starting without saved stages; the file could not be set aside ({e}) and the next save replaces it"),
    }
    None
}

/// After a wallpaper click left to macOS's reveal, how long a check finding every window on
/// screen waits before the reveal counts as over (macOS slides them away over a moment).
const MACOS_REVEAL_SETTLE: Duration = Duration::from_secs(3);

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
    /// When `state.json` is written next ([`SaveClock`]: dirty until a save succeeds).
    save: SaveClock,
    strip_dirty: bool,
    /// Saved entries of windows just destroyed, read before the model forgot them: those of an
    /// app that quits within [`CLOSING_KEEP`] are kept for `state.json` (round 8H,
    /// [`Model::keep_quit`]).
    closing: Vec<(Instant, Closing)>,
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
    /// The last switch's focus, owed again once its windows flying their last stretch have
    /// landed ([`landing_focus`]).
    landing_focus: Option<landing_focus::LandingFocus>,
    next_transition: u64,
    /// The transition whose outgoing windows just parked, for the next snapshot.
    parked_note: Option<u64>,
    /// A just-opened app's window of transition `.0`, let out where it shows mid-motion
    /// (`.1`): the next snapshot tells the strip, whose overlay goes under it.
    under_note: Option<(u64, u32)>,
    /// The transition whose incoming windows are in place, with their frames read back, for
    /// the next snapshot.
    arrived_note: Option<(u64, Vec<(u32, Rect)>)>,
    /// How the next animated batch runs when an activation caused it ([`ActivationRun`],
    /// `[animation] activation_grows`).
    activation: ActivationRun,
    /// Why activations arrived at once instead of growing, each logged once ([`note_fallback`]).
    fallback_said: HashSet<&'static str>,
    /// Finder's desktop icons as last read for the patches ([`PatchItems`]; the icons are
    /// read for every screen at once).
    patch_items: PatchItems,
    /// Tracked windows not given to the engine yet: not on screen when found (another
    /// Space, minimized, app hidden). Offered once they show.
    unplaced: BTreeSet<u32>,
    /// Windows of `unplaced` that were just found, not yet listed on screen, and until when
    /// the fast tick looks at them ([`FRESH_LOOK`]).
    fresh_unplaced: BTreeMap<u32, Instant>,
    /// Window tabs selected that Accessibility did not list when their app was asked again
    /// ([`tabs`]), and when it was: asked again only after [`tabs::ASK_AGAIN`].
    tab_asked: HashMap<u32, Instant>,
    /// Windows our own batches moved onto the screen or set a frame for, and when (round
    /// stage-lost): named to the arrivals of a Space change within [`SLIDE_CARRY`]
    /// ([`Model::set_slide_carried`]).
    shown_by_us: HashMap<u32, Instant>,
    /// The launch's stages wait: the app could not see the Desktop ([`session::blindness`])
    /// when observation started; why and since when. Nothing is built or saved meanwhile.
    build_wait: Option<(session::Blind, Instant)>,
    /// The launch's stages wait for the screens (round early-start,
    /// [`early_start::screens_wait`]): why; and when the launch first tried to build.
    screens_wait: Option<early_start::ScreensWait>,
    build_tried: Option<Instant>,
    /// The screen is locked (or the session is not on the console): no Space is inferred,
    /// no window is counted gone, and `state.json` is not written until it unlocks.
    locked: bool,
    /// When the login session was last asked ([`LOCK_POLL`]).
    lock_checked: Option<Instant>,
    /// Since when Accessibility has listed none of the known windows on screen at a Space
    /// change (the arrival waits at most [`AX_SILENT_MAX`]).
    ax_silent_since: Option<Instant>,
    /// When every app's window list was last read again while Accessibility was silent at an
    /// arrival (at most once per [`LOCK_POLL`]).
    ax_silent_rescan: Option<Instant>,
    /// Waits out full-screen animations.
    settle: Settle,
    /// The other screens' full-screen settles and overview watches ([`ScreenWatch`], by the
    /// screen's id; round displays-2): the main screen's are `settle` and `overview`.
    screens: BTreeMap<u32, ScreenWatch>,
    /// The Desktop and transition axis as one state ([`phase::Tracker`]): a Desktop steady,
    /// a switch's windows held behind their snapshots, a Space change waiting to be
    /// recognized (and for what), a full-screen app's Space; with the settle after a Space
    /// change and the Desktop's slide as the corner patch's plan reads it. Every hold on
    /// one of these asks it.
    phase: phase::Tracker,
    /// Space changes handled since launch (`ctl space`: a test's switch shows as a step).
    space_changes: u64,
    /// When the last animated switch was sent (the desktop's still rests after a while with
    /// no input and no switch, [`desktop_shot::rest`]).
    last_switch: Option<Instant>,
    /// When the set of displays last changed (a lid closed or opened, a screen plugged in or
    /// out): for [`DISPLAY_SETTLE`] after it, windows found on another display are macOS's
    /// doing, not the user's drag ([`Self::follow_display_moves`]).
    displays_changed_at: Option<Instant>,
    /// The displays that came at that change (the lid opened), kept through a change after
    /// it that adds none: within [`DISPLAY_SETTLE`] a window the model has parked elsewhere
    /// that macOS lists on one of them goes back to it ([`Self::reconcile_parked`]).
    displays_added: Vec<DisplayId>,
    /// When the latest switch we began started (a batch of ours that parks or focuses): an
    /// activation that fired before it never starts one ([`stale_activation::judge`]).
    our_switch: Option<Instant>,
    /// When the window server's list was last checked for Space changes.
    space_checked: Instant,
    /// Managed windows of other Spaces on screen: counted once the same ones show at two
    /// checks a settle apart ([`Persisting`]).
    foreign: Persisting,
    /// Mission Control, App Exposé and Show Desktop: no Space is inferred from the window
    /// list while one shows and for a settle after ([`OverviewWatch`]).
    overview: OverviewWatch,
    /// The windows as read when a card was pressed ([`StripEvent::CardPressed`]): the click
    /// that follows uses them instead of reading every frame again through AX after the
    /// release. Any event, batch, Space change, config reload, retried restore or window
    /// left for another display forgets them.
    pressed_obs: Option<(Instant, Observation)>,
    /// A focus or activation that came before its Desktop was recognized (its window on a
    /// Desktop not shown, not known yet, or a Space change settling): replayed on arrival
    /// ([`FocusReplay`], [`Model::bring_focus_forward`]).
    focus_replay: FocusReplay,
    /// An activation or focus held because a Desktop slides before the Space change's notice
    /// ([`slide_hold`]): the arrival replays it, or it is decided once the hold is over
    /// ([`Self::slide_hold_step`]).
    slide_hold: Option<SlideHold>,
    /// The held window's Desktop, arranged before it was recognized ([`arrival`]).
    ahead: Option<arrival::AheadSent>,
    /// A left mouse-down on an application item of the Dock, until its mouse-up.
    dock_press: Option<super::dock::DockPress<arrival::Prediction>>,
    /// The Dock's pid, while it runs.
    dock_pid: Option<i32>,
    /// The Dock and its bar as the inert-slivers tap last got them, and when they were read
    /// ([`Self::publish_shield`]).
    dock_bar: Option<(Instant, sliver::DockBar)>,
    /// "Follow the Dock": when the Dock's order is read next, and one waiting for the strip
    /// to be still ([`dock_order`]).
    dock_order: dock_order::DockOrderWatch,
    /// How windows are hidden, shown and brought back ([`window_engine::current`]).
    engine: Arc<dyn WindowEngine>,
    /// What the Corner engine keeps on this thread between its calls ([`CornerUpkeep`]: the
    /// corner patch, the windows whose restore failed, the strays, a minimize's dive).
    corner: CornerUpkeep,
    /// What the Spaces engine keeps on this thread between its calls ([`spaces::SpacesUpkeep`]);
    /// empty under the Corner engine.
    spaces: spaces::SpacesUpkeep,
    /// A minimized window on its way to the strip, while its batch runs: its transition's
    /// picture starts at its Dock tile ([`Self::on_minimized`]).
    minimizing: Option<Minimized>,
    /// Minimized windows un-minimized on their way to the strip, and when: each is parked
    /// again as soon as its un-minimize is over ([`Self::on_deminiaturized`]).
    unminimizing: HashMap<u32, Instant>,
    /// The moves of an arrangement made at a Dock click, until the window server shows them.
    verify: Option<arrival::Verify>,
    /// The latest Dock press's prediction, until a Space change shows where it went.
    dock_seen: Option<arrival::DockSeen>,
    /// A window put on the Desktop shown at a Dock press, still counted hidden, until its
    /// stage comes in front or it goes back ([`arrival::Lent`]).
    lent: Option<arrival::Lent>,
    /// ⌘Tab's switcher as the keys and the Dock show it ([`switcher`], round cmd-tab).
    switcher: switcher::SwitcherWatch,
    /// While set, a batch leaves the strip snapshot for later (`strip_dirty`): an arrival
    /// hands the strip one snapshot, in its final order.
    defer_present: bool,
    /// Managed windows of the shown Space missing from the screen: checks in a row.
    absent: HashMap<u32, u32>,
    /// Apps that did not answer an off-screen check in time, left alone a moment (review H1,
    /// [`crate::engine::busy_apps`]). A cell: the checks run from `&self` paths too.
    busy_apps: std::cell::RefCell<BusyApps>,
    /// Managed windows showing mostly on another display than the main one: checks in a
    /// row (left alone there, the guide).
    elsewhere: HashMap<u32, u32>,
    /// Managed windows listed on screen off every display, not in a park zone (moved there by
    /// their app: not the user's, [`crate::engine::displays`]): checks in a row.
    off_screen: HashMap<u32, u32>,
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
    /// The frames the latest [`Self::unpark_all`] restored windows to: the leftovers check
    /// after it brings a window its app moved late back there ([`safety::Ours::unparked`], S1).
    unparked: BTreeMap<u32, Rect>,
    /// The app of each window in [`Self::unparked`]: a window of the same app and size the
    /// leftovers check finds unknown in the corner is the tab group's selected tab, and goes
    /// back to the group's frame ([`safety::Ours::unparked_apps`], round tab-quit).
    unparked_apps: BTreeMap<u32, i32>,
    /// Windows whose Accessibility handle died: when each was looked up afresh, and those
    /// judged closed once ([`stale`], [`crate::engine::stale_handle`]).
    stale: stale::StaleHandles,
    /// When the user last focused each window (a focus event that was not our echo), kept
    /// until our next park of it: the reconciler adopts a parked window the user focused
    /// since it was parked, never one macOS focused at an un-minimize or our own focus
    /// ([`Self::reconcile_parked`]).
    user_focus: HashMap<u32, Instant>,
    /// When a wallpaper click was left to macOS's own reveal (Show Desktop: every window slides
    /// aside): until its windows are back, a check never counts them gone, so the reveal is
    /// never read as a Space change (review R2 V8). Ends with a click off the wallpaper or a
    /// Space change.
    macos_reveal_at: Option<Instant>,
    /// A wallpaper press near a window's edge whose reveal waits for its release.
    wallpaper_press: Option<WallpaperPress>,
    /// Shown windows (the active stage's, pinned ones) the window server did not show at the
    /// last strip snapshot (app hidden, minimized), per display: left out of the strip's
    /// overlap test; a change seen by the once-a-second check sends a new snapshot.
    active_off_screen: HashMap<DisplayId, BTreeSet<u32>>,
    /// When each app with pending windows was last busy: a quiet one's pending windows are
    /// given up ([`crate::engine::restore`]).
    quiet: AppQuiet,
    /// Accessibility blind for the session ([`ax_blind`]).
    ax: ax_blind::AxWatch,
    /// When pending windows were last checked for giving up.
    quiet_checked: Instant,
    /// Each saved Space's last time shown (Unix seconds), from the loaded file and the saves
    /// since ([`StateFile::stamp`]).
    seen_times: HashMap<u64, u64>,
    /// The restore window's clock while it is open (its ends by itself: apps quiet, or
    /// [`crate::engine::restore::WINDOW`] after the stages were built from `state.json`); `None` once closed
    /// or when there is none.
    settling: Option<Settling>,
    /// When a window's new title stops taking it to its saved stage
    /// ([`Settling::rematch_until`], [`Model::end_rematch`]): the restore window may end
    /// sooner (a first click), this does not.
    rematch_until: Option<Instant>,
    /// When the waiting entries of apps not running are given up, once the restore window
    /// ended ([`Settling::give_up_absent_at`], [`Controller::give_up_absent`]).
    absent_due: Option<Instant>,
    /// The restore window ended (or a give-up after it emptied the front stage): at the next
    /// tick a still-empty saved active stage gives way to a stage with windows
    /// ([`Model::front_after_restore`]), after the user's action that ended it had its say.
    front_after_restore: bool,
    /// The restore window ended by itself: at the next tick the saved key window takes the
    /// keyboard ([`Model::focus_after_restore`]).
    focus_after_restore: bool,
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

/// The windows of `list` by id, at their bounds (top-left global points, as AX frames are),
/// on screen or not: `ctl state`'s live frames.
fn described_bounds(list: &[windows::CgWindow]) -> HashMap<u32, Rect> {
    list.iter().map(|w| (w.id, w.bounds)).collect()
}

/// The windows the strip keeps full-size snapshots of for the next switch (`windows`): none
/// while the user has hidden the strip, when no switch animates ([`Controller`]'s animated
/// batch runs at once then) and a snapshot could never be used (the review's U1: a capture of
/// every active window every two seconds, for nothing).
fn kept_ready(strip_visible: bool, windows: Vec<CardWindow>) -> Vec<CardWindow> {
    if strip_visible { windows } else { Vec::new() }
}

/// True when a window found off screen may be about to show, so the fast tick looks at it
/// ([`FRESH_LOOK`]): the window server does not list it yet or lists it with no frame yet (a
/// new window is listed off screen at 0x0 from its creation until it first shows, `probe-spaces
/// sources`), or its app launched within [`FRESH_APP`] (`launched_ago`). A window of another
/// Desktop, a minimized one or one of a hidden app is listed at its frame: an app's rescan at
/// its activation finds such windows, and each kept the fast tick reading the whole window
/// list every 20 ms for 1.5 s (the review of 1.9, B-7); the once-a-second check offers them.
fn looks_soon(listed: Option<Rect>, launched_ago: Option<Duration>) -> bool {
    listed.is_none_or(|b| b.is_degenerate()) || launched_ago.is_some_and(|d| d < FRESH_APP)
}

/// Notes in `shown` that our own batch set window `id`'s frame at `now`, forgetting the
/// windows noted [`SLIDE_CARRY`] or longer ago ([`Controller::slide_carried`] reads what is
/// left). Pruned only at a Space change before (the review of 1.9, B-8), the map kept every
/// window ever shown on a Mac with one Desktop.
fn note_shown_by_us(shown: &mut HashMap<u32, Instant>, id: u32, now: Instant) {
    shown.retain(|_, at| now.saturating_duration_since(*at) < SLIDE_CARRY);
    shown.insert(id, now);
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
                let using = store.fallback().map_or("the built-in defaults", |s| s.describe());
                tracing::error!("config: {e}; using {using}");
                Some(e.to_string())
            }
        };
        let this = Self::with_store(store, &path, config_error);
        logging::apply_config(&this.store.config().log);
        this.notify_config();
        this
    }

    /// The controller around config `store`, read from `path` (`config_error`: why it could
    /// not be), nothing logged or told yet: [`Self::new`] does that; a test builds one on a
    /// scratch file.
    fn with_store(store: ConfigStore, path: &Path, config_error: Option<String>) -> Self {
        let style = Arc::new(store.config().strip_style());
        let mut echoes = Echoes::default();
        echoes.set_window(Duration::from_millis(store.config().windows.focus_echo_ms));
        Self {
            style,
            model: None,
            config_mtime: mtime(path),
            store,
            config_error,
            config_checked: Instant::now(),
            settings: NativeSettings::default(),
            displays: Vec::new(),
            strip_visible: true,
            echoes,
            save: SaveClock::default(),
            strip_dirty: false,
            closing: Vec::new(),
            triggers: Triggers::default(),
            press: None,
            drag: None,
            announce: false,
            before: transition_plan::Before::default(),
            pending: None,
            landing_focus: None,
            next_transition: 0,
            parked_note: None,
            under_note: None,
            arrived_note: None,
            activation: ActivationRun::default(),
            fallback_said: HashSet::new(),
            patch_items: PatchItems::default(),
            unplaced: BTreeSet::new(),
            fresh_unplaced: BTreeMap::new(),
            tab_asked: HashMap::new(),
            shown_by_us: HashMap::new(),
            build_wait: None,
            screens_wait: None,
            build_tried: None,
            locked: false,
            lock_checked: None,
            ax_silent_since: None,
            ax_silent_rescan: None,
            settle: Settle::default(),
            screens: BTreeMap::new(),
            phase: phase::Tracker::default(),
            space_changes: 0,
            last_switch: None,
            displays_changed_at: None,
            displays_added: Vec::new(),
            our_switch: None,
            space_checked: Instant::now(),
            foreign: Persisting::default(),
            overview: OverviewWatch::default(),
            pressed_obs: None,
            focus_replay: FocusReplay::default(),
            slide_hold: None,
            ahead: None,
            dock_press: None,
            dock_pid: None,
            dock_bar: None,
            dock_order: dock_order::DockOrderWatch::default(),
            engine: window_engine::current(),
            corner: CornerUpkeep::new(),
            spaces: spaces::SpacesUpkeep::new(),
            minimizing: None,
            unminimizing: HashMap::new(),
            verify: None,
            dock_seen: None,
            lent: None,
            switcher: switcher::SwitcherWatch::default(),
            defer_present: false,
            absent: HashMap::new(),
            busy_apps: std::cell::RefCell::new(BusyApps::default()),
            elsewhere: HashMap::new(),
            off_screen: HashMap::new(),
            seen_active: HashMap::new(),
            seen_stages: None,
            desktop_echo: DesktopEcho::default(),
            strip_hidden_for_desktop: false,
            focus_after_press: None,
            unparked: BTreeMap::new(),
            unparked_apps: BTreeMap::new(),
            stale: stale::StaleHandles::default(),
            user_focus: HashMap::new(),
            active_off_screen: HashMap::new(),
            macos_reveal_at: None,
            wallpaper_press: None,
            quiet: AppQuiet::default(),
            quiet_checked: Instant::now(),
            ax: ax_blind::AxWatch::default(),
            seen_times: HashMap::new(),
            settling: None,
            rematch_until: None,
            absent_due: None,
            front_after_restore: false,
            focus_after_restore: false,
        }
    }

    fn notify_config(&self) {
        let config = self.store.config();
        notify_ui(UiUpdate::Config {
            error: self.config_error.clone(),
            config: Box::new(config.clone()),
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

    /// The displays the model manages, the main one first (round displays): the model's,
    /// else those reported (the main one alone with `[displays] main_only`).
    fn managed_displays(&self) -> Vec<Display> {
        if let Some(m) = &self.model {
            return m.displays().to_vec();
        }
        let mut displays = if self.displays.is_empty() { fallback_displays() } else { self.displays.clone() };
        if self.main_only() {
            displays.truncate(1);
        }
        displays
    }

    /// `[displays] main_only` as the stages were built with it: read at launch (the model
    /// keeps it), the config's own value only until the model is built. A changed value
    /// applies at the next launch; the platform's reads follow the model's, so a reload never
    /// leaves the two disagreeing about which screens are managed (the review's S3).
    fn main_only(&self) -> bool {
        self.model.as_ref().map_or(self.store.config().displays.main_only, |m| m.options().main_only)
    }

    /// The managed display that shows most of `frame` ([`display_of`], macOS's rule for a
    /// window across two screens); the main one when none shows a point of it (off screen,
    /// a park zone's offscreen part).
    fn display_at(&self, frame: Rect) -> DisplayId {
        self.display_of_window(None, frame)
    }

    /// [`Self::display_at`] for window `id` at `frame`: one the model manages that shows on
    /// no display keeps the display it has there ([`display_kept`]), never the main one's.
    fn display_of_window(&self, id: Option<u32>, frame: Rect) -> DisplayId {
        let rects: Vec<(DisplayId, Rect)> = self.managed_displays().iter().map(|d| (d.id, d.frame)).collect();
        let had = id.and_then(|id| self.model.as_ref()?.window(WindowId(id))).map(|w| w.meta.display);
        display_kept(frame, &rects, had).unwrap_or_else(|| self.main_display_id())
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
            display: self.display_of_window(Some(id), w.frame),
        })
    }

    /// Live frames of every shown managed window of the shown Space, the z-order and the
    /// focused window.
    fn observe(&self, ob: &mut Observer) -> Observation {
        self.observe_with(ob, &windows::list_windows())
    }

    /// [`Self::observe`] with the window server's list already read (`list`). A frame whose
    /// Accessibility read fails (a busy app's timeout) is the window server's bounds when
    /// they can be trusted, as for an activation ([`observed_frames`], W3): the tracked frame
    /// of an app that posts no move notice is where the window was before the user moved it.
    fn observe_with(&self, ob: &mut Observer, list: &[windows::CgWindow]) -> Observation {
        let Some(m) = &self.model else { return Observation::default() };
        let z_order: Vec<WindowId> = list
            .iter()
            .filter(|w| w.onscreen && w.layer == 0)
            .map(|w| WindowId(w.id))
            .filter(|id| m.window(*id).is_some())
            .collect();
        // The shown Space of every display (round displays).
        let shown: Vec<(u32, Display, bool)> = m
            .windows()
            .filter(|w| !w.parked && (m.is_shown_space(w.space) || w.sticky))
            // Shown by the model while still parked for ⌘Tab's lend (its switch's frame on its
            // way): where it is is not its frame.
            .filter(|w| !self.engine.parked_lend(w.meta.id))
            .map(|w| (w.meta.id.0, *m.display(w.meta.display), w.sticky))
            .collect();
        let mut reads = Vec::new();
        for (id, display, sticky) in shown {
            if let Some((frame, fresh)) = ob.read_frame(id) {
                reads.push(FrameRead { id: WindowId(id), display, sticky, frame, fresh });
            }
        }
        let listed: HashMap<u32, Rect> =
            list.iter().filter(|w| w.onscreen && w.layer == 0).map(|w| (w.id, w.bounds)).collect();
        let settling = self.phase.space_settling(Instant::now());
        let frames = frames_unless_aside(observed_frames(&reads, &listed, settling), self.overview.moves_windows(Instant::now()));
        ob.note_frames(frames.iter().map(|(id, f)| (id.0, *f)));
        Observation { frames, z_order, focused: focused_window().map(WindowId) }
    }

    /// The observation an activation's switch needs, without asking any app: the z-order
    /// and the shown windows' frames from the window server's list (one read; a window not
    /// listed on screen keeps its tracked frame, kept current by its move and resize
    /// notices), and the focused window only when the switch depends on it
    /// ([`Model::activation_needs_focus`]), asked of the activated app alone with a bounded
    /// timeout (`focused`, asked by the caller). [`Self::observe`] asked every shown window's frame and the system-wide focus
    /// through Accessibility, which for an app busy activating (and the one it took over
    /// from) cost most of the activation's gap (6V). The tracked frame alone is not enough:
    /// an app that posts no move notice would get its windows back where they were before
    /// the user moved them (the switch keeps these frames as their restore frames).
    fn observe_activation(&self, ob: &Observer, list: &[windows::CgWindow], focused: Option<WindowId>) -> Observation {
        let Some(m) = &self.model else { return Observation::default() };
        let z_order: Vec<WindowId> = list
            .iter()
            .filter(|w| w.onscreen && w.layer == 0)
            .map(|w| WindowId(w.id))
            .filter(|id| m.window(*id).is_some())
            .collect();
        // CG bounds are top-left global points, as AX frames are (the observer takes them
        // as a window's frame when AX has none).
        let listed: HashMap<u32, Rect> =
            list.iter().filter(|w| w.onscreen && w.layer == 0).map(|w| (w.id, w.bounds)).collect();
        // The activation often lands before the Space change's notice, while the Desktop
        // slides: the list then shows its windows shifted sideways by up to the display's
        // width. Settling after a change, or off the display, or slid on a display where no
        // window is listed at its tracked frame, the tracked frame stays.
        let settling = self.phase.space_settling(Instant::now());
        let shown: Vec<(&WindowState, Rect, Option<Rect>)> = m
            .windows()
            .filter(|w| !w.parked && (m.is_shown_space(w.space) || w.sticky))
            .filter_map(|w| Some((w, ob.window(w.meta.id.0)?.frame, listed.get(&w.meta.id.0).copied())))
            .collect();
        let steady: HashSet<DisplayId> = shown
            .iter()
            .filter(|(w, tracked, cg)| !w.sticky && cg.is_some_and(|cg| !frames_differ(cg, *tracked)))
            .map(|(w, _, _)| m.display(w.meta.display).id)
            .collect();
        let frames = shown
            .into_iter()
            .map(|(w, tracked, cg)| {
                let display = m.display(w.meta.display);
                let frame = activation_frame(tracked, cg, display.frame, settling, steady.contains(&display.id));
                if frame != tracked {
                    tracing::debug!(id = w.meta.id.0, %tracked, %frame, "activation: the window server's frame differs from the tracked one; it wins");
                } else if let Some(cg) = cg.filter(|cg| frames_differ(*cg, tracked)) {
                    tracing::debug!(id = w.meta.id.0, %tracked, listed = %cg, settling, "activation: the window server's frame looks mid-slide; the tracked one stays");
                }
                (w.meta.id, frame)
            })
            .collect();
        let frames = frames_unless_aside(frames, self.overview.moves_windows(Instant::now()));
        Observation { frames, z_order, focused }
    }

    /// How a batch an activation of app `pid` caused runs ([`ActivationRun`], `[animation]
    /// activation_grows`).
    fn activation_for(&self, pid: i32, actions: &[Action], ob: &Observer) -> ActivationRun {
        let grows = self.store.config().style.animation.activation_grows;
        let noticed = ob.activated_at(pid).filter(|t| t.elapsed() < ACTIVATION_NOTICE_FRESH);
        ActivationRun { noticed, ..activation_run(grows, pid, actions, |id| ob.window(id).map(|w| w.pid)) }
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
                let fitted = m.window(id).filter(|w| !w.parked && w.original.is_some()).map(|w| w.restore);
                match ob.window(id.0) {
                    Some(w) => {
                        let child = w.kind == WindowKind::Child;
                        // A fresh handle found for a dead one this batch, else the tracked one.
                        let element = ob.element(id.0).unwrap_or_else(|| w.element.clone());
                        Some((id.0, ParkedEntry { element, restore, child, fitted }))
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
        for (id, e) in self.kept_entries() {
            entries.entry(id).or_insert(e);
        }
        entries
    }

    /// From the tick: reads the Dock's pid and bar again (Accessibility, the Dock's bounded
    /// timeout) once the last read is [`DOCK_BAR_FRESH`] old, so its size, side, display or
    /// auto-hide changing is caught while nothing else happens; the tap gets it when it
    /// changed. Not while an overview shows (the shield is off then). Read whatever the
    /// config: a press's wallpaper and Dock tests see through the Dock's window by it
    /// ([`Self::dock_bar_now`]).
    fn refresh_dock_bar(&mut self, now: Instant) {
        if self.overview.showing().is_some() {
            return;
        }
        if self.dock_bar.as_ref().is_some_and(|(at, _)| now.duration_since(*at) < DOCK_BAR_FRESH) {
            return;
        }
        let pid = super::dock::dock_pid();
        let bars = pid.and_then(super::dock::bar_frame).into_iter().collect();
        let bar = sliver::DockBar { pid, bars };
        let changed = self.dock_bar.as_ref().is_none_or(|(_, was)| *was != bar);
        self.dock_bar = Some((Instant::now(), bar));
        if changed {
            tracing::debug!(bar = ?self.dock_bar.as_ref().map(|(_, b)| b), "inert slivers: the Dock's bar");
            self.hidden_changed();
        }
    }

    /// The Dock's bar as last read ([`Self::refresh_dock_bar`]; unknown: none), for a
    /// press's hit tests: no Accessibility call on a press.
    pub(super) fn dock_bar_now(&self) -> sliver::DockBar {
        self.dock_bar.as_ref().map(|(_, b)| b.clone()).unwrap_or_default()
    }

    /// Takes a finished read of Finder's desktop icons ([`Self::patch_items`]): the icons it
    /// gave are known from now, a desktop not as expected or too crowded leaves them unknown,
    /// a failed read keeps the last ones; the next read is planned.
    fn take_items_answer(&mut self, answer: Option<crate::engine::corner::corner_patch::DesktopIcons>, now: Instant) {
        use crate::engine::corner::corner_patch::{DesktopIcons, MAX_ICONS, item_reach};
        let p = &mut self.patch_items;
        p.reading = None;
        let said = match answer {
            Some(DesktopIcons::Icons(icons)) => {
                let items: Vec<Rect> = icons.into_iter().map(item_reach).collect();
                for (screen, r) in &p.read_for {
                    let under = items.iter().filter(|i| i.intersect(r).is_some()).count();
                    let was = p.known.as_ref().is_some_and(|k| k.iter().any(|i| i.intersect(r).is_some()));
                    if under > 0 && !was {
                        tracing::info!(screen, under, "corner patch: a desktop icon lies under the corner; only a still of the desktop stands for it");
                    }
                }
                p.known = Some(items);
                p.known_at = Some(now);
                p.fails = 0;
                p.due = Some(sooner(p.due, now + PATCH_ITEMS_FRESH));
                None
            }
            Some(DesktopIcons::TooMany(n)) => {
                p.known = None;
                p.fails = 0;
                p.due = Some(sooner(p.due, now + PATCH_ITEMS_FRESH));
                Some(format!("{n} desktop items, more than the {MAX_ICONS} read; the patch stays off"))
            }
            Some(DesktopIcons::Unexpected(why)) => {
                // Not knowing is off: an older good read must not stand for a desktop
                // Finder now answers oddly (an icon just dropped with no frame yet).
                p.known = None;
                p.fails = p.fails.saturating_add(1);
                p.due = Some(sooner(p.due, now + items_retry(p.fails)));
                Some(format!("Finder's desktop is not as expected ({why}); its icons are unknown"))
            }
            None => {
                if p.fails == 0 {
                    tracing::debug!("corner patch: Finder's desktop icons unreadable; the last read stands, tried again");
                }
                p.fails = p.fails.saturating_add(1);
                p.due = Some(sooner(p.due, now + items_retry(p.fails)));
                p.said.clone()
            }
        };
        if said != p.said {
            if let Some(line) = &said {
                tracing::info!("corner patch: {line}");
            }
            p.said = said;
        }
    }

    /// Finder's desktop icons for a press at `p` with no window under it
    /// ([`desktop_click::desktop_press`]): the last read when it is fresh (the corner patch
    /// reads them every [`PATCH_ITEMS_FRESH`]), else one read now, waited for
    /// [`PRESS_ITEMS_WAIT`] at most (the read already running, or a new one); one that does
    /// not answer in time stays running for the patch, and the last icons known stand, of
    /// any age; none known: the press reveals as before (never guessed on an icon). Logged
    /// when not fresh.
    fn icons_for_press(&mut self, p: Point, now: Instant) -> Option<Vec<Rect>> {
        let fresh = self.patch_items.known_at.is_some_and(|t| now.saturating_duration_since(t) <= PATCH_ITEMS_FRESH + PATCH_ITEMS_RETRY);
        if fresh && self.patch_items.known.is_some() {
            return self.patch_items.known.clone();
        }
        let rx = self.patch_items.reading.take().or_else(super::desktop_items::start_read);
        let answered = match rx {
            Some(rx) => match rx.recv_timeout(PRESS_ITEMS_WAIT) {
                Ok(answer) => {
                    self.take_items_answer(answer, Instant::now());
                    true
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    self.patch_items.reading = Some(rx);
                    false
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.take_items_answer(None, Instant::now());
                    true
                }
            },
            None => false,
        };
        let known = self.patch_items.known.clone();
        let waited_ms = now.elapsed().as_millis() as u64;
        tracing::debug!(x = p.x, y = p.y, answered, waited_ms, icons = known.as_ref().map(Vec::len),
            "a press on the desktop: Finder's desktop icons read for it (none fresh); unknown, the press counts as on the wallpaper");
        known
    }

    /// Keeps the model's stage-area fitting in step with the strip, Apple's settings and
    /// config, and its park request and `[restore] saved_size` with config.
    fn sync_fit(&mut self) {
        let (strip_visible, settings) = (self.strip_visible, self.settings);
        let config = self.store.config();
        if let Some(m) = self.model.as_mut() {
            // The main display's as the model's own, then each display's against its own
            // strip's column (round displays-2).
            let fit = fit_options(m.main_display().visible, strip_visible, &settings, config);
            m.set_fit(fit);
            for d in m.displays().to_vec() {
                m.set_display_fit(d.id, fit_options(d.visible, strip_visible, &settings, config));
            }
            m.set_park_request(ParkRequest { inside: config.windows.park_inside, drop: config.windows.park_drop });
            m.set_saved_size(config.restore.saved_size);
        }
    }

    /// Where a hide of window `id` leaves it now: where the window engine hides it as the
    /// model knows the window ([`WindowEngine::hidden_at`]), or this once a minimized
    /// window's dive frame ([`Self::dive`]). `None` for a window the model does not manage.
    fn hide_frame(&self, id: u32) -> Option<Rect> {
        self.hide_place_once(id).or_else(|| self.model.as_ref().and_then(|m| self.engine.hidden_at(m, WindowId(id))))
    }

    /// Executes actions in order; returns how many AX calls failed. Once Quit is requested
    /// the rest of the batch runs only its `SetFrame`s: unpark-all covers every window the
    /// model counts as parked, whether or not its park was carried out, but a window the
    /// model already counts as shown would otherwise stay parked (the review's S2).
    fn execute(&mut self, ob: &Observer, actions: Vec<Action>) -> usize {
        self.execute_batch(ob, actions, Batch::User)
    }

    fn execute_batch(&mut self, ob: &Observer, actions: Vec<Action>, batch: Batch) -> usize {
        let interruptible = batch.interruptible();
        // An unpark (quit, stand-down) tries a busy app again quickly (6X).
        let show_how = if batch.retries() { ShowHow::Retrying } else { ShowHow::Once };
        let engine = self.engine.clone();
        if actions.is_empty() {
            return 0;
        }
        // Each app's enhanced UI off once for all its windows' moves (round enhanced-ui).
        let _quiet = enhanced_ui::Batch::open();
        self.pressed_obs = None;
        let parked = self.parked_entries(ob);
        safety::registry_add(parked.clone());
        let now = Instant::now();
        let mut failed = 0;
        let mut changed = false;
        let mut interrupted = false;
        let total = actions.len();
        // What the batch raises and focuses, for the order check after it (`raise_order`; an
        // unveil's runs once the strip has its note, [`Self::flush_pending`]).
        let (raises, focus) = raise_order::raises_and_focus(&actions);
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
                Action::SetFrame { id, .. } | Action::Hide { id } | Action::Raise { id } | Action::Focus { id } => id.0,
                Action::StateChanged => {
                    changed = true;
                    continue;
                }
            };
            if matches!(action, Action::SetFrame { .. }) {
                note_shown_by_us(&mut self.shown_by_us, id, now);
            }
            // Where a hide puts the window (asked once: a write tried again asks the same).
            let park = match action {
                Action::Hide { .. } => match self.hide_frame(id) {
                    Some(frame) => Some(frame),
                    None => {
                        tracing::debug!(id, ?action, "action for a window the model let go skipped");
                        continue;
                    }
                },
                _ => None,
            };
            let Some(w) = ob.window(id) else {
                // Lost by the observer (Accessibility revoked, say): a move still goes through
                // the registry's element; a restore that cannot is a failure.
                let kept = matches!(action, Action::SetFrame { .. } | Action::Hide { .. })
                    .then(|| safety::registry_entry(id))
                    .flatten();
                let result = match (action, &kept, park) {
                    (Action::SetFrame { frame, .. }, Some(e), _) => Some(engine.show(WindowId(id), frame, &e.element, show_how)),
                    (Action::Hide { .. }, Some(e), Some(at)) => Some(engine.hide(WindowId(id), at, &e.element)),
                    _ => None,
                };
                match result {
                    Some(Ok(())) => tracing::debug!(id, ?action, "done through the registry's element"),
                    Some(Err(e)) => {
                        failed += 1;
                        tracing::warn!(id, ?action, "action through the registry's element failed: {e}");
                        if let (Action::SetFrame { frame, .. }, Some(kept)) = (action, kept) {
                            self.show_failed(id, ParkedEntry { restore: frame, fitted: None, ..kept });
                        } else if matches!(action, Action::Hide { .. }) {
                            self.hide_failed(id, now);
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
            if matches!(action, Action::Hide { .. }) {
                // A focus before this park is not the user's choice about where it is now.
                self.user_focus.remove(&id);
            }
            if matches!(action, Action::Focus { .. }) {
                self.echoes.touch_app(w.pid, now);
            }
            let pid = w.pid;
            // A dead handle is looked up afresh and the move tried once more (stale-handles).
            let element = ob.element(id).unwrap_or_else(|| w.element.clone());
            let result = self.write_window(ob, id, pid, &element, |e| match action {
                Action::SetFrame { frame, .. } => engine.show(WindowId(id), frame, e, show_how),
                Action::Hide { .. } => match park {
                    Some(at) => engine.hide(WindowId(id), at, e),
                    None => Ok(()),
                },
                Action::Raise { .. } => e.raise(),
                Action::Focus { .. } => ax_focus(e, pid),
                Action::StateChanged => Ok(()),
            });
            if let Err(e) = result {
                failed += 1;
                tracing::warn!(id, ?action, "action failed: {e}");
                // Its app may have quit with no notice (round missed-quit): tested next tick.
                ob.suspect(w.pid);
                if let Action::SetFrame { frame, .. } = action {
                    let child = w.kind == WindowKind::Child;
                    let element = ob.element(id).unwrap_or_else(|| w.element.clone());
                    self.show_failed(id, ParkedEntry { element, restore: frame, child, fitted: None });
                } else if matches!(action, Action::Hide { .. }) {
                    self.hide_failed(id, now);
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
        // Two apps' raises can land in either order: the stage's windows end as asked.
        if !interrupted && batch == Batch::User {
            self.settle_raise_order(ob, &raises, focus, now);
        }
        self.hidden_changed();
        self.publish_minimizable(ob);
        self.look_at_screen();
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
        // Any screen's strip (round displays-2).
        m.displays().iter().any(|d| {
            let was = self.active_off_screen.get(&d.id);
            let Some(active) = m.active_stage(d.id) else { return was.is_some_and(|w| !w.is_empty()) };
            let space = m.current_space(d.id);
            let off: BTreeSet<u32> = m
                .windows()
                .filter(|w| {
                    !w.parked
                        && ((w.home == Home::Pinned && (w.space == space || w.sticky)) || w.home == Home::Stage(active))
                })
                .map(|w| w.meta.id.0)
                .filter(|id| !shown.contains(id))
                .collect();
            was.is_none_or(|w| *w != off)
        })
    }

    /// The registry entries of those of `ids` the model counts as parked (taken before the
    /// model forgets them: [`Self::watch_forgotten`]).
    fn parked_entries_of(&self, ids: &[u32]) -> Vec<(u32, ParkedEntry)> {
        let Some(m) = &self.model else { return Vec::new() };
        ids.iter()
            .filter(|&&id| m.window(WindowId(id)).is_some_and(|w| w.parked))
            .filter_map(|&id| safety::registry_entry(id).map(|e| (id, e)))
            .collect()
    }

    /// Every window the model manages (the safety paths count a sliver of theirs as parked).
    pub fn model_window_ids(&self) -> BTreeSet<u32> {
        self.model.as_ref().map(|m| m.windows().map(|w| w.meta.id.0).collect()).unwrap_or_default()
    }

    /// Every window the model manages and has not parked: vouches for a hidden tab reported
    /// destroyed (8J, [`safety::Ours::shown_managed`]).
    pub fn model_shown_ids(&self) -> BTreeSet<u32> {
        self.model.as_ref().map(|m| m.windows().filter(|w| !w.parked).map(|w| w.meta.id.0).collect()).unwrap_or_default()
    }

    /// What the safety paths know of whose windows are ours ([`safety::Ours`]): the
    /// registry, the model's windows, `state.json`, the registered windows the model let go
    /// as reported destroyed, and the model's windows not parked.
    pub fn safety_ours(&self) -> safety::Ours {
        safety::Ours::now(self.model_window_ids())
            .with_tab_signs(self.let_go_destroyed(), self.model_shown_ids())
            .with_unparked(self.unparked.clone())
            .with_unparked_apps(self.unparked_apps.clone())
    }

    /// Notes which windows are parked, where every window lives and the strip's slots, for
    /// the next transition: after every batch, and after a change of the strip alone (a Dock
    /// re-sort, a config reload), never between a model change and its batch.
    fn remember(&mut self) {
        let Some(m) = &self.model else { return };
        self.before = transition_plan::Before::of(m);
    }

    /// The windows a batch moves on or off the screen, if any, with the card slot each one
    /// leaves into or arrives from (the engine's, [`transition_plan::plan`]).
    fn transition_for(&mut self, ob: &Observer, actions: &[Action]) -> Option<Transition> {
        let m = self.model.as_ref()?;
        let mut t = transition_plan::plan(m, actions, &self.before, |id| ob.window(id).map(|t| t.frame), self.next_transition + 1)?;
        self.next_transition = t.id;
        t.minimized = self.minimizing.filter(|w| t.outgoing.iter().any(|g| g.window.id == w.window));
        for g in &t.outgoing {
            tracing::debug!(id = t.id, window = g.window.id, stage = ?g.stage, to_slot = ?g.slot, from = %g.window.frame, "transition: outgoing");
        }
        for g in &t.incoming {
            tracing::debug!(id = t.id, window = g.window.id, stage = ?g.stage, from_slot = ?g.slot, to = %g.window.frame, "transition: incoming");
        }
        let plays_on = transition_plan::display_of(m, actions);
        tracing::debug!(
            id = t.id,
            display = plays_on.0,
            before = ?transition_plan::order(self.before.slots_on(plays_on)),
            after = ?transition_plan::order(&transition_plan::slots_on(m, plays_on)),
            "transition: strip order"
        );
        Some(t)
    }

    /// Executes a batch the user caused, animated (`[animation]`): the strip first draws
    /// snapshots over the windows that leave and grows snapshots of those that arrive; the
    /// leaving windows park behind theirs, the arriving ones wait until the strip unveils
    /// them. Without the strip's cover the batch runs at once.
    fn execute_animated(&mut self, ob: &Observer, actions: Vec<Action>) -> usize {
        // One batch for the switch (round enhanced-ui), opened before the switch left behind
        // lands: an app in both keeps its enhanced UI off across them.
        let quiet = enhanced_ui::Batch::open();
        if supersedes(&actions) {
            self.flush_superseded(ob, "a new switch");
        } else {
            self.flush_pending(ob);
        }
        self.last_switch = Some(Instant::now());
        if supersedes(&actions) {
            // A switch of ours begins: an activation that fired before it is older than it.
            self.our_switch = self.last_switch;
        }
        let mut run = std::mem::take(&mut self.activation);
        // An activation that grows: can the overlay cover the corner where macOS has raised
        // the activated windows? Decided before the strip is told, from the batch (its
        // arrivals and parks); when not, those windows arrive at once (6V), never shown in
        // the corner for the grow (8L2).
        let mut early_corner: Option<(Option<SwitchCover>, Vec<windows::CgWindow>)> = None;
        let anim = &self.store.config().style.animation;
        if !run.fallback.is_empty() && anim.animates() && self.strip_visible {
            let list = windows::on_screen();
            let arriving: HashSet<u32> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::SetFrame { id, .. } => Some(id.0),
                    _ => None,
                })
                .collect();
            let outgoing: HashSet<u32> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::Hide { id } => Some(id.0),
                    _ => None,
                })
                .collect();
            // The activated app's windows are raised by macOS wherever it put them (8L6).
            let activated: HashSet<u32> = run.fallback.intersection(&arriving).copied().collect();
            let found =
                self.switch_cover(&arriving, &outgoing, &activated, &list);
            match found {
                Ok(c) => early_corner = Some((c, list)),
                Err(r) => {
                    note_fallback(&mut self.fallback_said, r.why);
                    run.arrive_now(r.why);
                    run.at_once_windows = r.windows;
                }
            }
        }
        // With `activation_grows` off (or the corner not coverable), an activated app's
        // windows first, before the strip is told and anything parks: macOS has raised them
        // in the park corner already (6V).
        let first = std::mem::take(&mut run.now);
        let (early, actions): (Vec<Action>, Vec<Action>) =
            actions.into_iter().partition(|a| matches!(a, Action::SetFrame { id, .. } if first.contains(&id.0)));
        let mut failed = 0;
        if !early.is_empty() {
            let started = Instant::now();
            let n = early.len();
            // The early batch notes the model as it is now (the windows leaving already parked
            // in its eyes); the transition is planned against the state before the switch, or
            // the windows leaving get no snapshot (10-13: every Cmd-Tab switch since 6V was
            // instant, the outgoing stage gone with no motion into the strip).
            let before = self.before.clone();
            failed += self.execute_batch(ob, early, Batch::User);
            self.before = before;
            tracing::debug!(windows = n, took_ms = started.elapsed().as_millis() as u64, "the activated app's windows came to their frames first");
            // Windows moved: the list read before is stale.
            early_corner = None;
        }
        let failed = failed + self.execute_animated_rest(ob, actions, &first, run, early_corner);
        // The held arrivals keep the switch's batch open (their own, nested) until they land.
        drop(quiet);
        failed
    }

    /// The frontmost window of the main display's active stage already in place (shown, and
    /// not among `arriving`, whose snapshots grow from their card): a window just opened, an
    /// activated app's moved first. A transition's snapshots pass under it (10-13: a new
    /// System Settings window, the outgoing VS Code snapshot drawn over it, then Settings
    /// popping up as it ended). None (snapshots above every window, as before) when a window
    /// leaving (`outgoing`) sits in front of it (Cmd-M, a window dragged to the strip), or
    /// when the batch ends with an arriving window raised (a merge, a drop).
    fn window_in_place(&self, arriving: &HashSet<u32>, outgoing: &HashSet<u32>, actions: &[Action], list: &[windows::CgWindow]) -> Option<u32> {
        let last_raised = actions.iter().rev().find_map(|a| match a {
            Action::Raise { id } | Action::Focus { id } => Some(id.0),
            _ => None,
        });
        if last_raised.is_some_and(|id| arriving.contains(&id)) {
            return None;
        }
        let m = self.model.as_ref()?;
        // The screen the switch plays on: the arriving windows' (round displays-2).
        let display = arriving.iter().find_map(|id| m.window(WindowId(*id)).map(|w| w.meta.display)).unwrap_or(m.main_display().id);
        let stage = m.stage(m.active_stage(display)?)?;
        let shown: HashSet<u32> = stage
            .windows
            .iter()
            .filter(|id| !arriving.contains(&id.0) && m.window(**id).is_some_and(|w| !w.parked))
            .map(|id| id.0)
            .collect();
        if shown.is_empty() {
            return None;
        }
        let front = list.iter().find(|w| w.layer == 0 && (shown.contains(&w.id) || outgoing.contains(&w.id)))?;
        shown.contains(&front.id).then_some(front.id)
    }

    /// The arriving stage's windows front to back as they end (the model after the batch),
    /// and those of them shown in place already (neither `arriving` nor parked): where each
    /// incoming picture goes in the window order ([`Transition::order`], round sm-landing).
    fn arriving_order(&self, arriving: &HashSet<u32>) -> (Vec<u32>, Vec<u32>) {
        let Some(m) = self.model.as_ref() else { return (Vec::new(), Vec::new()) };
        let display = arriving.iter().find_map(|id| m.window(WindowId(*id)).map(|w| w.meta.display)).unwrap_or(m.main_display().id);
        let Some(stage) = m.active_stage(display).and_then(|s| m.stage(s)) else { return (Vec::new(), Vec::new()) };
        let order = stage.windows.iter().map(|id| id.0).collect();
        let in_place = stage.windows.iter().filter(|id| !arriving.contains(&id.0) && m.window(**id).is_some_and(|w| !w.parked)).map(|id| id.0).collect();
        (order, in_place)
    }

    /// Routes the keyboard to window `key` now, for transition `id`, outside any batch (the
    /// registry is left as it is: the windows of the switch still wait parked, the outgoing
    /// ones not parked yet; a batch would note the model's view of them before they moved):
    /// one bounded step, the window made main and its app frontmost, [`EARLY_FOCUS_BUDGET`]
    /// in all; the raise and the rest of the focus run with the parks. The step's time when
    /// it took.
    fn focus_now(&mut self, ob: &Observer, key: WindowId, id: u64) -> Option<u64> {
        let w = ob.window(key.0)?;
        let started = Instant::now();
        self.echoes.touch_window(key.0, started);
        self.echoes.touch_app(w.pid, started);
        let result = w.element.bounded(EARLY_FOCUS_BUDGET.as_secs_f32(), |e| e.set_main(true)).and_then(|()| {
            let left = EARLY_FOCUS_BUDGET.saturating_sub(started.elapsed());
            if left < EARLY_FOCUS_MIN_CALL {
                return Err(AxError::CANNOT_COMPLETE);
            }
            AxElement::application(w.pid).bounded(left.as_secs_f32(), |a| a.set_frontmost(true))
        });
        let early_focus_ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(()) => tracing::debug!(id, key = key.0, early_focus_ms, "activation: the keyboard goes to the activated window now, before the motion"),
            Err(e) => tracing::debug!(id, key = key.0, early_focus_ms, "activation: the early focus failed ({e}); it goes with the parks"),
        }
        result.ok().map(|()| early_focus_ms)
    }

    /// The animated batch after its early windows (`first`): planned as a transition (shared,
    /// [`Self::transition_for`]), then moved the way the engine says ([`WindowEngine::animate`]).
    /// An activation's focus is `run`'s ([`ActivationRun::focus`]). `early_corner`: the
    /// corner's cover as the activation found it, and the window list it read (none read, or
    /// moved since: both read again).
    fn execute_animated_rest(
        &mut self,
        ob: &Observer,
        actions: Vec<Action>,
        first: &HashSet<u32>,
        run: ActivationRun,
        early_corner: Option<(Option<SwitchCover>, Vec<windows::CgWindow>)>,
    ) -> usize {
        let anim = self.store.config().style.animation.clone();
        if !anim.animates() || !self.strip_visible {
            return self.execute(ob, actions);
        }
        // The windows already in place stay out of the transition: no snapshot grows over them.
        let Some(mut t) = self.transition_for(ob, &actions) else { return self.execute(ob, actions) };
        t.incoming.retain(|w| !first.contains(&w.window.id));
        match self.engine.animate(&t) {
            Motion::Pictures => self.switch_behind_pictures(ob, actions, t, run, early_corner, anim.cover_ms),
        }
    }

    /// A switch played with pictures ([`Motion::Pictures`]), the Corner engine's choreography:
    /// the park corner covered where the arriving windows wait ([`Self::switch_corner`]), the
    /// transition sent to the strip and its pictures waited for (shared,
    /// [`Self::send_transition`]), then the hides behind the outgoing pictures and the
    /// arrivals held hidden until the strip unveils them ([`Self::flush_pending_as`]).
    fn switch_behind_pictures(
        &mut self,
        ob: &Observer,
        actions: Vec<Action>,
        mut t: Transition,
        run: ActivationRun,
        early_corner: Option<(Option<SwitchCover>, Vec<windows::CgWindow>)>,
        cover_ms: u64,
    ) -> usize {
        let id = t.id;
        // A new switch: the last one's windows owe no focus when they land.
        self.landing_focus = None;
        let arriving: HashSet<u32> = t.incoming.iter().map(|w| w.window.id).collect();
        let outgoing: HashSet<u32> = t.outgoing.iter().map(|w| w.window.id).collect();
        let (corner, list) = match early_corner {
            Some((corner, list)) => (corner, list),
            None => {
                let list = windows::on_screen();
                let activated: HashSet<u32> = run.fallback.intersection(&arriving).copied().collect();
                let corner = self.switch_cover(&arriving, &outgoing, &activated, &list);
                if let Err(r) = &corner
                    && !arriving.is_empty()
                {
                    tracing::debug!(id, why = r.why, windows = ?r.windows, "transition: the park corner is not covered");
                }
                (corner.ok().flatten(), list)
            }
        };
        t.under = self.window_in_place(&arriving, &outgoing, &actions, &list);
        (t.order, t.in_place) = self.arriving_order(&arriving);
        t.corner = corner;
        let frames: Vec<Rect> = t.incoming.iter().map(|w| w.window.frame).collect();
        let cover = self.send_transition(ob, t, &actions, &run, cover_ms);
        if cover.outgoing {
            self.parked_note = Some(id);
        }
        if !cover.incoming {
            let failed = self.execute(ob, actions);
            self.note_parked();
            return failed;
        }
        let engine = self.engine.clone();
        let (now, later) = split_covered(actions, &arriving, |w| engine.focus_waits_for_show(w));
        if let Some(Action::Focus { id: key }) = now.iter().rev().find(|a| matches!(a, Action::Focus { .. })) {
            tracing::debug!(id, key = key.0, "the keyboard goes to the new stage now; its windows arrive behind their snapshots");
        } else if let Some(Action::Focus { id: key }) = later.iter().rev().find(|a| matches!(a, Action::Focus { .. })) {
            tracing::debug!(id, key = key.0, "the keyboard goes to the new stage with its windows: hidden off its Desktop, the window's app would take macOS elsewhere");
        }
        let entries = later
            .iter()
            .filter_map(|a| match *a {
                Action::SetFrame { id, frame } => {
                    let w = ob.window(id.0)?;
                    let child = w.kind == WindowKind::Child;
                    Some((id.0, ParkedEntry { element: w.element.clone(), restore: frame, child, fitted: None }))
                }
                _ => None,
            })
            .collect();
        let max = motion_math::unveil_backstop(&self.store.config().style);
        self.pending = Some(Pending { id, actions: later, entries, frames, _quiet: enhanced_ui::Batch::open() });
        self.phase.feed(phase::Event::SwitchSent { id, max }, Instant::now());
        let failed = self.execute(ob, now);
        self.note_parked();
        failed
    }

    /// The part of an animated switch every engine shares: transition `t` sent to the strip,
    /// an activation's keyboard routed at once, and the strip's answer waited for (what it
    /// drew over the windows, [`strip_model::wait_cover`]), with `cover_ms` for the pictures
    /// to reach the screen. No window has moved when this returns.
    fn send_transition(&mut self, ob: &Observer, t: Transition, actions: &[Action], run: &ActivationRun, cover_ms: u64) -> strip_model::Cover {
        let id = t.id;
        let started = Instant::now();
        // An engine that hides in one step: the outgoing windows wait on screen for pictures
        // taken now, none being kept fresh on a timer; the keyboard goes first.
        let waiting = if self.engine.pictures_at_switch() && !t.outgoing.is_empty() {
            Some(t)
        } else {
            self.present_with(Some(Transition { sent: Some(started), ..t }));
            None
        };
        // An activation's keyboard goes at once, while the strip draws: macOS has activated
        // the app and raised its windows in the corner already, and typing right after ⌘Tab
        // must reach the window the switch focuses (8L). One bounded step (8L2); the whole
        // focus runs with the parks as for a card click.
        let early_focus_ms = match run.focus.filter(|key| actions.contains(&Action::Focus { id: *key })) {
            Some(key) => self.focus_now(ob, key, id),
            None => None,
        };
        if let Some(t) = waiting {
            switch_pictures::take(&t);
            self.present_with(Some(Transition { sent: Some(Instant::now()), ..t }));
        }
        let focus_since_activation_ms = early_focus_ms.and(run.noticed).map(|t| t.elapsed().as_millis() as u64);
        let cover = strip_model::wait_cover(id, COVER_TIMEOUT);
        let unveil_latest_ms = (motion_math::unveil_latest(&self.store.config().style) * 1000.0).round() as u64;
        tracing::debug!(
            id,
            ?cover,
            waited_ms = started.elapsed().as_millis() as u64,
            early_focus_ms,
            focus_since_activation_ms,
            unveil_latest_ms,
            at_once = run.at_once,
            at_once_windows = ?run.at_once_windows,
            "transition covered"
        );
        if cover.outgoing || cover.incoming {
            std::thread::sleep(Duration::from_millis(cover_ms));
        }
        cover
    }

    /// Tells the strip a transition's outgoing windows have parked, if no snapshot did yet: its
    /// motion starts then, the outgoing and the incoming snapshots on one frame.
    fn note_parked(&mut self) {
        if self.parked_note.is_some() {
            self.present();
        }
    }

    /// Brings held incoming windows to their frames now (their snapshots cover them, or other
    /// input arrived). Never interrupted: these windows are not parked in the model's eyes.
    fn flush_pending(&mut self, ob: &Observer) {
        self.flush_pending_as(ob, None);
    }

    /// The user acted, or a new switch begins (`why`), while a switch's windows may still be
    /// on their way: they come to their frames, but nothing focuses a window or activates an
    /// app for the switch left behind any more (11-02: two card clicks 0.56 s apart, the
    /// first one's held focus ran after the second one's). The held batch runs
    /// without its focus ([`without_focus`]).
    fn flush_superseded(&mut self, ob: &Observer, why: &'static str) {
        self.flush_pending_as(ob, Some(why));
    }

    fn flush_pending_as(&mut self, ob: &Observer, superseded: Option<&'static str>) {
        if let Some(mut p) = self.pending.take() {
            self.landing_focus = None;
            if let Some(why) = superseded {
                let (kept, dropped) = without_focus(p.actions);
                p.actions = kept;
                if !dropped.is_empty() {
                    tracing::debug!(id = p.id, why, dropped = ?dropped, "a switch superseded before its landing: its windows come, its focus is dropped");
                }
            }
            let now = Instant::now();
            let since = self.phase.switching().map_or(now, |s| s.since);
            self.phase.feed(phase::Event::SwitchDone, now);
            tracing::trace!(id = p.id, held_ms = now.duration_since(since).as_millis() as u64, "incoming windows unveiled");
            let asked: Vec<(u32, Rect)> = p
                .actions
                .iter()
                .filter_map(|a| match *a {
                    Action::SetFrame { id, frame } => Some((id.0, frame)),
                    _ => None,
                })
                .collect();
            let (raises, focus) = raise_order::raises_and_focus(&p.actions);
            // Read before the shows below, which stop a stretch not taken over yet (it ends
            // and tells so all the same).
            self.landing_focus = landing_focus::LandingFocus::after_unveil(p.id, focus, asked.iter().map(|(id, _)| *id), &self.engine.stretching());
            // The key window still flying, on no Desktop, its app with a window elsewhere: a
            // focus now activates the app and macOS goes to that other window (the owner's log
            // of 11-15 06:42:57). The focus after the landing is the switch's only one.
            if let Some(f) = self.landing_focus.as_mut()
                && self.engine.focus_waits_for_show(WindowId(f.key()))
            {
                let key = f.key();
                f.defer();
                p.actions.retain(|a| !matches!(a, Action::Focus { id } if id.0 == key));
                tracing::debug!(id = p.id, key, "landing: the key window flies on no Desktop and its app has a window elsewhere: its focus waits for the landing");
            }
            self.execute_batch(ob, p.actions, Batch::Unveil);
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
            // The strip has its note: now the order its card shows (`raise_order`), unless the
            // user clicked since the switch began (this flush may be the owner's next click's).
            self.settle_raise_order(ob, &raises, focus, since);
        }
    }

    /// The user may have switched apps (a click, a ⌘ key press, an activation not ours): no
    /// late echo is awaited any more ([`Echoes::user_switched`]), and no focus of a switch
    /// left behind runs when its windows land.
    fn user_switched(&mut self) {
        self.echoes.user_switched();
        self.landing_focus = None;
    }

    /// Window `window` of transition `id` ended its last stretch (on its Desktop when
    /// `landed`): once the switch's last flying window has, and one landed, the switch's
    /// focus runs again, now that the window is on a Desktop (round S7b: a focus while it
    /// flies in its landing Space never activates its app), when its stage is still in front.
    fn stretch_ended(&mut self, ob: &Observer, id: u64, window: u32, landed: bool) {
        let Some(f) = self.landing_focus.as_mut() else { return };
        let key = match f.ended(id, window, landed) {
            landing_focus::AfterEnd::Waiting => return,
            landing_focus::AfterEnd::Done => {
                self.landing_focus = None;
                return;
            }
            landing_focus::AfterEnd::Refocus(key) => key,
        };
        self.landing_focus = None;
        let in_front = self.model.as_ref().is_some_and(|m| m.stage_of(WindowId(key)).is_some_and(|s| m.stage_in_front(s)));
        let Some(w) = ob.window(key).filter(|_| in_front) else {
            tracing::debug!(id, key, "landing: the switch's window is no longer in front; no focus again");
            return;
        };
        let now = Instant::now();
        self.echoes.touch_window(key, now);
        self.echoes.touch_app(w.pid, now);
        let element = ob.element(key).unwrap_or_else(|| w.element.clone());
        match ax_focus(&element, w.pid) {
            Ok(()) => tracing::debug!(id, key, pid = w.pid, "landing: the window is on its Desktop; the switch's focus again, so its app comes in front"),
            Err(e) => tracing::debug!(id, key, pid = w.pid, "landing: the focus again failed: {e}"),
        }
    }

    /// After a caught panic: make sure every window the model has parked is in the registry
    /// (entries already there stay, since a batch may have stopped halfway).
    pub fn resync_registry(&self, ob: &Observer) {
        safety::registry_add(self.parked_entries(ob));
    }

    fn mark_dirty(&mut self) {
        self.save.mark(Instant::now());
    }

    // ----- strip -------------------------------------------------------------------------

    /// Builds the strip snapshot and hands it to the strip.
    pub fn present(&mut self) {
        self.present_with(None);
    }

    /// The display a transition plays on: the one its windows are on (the main one when
    /// it names none).
    fn transition_display(&self, transition: Option<&Transition>) -> DisplayId {
        let main = self.main_display_id();
        let Some((t, m)) = transition.zip(self.model.as_ref()) else { return main };
        t.outgoing
            .iter()
            .chain(t.incoming.iter())
            .find_map(|mw| m.window(WindowId(mw.window.id)).map(|w| w.meta.display))
            .unwrap_or(main)
    }

    /// Counts and logs what changed since the last strip snapshot: the active stage of the
    /// Space the strip shows (a switch, whatever caused it, since that Space's last
    /// snapshot) and stages made.
    fn note_changes(&mut self, space: SpaceId) {
        let mut user_switched = None;
        self.note_changes_of(space, &mut user_switched);
        if let (Some(to), Some(m)) = (user_switched, self.model.as_mut()) {
            m.touch_stage(to);
        }
    }

    /// [`Self::note_changes`]; a switch that followed a click or key press goes to `user_switched`.
    fn note_changes_of(&mut self, space: SpaceId, user_switched: &mut Option<StageId>) {
        let Some(m) = &self.model else { return };
        let active = m.active_stage_in(space);
        if let Some(&before) = self.seen_active.get(&space.0)
            && before != active.map(|a| a.0)
            && let Some(to) = active
        {
            counters::bump(Count::Switch);
            let from = before.and_then(|b| m.stage_number(StageId(b)));
            tracing::info!(from, to = m.stage_number(to), stage = to.0, "stage switch");
            if input::since_last_press() < USER_SWITCH {
                // The user's switch (a card or Dock click, ⌘-Tab, a shortcut): its windows
                // are touched; a late title re-matches them no more.
                *user_switched = Some(to);
            }
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

    /// The Space a display's strip shows: the one shown, or (the main display, while a Space
    /// change settles) the Desktop arranged ahead for it (its strip slides in arranged,
    /// [`arrival`]).
    fn strip_space(&self, display: DisplayId) -> Option<SpaceId> {
        let m = self.model.as_ref()?;
        let ahead = self.phase.changing().then(|| self.ahead_space()).flatten().filter(|s| m.display_of_space(*s) == m.known_display(display));
        Some(ahead.unwrap_or_else(|| m.current_space(display)))
    }

    /// One snapshot for every display's strip (round displays): the transition, if any, on
    /// the display its windows are on; the notes of a switch under way (parked, arrived) to
    /// every strip, each acting on the transition it holds.
    fn present_with(&mut self, transition: Option<Transition>) {
        self.strip_dirty = false;
        let plays_on = self.transition_display(transition.as_ref());
        let displays: Vec<DisplayId> = self.model.as_ref().map(|m| m.displays().iter().map(|d| d.id).collect()).unwrap_or_default();
        let notes = PresentNotes {
            announce: std::mem::take(&mut self.announce),
            parked: self.parked_note.take(),
            arrived: self.arrived_note.take(),
            under_now: self.under_note.take(),
        };
        for d in displays {
            let t = if d == plays_on { transition.clone() } else { None };
            self.present_for(d, t, &notes);
        }
    }

    /// The snapshot of one display's strip ([`Self::present_with`]).
    fn present_for(&mut self, display: DisplayId, transition: Option<Transition>, notes: &PresentNotes) {
        let Some(space) = self.strip_space(display) else { return };
        self.note_changes(space);
        let Some(m) = &self.model else { return };
        let main = display == m.main_display().id;
        let current = space == m.current_space(display);
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
        self.active_off_screen.insert(display, off.into_iter().map(|w| w.meta.id.0).collect());
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
        // Pinned windows stay shown through a switch: snapshots of them kept ready let the
        // switch's cover draw one reaching the park corner (8L5).
        let pinned_windows = m
            .windows()
            .filter(|w| !w.parked && w.home == Home::Pinned && (w.space == space || w.sticky) && on.contains(&w.meta.id.0))
            .map(|w| CardWindow { id: w.meta.id.0, pid: w.meta.pid, frame: w.meta.frame })
            .collect();
        strip_model::present(StripSnapshot {
            cards,
            active_stage: active.map(|s| s.0),
            hide_recent_apps: m.strip_auto_hides(),
            user_visible: self.strip_visible && !(current && self.strip_hidden_for_desktop && m.desktop_revealed()),
            style: self.style.clone(),
            active_frames,
            window_drag: dragged.map(|d| CardWindow { id: d.id, pid: d.pid, frame: d.pre }),
            window_drag_grab: dragged.map(|d| d.grab),
            announce: main && notes.announce,
            active_windows: kept_ready(self.strip_visible, active_windows),
            pictures_at_switch: self.engine.pictures_at_switch(),
            pinned_windows: kept_ready(self.strip_visible, pinned_windows),
            transition,
            parked: notes.parked,
            arrived: notes.arrived.clone(),
            under_now: notes.under_now,
            late: None,
            space: space.0,
            display: display.0,
            fullscreen: main && current && self.phase.fullscreen(),
            away: false,
            quiet: m.settling(),
        });
    }

    /// App `pid` quit at `now`: the saved entries of its windows destroyed within
    /// [`CLOSING_KEEP`] are kept for `state.json` (round 8H, [`Model::keep_quit`]), so a logout
    /// whose early signal never came still leaves their stages for the next login.
    fn keep_quit_windows(&mut self, pid: i32, now: Instant) {
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.closing)
            .into_iter()
            .filter(|(t, _)| now.duration_since(*t) <= CLOSING_KEEP)
            .partition(|(_, c)| c.pid == pid);
        self.closing = rest;
        if mine.is_empty() {
            return;
        }
        let Some(m) = self.model.as_mut() else { return };
        let n = mine.len();
        let stages: BTreeSet<StageId> = mine.iter().map(|(_, c)| c.stage).collect();
        m.keep_quit(mine.into_iter().map(|(_, c)| c).collect(), unix_now());
        tracing::info!(pid, windows = n, stages = stages.len(), "an app quit: its windows' saved entries stay in state.json for two minutes (a login after a restart puts them back)");
        self.mark_dirty();
    }

    // ----- persistence -------------------------------------------------------------------

    /// Writes `state.json` now (atomically and durably, [`super::durable`], readable by the user
    /// only in a private folder, [`crate::private_files`]). A state that cannot be encoded (a
    /// frame that is not finite) is not written: the file on disk stays. The state is clean
    /// only once the file is replaced; a failure is tried again later ([`SaveClock::failed`]).
    pub fn save_now(&mut self) {
        let start = Instant::now();
        let Some(m) = self.model.as_mut() else {
            self.save.saved();
            return;
        };
        let path = paths::state_file();
        let now = unix_now();
        // Windows of apps that quit too long ago are not reopened by a login.
        m.prune_quit(now);
        let mut file = m.to_state();
        // The window ids in it hold for this window server session only.
        file.session = session::window_session().map(str::to_string);
        file.stamp(now, &mut self.seen_times);
        if !self.ax_looked() {
            // Accessibility listed no window: nothing was looked for in this session.
            file.not_looked_for();
        }
        let text = match file.to_json() {
            Ok(text) => text,
            Err(e) => {
                let wait = self.save.failed(Instant::now());
                tracing::error!(failures = self.save.failures(), retry_s = wait.as_secs(), "{e}; the saved stages on disk are kept");
                return;
            }
        };
        let encoded = start.elapsed();
        let result = crate::private_files::ensure_dir(&paths::config_dir())
            .and_then(|_| super::durable::replace(&path, text.as_bytes(), super::durable::Flush::Full));
        match result {
            Ok(()) => {
                if self.save.failures() > 0 {
                    tracing::info!(failures = self.save.failures(), "state saved to {} again", path.display());
                }
                self.save.saved();
                // How long the save held the window thread (1.9: off the thread if over ~10 ms).
                let ms = |d: Duration| (d.as_secs_f64() * 10_000.0).round() / 10.0;
                let took = start.elapsed();
                tracing::debug!(ms = ms(took), encode_ms = ms(encoded), write_ms = ms(took - encoded), bytes = text.len(), "state saved to {}", path.display());
            }
            Err(e) => {
                let wait = self.save.failed(Instant::now());
                tracing::warn!(failures = self.save.failures(), retry_s = wait.as_secs(), "cannot save {}: {e}; tried again later", path.display());
            }
        }
    }

    /// Reads `state.json`. Written in another window server session (a reboot or a new login
    /// since: the ids name other windows now), its window ids are dropped, every saved window
    /// is pending until it shows, and the ones pending through a whole session already are
    /// given up ([`StateFile::start_new_session`]); the restore says which (the restore
    /// window opens). A session that cannot be read now counts as the file's own for the
    /// restore (no restore window at every relaunch), while its window ids are still dropped.
    /// `restore_after_restart` off (`[stages]`): after a restart nothing waits (1.2.0). The
    /// restore knows the main display's size when saved (saved frames only on that size).
    fn load_state(restore_after_restart: bool) -> Option<(StateFile, Restore)> {
        let current = session::window_session();
        match read_state_or_set_aside(&paths::state_file()) {
            Some(mut s) => {
                let restoring = current.is_some() && !s.same_session(current);
                // Windows of apps that quit just before the save (a logout whose early signal
                // never came) wait in their stages after a restart; otherwise they are gone.
                let (kept, dropped) = s.settle_quit(restoring && restore_after_restart);
                if kept + dropped > 0 {
                    tracing::info!(kept, dropped, "restore: saved windows of apps that quit before the save: kept when they quit within two minutes of it, after a restart");
                }
                if restoring && !restore_after_restart {
                    tracing::info!("restore: state.json is from another window server session; [stages] restore_after_restart is off, so saved windows not on screen are forgotten");
                } else if restoring {
                    let expired = s.start_new_session();
                    tracing::info!(
                        pending = s.pending_windows(),
                        expired,
                        "restore: state.json is from another window server session (a restart or a new login); its windows go back to their stages as they show"
                    );
                }
                let (s, dropped) = s.for_session(current);
                if dropped > 0 {
                    tracing::info!(dropped, "state.json was written in another window server session (a reboot or a login since): its windows are matched by app, title and frame");
                }
                let now = current.unwrap_or_default();
                let restore = match (restoring, restore_after_restart) {
                    (true, true) => Restore::after_restart(now),
                    (true, false) => Restore::off_after_restart(now),
                    (false, _) => Restore::same_session(now),
                };
                let restore = restore.with_display(s.display.as_ref().map(|d| (d.w, d.h)));
                Some((s, restore))
            }
            None => None,
        }
    }

    // ----- events ------------------------------------------------------------------------

    /// Handles a batch of observer events. Runs of `Found` are taken in window-id order
    /// (creation order), each app's windows front to back ([`found_order`], [`Self::found_z`]).
    pub fn on_events(&mut self, ob: &mut Observer, events: Vec<WinEvent>) {
        let mut i = 0;
        while i < events.len() {
            if let WinEvent::Found(_) = events[i] {
                let mut run: Vec<u32> = Vec::new();
                while let Some(WinEvent::Found(id)) = events.get(i) {
                    run.push(*id);
                    i += 1;
                }
                let z = if run.len() > 1 { self.found_z(ob, &run) } else { HashMap::new() };
                let run = found_order(run, |id| ob.window(id).map(|w| w.pid), |id| z.get(&id).copied());
                for id in run {
                    self.on_found(ob, id);
                }
                continue;
            }
            self.on_event(ob, events[i]);
            i += 1;
        }
    }

    /// The front-to-back order of a run of found windows ([`found_order`]): the window
    /// server's on-screen order, and for windows the engine holds on their way in (off
    /// screen: the Spaces engine, a just-opened app's) their app's own order, as its
    /// `AXWindows` lists them, after every on-screen one ([`held_ranks`]). The window
    /// server's list of a Space never shown does not keep the app's order (live 11-14: an app
    /// opening two windows at once listed the one it opened first in front, 2 runs of 3).
    fn found_z(&self, ob: &Observer, run: &[u32]) -> HashMap<u32, usize> {
        let mut z = windows::z_order(&windows::list_windows());
        let held: Vec<(u32, i32)> =
            run.iter().filter(|&&id| self.engine.about_to_show(WindowId(id))).filter_map(|&id| ob.window(id).map(|w| (id, w.pid))).collect();
        let pids: BTreeSet<i32> = held.iter().map(|h| h.1).collect();
        for pid in pids {
            let mine: Vec<u32> = held.iter().filter(|h| h.1 == pid).map(|h| h.0).collect();
            if mine.len() < 2 {
                continue;
            }
            let listed: Vec<u32> = AxElement::application(pid)
                .bounded(HELD_ORDER_BUDGET, |app| app.windows())
                .unwrap_or_default()
                .iter()
                .filter_map(|w| w.window_id_within(HELD_ORDER_BUDGET).ok())
                .collect();
            let base = z.len();
            for (id, rank) in held_ranks(&mine, &listed) {
                z.insert(id, base + rank);
            }
        }
        z
    }

    fn on_event(&mut self, ob: &mut Observer, event: WinEvent) {
        let now = Instant::now();
        // Something changed since a card was pressed: the click reads the windows afresh.
        self.pressed_obs = None;
        // macOS raises an activated app's windows, parked ones too, above the corner patch:
        // the patch is ordered again now, not at the next 250 ms tick (L2).
        if matches!(event, WinEvent::Focused(_) | WinEvent::AppActivated(..)) {
            self.raised_by_macos(now);
        }
        // Any other event brings windows held behind a transition's snapshots in at once;
        // moves, retitles and the echoes of our own focus (sent before the windows arrive,
        // so the keyboard goes to the new stage at once) leave the animation alone.
        if !matches!(
            event,
            WinEvent::Moved(_)
                | WinEvent::Retitled(_)
                | WinEvent::Focused(_)
                | WinEvent::AppActivated(..)
                | WinEvent::AppTookActivation(_)
                | WinEvent::AppHidden(_)
                | WinEvent::AppUnhidden(_)
                | WinEvent::AppTerminated(_)
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
                self.user_focus.remove(&id);
                // A tab no longer selected may be reported destroyed while its app keeps it
                // (8J): when the tab selected shows in its place already, it takes that place in
                // the stage, as at the Space check (round tab-lost).
                let shown_tab = self.model.as_ref().is_some_and(|m| {
                    m.window(WindowId(id)).is_some_and(|w| w.meta.role == WindowRole::Standard && m.is_shown_space(w.space))
                });
                if shown_tab
                    && self.take_tabs(ob, &windows::list_windows())
                    && self.model.as_ref().is_some_and(|m| m.window(WindowId(id)).is_none())
                {
                    tracing::debug!(id, "a window reported destroyed while the window server keeps it, a tab selected in its place: a hidden tab");
                    return;
                }
                let kept = self.parked_entries_of(&[id]);
                // Its saved entry as it is now, should its app be quitting (round 8H).
                if let Some(c) = self.model.as_ref().and_then(|m| m.closing(WindowId(id))) {
                    self.closing.retain(|(t, _)| now.duration_since(*t) <= CLOSING_KEEP);
                    self.closing.push((now, c));
                }
                if let Some(m) = self.model.as_mut() {
                    let actions = m.remove_window(WindowId(id));
                    self.execute(ob, actions);
                }
                // Destroyed while the window server still lists it (`watch_forgotten` keeps
                // only those): the tab's signature, the one let-go a hidden tab may have.
                self.let_go_hidden(ob, kept, LetGo::Destroyed);
            }
            WinEvent::Moved(id) => {
                self.maybe_start_drag(ob, id, now);
                self.publish_minimizable(ob);
                let frame = ob.window(id).map(|w| w.frame);
                let ours = self.echoes.is_window_echo(id, now);
                if !ours
                    && input::left_button_down()
                    && let Some(m) = self.model.as_mut()
                {
                    // Moved or resized by the user: a late title re-matches it no more.
                    m.touch_window(WindowId(id));
                }
                if !ours && input::left_button_down() && self.model.as_ref().and_then(|m| m.window(WindowId(id))).is_some_and(|w| w.parked) {
                    // Dragged while parked (its sliver grabbed without a focus, ⌘ held): the
                    // user's; the tick adopts it where they leave it rather than parking it
                    // again under them ([`Self::reconcile_parked`]).
                    self.hidden_moved_by_user(id);
                }
                // Show Desktop pushing the windows aside (or sliding them back) is macOS's move,
                // never the window's own frame (11-09).
                let aside = !ours && !input::left_button_down() && self.overview.moves_windows(now);
                if aside {
                    tracing::trace!(id, "a move while Show Desktop has the windows aside: not the window's frame");
                }
                // Parked for ⌘Tab's lend (round cmdtab-park), or its frame not set again yet:
                // the park's and the restore's moves are ours, never the window's frame.
                let aside = aside || self.engine.parked_lend(WindowId(id));
                if let (Some(m), Some(frame)) = (self.model.as_mut(), frame.filter(|_| !aside))
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
                    // A window titled late (an Electron app's) may be a saved one after all.
                    let actions = m.window_retitled(WindowId(id));
                    // VoiceOver reads a card by its front window's title (as Apple's): a card's
                    // front window retitled is presented again while an assistive app listens.
                    let fronts_card = assistive::on() && fronts_a_card(m, WindowId(id));
                    self.mark_dirty();
                    if !actions.is_empty() {
                        self.execute(ob, actions);
                        self.present();
                    } else if fronts_card {
                        self.present();
                    }
                }
            }
            WinEvent::Focused(id) => {
                if self.unplaced.contains(&id) {
                    self.offer_waiting(ob, vec![id]);
                }
                if self.echoes.is_window_echo(id, now) && !self.focus_beats_echo(ob, id, now) {
                    tracing::debug!(id, "focus echo of our own action ignored");
                    return;
                }
                if input::since_last_press() < USER_FOCUS
                    && let Some(m) = self.model.as_mut()
                {
                    // Focused by a click or a key press on it: a late title re-matches it no
                    // more (a focus an app makes by itself at login leaves it).
                    m.touch_window(WindowId(id));
                }
                if let Some(pid) = ob.window(id).map(|w| w.pid) {
                    let (hidden, active) = (super::apps::is_hidden(pid), super::apps::is_active(pid));
                    if !focus_may_switch(hidden, active, input::left_button_down()) {
                        tracing::debug!(id, pid, ?hidden, ?active, "a focus in an app not in front (hidden, or not the active app): its own, or a late echo of ours; no switch");
                        return;
                    }
                }
                if self.macos_after_display_change() {
                    tracing::debug!(id, "a focus macOS made after a display change (no click or key since): no switch");
                    return;
                }
                // Not our echo: the user's (or an app's own), remembered for the reconciler.
                self.user_focus.insert(id, now);
                let bundle = ob.window(id).and_then(|w| ob.bundle_id(w.pid));
                if self.desktop_echo.classify(bundle, now) == Cause::WallpaperEcho {
                    tracing::debug!(id, "Finder window focus after a wallpaper click: ignored (the desktop is Finder's)");
                    return;
                }
                if self.model.as_ref().is_some_and(Model::settling) {
                    if input::left_button_down() {
                        // A click whose press the input monitor has not told us about yet.
                        self.end_restore(ob, "a click", End::User);
                    } else {
                        // At login a focus is the app's (a click or key press would have
                        // ended the settling): no switch, nothing held for later.
                        tracing::debug!(id, "a focus while the login settles: no switch");
                        return;
                    }
                }
                self.flush_superseded(ob, "a focus");
                self.show_strip_after_desktop("a window was focused");
                let pid = ob.window(id).map(|w| w.pid);
                let unknown = self.model.as_ref().is_some_and(|m| m.window(WindowId(id)).is_none());
                let elsewhere = self.model.as_ref().is_some_and(|m| m.waits_on_another_space(WindowId(id)));
                // Its own screen's answer: a full-screen app on the other screen stops nothing here.
                let frozen = self.frozen_for_window(id);
                if let Some(pid) = pid
                    && (frozen || elsewhere || (unknown && self.unplaced.contains(&id)))
                {
                    // Its Desktop is not recognized yet (a Dock click or ⌘Tab to a window of
                    // another Desktop, often before the Space change notice): replayed once
                    // it is, never dropped.
                    let predicted = self.predicted_space(id);
                    tracing::debug!(id, pid, settling = self.phase.changing(), elsewhere, unknown, predicted, "focus held until its Desktop is recognized");
                    self.focus_replay.hold(Wanted::Window { id: WindowId(id), pid }, now);
                    self.after_hold(ob, now);
                    return;
                }
                if frozen {
                    return;
                }
                self.forget_held(ob);
                if input::left_button_down() {
                    // Clicked or grabbed by the user: a move made ahead for it is no longer
                    // sent again under them (R11 3). A focus alone may be macOS's own at an
                    // arrival, and keeps it.
                    self.user_took(id, "clicked");
                }
                if self.model.as_ref().and_then(|m| m.window(WindowId(id))).is_some_and(|w| w.parked)
                    && input::left_button_down()
                {
                    // Grabbed by its sliver: nothing moves under the user's drag; its stage
                    // comes once the button is up (Apple's parked windows cannot be grabbed).
                    tracing::debug!(id, "a parked window was grabbed; its stage comes when the button is up");
                    self.focus_after_press = Some(id);
                    return;
                }
                let list = windows::list_windows();
                if let Some(pid) = pid
                    && self.window_elsewhere(id)
                    && self.model.as_ref().is_some_and(|m| slide_hold::slide_listed(m, &listed_bounds(&list)))
                {
                    // A Desktop slides before its notice: a stage switch here would raise and
                    // focus a window of the Desktop being left, and macOS would slide back.
                    tracing::info!(id, pid, "focus held: a Desktop slides; decided once the Space is recognized");
                    self.hold_for_slide(ob, Wanted::Window { id: WindowId(id), pid }, false, now);
                    return;
                }
                if let Some(pid) = pid
                    && self.window_elsewhere(id)
                    && !input::left_button_down()
                {
                    // A focus that would switch stages, still true? Its notice carries no
                    // stamp, so its app is asked (bounded) where its focus is now: a late
                    // focus of a window its app has left since never switches (round
                    // stale-activations).
                    let now_focused = arrival::focused_window_of(pid);
                    let stages = self.model.as_ref().map(|m| {
                        let stage = |w: WindowId| m.resolve_standard(w).and_then(|s| m.stage_of(s));
                        (stage(WindowId(id)), now_focused.and_then(stage))
                    });
                    if let Some((was, is)) = stages
                        && stale_activation::focus_moved_on(was, is)
                    {
                        tracing::info!(id, pid, now_focused = ?now_focused.map(|w| w.0), reason = "its app's focus is in another stage now", "focus dropped: stale, no switch");
                        return;
                    }
                }
                let obs = self.observe_with(ob, &list);
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
                    if let Some(pid) = pid {
                        self.activation = self.activation_for(pid, &actions, ob);
                    }
                    self.execute_animated(ob, actions);
                }
            }
            WinEvent::Minimized(id) => self.on_minimized(ob, id),
            WinEvent::Deminiaturized(id) => self.on_deminiaturized(ob, id),
            WinEvent::AppHidden(pid) => self.on_app_hidden(ob, pid),
            WinEvent::AppUnhidden(pid) => self.on_app_unhidden(ob, pid),
            // Logged by the observer; a running switch's windows came first (above).
            WinEvent::AppTookActivation(_) => {}
            WinEvent::AppTerminated(pid) => {
                if let Some(m) = self.model.as_mut() {
                    m.forget_hidden_app(pid);
                }
                // Its icon leaves the Dock unless pinned: the strip may follow.
                self.dock_order.soon(now);
                let left: BTreeSet<StageId> = self
                    .closing
                    .iter()
                    .filter(|(t, c)| c.pid == pid && now.duration_since(*t) <= CLOSING_KEEP)
                    .map(|(_, c)| c.stage)
                    .collect();
                self.keep_quit_windows(pid, now);
                enhanced_ui::gone(pid);
                self.front_after_quit(ob, &left);
            }
            WinEvent::AppActivated(pid, fired) => {
                crate::platform::switcher_probe::note("activated", format!("pid={pid} notice_age_ms={}", fired.elapsed().as_millis()));
                // A window put home for ⌘Tab's switcher is kept only for its app's activation.
                self.switcher_activated(pid);
                // Its enhanced UI is read again at its next move (round enhanced-ui).
                enhanced_ui::forget(pid);
                // Before the echo check: a window of the app that shows now is placed whoever
                // activated it. One still away is left alone ([`offered_at_notice`]).
                let waiting: Vec<u32> =
                    self.unplaced.iter().copied().filter(|id| ob.window(*id).is_some_and(|w| w.pid == pid)).collect();
                self.offer_waiting(ob, waiting);
                if self.echoes.is_app_echo(pid, now) {
                    tracing::debug!(pid, "activation echo of our own action ignored");
                    return;
                }
                if self.desktop_echo.classify(ob.bundle_id(pid), now) == Cause::WallpaperEcho {
                    tracing::debug!(pid, "Finder activation after a wallpaper click: ignored (the desktop is Finder's)");
                    return;
                }
                // Late under load: no longer true, or older than a switch we began since
                // (round stale-activations). Not the owner's: nothing below runs for it.
                let front = super::apps::frontmost_pid();
                if let Some(why) = stale_activation::judge(pid, fired, self.our_switch, front) {
                    let age_ms = fired.elapsed().as_millis() as u64;
                    let before_switch_ms = match why {
                        stale_activation::Stale::BeforeOurSwitch { by } => Some(by.as_millis() as u64),
                        stale_activation::Stale::NotFrontmost { .. } => None,
                    };
                    tracing::info!(pid, age_ms, reason = why.reason(), ?front, before_switch_ms, "activation dropped: stale, no switch");
                    return;
                }
                if self.macos_after_display_change() {
                    // The lid closed or opened: macOS activates an app of its choosing (11-05
                    // 20:25:46.8, Mail, whose switch sent the laptop's front to a card and
                    // slid the 5K back to the Desktop it had left).
                    tracing::info!(pid, "an activation macOS made after a display change (no click or key since): no switch");
                    return;
                }
                if self.model.as_ref().is_some_and(Model::settling) {
                    if input::left_button_down() {
                        // A Dock click whose press the input monitor has not told us about yet.
                        self.end_restore(ob, "a click (or a Dock click)", End::User);
                    }
                    if self.model.as_ref().is_some_and(Model::settling) {
                        // A launch-time activation at login is the app's, not the user's (or a
                        // click the restore window outlives while Accessibility lists no
                        // window): no switch, nothing held for later.
                        tracing::debug!(pid, "an activation while the login settles: no switch");
                        return;
                    }
                }
                // The user switched apps: a late echo of an earlier focus of ours is no
                // longer one.
                self.user_switched();
                self.flush_superseded(ob, "an activation");
                self.show_strip_after_desktop("an app was activated");
                if self.activation_frozen(pid) {
                    tracing::debug!(pid, "activation held until the Desktop is recognized");
                    self.focus_replay.hold(Wanted::App(pid), now);
                    self.after_hold(ob, now);
                    return;
                }
                self.decide_activation(ob, pid, now, true);
            }
        }
    }

    /// App `pid`'s activation decided (`now`: when it came): its stage on the Desktop shown,
    /// Shift's bring-here, or held. With `watch_slide`, an activation that would switch
    /// stages first looks for a Desktop slide under way ([`slide_hold::activation_waits`]:
    /// the window list it reads for the switch anyway, and, only for an app with a window on
    /// a Desktop not shown, the app's focused window) and is held for the Space change
    /// instead ([`Self::hold_for_slide`]); one held so is decided again without it.
    fn decide_activation(&mut self, ob: &mut Observer, pid: i32, now: Instant, watch_slide: bool) {
        let list = windows::list_windows();
        let present = self.app_present(pid);
        let needs_focus = self.model.as_ref().is_none_or(|m| m.activation_needs_focus(pid));
        let watch = watch_slide && !present;
        let asks = watch && self.model.as_ref().is_some_and(|m| slide_hold::asks_focus(m, pid));
        let focused = if needs_focus || asks { arrival::focused_window_of(pid) } else { None };
        let wait = if watch {
            self.model.as_ref().and_then(|m| slide_hold::activation_waits(m, slide_hold::slide_listed(m, &listed_bounds(&list)), focused))
        } else {
            None
        };
        let wait = wait.filter(|why| !self.activation_stays_here(pid, *why));
        if let Some(why) = wait {
            let wanted = match why {
                slide_hold::Wait::FocusElsewhere(id) => Wanted::Window { id, pid },
                slide_hold::Wait::Slides => Wanted::App(pid),
            };
            let decide_ms = now.elapsed().as_millis() as u64;
            tracing::info!(
                pid,
                why = why.name(),
                ax_focused = ?focused.map(|f| f.0),
                decide_ms,
                "activation held: it belongs to the Desktop sliding in; decided once the Space is recognized"
            );
            self.hold_for_slide(ob, wanted, true, now);
            if let slide_hold::Wait::FocusElsewhere(id) = why {
                // A window the engine hides off its Desktop: macOS goes nowhere for it, so the
                // arrival is made ours (the Spaces engine; never the Corner's).
                self.arrange_for_hidden_focus(ob, id, pid, now);
            }
            return;
        }
        let list = if self.adopt_stranded_before_switch(ob, "an activation") { windows::list_windows() } else { list };
        let obs = self.observe_activation(ob, &list, focused.filter(|_| needs_focus));
        // The window server's frames the switch keeps are tracked too: its outgoing picture
        // starts where the windows are (an app may move one without a notice).
        ob.note_frames(obs.frames.iter().map(|(id, f)| (id.0, *f)));
        if self.bring_here_if_shift(ob, pid, &obs, now) {
            self.forget_held(ob);
            return;
        }
        let since = ob.activated_at(pid);
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
                    let since_activation_ms = since.map(|t| t.elapsed().as_millis() as u64);
                    let decide_ms = now.elapsed().as_millis() as u64;
                    tracing::info!(pid, since_activation_ms, decide_ms, asked_focus = needs_focus, "activated app lives in another stage; switching");
                }
                self.forget_held(ob);
            }
            self.activation = self.activation_for(pid, &actions, ob);
            self.execute_animated(ob, actions);
        }
    }

    /// True when an activation held for its focused window on another Desktop (`why`) stays
    /// on the Desktop shown after all: that window waits off every Desktop (the Spaces
    /// engine's stage Space), or a window of the app was put home at a Dock press, and the
    /// app has a stage here with nothing macOS counts elsewhere, or that window put home
    /// ([`crate::engine::model::Model::activation_stays_here`]). macOS goes nowhere then; the
    /// app's stage here comes in front, as under the Corner engine and Apple's (the owner's
    /// log of 11-15 06:31:28: VS Code, a stage here, its focused window hidden on Desktop 4,
    /// was taken to Desktop 4 by the arrival made for that window). Never under the Corner
    /// engine.
    fn activation_stays_here(&self, pid: i32, why: slide_hold::Wait) -> bool {
        let slide_hold::Wait::FocusElsewhere(id) = why else { return false };
        let Some(m) = self.model.as_ref() else { return false };
        let lent = self.lent_for(pid);
        let off = m.resolve_standard(id).is_some_and(|s| self.engine.off_every_desktop(s));
        let stays = (lent || off) && m.activation_stays_here(pid, lent);
        if stays {
            tracing::info!(pid, focused = id.0, lent, "activation: its focused window waits off every Desktop elsewhere and its stage is here: macOS stays, the stage here comes in front");
        }
        stays
    }

    /// Holds `wanted` (an activation, else a window focus) for a Desktop slide: the focus
    /// replay keeps it for the Space change's arrival ([`Self::bring_focus_forward`]); with
    /// no change, [`Self::slide_hold_step`] decides it after the hold.
    fn hold_for_slide(&mut self, ob: &Observer, wanted: Wanted, activation: bool, now: Instant) {
        self.focus_replay.hold(wanted, now);
        self.slide_hold = Some(SlideHold { wanted, activation, came: now, changes: self.space_changes });
        self.after_hold(ob, now);
    }

    /// True while a slide hold waits ([`Self::slide_hold_step`] on the fast tick).
    pub(super) fn slide_held(&self) -> bool {
        self.slide_hold.is_some()
    }

    /// A slide hold looked at (the fast tick, and every tick): a Space change notified since
    /// takes it over (its arrival replays what is held); with none, once the hold is over
    /// ([`SlideHold::step`]) and the user did nothing meanwhile (it is still what the focus
    /// replay holds), it is decided as it would have been: an activation as any activation,
    /// a focus as a held focus whose wait ended.
    pub(super) fn slide_hold_step(&mut self, ob: &mut Observer) {
        let Some(hold) = self.slide_hold else { return };
        let now = Instant::now();
        let step = hold.step(now, self.space_changes, || {
            self.model.as_ref().is_some_and(|m| slide_hold::slide_listed(m, &listed_bounds(&windows::on_screen())))
        });
        match step {
            slide_hold::Step::Wait => return,
            slide_hold::Step::Arrived | slide_hold::Step::Decide => self.slide_hold = None,
        }
        let frozen = match hold.wanted {
            Wanted::Window { id, .. } => self.frozen_for_window(id.0),
            Wanted::App(pid) => self.activation_frozen(pid),
        };
        if step == slide_hold::Step::Arrived
            || frozen
            || self.focus_replay.peek(now).map(Wanted::pid) != Some(hold.wanted.pid())
        {
            // A Space change came (its arrival replays what is held), or the user acted
            // meanwhile.
            return;
        }
        let held_ms = now.saturating_duration_since(hold.came).as_millis() as u64;
        tracing::info!(pid = hold.wanted.pid(), held_ms, activation = hold.activation, "slide hold over with no Space change: decided now");
        if hold.activation {
            self.focus_replay.clear();
            self.decide_activation(ob, hold.wanted.pid(), hold.came, false);
        } else {
            self.bring_focus_forward(ob, "a slide hold ended", None);
        }
    }

    /// How long before an activation a Shift press still counts (`[sugar]`).
    fn activation_window(&self) -> Duration {
        Duration::from_millis(self.store.config().sugar.activation_window_ms)
    }

    /// A managed window minimized (the yellow button, ⌘M, Window → Minimize, its app's own
    /// code; macOS tells once its genie has put it in the Dock): with `[windows]
    /// minimize_to_strip` (Apple's behavior) it goes to the strip as a card of its own
    /// instead. Its picture comes out of its Dock tile into the card ([`Minimized`]); the
    /// window is put beyond the screen's Dock edge in line with its park frame while still
    /// minimized ([`minimize::dive_frame`]), so its un-minimize, which macOS always animates,
    /// sinks out of the tile off the screen instead of crossing it to the corner, and it is
    /// parked again the moment it is down ([`Self::on_deminiaturized`]).
    fn on_minimized(&mut self, ob: &mut Observer, id: u32) {
        // Its own screen's answer, as for a minimize caught before the genie
        // ([`Self::minimize_direct`]): a full-screen app on the other screen stops nothing here.
        let display = self.model.as_ref().and_then(|m| m.window(WindowId(id))).map(|w| w.meta.display);
        if !self.store.config().windows.minimize_to_strip || display.is_none_or(|d| self.frozen_on(d)) {
            return;
        }
        let obs = self.observe(ob);
        let Some(m) = self.model.as_mut() else { return };
        let actions = m.minimize_to_strip(WindowId(id), &obs);
        if actions.is_empty() {
            return;
        }
        let screen = m.window(WindowId(id)).map(|w| m.display(w.meta.display).frame);
        let (pid, title) = ob.window(id).map_or((0, String::new()), |w| (w.pid, w.title.clone()));
        let read_at = Instant::now();
        let dock = super::dock::dock_pid();
        let tile = dock.and_then(|d| super::dock::minimized_tile(d, super::dock::app_path(pid).as_deref(), &title));
        // The Dock's edge from its bar when no tile was read.
        let edge = tile.or_else(|| dock.and_then(super::dock::bar_frame));
        let read_ms = read_at.elapsed().as_millis() as u64;
        let mut dive = None;
        if let (Some(screen), Some(edge)) = (screen, edge)
            && actions.contains(&Action::Hide { id: WindowId(id) })
            && let Some(park) = self.engine.hidden_at(m, WindowId(id))
        {
            dive = minimize::dive_frame(park, edge, screen, &desktop::cg_display_bounds());
        }
        tracing::info!(
            id,
            tile = ?tile,
            dive = ?dive,
            read_ms,
            "window minimized: it goes to the strip instead of the Dock"
        );
        self.minimizing = Some(Minimized { window: id, tile });
        // Its hide in this batch goes to the dive frame, no later one.
        self.hide_once_at(dive.map(|d| (id, d)));
        self.execute_animated(ob, actions);
        self.hide_once_at(None);
        self.minimizing = None;
        if let Some(w) = ob.window(id) {
            let now = Instant::now();
            self.echoes.touch_window(id, now);
            self.echoes.touch_app(w.pid, now);
            self.unminimizing.retain(|_, at| now.duration_since(*at) < UNMINIMIZE_MAX);
            self.unminimizing.insert(id, now);
            if let Err(e) = w.element.set_minimized(false) {
                tracing::warn!(id, "cannot restore the minimized window: {e}");
            }
        }
    }

    /// A minimize the minimize tap caught before macOS's genie (the yellow button, ⌘M;
    /// [`super::minimize_tap`]): with `[windows] minimize_to_strip` the window goes straight
    /// to the strip, as Apple's (recorded 11-09): it leaves as a stage does in a switch, its
    /// picture flying from its frame into a new card at the top of the strip, the other cards
    /// one slot down; under Reduce Motion it fades out where it is while its card fades in.
    /// It never goes into the Dock. When it cannot go now (the setting off, no stage change
    /// possible, a window no longer shown in its stage) it is minimized as macOS would: the
    /// tap swallowed the person's minimize, so it is never lost.
    pub fn minimize_direct(&mut self, ob: &mut Observer, ask: MinimizeAsk) {
        let id = ask.window;
        let waited_ms = ask.at.elapsed().as_millis() as u64;
        let Some(m) = self.model.as_ref() else { return ask.as_macos("not managing windows") };
        let Some(w) = m.window(WindowId(id)) else { return ask.as_macos("not a managed window") };
        if w.parked {
            tracing::debug!(id, by = ask.by.name(), "a caught minimize of a window already parked: nothing");
            return;
        }
        let display = w.meta.display;
        let why = if !self.store.config().windows.minimize_to_strip {
            Some("[windows] minimize_to_strip is off")
        } else if let Some(why) = self.frozen_reason() {
            Some(why)
        } else if self.fullscreen_on(display) {
            Some("a full-screen app's Space shows on its screen")
        } else if self.overview.showing().is_some() {
            Some("Mission Control, App Expose or Show Desktop shows")
        } else if !m.minimizable_windows().any(|w| w.0 == id) {
            Some("not a window shown in its stage")
        } else {
            None
        };
        if let Some(why) = why {
            return ask.as_macos(why);
        }
        self.end_restore(ob, "a minimize", End::User);
        let obs = self.observe(ob);
        let Some(m) = self.model.as_mut() else { return };
        let actions = m.minimize_to_strip(WindowId(id), &obs);
        if actions.is_empty() {
            return ask.as_macos("the stage model kept it");
        }
        tracing::info!(id, by = ask.by.name(), waited_ms, "window minimized: straight to the strip, no genie (caught before macOS)");
        self.minimizing = None;
        self.execute_animated(ob, actions);
    }

    /// Tells the minimize tap ([`super::minimize_tap`]) which windows a minimize is caught on,
    /// with their tracked frames: the model's shown stage windows
    /// ([`Model::minimizable_windows`]), none held behind a transition's snapshots or on a
    /// screen showing a full-screen app; none at all with `[windows] minimize_to_strip` off,
    /// no stage change possible or an overview showing. Cheap (no Accessibility call); after
    /// every batch, every move and every tick.
    fn publish_minimizable(&self, ob: &Observer) {
        let Some(m) = &self.model else {
            super::minimize_tap::publish(Minimizable::default());
            return;
        };
        let on = self.store.config().windows.minimize_to_strip && !self.frozen() && self.overview.showing().is_none();
        let windows = if on {
            m.minimizable_windows()
                .filter(|id| self.pending.as_ref().is_none_or(|p| !p.entries.contains_key(&id.0)))
                .filter(|id| m.window(*id).is_some_and(|w| !self.fullscreen_on(w.meta.display)))
                .filter_map(|id| ob.window(id.0).map(|w| (id.0, w.frame)))
                .collect()
        } else {
            HashMap::new()
        };
        let dock = self.dock_bar.as_ref().map(|(_, b)| b.clone()).unwrap_or_default();
        super::minimize_tap::publish(Minimizable { on, windows, dock });
    }

    /// A window un-minimized on its way to the strip ([`Self::on_minimized`]) is down: AppKit
    /// has set it where it keeps a title bar on screen (from beyond the Dock's edge, in the park
    /// corner, or at that edge for a side Dock). It is parked again now, before the reconcile's
    /// wait for it to rest, so it shows outside the corner a frame or two at most.
    fn on_deminiaturized(&mut self, ob: &mut Observer, id: u32) {
        let Some(at) = self.unminimizing.remove(&id) else { return };
        if at.elapsed() >= UNMINIMIZE_MAX {
            return;
        }
        let Some(frame) = self.model.as_ref().filter(|m| m.window(WindowId(id)).is_some_and(|w| w.parked)).and_then(|m| m.frame_left(WindowId(id)))
        else {
            return;
        };
        let listed = ob.window(id).and_then(|w| w.element.frame().ok());
        tracing::debug!(id, listed = ?listed, park = %frame, after_ms = at.elapsed().as_millis() as u64, "a window un-minimized on its way to the strip is down: parked again at once");
        self.execute(ob, vec![Action::Hide { id: WindowId(id) }]);
    }

    /// True when window `id` sits in a stage that is not in front on its screen.
    fn window_elsewhere(&self, id: u32) -> bool {
        self.model.as_ref().is_some_and(|m| m.stage_of(WindowId(id)).is_some_and(|s| !m.stage_in_front(s)))
    }

    /// True when app `pid` has a standard window on the current stage (or pinned on this
    /// Space): its activation switches nothing, so Shift has nothing to change (a Shift-click
    /// inside one of its windows, to extend a selection, must not pull in its other windows).
    fn app_present(&self, pid: i32) -> bool {
        self.model.as_ref().is_some_and(|m| {
            // On any screen's shown Desktop (round displays-2).
            m.displays().iter().any(|d| {
                let active = m.active_stage(d.id);
                let space = m.current_space(d.id);
                m.windows().any(|w| {
                    w.meta.pid == pid
                        && w.meta.role == WindowRole::Standard
                        && ((w.home == Home::Pinned && (w.space == space || w.sticky))
                            || active.is_some_and(|a| w.home == Home::Stage(a)))
                })
            })
        })
    }

    /// An app was hidden (⌘H, Hide Others, the app itself; Apple's, measured 11-09): its
    /// windows leave the cards, a stage of its alone leaves the strip, and when that stage
    /// was in front the most recent stage comes in front ([`Model::app_hidden`]). The hidden
    /// windows are parked first, unanimated: macOS has taken them off the screen already, so
    /// there is nothing to shrink into a card; the stage coming in front grows as on a click.
    fn on_app_hidden(&mut self, ob: &mut Observer, pid: i32) {
        let obs = self.observe(ob);
        // No stage comes in front on a screen that takes no input now, as for any switch.
        let frozen: Vec<DisplayId> = self.managed_displays().into_iter().map(|d| d.id).filter(|d| self.frozen_on(*d)).collect();
        let Some(m) = self.model.as_mut() else { return };
        // Hide Others hides several apps together, each with a notice of its own: every
        // managed app macOS says is hidden by now is hidden for the model before it picks the
        // stage that comes in front (asked after the windows were read, the notices' gap past).
        let mut pids = vec![pid];
        let others: BTreeSet<i32> = m.windows().map(|w| w.meta.pid).filter(|p| *p != pid && !m.app_is_hidden(*p)).collect();
        pids.extend(others.into_iter().filter(|p| super::apps::is_hidden(*p) == Some(true)));
        let actions = m.apps_hidden(&pids, &obs, |d| !frozen.contains(&d));
        let switched = actions.iter().any(|a| matches!(a, Action::SetFrame { .. }));
        tracing::info!(pid, also = ?&pids[1..], switched, "app hidden: its windows leave the strip; the most recent stage comes in front if it was in front");
        let hidden = |id: WindowId| m.window(id).is_some_and(|w| m.app_is_hidden(w.meta.pid));
        let (quiet, rest): (Vec<Action>, Vec<Action>) =
            actions.into_iter().partition(|a| matches!(a, Action::Hide { id } if hidden(*id)));
        self.execute(ob, quiet);
        self.execute_animated(ob, rest);
    }

    /// An app quit and `left` are the stages its windows were in: where one is left empty in
    /// front, the most recent stage comes in front when macOS brings none
    /// ([`Model::front_after_quit`]: the Spaces engine; under the Corner engine macOS
    /// activates the next app, which switches). No stage comes in front on a screen that
    /// takes no input now, as for any switch.
    fn front_after_quit(&mut self, ob: &mut Observer, left: &BTreeSet<StageId>) {
        if left.is_empty() {
            return;
        }
        let obs = self.observe(ob);
        let frozen: Vec<DisplayId> = self.managed_displays().into_iter().map(|d| d.id).filter(|d| self.frozen_on(*d)).collect();
        let Some(m) = self.model.as_mut() else { return };
        let actions = m.front_after_quit(left, &obs, |d| !frozen.contains(&d));
        if !actions.is_empty() {
            self.execute_animated(ob, actions);
        }
    }

    /// An app was unhidden: a stage of its that left the strip comes back as a card in the
    /// slot it left, not in front ([`Model::app_unhidden`]); an activation with it (⌘-Tab, a
    /// Dock click) switches as any activation does, on its own event.
    fn on_app_unhidden(&mut self, ob: &Observer, pid: i32) {
        let Some(m) = self.model.as_mut() else { return };
        let actions = m.app_unhidden(pid);
        tracing::info!(pid, "app unhidden: its stage's card is back in the strip");
        self.execute(ob, actions);
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
        let (Some(_), Some(m)) = (&self.press, &self.model) else { return };
        // A window no stage holds, on screen (stranded, round stage-lost): its drag counts
        // too, and it is taken into the stage in front when the drag ends.
        let (pid, restore, adopt) = match m.window(WindowId(id)) {
            Some(w) => {
                let shown = !w.parked
                    && (w.home == Home::Pinned || m.stage_of(WindowId(id)).is_some_and(|s| m.stage_in_front(s)));
                if !shown || w.meta.role != WindowRole::Standard {
                    return;
                }
                (w.meta.pid, w.restore, false)
            }
            None => {
                let Some(t) = ob.window(id) else { return };
                let (pid, frame) = (t.pid, t.frame);
                // The cheap reasons first: the window server is read only for a window that
                // may be stranded (a dialog's or a new window's drag reads nothing).
                let maybe = t.kind == WindowKind::Standard
                    && !self.unplaced.contains(&id)
                    && now.saturating_duration_since(t.found_at) >= stranded::SETTLED
                    && m.windows().any(|w| w.meta.pid == pid);
                if !maybe || self.stranded_windows(ob, &windows::on_screen(), Some(id)).is_empty() {
                    return;
                }
                (pid, frame, true)
            }
        };
        if !input::left_button_down() {
            return;
        }
        let Some(press) = &self.press else { return };
        if !drag_plausible(press.at, press.live.get(&id).copied(), input::pointer()) {
            tracing::trace!(id, "a move while the button is down elsewhere: the app moved its window, no drag");
            return;
        }
        let pre = press.frames.get(&id).copied().unwrap_or(restore);
        let Some(now_frame) = ob.window(id).map(|t| t.frame) else { return };
        let moving = (now_frame.w - pre.w).abs() < 1.0 && (now_frame.h - pre.h).abs() < 1.0;
        self.drag = Some(WindowDrag { id, pid, pre, moving, grab: press.at, adopt });
        self.user_took(id, "dragged");
        if moving {
            tracing::info!(id, "window drag started");
            self.present();
        }
    }

    /// Global input in another app (mouse down/up, key down). `drop` tells, for a mouse-up
    /// over the visible strip, which slot a dragged window goes to.
    pub fn input(&mut self, ob: &mut Observer, event: InputEvent, drop: Option<StripDrop>) {
        let now = Instant::now();
        if event.kind == InputKind::CommandKey {
            // ⌘ pressed or let go: only ⌘Tab's switcher cares (its release activates).
            self.switcher_key(ob, &event, now);
            return;
        }
        let mods = Modifiers { shift: event.shift, cmd: event.cmd };
        if event.kind == InputKind::MouseUp {
            self.triggers.record_release(now, mods);
        } else {
            self.triggers.record(now, mods);
        }
        if event.kind == InputKind::MouseUp {
            // A desktop icon may have been dropped: Finder's icons are read again shortly.
            self.patch_items.due = Some(now + PATCH_ITEMS_RETRY);
            self.pointer_released(event.at, now);
        }
        if matches!(event.kind, InputKind::MouseDown | InputKind::MouseUp) {
            // A press may start a window drag: the corner patch's fallback hides at once.
            self.look_at_screen();
        }
        match event.kind {
            InputKind::MouseDown => {
                tracing::debug!(x = event.at.x, y = event.at.y, "mouse down in another app");
                // A press whose release never came (missed by the monitor) waits no more.
                self.wallpaper_press = None;
                self.end_restore(ob, "a click (or a Dock click)", End::User);
                // One window list for the press: the Dock's bar and the wallpaper are looked
                // up in it. A press on a Dock item: its click may take the user to another
                // Desktop. Not read while nothing can act on the press: an unread list is
                // never an empty one, where every press would be on the wallpaper.
                let pressed = self.managed_displays().into_iter().find(|d| d.frame.contains(event.at)).map(|d| self.frozen_on(d.id));
                let list = press_reads_list(self.model.is_some(), self.frozen(), pressed).then(windows::list_windows);
                self.dock_pressed(ob, list.as_deref().unwrap_or_default(), event.at, now);
                // A click where an arriving window is about to be was meant for it: it
                // arrives now, and the click is no wallpaper click.
                let for_arriving = self.pending.as_ref().is_some_and(|p| p.frames.iter().any(|f| f.contains(event.at)));
                // The owner's click decides where the keyboard goes: no focus of ours follows it, but
                // for a click on an arriving window, which lands now as the switch meant.
                if for_arriving {
                    self.flush_pending(ob);
                } else {
                    self.flush_superseded(ob, "a click");
                }
                self.end_drag();
                self.user_switched();
                self.press = self.model.as_ref().map(|m| Press {
                    at: event.at,
                    frames: m.windows().filter(|w| !w.parked).map(|w| (w.meta.id.0, w.restore)).collect(),
                    live: m.windows().filter(|w| !w.parked).map(|w| (w.meta.id.0, w.meta.frame)).collect(),
                });
                let wallpaper = if for_arriving {
                    tracing::debug!("click during a transition, on an arriving window");
                    false
                } else {
                    list.as_deref().is_some_and(|list| self.mouse_down(list, event.at, event.clicks, event.stamp))
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
                // A desktop press (a click, a long press, a drag): Finder's echo runs on from here.
                self.desktop_echo.released(now);
                if let Some(press) = self.wallpaper_press.take() {
                    self.wallpaper_released(ob, press, event.at, event.stamp);
                }
                self.focus_after_press_done(ob);
                let Some(drag) = self.drag.take() else { return };
                if drag.adopt && self.model.as_ref().is_some_and(|m| m.window(WindowId(drag.id)).is_none()) {
                    // A window no stage held: the user's own act on it takes it into the stage
                    // in front first, then the drop goes on (round stage-lost).
                    let why = if drop.is_some() { "dropped on the strip" } else { "moved by the user" };
                    if self.adopt_stranded(ob, why, Some(drag.id)).is_empty() {
                        tracing::info!(id = drag.id, "a window no stage holds was dragged: it is not one to take in (not stranded any more); nothing done");
                        self.present();
                        return;
                    }
                }
                if let Some(slot) = drop.filter(|_| drag.moving) {
                    counters::bump(Count::Drop);
                    tracing::info!(id = drag.id, before = ?slot.before, "window dropped on the strip: a stage of its own");
                    let obs = self.observe(ob);
                    if let Some(m) = self.model.as_mut() {
                        let refused = m.send_refused(WindowId(drag.id));
                        let actions = m.drop_window_on_strip(WindowId(drag.id), drag.pre, slot.before.map(StageId), &obs);
                        if actions.is_empty() {
                            tracing::info!(id = drag.id, why = refused.unwrap_or("nothing to change"), "window dropped on the strip: nothing done");
                        }
                        self.execute_animated(ob, actions);
                    }
                } else if drag.moving {
                    tracing::debug!(id = drag.id, "window drag ended in the main area");
                }
                self.present();
            }
            InputKind::OtherMouseDown => {
                // A right or middle click is the user's: it ends the login's settling (their
                // focus and stage stand), and is no drag, Dock or wallpaper click.
                tracing::debug!(x = event.at.x, y = event.at.y, "right or other mouse down in another app");
                self.end_restore(ob, "a right or middle click", End::User);
            }
            InputKind::CommandKey => {}
            InputKind::KeyDown => {
                // The switcher's selection first: its window put home before ⌘'s release.
                self.switcher_key(ob, &event, now);
                // ⌘Tab's own app switch brings its stage; nothing else comes forward for it.
                if event.is_switcher_key() {
                    self.end_restore(ob, "an app switcher key", End::Switcher);
                } else {
                    self.end_restore(ob, "a key press", End::User);
                    self.touch_typed_window();
                }
                // Typing never cuts a switch's animation short: the keyboard is already the
                // new stage's (its focus ran before the windows arrived).
                if event.cmd {
                    self.user_switched();
                }
                if self.desktop_echo.user_acted() {
                    tracing::debug!("a key press after a wallpaper click: Finder's next activation is deliberate");
                }
            }
        }
    }

    /// A key press (not a switcher's) went to the frontmost app: its focused window is the
    /// user's, a late title re-matches it no more ([`Model::touch_window`]). Asked of the app
    /// (bounded) only while a window of it may still be re-matched, so typing costs nothing
    /// after the re-matches or in an app with none.
    fn touch_typed_window(&mut self) {
        let Some(pid) = super::apps::frontmost_pid() else { return };
        if !self.model.as_ref().is_some_and(|m| m.may_rematch_app(pid)) {
            return;
        }
        let Some(id) = arrival::focused_window_of(pid) else { return };
        if let Some(m) = self.model.as_mut() {
            tracing::debug!(id = id.0, pid, "a key press in a window a late title could still move: the user's now");
            m.touch_window(id);
        }
    }

    /// The button is up after a parked window was grabbed by its sliver: its stage comes now,
    /// the window where the user left it when that is outside the park zone.
    fn focus_after_press_done(&mut self, ob: &mut Observer) {
        let Some(id) = self.focus_after_press.take() else { return };
        // The user holds it: no check of a move made ahead parks it again (R11 3).
        self.user_took(id, "grabbed by its sliver");
        if self.phase.changing() {
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
                if self.locked {
                    // Recognized once the screen unlocks ([`Self::settle_space`]).
                    self.phase.feed(phase::Event::SpaceChanged, Instant::now());
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
                // Every screen: the main one's arrival goes on to the others. A second
                // arrival on them from the same list counted as two in a row (a Desktop seen
                // beside the one shown had its windows taken as moved at once).
                self.arrive_here(ob, &list, main, None);
            }
            return;
        }
        let displays = if self.displays.is_empty() { fallback_displays() } else { self.displays.clone() };
        let config = self.store.config().clone();
        let capacity = strip_capacity(displays[0].visible, &config);
        let mut options = config.model_options(self.settings, capacity);
        options.engine = self.engine.kind();
        options.fit = fit_options(displays[0].visible, self.strip_visible, &self.settings, &config);
        let main = displays[0];
        let mut model = Model::new(displays, options);
        for d in model.displays().to_vec() {
            model.set_display_fit(d.id, fit_options(d.visible, self.strip_visible, &self.settings, &config));
        }
        let list = windows::list_windows();
        if !self.can_build(ob, &list) {
            return;
        }
        Self::read_desktop_counts(&mut model);
        // Windows on a display not managed are left alone; they wait until they show on one
        // (and so do windows off every display: none is managed yet).
        let frames: Vec<Rect> = model.displays().iter().map(|d| d.frame).collect();
        let shown = on_managed_screens(&list, &frames, |_| false);
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
        // On screen now but not listed through Accessibility (its app does not answer yet):
        // found later, such a window was there all along and brings no stage forward.
        let listed: HashSet<u32> = live.iter().map(|m| m.id.0).collect();
        let unlisted: Vec<WindowId> = shown.iter().filter(|id| !listed.contains(id)).map(|id| WindowId(*id)).collect();
        if !unlisted.is_empty() {
            tracing::debug!(windows = unlisted.len(), "windows on screen at launch that Accessibility does not list yet: found later, they bring no stage forward");
        }
        model.set_launch_listed(unlisted);
        let state = Self::load_state(config.stages.restore_after_restart);
        let first_start = state.is_none();
        tracing::info!(
            live = live.len(),
            off_screen = self.unplaced.len(),
            saved_spaces = state.as_ref().map_or(0, |s| s.0.spaces.len()),
            "building stages"
        );
        if let Some((state, restore)) = state {
            self.seen_times = state.seen_times();
            self.settling = restore.restoring.then(|| {
                let login = if restore.new_session { super::apps::login_started() } else { None };
                tracing::info!(after_restart = restore.new_session, login_ago_s = login.map(|l| l.elapsed().as_secs()), "restore: the restore window opens");
                Settling::new(Instant::now(), login)
            });
            self.rematch_until = self.settling.as_ref().map(Settling::rematch_until);
            model.set_restore(restore);
            model.restore_spaces(state.spaces);
        }
        if main.frame.w > 0.0 && self.screen_seen(ob, &list, main.frame) == Seen::FullScreen {
            self.phase.feed(phase::Event::LaunchedFullScreen, Instant::now());
            tracing::info!("launched on a full-screen app's Space: no strip until a Desktop shows");
        }
        let hints = launch_hints(&list);
        // Every display's Desktop recognized from the windows on its screen (round displays).
        let actions = if self.phase.fullscreen() {
            Vec::new()
        } else {
            let mut all = Vec::new();
            for d in model.displays().to_vec() {
                let here: Vec<WindowMeta> = live.iter().filter(|m| m.display == d.id).cloned().collect();
                // A screen with windows sent to it by the first arrival (back to their saved
                // stage's screen, `engine::model::pool`) arrives even with nothing else, and so
                // does one whose only Desktop is its one saved Desktop (its windows not listed
                // yet: they go to their stages there as they show).
                if d.id != main.id && here.is_empty() && !model.awaits_travellers(d.id) && !model.forms_saved_desktop(d.id) {
                    continue;
                }
                let more = model.arrive(d.id, &here, &[], &hints);
                all.extend(more.into_iter().filter(|a| *a != Action::StateChanged));
            }
            // None is left (every screen arrived on); never one unmanaged for good if it is.
            let left = model.take_travellers();
            if !left.is_empty() {
                tracing::warn!(windows = left.len(), "restore: windows sent to another screen whose Desktop did not form; placed when found");
                self.unplaced.extend(left.iter().map(|m| m.id.0));
            }
            all.push(Action::StateChanged);
            all
        };
        if first_start {
            tracing::info!(stages = model.stages().len(), "first start: one stage per app, as Apple's Stage Manager does");
        }
        if self.phase.fullscreen() {
            self.unplaced.extend(live.iter().map(|m| m.id.0));
        }
        // Saved windows on screen at the launch went back at once: the first placed now.
        if let Some(clock) = self.settling.as_mut()
            && model.restore_summary(0).windows > 0
        {
            clock.placed(Instant::now());
        }
        self.model = Some(model);
        // Following the Dock, the first strip shows in the Dock's order already.
        self.refresh_dock_order(Instant::now(), ob.last_launch());
        self.announce = config.style.reveal.launch_reveal;
        self.execute(ob, actions);
        if !self.phase.fullscreen() {
            self.pin_floats(ob, &live);
        }
        self.present();
        self.mark_dirty();
    }

    /// [`on_managed_screens`] with the model's windows as the managed ones, and the managed
    /// windows the engine hides off the list on a Desktop that shows
    /// ([`WindowEngine::present_off_screen`]).
    pub(super) fn shown_managed(&self, list: &[windows::CgWindow]) -> HashSet<u32> {
        let frames: Vec<Rect> = self.managed_displays().iter().map(|d| d.frame).collect();
        let m = self.model.as_ref();
        let mut shown = on_managed_screens(list, &frames, |id| m.is_some_and(|m| m.window(WindowId(id)).is_some()));
        // Hidden where the list shows them off screen: on their Desktop all the same.
        shown.extend(self.engine.present_off_screen().into_iter().filter(|id| m.is_some_and(|m| m.window(WindowId(*id)).is_some())));
        shown
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
        self.phase.fullscreen() || self.phase.changing() || self.locked
    }

    /// Whether `display` shows a full-screen app's Space: the main screen's is the phase's,
    /// another's its own watch's ([`Self::other_screen_seen`]).
    fn fullscreen_on(&self, display: DisplayId) -> bool {
        fullscreen_there(display, self.main_display_id(), self.phase.fullscreen(), |d| self.other_screen_fullscreen(d.0))
    }

    /// [`Self::frozen`] for input addressed to `display`'s stages (a card of its strip, a
    /// shortcut with the keyboard on it, a click on its wallpaper): a full-screen app's Space
    /// on another screen stops nothing here (the review's S1; UX-SPEC Screens). A Space
    /// change settling and the lock hold every screen.
    fn frozen_on(&self, display: DisplayId) -> bool {
        self.fullscreen_on(display) || self.phase.changing() || self.locked
    }

    /// [`Self::frozen`] for a focus of window `id`: its own screen's answer
    /// ([`Self::frozen_on`]) when the model places it, the main screen's for one it does not.
    fn frozen_for_window(&self, id: u32) -> bool {
        match self.model.as_ref().and_then(|m| m.window(WindowId(id))).map(|w| w.meta.display) {
            Some(display) => self.frozen_on(display),
            None => self.frozen(),
        }
    }

    /// [`Self::frozen`] for an activation of app `pid` ([`activation_held`]): the screens its
    /// windows are on answer for it.
    fn activation_frozen(&self, pid: i32) -> bool {
        let screens: BTreeSet<DisplayId> =
            self.model.as_ref().map(|m| m.windows().filter(|w| w.meta.pid == pid).map(|w| w.meta.display).collect()).unwrap_or_default();
        activation_held(self.frozen(), screens.into_iter().map(|d| self.frozen_on(d)))
    }

    /// The screen the keyboard is on ([`Model::keyboard_display`]): the focused window's, by
    /// the model when it manages it, else the managed screen most of it shows on in `list`;
    /// the main one with no focused window.
    fn keyboard_display(&self, focused: Option<WindowId>, list: &[windows::CgWindow]) -> DisplayId {
        let shown_on = focused.and_then(|f| list.iter().find(|w| w.id == f.0)).map(|w| self.display_at(w.bounds));
        self.model.as_ref().map_or_else(|| self.main_display_id(), |m| m.keyboard_display(focused, shown_on))
    }

    /// Why stage commands are refused now, if they are ([`Self::frozen`]).
    fn frozen_reason(&self) -> Option<&'static str> {
        if self.locked {
            Some("the screen is locked")
        } else if self.phase.changing() {
            Some("a Space change is settling")
        } else if self.phase.fullscreen() {
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

    /// What another screen `d` (not the main one) shows in `list` ([`OtherSeen`]): its own
    /// overview watch and full-screen settle step (round displays-2), logged once a change.
    fn other_screen_seen(&mut self, ob: &Observer, list: &[windows::CgWindow], d: Display, now: Instant) -> OtherSeen {
        let screen = d.id.0;
        let sightings = list.iter().map(|w| Sighting { owner: &w.owner, layer: w.layer, bounds: w.bounds, onscreen: w.onscreen });
        let sight = overview::overview_in(sightings, d.frame);
        let watch = self.screens.entry(screen).or_default();
        match watch.overview.see(sight, now) {
            overview::Change::None => {}
            overview::Change::Began(kind) => tracing::info!(screen, "{} shows on another screen: no Space is inferred from its windows until it closes", kind.name()),
            overview::Change::Ended(kind) => tracing::info!(screen, "{} closed on another screen", kind.name()),
            overview::Change::Stuck(kind) => tracing::warn!(screen, "{} has been listed on another screen for {} s without a break: taken for something else", kind.name(), overview::OVERVIEW_STUCK.as_secs()),
        }
        if watch.overview.freezes(now) {
            watch.absent.clear();
            return OtherSeen::Overview;
        }
        let seen = self.screen_seen(ob, list, d.frame);
        let watch = self.screens.entry(screen).or_default();
        let verdict = watch.settle.step(seen, now);
        let fullscreen = match verdict {
            Verdict::Wait => return OtherSeen::Animation,
            Verdict::FullScreen => true,
            Verdict::Desktop => false,
        };
        if watch.fullscreen != fullscreen {
            watch.fullscreen = fullscreen;
            watch.absent.clear();
            if fullscreen {
                tracing::info!(screen, "a full-screen app's Space shows on another screen (no strip there; the other screens go on)");
            } else {
                tracing::info!(screen, "another screen shows a Desktop again");
            }
        }
        if fullscreen { OtherSeen::FullScreen } else { OtherSeen::Desktop }
    }

    /// Whether another screen `screen` shows a full-screen app's Space, as last seen
    /// ([`Self::other_screen_seen`]).
    fn other_screen_fullscreen(&self, screen: u32) -> bool {
        self.screens.get(&screen).is_some_and(|w| w.fullscreen)
    }

    /// The once-a-second "left this Space" check for every screen but the main one (round
    /// displays-2; the main one's is [`Self::check_spaces`] itself): on a screen showing a
    /// Desktop, windows of another of its Desktops on screen there mean its Desktop changed
    /// (recognized again, [`Self::arrive_others`]); windows of its Desktop missing from the
    /// screen for no visible reason at [`SPACE_CHECKS`] checks in a row are let go (most of
    /// them at once: its Desktop changed instead). Nothing is inferred on a screen showing an
    /// overview, a full-screen animation or a full-screen app's Space.
    fn check_other_screens(&mut self, ob: &mut Observer, list: &[windows::CgWindow], now: Instant) {
        let others: Vec<Display> = self.model.as_ref().map(|m| m.displays().iter().skip(1).copied().collect()).unwrap_or_default();
        if others.is_empty() {
            self.screens.clear();
            return;
        }
        self.screens.retain(|screen, _| others.iter().any(|d| d.id.0 == *screen));
        let shown = self.present_in(list);
        let mut arrive = false;
        let mut left: Vec<u32> = Vec::new();
        for d in others {
            if self.other_screen_seen(ob, list, d, now) != OtherSeen::Desktop {
                continue;
            }
            let Some(m) = self.model.as_ref() else { return };
            let space = m.current_space(d.id);
            let standard = |w: &&crate::engine::model::WindowState| w.meta.role == WindowRole::Standard && !w.sticky;
            // Windows of another of this screen's Desktops on screen there: its Desktop
            // changed (no notice names the screen); recognized again.
            let elsewhere = m.windows().filter(standard).any(|w| w.space != space && m.display_of_space(w.space) == d.id && shown.contains(&w.meta.id.0));
            if elsewhere {
                arrive = true;
                continue;
            }
            let here = m.windows().filter(standard).filter(|w| w.space == space).count();
            let here_shown = m.windows().filter(standard).filter(|w| w.space == space && shown.contains(&w.meta.id.0)).count();
            let missing: Vec<(u32, i32)> = m
                .windows()
                .filter(standard)
                .filter(|w| w.space == space && !shown.contains(&w.meta.id.0))
                .filter(|w| !self.echoes.is_window_echo(w.meta.id.0, now))
                .filter(|w| !ob.noted_closed(w.meta.id.0))
                .filter(|w| ob.window(w.meta.id.0).is_some())
                .map(|w| (w.meta.id.0, w.meta.pid))
                .collect();
            let excused = self.off_screen_for_a_reason(ob, &missing);
            let missing: Vec<u32> = missing.into_iter().map(|(id, _)| id).filter(|id| !excused.contains(id)).collect();
            let watch = self.screens.entry(d.id.0).or_default();
            watch.absent.retain(|id, _| missing.contains(id));
            let mut gone = Vec::new();
            for &id in &missing {
                let n = watch.absent.entry(id).or_insert(0);
                *n += 1;
                if *n >= SPACE_CHECKS {
                    gone.push(id);
                }
            }
            if gone.is_empty() {
                continue;
            }
            if missed_space_change(here, here_shown, gone.len()) {
                tracing::info!(screen = d.id.0, missing = gone.len(), of = here, "most windows of another screen's Desktop are gone from it; recognizing that screen's Desktop again");
                watch.absent.clear();
                arrive = true;
                continue;
            }
            let listed: HashSet<u32> = list.iter().map(|w| w.id).collect();
            for id in gone.into_iter().filter(|id| listed.contains(id)) {
                watch.absent.remove(&id);
                left.push(id);
            }
        }
        if arrive {
            // What is held for that Desktop is replayed there at once.
            let awaited = self.awaited_elsewhere(now);
            self.arrive_others_for(ob, list, awaited);
        }
        for id in left {
            let Some(m) = self.model.as_mut() else { return };
            if m.window(WindowId(id)).is_none() {
                continue;
            }
            tracing::info!(id, "window left its Space on another screen (not on screen, not minimized, app not hidden)");
            let group = m.group_of(WindowId(id));
            let kept = self.parked_entries_of(&group);
            let Some(m) = self.model.as_mut() else { return };
            let (actions, forgotten) = m.window_left_space(WindowId(id));
            self.execute(ob, actions);
            self.let_go_hidden(ob, kept, LetGo::Other);
            self.unplaced.extend(forgotten.into_iter().map(|w| w.0).filter(|w| ob.window(*w).is_some()));
        }
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
                self.hidden_changed();
            }
            overview::Change::Ended(kind) => {
                tracing::info!("{} closed: Space inference resumes after a settle", kind.name());
                self.hidden_changed();
            }
            overview::Change::Stuck(kind) => {
                tracing::warn!(
                    "{} has been listed for {} s without a break: taken for something else; Space inference resumes",
                    kind.name(),
                    overview::OVERVIEW_STUCK.as_secs()
                );
                self.hidden_changed();
            }
        }
        let frozen = self.overview.freezes(now);
        if frozen {
            // What was counted before it showed is looked at afresh once it has closed.
            self.foreign.clear();
            self.absent.clear();
            self.elsewhere.clear();
            self.off_screen.clear();
        }
        frozen
    }

    /// True when the window list disagrees with the Space the engine thinks is shown (a
    /// Space change under way, its notification not handled yet): a known window of another
    /// Space is on screen, or none of this Space's windows is while one of them is missing
    /// for no visible reason.
    fn space_in_doubt(&self, ob: &Observer, shown: &HashSet<u32>, display: DisplayId) -> bool {
        let Some(m) = &self.model else { return false };
        let space = m.current_space(display);
        let standard = |w: &&crate::engine::model::WindowState| w.meta.role == WindowRole::Standard && !w.sticky;
        if m.windows().filter(standard).any(|w| w.space != space && m.display_of_space(w.space) == display && shown.contains(&w.meta.id.0)) {
            return true;
        }
        let here: Vec<WindowId> = m.windows().filter(standard).filter(|w| w.space == space).map(|w| w.meta.id).collect();
        if here.is_empty() || here.iter().any(|id| shown.contains(&id.0)) {
            return false;
        }
        let absent = self.absent_windows(ob, shown, display);
        here.iter().any(|id| absent.contains(id))
    }

    /// Managed standard windows missing from the screen for no reason we can see: their app
    /// is not hidden, they are not minimized and not in full screen (asked through AX; a
    /// window in full screen is on a Space of its own and still belongs to its Desktop).
    /// Evidence that the Space they live on is not the one shown.
    fn absent_windows(&self, ob: &Observer, shown: &HashSet<u32>, display: DisplayId) -> Vec<WindowId> {
        let Some(m) = &self.model else { return Vec::new() };
        let off: Vec<(u32, i32)> = m
            .windows()
            .filter(|w| w.meta.role == WindowRole::Standard && !w.sticky && !shown.contains(&w.meta.id.0))
            .filter(|w| m.display_of_space(w.space) == display)
            .filter(|w| ob.window(w.meta.id.0).is_some())
            .map(|w| (w.meta.id.0, w.meta.pid))
            .collect();
        let excused = self.off_screen_for_a_reason(ob, &off);
        off.into_iter().filter(|(id, _)| !excused.contains(id)).map(|(id, _)| WindowId(id)).collect()
    }

    /// Those of `windows` (id, pid) off screen for a reason: their app hidden (ours or the
    /// user's), minimized, in full screen, or their app too busy to tell. The app's hidden
    /// state is asked once per app, and an app that did not answer in time is not asked
    /// again for a moment ([`OffScreen`], review H1): a hung app no longer costs a timeout per
    /// window every second on the WM thread.
    fn off_screen_for_a_reason(&self, ob: &Observer, windows: &[(u32, i32)]) -> Vec<u32> {
        let now = Instant::now();
        let mut busy = self.busy_apps.borrow_mut();
        busy.prune(now);
        let mut check = OffScreen::new(&mut busy, now);
        let out: Vec<u32> = windows
            .iter()
            .filter(|&&(id, pid)| check.has_reason(pid, || ob.app_hidden_reply(pid), || ob.minimized_reply(id), || ob.fullscreen_reply(id)))
            .map(|&(id, _)| id)
            .collect();
        // Once per episode, not every retry (8M: two apps polled for a minute, a line each).
        for e in busy.take_episodes() {
            match e {
                Episode::Started { pid } => tracing::info!(
                    pid,
                    bundle = ob.bundle_id(pid),
                    retry_s = BUSY_BACKOFF.as_secs(),
                    "an app did not answer whether its windows are hidden or minimized: asked again now and then, its windows left as they are (logged again when it answers)"
                ),
                Episode::Answered { pid, lasted } => {
                    tracing::info!(pid, bundle = ob.bundle_id(pid), lasted_s = lasted.as_secs(), "an app that did not answer answers again")
                }
                Episode::NotAsked { pid, lasted } => tracing::info!(
                    pid,
                    lasted_s = lasted.as_secs(),
                    "an app that did not answer is no longer asked (its windows went, or it quit)"
                ),
            }
        }
        out
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
        self.space_checked = now;
        self.pressed_obs = None;
        self.switcher_space_changed(now);
        // A focus held for the Desktop being recognized counts from this change: a change
        // inferred from the window list comes seconds after the focus (R3 4).
        self.focus_replay.touch(now);
        self.foreign.clear();
        self.absent.clear();
        // The held window's Desktop slides in: its stage goes out now, before every app's
        // window list is read again (~60 ms) and the Desktop recognized.
        self.arrange_at_notice(ob, now);
        self.phase.feed(phase::Event::SpaceChanged, now);
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
        let now = Instant::now();
        if self.poll_lock(now, true) {
            // Recognized once the screen unlocks (the tick tries again).
            return;
        }
        let list = windows::wake_list();
        // Clicked in Mission Control (or switched there with the keyboard), the Desktop shows
        // once Mission Control has closed: until then the list holds its layout (App Exposé
        // even other Desktops' windows). The notification stays authoritative; the arrival
        // waits for a list that shows the Desktop.
        self.look_for_overview(&list, main.frame, now);
        if self.overview.holds_arrival(now) {
            if self.phase.hold() != Some(Hold::Overview) {
                self.phase.feed(phase::Event::Held(Hold::Overview), now);
                tracing::info!(
                    "Space changed while {} shows; the Desktop is recognized once it has closed",
                    self.overview.showing().map_or("an overview", Overview::name)
                );
            }
            return;
        }
        let was_waiting = self.settle.waiting(now);
        match self.settle.step(self.screen_seen(ob, &list, main.frame), now) {
            Verdict::Wait => {
                self.phase.feed(phase::Event::Held(Hold::Animation), now);
                if !was_waiting {
                    tracing::info!("Space changed during a full-screen animation; waiting for it to settle");
                }
            }
            Verdict::FullScreen => {
                let new = !self.phase.fullscreen();
                self.phase.feed(phase::Event::FullScreenCame, now);
                // An arrangement made ahead on this screen is undone; one on another screen
                // is that screen's arrival's to settle.
                if self.ahead_space().is_some_and(|s| self.screen_of_space(s).id == main.id) {
                    self.undo_ahead(ob, "a full-screen app's Space came");
                }
                if new {
                    counters::bump(Count::FullScreen);
                    tracing::info!("Space changed: a full-screen app's Space (no strip)");
                }
                // The notice may be another screen's (none names its screen): each is
                // recognized as at any Space change, whatever the main one shows.
                let awaited = self.awaited_elsewhere(now);
                self.defer_present = true;
                self.arrive_others_for(ob, &list, awaited);
                self.defer_present = false;
                self.present();
            }
            Verdict::Desktop => {
                let mut view = None;
                if self.arrival_waits(ob, &list, main, now, &mut view) {
                    return;
                }
                self.phase.feed(phase::Event::Arrived, now);
                // A sliver grab let go while the change settled runs first, so the arrival
                // finds the window where the user dropped it, already unparked (R2 L1).
                if self.focus_after_press.is_some() && !input::left_button_down() {
                    self.focus_after_press_done(ob);
                }
                self.arrive_here(ob, &list, main, view);
            }
        }
    }

    /// A Desktop shows: the engine recognizes it by the windows on screen ([`Model::arrive`]),
    /// its stages and strip come back. `view` is what [`Self::arrival_waits`] read from the
    /// same `list` ([`Self::screen_view`]), when it did: the absent sweep (Accessibility asked
    /// about every window not on screen) is not made twice.
    fn arrive_here(&mut self, ob: &mut Observer, list: &[windows::CgWindow], main: Display, view: Option<ScreenView>) {
        let Some(m) = self.model.as_ref() else { return };
        let before = m.current_space(main.id);
        let shown = self.shown_managed(list);
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
        // Windows that waited off screen and show now are read again ([`Observer::recheck_kind`]);
        // a view read before with a kind that changed is read again.
        let waited: Vec<u32> = self.unplaced.iter().copied().filter(|id| shown.contains(id)).collect();
        let rekinded = waited.into_iter().fold(false, |any, id| ob.recheck_kind(id) | any);
        let view = view.filter(|_| !rekinded);
        let Some(m) = self.model.as_ref() else { return };
        let (live, absent) = match view {
            Some((mut live, absent)) => {
                // The frames just read again.
                for meta in &mut live {
                    if let Some(w) = ob.window(meta.id.0) {
                        meta.frame = w.frame;
                    }
                }
                (live, absent)
            }
            None => self.screen_view(ob, list, main),
        };
        let arrivals: Vec<WindowMeta> = live.iter().filter(|meta| m.window(meta.id).is_none()).cloned().collect();
        for meta in &live {
            self.unplaced.remove(&meta.id.0);
        }
        let hints = launch_hints(list);
        let hold = self.macos_after_display_change();
        let carried = self.slide_carried(Instant::now());
        let Some(m) = self.model.as_mut() else { return };
        m.set_hold_arrivals(hold);
        m.set_slide_carried(carried);
        let mut actions = m.arrive(main.id, &live, &absent, &hints);
        m.set_hold_arrivals(false);
        let after = m.current_space(main.id);
        let note = m.last_arrival().clone();
        // Listed for a moment as a display changes: offered again once it still shows.
        self.unplaced.extend(note.held.iter().map(|w| w.0));
        // `new`: windows this run had not seen (all of a saved Desktop's on its first visit,
        // placed into its saved stages: `rebuilt`, `restored` of them by their saved entries;
        // each rebuilt stage is a "new stage" line); `moved`: known windows of another Desktop
        // taken as moved here; `beside`: Desktops still on screen whose windows stay theirs.
        tracing::info!(
            from = before.0,
            to = after.0,
            on_screen = live.len(),
            new = arrivals.len(),
            rebuilt = ?note.rebuilt,
            restored = note.restored,
            other_screen = note.other_screen,
            moved = note.moved,
            carried = note.carried,
            beside = ?note.beside,
            held = note.held.len(),
            kept_home = note.kept_home.len(),
            sticky = note.sticky.len(),
            "Space changed"
        );
        log_arrival_lets(&note);
        self.note_dock_arrival(after, before);
        // A Desktop arranged ahead for the window it was shown for: confirmed or undone.
        let (more, predicted) = self.settle_ahead(ob, list, main.id, after, Instant::now());
        actions.extend(more);
        // A focus or activation still held for a Desktop not shown: maybe another screen's,
        // recognized below.
        let awaited = self.awaited_elsewhere(Instant::now());
        // One batch and one strip snapshot for the arrival: the strip of the Desktop shown
        // takes over in its final order, never first with the stage left active.
        self.defer_present = true;
        self.execute(ob, actions);
        self.pin_floats(ob, &arrivals);
        // Arriving through an app (Dock, ⌘Tab, an activation) whose window waits in a stage
        // here: show it, as Apple's does, at once (the Desktop's slide is the motion).
        self.bring_focus_forward(ob, "arrival", predicted);
        self.arrive_others_for(ob, list, awaited);
        self.defer_present = false;
        self.present();
        self.mark_dirty();
    }

    /// What an arrival on `main` is given: the windows on its screen the engine knows or
    /// waits to place (rules applied), and the managed windows missing from every screen for
    /// no visible reason ([`Self::absent_windows`]; a window on another display is not
    /// missing).
    fn screen_view(&self, ob: &Observer, list: &[windows::CgWindow], display: Display) -> ScreenView {
        let Some(m) = self.model.as_ref() else { return (Vec::new(), Vec::new()) };
        let shown = self.shown_managed(list);
        let z = windows::z_order(list);
        let live = ob
            .windows()
            .keys()
            .copied()
            .filter(|id| shown.contains(id))
            .filter(|id| self.unplaced.contains(id) || m.window(WindowId(*id)).is_some())
            .filter_map(|id| self.meta(ob, id, &z))
            .filter(|meta| meta.display == display.id)
            .filter(|meta| self.allowed_by_rules(meta))
            .collect();
        (live, self.absent_windows(ob, &self.present_in(list), display.id))
    }

    /// Windows stranded on screen (round stage-lost, [`stranded`]): seen by the window server
    /// on a managed screen and tracked by Accessibility, of an app the model manages another
    /// window of, that the model neither manages nor holds waiting. `only`: that window alone.
    fn stranded_windows(&self, ob: &Observer, list: &[windows::CgWindow], only: Option<u32>) -> Vec<u32> {
        let Some(m) = self.model.as_ref() else { return Vec::new() };
        let frames: Vec<Rect> = self.managed_displays().iter().map(|d| d.frame).collect();
        let on = on_managed_screens(list, &frames, |_| false);
        let now = Instant::now();
        let managed_pids: HashSet<i32> = m.windows().map(|w| w.meta.pid).collect();
        let z = windows::z_order(list);
        let seen: Vec<stranded::Seen> = list
            .iter()
            .filter(|w| only.is_none_or(|o| o == w.id) && w.onscreen && w.layer == 0)
            .filter_map(|w| {
                let t = ob.window(w.id)?;
                let meta = self.meta(ob, w.id, &z)?;
                Some(stranded::Seen {
                    id: w.id,
                    standard: t.kind == WindowKind::Standard,
                    on_managed_screen: on.contains(&w.id),
                    managed: m.window(WindowId(w.id)).is_some(),
                    waiting: self.unplaced.contains(&w.id),
                    ignored: !self.allowed_by_rules(&meta),
                    arriving: self.engine.about_to_show(WindowId(w.id)),
                    app_managed: managed_pids.contains(&t.pid),
                    tracked: now.saturating_duration_since(t.found_at),
                })
            })
            .collect();
        stranded::stranded(&seen)
    }

    /// Takes the stranded windows (`only`: that one alone) into the stage in front on their
    /// screen ([`Model::adopt_stranded`]), each said with `why`. The windows taken.
    fn adopt_stranded(&mut self, ob: &mut Observer, why: &'static str, only: Option<u32>) -> Vec<u32> {
        let list = windows::list_windows();
        let ids = self.stranded_windows(ob, &list, only);
        if ids.is_empty() {
            return ids;
        }
        let z = windows::z_order(&list);
        let mut taken = Vec::new();
        for id in ids {
            let Some(mut meta) = self.meta(ob, id, &z) else { continue };
            if let Some(w) = list.iter().find(|w| w.id == id) {
                meta.frame = w.bounds;
            }
            let obs = self.observe_with(ob, &list);
            let Some(m) = self.model.as_mut() else { break };
            let actions = m.adopt_stranded(meta, &obs);
            let stage = m.stage_of(WindowId(id)).map(|s| s.0);
            if stage.is_none() && m.window(WindowId(id)).is_none() {
                tracing::warn!(id, why, "a window stranded on screen could not be taken into the stage in front");
                continue;
            }
            tracing::info!(id, why, ?stage, "a window on screen that no stage held (stranded): taken into the stage in front");
            self.unplaced.remove(&id);
            self.execute(ob, actions);
            taken.push(id);
        }
        taken
    }

    /// Before a switch: when the stage in front on the main screen shows nothing while
    /// stranded windows are on screen, they join it first, so they leave with it rather than
    /// stay over the stage that comes (round stage-lost: the owner's 08:05 card click). True
    /// when one was taken.
    fn adopt_stranded_before_switch(&mut self, ob: &mut Observer, why: &'static str) -> bool {
        let main = self.main_display_id();
        if !self.model.as_ref().is_some_and(|m| m.front_shows_nothing(main)) {
            return false;
        }
        !self.adopt_stranded(ob, why, None).is_empty()
    }

    /// The windows our own batches showed within [`SLIDE_CARRY`] (older ones forgotten), for
    /// the arrival about to run ([`Model::set_slide_carried`]).
    fn slide_carried(&mut self, now: Instant) -> HashSet<WindowId> {
        self.shown_by_us.retain(|_, at| now.saturating_duration_since(*at) < SLIDE_CARRY);
        self.shown_by_us.keys().map(|id| WindowId(*id)).collect()
    }

    /// What is held now (a window's focus, an app's activation) for a Desktop no screen
    /// shows yet, with those Desktops ([`Model::spaces_awaited`]): after the main screen's
    /// arrival, one of them may be another screen's, recognized next
    /// ([`Self::arrive_others_for`]).
    fn awaited_elsewhere(&self, now: Instant) -> Option<(Wanted, Vec<SpaceId>)> {
        let held = self.focus_replay.peek(now)?;
        let spaces = self.model.as_ref()?.spaces_awaited(held);
        (!spaces.is_empty()).then_some((held, spaces))
    }

    /// The other screens' Desktops at a Space change ([`Self::arrive_others`]; no notice names
    /// the screen whose Desktop changed), then the window the user went there for: when a
    /// Desktop `awaited` named ([`Self::awaited_elsewhere`]) shows now, or an arrangement
    /// made ahead there was confirmed, what was held is replayed and its stage comes forward
    /// there, as on the main screen ([`Self::bring_focus_forward`]). The main screen's own
    /// replay came first and could do nothing for it: its Desktop was not recognized yet (a
    /// Dock click or ⌘-Tab to an app on the laptop's other Desktop slid it in with the app's
    /// window left in its card).
    fn arrive_others_for(&mut self, ob: &mut Observer, list: &[windows::CgWindow], awaited: Option<(Wanted, Vec<SpaceId>)>) {
        let predicted = self.arrive_others(ob, list);
        let Some(m) = self.model.as_ref() else { return };
        let came = awaited.filter(|(_, spaces)| spaces.iter().any(|s| m.is_shown_space(*s))).map(|(held, _)| held);
        if came.is_none() && predicted.is_none() {
            return;
        }
        if let Some(held) = came {
            self.focus_replay.hold(held, Instant::now());
        }
        self.bring_focus_forward(ob, "arrival on another screen", predicted);
    }

    /// The other displays' Desktops (round displays), each recognized by the windows on its
    /// screen as the main display's is ([`Self::arrive_here`]; `list`: the window server's
    /// list read for it), with none of the main display's motion. An arrangement made ahead
    /// on one of their Desktops is confirmed or undone at that screen's arrival
    /// ([`Self::settle_ahead`]); the window it was confirmed for is returned. Called after
    /// every arrival on the main display and when windows of another Desktop show on a
    /// secondary screen.
    fn arrive_others(&mut self, ob: &mut Observer, list: &[windows::CgWindow]) -> Option<WindowId> {
        let others: Vec<Display> = self.model.as_ref().map(|m| m.displays().iter().skip(1).copied().collect()).unwrap_or_default();
        if others.is_empty() {
            return None;
        }
        let hints = launch_hints(list);
        let now = Instant::now();
        let mut predicted = None;
        for d in others {
            // A screen showing an overview, a full-screen animation or a full-screen app's
            // Space: its windows say nothing of its Desktop now (round displays-2).
            if self.other_screen_seen(ob, list, d, now) != OtherSeen::Desktop {
                continue;
            }
            let (live, absent) = self.screen_view(ob, list, d);
            let Some(m) = self.model.as_ref() else { return predicted };
            let new = live.iter().filter(|meta| m.window(meta.id).is_none()).count();
            for meta in &live {
                self.unplaced.remove(&meta.id.0);
            }
            let hold = self.macos_after_display_change();
            let carried = self.slide_carried(now);
            let Some(m) = self.model.as_mut() else { return predicted };
            let before = m.current_space(d.id);
            m.set_hold_arrivals(hold);
            m.set_slide_carried(carried);
            let mut actions = m.arrive(d.id, &live, &absent, &hints);
            m.set_hold_arrivals(false);
            let after = m.current_space(d.id);
            let note = m.last_arrival().clone();
            self.unplaced.extend(note.held.iter().map(|w| w.0));
            if before != after || new > 0 || note.moved > 0 || note.rebuilt.is_some() {
                tracing::info!(
                    display = d.id.0,
                    from = before.0,
                    to = after.0,
                    on_screen = live.len(),
                    new,
                    rebuilt = ?note.rebuilt,
                    restored = note.restored,
                    other_screen = note.other_screen,
                    moved = note.moved,
                    carried = note.carried,
                    "Space changed on another display"
                );
            }
            log_arrival_lets(&note);
            // A Desktop of this screen arranged ahead: confirmed or undone, in its batch.
            let (more, window) = self.settle_ahead(ob, list, d.id, after, now);
            actions.extend(more);
            predicted = predicted.or(window);
            self.execute(ob, actions);
        }
        predicted
    }

    /// True while a settled Space change waits before the Desktop is recognized from `list`:
    /// Accessibility lists none of the windows on screen the model knows (every app read
    /// again at most once per [`LOCK_POLL`]; the wait at most [`AX_SILENT_MAX`]), or, less
    /// than [`MIXED_WAIT`] after the notice, the list shows two Desktops at once
    /// ([`Model::shows_two_desktops`]: one still sliding). A Desktop whose windows the model
    /// does not know yet is never held for silence: Accessibility may simply not list some
    /// app's windows, and the wait would come back at every visit. The screen view read for
    /// the check is left in `view` for the arrival.
    fn arrival_waits(
        &mut self,
        ob: &mut Observer,
        list: &[windows::CgWindow],
        main: Display,
        now: Instant,
        view: &mut Option<ScreenView>,
    ) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let (known, listed) = session::counts(
            list,
            |w| ob.observes(w.pid) && m.window(WindowId(w.id)).is_some(),
            |id| ob.window(id).is_some(),
        );
        if session::arrival_silent(known, listed) {
            let since = *self.ax_silent_since.get_or_insert(now);
            if now.duration_since(since) < AX_SILENT_MAX {
                self.phase.feed(phase::Event::Held(Hold::Silence), now);
                if since == now {
                    tracing::info!(known_on_screen = known, "Accessibility lists none of the known windows on screen; the Desktop is recognized once it does");
                }
                if self.ax_silent_rescan.is_none_or(|t| now.duration_since(t) >= LOCK_POLL) {
                    self.ax_silent_rescan = Some(now);
                    let found = ob.rescan_all();
                    self.unplaced.extend(found);
                }
                return true;
            }
            if self.ax_silent_since.take().is_some() {
                self.ax_silent_rescan = None;
                tracing::warn!(known_on_screen = known, waited_s = AX_SILENT_MAX.as_secs(), "Accessibility still lists none of the known windows on screen; recognizing the Desktop from what it lists");
            }
            return false;
        }
        self.ax_silent_since = None;
        self.ax_silent_rescan = None;
        let since_notice = self.phase.since_change(now).unwrap_or(MIXED_WAIT);
        if since_notice < MIXED_WAIT {
            let (live, absent) = self.screen_view(ob, list, main);
            let two = self.model.as_ref().is_some_and(|m| m.shows_two_desktops(main.id, &live, &absent));
            if two {
                self.phase.feed(phase::Event::Held(Hold::TwoDesktops), now);
                tracing::debug!(on_screen = live.len(), since_notice_ms = since_notice.as_millis() as u64, "the window list shows two Desktops at once (one still sliding); read again");
                return true;
            }
            *view = Some((live, absent));
        }
        false
    }

    /// The login session asked whether the screen is locked (`force`, or [`LOCK_POLL`] since
    /// the last time); a change is logged and resets what was counted. True while locked.
    fn poll_lock(&mut self, now: Instant, force: bool) -> bool {
        if !force && self.lock_checked.is_some_and(|t| now.duration_since(t) < LOCK_POLL) {
            return self.locked;
        }
        self.lock_checked = Some(now);
        let locked = session::screen_locked();
        if locked {
            session::saw_rest(now);
        }
        if locked != self.locked {
            self.locked = locked;
            // What was counted towards windows gone or moved is looked at afresh.
            self.absent.clear();
            self.foreign.clear();
            if locked {
                tracing::info!("the screen is locked: no Space is inferred and state.json is not written until it unlocks");
            } else {
                tracing::info!("the screen is unlocked: the Desktop is looked at again after a settle");
                self.phase.feed(phase::Event::Unlocked, now);
                self.space_checked = now;
            }
        }
        locked
    }

    /// At launch: true when stages can be built from what the app sees now (`list`). While
    /// the screen is locked, or Accessibility lists none of the windows on screen (for at
    /// most [`AX_SILENT_MAX`]), the build waits ([`Self::build_wait`]; the tick looks again):
    /// a Desktop recognized from nothing would leave its windows to be filed as new ones
    /// once they show, and the Desktops saved in `state.json` would be saved again under it.
    fn can_build(&mut self, ob: &Observer, list: &[windows::CgWindow]) -> bool {
        let now = Instant::now();
        let locked = self.poll_lock(now, true);
        let (server, ax) = session::counts(list, |w| ob.observes(w.pid), |id| ob.window(id).is_some());
        let blind = session::blindness(locked, server, ax);
        match session::wait_step(blind, self.build_wait, now, AX_SILENT_MAX) {
            session::Step::Wait(since) => {
                if let Some(why) = blind.filter(|_| since == now) {
                    tracing::info!(why = why.name(), on_screen = server, "the Desktop cannot be seen: stages are built once it can; state.json is left as it is");
                }
                self.build_wait = blind.map(|b| (b, since));
                false
            }
            session::Step::GiveUp => {
                tracing::warn!(on_screen = server, waited_s = AX_SILENT_MAX.as_secs(), "Accessibility still lists none of the windows on screen; building stages from what it lists");
                self.build_wait = None;
                true
            }
            session::Step::Build => {
                if let Some((why, since)) = self.build_wait.take() {
                    tracing::info!(was = why.name(), waited_ms = now.duration_since(since).as_millis() as u64, "the Desktop can be seen: building stages");
                }
                self.screens_ready(now)
            }
        }
    }

    /// At launch: true when the screens are there to build on ([`early_start::screens_wait`]):
    /// the displays read, and, at the session's start, every screen `state.json` saved stages
    /// on connected, each waited for a moment at most. The tick looks again meanwhile.
    fn screens_ready(&mut self, now: Instant) -> bool {
        let tried = *self.build_tried.get_or_insert(now);
        let read = !self.displays.is_empty();
        let config = self.store.config();
        let missing = if read && config.stages.restore_after_restart {
            let connected: Vec<_> = self.displays.iter().map(|d| d.key).collect();
            std::fs::read_to_string(paths::state_file())
                .ok()
                .and_then(|t| StateFile::from_json(&t).ok())
                .map_or(0, |s| early_start::missing_screens(&s, &connected))
        } else {
            0
        };
        let since_login = super::apps::login_started().map(|l| l.elapsed());
        let wait = early_start::screens_wait(read, missing, since_login, now.duration_since(tried), config.displays.main_only);
        match (wait, self.screens_wait) {
            (Some(w), was) => {
                if was != Some(w) {
                    tracing::info!(why = w.name(), missing, login_ago_s = since_login.map(|d| d.as_secs()), "the launch waits a moment for the screens before building stages");
                }
                self.screens_wait = Some(w);
                false
            }
            (None, Some(was)) => {
                self.screens_wait = None;
                tracing::info!(was = was.name(), missing, waited_ms = now.duration_since(tried).as_millis() as u64, "building stages on the screens there are");
                true
            }
            (None, None) => true,
        }
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
            || self.phase.space_settling(now)
        {
            return;
        }
        self.space_checked = now;
        let Some(m) = self.model.as_ref() else { return };
        let main = *m.main_display();
        // The tick's read, shared with its other duties ([`windows::wake_list`]).
        let list = windows::wake_list();
        // The other screens first (round displays-2): each is judged on its own, whatever
        // the main screen shows.
        self.check_other_screens(ob, &list, now);
        // Mission Control, App Exposé, Show Desktop: the list shows their layout (other
        // Desktops' windows, windows slid aside), not the Desktop. Nothing is inferred while
        // one shows and for a settle after; a Desktop clicked there comes with its
        // notification.
        if self.look_for_overview(&list, main.frame, now) {
            return;
        }
        if !self.phase.fullscreen()
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
            Verdict::FullScreen if !self.phase.fullscreen() => {
                self.on_space_changed(ob);
                return;
            }
            Verdict::Desktop if self.phase.fullscreen() => {
                self.on_space_changed(ob);
                return;
            }
            _ => {}
        }
        if self.phase.fullscreen() {
            return;
        }
        // A window tab selected takes the place of the tab it hides, before the tab left is
        // counted missing or the tab selected is taken for a new window (round tab-lost).
        self.take_tabs(ob, &list);
        let shown = self.present_in(&list);
        let shown_main = self.shown_managed(&list);
        if self.active_shown_changed(&shown) {
            tracing::debug!("a shown window was hidden or came back: the strip looks again");
            self.strip_dirty = true;
        }
        self.unplaced.retain(|id| ob.window(*id).is_some());
        let showing: Vec<u32> = self.unplaced.iter().copied().filter(|id| shown_main.contains(id)).collect();
        for id in showing {
            self.on_found(ob, id);
        }
        if self.main_only() {
            self.leave_other_displays(ob, &shown, &shown_main);
        } else {
            self.follow_display_moves(ob, &list);
        }
        self.leave_off_screen(ob, &list);
        // A managed window off screen whose handle died: found again, or let go if closed.
        self.check_stale_handles(ob, &list);

        let Some(m) = self.model.as_ref() else { return };
        let space = m.current_space(main.id);
        let standard = |w: &&crate::engine::model::WindowState| w.meta.role == WindowRole::Standard && !w.sticky;
        // The same windows of other Spaces on screen at two checks a settle apart (never one
        // listing): a Space change went unseen, the arrival guessed wrong, or a window was
        // moved here. Arrive again.
        let mut foreign: Vec<u32> = m
            .windows()
            .filter(standard)
            .filter(|w| w.space != space && m.display_of_space(w.space) == main.id && shown.contains(&w.meta.id.0))
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
            // Judged closed just now (its handle dead): it goes as destroyed, not elsewhere.
            .filter(|w| !ob.noted_closed(w.meta.id.0))
            .map(|w| (w.meta.id.0, w.meta.pid))
            .collect();
        // In full screen, a window is on a Space of its own but still this Desktop's: it is
        // not gone (it comes back here when it leaves full screen).
        let missing: Vec<(u32, i32)> = missing.into_iter().filter(|&(id, _)| ob.window(id).is_some()).collect();
        let excused = self.off_screen_for_a_reason(ob, &missing);
        let missing: Vec<u32> = missing.into_iter().map(|(id, _)| id).filter(|id| !excused.contains(id)).collect();
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
            let group = m.group_of(WindowId(id));
            let kept = self.parked_entries_of(&group);
            let Some(m) = self.model.as_mut() else { return };
            let (actions, forgotten) = m.window_left_space(WindowId(id));
            self.execute(ob, actions);
            self.let_go_hidden(ob, kept, LetGo::Other);
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
            let group = m.group_of(WindowId(id));
            let kept = self.parked_entries_of(&group);
            let Some(m) = self.model.as_mut() else { return };
            let (actions, forgotten) = m.window_left_space(WindowId(id));
            self.execute(ob, actions);
            self.let_go_hidden(ob, kept, LetGo::Other);
            self.unplaced.extend(forgotten.into_iter().map(|w| w.0).filter(|w| ob.window(*w).is_some()));
        }
    }

    /// Managed standard windows that show mostly on another display than the one the model
    /// has them on ([`display_of`]), in two checks in a row (a drag across the seam; macOS
    /// moving windows back to a screen that came back), move there
    /// ([`Model::windows_moved_to_display`]: a stage of its own there, in front; one macOS
    /// lists in that display's park zone a card there, still). A parked window in its own
    /// display's park zone never counts, whatever shows of it on a screen beyond. Nothing
    /// moves while the left button is down ([`display_moves_now`]): a slow drag across the
    /// seam changes stage when the window is let go. Within
    /// [`DISPLAY_SETTLE`] of a display change the moves are macOS's, not the user's drag, and
    /// the log says so. The default (round displays); with `[displays] main_only`
    /// [`Self::leave_other_displays`] runs instead.
    fn follow_display_moves(&mut self, ob: &Observer, list: &[windows::CgWindow]) {
        let Some(m) = self.model.as_ref() else { return };
        let displays = m.displays().to_vec();
        if displays.len() < 2 {
            self.elsewhere.clear();
            return;
        }
        let rects: Vec<(DisplayId, Rect)> = displays.iter().map(|d| (d.id, d.frame)).collect();
        let mut away: Vec<(u32, DisplayId, Rect)> = Vec::new();
        for w in list.iter().filter(|w| w.onscreen && w.layer == 0) {
            let Some(ws) = m.window(WindowId(w.id)) else { continue };
            if ws.meta.role != WindowRole::Standard {
                continue;
            }
            if ws.parked && m.listed_hidden(ws.meta.display, w.bounds) {
                continue;
            }
            // A parked one in another screen's park zone is that screen's (macOS put it
            // back at its old corner), however little of it shows there.
            let zone = if ws.parked { m.park_zone_display(w.bounds, ws.meta.display) } else { None };
            let Some(d) = zone.or_else(|| display_of(w.bounds, &rects)) else { continue };
            if d != ws.meta.display {
                away.push((w.id, d, w.bounds));
            }
        }
        self.elsewhere.retain(|id, _| away.iter().any(|(a, _, _)| a == id));
        let mut moves: Vec<(WindowId, DisplayId, Rect)> = Vec::new();
        for (id, d, bounds) in away {
            let n = self.elsewhere.entry(id).or_insert(0);
            *n += 1;
            if *n >= SPACE_CHECKS {
                moves.push((WindowId(id), d, bounds));
            }
        }
        // A drag across the seam goes on: decided at the first check after the button comes
        // up (its count kept), where the window is let go.
        if moves.is_empty() || !display_moves_now(input::left_button_down(), self.displays_settling()) {
            return;
        }
        for (id, _, _) in &moves {
            self.elsewhere.remove(&id.0);
        }
        let z: Vec<WindowId> = list.iter().filter(|w| w.onscreen && w.layer == 0).map(|w| WindowId(w.id)).collect();
        self.take_display_moves(ob, &moves, &z);
    }

    /// True for [`DISPLAY_SETTLE`] after a display came or went: what moves now is macOS's.
    fn displays_settling(&self) -> bool {
        self.displays_changed_at.is_some_and(|t| Instant::now().duration_since(t) < DISPLAY_SETTLE)
    }

    /// True when an activation or focus heard now is macOS's after a display change: within
    /// [`DISPLAY_SETTLE`] of it with no click or key press since
    /// ([`crate::engine::displays::macos_after_display_change`]).
    fn macos_after_display_change(&self) -> bool {
        let since = self.displays_changed_at.map(|t| Instant::now().duration_since(t));
        crate::engine::displays::macos_after_display_change(since, input::since_last_press(), DISPLAY_SETTLE)
    }

    /// Windows listed on another display than the model has them on, each with that display
    /// and its listed frame, go there ([`Model::windows_moved_to_display`]); `z` is the
    /// window list's front-to-back order. Logged as macOS's within [`DISPLAY_SETTLE`] of a
    /// display change, else as the user's drag.
    fn take_display_moves(&mut self, ob: &Observer, moves: &[(WindowId, DisplayId, Rect)], z: &[WindowId]) {
        // What a card press read no longer holds.
        self.pressed_obs = None;
        let by_macos = self.displays_settling();
        let Some(m) = self.model.as_mut() else { return };
        let parked_before: Vec<bool> = moves.iter().map(|(id, _, _)| m.window(*id).is_some_and(|w| w.parked)).collect();
        let (actions, forgotten) = m.windows_moved_to_display(moves, z);
        for ((id, d, at), was_parked) in moves.iter().zip(parked_before) {
            let card = m.window(*id).is_some_and(|w| w.parked);
            if by_macos {
                tracing::info!(
                    id = id.0,
                    display = d.0,
                    %at,
                    was_parked,
                    card,
                    "window macOS put on a display that changed (not the user's drag): its stage is that display's again; shown where macOS put it, or a card still (the lid's rule)"
                );
            } else {
                tracing::info!(id = id.0, display = d.0, %at, card, "window moved to another display: a stage of its own there, in front (Apple's rule)");
            }
        }
        self.execute(ob, actions);
        self.unplaced.extend(forgotten.into_iter().map(|w| w.0).filter(|w| ob.window(*w).is_some()));
    }

    /// Managed windows its app moved off every display ([`off_screen_not_ours`]; one we
    /// parked beyond the edges is not judged), listed so in two checks in a row, are let go
    /// where they are ([`Model::window_went_off_screen`]): never parked, never brought back,
    /// so no restore puts on a display a window its app keeps off them. They wait in
    /// `unplaced` and come back as new windows once they show on the display. A user cannot
    /// drag a window there (macOS keeps a title bar on screen), so no user's window is let go.
    fn leave_off_screen(&mut self, ob: &Observer, list: &[windows::CgWindow]) {
        let Some(m) = self.model.as_ref() else { return };
        let displays = desktop::cg_display_bounds();
        let away: Vec<u32> = list
            .iter()
            .filter(|w| w.onscreen && w.layer == 0 && m.window(WindowId(w.id)).is_some())
            .filter(|w| off_screen_not_ours(w.bounds, &displays, true))
            .map(|w| w.id)
            .collect();
        self.off_screen.retain(|id, _| away.contains(id));
        let mut leave = Vec::new();
        for id in away {
            let n = self.off_screen.entry(id).or_insert(0);
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
            self.off_screen.remove(&id);
            let Some(m) = self.model.as_mut() else { return };
            let Some(w) = m.window(WindowId(id)) else { continue };
            let at = list.iter().find(|l| l.id == id).map(|l| l.bounds).unwrap_or_default();
            tracing::info!(id, %at, parked = w.parked, "window moved off every display by its app (not the user's: a headless or offscreen window); let go where it is, left alone until it shows on a display");
            let group = if w.meta.role == WindowRole::Standard { m.group_of(WindowId(id)) } else { vec![id] };
            let kept = self.parked_entries_of(&group);
            let Some(m) = self.model.as_mut() else { return };
            let (actions, forgotten) = m.window_went_off_screen(WindowId(id));
            self.execute(ob, actions);
            self.let_go_hidden(ob, kept, LetGo::Other);
            self.unplaced.extend(forgotten.into_iter().map(|w| w.0).filter(|w| ob.window(*w).is_some()));
        }
    }

    /// Offers windows waiting in `unplaced` (`ids`, an app's at its activation or a focus) to
    /// [`Self::on_found`]: those the window server lists on a managed screen now
    /// ([`offered_at_notice`]), one list read for them all, none when there are none.
    fn offer_waiting(&mut self, ob: &mut Observer, ids: Vec<u32>) {
        if ids.is_empty() {
            return;
        }
        let listed = self.shown_managed(&windows::list_windows());
        for id in offered_at_notice(ids, &listed) {
            self.on_found(ob, id);
        }
    }

    /// Window `id`, just found and not listed on screen yet (`listed`: its frame in the
    /// window server's list, if there), is looked at on every fast tick for [`FRESH_LOOK`]
    /// from its first wait (a later offer never lengthens it) when it may be about to show
    /// ([`looks_soon`]).
    fn look_soon(&mut self, ob: &Observer, id: u32, listed: Option<Rect>) {
        let launched = ob.window(id).and_then(|w| ob.launched_at(w.pid)).map(|at| at.elapsed());
        if looks_soon(listed, launched) {
            self.fresh_unplaced.entry(id).or_insert_with(|| Instant::now() + FRESH_LOOK);
        }
    }

    /// The fast tick's look at windows of [`Self::look_soon`]: those listed on a managed
    /// screen now are offered to [`Self::on_found`]; one past its time, gone, or placed by
    /// another path is dropped (the once-a-second check still offers it).
    pub(super) fn offer_fresh(&mut self, ob: &mut Observer, now: Instant) {
        if self.fresh_unplaced.is_empty() {
            return;
        }
        let unplaced = &self.unplaced;
        self.fresh_unplaced.retain(|id, until| now < *until && unplaced.contains(id) && ob.window(*id).is_some());
        let ids: Vec<u32> = self.fresh_unplaced.keys().copied().collect();
        self.offer_waiting(ob, ids);
    }

    /// Whether a just-found window waits for its first frame ([`Self::offer_fresh`]).
    pub(super) fn fresh_waiting(&self) -> bool {
        !self.fresh_unplaced.is_empty()
    }

    /// App `pid` was just opened (its launch notice, WM thread): the engine may hold its
    /// new windows on their way in, so that each is placed before it first shows
    /// ([`WindowEngine::app_launched`]; the Spaces engine). Only for an app the arrangement
    /// takes in, while windows are arranged.
    pub fn app_launched(&self, ob: &Observer, pid: i32) {
        if self.model.is_some() && !self.locked && ob.wants(pid) {
            self.engine.app_launched(pid);
        }
    }

    /// Window `id`, held by the engine on its way in, waits instead to show as it would
    /// have (`on_found` does not place it now): it is let out unplaced, and placed once it
    /// shows, as any window is.
    fn new_window_waits(&self, id: u32) {
        if self.engine.about_to_show(WindowId(id)) {
            self.engine.let_new_out(WindowId(id), false);
        }
    }

    /// A window appeared after launch: the rules decide where it goes. One that is not on
    /// screen (another Space, minimized, app hidden; or any window while a full-screen app's
    /// Space shows) waits in `unplaced` until it shows, and a switch's windows held behind
    /// their snapshots stay held: only a window placed now brings them in first.
    ///
    /// A window the engine holds on its way in ([`WindowEngine::about_to_show`]: the Spaces
    /// engine, a just-opened app's) counts as shown: it is placed now, its stage's switch
    /// hides the stage left behind, and then it is let out where it shows
    /// ([`WindowEngine::let_new_out`]), so it is never on screen over the stage being left.
    fn on_found(&mut self, ob: &mut Observer, id: u32) {
        if self.model.as_ref().is_none_or(|m| m.window(WindowId(id)).is_some()) {
            return;
        }
        let about_to_show = self.engine.about_to_show(WindowId(id));
        self.ax_window_found(ob, Instant::now());
        let displays = desktop::cg_display_bounds();
        if let Some(w) = windows::describe(&[id]).into_iter().find(|w| w.onscreen && w.layer == 0)
            && off_screen_not_ours(w.bounds, &displays, false)
        {
            // A headless browser's or an offscreen render window ([`on_main_screen`]): not the
            // user's, and no sign its app is reopening windows. Taken in as new once it shows.
            if self.unplaced.insert(id) {
                tracing::info!(id, at = %w.bounds, "window off every display: not the user's (a headless or offscreen window); left alone until it shows on a display");
            }
            return;
        }
        if let Some(bundle) = ob.window(id).and_then(|w| ob.bundle_id(w.pid)).map(str::to_string) {
            // A new window: its app is not done reopening windows yet.
            self.quiet.window(&bundle, Instant::now());
        }
        let list = windows::list_windows();
        let shown = self.shown_managed(&list);
        // The screen the window shows on: its own overview and full-screen checks gate it
        // (round displays-2), so a full-screen app on one screen never stops windows arriving
        // on another. A Space change settling holds every screen (the arrival places it).
        let screen = list.iter().find(|w| w.id == id).map(|w| w.bounds).map(|b| self.display_at(b));
        let listed = list.iter().find(|w| w.id == id).map(|w| w.bounds);
        let Some(screen) = screen.and_then(|s| self.managed_displays().into_iter().find(|d| d.id == s)) else {
            if self.unplaced.insert(id) {
                tracing::debug!(id, "window not on screen; it is placed once it shows");
            }
            self.new_window_waits(id);
            self.look_soon(ob, id, listed);
            return;
        };
        let is_main = screen.id == self.main_display_id();
        let frozen = self.locked || self.phase.changing() || (is_main && self.phase.fullscreen());
        if frozen || !(shown.contains(&id) || about_to_show) {
            if self.unplaced.insert(id) {
                tracing::debug!(id, "window not on screen; it is placed once it shows");
            }
            self.new_window_waits(id);
            if !frozen {
                self.look_soon(ob, id, listed);
            }
            return;
        }
        if about_to_show {
            tracing::debug!(id, "a just-opened app's window, held on its way in: placed before it shows");
        }
        // It shows: a switch's windows still held behind their snapshots come to their frames
        // first, and the window server is read again after them.
        let list = if self.pending.is_some() {
            self.flush_pending(ob);
            windows::list_windows()
        } else {
            list
        };
        let now = Instant::now();
        if is_main {
            if self.look_for_overview(&list, screen.frame, now) {
                // App Exposé lists other Desktops' windows on screen: placed once it has closed
                // (the once-a-second check offers it again).
                if self.unplaced.insert(id) {
                    tracing::debug!(id, "Mission Control, App Exposé or Show Desktop shows; the window is placed once it has closed");
                }
                self.new_window_waits(id);
                return;
            }
            if self.settle.step(self.screen_seen(ob, &list, screen.frame), now) != Verdict::Desktop {
                // Most likely the full-screen animation's stand-in window (it goes away once the
                // animation is over), or a window on a full-screen Space not handled yet.
                if self.unplaced.insert(id) {
                    tracing::debug!(id, "a full-screen animation or Space shows; the window is placed once a Desktop shows");
                }
                self.new_window_waits(id);
                return;
            }
        } else {
            match self.other_screen_seen(ob, &list, screen, now) {
                OtherSeen::Desktop => {}
                seen => {
                    if self.unplaced.insert(id) {
                        tracing::debug!(id, screen = screen.id.0, ?seen, "the window's screen shows no Desktop now; the window is placed once it does");
                    }
                    self.new_window_waits(id);
                    return;
                }
            }
        }
        if self.macos_after_display_change() {
            // A display change under way, no click or key since: macOS lists windows of
            // other Desktops for a moment (11-05 20:59:03.9, the lid closed: a window of the
            // 5K's other Desktop joined its app's stage here and sent the laptop's front to
            // a card). Placed once the change settles, if it still shows; never a switch now.
            if self.unplaced.insert(id) {
                tracing::info!(id, "a window found after a display change (no click or key since): placed once the change settles, if it still shows");
            }
            self.new_window_waits(id);
            return;
        }
        if self.space_in_doubt(ob, &self.present_in(&list), screen.id) {
            // Its notification came before the Space change's: placing it now would put it
            // (and maybe a stage switch, a focus) on the Space being left.
            if self.unplaced.insert(id) {
                tracing::debug!(id, "the window list disagrees with the Space shown; the window is placed on arrival");
            }
            self.new_window_waits(id);
            return;
        }
        // A window tab selected (an activation or a focus may offer it before the Space
        // check): it takes the place of the tab it hides, never a new window's.
        if self.take_tabs(ob, &list) && self.model.as_ref().is_some_and(|m| m.window(WindowId(id)).is_some()) {
            return;
        }
        let waited = self.unplaced.remove(&id);
        if let Some(until) = self.fresh_unplaced.remove(&id) {
            let ms = FRESH_LOOK.saturating_sub(until.saturating_duration_since(Instant::now())).as_millis() as u64;
            tracing::debug!(id, ms, "a new window shows: placed on the fast tick's look");
        }
        if waited {
            // It waited off screen: one read then as a dialog is read again (a window minimized
            // then or now is standard: taken for a child, it would follow Apple's rule for a
            // child, never its saved stage).
            ob.recheck_kind(id);
        }
        let z = windows::z_order(&list);
        let Some(mut meta) = self.meta(ob, id, &z) else {
            self.new_window_waits(id);
            return;
        };
        if let Some(title) = ob.window(id).and_then(|w| w.element.title().ok()) {
            meta.title = title;
        }
        let mut decision = match meta.role {
            WindowRole::Standard => self.store.config().rules.decide(&meta.bundle_id, &meta.title),
            WindowRole::Child { .. } => Decision::default(),
        };
        if meta.role == WindowRole::Standard {
            // Shift and `[stages] new_apps_join_current` (`Decision::for_new_window`).
            let new_app = self.model.as_ref().is_some_and(|m| m.app_is_new(meta.pid));
            let config = self.store.config();
            // While the restore window is open an app's window is one it reopens, not one
            // the user opened: the saved stages and Apple's rule place it, never the key
            // (the model ignores it then too, `Model::add_window`).
            let restoring = self.model.as_ref().is_some_and(Model::restoring);
            let join_current = config.stages.new_apps_join_current && !restoring;
            let sugar = &config.sugar;
            let mut shift = false;
            if sugar.shift_brings_here && decision.action.shift_counts() {
                let within = Duration::from_millis(if new_app { sugar.launch_window_ms } else { sugar.activation_window_ms });
                shift = self.triggers.shift_here(Instant::now(), within, input::live_modifiers());
                if shift && new_app {
                    self.triggers.consume();
                }
            }
            let effect;
            (decision, effect) = decision.for_new_window(new_app, join_current, shift);
            match effect {
                ShiftEffect::BringsHere => tracing::info!(id, "Shift held: the new window joins the current stage"),
                ShiftEffect::OwnStage => tracing::info!(id, "Shift held: the newly opened app gets a stage of its own"),
                ShiftEffect::None if decision.new_app_joins_current => {
                    tracing::debug!(id, "a newly opened app: asked to join the current stage ([stages] new_apps_join_current)")
                }
                ShiftEffect::None => {}
            }
        }
        if decision.action == RuleAction::Ignore {
            tracing::debug!(id, "rule: ignore");
            self.new_window_waits(id);
            return;
        }
        counters::bump(Count::NewWindow);
        tracing::info!(id, action = %decision.action, placement = %decision.placement, "new window");
        let obs = self.observe(ob);
        let transitions_before = self.next_transition;
        if let Some(m) = self.model.as_mut() {
            let actions = m.add_window(meta, &decision, &obs);
            let back = m.last_pending().filter(|n| n.window.0 == id);
            if let Some(note) = back {
                tracing::info!(id, grade = note.grade.name(), points = note.points, parked = note.parked, switched = note.switched, adopted = ?note.adopted, from_other = note.from_other, "restore: back in its saved stage");
            }
            let settling = m.settling();
            if let (Some(clock), true) = (self.settling.as_mut(), back.is_some()) {
                // The restore is still under way.
                clock.placed(Instant::now());
            }
            if settling {
                // While the login settles nothing animates: windows go to their stages quietly.
                self.execute(ob, actions);
            } else {
                self.execute_animated(ob, actions);
            }
        }
        if about_to_show && self.engine.about_to_show(WindowId(id)) {
            // Still held on its way in: the batch has hidden the stage it leaves behind (a
            // window joining a stage not shown went into that stage's Space instead).
            let shown_by_model = self.model.as_ref().and_then(|m| m.window(WindowId(id))).is_none_or(|w| !w.parked);
            self.engine.let_new_out(WindowId(id), shown_by_model);
        }
        if about_to_show && self.next_transition != transitions_before && self.model.as_ref().and_then(|m| m.window(WindowId(id))).is_some_and(|w| !w.parked) {
            // Out where it shows now (let out above, or shown by its stage's switch in the
            // batch), while that switch still moves behind pictures above every window: the
            // transition could not name it to pass under while it waited off screen, so the
            // strip's overlay goes under it now.
            self.under_note = Some((self.next_transition, id));
            self.present();
        }
        // The measure of round login-observe: from found (tracked) to placed.
        if let Some(w) = ob.window(id) {
            tracing::debug!(id, pid = w.pid, ms = w.found_at.elapsed().as_millis() as u64, waited_off_screen = waited, "window placed: ms since it was found");
        }
    }

    // ----- commands ----------------------------------------------------------------------

    /// Gives the model how many Desktops macOS keeps for each screen (`com.apple.spaces`,
    /// read only; [`super::desktop_counts`]), logged when it differs from what it had.
    fn read_desktop_counts(model: &mut Model) {
        let counts = super::desktop_counts::desktop_counts(model.displays());
        let changed = model.displays().iter().any(|d| counts.iter().find(|c| c.0 == d.key).map(|c| c.1) != model.desktops_on(d.id));
        if changed {
            let named: Vec<String> = counts.iter().map(|(k, n)| format!("{}={n}", k.text())).collect();
            tracing::info!(screens = %named.join(" "), "Desktops per screen, as macOS keeps them (com.apple.spaces, read only)");
        }
        model.set_desktop_counts(counts);
    }

    /// The displays changed (or were first reported): every screen, the main one first
    /// (round displays; the main one alone with `[displays] main_only`). Every screen's park
    /// zone reaches the safety paths, and each screen's strip gets its own capacity. `heard`:
    /// when the main thread heard the change, where its settle begins ([`DISPLAY_SETTLE`]),
    /// however long this thread took to come to it.
    pub fn set_displays(&mut self, ob: &mut Observer, displays: Vec<Display>, heard: Instant) {
        self.flush_pending(ob);
        let mut displays = displays;
        let config = self.store.config().clone();
        if self.main_only() {
            displays.truncate(1);
        }
        if displays.is_empty() {
            return;
        }
        safety::set_displays(&displays);
        let placed = |ds: &[Display]| ds.iter().map(|d| (d.id, d.frame)).collect::<Vec<_>>();
        if crate::engine::displays::displays_changed(&placed(&self.displays), &placed(&displays)) {
            // A display came, went or moved (never the launch's first reading): what the
            // checks counted as moves before it is stale, and the moves, activations and
            // focuses that follow are macOS's ([`DISPLAY_SETTLE`]).
            let settling = self.displays_settling();
            self.displays_changed_at = Some(heard);
            self.elsewhere.clear();
            let added: Vec<DisplayId> = displays.iter().filter(|d| !self.displays.iter().any(|o| o.id == d.id)).map(|d| d.id).collect();
            let connected: Vec<DisplayId> = displays.iter().map(|d| d.id).collect();
            self.displays_added = returned_screens(&self.displays_added, settling, added, &connected);
        }
        self.displays = displays.clone();
        let mut actions = Vec::new();
        if let Some(m) = self.model.as_mut() {
            // Each strip's capacity first: a removed display's stages are counted against
            // the remaining one's capacity as it is now (its Dock, its size), not as it was.
            for d in displays.iter().filter(|d| d.visible.is_finite()) {
                m.set_display_capacity(d.id, strip_capacity(d.visible, &config));
            }
            actions = m.set_displays(displays);
            Self::read_desktop_counts(m);
        }
        if self.model.is_some() {
            self.sync_fit();
            self.execute(ob, actions);
        }
        self.hidden_changed();
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

    /// A left mouse-down in another app at `p` (at `stamp`: [`InputEvent::stamp`]), the
    /// `clicks`th click of a multiple click: on the wallpaper it reveals the desktop on its
    /// release ([`wallpaper_click`]: ours only with "Only in Stage Manager"; a double click is
    /// one click; a drag or a long press reveals nothing: [`Self::wallpaper_released`]). True
    /// when it was a wallpaper click, whoever reveals: Finder's activation and focus that
    /// follow are its echo ([`DesktopEcho`]) either way.
    pub fn mouse_down(&mut self, list: &[windows::CgWindow], p: Point, clicks: i64, stamp: f64) -> bool {
        if self.model.is_none() {
            return false;
        }
        // The screen clicked: its own Desktop is revealed (round displays), unless a
        // full-screen app's Space shows there (another screen's stops nothing here).
        let Some(screen) = self.managed_displays().into_iter().find(|d| d.frame.contains(p)) else { return false };
        if self.frozen_on(screen.id) {
            return false;
        }
        // Our own windows (the strip below windows, the motion overlay above them) never
        // receive other apps' clicks, so they never hide the wallpaper.
        let own = std::process::id() as i32;
        let list: Vec<_> = list.iter().filter(|w| w.pid != own).cloned().collect();
        if !desktop::is_wallpaper_click(&list, p, &self.dock_bar_now()) {
            return false;
        }
        // Just outside a window, in its resize band, the press takes the window's edge (11-09:
        // VS Code's right edge, 0.3 pt out, parked mid-resize): the window's, not the desktop's.
        if desktop_click::in_any_resize_band(p, desktop::resizable_frames(&list)) {
            tracing::info!(x = p.x, y = p.y, screen = screen.id.0, "a press in a window's resize band: its edge, no wallpaper click");
            return false;
        }
        // The desktop is Finder's either way: its activation and focus while the press is down
        // and just after its release are echoes, whether it ends as a click or not.
        self.desktop_echo.desktop_pressed(Instant::now());
        // A press on a desktop file or folder (to select it, drag it onto a window, open it) is
        // Finder's, as on Apple's: nothing reveals, ours or macOS's.
        let icons = self.icons_for_press(p, Instant::now());
        let known = icons.as_deref().map_or(desktop_click::Icons::Unknown, desktop_click::Icons::Read);
        if desktop_click::desktop_press(p, known) == desktop_click::DesktopPress::Icon {
            tracing::info!(x = p.x, y = p.y, screen = screen.id.0, "a press on a desktop icon: Finder's, no wallpaper click");
            return true;
        }
        match wallpaper_click(&self.settings, clicks) {
            WallpaperClick::Toggle => {
                // Apple's reveals on the release of a short still click, never on the press:
                // nothing parks while the press may still turn into a drag (a resize, a
                // rubber-band selection) or be held.
                tracing::info!(x = p.x, y = p.y, screen = screen.id.0, "wallpaper press: the reveal waits for the release");
                self.wallpaper_press = Some(WallpaperPress { at: p, screen: screen.id, stamp });
            }
            WallpaperClick::LeftToMacOs => {
                self.macos_reveal_at = Some(Instant::now());
                // Opening Show Desktop (not closing it): the patch goes at once.
                if self.overview.expect_show_desktop(Instant::now()) {
                    self.look_at_screen();
                }
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

    /// The release of a wallpaper press ([`WallpaperPress`]) at `up` (at `stamp`): where it was
    /// pressed and within [`desktop_click::CLICK_HOLD`], the click reveals now; moved, it was a
    /// drag (a resize, a rubber-band selection), and held longer it was no click (Apple's
    /// reveals nothing for either).
    fn wallpaper_released(&mut self, ob: &mut Observer, press: WallpaperPress, up: Point, stamp: f64) {
        if self.model.is_none() || self.frozen_on(press.screen) {
            return;
        }
        let held = Duration::try_from_secs_f64(stamp - press.stamp).unwrap_or(Duration::ZERO);
        let held_ms = held.as_millis() as u64;
        if desktop_click::reveals_on_release(press.at, up, held) {
            tracing::info!(x = press.at.x, y = press.at.y, screen = press.screen.0, held_ms, on = "release", "wallpaper click");
            self.reveal_on(ob, Some(press.screen), true);
        } else if desktop_click::reveals_on_release(press.at, up, Duration::ZERO) {
            tracing::info!(x = up.x, y = up.y, held_ms, "a long press on the wallpaper: no click, nothing revealed");
        } else {
            tracing::info!(x = up.x, y = up.y, held_ms, "a drag from the wallpaper: nothing revealed");
        }
    }

    /// Reveals the desktop, or brings the revealed stage back (a second click). With
    /// `[strip] hide_while_desktop_shown` the strip hides while the desktop shows, and the
    /// windows move without the strip's animation (there is no card to grow from or shrink
    /// into).
    fn reveal(&mut self, ob: &mut Observer, animated: bool) -> usize {
        self.reveal_on(ob, None, animated)
    }

    /// [`Self::reveal`] on the Desktop `display` shows (a wallpaper click on that screen;
    /// `None`: the focused window's display, else the main one).
    fn reveal_on(&mut self, ob: &mut Observer, display: Option<DisplayId>, animated: bool) -> usize {
        let obs = self.observe(ob);
        let hide = self.store.config().strip.hide_while_desktop_shown;
        let hidden_before = self.strip_hidden_for_desktop && self.model.as_ref().is_some_and(Model::desktop_revealed);
        let Some(m) = self.model.as_mut() else { return 0 };
        let actions = m.reveal_desktop_on(display, &obs);
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
        let command = self.end_restore_for_hotkey(ob, command);
        self.flush_pending(ob);
        if command != HotkeyCommand::ToggleStrip && self.hotkey_frozen() {
            // Refused: nothing acted, so a focus held (and a Desktop arranged for it) stays.
            tracing::debug!(settling = self.phase.changing(), "a full-screen app's Space shows or a Space change is settling; the hotkey does nothing");
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

    /// [`Self::frozen_on`] the screen with the keyboard (the focused window's, asked only
    /// while a screen shows a full-screen app's Space).
    fn hotkey_frozen(&self) -> bool {
        if self.locked || self.phase.changing() {
            return true;
        }
        if !self.phase.fullscreen() && !self.screens.values().any(|w| w.fullscreen) {
            return false;
        }
        let list = windows::list_windows();
        self.frozen_on(self.keyboard_display(focused_window().map(WindowId), &list))
    }

    /// Ends the restore window for a hotkey; a stage number keeps meaning the stage the user
    /// saw under it when the window's end moves a voter (its stage may go: the number then
    /// names the stage its window went to; the review of 8I2).
    fn end_restore_for_hotkey(&mut self, ob: &mut Observer, command: HotkeyCommand) -> HotkeyCommand {
        let n = match command {
            HotkeyCommand::SwitchTo(n) | HotkeyCommand::SendTo(n) => n,
            _ => {
                self.end_restore(ob, "a hotkey", End::User);
                return command;
            }
        };
        let held = if self.settling.is_some() && self.model.is_some() {
            let list = windows::list_windows();
            let obs = self.observe_with(ob, &list);
            let keyboard = self.keyboard_display(obs.focused, &list);
            self.model.as_ref().map(|m| {
                // The Space each command acts on: the keyboard's screen's, or the focused
                // window's.
                let space = match command {
                    HotkeyCommand::SendTo(_) => obs.focused.and_then(|f| m.space_of(f)),
                    _ => None,
                }
                .unwrap_or_else(|| m.hotkey_space(keyboard));
                (space, m.number_ref(space, n))
            })
        } else {
            None
        };
        self.end_restore(ob, "a hotkey", End::User);
        let Some((space, r)) = held else { return command };
        let Some(now) = self.model.as_ref().map(|m| m.number_now(space, r)) else { return command };
        if now != n {
            tracing::debug!(was = n, now, "restore: the stage under the number moved as the restore window ended; the number follows it");
        }
        match command {
            HotkeyCommand::SwitchTo(_) => HotkeyCommand::SwitchTo(now),
            HotkeyCommand::SendTo(_) => HotkeyCommand::SendTo(now),
            other => other,
        }
    }

    /// `event` with each stage it names as it stands now ([`Model::follow_stage`]).
    fn follow_event(&self, event: StripEvent) -> StripEvent {
        let Some(m) = self.model.as_ref() else { return event };
        let f = |stage: u64| {
            let to = m.follow_stage(StageId(stage)).0;
            if to != stage {
                tracing::debug!(stage, to, "restore: the card's stage went as the restore window ended; its window's stage instead");
            }
            to
        };
        match event {
            StripEvent::CardClicked { stage, shift } => StripEvent::CardClicked { stage: f(stage), shift },
            StripEvent::CardPressed { stage } => StripEvent::CardPressed { stage: f(stage) },
            StripEvent::CardDropped { stage, window, x, y } => StripEvent::CardDropped { stage: f(stage), window, x, y },
            StripEvent::IconClicked { stage, pid } => StripEvent::IconClicked { stage: f(stage), pid },
            StripEvent::CardMoved { stage, before } => StripEvent::CardMoved { stage: f(stage), before: before.map(f) },
            StripEvent::CardMerged { stage, into } => StripEvent::CardMerged { stage: f(stage), into: f(into) },
            StripEvent::WindowToNewCard { stage, window, before } => StripEvent::WindowToNewCard { stage: f(stage), window, before: before.map(f) },
            StripEvent::WindowToCard { stage, window, into } => StripEvent::WindowToCard { stage: f(stage), window, into: f(into) },
            StripEvent::Unveil { id } => StripEvent::Unveil { id },
            StripEvent::StretchEnded { id, window, landed } => StripEvent::StretchEnded { id, window, landed },
        }
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
        if !matches!(event, StripEvent::Unveil { .. } | StripEvent::StretchEnded { .. }) {
            self.end_restore(ob, "a card click", End::User);
        }
        if let StripEvent::CardPressed { stage } = event {
            // Read the windows now, while the button is down (~100 ms before the release),
            // so the click's motion starts without waiting on AX. Not while a transition's
            // windows are still on their way (their frames are not the ones to save).
            let display = self.strip_event_display(&event);
            self.pressed_obs = (self.phase.switching().is_none() && !self.frozen_on(display) && self.model.is_some()).then(|| (Instant::now(), self.observe(ob)));
            tracing::trace!(stage, read = self.pressed_obs.is_some(), "card pressed");
            return;
        }
        if let StripEvent::Unveil { id } = event {
            if self.phase.switching().is_some_and(|s| s.id == id) {
                self.flush_pending(ob);
            }
            return;
        }
        if let StripEvent::StretchEnded { id, window, landed } = event {
            self.stretch_ended(ob, id, window, landed);
            return;
        }
        // The restore window's end (at this event or at the card's press) may have moved a
        // voter out of the stage the card showed: the card follows its window.
        let event = self.follow_event(event);
        self.flush_superseded(ob, "a strip click");
        // The strip shows a reorder or a merge before the engine has decided: when the engine
        // refuses it (or does nothing), a snapshot puts the strip back (review R2 V4).
        // A window dragged off a combined card leaves the card drawn without it, the same.
        let local_edit = matches!(
            event,
            StripEvent::CardMoved { .. }
                | StripEvent::CardMerged { .. }
                | StripEvent::CardDropped { window: Some(_), .. }
                | StripEvent::WindowToNewCard { .. }
                | StripEvent::WindowToCard { .. }
        );
        if self.frozen_on(self.strip_event_display(&event)) {
            // Refused: nothing acted, so a focus held (and a Desktop arranged for it) stays.
            tracing::debug!(settling = self.phase.changing(), "a full-screen app's Space shows or a Space change is settling; the strip's input does nothing");
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
        // A card dropped on another screen opens there (Apple's, 11-09): that screen must take
        // input too.
        let drop_to = self.card_drop_display(&event);
        if drop_to.is_some_and(|d| d != self.strip_event_display(&event) && self.frozen_on(d)) {
            tracing::debug!("a card dropped on a screen showing a full-screen app's Space or settling: nothing happens");
            self.present();
            return;
        }
        if matches!(event, StripEvent::CardClicked { .. } | StripEvent::IconClicked { .. }) {
            // A tab selected just now, before the Space check saw it: the switch takes it with
            // its stage, never leaving it on screen (round tab-lost).
            self.take_tabs(ob, &windows::list_windows());
            if self.adopt_stranded_before_switch(ob, "a card click") {
                self.pressed_obs = None;
            }
        }
        let pressed = self.pressed_obs.take().filter(|(at, _)| at.elapsed() < PRESSED_OBS_MAX).map(|(_, obs)| obs);
        let obs = match pressed {
            Some(obs) if matches!(event, StripEvent::CardClicked { .. } | StripEvent::IconClicked { .. }) => obs,
            _ => self.observe(ob),
        };
        let Some(m) = self.model.as_mut() else { return };
        // A card click the engine does not take (its stage on a Desktop not shown: a card
        // drawn where the engine lists none, round displays-lid-2) says why, and the strips
        // show the engine's cards again: a click never does nothing silently.
        let mut refused = None;
        let actions = match event {
            StripEvent::CardClicked { stage, shift } => {
                counters::bump(Count::CardClick);
                tracing::info!(stage, number = m.stage_number(StageId(stage)), shift, "card clicked");
                refused = m.switch_refused(StageId(stage)).filter(|r| *r != SwitchRefused::InFront).map(|r| (stage, r));
                if shift { m.add_card_front_to_active(StageId(stage), &obs) } else { m.switch_to(StageId(stage), &obs) }
            }
            StripEvent::CardDropped { stage, window, x, y } if drop_to.is_some_and(|d| m.stage(StageId(stage)).is_some_and(|s| s.display != d)) => {
                counters::bump(Count::Drop);
                let to = drop_to.unwrap_or_else(|| m.main_display().id);
                tracing::info!(stage, ?window, x, y, screen = to.0, "card dropped on another screen: it opens there, in that screen's current stage");
                m.drop_card_on_display(StageId(stage), window.map(WindowId), Point::new(x, y), to, &obs)
            }
            StripEvent::CardDropped { stage, window: Some(window), x, y } => {
                counters::bump(Count::Drop);
                tracing::info!(stage, window, x, y, "a window dragged out of a combined card: it alone joins the current stage");
                m.move_window_into_active_at(StageId(stage), WindowId(window), Point::new(x, y), &obs)
            }
            StripEvent::CardDropped { stage, window: None, x, y } => {
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
            StripEvent::WindowToNewCard { stage, window, before } => {
                counters::bump(Count::Drop);
                tracing::info!(stage, window, ?before, "a window dragged off a combined card into a gap of the strip: a stage of its own");
                m.move_window_to_new_stage(StageId(stage), WindowId(window), before.map(StageId))
            }
            StripEvent::WindowToCard { stage, window, into } => {
                counters::bump(Count::Drop);
                tracing::info!(stage, window, into, "a window dragged off a combined card onto a card: it alone joins that stage");
                m.move_window_to_stage(StageId(stage), WindowId(window), StageId(into), &obs)
            }
            StripEvent::CardMoved { .. } | StripEvent::Unveil { .. } | StripEvent::StretchEnded { .. } | StripEvent::CardPressed { .. } => Vec::new(),
        };
        let changed = actions.contains(&Action::StateChanged);
        self.execute_animated(ob, actions);
        if let Some((stage, why)) = refused.filter(|_| !changed) {
            tracing::info!(stage, why = why.text(), "card click: nothing to switch to; the strips show the engine's cards again");
            self.present();
        } else if local_edit && !changed {
            tracing::debug!("the engine did not take the strip's edit; the strip shows the engine's cards again");
            self.present();
        }
    }

    /// The screen a card drop lands on: the managed screen showing most of the dropped window
    /// (its front window's size at the drop's top-left corner, or the window dragged out
    /// alone; [`Self::display_at`]). `None` for any other event or an unknown stage.
    fn card_drop_display(&self, event: &StripEvent) -> Option<DisplayId> {
        let StripEvent::CardDropped { stage, window, x, y } = *event else { return None };
        let m = self.model.as_ref()?;
        let front = window.map(WindowId).or_else(|| m.stage(StageId(stage))?.windows.first().copied())?;
        let size = m.window(front)?.restore;
        Some(self.display_at(Rect::new(x, y, size.w, size.h)))
    }

    /// The screen whose stages a strip event addresses: its stage's (the main one when the
    /// model has no such stage).
    fn strip_event_display(&self, event: &StripEvent) -> DisplayId {
        let stage = match *event {
            StripEvent::CardClicked { stage, .. }
            | StripEvent::CardPressed { stage }
            | StripEvent::CardDropped { stage, .. }
            | StripEvent::IconClicked { stage, .. }
            | StripEvent::CardMoved { stage, .. }
            | StripEvent::CardMerged { stage, .. }
            | StripEvent::WindowToNewCard { stage, .. }
            | StripEvent::WindowToCard { stage, .. } => Some(stage),
            StripEvent::Unveil { .. } | StripEvent::StretchEnded { .. } => None,
        };
        stage
            .and_then(|s| self.model.as_ref()?.stage(StageId(s)).map(|s| s.display))
            .unwrap_or_else(|| self.main_display_id())
    }

    /// Runs a batch animated (user input) or at once (scripted ctl commands).
    fn run_batch(&mut self, ob: &Observer, actions: Vec<Action>, animated: bool) -> usize {
        if animated { self.execute_animated(ob, actions) } else { self.execute(ob, actions) }
    }

    fn switch(&mut self, ob: &mut Observer, n: usize, animated: bool) -> Result<usize, String> {
        let list = windows::list_windows();
        let obs = self.observe_with(ob, &list);
        let keyboard = self.keyboard_display(obs.focused, &list);
        let m = self.model.as_mut().ok_or_else(not_managing)?;
        if m.stage_by_number(m.hotkey_space(keyboard), n).is_none() {
            return Err(format!("there is no stage {n}"));
        }
        let actions = m.switch_to_number(n, keyboard, &obs);
        let count = actions.len();
        self.run_batch(ob, actions, animated);
        Ok(count)
    }

    fn send(&mut self, ob: &mut Observer, id: Option<u32>, target: Target, animated: bool) -> Result<usize, String> {
        let obs = self.observe(ob);
        let id = id.map(WindowId).or(obs.focused).ok_or("no focused window")?;
        let unknown = self.model.as_ref().ok_or_else(not_managing)?.window(id).is_none();
        // A window no stage holds, on screen (round stage-lost): taken into the stage in
        // front first, then sent.
        let obs = if unknown && !self.adopt_stranded(ob, "sent to a stage", Some(id.0)).is_empty() { self.observe(ob) } else { obs };
        let m = self.model.as_mut().ok_or_else(not_managing)?;
        if m.window(id).is_none() {
            return Err(format!("window {} is not managed", id.0));
        }
        if let Some(why) = m.send_refused(id) {
            return Err(format!("window {} is not sent: {why}", id.0));
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
    /// failed; the registry is empty afterwards when none did. The frames it restored stay
    /// known to the leftovers check that follows ([`Self::safety_ours`]), and the stages are
    /// saved before that check reads `state.json` (the review of 10-29, S1).
    pub fn unpark_all(&mut self, ob: &mut Observer) -> usize {
        self.flush_pending(ob);
        // A tab selected since the last check, its group hidden: the tab selected comes back
        // at the group's frame, not the hidden tab (round tab-quit).
        self.take_tabs(ob, &windows::list_windows());
        // Windows whose restore failed earlier: the model counts them shown, so its unpark
        // has nothing for them (S1).
        self.retry_shows(ob, true);
        let Some(m) = self.model.as_mut() else { return self.shows_owed() };
        let actions = m.unpark_all();
        self.unparked = unparked_frames(&actions);
        self.unparked_apps = self.unparked.keys().filter_map(|&id| m.window(WindowId(id)).map(|w| (id, w.meta.pid))).collect();
        let n = actions.len();
        let failed = self.execute_batch(ob, actions, Batch::Unpark);
        safety::registry_replace(self.parked_entries(ob));
        self.save_now();
        tracing::info!(windows = n, failed, unrestored = self.shows_owed(), "unparked every window");
        failed + self.shows_owed()
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
                let style = config.strip_style();
                if *self.style != style {
                    self.style = Arc::new(style);
                }
                self.echoes.set_window(Duration::from_millis(config.windows.focus_echo_ms));
                if self.model.is_some() && config.displays.main_only != self.main_only() {
                    tracing::warn!(
                        main_only = config.displays.main_only,
                        "config: [displays] main_only changed; it applies at the next launch (quit and open Stage Manager again)"
                    );
                }
                self.sync_fit();
                self.hidden_changed();
                if let Some(m) = self.model.as_mut() {
                    let capacity = strip_capacity(m.main_display().visible, &config);
                    m.set_strip_options(config.strip_options(capacity));
                    for d in m.displays().to_vec() {
                        m.set_display_capacity(d.id, strip_capacity(d.visible, &config));
                    }
                    // Following the Dock, a changed [strip] dock_ignore re-sorts at once, as a
                    // changed Dock does: the strip presents below.
                    if m.set_dock_ignore(config.strip.dock_ignore.clone()) {
                        tracing::info!("follow the Dock: [strip] dock_ignore changed the strip's order");
                    }
                    let actions = m.set_named_stages(config.stages.names.clone());
                    self.execute(ob, actions);
                }
                // The strip's slots may have changed with nothing to execute: the next switch
                // grows from the cards as they are now (the review's K3).
                self.remember();
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
        self.publish_minimizable(ob);
        if let Some(s) = self.phase.switching().filter(|s| s.overdue(now)) {
            tracing::warn!(id = s.id, max_ms = s.max.as_millis() as u64, "the strip never unveiled a transition's windows; they come back now");
            self.flush_pending(ob);
        }
        self.echoes.prune(now);
        if !safety::quit_requested() {
            self.retry_shows(ob, false);
        }
        if self.focus_after_press.is_some() && !input::left_button_down() {
            // The button came up without a mouse-up the monitor saw.
            self.focus_after_press_done(ob);
        }
        if self.model.is_none() && (self.build_wait.is_some() || self.screens_wait.is_some()) && !safety::quit_requested() {
            // The launch's stages wait for the Desktop to be seen, or for the screens: look again
            // now and then.
            if self.lock_checked.is_none_or(|t| now.duration_since(t) >= LOCK_POLL) {
                if !self.poll_lock(now, true) {
                    ob.rescan_all();
                }
                self.on_started(ob);
            }
        }
        if self.model.is_some() {
            self.poll_lock(now, false);
        }
        self.watch_accessibility(ob, now);
        // No capture round while nobody can see the screen (8M: rounds failing for minutes).
        let away = if self.locked {
            Some(capture_pace::Away::Locked)
        } else if objc2_core_graphics::CGDisplayIsAsleep(objc2_core_graphics::CGMainDisplayID()) {
            Some(capture_pace::Away::Asleep)
        } else {
            None
        };
        super::capture::set_away(away);
        if self.save.due(now) && !self.locked {
            // Held while the screen is locked: nothing seen then is written over the file.
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
            self.refresh_dock_bar(now);
            self.refresh_dock_order(now, ob.last_launch());
            // A wallpaper click's hold over: the patch is planned again (it stays hidden while
            // Show Desktop is listed; back if macOS showed none).
            if self.overview.expectation_over(now) {
                self.look_at_screen();
            }
            // ⌘Tab's switcher closed with its release unheard first: its window put home is
            // then judged as after a release.
            self.switcher_tick(ob, now);
            self.expire_ahead(ob, now);
            self.slide_hold_step(ob);
            if self.absent_due.is_some_and(|t| now >= t) && !self.ax_sees_nothing() {
                self.give_up_absent(ob);
            }
            if self.rematch_until.is_some_and(|t| now >= t) {
                self.rematch_until = None;
                if let Some(m) = self.model.as_mut() {
                    m.end_rematch();
                }
            }
            if now.duration_since(self.quiet_checked) >= Duration::from_secs(1) {
                self.quiet_checked = now;
                self.give_up_pending(ob, now);
            }
            // An app with saved windows running with no window yet is still opening (Xcode, an
            // Electron app: 30-90 s): the login is not quiet (the review of 8F, M2).
            let opening = self.settling.is_some() && self.pending_apps_opening(ob);
            let unseen = self.ax_sees_nothing();
            if let Some(clock) = self.settling.as_mut() {
                if let Some(t) = ob.last_launch() {
                    clock.busy(t);
                }
                if opening {
                    clock.busy(now);
                }
                // Not while the screen is locked (a login that locks at once): nothing is done
                // for a screen nobody sees; the end comes after the unlock.
                // Nor while Accessibility lists no window ([`ax_blind`]): nothing is seen.
                if let Some(end) = clock.due(now).filter(|_| !self.locked && !unseen) {
                    let why = if end == End::TimeUp { "its five minutes are up" } else { "apps went quiet" };
                    self.end_restore(ob, why, end);
                }
            } else if (self.front_after_restore || self.focus_after_restore)
                && !self.frozen()
                && self.phase.switching().is_none()
                && self.drag.is_none()
                && !input::left_button_down()
            {
                let (front, focus) = (std::mem::take(&mut self.front_after_restore), std::mem::take(&mut self.focus_after_restore));
                let obs = self.observe(ob);
                if let Some(m) = self.model.as_mut() {
                    // Ended by itself after a restart: the saved active stage of the Desktop
                    // shown when saved comes forward when its windows came back (round 8I).
                    let mut actions = if focus { m.front_of_saved_current(&obs) } else { Vec::new() };
                    if actions.is_empty() && front {
                        actions = m.front_after_restore(&obs);
                    }
                    if actions.is_empty() && focus {
                        actions = m.focus_after_restore(&obs);
                    }
                    self.execute_animated(ob, actions);
                }
            }
            if self.locked {
                // Nothing is inferred from what a locked screen lists.
            } else if self.phase.changing() {
                self.settle_space(ob);
            } else {
                self.check_spaces(ob);
            }
            // The running engine's checks: a failed one hands over to the Corner engine.
            self.check_engine(ob);
            // What the model counts hidden against the screen, the engine's way.
            self.upkeep_tick(ob);
        }
        if self.strip_dirty {
            self.present();
        }
    }

    /// Why the tick stays brisk for the controller ([`crate::engine::tick_pace`]): the first
    /// of its duties that waits on the tick, or `None` when none does and the tick may rest.
    /// What waits here is what the tick would otherwise act on late: a deadline set (a
    /// switch's unveil, a save, the restore's ends, an arrangement made ahead, a held focus),
    /// evidence counted towards a second look (windows missing from the Space, windows of
    /// another Space on screen, a dead handle judged closed once), a press or a drag, and the
    /// engine's own retries ([`Self::upkeep_tick_waits`]). Cheap: fields and one read of the
    /// mouse button.
    pub fn tick_waits(&self, now: Instant) -> Option<&'static str> {
        let waits = [
            (self.model.is_none() && (self.build_wait.is_some() || self.screens_wait.is_some()), "the launch's stages wait"),
            (self.settling.is_some(), "the restore window is open"),
            (self.phase.switching().is_some() || self.pending.is_some(), "a switch"),
            (self.phase.space_settling(now), "a Space change settles"),
            (self.save.is_dirty(), "a save is due"),
            (self.strip_dirty, "the strip is presented"),
            (self.press.is_some() || self.drag.is_some() || self.focus_after_press.is_some(), "a press or a drag"),
            (self.front_after_restore || self.focus_after_restore, "after the restore"),
            (self.absent_due.is_some() || self.rematch_until.is_some(), "the restore's deadlines"),
            (
                self.ahead.is_some() || self.verify.is_some() || self.slide_hold.is_some() || self.dock_press.is_some(),
                "an arrangement made ahead",
            ),
            (self.overview.showing_or_expected(now) || self.overview.freezes(now) || self.macos_reveal_at.is_some(), "an overview"),
            (!self.absent.is_empty() || self.foreign.is_pending() || !self.stale.waiting().is_empty(), "windows counted missing"),
            (self.focus_replay.peek(now).is_some(), "a focus held"),
            (self.unminimizing.values().any(|at| now.duration_since(*at) < UNMINIMIZE_MAX), "an un-minimize"),
            (self.fresh_waiting(), "a new window"),
            (self.ax_sees_nothing(), "Accessibility lists no window"),
            (self.screens.values().any(|w| w.waits(now)), "another screen's checks"),
        ];
        if let Some((_, why)) = waits.into_iter().find(|(w, _)| *w) {
            return Some(why);
        }
        if let Some(why) = self.upkeep_tick_waits(now) {
            return Some(why);
        }
        input::left_button_down().then_some("the mouse button is down")
    }

    /// The longest the tick may rest for the controller ([`crate::engine::tick_pace::REST`],
    /// or less when the engine's upkeep caps it: [`Self::upkeep_rest_cap`]).
    pub fn tick_rest(&self) -> Duration {
        self.upkeep_rest_cap().map_or(crate::engine::tick_pace::REST, |cap| cap.min(crate::engine::tick_pace::REST))
    }

    /// True while the restore window is open (the login settles): apps that do not answer
    /// Accessibility yet are asked often then ([`crate::engine::observe_retry`]).
    pub fn restore_window_open(&self) -> bool {
        self.settling.is_some()
    }

    /// Closes the restore window (the first user action, apps quiet, or its time up; see
    /// [`crate::engine::restore`]): from now on a window that goes back to an inactive stage
    /// brings that stage forward, and no Desktop becomes provisional. At the next tick an
    /// empty saved active stage with cards beside it gives way (`front_after_restore`; not
    /// after an app switcher key, whose own switch wins), and when it ended by itself the
    /// saved key window takes the keyboard (`focus_after_restore`). After a restart the
    /// restore's line is logged (`log summary` repeats it).
    ///
    /// A user's end waits while Accessibility has listed no window in this run, or lists
    /// none now ([`ax_blind`]): it would end the restore on nothing.
    fn end_restore(&mut self, ob: &Observer, why: &str, end: End) {
        if self.settling.is_some() && !end.by_itself() && !self.user_may_end_restore(ob) {
            tracing::debug!(why, "restore: the restore window stays open: Accessibility lists no window");
            return;
        }
        let Some(clock) = self.settling.take() else { return };
        let now = Instant::now();
        // The entries of apps not running then: given up at the next tick (or the login's
        // minute), before a still-empty front gives way (`front_after_restore`).
        self.absent_due = Some(clock.give_up_absent_at(now));
        let mut voters = Vec::new();
        let mut line = None;
        let mut away = String::new();
        if let Some(m) = self.model.as_mut()
            && let Some(actions) = m.end_restore()
        {
            tracing::info!(why, pending = m.pending_count(), pending_unvisited = m.pending_unvisited(), "restore: the restore window ends");
            line = m.restore_line(clock.age(now).as_secs()).map(|l| Summary { first_after_login: clock.first_placed_after_login(), ..l });
            away = m.away_line(logging::title);
            self.front_after_restore = end.brings_front();
            // Only after a restart: a relaunch in the same session changed no focus.
            self.focus_after_restore = end.by_itself() && m.after_restart();
            voters = actions;
        }
        if let Some(line) = line {
            // The windows back on another Desktop follow the counts, titles hashed.
            tracing::info!("{line}{away}");
        }
        // The windows that voted for a saved Desktop, back in their saved stages (round 8I2):
        // run now, before the action that ended the window acts on the model.
        self.execute(ob, voters);
    }

    /// Gives up the pending windows of apps running and quiet for
    /// [`crate::engine::restore::QUIET`] (no new window): the windows were closed inside the
    /// app's own session. An app that is not running keeps them (until a later session); one
    /// with a window not shown yet (another Desktop, minimized, hidden) is not quiet. Its
    /// entries on saved Desktops not visited yet stay for that Desktop, taking no window shown
    /// elsewhere from then on ([`Model::settle_quiet`]); that Desktop's first show starts its
    /// minute again.
    fn give_up_pending(&mut self, ob: &mut Observer, now: Instant) {
        // Apps that quit have no clock (a later launch starts one again).
        self.quiet.prune(|b| !ob.pids_with_bundle(b).is_empty());
        let Some(m) = self.model.as_mut() else { return };
        for b in m.take_shown_apps() {
            self.quiet.window(&b, now);
        }
        let Some(m) = self.model.as_ref() else { return };
        let bundles = m.waiting_bundles();
        if bundles.is_empty() {
            return;
        }
        let waiting = self.waiting_bundles(ob);
        for b in &bundles {
            if ob.pids_with_bundle(b).is_empty() {
                self.quiet.stopped(b);
            } else {
                // The clock starts at the app's first window: one with none yet (still
                // launching, or windowless) is never quiet.
                self.quiet.running(b, app_has_window(m, &waiting, b), now);
            }
        }
        if self.ax_sees_nothing() {
            // Accessibility lists no window: no app can show one, so no app's minute runs.
            self.quiet.restart_all(now);
            return;
        }
        let settled: Vec<String> =
            self.quiet.settled(now).into_iter().filter(|b| bundles.contains(b) && !waiting.contains(b)).collect();
        if settled.is_empty() || self.locked || self.frozen() {
            return;
        }
        let Some(m) = self.model.as_mut() else { return };
        let (actions, dropped, closed) = m.settle_quiet(&settled);
        if !closed.is_empty() {
            tracing::info!(apps = ?closed, "restore: apps quiet for a minute; their windows saved on Desktops not shown yet wait for those Desktops only");
        }
        if dropped > 0 {
            tracing::info!(dropped, apps = ?settled, "restore: pending windows given up (their apps opened no window for a minute)");
            self.execute(ob, actions);
            if self.settling.is_none() {
                // A saved active stage emptied for good after the window: it gives way too.
                self.front_after_restore = true;
            }
        }
    }

    /// After the restore window (at `absent_due`): the waiting
    /// entries of apps not running are given up, in the model's stages and on the saved
    /// Desktops not visited ([`Model::give_up_absent`]): macOS did not reopen those apps, and a
    /// later launch goes by the new-window rules. Running apps keep theirs under their own
    /// quiet minute ([`Self::give_up_pending`]). Waits while the screen is locked or a stage
    /// change is frozen, and while no running app is known yet.
    fn give_up_absent(&mut self, ob: &mut Observer) {
        if self.frozen() || !ob.knows_apps() {
            return;
        }
        self.absent_due = None;
        let Some(m) = self.model.as_mut() else { return };
        let (actions, absent, dropped) = m.give_up_absent(|b| !ob.pids_with_bundle(b).is_empty());
        if dropped > 0 {
            tracing::info!(dropped, apps = ?absent, "restore: saved windows given up (their apps were not running when the restore window ended)");
            self.execute(ob, actions);
            // A saved active stage emptied for good gives way too.
            self.front_after_restore = true;
        }
    }

    /// The apps of windows waiting to show (another Desktop's, minimized, hidden: unplaced).
    fn waiting_bundles(&self, ob: &Observer) -> HashSet<String> {
        self.unplaced.iter().filter_map(|id| ob.window(*id).and_then(|w| ob.bundle_id(w.pid)).map(str::to_string)).collect()
    }

    /// True when an app with pending windows is running with no window yet (managed or
    /// waiting to show): still opening ([`restore::still_opening`]).
    fn pending_apps_opening(&self, ob: &Observer) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let bundles = m.pending_bundles();
        if bundles.is_empty() {
            return false;
        }
        let waiting = self.waiting_bundles(ob);
        restore::still_opening(bundles.iter().map(String::as_str), |b| !ob.pids_with_bundle(b).is_empty(), |b| {
            app_has_window(m, &waiting, b)
        })
    }

    // ----- ctl ---------------------------------------------------------------------------

    /// Answers a ctl command that acts on the engine.
    pub fn ctl(&mut self, ob: &mut Observer, cmd: Command) -> Value {
        let done = |r: Result<usize, String>| match r {
            Ok(actions) => json!({ "ok": true, "actions": actions }),
            Err(e) => json!({ "ok": false, "error": e }),
        };
        if ctl_lands_switch(&cmd) {
            self.flush_pending(ob);
        }
        let acts = matches!(
            cmd,
            Command::Switch(_) | Command::Send(..) | Command::New(_) | Command::Pin(_) | Command::Merge(_) | Command::Reveal
        );
        if acts || matches!(cmd, Command::DebugDockClick { .. }) {
            self.end_restore(ob, "a ctl command", End::User);
        }
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
            Command::State => self.state_json(),
            // The guardian of a crashed copy: which of its waiting windows are ours now.
            Command::Adopted(ids) => {
                let adopted: Vec<u32> =
                    ids.into_iter().filter(|id| self.model.as_ref().is_some_and(|m| m.window(WindowId(*id)).is_some())).collect();
                json!({ "ok": true, "adopted": adopted })
            }
            Command::Space => match &self.model {
                Some(m) => json!({
                    "ok": true,
                    "space": m.current_space(m.main_display().id).0,
                    "phase": self.phase.phase().to_string(),
                    "fullscreen": self.phase.fullscreen(),
                    "settling": self.phase.changing(),
                    "locked": self.locked,
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
            Command::DebugMinimizeButton { id } => {
                if std::env::var_os("STAGE_MANAGER_TEST").is_none() {
                    return json!({ "ok": false, "error": "debug-minimize-button needs STAGE_MANAGER_TEST=1" });
                }
                match test_window(ob, id, "debug-minimize-button").and_then(|e| e.minimize_button().and_then(|b| b.frame()).map_err(|e| e.to_string())) {
                    Ok(r) => json!({ "ok": true, "frame": { "x": r.x, "y": r.y, "w": r.w, "h": r.h } }),
                    Err(e) => json!({ "ok": false, "error": e }),
                }
            }
            Command::DebugStaleHandle { id } => {
                if std::env::var_os("STAGE_MANAGER_TEST").is_none() {
                    return json!({ "ok": false, "error": "debug-stale-handle needs STAGE_MANAGER_TEST=1" });
                }
                match test_window(ob, id, "debug-stale-handle") {
                    Ok(_) if ob.poison(id) => {
                        tracing::info!(id, "debug-stale-handle: our handle is taken for dead until looked up afresh");
                        json!({ "ok": true })
                    }
                    Ok(_) => json!({ "ok": false, "error": format!("window {id} is not tracked") }),
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
            Command::DebugSwitcher => {
                if std::env::var_os("STAGE_MANAGER_TEST").is_none() {
                    return json!({ "ok": false, "error": "debug-switcher needs STAGE_MANAGER_TEST=1" });
                }
                match super::dock::dock_pid() {
                    Some(dock) => {
                        let mut v = super::switcher_ax::list(dock);
                        v["ok"] = json!(true);
                        v
                    }
                    None => json!({ "ok": false, "error": "the Dock does not run" }),
                }
            }
            Command::DebugCmdTab(probe) => {
                if std::env::var_os("STAGE_MANAGER_TEST").is_none() {
                    return json!({ "ok": false, "error": "debug-cmd-tab needs STAGE_MANAGER_TEST=1" });
                }
                self.debug_cmd_tab(ob, probe)
            }
            Command::DebugDockClick { bundle, real } => {
                if std::env::var_os("STAGE_MANAGER_TEST").is_none() {
                    return json!({ "ok": false, "error": "debug-dock-click needs STAGE_MANAGER_TEST=1" });
                }
                self.debug_dock_click(ob, &bundle, real)
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
            Command::Ping | Command::TakeWatch(_) | Command::Quit | Command::Settings | Command::Mark(_) | Command::DebugPanic | Command::DebugEngineFail(_) | Command::DebugMenuQuit | Command::DebugUpdate | Command::DebugClick { .. }
            | Command::DebugHover { .. }
            | Command::DebugPointerBack
            | Command::DebugDrag { .. }
            | Command::DebugDesktop { .. }
            | Command::DebugStrip
            | Command::DebugToggleStrip
            | Command::DebugOverlay { .. }
            | Command::DebugGlass { .. }
            | Command::DebugStripHover { .. }
            | Command::DebugSpace { .. }
            | Command::DebugCmdM
            | Command::DebugExpose { .. } => {
                json!({ "ok": false, "error": "not a WM command" })
            }
        }
    }

    /// Stages, windows, frames and parked flags as JSON (`ctl state`).
    pub fn state_json(&self) -> Value {
        let Some(m) = &self.model else {
            return json!({ "ok": false, "error": not_managing() });
        };
        let active = m.main_active_stage();
        let number = |s: StageId| m.stage_number(s);
        // The main screen's stages, as before round displays (every other screen's are under
        // `displays[i]`): the suite's counts are of the main screen's.
        let main_id = m.main_display().id;
        let stages: Vec<Value> = m
            .stages()
            .iter()
            .filter(|s| m.display_of_space(s.space) == main_id)
            .map(|s| {
                json!({
                    "id": s.id.0,
                    "space": s.space.0,
                    "number": number(s.id),
                    "name": s.name,
                    "active": m.active_stage_in(s.space) == Some(s.id),
                    "key": s.key.map(|k| k.0),
                    "windows": s.windows.iter().map(|w| w.0).collect::<Vec<_>>(),
                    "pending": m.pending_of(s.id).len(),
                    // When it was last active: past the capacity, in place, the bottom
                    // places go by it (`Model::strip_in`).
                    "last_active": s.last_active,
                })
            })
            .collect();
        // Each window's frame as the window server describes it now, in one query: never an
        // Accessibility round trip per window on the WM thread, which scripts polling this
        // during a switch held up behind a busy app (the review's K8).
        let ids: Vec<u32> = m.windows().map(|w| w.meta.id.0).collect();
        let listed = described_bounds(&windows::describe(&ids));
        let windows: Vec<Value> = m
            .windows()
            .map(|w| {
                let live = listed.get(&w.meta.id.0);
                json!({
                    "id": w.meta.id.0,
                    "pid": w.meta.pid,
                    "bundle_id": w.meta.bundle_id,
                    "title": w.meta.title,
                    "role": match w.meta.role { WindowRole::Standard => "standard", WindowRole::Child { .. } => "child" },
                    "parent": match w.meta.role { WindowRole::Child { parent } => parent.map(|p| p.0), WindowRole::Standard => None },
                    "home": match w.home { Home::Pinned => json!("pinned"), Home::Stage(s) => json!(number(s)) },
                    // Its stage's id: `home` is a number on its own Desktop, so two screens
                    // each have a stage 2 (round displays-live-1).
                    "stage": match w.home { Home::Pinned => None, Home::Stage(s) => Some(s.0) },
                    "display": w.meta.display.0,
                    "restore": w.restore,
                    "original": w.original,
                    "live": live,
                    "parked": w.parked,
                    "space": w.space.0,
                    "sticky": w.sticky,
                    "restored": m.restored_grade(w.meta.id).map(|g| g.name()),
                    "pooled": m.is_pooled(w.meta.id),
                })
            })
            .collect();
        // The desktop's still for a switch's cover (8L3-8L4) and the icons it is keyed by
        // (the main display's; every display's in `displays`).
        let main_screen = m.main_display().id.0;
        let mut desktop_still = desktop_shot::status(main_screen);
        desktop_still["icons"] = json!(self.patch_items.known);
        // The parked windows the screen contradicts, being parked again or adopted
        // ([`Self::reconcile_parked`]), and the corner patch's mode (L3).
        let strays: Vec<Value> = self.status_strays();
        let corner_patch = self.status_patch(main_screen);
        desktop_still["patch_plan"] = self.status_patch_plan(main_screen);
        // The reach its rectangle grows each sliver by (round fixes-151: per side).
        let reach = self.store.config().windows.patch_reach();
        desktop_still["patch_reach"] = json!({ "left": reach.left, "top": reach.top, "right": reach.right, "bottom": reach.bottom });
        // Every screen (round displays): its Desktop, front stage, strip and park corner.
        let displays_json: Vec<Value> = m
            .displays()
            .iter()
            .map(|d| {
                let space = m.current_space(d.id);
                let strip_key = if d.id == m.main_display().id { strip_model::MAIN_STRIP } else { d.id.0 };
                let mut still = desktop_shot::status(d.id.0);
                still["patch_plan"] = self.status_patch_plan(d.id.0);
                json!({
                    "id": d.id.0,
                    "key": d.key.text(),
                    "frame": d.frame,
                    "visible": d.visible,
                    "corner": format!("{:?}", d.corner).to_lowercase(),
                    "space": space.0,
                    // A full-screen app's Space shows there (the main screen's is the phase's,
                    // the others' their own watch; round displays-2).
                    "fullscreen": if d.id == m.main_display().id { self.phase.fullscreen() } else { self.other_screen_fullscreen(d.id.0) },
                    "active": m.active_stage(d.id).and_then(number),
                    // Its front stage's id (`active` is its number, on this screen's Desktop).
                    "active_stage": m.active_stage(d.id).map(|s| s.0),
                    "strip": m.strip_slots(d.id).iter().filter_map(|s| m.stage_number(*s)).collect::<Vec<_>>(),
                    "capacity": m.capacity_of(space),
                    // The cards its strip draws now (the main display's strip publishes as 0).
                    "cards": strip_model::published_cards_of(strip_key)
                        .into_iter()
                        .map(|(stage, r)| json!({ "number": m.stage_number(StageId(stage)), "frame": r }))
                        .collect::<Vec<_>>(),
                    // Its strip panel and panes, its corner patch and the still of its corner
                    // (round displays-2: every screen has its own).
                    "strip_panel": strip_model::panel_status_of(strip_key),
                    "strip_panels": strip_model::pane_statuses_of(strip_key),
                    // The windows its strip holds a thumbnail of (drawn on its cards).
                    "pictures": strip_model::pictures_of(strip_key),
                    "corner_patch": self.status_patch(d.id.0),
                    "desktop_still": still,
                })
            })
            .collect();
        let mut state = json!({
            "ok": true,
            "space": m.current_space(m.main_display().id).0,
            "fullscreen": self.phase.fullscreen(),
            "unplaced": self.unplaced.iter().collect::<Vec<_>>(),
            "active": active.and_then(number),
            // The cards drawn (the active stage's empty slot left out: `strip_gap` is its slot).
            "strip": m.strip().iter().filter(|c| !c.gap).map(|c| c.number).collect::<Vec<_>>(),
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
            // A switch whose incoming windows still wait behind their snapshots (its id): a
            // question asked now leaves them there (8L3), so the checks wait for it to land.
            "switching": self.phase.switching().map(|s| s.id),
            "capture": super::capture::status(),
            "desktop_still": desktop_still,
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
            "focused": focused_window(),
            "restore": {
                "restoring": m.restoring(),
                "after_restart": m.after_restart(),
                "settling": m.settling(),
                "provisional": m.provisional_spaces().iter().map(|s| s.0).collect::<Vec<_>>(),
                "pending": m.pending_count(),
                "pending_unvisited": m.pending_unvisited(),
                "saved_spaces_waiting": m.unvisited_saved_spaces(),
                "pooling": m.pooling(),
                "pooled": m.pooled_count(),
                "slots": m.strip_slots(m.main_display().id).iter().filter_map(|s| m.stage_number(*s)).collect::<Vec<_>>(),
            },
            "stages": stages,
            "windows": windows,
        });
        state["corner_patch"] = corner_patch;
        // Which window engine runs, and what it keeps and counts.
        state["engine"] = self.status_engine();
        // The WM tick's pace (round sm-tick): ticks brisk and at rest since the launch.
        state["tick"] = super::wm_thread::tick_status();
        // Accessibility lists no window for the session (round ax-blind), since (Unix s).
        (state["accessibility_blind"], state["accessibility_blind_since"]) = self.ax_blind_json();
        state["displays"] = Value::Array(displays_json);
        state["strays"] = Value::Array(strays);
        // Windows whose handle died judged closed once, waiting for a second look.
        state["stale_closing"] = json!(self.stale.waiting());
        // Keep stages in place (9B): the order, the active stage's mode and its empty slot.
        state["strip_gap"] = json!(m.strip().iter().position(|c| c.gap));
        state["strip_order"] = json!(format!("{:?}", m.options().strip.order));
        state["strip_active"] = json!(format!("{:?}", m.options().strip.active));
        // Follow the Dock (9D): the Dock's application items as last read, in order.
        state["dock_order"] = json!(m.dock_order().iter().map(|d| json!({ "bundle": d.bundle, "pid": d.pid })).collect::<Vec<_>>());
        // The registry's windows, and those taken from an old guardian (round 8N).
        state["registry_ids"] = json!(safety::registry_ids());
        state["taken"] = json!(safety::taken_ids());
        state
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
                    "pending": m.stages_in(s.id).map(|st| m.pending_of(st.id).len()).sum::<usize>(),
                    "provisional": m.provisional_spaces().contains(&s.id),
                })
            })
            .collect();
        json!({
            "fullscreen": self.phase.fullscreen(),
            "settling": self.phase.changing(),
            "locked": self.locked,
            "last_arrival": {
                "rebuilt": m.last_arrival().rebuilt,
                "restored": m.last_arrival().restored,
                "moved": m.last_arrival().moved,
                "carried": m.last_arrival().carried,
                "beside": m.last_arrival().beside,
            },
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

/// Whether `display` shows a full-screen app's Space: the main screen's answer is the
/// phase's (`main_fullscreen`), another screen's its own (`other`), so a full-screen app on
/// one screen never freezes the other's input (the review's S1).
fn fullscreen_there(display: DisplayId, main: DisplayId, main_fullscreen: bool, other: impl Fn(DisplayId) -> bool) -> bool {
    if display == main { main_fullscreen } else { other(display) }
}

fn on_screen(list: &[windows::CgWindow]) -> HashSet<u32> {
    list.iter().filter(|w| w.onscreen && w.layer == 0).map(|w| w.id).collect()
}

/// The windows `list` shows on screen (layer 0), with their bounds: what
/// [`slide_hold::slide_listed`] reads.
fn listed_bounds(list: &[windows::CgWindow]) -> Vec<(u32, Rect)> {
    list.iter().filter(|w| w.onscreen && w.layer == 0).map(|w| (w.id, w.bounds)).collect()
}

/// True when `id` is the front window of a stage in the strip (a parked one's): the window a
/// card's accessibility label reads.
fn fronts_a_card(m: &Model, id: WindowId) -> bool {
    m.window(id).is_some_and(|w| match w.home {
        Home::Stage(s) => w.parked && m.stage(s).is_some_and(|s| s.windows.first() == Some(&id)),
        _ => false,
    })
}

/// The strip's cards of `space`, in slot order.
fn cards_in(m: &Model, space: SpaceId) -> Vec<CardSnapshot> {
    m.strip_in(space)
        .into_iter()
        .map(|c| CardSnapshot {
            stage: c.stage.0,
            number: c.number,
            title: c.windows.first().and_then(|id| m.window(*id)).map(|w| w.meta.title.clone()).unwrap_or_default(),
            name: c.name,
            windows: c
                .windows
                .iter()
                .filter_map(|id| m.window(*id))
                .map(|w| CardWindow { id: w.meta.id.0, pid: w.meta.pid, frame: w.restore })
                .collect(),
            apps: c.apps,
            active: c.active,
            gap: c.gap,
        })
        .collect()
}

/// The windows on screen on the managed displays (`managed`: their frames, the main one
/// first; every screen by default, the main one alone with `[displays] main_only`):
/// [`on_screen`] minus those that show mostly on a display not managed
/// ([`on_other_display`]; left alone, docs/GUIDE.md), and minus those off every display
/// ([`off_screen_not_ours`]: a headless browser's, an offscreen render window; `is_managed`
/// tells the model's, whose parked frame beyond the edges stays in).
///
/// The one gate every path that takes a window in goes through (a new window, the launch
/// listing, an arrival on a Desktop and the restore riding on them): a window off every
/// display waits in `unplaced` and is taken in as new only once it shows on a display.
fn on_managed_screens(list: &[windows::CgWindow], managed: &[Rect], is_managed: impl Fn(u32) -> bool) -> HashSet<u32> {
    let displays = desktop::cg_display_bounds();
    // A display CoreGraphics lists that no managed display covers (frames may differ by a
    // rounding: half its area says it is the same screen).
    let covered = |d: &Rect| managed.iter().any(|m| m.intersect(d).is_some_and(|r| r.w * r.h > 0.5 * d.w * d.h));
    let others: Vec<Rect> = displays.iter().filter(|d| !covered(d)).copied().collect();
    let main = managed.first().copied().unwrap_or_default();
    let own = |frame: Rect| -> Rect {
        managed
            .iter()
            .copied()
            .max_by(|a, b| {
                let area = |d: &Rect| frame.intersect(d).map_or(0.0, |r| r.w * r.h);
                area(a).total_cmp(&area(b))
            })
            .unwrap_or(main)
    };
    list.iter()
        .filter(|w| w.onscreen && w.layer == 0)
        .filter(|w| !off_screen_not_ours(w.bounds, &displays, is_managed(w.id)))
        .filter(|w| others.is_empty() || !on_other_display(w.bounds, own(w.bounds), &others))
        .map(|w| w.id)
        .collect()
}

/// Whether a batch begins a switch of its own (it parks or focuses a window): the switch still
/// on its way is superseded ([`Controller::flush_superseded`]). A batch that only brings a
/// window to its frame (a new window in the stage shown) leaves it landing as it was.
fn supersedes(actions: &[Action]) -> bool {
    actions.iter().any(|a| matches!(a, Action::Hide { .. } | Action::Focus { .. }))
}

/// A superseded switch's held batch without its focuses: the windows still come to their
/// frames and stack (the model counts them shown), but the keyboard is the new switch's, or
/// where the user's click put it. Returns the batch kept and the windows whose focus went.
fn without_focus(actions: Vec<Action>) -> (Vec<Action>, Vec<u32>) {
    let mut dropped = Vec::new();
    let kept = actions
        .into_iter()
        .filter(|a| match a {
            Action::Focus { id } => {
                dropped.push(id.0);
                false
            }
            _ => true,
        })
        .collect();
    (kept, dropped)
}

/// Whether a window's focus notice may switch stages: its app is not `hidden` and is the
/// `active` app now (`None`: not running), or the left button is down (a sliver grabbed, its
/// app becoming active). A hidden app's window cannot be the user's choice, and a focus in an
/// app behind is the app's own (an app's key window changes as it is hidden, unhidden or
/// passed over) or a late echo of ours: 11-02, with Accessibility slow, such notices came up
/// to a second late, past the echo window, and each switched stages nobody asked for. An
/// activation of the app, the user's way to it, decides through its own notice.
fn focus_may_switch(hidden: Option<bool>, active: Option<bool>, button_down: bool) -> bool {
    hidden != Some(true) && (active == Some(true) || (active.is_some() && button_down))
}

/// Recency hints for windows placed on arrival: the window list's z-order and the frontmost
/// app.
/// The arrival's windows that stayed out of it or left their stage, each said (round
/// stage-lost: a window never leaves its stage without a line).
fn log_arrival_lets(note: &crate::engine::model::ArrivalNote) {
    if !note.kept_home.is_empty() {
        tracing::info!(
            windows = ?note.kept_home.iter().map(|w| w.0).collect::<Vec<_>>(),
            "windows our switch had just shown, listed on the Desktop macOS slid to: they stay in their stage on their own Desktop"
        );
    }
    for id in &note.sticky {
        tracing::info!(id = id.0, "window seen on every Desktop gone to, back where it came from: on every Desktop now, out of its stage (pinned)");
    }
}

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
        key: desktop::screen_key(objc2_core_graphics::CGMainDisplayID()),
        frame: main,
        visible,
        corner: choose_corner(main, visible, &bounds).corner,
    }]
}

/// True when the window server's bounds `cg` disagree with frame `tracked` by more than a
/// point (CG rounds to whole points).
fn frames_differ(cg: Rect, tracked: Rect) -> bool {
    (cg.x - tracked.x).abs() > 1.0
        || (cg.y - tracked.y).abs() > 1.0
        || (cg.w - tracked.w).abs() > 1.0
        || (cg.h - tracked.h).abs() > 1.0
}

/// True when app `bundle` has a window: one the model manages, or one waiting to show
/// (`waiting`: the apps of unplaced windows).
fn app_has_window(m: &Model, waiting: &HashSet<String>, bundle: &str) -> bool {
    waiting.contains(bundle) || m.windows().any(|w| w.meta.bundle_id == bundle)
}

/// What one present pass hands every display's strip ([`Controller::present_with`]): the
/// launch reveal (the main display's) and the notes of a switch under way.
struct PresentNotes {
    announce: bool,
    parked: Option<u64>,
    arrived: Option<(u64, Vec<(u32, Rect)>)>,
    under_now: Option<(u64, u32)>,
}

/// One shown window's frame as [`Controller::observe_with`] read it through Accessibility.
#[derive(Debug, Clone, Copy)]
struct FrameRead {
    id: WindowId,
    /// The display the model has it on.
    display: Display,
    sticky: bool,
    /// The frame read, or the tracked one when the read failed.
    frame: Rect,
    /// The read answered.
    fresh: bool,
}

/// The frames of an observation from Accessibility's `reads`: each frame read; for a read
/// that failed (a busy app's timeout), the window server's bounds (`listed`) when they can
/// be trusted, by [`activation_frame`]'s rule, else the tracked frame (W3). A display is
/// steady (no Desktop slide shifts its list) when a window read there is listed at the
/// frame read.
fn observed_frames(reads: &[FrameRead], listed: &HashMap<u32, Rect>, settling: bool) -> HashMap<WindowId, Rect> {
    let steady: HashSet<DisplayId> = reads
        .iter()
        .filter(|r| r.fresh && !r.sticky && listed.get(&r.id.0).is_some_and(|cg| !frames_differ(*cg, r.frame)))
        .map(|r| r.display.id)
        .collect();
    reads
        .iter()
        .map(|r| {
            if r.fresh {
                return (r.id, r.frame);
            }
            let cg = listed.get(&r.id.0).copied();
            let frame = activation_frame(r.frame, cg, r.display.frame, settling, steady.contains(&r.display.id));
            if frame != r.frame {
                tracing::debug!(id = r.id.0, tracked = %r.frame, %frame, "frame read failed: the window server's bounds stand in");
            }
            (r.id, frame)
        })
        .collect()
}

/// The frames an observation takes (`frames`, read now): none while macOS's Show Desktop may
/// have the windows pushed aside (`aside`, [`OverviewWatch::moves_windows`]: listed, expected
/// from a wallpaper click, or sliding back after its close), so the model keeps each window's
/// own frame. 11-09: an activation right at Show Desktop's close took a window's listed frame,
/// pushed off the right edge (2408,42 for 452,55), as its own, and every switch to its stage
/// brought it back there. A parked window macOS pushes out of the park zone then is left to
/// [`Controller::reconcile_parked`], which waits for the overview's settle.
fn frames_unless_aside(frames: HashMap<WindowId, Rect>, aside: bool) -> HashMap<WindowId, Rect> {
    if aside && !frames.is_empty() {
        tracing::debug!(windows = frames.len(), "Show Desktop has the windows aside: their frames are not taken; each keeps its own");
        return HashMap::new();
    }
    frames
}

/// A shown window's frame for an activation's switch: the window server's bounds
/// (`listed`) when they disagree with the tracked frame by more than a point (an app may
/// have moved the window without a notice), else the tracked one. The tracked one stays too
/// when the list may be a Desktop slide's (the activation often lands before the Space
/// change's notice): a Space change is pending or `settling`; the bounds miss the window's
/// `display`; or they are the tracked frame moved sideways only, by up to about the
/// display's width ([`arrival::slid_from`]), and the display is not `steady` (no window of
/// its Desktop listed at its tracked frame: a slide shifts them all).
fn activation_frame(tracked: Rect, listed: Option<Rect>, display: Rect, settling: bool, steady: bool) -> Rect {
    match listed {
        Some(cg)
            if frames_differ(cg, tracked)
                && !settling
                && cg.intersect(&display).is_some()
                && (steady || !arrival::slid_from(cg, tracked, display.w)) =>
        {
            cg
        }
        _ => tracked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1.8 someone goes back to from a later version must not wipe that version's stages:
    /// the first save replaced a `state.json` this version could not read.
    #[test]
    fn a_state_file_that_cannot_be_read_is_set_aside_before_any_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let aside = dir.path().join("state.json.unread");
        assert!(read_state_or_set_aside(&path).is_none(), "no file");
        assert!(!aside.exists(), "nothing to set aside");
        // A later version's file.
        let newer = format!(r#"{{"version": {}, "spaces": [], "stages_of_a_later_version": true}}"#, crate::engine::persist::STATE_VERSION + 1);
        std::fs::write(&path, &newer).expect("write");
        assert!(read_state_or_set_aside(&path).is_none());
        assert!(!path.exists(), "nothing left for the first save to replace");
        assert_eq!(std::fs::read_to_string(&aside).ok().as_deref(), Some(newer.as_str()), "kept whole");
        // A damaged file later on replaces the older one set aside.
        std::fs::write(&path, "{ not json").expect("write");
        assert!(read_state_or_set_aside(&path).is_none());
        assert_eq!(std::fs::read_to_string(&aside).ok().as_deref(), Some("{ not json"));
        // A file this version reads stays where it is.
        let good = StateFile::single(crate::engine::persist::SavedSpace {
            screen: None,
            id: 1,
            current: true,
            stages: Vec::new(),
            active_index: None,
            strip_slots: Vec::new(),
            last_seen: None,
            unvisited: false,
        })
        .to_json()
        .expect("json");
        std::fs::write(&path, &good).expect("write");
        assert!(read_state_or_set_aside(&path).is_some());
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&aside).ok().as_deref(), Some("{ not json"), "untouched");
    }

    /// A controller on a scratch config file, as [`Controller::new`] builds one, nothing told.
    fn scratch() -> (Controller, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let (store, _) = ConfigStore::open(path.clone());
        (Controller::with_store(store, &path, None), dir)
    }

    /// The review of 1.9, B-11: the tick's scenarios pace a clock with waits the test returns,
    /// so a duty the controller leaves out of its own waits failed nothing (B-10 was one).
    /// Here each duty's field is set in turn on a fresh controller, and the controller's own
    /// [`Controller::tick_waits`] must name it. The fields whose types need a whole switch or
    /// press to build (a press or a drag, an arrangement made ahead, a held focus, the stale
    /// handles, the foreign windows) are left to the scenarios.
    #[test]
    fn each_duty_that_waits_on_the_tick_keeps_it_brisk() {
        use crate::engine::fullscreen::Seen;
        use crate::engine::overview::Overview;
        let now = Instant::now();
        let cases: Vec<(&str, fn(&mut Controller, Instant))> = vec![
            ("the launch's stages wait", |c, now| c.build_wait = Some((session::Blind::AxSilent, now))),
            ("the restore window is open", |c, now| c.settling = Some(Settling::new(now, None))),
            ("a switch", |c, now| c.phase.feed(phase::Event::SwitchSent { id: 1, max: Duration::from_secs(1) }, now)),
            ("a Space change settles", |c, now| c.phase.feed(phase::Event::SpaceChanged, now)),
            ("a save is due", |c, now| c.save.mark(now)),
            ("the strip is presented", |c, _| c.strip_dirty = true),
            ("a press or a drag", |c, _| c.focus_after_press = Some(1)),
            ("after the restore", |c, _| c.front_after_restore = true),
            ("after the restore", |c, _| c.focus_after_restore = true),
            ("the restore's deadlines", |c, now| c.absent_due = Some(now)),
            ("the restore's deadlines", |c, now| c.rematch_until = Some(now)),
            ("an overview", |c, now| {
                c.overview.see(Some(Overview::ShowDesktop), now);
            }),
            ("an overview", |c, now| c.macos_reveal_at = Some(now)),
            ("windows counted missing", |c, _| {
                c.absent.insert(1, 1);
            }),
            ("an un-minimize", |c, now| {
                c.unminimizing.insert(1, now);
            }),
            ("a new window", |c, now| {
                c.fresh_unplaced.insert(1, now + FRESH_LOOK);
            }),
            // Another screen's checks (B-10).
            ("another screen's checks", |c, _| {
                c.screens.entry(2).or_default().absent.insert(1, 1);
            }),
            ("another screen's checks", |c, now| {
                c.screens.entry(2).or_default().settle.step(Seen::Transition, now);
            }),
            ("another screen's checks", |c, now| {
                c.screens.entry(2).or_default().overview.see(Some(Overview::MissionControl), now);
            }),
        ];
        let (fresh, _dir) = scratch();
        let idle = fresh.tick_waits(now);
        assert!(idle.is_none() || idle == Some("the mouse button is down"), "a fresh controller waits on nothing: {idle:?}");
        for (why, set) in cases {
            let (mut c, _dir) = scratch();
            set(&mut c, now);
            assert_eq!(c.tick_waits(now), Some(why));
        }
        // A screen with nothing waiting keeps nothing brisk.
        let (mut c, _dir) = scratch();
        c.screens.entry(2).or_default();
        let idle = c.tick_waits(now);
        assert!(idle.is_none() || idle == Some("the mouse button is down"), "{idle:?}");
    }

    /// The review of 1.9, B-7: the fast tick looks at a window found off screen only when it
    /// may be about to show.
    #[test]
    fn only_a_window_that_may_be_about_to_show_is_looked_at_on_the_fast_tick() {
        let at = Rect::new(300.0, 200.0, 800.0, 600.0);
        assert!(looks_soon(None, None), "not listed yet: a new window");
        assert!(looks_soon(Some(Rect::new(0.0, 0.0, 0.0, 0.0)), None), "listed with no frame yet: a new window");
        assert!(looks_soon(Some(at), Some(Duration::from_secs(2))), "a just-opened app's window");
        assert!(!looks_soon(Some(at), None), "another Desktop's, a minimized one, a hidden app's: at its frame");
        assert!(!looks_soon(Some(at), Some(FRESH_APP)), "an app launched long ago");
    }

    /// The review of 1.9, B-8: the windows our batches showed are kept only for the slide
    /// carry's time, also with no Space change ever (one Desktop).
    #[test]
    fn the_windows_shown_by_us_are_kept_only_for_the_slide_carry() {
        let t0 = Instant::now();
        let mut shown = HashMap::new();
        for i in 0..100 {
            note_shown_by_us(&mut shown, i, t0 + Duration::from_millis(u64::from(i) * 100));
        }
        let last = t0 + Duration::from_millis(9_900);
        let kept: BTreeSet<u32> = shown.keys().copied().collect();
        let carried = SLIDE_CARRY.as_millis() as u32 / 100;
        assert_eq!(kept, (100 - carried..100).collect(), "only those within the carry of the last");
        assert!(shown.values().all(|at| last.saturating_duration_since(*at) < SLIDE_CARRY));
    }

    /// 11-09 10:00:03: an activation as Show Desktop closed read a window at its pushed-aside
    /// frame and kept it; while Show Desktop may have the windows aside no frame is taken,
    /// and the switch keeps the window's own (the model's tracked frame).
    #[test]
    fn a_frame_show_desktop_set_is_never_taken() {
        use crate::engine::overview::{Overview, OverviewWatch};
        let tracked = Rect::new(452.0, 55.0, 1786.0, 1285.0);
        let pushed = Rect::new(2408.0, 42.0, 1786.0, 1285.0);
        let studio = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let read: HashMap<WindowId, Rect> =
            [(WindowId(299), activation_frame(tracked, Some(pushed), studio, false, true))].into();
        assert_eq!(read[&WindowId(299)], pushed, "the window server's bounds win an activation");
        let t0 = Instant::now();
        let mut watch = OverviewWatch::default();
        watch.see(Some(Overview::ShowDesktop), t0);
        let kept = frames_unless_aside(read.clone(), watch.moves_windows(t0));
        assert!(kept.is_empty(), "Show Desktop listed: nothing taken");
        watch.see(None, t0 + Duration::from_millis(300));
        assert!(frames_unless_aside(read.clone(), watch.moves_windows(t0 + Duration::from_millis(400))).is_empty(), "sliding back");
        assert_eq!(frames_unless_aside(read.clone(), watch.moves_windows(t0 + Duration::from_secs(2))), read, "settled: read again");
    }

    /// 11-09: an app's windows found together go front to back, so the one it opened last (in
    /// front) is placed first and its stage's arrival raises and focuses it; other apps keep
    /// their places in creation order.
    #[test]
    fn windows_found_together_go_front_to_back_within_their_app() {
        // App 7 opened 11 (Red 1) then 12 (Red 2, in front); app 8's 10 and 13 in between.
        let pid = |id: u32| Some(if id == 11 || id == 12 { 7 } else { 8 });
        let z = |id: u32| match id {
            12 => Some(0),
            11 => Some(1),
            13 => Some(2),
            10 => Some(3),
            _ => None,
        };
        assert_eq!(found_order(vec![12, 11, 10, 13, 11], pid, z), vec![13, 12, 11, 10]);
        // Unlisted windows go after the listed ones of their app, oldest first.
        assert_eq!(found_order(vec![5, 4, 12], |_| Some(1), |id| (id == 12).then_some(0)), vec![12, 4, 5]);
        // One window per app: creation order, as before.
        assert_eq!(found_order(vec![3, 1, 2], |id| Some(id as i32), |_| None), vec![1, 2, 3]);
    }

    /// An app opening two windows at once under the Spaces engine: both held off screen,
    /// ranked by the app's own `AXWindows` (Fixture 2, opened last, listed first), so the
    /// one opened last is placed first and its stage's key (live 11-14).
    #[test]
    fn held_windows_rank_by_their_apps_own_order() {
        assert_eq!(held_ranks(&[21, 22], &[22, 21, 30]), vec![(22, 0), (21, 1)]);
        assert_eq!(held_ranks(&[23, 21, 22], &[22]), vec![(22, 0), (21, 1), (23, 2)], "unlisted: after, by id");
        assert_eq!(held_ranks(&[21, 22], &[]), vec![(21, 0), (22, 1)], "the app did not answer: creation order");
        let z: HashMap<u32, usize> = held_ranks(&[21, 22], &[22, 21]).into_iter().map(|(id, r)| (id, 5 + r)).collect();
        assert_eq!(found_order(vec![21, 22], |_| Some(7), |id| z.get(&id).copied()), vec![22, 21]);
    }

    /// The review's S1: a full-screen app on the main screen froze every strip's
    /// input; each screen now answers for itself.
    #[test]
    fn a_full_screen_app_freezes_only_its_own_screens_input() {
        let (main, laptop, third) = (DisplayId(1), DisplayId(2), DisplayId(3));
        let others = |d: DisplayId| d == third;
        assert!(fullscreen_there(main, main, true, others), "the main screen's full-screen Space");
        assert!(!fullscreen_there(laptop, main, true, others), "the laptop's Desktop goes on");
        assert!(!fullscreen_there(main, main, false, others), "the third screen's stops nothing on the main one");
        assert!(fullscreen_there(third, main, false, others));
    }

    /// A full-screen app on the main screen held every activation, so Cmd-Tab or a Dock click
    /// to an app whose stage is a card on the other screen brought nothing. Its windows'
    /// screens answer: held only when one of them takes no input, or none is known.
    #[test]
    fn an_activation_is_held_only_for_a_screen_of_the_apps_that_takes_no_input() {
        assert!(!activation_held(false, [false, true]), "nothing frozen on the main screen: decided as before");
        assert!(!activation_held(true, [false]), "full screen on the main screen, the app's windows on the other");
        assert!(activation_held(true, [false, true]), "one of its windows on the full-screen app's screen: macOS may go there");
        assert!(activation_held(true, [true]), "a Space change settling or the lock holds every screen");
        assert!(activation_held(true, []), "no window of the app placed: held as before");
    }

    /// A full-screen app on the main screen: a press on the other screen, whose wallpaper
    /// click still works, is judged on the window list as read (unread, every press in a
    /// window there was a wallpaper click and sent that screen's stage to the strip).
    #[test]
    fn a_press_on_a_screen_that_takes_input_reads_the_window_list() {
        assert!(press_reads_list(true, false, Some(false)), "nothing frozen");
        assert!(press_reads_list(true, true, Some(false)), "full screen on the main screen, a press on the other one");
        assert!(!press_reads_list(true, true, Some(true)), "a press on the full-screen app's own screen");
        assert!(!press_reads_list(true, true, None), "frozen, and no managed screen under the press");
        assert!(press_reads_list(true, false, None), "the Dock's bar is still looked up");
        assert!(!press_reads_list(false, false, Some(false)), "not managing windows");
    }

    /// The review's U1: with the strip hidden no switch animates, so the strip is asked to
    /// keep no full-size snapshot (nothing is captured); shown, every window is kept ready.
    /// K8: `ctl state`'s live frames are the window server's, for every window it describes,
    /// on screen (a shown window) or not (one on another Desktop).
    #[test]
    fn ctl_state_reads_live_frames_from_the_window_server() {
        let on = listed(3, 7, Rect::new(10.0, 40.0, 800.0, 600.0));
        let off = windows::CgWindow { onscreen: false, ..listed(4, 7, Rect::new(1500.0, 900.0, 640.0, 480.0)) };
        let live = described_bounds(&[on, off]);
        assert_eq!(live.get(&3), Some(&Rect::new(10.0, 40.0, 800.0, 600.0)));
        assert_eq!(live.get(&4), Some(&Rect::new(1500.0, 900.0, 640.0, 480.0)));
        assert_eq!(live.get(&5), None);
    }

    #[test]
    fn a_hidden_strip_keeps_no_snapshots_ready() {
        let w = |id| CardWindow { id, pid: 7, frame: Rect::new(0.0, 0.0, 800.0, 600.0) };
        assert!(kept_ready(false, vec![w(1), w(2)]).is_empty());
        assert_eq!(kept_ready(true, vec![w(1), w(2)]).iter().map(|w| w.id).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn a_finder_that_keeps_failing_is_read_less_and_less_often() {
        assert_eq!(items_retry(1), PATCH_ITEMS_RETRY, "the first failure: the next tick");
        assert_eq!(items_retry(2), Duration::from_millis(500));
        assert_eq!(items_retry(3), Duration::from_secs(1));
        assert_eq!(items_retry(5), PATCH_ITEMS_RETRY_MAX);
        assert_eq!(items_retry(u32::MAX), PATCH_ITEMS_RETRY_MAX);
        assert_eq!(items_retry(0), PATCH_ITEMS_RETRY);
    }

    #[test]
    fn a_quit_mid_batch_still_brings_back_what_the_model_shows() {
        let id = WindowId(3);
        let frame = Rect::new(10.0, 40.0, 300.0, 200.0);
        assert!(runs_after_quit(&Action::SetFrame { id, frame }));
        assert!(!runs_after_quit(&Action::Hide { id }));
        assert!(!runs_after_quit(&Action::Raise { id }));
        assert!(!runs_after_quit(&Action::Focus { id }));
        assert!(!runs_after_quit(&Action::StateChanged));
    }

    /// 11-02: a card click 0.56 s after another: the first switch's held batch still brings
    /// its windows, without its focus; a batch that parks or focuses supersedes it, one that
    /// only places a window does not.
    #[test]
    fn a_superseded_switch_lands_without_its_focus() {
        let frame = Rect::new(0.0, 0.0, 10.0, 10.0);
        let held = vec![
            Action::SetFrame { id: WindowId(7), frame },
            Action::Raise { id: WindowId(7) },
            Action::Focus { id: WindowId(7) },
            Action::StateChanged,
        ];
        let (kept, dropped) = without_focus(held);
        assert_eq!(kept, vec![Action::SetFrame { id: WindowId(7), frame }, Action::Raise { id: WindowId(7) }, Action::StateChanged]);
        assert_eq!(dropped, vec![7]);
        assert!(supersedes(&[Action::Hide { id: WindowId(1) }, Action::SetFrame { id: WindowId(2), frame }]));
        assert!(supersedes(&[Action::Focus { id: WindowId(2) }]));
        assert!(!supersedes(&[Action::SetFrame { id: WindowId(2), frame }, Action::StateChanged]));
        assert!(!supersedes(&[]));
    }

    /// 11-02: Telegram, hidden by us, posted its window's focus a second late, and the stage
    /// switched to it; Mail's, an app behind, the same. A focus switches only in the active app
    /// (or with the button down on a sliver), never in a hidden one.
    #[test]
    fn only_a_focus_in_the_app_in_front_switches() {
        assert!(focus_may_switch(Some(false), Some(true), false), "the active app's: ⌘`, a click");
        assert!(!focus_may_switch(Some(false), Some(false), false), "an app behind: its own, or a late echo");
        assert!(focus_may_switch(Some(false), Some(false), true), "a sliver grabbed, its app on its way to the front");
        assert!(!focus_may_switch(Some(true), Some(true), true), "a hidden app's window is never the user's");
        assert!(!focus_may_switch(Some(true), Some(false), false));
        assert!(!focus_may_switch(None, None, true), "the app is gone");
    }

    /// The review of 6X, M2: only an unpark tries a busy app again; a switch's arrivals
    /// (unveiled behind their snapshots) keep the single try and are never interrupted.
    #[test]
    fn only_an_unpark_retries_a_busy_app() {
        assert!(Batch::Unpark.retries() && !Batch::Unpark.interruptible());
        assert!(!Batch::Unveil.retries() && !Batch::Unveil.interruptible());
        assert!(!Batch::User.retries() && Batch::User.interruptible());
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
        let owners: RawSettings = [("AppWindowGroupingBehavior".to_string(), 1), ("GloballyEnabled".to_string(), 0)].into();
        assert!(!wallpaper_reveal_enabled(&native_settings(&owners)), "the owner's Mac: the key is absent");
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
        // The model and the controller's decision, wired as `mouse_down` wires them: the owner's log
        // of 10-12 had two clicks 40 ms apart reveal and bring the stage straight back.
        use crate::engine::model::{ModelOptions, Observation};
        use crate::engine::rules::{Decision, Placement, RuleAction};
        use crate::engine::testkit::{meta, model_with};
        let only_sm: RawSettings = [("EnableStandardClickToShowDesktop".to_string(), 0)].into();
        let settings = native_settings(&only_sm);
        let mut m = model_with(ModelOptions::default());
        let window = meta(1, 10, "com.example.a");
        let obs = Observation { frames: [(window.id, window.frame)].into(), z_order: vec![window.id], focused: Some(window.id) };
        m.add_window(window.clone(), &Decision { action: RuleAction::New, placement: Placement::AsIs, ..Decision::default() }, &obs);
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
                Action::Hide { id } => format!("park {}", id.0),
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
            Action::Hide { id: WindowId(1) },
            Action::Hide { id: WindowId(2) },
            Action::SetFrame { id: WindowId(4), frame: r },
            Action::SetFrame { id: WindowId(3), frame: r },
            Action::Raise { id: WindowId(4) },
            Action::Raise { id: WindowId(3) },
            Action::Focus { id: WindowId(3) },
            Action::StateChanged,
        ];
        let arriving: HashSet<u32> = [3, 4].into();
        let (now, later) = split_covered(batch, &arriving, |_| false);
        assert_eq!(ids(&now), ["park 1", "park 2", "state", "focus 3"], "the keyboard goes to the new stage with the parks");
        assert_eq!(
            ids(&later),
            ["frame 4", "frame 3", "raise 4", "raise 3", "focus 3"],
            "the windows arrive behind their snapshots, the key window in front last"
        );
        // A batch without a focus focuses nothing early.
        let (now, later) = split_covered(vec![Action::SetFrame { id: WindowId(3), frame: r }], &arriving, |_| false);
        assert!(now.is_empty());
        assert_eq!(ids(&later), ["frame 3"]);
    }

    /// Live 11-14 (the Spaces engine, two Desktops): a card click on Desktop 2 for the stage of
    /// an app with windows on Desktop 1 focused its window while it was still in its stage
    /// Space; macOS took the activation to Desktop 1 and back. A focus the engine says must
    /// wait goes with the arrivals only; one on a window already shown still goes at once.
    #[test]
    fn a_focus_on_a_window_hidden_off_its_desktop_waits_for_its_show() {
        let r = Rect::new(100.0, 100.0, 400.0, 300.0);
        let batch = vec![
            Action::Hide { id: WindowId(1) },
            Action::SetFrame { id: WindowId(3), frame: r },
            Action::Raise { id: WindowId(3) },
            Action::Focus { id: WindowId(3) },
            Action::StateChanged,
        ];
        let arriving: HashSet<u32> = [3].into();
        let (now, later) = split_covered(batch.clone(), &arriving, |w| w == WindowId(3));
        assert_eq!(ids(&now), ["park 1", "state"], "nothing activates the app before its window is on the Desktop");
        assert_eq!(ids(&later), ["frame 3", "raise 3", "focus 3"], "the focus comes once the window is shown");
        // A focus on a window that does not arrive (shown already) is not held.
        let (now, _) = split_covered(batch, &HashSet::new(), |_| true);
        assert_eq!(ids(&now).last().map(String::as_str), Some("focus 3"));
    }

    /// A switch to stage {3, 4} from {1, 2}, as `Model::switch_to` gives it.
    fn activation_batch() -> Vec<Action> {
        let r = Rect::new(100.0, 100.0, 400.0, 300.0);
        vec![
            Action::Hide { id: WindowId(1) },
            Action::Hide { id: WindowId(2) },
            Action::SetFrame { id: WindowId(4), frame: r },
            Action::SetFrame { id: WindowId(3), frame: r },
            Action::Raise { id: WindowId(4) },
            Action::Raise { id: WindowId(3) },
            Action::Focus { id: WindowId(3) },
            Action::StateChanged,
        ]
    }

    #[test]
    fn an_activation_grows_its_windows_from_their_card_with_the_keyboard_at_once() {
        // Windows 3 and 4 are the activated app's (pid 30); 1 and 2 another's.
        let pid_of = |id: u32| Some(if id >= 3 { 30 } else { 10 });
        let run = activation_run(true, 30, &activation_batch(), pid_of);
        assert!(run.now.is_empty(), "no window taken out of the transition: every incoming one grows from its card");
        assert_eq!(run.focus, Some(WindowId(3)), "the keyboard goes to the activated window as the transition is sent");
        assert_eq!(run.fallback, [3, 4].into(), "brought at once should the corner not be coverable");
        // The switch's focus on another app's window: not hurried, so that app is not raised
        // in the corner before the overlay covers it. (An activation's own switch keys the
        // activated app's window, so this is only a batch the activation did not choose.)
        let other = |id: u32| Some(if id == 3 { 10 } else { 30 });
        assert_eq!(activation_run(true, 30, &activation_batch(), other).focus, None);
        // Nothing to switch: nothing to do.
        assert_eq!(activation_run(true, 30, &[], pid_of), ActivationRun::default());
    }

    #[test]
    fn a_dock_click_on_the_other_app_of_a_shared_stage_gives_it_the_keyboard_and_the_front() {
        // The owner's 10-27 log: one stage holds Mail (B, the key) and Calendar (A); a Dock click on
        // Calendar switched with Mail's key, no early focus, and our focus activated Mail.
        use crate::engine::model::ModelOptions;
        use crate::engine::rules::{Decision, Placement, RuleAction};
        use crate::engine::testkit::{Desk, child, meta, model_with};
        let mut m = model_with(ModelOptions::default());
        let mut d = Desk::default();
        let a = meta(1, 10, "com.example.a");
        let sheet = child(4, 10, Some(1));
        let b = meta(2, 11, "com.example.b");
        let c = meta(3, 12, "com.example.c");
        for (w, action) in [(a, RuleAction::Current), (sheet, RuleAction::Current), (b, RuleAction::Current), (c, RuleAction::New)] {
            d.open(&w);
            let batch = m.add_window(w, &Decision { action, placement: Placement::AsIs, ..Decision::default() }, &d.obs());
            d.apply(&m, &batch);
        }
        let pid_of = |id: u32| m.window(WindowId(id)).map(|w| w.meta.pid);
        let mut m2 = m.clone();
        let batch = m2.app_activated(10, &Observation { focused: None, ..d.obs() });
        let run = activation_run(true, 10, &batch, pid_of);
        assert_eq!(run.focus, Some(WindowId(1)), "the keyboard goes to the clicked app's window before the motion");
        let arriving: HashSet<u32> = [1, 2, 4].into();
        let (now, later) = split_covered(batch, &arriving, |_| false);
        assert_eq!(ids(&now).last().map(String::as_str), Some("focus 1"));
        let tail: Vec<String> = ids(&later).into_iter().filter(|a| a.starts_with("raise") || a.starts_with("focus")).collect();
        assert_eq!(tail, ["raise 2", "raise 1", "raise 4", "focus 1"], "the other app's window raised first, the key and its sheet last");
    }

    #[test]
    fn with_activation_grows_off_the_activated_windows_come_first_and_at_once() {
        let pid_of = |id: u32| Some(if id >= 3 { 30 } else { 10 });
        let run = activation_run(false, 30, &activation_batch(), pid_of);
        assert_eq!(run.now, [3, 4].into(), "the activated app's arrivals, outside the transition (6V)");
        assert_eq!(run.focus, None, "the focus runs with the parks, as before");
        assert!(run.fallback.is_empty());
        // Another app's arrivals in the same stage stay in the transition.
        let mixed = |id: u32| Some(if id == 4 { 30 } else { 10 });
        assert_eq!(activation_run(false, 30, &activation_batch(), mixed).now, [4].into());
    }

    #[test]
    fn an_activation_whose_corner_cannot_be_covered_arrives_at_once_as_with_it_off() {
        // 8L2: the patch off, an icon under the corner, its picture not ready...: the grow
        // would show the windows macOS raised in the corner, so they come at once (6V).
        let pid_of = |id: u32| Some(if id >= 3 { 30 } else { 10 });
        let mut run = activation_run(true, 30, &activation_batch(), pid_of);
        run.arrive_now("off");
        let off = activation_run(false, 30, &activation_batch(), pid_of);
        assert_eq!((&run.now, run.focus), (&off.now, off.focus), "exactly the instant arrival: the focus with the parks");
        assert!(run.fallback.is_empty());
        // A mixed stage: only the activated app's windows (another app's are not raised).
        let mixed = |id: u32| Some(if id == 4 { 30 } else { 10 });
        let mut run = activation_run(true, 30, &activation_batch(), mixed);
        run.arrive_now("off");
        assert_eq!(run.now, [4].into());
        // Each reason logged once.
        let mut said = HashSet::new();
        note_fallback(&mut said, "off");
        note_fallback(&mut said, "off");
        note_fallback(&mut said, "a desktop icon lies under the corner");
        assert_eq!(said.len(), 2);
    }

    /// A window of the owner's 2560x1440 Mac (10-16), as the window server lists it.
    fn listed(id: u32, pid: i32, bounds: Rect) -> windows::CgWindow {
        windows::CgWindow { id, pid, owner: String::new(), name: None, layer: 0, bounds, onscreen: true, alpha: 1.0 }
    }

    #[test]
    fn a_ctl_question_never_lands_a_running_switch() {
        // 8L3: the checks poll `state` while the grow runs; that poll must not bring the
        // incoming windows to their frames ahead of their snapshots.
        for q in [Command::State, Command::Space, Command::Dump, Command::Adopted(vec![1])] {
            assert!(!ctl_lands_switch(&q), "{q:?}");
        }
        for c in [Command::Switch(1), Command::Send(1, None), Command::New(None), Command::Reload, Command::UnparkAll] {
            assert!(ctl_lands_switch(&c), "{c:?}");
        }
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
    fn our_own_activation_never_lands_a_held_switch_through_a_window_still_away() {
        // 11-09 10:06:42.737: a card click; the keyboard goes to Messages (pid 740) while its
        // and Telegram's windows wait behind their growing snapshots. Messages also has window
        // 176 waiting off screen since 09:56, Safari (738) its dialog 314.
        let (messages, safari) = (740, 738);
        let pid_of = |id: u32| match id {
            176 => Some(messages),
            314 => Some(safari),
            _ => None,
        };
        let unplaced = [176u32, 314];
        let listed: HashSet<u32> = [134, 166, 299].into();
        let mut e = Echoes::default();
        let t0 = Instant::now();
        e.touch_app(messages, t0);
        // 24 ms later macOS says Messages activated: its waiting windows are offered first,
        // and only those that show reach `on_found`, whose placing lands the switch.
        let waiting = unplaced.iter().copied().filter(|id| pid_of(*id) == Some(messages));
        assert!(offered_at_notice(waiting, &listed).is_empty(), "176 is still away: nothing is placed, the switch stays held");
        assert!(e.is_app_echo(messages, t0 + Duration::from_millis(24)), "and the activation is our echo: nothing below runs for it");
        // Safari's the same at 10:06:46.411 (its dialog 314 away).
        let waiting = unplaced.iter().copied().filter(|id| pid_of(*id) == Some(safari));
        assert!(offered_at_notice(waiting, &listed).is_empty());
        // A window of the app that does show now (un-minimized, say) is a new window: placed.
        let shows: HashSet<u32> = [134, 166, 176].into();
        assert_eq!(offered_at_notice(unplaced, &shows), vec![176]);
    }

    #[test]
    fn a_landing_focus_after_the_activation_awaits_no_second_one() {
        // The 11-09 suite: a card click to the fixture's stage focuses its window at once
        // (46.981) and macOS reports the activation (46.996); the landing focuses it again
        // (~47.5). A card click to Alpha follows (49.8, its activation ours), then Alpha quits
        // and macOS activates the fixture (51.939): the user's, not an echo.
        let (fixture, alpha) = (67303, 67932);
        let mut e = Echoes::default();
        let t0 = Instant::now();
        e.touch_app(fixture, t0);
        assert!(e.is_app_echo(fixture, t0 + Duration::from_millis(15)));
        e.touch_app(fixture, t0 + Duration::from_millis(560));
        e.touch_app(alpha, t0 + Duration::from_millis(2850));
        assert!(e.is_app_echo(alpha, t0 + Duration::from_millis(2910)));
        assert!(!e.is_app_echo(fixture, t0 + Duration::from_millis(4960)), "macOS's activation after the front app quit");
        // A landing focus that comes before the activation is reported still awaits it.
        let mut e = Echoes::default();
        e.touch_app(alpha, t0);
        e.touch_app(fixture, t0 + Duration::from_millis(10));
        e.touch_app(fixture, t0 + Duration::from_millis(500));
        assert!(e.is_app_echo(fixture, t0 + Duration::from_millis(1500)), "late, but the one we asked for");
    }

    #[test]
    fn our_later_switch_ends_an_older_apps_late_echo() {
        // The suite on 9af41d3 (11-14, the live landing): the switch to the fixture's stage
        // focuses its window (14.578, again at the unveil 15.121) and no activation comes; a
        // switch to Alpha's new stage (16.880) is reported (16.937); the test's ⌘Tab-style
        // activation of the fixture (18.265) is the user's, not that late echo.
        let (fixture, alpha) = (85618, 86476);
        let mut e = Echoes::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        e.touch_app(fixture, at(0));
        e.touch_app(fixture, at(543));
        e.touch_app(alpha, at(2302));
        assert!(e.is_app_echo(alpha, at(2359)));
        assert!(!e.is_app_echo(fixture, at(3687)), "the user's activation brings the fixture's stage");
        // Only an awaited activation ends the others': one reported within the echo window
        // of a focus that awaited none (the app was in front) leaves them.
        let mut e = Echoes::default();
        e.touch_app(fixture, at(0));
        e.front = Some(alpha);
        e.touch_app(alpha, at(1000));
        assert!(e.is_app_echo(alpha, at(1050)));
        assert!(e.is_app_echo(fixture, at(1400)), "still the late echo we asked for");
        // Two apps focused by one batch (or two quick switches): the first one's late echo
        // stays ours after the second one's activation.
        let (mail, slack) = (20, 10);
        let mut e = Echoes::default();
        e.touch_app(slack, at(0));
        e.touch_app(mail, at(5));
        assert!(e.is_app_echo(mail, at(30)));
        assert!(e.is_app_echo(slack, at(1500)), "busy, but the one we asked for");
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

    /// A 1440-point-wide display at the origin.
    const DISPLAY: Rect = Rect { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 };

    #[test]
    fn an_activation_takes_the_window_servers_frame_when_the_tracked_one_is_stale() {
        let tracked = Rect::new(100.0, 200.0, 800.0, 600.0);
        // An app that moved its window without a notice: the window server's bounds win.
        let moved = Rect::new(400.0, 260.0, 800.0, 600.0);
        assert_eq!(activation_frame(tracked, Some(moved), DISPLAY, false, false), moved);
        let resized = Rect::new(100.0, 200.0, 900.0, 600.0);
        assert_eq!(activation_frame(tracked, Some(resized), DISPLAY, false, false), resized);
        // CG's whole points against a fractional AX frame: the exact tracked one stays.
        let exact = Rect::new(100.5, 200.25, 800.0, 599.5);
        assert_eq!(activation_frame(exact, Some(tracked), DISPLAY, false, false), exact);
        // Not listed on screen: the tracked frame.
        assert_eq!(activation_frame(tracked, None, DISPLAY, false, false), tracked);
    }

    #[test]
    fn an_activation_mid_slide_keeps_the_tracked_frame() {
        let tracked = Rect::new(100.0, 200.0, 800.0, 600.0);
        // The Desktop slides before the Space change's notice: shifted sideways on screen.
        let slid = Rect::new(-1100.0, 200.0, 800.0, 600.0);
        assert_eq!(activation_frame(tracked, Some(slid), DISPLAY, false, false), tracked);
        let slid_right = Rect::new(1300.0, 200.0, 800.0, 600.0);
        assert_eq!(activation_frame(tracked, Some(slid_right), DISPLAY, false, false), tracked);
        // Shifted off the window's display altogether: tracked.
        let off = Rect::new(2100.0, 200.0, 800.0, 600.0);
        assert_eq!(activation_frame(tracked, Some(off), DISPLAY, false, true), tracked, "off the display");
        // A Space change pending or settling: tracked, whatever the list says.
        let moved = Rect::new(400.0, 260.0, 800.0, 600.0);
        assert_eq!(activation_frame(tracked, Some(moved), DISPLAY, true, true), tracked);
        // A sideways move while another window of the Desktop is listed where tracked: no
        // slide (it would shift them all), so the app's move wins.
        assert_eq!(activation_frame(tracked, Some(slid_right), DISPLAY, false, true), slid_right);
        // A move with a vertical part is never a slide.
        assert_eq!(activation_frame(tracked, Some(moved), DISPLAY, false, false), moved);
    }

    #[test]
    fn a_frame_read_that_times_out_takes_the_window_servers_bounds() {
        // W3: a busy app's timed-out read kept the tracked frame, and for an app that posts no
        // move notice that is where the window was before the user moved it.
        let display = Display { id: DisplayId(1), key: crate::engine::displays::ScreenKey::NONE, frame: DISPLAY, visible: DISPLAY, corner: Corner::BottomRight };
        let read = |id: u32, frame: Rect, fresh: bool| FrameRead { id: WindowId(id), display, sticky: false, frame, fresh };
        let tracked = Rect::new(100.0, 200.0, 800.0, 600.0);
        let moved = Rect::new(400.0, 260.0, 800.0, 600.0);
        let other = Rect::new(600.0, 100.0, 500.0, 400.0);
        let listed: HashMap<u32, Rect> = [(1, moved), (2, other)].into_iter().collect();
        let frames = observed_frames(&[read(1, tracked, false), read(2, other, true)], &listed, false);
        assert_eq!(frames[&WindowId(1)], moved);
        assert_eq!(frames[&WindowId(2)], other, "a frame read stands");
        // Settling after a Space change: the list may be a slide's, the tracked frame stays.
        let frames = observed_frames(&[read(1, tracked, false)], &listed, true);
        assert_eq!(frames[&WindowId(1)], tracked);
        // Shifted sideways only, no window of the display listed where it was read: a slide.
        let slid: HashMap<u32, Rect> = [(1, tracked.translate(-700.0, 0.0)), (2, other.translate(-700.0, 0.0))].into_iter().collect();
        let frames = observed_frames(&[read(1, tracked, false), read(2, other, true)], &slid, false);
        assert_eq!(frames[&WindowId(1)], tracked);
        // A read that answered is never replaced.
        let frames = observed_frames(&[read(1, tracked, true)], &listed, false);
        assert_eq!(frames[&WindowId(1)], tracked);
    }
}
