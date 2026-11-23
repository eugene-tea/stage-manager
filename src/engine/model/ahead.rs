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

use super::{Action, Home, Model, Observation, Pid, SpaceId, StageId, WindowId, WindowRole, WindowState};

/// What the platform knows of an app a Dock click is for, beyond the model, asked at the
/// press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppFacts {
    /// The app is hidden (⌘H).
    pub hidden: bool,
    /// The app has windows the model does not place that may be on a Desktop not shown
    /// (minimized there, or on a Desktop not visited since launch): where macOS goes then is
    /// not certain. Under an engine that reads the window server's Spaces, one listed on no
    /// Desktop but the one shown does not count (a tab not selected is listed on none).
    pub unplaced: bool,
    /// The app is the active (frontmost) one.
    pub active: bool,
    /// It has a window on screen out of the park zone in the window server's list (on any
    /// display).
    pub visible_here: bool,
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
    /// The desktop reveal the arrangement ended there (`previous` was its empty stage, gone
    /// once left): the stage the reveal had set aside. An undo sets the reveal up again.
    reveal_from: Option<StageId>,
}

/// A stage made active ahead on its Desktop, with its recency then: the moves sent for it
/// may be checked and sent again only while it stands ([`Model::ahead_holds`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AheadMark {
    /// The Desktop arranged.
    pub space: SpaceId,
    /// The stage made active there.
    pub stage: StageId,
    /// Its recency right after: every switch to or from it, every merge or gather into
    /// it, moves it on.
    recency: u64,
}

/// The Desktop an activation acts on as it stood before the activation's switch
/// ([`Model::before_activation`]): with the switch made, [`Model::switch_undo`] turns it into
/// an [`Ahead`] that [`Model::undo_ahead`] takes back once macOS has left that Desktop (the
/// app's own focus went to its window hidden on another one:
/// [`crate::engine::focus_elsewhere::hidden_focus`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeforeSwitch {
    /// The Desktop.
    space: SpaceId,
    /// Its active stage.
    active: StageId,
    /// Its strip slots.
    slots: Vec<StageId>,
    /// Each of its stages' key window and recency.
    stages: Vec<(StageId, Option<WindowId>, u64)>,
}

impl Model {
    /// The Desktop [`Model::app_activated`] acts on for `obs`, as it stands: `None` with a
    /// desktop reveal there (a switch ends it, and nothing sets it up again unseen) or no
    /// active stage.
    pub fn before_activation(&self, obs: &Observation) -> Option<BeforeSwitch> {
        let space = self.current_space_for(obs);
        if self.revealed.contains_key(&space) {
            return None;
        }
        let active = self.active.get(&space).copied()?;
        let stages = self.stages.iter().filter(|s| s.space == space).map(|s| (s.id, s.key, s.last_active)).collect();
        Some(BeforeSwitch { space, active, slots: self.slots.get(&space).cloned().unwrap_or_default(), stages })
    }

    /// The switch made since `before` as an arrangement [`Model::undo_ahead`] takes back: its
    /// Desktop gets its stage, key windows, recency and strip slots back, with moves only.
    /// `None` when no switch was made there (or the stage left is gone or empty).
    pub fn switch_undo(&self, before: BeforeSwitch) -> Option<Ahead> {
        let space = before.space;
        let stage = self.active.get(&space).copied()?;
        if stage == before.active || self.stage(before.active).is_none_or(|s| s.space != space || s.windows.is_empty()) {
            return None;
        }
        let was = |id: StageId| before.stages.iter().find(|(s, _, _)| *s == id).copied();
        let (_, key, recency) = was(stage)?;
        let (_, _, previous_recency) = was(before.active)?;
        let window = self.stage(stage).and_then(|s| s.key.or(s.windows.first().copied()))?;
        Some(Ahead {
            window,
            space,
            stage,
            previous: before.active,
            slots: before.slots,
            key,
            recency: (previous_recency, recency),
            reveal_from: None,
        })
    }

