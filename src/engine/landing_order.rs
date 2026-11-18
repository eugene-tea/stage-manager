//! The order of a switch's windows flying their last stretch together (round landing-order,
//! the Spaces engine's live landing).
//!
//! Each incoming window flies its last stretch in a landing Space of its own, on a thread of
//! its own ([`crate::platform::spaces::landing`]). Two overlapping windows of one stage were
//! seen to trade places while they flew and at the landing (live 11-15, Messages over
//! Telegram): each thread set its Space's transform at its own instants, and the window that
//! landed last was listed on top until the switch's focus raised the key again. The window
//! list kept their order during the flight; it cannot show how the window server composites
//! two landing Spaces.
//!
//! What fixes the order, front to back as the stage shows them ([`ranks`]: rank 0 the front):
//!
//! - **One convoy a switch** ([`Convoy`]): every stretch of the switch tells where it is
//!   ([`Leg`]), and waits, bounded, on the ones behind it, never on the ones in front (so
//!   nothing can wait in a circle).
//! - **Shown back to front**: a landing Space is shown once the Spaces behind it are
//!   ([`SHOW_WAIT`] at most).
//! - **Handed over back to front** ([`handovers_back_first`]): a window takes over from its
//!   picture no earlier than every window behind it. The pictures fly above every window, so
//!   a picture behind still flying when the front window took over was drawn over it until
//!   its own window took over under it (11-16 10:43:04, Messages at .653, Telegram's picture
//!   over it until .669: one frame of Telegram in front, the "rapid flip").
//! - **Front set last at each step** ([`stagger`]): within a step the front window's
//!   transform follows those behind it by a millisecond or so, never the other way.
//! - **Landed back to front**: a window goes onto its Desktop (where it is listed on top)
//!   only once every window behind it has ([`LAND_WAIT`] at most); it waits at its frame, at
//!   the identity, which shows nothing.
//! - **Levels** ([`level`]): `SLSSpaceSetAbsoluteLevel` gives a Space a tier of the whole
//!   session (public sources: 0 is the default, the user's own; 100 Setup Assistant, 200 the
//!   security agent, 300 the lock screen, 400 Notification Center at the lock screen, 500
//!   boot progress, 600 VoiceOver; notch apps lift a Space to 400 or `i32::MAX` to draw over
//!   the lock screen and Mission Control). A level above 0 lifts a landing window over the
//!   whole Desktop, the menu bar, the Dock, the strip and the pictures still flying in front
//!   of it. So the landing Spaces keep the level they are made with unless the test switch
//!   asks for [`LevelMode::Ranked`] (`STAGE_MANAGER_LANDING_LEVEL=ranked`): 1 for the back,
//!   one more for each window in front.
//!
//! A switch with one stretch has no convoy: nothing waits, nothing is staggered, no level is
//! set (as before this round).

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long a landing Space waits, at most, for the Spaces behind it to be shown before it
/// shows its own (S4: four Spaces made, filled and shown in 5-8 ms).
pub const SHOW_WAIT: Duration = Duration::from_millis(40);

/// How long a window that finished its flight waits, at most, at its frame for the windows
/// behind it to land (live 11-15: Messages landed ~30 ms before Telegram behind it).
pub const LAND_WAIT: Duration = Duration::from_millis(200);

/// Within a step, how much later the front window's transform is set than the one just
/// behind it ([`stagger`]).
pub const STAGGER: Duration = Duration::from_millis(1);

/// The most a stretch is staggered, however many fly ([`stagger`]).
pub const STAGGER_MAX: Duration = Duration::from_millis(3);

/// How a waiting stretch looks again for its own stop (a hide mid-flight).
const POLL: Duration = Duration::from_millis(4);

/// Where one stretch of a convoy is; ordered (a later one counts as past every earlier one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Leg {
    /// Planned; its landing Space not shown yet.
    Planned,
    /// Its landing Space shown.
    Shown,
    /// On its Desktop, or never to land in a landing Space (skipped, stopped): nothing behind
    /// waits for it any more.
    Gone,
}

