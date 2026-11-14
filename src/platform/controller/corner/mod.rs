//! The Corner engine's upkeep: what the controller keeps on the WM thread, and does around
//! the engine's calls, only because hidden windows sit in a park corner where the window
//! server still lists them and a sliver of each may show
//! ([`crate::platform::corner_engine::CornerEngine`]).
//!
//! [`CornerUpkeep`] is that state, one value the controller owns ([`Controller::corner`]).
//! Nothing here is asked by an engine that hides its windows elsewhere.

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::{Value, json};

use self::patch::PatchTrack;
use self::unrestored::Unrestored;
use super::Controller;
use crate::engine::geometry::Rect;
use crate::engine::park_reconcile::Stray;
use crate::platform::observe::Observer;
use crate::platform::safety::ParkedEntry;
use crate::platform::{sliver_tap, windows};

pub(super) mod cover;
pub(super) mod patch;
pub(super) mod reconcile;
pub(super) mod shield;
pub(super) mod unrestored;

/// What the Corner engine keeps between its calls, on the WM thread.
pub(super) struct CornerUpkeep {
    /// Each screen's corner patch as planned here ([`PatchTrack`], by the screen's id; round
    /// displays-2).
    pub(super) patches: BTreeMap<u32, PatchTrack>,
    /// Widgets shown on the desktop were logged as possibly under the patch.
    pub(super) patch_widgets_logged: bool,
    /// Until when the corner patch is planned again on the fast tick after a focus or an
    /// activation ([`Controller::replan_patch`]).
    pub(super) patch_fast_until: Option<Instant>,
    /// A window minimized in the Dock on its way to the strip, and where its hide puts it
    /// this once in place of its park frame ([`crate::engine::minimize::dive_frame`];
    /// [`Controller::on_minimized`]).
    pub(super) dive: Option<(u32, Rect)>,
    /// Windows whose restore failed, kept registered until seen back ([`Unrestored`]).
    pub(super) unrestored: BTreeMap<u32, Unrestored>,
    /// When the unrestored windows were last checked.
    pub(super) unrestored_checked: Instant,
    /// Model-parked windows of the shown Desktop the window server lists out of the park
    /// zone, or whose park failed: parked again or adopted every tick
    /// ([`Controller::reconcile_parked`], [`crate::engine::park_reconcile`]).
    pub(super) strays: BTreeMap<u32, Stray>,
}

impl CornerUpkeep {
    /// Nothing planned, nothing kept.
    pub(super) fn new() -> Self {
        Self {
            patches: BTreeMap::new(),
            patch_widgets_logged: false,
            patch_fast_until: None,
            dive: None,
            unrestored: BTreeMap::new(),
            unrestored_checked: Instant::now(),
            strays: BTreeMap::new(),
        }
    }
}

impl Controller {
    /// The tick's last step under the Corner engine: one read of the window server's list a
    /// tick, the patch's, or the tap's own when the patch is off (the inert-slivers tap
    /// judges presses on it, never reading it itself), then the park reconcile on the same
    /// list.
    pub(super) fn corner_tick(&mut self, ob: &mut Observer) {
        let list = self.update_patch().or_else(|| sliver_tap::wants_list().then(windows::on_screen));
        sliver_tap::refresh(list.as_deref());
        self.reconcile_parked(ob, list.as_deref());
    }

    /// The fast tick's share of the corner patch: after a focus or an activation (round
    /// park-reconcile, L2), else through Show Desktop (round show-desktop-patch); one plan a
    /// fast tick at most.
    pub(super) fn corner_fast_tick(&mut self, now: Instant) {
        if !self.patch_fast_step(now) && self.reveal_fast(now) {
            self.update_patch();
        }
    }

    /// `ctl state`'s `strays`: the parked windows the screen contradicts, being parked again
    /// or adopted ([`Self::reconcile_parked`]).
    pub(super) fn strays_status(&self) -> Vec<Value> {
        self.corner.strays.iter().map(|(id, s)| json!({ "id": id, "tries": s.tries, "gave_up": s.gave_up, "user_moved": s.user_moved })).collect()
    }

    /// Keeps window `id` registered (`entry`: its element and the frame it was brought to)
    /// until it is seen back out of the park zone, as a failed restore is
    /// ([`Self::restore_failed`], without its warning: nothing failed).
    pub(super) fn keep_unrestored(&mut self, id: u32, entry: ParkedEntry) {
        self.corner.unrestored.insert(id, Unrestored::failed(entry));
    }
}
