//! Test helpers: a fake desktop that executes actions, window builders, and the engine
//! matrix. Made-up titles only.
//!
//! **The engine matrix.** The general suite runs once per window engine: a test written
//! inside [`both_engines!`] becomes two, `<name>::corner` and `<name>::spaces`, each
//! running the same body with [`engine`] set ([`on`]). [`model`], [`model_with`],
//! [`model_on`] and a new [`Desk`] take the engine from there, so a test body never names
//! one. A test that checks one engine's own geometry sits in [`corner_nuance!`] (the park
//! corner) or in that engine's module (`engine::spaces::tests`) and names its engine.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

pub use super::corner::park_frame;
use super::displays::ScreenKey;
use super::geometry::Rect;
use super::corner::park::MACOS_KEEPS_TITLE;
use super::model::{
    Action, Display, DisplayId, Model, ModelOptions, Observation, Pid, WindowId, WindowMeta,
    WindowRole,
};
use super::recovery::EngineKind;

thread_local! {
    /// The engine the matrix runs this test on ([`on`]); `None` outside it.
    static ENGINE: Cell<Option<EngineKind>> = const { Cell::new(None) };
}

/// The engine the matrix runs this test on ([`both_engines!`]); `None` in a test outside
/// it, where the model's options decide (the Corner engine by default).
pub fn engine() -> Option<EngineKind> {
    ENGINE.with(Cell::get)
}

/// Runs `f` with [`engine`] set to `kind`, and clears it after, also when `f` panics.
pub fn on(kind: EngineKind, f: impl FnOnce()) {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            ENGINE.with(|e| e.set(None));
        }
    }
    ENGINE.with(|e| e.set(Some(kind)));
    let _clear = Clear;
    f();
}

