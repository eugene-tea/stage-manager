//! The pool after a restart (round 8I). Made-up titles only.
//!
//! The 10-16 restart, as the owner explained it: native apps reopen their windows on their own
//! saved Desktops; non-native ones (Electron and the like) on the Desktop shown at login. He
//! restarted on Desktop 1 (saved 4, the larger) and the login showed Desktop 2 (saved 8).

use super::*;
use crate::engine::geometry::Rect;
use crate::engine::model::{Model, ModelOptions, Observation, Pid, WindowRole};
use crate::engine::persist::{LaunchHints, SavedStage, StateFile};
use crate::engine::restore::Restore;
use crate::engine::rules::Decision;
use crate::engine::testkit::*;

/// Electron-like apps: their windows come back on the Desktop shown.
const IDE: &str = "com.example.ide";
const TEAMCHAT: &str = "com.example.teamchat";
const GIT: &str = "com.example.git";
/// Native apps: back on their own Desktop.
const TERM: &str = "com.example.term";
const MAIL: &str = "com.example.mail";
const MUSIC: &str = "com.example.music";
const CAL: &str = "com.example.cal";
const WEB: &str = "com.example.web";

fn r(i: f64) -> Rect {
    Rect::new(60.0 + 37.0 * i, 40.0 + 23.0 * i, 500.0 + 11.0 * i, 360.0 + 7.0 * i)
}

fn win(id: u32, pid: Pid, bundle: &str, title: &str, frame: Rect) -> WindowMeta {
    WindowMeta { title: title.into(), frame, ..meta(id, pid, bundle) }
}

fn sw(bundle: &str, title: &str, frame: Rect, z: usize) -> SavedWindow {
    SavedWindow { bundle_id: bundle.into(), title: title.into(), frame, z, key: z == 0, ..Default::default() }
}

fn stage(windows: Vec<SavedWindow>, last_active: u64) -> SavedStage {
    SavedStage { name: None, windows, last_active: Some(last_active) }
}

/// Desktop 1 (saved 4, shown when saved): the IDE's two windows with a terminal (active), the
/// team chat, music, two git windows, mail. Strip: chat, git, music, mail.
fn desktop_1() -> SavedSpace {
    SavedSpace {
        screen: None,
        id: 4,
        current: true,
        stages: vec![
            stage(vec![sw(IDE, "main.rs — proj", r(0.0), 0), sw(IDE, "lib.rs — proj", r(1.0), 1), sw(TERM, "build", r(2.0), 2)], 90),
            stage(vec![sw(TEAMCHAT, "General", r(3.0), 0)], 80),
            stage(vec![sw(MUSIC, "Library", r(4.0), 0)], 60),
            stage(vec![sw(GIT, "proj", r(5.0), 0), sw(GIT, "site", r(6.0), 1)], 70),
            stage(vec![sw(MAIL, "Inbox", r(7.0), 0)], 50),
        ],
        active_index: Some(0),
        strip_slots: vec![1, 3, 2, 4],
        last_seen: None,
        unvisited: false,
    }
}

/// Desktop 2 (saved 8): mail's archive (active), a terminal, the calendar, the web browser.
/// Strip: calendar, terminal, web.
fn desktop_2() -> SavedSpace {
    SavedSpace {
        screen: None,
        id: 8,
        current: false,
        stages: vec![
            stage(vec![sw(MAIL, "Archive", r(8.0), 0)], 40),
            stage(vec![sw(TERM, "server", r(9.0), 0)], 30),
            stage(vec![sw(CAL, "Week", r(10.0), 0)], 35),
            stage(vec![sw(WEB, "Docs — Web", r(11.0), 0)], 20),
        ],
        active_index: Some(0),
        strip_slots: vec![2, 1, 3],
        last_seen: None,
        unvisited: false,
    }
}

/// A launch in session "B" of a file written in session "A" (`restore`: the mode).
fn launched(spaces: Vec<SavedSpace>, restore: Restore) -> Model {
    let mut file = StateFile { session: Some("A".into()), ..StateFile::single(SavedSpace::default()) };
    file.spaces = spaces;
    if restore.new_session {
        file.start_new_session();
    }
    let (file, _) = file.for_session(Some(if restore.new_session { "B" } else { "A" }));
    let mut m = model();
    m.set_restore(restore);
    m.restore_spaces(file.spaces);
    m
}

fn add(m: &mut Model, x: WindowMeta) -> Vec<Action> {
    m.add_window(x, &Decision::default(), &Observation::default())
}

fn parks(actions: &[Action], id: u32) -> bool {
    actions.iter().any(|a| matches!(a, Action::Park { id: x, .. } if x.0 == id))
}

fn kin(m: &Model, id: u32) -> Option<Kin> {
    m.stage_of(w(id)).map(|s| m.root_of(s))
}

/// Every title in `state.json` with how many times it is written (live or waiting).
fn titles(m: &Model) -> Vec<(String, usize)> {
    let file = m.to_state();
    let mut all: Vec<String> = file.spaces.iter().flat_map(|s| &s.stages).flat_map(|s| &s.windows).map(|w| w.title.clone()).collect();
    all.sort();
    let mut out: Vec<(String, usize)> = Vec::new();
    for t in all {
        match out.last_mut() {
            Some((last, n)) if *last == t => *n += 1,
            _ => out.push((t, 1)),
        }
    }
    out
}

