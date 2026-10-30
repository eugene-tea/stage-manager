//! Windows of apps that quit, kept for the file (round 8H): the stages survive in
//! `state.json`, at the windows' real frames, and come back after a restart. Made-up titles.

use super::*;
use crate::engine::geometry::Rect;
use crate::engine::model::{Action, Model, Observation, WindowMeta};
use crate::engine::park::park_frame;
use crate::engine::persist::{LaunchHints, StateFile};
use crate::engine::restore::{QUIT_KEEP, Restore};
use crate::engine::rules::Decision;
use crate::engine::testkit::*;

const EDITOR: &str = "com.example.editor";
const CHAT: &str = "com.example.chat";
const BROWSER: &str = "com.example.browser";
const MAIL: &str = "com.example.mail";

const F_PLAN: Rect = Rect::new(100.0, 80.0, 600.0, 400.0);
const F_NOTES: Rect = Rect::new(160.0, 120.0, 640.0, 420.0);
const F_TEAM: Rect = Rect::new(400.0, 100.0, 700.0, 500.0);
const F_RECIPES: Rect = Rect::new(250.0, 150.0, 900.0, 600.0);
const F_INBOX: Rect = Rect::new(60.0, 60.0, 800.0, 560.0);

/// When the apps quit (Unix seconds).
const T: u64 = 1_760_600_000;

fn win(id: u32, pid: i32, bundle: &str, title: &str, frame: Rect) -> WindowMeta {
    WindowMeta { title: title.into(), frame, ..meta(id, pid, bundle) }
}

fn add(m: &mut Model, x: WindowMeta) -> Vec<Action> {
    m.add_window(x, &Decision::default(), &Observation::default())
}

/// The editor (two windows), Chat, the browser and Mail, each app in a stage of its own
/// (Apple's rule), the editor's in front.
fn desk() -> Model {
    let mut m = model();
    add(&mut m, win(11, 10, EDITOR, "Plan.txt", F_PLAN));
    add(&mut m, win(12, 10, EDITOR, "Notes.txt", F_NOTES));
    add(&mut m, win(21, 20, CHAT, "Team", F_TEAM));
    add(&mut m, win(31, 30, BROWSER, "Recipes — Example Browser", F_RECIPES));
    add(&mut m, win(41, 40, MAIL, "Inbox", F_INBOX));
    let editor = m.stage_of(w(11)).expect("the editor's stage");
    m.switch_to(editor, &Observation::default());
    assert_eq!(m.main_active_stage(), Some(editor));
    m
}

/// The app of `pid` quits: each of its windows read as it goes, then kept.
fn quit_app(m: &mut Model, ids: &[u32], at: u64) {
    let mut closing = Vec::new();
    for &id in ids {
        if let Some(c) = m.closing(w(id)) {
            closing.push(c);
        }
        m.remove_window(w(id));
    }
    m.keep_quit(closing, at);
}

/// A file as the layout a user sees in it: per stage its name and windows (title, frame), the
/// active stage, the strip (stage indices), the key per stage.
type Layout = (Vec<(Option<String>, Vec<(String, Rect)>)>, Option<usize>, Vec<usize>, Vec<Option<String>>);

fn layout(f: &StateFile) -> Layout {
    let sp = &f.spaces[0];
    let stages = sp.stages.iter().map(|s| (s.name.clone(), s.windows.iter().map(|w| (w.title.clone(), w.frame)).collect())).collect();
    let keys = sp.stages.iter().map(|s| s.windows.iter().find(|w| w.key).map(|w| w.title.clone())).collect();
    (stages, sp.active_index, sp.strip_slots.clone(), keys)
}

