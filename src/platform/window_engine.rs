//! The window engine: the one seam between what is shared (the model, the controller, the
//! strip, the rules, `state.json`, the guardian protocol) and how a stage's windows are
//! hidden, shown and brought back (docs/ARCHITECTURE.md, "Window engines").
//!
//! One strategy, [`WindowEngine`], with three implementations: the Corner engine
//! ([`super::corner::engine::CornerEngine`], today's Accessibility park corner), the Spaces
//! engine ([`super::spaces::engine::SpacesEngine`], 1.9), and the Apple mode's null-hide
//! engine ([`super::apple::engine::AppleEngine`]: Apple's Stage Manager arranges, nothing of
//! ours is hidden; installed at the release while `[windows] apple_stage_manager_on = "integrate"`, the
//! launch's choice installed again at the resume). Shared code asks the engine and never
//! which one runs; what only one engine needs stays inside it.
//!
//! **Which one runs** is decided at every launch ([`choose_at_launch`]: the config, the
//! launch probe, the switches; [`crate::engine::engine_choice`]) and can change once while
//! running: a Spaces engine whose checks fail ([`WindowEngine::health`]) hands over to the
//! Corner engine for the rest of the run ([`fall_back`], called by the controller once every
//! window is back).
//!
//! **Threads.** The engine is one value for the whole process ([`current`]): the WM thread
//! hides and shows through it inside the controller's batches, and the safety paths (quit, a
//! stand-down, a logout, the menu's last resort, the launch's and a display change's scan;
//! the panic hook and the signal watcher run on threads of their own) bring everything back
//! through the same value. The guardian, a process of its own, runs the engine the app it
//! watched ran ([`of`] the parked file's [`EngineKind`], [`install`]ed before its first scan). Its methods therefore take
//! `&self` and the engine is `Send + Sync`; state an engine keeps is behind its own locks.
//! The window calls ([`WindowEngine::hide`], [`WindowEngine::show`]) run on the WM thread
//! only, inside the controller's write (the stale-handle retry, the echo and failure
//! bookkeeping are the controller's, the same for every engine).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

use objc2_foundation::MainThreadMarker;

use super::ax::{AxElement, AxResult};
use super::safety::{Ours, Report, ScanScope};
use crate::engine::engine_choice::{self, Asked, Choice, ConfigEngine};
use crate::engine::geometry::Rect;
use crate::engine::landing_order;
use crate::engine::lend_round::Left;
use crate::engine::model::{Display, Model, WindowId};
use crate::ui::landing_math::Affine;
pub use crate::engine::recovery::EngineKind;
use crate::engine::recovery::{Known, ParkedFile, ScanDisplays, StageSpace};

/// A park asked for a lend ([`WindowEngine::lend_home`], round cmdtab-park).
#[derive(Debug, Clone)]
pub struct LendPark {
    /// The park frame, the display and the fallback frame, and whether to lend when the park
    /// does not take ([`crate::engine::lend_park::ParkAsk`]).
    pub ask: crate::engine::lend_park::ParkAsk,
    /// The window's element: the park's and the restore's moves.
    pub window: AxElement,
    /// How long the park's move may wait for the app's answer (seconds); `None`: the
    /// engine's own bound. A Dock press asks less: its lend must be made before the release.
    pub ax_seconds: Option<f32>,
}

/// What a lend did ([`WindowEngine::lend_home`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Lend {
    /// On its Desktop now.
    pub home: bool,
    /// The park asked: whether it took (`None`: none asked).
    pub parked: Option<bool>,
    /// What the park took (its move and read back), in milliseconds.
    pub park_ms: u64,
}

/// What a take back did ([`WindowEngine::take_back`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Back {
    /// Back where the engine hides it.
    pub back: bool,
    /// Its frame restored after a park (`None`: it was not parked).
    pub restored: Option<bool>,
}

/// A window of a round of Mission Control's lends ([`WindowEngine::park_round`]).
#[derive(Debug, Clone)]
pub struct RoundAsk {
    /// The window.
    pub id: WindowId,
    /// Its app: one app's windows are asked in turn, the apps at once.
    pub pid: i32,
    /// Its park.
    pub park: LendPark,
}

