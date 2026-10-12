//! Persistence: `state.json` and re-matching saved windows to live ones on launch.
//!
//! Window IDs do not survive an app relaunch, so saved windows are matched heuristically:
//! exact bundle ID + title first, then bundle ID only in saved order. Each live window is
//! used at most once; live windows that match nothing join the active stage.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::geometry::Rect;
use super::model::{
    Action, Home, Model, Observation, StageId, WindowId, WindowMeta, WindowRole, WindowState,
};
use super::park::is_parked;
use super::rules::Decision;

/// The state file format version this build reads and writes.
pub const STATE_VERSION: u32 = 1;

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
}

/// One saved stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedStage {
    /// The stage's name, if any.
    pub name: Option<String>,
    /// Its windows, front to back.
    pub windows: Vec<SavedWindow>,
}

/// The contents of `state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateFile {
    /// Always [`STATE_VERSION`].
    pub version: u32,
    /// Stages in creation order (hotkey numbers).
    pub stages: Vec<SavedStage>,
    /// Index into `stages` of the main display's active stage.
    pub active_index: Option<usize>,
    /// Indices into `stages` of the main display's strip slots, top to bottom.
    #[serde(default)]
    pub strip_slots: Vec<usize>,
}

impl StateFile {
    /// Pretty JSON for writing to disk.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Parses JSON, rejecting other versions.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let state: StateFile =
            serde_json::from_str(text).map_err(|e| format!("state.json: {e}"))?;
        if state.version != STATE_VERSION {
            return Err(format!(
                "state.json: version {} is not supported (expected {STATE_VERSION})",
                state.version
            ));
        }
        Ok(state)
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

/// Places live standard windows into saved stages: exact bundle + title first, then bundle
/// only, both in saved order (stage order, then front to back). Each live window is used at
/// most once. Child windows on either side are left out; they follow their parents.
pub fn rematch(state: &StateFile, live: &[WindowMeta]) -> Assignment {
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
    let passes: [fn(&SavedWindow, &WindowMeta) -> bool; 2] = [
        |s, l| s.bundle_id == l.bundle_id && s.title == l.title,
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
    /// Snapshot for `state.json`. Pinned windows are saved with the main display's active
    /// stage, flagged `pinned`; children are saved with their stage after its members.
    pub fn to_state(&self) -> StateFile {
        let active = self.main_active_stage();
        let active_index = self.stages.iter().position(|s| Some(s.id) == active);
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
            })
        };
        let mut stages: Vec<SavedStage> = self
            .stages
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
            let base = stages[i].windows.len();
            stages[i].windows.extend(
                pinned
                    .into_iter()
                    .enumerate()
                    .filter_map(|(k, id)| save(id, base + k, false, true)),
            );
        }
        let strip_slots = self
            .strip_slots(self.main_display().id)
            .into_iter()
            .filter_map(|id| self.stages.iter().position(|s| s.id == id))
            .collect();
        StateFile {
            version: STATE_VERSION,
            stages,
            active_index,
            strip_slots,
        }
    }

    /// Rebuilds stages from an [`Assignment`] on launch. `live` lists every live window
    /// (children too; they follow their parents). Saved stages with no matched window are
    /// dropped unless active or declared. Unmatched standard windows join the active stage.
    /// Windows of inactive stages are parked; with `restore_frames` the active stage's and
    /// pinned windows get their saved frames, else they keep their live frames (a live frame
    /// in a park zone always falls back to the saved one). Windows already known are skipped.
    pub fn apply_assignment(
        &mut self,
        live: &[WindowMeta],
        assignment: &Assignment,
        restore_frames: bool,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let metas: HashMap<WindowId, &WindowMeta> = live.iter().map(|m| (m.id, m)).collect();
        let main = self.main_display().id;
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
            let display = standing
                .first()
                .map(|w| self.known_display(metas[&w.id].display))
                .unwrap_or(main);
            let sid = match st.name.as_deref().and_then(|n| self.find_named(n)) {
                Some(s) if !built.contains(&Some(s)) => s,
                _ => self.create_stage(display, st.name.clone()),
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
            let display = self.stage(sid).map_or(main, |s| s.display);
            let previous = self.active.insert(display, sid);
            let t = self.tick();
            if let Some(stage) = self.stage_mut(sid) {
                stage.last_active = t;
            }
            if let Some(p) = previous.filter(|&p| p != sid) {
                self.drop_if_dead(p, &mut actions);
            }
        }

        for &id in &assignment.unmatched {
            let Some(meta) = metas.get(&id) else { continue };
            if self.windows.contains_key(&id) || placed.contains(&id) {
                continue;
            }
            let display = self.known_display(meta.display);
            let stage = self.ensure_active(display);
            let mut meta = (*meta).clone();
            meta.display = display;
            let restore = self.safe_restore(meta.frame, display);
            self.windows.insert(
                id,
                WindowState {
                    meta,
                    home: Home::Stage(stage),
                    restore,
                    parked: false,
                },
            );
            if let Some(s) = self.stage_mut(stage) {
                s.windows.push(id);
                if s.key.is_none() {
                    s.key = Some(id);
                }
            }
        }

        let stage_ids: Vec<StageId> = self.stages.iter().map(|s| s.id).collect();
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
                self.restore_window(id, &mut actions);
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
        self.slots.insert(main, slots);
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
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
                .is_some_and(|w| is_parked(w.meta.frame, self.display(w.meta.display).frame));
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
        let live_parked = is_parked(meta.frame, self.display(display).frame);
        let base = if restore_frames || live_parked || meta.frame.is_degenerate() {
            saved.frame
        } else {
            meta.frame
        };
        let restore = self.safe_restore(base, display);
        let mut meta = meta.clone();
        meta.display = display;
        self.windows.insert(
            meta.id,
            WindowState {
                meta,
                home,
                restore,
                parked: false,
            },
        );
    }
}

#[cfg(test)]
mod tests;
