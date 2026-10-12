//! The stage model: displays, stages, windows, and the operations that move between them.
//!
//! The model never touches a window itself. The platform layer reports what it sees
//! ([`WindowMeta`], [`Observation`]) and executes the [`Action`]s each operation returns, in
//! order. One invariant rules everything: **every window the model parks stays recorded with a
//! safe restore frame until it is shown again**, and whenever the model forgets a parked window
//! it first emits a [`Action::SetFrame`] that brings it back. [`Model::unpark_all`] therefore
//! always covers every parked window.

mod ops;
mod strip;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::geometry::Rect;
use super::park::{clamp_to_visible, is_parked, park_frame};
use super::settings::NativeSettings;

pub use strip::{CARD_PITCH, STRIP_MARGIN, StripCard, StripOptions, StripOrder, capacity_for_height};

/// A process ID.
pub type Pid = i32;

/// A window's `CGWindowID`: stable for the window's lifetime and unique system-wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WindowId(pub u32);

/// A stage's ID, unique for the model's lifetime (never reused).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StageId(pub u64);

/// A display's `CGDirectDisplayID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DisplayId(pub u32);

/// Whether a window is managed on its own or follows another window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRole {
    /// An `AXStandardWindow`: a stage member.
    Standard,
    /// A sheet, dialog, popover or any other subrole. It follows its parent's stage, is parked
    /// and restored with it, never appears as a stage member and never keeps a stage alive.
    /// `parent` is `None` when the platform could not tell; such a child stays with the stage
    /// it was born in.
    Child {
        /// The window it belongs to, when known.
        parent: Option<WindowId>,
    },
}

/// What the platform knows about a window.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowMeta {
    /// The window's `CGWindowID`.
    pub id: WindowId,
    /// The owning process.
    pub pid: Pid,
    /// The owning app's bundle ID (empty when unknown).
    pub bundle_id: String,
    /// The window title.
    pub title: String,
    /// Standard window or child.
    pub role: WindowRole,
    /// The frame when observed.
    pub frame: Rect,
    /// The display the window is on.
    pub display: DisplayId,
}

/// A display: its full frame and the visible frame (minus menu bar and Dock).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Display {
    /// The display's ID.
    pub id: DisplayId,
    /// The full frame in global coordinates.
    pub frame: Rect,
    /// The usable frame (no menu bar, no Dock).
    pub visible: Rect,
}

/// A group of windows shown together.
#[derive(Debug, Clone, PartialEq)]
pub struct Stage {
    /// The stage's ID.
    pub id: StageId,
    /// A name, from config (`[stages] names`) or a `stage:<name>` rule.
    pub name: Option<String>,
    /// The display the stage lives on.
    pub display: DisplayId,
    /// Standard member windows, front to back.
    pub windows: Vec<WindowId>,
    /// The window focused last time the stage was left; focused again on return.
    pub key: Option<WindowId>,
    /// Logical time the stage was last made active.
    pub last_active: u64,
}

/// Where a managed window belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Home {
    /// In a stage (children: their parent's stage).
    Stage(StageId),
    /// Pinned: on no stage, visible on every stage, never parked.
    Pinned,
}

/// Everything the model keeps about one managed window.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowState {
    /// The platform's description, kept current by the model's update calls.
    pub meta: WindowMeta,
    /// Stage or pinned.
    pub home: Home,
    /// The frame to restore when the window is shown again. Never inside a park zone.
    pub restore: Rect,
    /// True while the model has the window parked.
    pub parked: bool,
}

/// The platform's view at decision time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observation {
    /// Live frames of onscreen managed windows (parked windows included, if seen).
    pub frames: HashMap<WindowId, Rect>,
    /// Managed windows, front to back.
    pub z_order: Vec<WindowId>,
    /// The focused window, if any.
    pub focused: Option<WindowId>,
}

