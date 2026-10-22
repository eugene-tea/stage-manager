//! "Follow the Dock" (`[strip] order = "dock"`): the Dock's order of applications, read
//! through Accessibility ([`dock::app_order`]) and handed to the model, which sorts the strip
//! by it ([`crate::engine::model::Model::set_dock_order`]).
//!
//! **When it is read** (only while the strip follows the Dock): when the model is built at
//! launch, before the first strip shows; [`SETTLE`] after an app launches or quits (its
//! icon, unpinned, comes or goes, and the Dock adds or removes it a moment after
//! NSWorkspace says so); and every [`POLL`] on the controller's tick, which is the signal
//! for the user rearranging the Dock. No Accessibility notification is documented for the
//! Dock's list, and the app may not be run to find one that fires (a reorder by drag would
//! have to be observed live); the tick is certain, and cheap: the Dock's lists, then each
//! item's subrole and `AXURL`, about two calls an item at the Dock's 0.1 s bound, a read
//! dropped at the first call the Dock cannot answer. `refresh_dock_bar` already polls the
//! Dock's bar on the same tick at the same pace.
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

/// How often the Dock's order is read while the strip follows it: the signal for the user
/// rearranging the Dock.
pub(super) const POLL: Duration = Duration::from_secs(2);

/// How long after an app launches or quits the Dock's order is read (the Dock adds or
/// removes the icon a moment after).
pub(super) const SETTLE: Duration = Duration::from_millis(500);

/// When the Dock's order is read next, and an order read that the model has not taken yet.
#[derive(Debug, Default)]
pub(super) struct DockOrderWatch {
    /// When to read next; `None`: at the next chance.
    next: Option<Instant>,
    /// The last app launch seen (the observer's).
    launch_seen: Option<Instant>,
    /// An order read while the strip could not take it, given to the model at the first tick
    /// that can.
    waiting: Option<Vec<DockApp>>,
}

impl DockOrderWatch {
    /// True when the order is to be read at `now`. `last_launch`: the last app launch the
    /// observer saw; a new one brings the next read to [`SETTLE`] after it.
    fn due(&mut self, now: Instant, last_launch: Option<Instant>) -> bool {
        if last_launch != self.launch_seen {
            self.launch_seen = last_launch;
            if let Some(t) = last_launch {
                self.soon(t);
            }
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
        self.next = None;
        self.waiting = None;
    }

    /// A read was made at `now` (whatever it found): the next is [`POLL`] later.
    fn read(&mut self, now: Instant) {
        self.next = Some(now + POLL);
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
        if self.dock_order.due(now, last_launch) {
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
        }
    }

    /// True while the strip's order must not change: a switch's windows wait behind their
    /// snapshots, a window is dragged, a batch holds the strip's snapshot, or a Space change
    /// settles (or the screen is locked, or a full-screen Space shows).
    fn strip_in_motion(&self) -> bool {
        self.pending.is_some() || self.drag.is_some() || self.defer_present || self.frozen()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dock_is_read_at_once_then_every_poll_and_soon_after_a_launch() {
        let t0 = Instant::now();
        let mut w = DockOrderWatch::default();
        assert!(w.due(t0, None), "at launch, at once");
        w.read(t0);
        assert!(!w.due(t0 + Duration::from_millis(1900), None));
        assert!(w.due(t0 + POLL, None), "every poll: the user may have rearranged the Dock");
        w.read(t0 + POLL);
        // An app launches: read once its icon is in, before the next poll.
        let launch = t0 + POLL + Duration::from_millis(100);
        assert!(!w.due(launch, Some(launch)), "not before the Dock adds the icon");
        assert!(w.due(launch + SETTLE, Some(launch)), "a moment after the launch");
        w.read(launch + SETTLE);
        assert!(!w.due(launch + SETTLE + Duration::from_millis(10), Some(launch)), "the same launch counts once");
        // An app quits.
        let quit = launch + Duration::from_secs(1);
        w.soon(quit);
        assert!(w.due(quit + SETTLE, Some(launch)));
        // Not following the Dock: the next time it does, at once.
        w.read(quit + SETTLE);
        w.reset();
        assert!(w.due(quit + SETTLE, Some(launch)));
    }
}