/// The login on Desktop 2: two of its native windows adopt it, then the strays of Desktop 1
/// and Desktop 2's last natives come, interleaved as apps reopen.
fn login_on_desktop_2() -> Model {
    let mut m = launched(vec![desktop_1(), desktop_2()], Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    assert!(m.pooling());
    add(&mut m, win(81, 10, CAL, "Week", r(10.0)));
    assert_ne!(m.current_space(MAIN).0, 8, "one window of a Desktop not shown when saved (M2)");
    assert_eq!(m.restored_grade(w(81)), None, "a voter waits by Apple's rule, as before");
    add(&mut m, win(82, 11, TERM, "server", r(9.0)));
    assert_eq!(m.current_space(MAIN).0, 8, "the second adopts it");
    // Strays of Desktop 1, then Desktop 2's last natives.
    add(&mut m, win(41, 20, IDE, "main.rs — proj", r(0.0)));
    add(&mut m, win(42, 20, IDE, "lib.rs — proj", r(1.0)));
    add(&mut m, win(43, 21, TEAMCHAT, "General", r(3.0)));
    add(&mut m, win(83, 12, MAIL, "Archive", r(8.0)));
    add(&mut m, win(44, 22, GIT, "proj", r(5.0)));
    add(&mut m, win(45, 22, GIT, "site", r(6.0)));
    add(&mut m, win(84, 13, WEB, "Docs — Web", r(11.0)));
    m
}

#[test]
fn strays_of_another_saved_desktop_go_back_to_their_saved_stages() {
    let mut m = login_on_desktop_2();
    let here = m.current_space(MAIN);
    // Desktop 2's own windows in its own stages.
    for (id, k) in [(83, 0), (82, 1), (81, 2), (84, 3)] {
        assert_eq!(kin(&m, id), Some(Kin::Saved(8, k)), "window {id}");
        assert!(!m.is_pooled(w(id)));
    }
    // Desktop 1's strays, each in its saved stage, made here.
    assert_eq!(kin(&m, 41), Some(Kin::Saved(4, 0)));
    assert_eq!(m.stage_of(w(41)), m.stage_of(w(42)), "the IDE's two windows together");
    assert_eq!(kin(&m, 43), Some(Kin::Saved(4, 1)));
    assert_eq!(m.stage_of(w(44)), m.stage_of(w(45)));
    assert_eq!(kin(&m, 44), Some(Kin::Saved(4, 3)));
    for id in [41, 42, 43, 44, 45] {
        assert!(m.is_pooled(w(id)) && m.restored_grade(w(id)).is_some_and(Grade::confident), "window {id}");
        assert_eq!(m.window(w(id)).map(|x| x.space), Some(here));
    }
    assert_eq!(m.stages_in(here).count(), 7, "Desktop 2's four and three of Desktop 1's");
    // The IDE's part took the empty front (Desktop 2's saved active stage still waited then);
    // Mail's archive went back to its stage, parked.
    assert_eq!(m.main_active_stage(), m.stage_of(w(41)));
    assert!(m.window(w(83)).is_some_and(|x| x.parked));
    // Strip: Desktop 2's stages first, its saved slots in their saved order (calendar,
    // terminal, web; the archive's stage, active when saved and so with no saved slot, where
    // its card came), then Desktop 1's stages in its saved order (chat, git), whatever order
    // their windows came in.
    let order: Vec<Option<Kin>> = m.strip().iter().map(|c| Some(m.root_of(c.stage))).collect();
    assert_eq!(
        order,
        [Kin::Saved(8, 2), Kin::Saved(8, 1), Kin::Saved(8, 0), Kin::Saved(8, 3), Kin::Saved(4, 1), Kin::Saved(4, 3)].map(Some)
    );
    // No window both live and waiting: Desktop 1 keeps only its natives, for its visit.
    assert!(titles(&m).iter().all(|(_, n)| *n == 1), "{:?}", titles(&m));
    assert_eq!(m.pending_unvisited(), 3, "the terminal's build, music, mail's inbox");
    assert_eq!(m.pending_count(), 0);
    let file = m.to_state();
    let d1 = file.spaces.iter().find(|s| s.id == 4).expect("Desktop 1 still waits");
    assert_eq!(d1.stages.len(), 3, "the stages emptied by strays are not written");
    assert_eq!(d1.active_index, Some(0), "its active stage keeps the terminal");
    // The restore's line.
    m.end_restore();
    let line = m.restore_summary(28);
    assert_eq!(line.pooled, 5);
    assert_eq!(
        line.to_string(),
        "restore: 9 windows into 7 stages on 1 Desktop in 28 s; 3 pending; 1 by Apple's rule; 0 at the corner; 5 from other Desktops"
    );
    // (The one by Apple's rule: the calendar, counted while its vote waited, as before.)
    assert_eq!(m.unvisited_saved_spaces(), 1, "the restore window's end drops no waiting window");
    // Desktop 1 is not the one shown: its saved active stage never takes this front.
    assert!(m.front_of_saved_current(&Observation::default()).is_empty());
}

#[test]
fn a_stage_split_across_two_desktops_makes_its_part_on_each() {
    let mut m = login_on_desktop_2();
    let d2 = m.current_space(MAIN);
    m.end_restore();
    // He goes to Desktop 1: its natives are there.
    let natives = [win(51, 30, TERM, "build", r(2.0)), win(52, 31, MUSIC, "Library", r(4.0)), win(53, 12, MAIL, "Inbox", r(7.0))];
    let shown: Vec<WindowId> = m.windows().filter(|x| x.space == d2).map(|x| x.meta.id).collect();
    m.arrive(MAIN, &natives, &shown, &LaunchHints::default());
    let d1 = m.current_space(MAIN);
    assert_eq!(d1.0, 4, "Desktop 1 by its own windows");
    assert_eq!(m.unvisited_saved_spaces(), 0);
    // The IDE's stage: its terminal here, the IDE's windows on Desktop 2, each a part.
    assert_eq!(kin(&m, 51), Some(Kin::Saved(4, 0)));
    assert_eq!(kin(&m, 41), Some(Kin::Saved(4, 0)));
    assert_eq!(m.window(w(51)).map(|x| x.space), Some(d1));
    assert_eq!(m.window(w(41)).map(|x| x.space), Some(d2));
    assert_eq!(m.main_active_stage(), m.stage_of(w(51)), "Desktop 1's saved active stage");
    assert!(!m.is_pooled(w(51)), "on its own Desktop");
    assert_eq!(m.stages_in(d1).count(), 3, "build, music, inbox; no empty stage for the strays' stages");
    assert_eq!(m.pending_count() + m.pending_unvisited(), 0, "nothing waits: every app reopened");
    assert!(titles(&m).iter().all(|(_, n)| *n == 1), "{:?}", titles(&m));
    // A window waiting in a stage of Desktop 1 that shows on Desktop 2: its part there.
    let mut m = login_on_desktop_2();
    m.remove_window(w(83));
    m.end_restore();
    let shown: Vec<WindowId> = m.windows().filter(|x| x.space == d2).map(|x| x.meta.id).collect();
    m.arrive(MAIN, &natives[..2], &shown, &LaunchHints::default());
    assert_eq!(m.current_space(MAIN).0, 4);
    let inbox = m.stages_in(SpaceId(4)).find(|s| m.has_pending(s.id)).map(|s| s.id).expect("the inbox waits on Desktop 1");
    // Back on Desktop 2 the inbox shows there (macOS put it on the Desktop shown).
    let d1_windows: Vec<WindowId> = m.windows().filter(|x| x.space == SpaceId(4)).map(|x| x.meta.id).collect();
    let back: Vec<WindowMeta> = m.windows().filter(|x| x.space == d2).map(|x| x.meta.clone()).collect();
    m.arrive(MAIN, &back, &d1_windows, &LaunchHints::default());
    assert_eq!(m.current_space(MAIN), d2);
    add(&mut m, win(54, 12, MAIL, "Inbox", r(7.0)));
    assert_eq!(kin(&m, 54), Some(Kin::Saved(4, 4)));
    assert_eq!(m.window(w(54)).map(|x| x.space), Some(d2));
    assert!(m.stage(inbox).is_none(), "the stage waiting on Desktop 1 had only it: gone");
    assert!(titles(&m).iter().all(|(_, n)| *n == 1), "{:?}", titles(&m));
}

#[test]
fn a_saved_desktops_windows_of_apps_not_running_at_the_end_are_given_up() {
    // Desktop 1, not visited, waits for the terminal's build, music and mail's inbox. Music is
    // not running when the restore window ends: its entry goes, and its stage with it.
    let mut m = login_on_desktop_2();
    let d2 = m.current_space(MAIN);
    m.end_restore();
    assert_eq!(m.pending_unvisited(), 3);
    let (actions, apps, dropped) = m.give_up_absent(|b| b != MUSIC);
    assert_eq!((apps, dropped), (vec![MUSIC.to_string()], 1));
    assert!(actions.contains(&Action::StateChanged));
    assert_eq!(m.pending_unvisited(), 2);
    assert!(titles(&m).iter().all(|(t, _)| t != "Library"), "no save lists it");
    let file = m.to_state();
    assert_eq!(file.spaces.iter().find(|s| s.id == 4).map(|d| d.stages.len()), Some(2), "build's and inbox's stages");
    // Music launched later: a stage of its own by Apple's rule, nothing restored.
    add(&mut m, win(55, 32, MUSIC, "Library", r(4.0)));
    assert!(m.restored_grade(w(55)).is_none() && !m.is_pooled(w(55)));
    assert_eq!(m.window(w(55)).map(|x| x.space), Some(d2));
    // He goes to Desktop 1: no stage is built for the one given up.
    let natives = [win(51, 30, TERM, "build", r(2.0)), win(53, 12, MAIL, "Inbox", r(7.0))];
    let shown: Vec<WindowId> = m.windows().filter(|x| x.space == d2).map(|x| x.meta.id).collect();
    m.arrive(MAIN, &natives, &shown, &LaunchHints::default());
    assert_eq!(m.current_space(MAIN).0, 4);
    assert_eq!(m.stages_in(SpaceId(4)).count(), 2, "build and inbox");

    // None of its apps running (Mail quit with its archive): the saved Desktop goes whole.
    let mut m = login_on_desktop_2();
    m.remove_window(w(83));
    m.end_restore();
    let (_, _, dropped) = m.give_up_absent(|b| ![TERM, MUSIC, MAIL].contains(&b));
    assert_eq!(dropped, 3);
    assert_eq!(m.unvisited_saved_spaces(), 0);
    assert!(m.to_state().spaces.iter().all(|s| s.id != 4), "no save lists it");
}

#[test]
fn a_quiet_apps_windows_of_a_desktop_not_shown_wait_for_that_desktop_alone() {
    // R3 of the review: Desktop 1, not visited, waits for mail's inbox; Mail runs (its
    // archive back on Desktop 2) and opens nothing for a minute. Its next window "Inbox" an
    // hour later, on Desktop 2, is a new window by Apple's rule, not one of Desktop 1's.
    let mut m = login_on_desktop_2();
    let d2 = m.current_space(MAIN);
    m.end_restore();
    assert!(m.take_shown_apps().contains(&CAL.to_string()), "Desktop 2 adopted at the login");
    // Before its minute the pool takes it (a stray of Desktop 1, as ever)...
    let mut early = m.clone();
    add(&mut early, win(56, 12, MAIL, "Inbox", r(7.0)));
    assert!(early.is_pooled(w(56)), "a stray before the minute");
    // ...and the minute's give-up of the model's own stages alone (as before) leaves it so.
    let mut expired_only = m.clone();
    expired_only.expire_pending(&[MAIL.to_string()]);
    add(&mut expired_only, win(56, 12, MAIL, "Inbox", r(7.0)));
    assert!(expired_only.is_pooled(w(56)), "the bug: Desktop 1's entry still takes it");
    // The minute closes Desktop 1's entries of Mail to other Desktops.
    let (_, dropped, closed) = m.settle_quiet(&[MAIL.to_string(), WEB.to_string()]);
    assert_eq!((dropped, closed), (0, vec![MAIL.to_string()]), "nothing of the model's stages waits; only Mail waits on Desktop 1");
    assert_eq!(m.pending_unvisited(), 3, "the entry stays, for Desktop 1");
    add(&mut m, win(56, 12, MAIL, "Inbox", r(7.0)));
    assert!(!m.is_pooled(w(56)) && m.restored_grade(w(56)).is_none(), "by Apple's rule");
    assert_eq!(m.window(w(56)).map(|x| x.space), Some(d2));
    assert_ne!(kin(&m, 56), Some(Kin::Saved(4, 4)));
    assert!(m.take_shown_apps().is_empty(), "no saved Desktop shown since");
    // He goes to Desktop 1: its inbox, shown there, goes to its saved stage all the same.
    let natives = [win(51, 30, TERM, "build", r(2.0)), win(53, 12, MAIL, "Inbox", r(7.0))];
    let shown: Vec<WindowId> = m.windows().filter(|x| x.space == d2).map(|x| x.meta.id).collect();
    m.arrive(MAIN, &natives, &shown, &LaunchHints::default());
    assert_eq!(m.current_space(MAIN).0, 4);
    assert!(m.restored_grade(w(53)).is_some_and(Grade::confident), "its saved window");
    assert_ne!(m.stage_of(w(53)), m.stage_of(w(51)));
    // Its apps' minutes start again there, for the windows of Desktop 1 still to show.
    let mut apps = m.take_shown_apps();
    apps.sort();
    assert_eq!(apps, [MAIL, MUSIC, TERM].map(String::from).to_vec());
    assert!(m.take_shown_apps().is_empty());
}

/// Desktops 1 and 2 saved in session "A" with their windows' ids (100 on): a relaunch in the
/// same session finds each window by its id.
fn with_ids() -> (SavedSpace, SavedSpace) {
    let mut d1 = desktop_1();
    let mut d2 = desktop_2();
    for (i, x) in d1.stages.iter_mut().chain(d2.stages.iter_mut()).flat_map(|s| s.windows.iter_mut()).enumerate() {
        x.window = Some(100 + i as u32);
    }
    (d1, d2)
}

/// Desktop 1's windows as macOS lists them at its first show (ids 100 to 107), some titles
/// changed since the save.
fn desktop_1_live() -> Vec<WindowMeta> {
    vec![
        win(100, 20, IDE, "other.rs — proj", r(0.0)),
        win(101, 20, IDE, "lib.rs — proj", r(1.0)),
        win(102, 30, TERM, "build — 2", r(2.0)),
        win(103, 21, TEAMCHAT, "General (3)", r(3.0)),
        win(104, 31, MUSIC, "Library", r(4.0)),
        win(105, 22, GIT, "proj", r(5.0)),
        win(106, 22, GIT, "site", r(6.0)),
        win(107, 12, MAIL, "Inbox", r(7.0)),
    ]
}

/// Desktop 1 rebuilt as saved: the IDE's two windows with the terminal (in front), the git
/// windows together, each window its saved entry's (by its id).
fn assert_desktop_1_rebuilt(m: &Model) {
    assert_eq!(m.current_space(MAIN).0, 4);
    assert_eq!((m.last_arrival().rebuilt, m.last_arrival().restored), (Some(4), 8), "{:?}", m.last_arrival());
    for id in 100..=107 {
        assert!(m.restored_grade(w(id)).is_some_and(Grade::confident), "window {id} its saved entry's: {:?}", m.restored_grade(w(id)));
        assert_eq!(m.window(w(id)).map(|x| x.space.0), Some(4));
    }
    assert_eq!(kin(m, 100), Some(Kin::Saved(4, 0)));
    assert_eq!(m.stage_of(w(100)), m.stage_of(w(101)), "the IDE's two windows together");
    assert_eq!(m.stage_of(w(100)), m.stage_of(w(102)), "with the terminal");
    assert_eq!(m.stage_of(w(105)), m.stage_of(w(106)), "the git windows together");
    assert_eq!(m.stages_in(SpaceId(4)).count(), 5, "the five saved stages, no stage per window");
    assert_eq!(m.main_active_stage(), m.stage_of(w(100)), "the saved front in front");
    assert_eq!(m.pending_unvisited(), 0);
}

/// A relaunch in session "A" on Desktop 2 (its windows by their ids), the restore window
/// over, every app quiet for its minute (R3: Desktop 1's entries closed to other Desktops),
/// the re-match by titles over: an hour later Desktop 1 has not been shown yet.
fn relaunched_on_desktop_2_an_hour_ago() -> Model {
    let (d1, d2) = with_ids();
    let mut m = launched(vec![d1, d2], Restore::same_session("A"));
    let live = [win(108, 12, MAIL, "Archive", r(8.0)), win(109, 11, TERM, "server", r(9.0)), win(110, 10, CAL, "Week", r(10.0)), win(111, 13, WEB, "Docs — Web", r(11.0))];
    m.arrive(MAIN, &live, &[], &LaunchHints::default());
    assert_eq!(m.current_space(MAIN).0, 8);
    m.end_restore();
    m.take_shown_apps();
    let quiet: Vec<String> = m.waiting_bundles();
    let (_, dropped, closed) = m.settle_quiet(&quiet);
    assert_eq!(dropped, 0);
    assert_eq!(closed.len(), 6, "every app of Desktop 1 closed to other Desktops: {closed:?}");
    m.end_rematch();
    assert_eq!(m.pending_unvisited(), 8, "Desktop 1 keeps every saved window");
    m
}

#[test]
fn a_desktop_first_shown_an_hour_after_a_relaunch_gets_its_saved_stages_back() {
    // The 11-06 regression report: relaunched on Desktop 2 at 11:34 (the same login
    // session), Desktop 1 first shown at 12:00 by a slide. Its saved stages come back
    // however long after the launch, the quiet minute's R3 closing notwithstanding.
    let mut m = relaunched_on_desktop_2_an_hour_ago();
    let d2: Vec<WindowId> = m.windows().filter(|x| x.space.0 == 8).map(|x| x.meta.id).collect();
    m.arrive(MAIN, &desktop_1_live(), &d2, &LaunchHints::default());
    assert_desktop_1_rebuilt(&m);
    assert!(m.last_arrival().held.is_empty());
    let mut apps = m.take_shown_apps();
    apps.sort();
    assert_eq!(apps.len(), 6, "their minutes start again: {apps:?}");
    // R3 holds all the same: a new mail window on Desktop 2 later is a new window.
    let mut m = relaunched_on_desktop_2_an_hour_ago();
    add(&mut m, win(56, 12, MAIL, "Inbox", r(7.0)));
    assert!(!m.is_pooled(w(56)) && m.restored_grade(w(56)).is_none(), "by Apple's rule");
    assert_eq!(m.window(w(56)).map(|x| x.space.0), Some(8));
    assert_eq!(m.pending_unvisited(), 8);
    let d2: Vec<WindowId> = m.windows().filter(|x| x.space.0 == 8).map(|x| x.meta.id).collect();
    m.arrive(MAIN, &desktop_1_live(), &d2, &LaunchHints::default());
    assert_desktop_1_rebuilt(&m);
}

#[test]
fn a_desktop_first_shown_during_a_display_changes_settle_gets_its_saved_stages_back() {
    // The lid closed at 12:00:37 and macOS slid to Desktop 1 within the display change's
    // settle (arrivals of the Desktop still shown held): a Desktop that changed takes its
    // windows, its saved stages rebuilt.
    let mut m = relaunched_on_desktop_2_an_hour_ago();
    let d2: Vec<WindowId> = m.windows().filter(|x| x.space.0 == 8).map(|x| x.meta.id).collect();
    m.set_hold_arrivals(true);
    m.arrive(MAIN, &desktop_1_live(), &d2, &LaunchHints::default());
    m.set_hold_arrivals(false);
    assert!(m.last_arrival().held.is_empty(), "{:?}", m.last_arrival());
    assert_desktop_1_rebuilt(&m);
}

#[test]
fn the_desktop_shown_when_saved_brings_its_saved_active_stage_forward_at_the_end() {
    // The login shows Desktop 1 itself. An app with no saved window opens first and takes the
    // empty front; Desktop 1's terminal (in its saved active stage) then adopts it, parked
    // behind the front that shows a window; a stray of Desktop 2 goes to its part. At the end
    // the saved active stage comes forward.
    let mut m = launched(vec![desktop_1(), desktop_2()], Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(70, 40, "com.example.notes", "List", r(13.0)));
    let front = m.main_active_stage();
    add(&mut m, win(51, 30, TERM, "build", r(2.0)));
    assert_ne!(m.current_space(MAIN).0, 4, "with Desktop 2 waiting too, one window is no proof (M3 of the review)");
    add(&mut m, win(42, 20, IDE, "lib.rs — proj", r(1.0)));
    assert_eq!(m.current_space(MAIN).0, 4);
    assert_eq!(m.main_active_stage(), front, "a front that shows a window stays while settling");
    assert!(m.window(w(51)).is_some_and(|x| x.parked));
    add(&mut m, win(84, 13, WEB, "Docs — Web", r(11.0)));
    assert!(m.is_pooled(w(84)) && kin(&m, 84) == Some(Kin::Saved(8, 3)));
    m.end_restore();
    let a = m.front_of_saved_current(&Observation::default());
    assert!(a.iter().any(|x| matches!(x, Action::Focus { id } if *id == w(51))), "{a:?}");
    assert_eq!(m.main_active_stage(), m.stage_of(w(51)));
    assert!(m.front_of_saved_current(&Observation::default()).is_empty(), "once");
    // None of its windows back: nothing.
    let mut m = launched(vec![desktop_1(), desktop_2()], Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(70, 40, "com.example.notes", "List", r(13.0)));
    add(&mut m, win(52, 31, MUSIC, "Library", r(4.0)));
    add(&mut m, win(53, 12, MAIL, "Inbox", r(7.0)));
    assert_eq!(m.current_space(MAIN).0, 4);
    m.end_restore();
    assert!(m.front_of_saved_current(&Observation::default()).is_empty());
}

#[test]
fn a_relaunch_in_the_same_session_keeps_each_desktop_to_itself() {
    let mut d1 = desktop_1();
    let mut d2 = desktop_2();
    for (i, w) in d1.stages.iter_mut().chain(d2.stages.iter_mut()).flat_map(|s| s.windows.iter_mut()).enumerate() {
        w.window = Some(100 + i as u32);
    }
    let ide = d1.stages[0].windows[0].clone();
    let mut m = launched(vec![d1, d2], Restore::same_session("A"));
    assert!(!m.pooling());
    m.arrive(MAIN, &[win(110, 10, CAL, "Week", r(10.0)), win(109, 11, TERM, "server", r(9.0))], &[], &LaunchHints::default());
    assert_eq!(m.current_space(MAIN).0, 8);
    // A window of Desktop 1 on Desktop 2 in the same session: Apple's rule, as before.
    add(&mut m, win(ide.window.unwrap_or_default() + 50, 20, IDE, "main.rs — proj", r(0.0)));
    assert_eq!(m.restored_grade(w(150)), None);
    assert!(!m.is_pooled(w(150)));
    assert_eq!(m.pending_unvisited(), 8, "Desktop 1 keeps every saved window");
}

#[test]
fn strays_listed_at_an_arrival_go_to_their_saved_stages_with_their_children() {
    let mut m = launched(vec![desktop_1(), desktop_2()], Restore::after_restart("B"));
    // The login's first list already holds Desktop 2's windows and a stray with a sheet.
    let sheet = WindowMeta { role: WindowRole::Child { parent: Some(w(43)) }, ..win(90, 21, TEAMCHAT, "Save", r(12.0)) };
    let live = [win(81, 10, CAL, "Week", r(10.0)), win(82, 11, TERM, "server", r(9.0)), win(43, 21, TEAMCHAT, "General", r(3.0)), sheet];
    m.arrive(MAIN, &live, &[], &LaunchHints::default());
    assert_eq!(m.current_space(MAIN).0, 8);
    assert_eq!(kin(&m, 43), Some(Kin::Saved(4, 1)));
    assert!(m.is_pooled(w(43)) && m.window(w(43)).is_some_and(|x| x.parked), "parked in its part (inactive)");
    assert!(m.window(w(90)).is_some_and(|x| x.parked && x.home == m.window(w(43)).map(|p| p.home).unwrap_or(Home::Pinned)), "the sheet follows it");
    assert_eq!(m.restore_summary(0).by_apple, 0);
    assert!(titles(&m).iter().filter(|(t, _)| t == "General").all(|(_, n)| *n == 1));
}

// ----- the review of 8I (8I2) -----------------------------------------------------------

/// Desktop 1 with its saved active stage all strays: the IDE's two windows only (the
/// terminal's "build" in a stage of its own, used after the git stage).
fn desktop_1_ide_front() -> SavedSpace {
    let mut d1 = desktop_1();
    d1.stages[0].windows.retain(|x| x.bundle_id == IDE);
    d1.stages.push(stage(vec![sw(TERM, "build", r(2.0), 0)], 75));
    d1.strip_slots.push(5);
    d1
}

/// The login on Desktop 2 (adopted by two of its natives) with every stray of `d1` there.
fn login_with(d1: SavedSpace) -> Model {
    let mut m = launched(vec![d1, desktop_2()], Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(81, 10, CAL, "Week", r(10.0)));
    add(&mut m, win(82, 11, TERM, "server", r(9.0)));
    assert_eq!(m.current_space(MAIN).0, 8);
    for x in [
        win(41, 20, IDE, "main.rs — proj", r(0.0)),
        win(42, 20, IDE, "lib.rs — proj", r(1.0)),
        win(43, 21, TEAMCHAT, "General", r(3.0)),
        win(44, 22, GIT, "proj", r(5.0)),
        win(45, 22, GIT, "site", r(6.0)),
    ] {
        add(&mut m, x);
    }
    m
}

#[test]
fn a_visit_to_a_desktop_whose_active_stage_all_strayed_shows_its_most_recent_stage() {
    // M1: Desktop 1's saved active stage (the IDE's) came back whole on Desktop 2. At Desktop
    // 1's visit no empty stage is built for it, nor made active with the natives parked
    // beside it: the most recently used stage with windows is in front.
    let mut m = login_with(desktop_1_ide_front());
    let d2 = m.current_space(MAIN);
    let file = m.to_state();
    let saved = file.spaces.iter().find(|s| s.id == 4).expect("Desktop 1 waits");
    assert_eq!(saved.stages.len(), 3, "music, inbox, build: no stage with nothing in it is written");
    m.end_restore();
    let natives = [win(51, 30, TERM, "build", r(2.0)), win(52, 31, MUSIC, "Library", r(4.0)), win(53, 12, MAIL, "Inbox", r(7.0))];
    let shown: Vec<WindowId> = m.windows().filter(|x| x.space == d2).map(|x| x.meta.id).collect();
    let a = m.arrive(MAIN, &natives, &shown, &LaunchHints::default());
    let d1 = m.current_space(MAIN);
    assert_eq!(d1.0, 4);
    assert_eq!(m.stages_in(d1).count(), 3, "build, music, inbox; nothing for the strays' stages");
    assert_eq!(m.main_active_stage(), m.stage_of(w(51)), "build's stage, the most recent left: {a:?}");
    assert!(m.window(w(51)).is_some_and(|x| !x.parked));
    assert!(m.window(w(52)).is_some_and(|x| x.parked) && m.window(w(53)).is_some_and(|x| x.parked));
    assert!(m.stages_in(d1).all(|s| !s.windows.is_empty()), "no empty stage on Desktop 1");
    assert!(m.pooled_out.is_empty());
    assert!(titles(&m).iter().all(|(_, n)| *n == 1), "{:?}", titles(&m));
    // Only the natives of an older stage back yet: that one is in front.
    let mut m = login_with(desktop_1_ide_front());
    m.end_restore();
    let shown: Vec<WindowId> = m.windows().filter(|x| x.space == d2).map(|x| x.meta.id).collect();
    m.arrive(MAIN, &natives[2..], &shown, &LaunchHints::default());
    assert_eq!(m.current_space(MAIN).0, 4);
    let front = m.main_active_stage().expect("a front");
    assert_eq!(m.root_of(front), Kin::Saved(4, 5), "build's stage, the most recent with a window waiting");
    assert!(m.has_pending(front) && m.window(w(53)).is_some_and(|x| x.parked));
}

#[test]
fn a_lone_voter_joins_its_saved_stage_at_the_restore_windows_end() {
    // M2: one window of Desktop 2 on the Desktop shown at login and nothing more: a vote that
    // never adopts. At the restore window's end it can adopt nothing: it joins its saved
    // stage where it is, its entry leaves Desktop 2.
    let mut m = launched(vec![desktop_1(), desktop_2()], Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    let here = m.current_space(MAIN);
    add(&mut m, win(81, 10, CAL, "Week", r(10.0)));
    assert_eq!(m.restored_grade(w(81)), None, "a voter, by Apple's rule meanwhile");
    assert_eq!(titles(&m).iter().find(|(t, _)| t == "Week").map(|x| x.1), Some(2), "live and waiting (the finding)");
    let stage = m.stage_of(w(81));
    let a = m.end_restore().expect("it was open");
    assert!(!parks(&a, 81), "where it is: {a:?}");
    assert_eq!(m.stage_of(w(81)), stage, "the stage Apple's rule made is its part now");
    assert_eq!(kin(&m, 81), Some(Kin::Saved(8, 2)));
    assert!(m.is_pooled(w(81)) && m.window(w(81)).is_some_and(|x| x.space == here && !x.parked));
    assert!(titles(&m).iter().all(|(_, n)| *n == 1), "{:?}", titles(&m));
    // A voter with a sheet of its own stays where it is; its entry is forgotten.
    let mut m = launched(vec![desktop_1(), desktop_2()], Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(81, 10, CAL, "Week", r(10.0)));
    let sheet = WindowMeta { role: WindowRole::Child { parent: Some(w(81)) }, ..win(91, 10, CAL, "New Event", r(12.0)) };
    add(&mut m, sheet);
    let stage = m.stage_of(w(81));
    let a = m.end_restore().expect("it was open");
    assert!(a.is_empty(), "{a:?}");
    assert_eq!(m.stage_of(w(81)), stage);
    assert_eq!(m.restored_grade(w(81)), None, "by Apple's rule");
    assert!(titles(&m).iter().all(|(_, n)| *n == 1), "{:?}", titles(&m));
}

#[test]
fn a_lone_stray_of_the_desktop_shown_when_saved_does_not_take_the_login_desktop() {
    // M3, the 10-16 order: the team chat (Desktop 1's, the Desktop shown when saved) reopens
    // first on the Desktop shown at login, Desktop 2. One window of Desktop 1 no longer makes
    // it Desktop 1 while Desktop 2 waits too: Desktop 2's natives adopt it, and the chat, a
    // vote that can adopt nothing now, joins its saved stage here.
    let mut m = launched(vec![desktop_1(), desktop_2()], Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(43, 21, TEAMCHAT, "General", r(3.0)));
    assert_ne!(m.current_space(MAIN).0, 4, "one window is no proof");
    assert_eq!(m.restored_grade(w(43)), None);
    add(&mut m, win(81, 10, CAL, "Week", r(10.0)));
    add(&mut m, win(82, 11, TERM, "server", r(9.0)));
    assert_eq!(m.current_space(MAIN).0, 8, "Desktop 2 by its own windows");
    assert_eq!(kin(&m, 43), Some(Kin::Saved(4, 1)), "the chat in its saved stage, a part here");
    assert!(m.is_pooled(w(43)) && m.window(w(43)).is_some_and(|x| !x.parked), "where it shows");
    assert_eq!(kin(&m, 81), Some(Kin::Saved(8, 2)));
    for x in [win(41, 20, IDE, "main.rs — proj", r(0.0)), win(42, 20, IDE, "lib.rs — proj", r(1.0)), win(83, 12, MAIL, "Archive", r(8.0))] {
        add(&mut m, x);
    }
    assert_eq!(kin(&m, 41), Some(Kin::Saved(4, 0)));
    assert_eq!(kin(&m, 83), Some(Kin::Saved(8, 0)));
    assert!(titles(&m).iter().all(|(_, n)| *n == 1), "{:?}", titles(&m));
    assert_eq!(m.unvisited_saved_spaces(), 1, "Desktop 1 waits for its natives");
    // The same session is unchanged: there the Desktop shown when saved takes one window.
    let mut d1 = desktop_1();
    let mut d2 = desktop_2();
    for (i, x) in d1.stages.iter_mut().chain(d2.stages.iter_mut()).flat_map(|s| s.windows.iter_mut()).enumerate() {
        x.window = Some(100 + i as u32);
    }
    let m = launched(vec![d1, d2], Restore::same_session("A"));
    assert!(!m.pooling());
}

#[test]
fn a_rule_placed_stray_empties_its_saved_stage_like_any_other() {
    // L2: a rule (float) places the team chat; its entry leaves Desktop 1 and the stage it
    // emptied is neither written nor built at the visit.
    let mut m = launched(vec![desktop_1(), desktop_2()], Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(81, 10, CAL, "Week", r(10.0)));
    add(&mut m, win(82, 11, TERM, "server", r(9.0)));
    let float = Decision { action: crate::engine::rules::RuleAction::Float, ..Decision::default() };
    m.add_window(win(43, 21, TEAMCHAT, "General", r(3.0)), &float, &Observation::default());
    assert_eq!(m.window(w(43)).map(|x| x.home), Some(Home::Pinned));
    assert!(m.pooled_out.contains(&(4, 1)));
    let file = m.to_state();
    let d1 = file.spaces.iter().find(|s| s.id == 4).expect("Desktop 1 waits");
    assert_eq!(d1.stages.len(), 4, "no empty stage written for the chat's");
    assert!(d1.stages.iter().all(|s| !s.windows.is_empty()));
}

#[test]
fn a_native_on_its_own_desktop_keeps_its_confidence_against_a_same_title_elsewhere() {
    // L3: both Desktops had a terminal titled "zsh". The one that shows on Desktop 2 is scored
    // against Desktop 2's waiting windows first: confident there, it never looks elsewhere.
    let mut d1 = desktop_1();
    d1.stages[4].windows.push(sw(TERM, "zsh", r(14.0), 1));
    let mut d2 = desktop_2();
    d2.stages[1].windows.push(sw(TERM, "zsh", r(15.0), 1));
    let mut m = launched(vec![d1, d2], Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(81, 10, CAL, "Week", r(10.0)));
    add(&mut m, win(82, 11, TERM, "server", r(9.0)));
    assert_eq!(m.current_space(MAIN).0, 8);
    add(&mut m, win(85, 11, TERM, "zsh", r(20.0)));
    assert_eq!(kin(&m, 85), Some(Kin::Saved(8, 1)), "its own Desktop's");
    assert!(!m.is_pooled(w(85)) && m.restored_grade(w(85)).is_some_and(Grade::confident));
    assert!(titles(&m).iter().any(|(t, n)| t == "zsh" && *n == 2), "Desktop 1's still waits for its visit");
    // Nothing on its own Desktop: the other Desktop's then, with confidence.
    add(&mut m, win(86, 11, TERM, "zsh", r(14.0)));
    assert_eq!(kin(&m, 86), Some(Kin::Saved(4, 4)));
    assert!(m.is_pooled(w(86)));
    // Two live windows titled "zsh", nothing waiting for either.
    assert!(titles(&m).iter().all(|(t, n)| *n == if t == "zsh" { 2 } else { 1 }), "{:?}", titles(&m));
    assert!(!m.to_state().spaces.iter().filter(|s| s.id == 4).flat_map(|s| &s.stages).flat_map(|s| &s.windows).any(|x| x.title == "zsh"));
}


/// The login on a Desktop that adopts nothing: a window of no saved Desktop first (in front),
/// then the calendar of Desktop 2, a vote that never adopts, parked alone in a stage of its own
/// by Apple's rule, then (`third`) another window of no saved Desktop in a stage after it.
fn lone_voter_behind(m: &mut Model, third: bool) -> (SpaceId, StageId) {
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    let here = m.current_space(MAIN);
    add(m, win(99, 30, "com.example.notes", "Notes", r(13.0)));
    add(m, win(81, 10, CAL, "Week", r(10.0)));
    if third {
        add(m, win(98, 31, "com.example.draw", "Sketch", r(16.0)));
    }
    let y = m.stage_of(w(81)).expect("managed");
    assert!(!m.is_active(y) && m.window(w(81)).is_some_and(|x| x.parked), "a voter parked alone in an inactive stage");
    assert_eq!(m.restored_grade(w(81)), None, "by Apple's rule meanwhile");
    (here, y)
}

#[test]
fn the_card_click_that_ends_the_restore_window_keeps_its_stage() {
    // The review of 8I2 (Medium): the user presses the card of the voter's stage; the press
    // ends the restore window. The stage becomes the voter's part in place: the click that
    // follows finds it, and the window shows with it.
    let mut m = launched(vec![desktop_1(), desktop_2()], Restore::after_restart("B"));
    let (here, y) = lone_voter_behind(&mut m, true);
    let number = m.stage_number(y);
    let a = m.end_restore().expect("it was open (the card's press)");
    assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } if id.0 == 81)), "never shown on the way: {a:?}");
    assert!(m.stage(y).is_some() && m.stage_of(w(81)) == Some(y), "the same stage");
    assert_eq!(m.stage_number(y), number, "the same number");
    assert_eq!(m.root_of(y), Kin::Saved(8, 2), "now the part of its saved stage");
    assert!(m.is_pooled(w(81)) && m.restored_grade(w(81)).is_some_and(Grade::confident));
    assert!(titles(&m).iter().all(|(_, n)| *n == 1), "{:?}", titles(&m));
    // The click (mouse-up).
    assert_eq!(m.follow_stage(y), y);
    let a = m.switch_to(m.follow_stage(y), &Observation::default());
    assert!(m.is_active(y) && m.window(w(81)).is_some_and(|x| !x.parked && x.space == here));
    let back = a.iter().find_map(|x| match x {
        Action::SetFrame { id, frame } if id.0 == 81 => Some(*frame),
        _ => None,
    });
    assert!(back.is_some_and(|f| !m.display(MAIN).parks(f)), "shown at a real frame: {a:?}");
}

