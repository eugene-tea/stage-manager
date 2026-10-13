//! Arranging a Desktop ahead of its arrival: a Dock click, ⌘Tab or activation that lands on
//! a window waiting in an inactive stage of another Desktop makes macOS slide that Desktop
//! in. Apple's Stage Manager shows it with **that window's stage** already active: the
//! Desktop slides in arranged. The platform learns which window (and so which Desktop) the
//! user goes to before the Desktop is recognized by its windows ([`Model::arrive`]): the
//! focus comes first, then the Space change notice, then the recognition ~60–700 ms later.
//!
//! [`Model::arrange_ahead`] makes that window's stage the active one of its Desktop at once
//! (the stage left parked, the window's stage brought back, the strip's slots swapped as for
//! a click), without raising or focusing anything: it is the same switch an arrival would
//! make, only earlier. It returns an [`Ahead`], which says what undoes it:
//!
//! - The predicted Desktop arrives: [`Model::finish_ahead`] raises the stage's windows and
//!   focuses the window (they are on the Desktop shown by then). Nothing moves at the
//!   arrival: the windows are already where the arrival would have put them. When the
//!   window is gone from its stage by then (closed, moved), the stage that was active comes
//!   back instead.
//! - Another Desktop arrives, the user acts on the Desktop shown, or another focus is held
//!   instead: [`Model::undo_ahead`] gives the Desktop back its previous stage, its key
//!   window, its strip slots and its recency. A Desktop shown by then is left as it is
//!   (what shows is what the user sees).
//!
//! Every window parked here is parked in the model too (and so in the platform's safety
//! registry) for as long as it is parked; the platform's arrival matching
//! ([`Model::arrive`]: a parked window found shown is parked again, a shown one found parked
//! is brought back) covers a move macOS did not carry out on a Desktop not shown.

use super::{Action, Home, Model, Observation, Pid, SpaceId, StageId, WindowId, WindowRole};

/// What the platform knows of an app a Dock click is for, beyond the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppFacts {
    /// The app is hidden (⌘H).
    pub hidden: bool,
    /// The app has windows the model does not place (minimized, or on a Desktop not visited
    /// since launch): where macOS goes then is not certain.
    pub unplaced: bool,
}

/// A stage switch made on a Desktop before it was recognized, and what undoes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ahead {
    /// The window the user went there for (a standard window).
    pub window: WindowId,
    /// Its Desktop: the one predicted to arrive.
    pub space: SpaceId,
    /// Its stage, made active there.
    pub stage: StageId,
    /// The stage that was active there, parked.
    pub previous: StageId,
    /// The Desktop's strip slots before.
    slots: Vec<StageId>,
    /// The stage's key window before.
    key: Option<WindowId>,
    /// Both stages' recency before (`previous`, `stage`).
    recency: (u64, u64),
}

impl Model {
    /// Makes the stage of window `id` (or of the window a child of it follows) the active
    /// stage of its Desktop, which no display shows now, with `id` as its key window: the
    /// stage active there is parked, `id`'s stage brought back (no raise, no focus: nothing
    /// acts on a Desktop not shown but moves). `obs`, when the Desktop is already on screen
    /// (the Space change notice), saves the stage left as a switch does: its windows' order
    /// as the window list shows them and its front window as its key. `None`, changing nothing, when there is
    /// nothing to arrange or it could not be undone: an unknown, pinned or sticky window, a
    /// Desktop shown now, a stage already active, a desktop reveal there, or a stage active
    /// there that holds no window (leaving it would drop it).
    pub fn arrange_ahead(&mut self, id: WindowId, obs: Option<&Observation>) -> Option<(Ahead, Vec<Action>)> {
        let window = self.resolve_standard(id)?;
        let w = self.windows.get(&window)?;
        let Home::Stage(stage) = w.home else { return None };
        let space = w.space;
        if w.sticky || self.is_shown_space(space) || self.revealed.contains_key(&space) {
            return None;
        }
        let previous = self.active.get(&space).copied()?;
        if previous == stage || self.stage(previous).is_none_or(|s| s.windows.is_empty() || s.space != space) {
            return None;
        }
        let last = |m: &Self, s: StageId| m.stage(s).map_or(0, |s| s.last_active);
        let ahead = Ahead {
            window,
            space,
            stage,
            previous,
            slots: self.slots.get(&space).cloned().unwrap_or_default(),
            key: self.stage(stage).and_then(|s| s.key),
            recency: (last(self, previous), last(self, stage)),
        };
        if let Some(s) = self.stage_mut(stage) {
            s.key = Some(window);
        }
        let mut actions = self.switch_in(space, previous, stage, obs);
        actions.retain(|a| !matches!(a, Action::Raise { .. } | Action::Focus { .. }));
        Some((ahead, actions))
    }

