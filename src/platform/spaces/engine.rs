//! The Spaces engine: a hidden window sits in a window-server Space of its stage, never
//! shown, at its own frame; it is brought back by moving it onto its Desktop again. Its pure
//! half is [`crate::engine::spaces`]; the calls are [`super::ffi`]'s.
//!
//! **The map.** One Space per stage per Desktop, made when the stage's first window hides
//! there (`SLSSpaceCreate`, the classic call: the Space is this connection's, so the window
//! server ends it with the process and its windows show again by themselves, crash or kill).
//! A hidden window's Desktop is read from the window server as it hides (the Space it is on
//! then), never assumed, and that is where a show puts it back: a window hidden on a Desktop
//! that is not shown goes back to that Desktop.
//!
//! **Hide** is the bridged add into the stage's Space (the only call that moves another
//! app's window), which takes the window off its Desktop. **Show** is the same add into the
//! Desktop, told to take the window out of every kind of Space: the call's last argument is
//! a mask of the kinds it removes from, and with the hide's own (7, the Desktops' kinds) or
//! with the bridged move, the window shows on its Desktop but stays listed in the stage
//! Space too (measured, 11-14). Then the frame through Accessibility. The stage Space itself
//! is never shown (a shown unmanaged Space shows on every Desktop).
//!
//! **Read back.** No return code means anything, so after every hide and show the window's
//! Spaces are read (`SLSCopySpacesForWindows`); a window not where the call put it is logged
//! and counted ([`SpacesEngine::status`]), a show that left it hidden is owed and tried
//! again ([`SpacesEngine::retry_owed`]).
//!
//! **Threads.** The state is behind one lock, never held across a window-server call: the
//! WM thread hides and shows; a bring-back from any other thread ([`SpacesEngine::sweep`])
//! asks the window server which windows are in this engine's Spaces and needs the state only
//! for their Desktops.
//!
//! **Health.** A window not where a hide or a show put it is sent once more; one the retry
//! sets right is a strike, one it does not fails the engine ([`Health`]: also
//! [`crate::engine::engine_choice::STRIKES`] strikes close together). A window gone, or on
//! no Space before the call (minimized, ordered out), says nothing of the engine. Once failed,
//! [`WindowEngine::health`] says why and the controller falls back to the Corner engine.
//!
//! Chosen at launch ([`crate::platform::window_engine::choose_at_launch`]: the launch probe,
//! the config, and for now an opt-in); the Corner engine is the default.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::ffi::{ALL_SPACES_MASK, Route, Sky};
use crate::engine::engine_choice::Health;
use crate::engine::geometry::Rect;
use crate::engine::leftovers;
use crate::engine::lend_park::{self, LendSteps, Parked};
use crate::engine::model::{Display, Model, WindowId};
use crate::engine::recovery::{Known, ParkedFile, ScanDisplays, StageSpace};
use crate::platform::ax::{AxElement, AxError, AxResult};
use crate::platform::safety::{self, Ours, Report, ScanScope};
use crate::platform::window_engine::{Back, EngineKind, Lend, LendPark, Motion, ShowHow, Stretch, StretchDone, WindowEngine};
use crate::platform::windows;
use crate::ui::strip_model::Transition;

#[path = "landing.rs"]
mod landing;

/// How long a hide or a show waits for the window server to list the window where the call
/// put it (the spike: within a millisecond or two).
const READ_BACK: Duration = Duration::from_millis(60);

/// The pause between two reads of a window's Spaces while waiting ([`READ_BACK`]).
const READ_BACK_STEP: Duration = Duration::from_micros(300);

/// How long a park's or a restore's move may hold the WM thread for an app's answer
/// (Accessibility's messaging timeout, round cmdtab-park): a quick ⌘Tab gives ~60 ms from
/// the Tab to ⌘'s release.
const PARK_AX_SECONDS: f32 = 0.1;

/// How long a park's or a restore's move waits for the window server to list the window
/// moved (the app moves it as it answers; a frame or so).
const PARK_READ_BACK: Duration = Duration::from_millis(20);

/// The stage key of a window the model names no stage for (a child read before its parent).
const NO_STAGE: u64 = 0;

/// The stage key of the Space where a just-opened app's new windows wait, never shown, until
/// they are placed ([`SpacesEngine::hold_new`]).
const NEW_STAGE: u64 = u64::MAX;

/// A window this engine hid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    /// The stage Space it is in.
    space: u64,
    /// The Desktop it was on when hidden: where a show puts it back.
    home: u64,
}

/// What the engine counts, for `ctl state` and the checks while running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    hides: u64,
    shows: u64,
    /// Hides after which the window was not in its stage's Space.
    hide_misses: u64,
    /// Shows after which the window was still in a stage Space.
    show_misses: u64,
    /// The slowest read back, in microseconds.
    read_back_max_us: u64,
    /// A just-opened app's new windows held before they showed, let out once placed, let
    /// out unplaced (not placed in time, or never shown by their app), and seen on screen
    /// before they could be held.
    new_held: u64,
    new_placed: u64,
    new_unplaced: u64,
    new_missed: u64,
    /// Windows put home for a Dock press ([`SpacesEngine::lend_window`]), and taken back
    /// into their stage Space unshown.
    lent: u64,
    taken_back: u64,
}

/// Which calls a test run makes fail (`ctl debug-engine-fail`): nothing is sent for them,
/// so their read back finds the window where it was, as when the window server refuses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FailMode {
    /// Every call is sent.
    #[default]
    Off,
    /// The next hide (with its retry).
    NextHide,
    /// The next show (with its retry).
    NextShow,
    /// The next hide or show, whichever comes first (with its retry).
    Next,
    /// Every bridged call from now on, the bring-backs' too (a window server that blocked
    /// the bridge mid-run); the Space calls of the classic route still go.
    All,
}

impl FailMode {
    /// The mode `word` names (`hide`, `show`, `next`, `all`, `off`).
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "next" | "" => Self::Next,
            "hide" => Self::NextHide,
            "show" => Self::NextShow,
            "all" => Self::All,
            "off" => Self::Off,
            _ => return None,
        })
    }
}

/// A hide or a show, for [`FailMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Hide,
    Show,
}

#[derive(Default)]
struct State {
    /// (Desktop, stage) to the Space made for it.
    stages: BTreeMap<(u64, u64), u64>,
    /// The windows hidden, as last read back.
    held: BTreeMap<u32, Held>,
    /// The stage the model last named for a window, and the frame it had then
    /// ([`WindowEngine::hidden_at`]).
    plans: BTreeMap<u32, (u64, Rect)>,
    /// Windows whose show left them in a stage Space.
    owed: BTreeSet<u32>,
    /// Windows of a just-opened app waiting in the Space for new windows, never shown yet
    /// ([`SpacesEngine::hold_new`]); each also in `held`.
    new: BTreeSet<u32>,
    counts: Counts,
    /// The checks while running.
    health: Health,
    /// The calls a test run makes fail.
    fail: FailMode,
    /// Stage Spaces of another process, as its parked file names them, each with its
    /// Desktop (the guardian's: [`WindowEngine::assume`]); swept with this engine's own.
    foreign: BTreeMap<u64, u64>,
    /// What the incoming windows' last stretches counted ([`landing`]).
    stretch: landing::StretchCounts,
    /// Windows put on their Desktop while still counted hidden ([`SpacesEngine::lend_window`]):
    /// each also in `held`, with the stage Space it goes back to.
    lent: BTreeSet<u32>,
    /// Windows parked for a lend whose frame is not set again yet (round cmdtab-park,
    /// [`lend_park`]), with the element their moves go through: lent and parked, or taken back
    /// with the restore owed. Their move notices are not the model's
    /// ([`WindowEngine::parked_lend`]); a hide, a show or a sweep sets the frame again.
    parked: BTreeMap<u32, (Parked, AxElement)>,
}

