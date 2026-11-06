//! A strip and stages on every screen (round displays): Apple's behaviour as measured on
//! the owner's Mac 11-05 (a 5K Studio Display main, the MacBook's screen to its left, their
//! edges touching), the unmeasured cases decided, `[displays] main_only`, and `state.json`
//! keyed by the screens' identities. Made-up titles only.

use super::*;
use crate::engine::displays::ScreenKey;
use crate::engine::park::{Corner, choose_corner, park_frame_at};
use crate::engine::persist::LaunchHints;
use crate::engine::rules::{Decision, RuleAction};
use crate::engine::testkit::Desk;

const STUDIO: DisplayId = DisplayId(1);
const LAPTOP: DisplayId = DisplayId(2);
const STUDIO_KEY: ScreenKey = ScreenKey::new(0x610, 0xae2f, 0x4d6);
const LAPTOP_KEY: ScreenKey = ScreenKey::new(0x610, 0xa050, 0);

fn studio() -> Display {
    let frame = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    let visible = Rect::new(0.0, 25.0, 2560.0, 1415.0);
    Display { id: STUDIO, key: STUDIO_KEY, frame, visible, corner: choose_corner(frame, visible, &[laptop_frame()]).corner }
}

fn laptop_frame() -> Rect {
    Rect::new(-1800.0, 271.0, 1800.0, 1169.0)
}

fn laptop() -> Display {
    let frame = laptop_frame();
    let visible = Rect::new(-1800.0, 310.0, 1800.0, 1130.0);
    Display { id: LAPTOP, key: LAPTOP_KEY, frame, visible, corner: choose_corner(frame, visible, &[studio().frame]).corner }
}

/// A frame on a display, `at` points in from its visible frame's top-left corner.
fn on(d: &Display, at: (f64, f64)) -> Rect {
    Rect::new(d.visible.x + at.0, d.visible.y + at.1, 800.0, 600.0)
}

struct Env {
    m: Model,
    d: Desk,
}

impl Env {
    fn both() -> Self {
        Self { m: Model::new(vec![studio(), laptop()], ModelOptions::default()), d: Desk::default() }
    }

    fn run(&mut self, actions: Vec<Action>) -> Vec<Action> {
        apply_plain(&mut self.d, &actions);
        actions
    }

    /// Opens window `id` of `pid` on `display`, `at` points in from its corner, by `action`.
    fn open(&mut self, id: u32, pid: Pid, display: &Display, at: (f64, f64), action: RuleAction) -> Vec<Action> {
        let meta = WindowMeta {
            id: WindowId(id),
            pid,
            bundle_id: format!("com.example.app{pid}"),
            title: format!("Doc {id}"),
            role: WindowRole::Standard,
            frame: on(display, at),
            display: display.id,
        };
        self.d.open(&meta);
        let obs = self.d.obs();
        let a = self.m.add_window(meta, &Decision { action, ..Decision::default() }, &obs);
        self.run(a)
    }

    fn stage_of(&self, id: u32) -> StageId {
        self.m.stage_of(WindowId(id)).expect("in a stage")
    }

    fn space_of(&self, display: DisplayId) -> SpaceId {
        self.m.current_space(display)
    }

    /// True when window `id`'s frame on the desk is in `display`'s park zone.
    fn parked_on(&self, id: u32, display: &Display) -> bool {
        self.d.frames.get(&WindowId(id)).is_some_and(|f| display.parks(*f))
    }

    fn switch(&mut self, stage: StageId) -> Vec<Action> {
        let obs = self.d.obs();
        let a = self.m.switch_to(stage, &obs);
        self.run(a)
    }

    fn moved(&mut self, moves: &[(u32, &Display, (f64, f64))]) -> (Vec<Action>, Vec<WindowId>) {
        let list: Vec<(WindowId, DisplayId, Rect)> = moves.iter().map(|(id, d, at)| (WindowId(*id), d.id, on(d, *at))).collect();
        for (id, _, f) in &list {
            self.d.frames.insert(*id, *f);
        }
        let z = self.d.z.clone();
        let (a, forgotten) = self.m.windows_moved_to_display(&list, &z);
        apply_plain(&mut self.d, &a);
        (a, forgotten)
    }
}

/// Executes actions on the desk with every frame taken as asked: the desk's own `apply`
/// clamps against the one test display, and here windows park at two screens' corners.
fn apply_plain(d: &mut Desk, actions: &[Action]) {
    for a in actions {
        match *a {
            Action::SetFrame { id, frame } | Action::Park { id, frame } => {
                if let Some(f) = d.frames.get_mut(&id) {
                    *f = frame;
                }
            }
            Action::Raise { id } => d.raise(id),
            Action::Focus { id } => {
                if d.frames.contains_key(&id) {
                    d.raise(id);
                    d.focused = Some(id);
                }
            }
            Action::StateChanged => {}
        }
    }
}

