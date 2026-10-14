//! Pending stages: saved windows not on screen yet, kept in their saved stages until their
//! apps bring them back ([`crate::engine::restore`] has the whole story).
//!
//! - A window appearing later is scored against the pending windows of its Space
//!   ([`Model::place_from_pending`], called by [`Model::add_window`] for windows the rules
//!   leave to Apple's default). A strong or medium match goes to its saved stage at its saved
//!   frame; when that stage is inactive the window is parked there quietly while the restore
//!   window is open, and after it the stage comes forward (the window shows because of the
//!   user, as Apple's rule switches to an app's stage). A low one goes only into the active
//!   stage, with its live frame, and leaves the saved window waiting. Anything else is
//!   Apple's rule. A window a rule places takes its confident match out of the waiting ones.
//! - An arrival on a Space with pending stages matches its new windows the same way first
//!   (parked, never switched to).
//! - **Adoption**: on a provisional Space (made from an empty screen while the restore window
//!   was open), a window matching a saved Desktop's window with confidence makes the Space
//!   that Desktop: its saved id, its stages rebuilt pending with their names, recency and
//!   strip order, its active stage (unless the Space's own active stage already shows
//!   windows). The saved Desktop shown when saved takes one such window; another takes two
//!   while the restore window is open (one window of a Desktop is weak evidence: macOS may
//!   reopen it anywhere), and the windows that voted go back to their saved stages (and
//!   frames) then. After the restore window a Desktop not shown when saved is never adopted
//!   (a vote would move a window the user sees into a stage not shown).
//! - A Space left with only pending stages goes back to the saved Desktops waiting for a
//!   visit ([`Model::pending_to_leftover`]); nothing pending is lost.

use std::collections::HashSet;

use super::{Action, DisplayId, Home, Model, Observation, SpaceId, StageId, WindowId, WindowMeta, WindowRole, WindowState};
use crate::engine::park::clamp_to_visible;
use crate::engine::persist::{LaunchFrames, LaunchHints, Pending, SavedRole, SavedWindow, live_key, rematch_on};
use crate::engine::restore::{Restore, slot_for};
use crate::engine::stage_area::same_frame;
use crate::engine::window_match::{Grade, LowRule, Saved, assign};

/// A window placed from a saved entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Restored {
    /// How sure the match was.
    pub grade: Grade,
    /// Its saved place in its stage, 0 = front.
    pub z: usize,
}

/// What the last placement from a pending entry did, for the log (no titles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingNote {
    /// The window placed.
    pub window: WindowId,
    /// How sure the match was.
    pub grade: Grade,
    /// Its points.
    pub points: u32,
    /// It went into an inactive stage (parked).
    pub parked: bool,
    /// Its inactive stage came forward (after the restore window).
    pub switched: bool,
    /// The saved Desktop the window's Space became (adoption), if it did.
    pub adopted: Option<u64>,
}

impl Model {
    /// Sets the restore's session and mode (the platform, before the first arrival).
    pub fn set_restore(&mut self, restore: Restore) {
        self.restore = restore;
    }

    /// True while the restore window is open ([`crate::engine::restore`]).
    pub fn restoring(&self) -> bool {
        self.restore.restoring
    }

    /// True when `state.json` came from another window server session (a restart or a new
    /// login since).
    pub fn after_restart(&self) -> bool {
        self.restore.new_session
    }

    /// Closes the restore window for good (the first user action, or its time is up): no
    /// Desktop becomes provisional any more, an empty saved active stage no longer holds its
    /// place, and a window matched to an inactive stage brings it forward. Returns true when
    /// it was open.
    pub fn end_restore(&mut self) -> bool {
        // Votes for a Desktop not shown when saved count only while the window is open.
        self.adopt_votes.clear();
        std::mem::replace(&mut self.restore.restoring, false)
    }

