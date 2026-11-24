//! The Spaces engine's companions: a window in a stage Space that the engine does not hold
//! but that goes with a window it does (an app's attached side panel, a child window that
//! Accessibility does not list). The window server moves a window and its attached child
//! windows between Spaces together: hiding the held window takes the child into the stage
//! Space with it, and letting the child out onto the Desktop drags the held window out too
//! (live 11-18: a calendar's side panel let out every 10 s pulled its main window onto the
//! Desktop, which the tick hid again, for minutes).
//!
//! No call the engine loads names a window's parent, so the relation is the app's: a window
//! the model does not know, in a stage Space where a window of the same process is held,
//! stays there ([`Companions::stays`]). It goes with its stage: the window server hides and
//! shows it with its parent, and every bring-back (quit, crash, the guardian, a fallback)
//! takes everything out of the stage Spaces by the window server's own list.
//!
//! The loop is also stopped in general: a let-out that pulls a held window out of its Space
//! within [`PULL_WINDOW`] ([`Companions::escaped`]) is not repeated while that window is
//! held there again.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// How soon after a let-out a held window seen out of the same stage Space counts as pulled
/// out by it (live 11-18: the tick saw it ~240 ms after).
pub const PULL_WINDOW: Duration = Duration::from_secs(1);

/// A window the engine holds, as the companions' rule sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldWindow {
    pub id: u32,
    pub pid: i32,
    /// The stage Space it is held in.
    pub space: u64,
}

/// Why a window not held stays in its stage Space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stay {
    /// A window of its process is held in that Space: it goes with it.
    Sibling(u32),
    /// Its let-out pulled this held window out of that Space before.
    Pulled(u32),
}

/// What the sweep for windows nothing answers for remembers between rounds.
#[derive(Debug, Default)]
pub struct Companions {
    /// Windows let out lately, with when and from which stage Space.
    let_out: BTreeMap<u32, (Instant, u64)>,
    /// Windows whose let-out pulled held windows out of their stage Space: the Space, and the
    /// windows pulled.
    pulls: BTreeMap<u32, (u64, BTreeSet<u32>)>,
    /// Windows already said to stay (each said once while it stays).
    said: BTreeSet<u32>,
}

impl Companions {
    /// Why window `id` of process `pid`, found in stage Space `space` and not held, stays
    /// there; `None` when it is let out. `known`: the model has the window (Accessibility
    /// answers for it), so it is never a companion: one the model shows is let out.
    pub fn stays(&self, id: u32, pid: i32, space: u64, known: bool, held: &[HeldWindow]) -> Option<Stay> {
        if known {
            return None;
        }
        if let Some(h) = held.iter().find(|h| h.pid == pid && h.space == space) {
            return Some(Stay::Sibling(h.id));
        }
        let (pulled_from, pulled) = self.pulls.get(&id)?;
        if *pulled_from != space {
            return None;
        }
        held.iter().find(|h| h.space == space && pulled.contains(&h.id)).map(|h| Stay::Pulled(h.id))
    }

    /// True the first time window `id` is said to stay (since it last did not).
    pub fn say_once(&mut self, id: u32) -> bool {
        self.said.insert(id)
    }

    /// A sweep over every stage Space found `stayed` staying: the others are said again when
    /// they next stay.
    pub fn settle(&mut self, stayed: &BTreeSet<u32>) {
        self.said.retain(|id| stayed.contains(id));
    }

    /// Window `id` let out of stage Space `space` at `now`.
    pub fn let_out(&mut self, id: u32, space: u64, now: Instant) {
        self.said.remove(&id);
        self.let_out.insert(id, (now, space));
    }

    /// Held windows seen out of their stage Space at `now` (each with that Space): a window
    /// let out of the same Space within [`PULL_WINDOW`] before pulled them out. The pulls
    /// noted for the first time, as (let out, pulled), for the log.
    pub fn escaped(&mut self, out: &[(u32, u64)], now: Instant) -> Vec<(u32, u32)> {
        self.let_out.retain(|_, (t, _)| {
            let since = now.saturating_duration_since(*t);
            crate::engine::margin::note("PULL_WINDOW", since, PULL_WINDOW, since <= PULL_WINDOW)
        });
        let mut noted = Vec::new();
        for (held, space) in out.iter().copied() {
            for (id, (_, from)) in &self.let_out {
                if *from != space {
                    continue;
                }
                let entry = self.pulls.entry(*id).or_insert_with(|| (space, BTreeSet::new()));
                if entry.0 != space {
                    *entry = (space, BTreeSet::new());
                }
                if entry.1.insert(held) {
                    noted.push((*id, held));
                }
            }
        }
        noted
    }

