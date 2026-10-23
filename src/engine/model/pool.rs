//! One pool after a restart (round 8I): windows that come back on another Desktop than the
//! one they were saved on (strays) still find their saved stage.
//!
//! At a login macOS reopens a native app's windows on their own saved Desktops, but a
//! non-native app's (Electron and the like) on the Desktop shown (the owner's restarts of
//! 10-16). Each Desktop still becomes its saved Desktop by its own windows (adoption,
//! [`super::pending`], unchanged); the pool is for the strays. After a restart
//! ([`Model::pooling`]) a window appearing anywhere is scored against the waiting windows of
//! its own Desktop *and* of every other: the other Desktops' stages in the model and the
//! saved Desktops not visited yet. Its own Desktop's come first: only a window none of them
//! takes with confidence is scored against the other Desktops' (one call for those, so a
//! title saved on two other Desktops is as ambiguous as one saved in two stages; a native
//! keeps its own Desktop's entry whatever waits elsewhere). A confident match (strong or
//! medium; a low one only ever goes to the stage shown, its own Desktop's) recreates the
//! saved stage on the Desktop where the window showed, as a **part**:
//!
//! - the part already made there for the same saved stage (its [`Kin`]), else the stage of
//!   the saved stage's name there, else the Desktop's empty front stage (nothing shown there;
//!   used as Apple's rule uses it), else a new stage (which, while the login settles, takes
//!   the front from an empty saved active stage still waiting for its windows, as a new stage
//!   by Apple's rule does);
//! - only the matched entry moves: the saved stage's other windows keep waiting where they
//!   were (the natives, for their Desktop's visit), so a stage whose windows show on two
//!   Desktops makes its part on each;
//! - a window is live or waiting in one place only; a saved Desktop left with no window at
//!   all is dropped (one with windows still waiting keeps them, under the usual give-up
//!   rules).
//!
//! A window that votes for a saved Desktop on a Desktop made from an empty screen goes by
//! Apple's rule until that Desktop is taken, as before (its vote, not the pool, decides). In
//! the strip the adopted Desktop's saved slots come first, then the parts in their saved relative order
//! ([`Rank`]). When the restore window ends by itself, the Desktop that was shown when saved,
//! if it is the one shown now, brings its saved active stage forward when any of its windows
//! came back ([`Model::front_of_saved_current`]).
//!
//! A relaunch in the same window server session (window ids still valid) keeps each Desktop's
//! windows to that Desktop, as before.

use std::cmp::Reverse;
use std::collections::HashSet;

use super::pending::Placed;
use super::{Action, Home, Model, Observation, SpaceId, Stage, StageId, WindowId, WindowMeta, WindowRole};
use crate::engine::park::clamp_to_visible;
use crate::engine::persist::{Assignment, SavedRole, SavedSpace, SavedWindow, live_key};
use crate::engine::window_match::{Grade, LowRule, Saved, assign};

/// The saved stage a model stage stands for: built from it (an adoption, an arrival on a
/// saved Desktop) or made as its part on another Desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kin {
    /// Stage `.1` (its index in the file) of saved Desktop `.0`.
    Saved(u64, usize),
    /// A model stage with no saved origin (parts made from it name it).
    Stage(StageId),
}

/// A part's place among the parts in the strip: its saved slot (0 for its Desktop's active
/// stage, 1 + its strip slot, [`UNSLOTTED`] when it had no card), then the most recently
/// used first where slots collide.
pub(crate) type Rank = (usize, Reverse<u64>);

/// The rank of a stage that had no card when saved.
const UNSLOTTED: usize = usize::MAX / 2;

/// The scorer's group of stage `stage` of a saved Desktop `space` (model stages use their own
/// id; these never collide with one).
fn saved_group(space: u64, stage: usize) -> u64 {
    (1 << 62) | (space << 20) | stage as u64
}

/// The rank of stage `stage` of saved Desktop `saved`.
fn rank_of_saved(saved: &SavedSpace, stage: usize) -> Rank {
    let pos = if saved.active_index == Some(stage) {
        0
    } else {
        saved.strip_slots.iter().position(|&i| i == stage).map_or(UNSLOTTED, |p| p + 1)
    };
    (pos, Reverse(saved.stages.get(stage).and_then(|s| s.last_active).unwrap_or(0)))
}