    /// At the restore window's end: a Space shown whose active stage is still empty, its
    /// saved windows still to come, while other stages there have windows, brings one of
    /// those forward (never an empty front stage with cards beside it): the stage of the
    /// window focused (`obs`), else the most recently active one, as Apple's Stage Manager
    /// shows the app in front. The empty stage keeps waiting for its windows, with no card.
    pub fn front_after_restore(&mut self, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        let shown: Vec<SpaceId> = self.current.values().copied().collect();
        for space in shown {
            let Some(active) = self.active_stage_in(space) else { continue };
            if !self.stage(active).is_some_and(|s| s.windows.is_empty()) || !self.has_pending(active) {
                continue;
            }
            let with_windows = |s: &&super::Stage| s.space == space && s.id != active && !s.windows.is_empty();
            let focused = obs.focused.and_then(|f| self.stages.iter().filter(with_windows).find(|s| s.windows.contains(&f)));
            let Some(target) = focused.or_else(|| self.stages.iter().filter(with_windows).max_by_key(|s| s.last_active)).map(|s| s.id) else {
                continue;
            };
            tracing::info!(stage = target.0, "restore: the saved active stage is still empty at the window's end; the most recent stage with windows comes forward");
            actions.extend(self.switch_to(target, obs));
        }
        actions
    }

    /// True when stage `stage` holds pending windows.
    pub fn has_pending(&self, stage: StageId) -> bool {
        self.pending.get(&stage).is_some_and(|p| !p.is_empty())
    }

    /// The pending windows of a stage, front to back.
    pub fn pending_of(&self, stage: StageId) -> &[SavedWindow] {
        self.pending.get(&stage).map_or(&[], Vec::as_slice)
    }

    /// Pending windows in the model's stages (saved Desktops not visited yet not counted).
    pub fn pending_count(&self) -> usize {
        self.pending.values().map(Vec::len).sum()
    }

    /// Pending windows of the saved Desktops not visited yet.
    pub fn pending_unvisited(&self) -> usize {
        self.leftovers.iter().map(|l| l.standard_windows()).sum()
    }

    /// The apps with pending windows in the model's stages, sorted.
    pub fn pending_bundles(&self) -> Vec<String> {
        let mut out: Vec<String> = self.pending.values().flatten().map(|w| w.bundle_id.clone()).collect();
        out.sort();
        out.dedup();
        out
    }

    /// True when a stage of `space` holds pending windows.
    pub(crate) fn space_has_pending(&self, space: SpaceId) -> bool {
        self.stages.iter().any(|s| s.space == space && self.has_pending(s.id))
    }

    /// The provisional Spaces (not adopted yet).
    pub fn provisional_spaces(&self) -> Vec<SpaceId> {
        self.provisional.iter().copied().filter(|p| self.spaces.iter().any(|s| s.id == *p)).collect()
    }

    /// How sure the match that placed window `id` was, when it came from a saved entry.
    pub fn restored_grade(&self, id: WindowId) -> Option<Grade> {
        self.restored.get(&id).map(|r| r.grade)
    }

    /// What the last placement from a pending entry did.
    pub fn last_pending(&self) -> Option<&PendingNote> {
        self.last_pending.as_ref()
    }

    /// Saved windows about to be pending: standard ones, stamped with this session unless
    /// already pending since an earlier one.
    pub(crate) fn mark_pending(&self, list: &[SavedWindow]) -> Vec<SavedWindow> {
        list.iter()
            .filter(|w| w.role == SavedRole::Standard)
            .cloned()
            .map(|mut w| {
                w.pending.get_or_insert_with(|| Pending { since: self.restore.session.clone() });
                w
            })
            .collect()
    }

    /// Gives up the pending windows of apps `bundles` (running and quiet for
    /// [`crate::engine::restore::QUIET`]): stages left with nothing go away unless active or
    /// declared. Returns the actions and how many were given up.
    pub fn expire_pending(&mut self, bundles: &[String]) -> (Vec<Action>, usize) {
        let mut actions = Vec::new();
        let mut dropped = 0;
        let mut touched = Vec::new();
        for (stage, list) in &mut self.pending {
            let before = list.len();
            list.retain(|w| !bundles.contains(&w.bundle_id));
            if list.len() != before {
                dropped += before - list.len();
                touched.push(*stage);
            }
        }
        self.pending.retain(|_, l| !l.is_empty());
        for s in touched {
            self.drop_if_dead(s, &mut actions);
        }
        self.tidy_slot_order();
        if dropped > 0 {
            self.normalize_slots();
            actions.push(Action::StateChanged);
        }
        (actions, dropped)
    }

