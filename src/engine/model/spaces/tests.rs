//! Spaces tests: two Desktops with their own stages and strips, recognition by windows,
//! moves, sticky windows, leaving, persistence per Space. Made-up titles only.

use std::collections::BTreeMap;

use super::*;
use crate::engine::geometry::Rect;
use crate::engine::model::{NativeTarget, Observation, Pid, Target};
use crate::engine::park::is_parked;
use crate::engine::persist::{StateFile, rematch};
use crate::engine::rules::Decision;
use crate::engine::testkit::*;

/// The model plus a fake Mac with two Desktops: each window lives on one (or, sticky, on
/// both); only the shown Desktop's windows are "on screen".
struct Mac {
    m: Model,
    /// Window → (Desktop index, or `None` for every Desktop; live frame; meta).
    wins: BTreeMap<u32, (Option<usize>, Rect, WindowMeta)>,
    shown: usize,
    focused: Option<WindowId>,
    /// Every action on a window that is not on the shown Desktop.
    elsewhere: Vec<Action>,
}

impl Mac {
    fn new() -> Self {
        Self {
            m: model(),
            wins: BTreeMap::new(),
            shown: 0,
            focused: None,
            elsewhere: Vec::new(),
        }
    }

    /// Puts a window on Desktop `desk` without telling the model.
    fn put(&mut self, desk: usize, id: u32, pid: Pid, bundle: &str, title: &str) {
        let meta = WindowMeta {
            title: title.into(),
            ..meta(id, pid, bundle)
        };
        self.wins.insert(id, (Some(desk), meta.frame, meta));
    }

    fn on_screen(&self, id: u32) -> bool {
        self.wins
            .get(&id)
            .is_some_and(|(d, _, _)| d.is_none_or(|d| d == self.shown))
    }

    fn live(&self) -> Vec<WindowMeta> {
        self.wins
            .iter()
            .filter(|(id, _)| self.on_screen(**id))
            .map(|(_, (_, f, m))| WindowMeta { frame: *f, ..m.clone() })
            .collect()
    }

    fn run(&mut self, actions: Vec<Action>) -> Vec<Action> {
        for a in &actions {
            let id = match *a {
                Action::SetFrame { id, .. } | Action::Park { id, .. } | Action::Raise { id } | Action::Focus { id } => id,
                Action::StateChanged => continue,
            };
            if !self.on_screen(id.0) {
                self.elsewhere.push(*a);
            }
            match *a {
                Action::SetFrame { id, frame } | Action::Park { id, frame } => {
                    if let Some(w) = self.wins.get_mut(&id.0) {
                        w.1 = frame;
                    }
                }
                Action::Focus { id } => self.focused = Some(id),
                _ => {}
            }
        }
        actions
    }

    /// The user goes to Desktop `desk` (or the app launches there).
    fn go(&mut self, desk: usize) -> Vec<Action> {
        self.shown = desk;
        self.arrive()
    }

    /// What the platform reports on a Space change: the windows on screen, the managed ones
    /// missing (windows on a Desktop numbered 9 stand for hidden apps' windows: off screen
    /// for a visible reason), and the focused window first in z-order.
    fn arrive(&mut self) -> Vec<Action> {
        let live = self.live();
        let absent: Vec<WindowId> = self
            .wins
            .iter()
            .filter(|(id, (d, _, _))| !self.on_screen(**id) && *d != Some(9))
            .map(|(id, _)| w(*id))
            .filter(|id| self.m.window(*id).is_some())
            .collect();
        let mut z: Vec<WindowId> = live.iter().map(|m| m.id).collect();
        if let Some(f) = self.focused.filter(|f| z.contains(f)) {
            z.retain(|x| *x != f);
            z.insert(0, f);
        }
        let frontmost = self.focused.and_then(|f| self.wins.get(&f.0)).map(|x| x.2.pid);
        let hints = LaunchHints { z_order: z, frontmost };
        let a = self.m.arrive(MAIN, &live, &absent, &hints);
        self.run(a)
    }

    fn obs(&self) -> Observation {
        Observation {
            frames: self
                .wins
                .iter()
                .filter(|(id, _)| self.on_screen(**id))
                .map(|(id, (_, f, _))| (w(*id), *f))
                .collect(),
            z_order: self.live().iter().map(|m| m.id).collect(),
            focused: self.focused,
        }
    }

    /// A new window opened on the shown Desktop.
    fn open(&mut self, id: u32, pid: Pid, bundle: &str) -> Vec<Action> {
        self.put(self.shown, id, pid, bundle, &format!("Doc {id}"));
        let meta = self.wins[&id].2.clone();
        self.focused = Some(w(id));
        let obs = self.obs();
        let a = self.m.add_window(meta, &Decision::default(), &obs);
        self.run(a)
    }