#[test]
fn the_number_key_that_ends_the_restore_window_keeps_its_stage() {
    // The same with a number key: the stage after the voter's keeps its number, the voter's
    // stage its own.
    let mut m = launched(vec![desktop_1(), desktop_2()], Restore::after_restart("B"));
    let (here, y) = lone_voter_behind(&mut m, true);
    let n = m.stage_number(y).expect("numbered");
    let after = m.stage_by_number(here, n + 1).expect("a stage after it");
    let r = m.number_ref(here, n);
    m.end_restore().expect("it was open (the key)");
    assert_eq!(m.number_now(here, r), n);
    assert_eq!(m.stage_number(after), Some(n + 1));
    let main = m.main_display().id;
    m.switch_to_number(m.number_now(here, r), main, &Observation::default());
    assert!(m.is_active(y) && m.window(w(81)).is_some_and(|x| !x.parked));
    // The next number still makes a stage.
    let next = m.number_ref(here, m.stages_in(here).count() + 1);
    assert_eq!(next, NumberRef::Next);
    assert_eq!(m.number_now(here, next), m.stages_in(here).count() + 1);
    assert_eq!(m.number_now(here, NumberRef::Missing), 0);
}

#[test]
fn a_card_or_number_of_a_voters_stage_that_goes_follows_its_window() {
    // The voter's saved stage is named, and a declared stage of that name is on the Desktop:
    // the voter goes there and its own stage goes. The card clicked and the number pressed
    // for that stage follow the window; the number of the stage after it too.
    let mut d2 = desktop_2();
    d2.stages[2].name = Some("Plan".into());
    let options = ModelOptions { named_stages: vec!["Plan".into()], ..Default::default() };
    let mut file = StateFile { session: Some("A".into()), ..StateFile::single(SavedSpace::default()) };
    file.spaces = vec![desktop_1(), d2];
    file.start_new_session();
    let (file, _) = file.for_session(Some("B"));
    let mut m = crate::engine::testkit::model_with(options);
    m.set_restore(Restore::after_restart("B"));
    m.restore_spaces(file.spaces);
    let (here, y) = lone_voter_behind(&mut m, true);
    let plan = m.find_named(here, "Plan").expect("declared here");
    assert_ne!(plan, y);
    let n = m.stage_number(y).expect("numbered");
    let r = m.number_ref(here, n);
    let r_after = m.number_ref(here, n + 1);
    let sketch = m.stage_of(w(98)).expect("managed");
    m.end_restore().expect("it was open");
    assert!(m.stage(y).is_none(), "its stage went");
    assert_eq!(m.stage_of(w(81)), Some(plan));
    assert_eq!(m.follow_stage(y), plan, "the card follows the window");
    assert_eq!(m.number_now(here, r), m.stage_number(plan).expect("numbered"), "the number too");
    assert_eq!(m.number_now(here, r_after), m.stage_number(sketch).expect("numbered"), "the next stage keeps meaning itself");
    assert!(titles(&m).iter().all(|(_, n)| *n == 1), "{:?}", titles(&m));
}