/// One window's lend and take back calls ([`LendSteps`]): the window server's add and read
/// back, and the moves through the window's element.
struct Steps<'a> {
    engine: &'a SpacesEngine,
    sky: &'a Sky,
    id: u32,
    held: Held,
    ours: &'a BTreeSet<u64>,
    element: Option<&'a AxElement>,
    /// What the moves took, with their read back.
    moves: Duration,
}

impl LendSteps for Steps<'_> {
    fn listed(&mut self) -> Option<Rect> {
        windows::describe(&[self.id]).first().map(|w| w.bounds)
    }

    fn move_to(&mut self, to: Rect) -> Option<Rect> {
        let element = self.element?;
        let t = Instant::now();
        let before = self.listed();
        let sent = element.bounded(PARK_AX_SECONDS, |e| e.set_position(to.origin()));
        let out = match sent {
            Err(e) => {
                tracing::debug!(id = self.id, "spaces engine: a lend's move was not taken: {e}");
                None
            }
            Ok(()) => {
                let by = Instant::now() + PARK_READ_BACK;
                loop {
                    let now = self.listed();
                    if now != before || now.is_some_and(|r| lend_park::at_origin(r, to)) || Instant::now() >= by {
                        break now;
                    }
                    std::thread::sleep(READ_BACK_STEP);
                }
            }
        };
        self.moves += t.elapsed();
        out
    }

    fn put_home(&mut self) -> bool {
        let (id, home) = (self.id, self.held.home);
        if let Err(e) = self.sky.add_remove_from(Route::Bridged, home, &[id], ALL_SPACES_MASK) {
            tracing::warn!(id, desktop = home, "spaces engine: a window could not be put home: {e}");
            return false;
        }
        let ours = self.ours;
        let (ok, after) = self.engine.read_back(self.sky, id, |s| out_of(s, ours));
        if !ok {
            tracing::warn!(id, desktop = home, listed = ?after, "spaces engine: a window put home is still in a stage Space");
        }
        ok
    }

    fn put_back(&mut self) -> bool {
        let (id, space) = (self.id, self.held.space);
        if !self.engine.blocked()
            && let Err(e) = self.sky.add_remove(Route::Bridged, space, &[id])
        {
            tracing::warn!(id, space, "spaces engine: a window put home could not be taken back: {e}");
        }
        let (ok, after) = self.engine.read_back(self.sky, id, |s| hidden_in(s, space));
        if !ok {
            tracing::warn!(id, space, listed = ?after, "spaces engine: a window put home did not go back into its stage Space");
        }
        ok
    }
}

/// The Spaces engine (see the module docs).
pub struct SpacesEngine {
    sky: OnceLock<Option<Sky>>,
    state: Mutex<State>,
    /// The incoming windows' last stretches ([`landing`]): their own lock, taken before the
    /// state's when both are.
    stretches: Mutex<BTreeMap<u32, landing::Fly>>,
}

/// The one Spaces engine of this process: the stage Spaces belong to the process's
/// window-server connection, so there is one map of them.
pub fn shared() -> Arc<SpacesEngine> {
    static ENGINE: OnceLock<Arc<SpacesEngine>> = OnceLock::new();
    ENGINE.get_or_init(|| Arc::new(SpacesEngine { sky: OnceLock::new(), state: Mutex::new(State::default()), stretches: Mutex::new(BTreeMap::new()) })).clone()
}

/// The engine the environment asks for: `STAGE_MANAGER_ENGINE=spaces`, honoured only with
/// `STAGE_MANAGER_ONLY` set (a test run on fixture apps), as the other test switches are.
/// `None` for the default.
pub fn from_env() -> Option<EngineKind> {
    kind_from(std::env::var("STAGE_MANAGER_ONLY").ok().as_deref(), std::env::var("STAGE_MANAGER_ENGINE").ok().as_deref())
}

fn kind_from(only: Option<&str>, engine: Option<&str>) -> Option<EngineKind> {
    if only.is_none_or(|o| o.trim().is_empty()) {
        return None;
    }
    match engine?.trim() {
        "spaces" => Some(EngineKind::Spaces),
        "corner" => Some(EngineKind::Corner),
        _ => None,
    }
}

/// Those of `ids` the window server lists in a Space that is no Desktop of any display (and
/// on no Desktop): in a stage Space, whichever process made it. The window server's own word,
/// for a test's probe (`probe-windows` under the test switch).
pub fn staged(ids: &[u32]) -> HashSet<u32> {
    let Ok(sky) = Sky::open() else { return HashSet::new() };
    let (desktops, _) = SpacesEngine::desktops(&sky);
    ids.iter()
        .copied()
        .filter(|id| {
            let now = sky.spaces_of(&[*id]);
            !now.is_empty() && desktop_among(&now, &desktops, &[]).is_none()
        })
        .collect()
}

/// True when the window server lists window `id` at `frame` (within half a point).
fn listed_at(id: u32, frame: Rect) -> bool {
    let near = |a: f64, b: f64| (a - b).abs() <= 0.5;
    windows::describe(&[id]).first().is_some_and(|w| near(w.bounds.x, frame.x) && near(w.bounds.y, frame.y) && near(w.bounds.w, frame.w) && near(w.bounds.h, frame.h))
}

/// True when `now` (a window's Spaces) says it is hidden in `space` and nowhere else.
fn hidden_in(now: &[u64], space: u64) -> bool {
    now == [space]
}

/// True when `now` (a window's Spaces) names none of `ours`: the window is out of every
/// stage Space.
fn out_of(now: &[u64], ours: &BTreeSet<u64>) -> bool {
    !now.iter().any(|s| ours.contains(s))
}

/// The Desktop among `now` (a window's Spaces), `desktops` being every Desktop of every
/// display and `shown` the ones that show: a window on several (assigned to all Desktops)
/// counts on one that shows.
fn desktop_among(now: &[u64], desktops: &BTreeSet<u64>, shown: &[u64]) -> Option<u64> {
    now.iter().copied().find(|s| shown.contains(s)).or_else(|| now.iter().copied().find(|s| desktops.contains(s)))
}

/// True when `now` (the Spaces of an app's windows) names a Desktop that is not shown,
/// `desktops` being every Desktop (and full-screen Space) of every display and `shown` the
/// ones that show: macOS, activating the app, would go there.
fn elsewhere(now: &[u64], desktops: &BTreeSet<u64>, shown: &[u64]) -> bool {
    now.iter().any(|s| desktops.contains(s) && !shown.contains(s))
}

