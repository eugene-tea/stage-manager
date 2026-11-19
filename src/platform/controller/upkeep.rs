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
//! elsewhere answers each moment here with its own work, or with none. The Apple mode
//! ([`EngineKind::Apple`]) answers none in every arm: no controller of ours runs while Apple's
//! Stage Manager arranges the windows (`platform::apple::controller` does, behind
//! `platform::conductor`).

use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Controller, spaces};
use super::corner::unrestored::LetGo;
use crate::engine::corner::corner_patch::{Refused, SwitchCover};
use crate::engine::geometry::{Point, Rect};
use crate::platform::corner::sliver_tap;
use crate::platform::observe::Observer;
use crate::platform::safety::ParkedEntry;
use crate::platform::window_engine::EngineKind;
use crate::platform::windows;

/// The longest the tick rests while the inert slivers' tap judges presses on the tick's list
/// of the windows near the corners ([`Controller::upkeep_rest_cap`]): the tap trusts that list
/// for [`crate::engine::corner::sliver::SEEN_FRESH`] (1 s), so a rest, its timer's tolerance
/// (a tenth) and a tick's own time stay under it.
pub(super) const SLIVER_REST: Duration = Duration::from_millis(750);

impl Controller {
    /// The windows present on a Desktop that shows, as `list` (the window server's) and the
    /// engine say: those listed on screen, and those the engine hides where the list shows
    /// them off screen ([`WindowEngine::present_off_screen`](crate::platform::window_engine::WindowEngine::present_off_screen)).
    /// What every count of a Desktop's windows reads: a window gone from it left its
    /// Desktop, a hidden one did not.
    pub(super) fn present_in(&self, list: &[windows::CgWindow]) -> HashSet<u32> {
        let mut present = super::on_screen(list);
        present.extend(self.engine.present_off_screen());
        present
    }

    // ── What is hidden, for whoever watches it ──────────────────────────────────────────

    /// The hidden windows, where they wait, or what a press there may reach changed: after
    /// every batch, when an overview opens or closes, at a display or config change, when
    /// the Dock's bar moved. The Corner engine: the inert-slivers tap's shield
    /// ([`Self::publish_shield`]).
    pub(super) fn hidden_changed(&self) {
        match self.engine.kind() {
            EngineKind::Corner => self.publish_shield(),
            EngineKind::Spaces | EngineKind::Apple => {},
        }
    }

    /// The engine's look at the screen: after every batch, at a press or a release, when a
    /// wallpaper click's hold is over. The window server's on-screen list when it read one,
    /// for the caller's own use. The Corner engine: each screen's corner patch planned
    /// ([`Self::update_patch`]).
    pub(super) fn look_at_screen(&mut self) -> Option<std::rc::Rc<Vec<windows::CgWindow>>> {
        match self.engine.kind() {
            EngineKind::Corner => self.update_patch(),
            EngineKind::Spaces | EngineKind::Apple => None,
        }
    }

    /// A focus or an activation: macOS raises the app's windows, the hidden ones too. The
    /// Corner engine: the patch planned again now and on the fast tick for a moment
    /// ([`Self::replan_patch`]).
    pub(super) fn raised_by_macos(&mut self, now: Instant) {
        match self.engine.kind() {
            EngineKind::Corner => self.replan_patch(now),
            EngineKind::Spaces | EngineKind::Apple => {},
        }
    }

    /// A mouse-up at `at`. The Corner engine: a new still of the desktop in the corner
    /// shortly ([`Self::corner_touched`]).
    pub(super) fn pointer_released(&self, at: Point, now: Instant) {
        match self.engine.kind() {
            EngineKind::Corner => self.corner_touched(at, now),
            EngineKind::Spaces | EngineKind::Apple => {},
        }
    }

