//! The Spaces engine's upkeep ([`crate::platform::spaces::engine::SpacesEngine`]): what the
//! controller does on the WM thread, around the engine's calls, only because hidden windows
//! sit in window-server Spaces. Little: no window of a stage Space shows on screen, so there
//! is nothing to patch, shield or cover. What is left is the check that the model and the
//! window server agree on which windows are hidden, about once a second.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Controller;
use crate::engine::model::{Home, SpaceId, WindowId};
use crate::platform::input;
use crate::platform::spaces::engine;
use crate::platform::windows;

/// How many times the tick puts a window into its stage's Space again before it says so
/// once and leaves it.
const HIDE_TRIES: u8 = 3;

/// How often what the engine keeps for windows and stages that are gone is dropped.
const TIDY_EVERY: Duration = Duration::from_secs(10);

/// What the Spaces engine's upkeep keeps between ticks, on the WM thread.
pub(super) struct SpacesUpkeep {
    /// Windows the model counts hidden that were found out of their stage's Space, and how
    /// often each was hidden again.
    pub(super) strays: BTreeMap<u32, u8>,
    /// When the engine's map was last tidied.
    tidied: Instant,
}

impl SpacesUpkeep {
    /// Nothing kept.
    pub(super) fn new() -> Self {
        Self { strays: BTreeMap::new(), tidied: Instant::now() }
    }
}

impl Controller {
    /// The tick's last step under the Spaces engine: a show the window server did not carry
    /// out is tried again; a window the model counts hidden on a Desktop shown that is not
    /// in a stage Space is hidden again (a few times, then said once); a window the model
    /// shows that a stage Space still holds is let out. Not while the two may differ for a
    /// reason: a switch's windows waiting behind their pictures, a Space change settling, an
    /// overview, an arrangement made ahead, a drag.
    pub(super) fn spaces_tick(&mut self) {
        let now = Instant::now();
        let spaces = engine::shared();
        let Some(m) = self.model.as_ref() else {
            self.spaces.strays.clear();
            return;
        };
        if self.frozen()
            || self.overview.freezes(now)
            || self.phase.space_settling(now)
            || self.macos_reveal_at.is_some()
            || self.phase.switching().is_some()
            || self.ahead.is_some()
            || self.verify.is_some()
            || self.slide_held()
            || self.drag.is_some()
            || input::left_button_down()
        {
            return;
        }
        spaces.retry_owed();
        let shown: Vec<SpaceId> = m.displays().iter().map(|d| m.current_space(d.id)).collect();
        let held = spaces.held();
        let owed = spaces.owed();
        let hidden: Vec<u32> =
            m.windows().filter(|w| w.parked && shown.contains(&w.space) && w.home != Home::Pinned).map(|w| w.meta.id.0).collect();
        self.spaces.strays.retain(|id, _| hidden.contains(id) && !held.contains(id));
        for id in hidden.iter().copied().filter(|id| !held.contains(id)) {
            let tries = self.spaces.strays.entry(id).or_insert(0);
            if *tries >= HIDE_TRIES {
                continue;
            }
            *tries += 1;
            let ok = spaces.hide_again(m, id);
            if ok {
                tracing::info!(id, "a window the model counts hidden was out of its stage's Space: hidden again");
            } else if *tries == HIDE_TRIES {
                tracing::warn!(id, tries = HIDE_TRIES, "a window the model counts hidden does not go into its stage's Space: left where it is");
            }
        }
        let shown_held: Vec<u32> =
            held.iter().copied().filter(|id| !owed.contains(id) && m.window(WindowId(*id)).is_some_and(|w| !w.parked)).collect();
        if !shown_held.is_empty() {
            spaces.release(&shown_held, "the model shows them");
        }
        if now.duration_since(self.spaces.tidied) >= TIDY_EVERY {
            self.spaces.tidied = now;
            let alive: HashSet<u32> = windows::every_window().iter().map(|w| w.id).collect();
            spaces.tidy(&alive, |stage| m.stages().iter().any(|s| s.id.0 == stage));
        }
    }

    /// `ctl state`'s `strays` under the Spaces engine: the windows being hidden again.
    pub(super) fn spaces_strays_status(&self) -> Vec<Value> {
        self.spaces.strays.iter().map(|(id, tries)| json!({ "id": id, "tries": tries, "gave_up": *tries >= HIDE_TRIES, "user_moved": false })).collect()
    }
}
