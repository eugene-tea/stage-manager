//! Windows whose restore failed, and parked windows the model let go: the model counts each
//! shown (or no longer has it) while it may still sit in the park zone, so it stays in the
//! safety registry and is moved again about once a second until the window server lists it
//! out of the zone, or nowhere ([`Unrestored`], [`Controller::retry_unrestored`]).

use crate::platform::clock;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use super::super::*;

/// How often unrestored windows are checked and retried.
pub(in crate::platform::controller) const UNRESTORED_RETRY: Duration = Duration::from_secs(1);

/// A window whose restore (`SetFrame`) failed: the model counts it shown, but it may still sit
/// in the park zone. It keeps its registry entry until the window server lists it on screen
/// out of the park zone (the review's S1), and the restore is retried meanwhile.
#[derive(Debug)]
pub(in crate::platform::controller) struct Unrestored {
    /// The registry entry: element and the frame it was asked to take.
    pub(in crate::platform::controller) entry: ParkedEntry,
    /// Retries so far (the first failure is logged at warn, later ones at debug).
    pub(in crate::platform::controller) tries: u32,
    /// A parked window the model let go ([`Controller::watch_forgotten`]): quiet in the log
    /// until it is seen in the park zone a second time (closing a parked window lists it there
    /// for a moment; the review of 6X, L6).
    pub(in crate::platform::controller) forgotten: Forgotten,
    /// The element was looked up afresh in its app's list once already (L5).
    pub(in crate::platform::controller) refreshed: bool,
    /// Why the model let it go (the review of 8J): only a window reported destroyed while
    /// the window server still listed it may be judged a hidden tab.
    pub(in crate::platform::controller) why: LetGo,
    /// Listed in the park zone at its last check (true until checked: its restore failed,
    /// so it is where it was): the sliver tap and the corner patch still count it parked
    /// while so (the review's W4: the model says shown the moment the restore is
    /// asked, and a press on its sliver reached the window for the retry second).
    pub(in crate::platform::controller) in_zone: bool,
    /// Nothing could be done for it at its last check but wait ([`only_waits`]): listed off
    /// screen (its Desktop not shown, its app hidden). It keeps its entry and is looked at
    /// again at every tick, a rest apart at most, and within a second of the Space change or
    /// the unhide that lists it on screen (each stirs the tick); it does not keep the tick
    /// brisk ([`Controller::upkeep_tick_waits`], audit U3). False until checked.
    pub(in crate::platform::controller) waits: bool,
}

/// Why the model let a parked window go ([`Unrestored::why`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::platform::controller) enum LetGo {
    /// Not let go: a failed restore (the model still has it).
    Kept,
    /// Accessibility reported it destroyed while the window server still listed it: how
    /// macOS reports a tab no longer selected (or a window closing, listed for a moment).
    Destroyed,
    /// It left its Space or display, or anything else: a real window, never judged a tab.
    Other,
}

impl LetGo {
    /// What the hidden-tab test may judge of a window let go so (8J): only the tab's own
    /// signature, a window reported destroyed, is ever a candidate.
    fn candidate(self) -> Candidate {
        match self {
            Self::Destroyed => Candidate::Destroyed,
            Self::Kept | Self::Other => Candidate::Real,
        }
    }
}

/// How often a forgotten parked window was seen in the park zone ([`Unrestored::forgotten`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::platform::controller) enum Forgotten {
    /// Not a forgotten window: a failed restore.
    Not,
    /// Seen once, when the model let it go.
    Once,
    /// Seen again at a check: warned.
    Again,
}

impl Unrestored {
    pub(in crate::platform::controller) fn failed(entry: ParkedEntry) -> Self {
        Self { entry, tries: 0, forgotten: Forgotten::Not, refreshed: false, why: LetGo::Kept, in_zone: true, waits: false }
    }
}