#[test]
fn what_the_restore_remembers_of_a_window_or_stage_goes_with_it() {
    // Review E10 (10-29): `followed`, `merged` and the waiting fronts kept stages and windows
    // gone for the rest of the run.
    let mut d2 = desktop_2();
    d2.stages[2].name = Some("Plan".into());
    let options = ModelOptions { named_stages: vec!["Plan".into()], ..Default::default() };
    let mut file = StateFile { session: Some("A".into()), ..StateFile::single(SavedSpace::default()) };
    file.spaces = vec![desktop_1(), d2];
    file.start_new_session();
    let (file, _) = file.for_session(Some("B"));
    let mut m = crate::engine::testkit::model_with(options);
    m.set_restore(Restore::after_restart("B"));
    m.restore_spaces(file.spaces);
    let (_, y) = lone_voter_behind(&mut m, true);
    m.end_restore().expect("it was open");
    assert_eq!(m.followed.get(&y), Some(&w(81)), "its stage went; a click on its card follows it");
    let gone = StageId(9_999);
    m.merged.insert(Kin::Stage(gone), gone);
    m.restore.front_waits.push(gone);
    m.remove_window(w(81));
    assert!(m.followed.is_empty(), "the window gone, nothing follows it");
    assert!(m.merged.is_empty() && m.restore.front_waits.is_empty(), "no stage gone is remembered");
}

