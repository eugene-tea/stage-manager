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

/// The measured desk's laptop given a second Desktop (a notes window 8 in front, a mail
/// window 9 in a card), then shown its first one again. Returns the second Desktop.
fn laptop_with_two_desktops(e: &mut Env) -> SpaceId {
    e.open(1, 10, &studio(), (200.0, 100.0), RuleAction::Current);
    e.open(5, 20, &laptop(), (100.0, 100.0), RuleAction::Current);
    let first = e.space_of(LAPTOP);
    for (id, pid) in [(8, 30), (9, 31)] {
        e.d.open(&meta(id, pid, &laptop(), (300.0, 200.0)));
    }
    let both = [meta(8, 30, &laptop(), (300.0, 200.0)), meta(9, 31, &laptop(), (300.0, 200.0))];
    let hints = LaunchHints { z_order: vec![WindowId(8), WindowId(9)], frontmost: Some(30) };
    let a = e.m.arrive(LAPTOP, &both, &[WindowId(5)], &hints);
    e.run(a);
    let second = e.space_of(LAPTOP);
    assert_ne!(second, first);
    let notes = e.stage_of(8);
    e.switch(notes);
    assert!(e.m.window(WindowId(9)).is_some_and(|w| w.parked), "mail waits in a card there");
    let a = e.m.arrive(LAPTOP, &[meta(5, 20, &laptop(), (100.0, 100.0))], &[WindowId(8), WindowId(9)], &LaunchHints::default());
    e.run(a);
    assert_eq!(e.space_of(LAPTOP), first);
    second
}

/// The laptop's second Desktop as the window server lists it once it shows.
fn second_listed(e: &Env) -> Vec<WindowMeta> {
    [8u32, 9].iter().map(|id| WindowMeta { frame: e.d.frames[&WindowId(*id)], ..e.m.window(WindowId(*id)).expect("known").meta.clone() }).collect()
}

#[test]
fn a_focus_held_for_another_screens_desktop_acts_once_that_screen_arrives() {
    // ⌘-Tab to mail, on the laptop's other Desktop: the focus comes before the notice and is
    // held. The Studio arrives first, its Desktop unchanged: nothing there is mail's, and the
    // Desktop the focus waits for is not shown yet. The laptop's arrival shows it: mail's
    // stage comes forward there.
    use crate::engine::focus_replay::Wanted;
    let mut e = Env::both();
    let second = laptop_with_two_desktops(&mut e);
    let held = Wanted::Window { id: WindowId(9), pid: 31 };
    assert_eq!(e.m.spaces_awaited(held), vec![second]);
    assert_eq!(e.m.spaces_awaited(Wanted::App(31)), vec![second], "an activation of mail waits for it too");
    assert!(e.m.spaces_awaited(Wanted::App(20)).is_empty(), "an app with a window on a Desktop shown waits for none");
    assert!(e.m.spaces_awaited(Wanted::Window { id: WindowId(77), pid: 31 }).is_empty(), "nor does a window not known");
    let a = e.m.arrive(STUDIO, &[meta(1, 10, &studio(), (200.0, 100.0))], &[], &LaunchHints::default());
    e.run(a);
    assert_eq!(e.m.spaces_awaited(held), vec![second], "the Studio's arrival showed nothing of it");
    let obs = e.d.obs();
    assert!(e.m.bring_focus_forward(Some(WindowId(9)), Some(held), &obs).is_empty(), "nothing to act on yet");
    let listed = second_listed(&e);
    let a = e.m.arrive(LAPTOP, &listed, &[WindowId(5)], &LaunchHints::default());
    e.run(a);
    assert_eq!(e.space_of(LAPTOP), second);
    assert!(e.m.spaces_awaited(held).is_empty(), "its Desktop shows now");
    let obs = e.d.obs();
    let a = e.m.bring_focus_forward(Some(WindowId(9)), Some(held), &obs);
    assert!(a.contains(&Action::Focus { id: WindowId(9) }), "mail comes forward: {a:?}");
    assert_eq!(e.m.active_stage(LAPTOP), Some(e.stage_of(9)));
}