impl Forgotten {
    /// A check of a forgotten window gave `step`: whether to warn now (its second sighting
    /// in the park zone) and what it is now. A closed window, gone at the first check,
    /// never warns (L6).
    fn after(self, step: UnrestoredStep) -> (Self, bool) {
        match (self, step) {
            (Self::Once, UnrestoredStep::Retry | UnrestoredStep::Wait) => (Self::Again, true),
            (f, _) => (f, false),
        }
    }
}

/// Whether a failed move is worth looking the window up afresh in its app's list: only when
/// the element we kept is stale (`kAXErrorInvalidUIElement`: a window reported destroyed while
/// ordered out), and once per window, not at every check while it refuses to move (L5).
pub(in crate::platform::controller) fn refresh_element(error: AxError, refreshed: bool) -> bool {
    !refreshed && error == AxError::INVALID_ELEMENT
}

/// What to do with an [`Unrestored`] window at a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::platform::controller) enum UnrestoredStep {
    /// Back on screen out of the park zone, gone, or parked again in the model's eyes (its
    /// own registry entry covers it): forget it.
    Done,
    /// On screen and still in the park zone: move it again.
    Retry,
    /// Not on screen (another Space, its app hidden): keep it registered and wait.
    Wait,
}

/// The step for an [`Unrestored`] window: `model_parked` is `None` when the model no longer
/// has the window, `listed` is `None` when the window server lists it nowhere, else whether
/// it is on screen and whether its frame lies in the park zone.
///
/// A window the model forgot (it left its Space, its element was reported destroyed) while
/// the window server still lists it in the park zone is not done: the model forgetting a
/// window does not bring it back (6X: such windows sat parked with no run tracking them).
/// Unless it is a hidden tab (`hidden_tab`, 8J: [`crate::engine::hidden_tab`]): off screen,
/// out of Accessibility's reach beside its group's selected tab, nothing can move it and it
/// takes its group's frame once selected.
///
/// A window of a `busy` app (one that did not answer lately, [`BusyApps`]) that would be
/// moved again waits instead: a hung app's windows cost the WM thread one timeout per
/// [`BUSY_BACKOFF`], not half a second per window every second (the review's K1).
pub(in crate::platform::controller) fn unrestored_step(model_parked: Option<bool>, listed: Option<(bool, bool)>, hidden_tab: bool, busy: bool) -> UnrestoredStep {
    match (model_parked, listed) {
        (Some(true), _) | (_, None) => UnrestoredStep::Done,
        (None, Some((false, true))) if hidden_tab => UnrestoredStep::Done,
        (_, Some((true, false))) => UnrestoredStep::Done,
        (_, Some((true, true))) if busy => UnrestoredStep::Wait,
        (_, Some((true, true))) => UnrestoredStep::Retry,
        (Some(false), Some((false, _))) | (None, Some((false, true))) => UnrestoredStep::Wait,
        (None, Some((false, false))) => UnrestoredStep::Done,
    }
}

/// Whether an [`Unrestored`] window whose check gave `step` only waits: the window server
/// lists it off screen (`listed`, as for [`unrestored_step`]), so no retry can act until its
/// Desktop shows or its app is unhidden. One that waits for a busy app's backoff is on
/// screen in the park zone: its retry is owed, and the tick stays brisk for it as before.
pub(in crate::platform::controller) fn only_waits(step: UnrestoredStep, listed: Option<(bool, bool)>) -> bool {
    step == UnrestoredStep::Wait && listed.is_some_and(|(onscreen, _)| !onscreen)
}

/// [`unrestored_step`], or with `force` (quit) a retry at once for every window still listed
/// in the park zone, on any Desktop, whatever its app said before. Never for one listed out of
/// the zone (S8 of the review of 10-29: the forced retry set the failed restore's old
/// frame on a window shown at a frame of its own on another Desktop).
pub(in crate::platform::controller) fn unrestored_step_at(force: bool, model_parked: Option<bool>, listed: Option<(bool, bool)>, hidden_tab: bool, busy: bool) -> UnrestoredStep {
    if force && !hidden_tab && model_parked != Some(true) && listed.is_some_and(|(_, zone)| zone) {
        UnrestoredStep::Retry
    } else {
        unrestored_step(model_parked, listed, hidden_tab, busy)
    }
}

