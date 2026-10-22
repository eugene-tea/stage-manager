//! Which windows a batch moves on or off the screen, and the card slot each one leaves into
//! or arrives from: decided once, from the engine, so the strip aims its snapshots at the
//! slot the engine gives the card (the one source of truth for "where it lands").
//!
//! The controller notes [`Before`] after every batch; when the next batch parks shown
//! windows or brings parked ones back, [`plan`] turns it into a [`Transition`]: outgoing
//! windows at their live frames, each with the slot its stage's card has **after** the change
//! (Apple's swap: the slot the clicked card vacated), incoming windows at their arrival
//! frames, each with the slot its card had **before** (where it is drawn when the motion
//! starts).

use std::collections::{HashMap, HashSet};

use crate::engine::geometry::Rect;
use crate::engine::model::{Action, Home, Model, StripCard};
use crate::ui::strip_model::{CardWindow, MovingWindow, Transition};

/// The strip's cards as slots: (stage, its windows), top to bottom.
pub type Slots = Vec<(u64, Vec<u32>)>;

/// The cards of the main display's strip, as [`Slots`].
pub fn slots_of(m: &Model) -> Slots {
    m.strip().iter().map(card_slot).collect()
}

fn card_slot(c: &StripCard) -> (u64, Vec<u32>) {
    (c.stage.0, c.windows.iter().map(|w| w.0).collect())
}

/// The slot of the card showing window `id` (else the card of `stage`), and that card's stage.
pub fn slot_of(slots: &Slots, id: u32, stage: Option<u64>) -> Option<(usize, u64)> {
    slots
        .iter()
        .position(|(_, ws)| ws.contains(&id))
        .or_else(|| slots.iter().position(|(s, _)| Some(*s) == stage))
        .map(|i| (i, slots[i].0))
}

/// What the strip and the windows were after the last batch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Before {
    /// Windows the model had parked.
    pub parked: HashSet<u32>,
    /// Every window's stage.
    pub homes: HashMap<u32, u64>,
    /// The strip's cards.
    pub slots: Slots,
}

impl Before {
    /// Notes the model as it is now.
    pub fn of(m: &Model) -> Self {
        Self {
            parked: m.windows().filter(|w| w.parked).map(|w| w.meta.id.0).collect(),
            homes: m
                .windows()
                .filter_map(|w| match w.home {
                    Home::Stage(s) => Some((w.meta.id.0, s.0)),
                    Home::Pinned => None,
                })
                .collect(),
            slots: slots_of(m),
        }
    }
}

/// The transition `id` for `actions` (already applied to `m`), if they park shown windows or
/// bring parked ones back. `live` gives a window's frame on screen now (falls back to the
/// model's). Outgoing windows are listed back to front (parks come front to back).
pub fn plan(m: &Model, actions: &[Action], before: &Before, live: impl Fn(u32) -> Option<Rect>, id: u64) -> Option<Transition> {
    let after = slots_of(m);
    let mut outgoing = Vec::new();
    let mut incoming: Vec<MovingWindow> = Vec::new();
    for a in actions {
        match *a {
            Action::Park { id: w, .. } if !before.parked.contains(&w.0) => {
                let Some(state) = m.window(w) else { continue };
                let frame = live(w.0).unwrap_or(state.meta.frame);
                let stage = m.stage_of(w).map(|s| s.0);
                let card = slot_of(&after, w.0, stage);
                outgoing.push(MovingWindow {
                    window: CardWindow { id: w.0, pid: state.meta.pid, frame },
                    stage: card.map(|c| c.1).or(stage),
                    slot: card.map(|c| c.0),
                });
            }
            Action::SetFrame { id: w, frame } if before.parked.contains(&w.0) => {
                let Some(state) = m.window(w) else { continue };
                let stage = before.homes.get(&w.0).copied();
                let card = slot_of(&before.slots, w.0, stage);
                incoming.retain(|x| x.window.id != w.0);
                incoming.push(MovingWindow {
                    window: CardWindow { id: w.0, pid: state.meta.pid, frame },
                    stage: card.map(|c| c.1).or(stage),
                    slot: card.map(|c| c.0),
                });
            }
            _ => {}
        }
    }
    if outgoing.is_empty() && incoming.is_empty() {
        return None;
    }
    outgoing.reverse();
    Some(Transition { id, outgoing, incoming, sent: None, under: None, corner: None })
}

