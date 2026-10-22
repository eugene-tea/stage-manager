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
//!   saved (a saved window is found by its window id first, then by bundle ID and title).
//! - **A Desktop still on screen**: a known Space with more of its windows on screen than
//!   missing that the arrival did not choose is still showing beside the one arriving (the
//!   window list read while a Desktop slides lists both Desktops' windows). Its windows are
//!   not taken for moves: they stay in their stages. Only when it is seen so at
//!   [`BESIDE_ARRIVALS`] arrivals in a row (the platform arrives again a settle later while
//!   windows of another Space stay on screen) were they really moved here.
//! - **Moves**: a known window on screen on a Space other than its own was moved there
//!   (Mission Control, or dragged across with the pointer): it leaves its stage and goes
//!   where a window of its app opening here goes: the active stage when that holds a window
//!   of its app, else the app's most recently active stage here, else a stage of its own;
//!   never filed with another app's windows. The stage of the frontmost moved window comes
//!   forward (the active stage goes to the strip); when windows of several apps move at
//!   once the others wait in the strip. Whole stages that move together to a Desktop
//!   without windows of its own (a known Desktop seen under a new id) are carried over as
//!   they are, grouping, order, names, active stage, strip order, before any window seen
//!   there for the first time is placed.
//!   One that keeps showing on every Space the user goes to
//!   ([`STICKY_HOPS`] arrivals in a row, the last back where it came from) is on every Space
//!   (sticky): it is treated as pinned and no longer counted. Seen back where it came from
//!   once is not enough: the user may simply have moved it there and straight back.
//! - **Leaving** ([`Model::window_left_space`]): a window gone from the screen while its
//!   app is not hidden and it is not minimized has left the Space; it leaves its stage and
//!   comes back as a new arrival wherever it shows next.
//!
//! Empty Spaces are forgotten when left: they hold nothing, and the next visit makes a
//! fresh one. A Space holding only pending stages (saved windows still to come) goes back to
//! the saved Spaces waiting for a visit instead.
//!
//! **After a restart** (`state.json` from another window server session): while the restore
//! window is open ([`crate::engine::restore`]) a Space made from an empty screen is
//! provisional until windows that match a saved Desktop's with confidence make it that
//! Desktop (adoption, `pending.rs`); all session, saved Spaces are recognized at an arrival
//! by confident matches (exact title, frame, fingerprint) rather than exact titles alone.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::pool::Scope;
use super::{Action, DisplayId, Home, Model, NativeTarget, Observation, Stage, StageId, WindowId, WindowMeta, WindowRole};
use crate::engine::persist::{LaunchFrames, LaunchHints, SavedSpace, bundle_matches, confident_matches, exact_matches, rematch_on};
use crate::engine::rules::Decision;
use crate::engine::window_match::LowRule;

/// A window seen on screen on a Space other than its own at this many arrivals in a row,
/// the last one back on the Space it came from, is on every Space (sticky). Two are not
/// enough: moved to another Desktop and straight back (Mission Control, a quick user) looks
/// the same for two arrivals; a third move in a row within seconds does not happen.
pub const STICKY_HOPS: u32 = 3;

/// A known Space seen on screen beside the one an arrival chose (more of its windows on
/// screen than missing) at this many arrivals in a row has really had its windows moved
/// here; fewer, it is a Desktop still sliding out (or in) and its windows stay where they
/// are. The platform arrives again a settle after windows of another Space stay on screen,
/// so a real move is learned about a second later.
pub const BESIDE_ARRIVALS: u32 = 2;

