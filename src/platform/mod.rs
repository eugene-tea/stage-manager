//! macOS glue: Accessibility, CGWindowList, hotkeys, displays, safety.
//!
//! Threading: AppKit, NSWorkspace and Carbon hotkeys live on the main thread; every
//! Accessibility call runs on the WM thread ([`wm_thread`]), which owns the engine through
//! [`controller`]; the ctl socket and the signal watcher have their own threads.

pub mod apple;
pub mod apps;
pub mod ax;
pub mod capture;
pub mod capture_fast;
pub mod capture_frame;
pub mod capture_gate;
pub mod capture_pace;
pub mod capture_pixels;
pub mod clock;
pub mod code_sign;
pub mod conductor;
pub mod controller;
pub mod corner;
pub mod costs;
pub mod ctl;
pub mod desktop;
pub mod desktop_counts;
pub mod desktop_items;
pub mod desktop_shot;
pub mod dock;
pub mod durable;
pub mod enhanced_ui;
#[cfg(test)]
pub mod fake;

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
pub mod hidden_tabs;
pub mod hotkeys;
pub mod input;
pub mod instance;
pub mod launch_scan;
pub mod logging;
pub mod logout_watch;
pub mod login_item;
pub mod minimize_tap;
pub mod mission_key_tap;
pub mod native_settings;
pub mod observe;
pub mod observe_probe;
#[cfg(feature = "probes")]
pub mod park_probe;
pub mod paths;
pub mod pixels;
pub mod permissions;
pub mod safety;
pub mod self_capture;
pub mod session;
#[cfg(feature = "probes")]
pub mod pixels_probe;
#[cfg(feature = "probes")]
pub mod settle_probe;
#[cfg(feature = "probes")]
pub mod shadow_probe;
#[cfg(feature = "probes")]
pub mod shadow_profile;
pub mod space_keys;
pub mod spaces;
#[cfg(feature = "probes")]
pub mod new_window_probe;
#[cfg(feature = "probes")]
pub mod spaces_probe;
pub mod switch_shots;
pub mod switcher_ax;
pub mod switcher_notes;
#[cfg(feature = "probes")]
pub mod switcher_probe;
pub mod tap_switch;
pub mod test_press;
pub mod update_check;
pub mod updater;
pub mod wallpaper;
pub mod window_look;
pub mod window_engine;
pub mod windows;
pub mod wm_thread;