/// The owner's desk as measured: on the Studio, Code in front, Music and Terminal cards; on
/// the laptop, Finder in front, Chrome a card. Returns (code, music, terminal, chrome, finder).
fn measured_desk(e: &mut Env) -> [StageId; 5] {
    e.open(1, 10, &studio(), (200.0, 100.0), RuleAction::Current); // Code
    e.open(3, 12, &studio(), (900.0, 300.0), RuleAction::New); // Music
    e.open(4, 13, &studio(), (1200.0, 400.0), RuleAction::New); // Terminal
    let code = e.stage_of(1);
    e.switch(code);
    e.open(5, 20, &laptop(), (100.0, 100.0), RuleAction::Current); // Chrome
    e.open(6, 21, &laptop(), (400.0, 200.0), RuleAction::New); // Finder
    [code, e.stage_of(3), e.stage_of(4), e.stage_of(5), e.stage_of(6)]
}

#[test]
fn every_screen_has_its_own_space_front_stage_and_strip() {
    let mut e = Env::both();
    let [code, music, terminal, chrome, finder] = measured_desk(&mut e);
    assert_ne!(e.space_of(STUDIO), e.space_of(LAPTOP));
    assert_eq!(e.m.active_stage(STUDIO), Some(code));
    assert_eq!(e.m.active_stage(LAPTOP), Some(finder));
    let mut studio_slots = e.m.strip_slots(STUDIO);
    studio_slots.sort();
    assert_eq!(studio_slots, vec![music, terminal]);
    assert_eq!(e.m.strip_slots(LAPTOP), vec![chrome]);
    assert!(e.parked_on(3, &studio()) && e.parked_on(4, &studio()), "the Studio's cards park at its corner");
    assert!(e.parked_on(5, &laptop()), "the laptop's card parks at its own corner");
    assert!(!e.parked_on(5, &studio()));
    assert!(e.m.stages().iter().all(|s| s.display == e.m.display_of_space(s.space)));
}

#[test]
fn the_park_corner_is_where_no_other_screen_lies_beyond() {
    // His arrangement: the laptop's right edge touches the Studio's left edge.
    assert_eq!(laptop().corner, Corner::BottomLeft);
    assert_eq!(studio().corner, Corner::BottomRight);
    // A laptop below a 5K: bottom-right still (nothing right of or below it).
    let below = Rect::new(500.0, 1440.0, 1512.0, 982.0);
    assert_eq!(choose_corner(below, below, &[studio().frame]).corner, Corner::BottomRight);
    assert_eq!(choose_corner(studio().frame, studio().visible, &[below]).corner, Corner::BottomRight, "the laptop under the left half");
    // A display to the right of the main one, as tall: bottom-left (the body would land on
    // it); a shorter one leaves the bottom-right corner's reach clear.
    let right = Rect::new(2560.0, 0.0, 1920.0, 1440.0);
    assert_eq!(choose_corner(studio().frame, studio().visible, &[right]).corner, Corner::BottomLeft);
    let shorter = Rect::new(2560.0, 0.0, 1920.0, 1080.0);
    assert_eq!(choose_corner(studio().frame, studio().visible, &[shorter]).overlap, 0.0);
    // Screens on every side: the least overlap, as today, and reported.
    let main = Rect::new(0.0, 0.0, 1000.0, 1000.0);
    let around = [
        Rect::new(-1000.0, 0.0, 1000.0, 1000.0),
        Rect::new(1000.0, 0.0, 1000.0, 1000.0),
        Rect::new(0.0, -1000.0, 1000.0, 1000.0),
        Rect::new(0.0, 1000.0, 1000.0, 1000.0),
    ];
    let choice = choose_corner(main, main, &around);
    assert!(choice.overlap > 0.0, "every corner has a screen beyond it");
}

#[test]
fn a_window_dragged_to_another_screen_starts_a_stage_of_its_own_there_in_front() {
    // Measured: Code dragged from the Studio to the laptop: Code alone in front there,
    // Finder to a card beside Chrome; on the Studio, Music came in front.
    let mut e = Env::both();
    let [code, music, terminal, chrome, finder] = measured_desk(&mut e);
    let (a, forgotten) = e.moved(&[(1, &laptop(), (300.0, 200.0))]);
    assert!(forgotten.is_empty());
    let code_now = e.stage_of(1);
    assert_eq!(code_now, code, "a stage of one window moves whole: the same stage");
    assert_eq!(e.m.stage(code_now).map(|s| s.space), Some(e.space_of(LAPTOP)));
    assert_eq!(e.m.stage(code_now).map(|s| s.display), Some(LAPTOP));
    assert_eq!(e.m.active_stage(LAPTOP), Some(code_now), "Code alone in front on the laptop");
    assert_eq!(e.m.stage(code_now).map(|s| s.windows.clone()), Some(vec![WindowId(1)]));
    assert!(!e.parked_on(1, &laptop()) && !e.parked_on(1, &studio()));
    assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } if *id == WindowId(1))), "a shown window is not moved: its frame on the laptop is the user's");
    assert_eq!(e.m.window(WindowId(1)).map(|w| w.restore), Some(on(&laptop(), (300.0, 200.0))));
    assert!(e.parked_on(6, &laptop()), "Finder went to a card, parked at the laptop's corner");
    let slots = e.m.strip_slots(LAPTOP);
    assert!(slots.contains(&chrome) && slots.contains(&finder) && slots.len() == 2, "{slots:?}");
    // The Studio: its front stage left, the most recently used card (Terminal, opened
    // after Music here; Music on the owner's desk) came forward and the other keeps its card.
    assert_eq!(e.m.active_stage(STUDIO), Some(terminal));
    assert!(!e.parked_on(4, &studio()), "Terminal shown");
    assert!(e.parked_on(3, &studio()));
    assert_eq!(e.m.strip_slots(STUDIO), vec![music]);
}