#[test]
fn an_apps_quit_keeps_its_stage_in_the_file_at_its_real_frame() {
    let mut m = desk();
    let before = layout(&m.to_state());
    let chat = m.window(w(21)).expect("Chat's window").clone();
    assert!(chat.parked, "Chat's stage is inactive: its window is parked");
    quit_app(&mut m, &[21], T);
    assert!(m.window(w(21)).is_none(), "the window left the model, as Apple's does");
    assert!(m.strip().iter().all(|c| !c.windows.contains(&w(21))), "no card for it");
    let file = m.to_state();
    assert_eq!(layout(&file), before, "the file keeps the stages it had, in their places and in the strip");
    let saved = file.spaces[0].stages.iter().flat_map(|s| &s.windows).find(|x| x.title == "Team").expect("kept");
    assert_eq!(saved.quit_at, Some(T));
    assert_eq!(saved.frame, chat.restore, "its last real frame");
    assert_eq!(saved.frame, F_TEAM);
    assert!(!parked_at(saved.frame), "never the park frame");
}

#[test]
fn the_front_stages_app_quits_and_its_stage_keeps_its_windows_and_key() {
    let mut m = desk();
    let before = layout(&m.to_state());
    // The windows leave in any order (an app closes them as it quits).
    quit_app(&mut m, &[12, 11], T);
    assert!(m.main_active_stage().is_some_and(|s| m.stage(s).is_some_and(|s| s.windows.is_empty())), "the front stage stays, empty");
    let file = m.to_state();
    assert_eq!(layout(&file), before, "Plan.txt first and the key, as before");
}

#[test]
fn the_owners_restart_without_an_early_signal_keeps_every_stage() {
    // 10-16: the apps quit one by one before any logout signal; activations in between brought
    // other stages forward and every emptied stage left the strip.
    let mut m = desk();
    let before = layout(&m.to_state());
    quit_app(&mut m, &[41], T); // Mail
    quit_app(&mut m, &[11, 12], T + 1); // the editor (the front stage)
    let chat = m.stage_of(w(21)).expect("Chat's stage");
    m.switch_to(chat, &Observation::default()); // Chat activated as the others quit
    quit_app(&mut m, &[31], T + 3); // the browser
    quit_app(&mut m, &[21], T + 7); // Chat last
    assert!(m.strip().is_empty(), "nothing on screen, no card");
    let file = m.to_state();
    let (stages, active, strip, _) = layout(&file);
    assert_eq!(stages, before.0, "every stage, its windows and frames, in its place");
    assert_eq!(file.spaces[0].stages.iter().flat_map(|s| &s.windows).filter(|w| w.quit_at.is_some()).count(), 5);
    assert_eq!(active, before.0.iter().position(|s| s.1.iter().any(|w| w.0 == "Team")), "Chat came in front last");
    // The strip keeps the places their cards had (the editor's stage was in front when it
    // emptied: it had none).
    let titled = |i: &usize| stages[*i].1[0].0.clone();
    let before_strip: Vec<String> = before.2.iter().map(|i| before.0[*i].1[0].0.clone()).filter(|t| t != "Team").collect();
    assert_eq!(strip.iter().map(titled).collect::<Vec<_>>(), before_strip, "({strip:?})");
}