/// What the platform must do, in the order returned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// Move and resize a window (the platform sets size, position, then size again).
    SetFrame {
        /// The window.
        id: WindowId,
        /// The target frame.
        frame: Rect,
    },
    /// Move a window into its display's park zone (macOS clamps it; that is expected).
    Park {
        /// The window.
        id: WindowId,
        /// The park frame.
        frame: Rect,
    },
    /// Raise a window (`kAXRaiseAction`).
    Raise {
        /// The window.
        id: WindowId,
    },
    /// Raise, make main and bring the app to the front.
    Focus {
        /// The window.
        id: WindowId,
    },
    /// The model changed: persist state and refresh the strip. Always last.
    StateChanged,
}

/// Where [`Model::send_window`] sends a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Stage N (1-based, creation order). N = count + 1 creates a stage; larger is a no-op.
    Number(usize),
    /// A new stage.
    New,
    /// The stage with this name, created if missing.
    Named(String),
}

/// Model-wide options, set at construction and updated on config or settings reload.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelOptions {
    /// Apple's Stage Manager settings.
    pub settings: NativeSettings,
    /// Strip ordering and capacity.
    pub strip: StripOptions,
    /// Stages declared in config (`[stages] names`): created up front in this order, they keep
    /// their numbers and survive being empty.
    pub named_stages: Vec<String>,
}

/// State of a desktop reveal, so a second reveal brings the stage back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reveal {
    /// The stage that was active before the reveal.
    pub(crate) from: StageId,
    /// The empty stage made active by the reveal.
    pub(crate) desktop: StageId,
}

/// Nominal display used only when the platform reports none, so the model never panics.
const FALLBACK_DISPLAY: Display = Display {
    id: DisplayId(0),
    frame: Rect::new(0.0, 0.0, 1440.0, 900.0),
    visible: Rect::new(0.0, 25.0, 1440.0, 875.0),
};

/// The stage model. See the module docs for the contract.
#[derive(Debug, Clone)]
pub struct Model {
    /// Displays; the first is the main display.
    pub(crate) displays: Vec<Display>,
    /// Stages in creation order (stage N = `stages[N - 1]`).
    pub(crate) stages: Vec<Stage>,
    /// Every managed window, standard and child.
    pub(crate) windows: BTreeMap<WindowId, WindowState>,
    /// The active stage per display.
    pub(crate) active: HashMap<DisplayId, StageId>,
    /// Strip slots per display, top to bottom (native order).
    pub(crate) slots: HashMap<DisplayId, Vec<StageId>>,
    /// Pending desktop reveal per display.
    pub(crate) revealed: HashMap<DisplayId, Reveal>,
    /// Logical clock for stage recency.
    pub(crate) clock: u64,
    /// Next stage ID.
    pub(crate) next_stage: u64,
    /// Options.
    pub(crate) options: ModelOptions,
}

impl Model {
    /// A model for `displays` (the first is the main display). Declared named stages are
    /// created on the main display; the first becomes active, else an empty stage does.
    /// Every other display gets an empty active stage.
    pub fn new(displays: Vec<Display>, options: ModelOptions) -> Self {
        let displays = if displays.is_empty() {
            vec![FALLBACK_DISPLAY]
        } else {
            displays
        };
        let mut model = Self {
            displays,
            stages: Vec::new(),
            windows: BTreeMap::new(),
            active: HashMap::new(),
            slots: HashMap::new(),
            revealed: HashMap::new(),
            clock: 0,
            next_stage: 1,
            options,
        };
        let names = model.options.named_stages.clone();
        model.declare_stages(&names);
        let ids: Vec<DisplayId> = model.displays.iter().map(|d| d.id).collect();
        for id in ids {
            model.ensure_active(id);
        }
        model.normalize_slots();
        model
    }

    // ----- read access -------------------------------------------------------------------

    /// The displays; the first is the main display.
    pub fn displays(&self) -> &[Display] {
        &self.displays
    }

    /// The main display.
    pub fn main_display(&self) -> &Display {
        &self.displays[0]
    }

    /// Stages in creation order (stage N is `stages()[N - 1]`).
    pub fn stages(&self) -> &[Stage] {
        &self.stages
    }

