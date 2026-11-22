//! The fake desk (`cfg(test)`): one Mac's worth of macOS for a unit test, standing in for
//! everything the platform layer reads and asks (docs/DEVELOPMENT.md, "The bench").
//!
//! **The seam.** A [`Desk`] installed on a thread ([`install`]) answers that thread's
//! platform leaves, each of which asks [`with`] first under `cfg(test)`: the window server's
//! list (`windows::query`, `describe`), Accessibility ([`super::ax::AxElement`]'s fake
//! variant, [`El`]), input state (`input::*`), the frontmost app (`apps::*`), the Dock
//! (`dock::*`), the screens (`desktop::cg_displays`), the session lock, the strip
//! (`strip_model::present`, `wait_cover`), SkyLight's Space calls (`spaces::ffi::Sky`) and
//! the observer's probes. No desk installed: every leaf is the real one, as before. The
//! controller, the observer and both window engines above the leaves are the real code.
//!
//! **What the desk does** is what macOS was measured to do, each from the doc comment that
//! says so: a frame set is clamped as macOS clamps it and comes back as a move notice; our
//! `AXFrontmost` or a Dock click activates an app, which reports its activation, then its
//! own focus; an app with no window on the Desktop shown and one on another slides that
//! Desktop in, the Space change's notice at the slide's end
//! (`controller/arrival.rs`: windows of the arriving Desktop listed from +90 ms, the leaving
//! one's until +327 ms, the activation +324 ms, the notice +346 ms; the slide's own length
//! is the window server's, never the load's); a window in a stage
//! Space is on no Desktop, and macOS slides nowhere for it.
//!
//! **Time.** The thread's clock is frozen ([`super::clock::test`]). Every notice is queued
//! with a delay: its base as measured ([`delays`]) times the desk's load factor
//! ([`Desk::load`]; macOS's own animations, a slide or Mission Control's, keep their length); every platform call costs time on the clock ([`Costs`], times the
//! load), so a handler that asks many apps takes as long as it would and what is due
//! meanwhile is handled late. The bench (`controller/bench.rs`) delivers the queue.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::apps::AppInfo;
use super::ax::{AxError, AxResult, names, notifications as n};
use super::clock;
use super::input::{InputEvent, InputKind};
use super::spaces::engine::SpacesEngine;
use super::windows::CgWindow;
use crate::engine::displays::ScreenKey;
use crate::engine::geometry::{Point, Rect, Size};
use crate::engine::model::{Display, DisplayId};
use crate::engine::server_notice::{Burst, Kind};
use crate::ui::strip_model::{Cover, StripEvent, StripSnapshot};

/// The base delay of each notice kind, in milliseconds, as measured (each line names where);
/// the desk multiplies them by its load factor.
pub(crate) mod delays {
    /// Our `AXMain` on a window to its app's `AXFocusedWindowChanged` (an idle app answers
    /// within a frame or two; `engine/causes.rs`).
    pub const FOCUS: f64 = 15.0;
    /// An app made frontmost (our `AXFrontmost`, a Dock click's release, ⌘'s release) to
    /// NSWorkspace's activation notice (`engine/switcher.rs`: 3-12 ms after ⌘'s release;
    /// an app answering at the Accessibility timeout takes far longer: [`super::App::activates_ms`]).
    pub const ACTIVATION: f64 = 12.0;
    /// The activation notice to the app's own `AXApplicationActivated` ("once the app has
    /// taken it", `observe.rs`).
    pub const TOOK_ACTIVATION: f64 = 20.0;
    /// The activation to the app's own focus notice for its key window.
    pub const FOCUS_AFTER_ACTIVATION: f64 = 8.0;
    /// A frame set to its `AXWindowMoved` / `AXWindowResized`.
    pub const MOVED: f64 = 10.0;
    /// A Desktop slide (`controller/arrival.rs`, measured 10-13: the arriving Desktop's
    /// windows on screen from +90 ms, the leaving one's until +327 ms, the activation
    /// ~+324 ms, the notice +346 ms). The slide is the window server's own animation: its
    /// length is not the load's. The arriving Desktop's windows are listed from here on.
    pub const SLIDE_SHOWS: f64 = 90.0;
    /// The slide: the app it was made for takes the activation here ([`ACTIVATION`] later,
    /// times the load, its notice: ~+324 ms as measured).
    pub const SLIDE_ACTIVATES: f64 = 312.0;
    /// The slide: the leaving Desktop's windows listed until here; the window server's
    /// current Space is the arriving one from here on.
    pub const SLIDE_LEAVES: f64 = 327.0;
    /// The slide's end to `activeSpaceDidChangeNotification` (times the load): +346 ms.
    pub const SPACE_NOTICE: f64 = 19.0;
    /// A Desktop key or swipe: the activation of the arriving Desktop's front app, this long
    /// into the slide (`engine/slide_hold.rs`, 10-23: its notice 100-185 ms before the Space
    /// change's).
    pub const SLIDE_FRONT_APP: f64 = 190.0;
    /// A wallpaper click to Finder's activation (`engine/desktop_echo.rs`: 1025 ms after
    /// the press, 11-09).
    pub const FINDER_AFTER_WALLPAPER: f64 = 1025.0;
    /// Mission Control's own animation, not the load's: its key to its windows listed
    /// (`engine/overview.rs`, `engine/mission_control.rs`: seen ~40 ms after a heard key).
    pub const MISSION_SHOWS: f64 = 40.0;
    /// Mission Control closing: WindowManager's window goes first.
    pub const MISSION_WM_GOES: f64 = 50.0;
    /// Mission Control closing: the Dock's cover goes ~0.5 s after the closing key, when the
    /// windows are back in place (`engine/overview.rs`).
    pub const MISSION_DOCK_GOES: f64 = 500.0;
    /// The window server's notices, as the machine sends them, not the load's
    /// (`engine/server_notice.rs`, `probe-spaces cgs` 11-21): a window put into a Space and back
    /// to its membership notices (1325/1326, 3 ms); Mission Control's key to its open's notice
    /// (1508, 40 ms) and its closing key to its close's (32 ms).
    pub const SERVER_MEMBERSHIP: f64 = 3.0;
    /// See [`SERVER_MEMBERSHIP`].
    pub const SERVER_MISSION_OPENS: f64 = 40.0;
    /// See [`SERVER_MEMBERSHIP`].
    pub const SERVER_MISSION_CLOSES: f64 = 32.0;
    /// ⌘Tab's switcher: its list shows ~180 ms after the first Tab (`engine/switcher.rs`).
    pub const SWITCHER_SHOWS: f64 = 180.0;
    /// The strip: a transition presented to its pictures drawn (its answer to the cover
    /// wait; the main thread's next frame or two).
    pub const STRIP_COVERS: f64 = 20.0;
    /// A new window: `AXWindowCreated` to the window server listing it on screen.
    pub const WINDOW_SHOWS: f64 = 60.0;
    /// A launched app answering Accessibility after its launch notice.
    pub const APP_ANSWERS: f64 = 150.0;
}

/// What a platform call costs on the clock, in milliseconds (times the load): the desk's own
/// plain assumption of an idle Mac, there to make handlers take time at a load factor.
#[derive(Debug, Clone, Copy)]
pub struct Costs {
    /// One Accessibility call an app answers.
    pub ax: f64,
    /// One read of the window server's list.
    pub list: f64,
}

impl Default for Costs {
    fn default() -> Self {
        Self { ax: 0.5, list: 1.0 }
    }
}

/// A fake Accessibility element ([`super::ax::AxElement`]'s test variant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum El {
    /// The system-wide element.
    System,
    /// An app's element.
    App(i32),
    /// A window's.
    Window(u32),
    /// The Dock's application item `n`.
    DockItem(usize),
}

/// A value a fake element's attribute holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    /// A string.
    Str(String),
    /// A flag.
    Bool(bool),
    /// An element.
    El(El),
}

/// An app on the desk.
#[derive(Debug, Clone)]
pub struct App {
    /// Its pid.
    pub pid: i32,
    /// Its bundle id.
    pub bundle: String,
    /// Its name.
    pub name: String,
    /// Hidden (⌘H).
    pub hidden: bool,
    /// Its key window (`AXFocusedWindow`).
    pub focused: Option<u32>,
    /// How long it takes to report an activation, in ms before the load factor (`None`:
    /// [`delays::ACTIVATION`]): the knob for "an app busy at the Accessibility timeout".
    pub activates_ms: Option<f64>,
    /// How long it takes to say its key window changed (`AXFocusedWindowChanged`), in ms
    /// before the load factor (`None`: [`delays::FOCUS`], or [`delays::FOCUS_AFTER_ACTIVATION`]
    /// after an activation): the knob for "a busy app's focus notice came a second late"
    /// (`controller.rs`, `focus_may_switch`, 11-02).
    pub reports_focus_ms: Option<f64>,
    /// Its focus notice after an activation comes before NSWorkspace's activation notice
    /// (the two come through two queues, in either order: `engine/focus_replay.rs`, 10-12).
    pub focus_before_activation: bool,
    /// What one of its Accessibility answers costs, in ms before the load factor (`None`:
    /// [`Costs::ax`]); beyond the messaging timeout the call fails as `CannotComplete`.
    pub answers_ms: Option<f64>,
    /// The messaging timeout set on it, in seconds (0: the global 0.25 s).
    pub timeout: f32,
    /// `AXEnhancedUserInterface`.
    pub enhanced: bool,
    /// The process is gone.
    pub gone: bool,
    /// How long after answering an `AXRaise` the window server reorders its window, in ms
    /// before the load factor (`None`: at once): the knob for "an app answers its raise before
    /// its reorder reaches the window server" (`engine/raise_order.rs`: the fixture's 40 ms
    /// lag; a slow app's longer).
    pub reorders_ms: Option<f64>,
}