#[test]
fn a_voter_back_in_its_own_desktops_stage_leaves_the_entry_it_voted_with() {
    // Low 3: "zsh" on both Desktops; the one that shows first is nearest Desktop 1's and votes
    // for it; Desktop 2 then takes the Desktop, and the voter's own Desktop's entry takes it.
    // Desktop 1's entry it voted with goes too (no stale duplicate).
    let mut d1 = desktop_1();
    d1.stages[4].windows.push(sw(TERM, "zsh", r(14.0), 1));
    let mut d2 = desktop_2();
    d2.stages[1].windows.push(sw(TERM, "zsh", r(15.0), 1));
    let mut m = launched(vec![d1, d2], Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(86, 11, TERM, "zsh", r(14.0)));
    assert_eq!(m.restored_grade(w(86)), None, "a vote");
    add(&mut m, win(81, 10, CAL, "Week", r(10.0)));
    add(&mut m, win(82, 11, TERM, "server", r(9.0)));
    assert_eq!(m.current_space(MAIN).0, 8);
    assert_eq!(kin(&m, 86), Some(Kin::Saved(8, 1)), "its own Desktop's");
    assert!(titles(&m).iter().all(|(_, n)| *n == 1), "{:?}", titles(&m));
}

#[test]
fn an_orphan_parked_in_a_stage_a_rule_empties_is_brought_back_on_screen() {
    // Review E2 (10-29): a rule places the one window a waiting stage still waits for; the
    // stage goes, its orphan dialog (parked with it) moves to the active stage and is shown,
    // but `drop_spot` threw away the frame that shows it: counted shown, left in the corner.
    let mut m = launched(vec![desktop_2()], Restore::same_session("A"));
    m.arrive(MAIN, &[win(82, 11, TERM, "server", r(9.0))], &[], &LaunchHints::default());
    assert_eq!(m.current_space(MAIN).0, 8);
    let mail = m.active_stage_in(m.current_space(MAIN)).expect("active");
    assert!(m.stage(mail).is_some_and(|s| s.windows.is_empty()) && m.has_pending(mail), "the mail stage waits, in front");
    let dialog = WindowMeta { role: WindowRole::Child { parent: None }, ..win(90, 30, "com.example.tool", "Progress", r(12.0)) };
    add(&mut m, dialog);
    assert_eq!(m.window(w(90)).map(|x| x.home), Some(Home::Stage(mail)), "an orphan in the stage in front");
    let term = m.stage_of(w(82)).expect("stage");
    let a = m.switch_to(term, &Observation::default());
    assert!(parks(&a, 90), "parked with the mail stage");
    let float = Decision { action: crate::engine::rules::RuleAction::Float, ..Decision::default() };
    let a = m.add_window(win(83, 12, MAIL, "Archive", r(8.0)), &float, &Observation::default());
    assert!(m.stage(mail).is_none(), "the mail stage has nothing left to wait for");
    assert!(m.window(w(90)).is_some_and(|x| x.home == Home::Stage(term) && !x.parked), "the orphan joins the stage shown");
    assert!(a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } if id.0 == 90)), "and is put back on screen: {a:?}");
}