    /// A stage by ID.
    pub fn stage(&self, id: StageId) -> Option<&Stage> {
        self.stages.iter().find(|s| s.id == id)
    }

    /// The 1-based hotkey number of a stage.
    pub fn stage_number(&self, id: StageId) -> Option<usize> {
        self.stages.iter().position(|s| s.id == id).map(|i| i + 1)
    }

    /// The active stage of a display.
    pub fn active_stage(&self, display: DisplayId) -> Option<StageId> {
        self.active.get(&display).copied()
    }

    /// The active stage of the main display.
    pub fn main_active_stage(&self) -> Option<StageId> {
        self.active_stage(self.main_display().id)
    }

    /// A managed window by ID.
    pub fn window(&self, id: WindowId) -> Option<&WindowState> {
        self.windows.get(&id)
    }

    /// Every managed window, by ID.
    pub fn windows(&self) -> impl Iterator<Item = &WindowState> {
        self.windows.values()
    }

    /// The stage a window belongs to (`None` for pinned or unknown windows).
    pub fn stage_of(&self, id: WindowId) -> Option<StageId> {
        match self.windows.get(&id)?.home {
            Home::Stage(s) => Some(s),
            Home::Pinned => None,
        }
    }

    /// Pinned standard windows.
    pub fn pinned(&self) -> Vec<WindowId> {
        self.windows
            .values()
            .filter(|w| w.home == Home::Pinned && w.meta.role == WindowRole::Standard)
            .map(|w| w.meta.id)
            .collect()
    }

    /// Every parked window with its restore frame (for the platform's safety registry).
    pub fn parked_windows(&self) -> Vec<(WindowId, Rect)> {
        self.windows
            .values()
            .filter(|w| w.parked)
            .map(|w| (w.meta.id, w.restore))
            .collect()
    }

    /// The options in force.
    pub fn options(&self) -> &ModelOptions {
        &self.options
    }

    /// Replaces Apple's settings after the platform re-read them.
    pub fn set_settings(&mut self, settings: NativeSettings) {
        self.options.settings = settings;
    }

    // ----- internal helpers --------------------------------------------------------------

    pub(crate) fn display(&self, id: DisplayId) -> &Display {
        self.displays
            .iter()
            .find(|d| d.id == id)
            .unwrap_or(&self.displays[0])
    }

    pub(crate) fn known_display(&self, id: DisplayId) -> DisplayId {
        self.display(id).id
    }

    pub(crate) fn stage_mut(&mut self, id: StageId) -> Option<&mut Stage> {
        self.stages.iter_mut().find(|s| s.id == id)
    }

    pub(crate) fn is_active(&self, stage: StageId) -> bool {
        self.active.values().any(|&s| s == stage)
    }

    pub(crate) fn is_declared(&self, name: &str) -> bool {
        self.options
            .named_stages
            .iter()
            .any(|n| n.eq_ignore_ascii_case(name))
    }