#[test]
fn the_kept_stages_come_back_after_the_restart_even_clamped_against_the_corner() {
    let mut m = desk();
    let before = layout(&m.to_state());
    quit_app(&mut m, &[21], T);
    quit_app(&mut m, &[31], T + 2);
    quit_app(&mut m, &[41], T + 4);
    quit_app(&mut m, &[11, 12], T + 6);
    // The head start's (or the quit's) save, a few seconds later.
    let mut file = m.to_state();
    file.session = Some("A".into());
    file.saved_at = Some(T + 9);
    let json = file.to_json().expect("encodes");

    // The next login: session B.
    let mut file = StateFile::from_json(&json).expect("parses");
    assert_eq!(file.settle_quit(true), (5, 0), "every window that quit within two minutes waits");
    file.start_new_session();
    let (file, _) = file.for_session(Some("B"));
    let mut m = model();
    m.set_restore(Restore::after_restart("B"));
    m.restore_spaces(file.spaces);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    assert!(m.strip().is_empty(), "nothing yet: no card");

    // The apps reopen: the editor where it was; Chat squeezed against the park corner (its app
    // saved the park frame: macOS clamped it onto the screen); the browser at the park spot.
    let vis = display().visible;
    let clamped = Rect::new(vis.max_x() - F_TEAM.w, vis.max_y() - F_TEAM.h, F_TEAM.w, F_TEAM.h);
    let at_park = park_frame(F_RECIPES.size(), FRAME);
    add(&mut m, win(51, 50, EDITOR, "Plan.txt", F_PLAN));
    add(&mut m, win(52, 50, EDITOR, "Notes.txt", F_NOTES));
    let a_chat = add(&mut m, win(61, 60, CHAT, "Team", clamped));
    add(&mut m, win(71, 70, BROWSER, "Recipes — Example Browser", at_park));
    add(&mut m, win(81, 80, MAIL, "Inbox", F_INBOX));
    assert_eq!(m.current_space(MAIN).0, 1, "the saved Desktop adopted");
    for (id, frame) in [(51, F_PLAN), (52, F_NOTES), (61, F_TEAM), (71, F_RECIPES), (81, F_INBOX)] {
        let st = m.window(w(id)).expect("managed");
        assert_eq!(st.restore, frame, "window {id}: its saved frame");
        assert!(m.restored_grade(w(id)).is_some_and(|g| g.confident()), "window {id}: matched by title and app");
    }
    assert!(a_chat.iter().any(|a| matches!(a, Action::Park { id, .. } if *id == w(61))), "Chat's stage is inactive: parked there, never left in the corner");
    let after = layout(&m.to_state());
    assert_eq!(after.0, before.0, "the stages as before the restart");
    assert_eq!(after.1, before.1, "the editor's stage in front");
    assert_eq!(m.restore_summary(0).corner, 0, "matched windows are no corner leftovers");
}

#[test]
fn entries_last_two_minutes_and_go_when_their_app_is_back() {
    let mut m = desk();
    quit_app(&mut m, &[21], T);
    m.prune_quit(T + QUIT_KEEP.as_secs());
    assert_eq!(m.quit_windows().len(), 1, "two minutes: kept");
    m.prune_quit(T + QUIT_KEEP.as_secs() + 1);
    assert!(m.quit_windows().is_empty(), "older: a login does not reopen it");
    let before = layout(&m.to_state());
    assert!(before.0.iter().all(|s| s.1.iter().all(|w| w.0 != "Team")), "Chat's stage gone from the file with it");

    // The user quits Chat and opens it again in this session: its window goes by Apple's rule
    // and the entry from its quit goes.
    let mut m = desk();
    quit_app(&mut m, &[21], T);
    add(&mut m, win(22, 21, CHAT, "Team", F_TEAM));
    assert!(m.quit_windows().is_empty(), "its app is back");
    let titles: Vec<String> = m.to_state().spaces[0].stages.iter().flat_map(|s| &s.windows).map(|w| w.title.clone()).collect();
    assert_eq!(titles.iter().filter(|t| *t == "Team").count(), 1, "one entry, the live window's ({titles:?})");
}

#[test]
fn a_window_closed_by_the_user_is_not_kept() {
    // The platform keeps only the windows of an app that quits; one closed alone is gone.
    let mut m = desk();
    m.remove_window(w(21));
    assert!(m.quit_windows().is_empty());
    assert!(m.to_state().spaces[0].stages.iter().flat_map(|s| &s.windows).all(|w| w.title != "Team"));
}

