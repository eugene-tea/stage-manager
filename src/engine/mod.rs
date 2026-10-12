//! Pure Rust engine: stage model, rules, placement, persistence. No Objective-C types.
//!
//! The engine compiles and tests on any OS. The platform layer feeds it observations and
//! executes the [`model::Action`]s it returns.

pub mod geometry;
pub mod model;
pub mod park;
pub mod persist;
pub mod placement;
pub mod recovery;
pub mod rules;
pub mod settings;
pub mod stage_area;
#[cfg(test)]
pub(crate) mod testkit;

pub use geometry::{Point, Rect, Size};
pub use model::{
    Action, Display, DisplayId, Home, Model, ModelOptions, NativeTarget, Observation, Pid, Stage, StageId,
    StripCard, StripOptions, StripOrder, Target, WindowId, WindowMeta, WindowRole, WindowState,
};
pub use persist::{Assignment, LaunchHints, StateFile, rematch};
pub use rules::{Decision, Placement, RuleAction, RuleSet};
pub use settings::{AppWindows, NativeSettings, PrefValue};