    pub(crate) fn find_named(&self, name: &str) -> Option<StageId> {
        self.stages
            .iter()
            .find(|s| {
                s.name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .map(|s| s.id)
    }

    /// The display an operation acts on: the focused window's, else the main display.
    pub(crate) fn current_display(&self, obs: &Observation) -> DisplayId {
        obs.focused
            .and_then(|id| self.windows.get(&id))
            .map(|w| self.known_display(w.meta.display))
            .unwrap_or(self.displays[0].id)
    }

    pub(crate) fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    pub(crate) fn create_stage(&mut self, display: DisplayId, name: Option<String>) -> StageId {
        let id = StageId(self.next_stage);
        self.next_stage += 1;
        let display = self.known_display(display);
        self.stages.push(Stage {
            id,
            name,
            display,
            windows: Vec::new(),
            key: None,
            last_active: 0,
        });
        id
    }

    /// Creates any declared stage that does not exist yet, on the main display.
    pub(crate) fn declare_stages(&mut self, names: &[String]) {
        let main = self.displays[0].id;
        for name in names {
            if self.find_named(name).is_none() {
                self.create_stage(main, Some(name.clone()));
            }
        }
    }

    /// Gives `display` an active stage if it has none: its first stage if any, else a new one.
    pub(crate) fn ensure_active(&mut self, display: DisplayId) -> StageId {
        if let Some(&s) = self.active.get(&display) {
            return s;
        }
        let stage = match self.stages.iter().find(|s| s.display == display) {
            Some(s) => s.id,
            None => self.create_stage(display, None),
        };
        self.active.insert(display, stage);
        let t = self.tick();
        if let Some(s) = self.stage_mut(stage) {
            s.last_active = t;
        }
        stage
    }

    /// The standard window a window stands for: itself, or the parent chain of a child.
    pub(crate) fn resolve_standard(&self, id: WindowId) -> Option<WindowId> {
        let mut cur = id;
        for _ in 0..8 {
            let w = self.windows.get(&cur)?;
            match w.meta.role {
                WindowRole::Standard => return Some(cur),
                WindowRole::Child { parent: Some(p) } if self.windows.contains_key(&p) => cur = p,
                WindowRole::Child { .. } => return None,
            }
        }
        None
    }

    /// Children (direct and nested) of a window, in ID order.
    pub(crate) fn children_of(&self, id: WindowId) -> Vec<WindowId> {
        let mut out = Vec::new();
        let mut frontier = vec![id];
        while let Some(p) = frontier.pop() {
            for w in self.windows.values() {
                if w.meta.role == (WindowRole::Child { parent: Some(p) })
                    && !out.contains(&w.meta.id)
                {
                    out.push(w.meta.id);
                    frontier.push(w.meta.id);
                }
            }
        }
        out
    }

    /// Children whose home is `home` (all children follow their parent's home).
    pub(crate) fn children_in(&self, home: Home) -> Vec<WindowId> {
        self.windows
            .values()
            .filter(|w| matches!(w.meta.role, WindowRole::Child { .. }) && w.home == home)
            .map(|w| w.meta.id)
            .collect()
    }

    /// Sets the home of a window and all its children.
    pub(crate) fn set_home(&mut self, id: WindowId, home: Home, display: DisplayId) {
        let mut ids = self.children_of(id);
        ids.push(id);
        for i in ids {
            if let Some(w) = self.windows.get_mut(&i) {
                w.home = home;
                w.meta.display = display;
            }
        }
    }

    /// Records `frame` as a window's restore frame unless it is unusable (in a park zone or
    /// degenerate). Returns whether it was taken.
    pub(crate) fn record_frame(&mut self, id: WindowId, frame: Rect) -> bool {
        let Some(w) = self.windows.get(&id) else {
            return false;
        };
        let display = *self.display(w.meta.display);
        let usable = !frame.is_degenerate()
            && !is_parked(frame, display.frame)
            && !is_parked(frame, display.visible);
        if let Some(w) = self.windows.get_mut(&id) {
            w.meta.frame = frame;
            if usable {
                w.restore = frame;
            }
        }
        usable
    }

    /// A restore frame that is safe to record for a new window: `frame`, or a centered one if
    /// `frame` is unusable.
    pub(crate) fn safe_restore(&self, frame: Rect, display: DisplayId) -> Rect {
        let d = self.display(display);
        if frame.is_degenerate() || is_parked(frame, d.frame) || is_parked(frame, d.visible) {
            clamp_to_visible(frame, d.visible)
        } else {
            frame
        }
    }

    /// Parks one window (no-op for pinned windows).
    pub(crate) fn park_window(&mut self, id: WindowId, actions: &mut Vec<Action>) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if w.home == Home::Pinned {
            return;
        }
        let frame = park_frame(w.restore.size(), self.display(w.meta.display).frame);
        if let Some(w) = self.windows.get_mut(&id) {
            w.parked = true;
        }
        actions.push(Action::Park { id, frame });
    }

    /// Brings one window to its clamped restore frame.
    pub(crate) fn restore_window(&mut self, id: WindowId, actions: &mut Vec<Action>) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let frame = clamp_to_visible(w.restore, self.display(w.meta.display).visible);
        if let Some(w) = self.windows.get_mut(&id) {
            w.parked = false;
        }
        actions.push(Action::SetFrame { id, frame });
    }