    /// Forgets saved strip orders of Spaces with nothing pending any more.
    fn tidy_slot_order(&mut self) {
        let keep: Vec<SpaceId> = self.slot_order.keys().copied().filter(|&s| self.space_has_pending(s)).collect();
        self.slot_order.retain(|s, _| keep.contains(s));
    }

    /// A standard window appeared on `space` (the rules leave it to Apple's default): placed
    /// from a pending entry when it matches one well enough (see the module docs), adopting a
    /// saved Desktop first when `space` is provisional. `obs`: the screen now (the window
    /// focused there keeps the stage's key; after the restore window a match into an inactive
    /// stage switches to it). Actions go to `actions` (adoption's too, when the window itself
    /// then goes by Apple's rule). True when the window was placed.
    pub(crate) fn place_from_pending(&mut self, meta: &WindowMeta, space: SpaceId, obs: &Observation, actions: &mut Vec<Action>) -> bool {
        let mut space = space;
        let mut adopted = None;
        if self.provisional.contains(&space)
            && !self.space_has_pending(space)
            && space == self.current_space(self.main_display().id)
            && let Some(i) = self.adoption_for(meta)
        {
            let id = self.leftovers[i].id;
            let take = if self.leftovers[i].current {
                true
            } else if self.restore.restoring {
                let votes = self.adopt_votes.entry(space).or_default();
                if !votes.contains(&(id, meta.id)) {
                    votes.push((id, meta.id));
                }
                let n = votes.iter().filter(|v| v.0 == id).count();
                if n < 2 {
                    tracing::debug!(id = meta.id.0, desktop = id, "restore: a window of a saved Desktop not shown when saved; a second one is needed to take it");
                }
                n >= 2
            } else {
                // After the restore window a vote would move a window the user sees (the
                // first voter) into a stage not shown: Apple's rule instead.
                tracing::debug!(id = meta.id.0, desktop = id, "restore: a window of a saved Desktop not shown when saved, after the restore window; not adopted");
                false
            };
            if take {
                let voters: Vec<WindowId> =
                    self.adopt_votes.remove(&space).unwrap_or_default().into_iter().filter(|v| v.0 == id && v.1 != meta.id).map(|v| v.1).collect();
                actions.extend(self.adopt(space, i, &voters));
                space = SpaceId(id);
                adopted = Some(id);
            }
        }
        if !self.space_has_pending(space) {
            return false;
        }
        let shown = self.active_stage_in(space).map(|s| s.0);
        let placed = self.match_pending(space, std::slice::from_ref(meta), LowRule { shown }, Some(obs), actions);
        if let Some(note) = self.last_pending.as_mut().filter(|n| n.window == meta.id) {
            note.adopted = adopted;
        }
        !placed.is_empty()
    }

    /// A standard window a rule placed (not Apple's default) on `space`: the saved window it
    /// matches with confidence is no longer awaited (the rule decided where it goes; the entry
    /// would otherwise wait for, and maybe catch, another window).
    pub(crate) fn forget_pending_match(&mut self, meta: &WindowMeta, space: SpaceId) {
        if meta.role != WindowRole::Standard || !self.space_has_pending(space) {
            return;
        }
        let mut entries: Vec<(StageId, usize)> = Vec::new();
        let mut keys: Vec<Saved> = Vec::new();
        for s in self.stages.iter().filter(|s| s.space == space) {
            for (k, w) in self.pending_of(s.id).iter().enumerate() {
                keys.push(w.scorer_key(s.id.0));
                entries.push((s.id, k));
            }
        }
        let parked = self.display(meta.display).parks(meta.frame);
        let pick = assign(&keys, &[live_key(meta, parked)], LowRule::default()).into_iter().next().flatten();
        let Some((stage, k)) = pick.filter(|p| p.grade.confident()).map(|p| entries[p.saved]) else {
            return;
        };
        if let Some(list) = self.pending.get_mut(&stage)
            && k < list.len()
        {
            list.remove(k);
            tracing::debug!(id = meta.id.0, "restore: a rule placed a window that was awaited; its saved entry is dropped");
        }
        self.pending.retain(|_, l| !l.is_empty());
        let mut ignored = Vec::new();
        self.drop_if_dead(stage, &mut ignored);
        self.tidy_slot_order();
    }

