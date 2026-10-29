//! `[windows] hide_parked_apps` (off by default, a prototype): apps whose every window sits
//! in inactive stages are hidden instead of showing their parked slivers
//! ([`crate::engine::hide`] has the rules).
//!
//! - **Hiding** ([`Controller::hide_pass`], every tick): the windows are parked as always
//!   first; an app that may be hidden is asked to hide (`NSRunningApplication.hide`). Once
//!   macOS says it is hidden, the windows of the stage an activation of it would bring go
//!   back to their own frames, unseen ("in place"), so ⌘Tab or a Dock click, which unhide
//!   the app natively, show them where they belong at once. An app still not hidden a
//!   second later is left parked and not asked again for a minute.
//! - **Our unhide** ([`Controller::before_batch_hides`], [`Controller::after_batch_hides`]):
//!   a batch that brings back a window of an app we hid first sends that app's other
//!   in-place windows the stage leaves parked to the corner (still unseen), runs, then
//!   unhides the app: its windows show at their frames. In an animated switch those windows
//!   come first, outside the transition (`execute_animated`'s early batch). The batch's
//!   focus ran while the app was still hidden and left it inactive (live 10-29: the app
//!   switched away from stayed the active app, so it was never hidden until the user
//!   activated something else); once the unhide has taken, the key window of such an app is
//!   focused again ([`Hides::refocus`]), and the hide pass runs again at every activation
//!   ([`Controller::hide_pass`] from `AppActivated`), so the app left behind is hidden the
//!   moment the new stage's app is active.
//! - **macOS's unhide** (⌘Tab, a Dock click, Show All; [`Controller::app_visibility`]): the
//!   app is ours no more; the activation's switch follows as for any app (its windows already
//!   in place count as arriving at once). An in-place window the switch did not bring (the
//!   focus picked another stage, or no switch came: Show All) goes to the corner a moment
//!   later ([`RECONCILE_AFTER`]), once no activation of the app is held and no switch is
//!   running ([`reconcile_due`]).
//! - **The user's hides** are never adopted (an app hidden already is never "ours") and
//!   never undone by an unhide of ours. A switch back to its stage raises its window, and
//!   macOS unhides the app for that, as Apple's Stage Manager does when the stage of a
//!   hidden app is clicked (live 10-29). Finder is never hidden, nor an app without a
//!   bundle id (our unhides match the pid against it).
//! - **Safety**: every app we hid is recorded ([`safety::set_hidden`]) in the parked file
//!   before it is asked to hide, and stays recorded until our unhide returns (a crash in
//!   between leaves it listed); every unpark path unhides it (quit through the batch above,
//!   the panic hook, the signals, the guardian), each only while the pid still runs the same
//!   bundle. An app that quits is forgotten at once ([`Controller::app_terminated`]).
//! - **Space checks** count an app we hid, or just unhid (its windows not on screen yet), as
//!   hidden ([`Controller::counts_hidden`]), so its windows never look as if they left the
//!   Desktop.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use super::{Controller, ax_focus};
use crate::engine::geometry::Point;
use crate::engine::hide::{HideFacts, may_hide};
use crate::engine::focus_replay::Wanted;
use crate::engine::model::{Action, WindowId};
use crate::platform::observe::Observer;
use crate::platform::windows::{self, FINDER_BUNDLE_ID};
use crate::platform::{apps, safety};

/// How long a hide may take before macOS counts as refusing it.
const HIDE_TIMEOUT: Duration = Duration::from_secs(1);
/// How long an app macOS refused to hide is left parked before it is asked again.
const REFUSED_FOR: Duration = Duration::from_secs(60);
/// How long after our unhide its notice (and windows not on screen yet) are ours.
const UNHIDE_ECHO: Duration = Duration::from_secs(1);
/// After macOS unhid an app of ours: when its in-place windows the switch left parked go to
/// the corner.
const RECONCILE_AFTER: Duration = Duration::from_millis(150);
/// The Accessibility timeout for the moves this module makes on its own (in place, back to
/// the corner): an app that does not answer must not hold the WM thread up. As
/// `arrival`'s query timeout; the app goes back to the global timeout right after
/// ([`crate::platform::ax::AxElement::bounded`]).
const HIDE_AX_TIMEOUT_SECONDS: f32 = 0.05;
/// A window smaller than this (either side) does not count as an unmanaged window of an app
/// (helper and offscreen utility windows).
const UNMANAGED_MIN: f64 = 50.0;

