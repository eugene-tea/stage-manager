//! What the Apple mode reads and does on the desk, as one trait ([`AppleDesk`]): the platform
//! implements it over WindowManager's Accessibility, the window server's list and the AX
//! focus (`platform::apple::desk`), the tests over a fake WindowManager
//! (`engine::testkit::apple`). Everything above it (the mirror, following, the plans and
//! routes) runs the same against either.

use serde::Serialize;

use super::mirror::{Listed, Reading};
use crate::engine::geometry::Point;
use crate::engine::model::Display;

/// The user's input as it stands, for the gate a posted step waits on (A3): a posted click
/// never fights the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct InputNow {
    /// A mouse button is down.
    pub button_down: bool,
    /// ⌘, ⌥ or ⌃ is held.
    pub command_keys: bool,
    /// Shift is held.
    pub shift: bool,
    /// How long the pointer has been still, in ms.
    pub pointer_still_ms: u64,
    /// How long since the last key event, in ms.
    pub key_idle_ms: u64,
    /// The screen is locked.
    pub locked: bool,
}

/// A posted gesture (A3's routes: a Shift-click on a thumbnail, a drag onto the strip).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum Posted {
    /// A click at a global point, Shift around it when `shift`.
    Click {
        /// Where (global, top-left).
        at: Point,
        /// A Shift key-down and key-up around it.
        shift: bool,
    },
    /// A drag of the left button from one point to another.
    Drag {
        /// Where it starts (a title bar).
        from: Point,
        /// Where it ends (the strip).
        to: Point,
    },
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
    /// Posts a gesture (A3; refused until its routes are built).
    fn post(&mut self, posted: &Posted) -> Result<(), String>;
}
