//! The Corner engine: a hidden window sits in its display's park corner, moved there and back
//! through Accessibility (today's way, and the fallback wherever the Spaces engine cannot
//! run). Where it parks is [`crate::engine::corner`] and [`crate::engine::park`]; bringing
//! everything back is [`super::safety`] (the registry, else the recovery scan of the park
//! zones).

use super::ax::{AxElement, AxResult};
use super::safety::{self, Ours, Report, ScanScope};
use super::window_engine::{EngineKind, ShowHow, WindowEngine};
use crate::engine::corner;
use crate::engine::geometry::Rect;
use crate::engine::leftovers;
use crate::engine::model::{Display, Model, WindowId};

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
    /// ([`safety::leftovers_back_knowing`], [`safety::leftovers_retry`]).
    fn bring_back_rest(&self, why: &str, ours: Ours, moved: bool) -> Option<Ours> {
        safety::leftovers_retry(why, ours, moved)
    }

    /// The scan of the park zones ([`safety::recovery_scan_with`]).
    fn recover(&self, why: &str, scope: &ScanScope) -> (Report, Vec<i32>) {
        safety::recovery_scan_with(why, scope)
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
}
