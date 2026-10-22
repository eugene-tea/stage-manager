//! Why a managed window is off screen, asked through Accessibility without letting a hung app
//! starve the WM thread (the review of 1.1.0, Fable H1), decided without touching anything.
//!
//! The Spaces check asks, once a second, for every managed window of the Desktop shown that is
//! not on screen: is its app hidden, is the window minimized, is it in full screen. Each
//! question is a synchronous Accessibility message with a 0.25 s timeout; a beach-balling app
//! with five such windows cost over a second of every second. Two rules bound it:
//!
//! - **Per pass, per app**: the app's hidden state is asked once ([`OffScreen`]'s cache).
//! - **Busy until**: an app that did not answer (timeout) is not asked again for
//!   [`BUSY_BACKOFF`] ([`BusyApps`]); meanwhile its windows count as off screen for a reason
//!   (as a "cannot tell" answer always did), so they are no evidence of another Desktop.
//!
//! A hung app now costs one timeout per [`BUSY_BACKOFF`], whatever its window count.
//!
//! **Logged once per episode** (round 8M): on 10-19 two apps did not answer for a minute and
//! more, and every retry (every ~3.5 s) logged an info line. An app's episode starts at its
//! first timeout and ends at its first answer, or once it has not been asked for
//! [`EPISODE_FORGET`] (its windows went, or it quit): [`BusyApps::take_episodes`] hands each
//! start and end once, with the duration, for the caller to log. The retry is unchanged.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long an app that did not answer is left alone.
pub const BUSY_BACKOFF: Duration = Duration::from_secs(3);

/// An episode of an app not answering ends when the app has not been asked (no timeout) for
/// this long: nothing asked it any more.
pub const EPISODE_FORGET: Duration = Duration::from_secs(60);

/// The start or the end of an app's episode of not answering, to be logged once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Episode {
    /// App `pid` did not answer (its first timeout since it last answered).
    Started {
        /// The app's process id.
        pid: i32,
    },
    /// App `pid` answered again after `lasted` (from its first timeout).
    Answered {
        /// The app's process id.
        pid: i32,
        /// From its first timeout to the answer.
        lasted: Duration,
    },
    /// App `pid` was not asked for [`EPISODE_FORGET`]: its episode is over without an answer.
    NotAsked {
        /// The app's process id.
        pid: i32,
        /// From its first timeout to its last.
        lasted: Duration,
    },
}

/// One Accessibility yes/no answer, as the platform read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// Yes.
    Yes,
    /// No (or the attribute is missing).
    No,
    /// An error other than a timeout (an invalid element, say).
    CannotTell,
    /// The app did not answer within the timeout: busy.
    TimedOut,
}

/// Apps that did not answer lately, by pid, with when they may be asked again.
#[derive(Debug, Clone, Default)]
pub struct BusyApps {
    until: HashMap<i32, Instant>,
    /// Each app's episode of not answering: its first and its latest timeout.
    episodes: HashMap<i32, (Instant, Instant)>,
    /// Starts and ends not handed out yet ([`Self::take_episodes`]).
    changes: Vec<Episode>,
}

impl BusyApps {
    /// True when app `pid` is left alone at `now`.
    pub fn busy(&self, pid: i32, now: Instant) -> bool {
        self.until.get(&pid).is_some_and(|t| now < *t)
    }

    /// Records an answer of app `pid` at `now`: a timeout makes it busy for
    /// [`BUSY_BACKOFF`]; any answer at all clears it.
    pub fn note(&mut self, pid: i32, reply: Reply, now: Instant) {
        if reply == Reply::TimedOut {
            self.until.insert(pid, now + BUSY_BACKOFF);
            match self.episodes.get_mut(&pid) {
                Some((_, last)) => *last = now,
                None => {
                    self.episodes.insert(pid, (now, now));
                    self.changes.push(Episode::Started { pid });
                }
            }
        } else {
            self.until.remove(&pid);
            if let Some((since, _)) = self.episodes.remove(&pid) {
                self.changes.push(Episode::Answered { pid, lasted: now.saturating_duration_since(since) });
            }
        }
    }

    /// Forgets apps whose backoff is over (keeps the map small), and ends the episodes of
    /// apps not asked for [`EPISODE_FORGET`].
    pub fn prune(&mut self, now: Instant) {
        self.until.retain(|_, t| now < *t);
        let over: Vec<i32> =
            self.episodes.iter().filter(|(_, (_, last))| now.saturating_duration_since(*last) >= EPISODE_FORGET).map(|(p, _)| *p).collect();
        for pid in over {
            if let Some((since, last)) = self.episodes.remove(&pid) {
                self.changes.push(Episode::NotAsked { pid, lasted: last.saturating_duration_since(since) });
            }
        }
    }

