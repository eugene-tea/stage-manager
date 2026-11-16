//! What the Apple mode reads and does on the desk, as one trait ([`AppleDesk`]): the platform
//! implements it over WindowManager's Accessibility, the window server's list and the AX
//! focus (`platform::apple::desk`), the tests over a fake WindowManager
//! (`engine::testkit::apple`). Everything above it (the mirror, following, the plans and
//! routes) runs the same against either.

use serde::Serialize;

use super::mirror::{Listed, Reading};
use crate::engine::geometry::{Point, Rect};
use crate::engine::model::Display;

/// The user's input as it stands, for the gate a posted step waits on
/// ([`super::gate`]): a posted click never fights the user. The keys and the pointer are the
/// person's own (the hardware's state), never a posted event's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct InputNow {
    /// A mouse button is down.
    pub button_down: bool,
    /// ⌘, ⌥ or ⌃ is held.
    pub command_keys: bool,
    /// Shift is held (by the person: a posted Shift-click then uses it as it is).
    pub shift: bool,
    /// How long the pointer has been still, in ms.
    pub pointer_still_ms: u64,
    /// How long since the last key event, in ms.
    pub key_idle_ms: u64,
    /// The screen is locked.
    pub locked: bool,
    /// Mission Control, App Exposé or Show Desktop shows.
    pub overview: bool,
}

impl InputNow {
    /// The person idle: nothing held, the pointer and the keys still for a second.
    pub fn idle() -> Self {
        Self { pointer_still_ms: 1000, key_idle_ms: 1000, ..Self::default() }
    }
}

/// One Shift-click of a press: window `window`'s thumbnail at global point `at`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Press {
    /// The window whose thumbnail is clicked (the guard: the topmost window there is
    /// WindowManager's).
    pub window: u32,
    /// Where (global, top-left).
    pub at: Point,
}

/// A posted gesture (the routes `shift_click` and `drag`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Posted {
    /// Shift-clicks on thumbnails within one Shift press (a probe of 11-16: a stage of two,
    /// both thumbnails Shift-clicked 50 ms apart from one settled read, both joined): a real
    /// Shift key-down about 12 ms before the first (none when the person holds Shift:
    /// `own_shift` false), each click with no hold and the pointer warped back at once, the
    /// next [`CLICK_GAP`] later, Shift up about 12 ms after the last.
    Clicks {
        /// The clicks, in order (front to back within an item, the items top to bottom).
        presses: Vec<Press>,
        /// A Shift key-down and key-up of ours around them.
        own_shift: bool,
    },
    /// A drag of window `window`'s title bar onto the strip.
    Drag {
        /// The window dragged (the guard: the topmost window at `from` is it).
        window: u32,
        /// Where it starts (its title bar).
        from: Point,
        /// Where it ends (the strip's column).
        to: Point,
        /// Its frame before, set back through Accessibility when the person's own input ends
        /// the drag.
        frame: Rect,
    },
}

/// Between two Shift-clicks of one press (the probe of 11-16 that brought a stage of two in
/// with one Shift press, 50 ms apart). Short enough that every click lands before Apple lays
/// out the strip item again around the thumbnails left: live 11-16 15:20:35, a stage of three
/// clicked 50 ms apart, the third click (100 ms in) missed its thumbnail, already moved.
pub const CLICK_GAP: std::time::Duration = std::time::Duration::from_millis(15);

impl Posted {
    /// One Shift-click on window `window`'s thumbnail at `at`.
    pub fn click(window: u32, at: Point, own_shift: bool) -> Self {
        Self::Clicks { presses: vec![Press { window, at }], own_shift }
    }

    /// The window the gesture is for (a press's first).
    pub fn window(&self) -> u32 {
        match self {
            Self::Clicks { presses, .. } => presses.first().map_or(0, |p| p.window),
            Self::Drag { window, .. } => *window,
        }
    }
}

/// How a posted gesture ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Landed {
    /// Every event posted; Apple's answer is for the mirror to see.
    Posted,
    /// Nothing posted (the guard: the point is not where it should be, the button down).
    Refused(String),
    /// The person's own input ended it midway (a drag): nothing more posted, the window's
    /// frame set back.
    Interrupted(String),
}

/// The desk the Apple mode reads and acts on.
pub trait AppleDesk {
    /// Screen `display`'s strip as WindowManager's Accessibility shows it now; `None` when it
    /// cannot be read (WindowManager not running, no answer).
    fn strip(&mut self, display: &Display) -> Option<Reading>;
    /// The window server's list now.
    fn listed(&mut self) -> Listed;
    /// The user's input as it stands.
    fn input(&mut self) -> InputNow;
    /// Window `window` raised, made main and its app frontmost (the focus route): Apple's
    /// switches to its stage.
    fn focus(&mut self, window: u32) -> Result<(), String>;
    /// Starts a gesture: `Ok` once it is under way (the real desk posts it on a thread of
    /// its own; how it ended comes from [`AppleDesk::landed`]), `Err` when it cannot start.
    fn post(&mut self, posted: &Posted) -> Result<(), String>;
    /// How the last gesture ended, once it has (taken: asked again, `None`).
    fn landed(&mut self) -> Option<Landed>;
}
