//! The Corner engine: a hidden window sits in its display's park corner, moved there and back
//! through Accessibility (today's way, and the fallback wherever the Spaces engine cannot
//! run). Where it parks is [`crate::engine::corner`] and [`crate::engine::corner::park`]; bringing
//! everything back is [`crate::platform::safety`] (the registry, else the recovery scan of the park
//! zones).

use crate::platform::ax::{AxElement, AxResult};
use crate::platform::safety::{self, Ours, Report, ScanScope};
use crate::platform::window_engine::{EngineKind, ShowHow, WindowEngine};
use crate::engine::corner;
use crate::engine::geometry::Rect;
use crate::engine::leftovers;
use crate::engine::model::{Display, Model, WindowId};
use crate::engine::recovery::{Known, ParkedFile, ScanDisplays};

/// The Corner engine. It keeps no state of its own: the model's parked flags, the safety
/// registry and the window server's list say everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct CornerEngine;

impl WindowEngine for CornerEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Corner
    }

    /// The window's park frame ([`corner::park_frame`]).
    fn hidden_at(&self, m: &Model, id: WindowId) -> Option<Rect> {
        corner::park_frame(m, id)
    }

    /// A position only: the window keeps its size in the corner (macOS clamps the request,
    /// that is expected).
    fn hide(&self, _id: WindowId, at: Rect, window: &AxElement) -> AxResult<()> {
        window.set_position(at.origin())
    }

    /// Size, position, size ([`AxElement::set_frame`]): out of the corner or moved where it
    /// shows, the same call.
    fn show(&self, id: WindowId, frame: Rect, window: &AxElement, how: ShowHow) -> AxResult<()> {
        match how {
            ShowHow::Once => window.set_frame(frame),
            ShowHow::Retrying => safety::set_frame_retrying(window, id.0, frame, leftovers::RESTORE_RETRY.budget),
        }
    }

    /// In the display's park zone ([`Display::parks`]).
    fn is_hidden(&self, d: &Display, listed: Rect) -> bool {
        d.parks(listed)
    }

    /// In a park zone and not meaningfully on another display ([`recovery::is_stranded`]).
    fn stranded(&self, frame: Rect, displays: &ScanDisplays, known: Known) -> bool {
        EngineKind::Corner.stranded(frame, displays, known)
    }

    /// The park zones as the app had them: its main display's visible frame (the zone sits
    /// above a bottom Dock), every corner it parked in this run, and its other screens'
    /// zones.
    fn assume(&self, file: &ParkedFile) {
        // The park zone sits above a bottom Dock: the app's visible frame, not a guess.
        match file.visible {
            Some(v) => safety::assume_visible_frame(v),
            None => tracing::warn!("guardian: the app left no visible frame; guessing one (a Dock at the bottom may hide parked windows from the scan)"),
        }
        // Windows may sit at any corner the app parked them in this run, on any of its screens.
        safety::assume_corners(&file.corners);
        safety::assume_zones(&file.zones);
    }

    /// The registry's unpark, else the recovery scan ([`safety::emergency_unpark`]).
    fn bring_all_back(&self, why: &str) -> bool {
        safety::emergency_unpark(why)
    }

    /// The registry's unpark, 100 ms for its lock and one try a window
    /// ([`safety::unpark_registry`]).
    fn bring_back_briefly(&self) -> Option<Report> {
        safety::unpark_registry(std::time::Duration::from_millis(100), std::time::Duration::ZERO)
    }

    /// The leftovers check: the windows the window server still lists in a park zone
    /// ([`safety::leftovers_retry`]).
    fn bring_back_rest(&self, why: &str, ours: Ours, moved: bool) -> Option<Ours> {
        safety::leftovers_retry(why, ours, moved)
    }

    /// The scan of the park zones ([`safety::recovery_scan_with`]).
    fn recover(&self, why: &str, scope: &ScanScope) -> (Report, Vec<i32>) {
        safety::recovery_scan_with(why, scope)
    }

    /// None: a parked window is listed on screen, in the corner.
    fn present_off_screen(&self) -> std::collections::HashSet<u32> {
        std::collections::HashSet::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::Observation;
    use crate::engine::rules::{Decision, RuleAction};
    use crate::engine::testkit::{display, meta, model, w};

    #[test]
    fn a_hidden_window_is_left_in_its_displays_park_zone() {
        let mut m = model();
        let obs = Observation::default();
        m.add_window(meta(1, 10, "com.example.a"), &Decision::default(), &obs);
        m.add_window(meta(2, 20, "com.example.b"), &Decision { action: RuleAction::New, ..Decision::default() }, &obs);
        let (e, d) = (CornerEngine, display());
        let at = e.hidden_at(&m, w(1)).expect("managed");
        assert_eq!(Some(at), corner::park_frame(&m, w(1)));
        assert!(e.is_hidden(&d, at), "where it hides is where it counts as hidden");
        assert!(!e.is_hidden(&d, m.window(w(1)).expect("managed").restore), "never at its restore frame");
        assert_eq!(e.hidden_at(&m, w(9)), None);
    }

    /// The arrival's check of a Desktop arranged ahead stays for the Corner engine's moves,
    /// which go through Accessibility; the Spaces engine reads its own back.
    #[test]
    fn only_the_corner_engines_moves_get_the_arrival_check() {
        assert!(!CornerEngine.verifies_own_moves());
        assert!(crate::platform::spaces::engine::shared().verifies_own_moves());
    }
}