/// What a round's parks did ([`WindowEngine::park_round`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parks {
    /// Parked out of sight, still hidden: ready to go home ([`WindowEngine::home_round`]).
    pub took: Vec<WindowId>,
    /// Asked, and the park did not take (the app did not move the window in time): still
    /// hidden; judged again where it is listed when the round goes home.
    pub missed: Vec<WindowId>,
    /// Not asked: the round's deadline came before their turn (untouched, hidden).
    pub late: Vec<WindowId>,
    /// Not asked, and why ([`Left::NotHeld`], [`Left::Busy`]): left hidden.
    pub skipped: Vec<(WindowId, Left)>,
    /// Where each window asked was listed after its park's move (`None`: not listed, or the
    /// move not taken).
    pub landed: Vec<(WindowId, Option<Rect>)>,
    /// The apps asked at once.
    pub apps: usize,
    /// From the first park to the last answer, in milliseconds.
    pub ms: u64,
    /// The windows' moves added up, in milliseconds (what one at a time would have taken).
    pub sum_ms: u64,
}

/// What a round's second phase did ([`WindowEngine::home_round`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Homes {
    /// On their Desktop now, still counted hidden, with where the window server lists each
    /// and whether it was parked at its stage's card ([`crate::engine::lend_park::Zone::Card`]).
    pub lent: Vec<(WindowId, Option<Rect>, bool)>,
    /// Not put home, and why (the park did not take, the window did not go, or the engine
    /// does not hold it): still hidden.
    pub left: Vec<(WindowId, Left)>,
    /// The calls and their read back, in milliseconds.
    pub ms: u64,
    /// When the first call putting windows home was made and the last one returned (`None`:
    /// no call made): the gap between the first and the last window put home.
    pub calls: Option<(Instant, Instant)>,
}

/// How a window is brought to its frame ([`WindowEngine::show`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowHow {
    /// One try (a switch, a new window).
    Once,
    /// A busy app is tried again quickly, within the restore budget (quit, a stand-down:
    /// [`crate::engine::leftovers::RESTORE_RETRY`]).
    Retrying,
}

/// One incoming window's last stretch (round sm-landing): from the first step on, the real
/// window flies the rest of its picture's path through its Space's transform (the Spaces
/// engine), each step's transform at its instant, the last the identity at the flight's end;
/// then it goes onto its Desktop at `frame`.
#[derive(Debug, Clone, PartialEq)]
pub struct Stretch {
    /// The window.
    pub window: u32,
    /// Its frame (global, top-left points): where it is listed now, hidden, and where it lands.
    pub frame: Rect,
    /// Each step: when, and the transform then ([`crate::ui::landing_math::stretch`]).
    pub steps: Vec<(Instant, Affine)>,
    /// Its place among the switch's stretches, 0 the front ([`landing_order::ranks`]).
    pub rank: usize,
    /// The switch's stretches flying together (round landing-order: shown, stepped and
    /// landed back to front); `None` when it flies alone.
    pub convoy: Option<Arc<landing_order::Convoy>>,
}

/// What became of a [`Stretch`], told from the landing's own thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StretchNote {
    /// The window took over: the window server lists it on screen at its first pose (its
    /// picture goes now).
    Shown,
    /// It never took over (why): its picture lands as before.
    Skipped(&'static str),
    /// It flew to the end and is on its Desktop at its frame.
    Landed,
    /// It was stopped mid-flight (hidden again, or a bring-back took it home).
    Stopped,
}

impl StretchNote {
    /// How the stretch ended: `Some(true)` on its Desktop, `Some(false)` stopped or never
    /// taken over; `None` while it flies ([`Self::Shown`]).
    pub fn ended(self) -> Option<bool> {
        match self {
            Self::Shown => None,
            Self::Landed => Some(true),
            Self::Skipped(_) | Self::Stopped => Some(false),
        }
    }
}

/// Hears each [`StretchNote`] of a window (any thread).
pub type StretchDone = Arc<dyn Fn(u32, StretchNote) + Send + Sync>;

