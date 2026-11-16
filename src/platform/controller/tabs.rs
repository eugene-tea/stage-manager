//! Window tabs selected, on the WM thread (round tab-lost): the tab selected takes the place
//! of the tab it hides in its stage ([`hidden_tab::tabs_selected`], [`Model::tab_selected`]).
//!
//! Selecting a tab posts no Accessibility notification (Terminal on 11-15: five tab switches,
//! no notice for any; a tab Accessibility had not listed yet was found only at the app's
//! next activation, 104 s later). What tells it is the window server's list: the tab left
//! off screen at its frame, the tab selected on screen at that same frame. The once-a-second
//! Space check reads that list anyway, and a click or a ⌘ key (how a tab is selected) stirs
//! the tick, so the check runs within a brisk period of the switch: nothing is polled for
//! tabs. A card click looks first too, so a switch right after a tab switch takes the tab
//! selected with its stage. Before this, the tab left was let go as "left this Space"
//! (no stage), and the tab selected again came back only through the windows waiting
//! (`unplaced`), as a new window, if at all: the owner's 11-15 Terminal tab stayed on
//! screen under the stage switched to.
//!
//! A hidden stage's tab group counts too (round tab-quit): its tab selected at the park spot
//! is hidden with the stage, at the group's frame to come back to. The quit looks once
//! before its unpark ([`Controller::unpark_all`]): a tab selected a moment before is put
//! back at its group's frame, not left in the corner for the leftovers check.
//!
//! Under the Spaces engine a hidden window is listed off screen at its own frame, the tab
//! left's look: a new window opened at a hidden window's frame was taken for its tab selected
//! (the review of 1.9, C-1). There the tab left must also be gone from its app's
//! Accessibility list ([`hidden_tab::tabs_selected`]).
//!
//! [`Model::tab_selected`]: crate::engine::model::Model::tab_selected

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::Controller;
use crate::platform::window_engine::EngineKind;
use crate::engine::busy_apps::Reply;
use crate::engine::hidden_tab::{self, TabListed};
use crate::engine::model::{WindowId, WindowRole};
use crate::platform::hidden_tabs::{ax_window_ids, tab_listed};
use crate::platform::observe::Observer;
use crate::platform::windows::{self, CgWindow};

/// A tab selected that its app's window list did not name is asked about again only after
/// this long (an app that does not answer is not asked at every check).
pub(super) const ASK_AGAIN: Duration = Duration::from_secs(3);

