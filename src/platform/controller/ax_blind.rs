//! Accessibility blind for the session ([`crate::engine::ax_blind`] has the line): the facts
//! fed about once a second, the menu's flag ([`session::set_accessibility_blind`]), one log
//! line at its start and end, and the restore run again once windows are listed.
//!
//! While the app sees nothing ([`Watch::sees_nothing`]) the apps are read again every
//! [`RESCAN`] ([`Observer::rescan_all`]: a window Accessibility lists again posts no notice),
//! the restore window's own ends wait, and no saved window is given up (no app's quiet minute
//! runs; the entries of apps not running wait too). While the window server shows windows
//! Accessibility has not listed (none listed yet in this run), a click or a key press leaves
//! the restore window open
//! ([`Controller::user_may_end_restore`]).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Controller, unix_now};
use crate::engine::ax_blind::{Change, Facts, Watch};
use crate::engine::model::WindowId;
use crate::engine::restore::Settling;
use crate::platform::observe::Observer;
use crate::platform::{apps, session, windows};

/// How often the facts are looked at.
const LOOK: Duration = Duration::from_secs(1);

/// How often every app is read again while Accessibility lists nothing.
const RESCAN: Duration = Duration::from_secs(2);

/// The platform's side of the watch.
#[derive(Debug, Default)]
pub(super) struct AxWatch {
    /// The verdict.
    watch: Watch,
    /// The last look.
    looked: Option<Instant>,
    /// The last rescan of every app.
    rescanned: Option<Instant>,
    /// The login (read once).
    login: Option<Option<Instant>>,
    /// The real windows on screen at the last silent look: there all along, once found.
    shown: Vec<u32>,
}

impl Controller {
    /// About once a second (the tick): the facts, the verdict, what changes with it.
    pub(super) fn watch_accessibility(&mut self, ob: &mut Observer, now: Instant) {
        if self.ax.looked.is_some_and(|t| now.duration_since(t) < LOOK) {
            return;
        }
        self.ax.looked = Some(now);
        let was_silent = self.ax.watch.sees_nothing();
        if was_silent
            && self.model.is_some()
            && !self.locked
            && ob.windows().is_empty()
            && self.ax.rescanned.is_none_or(|t| now.duration_since(t) >= RESCAN)
        {
            self.ax.rescanned = Some(now);
            let found = ob.rescan_all();
            self.unplaced.extend(found);
        }
        let facts = self.ax_facts(ob, now);
        let change = self.ax.watch.feed(&facts, now);
        self.ax_changed(ob, change, now);
        if was_silent
            && !self.ax.watch.sees_nothing()
            && let Some(clock) = self.settling.as_mut()
        {
            // The restore window's quiet counts from when windows can be seen.
            clock.busy(now);
        }
    }

    /// A window was found (the observer's notice, or one waiting that shows): Accessibility
    /// lists windows. Ends a blindness before the window is placed, so it goes back to its
    /// saved stage in the restore window opened again.
    pub(super) fn ax_window_found(&mut self, ob: &Observer, now: Instant) {
        let change = self.ax.watch.listed(now);
        self.ax_changed(ob, change, now);
    }

    /// True when a click, a key press, a card click, a hotkey or a ctl command may end the
    /// restore window ([`Watch::user_ends_restore`]; a window the observer lists now counts
    /// before the next look does).
    pub(super) fn user_may_end_restore(&self, ob: &Observer) -> bool {
        !ob.windows().is_empty() || self.ax.watch.user_ends_restore()
    }

    /// True when a save may say this session looked for its Desktops' windows
    /// ([`Watch::looked`]).
    pub(super) fn ax_looked(&self) -> bool {
        self.ax.watch.looked()
    }

    /// True while the app sees nothing to restore into ([`Watch::sees_nothing`]).
    pub(super) fn ax_sees_nothing(&self) -> bool {
        self.ax.watch.sees_nothing()
    }

    /// For `ctl state`: `accessibility_blind` and since when (Unix seconds).
    pub(super) fn ax_blind_json(&self) -> (Value, Value) {
        let since = self.ax.watch.since().map(|t| unix_now().saturating_sub(t.elapsed().as_secs()));
        (json!(self.ax.watch.blind()), json!(since))
    }

    fn ax_facts(&mut self, ob: &Observer, now: Instant) -> Facts {
        let login = *self.ax.login.get_or_insert_with(apps::login_started);
        let mut facts = Facts {
            trusted: ob.accessibility(),
            locked: self.locked,
            overview: self.overview.showing().is_some() || self.phase.fullscreen(),
            since_login: login.map(|l| now.saturating_duration_since(l)),
            listed: ob.windows().len(),
            ..Facts::default()
        };
        if facts.listed > 0 || !facts.trusted {
            return facts;
        }
        let list = windows::on_screen();
        facts.regular = session::real_windows(&list, |w| ob.wants(w.pid)).count();
        let real: Vec<&windows::CgWindow> = session::real_windows(&list, |w| ob.observes(w.pid)).collect();
        facts.shown = real.len();
        facts.answering = real.iter().map(|w| w.pid).filter(|p| ob.answers(*p)).collect::<HashSet<_>>().len();
        self.ax.shown = real.iter().map(|w| w.id).collect();
        facts
    }

    fn ax_changed(&mut self, ob: &Observer, change: Option<Change>, now: Instant) {
        let (pending, unvisited) = self.model.as_ref().map_or((0, 0), |m| (m.pending_count(), m.pending_unvisited()));
        match change {
            None => {}
            Some(Change::Began { held }) => {
                session::set_accessibility_blind(true);
                tracing::info!(
                    held_s = held.as_secs(),
                    on_screen = self.ax.shown.len(),
                    pending,
                    pending_unvisited = unvisited,
                    "Accessibility lists no window of any app while the window server shows them (macOS's own fault; a logout fixes it): the menu says so; saved stages are kept"
                );
            }
            Some(Change::Ended { lasted, recovered }) => {
                session::set_accessibility_blind(false);
                if recovered {
                    tracing::info!(lasted_s = lasted.as_secs(), listed = ob.windows().len(), pending, pending_unvisited = unvisited, "Accessibility lists windows again");
                    self.restore_after_blindness(now);
                } else {
                    tracing::info!(lasted_s = lasted.as_secs(), "Accessibility is no longer trusted: no window can be seen (the menu asks for it)");
                }
            }
        }
    }

    /// Windows listed again after blindness: the restore runs as at a launch
    /// ([`crate::engine::model::Model::reopen_restore`]), its clocks from now, its quiet ending
    /// no sooner than a minute from now; every app's quiet minute starts again.
    fn restore_after_blindness(&mut self, now: Instant) {
        self.absent_due = None;
        self.quiet.restart_all(now);
        let shown: Vec<WindowId> = std::mem::take(&mut self.ax.shown).into_iter().map(WindowId).collect();
        let Some(m) = self.model.as_mut() else { return };
        if !m.reopen_restore(shown) {
            return;
        }
        let (pending, unvisited) = (m.pending_count(), m.pending_unvisited());
        let clock = Settling::new(now, None);
        self.rematch_until = Some(clock.rematch_until());
        let again = self.settling.replace(clock).is_none();
        tracing::info!(again, pending, pending_unvisited = unvisited, "restore: the restore window opens as at a launch: saved windows go back to their stages as they are found");
    }
}