/// Where a waiting entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spot {
    /// A model stage's pending list, at this index.
    Stage(StageId, usize),
    /// A saved Desktop not visited (index into the waiting ones), its stage, the entry's index
    /// among the stage's windows.
    Saved(usize, usize, usize),
}

/// Which waiting entries a match looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The Desktop's own stages only (the same session).
    Own,
    /// Its own and every other Desktop's (after a restart).
    OwnAndPool,
    /// Every other Desktop's only (an arrival's windows its own saved stages did not take).
    Pool,
}

/// What a stage number meant before the restore window's end acted
/// ([`Model::number_ref`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberRef {
    /// This stage.
    Stage(StageId),
    /// The stage after the last (a send creates it).
    Next,
    /// Nothing.
    Missing,
}

/// What a part stands for, taken with its first window.
#[derive(Debug, Clone, PartialEq)]
struct Origin {
    /// The saved stage.
    root: Kin,
    /// Its place among the parts in the strip.
    rank: Rank,
    /// Its name.
    name: Option<String>,
}

/// Where a matched window goes.
#[derive(Debug, Clone, PartialEq)]
enum Dest {
    /// A stage of the Desktop it showed on, waiting for it.
    Own(StageId),
    /// The part of another Desktop's stage on the Desktop it showed on.
    Part(Origin),
}

impl Model {
    /// True when every saved Desktop's waiting windows are one pool: after a restart (a file
    /// from another window server session) with the restore on.
    pub fn pooling(&self) -> bool {
        self.restore.new_session && !self.restore.drop_missing
    }

    /// Windows placed from another Desktop's saved entry (still managed): "K from other
    /// Desktops" in the restore's line.
    pub fn pooled_count(&self) -> usize {
        self.pooled.keys().filter(|id| self.windows.contains_key(id)).count()
    }

    /// True when window `id` was placed from another Desktop's saved entry.
    pub fn is_pooled(&self, id: WindowId) -> bool {
        self.pooled.contains_key(&id)
    }

    /// The saved stage stage `stage` stands for (itself when none).
    pub(crate) fn root_of(&self, stage: StageId) -> Kin {
        self.kin.get(&stage).copied().unwrap_or(Kin::Stage(stage))
    }

    /// The waiting entries `scope` looks at from `space`, with where each one is.
    fn waiting(&self, space: SpaceId, scope: Scope) -> (Vec<Spot>, Vec<Saved<'_>>) {
        let stage_ok = |s: &Stage| match scope {
            Scope::Own => s.space == space,
            Scope::OwnAndPool => true,
            Scope::Pool => s.space != space,
        };
        self.waiting_where(stage_ok, |_| scope != Scope::Own)
    }

    /// The waiting entries of the model's stages `stage_ok` takes and of the saved Desktops
    /// not visited `saved_ok` takes, with where each one is.
    fn waiting_where(&self, stage_ok: impl Fn(&Stage) -> bool, saved_ok: impl Fn(&SavedSpace) -> bool) -> (Vec<Spot>, Vec<Saved<'_>>) {
        let mut spots = Vec::new();
        let mut keys = Vec::new();
        for s in self.stages.iter().filter(|s| stage_ok(s)) {
            for (k, w) in self.pending_of(s.id).iter().enumerate() {
                keys.push(w.scorer_key(s.id.0));
                spots.push(Spot::Stage(s.id, k));
            }
        }
        for (i, l) in self.leftovers.iter().enumerate().filter(|(_, l)| saved_ok(l)) {
            for (j, st) in l.stages.iter().enumerate() {
                for (k, w) in st.windows.iter().enumerate().filter(|(_, w)| w.role == SavedRole::Standard) {
                    keys.push(w.scorer_key(saved_group(l.id, j)));
                    spots.push(Spot::Saved(i, j, k));
                }
            }
        }
        (spots, keys)
    }

