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
//!   windows here. The app switcher (Command held) never counts. With
//!   `[stages] new_apps_join_current` on, a newly opened app's first window joins the current
//!   stage without Shift, and Shift gives it a stage of its own (`Decision::for_new_window`).
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

use super::ax::{AxElement, AxError};
use super::ctl::Command;
use super::input::{self, InputEvent, InputKind, Modifiers, Triggers};
use super::native_settings::RawSettings;
use super::observe::{Observer, WinEvent, WindowKind};
use super::safety::{self, ParkedEntry};
use super::{capture_pace, desktop, desktop_shot, hidden_tabs, logging, paths, session, windows};
use crate::config::{Config, ConfigStore, FileAction, FitMode, HotkeyCommand, HotkeyConfig, StripStyle};
use crate::engine::busy_apps::{BUSY_BACKOFF, BusyApps, Episode, OffScreen};
use crate::engine::corner_patch::{CoverPicture, Refused, Staying, SwitchCover};
use crate::engine::desktop_echo::{Cause, DesktopEcho};
use crate::engine::displays::on_other_display;
use crate::engine::focus_replay::{FocusReplay, Wanted};
use crate::engine::hidden_tab::Candidate;
use crate::engine::leftovers;
use crate::engine::park::{Corner, ParkRequest, choose_corner};
use crate::engine::recovery::{self, Known};
use crate::engine::sliver;
use crate::engine::fullscreen::{self, Seen, Settle, Verdict};
use crate::engine::overview::{self, FOREIGN_SETTLE, Overview, OverviewWatch, Persisting, Sighting};
use crate::engine::geometry::{Point, Rect};
use crate::engine::model::{
    Action, Closing, Display, DisplayId, Home, Model, Observation, SpaceId, StageId, Target, WindowId, WindowMeta, WindowRole, WindowState,
};
use crate::engine::stage_area::{FitOptions, capacity_for, stage_inset, strip_width};
use crate::engine::persist::{LaunchHints, StateFile};
use crate::engine::restore::{self, AppQuiet, End, Restore, Settling};
use crate::engine::rules::{Decision, RuleAction, ShiftEffect};
use crate::engine::settings::{NativeSettings, PrefValue};
use crate::logbook::counters::{self, Count};
use crate::ui::strip_model::{self, CardSnapshot, CardWindow, StripDrop, StripEvent, StripSnapshot, Transition};
use crate::ui::{motion_math, transition_plan};

mod arrival;
mod dock_order;
mod hide;

/// The windows read at a card's press serve its click this long (a press held longer reads
/// them again at the release).
const PRESSED_OBS_MAX: Duration = Duration::from_secs(1);

/// How long focus and activation events for windows and apps we just touched are ignored
/// by default (`[windows] focus_echo_ms`).
pub const ECHO_WINDOW: Duration = Duration::from_millis(700);

/// Quiet time before `state.json` is written after a change.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// How long before its app's quit notice a destroyed window still counts as quit with it (an
/// app closes its windows as it quits, a moment before macOS says it terminated; round 8H).
const CLOSING_KEEP: Duration = Duration::from_secs(10);

/// Now, in Unix seconds (0 when the clock is before 1970).
fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A busy stream of changes is still saved at least this often.
const SAVE_MAX_DELAY: Duration = Duration::from_secs(3);

/// How often config.toml's modification time is checked.
const CONFIG_POLL: Duration = Duration::from_secs(1);

/// How long the Dock's bar as read for the inert-slivers tap is trusted ([`Controller::refresh_dock_bar`]).
const DOCK_BAR_FRESH: Duration = Duration::from_secs(2);

/// How long a wallpaper click that macOS answers with Show Desktop hides the corner patch
/// before the overview is listed (seen ~0.7 s after the click); once listed, the overview
/// keeps it hidden ([`Controller::update_patch`]).
const PATCH_REVEAL_HOLD: Duration = Duration::from_millis(1500);

/// How long Finder's desktop icons as read for the corner patch are trusted (a new
/// rectangle reads them at once).
const PATCH_ITEMS_FRESH: Duration = Duration::from_secs(5);

/// How soon a read of Finder's desktop icons that failed is tried again (the next tick),
/// and how soon after a mouse-up (a desktop icon may have been dropped) they are read.
const PATCH_ITEMS_RETRY: Duration = Duration::from_millis(250);

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

/// Finder's desktop icons for the corner patch, read on a thread of their own
/// ([`super::desktop_items::start_read`]).
#[derive(Default)]
struct PatchItems {
    /// The icons (grown by their labels) of the last read that knew them; `None` before one,
    /// or after a read found too many to read: the patch stays off.
    known: Option<Vec<Rect>>,
    /// The rectangle the last read was started for.
    read_for: Option<Rect>,
    /// When the next read is due (`None`: at once).
    due: Option<Instant>,
    /// The read running.
    reading: Option<std::sync::mpsc::Receiver<Option<crate::engine::corner_patch::DesktopIcons>>>,
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

/// No such check this soon after a Space change (the list may still be the old Space's).
const SPACE_SETTLE: Duration = Duration::from_secs(1);

/// A window counts as moved to or gone from the shown Space after this many checks in a row.
const SPACE_CHECKS: u32 = 2;

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
/// raises, so the key window ends in front) wait. An activation's early focus
/// ([`Controller::focus_now`]) comes before all this and changes none of it.
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

/// Switch cover `c` as it can be drawn now (round 8L3), or why not: where nothing lies on the
/// desktop, the corner patch's picture (`wallpaper`: ready for that rectangle) or, when that
/// is not ready (an aerial, a picture still rendering), the desktop's still; where a desktop
/// icon lies or the icons are not known, only the desktop's still (`desktop`: ready for that
/// rectangle), since the patch's picture would hide the icon. The reason given is the
/// picture's that was wanted first.
fn resolve_cover(
    c: SwitchCover,
    wallpaper: impl FnOnce(Rect) -> Result<(), &'static str>,
    desktop: impl FnOnce(Rect) -> Result<(), &'static str>,
) -> Result<SwitchCover, &'static str> {
    let rect = c.rect;
    let picture = match c.picture {
        CoverPicture::Wallpaper => match wallpaper(rect) {
            Ok(()) => CoverPicture::Wallpaper,
            Err(why) => desktop(rect).map(|()| CoverPicture::Desktop).map_err(|_| why)?,
        },
        CoverPicture::Desktop => desktop(rect).map(|()| CoverPicture::Desktop)?,
    };
    Ok(SwitchCover { picture, ..c })
}

/// The windows of `parked` (the model's parked windows) that window server list `list` shows
/// in the park zone of the display `frame` (visible frame `visible`, park `corner`): parked in
/// fact. One the model has parked but the window server lists elsewhere (a park that did not
/// hold, an app that put its window back) shows there: never a sliver of the corner patch nor
/// of a switch's cover (round 8L6: on the owner's Mac one listed at 533,184 1741x1173 grew both
/// to 473,124 2087x1316 for minutes), and a window that stays shown through a switch.
fn parked_in_zone(list: &[windows::CgWindow], parked: impl IntoIterator<Item = u32>, frame: Rect, visible: Rect, corner: Corner) -> HashSet<u32> {
    let parked: HashSet<u32> = parked.into_iter().collect();
    let (inside, elsewhere): (Vec<_>, Vec<_>) = list
        .iter()
        .filter(|w| parked.contains(&w.id))
        .partition(|w| crate::engine::park::is_parked_in_at(w.bounds, frame, visible, corner));
    note_parked_elsewhere(&elsewhere);
    inside.iter().map(|w| w.id).collect()
}

/// Logs once per window and listed frame a window the model has parked that the window server
/// lists outside the park zone (8L6 review: the owner's 10-16 Fork window, listed at its frame
/// for minutes while parked; the next log settles whether the park held).
fn note_parked_elsewhere(elsewhere: &[&windows::CgWindow]) {
    static SEEN: Mutex<Vec<(u32, i64, i64)>> = Mutex::new(Vec::new());
    let Ok(mut seen) = SEEN.lock() else { return };
    for w in elsewhere {
        let key = (w.id, w.bounds.x.round() as i64, w.bounds.y.round() as i64);
        if seen.contains(&key) {
            continue;
        }
        if seen.len() >= 256 {
            seen.clear();
        }
        seen.push(key);
        tracing::debug!(id = w.id, pid = w.pid, bounds = %format!("{:.0},{:.0} {:.0}x{:.0}", w.bounds.x, w.bounds.y, w.bounds.w, w.bounds.h), onscreen = w.onscreen,
            "a window the model has parked is listed outside the park zone");
    }
}

/// The bounds, as listed in `list`, of a switch's `arriving` windows macOS has raised above the
/// corner patch, which its cover must hide ([`crate::engine::corner_patch::CoverFacts::raised`]):
/// those waiting in the park zone (`in_zone`), and the `activated` app's wherever macOS put
/// them (one brought into view, outside the corner's square, means no cover: round 8L6).
/// Normal, drawn, not ours (`own`).
fn raised_windows(list: &[windows::CgWindow], own: i32, arriving: &HashSet<u32>, activated: &HashSet<u32>, in_zone: impl Fn(Rect) -> bool) -> Vec<Rect> {
    list.iter()
        .filter(|w| w.layer == 0 && w.alpha > 0.0 && w.pid != own && arriving.contains(&w.id))
        .filter(|w| in_zone(w.bounds) || activated.contains(&w.id))
        .map(|w| w.bounds)
        .collect()
}

/// The windows of window server list `list` (front to back) that stay shown through a switch
/// ([`Staying`]): normal, drawn, not ours (`own`), neither leaving (`outgoing`, the switch's
/// parks: the outgoing stage's windows never count, however much of the screen they fill),
/// nor arriving, nor parked in the park zone (`parked`, [`parked_in_zone`]): pinned windows,
/// unmanaged ones (another app's, an ignored app's), a dialog of one, and one the model has
/// parked that the window server lists elsewhere (8L6). `snapshot`: whether the strip holds a
/// full-size snapshot of a window, for the cover to draw it (round 8L5).
fn staying_windows(
    list: &[windows::CgWindow],
    own: i32,
    outgoing: &HashSet<u32>,
    arriving: &HashSet<u32>,
    parked: &HashSet<u32>,
    snapshot: impl Fn(u32) -> bool,
) -> Vec<Staying> {
    list.iter()
        .filter(|w| w.layer == 0 && w.alpha > 0.0 && w.pid != own)
        .filter(|w| !outgoing.contains(&w.id) && !arriving.contains(&w.id) && !parked.contains(&w.id))
        .map(|w| Staying { id: w.id, bounds: w.bounds, snapshot: snapshot(w.id) })
        .collect()
}

/// Test only (`STAGE_MANAGER_COVER=desktop`, honoured with `STAGE_MANAGER_TEST` set): a
/// switch's cover is the desktop's still even where no icon lies under the corner, and the
/// still is kept fresh, so the capture phase sees the still-based cover on any desktop
/// (round 8L4).
fn cover_desktop_forced() -> bool {
    static FORCED: OnceLock<bool> = OnceLock::new();
    *FORCED.get_or_init(|| {
        std::env::var_os("STAGE_MANAGER_TEST").is_some() && std::env::var("STAGE_MANAGER_COVER").is_ok_and(|v| v == "desktop")
    })
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
    !matches!(cmd, Command::State | Command::Space | Command::Dump | Command::Adopted(_))
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
    /// corner, which the overlay covers ([`crate::engine::corner_patch::switch_cover`]).
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
    /// A parked window the model let go ([`Controller::watch_forgotten`]): quiet in the log
    /// until it is seen in the park zone a second time (closing a parked window lists it there
    /// for a moment; the review of 6X, L6).
    forgotten: Forgotten,
    /// The element was looked up afresh in its app's list once already (L5).
    refreshed: bool,
    /// Why the model let it go (the review of 8J): only a window reported destroyed while
    /// the window server still listed it may be judged a hidden tab.
    why: LetGo,
}

