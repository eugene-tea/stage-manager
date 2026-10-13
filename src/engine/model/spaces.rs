//! Spaces (Desktops): telling them apart, learning which windows live on each, and building
//! a Space's stages the first time it is visited.
//!
//! macOS has no public id for a Space, so the model gives each one a synthetic [`SpaceId`]
//! and recognizes it by its windows. The platform reports what the window server lists on
//! screen: a Space's own windows (parked ones included, since a parked window keeps a
//! sliver on screen), never another Space's. From that:
//!
//! - **Arrival** ([`Model::arrive`], on a Space change, at launch, and when the window list
//!   disagrees with the Space shown): each known Space scores its windows on screen minus
//!   its windows missing from the screen for no reason the platform can see (their app is
//!   not hidden, they are not minimized); the best Space with more present than missing is
//!   the one shown. So one window carried along or shown on every Space never makes a new
//!   Desktop look like the old one. With no such Space, the saved Space (`state.json`)
//!   whose windows match the on-screen ones exactly (at launch, the one shown when saved);
//!   with neither, a new Space (or the empty one that was shown, which loses nothing).
//!   Windows seen here for the first time are placed the way Apple's Stage Manager does
//!   when it is turned on (one stage per app), into the saved stages when the Space was
//!   saved.
//! - **Moves**: a known window on screen on a Space other than its own was moved there
//!   (Mission Control, or dragged across with the pointer): it leaves its stage and joins
//!   the shown Space's active stage. One that keeps showing on every Space the user goes to
//!   ([`STICKY_HOPS`] arrivals in a row, the last back where it came from) is on every Space
//!   (sticky): it is treated as pinned and no longer counted. Seen back where it came from
//!   once is not enough: the user may simply have moved it there and straight back.
//! - **Leaving** ([`Model::window_left_space`]): a window gone from the screen while its
//!   app is not hidden and it is not minimized has left the Space; it leaves its stage and
//!   comes back as a new arrival wherever it shows next.
//!
//! Empty Spaces are forgotten when left: they hold nothing, and the next visit makes a
//! fresh one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Action, DisplayId, Home, Model, Stage, StageId, WindowId, WindowMeta, WindowRole};
use crate::engine::persist::{LaunchHints, SavedSpace, exact_matches, rematch};

/// A window seen on screen on a Space other than its own at this many arrivals in a row,
/// the last one back on the Space it came from, is on every Space (sticky). Two are not
/// enough: moved to another Desktop and straight back (Mission Control, a quick user) looks
/// the same for two arrivals; a third move in a row within seconds does not happen.
pub const STICKY_HOPS: u32 = 3;

/// A Space's synthetic id: given by the model, stable while the app runs and kept in
/// `state.json`. Never Apple's own id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SpaceId(pub u64);

/// A Space the model knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    /// Its synthetic id.
    pub id: SpaceId,
    /// The display it belongs to.
    pub display: DisplayId,
    /// Logical time of the last visit.
    pub last_seen: u64,
}

impl Model {
    // ----- read access -------------------------------------------------------------------

    /// The Spaces visited so far, in creation order.
    pub fn spaces(&self) -> &[Space] {
        &self.spaces
    }

    /// The Space a display shows now.
    pub fn current_space(&self, display: DisplayId) -> SpaceId {
        self.current
            .get(&self.known_display(display))
            .copied()
            .or_else(|| self.spaces.first().map(|s| s.id))
            .unwrap_or(SpaceId(0))
    }

    /// The stages of a Space, in creation order (stage N is the Nth).
    pub fn stages_in(&self, space: SpaceId) -> impl Iterator<Item = &Stage> {
        self.stages.iter().filter(move |s| s.space == space)
    }

    /// Stage `n` (1-based) of a Space.
    pub fn stage_by_number(&self, space: SpaceId, n: usize) -> Option<StageId> {
        self.stages_in(space).nth(n.checked_sub(1)?).map(|s| s.id)
    }

    /// The Space a managed window lives on.
    pub fn space_of(&self, id: WindowId) -> Option<SpaceId> {
        self.windows.get(&id).map(|w| w.space)
    }

    /// True for a window seen on every Space.
    pub fn is_sticky(&self, id: WindowId) -> bool {
        self.windows.get(&id).is_some_and(|w| w.sticky)
    }