#[test]
fn a_stage_that_left_goes_back_before_the_first_stage_made_after_it() {
    let q = |stage: u64, title: &str, z: usize, slot: Option<usize>| QuitWindow {
        space: SpaceId(1),
        stage: StageId(stage),
        name: None,
        last_active: stage,
        slot,
        window: SavedWindow { bundle_id: "com.example.app".into(), title: title.into(), z, key: z == 0, quit_at: Some(T), ..Default::default() },
    };
    let live_stage = |title: &str| SavedStage { windows: vec![SavedWindow { title: title.into(), key: true, ..Default::default() }], ..Default::default() };
    // Live stages 1 and 4 (strip: 4); stages 2 and 3 left (their cards were the first two).
    let (q2a, q2b, q3) = (q(2, "Two B", 1, Some(0)), q(2, "Two A", 0, Some(0)), q(3, "Three", 0, Some(1)));
    let (stages, active, slots) =
        merge_quit(&[StageId(1), StageId(4)], vec![live_stage("One"), live_stage("Four")], Some(0), vec![1], &[&q2a, &q2b, &q3]);
    let titles: Vec<Vec<&str>> = stages.iter().map(|s| s.windows.iter().map(|w| w.title.as_str()).collect()).collect();
    assert_eq!(titles, [vec!["One"], vec!["Two A", "Two B"], vec!["Three"], vec!["Four"]]);
    assert_eq!(active, Some(0));
    assert_eq!(slots, [1, 2, 3], "their cards first, then Four's");
    assert!(stages[1].windows[0].key && !stages[1].windows[1].key, "one key, the front window");
    // An entry whose stage is still there goes after its windows, never its key.
    let (stages, _, _) = merge_quit(&[StageId(2)], vec![live_stage("Two C")], Some(0), vec![], &[&q2b]);
    let w: Vec<(&str, usize, bool)> = stages[0].windows.iter().map(|w| (w.title.as_str(), w.z, w.key)).collect();
    assert_eq!(w, [("Two C", 0, true), ("Two A", 1, false)]);
}

// ----- a quit found late (round missed-quit) ---------------------------------------------------

const HOME_APP: &str = "com.example.home";
const F_HOME: Rect = Rect::new(300.0, 90.0, 800.0, 600.0);

/// A window of a new app as the platform adds it with `[stages] new_apps_join_current` on.
fn add_joining(m: &mut Model, x: WindowMeta) -> Vec<Action> {
    let (decision, _) = Decision::default().for_new_window(m.app_is_new(x.pid), true, false);
    m.add_window(x, &decision, &Observation::default())
}

/// [`desk`] with the home app's window in a stage of its own (opened before the key was on),
/// the editor's stage in front again.
fn desk_with_home() -> Model {
    let mut m = desk();
    add(&mut m, win(91, 90, HOME_APP, "Home", F_HOME));
    let editor = m.stage_of(w(11)).expect("the editor's stage");
    m.switch_to(editor, &Observation::default());
    assert_ne!(m.stage_of(w(91)), Some(editor), "a stage of its own");
    m
}

/// The file as a save in session "A" at `at` writes it.
fn saved_file(m: &Model, at: u64) -> StateFile {
    let mut file = m.to_state();
    file.session = Some("A".into());
    file.saved_at = Some(at);
    StateFile::from_json(&file.to_json().expect("encodes")).expect("parses")
}

/// A relaunch of ours in the same window server session (as `load_state` does), the five
/// windows of [`desk`] still open, the restore window ended.
fn relaunched_same_session(mut file: StateFile) -> Model {
    file.settle_quit(false);
    let (file, _) = file.for_session(Some("A"));
    let mut m = model();
    m.set_restore(Restore::same_session("A"));
    m.restore_spaces(file.spaces);
    let live = [
        win(11, 10, EDITOR, "Plan.txt", F_PLAN),
        win(12, 10, EDITOR, "Notes.txt", F_NOTES),
        win(21, 20, CHAT, "Team", F_TEAM),
        win(31, 30, BROWSER, "Recipes — Example Browser", F_RECIPES),
        win(41, 40, MAIL, "Inbox", F_INBOX),
    ];
    m.arrive(MAIN, &live, &[], &LaunchHints::default());
    m.end_restore();
    m
}

