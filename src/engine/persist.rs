//! Persistence: `state.json` and re-matching saved windows to live ones on launch.
//!
//! The file keeps stages **per Space** ([`SavedSpace`], keyed by the model's synthetic
//! [`SpaceId`]). A saved window is found again by its window id first (ids outlive our
//! app, not the window's own app), then by exact bundle ID + title, then by bundle ID only in
//! saved order. Each
//! live window is used at most once; live windows that match nothing are grouped the way
//! Apple's Stage Manager does when it is turned on: one stage per app (see
//! [`Model::apply_launch`]). A Space is recognized by its windows: the saved Space whose
//! windows match the ones on screen (window id, or exact bundle ID + title) is the one shown
//! ([`Model::arrive`]); other saved Spaces wait, untouched, for their first visit.
//!
//! Version 1 files (one Space) are read as a single Space.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::geometry::Rect;
use super::model::{
    Action, Home, Model, Observation, Pid, SpaceId, StageId, WindowId, WindowMeta, WindowRole,
    WindowState,
};
use super::rules::Decision;
use super::settings::AppWindows;
use super::stage_area::same_frame;

/// The state file format version this build writes (it also reads version 1).
pub const STATE_VERSION: u32 = 2;

/// A saved window's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedRole {
    /// A standard window: matched on launch.
    Standard,
    /// A child window: saved for completeness, never matched (children follow their parent).
    Child,
}

/// One saved window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    /// The app's bundle ID.
    pub bundle_id: String,
    /// The title when saved.
    pub title: String,
    /// The restore frame.
    pub frame: Rect,
    /// Position in the stage, 0 = front.
    pub z: usize,
    /// True for the stage's key window.
    pub key: bool,
    /// True for a pinned window (saved with the active stage, restored pinned).
    pub pinned: bool,
    /// Standard or child.
    pub role: SavedRole,
    /// The frame before we fitted the window clear of the strip, when we did (quit and the
    /// recovery scan put it back there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<Rect>,
    /// The window's `CGWindowID` when saved. Window ids outlive our app (the windows belong to
    /// other apps), so a restart finds the very window by it whatever its title is now; a
    /// relaunched app's windows get new ids and are matched by bundle ID and title instead.
    /// Absent in files written before 10-13.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
}

impl SavedWindow {
    /// True when `live` is this very window: the same `CGWindowID` and the same app (an id
    /// reused after a reboot by another app's window never matches).
    pub fn is_window(&self, live: &WindowMeta) -> bool {
        self.window == Some(live.id.0) && !self.bundle_id.is_empty() && self.bundle_id == live.bundle_id
    }

    /// True when `live` has this window's app and title.
    pub fn same_title(&self, live: &WindowMeta) -> bool {
        self.bundle_id == live.bundle_id && self.title == live.title
    }
}

/// One saved stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedStage {
    /// The stage's name, if any.
    pub name: Option<String>,
    /// Its windows, front to back.
    pub windows: Vec<SavedWindow>,
}

/// One saved Space: its stages, active stage and strip, like a whole Stage Manager.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedSpace {
    /// The model's synthetic Space id (never Apple's).
    pub id: u64,
    /// The Space shown when saved.
    #[serde(default)]
    pub current: bool,
    /// Stages in creation order (hotkey numbers).
    pub stages: Vec<SavedStage>,
    /// Index into `stages` of the Space's active stage.
    pub active_index: Option<usize>,
    /// Indices into `stages` of the strip slots, top to bottom.
    #[serde(default)]
    pub strip_slots: Vec<usize>,
}

impl SavedSpace {
    /// How many standard windows it saved (children follow their parents and are never
    /// matched).
    pub fn standard_windows(&self) -> usize {
        self.stages.iter().flat_map(|s| s.windows.iter()).filter(|w| w.role == SavedRole::Standard).count()
    }
}

