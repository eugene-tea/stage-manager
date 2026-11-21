//! The Spaces engine's upkeep ([`crate::platform::spaces::engine::SpacesEngine`]): what the
//! controller does on the WM thread, around the engine's calls, only because hidden windows
//! sit in window-server Spaces. Little: no window of a stage Space shows on screen, so there
//! is nothing to patch, shield or cover. What is left is the check that the model and the
//! window server agree on which windows are hidden, on each tick (every 250 ms while
//! something happens, every 2 s at rest).

use crate::platform::clock;
use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Controller;
use crate::engine::busy_apps::Backoff;
use crate::engine::geometry::Rect;
use crate::engine::model::{Home, SpaceId, StageId, WindowId};
use crate::platform::input;
use crate::platform::safety::ParkedEntry;
use crate::platform::spaces::engine;
use crate::platform::windows;

/// How many times the tick puts a window into its stage's Space again before it says once
/// that the window does not go; the tick stays brisk for it until then, and goes on trying
/// at the window's slowing pace ([`Stray`]).
pub(super) const HIDE_TRIES: u8 = 3;

/// A window the model counts hidden found out of its stage's Space: how often it was hidden
/// again, the pace of the next try ([`Backoff`]: 1 s doubling to 30 s), and the stage and
/// Space it was counted in; a change of either starts afresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stray {
    pub(super) tries: u8,
    backoff: Option<Backoff>,
    place: (Option<StageId>, SpaceId),
}

impl Stray {
    /// Whether window `id`, counted in `place` at `now`, is hidden again now: at once when
    /// first found or found counted elsewhere, else at its pace. The try is counted.
    pub(super) fn due(strays: &mut BTreeMap<u32, Stray>, id: u32, place: (Option<StageId>, SpaceId), now: Instant) -> bool {
        let fresh = Stray { tries: 0, backoff: None, place };
        let s = strays.entry(id).or_insert(fresh);
        if s.place != place {
            *s = fresh;
        }
        if s.backoff.is_some_and(|b| now < b.next) {
            return false;
        }
        s.tries = s.tries.saturating_add(1);
        true
    }

    /// The try at `now` did not take: the next waits 1 s, then twice the last wait, up to 30 s.
    pub(super) fn missed(&mut self, now: Instant) {
        self.backoff = Some(self.backoff.map_or_else(|| Backoff::new(now), |b| b.doubled(now)));
    }
}

/// How often what the engine keeps for windows and stages that are gone is dropped, and a
/// window in a stage Space that the engine does not hold is let out.
pub(super) const TIDY_EVERY: Duration = Duration::from_secs(10);

/// What the Spaces engine's upkeep keeps between ticks, on the WM thread.
pub(super) struct SpacesUpkeep {
    /// Windows the model counts hidden that were found out of their stage's Space, and how
    /// often each was hidden again.
    pub(super) strays: BTreeMap<u32, Stray>,
    /// When the engine's map was last tidied.
    tidied: Instant,
    /// A window minimized in the Dock on its way to the strip, and where its hide puts it
    /// this once ([`crate::engine::minimize::dive_frame`]): macOS shows a window it
    /// un-minimizes on the Desktop, out of any stage Space, so it is put beyond the Dock's
    /// edge while minimized, comes up off the screen, and is hidden again once it is down.
    pub(super) dive: Option<(u32, Rect)>,
    /// Windows a failed show left out of their Space away from its frame, with their
    /// registry entries: kept registered while the engine owes the frame
    /// ([`Controller::show_failed`]).
    pub(super) frames_owed: BTreeMap<u32, ParkedEntry>,
    /// After a fallback from this engine: what it left in its stage Spaces is swept again
    /// until nothing is left ([`Controller::check_engine`]).
    pub(super) left_behind: bool,
    /// Mission Control's lends: the hidden stages' windows on their Desktop while it shows
    /// ([`super::mission_control`]).
    pub(super) mission: super::mission_control::MissionWatch,
}

impl SpacesUpkeep {
    /// Nothing kept.
    pub(super) fn new() -> Self {
        Self { strays: BTreeMap::new(), tidied: clock::now(), dive: None, frames_owed: BTreeMap::new(), left_behind: false, mission: super::mission_control::MissionWatch::default() }
    }
}