/// What an arrival did, for the log ([`Model::last_arrival`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArrivalNote {
    /// The saved Space (`state.json`) rebuilt on this visit, if any.
    pub rebuilt: Option<u64>,
    /// Known windows of other Spaces taken as moved here.
    pub moved: usize,
    /// Stages carried over whole (a known Desktop seen under a new id).
    pub carried: usize,
    /// Known Spaces seen on screen beside this one whose windows were left where they are.
    pub beside: Vec<u64>,
}

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

    /// What the last arrival did ([`Model::arrive`]).
    pub fn last_arrival(&self) -> &ArrivalNote {
        &self.last_arrival
    }

    /// True when the windows on screen look like two Desktops at once: two or more known
    /// Spaces with more of their windows on screen than missing, or saved Spaces whose
    /// windows show among the first-seen ones, together (the window list read while a
    /// Desktop slides in). A saved Space counts as showing when at least two of its windows
    /// match, or more than half of them: one window matching (a window of the same app and
    /// title opened elsewhere, one carried along) is not a whole Desktop. The platform reads
    /// the list again before it arrives. Arguments as [`Model::arrive`].
    pub fn shows_two_desktops(&self, display: DisplayId, live: &[WindowMeta], absent: &[WindowId]) -> bool {
        let display = self.known_display(display);
        let known: Vec<WindowId> = live.iter().map(|m| m.id).filter(|id| self.windows.contains_key(id)).collect();
        let arrivals: Vec<WindowMeta> = live.iter().filter(|m| !self.windows.contains_key(&m.id)).cloned().collect();
        let shown = self.scores(display, &known, absent).values().filter(|(n, _, _)| *n > 0).count();
        let saved = self
            .leftovers
            .iter()
            .filter(|l| {
                let n = exact_matches(l, &arrivals);
                n >= 2 || 2 * n > l.standard_windows()
            })
            .count();
        shown + saved >= 2
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

    /// True when `space` holds nothing: no managed window lives there and none is pending.
    pub(crate) fn space_is_empty(&self, space: SpaceId) -> bool {
        !self.windows.values().any(|w| w.space == space) && !self.space_has_pending(space)
    }

    /// Forgets a Space that is not shown and has no window: its stages, active stage, strip
    /// and reveal; pending stages go back to the saved Spaces waiting for a visit.
    fn prune_space(&mut self, space: SpaceId) {
        if self.is_shown_space(space) || self.windows.values().any(|w| w.space == space) {
            return;
        }
        self.pending_to_leftover(space);
        let gone: Vec<StageId> = self.stages.iter().filter(|s| s.space == space).map(|s| s.id).collect();
        self.kin.retain(|s, _| !gone.contains(s));
        self.pool_rank.retain(|s, _| !gone.contains(s));
        self.stages.retain(|s| s.space != space);
        self.active.remove(&space);
        self.slots.remove(&space);
        self.revealed.remove(&space);
        self.spaces.retain(|s| s.id != space);
        self.moved_from.retain(|_, from| *from != space);
    }

    /// Renames a Space everywhere it is referenced.
    pub(crate) fn relabel_space(&mut self, old: SpaceId, new: SpaceId) {
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
        if let Some(v) = self.slot_order.remove(&old) {
            self.slot_order.insert(new, v);
        }
        if self.provisional.remove(&old) {
            self.provisional.insert(new);
        }
        if let Some(v) = self.adopt_votes.remove(&old) {
            self.adopt_votes.insert(new, v);
        }
        if let Some(v) = self.beside.remove(&old) {
            self.beside.insert(new, v);
        }
        for from in self.moved_from.values_mut() {
            if *from == old {
                *from = new;
            }
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
        self.restore.saved_front = self.leftovers.iter().find(|l| l.current).and_then(|l| l.active_index.map(|i| (l.id, i)));
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
        let all = self.scores(display, &known, absent);
        let mut scores = all.clone();
        scores.retain(|_, (n, _, _)| *n > 0);
        let parks = |m: &WindowMeta| self.display(m.display).parks(m.frame);
        for saved in &self.leftovers {
            let n = if self.restore.new_session { confident_matches(saved, &arrivals, parks) } else { exact_matches(saved, &arrivals) } as i64;
            if n > 0 {
                scores.entry(SpaceId(saved.id)).or_insert((0, false, 0)).0 += n;
            }
        }
        // At launch with no exact match (titles change), the Space shown when saved, when
        // the apps on screen say so too (V3: quit on Desktop 2, relaunched on Desktop 1).
        let launch = self.spaces.iter().all(|s| s.last_seen == 0);
        let winner = scores
            .iter()
            .max_by_key(|(id, (n, known, seen))| (*n, *known, *seen, std::cmp::Reverse(**id)))
            .map(|(id, _)| *id)
            .or_else(|| launch.then(|| self.launch_fallback(&arrivals)).flatten());
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
        let saved = self
            .leftovers
            .iter()
            .position(|l| SpaceId(l.id) == target)
            .map(|i| self.leftovers.remove(i));
        // No window of its own and no saved stages: a known Desktop seen under a new id (or
        // a new Desktop), whose stages that moved here whole are carried over before the
        // windows seen here for the first time are placed.
        let bare = self.space_is_empty(target) && saved.is_none();
        let beside = self.hold_beside(target, &all);
        self.show_space(display, target, prev);
        self.match_screen(target, live, &mut actions);

        let mut moving = self.collect_moves(target, &known, &beside, &mut actions);
        let carried = if bare { self.carry_stages(target, &mut moving, &mut actions) } else { 0 };
        let mut arrivals = arrivals;
        if saved.is_none() && self.space_has_pending(target) {
            // A Space with stages still waiting: its new windows go to them first.
            let standard: Vec<WindowMeta> = arrivals.iter().filter(|m| m.role == WindowRole::Standard).cloned().collect();
            let shown = self.active_stage_in(target).map(|s| s.0);
            let placed = self.match_pending(target, &standard, LowRule { shown }, None, &mut actions);
            arrivals.retain(|m| !placed.contains(&m.id));
        }
        if !arrivals.is_empty() || saved.is_some() {
            let empty = SavedSpace::default();
            let mut assignment = rematch_on(saved.as_ref().unwrap_or(&empty), &arrivals, |m| self.display(m.display).parks(m.frame));
            if let Some(s) = &saved {
                // Its stages whose windows all came back on another Desktop are not built.
                self.skip_pooled_out(s.id, &mut assignment);
            }
            // After a restart, windows this Desktop's saved stages do not take are looked for
            // among every other Desktop's waiting windows (one pool, `pool.rs`).
            let unmatched: Vec<WindowMeta> = arrivals.iter().filter(|m| assignment.unmatched.contains(&m.id)).cloned().collect();
            let pooled = self.pool_claims(target, &unmatched);
            assignment.unmatched.retain(|id| !pooled.contains(id));
            if self.restore.restoring {
                // Windows no saved one claims go by Apple's rule (counted for the restore's line).
                self.note_by_apple(unmatched.iter().filter(|m| !pooled.contains(&m.id)));
            }
            let (claimed, rest): (Vec<WindowMeta>, Vec<WindowMeta>) = arrivals.iter().cloned().partition(|m| pooled.contains(&m.id));
            let more = self.launch_with(&rest, &assignment, LaunchFrames::Live, hints, saved.as_ref().map(|s| s.id));
            actions.extend(more.into_iter().filter(|a| *a != Action::StateChanged));
            if !claimed.is_empty() {
                let placed = self.match_waiting(target, &claimed, LowRule::default(), None, Scope::Pool, &mut actions);
                // Their children (the launch could not place them before their parents).
                let children: Vec<WindowMeta> = rest
                    .iter()
                    .filter(|m| matches!(m.role, WindowRole::Child { parent: Some(p) } if placed.contains(&p)))
                    .cloned()
                    .collect();
                for c in children {
                    let more = self.add_window(c, &Decision::default(), &Observation::default());
                    actions.extend(more.into_iter().filter(|a| *a != Action::StateChanged));
                }
            }
        }
        // Windows carried with their stages are not counted as moved one by one.
        let moved = moving.len();
        self.move_in(target, &moving, hints, &mut actions);
        self.moved_from
            .retain(|id, from| *from != target || known.contains(id));
        // A window not on screen here broke its run of hops: it is not on every Space.
        self.hops.retain(|id, _| known.contains(id));
        if self.restore.after_restart_window() && bare && carried == 0 && !self.leftovers.is_empty() && !self.space_has_pending(target) {
            // Made from a screen no saved Desktop matched while the restore window is open:
            // its confident windows say which saved Desktop it is. A Desktop made after the
            // window never takes a saved one.
            self.provisional.insert(target);
        }
        self.normalize_slots();
        self.last_arrival = ArrivalNote {
            rebuilt: saved.map(|s| s.id),
            moved,
            carried,
            beside: beside.iter().map(|s| s.0).collect(),
        };
        actions.push(Action::StateChanged);
        actions
    }

    /// Known Spaces other than `target` with a positive score in `all` (more of their windows
    /// on screen than missing) whose windows stay where they are at this arrival: seen so at
    /// fewer than [`BESIDE_ARRIVALS`] arrivals in a row.
    fn hold_beside(&mut self, target: SpaceId, all: &BTreeMap<SpaceId, (i64, bool, u64)>) -> BTreeSet<SpaceId> {
        let seen: Vec<SpaceId> = all.iter().filter(|(id, (n, _, _))| **id != target && *n > 0).map(|(id, _)| *id).collect();
        self.beside.retain(|s, _| seen.contains(s));
        let mut held = BTreeSet::new();
        for s in seen {
            let n = self.beside.entry(s).or_insert(0);
            *n += 1;
            if *n < BESIDE_ARRIVALS {
                held.insert(s);
            } else {
                self.beside.remove(&s);
            }
        }
        held
    }

    /// The saved Space shown when the app quit, for a launch whose windows match no saved
    /// title: taken only when at least one window on screen matches it by app and no other
    /// saved Space matches more windows by app; else `None` (a new Space; every saved one
    /// waits for its own visit).
    fn launch_fallback(&self, arrivals: &[WindowMeta]) -> Option<SpaceId> {
        let current = self.leftovers.iter().find(|l| l.current)?;
        let n = bundle_matches(current, arrivals);
        let beaten = self.leftovers.iter().any(|l| !l.current && bundle_matches(l, arrivals) > n);
        (n > 0 && !beaten).then_some(SpaceId(current.id))
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
    /// down: (score, known Space, last seen). Sticky windows do not count; children count
    /// only on a Space without standard windows (a Desktop holding just a dialog is still
    /// recognized by it).
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
                let counts = w.meta.role == WindowRole::Standard
                    || !self.windows.values().any(|o| o.space == w.space && o.meta.role == WindowRole::Standard);
                if w.sticky || !counts {
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

    /// Makes the screen match the model for `target`'s windows in `live`: one the model
    /// counts as parked but shown outside the park zone (its display changed size while its
    /// Space was not shown) is parked again; one the model counts as shown but found in the
    /// park zone (a batch cut short while Accessibility was off, a crash) is brought back.
    fn match_screen(&mut self, target: SpaceId, live: &[WindowMeta], actions: &mut Vec<Action>) {
        let mut repark = Vec::new();
        let mut unstick = Vec::new();
        for m in live {
            let Some(w) = self.windows.get(&m.id) else { continue };
            if w.space != target && !w.sticky {
                continue;
            }
            let in_zone = self.display(w.meta.display).parks(m.frame);
            if w.parked && !in_zone {
                repark.push(m.id);
            } else if !w.parked && in_zone {
                unstick.push(m.id);
            }
        }
        for id in repark {
            self.park_window(id, actions);
        }
        for id in unstick {
            self.restore_window(id, actions);
        }
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

    /// Known standard windows on screen on `target` whose Space is another one: moved here
    /// (returned, for [`Self::carry_stages`] and [`Self::move_in`]), or (seen on every Space
    /// gone to, [`STICKY_HOPS`] in a row, and now back where they came from) on every Space,
    /// made sticky here. Windows of a Space in `beside` (still on screen beside this one) stay
    /// where they are.
    fn collect_moves(
        &mut self,
        target: SpaceId,
        known: &[WindowId],
        beside: &BTreeSet<SpaceId>,
        actions: &mut Vec<Action>,
    ) -> Vec<WindowId> {
        let mut moving = Vec::new();
        for &id in known {
            let Some(w) = self.windows.get(&id) else { continue };
            if w.sticky || w.meta.role != WindowRole::Standard || w.space == target || beside.contains(&w.space) {
                continue;
            }
            let hops = self.hops.entry(id).or_insert(0);
            *hops += 1;
            if *hops >= STICKY_HOPS && self.moved_from.get(&id) == Some(&target) {
                self.make_sticky(id, target, actions);
            } else {
                moving.push(id);
            }
        }
        moving
    }

    /// Carries every stage whose standard windows are all in `moving` to `target` (a Space
    /// with no window of its own) as it is: name, members front to back, key window, strip
    /// order; the active one of their Spaces becomes `target`'s active stage (else the most
    /// recently active carried one). Carried windows leave `moving`. The windows of the
    /// active stage are shown where parked, the others parked where shown (a carried
    /// Desktop looks as it did). Returns how many stages were carried.
    fn carry_stages(&mut self, target: SpaceId, moving: &mut Vec<WindowId>, actions: &mut Vec<Action>) -> usize {
        let whole: Vec<(StageId, SpaceId)> = self
            .stages
            .iter()
            .filter(|s| s.space != target && !s.windows.is_empty() && s.windows.iter().all(|w| moving.contains(w)))
            .map(|s| (s.id, s.space))
            .collect();
        if whole.is_empty() {
            return 0;
        }
        let display = self.space_display(target);
        // Strip order: the carried stages in their old Spaces' slot order.
        let mut slots: Vec<StageId> = Vec::new();
        let mut was_active: Option<StageId> = None;
        for &(sid, from) in &whole {
            if let Some(from_slots) = self.slots.get_mut(&from) {
                if from_slots.contains(&sid) {
                    slots.push(sid);
                }
                from_slots.retain(|&x| x != sid);
            }
            if self.active.get(&from) == Some(&sid) {
                self.active.remove(&from);
                was_active.get_or_insert(sid);
            }
            let mut group: Vec<WindowId> = self.stage(sid).map(|s| s.windows.clone()).unwrap_or_default();
            for &m in &group {
                self.moved_from.insert(m, from);
            }
            group.extend(self.children_in(Home::Stage(sid)));
            self.set_space(&group, target);
            for &g in &group {
                if let Some(w) = self.windows.get_mut(&g) {
                    w.meta.display = display;
                }
            }
            if let Some(st) = self.stage_mut(sid) {
                st.space = target;
                st.display = display;
            }
            moving.retain(|w| !group.contains(w));
        }
        // An empty placeholder (the new Space's first stage, an empty declared one with a
        // carried stage's name) gives way.
        let names: Vec<Option<String>> = whole.iter().filter_map(|(sid, _)| self.stage(*sid).map(|s| s.name.clone())).collect();
        let carried: Vec<StageId> = whole.iter().map(|(sid, _)| *sid).collect();
        let placeholders: Vec<StageId> = self
            .stages
            .iter()
            .filter(|s| s.space == target && !carried.contains(&s.id) && s.windows.is_empty() && !self.has_pending(s.id))
            .filter(|s| s.name.is_none() || names.contains(&s.name))
            .map(|s| s.id)
            .collect();
        let pick = was_active
            .or_else(|| carried.iter().copied().max_by_key(|s| self.stage(*s).map_or(0, |st| st.last_active)))
            .unwrap_or(carried[0]);
        let current = self.active.get(&target).copied();
        if current.is_none_or(|c| placeholders.contains(&c)) {
            self.active.insert(target, pick);
        }
        for p in placeholders {
            if self.children_in(Home::Stage(p)).is_empty() && self.active.get(&target) != Some(&p) {
                self.stages.retain(|s| s.id != p);
                if let Some(sl) = self.slots.get_mut(&target) {
                    sl.retain(|&x| x != p);
                }
            }
        }
        let mut target_slots = self.slots.remove(&target).unwrap_or_default();
        target_slots.extend(slots);
        self.slots.insert(target, target_slots);
        for &sid in &carried {
            let mut ids: Vec<WindowId> = self.stage(sid).map(|s| s.windows.clone()).unwrap_or_default();
            ids.extend(self.children_in(Home::Stage(sid)));
            let shown = self.is_active(sid);
            for id in ids {
                match (shown, self.windows.get(&id).is_some_and(|w| w.parked)) {
                    (true, true) => self.restore_window(id, actions),
                    (false, false) => self.park_window(id, actions),
                    _ => {}
                }
            }
        }
        // The Spaces they came from: forgotten when empty; else, when their active stage went,
        // a fresh empty active stage (as Apple's: the Desktop left shows nothing, every stage
        // still there keeps its card). Never one of their parked stages made active without
        // its windows shown: no card, its app "present", ⌘-Tab doing nothing (Fable H3).
        let froms: BTreeSet<SpaceId> = whole.iter().map(|(_, from)| *from).collect();
        for from in froms {
            if self.space_is_empty(from) && !self.is_shown_space(from) {
                self.prune_space(from);
            } else if self.has_space(from) && !self.active.contains_key(&from) {
                self.fresh_active(from);
            }
        }
        carried.len()
    }

    /// Makes an empty stage `space`'s active one: an unnamed empty stage of its own when it
    /// has one (no child windows either), else a new one.
    fn fresh_active(&mut self, space: SpaceId) -> StageId {
        let empty = self
            .stages
            .iter()
            .filter(|s| s.space == space && s.windows.is_empty() && s.name.is_none())
            .map(|s| s.id)
            .find(|&s| self.children_in(Home::Stage(s)).is_empty());
        let stage = empty.unwrap_or_else(|| self.create_stage(space, None));
        if let Some(sl) = self.slots.get_mut(&space) {
            sl.retain(|&x| x != stage);
        }
        self.active.insert(space, stage);
        let t = self.tick();
        if let Some(s) = self.stage_mut(stage) {
            s.last_active = t;
        }
        stage
    }

    /// Windows moved to `target` from other Spaces (each with its children) go where a
    /// window of their app opening here goes ([`Model::native_target_in`]): with the app's
    /// windows in the active stage (or pinned), else into the app's most recently active
    /// stage on this Desktop, else into a stage of their app's own (the active stage when it
    /// is empty). So windows of one app moved together share a stage, those of different
    /// apps never do. The stage holding the frontmost moved window (`hints.z_order`; else
    /// the last one a window went to) is shown, the active stage going to the strip; the
    /// other stages moved windows went to wait in the strip, those windows parked. A pinned
    /// window stays pinned and shown.
    fn move_in(&mut self, target: SpaceId, ids: &[WindowId], hints: &LaunchHints, actions: &mut Vec<Action>) {
        if ids.is_empty() {
            return;
        }
        let display = self.space_display(target);
        let active = self.ensure_active(target);
        // (moved window, the stage it went to), in the order they were taken.
        let mut went: Vec<(WindowId, StageId)> = Vec::new();
        for &id in ids {
            let Some(w) = self.windows.get(&id) else { continue };
            let (from, home, pid) = (w.space, w.home, w.meta.pid);
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
                    // The window still counts as its old Space's here: only the app's
                    // windows already on `target` (moved ones included) decide.
                    let dest = match self.native_target_in(pid, target) {
                        NativeTarget::Join(d) => d,
                        NativeTarget::Fresh if self.stage_is_empty(active) => active,
                        NativeTarget::Fresh => self.create_stage(target, None),
                    };
                    if let Some(stage) = self.stage_mut(dest) {
                        stage.windows.insert(0, id);
                        stage.key = Some(id);
                    }
                    self.set_home(id, Home::Stage(dest), display);
                    self.set_space(&group, target);
                    self.drop_if_dead(s, actions);
                    went.push((id, dest));
                }
                Home::Pinned => {
                    self.set_home(id, Home::Pinned, display);
                    self.set_space(&group, target);
                    for &g in &group {
                        if self.windows.get(&g).is_some_and(|w| w.parked) {
                            self.restore_window(g, actions);
                        }
                    }
                }
            }
            self.moved_from.insert(id, from);
        }
        let Some(&(_, last)) = went.last() else { return };
        let front = hints
            .z_order
            .iter()
            .find_map(|z| went.iter().find(|(id, _)| id == z))
            .map_or(last, |(_, d)| *d);
        let mut waiting: Vec<StageId> = Vec::new();
        for &(id, dest) in &went {
            let mut group = vec![id];
            group.extend(self.children_of(id));
            if dest == front && front == active {
                // Shown where it is: in front of the active stage.
                for &g in &group {
                    if self.windows.get(&g).is_some_and(|w| w.parked) {
                        self.restore_window(g, actions);
                    }
                }
            } else if dest != front && dest != active {
                for &g in &group {
                    if self.windows.get(&g).is_some_and(|w| !w.parked) {
                        self.park_window(g, actions);
                    }
                }
                if !waiting.contains(&dest) {
                    waiting.push(dest);
                }
            }
            // Else the switch below shows the front stage and parks the active one.
        }
        if front != active {
            // The front stage comes forward; the one shown goes to the strip.
            let more = self.switch_in(target, active, front, None);
            actions.extend(more.into_iter().filter(|a| *a != Action::StateChanged));
        }
        for stage in waiting {
            // Just used, as far as the strip's eviction goes: a moved window's card is not
            // the first to go (the review of 6Y2, 3).
            let t = self.tick();
            if let Some(s) = self.stage_mut(stage) {
                s.last_active = t;
            }
            self.strip_enter(target, stage);
        }
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