#[test]
fn an_app_found_gone_quits_as_with_a_notice_and_its_next_launch_joins_the_current_stage() {
    // 10-23: Home quit with its last window and no notice came. Its process found gone runs
    // the termination path: its windows leave (destroyed, then the app's quit).
    let mut m = desk_with_home();
    let stages = m.stages().len();
    quit_app(&mut m, &[91], T);
    assert!(m.window(w(91)).is_none(), "the window left the model");
    assert!(m.strip().iter().all(|c| !c.windows.contains(&w(91))), "no gray card left behind");
    assert_eq!(m.stages().len(), stages - 1, "its stage, left empty, went as Apple's does");
    let entry = m.to_state().spaces[0].stages.iter().flat_map(|s| &s.windows).find(|x| x.bundle_id == HOME_APP).cloned();
    assert_eq!(entry.and_then(|x| x.quit_at), Some(T), "its entry has its quit_at");

    // Launched again later in this session (any time: the entries kept for the file are never
    // matched in the session): the key's rule, the current stage.
    let editor = m.stage_of(w(11));
    add_joining(&mut m, win(92, 95, HOME_APP, "Home", F_HOME));
    assert_eq!(m.stage_of(w(92)), editor, "joins the current stage");
    assert_eq!(m.main_active_stage(), editor, "no switch");
    assert!(m.restored_grade(w(92)).is_none(), "nothing restored");
    assert!(m.quit_windows().is_empty(), "its app is back: the entry from its quit goes");
}

#[test]
fn a_relaunch_of_ours_after_the_quit_never_restores_its_window() {
    // Two minutes and more after the quit, or any time in the same window server session: a
    // load drops the entry, so the app's next window goes by the new-window rules.
    let mut m = desk_with_home();
    quit_app(&mut m, &[91], T);
    let file = saved_file(&m, T + QUIT_KEEP.as_secs() + 60);
    let mut m = relaunched_same_session(file);
    assert_eq!(m.pending_count(), 0, "nothing of the home app waits");
    let editor = m.stage_of(w(11));
    add_joining(&mut m, win(92, 95, HOME_APP, "Home", F_HOME));
    assert_eq!(m.stage_of(w(92)), editor, "joins the current stage");
    assert!(m.restored_grade(w(92)).is_none());
}

#[test]
fn without_its_quit_the_dead_apps_window_was_saved_live_and_came_back_to_its_old_stage() {
    // The bug as it was: the window of the app that quit stayed in the model, was saved as a
    // live window (no quit_at), waited after our next launch and took the app's next window
    // back into its old stage, over the key.
    let m = desk_with_home();
    let file = saved_file(&m, T);
    let mut m = relaunched_same_session(file);
    assert_eq!(m.pending_count(), 1, "the home app's window waits, as any saved window not on screen");
    let editor = m.stage_of(w(11));
    add_joining(&mut m, win(92, 95, HOME_APP, "Home", F_HOME));
    assert!(m.restored_grade(w(92)).is_some_and(|g| g.confident()), "back in its saved stage");
    assert_ne!(m.stage_of(w(92)), editor, "not the current stage");
    assert_eq!(m.main_active_stage(), m.stage_of(w(92)), "switched there");
}

/// After a restart (the home app quit at the logout, a few seconds before the save), the
/// restore window open: the stages rebuilt pending, none of the apps reopened yet.
fn restarted_with_home() -> Model {
    let mut m = desk_with_home();
    quit_app(&mut m, &[91], T);
    let mut file = saved_file(&m, T + 5);
    assert_eq!(file.settle_quit(true), (1, 0), "quit within two minutes of the save: it waits");
    file.start_new_session();
    let (file, _) = file.for_session(Some("B"));
    let mut m = model();
    m.set_restore(Restore::after_restart("B"));
    m.restore_spaces(file.spaces);
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    m
}

