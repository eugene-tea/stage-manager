//! The handoff of a guardian's watch (round 8N), decided without touching anything.
//!
//! A guardian whose app died unclean waits for every window that app had registered until it
//! has seen each one back or gone ([`super::guard_watch`]). A window it can never see again (a
//! helper window off screen, never shown) kept one waiting for days (10-19…10-21), asking the
//! running copy `adopted` every few seconds, and blocking every live test. So when a new
//! Stage Manager runs and answers `adopted` with a list (it manages windows), the guardian
//! offers it the windows it still waits for ([`Offer`], the ctl command [`COMMAND`]): their
//! ids, pids and restore frames, and the park corners the crashed app used. The new app adds
//! them to its own safety registry ([`take`]): from then on its quit, its own guardian (the
//! parked file names them) and its watch of the windows it took bring them back, as the old
//! guardian would have. The old guardian lets go of each window taken and exits once none is
//! left. Nothing is dropped: the windows only change hands.
//!
//! When the new app does not take them (an earlier version that does not know the command,
//! a quit under way, an error), the guardian goes on watching as before and offers again
//! [`REOFFER_AFTER`] later ([`Offering`]); when it does not answer at all (gone, or quitting
//! before it could take), at the guardian's next question. Two old guardians each hand over
//! their own windows; a window offered twice keeps its first entry.
//!
//! A handed-over window never travels from copy to copy forever ([`TakenDrops`], the owner's
//! word on 8N): the app drops it from its taken windows, logged once at info, when its app's
//! process no longer runs, or when the window server's complete list no longer has it on two
//! looks at least [`LOOK_APART`] apart (8M's rule, [`super::vanished`]). One that still exists
//! and cannot be reached at a quit is passed on to the app's guardian as before (a warning,
//! never the error of a window the app parked itself).

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::park::Corner;
use super::recovery::Registered;
use super::vanished::Vanished;
pub use super::vanished::LOOK_APART;

/// The ctl command a guardian hands its watch over with: `take-watch <offer as JSON>`.
pub const COMMAND: &str = "take-watch";

/// After a refusal the watch is offered again this much later: another version may run by
/// then.
pub const REOFFER_AFTER: Duration = Duration::from_secs(60);

/// A guardian's watch, as it hands it over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    /// The crashed app's process id (for the log).
    pub app: u32,
    /// The guardian's own process id (for the log).
    pub guardian: u32,
    /// Every park corner the crashed app used: a window may still sit at one the new app does
    /// not pick.
    #[serde(default)]
    pub corners: Vec<Corner>,
    /// The windows the guardian still waits for: ids, pids, restore frames.
    pub windows: Vec<Registered>,
}

impl Offer {
    /// The request line: [`COMMAND`] and the offer as one line of JSON.
    pub fn line(&self) -> Result<String, String> {
        serde_json::to_string(self).map(|json| format!("{COMMAND} {json}")).map_err(|e| e.to_string())
    }

    /// The offer from the words after [`COMMAND`] (the ctl parser splits the line on
    /// whitespace; JSON written by [`Self::line`] has none inside its strings).
    pub fn parse(words: &[&str]) -> Result<Self, String> {
        serde_json::from_str(&words.join(" ")).map_err(|e| format!("take-watch: {e}"))
    }
}

/// What the running app said to an offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// It took these windows (every one offered, normally); `pid`: its process id.
    Taken {
        /// The windows it holds now.
        ids: Vec<u32>,
        /// Its process id, when it said.
        pid: Option<u32>,
    },
    /// It answered but did not take them (an earlier version, a quit under way, an error).
    Refused(String),
    /// No answer (gone, busy, or a reply that cannot be read).
    Unanswered,
}

/// A reply to [`COMMAND`] as an [`Answer`].
pub fn answer_from_reply(reply: &str) -> Answer {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(reply) else { return Answer::Unanswered };
    let ok = v.get("ok").and_then(serde_json::Value::as_bool);
    if ok == Some(true)
        && let Some(list) = v.get("taken").and_then(serde_json::Value::as_array)
    {
        let ids = list.iter().filter_map(serde_json::Value::as_u64).filter_map(|x| u32::try_from(x).ok()).collect();
        let pid = v.get("pid").and_then(serde_json::Value::as_u64).and_then(|p| u32::try_from(p).ok());
        return Answer::Taken { ids, pid };
    }
    match (ok, v.get("error").and_then(serde_json::Value::as_str)) {
        (Some(false), Some(e)) => Answer::Refused(e.to_string()),
        _ => Answer::Unanswered,
    }
}

