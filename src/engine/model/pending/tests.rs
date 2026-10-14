//! Pending stages, adoption, giving up and the strip order of a restore (the design 8D's
//! engine tests). Made-up titles only.

use super::*;
use crate::engine::geometry::{Rect, Size};
use crate::engine::model::{Model, Observation, Pid};
use crate::engine::park::park_frame;
use crate::engine::persist::{SavedSpace, SavedStage, StateFile};
use crate::engine::restore::Restore;
use crate::engine::rules::Decision;
use crate::engine::testkit::*;

const F_PLAN: Rect = Rect::new(100.0, 80.0, 600.0, 400.0);
const F_NOTES: Rect = Rect::new(160.0, 120.0, 640.0, 420.0);
const F_TEAM: Rect = Rect::new(400.0, 100.0, 700.0, 500.0);
const F_RECIPES: Rect = Rect::new(250.0, 150.0, 900.0, 600.0);
/// Where an app opens a window when it does not restore its frame.
const DEFAULT: Rect = Rect::new(300.0, 300.0, 500.0, 350.0);

const EDITOR: &str = "com.example.editor";
const CHAT: &str = "com.example.chat";
const BROWSER: &str = "com.example.browser";

fn win(id: u32, pid: Pid, bundle: &str, title: &str, frame: Rect) -> WindowMeta {
    WindowMeta { title: title.into(), frame, ..meta(id, pid, bundle) }
}

fn sw(bundle: &str, title: &str, frame: Rect, z: usize) -> SavedWindow {
    SavedWindow { bundle_id: bundle.into(), title: title.into(), frame, z, key: z == 0, ..Default::default() }
}

fn stage(name: Option<&str>, windows: Vec<SavedWindow>) -> SavedStage {
    SavedStage { name: name.map(str::to_string), windows, ..Default::default() }
}

/// Desktop 5 as saved in session "A": the editor's two windows (active), Chat (named), the
/// browser; the strip showed the browser above Chat.
fn desktop_5() -> SavedSpace {
    SavedSpace {
        id: 5,
        current: true,
        stages: vec![
            stage(None, vec![sw(EDITOR, "Plan.txt", F_PLAN, 0), sw(EDITOR, "Notes.txt", F_NOTES, 1)]),
            stage(Some("Chat"), vec![sw(CHAT, "Team", F_TEAM, 0)]),
            stage(None, vec![sw(BROWSER, "Recipes — Example Browser", F_RECIPES, 0)]),
        ],
        active_index: Some(0),
        strip_slots: vec![2, 1],
        last_seen: None,
    }
}

/// A launch in session "B" of a file written in session "A": restore mode.
fn restoring(spaces: Vec<SavedSpace>) -> Model {
    let mut file = StateFile { session: Some("A".into()), ..StateFile::single(SavedSpace::default()) };
    file.spaces = spaces;
    file.start_new_session();
    let (file, _) = file.for_session(Some("B"));
    let mut m = model();
    m.set_restore(Restore::after_restart("B"));
    m.restore_spaces(file.spaces);
    m
}

fn add(m: &mut Model, w: WindowMeta) -> Vec<Action> {
    m.add_window(w, &Decision::default(), &Observation::default())
}

fn parks(actions: &[Action], id: u32) -> bool {
    actions.iter().any(|a| matches!(a, Action::Park { id: x, .. } if x.0 == id))
}

fn set_frame(actions: &[Action], id: u32) -> Option<Rect> {
    actions.iter().find_map(|a| match a {
        Action::SetFrame { id: x, frame } if x.0 == id => Some(*frame),
        _ => None,
    })
}

fn switches(actions: &[Action]) -> bool {
    actions.iter().any(|a| matches!(a, Action::Focus { .. }))
}

fn names(m: &Model) -> Vec<Option<String>> {
    m.stages_in(m.current_space(MAIN)).map(|s| s.name.clone()).collect()
}

fn number_of(m: &Model, id: u32) -> Option<usize> {
    m.stage_of(w(id)).and_then(|s| m.stage_number(s))
}

#[test]
fn a_launch_on_an_empty_screen_keeps_every_saved_stage_waiting() {
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    let space = m.current_space(MAIN);
    assert_eq!(m.provisional_spaces(), [space], "made from an empty screen: provisional");
    assert_ne!(space.0, 5, "not taken for the saved Desktop without evidence");
    assert!(m.strip().is_empty(), "no card for anything pending");
    // Saved again: nothing dropped, every window flagged pending since session A.
    let file = m.to_state();
    let saved = file.spaces.iter().find(|s| s.id == 5).expect("the saved Desktop is kept");
    assert_eq!(saved.standard_windows(), 4);
    assert!(saved.stages.iter().flat_map(|s| &s.windows).all(|w| w.pending.as_ref().is_some_and(|p| p.since == "A")));
    assert_eq!(m.pending_unvisited(), 4);
}