/// An app we hid.
#[derive(Debug)]
pub(super) struct Ours {
    bundle: String,
    asked: Instant,
    /// macOS said it is hidden.
    confirmed: bool,
    /// Windows moved back to their own frames while hidden.
    in_place: BTreeSet<u32>,
}

/// The state of `[windows] hide_parked_apps`.
#[derive(Debug, Default)]
pub(super) struct Hides {
    ours: BTreeMap<i32, Ours>,
    /// Apps a batch is bringing back (out of `ours`), still recorded until our unhide of
    /// each returns ([`Controller::after_batch_hides`]): their bundle ids.
    taking: BTreeMap<i32, String>,
    refused: HashMap<i32, Instant>,
    unhiding: HashMap<i32, Instant>,
    reconcile: Vec<(i32, BTreeSet<u32>, Instant)>,
    /// The key window a batch focused of each app it unhid, focused again once the unhide
    /// takes (a focus sent to a hidden app leaves it inactive).
    refocus: HashMap<i32, u32>,
}

impl Hides {
    /// True when app `pid` is one we hid.
    pub(super) fn ours(&self, pid: i32) -> bool {
        self.ours.contains_key(&pid)
    }

    /// What the parked file lists: the apps we hid, and those a batch is unhiding.
    fn recorded(&self) -> BTreeMap<i32, String> {
        let mut out: BTreeMap<i32, String> = self.taking.clone();
        out.extend(self.ours.iter().map(|(&pid, o)| (pid, o.bundle.clone())));
        out
    }

    /// Moves app `pid` from ours to the batch's unhides; its in-place windows.
    fn take_for_batch(&mut self, pid: i32) -> Option<BTreeSet<u32>> {
        let o = self.ours.remove(&pid)?;
        self.taking.insert(pid, o.bundle);
        Some(o.in_place)
    }

    /// App `pid` quit: nothing of it is ours any more (its pid may come back as another
    /// app's). True when something was forgotten that the parked file listed.
    fn forget(&mut self, pid: i32) -> bool {
        let listed = self.ours.remove(&pid).is_some() | self.taking.remove(&pid).is_some();
        self.refused.remove(&pid);
        self.unhiding.remove(&pid);
        self.refocus.remove(&pid);
        self.reconcile.retain(|(p, _, _)| *p != pid);
        listed
    }

    /// A batch unhid app `pid` and focused `focus` (its key window, when `of_pid` says the
    /// window is the app's): remembered to be focused again once the unhide has taken.
    fn remember_refocus(&mut self, pid: i32, focus: Option<u32>, of_pid: impl Fn(u32) -> Option<i32>) {
        if let Some(key) = focus.filter(|&k| of_pid(k) == Some(pid)) {
            self.refocus.insert(pid, key);
        }
    }
}

/// Whether an app's in-place windows that macOS's unhide left behind go to the corner now:
/// [`RECONCILE_AFTER`] has passed since the unhide, no activation of the app is held (its
/// switch may still bring them) and no switch's transition is running.
pub(super) fn reconcile_due(waited: Duration, held: bool, transition: bool) -> bool {
    waited >= RECONCILE_AFTER && !held && !transition
}

/// Whether pid's running app (`now`: its bundle id) is still the one we hid (`recorded`): a
/// pid macOS gave to another app since is never unhidden.
pub(super) fn same_app(now: Option<&str>, recorded: &str) -> bool {
    now.is_some_and(|b| b == recorded)
}

impl Controller {
    /// True when app `pid` counts as hidden for the Space checks: hidden by us, or unhidden
    /// by us a moment ago (its windows may not be on screen yet).
    pub(super) fn counts_hidden(&self, pid: i32) -> bool {
        self.hides.ours(pid) || self.hides.unhiding.get(&pid).is_some_and(|t| t.elapsed() < UNHIDE_ECHO)
    }

    fn record_hides(&self) {
        safety::set_hidden(self.hides.recorded());
    }