/// A window on the desk.
#[derive(Debug, Clone)]
pub struct Win {
    /// Its id.
    pub id: u32,
    /// Its app.
    pub pid: i32,
    /// Its title (made up).
    pub title: String,
    /// `AXSubrole`.
    pub subrole: String,
    /// Its frame.
    pub frame: Rect,
    /// Its layer (0: an app's window).
    pub layer: i64,
    /// The Spaces the window server lists it in: a Desktop, or a stage Space the Spaces
    /// engine made (then it is on no Desktop), or both after a show that left it listed.
    pub spaces: Vec<u64>,
    /// Minimized.
    pub minimized: bool,
    /// In full screen.
    pub fullscreen: bool,
    /// The window server lists it on screen (false for a just-created window until it shows).
    pub ordered_in: bool,
    /// Its owner's name in the window list.
    pub owner: String,
}

/// One screen and its Desktops.
#[derive(Debug, Clone)]
pub struct Screen {
    /// The display as the app knows it.
    pub display: Display,
    /// Its uuid (SkyLight's "Display Identifier").
    pub uuid: String,
    /// Its Desktops, left to right (window-server Space ids).
    pub desktops: Vec<u64>,
    /// The Desktop shown.
    pub shown: u64,
    /// A Desktop sliding in: listed on screen beside the one shown.
    pub sliding_in: Option<u64>,
    /// The Desktop a slide under way goes to, from its start.
    pub sliding_to: Option<u64>,
}