    /// A Dock click on app `pid`: the window macOS will show on another Desktop, or why no
    /// prediction is made. macOS switches Desktops only with "When switching to an
    /// application, switch to a Space with open windows for the application" on, and only
    /// when the app has no window on the Desktop shown; it shows the app's main window,
    /// `ax_focused` (its focused window, asked now) when that waits on another Desktop, else
    /// its frontmost window there in the window server's order (`z_order`, front to back,
    /// every Desktop). On the safe side, no prediction for a hidden app or one with windows
    /// the model does not place (`facts`).
    pub fn dock_arrival(
        &self,
        pid: Pid,
        facts: AppFacts,
        ax_focused: Option<WindowId>,
        z_order: &[WindowId],
    ) -> Result<WindowId, &'static str> {
        if !self.options.settings.switch_to_app_space {
            return Err("\"switch to a Space with open windows for the application\" is off");
        }
        if facts.hidden {
            return Err("the app is hidden");
        }
        if facts.unplaced {
            return Err("the app has windows not placed (minimized, or on a Desktop not visited)");
        }
        let mine: Vec<&super::WindowState> = self.windows.values().filter(|w| w.meta.pid == pid).collect();
        if mine.iter().any(|w| w.sticky || self.is_shown_space(w.space)) {
            return Err("the app has a window on the Desktop shown");
        }
        let elsewhere = |id: WindowId| {
            self.resolve_standard(id)
                .and_then(|s| self.windows.get(&s))
                .filter(|w| w.meta.pid == pid && w.meta.role == WindowRole::Standard && !self.is_shown_space(w.space))
                .map(|w| w.meta.id)
        };
        if !mine.iter().any(|w| w.meta.role == WindowRole::Standard) {
            return Err("the app has no managed window");
        }
        ax_focused
            .and_then(elsewhere)
            .or_else(|| z_order.iter().find_map(|id| elsewhere(*id)))
            .ok_or("none of the app's windows is in the window server's order")
    }

    /// The predicted Desktop arrived with the stage arranged ahead still active: raises its
    /// windows back to front and focuses the window the user went there for (on the Desktop
    /// shown now). When that window is no longer in the stage (closed or moved meanwhile), the
    /// stage that was active comes back instead, with its strip slots, as a switch does.
    /// Nothing when the Desktop is not shown or its active stage changed since.
    pub fn finish_ahead(&mut self, ahead: &Ahead) -> Vec<Action> {
        if !self.is_shown_space(ahead.space) || self.active.get(&ahead.space) != Some(&ahead.stage) {
            return Vec::new();
        }
        if !self.stage(ahead.stage).is_some_and(|s| s.windows.contains(&ahead.window)) {
            if self.stage(ahead.previous).is_none_or(|s| s.space != ahead.space) {
                return Vec::new();
            }
            let actions = self.switch_in(ahead.space, ahead.stage, ahead.previous, None);
            self.restore_slots(ahead);
            return actions;
        }
        let mut actions: Vec<Action> = self.front_order(ahead.stage).into_iter().map(|id| Action::Raise { id }).collect();
        actions.push(Action::Focus { id: ahead.window });
        actions
    }

    /// The prediction was wrong (another Desktop arrived, the user acted on the Desktop
    /// shown, another focus came): the Desktop gets back the stage that was active, its strip
    /// slots, recency and the stage's key window. Nothing when the Desktop shows now (what
    /// shows is right), or its stages changed since (its active stage is another one, or the
    /// stage that was active is gone).
    pub fn undo_ahead(&mut self, ahead: &Ahead) -> Vec<Action> {
        let space = ahead.space;
        if self.is_shown_space(space)
            || self.active.get(&space) != Some(&ahead.stage)
            || self.stage(ahead.previous).is_none_or(|s| s.space != space)
        {
            return Vec::new();
        }
        let mut actions = self.switch_in(space, ahead.stage, ahead.previous, None);
        actions.retain(|a| !matches!(a, Action::Raise { .. } | Action::Focus { .. }));
        if let Some(s) = self.stage_mut(ahead.stage) {
            s.key = ahead.key.filter(|k| s.windows.contains(k)).or(s.windows.first().copied());
            s.last_active = ahead.recency.1;
        }
        if let Some(s) = self.stage_mut(ahead.previous) {
            s.last_active = ahead.recency.0;
        }
        self.restore_slots(ahead);
        actions
    }

    /// The Desktop's strip slots as they were before `ahead`, when the same stages fill them.
    fn restore_slots(&mut self, ahead: &Ahead) {
        let now: Vec<StageId> = self.slots.get(&ahead.space).cloned().unwrap_or_default();
        let same = |a: &[StageId], b: &[StageId]| a.len() == b.len() && a.iter().all(|s| b.contains(s));
        if same(&now, &ahead.slots) {
            self.slots.insert(ahead.space, ahead.slots.clone());
        }
        self.normalize_slots();
    }

    /// A stage's windows in the order a switch raises them: back to front, each followed by
    /// its children ([`Model::show_stage`]).
    pub(crate) fn front_order(&self, stage: StageId) -> Vec<WindowId> {
        let Some(s) = self.stage(stage) else { return Vec::new() };
        let mut order = Vec::new();
        for &m in s.windows.iter().rev() {
            order.push(m);
            order.extend(self.children_of(m));
        }
        order
    }
}
