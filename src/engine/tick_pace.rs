//! How often the WM thread's housekeeping tick runs (round sm-tick), decided without touching
//! anything.
//!
//! The tick ([`crate::platform::wm_thread`]) runs the duties that have no event of their own:
//! a window closed while its app did not answer, an app gone without its quit notice, the
//! Space check, the engine's upkeep, the debounced save, the config file's poll and the
//! deadlines the controller sets itself. Until 1.9 it ran every 0.25 s for the life of the
//! app; measured at rest (11-14, captures off) it was 83-87 % of the app's CPU and 4 of its
//! 7 wake-ups a second.
//!
//! Now it is **brisk** ([`BRISK`], the old period) while something happens and **at rest**
//! ([`REST`]) otherwise:
//!
//! - every event that may change windows **stirs** it for [`BRISK_FOR`]: an Accessibility
//!   notice other than a title change, a workspace notice, a click, a key with ⌘, ⌥ or ⌃, a
//!   strip, hotkey, display or ctl command ([`TickPace::stir`]); after one, the next tick is
//!   never more than [`BRISK`] away, as before;
//! - a duty with work waiting (a switch under way, a save due, a window counted missing, a
//!   stray parked again, a deadline set) keeps it brisk for as long as it waits: its owner says
//!   why ([`Pace::Brisk`]'s reason), so the confirmations and retries run at their old pace;
//! - a duty whose answer another thread reads while it is fresh caps the rest ([`Pace::Rest`]'s
//!   period: the inert slivers' list, judged by the event tap only within a second).
//!
//! Every duty still runs at every tick, in the same order, so every case the tick caught is
//! still caught; only how soon at rest changes (at most [`REST`] later). The full pass (the
//! closed windows' sweep, the gone apps' pids, the pending apps) runs about once a second,
//! by the clock now ([`TickPace::full_pass`]), so at rest every tick is a full one.

use std::time::{Duration, Instant};

/// The tick's period while something happens (the only period before 1.9).
pub const BRISK: Duration = Duration::from_millis(250);

/// How long one event keeps the tick brisk.
pub const BRISK_FOR: Duration = Duration::from_secs(3);

/// The tick's period at rest.
pub const REST: Duration = Duration::from_secs(2);

/// The full pass runs once this much has passed since the last: every fourth brisk tick (the
/// old "every four ticks"). A tick after a rest is always one ([`TickPace::full_pass`]).
pub const FULL_EVERY: Duration = Duration::from_millis(900);

/// The timer's tolerance at rest, at most: the system may fire it this much late to batch
/// wake-ups. A brisk tick has none, as before.
pub const REST_TOLERANCE: Duration = Duration::from_millis(200);

/// The pace of the next tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    /// Every [`BRISK`]: stirred by an event within [`BRISK_FOR`] (`None`), or a duty waits
    /// (its reason).
    Brisk(Option<&'static str>),
    /// Every so often: [`REST`], or less when a duty caps it.
    Rest(Duration),
}

impl Pace {
    /// The period of this pace.
    pub fn period(self) -> Duration {
        match self {
            Pace::Brisk(_) => BRISK,
            Pace::Rest(d) => d.max(BRISK),
        }
    }

    /// The timer's tolerance for this pace.
    pub fn tolerance(self) -> Duration {
        match self {
            Pace::Brisk(_) => Duration::ZERO,
            Pace::Rest(d) => (d / 10).min(REST_TOLERANCE),
        }
    }

    /// True at rest.
    pub fn at_rest(self) -> bool {
        matches!(self, Pace::Rest(_))
    }
}

/// The tick's clock: the last event that stirred it, the last full pass.
#[derive(Debug, Clone, Default)]
pub struct TickPace {
    stirred: Option<Instant>,
    full_at: Option<Instant>,
    /// The last tick ([`Self::full_pass`] is asked once a tick).
    ticked: Option<Instant>,
}

impl TickPace {
    /// A clock stirred at `now` (the thread's start: the launch is busy).
    pub fn new(now: Instant) -> Self {
        Self { stirred: Some(now), full_at: None, ticked: None }
    }

    /// An event at `now` that may change windows: brisk for [`BRISK_FOR`].
    pub fn stir(&mut self, now: Instant) {
        self.stirred = Some(self.stirred.map_or(now, |t| t.max(now)));
    }

    /// True while the last event is less than [`BRISK_FOR`] old at `now`.
    pub fn stirred(&self, now: Instant) -> bool {
        self.stirred.is_some_and(|t| now.saturating_duration_since(t) < BRISK_FOR)
    }

    /// The pace at `now`: brisk while stirred or while a duty waits (`waits`, its reason),
    /// else at rest every `rest` ([`REST`], or a duty's cap).
    pub fn pace(&self, now: Instant, waits: Option<&'static str>, rest: Duration) -> Pace {
        match waits {
            Some(why) => Pace::Brisk(Some(why)),
            None if self.stirred(now) => Pace::Brisk(None),
            None => Pace::Rest(rest),
        }
    }

    /// Whether the tick at `now` is a full pass, noted as made when it is (asked once a tick):
    /// about once a second while brisk ([`FULL_EVERY`]), and every tick after a rest (two brisk
    /// periods or more since the last tick: a rest shorter than a second, the inert slivers'
    /// cap, must not make it every other rest).
    pub fn full_pass(&mut self, now: Instant) -> bool {
        let rested = self.ticked.is_some_and(|t| now.saturating_duration_since(t) >= BRISK * 2);
        self.ticked = Some(now);
        let due = rested || self.full_at.is_none_or(|t| now.saturating_duration_since(t) >= FULL_EVERY);
        if due {
            self.full_at = Some(now);
        }
        due
    }
}

