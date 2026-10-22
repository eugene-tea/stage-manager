//! Hidden tabs on the window server's list (8J): the platform side of
//! [`crate::engine::hidden_tab`], beside the park-zone checks. The quit's still-parked check
//! ([`super::safety::stranded_allowed`]), the controller's retry of windows whose restore
//! failed, and the guardian's watch ask it before counting a window in the park zone as one
//! to bring back: a hidden tab cannot be moved and needs not be, so none of them ever waits
//! for one. The recovery scan reads windows through Accessibility, which never lists a
//! hidden tab, so it never meets one.
//!
//! The same look also tells another app's off-screen window in the zone we never parked
//! (round 8M, [`crate::engine::not_ours`]: Chrome's helpers, Terminal's hidden tabs whose
//! selected tab is on another Desktop): the quit's check and the guardian leave it alone too.

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use super::ax::{AxElement, AxError};
use super::windows::CgWindow;
use crate::engine::hidden_tab::{self, Candidate, TabListed};
use crate::engine::not_ours;

/// One look at the window server's list: each app's Accessibility answer is asked once, and
/// only for a window that may be a hidden tab ([`hidden_tab::may_be_hidden_tab`]).
pub struct Tabs {
    listed: Vec<TabListed>,
    ax: HashMap<i32, Option<Vec<u32>>>,
}

impl Tabs {
    /// A look at `list` (the window server's windows on every Space).
    pub fn new(list: &[CgWindow]) -> Self {
        Self { listed: list.iter().map(tab_listed).collect(), ax: HashMap::new() }
    }

    /// True when `w` (one of the list's) is a hidden tab
    /// ([`hidden_tab::is_hidden_tab`]); `shown_managed(id)`: the model manages window `id`
    /// and has not parked it (vouches for a [`Candidate::Destroyed`] window only). Logged
    /// once per window, at debug.
    pub fn is_hidden_tab(&mut self, w: &CgWindow, candidate: Candidate, shown_managed: impl Fn(u32) -> bool) -> bool {
        let t = tab_listed(w);
        if !hidden_tab::may_be_hidden_tab(&t, &self.listed, candidate) {
            return false;
        }
        let ax = self.ax.entry(w.pid).or_insert_with(|| ax_window_ids(w.pid));
        let tab = hidden_tab::is_hidden_tab(&t, &self.listed, candidate, ax.as_deref(), shown_managed);
        if tab {
            log_once(w);
        }
        tab
    }

    /// True when `w` (one of the list's, found in the park zone) is another app's off-screen
    /// window we never parked ([`not_ours::not_ours`]); `saved`: this session's `state.json`
    /// names it. Its app is asked through Accessibility only when nothing else rules it out.
    /// Logged once per window, at debug.
    pub fn is_not_ours(&mut self, w: &CgWindow, candidate: Candidate, saved: bool) -> bool {
        let t = tab_listed(w);
        if !not_ours::not_ours(&t, candidate, saved, Some(&[])) {
            return false;
        }
        let ax = self.ax.entry(w.pid).or_insert_with(|| ax_window_ids(w.pid));
        let foreign = not_ours::not_ours(&t, candidate, saved, ax.as_deref());
        if foreign {
            log_not_ours_once(w);
        }
        foreign
    }
}

fn tab_listed(w: &CgWindow) -> TabListed {
    TabListed { id: w.id, pid: w.pid, layer: w.layer, onscreen: w.onscreen, bounds: w.bounds }
}

/// The window ids app `pid` lists through Accessibility (an app with no window: none);
/// `None` when it does not answer, or a window's id cannot be read (a window it lists might
/// then be taken for one it does not).
fn ax_window_ids(pid: i32) -> Option<Vec<u32>> {
    match AxElement::application(pid).windows() {
        Ok(list) => list.iter().map(|w| w.window_id().ok()).collect(),
        Err(e) if e == AxError::NO_VALUE => Some(Vec::new()),
        Err(e) => {
            tracing::debug!(pid, "hidden tabs: the app does not answer ({e}); none of its windows counts as one");
            None
        }
    }
}

/// Windows already logged as hidden tabs (this process).
static LOGGED: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

/// Windows already logged as not ours (this process).
static LOGGED_NOT_OURS: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

fn log_not_ours_once(w: &CgWindow) {
    if LOGGED_NOT_OURS.lock().is_ok_and(|mut l| l.insert(w.id)) {
        let bundle = super::safety::bundle_id_of(w.pid).unwrap_or_else(|| "?".into());
        tracing::debug!(id = w.id, pid = w.pid, bundle, at = %w.bounds, "another app's off-screen window in the zone, not ours: left alone");
    }
}

fn log_once(w: &CgWindow) {
    if LOGGED.lock().is_ok_and(|mut l| l.insert(w.id)) {
        let bundle = super::safety::bundle_id_of(w.pid).unwrap_or_else(|| "?".into());
        tracing::debug!(id = w.id, pid = w.pid, at = %w.bounds, "a hidden tab of {bundle} left alone");
    }
}