/// Every title `state.json` would write for `bundle`.
fn saved_titles(m: &Model, bundle: &str) -> Vec<String> {
    let file = m.to_state();
    file.spaces.iter().flat_map(|s| &s.stages).flat_map(|s| &s.windows).filter(|x| x.bundle_id == bundle).map(|x| x.title.clone()).collect()
}

#[test]
fn a_saved_window_whose_app_was_not_reopened_is_given_up_when_the_restore_window_ends() {
    // The gap of round missed-quit (item 3), closed: after a restart, a saved window whose app
    // macOS did not reopen waited all session, and the app launched an hour later went back
    // into that stage and switched there, over `[stages] new_apps_join_current`. Now its entry
    // is given up when the restore window ends, and the late launch goes by the new-window
    // rules.
    let mut m = restarted_with_home();
    // Every app but the home app reopens.
    add(&mut m, win(51, 50, EDITOR, "Plan.txt", F_PLAN));
    add(&mut m, win(52, 50, EDITOR, "Notes.txt", F_NOTES));
    add(&mut m, win(61, 60, CHAT, "Team", F_TEAM));
    add(&mut m, win(71, 70, BROWSER, "Recipes — Example Browser", F_RECIPES));
    add(&mut m, win(81, 80, MAIL, "Inbox", F_INBOX));
    let stages = m.stages().len();
    m.end_restore();
    assert_eq!(m.pending_bundles(), [HOME_APP], "it waits until the give-up");
    let (actions, apps, dropped) = m.give_up_absent(|b| b != HOME_APP);
    assert_eq!((apps, dropped), (vec![HOME_APP.to_string()], 1), "the home app's one entry");
    assert!(actions.contains(&Action::StateChanged), "saved");
    assert_eq!(m.pending_count(), 0);
    assert_eq!(m.stages().len(), stages - 1, "its stage, which only held it, went");
    assert!(saved_titles(&m, HOME_APP).is_empty(), "a save after the drop no longer lists it");
    // A second look finds nothing more.
    assert_eq!(m.give_up_absent(|b| b != HOME_APP).2, 0);

    // The owner opens the home app an hour later: the key's rule, the current stage.
    let front = m.main_active_stage();
    add_joining(&mut m, win(92, 95, HOME_APP, "Home", F_HOME));
    assert_eq!(m.stage_of(w(92)), front, "joins the current stage");
    assert_eq!(m.main_active_stage(), front, "no switch");
    assert!(m.restored_grade(w(92)).is_none(), "nothing restored");
}

#[test]
fn a_running_app_keeps_its_saved_windows_at_the_restore_windows_end() {
    // Mail runs with its window not shown yet (hidden, or still opening): its entry is not
    // given up; its window, when it shows after the restore window, goes back to its stage as
    // before (Apple's rule: its stage comes forward).
    let mut m = restarted_with_home();
    add(&mut m, win(51, 50, EDITOR, "Plan.txt", F_PLAN));
    add(&mut m, win(52, 50, EDITOR, "Notes.txt", F_NOTES));
    add(&mut m, win(61, 60, CHAT, "Team", F_TEAM));
    add(&mut m, win(71, 70, BROWSER, "Recipes — Example Browser", F_RECIPES));
    m.end_restore();
    let running = |b: &str| b != HOME_APP;
    let (_, apps, dropped) = m.give_up_absent(running);
    assert_eq!((apps, dropped), (vec![HOME_APP.to_string()], 1), "only the app not running");
    assert_eq!(m.pending_bundles(), [MAIL], "Mail, running, keeps its entry");
    assert_eq!(saved_titles(&m, MAIL), ["Inbox"], "and its save");
    add(&mut m, win(81, 80, MAIL, "Inbox", F_INBOX));
    assert!(m.restored_grade(w(81)).is_some_and(|g| g.confident()), "back in its saved stage");
    assert_eq!(m.main_active_stage(), m.stage_of(w(81)), "its stage came forward");
}

