//! "Follow the Dock" (`[strip] order = "dock"`): the in-place places in the Dock's order of
//! the stages' apps. A stage sits at its leftmost app; ties and apps not in the Dock keep
//! creation order (the latter last); a changed Dock re-sorts; overflow, the active modes and
//! hotkey numbers work as in place; a card drag is refused; a dropped window takes its Dock
//! place. Made-up apps and titles only.

use super::*;
use crate::engine::model::{Action, ModelOptions, Observation, StageId, WindowMeta};
use crate::engine::rules::{Decision, Placement, RuleAction};
use crate::engine::testkit::*;

const MODES: [ActiveCard; 3] = [ActiveCard::Close, ActiveCard::Gap, ActiveCard::Card];

/// The bundle id of made-up app `pid`.
fn bundle(pid: i32) -> String {
    format!("com.example.app{pid}")
}

/// A Dock of these apps, left to right, each running.
fn dock(pids: &[i32]) -> Vec<DockApp> {
    pids.iter().map(|&p| DockApp::new(&bundle(p), Some(p))).collect()
}

/// A model following the Dock plus a fake desktop that runs every action.
struct Env {
    m: Model,
    d: Desk,
}

impl Env {
    fn new(active: ActiveCard, capacity: usize) -> Self {
        let options = ModelOptions { strip: StripOptions { order: StripOrder::Dock, active, capacity }, ..ModelOptions::default() };
        Self { m: model_with(options), d: Desk::default() }
    }

    /// One stage per app, in creation order `pids` (window i + 1 for the ith, opened in a
    /// stage of its own and shown: the last one is active).
    fn stages(active: ActiveCard, capacity: usize, pids: &[i32]) -> Self {
        let mut e = Self::new(active, capacity);
        for (i, &pid) in pids.iter().enumerate() {
            e.add(i as u32 + 1, pid, if i == 0 { RuleAction::Current } else { RuleAction::New });
        }
        e
    }

    /// Window `id` of app `pid`, placed by `action`.
    fn add(&mut self, id: u32, pid: i32, action: RuleAction) {
        self.add_meta(WindowMeta { title: format!("Note {id}"), ..meta(id, pid, &bundle(pid)) }, action);
    }

    fn add_meta(&mut self, mt: WindowMeta, action: RuleAction) {
        self.d.open(&mt);
        let obs = self.d.obs();
        let a = self.m.add_window(mt, &Decision { action, placement: Placement::AsIs, ..Decision::default() }, &obs);
        self.d.apply(&a);
    }

    fn obs(&self) -> Observation {
        self.d.obs()
    }

    fn s(&self, id: u32) -> StageId {
        self.m.stage_of(w(id)).expect("in a stage")
    }

    fn active(&self) -> StageId {
        self.m.main_active_stage().expect("an active stage")
    }

    fn switch(&mut self, stage: StageId) {
        let obs = self.obs();
        let a = self.m.switch_to(stage, &obs);
        self.d.apply(&a);
    }

    /// The strip as the first window of each card's stage (the gap and the active card
    /// included).
    fn ids(&self) -> Vec<u32> {
        self.m
            .strip()
            .iter()
            .map(|c| self.m.stage(c.stage).and_then(|s| s.windows.iter().map(|w| w.0).min()).unwrap_or(0))
            .collect()
    }

    /// Every stage holding windows in its place (by its first window), the active one as
    /// `mode` shows it (left out with `Close`).
    fn want(&self, places: &[u32], mode: ActiveCard) -> Vec<u32> {
        let active = self.active();
        places.iter().copied().filter(|&id| mode != ActiveCard::Close || self.s(id) != active).collect()
    }
}

#[test]
fn stages_sit_in_the_docks_order_and_their_numbers_follow() {
    for mode in MODES {
        // Created 101, 102, 103, 104; the Dock shows 103, 101, 104, 102.
        let mut e = Env::stages(mode, 9, &[101, 102, 103, 104]);
        assert!(e.m.set_dock_order(dock(&[103, 101, 104, 102])), "{mode:?}: the strip changed");
        assert_eq!(e.ids(), e.want(&[3, 1, 4, 2], mode), "{mode:?}: the Dock's order");
        let numbers: Vec<usize> = [3, 1, 4, 2].iter().map(|&i| e.m.stage_number(e.s(i)).expect("numbered")).collect();
        assert_eq!(numbers, [1, 2, 3, 4], "{mode:?}: hotkey numbers follow the strip");
        let space = e.m.current_space(e.m.main_display().id);
        assert_eq!(e.m.stage_by_number(space, 1), Some(e.s(3)), "{mode:?}: ⌃⌥1 is the Dock's first");
        // Clicks never move a place, whatever is clicked (each active mode).
        for id in [2, 3, 1, 4, 3] {
            let target = e.s(id);
            e.switch(target);
            assert_eq!(e.active(), target);
            assert_eq!(e.ids(), e.want(&[3, 1, 4, 2], mode), "{mode:?}: after a switch to {id}, every stage in its Dock place");
        }
        if mode == ActiveCard::Gap {
            assert!(e.m.strip().iter().any(|c| c.gap && c.stage == e.s(3)), "the gap is the active stage's own place");
        }
        if mode == ActiveCard::Card {
            assert!(e.m.strip().iter().any(|c| c.active && c.stage == e.s(3)), "the active card in its own place");
        }
    }
}