    /// Saves the live state of a stage from `obs`: frames, z-order, key window.
    pub(crate) fn capture(&mut self, stage: StageId, obs: &Observation) {
        let Some(s) = self.stage(stage) else { return };
        let mut ids = s.windows.clone();
        ids.extend(self.children_in(Home::Stage(stage)));
        for id in ids {
            if let Some(&f) = obs.frames.get(&id) {
                self.record_frame(id, f);
            }
        }
        let focused_member = obs.focused.and_then(|f| self.resolve_standard(f));
        let Some(s) = self.stage_mut(stage) else {
            return;
        };
        let z = |id: &WindowId| {
            obs.z_order
                .iter()
                .position(|x| x == id)
                .unwrap_or(usize::MAX)
        };
        s.windows.sort_by_key(z);
        s.key = match focused_member {
            Some(f) if s.windows.contains(&f) => Some(f),
            _ => s.windows.first().copied(),
        };
    }

    /// Parks a stage's members and children.
    pub(crate) fn park_stage(&mut self, stage: StageId, actions: &mut Vec<Action>) {
        let Some(s) = self.stage(stage) else { return };
        let mut ids = s.windows.clone();
        ids.extend(self.children_in(Home::Stage(stage)));
        for id in ids {
            self.park_window(id, actions);
        }
    }

    /// Shows a stage: restore frames, raise back to front (each window's children right after
    /// it), focus the key window last.
    pub(crate) fn show_stage(&mut self, stage: StageId, actions: &mut Vec<Action>) {
        let Some(s) = self.stage(stage) else { return };
        let members = s.windows.clone();
        let key = s
            .key
            .filter(|k| members.contains(k))
            .or(members.first().copied());
        let orphans: Vec<WindowId> = self
            .children_in(Home::Stage(stage))
            .into_iter()
            .filter(|&c| self.resolve_standard(c).is_none())
            .collect();
        let mut order = Vec::new();
        for &m in members.iter().rev() {
            order.push(m);
            order.extend(self.children_of(m));
        }
        order.extend(orphans);
        for &id in &order {
            self.restore_window(id, actions);
        }
        for &id in &order {
            actions.push(Action::Raise { id });
        }
        if let Some(k) = key {
            actions.push(Action::Focus { id: k });
        }
    }

    /// True when a stage must be kept: active, has a standard member, or is declared.
    pub(crate) fn stage_is_alive(&self, stage: StageId) -> bool {
        let Some(s) = self.stage(stage) else {
            return false;
        };
        self.is_active(stage)
            || !s.windows.is_empty()
            || s.name.as_deref().is_some_and(|n| self.is_declared(n))
    }

    /// Removes a stage that is no longer alive. Children left in it (orphans) move to the
    /// display's active stage and are shown, so no parked window is ever forgotten.
    pub(crate) fn drop_if_dead(&mut self, stage: StageId, actions: &mut Vec<Action>) {
        if self.stage(stage).is_none() || self.stage_is_alive(stage) {
            return;
        }
        let display = self
            .stage(stage)
            .map(|s| s.display)
            .unwrap_or(self.displays[0].id);
        let leftovers = self.children_in(Home::Stage(stage));
        self.stages.retain(|s| s.id != stage);
        self.revealed
            .retain(|_, r| r.from != stage && r.desktop != stage);
        if let Some(slots) = self.slots.get_mut(&display) {
            slots.retain(|&s| s != stage);
        }
        let to = self.ensure_active(display);
        for c in leftovers {
            if let Some(w) = self.windows.get_mut(&c) {
                w.home = Home::Stage(to);
                if w.parked {
                    self.restore_window(c, actions);
                }
            }
        }
    }
}
