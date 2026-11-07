//! A window its app reopens much smaller than saved gets its saved frame back, once (round
//! restore-saved-size). Made-up titles only.

use super::*;
use crate::engine::model::{Action, ModelOptions, Observation, Pid, StageId};
use crate::engine::persist::{LaunchHints, SavedSpace, SavedStage, SavedWindow, StateFile};
use crate::engine::restore::Restore;
use crate::engine::testkit::*;

const EDITOR: &str = "com.example.editor";
const CHAT: &str = "com.example.chat";

/// The editor's window as saved: most of the test display (1440x900, visible 1440x800).
const SAVED_PLAN: Rect = Rect::new(150.0, 60.0, 1170.0, 705.0);
/// Chat's window as saved.
const SAVED_TEAM: Rect = Rect::new(200.0, 90.0, 1000.0, 650.0);
/// Where the editor reopened its window after the restart (his 11-06 case, scaled: 665x401
/// for a saved 2082x1253, about a tenth of the area).
const SMALL_PLAN: Rect = Rect::new(19.0, 413.0, 374.0, 225.0);
/// Where Chat reopened its window.
const SMALL_TEAM: Rect = Rect::new(40.0, 300.0, 400.0, 260.0);

fn win(id: u32, pid: Pid, bundle: &str, title: &str, frame: Rect) -> WindowMeta {
    WindowMeta { title: title.into(), frame, ..meta(id, pid, bundle) }
}

fn sw(bundle: &str, title: &str, frame: Rect) -> SavedWindow {
    SavedWindow { bundle_id: bundle.into(), title: title.into(), frame, key: true, ..Default::default() }
}

/// Desktop 5 as saved in session "A": the editor in front, Chat a card.
fn desktop_5() -> SavedSpace {
    SavedSpace {
        screen: None,
        id: 5,
        current: true,
        stages: vec![
            SavedStage { windows: vec![sw(EDITOR, "Plan.txt", SAVED_PLAN)], ..Default::default() },
            SavedStage { windows: vec![sw(CHAT, "Team", SAVED_TEAM)], ..Default::default() },
        ],
        active_index: Some(0),
        strip_slots: vec![1],
        last_seen: None,
        unvisited: false,
    }
}

/// A launch in session "B" (`restore`) of a file written in session "A".
fn restoring_with(options: ModelOptions, restore: Restore) -> Model {
    let mut file = StateFile { session: Some("A".into()), ..StateFile::single(SavedSpace::default()) };
    file.spaces = vec![desktop_5()];
    file.start_new_session();
    let (file, _) = file.for_session(Some("B"));
    let mut m = model_with(options);
    m.set_restore(restore);
    m.restore_spaces(file.spaces);
    m
}

fn after_restart() -> Model {
    restoring_with(ModelOptions::default(), Restore::after_restart("B"))
}

/// The launch's arrival with the editor's and Chat's windows where their apps reopened them.
fn launch(m: &mut Model, plan: WindowMeta, team: WindowMeta) -> Vec<Action> {
    m.arrive(MAIN, &[plan, team], &[], &LaunchHints::default())
}

fn set_frame(actions: &[Action], id: u32) -> Option<Rect> {
    actions.iter().find_map(|a| match a {
        Action::SetFrame { id: x, frame } if x.0 == id => Some(*frame),
        _ => None,
    })
}

fn restore_of(m: &Model, id: u32) -> Option<Rect> {
    m.window(w(id)).map(|x| x.restore)
}

fn saved_frame(m: &Model, title: &str) -> Option<Rect> {
    m.to_state().spaces.iter().flat_map(|s| &s.stages).flat_map(|s| &s.windows).find(|w| w.title == title).map(|w| w.frame)
}

fn stage(m: &Model, id: u32) -> StageId {
    m.stage_of(w(id)).expect("in a stage")
}

#[test]
fn the_rule_is_under_half_the_area_and_the_saved_size_fitting_the_screen() {
    assert!(owes(SMALL_PLAN, SAVED_PLAN, VISIBLE), "his case");
    let half = Rect::new(0.0, 25.0, SAVED_PLAN.w / 2.0, SAVED_PLAN.h);
    assert!(!owes(half, SAVED_PLAN, VISIBLE), "half the area is not under half");
    assert!(!owes(Rect::new(0.0, 25.0, 1300.0, 760.0), SAVED_PLAN, VISIBLE), "larger than saved");
    assert!(!owes(SMALL_PLAN, Rect::new(0.0, 25.0, 1500.0, 700.0), VISIBLE), "saved wider than the screen");
    assert!(!owes(Rect::new(0.0, 25.0, 0.0, 0.0), SAVED_PLAN, VISIBLE), "no frame of its own");
}

#[test]
fn a_window_reopened_much_smaller_gets_its_saved_frame_when_first_shown() {
    // His 11-06 restart: strong matches (exact titles) at a tenth of their saved size.
    let mut m = after_restart();
    let a = launch(&mut m, win(21, 50, EDITOR, "Plan.txt", SMALL_PLAN), win(22, 30, CHAT, "Team", SMALL_TEAM));
    assert_eq!(m.current_space(MAIN).0, 5, "the saved Desktop recognized");
    assert_eq!(m.restored_grade(w(21)), Some(Grade::Strong));
    // In front at the restore: its saved frame now.
    assert_eq!(set_frame(&a, 21), Some(SAVED_PLAN), "{a:?}");
    assert_eq!(restore_of(&m, 21), Some(SAVED_PLAN));
    // A card: parked at its app's size, owed the saved one; state.json keeps the saved one.
    assert!(m.window(w(22)).is_some_and(|x| x.parked));
    assert_eq!(restore_of(&m, 22), Some(SMALL_TEAM));
    assert_eq!(saved_frame(&m, "Team"), Some(SAVED_TEAM), "a second restart still knows it");
    // Its card's first click.
    let chat = stage(&m, 22);
    let a = m.switch_to(chat, &Observation::default());
    assert_eq!(set_frame(&a, 22), Some(SAVED_TEAM), "{a:?}");
    // Once: the user's frame from then on.
    let mine = Rect::new(300.0, 120.0, 800.0, 500.0);
    m.window_frame_changed(w(22), mine);
    let editor = stage(&m, 21);
    m.switch_to(editor, &Observation::default());
    let a = m.switch_to(chat, &Observation::default());
    assert_eq!(set_frame(&a, 22), Some(mine), "{a:?}");
}