/// The contents of `state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateFile {
    /// Always [`STATE_VERSION`] when written.
    pub version: u32,
    /// The window server session the saved window ids belong to (the boot and the login
    /// session, as the platform reads them). Window ids start over after a reboot or a new
    /// login, so ids saved in another session name other windows: [`StateFile::for_session`]
    /// drops them at load. Absent in files written before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Every Space with stages, the shown one flagged `current`.
    pub spaces: Vec<SavedSpace>,
}

/// Version 1: one Space, no ids.
#[derive(Deserialize)]
struct StateFileV1 {
    stages: Vec<SavedStage>,
    active_index: Option<usize>,
    #[serde(default)]
    strip_slots: Vec<usize>,
}

impl StateFile {
    /// A file holding one Space.
    pub fn single(space: SavedSpace) -> Self {
        Self {
            version: STATE_VERSION,
            session: None,
            spaces: vec![space],
        }
    }

    /// The file as read in window server session `current`: saved window ids are kept only
    /// when the file was written in that very session; otherwise (a reboot or a new login
    /// since, a file that does not say, a session that cannot be read now) every saved
    /// window id is dropped, so a reused id never names another window and matching falls
    /// back to bundle ID and title. Returns how many ids were dropped too.
    pub fn for_session(mut self, current: Option<&str>) -> (Self, usize) {
        let same = current.is_some() && self.session.as_deref() == current;
        let mut dropped = 0;
        if !same {
            for w in self.spaces.iter_mut().flat_map(|s| s.stages.iter_mut()).flat_map(|s| s.windows.iter_mut()) {
                if w.window.take().is_some() {
                    dropped += 1;
                }
            }
        }
        (self, dropped)
    }

    /// Every saved stage of every Space.
    pub fn all_stages(&self) -> impl Iterator<Item = &SavedStage> {
        self.spaces.iter().flat_map(|s| s.stages.iter())
    }

    /// Pretty JSON for writing to disk, or why it cannot be written: a frame that is not
    /// finite (JSON has no NaN or infinity; `serde_json` writes `null`, which no load reads
    /// back, so the whole file would be lost), or any encoding error. Never a stand-in such
    /// as `{}`: the caller keeps the file it has.
    pub fn to_json(&self) -> Result<String, String> {
        if let Some(w) = self.all_stages().flat_map(|s| s.windows.iter()).find(|w| !w.frame.is_finite() || w.original.is_some_and(|o| !o.is_finite())) {
            return Err(format!(
                "state.json: a frame of window {} of {} is not finite; not written",
                w.window.map_or_else(|| "?".to_string(), |id| id.to_string()),
                if w.bundle_id.is_empty() { "an unknown app" } else { &w.bundle_id }
            ));
        }
        serde_json::to_string_pretty(self).map_err(|e| format!("state.json: {e}; not written"))
    }

    /// Parses JSON: version 2, or version 1 read as one current Space; other versions are
    /// rejected.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("state.json: {e}"))?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(1) => {
                let v1: StateFileV1 =
                    serde_json::from_value(value).map_err(|e| format!("state.json: {e}"))?;
                Ok(Self::single(SavedSpace {
                    id: 1,
                    current: true,
                    stages: v1.stages,
                    active_index: v1.active_index,
                    strip_slots: v1.strip_slots,
                }))
            }
            Some(v) if v == u64::from(STATE_VERSION) => {
                serde_json::from_value(value).map_err(|e| format!("state.json: {e}"))
            }
            Some(v) => Err(format!(
                "state.json: version {v} is not supported (expected {STATE_VERSION})"
            )),
            None => Err("state.json: no version".to_string()),
        }
    }
}

/// One live window placed into a saved stage.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignedWindow {
    /// The live window.
    pub id: WindowId,
    /// The saved restore frame.
    pub frame: Rect,
    /// It was the stage's key window.
    pub key: bool,
    /// It was pinned.
    pub pinned: bool,
    /// Its saved pre-fit frame.
    pub original: Option<Rect>,
}

/// A saved stage with the live windows matched into it, front to back.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignedStage {
    /// The saved name.
    pub name: Option<String>,
    /// Matched windows, front to back (pinned ones included, flagged).
    pub windows: Vec<AssignedWindow>,
}