#[test]
fn a_card_click_changes_only_its_own_screen() {
    let mut e = Env::both();
    let [code, _, _, chrome, _] = measured_desk(&mut e);
    e.switch(chrome);
    assert_eq!(e.m.active_stage(LAPTOP), Some(chrome));
    assert!(!e.parked_on(5, &laptop()) && e.parked_on(6, &laptop()), "Chrome in front, Finder took its slot");
    assert_eq!(e.m.active_stage(STUDIO), Some(code), "the Studio did not move");
    assert!(!e.parked_on(1, &studio()));
}

#[test]
fn a_new_window_opens_as_its_own_stage_on_the_screen_it_shows_on() {
    // Measured: Calculator opened on the Studio (keyboard focus there) while the pointer
    // was on the laptop: its own stage there, Code to a card; the laptop untouched.
    let mut e = Env::both();
    let [code, _, _, _, finder] = measured_desk(&mut e);
    e.open(7, 30, &studio(), (1000.0, 500.0), RuleAction::Native);
    let calc = e.stage_of(7);
    assert_eq!(e.m.stage(calc).map(|s| s.space), Some(e.space_of(STUDIO)));
    assert_eq!(e.m.active_stage(STUDIO), Some(calc));
    assert!(e.parked_on(1, &studio()) && e.m.strip_slots(STUDIO).contains(&code));
    assert_eq!(e.m.active_stage(LAPTOP), Some(finder));
    assert!(!e.parked_on(6, &laptop()));
}

#[test]
fn a_window_dragged_out_of_a_stage_of_several_leaves_the_rest_in_their_stage() {
    // Unmeasured, decided: the rest stay; the stage left keeps the front.
    let mut e = Env::both();
    e.open(1, 10, &studio(), (200.0, 100.0), RuleAction::Current);
    e.open(2, 11, &studio(), (600.0, 300.0), RuleAction::Current);
    let shared = e.stage_of(1);
    assert_eq!(e.stage_of(2), shared);
    e.moved(&[(2, &laptop(), (100.0, 100.0))]);
    assert_eq!(e.m.stage(shared).map(|s| s.windows.clone()), Some(vec![WindowId(1)]));
    assert_eq!(e.m.active_stage(STUDIO), Some(shared), "still in front: not emptied");
    let own = e.stage_of(2);
    assert_ne!(own, shared);
    assert_eq!(e.m.active_stage(LAPTOP), Some(own));
    assert_eq!(e.m.stage(own).map(|s| s.key), Some(Some(WindowId(2))));
}

#[test]
fn several_windows_moved_at_once_put_the_frontmost_in_front_and_the_rest_in_the_strip() {
    let mut e = Env::both();
    e.open(1, 10, &studio(), (200.0, 100.0), RuleAction::Current);
    e.open(2, 11, &studio(), (600.0, 300.0), RuleAction::Current);
    e.open(3, 12, &studio(), (900.0, 300.0), RuleAction::Current);
    e.d.raise(WindowId(2));
    e.moved(&[(1, &laptop(), (10.0, 10.0)), (2, &laptop(), (20.0, 20.0))]);
    let (s1, s2) = (e.stage_of(1), e.stage_of(2));
    assert_ne!(s1, s2, "different apps never share a stage");
    assert_eq!(e.m.active_stage(LAPTOP), Some(s2), "the frontmost moved window's stage comes in front");
    assert!(e.parked_on(1, &laptop()) && !e.parked_on(2, &laptop()));
    assert_eq!(e.m.strip_slots(LAPTOP), vec![s1]);
    assert!(!e.parked_on(3, &studio()), "the window left behind keeps the Studio's front");
}