/// A switch's stretches, by rank (0 the front).
#[derive(Debug)]
pub struct Convoy {
    legs: Mutex<Vec<Leg>>,
    moved: Condvar,
}

impl PartialEq for Convoy {
    /// The same convoy (one per switch).
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

/// How a wait on the windows behind ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waited {
    /// Every window behind was there (`waited`: how long that took).
    Ready(Duration),
    /// The bound passed first.
    TimedOut,
    /// The stretch itself was stopped meanwhile.
    Stopped,
}

impl Convoy {
    /// A convoy of `count` stretches, each planned.
    pub fn new(count: usize) -> Self {
        Self { legs: Mutex::new(vec![Leg::Planned; count]), moved: Condvar::new() }
    }

    /// How many stretches fly together.
    pub fn count(&self) -> usize {
        self.legs().len()
    }

    fn legs(&self) -> std::sync::MutexGuard<'_, Vec<Leg>> {
        self.legs.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Stretch `rank` is at `leg` now (never back to an earlier one).
    pub fn mark(&self, rank: usize, leg: Leg) {
        let mut legs = self.legs();
        if let Some(l) = legs.get_mut(rank)
            && leg > *l
        {
            *l = leg;
            self.moved.notify_all();
        }
    }

    /// Where stretch `rank` is.
    pub fn leg(&self, rank: usize) -> Option<Leg> {
        self.legs().get(rank).copied()
    }

    /// Waits until every stretch behind `rank` is at `leg` or past it, until `by` at most;
    /// `stopped` is asked every few ms (a hide mid-flight ends the wait).
    pub fn wait_behind(&self, rank: usize, leg: Leg, by: Instant, stopped: impl Fn() -> bool) -> Waited {
        let began = Instant::now();
        let mut legs = self.legs();
        loop {
            if behind_at(&legs, rank, leg) {
                return Waited::Ready(began.elapsed());
            }
            let now = Instant::now();
            if now >= by {
                return Waited::TimedOut;
            }
            drop(legs);
            if stopped() {
                return Waited::Stopped;
            }
            legs = self.legs();
            if behind_at(&legs, rank, leg) {
                continue;
            }
            let (l, _) = self.moved.wait_timeout(legs, POLL.min(by - now)).unwrap_or_else(|p| p.into_inner());
            legs = l;
        }
    }
}

/// The convoy of a switch whose `count` stretches fly: `None` for one alone (it waits on
/// nothing, as before this round).
pub fn convoy_for(count: usize) -> Option<Arc<Convoy>> {
    (count > 1).then(|| Arc::new(Convoy::new(count)))
}

/// True when every stretch behind `rank` (a higher rank) is at `leg` or past it.
pub fn behind_at(legs: &[Leg], rank: usize, leg: Leg) -> bool {
    legs.iter().skip(rank + 1).all(|l| *l >= leg)
}

/// Each window of `flying` with its rank among them (0 the front), by its place in `order`
/// (the stage's windows front to back as they end); one `order` does not name goes behind
/// those it names, in the order given.
pub fn ranks(order: &[u32], flying: &[u32]) -> Vec<(u32, usize)> {
    let place = |w: &u32| order.iter().position(|x| x == w).unwrap_or(order.len());
    let mut sorted: Vec<(usize, usize, u32)> = flying.iter().enumerate().map(|(i, w)| (place(w), i, *w)).collect();
    sorted.sort();
    let rank_of = |w: u32| sorted.iter().position(|(_, _, x)| *x == w).unwrap_or(0);
    flying.iter().map(|w| (*w, rank_of(*w))).collect()
}

/// The handover instants of a convoy's windows, each given as (its rank, its own instant in
/// seconds on one clock): each moved no earlier than the latest of those behind it (a higher
/// rank), in the order given ([`handovers_back_first`] in the module notes).
pub fn handovers_back_first(own: &[(usize, f64)]) -> Vec<f64> {
    own.iter().map(|&(rank, at)| own.iter().filter(|(r, _)| *r >= rank).map(|(_, a)| *a).fold(at, f64::max)).collect()
}

