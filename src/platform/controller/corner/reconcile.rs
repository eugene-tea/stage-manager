//! The park reconcile ([`crate::engine::corner::park_reconcile`]): a window the model counts parked
//! that the window server lists out of the park zone, or whose park failed, is parked again
//! or adopted as shown, every tick.

use crate::platform::clock;
use std::collections::HashSet;
use std::time::Instant;

use super::super::*;

impl Controller {
    /// A `Park` that failed (a busy app, one mid-launch) must not leave the model saying
    /// parked with nothing to make it so (the review's K2): the window is a stray,
    /// parked again by the tick with a backoff ([`Self::reconcile_parked`]). One the
    /// reconciler is already on keeps its count (its try was this one).
    pub(in crate::platform::controller) fn park_failed(&mut self, id: u32, now: Instant) {
        self.corner.strays
            .entry(id)
            .and_modify(|s| s.next = Some(now + park_reconcile::backoff(s.tries)))
            .or_insert_with(|| Stray::park_failed(now));
        tracing::info!(id, "park failed: the window is parked again by the tick, with a backoff");
    }

    /// Every tick: a window the model has parked on the shown Desktop that the window server
    /// lists outside the park zone, or whose park failed, is parked again or adopted as shown
    /// ([`crate::engine::corner::park_reconcile`]; the review's L1, K2, S5: until now only a
    /// Space arrival's `match_screen` reconciled the two, and such a window sat mid-screen,
    /// counted parked, with no card, until the next Desktop change). A window the user
    /// focused since its park ([`crate::engine::causes::Causes::focused_since_hidden`]), or moved while parked, joins the active
    /// stage where it is ([`Model::adopt_shown`]); any other is parked again once it rests
    /// (nothing is judged while our own park, raise or un-minimize may still be landing, nor
    /// while its listed frame changes: live 10-29, ⌘M's un-minimize focused the window and
    /// set it down out of the zone, and "focused" adopted it out of its new card),
    /// echo-touched through the batch, with a backoff, and given up after a few tries (said
    /// once at info). `list`: the tick's read of the on-screen list, when it made one.
    ///
    /// Not while the list cannot be trusted: a Space change settling or a full-screen Space,
    /// Mission Control and its settle, a Desktop slide before its notice (the parked windows
    /// listed shifted sideways: the owner's log of 10-27 lists every parked window at x=3,
    /// 1224 and 2045 during slides), macOS's own reveal, a transition's windows behind their
    /// snapshots, an arrangement made ahead being checked, a held activation, a drag or the
    /// button down (a sliver grabbed: [`Self::focus_after_press_done`] takes it).
    pub(in crate::platform::controller) fn reconcile_parked(&mut self, ob: &mut Observer, list: Option<&[windows::CgWindow]>) {
        use crate::engine::corner::corner_patch::desktop_slides;
        use crate::engine::corner::park_reconcile::{Facts, Seen, Verdict, step};
        let now = clock::now();
        let Some(m) = self.model.as_ref() else {
            self.corner.strays.clear();
            return;
        };
        let displays = m.displays().to_vec();
        // Every screen's shown Desktop (round displays-2): a parked window is judged against
        // its own screen's park zone.
        let shown_spaces: Vec<SpaceId> = displays.iter().map(|d| m.current_space(d.id)).collect();
        let model_parked: Vec<(u32, i32)> = m
            .windows()
            .filter(|w| w.parked && shown_spaces.contains(&w.space) && w.home != Home::Pinned)
            .map(|w| (w.meta.id.0, w.meta.pid))
            .collect();
        // Shown, closed, or moved on: no stray any more.
        self.corner.strays.retain(|id, _| model_parked.iter().any(|(i, _)| i == id));
        if model_parked.is_empty()
            || self.frozen()
            || self.overview.freezes(now)
            // Frames are judged: the slide's may still be listed after an arrival the
            // window server's listing named, which has no settle of its own.
            || self.phase.frames_settling(now)
            || self.macos_reveal_at.is_some()
            || self.phase.switching().is_some()
            || self.ahead.is_some()
            || self.verify.is_some()
            || self.slide_held()
            || self.drag.is_some()
            || self.focus_after_press.is_some()
            || input::left_button_down()
        {
            return;
        }
        let read;
        let list = match list {
            Some(l) => l,
            None => {
                read = windows::wake_on_screen();
                read.as_slice()
            }
        };
        let parked_here: HashSet<u32> = model_parked.iter().map(|(id, _)| *id).collect();
        for d in &displays {
            let here = m.current_space(d.id);
            let on_d = |id: u32| m.window(WindowId(id)).is_some_and(|x| x.space == here);
            let listed_parked: Vec<Rect> = list.iter().filter(|w| w.layer == 0 && parked_here.contains(&w.id) && on_d(w.id)).map(|w| w.bounds).collect();
            let elsewhere = list.iter().any(|w| {
                w.layer == 0 && w.onscreen && m.window(WindowId(w.id)).is_some_and(|x| x.parked && x.space != here && m.display_of_space(x.space) == d.id)
            });
            let parked_on_d = parked_here.iter().filter(|id| on_d(**id)).count();
            if desktop_slides(parked_on_d, &listed_parked, elsewhere, d.frame, d.visible, d.corner) {
                return;
            }
        }
        let zone_of = |id: u32| m.window(WindowId(id)).map(|x| *m.display(x.meta.display));
        // The focused window is asked (two Accessibility messages) only once a stray shows.
        let mut focused: Option<Option<u32>> = None;
        let mut displays: Option<Vec<Rect>> = None;
        let mut verdicts: Vec<(u32, Verdict, Rect)> = Vec::new();
        // A screen came back (the lid opened): a parked window macOS listed in its park zone
        // is that screen's card, taken by the display reconcile before anything parks it
        // again elsewhere (11-05 20:26:05: the laptop's two cards, counted on the 5K, were
        // parked again at the 5K's corner and stayed its cards); one it listed at a normal
        // frame on the screen that came back shows there, that screen's front (11-06
        // 12:00:51: Chrome, a card on the 5K since a switch there, was back at its laptop
        // frame and parked again at the 5K's corner) ([`Model::returned_display`]).
        let settling_displays = self.displays_settling();
        let added: &[DisplayId] = if settling_displays { &self.displays_added } else { &[] };
        let mut returned: Vec<(WindowId, DisplayId, Rect)> = Vec::new();
        for (id, pid) in model_parked {
            let listed = list.iter().find(|w| w.id == id && w.layer == 0);
            if settling_displays
                && let Some(w) = listed
                && let Some(to) = m.window(WindowId(id)).and_then(|x| m.returned_display(w.bounds, x.meta.display, added))
            {
                returned.push((WindowId(id), to, w.bounds));
                self.corner.strays.remove(&id);
                continue;
            }
            let seen = match listed {
                None => Seen::NotListed,
                Some(w) if zone_of(id).is_some_and(|d| self.engine.is_hidden(&d, w.bounds)) => Seen::InZone,
                Some(_) => Seen::OutOfZone,
            };
            if seen != Seen::OutOfZone {
                // Nothing to settle unless it is known: a parked window off screen (its app
                // hidden) or in the zone is as it should be.
                if let Some(s) = self.corner.strays.get_mut(&id)
                    && step(s, Facts { seen, ..Facts::out_at(Rect::default()) }, now) == Verdict::Settled
                {
                    tracing::debug!(id, "a stray window is listed in the park zone: settled");
                    self.corner.strays.remove(&id);
                }
                continue;
            }
            let bounds = listed.map(|w| w.bounds).unwrap_or_default();
            if off_every_display(bounds, displays.get_or_insert_with(desktop::cg_display_bounds)) {
                // Moved off every display by its app: parked again, a later restore would put
                // it on the display. The off-screen check lets it go where it is
                // ([`Self::leave_off_screen`]).
                self.corner.strays.remove(&id);
                continue;
            }
            // The user's choice only: focused now, and by a focus of theirs since our park
            // (macOS's focus at an un-minimize, or our own, counts for nothing).
            let is_focused = *focused.get_or_insert_with(focused_window) == Some(id);
            let user_focused = is_focused && self.causes.focused_since_hidden(id);
            let facts = Facts {
                seen,
                bounds,
                focused: user_focused,
                user_moved: false,
                busy: self.busy_apps.borrow().busy(pid, now),
                echo: self.causes.wrote_recently(id, now),
            };
            let s = self.corner.strays.entry(id).or_default();
            let verdict = step(s, facts, now);
            if !matches!(verdict, Verdict::Wait | Verdict::Settled) {
                verdicts.push((id, verdict, bounds));
            }
        }
        if !returned.is_empty() {
            let z: Vec<WindowId> = list.iter().filter(|w| w.onscreen && w.layer == 0).map(|w| WindowId(w.id)).collect();
            self.take_display_moves(ob, &returned, &z);
        }
        for (id, verdict, bounds) in verdicts {
            let listed = format!("{:.0},{:.0} {:.0}x{:.0}", bounds.x, bounds.y, bounds.w, bounds.h);
            let stray = self.corner.strays.get(&id).copied().unwrap_or_default();
            match verdict {
                Verdict::Adopt => {
                    let obs = Observation { focused: focused.flatten().map(WindowId), ..Observation::default() };
                    let Some(m) = self.model.as_mut() else { return };
                    let actions = m.adopt_shown(WindowId(id), bounds, &obs);
                    tracing::info!(id, listed, user_moved = stray.user_moved, tries = stray.tries, "a parked window the user has out of the corner joins the active stage where it is");
                    self.corner.strays.remove(&id);
                    self.execute(ob, actions);
                }
                Verdict::Park => {
                    if !self.model.as_ref().is_some_and(|m| m.window(WindowId(id)).is_some_and(|w| w.parked)) {
                        continue;
                    }
                    tracing::info!(id, listed, tries = stray.tries, "a parked window is listed out of the park zone: parked again");
                    self.execute(ob, vec![Action::Hide { id: WindowId(id) }]);
                }
                Verdict::GiveUp => tracing::info!(
                    id,
                    listed,
                    tries = stray.tries,
                    "a parked window keeps leaving the park zone: given up on; it stays counted parked until its focus, a switch or a Desktop change settles it"
                ),
                Verdict::Wait | Verdict::Settled => {}
            }
        }
    }
}
