//! Windows of apps that quit, kept for `state.json` (round 8H): the fallback when a logout's
//! early signal never comes.
//!
//! The owner's restart of 10-16 had every app quit before any logout signal: each quit took
//! the app's windows out of the model, its stage went with them, and the save that followed
//! wrote the loss, so the next login had almost nothing to restore. Now an app's quit still
//! takes its windows out of the model (Apple's behaviour: no card, nothing waits in this
//! session), but their saved entries are kept apart ([`QuitWindow`]) and written with
//! `quit_at` in their stages, a stage gone with them written too, in its old place among the
//! stages and in the strip. At a load in a new session those that quit within
//! [`crate::engine::restore::QUIT_KEEP`] before the save wait in their stages as any saved
//! window not on screen does ([`crate::engine::persist::StateFile::settle_quit`]).
//!
//! - **Taken before the window goes** ([`Model::closing`]): the platform reads a window's
//!   entry when it is destroyed, before the model forgets it, and hands those of an app that
//!   quits a moment later to [`Model::keep_quit`] (a window the user closed on its own stays
//!   closed). Its frame is its last real one (the restore frame), never a park frame.
//! - **Kept for a while** ([`Model::prune_quit`]): an entry older than `QUIT_KEEP` at a save is
//!   dropped, and so are an app's entries when the app shows a window again in this session
//!   ([`Model::forget_quit_of`]): it is back, its windows go by the usual rules.

use std::collections::BTreeMap;

use super::{Home, Model, Pid, SpaceId, StageId, WindowId, WindowRole};
use crate::engine::park::clamp_to_visible;
use crate::engine::persist::{SavedRole, SavedStage, SavedWindow};
use crate::engine::restore::QUIT_KEEP;
use crate::engine::window_match::fingerprint;

/// A window's saved entry read just before it left the model (its app may be quitting).
#[derive(Debug, Clone, PartialEq)]
pub struct Closing {
    /// The window's app.
    pub pid: Pid,
    /// The window.
    pub id: WindowId,
    /// Its Space.
    pub space: SpaceId,
    /// Its stage (a pinned window's: its Space's active stage, flagged pinned).
    pub stage: StageId,
    /// The stage's name.
    pub name: Option<String>,
    /// The stage's recency.
    pub last_active: u64,
    /// The stage's place in the strip then, if it had a card.
    pub slot: Option<usize>,
    /// The stage's windows then, front to back (the window's own place among them).
    pub order: Vec<WindowId>,
    /// The stage's key window then.
    pub key: Option<WindowId>,
    /// The entry as a save writes it (`z` and `key` as of the stage then).
    pub window: SavedWindow,
}

/// The saved entry of a window whose app quit, kept for the file only.
#[derive(Debug, Clone, PartialEq)]
pub struct QuitWindow {
    /// Its Space.
    pub space: SpaceId,
    /// Its stage (may be gone from the model by now).
    pub stage: StageId,
    /// The stage's name.
    pub name: Option<String>,
    /// The stage's recency.
    pub last_active: u64,
    /// The stage's place in the strip when the window left.
    pub slot: Option<usize>,
    /// The entry, `quit_at` set.
    pub window: SavedWindow,
}

impl Model {
    /// Window `id`'s saved entry as it is now, for [`Model::keep_quit`] should its app be
    /// quitting; `None` for a child or a window not managed. Its frame is its restore frame
    /// (never a park frame).
    pub fn closing(&self, id: WindowId) -> Option<Closing> {
        let w = self.windows.get(&id)?;
        if w.meta.role != WindowRole::Standard {
            return None;
        }
        let (stage, pinned) = match w.home {
            Home::Stage(s) => (s, false),
            Home::Pinned => (self.active_stage_in(w.space)?, true),
        };
        let st = self.stage(stage)?;
        let d = self.display(w.meta.display);
        let frame = if d.parks(w.restore) || w.restore.is_degenerate() { clamp_to_visible(w.restore, d.visible) } else { w.restore };
        let z = st.windows.iter().position(|&x| x == id).unwrap_or(st.windows.len());
        let fp = fingerprint(&w.meta.title);
        Some(Closing {
            pid: w.meta.pid,
            id,
            space: st.space,
            stage,
            name: st.name.clone(),
            last_active: st.last_active,
            slot: self.slots.get(&st.space).and_then(|s| s.iter().position(|&x| x == stage)),
            order: st.windows.clone(),
            key: st.key,
            window: SavedWindow {
                bundle_id: w.meta.bundle_id.clone(),
                title: w.meta.title.clone(),
                frame,
                z,
                key: st.key == Some(id) && !pinned,
                pinned,
                role: SavedRole::Standard,
                original: w.original,
                window: Some(id.0),
                fp: (!fp.is_empty()).then_some(fp),
                pending: None,
                quit_at: None,
            },
        })
    }