/// The last word of a quit, a stand-down or a logout's head start on a window whose restore
/// failed, after the unpark and its leftovers check ([`Controller::account_unrestored`]): it is
/// left only while the window server lists it in a park zone (`in_zone`; `None`: not listed,
/// closed). One listed out of every park zone on a Desktop not shown is not parked, whatever
/// its handle answered: nothing is left to bring back, so it keeps neither the exit unclean
/// nor a guardian waiting (a log of 11-09: a window whose handle had died, listed at its own
/// frame on another Desktop, made the quit unclean, and the guardian could only wait for that
/// Desktop to show and let it go unmoved).
pub(in crate::platform::controller) fn left_after_unpark(in_zone: Option<bool>) -> bool {
    in_zone == Some(true)
}

/// Whether an [`Unrestored`] window is let go as a hidden tab (8J): only one the model no
/// longer has (`model_parked` `None`), let go as reported destroyed (`why`: the tab's own
/// signature; the review of 8J), listed off screen in the park zone (`listed`), and judged
/// one by `judge` with its [`Candidate`] (asked only then: it reads Accessibility).
pub(in crate::platform::controller) fn let_go_as_tab(model_parked: Option<bool>, listed: Option<(bool, bool)>, why: LetGo, judge: impl FnOnce(Candidate) -> bool) -> bool {
    let candidate = why.candidate();
    model_parked.is_none() && candidate != Candidate::Real && listed == Some((false, true)) && judge(candidate)
}

impl Controller {
    /// Keeps a window whose `SetFrame` failed registered (with the frame it was asked to
    /// take) until it is seen back out of the park zone; [`Self::retry_unrestored`] retries
    /// it. Never lets the batch's registry replace drop a window whose restore failed (S1).
    pub(in crate::platform::controller) fn restore_failed(&mut self, id: u32, entry: ParkedEntry) {
        tracing::warn!(id, restore = %entry.restore, "restore failed: the window stays registered and is retried until it is seen back");
        self.corner.unrestored.insert(id, Unrestored::failed(entry));
    }