    /// App `pid` quit (NSWorkspace): forgets it in every list, and in the parked file.
    pub(super) fn app_terminated(&mut self, pid: i32) {
        if self.hides.forget(pid) {
            tracing::info!(pid, "hide: an app we hid quit; forgotten");
            self.record_hides();
        }
    }

    /// Every tick: confirms hides asked, moves the kept windows of confirmed ones in place,
    /// re-parks in-place windows macOS showed that no switch brought, and hides apps that
    /// may be hidden now. With the key off, unhides every app of ours.
    pub(super) fn hide_pass(&mut self, ob: &Observer) {
        let now = Instant::now();
        self.hides.unhiding.retain(|_, t| now.duration_since(*t) < UNHIDE_ECHO);
        self.hides.refused.retain(|_, t| now.duration_since(*t) < REFUSED_FOR);
        if self.model.is_none() || self.frozen() || self.space_pending {
            return;
        }
        self.reconcile(ob, now);
        if !self.store.config().windows.hide_parked_apps {
            if !self.hides.ours.is_empty() {
                tracing::info!(apps = self.hides.ours.len(), "hide_parked_apps is off: the apps we hid come back parked");
                self.unhide_all(ob);
            }
            return;
        }
        self.confirm_hides(ob, now);
        self.hide_new(ob, now);
    }

    fn confirm_hides(&mut self, ob: &Observer, now: Instant) {
        let pending: Vec<i32> = self.hides.ours.iter().filter(|(_, o)| !o.confirmed).map(|(&p, _)| p).collect();
        for pid in pending {
            match apps::is_hidden(pid) {
                Some(true) => {
                    let keep = self.model.as_ref().map(|m| m.app_park(pid).keep).unwrap_or_default();
                    let mut in_place = BTreeSet::new();
                    for (id, frame) in keep {
                        let Some(w) = ob.window(id) else { continue };
                        self.echoes.touch_window(id, now);
                        match w.element.bounded(HIDE_AX_TIMEOUT_SECONDS, |e| e.set_frame(frame)) {
                            Ok(()) => {
                                in_place.insert(id);
                            }
                            Err(e) => tracing::debug!(pid, id, "hide: a kept window cannot go in place while hidden: {e}"),
                        }
                    }
                    tracing::info!(pid, in_place = ?in_place, took_ms = self.hides.ours.get(&pid).map(|o| o.asked.elapsed().as_millis() as u64), "hide: the app is hidden; its kept stage waits in place");
                    if let Some(o) = self.hides.ours.get_mut(&pid) {
                        o.confirmed = true;
                        o.in_place = in_place;
                    }
                }
                Some(false) if now.duration_since(self.hides.ours.get(&pid).map_or(now, |o| o.asked)) >= HIDE_TIMEOUT => {
                    tracing::warn!(pid, "hide: macOS did not hide the app; it stays parked");
                    self.hides.ours.remove(&pid);
                    self.hides.refused.insert(pid, now);
                    self.record_hides();
                }
                Some(false) => {}
                None => {
                    self.hides.ours.remove(&pid);
                    self.record_hides();
                }
            }
        }
    }

