//! macOS glue: Accessibility, CGWindowList, hotkeys, displays, safety.
//!
//! Threading: AppKit, NSWorkspace and Carbon hotkeys live on the main thread; every
//! Accessibility call runs on the WM thread ([`wm_thread`]), which owns the engine through
//! [`controller`]; the ctl socket and the signal watcher have their own threads.

pub mod apps;
pub mod ax;
pub mod capture;
pub mod controller;
pub mod ctl;
pub mod desktop;
pub mod hotkeys;
pub mod logging;
pub mod native_settings;
pub mod observe;
pub mod paths;
pub mod permissions;
pub mod safety;
pub mod windows;
pub mod wm_thread;
