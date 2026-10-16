//! The pool after a restart (round 8I). Made-up titles only.
//!
//! The 10-16 restart, as the owner explained it: native apps reopen their windows on their own
//! saved Desktops; non-native ones (Electron and the like) on the Desktop shown at login. He
//! restarted on Desktop 1 (saved 4, the larger) and the login showed Desktop 2 (saved 8).

use super::*;
use crate::engine::geometry::Rect;
use crate::engine::model::{Model, Observation, Pid, WindowRole};
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
