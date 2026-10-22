//! "Keep stages in place" (`[strip] order = "in_place"`): every stage keeps its place in
//! creation order whatever is clicked, in each active-stage mode; stages past the capacity
//! share the bottom place; a new, closed, merged, dragged, grouped and restored stage. Made-up
//! titles only.

use super::*;
use crate::engine::model::{Action, ModelOptions, Observation, StageId, WindowMeta};
use crate::engine::persist::{StateFile, rematch};
use crate::engine::rules::{Decision, Placement, RuleAction};
use crate::engine::settings::AppWindows;
use crate::engine::testkit::*;

const MODES: [ActiveCard; 3] = [ActiveCard::Close, ActiveCard::Gap, ActiveCard::Card];

/// A model in place plus a fake desktop that runs every action.
struct Env {
    m: Model,
    d: Desk,
    /// Each stage by the window that opened it.
    names: Vec<(StageId, u32)>,
}

impl Env {
    fn new(active: ActiveCard, capacity: usize) -> Self {
        Self::with(active, capacity, AppWindows::AllAtOnce)
    }

    fn with(active: ActiveCard, capacity: usize, app_windows: AppWindows) -> Self {
        let mut options = ModelOptions { strip: StripOptions { order: StripOrder::InPlace, active, capacity }, ..ModelOptions::default() };
        options.settings.app_windows = app_windows;
        Self { m: model_with(options), d: Desk::default(), names: Vec::new() }
    }

    /// `n` apps, one window and one stage each (window i, pid 100 + i), each opened in a stage
    /// of its own and shown: the last one is active.
    fn stages(active: ActiveCard, capacity: usize, n: u32) -> Self {
        let mut e = Self::new(active, capacity);
        for i in 1..=n {
            e.add(i, 100 + i as i32, if i == 1 { RuleAction::Current } else { RuleAction::New });
        }
        e
    }

    fn add(&mut self, id: u32, pid: i32, action: RuleAction) {
        let mt = WindowMeta { title: format!("Note {id}"), ..meta(id, pid, &format!("com.example.app{pid}")) };
        self.d.open(&mt);
        let obs = self.d.obs();
        let a = self.m.add_window(mt, &Decision { action, placement: Placement::AsIs, ..Decision::default() }, &obs);
        self.d.apply(&a);
        if let Some(s) = self.m.stage_of(w(id))
            && !self.names.iter().any(|n| n.0 == s)
        {
            self.names.push((s, id));
        }
    }

    /// The window that opened `stage`.
    fn name(&self, stage: StageId) -> u32 {
        self.names.iter().find(|n| n.0 == stage).map_or(0, |n| n.1)
    }

    fn obs(&self) -> Observation {
        self.d.obs()
    }

    /// The stage holding window `id`.
    fn s(&self, id: u32) -> StageId {
        self.m.stage_of(w(id)).expect("in a stage")
    }

    fn active(&self) -> StageId {
        self.m.main_active_stage().expect("an active stage")
    }

    /// Clicks the card in 1-based `slot` of the strip as drawn (the gap counts as a slot).
    fn click_slot(&mut self, slot: usize) -> StageId {
        let card = self.m.strip()[slot - 1].clone();
        assert!(!card.gap && !card.active, "slot {slot} is a card to click");
        let obs = self.obs();
        let a = self.m.switch_to(card.stage, &obs);
        self.d.apply(&a);
        card.stage
    }

    fn switch(&mut self, stage: StageId) {
        let obs = self.obs();
        let a = self.m.switch_to(stage, &obs);
        self.d.apply(&a);
    }

    /// The strip as (the window that opened its stage, kind): `'c'` a card, `'g'` the gap,
    /// `'a'` the active card.
    fn places(&self) -> Vec<(u32, char)> {
        self.m.strip().iter().map(|c| (self.name(c.stage), kind(c))).collect()
    }

    fn ids(&self) -> Vec<u32> {
        self.places().into_iter().map(|p| p.0).collect()
    }
}

fn kind(c: &StripCard) -> char {
    if c.gap {
        'g'
    } else if c.active {
        'a'
    } else {
        'c'
    }
}