/// What macOS tells the app, queued until due.
#[derive(Debug, Clone)]
pub enum Notice {
    /// An Accessibility notification for app `pid` about `el`.
    Ax { pid: i32, el: El, name: &'static str },
    /// NSWorkspace: an app activated.
    Activated(i32),
    /// NSWorkspace: an app launched.
    Launched(i32),
    /// NSWorkspace: an app quit.
    Terminated(i32),
    /// `activeSpaceDidChangeNotification`.
    SpaceChanged,
    /// Global input in another app.
    Input(InputEvent),
    /// Input from the strip.
    Strip(StripEvent),
    /// The screens changed.
    Displays(Vec<Display>),
    /// One of the Dock's notices about ⌘Tab's switcher.
    Switcher(&'static str),
    /// Mission Control's key tap heard something.
    Mission(super::mission_key_tap::Heard),
    /// The window server's notices of one kind in one turn (its `at` is set as delivered).
    Server(crate::engine::server_notice::Burst),
    /// An app waited for answers Accessibility now (the observer's probe).
    Answers(i32),
    /// A step of the desk's own (macOS moving on), nothing told to the app.
    Step(Step),
}

/// macOS moving on by itself ([`Notice::Step`]).
#[derive(Debug, Clone)]
pub enum Step {
    /// An app's activation completes: it is frontmost from now on.
    Activation { pid: i32, raise_all: bool },
    /// A slide's arriving Desktop is listed on screen from now on.
    SlideShows { screen: usize, to: u64 },
    /// A slide's leaving Desktop is gone from the list; the arriving one is the one shown.
    SlideLeaves { screen: usize, to: u64 },
    /// A window the window server lists on screen from now on.
    WindowShows(u32),
    /// An overlay window of macOS's own listed from now on (`true`) or no longer.
    Overlay { owner: &'static str, layer: i64, on: bool },
    /// Mission Control puts the windows back in place (its close).
    MissionRestores,
    /// ⌘Tab's switcher shows its list from now on (`true`) or no longer.
    Switcher(bool),
    /// An app opens a window ([`Desk::open_window`]).
    Opens { id: u32, pid: i32, frame: Rect, title: Option<&'static str> },
}

/// A queued notice.
/// Something macOS does at its own time ([`Desk::at`]).
type Act = Box<dyn FnOnce(&mut Desk)>;

#[derive(Debug, Clone)]
pub struct Queued {
    /// When macOS sends it.
    pub due: Instant,
    seq: u64,
    /// What.
    pub notice: Notice,
}

/// What the app asked of the platform, in order ([`Desk::calls`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// A window's position or size set through Accessibility.
    Frame(u32, Rect),
    /// `AXRaise`.
    Raise(u32),
    /// `AXMain` set.
    Main(u32),
    /// An app's `AXFrontmost` set.
    Frontmost(i32),
    /// `AXMinimized` set.
    Minimized(u32, bool),
    /// SkyLight: windows added into a Space (and taken out of others).
    IntoSpace(u64, Vec<u32>),
    /// A strip snapshot presented, with its transition's id.
    Present(Option<u64>),
}

/// The strip as the desk plays it.
#[derive(Debug, Default)]
pub struct Strip {
    /// Whether it draws pictures for a transition (false: it answers nothing, as with no
    /// strip or a busy main thread).
    pub draws: bool,
    /// How long its pictures take to cover the windows, in ms before the load factor.
    pub covers_ms: f64,
    /// Transitions whose pictures covered the windows in time (the switch played with them),
    /// and those the switch moved windows for without them.
    pub covered: usize,
    /// See [`Self::covered`].
    pub uncovered: usize,
    /// The last snapshot presented.
    pub last: Option<StripSnapshot>,
    /// Transitions presented and not answered for yet.
    answered: BTreeSet<u64>,
    /// When each transition was presented.
    presented: BTreeMap<u64, Instant>,
    /// How many snapshots were presented.
    pub count: usize,
    /// How many transitions were presented (animated switches).
    pub transitions: usize,
    /// The card frames it published, per display.
    pub cards: BTreeMap<u32, Vec<(u64, Rect)>>,
}

/// The Dock.
#[derive(Debug, Clone)]
pub struct Dock {
    /// Its pid.
    pub pid: i32,
    /// Its bar.
    pub bar: Rect,
    /// Its application items: bundle path and frame.
    pub items: Vec<(String, Rect)>,
}

/// ⌘Tab's switcher as the Dock shows it.
#[derive(Debug, Clone, Default)]
pub struct Switcher {
    /// The list shows.
    pub shown: bool,
    /// The apps listed, in order.
    pub pids: Vec<i32>,
    /// The one selected.
    pub selected: usize,
}

/// The fake desk (module docs).
pub struct Desk {
    /// The load factor: every delay and cost is multiplied by it (1: the Mac as measured).
    pub load: f64,
    /// What calls cost.
    pub costs: Costs,
    /// The apps by pid.
    pub apps: BTreeMap<i32, App>,
    /// The windows by id.
    pub windows: BTreeMap<u32, Win>,
    /// Every window front to back.
    pub z: Vec<u32>,
    /// The frontmost app.
    pub front: Option<i32>,
    /// The screens, the main one first.
    pub screens: Vec<Screen>,
    /// The pointer.
    pub pointer: Point,
    /// The left button is down.
    pub left_down: bool,
    /// Shift, Command held.
    pub shift: bool,
    /// Command held.
    pub cmd: bool,
    /// When a mouse button or a key was last pressed.
    pub last_press: Option<Instant>,
    /// When a mouse button was last pressed.
    pub last_click: Option<Instant>,
    /// The Dock.
    pub dock: Dock,
    /// The screen is locked.
    pub locked: bool,
    /// The strip.
    pub strip: Strip,
    /// ⌘Tab's switcher.
    pub switcher: Switcher,
    /// The HID key tap hears (`mission_key_tap::hears`): false is "macOS refused the tap" (or
    /// keeps it off for secure input), and nothing it would forward reaches the app.
    pub tap_hears: bool,
    /// The app armed the tap for ⌘Tab's switcher (`mission_key_tap::switcher_up`): while it
    /// is, the tap forwards Escape, ⌘ let go and mouse-downs.
    pub tap_switcher: bool,
    /// An activation that slides tells its notice at the slide's start, not its end: as a
    /// Dock item's `AXPress` did in the live two-Desktop check (11-21, `spaces --auto`, the
    /// Dock arrivals: the activation handled ~30 ms before the arriving Desktop's windows were
    /// listed, the Space change's notice ~300 ms after it). False: as measured 10-13 for an
    /// `open` ([`delays::SLIDE_ACTIVATES`]).
    pub activation_leads_slide: bool,
    /// The window server tells the app its notices (`engine/server_notice.rs`: measured with
    /// the AppKit event loop turning, 11-21). False: none come, as on a Mac where they do not
    /// (or before round r5-notices); every poll then decides alone.
    pub server_notices: bool,
    /// macOS's own windows listed above the apps' (Mission Control's covers, Show Desktop's).
    pub overlays: Vec<CgWindow>,
    /// Stage Spaces the app made through SkyLight (never a Desktop).
    pub made_spaces: BTreeSet<u64>,
    /// Mission Control shows: the windows listed moved into its layout.
    pub mission: bool,
    /// The frames Mission Control's layout took the windows from.
    mission_frames: BTreeMap<u32, Rect>,
    /// What the app asked, in order, each with the ms since the desk was made.
    pub calls: Vec<(u64, Call)>,
    /// The probes the observer started: pid to (probe id, retry).
    pub probes: BTreeMap<i32, (u64, crate::engine::observe_retry::Retry)>,
    /// The scratch home (`state.json`, the parked file).
    pub home: PathBuf,
    /// When this login session began (`None`: unknown).
    pub login_started: Option<Instant>,
    /// The window server's session, as `state.json` names it (a restart makes another).
    pub session: &'static str,
    /// What a picture of a window taken at a switch costs, in ms before the load factor.
    pub shot_ms: f64,
    /// How long after a transition's pictures cover the windows the strip unveils them (the
    /// motion's length; the bench sets it from the config in force).
    pub unveil_after: Duration,
    /// macOS acts on its own clock, whatever the app's thread is doing: each act runs at its
    /// time at the desk's first use after it ([`Self::at`]), a handler that waits included.
    acts: Vec<(Instant, u64, Act)>,
    /// Windows raised whose reorder reaches the window server later ([`App::reorders_ms`]):
    /// in front from their time on.
    late_raises: Vec<(Instant, u32)>,
    /// What macOS does a while after the app's `count`-th `AXRaise` of a window
    /// ([`Self::on_raise`]): window, raises left to skip, how long after, the act.
    raise_hooks: Vec<(u32, usize, Duration, Act)>,
    queue: Vec<Queued>,
    seq: u64,
    next_space: u64,
    started: Instant,
    spaces_engine: Option<Arc<SpacesEngine>>,
}

thread_local! {
    static DESK: RefCell<Option<Desk>> = const { RefCell::new(None) };
}

/// What the whole process shares and a bench writes: the safety registry and the parked
/// file's record, the count of window writes, the enhanced-UI ledger.
static SHARED: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One test at a time on what the process shares ([`SHARED`]): a bench holds it for its
/// life, and so does a test that reads one of those while benches may run beside it.
pub fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    SHARED.lock().unwrap_or_else(|p| p.into_inner())
}

/// Removes the desk from its thread when dropped ([`install`]).
#[must_use = "the desk is removed when the guard drops"]
pub struct Installed(());

impl Drop for Installed {
    fn drop(&mut self) {
        DESK.with(|d| *d.borrow_mut() = None);
        crate::config::test_home::set(None);
    }
}

/// Makes `desk` this thread's platform until the guard drops; its scratch home becomes the
/// thread's config folder.
pub fn install(desk: Desk) -> Installed {
    crate::config::test_home::set(Some(desk.home.clone()));
    DESK.with(|d| *d.borrow_mut() = Some(desk));
    Installed(())
}

/// Runs `f` on this thread's desk; `None` when none is installed (the real platform answers).
/// A leaf asks one thing and returns: the desk never calls back into a leaf.
pub fn with<T>(f: impl FnOnce(&mut Desk) -> T) -> Option<T> {
    DESK.with(|d| {
        let mut d = d.try_borrow_mut().expect("the fake desk asked while it answers: a leaf called from inside the desk");
        d.as_mut().map(|d| {
            d.run_acts();
            f(d)
        })
    })
}

/// True when this thread has a desk.
pub fn active() -> bool {
    DESK.with(|d| d.try_borrow().is_ok_and(|d| d.is_some()))
}

/// A snapshot handed to the strip: taken by this thread's desk (`None` back), or handed back
/// for the real presenter when there is no desk.
pub fn present(snapshot: StripSnapshot) -> Option<StripSnapshot> {
    DESK.with(|d| {
        let mut d = d.try_borrow_mut().expect("the fake desk asked while it answers");
        match d.as_mut() {
            Some(desk) => {
                desk.present(snapshot);
                None
            }
            None => Some(snapshot),
        }
    })
}

/// This thread's Spaces engine while a desk is installed (the desk's own, over its fake
/// SkyLight), for `spaces::engine::shared`.
pub fn spaces_engine() -> Option<Arc<SpacesEngine>> {
    with(|d| d.spaces_engine.get_or_insert_with(|| Arc::new(SpacesEngine::for_test())).clone())
}

/// The first Desktop's Space id on the desk's main screen; the others follow.
pub const FIRST_DESKTOP: u64 = 1;
/// The first Space id SkyLight's fake gives a Space the app makes.
const FIRST_MADE_SPACE: u64 = 1000;
/// The Dock's pid on the desk.
pub const DOCK_PID: i32 = 90;
/// Finder's pid on the desk.
pub const FINDER_PID: i32 = 91;
/// WindowManager's pid on the desk (the owner of Mission Control's and Show Desktop's covers).
pub const WINDOW_MANAGER_PID: i32 = 92;

impl Desk {
    /// A desk with one 1440x900 screen (a menu bar, a Dock at the bottom), `desktops`
    /// Desktops on it, the first shown, Finder running with no window, at load factor `load`.
    pub fn new(desktops: usize, load: f64) -> Self {
        let dir = tempfile::tempdir().expect("a scratch home").keep();
        let display = Display {
            id: DisplayId(1),
            key: ScreenKey::new(0x610, 0xae2f, 0x4d6),
            frame: Rect::new(0.0, 0.0, 1440.0, 900.0),
            visible: Rect::new(0.0, 25.0, 1440.0, 800.0),
            corner: Default::default(),
        };
        let ids: Vec<u64> = (0..desktops.max(1) as u64).map(|i| FIRST_DESKTOP + i).collect();
        let mut desk = Self {
            load,
            costs: Costs::default(),
            apps: BTreeMap::new(),
            windows: BTreeMap::new(),
            z: Vec::new(),
            front: None,
            screens: vec![Screen { display, uuid: "Main".into(), shown: ids[0], desktops: ids, sliding_in: None, sliding_to: None }],
            pointer: Point::new(700.0, 450.0),
            left_down: false,
            shift: false,
            cmd: false,
            last_press: None,
            last_click: None,
            dock: Dock { pid: DOCK_PID, bar: Rect::new(420.0, 830.0, 600.0, 66.0), items: Vec::new() },
            locked: false,
            strip: Strip { draws: true, covers_ms: delays::STRIP_COVERS, ..Strip::default() },
            switcher: Switcher::default(),
            tap_hears: true,
            tap_switcher: false,
            activation_leads_slide: false,
            server_notices: false,
            overlays: Vec::new(),
            made_spaces: BTreeSet::new(),
            mission: false,
            mission_frames: BTreeMap::new(),
            calls: Vec::new(),
            probes: BTreeMap::new(),
            home: dir,
            login_started: None,
            session: "bench-session",
            shot_ms: 40.0,
            unveil_after: Duration::from_millis(300),
            acts: Vec::new(),
            late_raises: Vec::new(),
            raise_hooks: Vec::new(),
            queue: Vec::new(),
            seq: 0,
            next_space: FIRST_MADE_SPACE,
            started: clock::now(),
            spaces_engine: None,
        };
        desk.add_app(FINDER_PID, super::windows::FINDER_BUNDLE_ID, "Finder");
        desk
    }

    // ----- the script's side: apps and windows ------------------------------------------------

    /// An app running, with no window.
    pub fn add_app(&mut self, pid: i32, bundle: &str, name: &str) {
        self.apps.insert(
            pid,
            App { pid, bundle: bundle.into(), name: name.into(), hidden: false, focused: None, activates_ms: None, reports_focus_ms: None, focus_before_activation: false, answers_ms: None, timeout: 0.0, enhanced: false, gone: false, reorders_ms: None },
        );
        let n = self.dock.items.len() as f64;
        self.dock.items.push((format!("/Applications/{name}.app/"), Rect::new(self.dock.bar.x + 8.0 + 58.0 * n, self.dock.bar.y + 8.0, 50.0, 50.0)));
    }

    /// A standard window of app `pid` at `frame` on Desktop `desktop`, in front of the others,
    /// its app's key window. Nothing is told to the app (a window there from the start).
    pub fn add_window(&mut self, id: u32, pid: i32, frame: Rect, desktop: u64) {
        let owner = self.apps.get(&pid).map(|a| a.name.clone()).unwrap_or_default();
        self.windows.insert(
            id,
            Win { id, pid, title: format!("Doc {id}"), subrole: names::SUBROLE_STANDARD.into(), frame, layer: 0, spaces: vec![desktop], minimized: false, fullscreen: false, ordered_in: true, owner },
        );
        self.z.insert(0, id);
        if let Some(a) = self.apps.get_mut(&pid) {
            a.focused = Some(id);
        }
    }

    /// The app as NSWorkspace names it.
    pub fn app_info(&self, pid: i32) -> AppInfo {
        let a = self.apps.get(&pid);
        AppInfo { pid, bundle_id: a.map(|a| a.bundle.clone()), name: a.map(|a| a.name.clone()) }
    }

    /// The running apps as NSWorkspace lists them.
    pub fn running(&self) -> Vec<AppInfo> {
        self.apps.values().filter(|a| !a.gone).map(|a| self.app_info(a.pid)).collect()
    }

    /// A second screen comes, to the right of the main one, with one Desktop of its own;
    /// the app is told the screens ([`delays::ACTIVATION`] later).
    pub fn screen_comes(&mut self) {
        let main = self.screens[0].display;
        let frame = Rect::new(main.frame.max_x(), 0.0, 1920.0, 1080.0);
        let display = Display { id: DisplayId(2), key: ScreenKey::new(0x1e6d, 0x5b11, 0x77), frame, visible: Rect::new(frame.x, 25.0, frame.w, frame.h - 25.0), corner: Default::default() };
        self.screens.push(Screen { display, uuid: "External".into(), desktops: vec![50], shown: 50, sliding_in: None, sliding_to: None });
        let displays = self.displays();
        self.post(delays::ACTIVATION, Notice::Displays(displays));
    }

    /// The second screen goes: its windows go to the main screen's Desktop shown, as macOS
    /// moves them; the app is told the screens.
    pub fn screen_goes(&mut self) {
        if self.screens.len() < 2 {
            return;
        }
        let gone = self.screens.remove(1);
        let (shown, main) = (self.screens[0].shown, self.screens[0].display.visible);
        for w in self.windows.values_mut() {
            if w.spaces.iter().any(|s| gone.desktops.contains(s)) {
                w.spaces.retain(|s| !gone.desktops.contains(s));
                w.spaces.push(shown);
                w.frame = Rect::new(main.x + 60.0, main.y + 40.0, w.frame.w.min(main.w - 120.0), w.frame.h.min(main.h - 80.0));
            }
        }
        let displays = self.displays();
        self.post(delays::ACTIVATION, Notice::Displays(displays));
    }

    /// The displays as the app is told them.
    pub fn displays(&self) -> Vec<Display> {
        self.screens.iter().map(|s| s.display).collect()
    }

    /// The Dock item of app `pid`: its frame's center.
    pub fn dock_point(&self, pid: i32) -> Point {
        let path = self.apps.get(&pid).map(|a| format!("/Applications/{}.app/", a.name)).unwrap_or_default();
        let frame = self.dock.items.iter().find(|(p, _)| *p == path).map(|(_, f)| *f).expect("a Dock item for the app");
        Point::new(frame.x + frame.w / 2.0, frame.y + frame.h / 2.0)
    }

    // ----- time and the queue -------------------------------------------------------------

    /// `ms` at the desk's load, as a duration.
    pub fn at_load(&self, ms: f64) -> Duration {
        Duration::from_secs_f64((ms * self.load).max(0.0) / 1000.0)
    }

    /// The clock moves on by `ms` times the load: what the call just made took.
    fn spend(&self, ms: f64) {
        clock::test::advance(self.at_load(ms));
    }

    /// Queues `notice` `ms` (times the load) from now.
    pub fn post(&mut self, ms: f64, notice: Notice) {
        self.post_at(clock::now() + self.at_load(ms), notice);
    }

    /// Queues `notice` `fixed_ms` from now plus `ms` times the load: something macOS does at
    /// its own pace (an animation's step), then tells us as late as the load makes it.
    pub fn post_after(&mut self, fixed_ms: f64, ms: f64, notice: Notice) {
        self.post_at(clock::now() + Duration::from_secs_f64(fixed_ms.max(0.0) / 1000.0) + self.at_load(ms), notice);
    }

    /// Queues `notice` for `due` (no load factor: the script's own times).
    pub fn post_at(&mut self, due: Instant, notice: Notice) {
        self.seq += 1;
        self.queue.push(Queued { due, seq: self.seq, notice });
    }

    /// When the next queued notice or act of macOS is due.
    pub fn next_due(&self) -> Option<Instant> {
        self.queue.iter().map(|q| q.due).chain(self.acts.iter().map(|a| a.0)).chain(self.late_raises.iter().map(|r| r.0)).min()
    }

    /// macOS does `act` at `due` (the script's own time), on its own clock: at the desk's first
    /// use from then on, even inside a handler that holds the app's thread (a press, an app
    /// activated by itself, while the app waits for the strip).
    pub fn at(&mut self, due: Instant, act: impl FnOnce(&mut Desk) + 'static) {
        self.seq += 1;
        self.acts.push((due, self.seq, Box::new(act)));
    }

    /// macOS does `act` `after` the app's `nth` `AXRaise` of window `window` from now on (1:
    /// the next): a person's click while the app checks the order of a switch's raises, say.
    pub fn on_raise(&mut self, window: u32, nth: usize, after: Duration, act: impl FnOnce(&mut Desk) + 'static) {
        self.raise_hooks.push((window, nth.max(1), after, Box::new(act)));
    }

    /// The app raised `window`: its hooks counted, those due scheduled ([`Self::on_raise`]).
    fn raised_hooks(&mut self, window: u32) {
        let now = clock::now();
        for hook in std::mem::take(&mut self.raise_hooks) {
            match hook {
                (w, 1, after, act) if w == window => {
                    self.seq += 1;
                    self.acts.push((now + after, self.seq, act));
                }
                (w, n, after, act) if w == window => self.raise_hooks.push((w, n - 1, after, act)),
                other => self.raise_hooks.push(other),
            }
        }
    }

    /// Runs the acts due by now, in their order.
    fn run_acts(&mut self) {
        loop {
            let now = clock::now();
            let Some(i) = self.acts.iter().enumerate().filter(|(_, a)| a.0 <= now).min_by_key(|(_, a)| (a.0, a.1)).map(|(i, _)| i) else { break };
            let (_, _, act) = self.acts.remove(i);
            act(self);
        }
        self.settle_raises();
    }

    /// The late reorders due by now reach the window server, in their order.
    fn settle_raises(&mut self) {
        let now = clock::now();
        self.late_raises.sort_by_key(|r| r.0);
        while self.late_raises.first().is_some_and(|r| r.0 <= now) {
            let (_, id) = self.late_raises.remove(0);
            if self.windows.contains_key(&id) {
                self.z.retain(|w| *w != id);
                self.z.insert(0, id);
            }
        }
    }


    /// Takes the queued notice due first, when it is due by `by`.
    pub fn take_due(&mut self, by: Instant) -> Option<Queued> {
        let i = self.queue.iter().enumerate().filter(|(_, q)| q.due <= by).min_by_key(|(_, q)| (q.due, q.seq)).map(|(i, _)| i)?;
        Some(self.queue.remove(i))
    }

    fn call(&mut self, call: Call) {
        let ms = clock::now().saturating_duration_since(self.started).as_millis() as u64;
        self.calls.push((ms, call));
    }

    // ----- what shows -----------------------------------------------------------------------

    /// True when Space `space` is a Desktop listed on screen now (shown, or sliding in).
    fn space_listed(&self, space: u64) -> bool {
        self.screens.iter().any(|s| s.shown == space || s.sliding_in == Some(space))
    }

    /// True when `space` is a Desktop of some screen.
    pub fn is_desktop(&self, space: u64) -> bool {
        self.screens.iter().any(|s| s.desktops.contains(&space))
    }

    /// The Desktop window `id` is on, when it is on one.
    pub fn desktop_of(&self, id: u32) -> Option<u64> {
        self.windows.get(&id)?.spaces.iter().copied().find(|s| self.is_desktop(*s))
    }

    /// True when the window server lists window `w` on screen.
    pub fn on_screen(&self, w: &Win) -> bool {
        w.ordered_in && !w.minimized && !self.apps.get(&w.pid).is_some_and(|a| a.hidden) && w.spaces.iter().any(|s| self.space_listed(*s))
    }

    fn listed(&self, w: &Win) -> CgWindow {
        CgWindow { id: w.id, pid: w.pid, owner: w.owner.clone(), name: None, layer: w.layer, bounds: w.frame, onscreen: self.on_screen(w), alpha: 1.0 }
    }

    /// The window server's list: the on-screen windows front to back (macOS's own overlays
    /// and the Dock's bar first), then with `all` every other window.
    pub fn list(&mut self, all: bool) -> Vec<CgWindow> {
        self.spend(self.costs.list);
        self.settle_raises();
        let mut out: Vec<CgWindow> = self.overlays.clone();
        out.push(CgWindow { id: 9, pid: self.dock.pid, owner: "Dock".into(), name: None, layer: super::dock::DOCK_LAYER, bounds: self.dock.bar, onscreen: true, alpha: 1.0 });
        let (on, off): (Vec<&Win>, Vec<&Win>) = self.z.iter().filter_map(|id| self.windows.get(id)).partition(|w| self.on_screen(w));
        out.extend(on.into_iter().map(|w| self.listed(w)));
        if all {
            out.extend(off.into_iter().map(|w| self.listed(w)));
        }
        out
    }

    /// The windows `ids` as the window server describes them.
    pub fn describe(&mut self, ids: &[u32]) -> Vec<CgWindow> {
        self.spend(self.costs.list / 4.0);
        ids.iter().filter_map(|id| self.windows.get(id)).map(|w| self.listed(w)).collect()
    }

    /// The frontmost on-screen window of an app on Desktop `desktop`.
    fn front_window_on(&self, desktop: u64) -> Option<u32> {
        self.z.iter().copied().find(|id| self.windows.get(id).is_some_and(|w| w.layer == 0 && !w.minimized && w.spaces.contains(&desktop)))
    }

    // ----- Accessibility ---------------------------------------------------------------------

    fn app_of(&self, el: El) -> Option<i32> {
        match el {
            El::System => None,
            El::App(pid) => Some(pid),
            El::Window(id) => self.windows.get(&id).map(|w| w.pid),
            El::DockItem(_) => Some(self.dock.pid),
        }
    }

    /// One Accessibility call to `el`'s app: its cost on the clock, or the timeout's and
    /// `CannotComplete` when the app answers later than the messaging timeout; an element
    /// whose window or app is gone is invalid.
    fn ask(&mut self, el: El) -> AxResult<()> {
        let pid = match el {
            El::Window(id) if !self.windows.contains_key(&id) => return Err(AxError::INVALID_ELEMENT),
            _ => self.app_of(el),
        };
        let Some(app) = pid.and_then(|p| self.apps.get(&p)) else {
            self.spend(self.costs.ax);
            return if pid.is_some() && pid != Some(self.dock.pid) { Err(AxError::INVALID_ELEMENT) } else { Ok(()) };
        };
        if app.gone {
            return Err(AxError::INVALID_ELEMENT);
        }
        let ms = app.answers_ms.unwrap_or(self.costs.ax) * self.load;
        let timeout = if app.timeout > 0.0 { f64::from(app.timeout) * 1000.0 } else { 250.0 };
        if ms > timeout {
            clock::test::advance(Duration::from_secs_f64(timeout / 1000.0));
            return Err(AxError::CANNOT_COMPLETE);
        }
        clock::test::advance(Duration::from_secs_f64(ms / 1000.0));
        Ok(())
    }

    /// `AXUIElementSetMessagingTimeout`.
    pub fn ax_timeout(&mut self, el: El, seconds: f32) {
        if let Some(a) = self.app_of(el).and_then(|p| self.apps.get_mut(&p)) {
            a.timeout = seconds;
        }
    }

    /// The pid behind an element (no message sent).
    pub fn ax_pid(&self, el: El) -> AxResult<i32> {
        self.app_of(el).ok_or(AxError::INVALID_ELEMENT)
    }

    /// The window id behind an element.
    pub fn ax_window_id(&self, el: El) -> AxResult<u32> {
        match el {
            El::Window(id) if self.windows.contains_key(&id) => Ok(id),
            El::Window(_) => Err(AxError::INVALID_ELEMENT),
            _ => Err(AxError::NO_VALUE),
        }
    }

    /// An attribute read.
    pub fn ax_get(&mut self, el: El, name: &str) -> AxResult<Val> {
        self.ask(el)?;
        match el {
            El::System => match name {
                names::FOCUSED_APPLICATION => self.front.map(|p| Val::El(El::App(p))).ok_or(AxError::NO_VALUE),
                _ => Err(AxError::ATTRIBUTE_UNSUPPORTED),
            },
            El::App(pid) => {
                let app = self.apps.get(&pid).ok_or(AxError::INVALID_ELEMENT)?;
                match name {
                    names::ROLE => Ok(Val::Str("AXApplication".into())),
                    names::TITLE => Ok(Val::Str(app.name.clone())),
                    names::FRONTMOST => Ok(Val::Bool(self.front == Some(pid))),
                    names::HIDDEN => Ok(Val::Bool(app.hidden)),
                    names::ENHANCED_USER_INTERFACE => Ok(Val::Bool(app.enhanced)),
                    names::FOCUSED_WINDOW | names::MAIN_WINDOW => app.focused.filter(|w| self.windows.contains_key(w)).map(|w| Val::El(El::Window(w))).ok_or(AxError::NO_VALUE),
                    _ => Err(AxError::ATTRIBUTE_UNSUPPORTED),
                }
            }
            El::Window(id) => {
                let w = self.windows.get(&id).ok_or(AxError::INVALID_ELEMENT)?;
                match name {
                    names::ROLE => Ok(Val::Str(names::ROLE_WINDOW.into())),
                    names::SUBROLE => Ok(Val::Str(w.subrole.clone())),
                    names::TITLE => Ok(Val::Str(w.title.clone())),
                    names::MINIMIZED => Ok(Val::Bool(w.minimized)),
                    names::FULLSCREEN => Ok(Val::Bool(w.fullscreen)),
                    names::MAIN => Ok(Val::Bool(self.apps.get(&w.pid).is_some_and(|a| a.focused == Some(id)))),
                    _ => Err(AxError::ATTRIBUTE_UNSUPPORTED),
                }
            }
            El::DockItem(i) => match name {
                names::ROLE => Ok(Val::Str(names::ROLE_DOCK_ITEM.into())),
                names::SUBROLE => Ok(Val::Str(names::SUBROLE_APPLICATION_DOCK_ITEM.into())),
                names::URL => self.dock.items.get(i).map(|(p, _)| Val::Str(p.clone())).ok_or(AxError::NO_VALUE),
                _ => Err(AxError::ATTRIBUTE_UNSUPPORTED),
            },
        }
    }

    /// A flag set.
    pub fn ax_set_bool(&mut self, el: El, name: &str, value: bool) -> AxResult<()> {
        self.ask(el)?;
        match (el, name) {
            (El::Window(id), names::MAIN) => {
                self.call(Call::Main(id));
                if value {
                    self.make_key(id, delays::FOCUS);
                    self.follow(id);
                }
                Ok(())
            }
            (El::Window(id), names::MINIMIZED) => {
                self.call(Call::Minimized(id, value));
                self.set_minimized(id, value);
                Ok(())
            }
            (El::App(pid), names::FRONTMOST) => {
                self.call(Call::Frontmost(pid));
                if value && self.front != Some(pid) {
                    self.activate(pid, false);
                }
                Ok(())
            }
            (El::App(pid), names::ENHANCED_USER_INTERFACE) => {
                if let Some(a) = self.apps.get_mut(&pid) {
                    a.enhanced = value;
                }
                Ok(())
            }
            _ => Err(AxError::ATTRIBUTE_UNSUPPORTED),
        }
    }

    /// `AXPosition`.
    pub fn ax_position(&mut self, el: El) -> AxResult<Point> {
        self.ask(el)?;
        match el {
            El::Window(id) => self.windows.get(&id).map(|w| w.frame.origin()).ok_or(AxError::INVALID_ELEMENT),
            El::DockItem(i) => self.dock.items.get(i).map(|(_, f)| f.origin()).ok_or(AxError::NO_VALUE),
            _ => Err(AxError::ATTRIBUTE_UNSUPPORTED),
        }
    }

    /// `AXSize`.
    pub fn ax_size(&mut self, el: El) -> AxResult<Size> {
        self.ask(el)?;
        match el {
            El::Window(id) => self.windows.get(&id).map(|w| w.frame.size()).ok_or(AxError::INVALID_ELEMENT),
            El::DockItem(i) => self.dock.items.get(i).map(|(_, f)| f.size()).ok_or(AxError::NO_VALUE),
            _ => Err(AxError::ATTRIBUTE_UNSUPPORTED),
        }
    }

    /// `AXPosition` set: the window goes where macOS leaves it (its title bar kept above the
    /// visible frame's bottom, `engine/corner/park.rs`), and its app says it moved.
    pub fn ax_set_position(&mut self, el: El, p: Point) -> AxResult<()> {
        self.ask(el)?;
        let El::Window(id) = el else { return Err(AxError::ATTRIBUTE_UNSUPPORTED) };
        let size = self.windows.get(&id).ok_or(AxError::INVALID_ELEMENT)?.frame.size();
        self.place(id, Rect::from_parts(p, size));
        Ok(())
    }

    /// `AXSize` set.
    pub fn ax_set_size(&mut self, el: El, s: Size) -> AxResult<()> {
        self.ask(el)?;
        let El::Window(id) = el else { return Err(AxError::ATTRIBUTE_UNSUPPORTED) };
        let origin = self.windows.get(&id).ok_or(AxError::INVALID_ELEMENT)?.frame.origin();
        self.place(id, Rect::from_parts(origin, s));
        Ok(())
    }

    /// Window `id` asked to sit at `frame`: where macOS leaves it, with its move notice.
    fn place(&mut self, id: u32, frame: Rect) {
        let visible = self.screens[0].display.visible;
        let Some(w) = self.windows.get_mut(&id) else { return };
        let to = Rect::new(frame.x, frame.y.min(visible.max_y() - crate::engine::corner::park::MACOS_KEEPS_TITLE), frame.w, frame.h);
        if to == w.frame {
            return;
        }
        let (moved, resized) = (to.origin() != w.frame.origin(), to.size() != w.frame.size());
        w.frame = to;
        let pid = w.pid;
        self.call(Call::Frame(id, to));
        if moved {
            self.post(delays::MOVED, Notice::Ax { pid, el: El::Window(id), name: n::WINDOW_MOVED });
        }
        if resized {
            self.post(delays::MOVED, Notice::Ax { pid, el: El::Window(id), name: n::WINDOW_RESIZED });
        }
    }

    /// An action.
    pub fn ax_action(&mut self, el: El, name: &str) -> AxResult<()> {
        self.ask(el)?;
        match (el, name) {
            (El::Window(id), names::RAISE) => {
                self.call(Call::Raise(id));
                self.raised_hooks(id);
                let late = self.windows.get(&id).and_then(|w| self.apps.get(&w.pid)).and_then(|a| a.reorders_ms);
                match late {
                    Some(ms) => {
                        let due = clock::now() + self.at_load(ms);
                        self.late_raises.push((due, id));
                    }
                    None => self.raise(id),
                }
                self.follow(id);
                Ok(())
            }
            _ => Err(AxError::Api(-25206)),
        }
    }

    /// An app's `AXWindows`: its windows on a Desktop that shows, and its minimized ones
    /// (most apps list only the current Space's windows, `observe.rs`).
    pub fn ax_windows(&mut self, el: El) -> AxResult<Vec<El>> {
        self.ask(el)?;
        let El::App(pid) = el else { return Err(AxError::ATTRIBUTE_UNSUPPORTED) };
        Ok(self
            .windows
            .values()
            .filter(|w| w.pid == pid && (w.minimized || w.spaces.iter().any(|s| self.space_listed(*s))))
            .map(|w| El::Window(w.id))
            .collect())
    }

    /// The element at a point (the system-wide element's, or an app's own).
    pub fn ax_element_at(&mut self, el: El, p: Point) -> AxResult<El> {
        self.ask(el)?;
        if el == El::App(self.dock.pid) || el == El::System {
            if let Some(i) = self.dock.items.iter().position(|(_, f)| f.contains(p)) {
                return Ok(El::DockItem(i));
            }
            if el == El::App(self.dock.pid) {
                return Err(AxError::NO_VALUE);
            }
        }
        self.window_at(p).map(El::Window).ok_or(AxError::NO_VALUE)
    }

    /// The frontmost on-screen app window at `p`.
    pub fn window_at(&self, p: Point) -> Option<u32> {
        self.z.iter().copied().find(|id| self.windows.get(id).is_some_and(|w| w.layer == 0 && self.on_screen(w) && w.frame.contains(p)))
    }

    // ----- macOS's own behaviour ---------------------------------------------------------

    /// Window `id` to the front of the window order.
    pub fn raise(&mut self, id: u32) {
        self.settle_raises();
        if self.windows.contains_key(&id) {
            self.z.retain(|w| *w != id);
            self.z.insert(0, id);
        }
    }

    /// Window `id` becomes its app's key window; when that changes it, the app says so
    /// `after_ms` later (its `AXFocusedWindowChanged`).
    pub fn make_key(&mut self, id: u32, after_ms: f64) {
        let Some(pid) = self.windows.get(&id).map(|w| w.pid) else { return };
        let Some(app) = self.apps.get_mut(&pid) else { return };
        if app.focused == Some(id) {
            return;
        }
        app.focused = Some(id);
        let after_ms = app.reports_focus_ms.unwrap_or(after_ms);
        self.post(after_ms, Notice::Ax { pid, el: El::Window(id), name: n::FOCUSED_WINDOW_CHANGED });
    }

    /// The user drags window `id` to `frame` (one step of a drag: the button is down): it
    /// is there at once, and its app says it moved ([`delays::MOVED`] later).
    pub fn user_moves(&mut self, id: u32, frame: Rect) {
        let Some(w) = self.windows.get_mut(&id) else { return };
        if w.frame == frame {
            return;
        }
        w.frame = frame;
        let pid = w.pid;
        self.post(delays::MOVED, Notice::Ax { pid, el: El::Window(id), name: n::WINDOW_MOVED });
    }

    /// An app brings its window `id` forward by itself: raised, its key window (its focus
    /// notice at once), and macOS follows it to its Desktop when the app is frontmost
    /// ([`Self::follow`]).
    pub fn app_brings_forward(&mut self, id: u32) {
        self.raise(id);
        self.make_key(id, 0.0);
        self.follow(id);
    }

    /// The frontmost app's window `id` made key or raised while it is on a Desktop that does
    /// not show: macOS slides to that Desktop (`engine/slide_hold.rs`: a window raised and
    /// focused on the Desktop being left, "and macOS slid back to it";
    /// `engine/focus_elsewhere.rs`: "the landing's raise took macOS back to Desktop 4").
    fn follow(&mut self, id: u32) {
        let Some(w) = self.windows.get(&id) else { return };
        if self.front != Some(w.pid) || w.minimized {
            return;
        }
        let Some(desktop) = self.desktop_of(id) else { return };
        let Some(screen) = self.screens.iter().position(|s| s.desktops.contains(&desktop)) else { return };
        if self.screens[screen].sliding_to.unwrap_or(self.screens[screen].shown) != desktop {
            self.slide(screen, desktop);
        }
    }

    /// The Desktop macOS goes to for an activation of app `pid`: none when the app has a
    /// window on a Desktop that shows, or no window on any Desktop (a window in a stage
    /// Space is on none); else the Desktop of its key window, else of its frontmost one.
    fn desktop_for(&self, pid: i32) -> Option<(usize, u64)> {
        let mine: Vec<&Win> = self.z.iter().filter_map(|id| self.windows.get(id)).filter(|w| w.pid == pid && !w.minimized && w.layer == 0).collect();
        // The Desktop a slide under way goes to counts as the one shown.
        let shows = |s: u64| self.screens.iter().any(|sc| sc.sliding_to.unwrap_or(sc.shown) == s);
        if mine.iter().any(|w| w.spaces.iter().any(|s| shows(*s))) {
            return None;
        }
        let key = self.apps.get(&pid).and_then(|a| a.focused);
        let on_desktop = |w: &&Win| self.desktop_of(w.id);
        let target = mine.iter().find(|w| Some(w.id) == key).and_then(on_desktop).or_else(|| mine.iter().find_map(on_desktop))?;
        let screen = self.screens.iter().position(|s| s.desktops.contains(&target))?;
        Some((screen, target))
    }

    /// App `pid` is activated (our `AXFrontmost`, a Dock click's release, ⌘Tab's release):
    /// it takes the activation [`App::activates_ms`] later ([`Step::Activation`]).
    /// `raise_all`: a Dock click or ⌘Tab brings every window of the app forward.
    pub fn activate(&mut self, pid: i32, raise_all: bool) {
        let Some(app) = self.apps.get(&pid) else { return };
        let own = app.activates_ms.unwrap_or(delays::ACTIVATION);
        self.post(own, Notice::Step(Step::Activation { pid, raise_all }));
    }

    /// The Desktop of `screen` slides to `to`, as measured ([`delays`]).
    pub fn slide(&mut self, screen: usize, to: u64) {
        self.screens[screen].sliding_to = Some(to);
        self.post_after(delays::SLIDE_SHOWS, 0.0, Notice::Step(Step::SlideShows { screen, to }));
        self.post_after(delays::SLIDE_LEAVES, 0.0, Notice::Step(Step::SlideLeaves { screen, to }));
        self.post_after(delays::SLIDE_LEAVES, delays::SPACE_NOTICE, Notice::SpaceChanged);
    }

    /// The user goes to Desktop `to` of the main screen (⌃→, a swipe): the slide, and near
    /// its end the activation of that Desktop's front app when another was in front
    /// (`engine/slide_hold.rs`: 100-185 ms before the notice).
    pub fn go_to_desktop(&mut self, to: u64) {
        self.slide(0, to);
        if let Some(pid) = self.front_window_on(to).and_then(|id| self.windows.get(&id)).map(|w| w.pid)
            && self.front != Some(pid)
        {
            self.post_after(delays::SLIDE_FRONT_APP, 0.0, Notice::Step(Step::Activation { pid, raise_all: false }));
        }
    }

    /// Runs a step of the desk's own, now.
    pub fn step(&mut self, step: Step) {
        match step {
            Step::Activation { pid, raise_all } => {
                if self.apps.get(&pid).is_none_or(|a| a.gone) {
                    return;
                }
                // macOS picks the Desktop now, from where the app's windows are now; the
                // notices of an activation that slides come at the slide's end.
                let slide = self.desktop_for(pid).filter(|(screen, to)| self.screens[*screen].shown != *to && self.screens[*screen].sliding_to != Some(*to));
                self.front = Some(pid);
                // (The activation itself was `ACTIVATION` late already.)
                let after = match slide {
                    Some((screen, to)) => {
                        self.slide(screen, to);
                        if self.activation_leads_slide { 0.0 } else { delays::SLIDE_ACTIVATES }
                    }
                    None => 0.0,
                };
                let there = |d: &Self, w: &Win| w.spaces.iter().any(|s| d.space_listed(*s) || d.screens.iter().any(|sc| sc.sliding_to == Some(*s)));
                let mut key = self.apps.get(&pid).and_then(|a| a.focused).filter(|w| self.windows.contains_key(w));
                // A key window on no Desktop that shows: the app's frontmost window there is.
                if !key.and_then(|k| self.windows.get(&k)).is_some_and(|w| there(self, w))
                    && let Some(other) = self.z.iter().copied().find(|id| self.windows.get(id).is_some_and(|w| w.pid == pid && !w.minimized && there(self, w)))
                {
                    key = Some(other);
                    if let Some(a) = self.apps.get_mut(&pid) {
                        a.focused = key;
                    }
                }
                if raise_all {
                    let mine: Vec<u32> = self.z.iter().copied().filter(|id| self.windows.get(id).is_some_and(|w| w.pid == pid)).rev().collect();
                    for id in mine {
                        self.raise(id);
                    }
                }
                if let Some(key) = key {
                    self.raise(key);
                }
                let (reports, focus_first) = self.apps.get(&pid).map_or((None, false), |a| (a.reports_focus_ms, a.focus_before_activation));
                let focus_after = reports.unwrap_or(delays::FOCUS_AFTER_ACTIVATION);
                // The two queues: the focus first for an app that says so, the activation
                // notice then as late after it as the focus would have been.
                let (activated, focused) = if focus_first { (focus_after, 0.0) } else { (0.0, focus_after) };
                self.post_after(after, activated, Notice::Activated(pid));
                self.post_after(after, activated + delays::TOOK_ACTIVATION, Notice::Ax { pid, el: El::App(pid), name: n::APPLICATION_ACTIVATED });
                if let Some(key) = key {
                    self.post_after(after, focused, Notice::Ax { pid, el: El::Window(key), name: n::FOCUSED_WINDOW_CHANGED });
                }
            }
            Step::SlideShows { screen, to } => self.screens[screen].sliding_in = Some(to),
            Step::SlideLeaves { screen, to } => {
                self.screens[screen].shown = to;
                self.screens[screen].sliding_in = None;
                self.screens[screen].sliding_to = None;
            }
            Step::WindowShows(id) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.ordered_in = true;
                }
            }
            Step::Overlay { owner, layer, on } => {
                self.overlays.retain(|w| !(w.owner == owner && w.layer == layer));
                if on {
                    let pid = if owner == "Dock" { self.dock.pid } else { WINDOW_MANAGER_PID };
                    let id = 20 + layer as u32;
                    self.overlays.push(CgWindow { id, pid, owner: owner.into(), name: None, layer, bounds: self.screens[0].display.frame, onscreen: true, alpha: 1.0 });
                    self.overlays.sort_by_key(|w| std::cmp::Reverse(w.layer));
                }
            }
            Step::Switcher(shown) => self.switcher.shown = shown,
            Step::Opens { id, pid, frame, title } => {
                self.open_window(id, pid, frame);
                if let (Some(title), Some(w)) = (title, self.windows.get_mut(&id)) {
                    w.title = title.into();
                }
            }
            Step::MissionRestores => {
                self.mission = false;
                for (id, frame) in std::mem::take(&mut self.mission_frames) {
                    if let Some(w) = self.windows.get_mut(&id) {
                        w.frame = frame;
                    }
                }
            }
        }
    }

