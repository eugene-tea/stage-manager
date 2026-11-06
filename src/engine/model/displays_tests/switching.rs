//! Switching and the strip on two screens (the Fable review before 1.7, "Switching and the
//! strip"): what the engine answers for a window, an icon click, a hotkey and a hide when the
//! focus or the stage is on the screen that is not the main one. Made-up titles only.

use super::*;

/// [`measured_desk`] with Chrome's second window as a laptop card of its own and a third on
/// the Studio, also a card: (code, chrome, finder, chrome on the laptop again, chrome on the
/// Studio).
fn chrome_everywhere(e: &mut Env) -> [StageId; 5] {
    let [code, _, _, chrome, finder] = measured_desk(e);
    e.open(7, 20, &laptop(), (500.0, 300.0), RuleAction::New);
    e.switch(finder);
    e.open(8, 20, &studio(), (600.0, 300.0), RuleAction::New);
    e.switch(code);
    assert_eq!((e.m.active_stage(STUDIO), e.m.active_stage(LAPTOP)), (Some(code), Some(finder)));
    [code, chrome, finder, e.stage_of(7), e.stage_of(8)]
}

#[test]
fn the_laptops_front_stage_is_in_front_though_the_main_screen_shows_another() {
    // Switching 1: a window of the laptop's front stage is shown (a drag of it can start, its
    // focus switches nothing), as the Studio's front's is.
    let mut e = Env::both();
    let [code, music, _, chrome, finder] = measured_desk(&mut e);
    assert_ne!(e.m.main_active_stage(), Some(finder), "the main screen's front is another stage");
    assert!(e.m.stage_in_front(finder), "the laptop's front");
    assert!(e.m.stage_in_front(code));
    assert!(!e.m.stage_in_front(chrome) && !e.m.stage_in_front(music), "cards");
}

#[test]
fn an_icon_click_on_a_laptop_card_gathers_the_apps_windows_of_the_laptop_only() {
    // Switching 2: the focus is still on the Studio (Code) when the laptop's card is clicked.
    let mut e = Env::both();
    let [code, chrome, _, _, on_studio] = chrome_everywhere(&mut e);
    e.d.focused = Some(WindowId(1));
    let obs = e.d.obs();
    let a = e.m.show_app_everywhere(chrome, 20, &obs);
    e.run(a);
    assert_eq!(e.m.active_stage(LAPTOP), Some(chrome), "the card's stage in front on the laptop");
    assert_eq!(e.stage_of(7), chrome, "the laptop's other Chrome window joined it");
    assert!(!e.parked_on(7, &laptop()) && !e.parked_on(7, &studio()));
    assert_eq!(e.stage_of(8), on_studio, "the Studio's Chrome window stays in its own card there");
    assert!(e.parked_on(8, &studio()));
    assert_eq!(e.m.active_stage(STUDIO), Some(code), "the Studio did not move");
    assert_eq!(e.m.stage(code).map(|s| s.windows.clone()), Some(vec![WindowId(1)]));
}

#[test]
fn a_stage_hotkey_counts_the_strip_of_the_screen_with_the_keyboard() {
    // The Codex review's W3, his word 11-06: the shortcuts and `ctl switch N` count the strip
    // of the screen the keyboard is on (the focused window's), for the check and the switch.
    // The Studio has three stages, the laptop four: 4 is the laptop's alone.
    let mut e = Env::both();
    let [code, music, _, _, finder] = measured_desk(&mut e);
    e.open(9, 22, &laptop(), (300.0, 150.0), RuleAction::New);
    e.open(10, 23, &laptop(), (350.0, 180.0), RuleAction::New);
    let fourth = e.stage_of(10);
    e.switch(finder);
    e.d.focused = Some(WindowId(6));
    let keyboard = e.m.keyboard_display(e.d.focused, None);
    assert_eq!(keyboard, LAPTOP, "Finder's window is on the laptop");
    assert_eq!(e.m.hotkey_space(keyboard), e.space_of(LAPTOP));
    assert_eq!(e.m.stage_by_number(e.m.hotkey_space(keyboard), 4), Some(fourth), "the check finds the laptop's stage 4");
    assert_eq!(e.m.stage_by_number(e.m.hotkey_space(STUDIO), 4), None, "the Studio has no stage 4");
    let obs = e.d.obs();
    let a = e.m.switch_to_number(4, keyboard, &obs);
    e.run(a);
    assert_eq!(e.m.active_stage(LAPTOP), Some(fourth), "the laptop's stage 4");
    assert_eq!(e.m.active_stage(STUDIO), Some(code), "the Studio did not move");
    // The keyboard on the Studio: its stage 2.
    e.d.focused = Some(WindowId(1));
    let keyboard = e.m.keyboard_display(e.d.focused, None);
    assert_eq!(keyboard, STUDIO);
    let obs = e.d.obs();
    let a = e.m.switch_to_number(2, keyboard, &obs);
    e.run(a);
    assert_eq!(e.m.active_stage(STUDIO), Some(music), "the Studio's stage 2");
    assert_eq!(e.m.active_stage(LAPTOP), Some(fourth), "the laptop did not move");
    // A focused window the model does not manage counts where it shows; none, the main screen.
    assert_eq!(e.m.keyboard_display(Some(WindowId(99)), Some(LAPTOP)), LAPTOP);
    assert_eq!(e.m.keyboard_display(None, None), STUDIO);
    assert_eq!(e.m.keyboard_display(None, Some(DisplayId(77))), STUDIO, "a screen the model does not know: the main one");
}

#[test]
fn hiding_an_app_judges_the_screen_its_windows_are_on() {
    // Switching 6: Chrome is wholly on the laptop, a card there.
    let mut e = Env::both();
    let [_, _, _, chrome, _] = measured_desk(&mut e);
    let p = e.m.app_park(20);
    assert_eq!((p.windows, p.shown), (1, 0));
    assert_eq!(p.keep.iter().map(|k| k.0).collect::<Vec<_>>(), vec![5], "the laptop's card waits in place while hidden: {chrome:?}");
    assert!(p.stage_here);
    // The laptop's desktop revealed: its front is empty, its Desktop then known by the
    // slivers alone, so its apps stay parked; the Studio's apps are not concerned.
    let obs = e.d.obs();
    let a = e.m.reveal_desktop_on(Some(LAPTOP), &obs);
    e.run(a);
    assert!(e.m.active_stage(LAPTOP).and_then(|s| e.m.stage(s)).is_some_and(|s| s.windows.is_empty()), "revealed");
    assert!(!e.m.app_park(20).stage_here, "the laptop's front has no window");
    assert!(e.m.app_park(12).stage_here, "Music, on the Studio: its screen's front shows Code");
}
