//! Pending stages, adoption, giving up and the strip order of a restore (the design 8D's
//! engine tests). Made-up titles only.

use super::*;
use crate::engine::geometry::{Rect, Size};
use crate::engine::model::{Model, ModelOptions, Observation, Pid};
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
    restoring_with(ModelOptions::default(), spaces)
}

/// [`restoring`] with model options (declared stages, say).
fn restoring_with(options: ModelOptions, spaces: Vec<SavedSpace>) -> Model {
    let mut file = StateFile { session: Some("A".into()), ..StateFile::single(SavedSpace::default()) };
    file.spaces = spaces;
    file.start_new_session();
    let (file, _) = file.for_session(Some("B"));
    let mut m = model_with(options);
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
fn a_declared_stage_keeps_its_card_while_its_windows_wait() {
    // The 10-15 live run: `[stages] names = ["Mail"]` and Mail's windows reopened minimized
    // (in the Dock, not on screen: no window yet). A declared stage shows its card with no
    // window, as in v1.2.0; waiting for windows changes nothing about that. A stage that is
    // not declared has no card while it waits (a card is a picture of real windows).
    // Declared stages are made up front, so the saved layout has Chat first.
    let saved = SavedSpace {
        id: 5,
        current: true,
        stages: vec![
            stage(Some("Chat"), vec![sw(CHAT, "Team", F_TEAM, 0)]),
            stage(None, vec![sw(EDITOR, "Plan.txt", F_PLAN, 0), sw(EDITOR, "Notes.txt", F_NOTES, 1)]),
            stage(None, vec![sw(BROWSER, "Recipes — Example Browser", F_RECIPES, 0)]),
        ],
        active_index: Some(1),
        strip_slots: vec![2, 0],
        last_seen: None,
    };
    let options = ModelOptions { named_stages: vec!["Chat".into()], ..Default::default() };
    let mut m = restoring_with(options, vec![saved]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    assert!(m.strip().is_empty(), "the declared stage is the only one, and active: no card");
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_eq!(m.current_space(MAIN).0, 5, "adopted");
    assert_eq!(names(&m), [Some("Chat".into()), None, None], "the declared stage is the saved one, not a second Chat");
    assert_eq!(number_of(&m, 23), Some(2), "the saved active stage");
    let ids: Vec<StageId> = m.stages_in(m.current_space(MAIN)).map(|s| s.id).collect();
    let (chat, browser) = (ids[0], ids[2]);
    assert!(m.has_pending(chat) && m.has_pending(browser), "both wait");
    assert!(m.stage(chat).is_some_and(|s| s.windows.is_empty()));
    let strip: Vec<usize> = m.strip().iter().map(|c| c.number).collect();
    assert_eq!(strip, [1], "Chat's card (declared, empty); none for the browser's waiting stage");
    // Chat's window un-minimized: parked into its stage, its card now shows it.
    let a = add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert!(parks(&a, 21) && !switches(&a), "{a:?}");
    assert_eq!(m.strip().iter().map(|c| c.windows.clone()).collect::<Vec<_>>(), [vec![w(21)]]);
    add(&mut m, win(22, 40, BROWSER, "Recipes", DEFAULT));
    assert_eq!(m.strip().iter().map(|c| c.number).collect::<Vec<_>>(), [3, 1], "the saved strip order");
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
    assert_eq!(m.restored_grade(w(31)), None, "{a:?}");
    assert_eq!(number_of(&m, 31), Some(4), "a stage of its own, by Apple's rule (a card while the login settles)");
    assert!(m.strip().iter().any(|c| c.windows == [w(31)]), "never hidden in either saved stage");
    assert_eq!(m.pending_count(), 2, "both saved windows still wait");
    // After the restore window Apple's rule brings it forward, shown.
    let mut after = m.clone();
    after.end_restore();
    after.remove_window(w(31));
    let a = add(&mut after, win(33, 30, CHAT, "Untitled", DEFAULT));
    assert!(!parks(&a, 33) && after.window(w(33)).is_some_and(|x| !x.parked), "{a:?}");
    assert_eq!(after.restored_grade(w(33)), None);
    // At one of the saved frames the frame tells them apart: strong, back in its stage.
    let a = add(&mut m, win(32, 30, CHAT, "Untitled", F_RECIPES));
    assert_eq!(number_of(&m, 32), Some(3));
    assert!(parks(&a, 32));
}

#[test]
fn a_stray_of_another_saved_desktop_joins_its_saved_stage_here() {
    let mut d6 = desktop_5();
    d6.id = 6;
    d6.current = false;
    d6.stages = vec![stage(None, vec![sw(BROWSER, "Holidays", F_RECIPES, 0)]), stage(None, vec![sw(CHAT, "Family", F_TEAM, 0)])];
    d6.strip_slots = vec![1];
    let mut m = restoring(vec![desktop_5(), d6]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert_ne!(m.current_space(MAIN).0, 5, "with Desktop 6 waiting too, one window is no proof (8I, M3)");
    add(&mut m, win(22, 40, BROWSER, "Recipes — Example Browser", F_RECIPES));
    assert_eq!(m.current_space(MAIN).0, 5);
    // Desktop 6's window shows here (a stray, round 8I): its saved stage is made here, its
    // entry leaves Desktop 6, which still waits for its visit with the rest.
    let a = add(&mut m, win(41, 30, CHAT, "Family", Rect::new(500.0, 200.0, 650.0, 450.0)));
    assert_eq!(m.restored_grade(w(41)), Some(Grade::Strong), "{a:?}");
    assert!(m.is_pooled(w(41)));
    assert_ne!(m.stage_of(w(41)), m.stage_of(w(21)), "its own saved stage, not the app's");
    assert_eq!(m.current_space(MAIN).0, 5, "no adoption by a stray");
    assert_eq!(m.unvisited_saved_spaces(), 1, "Desktop 6 still waits for its visit");
    let file = m.to_state();
    assert_eq!(file.spaces.iter().find(|s| s.id == 6).map(|s| s.standard_windows()), Some(1), "Holidays only");
    let family = file.spaces.iter().flat_map(|s| &s.stages).flat_map(|s| &s.windows).filter(|w| w.title == "Family").count();
    assert_eq!(family, 1, "live in one place, waiting nowhere else");
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
    assert!(m.end_restore().is_some(), "the first user action, or five minutes");
    assert!(!m.restoring() && m.end_restore().is_none());
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
    // Two windows: with Desktop 6 waiting too, Desktop 5 takes two (the review of 8I, M3).
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    add(&mut m, win(24, 50, EDITOR, "Notes.txt", F_NOTES));
    assert_eq!(m.current_space(MAIN).0, 5);
    m.end_restore();
    // The user makes a new Desktop and goes there: empty, but not provisional.
    m.arrive(MAIN, &[], &[w(23), w(24)], &LaunchHints::default());
    assert!(m.provisional_spaces().is_empty(), "no provisional Desktop after the restore window");
    let here = m.current_space(MAIN);
    add(&mut m, win(41, 30, CHAT, "Family", F_TEAM));
    add(&mut m, win(42, 40, BROWSER, "Holidays", F_RECIPES));
    assert_eq!(m.current_space(MAIN), here, "not taken for Desktop 6");
    // Strays (round 8I): each in its saved stage here; Desktop 6, left with nothing, is gone.
    assert_eq!(m.restored_grade(w(41)), Some(Grade::Strong));
    assert_eq!(m.restored_grade(w(42)), Some(Grade::Strong));
    assert_ne!(m.stage_of(w(41)), m.stage_of(w(42)));
    assert_eq!(m.unvisited_saved_spaces(), 0, "every window of Desktop 6 is here");
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
    // Desktop 5's windows: after a restart with Desktop 6 waiting too, the Desktop shown when
    // saved takes two as well (the review of 8I, M3).
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_ne!(m.current_space(MAIN).0, 5, "one window of Desktop 5 is no proof either");
    add(&mut m, win(24, 50, EDITOR, "Notes.txt", F_NOTES));
    assert_eq!(m.current_space(MAIN).0, 5);
    assert_eq!(m.unvisited_saved_spaces(), 1, "Desktop 6 still waits");
    assert!(m.window(w(41)).is_some_and(|x| !x.parked), "Family stays where it shows");
    // Family's vote can adopt nothing now: it joins its saved stage where it is, its entry
    // gone from Desktop 6 (8I, M2): never live and waiting at once.
    assert!(m.is_pooled(w(41)) && m.restored_grade(w(41)).is_some_and(Grade::confident));
    assert!(m.adopt_votes.is_empty());
    let family = m.to_state().spaces.iter().flat_map(|s| &s.stages).flat_map(|s| &s.windows).filter(|w| w.title == "Family").count();
    assert_eq!(family, 1);
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
    assert_eq!(m.restored_grade(w(31)), None, "{a:?}");
    assert!(m.strip().iter().any(|c| c.windows == [w(31)]), "a stage of its own (a card while the login settles), not the saved page's");
    assert_eq!(m.pending_of(m.stages_in(m.current_space(MAIN)).nth(2).expect("browser stage").id).len(), 1, "Recipes still awaited");
    // A file manager's new window where the pending "Downloads" was.
    let finder = "com.example.files";
    let mut space = desktop_5();
    space.stages.push(stage(None, vec![sw(finder, "Downloads", F_NOTES, 0)]));
    let mut m = restoring(vec![space]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    let a = add(&mut m, win(32, 80, finder, "Recents", F_NOTES));
    assert_eq!(m.restored_grade(w(32)), None, "{a:?}");
    assert!(m.strip().iter().any(|c| c.windows == [w(32)]));
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
    // Player took the empty front stage; Memo and Canvas, opened while the login settles,
    // came in as cards (not brought forward).
    assert_eq!(cards(&m), [vec![52], vec![53]]);
    let front = m.main_active_stage();
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_eq!(m.current_space(MAIN).0, 5);
    // The live cards as they were (not rebuilt from the pending stages' slots, then refilled
    // by recency); the editor's stage, inactive (Player's shows), enters below.
    assert_eq!(cards(&m), [vec![52], vec![53], vec![23]]);
    assert_eq!(m.main_active_stage(), front);
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
    // Another app by Apple's rule while the login settles: a card of its own, not brought
    // forward; Family stays in front.
    add(&mut m, win(51, 60, "com.example.music", "Player", DEFAULT));
    assert!(m.window(w(41)).is_some_and(|x| !x.parked) && m.window(w(51)).is_some_and(|x| x.parked));
    let a = add(&mut m, win(42, 40, BROWSER, "Holidays", F_RECIPES));
    assert_eq!(m.current_space(MAIN).0, 6);
    // Player's stage (1), then the saved ones in saved order: Holidays (2), Family (3); the
    // stage Apple's rule made for Family is gone.
    assert_eq!((number_of(&m, 51), number_of(&m, 42), number_of(&m, 41)), (Some(1), Some(2), Some(3)));
    assert_eq!(m.stages_in(m.current_space(MAIN)).count(), 3);
    assert_eq!(m.window(w(41)).map(|x| (x.parked, x.restore)), Some((true, F_TEAM)), "parked in its saved stage, its frame kept: {a:?}");
    assert_eq!(m.main_active_stage(), m.stage_of(w(42)), "the saved active stage in front");
    assert!(m.window(w(42)).is_some_and(|x| !x.parked) && m.window(w(51)).is_some_and(|x| x.parked));
    assert_eq!(m.restored_grade(w(41)), Some(Grade::Strong));
}

#[test]
fn after_the_restore_window_a_desktop_not_shown_when_saved_moves_no_window() {
    // The re-review of 8E2: a vote cast during the restore window and a second window of the
    // Desktop after it. Adopting then would move the first window, shown by Apple's rule,
    // into a stage the user does not see. After the window a Desktop not shown when saved is
    // never adopted by votes: nothing moves.
    let d6 = || {
        let mut d6 = desktop_5();
        d6.id = 6;
        d6.current = false;
        d6.stages = vec![stage(None, vec![sw(BROWSER, "Holidays", F_RECIPES, 0)]), stage(None, vec![sw(CHAT, "Family", F_TEAM, 0)])];
        d6.active_index = Some(0);
        d6
    };
    let mut m = restoring(vec![d6()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    let here = m.current_space(MAIN);
    add(&mut m, win(41, 30, CHAT, "Family", F_TEAM));
    assert!(m.end_restore().is_some());
    assert!(m.adopt_votes.is_empty(), "votes count only while the window is open");
    let stage_41 = m.stage_of(w(41));
    let a = add(&mut m, win(42, 40, BROWSER, "Holidays", F_RECIPES));
    assert_eq!(m.current_space(MAIN), here, "not taken for Desktop 6");
    // Apple's rule only: the browser's new stage comes forward, Family's (the one Apple's
    // rule made) becomes a card; no saved stage, no saved frame.
    assert_eq!(m.stage_of(w(41)), stage_41, "the first window stays in its own stage: {a:?}");
    assert!(set_frame(&a, 41).is_none());
    assert_eq!(m.main_active_stage(), m.stage_of(w(42)));
    assert_eq!(m.stages_in(here).count(), 2, "no saved stage rebuilt");
    // The voter, at the window's end, joined its saved stage where it is (the review of 8I,
    // M2: the stage Apple's rule made, nothing moved); the second window, a stray after the
    // window, is in its saved stage (round 8I), its stage brought forward as Apple's rule
    // would.
    assert_eq!((m.restored_grade(w(41)), m.restored_grade(w(42))), (Some(Grade::Strong), Some(Grade::Strong)));
    assert_eq!(m.unvisited_saved_spaces(), 0, "every window of Desktop 6 is here");
    // Both windows after the window: the same.
    let mut m = restoring(vec![d6()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    m.end_restore();
    add(&mut m, win(41, 30, CHAT, "Family", F_TEAM));
    let stage_41 = m.stage_of(w(41));
    add(&mut m, win(42, 40, BROWSER, "Holidays", F_RECIPES));
    assert_eq!(m.current_space(MAIN), here);
    assert_eq!(m.stage_of(w(41)), stage_41);
    assert_eq!((m.restored_grade(w(41)), m.restored_grade(w(42))), (Some(Grade::Strong), Some(Grade::Strong)), "strays, each in its saved stage");
    // The Desktop shown when saved is still adopted by its window, and brought forward.
    let mut m = restoring(vec![desktop_5(), d6()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    m.end_restore();
    add(&mut m, win(41, 30, CHAT, "Family", F_TEAM));
    let a = add_focused(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert_eq!(m.current_space(MAIN).0, 5);
    assert_eq!(m.main_active_stage(), m.stage_of(w(21)), "switched to: {a:?}");
    assert_eq!(m.restored_grade(w(41)), Some(Grade::Strong), "Family, a stray, in its own saved stage");
    assert_ne!(m.stage_of(w(41)), m.stage_of(w(21)));
}

#[test]
fn a_window_closed_takes_its_vote_with_it() {
    let mut d6 = desktop_5();
    d6.id = 6;
    d6.current = false;
    d6.stages = vec![stage(None, vec![sw(BROWSER, "Holidays", F_RECIPES, 0)]), stage(None, vec![sw(CHAT, "Family", F_TEAM, 0)])];
    d6.active_index = Some(0);
    let mut m = restoring(vec![d6]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(41, 30, CHAT, "Family", F_TEAM));
    assert_eq!(m.adopt_votes.values().map(Vec::len).sum::<usize>(), 1);
    m.remove_window(w(41));
    assert!(m.adopt_votes.is_empty(), "the closed window's vote is gone");
    add(&mut m, win(42, 40, BROWSER, "Holidays", F_RECIPES));
    assert_ne!(m.current_space(MAIN).0, 6, "one window alive: not enough");
    // Family reopened: two windows alive, adopted.
    add(&mut m, win(44, 30, CHAT, "Family", F_TEAM));
    assert_eq!(m.current_space(MAIN).0, 6);
    assert!(m.adopt_votes.is_empty());
}

#[test]
fn an_empty_saved_active_stage_gives_way_at_the_restore_windows_end() {
    // The editor's stage (active when saved) waits for its windows; Chat and the browser
    // came back into their stages. At the window's end the screen shows no empty front
    // stage with cards beside it: the most recent stage with windows comes forward (Apple's
    // rule), the focused window's when there is one.
    let saved = || {
        let mut d = desktop_5();
        d.stages[1].last_active = Some(5);
        d.stages[2].last_active = Some(3);
        d
    };
    let mut m = restoring(vec![saved()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    add(&mut m, win(22, 40, BROWSER, "Recipes — Example Browser", F_RECIPES));
    let empty = m.main_active_stage().expect("the saved active stage");
    assert_eq!(m.stage(empty).map(|s| s.windows.len()), Some(0));
    assert!(m.has_pending(empty));
    assert!(m.end_restore().is_some());
    let a = m.front_after_restore(&Observation::default());
    assert_eq!(m.main_active_stage(), m.stage_of(w(21)), "the most recent stage with windows: {a:?}");
    assert!(switches(&a) && !parks(&a, 21));
    assert!(m.has_pending(empty), "the editor's stage still waits, with no card");
    assert!(m.strip().iter().all(|c| !c.windows.is_empty()), "no empty card");
    assert!(m.front_after_restore(&Observation::default()).is_empty(), "a front stage with windows stays");
    // The editor's window comes later: its stage comes forward with it (after the window).
    let a = add_focused(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_eq!(m.main_active_stage(), Some(empty), "{a:?}");

    // The window focused decides when there is one.
    let mut m = restoring(vec![saved()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    add(&mut m, win(22, 40, BROWSER, "Recipes — Example Browser", F_RECIPES));
    m.end_restore();
    m.front_after_restore(&focused(22));
    assert_eq!(m.main_active_stage(), m.stage_of(w(22)));
    // Nothing anywhere else: the empty stage stays in front (nothing to show instead).
    let mut m = restoring(vec![saved()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    m.end_restore();
    assert!(m.front_after_restore(&Observation::default()).is_empty(), "the front stage shows a window");
}

#[test]
fn voters_go_back_at_their_saved_frames() {
    // A voter matched by its title, shown at another frame: adopted into the saved active
    // stage it is moved to its saved frame, as any window back from a saved entry; a parked
    // one keeps the saved frame for when its stage shows.
    let d6 = |active| {
        let mut d6 = desktop_5();
        d6.id = 6;
        d6.current = false;
        d6.stages = vec![stage(None, vec![sw(BROWSER, "Holidays", F_RECIPES, 0)]), stage(None, vec![sw(CHAT, "Family", F_TEAM, 0)])];
        d6.active_index = Some(active);
        d6
    };
    let mut m = restoring(vec![d6(1)]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(41, 30, CHAT, "Family", DEFAULT));
    let a = add(&mut m, win(42, 40, BROWSER, "Holidays", F_RECIPES));
    assert_eq!(m.current_space(MAIN).0, 6);
    assert_eq!(m.main_active_stage(), m.stage_of(w(41)), "the saved active stage");
    assert_eq!(set_frame(&a, 41), Some(F_TEAM), "moved to its saved frame: {a:?}");
    assert_eq!(m.window(w(41)).map(|x| (x.parked, x.restore)), Some((false, F_TEAM)));
    assert_eq!(m.restored_grade(w(41)), Some(Grade::Strong));
    // Into an inactive stage: parked, its saved frame kept for later.
    let mut m = restoring(vec![d6(0)]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(41, 30, CHAT, "Family", DEFAULT));
    let a = add(&mut m, win(42, 40, BROWSER, "Holidays", F_RECIPES));
    assert_eq!(m.current_space(MAIN).0, 6);
    assert!(parks(&a, 41), "{a:?}");
    assert_eq!(m.window(w(41)).map(|x| (x.parked, x.restore)), Some((true, F_TEAM)));
}

// ----- 8F: settling, the end's focus, the restore's line, the frame gate, the setting -----

fn restoring_after(spaces: Vec<SavedSpace>, restore: Restore) -> Model {
    let mut file = StateFile { session: Some("A".into()), ..StateFile::single(SavedSpace::default()) };
    file.spaces = spaces;
    file.start_new_session();
    let (file, _) = file.for_session(Some("B"));
    let mut m = model();
    m.set_restore(restore);
    m.restore_spaces(file.spaces);
    m
}

const MAIL: &str = "com.example.mail";

#[test]
fn while_the_login_settles_nothing_switches_stages() {
    let mut m = adopted();
    assert!(m.settling());
    let editor = m.main_active_stage();
    // Chat reopens into its inactive stage: parked, no switch.
    let a = add_focused(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert!(parks(&a, 21) && !switches(&a), "{a:?}");
    // An app with no saved window: Apple's rule puts it in a stage of its own, parked there
    // with its card, not switched to.
    let a = add_focused(&mut m, win(31, 60, MAIL, "Inbox", DEFAULT));
    assert!(parks(&a, 31) && !switches(&a), "a new stage, not brought forward: {a:?}");
    assert_ne!(m.stage_of(w(31)), editor);
    assert!(m.strip().iter().any(|c| c.windows.contains(&w(31))), "its card shows");
    // Launch-time activations and focus changes switch nothing.
    assert!(m.app_activated(30, &focused(21)).is_empty());
    assert!(m.app_activated(60, &focused(31)).is_empty());
    assert!(m.window_focused(w(31), &focused(31)).is_empty());
    assert_eq!(m.main_active_stage(), editor, "the saved active stage stays in front");
    // After the window, the user's activation switches as Apple's does.
    assert!(m.end_restore().is_some());
    assert!(!m.settling());
    let a = m.app_activated(60, &focused(31));
    assert!(switches(&a) && m.main_active_stage() == m.stage_of(w(31)), "{a:?}");
}

#[test]
fn a_same_session_launch_does_not_hold_switches() {
    let mut m = model();
    m.set_restore(Restore::same_session("A"));
    m.restore_spaces(vec![desktop_5()]);
    m.arrive(MAIN, &[win(23, 50, EDITOR, "Plan.txt", F_PLAN)], &[], &LaunchHints::default());
    assert!(m.restoring() && !m.settling(), "settling is the login's, after a restart");
    let a = add_focused(&mut m, win(31, 60, MAIL, "Inbox", DEFAULT));
    assert!(switches(&a), "Apple's rule as in 1.2.0: {a:?}");
}

#[test]
fn the_saved_key_window_takes_the_keyboard_at_the_end() {
    let mut m = adopted();
    add(&mut m, win(24, 50, EDITOR, "Notes.txt", F_NOTES));
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    let key = m.main_active_stage().and_then(|s| m.stage(s)).and_then(|s| s.key);
    assert_eq!(key, Some(w(23)), "Plan.txt was the saved key");
    m.end_restore();
    let focus = |id: Option<u32>| m.focus_after_restore(&Observation { focused: id.map(w), ..Observation::default() });
    assert_eq!(focus(None), [Action::Focus { id: w(23) }], "nothing focused: the saved key window");
    assert_eq!(focus(Some(21)), [Action::Focus { id: w(23) }], "a parked window focused at launch: the key window");
    assert!(focus(Some(23)).is_empty(), "already there");
    assert!(focus(Some(24)).is_empty(), "a window shown in front has the keyboard: never taken from it");
}

#[test]
fn the_restore_line_counts_windows_stages_and_desktops() {
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    add(&mut m, win(22, 40, BROWSER, "Recipes", DEFAULT));
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    add(&mut m, win(31, 60, MAIL, "Inbox", DEFAULT));
    let s = m.restore_summary(41);
    assert_eq!(s.to_string(), "restore: 3 windows into 3 stages on 1 Desktop in 41 s; 1 pending; 1 by Apple's rule; 0 at the corner; 0 from other Desktops");
    // After the window nothing more counts as the restore's.
    m.end_restore();
    add(&mut m, win(32, 61, "com.example.other", "Other", DEFAULT));
    assert_eq!(m.restore_summary(41).by_apple, 1);
}

#[test]
fn saved_frames_come_back_only_on_a_display_of_the_saved_size() {
    // Saved on a 2560×1440 main display; the test display is 1440×900.
    let mut m = restoring_after(vec![desktop_5()], Restore::after_restart("B").with_display(Some((2560.0, 1440.0))));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    let a = add(&mut m, win(21, 30, CHAT, "Team", DEFAULT));
    assert!(parks(&a, 21), "still back in its stage");
    assert_eq!(m.window(w(21)).map(|x| x.restore), Some(DEFAULT), "the frame its app gave it");
    let a = add(&mut m, win(23, 50, EDITOR, "Plan.txt", DEFAULT));
    assert_eq!(set_frame(&a, 23), None, "a shown window is not moved: {a:?}");
    assert_eq!(m.window(w(23)).map(|x| x.restore), Some(DEFAULT));
    // A window reopened in the park zone still comes out of it.
    let corner = park_frame(Size::new(640.0, 420.0), FRAME);
    add(&mut m, win(24, 50, EDITOR, "Notes.txt", corner));
    assert!(m.window(w(24)).is_some_and(|x| !parked_at(x.restore)));
    // The same display size: the saved frames.
    let mut m = restoring_after(vec![desktop_5()], Restore::after_restart("B").with_display(Some((1440.0, 900.0))));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(21, 30, CHAT, "Team", DEFAULT));
    assert_eq!(m.window(w(21)).map(|x| x.restore), Some(F_TEAM));
}

#[test]
fn restore_after_restart_off_keeps_nothing_waiting_as_before() {
    // The setting off: a launch after a restart behaves as 1.2.0 did.
    let mut m = restoring_after(vec![desktop_5()], Restore::off_after_restart("B"));
    m.arrive(MAIN, &[win(21, 30, CHAT, "Team", F_TEAM)], &[], &LaunchHints::default());
    assert!(!m.restoring() && !m.settling() && m.provisional_spaces().is_empty());
    assert_eq!(m.current_space(MAIN).0, 5, "the saved Desktop recognized by its window");
    assert_eq!(m.pending_count(), 0, "nothing waits");
    assert_eq!(names(&m), [None, Some("Chat".into())], "the browser's stage, with no window on screen, is gone");
    let file = m.to_state();
    assert!(file.spaces.iter().flat_map(|s| &s.stages).flat_map(|s| &s.windows).all(|w| w.pending.is_none()));
    assert_eq!(file.spaces[0].standard_windows(), 1);
    // A window opened later goes by Apple's rule, switched to.
    let a = add_focused(&mut m, win(22, 40, BROWSER, "Recipes — Example Browser", F_RECIPES));
    assert!(m.restored_grade(w(22)).is_none() && m.window(w(22)).is_some_and(|x| !x.parked), "Apple's rule, shown: {a:?}");
    assert_eq!(m.main_active_stage(), m.stage_of(w(22)));
}

#[test]
fn an_empty_saved_active_stage_given_up_still_gives_way() {
    // The review of 1e15dcf: the editor's windows given up (its app settled) before the
    // window's end left an empty front stage with cards beside it.
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    add(&mut m, win(22, 40, BROWSER, "Recipes — Example Browser", F_RECIPES));
    let editor = m.main_active_stage().expect("the saved active stage");
    m.expire_pending(&[EDITOR.to_string()]);
    assert!(!m.has_pending(editor) && m.main_active_stage() == Some(editor), "empty, still in front");
    m.end_restore();
    let a = m.front_after_restore(&Observation::default());
    assert!(switches(&a) && m.main_active_stage() != Some(editor), "a stage with windows comes forward: {a:?}");
    assert!(m.strip().iter().all(|c| !c.windows.is_empty()), "no empty card");

    // Given up after the window's end (the front not brought forward then): the next pass does.
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    let editor = m.main_active_stage().expect("the saved active stage");
    m.end_restore();
    m.expire_pending(&[EDITOR.to_string()]);
    let a = m.front_after_restore(&Observation::default());
    assert!(switches(&a) && m.main_active_stage() == m.stage_of(w(21)), "{a:?}");
    assert_ne!(m.main_active_stage(), Some(editor));

    // One that got a window waits no more: it stays in front.
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    let editor = m.main_active_stage();
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    m.expire_pending(&[EDITOR.to_string()]);
    m.end_restore();
    assert!(m.front_after_restore(&Observation::default()).is_empty());
    assert_eq!(m.main_active_stage(), editor);
}

/// The restore phase's layout (made-up titles): Mail (declared) holds two editor windows, the
/// active stage the editor's third window and Chat's, the browser a stage of its own.
fn mail_desktop() -> (Model, StageId) {
    let saved = SavedSpace {
        id: 7,
        current: true,
        stages: vec![
            stage(Some("Mail"), vec![sw(EDITOR, "Inbox", F_NOTES, 0), sw(EDITOR, "Drafts", F_TEAM, 1)]),
            stage(None, vec![sw(EDITOR, "Plan.txt", F_PLAN, 0), sw(CHAT, "Team", F_RECIPES, 1)]),
            stage(None, vec![sw(BROWSER, "Recipes — Example Browser", DEFAULT, 0)]),
        ],
        active_index: Some(1),
        strip_slots: vec![2, 0],
        last_seen: None,
    };
    let options = ModelOptions { named_stages: vec!["Mail".into()], ..Default::default() };
    let mut m = restoring_with(options, vec![saved]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    add(&mut m, win(24, 30, CHAT, "Team", F_RECIPES));
    assert_eq!(m.current_space(MAIN).0, 7, "adopted");
    let mail = m.stages_in(m.current_space(MAIN)).next().map(|s| s.id).expect("Mail");
    assert_eq!(m.stage(mail).and_then(|s| s.name.clone()).as_deref(), Some("Mail"));
    assert_eq!(number_of(&m, 23), Some(2), "the saved active stage");
    (m, mail)
}

#[test]
fn an_app_s_window_unminimized_during_the_restore_window_goes_to_its_saved_stage_not_its_app_s() {
    // The 10-15 live run: the editor (Alpha) has a window shown in the active stage; its
    // other two reopened minimized and wait for Mail. Un-minimized while the restore window
    // is open, one goes quietly to Mail (parked, no switch), never to the active stage as
    // Apple's rule for the app's window shown would put it.
    let (mut m, mail) = mail_desktop();
    let active = m.main_active_stage();
    let a = add_focused(&mut m, win(21, 50, EDITOR, "Inbox", F_NOTES));
    assert_eq!(m.stage_of(w(21)), Some(mail), "its saved stage");
    assert!(parks(&a, 21) && !switches(&a), "parked quietly: {a:?}");
    assert!(m.window(w(21)).is_some_and(|x| x.parked && x.restore == F_NOTES));
    assert_eq!(m.restored_grade(w(21)), Some(Grade::Strong));
    assert_eq!(m.last_pending().map(|n| (n.window, n.parked, n.switched)), Some((w(21), true, false)));
    assert_eq!(m.main_active_stage(), active, "no switch");
    assert!(m.has_pending(mail), "Drafts still waits");
}

#[test]
fn an_app_s_window_unminimized_after_the_restore_window_brings_its_saved_stage_forward() {
    // The same, after the restore window: Mail comes forward with it (the window shows
    // because of the user), the stage left is parked, and an app with no saved window then
    // goes by Apple's rule, leaving Mail's windows where they are.
    let (mut m, mail) = mail_desktop();
    add_focused(&mut m, win(21, 50, EDITOR, "Inbox", F_NOTES));
    assert!(m.end_restore().is_some());
    let a = add_focused(&mut m, win(22, 50, EDITOR, "Drafts", F_TEAM));
    assert_eq!(m.stage_of(w(22)), Some(mail), "its saved stage");
    assert_eq!(m.main_active_stage(), Some(mail), "came forward: {a:?}");
    assert!(switches(&a) && !parks(&a, 22));
    assert!(parks(&a, 23) && parks(&a, 24), "the stage left is parked");
    assert!(m.window(w(21)).is_some_and(|x| !x.parked) && m.window(w(22)).is_some_and(|x| !x.parked));
    assert_eq!(m.restored_grade(w(22)), Some(Grade::Strong));
    assert_eq!(m.last_pending().map(|n| (n.window, n.parked, n.switched)), Some((w(22), false, true)));
    assert!(!m.has_pending(mail));
    // An app with no saved window: a stage of its own, in front.
    const OTHER: &str = "com.example.other";
    add_focused(&mut m, win(31, 60, OTHER, "Sketch", DEFAULT));
    assert_eq!(m.restored_grade(w(31)), None);
    let own = m.stage_of(w(31)).expect("managed");
    assert!(own != mail && m.main_active_stage() == Some(own));
    assert_eq!(m.stage(mail).map(|s| s.windows.clone()), Some(vec![w(21), w(22)]), "Mail untouched");
}

// ----- the review of 8F (8F2) ---------------------------------------------------------------

#[test]
fn a_new_stage_takes_the_front_from_an_empty_saved_active_stage_while_the_login_settles() {
    // M4: the editor (the saved active stage) is slow to open; Chat came back into its
    // inactive stage. An app with no saved window opens a window: rather than a card beside
    // an empty screen for up to ~80 s, its stage of its own takes the front (nothing shown
    // there to animate away). The editor's stage waits, inactive, with no card.
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    let editor = m.main_active_stage().expect("the saved active stage");
    assert!(m.settling() && m.has_pending(editor) && m.stage(editor).is_some_and(|s| s.windows.is_empty()));
    let a = add_focused(&mut m, win(31, 60, MAIL, "Inbox", DEFAULT));
    let own = m.stage_of(w(31)).expect("managed");
    assert!(own != editor && m.main_active_stage() == Some(own), "its own stage, in front: {a:?}");
    assert!(!parks(&a, 31) && m.window(w(31)).is_some_and(|x| !x.parked), "shown, not parked");
    assert!(m.has_pending(editor) && m.stage(editor).is_some(), "the editor's stage still waits");
    assert!(m.strip().iter().all(|c| c.stage != editor && !c.windows.is_empty()), "with no card");
    // Another app with no saved window now: the front shows a window, so a card as before.
    let a = add_focused(&mut m, win(32, 61, "com.example.other", "Sketch", DEFAULT));
    assert!(parks(&a, 32) && m.main_active_stage() == Some(own), "a card, no switch: {a:?}");
    // The editor's window comes during the restore window: parked in its saved stage, which
    // gets its card in its saved place; the front stays.
    let a = add_focused(&mut m, win(23, 50, EDITOR, "Plan.txt", F_PLAN));
    assert_eq!(m.stage_of(w(23)), Some(editor));
    assert!(parks(&a, 23) && !switches(&a) && m.main_active_stage() == Some(own), "{a:?}");
    assert!(m.strip().iter().any(|c| c.stage == editor), "the editor's card shows");
    // At the end: nothing to give way (the front shows a window); its key window is the one
    // that takes the keyboard.
    assert!(m.end_restore().is_some());
    assert!(m.front_after_restore(&Observation::default()).is_empty());
    assert_eq!(m.focus_after_restore(&Observation::default()), [Action::Focus { id: w(31) }]);
    // After the window the editor's other window brings its stage forward, as usual.
    let a = add_focused(&mut m, win(24, 50, EDITOR, "Notes.txt", F_NOTES));
    assert_eq!(m.main_active_stage(), Some(editor), "{a:?}");
}

#[test]
fn a_new_window_joins_no_saved_stage_and_a_front_with_windows_stays_while_settling() {
    // M4's edge: the saved active stage shows a window already: a new stage is a card.
    let mut m = adopted();
    let editor = m.main_active_stage();
    let a = add_focused(&mut m, win(31, 60, MAIL, "Inbox", DEFAULT));
    assert!(parks(&a, 31) && m.main_active_stage() == editor, "{a:?}");
    // Before any saved Desktop is adopted the screen's own empty stage is simply used (no
    // waiting stage in front then).
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    let front = m.main_active_stage();
    let a = add_focused(&mut m, win(31, 60, MAIL, "Inbox", DEFAULT));
    assert!(!parks(&a, 31) && m.stage_of(w(31)) == front, "{a:?}");
}

#[test]
fn the_restore_line_is_logged_after_a_restart_only() {
    // L1: `log summary` repeats it as "after a restart".
    let mut m = restoring(vec![desktop_5()]);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(21, 30, CHAT, "Team", F_TEAM));
    assert_eq!(m.restore_line(12).map(|s| s.windows), Some(1));
    let mut m = model();
    m.set_restore(Restore::same_session("A"));
    m.restore_spaces(vec![desktop_5()]);
    m.arrive(MAIN, &[win(23, 50, EDITOR, "Plan.txt", F_PLAN)], &[], &LaunchHints::default());
    assert!(m.restoring() && m.restore_line(12).is_none(), "a relaunch in the same session");
}

// ----- frames after a restart (round 8H) ------------------------------------------------------

/// Where macOS puts a window of `size` whose app saved a parked frame: squeezed onto the screen
/// against the park corner (the 10-16 restart: right and bottom edges on the visible frame's).
fn clamped(size: Rect) -> Rect {
    let v = VISIBLE;
    Rect::new(v.max_x() - size.w, v.max_y() - size.h, size.w, size.h)
}

#[test]
fn a_window_clamped_against_the_corner_is_matched_by_title_and_app_and_takes_its_saved_frame() {
    let mut m = adopted();
    // Chat's stage is inactive: parked there at its saved frame, whatever frame it came at.
    let a = add(&mut m, win(21, 30, CHAT, "Team", clamped(F_TEAM)));
    assert_eq!(m.restored_grade(w(21)), Some(Grade::Strong), "the title and the app agree");
    assert!(parks(&a, 21), "{a:?}");
    assert_eq!(m.window(w(21)).map(|s| s.restore), Some(F_TEAM), "its saved frame, not the corner");
    // The editor's second window, in the stage shown: moved back to its saved frame.
    let a = add(&mut m, win(24, 50, EDITOR, "Notes.txt", clamped(F_NOTES)));
    assert_eq!(set_frame(&a, 24), Some(F_NOTES), "{a:?}");
    // Even when its size changed too (macOS shrank it to fit).
    let small = Rect::new(VISIBLE.max_x() - 300.0, VISIBLE.max_y() - 200.0, 300.0, 200.0);
    let a = add(&mut m, win(22, 40, BROWSER, "Recipes — Example Browser", small));
    assert!(m.restored_grade(w(22)).is_some_and(Grade::confident), "matched with confidence");
    assert!(parks(&a, 22));
    assert_eq!(m.window(w(22)).map(|s| s.restore), Some(F_RECIPES));
    assert_eq!(m.restore_summary(0).corner, 0, "matched: not counted at the corner");
}

#[test]
fn an_unmatched_window_at_the_corner_is_left_alone_and_counted_one_at_the_park_spot_comes_back() {
    let mut m = adopted();
    let at_corner = clamped(Rect::new(0.0, 0.0, 500.0, 300.0));
    let a = add(&mut m, win(31, 60, MAIL, "Inbox", at_corner));
    assert_eq!(m.restored_grade(w(31)), None, "no saved window of Mail: Apple's rule");
    assert_eq!(set_frame(&a, 31), None, "left where it is: {a:?}");
    assert_eq!(m.window(w(31)).map(|s| s.restore), Some(at_corner));
    let at_park = park_frame(Size { w: 500.0, h: 300.0 }, FRAME);
    assert!(parked_at(at_park));
    let a = add(&mut m, win(32, 61, "com.example.notes", "List", at_park));
    let back = m.window(w(32)).map(|s| s.restore).expect("managed");
    assert!(!parked_at(back), "brought back from the park spot ({back:?})");
    assert!(set_frame(&a, 32).is_some_and(|f| !parked_at(f)) || parks(&a, 32), "moved out of the park spot or parked by its stage: {a:?}");
    let s = m.restore_summary(41);
    assert_eq!((s.by_apple, s.corner), (2, 1), "only the clamped one counts at the corner");
    assert!(s.to_string().ends_with("2 by Apple's rule; 1 at the corner; 0 from other Desktops"), "{s}");
}