    /// Window `id` minimized or not, with its app's notice.
    pub fn set_minimized(&mut self, id: u32, minimized: bool) {
        let Some(w) = self.windows.get_mut(&id) else { return };
        if w.minimized == minimized {
            return;
        }
        w.minimized = minimized;
        let pid = w.pid;
        let name = if minimized { n::WINDOW_MINIATURIZED } else { n::WINDOW_DEMINIATURIZED };
        self.post(if minimized { 400.0 } else { 650.0 }, Notice::Ax { pid, el: El::Window(id), name });
    }

    /// App `pid` opens a window at `frame` on the Desktop shown: its `AXWindowCreated` now,
    /// listed on screen [`delays::WINDOW_SHOWS`] later, its app's key window.
    pub fn open_window(&mut self, id: u32, pid: i32, frame: Rect) {
        let desktop = self.screens[0].shown;
        self.add_window(id, pid, frame, desktop);
        if let Some(w) = self.windows.get_mut(&id) {
            w.ordered_in = false;
        }
        self.post(0.0, Notice::Ax { pid, el: El::Window(id), name: n::WINDOW_CREATED });
        self.post(delays::WINDOW_SHOWS, Notice::Step(Step::WindowShows(id)));
        self.post(delays::WINDOW_SHOWS + 5.0, Notice::Ax { pid, el: El::Window(id), name: n::FOCUSED_WINDOW_CHANGED });
    }