    /// True while the upkeep has work for the WM thread's fast tick. The Corner engine: the
    /// patch after a focus or through Show Desktop.
    pub(super) fn upkeep_wants_fast_tick(&self, now: Instant) -> bool {
        match self.engine.kind() {
            EngineKind::Corner => self.patch_replans(now) || self.reveal_fast(now),
            EngineKind::Spaces => self.mission_wants_fast(now),
            EngineKind::Apple => false,
        }
    }

    /// Why the tick stays brisk for the engine's upkeep ([`Controller::tick_waits`]): its
    /// retries at their old pace. The Corner engine: strays parked again, failed restores
    /// retried, the patch planned again after a focus. The Spaces engine: strays hidden
    /// again, shows owed.
    pub(super) fn upkeep_tick_waits(&self, now: Instant) -> Option<&'static str> {
        match self.engine.kind() {
            EngineKind::Corner => {
                if self.corner.strays.values().any(|s| !s.gave_up) {
                    Some("parked windows out of the corner")
                } else if !self.corner.unrestored.is_empty() {
                    Some("shows owed")
                } else if self.corner.patch_fast_until.is_some_and(|t| now < t) {
                    Some("the corner patch after a focus")
                } else {
                    None
                }
            }
            EngineKind::Spaces => {
                if self.spaces.strays.values().any(|tries| *tries < spaces::HIDE_TRIES) {
                    Some("hidden windows out of their Space")
                } else if !crate::platform::spaces::engine::shared().owed().is_empty() {
                    Some("shows owed")
                } else if crate::platform::spaces::engine::shared().watching_now() {
                    Some("a screen gone: hidden windows kept in their stages")
                } else {
                    None
                }
            }
            // Nothing of the Apple mode's is hidden: no controller of ours runs it.
            EngineKind::Apple => None,
        }
    }

    /// How long the tick may rest at most for the engine's upkeep, if it caps the rest
    /// ([`Controller::tick_rest`]). The Corner engine: while the inert slivers' tap judges
    /// presses, on the tick's list of the windows near the corners, which it trusts only
    /// while [`crate::engine::corner::sliver::SEEN_FRESH`] old ([`SLIVER_REST`]). The
    /// Spaces engine: no cap.
    pub(super) fn upkeep_rest_cap(&self) -> Option<Duration> {
        match self.engine.kind() {
            EngineKind::Corner => sliver_tap::wants_list().then_some(SLIVER_REST),
            EngineKind::Spaces | EngineKind::Apple => None,
        }
    }

    /// The fast tick's share of the upkeep. The Corner engine: one plan of the patch at most
    /// ([`Self::corner_fast_tick`]). The Spaces engine: Mission Control looked for after one
    /// of its keys, the hidden stages' windows put home for it and taken back
    /// ([`Self::mission_fast_tick`]).
    pub(super) fn upkeep_fast_tick(&mut self, ob: &Observer, now: Instant) {
        match self.engine.kind() {
            EngineKind::Corner => self.corner_fast_tick(now),
            EngineKind::Spaces => self.mission_fast_tick(ob, now),
            EngineKind::Apple => {}
        }
    }

    /// A key-down in another app. The Spaces engine: one of Mission Control's shortcuts
    /// ([`Self::mission_key`]); the Corner engine's hidden windows are on their Desktop,
    /// where Mission Control shows them.
    pub(super) fn upkeep_key(&mut self, ob: &Observer, event: &crate::platform::input::InputEvent, now: Instant) {
        match self.engine.kind() {
            EngineKind::Spaces => self.mission_key(ob, event, now),
            EngineKind::Corner | EngineKind::Apple => {}
        }
    }

    /// A notice of the Dock's (`name`). The Spaces engine: Mission Control may be opening
    /// ([`Self::mission_dock_notice`]).
    pub(super) fn upkeep_dock_notice(&mut self, name: &str, now: Instant) {
        match self.engine.kind() {
            EngineKind::Spaces => self.mission_dock_notice(name, now),
            EngineKind::Corner | EngineKind::Apple => {}
        }
    }

    /// `ctl debug-mission-control …` (a test's). The Spaces engine only
    /// ([`Self::debug_mission_control`]).
    pub(super) fn upkeep_debug_mission_control(&mut self, ob: &Observer, words: &[String]) -> Value {
        match self.engine.kind() {
            EngineKind::Spaces => self.debug_mission_control(ob, words),
            EngineKind::Corner | EngineKind::Apple => {
                json!({ "ok": false, "error": "only under the Spaces engine: with the others Mission Control shows every window already" })
            }
        }
    }

    /// The tick's last step while managing: what the model counts hidden checked against
    /// the screen. The Corner engine: the patch planned, the inert-slivers tap given the
    /// window list, and the park reconcile ([`Self::corner_tick`]).
    pub(super) fn upkeep_tick(&mut self, ob: &mut Observer) {
        match self.engine.kind() {
            EngineKind::Corner => self.corner_tick(ob),
            EngineKind::Spaces => {
                // Mission Control first: what it has put home is the engine's to count.
                self.mission_tick(ob, Instant::now());
                self.spaces_tick();
            }
            EngineKind::Apple => {}
        }
    }

    // ── Around a batch's window calls ───────────────────────────────────────────────────

    /// Where this one hide of window `id` goes in place of what the engine answers
    /// ([`WindowEngine::hidden_at`](crate::platform::window_engine::WindowEngine::hidden_at)),
    /// when one was set ([`Self::hide_once_at`]). Both engines: a minimized window's dive
    /// frame (the Spaces engine moves the window there and hides it).
    pub(super) fn hide_place_once(&self, id: u32) -> Option<Rect> {
        match self.engine.kind() {
            EngineKind::Corner => self.corner.dive.filter(|(w, _)| *w == id).map(|(_, frame)| frame),
            EngineKind::Spaces => self.spaces.dive.filter(|(w, _)| *w == id).map(|(_, frame)| frame),
            EngineKind::Apple => None,
        }
    }

    /// Sets (or with `None` forgets) the one hide that goes elsewhere
    /// ([`Self::hide_place_once`]): around a minimized window's batch.
    pub(super) fn hide_once_at(&mut self, place: Option<(u32, Rect)>) {
        match self.engine.kind() {
            EngineKind::Corner => self.corner.dive = place,
            EngineKind::Spaces => self.spaces.dive = place,
            EngineKind::Apple => {}
        }
    }

    /// A show ([`Action::SetFrame`](crate::engine::model::Action::SetFrame)) of window `id`
    /// failed; `entry`: its element and the frame it was asked to take. The Corner engine:
    /// kept registered and retried until it is seen back ([`Self::restore_failed`]).
    pub(super) fn show_failed(&mut self, id: u32, entry: ParkedEntry) {
        match self.engine.kind() {
            EngineKind::Corner => self.restore_failed(id, entry),
            EngineKind::Spaces => {
                // The engine brings the window out of its stage Space before it asks the app
                // for the frame. Still in the Space: the engine owes the show and the tick
                // tries it again ([`Self::retry_shows`]). Out of it away from the frame (a
                // lend's park, a minimize's dive): the engine owes the frame and tries it
                // again; the entry stays registered meanwhile, so a crash or a quit finds the
                // window (one still in its Space needs none: the Space ends with the process).
                // Out of it at its own frame: shown where it was, nothing is owed.
                let spaces = crate::platform::spaces::engine::shared();
                let owed = spaces.owed().contains(&id);
                if spaces.owes_frame(id) {
                    self.spaces.frames_owed.insert(id, entry);
                }
                tracing::warn!(id, owed, "a show failed: tried again when owed (still hidden, or out away from its frame), else shown where it was");
            }
            EngineKind::Apple => {}
        }
    }

    /// A hide of window `id` failed. The Corner engine: a stray, parked again by the tick
    /// ([`Self::park_failed`]).
    pub(super) fn hide_failed(&mut self, id: u32, now: Instant) {
        match self.engine.kind() {
            EngineKind::Corner => self.park_failed(id, now),
            EngineKind::Spaces => {
                // The window is still on its Desktop, the model counts it hidden: the
                // tick's look hides it again ([`Self::spaces_tick`]).
                let _ = now;
                tracing::info!(id, "hide failed: the window is hidden again by the tick");
            }
            EngineKind::Apple => {}
        }
    }

    /// A window the model counts shown from now on, brought back where the window server
    /// may not show it yet (a Desktop not shown): `entry` stays in the safety registry until
    /// it is seen back. The Corner engine: as a failed restore ([`Self::show_failed`], said
    /// by the caller).
    pub(super) fn keep_until_seen(&mut self, id: u32, entry: ParkedEntry) {
        match self.engine.kind() {
            EngineKind::Corner => self.keep_unrestored(id, entry),
            EngineKind::Spaces | EngineKind::Apple => {},
        }
    }

    /// The registry entries the upkeep keeps for windows the model no longer counts hidden
    /// (or no longer has): they join every batch's registry sync
    /// ([`Self::parked_entries`]). The Corner engine: the windows whose restore failed.
    pub(super) fn kept_entries(&self) -> Vec<(u32, ParkedEntry)> {
        match self.engine.kind() {
            EngineKind::Corner => self.corner.unrestored.iter().map(|(id, u)| (*id, u.entry.clone())).collect(),
            // The windows left at a lend's park whose show's frame is still owed.
            EngineKind::Spaces => {
                let owed = crate::platform::spaces::engine::shared().owed();
                self.spaces.frames_owed.iter().filter(|(id, _)| owed.contains(id)).map(|(id, e)| (*id, e.clone())).collect()
            }
            EngineKind::Apple => Vec::new(),
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
                // the model no longer counts must not stay in one: let out, where it was.
                let _ = (ob, why);
                let ids: Vec<u32> = kept.into_iter().map(|(id, _)| id).collect();
                let spaces = crate::platform::spaces::engine::shared();
                spaces.release(&ids, "the model let them go");
                spaces.forget_frames(&ids);
            }
            EngineKind::Apple => {}
        }
    }

    /// The user moves or resizes hidden window `id` with the button down. The Corner engine:
    /// a sliver grabbed; the park reconcile adopts the window where it is left
    /// ([`Self::reconcile_parked`]).
    pub(super) fn hidden_moved_by_user(&mut self, id: u32) {
        match self.engine.kind() {
            EngineKind::Corner => self.corner.strays.entry(id).or_default().user_moved = true,
            EngineKind::Spaces | EngineKind::Apple => {},
        }
    }

    // ── Shows still owed ────────────────────────────────────────────────────────────────

    /// The shows that failed are tried again: about once a second from the tick, with
    /// `force` (an unpark) every one at once. The Corner engine: [`Self::retry_unrestored`].
    pub(super) fn retry_shows(&mut self, ob: &Observer, force: bool) {
        match self.engine.kind() {
            EngineKind::Corner => self.retry_unrestored(ob, force),
            EngineKind::Spaces => {
                let _ = ob;
                let owed = crate::platform::spaces::engine::shared().retry_owed(force, &mut self.busy_apps.borrow_mut());
                self.spaces.frames_owed.retain(|id, _| owed.contains(id));
            }
            EngineKind::Apple => {}
        }
    }

    /// How many windows are still owed a show.
    pub(super) fn shows_owed(&self) -> usize {
        match self.engine.kind() {
            EngineKind::Corner => self.corner.unrestored.len(),
            EngineKind::Spaces => crate::platform::spaces::engine::shared().owed().len(),
            EngineKind::Apple => 0,
        }
    }

    /// The windows still owed a show (quit counts what is left).
    pub fn shows_owed_ids(&self) -> BTreeSet<u32> {
        match self.engine.kind() {
            EngineKind::Corner => self.unrestored_ids(),
            EngineKind::Spaces => crate::platform::spaces::engine::shared().owed(),
            EngineKind::Apple => BTreeSet::new(),
        }
    }

    /// Quit, a stand-down, a logout's head start, after the last look: the windows owed a
    /// show are checked against the window server at once; how many are left. The Corner
    /// engine: [`Self::account_unrestored`].
    pub fn account_shows_owed(&mut self, ob: &Observer) -> usize {
        match self.engine.kind() {
            EngineKind::Corner => self.account_unrestored(ob),
            EngineKind::Spaces => {
                let _ = ob;
                let owed = crate::platform::spaces::engine::shared().retry_owed(true, &mut self.busy_apps.borrow_mut());
                self.spaces.frames_owed.retain(|id, _| owed.contains(id));
                owed.len()
            }
            EngineKind::Apple => 0,
        }
    }

    /// The registered windows the model let go as reported destroyed (a hidden tab's
    /// signature, [`crate::platform::safety::Ours::destroyed`]). The Corner engine:
    /// [`Self::destroyed_ids`].
    pub(super) fn let_go_destroyed(&self) -> BTreeSet<u32> {
        match self.engine.kind() {
            EngineKind::Corner => self.destroyed_ids(),
            EngineKind::Spaces | EngineKind::Apple => BTreeSet::new(),
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
            EngineKind::Spaces | EngineKind::Apple => Ok(None),
        }
    }

    // ── `ctl state` ─────────────────────────────────────────────────────────────────────

    /// `ctl state`'s `engine`: which engine runs (`kind`), why (`why`, `probe`, `fallback`),
    /// and what it keeps and counts.
    pub(super) fn status_engine(&self) -> Value {
        let mut status = match self.engine.kind() {
            EngineKind::Corner => json!({ "kind": EngineKind::Corner.name() }),
            EngineKind::Spaces => {
                let mut status = crate::platform::spaces::engine::shared().status();
                status["kind"] = json!(EngineKind::Spaces.name());
                status
            }
            EngineKind::Apple => json!({ "kind": EngineKind::Apple.name() }),
        };
        // Why it runs (the launch's choice, the probe's verdict, a fallback), every engine.
        if let Some(c) = crate::platform::window_engine::choice() {
            status["why"] = json!(c.why);
            status["probe"] = json!(c.probe);
            status["fallback"] = json!(c.fallback);
        }
        status
    }

    /// `ctl state`'s `strays`: the hidden windows the screen contradicts. The Corner engine:
    /// the park reconcile's ([`Self::strays_status`]).
    pub(super) fn status_strays(&self) -> Vec<Value> {
        match self.engine.kind() {
            EngineKind::Corner => self.strays_status(),
            EngineKind::Spaces => self.spaces_strays_status(),
            EngineKind::Apple => Vec::new(),
        }
    }

    /// `ctl state`'s `mission_control`: the Spaces engine's lends for Mission Control
    /// ([`Self::mission_status`]); `null` under the others.
    pub(super) fn status_mission_control(&self) -> Value {
        match self.engine.kind() {
            EngineKind::Spaces => self.mission_status(),
            EngineKind::Corner | EngineKind::Apple => Value::Null,
        }
    }

    /// `ctl state`'s `corner_patch` for `screen` ([`Self::patch_status`]).
    pub(super) fn status_patch(&self, screen: u32) -> Value {
        match self.engine.kind() {
            EngineKind::Corner => self.patch_status(screen),
            EngineKind::Spaces | EngineKind::Apple => json!({ "on": false, "mode": "off", "shown": false, "rect": null, "refused_against": null, "hidden_why": null }),
        }
    }

    /// `ctl state`'s `patch_plan` for `screen` ([`Self::patch_plan_status`]).
    pub(super) fn status_patch_plan(&self, screen: u32) -> Value {
        match self.engine.kind() {
            EngineKind::Corner => self.patch_plan_status(screen),
            EngineKind::Spaces | EngineKind::Apple => Value::Null,
        }
    }
}