#[test]
fn the_lid_closed_brings_that_screens_stages_to_the_remaining_one() {
    // Measured: the laptop's front (Code) comes to the Studio as its front, the Studio's
    // front (Music) goes to a card, the laptop's other stages (Chrome, Finder) become
    // stages there too, after the Studio's own.
    let mut e = Env::both();
    let [_, music, terminal, chrome, finder] = measured_desk(&mut e);
    e.moved(&[(1, &laptop(), (300.0, 200.0))]);
    let code = e.stage_of(1);
    assert_eq!(e.m.active_stage(STUDIO), Some(terminal));
    assert_eq!(e.m.active_stage(LAPTOP), Some(code));
    // macOS moves every laptop window onto the Studio; the desk follows for the shown one.
    e.d.frames.insert(WindowId(1), Rect::new(400.0, 300.0, 800.0, 600.0));
    let a = e.m.set_displays(vec![studio()]);
    e.run(a.clone());
    assert_eq!(e.m.displays().len(), 1);
    assert_eq!(e.m.active_stage(STUDIO), Some(e.stage_of(1)), "Code in front on the Studio");
    assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } if *id == WindowId(1))), "Code keeps the frame macOS gave it");
    assert!(e.parked_on(4, &studio()), "Terminal went to a card");
    for id in [5, 6] {
        assert!(e.parked_on(id, &studio()), "w{id} parked at the Studio's corner now");
        assert_eq!(e.m.window(WindowId(id)).map(|w| w.meta.display), Some(STUDIO));
        let restore = e.m.window(WindowId(id)).map(|w| w.restore).expect("restore");
        assert!(!studio().parks(restore) && studio().visible.intersect(&restore).is_some(), "{restore:?} within reach of the Studio");
    }
    let slots = e.m.strip_slots(STUDIO);
    let (mut own, mut theirs) = (slots[..2].to_vec(), slots[2..].to_vec());
    own.sort();
    theirs.sort();
    assert_eq!((own, theirs), (vec![music, terminal], vec![chrome, finder]), "the Studio's own cards first, then the laptop's: {slots:?}");
    assert!(e.m.stages().iter().all(|s| s.display == STUDIO && s.space == e.space_of(STUDIO)));
    assert!(e.m.spaces().iter().all(|s| s.display == STUDIO));
    // Past the capacity the laptop's stages are hidden, not the Studio's own.
    let mut e = Env::both();
    e.m.set_display_capacity(STUDIO, 2);
    let [code, music, terminal, chrome, finder] = measured_desk(&mut e);
    let a = e.m.set_displays(vec![studio()]);
    e.run(a);
    // Finder (the laptop's front) in front; Code to a card, evicting the least recent
    // (Music) as Apple's swap does; Chrome past the capacity, out of view.
    assert_eq!(e.m.active_stage(STUDIO), Some(finder));
    let mut slots = e.m.strip_slots(STUDIO);
    slots.sort();
    assert_eq!(slots, vec![code, terminal], "{slots:?}");
    assert!(e.m.stage(music).is_some() && e.m.stage(chrome).is_some(), "still stages, out of view");
    assert!(e.parked_on(1, &studio()) && e.parked_on(5, &studio()) && !e.parked_on(6, &studio()));
}

#[test]
fn an_empty_front_on_the_remaining_screen_gives_way_to_the_removed_screens_front() {
    let mut e = Env::both();
    e.open(5, 20, &laptop(), (100.0, 100.0), RuleAction::Current);
    let chrome = e.stage_of(5);
    let empty = e.m.active_stage(STUDIO).expect("an empty front");
    let a = e.m.set_displays(vec![studio()]);
    e.run(a);
    assert_eq!(e.m.active_stage(STUDIO), Some(chrome));
    assert!(e.m.stage(empty).is_none(), "the placeholder went");
    assert!(!e.parked_on(5, &studio()));
}

#[test]
fn the_lid_opened_gives_the_screen_back_the_stages_macos_moves_there() {
    // Measured: Chrome and Finder (the ones not in front when the lid closed) go back to
    // the laptop: its stages again, Chrome in front, Finder a card; Code stays where it is.
    let mut e = Env::both();
    let [_, music, _, chrome, finder] = measured_desk(&mut e);
    // Chrome's stage holds two windows: a stage moved whole stays one stage.
    e.switch(chrome);
    e.open(8, 20, &laptop(), (500.0, 300.0), RuleAction::Native);
    assert_eq!(e.stage_of(8), chrome);
    e.switch(finder);
    e.moved(&[(1, &laptop(), (300.0, 200.0))]);
    let a = e.m.set_displays(vec![studio()]);
    e.run(a);
    let code = e.stage_of(1);
    assert_eq!(e.m.active_stage(STUDIO), Some(code));
    // The lid opens: the laptop is back under a new id, with its identity.
    let back = Display { id: DisplayId(9), ..laptop() };
    let a = e.m.set_displays(vec![studio(), back]);
    e.run(a);
    assert_eq!(e.m.displays().len(), 2);
    let empty = e.m.active_stage(DisplayId(9)).expect("an empty front on the laptop");
    assert!(e.m.stage(empty).is_some_and(|s| s.windows.is_empty()));
    // macOS moves Chrome's two windows and Finder back, Chrome's in front.
    e.d.raise(WindowId(5));
    let (a, _) = e.moved(&[(5, &back, (100.0, 100.0)), (8, &back, (500.0, 300.0)), (6, &back, (400.0, 200.0))]);
    assert_eq!(e.stage_of(5), chrome, "Chrome's stage is the same stage");
    assert_eq!(e.stage_of(8), chrome, "with both its windows");
    assert_eq!(e.stage_of(6), finder);
    assert_eq!(e.m.stage(chrome).map(|s| s.display), Some(DisplayId(9)));
    assert_eq!(e.m.active_stage(DisplayId(9)), Some(chrome), "Chrome in front on the laptop");
    // Shown where macOS put them (the frames they had on the laptop), not moved to a frame
    // carried over from the Studio (round displays-lid).
    for id in [5, 8] {
        assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { id: x, .. } if *x == WindowId(id))), "w{id} stays where macOS put it: {a:?}");
        assert!(e.m.window(WindowId(id)).is_some_and(|w| !w.parked && w.restore == e.d.frames[&WindowId(id)]));
    }
    assert!(!e.parked_on(5, &back) && !e.parked_on(8, &back));
    assert!(e.parked_on(6, &back), "Finder a card, parked at the laptop's corner");
    assert_eq!(e.m.strip_slots(DisplayId(9)), vec![finder]);
    assert!(e.m.stage(empty).is_none(), "the empty front gave way");
    // The Studio keeps Code in front and Music as a card.
    assert_eq!(e.m.active_stage(STUDIO), Some(code));
    assert_eq!(e.m.stage(code).map(|s| s.display), Some(STUDIO));
    assert!(e.m.strip_slots(STUDIO).contains(&music));
    assert!(e.m.stages().iter().filter(|s| s.display == DisplayId(9)).count() == 2);
}

