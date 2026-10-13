//! AppKit UI: status item, strip panel and the motion overlay.

pub mod motion_math;
pub mod overlay;
pub mod panels;
pub mod status_menu;
pub mod strip;
pub mod strip_demo;
pub mod strip_layout;
pub mod strip_model;
#[cfg(feature = "debug-render")]
pub mod strip_render;
pub mod strip_visibility;
