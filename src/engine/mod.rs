//! Pure Rust engine: stage model, rules, placement, persistence. No Objective-C types.
//!
//! The engine compiles and tests on any OS. The platform layer feeds it observations and
//! executes the [`model::Action`]s it returns.

pub mod busy_apps;
pub mod corner_patch;
pub mod desktop_echo;
pub mod displays;
pub mod focus_replay;
pub mod fullscreen;
pub mod geometry;
pub mod guard_watch;
pub mod hide;
pub mod leftovers;
pub mod model;
pub mod overview;
pub mod park;
pub mod persist;
pub mod placement;
pub mod recovery;
pub mod rescan;
pub mod restore;
pub mod rules;
pub mod settings;
pub mod sliver;
pub mod stage_area;
pub mod stand_down;
pub mod wallpaper;
pub mod window_match;
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