#[test]
fn the_lid_opened_puts_back_a_card_as_a_card_and_a_shown_window_where_macos_put_it() {
    // 11-05 16:27:31: Finder (parked on the 5K by then) came back shown at the frame it had
    // on the laptop, Chrome at the laptop's old park corner. Finder's stage comes in front
    // there at that frame, not at a frame carried over from the 5K (the "window repositioned
    // a bit"); Chrome is a card still, and first in the window list it brings nothing forward.
    let mut e = Env::both();
    let [code, _, _, chrome, finder] = measured_desk(&mut e);
    let finder_frame = e.d.frames[&WindowId(6)];
    let a = e.m.set_displays(vec![studio()]);
    e.run(a);
    // On the 5K the owner drags Finder, then a card click parks it: its restore frame is a 5K one.
    e.d.frames.insert(WindowId(6), Rect::new(576.0, 721.0, 800.0, 600.0));
    e.switch(code);
    assert!(e.m.window(WindowId(6)).is_some_and(|w| w.parked));
    let back = Display { id: DisplayId(9), ..laptop() };
    let a = e.m.set_displays(vec![studio(), back]);
    e.run(a);
    let empty = e.m.active_stage(DisplayId(9)).expect("an empty front");
    // macOS puts both back: Finder at its laptop frame, Chrome at the laptop's park corner,
    // its sliver first in the window list.
    let chrome_park = park_frame_at(e.d.frames[&WindowId(5)].size(), back.frame, back.corner);
    e.d.frames.insert(WindowId(6), finder_frame);
    e.d.frames.insert(WindowId(5), chrome_park);
    let z = vec![WindowId(5), WindowId(6), WindowId(1)];
    let (a, _) = e.m.windows_moved_to_display(&[(WindowId(5), DisplayId(9), chrome_park), (WindowId(6), DisplayId(9), finder_frame)], &z);
    e.run(a.clone());
    assert_eq!(e.m.active_stage(DisplayId(9)), Some(finder), "Finder's stage in front on the laptop");
    assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } if *id == WindowId(6))), "Finder stays where macOS put it: {a:?}");
    assert_eq!(e.m.window(WindowId(6)).map(|w| (w.parked, w.restore)), Some((false, finder_frame)));
    assert_eq!(e.d.frames[&WindowId(6)], finder_frame);
    assert_eq!(e.stage_of(5), chrome);
    assert!(e.m.window(WindowId(5)).is_some_and(|w| w.parked), "Chrome a card still");
    assert!(e.parked_on(5, &back));
    assert_eq!(e.m.strip_slots(DisplayId(9)), vec![chrome]);
    assert!(e.m.stage(empty).is_none(), "the empty front gave way");
    assert_eq!(e.m.active_stage(STUDIO), Some(code));
    // Only a card comes back (Finder stayed on the 5K): nothing comes forward on the laptop.
    let mut e = Env::both();
    let [_, _, _, chrome, finder] = measured_desk(&mut e);
    let a = e.m.set_displays(vec![studio()]);
    e.run(a);
    let a = e.m.set_displays(vec![studio(), back]);
    e.run(a);
    let empty = e.m.active_stage(DisplayId(9)).expect("an empty front");
    let chrome_park = park_frame_at(e.d.frames[&WindowId(5)].size(), back.frame, back.corner);
    e.d.frames.insert(WindowId(5), chrome_park);
    let (a, _) = e.m.windows_moved_to_display(&[(WindowId(5), DisplayId(9), chrome_park)], &[WindowId(5), WindowId(6)]);
    e.run(a.clone());
    assert_eq!(e.m.active_stage(DisplayId(9)), Some(empty), "the empty front stays: a card put back brings nothing forward");
    assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } if *id == WindowId(5))), "{a:?}");
    assert_eq!(e.stage_of(5), chrome);
    assert!(e.m.window(WindowId(5)).is_some_and(|w| w.parked) && e.parked_on(5, &back));
    assert_eq!(e.m.strip_slots(DisplayId(9)), vec![chrome]);
    assert_eq!(e.m.active_stage(STUDIO), Some(finder), "the 5K untouched");
}

