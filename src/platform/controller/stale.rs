//! Windows whose Accessibility handle died, on the WM thread (round stale-handles,
//! [`crate::engine::stale_handle`]).
//!
//! Every move the engine asks for (a park, a restore, a fit, an arrival's move, a raise, a
//! focus) goes through [`Controller::write_window`]: a write answered
//! `kAXErrorInvalidUIElement` looks the window up afresh by its id in its app's `AXWindows`;
//! listed, the write is tried once more with the fresh handle, which the observer keeps
//! ([`Observer::note_fresh`]); judged closed, it is let go as a destroyed notification would
//! have, once a second look agrees ([`Observer::note_closed`]). The once-a-second Space check
//! looks at the managed windows of the Desktop shown that are off screen
//! ([`Controller::check_stale_handles`]): a card cannot wait for a move to find its window
//! closed.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use super::Controller;
use crate::engine::hidden_tab::{self, Candidate, TabListed};
use crate::engine::model::WindowId;
use crate::engine::stale_handle::{self as sh, Facts, Judgement, Keep, Lookup, Server};
use crate::platform::{apps, desktop};
use crate::platform::ax::{AxElement, AxError, AxResult};
use crate::platform::observe::Observer;
use crate::platform::windows::{self, CgWindow};

/// The controller's share of the dead handles: when each window's app was last asked, the
/// windows judged closed once, and why each kept one was kept (logged when that changes).
#[derive(Debug, Default)]
pub(super) struct StaleHandles {
    looks: sh::Looks,
    sightings: sh::Sightings,
    kept: HashMap<u32, Keep>,
}

impl StaleHandles {
    /// Windows judged closed once, waiting for a second look (for `ctl state`).
    pub(super) fn waiting(&self) -> Vec<u32> {
        self.sightings.waiting()
    }
}

/// Window `id` in app `pid`'s `AXWindows`: the fresh handle when listed.
fn look_up(pid: i32, id: u32) -> (Lookup, Option<AxElement>) {
    let list = match AxElement::application(pid).windows() {
        Ok(list) => list,
        // An app with no window at all answers "no value".
        Err(e) if e == AxError::NO_VALUE => return (Lookup::NotListed, None),
        Err(e) => {
            tracing::debug!(id, pid, "a dead handle's lookup: its app does not answer ({e})");
            return (Lookup::NoAnswer, None);
        }
    };
    let mut unreadable = false;
    for element in list {
        match element.window_id() {
            Ok(i) if i == id => return (Lookup::Listed, Some(element)),
            Ok(_) => {}
            Err(_) => unreadable = true,
        }
    }
    // A window whose id cannot be read might be this one.
    if unreadable { (Lookup::NoAnswer, None) } else { (Lookup::NotListed, None) }
}

/// What the window server says of window `id` in `list` (every window, on every Desktop).
fn server_of(list: &[CgWindow], id: u32) -> Server {
    match list.iter().find(|w| w.id == id) {
        Some(w) if w.onscreen => Server::OnScreen,
        Some(_) => Server::OffScreen,
        None => Server::Unlisted,
    }
}

/// Whether window `id` in `list` may be a hidden tab: off screen beside a same-size window of
/// its app ([`hidden_tab::may_be_hidden_tab`], judged as a window reported destroyed while
/// listed, the tab's signature, with no Accessibility vouching asked: kept on doubt).
fn maybe_tab(list: &[CgWindow], id: u32) -> bool {
    let tab = |w: &CgWindow| TabListed { id: w.id, pid: w.pid, layer: w.layer, onscreen: w.onscreen, bounds: w.bounds };
    let listed: Vec<TabListed> = list.iter().map(tab).collect();
    list.iter().find(|w| w.id == id).is_some_and(|w| hidden_tab::may_be_hidden_tab(&tab(w), &listed, Candidate::Destroyed))
}

impl Controller {
    /// One write of ours to window `id` of app `pid` through `element` (the observer's,
    /// [`Observer::element`]): when its handle is dead (`kAXErrorInvalidUIElement`), the
    /// window is looked up afresh ([`Self::look_up_dead`]) and, listed, `write` tried once
    /// more with the fresh handle. A window already judged closed is not written to.
    pub(super) fn write_window(
        &mut self,
        ob: &Observer,
        id: u32,
        pid: i32,
        element: &AxElement,
        write: impl Fn(&AxElement) -> AxResult<()>,
    ) -> AxResult<()> {
        if ob.noted_closed(id) {
            return Err(AxError::INVALID_ELEMENT);
        }
        let first = if ob.poisoned(id) { Err(AxError::INVALID_ELEMENT) } else { write(element) };
        if first != Err(AxError::INVALID_ELEMENT) {
            return first;
        }
        match self.look_up_dead(ob, id, pid, None) {
            Some(fresh) => {
                let again = write(&fresh);
                match &again {
                    Ok(()) => tracing::info!(id, pid, "a dead handle's window found again: the move went through with its new handle"),
                    Err(e) => tracing::info!(id, pid, "a dead handle's window found again: the move with its new handle failed too ({e})"),
                }
                again
            }
            None => first,
        }
    }

