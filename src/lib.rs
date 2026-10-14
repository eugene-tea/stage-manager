//! Stage Manager: a floating-window stage manager for macOS.
//!
//! `engine`, `config`, `updates`, `app_state` and `private_files` are pure Rust and unit
//! tested; `platform` and `ui` hold the macOS glue.

pub mod app_state;
pub mod config;
pub mod engine;
pub mod logbook;
pub mod private_files;
#[cfg(target_os = "macos")]
pub mod platform;
#[cfg(target_os = "macos")]
pub mod ui;
pub mod updates;
