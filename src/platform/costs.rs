//! What the app asks of macOS, counted since the launch, for `ctl state`'s `costs`: the calls
//! that cost the most while nothing happens (round idle-subsystems). Read it twice a minute
//! apart with nothing touched, in each mode, and the difference is the cost at rest.
//!
//! - `window_lists`: `CGWindowListCopyWindowInfo` queries ([`super::windows`]).
//! - `launch_services`: `NSRunningApplication` properties asked of LaunchServices (a bundle
//!   URL or a pid, one XPC round trip each, [`super::dock::running_by_path`]).
//! - `dock_reads`: the Dock's list of apps read through Accessibility ([`super::dock::app_order`]).
//! - `captures`: windows a capture round asked a picture of ([`super::capture`]).
//! - `wallpaper_reads`: the desktop picture asked of NSWorkspace ([`super::wallpaper`]).
//! - `lock_polls`: the login session asked whether the screen is locked ([`super::session`]).
//! - `tap_events`: events through our two event taps, the inert slivers' and the minimize's
//!   (each one wakes a thread of ours while every input of the session waits on it).
//! - `mouse_moves`: mouse moves the global monitor handed the strip (each one wakes the main
//!   thread).
//!
//! Process-wide atomics, cheap enough to bump from any thread.

use std::sync::atomic::{AtomicU64, Ordering};

/// Something counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cost {
    /// A window list query.
    WindowList,
    /// One property of a running app asked of LaunchServices.
    LaunchServices,
    /// The Dock's apps read.
    DockRead,
    /// A window's picture asked for.
    Capture,
    /// A desktop picture read.
    WallpaperRead,
    /// The screen lock asked.
    LockPoll,
    /// An event through one of our taps.
    TapEvent,
    /// A mouse move from the global monitor.
    MouseMove,
}

/// Every cost, in the order `ctl state` prints them.
pub const ALL: [Cost; 8] =
    [Cost::WindowList, Cost::LaunchServices, Cost::DockRead, Cost::Capture, Cost::WallpaperRead, Cost::LockPoll, Cost::TapEvent, Cost::MouseMove];

impl Cost {
    /// The key in `ctl state`'s `costs`.
    pub fn key(self) -> &'static str {
        match self {
            Cost::WindowList => "window_lists",
            Cost::LaunchServices => "launch_services",
            Cost::DockRead => "dock_reads",
            Cost::Capture => "captures",
            Cost::WallpaperRead => "wallpaper_reads",
            Cost::LockPoll => "lock_polls",
            Cost::TapEvent => "tap_events",
            Cost::MouseMove => "mouse_moves",
        }
    }

    fn index(self) -> usize {
        ALL.iter().position(|c| *c == self).unwrap_or(0)
    }
}

static COUNTS: [AtomicU64; ALL.len()] = [const { AtomicU64::new(0) }; ALL.len()];

/// Counts `n` of `cost`.
pub fn add(cost: Cost, n: u64) {
    COUNTS[cost.index()].fetch_add(n, Ordering::Relaxed);
}

/// Counts one of `cost`.
pub fn bump(cost: Cost) {
    add(cost, 1);
}

/// The count of `cost` so far.
pub fn get(cost: Cost) -> u64 {
    COUNTS[cost.index()].load(Ordering::Relaxed)
}

/// `ctl state`'s `costs`: every count since the launch, and which of our event taps are
/// switched on now (`taps_on`: they are on only while they have something to judge,
/// [`super::tap_switch`]).
pub fn status() -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for c in ALL {
        out.insert(c.key().to_string(), get(c).into());
    }
    out.insert(
        "taps_on".to_string(),
        serde_json::json!({ "slivers": super::corner::sliver_tap::tap_on(), "minimize": super::minimize_tap::tap_on() }),
    );
    serde_json::Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cost_has_its_own_key_and_counts_up() {
        let keys: std::collections::HashSet<&str> = ALL.iter().map(|c| c.key()).collect();
        assert_eq!(keys.len(), ALL.len());
        let before = get(Cost::DockRead);
        add(Cost::DockRead, 3);
        assert!(get(Cost::DockRead) >= before + 3, "other tests may count too, never less");
        assert!(status().get("dock_reads").and_then(|v| v.as_u64()).is_some());
    }
}
