//! AppKit UI: status item, strip panel and the motion overlay; the app's own windows
//! (Settings, the welcome, About, update alerts) and the shell that runs them.

pub mod about;
pub mod alert;
pub mod app_menu;
pub mod card_glass;
pub mod corner_patch;
pub mod corner_settle;
pub mod display_options;
pub mod form;
pub mod glass_math;
pub mod glyph;
pub mod motion_math;
pub mod overlay;
pub mod panels;
pub mod patch_probe;
pub mod preview;
pub mod settings_window;
pub mod shadow_look;
pub mod shadow_math;
pub mod shell;
pub mod status_menu;
pub mod strip;
pub mod strip_access;
pub mod strip_demo;
pub mod strip_layout;
pub mod strip_model;
#[cfg(feature = "debug-render")]
pub mod strip_render;
pub mod strip_visibility;
pub mod transition_plan;
pub mod welcome;
pub mod window_shadow;