    fn click(&mut self, stage: StageId) -> Vec<Action> {
        let obs = self.obs();
        let a = self.m.switch_to(stage, &obs);
        self.run(a)
    }

    fn parked(&self, id: u32) -> bool {
        is_parked(self.wins[&id].1, FRAME)
    }

    fn space(&self) -> SpaceId {
        self.m.current_space(MAIN)
    }

    /// The strip's stages as sets of window ids.
    fn strip(&self) -> Vec<Vec<u32>> {
        self.m.strip().iter().map(|c| c.windows.iter().map(|x| x.0).collect()).collect()
    }

    fn active(&self) -> Vec<u32> {
        let s = self.m.main_active_stage().expect("active");
        self.m.stage(s).expect("stage").windows.iter().map(|x| x.0).collect()
    }
}

/// Desktop 1: an editor window and a browser; Desktop 2: another editor window and chat.
fn two_desktops() -> Mac {
    let mut mac = Mac::new();
    mac.put(0, 1, 10, "com.example.editor", "Work notes");
    mac.put(0, 2, 20, "com.example.browser", "Work wiki");
    mac.put(1, 3, 10, "com.example.editor", "Home notes");
    mac.put(1, 4, 30, "com.example.chat", "Family");
    mac
}

#[test]
fn each_desktop_has_its_own_stages_and_strip() {
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    let d1 = mac.space();
    assert_eq!(mac.m.stages_in(d1).count(), 2, "one stage per app on Desktop 1");
    assert_eq!(mac.strip().len(), 1);
    assert!(mac.elsewhere.is_empty(), "nothing on Desktop 2 touched");

    mac.go(1);
    let d2 = mac.space();
    assert_ne!(d1, d2, "Desktop 2 gets a Space of its own");
    assert_eq!(mac.m.stages_in(d2).count(), 2, "the editor has a stage on each Desktop");
    let strip: Vec<u32> = mac.strip().concat();
    assert!(strip.iter().all(|id| [3, 4].contains(id)), "Desktop 2's strip shows only its windows: {strip:?}");
    assert!(mac.elsewhere.is_empty(), "nothing on Desktop 1 touched: {:?}", mac.elsewhere);
    assert_eq!(mac.m.space_of(w(1)), Some(d1));
    assert_eq!(mac.m.space_of(w(3)), Some(d2));

    // Back on Desktop 1: recognized by its windows, its stages as they were.
    mac.go(0);
    assert_eq!(mac.space(), d1);
    assert_eq!(mac.strip().concat().len(), 1);
    assert!(mac.strip().concat().iter().all(|id| [1, 2].contains(id)));
    assert!(mac.elsewhere.is_empty());
}

#[test]
fn a_card_click_never_touches_another_desktop() {
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    let d1 = mac.space();
    let d1_card = mac.m.strip()[0].stage;
    mac.go(1);
    // A stale click on a Desktop 1 card (or a hotkey) while Desktop 2 shows does nothing.
    assert!(mac.click(d1_card).is_empty());
    let card = mac.m.strip()[0].stage;
    let a = mac.click(card);
    assert!(a.iter().any(|x| matches!(x, Action::Focus { .. })));
    assert!(mac.elsewhere.is_empty(), "{:?}", mac.elsewhere);
    assert_eq!(mac.m.active_stage_in(d1).map(|s| mac.m.stage(s).is_some()), Some(true));
}

#[test]
fn stage_numbers_and_hotkeys_are_per_desktop() {
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    mac.go(1);
    let d2 = mac.space();
    assert_eq!(mac.m.stage_by_number(d2, 1), mac.m.stages_in(d2).next().map(|s| s.id));
    let first = mac.m.stages_in(d2).next().map(|s| s.id).expect("stage");
    assert_eq!(mac.m.stage_number(first), Some(1));
    let obs = mac.obs();
    let a = mac.m.switch_to_number(2, &obs);
    mac.run(a);
    assert!(mac.elsewhere.is_empty());
    assert!(mac.m.switch_to_number(3, &obs).is_empty(), "no third stage on Desktop 2");
    // Sending the focused window to a new stage stays on its Desktop.
    let obs = mac.obs();
    let focused = mac.focused.expect("focused");
    let a = mac.m.send_window(focused, Target::New, &obs);
    mac.run(a);
    assert_eq!(mac.m.space_of(focused), Some(d2));
    assert!(mac.m.stage_of(focused).is_some_and(|s| mac.m.stage(s).is_some_and(|s| s.space == d2)));
}