    /// The episodes' starts and ends since the last call, each once, in order.
    pub fn take_episodes(&mut self) -> Vec<Episode> {
        std::mem::take(&mut self.changes)
    }

    /// The apps left alone now.
    pub fn pids(&self, now: Instant) -> Vec<i32> {
        let mut v: Vec<i32> = self.until.iter().filter(|(_, t)| now < **t).map(|(p, _)| *p).collect();
        v.sort_unstable();
        v
    }
}

/// Apps that did not answer another periodic question (the park-zone rescans, round 8M), by
/// pid with their first miss: each app's episode is logged once at its start and once at its
/// end, with the duration, however many retries it takes.
#[derive(Debug, Clone, Default)]
pub struct Unanswered {
    since: HashMap<i32, Instant>,
}

impl Unanswered {
    /// App `pid` did not answer at `now`: true when this starts its episode (log it).
    pub fn missed(&mut self, pid: i32, now: Instant) -> bool {
        let mut new = false;
        self.since.entry(pid).or_insert_with(|| {
            new = true;
            now
        });
        new
    }

    /// App `pid` answered (or is no longer asked) at `now`: how long its episode lasted, when
    /// one was running (log it).
    pub fn ended(&mut self, pid: i32, now: Instant) -> Option<Duration> {
        self.since.remove(&pid).map(|t| now.saturating_duration_since(t))
    }
}

/// One pass of "why is this window off screen": the app's hidden state asked once per app,
/// busy apps not asked at all.
#[derive(Debug)]
pub struct OffScreen<'a> {
    busy: &'a mut BusyApps,
    hidden: HashMap<i32, bool>,
    now: Instant,
}

impl<'a> OffScreen<'a> {
    /// A pass at `now` over `busy`.
    pub fn new(busy: &'a mut BusyApps, now: Instant) -> Self {
        Self { busy, hidden: HashMap::new(), now }
    }

