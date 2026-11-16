//! The real desk of the Apple mode ([`Desk`], an [`AppleDesk`]): WindowManager's
//! Accessibility through [`super::strip_ax`], the window server's list (the tick's shared
//! read, [`windows::wake_list`]), and the focus route's calls (raise, main, app frontmost:
//! the controller's own [`ax_focus`]). WM thread only. Posted gestures come with A3: refused
//! here until then.

use crate::engine::apple::desk::{AppleDesk, InputNow, Posted};
use crate::engine::apple::mirror::{Listed, ListedWindow, Reading};
use crate::engine::model::Display;
use crate::platform::controller::ax_focus;
use crate::platform::observe::Observer;
use crate::platform::{input, windows};

/// The desk as seen through `ob` (the observer's window elements) and WindowManager `manager`.
pub struct Desk<'a> {
    /// The observer: each managed window's element and app.
    pub ob: &'a Observer,
    /// WindowManager's pid, when it runs.
    pub manager: Option<i32>,
}

impl AppleDesk for Desk<'_> {
    /// The first `sm.space`'s strip (the main screen first; one screen until A6).
    fn strip(&mut self, _display: &Display) -> Option<Reading> {
        super::strip_ax::read(self.manager?)?.into_iter().next()
    }

    fn listed(&mut self) -> Listed {
        let list = windows::wake_list();
        Listed {
            windows: list.iter().map(|w| ListedWindow { id: w.id, pid: w.pid, layer: w.layer, bounds: w.bounds, onscreen: w.onscreen }).collect(),
            manager: self.manager,
        }
    }

    /// The button only, until the gate (A3) needs the rest.
    fn input(&mut self) -> InputNow {
        InputNow { button_down: input::left_button_down(), ..InputNow::default() }
    }

    /// Raised, made main, its app frontmost ([`ax_focus`]): Apple's brings its stage in front.
    fn focus(&mut self, window: u32) -> Result<(), String> {
        let w = self.ob.window(window).ok_or(format!("window {window} is not observed"))?;
        ax_focus(&w.element, w.pid).map_err(|e| format!("window {window}: {e}"))
    }

    fn post(&mut self, _posted: &Posted) -> Result<(), String> {
        Err("posted gestures are not built yet in the Apple mode".into())
    }
}
