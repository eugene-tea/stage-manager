//! The windows a restart brought back on another Desktop than their saved one, as the model
//! keeps them for the list ([`crate::engine::desktop_notice`]): each one the pool placed in a
//! part (`pool.rs`) is noted with its saved Desktop, in the order they came back.

use super::{Action, Home, Model, Observation, SpaceId, WindowId, WindowRole};
use crate::engine::desktop_notice::{APPS_NAMED, DesktopRef, Entry};
use crate::engine::persist::SavedRole;

impl Model {
    /// Notes that window `id` came back from saved Desktop `saved`'s entry on another Desktop.
    pub(crate) fn note_away(&mut self, id: WindowId, saved: u64) {
        self.away.retain(|(w, _)| *w != id);
        self.away.push((id, saved));
    }

    /// The windows back on another Desktop than their saved one, in the order they came
    /// back: each noted one still managed and not on its saved Desktop, unless dismissed.
    pub fn desktop_notice(&self) -> Vec<Entry> {
        let listed: Vec<WindowId> = self.away_now().map(|(id, _)| id).collect();
        self.away_now()
            .filter_map(|(id, saved)| {
                let w = self.windows.get(&id)?;
                Some(Entry {
                    window: id,
                    pid: w.meta.pid,
                    bundle_id: w.meta.bundle_id.clone(),
                    title: w.meta.title.clone(),
                    saved: self.desktop_ref(saved, &listed),
                    now: self.desktop_ref(w.space.0, &listed),
                })
            })
            .collect()
    }

    /// The noted windows still away: managed, a standard window, not on their saved Desktop.
    fn away_now(&self) -> impl Iterator<Item = (WindowId, u64)> + '_ {
        self.away.iter().copied().filter(|(id, saved)| {
            self.windows.get(id).is_some_and(|w| w.meta.role == WindowRole::Standard && w.space.0 != *saved)
        })
    }

    /// Forgets the noted windows that are no longer away (gone, or shown on their saved
    /// Desktop: an entry never comes back once its window was where it belongs). True when
    /// one was.
    pub fn tidy_desktop_notice(&mut self) -> bool {
        let before = self.away.len();
        let keep: Vec<(WindowId, u64)> = self.away_now().collect();
        self.away = keep;
        self.away.len() != before
    }

    /// Dismisses the whole list (the menu's Dismiss): every entry leaves it.
    pub fn dismiss_desktop_notice(&mut self) {
        self.away.clear();
    }

    /// Brings listed window `id` forward when it is on the Desktop shown: its stage comes
    /// forward with it as the key window, or, its stage already in front, it takes the
    /// keyboard. Nothing for a window on another Desktop (acting there would make macOS
    /// switch Desktops), a pinned one or one not listed.
    pub fn show_noticed(&mut self, id: WindowId, obs: &Observation) -> Vec<Action> {
        if !self.away_now().any(|(w, _)| w == id) {
            return Vec::new();
        }
        let Some(w) = self.windows.get(&id) else { return Vec::new() };
        let (Home::Stage(stage), space) = (w.home, w.space) else { return Vec::new() };
        if !self.is_shown_space(space) {
            return Vec::new();
        }
        if let Some(s) = self.stage_mut(stage) {
            s.key = Some(id);
        }
        if self.is_active(stage) {
            return vec![Action::Focus { id }];
        }
        self.switch_to(stage, obs)
    }

    /// Desktop `id` as an entry names it: whether it is shown, and up to [`APPS_NAMED`] apps on
    /// it other than the `listed` windows' (its stages most recently used first; a saved
    /// Desktop not visited by its waiting entries, its active stage first).
    fn desktop_ref(&self, id: u64, listed: &[WindowId]) -> DesktopRef {
        let space = SpaceId(id);
        let mut apps: Vec<String> = Vec::new();
        let mut push = |b: &str| {
            if !b.is_empty() && apps.len() < APPS_NAMED && !apps.iter().any(|a| a == b) {
                apps.push(b.to_string());
            }
        };
        let mut stages: Vec<_> = self.stages_in(space).collect();
        stages.sort_by_key(|s| std::cmp::Reverse(s.last_active));
        for s in stages {
            for w in s.windows.iter().filter(|w| !listed.contains(w)).filter_map(|w| self.windows.get(w)) {
                push(&w.meta.bundle_id);
            }
        }
        if let Some(saved) = self.leftovers.iter().find(|l| l.id == id) {
            let mut order: Vec<usize> = (0..saved.stages.len()).collect();
            order.sort_by_key(|&i| (saved.active_index != Some(i), std::cmp::Reverse(saved.stages[i].last_active.unwrap_or(0))));
            for i in order {
                for w in saved.stages[i].windows.iter().filter(|w| w.role == SavedRole::Standard) {
                    push(&w.bundle_id);
                }
            }
        }
        DesktopRef { id, number: None, shown: self.is_shown_space(space), apps }
    }
}
