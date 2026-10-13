//! Stage Manager: a floating-window stage manager for macOS.
//!
//! `engine` and `config` are pure Rust and unit tested; `platform` and `ui` hold the
//! macOS glue.

pub mod config;
pub mod engine;
pub mod logbook;
#[cfg(target_os = "macos")]
pub mod platform;
#[cfg(target_os = "macos")]
pub mod ui;
