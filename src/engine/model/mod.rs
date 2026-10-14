//! The stage model: displays, stages, windows, and the operations that move between them.
//!
//! The model never touches a window itself. The platform layer reports what it sees
//! ([`WindowMeta`], [`Observation`]) and executes the [`Action`]s each operation returns, in
//! order. One invariant rules everything: **every window the model parks stays recorded with a
//! safe restore frame until it is shown again**, and whenever the model forgets a parked window
//! it first emits a [`Action::SetFrame`] that brings it back. [`Model::unpark_all`] therefore
//! always covers every parked window.
//!
//! **Spaces.** Every Space (Desktop) has its own Stage Manager, as Apple's does: its own
//! stages, active stage, strip order and recency. A stage belongs to one Space ([`SpaceId`],
//! a synthetic id the model gives each Space as the platform reports visits, see
//! [`Model::arrive`]); operations act on the current Space of a display only, so nothing
//! ever raises or focuses a window on another Space (which would make macOS switch
//! Spaces). Only the safety paths ([`Model::unpark_all`]) and the arrangement of a Desktop
//! macOS is about to show for a window the user went to ([`Model::arrange_ahead`], moves
//! only, undone when another Desktop comes) touch windows of a Space not shown.

mod ahead;
mod hide;
mod ops;
mod spaces;
mod strip;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::geometry::Rect;
use super::park::{Corner, ParkRequest, clamp_to_visible, is_parked_in_at, park_frame_asked};
use super::settings::NativeSettings;
use super::stage_area::{FitOptions, fit_frame, same_frame, same_size, stage_area};

pub use ahead::{Ahead, AheadMark, AppFacts};
pub use spaces::{ArrivalNote, Space, SpaceId};
pub use strip::{StripCard, StripOptions, StripOrder};

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
    /// Where its inactive stages park: the corner with no other display beyond it
    /// ([`crate::engine::park::choose_corner`], chosen by the platform from the arrangement).
    pub corner: Corner,
}

impl Display {
    /// True when `frame` sits in this display's park zone at its corner, where macOS really
    /// leaves a parked window (above a bottom Dock too): the one park test
    /// ([`is_parked_in_at`]).
    pub fn parks(&self, frame: Rect) -> bool {
        is_parked_in_at(frame, self.frame, self.visible, self.corner)
    }
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
    /// The Space (Desktop) the stage lives on; its windows are all there.
    pub space: SpaceId,
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
    /// The frame the window had before we fitted it into the stage area (moved it clear of
    /// the strip, or narrowed it). Quit, unpark-all and every safety path put the window
    /// back here; the user resizing the window forgets it, and moving it carries it along.
    pub original: Option<Rect>,
    /// The Space the window was last seen on (for a stage member, its stage's Space).
    pub space: SpaceId,
    /// Seen on every Space ("Assign To: All Desktops"): treated as pinned, never moved
    /// between Spaces and never counted when telling Spaces apart.
    pub sticky: bool,
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

/// Where Apple's default puts a new window ([`Model::native_target`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTarget {
    /// Join this stage (the active one, or the app's parked stage, which comes forward).
    Join(StageId),
    /// A stage of its own.
    Fresh,
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
    /// Keeping placed windows clear of the strip (off by default; the platform sets it).
    pub fit: FitOptions,
    /// How park frames are asked (`[windows] park_inside`, `park_drop`).
    pub park: ParkRequest,
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
    corner: Corner::BottomRight,
};