    /// The mark of `ahead` as the model stands now (right after it was arranged).
    pub fn ahead_mark(&self, ahead: &Ahead) -> AheadMark {
        AheadMark { space: ahead.space, stage: ahead.stage, recency: self.stage(ahead.stage).map_or(0, |s| s.last_active) }
    }

    /// True while the stage `mark` names is still the active one of its Desktop and no switch
    /// (to another stage and back, an undo, a merge, a reveal) touched it since: what was
    /// arranged ahead is what the model shows there.
    pub fn ahead_holds(&self, mark: &AheadMark) -> bool {
        self.active.get(&mark.space) == Some(&mark.stage)
            && self.stage(mark.stage).is_some_and(|s| s.last_active == mark.recency && s.space == mark.space)
    }

    /// True while the model still has window `id` as a move made ahead left it: parked or
    /// shown as `park` says, in stage `stage` (`None`: pinned).
    pub fn move_holds(&self, id: WindowId, park: bool, stage: Option<StageId>) -> bool {
        self.windows.get(&id).is_some_and(|w| w.parked == park) && self.stage_of(id) == stage
    }

    /// Makes the stage of window `id` (or of the window a child of it follows) the active
    /// stage of its Desktop, which no display shows now, with `id` as its key window: the
    /// stage active there is parked, `id`'s stage brought back (no raise, no focus: nothing
    /// acts on a Desktop not shown but moves). `obs`, when the Desktop is already on screen
    /// (the Space change notice), saves the stage left as a switch does: its windows' order
    /// as the window list shows them and its front window as its key. A desktop reveal there
    /// (its empty stage active) ends: the stage it set aside, or another, comes in front, as
    /// Apple's does for an app's activation on a revealed Desktop; an undo reveals again.
    /// `None`, changing nothing, when there is nothing to arrange or it could not be undone:
    /// an unknown, pinned or sticky window, a Desktop shown now, a stage already active, or
    /// a stage active there that holds no window and is no reveal's (leaving it would drop
    /// it).
    pub fn arrange_ahead(&mut self, id: WindowId, obs: Option<&Observation>) -> Option<(Ahead, Vec<Action>)> {
        let window = self.resolve_standard(id)?;
        let w = self.windows.get(&window)?;
        let Home::Stage(stage) = w.home else { return None };
        let space = w.space;
        if w.sticky || self.is_shown_space(space) {
            return None;
        }
        let previous = self.active.get(&space).copied()?;
        if previous == stage {
            return None;
        }
        // A wallpaper click there set every window aside: the window's app is activated
        // (a Dock click, ⌘Tab), and Apple's ends the reveal and shows the stage. Left, the
        // reveal's empty stage goes; an undo sets the reveal up again.
        let reveal_from = match self.revealed.get(&space).copied() {
            Some(r) if r.desktop == previous && self.reveal_stands_empty(space, previous) => Some(r.from),
            Some(_) => return None,
            None if self.stage(previous).is_none_or(|s| s.windows.is_empty() || s.space != space) => return None,
            None => None,
        };
        let last = |m: &Self, s: StageId| m.stage(s).map_or(0, |s| s.last_active);
        let ahead = Ahead {
            window,
            space,
            stage,
            previous,
            slots: self.slots.get(&space).cloned().unwrap_or_default(),
            key: self.stage(stage).and_then(|s| s.key),
            recency: (last(self, previous), last(self, stage)),
            reveal_from,
        };
        if let Some(s) = self.stage_mut(stage) {
            s.key = Some(window);
        }
        let mut actions = self.switch_in(space, previous, stage, obs);
        actions.retain(|a| !matches!(a, Action::Raise { .. } | Action::Focus { .. }));
        Some((ahead, actions))
    }