    /// The windows `closing` (read by [`Model::closing`] as they left, in that order) belonged
    /// to an app that quit at `now` (Unix seconds): their entries are kept for the file. Each
    /// stage's front-to-back order and key are the ones its first window to leave saw (the
    /// others left one by one after it). Older entries are pruned.
    pub fn keep_quit(&mut self, closing: Vec<Closing>, now: u64) {
        let mut first: BTreeMap<StageId, (Vec<WindowId>, Option<WindowId>)> = BTreeMap::new();
        for c in &closing {
            first.entry(c.stage).or_insert_with(|| (c.order.clone(), c.key));
        }
        for c in closing {
            if c.window.bundle_id.is_empty() {
                continue;
            }
            let (order, key) = &first[&c.stage];
            let mut window = c.window;
            if let Some(z) = order.iter().position(|&x| x == c.id) {
                window.z = z;
            }
            window.key = !window.pinned && *key == Some(c.id);
            window.quit_at = Some(now);
            self.quit.retain(|q| q.window.window != window.window || q.window.bundle_id != window.bundle_id);
            self.quit.push(QuitWindow { space: c.space, stage: c.stage, name: c.name, last_active: c.last_active, slot: c.slot, window });
        }
        self.prune_quit(now);
    }

    /// Drops the entries of apps that quit longer than [`QUIT_KEEP`] before `now` (Unix
    /// seconds): a login does not reopen them.
    pub fn prune_quit(&mut self, now: u64) {
        let keep = QUIT_KEEP.as_secs();
        self.quit.retain(|q| q.window.quit_at.is_some_and(|t| t.saturating_add(keep) >= now));
    }

    /// App `bundle` shows a window again in this session: its entries from its quit go (its
    /// windows go by the usual rules now).
    pub(crate) fn forget_quit_of(&mut self, bundle: &str) {
        if !bundle.is_empty() {
            self.quit.retain(|q| q.window.bundle_id != bundle);
        }
    }

    /// The entries of windows whose app quit, kept for the file.
    pub fn quit_windows(&self) -> &[QuitWindow] {
        &self.quit
    }

    /// True when Space `space` has entries of windows whose app quit.
    pub(crate) fn space_has_quit(&self, space: SpaceId) -> bool {
        self.quit.iter().any(|q| q.space == space)
    }

    /// The Space's saved stages with the entries of its windows whose app quit
    /// ([`merge_quit`]); `live`: the ids of `stages` in order.
    pub(crate) fn with_quit(&self, space: SpaceId, live: &[StageId], stages: Vec<SavedStage>, active: Option<usize>, slots: Vec<usize>) -> (Vec<SavedStage>, Option<usize>, Vec<usize>) {
        let quit: Vec<&QuitWindow> = self.quit.iter().filter(|q| q.space == space).collect();
        merge_quit(live, stages, active, slots, &quit)
    }
}

/// One Space's saved stages (`stages`, their ids `live`, the active index and strip slots as
/// indices into them) with the entries `quit` of its windows whose app quit: an entry whose
/// stage is still there goes after that stage's windows; the others make the stages they had
/// again (name, recency, windows front to back, the first key only), each among the stages
/// where its id puts it (creation order) and in the strip where its card was.
pub(crate) fn merge_quit(live: &[StageId], stages: Vec<SavedStage>, active: Option<usize>, slots: Vec<usize>, quit: &[&QuitWindow]) -> (Vec<SavedStage>, Option<usize>, Vec<usize>) {
    if quit.is_empty() {
        return (stages, active, slots);
    }
    let mut stages = stages;
    let mut sorted: Vec<&QuitWindow> = quit.to_vec();
    sorted.sort_by_key(|q| (q.window.z, q.window.quit_at));
    let mut ghosts: BTreeMap<StageId, (SavedStage, Option<usize>)> = BTreeMap::new();
    for q in sorted {
        let (stage, has_key) = match live.iter().position(|&s| s == q.stage) {
            Some(i) => {
                let has_key = stages[i].windows.iter().any(|w| w.key);
                (&mut stages[i], has_key)
            }
            None => {
                let (g, slot) = ghosts.entry(q.stage).or_insert_with(|| {
                    (SavedStage { name: q.name.clone(), windows: Vec::new(), last_active: Some(q.last_active) }, q.slot)
                });
                *slot = match (*slot, q.slot) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                let has_key = g.windows.iter().any(|w| w.key);
                (g, has_key)
            }
        };
        let z = stage.windows.len();
        stage.windows.push(SavedWindow { z, key: q.window.key && !has_key, ..q.window.clone() });
    }
    // Each stage that left with its windows goes back before the first stage made after it.
    let mut ids: Vec<Option<StageId>> = live.iter().copied().map(Some).collect();
    let mut placed_slots: Vec<(usize, StageId)> = Vec::new();
    for (id, (stage, slot)) in ghosts {
        let at = ids.iter().position(|x| x.is_some_and(|x| x > id)).unwrap_or(ids.len());
        ids.insert(at, Some(id));
        stages.insert(at, stage);
        if let Some(slot) = slot {
            placed_slots.push((slot, id));
        }
    }
    let index_of = |id: StageId| ids.iter().position(|x| *x == Some(id));
    let active = active.and_then(|i| live.get(i)).and_then(|&id| index_of(id));
    let mut slots: Vec<usize> = slots.iter().filter_map(|&i| live.get(i)).filter_map(|&id| index_of(id)).collect();
    placed_slots.sort();
    for (slot, id) in placed_slots {
        if let Some(i) = index_of(id) {
            slots.insert(slot.min(slots.len()), i);
        }
    }
    (stages, active, slots)
}

#[cfg(test)]
mod tests;