#[test]
fn windows_reopened_after_a_restart_go_back_to_their_stages_quietly() {
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());

    // Chat reopens at its saved frame: strong; the Desktop becomes the saved Desktop 5.
    let a = add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert_eq!(m.current_space(MAIN).0, 5, "adopted: the saved Desktop's id");
    assert!(m.provisional_spaces().is_empty());
    assert_eq!(m.unvisited_saved_spaces(), 0);
    assert_eq!(names(&m), [None, Some("Chat".into()), None], "the saved stages, in order, with their names");
    assert_eq!(m.main_active_stage().and_then(|s| m.stage_number(s)), Some(1), "the saved active stage");
    assert_eq!(number_of(&m, 21), Some(2));
    assert!(parks(&a, 21) && !switches(&a), "parked into its inactive stage, no switch: {a:?}");
    assert_eq!(m.restored_grade(w(21)), Some(Grade::Strong));
    assert_eq!(m.window(w(21)).map(|x| x.restore), Some(F_TEAM));

    // The browser reopens with another title suffix and frame: medium by the fingerprint.
    let a = add(&mut m, win(22, 40, BROWSER, "Recipes", DEFAULT));
    assert_eq!(m.restored_grade(w(22)), Some(Grade::Medium));
    assert_eq!(number_of(&m, 22), Some(3));
    assert!(parks(&a, 22) && !switches(&a));
    assert_eq!(m.window(w(22)).map(|x| x.restore), Some(F_RECIPES), "medium: the saved frame");
    let strip: Vec<usize> = m.strip().iter().map(|c| c.number).collect();
    assert_eq!(strip, [3, 2], "the saved strip order, whatever order the cards came in");

    // The editor's document of the active stage: shown, moved to its saved frame.
    let a = add(&mut m, win(23, 50, EDITOR, "Plan.txt", DEFAULT));
    assert_eq!(number_of(&m, 23), Some(1));
    assert_eq!(set_frame(&a, 23), Some(F_PLAN));
    assert!(!parks(&a, 23) && !switches(&a));

    // A new untitled editor window: the app alone (low), into the active stage it is shown
    // in, at its own frame; Notes.txt is still awaited (a low match is no proof).
    let a = add(&mut m, win(24, 50, EDITOR, "Untitled", DEFAULT));
    assert_eq!(m.restored_grade(w(24)), Some(Grade::Low));
    assert_eq!(number_of(&m, 24), Some(1));
    assert_eq!(set_frame(&a, 24), None, "low: the live frame stays");
    assert!(!parks(&a, 24));
    let active = m.main_active_stage().expect("active");
    assert_eq!(m.stage(active).map(|s| s.windows.clone()), Some(vec![w(23), w(24)]), "in saved order");
    assert_eq!(m.stage(active).and_then(|s| s.key), Some(w(23)), "the saved key window");
    assert_eq!(m.pending_count(), 1);
    // Notes.txt itself comes: medium by its frame, into the same stage.
    add(&mut m, win(25, 50, EDITOR, "Notes.txt — Edited", DEFAULT));
    assert_eq!(number_of(&m, 25), Some(1));
    assert_eq!(m.pending_count(), 0);

    let file = m.to_state();
    assert_eq!(file.spaces.len(), 1);
    assert_eq!((file.spaces[0].id, file.spaces[0].current), (5, true));
    assert!(file.spaces[0].stages.iter().flat_map(|s| &s.windows).all(|w| w.pending.is_none()));
    assert_eq!(file.spaces[0].strip_slots, [2, 1]);
}

