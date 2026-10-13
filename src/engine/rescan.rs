//! The recovery scan run again, in slices, for apps a scan could not read and after a main
//! display change (6X; the review of 6X, M1), decided without touching anything.
//!
//! An app busy after a display change answers `kAXErrorCannotComplete` for a while: its
//! windows cannot be read, so a scan cannot tell whether one sits in the park zone (6X: Mail
//! and Telegram stayed parked behind it). Such apps are scanned again ([`Rescan::unanswered`])
//! for [`RESCAN_FOR`], whether the launch's scan or a display change's sweep missed them.
//!
//! After a main display change macOS puts windows back where they were the last time that
//! arrangement was in use, parked ones included, so every allowed app is scanned again
//! ([`Rescan::sweep`]). Each app that does not answer holds the WM thread up to the
//! messaging timeout, so no pass takes more than [`RESCAN_APPS`] apps, and a pass that met
//! one that did not answer puts the next off by [`RESCAN_EVERY`]: the sweep never costs the
//! WM thread more than the rescan of the apps that did not answer. Apps that answer go
//! quickly: while every app of a pass answers, the sweep's next slice is due at once.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Apps that did not answer a scan are scanned again for this long (counted from the last
/// scan they missed) ...
pub const RESCAN_FOR: Duration = Duration::from_secs(30);

/// ... a pass at most every this long while an app does not answer ...
pub const RESCAN_EVERY: Duration = Duration::from_secs(3);

/// ... and at most this many apps a pass (each one that does not answer holds the WM thread
/// up to the 0.25 s messaging timeout).
pub const RESCAN_APPS: usize = 2;

/// Why an app is queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The launch's scan could not read it: once the observer reads the app, the controller
    /// brings back a window it finds in the park zone, so no scan is needed any more.
    Launch,
    /// A main display change: macOS may have moved any of its windows into a corner, the
    /// observed apps' too.
    Display,
}

/// One app queued for a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Queued {
    /// The app's process id.
    pub pid: i32,
    /// Why it is queued.
    pub why: Why,
}

/// The apps waiting for a scan and when the next pass is due.
#[derive(Debug, Clone, Default)]
pub struct Rescan {
    /// Apps not scanned since they were queued (a sweep's), first come first.
    fresh: VecDeque<Queued>,
    /// Apps that did not answer, tried again at [`RESCAN_EVERY`] until `until`.
    retry: VecDeque<Queued>,
    until: Option<Instant>,
    /// No pass before this.
    next: Option<Instant>,
}

impl Rescan {
    /// True when no app waits.
    pub fn is_empty(&self) -> bool {
        self.fresh.is_empty() && self.retry.is_empty()
    }

    /// Apps a scan at `now` could not read: tried again at [`RESCAN_EVERY`] for
    /// [`RESCAN_FOR`] from now.
    pub fn unanswered(&mut self, pids: impl IntoIterator<Item = i32>, why: Why, now: Instant) {
        let mut any = false;
        for pid in pids {
            any = true;
            self.fresh.retain(|q| q.pid != pid);
            match self.retry.iter_mut().find(|q| q.pid == pid) {
                Some(q) => q.why = stronger(q.why, why),
                None => self.retry.push_back(Queued { pid, why }),
            }
        }
        if any {
            self.until = Some(self.until.map_or(now + RESCAN_FOR, |u| u.max(now + RESCAN_FOR)));
            self.next = Some(self.next.map_or(now + RESCAN_EVERY, |n| n.max(now + RESCAN_EVERY)));
        }
    }

    /// A main display change: every app in `pids` is scanned, in slices, the first slice due
    /// at once (unless an app that did not answer holds the pace).
    pub fn sweep(&mut self, pids: impl IntoIterator<Item = i32>) {
        for pid in pids {
            if let Some(q) = self.retry.iter_mut().find(|q| q.pid == pid) {
                q.why = Why::Display;
            } else if !self.fresh.iter().any(|q| q.pid == pid) {
                self.fresh.push_back(Queued { pid, why: Why::Display });
            }
        }
    }

    /// The apps for a pass at `now` (empty when none is due): up to [`RESCAN_APPS`] of a
    /// sweep's first, else of those that did not answer. A [`Why::Launch`] app the observer
    /// reads now (`observed`) leaves the queue. Also returns the apps whose time ran out.
    pub fn due(&mut self, now: Instant, observed: impl Fn(i32) -> bool) -> (Vec<Queued>, Vec<Queued>) {
        let keep = |q: &Queued| q.why != Why::Launch || !observed(q.pid);
        self.fresh.retain(keep);
        self.retry.retain(keep);
        let mut expired = Vec::new();
        if self.until.is_some_and(|u| now >= u) {
            expired.extend(self.retry.drain(..));
            self.until = None;
        }
        if self.next.is_some_and(|n| now < n) {
            return (Vec::new(), expired);
        }
        let from = if self.fresh.is_empty() { &mut self.retry } else { &mut self.fresh };
        let n = from.len().min(RESCAN_APPS);
        (from.drain(..n).collect(), expired)
    }