/// The result of [`rematch`]: saved stages (same order and indices as the state file) filled
/// with live windows.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// One entry per saved stage, in saved order.
    pub stages: Vec<AssignedStage>,
    /// The saved active stage index.
    pub active_index: Option<usize>,
    /// The saved strip slots (indices into `stages`).
    pub strip_slots: Vec<usize>,
    /// Live standard windows that matched nothing; they join the active stage.
    pub unmatched: Vec<WindowId>,
}

/// What the platform knows at launch about recency, for windows no saved stage claims.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LaunchHints {
    /// Live windows front to back (the window list's z-order).
    pub z_order: Vec<WindowId>,
    /// The frontmost app, if known.
    pub frontmost: Option<Pid>,
}

/// How many live standard windows match a saved standard window of `space` exactly (the
/// same window id, else the same bundle ID and title; each window on either side used once):
/// the evidence that `space` is on screen.
pub fn exact_matches(space: &SavedSpace, live: &[WindowMeta]) -> usize {
    let saved: Vec<&SavedWindow> = space
        .stages
        .iter()
        .flat_map(|s| s.windows.iter())
        .filter(|w| w.role == SavedRole::Standard)
        .collect();
    let standard: Vec<&WindowMeta> = live.iter().filter(|l| l.role == WindowRole::Standard).collect();
    let mut used: HashSet<WindowId> = HashSet::new();
    let mut matched = vec![false; saved.len()];
    let passes: [fn(&SavedWindow, &WindowMeta) -> bool; 2] = [SavedWindow::is_window, SavedWindow::same_title];
    for pass in passes {
        for (done, sw) in matched.iter_mut().zip(&saved) {
            if *done {
                continue;
            }
            if let Some(l) = standard.iter().find(|l| !used.contains(&l.id) && pass(sw, l)) {
                used.insert(l.id);
                *done = true;
            }
        }
    }
    matched.iter().filter(|m| **m).count()
}

/// How many live standard windows match a saved standard window of `space` by bundle ID
/// alone (each saved window used once): weak evidence, for the launch fallback.
pub fn bundle_matches(space: &SavedSpace, live: &[WindowMeta]) -> usize {
    let mut used: HashSet<WindowId> = HashSet::new();
    let mut n = 0;
    for sw in space
        .stages
        .iter()
        .flat_map(|s| s.windows.iter())
        .filter(|w| w.role == SavedRole::Standard)
    {
        if let Some(l) = live.iter().find(|l| {
            l.role == WindowRole::Standard && !used.contains(&l.id) && !l.bundle_id.is_empty() && l.bundle_id == sw.bundle_id
        }) {
            used.insert(l.id);
            n += 1;
        }
    }
    n
}

