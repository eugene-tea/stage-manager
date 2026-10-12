//! macOS glue: Accessibility, CGWindowList, ScreenCaptureKit, hotkeys, safety.
//!
//! Threading: AppKit and NSWorkspace live on the main thread; every Accessibility call runs
//! on the WM thread ([`wm_thread`]); the ctl socket has its own listener thread.

pub mod apps;
pub mod ax;
pub mod ctl;
pub mod geometry;
pub mod logging;
pub mod native_settings;
pub mod observe;
pub mod paths;
pub mod permissions;
pub mod windows;
pub mod wm_thread;
