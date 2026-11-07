//! When `state.json` is written: a change marks the state dirty, and it is saved once changes
//! have been quiet for [`SAVE_DEBOUNCE`] (a busy stream at least every [`SAVE_MAX_DELAY`]).
//! The state stays dirty until a save has replaced the file durably (R4 of the review
//! before 1.7: a save that failed, a full disk or a replacement refused, cleared the mark
//! first, so nothing retried it until the next change, and a crash then lost the stages the
//! user had arranged). A failed save is tried again after [`SAVE_RETRY_FIRST`], doubling to
//! [`SAVE_RETRY_MAX`], until one succeeds; a new change does not hurry it.

use std::time::{Duration, Instant};

/// Quiet time before `state.json` is written after a change.
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// A busy stream of changes is still saved at least this often.
pub const SAVE_MAX_DELAY: Duration = Duration::from_secs(3);

/// The wait before a failed save is tried again, the first time.
pub const SAVE_RETRY_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between tries of a save that keeps failing.
pub const SAVE_RETRY_MAX: Duration = Duration::from_secs(60);

/// The save's clock: what is unsaved since when, and when a failed save is tried again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaveClock {
    /// The first and the last change not saved yet.
    dirty: Option<(Instant, Instant)>,
    /// Saves failed in a row.
    failures: u32,
    /// No try before this, after a failure.
    retry_at: Option<Instant>,
}

impl SaveClock {
    /// A change at `now`.
    pub fn mark(&mut self, now: Instant) {
        self.dirty = Some((self.dirty.map_or(now, |d| d.0), now));
    }

    /// True while a change is not saved.
    pub fn is_dirty(&self) -> bool {
        self.dirty.is_some()
    }

    /// Saves failed in a row.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// True when the state should be written at `now`: unsaved, quiet long enough (or unsaved
    /// for long enough), and past any wait after a failure.
    pub fn due(&self, now: Instant) -> bool {
        let Some((first, last)) = self.dirty else { return false };
        let ripe = now.duration_since(last) >= SAVE_DEBOUNCE || now.duration_since(first) >= SAVE_MAX_DELAY;
        ripe && self.retry_at.is_none_or(|t| now >= t)
    }

    /// The file was replaced durably: nothing is unsaved.
    pub fn saved(&mut self) {
        *self = Self::default();
    }

    /// A save failed at `now` (not encoded, not written): the state stays unsaved (also when
    /// the save was asked with nothing marked) and is tried again after the returned wait.
    pub fn failed(&mut self, now: Instant) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let wait = SAVE_RETRY_FIRST.saturating_mul(1 << (self.failures - 1).min(16)).min(SAVE_RETRY_MAX);
        self.retry_at = Some(now + wait);
        if self.dirty.is_none() {
            self.dirty = Some((now, now));
        }
        wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_is_saved_once_quiet_or_after_the_longest_delay() {
        let t = Instant::now();
        let mut c = SaveClock::default();
        assert!(!c.due(t));
        c.mark(t);
        assert!(!c.due(t + Duration::from_millis(400)));
        assert!(c.due(t + SAVE_DEBOUNCE));
        // A change every 400 ms: never quiet, saved after the longest delay all the same.
        for i in 1..=7 {
            c.mark(t + Duration::from_millis(400 * i));
        }
        assert!(!c.due(t + Duration::from_millis(2900)));
        assert!(c.due(t + SAVE_MAX_DELAY));
        c.saved();
        assert!(!c.is_dirty() && !c.due(t + Duration::from_secs(10)));
    }

    #[test]
    fn a_failed_save_stays_unsaved_and_is_tried_again_later_and_later() {
        let t = Instant::now();
        let mut c = SaveClock::default();
        c.mark(t);
        let at = t + SAVE_DEBOUNCE;
        assert!(c.due(at));
        // The disk is full: the stages are still unsaved, with no change after.
        assert_eq!(c.failed(at), SAVE_RETRY_FIRST);
        assert!(c.is_dirty(), "a failure keeps the state unsaved");
        assert!(!c.due(at + Duration::from_millis(900)), "not at every tick");
        assert!(c.due(at + SAVE_RETRY_FIRST), "tried again with no change");
        let again = at + SAVE_RETRY_FIRST;
        assert_eq!(c.failed(again), 2 * SAVE_RETRY_FIRST);
        assert!(!c.due(again + SAVE_RETRY_FIRST));
        assert!(c.due(again + 2 * SAVE_RETRY_FIRST));
        // A change meanwhile does not hurry a disk that is full.
        c.mark(again + Duration::from_millis(100));
        assert!(!c.due(again + Duration::from_millis(1500)));
        // The waits stop growing at the longest.
        let mut now = again;
        for _ in 0..20 {
            now += SAVE_RETRY_MAX;
            c.failed(now);
        }
        assert!(c.due(now + SAVE_RETRY_MAX));
        // Space freed: the next try writes it, and the clock starts over.
        c.saved();
        assert_eq!(c.failures(), 0);
        c.mark(now);
        assert!(c.due(now + SAVE_DEBOUNCE));
    }

    #[test]
    fn a_failed_save_asked_with_nothing_marked_is_tried_again() {
        // A save asked outright (unpark-all, `ctl`) that fails still leaves the model unsaved.
        let t = Instant::now();
        let mut c = SaveClock::default();
        c.failed(t);
        assert!(c.is_dirty());
        assert!(c.due(t + SAVE_RETRY_FIRST));
    }
}
