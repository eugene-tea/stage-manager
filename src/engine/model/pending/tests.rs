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
    m.set_restore(Restore { session: "B".into(), restoring: true });
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
    // in, at its own frame.
    let a = add(&mut m, win(24, 50, EDITOR, "Untitled", DEFAULT));
    assert_eq!(m.restored_grade(w(24)), Some(Grade::Low));
    assert_eq!(number_of(&m, 24), Some(1));
    assert_eq!(set_frame(&a, 24), None, "low: the live frame stays");
    assert!(!parks(&a, 24));
    let active = m.main_active_stage().expect("active");
    assert_eq!(m.stage(active).map(|s| s.windows.clone()), Some(vec![w(23), w(24)]), "in saved order");
    assert_eq!(m.stage(active).and_then(|s| s.key), Some(w(23)), "the saved key window");
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
    m.set_restore(Restore { session: "A".into(), restoring: false });
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
    // Back on Desktop 1, empty: provisional again; its first window adopts it again.
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    assert!(m.provisional_spaces().contains(&m.current_space(MAIN)));
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert_eq!(m.current_space(MAIN).0, 5);
    assert_eq!(number_of(&m, 21), Some(2));
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
