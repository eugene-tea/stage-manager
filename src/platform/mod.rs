//! macOS glue: Accessibility, CGWindowList, hotkeys, displays, safety.
//!
//! Threading: AppKit, NSWorkspace and Carbon hotkeys live on the main thread; every
//! Accessibility call runs on the WM thread ([`wm_thread`]), which owns the engine through
//! [`controller`]; the ctl socket and the signal watcher have their own threads.

pub(crate) mod apple;
pub mod apps;
pub(crate) mod ax;
pub mod capture;
pub(crate) mod capture_fast;
pub(crate) mod capture_frame;
pub(crate) mod capture_gate;
pub(crate) mod capture_pace;
pub(crate) mod capture_pixels;
pub(crate) mod clock;
pub(crate) mod code_sign;
pub(crate) mod conductor;
pub mod controller;
pub(crate) mod corner;
pub(crate) mod costs;
pub mod ctl;
pub mod desktop;
pub(crate) mod desktop_counts;
pub(crate) mod desktop_items;
pub mod desktop_shot;
pub(crate) mod dock;
pub(crate) mod durable;
pub mod enhanced_ui;
#[cfg(test)]
pub(crate) mod fake;

/// True on a thread a test gave a fake desk ([`fake`]); always false in a build.
#[cfg(test)]
pub(crate) fn fake_active() -> bool {
    fake::active()
}

/// True on a thread a test gave a fake desk; always false in a build.
#[cfg(not(test))]
#[inline]
pub(crate) fn fake_active() -> bool {
    false
}
pub mod guardian;
pub mod handoff;
pub(crate) mod hidden_tabs;
pub mod hotkeys;
pub mod input;
pub mod instance;
pub(crate) mod launch_scan;
pub mod logging;
pub(crate) mod logout_watch;
pub mod login_item;
pub(crate) mod minimize_tap;
pub(crate) mod mission_key_tap;
pub mod native_settings;
pub(crate) mod observe;
pub(crate) mod observe_probe;
#[cfg(feature = "probes")]
pub mod park_probe;
pub mod paths;
pub mod pixels;
pub mod permissions;
pub mod safety;
pub mod self_capture;
pub mod session;
#[cfg(feature = "probes")]
pub(crate) mod pixels_probe;
#[cfg(feature = "probes")]
pub(crate) mod settle_probe;
#[cfg(feature = "probes")]
pub mod shadow_probe;
#[cfg(feature = "probes")]
pub(crate) mod shadow_profile;
pub mod space_keys;
pub mod spaces;
#[cfg(feature = "probes")]
pub(crate) mod new_window_probe;
#[cfg(feature = "probes")]
pub mod spaces_probe;
pub(crate) mod switch_shots;
pub(crate) mod switcher_ax;
pub(crate) mod switcher_notes;
#[cfg(feature = "probes")]
pub(crate) mod switcher_probe;
pub(crate) mod tap_switch;
pub mod test_press;
pub(crate) mod update_check;
pub mod updater;
pub(crate) mod wallpaper;
pub mod window_look;
pub mod window_engine;
pub mod windows;
pub mod wm_thread;
