//! Pure Rust engine: stage model, rules, placement, persistence. No Objective-C types.
//!
//! The engine compiles and tests on any OS. The platform layer feeds it observations and
//! executes the [`model::Action`]s it returns.

pub(crate) mod apple;
pub(crate) mod attribution;
pub(crate) mod ax_blind;
pub(crate) mod busy_apps;
pub(crate) mod card_park;
pub(crate) mod causes;
pub mod corner;
pub(crate) mod desk_reads;
pub(crate) mod desk_shadow;
pub(crate) mod desktop_click;
pub(crate) mod desktop_echo;
pub(crate) mod displays;
pub(crate) mod early_start;
pub(crate) mod engine_choice;
pub(crate) mod enhanced_ui;
pub(crate) mod focus_elsewhere;
pub(crate) mod focus_replay;
pub mod fullscreen;
pub mod geometry;
pub(crate) mod gone;
pub(crate) mod guard_watch;
pub(crate) mod handoff;
pub(crate) mod hidden_tab;
pub mod leftovers;
pub(crate) mod landing_order;
pub(crate) mod lend_park;
pub(crate) mod lend_round;
pub(crate) mod lend_wait;
pub(crate) mod margin;
pub mod logout_watch;
pub(crate) mod minimize;
pub mod minimize_press;
pub(crate) mod mission_close;
pub(crate) mod mission_control;
pub mod model;
pub(crate) mod not_ours;
pub(crate) mod observe_retry;
pub(crate) mod overview;
pub(crate) mod persist;
pub(crate) mod perspective;
pub(crate) mod phase;
pub(crate) mod placement;
pub(crate) mod raise_order;
pub(crate) mod recovery;
pub(crate) mod reopen_list;
pub(crate) mod rescan;
pub(crate) mod restore;
pub(crate) mod rules;
pub(crate) mod save_clock;
pub(crate) mod server_notice;
pub(crate) mod settings;
pub(crate) mod setup_frames;
pub(crate) mod show_desktop;
pub(crate) mod slow_task;
pub(crate) mod slide_hold;
pub(crate) mod slide_watch;
pub(crate) mod solar_settle;
pub(crate) mod spaces;
pub(crate) mod spill;
pub(crate) mod stale_activation;
pub(crate) mod stale_handle;
pub(crate) mod vanished;
pub(crate) mod stage_area;
pub(crate) mod stage_companions;
pub mod stand_down;
pub(crate) mod stranded;
pub(crate) mod strip_cover;
pub(crate) mod switch_landing;
pub(crate) mod switcher;
pub(crate) mod tick_pace;
pub(crate) mod wallpaper;
pub(crate) mod window_match;
#[cfg(test)]
pub(crate) mod testkit;

pub use geometry::{Point, Rect, Size};
pub use model::{
    Action, Display, DisplayId, Home, Model, ModelOptions, NativeTarget, Observation, Pid, Stage, StageId,
    ActiveCard, DockApp, StripCard, StripOptions, StripOrder, Target, WindowId, WindowMeta, WindowRole, WindowState,
};
pub use persist::{Assignment, LaunchHints, StateFile, rematch};
pub use rules::{Decision, Placement, RuleAction, RuleSet};
pub use settings::{AppWindows, NativeSettings, PrefValue};
