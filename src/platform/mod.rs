//! macOS glue: Accessibility, CGWindowList, hotkeys, displays, safety.
//!
//! Threading: AppKit, NSWorkspace and Carbon hotkeys live on the main thread; every
//! Accessibility call runs on the WM thread ([`wm_thread`]), which owns the engine through
//! [`controller`]; the ctl socket and the signal watcher have their own threads.

pub mod apps;
pub mod ax;
pub mod capture;
pub mod capture_gate;
pub mod code_sign;
pub mod controller;
pub mod ctl;
pub mod desktop;
pub mod desktop_items;
pub mod dock;
pub mod durable;
pub mod guardian;
pub mod hotkeys;
pub mod input;
pub mod instance;
pub mod logging;
pub mod login_item;
pub mod native_settings;
pub mod observe;
pub mod park_probe;
pub mod paths;
pub mod permissions;
pub mod safety;
pub mod self_capture;
pub mod session;
pub mod sliver_tap;
pub mod space_keys;
pub mod update_check;
pub mod updater;
pub mod wallpaper;
pub mod windows;
pub mod wm_thread;