/// Why the model let a parked window go ([`Unrestored::why`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LetGo {
    /// Not let go: a failed restore (the model still has it).
    Kept,
    /// Accessibility reported it destroyed while the window server still listed it: how
    /// macOS reports a tab no longer selected (or a window closing, listed for a moment).
    Destroyed,
    /// It left its Space or display, or anything else: a real window, never judged a tab.
    Other,
}

impl LetGo {
    /// What the hidden-tab test may judge of a window let go so (8J): only the tab's own
    /// signature, a window reported destroyed, is ever a candidate.
    fn candidate(self) -> Candidate {
        match self {
            Self::Destroyed => Candidate::Destroyed,
            Self::Kept | Self::Other => Candidate::Real,
        }
    }
}

/// How often a forgotten parked window was seen in the park zone ([`Unrestored::forgotten`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Forgotten {
    /// Not a forgotten window: a failed restore.
    Not,
    /// Seen once, when the model let it go.
    Once,
    /// Seen again at a check: warned.
    Again,
}

impl Unrestored {
    fn failed(entry: ParkedEntry) -> Self {
        Self { entry, tries: 0, forgotten: Forgotten::Not, refreshed: false, why: LetGo::Kept }
    }
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

impl Forgotten {
    /// A check of a forgotten window gave `step`: whether to warn now (its second sighting
    /// in the park zone) and what it is now. A closed window, gone at the first check,
    /// never warns (L6).
    fn after(self, step: UnrestoredStep) -> (Self, bool) {
        match (self, step) {
            (Self::Once, UnrestoredStep::Retry | UnrestoredStep::Wait) => (Self::Again, true),
            (f, _) => (f, false),
        }
    }
}

/// Whether a failed move is worth looking the window up afresh in its app's list: only when
/// the element we kept is stale (`kAXErrorInvalidUIElement`: a window reported destroyed while
/// ordered out), and once per window, not at every check while it refuses to move (L5).
fn refresh_element(error: AxError, refreshed: bool) -> bool {
    !refreshed && error == AxError::INVALID_ELEMENT
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
///
/// A window the model forgot (it left its Space, its element was reported destroyed) while
/// the window server still lists it in the park zone is not done: the model forgetting a
/// window does not bring it back (6X: such windows sat parked with no run tracking them).
/// Unless it is a hidden tab (`hidden_tab`, 8J: [`crate::engine::hidden_tab`]): off screen,
/// out of Accessibility's reach beside its group's selected tab, nothing can move it and it
/// takes its group's frame once selected.
fn unrestored_step(model_parked: Option<bool>, listed: Option<(bool, bool)>, hidden_tab: bool) -> UnrestoredStep {
    match (model_parked, listed) {
        (Some(true), _) | (_, None) => UnrestoredStep::Done,
        (None, Some((false, true))) if hidden_tab => UnrestoredStep::Done,
        (_, Some((true, false))) => UnrestoredStep::Done,
        (_, Some((true, true))) => UnrestoredStep::Retry,
        (Some(false), Some((false, _))) | (None, Some((false, true))) => UnrestoredStep::Wait,
        (None, Some((false, false))) => UnrestoredStep::Done,
    }
}

/// Whether an [`Unrestored`] window is let go as a hidden tab (8J): only one the model no
/// longer has (`model_parked` `None`), let go as reported destroyed (`why`: the tab's own
/// signature; the review of 8J), listed off screen in the park zone (`listed`), and judged
/// one by `judge` with its [`Candidate`] (asked only then: it reads Accessibility).
fn let_go_as_tab(model_parked: Option<bool>, listed: Option<(bool, bool)>, why: LetGo, judge: impl FnOnce(Candidate) -> bool) -> bool {
    let candidate = why.candidate();
    model_parked.is_none() && candidate != Candidate::Real && listed == Some((false, true)) && judge(candidate)
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
    next_transition: u64,
    /// The transition whose outgoing windows just parked, for the next snapshot.
    parked_note: Option<u64>,
    /// The transition whose incoming windows are in place, with their frames read back, for
    /// the next snapshot.
    arrived_note: Option<(u64, Vec<(u32, Rect)>)>,
    /// How the next animated batch runs when an activation caused it ([`ActivationRun`],
    /// `[animation] activation_grows`).
    activation: ActivationRun,
    /// Why activations arrived at once instead of growing, each logged once ([`note_fallback`]).
    fallback_said: HashSet<&'static str>,
    /// `[windows] hide_parked_apps`: the apps we hid ([`hide`]).
    hides: hide::Hides,
    /// The corner patch's last plan sent to the main thread.
    patch_sent: Option<crate::ui::corner_patch::PatchPlan>,
    /// The corner patch's rectangle in use ([`crate::engine::corner_patch::steady_rect`]).
    patch_rect: Option<Rect>,
    /// The patch's rectangle in use on each Desktop (the engine's Space ids): each Desktop
    /// has a panel of its own ([`crate::ui::corner_patch`]), steady on its own.
    patch_rects: HashMap<u64, Rect>,
    /// Since when the window server's list shows the Desktop sliding before the Space
    /// change's notice ([`crate::engine::corner_patch::desktop_slides`]).
    patch_slide_since: Option<Instant>,
    /// The patch's order as last checked ([`crate::engine::corner_patch::order_step`]).
    patch_order: crate::engine::corner_patch::OrderState,
    /// Why the last plan hid the patch (logged once a change).
    patch_why: Option<&'static str>,
    /// The patch was once found ordered right (logged once).
    patch_honoured: bool,
    /// Finder's desktop icons as last read for the patch ([`PatchItems`]).
    patch_items: PatchItems,
    /// Widgets shown on the desktop were logged as possibly under the patch.
    patch_widgets_logged: bool,
    /// Tracked windows not given to the engine yet: not on screen when found (another
    /// Space, minimized, app hidden). Offered once they show.
    unplaced: BTreeSet<u32>,
    /// The launch's stages wait: the app could not see the Desktop ([`session::blindness`])
    /// when observation started; why and since when. Nothing is built or saved meanwhile.
    build_wait: Option<(session::Blind, Instant)>,
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
    /// When the last animated switch was sent (the desktop's still rests after a while with
    /// no input and no switch, [`desktop_shot::rest`]).
    last_switch: Option<Instant>,
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
    /// The Dock and its bar as the inert-slivers tap last got them, and when they were read
    /// ([`Self::publish_shield`]).
    dock_bar: Option<(Instant, sliver::DockBar)>,
    /// "Follow the Dock": when the Dock's order is read next, and one waiting for the strip
    /// to be still ([`dock_order`]).
    dock_order: dock_order::DockOrderWatch,
    /// The moves of an arrangement made at a Dock click, until the window server shows them.
    verify: Option<arrival::Verify>,
    /// The latest Dock press's prediction, until a Space change shows where it went.
    dock_seen: Option<arrival::DockSeen>,
    /// While set, a batch leaves the strip snapshot for later (`strip_dirty`): an arrival
    /// hands the strip one snapshot, in its final order.
    defer_present: bool,
    /// Managed windows of the shown Space missing from the screen: checks in a row.
    absent: HashMap<u32, u32>,
    /// Apps that did not answer an off-screen check in time, left alone a moment (Fable H1,
    /// [`crate::engine::busy_apps`]). A cell: the checks run from `&self` paths too.
    busy_apps: std::cell::RefCell<BusyApps>,
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
    /// A wallpaper click macOS answers with Show Desktop, seen before its window is: the
    /// corner patch hides at the click, not ~0.7 s later when the overview is listed.
    patch_reveal_hold: Option<Instant>,
    /// Shown windows (the active stage's, pinned ones) the window server did not show at the
    /// last strip snapshot (app hidden, minimized): left out of the strip's overlap test;
    /// a change seen by the once-a-second check sends a new snapshot.
    active_off_screen: BTreeSet<u32>,
    /// When each app with pending windows was last busy: a quiet one's pending windows are
    /// given up ([`crate::engine::restore`]).
    quiet: AppQuiet,
    /// When pending windows were last checked for giving up.
    quiet_checked: Instant,
    /// Each saved Space's last time shown (Unix seconds), from the loaded file and the saves
    /// since ([`StateFile::stamp`]).
    seen_times: HashMap<u64, u64>,
    /// The restore window's clock while it is open (its ends by itself: apps quiet, or
    /// [`crate::engine::restore::WINDOW`] after the stages were built from `state.json`); `None` once closed
    /// or when there is none.
    settling: Option<Settling>,
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
        let style = Arc::new(store.config().strip_style());
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
            closing: Vec::new(),
            triggers: Triggers::default(),
            press: None,
            drag: None,
            announce: false,
            before: transition_plan::Before::default(),
            pending: None,
            next_transition: 0,
            parked_note: None,
            arrived_note: None,
            activation: ActivationRun::default(),
            fallback_said: HashSet::new(),
            hides: hide::Hides::default(),
            patch_sent: None,
            patch_rect: None,
            patch_rects: HashMap::new(),
            patch_slide_since: None,
            patch_order: Default::default(),
            patch_why: None,
            patch_honoured: false,
            patch_items: PatchItems::default(),
            patch_widgets_logged: false,
            unplaced: BTreeSet::new(),
            build_wait: None,
            locked: false,
            lock_checked: None,
            ax_silent_since: None,
            ax_silent_rescan: None,
            fullscreen: false,
            space_pending: false,
            settle: Settle::default(),
            space_changed_at: None,
            space_changes: 0,
            last_switch: None,
            space_checked: Instant::now(),
            foreign: Persisting::default(),
            overview: OverviewWatch::default(),
            arrival_held: false,
            pressed_obs: None,
            focus_replay: FocusReplay::default(),
            ahead: None,
            dock_press: None,
            dock_pid: None,
            dock_bar: None,
            dock_order: dock_order::DockOrderWatch::default(),
            verify: None,
            dock_seen: None,
            defer_present: false,
            absent: HashMap::new(),
            busy_apps: std::cell::RefCell::new(BusyApps::default()),
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
            patch_reveal_hold: None,
            quiet: AppQuiet::default(),
            quiet_checked: Instant::now(),
            seen_times: HashMap::new(),
            settling: None,
            absent_due: None,
            front_after_restore: false,
            focus_after_restore: false,
        };
        logging::apply_config(&this.store.config().log);
        this.notify_config();
        this
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

    /// The observation an activation's switch needs, without asking any app: the z-order
    /// and the shown windows' frames from the window server's list (one read; a window not
    /// listed on screen keeps its tracked frame, kept current by its move and resize
    /// notices), and the focused window only when the switch depends on it
    /// ([`Model::activation_needs_focus`]), asked of the activated app alone with a bounded
    /// timeout. [`Self::observe`] asked every shown window's frame and the system-wide focus
    /// through Accessibility, which for an app busy activating (and the one it took over
    /// from) cost most of the activation's gap (6V). The tracked frame alone is not enough:
    /// an app that posts no move notice would get its windows back where they were before
    /// the user moved them (the switch keeps these frames as their restore frames).
    fn observe_activation(&self, ob: &Observer, pid: i32, needs_focus: bool) -> Observation {
        let Some(m) = &self.model else { return Observation::default() };
        let space = m.current_space(m.main_display().id);
        let list = windows::list_windows();
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
        let settling = self.space_pending || self.space_changed_at.is_some_and(|t| t.elapsed() < SPACE_SETTLE);
        let shown: Vec<(&WindowState, Rect, Option<Rect>)> = m
            .windows()
            .filter(|w| !w.parked && (w.space == space || w.sticky))
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
        let focused = if needs_focus { arrival::focused_window_of(pid) } else { None };
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

    /// Tells the inert-slivers tap ([`super::sliver_tap`], `[windows] inert_slivers`) the park
    /// corner's zone of each display with a parked window (the main display's always) and
    /// which windows sit parked (with incoming windows still held in the corner behind a
    /// transition's snapshots).
    ///
    /// Off while Mission Control, App Exposé or Show Desktop shows: their windows stand where
    /// the slivers are, and a press there is theirs. The Dock's bar comes from the cache
    /// [`Self::refresh_dock_bar`] keeps (no Accessibility call here: this runs after every
    /// batch).
    fn publish_shield(&self) {
        let Some(m) = &self.model else { return };
        let on = self.store.config().windows.inert_slivers && self.overview.showing().is_none();
        let dock = if on { self.dock_bar.as_ref().map(|(_, b)| b.clone()).unwrap_or_default() } else { sliver::DockBar::default() };
        let mut parked: HashSet<u32> =
            if on { m.windows().filter(|w| w.parked).map(|w| w.meta.id.0).collect() } else { HashSet::new() };
        if on && let Some(p) = &self.pending {
            parked.extend(p.entries.keys().copied());
        }
        let zones = if on {
            let mut used: Vec<DisplayId> = vec![m.main_display().id];
            for id in &parked {
                if let Some(w) = m.window(WindowId(*id)) {
                    let d = m.known_display(w.meta.display);
                    if !used.contains(&d) {
                        used.push(d);
                    }
                }
            }
            sliver::zones(used.into_iter().map(|id| {
                let d = m.display(id);
                (d.frame, d.visible, d.corner)
            }))
        } else {
            Vec::new()
        };
        super::sliver_tap::publish(sliver::Shield { on, zones, parked, dock });
    }

    /// From the tick: reads the Dock's pid and bar again (Accessibility, the Dock's bounded
    /// timeout) once the last read is [`DOCK_BAR_FRESH`] old, so its size, side, display or
    /// auto-hide changing is caught while nothing else happens; the tap gets it when it
    /// changed. Not while an overview shows (the shield is off then).
    fn refresh_dock_bar(&mut self, now: Instant) {
        let windows = &self.store.config().windows;
        if !(windows.inert_slivers || windows.corner_patch) || self.overview.showing().is_some() {
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
            self.publish_shield();
        }
    }

    /// Finder's desktop icons for the patch at `rect` as known (`None`: not known, the patch
    /// stays off). Takes a finished read's answer, and starts a read (on its own thread,
    /// [`super::desktop_items::start_read`]) for a new rectangle or when one is due: the last
    /// good read [`PATCH_ITEMS_FRESH`] old, a failed one [`items_retry`] old. A read
    /// that fails keeps the icons known. None starts while `quiet` (a drag, the fast tick).
    fn patch_items(&mut self, rect: Option<Rect>, now: Instant, quiet: bool) -> Option<Vec<Rect>> {
        use crate::engine::corner_patch::{DesktopIcons, MAX_ICONS, item_reach};
        let answer = match self.patch_items.reading.as_ref().map(|r| r.try_recv()) {
            Some(Ok(answer)) => Some(answer),
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => Some(None),
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => None,
        };
        if let Some(answer) = answer {
            let p = &mut self.patch_items;
            p.reading = None;
            let said = match answer {
                Some(DesktopIcons::Icons(icons)) => {
                    let items: Vec<Rect> = icons.into_iter().map(item_reach).collect();
                    if let Some(r) = p.read_for {
                        let under = items.iter().filter(|i| i.intersect(&r).is_some()).count();
                        let was = p.known.as_ref().is_some_and(|k| k.iter().any(|i| i.intersect(&r).is_some()));
                        if under > 0 && !was {
                            tracing::info!(under, "corner patch: a desktop icon lies under the corner; the patch stays off while it does");
                        }
                    }
                    p.known = Some(items);
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
        let rect = rect?;
        let p = &mut self.patch_items;
        let due = p.read_for != Some(rect) || p.due.is_none_or(|d| now >= d);
        if due && p.reading.is_none() && !quiet {
            p.read_for = Some(rect);
            p.due = Some(now + PATCH_ITEMS_FRESH);
            p.reading = super::desktop_items::start_read();
        }
        if !self.settings.hide_widgets_standard && !self.patch_widgets_logged {
            self.patch_widgets_logged = true;
            tracing::info!(
                "corner patch: widgets may show on the desktop (StandardHideWidgets off); their places are not public, so one in the corner would be covered"
            );
        }
        self.patch_items.known.clone()
    }

    /// Works out the wallpaper corner patch's plan ([`crate::engine::corner_patch`]) and sends
    /// it to the main thread when it changed. Reads the window server's on-screen list only
    /// while `[windows] corner_patch` is on; checks the patch's order in it every time (an
    /// activation raising a parked window undoes it), and has it ordered again when it was
    /// right and is not any more. Returns the list when it read one, for the rest of the tick
    /// ([`super::sliver_tap::refresh`]).
    fn update_patch(&mut self) -> Option<Vec<windows::CgWindow>> {
        use crate::engine::corner_patch::{
            Listed, PatchFacts, desktop_slides, order_step, ordered_right, patch_rect, slide_waits, steady_rect, visibility, would_be_right,
        };
        use crate::ui::corner_patch::{self as patch_ui, PatchPlan};
        let config = self.store.config();
        let (on, reach, space) = (config.windows.corner_patch, config.windows.corner_patch_reach, self.space_changes);
        // Mission Control, App Exposé, Show Desktop: the panel is Transient, so macOS hides it
        // while one shows and shows it again the moment it closes, with the windows. A patch
        // shown when one opened is left as it is (no plan, no reorder during it); hiding it
        // from the plan left the slivers bare until the close was seen (up to a second's
        // poll) and settled (10-13, F3 twice: the corner bare until a window was focused).
        // A Desktop picked inside Mission Control plans as before (hidden until it is
        // recognized): the old Desktop's patch must not come back on the new one.
        // Only Mission Control and App Exposé: Show Desktop pushes every window to the screen's
        // edges, the patch then over a pushed window's edge (10-13: a lighter block in the
        // corner each time), so it hides by the plan.
        if on && !self.space_pending && self.overview.showing() == Some(Overview::MissionControl) && self.patch_sent.as_ref().is_some_and(|p| p.show.is_some()) {
            return None;
        }
        // A Space change being recognized: no plan. Each Desktop has its own panel, which
        // slides in with its Desktop as it was left; the main thread leaves it so until the
        // first plan for that Desktop (its Space-change count is newer), so nothing meant
        // for the Desktop left reaches it.
        if on && self.space_pending && self.model.is_some() {
            return None;
        }
        let mut read = None;
        let mut reorder = self.patch_sent.as_ref().map_or(0, |p| p.reorder);
        let plan = match &self.model {
            Some(m) if on => {
                let list = windows::on_screen();
                let own = std::process::id() as i32;
                let main = m.main_display();
                let display = main.frame;
                // Only those listed in the park zone (8L6): one the model has parked but the
                // window server lists elsewhere shows there, and grew the patch's rectangle
                // (and every switch's cover) to 2087x1316 pt on the owner's Mac (10-16).
                // This Desktop's parked windows only: another Desktop's listed on screen means
                // it slides in (or out) before the Space change's notice.
                let here = m.current_space(main.id);
                let model_parked: HashSet<u32> = m.windows().filter(|w| w.parked && w.space == here).map(|w| w.meta.id.0).collect();
                let parked_elsewhere: HashSet<u32> = m.windows().filter(|w| w.parked && w.space != here).map(|w| w.meta.id.0).collect();
                let parked = parked_in_zone(&list, model_parked.iter().copied(), main.frame, main.visible, main.corner);
                // A Desktop slides before the Space change's notice (10-23: the parked windows
                // listed at x=110, all together; at 11:51 the next Desktop's listed in the zone
                // and planned as this one's): the plan stays as it was, so the panel slides away
                // with its Desktop drawn and is there when it slides back.
                let listed_parked: Vec<Rect> = list.iter().filter(|w| w.layer == 0 && model_parked.contains(&w.id)).map(|w| w.bounds).collect();
                let elsewhere = list.iter().any(|w| w.layer == 0 && w.onscreen && parked_elsewhere.contains(&w.id));
                let now = Instant::now();
                let slides = (elsewhere || parked.is_empty()) && desktop_slides(&listed_parked, elsewhere, main.frame, main.visible, main.corner);
                self.patch_slide_since = slides.then(|| self.patch_slide_since.unwrap_or(now));
                let shown_now = self.patch_sent.as_ref().is_some_and(|p| p.show.is_some());
                if slide_waits(self.patch_slide_since.map(|t| now.saturating_duration_since(t)), shown_now) {
                    if self.patch_why != Some("the Desktop slides") {
                        tracing::debug!("corner patch: the Desktop slides; the plan stays as it was");
                        self.patch_why = Some("the Desktop slides");
                    }
                    return Some(list);
                }
                let slivers: Vec<Rect> = list.iter().filter(|w| w.layer == 0 && parked.contains(&w.id)).map(|w| w.bounds).collect();
                let rect = patch_rect(&slivers, display, reach).map(|r| steady_rect(self.patch_rects.get(&here.0).copied(), r));
                match rect {
                    Some(r) => self.patch_rects.insert(here.0, r),
                    None => self.patch_rects.remove(&here.0),
                };
                self.patch_rect = rect;
                let listed: Vec<Listed> =
                    list.iter().map(|w| Listed { id: w.id, layer: w.layer, bounds: w.bounds, drawn: w.alpha > 0.0 && w.pid != own }).collect();
                // The topmost parked window under the patch: what it is ordered against.
                let above = rect.and_then(|r| {
                    list.iter().find(|w| w.layer == 0 && parked.contains(&w.id) && w.bounds.intersect(&r).is_some()).map(|w| w.id)
                });
                // Whether just above it could be right at all (no shown window under it that
                // reaches the patch); when not, the fallback, without a show to find out.
                let number = patch_ui::window_number();
                let possible = match (rect, above) {
                    (Some(r), Some(a)) => would_be_right(&listed, number.unwrap_or(0), a, r, &parked),
                    _ => false,
                };
                // Checked only while the panel is on screen ordered against that window with
                // this reorder count, and that order could be right; otherwise not known.
                let check = match (number, rect, above) {
                    (Some(n), Some(r), Some(a))
                        if possible && patch_ui::ordered_against() == Some((a, reorder)) && list.iter().any(|w| w.id == n) =>
                    {
                        Some(ordered_right(&listed, n, r, &parked))
                    }
                    _ => None,
                };
                let was = self.patch_order;
                let step = order_step(&mut self.patch_order, above, check);
                if step.reorder {
                    reorder = reorder.wrapping_add(1);
                    tracing::debug!("corner patch: no longer just above the parked windows; ordered again");
                } else if check == Some(true) && !self.patch_honoured {
                    self.patch_honoured = true;
                    tracing::info!("corner patch: ordered just above the parked windows (macOS honours the order across apps)");
                } else if self.patch_order.refused.is_some() && self.patch_order.refused != was.refused {
                    tracing::info!("corner patch: macOS did not keep the patch just above the parked window; it shows only where no window reaches");
                } else if check.is_some() && check != was.last {
                    tracing::debug!(?check, "corner patch: order checked");
                }
                let ordered = step.ordered;
                let dragging = input::left_button_down() || self.drag.is_some();
                let items = self.patch_items(rect, now, dragging || self.wants_fast_tick());
                let facts = PatchFacts {
                    on,
                    rect,
                    // Not the settle after an overview (that is for Space inference): the patch
                    // comes back with the windows.
                    overview: self.overview.showing().is_some()
                        || self.space_pending
                        || self.patch_reveal_hold.is_some_and(|t| now.duration_since(t) < PATCH_REVEAL_HOLD),
                    ordered,
                    can_order: possible,
                    fullscreen: self.fullscreen,
                    // The same filter as the order's check (`Listed::counts`, the rectangle
                    // tested by `visibility`).
                    shown: listed.iter().filter(|w| w.layer == 0 && w.drawn && !parked.contains(&w.id)).map(|w| w.bounds).collect(),
                    dragging,
                    transition: self.pending.is_some(),
                    items,
                };
                let visible = visibility(&facts);
                // A switch's cover where a desktop icon lies: a still of the desktop (8L3), laid
                // over the patch's picture when it holds the icons only (8L4).
                let still = self.keep_desktop_shot(rect, facts.items.as_deref(), now);
                let why = visible.err();
                if why != self.patch_why {
                    if let Some(why) = why {
                        tracing::debug!(why, "corner patch: hidden by the plan");
                    }
                    self.patch_why = why;
                }
                let show = visible.ok();
                read = Some(list);
                // The test's forced still is compared with the patch's picture (`debug-overlay`).
                let wanted = desktop_shot::needs_wallpaper() || cover_desktop_forced();
                let picture = if show.is_none() && still && wanted { rect } else { None };
                // The icons as last read well, whatever this plan's rectangle (none read for a
                // plan with no sliver): they are the same on every Desktop, and other Desktops'
                // panels are checked against them (10-23: `None` here hid them all).
                PatchPlan { show, above: show.and(above), reorder, space, picture, on: true, items: self.patch_items.known.clone() }
            }
            _ => {
                self.patch_rect = None;
                self.patch_rects.clear();
                self.patch_slide_since = None;
                self.patch_order = Default::default();
                self.patch_why = None;
                // The icons known stay (they do not depend on the patch); a read is due when
                // the patch is wanted again.
                self.patch_items.read_for = None;
                PatchPlan { show: None, above: None, reorder, space, picture: None, on: false, items: None }
            }
        };
        if self.patch_sent.as_ref() == Some(&plan) || (self.patch_sent.is_none() && plan.show.is_none() && plan.picture.is_none()) {
            return read;
        }
        self.patch_sent = Some(plan.clone());
        notify_ui(UiUpdate::CornerPatch(plan));
        read
    }

    /// Keeps the model's stage-area fitting in step with the strip, Apple's settings and
    /// config, and its park request with config.
    fn sync_fit(&mut self) {
        let (strip_visible, settings) = (self.strip_visible, self.settings);
        let config = self.store.config();
        if let Some(m) = self.model.as_mut() {
            let fit = fit_options(m.main_display().visible, strip_visible, &settings, config);
            m.set_fit(fit);
            m.set_park_request(ParkRequest { inside: config.windows.park_inside, drop: config.windows.park_drop });
        }
    }

    /// Executes actions in order; returns how many AX calls failed. Once Quit is requested
    /// the rest of the batch runs only its `SetFrame`s: unpark-all covers every window the
    /// model counts as parked, whether or not its park was carried out, but a window the
    /// model already counts as shown would otherwise stay parked (the review's S2).
    fn execute(&mut self, ob: &Observer, actions: Vec<Action>) -> usize {
        self.execute_batch(ob, actions, Batch::User)
    }

    fn execute_batch(&mut self, ob: &Observer, actions: Vec<Action>, batch: Batch) -> usize {
        let (interruptible, retrying) = (batch.interruptible(), batch.retries());
        if actions.is_empty() {
            return 0;
        }
        self.pressed_obs = None;
        let unhide = self.before_batch_hides(ob, &actions);
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
                    (Action::SetFrame { frame, .. }, Some(e)) if retrying => {
                        Some(safety::set_frame_retrying(&e.element, id, frame, leftovers::RESTORE_RETRY.budget))
                    }
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
                // An unpark (quit, stand-down) tries a busy app again quickly (6X).
                Action::SetFrame { frame, .. } if retrying => safety::set_frame_retrying(&w.element, id, frame, leftovers::RESTORE_RETRY.budget),
                Action::SetFrame { frame, .. } => w.element.set_frame(frame),
                Action::Park { frame, .. } => w.element.set_position(frame.origin()),
                Action::Raise { .. } => w.element.raise(),
                Action::Focus { .. } => {
                    self.echoes.touch_app(w.pid, now);
                    ax_focus(&w.element, w.pid)
                }
                Action::StateChanged => Ok(()),
            };
            if let Err(e) = result {
                failed += 1;
                tracing::warn!(id, ?action, "action failed: {e}");
                // Its app may have quit with no notice (round missed-quit): tested next tick.
                ob.suspect(w.pid);
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
        self.after_batch_hides(unhide);
        self.publish_shield();
        self.update_patch();
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
        self.unrestored.insert(id, Unrestored::failed(entry));
    }

    /// Checks windows whose restore failed against the window server, about once a second:
    /// seen on screen out of the park zone, gone, or parked again in the model → forgotten;
    /// still in the park zone → moved again; off screen → kept registered. With `force`
    /// (quit) every one is retried at once, whatever the list says, a busy app tried again
    /// quickly within one [`leftovers::RESTORE_RETRY`] budget per window (the review of 6X,
    /// L1).
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
        let mut scan: Option<recovery::ScanDisplays> = None;
        let mut tabs: Option<hidden_tabs::Tabs> = None;
        let mut shown: Option<BTreeSet<u32>> = None;
        let before = self.unrestored.len();
        let ids: Vec<u32> = self.unrestored.keys().copied().collect();
        for id in ids {
            let model_parked = self.model.as_ref().and_then(|m| m.window(WindowId(id))).map(|w| w.parked);
            let listed = list.iter().find(|w| w.id == id).map(|w| {
                let zone = match model_parked {
                    Some(_) => self.model.as_ref().is_some_and(|m| m.main_display().parks(w.bounds)),
                    // A window the model forgot: the safety paths' test, which honours the
                    // other displays (it may sit on one now) and knows it is ours (L2).
                    None => recovery::is_stranded(w.bounds, scan.get_or_insert_with(safety::scan_displays), Known::Registered),
                };
                (w.onscreen, zone)
            });
            let forgotten_parked = model_parked.is_none() && listed.is_some_and(|(_, zone)| zone);
            // A hidden tab (8J)? Only by its own signature: one that left its Space stays real.
            let why = self.unrestored.get(&id).map_or(LetGo::Kept, |u| u.why);
            let hidden_tab = let_go_as_tab(model_parked, listed, why, |candidate| {
                list.iter().find(|w| w.id == id).is_some_and(|w| {
                    let shown = shown.get_or_insert_with(|| self.model_shown_ids());
                    tabs.get_or_insert_with(|| hidden_tabs::Tabs::new(&list)).is_hidden_tab(w, candidate, |i| shown.contains(&i))
                })
            });
            let step = if force && !hidden_tab && (model_parked == Some(false) || forgotten_parked) && listed.is_some() {
                UnrestoredStep::Retry
            } else {
                unrestored_step(model_parked, listed, hidden_tab)
            };
            let Some(u) = self.unrestored.get_mut(&id) else { continue };
            // A forgotten window is quiet until it is seen in the zone a second time (L6).
            let quiet = u.forgotten == Forgotten::Once;
            let (forgotten, warn) = u.forgotten.after(step);
            u.forgotten = forgotten;
            if warn {
                tracing::warn!(id, ?listed, restore = %u.entry.restore, "a parked window the model let go is still in the park zone: it stays registered until it is back");
            }
            match step {
                UnrestoredStep::Done if hidden_tab => {
                    tracing::info!(id, ?listed, "a parked window the model let go is a hidden tab: nothing can move it, and it needs not be");
                    self.unrestored.remove(&id);
                }
                UnrestoredStep::Done if quiet => {
                    tracing::debug!(id, ?listed, "a parked window the model let go is accounted for (closed, or back)");
                    self.unrestored.remove(&id);
                }
                UnrestoredStep::Done => {
                    tracing::info!(id, ?model_parked, ?listed, "a window whose restore failed is accounted for");
                    self.unrestored.remove(&id);
                }
                UnrestoredStep::Wait => {}
                UnrestoredStep::Retry => {
                    u.tries += 1;
                    let element = ob.window(id).map(|w| w.element.clone()).unwrap_or_else(|| u.entry.element.clone());
                    let restore = u.entry.restore;
                    let set = |e: &AxElement, end: Instant| {
                        if force { safety::set_frame_retrying(e, id, restore, end.saturating_duration_since(Instant::now())) } else { e.set_frame(restore) }
                    };
                    let mut result = set(&element, Instant::now() + leftovers::RESTORE_RETRY.budget);
                    if let Err(e) = result
                        && refresh_element(e, u.refreshed)
                        && let Ok(pid) = element.pid()
                    {
                        // The element we kept went stale (a window reported destroyed while
                        // ordered out): the app's own list may have it again. Once (L5). The
                        // lookup and its try have a budget of their own (the review of 6X2, 1:
                        // the busy app's budget never fitted them, so at quit it never ran).
                        u.refreshed = true;
                        let end = Instant::now() + leftovers::REFRESH_BUDGET;
                        match safety::ax_window(pid, id) {
                            // At quit, the try only when it still fits after the lookup.
                            Ok(fresh) if !force || leftovers::leftover_fits(end.saturating_duration_since(Instant::now()), true) => {
                                result = set(&fresh, end);
                                u.entry.element = fresh;
                            }
                            Ok(fresh) => {
                                tracing::warn!(id, "a stale element's lookup ran long: no time left to try the window afresh");
                                u.entry.element = fresh;
                            }
                            Err(why) => tracing::debug!(id, "a stale element's lookup: {why}"),
                        }
                    }
                    match result {
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

    /// The registry entries of those of `ids` the model counts as parked (taken before the
    /// model forgets them: [`Self::watch_forgotten`]).
    fn parked_entries_of(&self, ids: &[u32]) -> Vec<(u32, ParkedEntry)> {
        let Some(m) = &self.model else { return Vec::new() };
        ids.iter()
            .filter(|&&id| m.window(WindowId(id)).is_some_and(|w| w.parked))
            .filter_map(|&id| safety::registry_entry(id).map(|e| (id, e)))
            .collect()
    }

    /// Windows the model just forgot while they were parked (`kept`: their registry entries):
    /// each one the window server still lists in the park zone stays registered as an
    /// unrestored window ([`Unrestored`]) until it is seen back or gone, so no run loses
    /// track of a window it parked (6X). `why`: why the model let them go (only
    /// [`LetGo::Destroyed`] may later be judged a hidden tab).
    fn watch_forgotten(&mut self, ob: &Observer, kept: Vec<(u32, ParkedEntry)>, why: LetGo) {
        if kept.is_empty() {
            return;
        }
        let ids: Vec<u32> = kept.iter().map(|(id, _)| *id).collect();
        let listed = windows::describe(&ids);
        let displays = safety::scan_displays();
        let mut added = false;
        for (id, entry) in kept {
            if self.unrestored.contains_key(&id) || self.model.as_ref().is_some_and(|m| m.window(WindowId(id)).is_some()) {
                continue;
            }
            let Some(l) = listed.iter().find(|l| l.id == id) else { continue };
            // The safety paths' test: it honours the other displays (L2).
            if !recovery::is_stranded(l.bounds, &displays, Known::Registered) {
                continue;
            }
            // A closed window is listed here for a moment: quiet until a second sighting (L6).
            tracing::debug!(id, at = %l.bounds, onscreen = l.onscreen, restore = %entry.restore, "a parked window the model let go is in the park zone: it stays registered until it is back or gone");
            self.unrestored.insert(id, Unrestored { forgotten: Forgotten::Once, why, ..Unrestored::failed(entry) });
            added = true;
        }
        if added {
            safety::registry_replace(self.parked_entries(ob));
        }
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

    /// Windows whose registry entry is kept while the model has let them go as reported
    /// destroyed (the tab's signature): the only registered windows the hidden-tab test may
    /// judge (the review of 8J, [`safety::Ours::destroyed`]). One that left its Space, or was
    /// let go for any other reason, is not among them. None without a model.
    pub fn destroyed_ids(&self) -> BTreeSet<u32> {
        let Some(m) = &self.model else { return BTreeSet::new() };
        self.unrestored.iter().filter(|(id, u)| u.why == LetGo::Destroyed && m.window(WindowId(**id)).is_none()).map(|(id, _)| *id).collect()
    }

    /// What the safety paths know of whose windows are ours ([`safety::Ours`]): the
    /// registry, the model's windows, `state.json`, the registered windows the model let go
    /// as reported destroyed, and the model's windows not parked.
    pub fn safety_ours(&self) -> safety::Ours {
        safety::Ours::now(self.model_window_ids()).with_tab_signs(self.destroyed_ids(), self.model_shown_ids())
    }

    /// Quit, after its leftovers check: windows whose restore failed are checked against the
    /// window server at once (no wait for the once-a-second check): those seen back out of the
    /// park zone, or gone, are forgotten; one still in the zone on screen is tried once more.
    /// Returns how many are left (another Space's, an app that does not answer).
    pub fn account_unrestored(&mut self, ob: &Observer) -> usize {
        self.unrestored_checked = Instant::now().checked_sub(UNRESTORED_RETRY).unwrap_or(self.unrestored_checked);
        self.retry_unrestored(ob, false);
        self.unrestored.len()
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
        self.last_switch = Some(Instant::now());
        let mut run = std::mem::take(&mut self.activation);
        let hidden = self.hidden_arrivals(ob, &actions);
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
                    Action::SetFrame { id, .. } if !hidden.contains(&id.0) => Some(id.0),
                    _ => None,
                })
                .collect();
            let outgoing: HashSet<u32> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::Park { id, .. } => Some(id.0),
                    _ => None,
                })
                .collect();
            // The activated app's windows are raised by macOS wherever it put them (8L6).
            let activated: HashSet<u32> = run.fallback.intersection(&arriving).copied().collect();
            let found =
                self.switch_corner(&arriving, &outgoing, &activated, &list).and_then(|c| self.cover_ready(c).map_err(Refused::from));
            match found {
                Ok(c) => early_corner = Some((Some(c), list)),
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
        let mut first = std::mem::take(&mut run.now);
        first.extend(hidden);
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
        failed + self.execute_animated_rest(ob, actions, &first, run, early_corner)
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
        let stage = m.stage(m.main_active_stage()?)?;
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

    /// Whether switch cover `c` can be drawn now ([`resolve_cover`]): the patch's picture or
    /// the desktop's still (round 8L3), whichever is ready; the cover as it will be drawn, or
    /// why not.
    fn cover_ready(&self, c: SwitchCover) -> Result<SwitchCover, &'static str> {
        let key = self.shot_key();
        resolve_cover(c, crate::ui::corner_patch::cover_ready, |rect| match &key {
            // A still that holds the icons only is laid over the patch's picture (8L4).
            Some(key) => desktop_shot::ready(rect, key, crate::ui::corner_patch::cover_ready),
            None => Err("no model"),
        })
    }

    /// What a still of the desktop in the park corner is of now ([`desktop_shot::Key`]): the
    /// Space count, the main display, Finder's icons as last read.
    fn shot_key(&self) -> Option<desktop_shot::Key> {
        let m = self.model.as_ref()?;
        Some(desktop_shot::Key::new(self.space_changes, m.main_display().frame, self.patch_items.known.as_deref()))
    }

    /// Keeps the still of the desktop in the park corner fresh while a switch's cover would
    /// need it (round 8L3): an activation may grow (`[animation] activation_grows`, motion on,
    /// the strip shown, `[windows] corner_patch` on), something is parked there (`rect`, the
    /// patch's rectangle), and the patch's picture may not stand for the corner a switch
    /// covers: a desktop icon (`items`, Finder's as last read) in the still's block around the
    /// corner, not just under the patch (a switch's corner is the patch's grown by the
    /// incoming windows: 10-16, the patch clear and the switch's corner on an icon), the
    /// icons not known, or no picture of it now. Never during an overview, a Space change,
    /// on a full-screen Space or while a switch runs, and none at rest ([`desktop_shot::rest`]:
    /// the screen locked, the display asleep, no input and no switch for 2 min; the first
    /// input after brings a fresh one, 8L4). Whether a still is wanted for the corner now
    /// (the patch then keeps its picture rendered while hidden, for a still that holds the
    /// icons only, 8L4).
    fn keep_desktop_shot(&self, rect: Option<Rect>, items: Option<&[Rect]>, now: Instant) -> bool {
        let config = self.store.config();
        let anim = &config.style.animation;
        if !(anim.activation_grows && anim.animates() && self.strip_visible && config.windows.corner_patch) {
            return false;
        }
        let Some(rect) = rect else { return false };
        if self.overview.showing().is_some() || self.space_pending || self.fullscreen || self.pending.is_some() {
            return false;
        }
        let (Some(key), Some(m)) = (self.shot_key(), self.model.as_ref()) else { return false };
        let block = desktop_shot::block(m.main_display().frame, Some(rect));
        let icons = items.is_none_or(|i| i.iter().any(|i| i.intersect(&block).is_some()));
        if !icons && !cover_desktop_forced() && crate::ui::corner_patch::cover_ready(rect).is_ok() {
            return false;
        }
        // No still while the screen is locked, the display sleeps or nobody has touched the
        // Mac or switched for a while (8L4): the cover is only needed at a switch.
        let moment = desktop_shot::Moment {
            locked: self.locked,
            asleep: objc2_core_graphics::CGDisplayIsAsleep(objc2_core_graphics::CGMainDisplayID()),
            since_input: input::since_last_input(),
            since_switch: self.last_switch.map(|t| now.saturating_duration_since(t)),
        };
        desktop_shot::keep_fresh(key, block, moment, now);
        // At rest the patch's picture is not kept rendered either.
        desktop_shot::rest(moment).is_none()
    }

    /// The park corner a switch's overlay covers while its incoming windows (`arriving`) wait
    /// there behind their snapshots ([`crate::engine::corner_patch::switch_cover`], round
    /// 8L): macOS has raised an activated app's windows above the corner patch, and the
    /// switch's focus raises its key window there too. `list` is the window server's list now;
    /// `outgoing` the windows about to park. A window that stays shown into the corner is drawn
    /// on the cover from the strip's snapshot of it (8L5). The reason, and the windows it
    /// names, when the corner cannot be covered (it is left as it is).
    fn switch_corner(
        &self,
        arriving: &HashSet<u32>,
        outgoing: &HashSet<u32>,
        activated: &HashSet<u32>,
        list: &[windows::CgWindow],
    ) -> Result<SwitchCover, Refused> {
        use crate::engine::corner_patch::{CoverFacts, switch_cover};
        use crate::engine::park::is_parked_in_at;
        if arriving.is_empty() {
            return Err("nothing arrives".into());
        }
        let m = self.model.as_ref().ok_or("no model")?;
        let config = self.store.config();
        let display = m.main_display();
        let own = std::process::id() as i32;
        // Parked where the window server lists them in the park zone; one the model has parked
        // listed elsewhere shows there and stays shown through the switch (8L6).
        let parked = parked_in_zone(list, m.windows().filter(|w| w.parked).map(|w| w.meta.id.0), display.frame, display.visible, display.corner);
        let normal = |w: &&windows::CgWindow| w.layer == 0 && w.alpha > 0.0 && w.pid != own;
        let in_zone = |b: Rect| is_parked_in_at(b, display.frame, display.visible, display.corner);
        let zone: Vec<&windows::CgWindow> = list
            .iter()
            .filter(normal)
            .filter(|w| !outgoing.contains(&w.id) && (arriving.contains(&w.id) || parked.contains(&w.id)) && in_zone(w.bounds))
            .collect();
        let slivers = zone.iter().map(|w| w.bounds).collect();
        let raised = raised_windows(list, own, arriving, activated, in_zone);
        let staying = staying_windows(list, own, outgoing, arriving, &parked, strip_model::has_snapshot);
        let now = Instant::now();
        let facts = CoverFacts {
            on: config.windows.corner_patch,
            overview: self.overview.showing().is_some()
                || self.space_pending
                || self.patch_reveal_hold.is_some_and(|t| now.duration_since(t) < PATCH_REVEAL_HOLD),
            fullscreen: self.fullscreen,
            slivers,
            raised,
            display: display.frame,
            corner: display.corner,
            visible: display.visible,
            dock_bars: self.dock_bar.as_ref().map(|(_, b)| b.bars.clone()).filter(|b| !b.is_empty()),
            reach: config.windows.corner_patch_reach,
            in_use: self.patch_rect,
            items: self.patch_items.known.clone(),
            staying,
        };
        let forced = cover_desktop_forced();
        switch_cover(&facts).map(|c| if forced { SwitchCover { picture: CoverPicture::Desktop, ..c } } else { c })
    }

    /// The animated batch after its early windows (`first`): planned as a transition, sent to
    /// the strip, an activation's focus (`run`, [`ActivationRun::focus`]) routed at once, then
    /// the parks behind the strip's snapshots and the arrivals held until it unveils them.
    /// `early_corner`: the corner's cover as the activation found it, and the window list it
    /// read (none read, or moved since: both read here).
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
        let id = t.id;
        let arriving: HashSet<u32> = t.incoming.iter().map(|w| w.window.id).collect();
        let outgoing: HashSet<u32> = t.outgoing.iter().map(|w| w.window.id).collect();
        let (corner, list) = match early_corner {
            Some((corner, list)) => (corner, list),
            None => {
                let list = windows::on_screen();
                let activated: HashSet<u32> = run.fallback.intersection(&arriving).copied().collect();
                let corner =
                    self.switch_corner(&arriving, &outgoing, &activated, &list).and_then(|c| self.cover_ready(c).map_err(Refused::from));
                if let Err(r) = &corner
                    && !arriving.is_empty()
                {
                    tracing::debug!(id, why = r.why, windows = ?r.windows, "transition: the park corner is not covered");
                }
                (corner.ok(), list)
            }
        };
        t.under = self.window_in_place(&arriving, &outgoing, &actions, &list);
        t.corner = corner;
        let frames: Vec<Rect> = t.incoming.iter().map(|w| w.window.frame).collect();
        let started = Instant::now();
        self.present_with(Some(Transition { sent: Some(started), ..t }));
        // An activation's keyboard goes at once, while the strip draws: macOS has activated
        // the app and raised its windows in the corner already, and typing right after ⌘Tab
        // must reach the window the switch focuses (8L). One bounded step (8L2); the whole
        // focus runs with the parks as for a card click.
        let early_focus_ms = match run.focus.filter(|key| actions.contains(&Action::Focus { id: *key })) {
            Some(key) => self.focus_now(ob, key, id),
            None => None,
        };
        let focus_since_activation_ms = early_focus_ms.and(run.noticed).map(|t| t.elapsed().as_millis() as u64);
        let cover = strip_model::wait_cover(id, COVER_TIMEOUT);
        let unveil_ms = (motion_math::unveil_time(&self.store.config().style) * 1000.0).round() as u64;
        tracing::debug!(
            id,
            ?cover,
            waited_ms = started.elapsed().as_millis() as u64,
            early_focus_ms,
            focus_since_activation_ms,
            unveil_ms,
            at_once = run.at_once,
            at_once_windows = ?run.at_once_windows,
            "transition covered"
        );
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
            announce: std::mem::take(&mut self.announce),
            active_windows,
            pinned_windows,
            transition,
            parked: self.parked_note.take(),
            arrived: self.arrived_note.take(),
            space: space.0,
            fullscreen: current && self.fullscreen,
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
    /// frame that is not finite) is not written: the file on disk stays.
    pub fn save_now(&mut self) {
        self.dirty = None;
        let Some(m) = self.model.as_mut() else { return };
        let path = paths::state_file();
        let now = unix_now();
        // Windows of apps that quit too long ago are not reopened by a login.
        m.prune_quit(now);
        let mut file = m.to_state();
        // The window ids in it hold for this window server session only.
        file.session = session::window_session().map(str::to_string);
        file.stamp(now, &mut self.seen_times);
        let text = match file.to_json() {
            Ok(text) => text,
            Err(e) => {
                tracing::error!("{e}; the saved stages on disk are kept");
                return;
            }
        };
        let result = crate::private_files::ensure_dir(&paths::config_dir())
            .and_then(|_| super::durable::replace(&path, text.as_bytes(), super::durable::Flush::Full));
        match result {
            Ok(()) => tracing::debug!("state saved to {}", path.display()),
            Err(e) => tracing::warn!("cannot save {}: {e}", path.display()),
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
        let path = paths::state_file();
        let text = std::fs::read_to_string(&path).ok()?;
        let current = session::window_session();
        match StateFile::from_json(&text) {
            Ok(mut s) => {
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
            WinEvent::Moved(_)
                | WinEvent::Retitled(_)
                | WinEvent::Focused(_)
                | WinEvent::AppActivated(_)
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
                self.watch_forgotten(ob, kept, LetGo::Destroyed);
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
                    if let Some(pid) = pid {
                        self.activation = self.activation_for(pid, &actions, ob);
                    }
                    self.execute_animated(ob, actions);
                }
            }
            WinEvent::Minimized(id) => self.on_minimized(ob, id),
            WinEvent::AppHidden(pid) => self.on_app_hidden(ob, pid, true),
            WinEvent::AppUnhidden(pid) => self.on_app_hidden(ob, pid, false),
            WinEvent::AppTerminated(pid) => {
                // Its icon leaves the Dock unless pinned: the strip may follow.
                self.dock_order.soon(now);
                self.keep_quit_windows(pid, now);
                self.app_terminated(pid);
            }
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
                if self.model.as_ref().is_some_and(Model::settling) {
                    if input::left_button_down() {
                        // A Dock click whose press the input monitor has not told us about yet.
                        self.end_restore(ob, "a click (or a Dock click)", End::User);
                    } else {
                        // A launch-time activation at login is the app's, not the user's: no
                        // switch, nothing held for later.
                        tracing::debug!(pid, "an activation while the login settles: no switch");
                        return;
                    }
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
                let needs_focus = self.model.as_ref().is_none_or(|m| m.activation_needs_focus(pid));
                let obs = self.observe_activation(ob, pid, needs_focus);
                if self.bring_here_if_shift(ob, pid, &obs, now) {
                    self.forget_held(ob);
                    return;
                }
                let present = self.app_present(pid);
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
        }
    }

    /// An app was hidden or unhidden (NSWorkspace): `[windows] hide_parked_apps` follows
    /// its own hides and unhides.
    fn on_app_hidden(&mut self, _ob: &mut Observer, pid: i32, hidden: bool) {
        self.app_visibility(pid, hidden);
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
        let mods = Modifiers { shift: event.shift, cmd: event.cmd };
        if event.kind == InputKind::MouseUp {
            self.triggers.record_release(now, mods);
        } else {
            self.triggers.record(now, mods);
        }
        if event.kind == InputKind::MouseUp {
            // A desktop icon may have been dropped: Finder's icons are read again shortly.
            self.patch_items.due = Some(now + PATCH_ITEMS_RETRY);
        }
        if matches!(event.kind, InputKind::MouseDown | InputKind::MouseUp) {
            // A press may start a window drag: the corner patch's fallback hides at once.
            self.update_patch();
        }
        match event.kind {
            InputKind::MouseDown => {
                tracing::debug!(x = event.at.x, y = event.at.y, "mouse down in another app");
                self.end_restore(ob, "a click (or a Dock click)", End::User);
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
            InputKind::OtherMouseDown => {
                // A right or middle click is the user's: it ends the login's settling (their
                // focus and stage stand), and is no drag, Dock or wallpaper click.
                tracing::debug!(x = event.at.x, y = event.at.y, "right or other mouse down in another app");
                self.end_restore(ob, "a right or middle click", End::User);
            }
            InputKind::KeyDown => {
                // ⌘Tab's own app switch brings its stage; nothing else comes forward for it.
                if event.is_switcher_key() {
                    self.end_restore(ob, "an app switcher key", End::Switcher);
                } else {
                    self.end_restore(ob, "a key press", End::User);
                }
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
        // The user holds it: no check of a move made ahead parks it again (R11 3).
        self.user_took(id, "grabbed by its sliver");
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
                if self.locked {
                    // Recognized once the screen unlocks ([`Self::settle_space`]).
                    self.space_pending = true;
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
                self.arrive_here(ob, &list, main, None);
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
        if !self.can_build(ob, &list) {
            return;
        }
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
            model.set_restore(restore);
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
        // Following the Dock, the first strip shows in the Dock's order already.
        self.refresh_dock_order(Instant::now(), ob.last_launch());
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
        self.fullscreen || self.space_pending || self.locked
    }

    /// Why stage commands are refused now, if they are ([`Self::frozen`]).
    fn frozen_reason(&self) -> Option<&'static str> {
        if self.locked {
            Some("the screen is locked")
        } else if self.space_pending {
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
                self.publish_shield();
            }
            overview::Change::Ended(kind) => {
                tracing::info!("{} closed: Space inference resumes after a settle", kind.name());
                self.publish_shield();
            }
            overview::Change::Stuck(kind) => {
                tracing::warn!(
                    "{} has been listed for {} s without a break: taken for something else; Space inference resumes",
                    kind.name(),
                    overview::OVERVIEW_STUCK.as_secs()
                );
                self.publish_shield();
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
        let off: Vec<(u32, i32)> = m
            .windows()
            .filter(|w| w.meta.role == WindowRole::Standard && !w.sticky && !shown.contains(&w.meta.id.0))
            .filter(|w| ob.window(w.meta.id.0).is_some())
            .map(|w| (w.meta.id.0, w.meta.pid))
            .collect();
        let excused = self.off_screen_for_a_reason(ob, &off);
        off.into_iter().filter(|(id, _)| !excused.contains(id)).map(|(id, _)| WindowId(id)).collect()
    }

    /// Those of `windows` (id, pid) off screen for a reason: their app hidden (ours or the
    /// user's), minimized, in full screen, or their app too busy to tell. The app's hidden
    /// state is asked once per app, and an app that did not answer in time is not asked
    /// again for a moment ([`OffScreen`], Fable H1): a hung app no longer costs a timeout per
    /// window every second on the WM thread.
    fn off_screen_for_a_reason(&self, ob: &Observer, windows: &[(u32, i32)]) -> Vec<u32> {
        let now = Instant::now();
        let mut busy = self.busy_apps.borrow_mut();
        busy.prune(now);
        let mut check = OffScreen::new(&mut busy, now);
        let out: Vec<u32> = windows
            .iter()
            .filter(|&&(id, pid)| {
                self.counts_hidden(pid)
                    || check.has_reason(pid, || ob.app_hidden_reply(pid), || ob.minimized_reply(id), || ob.fullscreen_reply(id))
            })
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
        let now = Instant::now();
        if self.poll_lock(now, true) {
            // Recognized once the screen unlocks (the tick tries again).
            return;
        }
        let list = windows::list_windows();
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
                let mut view = None;
                if self.arrival_waits(ob, &list, main, now, &mut view) {
                    return;
                }
                self.space_pending = false;
                self.fullscreen = false;
                self.space_changed_at = Some(now);
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
        let shown = on_main_screen(list, main.frame);
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
        let Some(m) = self.model.as_mut() else { return };
        let mut actions = m.arrive(main.id, &live, &absent, &hints);
        let after = m.current_space(main.id);
        let note = m.last_arrival();
        // `new`: windows this run had not seen (all of a saved Desktop's on its first visit,
        // placed into its saved stages: `rebuilt`); `moved`: known windows of another Desktop
        // taken as moved here; `beside`: Desktops still on screen whose windows stay theirs.
        tracing::info!(
            from = before.0,
            to = after.0,
            on_screen = live.len(),
            new = arrivals.len(),
            rebuilt = ?note.rebuilt,
            moved = note.moved,
            carried = note.carried,
            beside = ?note.beside,
            "Space changed"
        );
        self.note_dock_arrival(after, before);
        // A Desktop arranged ahead for the window it was shown for: confirmed or undone.
        let (more, predicted) = self.settle_ahead(ob, list, after, Instant::now());
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

    /// What an arrival on `main` is given: the windows on its screen the engine knows or
    /// waits to place (rules applied), and the managed windows missing from every screen for
    /// no visible reason ([`Self::absent_windows`]; a window on another display is not
    /// missing).
    fn screen_view(&self, ob: &Observer, list: &[windows::CgWindow], main: Display) -> ScreenView {
        let Some(m) = self.model.as_ref() else { return (Vec::new(), Vec::new()) };
        let shown = on_main_screen(list, main.frame);
        let z = windows::z_order(list);
        let live = ob
            .windows()
            .keys()
            .copied()
            .filter(|id| shown.contains(id))
            .filter(|id| self.unplaced.contains(id) || m.window(WindowId(*id)).is_some())
            .filter_map(|id| self.meta(ob, id, &z))
            .filter(|meta| self.allowed_by_rules(meta))
            .collect();
        (live, self.absent_windows(ob, &on_screen(list)))
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
        let since_notice = self.space_changed_at.map_or(MIXED_WAIT, |t| now.duration_since(t));
        if since_notice < MIXED_WAIT {
            let (live, absent) = self.screen_view(ob, list, main);
            let two = self.model.as_ref().is_some_and(|m| m.shows_two_desktops(main.id, &live, &absent));
            if two {
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
        if locked != self.locked {
            self.locked = locked;
            // What was counted towards windows gone or moved is looked at afresh.
            self.absent.clear();
            self.foreign.clear();
            if locked {
                tracing::info!("the screen is locked: no Space is inferred and state.json is not written until it unlocks");
            } else {
                tracing::info!("the screen is unlocked: the Desktop is looked at again after a settle");
                self.space_changed_at = Some(now);
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
                true
            }
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
            self.watch_forgotten(ob, kept, LetGo::Other);
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
            self.watch_forgotten(ob, kept, LetGo::Other);
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
        if let Some(bundle) = ob.window(id).and_then(|w| ob.bundle_id(w.pid)).map(str::to_string) {
            // A new window: its app is not done reopening windows yet.
            self.quiet.window(&bundle, Instant::now());
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
        if self.unplaced.remove(&id) {
            // It waited off screen: one read then as a dialog is read again (a window minimized
            // then or now is standard: taken for a child, it would follow Apple's rule for a
            // child, never its saved stage).
            ob.recheck_kind(id);
        }
        let z = windows::z_order(&list);
        let Some(mut meta) = self.meta(ob, id, &z) else { return };
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
            let join_current = config.stages.new_apps_join_current;
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
            return;
        }
        counters::bump(Count::NewWindow);
        tracing::info!(id, action = %decision.action, placement = %decision.placement, "new window");
        let obs = self.observe(ob);
        if let Some(m) = self.model.as_mut() {
            let actions = m.add_window(meta, &decision, &obs);
            let back = m.last_pending().filter(|n| n.window.0 == id);
            if let Some(note) = back {
                tracing::info!(id, grade = note.grade.name(), points = note.points, parked = note.parked, switched = note.switched, adopted = ?note.adopted, from_other = note.from_other, "restore: back in its saved stage");
            }
            let settling = m.settling();
            if let (Some(clock), true) = (self.settling.as_mut(), back.is_some()) {
                // The restore is still under way.
                clock.busy(Instant::now());
            }
            if settling {
                // While the login settles nothing animates: windows go to their stages quietly.
                self.execute(ob, actions);
            } else {
                self.execute_animated(ob, actions);
            }
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
        self.publish_shield();
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
                // Opening Show Desktop (not closing it): the patch goes at once.
                if self.overview.showing() != Some(Overview::ShowDesktop) {
                    self.patch_reveal_hold = Some(Instant::now());
                    self.update_patch();
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
        let command = self.end_restore_for_hotkey(ob, command);
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
            let obs = self.observe(ob);
            self.model.as_ref().map(|m| {
                // The Space each command acts on: the current one, or the focused window's.
                let space = match command {
                    HotkeyCommand::SendTo(_) => obs.focused.and_then(|f| m.space_of(f)),
                    _ => None,
                }
                .unwrap_or_else(|| m.current_space_for(&obs));
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
            StripEvent::CardDropped { stage, x, y } => StripEvent::CardDropped { stage: f(stage), x, y },
            StripEvent::IconClicked { stage, pid } => StripEvent::IconClicked { stage: f(stage), pid },
            StripEvent::CardMoved { stage, before } => StripEvent::CardMoved { stage: f(stage), before: before.map(f) },
            StripEvent::CardMerged { stage, into } => StripEvent::CardMerged { stage: f(stage), into: f(into) },
            StripEvent::Unveil { id } => StripEvent::Unveil { id },
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
        if !matches!(event, StripEvent::Unveil { .. }) {
            self.end_restore(ob, "a card click", End::User);
        }
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
        // The restore window's end (at this event or at the card's press) may have moved a
        // voter out of the stage the card showed: the card follows its window.
        let event = self.follow_event(event);
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
        let failed = self.execute_batch(ob, actions, Batch::Unpark);
        safety::registry_replace(self.parked_entries(ob));
        // Any app of ours the batch did not bring back (none should be left).
        let unhidden = safety::unhide_ours("unpark-all");
        tracing::info!(windows = n, failed, unrestored = self.unrestored.len(), unhidden, "unparked every window");
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
                let style = config.strip_style();
                if *self.style != style {
                    self.style = Arc::new(style);
                }
                self.echoes.set_window(Duration::from_millis(config.windows.focus_echo_ms));
                self.sync_fit();
                self.publish_shield();
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
        if self.model.is_none() && self.build_wait.is_some() && !safety::quit_requested() {
            // The launch's stages wait for the Desktop to be seen: look again now and then.
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
        // No capture round while nobody can see the screen (8M: rounds failing for minutes).
        let away = if self.locked {
            Some(capture_pace::Away::Locked)
        } else if objc2_core_graphics::CGDisplayIsAsleep(objc2_core_graphics::CGMainDisplayID()) {
            Some(capture_pace::Away::Asleep)
        } else {
            None
        };
        super::capture::set_away(away);
        if let Some((first, last)) = self.dirty
            && (now.duration_since(last) >= SAVE_DEBOUNCE || now.duration_since(first) >= SAVE_MAX_DELAY)
            && !self.locked
        {
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
            if self.patch_reveal_hold.is_some_and(|t| now.duration_since(t) >= PATCH_REVEAL_HOLD) {
                self.patch_reveal_hold = None;
                self.update_patch();
            }
            self.expire_ahead(ob, now);
            if self.absent_due.is_some_and(|t| now >= t) {
                self.give_up_absent(ob);
            }
            if now.duration_since(self.quiet_checked) >= Duration::from_secs(1) {
                self.quiet_checked = now;
                self.give_up_pending(ob, now);
            }
            // An app with saved windows running with no window yet is still opening (Xcode, an
            // Electron app: 30-90 s): the login is not quiet (the review of 8F, M2).
            let opening = self.settling.is_some() && self.pending_apps_opening(ob);
            if let Some(clock) = self.settling.as_mut() {
                if let Some(t) = ob.last_launch() {
                    clock.busy(t);
                }
                if opening {
                    clock.busy(now);
                }
                // Not while the screen is locked (a login that locks at once): nothing is done
                // for a screen nobody sees; the end comes after the unlock.
                if let Some(end) = clock.due(now).filter(|_| !self.locked) {
                    let why = if end == End::TimeUp { "its five minutes are up" } else { "apps went quiet" };
                    self.end_restore(ob, why, end);
                }
            } else if (self.front_after_restore || self.focus_after_restore)
                && !self.frozen()
                && self.pending.is_none()
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
            } else if self.space_pending {
                self.settle_space(ob);
            } else {
                self.check_spaces(ob);
            }
            self.hide_pass(ob);
            // One read of the window server's list a tick: the patch's, or the tap's own when
            // the patch is off. The inert-slivers tap judges presses on it, never reading it
            // itself.
            let list = self.update_patch().or_else(|| super::sliver_tap::wants_list().then(windows::on_screen));
            super::sliver_tap::refresh(list.as_deref());
        }
        if self.strip_dirty {
            self.present();
        }
    }

    /// Closes the restore window (the first user action, apps quiet, or its time up; see
    /// [`crate::engine::restore`]): from now on a window that goes back to an inactive stage
    /// brings that stage forward, and no Desktop becomes provisional. At the next tick an
    /// empty saved active stage with cards beside it gives way (`front_after_restore`; not
    /// after an app switcher key, whose own switch wins), and when it ended by itself the
    /// saved key window takes the keyboard (`focus_after_restore`). After a restart the
    /// restore's line is logged (`log summary` repeats it).
    fn end_restore(&mut self, ob: &Observer, why: &str, end: End) {
        let Some(clock) = self.settling.take() else { return };
        let now = Instant::now();
        // The entries of apps not running then: given up at the next tick (or the login's
        // minute), before a still-empty front gives way (`front_after_restore`).
        self.absent_due = Some(clock.give_up_absent_at(now));
        let mut voters = Vec::new();
        if let Some(m) = self.model.as_mut()
            && let Some(actions) = m.end_restore()
        {
            tracing::info!(why, pending = m.pending_count(), pending_unvisited = m.pending_unvisited(), "restore: the restore window ends");
            if let Some(line) = m.restore_line(clock.age(now).as_secs()) {
                tracing::info!("{line}");
            }
            self.front_after_restore = end.brings_front();
            // Only after a restart: a relaunch in the same session changed no focus.
            self.focus_after_restore = end.by_itself() && m.after_restart();
            voters = actions;
        }
        // The windows that voted for a saved Desktop, back in their saved stages (round 8I2):
        // run now, before the action that ended the window acts on the model.
        self.execute(ob, voters);
    }

    /// Gives up the pending windows of apps running and quiet for
    /// [`crate::engine::restore::QUIET`] (no new window): the windows were closed inside the
    /// app's own session. An app that is not running keeps them (until a later session); one
    /// with a window not shown yet (another Desktop, minimized, hidden) is not quiet.
    fn give_up_pending(&mut self, ob: &mut Observer, now: Instant) {
        // Apps that quit have no clock (a later launch starts one again).
        self.quiet.prune(|b| !ob.pids_with_bundle(b).is_empty());
        let Some(m) = self.model.as_ref() else { return };
        let bundles = m.pending_bundles();
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
        let settled: Vec<String> =
            self.quiet.settled(now).into_iter().filter(|b| bundles.contains(b) && !waiting.contains(b)).collect();
        if settled.is_empty() || self.locked || self.frozen() {
            return;
        }
        let Some(m) = self.model.as_mut() else { return };
        let (actions, dropped) = m.expire_pending(&settled);
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
            Command::State => self.state_json(ob),
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
                    "fullscreen": self.fullscreen,
                    "settling": self.space_pending,
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
            Command::Ping | Command::TakeWatch(_) | Command::Quit | Command::Settings | Command::Mark(_) | Command::DebugPanic | Command::DebugMenuQuit | Command::DebugUpdate | Command::DebugClick { .. }
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
                    "pending": m.pending_of(s.id).len(),
                    // When it was last active: past the capacity, in place, the bottom
                    // places go by it (`Model::strip_in`).
                    "last_active": s.last_active,
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
                    "restored": m.restored_grade(w.meta.id).map(|g| g.name()),
                    "pooled": m.is_pooled(w.meta.id),
                })
            })
            .collect();
        // The desktop's still for a switch's cover (8L3-8L4) and the icons it is keyed by.
        let mut desktop_still = desktop_shot::status();
        desktop_still["icons"] = json!(self.patch_items.known);
        let mut state = json!({
            "ok": true,
            "space": m.current_space(m.main_display().id).0,
            "fullscreen": self.fullscreen,
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
            "switching": self.pending.as_ref().map(|p| p.id),
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
            "fullscreen": self.fullscreen,
            "settling": self.space_pending,
            "locked": self.locked,
            "last_arrival": {
                "rebuilt": m.last_arrival().rebuilt,
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
            active: c.active,
            gap: c.gap,
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
        assert!(!runs_after_quit(&Action::Park { id, frame }));
        assert!(!runs_after_quit(&Action::Raise { id }));
        assert!(!runs_after_quit(&Action::Focus { id }));
        assert!(!runs_after_quit(&Action::StateChanged));
    }

    #[test]
    fn a_failed_restore_stays_registered_until_the_window_is_seen_back() {
        use UnrestoredStep::*;
        // Still in the park zone on screen: moved again, never dropped.
        assert_eq!(unrestored_step(Some(false), Some((true, true)), false), Retry);
        // Off screen (another Space, app hidden): kept, waiting.
        assert_eq!(unrestored_step(Some(false), Some((false, true)), false), Wait);
        assert_eq!(unrestored_step(Some(false), Some((false, false)), false), Wait);
        // Seen back out of the zone, closed, or parked again by the model: done.
        assert_eq!(unrestored_step(Some(false), Some((true, false)), false), Done);
        assert_eq!(unrestored_step(Some(false), None, false), Done);
        assert_eq!(unrestored_step(Some(true), Some((true, true)), false), Done);
    }

    /// 6X: a parked window the model forgot (it left its Space, its element was reported
    /// destroyed) while the window server still lists it in the park zone stays registered.
    #[test]
    fn a_parked_window_the_model_forgot_stays_registered_until_it_is_back() {
        use UnrestoredStep::*;
        assert_eq!(unrestored_step(None, Some((true, true)), false), Retry, "on screen in the corner: moved back");
        assert_eq!(unrestored_step(None, Some((false, true)), false), Wait, "another Space's corner: waits for it");
        assert_eq!(unrestored_step(None, Some((true, false)), false), Done, "seen back");
        assert_eq!(unrestored_step(None, Some((false, false)), false), Done, "out of the zone elsewhere");
        assert_eq!(unrestored_step(None, None, false), Done, "closed");
    }

    /// 8J: a parked window the model let go that is a hidden tab (off screen in the park
    /// zone, its group's selected tab listed or managed) is let go: nothing can move it. A
    /// window the model still has is never judged one, nor one on screen.
    #[test]
    fn a_forgotten_hidden_tab_is_let_go() {
        use UnrestoredStep::*;
        assert_eq!(unrestored_step(None, Some((false, true)), true), Done, "a hidden tab at the park spot");
        assert_eq!(unrestored_step(None, Some((true, true)), true), Retry, "on screen: no hidden tab");
        assert_eq!(unrestored_step(Some(false), Some((false, true)), true), Wait, "the model has it: a real window");
    }

    /// The review of 8J, High: only a window the model let go as reported destroyed is ever
    /// judged; one that left its Space (dragged to another Desktop in Mission Control) beside
    /// a same-size sibling that would vouch stays real: it waits (and a quit retries it).
    #[test]
    fn only_a_window_reported_destroyed_is_judged_a_hidden_tab() {
        use UnrestoredStep::*;
        let zone_off = Some((false, true));
        let vouched = |c: Candidate| c == Candidate::Destroyed;
        assert!(let_go_as_tab(None, zone_off, LetGo::Destroyed, vouched), "the tab's signature");
        let mut asked = false;
        assert!(!let_go_as_tab(None, zone_off, LetGo::Other, |_| { asked = true; true }), "left its Space: real");
        assert!(!asked, "a real window is never even judged");
        assert!(!let_go_as_tab(None, zone_off, LetGo::Kept, |_| true), "a failed restore: real");
        assert!(!let_go_as_tab(Some(false), zone_off, LetGo::Destroyed, |_| true), "the model has it again");
        assert!(!let_go_as_tab(None, Some((true, true)), LetGo::Destroyed, |_| true), "on screen");
        assert!(!let_go_as_tab(None, Some((false, false)), LetGo::Destroyed, |_| true), "out of the zone");
        // The left-Space window: no hidden tab, so it waits off screen, as before.
        let tab = let_go_as_tab(None, zone_off, LetGo::Other, |_| true);
        assert_eq!(unrestored_step(None, zone_off, tab), Wait);
        assert_eq!(LetGo::Other.candidate(), Candidate::Real);
        assert_eq!(LetGo::Destroyed.candidate(), Candidate::Destroyed);
    }

    /// The review of 6X, M2: only an unpark tries a busy app again; a switch's arrivals
    /// (unveiled behind their snapshots) keep the single try and are never interrupted.
    #[test]
    fn only_an_unpark_retries_a_busy_app() {
        assert!(Batch::Unpark.retries() && !Batch::Unpark.interruptible());
        assert!(!Batch::Unveil.retries() && !Batch::Unveil.interruptible());
        assert!(!Batch::User.retries() && Batch::User.interruptible());
    }

    /// The review of 6X, L5: a window that refuses to move is looked up afresh only when its
    /// element went stale, and once.
    #[test]
    fn a_fresh_lookup_only_for_a_stale_element_and_once() {
        assert!(refresh_element(AxError::INVALID_ELEMENT, false));
        assert!(!refresh_element(AxError::INVALID_ELEMENT, true), "once");
        assert!(!refresh_element(AxError::CANNOT_COMPLETE, false), "a busy app keeps its element");
        assert!(!refresh_element(AxError::ATTRIBUTE_UNSUPPORTED, false), "a window that refuses to move");
    }

    /// The review of 6X, L6: closing a parked window lists it in the park zone for a moment;
    /// it warns only when a check sees it there again.
    #[test]
    fn a_forgotten_window_warns_only_on_its_second_sighting() {
        use UnrestoredStep::*;
        assert_eq!(Forgotten::Once.after(Done), (Forgotten::Once, false), "closed: gone at the first check, quiet");
        assert_eq!(Forgotten::Once.after(Retry), (Forgotten::Again, true));
        assert_eq!(Forgotten::Once.after(Wait), (Forgotten::Again, true));
        assert_eq!(Forgotten::Again.after(Retry), (Forgotten::Again, false), "warned once");
        assert_eq!(Forgotten::Not.after(Retry), (Forgotten::Not, false), "a failed restore logs as before");
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

    /// A switch to stage {3, 4} from {1, 2}, as `Model::switch_to` gives it.
    fn activation_batch() -> Vec<Action> {
        let r = Rect::new(100.0, 100.0, 400.0, 300.0);
        vec![
            Action::Park { id: WindowId(1), frame: r },
            Action::Park { id: WindowId(2), frame: r },
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
        // The switch's focus on another app's window (a stage of two apps): not hurried, so
        // that app is not raised in the corner before the overlay covers it.
        let other = |id: u32| Some(if id == 3 { 10 } else { 30 });
        assert_eq!(activation_run(true, 30, &activation_batch(), other).focus, None);
        // Nothing to switch: nothing to do.
        assert_eq!(activation_run(true, 30, &[], pid_of), ActivationRun::default());
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

    #[test]
    fn a_corner_with_icons_is_covered_by_the_desktops_still_never_the_wallpaper() {
        // 8L3: the owner's corner has desktop icons; the patch's picture would hide them.
        let rect = Rect::new(2499.0, 1348.0, 61.0, 92.0);
        let cap = Rect::new(2240.0, 1120.0, 320.0, 320.0);
        let cover = |picture| SwitchCover { rect, cap, picture, over: vec![(9, Rect::new(2000.0, 1000.0, 560.0, 440.0))] };
        let ok = |_: Rect| Ok(());
        let no = |_: Rect| Err("not ready");
        // Icons there: the still, or nothing (the activation then arrives at once).
        assert_eq!(resolve_cover(cover(CoverPicture::Desktop), ok, ok), Ok(cover(CoverPicture::Desktop)));
        assert_eq!(resolve_cover(cover(CoverPicture::Desktop), ok, no), Err("not ready"));
        // Nothing there: the patch's picture first, the still when that is not ready.
        assert_eq!(resolve_cover(cover(CoverPicture::Wallpaper), ok, ok), Ok(cover(CoverPicture::Wallpaper)));
        assert_eq!(resolve_cover(cover(CoverPicture::Wallpaper), |_| Err("an aerial"), ok), Ok(cover(CoverPicture::Desktop)));
        assert_eq!(resolve_cover(cover(CoverPicture::Wallpaper), |_| Err("an aerial"), no), Err("an aerial"));
    }

    #[test]
    fn only_windows_that_really_stay_shown_can_keep_a_switchs_corner_uncovered() {
        // 8L5: his windows nearly fill the screen. The outgoing stage's (parking), the
        // arriving ones, the parked ones, ours, a hidden or non-normal one never count; a
        // pinned or unmanaged window does, with whether a snapshot of it is at hand.
        let own = 500;
        let win = |id: u32, pid: i32, layer: i64, alpha: f64| windows::CgWindow {
            id,
            pid,
            owner: String::new(),
            name: None,
            layer,
            bounds: Rect::new(0.0, 0.0, 2560.0, 1410.0),
            onscreen: true,
            alpha,
        };
        let list = [
            win(1, 10, 0, 1.0), // the outgoing stage's, filling the screen
            win(2, 10, 0, 1.0), // its other window
            win(3, 30, 0, 1.0), // arriving (raised in the corner by the activation)
            win(4, 30, 0, 1.0), // parked in another stage
            win(5, own, 0, 1.0), // the corner patch, the overlay
            win(6, 40, 3, 1.0), // a floating panel, not a normal window
            win(7, 41, 0, 0.0), // drawn transparent
            win(8, 50, 0, 1.0), // pinned, snapshot kept
            win(9, 60, 0, 1.0), // unmanaged (the owner's), no snapshot
        ];
        let staying = staying_windows(&list, own, &[1, 2].into(), &[3].into(), &[1, 2, 4].into(), |id| id == 8);
        assert_eq!(
            staying.iter().map(|w| (w.id, w.snapshot)).collect::<Vec<_>>(),
            [(8, true), (9, false)],
            "front to back, as listed"
        );
        // The outgoing windows never count, even when the model has not marked them parked.
        let staying = staying_windows(&list[..2], own, &[1, 2].into(), &HashSet::new(), &HashSet::new(), |_| false);
        assert!(staying.is_empty());
    }

    /// A window of the owner's 2560x1440 Mac (10-16), as the window server lists it.
    fn listed(id: u32, pid: i32, bounds: Rect) -> windows::CgWindow {
        windows::CgWindow { id, pid, owner: String::new(), name: None, layer: 0, bounds, onscreen: true, alpha: 1.0 }
    }

    #[test]
    fn a_window_the_model_has_parked_counts_as_parked_only_where_it_is_listed_in_the_zone() {
        // 8L6, his log: window 101 (its stage parked) listed at its restore frame 533,184
        // 1741x1173 grew the patch's rectangle and every switch's cover to 473,124 2087x1316.
        let (display, visible) = (Rect::new(0.0, 0.0, 2560.0, 1440.0), Rect::new(0.0, 33.0, 2560.0, 1407.0));
        let list = [
            listed(96, 760, Rect::new(2559.0, 1408.0, 1794.0, 1251.0)), // parked, the corner
            listed(115, 795, Rect::new(2559.0, 1399.0, 1894.0, 1336.0)), // parked, the corner
            listed(101, 784, Rect::new(533.0, 184.0, 1741.0, 1173.0)),  // parked in the model's eyes
            listed(109, 746, Rect::new(263.0, 43.0, 2208.0, 1271.0)),   // shown, not parked
        ];
        let parked = parked_in_zone(&list, [96, 115, 101], display, visible, Corner::BottomRight);
        assert_eq!(parked, [96, 115].into());
        // The patch's rectangle from those: the corner's, never the screen's.
        let slivers: Vec<Rect> = list.iter().filter(|w| parked.contains(&w.id)).map(|w| w.bounds).collect();
        let rect = crate::engine::corner_patch::patch_rect(&slivers, display, 60.0).expect("slivers");
        assert!(crate::engine::corner_patch::cover_cap(display, Corner::BottomRight).contains_rect(&rect), "{rect:?}");
        // And 101 stays shown through a switch: the cover draws it or is not drawn.
        let staying = staying_windows(&list, 500, &[109].into(), &[96].into(), &parked, |_| false);
        assert_eq!(staying.iter().map(|w| w.id).collect::<Vec<_>>(), [101]);
    }

    #[test]
    fn an_activated_apps_window_counts_as_raised_wherever_macos_put_it() {
        // 8L6: the cover must hide what macOS raised; an activated app's window brought into
        // view counts (and then cannot be hidden in the corner); a card click's arriving
        // window outside the zone is not raised by macOS.
        let (display, visible) = (Rect::new(0.0, 0.0, 2560.0, 1440.0), Rect::new(0.0, 33.0, 2560.0, 1407.0));
        let in_zone = |b: Rect| crate::engine::park::is_parked_in_at(b, display, visible, Corner::BottomRight);
        let corner = Rect::new(2559.0, 1408.0, 1794.0, 1251.0);
        let into_view = Rect::new(492.0, 71.0, 1794.0, 1251.0);
        let list = [listed(96, 760, corner), listed(97, 760, into_view), listed(98, 761, into_view), listed(99, 761, corner)];
        let arriving: HashSet<u32> = [96, 97, 98].into();
        assert_eq!(raised_windows(&list, 500, &arriving, &[96, 97].into(), in_zone), [corner, into_view]);
        assert_eq!(raised_windows(&list, 500, &arriving, &HashSet::new(), in_zone), [corner], "a card click");
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
}