/// How a stage's windows are hidden, shown, moved in a switch and brought back.
pub trait WindowEngine: Send + Sync {
    /// Which engine this is (the parked file says it to the guardian, logs name it).
    fn kind(&self) -> EngineKind;

    /// Where a hide leaves window `id` of model `m`, as the window server will list it: what
    /// the controller checks a hide against, and where bringing the window back starts from.
    /// `None` for a window the model does not manage.
    fn hidden_at(&self, m: &Model, id: WindowId) -> Option<Rect>;

    /// Hides one window (an [`Action::Hide`](crate::engine::model::Action::Hide)) through
    /// `window`, its Accessibility element; `at` is what [`Self::hidden_at`] answered for it
    /// before the write (or the controller's own place for this one hide: a minimized
    /// window's dive). WM thread.
    fn hide(&self, id: WindowId, at: Rect, window: &AxElement) -> AxResult<()>;

    /// Brings one window to `frame`, shown (an
    /// [`Action::SetFrame`](crate::engine::model::Action::SetFrame): the model counts the
    /// window shown whether it was hidden or not, so the engine shows it either way).
    /// WM thread.
    fn show(&self, id: WindowId, frame: Rect, window: &AxElement, how: ShowHow) -> AxResult<()>;

    /// True when a window the window server lists at `listed` on display `d` is where this
    /// engine keeps hidden windows.
    fn is_hidden(&self, d: &Display, listed: Rect) -> bool;

    /// True when window `id`, listed at `listed` on display `d`, is hidden by this engine
    /// now: what a check of a hide or a show the engine was asked for reads (an arrival's).
    /// By the frame unless the engine knows better ([`Self::is_hidden`]); the Spaces engine:
    /// by what it holds, as last read back from the window server.
    fn hides_now(&self, _id: WindowId, d: &Display, listed: Rect) -> bool {
        self.is_hidden(d, listed)
    }

    /// True when window `id`, listed at `listed` on display `d`, is kept out of view by this
    /// engine now: what a switch's plan asks before it lets an incoming window that is
    /// already shown keep its place with no picture growing over it
    /// ([`crate::ui::transition_plan::already_shown`]). By [`Self::hides_now`] unless the
    /// engine knows better; the Spaces engine: held in its stage Space off every Desktop, or
    /// parked for a lend and listed away from the frame it had (a window put home at its
    /// frame on its Desktop, its park not taken or none asked, is in view though still
    /// counted hidden).
    fn keeps_out_of_view(&self, id: WindowId, d: &Display, listed: Rect) -> bool {
        self.hides_now(id, d, listed)
    }

    /// True when window `id`'s focus must wait until it is shown: focused (made main, its
    /// app frontmost) while this engine hides it, its app could take macOS to another
    /// Desktop. macOS activates an app on a Desktop where it has a window; a window kept off
    /// its Desktop is none, so an app whose other windows live on another Desktop is followed
    /// there (live 11-14, the Spaces engine: a card click on Desktop 2 for a stage of an app
    /// with windows on Desktop 1 showed Desktop 1 for 300 ms); an app with no window on a
    /// Desktop not shown is followed nowhere. The Corner engine: never, a parked window
    /// stays on its Desktop, in the corner. The Spaces engine also while the window flies its
    /// last stretch in a landing Space, which is no Desktop either.
    fn focus_waits_for_show(&self, _id: WindowId) -> bool {
        false
    }

    /// True when window `id` is on no Desktop for macOS while this engine hides it: macOS
    /// goes to a Desktop for an app's window only when it counts the window there, so an
    /// activation for such a window on another Desktop is this app's to carry out (an
    /// arrival, S3c). The Corner engine: never, a parked window stays on its Desktop.
    fn off_every_desktop(&self, _id: WindowId) -> bool {
        false
    }

