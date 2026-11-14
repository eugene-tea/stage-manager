//! The Corner engine's upkeep: what the controller keeps on the WM thread, and does around
//! the engine's calls, only because hidden windows sit in a park corner where the window
//! server still lists them and a sliver of each may show
//! ([`crate::platform::corner_engine::CornerEngine`]).
//!
//! [`CornerUpkeep`] is that state, one value the controller owns ([`Controller::corner`]).
//! Nothing here is asked by an engine that hides its windows elsewhere.

use std::collections::BTreeMap;
use std::time::Instant;

use self::patch::PatchTrack;
use self::unrestored::Unrestored;
use crate::engine::geometry::Rect;
use crate::engine::park_reconcile::Stray;

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