#[test]
fn a_voter_placed_at_its_own_frame_keeps_the_frame_it_had_before_fitting() {
    // Review E10 (10-29): `lift` dropped `original`, so a fitted voter placed back at its own
    // frame (the display not the saved size: no saved frames) lost the frame quit and every
    // safety path put it back at.
    let restore = Restore::after_restart("B").with_display(Some((1920.0, 1080.0)));
    let mut m = launched(vec![desktop_1(), desktop_2()], restore);
    m.set_fit(crate::engine::stage_area::FitOptions { inset: 216.0, shrink: false });
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(99, 30, "com.example.notes", "Notes", r(13.0)));
    let under_strip = Rect::new(20.0, 100.0, 500.0, 360.0);
    add(&mut m, win(81, 10, CAL, "Week", under_strip));
    add(&mut m, win(98, 31, "com.example.draw", "Sketch", r(16.0)));
    assert_eq!(m.window(w(81)).and_then(|x| x.original), Some(under_strip), "fitted clear of the strip");
    m.end_restore().expect("it was open");
    assert!(m.restored_grade(w(81)).is_some_and(Grade::confident), "back in its saved stage");
    let x = m.window(w(81)).expect("managed");
    assert_eq!(x.restore.x, 216.0, "at its own (fitted) frame");
    assert_eq!(x.original, Some(under_strip), "its frame before fitting kept");
}