#[test]
fn a_stage_of_several_apps_sits_at_its_leftmost_app() {
    let mut e = Env::new(ActiveCard::Card, 9);
    // Stage A: Mail-like 101 and Calendar-like 105 together; stage B: 102; stage C: 103.
    e.add(1, 101, RuleAction::Current);
    e.add(2, 105, RuleAction::Current);
    e.add(3, 102, RuleAction::New);
    e.add(4, 103, RuleAction::New);
    assert_eq!(e.s(1), e.s(2), "one stage with both apps");
    // The Dock: 102, 105, 103, 101: the stage sits at 105's place (second).
    e.m.set_dock_order(dock(&[102, 105, 103, 101]));
    assert_eq!(e.ids(), [3, 1, 4]);
    // 101 moves to the Dock's front: the stage goes first, whichever window came first.
    e.m.set_dock_order(dock(&[101, 102, 105, 103]));
    assert_eq!(e.ids(), [1, 3, 4]);
    // A merge: the merged stage sits at its leftmost app, wherever the dropped card was.
    let (a_stage, b, c) = (e.s(1), e.s(3), e.s(4));
    e.switch(a_stage);
    let obs = e.obs();
    let a = e.m.merge_stages(c, b, &obs);
    e.d.apply(&a);
    e.m.set_dock_order(dock(&[103, 101, 102, 105]));
    assert_eq!(e.ids(), [3, 1], "the stage of 102 and 103 at 103's place");
}

#[test]
fn stages_of_the_same_app_keep_creation_order() {
    let mut e = Env::new(ActiveCard::Card, 9);
    e.add(1, 102, RuleAction::Current);
    e.add(2, 101, RuleAction::New);
    e.add(3, 102, RuleAction::New);
    e.add(4, 101, RuleAction::New);
    e.m.set_dock_order(dock(&[101, 102]));
    assert_eq!(e.ids(), [2, 4, 1, 3], "101's two stages, then 102's, each in creation order");
    e.m.set_dock_order(dock(&[102, 101]));
    assert_eq!(e.ids(), [1, 3, 2, 4]);
}

#[test]
fn apps_not_in_the_dock_go_last_in_creation_order() {
    let mut e = Env::stages(ActiveCard::Card, 9, &[201, 101, 202, 102]);
    // 201 and 202 have no Dock icon (agent apps).
    e.m.set_dock_order(dock(&[102, 101]));
    assert_eq!(e.ids(), [4, 2, 1, 3]);
    // No Dock read yet: creation order.
    let fresh = Env::stages(ActiveCard::Card, 9, &[201, 101, 202, 102]);
    assert_eq!(fresh.ids(), [1, 2, 3, 4]);
}

#[test]
fn an_app_matches_its_dock_item_by_bundle_id_then_by_pid() {
    let mut e = Env::new(ActiveCard::Card, 9);
    // Window 1's app has no bundle id: found by its pid. Window 2's bundle id names the
    // item though another pid runs it (a second instance).
    e.add_meta(WindowMeta { title: "Note 1".into(), ..meta(1, 300, "") }, RuleAction::Current);
    e.add_meta(WindowMeta { title: "Note 2".into(), ..meta(2, 301, &bundle(101)) }, RuleAction::New);
    e.add(3, 102, RuleAction::New);
    e.m.set_dock_order(vec![DockApp::new(&bundle(102), Some(102)), DockApp::new(&bundle(101), Some(101)), DockApp { bundle: None, pid: Some(300) }]);
    assert_eq!(e.ids(), [3, 2, 1]);
    // The bundle id wins over a pid that matches another item.
    e.m.set_dock_order(vec![DockApp { bundle: None, pid: Some(301) }, DockApp::new(&bundle(102), Some(102)), DockApp::new(&bundle(101), Some(101))]);
    assert_eq!(e.ids()[..2], [3, 2], "window 2 sits at its bundle's item, not its pid's");
}

#[test]
fn a_changed_dock_re_sorts_the_strip_and_the_same_one_changes_nothing() {
    let mut e = Env::stages(ActiveCard::Close, 9, &[101, 102, 103]);
    assert!(e.m.set_dock_order(dock(&[103, 102, 101])));
    let before = e.m.strip();
    assert!(!e.m.set_dock_order(dock(&[103, 102, 101])), "the same order: nothing to present");
    assert!(!e.m.set_dock_order(dock(&[103, 102, 101, 104])), "an icon of an app with no stage: the strip is the same");
    assert_eq!(e.m.strip(), before);
    // The user drags 101 to the Dock's front.
    assert!(e.m.set_dock_order(dock(&[101, 103, 102])));
    assert_eq!(e.ids(), [1, 2], "103 active (no card), then 101 before 102");
    // An unpinned app quits: its icon goes; a stage of another app is not moved by that.
    assert!(!e.m.set_dock_order(dock(&[101, 103, 102])));
    // Keep in place ignores the Dock: the order is kept for a switch back.
    e.m.set_strip_options(StripOptions { order: StripOrder::InPlace, active: ActiveCard::Close, capacity: 9 });
    assert!(!e.m.set_dock_order(dock(&[102, 101, 103])), "in place, creation order: nothing changes");
    assert_eq!(e.ids(), [1, 2]);
    e.m.set_strip_options(StripOptions { order: StripOrder::Dock, active: ActiveCard::Close, capacity: 9 });
    assert_eq!(e.ids(), [2, 1], "switched to follow the Dock: sorted at once");
}

