//! Arrivals with two screens: at a Space change every screen arrives in turn (the main one,
//! then the others), and what one screen's arrivals count in a row is its own.

use super::*;

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

/// The platform's arrival on the Studio (`shown`: the windows on its screen; `absent`: its
/// windows missing from it), then on the laptop, whose Desktop did not change.
fn arrive_both(e: &mut Env, shown: &[WindowMeta], absent: &[u32], laptop_shown: &[WindowMeta]) {
    let absent: Vec<WindowId> = absent.iter().map(|id| WindowId(*id)).collect();
    let hints = LaunchHints { z_order: shown.iter().map(|m| m.id).collect(), frontmost: None };
    let a = e.m.arrive(STUDIO, shown, &absent, &hints);
    e.run(a);
    let a = e.m.arrive(LAPTOP, laptop_shown, &[], &LaunchHints::default());
    e.run(a);
}

#[test]
fn windows_moved_to_another_desktop_are_taken_at_the_second_arrival_with_a_second_screen() {
    // Most of a Desktop's windows moved to the Studio's other Desktop (Mission Control): seen
    // beside the Desktop shown at two of the Studio's arrivals in a row, they moved there.
    // The laptop's arrival between the two ended the count, and the app recognized the
    // Desktop again every other second for good.
    let mut e = Env::both();
    e.open(1, 10, &studio(), (200.0, 100.0), RuleAction::Current);
    e.open(2, 11, &studio(), (400.0, 100.0), RuleAction::New);
    e.open(3, 12, &studio(), (600.0, 100.0), RuleAction::New);
    e.open(5, 20, &laptop(), (100.0, 100.0), RuleAction::Current);
    let first = e.space_of(STUDIO);
    let on_laptop = [meta(5, 20, &laptop(), (100.0, 100.0))];
    let own: Vec<WindowMeta> = [(7, 40), (8, 41), (9, 42)].iter().map(|(id, pid)| meta(*id, *pid, &studio(), (300.0, 300.0))).collect();
    arrive_both(&mut e, &own, &[1, 2, 3], &on_laptop);
    let second = e.space_of(STUDIO);
    assert_ne!(second, first, "the Studio's second Desktop");
    let mut with_moved = own.clone();
    with_moved.extend([meta(1, 10, &studio(), (200.0, 100.0)), meta(2, 11, &studio(), (400.0, 100.0))]);
    arrive_both(&mut e, &with_moved, &[3], &on_laptop);
    assert_eq!(e.space_of(STUDIO), second);
    assert_eq!(e.m.last_arrival().moved, 0);
    assert_eq!(e.m.space_of(WindowId(1)), Some(first), "first sight: maybe a Desktop sliding out");
    arrive_both(&mut e, &with_moved, &[3], &on_laptop);
    assert_eq!(e.space_of(STUDIO), second);
    assert_eq!(e.m.space_of(WindowId(1)), Some(second), "seen again on the Studio: moved here");
    assert_eq!(e.m.space_of(WindowId(2)), Some(second));
    assert_eq!(e.m.space_of(WindowId(3)), Some(first), "the window left behind stays");
    assert_eq!(e.space_of(LAPTOP), e.m.space_of(WindowId(5)).expect("managed"), "the laptop's Desktop untouched");
}

#[test]
fn a_window_on_every_desktop_becomes_sticky_with_a_second_screen() {
    // A window assigned to all Desktops shows on each of the Studio's Desktops gone to: three
    // of the Studio's arrivals in a row make it sticky, whatever the laptop's arrivals
    // between them list.
    let mut e = Env::both();
    e.open(1, 10, &studio(), (200.0, 100.0), RuleAction::Current);
    e.open(7, 50, &studio(), (900.0, 100.0), RuleAction::New);
    e.open(5, 20, &laptop(), (100.0, 100.0), RuleAction::Current);
    let on_laptop = [meta(5, 20, &laptop(), (100.0, 100.0))];
    let clock = meta(7, 50, &studio(), (900.0, 100.0));
    let first = [meta(1, 10, &studio(), (200.0, 100.0)), clock.clone()];
    let second = [meta(2, 11, &studio(), (300.0, 300.0)), clock.clone()];
    arrive_both(&mut e, &second, &[1], &on_laptop);
    assert!(!e.m.is_sticky(WindowId(7)), "seen once elsewhere: a move, as far as we know");
    arrive_both(&mut e, &first, &[2], &on_laptop);
    assert!(!e.m.is_sticky(WindowId(7)), "seen back where it came from: maybe moved there and straight back");
    arrive_both(&mut e, &second, &[1], &on_laptop);
    assert!(e.m.is_sticky(WindowId(7)), "on every Desktop of its screen, three times in a row");
    assert_eq!(e.m.window(WindowId(7)).map(|w| w.home), Some(Home::Pinned));
}
