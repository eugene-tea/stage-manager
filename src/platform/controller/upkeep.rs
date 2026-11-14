//! The controller's one door to the upkeep of the engine that runs: what is kept, checked
//! and drawn on the WM thread around the engine's calls, at moments every engine shares (a
//! batch ended, a tick, a press, a write that failed, a switch about to play) but with work
//! only one engine has (docs/ARCHITECTURE.md, "Window engines").
//!
//! The window calls themselves go through the trait
//! ([`WindowEngine`](crate::platform::window_engine::WindowEngine)); the upkeep cannot: it
//! reads and changes the controller's own state (the model, the phase, the observer, the
//! echoes), so it stays methods of the controller, and each moment here is one `match` on
//! the engine that runs ([`WindowEngine::kind`](crate::platform::window_engine::WindowEngine::kind)),
//! as the model's door is (`engine/model/hiding.rs`). No other file of the controller asks
//! which engine runs, and none but the Corner engine's own ([`super::corner`]) touches its
//! state ([`CornerUpkeep`](super::corner::CornerUpkeep)): an engine that hides its windows
//! elsewhere answers each moment here with its own work, or with none.

use std::collections::{BTreeSet, HashSet};
use std::time::Instant;

use serde_json::{Value, json};

use super::Controller;
use super::corner::unrestored::LetGo;
use crate::engine::corner::corner_patch::{Refused, SwitchCover};
use crate::engine::geometry::{Point, Rect};
use crate::platform::observe::Observer;
use crate::platform::safety::ParkedEntry;
use crate::platform::window_engine::EngineKind;
use crate::platform::windows;

impl Controller {
    // ── What is hidden, for whoever watches it ──────────────────────────────────────────

    /// The hidden windows, where they wait, or what a press there may reach changed: after
    /// every batch, when an overview opens or closes, at a display or config change, when
    /// the Dock's bar moved. The Corner engine: the inert-slivers tap's shield
    /// ([`Self::publish_shield`]).
    pub(super) fn hidden_changed(&self) {
        match self.engine.kind() {
            EngineKind::Corner => self.publish_shield(),
            EngineKind::Spaces => {},
        }
    }

    /// The engine's look at the screen: after every batch, at a press or a release, when a
    /// wallpaper click's hold is over. The window server's on-screen list when it read one,
    /// for the caller's own use. The Corner engine: each screen's corner patch planned
    /// ([`Self::update_patch`]).
    pub(super) fn look_at_screen(&mut self) -> Option<Vec<windows::CgWindow>> {
        match self.engine.kind() {
            EngineKind::Corner => self.update_patch(),
            EngineKind::Spaces => None,
        }
    }

    /// A focus or an activation: macOS raises the app's windows, the hidden ones too. The
    /// Corner engine: the patch planned again now and on the fast tick for a moment
    /// ([`Self::replan_patch`]).
    pub(super) fn raised_by_macos(&mut self, now: Instant) {
        match self.engine.kind() {
            EngineKind::Corner => self.replan_patch(now),
            EngineKind::Spaces => {},
        }
    }

    /// A mouse-up at `at`. The Corner engine: a new still of the desktop in the corner
    /// shortly ([`Self::corner_touched`]).
    pub(super) fn pointer_released(&self, at: Point, now: Instant) {
        match self.engine.kind() {
            EngineKind::Corner => self.corner_touched(at, now),
            EngineKind::Spaces => {},
        }
    }

    /// True while the upkeep has work for the WM thread's fast tick. The Corner engine: the
    /// patch after a focus or through Show Desktop.
    pub(super) fn upkeep_wants_fast_tick(&self, now: Instant) -> bool {
        match self.engine.kind() {
            EngineKind::Corner => self.patch_replans(now) || self.reveal_fast(now),
            EngineKind::Spaces => false,
        }
    }

    /// The fast tick's share of the upkeep. The Corner engine: one plan of the patch at most
    /// ([`Self::corner_fast_tick`]).
    pub(super) fn upkeep_fast_tick(&mut self, now: Instant) {
        match self.engine.kind() {
            EngineKind::Corner => self.corner_fast_tick(now),
            EngineKind::Spaces => {},
        }
    }

    /// The tick's last step while managing: what the model counts hidden checked against
    /// the screen. The Corner engine: the patch planned, the inert-slivers tap given the
    /// window list, and the park reconcile ([`Self::corner_tick`]).
    pub(super) fn upkeep_tick(&mut self, ob: &mut Observer) {
        match self.engine.kind() {
            EngineKind::Corner => self.corner_tick(ob),
            EngineKind::Spaces => {},
        }
    }

