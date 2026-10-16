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
    }
}

/// Desktop 2 (saved 8): mail's archive (active), a terminal, the calendar, the web browser.
/// Strip: calendar, terminal, web.
fn desktop_2() -> SavedSpace {
    SavedSpace {
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
    m.switch_to_number(m.number_now(here, r), &Observation::default());
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