    /// Puts window `id`, hidden by this engine off every Desktop, on its own Desktop for a
    /// while, still counted hidden: macOS counts it there from now on when it picks a Desktop
    /// for its app's activation (a Dock press, or ⌘Tab's switcher selecting the app, under the
    /// Spaces engine: [`crate::engine::model::Model::dock_home`]). With `park` (⌘Tab's
    /// switcher, round cmdtab-park; a Dock press, round dock-put-home) the window is moved to
    /// its park frame first, while still hidden ([`crate::engine::lend_park::lend`]); without
    /// it shows at its frame, at its old place in the window order. The model's show of it later brings it to
    /// its frame; [`Self::take_back`] hides it again. The Corner engine: nothing (a parked
    /// window is on its Desktop already).
    fn lend_home(&self, _id: WindowId, _park: Option<LendPark>) -> Lend {
        Lend::default()
    }

    /// Window `id`, put home by [`Self::lend_home`] and still counted hidden, goes back where
    /// this engine hid it, its frame restored after a park. `back` false when it is not put
    /// home any more (shown or hidden since).
    fn take_back(&self, _id: WindowId) -> Back {
        Back::default()
    }

    /// The window server's listing of every screen's Desktops, read now
    /// (`SLSCopyManagedDisplaySpaces`: each screen's Space shown and its kind, its Desktops
    /// with their uuids). `None`: this engine does not read it (the Corner engine).
    fn desktops(&self) -> Option<Vec<crate::platform::spaces::ffi::ScreenDesks>> {
        None
    }

    /// The Spaces the window server lists window `id` on now (Desktops, stage Spaces, a
    /// full-screen app's). `None`: this engine does not read them (the Corner engine).
    fn spaces_of(&self, _id: WindowId) -> Option<Vec<u64>> {
        None
    }

    /// The Desktops the window server shows now, one per screen (SkyLight's own word, read
    /// now, [`Self::desktops`]): what Mission Control shows while a Space change made inside
    /// it is not recognized yet. `None`: this engine does not know (the Corner engine).
    fn shown_desktops(&self) -> Option<Vec<u64>> {
        self.desktops().map(|list| list.iter().map(|d| d.current).filter(|c| *c != 0).collect())
    }

    /// The Desktop window `id`, hidden by this engine off every Desktop, was hidden from (and
    /// goes back to): the window server's id, as [`Self::shown_desktops`] names them. `None`
    /// for a window this engine does not hold.
    fn home_of(&self, _id: WindowId) -> Option<u64> {
        None
    }

    /// The first phase of a round of Mission Control's lends (round mc-lends,
    /// [`crate::engine::lend_round`]): every window of `asks`, hidden by this engine off every
    /// Desktop, moved to its park while still hidden, the apps at once, each app's windows in
    /// turn while `deadline` lasts. Nothing shows; each park's restore is owed from now on
    /// (a hide, a show, a sweep, [`Self::unpark_round`] or the take back sets the frame
    /// again). The Corner engine: nothing.
    fn park_round(&self, _asks: Vec<RoundAsk>, _deadline: Instant) -> Parks {
        Parks::default()
    }

    /// The second phase: windows `ids`, parked by [`Self::park_round`] and still hidden,
    /// onto their Desktops in one call per Desktop, those listed in the park zone only; each
    /// counted put home as [`Self::lend_home`]'s are. The others stay hidden.
    fn home_round(&self, _ids: &[WindowId]) -> Homes {
        Homes::default()
    }

    /// Windows `ids` (with their apps), put home by [`Self::home_round`] or
    /// [`Self::lend_home`], back into their stage Spaces in one call per Space, then their
    /// frames set again, the apps at once ([`Self::take_back`] for many).
    fn back_round(&self, ids: &[(WindowId, i32)]) -> Vec<(WindowId, Back)> {
        ids.iter().map(|(id, _)| (*id, self.take_back(*id))).collect()
    }

    /// Windows `ids` (with their apps), parked by [`Self::park_round`] and never put home,
    /// get their frames again where they are, hidden, the apps at once (Mission Control did
    /// not show after its key). How many took it.
    fn unpark_round(&self, _ids: &[(WindowId, i32)]) -> usize {
        0
    }

