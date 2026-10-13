//! The model's side of `[windows] hide_parked_apps` ([`crate::engine::hide`]): what an app's
//! windows are doing, for the decision to hide it.

use super::{Home, Model, Pid, WindowId, WindowRole};
use crate::engine::geometry::Rect;
use crate::engine::hide::AppPark;
use crate::engine::park::park_frame_asked;

impl Model {
    /// App `pid`'s windows as [`crate::engine::hide::may_hide`] needs them: how many the
    /// model manages (every Desktop), how many show, the stage an activation would bring on
    /// the shown Desktop (its most recent inactive stage there with a window of the app) with
    /// the app's windows in it and their restore frames, and whether the shown Desktop's
    /// active stage has a standard window.
    pub fn app_park(&self, pid: Pid) -> AppPark {
        let space = self.current_space(self.displays[0].id);
        let active = self.active.get(&space).copied();
        let mine = || self.windows.values().filter(move |w| w.meta.pid == pid);
        let windows = mine().count();
        let shown = mine().filter(|w| !w.parked || w.home == Home::Pinned || w.sticky).count();
        let keep_stage = self
            .stages
            .iter()
            .filter(|s| s.space == space && !self.is_active(s.id))
            .filter(|s| mine().any(|w| w.home == Home::Stage(s.id)))
            .max_by_key(|s| s.last_active)
            .map(|s| s.id);
        let keep = keep_stage
            .map(|s| mine().filter(|w| w.home == Home::Stage(s) && w.parked).map(|w| (w.meta.id.0, w.restore)).collect())
            .unwrap_or_default();
        let stage_here = active.is_some_and(|a| {
            self.windows.values().any(|w| w.home == Home::Stage(a) && w.meta.role == WindowRole::Standard && !w.parked)
        });
        AppPark { windows, shown, keep, stage_here }
    }

    /// Where window `id` parks (its display's corner, the park request), for a window kept at
    /// its own frame while its app was hidden that must go to the corner after all.
    pub fn park_frame_of(&self, id: WindowId) -> Option<Rect> {
        let w = self.windows.get(&id)?;
        let d = self.display(w.meta.display);
        Some(park_frame_asked(w.restore.size(), d.frame, d.corner, self.options.park))
    }
}