    /// The saved Desktop (index into the waiting ones) whose window `meta` matches with
    /// confidence, unambiguously between Desktops.
    fn adoption_for(&self, meta: &WindowMeta) -> Option<usize> {
        let mut owner = Vec::new();
        let mut keys: Vec<Saved> = Vec::new();
        for (i, l) in self.leftovers.iter().enumerate() {
            for w in l.stages.iter().flat_map(|s| s.windows.iter()).filter(|w| w.role == SavedRole::Standard) {
                keys.push(w.scorer_key(i as u64));
                owner.push(i);
            }
        }
        let parked = self.display(meta.display).parks(meta.frame);
        let pick = assign(&keys, &[live_key(meta, parked)], LowRule::default()).into_iter().next().flatten()?;
        pick.grade.confident().then(|| owner[pick.saved])
    }

    /// Makes provisional Space `space` the saved Desktop `self.leftovers[index]`: its id, its
    /// stages rebuilt pending (names, recency, strip order) and its active stage unless the
    /// Space's own active stage shows windows already. `voters`: windows that matched it
    /// earlier (in the restore window) and went by Apple's rule; they go back to their saved
    /// stages now, at their saved frames on a confident match (a window with children of its
    /// own stays where it is).
    fn adopt(&mut self, space: SpaceId, index: usize, voters: &[WindowId]) -> Vec<Action> {
        let saved = self.leftovers.remove(index);
        let target = SpaceId(saved.id);
        self.provisional.remove(&space);
        self.relabel_space(space, target);
        let mut actions = Vec::new();
        let mut moved: Vec<WindowMeta> = Vec::new();
        for &v in voters {
            let Some(w) = self.windows.get(&v) else { continue };
            let Home::Stage(from) = w.home else { continue };
            if w.space != target || w.meta.role != WindowRole::Standard || self.restored.contains_key(&v) || !self.children_of(v).is_empty() {
                continue;
            }
            if w.parked {
                // Shown again first, so its frame is a real one wherever it goes now (parked
                // again below when its saved stage is inactive).
                self.restore_window(v, &mut actions);
                let shown = self.windows.get(&v).map(|w| clamp_to_visible(w.restore, self.display(w.meta.display).visible));
                if let (Some(frame), Some(w)) = (shown, self.windows.get_mut(&v)) {
                    w.meta.frame = frame;
                }
            }
            let Some(w) = self.windows.remove(&v) else { continue };
            moved.push(w.meta);
            if let Some(st) = self.stage_mut(from) {
                st.windows.retain(|&x| x != v);
                if st.key == Some(v) {
                    st.key = st.windows.first().copied();
                }
            }
            self.drop_if_dead(from, &mut actions);
        }
        let mut assignment = rematch_on(&saved, &moved, |m| self.display(m.display).parks(m.frame));
        let showing = self.active_stage_in(target).is_some_and(|a| self.stage(a).is_some_and(|s| !s.windows.is_empty()));
        if showing {
            assignment.active_index = None;
        }
        // Their saved frames on a confident match, as any window back from a saved entry.
        actions.extend(self.launch_with(&moved, &assignment, LaunchFrames::Matched, &LaunchHints::default()));
        actions.retain(|a| *a != Action::StateChanged);
        tracing::info!(desktop = saved.id, stages = saved.stages.len(), voters = moved.len(), "restore: a Desktop made from an empty screen is the saved Desktop its windows belong to");
        actions
    }