/// When the tick's timer should fire next, if it should move: `next`, when it fires now (in
/// seconds on the timer's clock), at `now`, for `pace`. After a tick (`after_tick`) at rest it
/// waits the whole rest from now (a repeating timer would fire again a brisk period later);
/// otherwise it only ever comes sooner, never more than the pace's period away (an event
/// right after a rest tick brings the next one within [`BRISK`]).
pub fn retime(pace: Pace, now: f64, next: f64, after_tick: bool) -> Option<f64> {
    let period = pace.period().as_secs_f64();
    let want = now + period;
    if after_tick && pace.at_rest() {
        return ((next - want).abs() > 0.001).then_some(want);
    }
    (next > want + 0.001).then_some(want)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn an_event_keeps_it_brisk_for_a_while_then_it_rests() {
        let t0 = Instant::now();
        let mut p = TickPace::default();
        assert_eq!(p.pace(t0, None, REST), Pace::Rest(REST), "nothing ever happened: at rest");
        p.stir(t0);
        assert_eq!(p.pace(t0, None, REST), Pace::Brisk(None));
        assert_eq!(p.pace(ms(t0, 2999), None, REST), Pace::Brisk(None));
        assert_eq!(p.pace(t0 + BRISK_FOR, None, REST), Pace::Rest(REST));
        // A later event starts the time again; an older one (a late clock read) never shortens it.
        p.stir(ms(t0, 2000));
        p.stir(ms(t0, 1000));
        assert_eq!(p.pace(ms(t0, 4900), None, REST), Pace::Brisk(None));
        assert_eq!(p.pace(ms(t0, 5000), None, REST), Pace::Rest(REST));
    }

    #[test]
    fn a_duty_that_waits_keeps_it_brisk_and_says_why() {
        let t0 = Instant::now();
        let p = TickPace::default();
        assert_eq!(p.pace(t0, Some("a save is due"), REST), Pace::Brisk(Some("a save is due")));
        assert_eq!(p.pace(t0, None, Duration::from_millis(750)), Pace::Rest(Duration::from_millis(750)), "a duty caps the rest");
    }

    #[test]
    fn the_thread_starts_brisk() {
        let t0 = Instant::now();
        let p = TickPace::new(t0);
        assert!(p.pace(t0, None, REST) == Pace::Brisk(None));
        assert!(p.pace(t0 + BRISK_FOR, None, REST).at_rest());
    }

    #[test]
    fn the_full_pass_is_every_fourth_brisk_tick_and_every_tick_at_rest() {
        let t0 = Instant::now();
        let mut p = TickPace::default();
        let brisk: Vec<bool> = (0..9).map(|i| p.full_pass(t0 + BRISK * i)).collect();
        assert_eq!(brisk, [true, false, false, false, true, false, false, false, true]);
        // Brisk ticks with a little jitter: the fourth is still the full one.
        let mut p = TickPace::default();
        let jitter: Vec<bool> = [0, 260, 480, 730, 990].into_iter().map(|t| p.full_pass(ms(t0, t))).collect();
        assert_eq!(jitter, [true, false, false, false, true]);
        let rest: Vec<bool> = (1..5).map(|i| p.full_pass(ms(t0, 990) + REST * i)).collect();
        assert_eq!(rest, [true; 4]);
        // A rest under a second (the inert slivers' cap): still every tick.
        let mut p = TickPace::default();
        let capped: Vec<bool> = (0..5).map(|i| p.full_pass(t0 + Duration::from_millis(750) * i)).collect();
        assert_eq!(capped, [true; 5]);
        // A brisk tick a little late (the thread was busy) is no rest.
        let mut p = TickPace::default();
        assert!(p.full_pass(t0));
        assert!(!p.full_pass(ms(t0, 320)));
    }

    #[test]
    fn periods_and_tolerances() {
        assert_eq!(Pace::Brisk(None).period(), BRISK);
        assert_eq!(Pace::Brisk(None).tolerance(), Duration::ZERO, "brisk ticks as before: no tolerance");
        assert_eq!(Pace::Rest(REST).period(), REST);
        assert_eq!(Pace::Rest(REST).tolerance(), REST_TOLERANCE);
        assert_eq!(Pace::Rest(Duration::from_millis(750)).tolerance(), Duration::from_millis(75));
        assert_eq!(Pace::Rest(Duration::ZERO).period(), BRISK, "never faster than brisk");
    }

    #[test]
    fn the_timer_moves_only_when_it_must() {
        let rest = Pace::Rest(REST);
        let brisk = Pace::Brisk(None);
        // After a tick at rest: the whole rest from now, not the repeating timer's next.
        assert_eq!(retime(rest, 100.0, 100.25, true), Some(102.0));
        assert_eq!(retime(rest, 100.0, 102.0, true), None, "already there");
        // After a brisk tick the repeating timer is already a brisk period away.
        assert_eq!(retime(brisk, 100.0, 100.25, true), None);
        // An event while at rest: the next tick within a brisk period.
        assert_eq!(retime(brisk, 100.5, 102.0, false), Some(100.75));
        // An event just before a tick due anyway: left alone.
        assert_eq!(retime(brisk, 101.9, 102.0, false), None);
        // A cap that shortens the rest brings the next tick in; a longer rest never pushes it out.
        assert_eq!(retime(Pace::Rest(Duration::from_millis(750)), 100.0, 102.0, false), Some(100.75));
        assert_eq!(retime(rest, 100.0, 100.75, false), None);
    }
}