    fn hide_new(&mut self, ob: &Observer, now: Instant) {
        let Some(m) = &self.model else { return };
        let pids: BTreeSet<i32> = m.windows().map(|w| w.meta.pid).collect();
        let candidates: Vec<i32> = pids
            .into_iter()
            .filter(|p| !self.hides.ours(*p))
            .filter(|p| {
                let park = m.app_park(*p);
                park.windows > 0 && park.shown == 0 && park.stage_here
            })
            .collect();
        if candidates.is_empty() {
            return;
        }
        let managed: HashSet<u32> = m.windows().map(|w| w.meta.id.0).collect();
        // Read once, and only when an app passes every cheap test first.
        let mut list: Option<Vec<windows::CgWindow>> = None;
        for pid in candidates {
            let bundle = ob.bundle_id(pid).map(String::from).or_else(|| apps::bundle_of(pid)).filter(|b| !b.is_empty());
            let bundle_known = bundle.is_some();
            let bundle = bundle.unwrap_or_default();
            let waiting = self.unplaced.iter().filter(|id| ob.window(**id).is_some_and(|w| w.pid == pid)).count();
            let mut facts = HideFacts {
                on: true,
                finder: bundle == FINDER_BUNDLE_ID,
                bundle_known,
                park: self.model.as_ref().map(|m| m.app_park(pid)).unwrap_or_default(),
                unmanaged: 0,
                waiting,
                hidden: apps::is_hidden(pid).unwrap_or(true),
                active: apps::is_active(pid).unwrap_or(true),
                refused: self.hides.refused.contains_key(&pid),
            };
            if may_hide(&facts).is_ok() {
                let list = list.get_or_insert_with(windows::list_windows);
                facts.unmanaged = list
                    .iter()
                    .filter(|w| w.pid == pid && w.layer == 0 && w.alpha > 0.0)
                    .filter(|w| w.bounds.w >= UNMANAGED_MIN && w.bounds.h >= UNMANAGED_MIN)
                    .filter(|w| !managed.contains(&w.id))
                    .count();
            }
            match may_hide(&facts) {
                Ok(()) => {
                    // Recorded before macOS is asked: a crash right after the hide still
                    // finds it in the parked file.
                    self.hides.ours.insert(pid, Ours { bundle: bundle.clone(), asked: now, confirmed: false, in_place: BTreeSet::new() });
                    self.record_hides();
                    // Its answer is not the outcome: on macOS 27 `hide` answers NO for another
                    // app and hides it a moment later (live 10-29: `ok=false`, then the hidden
                    // notice 20 ms on). The app stays ours, pending, until `confirm_hides`
                    // reads it hidden or `HIDE_TIMEOUT` passes.
                    let ok = apps::hide(pid);
                    tracing::info!(pid, bundle, ok, "hide: every window of the app is parked; hiding it");
                }
                Err(why) => tracing::trace!(pid, bundle, why, "hide: the app stays parked"),
            }
        }
    }

    /// Before a batch runs: the apps we hid that it brings a window of back (a `SetFrame`,
    /// a focus) are ours no more; their in-place windows the model still counts parked go to
    /// the corner first, unseen. They stay recorded until [`Self::after_batch_hides`] has
    /// unhidden them. Returns those apps, for it.
    pub(super) fn before_batch_hides(&mut self, ob: &Observer, actions: &[Action]) -> Vec<i32> {
        if self.hides.ours.is_empty() {
            return Vec::new();
        }
        let touched: BTreeSet<i32> = actions
            .iter()
            .filter_map(|a| match a {
                Action::SetFrame { id, .. } | Action::Focus { id } => ob.window(id.0).map(|w| w.pid),
                _ => None,
            })
            .filter(|p| self.hides.ours(*p))
            .collect();
        let now = Instant::now();
        for &pid in &touched {
            let Some(in_place) = self.hides.take_for_batch(pid) else { continue };
            self.repark(ob, pid, &in_place, now);
        }
        touched.into_iter().collect()
    }

    /// After a batch: unhides the apps [`Self::before_batch_hides`] took back (each only while
    /// its pid still runs the bundle we hid), then records them as ours no more. The batch's
    /// focus (`focus`) of a window of such an app ran while the app was hidden, which leaves
    /// the app inactive: it is sent again once the unhide has taken (now, when macOS was
    /// quick; else at its notice, [`Self::app_visibility`]).
    pub(super) fn after_batch_hides(&mut self, ob: &Observer, pids: Vec<i32>, focus: Option<u32>) {
        if pids.is_empty() {
            return;
        }
        let now = Instant::now();
        for pid in pids {
            let Some(bundle) = self.hides.taking.get(&pid).cloned() else { continue };
            if same_app(apps::bundle_of(pid).as_deref(), &bundle) {
                let ok = apps::unhide(pid);
                self.hides.unhiding.insert(pid, now);
                tracing::info!(pid, ok, "hide: a window of an app we hid comes back; unhiding it");
                self.hides.remember_refocus(pid, focus, |id| ob.window(id).map(|w| w.pid));
                if apps::is_hidden(pid) == Some(false) {
                    self.refocus(ob, pid);
                }
            } else {
                tracing::info!(pid, bundle, "hide: an app we hid is gone (or its pid is another app's); not unhidden");
            }
            self.hides.taking.remove(&pid);
        }
        self.record_hides();
    }