    /// Saved Spaces not visited yet this run.
    pub fn unvisited_saved_spaces(&self) -> usize {
        self.leftovers.len()
    }

    // ----- internal helpers --------------------------------------------------------------

    pub(crate) fn new_space(&mut self, display: DisplayId) -> SpaceId {
        let id = SpaceId(self.next_space);
        self.next_space += 1;
        self.spaces.push(Space {
            id,
            display,
            last_seen: 0,
        });
        id
    }

    /// The display a Space belongs to.
    pub(crate) fn space_display(&self, space: SpaceId) -> DisplayId {
        self.spaces
            .iter()
            .find(|s| s.id == space)
            .map_or(self.displays[0].id, |s| s.display)
    }

    /// True when some display shows `space` now.
    pub(crate) fn is_shown_space(&self, space: SpaceId) -> bool {
        self.current.values().any(|&s| s == space)
    }

    fn has_space(&self, space: SpaceId) -> bool {
        self.spaces.iter().any(|s| s.id == space)
    }

    /// True when no managed window lives on `space`.
    pub(crate) fn space_is_empty(&self, space: SpaceId) -> bool {
        !self.windows.values().any(|w| w.space == space)
    }

    /// Forgets an empty Space that is not shown: its (empty) stages, active stage, strip and
    /// reveal.
    fn prune_space(&mut self, space: SpaceId) {
        if self.is_shown_space(space) || !self.space_is_empty(space) {
            return;
        }
        self.stages.retain(|s| s.space != space);
        self.active.remove(&space);
        self.slots.remove(&space);
        self.revealed.remove(&space);
        self.spaces.retain(|s| s.id != space);
        self.moved_from.retain(|_, from| *from != space);
    }

    /// Renames a Space everywhere it is referenced.
    fn relabel_space(&mut self, old: SpaceId, new: SpaceId) {
        for s in &mut self.spaces {
            if s.id == old {
                s.id = new;
            }
        }
        for s in &mut self.stages {
            if s.space == old {
                s.space = new;
            }
        }
        for w in self.windows.values_mut() {
            if w.space == old {
                w.space = new;
            }
        }
        for c in self.current.values_mut() {
            if *c == old {
                *c = new;
            }
        }
        if let Some(v) = self.active.remove(&old) {
            self.active.insert(new, v);
        }
        if let Some(v) = self.slots.remove(&old) {
            self.slots.insert(new, v);
        }
        if let Some(v) = self.revealed.remove(&old) {
            self.revealed.insert(new, v);
        }
    }

    /// Takes the Spaces saved in `state.json`: each is rebuilt on its first visit
    /// ([`Model::arrive`]) and saved again untouched until then. The model's own Spaces are
    /// renumbered past the saved ids so none collides. Call it before the first arrival.
    pub fn restore_spaces(&mut self, saved: Vec<SavedSpace>) {
        let top = saved.iter().map(|s| s.id).max().unwrap_or(0);
        if self.spaces.iter().any(|s| s.id.0 <= top) {
            let own: Vec<SpaceId> = self.spaces.iter().map(|s| s.id).collect();
            self.next_space = self.next_space.max(top + 1);
            for old in own {
                let new = SpaceId(self.next_space);
                self.next_space += 1;
                self.relabel_space(old, new);
            }
        }
        self.leftovers = saved.into_iter().filter(|s| s.id != 0).collect();
    }

    // ----- arrival and learning ----------------------------------------------------------