#[test]
fn the_lid_closed_with_macos_showing_another_desktop_keeps_the_laptops_stages_whole() {
    // 11-05 16:27:07: the lid closed and macOS slid the 5K to its second Desktop (nine
    // windows not shown since the launch) with the laptop's two windows on it. That
    // Desktop's stages are rebuilt from state.json; the laptop's front stage comes in front
    // there whole, the laptop's card a card of its own after the Desktop's own (not joined to
    // its app's stage there), the Desktop's own front a card; the 5K's first Desktop keeps
    // its stages.
    // The 5K's second Desktop, saved: a Chrome window (pid 20, as the laptop's) and Mail.
    let mut other = Env::both();
    other.open(9, 20, &studio(), (300.0, 300.0), RuleAction::Current);
    other.open(10, 40, &studio(), (700.0, 300.0), RuleAction::New);
    let saved = other
        .m
        .to_state()
        .spaces
        .into_iter()
        .find(|s| s.screen.as_ref().is_some_and(|sc| sc.key == STUDIO_KEY.text()))
        .expect("saved");
    let nine: Vec<WindowMeta> = [9, 10].iter().map(|id| other.m.window(WindowId(*id)).expect("open").meta.clone()).collect();
    let mut e = Env::both();
    let [code, music, terminal, chrome, finder] = measured_desk(&mut e);
    e.m.restore_spaces(vec![saved.clone()]);
    let first = e.space_of(STUDIO);
    let a = e.m.set_displays(vec![studio()]);
    e.run(a);
    assert_eq!(e.m.active_stage(STUDIO), Some(finder));
    // macOS shows the second Desktop: its windows, Finder and Chrome (at the 5K's corner,
    // where the adoption parked it); the first Desktop's windows are gone from the screen.
    let mut live = nine.clone();
    for id in [6u32, 5] {
        let w = e.m.window(WindowId(id)).expect("known");
        live.push(WindowMeta { frame: e.d.frames[&WindowId(id)], ..w.meta.clone() });
    }
    let absent: Vec<WindowId> = [1, 3, 4].map(WindowId).to_vec();
    let hints = LaunchHints { z_order: vec![WindowId(6), WindowId(9), WindowId(10), WindowId(5)], frontmost: Some(21) };
    let a = e.m.arrive(STUDIO, &live, &absent, &hints);
    e.run(a);
    let note = e.m.last_arrival();
    assert_eq!((note.rebuilt, note.carried, note.moved), (Some(saved.id), 2, 0), "the laptop's stages came whole: {note:?}");
    let second = e.space_of(STUDIO);
    assert_ne!(second, first);
    assert_eq!((e.stage_of(6), e.stage_of(5)), (finder, chrome), "the same stages");
    assert_eq!(e.m.active_stage(STUDIO), Some(finder), "the laptop's front in front");
    assert!(e.m.window(WindowId(6)).is_some_and(|w| !w.parked));
    assert_ne!(e.stage_of(5), e.stage_of(9), "Chrome's laptop window keeps its own stage, not its app's stage here");
    assert!(e.m.window(WindowId(5)).is_some_and(|w| w.parked) && e.parked_on(5, &studio()));
    let slots = e.m.strip_slots(STUDIO);
    let (own, mail) = (e.stage_of(9), e.stage_of(10));
    assert!(slots.contains(&own) && slots.contains(&mail), "the Desktop's own front went to a card: {slots:?}");
    assert!(slots.iter().position(|s| *s == chrome) > slots.iter().position(|s| *s == own), "after the Desktop's own cards: {slots:?}");
    assert!(e.m.window(WindowId(9)).is_some_and(|w| w.parked) && e.m.window(WindowId(10)).is_some_and(|w| w.parked));
    assert!(e.m.stages_in(second).all(|s| s.display == STUDIO));
    // The first Desktop keeps its stages.
    let mut kept: Vec<StageId> = e.m.stages_in(first).map(|s| s.id).collect();
    kept.sort();
    let mut want = vec![code, music, terminal];
    want.sort();
    assert_eq!(kept, want);
}

#[test]
fn a_screen_plugged_in_again_under_a_new_id_is_the_same_screen() {
    let mut e = Env::both();
    let [code, _, _, chrome, finder] = measured_desk(&mut e);
    let space = e.space_of(LAPTOP);
    let again = Display { id: DisplayId(9), ..laptop() };
    let a = e.m.set_displays(vec![studio(), again]);
    e.run(a);
    assert_eq!(e.m.current_space(DisplayId(9)), space, "the same Space");
    assert_eq!(e.m.active_stage(DisplayId(9)), Some(finder));
    assert_eq!(e.m.strip_slots(DisplayId(9)), vec![chrome]);
    assert!(e.m.stages().iter().filter(|s| s.space == space).all(|s| s.display == DisplayId(9)));
    assert_eq!(e.m.window(WindowId(5)).map(|w| w.meta.display), Some(DisplayId(9)));
    assert_eq!(e.m.active_stage(STUDIO), Some(code));
}

#[test]
fn main_only_keeps_the_main_screen_alone_and_lets_a_moved_window_go() {
    let options = ModelOptions { main_only: true, ..ModelOptions::default() };
    let m = Model::new(vec![studio(), laptop()], options);
    assert_eq!(m.displays().len(), 1);
    let mut e = Env { m, d: Desk::default() };
    e.open(1, 10, &studio(), (200.0, 100.0), RuleAction::Current);
    e.open(2, 11, &studio(), (600.0, 300.0), RuleAction::New);
    let a = e.m.set_displays(vec![studio(), laptop()]);
    e.run(a);
    assert_eq!(e.m.displays().len(), 1, "a second display is never taken");
    // Today's behaviour: the window leaves its stage and is left alone there.
    let (a, forgotten) = e.moved(&[(2, &laptop(), (100.0, 100.0))]);
    assert_eq!(forgotten, vec![WindowId(2)]);
    assert!(e.m.window(WindowId(2)).is_none());
    assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { .. } | Action::Park { .. })), "nothing moved: {a:?}");
    assert!(e.m.stages().iter().all(|s| s.display == STUDIO));
    // Exactly today's behaviour: its emptied stage stays the front, empty.
    assert_ne!(e.m.active_stage(STUDIO), Some(e.stage_of(1)));
    assert!(e.parked_on(1, &studio()));
}