    /// Focuses again the key window a batch focused of app `pid` while the app was hidden
    /// (the unhide has taken now): the app becomes active, as the batch meant.
    fn refocus(&mut self, ob: &Observer, pid: i32) {
        let Some(key) = self.hides.refocus.remove(&pid) else { return };
        let Some(w) = ob.window(key) else { return };
        let now = Instant::now();
        self.echoes.touch_window(key, now);
        self.echoes.touch_app(pid, now);
        match ax_focus(&w.element, pid) {
            Ok(()) => tracing::debug!(pid, key, "hide: the key window focused again now that its app is unhidden"),
            Err(e) => tracing::debug!(pid, key, "hide: the key window cannot be focused again: {e}"),
        }
    }

    /// Sends app `pid`'s in-place windows the model counts parked to the corner.
    fn repark(&mut self, ob: &Observer, pid: i32, in_place: &BTreeSet<u32>, now: Instant) {
        let Some(m) = &self.model else { return };
        let moves: Vec<(u32, Point)> = in_place
            .iter()
            .filter(|id| m.window(WindowId(**id)).is_some_and(|w| w.parked))
            .filter_map(|id| m.park_frame_of(WindowId(*id)).map(|f| (*id, f.origin())))
            .collect();
        for (id, at) in moves {
            let Some(w) = ob.window(id) else { continue };
            self.echoes.touch_window(id, now);
            match w.element.bounded(HIDE_AX_TIMEOUT_SECONDS, |e| e.set_position(at)) {
                Ok(()) => tracing::debug!(pid, id, "hide: an in-place window this switch leaves goes to the corner"),
                Err(e) => tracing::warn!(pid, id, "hide: an in-place window cannot go to the corner: {e}"),
            }
        }
    }

    /// An app was hidden or unhidden (NSWorkspace). Our own notices confirm or echo (an
    /// unhide of ours that took: the batch's key window is focused again); macOS unhiding
    /// an app of ours (⌘Tab, the Dock, Show All) makes it no longer ours, and its in-place
    /// windows no switch brings go to the corner shortly after.
    pub(super) fn app_visibility(&mut self, ob: &Observer, pid: i32, hidden: bool) {
        if hidden {
            if let Some(o) = self.hides.ours.get(&pid) {
                tracing::debug!(pid, after_ms = o.asked.elapsed().as_millis() as u64, "hide: our hide took");
            } else {
                tracing::debug!(pid, "hide: the app was hidden by the user (never ours)");
            }
            return;
        }
        if self.hides.unhiding.contains_key(&pid) {
            tracing::debug!(pid, "hide: our unhide took");
            self.refocus(ob, pid);
            return;
        }
        if let Some(o) = self.hides.ours.remove(&pid) {
            tracing::info!(pid, in_place = ?o.in_place, "hide: macOS unhid an app we hid (an activation, Show All); its stage follows");
            self.hides.unhiding.insert(pid, Instant::now());
            self.hides.reconcile.push((pid, o.in_place, Instant::now()));
            self.record_hides();
        }
    }