/// Every stage of the Space in creation order, by the window that opened it, the active one
/// marked as `mode` shows it (left out with `Close`).
fn in_order(e: &Env, mode: ActiveCard) -> Vec<(u32, char)> {
    let active = e.active();
    e.m.stages()
        .iter()
        .filter(|s| !s.windows.is_empty())
        .filter_map(|s| {
            let first = e.name(s.id);
            match (s.id == active, mode) {
                (false, _) => Some((first, 'c')),
                (true, ActiveCard::Close) => None,
                (true, ActiveCard::Gap) => Some((first, 'g')),
                (true, ActiveCard::Card) => Some((first, 'a')),
            }
        })
        .collect()
}

#[test]
fn clicks_on_slots_1_3_and_6_leave_every_stage_in_its_place_in_each_mode() {
    for mode in MODES {
        let mut e = Env::stages(mode, 9, 7);
        let start = e.ids();
        for slot in [1, 3, 6] {
            // In Card mode slot 3 or 6 may be the active card itself: click the next one.
            let mut at = slot;
            while e.m.strip()[at - 1].gap || e.m.strip()[at - 1].active {
                at += 1;
            }
            let left = e.active();
            let clicked = e.click_slot(at);
            assert_eq!(e.active(), clicked, "{mode:?}: slot {at} switched");
            assert_eq!(e.places(), in_order(&e, mode), "{mode:?}: after slot {at}, every stage in its place");
            let left_at = e.m.strip().iter().position(|c| c.stage == left);
            let own = e.m.stages().iter().filter(|s| !s.windows.is_empty() && (mode != ActiveCard::Close || s.id != clicked)).position(|s| s.id == left);
            assert_eq!(left_at, own, "{mode:?}: the stage left went back to its own place");
        }
        if mode != ActiveCard::Close {
            assert_eq!(e.ids(), start, "{mode:?}: not one place moved");
        }
        let numbers: Vec<usize> = (1..=7).map(|i| e.m.stage_number(e.s(i)).expect("numbered")).collect();
        assert_eq!(numbers, [1, 2, 3, 4, 5, 6, 7], "{mode:?}: hotkey numbers are the places");
    }
    // Apple's order for contrast: the same clicks leave the stages in click order.
    let mut e = Env::stages(ActiveCard::Close, 9, 7);
    e.m.set_strip_options(StripOptions { order: StripOrder::Native, active: ActiveCard::Close, capacity: 9 });
    let before = e.ids();
    e.click_slot(1);
    assert_ne!(e.ids()[0], before[0], "native: the stage left took the clicked card's slot");
}

#[test]
fn in_gap_mode_the_gap_is_the_active_stages_slot_and_its_card_fills_it_when_left() {
    let mut e = Env::stages(ActiveCard::Gap, 9, 5);
    assert_eq!(e.places(), [(1, 'c'), (2, 'c'), (3, 'c'), (4, 'c'), (5, 'g')]);
    let gap = e.m.strip().into_iter().find(|c| c.gap).expect("a gap");
    assert_eq!(gap.windows, [w(5)], "the gap is laid out with the active stage's windows (its card's size)");
    e.click_slot(2);
    assert_eq!(e.places(), [(1, 'c'), (2, 'g'), (3, 'c'), (4, 'c'), (5, 'c')]);
    // A desktop reveal: the stage left gets its card back; the empty desktop stage has no place.
    let obs = e.obs();
    let a = e.m.reveal_desktop(&obs);
    e.d.apply(&a);
    assert_eq!(e.places(), [(1, 'c'), (2, 'c'), (3, 'c'), (4, 'c'), (5, 'c')]);
}

#[test]
fn a_new_stage_goes_last_and_a_closed_one_leaves_no_gap() {
    for mode in MODES {
        let mut e = Env::stages(mode, 9, 4);
        e.click_slot(1);
        // A new app: a stage of its own, last, and shown.
        e.add(9, 200, RuleAction::Native);
        assert_eq!(e.active(), e.s(9));
        assert_eq!(e.places(), in_order(&e, mode), "{mode:?}: the new stage last");
        assert_eq!(e.m.stages().iter().rfind(|s| !s.windows.is_empty()).map(|s| s.id), Some(e.s(9)));
        // A stage that empties goes, and the places close up behind it.
        let a = e.m.remove_window(w(3));
        e.d.close(w(3));
        e.d.apply(&a);
        assert!(!e.ids().contains(&3), "{mode:?}: stage 3 is gone");
        assert_eq!(e.places(), in_order(&e, mode), "{mode:?}: no gap where it was");
        assert_eq!(e.m.stage_number(e.s(4)), Some(3), "{mode:?}: the next stage takes its number");
    }
}