#[test]
fn per_screen_capacity_keeps_each_strip_its_own_size() {
    let mut e = Env::both();
    e.m.set_display_capacity(LAPTOP, 2);
    for (i, pid) in (1..=4).zip(20..) {
        e.open(i, pid, &laptop(), (50.0 * f64::from(i), 50.0), RuleAction::New);
    }
    for (i, pid) in (11..=14).zip(30..) {
        e.open(i, pid, &studio(), (50.0 * f64::from(i), 50.0), RuleAction::New);
    }
    assert_eq!(e.m.strip_slots(LAPTOP).len(), 2);
    assert_eq!(e.m.strip_slots(STUDIO).len(), 3, "the default six holds them all");
    assert_eq!(e.m.capacity_of(e.space_of(LAPTOP)), 2);
    assert_eq!(e.m.capacity_of(e.space_of(STUDIO)), 6);
    // The whole-model setting forgets the per-display numbers.
    e.m.set_strip_capacity(1);
    assert_eq!(e.m.strip_slots(LAPTOP).len(), 1);
    assert_eq!(e.m.strip_slots(STUDIO).len(), 1);
}

/// The measured desk saved, then a new model (a restart) with the screens under new ids.
fn saved_desk() -> (crate::engine::persist::StateFile, Vec<WindowMeta>) {
    let mut e = Env::both();
    measured_desk(&mut e);
    let state = e.m.to_state();
    let live: Vec<WindowMeta> = e.m.windows().map(|w| w.meta.clone()).collect();
    (state, live)
}

#[test]
fn state_json_names_each_spaces_screen() {
    let (state, _) = saved_desk();
    assert_eq!(state.display.as_ref().and_then(|d| d.key.clone()), Some(STUDIO_KEY.text()));
    let keys: Vec<Option<String>> = state.spaces.iter().map(|s| s.screen.as_ref().map(|sc| sc.key.clone())).collect();
    assert!(keys.contains(&Some(STUDIO_KEY.text())) && keys.contains(&Some(LAPTOP_KEY.text())), "{keys:?}");
    let laptop_space = state.spaces.iter().find(|s| s.screen.as_ref().is_some_and(|sc| sc.key == LAPTOP_KEY.text())).expect("saved");
    assert_eq!((laptop_space.w(), laptop_space.h()), (1800.0, 1169.0));
    assert!(laptop_space.current, "shown on its screen when saved");
}

trait ScreenSize {
    fn w(&self) -> f64;
    fn h(&self) -> f64;
}

impl ScreenSize for crate::engine::persist::SavedSpace {
    fn w(&self) -> f64 {
        self.screen.as_ref().map_or(0.0, |s| s.w)
    }
    fn h(&self) -> f64 {
        self.screen.as_ref().map_or(0.0, |s| s.h)
    }
}

#[test]
fn a_restart_restores_each_screens_stages_on_that_screen_whatever_its_id() {
    let (state, live) = saved_desk();
    let (studio_now, laptop_now) = (Display { id: DisplayId(7), ..studio() }, Display { id: DisplayId(9), ..laptop() });
    let mut m = Model::new(vec![studio_now, laptop_now], ModelOptions::default());
    m.restore_spaces(state.spaces.clone());
    let with_display = |d: DisplayId| -> Vec<WindowMeta> {
        live.iter().filter(|w| w.display == if d == DisplayId(7) { STUDIO } else { LAPTOP }).cloned().map(|mut w| {
            w.display = d;
            w
        }).collect()
    };
    let hints = LaunchHints::default();
    m.arrive(DisplayId(9), &with_display(DisplayId(9)), &[], &hints);
    let laptop_saved = state.spaces.iter().find(|s| s.screen.as_ref().is_some_and(|sc| sc.key == LAPTOP_KEY.text())).expect("saved").id;
    assert_eq!(m.last_arrival().rebuilt, Some(laptop_saved), "the laptop's saved Desktop forms on the laptop");
    assert_eq!(m.current_space(DisplayId(9)).0, laptop_saved);
    assert!(m.stages_in(SpaceId(laptop_saved)).count() == 2);
    assert!(m.stages_in(SpaceId(laptop_saved)).all(|s| s.display == DisplayId(9)));
    m.arrive(DisplayId(7), &with_display(DisplayId(7)), &[], &hints);
    let studio_saved = state.spaces.iter().find(|s| s.screen.as_ref().is_some_and(|sc| sc.key == STUDIO_KEY.text())).expect("saved").id;
    assert_eq!(m.last_arrival().rebuilt, Some(studio_saved));
    assert_eq!(m.stages_in(SpaceId(studio_saved)).count(), 3);
    assert!(m.unvisited_saved_spaces() == 0);
}