#[test]
fn a_medium_match_keeps_its_apps_frame() {
    // Chat's title gained a suffix: the same fingerprint, no exact title (medium).
    let mut m = after_restart();
    launch(&mut m, win(21, 50, EDITOR, "Plan.txt", SMALL_PLAN), win(22, 30, CHAT, "Team — 4 new", SMALL_TEAM));
    assert_eq!(m.restored_grade(w(22)), Some(Grade::Medium));
    let chat = stage(&m, 22);
    let a = m.switch_to(chat, &Observation::default());
    assert_eq!(set_frame(&a, 22), Some(SMALL_TEAM), "{a:?}");
}

#[test]
fn a_window_the_user_moves_before_it_shows_keeps_their_frame() {
    // Chat's parked window dragged out of the corner by the user before its card was clicked.
    let mut m = after_restart();
    launch(&mut m, win(21, 50, EDITOR, "Plan.txt", SMALL_PLAN), win(22, 30, CHAT, "Team", SMALL_TEAM));
    let mine = Rect::new(500.0, 200.0, 400.0, 260.0);
    m.adopt_shown(w(22), mine, &Observation::default());
    assert_eq!(restore_of(&m, 22), Some(mine));
    assert_eq!(saved_frame(&m, "Team"), Some(mine));
    assert!(!m.owes_saved_size(w(22)), "a drag seen by us ends it");
    // Shown again later: still the user's.
    let mut a = Vec::new();
    m.restore_window(w(22), &mut a);
    assert_eq!(set_frame(&a, 22), Some(mine), "{a:?}");
}

#[test]
fn a_relaunch_in_the_same_session_keeps_the_apps_frames() {
    let mut m = restoring_with(ModelOptions::default(), Restore::same_session("A"));
    let a = launch(&mut m, win(21, 50, EDITOR, "Plan.txt", SMALL_PLAN), win(22, 30, CHAT, "Team", SMALL_TEAM));
    assert_eq!(m.current_space(MAIN).0, 5);
    assert_eq!(set_frame(&a, 21), None, "{a:?}");
    assert_eq!(restore_of(&m, 21), Some(SMALL_PLAN));
    let chat = stage(&m, 22);
    let a = m.switch_to(chat, &Observation::default());
    assert_eq!(set_frame(&a, 22), Some(SMALL_TEAM), "{a:?}");
}

#[test]
fn a_window_reopened_larger_or_not_much_smaller_keeps_its_apps_frame() {
    let larger = Rect::new(100.0, 40.0, 1300.0, 760.0);
    let most = Rect::new(100.0, 40.0, 900.0, 600.0);
    let mut m = after_restart();
    let a = launch(&mut m, win(21, 50, EDITOR, "Plan.txt", larger), win(22, 30, CHAT, "Team", most));
    assert_eq!(set_frame(&a, 21), None, "{a:?}");
    assert_eq!(restore_of(&m, 21), Some(larger));
    let chat = stage(&m, 22);
    let a = m.switch_to(chat, &Observation::default());
    assert_eq!(set_frame(&a, 22), Some(most), "{a:?}");
}

#[test]
fn on_another_screen_size_windows_keep_their_apps_frames() {
    // Saved on a 2560x1440 main display; the test display is 1440x900.
    let restore = Restore::after_restart("B").with_display(Some((2560.0, 1440.0)));
    let mut m = restoring_with(ModelOptions::default(), restore);
    let a = launch(&mut m, win(21, 50, EDITOR, "Plan.txt", SMALL_PLAN), win(22, 30, CHAT, "Team", SMALL_TEAM));
    assert_eq!(set_frame(&a, 21), None, "{a:?}");
    let chat = stage(&m, 22);
    let a = m.switch_to(chat, &Observation::default());
    assert_eq!(set_frame(&a, 22), Some(SMALL_TEAM), "{a:?}");
}

#[test]
fn switched_off_every_window_keeps_its_apps_frame() {
    let options = ModelOptions { keep_app_frames: true, ..ModelOptions::default() };
    let mut m = restoring_with(options, Restore::after_restart("B"));
    let a = launch(&mut m, win(21, 50, EDITOR, "Plan.txt", SMALL_PLAN), win(22, 30, CHAT, "Team", SMALL_TEAM));
    assert_eq!(set_frame(&a, 21), None, "{a:?}");
    assert_eq!(saved_frame(&m, "Team"), Some(SMALL_TEAM));
    // Turned off after the launch: what was owed is not paid.
    let mut m = after_restart();
    launch(&mut m, win(21, 50, EDITOR, "Plan.txt", SMALL_PLAN), win(22, 30, CHAT, "Team", SMALL_TEAM));
    m.set_saved_size(false);
    let chat = stage(&m, 22);
    let a = m.switch_to(chat, &Observation::default());
    assert_eq!(set_frame(&a, 22), Some(SMALL_TEAM), "{a:?}");
}
