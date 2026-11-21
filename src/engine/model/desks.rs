//! The window server's Desktops, bound to the model's Spaces (design r3, slice 1: read and
//! shadow).
//!
//! The model names its Spaces by ids of its own ([`SpaceId`]), guessed at each arrival from
//! the windows on screen. Under the Spaces engine the window server's listing names the
//! Desktop shown on each screen too (`SLSCopyManagedDisplaySpaces`: a small id of the login
//! session and a uuid). Each arrival the model guessed is then given that Desktop
//! ([`Model::arrive_on`]): the first time, the Desktop and the Space are bound to each other
//! (one id, one Space); after that the guess either names the Space the Desktop is bound to
//! or it does not ([`Agree`]). Nothing here decides anything yet: the bindings only let the
//! platform say when the model's guess and the window server disagree
//! ([`crate::engine::desk_shadow`]), and are saved with their uuids (`state.json`'s `desks`)
//! to learn whether a uuid lasts over a restart.
//!
//! The table is kept right as Spaces change name ([`Model::relabel_space`]), go
//! ([`Model::prune_space`], a Desktop of a screen gone merged into the main one) and when the
//! engine falls back to one with no listing ([`Model::set_engine`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Action, DisplayId, Model, SpaceId, WindowId, WindowMeta};
use crate::engine::persist::LaunchHints;

/// A Desktop as the window server lists it: its id (one login session's) and its uuid
/// (empty for the Desktop that has none, the main screen's first in some setups).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Desk {
    pub id: u64,
    pub uuid: String,
}

/// A Desktop's binding: the model's Space for it and the uuid it was listed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub space: SpaceId,
    pub uuid: String,
}

/// A binding as `state.json` keeps it ([`crate::engine::persist::StateFile::desks`]): the
/// saved Space's id and the uuid of its Desktop. The window server's id is not saved: it
/// lasts one login session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDesk {
    pub space: u64,
    pub uuid: String,
}

/// What an arrival's guess said of the Desktop the window server listed
/// ([`Model::arrive_on`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agree {
    /// Neither the Desktop nor the Space was bound: bound now.
    New,
    /// The Desktop was bound to the Space the guess chose.
    Yes,
    /// The Desktop is bound to another Space, or the Space to another Desktop: the bindings
    /// stay as they were.
    No,
}

impl Agree {
    /// The log's word.
    pub fn word(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Yes => "yes",
            Self::No => "no",
        }
    }
}

impl Model {
    /// [`Model::arrive`], then the Desktop `desk` the window server lists on `display` now
    /// bound to the Space the arrival chose, or checked against it ([`Agree`], in
    /// [`super::ArrivalNote::desk`]). `None` (no listing: another engine, the listing not
    /// readable now): the arrival alone, as [`Model::arrive`]. The arrival runs as ever; the
    /// binding decides nothing.
    pub fn arrive_on(&mut self, display: DisplayId, desk: Option<&Desk>, live: &[WindowMeta], absent: &[WindowId], hints: &LaunchHints) -> Vec<Action> {
        let prev = self.current_space(display);
        let reused = self.space_is_empty(prev);
        let actions = self.arrive(display, live, absent, hints);
        if let Some(desk) = desk {
            let space = self.current_space(display);
            if reused && space == prev && self.desk_of_space(space) != Some(desk.id) {
                // An empty Space stands for no Desktop: the arrival took the one shown, empty,
                // for this Desktop (the model's way with a Desktop it never saw), and its
                // binding goes with what it held.
                self.unbind_space(space);
            }
            let agree = self.bind_desk(space, desk);
            self.last_arrival.desk = Some((desk.id, agree));
        }
        actions
    }

    /// Binds Desktop `desk` to `space` when neither is bound; else whether they are bound to
    /// each other. A uuid that changed under the same id is taken.
    pub(crate) fn bind_desk(&mut self, space: SpaceId, desk: &Desk) -> Agree {
        if let Some(b) = self.desks.get_mut(&desk.id) {
            if b.space != space {
                return Agree::No;
            }
            if b.uuid != desk.uuid {
                b.uuid = desk.uuid.clone();
            }
            return Agree::Yes;
        }
        if self.desk_of_space(space).is_some() {
            return Agree::No;
        }
        self.desks.insert(desk.id, Bound { space, uuid: desk.uuid.clone() });
        Agree::New
    }