/// Places live standard windows into one saved Space's stages: the same window id first,
/// then exact bundle + title, then bundle only, each in saved order (stage order, then front to back). Each live window
/// is used at most once. Child windows on either side are left out; they follow their
/// parents.
pub fn rematch(state: &SavedSpace, live: &[WindowMeta]) -> Assignment {
    let standard: Vec<&WindowMeta> = live
        .iter()
        .filter(|m| m.role == WindowRole::Standard)
        .collect();
    let mut saved: Vec<(usize, &SavedWindow)> = Vec::new();
    for (i, st) in state.stages.iter().enumerate() {
        let mut ws: Vec<&SavedWindow> = st
            .windows
            .iter()
            .filter(|w| w.role == SavedRole::Standard)
            .collect();
        ws.sort_by_key(|w| w.z);
        saved.extend(ws.into_iter().map(|w| (i, w)));
    }
    let mut used: HashSet<WindowId> = HashSet::new();
    let mut matched: Vec<Option<WindowId>> = vec![None; saved.len()];
    let passes: [fn(&SavedWindow, &WindowMeta) -> bool; 3] = [
        SavedWindow::is_window,
        SavedWindow::same_title,
        |s, l| s.bundle_id == l.bundle_id,
    ];
    for pass in passes {
        for (slot, (_, sw)) in matched.iter_mut().zip(&saved) {
            if slot.is_some() {
                continue;
            }
            if let Some(l) = standard
                .iter()
                .find(|l| !used.contains(&l.id) && pass(sw, l))
            {
                used.insert(l.id);
                *slot = Some(l.id);
            }
        }
    }
    let mut stages: Vec<AssignedStage> = state
        .stages
        .iter()
        .map(|s| AssignedStage {
            name: s.name.clone(),
            windows: Vec::new(),
        })
        .collect();
    for ((stage, sw), id) in saved.iter().zip(&matched) {
        if let Some(id) = id {
            stages[*stage].windows.push(AssignedWindow {
                id: *id,
                frame: sw.frame,
                key: sw.key,
                pinned: sw.pinned,
                original: sw.original,
            });
        }
    }
    let active_index = state.active_index.filter(|&i| i < stages.len());
    Assignment {
        strip_slots: state
            .strip_slots
            .iter()
            .copied()
            .filter(|&i| i < stages.len())
            .collect(),
        stages,
        active_index,
        unmatched: standard
            .iter()
            .filter(|l| !used.contains(&l.id))
            .map(|l| l.id)
            .collect(),
    }
}

impl Model {
    /// Snapshot for `state.json`: every Space with windows, the shown one flagged `current`,
    /// plus the saved Spaces not visited yet this run (untouched). Pinned windows are saved
    /// with their Space's active stage, flagged `pinned`; children are saved with their stage
    /// after its members. The window server session is the platform's to stamp
    /// ([`StateFile::session`]).
    pub fn to_state(&self) -> StateFile {
        let save = |id: WindowId, z: usize, key: bool, pinned: bool| -> Option<SavedWindow> {
            let w = self.windows.get(&id)?;
            Some(SavedWindow {
                bundle_id: w.meta.bundle_id.clone(),
                title: w.meta.title.clone(),
                frame: w.restore,
                z,
                key,
                pinned,
                role: match w.meta.role {
                    WindowRole::Standard => SavedRole::Standard,
                    WindowRole::Child { .. } => SavedRole::Child,
                },
                original: w.original,
                window: Some(id.0),
            })
        };
        let mut spaces = Vec::new();
        for space in &self.spaces {
            if self.space_is_empty(space.id) {
                continue;
            }
            let here: Vec<&crate::engine::model::Stage> = self.stages_in(space.id).collect();
            let active = self.active_stage_in(space.id);
            let active_index = here.iter().position(|s| Some(s.id) == active);
            let mut stages: Vec<SavedStage> = here
                .iter()
                .map(|s| {
                    let mut windows: Vec<SavedWindow> = s
                        .windows
                        .iter()
                        .enumerate()
                        .filter_map(|(z, &id)| save(id, z, s.key == Some(id), false))
                        .collect();
                    let base = windows.len();
                    windows.extend(
                        self.children_in(Home::Stage(s.id))
                            .into_iter()
                            .enumerate()
                            .filter_map(|(i, id)| save(id, base + i, false, false)),
                    );
                    SavedStage {
                        name: s.name.clone(),
                        windows,
                    }
                })
                .collect();
            if let Some(i) = active_index {
                let mut pinned = self.pinned();
                pinned.extend(self.children_in(Home::Pinned));
                pinned.retain(|id| self.windows.get(id).is_some_and(|w| w.space == space.id));
                let base = stages[i].windows.len();
                stages[i].windows.extend(
                    pinned
                        .into_iter()
                        .enumerate()
                        .filter_map(|(k, id)| save(id, base + k, false, true)),
                );
            }
            let strip_slots = self
                .slots
                .get(&space.id)
                .map(|slots| {
                    slots
                        .iter()
                        .filter_map(|id| here.iter().position(|s| s.id == *id))
                        .collect()
                })
                .unwrap_or_default();
            spaces.push(SavedSpace {
                id: space.id.0,
                current: self.current.values().any(|&c| c == space.id),
                stages,
                active_index,
                strip_slots,
            });
        }
        spaces.extend(self.leftovers.iter().cloned().map(|mut l| {
            l.current = false;
            l
        }));
        StateFile {
            version: STATE_VERSION,
            session: None,
            spaces,
        }
    }