/// The stage model. See the module docs for the contract.
#[derive(Debug, Clone)]
pub struct Model {
    /// Displays; the first is the main display.
    pub(crate) displays: Vec<Display>,
    /// Stages in creation order (stage N of a Space = its Nth stage in this list).
    pub(crate) stages: Vec<Stage>,
    /// Every managed window, standard and child.
    pub(crate) windows: BTreeMap<WindowId, WindowState>,
    /// The active stage per Space.
    pub(crate) active: HashMap<SpaceId, StageId>,
    /// Strip slots per Space, top to bottom (native order).
    pub(crate) slots: HashMap<SpaceId, Vec<StageId>>,
    /// Pending desktop reveal per Space.
    pub(crate) revealed: HashMap<SpaceId, Reveal>,
    /// Spaces visited so far (and the ones of a new display), in creation order.
    pub(crate) spaces: Vec<Space>,
    /// The Space each display shows now.
    pub(crate) current: HashMap<DisplayId, SpaceId>,
    /// Next synthetic Space id.
    pub(crate) next_space: u64,
    /// Saved Spaces from `state.json` not visited yet this run: their stages are rebuilt on
    /// the first visit ([`Model::arrive`]) and saved again untouched until then.
    pub(crate) leftovers: Vec<super::persist::SavedSpace>,
    /// Windows that moved to the current Space at a Space change, with the Space they came
    /// from: seen there again later, they are on every Space (sticky).
    pub(crate) moved_from: HashMap<WindowId, SpaceId>,
    /// Windows seen on screen on a Space other than their own at consecutive arrivals: how
    /// many in a row ([`spaces::STICKY_HOPS`] make a window sticky).
    pub(crate) hops: HashMap<WindowId, u32>,
    /// Known Spaces found on screen beside the one an arrival chose (more of their windows
    /// on screen than missing) at consecutive arrivals: how many in a row
    /// ([`spaces::BESIDE_ARRIVALS`] and their windows count as moved).
    pub(crate) beside: HashMap<SpaceId, u32>,
    /// What the last arrival did, for the log.
    pub(crate) last_arrival: spaces::ArrivalNote,
    /// Logical clock for stage recency.
    pub(crate) clock: u64,
    /// Next stage ID.
    pub(crate) next_stage: u64,
    /// Options.
    pub(crate) options: ModelOptions,
}

