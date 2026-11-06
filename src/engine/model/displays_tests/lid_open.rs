//! The lid opened after the user switched on the remaining screen (his run 11-06 12:00:35:
//! the laptop's front and a laptop card he had used on the 5K while the lid was closed never
//! came back to the laptop; the card he had not touched did). Apple's rule: the stages macOS
//! moves back are the laptop's again, the window it shows there at a normal frame its front,
//! the ones it puts in the laptop's park zone its cards, whatever the 5K had made of them.
//! Made-up titles only.

use super::*;

/// The desk before the close: on the Studio, Code in front, Music a card; on the laptop,
/// Chrome in front, Notes and Disk cards. Returns (code, music, chrome, notes, disk).
fn desk(e: &mut Env) -> [StageId; 5] {
    e.open(1, 10, &studio(), (200.0, 100.0), RuleAction::Current); // Code
    e.open(3, 12, &studio(), (900.0, 300.0), RuleAction::New); // Music
    let code = e.stage_of(1);
    e.switch(code);
    e.open(7, 22, &laptop(), (300.0, 150.0), RuleAction::Current); // Disk
    e.open(6, 21, &laptop(), (400.0, 200.0), RuleAction::New); // Notes
    e.open(5, 20, &laptop(), (100.0, 100.0), RuleAction::New); // Chrome
    let chrome = e.stage_of(5);
    assert_eq!(e.m.active_stage(LAPTOP), Some(chrome));
    [code, e.stage_of(3), chrome, e.stage_of(6), e.stage_of(7)]
}

/// Where macOS puts window `id` back at the laptop's park corner.
fn laptop_corner(e: &Env, id: u32) -> Rect {
    park_frame_at(e.d.frames[&WindowId(id)].size(), laptop().frame, laptop().corner)
}

/// macOS puts `id` back at `frame` on the laptop: the platform's reconcile hands a parked
/// window listed there to the display reconcile ([`Model::returned_display`] within the
/// settle after the laptop came back), a shown one comes from the display check.
fn put_back(e: &mut Env, id: u32, frame: Rect, z: &[WindowId]) -> Vec<Action> {
    let w = e.m.window(WindowId(id)).expect("managed");
    if w.parked {
        assert_eq!(e.m.returned_display(frame, w.meta.display, &[LAPTOP]), Some(LAPTOP), "w{id} is the laptop's again");
    }
    e.d.frames.insert(WindowId(id), frame);
    let (a, _) = e.m.windows_moved_to_display(&[(WindowId(id), LAPTOP, frame)], z);
    e.run(a)
}

#[test]
fn the_lid_opened_after_a_switch_on_the_5k_gives_the_laptop_its_front_and_cards_back() {
    let mut e = Env::both();
    let [code, _, chrome, notes, disk] = desk(&mut e);
    let chrome_frame = e.d.frames[&WindowId(5)];
    let notes_restore = e.m.window(WindowId(6)).map(|w| w.restore).expect("Notes");
    let a = e.m.set_displays(vec![studio()]);
    e.run(a);
    assert_eq!(e.m.active_stage(STUDIO), Some(chrome), "the laptop's front in front on the 5K");
    // On the 5K he clicks Notes' card: Chrome goes to a card at the 5K's corner.
    e.switch(notes);
    assert!(e.m.window(WindowId(5)).is_some_and(|w| w.parked) && e.parked_on(5, &studio()));
    assert!(e.m.window(WindowId(6)).is_some_and(|w| !w.parked));
    // The lid opens.
    let a = e.m.set_displays(vec![studio(), laptop()]);
    e.run(a);
    let z = [WindowId(7), WindowId(5), WindowId(6), WindowId(1)];
    // Disk (untouched) at the laptop's corner first, then Chrome at its laptop frame: a
    // normal frame on the laptop, not the 5K's park zone, is not "parked again" at the 5K's
    // corner (the old check: no other screen's park zone held it).
    let disk_at = laptop_corner(&e, 7);
    put_back(&mut e, 7, disk_at, &z);
    assert!(!studio().parks(chrome_frame));
    assert_eq!(e.m.returned_display(chrome_frame, STUDIO, &[]), None, "outside a settle after an add: the old rule");
    let a = put_back(&mut e, 5, chrome_frame, &z);
    assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } | Action::Park { id, .. } if *id == WindowId(5))), "Chrome stays where macOS put it: {a:?}");
    assert_eq!(e.m.active_stage(LAPTOP), Some(chrome), "Chrome in front on the laptop");
    assert_eq!(e.m.window(WindowId(5)).map(|w| (w.parked, w.restore)), Some((false, chrome_frame)));
    assert_eq!(e.m.active_stage(STUDIO), Some(notes), "the 5K's front untouched so far");
    // Notes, shown on the 5K, macOS puts at the laptop's corner, where it was a card: a card
    // there, not a front shown in the corner.
    let notes_at = laptop_corner(&e, 6);
    put_back(&mut e, 6, notes_at, &z);
    let w = e.m.window(WindowId(6)).expect("Notes");
    assert!(w.parked && w.meta.display == LAPTOP, "Notes a laptop card");
    assert!(e.parked_on(6, &laptop()));
    assert!(!laptop().parks(w.restore) && w.restore.intersect(&laptop().visible).is_some(), "it comes back on the laptop: {:?}", w.restore);
    assert_eq!((w.restore.w, w.restore.h), (notes_restore.w, notes_restore.h));
    assert_eq!(e.m.active_stage(LAPTOP), Some(chrome), "Chrome stays in front");
    let mut slots = e.m.strip_slots(LAPTOP);
    slots.sort();
    let mut want = vec![notes, disk];
    want.sort();
    assert_eq!(slots, want, "the laptop's cards");
    for s in [chrome, notes, disk] {
        assert_eq!(e.m.stage(s).map(|s| s.display), Some(LAPTOP));
    }
    // The 5K's front went with Notes: its own most recent stage comes forward, shown.
    assert_eq!(e.m.active_stage(STUDIO), Some(code));
    assert!(e.m.window(WindowId(1)).is_some_and(|w| !w.parked) && !e.parked_on(1, &studio()));
    assert!(e.m.stages().iter().filter(|s| s.display == STUDIO).all(|s| s.windows.iter().all(|w| [1, 3].contains(&w.0))));
}