    /// True when window of app `pid` is off screen for a reason (its app hidden, the window
    /// minimized or in full screen, or its app busy or unable to tell). The questions are
    /// asked in that order, each only while no earlier one gave a reason and the app has not
    /// timed out; `hidden` once per app per pass. A full-screen "cannot tell" is no reason
    /// (as before); every other "cannot tell" and every timeout is.
    pub fn has_reason(
        &mut self,
        pid: i32,
        hidden: impl FnOnce() -> Reply,
        minimized: impl FnOnce() -> Reply,
        fullscreen: impl FnOnce() -> Reply,
    ) -> bool {
        if self.busy.busy(pid, self.now) {
            return true;
        }
        let now = self.now;
        let busy = &mut *self.busy;
        let hidden = *self.hidden.entry(pid).or_insert_with(|| {
            let r = hidden();
            busy.note(pid, r, now);
            r != Reply::No
        });
        if hidden {
            return true;
        }
        let r = minimized();
        self.busy.note(pid, r, now);
        if r != Reply::No {
            return true;
        }
        let r = fullscreen();
        self.busy.note(pid, r, now);
        matches!(r, Reply::Yes | Reply::TimedOut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// The H1 scenario: an app beach-balls with five windows off screen. One timeout per
    /// pass at most, then none until the backoff is over.
    #[test]
    fn a_hung_app_costs_one_timeout_per_backoff() {
        let t0 = Instant::now();
        let mut busy = BusyApps::default();
        let asked = Cell::new(0);
        let hung = || {
            asked.set(asked.get() + 1);
            Reply::TimedOut
        };
        let pass = |busy: &mut BusyApps, at: Instant| {
            let mut check = OffScreen::new(busy, at);
            (0..5).map(|_| check.has_reason(7, hung, hung, hung)).collect::<Vec<bool>>()
        };
        assert_eq!(pass(&mut busy, t0), vec![true; 5], "a busy app's windows are off screen for a reason");
        assert_eq!(asked.get(), 1, "one message for five windows");
        assert_eq!(pass(&mut busy, t0 + Duration::from_secs(1)), vec![true; 5]);
        assert_eq!(asked.get(), 1, "none while it is busy");
        assert_eq!(busy.pids(t0 + Duration::from_secs(1)), vec![7]);
        pass(&mut busy, t0 + BUSY_BACKOFF);
        assert_eq!(asked.get(), 2, "asked again once the backoff is over");
    }

    /// Round 8M, 10-19 07:31: two apps (Home and another) do not answer for a minute and
    /// more while the Spaces check asks every second. One start each, no line per retry; the
    /// retry goes on every [`BUSY_BACKOFF`]; one end with the duration when an app answers;
    /// one when the other is no longer asked.
    #[test]
    fn an_app_not_answering_is_logged_once_per_episode() {
        let t0 = Instant::now();
        let mut busy = BusyApps::default();
        let asked = Cell::new(0);
        let (home, other) = (48742, 57258);
        let mut answer_from: Option<Instant> = None;
        for s in 0..80u64 {
            let at = t0 + Duration::from_secs(s);
            busy.prune(at);
            let mut check = OffScreen::new(&mut busy, at);
            let reply = |pid: i32| {
                asked.set(asked.get() + 1);
                if pid == home && answer_from.is_some_and(|t| at >= t) { Reply::No } else { Reply::TimedOut }
            };
            if s < 70 {
                check.has_reason(home, || reply(home), || reply(home), || reply(home));
            }
            if s < 10 {
                check.has_reason(other, || reply(other), || reply(other), || reply(other));
            }
            if s == 65 {
                answer_from = Some(at);
            }
        }
        let starts: Vec<Episode> = busy.take_episodes();
        assert_eq!(starts.iter().filter(|e| matches!(e, Episode::Started { .. })).count(), 2, "{starts:?}");
        assert_eq!(starts[0], Episode::Started { pid: home });
        assert_eq!(starts[1], Episode::Started { pid: other });
        assert!(starts.contains(&Episode::Answered { pid: home, lasted: Duration::from_secs(66) }), "{starts:?}");
        assert!(starts.iter().any(|e| matches!(e, Episode::NotAsked { pid, .. } if *pid == other)), "{starts:?}");
        assert_eq!(starts.len(), 4, "nothing else: {starts:?}");
        assert!(asked.get() >= 20, "the retry goes on: asked {}", asked.get());
        assert!(busy.take_episodes().is_empty(), "each once");
    }

    #[test]
    fn a_rescan_s_unanswered_app_starts_and_ends_once() {
        let t0 = Instant::now();
        let mut u = Unanswered::default();
        assert!(u.missed(7, t0), "the first miss starts it");
        for s in 1..20 {
            assert!(!u.missed(7, t0 + Duration::from_secs(3 * s)), "retries say nothing");
        }
        assert_eq!(u.ended(7, t0 + Duration::from_secs(70)), Some(Duration::from_secs(70)));
        assert_eq!(u.ended(7, t0 + Duration::from_secs(71)), None, "once");
        assert_eq!(u.ended(8, t0), None, "an app that always answered");
    }

    #[test]
    fn an_answering_app_is_asked_hidden_once_and_each_window_on() {
        let t0 = Instant::now();
        let mut busy = BusyApps::default();
        let (hidden, minimized) = (Cell::new(0), Cell::new(0));
        let mut check = OffScreen::new(&mut busy, t0);
        let ask = |check: &mut OffScreen, min: Reply, full: Reply| {
            check.has_reason(
                3,
                || {
                    hidden.set(hidden.get() + 1);
                    Reply::No
                },
                || {
                    minimized.set(minimized.get() + 1);
                    min
                },
                || full,
            )
        };
        assert!(!ask(&mut check, Reply::No, Reply::No), "missing for no reason");
        assert!(ask(&mut check, Reply::Yes, Reply::No), "minimized");
        assert!(ask(&mut check, Reply::No, Reply::Yes), "in full screen");
        assert!(ask(&mut check, Reply::CannotTell, Reply::No), "cannot tell if minimized: a reason");
        assert!(!ask(&mut check, Reply::No, Reply::CannotTell), "cannot tell if full screen: no reason, as before");
        assert_eq!(hidden.get(), 1, "the app's hidden state once per pass");
        assert_eq!(minimized.get(), 5);
    }

    #[test]
    fn a_window_timing_out_makes_its_app_busy_and_an_answer_clears_it() {
        let t0 = Instant::now();
        let mut busy = BusyApps::default();
        {
            let mut check = OffScreen::new(&mut busy, t0);
            assert!(check.has_reason(5, || Reply::No, || Reply::TimedOut, || Reply::No));
            let mut later = false;
            assert!(check.has_reason(5, || Reply::No, || { later = true; Reply::No }, || Reply::No));
            assert!(!later, "its next window is not asked");
        }
        assert!(busy.busy(5, t0));
        busy.note(5, Reply::No, t0 + Duration::from_secs(4));
        assert!(!busy.busy(5, t0 + Duration::from_secs(4)));
        busy.note(6, Reply::TimedOut, t0);
        busy.prune(t0 + BUSY_BACKOFF);
        assert!(busy.pids(t0).is_empty(), "pruned once over");
    }
}