/// How much later than the back one stretch `rank` of `count` sets each step's transform:
/// the front last, [`STAGGER`] a rank, [`STAGGER_MAX`] in all; nothing for one alone.
pub fn stagger(rank: usize, count: usize) -> Duration {
    if count < 2 || rank >= count {
        return Duration::ZERO;
    }
    let per = STAGGER.min(STAGGER_MAX / (count as u32 - 1));
    per * (count - 1 - rank) as u32
}

/// Which level the landing Spaces are given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelMode {
    /// The level the window server makes them with (see the module notes).
    AsMade,
    /// 1 for the back, one more a window in front (the test switch only).
    Ranked,
}

/// The environment variable of the test switch's [`LevelMode`] (honoured as
/// `STAGE_MANAGER_LANDING` is).
pub const LEVEL_ENV: &str = "STAGE_MANAGER_LANDING_LEVEL";

impl LevelMode {
    /// The mode `word` names: `as-made`, `ranked`.
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim() {
            "as-made" => Some(Self::AsMade),
            "ranked" => Some(Self::Ranked),
            _ => None,
        }
    }

    /// The mode asked for: honoured only in a test or review run (`gated`); else as made.
    pub fn from(gated: bool, word: Option<&str>) -> Self {
        if !gated {
            return Self::AsMade;
        }
        word.and_then(Self::parse).unwrap_or(Self::AsMade)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::AsMade => "as-made",
            Self::Ranked => "ranked",
        }
    }
}