/// Tests that run once per window engine: each `#[test] fn name() { .. }` inside becomes
/// the plain function `name` and a module `name` of two tests, `corner` and `spaces`, that
/// call it [`on`] each engine. Doc comments go before `#[test]`; attributes after it (such
/// as `#[should_panic]`) go on both tests. Only tests inside: helpers stay outside.
macro_rules! both_engines {
    ($( $(#[doc = $doc:literal])* #[test] $(#[$attr:meta])* fn $name:ident() $body:block )*) => {
        $(
            $(#[doc = $doc])*
            fn $name() $body

            mod $name {
                #[test]
                $(#[$attr])*
                fn corner() {
                    $crate::engine::testkit::on($crate::engine::recovery::EngineKind::Corner, super::$name);
                }

                #[test]
                $(#[$attr])*
                fn spaces() {
                    $crate::engine::testkit::on($crate::engine::recovery::EngineKind::Spaces, super::$name);
                }
            }
        )*
    };
}
pub(crate) use both_engines;

/// Tests of the Corner engine's own geometry (the park zone, the park frame, how macOS
/// clamps a parked window): they run once, on the Corner engine, and say so where they
/// stand. The items pass through unchanged.
macro_rules! corner_nuance {
    ($($item:item)*) => {
        $($item)*
    };
}
pub(crate) use corner_nuance;

pub const MAIN: DisplayId = DisplayId(1);
pub const FRAME: Rect = Rect::new(0.0, 0.0, 1440.0, 900.0);
/// A menu bar on top and a 75 pt Dock at the bottom (the review's S1 case: macOS keeps a
/// parked window's title bar above it, outside the full frame's park zone).
pub const VISIBLE: Rect = Rect::new(0.0, 25.0, 1440.0, 800.0);

/// True when `f` is in the test display's park zone (the one park test, [`Display::parks`]).
pub fn parked_at(f: Rect) -> bool {
    display().parks(f)
}

/// Where macOS leaves a window asked to sit at `asked` on the test display: its title bar
/// kept [`MACOS_KEEPS_TITLE`] points above the visible frame's bottom (above the Dock).
pub fn macos_places(asked: Rect) -> Rect {
    macos_places_in(asked, VISIBLE)
}

/// [`macos_places`] on a display whose visible frame is `visible`.
pub fn macos_places_in(asked: Rect, visible: Rect) -> Rect {
    Rect::new(asked.x, asked.y.min(visible.max_y() - MACOS_KEEPS_TITLE), asked.w, asked.h)
}

pub fn display() -> Display {
    Display {
        id: MAIN,
        key: ScreenKey::new(0x610, 0xae2f, 0x4d6),
        frame: FRAME,
        visible: VISIBLE,
        corner: Default::default(),
    }
}

/// A model of the test [`display`] with default options, on the matrix's [`engine`].
pub fn model() -> Model {
    model_with(ModelOptions::default())
}

/// A model of the test [`display`] with `options`, on the matrix's [`engine`] when the test
/// runs in it (else on `options.engine`).
pub fn model_with(options: ModelOptions) -> Model {
    model_on(vec![display()], options)
}

/// A model of `displays` with `options`, on the matrix's [`engine`] when the test runs in it
/// (else on `options.engine`).
pub fn model_on(displays: Vec<Display>, mut options: ModelOptions) -> Model {
    if let Some(kind) = engine() {
        options.engine = kind;
    }
    Model::new(displays, options)
}

pub fn w(id: u32) -> WindowId {
    WindowId(id)
}

/// A standard window with a frame derived from its ID.
pub fn meta(id: u32, pid: Pid, bundle: &str) -> WindowMeta {
    let k = f64::from(id % 10);
    WindowMeta {
        id: WindowId(id),
        pid,
        bundle_id: bundle.to_string(),
        title: format!("Doc {id}"),
        role: WindowRole::Standard,
        frame: Rect::new(100.0 + 20.0 * k, 80.0 + 15.0 * k, 600.0, 400.0),
        display: MAIN,
    }
}

pub fn child(id: u32, pid: Pid, parent: Option<u32>) -> WindowMeta {
    WindowMeta {
        role: WindowRole::Child {
            parent: parent.map(WindowId),
        },
        frame: Rect::new(300.0, 200.0, 300.0, 150.0),
        title: "Save".to_string(),
        ..meta(id, pid, "com.example.app")
    }
}

/// A fake desktop: window frames, z-order and focus, mutated by executing actions the way
/// the model's window engine would ([`Desk::apply`]).
#[derive(Debug, Clone)]
pub struct Desk {
    pub frames: BTreeMap<WindowId, Rect>,
    /// Front to back.
    pub z: Vec<WindowId>,
    pub focused: Option<WindowId>,
    /// The display now: macOS clamps against its visible frame, and its park corner says
    /// which windows are parked (`None`: the test [`display`]).
    pub display: Option<Display>,
    /// The engine that hides here: the matrix's [`engine`] when the desk is made, then the
    /// model's at every [`Desk::apply`].
    pub engine: EngineKind,
    /// The Spaces engine's stage Spaces, as one: windows hidden there keep their frames and
    /// are off screen (not in [`Desk::obs`]).
    pub stage_space: BTreeSet<WindowId>,
}

impl Default for Desk {
    fn default() -> Self {
        Self {
            frames: BTreeMap::new(),
            z: Vec::new(),
            focused: None,
            display: None,
            engine: engine().unwrap_or_default(),
            stage_space: BTreeSet::new(),
        }
    }
}

impl Desk {
    pub fn open(&mut self, m: &WindowMeta) {
        self.frames.insert(m.id, m.frame);
        self.stage_space.remove(&m.id);
        self.z.retain(|&x| x != m.id);
        self.z.insert(0, m.id);
        self.focused = Some(m.id);
    }

    pub fn close(&mut self, id: WindowId) {
        self.frames.remove(&id);
        self.stage_space.remove(&id);
        self.z.retain(|&x| x != id);
        if self.focused == Some(id) {
            self.focused = None;
        }
    }

    pub fn raise(&mut self, id: WindowId) {
        if self.frames.contains_key(&id) {
            self.z.retain(|&x| x != id);
            self.z.insert(0, id);
        }
    }

    /// Executes `actions` as model `m`'s window engine does. A window hidden by the Corner
    /// engine goes to its park frame as `m` knows it now ([`park_frame`]), where macOS leaves
    /// it; by the Spaces engine it goes into the stage Space at its own frame. A frame set
    /// brings a window out of the stage Space.
    pub fn apply(&mut self, m: &Model, actions: &[Action]) {
        self.engine = m.options().engine;
        for a in actions {
            match *a {
                Action::SetFrame { id, frame } => {
                    self.stage_space.remove(&id);
                    self.place(id, frame);
                }
                Action::Hide { id } => match self.engine {
                    EngineKind::Corner => {
                        if let Some(frame) = park_frame(m, id) {
                            self.place(id, frame);
                        }
                    }
                    EngineKind::Spaces => {
                        if self.frames.contains_key(&id) {
                            self.stage_space.insert(id);
                        }
                    }
                    // Apple's strip takes it: the model's hide is dropped, nothing moves.
                    EngineKind::Apple => {}
                },
                Action::Raise { id } => self.raise(id),
                Action::Focus { id } => {
                    if self.frames.contains_key(&id) {
                        self.raise(id);
                        self.focused = Some(id);
                    }
                }
                Action::StateChanged => {}
            }
        }
    }

    /// Window `id` asked to sit at `frame`: where macOS leaves it on the desk's display.
    fn place(&mut self, id: WindowId, frame: Rect) {
        if let Some(f) = self.frames.get_mut(&id) {
            *f = macos_places_in(frame, self.display.unwrap_or_else(display).visible);
        }
    }

    /// What the platform observes: the windows on screen, front to back, with their frames.
    /// A window in the stage Space is off screen, so it is in neither (the window server
    /// lists it only to a query of every window).
    pub fn obs(&self) -> Observation {
        Observation {
            frames: self.frames.iter().filter(|(k, _)| !self.stage_space.contains(k)).map(|(k, v)| (*k, *v)).collect(),
            z_order: self.z.iter().copied().filter(|k| !self.stage_space.contains(k)).collect(),
            focused: self.focused,
        }
    }

    /// True when window `id` is hidden by the desk's engine: in the display's park zone
    /// (Corner), in the stage Space (Spaces).
    pub fn parked(&self, id: WindowId) -> bool {
        match self.engine {
            EngineKind::Corner => self.frames.get(&id).is_some_and(|f| self.display.unwrap_or_else(display).parks(*f)),
            EngineKind::Spaces => self.stage_space.contains(&id),
            EngineKind::Apple => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    both_engines! {
        #[test]
        fn the_matrix_hands_its_engine_to_the_model_and_the_desk() {
            let kind = engine().expect("run by the matrix");
            assert_eq!(model().options().engine, kind);
            assert_eq!(model_with(ModelOptions::default()).options().engine, kind);
            assert_eq!(Desk::default().engine, kind);
        }
    }

    #[test]
    fn outside_the_matrix_the_options_name_the_engine() {
        assert_eq!(engine(), None);
        assert_eq!(model().options().engine, EngineKind::Corner);
        let spaces = model_with(ModelOptions { engine: EngineKind::Spaces, ..ModelOptions::default() });
        assert_eq!(spaces.options().engine, EngineKind::Spaces);
        on(EngineKind::Spaces, || assert_eq!(engine(), Some(EngineKind::Spaces)));
        assert_eq!(engine(), None, "cleared after");
    }
}