    /// Rebuilds stages from an [`Assignment`] on launch, with no recency hints (see
    /// [`Model::apply_launch`]).
    pub fn apply_assignment(
        &mut self,
        live: &[WindowMeta],
        assignment: &Assignment,
        restore_frames: bool,
    ) -> Vec<Action> {
        self.apply_launch(live, assignment, restore_frames, &LaunchHints::default())
    }

    /// Rebuilds stages from an [`Assignment`] on launch, on the Space the main display shows.
    /// `live` lists every live window there (children too; they follow their parents). Saved
    /// stages with no matched window are dropped unless active or declared.
    ///
    /// Standard windows no saved stage claims (every window on a first start, without a
    /// `state.json`) are placed the way Apple's Stage Manager does when it is turned on: an
    /// app that already has a saved stage takes its extra windows into that stage (the active
    /// one if it holds the app); every other app gets a stage of its own holding all its
    /// windows ([`AppWindows::AllAtOnce`]) or one stage per window
    /// ([`AppWindows::OneAtATime`]). The frontmost app ([`LaunchHints::frontmost`], else the
    /// app of the frontmost window) comes first and takes an empty active stage; when no
    /// saved stage is active with windows, its stage becomes active. The others enter the
    /// strip most recently used first, by the window list's z-order.
    ///
    /// Windows of the Space's inactive stages are parked; with `restore_frames` the active
    /// stage's and pinned windows get their saved frames, else they keep their live frames (a
    /// live frame in a park zone always falls back to the saved one). Windows already known
    /// are skipped. Other Spaces are left alone.
    pub fn apply_launch(
        &mut self,
        live: &[WindowMeta],
        assignment: &Assignment,
        restore_frames: bool,
        hints: &LaunchHints,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let metas: HashMap<WindowId, &WindowMeta> = live.iter().map(|m| (m.id, m)).collect();
        let space = self.current_space(self.main_display().id);
        let mut built: Vec<Option<StageId>> = vec![None; assignment.stages.len()];
        let mut placed: HashSet<WindowId> = HashSet::new();

        for (i, st) in assignment.stages.iter().enumerate() {
            let members: Vec<&AssignedWindow> = st
                .windows
                .iter()
                .filter(|w| {
                    metas.contains_key(&w.id)
                        && !self.windows.contains_key(&w.id)
                        && placed.insert(w.id)
                })
                .collect();
            let declared = st.name.as_deref().is_some_and(|n| self.is_declared(n));
            let standing: Vec<&&AssignedWindow> = members.iter().filter(|w| !w.pinned).collect();
            if standing.is_empty() && Some(i) != assignment.active_index && !declared {
                for w in members.iter().filter(|w| w.pinned) {
                    self.insert_restored(metas[&w.id], w, Home::Pinned, restore_frames);
                }
                continue;
            }
            let sid = match st.name.as_deref().and_then(|n| self.find_named(space, n)) {
                Some(s) if !built.contains(&Some(s)) => s,
                _ => self.create_stage(space, st.name.clone()),
            };
            built[i] = Some(sid);
            for w in &members {
                let home = if w.pinned {
                    Home::Pinned
                } else {
                    Home::Stage(sid)
                };
                self.insert_restored(metas[&w.id], w, home, restore_frames);
            }
            let ids: Vec<WindowId> = standing.iter().map(|w| w.id).collect();
            let key = standing
                .iter()
                .find(|w| w.key)
                .map(|w| w.id)
                .or(ids.first().copied());
            let t = self.tick();
            if let Some(stage) = self.stage_mut(sid) {
                stage.windows.extend(ids);
                stage.key = key;
                stage.last_active = t;
            }
        }

        if let Some(sid) = assignment.active_index.and_then(|i| built[i]) {
            let previous = self.active.insert(space, sid);
            let t = self.tick();
            if let Some(stage) = self.stage_mut(sid) {
                stage.last_active = t;
            }
            if let Some(p) = previous.filter(|&p| p != sid) {
                self.drop_if_dead(p, &mut actions);
            }
        }

        self.place_unmatched(space, &metas, assignment, &placed, hints);

        // Every window of every stage clear of the strip, so restored frames already are.
        let ids: Vec<WindowId> = live.iter().map(|m| m.id).collect();
        let fitted: Vec<WindowId> = ids.into_iter().filter(|&id| self.fit_window(id)).collect();

        let stage_ids: Vec<StageId> = self.stages_in(space).map(|s| s.id).collect();
        for s in stage_ids {
            if self.is_active(s) {
                if restore_frames {
                    self.show_stage(s, &mut actions);
                } else {
                    self.unstick(s, &mut actions);
                }
            } else {
                self.park_stage(s, &mut actions);
            }
        }
        if restore_frames {
            for id in self.pinned() {
                if self.windows.get(&id).is_some_and(|w| w.space == space) {
                    self.restore_window(id, &mut actions);
                }
            }
        }
        for id in fitted {
            let shown = self.windows.get(&id).filter(|w| !w.parked).map(|w| w.restore);
            let done = actions
                .iter()
                .any(|a| matches!(a, Action::SetFrame { id: x, .. } if *x == id));
            if let Some(frame) = shown
                && !done
            {
                actions.push(Action::SetFrame { id, frame });
            }
        }

        for meta in live
            .iter()
            .filter(|m| matches!(m.role, WindowRole::Child { .. }))
        {
            let child_actions =
                self.add_window(meta.clone(), &Decision::default(), &Observation::default());
            actions.extend(
                child_actions
                    .into_iter()
                    .filter(|a| *a != Action::StateChanged),
            );
        }

        let slots: Vec<StageId> = assignment
            .strip_slots
            .iter()
            .filter_map(|&i| built.get(i).copied().flatten())
            .collect();
        if !slots.is_empty() || !self.slots.contains_key(&space) {
            self.slots.insert(space, slots);
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Places standard windows no saved stage claims (see [`Model::apply_launch`]).
    fn place_unmatched(
        &mut self,
        space: SpaceId,
        metas: &HashMap<WindowId, &WindowMeta>,
        assignment: &Assignment,
        placed: &HashSet<WindowId>,
        hints: &LaunchHints,
    ) {
        let z = |id: WindowId| {
            hints
                .z_order
                .iter()
                .position(|&x| x == id)
                .unwrap_or(usize::MAX)
        };
        let mut pending: Vec<&WindowMeta> = assignment
            .unmatched
            .iter()
            .filter_map(|id| metas.get(id).copied())
            .filter(|m| !self.windows.contains_key(&m.id) && !placed.contains(&m.id))
            .collect();
        pending.sort_by_key(|m| z(m.id));
        // Apps in recency order: the frontmost app first, then by their frontmost window.
        let mut apps: Vec<(Pid, Vec<&WindowMeta>)> = Vec::new();
        for m in pending {
            match apps.iter_mut().find(|(p, _)| *p == m.pid) {
                Some((_, ws)) => ws.push(m),
                None => apps.push((m.pid, vec![m])),
            }
        }
        if let Some(front) = hints.frontmost
            && let Some(i) = apps.iter().position(|(p, _)| *p == front)
        {
            let app = apps.remove(i);
            apps.insert(0, app);
        }
        let one_at_a_time = self.options.settings.app_windows == AppWindows::OneAtATime;
        let mut fresh: Vec<StageId> = Vec::new();
        for (pid, windows) in apps {
            let existing = self.app_stage(pid, space);
            let units: Vec<Vec<&WindowMeta>> = if existing.is_none() && one_at_a_time {
                windows.into_iter().map(|w| vec![w]).collect()
            } else {
                vec![windows]
            };
            for unit in units {
                let stage = match existing {
                    Some(s) => s,
                    None => {
                        let active = self.ensure_active(space);
                        let reusable = self.stage(active).is_some_and(|s| {
                            s.windows.is_empty() && s.name.is_none() && !fresh.contains(&active)
                        });
                        let s = if reusable {
                            active
                        } else {
                            self.create_stage(space, None)
                        };
                        fresh.push(s);
                        s
                    }
                };
                for meta in unit {
                    self.insert_unmatched(meta, stage, space);
                }
            }
        }
        // Recency: the first fresh stage is the most recent after the active one.
        for &s in fresh.iter().rev() {
            let t = self.tick();
            if let Some(st) = self.stage_mut(s) {
                st.last_active = t;
            }
        }
        let active = self.ensure_active(space);
        if self.stage_is_empty(active)
            && let Some(&first) = fresh.first()
            && first != active
        {
            self.active.insert(space, first);
            let mut ignored = Vec::new();
            self.drop_if_dead(active, &mut ignored);
        }
        let active = self.ensure_active(space);
        let t = self.tick();
        if let Some(st) = self.stage_mut(active) {
            st.last_active = t;
        }
    }

    /// The stage holding app `pid`'s windows on `space`: the active one if it does, else
    /// the most recently active.
    fn app_stage(&self, pid: Pid, space: SpaceId) -> Option<StageId> {
        let holds = |s: &&crate::engine::model::Stage| {
            s.space == space
                && s.windows
                    .iter()
                    .any(|w| self.windows.get(w).is_some_and(|w| w.meta.pid == pid))
        };
        let active = self.active_stage_in(space);
        self.stages
            .iter()
            .filter(holds)
            .max_by_key(|s| (Some(s.id) == active, s.last_active))
            .map(|s| s.id)
    }

    fn insert_unmatched(&mut self, meta: &WindowMeta, stage: StageId, space: SpaceId) {
        let display = self.known_display(meta.display);
        let mut meta = meta.clone();
        meta.display = display;
        let id = meta.id;
        let restore = self.safe_restore(meta.frame, display);
        self.windows.insert(
            id,
            WindowState {
                meta,
                home: Home::Stage(stage),
                restore,
                parked: false,
                original: None,
                space,
                sticky: false,
            },
        );
        if let Some(s) = self.stage_mut(stage) {
            s.windows.push(id);
            if s.key.is_none() {
                s.key = Some(id);
            }
        }
    }

    /// Restores members of an active stage whose live frame is in a park zone (left there by a
    /// crash), since they would otherwise stay hidden while counted as visible.
    fn unstick(&mut self, stage: StageId, actions: &mut Vec<Action>) {
        let Some(s) = self.stage(stage) else { return };
        let mut ids = s.windows.clone();
        ids.extend(self.children_in(Home::Stage(stage)));
        for id in ids {
            let stuck = self
                .windows
                .get(&id)
                .is_some_and(|w| self.display(w.meta.display).parks(w.meta.frame));
            if stuck {
                self.restore_window(id, actions);
            }
        }
    }

    fn insert_restored(
        &mut self,
        meta: &WindowMeta,
        saved: &AssignedWindow,
        home: Home,
        restore_frames: bool,
    ) {
        let display = self.known_display(meta.display);
        let live_parked = self.display(display).parks(meta.frame);
        let base = if restore_frames || live_parked || meta.frame.is_degenerate() {
            saved.frame
        } else {
            meta.frame
        };
        let restore = self.safe_restore(base, display);
        // The saved pre-fit frame still applies while the window is where we fitted it.
        let original = saved.original.filter(|_| same_frame(base, saved.frame));
        let space = self.current_space(display);
        let mut meta = meta.clone();
        meta.display = display;
        self.windows.insert(
            meta.id,
            WindowState {
                meta,
                home,
                restore,
                parked: false,
                original,
                space,
                sticky: false,
            },
        );
    }
}

#[cfg(test)]
mod tests;