    /// Matches `metas` (standard windows new on `space`) against the pending windows of
    /// `space`'s stages and places the matched ones ([`Model::insert_from_pending`]): a
    /// confident match takes its saved entry, a low one (into the stage shown only) leaves it
    /// waiting for the window itself. `obs`: one window appearing ([`Model::add_window`]),
    /// none at an arrival. Returns the windows placed.
    pub(crate) fn match_pending(
        &mut self,
        space: SpaceId,
        metas: &[WindowMeta],
        low: LowRule,
        obs: Option<&Observation>,
        actions: &mut Vec<Action>,
    ) -> HashSet<WindowId> {
        let mut entries: Vec<(StageId, usize)> = Vec::new();
        let mut keys: Vec<Saved> = Vec::new();
        for s in self.stages.iter().filter(|s| s.space == space) {
            for (k, w) in self.pending_of(s.id).iter().enumerate() {
                keys.push(w.scorer_key(s.id.0));
                entries.push((s.id, k));
            }
        }
        let parked: Vec<bool> = metas.iter().map(|m| self.display(m.display).parks(m.frame)).collect();
        let lives: Vec<_> = metas.iter().zip(&parked).map(|(m, p)| live_key(m, *p)).collect();
        let picks = assign(&keys, &lives, low);
        let mut chosen: Vec<(usize, StageId, usize, Grade, u32)> = metas
            .iter()
            .enumerate()
            .zip(&picks)
            .filter_map(|((i, _), p)| p.map(|p| (i, entries[p.saved].0, entries[p.saved].1, p.grade, p.points)))
            .collect();
        // Take the confident entries out (highest index first per stage, so the others stay
        // valid); a low match leaves its entry waiting.
        chosen.sort_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)));
        let mut taken: Vec<(usize, StageId, SavedWindow, Grade, u32)> = Vec::new();
        for (i, stage, k, grade, points) in chosen {
            if let Some(list) = self.pending.get_mut(&stage)
                && k < list.len()
            {
                let entry = if grade.confident() { list.remove(k) } else { list[k].clone() };
                taken.push((i, stage, entry, grade, points));
            }
        }
        self.pending.retain(|_, l| !l.is_empty());
        taken.sort_by_key(|t| t.0);
        let mut placed = HashSet::new();
        for (i, stage, entry, grade, points) in taken {
            let meta = &metas[i];
            let went = self.insert_from_pending(meta, space, stage, &entry, grade, obs, actions);
            let (parked, switched) = (went == Placed::Parked, went == Placed::Switched);
            tracing::debug!(
                id = meta.id.0,
                bundle = %meta.bundle_id,
                grade = grade.name(),
                points,
                parked,
                switched,
                "restore: a window back in its saved stage"
            );
            self.last_pending = Some(PendingNote { window: meta.id, grade, points, parked, switched, adopted: None });
            placed.insert(meta.id);
        }
        self.tidy_slot_order();
        placed
    }

    /// Puts window `meta` into `stage` of `space` from its saved entry: a strong or medium
    /// match at its saved frame with its flags (pinned, pre-fit frame), a low one at its live
    /// frame (a live frame in a park zone always takes the saved one); in the saved place
    /// among the stage's restored windows, the stage's key when it was saved as the key (not
    /// over a window of the stage the user has focused, `obs`). A window of an inactive stage
    /// is parked and the stage's card takes its saved strip place, or, for a window appearing
    /// after the restore window (`obs` given), the stage comes forward with it as its key;
    /// one shown is moved only when its frame differs.
    #[allow(clippy::too_many_arguments)]
    fn insert_from_pending(
        &mut self,
        meta: &WindowMeta,
        space: SpaceId,
        stage: StageId,
        entry: &SavedWindow,
        grade: Grade,
        obs: Option<&Observation>,
        actions: &mut Vec<Action>,
    ) -> Placed {
        let display: DisplayId = self.known_display(meta.display);
        let live_parked = self.display(display).parks(meta.frame);
        let sure = grade.confident();
        let base = if sure || live_parked || meta.frame.is_degenerate() { entry.frame } else { meta.frame };
        let restore = self.safe_restore(base, display);
        let original = entry.original.filter(|_| sure && same_frame(base, entry.frame));
        let home = if sure && entry.pinned { Home::Pinned } else { Home::Stage(stage) };
        let id = meta.id;
        let mut meta = meta.clone();
        meta.display = display;
        let live = meta.frame;
        self.windows.insert(id, WindowState { meta, home, restore, parked: false, original, space, sticky: false });
        self.restored.insert(id, Restored { grade, z: entry.z });
        let shown = match home {
            Home::Pinned => true,
            Home::Stage(s) => self.is_active(s),
        };
        let follow = obs.filter(|_| !shown && sure && !self.restore.restoring);
        if let Home::Stage(s) = home {
            let at = self.stage(s).map_or(0, |st| {
                st.windows
                    .iter()
                    .position(|w| self.restored.get(w).is_some_and(|r| r.z > entry.z))
                    .unwrap_or(st.windows.len())
            });
            // The user's own focus on another window of the stage keeps the key there.
            let focused_other = obs.and_then(|o| o.focused).is_some_and(|f| f != id && self.stage(s).is_some_and(|st| st.windows.contains(&f)));
            if let Some(st) = self.stage_mut(s) {
                st.windows.insert(at, id);
                if follow.is_some() || st.key.is_none() || (entry.key && !focused_other) {
                    st.key = Some(id);
                }
            }
        }
        self.fit_window(id);
        if shown {
            let restore = self.windows[&id].restore;
            if !same_frame(restore, live) {
                actions.push(Action::SetFrame { id, frame: restore });
            }
            Placed::Shown
        } else if let Some(obs) = follow {
            // After the restore window the window shows because of the user (an activation,
            // an un-minimize, a Dock click): its stage comes forward, as Apple's rule brings
            // an app's stage forward (`NativeTarget::Join`).
            actions.extend(self.switch_to(stage, obs));
            Placed::Switched
        } else {
            self.park_window(id, actions);
            self.strip_enter_restored(space, stage);
            Placed::Parked
        }
    }

    /// A stage that came back enters the strip in its saved place when the saved order
    /// knows it ([`slot_for`]), evicting the least recently used other card when full; else
    /// as any stage entering the strip.
    fn strip_enter_restored(&mut self, space: SpaceId, stage: StageId) {
        let capacity = self.options.strip.capacity;
        let place = self
            .slot_order
            .get(&space)
            .and_then(|order| slot_for(self.slots.get(&space).map_or(&[], Vec::as_slice), order, stage));
        let Some(at) = place.filter(|_| capacity > 0) else {
            self.strip_enter(space, stage);
            return;
        };
        let recency: Vec<(StageId, u64)> = self.stages.iter().map(|s| (s.id, s.last_active)).collect();
        let age = |id: StageId| recency.iter().find(|r| r.0 == id).map_or(0, |r| r.1);
        let slots = self.slots.entry(space).or_default();
        if slots.contains(&stage) {
            return;
        }
        slots.insert(at.min(slots.len()), stage);
        while slots.len() > capacity {
            let Some(i) = slots.iter().enumerate().filter(|(_, s)| **s != stage).min_by_key(|(_, s)| age(**s)).map(|(i, _)| i) else {
                break;
            };
            slots.remove(i);
        }
    }

    /// A Space being forgotten (left, with no window on it) keeps what it still waits for:
    /// its pending stages go back to the saved Desktops waiting for a visit, under its id.
    pub(crate) fn pending_to_leftover(&mut self, space: SpaceId) {
        if self.space_has_pending(space) {
            let mut saved = self.save_space(space);
            saved.current = false;
            if saved.standard_windows() > 0 && !self.leftovers.iter().any(|l| l.id == saved.id) {
                self.leftovers.push(saved);
            }
        }
        let stages: Vec<StageId> = self.stages_in(space).map(|s| s.id).collect();
        for s in stages {
            self.pending.remove(&s);
        }
        self.slot_order.remove(&space);
        self.provisional.remove(&space);
        self.adopt_votes.remove(&space);
    }
}

/// Where [`Model::insert_from_pending`] put a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placed {
    /// In a stage shown (or pinned).
    Shown,
    /// Parked in its inactive stage.
    Parked,
    /// Its inactive stage came forward with it.
    Switched,
}

#[cfg(test)]
mod tests;