    /// Windows `ids` (with their apps), put home and parked by [`Self::home_round`], get
    /// their own frames again on their Desktop, still lent, the apps at once: picked in
    /// Mission Control, whose close carries them there (round mc-pick). Each move is read
    /// back through its element, as the window list gives Mission Control's layout while it
    /// shows. Those that took it.
    fn frames_back(&self, _ids: &[(WindowId, i32)]) -> Vec<WindowId> {
        Vec::new()
    }

    /// Windows `ids` (with their apps and corner parks), lent and parked at their stages'
    /// cards, moved to the park corner on their Desktop, still lent, the apps at once: their
    /// app in front waits for a pick's switch after Mission Control's close
    /// ([`crate::engine::mission_control::corner_while_waiting`]). Those that took it.
    fn to_corner(&self, _ids: &[(WindowId, i32, Rect)]) -> Vec<WindowId> {
        Vec::new()
    }

    /// True while window `id` is parked for a lend ([`Self::lend_home`] with a park) and its
    /// frame not set again: where it is listed is not its frame, so its move notices are not
    /// the model's (the platform drops them). The Corner engine: never.
    fn parked_lend(&self, _id: WindowId) -> bool {
        false
    }

    /// True when a window at `frame` is still where this engine hid it, out of anyone's
    /// reach, as far as what is `known` of the window allows; `displays`: the screens as the
    /// safety paths see them now. What the guardian's watch (and the app's watch of the
    /// windows it took from one) asks of every frame it reads. Any thread.
    fn stranded(&self, frame: Rect, displays: &ScanDisplays, known: Known) -> bool;

    /// The guardian, before its first scan: what the app's parked file says of where it hid
    /// its windows is taken as this process's own knowledge (the guardian never saw the
    /// app's screens or config).
    fn assume(&self, file: &ParkedFile);

    /// True when a window leaves the screen in one step as soon as it is told (no move an
    /// app carries out): a switch's outgoing windows can stay where they are until their
    /// pictures are taken at the switch, so no full-size picture is kept fresh on a timer
    /// for a switch nobody announced (a Dock click, ⌘Tab). False: the strip keeps the shown
    /// windows' pictures fresh every `[animation] snapshot_interval_ms`. Any thread.
    fn pictures_at_switch(&self) -> bool {
        false
    }

    /// True when this engine reads back each of its own hides and shows and tries again
    /// what did not take (its owed retries): the arrival's check of a Desktop arranged ahead
    /// ([`super::controller`]'s `Verify`, built for moves an app may apply late or not at
    /// all) is not built. The Corner engine: false, its parks and restores go through
    /// Accessibility. The Spaces engine: true (under it the check only re-read the engine's
    /// own answer, held the engine's tick off while it lived, and answered a hide that did not
    /// take with a move to the Corner engine's park frame).
    fn verifies_own_moves(&self) -> bool {
        false
    }

    /// True when an incoming window can fly the last stretch of its switch itself
    /// ([`Self::fly_stretch`]). The Corner engine: never (no live window can be scaled).
    fn lands_live(&self) -> bool {
        false
    }

    /// Plans window `s.window`'s last stretch ([`Stretch`]), told to `done` on the landing's
    /// thread; false when it cannot be planned (not hidden by this engine). The window stays
    /// hidden for the controller until it shows it ([`Self::show`] then asks the window
    /// nothing: it is at its frame). Main thread.
    fn fly_stretch(&self, _s: Stretch, _done: StretchDone) -> bool {
        false
    }

    /// Window `id`'s stretch is no longer wanted (its picture goes elsewhere now): it never
    /// takes over if it has not yet; one already flying finishes.
    fn cancel_stretch(&self, _id: u32) {}

    /// The windows whose stretch has not ended yet (planned or flying, not stopped): each
    /// still tells its end to its `done`. The Corner engine: none.
    fn stretching(&self) -> HashSet<u32> {
        HashSet::new()
    }

    /// The last line of defence in this process: every hidden window brought back, whatever
    /// state the model and the WM thread are in. Any thread. True when nothing failed.
    fn bring_all_back(&self, why: &str) -> bool;

    /// An abort's bring-back (SIGABRT, from the signal watcher's thread while the handler
    /// waits a bounded moment): what can be brought back at once, no scan, no retries; the
    /// guardian covers the rest. `None` when the engine's own state was busy throughout.
    fn bring_back_briefly(&self) -> Option<Report>;