    /// The matches of `metas` (windows on `space`) among the waiting entries `scope` looks at:
    /// (index into `metas`, the entry, how sure, the points). With [`Scope::OwnAndPool`] the
    /// window's own Desktop is scored first and the other Desktops only for a window nothing
    /// there takes with confidence (a native on its own Desktop never loses its confidence to
    /// a same-titled window waiting on another; the review of 8I, L3); a confident match
    /// elsewhere beats a low one into the stage shown.
    fn picks(&self, space: SpaceId, metas: &[WindowMeta], low: LowRule, scope: Scope) -> Vec<(usize, Spot, Grade, u32)> {
        let lives: Vec<_> = metas.iter().map(|m| live_key(m, self.display(m.display).parks(m.frame))).collect();
        let run = |scope: Scope, which: &[usize], low: LowRule| -> Vec<(usize, Spot, Grade, u32)> {
            let (spots, keys) = self.waiting(space, scope);
            let sub: Vec<_> = which.iter().map(|&i| lives[i].clone()).collect();
            assign(&keys, &sub, low).into_iter().zip(which).filter_map(|(p, &i)| p.map(|p| (i, spots[p.saved], p.grade, p.points))).collect()
        };
        let all: Vec<usize> = (0..metas.len()).collect();
        if scope != Scope::OwnAndPool {
            return run(scope, &all, low);
        }
        let own = run(Scope::Own, &all, low);
        let sure: HashSet<usize> = own.iter().filter(|c| c.2.confident()).map(|c| c.0).collect();
        let rest: Vec<usize> = all.into_iter().filter(|i| !sure.contains(i)).collect();
        let pool: Vec<_> = run(Scope::Pool, &rest, LowRule::default()).into_iter().filter(|c| c.2.confident()).collect();
        let pooled: HashSet<usize> = pool.iter().map(|c| c.0).collect();
        own.into_iter().filter(|c| !pooled.contains(&c.0)).chain(pool).collect()
    }

    /// Matches `metas` (standard windows new on `space`) against the waiting entries `scope`
    /// looks at and places the matched ones: a confident match takes its entry, into its
    /// stage on `space` or into its part there (see the module docs); a low one (into the
    /// stage shown only) leaves it waiting for the window itself. `obs`: one window appearing
    /// ([`Model::add_window`]), none at an arrival. Returns the windows placed.
    pub(crate) fn match_waiting(
        &mut self,
        space: SpaceId,
        metas: &[WindowMeta],
        low: LowRule,
        obs: Option<&Observation>,
        scope: Scope,
        actions: &mut Vec<Action>,
    ) -> HashSet<WindowId> {
        let chosen = self.picks(space, metas, low, scope);
        let mut touched = Vec::new();
        let mut taken = self.take_waiting(space, chosen, &mut touched);
        self.pending.retain(|_, l| !l.is_empty());
        taken.sort_by_key(|t| t.0);
        let mut placed = HashSet::new();
        for (i, dest, entry, grade, points) in taken {
            let meta = &metas[i];
            let from_other = matches!(dest, Dest::Part(_));
            let went = match dest {
                Dest::Own(stage) => self.insert_from_pending(meta, space, stage, &entry, grade, obs, actions),
                Dest::Part(origin) => self.place_in_part(meta, space, origin, entry, grade, obs, actions),
            };
            let (parked, switched) = (went == Placed::Parked, went == Placed::Switched);
            tracing::debug!(
                id = meta.id.0,
                bundle = %meta.bundle_id,
                grade = grade.name(),
                points,
                parked,
                switched,
                from_other,
                "restore: a window back in its saved stage"
            );
            self.last_pending =
                Some(super::PendingNote { window: meta.id, grade, points, parked, switched, adopted: None, from_other });
            placed.insert(meta.id);
        }
        for s in touched {
            self.drop_if_dead(s, actions);
        }
        self.drop_empty_leftovers();
        self.tidy_slot_order();
        placed
    }

    /// The windows of `metas` (standard ones new on `space`) that a saved entry of another
    /// Desktop claims with confidence ([`Scope::Pool`]); nothing changes.
    pub(crate) fn pool_claims(&self, space: SpaceId, metas: &[WindowMeta]) -> HashSet<WindowId> {
        if !self.pooling() {
            return HashSet::new();
        }
        self.picks(space, metas, LowRule::default(), Scope::Pool).into_iter().filter(|c| c.2.confident()).map(|c| metas[c.0].id).collect()
    }

