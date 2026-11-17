//! The real desk of the Apple mode ([`Desk`], an [`AppleDesk`]): WindowManager's
//! Accessibility through [`super::strip_ax`], the window server's list (the tick's shared
//! read, [`windows::wake_list`]), the person's input from the hardware's own state, the focus
//! route's calls (raise, main, app frontmost: the controller's own [`ax_focus`]; an app hidden
//! at its launch shown again first), and the posted gestures ([`super::post`], each on a
//! thread of its own). WM thread only.

use std::time::Duration;

use objc2_core_graphics::{CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventType, CGMouseButton};

use crate::engine::apple::desk::{AppleDesk, InputNow, Landed, Posted};
use crate::engine::apple::mirror::{Listed, ListedWindow, Reading};
use crate::engine::apple::screens;
use crate::engine::geometry::Rect;
use crate::engine::model::Display;
use crate::engine::overview::{self, Sighting};
use crate::platform::controller::ax_focus;
use crate::platform::observe::Observer;
use crate::platform::{session, windows};

/// The desk as seen through `ob` (the observer's window elements) and WindowManager `manager`.
pub struct Desk<'a> {
    /// The observer: each managed window's element and app.
    pub ob: &'a Observer,
    /// WindowManager's pid, when it runs.
    pub manager: Option<i32>,
    /// The main screen's frame (Mission Control is told over it).
    pub screen: Option<Rect>,
    /// Every screen, the main one first (each `sm.space` is matched to one).
    pub displays: &'a [Display],
}

impl Desk<'_> {
    /// Every screen's strip as WindowManager's Accessibility shows it (`sm.space` by
    /// `sm.space`, each with its frame when it has one); `None` when it does not answer.
    pub fn strips(&mut self) -> Option<Vec<(Option<Rect>, Reading)>> {
        super::strip_ax::read(self.manager?)
    }
}

impl AppleDesk for Desk<'_> {
    /// Screen `display`'s `sm.space`'s strip ([`screens::reading_for`]).
    fn strip(&mut self, display: &Display) -> Option<Reading> {
        let spaces = self.strips()?;
        screens::reading_for(&spaces, self.displays, display)
    }

    fn listed(&mut self) -> Listed {
        let list = windows::wake_list();
        Listed {
            windows: list
                .iter()
                // A clear window above the windows' layer covers nothing (the gestures' guard
                // passes over it too): counted off screen.
                .map(|w| ListedWindow { id: w.id, pid: w.pid, layer: w.layer, bounds: w.bounds, onscreen: w.onscreen && (w.layer == 0 || w.alpha > 0.0) })
                .collect(),
            manager: self.manager,
        }
    }

    /// The person's input from the hardware's own state (a posted event of ours never
    /// counts): the buttons (any source: a press of ours in flight holds too), ⌘ ⌥ ⌃ and Shift,
    /// how long since the pointer and the keys last moved, the lock, Mission Control.
    fn input(&mut self) -> InputNow {
        let hid = CGEventSourceStateID::HIDSystemState;
        let flags = CGEventSource::flags_state(hid);
        let since = |kinds: &[CGEventType]| -> u64 {
            let secs = kinds.iter().map(|k| CGEventSource::seconds_since_last_event_type(hid, *k)).fold(f64::INFINITY, f64::min);
            Duration::try_from_secs_f64(secs.max(0.0)).map_or(u64::MAX, |d| d.as_millis() as u64)
        };
        let button_down = [CGMouseButton::Left, CGMouseButton::Right, CGMouseButton::Center]
            .into_iter()
            .any(|b| CGEventSource::button_state(CGEventSourceStateID::CombinedSessionState, b));
        let overview = self.screen.is_some_and(|screen| {
            let list = windows::wake_list();
            let sightings = list.iter().map(|w| Sighting { owner: &w.owner, layer: w.layer, bounds: w.bounds, onscreen: w.onscreen });
            overview::overview_in(sightings, screen).overview.is_some()
        });
        InputNow {
            button_down,
            command_keys: flags.intersects(CGEventFlags::MaskCommand | CGEventFlags::MaskAlternate | CGEventFlags::MaskControl),
            shift: flags.contains(CGEventFlags::MaskShift),
            pointer_still_ms: since(&[CGEventType::MouseMoved, CGEventType::LeftMouseDragged, CGEventType::RightMouseDragged, CGEventType::OtherMouseDragged, CGEventType::ScrollWheel]),
            key_idle_ms: since(&[CGEventType::KeyDown, CGEventType::KeyUp, CGEventType::FlagsChanged]),
            locked: session::screen_locked(),
            overview,
        }
    }

    /// Raised, made main, its app frontmost ([`ax_focus`]): Apple's brings its stage in front.
    /// An app hidden (at its launch, by the hide route) is shown again first. A window the
    /// observer does not track (a running app's reopened window, taken in by its pid): its app
    /// activated, found by the window list.
    fn focus(&mut self, window: u32) -> Result<(), String> {
        let Some(w) = self.ob.window(window) else {
            let pid = windows::wake_list().iter().find(|w| w.id == window).map(|w| w.pid).ok_or(format!("window {window} is not observed nor listed"))?;
            super::launch_hide::activate(pid);
            return Ok(());
        };
        if super::launch_hide::is_hidden(w.pid) {
            super::launch_hide::unhide(w.pid);
        }
        ax_focus(&w.element, w.pid).map_err(|e| format!("window {window}: {e}"))
    }

    /// Starts the gesture on a thread of its own ([`super::post::start`]).
    fn post(&mut self, posted: &Posted) -> Result<(), String> {
        super::post::start(posted.clone(), self.manager)
    }

    fn landed(&mut self) -> Option<Landed> {
        super::post::take_landed()
    }

    /// `unhide()` with no activation ([`super::launch_hide::unhide`]): it answers false and
    /// still shows the app (the sixth probe of 11-16).
    fn unhide(&mut self, pid: i32) -> Result<(), String> {
        super::launch_hide::unhide(pid);
        Ok(())
    }
}