// ----- the windows back on another Desktop, as the restore's line names them ------------------

fn listed(m: &Model) -> Vec<u32> {
    m.away_now().map(|(id, _)| id.0).collect()
}

#[test]
fn the_restores_line_names_the_strays_with_the_desktop_they_belong_on_and_the_one_they_are_on() {
    let m = login_on_desktop_2();
    let here = m.current_space(MAIN);
    assert_eq!(listed(&m), [41, 42, 43, 44, 45], "Desktop 1's strays, in the order they came back; no native");
    assert!(m.away_now().all(|(_, saved)| saved == 4));
    let line = m.away_line(|_| "#1a2b3c4d".to_string());
    assert!(line.starts_with("; on another Desktop: "), "{line}");
    assert!(line.contains(&format!("{TEAMCHAT} #1a2b3c4d (saved 4, now {})", here.0)), "{line}");
    assert!(!line.contains("General"), "titles through the hash");
}

#[test]
fn a_stray_leaves_the_line_once_its_window_shows_on_its_saved_desktop_or_closes() {
    let mut m = login_on_desktop_2();
    let d2 = m.current_space(MAIN);
    m.end_restore();
    m.remove_window(w(42));
    assert_eq!(listed(&m), [41, 43, 44, 45], "closed");
    // In Mission Control he drags the team chat onto Desktop 1, then goes there.
    let natives = [win(51, 30, TERM, "build", r(2.0)), win(52, 31, MUSIC, "Library", r(4.0)), win(53, 12, MAIL, "Inbox", r(7.0))];
    let chat = m.window(w(43)).map(|x| x.meta.clone()).expect("managed");
    let mut live = natives.to_vec();
    live.push(chat);
    let absent: Vec<WindowId> = m.windows().filter(|x| x.space == d2 && x.meta.id != w(43)).map(|x| x.meta.id).collect();
    m.arrive(MAIN, &live, &absent, &LaunchHints::default());
    assert_eq!(m.window(w(43)).map(|x| x.space.0), Some(4), "moved here");
    assert_eq!(listed(&m), [41, 44, 45], "on its saved Desktop");
    // Nothing restored from another Desktop: an empty line (a relaunch in the same session).
    let m = launched(vec![desktop_1(), desktop_2()], Restore::same_session("A"));
    assert!(m.away_line(|t| t.to_string()).is_empty());
}

/// One Desktop (saved 4, shown when saved): the IDE's two workspaces as two stages, "alpha"
/// in front with a terminal, "beta" a card; the IDE's titles are its workspaces' names with
/// no file open (the 11-06 restart: two VS Code windows, two stages).
fn two_workspaces() -> SavedSpace {
    SavedSpace {
        screen: None,
        id: 4,
        current: true,
        stages: vec![
            stage(vec![sw(IDE, "alpha (Workspace)", r(0.0), 0), sw(TERM, "build", r(2.0), 1)], 90),
            stage(vec![sw(IDE, "beta (Workspace)", r(1.0), 0)], 80),
        ],
        active_index: Some(0),
        strip_slots: vec![1],
        last_seen: None,
        unvisited: false,
    }
}

#[test]
fn an_editor_reopened_with_a_file_in_front_of_its_workspace_goes_to_its_own_stage() {
    // The title gained the file shown, in front of the workspace's name: the fingerprint
    // (its first segment) differs, the frame is the saved one; strong, not Apple's rule into
    // the IDE's other stage.
    let mut m = launched(vec![two_workspaces()], Restore::after_restart("B"));
    let alpha = win(41, 20, IDE, "alpha (Workspace)", r(0.0));
    let beta = win(42, 20, IDE, "main.rs — beta (Workspace)", r(1.0));
    m.arrive(MAIN, &[alpha, beta, win(43, 21, TERM, "build", r(2.0))], &[], &LaunchHints::default());
    assert_ne!(m.stage_of(w(41)), m.stage_of(w(42)), "two stages, as saved");
    assert_eq!(m.restored_grade(w(42)), Some(Grade::Strong));
    assert_eq!(m.stage_of(w(41)), m.stage_of(w(43)));
    assert_eq!(m.restore_summary(0).by_apple, 0);
}

#[test]
fn an_editor_titled_late_goes_to_its_own_stage_when_its_title_comes() {
    // The second window shows first under its app's name (an Electron window's title before
    // its workspace loads): no saved window takes it, and Apple's rule joins it to the IDE's
    // stage. Its title comes during the restore window: back in its saved stage, a card.
    let mut m = launched(vec![two_workspaces()], Restore::after_restart("B"));
    let alpha = win(41, 20, IDE, "alpha (Workspace)", r(0.0));
    let early = win(42, 20, IDE, "Example IDE", Rect::new(300.0, 200.0, 700.0, 500.0));
    m.arrive(MAIN, &[alpha, early, win(43, 21, TERM, "build", r(2.0))], &[], &LaunchHints::default());
    assert_eq!(m.stage_of(w(41)), m.stage_of(w(42)), "joined by Apple's rule, the late title's doing");
    assert_eq!(m.restore_summary(0).by_apple, 1);
    // A title that still says nothing: no change.
    assert!(m.window_title_changed(w(42), "Example IDE — Welcome"));
    assert!(m.window_retitled(w(42)).is_empty());
    assert!(m.window_title_changed(w(42), "beta (Workspace)"));
    let a = m.window_retitled(w(42));
    let beta = m.stage_of(w(42)).expect("placed");
    assert_ne!(Some(beta), m.stage_of(w(41)), "two stages again");
    assert_eq!(m.stage(beta).map(|s| s.windows.clone()), Some(vec![w(42)]));
    assert!(parks(&a, 42), "a card, parked: its stage is not in front: {a:?}");
    assert_eq!(m.window(w(42)).map(|x| x.restore), Some(r(1.0)), "its saved frame");
    assert_eq!(m.main_active_stage(), m.stage_of(w(41)), "nothing switched");
    assert!(m.strip_slots(MAIN).contains(&beta));
    let line = m.restore_summary(0);
    assert_eq!((line.by_apple, line.windows), (0, 3));
    // Once placed, a later title changes nothing.
    assert!(m.window_title_changed(w(42), "lib.rs — beta (Workspace)"));
    assert!(m.window_retitled(w(42)).is_empty());
}

/// The second workspace's window joined the first's stage by Apple's rule, titled late.
fn joined_by_a_late_title() -> Model {
    let mut m = launched(vec![two_workspaces()], Restore::after_restart("B"));
    let alpha = win(41, 20, IDE, "alpha (Workspace)", r(0.0));
    let early = win(42, 20, IDE, "Example IDE", Rect::new(300.0, 200.0, 700.0, 500.0));
    m.arrive(MAIN, &[alpha, early, win(43, 21, TERM, "build", r(2.0))], &[], &LaunchHints::default());
    assert_eq!(m.stage_of(w(41)), m.stage_of(w(42)));
    m
}

#[test]
fn a_click_on_another_window_early_leaves_a_late_title_its_rematch() {
    // He clicks the terminal before the editor's title comes: the restore window ends, and
    // the editor's window, untouched, still goes to its stage when its title comes.
    let mut m = joined_by_a_late_title();
    m.touch_window(w(43));
    m.end_restore();
    assert!(!m.restoring() && m.rematching());
    assert!(m.window_title_changed(w(42), "beta (Workspace)"));
    let a = m.window_retitled(w(42));
    assert_ne!(m.stage_of(w(42)), m.stage_of(w(41)), "its own stage");
    assert!(parks(&a, 42), "a card: {a:?}");
    assert_eq!(m.main_active_stage(), m.stage_of(w(41)), "nothing switched");
}

#[test]
fn a_window_the_user_touched_stays_where_it_is_when_its_title_comes() {
    // Focused by the owner's click (or moved, or its stage switched to): the owner's.
    let mut m = joined_by_a_late_title();
    m.touch_window(w(42));
    assert!(m.window_title_changed(w(42), "beta (Workspace)"));
    assert!(m.window_retitled(w(42)).is_empty());
    assert_eq!(m.stage_of(w(41)), m.stage_of(w(42)));
    // So is one whose stage he switched to.
    let mut m = joined_by_a_late_title();
    let shared = m.stage_of(w(42)).expect("in a stage");
    m.touch_stage(shared);
    assert!(m.window_title_changed(w(42), "beta (Workspace)"));
    assert!(m.window_retitled(w(42)).is_empty());
}