    /// True once any Desktop is bound: the window server's listing was read and named one.
    pub fn keyed(&self) -> bool {
        !self.desks.is_empty()
    }

    /// Every binding: the window server's id, the Space, the uuid.
    pub fn desks(&self) -> &BTreeMap<u64, Bound> {
        &self.desks
    }

    /// The Desktop bound to `space`.
    pub fn desk_of_space(&self, space: SpaceId) -> Option<u64> {
        self.desks.iter().find(|(_, b)| b.space == space).map(|(id, _)| *id)
    }

    /// The Space Desktop `desk` is bound to.
    pub fn space_of_desk(&self, desk: u64) -> Option<SpaceId> {
        self.desks.get(&desk).map(|b| b.space)
    }

    /// The Desktop of window `id`: the one bound to its Space.
    pub fn desk_of(&self, id: WindowId) -> Option<u64> {
        self.space_of(id).and_then(|s| self.desk_of_space(s))
    }

    /// The Desktop the model says `display` shows: the one bound to its current Space.
    pub fn shown_desk(&self, display: DisplayId) -> Option<u64> {
        self.desk_of_space(self.current_space(display))
    }

    /// Whether the model's `space` and the window server's Desktop `desk` are the same one:
    /// `Some(true)` when they are bound to each other, `Some(false)` when either is bound to
    /// another, `None` when neither is bound (nothing to say).
    pub fn desk_agrees(&self, space: SpaceId, desk: u64) -> Option<bool> {
        match (self.desk_of_space(space), self.space_of_desk(desk)) {
            (None, None) => None,
            (Some(d), _) => Some(d == desk),
            (None, Some(_)) => Some(false),
        }
    }

    /// The bindings of Space `old` follow it to `new` ([`Model::relabel_space`]).
    pub(crate) fn relabel_desks(&mut self, old: SpaceId, new: SpaceId) {
        if old != new && self.desk_of_space(old).is_some() {
            // Never two Desktops for one Space: the one `new` had goes.
            self.unbind_space(new);
        }
        for b in self.desks.values_mut() {
            if b.space == old {
                b.space = new;
            }
        }
    }

    /// Space `space` is gone: its Desktop is bound to nothing.
    pub(crate) fn unbind_space(&mut self, space: SpaceId) {
        self.desks.retain(|_, b| b.space != space);
    }

    /// Forgets every binding (the engine falls back to one with no listing).
    pub(crate) fn unbind_all(&mut self) {
        self.desks.clear();
    }

    /// Takes the bindings `state.json` saved (`desks`): kept for the saved Spaces not visited
    /// yet, so a save before their visit writes them again. Nothing is bound from them yet
    /// (slice 6 of design r3 finds a Desktop again by its uuid).
    pub fn restore_desks(&mut self, saved: Vec<SavedDesk>) {
        self.saved_desks = saved;
    }

    /// The bindings as `state.json` keeps them: each bound Space that `saved` (the Spaces the
    /// file writes) holds, and the loaded ones of saved Spaces not visited yet.
    pub(crate) fn saved_desks(&self, saved: &[u64]) -> Vec<SavedDesk> {
        let mut out: Vec<SavedDesk> =
            self.desks.values().filter(|b| saved.contains(&b.space.0)).map(|b| SavedDesk { space: b.space.0, uuid: b.uuid.clone() }).collect();
        for d in &self.saved_desks {
            let waiting = self.leftovers.iter().any(|l| l.id == d.space);
            if waiting && saved.contains(&d.space) && !out.iter().any(|o| o.space == d.space) {
                out.push(d.clone());
            }
        }
        out.sort_by_key(|d| d.space);
        out
    }
}

#[cfg(test)]
mod tests;