#[test]
fn past_the_capacity_the_docks_head_stays_and_the_bottom_slot_takes_turns() {
    for mode in MODES {
        // Seven stages created 101..107; the Dock shows them reversed. Capacity 4: the
        // Dock's first three keep their places, the rest share the last by recency.
        let pids = [101, 102, 103, 104, 105, 106, 107];
        let mut e = Env::stages(mode, 4, &pids);
        e.m.set_dock_order(dock(&[107, 106, 105, 104, 103, 102, 101]));
        let strip = e.m.strip();
        assert!(strip.len() <= 4, "{mode:?}: at most capacity places");
        let head: Vec<u32> = e.ids().into_iter().take(3).collect();
        match mode {
            // 7 is active: its place frees and the cards close up.
            ActiveCard::Close => assert_eq!(head, [6, 5, 4]),
            _ => assert_eq!(head, [7, 6, 5], "{mode:?}"),
        }
        // Every stage switched to and left keeps a card, and the head never moves.
        for id in [1, 2, 3, 1] {
            let left = e.active();
            let target = e.s(id);
            e.switch(target);
            let strip = e.m.strip();
            assert!(strip.iter().any(|c| c.stage == left && !c.gap), "{mode:?}: the stage left shows");
            if mode != ActiveCard::Close {
                assert_eq!(e.ids()[..3], [7, 6, 5], "{mode:?}: the head in the Dock's order");
            }
        }
    }
}

#[test]
fn a_card_drag_is_refused_and_switches_nothing() {
    let mut e = Env::stages(ActiveCard::Gap, 9, &[101, 102, 103, 104]);
    e.m.set_dock_order(dock(&[104, 103, 102, 101]));
    let (before, active) = (e.ids(), e.active());
    let numbers: Vec<Option<usize>> = (1..=4).map(|i| e.m.stage_number(e.s(i))).collect();
    let (one, three) = (e.s(1), e.s(3));
    assert_eq!(e.m.move_card(one, Some(three)), Vec::<Action>::new(), "no move: the Dock decides");
    assert_eq!(e.m.move_card(three, None), Vec::<Action>::new());
    assert_eq!(e.ids(), before, "the card springs back to its place");
    assert_eq!(e.active(), active, "no click: no switch");
    assert_eq!((1..=4).map(|i| e.m.stage_number(e.s(i))).collect::<Vec<_>>(), numbers);
    // A drop onto a card still merges.
    let obs = e.obs();
    let a = e.m.merge_stages(one, three, &obs);
    e.d.apply(&a);
    assert_eq!(e.m.stage_of(w(1)), Some(three));
}

#[test]
fn a_window_dropped_on_the_strip_takes_its_dock_place_not_the_drop_position() {
    for mode in MODES {
        let mut e = Env::stages(mode, 9, &[101, 102, 103]);
        // 104 sits third in the Dock; its window joins the active stage, then is dropped at
        // the top of the strip.
        e.m.set_dock_order(dock(&[101, 102, 104, 103]));
        e.add(4, 104, RuleAction::Current);
        let top = e.m.strip()[0].stage;
        let obs = e.obs();
        let frame = obs.frames[&w(4)];
        let a = e.m.drop_window_on_strip(w(4), frame, Some(top), &obs);
        e.d.apply(&a);
        let dropped = e.s(4);
        assert_ne!(dropped, e.s(3), "{mode:?}: a stage of its own");
        assert_eq!(e.ids(), e.want(&[1, 2, 4, 3], mode), "{mode:?}: at 104's Dock place");
        assert_eq!(e.m.stage_number(dropped), Some(3), "{mode:?}: its number is its place");
    }
    // Past the capacity it shows (it counts as used).
    let mut e = Env::stages(ActiveCard::Close, 3, &[101, 102, 103, 105, 106]);
    e.m.set_dock_order(dock(&[101, 102, 103, 104, 105, 106]));
    e.add(9, 104, RuleAction::Current);
    let obs = e.obs();
    let frame = obs.frames[&w(9)];
    let a = e.m.drop_window_on_strip(w(9), frame, None, &obs);
    e.d.apply(&a);
    let dropped = e.s(9);
    assert!(e.m.strip().iter().any(|c| c.stage == dropped), "the dropped window's stage shows: {:?}", e.m.strip());
}
