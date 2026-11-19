//! A screen going away under the Spaces engine: a window of a hidden stage of one of its
//! Desktops that the window server lets out onto another Desktop as the screen goes stays in
//! its stage, hidden again, while the change settles (a log, 11-18). Made-up titles only.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

use crate::engine::recovery::EngineKind;

fn meta(id: u32, pid: Pid, display: &Display, at: (f64, f64)) -> WindowMeta {
    WindowMeta {
        id: WindowId(id),
        pid,
        bundle_id: format!("com.example.app{pid}"),
        title: format!("Doc {id}"),
        role: WindowRole::Standard,
        frame: on(display, at),
        display: display.id,
    }
}

/// Two screens on `engine`: on the Studio, Code in front (1); on the laptop, its first
/// Desktop with Chrome (5) a hidden card behind Finder (6), then its second Desktop shown
/// (Notes, 7). The laptop goes. Returns the laptop's first Desktop and Chrome's stage.
fn laptop_gone(engine: EngineKind) -> (Env, SpaceId, StageId) {
    let mut e = Env { m: model_on(vec![studio(), laptop()], ModelOptions { engine, ..ModelOptions::default() }), d: Desk::default() };
    e.open(1, 10, &studio(), (200.0, 100.0), RuleAction::Current);
    e.open(5, 20, &laptop(), (100.0, 100.0), RuleAction::Current);
    e.open(6, 21, &laptop(), (400.0, 200.0), RuleAction::New);
    let first = e.space_of(LAPTOP);
    let chrome = e.stage_of(5);
    assert!(e.m.window(WindowId(5)).is_some_and(|w| w.parked), "Chrome a card");
    let a = e.m.arrive(LAPTOP, &[meta(7, 30, &laptop(), (300.0, 300.0))], &[WindowId(5), WindowId(6)], &LaunchHints::default());
    e.run(a);
    assert_ne!(e.space_of(LAPTOP), first, "the laptop's second Desktop shows");
    let a = e.m.set_displays(vec![studio()]);
    e.run(a);
    (e, first, chrome)
}

/// The Studio's arrival listing Code and Chrome (let out of its stage Space onto the
/// Studio's Desktop as the laptop went). `hold`: the change settling with no click or key
/// since; `watch`: the Spaces engine watching the screen gone (the platform says so before
/// every arrival and at every tick).
fn arrive_with_chrome(e: &mut Env, hold: bool, watch: bool) -> Vec<Action> {
    let live = [meta(1, 10, &studio(), (200.0, 100.0)), meta(5, 20, &studio(), (100.0, 100.0))];
    e.m.set_hold_arrivals(hold);
    e.m.set_display_watch(watch);
    let a = e.m.arrive(STUDIO, &live, &[], &LaunchHints::default());
    e.m.set_hold_arrivals(false);
    e.run(a)
}

/// Chrome kept in its stage, hidden again, on its own Desktop.
fn assert_chrome_kept(e: &Env, a: &[Action], first: SpaceId, chrome: StageId) {
    let note = e.m.last_arrival().clone();
    assert_eq!(note.kept_hidden, vec![WindowId(5)], "{note:?}");
    assert!(a.contains(&Action::Hide { id: WindowId(5) }), "hidden again: {a:?}");
    assert_eq!(e.m.space_of(WindowId(5)), Some(first), "still its own Desktop's");
    assert_eq!(e.m.stage_of(WindowId(5)), Some(chrome), "still its own stage");
    assert!(e.m.window(WindowId(5)).is_some_and(|w| w.parked));
    assert_eq!(note.moved, 0);
}

#[test]
fn a_hidden_window_let_out_as_its_screen_went_stays_in_its_stage_hidden_again() {
    let (mut e, first, chrome) = laptop_gone(EngineKind::Spaces);
    let a = arrive_with_chrome(&mut e, true, true);
    assert_chrome_kept(&e, &a, first, chrome);
}

#[test]
fn a_key_press_before_the_visit_keeps_the_window_in_its_stage_while_the_engine_watches() {
    // 20 s after the change, a key pressed since (arrivals no longer held): the engine still
    // watches the screen gone, and the arrival on the moved Desktop keeps Chrome hidden.
    let (mut e, first, chrome) = laptop_gone(EngineKind::Spaces);
    let a = arrive_with_chrome(&mut e, false, true);
    assert_chrome_kept(&e, &a, first, chrome);
}

#[test]
fn after_the_watch_a_window_listed_elsewhere_is_taken_as_ever() {
    // The engine's watch over: nothing kept, and nothing is kept for later either.
    let (mut e, _, _) = laptop_gone(EngineKind::Spaces);
    let a = arrive_with_chrome(&mut e, true, false);
    assert!(e.m.last_arrival().kept_hidden.is_empty());
    assert!(!a.contains(&Action::Hide { id: WindowId(5) }));
    assert!(e.m.handed_hidden.is_empty(), "cleared as the watch ended");
    // The same seen twice (the second look a window of another Desktop needs): it moved.
    arrive_with_chrome(&mut e, false, false);
    assert_eq!(e.m.space_of(WindowId(5)), Some(e.space_of(STUDIO)), "moved, as today");
}

#[test]
fn a_window_gone_is_no_longer_kept() {
    let (mut e, _, _) = laptop_gone(EngineKind::Spaces);
    assert!(e.m.handed_hidden.contains(&WindowId(5)));
    let a = e.m.remove_window(WindowId(5));
    e.run(a);
    assert!(!e.m.handed_hidden.contains(&WindowId(5)));
}

