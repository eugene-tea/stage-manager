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
//!   come first, outside the transition (`arrive_now`).
//! - **macOS's unhide** (⌘Tab, a Dock click, Show All; [`Controller::app_visibility`]): the
//!   app is ours no more; the activation's switch follows as for any app (its windows already
//!   in place count as arriving at once). An in-place window the switch did not bring (the
//!   focus picked another stage, or no switch came: Show All) goes to the corner a moment
//!   later ([`RECONCILE_AFTER`]).
//! - **The user's hides** are never adopted (an app hidden already is never "ours") and
//!   never undone. Finder is never hidden.
//! - **Safety**: every app we hid is recorded ([`safety::set_hidden`]) in the parked file;
//!   every unpark path unhides it (quit through the batch above, the panic hook, the signals,
//!   the guardian).
//! - **Space checks** count an app we hid, or just unhid (its windows not on screen yet), as
//!   hidden ([`Controller::counts_hidden`]), so its windows never look as if they left the
//!   Desktop.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use super::Controller;
use crate::engine::geometry::Point;
use crate::engine::hide::{HideFacts, may_hide};
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
    refused: HashMap<i32, Instant>,
    unhiding: HashMap<i32, Instant>,
    reconcile: Vec<(i32, BTreeSet<u32>, Instant)>,
}

impl Hides {
    /// True when app `pid` is one we hid.
    pub(super) fn ours(&self, pid: i32) -> bool {
        self.ours.contains_key(&pid)
    }
}

impl Controller {
    /// True when app `pid` counts as hidden for the Space checks: hidden by us, or unhidden
    /// by us a moment ago (its windows may not be on screen yet).
    pub(super) fn counts_hidden(&self, pid: i32) -> bool {
        self.hides.ours(pid) || self.hides.unhiding.get(&pid).is_some_and(|t| t.elapsed() < UNHIDE_ECHO)
    }

    fn record_hides(&self) {
        safety::set_hidden(self.hides.ours.iter().map(|(&pid, o)| (pid, o.bundle.clone())).collect());
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
                        match w.element.set_frame(frame) {
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
        let mut asked = false;
        for pid in candidates {
            let bundle = ob.bundle_id(pid).map(String::from).or_else(|| apps::bundle_of(pid)).unwrap_or_default();
            let waiting = self.unplaced.iter().filter(|id| ob.window(**id).is_some_and(|w| w.pid == pid)).count();
            let mut facts = HideFacts {
                on: true,
                finder: bundle == FINDER_BUNDLE_ID,
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
                    let ok = apps::hide(pid);
                    tracing::info!(pid, bundle, ok, "hide: every window of the app is parked; hiding it");
                    if ok {
                        self.hides.ours.insert(pid, Ours { bundle, asked: now, confirmed: false, in_place: BTreeSet::new() });
                        asked = true;
                    } else {
                        self.hides.refused.insert(pid, now);
                    }
                }
                Err(why) => tracing::trace!(pid, bundle, why, "hide: the app stays parked"),
            }
        }
        if asked {
            self.record_hides();
        }
    }

    /// Before a batch runs: the apps we hid that it brings a window of back (a `SetFrame`,
    /// a focus) are ours no more; their in-place windows the model still counts parked go to
    /// the corner first, unseen. Returns those apps, for [`Self::after_batch_hides`].
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
            let Some(o) = self.hides.ours.remove(&pid) else { continue };
            self.repark(ob, pid, &o.in_place, now);
        }
        if !touched.is_empty() {
            self.record_hides();
        }
        touched.into_iter().collect()
    }

    /// After a batch: unhides the apps [`Self::before_batch_hides`] took back.
    pub(super) fn after_batch_hides(&mut self, pids: Vec<i32>) {
        let now = Instant::now();
        for pid in pids {
            let ok = apps::unhide(pid);
            self.hides.unhiding.insert(pid, now);
            tracing::info!(pid, ok, "hide: a window of an app we hid comes back; unhiding it");
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
            match w.element.set_position(at) {
                Ok(()) => tracing::debug!(pid, id, "hide: an in-place window this switch leaves goes to the corner"),
                Err(e) => tracing::warn!(pid, id, "hide: an in-place window cannot go to the corner: {e}"),
            }
        }
    }

    /// An app was hidden or unhidden (NSWorkspace). Our own notices confirm or echo; macOS
    /// unhiding an app of ours (⌘Tab, the Dock, Show All) makes it no longer ours, and its
    /// in-place windows no switch brings go to the corner shortly after.
    pub(super) fn app_visibility(&mut self, pid: i32, hidden: bool) {
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
        let (due, later): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.hides.reconcile).into_iter().partition(|(_, _, t)| now.duration_since(*t) >= RECONCILE_AFTER);
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
        for (pid, o) in all {
            self.repark(ob, pid, &o.in_place, now);
            let ok = apps::unhide(pid);
            self.hides.unhiding.insert(pid, now);
            tracing::info!(pid, ok, "hide: unhidden");
        }
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
