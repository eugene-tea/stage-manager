//! Hidden apps (⌘H, Hide Others, an app hiding itself), as Apple's Stage Manager treats them
//! (measured 11-09, macOS 27.2):
//!
//! - **Hidden**, an app's windows show on no card. A stage of the hidden app alone leaves the
//!   strip entirely: the most recent stage out of view takes its slot, in place
//!   ([`Model::normalize_slots`]). When that stage was the one in front, the most recent
//!   stage comes in front, as a card click would bring it, and the hidden stage's slot (the
//!   one the switch gave it) is the slot it leaves.
//! - **Unhidden** without an activation, the stage comes back as a card in the slot it left,
//!   not in front; the stage in front stays. The card that took the slot goes out of view
//!   when the strip is full. An activation with the unhide (⌘-Tab, a Dock click) brings the
//!   stage in front as any activation does: that is the activation's own event.
//!
//! A stage shared with other apps stays: it shows the others' windows meanwhile.

use super::{Action, Model, Observation, Pid, StageId, WindowRole};

impl Model {
    /// True while app `pid` is hidden ([`Model::app_hidden`]).
    pub fn app_is_hidden(&self, pid: Pid) -> bool {
        self.hidden_apps.contains(&pid)
    }

    /// True when every standard window of `stage` (at least one) belongs to a hidden app: it
    /// shows no card.
    pub(crate) fn hidden_stage(&self, stage: StageId) -> bool {
        if self.hidden_apps.is_empty() {
            return false;
        }
        let Some(s) = self.stage(stage) else { return false };
        let mut standard = s
            .windows
            .iter()
            .filter_map(|w| self.windows.get(w))
            .filter(|w| w.meta.role == WindowRole::Standard)
            .peekable();
        standard.peek().is_some() && standard.all(|w| self.hidden_apps.contains(&w.meta.pid))
    }

    /// App `pid` was hidden: its windows leave the cards, a stage of its alone leaves the
    /// strip (its slot to the most recent stage out of view), and where such a stage is in
    /// front on a Desktop shown, the most recent stage of that Desktop comes in front
    /// ([`Model::switch_to`]). Nothing when it was hidden already or has no window here.
    pub fn app_hidden(&mut self, pid: Pid, obs: &Observation) -> Vec<Action> {
        self.apps_hidden(&[pid], obs)
    }

    /// [`Model::app_hidden`] for every app hidden by now (`pids`: the one whose notice came and
    /// those macOS says are hidden too; Hide Others hides them together, each with a notice of
    /// its own): all are hidden before a stage is picked to come in front, so that stage is
    /// never one of an app just hidden, whose focus would bring the app straight back.
    /// Nothing when none of them is newly hidden with a window here.
    pub fn apps_hidden(&mut self, pids: &[Pid], obs: &Observation) -> Vec<Action> {
        let mut news = false;
        for &pid in pids {
            news |= self.hidden_apps.insert(pid) && self.has_app_window(pid);
        }
        if !news {
            return Vec::new();
        }
        let mut actions = Vec::new();
        let mut shown: Vec<_> = self.current.values().copied().collect();
        shown.sort_by_key(|s| s.0);
        shown.dedup();
        for space in shown {
            let Some(active) = self.active_stage_in(space) else { continue };
            if !self.hidden_stage(active) {
                continue;
            }
            let recent = self
                .stages
                .iter()
                .filter(|s| s.space == space && s.id != active && self.strip_shows(s.id))
                .max_by_key(|s| s.last_active)
                .map(|s| s.id);
            if let Some(target) = recent {
                actions.extend(self.switch_to(target, obs));
            }
        }
        self.normalize_slots();
        if !actions.contains(&Action::StateChanged) {
            actions.push(Action::StateChanged);
        }
        actions
    }

    /// App `pid` was unhidden: its windows show on their cards again, and each stage of its
    /// that left the strip when hidden comes back as a card in the slot it left (the card
    /// there goes out of view when the strip is full). Nothing comes in front: an activation
    /// with the unhide is its own event. Nothing when it was not hidden or has no window here.
    pub fn app_unhidden(&mut self, pid: Pid) -> Vec<Action> {
        if !self.hidden_apps.remove(&pid) || !self.has_app_window(pid) {
            return Vec::new();
        }
        let mut back: Vec<(usize, StageId)> = self
            .hidden_slots
            .iter()
            .filter(|&(&s, _)| !self.hidden_stage(s) && self.strip_eligible(s))
            .map(|(&s, &i)| (i, s))
            .collect();
        back.sort();
        // A slot noted for a stage that shows again otherwise (in front already: the
        // activation came before this notice) is a slot it left no more.
        let stale: Vec<StageId> = self.hidden_slots.keys().copied().filter(|s| !self.hidden_stage(*s)).collect();
        for stage in stale {
            if !back.iter().any(|b| b.1 == stage) {
                self.hidden_slots.remove(&stage);
            }
        }
        for &(i, stage) in &back {
            self.hidden_slots.remove(&stage);
            let Some(space) = self.stage(stage).map(|s| s.space) else { continue };
            let capacity = self.capacity_of(space);
            let slots = self.slots.entry(space).or_default();
            if capacity == 0 || slots.contains(&stage) {
                continue;
            }
            if slots.len() < capacity {
                slots.insert(i.min(slots.len()), stage);
            } else {
                let at = i.min(slots.len() - 1);
                slots[at] = stage;
            }
        }
        self.normalize_slots();
        vec![Action::StateChanged]
    }

    /// True when the model manages a window of app `pid`.
    fn has_app_window(&self, pid: Pid) -> bool {
        self.windows.values().any(|w| w.meta.pid == pid)
    }

    /// App `pid` quit: forgotten as hidden, with nothing to do on screen.
    pub fn forget_hidden_app(&mut self, pid: Pid) {
        self.hidden_apps.remove(&pid);
    }
}