impl Controller {
    /// The tick's last step under the Spaces engine: a show the window server did not carry
    /// out is tried again; a window the model counts hidden on a Desktop shown that is not
    /// in its stage Space (by the window server's word) is hidden again (a few times, said
    /// once, then ever more slowly: [`Stray`]); a window the model
    /// shows that a stage Space still holds is let out; every 10 s the engine's map is tidied
    /// ([`TIDY_EVERY`]), and windows nothing answers for are let out (at once too for a window
    /// found in a stage Space: [`Self::spaces_found`]). Not while the two may differ for a
    /// reason: a switch's windows waiting behind their pictures, a Space change settling, an
    /// overview, an arrangement made ahead, a drag.
    pub(super) fn spaces_tick(&mut self) {
        let now = clock::now();
        let spaces = engine::shared();
        // A screen gone: windows the window server let out of their stage Spaces go back in,
        // on their own Desktops, whatever else holds the tick back (a slide there comes later);
        // a Desktop that ended joins its stages where the model's windows of it show.
        {
            let m = self.model.as_ref();
            spaces.keep_tick(&|id| m.map(|m| m.shown_with(WindowId(id)).into_iter().map(|w| w.0).collect()).unwrap_or_default());
        }
        // The watch over, the model no longer keeps the gone screen's hidden windows.
        let watching = spaces.watching_now();
        if let Some(m) = self.model.as_mut() {
            m.set_display_watch(watching);
        }
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
        let owed = spaces.retry_owed(false, &mut self.busy_apps.borrow_mut());
        self.spaces.frames_owed.retain(|id, _| owed.contains(id));
        let shown: Vec<SpaceId> = m.displays().iter().map(|d| m.current_space(d.id)).collect();
        // What the engine holds checked against the window server first: a window taken out
        // of its stage Space by anything but the engine is hidden again below.
        let _ = spaces.forget_escaped();
        let held = spaces.held();
        let hidden: Vec<u32> =
            m.windows().filter(|w| w.parked && shown.contains(&w.space) && w.home != Home::Pinned).map(|w| w.meta.id.0).collect();
        self.spaces.strays.retain(|id, _| hidden.contains(id) && !held.contains(id));
        for id in hidden.iter().copied().filter(|id| !held.contains(id)) {
            let place = (m.stage_of(WindowId(id)), m.window(WindowId(id)).map_or(SpaceId(0), |w| w.space));
            if !Stray::due(&mut self.spaces.strays, id, place, now) {
                continue;
            }
            let ok = spaces.hide_again(m, id);
            let Some(s) = self.spaces.strays.get_mut(&id) else { continue };
            if ok {
                tracing::info!(id, "a window the model counts hidden was out of its stage's Space: hidden again");
            } else {
                s.missed(now);
                if s.tries == HIDE_TRIES {
                    tracing::warn!(id, tries = HIDE_TRIES, "a window the model counts hidden does not go into its stage's Space: tried again ever more slowly");
                }
            }
        }
        let shown_held: Vec<u32> =
            held.iter().copied().filter(|id| !owed.contains(id) && m.window(WindowId(*id)).is_some_and(|w| !w.parked)).collect();
        if !shown_held.is_empty() {
            spaces.release(&shown_held, "the model shows them");
        }
        if now.duration_since(self.spaces.tidied) >= TIDY_EVERY {
            self.spaces.tidied = now;
            // The tick's read of every window, shared with its other duties (read afresh
            // for the strangers only when the tidy ended a Space).
            let alive: HashSet<u32> = windows::wake_list().iter().map(|w| w.id).collect();
            spaces.tidy(&alive, |stage| m.stages().iter().any(|s| s.id.0 == stage));
            spaces.let_strangers_out(&windows::wake_list(), |id| m.window(WindowId(id)).is_some());
        }
    }

    /// A window of an observed app found (`WinEvent::Found`) that the model does not know:
    /// when the window server lists it in a stage Space the engine does not hold it in (its
    /// app ordered it out while hidden and in again: it comes back in the Space it was in),
    /// that app's windows nothing answers for are let out now
    /// ([`engine::SpacesEngine::let_strangers_out`]), not at the next tidy ([`TIDY_EVERY`],
    /// still the backstop): before, the window stayed invisible up to 10 s.
    pub(super) fn spaces_found(&self, pid: i32, id: u32) {
        let spaces = engine::shared();
        if !spaces.stranger_in_stage(id) {
            return;
        }
        let list: Vec<windows::CgWindow> = windows::wake_list().iter().filter(|w| w.pid == pid).cloned().collect();
        let m = self.model.as_ref();
        let out = spaces.let_strangers_out(&list, |w| m.is_some_and(|m| m.window(WindowId(w)).is_some()));
        tracing::info!(id, pid, out, "spaces engine: a window found in a stage's Space that nothing holds: its app's strangers let out now");
    }

    /// `ctl state`'s `strays` under the Spaces engine: the windows being hidden again.
    pub(super) fn spaces_strays_status(&self) -> Vec<Value> {
        self.spaces.strays.iter().map(|(id, s)| json!({ "id": id, "tries": s.tries, "gave_up": false, "user_moved": false })).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window that will not go into its stage's Space (before, left visible after three
    /// tries): tried at once, then 1 s, 2 s, 4 s ... up to 30 s apart, never given up; counted
    /// in another stage or Space, it starts afresh.
    #[test]
    fn a_stray_is_hidden_again_ever_more_slowly_and_never_given_up() {
        let t0 = Instant::now();
        let place = (Some(StageId(1)), SpaceId(7));
        let mut strays = BTreeMap::new();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut tried = Vec::new();
        for ms in (0..120_000).step_by(250) {
            if Stray::due(&mut strays, 5, place, at(ms)) {
                tried.push(ms);
                strays.get_mut(&5).expect("kept").missed(at(ms));
            }
        }
        assert_eq!(&tried[..7], [0, 1000, 3000, 7000, 15000, 31000, 61000], "1 s doubling");
        assert_eq!(tried[7], 91000, "capped at 30 s");
        assert!(strays[&5].tries > HIDE_TRIES, "never given up");
        assert!(!Stray::due(&mut strays, 5, place, at(91_250)), "at its pace");
        assert!(Stray::due(&mut strays, 5, (Some(StageId(2)), SpaceId(7)), at(91_250)), "another stage: at once");
        assert_eq!(strays[&5].tries, 1, "counted afresh");
        strays.get_mut(&5).expect("kept").missed(at(91_250));
        assert!(Stray::due(&mut strays, 5, (Some(StageId(2)), SpaceId(8)), at(91_500)), "another Space: at once");
    }
}