#[test]
fn the_lid_opened_with_nothing_touched_on_the_5k_stays_as_before() {
    let mut e = Env::both();
    let [code, _, chrome, notes, disk] = desk(&mut e);
    let chrome_frame = e.d.frames[&WindowId(5)];
    let a = e.m.set_displays(vec![studio()]);
    e.run(a);
    let a = e.m.set_displays(vec![studio(), laptop()]);
    e.run(a);
    let z = [WindowId(5), WindowId(7), WindowId(6), WindowId(1)];
    let (disk_at, notes_at) = (laptop_corner(&e, 7), laptop_corner(&e, 6));
    put_back(&mut e, 7, disk_at, &z);
    put_back(&mut e, 6, notes_at, &z);
    let a = put_back(&mut e, 5, chrome_frame, &z);
    assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } if *id == WindowId(5))), "{a:?}");
    assert_eq!(e.m.active_stage(LAPTOP), Some(chrome));
    assert!(e.m.window(WindowId(5)).is_some_and(|w| !w.parked));
    for id in [6, 7] {
        assert!(e.m.window(WindowId(id)).is_some_and(|w| w.parked) && e.parked_on(id, &laptop()), "w{id} a laptop card");
    }
    let mut slots = e.m.strip_slots(LAPTOP);
    slots.sort();
    let mut want = vec![notes, disk];
    want.sort();
    assert_eq!(slots, want);
    assert_eq!(e.m.active_stage(STUDIO), Some(code), "the 5K's own front again");
}

#[test]
fn only_a_screen_that_came_back_takes_a_parked_window_listed_at_a_normal_frame() {
    let e = Env::both();
    let on_laptop = on(&laptop(), (300.0, 200.0));
    let on_studio = on(&studio(), (300.0, 200.0));
    let studio_corner = park_frame_at(on_studio.size(), studio().frame, studio().corner);
    let laptop_corner = park_frame_at(on_laptop.size(), laptop().frame, laptop().corner);
    assert_eq!(e.m.returned_display(on_laptop, STUDIO, &[LAPTOP]), Some(LAPTOP));
    assert_eq!(e.m.returned_display(on_laptop, STUDIO, &[]), None, "no screen came back: parked again");
    assert_eq!(e.m.returned_display(on_studio, STUDIO, &[LAPTOP]), None, "on its own screen: parked again");
    assert_eq!(e.m.returned_display(studio_corner, STUDIO, &[LAPTOP]), None, "in its own zone: a card still");
    assert_eq!(e.m.returned_display(laptop_corner, STUDIO, &[]), Some(LAPTOP), "another screen's zone, as before");
}
