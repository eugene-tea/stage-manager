//! Model operations. Each returns the actions the platform executes in order: parks first,
//! then restore frames, then raises back to front, then focus, then `StateChanged`.

use super::{
    Action, Display, DisplayId, Home, Model, Observation, Pid, Reveal, StageId, Target, WindowId,
    WindowMeta, WindowRole, WindowState,
};
use crate::engine::geometry::Rect;
use crate::engine::park::is_parked;
use crate::engine::placement::{CASCADE_OFFSET, cascade, center};
use crate::engine::rules::{Decision, Placement, RuleAction};
use crate::engine::settings::AppWindows;

impl Model {
    /// A window appeared. `decision` comes from the rules; it is ignored for children, which
    /// follow their parent (a child whose parent is known but unmanaged stays unmanaged; one
    /// with no known parent joins the active stage). Duplicate notifications for a known
    /// `WindowId` only refresh its title.
    ///
    /// Standard windows: `current` joins the active stage at the front; `new` creates a stage
    /// and switches to it; `stage:<name>` joins that stage (created if missing) and switches to
    /// it when the window is focused in `obs`, else parks it there; `float` pins it; `ignore`
    /// leaves it unmanaged. Placement (`center`, `cascade` from the stage's front window)
    /// applies before any switch.
    pub fn add_window(&mut self, meta: WindowMeta, decision: &Decision, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(w) = self.windows.get_mut(&meta.id) {
            w.meta.title = meta.title;
            return actions;
        }
        if decision.action == RuleAction::Ignore {
            return actions;
        }
        let display = self.known_display(meta.display);
        let mut meta = meta;
        meta.display = display;
        let id = meta.id;

        if let WindowRole::Child { parent } = meta.role {
            let home = match parent {
                Some(p) => match self.windows.get(&p) {
                    Some(pw) => {
                        meta.display = pw.meta.display;
                        pw.home
                    }
                    None => return actions,
                },
                None => Home::Stage(self.ensure_active(display)),
            };
            let restore = self.safe_restore(meta.frame, meta.display);
            let born_parked = restore != meta.frame;
            self.windows.insert(id, WindowState { meta, home, restore, parked: false });
            match home {
                Home::Stage(s) if !self.is_active(s) => self.park_window(id, &mut actions),
                _ if born_parked => actions.push(Action::SetFrame { id, frame: restore }),
                _ => {}
            }
            actions.push(Action::StateChanged);
            return actions;
        }

        let active = self.ensure_active(display);
        let (home, created) = match &decision.action {
            RuleAction::Float => (Home::Pinned, false),
            RuleAction::Current | RuleAction::Ignore => (Home::Stage(active), false),
            RuleAction::New => (Home::Stage(self.create_stage(display, None)), true),
            RuleAction::Stage(name) => match self.find_named(name) {
                Some(s) => (Home::Stage(s), false),
                None => (Home::Stage(self.create_stage(display, Some(name.clone()))), true),
            },
        };
        let front_stage = match home {
            Home::Stage(s) if !created => s,
            _ => active,
        };
        let front = self
            .stage(front_stage)
            .and_then(|s| s.windows.first().copied())
            .and_then(|f| obs.frames.get(&f).copied().or(self.windows.get(&f).map(|w| w.restore)));
        let visible = self.display(display).visible;
        let placed = match decision.placement {
            Placement::AsIs => meta.frame,
            Placement::Center => center(meta.frame, visible),
            Placement::Cascade => cascade(meta.frame, front, visible, CASCADE_OFFSET),
        };
        let restore = self.safe_restore(placed, display);
        self.windows.insert(id, WindowState { meta, home, restore, parked: false });
        if let Home::Stage(s) = home
            && let Some(stage) = self.stage_mut(s)
        {
            stage.windows.insert(0, id);
            stage.key = Some(id);
        }

        let target_active = match home {
            Home::Stage(s) => self.is_active(s),
            Home::Pinned => true,
        };
        if target_active {
            // `restore` differs from `placed` only when the window was born in a park zone.
            if restore != self.windows[&id].meta.frame {
                actions.push(Action::SetFrame { id, frame: restore });
            }
        } else if let Home::Stage(s) = home {
            let follow = decision.action == RuleAction::New || obs.focused == Some(id);
            if follow {
                actions.extend(self.switch_to(s, obs));
                return actions;
            }
            self.park_window(id, &mut actions);
            self.strip_enter(display, s);
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// A window is gone. It leaves its stage; its children are forgotten too (any that were
    /// parked are first restored, in case they outlive it). An inactive stage left with no
    /// standard window disappears; the active stage may become empty.
    pub fn remove_window(&mut self, id: WindowId) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(w) = self.windows.get(&id) else { return actions };
        let home = w.home;
        for c in self.children_of(id) {
            if self.windows.get(&c).is_some_and(|w| w.parked) {
                self.restore_window(c, &mut actions);
            }
            self.windows.remove(&c);
        }
        self.windows.remove(&id);
        if let Home::Stage(s) = home {
            if let Some(stage) = self.stage_mut(s) {
                stage.windows.retain(|&w| w != id);
                if stage.key == Some(id) {
                    stage.key = stage.windows.first().copied();
                }
            }
            self.drop_if_dead(s, &mut actions);
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Switches the target's display to `target`: saves the active stage's live frames, z-order
    /// and key window from `obs`, parks it, restores the target's frames, raises its windows
    /// back to front and focuses its key window last. A no-op for the active stage or an
    /// unknown one. Pinned windows are untouched. In the strip the stage left behind takes the
    /// target's slot (or enters the strip if the target had none).
    pub fn switch_to(&mut self, target: StageId, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(display) = self.stage(target).map(|s| s.display) else { return actions };
        let current = self.ensure_active(display);
        if current == target {
            return actions;
        }
        self.revealed.remove(&display);
        self.capture(current, obs);
        self.park_stage(current, &mut actions);
        let t = self.tick();
        if let Some(s) = self.stage_mut(current) {
            s.last_active = t;
        }
        self.active.insert(display, target);
        let t = self.tick();
        if let Some(s) = self.stage_mut(target) {
            s.last_active = t;
        }
        if self.strip_eligible(current) {
            self.strip_swap(display, current, target);
        } else if let Some(slots) = self.slots.get_mut(&display) {
            slots.retain(|&s| s != target);
        }
        self.drop_if_dead(current, &mut actions);
        self.show_stage(target, &mut actions);
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Switches to stage `n` (1-based, creation order); a no-op when there is none.
    pub fn switch_to_number(&mut self, n: usize, obs: &Observation) -> Vec<Action> {
        match n.checked_sub(1).and_then(|i| self.stages.get(i)).map(|s| s.id) {
            Some(s) => self.switch_to(s, obs),
            None => Vec::new(),
        }
    }

    /// Sends a standard window (a child stands for its parent) and its children to another
    /// stage, at the front, as its key window. The view does not switch: if the target is
    /// inactive the window is parked; if the source is the active stage and the window was
    /// focused, the source's next front window takes focus. A pinned window is unpinned.
    /// `Target::Number(n)` creates a stage only when `n == count + 1`, else is a no-op for a
    /// missing stage. Sending the last window out leaves the active stage empty.
    pub fn send_window(&mut self, id: WindowId, target: Target, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(id) = self.resolve_standard(id) else { return actions };
        let src = self.windows[&id].home;
        let src_display = match src {
            Home::Stage(s) => self.stage(s).map_or(self.displays[0].id, |s| s.display),
            Home::Pinned => self.known_display(self.windows[&id].meta.display),
        };
        let dest = match target {
            Target::Number(n) if n >= 1 && n <= self.stages.len() => self.stages[n - 1].id,
            Target::Number(n) if n == self.stages.len() + 1 => self.create_stage(src_display, None),
            Target::Number(_) => return actions,
            Target::New => self.create_stage(src_display, None),
            Target::Named(name) => match self.find_named(&name) {
                Some(s) => s,
                None => self.create_stage(src_display, Some(name)),
            },
        };
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
            let parked: Vec<WindowId> =
                group.iter().copied().filter(|w| self.windows.get(w).is_some_and(|w| w.parked)).collect();
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
            self.strip_enter(dest_display, dest);
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Pins a window (a child stands for its parent) so it shows on every stage and is never
    /// parked, or unpins it into the active stage of its display.
    pub fn toggle_pin(&mut self, id: WindowId, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(id) = self.resolve_standard(id) else { return actions };
        let display = self.known_display(self.windows[&id].meta.display);
        let mut group = vec![id];
        group.extend(self.children_of(id));
        match self.windows[&id].home {
            Home::Pinned => {
                let stage = self.ensure_active(display);
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
                let parked: Vec<WindowId> =
                    group.iter().copied().filter(|w| self.windows.get(w).is_some_and(|w| w.parked)).collect();
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

    /// An app became frontmost (Dock click, ⌘Tab).
    ///
    /// The candidate is the stage of the app's focused window in `obs` (when it is inactive),
    /// else the most recently active stage holding a window of the app. With
    /// [`AppWindows::AllAtOnce`] nothing happens while the active stage holds a window of the
    /// app (or the app has a pinned window); with [`AppWindows::OneAtATime`] the focused
    /// window's stage wins even then.
    pub fn app_activated(&mut self, pid: Pid, obs: &Observation) -> Vec<Action> {
        let display = self.current_display(obs);
        let active = self.ensure_active(display);
        let present = self.windows.values().any(|w| {
            w.meta.pid == pid
                && w.meta.role == WindowRole::Standard
                && (w.home == Home::Stage(active) || w.home == Home::Pinned)
        });
        let focused_stage = obs
            .focused
            .and_then(|f| self.resolve_standard(f))
            .and_then(|f| self.windows.get(&f))
            .filter(|w| w.meta.pid == pid)
            .and_then(|w| match w.home {
                Home::Stage(s) if !self.is_active(s) => Some(s),
                _ => None,
            });
        let most_recent = || {
            self.stages
                .iter()
                .filter(|s| !self.is_active(s.id))
                .filter(|s| s.windows.iter().any(|w| self.windows.get(w).is_some_and(|w| w.meta.pid == pid)))
                .max_by_key(|s| s.last_active)
                .map(|s| s.id)
        };
        let target = match self.options.settings.app_windows {
            AppWindows::AllAtOnce if present => None,
            AppWindows::AllAtOnce => focused_stage.or_else(most_recent),
            AppWindows::OneAtATime => focused_stage.or_else(|| if present { None } else { most_recent() }),
        };
        match target {
            Some(s) => self.switch_to(s, obs),
            None => Vec::new(),
        }
    }

    /// A window got focus (for example through ⌘` or a click on a sliver). If it sits in an
    /// inactive stage, switch to that stage so the focused window is never left parked.
    pub fn window_focused(&mut self, id: WindowId, obs: &Observation) -> Vec<Action> {
        match self.resolve_standard(id).and_then(|w| self.stage_of(w)) {
            Some(s) if !self.is_active(s) => self.switch_to(s, obs),
            _ => Vec::new(),
        }
    }

    /// Click on the wallpaper. Parks the active stage (it enters the strip) and makes a fresh
    /// empty stage active, as Apple's reveal does. A second reveal, while that empty stage is
    /// still active and the revealed stage is still the most recent, brings the stage back.
    /// A no-op when the active stage is empty and nothing is pending.
    pub fn reveal_desktop(&mut self, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        let display = self.current_display(obs);
        let current = self.ensure_active(display);
        let current_empty = self.stage(current).is_some_and(|s| s.windows.is_empty());
        if let Some(r) = self.revealed.get(&display).copied()
            && r.desktop == current
            && current_empty
            && self.is_most_recent_inactive(display, r.from)
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
        let desktop = self.create_stage(display, None);
        self.active.insert(display, desktop);
        let t = self.tick();
        if let Some(s) = self.stage_mut(desktop) {
            s.last_active = t;
        }
        self.strip_enter(display, current);
        self.revealed.insert(display, Reveal { from: current, desktop });
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    fn is_most_recent_inactive(&self, display: DisplayId, stage: StageId) -> bool {
        let Some(s) = self.stage(stage) else { return false };
        !self.is_active(stage)
            && self
                .stages
                .iter()
                .filter(|o| o.display == display && !self.is_active(o.id))
                .all(|o| o.last_active <= s.last_active)
    }

    /// A window moved or resized. The frame becomes its restore frame unless it lies in a park
    /// zone (our own parks report back here) or is degenerate.
    pub fn window_frame_changed(&mut self, id: WindowId, frame: Rect) {
        self.record_frame(id, frame);
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
    /// visible frame. Used on quit and by safety code; the stages stay as they are.
    pub fn unpark_all(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        let parked: Vec<WindowId> = self.windows.values().filter(|w| w.parked).map(|w| w.meta.id).collect();
        for id in parked {
            self.restore_window(id, &mut actions);
        }
        actions
    }

    /// Replaces the display list (first = main). Stages on a display that is gone move to the
    /// main display; the one that was active there becomes inactive and is parked, and windows
    /// already parked are parked again on the main display. New displays get an empty active
    /// stage. An empty list is ignored.
    pub fn set_displays(&mut self, displays: Vec<Display>) -> Vec<Action> {
        let mut actions = Vec::new();
        if displays.is_empty() {
            return actions;
        }
        self.displays = displays;
        let main = self.displays[0].id;
        let live: Vec<DisplayId> = self.displays.iter().map(|d| d.id).collect();
        let gone: Vec<DisplayId> =
            self.stages.iter().map(|s| s.display).filter(|d| !live.contains(d)).collect();
        let moved: Vec<StageId> =
            self.stages.iter().filter(|s| !live.contains(&s.display)).map(|s| s.id).collect();
        for d in &gone {
            self.active.remove(d);
            self.revealed.remove(d);
        }
        for s in &moved {
            if let Some(st) = self.stage_mut(*s) {
                st.display = main;
            }
        }
        let ids: Vec<WindowId> = self.windows.keys().copied().collect();
        for id in ids {
            let home = self.windows[&id].home;
            let on_gone = match home {
                Home::Stage(s) => moved.contains(&s),
                Home::Pinned => !live.contains(&self.windows[&id].meta.display),
            };
            if on_gone && let Some(w) = self.windows.get_mut(&id) {
                w.meta.display = main;
            }
        }
        for d in live {
            self.ensure_active(d);
        }
        for s in moved {
            if !self.is_active(s) {
                self.park_stage(s, &mut actions);
                self.drop_if_dead(s, &mut actions);
            }
        }
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Replaces the declared stage names (config reload). New names get a stage; stages whose
    /// name is no longer declared lose their protection and go away if empty and inactive.
    pub fn set_named_stages(&mut self, names: Vec<String>) -> Vec<Action> {
        let mut actions = Vec::new();
        self.options.named_stages = names.clone();
        self.declare_stages(&names);
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
        is_parked(frame, self.display(display).frame)
    }
}