    /// Checks windows whose restore failed against the window server, about once a second:
    /// seen on screen out of the park zone, gone, or parked again in the model → forgotten;
    /// still in the park zone → moved again; off screen → kept registered. With `force`
    /// (quit) every one is retried at once, whatever the list says, a busy app tried again
    /// quickly within one [`leftovers::RESTORE_RETRY`] budget per window (the review of 6X,
    /// L1).
    pub(in crate::platform::controller) fn retry_unrestored(&mut self, ob: &Observer, force: bool) {
        if self.corner.unrestored.is_empty() {
            return;
        }
        let now = clock::now();
        if !force && now.duration_since(self.corner.unrestored_checked) < UNRESTORED_RETRY {
            return;
        }
        let _quiet = enhanced_ui::Batch::open();
        self.corner.unrestored_checked = now;
        let list = windows::wake_list();
        let mut scan: Option<recovery::ScanDisplays> = None;
        let mut tabs: Option<hidden_tabs::Tabs> = None;
        let mut shown: Option<BTreeSet<u32>> = None;
        let before = self.corner.unrestored.len();
        let ids: Vec<u32> = self.corner.unrestored.keys().copied().collect();
        for id in ids {
            let model_parked = self.model.as_ref().and_then(|m| m.window(WindowId(id))).map(|w| w.parked);
            let listed = list.iter().find(|w| w.id == id).map(|w| {
                let zone = match model_parked {
                    Some(_) => self.model.as_ref().is_some_and(|m| m.window(WindowId(id)).is_some_and(|x| m.listed_hidden(x.meta.display, w.bounds))),
                    // A window the model forgot: the safety paths' test, which honours the
                    // other displays (it may sit on one now) and knows it is ours (L2).
                    None => recovery::is_stranded(w.bounds, scan.get_or_insert_with(safety::scan_displays), Known::Registered),
                };
                (w.onscreen, zone)
            });
            // A hidden tab (8J)? Only by its own signature: one that left its Space stays real.
            let why = self.corner.unrestored.get(&id).map_or(LetGo::Kept, |u| u.why);
            let hidden_tab = let_go_as_tab(model_parked, listed, why, |candidate| {
                list.iter().find(|w| w.id == id).is_some_and(|w| {
                    let shown = shown.get_or_insert_with(|| self.model_shown_ids());
                    tabs.get_or_insert_with(|| hidden_tabs::Tabs::new(&list)).is_hidden_tab(w, candidate, |i| shown.contains(&i))
                })
            });
            let pid = ob.window(id).map(|w| w.pid).or_else(|| self.corner.unrestored.get(&id).and_then(|u| u.entry.element.pid().ok()));
            // A busy app is left alone between checks, never at quit (its own budget, K1).
            let busy = !force && pid.is_some_and(|p| self.busy_apps.borrow().busy(p, now));
            let step = unrestored_step_at(force, model_parked, listed, hidden_tab, busy);
            let Some(u) = self.corner.unrestored.get_mut(&id) else { continue };
            u.in_zone = listed.is_some_and(|(_, zone)| zone);
            u.waits = only_waits(step, listed);
            // A forgotten window is quiet until it is seen in the zone a second time (L6).
            let quiet = u.forgotten == Forgotten::Once;
            let (forgotten, warn) = u.forgotten.after(step);
            u.forgotten = forgotten;
            if warn {
                tracing::warn!(id, ?listed, restore = %u.entry.restore, "a parked window the model let go is still in the park zone: it stays registered until it is back");
            }
            match step {
                UnrestoredStep::Done if hidden_tab => {
                    tracing::info!(id, ?listed, "a parked window the model let go is a hidden tab: nothing can move it, and it needs not be");
                    self.corner.unrestored.remove(&id);
                }
                UnrestoredStep::Done if quiet => {
                    tracing::debug!(id, ?listed, "a parked window the model let go is accounted for (closed, or back)");
                    self.corner.unrestored.remove(&id);
                }
                UnrestoredStep::Done => {
                    tracing::info!(id, ?model_parked, ?listed, "a window whose restore failed is accounted for");
                    self.corner.unrestored.remove(&id);
                }
                UnrestoredStep::Wait => {}
                UnrestoredStep::Retry => {
                    u.tries += 1;
                    let element = ob.element(id).unwrap_or_else(|| u.entry.element.clone());
                    let restore = u.entry.restore;
                    let set = |e: &AxElement, end: Instant| {
                        if force { safety::set_frame_retrying(e, id, restore, end.saturating_duration_since(clock::now())) } else { e.set_frame(restore) }
                    };
                    let mut result = set(&element, clock::now() + leftovers::RESTORE_RETRY.budget);
                    if let Err(e) = result
                        && refresh_element(e, u.refreshed)
                        && let Ok(pid) = element.pid()
                    {
                        // The element we kept went stale (a window reported destroyed while
                        // ordered out): the app's own list may have it again. Once (L5). The
                        // lookup and its try have a budget of their own (the review of 6X2, 1:
                        // the busy app's budget never fitted them, so at quit it never ran).
                        u.refreshed = true;
                        let end = clock::now() + leftovers::REFRESH_BUDGET;
                        match safety::ax_window(pid, id) {
                            // At quit, the try only when it still fits after the lookup.
                            Ok(fresh) if !force || leftovers::leftover_fits(end.saturating_duration_since(clock::now()), true) => {
                                result = set(&fresh, end);
                                // The observer keeps it too (stale-handles): every later move.
                                ob.note_fresh(id, fresh.clone());
                                u.entry.element = fresh;
                            }
                            Ok(fresh) => {
                                tracing::warn!(id, "a stale element's lookup ran long: no time left to try the window afresh");
                                ob.note_fresh(id, fresh.clone());
                                u.entry.element = fresh;
                            }
                            Err(why) => tracing::debug!(id, "a stale element's lookup: {why}"),
                        }
                    }
                    if let Some(pid) = pid {
                        let reply = match &result {
                            Ok(()) => Reply::No,
                            Err(e) if *e == AxError::CANNOT_COMPLETE => Reply::TimedOut,
                            Err(_) => Reply::CannotTell,
                        };
                        self.busy_apps.borrow_mut().note(pid, reply, now);
                    }
                    match result {
                        Ok(()) => {
                            tracing::info!(id, tries = u.tries, restore = %u.entry.restore, "a failed restore went through on a retry");
                            // A window moved: a click pressed before reads the frames again.
                            self.pressed_obs = None;
                            if force {
                                self.corner.unrestored.remove(&id);
                            }
                        }
                        Err(e) if u.tries == 1 || force => tracing::warn!(id, tries = u.tries, "a failed restore failed again: {e}"),
                        Err(e) => tracing::debug!(id, tries = u.tries, "a failed restore failed again: {e}"),
                    }
                }
            }
        }
        if self.corner.unrestored.len() != before {
            safety::registry_replace(self.parked_entries(ob));
        }
    }

