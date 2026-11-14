//! The fallback mid-run (1.9, docs/ARCHITECTURE.md "Window engines"): a running engine whose
//! checks failed ([`WindowEngine::health`](crate::platform::window_engine::WindowEngine::health))
//! brings every window it hid back, and the Corner engine runs for the rest of the run: the
//! process's engine, the controller's, the model's answers and the parked file all change
//! together, then the windows the model counts hidden are hidden again the Corner way. The
//! next launch probes again.

use super::{Controller, spaces::SpacesUpkeep};
use crate::engine::model::{Action, Home, SpaceId};
use crate::engine::recovery::EngineKind;
use crate::platform::observe::Observer;
use crate::platform::{safety, window_engine};

impl Controller {
    /// The tick's look at the running engine's health: a failed engine is left now, unless
    /// a switch's windows still wait behind their pictures (the switch ends first, within its
    /// own time limit) or a quit is under way (its bring-back covers every window).
    pub(super) fn check_engine(&mut self, ob: &Observer) {
        if self.engine.kind() == EngineKind::Corner || safety::quit_requested() || self.phase.switching().is_some() {
            return;
        }
        if let Some(why) = self.engine.health() {
            self.fall_back(ob, &why);
        }
    }

    /// Every window the engine hid brought back (its own bring-back, which ends its Spaces),
    /// then the Corner engine for the rest of the run, and the windows the model counts
    /// hidden on a Desktop shown hidden again the Corner way (those on another Desktop are,
    /// as always, when it shows). Logged with the reason.
    fn fall_back(&mut self, ob: &Observer, why: &str) {
        let from = self.engine.kind();
        tracing::warn!(from = from.name(), "window engine: falling back to the Corner engine for the rest of the run: {why}");
        let back = self.engine.bring_all_back("fallback");
        window_engine::fall_back(why);
        self.engine = window_engine::current();
        self.spaces = SpacesUpkeep::new();
        let Some(m) = self.model.as_mut() else {
            safety::rewrite_parked_file();
            tracing::warn!(back, "window engine: the Corner engine runs now (no stages yet)");
            return;
        };
        m.set_engine(self.engine.kind());
        let shown: Vec<SpaceId> = m.displays().iter().map(|d| m.current_space(d.id)).collect();
        let hide: Vec<Action> =
            m.windows().filter(|w| w.parked && shown.contains(&w.space) && w.home != Home::Pinned).map(|w| Action::Hide { id: w.meta.id }).collect();
        let hidden = hide.len();
        safety::rewrite_parked_file();
        let failed = self.execute(ob, hide);
        self.hidden_changed();
        self.strip_dirty = true;
        tracing::warn!(back, hidden, failed, "window engine: the Corner engine runs now; the hidden windows are in the park corner");
    }
}
