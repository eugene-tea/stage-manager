//! "Follow the Dock" (`[strip] order = "dock"`): the Dock's order of applications, read
//! through Accessibility ([`dock::app_order`]) and handed to the model, which sorts the strip
//! by it ([`crate::engine::model::Model::set_dock_order`]).
//!
//! **When it is read** (only while the strip follows the Dock), at events, not on a timer:
//! - when the model is built at launch, before the first strip shows;
//! - [`SETTLE`] after an app launches or quits (its icon, unpinned, comes or goes, and the
//!   Dock adds or removes it a moment after NSWorkspace says so);
//! - at once when the Dock's preferences file changes ([`dock::preferences_stamp`], one
//!   `stat` a tick, the file never opened or written): the Dock writes it when the user
//!   moves an icon, keeps one in the Dock or removes it. No Accessibility notification is
//!   documented for the Dock's list;
//! - and [`BACKSTOP`] after the last read, in case a change wrote nothing to the file.
//!
//! Until 1.9 it was read every 2 s on the tick: the Dock's lists, then each item's subrole
//! and `AXURL`, about two Accessibility calls an item (sixty for a Dock of thirty icons), 30
//! reads a minute at rest against about one now. What reads the order (the model's strip
//! sort, `ctl state`'s `dock_order`) needs it only when it changes, and every change is one
//! of the events above.
//!
//! **Never mid-transition.** An order read while a switch's windows wait behind their
//! snapshots, a window is dragged, a batch holds the strip's snapshot or a Space change
//! settles waits, and the model takes it at the first tick that is clear: the transition's
//! slots stay the ones its snapshots aim at. A changed order then presents the strip, whose
//! cards glide on the strip spring to their new places like any other strip update.

use std::time::{Duration, Instant};

use super::Controller;
use crate::engine::model::{DockApp, StripOrder};
use crate::platform::dock;

/// The longest time between two reads while the strip follows the Dock, with no event: only
/// a backstop for a change the preferences file did not show.
pub(super) const BACKSTOP: Duration = Duration::from_secs(60);

/// How long after an app launches or quits the Dock's order is read (the Dock adds or
/// removes the icon a moment after).
pub(super) const SETTLE: Duration = Duration::from_millis(500);

/// The Dock's preferences file as last looked at: its modification time and length.
pub(super) type FileStamp = (std::time::SystemTime, u64);

/// What the tick saw of the Dock ([`DockOrderWatch::due`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct DockSigns {
    /// The last app launch the observer saw.
    pub last_launch: Option<Instant>,
    /// The Dock's preferences file now ([`dock::preferences_stamp`]).
    pub file: Option<FileStamp>,
}

/// When the Dock's order is read next, and an order read that the model has not taken yet.
#[derive(Debug, Default)]
pub(super) struct DockOrderWatch {
    /// When to read next; `None`: at the next chance.
    next: Option<Instant>,
    /// The last app launch seen (the observer's).
    launch_seen: Option<Instant>,
    /// The preferences file as last seen; `None`: not looked at yet.
    file_seen: Option<FileStamp>,
    /// An order read while the strip could not take it, given to the model at the first tick
    /// that can.
    waiting: Option<Vec<DockApp>>,
}

impl DockOrderWatch {
    /// True when the order is to be read at `now`, from what the tick saw (`signs`): a new
    /// app launch brings the next read to [`SETTLE`] after it (a quit: [`Self::soon`]); a
    /// changed preferences file, to now. The first look at the file only notes it (the first
    /// read is due anyway). The Dock restarting is an app quitting and one launching.
    fn due(&mut self, now: Instant, signs: DockSigns) -> bool {
        if signs.last_launch != self.launch_seen {
            self.launch_seen = signs.last_launch;
            if let Some(t) = signs.last_launch {
                self.soon(t);
            }
        }
        if signs.file.is_some() && signs.file != self.file_seen {
            if self.file_seen.is_some() {
                tracing::debug!("follow the Dock: the Dock's preferences changed; its order is read again");
                self.next = Some(now);
            }
            self.file_seen = signs.file;
        }
        self.next.is_none_or(|t| now >= t)
    }

    /// Read at least by [`SETTLE`] after `at` (an app launched or quit).
    pub(super) fn soon(&mut self, at: Instant) {
        let t = at + SETTLE;
        self.next = Some(self.next.map_or(t, |n| n.min(t)));
    }

    /// Read at the next chance (the strip does not follow the Dock now: once it does, the
    /// order is read at once).
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// A read was made at `now` (whatever it found): the next at an event, or [`BACKSTOP`]
    /// later.
    fn read(&mut self, now: Instant) {
        self.next = Some(now + BACKSTOP);
    }
}