/// The stage ids of `slots`, top to bottom (for the logs).
pub fn order(slots: &Slots) -> Vec<u64> {
    slots.iter().map(|s| s.0).collect()
}

#[cfg(test)]
mod tests {
    //! One test per kind of switch: the outgoing windows' card slot in the transition is the
    //! slot their card has once the strip is laid out, and Apple's swap rule holds (every
    //! other card keeps its slot).

    use super::*;
    use crate::engine::model::{ModelOptions, Observation, StageId, WindowId};
    use crate::engine::rules::{Decision, Placement, RuleAction};
    use crate::engine::testkit::{Desk, meta, model_with, w};

    struct Env {
        m: Model,
        d: Desk,
        before: Before,
    }

    impl Env {
        /// `n` apps, one window and one stage each (window i, pid 100 + i), the last active.
        fn new(n: u32, capacity: usize) -> Self {
            let mut options = ModelOptions::default();
            options.strip.capacity = capacity;
            let mut e = Self { m: model_with(options), d: Desk::default(), before: Before::default() };
            for i in 1..=n {
                let action = if i == 1 { RuleAction::Current } else { RuleAction::New };
                e.add(i, 100 + i as i32, action);
            }
            e
        }

        fn add(&mut self, id: u32, pid: i32, action: RuleAction) -> Option<Transition> {
            let mt = meta(id, pid, &format!("com.example.app{pid}"));
            self.d.open(&mt);
            let obs = self.d.obs();
            let a = self.m.add_window(mt, &Decision { action, placement: Placement::AsIs }, &obs);
            self.apply(a)
        }

        fn obs(&self) -> Observation {
            self.d.obs()
        }

        /// Runs `actions` on the desk and plans their transition from the state before.
        fn apply(&mut self, actions: Vec<Action>) -> Option<Transition> {
            let frames = self.d.frames.clone();
            let t = plan(&self.m, &actions, &self.before, |id| frames.get(&WindowId(id)).copied(), 1);
            self.d.apply(&actions);
            self.before = Before::of(&self.m);
            t
        }

        fn order(&self) -> Vec<u64> {
            order(&slots_of(&self.m))
        }

        fn active(&self) -> u64 {
            self.m.main_active_stage().map(|s| s.0).expect("an active stage")
        }

        fn stage_of(&self, id: u32) -> u64 {
            self.m.stage_of(w(id)).map(|s| s.0).expect("in a stage")
        }

        fn click(&mut self, stage: u64) -> Option<Transition> {
            let obs = self.obs();
            let a = self.m.switch_to(StageId(stage), &obs);
            self.apply(a)
        }
    }

    /// Every outgoing window's slot is where its card is now, and every incoming window's
    /// slot is where its card was.
    fn assert_aims(e: &Env, t: &Transition, before: &[u64], what: &str) {
        let after = e.order();
        for g in &t.outgoing {
            let card = g.slot.map(|s| after[s]);
            assert_eq!(card, g.stage, "{what}: outgoing window {} aims at slot {:?} of {after:?}", g.window.id, g.slot);
            assert_eq!(g.slot, after.iter().position(|s| Some(*s) == g.stage), "{what}: outgoing slot");
        }
        for g in &t.incoming {
            assert_eq!(g.slot, before.iter().position(|s| Some(*s) == g.stage), "{what}: incoming slot");
        }
    }

    /// Apple's swap: `leaving` took slot `slot`; every other card kept its slot.
    fn assert_swap(before: &[u64], after: &[u64], slot: usize, leaving: u64, what: &str) {
        assert_eq!(after.len(), before.len(), "{what}: card count");
        assert_eq!(after[slot], leaving, "{what}: the stage left takes the vacated slot");
        for (i, (b, a)) in before.iter().zip(after).enumerate() {
            if i != slot {
                assert_eq!(a, b, "{what}: slot {i} kept its card ({before:?} -> {after:?})");
            }
        }
    }

    #[test]
    fn a_card_click_swaps_and_the_snapshot_aims_at_the_vacated_slot() {
        let mut e = Env::new(7, 5);
        // Click around in a mixed order, as he does.
        for pick in [3usize, 0, 4, 1, 2, 4, 0, 3, 1, 2, 0, 4] {
            let before = e.order();
            let leaving = e.active();
            let target = before[pick];
            let t = e.click(target).expect("a transition");
            let after = e.order();
            assert_swap(&before, &after, pick, leaving, &format!("click slot {pick}"));
            assert_aims(&e, &t, &before, &format!("click slot {pick}"));
            assert!(t.outgoing.iter().all(|g| g.slot == Some(pick) && g.stage == Some(leaving)));
            assert!(t.incoming.iter().all(|g| g.slot == Some(pick) && g.stage == Some(target)));
        }
    }