#[test]
fn a_doubtful_window_is_never_parked() {
    // Two saved "Untitled" windows in two inactive stages, the active stage waiting for its
    // own: a new "Untitled" at another frame could be either, so Apple's rule decides.
    let space = SavedSpace {
        id: 5,
        current: true,
        stages: vec![
            stage(None, vec![sw(EDITOR, "Plan.txt", F_PLAN, 0)]),
            stage(None, vec![sw(CHAT, "Untitled", F_TEAM, 0)]),
            stage(None, vec![sw(CHAT, "Untitled", F_RECIPES, 0)]),
        ],
        active_index: Some(0),
        ..Default::default()
    };
    let mut m = restoring(vec![space]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_eq!(m.current_space(MAIN).0, 5);
    let a = add(&mut m, win(31, 30, CHAT, "Untitled", DEFAULT));
    assert!(!parks(&a, 31), "{a:?}");
    assert_eq!(m.restored_grade(w(31)), None);
    assert_eq!(number_of(&m, 31), Some(4), "a stage of its own, by Apple's rule");
    assert!(m.window(w(31)).is_some_and(|x| !x.parked));
    assert_eq!(m.pending_count(), 2, "both saved windows still wait");
    // At one of the saved frames the frame tells them apart: strong, back in its stage.
    let a = add(&mut m, win(32, 30, CHAT, "Untitled", F_RECIPES));
    assert_eq!(number_of(&m, 32), Some(3));
    assert!(parks(&a, 32));
}

#[test]
fn a_window_of_another_saved_desktop_goes_by_apples_rule() {
    let mut d6 = desktop_5();
    d6.id = 6;
    d6.current = false;
    d6.stages = vec![stage(None, vec![sw(BROWSER, "Holidays", F_RECIPES, 0)]), stage(None, vec![sw(CHAT, "Family", F_TEAM, 0)])];
    d6.strip_slots = vec![1];
    let mut m = restoring(vec![desktop_5(), d6]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert_eq!(m.current_space(MAIN).0, 5);
    // Desktop 6's window shows here: macOS put it here; no guess.
    let a = add(&mut m, win(41, 30, CHAT, "Family", Rect::new(500.0, 200.0, 650.0, 450.0)));
    assert_eq!(m.restored_grade(w(41)), None);
    assert!(!parks(&a, 41) || number_of(&m, 41) == number_of(&m, 21), "joins the app's stage at most: {a:?}");
    assert_eq!(m.unvisited_saved_spaces(), 1, "Desktop 6 still waits for its visit");
    assert_eq!(m.to_state().spaces.iter().find(|s| s.id == 6).map(|s| s.standard_windows()), Some(2));
}

#[test]
fn adoption_keeps_windows_already_shown_in_front() {
    // A doubtful window came first (Apple's rule, shown); the Desktop is adopted afterwards
    // without hiding it: its stage stays active.
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    let a = add(&mut m, win(51, 60, "com.example.music", "Player", DEFAULT));
    assert!(!parks(&a, 51));
    assert_ne!(m.current_space(MAIN).0, 5, "nothing to adopt yet");
    let a = add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert_eq!(m.current_space(MAIN).0, 5);
    assert!(!a.iter().any(|x| matches!(x, Action::Park { id, .. } if id.0 == 51)), "{a:?}");
    assert_eq!(m.main_active_stage(), m.stage_of(w(51)));
    assert!(m.window(w(51)).is_some_and(|x| !x.parked));
}

#[test]
fn a_restored_window_in_the_park_zone_gets_its_saved_frame() {
    // The app saved its state while we had it parked at logout.
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    let corner = park_frame(Size::new(600.0, 400.0), FRAME);
    let a = add(&mut m, win(23, 50, EDITOR, "Plan.txt", corner));
    assert_eq!(number_of(&m, 23), Some(1));
    assert_eq!(set_frame(&a, 23), Some(F_PLAN), "brought out of the corner to its saved frame");
    let a = add(&mut m, win(21, 30, CHAT, "Team", park_frame(Size::new(700.0, 500.0), FRAME)));
    assert!(parks(&a, 21));
    assert_eq!(m.window(w(21)).map(|x| x.restore), Some(F_TEAM), "never a park frame to come back to");
}

#[test]
fn late_windows_at_launch_join_their_saved_stages() {
    // The review of 1.1.0, test 3 (H4), in the same window server session: one window listed
    // at launch, the others found afterwards by id or title.
    let mut saved = desktop_5();
    saved.stages[0].windows[0].window = Some(11);
    saved.stages[1].windows[0].window = Some(12);
    let mut m = model();
    m.set_restore(Restore::same_session("A"));
    m.restore_spaces(vec![saved]);
    m.arrive(MAIN, &[win(11, 50, EDITOR, "Plan.txt", F_PLAN)], &[], &LaunchHints::default());
    assert_eq!(m.current_space(MAIN).0, 5);
    assert!(m.provisional_spaces().is_empty(), "no restore mode in the same session");
    assert_eq!(m.pending_count(), 3, "nothing dropped (H4)");
    assert_eq!(names(&m), [None, Some("Chat".into()), None]);
    // By id, whatever its title now.
    let a = add(&mut m, win(12, 30, CHAT, "Team — 4 new", DEFAULT));
    assert_eq!(number_of(&m, 12), Some(2));
    assert!(parks(&a, 12));
    // By title.
    let a = add(&mut m, win(13, 40, BROWSER, "Recipes — Example Browser", F_RECIPES));
    assert_eq!(number_of(&m, 13), Some(3));
    assert!(parks(&a, 13));
    let a = add(&mut m, win(14, 50, EDITOR, "Notes.txt", F_NOTES));
    assert_eq!(number_of(&m, 14), Some(1));
    assert!(!parks(&a, 14) && set_frame(&a, 14).is_none(), "shown where it already is");
    assert_eq!(m.strip().iter().map(|c| c.number).collect::<Vec<_>>(), [3, 2]);
}

#[test]
fn a_same_session_launch_with_every_window_on_screen_is_unchanged() {
    let mut saved = desktop_5();
    for (i, st) in saved.stages.iter_mut().enumerate() {
        for (k, w) in st.windows.iter_mut().enumerate() {
            w.window = Some(10 + 2 * i as u32 + k as u32);
        }
    }
    let mut m = model();
    m.restore_spaces(vec![saved]);
    let live = [
        win(10, 50, EDITOR, "Plan.txt", F_PLAN),
        win(11, 50, EDITOR, "Notes.txt", F_NOTES),
        win(12, 30, CHAT, "Team", F_TEAM),
        win(14, 40, BROWSER, "Recipes — Example Browser", F_RECIPES),
    ];
    m.arrive(MAIN, &live, &[], &LaunchHints::default());
    assert_eq!(m.pending_count(), 0);
    assert_eq!(m.strip().iter().map(|c| c.number).collect::<Vec<_>>(), [3, 2]);
    assert!([10, 11, 12, 14].iter().all(|&id| m.restored_grade(w(id)) == Some(Grade::Strong)));
    assert!(m.to_state().spaces[0].stages.iter().flat_map(|s| &s.windows).all(|w| w.pending.is_none()));
}

#[test]
fn a_quiet_apps_pending_windows_are_given_up() {
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_eq!(m.pending_bundles(), [BROWSER, CHAT, EDITOR]);
    let (_, dropped) = m.expire_pending(&[CHAT.to_string(), EDITOR.to_string()]);
    assert_eq!(dropped, 2, "Team and Notes.txt");
    assert_eq!(names(&m), [None, None], "the Chat stage, empty now, is gone");
    assert_eq!(m.pending_bundles(), [BROWSER], "an app that never launched keeps its windows");
    let file = m.to_state();
    assert_eq!(file.spaces[0].stages.iter().map(|s| s.windows.len()).collect::<Vec<_>>(), [1, 1]);
}

#[test]
fn a_window_pending_through_a_whole_session_is_given_up_at_the_next() {
    let mut space = desktop_5();
    space.stages[0].windows[0].pending = Some(Pending { since: "A".into() }); // pending all through B
    space.stages[1].windows[0].pending = Some(Pending { since: "B".into() }); // went missing in B
    let mut file = StateFile { session: Some("B".into()), ..StateFile::single(space) };
    let mut gone = SavedSpace { id: 7, ..desktop_5() };
    gone.stages = vec![stage(None, vec![SavedWindow { pending: Some(Pending { since: "A".into() }), ..sw(CHAT, "Old", F_TEAM, 0) }])];
    file.spaces.push(gone);
    assert_eq!(file.start_new_session(), 2);
    let ws: Vec<(&str, &str)> = file.spaces[0]
        .stages
        .iter()
        .flat_map(|s| &s.windows)
        .map(|w| (w.title.as_str(), w.pending.as_ref().map_or("", |p| p.since.as_str())))
        .collect();
    assert_eq!(ws, [("Notes.txt", "B"), ("Team", "B"), ("Recipes — Example Browser", "B")]);
    assert_eq!(file.spaces.len(), 1, "a saved Desktop left with no window is gone");
    assert_eq!(file.spaces[0].stages.len(), 3, "stage indices kept");
}

#[test]
fn a_desktop_left_waiting_is_saved_and_adopted_again_on_return() {
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_eq!(m.current_space(MAIN).0, 5);
    m.remove_window(w(23));
    // The user goes to another Desktop: Desktop 5 holds only pending stages now.
    m.arrive(MAIN, &[win(61, 70, "com.example.music", "Player", DEFAULT)], &[], &LaunchHints::default());
    assert_ne!(m.current_space(MAIN).0, 5);
    assert_eq!(m.unvisited_saved_spaces(), 1, "back with the saved Desktops waiting");
    assert_eq!(m.pending_unvisited(), 3, "Plan.txt was closed; the three others wait");
    // Back on an empty Desktop: provisional again. Desktop 5 is no longer the one shown when
    // saved, so one window of it is not enough (M2): the second adopts it, and the first
    // goes back to its saved stage too.
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    assert!(m.provisional_spaces().contains(&m.current_space(MAIN)));
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert_ne!(m.current_space(MAIN).0, 5, "one window of a Desktop not shown when saved");
    assert_eq!(m.restored_grade(w(21)), None, "Apple's rule meanwhile");
    add(&mut m, win(22, 40, BROWSER, "Recipes — Example Browser", F_RECIPES));
    assert_eq!(m.current_space(MAIN).0, 5);
    assert_eq!(number_of(&m, 22), Some(3));
    assert_eq!(number_of(&m, 21), Some(2), "the first window back in its saved stage");
    assert_eq!(m.restored_grade(w(21)), Some(Grade::Strong));
    assert_eq!(m.stages_in(m.current_space(MAIN)).count(), 3, "no stage left over from Apple's rule");
    assert!(m.window(w(21)).is_some_and(|x| x.parked) && m.window(w(22)).is_some_and(|x| x.parked), "both stages inactive");
}

#[test]
fn an_arrival_on_a_desktop_with_pending_stages_matches_its_new_windows() {
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    let plan = win(23, 50, EDITOR, "Plan.txt", F_PLAN);
    add(&mut m, plan.clone());
    let player = win(61, 70, "com.example.music", "Player", DEFAULT);
    m.arrive(MAIN, std::slice::from_ref(&player), &[w(23)], &LaunchHints::default());
    assert_ne!(m.current_space(MAIN).0, 5);
    // Chat reopened its window on Desktop 1 meanwhile: listed at the arrival back there.
    let a = m.arrive(MAIN, &[plan, win(21, 30, CHAT, "Team", F_TEAM)], &[w(61)], &LaunchHints::default());
    assert_eq!(m.current_space(MAIN).0, 5);
    assert_eq!(number_of(&m, 21), Some(2));
    assert!(parks(&a, 21));
    assert_eq!(m.restored_grade(w(21)), Some(Grade::Strong));
}

#[test]
fn saved_recency_survives_a_restart() {
    let mut space = desktop_5();
    space.stages[0].last_active = Some(900);
    space.stages[1].last_active = Some(300);
    space.stages[2].last_active = Some(700);
    let mut m = model();
    m.restore_spaces(vec![space]);
    let live = [win(10, 50, EDITOR, "Plan.txt", F_PLAN), win(12, 30, CHAT, "Team", F_TEAM), win(14, 40, BROWSER, "Recipes — Example Browser", F_RECIPES)];
    m.arrive(MAIN, &live, &[], &LaunchHints::default());
    let recency: Vec<u64> = m.stages_in(m.current_space(MAIN)).map(|s| s.last_active).collect();
    assert!(recency[2] > recency[1], "the browser's stage was used after Chat's: {recency:?}");
    assert!(recency[0] > recency[2], "the active one most recently");
    let saved: Vec<Option<u64>> = m.to_state().spaces[0].stages.iter().map(|s| s.last_active).collect();
    assert!(saved.iter().all(Option::is_some));
}

// ----- the 8E review (8E2): the restore window, the frame against names, adoption ----------

fn focused(id: u32) -> Observation {
    Observation { focused: Some(w(id)), ..Observation::default() }
}

fn add_focused(m: &mut Model, x: WindowMeta) -> Vec<Action> {
    let id = x.id.0;
    m.add_window(x, &Decision::default(), &focused(id))
}

/// Desktop 5 adopted by its editor window, the editor's stage active.
fn adopted() -> Model {
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_eq!(m.current_space(MAIN).0, 5);
    m
}

#[test]
fn after_the_restore_window_a_pending_match_brings_its_stage_forward() {
    // H1: Chat opened hours later (the restore window long over): its window goes back to
    // its saved stage and that stage comes forward, as Apple's rule does for an app whose
    // windows are in another stage. Never parked out of the user's sight.
    let mut m = adopted();
    assert!(m.restoring());
    assert!(m.end_restore(), "the first user action, or five minutes");
    assert!(!m.restoring() && !m.end_restore());
    let a = add_focused(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert_eq!(number_of(&m, 21), Some(2), "its saved stage");
    assert_eq!(m.main_active_stage(), m.stage_of(w(21)), "switched to: {a:?}");
    assert!(switches(&a) && !parks(&a, 21));
    assert!(parks(&a, 23), "the stage left is parked");
    assert!(m.window(w(21)).is_some_and(|x| !x.parked));
    assert_eq!(m.restored_grade(w(21)), Some(Grade::Strong));
    assert_eq!(m.last_pending().map(|n| (n.parked, n.switched)), Some((false, true)));
    // A low match still never takes an inactive stage: Apple's rule, the entry kept.
    let before = m.pending_count();
    add(&mut m, win(31, 40, BROWSER, "Weather", DEFAULT));
    assert_eq!(m.restored_grade(w(31)), None);
    assert_eq!(m.pending_count(), before);
}

#[test]
fn an_unminimized_pending_window_during_and_after_the_restore_window() {
    // H1: a window the app reopened minimized shows when un-minimized. During the restore
    // window it goes quietly to its inactive stage (parked, no switch); after it, its stage
    // comes forward.
    let mut m = adopted();
    let a = add_focused(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert!(parks(&a, 21) && !switches(&a), "during: parked quietly: {a:?}");
    assert_eq!(m.main_active_stage(), m.stage_of(w(23)));
    m.end_restore();
    let a = add_focused(&mut m, win(22, 40, BROWSER, "Recipes — Example Browser", F_RECIPES));
    assert_eq!(number_of(&m, 22), Some(3));
    assert!(switches(&a) && !parks(&a, 22), "after: switched to: {a:?}");
    assert_eq!(m.main_active_stage(), m.stage_of(w(22)));
    assert_eq!(m.stage(m.stage_of(w(22)).expect("stage")).and_then(|s| s.key), Some(w(22)), "the key of its stage");
}

#[test]
fn a_desktop_made_after_the_restore_window_never_adopts_a_saved_one() {
    let mut d6 = desktop_5();
    d6.id = 6;
    d6.current = false;
    d6.stages = vec![stage(None, vec![sw(BROWSER, "Holidays", F_RECIPES, 0)]), stage(None, vec![sw(CHAT, "Family", F_TEAM, 0)])];
    d6.strip_slots = vec![1];
    let mut m = restoring(vec![desktop_5(), d6]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_eq!(m.current_space(MAIN).0, 5);
    m.end_restore();
    // The user makes a new Desktop and goes there: empty, but not provisional.
    m.arrive(MAIN, &[], &[w(23)], &LaunchHints::default());
    assert!(m.provisional_spaces().is_empty(), "no provisional Desktop after the restore window");
    let here = m.current_space(MAIN);
    add(&mut m, win(41, 30, CHAT, "Family", F_TEAM));
    add(&mut m, win(42, 40, BROWSER, "Holidays", F_RECIPES));
    assert_eq!(m.current_space(MAIN), here, "not taken for Desktop 6");
    assert_eq!(m.restored_grade(w(41)), None);
    assert_eq!(m.restored_grade(w(42)), None);
    assert_eq!(m.unvisited_saved_spaces(), 1, "Desktop 6 still waits");
}

#[test]
fn a_provisional_desktop_from_the_restore_window_is_still_adopted_hours_later() {
    // "Reopen windows" off: nothing comes back at login; the user opens Chat hours later and
    // finds its stage, brought forward.
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    let space = m.current_space(MAIN);
    m.end_restore();
    assert_eq!(m.provisional_spaces(), [space], "made during the restore window: still provisional");
    let a = add_focused(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert_eq!(m.current_space(MAIN).0, 5, "adopted");
    assert_eq!(number_of(&m, 21), Some(2));
    assert_eq!(m.main_active_stage(), m.stage_of(w(21)), "brought forward: {a:?}");
    assert!(!parks(&a, 21));
}

#[test]
fn one_window_of_a_desktop_not_shown_when_saved_does_not_adopt_it() {
    // M2, the reviewer's D1/D2 case: Desktop 5 was shown when saved, Desktop 6 was not. macOS
    // reopens a Desktop 6 window on the empty Desktop 1 first: that alone is no proof.
    let mut d6 = desktop_5();
    d6.id = 6;
    d6.current = false;
    d6.stages = vec![stage(None, vec![sw(BROWSER, "Holidays", F_RECIPES, 0)]), stage(None, vec![sw(CHAT, "Family", F_TEAM, 0)])];
    let mut m = restoring(vec![desktop_5(), d6]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    let a = add(&mut m, win(41, 30, CHAT, "Family", F_TEAM));
    assert_ne!(m.current_space(MAIN).0, 6, "one window of Desktop 6");
    assert_eq!(m.restored_grade(w(41)), None, "Apple's rule");
    assert!(!parks(&a, 41));
    // Desktop 5's window: the Desktop shown when saved takes one.
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_eq!(m.current_space(MAIN).0, 5);
    assert_eq!(m.unvisited_saved_spaces(), 1, "Desktop 6 still waits");
    assert!(m.window(w(41)).is_some_and(|x| !x.parked), "Family stays where Apple's rule put it");
    assert_eq!(m.restored_grade(w(41)), None);
}

#[test]
fn two_windows_of_a_desktop_not_shown_when_saved_adopt_it() {
    let mut d6 = desktop_5();
    d6.id = 6;
    d6.current = false;
    d6.stages = vec![stage(None, vec![sw(BROWSER, "Holidays", F_RECIPES, 0)]), stage(None, vec![sw(CHAT, "Family", F_TEAM, 0)])];
    d6.active_index = Some(0);
    let mut m = restoring(vec![d6]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(41, 30, CHAT, "Family", F_TEAM));
    assert_ne!(m.current_space(MAIN).0, 6);
    let a = add(&mut m, win(42, 40, BROWSER, "Holidays", F_RECIPES));
    assert_eq!(m.current_space(MAIN).0, 6, "the second window adopts it");
    assert_eq!(number_of(&m, 42), Some(1), "the saved active stage");
    assert_eq!(number_of(&m, 41), Some(2), "the first window back in its saved stage");
    assert!(parks(&a, 41) && !parks(&a, 42), "{a:?}");
    assert_eq!(m.stages_in(m.current_space(MAIN)).count(), 2);
    assert_eq!(m.pending_count(), 0);
}

#[test]
fn a_new_window_at_the_saved_frame_with_another_name_is_not_that_window() {
    // H2: the browser opens "Start Page" where the saved page's window was. The frame alone
    // against two different names is the size only: low, Apple's rule, the saved page
    // still awaited; nothing parked.
    let mut m = adopted();
    let a = add(&mut m, win(31, 40, BROWSER, "Start Page", F_RECIPES));
    assert_eq!(m.restored_grade(w(31)), None);
    assert!(!parks(&a, 31) && m.window(w(31)).is_some_and(|x| !x.parked));
    assert_eq!(m.pending_of(m.stages_in(m.current_space(MAIN)).nth(2).expect("browser stage").id).len(), 1, "Recipes still awaited");
    // A file manager's new window where the pending "Downloads" was.
    let finder = "com.example.files";
    let mut space = desktop_5();
    space.stages.push(stage(None, vec![sw(finder, "Downloads", F_NOTES, 0)]));
    let mut m = restoring(vec![space]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    let a = add(&mut m, win(32, 80, finder, "Recents", F_NOTES));
    assert_eq!(m.restored_grade(w(32)), None);
    assert!(!parks(&a, 32));
    assert_eq!(m.pending_bundles().iter().filter(|b| *b == finder).count(), 1, "Downloads still awaited");
}

#[test]
fn a_window_a_rule_places_is_no_longer_awaited() {
    // A rule (here: float) places the window: its confident saved entry is dropped, so it
    // neither waits forever nor catches another window.
    let mut m = adopted();
    let float = Decision { action: crate::engine::rules::RuleAction::Float, ..Decision::default() };
    m.add_window(win(21, 30, CHAT, "Team", F_TEAM), &float, &Observation::default());
    assert_eq!(m.window(w(21)).map(|x| x.home), Some(Home::Pinned));
    assert!(!m.pending_bundles().contains(&CHAT.to_string()));
    assert_eq!(names(&m), [None, None], "the Chat stage, waiting for nothing now, is gone");
    // A doubtful one (low) leaves the entry alone.
    let current = Decision { action: crate::engine::rules::RuleAction::Current, ..Decision::default() };
    m.add_window(win(31, 40, BROWSER, "Weather", DEFAULT), &current, &Observation::default());
    assert!(m.pending_bundles().contains(&BROWSER.to_string()));
}

#[test]
fn a_restored_window_does_not_take_the_key_from_the_window_the_user_focused() {
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(24, 50, EDITOR, "Notes.txt", F_NOTES));
    let active = m.main_active_stage().expect("active");
    assert_eq!(m.stage(active).and_then(|s| s.key), Some(w(24)));
    // The user works in Notes.txt; Plan.txt (the saved key window) comes back.
    m.add_window(win(23, 50, EDITOR, "Plan.txt", F_PLAN), &Decision::default(), &focused(24));
    assert_eq!(number_of(&m, 23), Some(1));
    assert_eq!(m.stage(active).and_then(|s| s.key), Some(w(24)), "the user's focus keeps the key");
    // Without a focus of the user's there, the saved key window takes it.
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(24, 50, EDITOR, "Notes.txt", F_NOTES));
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    let active = m.main_active_stage().expect("active");
    assert_eq!(m.stage(active).and_then(|s| s.key), Some(w(23)));
}

#[test]
fn an_adoption_keeps_the_strip_shown() {
    // Two apps opened before the Desktop is adopted: their cards stay.
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(51, 60, "com.example.music", "Player", DEFAULT));
    add(&mut m, win(52, 61, "com.example.memo", "Memo", F_NOTES));
    add(&mut m, win(53, 62, "com.example.paint", "Canvas", F_TEAM));
    let cards = |m: &Model| m.strip().iter().map(|c| c.windows.iter().map(|x| x.0).collect::<Vec<_>>()).collect::<Vec<_>>();
    // Not the recency order (Memo was used after Player).
    assert_eq!(cards(&m), [vec![51], vec![52]]);
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_eq!(m.current_space(MAIN).0, 5);
    // The live cards as they were (not rebuilt from the pending stages' slots, then refilled
    // by recency); the editor's stage, inactive, enters below.
    assert_eq!(cards(&m), [vec![51], vec![52], vec![23]]);
}

#[test]
fn a_window_pending_since_an_unknown_session_is_given_up_at_the_next_known_one() {
    // A session that could not be read stamps `since: ""`; the next readable new session
    // gives such windows up instead of keeping them forever.
    let mut space = desktop_5();
    space.stages[0].windows[0].pending = Some(Pending { since: String::new() });
    let mut file = StateFile { session: None, ..StateFile::single(space) };
    assert_eq!(file.start_new_session(), 1);
    assert_eq!(file.pending_windows(), 3, "the others pending since the file's (unknown) session");
    let mut space = desktop_5();
    space.stages[0].windows[0].pending = Some(Pending { since: String::new() });
    let mut file = StateFile { session: Some("B".into()), ..StateFile::single(space) };
    assert_eq!(file.start_new_session(), 1);
}

#[test]
fn a_parked_voter_goes_back_to_its_saved_stage_at_its_frame() {
    let mut d6 = desktop_5();
    d6.id = 6;
    d6.current = false;
    d6.stages = vec![stage(None, vec![sw(BROWSER, "Holidays", F_RECIPES, 0)]), stage(None, vec![sw(CHAT, "Family", F_TEAM, 0)])];
    d6.active_index = Some(0);
    let mut m = restoring(vec![d6]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(41, 30, CHAT, "Family", F_TEAM));
    // Another app by Apple's rule: its stage comes forward, Family's is parked.
    add(&mut m, win(51, 60, "com.example.music", "Player", DEFAULT));
    assert!(m.window(w(41)).is_some_and(|x| x.parked));
    let a = add(&mut m, win(42, 40, BROWSER, "Holidays", F_RECIPES));
    assert_eq!(m.current_space(MAIN).0, 6);
    // Player's stage (1), then the saved ones in saved order: Holidays (2), Family (3); the
    // stage Apple's rule made for Family is gone.
    assert_eq!((number_of(&m, 51), number_of(&m, 42), number_of(&m, 41)), (Some(1), Some(2), Some(3)));
    assert_eq!(m.stages_in(m.current_space(MAIN)).count(), 3);
    assert_eq!(m.window(w(41)).map(|x| (x.parked, x.restore)), Some((true, F_TEAM)), "parked in its saved stage, its frame kept: {a:?}");
    assert_eq!(m.main_active_stage(), m.stage_of(w(51)), "the stage shown stays in front");
    assert!(m.window(w(51)).is_some_and(|x| !x.parked));
    assert_eq!(m.restored_grade(w(41)), Some(Grade::Strong));
}