/// The taking side: adds `offered` to the windows `held` (a window held already keeps its
/// entry: the first offer wins). Returns every offered id (all are held now) and how many
/// were new.
pub fn take(held: &mut BTreeMap<u32, Registered>, offered: &[Registered]) -> (Vec<u32>, usize) {
    let mut new = 0;
    let mut ids = Vec::with_capacity(offered.len());
    for r in offered {
        if let std::collections::btree_map::Entry::Vacant(slot) = held.entry(r.id) {
            slot.insert(*r);
            new += 1;
        }
        if !ids.contains(&r.id) {
            ids.push(r.id);
        }
    }
    (ids, new)
}

/// The guardian's side: when an offer is due.
#[derive(Debug, Clone, Default)]
pub struct Offering {
    /// The latest refusal.
    refused_at: Option<Instant>,
    /// A refusal was logged (once per guardian).
    told: bool,
}

impl Offering {
    /// True when the watch may be offered at `now`: never refused, or refused at least
    /// [`REOFFER_AFTER`] ago.
    pub fn due(&self, now: Instant) -> bool {
        self.refused_at.is_none_or(|t| now.duration_since(t) >= REOFFER_AFTER)
    }

    /// Notes a refusal at `now`; true the first time (log it then).
    pub fn refused(&mut self, now: Instant) -> bool {
        self.refused_at = Some(now);
        !std::mem::replace(&mut self.told, true)
    }
}

/// Why a handed-over window was dropped from the windows taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    /// Its app's process no longer runs.
    AppGone,
    /// The window server's complete list no longer has it (two looks [`LOOK_APART`] apart).
    Vanished,
}

impl fmt::Display for Dropped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AppGone => write!(f, "its app no longer runs"),
            Self::Vanished => write!(f, "the window server no longer lists it (closed)"),
        }
    }
}

/// The drop rules for handed-over windows, look by look.
#[derive(Debug, Clone, Default)]
pub struct TakenDrops {
    vanished: Vanished,
}