    /// After a pass at `now`: `unanswered` are those of its apps that did not answer. The
    /// next pass is due at once while a sweep's apps wait and every app answered, else after
    /// [`RESCAN_EVERY`].
    pub fn done(&mut self, unanswered: &[Queued], now: Instant) {
        for q in unanswered {
            self.unanswered([q.pid], q.why, now);
        }
        self.next = if unanswered.is_empty() && !self.fresh.is_empty() { None } else { Some(now + RESCAN_EVERY) };
    }
}

/// The reason that scans more: a display change scans an app the observer reads too.
fn stronger(a: Why, b: Why) -> Why {
    if a == Why::Display || b == Why::Display { Why::Display } else { Why::Launch }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pids(q: &[Queued]) -> Vec<i32> {
        q.iter().map(|q| q.pid).collect()
    }

    const NONE: fn(i32) -> bool = |_| false;

    /// The launch's scan: apps that did not answer are tried again two at a time every 3 s
    /// for 30 s; an app the observer reads by then needs no scan.
    #[test]
    fn apps_that_did_not_answer_the_launch_are_tried_again_two_a_pass() {
        let t0 = Instant::now();
        let mut r = Rescan::default();
        r.unanswered([11, 12, 13], Why::Launch, t0);
        assert!(r.due(t0, NONE).0.is_empty(), "not at once");
        let (pass, _) = r.due(t0 + RESCAN_EVERY, NONE);
        assert_eq!(pids(&pass), vec![11, 12]);
        r.done(&[pass[0]], t0 + RESCAN_EVERY);
        assert!(r.due(t0 + RESCAN_EVERY + Duration::from_secs(1), NONE).0.is_empty(), "the pace holds");
        let (pass, _) = r.due(t0 + RESCAN_EVERY * 2, |p| p == 13);
        assert_eq!(pids(&pass), vec![11], "13 is observed now");
        r.done(&pass, t0 + RESCAN_EVERY * 2);
        // 11 missed again at 6 s: tried until 36 s, then let go.
        let (pass, expired) = r.due(t0 + Duration::from_secs(36), NONE);
        assert!(pass.is_empty());
        assert_eq!(pids(&expired), vec![11]);
        assert!(r.is_empty());
    }

    /// The review of 6X, M1: a display change's sweep goes two apps a pass, the next slice
    /// at once while every app answers; one that does not answer joins the apps tried again
    /// (30 s, two a pass) and holds the pace to the rescan's.
    #[test]
    fn a_display_change_sweeps_in_slices_and_keeps_the_apps_that_did_not_answer() {
        let t0 = Instant::now();
        let mut r = Rescan::default();
        r.sweep([1, 2, 3, 4, 5]);
        let (pass, _) = r.due(t0, NONE);
        assert_eq!(pids(&pass), vec![1, 2]);
        r.done(&[], t0);
        let (pass, _) = r.due(t0, NONE);
        assert_eq!(pids(&pass), vec![3, 4], "the next slice at once: both answered");
        r.done(&[pass[1]], t0);
        assert!(r.due(t0 + Duration::from_secs(1), NONE).0.is_empty(), "4 did not answer: the pace is the rescan's");
        let t1 = t0 + RESCAN_EVERY;
        let (pass, _) = r.due(t1, |p| p == 4);
        assert_eq!(pids(&pass), vec![5], "the sweep's own apps first");
        r.done(&[], t1);
        let (pass, _) = r.due(t1, |p| p == 4);
        assert!(pass.is_empty(), "the slow pace for the apps that did not answer");
        let (pass, _) = r.due(t1 + RESCAN_EVERY, |p| p == 4);
        assert_eq!(pass, vec![Queued { pid: 4, why: Why::Display }], "observed, still scanned: a display change's");
        r.done(&pass, t1 + RESCAN_EVERY);
        let (_, expired) = r.due(t0 + RESCAN_EVERY * 2 + RESCAN_FOR, NONE);
        assert_eq!(pids(&expired), vec![4], "tried for 30 s after the last miss");
    }

    #[test]
    fn a_sweep_does_not_queue_an_app_twice_and_widens_a_launch_entry() {
        let t0 = Instant::now();
        let mut r = Rescan::default();
        r.unanswered([7], Why::Launch, t0);
        r.sweep([7, 8, 8]);
        assert!(r.due(t0, |p| p == 7).0.is_empty(), "7 did not answer: it holds the pace, the sweep too");
        let t1 = t0 + RESCAN_EVERY;
        let (pass, _) = r.due(t1, |p| p == 7);
        assert_eq!(pids(&pass), vec![8], "the sweep's first, 8 once");
        r.done(&[], t1);
        let (pass, _) = r.due(t1 + RESCAN_EVERY, |p| p == 7);
        assert_eq!(pass, vec![Queued { pid: 7, why: Why::Display }], "observed, scanned all the same");
    }

    /// No pass ever takes more than two apps.
    #[test]
    fn no_pass_takes_more_than_two_apps() {
        let t0 = Instant::now();
        let mut r = Rescan::default();
        r.sweep(0..40);
        r.unanswered(100..140, Why::Launch, t0);
        let mut t = t0;
        while !r.is_empty() {
            let (pass, _) = r.due(t, NONE);
            assert!(pass.len() <= RESCAN_APPS);
            if !pass.is_empty() {
                r.done(&[], t);
            }
            assert!(t < t0 + RESCAN_FOR + RESCAN_EVERY, "the queue empties: the sweep's apps go, the others expire");
            t += Duration::from_millis(250);
        }
    }
}