#[test]
fn an_app_launched_in_the_restore_window_keeps_its_minute() {
    // The editor launched during the restore window and is still opening its windows when it
    // ends: Plan.txt is back, Notes.txt not yet. Running, it keeps Notes.txt's entry; its own
    // quiet minute gives it up (`AppQuiet`), as before.
    let mut m = restarted_with_home();
    add(&mut m, win(51, 50, EDITOR, "Plan.txt", F_PLAN));
    m.end_restore();
    let running = |b: &str| b == EDITOR;
    let (_, mut apps, _) = m.give_up_absent(running);
    apps.sort();
    let mut absent = vec![CHAT, BROWSER, MAIL, HOME_APP];
    absent.sort();
    assert_eq!(apps, absent, "every app not running");
    assert_eq!(m.pending_bundles(), [EDITOR], "the editor keeps Notes.txt");
    let t0 = std::time::Instant::now();
    let mut quiet = crate::engine::restore::AppQuiet::default();
    quiet.window(EDITOR, t0);
    assert!(quiet.settled(t0 + crate::engine::restore::QUIET / 2).is_empty(), "within its minute");
    let settled = quiet.settled(t0 + crate::engine::restore::QUIET);
    assert_eq!(settled, [EDITOR]);
    let (_, dropped) = m.expire_pending(&settled);
    assert_eq!(dropped, 1, "its minute up: Notes.txt given up");
    assert_eq!(m.pending_count(), 0);
}

/// W2 (the Fable review of 10-29): Chat saves its own window frames. Its window was parked
/// when it quit, so it wrote the park frame, and an hour later it reopens the window in the
/// corner. The window takes the frame it had at the quit (by app and title, else the app's
/// only one left), not the center; once, and not past a day.
#[test]
fn an_app_reopening_its_window_in_the_corner_gets_its_frame_back_within_a_day() {
    let corner = park_frame(crate::engine::geometry::Size::new(F_TEAM.w, F_TEAM.h), FRAME);
    let mut m = desk();
    assert!(m.window(w(21)).is_some_and(|x| x.parked));
    quit_app(&mut m, &[21], T);
    m.prune_quit(T + 3600); // the saves of the hour since
    add(&mut m, win(51, 22, CHAT, "Team", corner));
    assert_eq!(m.window(w(51)).map(|x| x.restore), Some(F_TEAM), "its frame at the quit");
    // Used once: another window born there is centered (its size kept).
    add(&mut m, win(52, 22, CHAT, "Team", corner));
    assert!(m.window(w(52)).is_some_and(|x| x.restore != F_TEAM && !parked_at(x.restore)));
    // Retitled: the app's only frame left still fits it.
    let mut m = desk();
    quit_app(&mut m, &[21], T);
    add(&mut m, win(51, 22, CHAT, "Team — 3 unread", corner));
    assert_eq!(m.window(w(51)).map(|x| x.restore), Some(F_TEAM));
    // A day later it is gone; a window born out of the zone never takes it.
    let mut m = desk();
    quit_app(&mut m, &[21], T);
    m.prune_quit(T + 24 * 3600 + 1);
    add(&mut m, win(51, 22, CHAT, "Team", corner));
    assert!(m.window(w(51)).is_some_and(|x| x.restore != F_TEAM && !parked_at(x.restore)), "centered");
    let mut m = desk();
    quit_app(&mut m, &[21], T);
    let elsewhere = Rect::new(300.0, 200.0, 700.0, 500.0);
    add(&mut m, win(51, 22, CHAT, "Team", elsewhere));
    assert_eq!(m.window(w(51)).map(|x| x.restore), Some(elsewhere), "where its app put it");
}
