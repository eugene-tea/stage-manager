//! A screen going away under the Spaces engine: a window of a hidden stage of one of its
//! Desktops that the window server lets out onto another Desktop as the screen goes stays in
//! its stage, hidden again, while the change settles (a log, 11-18). Made-up titles only.

use super::*;
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
/// Studio's Desktop as the laptop went), `hold`: the change settling with no click or key
/// since.
fn arrive_with_chrome(e: &mut Env, hold: bool) -> Vec<Action> {
    let live = [meta(1, 10, &studio(), (200.0, 100.0)), meta(5, 20, &studio(), (100.0, 100.0))];
    e.m.set_hold_arrivals(hold);
    let a = e.m.arrive(STUDIO, &live, &[], &LaunchHints::default());
    e.m.set_hold_arrivals(false);
    e.run(a)
}

#[test]
fn a_hidden_window_let_out_as_its_screen_went_stays_in_its_stage_hidden_again() {
    let (mut e, first, chrome) = laptop_gone(EngineKind::Spaces);
    let a = arrive_with_chrome(&mut e, true);
    let note = e.m.last_arrival().clone();
    assert_eq!(note.kept_hidden, vec![WindowId(5)], "{note:?}");
    assert!(a.contains(&Action::Hide { id: WindowId(5) }), "hidden again: {a:?}");
    assert_eq!(e.m.space_of(WindowId(5)), Some(first), "still its own Desktop's");
    assert_eq!(e.m.stage_of(WindowId(5)), Some(chrome), "still its own stage");
    assert!(e.m.window(WindowId(5)).is_some_and(|w| w.parked));
    assert_eq!(note.moved, 0);
}

#[test]
fn after_the_settle_or_a_click_a_window_listed_elsewhere_is_taken_as_ever() {
    // Arrivals not held (the settle over, or the user clicked or typed since): nothing kept.
    let (mut e, _, _) = laptop_gone(EngineKind::Spaces);
    let a = arrive_with_chrome(&mut e, false);
    assert!(e.m.last_arrival().kept_hidden.is_empty());
    assert!(!a.contains(&Action::Hide { id: WindowId(5) }));
    // The same seen twice (the second look a window of another Desktop needs): it moved.
    arrive_with_chrome(&mut e, false);
    assert_eq!(e.m.space_of(WindowId(5)), Some(e.space_of(STUDIO)), "moved, as today");
}

#[test]
fn the_corner_engine_keeps_its_own_rules_for_a_gone_screens_cards() {
    // Parked windows sit on their Desktop under the Corner engine; macOS moves them with it.
    let (mut e, _, _) = laptop_gone(EngineKind::Corner);
    arrive_with_chrome(&mut e, true);
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