    // ── Around a batch's window calls ───────────────────────────────────────────────────

    /// Where this one hide of window `id` goes in place of what the engine answers
    /// ([`WindowEngine::hidden_at`](crate::platform::window_engine::WindowEngine::hidden_at)),
    /// when one was set ([`Self::hide_once_at`]). The Corner engine: a minimized window's
    /// dive frame.
    pub(super) fn hide_place_once(&self, id: u32) -> Option<Rect> {
        match self.engine.kind() {
            EngineKind::Corner => self.corner.dive.filter(|(w, _)| *w == id).map(|(_, frame)| frame),
            EngineKind::Spaces => None,
        }
    }

    /// Sets (or with `None` forgets) the one hide that goes elsewhere
    /// ([`Self::hide_place_once`]): around a minimized window's batch.
    pub(super) fn hide_once_at(&mut self, place: Option<(u32, Rect)>) {
        match self.engine.kind() {
            EngineKind::Corner => self.corner.dive = place,
            EngineKind::Spaces => {},
        }
    }

    /// A show ([`Action::SetFrame`](crate::engine::model::Action::SetFrame)) of window `id`
    /// failed; `entry`: its element and the frame it was asked to take. The Corner engine:
    /// kept registered and retried until it is seen back ([`Self::restore_failed`]).
    pub(super) fn show_failed(&mut self, id: u32, entry: ParkedEntry) {
        match self.engine.kind() {
            EngineKind::Corner => self.restore_failed(id, entry),
            EngineKind::Spaces => {
                // The engine's show brings the window out of its stage Space before it asks
                // the app for the frame, so a show that failed left it on screen, where it
                // was: nothing is owed for safety, and nothing is kept registered.
                let _ = entry;
                tracing::warn!(id, "a shown window did not take its frame: it shows where it was");
            }
        }
    }

    /// A hide of window `id` failed. The Corner engine: a stray, parked again by the tick
    /// ([`Self::park_failed`]).
    pub(super) fn hide_failed(&mut self, id: u32, now: Instant) {
        match self.engine.kind() {
            EngineKind::Corner => self.park_failed(id, now),
            EngineKind::Spaces => {
                // The window is still on its Desktop. The tick's look puts it into its
                // stage's Space again ([`Self::upkeep_tick`]).
                let _ = now;
                tracing::warn!(id, "a window did not go into its stage's Space: tried again by the tick");
            }
        }
    }

    /// A window the model counts shown from now on, brought back where the window server
    /// may not show it yet (a Desktop not shown): `entry` stays in the safety registry until
    /// it is seen back. The Corner engine: as a failed restore ([`Self::show_failed`], said
    /// by the caller).
    pub(super) fn keep_until_seen(&mut self, id: u32, entry: ParkedEntry) {
        match self.engine.kind() {
            EngineKind::Corner => self.keep_unrestored(id, entry),
            EngineKind::Spaces => {},
        }
    }

    /// The registry entries the upkeep keeps for windows the model no longer counts hidden
    /// (or no longer has): they join every batch's registry sync
    /// ([`Self::parked_entries`]). The Corner engine: the windows whose restore failed.
    pub(super) fn kept_entries(&self) -> Vec<(u32, ParkedEntry)> {
        match self.engine.kind() {
            EngineKind::Corner => self.corner.unrestored.iter().map(|(id, u)| (*id, u.entry.clone())).collect(),
            EngineKind::Spaces => Vec::new(),
        }
    }

    /// The model just forgot windows it counted hidden (`kept`: their registry entries,
    /// [`Self::parked_entries_of`]); `why` it let them go. The Corner engine: each one still
    /// listed in a park zone stays registered until it is back or gone
    /// ([`Self::watch_forgotten`]).
    pub(super) fn let_go_hidden(&mut self, ob: &Observer, kept: Vec<(u32, ParkedEntry)>, why: LetGo) {
        match self.engine.kind() {
            EngineKind::Corner => self.watch_forgotten(ob, kept, why),
            EngineKind::Spaces => {
                // Nothing but the engine brings a window out of a stage Space, so a window
                // the model no longer counts must not stay in one: the engine's platform
                // half lets them out here.
                let _ = (ob, kept, why);
            }
        }
    }