#[test]
fn a_key_press_in_a_window_a_late_title_could_move_makes_it_the_users() {
    // The review of 11-06, restore 2: the IDE focused its late-titled window itself at
    // the login (no press, no touch), and he types in it. The platform asks the app's focused
    // window at a key press only while one of its windows may still be re-matched; touched,
    // its title no longer moves it.
    let mut m = joined_by_a_late_title();
    assert!(m.may_rematch_app(20), "the IDE's window went by Apple's rule");
    assert!(!m.may_rematch_app(21), "the terminal's matched");
    m.touch_window(w(42));
    assert!(!m.may_rematch_app(20), "typed in: nothing left to ask");
    assert!(m.window_title_changed(w(42), "beta (Workspace)"));
    assert!(m.window_retitled(w(42)).is_empty());
    assert_eq!(m.stage_of(w(41)), m.stage_of(w(42)));
    // After the two minutes no key press asks.
    let mut m = joined_by_a_late_title();
    m.end_rematch();
    assert!(!m.may_rematch_app(20));
}

#[test]
fn a_late_title_after_the_two_minutes_moves_nothing() {
    let mut m = joined_by_a_late_title();
    m.end_rematch();
    assert!(m.window_title_changed(w(42), "beta (Workspace)"));
    assert!(m.window_retitled(w(42)).is_empty());
    assert_eq!(m.stage_of(w(41)), m.stage_of(w(42)));
}

/// The 11-06 restart, Fork's two windows (made-up titles): the git client's "proj" saved as a
/// stage of its own on Desktop 2 (saved 8), its "site" as a stage of its own on Desktop 1
/// (saved 4, shown when saved). The login shows Desktop 2, and the app (non-native) reopens
/// both windows there, at their saved frames.
fn git_on_two_desktops() -> Vec<SavedSpace> {
    let one = SavedSpace {
        screen: None,
        id: 4,
        current: true,
        stages: vec![stage(vec![sw(MAIL, "Inbox", r(7.0), 0)], 50), stage(vec![sw(GIT, "site", r(6.0), 0)], 45)],
        active_index: Some(0),
        strip_slots: vec![1],
        last_seen: None,
        unvisited: false,
    };
    let two = SavedSpace {
        screen: None,
        id: 8,
        current: false,
        stages: vec![
            stage(vec![sw(MAIL, "Archive", r(8.0), 0)], 40),
            stage(vec![sw(TERM, "server", r(9.0), 0)], 30),
            stage(vec![sw(GIT, "proj", r(5.0), 0)], 35),
        ],
        active_index: Some(0),
        strip_slots: vec![2, 1],
        last_seen: None,
        unvisited: false,
    };
    vec![one, two]
}

/// The login on Desktop 2: its two natives adopt it.
fn login_for_git() -> Model {
    let mut m = launched(git_on_two_desktops(), Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(81, 12, MAIL, "Archive", r(8.0)));
    add(&mut m, win(82, 11, TERM, "server", r(9.0)));
    assert_eq!(m.current_space(MAIN).0, 8);
    m
}

/// A git window as the controller decides it with `[stages] new_apps_join_current` on while
/// the restore window is open (the key does not apply then; the model ignores it either way).
fn add_git(m: &mut Model, id: u32, title: &str, frame: Rect) -> Vec<Action> {
    let joins = Decision { new_app_joins_current: true, ..Decision::default() };
    m.add_window(win(id, 30, GIT, title, frame), &joins, &Observation::default())
}

#[test]
fn an_app_reopening_two_windows_saved_as_two_stages_on_two_desktops_makes_two_stages() {
    let mut m = login_for_git();
    let (front, apple) = (m.main_active_stage(), m.restore_summary(0).by_apple);
    add_git(&mut m, 61, "proj", r(5.0));
    add_git(&mut m, 62, "site", r(6.0));
    assert_ne!(m.stage_of(w(61)), m.stage_of(w(62)), "two stages, as saved");
    assert_eq!(kin(&m, 61), Some(Kin::Saved(8, 2)), "its own Desktop's saved stage");
    assert_eq!(kin(&m, 62), Some(Kin::Saved(4, 1)), "the other Desktop's, made here");
    assert!(m.is_pooled(w(62)) && !m.is_pooled(w(61)));
    assert_eq!(m.main_active_stage(), front, "nothing switched");
    assert_eq!(m.restore_summary(0).by_apple, apple, "none of them by Apple's rule");
}

#[test]
fn a_window_reopened_under_another_name_at_its_saved_frame_goes_to_its_own_stage() {
    // Fork's second window came back with another title (the repository it shows): two
    // names, so the frame counted as the size only, a low match, and Apple's rule joined it
    // to the first window's stage. While the restore window is open it is the only window of
    // the app at the only saved frame of the app's still waiting there: that window.
    let mut m = login_for_git();
    let apple = m.restore_summary(0).by_apple;
    add_git(&mut m, 61, "proj", r(5.0));
    let a = add_git(&mut m, 62, "notes", r(6.0));
    let site = m.stage_of(w(62)).expect("placed");
    assert_ne!(Some(site), m.stage_of(w(61)), "two stages");
    assert_eq!(m.stage(site).map(|s| s.windows.clone()), Some(vec![w(62)]));
    assert_eq!(kin(&m, 62), Some(Kin::Saved(4, 1)));
    assert_eq!(m.restored_grade(w(62)), Some(Grade::Medium));
    assert_eq!(m.restore_summary(0).by_apple, apple);
    assert!(parks(&a, 62) || m.main_active_stage() == Some(site), "a card or the empty front: {a:?}");
    let stages = m.stages_in(m.current_space(MAIN)).count();
    // The other order: the renamed window first (no window of its app back yet: Apple's
    // rule, a stage of its own), then the one with its name, which shows its app is
    // reopening its saved windows: the first is looked at again and goes to its saved stage.
    let mut m = login_for_git();
    add_git(&mut m, 62, "notes", r(6.0));
    assert_eq!(m.restored_grade(w(62)), None);
    add_git(&mut m, 61, "proj", r(5.0));
    assert_ne!(m.stage_of(w(61)), m.stage_of(w(62)));
    assert_eq!((kin(&m, 61), kin(&m, 62)), (Some(Kin::Saved(8, 2)), Some(Kin::Saved(4, 1))));
    assert_eq!(m.restored_grade(w(62)), Some(Grade::Medium));
    assert_eq!(m.restore_summary(0).by_apple, apple);
    assert_eq!(m.stages_in(m.current_space(MAIN)).count(), stages, "the stage Apple's rule made is gone");
}

#[test]
fn a_renamed_window_at_a_saved_frame_keeps_apples_rule_when_more_could_be_meant() {
    // After the restore window (a click or key: the user may have opened it, Finder's ⌘N
    // where a saved window was), the frame never outweighs two names.
    let mut m = login_for_git();
    add_git(&mut m, 61, "proj", r(5.0));
    m.end_restore();
    add_git(&mut m, 62, "notes", r(6.0));
    assert_eq!(m.stage_of(w(61)), m.stage_of(w(62)), "Apple's rule");
    // Two saved windows of the app at that frame: either could be meant.
    let mut spaces = git_on_two_desktops();
    spaces[0].stages.push(stage(vec![sw(GIT, "blog", r(6.0), 0)], 20));
    let mut m = launched(spaces, Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(81, 12, MAIL, "Archive", r(8.0)));
    add(&mut m, win(82, 11, TERM, "server", r(9.0)));
    add_git(&mut m, 61, "proj", r(5.0));
    add_git(&mut m, 62, "notes", r(6.0));
    assert_eq!(m.stage_of(w(61)), m.stage_of(w(62)), "Apple's rule");
    assert_eq!(m.restored_grade(w(62)), None);
}

#[test]
fn a_renamed_window_saved_with_its_sibling_goes_back_to_their_stage() {
    // The two windows saved in one stage: the renamed one goes there too.
    let mut spaces = git_on_two_desktops();
    spaces[0].stages.remove(1);
    spaces[1].stages[2].windows.push(sw(GIT, "site", r(6.0), 1));
    let mut m = launched(spaces, Restore::after_restart("B"));
    m.arrive(MAIN, &[], &[], &LaunchHints::default());
    add(&mut m, win(81, 12, MAIL, "Archive", r(8.0)));
    add(&mut m, win(82, 11, TERM, "server", r(9.0)));
    add_git(&mut m, 61, "proj", r(5.0));
    add_git(&mut m, 62, "notes", r(6.0));
    assert_eq!(m.stage_of(w(61)), m.stage_of(w(62)));
    assert_eq!(m.restored_grade(w(62)), Some(Grade::Medium));
}
