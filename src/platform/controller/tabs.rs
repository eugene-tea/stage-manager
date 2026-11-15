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
//! [`Model::tab_selected`]: crate::engine::model::Model::tab_selected

use std::time::{Duration, Instant};

use super::Controller;
use crate::engine::busy_apps::Reply;
use crate::engine::hidden_tab::{self, TabListed};
use crate::engine::model::{WindowId, WindowRole};
use crate::platform::hidden_tabs::tab_listed;
use crate::platform::observe::Observer;
use crate::platform::windows::{self, CgWindow};

/// A tab selected that its app's window list did not name is asked about again only after
/// this long (an app that does not answer is not asked at every check).
pub(super) const ASK_AGAIN: Duration = Duration::from_secs(3);

impl Controller {
    /// The tabs selected in `list` (every window, on screen first): each managed window the
    /// model shows on a Desktop shown, listed off screen, whose place a window of its app not
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
        // Parked windows (hidden by the engine) are not looked at: a stage shown only.
        let shown: Vec<u32> = m
            .windows()
            .filter(|w| w.meta.role == WindowRole::Standard && !w.parked && m.is_shown_space(w.space))
            .map(|w| w.meta.id.0)
            .collect();
        if shown.is_empty() {
            return false;
        }
        let listed: Vec<TabListed> = list.iter().map(tab_listed).collect();
        let found = hidden_tab::tabs_selected(&listed, &shown, |id| m.window(WindowId(id)).is_some());
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
            let (actions, gone) = m.tab_selected(WindowId(t.left), meta);
            if actions.is_empty() {
                continue;
            }
            tracing::info!(left = t.left, selected = t.selected, ?stage, "a window tab was selected: it takes the place of the tab it hides in its stage");
            self.unplaced.remove(&t.selected);
            self.fresh_unplaced.remove(&t.selected);
            self.tab_asked.remove(&t.selected);
            self.absent.remove(&t.left);
            self.execute(ob, actions);
            self.unplaced.extend(gone.into_iter().map(|w| w.0).filter(|w| ob.window(*w).is_some()));
            took = true;
        }
        took
    }
}
