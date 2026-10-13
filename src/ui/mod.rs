//! AppKit UI: status item, strip panel and the motion overlay; the app's own windows
//! (Settings, the welcome, About, update alerts) and the shell that runs them.

pub mod about;
pub mod alert;
pub mod app_menu;
pub mod form;
pub mod glyph;
pub mod motion_math;
pub mod overlay;
pub mod panels;
pub mod preview;
pub mod settings_window;
pub mod shell;
pub mod status_menu;
pub mod strip;
pub mod strip_demo;
pub mod strip_layout;
pub mod strip_model;
#[cfg(feature = "debug-render")]
pub mod strip_render;
pub mod strip_visibility;
pub mod transition_plan;
pub mod welcome;
