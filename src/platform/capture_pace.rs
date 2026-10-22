//! How often window capture rounds go while they keep failing, and none while nobody can see
//! the screen (round 8M).
//!
//! The owner's week (10-16 … 10-21): ScreenCaptureKit answered "Failed to start stream due
//! to audio/video capture failure", or nothing at all, for 5–10 minutes at a time with the
//! screen unlocked, and every round asked again at once (a round timed out every ~2.5 s,
//! hundreds of thousands of failed captures a day). So:
//!
//! - **Back-off**: after [`FAILS_BEFORE_BACKOFF`] failed rounds in a row (no image, or no
//!   answer in time), the next round waits [`BACKOFF_START`], then twice as long after each
//!   further failure, up to [`BACKOFF_CAP`]. The first round that brings an image back ends
//!   it. Its start and its end are logged once each; the rounds in between at debug.
//! - **Away**: while the screen is locked or the display sleeps (the controller tells,
//!   [`Pace::set_away`]) no round goes at all; the first one after goes at once.
//!
//! Cards keep their last good pictures meanwhile (the strip only replaces a picture when a new
//! one arrives). A refusal (user declined) is the gate's business ([`super::capture_gate`]),
//! not a failure here.
//!
//! Pure state, no clocks of its own: every call takes `now`, so the rules are unit tested.
//! The capturer keeps one pace for the whole process ([`super::capture`]).

use std::time::{Duration, Instant};

/// Failed rounds in a row before rounds back off.
pub const FAILS_BEFORE_BACKOFF: u32 = 3;
/// The first wait of a back-off.
pub const BACKOFF_START: Duration = Duration::from_secs(2);
/// The longest wait between two rounds while they fail.
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Why nobody can see the screen now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Away {
    /// The screen is locked (or the session is not on the console).
    Locked,
    /// The main display sleeps.
    Asleep,
}

impl Away {
    /// For the log.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Locked => "the screen is locked",
            Self::Asleep => "the display sleeps",
        }
    }
}

/// What a call changed, so the caller logs each change once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Nothing worth a line.
    None,
    /// Rounds back off: `failed` rounds in a row, the next waits `wait`.
    BackingOff {
        /// Failed rounds in a row.
        failed: u32,
        /// The next round's wait.
        wait: Duration,
    },
    /// A round brought an image back after a back-off of `lasted`, `failed` rounds in a row.
    BackoffOver {
        /// Failed rounds in a row before it.
        failed: u32,
        /// From the back-off's start.
        lasted: Duration,
    },
    /// Rounds pause.
    Paused(Away),
    /// Rounds go again after a pause of `lasted`.
    Resumed {
        /// How long they paused.
        lasted: Duration,
    },
}

/// One process's capture pace.
#[derive(Clone, Debug, Default)]
pub struct Pace {
    /// Failed rounds in a row.
    failed: u32,
    /// The current back-off's wait and its start (none while rounds work).
    backoff: Option<(Duration, Instant)>,
    /// No round before this (a back-off's wait from the last failed round's end).
    not_before: Option<Instant>,
    /// Why rounds pause, since when.
    away: Option<(Away, Instant)>,
}