impl Controller {
    /// Reads the Dock's order when due ([`DockOrderWatch`]) and hands it to the model when
    /// no transition runs; a changed strip is presented at the tick's end. Nothing unless
    /// the strip follows the Dock.
    pub(super) fn refresh_dock_order(&mut self, now: Instant, last_launch: Option<Instant>) {
        let Some(m) = self.model.as_ref() else { return };
        if m.options().strip.order != StripOrder::Dock {
            self.dock_order.reset();
            return;
        }
        let signs = DockSigns { last_launch, file: dock::preferences_stamp() };
        if self.dock_order.due(now, signs) {
            self.dock_order.read(now);
            match dock::dock_pid().and_then(dock::app_order) {
                Some(order) => self.dock_order.waiting = Some(order),
                None => tracing::debug!("follow the Dock: the Dock gave no order; the last one stays"),
            }
        }
        if self.dock_order.waiting.is_none() || self.strip_in_motion() {
            return;
        }
        let Some(order) = self.dock_order.waiting.take() else { return };
        let Some(m) = self.model.as_mut() else { return };
        let apps = order.len();
        if m.set_dock_order(order) {
            tracing::info!(apps, "follow the Dock: the Dock's order changed; the strip follows");
            self.strip_dirty = true;
            // The next switch grows from the new slots (the review's K3).
            self.remember();
        }
    }

    /// True while the strip's order must not change: a switch's windows wait behind their
    /// snapshots, a window is dragged, a batch holds the strip's snapshot, or a Space change
    /// settles (or the screen is locked, or a full-screen Space shows).
    fn strip_in_motion(&self) -> bool {
        self.phase.switching().is_some() || self.drag.is_some() || self.defer_present || self.frozen()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn file(secs: u64, len: u64) -> Option<FileStamp> {
        Some((SystemTime::UNIX_EPOCH + Duration::from_secs(secs), len))
    }

    fn signs(last_launch: Option<Instant>, file: Option<FileStamp>) -> DockSigns {
        DockSigns { last_launch, file }
    }

    #[test]
    fn the_dock_is_read_at_once_then_only_at_events_and_soon_after_a_launch() {
        let t0 = Instant::now();
        let f = file(100, 4000);
        let mut w = DockOrderWatch::default();
        assert!(w.due(t0, signs(None, f)), "at launch, at once");
        w.read(t0);
        assert!(!w.due(t0 + Duration::from_secs(2), signs(None, f)), "no read on a timer: 2 s on, nothing changed");
        assert!(!w.due(t0 + Duration::from_secs(30), signs(None, f)));
        // An app launches: read once its icon is in.
        let launch = t0 + Duration::from_secs(30);
        assert!(!w.due(launch, signs(Some(launch), f)), "not before the Dock adds the icon");
        assert!(w.due(launch + SETTLE, signs(Some(launch), f)), "a moment after the launch");
        w.read(launch + SETTLE);
        assert!(!w.due(launch + SETTLE + Duration::from_millis(10), signs(Some(launch), f)), "the same launch counts once");
        // An app quits.
        let quit = launch + Duration::from_secs(1);
        w.soon(quit);
        assert!(w.due(quit + SETTLE, signs(Some(launch), f)));
        w.read(quit + SETTLE);
        // Not following the Dock: the next time it does, at once.
        w.reset();
        assert!(w.due(quit + SETTLE, signs(Some(launch), f)));
    }

    #[test]
    fn a_changed_preferences_file_reads_the_order_at_once() {
        let t0 = Instant::now();
        let mut w = DockOrderWatch::default();
        assert!(w.due(t0, signs(None, file(100, 4000))));
        w.read(t0);
        let t1 = t0 + Duration::from_secs(5);
        assert!(w.due(t1, signs(None, file(101, 4000))), "the Dock wrote its file: an icon moved");
        w.read(t1);
        assert!(!w.due(t1 + Duration::from_secs(1), signs(None, file(101, 4000))), "the same file counts once");
        let t2 = t1 + Duration::from_secs(2);
        assert!(w.due(t2, signs(None, file(101, 4100))), "the same second, another length");
        w.read(t2);
        // The file cannot be looked at for a moment: nothing to tell, no read.
        assert!(!w.due(t2 + Duration::from_secs(1), signs(None, None)));
        assert!(!w.due(t2 + Duration::from_secs(2), signs(None, file(101, 4100))), "back as it was");
    }

    #[test]
    fn with_no_event_a_read_still_comes_by_the_backstop() {
        let t0 = Instant::now();
        let f = file(100, 4000);
        let mut w = DockOrderWatch::default();
        assert!(w.due(t0, signs(None, f)));
        w.read(t0);
        assert!(!w.due(t0 + BACKSTOP - Duration::from_millis(1), signs(None, f)));
        assert!(w.due(t0 + BACKSTOP, signs(None, f)));
    }

    #[test]
    fn the_first_look_at_the_file_alone_reads_nothing() {
        let t0 = Instant::now();
        let mut w = DockOrderWatch::default();
        // The file could not be looked at for the first read.
        assert!(w.due(t0, signs(None, None)));
        w.read(t0);
        assert!(!w.due(t0 + Duration::from_secs(1), signs(None, file(100, 4000))), "first seen: noted, no read");
        assert!(w.due(t0 + Duration::from_secs(2), signs(None, file(102, 4000))), "then changed: read");
    }
}