#[test]
fn new_windows_and_activation_follow_the_shown_desktop_only() {
    let mut mac = two_desktops();
    mac.focused = Some(w(2));
    mac.go(0);
    mac.go(1);
    // Chat only lives on Desktop 2; the browser only on Desktop 1.
    assert_eq!(mac.m.native_target(20, MAIN), NativeTarget::Fresh, "the browser has no window here");
    let obs = mac.obs();
    assert!(mac.m.app_activated(20, &obs).is_empty(), "macOS decides whether to switch Spaces");
    // A new browser window on Desktop 2 opens a stage here and leaves Desktop 1 alone.
    mac.open(5, 20, "com.example.browser");
    assert_eq!(mac.m.space_of(w(5)), Some(mac.space()));
    assert_eq!(mac.active(), [5]);
    assert!(mac.elsewhere.is_empty(), "{:?}", mac.elsewhere);
}

#[test]
fn a_window_moved_to_another_desktop_joins_it() {
    let mut mac = two_desktops();
    mac.put(0, 6, 40, "com.example.music", "Player");
    mac.focused = Some(w(1));
    mac.go(0);
    let d1 = mac.space();
    assert!(mac.parked(6), "the music player waits in the strip");
    mac.go(1);
    let d2 = mac.space();
    // Mission Control: the parked player goes to Desktop 2 and shows up there.
    mac.wins.get_mut(&6).expect("window").0 = Some(1);
    let a = mac.go(1);
    assert_eq!(mac.space(), d2);
    assert_eq!(mac.m.space_of(w(6)), Some(d2));
    assert!(mac.active().contains(&6), "joins the shown stage");
    assert!(a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } if *id == w(6))), "brought out of the park zone");
    assert!(!mac.parked(6));
    assert!(mac.m.stages_in(d1).all(|s| !s.windows.contains(&w(6))), "its old stage is gone or without it");
    assert!(mac.m.window(w(6)).is_some_and(|x| !x.parked));
}

#[test]
fn a_window_on_every_desktop_becomes_sticky() {
    let mut mac = two_desktops();
    mac.put(0, 7, 50, "com.example.clock", "Clock");
    mac.wins.get_mut(&7).expect("window").0 = None;
    mac.focused = Some(w(1));
    mac.go(0);
    mac.go(1);
    assert!(!mac.m.is_sticky(w(7)), "seen once elsewhere: a move, as far as we know");
    mac.go(0);
    assert!(mac.m.is_sticky(w(7)), "seen again where it came from: on every Desktop");
    assert_eq!(mac.m.window(w(7)).map(|x| x.home), Some(Home::Pinned));
    mac.go(1);
    mac.go(0);
    assert!(mac.m.is_sticky(w(7)));
    assert_eq!(mac.m.space_of(w(1)), mac.m.space_of(w(2)), "Desktop 1 still recognized");
}

#[test]
fn a_window_that_left_the_desktop_leaves_its_stage_and_comes_back_unparked() {
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    assert!(mac.parked(2));
    mac.wins.get_mut(&2).expect("window").0 = Some(1);
    let (a, forgotten) = mac.m.window_left_space(w(2));
    assert!(a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } if *id == w(2))), "parked: put back first");
    assert_eq!(forgotten, [w(2)]);
    assert!(mac.m.window(w(2)).is_none());
    assert!(mac.m.safety_targets().iter().all(|(id, _)| *id != w(2)));
}

#[test]
fn an_arrival_guessed_wrong_is_corrected_by_the_windows_on_screen() {
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    let d1 = mac.space();
    // Back from Desktop 2 while Desktop 1's apps are hidden: nothing on screen to tell.
    mac.go(1);
    let hidden = [1, 2];
    for id in hidden {
        mac.wins.get_mut(&id).expect("window").0 = Some(9);
    }
    mac.go(0);
    assert_ne!(mac.space(), d1, "no evidence: an empty Space for now");
    for id in hidden {
        mac.wins.get_mut(&id).expect("window").0 = Some(0);
    }
    // The windows on screen disagree with the Space shown: the platform arrives again.
    mac.arrive();
    assert_eq!(mac.space(), d1, "the windows tell which Desktop it is");
    assert_eq!(mac.m.spaces().len(), 2, "the empty guess is forgotten");
    assert!(mac.elsewhere.is_empty());
}