impl TakenDrops {
    /// One look at `now` at the windows `taken`: `listed`, the ids of the window server's
    /// complete list (every Desktop, ordered-out windows included); `blind`, that list shows no
    /// window on screen (no observation: nothing vanishes); `running`, whether a process runs.
    /// Returns the windows to drop, each once, with why.
    pub fn look(
        &mut self,
        now: Instant,
        taken: &[Registered],
        listed: &HashSet<u32>,
        blind: bool,
        running: impl Fn(i32) -> bool,
    ) -> Vec<(Registered, Dropped)> {
        let mut dropped = Vec::new();
        let mut alive = Vec::new();
        for r in taken {
            // A pid never read (-1) says nothing: only the window list judges that window.
            if r.pid > 0 && !running(r.pid) {
                dropped.push((*r, Dropped::AppGone));
            } else {
                alive.push(r.id);
            }
        }
        for id in self.vanished.look(now, alive, listed, blind) {
            if let Some(r) = taken.iter().find(|r| r.id == id) {
                dropped.push((*r, Dropped::Vanished));
            }
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Rect;

    fn reg(id: u32, x: f64) -> Registered {
        Registered { id, pid: 7, restore: Rect::new(x, 200.0, 600.0, 400.0), child: false, fitted: false }
    }

    fn offer(windows: Vec<Registered>) -> Offer {
        Offer { app: 41, guardian: 42, corners: vec![Corner::BottomRight], windows }
    }

    /// The line goes through the ctl parser's whitespace split and comes back the same.
    #[test]
    fn an_offer_survives_the_ctl_line() {
        let o = offer(vec![reg(34246, 300.5), Registered { fitted: true, child: true, ..reg(9, 10.0) }]);
        let line = o.line().expect("serializes");
        let words: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(words[0], COMMAND);
        assert_eq!(Offer::parse(&words[1..]), Ok(o));
        assert!(Offer::parse(&["{\"app\":1}"]).is_err(), "no windows: not an offer");
        assert!(Offer::parse(&["garbage"]).is_err());
    }

    /// An earlier file's offer without corners still parses (nothing at the old corners).
    #[test]
    fn corners_are_optional() {
        let o = Offer::parse(&[r#"{"app":1,"guardian":2,"windows":[]}"#]).expect("parses");
        assert!(o.corners.is_empty() && o.windows.is_empty());
    }

    #[test]
    fn the_new_apps_answer() {
        assert_eq!(answer_from_reply(r#"{"ok":true,"taken":[4,9],"pid":77}"#), Answer::Taken { ids: vec![4, 9], pid: Some(77) });
        assert_eq!(answer_from_reply(r#"{"ok":true,"taken":[]}"#), Answer::Taken { ids: vec![], pid: None });
        // An earlier version: the ctl parser's word.
        assert_eq!(
            answer_from_reply(r#"{"ok":false,"error":"unknown command: take-watch {}"}"#),
            Answer::Refused("unknown command: take-watch {}".into())
        );
        assert_eq!(answer_from_reply(r#"{"ok":false,"error":"quitting"}"#), Answer::Refused("quitting".into()));
        for reply in ["", "garbage", r#"{"ok":true}"#, r#"{"ok":false}"#, r#"{"taken":[4]}"#] {
            assert_eq!(answer_from_reply(reply), Answer::Unanswered, "{reply}");
        }
    }

    /// Two old guardians hand over their own windows; a window offered twice keeps its first
    /// entry, and each guardian hears that every window it offered is held.
    #[test]
    fn two_guardians_hand_over_into_one_registry() {
        let mut held = BTreeMap::new();
        assert_eq!(take(&mut held, &[reg(1, 100.0), reg(2, 100.0)]), (vec![1, 2], 2));
        assert_eq!(take(&mut held, &[reg(2, 900.0), reg(3, 100.0), reg(3, 100.0)]), (vec![2, 3], 1));
        assert_eq!(held.keys().copied().collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(held[&2].restore.x, 100.0, "the first offer's frame");
        assert_eq!(take(&mut held, &[]), (vec![], 0));
    }

    /// The owner's word on 8N: a handed-over window whose app quit is dropped at the first
    /// look; a pid never read is judged by the window list alone.
    #[test]
    fn a_taken_window_whose_app_quit_is_dropped() {
        let t0 = Instant::now();
        let gone = Registered { pid: 900, ..reg(1, 100.0) };
        let unknown = Registered { pid: -1, ..reg(3, 100.0) };
        let listed: HashSet<u32> = [1, 2, 3].into();
        let mut d = TakenDrops::default();
        let out = d.look(t0, &[gone, reg(2, 100.0), unknown], &listed, false, |pid| pid != 900);
        assert_eq!(out, vec![(gone, Dropped::AppGone)]);
        let out = d.look(t0, &[reg(2, 100.0), unknown], &listed, false, |_| false);
        assert_eq!(out, vec![(reg(2, 100.0), Dropped::AppGone)], "pid -1 is never judged by its pid");
    }

    /// 8M's rule: missing from the complete list on two looks at least half a second apart,
    /// listed on neither between; one look, a blind look or a sighting between start over.
    #[test]
    fn a_taken_window_the_window_server_no_longer_lists_is_dropped() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let taken = [reg(1, 100.0), reg(2, 100.0)];
        let with: HashSet<u32> = [1, 2].into();
        let without: HashSet<u32> = [2].into();
        let mut d = TakenDrops::default();
        let run = |_| true;
        assert!(d.look(ms(0), &taken, &without, false, run).is_empty(), "one look never drops");
        assert!(d.look(ms(400), &taken, &without, false, run).is_empty(), "too soon");
        assert_eq!(d.look(ms(500), &taken, &without, false, run), vec![(taken[0], Dropped::Vanished)]);
        // Listed again between: the count starts over.
        let mut d = TakenDrops::default();
        d.look(ms(0), &taken, &without, false, run);
        d.look(ms(300), &taken, &with, false, run);
        assert!(d.look(ms(600), &taken, &without, false, run).is_empty());
        assert_eq!(d.look(ms(1100), &taken, &without, false, run).len(), 1);
        // A blind look is no observation.
        let mut d = TakenDrops::default();
        d.look(ms(0), &taken, &without, false, run);
        assert!(d.look(ms(600), &taken, &HashSet::new(), true, run).is_empty());
        assert!(d.look(ms(700), &taken, &without, false, run).is_empty(), "starts over after the blind look");
        assert_eq!(d.look(ms(1200), &taken, &without, false, run).len(), 1);
    }

    /// A refusal (an earlier version) holds the next offer off for a minute; logged once.
    #[test]
    fn a_refusal_holds_offers_off_for_a_while() {
        let t0 = Instant::now();
        let mut o = Offering::default();
        assert!(o.due(t0));
        assert!(o.refused(t0), "the first refusal is logged");
        assert!(!o.due(t0 + Duration::from_secs(59)));
        assert!(o.due(t0 + REOFFER_AFTER));
        assert!(!o.refused(t0 + REOFFER_AFTER), "not logged again");
        assert!(!o.due(t0 + REOFFER_AFTER + Duration::from_secs(1)));
    }
}