#[test]
fn a_saved_desktop_of_a_connected_screen_never_forms_on_another() {
    // The laptop's windows shown on the Studio while the laptop is connected: not the
    // laptop's Desktop (it waits for its own screen); a new Desktop on the Studio.
    let (state, live) = saved_desk();
    let mut m = Model::new(vec![studio(), laptop()], ModelOptions::default());
    m.restore_spaces(state.spaces.clone());
    let chrome: Vec<WindowMeta> = live.iter().filter(|w| w.pid == 20 || w.pid == 21).cloned().map(|mut w| {
        w.display = STUDIO;
        w
    }).collect();
    m.arrive(STUDIO, &chrome, &[], &LaunchHints::default());
    assert_eq!(m.last_arrival().rebuilt, None);
    assert_eq!(m.unvisited_saved_spaces(), 2, "both saved Desktops still wait");
}

#[test]
fn a_saved_desktop_of_a_screen_not_connected_forms_where_its_windows_are() {
    // 10-23: a login on the laptop's screen alone; the Studio's saved stages form there.
    let (state, live) = saved_desk();
    let mut m = Model::new(vec![laptop()], ModelOptions::default());
    m.restore_spaces(state.spaces.clone());
    let studio_apps: Vec<WindowMeta> = live.iter().filter(|w| w.display == STUDIO).cloned().map(|mut w| {
        w.display = LAPTOP;
        w
    }).collect();
    m.arrive(LAPTOP, &studio_apps, &[], &LaunchHints::default());
    let studio_saved = state.spaces.iter().find(|s| s.screen.as_ref().is_some_and(|sc| sc.key == STUDIO_KEY.text())).expect("saved").id;
    assert_eq!(m.last_arrival().rebuilt, Some(studio_saved));
    assert!(m.stages_in(SpaceId(studio_saved)).all(|s| s.display == LAPTOP));
    // The restore says so (round displays-2): the arrival's note and the summary's line.
    assert!(m.last_arrival().other_screen, "formed on another screen than it was saved on");
    assert_eq!(m.restore_summary(0).other_screen, 1);
    assert!(m.restore_summary(0).to_string().ends_with("1 Desktop on another screen"));
}

#[test]
fn a_saved_desktop_on_its_own_screen_is_not_counted_as_moved() {
    let (state, live) = saved_desk();
    let mut m = Model::new(vec![studio(), laptop()], ModelOptions::default());
    m.restore_spaces(state.spaces.clone());
    let laptop_apps: Vec<WindowMeta> = live.iter().filter(|w| w.display == LAPTOP).cloned().collect();
    m.arrive(LAPTOP, &laptop_apps, &[], &LaunchHints::default());
    assert!(m.last_arrival().rebuilt.is_some());
    assert!(!m.last_arrival().other_screen);
    assert_eq!(m.restore_summary(0).other_screen, 0);
    assert!(!m.restore_summary(0).to_string().contains("another screen"));
}

#[test]
fn fit_keeps_a_window_clear_of_its_own_screens_strip() {
    use crate::engine::stage_area::FitOptions;
    let mut e = Env::both();
    // The Studio's strip column is wider than the laptop's (its cards are bigger).
    e.m.set_display_fit(STUDIO, FitOptions { inset: 216.0, shrink: false });
    e.m.set_display_fit(LAPTOP, FitOptions { inset: 160.0, shrink: false });
    e.open(1, 20, &studio(), (40.0, 100.0), RuleAction::New);
    e.open(2, 21, &laptop(), (40.0, 100.0), RuleAction::New);
    assert_eq!(e.d.frames[&WindowId(1)].x, studio().visible.x + 216.0, "moved clear of the Studio's strip");
    assert_eq!(e.d.frames[&WindowId(2)].x, laptop().visible.x + 160.0, "moved clear of the laptop's own strip, not the Studio's");
    assert_eq!(e.m.fit_of(LAPTOP).inset, 160.0);
    // The whole-model setting forgets the per-display ones.
    e.m.set_fit(FitOptions { inset: 0.0, shrink: false });
    assert_eq!(e.m.fit_of(LAPTOP).inset, 0.0);
    assert_eq!(e.m.fit_of(STUDIO).inset, 0.0);
}

#[test]
fn saved_frames_apply_only_on_the_screen_of_the_saved_size() {
    let (state, _) = saved_desk();
    let mut m = Model::new(vec![studio(), laptop()], ModelOptions::default());
    m.restore_spaces(state.spaces.clone());
    let laptop_saved = state.spaces.iter().find(|s| s.screen.as_ref().is_some_and(|sc| sc.key == LAPTOP_KEY.text())).expect("saved").id;
    let studio_saved = state.spaces.iter().find(|s| s.screen.as_ref().is_some_and(|sc| sc.key == STUDIO_KEY.text())).expect("saved").id;
    assert!(m.saved_frames_fit_for(LAPTOP, Some(laptop_saved)));
    assert!(m.saved_frames_fit_for(STUDIO, Some(studio_saved)));
    assert!(!m.saved_frames_fit_for(STUDIO, Some(laptop_saved)), "the laptop's frames never on the Studio");
    assert!(!m.saved_frames_fit_for(LAPTOP, Some(studio_saved)));
    // The same screen at another size: no.
    let smaller = Display { frame: Rect::new(-1440.0, 271.0, 1440.0, 900.0), visible: Rect::new(-1440.0, 310.0, 1440.0, 861.0), ..laptop() };
    let mut m = Model::new(vec![studio(), smaller], ModelOptions::default());
    m.restore_spaces(state.spaces);
    assert!(!m.saved_frames_fit_for(LAPTOP, Some(laptop_saved)));
}