#[test]
fn a_merge_keeps_the_targets_place_and_a_drag_moves_a_stage_in_creation_order() {
    for mode in MODES {
        let mut e = Env::stages(mode, 9, 6);
        let (two, five, one) = (e.s(2), e.s(5), e.s(1));
        let obs = e.obs();
        let a = e.m.merge_stages(two, five, &obs);
        e.d.apply(&a);
        assert_eq!(e.m.stage_of(w(2)), Some(five), "{mode:?}: merged");
        assert_eq!(e.places(), in_order(&e, mode), "{mode:?}: the target keeps its place");
        assert_eq!(e.ids()[..3], [1, 3, 4], "{mode:?}: the dropped card's place closed up");
        // Card 5 (holding 2 and 5) dragged above card 1.
        assert_eq!(e.m.move_card(five, Some(one)), [Action::StateChanged]);
        assert_eq!(e.ids()[0], 5, "{mode:?}: the dragged card first");
        assert_eq!(e.m.stage_number(five), Some(1), "{mode:?}: its number follows");
        // A click then leaves the new order as it is.
        let before = e.ids();
        e.click_slot(2);
        if mode != ActiveCard::Close {
            assert_eq!(e.ids(), before, "{mode:?}: places kept after the reorder");
        }
        assert_eq!(e.places(), in_order(&e, mode));
    }
}

#[test]
fn a_drop_right_above_the_gap_goes_right_above_the_active_stage() {
    let mut e = Env::stages(ActiveCard::Gap, 9, 4);
    e.click_slot(2);
    // The strip maps a drop above the active stage's empty slot to the active stage.
    let (four, two) = (e.s(4), e.s(2));
    assert_eq!(e.m.move_card(four, Some(two)), [Action::StateChanged]);
    assert_eq!(e.places(), [(1, 'c'), (4, 'c'), (2, 'g'), (3, 'c')]);
}

#[test]
fn past_the_capacity_the_head_stays_and_the_bottom_slot_takes_turns() {
    // Capacity 4, seven stages: stages 1-3 keep the first three places; 4-7 share the last.
    for mode in MODES {
        let mut e = Env::stages(mode, 4, 7);
        assert_eq!(e.ids(), [1, 2, 3, 6], "{mode:?}: the head, then the most recent of the rest");
        // A click on the bottom card: the stage left takes the bottom place (its own there).
        e.click_slot(4);
        assert_eq!(e.ids(), [1, 2, 3, 7], "{mode:?}: the stage left rotates into the bottom slot");
        e.click_slot(4);
        assert_eq!(e.ids(), [1, 2, 3, 6], "{mode:?}: and back");
        // A head stage active: its place shows as the mode says.
        e.click_slot(2);
        match mode {
            // Its place frees: the cards close up and two of the rest show.
            ActiveCard::Close => assert_eq!(e.ids(), [1, 3, 6, 7]),
            // Its place stays (the head never moves); the bottom slot keeps the stage left.
            ActiveCard::Gap => assert_eq!(e.places(), [(1, 'c'), (2, 'g'), (3, 'c'), (7, 'c')]),
            ActiveCard::Card => assert_eq!(e.places(), [(1, 'c'), (2, 'a'), (3, 'c'), (7, 'c')]),
        }
        // A stage never shown (4) comes in by a hotkey, and the one left shows at once.
        let (four, five) = (e.s(4), e.s(5));
        e.switch(four);
        assert_eq!(e.ids(), [1, 2, 3, 7], "{mode:?}: stage 2 back in its place; 7 the most recent of the rest");
        e.switch(five);
        assert_eq!(e.ids(), [1, 2, 3, 4], "{mode:?}: the stage left (4) takes the bottom slot");
    }
}