    /// Every bring-back's last look: the window server's word on it, since a move that
    /// "succeeded" is no proof. Each window of an allowed app still hidden is brought back;
    /// `None` when none is left, else what this look knew of the windows left (`ours`: what
    /// the caller knows of whose they are), for the next try. `moved`: windows moved a moment
    /// ago, so the list is given its settle first. Bounded
    /// ([`crate::engine::leftovers::LEFTOVERS_MAX`]). Any thread.
    fn bring_back_rest(&self, why: &str, ours: Ours, moved: bool) -> Option<Ours>;

    /// The recovery scan (a launch, a display change, the guardian): every window in `scope`
    /// found hidden with no running copy to answer for it goes back to a crashed copy's
    /// registered frame, else its saved frame, else centered. Also the apps that did not
    /// answer, to be scanned again.
    fn recover(&self, why: &str, scope: &ScanScope) -> (Report, Vec<i32>);

    /// The windows this engine keeps hidden on a Desktop that shows now where the window
    /// server lists them off screen: present on their Desktop all the same, for whoever
    /// reads presence off the list (a window gone from the screen left its Desktop, a
    /// Desktop is recognized by its windows). The Corner engine: none, its hidden windows
    /// are listed on screen, in the corner.
    fn present_off_screen(&self) -> HashSet<u32>;

    /// The checks while running: `Some(why)` once this engine should not hide another window
    /// (a window not where a call put it after a retry, or misses set right too often:
    /// [`crate::engine::engine_choice::Health`]). The Corner engine never fails this way.
    fn health(&self) -> Option<String> {
        None
    }

    /// The places this engine keeps hidden windows that another process could not find by
    /// itself, for the parked file ([`ParkedFile::spaces`]): the Spaces engine's stage
    /// Spaces. The Corner engine: none (its park zones are in the file already).
    fn parked_spaces(&self) -> Vec<StageSpace> {
        Vec::new()
    }

    /// App `pid` was just opened (its launch notice; an app the arrangement takes in): an
    /// engine that can keep the app's first windows from showing before they are placed
    /// starts doing so. The Corner engine: nothing (a window shows when its app orders it
    /// in, and is placed after). WM thread.
    fn app_launched(&self, _pid: i32) {}

    /// True when window `id`, not on screen yet, is held by this engine on its way in: it
    /// shows as soon as it is placed ([`Self::let_new_out`], or a hide into its stage), so
    /// the controller places it now instead of waiting for it to show. The Corner engine:
    /// never.
    fn about_to_show(&self, _id: WindowId) -> bool {
        false
    }

    /// Window `id`, held on its way in ([`Self::about_to_show`]), is let onto the Desktop
    /// shown, where it shows at once: `placed` (after the stage it leaves behind has
    /// hidden), or not to be placed now. Nothing for a window not held so. WM thread.
    fn let_new_out(&self, _id: WindowId, _placed: bool) {}
}

static ENGINE: OnceLock<RwLock<Arc<dyn WindowEngine>>> = OnceLock::new();

fn slot() -> &'static RwLock<Arc<dyn WindowEngine>> {
    ENGINE.get_or_init(|| RwLock::new(first()))
}

/// The engine a process starts with: the Corner engine, until the launch chooses
/// ([`choose_at_launch`]) or the guardian installs its app's ([`install`]).
fn first() -> Arc<dyn WindowEngine> {
    Arc::new(super::corner::engine::CornerEngine)
}

/// The launch's choice and what happened to it since, for `ctl state` (`None` in a process
/// that did not choose: the guardian, the tools).
static CHOICE: Mutex<Option<Choice>> = Mutex::new(None);

fn lock_choice() -> std::sync::MutexGuard<'static, Option<Choice>> {
    CHOICE.lock().unwrap_or_else(|p| p.into_inner())
}