    /// Windows the model just forgot while they were parked (`kept`: their registry entries):
    /// each one the window server still lists in the park zone stays registered as an
    /// unrestored window ([`Unrestored`]) until it is seen back or gone, so no run loses
    /// track of a window it parked (6X). `why`: why the model let them go (only
    /// [`LetGo::Destroyed`] may later be judged a hidden tab).
    pub(in crate::platform::controller) fn watch_forgotten(&mut self, ob: &Observer, kept: Vec<(u32, ParkedEntry)>, why: LetGo) {
        if kept.is_empty() {
            return;
        }
        let ids: Vec<u32> = kept.iter().map(|(id, _)| *id).collect();
        let listed = windows::describe(&ids);
        let displays = safety::scan_displays();
        let mut added = false;
        for (id, entry) in kept {
            if self.corner.unrestored.contains_key(&id) || self.model.as_ref().is_some_and(|m| m.window(WindowId(id)).is_some()) {
                continue;
            }
            let Some(l) = listed.iter().find(|l| l.id == id) else { continue };
            // The safety paths' test: it honours the other displays (L2).
            if !recovery::is_stranded(l.bounds, &displays, Known::Registered) {
                continue;
            }
            // A closed window is listed here for a moment: quiet until a second sighting (L6).
            tracing::debug!(id, at = %l.bounds, onscreen = l.onscreen, restore = %entry.restore, "a parked window the model let go is in the park zone: it stays registered until it is back or gone");
            self.corner.unrestored.insert(id, Unrestored { forgotten: Forgotten::Once, why, ..Unrestored::failed(entry) });
            added = true;
        }
        if added {
            safety::registry_replace(self.parked_entries(ob));
        }
    }

    /// Windows whose registry entry is kept while the model has let them go as reported
    /// destroyed (the tab's signature): the only registered windows the hidden-tab test may
    /// judge (the review of 8J, [`safety::Ours::destroyed`]). One that left its Space, or was
    /// let go for any other reason, is not among them. None without a model.
    pub fn destroyed_ids(&self) -> BTreeSet<u32> {
        let Some(m) = &self.model else { return BTreeSet::new() };
        self.corner.unrestored.iter().filter(|(id, u)| u.why == LetGo::Destroyed && m.window(WindowId(**id)).is_none()).map(|(id, _)| *id).collect()
    }

    /// The windows whose restore failed that are still on record.
    pub fn unrestored_ids(&self) -> BTreeSet<u32> {
        self.corner.unrestored.keys().copied().collect()
    }