/// The level landing Space `rank` of `count` is given, `None` to leave it as made (one alone
/// never has its level set).
pub fn level(mode: LevelMode, rank: usize, count: usize) -> Option<i32> {
    match mode {
        LevelMode::Ranked if count >= 2 && rank < count => i32::try_from(count - rank).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ranks follow the stage's order front to back, whatever order the windows are planned
    /// in; a window the order does not name goes behind; one alone is the front.
    /// 11-16 10:43:04: Messages (rank 0) would take over at 390 ms, Telegram behind it
    /// (rank 1) at 408 ms; Messages waits for it. One behind already earlier changes nothing.
    #[test]
    fn the_front_takes_over_no_earlier_than_those_behind() {
        assert_eq!(handovers_back_first(&[(1, 0.408), (0, 0.390)]), [0.408, 0.408]);
        assert_eq!(handovers_back_first(&[(0, 0.500), (1, 0.400)]), [0.500, 0.400]);
        assert_eq!(handovers_back_first(&[(2, 0.3), (0, 0.1), (1, 0.2)]), [0.3, 0.3, 0.3]);
        assert_eq!(handovers_back_first(&[(0, 0.4)]), [0.4], "one alone");
    }

    #[test]
    fn ranks_follow_the_stage_order() {
        assert_eq!(ranks(&[114, 21102], &[21102, 114]), vec![(21102, 1), (114, 0)]);
        assert_eq!(ranks(&[5, 6, 7], &[7, 5]), vec![(7, 1), (5, 0)], "ranks among the windows that fly");
        assert_eq!(ranks(&[5], &[9, 5, 8]), vec![(9, 1), (5, 0), (8, 2)], "unnamed ones behind, as given");
        assert_eq!(ranks(&[1, 2], &[2]), vec![(2, 0)]);
        assert!(ranks(&[1], &[]).is_empty());
    }

    /// One stretch alone flies as before: no convoy, rank 0, no stagger, no level.
    #[test]
    fn one_window_alone_changes_nothing() {
        assert!(convoy_for(0).is_none() && convoy_for(1).is_none());
        assert_eq!(convoy_for(2).map(|c| c.count()), Some(2));
        assert_eq!(ranks(&[3, 4], &[4]), vec![(4, 0)]);
        assert_eq!(stagger(0, 1), Duration::ZERO);
        assert_eq!(level(LevelMode::Ranked, 0, 1), None);
    }

    /// The front sets its step last; nothing is staggered for one window; the spread is bounded.
    #[test]
    fn the_front_is_staggered_last() {
        assert_eq!(stagger(0, 1), Duration::ZERO, "one alone: as before");
        assert_eq!(stagger(1, 2), Duration::ZERO, "the back first");
        assert_eq!(stagger(0, 2), STAGGER, "the front after it");
        assert!(stagger(0, 3) > stagger(1, 3) && stagger(1, 3) > stagger(2, 3));
        assert!(stagger(0, 10) <= STAGGER_MAX, "bounded: {:?}", stagger(0, 10));
        assert_eq!(stagger(5, 2), Duration::ZERO, "out of range: nothing");
    }

    /// Levels: none as made or for one window; ranked, the front highest and the back at 1.
    #[test]
    fn levels_rank_the_front_highest() {
        assert_eq!(level(LevelMode::AsMade, 0, 2), None);
        assert_eq!(level(LevelMode::Ranked, 0, 1), None, "one alone: as before");
        assert_eq!(level(LevelMode::Ranked, 0, 2), Some(2));
        assert_eq!(level(LevelMode::Ranked, 1, 2), Some(1));
        assert_eq!(level(LevelMode::Ranked, 2, 2), None);
        assert_eq!(LevelMode::from(false, Some("ranked")), LevelMode::AsMade, "not a test run: as made");
        assert_eq!(LevelMode::from(true, Some("ranked")), LevelMode::Ranked);
        assert_eq!(LevelMode::from(true, Some("nonsense")), LevelMode::AsMade);
        assert_eq!(LevelMode::from(true, None), LevelMode::AsMade);
    }

    /// A front window that finished first waits for the one behind to land; the back one
    /// never waits for the front.
    #[test]
    fn a_front_window_lands_after_the_one_behind() {
        let c = Convoy::new(2);
        let soon = Instant::now() + Duration::from_millis(5);
        assert!(matches!(c.wait_behind(1, Leg::Gone, soon, || false), Waited::Ready(_)), "the back: nothing behind it");
        assert_eq!(c.wait_behind(0, Leg::Gone, soon, || false), Waited::TimedOut, "the back still flies: bounded");
        c.mark(1, Leg::Gone);
        assert!(matches!(c.wait_behind(0, Leg::Gone, Instant::now(), || false), Waited::Ready(_)));
    }

    /// A back window finishing late: the front waits for it across threads and goes the moment
    /// it lands.
    #[test]
    fn a_back_window_finishing_late_is_waited_for() {
        let c = Arc::new(Convoy::new(2));
        let back = {
            let c = c.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                c.mark(1, Leg::Gone);
            })
        };
        let began = Instant::now();
        let w = c.wait_behind(0, Leg::Gone, Instant::now() + LAND_WAIT, || false);
        back.join().expect("joined");
        assert!(matches!(w, Waited::Ready(d) if d >= Duration::from_millis(25)), "{w:?}");
        assert!(began.elapsed() < LAND_WAIT, "it went as the back landed: {:?}", began.elapsed());
    }

    /// A stop (a hide mid-flight) ends the wait; legs never go back; a skipped one counts as
    /// landed for those in front; showing waits only for Spaces behind to be shown.
    #[test]
    fn the_gate_ends_on_a_stop_and_legs_only_move_on() {
        let c = Convoy::new(3);
        assert_eq!(c.wait_behind(0, Leg::Gone, Instant::now() + Duration::from_secs(5), || true), Waited::Stopped);
        c.mark(2, Leg::Gone);
        c.mark(2, Leg::Shown);
        assert_eq!(c.leg(2), Some(Leg::Gone), "never back");
        c.mark(1, Leg::Shown);
        assert!(matches!(c.wait_behind(0, Leg::Shown, Instant::now(), || false), Waited::Ready(_)), "a skipped one is past shown");
        assert_eq!(c.wait_behind(0, Leg::Gone, Instant::now(), || false), Waited::TimedOut, "rank 1 has not landed");
        assert!(behind_at(&[Leg::Planned, Leg::Gone], 0, Leg::Gone));
        assert!(!behind_at(&[Leg::Gone, Leg::Planned], 0, Leg::Shown));
        assert!(behind_at(&[Leg::Planned], 0, Leg::Gone), "nothing behind");
        c.mark(9, Leg::Gone);
        assert_eq!(c.count(), 3, "an unknown rank changes nothing");
    }
}
