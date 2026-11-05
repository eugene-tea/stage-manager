//! "Follow the Dock" (`[strip] order = "dock"`, [`StripOrder::Dock`]): the in-place places,
//! ordered by the Dock instead of creation order, so the strip's top-to-bottom order is the
//! Dock's left-to-right order.
//!
//! The platform reads the Dock's application items (pinned and running, as the Dock shows
//! them) and hands the model their order ([`Model::set_dock_order`]); the model sorts. A
//! stage sits at the leftmost Dock position among its windows' apps (an app matched by bundle
//! id first, then by pid), leaving out the apps of `[strip] dock_ignore` (the Finder by
//! default, whose windows join every stage) unless no other app of the stage has a Dock icon.
//! Stages at the same position (two stages of one app) and stages whose apps have no Dock
//! icon (an agent app, an app not read yet) keep creation order, the latter after every
//! stage in the Dock. Everything else in place works as with
//! [`StripOrder::InPlace`]: the active stage's modes, the overflow, hotkey numbers following
//! the strip.

use super::super::{Model, Pid, SpaceId, Stage, StageId, WindowId};
use super::StripOrder;

/// One application item of the Dock, in the Dock's order (left to right; top to bottom for a
/// Dock on the side).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DockApp {
    /// The app's bundle id, when its bundle has one.
    pub bundle: Option<String>,
    /// The app's process, when it runs.
    pub pid: Option<Pid>,
}

impl DockApp {
    /// An item for the app with bundle id `bundle`, running as `pid` if it runs.
    pub fn new(bundle: &str, pid: Option<Pid>) -> Self {
        Self { bundle: (!bundle.is_empty()).then(|| bundle.to_string()), pid }
    }
}

impl Model {
    /// The Dock's application items as the platform last read them, in order.
    pub fn dock_order(&self) -> &[DockApp] {
        &self.dock
    }

    /// The Dock's application items, read again by the platform (at launch, when an app
    /// launches or quits, when the user rearranges the Dock). True when the strip's order
    /// changed, on any Space (the caller presents the strip again; its cards glide to their
    /// new places like any other strip update). Kept in every order, so switching to
    /// [`StripOrder::Dock`] sorts at once.
    pub fn set_dock_order(&mut self, dock: Vec<DockApp>) -> bool {
        if self.dock == dock {
            return false;
        }
        let before = self.all_places();
        self.dock = dock;
        self.all_places() != before
    }

    /// Sets the apps left out of a stage's Dock place while another app of the stage has one
    /// ([`Model::dock_place`]; `[strip] dock_ignore`). True when the strip's order changed, on
    /// any Space (the caller presents the strip again, as for a changed Dock).
    pub fn set_dock_ignore(&mut self, ignore: Vec<String>) -> bool {
        if self.options.dock_ignore == ignore {
            return false;
        }
        let before = self.all_places();
        self.options.dock_ignore = ignore;
        self.all_places() != before
    }

    /// Every Space's stages in their places.
    fn all_places(&self) -> Vec<Vec<StageId>> {
        self.spaces.iter().map(|s| self.place_order(s.id).into_iter().map(|st| st.id).collect()).collect()
    }

    /// The stages of `space` in their places, top to bottom (hotkey numbers follow it):
    /// creation order, or with [`StripOrder::Dock`] by Dock position ([`Model::dock_place`]),
    /// creation order among stages at the same position and for those not in the Dock (last).
    pub fn place_order(&self, space: SpaceId) -> Vec<&Stage> {
        let mut stages: Vec<&Stage> = self.stages_in(space).collect();
        if self.options.strip.order == StripOrder::Dock {
            // Stable: ties keep creation order.
            stages.sort_by_cached_key(|s| self.dock_place(s).unwrap_or(usize::MAX));
        }
        stages
    }

    /// A stage's Dock position: the leftmost Dock position among its windows' apps, the apps
    /// of `[strip] dock_ignore` counting only when no other app of the stage has one (a VS
    /// Code stage a Finder window joined sits at VS Code's place; a stage of Finder windows
    /// alone at the Finder's). `None` when none of them has a Dock icon (or the stage has no
    /// window).
    pub(crate) fn dock_place(&self, stage: &Stage) -> Option<usize> {
        let (mut kept, mut ignored) = (None::<usize>, None::<usize>);
        for &w in &stage.windows {
            let Some(at) = self.dock_position(w) else { continue };
            let slot = if self.dock_ignored(w) { &mut ignored } else { &mut kept };
            *slot = Some(slot.map_or(at, |s| s.min(at)));
        }
        kept.or(ignored)
    }

    /// True when a window's app is one of `[strip] dock_ignore` (by bundle id).
    fn dock_ignored(&self, id: WindowId) -> bool {
        self.windows.get(&id).is_some_and(|w| {
            let bundle = w.meta.bundle_id.as_str();
            !bundle.is_empty() && self.options.dock_ignore.iter().any(|b| b == bundle)
        })
    }

    /// The Dock position of a window's app: its bundle id's item, else its pid's.
    fn dock_position(&self, id: WindowId) -> Option<usize> {
        let meta = &self.windows.get(&id)?.meta;
        let by_bundle = if meta.bundle_id.is_empty() {
            None
        } else {
            self.dock.iter().position(|d| d.bundle.as_deref() == Some(meta.bundle_id.as_str()))
        };
        by_bundle.or_else(|| self.dock.iter().position(|d| d.pid == Some(meta.pid)))
    }
}