    /// Forgets the windows `keep` says are gone.
    pub fn retain(&mut self, keep: impl Fn(u32) -> bool) {
        self.let_out.retain(|id, _| keep(*id));
        self.pulls.retain(|id, _| keep(*id));
        self.said.retain(|id| keep(*id));
    }

    /// Forgets everything (the stage Spaces ended).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The windows whose let-out pulled a held window out, for `ctl state`.
    pub fn pulls(&self) -> Vec<u32> {
        self.pulls.keys().copied().collect()
    }

    /// The windows said to stay now, for `ctl state`.
    pub fn staying(&self) -> Vec<u32> {
        self.said.iter().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: u32 = 7;
    const MAIN: u32 = 1;
    const APP: i32 = 80;
    const STAGE: u64 = 55;

    fn held(id: u32, pid: i32, space: u64) -> HeldWindow {
        HeldWindow { id, pid, space }
    }

    #[test]
    fn a_window_with_a_held_sibling_in_its_space_stays() {
        let c = Companions::default();
        assert_eq!(c.stays(PANEL, APP, STAGE, false, &[held(MAIN, APP, STAGE)]), Some(Stay::Sibling(MAIN)));
    }

    #[test]
    fn a_window_with_no_held_sibling_in_its_space_is_let_out() {
        let c = Companions::default();
        assert_eq!(c.stays(PANEL, APP, STAGE, false, &[]), None, "nothing held");
        assert_eq!(c.stays(PANEL, APP, STAGE, false, &[held(MAIN, APP, STAGE + 1)]), None, "its sibling held in another stage's Space");
        assert_eq!(c.stays(PANEL, APP, STAGE, false, &[held(MAIN, APP + 1, STAGE)]), None, "another app's window held there");
    }

    #[test]
    fn a_window_the_model_knows_is_never_a_companion() {
        let c = Companions::default();
        assert_eq!(c.stays(PANEL, APP, STAGE, true, &[held(MAIN, APP, STAGE)]), None);
    }

    #[test]
    fn a_let_out_that_pulled_a_held_window_out_is_not_repeated() {
        let mut c = Companions::default();
        let t = Instant::now();
        // Another process's window (no sibling rule): let out, and the held window seen out
        // of the same Space soon after.
        let other = 81;
        c.let_out(PANEL, STAGE, t);
        assert_eq!(c.escaped(&[(MAIN, STAGE)], t + Duration::from_millis(240)), vec![(PANEL, MAIN)]);
        assert_eq!(c.escaped(&[(MAIN, STAGE)], t + Duration::from_millis(300)), vec![], "noted once");
        // Hidden again (held there): the window stays.
        assert_eq!(c.stays(PANEL, other, STAGE, false, &[held(MAIN, APP, STAGE)]), Some(Stay::Pulled(MAIN)));
        // Shown (not held any more): let out.
        assert_eq!(c.stays(PANEL, other, STAGE, false, &[]), None);
    }

    #[test]
    fn a_held_window_out_long_after_a_let_out_was_not_pulled() {
        let mut c = Companions::default();
        let t = Instant::now();
        c.let_out(PANEL, STAGE, t);
        assert_eq!(c.escaped(&[(MAIN, STAGE)], t + PULL_WINDOW + Duration::from_millis(1)), vec![]);
        assert_eq!(c.stays(PANEL, 81, STAGE, false, &[held(MAIN, APP, STAGE)]), None);
    }

    #[test]
    fn a_held_window_out_of_another_space_was_not_pulled() {
        let mut c = Companions::default();
        let t = Instant::now();
        c.let_out(PANEL, STAGE, t);
        assert_eq!(c.escaped(&[(MAIN, STAGE + 1)], t + Duration::from_millis(100)), vec![]);
    }

    #[test]
    fn staying_is_said_once_until_let_out() {
        let mut c = Companions::default();
        assert!(c.say_once(PANEL));
        assert!(!c.say_once(PANEL));
        c.let_out(PANEL, STAGE, Instant::now());
        assert!(c.say_once(PANEL));
        c.settle(&BTreeSet::new());
        assert!(c.staying().is_empty(), "a sweep that found it gone from the stage Space");
        assert!(c.say_once(PANEL), "said again when it next stays");
    }

    #[test]
    fn gone_windows_are_forgotten() {
        let mut c = Companions::default();
        let t = Instant::now();
        c.let_out(PANEL, STAGE, t);
        let _ = c.escaped(&[(MAIN, STAGE)], t);
        c.retain(|id| id != PANEL);
        assert!(c.pulls().is_empty());
        assert_eq!(c.stays(PANEL, 81, STAGE, false, &[held(MAIN, APP, STAGE)]), None);
    }
}