    /// The user moves or resizes hidden window `id` with the button down. The Corner engine:
    /// a sliver grabbed; the park reconcile adopts the window where it is left
    /// ([`Self::reconcile_parked`]).
    pub(super) fn hidden_moved_by_user(&mut self, id: u32) {
        match self.engine.kind() {
            EngineKind::Corner => self.corner.strays.entry(id).or_default().user_moved = true,
            EngineKind::Spaces => {},
        }
    }

    // ── Shows still owed ────────────────────────────────────────────────────────────────

    /// The shows that failed are tried again: about once a second from the tick, with
    /// `force` (an unpark) every one at once. The Corner engine: [`Self::retry_unrestored`].
    pub(super) fn retry_shows(&mut self, ob: &Observer, force: bool) {
        match self.engine.kind() {
            EngineKind::Corner => self.retry_unrestored(ob, force),
            EngineKind::Spaces => {},
        }
    }

    /// How many windows are still owed a show.
    pub(super) fn shows_owed(&self) -> usize {
        match self.engine.kind() {
            EngineKind::Corner => self.corner.unrestored.len(),
            EngineKind::Spaces => 0,
        }
    }

    /// The windows still owed a show (quit counts what is left).
    pub fn shows_owed_ids(&self) -> BTreeSet<u32> {
        match self.engine.kind() {
            EngineKind::Corner => self.unrestored_ids(),
            EngineKind::Spaces => BTreeSet::new(),
        }
    }

    /// Quit, a stand-down, a logout's head start, after the last look: the windows owed a
    /// show are checked against the window server at once; how many are left. The Corner
    /// engine: [`Self::account_unrestored`].
    pub fn account_shows_owed(&mut self, ob: &Observer) -> usize {
        match self.engine.kind() {
            EngineKind::Corner => self.account_unrestored(ob),
            EngineKind::Spaces => 0,
        }
    }

    /// The registered windows the model let go as reported destroyed (a hidden tab's
    /// signature, [`crate::platform::safety::Ours::destroyed`]). The Corner engine:
    /// [`Self::destroyed_ids`].
    pub(super) fn let_go_destroyed(&self) -> BTreeSet<u32> {
        match self.engine.kind() {
            EngineKind::Corner => self.destroyed_ids(),
            EngineKind::Spaces => BTreeSet::new(),
        }
    }

    // ── A switch ────────────────────────────────────────────────────────────────────────

    /// What the switch's overlay must cover where the incoming windows (`arriving`) wait
    /// hidden behind their pictures, as it can be drawn now: `Ok(None)` for nothing to
    /// cover, an error (the reason, the windows it names) when there is and it cannot be.
    /// `outgoing`: the windows about to hide; `activated`: those macOS raised itself; `list`:
    /// the window server's list now. The Corner engine: the park corner
    /// ([`Self::switch_corner`], [`Self::cover_ready`]).
    pub(super) fn switch_cover(
        &self,
        arriving: &HashSet<u32>,
        outgoing: &HashSet<u32>,
        activated: &HashSet<u32>,
        list: &[windows::CgWindow],
    ) -> Result<Option<SwitchCover>, Refused> {
        match self.engine.kind() {
            EngineKind::Corner => {
                self.switch_corner(arriving, outgoing, activated, list).and_then(|c| self.cover_ready(c).map_err(Refused::from)).map(Some)
            }
            EngineKind::Spaces => Ok(None),
        }
    }

    // ── `ctl state` ─────────────────────────────────────────────────────────────────────

    /// `ctl state`'s `strays`: the hidden windows the screen contradicts. The Corner engine:
    /// the park reconcile's ([`Self::strays_status`]).
    pub(super) fn status_strays(&self) -> Vec<Value> {
        match self.engine.kind() {
            EngineKind::Corner => self.strays_status(),
            EngineKind::Spaces => Vec::new(),
        }
    }

    /// `ctl state`'s `corner_patch` for `screen` ([`Self::patch_status`]).
    pub(super) fn status_patch(&self, screen: u32) -> Value {
        match self.engine.kind() {
            EngineKind::Corner => self.patch_status(screen),
            EngineKind::Spaces => json!({ "on": false, "mode": "off", "shown": false, "rect": null, "refused_against": null, "hidden_why": null }),
        }
    }

    /// `ctl state`'s `patch_plan` for `screen` ([`Self::patch_plan_status`]).
    pub(super) fn status_patch_plan(&self, screen: u32) -> Value {
        match self.engine.kind() {
            EngineKind::Corner => self.patch_plan_status(screen),
            EngineKind::Spaces => Value::Null,
        }
    }
}