impl Controller {
    /// The tabs selected in `list` (every window, on screen first): each managed window of a
    /// Desktop shown (shown, or hidden with its stage), listed off screen, whose place a window of its app not
    /// managed takes on screen, hands its stage to that window, which the app's window list
    /// is read again for when Accessibility does not track it yet; the tab no longer
    /// selected waits with the windows not placed (`unplaced`), and takes the place back the
    /// same way when it is selected again. Not while a switch's windows wait behind their
    /// pictures (their stage Space is off screen) or a switch runs. True when a tab was taken.
    pub(super) fn take_tabs(&mut self, ob: &mut Observer, list: &[CgWindow]) -> bool {
        if self.pending.is_some() || self.phase.switching().is_some() {
            return false;
        }
        let Some(m) = self.model.as_ref() else { return false };
        // Hidden windows too (round tab-quit): a group parked in the corner shows its tab
        // selected there.
        let held: Vec<u32> = m
            .windows()
            .filter(|w| w.meta.role == WindowRole::Standard && m.is_shown_space(w.space))
            .map(|w| w.meta.id.0)
            .collect();
        if held.is_empty() {
            return false;
        }
        let listed: Vec<TabListed> = list.iter().map(tab_listed).collect();
        // A window hidden off screen at its own frame (the Spaces engine) looks like a tab
        // left: its app's list must no longer name it (the review of 1.9, C-1), read only for
        // a window whose place a window of its app takes, once per app.
        let mut ax: HashMap<i32, Option<Vec<u32>>> = HashMap::new();
        let still_listed = |left: &TabListed| {
            if !m.hidden_at_own_frame(WindowId(left.id)) {
                return false;
            }
            let ids = ax.entry(left.pid).or_insert_with(|| ax_window_ids(left.pid));
            let still = ids.as_ref().is_none_or(|ids| ids.contains(&left.id));
            if still {
                tracing::debug!(id = left.id, pid = left.pid, "a window of its app at a hidden window's frame, but the app still lists the hidden one: a new window, no tab selected");
            }
            still
        };
        let found = hidden_tab::tabs_selected(&listed, &held, |id| m.window(WindowId(id)).is_some(), still_listed);
        if found.is_empty() {
            return false;
        }
        let now = Instant::now();
        self.tab_asked.retain(|_, at| now.duration_since(*at) < ASK_AGAIN);
        let z = windows::z_order(list);
        let mut took = false;
        for t in found {
            // A window minimized is off screen for that reason, whatever shows where it was.
            if ob.minimized_reply(t.left) == Reply::Yes {
                continue;
            }
            if ob.window(t.selected).is_none() {
                if self.tab_asked.contains_key(&t.selected) {
                    continue;
                }
                if let Some(pid) = list.iter().find(|w| w.id == t.selected).map(|w| w.pid) {
                    ob.rescan_app_now(pid);
                }
                if ob.window(t.selected).is_none() {
                    tracing::debug!(left = t.left, selected = t.selected, "a window tab selected that its app does not list yet: asked again later");
                    self.tab_asked.insert(t.selected, now);
                    continue;
                }
            }
            if self.unplaced.contains(&t.selected) {
                // Read off screen before: its kind is read again, as for any window that waited.
                ob.recheck_kind(t.selected);
            }
            let Some(mut meta) = self.meta(ob, t.selected, &z) else { continue };
            // The tab selected takes its group's frame: the list's, not the frame it was found at.
            if let Some(w) = list.iter().find(|w| w.id == t.selected) {
                meta.frame = w.bounds;
            }
            if let Some(title) = ob.window(t.selected).and_then(|w| w.element.title().ok()) {
                meta.title = title;
            }
            let Some(m) = self.model.as_mut() else { return took };
            let stage = m.stage_of(WindowId(t.left)).map(|s| s.0);
            let hidden = m.window(WindowId(t.left)).is_some_and(|w| w.parked);
            let (actions, gone) = m.tab_selected(WindowId(t.left), meta);
            if actions.is_empty() {
                continue;
            }
            tracing::info!(left = t.left, selected = t.selected, ?stage, hidden, "a window tab was selected: it takes the place of the tab it hides in its stage");
            self.unplaced.remove(&t.selected);
            self.fresh_unplaced.remove(&t.selected);
            self.tab_asked.remove(&t.selected);
            self.absent.remove(&t.left);
            self.execute(ob, actions);
            if hidden {
                self.release_hidden_tab(t.left);
            }
            self.unplaced.extend(gone.into_iter().map(|w| w.0).filter(|w| ob.window(*w).is_some()));
            took = true;
        }
        took
    }

    /// The tab left by a tab selected in a hidden stage (round tab-quit), out of the model now.
    /// The Corner engine: nothing; it stays at the park spot, ordered out, and moving a hidden
    /// tab is never asked (its group moves with the tab selected; the leftovers check judges it
    /// a hidden tab beside it). The Spaces engine: let out of the stage Space where it is, as
    /// any window the model no longer counts, so it shows on its Desktop once selected again.
    fn release_hidden_tab(&mut self, left: u32) {
        match self.engine.kind() {
            EngineKind::Corner => {}
            EngineKind::Spaces => {
                crate::platform::spaces::engine::shared().release(&[left], "a hidden tab: the tab selected took its place");
            }
        }
    }
}