/// The app's launch: runs the launch probe when `config` or the test switch asks for the
/// Spaces engine, chooses ([`engine_choice::choose`]), installs the engine and logs the
/// choice with its reason. Main thread, once AppKit is up, before the WM thread starts.
pub fn choose_at_launch(mtm: MainThreadMarker, config: ConfigEngine) -> Choice {
    let asked = Asked { config, test: super::spaces::engine::from_env() };
    // `"spaces"` opens the landing variants as a test run does: the review build (11-14).
    crate::ui::landing_math::Landing::open_for_review(config == ConfigEngine::Spaces);
    // The window server's notices, registered before the probe so its panel's are heard
    // (round r5-notices); forwarded only if the Spaces engine is chosen, else removed.
    let notices = asked.probes() && super::spaces::notices::register();
    let probe = asked.probes().then(|| super::spaces::probe::run(mtm));
    match &probe {
        Some(p) if p.ok => tracing::info!(ms = format!("{:.1}", p.ms), steps = p.steps, "window engine: launch probe passed: {}", p.reason),
        Some(p) => tracing::warn!(ms = format!("{:.1}", p.ms), steps = p.steps, "window engine: launch probe failed: {}", p.reason),
        None => tracing::info!(config = config.name(), "window engine: launch probe not run"),
    }
    let choice = engine_choice::choose(asked, probe);
    tracing::info!(engine = choice.kind.name(), "window engine: {}", choice.why);
    let landing = crate::ui::landing_math::Landing::current();
    if landing != crate::ui::landing_math::Landing::Pictures {
        tracing::info!(landing = landing.name(), "window engine: a test or review run lands switches this way");
    }
    install(of(choice.kind));
    if notices {
        super::spaces::notices::chosen(choice.kind == EngineKind::Spaces);
    }
    *lock_choice() = Some(choice.clone());
    choice
}

/// The Corner engine from now on, for the rest of the run: a running engine's checks failed
/// (`why`) and every window it hid is back (the controller's part). The choice records it
/// for `ctl state`; the parked file names the engine from its next write.
pub fn fall_back(why: &str) {
    install(of(EngineKind::Corner));
    super::spaces::notices::stop();
    let mut c = lock_choice();
    match c.as_mut() {
        Some(choice) => {
            choice.kind = EngineKind::Corner;
            choice.fallback = Some(why.to_string());
        }
        None => *c = Some(Choice { kind: EngineKind::Corner, why: "fell back".into(), probe: None, fallback: Some(why.to_string()) }),
    }
}

/// The launch's choice as it stands ([`choose_at_launch`], [`fall_back`]).
pub fn choice() -> Option<Choice> {
    lock_choice().clone()
}

/// The engine this process runs: the Corner engine until another is installed.
pub fn current() -> Arc<dyn WindowEngine> {
    match slot().read() {
        Ok(e) => e.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// The engine of kind `kind`: how the guardian gets the engine the app it watched ran
/// ([`crate::engine::recovery::ParkedFile::engine`]).
pub fn of(kind: EngineKind) -> Arc<dyn WindowEngine> {
    match kind {
        EngineKind::Corner => Arc::new(super::corner::engine::CornerEngine),
        EngineKind::Spaces => super::spaces::engine::shared(),
        EngineKind::Apple => Arc::new(super::apple::engine::AppleEngine),
    }
}

/// Makes `engine` the one this process runs from now on ([`current`]). The swap itself
/// cannot panic, so the lock is never left held (the panic hook reads it).
pub fn install(engine: Arc<dyn WindowEngine>) {
    match slot().write() {
        Ok(mut e) => *e = engine,
        Err(poisoned) => *poisoned.into_inner() = engine,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_runs_the_corner_engine() {
        assert_eq!(current().kind(), EngineKind::Corner);
        assert_eq!(current().kind().name(), "corner");
    }

    #[test]
    fn a_parked_file_names_its_engine_and_an_older_one_is_the_corner_engines() {
        use crate::engine::recovery::ParkedFile;
        let older = ParkedFile::from_json(br#"{"visible":null,"windows":[]}"#).expect("an older file");
        assert_eq!(of(older.engine).kind(), EngineKind::Corner);
        install(of(older.engine));
        assert_eq!(current().kind(), EngineKind::Corner, "the guardian runs the engine the file names");
    }
}
