//! The Apple mode's window engine: the null-hide engine. While Apple's Stage Manager is on
//! and `[windows] with_apple = "augment"`, Apple's arranges the windows and this app hides
//! none, so every [`WindowEngine`] call is answered honestly with "nothing of ours is hidden"
//! and no trait method changes (docs/STRATEGIES.md, "Apple-augmented"). No controller of ours
//! runs meanwhile ([`super::controller::AppleController`] does), so `hide` and `show` are
//! never asked.
//!
//! The safety paths still find what a release before it left: the windows its unpark could
//! not bring back are held in the safety registry ([`safety::emergency_unpark`] at a quit or
//! a panic), and a crashed copy's registered windows are scanned for as the Spaces engine
//! scans ([`WindowEngine::recover`]).

use std::collections::HashSet;

use crate::engine::apple;
use crate::engine::geometry::Rect;
use crate::engine::model::{Display, Model, WindowId};
use crate::engine::recovery::{Known, ParkedFile, ScanDisplays};
use crate::platform::ax::{AxElement, AxResult};
use crate::platform::safety::{self, Ours, Report, ScanScope};
use crate::platform::window_engine::{EngineKind, Motion, ShowHow, WindowEngine};
use crate::ui::strip_model::Transition;

/// The null-hide engine. It keeps no state: Apple's strip holds the windows.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppleEngine;

impl WindowEngine for AppleEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Apple
    }

    /// The window's last frame: this engine moves nothing.
    fn hidden_at(&self, m: &Model, id: WindowId) -> Option<Rect> {
        m.window(id).map(|w| w.meta.frame)
    }

    /// Nothing: never asked (no controller of ours runs in this mode).
    fn hide(&self, _id: WindowId, _at: Rect, _window: &AxElement) -> AxResult<()> {
        Ok(())
    }

    /// Nothing: never asked.
    fn show(&self, _id: WindowId, _frame: Rect, _window: &AxElement, _how: ShowHow) -> AxResult<()> {
        Ok(())
    }

    /// Listed at thumbnail size in Apple's strip column, or off the left edge (the strip
    /// hidden): [`apple::is_hidden`].
    fn is_hidden(&self, d: &Display, listed: Rect) -> bool {
        apple::is_hidden(d, listed)
    }

    /// By the frame: a window in Apple's strip is listed at its thumbnail.
    fn hides_now(&self, _id: WindowId, d: &Display, listed: Rect) -> bool {
        apple::is_hidden(d, listed)
    }

    /// Only a window a release before this mode left registered in a park corner
    /// ([`EngineKind::stranded`]).
    fn stranded(&self, frame: Rect, displays: &ScanDisplays, known: Known) -> bool {
        EngineKind::Apple.stranded(frame, displays, known)
    }

    /// The park zones as the app had them, as the Corner engine takes them: a window held
    /// from the release before this mode waits in one.
    fn assume(&self, file: &ParkedFile) {
        if let Some(v) = file.visible {
            safety::assume_visible_frame(v);
        }
        safety::assume_corners(&file.corners);
        safety::assume_zones(&file.zones);
    }

    /// Pictures: never asked (no switch of ours plays).
    fn animate(&self, _t: &Transition) -> Motion {
        Motion::Pictures
    }

    /// True: no picture is kept fresh on a timer.
    fn pictures_at_switch(&self) -> bool {
        true
    }

    /// Nothing of ours is away but the registry's held windows (a release's), brought back
    /// as the Corner engine does; true with none.
    fn bring_all_back(&self, why: &str) -> bool {
        if safety::registry_len() == 0 {
            return true;
        }
        safety::emergency_unpark(why)
    }

    /// An empty report, or the registry's short unpark when it holds a window.
    fn bring_back_briefly(&self) -> Option<Report> {
        if safety::registry_len() == 0 {
            return Some(Report::default());
        }
        safety::unpark_registry(std::time::Duration::from_millis(100), std::time::Duration::ZERO)
    }

    /// None: nothing of this mode's is hidden. A release's leftovers are looked for by the
    /// engine that parked them (the WM thread asks the launch's engine).
    fn bring_back_rest(&self, _why: &str, _ours: Ours, _moved: bool) -> Option<Ours> {
        None
    }

    /// The Corner engine's scan only when an earlier copy left windows registered, at a
    /// launch or in the guardian, as the Spaces engine does.
    fn recover(&self, why: &str, scope: &ScanScope) -> (Report, Vec<i32>) {
        if scope.registered.is_empty() && !why.starts_with("launch") && !why.starts_with("guardian") {
            return (Report::default(), Vec::new());
        }
        safety::recovery_scan_with(why, scope)
    }

    /// None: a strip window is listed on screen at its thumbnail, or off the left edge while
    /// the strip hides and on its Desktop all the same; the mirror reads those itself.
    fn present_off_screen(&self) -> HashSet<u32> {
        HashSet::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testkit::display;

    #[test]
    fn the_apple_engine_hides_nothing_and_knows_a_thumbnail() {
        let e = AppleEngine;
        assert_eq!(e.kind(), EngineKind::Apple);
        assert_eq!(e.kind().name(), "apple");
        let d = display();
        assert!(e.is_hidden(&d, Rect::new(15.0, 300.0, 79.0, 104.0)));
        assert!(!e.is_hidden(&d, Rect::new(300.0, 100.0, 800.0, 600.0)));
        assert!(e.present_off_screen().is_empty());
        assert!(e.health().is_none());
        assert!(!e.lands_live());
    }
}