    #[test]
    fn a_dock_activation_swaps_like_a_click() {
        let mut e = Env::new(5, 6);
        let before = e.order();
        let leaving = e.active();
        let target = e.stage_of(2);
        let slot = before.iter().position(|s| *s == target).expect("a card");
        let obs = e.obs();
        let a = e.m.app_activated(102, &obs);
        let t = e.apply(a).expect("a transition");
        assert_eq!(e.active(), target);
        assert_swap(&before, &e.order(), slot, leaving, "dock");
        assert_aims(&e, &t, &before, "dock");
    }

    #[test]
    fn a_wallpaper_click_sends_the_stage_to_a_free_slot_or_the_least_recent_ones() {
        // A free slot: the bottom.
        let mut e = Env::new(3, 6);
        let before = e.order();
        let leaving = e.active();
        let obs = e.obs();
        let a = e.m.reveal_desktop(&obs);
        let t = e.apply(a).expect("a transition");
        let after = e.order();
        assert_eq!(after[..before.len()], before[..], "the cards stay");
        assert_eq!(after.last(), Some(&leaving));
        assert_aims(&e, &t, &before, "reveal, free slot");
        // No free slot: the least recently used card's slot.
        let mut e = Env::new(4, 3);
        let before = e.order();
        let leaving = e.active();
        let lru = *before
            .iter()
            .min_by_key(|s| e.m.stage(StageId(**s)).map(|s| s.last_active))
            .expect("cards");
        let slot = before.iter().position(|s| *s == lru).expect("slot");
        let obs = e.obs();
        let a = e.m.reveal_desktop(&obs);
        let t = e.apply(a).expect("a transition");
        assert_swap(&before, &e.order(), slot, leaving, "reveal, full strip");
        assert_aims(&e, &t, &before, "reveal, full strip");
    }

    #[test]
    fn a_new_stage_sends_the_stage_left_into_the_strip() {
        let mut e = Env::new(3, 6);
        let before = e.order();
        let leaving = e.active();
        let t = e.add(9, 200, RuleAction::Native).expect("a transition");
        let after = e.order();
        assert_eq!(after[..before.len()], before[..]);
        assert_eq!(after.last(), Some(&leaving));
        assert_aims(&e, &t, &before, "new stage");
        assert!(t.outgoing.iter().all(|g| g.slot == Some(after.len() - 1)));
    }

    #[test]
    fn cmd_m_sends_the_window_to_a_card_of_its_own_where_its_snapshot_lands() {
        let mut e = Env::new(3, 6);
        // A second window in the active stage, then ⌘M on it.
        e.add(8, 103, RuleAction::Current);
        let before = e.order();
        let obs = e.obs();
        let a = e.m.minimize_to_strip(w(8), &obs);
        let t = e.apply(a).expect("a transition");
        let after = e.order();
        let stage = e.stage_of(8);
        assert_eq!(after[..before.len()], before[..], "the cards stay");
        assert_eq!(after.last(), Some(&stage));
        assert_aims(&e, &t, &before, "cmd-m");
        assert_eq!(t.outgoing.len(), 1);
        assert_eq!(t.outgoing[0].slot, Some(after.len() - 1));
    }

    #[test]
    fn a_switch_to_a_stage_out_of_view_takes_the_least_recent_cards_slot() {
        let mut e = Env::new(6, 3);
        let before = e.order();
        let out = (1..=6u32).map(|i| e.stage_of(i)).find(|s| !before.contains(s) && *s != e.active()).expect("one out of view");
        let leaving = e.active();
        let lru = *before
            .iter()
            .min_by_key(|s| e.m.stage(StageId(**s)).map(|s| s.last_active))
            .expect("cards");
        let slot = before.iter().position(|s| *s == lru).expect("slot");
        let t = e.click(out).expect("a transition");
        assert_swap(&before, &e.order(), slot, leaving, "out of view");
        assert_aims(&e, &t, &before, "out of view");
        assert!(t.incoming.iter().all(|g| g.slot.is_none()), "it had no card: it grows from off the strip");
    }
}