    fn reconcile(&mut self, ob: &Observer, now: Instant) {
        if self.hides.reconcile.is_empty() {
            return;
        }
        let held = self.focus_replay.peek(now).map(Wanted::pid);
        let transition = self.pending.is_some();
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.hides.reconcile)
            .into_iter()
            .partition(|(pid, _, t)| reconcile_due(now.duration_since(*t), held == Some(*pid), transition));
        self.hides.reconcile = later;
        for (pid, in_place, _) in due {
            self.repark(ob, pid, &in_place, now);
        }
    }

    /// Unhides every app of ours, its in-place windows sent to the corner first (the key
    /// turned off).
    fn unhide_all(&mut self, ob: &Observer) {
        let now = Instant::now();
        let all: Vec<(i32, Ours)> = std::mem::take(&mut self.hides.ours).into_iter().collect();
        for (pid, o) in &all {
            self.repark(ob, *pid, &o.in_place, now);
            if same_app(apps::bundle_of(*pid).as_deref(), &o.bundle) {
                let ok = apps::unhide(*pid);
                self.hides.unhiding.insert(*pid, now);
                tracing::info!(pid, ok, "hide: unhidden");
            }
        }
        // Recorded once the unhides returned.
        self.record_hides();
    }

    /// The windows of apps we hid among a batch's `SetFrame`s: they come first and at once
    /// in an animated switch (macOS shows them where they are when the app unhides).
    pub(super) fn hidden_arrivals(&self, ob: &Observer, actions: &[Action]) -> HashSet<u32> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::SetFrame { id, .. } => Some(id.0),
                _ => None,
            })
            .filter(|id| ob.window(*id).is_some_and(|w| self.hides.ours(w.pid)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ours(bundle: &str) -> Ours {
        Ours { bundle: bundle.into(), asked: Instant::now(), confirmed: true, in_place: [7, 8].into() }
    }

    #[test]
    fn a_batch_s_unhide_stays_recorded_until_it_returns() {
        let mut h = Hides::default();
        h.ours.insert(10, ours("com.example.alpha"));
        h.ours.insert(11, ours("com.example.bravo"));
        let in_place = h.take_for_batch(10).expect("ours");
        assert_eq!(in_place, [7, 8].into());
        assert!(!h.ours(10), "ours no more for the batch's rules");
        let rec = h.recorded();
        assert_eq!(rec.get(&10).map(String::as_str), Some("com.example.alpha"), "still in the parked file");
        assert_eq!(rec.len(), 2);
        assert_eq!(h.take_for_batch(10), None, "taken once");
        h.taking.remove(&10);
        assert_eq!(h.recorded().keys().copied().collect::<Vec<_>>(), vec![11]);
    }

    #[test]
    fn a_quit_app_is_forgotten_everywhere() {
        let mut h = Hides::default();
        let now = Instant::now();
        h.ours.insert(10, ours("com.example.alpha"));
        h.taking.insert(12, "com.example.charlie".into());
        h.refused.insert(10, now);
        h.unhiding.insert(10, now);
        h.reconcile.push((10, [7].into(), now));
        h.reconcile.push((11, [9].into(), now));
        assert!(h.forget(10));
        assert!(!h.ours(10) && h.refused.is_empty() && h.unhiding.is_empty());
        assert_eq!(h.reconcile.len(), 1, "another app's reconcile stays");
        assert!(h.forget(12), "one a batch was unhiding");
        assert!(!h.forget(13), "never ours: nothing listed changes");
        assert!(h.recorded().is_empty());
    }

    /// Live 10-29: the batch's focus of a hidden app's window left the app inactive. The key
    /// window of the app the batch focused is remembered, for the app unhidden only, and
    /// forgotten with the app.
    #[test]
    fn the_batch_s_key_window_of_an_unhidden_app_is_focused_again() {
        let mut h = Hides::default();
        let of_pid = |id: u32| match id {
            7 | 8 => Some(10),
            9 => Some(11),
            _ => None,
        };
        h.remember_refocus(10, Some(7), of_pid);
        assert_eq!(h.refocus.get(&10), Some(&7));
        h.remember_refocus(11, Some(7), of_pid);
        assert!(!h.refocus.contains_key(&11), "another app's key is not this app's");
        h.remember_refocus(11, None, of_pid);
        assert!(!h.refocus.contains_key(&11), "a batch with no focus");
        h.remember_refocus(11, Some(4), of_pid);
        assert!(!h.refocus.contains_key(&11), "a window not observed");
        h.ours.insert(10, ours("com.example.alpha"));
        h.forget(10);
        assert!(h.refocus.is_empty(), "gone with the app");
    }

    #[test]
    fn a_reused_pid_is_never_unhidden() {
        assert!(same_app(Some("com.example.alpha"), "com.example.alpha"));
        assert!(!same_app(Some("com.example.other"), "com.example.alpha"));
        assert!(!same_app(None, "com.example.alpha"), "gone");
    }

    #[test]
    fn reconcile_waits_for_a_held_activation_and_a_running_switch() {
        let late = RECONCILE_AFTER + Duration::from_millis(1);
        assert!(!reconcile_due(Duration::from_millis(10), false, false), "too soon");
        assert!(reconcile_due(late, false, false));
        assert!(!reconcile_due(late, true, false), "its activation is held");
        assert!(!reconcile_due(late, false, true), "a switch is running");
        assert!(reconcile_due(late * 20, false, false), "later, once both are done");
    }
}
