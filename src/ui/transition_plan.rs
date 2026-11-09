//! Which windows a batch moves on or off the screen, and the card slot each one leaves into
//! or arrives from: decided once, from the engine, so the strip aims its snapshots at the
//! slot the engine gives the card (the one source of truth for "where it lands").
//!
//! The controller notes [`Before`] after every batch; when the next batch parks shown
//! windows or brings parked ones back, [`plan`] turns it into a [`Transition`]: outgoing
//! windows at their live frames, each with the slot its stage's card has **after** the change
//! (Apple's swap: the slot the clicked card vacated), incoming windows at their arrival
//! frames, each with the slot its card had **before** (where it is drawn when the motion
//! starts). In place (`[strip] order = "in_place"`) the stage left shrinks into its own
//! place, and with `active = "close"` the cards between the two places glide on the strip's
//! spring ([`glides`]). The slots count the active stage's empty slot (`active = "gap"`).
//!
//! The slots are those of the strip on the screen the moving windows are on
//! ([`display_of`]): a switch on a second screen is aimed at that screen's cards (round
//! displays-lid: before, every switch read the main screen's strip, and a card click on the
//! laptop found its windows in no engine slot).

use std::collections::{HashMap, HashSet};

use crate::engine::geometry::Rect;
use crate::engine::model::{Action, DisplayId, Home, Model, StripCard, WindowId};
use crate::ui::strip_model::{CardWindow, MovingWindow, Transition};

/// The strip's cards as slots: (stage, its windows), top to bottom.
pub type Slots = Vec<(u64, Vec<u32>)>;

/// The cards of the main display's strip, as [`Slots`].
pub fn slots_of(m: &Model) -> Slots {
    slots_on(m, m.main_display().id)
}

/// The cards of `display`'s strip, as [`Slots`].
pub fn slots_on(m: &Model, display: DisplayId) -> Slots {
    m.strip_for(display).iter().map(card_slot).collect()
}

fn card_slot(c: &StripCard) -> (u64, Vec<u32>) {
    (c.stage.0, c.windows.iter().map(|w| w.0).collect())
}

/// The display a batch's windows move on: that of the first window `actions` park or bring
/// back which the model knows, else the main one.
pub fn display_of(m: &Model, actions: &[Action]) -> DisplayId {
    actions
        .iter()
        .find_map(|a| match *a {
            Action::Park { id, .. } | Action::SetFrame { id, .. } => m.window(id).map(|w| w.meta.display),
            _ => None,
        })
        .unwrap_or(m.main_display().id)
}

/// The slot of the card showing window `id` (else the card of `stage`), and that card's stage.
pub fn slot_of(slots: &[(u64, Vec<u32>)], id: u32, stage: Option<u64>) -> Option<(usize, u64)> {
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
    /// Every display's strip's cards.
    pub slots: HashMap<DisplayId, Slots>,
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
            slots: m.displays().iter().map(|d| (d.id, slots_on(m, d.id))).collect(),
        }
    }

    /// The cards `display`'s strip had (none noted: no cards).
    pub fn slots_on(&self, display: DisplayId) -> &[(u64, Vec<u32>)] {
        self.slots.get(&display).map_or(&[], Vec::as_slice)
    }
}

/// The transition `id` for `actions` (already applied to `m`), if they park shown windows or
/// bring parked ones back. `live` gives a window's frame on screen now (falls back to the
/// model's). Outgoing windows are listed back to front (parks come front to back). The slots
/// are the strip's on the screen the windows are on ([`display_of`]).
pub fn plan(m: &Model, actions: &[Action], before: &Before, live: impl Fn(u32) -> Option<Rect>, id: u64) -> Option<Transition> {
    let display = display_of(m, actions);
    let after = slots_on(m, display);
    let before_slots = before.slots_on(display);
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
                let card = slot_of(before_slots, w.0, stage);
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
    // The key windows (their snapshots carry the key window's shadow): the outgoing one its
    // stage keeps as key, else its front one; the incoming one the batch focuses (none when it
    // focuses a window already in place), else the front one.
    let key_out = outgoing
        .iter()
        .map(|w| w.window.id)
        .find(|&id| m.stage_of(WindowId(id)).and_then(|s| m.stage(s)).is_some_and(|s| s.key == Some(WindowId(id))))
        .or_else(|| outgoing.last().map(|w| w.window.id));
    let focus = actions.iter().rev().find_map(|a| match *a {
        Action::Focus { id: w } => Some(w.0),
        _ => None,
    });
    let key_in = match focus {
        Some(w) => incoming.iter().any(|x| x.window.id == w).then_some(w),
        None => incoming.first().map(|w| w.window.id),
    };
    Some(Transition { id, outgoing, incoming, sent: None, under: None, corner: None, key_out, key_in, late: Vec::new(), minimized: None })
}