impl Pace {
    /// How long until a round may go: `None` while rounds pause (nobody sees the screen),
    /// zero when one may go now.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        if self.away.is_some() {
            return None;
        }
        Some(self.not_before.map_or(Duration::ZERO, |t| t.saturating_duration_since(now)))
    }

    /// True while rounds back off.
    pub fn backing_off(&self) -> bool {
        self.backoff.is_some()
    }

    /// Failed rounds in a row.
    pub fn failed_in_a_row(&self) -> u32 {
        self.failed
    }

    /// A round ended at `now`: `ok` when at least one image came back.
    pub fn finished(&mut self, ok: bool, now: Instant) -> Change {
        if ok {
            let failed = std::mem::take(&mut self.failed);
            self.not_before = None;
            return match self.backoff.take() {
                Some((_, since)) => Change::BackoffOver { failed, lasted: now.saturating_duration_since(since) },
                None => Change::None,
            };
        }
        self.failed = self.failed.saturating_add(1);
        if self.failed < FAILS_BEFORE_BACKOFF {
            return Change::None;
        }
        let (wait, change) = match self.backoff {
            None => (BACKOFF_START, true),
            Some((w, _)) => ((w * 2).min(BACKOFF_CAP), false),
        };
        let since = self.backoff.map_or(now, |(_, s)| s);
        self.backoff = Some((wait, since));
        self.not_before = Some(now + wait);
        if change { Change::BackingOff { failed: self.failed, wait } } else { Change::None }
    }

    /// The controller's word on whether anybody can see the screen (`None`: yes).
    pub fn set_away(&mut self, away: Option<Away>, now: Instant) -> Change {
        match (self.away, away) {
            (None, Some(a)) => {
                self.away = Some((a, now));
                Change::Paused(a)
            }
            (Some((_, since)), None) => {
                self.away = None;
                Change::Resumed { lasted: now.saturating_duration_since(since) }
            }
            (Some((was, since)), Some(a)) if was != a => {
                // Still paused, for another reason (locked, then the display slept).
                self.away = Some((a, since));
                Change::None
            }
            _ => Change::None,
        }
    }

    /// Short state for the ctl dump.
    pub fn describe(&self) -> String {
        match (self.away, self.backoff) {
            (Some((a, _)), _) => format!("paused: {}", a.as_str()),
            (None, Some((w, _))) => format!("backing off: {} failed rounds, {} s between", self.failed, w.as_secs()),
            (None, None) => "normal".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    /// 10-18 09:30: every round fails for minutes. Two failures change nothing; the third
    /// starts the back-off (logged once), each further one doubles the wait up to the cap
    /// (not logged again); the first image ends it (logged once) and the next round goes at
    /// once.
    #[test]
    fn failing_rounds_back_off_doubling_to_the_cap_and_one_image_resets() {
        let t0 = Instant::now();
        let mut p = Pace::default();
        assert_eq!(p.wait(t0), Some(Duration::ZERO));
        assert_eq!(p.finished(false, t0), Change::None);
        assert_eq!(p.finished(false, t0 + MS(5000)), Change::None);
        assert_eq!(p.wait(t0 + MS(5000)), Some(Duration::ZERO), "two failures: no back-off yet");
        let t = t0 + MS(10_000);
        assert_eq!(p.finished(false, t), Change::BackingOff { failed: 3, wait: BACKOFF_START });
        assert_eq!(p.wait(t), Some(BACKOFF_START));
        assert_eq!(p.wait(t + MS(1500)), Some(MS(500)));
        let mut waits = Vec::new();
        let mut now = t;
        for _ in 0..8 {
            now += p.wait(now).unwrap_or_default() + MS(5000);
            assert_eq!(p.finished(false, now), Change::None, "logged once, not every round");
            waits.push(p.wait(now).unwrap_or_default().as_secs());
        }
        assert_eq!(waits, vec![4, 8, 16, 30, 30, 30, 30, 30], "doubling up to the cap");
        assert!(p.backing_off());
        let end = now + MS(30_000);
        assert_eq!(p.finished(true, end), Change::BackoffOver { failed: 11, lasted: end - t });
        assert_eq!(p.wait(end), Some(Duration::ZERO), "the next round goes at once");
        assert!(!p.backing_off());
        assert_eq!(p.failed_in_a_row(), 0);
        assert_eq!(p.finished(false, end), Change::None, "counting starts again from zero");
    }

    #[test]
    fn a_success_before_the_back_off_resets_the_count() {
        let t0 = Instant::now();
        let mut p = Pace::default();
        p.finished(false, t0);
        p.finished(false, t0);
        assert_eq!(p.finished(true, t0), Change::None, "no back-off began: nothing to log");
        p.finished(false, t0);
        p.finished(false, t0);
        assert_eq!(p.wait(t0), Some(Duration::ZERO), "two in a row again, not four");
    }

    /// No round while the screen is locked or the display sleeps, logged once at each end;
    /// a change of reason while away is no new pause; the back-off's wait is kept.
    #[test]
    fn rounds_pause_while_locked_or_asleep() {
        let t0 = Instant::now();
        let mut p = Pace::default();
        assert_eq!(p.set_away(None, t0), Change::None);
        assert_eq!(p.set_away(Some(Away::Locked), t0), Change::Paused(Away::Locked));
        assert_eq!(p.wait(t0 + MS(60_000)), None, "no round while locked, however long");
        assert_eq!(p.set_away(Some(Away::Locked), t0 + MS(500)), Change::None, "said once");
        assert_eq!(p.set_away(Some(Away::Asleep), t0 + MS(1000)), Change::None, "still away");
        assert!(p.describe().contains("display sleeps"));
        let back = t0 + MS(90_000);
        assert_eq!(p.set_away(None, back), Change::Resumed { lasted: MS(90_000) });
        assert_eq!(p.wait(back), Some(Duration::ZERO), "the first round after goes at once");
        assert_eq!(p.set_away(None, back), Change::None);
    }

    #[test]
    fn a_pause_keeps_a_back_off_whose_wait_runs_on() {
        let t0 = Instant::now();
        let mut p = Pace::default();
        for _ in 0..3 {
            p.finished(false, t0);
        }
        p.set_away(Some(Away::Asleep), t0 + MS(100));
        assert_eq!(p.wait(t0 + MS(500)), None);
        p.set_away(None, t0 + MS(10_000));
        assert_eq!(p.wait(t0 + MS(10_000)), Some(Duration::ZERO), "its wait ran out meanwhile");
        assert!(p.backing_off(), "still backing off until an image comes");
    }
}