    /// A Dock click on app `pid`: the window macOS will show on another Desktop, or why no
    /// prediction is made. With "When switching to an application, switch to a Space with
    /// open windows for the application" on (else none), the app not hidden, in this order:
    ///
    /// - **The app is the active one** (`facts.active`): a click on its Dock item takes macOS
    ///   to another Desktop holding its windows (the owner's log of 10-13: VS Code and Mail,
    ///   each active with windows on two Desktops, every click went to the other one). Only
    ///   when exactly one other Desktop holds them, none unplaced: the key window of its
    ///   most recent stage there.
    /// - **An inactive app with a window on the Desktop shown** that macOS counts there (in
    ///   the model shown, or hidden by an engine whose hidden windows stay on their Desktop:
    ///   the Corner engine's park corner, [`Model::macos_counts_hidden`]; or on screen in the
    ///   window server's list): macOS activates it here, no switch (the owner's log of 10-13
    ///   09:55:50, VS Code inactive with a window on each Desktop, and the hands-free run
    ///   go10: the fixture app's focused window on the other Desktop did not take macOS
    ///   there). Under the Spaces engine a window in a stage Space is on no Desktop: an app
    ///   whose windows here are all hidden so goes nowhere when macOS counts none of its
    ///   windows elsewhere either (its activation switches here), and otherwise to the
    ///   Desktop of the window it counts (the owner's log of 11-15: Mail, a stage here and a
    ///   window shown on Desktop 4), unless the platform puts the stage's window home first
    ///   ([`Model::dock_home`]).
    /// - **The app's own answer** (`ax_window`: its focused, else main window, asked at the
    ///   press): macOS shows that window. A known window on another Desktop is the
    ///   prediction; one on the Desktop shown means no switch.
    /// - **No answer**: no prediction when the app has windows the model does not place;
    ///   else the key window of its most recent stage elsewhere.
    pub fn dock_arrival(&self, pid: Pid, facts: AppFacts, ax_window: Option<WindowId>) -> Result<WindowId, &'static str> {
        self.dock_gates(pid)?;
        if facts.hidden {
            return Err("the app is hidden");
        }
        let elsewhere = |id: WindowId| {
            self.resolve_standard(id)
                .and_then(|s| self.windows.get(&s))
                .filter(|w| w.meta.pid == pid && w.meta.role == WindowRole::Standard && !w.sticky && !self.is_shown_space(w.space))
                .map(|w| w.meta.id)
        };
        let mine = |id: &WindowId| self.windows.get(id).is_some_and(|w| w.meta.pid == pid);
        let key_of_recent = |space: Option<SpaceId>| {
            self.stages
                .iter()
                .filter(|s| !self.is_shown_space(s.space) && space.is_none_or(|sp| s.space == sp) && s.windows.iter().any(mine))
                .max_by_key(|s| s.last_active)
                .and_then(|s| s.key.filter(mine).or_else(|| s.windows.iter().copied().find(mine)))
                .and_then(elsewhere)
        };
        if facts.active {
            if facts.unplaced {
                return Err("the active app has windows not placed: which Desktop is next is not known");
            }
            let spaces = self.dock_spaces_elsewhere(pid);
            let [space] = spaces[..] else {
                return Err("the active app's windows are on several other Desktops");
            };
            return key_of_recent(Some(space)).ok_or("the active app has no stage on the other Desktop");
        }
        if self.windows.values().any(|w| w.meta.pid == pid && w.sticky) {
            return Err("the inactive app has a window on every Desktop: macOS activates it here");
        }
        let here = |w: &&WindowState| w.meta.pid == pid && self.is_shown_space(w.space);
        if facts.visible_here || self.windows.values().filter(here).any(|w| self.macos_counts(w)) {
            return Err("the inactive app has a window on the Desktop shown: macOS activates it here");
        }
        if self.windows.values().any(|w| here(&w)) && !self.counted_elsewhere(pid) && !facts.unplaced {
            return Err("the inactive app's windows are all hidden off every Desktop: macOS stays, its activation switches here");
        }
        if let Some(answer) = ax_window.and_then(|id| self.resolve_standard(id)).filter(|id| mine(id)) {
            return elsewhere(answer).ok_or("the app's focused window is on the Desktop shown");
        }
        if facts.unplaced {
            return Err("the app did not answer, and has windows not placed (minimized, or on a Desktop not visited)");
        }
        key_of_recent(None).ok_or("the app did not say which window, and no stage of it is known elsewhere")
    }

    /// True when macOS counts window `w` on its Desktop: shown, or hidden by an engine that
    /// leaves hidden windows there ([`Model::macos_counts_hidden`]).
    fn macos_counts(&self, w: &WindowState) -> bool {
        !w.parked || self.macos_counts_hidden()
    }

    /// True when macOS counts a standard window of app `pid` on a Desktop not shown (on every
    /// Desktop windows left out): where an activation of the app may take it.
    fn counted_elsewhere(&self, pid: Pid) -> bool {
        self.windows.values().any(|w| {
            w.meta.pid == pid && w.meta.role == WindowRole::Standard && !w.sticky && !self.is_shown_space(w.space) && self.macos_counts(w)
        })
    }

    /// The window of inactive app `pid` the platform puts on the Desktop shown at the press
    /// of a click on the app's Dock item, before the release (the Dock activates the app on
    /// the release, ~100 ms later), so macOS activates the app here as Apple's Stage Manager
    /// does, and the Corner engine, whose parked windows are on their Desktop: the key window
    /// of the app's most recent stage on a Desktop shown, the one its activation switches to
    /// ([`Model::app_activated`]). Only under an engine whose hidden windows macOS counts on
    /// no Desktop ([`Model::macos_counts_hidden`]: Spaces) and when the click would otherwise
    /// take macOS away: the app's windows here all hidden, one it counts on another Desktop,
    /// or windows not placed, which may be (the owner's log of 11-15: Mail, window 182 in a
    /// stage here, 295 shown on Desktop 4; macOS slid to 4 and back). `None` otherwise: an app hidden, active (its click goes to
    /// another Desktop, [`Model::dock_arrival`]), on screen here or on every Desktop, with no
    /// stage here, or with nothing counted elsewhere (macOS goes nowhere; its activation
    /// switches here, [`Model::activation_stays_here`]). The same window is put home while
    /// ⌘Tab's switcher selects the app (round cmd-tab, [`crate::engine::switcher`]): the
    /// switcher activates its app at ⌘'s release, as the Dock at the click's.
    pub fn dock_home(&self, pid: Pid, facts: AppFacts) -> Option<WindowId> {
        if self.macos_counts_hidden() || facts.hidden || facts.active || facts.visible_here {
            return None;
        }
        self.dock_gates(pid).ok()?;
        if self.windows.values().any(|w| w.meta.pid == pid && (w.sticky || (self.is_shown_space(w.space) && self.macos_counts(w)))) {
            return None;
        }
        // Windows not placed may be counted on a Desktop not visited: put home all the same.
        if !self.counted_elsewhere(pid) && !facts.unplaced {
            return None;
        }
        let has = |ids: &[WindowId]| ids.iter().any(|id| self.windows.get(id).is_some_and(|w| w.meta.pid == pid && w.meta.role == WindowRole::Standard));
        let stage = self
            .stages
            .iter()
            .filter(|s| self.is_shown_space(s.space) && !self.is_active(s.id) && has(&s.windows))
            .max_by_key(|s| s.last_active)?;
        self.activated_key(stage.id, pid, None)
    }

    /// True when an activation of app `pid` leaves macOS on the Desktop shown although the
    /// app's focused window is on another Desktop, hidden there off every Desktop: the app
    /// has a window in a stage of a Desktop shown, and macOS counts one of its windows here
    /// (`lent_here`: one put home for a Dock press, [`Model::dock_home`]) or none on a
    /// Desktop not shown. Asked only for a focused window the engine keeps off every Desktop
    /// (Spaces): macOS then goes nowhere, and the activation switches to the app's stage
    /// here, where the Corner engine (its parked windows counted on their Desktop) and
    /// Apple's end too. The owner's log of 11-15 06:31:28: VS Code, a window in a stage here,
    /// its focused window hidden on Desktop 4: the app went to Desktop 4 for it.
    pub fn activation_stays_here(&self, pid: Pid, lent_here: bool) -> bool {
        let stage_here = self.windows.values().any(|w| {
            w.meta.pid == pid
                && w.meta.role == WindowRole::Standard
                && !w.sticky
                && matches!(w.home, Home::Stage(_))
                && self.is_shown_space(w.space)
        });
        stage_here && (lent_here || !self.counted_elsewhere(pid))
    }

    /// The Desktops not shown that hold a standard window of app `pid` (on every Desktop
    /// windows left out).
    fn dock_spaces_elsewhere(&self, pid: Pid) -> Vec<SpaceId> {
        let mut spaces: Vec<SpaceId> = self
            .windows
            .values()
            .filter(|w| w.meta.pid == pid && w.meta.role == WindowRole::Standard && !w.sticky && !self.is_shown_space(w.space))
            .map(|w| w.space)
            .collect();
        spaces.sort();
        spaces.dedup();
        spaces
    }

    /// The part of [`Model::dock_arrival`] the model alone decides, asked before anything
    /// costs a query (an app with windows on the Desktop shown only is the common case): the
    /// setting on, a standard window of app `pid` on a Desktop not shown.
    pub fn dock_gates(&self, pid: Pid) -> Result<(), &'static str> {
        if !self.options.settings.switch_to_app_space {
            return Err("\"switch to a Space with open windows for the application\" is off");
        }
        if self.dock_spaces_elsewhere(pid).is_empty() {
            return Err("the app has no window on another Desktop");
        }
        Ok(())
    }

    /// The predicted Desktop arrived with the stage arranged ahead still active: raises its
    /// windows back to front and focuses the window the user went there for (on the Desktop
    /// shown now). When that window is no longer in the stage (closed or moved meanwhile), the
    /// stage that was active comes back instead, with its strip slots, as a switch does (the
    /// reveal it ended is set up again).
    /// Nothing when the Desktop is not shown or its active stage changed since.
    pub fn finish_ahead(&mut self, ahead: &Ahead) -> Vec<Action> {
        if !self.is_shown_space(ahead.space) || self.active.get(&ahead.space) != Some(&ahead.stage) {
            return Vec::new();
        }
        if !self.stage(ahead.stage).is_some_and(|s| s.windows.contains(&ahead.window)) {
            if let Some(from) = ahead.reveal_from {
                // Emptied, the stage active is a bare desktop already.
                if self.stage(ahead.stage).is_some_and(|s| s.windows.is_empty()) {
                    return Vec::new();
                }
                return self.reveal_ahead_again(ahead, from);
            }
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
        if self.is_shown_space(space) || self.active.get(&space) != Some(&ahead.stage) {
            return Vec::new();
        }
        if let Some(from) = ahead.reveal_from {
            let actions = self.reveal_ahead_again(ahead, from);
            self.restore_key(ahead);
            return actions;
        }
        if self.stage(ahead.previous).is_none_or(|s| s.space != space) {
            return Vec::new();
        }
        let mut actions = self.switch_in(space, ahead.stage, ahead.previous, None);
        actions.retain(|a| !matches!(a, Action::Raise { .. } | Action::Focus { .. }));
        self.restore_key(ahead);
        if let Some(s) = self.stage_mut(ahead.previous) {
            s.last_active = ahead.recency.0;
        }
        self.restore_slots(ahead);
        actions
    }

    /// The key window and recency `ahead`'s stage had before it was arranged.
    fn restore_key(&mut self, ahead: &Ahead) {
        if let Some(s) = self.stage_mut(ahead.stage) {
            s.key = ahead.key.filter(|k| s.windows.contains(k)).or(s.windows.first().copied());
            s.last_active = ahead.recency.1;
        }
    }

    /// The desktop reveal `ahead` ended, set up again on its Desktop: its stage parked into
    /// the strip, a fresh empty stage active, the reveal remembering `from` (the stage it had
    /// set aside, when still there; else `ahead`'s stage), the strip's slots as before. No
    /// raise or focus.
    fn reveal_ahead_again(&mut self, ahead: &Ahead, from: StageId) -> Vec<Action> {
        let space = ahead.space;
        let mut actions = Vec::new();
        self.park_stage(ahead.stage, &mut actions);
        let from = if self.stage(from).is_some_and(|s| s.space == space) { from } else { ahead.stage };
        self.reveal_with(space, ahead.stage, from);
        self.restore_slots(ahead);
        actions.push(Action::StateChanged);
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