impl Model {
    /// A model for `displays` (the first is the main display), each showing a Space of its
    /// own. Declared named stages are created on the main display's Space; the first becomes
    /// active, else an empty stage does. Every other display gets an empty active stage.
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
            spaces: Vec::new(),
            current: HashMap::new(),
            next_space: 1,
            leftovers: Vec::new(),
            moved_from: HashMap::new(),
            hops: HashMap::new(),
            beside: HashMap::new(),
            last_arrival: spaces::ArrivalNote::default(),
            clock: 0,
            next_stage: 1,
            options,
        };
        let ids: Vec<DisplayId> = model.displays.iter().map(|d| d.id).collect();
        for id in &ids {
            let space = model.new_space(*id);
            model.current.insert(*id, space);
        }
        let names = model.options.named_stages.clone();
        let main = model.current_space(ids[0]);
        model.declare_stages(main, &names);
        for id in ids {
            let space = model.current_space(id);
            model.ensure_active(space);
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

    /// Stages of every Space in creation order (see [`Model::stages_in`] for one Space's).
    pub fn stages(&self) -> &[Stage] {
        &self.stages
    }

    /// A stage by ID.
    pub fn stage(&self, id: StageId) -> Option<&Stage> {
        self.stages.iter().find(|s| s.id == id)
    }

    /// The 1-based hotkey number of a stage: its place among its Space's stages.
    pub fn stage_number(&self, id: StageId) -> Option<usize> {
        let space = self.stage(id)?.space;
        self.stages
            .iter()
            .filter(|s| s.space == space)
            .position(|s| s.id == id)
            .map(|i| i + 1)
    }

    /// The active stage of a display (on the Space it shows now).
    pub fn active_stage(&self, display: DisplayId) -> Option<StageId> {
        self.active.get(&self.current_space(display)).copied()
    }

    /// The active stage of a Space.
    pub fn active_stage_in(&self, space: SpaceId) -> Option<StageId> {
        self.active.get(&space).copied()
    }

    /// The active stage of the main display.
    pub fn main_active_stage(&self) -> Option<StageId> {
        self.active_stage(self.main_display().id)
    }

    /// True while the main display shows the desktop a wallpaper click revealed: the empty
    /// stage the reveal made active is still active and still empty.
    pub fn desktop_revealed(&self) -> bool {
        let space = self.current_space(self.main_display().id);
        self.revealed.get(&space).is_some_and(|r| {
            self.active.get(&space) == Some(&r.desktop) && self.stage(r.desktop).is_some_and(|s| s.windows.is_empty())
        })
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

    /// Every window a safety path must move back, with where to: parked windows and windows
    /// we fitted into the stage area. The target is the window's pre-fit frame when it has one
    /// (exactly, so its size comes back), else its restore frame clamped onto the visible
    /// frame.
    pub fn safety_targets(&self) -> Vec<(WindowId, Rect)> {
        self.windows
            .values()
            .filter(|w| w.parked || w.original.is_some())
            .map(|w| (w.meta.id, self.safety_target(w)))
            .collect()
    }

    pub(crate) fn safety_target(&self, w: &WindowState) -> Rect {
        let d = self.display(w.meta.display);
        match w.original {
            Some(o) if !o.is_degenerate() && !d.parks(o) => o,
            Some(o) => clamp_to_visible(o, d.visible),
            None => clamp_to_visible(w.restore, d.visible),
        }
    }

    /// Sets how placed windows are kept clear of the strip.
    pub fn set_fit(&mut self, fit: FitOptions) {
        self.options.fit = fit;
    }

    /// How park frames are asked from now on (windows parked already stay where they are).
    pub fn set_park_request(&mut self, req: ParkRequest) {
        self.options.park = req;
    }

    /// Every parked window with its restore frame.
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

    /// The stage of `space` named `name` (case-insensitive).
    pub(crate) fn find_named(&self, space: SpaceId, name: &str) -> Option<StageId> {
        self.stages
            .iter()
            .find(|s| {
                s.space == space
                    && s.name
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

    /// The Space an operation acts on: the one the current display shows.
    pub(crate) fn current_space_for(&self, obs: &Observation) -> SpaceId {
        self.current_space(self.current_display(obs))
    }

    pub(crate) fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// A new stage on `space` (and its display).
    pub(crate) fn create_stage(&mut self, space: SpaceId, name: Option<String>) -> StageId {
        let id = StageId(self.next_stage);
        self.next_stage += 1;
        let display = self.space_display(space);
        self.stages.push(Stage {
            id,
            name,
            display,
            space,
            windows: Vec::new(),
            key: None,
            last_active: 0,
        });
        id
    }

    /// Creates any declared stage that does not exist yet on `space`.
    pub(crate) fn declare_stages(&mut self, space: SpaceId, names: &[String]) {
        for name in names {
            if self.find_named(space, name).is_none() {
                self.create_stage(space, Some(name.clone()));
            }
        }
    }

    /// Gives `space` an active stage if it has none: its first stage if any, else a new one.
    pub(crate) fn ensure_active(&mut self, space: SpaceId) -> StageId {
        if let Some(&s) = self.active.get(&space) {
            return s;
        }
        let stage = match self.stages.iter().find(|s| s.space == space) {
            Some(s) => s.id,
            None => self.create_stage(space, None),
        };
        self.active.insert(space, stage);
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

    /// The windows [`Model::window_left_space`] lets go of for `id`: the standard window it
    /// stands for and that window's children (empty when it has none).
    pub fn group_of(&self, id: WindowId) -> Vec<u32> {
        let Some(top) = self.resolve_standard(id) else { return Vec::new() };
        std::iter::once(top).chain(self.children_of(top)).map(|w| w.0).collect()
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
    ///
    /// A frame that differs from the restore frame is the user's: resizing forgets the
    /// window's pre-fit frame, moving carries it along by the same offset.
    pub(crate) fn record_frame(&mut self, id: WindowId, frame: Rect) -> bool {
        self.record(id, frame, true)
    }

    /// Like [`Model::record_frame`] for a frame that results from our own action (macOS may
    /// have clamped what we asked for): the pre-fit frame is kept as it is.
    pub(crate) fn record_own_frame(&mut self, id: WindowId, frame: Rect) -> bool {
        self.record(id, frame, false)
    }

    fn record(&mut self, id: WindowId, frame: Rect, by_user: bool) -> bool {
        let Some(w) = self.windows.get(&id) else {
            return false;
        };
        let display = *self.display(w.meta.display);
        let usable = !frame.is_degenerate() && !display.parks(frame);
        if let Some(w) = self.windows.get_mut(&id) {
            w.meta.frame = frame;
            if usable {
                if by_user
                    && let Some(o) = w.original
                    && !same_frame(frame, w.restore)
                {
                    w.original = if !same_size(frame, w.restore) || same_size(frame, o) {
                        None
                    } else {
                        Some(o.translate(frame.x - w.restore.x, frame.y - w.restore.y))
                    };
                }
                w.restore = frame;
            }
        }
        usable
    }

    /// Fits a standard, non-pinned window's restore frame into the stage area
    /// ([`fit_frame`]) when fitting is on; remembers the frame it had before (once). Returns
    /// true when the restore frame changed.
    pub(crate) fn fit_window(&mut self, id: WindowId) -> bool {
        let fit = self.options.fit;
        if !fit.active() {
            return false;
        }
        let Some(w) = self.windows.get(&id) else {
            return false;
        };
        if w.meta.role != WindowRole::Standard || w.home == Home::Pinned {
            return false;
        }
        let visible = self.display(w.meta.display).visible;
        let from = clamp_to_visible(w.restore, visible);
        let to = fit_frame(from, stage_area(visible, fit.inset), fit.shrink);
        if same_frame(to, from) {
            return false;
        }
        if let Some(w) = self.windows.get_mut(&id) {
            w.original.get_or_insert(w.restore);
            w.restore = to;
        }
        true
    }

    /// A restore frame that is safe to record for a new window: `frame`, or a centered one if
    /// `frame` is unusable.
    pub(crate) fn safe_restore(&self, frame: Rect, display: DisplayId) -> Rect {
        let d = self.display(display);
        if frame.is_degenerate() || d.parks(frame) {
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
        let frame = self.park_frame_for(w);
        if let Some(w) = self.windows.get_mut(&id) {
            w.parked = true;
        }
        actions.push(Action::Park { id, frame });
    }

    /// The frame that parks window `w`: its restore size in its display's park corner.
    fn park_frame_for(&self, w: &WindowState) -> Rect {
        let d = self.display(w.meta.display);
        park_frame_asked(w.restore.size(), d.frame, d.corner, self.options.park)
    }

    /// Where the model left window `id`: its park frame while parked (where bringing it back
    /// starts from), else its last frame. `meta.frame` alone does not say it for a parked
    /// window: its frames are ignored while parked ([`Model::window_frame_changed`]), so it
    /// keeps the frame the window had before it was parked.
    pub fn frame_left(&self, id: WindowId) -> Option<Rect> {
        let w = self.windows.get(&id)?;
        Some(if w.parked { self.park_frame_for(w) } else { w.meta.frame })
    }

    /// Brings one window to its clamped restore frame, fitted into the stage area first.
    pub(crate) fn restore_window(&mut self, id: WindowId, actions: &mut Vec<Action>) {
        self.fit_window(id);
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
    /// Space's active stage and are shown, so no parked window is ever forgotten.
    pub(crate) fn drop_if_dead(&mut self, stage: StageId, actions: &mut Vec<Action>) {
        let Some(space) = self.stage(stage).map(|s| s.space) else {
            return;
        };
        if self.stage_is_alive(stage) {
            return;
        }
        let leftovers = self.children_in(Home::Stage(stage));
        self.stages.retain(|s| s.id != stage);
        self.revealed
            .retain(|_, r| r.from != stage && r.desktop != stage);
        if let Some(slots) = self.slots.get_mut(&space) {
            slots.retain(|&s| s != stage);
        }
        let to = self.ensure_active(space);
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