#[test]
fn a_window_carried_to_a_new_desktop_does_not_make_it_the_old_one() {
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    let d1 = mac.space();
    // Dragged across to Desktop 2 (never visited): the only known window there.
    mac.wins.get_mut(&1).expect("window").0 = Some(1);
    mac.go(1);
    let d2 = mac.space();
    assert_ne!(d2, d1, "Desktop 1's browser is missing from the screen: not Desktop 1");
    assert_eq!(mac.m.space_of(w(1)), Some(d2), "the carried window joins Desktop 2");
    assert_eq!(mac.m.space_of(w(2)), Some(d1), "Desktop 1 keeps its other windows");
    assert!(mac.elsewhere.is_empty(), "{:?}", mac.elsewhere);
    mac.go(0);
    assert_eq!(mac.space(), d1);
}

#[test]
fn empty_desktops_are_forgotten_when_left() {
    let mut mac = two_desktops();
    mac.go(2);
    let empty = mac.space();
    mac.focused = Some(w(1));
    mac.go(0);
    assert!(mac.m.spaces().iter().all(|s| s.id != empty) || mac.space() == empty);
    assert_eq!(mac.m.space_of(w(1)), Some(mac.space()));
}

#[test]
fn unpark_all_covers_parked_windows_on_every_desktop() {
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    mac.focused = Some(w(3));
    mac.go(1);
    assert!(mac.parked(2) && mac.parked(4));
    let a = mac.m.unpark_all();
    let ids: Vec<WindowId> = a
        .iter()
        .filter_map(|x| match x {
            Action::SetFrame { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert!(ids.contains(&w(2)) && ids.contains(&w(4)), "{ids:?}");
}

#[test]
fn state_is_saved_per_desktop_and_rebuilt_on_each_first_visit() {
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    mac.focused = Some(w(4));
    mac.go(1);
    let d2 = mac.space();
    let file = mac.m.to_state();
    assert_eq!(file.spaces.len(), 2);
    assert_eq!(file.spaces.iter().filter(|s| s.current).count(), 1);
    assert_eq!(file.spaces.iter().find(|s| s.current).map(|s| s.id), Some(d2.0));
    let json = file.to_json();

    // Relaunch on Desktop 1 with new window ids; Desktop 2's windows are not listed yet.
    let saved = StateFile::from_json(&json).expect("parses");
    let mut again = Mac::new();
    again.m.restore_spaces(saved.spaces);
    again.put(0, 11, 10, "com.example.editor", "Work notes");
    again.put(0, 12, 20, "com.example.browser", "Work wiki");
    again.put(1, 13, 10, "com.example.editor", "Home notes");
    again.put(1, 14, 30, "com.example.chat", "Family");
    again.focused = Some(w(11));
    again.go(0);
    let d1 = again.space();
    assert_ne!(d1, d2, "Desktop 1 is recognized by its titles, not taken for the current one");
    assert_eq!(again.m.unvisited_saved_spaces(), 1, "Desktop 2 waits for its first visit");
    // Saved again before that visit, Desktop 2's stages are kept as they were.
    let resaved = again.m.to_state();
    assert_eq!(resaved.spaces.len(), 2);
    assert!(resaved.spaces.iter().any(|s| s.id == d2.0 && !s.current));

    again.go(1);
    assert_eq!(again.space(), d2, "Desktop 2 keeps its saved id");
    assert_eq!(again.m.unvisited_saved_spaces(), 0);
    assert_eq!(again.active(), [14], "the saved active stage of Desktop 2");
    assert!(again.parked(13));
    assert!(again.elsewhere.is_empty(), "{:?}", again.elsewhere);
}

#[test]
fn a_first_launch_without_title_matches_uses_the_desktop_shown_when_saved() {
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    let d1 = mac.space();
    let file = mac.m.to_state();
    let mut again = Mac::new();
    again.m.restore_spaces(file.spaces);
    again.put(0, 21, 10, "com.example.editor", "Renamed notes");
    again.put(0, 22, 20, "com.example.browser", "Another page");
    again.focused = Some(w(21));
    again.go(0);
    assert_eq!(again.space(), d1);
    assert_eq!(again.m.stages().len(), 2, "matched by app into the saved stages");
}

#[test]
fn saved_ids_never_collide_with_the_models_own() {
    let mut m = model();
    let own = m.current_space(MAIN);
    let saved = crate::engine::persist::SavedSpace { id: own.0, current: true, ..Default::default() };
    m.restore_spaces(vec![saved]);
    assert_ne!(m.current_space(MAIN), own, "the model's Space was renumbered");
    assert_eq!(m.spaces().len(), 1);
    assert!(m.main_active_stage().is_some());
    let _ = rematch;
}
