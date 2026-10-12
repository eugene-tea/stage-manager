//! Test helpers: a fake desktop that executes actions, and window builders. Made-up titles only.

use std::collections::BTreeMap;

use super::geometry::Rect;
use super::model::{
    Action, Display, DisplayId, Model, ModelOptions, Observation, Pid, WindowId, WindowMeta,
    WindowRole,
};

pub const MAIN: DisplayId = DisplayId(1);
pub const FRAME: Rect = Rect::new(0.0, 0.0, 1440.0, 900.0);
pub const VISIBLE: Rect = Rect::new(0.0, 25.0, 1440.0, 875.0);

pub fn display() -> Display {
    Display { id: MAIN, frame: FRAME, visible: VISIBLE }
}

pub fn model() -> Model {
    Model::new(vec![display()], ModelOptions::default())
}

pub fn model_with(options: ModelOptions) -> Model {
    Model::new(vec![display()], options)
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
        role: WindowRole::Child { parent: parent.map(WindowId) },
        frame: Rect::new(300.0, 200.0, 300.0, 150.0),
        title: "Save".to_string(),
        ..meta(id, pid, "com.example.app")
    }
}

/// A fake desktop: window frames, z-order and focus, mutated by executing actions.
#[derive(Debug, Default, Clone)]
pub struct Desk {
    pub frames: BTreeMap<WindowId, Rect>,
    /// Front to back.
    pub z: Vec<WindowId>,
    pub focused: Option<WindowId>,
}

impl Desk {
    pub fn open(&mut self, m: &WindowMeta) {
        self.frames.insert(m.id, m.frame);
        self.z.retain(|&x| x != m.id);
        self.z.insert(0, m.id);
        self.focused = Some(m.id);
    }

    pub fn close(&mut self, id: WindowId) {
        self.frames.remove(&id);
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

    pub fn apply(&mut self, actions: &[Action]) {
        for a in actions {
            match *a {
                Action::SetFrame { id, frame } | Action::Park { id, frame } => {
                    if let Some(f) = self.frames.get_mut(&id) {
                        *f = frame;
                    }
                }
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

    pub fn obs(&self) -> Observation {
        Observation {
            frames: self.frames.iter().map(|(k, v)| (*k, *v)).collect(),
            z_order: self.z.clone(),
            focused: self.focused,
        }
    }

    pub fn parked(&self, id: WindowId) -> bool {
        self.frames.get(&id).is_some_and(|f| super::park::is_parked(*f, FRAME))
    }
}
