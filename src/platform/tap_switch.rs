//! Our event taps on only while they have something to judge (round idle-subsystems).
//!
//! An active tap at the head of the session ([`super::corner::sliver_tap`],
//! [`super::minimize_tap`]) wakes a thread of ours for every event of its kinds, the whole
//! session's input waiting on it, whether or not anything is there to catch. Until 1.9 each
//! stayed on from its first start to the quit, so in the Apple mode, while standing down and
//! with no parked window, every press, drag (and for the minimize tap every key) went through
//! a callback that passed it. Now each tap says what it wants ([`TapSwitch::set_wanted`]) and
//! is switched on and off at the change ([`TapSwitch::apply`]); its thread and its run loop
//! stay, so switching it on again costs one call. A press the tap swallowed keeps it on until
//! its release (`busy`): the release is swallowed with it, as before.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2_core_foundation::CFMachPort;
use objc2_core_graphics::CGEvent;

/// What to do with a tap: `Some(on)` to switch it, `None` to leave it. `enabled`: as it is
/// now; `wanted`: whether it has something to judge; `busy`: a press it swallowed is still
/// down (its drags and release are swallowed too, so it stays on until then).
pub fn step(enabled: bool, wanted: bool, busy: bool) -> Option<bool> {
    match (enabled, wanted) {
        (false, true) => Some(true),
        (true, false) if !busy => Some(false),
        _ => None,
    }
}

/// One tap's switch: whether it is wanted, and whether it is on.
pub struct TapSwitch {
    wanted: AtomicBool,
    enabled: AtomicBool,
    /// Serializes switching (the WM thread and the tap's callback).
    lock: Mutex<()>,
}

impl TapSwitch {
    /// Not wanted, not on.
    pub const fn new() -> Self {
        Self { wanted: AtomicBool::new(false), enabled: AtomicBool::new(false), lock: Mutex::new(()) }
    }

    /// Says whether the tap has something to judge now.
    pub fn set_wanted(&self, on: bool) {
        self.wanted.store(on, Ordering::SeqCst);
    }

    /// Whether the tap has something to judge.
    pub fn wanted(&self) -> bool {
        self.wanted.load(Ordering::SeqCst)
    }

    /// Whether the tap is switched on (as we last switched it).
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Switches the tap at `port` on or off as wanted ([`step`]). `wait`: false from the
    /// tap's callback, which never waits on the lock (then the next call switches it).
    pub fn apply(&self, port: Option<&CFMachPort>, busy: bool, wait: bool) {
        let Some(port) = port else { return };
        let _guard = if wait {
            match self.lock.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            }
        } else {
            match self.lock.try_lock() {
                Ok(g) => g,
                Err(_) => return,
            }
        };
        if let Some(on) = step(self.enabled(), self.wanted(), busy) {
            CGEvent::tap_enable(port, on);
            self.enabled.store(on, Ordering::SeqCst);
            tracing::debug!(on, "event tap {}", if on { "switched on: it has something to judge" } else { "switched off: nothing to judge" });
        }
    }

    /// The tap at `port` was just made (macOS makes it on): on only if wanted.
    pub fn made(&self, port: &CFMachPort) {
        let _guard = match self.lock.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let on = self.wanted();
        CGEvent::tap_enable(port, on);
        self.enabled.store(on, Ordering::SeqCst);
    }

    /// macOS switched the tap off for a timeout: on again if it is to be on (from the
    /// callback).
    pub fn on_again(&self, port: &CFMachPort) -> bool {
        if !self.enabled() {
            return false;
        }
        CGEvent::tap_enable(port, true);
        true
    }
}

impl Default for TapSwitch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tap_is_on_only_while_it_has_something_to_judge() {
        assert_eq!(step(false, true, false), Some(true), "wanted: on");
        assert_eq!(step(true, false, false), Some(false), "nothing to judge: off");
        assert_eq!(step(true, true, false), None);
        assert_eq!(step(false, false, false), None);
    }

    #[test]
    fn a_swallowed_press_keeps_its_tap_on_until_the_release() {
        assert_eq!(step(true, false, true), None, "its drags and release are swallowed too");
        assert_eq!(step(true, false, false), Some(false), "released: off");
        assert_eq!(step(false, true, true), Some(true));
    }

    /// The inert slivers' tap across mode changes: on while the Corner engine parks windows,
    /// off when Apple's Stage Manager comes on (augmenting or standing down) once a press
    /// it swallowed is released, on again when Apple's goes off and windows are parked.
    #[test]
    fn a_tap_follows_the_mode_on_and_off() {
        let mut on = false;
        let mut run = |wanted: bool, busy: bool| {
            if let Some(next) = step(on, wanted, busy) {
                on = next;
            }
            on
        };
        assert!(run(true, false), "Corner, windows parked");
        assert!(run(false, true), "Apple's on while a sliver press is held: on until its release");
        assert!(!run(false, false), "released: off for the Apple mode");
        assert!(!run(false, false), "stays off at every tick of the Apple mode");
        assert!(run(true, false), "Apple's off, the stages back and parked: on again");
        assert!(!run(false, false), "one stage left, nothing parked: off");
    }

    #[test]
    fn the_switch_keeps_what_is_wanted() {
        let s = TapSwitch::new();
        assert!(!s.wanted() && !s.enabled());
        s.set_wanted(true);
        assert!(s.wanted());
        // No tap made yet: nothing to switch.
        s.apply(None, false, true);
        assert!(!s.enabled());
    }
}