    /// Takes the chosen entries out of where they wait: a confident one leaves, a low one
    /// (its own Desktop's stage shown) stays waiting.
    /// Stages of other Desktops that gave an entry go to `touched`.
    fn take_waiting(
        &mut self,
        space: SpaceId,
        chosen: Vec<(usize, Spot, Grade, u32)>,
        touched: &mut Vec<StageId>,
    ) -> Vec<(usize, Dest, SavedWindow, Grade, u32)> {
        let mut out = Vec::new();
        // Model stages: the highest index first per stage, so the others stay valid.
        let mut in_stages: Vec<(usize, StageId, usize, Grade, u32)> = chosen
            .iter()
            .filter_map(|&(i, spot, g, p)| match spot {
                Spot::Stage(s, k) => Some((i, s, k, g, p)),
                Spot::Saved(..) => None,
            })
            .collect();
        in_stages.sort_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)));
        for (i, s, k, grade, points) in in_stages {
            let own = self.stage(s).is_some_and(|st| st.space == space);
            if !own && !grade.confident() {
                continue;
            }
            let dest = if own { Dest::Own(s) } else { Dest::Part(self.origin_of_stage(s)) };
            let Some(list) = self.pending.get_mut(&s) else { continue };
            if k >= list.len() {
                continue;
            }
            let entry = if grade.confident() { list.remove(k) } else { list[k].clone() };
            if !own && !touched.contains(&s) {
                touched.push(s);
            }
            out.push((i, dest, entry, grade, points));
        }
        // Saved Desktops not visited: each matched entry leaves (the highest index first per
        // stage); the stage's other windows keep waiting there for that Desktop's visit.
        let mut in_saved: Vec<(usize, usize, usize, usize, Grade, u32)> = chosen
            .iter()
            .filter_map(|&(i, spot, g, p)| match spot {
                Spot::Saved(l, j, k) if g.confident() => Some((i, l, j, k, g, p)),
                _ => None,
            })
            .collect();
        in_saved.sort_by(|a, b| (a.1, a.2).cmp(&(b.1, b.2)).then(b.3.cmp(&a.3)));
        for (i, l, j, k, grade, points) in in_saved {
            let Some(saved) = self.leftovers.get(l) else { continue };
            if j >= saved.stages.len() {
                continue;
            }
            let origin = Origin { root: Kin::Saved(saved.id, j), rank: rank_of_saved(saved, j), name: saved.stages[j].name.clone() };
            if let Some(entry) = self.take_saved_entry(l, j, k) {
                out.push((i, Dest::Part(origin), entry, grade, points));
            }
        }
        out
    }

    /// Takes entry `k` of stage `j` of the saved Desktop not visited `l` out; a stage left
    /// with no standard window is emptied and noted as gone to the pool (no stage with
    /// nothing in it is written, none is built at the Desktop's visit).
    fn take_saved_entry(&mut self, l: usize, j: usize, k: usize) -> Option<SavedWindow> {
        let saved = self.leftovers.get_mut(l)?;
        let id = saved.id;
        let st = saved.stages.get_mut(j).filter(|st| k < st.windows.len())?;
        let entry = st.windows.remove(k);
        if !st.windows.iter().any(|w| w.role == SavedRole::Standard) {
            st.windows.clear();
            self.pooled_out.insert((id, j));
        }
        Some(entry)
    }

    /// Drops the waiting entry at `spot` (a rule placed its window, or the window stays where
    /// Apple's rule put it): a stage of the model left with nothing goes, a saved Desktop
    /// left with nothing too.
    fn drop_spot(&mut self, spot: Spot) {
        match spot {
            Spot::Stage(stage, k) => {
                if let Some(list) = self.pending.get_mut(&stage)
                    && k < list.len()
                {
                    list.remove(k);
                }
                self.pending.retain(|_, l| !l.is_empty());
                let mut ignored = Vec::new();
                self.drop_if_dead(stage, &mut ignored);
            }
            Spot::Saved(l, j, k) => {
                self.take_saved_entry(l, j, k);
                self.drop_empty_leftovers();
            }
        }
        self.tidy_slot_order();
    }

    /// What a part of model stage `stage` (on another Desktop) stands for.
    fn origin_of_stage(&self, stage: StageId) -> Origin {
        let s = self.stage(stage);
        let rank = self.pool_rank.get(&stage).copied().unwrap_or_else(|| {
            let space = s.map(|s| s.space);
            let pos = if space.is_some_and(|sp| self.active.get(&sp) == Some(&stage)) {
                0
            } else {
                let in_list = |list: Option<&Vec<StageId>>| list.and_then(|l| l.iter().position(|&x| x == stage));
                space
                    .and_then(|sp| in_list(self.slot_order.get(&sp)).or_else(|| in_list(self.slots.get(&sp))))
                    .map_or(UNSLOTTED, |p| p + 1)
            };
            (pos, Reverse(s.map_or(0, |s| s.last_active)))
        });
        Origin { root: self.root_of(stage), rank, name: s.and_then(|s| s.name.clone()) }
    }

    /// Puts window `meta` into the part of `origin` on `space` (see the module docs) from its
    /// entry `entry`, as any window back from a saved entry
    /// ([`Model::insert_from_pending`]).
    #[allow(clippy::too_many_arguments)]
    fn place_in_part(
        &mut self,
        meta: &WindowMeta,
        space: SpaceId,
        origin: Origin,
        entry: SavedWindow,
        grade: Grade,
        obs: Option<&Observation>,
        actions: &mut Vec<Action>,
    ) -> Placed {
        let part = self.part_for(space, &origin, obs, actions);
        self.kin.insert(part, origin.root);
        self.pool_rank.insert(part, origin.rank);
        let went = self.insert_from_pending(meta, space, part, &entry, grade, obs, actions);
        tracing::debug!(id = meta.id.0, stage = part.0, "restore: a window of another Desktop's saved stage, in its part here");
        self.pooled.insert(meta.id, origin.root);
        went
    }

    /// The stage of `space` that takes a window of `origin` (see the module docs).
    fn part_for(&mut self, space: SpaceId, origin: &Origin, obs: Option<&Observation>, actions: &mut Vec<Action>) -> StageId {
        // Merged into another stage here (R5): the merge stands.
        if let Some(&s) = self.merged.get(&origin.root)
            && self.stage(s).is_some_and(|st| st.space == space)
        {
            return s;
        }
        if let Some(s) = self.stages.iter().find(|s| s.space == space && self.kin.get(&s.id) == Some(&origin.root)) {
            return s.id;
        }
        if let Some(s) = origin.name.as_deref().and_then(|n| self.find_named(space, n)) {
            return s;
        }
        let active = self.ensure_active(space);
        let free = self.stage(active).is_some_and(|s| s.windows.is_empty() && s.name.is_none())
            && !self.has_pending(active)
            && !self.kin.contains_key(&active)
            && !self.restore.front_waits.contains(&active)
            && self.children_in(Home::Stage(active)).is_empty();
        if free {
            return active;
        }
        let part = self.create_stage(space, origin.name.clone());
        let t = self.tick();
        if let Some(s) = self.stage_mut(part) {
            s.last_active = t;
        }
        if let Some(obs) = obs
            && self.settling()
            && self.front_waits_empty(space)
        {
            // Nothing is shown in front (the saved active stage still waits for its windows):
            // the part takes the front, as a new stage by Apple's rule does (the review of 8F,
            // M4); the saved stage waits, inactive and with no card.
            actions.extend(self.switch_to(part, obs).into_iter().filter(|a| *a != Action::StateChanged));
        }
        part
    }

    /// Takes window `meta`'s confident match out of the waiting entries `scope` looks at
    /// from `space` (a rule placed the window). True when one was taken.
    pub(crate) fn forget_waiting(&mut self, meta: &WindowMeta, space: SpaceId, scope: Scope) -> bool {
        let pick = self.picks(space, std::slice::from_ref(meta), LowRule::default(), scope).into_iter().find(|c| c.2.confident());
        let Some((_, spot, _, _)) = pick else { return false };
        self.drop_spot(spot);
        true
    }

    /// Voters that can no longer adopt anything (their Desktop was taken by another saved
    /// one, or the restore window is over): each goes into its saved stage where it is, a part
    /// on its Desktop (a confident match among every Desktop's waiting windows, as any stray),
    /// else it stays where Apple's rule put it and the entry it voted with (on saved Desktop
    /// `.0`) is forgotten. A window is never live and waiting at once (the review of 8I, M2).
    /// A voter with children of its own, or one placed since, stays where it is.
    ///
    /// The restore window ends at the user's first action (a card pressed, a number key), and
    /// that action must still mean what the user saw (the review of 8I2): a voter alone in a
    /// stage of its own (no name, no saved origin, nothing waiting) makes that very stage its
    /// part, keeping its id, number and card; a voter whose stage still goes (it joins a part
    /// or a stage that was waiting for it) leaves the stage's id to follow it
    /// ([`Model::follow_stage`]). A voter waiting in its own Desktop's stage also leaves the
    /// entry it voted with (no stale duplicate). A parked voter is placed from its real frame,
    /// not the park corner.
    pub(crate) fn pool_voters(&mut self, votes: Vec<(u64, WindowId)>, actions: &mut Vec<Action>) {
        if !self.pooling() {
            return;
        }
        for (desktop, v) in votes {
            let Some(w) = self.windows.get(&v) else { continue };
            if self.restored.contains_key(&v) {
                continue;
            }
            let (space, meta, home) = (w.space, w.meta.clone(), w.home);
            let movable = matches!(home, Home::Stage(_)) && meta.role == WindowRole::Standard && self.children_of(v).is_empty();
            let pick = if movable {
                self.picks(space, std::slice::from_ref(&meta), LowRule::default(), Scope::OwnAndPool).into_iter().find(|c| c.2.confident())
            } else {
                None
            };
            let (Some((_, spot, _, _)), Home::Stage(from)) = (pick, home) else {
                if self.forget_voted(&meta, desktop) {
                    tracing::debug!(id = v.0, desktop, "restore: a voter stays where Apple's rule put it; its saved entry is forgotten");
                }
                continue;
            };
            let own = matches!(spot, Spot::Stage(s, _) if self.stage(s).is_some_and(|st| st.space == space));
            let in_place = !own && self.become_part(from, v, space, spot);
            let mark = actions.len();
            let was_parked = self.windows.get(&v).is_some_and(|w| w.parked);
            let Some(meta) = self.lift(v) else { continue };
            let placed = self.match_waiting(space, std::slice::from_ref(&meta), LowRule::default(), None, Scope::OwnAndPool, actions);
            if placed.is_empty() {
                // Never left unmanaged: it comes back as any new window.
                tracing::warn!(id = v.0, "restore: a voter's saved stage could not take it after all; managed again as a new window");
                let more = self.add_window(meta.clone(), &crate::engine::rules::Decision::default(), &Observation::default());
                actions.extend(more.into_iter().filter(|a| *a != Action::StateChanged));
            } else {
                tracing::debug!(id = v.0, desktop, in_place, own, "restore: a voter that can adopt nothing goes back to its saved stage here");
            }
            if own && space.0 != desktop && self.forget_voted(&meta, desktop) {
                tracing::debug!(id = v.0, desktop, "restore: a voter back in its own Desktop's stage; the entry it voted with is forgotten");
            }
            // Parked, it was taken out at its real frame: shown now, it is brought back.
            let shown = self.windows.get(&v).is_some_and(|w| !w.parked);
            let moved = actions[mark..].iter().any(|a| matches!(a, Action::SetFrame { id, .. } if *id == v));
            if was_parked && shown && !moved {
                self.restore_window(v, actions);
            }
            if self.stage(from).is_some_and(|s| s.windows.is_empty()) && in_place {
                // Claimed as the part but the window went elsewhere after all: a plain stage
                // again.
                self.kin.remove(&from);
                self.pool_rank.remove(&from);
                if let Some(st) = self.stage_mut(from) {
                    st.name = None;
                }
            }
            self.drop_if_dead(from, actions);
            if self.stage(from).is_none() {
                self.followed.insert(from, v);
            }
        }
        self.drop_empty_leftovers();
    }

    /// Makes `stage`, where voter `v` of `space` is alone (no name, no saved origin, nothing
    /// waiting, no orphan), the part of the saved stage its waiting entry at `spot` (another
    /// Desktop's) stands for, when no stage of `space` is that part already or bears its name:
    /// the voter then goes back into the stage it is in. True when it did.
    fn become_part(&mut self, stage: StageId, v: WindowId, space: SpaceId, spot: Spot) -> bool {
        let alone = self.stage(stage).is_some_and(|s| s.windows == [v] && s.name.is_none())
            && !self.has_pending(stage)
            && !self.kin.contains_key(&stage)
            && !self.pool_rank.contains_key(&stage)
            && !self.restore.front_waits.contains(&stage)
            && self.children_in(Home::Stage(stage)).is_empty();
        if !alone {
            return false;
        }
        let origin = match spot {
            Spot::Stage(s, _) => self.origin_of_stage(s),
            Spot::Saved(l, j, _) => {
                let Some(saved) = self.leftovers.get(l).filter(|saved| j < saved.stages.len()) else { return false };
                Origin { root: Kin::Saved(saved.id, j), rank: rank_of_saved(saved, j), name: saved.stages[j].name.clone() }
            }
        };
        let elsewhere = self.stages.iter().any(|s| s.space == space && s.id != stage && self.kin.get(&s.id) == Some(&origin.root))
            || origin.name.as_deref().and_then(|n| self.find_named(space, n)).is_some_and(|s| s != stage);
        if elsewhere {
            return false;
        }
        self.kin.insert(stage, origin.root);
        self.pool_rank.insert(stage, origin.rank);
        if let Some(st) = self.stage_mut(stage) {
            st.name = origin.name;
        }
        tracing::debug!(id = v.0, stage = stage.0, "restore: a voter's own stage becomes its saved stage's part (its card and number stay)");
        true
    }

    /// Takes managed window `id` out of its stage (to be placed again at once; the stage
    /// stays, dropped by the caller if left dead). Its meta, when it was managed, at its real
    /// frame when it was parked (where it goes back to; as [`Model::adopt`] does).
    fn lift(&mut self, id: WindowId) -> Option<WindowMeta> {
        let w = self.windows.remove(&id)?;
        let mut meta = w.meta;
        if w.parked {
            meta.frame = clamp_to_visible(w.restore, self.display(meta.display).visible);
        }
        if let Home::Stage(from) = w.home
            && let Some(st) = self.stage_mut(from)
        {
            st.windows.retain(|&x| x != id);
            if st.key == Some(id) {
                st.key = st.windows.first().copied();
            }
        }
        Some(meta)
    }

    /// The stage `stage` stands for now: itself while it exists, else, when the restore
    /// window's end moved its voter into its saved stage, the stage that window is in (a card
    /// clicked or a number pressed for it as the window closed follows the window).
    pub fn follow_stage(&self, stage: StageId) -> StageId {
        if self.stage(stage).is_some() {
            return stage;
        }
        let to = self.followed.get(&stage).and_then(|v| self.windows.get(v)).and_then(|w| match w.home {
            Home::Stage(s) => Some(s),
            Home::Pinned => None,
        });
        to.unwrap_or(stage)
    }

    /// What number `n` (a hotkey's) means on `space` now, to be read again after the
    /// restore window's end ([`Model::number_now`]).
    pub fn number_ref(&self, space: SpaceId, n: usize) -> NumberRef {
        match self.stage_by_number(space, n) {
            Some(s) => NumberRef::Stage(s),
            None if n == self.stages_in(space).count() + 1 => NumberRef::Next,
            None => NumberRef::Missing,
        }
    }

    /// The number on `space` of what `r` meant ([`Model::number_ref`]): the same stage (or
    /// the one its window followed), the next new stage, or 0 (none).
    pub fn number_now(&self, space: SpaceId, r: NumberRef) -> usize {
        match r {
            NumberRef::Stage(s) => {
                let s = self.follow_stage(s);
                self.stage(s).filter(|st| st.space == space).and_then(|_| self.stage_number(s)).unwrap_or(0)
            }
            NumberRef::Next => self.stages_in(space).count() + 1,
            NumberRef::Missing => 0,
        }
    }

    /// Forgets the entry of saved Desktop `desktop` (waiting unvisited, or in the stages of
    /// the Desktop that took its id) that window `meta` matches with confidence. True when
    /// one was.
    fn forget_voted(&mut self, meta: &WindowMeta, desktop: u64) -> bool {
        let (spots, keys) = self.waiting_where(|s| s.space.0 == desktop, |l| l.id == desktop);
        let parked = self.display(meta.display).parks(meta.frame);
        let pick = assign(&keys, &[live_key(meta, parked)], LowRule::default()).into_iter().next().flatten();
        let Some(spot) = pick.filter(|p| p.grade.confident()).map(|p| spots[p.saved]) else { return false };
        self.drop_spot(spot);
        true
    }

    /// Leaves the stages the pool emptied out of `assignment` (saved Desktop `id`'s, taken out
    /// of the waiting ones at its visit or adoption): none is built, none takes a strip slot;
    /// when the saved active stage is one, the most recently used stage with windows or
    /// waiting entries is active instead (none: the usual fallback). Stage indices are kept
    /// (each stage built stands for its saved stage, [`Kin::Saved`]). The review of 8I, M1.
    pub(crate) fn skip_pooled_out(&mut self, id: u64, assignment: &mut Assignment) {
        let gone: Vec<usize> = (0..assignment.stages.len())
            .filter(|&i| {
                let st = &assignment.stages[i];
                self.pooled_out.contains(&(id, i)) && st.windows.is_empty() && st.pending.is_empty()
            })
            .collect();
        self.pooled_out.retain(|(d, _)| *d != id);
        if gone.is_empty() {
            return;
        }
        assignment.strip_slots.retain(|i| !gone.contains(i));
        if assignment.active_index.is_some_and(|a| gone.contains(&a)) {
            let next = assignment
                .stages
                .iter()
                .enumerate()
                .filter(|(i, s)| !gone.contains(i) && (!s.windows.is_empty() || !s.pending.is_empty()))
                .max_by_key(|(i, s)| (s.last_active.unwrap_or(0), Reverse(*i)))
                .map(|(i, _)| i);
            tracing::info!(desktop = id, active = ?next, "restore: the saved active stage's windows all came back on another Desktop; the most recent stage left is active");
            assignment.active_index = next;
        }
    }

    /// Drops the saved Desktops not visited that have no standard window left (every entry
    /// went elsewhere, or was given up), with the votes for them.
    /// Only after a restart: the same session drops nothing it did not before.
    pub(crate) fn drop_empty_leftovers(&mut self) {
        if !self.pooling() {
            return;
        }
        let gone: Vec<u64> = self.leftovers.iter().filter(|l| l.standard_windows() == 0).map(|l| l.id).collect();
        if gone.is_empty() {
            return;
        }
        for id in &gone {
            tracing::info!(desktop = id, "restore: a saved Desktop whose windows all came back elsewhere (or were given up) is dropped");
        }
        self.leftovers.retain(|l| !gone.contains(&l.id));
        for votes in self.adopt_votes.values_mut() {
            votes.retain(|v| !gone.contains(&v.0));
        }
        self.adopt_votes.retain(|_, v| !v.is_empty());
    }

    /// Keeps the parts' cards of `space` after the adopted Desktop's (the last card of a stage
    /// built from the saved Desktop `space` became), in their saved relative order ([`Rank`]).
    pub(crate) fn arrange_pool_slots(&mut self, space: SpaceId) {
        let Some(slots) = self.slots.get(&space) else { return };
        let is_part = |s: &StageId| self.pool_rank.contains_key(s);
        let mut parts: Vec<StageId> = slots.iter().copied().filter(is_part).collect();
        if parts.is_empty() {
            return;
        }
        parts.sort_by_key(|s| (self.pool_rank.get(s).copied(), *s));
        let mut rest: Vec<StageId> = slots.iter().copied().filter(|s| !is_part(s)).collect();
        let adopted = |s: &StageId| matches!(self.kin.get(s), Some(Kin::Saved(id, _)) if *id == space.0);
        let at = rest.iter().rposition(adopted).map_or(rest.len(), |p| p + 1);
        rest.splice(at..at, parts);
        self.slots.insert(space, rest);
    }

    /// When the restore window ended by itself after a restart, on the main display's
    /// Desktop if it is the one shown when saved: its saved active stage comes forward when
    /// any of its windows came back (an adoption keeps a front that already shows windows,
    /// a stray's stage say). Else nothing (the usual end: [`Model::front_after_restore`],
    /// [`Model::focus_after_restore`]). A part of that stage on another Desktop never takes
    /// that Desktop's front.
    pub fn front_of_saved_current(&mut self, obs: &Observation) -> Vec<Action> {
        if !self.after_restart() {
            return Vec::new();
        }
        let Some((id, index)) = self.restore.saved_front else { return Vec::new() };
        let space = self.current_space(self.main_display().id);
        if space.0 != id {
            return Vec::new();
        }
        let Some(target) = self
            .stages
            .iter()
            .find(|s| {
                s.space == space
                    && !self.pool_rank.contains_key(&s.id)
                    && self.kin.get(&s.id) == Some(&Kin::Saved(id, index))
                    && !s.windows.is_empty()
            })
            .map(|s| s.id)
        else {
            return Vec::new();
        };
        if self.is_active(target) {
            return Vec::new();
        }
        tracing::info!(stage = target.0, desktop = id, "restore: the saved active stage of the Desktop shown when saved comes forward");
        self.switch_to(target, obs)
    }
}

#[cfg(test)]
mod tests;