    /// Window `id` of app `pid`, whose handle answered `kAXErrorInvalidUIElement`, looked up
    /// afresh and judged ([`sh::judge`]); at most once per [`sh::LOOK_AGAIN`] per window
    /// ([`sh::Looks`]).
    /// The fresh handle when its app lists it (the observer keeps it); a window judged closed
    /// on two looks apart is let go ([`Observer::note_closed`]). `list`: the window server's
    /// complete list when the caller has one.
    fn look_up_dead(&mut self, ob: &Observer, id: u32, pid: i32, list: Option<&[CgWindow]>) -> Option<AxElement> {
        let now = Instant::now();
        if !self.stale.looks.may_look(id, now) {
            return None;
        }
        let (lookup, fresh) = look_up(pid, id);
        if lookup == Lookup::NoAnswer {
            ob.suspect(pid);
        }
        let facts = match lookup {
            Lookup::NotListed => {
                let read;
                let list = match list {
                    Some(l) => l,
                    None => {
                        read = windows::list_windows();
                        &read
                    }
                };
                let model = self.model.as_ref();
                // A window the model does not place is left to the paths that place it.
                let other_space = model.and_then(|m| m.window(WindowId(id)).map(|w| (m, w))).is_none_or(|(m, w)| {
                    !w.sticky && !m.is_shown_space(w.space)
                });
                // A full-screen window is on a Space of its own: the window server's bounds
                // are the only public evidence (no public API names a window's Space).
                let full_screen = list.iter().find(|w| w.id == id).is_some_and(|w| {
                    !w.onscreen && sh::on_full_screen_space(w.bounds, &desktop::cg_display_bounds())
                });
                Facts {
                    lookup,
                    server: server_of(list, id),
                    other_space,
                    full_screen,
                    app_hidden: self.counts_hidden(pid) || apps::is_hidden(pid) != Some(false),
                    maybe_tab: maybe_tab(list, id),
                }
            }
            _ => Facts { lookup, server: Server::Unlisted, other_space: false, full_screen: false, app_hidden: false, maybe_tab: false },
        };
        match sh::judge(facts) {
            Judgement::Refresh => {
                self.stale.sightings.clear(id);
                self.stale.kept.remove(&id);
                let fresh = fresh?;
                tracing::info!(id, pid, "a window whose handle died is found again in its app's list: its new handle is kept");
                ob.note_fresh(id, fresh.clone());
                Some(fresh)
            }
            Judgement::Keep(why) => {
                self.stale.sightings.clear(id);
                if self.stale.kept.insert(id, why) != Some(why) {
                    tracing::info!(id, pid, server = ?facts.server, "a window whose handle died is kept: {}", why.words());
                }
                None
            }
            Judgement::Gone => {
                self.stale.kept.remove(&id);
                if self.stale.sightings.closed(id, now) {
                    ob.note_closed(id);
                    self.stale.looks.retain(|i| i != id);
                } else {
                    tracing::debug!(id, pid, server = ?facts.server, "a window whose handle died looks closed: a second look decides");
                }
                None
            }
        }
    }

    /// The once-a-second look (from the Space check, never while a Space change settles, an
    /// overview shows or on a full-screen Space): each managed window of the Desktop shown
    /// that the window server does not show (`list`: every window, every Desktop) and whose
    /// handle is dead is looked up afresh and judged, so a closed window's card goes even
    /// when nothing moves it. Not a window we just moved, nor one of a hidden or busy app
    /// (asked nothing: its windows are off screen for that reason).
    pub(super) fn check_stale_handles(&mut self, ob: &Observer, list: &[CgWindow]) {
        let Some(m) = &self.model else { return };
        let now = Instant::now();
        let onscreen: HashSet<u32> = list.iter().filter(|w| w.onscreen).map(|w| w.id).collect();
        let candidates: Vec<(u32, i32)> = m
            .windows()
            .filter(|w| (w.sticky || m.is_shown_space(w.space)) && !onscreen.contains(&w.meta.id.0))
            .filter_map(|w| ob.window(w.meta.id.0).map(|t| (t.id, t.pid)))
            .filter(|&(id, _)| !self.echoes.is_window_echo(id, now))
            .collect();
        let ids: HashSet<u32> = candidates.iter().map(|&(id, _)| id).collect();
        self.stale.sightings.retain(|id| ids.contains(&id));
        self.stale.kept.retain(|id, _| m.window(WindowId(*id)).is_some());
        self.stale.looks.prune(now);
        for (id, pid) in candidates {
            let hidden = self.counts_hidden(pid) || apps::is_hidden(pid) == Some(true);
            if hidden || self.busy_apps.borrow().busy(pid, now) || ob.noted_closed(id) {
                continue;
            }
            if ob.handle_dead(id) {
                self.look_up_dead(ob, id, pid, Some(list));
            } else {
                self.stale.sightings.clear(id);
            }
        }
    }
}