/// The stage ids of `slots`, top to bottom (for the logs).
pub fn order(slots: &[(u64, Vec<u32>)]) -> Vec<u64> {
    slots.iter().map(|s| s.0).collect()
}

/// The cards in both `before` and `after` whose slot changed: (stage, slot before, slot
/// after). The strip glides them on its spring (`[motion] spring_*`).
pub fn glides(before: &[(u64, Vec<u32>)], after: &[(u64, Vec<u32>)]) -> Vec<(u64, usize, usize)> {
    after
        .iter()
        .enumerate()
        .filter_map(|(to, (stage, _))| {
            let from = before.iter().position(|(s, _)| s == stage)?;
            (from != to).then_some((*stage, from, to))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! One test per kind of switch: the outgoing windows' card slot in the transition is the
    //! slot their card has once the strip is laid out, and Apple's swap rule holds (every
    //! other card keeps its slot).

    use super::*;
    use crate::engine::displays::ScreenKey;
    use crate::engine::model::{ActiveCard, Display, ModelOptions, Observation, StageId, StripOrder, WindowId};
    use crate::engine::park::Corner;
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

        /// [`Env::new`] with the strip in place (`[strip] order = "in_place"`, `active = mode`).
        fn in_place(n: u32, capacity: usize, mode: ActiveCard) -> Self {
            let mut options = ModelOptions::default();
            options.strip.capacity = capacity;
            options.strip.order = StripOrder::InPlace;
            options.strip.active = mode;
            let mut e = Self { m: model_with(options), d: Desk::default(), before: Before::default() };
            for i in 1..=n {
                let action = if i == 1 { RuleAction::Current } else { RuleAction::New };
                e.add(i, 100 + i as i32, action);
            }
            e
        }

        /// The strip's slots as the model lays them out now (the gap included).
        fn slots(&self) -> Slots {
            slots_of(&self.m)
        }

        fn add(&mut self, id: u32, pid: i32, action: RuleAction) -> Option<Transition> {
            let mt = meta(id, pid, &format!("com.example.app{pid}"));
            self.d.open(&mt);
            let obs = self.d.obs();
            let a = self.m.add_window(mt, &Decision { action, placement: Placement::AsIs, ..Decision::default() }, &obs);
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

    /// In place: clicks on slots 1, 3 and 6 (and back) in each mode. The stage left shrinks
    /// into its own place, the clicked card's windows grow from where it was drawn, and only
    /// with `close` do cards glide: exactly those between the two places, one slot each.
    #[test]
    fn in_place_the_stage_left_shrinks_into_its_own_place_and_only_close_glides() {
        for mode in [ActiveCard::Close, ActiveCard::Gap, ActiveCard::Card] {
            let mut e = Env::in_place(7, 9, mode);
            for slot in [0usize, 2, 5, 1, 4] {
                let before = e.slots();
                let leaving = e.active();
                let (target, gap) = (before[slot].0, before.iter().position(|(s, _)| *s == leaving));
                if target == leaving {
                    continue; // the active card or gap itself
                }
                let t = e.click(target).expect("a transition");
                let after = e.slots();
                let what = format!("{mode:?}, slot {slot}");
                let own = after.iter().position(|(s, _)| *s == leaving).expect("the stage left has a card");
                assert!(t.outgoing.iter().all(|g| g.slot == Some(own) && g.stage == Some(leaving)), "{what}: outgoing into its own slot {own}");
                assert!(t.incoming.iter().all(|g| g.slot == Some(slot) && g.stage == Some(target)), "{what}: incoming from slot {slot}");
                assert_aims(&e, &t, &order(&before), &what);
                let glided = glides(&before, &after);
                match mode {
                    ActiveCard::Close => {
                        // The stage left comes in at its own place, the clicked one goes.
                        let (lo, hi) = if own <= slot { (own, slot) } else { (slot + 1, own + 1) };
                        let want: Vec<(u64, usize, usize)> = before
                            .iter()
                            .enumerate()
                            .filter(|(i, (s, _))| *i >= lo && *i < hi && *s != target)
                            .map(|(i, (s, _))| (*s, i, if own <= slot { i + 1 } else { i - 1 }))
                            .collect();
                        assert_eq!(glided, want, "{what}: the cards between the two places glide one slot");
                        assert_eq!(gap, None);
                    }
                    ActiveCard::Gap | ActiveCard::Card => {
                        assert!(glided.is_empty(), "{what}: no card moves ({glided:?})");
                        assert_eq!(Some(own), gap, "{what}: the stage left fills its gap or card's slot");
                        assert_eq!(after[slot].0, target, "{what}: the clicked stage's slot is its gap or card now");
                    }
                }
            }
        }
    }

    #[test]
    fn in_place_past_the_capacity_the_stage_left_takes_the_bottom_slot() {
        let mut e = Env::in_place(7, 4, ActiveCard::Close);
        let before = e.slots();
        let leaving = e.active();
        let target = before[3].0;
        let t = e.click(target).expect("a transition");
        let after = e.slots();
        assert_eq!(after[3].0, leaving, "the stage left rotates into the bottom slot");
        assert!(t.outgoing.iter().all(|g| g.slot == Some(3)));
        assert!(t.incoming.iter().all(|g| g.slot == Some(3)));
        assert!(glides(&before, &after).is_empty(), "the head never moves");
    }

    /// A Dock click, a hotkey and a desktop reveal follow the same rules as a click.
    #[test]
    fn in_place_a_dock_click_a_hotkey_and_a_reveal_leave_the_stage_in_its_own_place() {
        for mode in [ActiveCard::Close, ActiveCard::Gap, ActiveCard::Card] {
            let mut e = Env::in_place(6, 9, mode);
            let check = |e: &Env, t: &Transition, before: &Slots, leaving: u64, what: &str| {
                let after = e.slots();
                let own = after.iter().position(|(s, _)| *s == leaving);
                assert!(own.is_some() && t.outgoing.iter().all(|g| g.slot == own), "{mode:?} {what}: into its own slot");
                let stages: Vec<u64> = e.m.stages().iter().filter(|s| !s.windows.is_empty()).map(|s| s.id.0).collect();
                let shown: Vec<u64> = order(&after);
                assert!(shown.iter().all(|s| stages.contains(s)), "{mode:?} {what}");
                let mut sorted = shown.clone();
                sorted.sort_by_key(|s| stages.iter().position(|x| x == s));
                assert_eq!(shown, sorted, "{mode:?} {what}: creation order");
                if mode != ActiveCard::Close {
                    assert!(glides(before, &after).is_empty(), "{mode:?} {what}: nothing glides");
                }
            };
            // The Dock: app 102's stage.
            let (before, leaving) = (e.slots(), e.active());
            let obs = e.obs();
            let a = e.m.app_activated(102, &obs);
            let t = e.apply(a).expect("a transition");
            assert_eq!(e.active(), e.stage_of(2));
            check(&e, &t, &before, leaving, "dock");
            // A hotkey: stage 5.
            let (before, leaving) = (e.slots(), e.active());
            let obs = e.obs();
            let a = e.m.switch_to_number(5, e.m.main_display().id, &obs);
            let t = e.apply(a).expect("a transition");
            assert_eq!(e.active(), e.stage_of(5));
            check(&e, &t, &before, leaving, "hotkey");
            // A wallpaper click.
            let (before, leaving) = (e.slots(), e.active());
            let obs = e.obs();
            let a = e.m.reveal_desktop(&obs);
            let t = e.apply(a).expect("a transition");
            check(&e, &t, &before, leaving, "reveal");
        }
    }

    /// The owner's two screens: the 5K main, the laptop to its left.
    fn two_screens() -> (Display, Display) {
        let studio = Display {
            id: DisplayId(1),
            key: ScreenKey::NONE,
            frame: Rect::new(0.0, 0.0, 2560.0, 1440.0),
            visible: Rect::new(0.0, 25.0, 2560.0, 1415.0),
            corner: Corner::BottomRight,
        };
        let laptop = Display {
            id: DisplayId(2),
            key: ScreenKey::NONE,
            frame: Rect::new(-1800.0, 271.0, 1800.0, 1169.0),
            visible: Rect::new(-1800.0, 310.0, 1800.0, 1130.0),
            corner: Corner::BottomLeft,
        };
        (studio, laptop)
    }

    /// Opens window `id` of `pid` on `d`, by `action`.
    fn add_on(e: &mut Env, id: u32, pid: i32, d: &Display, action: RuleAction) {
        let mut mt = meta(id, pid, &format!("com.example.app{pid}"));
        mt.frame = Rect::new(d.visible.x + 100.0 * f64::from(id), d.visible.y + 50.0, 600.0, 400.0);
        mt.display = d.id;
        e.d.open(&mt);
        let obs = e.d.obs();
        let a = e.m.add_window(mt, &Decision { action, placement: Placement::AsIs, ..Decision::default() }, &obs);
        e.apply(a);
    }

    #[test]
    fn a_switch_on_another_screen_reads_that_screens_slots() {
        // 11-05 16:26, the owner's laptop: every card click there logged its windows in no
        // engine slot (the plan read the 5K's strip). The slots are the laptop's own.
        let (studio, laptop) = two_screens();
        let mut e = Env { m: Model::new(vec![studio, laptop], ModelOptions::default()), d: Desk::default(), before: Before::default() };
        // The 5K: three stages; the laptop: Chrome a card, Finder in front.
        add_on(&mut e, 1, 101, &studio, RuleAction::Current);
        add_on(&mut e, 2, 102, &studio, RuleAction::New);
        add_on(&mut e, 3, 103, &studio, RuleAction::New);
        add_on(&mut e, 5, 105, &laptop, RuleAction::Current);
        add_on(&mut e, 6, 106, &laptop, RuleAction::New);
        let (chrome, finder) = (e.stage_of(5), e.stage_of(6));
        assert_eq!(e.m.active_stage(DisplayId(2)), Some(StageId(finder)));
        assert_eq!(order(&slots_on(&e.m, DisplayId(2))), vec![chrome]);
        let t = e.click(chrome).expect("a transition");
        assert_eq!(order(&slots_on(&e.m, DisplayId(2))), vec![finder], "Apple's swap on the laptop's strip");
        let moving = |g: &[MovingWindow]| g.iter().map(|g| (g.window.id, g.stage, g.slot)).collect::<Vec<_>>();
        assert_eq!(moving(&t.outgoing), vec![(6, Some(finder), Some(0))], "Finder shrinks into the slot its card takes on the laptop's strip");
        assert_eq!(moving(&t.incoming), vec![(5, Some(chrome), Some(0))], "Chrome grows from the slot its card had there");
        // Read on the 5K's strip, neither window has a card.
        let main = slots_of(&e.m);
        assert!(slot_of(&main, 6, Some(finder)).is_none() && slot_of(&main, 5, Some(chrome)).is_none());
        assert_eq!(display_of(&e.m, &[Action::Park { id: w(6), frame: Rect::default() }]), DisplayId(2));
        assert_eq!(display_of(&e.m, &[Action::StateChanged]), DisplayId(1), "no window: the main screen");
        // A switch on the 5K still reads the 5K's slots.
        let before = e.order();
        let target = e.stage_of(2);
        let slot = before.iter().position(|s| *s == target).expect("a card");
        let t = e.click(target).expect("a transition");
        assert_aims(&e, &t, &before, "the 5K");
        assert!(t.incoming.iter().all(|g| g.slot == Some(slot)));
    }
}