impl SpacesEngine {
    /// True when window `id`'s app has a window macOS counts on a Desktop not shown now
    /// ([`elsewhere`]): one in a stage Space is on none, so it does not count. Unknown (no
    /// window server, the window gone): true, the safe answer for a focus.
    fn app_elsewhere(&self, id: u32) -> bool {
        let Some(sky) = self.sky() else { return true };
        let all = windows::every_window();
        let Some(pid) = all.iter().find(|w| w.id == id).map(|w| w.pid) else { return true };
        let mine: Vec<u32> = all.iter().filter(|w| w.pid == pid && w.layer == 0).map(|w| w.id).collect();
        let (desktops, shown) = Self::desktops(sky);
        elsewhere(&sky.spaces_of(&mine), &desktops, &shown)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// SkyLight's calls, opened once; `None` (said once) when it did not load.
    fn sky(&self) -> Option<&Sky> {
        self.sky
            .get_or_init(|| match Sky::open() {
                Ok(s) => {
                    let missing: Vec<&str> = s.names.iter().filter(|(_, found)| !found).map(|(n, _)| *n).collect();
                    tracing::info!(connection = s.cid, ?missing, "spaces engine: SkyLight loaded");
                    Some(s)
                }
                Err(e) => {
                    tracing::error!("spaces engine: {e}; nothing can be hidden");
                    None
                }
            })
            .as_ref()
    }

    /// Every stage Space this engine made, and those another process's file named.
    fn ours(&self) -> BTreeSet<u64> {
        let st = self.state();
        st.stages.values().copied().chain(st.foreign.keys().copied()).collect()
    }

    /// Each stage Space (this engine's and another process's) with its Desktop.
    fn homes(&self) -> BTreeMap<u64, u64> {
        let st = self.state();
        st.stages.iter().map(|((desktop, _), space)| (*space, *desktop)).chain(st.foreign.iter().map(|(s, d)| (*s, *d))).collect()
    }

    /// True when the test run makes this `call` fail; a one-call mode is used up by it.
    fn failing(&self, call: Call) -> bool {
        let mut st = self.state();
        let hit = match st.fail {
            FailMode::Off => false,
            FailMode::All | FailMode::Next => true,
            FailMode::NextHide => call == Call::Hide,
            FailMode::NextShow => call == Call::Show,
        };
        if hit && st.fail != FailMode::All {
            st.fail = FailMode::Off;
        }
        if hit {
            tracing::warn!(call = ?call, "spaces engine: a test run makes this call fail (nothing is sent)");
        }
        hit
    }

    /// True while every bridged call fails ([`FailMode::All`]).
    fn blocked(&self) -> bool {
        self.state().fail == FailMode::All
    }

    /// The calls a test run makes fail from now on (`ctl debug-engine-fail`).
    pub fn fail_calls(&self, mode: FailMode) {
        tracing::warn!(mode = ?mode, "spaces engine: a test run makes calls fail");
        self.state().fail = mode;
    }

    /// A window not where a call put it, sent once more by `retry` (true when the read back
    /// then found it there): a strike when the retry set it right, the engine failed when it
    /// did not, nothing when the window is gone or was on no Space (`before`). True when the
    /// window ended where the call put it.
    fn missed(&self, id: u32, what: &str, before: &[u64], retry: impl FnOnce() -> bool) -> bool {
        if before.is_empty() || windows::describe(&[id]).is_empty() {
            tracing::debug!(id, what, spaces = ?before, "spaces engine: a miss of a window gone or on no Space: no strike");
            return false;
        }
        if retry() {
            let failed = self.state().health.strike(Instant::now(), what);
            tracing::warn!(id, what, failed, "spaces engine: a window not where a call put it was set right by a retry (a strike)");
            return true;
        }
        let why = format!("a window not where a Space call put it after a retry ({what}, window {id})");
        let first = self.state().health.failed().is_none();
        if first {
            tracing::error!(id, what, "spaces engine: {why}; the engine has failed");
        } else {
            tracing::warn!(id, what, "spaces engine: {why} (the engine has failed already)");
        }
        self.state().health.fail(why);
        false
    }

    /// Every Desktop of every display, and the ones shown.
    fn desktops(sky: &Sky) -> (BTreeSet<u64>, Vec<u64>) {
        let displays = sky.displays();
        let all = displays.iter().flat_map(|(_, _, spaces)| spaces.iter().map(|(id, _)| *id)).collect();
        let shown = displays.iter().map(|(_, current, _)| *current).filter(|c| *c != 0).collect();
        (all, shown)
    }

    /// The Space of `stage` on Desktop `home`, made now when there is none.
    fn stage_space(&self, sky: &Sky, home: u64, stage: u64) -> Option<u64> {
        if let Some(space) = self.state().stages.get(&(home, stage)) {
            return Some(*space);
        }
        match sky.create(Route::Classic, false) {
            Ok(space) if space != 0 => {
                tracing::debug!(space, desktop = home, stage, "spaces engine: a Space made for a stage");
                self.state().stages.insert((home, stage), space);
                Some(space)
            }
            Ok(_) => {
                tracing::warn!(desktop = home, stage, "spaces engine: the window server made no Space");
                self.state().health.fail("the window server made no Space for a stage".into());
                None
            }
            Err(e) => {
                tracing::warn!(desktop = home, stage, "spaces engine: no Space made: {e}");
                self.state().health.fail(format!("no Space made for a stage: {e}"));
                None
            }
        }
    }

    /// Reads window `id`'s Spaces until `done` says so or [`READ_BACK`] is over; whether it
    /// did, and the last read.
    fn read_back(&self, sky: &Sky, id: u32, done: impl Fn(&[u64]) -> bool) -> (bool, Vec<u64>) {
        let began = Instant::now();
        loop {
            let now = sky.spaces_of(&[id]);
            if done(&now) {
                let us = began.elapsed().as_micros() as u64;
                let mut st = self.state();
                st.counts.read_back_max_us = st.counts.read_back_max_us.max(us);
                return (true, now);
            }
            if began.elapsed() >= READ_BACK {
                return (false, now);
            }
            std::thread::sleep(READ_BACK_STEP);
        }
    }

    /// Window `id` into its stage's Space; true when the window server lists it there. A
    /// window flying its last stretch ([`landing`]) stops and comes out of its landing Space
    /// with every other Space.
    pub(crate) fn hide_window(&self, id: u32) -> bool {
        let ok = self.hide_window_only(id);
        if ok {
            // A window parked for a lend takes its frame again, hidden.
            self.restore_parked(id, "hidden");
        }
        ok
    }

    fn hide_window_only(&self, id: u32) -> bool {
        let (_flying_guard, flying) = self.stop_stretch(id, true);
        let Some(sky) = self.sky() else { return false };
        let stage = self.state().plans.get(&id).map_or(NO_STAGE, |(stage, _)| *stage);
        let was = self.state().held.get(&id).copied();
        let now = sky.spaces_of(&[id]);
        let (desktops, shown) = Self::desktops(sky);
        // Where it is now, else where it was when hidden before, else the Desktop shown.
        let home = desktop_among(&now, &desktops, &shown).or(was.map(|h| h.home)).or(shown.first().copied());
        let Some(home) = home else {
            tracing::warn!(id, spaces = ?now, "spaces engine: no Desktop known for a window to hide");
            self.state().counts.hide_misses += 1;
            return false;
        };
        let Some(space) = self.stage_space(sky, home, stage) else {
            self.state().counts.hide_misses += 1;
            return false;
        };
        self.state().counts.hides += 1;
        if hidden_in(&now, space) {
            let mut st = self.state();
            st.held.insert(id, Held { space, home });
            st.lent.remove(&id);
            return true;
        }
        // Off its Desktop; a window already in another stage's Space (moved between two
        // hidden stages) out of that one too.
        let ours = self.ours();
        let failing = self.failing(Call::Hide);
        let send = || {
            if failing {
                return;
            }
            let sent = if out_of(&now, &ours) && flying.is_none() {
                sky.add_remove(Route::Bridged, space, &[id])
            } else {
                sky.add_remove_from(Route::Bridged, space, &[id], ALL_SPACES_MASK)
            };
            if let Err(e) = sent {
                tracing::warn!(id, space, "spaces engine: the hide could not be sent: {e}");
            }
        };
        send();
        let (mut ok, after) = self.read_back(sky, id, |s| hidden_in(s, space));
        if !ok {
            self.state().counts.hide_misses += 1;
            tracing::warn!(id, space, desktop = home, listed = ?after, "spaces engine: a hidden window is not in its stage's Space");
            ok = self.missed(id, "a hide", &now, || {
                send();
                self.read_back(sky, id, |s| hidden_in(s, space)).0
            });
        }
        if ok {
            let mut st = self.state();
            st.held.insert(id, Held { space, home });
            st.owed.remove(&id);
            st.lent.remove(&id);
            if st.new.remove(&id) {
                st.counts.new_placed += 1;
            }
        }
        ok
    }

    /// Window `id` out of every stage Space, onto its Desktop; true when the window server
    /// lists it in none of them (also when it was in none). A window planned to fly its last
    /// stretch ([`landing`]) never takes over now; one flying is out of every stage Space
    /// already and lands by itself.
    pub(crate) fn show_window(&self, id: u32) -> bool {
        let ok = self.show_window_only(id);
        if ok {
            // A window parked for a lend shown at the frame it had (a show with a frame of its
            // own sets that one instead: [`WindowEngine::show`]).
            self.restore_parked(id, "shown");
        }
        ok
    }

    /// The frame of window `id`, parked for a lend, set again through its element (it is
    /// hidden, or shown now); forgotten once the window server lists it there. `why` for the
    /// log.
    fn restore_parked(&self, id: u32, why: &str) {
        let Some((parked, element)) = self.state().parked.get(&id).cloned() else { return };
        let Some(sky) = self.sky() else { return };
        let ours = BTreeSet::new();
        let held = Held { space: 0, home: 0 };
        let mut steps = Steps { engine: self, sky, id, held, ours: &ours, element: Some(&element), moves: Duration::ZERO };
        let restored = lend_park::restore(&mut steps, parked);
        if restored {
            self.state().parked.remove(&id);
        }
        tracing::info!(id, why, restored, park_ms = steps.moves.as_millis() as u64, "spaces engine: a window parked for a lend takes its frame again");
    }

    fn show_window_only(&self, id: u32) -> bool {
        let _ = self.stop_stretch(id, false);
        let ours = self.ours();
        let held = self.state().held.get(&id).copied();
        if held.is_none() && ours.is_empty() {
            return true;
        }
        let Some(sky) = self.sky() else { return true };
        let now = sky.spaces_of(&[id]);
        if out_of(&now, &ours) {
            let mut st = self.state();
            st.held.remove(&id);
            st.owed.remove(&id);
            st.lent.remove(&id);
            return true;
        }
        let (desktops, shown) = Self::desktops(sky);
        // Its own Desktop while that still exists, else the one shown.
        let home = held.map(|h| h.home).filter(|h| desktops.contains(h)).or(shown.first().copied());
        let Some(home) = home else {
            tracing::warn!(id, "spaces engine: no Desktop to bring a window back to");
            self.missed_show(id);
            return false;
        };
        self.state().counts.shows += 1;
        let failing = self.failing(Call::Show);
        let send = || {
            if failing {
                return;
            }
            if let Err(e) = sky.add_remove_from(Route::Bridged, home, &[id], ALL_SPACES_MASK) {
                tracing::warn!(id, desktop = home, "spaces engine: the show could not be sent: {e}");
            }
        };
        send();
        let (mut ok, after) = self.read_back(sky, id, |s| out_of(s, &ours));
        if !ok {
            tracing::warn!(id, desktop = home, listed = ?after, "spaces engine: a shown window is still in a stage's Space; sent again");
            ok = self.missed(id, "a show", &now, || {
                send();
                self.read_back(sky, id, |s| out_of(s, &ours)).0
            });
        }
        if ok {
            let mut st = self.state();
            st.held.remove(&id);
            st.owed.remove(&id);
            st.lent.remove(&id);
            if st.new.remove(&id) {
                // A new window's show from the model's batch: placed.
                st.counts.new_placed += 1;
            }
        } else {
            tracing::warn!(id, desktop = home, "spaces engine: a shown window is still in a stage's Space after a retry");
            self.missed_show(id);
        }
        ok
    }

    fn missed_show(&self, id: u32) {
        let mut st = self.state();
        st.counts.show_misses += 1;
        st.owed.insert(id);
    }

    /// The Spaces the window server lists window `id` on now (none when SkyLight did not
    /// load).
    pub(crate) fn spaces_of(&self, id: u32) -> Vec<u64> {
        self.sky().map(|sky| sky.spaces_of(&[id])).unwrap_or_default()
    }

    /// True when this engine holds window `id` hidden (as last read back).
    pub fn holds(&self, id: u32) -> bool {
        self.state().held.contains_key(&id)
    }

    /// True when window `id` is held but put on its Desktop for a while
    /// ([`Self::lend_window`]).
    pub fn lent(&self, id: u32) -> bool {
        self.state().lent.contains(&id)
    }

    /// True when window `id` is on no Desktop for macOS: held in its stage Space (not lent),
    /// or flying its last stretch in its landing Space, until it lands (the unveil's show
    /// counts it shown before that: the owner's log of 11-15 06:42:57, a focus then let
    /// VS Code take macOS to its window on Desktop 1).
    fn off_desktop(&self, id: u32) -> bool {
        if self.in_flight(id) {
            return true;
        }
        let st = self.state();
        st.held.contains_key(&id) && !st.lent.contains(&id)
    }

    /// Window `id`, held in its stage Space, onto its own Desktop, while the engine still
    /// counts it hidden (the model does): macOS counts it on that Desktop from now on when it
    /// picks a Desktop for its app's activation (a Dock press, the owner's log of 11-15). With
    /// `park` (⌘Tab's switcher, round cmdtab-park) it is moved to its park frame first, while
    /// still in its stage Space ([`lend_park::lend`]): it shows at most as a sliver at the
    /// display's corner while the switcher is up. Without, it shows at its frame, at its old
    /// place in the window order (under the windows raised since it hid). The model's show of
    /// it later finds it out of every stage Space and sets its frame ([`WindowEngine::show`]);
    /// a hide puts it back as any hide (its frame set again); [`Self::take_back_window`] puts
    /// it back unshown. A window planned to fly its last stretch is not lent.
    pub(crate) fn lend_window(&self, id: u32, park: Option<LendPark>) -> Lend {
        if self.stretches().contains_key(&id) {
            return Lend::default();
        }
        let Some(held) = self.state().held.get(&id).copied() else { return Lend::default() };
        if self.lent(id) {
            let parked = park.is_some().then(|| self.state().parked.contains_key(&id));
            return Lend { home: true, parked, park_ms: 0 };
        }
        if self.blocked() {
            return Lend::default();
        }
        let Some(sky) = self.sky() else { return Lend::default() };
        let (desktops, _) = Self::desktops(sky);
        if !desktops.contains(&held.home) {
            return Lend::default();
        }
        let ours = self.ours();
        let (ask, element) = match park {
            Some(p) => (Some(p.ask), Some(p.window)),
            None => (None, None),
        };
        let mut steps = Steps { engine: self, sky, id, held, ours: &ours, element: element.as_ref(), moves: Duration::ZERO };
        let done = lend_park::lend(&mut steps, ask);
        let park_ms = steps.moves.as_millis() as u64;
        let mut st = self.state();
        if done.home {
            st.lent.insert(id);
            st.counts.lent += 1;
        }
        if let (Some(p), Some(e)) = (done.owed, element) {
            st.parked.insert(id, (p, e));
        }
        Lend { home: done.home, parked: done.parked, park_ms }
    }

    /// Window `id`, put home ([`Self::lend_window`]) and still counted hidden, back into its
    /// stage Space, then (parked for the lend) its frame set again there, unseen
    /// ([`lend_park::take_back`]). `back` false when it is not lent any more (shown or hidden
    /// since: nothing to do), or when it did not go back: then it is no longer held, so the
    /// tick's look hides it again ([`Self::hide_again`]), which also sets its frame again.
    pub(crate) fn take_back_window(&self, id: u32) -> Back {
        let (held, parked) = {
            let mut st = self.state();
            if !st.lent.remove(&id) {
                return Back::default();
            }
            (st.held.get(&id).copied(), st.parked.get(&id).cloned())
        };
        let Some(held) = held else { return Back::default() };
        let Some(sky) = self.sky() else { return Back::default() };
        let ours = self.ours();
        let element = parked.as_ref().map(|(_, e)| e.clone());
        let mut steps = Steps { engine: self, sky, id, held, ours: &ours, element: element.as_ref(), moves: Duration::ZERO };
        let done = lend_park::take_back(&mut steps, parked.map(|(p, _)| p));
        if done.moved_off {
            tracing::info!(id, "spaces engine: a window parked for a lend was moved off the park by its app");
        }
        let mut st = self.state();
        if done.back {
            st.counts.taken_back += 1;
        } else {
            st.held.remove(&id);
        }
        if done.restored == Some(true) {
            st.parked.remove(&id);
        }
        Back { back: done.back, restored: done.restored }
    }

    /// The windows this engine holds hidden (as last read back).
    pub fn held(&self) -> BTreeSet<u32> {
        self.state().held.keys().copied().collect()
    }

    /// Windows `ids` out of their stage Spaces, each onto its Desktop, at the frame it has:
    /// windows the model no longer counts hidden. How many came out.
    pub fn release(&self, ids: &[u32], why: &str) -> usize {
        let held: Vec<u32> = ids.iter().copied().filter(|id| self.holds(*id)).collect();
        let back = held.iter().filter(|id| self.show_window(**id)).count();
        if !held.is_empty() {
            tracing::info!(why, windows = ?held, back, "spaces engine: windows let out of their stage Spaces");
        }
        self.state().plans.retain(|id, _| !ids.contains(id));
        back
    }

    /// The shows that left a window hidden, tried again; the windows still owed one.
    pub fn retry_owed(&self) -> BTreeSet<u32> {
        let owed: Vec<u32> = self.state().owed.iter().copied().collect();
        for id in owed {
            if windows::describe(&[id]).is_empty() {
                // The window is gone: nothing is owed.
                let mut st = self.state();
                st.owed.remove(&id);
                st.held.remove(&id);
            } else if self.show_window(id) {
                tracing::info!(id, "spaces engine: a window owed its show is back");
            }
        }
        self.owed()
    }

    /// The windows whose show left them hidden.
    pub fn owed(&self) -> BTreeSet<u32> {
        self.state().owed.clone()
    }

    /// Hides window `id` again by the tick's look: the model counts it hidden and the engine
    /// does not hold it. True when it is in its stage's Space now.
    pub fn hide_again(&self, m: &Model, id: u32) -> bool {
        let _ = self.hidden_at(m, WindowId(id));
        self.hide_window(id)
    }

    /// Forgets what is kept for windows that are gone (`alive`: the windows the window
    /// server still lists) and ends the Spaces of stages that are gone (`stage_lives`) once
    /// no window is in them.
    pub fn tidy(&self, alive: &HashSet<u32>, stage_lives: impl Fn(u64) -> bool) {
        let empty: Vec<((u64, u64), u64)> = {
            let mut st = self.state();
            st.held.retain(|id, _| alive.contains(id));
            st.plans.retain(|id, _| alive.contains(id));
            st.owed.retain(|id| alive.contains(id));
            st.new.retain(|id| alive.contains(id));
            let held: BTreeSet<u32> = st.held.keys().copied().collect();
            st.lent.retain(|id| held.contains(id));
            st.parked.retain(|id, _| alive.contains(id));
            let used: BTreeSet<u64> = st.held.values().map(|h| h.space).collect();
            // The Space where new windows wait stays (a launch's watch may be putting a window
            // into it now); it ends with the others at a bring-back.
            st.stages
                .iter()
                .filter(|((_, stage), space)| *stage != NEW_STAGE && !used.contains(space) && !stage_lives(*stage))
                .map(|(k, s)| (*k, *s))
                .collect()
        };
        let Some(sky) = self.sky() else { return };
        for (key, space) in empty {
            let _ = sky.destroy(Route::Classic, space);
            self.state().stages.remove(&key);
            tracing::debug!(space, desktop = key.0, stage = key.1, "spaces engine: a gone stage's Space ended");
        }
    }

    /// Every window the window server lists in one of this engine's Spaces goes back onto
    /// its Desktop (the one shown when its own is not known or gone), whatever the state
    /// says: the bring-back of quit, a stand-down, a panic. With `end`, the Spaces left
    /// empty are ended. Any thread.
    pub fn sweep(&self, why: &str, end: bool) -> Report {
        self.sweep_where(why, end, false)
    }

    /// [`Self::sweep`]; with `strays_only`, the windows this run holds hidden stay where they
    /// are: only what nothing answers for comes back (a recovery scan while the run is
    /// arranging: its rescans every 3 s brought every hidden window back for a moment, live
    /// 11-14).
    fn sweep_where(&self, why: &str, end: bool, strays_only: bool) -> Report {
        if !strays_only {
            self.end_stretches(why);
        }
        let report = self.sweep_spaces(why, end, strays_only);
        if !strays_only {
            // A window parked for a lend (on its Desktop, or back in a stage Space with its
            // frame owed) takes its frame again; the registry's restore follows at a quit.
            let parked: Vec<u32> = self.state().parked.keys().copied().collect();
            for id in parked {
                self.restore_parked(id, why);
            }
        }
        report
    }

    fn sweep_spaces(&self, why: &str, end: bool, strays_only: bool) -> Report {
        let mut report = Report::default();
        let ours = self.ours();
        if ours.is_empty() {
            return report;
        }
        let Some(sky) = self.sky() else { return report };
        let own = std::process::id() as i32;
        let (desktops, shown) = Self::desktops(sky);
        let homes = self.homes();
        let blocked = self.blocked();
        let mut by_home: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
        let mut in_space: BTreeMap<u32, u64> = BTreeMap::new();
        for w in windows::every_window().iter().filter(|w| w.pid != own) {
            let now = sky.spaces_of(&[w.id]);
            let Some(space) = now.iter().copied().find(|s| ours.contains(s)) else { continue };
            if strays_only && self.state().held.contains_key(&w.id) {
                continue;
            }
            in_space.insert(w.id, space);
            report.checked += 1;
            let home = self
                .state()
                .held
                .get(&w.id)
                .map(|h| h.home)
                .or_else(|| homes.get(&space).copied())
                .filter(|h| desktops.contains(h))
                .or(shown.first().copied());
            match home {
                Some(home) => by_home.entry(home).or_default().push(w.id),
                None => report.failed += 1,
            }
        }
        for (home, ids) in &by_home {
            if blocked {
                tracing::warn!(why, desktop = home, "spaces engine: a test run makes the bring-back's call fail (nothing is sent)");
                continue;
            }
            if let Err(e) = sky.add_remove_from(Route::Bridged, *home, ids, ALL_SPACES_MASK) {
                tracing::warn!(why, desktop = home, "spaces engine: the bring-back could not be sent: {e}");
            }
        }
        let mut left = Vec::new();
        for id in by_home.values().flatten().copied() {
            let (ok, _) = self.read_back(sky, id, |s| out_of(s, &ours));
            let mut st = self.state();
            if ok {
                report.restored += 1;
                st.held.remove(&id);
                st.owed.remove(&id);
                st.new.remove(&id);
            } else {
                report.failed += 1;
                st.owed.insert(id);
                left.push(id);
            }
        }
        if report.checked > 0 || end {
            tracing::info!(why, hidden = report.checked, back = report.restored, left = ?left, "spaces engine: hidden windows brought back");
        }
        if end && !left.is_empty() {
            let rescued = self.end_spaces_holding(sky, why, &left, &in_space, &ours);
            report.restored += rescued.len();
            report.failed -= rescued.len();
            left.retain(|id| !rescued.contains(id));
        }
        if end && left.is_empty() {
            self.state().foreign.clear();
            let spaces: Vec<u64> = std::mem::take(&mut self.state().stages).into_values().collect();
            for space in &spaces {
                let _ = sky.destroy(Route::Classic, *space);
            }
            if !spaces.is_empty() {
                tracing::info!(why, spaces = spaces.len(), "spaces engine: the stage Spaces ended");
            }
        }
        report
    }

    /// The bring-back's last resort, for windows `left` in a stage Space after the moves
    /// (a window server that refuses the bridged move): the Spaces holding them (`in_space`)
    /// are ended with the classic call, which needs no bridge (this process made them), and
    /// each window is read back out of every stage Space. The windows that came out.
    fn end_spaces_holding(&self, sky: &Sky, why: &str, left: &[u32], in_space: &BTreeMap<u32, u64>, ours: &BTreeSet<u64>) -> Vec<u32> {
        let spaces: BTreeSet<u64> = left.iter().filter_map(|id| in_space.get(id).copied()).collect();
        for space in &spaces {
            let r = sky.destroy(Route::Classic, *space);
            tracing::warn!(why, space, result = ?r, "spaces engine: windows still in a stage Space after the bring-back: the Space ended");
            self.state().stages.retain(|_, s| s != space);
        }
        let mut out = Vec::new();
        for id in left.iter().copied() {
            let (ok, after) = self.read_back(sky, id, |s| out_of(s, ours));
            tracing::warn!(why, id, ok, listed = ?after, "spaces engine: a window after its Space ended");
            if ok {
                let mut st = self.state();
                st.held.remove(&id);
                st.owed.remove(&id);
                out.push(id);
            }
        }
        out
    }

    /// The windows hidden on a Desktop that shows now: present there, though the window
    /// server lists them off screen ([`WindowEngine::present_off_screen`]). Each one's Space
    /// is read from the window server: a window its app ordered out (closed, with or without
    /// a notice) is in no Space any more, and is not present.
    pub(crate) fn present_now(&self) -> HashSet<u32> {
        let held: Vec<(u32, Held)> = self.state().held.iter().map(|(id, h)| (*id, *h)).collect();
        if held.is_empty() {
            return HashSet::new();
        }
        let Some(sky) = self.sky() else { return HashSet::new() };
        let (_, shown) = Self::desktops(sky);
        held.into_iter().filter(|(id, h)| shown.contains(&h.home) && sky.spaces_of(&[*id]).contains(&h.space)).map(|(id, _)| id).collect()
    }

    /// Lets out every window the window server lists in one of this engine's Spaces that
    /// the engine does not hold (a window its app ordered out while hidden, forgotten, and
    /// ordered in again comes back in the Space it was in): onto that Space's Desktop. No
    /// window stays in a stage Space with nothing answering for it. `list`: every window, on
    /// screen or not (the tick's read, [`windows::wake_list`]). How many came out.
    pub fn let_strangers_out(&self, list: &[windows::CgWindow]) -> usize {
        let (spaces, held): (BTreeMap<u64, u64>, BTreeSet<u32>) = {
            let st = self.state();
            (st.stages.iter().map(|((desktop, _), space)| (*space, *desktop)).collect(), st.held.keys().copied().collect())
        };
        if spaces.is_empty() {
            return 0;
        }
        let Some(sky) = self.sky() else { return 0 };
        let own = std::process::id() as i32;
        let ours: BTreeSet<u64> = spaces.keys().copied().collect();
        let (desktops, shown) = Self::desktops(sky);
        let mut out = 0;
        for w in list.iter().filter(|w| w.pid != own && !w.onscreen && w.layer == 0 && !held.contains(&w.id)) {
            let now = sky.spaces_of(&[w.id]);
            let Some(space) = now.iter().find(|s| ours.contains(s)) else { continue };
            let home = spaces.get(space).copied().filter(|d| desktops.contains(d)).or(shown.first().copied());
            let Some(home) = home else { continue };
            let _ = sky.add_remove_from(Route::Bridged, home, &[w.id], ALL_SPACES_MASK);
            let (ok, after) = self.read_back(sky, w.id, |s| out_of(s, &ours));
            tracing::info!(id = w.id, pid = w.pid, space, desktop = home, ok, listed = ?after, "spaces engine: a window nothing answered for was in a stage's Space: let out");
            out += usize::from(ok);
        }
        out
    }

    /// The Desktop a just-opened app's window shows on: the one of the screen with the
    /// keyboard, else the first screen's.
    fn desktop_for_new(sky: &Sky) -> Option<u64> {
        let (_, shown) = Self::desktops(sky);
        sky.active_space().filter(|s| shown.contains(s)).or(shown.first().copied())
    }

    /// The Space where a just-opened app's new windows wait, made now when there is none (a
    /// launch's watch makes it at the launch notice, ahead of the first window).
    pub(crate) fn prepare_new(&self) -> Option<u64> {
        let sky = self.sky()?;
        let home = Self::desktop_for_new(sky)?;
        self.stage_space(sky, home, NEW_STAGE)
    }

    /// Window `id` of a just-opened app, listed by the window server on no Space and off
    /// screen (created, not ordered in yet), into the Space where new windows wait, before
    /// its app orders it in: it never shows over the stage shown until it is placed
    /// ([`Self::let_new`], or a hide into its stage's Space). True when the window server
    /// lists it there.
    pub(crate) fn hold_new(&self, id: u32) -> bool {
        let Some(sky) = self.sky() else { return false };
        let Some(home) = Self::desktop_for_new(sky) else { return false };
        let Some(space) = self.stage_space(sky, home, NEW_STAGE) else { return false };
        if let Err(e) = sky.add_remove_from(Route::Bridged, space, &[id], ALL_SPACES_MASK) {
            tracing::debug!(id, space, "spaces engine: a new window's hold could not be sent: {e}");
        }
        let (ok, after) = self.read_back(sky, id, |s| hidden_in(s, space));
        let mut st = self.state();
        if ok {
            st.held.insert(id, Held { space, home });
            st.new.insert(id);
            st.counts.new_held += 1;
        } else {
            drop(st);
            tracing::debug!(id, space, listed = ?after, "spaces engine: a new window did not go where new windows wait");
        }
        ok
    }

    /// True when window `id` waits where new windows wait ([`Self::hold_new`]).
    pub(crate) fn waits_new(&self, id: u32) -> bool {
        self.state().new.contains(&id)
    }

    /// A launch's window seen on screen before it could be held: counted.
    pub(crate) fn missed_new(&self) {
        self.state().counts.new_missed += 1;
    }

    /// Window `id`, waiting where new windows wait, onto the Desktop shown now (where its
    /// app ordered it in), where it shows; `placed`: the controller placed it (its stage in
    /// front), else it goes unplaced (not placed in time, never shown by its app, or not
    /// one to place). True when it came out (also when it was not waiting).
    pub(crate) fn let_new(&self, id: u32, placed: bool, why: &str) -> bool {
        let home = self.sky().and_then(Self::desktop_for_new);
        {
            let mut st = self.state();
            if !st.new.remove(&id) {
                return true;
            }
            if let (Some(home), Some(h)) = (home, st.held.get_mut(&id)) {
                h.home = home;
            }
            if placed {
                st.counts.new_placed += 1;
            } else {
                st.counts.new_unplaced += 1;
            }
        }
        let ok = self.show_window(id);
        tracing::debug!(id, placed, ok, why, "spaces engine: a new window let out where it shows");
        ok
    }

    /// The engine's state for `ctl state`.
    pub fn status(&self) -> Value {
        let st = self.state();
        json!({
            "spaces": st.stages.iter().map(|((desktop, stage), space)| json!({ "desktop": desktop, "stage": stage, "space": space })).collect::<Vec<_>>(),
            "held": st.held.iter().map(|(id, h)| json!({ "id": id, "space": h.space, "desktop": h.home })).collect::<Vec<_>>(),
            "owed": st.owed,
            "hides": st.counts.hides,
            "shows": st.counts.shows,
            "hide_misses": st.counts.hide_misses,
            "show_misses": st.counts.show_misses,
            "read_back_max_us": st.counts.read_back_max_us,
            "strikes": st.health.strikes(Instant::now()),
            "failed": st.health.failed(),
            "fail_mode": format!("{:?}", st.fail),
            "new_waiting": st.new,
            "new_held": st.counts.new_held,
            "new_placed": st.counts.new_placed,
            "new_unplaced": st.counts.new_unplaced,
            "new_missed": st.counts.new_missed,
            "lent_now": st.lent,
            "parked_lends": st.parked.keys().collect::<Vec<_>>(),
            "lent": st.counts.lent,
            "taken_back": st.counts.taken_back,
            "stretches": {
                "planned": st.stretch.planned,
                "shown": st.stretch.shown,
                "skipped": st.stretch.skipped,
                "landed": st.stretch.landed,
                "stopped": st.stretch.stopped,
            },
        })
    }
}

impl WindowEngine for SpacesEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Spaces
    }

    /// The window's own frame: a hide does not move it ([`Model::frame_left`]). Also notes
    /// the window's stage, which the hide that follows needs.
    fn hidden_at(&self, m: &Model, id: WindowId) -> Option<Rect> {
        let w = m.window(id)?;
        let stage = m.stage_of(id).or_else(|| m.group_of(id).first().and_then(|top| m.stage_of(WindowId(*top)))).map_or(NO_STAGE, |s| s.0);
        self.state().plans.insert(id.0, (stage, w.meta.frame));
        Some(w.meta.frame)
    }

    /// Into the stage's Space, read back. Nothing is asked of the app, so a busy app's
    /// window hides too. With a place of its own for this one hide (`at` is not the frame
    /// [`Self::hidden_at`] answered: a minimized window's dive), the window is also moved
    /// there first, through `window`: it is about to be brought out of the Space by macOS
    /// (an un-minimize shows the window on the Desktop) and must come out off the screen.
    fn hide(&self, id: WindowId, at: Rect, window: &AxElement) -> AxResult<()> {
        let own = self.state().plans.get(&id.0).map(|(_, frame)| *frame);
        if own.is_some_and(|own| own != at)
            && let Err(e) = window.set_position(at.origin())
        {
            tracing::debug!(id = id.0, "spaces engine: a hide's own place was not taken: {e}");
        }
        if self.hide_window(id.0) { Ok(()) } else { Err(AxError::FAILURE) }
    }

    /// Out of its stage's Space first (a window in a Space that is not shown is not asked
    /// to move), then the frame as the Corner engine sets it. A window that comes out of its
    /// Space already at the frame (the usual case: a hide does not move it) is asked
    /// nothing, so a busy app's window shows too; one never hidden only takes the frame.
    ///
    /// A window parked for a lend (round cmdtab-park: kept for the activation's switch) is
    /// never taken to be at its frame by where it is listed: the frame asked is set, and the
    /// park forgotten once it took (until then its move notices stay dropped).
    fn show(&self, id: WindowId, frame: Rect, window: &AxElement, how: ShowHow) -> AxResult<()> {
        let was_held = self.holds(id.0);
        let parked = self.state().parked.get(&id.0).map(|(p, _)| *p);
        if let Some(p) = parked
            && windows::describe(&[id.0]).first().is_some_and(|w| !p.display.parks(w.bounds))
        {
            tracing::info!(id = id.0, "spaces engine: a window parked for a lend was moved off the park by its app; shown at its frame");
        }
        let out = if parked.is_some() { self.show_window_only(id.0) } else { self.show_window(id.0) };
        if out && was_held && listed_at(id.0, frame) {
            self.state().parked.remove(&id.0);
            return Ok(());
        }
        let framed = match how {
            ShowHow::Once => window.set_frame(frame),
            ShowHow::Retrying => safety::set_frame_retrying(window, id.0, frame, leftovers::RESTORE_RETRY.budget),
        };
        if !out {
            return Err(AxError::FAILURE);
        }
        if framed.is_ok() && parked.is_some() {
            self.state().parked.remove(&id.0);
            tracing::debug!(id = id.0, "spaces engine: a window parked for a lend shown at its frame");
        }
        framed
    }

    /// Never by its frame: a hidden window is listed at its own.
    fn is_hidden(&self, _d: &Display, _listed: Rect) -> bool {
        false
    }

    /// By what the engine holds: the frame says nothing.
    fn hides_now(&self, id: WindowId, _d: &Display, _listed: Rect) -> bool {
        self.holds(id.0)
    }

    /// While the engine holds it and its app has a window on a Desktop not shown: a window
    /// in a stage Space is on no Desktop for macOS, which would follow the app to that
    /// other Desktop (S3c). An app with no window elsewhere takes macOS nowhere, so the
    /// keyboard goes to the new stage at once, as under the Corner engine (the full Spaces
    /// pass on 0eaa56b, 11-14: a card click on one Desktop waited for the unveil).
    fn focus_waits_for_show(&self, id: WindowId) -> bool {
        self.off_desktop(id.0) && self.app_elsewhere(id.0)
    }

    /// While the engine holds it in its stage Space (not put home for a while): a stage Space
    /// is no Desktop.
    fn off_every_desktop(&self, id: WindowId) -> bool {
        self.off_desktop(id.0)
    }

    fn lend_home(&self, id: WindowId, park: Option<LendPark>) -> Lend {
        self.lend_window(id.0, park)
    }

    fn take_back(&self, id: WindowId) -> Back {
        self.take_back_window(id.0)
    }

    fn parked_lend(&self, id: WindowId) -> bool {
        self.state().parked.contains_key(&id.0)
    }

    /// Never: no frame says a window is in a stage Space, and the Spaces end with the app.
    fn stranded(&self, frame: Rect, displays: &ScanDisplays, known: Known) -> bool {
        EngineKind::Spaces.stranded(frame, displays, known)
    }

    /// The app's stage Spaces as its file names them: swept with this engine's own. They
    /// end with the app, so after a crash or a kill they are gone and the sweep finds
    /// nothing; while the app still holds them (it hangs) the windows come out of them all
    /// the same (another process can move windows out of a Space, not end it).
    fn assume(&self, file: &ParkedFile) {
        let mut st = self.state();
        st.foreign = file.spaces.iter().map(|s| (s.space, s.desktop)).collect();
    }

    /// Pictures, as the Corner engine: the windows hide and show in one step behind them.
    fn animate(&self, _t: &Transition) -> Motion {
        Motion::Pictures
    }

    /// True: an incoming window can fly the last stretch itself in a Space of its own
    /// ([`landing`]).
    fn lands_live(&self) -> bool {
        true
    }

    fn fly_stretch(&self, s: Stretch, done: StretchDone) -> bool {
        self.begin_stretch(s, done)
    }

    fn cancel_stretch(&self, id: u32) {
        self.cancel(id);
    }

    fn stretching(&self) -> HashSet<u32> {
        self.stretches().iter().filter(|(_, f)| !f.stop).map(|(id, _)| *id).collect()
    }

    /// True: a hide is one window-server call (~2 ms a window), so the outgoing windows wait
    /// on screen for pictures taken at the switch; a window in a stage Space still draws and
    /// is captured with its current content there (`probe-spaces capture`, 11-14).
    fn pictures_at_switch(&self) -> bool {
        true
    }

    /// Every window out of the stage Spaces by the window server's own list, then the
    /// registry's frames (a fitted window's frame from before, a show that did not take its
    /// frame).
    fn bring_all_back(&self, why: &str) -> bool {
        let report = self.sweep(why, true);
        safety::emergency_unpark(why) && report.failed == 0
    }

    /// The sweep (a few window-server calls, no app asked), then the registry's short one.
    fn bring_back_briefly(&self) -> Option<Report> {
        let swept = self.sweep("abort", false);
        let registry = safety::unpark_registry(Duration::from_millis(100), Duration::ZERO)?;
        Some(Report { checked: swept.checked + registry.checked, restored: swept.restored + registry.restored, failed: swept.failed + registry.failed })
    }

    /// The window server's word: whatever is still in a stage Space is moved out; `None`
    /// when nothing is left in one (the Spaces are ended then).
    fn bring_back_rest(&self, why: &str, ours: Ours, _moved: bool) -> Option<Ours> {
        let report = self.sweep(why, true);
        if report.failed == 0 {
            return None;
        }
        let left: Vec<u32> = self.owed().into_iter().collect();
        tracing::error!(why, windows = ?left, "spaces engine: windows still in a stage's Space after the bring-back");
        Some(ours.kept_for_retry(&left))
    }

    /// The sweep: a window left in a stage Space with nothing answering for it (at a launch
    /// there is none: an earlier run's Spaces ended with it). Then, when an earlier copy
    /// left windows registered (`scope.registered`: it crashed with a parked file, likely a
    /// Corner engine's) or the launch's scan asks again for apps that did not answer, the
    /// Corner engine's scan: such a copy's windows wait in its park corner, and a Spaces run
    /// must bring them back the Corner way (this engine leaves nothing there itself, and the
    /// windows it registers are skipped).
    fn recover(&self, why: &str, scope: &ScanScope) -> (Report, Vec<i32>) {
        let swept = self.sweep_where(why, false, true);
        if scope.registered.is_empty() && !why.starts_with("launch") {
            return (swept, Vec::new());
        }
        let (corner, unanswered) = safety::recovery_scan_with(why, scope);
        (Report { checked: swept.checked + corner.checked, restored: swept.restored + corner.restored, failed: swept.failed + corner.failed }, unanswered)
    }

    fn present_off_screen(&self) -> HashSet<u32> {
        self.present_now()
    }

    fn health(&self) -> Option<String> {
        self.state().health.failed().map(str::to_string)
    }

    /// This engine's stage Spaces with their Desktops.
    fn parked_spaces(&self) -> Vec<StageSpace> {
        self.state().stages.iter().map(|((desktop, _), space)| StageSpace { space: *space, desktop: *desktop }).collect()
    }

    /// The launch's watch: the app's new windows held before they show
    /// ([`super::launch_watch`]).
    fn app_launched(&self, pid: i32) {
        super::launch_watch::start(shared(), pid);
    }

    fn about_to_show(&self, id: WindowId) -> bool {
        self.waits_new(id.0)
    }

    fn let_new_out(&self, id: WindowId, placed: bool) {
        let _ = self.let_new(id.0, placed, if placed { "placed" } else { "not placed now" });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_is_honoured_only_in_a_fixture_run() {
        assert_eq!(kind_from(Some("dev.fixture"), Some("spaces")), Some(EngineKind::Spaces));
        assert_eq!(kind_from(Some("dev.fixture"), Some(" corner ")), Some(EngineKind::Corner));
        assert_eq!(kind_from(None, Some("spaces")), None, "never for a user's run");
        assert_eq!(kind_from(Some("  "), Some("spaces")), None);
        assert_eq!(kind_from(Some("dev.fixture"), None), None);
        assert_eq!(kind_from(Some("dev.fixture"), Some("other")), None);
    }

    #[test]
    fn a_window_is_hidden_only_alone_in_its_stages_space() {
        assert!(hidden_in(&[836], 836));
        assert!(!hidden_in(&[836, 3], 836), "in the Space and on a Desktop: not hidden");
        assert!(!hidden_in(&[3], 836));
        assert!(!hidden_in(&[], 836));
    }

    #[test]
    fn a_window_is_out_when_no_stage_space_lists_it() {
        let ours: BTreeSet<u64> = [836, 840].into();
        assert!(out_of(&[3], &ours));
        assert!(out_of(&[], &ours), "on no Space at all (minimized, closed): not in ours");
        assert!(!out_of(&[840], &ours));
        assert!(!out_of(&[3, 836], &ours), "on its Desktop and still in a stage Space: not out");
    }

    #[test]
    fn a_windows_desktop_is_the_one_it_is_on() {
        let desktops: BTreeSet<u64> = [1, 3].into();
        assert_eq!(desktop_among(&[3], &desktops, &[1]), Some(3), "its own, shown or not");
        assert_eq!(desktop_among(&[1, 3], &desktops, &[3]), Some(3), "on every Desktop: the one that shows");
        assert_eq!(desktop_among(&[836], &desktops, &[3]), None, "already in a stage Space: its Desktop is the one kept");
        assert_eq!(desktop_among(&[], &desktops, &[3]), None);
    }

    #[test]
    fn an_app_is_elsewhere_only_with_a_window_on_a_desktop_not_shown() {
        let desktops: BTreeSet<u64> = [1, 3, 5].into();
        assert!(elsewhere(&[1, 3], &desktops, &[3]), "a window on Desktop 1 while 3 shows");
        assert!(!elsewhere(&[3], &desktops, &[3]), "all on the Desktop shown");
        assert!(!elsewhere(&[836, 3], &desktops, &[3]), "a stage Space is no Desktop");
        assert!(!elsewhere(&[836], &desktops, &[3]), "every other window hidden: macOS goes nowhere");
        assert!(!elsewhere(&[1, 3], &desktops, &[1, 3]), "two screens, both shown");
        assert!(!elsewhere(&[], &desktops, &[3]));
    }

    #[test]
    fn an_engine_that_hid_nothing_shows_and_sweeps_without_the_window_server() {
        let e = SpacesEngine { sky: OnceLock::new(), state: Mutex::new(State::default()), stretches: Mutex::new(BTreeMap::new()) };
        assert!(e.show_window(42), "never hidden: nothing to bring out");
        assert_eq!(e.sweep("test", true), Report::default());
        assert!(e.present_off_screen().is_empty());
        assert!(e.owed().is_empty() && e.held().is_empty());
        assert!(e.sky.get().is_none(), "SkyLight is not opened until a window hides");
    }
}
