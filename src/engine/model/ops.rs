//! Model operations. Each returns the actions the platform executes in order: parks first,
//! then restore frames, then raises back to front, then focus, then `StateChanged`.

use super::{
    Action, Display, DisplayId, Home, Model, NativeTarget, Observation, Pid, Reveal, SpaceId, StripOrder,
    StageId, Target, WindowId, WindowMeta, WindowRole, WindowState,
};
use crate::engine::geometry::{Point, Rect};
use crate::engine::park::clamp_to_visible;
use crate::engine::placement::{CASCADE_OFFSET, cascade, center};
use crate::engine::rules::{Decision, Placement, RuleAction};
use crate::engine::settings::AppWindows;

impl Model {
    /// A window appeared on the Space its display shows now (the platform offers a window
    /// only once it is on screen). `decision` comes from the rules; it is ignored for children, which
    /// follow their parent (a child whose parent is known but unmanaged stays unmanaged; one
    /// with no known parent joins the active stage). Duplicate notifications for a known
    /// `WindowId` only refresh its title.
    ///
    /// Standard windows: `native` follows Apple ([`Model::native_target`]): a window of an
    /// app with a window in the active stage (or pinned) joins the active stage; a window of
    /// an app whose windows sit only in parked stages joins the most recent of them and
    /// switches there (with one-at-a-time grouping it opens a stage of its own instead, the
    /// app's newest window shown alone); a window of an app with no managed window opens a
    /// new stage and switches to it (an empty active stage is simply used). `current` joins
    /// the active stage at the front; `new` creates a stage and switches to it;
    /// `stage:<name>` joins that stage (created if missing) and switches to it when the window
    /// is focused in `obs`, else parks it there; `float` pins it; `ignore` leaves it
    /// unmanaged. Placement (`center`, `cascade` from the stage's front window) applies before
    /// any switch.
    ///
    /// `[stages] new_apps_join_current` ([`Decision::new_app_joins_current`]): a window of an
    /// app with no managed standard window, which `native` gives a stage of its own, joins the
    /// active stage instead, unless it goes back to a saved stage (below) or the restore window
    /// is open (apps macOS reopens at a login, or that open by themselves after a launch, keep
    /// Apple's rule: piling them into one stage would undo the restore).
    ///
    /// Before Apple's default (`native`), a saved window still pending on this Space is looked
    /// for ([`Model::place_from_pending`]): a confident match goes back to its saved stage and
    /// frame; into an inactive stage it is parked there with no switch while the restore
    /// window is open, and switched to after it, as `native` joins an app's stage; a
    /// provisional Space first becomes the saved Desktop the window belongs to. A window
    /// another rule places takes its confident match out of the pending ones.
    ///
    /// While the login burst settles after a restart ([`Model::settling`]) no window switches
    /// stages: one whose stage would come forward is parked in it instead (its card shows).
    /// But while the saved active stage is still empty, waiting for its windows, a window
    /// opening a stage of its own takes the front ([`Model::front_waits_empty`]): nothing is
    /// shown to animate away, and the screen is not left empty while a slow app opens; the
    /// saved stage waits, inactive and with no card, for its windows.
    pub fn add_window(
        &mut self,
        meta: WindowMeta,
        decision: &Decision,
        obs: &Observation,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(w) = self.windows.get_mut(&meta.id) {
            w.meta.title = meta.title;
            return actions;
        }
        if decision.action == RuleAction::Ignore {
            return actions;
        }
        let display = self.known_display(meta.display);
        let space = self.current_space(display);
        let mut meta = meta;
        meta.display = display;
        let id = meta.id;

        if let WindowRole::Child { parent } = meta.role {
            let (home, space) = match parent {
                Some(p) => match self.windows.get(&p) {
                    Some(pw) => {
                        meta.display = pw.meta.display;
                        (pw.home, pw.space)
                    }
                    None => return actions,
                },
                None => (Home::Stage(self.ensure_active(space)), space),
            };
            let restore = self.safe_restore(meta.frame, meta.display);
            let born_parked = restore != meta.frame;
            self.windows.insert(
                id,
                WindowState {
                    meta,
                    home,
                    restore,
                    parked: false,
                    original: None,
                    space,
                    sticky: false,
                },
            );
            match home {
                Home::Stage(s) if !self.is_active(s) => self.park_window(id, &mut actions),
                _ if born_parked => actions.push(Action::SetFrame { id, frame: restore }),
                _ => {}
            }
            actions.push(Action::StateChanged);
            return actions;
        }

        // Its app is back in this session: the entries kept from its quit go (round 8H).
        self.forget_quit_of(&meta.bundle_id);
        if decision.action == RuleAction::Native {
            if self.place_from_pending(&meta, space, obs, &mut actions) {
                self.normalize_slots();
                actions.push(Action::StateChanged);
                return actions;
            }
        } else {
            self.forget_pending_match(&meta, space, &mut actions);
        }
        if decision.action == RuleAction::Native && self.restore.restoring {
            // Matched no saved window: Apple's rule (counted for the restore's line).
            self.note_by_apple([&meta]);
        }
        // Adoption may have renamed the Space.
        let space = self.current_space(display);
        let active = self.ensure_active(space);
        // (home, a stage was created for it, switch to its stage when that is inactive)
        let (home, created, follow) = match &decision.action {
            RuleAction::Float => (Home::Pinned, false, false),
            RuleAction::Current | RuleAction::Ignore => (Home::Stage(active), false, false),
            RuleAction::New => (Home::Stage(self.create_stage(space, None)), true, true),
            RuleAction::Stage(name) => {
                let focused = obs.focused == Some(id);
                match self.find_named(space, name) {
                    Some(s) => (Home::Stage(s), false, focused),
                    None => (
                        Home::Stage(self.create_stage(space, Some(name.clone()))),
                        true,
                        focused,
                    ),
                }
            }
            RuleAction::Native => match self.native_target(meta.pid, display) {
                NativeTarget::Join(s) => (Home::Stage(s), false, true),
                NativeTarget::Fresh if self.stage_is_empty(active) => {
                    (Home::Stage(active), false, false)
                }
                NativeTarget::Fresh if self.new_app_joins(&meta, decision) => {
                    (Home::Stage(active), false, false)
                }
                NativeTarget::Fresh => (Home::Stage(self.create_stage(space, None)), true, true),
            },
        };
        let front_stage = match home {
            Home::Stage(s) if !created => s,
            _ => active,
        };
        let front = self
            .stage(front_stage)
            .and_then(|s| s.windows.first().copied())
            .and_then(|f| {
                obs.frames
                    .get(&f)
                    .copied()
                    .or(self.windows.get(&f).map(|w| w.restore))
            });
        let visible = self.display(display).visible;
        let placed = match decision.placement {
            Placement::AsIs => meta.frame,
            Placement::Center => center(meta.frame, visible),
            Placement::Cascade => cascade(meta.frame, front, visible, CASCADE_OFFSET),
        };
        let restore = self.safe_restore(placed, display);
        self.windows.insert(
            id,
            WindowState {
                meta,
                home,
                restore,
                parked: false,
                original: None,
                space,
                sticky: false,
            },
        );
        if let Home::Stage(s) = home
            && let Some(stage) = self.stage_mut(s)
        {
            stage.windows.insert(0, id);
            stage.key = Some(id);
        }
        self.fit_window(id);
        let restore = self.windows[&id].restore;

        let target_active = match home {
            Home::Stage(s) => self.is_active(s),
            Home::Pinned => true,
        };
        if target_active {
            // `restore` differs from the live frame when the window was born in a park zone,
            // was placed by a rule, or was fitted clear of the strip.
            if restore != self.windows[&id].meta.frame {
                actions.push(Action::SetFrame { id, frame: restore });
            }
        } else if let Home::Stage(s) = home {
            // While the login settles a new stage takes the front only from an empty saved
            // active stage still waiting for its windows (the review of 8F, M4).
            if follow && (!self.settling() || (created && self.front_waits_empty(space))) {
                actions.extend(self.switch_to(s, obs));
                return actions;
            }
            self.park_window(id, &mut actions);
            self.strip_enter(space, s);
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// A window is gone (destroyed). It leaves its stage; its children are forgotten too (any
    /// that were parked are first restored, in case they outlive it). An inactive stage left
    /// with no standard window disappears; the active stage may become empty.
    ///
    /// The window itself is not restored even when parked: it no longer exists, and a frame
    /// sent to it would only fail and be retried as a window left in the corner. A window that
    /// still exists and leaves the model goes through [`Model::window_left_space`], which
    /// brings it back first (the module's invariant; Fable E9).
    pub fn remove_window(&mut self, id: WindowId) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(w) = self.windows.get(&id) else {
            return actions;
        };
        let home = w.home;
        for c in self.children_of(id) {
            if self.windows.get(&c).is_some_and(|w| w.parked) {
                self.restore_window(c, &mut actions);
            }
            self.windows.remove(&c);
        }
        self.windows.remove(&id);
        self.restored.remove(&id);
        self.pooled.remove(&id);
        // A window gone no longer vouches for a saved Desktop.
        for votes in self.adopt_votes.values_mut() {
            votes.retain(|v| v.1 != id);
        }
        self.adopt_votes.retain(|_, v| !v.is_empty());
        if let Home::Stage(s) = home {
            if let Some(stage) = self.stage_mut(s) {
                stage.windows.retain(|&w| w != id);
                if stage.key == Some(id) {
                    stage.key = stage.windows.first().copied();
                }
            }
            self.drop_if_dead(s, &mut actions);
        }
        self.prune_restore_refs();
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Switches the target's Space to `target`: saves the active stage's live frames, z-order
    /// and key window from `obs`, parks it, restores the target's frames, raises its windows
    /// back to front and focuses its key window last. A no-op for the active stage, an
    /// unknown one, or one on a Space its display does not show now (acting there would make
    /// macOS switch Spaces). Pinned windows are untouched. In the strip the stage left behind
    /// takes the target's slot (or enters the strip if the target had none).
    pub fn switch_to(&mut self, target: StageId, obs: &Observation) -> Vec<Action> {
        let Some(space) = self.stage(target).map(|s| s.space) else {
            return Vec::new();
        };
        if !self.is_shown_space(space) {
            return Vec::new();
        }
        let current = self.ensure_active(space);
        if current == target {
            return Vec::new();
        }
        self.switch_in(space, current, target, Some(obs))
    }

    /// The body of a switch on `space` from its active stage `current` to `target`: `obs`
    /// saves the live state of the stage left (none for a Desktop not shown, whose windows
    /// keep the frames last recorded: [`Model::arrange_ahead`]).
    pub(crate) fn switch_in(
        &mut self,
        space: SpaceId,
        current: StageId,
        target: StageId,
        obs: Option<&Observation>,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        // Where the user saw the target's card, before anything changes (V2).
        let clicked = self.slot_showing(space, target);
        self.revealed.remove(&space);
        if let Some(obs) = obs {
            self.capture(current, obs);
        }
        self.park_stage(current, &mut actions);
        let t = self.tick();
        if let Some(s) = self.stage_mut(current) {
            s.last_active = t;
        }
        self.active.insert(space, target);
        let t = self.tick();
        if let Some(s) = self.stage_mut(target) {
            s.last_active = t;
        }
        if self.strip_eligible(current) {
            self.strip_swap(space, current, target, clicked);
        } else if let Some(slots) = self.slots.get_mut(&space) {
            slots.retain(|&s| s != target);
        }
        self.drop_if_dead(current, &mut actions);
        self.show_stage(target, &mut actions);
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Switches to stage `n` (1-based, creation order) of the current Space; a no-op when
    /// there is none.
    pub fn switch_to_number(&mut self, n: usize, obs: &Observation) -> Vec<Action> {
        let space = self.current_space_for(obs);
        match self.stage_by_number(space, n) {
            Some(s) => self.switch_to(s, obs),
            None => Vec::new(),
        }
    }

    /// Sends a standard window (a child stands for its parent) and its children to another
    /// stage, at the front, as its key window. The view does not switch: if the target is
    /// inactive the window is parked; if the source is the active stage and the window was
    /// focused, the source's next front window takes focus. A pinned window is unpinned.
    /// Stages are those of the window's Space: `Target::Number(n)` creates a stage only when
    /// `n == count + 1`, else is a no-op for a missing stage. Sending the last window out
    /// leaves the active stage empty.
    pub fn send_window(&mut self, id: WindowId, target: Target, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(id) = self.resolve_standard(id) else {
            return actions;
        };
        let src = self.windows[&id].home;
        let space = self.windows[&id].space;
        if !self.is_shown_space(space) {
            // Its Desktop is not shown: moving, raising or focusing it there could make macOS
            // switch Desktops (L2; only `ctl send` by window id can ask this).
            return actions;
        }
        let count = self.stages_in(space).count();
        let dest = match target {
            Target::Number(n) if n >= 1 && n <= count => match self.stage_by_number(space, n) {
                Some(s) => s,
                None => return actions,
            },
            Target::Number(n) if n == count + 1 => self.create_stage(space, None),
            Target::Number(_) => return actions,
            Target::New => self.create_stage(space, None),
            Target::Named(name) => match self.find_named(space, &name) {
                Some(s) => s,
                None => self.create_stage(space, Some(name)),
            },
        };
        let src_display = self.space_display(space);
        if src == Home::Stage(dest) {
            return actions;
        }
        let mut group = vec![id];
        group.extend(self.children_of(id));
        for &w in &group {
            if let Some(&f) = obs.frames.get(&w) {
                self.record_frame(w, f);
            }
        }
        let was_focused = obs.focused.and_then(|f| self.resolve_standard(f)) == Some(id);
        if let Home::Stage(s) = src
            && let Some(stage) = self.stage_mut(s)
        {
            stage.windows.retain(|&w| w != id);
            if stage.key == Some(id) {
                stage.key = stage.windows.first().copied();
            }
        }
        let dest_display = self.stage(dest).map_or(src_display, |s| s.display);
        if let Some(stage) = self.stage_mut(dest) {
            stage.windows.insert(0, id);
            stage.key = Some(id);
        }
        self.set_home(id, Home::Stage(dest), dest_display);

        let dest_active = self.is_active(dest);
        if dest_active {
            let parked: Vec<WindowId> = group
                .iter()
                .copied()
                .filter(|w| self.windows.get(w).is_some_and(|w| w.parked))
                .collect();
            for &w in &parked {
                self.restore_window(w, &mut actions);
            }
            for &w in &parked {
                actions.push(Action::Raise { id: w });
            }
        } else {
            for &w in &group {
                self.park_window(w, &mut actions);
            }
        }
        if let Home::Stage(s) = src {
            if self.is_active(s)
                && was_focused
                && !dest_active
                && let Some(next) = self.stage(s).and_then(|st| st.windows.first().copied())
            {
                actions.push(Action::Focus { id: next });
            }
            self.drop_if_dead(s, &mut actions);
        }
        if !dest_active {
            self.strip_enter(space, dest);
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Moves every window of `stage` into the active stage of its Space (a shift-click on a
    /// strip card). The moved windows go in front, keep their order, are restored and raised
    /// back to front, and the moved stage's key window is focused last. The emptied stage
    /// goes away unless it is declared in config. Its windows still awaited after a restart
    /// move with it, in front, in saved order (R5). A no-op for the active stage, an unknown
    /// one or one on a Space not shown now. Pinned windows are untouched.
    pub fn merge_into_active(&mut self, stage: StageId, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(src) = self.stage(stage) else {
            return actions;
        };
        let display = src.display;
        let space = src.space;
        let members = src.windows.clone();
        let src_key = src.key;
        if !self.is_shown_space(space) {
            return actions;
        }
        let active = self.ensure_active(space);
        if active == stage {
            return actions;
        }
        self.revealed.remove(&space);
        self.capture(active, obs);
        self.merge_pending(stage, active, true);
        let orphans: Vec<WindowId> = self
            .children_in(Home::Stage(stage))
            .into_iter()
            .filter(|&c| self.resolve_standard(c).is_none())
            .collect();
        if let Some(s) = self.stage_mut(stage) {
            s.windows.clear();
            s.key = None;
        }
        for &id in &members {
            self.set_home(id, Home::Stage(active), display);
        }
        for &c in &orphans {
            if let Some(w) = self.windows.get_mut(&c) {
                w.home = Home::Stage(active);
                w.meta.display = display;
            }
        }
        let key = src_key
            .filter(|k| members.contains(k))
            .or(members.first().copied());
        let t = self.tick();
        if let Some(a) = self.stage_mut(active) {
            let mut windows = members.clone();
            windows.append(&mut a.windows);
            a.windows = windows;
            if key.is_some() {
                a.key = key;
            }
            a.last_active = t;
        }
        let mut order = Vec::new();
        for &m in members.iter().rev() {
            order.push(m);
            order.extend(self.children_of(m));
        }
        order.extend(orphans);
        for &id in &order {
            if self.windows.get(&id).is_some_and(|w| w.parked) {
                self.restore_window(id, &mut actions);
            }
        }
        for &id in &order {
            actions.push(Action::Raise { id });
        }
        if let Some(k) = key {
            actions.push(Action::Focus { id: k });
        }
        self.drop_if_dead(stage, &mut actions);
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Pins a window (a child stands for its parent) so it shows on every stage of its Space
    /// and is never parked, or unpins it into the active stage of its Space. A window seen on
    /// every Space (sticky) stays pinned.
    pub fn toggle_pin(&mut self, id: WindowId, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(id) = self.resolve_standard(id) else {
            return actions;
        };
        if self.windows[&id].sticky {
            return actions;
        }
        let display = self.known_display(self.windows[&id].meta.display);
        let space = self.windows[&id].space;
        if !self.is_shown_space(space) {
            // As in `send_window`: never act on another Desktop's window (L2).
            return actions;
        }
        let mut group = vec![id];
        group.extend(self.children_of(id));
        match self.windows[&id].home {
            Home::Pinned => {
                let stage = self.ensure_active(space);
                if let Some(s) = self.stage_mut(stage) {
                    s.windows.insert(0, id);
                    s.key = Some(id);
                }
                self.set_home(id, Home::Stage(stage), display);
            }
            Home::Stage(s) => {
                for &w in &group {
                    if let Some(&f) = obs.frames.get(&w) {
                        self.record_frame(w, f);
                    }
                }
                if let Some(stage) = self.stage_mut(s) {
                    stage.windows.retain(|&w| w != id);
                    if stage.key == Some(id) {
                        stage.key = stage.windows.first().copied();
                    }
                }
                self.set_home(id, Home::Pinned, display);
                let parked: Vec<WindowId> = group
                    .iter()
                    .copied()
                    .filter(|w| self.windows.get(w).is_some_and(|w| w.parked))
                    .collect();
                for &w in &parked {
                    self.restore_window(w, &mut actions);
                }
                for &w in &parked {
                    actions.push(Action::Raise { id: w });
                }
                self.drop_if_dead(s, &mut actions);
            }
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Where Apple puts a new standard window of app `pid` (the window itself not yet
    /// managed) on the Space `display` shows: see [`Model::add_window`] for `native`. The
    /// app's windows on other Spaces do not count (each Space has its own stages).
    pub fn native_target(&self, pid: Pid, display: DisplayId) -> NativeTarget {
        self.native_target_in(pid, self.current_space(display))
    }

    /// [`Model::native_target`] on `space`: where a standard window of app `pid` goes when it
    /// shows there (opened, or moved there from another Desktop).
    pub(crate) fn native_target_in(&self, pid: Pid, space: SpaceId) -> NativeTarget {
        let active = self.active_stage_in(space);
        let app: Vec<&WindowState> = self
            .windows
            .values()
            .filter(|w| w.meta.pid == pid && w.meta.role == WindowRole::Standard)
            .filter(|w| w.space == space || w.sticky)
            .collect();
        if app
            .iter()
            .any(|w| w.home == Home::Pinned || Some(w.home) == active.map(Home::Stage))
        {
            return match active {
                Some(a) => NativeTarget::Join(a),
                None => NativeTarget::Fresh,
            };
        }
        if self.options.settings.app_windows == AppWindows::OneAtATime {
            return NativeTarget::Fresh;
        }
        app.iter()
            .filter_map(|w| match w.home {
                Home::Stage(s) => self.stage(s),
                Home::Pinned => None,
            })
            .filter(|s| s.space == space)
            .max_by_key(|s| s.last_active)
            .map_or(NativeTarget::Fresh, |s| NativeTarget::Join(s.id))
    }

    /// True when app `pid` has no managed standard window (on any Desktop): a window of it
    /// shows a newly opened app (or one whose windows were all closed).
    pub fn app_is_new(&self, pid: Pid) -> bool {
        !self.windows.values().any(|w| w.meta.pid == pid && w.meta.role == WindowRole::Standard)
    }

    /// Whether `meta`'s window, which Apple's rule gives a stage of its own, joins the active
    /// stage instead ([`Decision::new_app_joins_current`]): its app is new and the restore
    /// window is closed.
    fn new_app_joins(&self, meta: &WindowMeta, decision: &Decision) -> bool {
        decision.new_app_joins_current && !self.restore.restoring && self.app_is_new(meta.pid)
    }

    /// True when a stage has no window and none pending (a stage waiting for its windows is
    /// not an empty stage to reuse).
    pub(crate) fn stage_is_empty(&self, stage: StageId) -> bool {
        self.stage(stage).is_some_and(|s| s.windows.is_empty()) && !self.has_pending(stage)
    }

    /// Brings app `pid`'s windows from other stages into the active stage instead of
    /// switching (Shift held on a Dock click or ⌘Tab). With [`AppWindows::AllAtOnce`] every
    /// standard window of the app in an inactive stage of the current Space comes, most recently
    /// active stage first and front to back within each; with [`AppWindows::OneAtATime`] only
    /// the app's most recent one. They go in front of the active stage's windows, are restored
    /// and raised, and the first is focused. Stages left empty go away. A no-op (no actions)
    /// when the app has no window elsewhere.
    pub fn bring_app_here(&mut self, pid: Pid, obs: &Observation) -> Vec<Action> {
        let one = self.options.settings.app_windows == AppWindows::OneAtATime;
        self.gather_app(pid, obs, one)
    }

    /// A click on a card's app icon: switch to the card's stage, then bring every window of
    /// app `pid` from the other stages there too (Apple: the icon shows all of the app's
    /// windows, including those in other sets). With nothing elsewhere it is a plain switch.
    pub fn show_app_everywhere(&mut self, stage: StageId, pid: Pid, obs: &Observation) -> Vec<Action> {
        let mut actions = self.switch_to(stage, obs);
        actions.retain(|a| *a != Action::StateChanged);
        // The stage just shown has no live frames to save yet.
        let mut more = self.gather_app(pid, &Observation::default(), false);
        actions.append(&mut more);
        if actions.last() != Some(&Action::StateChanged) {
            actions.push(Action::StateChanged);
        }
        actions
    }

    /// ⌘M on a shown window (`[windows] minimize_to_strip`): instead of the Dock, the window
    /// goes to the strip as a stage of its own, as Apple's does; the view does not switch.
    /// The platform un-minimizes it after parking it. A no-op for pinned, parked or unknown
    /// windows.
    pub fn minimize_to_strip(&mut self, id: WindowId, obs: &Observation) -> Vec<Action> {
        let Some(id) = self.resolve_standard(id) else { return Vec::new() };
        let Some(w) = self.windows.get(&id) else { return Vec::new() };
        if w.parked || w.home == Home::Pinned {
            return Vec::new();
        }
        self.send_window(id, Target::New, obs)
    }

    /// Moves app `pid`'s windows from the current Space's inactive stages into the active one
    /// (only the most recent one when `one`); see [`Model::bring_app_here`].
    fn gather_app(&mut self, pid: Pid, obs: &Observation, one: bool) -> Vec<Action> {
        let mut actions = Vec::new();
        let display = self.current_display(obs);
        let space = self.current_space(display);
        let active = self.ensure_active(space);
        let mut sources: Vec<(u64, StageId, Vec<WindowId>)> = self
            .stages
            .iter()
            .filter(|s| s.space == space && s.id != active)
            .map(|s| {
                let mine: Vec<WindowId> = s
                    .windows
                    .iter()
                    .copied()
                    .filter(|w| self.windows.get(w).is_some_and(|w| w.meta.pid == pid))
                    .collect();
                (s.last_active, s.id, mine)
            })
            .filter(|(_, _, mine)| !mine.is_empty())
            .collect();
        sources.sort_by_key(|s| std::cmp::Reverse(s.0));
        let mut moving: Vec<WindowId> = sources.iter().flat_map(|s| s.2.clone()).collect();
        if one {
            moving.truncate(1);
        }
        if moving.is_empty() {
            return actions;
        }
        self.revealed.remove(&space);
        self.capture(active, obs);
        for &id in &moving {
            if let Some(Home::Stage(s)) = self.windows.get(&id).map(|w| w.home)
                && let Some(stage) = self.stage_mut(s)
            {
                stage.windows.retain(|&w| w != id);
                if stage.key == Some(id) {
                    stage.key = stage.windows.first().copied();
                }
            }
            self.set_home(id, Home::Stage(active), display);
        }
        let t = self.tick();
        if let Some(a) = self.stage_mut(active) {
            let mut windows = moving.clone();
            windows.append(&mut a.windows);
            a.windows = windows;
            a.key = moving.first().copied();
            a.last_active = t;
        }
        let mut order = Vec::new();
        for &m in moving.iter().rev() {
            order.push(m);
            order.extend(self.children_of(m));
        }
        for &id in &order {
            if self.windows.get(&id).is_some_and(|w| w.parked) {
                self.restore_window(id, &mut actions);
            }
        }
        for &id in &order {
            actions.push(Action::Raise { id });
        }
        if let Some(&first) = moving.first() {
            actions.push(Action::Focus { id: first });
        }
        for (_, s, _) in sources {
            self.drop_if_dead(s, &mut actions);
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// A card dragged out of the strip and dropped in the main area: like
    /// [`Model::merge_into_active`], but the stage's front window lands with its top-left
    /// corner at `top_left` (its size kept, clamped onto the visible frame) and its children
    /// move with it. The other windows keep their saved frames.
    pub fn merge_into_active_at(
        &mut self,
        stage: StageId,
        top_left: Point,
        obs: &Observation,
    ) -> Vec<Action> {
        let Some(s) = self.stage(stage) else {
            return Vec::new();
        };
        if self.is_active(stage) {
            return Vec::new();
        }
        if let Some(front) = s.windows.first().copied()
            && let Some(w) = self.windows.get(&front)
        {
            let visible = self.display(w.meta.display).visible;
            let old = w.restore;
            let placed = clamp_to_visible(
                Rect::new(top_left.x, top_left.y, old.w, old.h),
                visible,
            );
            let (dx, dy) = (placed.x - old.x, placed.y - old.y);
            if let Some(w) = self.windows.get_mut(&front) {
                w.restore = placed;
            }
            for c in self.children_of(front) {
                let Some(cw) = self.windows.get(&c) else { continue };
                let display = self.display(cw.meta.display).visible;
                let moved = clamp_to_visible(cw.restore.translate(dx, dy), display);
                if let Some(cw) = self.windows.get_mut(&c) {
                    cw.restore = moved;
                }
            }
        }
        self.merge_into_active(stage, obs)
    }

    /// A card dropped onto another card (`[strip] merge_on_drop`, an extension: Apple's strip
    /// does not merge): every window of `stage` joins `into`, behind `into`'s own windows and
    /// in its own order, each child with its parent. Both stages are inactive, so nothing
    /// moves on screen: the windows stay parked with their saved frames. `into` keeps its
    /// slot, name and key window (an empty one takes `stage`'s) and the more recent of the
    /// two recencies; `stage`'s slot frees by the slot rule (the cards below close up, and a
    /// free slot takes the most recent stage out of view). `stage`'s windows still awaited
    /// after a restart move with it, behind, in saved order (R5). The emptied stage goes away
    /// unless it is declared in config. Into the active stage (a card of it shows only in
    /// place with `[strip] active = "card"`) it is [`Model::merge_into_active`]. A no-op for one
    /// stage onto itself, an unknown stage, the active stage dropped on a card, stages of two
    /// Spaces, or a Space not shown now.
    ///
    /// With "One at a Time" both ids are cards' stages, and a grouped card carries the app's
    /// least recently shown stage while sitting in the group's first slot
    /// ([`Model::slot_showing`]): a grouped card dropped on a card merges every stage of its
    /// app's group (the card shows them all), and a card dropped on a grouped card merges
    /// into the stage whose slot that card sits in, so the merged card is where it was
    /// dropped (review R2 V6).
    pub fn merge_stages(&mut self, stage: StageId, into: StageId, obs: &Observation) -> Vec<Action> {
        let (sources, into) = self.merge_sources(stage, into);
        let mut actions = Vec::new();
        let mut changed = false;
        for source in sources {
            let done = self.merge_one(source, into, obs);
            changed |= !done.is_empty();
            actions.extend(done.into_iter().filter(|a| *a != Action::StateChanged));
        }
        if changed {
            actions.push(Action::StateChanged);
        }
        actions
    }

    /// The stages a card drop of `stage` onto `into` merges, and the stage they merge into
    /// ([`Model::merge_stages`]).
    fn merge_sources(&self, stage: StageId, into: StageId) -> (Vec<StageId>, StageId) {
        if self.options.settings.app_windows != AppWindows::OneAtATime {
            return (vec![stage], into);
        }
        let Some(space) = self.stage(stage).map(|s| s.space) else { return (vec![stage], into) };
        let into = self.slot_showing(space, into).unwrap_or(into);
        let single_app = |id: StageId| {
            let s = self.stage(id)?;
            let pid = self.windows.get(s.windows.first()?)?.meta.pid;
            s.windows.iter().all(|w| self.windows.get(w).is_some_and(|w| w.meta.pid == pid)).then_some(pid)
        };
        let Some(pid) = single_app(stage).filter(|_| !self.is_active(stage)) else { return (vec![stage], into) };
        let mut group: Vec<(u64, StageId)> = self
            .stages
            .iter()
            .filter(|s| s.space == space && s.id != into && !self.is_active(s.id) && single_app(s.id) == Some(pid))
            .map(|s| (s.last_active, s.id))
            .collect();
        group.sort();
        if group.is_empty() {
            return (vec![stage], into);
        }
        (group.into_iter().map(|g| g.1).collect(), into)
    }

    /// Merges one stage's windows into `into` ([`Model::merge_stages`] for one card).
    fn merge_one(&mut self, stage: StageId, into: StageId, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        let (Some(src), Some(dest)) = (self.stage(stage), self.stage(into)) else {
            return actions;
        };
        if stage == into || src.space != dest.space || !self.is_shown_space(src.space) || self.is_active(stage) {
            return actions;
        }
        if self.is_active(into) {
            return self.merge_into_active(stage, obs);
        }
        let display = dest.display;
        let members = src.windows.clone();
        let src_key = src.key.filter(|k| members.contains(k));
        let recency = src.last_active.max(dest.last_active);
        self.merge_pending(stage, into, false);
        let orphans: Vec<WindowId> = self
            .children_in(Home::Stage(stage))
            .into_iter()
            .filter(|&c| self.resolve_standard(c).is_none())
            .collect();
        if let Some(s) = self.stage_mut(stage) {
            s.windows.clear();
            s.key = None;
        }
        for &id in &members {
            self.set_home(id, Home::Stage(into), display);
        }
        for &c in &orphans {
            if let Some(w) = self.windows.get_mut(&c) {
                w.home = Home::Stage(into);
                w.meta.display = display;
            }
        }
        if let Some(d) = self.stage_mut(into) {
            d.windows.extend(members.iter().copied());
            if d.key.is_none_or(|k| !d.windows.contains(&k)) {
                d.key = src_key.or(d.windows.first().copied());
            }
            d.last_active = recency;
        }
        self.drop_if_dead(stage, &mut actions);
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// A window dragged by its title bar and dropped on the strip: it goes to a new stage of
    /// its own (parked, its card in the strip), and its restore frame is `pre_drag`, the
    /// frame it had before the drag, not where it was dropped. Its children move back by the
    /// same offset. Its card takes the slot it was dropped into: right above `before`'s card,
    /// or the bottom when `before` is `None` (see [`Model::move_card`]); following the Dock
    /// ([`StripOrder::Dock`]) its Dock place instead. Same as [`Model::send_window`] with
    /// [`Target::New`] otherwise.
    pub fn drop_window_on_strip(
        &mut self,
        id: WindowId,
        pre_drag: Rect,
        before: Option<StageId>,
        obs: &Observation,
    ) -> Vec<Action> {
        let actions = self.drop_window_on_strip_as_new(id, pre_drag, obs);
        if let Some(stage) = self.stage_of(id).filter(|s| !self.is_active(*s))
            && before != Some(stage)
        {
            // A new stage, not a card dragged among the cards shown: in place it moves in
            // creation order; following the Dock it takes its Dock place.
            self.place_card(stage, before, false);
        }
        actions
    }

    fn drop_window_on_strip_as_new(
        &mut self,
        id: WindowId,
        pre_drag: Rect,
        obs: &Observation,
    ) -> Vec<Action> {
        let Some(id) = self.resolve_standard(id) else {
            return Vec::new();
        };
        let mut obs = obs.clone();
        let dropped = obs
            .frames
            .get(&id)
            .copied()
            .or(self.windows.get(&id).map(|w| w.meta.frame));
        if let Some(d) = dropped {
            let (dx, dy) = (pre_drag.x - d.x, pre_drag.y - d.y);
            for c in self.children_of(id) {
                if let Some(f) = obs.frames.get_mut(&c) {
                    *f = f.translate(dx, dy);
                }
            }
        }
        obs.frames.insert(id, pre_drag);
        self.send_window(id, Target::New, &obs)
    }

    /// A card dragged to another slot of the strip (`[strip] reorder`): it goes right above
    /// `before`'s card, or to the bottom when `before` is `None`. In Apple's order the slots
    /// change and the slot-stable rule carries on from them (a full strip drops its least
    /// recently used card, never the moved one); in place the stage moves in creation
    /// order, so hotkey numbers follow what the strip shows: among the places of the cards
    /// shown when both cards are in the strip ([`Model::reorder_shown`]), so the strip shows
    /// the same stages after the drop as it drew at the release. Following the Dock
    /// ([`StripOrder::Dock`]) nothing moves: the Dock decides, and the card springs back.
    /// Returns [`Action::StateChanged`] when anything moved (the order is saved with the
    /// state).
    pub fn move_card(&mut self, stage: StageId, before: Option<StageId>) -> Vec<Action> {
        if before == Some(stage) || !self.place_card(stage, before, true) {
            return Vec::new();
        }
        vec![Action::StateChanged]
    }

    /// [`Model::move_card`]'s move; `among_shown`: in place, a card dragged within the strip
    /// trades places with the cards shown ([`Model::reorder_shown`]).
    fn place_card(&mut self, stage: StageId, before: Option<StageId>, among_shown: bool) -> bool {
        let Some(space) = self.stage(stage).map(|s| s.space) else {
            return false;
        };
        match self.options.strip.order {
            StripOrder::Native => {
                let capacity = self.options.strip.capacity;
                if capacity == 0 || self.is_active(stage) {
                    return false;
                }
                let recency: Vec<(StageId, u64)> =
                    self.stages.iter().map(|s| (s.id, s.last_active)).collect();
                let age = |id: StageId| recency.iter().find(|r| r.0 == id).map_or(0, |r| r.1);
                let slots = self.slots.entry(space).or_default();
                let old = slots.clone();
                slots.retain(|&s| s != stage);
                let at = before
                    .and_then(|b| slots.iter().position(|&s| s == b))
                    .unwrap_or(slots.len());
                slots.insert(at, stage);
                while slots.len() > capacity {
                    let oldest = slots
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| **s != stage)
                        .min_by_key(|(_, s)| age(**s))
                        .map(|(i, _)| i);
                    match oldest {
                        Some(i) => {
                            slots.remove(i);
                        }
                        None => break,
                    }
                }
                *slots != old
            }
            StripOrder::InPlace => {
                // A card dragged among the cards shown: the strip comes out as drawn.
                if among_shown && let Some(moved) = self.reorder_shown(space, stage, before) {
                    return moved;
                }
                let Some(from) = self.stages.iter().position(|s| s.id == stage) else {
                    return false;
                };
                let moved = self.stages.remove(from);
                let at = before
                    .and_then(|b| self.stages.iter().position(|s| s.id == b))
                    .unwrap_or(self.stages.len());
                self.stages.insert(at, moved);
                // Put where the user can see it: past the capacity the bottom places go
                // by recency, so a card dragged (or a window dropped) there keeps showing.
                if !self.is_active(stage) {
                    self.touch(stage);
                }
                at != from
            }
            StripOrder::Dock => {
                // The Dock decides the place. A window dropped on the strip still counts as
                // used, so past the capacity its new stage shows (see the arm above).
                if !among_shown && !self.is_active(stage) {
                    self.touch(stage);
                }
                false
            }
        }
    }

    /// An app became frontmost (Dock click, ⌘Tab).
    ///
    /// Only the current Space counts: an app with no window here is left to macOS (which
    /// may switch Spaces, following "When switching to an application, switch to a Space
    /// with open windows for the application"; the arrival there is [`Model::arrive`]).
    /// The candidate is the stage of the app's focused window in `obs` (when it is inactive),
    /// else the most recently active stage holding a window of the app. With
    /// [`AppWindows::AllAtOnce`] nothing happens while the active stage holds a window of the
    /// app (or the app has a pinned window); with [`AppWindows::OneAtATime`] the focused
    /// window's stage wins even then. Nothing while the login burst settles
    /// ([`Model::settling`]).
    pub fn app_activated(&mut self, pid: Pid, obs: &Observation) -> Vec<Action> {
        if self.settling() {
            // A launch-time activation at login is the app's, not the user's (a click or a
            // key press would have ended the settling).
            return Vec::new();
        }
        let display = self.current_display(obs);
        let space = self.current_space(display);
        let active = self.ensure_active(space);
        let present = self.windows.values().any(|w| {
            w.meta.pid == pid
                && w.meta.role == WindowRole::Standard
                && (w.home == Home::Stage(active)
                    || (w.home == Home::Pinned && (w.space == space || w.sticky)))
        });
        let focused_stage = obs
            .focused
            .and_then(|f| self.resolve_standard(f))
            .and_then(|f| self.windows.get(&f))
            .filter(|w| w.meta.pid == pid && w.space == space)
            .and_then(|w| match w.home {
                Home::Stage(s) if !self.is_active(s) => Some(s),
                _ => None,
            });
        let most_recent = || {
            self.stages
                .iter()
                .filter(|s| s.space == space && !self.is_active(s.id))
                .filter(|s| {
                    s.windows
                        .iter()
                        .any(|w| self.windows.get(w).is_some_and(|w| w.meta.pid == pid))
                })
                .max_by_key(|s| s.last_active)
                .map(|s| s.id)
        };
        let target = match self.options.settings.app_windows {
            AppWindows::AllAtOnce if present => None,
            AppWindows::AllAtOnce => focused_stage.or_else(most_recent),
            AppWindows::OneAtATime => {
                focused_stage.or_else(|| if present { None } else { most_recent() })
            }
        };
        match target {
            Some(s) => {
                // The user asked for this app: its window comes in front with the keyboard,
                // whatever window of another app the stage had last (Apple's does so).
                if let Some(k) = self.activated_key(s, pid, obs.focused) {
                    self.make_key(s, k);
                }
                self.switch_to(s, obs)
            }
            None => Vec::new(),
        }
    }

    /// The window of app `pid` an activation that shows `stage` makes its key: its focused
    /// window (`focused`, a child standing for its parent) when that is a member of the stage,
    /// else the stage's key when it is the app's, else the app's front member. None when the
    /// stage holds no standard window of the app.
    fn activated_key(&self, stage: StageId, pid: Pid, focused: Option<WindowId>) -> Option<WindowId> {
        let s = self.stage(stage)?;
        let mine = |id: &WindowId| self.windows.get(id).is_some_and(|w| w.meta.pid == pid);
        focused
            .and_then(|f| self.resolve_standard(f))
            .filter(|f| s.windows.contains(f) && mine(f))
            .or(s.key.filter(|k| s.windows.contains(k) && mine(k)))
            .or_else(|| s.windows.iter().copied().find(|id| mine(id)))
    }

    /// Whether [`Self::app_activated`]'s answer for app `pid` depends on the focused window
    /// (`obs.focused`). It does only when the focus could pick among stages: the app's windows
    /// sit in more than one inactive stage of the shown Space, or, showing one app's windows
    /// at a time, it has a window here and one in an inactive stage. Otherwise the answer is
    /// the same whatever the focus, so the platform need not ask the app (busy activating)
    /// for it. The display is [`Self::app_activated`]'s own ([`Self::current_display`]):
    /// without a focus that is the main display, so an app with a window on another display
    /// (whose focus would move the answer there) needs the focus too.
    pub fn activation_needs_focus(&self, pid: Pid) -> bool {
        let display = self.current_display(&Observation::default());
        if self.windows.values().any(|w| w.meta.pid == pid && self.known_display(w.meta.display) != display) {
            return true;
        }
        let space = self.current_space(display);
        let Some(&active) = self.active.get(&space) else { return true };
        let present = self.windows.values().any(|w| {
            w.meta.pid == pid
                && w.meta.role == WindowRole::Standard
                && (w.home == Home::Stage(active) || (w.home == Home::Pinned && (w.space == space || w.sticky)))
        });
        let candidates = self
            .stages
            .iter()
            .filter(|s| s.space == space && !self.is_active(s.id))
            .filter(|s| s.windows.iter().any(|w| self.windows.get(w).is_some_and(|w| w.meta.pid == pid)))
            .count();
        match self.options.settings.app_windows {
            AppWindows::AllAtOnce if present => false,
            AppWindows::AllAtOnce => candidates > 1,
            AppWindows::OneAtATime if present => candidates >= 1,
            AppWindows::OneAtATime => candidates > 1,
        }
    }

    /// A window got focus (for example through ⌘` or a click on a sliver). If it sits in an
    /// inactive stage of a Space shown now, switch to that stage so the focused window is
    /// never left parked. Focus on another Space's window is left to the Space change.
    /// Nothing while the login burst settles ([`Model::settling`]).
    pub fn window_focused(&mut self, id: WindowId, obs: &Observation) -> Vec<Action> {
        if self.settling() {
            // As for an activation: at login a focus is the app's, not the user's.
            return Vec::new();
        }
        let Some(standard) = self.resolve_standard(id) else { return Vec::new() };
        match self.stage_of(standard) {
            Some(s) if !self.is_active(s) => {
                // The focused window is the one the user went for: it comes in front with
                // the keyboard, not the stage's last key.
                self.make_key(s, standard);
                self.switch_to(s, obs)
            }
            _ => Vec::new(),
        }
    }

    /// True when window `id` (or the window a child of it follows) is managed and lives on a
    /// Space no display shows now (sticky windows are on every Space): its focus cannot act
    /// until that Space is recognized ([`crate::engine::focus_replay`]).
    pub fn waits_on_another_space(&self, id: WindowId) -> bool {
        self.resolve_standard(id)
            .and_then(|w| self.windows.get(&w))
            .is_some_and(|w| !w.sticky && !self.is_shown_space(w.space))
    }

    /// True when `held` is a window focus that can act now with no Space change: its window
    /// is known and on a Space shown now (the "a wait ended" replay; one of a Desktop not
    /// shown stays held for its arrival, an unknown one until it is found).
    pub fn held_focus_acts_now(&self, held: Option<crate::engine::focus_replay::Wanted>) -> bool {
        matches!(held, Some(crate::engine::focus_replay::Wanted::Window { id, .. })
            if self.window(id).is_some() && !self.waits_on_another_space(id))
    }

    /// A Desktop was just recognized (or a wait ended): show the stage of the window the user
    /// went there for, as Apple's Stage Manager does when a Dock click, ⌘Tab or an activation
    /// lands on a window of another Desktop. `focused` is the frontmost app's focused window
    /// asked now; when it is a managed window of a Space shown now in an inactive stage, its
    /// stage comes, whatever order the events came in. When it is already shown, AX may be
    /// stale (the activation not complete: the previous frontmost app's window): a held
    /// window of **another app** than AX's, on a Space shown now, still brings its stage; a
    /// held window of AX's own app, or a held activation, changes nothing. When AX names no
    /// window here, `held`, the focus or activation that came while the Space was changing
    /// ([`crate::engine::focus_replay::FocusReplay`]), decides: a window of a shown Space
    /// brings its stage; an app is [`Model::app_activated`] again. A window or app with
    /// nothing on a shown Space changes nothing.
    pub fn bring_focus_forward(
        &mut self,
        focused: Option<WindowId>,
        held: Option<crate::engine::focus_replay::Wanted>,
        obs: &Observation,
    ) -> Vec<Action> {
        use crate::engine::focus_replay::Wanted;
        let here = |m: &Self, id: WindowId| m.resolve_standard(id).is_some() && !m.waits_on_another_space(id);
        if let Some(f) = focused.filter(|f| here(self, *f)) {
            let actions = self.window_focused(f, obs);
            if !actions.is_empty() {
                return actions;
            }
            let ax_pid = self.resolve_standard(f).and_then(|w| self.windows.get(&w)).map(|w| w.meta.pid);
            return match held {
                Some(Wanted::Window { id, pid }) if Some(pid) != ax_pid && here(self, id) => self.window_focused(id, obs),
                _ => Vec::new(),
            };
        }
        match held {
            Some(Wanted::Window { id, .. }) if here(self, id) => self.window_focused(id, obs),
            Some(Wanted::Window { pid, .. } | Wanted::App(pid)) => self.app_activated(pid, obs),
            None => Vec::new(),
        }
    }

    /// A parked window the user grabbed by its sliver and let go (the button is up again)
    /// at `frame`. Apple's parked windows cannot be grabbed, so nothing switched while the
    /// drag went on; now its stage comes as for any focus ([`Model::window_focused`]), with
    /// this window as the stage's key window and, when it was dropped outside the park zone,
    /// at the frame it was dropped at (its new restore frame), so it does not jump back.
    pub fn parked_window_dropped(&mut self, id: WindowId, frame: Rect, obs: &Observation) -> Vec<Action> {
        if let Some(standard) = self.resolve_standard(id)
            && self.windows.get(&standard).is_some_and(|w| w.parked)
        {
            if standard == id {
                self.record_frame(id, frame);
            }
            if let Some(stage) = self.stage_of(standard) {
                self.make_key(stage, standard);
            }
        }
        self.window_focused(id, obs)
    }

    /// Click on the wallpaper. Parks the active stage (it enters the strip) and makes a fresh
    /// empty stage active, as Apple's reveal does. A second reveal, while that empty stage is
    /// still active and the revealed stage is still the most recent, brings the stage back.
    /// A no-op when the active stage is empty and nothing is pending.
    pub fn reveal_desktop(&mut self, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        let space = self.current_space_for(obs);
        let current = self.ensure_active(space);
        let current_empty = self.stage(current).is_some_and(|s| s.windows.is_empty());
        if let Some(r) = self.revealed.get(&space).copied()
            && r.desktop == current
            && current_empty
            && self.is_most_recent_inactive(space, r.from)
        {
            return self.switch_to(r.from, obs);
        }
        if current_empty {
            return actions;
        }
        self.capture(current, obs);
        self.park_stage(current, &mut actions);
        let t = self.tick();
        if let Some(s) = self.stage_mut(current) {
            s.last_active = t;
        }
        let desktop = self.create_stage(space, None);
        self.active.insert(space, desktop);
        let t = self.tick();
        if let Some(s) = self.stage_mut(desktop) {
            s.last_active = t;
        }
        self.strip_enter(space, current);
        self.revealed.insert(
            space,
            Reveal {
                from: current,
                desktop,
            },
        );
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    fn is_most_recent_inactive(&self, space: SpaceId, stage: StageId) -> bool {
        let Some(s) = self.stage(stage) else {
            return false;
        };
        !self.is_active(stage)
            && self
                .stages
                .iter()
                .filter(|o| o.space == space && !self.is_active(o.id))
                .all(|o| o.last_active <= s.last_active)
    }

    /// A window moved or resized. The frame becomes its restore frame unless it lies in a park
    /// zone (our own parks report back here) or is degenerate. Ignored while the model has
    /// the window parked: macOS may clamp a parked window to a spot outside the park zone,
    /// and that spot must never become its restore frame.
    pub fn window_frame_changed(&mut self, id: WindowId, frame: Rect) {
        if self.windows.get(&id).is_some_and(|w| w.parked) {
            return;
        }
        self.record_frame(id, frame);
    }

    /// Like [`Model::window_frame_changed`] for the result of our own action (a frame we just
    /// set, maybe clamped by the app): it never counts as the user's resize.
    pub fn window_frame_settled(&mut self, id: WindowId, frame: Rect) {
        if self.windows.get(&id).is_some_and(|w| w.parked) {
            return;
        }
        self.record_own_frame(id, frame);
    }

    /// A window's title changed. Returns true when the window is managed.
    pub fn window_title_changed(&mut self, id: WindowId, title: &str) -> bool {
        match self.windows.get_mut(&id) {
            Some(w) => {
                w.meta.title = title.to_string();
                true
            }
            None => false,
        }
    }

    /// Brings every parked window back to its restore frame, clamped onto its display's
    /// visible frame, and every window we fitted into the stage area back to the exact frame
    /// it had before ([`Model::safety_targets`]). Used on quit and by safety code; the stages
    /// stay as they are, and the pre-fit frames become the restore frames.
    pub fn unpark_all(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        for (id, frame) in self.safety_targets() {
            if let Some(w) = self.windows.get_mut(&id) {
                w.parked = false;
                if w.original.take().is_some() {
                    w.restore = frame;
                }
            }
            actions.push(Action::SetFrame { id, frame });
        }
        actions
    }

    /// Replaces the display list (first = main). An empty list is ignored.
    ///
    /// - **The main display under a new id** (docking or undocking moves the menu bar to
    ///   another display, and the old main display is not in the list any more): the same
    ///   desk, relabeled in place. Its Spaces, stages, active stages and strip slots stay;
    ///   the active stage stays active and shown (the review's V1-B).
    /// - **A display's frame or visible frame changed** (resolution, scaling, a Dock moved,
    ///   a new id): every parked window of a Space shown on it is parked again at its new
    ///   corner (V1-A: parked windows would otherwise sit mid-screen on a bigger display).
    ///   Parked windows of Spaces not shown are parked again when their Space shows
    ///   ([`Model::arrive`]).
    /// - Spaces of a secondary display that is gone are folded into the main display's
    ///   current Space: their stages move there, the ones that were active become inactive
    ///   and are parked, and windows already parked are parked again on the main display.
    ///   New displays get a Space with an empty active stage.
    pub fn set_displays(&mut self, displays: Vec<Display>) -> Vec<Action> {
        let mut actions = Vec::new();
        // A report with a frame that is not finite is garbled: every restore frame would be
        // clamped against it.
        if displays.is_empty() || displays.iter().any(|d| !d.frame.is_finite() || !d.visible.is_finite()) {
            return actions;
        }
        let mut before = std::mem::take(&mut self.displays);
        let (old_main, new_main) = (before[0].id, displays[0].id);
        if old_main != new_main
            && !displays.iter().any(|d| d.id == old_main)
            && !before.iter().any(|d| d.id == new_main)
        {
            self.relabel_display(old_main, new_main);
            before[0].id = new_main;
        }
        let moved_frames: Vec<DisplayId> = displays
            .iter()
            .filter(|d| before.iter().any(|o| o.id == d.id && (o.frame != d.frame || o.visible != d.visible || o.corner != d.corner)))
            .map(|d| d.id)
            .collect();
        self.displays = displays;
        let main = self.displays[0].id;
        let live: Vec<DisplayId> = self.displays.iter().map(|d| d.id).collect();
        if !self.current.contains_key(&main) {
            let s = self.new_space(main);
            self.current.insert(main, s);
        }
        let main_space = self.current_space(main);
        let gone: Vec<SpaceId> = self
            .spaces
            .iter()
            .filter(|s| !live.contains(&s.display))
            .map(|s| s.id)
            .collect();
        let moved: Vec<StageId> = self
            .stages
            .iter()
            .filter(|s| gone.contains(&s.space) || !live.contains(&s.display))
            .map(|s| s.id)
            .collect();
        for g in &gone {
            self.active.remove(g);
            self.revealed.remove(g);
            self.slots.remove(g);
        }
        self.current.retain(|d, _| live.contains(d));
        self.spaces.retain(|s| live.contains(&s.display));
        for s in &moved {
            if let Some(st) = self.stage_mut(*s) {
                st.display = main;
                st.space = main_space;
            }
        }
        let ids: Vec<WindowId> = self.windows.keys().copied().collect();
        for id in ids {
            let on_gone = {
                let w = &self.windows[&id];
                gone.contains(&w.space) || !live.contains(&w.meta.display)
            };
            if on_gone && let Some(w) = self.windows.get_mut(&id) {
                w.meta.display = main;
                w.space = main_space;
            }
        }
        for d in live {
            if !self.current.contains_key(&d) {
                let s = self.new_space(d);
                self.current.insert(d, s);
            }
            let space = self.current_space(d);
            self.ensure_active(space);
        }
        for s in &moved {
            if !self.is_active(*s) {
                self.park_stage(*s, &mut actions);
                self.drop_if_dead(*s, &mut actions);
            }
        }
        let parked_again: Vec<WindowId> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Park { id, .. } => Some(*id),
                _ => None,
            })
            .collect();
        // A restore frame that falls in a changed display's park zone now (the display shrank)
        // is replaced by a safe one, so no window is ever brought back into hiding.
        let stale: Vec<(WindowId, Rect)> = self
            .windows
            .values()
            .filter(|w| moved_frames.contains(&w.meta.display) && self.display(w.meta.display).parks(w.restore))
            .map(|w| (w.meta.id, self.safe_restore(w.restore, w.meta.display)))
            .collect();
        for (id, restore) in stale {
            if let Some(w) = self.windows.get_mut(&id) {
                w.restore = restore;
            }
        }
        let repark: Vec<WindowId> = self
            .windows
            .values()
            .filter(|w| w.parked && moved_frames.contains(&w.meta.display) && self.is_shown_space(w.space))
            .map(|w| w.meta.id)
            .filter(|id| !parked_again.contains(id))
            .collect();
        for id in repark {
            self.park_window(id, &mut actions);
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Renames display `old` to `new` everywhere (the main display under a new id).
    fn relabel_display(&mut self, old: DisplayId, new: DisplayId) {
        for s in &mut self.spaces {
            if s.display == old {
                s.display = new;
            }
        }
        for s in &mut self.stages {
            if s.display == old {
                s.display = new;
            }
        }
        for w in self.windows.values_mut() {
            if w.meta.display == old {
                w.meta.display = new;
            }
        }
        if let Some(space) = self.current.remove(&old) {
            self.current.insert(new, space);
        }
    }

    /// Replaces the declared stage names (config reload). New names get a stage; stages whose
    /// name is no longer declared lose their protection and go away if empty and inactive.
    pub fn set_named_stages(&mut self, names: Vec<String>) -> Vec<Action> {
        let mut actions = Vec::new();
        self.options.named_stages = names.clone();
        let spaces: Vec<SpaceId> = self.spaces.iter().map(|s| s.id).collect();
        for space in spaces {
            self.declare_stages(space, &names);
        }
        let ids: Vec<StageId> = self.stages.iter().map(|s| s.id).collect();
        for s in ids {
            self.drop_if_dead(s, &mut actions);
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// True when `frame` would count as parked on the window's display. Exposed for the
    /// platform's recovery scan of windows it observes.
    pub fn frame_is_parked(&self, display: DisplayId, frame: Rect) -> bool {
        self.display(display).parks(frame)
    }
}
