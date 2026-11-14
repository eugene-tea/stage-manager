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
//! Chosen only by a test switch for now ([`from_env`]); the Corner engine is the default.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::ffi::{ALL_SPACES_MASK, Route, Sky};
use crate::engine::geometry::Rect;
use crate::engine::leftovers;
use crate::engine::model::{Display, Model, WindowId};
use crate::engine::recovery::{Known, ParkedFile, ScanDisplays};
use crate::platform::ax::{AxElement, AxError, AxResult};
use crate::platform::safety::{self, Ours, Report, ScanScope};
use crate::platform::window_engine::{EngineKind, Motion, ShowHow, WindowEngine};
use crate::platform::windows;
use crate::ui::strip_model::Transition;

/// How long a hide or a show waits for the window server to list the window where the call
/// put it (the spike: within a millisecond or two).
const READ_BACK: Duration = Duration::from_millis(60);

/// The pause between two reads of a window's Spaces while waiting ([`READ_BACK`]).
const READ_BACK_STEP: Duration = Duration::from_micros(300);

/// The stage key of a window the model names no stage for (a child read before its parent).
const NO_STAGE: u64 = 0;

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
    counts: Counts,
}

/// The Spaces engine (see the module docs).
pub struct SpacesEngine {
    sky: OnceLock<Option<Sky>>,
    state: Mutex<State>,
}

/// The one Spaces engine of this process: the stage Spaces belong to the process's
/// window-server connection, so there is one map of them.
pub fn shared() -> Arc<SpacesEngine> {
    static ENGINE: OnceLock<Arc<SpacesEngine>> = OnceLock::new();
    ENGINE.get_or_init(|| Arc::new(SpacesEngine { sky: OnceLock::new(), state: Mutex::new(State::default()) })).clone()
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

impl SpacesEngine {
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

    /// Every stage Space this engine made.
    fn ours(&self) -> BTreeSet<u64> {
        self.state().stages.values().copied().collect()
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
                None
            }
            Err(e) => {
                tracing::warn!(desktop = home, stage, "spaces engine: no Space made: {e}");
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

    /// Window `id` into its stage's Space; true when the window server lists it there.
    pub(crate) fn hide_window(&self, id: u32) -> bool {
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
            self.state().held.insert(id, Held { space, home });
            return true;
        }
        // Off its Desktop; a window already in another stage's Space (moved between two
        // hidden stages) out of that one too.
        let ours = self.ours();
        let sent = if out_of(&now, &ours) { sky.add_remove(Route::Bridged, space, &[id]) } else { sky.add_remove_from(Route::Bridged, space, &[id], ALL_SPACES_MASK) };
        if let Err(e) = sent {
            tracing::warn!(id, space, "spaces engine: the hide could not be sent: {e}");
        }
        let (ok, after) = self.read_back(sky, id, |s| hidden_in(s, space));
        let mut st = self.state();
        if ok {
            st.held.insert(id, Held { space, home });
            st.owed.remove(&id);
        } else {
            st.counts.hide_misses += 1;
            drop(st);
            tracing::warn!(id, space, desktop = home, listed = ?after, "spaces engine: a hidden window is not in its stage's Space");
        }
        ok
    }

    /// Window `id` out of every stage Space, onto its Desktop; true when the window server
    /// lists it in none of them (also when it was in none).
    pub(crate) fn show_window(&self, id: u32) -> bool {
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
        if let Err(e) = sky.add_remove_from(Route::Bridged, home, &[id], ALL_SPACES_MASK) {
            tracing::warn!(id, desktop = home, "spaces engine: the show could not be sent: {e}");
        }
        let (ok, after) = self.read_back(sky, id, |s| out_of(s, &ours));
        if ok {
            let mut st = self.state();
            st.held.remove(&id);
            st.owed.remove(&id);
        } else {
            tracing::warn!(id, desktop = home, listed = ?after, "spaces engine: a shown window is still in a stage's Space");
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
            let used: BTreeSet<u64> = st.held.values().map(|h| h.space).collect();
            st.stages.iter().filter(|((_, stage), space)| !used.contains(space) && !stage_lives(*stage)).map(|(k, s)| (*k, *s)).collect()
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
        let mut report = Report::default();
        let ours = self.ours();
        if ours.is_empty() {
            return report;
        }
        let Some(sky) = self.sky() else { return report };
        let own = std::process::id() as i32;
        let (desktops, shown) = Self::desktops(sky);
        let mut by_home: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
        for w in windows::every_window().iter().filter(|w| w.pid != own) {
            if out_of(&sky.spaces_of(&[w.id]), &ours) {
                continue;
            }
            report.checked += 1;
            let home = self.state().held.get(&w.id).map(|h| h.home).filter(|h| desktops.contains(h)).or(shown.first().copied());
            match home {
                Some(home) => by_home.entry(home).or_default().push(w.id),
                None => report.failed += 1,
            }
        }
        for (home, ids) in &by_home {
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
            } else {
                report.failed += 1;
                st.owed.insert(id);
                left.push(id);
            }
        }
        if report.checked > 0 || end {
            tracing::info!(why, hidden = report.checked, back = report.restored, left = ?left, "spaces engine: hidden windows brought back");
        }
        if end && left.is_empty() {
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
    /// window stays in a stage Space with nothing answering for it. How many came out.
    pub fn let_strangers_out(&self) -> usize {
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
        for w in windows::every_window().iter().filter(|w| w.pid != own && !w.onscreen && w.layer == 0 && !held.contains(&w.id)) {
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
    fn show(&self, id: WindowId, frame: Rect, window: &AxElement, how: ShowHow) -> AxResult<()> {
        let was_held = self.holds(id.0);
        let out = self.show_window(id.0);
        if out && was_held && listed_at(id.0, frame) {
            return Ok(());
        }
        let framed = match how {
            ShowHow::Once => window.set_frame(frame),
            ShowHow::Retrying => safety::set_frame_retrying(window, id.0, frame, leftovers::RESTORE_RETRY.budget),
        };
        if !out {
            return Err(AxError::FAILURE);
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

    /// Never: no frame says a window is in a stage Space, and the Spaces end with the app.
    fn stranded(&self, frame: Rect, displays: &ScanDisplays, known: Known) -> bool {
        EngineKind::Spaces.stranded(frame, displays, known)
    }

    /// Nothing: the app's stage Spaces ended with it, there is no place to know.
    fn assume(&self, _file: &ParkedFile) {}

    /// Pictures, as the Corner engine: the windows hide and show in one step behind them.
    fn animate(&self, _t: &Transition) -> Motion {
        Motion::Pictures
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

    /// The sweep: a window left in a stage Space with nothing answering for it. At a launch
    /// there is none (an earlier run's Spaces ended with it).
    fn recover(&self, why: &str, _scope: &ScanScope) -> (Report, Vec<i32>) {
        (self.sweep(why, false), Vec::new())
    }

    fn present_off_screen(&self) -> HashSet<u32> {
        self.present_now()
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
    fn an_engine_that_hid_nothing_shows_and_sweeps_without_the_window_server() {
        let e = SpacesEngine { sky: OnceLock::new(), state: Mutex::new(State::default()) };
        assert!(e.show_window(42), "never hidden: nothing to bring out");
        assert_eq!(e.sweep("test", true), Report::default());
        assert!(e.present_off_screen().is_empty());
        assert!(e.owed().is_empty() && e.held().is_empty());
        assert!(e.sky.get().is_none(), "SkyLight is not opened until a window hides");
    }
}