    /// App `pid` launches (`bundle`, `name`): NSWorkspace's notice [`delays::ACTIVATION`]
    /// later; it answers Accessibility once the observer's probe asked
    /// ([`delays::APP_ANSWERS`]); macOS activates it; it opens `windows` (id, frame, title)
    /// `opens_ms` after the launch, one every 200 ms.
    pub fn launch(&mut self, pid: i32, bundle: &str, name: &str, opens_ms: f64, windows: &[(u32, Rect, Option<&'static str>)]) {
        if !self.apps.contains_key(&pid) {
            self.add_app(pid, bundle, name);
        }
        if let Some(a) = self.apps.get_mut(&pid) {
            a.gone = false;
        }
        self.post(delays::ACTIVATION, Notice::Launched(pid));
        self.post(delays::ACTIVATION + 30.0, Notice::Step(Step::Activation { pid, raise_all: false }));
        for (i, (id, frame, title)) in windows.iter().enumerate() {
            self.post(opens_ms + 200.0 * i as f64, Notice::Step(Step::Opens { id: *id, pid, frame: *frame, title: *title }));
        }
    }

    /// App `pid` quits: its windows gone, NSWorkspace's notice; macOS then activates the app
    /// of the frontmost window left on the Desktop shown.
    pub fn quit(&mut self, pid: i32) {
        let mine: Vec<u32> = self.windows.values().filter(|w| w.pid == pid).map(|w| w.id).collect();
        for id in mine {
            self.windows.remove(&id);
            self.z.retain(|x| *x != id);
        }
        if let Some(a) = self.apps.get_mut(&pid) {
            a.gone = true;
        }
        self.post(0.0, Notice::Terminated(pid));
        if self.front == Some(pid) {
            self.front = None;
            let shown = self.screens[0].shown;
            if let Some(next) = self.front_window_on(shown).and_then(|id| self.windows.get(&id)).map(|w| w.pid) {
                self.activate(next, false);
            }
        }
    }

    /// Mission Control opens (`engine/overview.rs`: a `Dock` window at layer 20 and a
    /// `WindowManager` one at layer 19 over the display), the windows listed in its layout.
    pub fn mission_opens(&mut self) {
        self.post_after(delays::MISSION_SHOWS, 0.0, Notice::Step(Step::Overlay { owner: "WindowManager", layer: 19, on: true }));
        self.post_after(delays::MISSION_SHOWS, 0.0, Notice::Step(Step::Overlay { owner: "Dock", layer: 20, on: true }));
        self.mission = true;
        self.server_notice(delays::SERVER_MISSION_OPENS, Kind::Overview, Vec::new());
    }

    /// The window server's notice of `kind` naming `members` (window, Space), `ms` from now
    /// (the machine's time), when it tells the app ([`Self::server_notices`]).
    pub fn server_notice(&mut self, ms: f64, kind: Kind, members: Vec<(u32, u64)>) {
        if self.server_notices {
            let count = u32::try_from(members.len().max(1)).unwrap_or(u32::MAX);
            let burst = Burst { kind, at: clock::now(), count, windows: Vec::new(), members, unnamed: 0, ours: 0 };
            self.post_after(ms, 0.0, Notice::Server(burst));
        }
    }

    /// Something other than the app (macOS, its app ordering it out and in) puts window `id`
    /// into Space `to`, out of the others: listed there from now on, on screen when `to` is a
    /// Desktop shown; the window server's notices follow ([`delays::SERVER_MEMBERSHIP`]).
    pub fn space_moves(&mut self, id: u32, to: u64) {
        let Some(w) = self.windows.get_mut(&id) else { return };
        let from = std::mem::replace(&mut w.spaces, vec![to]);
        let out: Vec<(u32, u64)> = from.into_iter().filter(|s| *s != to).map(|s| (id, s)).collect();
        self.server_notice(delays::SERVER_MEMBERSHIP, Kind::TakenOut, out);
        self.server_notice(delays::SERVER_MEMBERSHIP, Kind::Added, vec![(id, to)]);
    }

    /// Mission Control closes: WindowManager's window first, the windows back in place and
    /// the Dock's cover gone ~0.5 s after ([`delays::MISSION_DOCK_GOES`]).
    pub fn mission_closes(&mut self) {
        self.server_notice(delays::SERVER_MISSION_CLOSES, Kind::Overview, Vec::new());
        self.post_after(delays::MISSION_WM_GOES, 0.0, Notice::Step(Step::Overlay { owner: "WindowManager", layer: 19, on: false }));
        self.post_after(delays::MISSION_DOCK_GOES - 10.0, 0.0, Notice::Step(Step::MissionRestores));
        self.post_after(delays::MISSION_DOCK_GOES, 0.0, Notice::Step(Step::Overlay { owner: "Dock", layer: 20, on: false }));
    }

    /// ⌘Tab's first Tab: the switcher selects the app used before the one in front (`order`:
    /// the apps, the one in front first); its list shows [`delays::SWITCHER_SHOWS`] later,
    /// with the Dock's notice.
    pub fn switcher_opens(&mut self, order: Vec<i32>) {
        self.switcher = Switcher { shown: false, pids: order, selected: 1 };
        self.post(delays::SWITCHER_SHOWS, Notice::Step(Step::Switcher(true)));
        self.post(delays::SWITCHER_SHOWS, Notice::Switcher("AXCreated"));
    }

    /// What the key tap heard while it is armed for ⌘Tab's switcher and hears: forwarded as
    /// the real tap does (`WmCommand::MissionInput`, stamped at its callback). Nothing when
    /// the tap is not armed or macOS refused it.
    pub fn tap_forwards(&mut self, heard: super::mission_key_tap::Heard) {
        if self.tap_hears && self.tap_switcher {
            self.post(0.0, Notice::Mission(heard));
        }
    }

    /// The switcher closes: its list, when it showed, goes 1 ms later (`engine/switcher.rs`;
    /// "a quicker ⌘Tab never shows it", `controller/switcher.rs`); with `activates`
    /// (⌘ let go, not Escape) macOS activates the app selected.
    pub fn switcher_closes(&mut self, activates: bool) {
        // Closed before the Dock built its list (a quick ⌘Tab, or a slow Dock): no list ever
        // shows, and none goes.
        self.queue.retain(|q| !matches!(q.notice, Notice::Step(Step::Switcher(true)) | Notice::Switcher("AXCreated")));
        if self.switcher.shown {
            self.post(1.0, Notice::Step(Step::Switcher(false)));
            self.post(1.0, Notice::Switcher("AXUIElementDestroyed"));
        }
        if activates && let Some(pid) = self.switcher.pids.get(self.switcher.selected).copied() {
            self.activate(pid, true);
        }
    }

    // ----- the person -----------------------------------------------------------------------

    /// A press of the left button at `p`, in another app's window or on the desktop: the
    /// input monitor hears it; macOS activates the app of the window under it and makes that
    /// window its key window (the bench's `press`; [`Self::at`] plays it at a time of its own).
    pub fn press(&mut self, p: Point) {
        self.pointer = p;
        let event = self.input(InputKind::MouseDown, None);
        self.post(0.0, Notice::Input(event));
        self.tap_forwards(super::mission_key_tap::Heard::Click);
        // The wallpaper: macOS hands the desktop to Finder (`engine/desktop_echo.rs`).
        if self.window_at(p).is_none() && !self.dock.bar.contains(p) {
            self.post(delays::FINDER_AFTER_WALLPAPER, Notice::Step(Step::Activation { pid: FINDER_PID, raise_all: false }));
        }
        let Some(id) = self.window_at(p) else { return };
        let pid = self.windows[&id].pid;
        self.raise(id);
        self.make_key(id, delays::FOCUS);
        if self.front != Some(pid) {
            self.activate(pid, false);
        }
    }

    /// The left button let go where the pointer is.
    pub fn release(&mut self) {
        let event = self.input(InputKind::MouseUp, None);
        self.post(0.0, Notice::Input(event));
    }

    // ----- input state ----------------------------------------------------------------------

    /// An input event of kind `kind` at the pointer, heard now, with the modifiers held.
    pub fn input(&mut self, kind: InputKind, key: Option<u16>) -> InputEvent {
        let now = clock::now();
        match kind {
            InputKind::MouseDown | InputKind::OtherMouseDown => {
                self.left_down |= kind == InputKind::MouseDown;
                self.last_click = Some(now);
                self.last_press = Some(now);
            }
            InputKind::MouseUp => self.left_down = false,
            InputKind::KeyDown => self.last_press = Some(now),
            InputKind::CommandKey => {}
        }
        InputEvent {
            kind,
            at: self.pointer,
            shift: self.shift,
            cmd: self.cmd,
            other_modifiers: false,
            control: false,
            option: false,
            clicks: i64::from(key.is_none()),
            key,
            stamp: now.saturating_duration_since(self.started).as_secs_f64() + 1000.0,
            heard: now,
        }
    }

    /// Since the last press of a mouse button (`click`) or of a button or a key.
    pub fn since_press(&self, click: bool) -> Duration {
        let at = if click { self.last_click } else { self.last_press };
        at.map_or(Duration::from_secs(3600), |t| clock::now().saturating_duration_since(t))
    }

    // ----- the strip ------------------------------------------------------------------------

    /// A snapshot presented.
    pub fn present(&mut self, snapshot: StripSnapshot) {
        let transition = snapshot.transition.as_ref().map(|t| t.id);
        if let Some(id) = transition
            && !self.strip.presented.contains_key(&id)
        {
            self.strip.presented.insert(id, clock::now());
            self.strip.transitions += 1;
        }
        self.call(Call::Present(transition));
        self.strip.count += 1;
        self.strip.last = Some(snapshot);
    }

    /// The WM thread waits for the strip's pictures of transition `id`: they cover the
    /// windows [`Strip::covers_ms`] after it was presented (the thread waits that out), or
    /// not within `timeout` (nothing covered, as `strip_model::wait_cover` answers then).
    /// The unveil follows at `unveil` after the answer.
    pub fn wait_cover(&mut self, id: u64, timeout: Duration) -> Cover {
        let unveil = self.unveil_after;
        let Some(presented) = self.strip.presented.get(&id).copied() else { return Cover::default() };
        let ready = presented + self.at_load(self.strip.covers_ms);
        let now = clock::now();
        if !self.strip.draws || ready > now + timeout {
            clock::test::advance(timeout);
            self.strip.uncovered += 1;
            return Cover::default();
        }
        clock::test::advance(ready.saturating_duration_since(now));
        self.strip.covered += 1;
        if self.strip.answered.insert(id) {
            self.post_at(clock::now() + unveil, Notice::Strip(StripEvent::Unveil { id }));
        }
        Cover { outgoing: true, incoming: true }
    }

    /// Pictures of `asked` windows taken at a switch, waited for `max` at most: how long it
    /// took, in ms.
    pub fn take_shots(&mut self, asked: usize, max: Duration) -> u64 {
        let took = if asked == 0 { Duration::ZERO } else { self.at_load(self.shot_ms).min(max) };
        clock::test::advance(took);
        took.as_millis() as u64
    }

    /// The observer started a probe of app `pid`: the app answers [`delays::APP_ANSWERS`]
    /// later (an app marked gone never does).
    pub fn probe_started(&mut self, pid: i32, id: u64, retry: crate::engine::observe_retry::Retry) {
        self.probes.insert(pid, (id, retry));
        if self.apps.get(&pid).is_some_and(|a| !a.gone) {
            self.post(delays::APP_ANSWERS, Notice::Answers(pid));
        }
    }

    // ----- SkyLight ----------------------------------------------------------------------

    /// `SLSCopyManagedDisplaySpaces`.
    pub fn sky_displays(&self) -> Vec<super::spaces::ffi::ScreenDesks> {
        self.screens
            .iter()
            .map(|s| super::spaces::ffi::ScreenDesks {
                screen: s.uuid.clone(),
                current: s.shown,
                current_kind: if s.desktops.contains(&s.shown) { 0 } else { 4 },
                desks: s.desktops.iter().map(|d| crate::engine::model::desks::Desk { id: *d, uuid: format!("DESK-{d}") }).collect(),
                others: Vec::new(),
            })
            .collect()
    }

    #[cfg(feature = "probes")]
    /// `SLSCopySpaces`: the Desktops, and with the orphans' bit the Spaces the app made.
    pub fn sky_all_spaces(&self, mask: i32) -> Vec<u64> {
        let mut out: Vec<u64> = self.screens.iter().flat_map(|s| s.desktops.iter().copied()).collect();
        if mask & 0x8 != 0 {
            out.extend(self.made_spaces.iter().copied());
        }
        out
    }

    /// `SLSCopySpacesForWindows`.
    pub fn sky_spaces_of(&self, ids: &[u32]) -> Vec<u64> {
        let mut out = Vec::new();
        for id in ids {
            for s in self.windows.get(id).map(|w| w.spaces.as_slice()).unwrap_or_default() {
                if !out.contains(s) {
                    out.push(*s);
                }
            }
        }
        out
    }

    /// `SLSSpaceCreate`.
    pub fn sky_create(&mut self) -> u64 {
        let id = self.next_space;
        self.next_space += 1;
        self.made_spaces.insert(id);
        id
    }

    /// `SLSSpaceDestroy`: its windows go to the Desktop shown.
    pub fn sky_destroy(&mut self, sid: u64) {
        self.made_spaces.remove(&sid);
        let shown = self.screens[0].shown;
        for w in self.windows.values_mut() {
            if w.spaces.contains(&sid) {
                w.spaces.retain(|s| *s != sid);
                if w.spaces.is_empty() {
                    w.spaces.push(shown);
                }
            }
        }
    }

    /// The bridged add: windows `ids` into Space `sid`, out of the kinds of Space `options`
    /// names (7: the Desktops; with the orphans' bit, the app's own Spaces too).
    pub fn sky_add_remove(&mut self, bridged: bool, sid: u64, ids: &[u32], options: i32) {
        // Only the bridged add moves another app's window (the spike, `spaces/ffi.rs`).
        if !bridged || !(self.made_spaces.contains(&sid) || self.is_desktop(sid)) {
            return;
        }
        self.call(Call::IntoSpace(sid, ids.to_vec()));
        let desktops: HashSet<u64> = self.screens.iter().flat_map(|s| s.desktops.iter().copied()).collect();
        for id in ids {
            let Some(w) = self.windows.get_mut(id) else { continue };
            w.spaces.retain(|s| *s != sid && !((options & 0x7 != 0 && desktops.contains(s)) || (options & 0x8 != 0 && !desktops.contains(s))));
            w.spaces.push(sid);
        }
    }

    // ----- the Dock --------------------------------------------------------------------------

    /// The bundle id of the app at bundle path `path`.
    pub fn bundle_at(&self, path: &str) -> Option<String> {
        self.apps.values().find(|a| format!("/Applications/{}.app/", a.name) == path).map(|a| a.bundle.clone())
    }
}