#[test]
fn a_desktop_arranged_ahead_on_another_screen_is_settled_by_that_screens_arrival() {
    // A Dock click on mail arranges the laptop's other Desktop ahead of its slide. The
    // Studio's arrival comes first with the Studio's Desktop unchanged: settled there, the
    // arrangement was undone as "another Desktop came" and mail parked again. It stands
    // until the laptop arrives, where it is confirmed.
    let mut e = Env::both();
    let second = laptop_with_two_desktops(&mut e);
    let (ahead, a) = e.m.arrange_ahead(WindowId(9), None).expect("arranged");
    e.run(a);
    assert_eq!(ahead.space, second);
    assert_eq!(e.m.display_of_space(ahead.space), LAPTOP, "the laptop's arrival settles it");
    let a = e.m.arrive(STUDIO, &[meta(1, 10, &studio(), (200.0, 100.0))], &[], &LaunchHints::default());
    e.run(a);
    assert_ne!(e.space_of(STUDIO), ahead.space);
    let mut settled_early = e.m.clone();
    let undone = settled_early.undo_ahead(&ahead);
    assert!(undone.iter().any(|a| matches!(a, Action::Park { id, .. } if *id == WindowId(9))), "settled at the Studio's arrival, mail was parked again: {undone:?}");
    let listed = second_listed(&e);
    let a = e.m.arrive(LAPTOP, &listed, &[WindowId(5)], &LaunchHints::default());
    e.run(a);
    assert_eq!(e.space_of(LAPTOP), ahead.space, "the Desktop arranged came");
    let a = e.m.finish_ahead(&ahead);
    assert!(a.contains(&Action::Focus { id: WindowId(9) }), "confirmed: {a:?}");
    assert!(e.m.window(WindowId(9)).is_some_and(|w| !w.parked) && e.m.window(WindowId(8)).is_some_and(|w| w.parked));
}

#[test]
fn the_remaining_screen_reported_again_keeps_the_closed_screens_stages_whole() {
    // The lid closed; a moment later the Dock comes onto the Studio (its visible frame
    // shrinks: the same screen reported again); then macOS slides the Studio to its other
    // Desktop with the laptop's two windows on it. The laptop's stages come whole there, as
    // with no second report: that report forgot which windows the closed screen brought, and
    // they went one by one into their apps' stages.
    let mut e = Env::both();
    let [_, _, _, chrome, finder] = measured_desk(&mut e);
    let first = e.space_of(STUDIO);
    // The Studio's other Desktop, visited once: another Chrome window (pid 20, the laptop's app).
    let gone: Vec<(WindowId, Rect)> = [1u32, 3, 4].iter().map(|id| (WindowId(*id), e.d.frames.remove(&WindowId(*id)).expect("open"))).collect();
    let a = e.m.arrive(STUDIO, &[], &[WindowId(1), WindowId(3), WindowId(4)], &LaunchHints::default());
    e.run(a);
    let second = e.space_of(STUDIO);
    assert_ne!(second, first);
    e.open(9, 20, &studio(), (300.0, 300.0), RuleAction::Current);
    let own = e.d.frames.remove(&WindowId(9)).expect("open");
    e.d.frames.extend(gone);
    let live = listed(&e, &[1, 3, 4]);
    let a = e.m.arrive(STUDIO, &live, &[WindowId(9)], &LaunchHints::default());
    e.run(a);
    assert_eq!(e.space_of(STUDIO), first);
    // The lid closes; the Dock comes onto the Studio.
    let a = e.m.set_displays(vec![studio()]);
    e.run(a);
    assert_eq!(e.m.active_stage(STUDIO), Some(finder));
    let mut with_dock = studio();
    with_dock.visible.h -= 80.0;
    let a = e.m.set_displays(vec![with_dock]);
    e.run(a);
    // macOS slides to the other Desktop: its own window, and the laptop's two.
    for id in [1u32, 3, 4] {
        e.d.frames.remove(&WindowId(id));
    }
    e.d.frames.insert(WindowId(9), own);
    let live = listed(&e, &[9, 6, 5]);
    let hints = LaunchHints { z_order: vec![WindowId(6), WindowId(9), WindowId(5)], frontmost: Some(21) };
    let a = e.m.arrive(STUDIO, &live, &[WindowId(1), WindowId(3), WindowId(4)], &hints);
    e.run(a);
    let note = e.m.last_arrival();
    assert_eq!(e.space_of(STUDIO), second);
    assert_eq!((note.carried, note.moved), (2, 0), "the laptop's stages came whole: {note:?}");
    assert_eq!((e.stage_of(6), e.stage_of(5)), (finder, chrome), "the same stages");
    assert_ne!(e.stage_of(5), e.stage_of(9), "Chrome's laptop window keeps its own stage, not its app's stage here");
    assert_eq!(e.m.active_stage(STUDIO), Some(finder), "the laptop's front in front");
}