    /// The platform arrived on a Space of `display` (a Space change, launch, or the window
    /// list disagreeing with the Space shown). `live` is every manageable window the window
    /// server lists on screen there now (children too), rules already applied (ignored
    /// windows left out); `absent` is every managed window not on screen whose app is not
    /// hidden and which is not minimized. See the module docs.
    ///
    /// Returns the actions: parking the inactive stages of windows seen for the first time,
    /// restoring moved windows that arrive parked. Nothing on another Space is touched.
    pub fn arrive(
        &mut self,
        display: DisplayId,
        live: &[WindowMeta],
        absent: &[WindowId],
        hints: &LaunchHints,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let display = self.known_display(display);
        let prev = self.current_space(display);
        let known: Vec<WindowId> = live
            .iter()
            .map(|m| m.id)
            .filter(|id| self.windows.contains_key(id))
            .collect();
        let arrivals: Vec<WindowMeta> = live
            .iter()
            .filter(|m| !self.windows.contains_key(&m.id))
            .cloned()
            .collect();

        // Known Spaces: present minus missing; saved Spaces: exact matches of first-seen
        // windows. Only a positive score counts; known Spaces win ties, then the most
        // recently seen.
        let mut scores: BTreeMap<SpaceId, (i64, bool, u64)> = self.scores(display, &known, absent);
        scores.retain(|_, (n, _, _)| *n > 0);
        for saved in &self.leftovers {
            let n = exact_matches(saved, &arrivals) as i64;
            if n > 0 {
                scores.entry(SpaceId(saved.id)).or_insert((0, false, 0)).0 += n;
            }
        }
        // At launch with no exact match (titles change), the Space shown when saved.
        let launch = self.spaces.iter().all(|s| s.last_seen == 0);
        let winner = scores
            .iter()
            .max_by_key(|(id, (n, known, seen))| (*n, *known, *seen, std::cmp::Reverse(**id)))
            .map(|(id, _)| *id)
            .or_else(|| {
                launch
                    .then(|| self.leftovers.iter().find(|l| l.current).map(|l| SpaceId(l.id)))
                    .flatten()
            });
        let target = match winner {
            Some(id) if self.has_space(id) => id,
            Some(id) => {
                self.spaces.push(Space {
                    id,
                    display,
                    last_seen: 0,
                });
                id
            }
            None if self.space_is_empty(prev) => prev,
            None => self.new_space(display),
        };
        self.show_space(display, target, prev);

        self.learn_moves(target, &known, &mut actions);
        self.moved_from
            .retain(|id, from| *from != target || known.contains(id));
        // A window not on screen here broke its run of hops: it is not on every Space.
        self.hops.retain(|id, _| known.contains(id));

        let saved = self
            .leftovers
            .iter()
            .position(|l| SpaceId(l.id) == target)
            .map(|i| self.leftovers.remove(i));
        if !arrivals.is_empty() || saved.is_some() {
            let empty = SavedSpace::default();
            let assignment = rematch(saved.as_ref().unwrap_or(&empty), &arrivals);
            let more = self.apply_launch(&arrivals, &assignment, false, hints);
            actions.extend(more.into_iter().filter(|a| *a != Action::StateChanged));
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// A managed window is no longer on the screen of the Space it lived on while its app is
    /// not hidden and it is not minimized: it moved to another Space (or full screen). It
    /// leaves its stage like a closed window; a parked one is first put back at its restore
    /// frame. Returns the actions and every window the model forgot (it and its children):
    /// the platform offers them again once they show on a screen.
    pub fn window_left_space(&mut self, id: WindowId) -> (Vec<Action>, Vec<WindowId>) {
        let Some(id) = self.resolve_standard(id) else {
            return (Vec::new(), Vec::new());
        };
        let mut actions = Vec::new();
        let mut group = vec![id];
        group.extend(self.children_of(id));
        for &g in &group {
            if self.windows.get(&g).is_some_and(|w| w.parked) {
                self.restore_window(g, &mut actions);
            }
        }
        self.moved_from.remove(&id);
        self.hops.remove(&id);
        actions.extend(self.remove_window(id));
        (actions, group)
    }

    /// Each of `display`'s known Spaces scored by its standard windows: on screen
    /// (`present`) count one up, missing from the screen for no visible reason (`absent`) one
    /// down: (score, known Space, last seen). Sticky windows and children do not count.
    fn scores(
        &self,
        display: DisplayId,
        present: &[WindowId],
        absent: &[WindowId],
    ) -> BTreeMap<SpaceId, (i64, bool, u64)> {
        let mut scores: BTreeMap<SpaceId, (i64, bool, u64)> = BTreeMap::new();
        for (ids, weight) in [(present, 1), (absent, -1)] {
            for id in ids {
                let Some(w) = self.windows.get(id) else { continue };
                if w.sticky || w.meta.role != WindowRole::Standard {
                    continue;
                }
                let Some(space) = self.spaces.iter().find(|s| s.id == w.space && s.display == display) else {
                    continue;
                };
                scores.entry(space.id).or_insert((0, true, space.last_seen)).0 += weight;
            }
        }
        scores
    }

    /// Makes `target` the Space `display` shows, declaring named stages there; forgets
    /// `prev` when it is empty.
    fn show_space(&mut self, display: DisplayId, target: SpaceId, prev: SpaceId) {
        self.current.insert(display, target);
        let names = self.options.named_stages.clone();
        self.declare_stages(target, &names);
        self.ensure_active(target);
        let t = self.tick();
        if let Some(s) = self.spaces.iter_mut().find(|s| s.id == target) {
            s.last_seen = t;
        }
        if prev != target {
            self.prune_space(prev);
        }
    }

    /// Known standard windows on screen on `target` whose Space is another one: moved here,
    /// or (seen on every Space gone to, [`STICKY_HOPS`] in a row, and now back where they
    /// came from) on every Space.
    fn learn_moves(&mut self, target: SpaceId, known: &[WindowId], actions: &mut Vec<Action>) {
        for &id in known {
            let Some(w) = self.windows.get(&id) else { continue };
            if w.sticky || w.meta.role != WindowRole::Standard || w.space == target {
                continue;
            }
            let hops = self.hops.entry(id).or_insert(0);
            *hops += 1;
            if *hops >= STICKY_HOPS && self.moved_from.get(&id) == Some(&target) {
                self.make_sticky(id, target, actions);
            } else {
                self.move_here(id, target, actions);
            }
        }
    }

    /// Moves a standard window (and its children) to `target`'s active stage, in front; a
    /// parked one is put back at its restore frame. A pinned window stays pinned.
    fn move_here(&mut self, id: WindowId, target: SpaceId, actions: &mut Vec<Action>) {
        let Some(w) = self.windows.get(&id) else { return };
        let (from, home) = (w.space, w.home);
        let display = self.space_display(target);
        let mut group = vec![id];
        group.extend(self.children_of(id));
        match home {
            Home::Stage(s) => {
                if let Some(stage) = self.stage_mut(s) {
                    stage.windows.retain(|&x| x != id);
                    if stage.key == Some(id) {
                        stage.key = stage.windows.first().copied();
                    }
                }
                let dest = self.ensure_active(target);
                if let Some(stage) = self.stage_mut(dest) {
                    stage.windows.insert(0, id);
                    stage.key.get_or_insert(id);
                }
                self.set_home(id, Home::Stage(dest), display);
                self.set_space(&group, target);
                self.drop_if_dead(s, actions);
            }
            Home::Pinned => {
                self.set_home(id, Home::Pinned, display);
                self.set_space(&group, target);
            }
        }
        for &g in &group {
            if self.windows.get(&g).is_some_and(|w| w.parked) {
                self.restore_window(g, actions);
            }
        }
        self.moved_from.insert(id, from);
    }

    /// A window seen on its old Space again after it showed on another: it is on every
    /// Space. It leaves its stage and is pinned for good (shown on every stage).
    fn make_sticky(&mut self, id: WindowId, target: SpaceId, actions: &mut Vec<Action>) {
        let Some(w) = self.windows.get(&id) else { return };
        let home = w.home;
        let display = self.space_display(target);
        let mut group = vec![id];
        group.extend(self.children_of(id));
        if let Home::Stage(s) = home {
            if let Some(stage) = self.stage_mut(s) {
                stage.windows.retain(|&x| x != id);
                if stage.key == Some(id) {
                    stage.key = stage.windows.first().copied();
                }
            }
            self.set_home(id, Home::Pinned, display);
            self.drop_if_dead(s, actions);
        }
        self.set_space(&group, target);
        for &g in &group {
            if let Some(w) = self.windows.get_mut(&g) {
                w.sticky = true;
            }
            if self.windows.get(&g).is_some_and(|w| w.parked) {
                self.restore_window(g, actions);
            }
        }
        self.moved_from.remove(&id);
        self.hops.remove(&id);
    }

    fn set_space(&mut self, ids: &[WindowId], space: SpaceId) {
        for id in ids {
            if let Some(w) = self.windows.get_mut(id) {
                w.space = space;
            }
        }
    }
}

#[cfg(test)]
mod tests;