    /// Quit, after its leftovers check: windows whose restore failed are checked against the
    /// window server at once (no wait for the once-a-second check): those seen back out of the
    /// park zone, or gone, are forgotten; one still in the zone on screen is tried once more;
    /// one in no park zone on a Desktop not shown is not parked and is forgotten too
    /// ([`Self::forget_not_parked`]). Returns how many are left (in a park zone on another
    /// Space, or of an app that does not answer).
    pub fn account_unrestored(&mut self, ob: &Observer) -> usize {
        self.corner.unrestored_checked = clock::now().checked_sub(UNRESTORED_RETRY).unwrap_or(self.corner.unrestored_checked);
        self.retry_unrestored(ob, false);
        self.forget_not_parked(ob);
        self.corner.unrestored.len()
    }

    /// After an unpark (quit, stand-down, a logout's head start): a window whose restore
    /// failed that the window server lists in no park zone, on any Desktop, is not parked
    /// ([`left_after_unpark`]) and leaves the registry. The test is the widest one: every
    /// corner used this run on every screen, slivers counted, and the window's own display's.
    /// Not while a Desktop change settles or a slide is held: the list is not to be trusted
    /// then, and the window stays on record as before.
    pub(in crate::platform::controller) fn forget_not_parked(&mut self, ob: &Observer) {
        if self.corner.unrestored.is_empty() || self.phase.frames_settling(clock::now()) || self.slide_held() {
            return;
        }
        let ids: Vec<u32> = self.corner.unrestored.keys().copied().collect();
        let listed = windows::describe(&ids);
        let displays = safety::scan_displays();
        let before = self.corner.unrestored.len();
        for id in ids {
            let at = listed.iter().find(|w| w.id == id);
            let in_zone = at.map(|w| {
                displays.parks(w.bounds)
                    || self.model.as_ref().is_some_and(|m| m.window(WindowId(id)).is_some_and(|x| m.listed_hidden(x.meta.display, w.bounds)))
            });
            if left_after_unpark(in_zone) {
                continue;
            }
            tracing::info!(id, at = ?at.map(|w| w.bounds), onscreen = ?at.map(|w| w.onscreen), "a window whose restore failed is in no park zone: not parked, nothing to bring back");
            self.corner.unrestored.remove(&id);
        }
        if self.corner.unrestored.len() != before {
            safety::registry_replace(self.parked_entries(ob));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// K1: a busy app's unrestored window is not moved again while the app is left alone.
    #[test]
    fn a_busy_app_s_unrestored_window_waits_for_the_backoff() {
        use UnrestoredStep::*;
        assert_eq!(unrestored_step(Some(false), Some((true, true)), false, true), Wait, "busy: not asked");
        assert_eq!(unrestored_step(None, Some((true, true)), false, true), Wait);
        assert_eq!(unrestored_step(Some(false), Some((true, false)), false, true), Done, "seen back: done, busy or not");
    }

    #[test]
    fn a_failed_restore_stays_registered_until_the_window_is_seen_back() {
        use UnrestoredStep::*;
        // Still in the park zone on screen: moved again, never dropped.
        assert_eq!(unrestored_step(Some(false), Some((true, true)), false, false), Retry);
        // Off screen (another Space, app hidden): kept, waiting.
        assert_eq!(unrestored_step(Some(false), Some((false, true)), false, false), Wait);
        assert_eq!(unrestored_step(Some(false), Some((false, false)), false, false), Wait);
        // Seen back out of the zone, closed, or parked again by the model: done.
        assert_eq!(unrestored_step(Some(false), Some((true, false)), false, false), Done);
        assert_eq!(unrestored_step(Some(false), None, false, false), Done);
        assert_eq!(unrestored_step(Some(true), Some((true, true)), false, false), Done);
    }

    /// Audit U3 (11-25): a window brought back on a Desktop not shown, or off screen with its
    /// app hidden, only waits: no retry can act, so it does not keep the tick brisk. One still
    /// on screen in the park zone does, a busy app's included.
    #[test]
    fn a_window_that_only_waits_off_screen_does_not_keep_the_tick_brisk() {
        use UnrestoredStep::*;
        let step = |model, listed, busy| (unrestored_step(model, listed, false, busy), listed);
        let waits = |(step, listed)| only_waits(step, listed);
        assert!(waits(step(Some(false), Some((false, false)), false)), "put back on a Desktop not shown");
        assert!(waits(step(Some(false), Some((false, true)), false)), "in another Desktop's corner, the model says shown");
        assert!(waits(step(None, Some((false, true)), false)), "forgotten, in another Desktop's corner");
        assert!(!waits(step(Some(false), Some((true, true)), false)), "on screen in the corner: moved again");
        assert!(!waits(step(Some(false), Some((true, true)), true)), "a busy app's: its retry is owed after the backoff");
        assert_eq!(step(Some(false), Some((true, true)), true).0, Wait);
        assert!(!waits(step(Some(false), Some((true, false)), false)), "seen back: done");
        assert!(!waits(step(Some(false), None, false)), "closed: done");
        let u = Unrestored::failed(ParkedEntry { element: AxElement::system_wide(), restore: Rect::new(0.0, 0.0, 10.0, 10.0), child: false, fitted: None });
        assert!(!u.waits, "not checked yet: its first check comes at the brisk pace");
    }

    /// S8 (the review of 10-29): quit's forced retry tries every window still listed in
    /// the park zone at once, on screen or not; one listed out of the zone (shown at a frame
    /// of its own on another Desktop, or back on this one) is never set to its old frame.
    #[test]
    fn quits_forced_retry_reaches_only_windows_in_the_park_zone() {
        use UnrestoredStep::*;
        assert_eq!(unrestored_step_at(true, Some(false), Some((false, true)), false, false), Retry, "another Desktop's corner");
        assert_eq!(unrestored_step_at(true, None, Some((false, true)), false, false), Retry, "forgotten, in a corner");
        assert_eq!(unrestored_step_at(true, Some(false), Some((false, false)), false, false), Wait, "another Desktop, its own frame");
        assert_eq!(unrestored_step_at(true, Some(false), Some((true, false)), false, false), Done, "back on this one");
        assert_eq!(unrestored_step_at(true, Some(true), Some((true, true)), false, false), Done, "parked again by the model");
        assert_eq!(unrestored_step_at(true, None, Some((false, true)), true, false), Done, "a hidden tab");
        assert_eq!(unrestored_step_at(false, Some(false), Some((false, true)), false, false), Wait, "not forced: waits");
    }

    /// A log of 11-09: a window whose handle had died sat at its own frame on a Desktop not
    /// shown; its restore failed at the quit, the running app's rule kept it on record (off
    /// screen: wait), and the quit was unclean for a window that was never in the corner.
    /// After the unpark only a window listed in a park zone is left.
    #[test]
    fn after_the_unpark_only_a_window_in_a_park_zone_is_left() {
        // The running app's rule for it: wait (it may still show parked on its Desktop).
        assert_eq!(unrestored_step(Some(false), Some((false, false)), false, false), UnrestoredStep::Wait);
        assert!(!left_after_unpark(Some(false)), "listed out of every park zone: not parked");
        assert!(!left_after_unpark(None), "not listed: closed");
        assert!(left_after_unpark(Some(true)), "in a park zone, on any Desktop: still to bring back");
    }

    /// 6X: a parked window the model forgot (it left its Space, its element was reported
    /// destroyed) while the window server still lists it in the park zone stays registered.
    #[test]
    fn a_parked_window_the_model_forgot_stays_registered_until_it_is_back() {
        use UnrestoredStep::*;
        assert_eq!(unrestored_step(None, Some((true, true)), false, false), Retry, "on screen in the corner: moved back");
        assert_eq!(unrestored_step(None, Some((false, true)), false, false), Wait, "another Space's corner: waits for it");
        assert_eq!(unrestored_step(None, Some((true, false)), false, false), Done, "seen back");
        assert_eq!(unrestored_step(None, Some((false, false)), false, false), Done, "out of the zone elsewhere");
        assert_eq!(unrestored_step(None, None, false, false), Done, "closed");
    }

    /// 8J: a parked window the model let go that is a hidden tab (off screen in the park
    /// zone, its group's selected tab listed or managed) is let go: nothing can move it. A
    /// window the model still has is never judged one, nor one on screen.
    #[test]
    fn a_forgotten_hidden_tab_is_let_go() {
        use UnrestoredStep::*;
        assert_eq!(unrestored_step(None, Some((false, true)), true, false), Done, "a hidden tab at the park spot");
        assert_eq!(unrestored_step(None, Some((true, true)), true, false), Retry, "on screen: no hidden tab");
        assert_eq!(unrestored_step(Some(false), Some((false, true)), true, false), Wait, "the model has it: a real window");
    }

    /// The review of 8J, High: only a window the model let go as reported destroyed is ever
    /// judged; one that left its Space (dragged to another Desktop in Mission Control) beside
    /// a same-size sibling that would vouch stays real: it waits (and a quit retries it).
    #[test]
    fn only_a_window_reported_destroyed_is_judged_a_hidden_tab() {
        use UnrestoredStep::*;
        let zone_off = Some((false, true));
        let vouched = |c: Candidate| c == Candidate::Destroyed;
        assert!(let_go_as_tab(None, zone_off, LetGo::Destroyed, vouched), "the tab's signature");
        let mut asked = false;
        assert!(!let_go_as_tab(None, zone_off, LetGo::Other, |_| { asked = true; true }), "left its Space: real");
        assert!(!asked, "a real window is never even judged");
        assert!(!let_go_as_tab(None, zone_off, LetGo::Kept, |_| true), "a failed restore: real");
        assert!(!let_go_as_tab(Some(false), zone_off, LetGo::Destroyed, |_| true), "the model has it again");
        assert!(!let_go_as_tab(None, Some((true, true)), LetGo::Destroyed, |_| true), "on screen");
        assert!(!let_go_as_tab(None, Some((false, false)), LetGo::Destroyed, |_| true), "out of the zone");
        // The left-Space window: no hidden tab, so it waits off screen, as before.
        let tab = let_go_as_tab(None, zone_off, LetGo::Other, |_| true);
        assert_eq!(unrestored_step(None, zone_off, tab, false), Wait);
        assert_eq!(LetGo::Other.candidate(), Candidate::Real);
        assert_eq!(LetGo::Destroyed.candidate(), Candidate::Destroyed);
    }

    /// The review of 6X, L5: a window that refuses to move is looked up afresh only when its
    /// element went stale, and once.
    #[test]
    fn a_fresh_lookup_only_for_a_stale_element_and_once() {
        assert!(refresh_element(AxError::INVALID_ELEMENT, false));
        assert!(!refresh_element(AxError::INVALID_ELEMENT, true), "once");
        assert!(!refresh_element(AxError::CANNOT_COMPLETE, false), "a busy app keeps its element");
        assert!(!refresh_element(AxError::ATTRIBUTE_UNSUPPORTED, false), "a window that refuses to move");
    }

    /// The review of 6X, L6: closing a parked window lists it in the park zone for a moment;
    /// it warns only when a check sees it there again.
    #[test]
    fn a_forgotten_window_warns_only_on_its_second_sighting() {
        use UnrestoredStep::*;
        assert_eq!(Forgotten::Once.after(Done), (Forgotten::Once, false), "closed: gone at the first check, quiet");
        assert_eq!(Forgotten::Once.after(Retry), (Forgotten::Again, true));
        assert_eq!(Forgotten::Once.after(Wait), (Forgotten::Again, true));
        assert_eq!(Forgotten::Again.after(Retry), (Forgotten::Again, false), "warned once");
        assert_eq!(Forgotten::Not.after(Retry), (Forgotten::Not, false), "a failed restore logs as before");
    }
}