#[test]
fn the_corner_engine_keeps_its_own_rules_for_a_gone_screens_cards() {
    // Parked windows sit on their Desktop under the Corner engine; macOS moves them with it.
    let (mut e, _, _) = laptop_gone(EngineKind::Corner);
    arrive_with_chrome(&mut e, true, true);
    assert!(e.m.last_arrival().kept_hidden.is_empty());
}

#[test]
fn the_desktop_the_gone_screen_showed_is_left_to_the_adoption() {
    // Notes (7) was on the Desktop shown: its stage goes with the adoption's rule
    // ([`Model::carry_adopted`]), never kept here.
    let (e, _, _) = laptop_gone(EngineKind::Spaces);
    assert!(e.m.handed_hidden.contains(&WindowId(5)));
    assert!(!e.m.handed_hidden.contains(&WindowId(7)));
    assert!(!e.m.handed_hidden.contains(&WindowId(6)), "Finder was in front there, not hidden");
}

/// The main screen goes while the screen left shows a Desktop of its own (macOS made it when
/// the other screen took the main role, and ends it now: a live run, 11-18). On the Studio
/// (main): Code (1) in front, Mail (2) and Notes (3) hidden cards. On the laptop: Finder (6)
/// in front, Notes' second window (7) a hidden card of a stage of its own. The Studio goes;
/// macOS shows its Desktop on the laptop, with Finder put on it. Returns the stages of 1, 2,
/// 3, 6 and 7, by window, before the change.
fn main_gone(engine: EngineKind) -> (Env, BTreeMap<u32, StageId>) {
    let mut e = Env { m: model_on(vec![studio(), laptop()], ModelOptions { engine, ..ModelOptions::default() }), d: Desk::default() };
    e.open(2, 20, &studio(), (100.0, 100.0), RuleAction::Current);
    e.open(3, 30, &studio(), (300.0, 150.0), RuleAction::New);
    e.open(1, 10, &studio(), (200.0, 100.0), RuleAction::New);
    e.open(7, 30, &laptop(), (400.0, 200.0), RuleAction::Current);
    e.open(6, 60, &laptop(), (100.0, 100.0), RuleAction::New);
    let stages: BTreeMap<u32, StageId> = [1, 2, 3, 6, 7].into_iter().map(|id| (id, e.stage_of(id))).collect();
    assert_eq!(stages.values().collect::<BTreeSet<_>>().len(), 5, "five stages: {stages:?}");
    for id in [2, 3, 7] {
        assert!(e.m.window(WindowId(id)).is_some_and(|w| w.parked), "{id} a card");
    }
    let main_laptop = Display { frame: Rect::new(0.0, 0.0, 1800.0, 1169.0), visible: Rect::new(0.0, 39.0, 1800.0, 1130.0), ..laptop() };
    let a = e.m.set_displays(vec![main_laptop]);
    e.run(a);
    (e, stages)
}

#[test]
fn the_main_screen_gone_the_ended_desktops_stages_join_the_desktop_moved_in_whole() {
    let (mut e, stages) = main_gone(EngineKind::Spaces);
    let shown = e.space_of(LAPTOP);
    // The arrival on the Desktop moved in: Code and Finder on screen (macOS put Finder there).
    let main_laptop = e.m.displays()[0];
    let live = [meta(1, 10, &main_laptop, (200.0, 100.0)), meta(6, 60, &main_laptop, (100.0, 100.0))];
    e.m.set_hold_arrivals(true);
    e.m.set_display_watch(true);
    let a = e.m.arrive(LAPTOP, &live, &[], &LaunchHints::default());
    e.m.set_hold_arrivals(false);
    e.run(a);
    for (id, stage) in &stages {
        assert_eq!(e.m.stage_of(WindowId(*id)), Some(*stage), "window {id} keeps its stage");
        assert_eq!(e.m.space_of(WindowId(*id)), Some(shown), "window {id} on the Desktop shown");
    }
    for stage in stages.values() {
        assert_eq!(e.m.stage(*stage).map(|s| s.windows.len()), Some(1), "no stage gains another's window");
    }
    assert!(e.m.is_active(stages[&1]), "the gone screen's front stage in front");
    for id in [2, 3, 6, 7] {
        assert!(e.m.window(WindowId(id)).is_some_and(|w| w.parked), "{id} hidden, a card");
    }
    // The model's windows shown there: where the engine makes the ended Desktop's stage
    // Spaces again.
    assert_eq!(e.m.shown_with(WindowId(7)), vec![WindowId(1)]);
    assert_eq!(e.m.shown_with(WindowId(1)), Vec::<WindowId>::new());
    assert!(e.m.shown_with(WindowId(99)).is_empty());
}

#[test]
fn the_main_screen_gone_the_corner_engine_keeps_the_stages_whole_too() {
    let (mut e, stages) = main_gone(EngineKind::Corner);
    let main_laptop = e.m.displays()[0];
    // Parked windows sit on their Desktop under the Corner engine: listed too, at the corner.
    let live = [meta(1, 10, &main_laptop, (200.0, 100.0)), meta(6, 60, &main_laptop, (100.0, 100.0))];
    let a = e.m.arrive(LAPTOP, &live, &[], &LaunchHints::default());
    e.run(a);
    for (id, stage) in &stages {
        assert_eq!(e.m.stage_of(WindowId(*id)), Some(*stage), "window {id} keeps its stage");
    }
    for stage in stages.values() {
        assert_eq!(e.m.stage(*stage).map(|s| s.windows.len()), Some(1));
    }
}