#[test]
fn past_the_capacity_no_stage_is_ever_without_its_card_once_left_and_the_head_never_moves() {
    for mode in MODES {
        let mut e = Env::stages(mode, 5, 9);
        let all: Vec<StageId> = (1..=9).map(|i| e.s(i)).collect();
        let mut seed = 7u64;
        for step in 0..200 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let target = all[(seed >> 33) as usize % all.len()];
            let left = e.active();
            e.switch(target);
            let strip = e.m.strip();
            let what = format!("{mode:?} step {step}");
            assert!(strip.len() <= 5, "{what}: at most capacity places ({})", strip.len());
            if left != target {
                assert!(strip.iter().any(|c| c.stage == left && !c.gap), "{what}: the stage left shows");
            }
            for (i, s) in all[..4].iter().enumerate() {
                let at = strip.iter().position(|c| c.stage == *s);
                if *s == e.active() && mode == ActiveCard::Close {
                    assert_eq!(at, None, "{what}: no card for the active stage");
                } else if mode == ActiveCard::Close {
                    assert!(at.is_some(), "{what}: head stage {} shows", i + 1);
                } else {
                    assert_eq!(at, Some(i), "{what}: head stage {} in its own slot", i + 1);
                }
            }
        }
    }
}

#[test]
fn one_at_a_time_the_grouped_card_takes_the_first_stages_place() {
    for mode in MODES {
        let mut e = Env::with(mode, 9, AppWindows::OneAtATime);
        // App 50 has three windows, each in a stage of its own (2, 3 and 5).
        e.add(1, 10, RuleAction::Current);
        e.add(2, 50, RuleAction::New);
        e.add(3, 50, RuleAction::New);
        e.add(4, 11, RuleAction::New);
        e.add(5, 50, RuleAction::New);
        e.add(6, 12, RuleAction::New);
        let app = |e: &Env| e.m.strip().iter().filter(|c| c.apps == [50]).map(|c| c.windows.clone()).collect::<Vec<_>>();
        let place = |e: &Env| e.m.strip().iter().position(|c| c.apps == [50]);
        assert_eq!(app(&e).len(), 1, "{mode:?}: one card for the app");
        let first = place(&e);
        assert_eq!(first, Some(1), "{mode:?}: in the place of its first stage");
        // Click it again and again: one of its windows shows each time, its card stays put.
        for _ in 0..4 {
            let k = place(&e).expect("the app's card");
            e.click_slot(k + 1);
            assert_eq!(place(&e), first, "{mode:?}: the grouped card keeps the app's first place");
            assert_eq!(app(&e).len(), 1, "{mode:?}: still one card");
            assert!(!e.m.strip().iter().any(|c| c.active || c.gap), "{mode:?}: the app's shown window takes no place of its own");
        }
        // Another stage: the app's windows all wait behind its one card.
        let four = e.s(4);
        e.switch(four);
        assert_eq!(app(&e)[0].len(), 3, "{mode:?}: all three windows on the card");
        assert_eq!(place(&e), first);
    }
}

#[test]
fn a_restart_brings_the_same_places_back() {
    let mut e = Env::stages(ActiveCard::Gap, 9, 5);
    let (five, two) = (e.s(5), e.s(2));
    assert_eq!(e.m.move_card(five, Some(two)), [Action::StateChanged]);
    e.click_slot(3);
    let saved_places = e.places();
    assert_eq!(saved_places, [(1, 'c'), (5, 'c'), (2, 'g'), (3, 'c'), (4, 'c')]);
    let json = e.m.to_state().to_json().expect("encodes");
    // Relaunch: new window ids, listed in another order.
    let relive: Vec<WindowMeta> = [5u32, 3, 1, 4, 2]
        .iter()
        .map(|&i| WindowMeta { title: format!("Note {i}"), ..meta(10 + i, 100 + i as i32, &format!("com.example.app{}", 100 + i)) })
        .collect();
    let state = StateFile::from_json(&json).expect("parses");
    let a = rematch(&state.spaces[0], &relive);
    let options = ModelOptions { strip: StripOptions { order: StripOrder::InPlace, active: ActiveCard::Gap, capacity: 9 }, ..ModelOptions::default() };
    let mut fresh = model_with(options);
    fresh.apply_assignment(&relive, &a, false);
    let places: Vec<(u32, char)> = fresh
        .strip()
        .iter()
        .map(|c| {
            let first = fresh.stage(c.stage).and_then(|s| s.windows.first()).map_or(0, |w| w.0 - 10);
            (first, kind(c))
        })
        .collect();
    assert_eq!(places, saved_places, "the same places and the same active stage");
}
