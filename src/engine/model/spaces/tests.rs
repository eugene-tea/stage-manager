//! Spaces tests: two Desktops with their own stages and strips, recognition by windows,
//! moves, sticky windows, leaving, persistence per Space. Made-up titles only.

use std::collections::BTreeMap;

use super::*;
use crate::engine::geometry::Rect;
use crate::engine::model::{NativeTarget, Observation, Pid, Target};
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
    /// macOS leaves windows of a Desktop not shown where they are (moves there do nothing).
    still_elsewhere: bool,
}

impl Mac {
    fn new() -> Self {
        Self {
            m: model(),
            wins: BTreeMap::new(),
            shown: 0,
            focused: None,
            elsewhere: Vec::new(),
            still_elsewhere: false,
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
                if self.still_elsewhere {
                    continue;
                }
            }
            match *a {
                Action::SetFrame { id, frame } | Action::Park { id, frame } => {
                    if let Some(w) = self.wins.get_mut(&id.0) {
                        w.1 = macos_places(frame);
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
    /// missing (windows on a Desktop numbered 9 stand for hidden apps' windows, on 8 for
    /// windows in full screen, on a Space of their own: off screen for a visible reason),
    /// and the focused window first in z-order.
    fn arrive(&mut self) -> Vec<Action> {
        let live = self.live();
        let absent: Vec<WindowId> = self
            .wins
            .iter()
            .filter(|(id, (d, _, _))| !self.on_screen(**id) && !matches!(*d, Some(FULL_SCREEN | HIDDEN)))
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
        parked_at(self.wins[&id].1)
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

/// The fake Desktop of hidden apps' windows.
const HIDDEN: usize = 9;
/// The fake Desktop of windows in full screen (each on a Space of its own).
const FULL_SCREEN: usize = 8;

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
fn a_card_dropped_on_a_card_merges_on_the_shown_desktop_only() {
    let mut mac = two_desktops();
    mac.put(0, 6, 40, "com.example.music", "Playlist");
    mac.focused = Some(w(1));
    mac.go(0);
    let d1 = mac.space();
    let d1_cards: Vec<StageId> = mac.m.strip().iter().map(|c| c.stage).collect();
    assert_eq!(d1_cards.len(), 2, "two cards on Desktop 1");
    mac.go(1);
    let d2 = mac.space();
    let d2_card = mac.m.strip()[0].stage;
    // While Desktop 2 shows, Desktop 1's cards cannot merge, nor with Desktop 2's.
    let obs = mac.obs();
    assert!(mac.m.merge_stages(d1_cards[0], d1_cards[1], &obs).is_empty());
    assert!(mac.m.merge_stages(d1_cards[0], d2_card, &obs).is_empty());
    assert!(mac.m.merge_stages(d2_card, d1_cards[1], &obs).is_empty());
    mac.go(0);
    assert_eq!(mac.space(), d1);
    let obs = mac.obs();
    let a = mac.m.merge_stages(d1_cards[0], d1_cards[1], &obs);
    let a = mac.run(a);
    assert_eq!(a, [Action::StateChanged]);
    assert!(mac.elsewhere.is_empty(), "nothing on Desktop 2 touched: {:?}", mac.elsewhere);
    assert_eq!(mac.strip().len(), 1, "one card left on Desktop 1");
    assert_eq!(mac.strip()[0].len(), 2, "holding both stages' windows");
    assert_eq!(mac.m.stages_in(d2).count(), 2, "Desktop 2's stages untouched");
    assert!(mac.m.stage(d1_cards[1]).is_some_and(|s| s.space == d1));
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
    assert!(!mac.m.is_sticky(w(7)), "seen back where it came from: maybe moved there and straight back");
    mac.go(1);
    assert!(mac.m.is_sticky(w(7)), "on every Desktop gone to, three times in a row: on every Desktop");
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
        mac.wins.get_mut(&id).expect("window").0 = Some(HIDDEN);
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
    let json = file.to_json().expect("encodes");

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
    // The editor by app alone (low) into the saved active stage; the browser's only
    // candidate is in an inactive stage: no low pick there (M3 of the 8E review), so it goes
    // by Apple's rule at a launch (a stage of its own, a card), its saved window still
    // pending in the saved stage (no card).
    assert_eq!(again.m.stages().len(), 3);
    assert_eq!(again.m.stage_of(w(21)), again.m.main_active_stage());
    assert_eq!(again.m.restored_grade(w(22)), None);
    assert!(again.strip().iter().any(|card| card == &vec![22]), "{:?}", again.strip());
    assert_eq!(again.m.pending_count(), 2, "both saved windows still awaited: low matches leave them waiting");
}

#[test]
fn a_first_launch_on_another_desktop_does_not_take_the_saved_current_one() {
    // V3: quit on Desktop 2, titles changed, relaunched on Desktop 1.
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    let d1 = mac.space();
    mac.go(1);
    let d2 = mac.space();
    let file = mac.m.to_state();
    let mut again = Mac::new();
    again.m.restore_spaces(file.spaces);
    // Desktop 1's apps (editor + browser), new titles: Desktop 1 matches more by app.
    again.put(0, 21, 10, "com.example.editor", "Renamed notes");
    again.put(0, 22, 20, "com.example.browser", "Another page");
    again.focused = Some(w(21));
    again.go(0);
    assert_ne!(again.space(), d2, "not the Desktop shown at quit");
    assert_eq!(again.m.unvisited_saved_spaces(), 2, "both saved Desktops wait for their visits");
    let _ = d1;
    // Only apps no saved Desktop has: a new Space too.
    let mut third = Mac::new();
    third.m.restore_spaces(mac.m.to_state().spaces);
    third.put(0, 31, 40, "com.example.music", "Player");
    third.go(0);
    assert_ne!(third.space(), d2);
}

#[test]
fn a_parked_window_found_outside_the_park_zone_on_arrival_is_parked_again() {
    // V1-A for a Desktop not shown: the display changed size while Desktop 1 was away; its
    // parked window now shows mid-screen when Desktop 1 comes back.
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    assert!(mac.parked(2));
    mac.go(1);
    mac.wins.get_mut(&2).expect("window").1 = Rect::new(700.0, 300.0, 600.0, 400.0);
    let a = mac.go(0);
    assert!(a.iter().any(|x| matches!(x, Action::Park { id, .. } if *id == w(2))), "{a:?}");
    assert!(mac.parked(2));
    assert!(mac.elsewhere.is_empty(), "{:?}", mac.elsewhere);
}

#[test]
fn send_and_pin_by_id_never_touch_another_desktops_window() {
    // L2: `ctl send N ID` / `ctl pin ID` naming a window of a Desktop not shown.
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    mac.go(1);
    let obs = mac.obs();
    assert!(mac.m.send_window(w(2), Target::New, &obs).is_empty());
    assert!(mac.m.toggle_pin(w(2), &obs).is_empty());
    assert!(mac.m.toggle_pin(w(1), &obs).is_empty());
    assert!(mac.elsewhere.is_empty());
    // The shown Desktop's windows still can.
    assert!(!mac.m.toggle_pin(w(3), &obs).is_empty());
}

#[test]
fn a_shown_window_found_parked_on_arrival_is_brought_back() {
    // R1-platform 9: a batch cut short while Accessibility was off left a window the model
    // counts as shown in the park zone; recognizing the Space again brings it back.
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    assert!(!mac.parked(1) && !mac.m.window(w(1)).expect("w1").parked);
    let size = mac.wins[&1].1.size();
    mac.wins.get_mut(&1).expect("window").1 =
        macos_places(crate::engine::park::park_frame(size, FRAME));
    assert!(mac.parked(1));
    let a = mac.arrive();
    assert!(a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } if *id == w(1))), "{a:?}");
    assert!(!mac.parked(1));
    assert!(mac.parked(2), "the parked stage stays parked");
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

#[test]
fn a_window_moved_to_another_desktop_and_straight_back_is_not_sticky() {
    // Mission Control: Desktop 1's browser dragged to Desktop 2, Desktop 2 clicked; then,
    // within a second or two (before the once-a-second checks forget it), dragged back
    // to Desktop 1 and Desktop 1 clicked.
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    let d1 = mac.space();
    mac.wins.get_mut(&2).expect("window").0 = Some(1);
    mac.go(1);
    assert_ne!(mac.space(), d1);
    assert!(mac.active().contains(&2), "moved here: it joins Desktop 2's active stage");
    mac.wins.get_mut(&2).expect("window").0 = Some(0);
    mac.go(0);
    assert_eq!(mac.space(), d1, "Desktop 1 still recognized");
    assert!(!mac.m.is_sticky(w(2)), "moved back, not on every Desktop");
    assert_eq!(mac.m.space_of(w(2)), Some(d1));
    assert!(matches!(mac.m.window(w(2)).map(|x| x.home), Some(Home::Stage(_))), "it lives in a stage again");
    assert!(!mac.parked(2));
    // It can be pinned and unpinned, unlike a sticky window.
    let obs = mac.obs();
    assert!(!mac.m.toggle_pin(w(2), &obs).is_empty());
    assert_eq!(mac.m.window(w(2)).map(|x| x.home), Some(Home::Pinned));
    let obs = mac.obs();
    assert!(!mac.m.toggle_pin(w(2), &obs).is_empty());
    assert!(matches!(mac.m.window(w(2)).map(|x| x.home), Some(Home::Stage(_))));
    // Desktop 2 and back: it stays Desktop 1's (its run of moves is over).
    mac.go(1);
    mac.go(0);
    assert!(!mac.m.is_sticky(w(2)));
    assert_eq!(mac.m.space_of(w(2)), Some(d1));
}

#[test]
fn a_window_in_full_screen_still_belongs_to_its_desktop() {
    // Desktop 1: the editor and the browser, two stages. The browser goes full screen (a
    // Space of its own); the user goes to Desktop 2 and back to Desktop 1.
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    let d1 = mac.space();
    let stages: Vec<StageId> = mac.m.stages_in(d1).map(|s| s.id).collect();
    assert_eq!(stages.len(), 2);
    mac.wins.get_mut(&2).expect("window").0 = Some(FULL_SCREEN);
    mac.go(1);
    let d2 = mac.space();

    // Counted missing for no visible reason (the old platform), Desktop 1 is not recognized
    // with two windows: score 1 − 1 = 0, a fresh Space, and its stages merge.
    let mut naive = mac.m.clone();
    let live = mac.wins.iter().filter(|(_, (d, _, _))| *d == Some(0)).map(|(_, x)| x.2.clone()).collect::<Vec<_>>();
    naive.arrive(MAIN, &live, &[w(2)], &LaunchHints::default());
    assert_ne!(naive.current_space(MAIN), d1, "the failure the platform now avoids");

    mac.go(0);
    assert_eq!(mac.space(), d1, "Desktop 1 keeps its id");
    let now: Vec<StageId> = mac.m.stages_in(d1).map(|s| s.id).collect();
    assert_eq!(now, stages, "its stages as they were");
    assert_eq!(mac.m.space_of(w(2)), Some(d1), "the full-screen window is still Desktop 1's");
    assert_ne!(mac.m.stage_of(w(1)), mac.m.stage_of(w(2)), "two stages, not merged");
    assert!(mac.elsewhere.is_empty());

    // Out of full screen, back on Desktop 1: the same Space, nothing re-placed.
    mac.wins.get_mut(&2).expect("window").0 = Some(0);
    mac.go(0);
    assert_eq!(mac.space(), d1);
    assert_eq!(mac.m.stages_in(d1).map(|s| s.id).collect::<Vec<_>>(), stages);
    assert_ne!(mac.space(), d2);
}

// ----- a Dock click, ⌘Tab or activation that lands on another Desktop (round 6R) ----------

mod focus_on_arrival {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::engine::focus_replay::{FOCUS_REPLAY_MAX, FocusReplay, Wanted};
    use crate::engine::overview::{Overview, OverviewWatch};

    /// The owner's case (10-12, VS Code): the editor (pid 10) has a window on each Desktop;
    /// Desktop 1 shows the browser's stage (the editor's window 1 parked in its own), Desktop 2
    /// the editor's window 3. The user is on Desktop 2.
    fn editor_on_both() -> (Mac, StageId) {
        let mut mac = two_desktops();
        mac.focused = Some(w(1));
        mac.go(0);
        let editor = mac.m.stage_of(w(1)).expect("editor stage");
        let browser = mac.m.stage_of(w(2)).expect("browser stage");
        mac.click(browser);
        assert!(mac.parked(1), "the editor's Desktop 1 window waits in its stage");
        mac.focused = Some(w(3));
        mac.go(1);
        mac.elsewhere.clear();
        (mac, editor)
    }

    /// What the controller does with a focus event: act on it, or hold it when its window's
    /// Desktop is not the one shown (or `settling`: a Space change or animation settles).
    fn focus_event(mac: &mut Mac, replay: &mut FocusReplay, id: u32, pid: Pid, settling: bool, now: Instant) {
        if settling || mac.m.waits_on_another_space(w(id)) {
            replay.hold(Wanted::Window { id: w(id), pid }, now);
            return;
        }
        let obs = mac.obs();
        let a = mac.m.window_focused(w(id), &obs);
        mac.run(a);
    }

    /// The Desktop is recognized: AX is asked for the focused window now (`ax_now`, which may
    /// be stale, wrong or missing), and the held event is replayed.
    fn arrive_with(mac: &mut Mac, replay: &mut FocusReplay, ax_now: Option<u32>, now: Instant) {
        mac.arrive();
        let held = replay.take(now);
        let obs = Observation { focused: ax_now.map(w), ..mac.obs() };
        let a = mac.m.bring_focus_forward(ax_now.map(w), held, &obs);
        mac.run(a);
    }

    #[test]
    fn focus_before_the_space_notice_brings_its_stage() {
        for ax_now in [None, Some(3)] {
            let (mut mac, editor) = editor_on_both();
            let t0 = Instant::now();
            let mut replay = FocusReplay::default();
            // macOS focuses window 1 (Desktop 1) ~40 ms before the Space change notice.
            focus_event(&mut mac, &mut replay, 1, 10, false, t0);
            assert!(mac.elsewhere.is_empty(), "nothing acts on a Desktop not shown");
            mac.focused = Some(w(1));
            mac.shown = 0;
            // AX asked at the arrival says nothing, or still the other Desktop's window.
            arrive_with(&mut mac, &mut replay, ax_now, t0 + Duration::from_millis(700));
            assert_eq!(mac.m.main_active_stage(), Some(editor), "AX now {ax_now:?}: the editor's stage shows");
            assert!(!mac.parked(1) && mac.parked(2));
            assert_eq!(mac.focused, Some(w(1)));
        }
    }

    #[test]
    fn focus_after_the_space_notice_brings_its_stage() {
        // During the settle (the notice handled, the Desktop not recognized yet): held.
        let (mut mac, editor) = editor_on_both();
        let t0 = Instant::now();
        let mut replay = FocusReplay::default();
        mac.shown = 0;
        focus_event(&mut mac, &mut replay, 1, 10, true, t0);
        mac.focused = Some(w(1));
        arrive_with(&mut mac, &mut replay, None, t0 + Duration::from_millis(300));
        assert_eq!(mac.m.main_active_stage(), Some(editor));
        // After the Desktop is recognized: it acts at once.
        let (mut mac, editor) = editor_on_both();
        let mut replay = FocusReplay::default();
        mac.shown = 0;
        arrive_with(&mut mac, &mut replay, None, t0);
        assert_ne!(mac.m.main_active_stage(), Some(editor), "nothing said what the user came for yet");
        focus_event(&mut mac, &mut replay, 1, 10, false, t0 + Duration::from_millis(100));
        assert_eq!(mac.m.main_active_stage(), Some(editor));
        assert!(!mac.parked(1));
    }

    #[test]
    fn focus_during_an_overview_is_replayed_once_it_has_closed() {
        // App Exposé on Desktop 2, the user clicks the editor's Desktop 1 window: its focus
        // comes while App Exposé shows, the Space change too; the arrival waits for the exit.
        let (mut mac, editor) = editor_on_both();
        let t0 = Instant::now();
        let at = |n: u64| t0 + Duration::from_millis(n);
        let mut watch = OverviewWatch::default();
        let mut replay = FocusReplay::default();
        watch.see(Some(Overview::MissionControl), t0);
        focus_event(&mut mac, &mut replay, 1, 10, false, at(400));
        // The activation of its app comes too; it does not replace the window's focus.
        replay.hold(Wanted::App(10), at(420));
        mac.shown = 0;
        mac.focused = Some(w(1));
        assert!(watch.holds_arrival(at(450)), "the notified change waits for the exit");
        watch.see(None, at(1000));
        assert!(watch.holds_arrival(at(1200)));
        assert!(!watch.holds_arrival(at(1300)));
        arrive_with(&mut mac, &mut replay, None, at(1300));
        assert_eq!(mac.m.main_active_stage(), Some(editor));
        assert!(mac.elsewhere.is_empty(), "nothing touched Desktop 2: {:?}", mac.elsewhere);
    }

    #[test]
    fn a_stale_ax_answer_never_hides_the_held_window_of_another_app() {
        // R3 2: at the arrival AX may still name the previous frontmost app's window (the
        // browser's, already in Desktop 1's active stage: nothing to do). The editor's window
        // focus held meanwhile (another app) still brings its stage. Both orders: the owner's
        // (the focus ~40 ms before the Space notice) and the focus during the settle after it.
        for settling in [false, true] {
            let (mut mac, editor) = editor_on_both();
            let t0 = Instant::now();
            let mut replay = FocusReplay::default();
            if settling {
                mac.shown = 0;
            }
            focus_event(&mut mac, &mut replay, 1, 10, settling, t0);
            assert!(mac.elsewhere.is_empty(), "nothing acts before the Desktop is recognized");
            mac.shown = 0;
            mac.focused = Some(w(2));
            arrive_with(&mut mac, &mut replay, Some(2), t0 + Duration::from_millis(650));
            assert_eq!(mac.m.main_active_stage(), Some(editor), "settling {settling}: the editor's stage shows");
            assert!(!mac.parked(1) && mac.parked(2));
            assert_eq!(mac.focused, Some(w(1)));
            assert!(mac.elsewhere.is_empty(), "nothing touched Desktop 2: {:?}", mac.elsewhere);
        }
        // AX names the editor's window itself: its stage, with nothing held at all.
        let (mut mac, editor) = editor_on_both();
        let mut replay = FocusReplay::default();
        mac.shown = 0;
        arrive_with(&mut mac, &mut replay, Some(1), Instant::now());
        assert_eq!(mac.m.main_active_stage(), Some(editor));
    }

    /// Desktop 1 with three stages, the browser's active (the editor's window 1 and the mail
    /// app's window 5 parked); the user is on Desktop 2.
    fn three_stages_on_desktop_1() -> (Mac, StageId, StageId, StageId) {
        let mut mac = two_desktops();
        mac.put(0, 5, 40, "com.example.mail", "Inbox");
        mac.focused = Some(w(1));
        mac.go(0);
        let editor = mac.m.stage_of(w(1)).expect("editor stage");
        let browser = mac.m.stage_of(w(2)).expect("browser stage");
        let mail = mac.m.stage_of(w(5)).expect("mail stage");
        assert!(editor != browser && browser != mail && mail != editor, "a stage per app");
        mac.click(browser);
        assert!(mac.parked(1) && mac.parked(5));
        mac.focused = Some(w(3));
        mac.go(1);
        mac.elsewhere.clear();
        (mac, editor, browser, mail)
    }

    #[test]
    fn an_ax_answer_that_acts_decides_over_a_held_window() {
        // AX names the editor's parked window when the Desktop is recognized: its stage
        // comes; the mail window's older focus (another app) does not override an answer
        // that acts.
        let (mut mac, editor, _, mail) = three_stages_on_desktop_1();
        let t0 = Instant::now();
        let mut replay = FocusReplay::default();
        focus_event(&mut mac, &mut replay, 5, 40, false, t0);
        mac.shown = 0;
        mac.focused = Some(w(1));
        arrive_with(&mut mac, &mut replay, Some(1), t0 + Duration::from_millis(600));
        assert_eq!(mac.m.main_active_stage(), Some(editor));
        assert_ne!(mac.m.main_active_stage(), Some(mail));
        assert!(mac.parked(5));
    }

    #[test]
    fn a_held_window_of_axs_own_app_leaves_its_answer_standing() {
        // The browser has a second window on Desktop 1, sent to a stage of its own. Its
        // focus was held; at the arrival AX names the browser's other window, in the active
        // stage: the same app, so AX's answer is the newer word and nothing changes.
        let (mut mac, _, browser, _) = three_stages_on_desktop_1();
        mac.put(0, 6, 20, "com.example.browser", "Other wiki");
        mac.shown = 0;
        mac.focused = Some(w(2));
        mac.arrive();
        let obs = mac.obs();
        let a = mac.m.send_window(w(6), Target::New, &obs);
        mac.run(a);
        let other = mac.m.stage_of(w(6)).expect("its own stage");
        assert_ne!(other, browser);
        assert_eq!(mac.m.main_active_stage(), Some(browser));
        assert!(mac.parked(6));
        mac.focused = Some(w(3));
        mac.go(1);
        mac.elsewhere.clear();

        let t0 = Instant::now();
        let mut replay = FocusReplay::default();
        focus_event(&mut mac, &mut replay, 6, 20, false, t0);
        mac.shown = 0;
        mac.focused = Some(w(2));
        arrive_with(&mut mac, &mut replay, Some(2), t0 + Duration::from_millis(600));
        assert_eq!(mac.m.main_active_stage(), Some(browser), "AX's answer stands");
        assert!(mac.parked(6));
    }

    /// The controller's "a wait ended" check (no Space change after a settle): the held
    /// focus is replayed only when its window is known and on the Desktop shown.
    fn wait_ended(mac: &mut Mac, replay: &mut FocusReplay, ax_now: Option<u32>, now: Instant) -> bool {
        if !mac.m.held_focus_acts_now(replay.peek(now)) {
            return false;
        }
        let held = replay.take(now);
        let obs = Observation { focused: ax_now.map(w), ..mac.obs() };
        let a = mac.m.bring_focus_forward(ax_now.map(w), held, &obs);
        mac.run(a);
        true
    }

    #[test]
    fn a_wait_ended_replays_a_focus_held_on_the_desktop_shown() {
        // A full-screen animation settled with no Space change after it: the editor's
        // window focus, held while it settled, is on the Desktop shown after all.
        let (mut mac, editor, browser, _) = three_stages_on_desktop_1();
        mac.shown = 0;
        mac.focused = Some(w(2));
        mac.arrive();
        assert_eq!(mac.m.main_active_stage(), Some(browser));
        let t0 = Instant::now();
        let mut replay = FocusReplay::default();
        focus_event(&mut mac, &mut replay, 1, 10, true, t0);
        assert_eq!(mac.m.main_active_stage(), Some(browser), "held while settling");
        assert!(wait_ended(&mut mac, &mut replay, None, t0 + Duration::from_millis(900)));
        assert_eq!(mac.m.main_active_stage(), Some(editor));
        assert!(!mac.parked(1));
        assert_eq!(replay.peek(t0 + Duration::from_millis(900)), None, "replayed once");
    }

    #[test]
    fn a_wait_ended_leaves_a_focus_of_another_desktop_or_an_unknown_window_held() {
        // Another Desktop's window: still held at the check, replayed on its arrival.
        let (mut mac, editor) = editor_on_both();
        let t0 = Instant::now();
        let mut replay = FocusReplay::default();
        focus_event(&mut mac, &mut replay, 1, 10, false, t0);
        assert!(!wait_ended(&mut mac, &mut replay, Some(3), t0 + Duration::from_millis(100)));
        assert!(replay.peek(t0 + Duration::from_millis(100)).is_some(), "still held");
        mac.shown = 0;
        mac.focused = Some(w(1));
        arrive_with(&mut mac, &mut replay, None, t0 + Duration::from_millis(700));
        assert_eq!(mac.m.main_active_stage(), Some(editor));

        // A window not known yet (Desktop 1 never shown since launch): held at the check,
        // found on the Desktop's first arrival, and its stage is the one that shows even
        // though AX still names the browser.
        let mut mac = two_desktops();
        mac.focused = Some(w(3));
        mac.go(1);
        assert!(mac.m.window(w(1)).is_none(), "Desktop 1's windows are not known yet");
        let mut replay = FocusReplay::default();
        replay.hold(Wanted::Window { id: w(1), pid: 10 }, t0);
        assert!(!wait_ended(&mut mac, &mut replay, Some(3), t0 + Duration::from_millis(100)));
        mac.shown = 0;
        mac.focused = Some(w(2));
        arrive_with(&mut mac, &mut replay, Some(2), t0 + Duration::from_millis(800));
        let editor = mac.m.stage_of(w(1)).expect("found");
        assert_eq!(mac.m.main_active_stage(), Some(editor), "the held window's stage shows");
        assert!(!mac.parked(1) && mac.parked(2));
        assert!(mac.elsewhere.is_empty(), "nothing touched Desktop 2: {:?}", mac.elsewhere);
    }

    #[test]
    fn a_held_activation_brings_the_apps_stage_and_an_old_one_nothing() {
        let (mut mac, editor) = editor_on_both();
        let t0 = Instant::now();
        let mut replay = FocusReplay::default();
        replay.hold(Wanted::App(10), t0);
        mac.shown = 0;
        arrive_with(&mut mac, &mut replay, None, t0 + Duration::from_millis(500));
        assert_eq!(mac.m.main_active_stage(), Some(editor), "the app's stage on the Desktop it went to");
        // Too old: it no longer says what the user is doing.
        let (mut mac, editor) = editor_on_both();
        let mut replay = FocusReplay::default();
        replay.hold(Wanted::Window { id: w(1), pid: 10 }, t0);
        mac.shown = 0;
        arrive_with(&mut mac, &mut replay, None, t0 + FOCUS_REPLAY_MAX);
        assert_ne!(mac.m.main_active_stage(), Some(editor));
    }

    #[test]
    fn a_window_of_a_desktop_shown_never_waits() {
        let (mac, _) = editor_on_both();
        assert!(!mac.m.waits_on_another_space(w(3)));
        assert!(!mac.m.waits_on_another_space(w(4)));
        assert!(mac.m.waits_on_another_space(w(1)));
        assert!(!mac.m.waits_on_another_space(w(99)), "unknown windows are the controller's to hold");
    }
}

// ----- an activation while a Desktop slides, before the notice (10-23) -------------------

mod slide_activation {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::engine::focus_replay::{FocusReplay, Wanted};
    use crate::engine::slide_hold::{SlideHold, Step, Wait, activation_waits, asks_focus, slide_listed};

    /// The owner's case (10-23, 11:57:50): the editor (pid 10, his browser that day) has a
    /// window on each Desktop. Desktop 1 shows the editor's stage (window 1); on Desktop 2 the
    /// chat's stage shows and the editor's window 3 waits parked. The user is on Desktop 2.
    fn editor_parked_on_desktop_2() -> Mac {
        let mut mac = two_desktops();
        mac.focused = Some(w(1));
        mac.go(0);
        assert!(!mac.parked(1) && mac.parked(2), "Desktop 1 shows the editor's stage");
        mac.focused = Some(w(4));
        mac.go(1);
        let chat = mac.m.stage_of(w(4)).expect("chat stage");
        mac.click(chat);
        assert!(mac.parked(3) && !mac.parked(4), "Desktop 2: the editor's window waits parked");
        mac.elsewhere.clear();
        mac
    }

    /// The window server's on-screen list as the platform reads it: the shown Desktop's
    /// windows at their frames, each moved `dx` (a slide), plus `extra`.
    fn listed(mac: &Mac, dx: f64, extra: &[(u32, Rect)]) -> Vec<(u32, Rect)> {
        let mut out: Vec<(u32, Rect)> = mac
            .wins
            .iter()
            .filter(|(id, _)| mac.on_screen(**id))
            .map(|(id, (_, f, _))| (*id, Rect::new(f.x + dx, f.y, f.w, f.h)))
            .collect();
        out.extend_from_slice(extra);
        out
    }

    /// What the controller does with an activation that would switch stages: held when it
    /// waits ([`activation_waits`]), else decided at once (`Model::app_activated`).
    fn activation(mac: &mut Mac, replay: &mut FocusReplay, pid: Pid, list: &[(u32, Rect)], ax: Option<u32>, now: Instant) -> Option<Wait> {
        let focused = asks_focus(&mac.m, pid).then_some(ax.map(w)).flatten();
        if let Some(why) = activation_waits(&mac.m, slide_listed(&mac.m, list), focused) {
            let wanted = match why {
                Wait::FocusElsewhere(id) => Wanted::Window { id, pid },
                Wait::Slides => Wanted::App(pid),
            };
            replay.hold(wanted, now);
            return Some(why);
        }
        let obs = mac.obs();
        let a = mac.m.app_activated(pid, &obs);
        mac.run(a);
        None
    }

    /// Desktop 1 recognized: AX asked now (`ax_now`), the held event replayed.
    fn arrive_on_desktop_1(mac: &mut Mac, replay: &mut FocusReplay, ax_now: Option<u32>, now: Instant) {
        mac.shown = 0;
        mac.focused = ax_now.map(w);
        mac.arrive();
        let held = replay.take(now);
        let obs = Observation { focused: ax_now.map(w), ..mac.obs() };
        let a = mac.m.bring_focus_forward(ax_now.map(w), held, &obs);
        mac.run(a);
    }

    #[test]
    fn decided_at_once_mid_slide_it_switches_on_the_desktop_being_left() {
        // The failure: the model still shows Desktop 2, so the editor's activation brought
        // its parked window 3 there and focused it; macOS slid back to Desktop 2.
        let mut mac = editor_parked_on_desktop_2();
        let obs = mac.obs();
        let a = mac.m.app_activated(10, &obs);
        assert!(a.contains(&Action::Focus { id: w(3) }), "{a:?}");
    }

    #[test]
    fn an_activation_mid_slide_is_held_and_resolved_on_the_new_desktop() {
        let mut mac = editor_parked_on_desktop_2();
        let t0 = Instant::now();
        let mut replay = FocusReplay::default();
        // Mid-slide toward Desktop 1: Desktop 2's windows listed moved aside, Desktop 1's
        // window 1 listed sliding in.
        let one = Rect::new(-300.0, 80.0, 600.0, 400.0);
        let list = listed(&mac, 1240.0, &[(1, one)]);
        let why = activation(&mut mac, &mut replay, 10, &list, None, t0);
        assert_eq!(why, Some(Wait::Slides));
        assert!(mac.parked(3) && !mac.parked(4), "nothing switched on the Desktop being left");
        assert_eq!(mac.focused, Some(w(4)), "nothing focused there");
        // The Space change: Desktop 1 recognized; macOS shows the editor's window 1 there.
        arrive_on_desktop_1(&mut mac, &mut replay, Some(1), t0 + Duration::from_millis(400));
        assert!(!mac.parked(1) && mac.parked(2), "Desktop 1 as macOS shows it");
        assert!(mac.elsewhere.is_empty(), "Desktop 2 never touched: {:?}", mac.elsewhere);
        mac.shown = 1;
        assert!(mac.parked(3) && !mac.parked(4), "Desktop 2 as it was left");
    }

    #[test]
    fn an_activation_whose_window_is_on_another_desktop_is_held_before_any_slide_shows() {
        // 12:00:29: the activation came before the list showed the slide; the app's focused
        // window says where it goes. Desktop 1 shows the browser, the editor parked there:
        // on arrival the editor's stage comes (Apple's rule).
        let mut mac = editor_parked_on_desktop_2();
        mac.shown = 0;
        let browser = mac.m.stage_of(w(2)).expect("browser stage");
        mac.arrive();
        mac.click(browser);
        mac.focused = Some(w(4));
        mac.go(1);
        assert!(mac.parked(3));
        mac.elsewhere.clear();
        let t0 = Instant::now();
        let mut replay = FocusReplay::default();
        let list = listed(&mac, 0.0, &[]);
        assert!(!slide_listed(&mac.m, &list), "no slide listed yet");
        let why = activation(&mut mac, &mut replay, 10, &list, Some(1), t0);
        assert_eq!(why, Some(Wait::FocusElsewhere(w(1))));
        assert!(mac.parked(3) && mac.elsewhere.is_empty(), "nothing switched on Desktop 2");
        arrive_on_desktop_1(&mut mac, &mut replay, Some(1), t0 + Duration::from_millis(300));
        assert!(!mac.parked(1) && mac.parked(2), "the editor's stage on Desktop 1");
        mac.shown = 1;
        assert!(mac.parked(3) && !mac.parked(4), "Desktop 2 as it was left");
    }

    #[test]
    fn without_a_slide_an_activation_switches_at_once_as_before() {
        // AX names the editor's window here (or nothing): switched at once.
        for ax in [Some(3), None] {
            let mut mac = editor_parked_on_desktop_2();
            let t0 = Instant::now();
            let mut replay = FocusReplay::default();
            let list = listed(&mac, 0.0, &[]);
            assert!(!slide_listed(&mac.m, &list), "Desktop 2's windows where they are, parked in the zone");
            let why = activation(&mut mac, &mut replay, 10, &list, ax, t0);
            assert_eq!(why, None, "AX {ax:?}");
            assert!(!mac.parked(3), "the editor's stage on the Desktop shown");
            assert_eq!(replay.peek(t0), None, "nothing held");
            assert!(mac.elsewhere.is_empty());
        }
        // An app with no window on another Desktop is never asked.
        let mac = editor_parked_on_desktop_2();
        assert!(asks_focus(&mac.m, 10));
        assert!(!asks_focus(&mac.m, 30), "the chat lives on Desktop 2 only");
    }

    #[test]
    fn the_desktop_being_lefts_parked_windows_slid_together_are_a_slide() {
        let mac = editor_parked_on_desktop_2();
        let three = mac.wins[&3].1;
        let parked_only = |dx: f64| vec![(3, Rect::new(three.x + dx, three.y, three.w, three.h))];
        assert!(!slide_listed(&mac.m, &parked_only(0.0)), "in the zone: no slide");
        assert!(slide_listed(&mac.m, &parked_only(-700.0)), "moved sideways out of the zone");
        assert!(!slide_listed(&mac.m, &[]), "none listed (a hidden app's): no slide");
    }

    #[test]
    fn a_held_activation_with_no_space_change_is_decided_after_the_hold() {
        let mut mac = editor_parked_on_desktop_2();
        let t0 = Instant::now();
        let mut replay = FocusReplay::default();
        let one = Rect::new(-300.0, 80.0, 600.0, 400.0);
        let list = listed(&mac, 1240.0, &[(1, one)]);
        assert_eq!(activation(&mut mac, &mut replay, 10, &list, None, t0), Some(Wait::Slides));
        let hold = SlideHold { wanted: Wanted::App(10), activation: true, came: t0, changes: 0 };
        assert_eq!(hold.step(t0 + Duration::from_millis(200), 0, || true), Step::Wait);
        // The slide given up: the list shows Desktop 2 as it was; decided as before.
        let back = listed(&mac, 0.0, &[]);
        let now = t0 + Duration::from_millis(520);
        assert_eq!(hold.step(now, 0, || slide_listed(&mac.m, &back)), Step::Decide);
        assert_eq!(replay.take(now), Some(Wanted::App(10)));
        let obs = mac.obs();
        let a = mac.m.app_activated(10, &obs);
        mac.run(a);
        assert!(!mac.parked(3), "the editor's stage on Desktop 2, as an activation always did");
    }
}

// ----- the Desktop arranged ahead of its arrival (round 6T) --------------------------------

mod arranged_ahead {
    use super::*;

    /// The owner's case of 10-13 (as `focus_on_arrival::editor_on_both`): Desktop 1 shows the
    /// browser's stage, the editor's window 1 waits parked in its own; the user is on
    /// Desktop 2 and goes to window 1 (Dock click, ⌘Tab).
    fn editor_on_both() -> (Mac, StageId, StageId) {
        let mut mac = two_desktops();
        mac.focused = Some(w(1));
        mac.go(0);
        let editor = mac.m.stage_of(w(1)).expect("editor stage");
        let browser = mac.m.stage_of(w(2)).expect("browser stage");
        mac.click(browser);
        assert!(mac.parked(1), "the editor's Desktop 1 window waits in its stage");
        mac.focused = Some(w(3));
        mac.go(1);
        mac.elsewhere.clear();
        (mac, editor, browser)
    }

    /// Every managed window is where the model says: parked ones in the park zone, shown
    /// ones out of it (on whichever Desktop).
    fn consistent(mac: &Mac) {
        for (id, (_, frame, _)) in &mac.wins {
            if let Some(st) = mac.m.window(w(*id)) {
                assert_eq!(st.parked, parked_at(*frame), "window {id}: the model says parked={}, its frame {frame}", st.parked);
            }
        }
    }

    fn moves(actions: &[Action]) -> Vec<Action> {
        actions.iter().copied().filter(|a| matches!(a, Action::SetFrame { .. } | Action::Park { .. })).collect()
    }

    fn raises_or_focus(actions: &[Action]) -> bool {
        actions.iter().any(|a| matches!(a, Action::Raise { .. } | Action::Focus { .. }))
    }

    /// The arrival as the controller runs it: recognized, then the arrangement confirmed
    /// (or undone), then the window the Desktop was shown for; every move it makes.
    fn arrival(mac: &mut Mac, ahead: Option<&Ahead>, held: Option<Wanted>) -> Vec<Action> {
        let mut all = mac.arrive();
        if let Some(a) = ahead {
            let more = if mac.space() == a.space { mac.m.finish_ahead(a) } else { mac.m.undo_ahead(a) };
            all.extend(mac.run(more));
        }
        let obs = mac.obs();
        let a = mac.m.bring_focus_forward(mac.focused, held, &obs);
        all.extend(mac.run(a));
        all
    }

    use crate::engine::focus_replay::Wanted;
    use crate::engine::model::Ahead;

    #[test]
    fn arranged_at_the_space_notice_the_desktop_arrives_with_nothing_left_to_move() {
        let (mut mac, editor, browser) = editor_on_both();
        let slot_of_editor = mac.m.slots[&mac.m.space_of(w(1)).expect("space")].iter().position(|s| *s == editor);
        // The notice: Desktop 1 slides in (macOS lists its windows), the model still says 2.
        mac.shown = 0;
        mac.focused = Some(w(1));
        let (ahead, actions) = mac.m.arrange_ahead(w(1), None).expect("arranged");
        assert_eq!((ahead.window, ahead.stage, ahead.previous), (w(1), editor, browser));
        assert!(!raises_or_focus(&actions), "moves only: {actions:?}");
        assert_eq!(actions.last(), Some(&Action::StateChanged));
        mac.run(actions);
        assert!(!mac.parked(1) && mac.parked(2), "the editor's window shows, the browser's parked");
        assert_eq!(mac.m.active_stage_in(ahead.space), Some(editor));
        assert_ne!(mac.m.main_active_stage(), Some(editor), "Desktop 2 is still the one shown for the model");

        let at = arrival(&mut mac, Some(&ahead), Some(Wanted::Window { id: w(1), pid: 10 }));
        assert!(moves(&at).is_empty(), "nothing moves at the arrival: {at:?}");
        assert!(at.contains(&Action::Focus { id: w(1) }), "the window is focused once its Desktop shows");
        assert_eq!(mac.m.main_active_stage(), Some(editor));
        assert_eq!(mac.focused, Some(w(1)));
        let slots = &mac.m.slots[&ahead.space];
        assert_eq!(slots.iter().position(|s| *s == browser), slot_of_editor, "the browser's card took the editor's slot");
        consistent(&mac);
    }

    #[test]
    fn arranged_at_the_focus_it_moves_only_the_predicted_desktops_windows() {
        let (mut mac, editor, _) = editor_on_both();
        let (ahead, actions) = mac.m.arrange_ahead(w(1), None).expect("arranged");
        mac.run(actions);
        assert!(!raises_or_focus(&mac.elsewhere), "never a raise or focus on a Desktop not shown: {:?}", mac.elsewhere);
        let touched: Vec<WindowId> = mac
            .elsewhere
            .iter()
            .filter_map(|a| match *a {
                Action::SetFrame { id, .. } | Action::Park { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        assert!(touched.iter().all(|id| [w(1), w(2)].contains(id)), "Desktop 1's windows only: {touched:?}");
        consistent(&mac);
        // The slide ends on Desktop 1: nothing left to move.
        mac.shown = 0;
        mac.focused = Some(w(1));
        let at = arrival(&mut mac, Some(&ahead), Some(Wanted::Window { id: w(1), pid: 10 }));
        assert!(moves(&at).is_empty(), "{at:?}");
        assert_eq!(mac.m.main_active_stage(), Some(editor));
        consistent(&mac);
    }

    #[test]
    fn a_wrong_prediction_is_undone_and_the_desktop_comes_back_as_it_was() {
        let (mut mac, editor, browser) = editor_on_both();
        let space = mac.m.space_of(w(1)).expect("space");
        let slots = mac.m.slots[&space].clone();
        let key = mac.m.stage(editor).and_then(|s| s.key);
        let (ahead, actions) = mac.m.arrange_ahead(w(1), None).expect("arranged");
        mac.run(actions);
        // macOS stays on Desktop 2 (or another focus comes): undone.
        let undo = mac.m.undo_ahead(&ahead);
        assert!(!raises_or_focus(&undo), "{undo:?}");
        assert!(!moves(&undo).is_empty());
        mac.run(undo);
        assert_eq!(mac.m.active_stage_in(space), Some(browser));
        assert_eq!(mac.m.slots[&space], slots, "the strip's slots as they were");
        assert_eq!(mac.m.stage(editor).and_then(|s| s.key), key);
        assert!(mac.parked(1) && !mac.parked(2));
        consistent(&mac);
        // Undone once; a second time changes nothing.
        assert!(mac.m.undo_ahead(&ahead).is_empty());
        // Desktop 1 later: as it was, nothing to fix.
        mac.focused = Some(w(2));
        let at = mac.go(0);
        assert!(moves(&at).is_empty(), "{at:?}");
        assert_eq!(mac.m.main_active_stage(), Some(browser));
        consistent(&mac);
    }

    #[test]
    fn moves_macos_did_not_carry_out_are_matched_at_the_arrival() {
        // A Desktop not shown whose windows macOS leaves where they are.
        for predicted in [true, false] {
            let (mut mac, editor, browser) = editor_on_both();
            mac.still_elsewhere = true;
            let (ahead, actions) = mac.m.arrange_ahead(w(1), None).expect("arranged");
            mac.run(actions);
            assert!(mac.parked(1) && !mac.parked(2), "nothing moved");
            if !predicted {
                // Undone while still away: the model is back to what the screen shows.
                let undo = mac.m.undo_ahead(&ahead);
                mac.run(undo);
                consistent(&mac);
            }
            // Desktop 1 comes, for window 1 (predicted) or another way (the browser in front).
            mac.shown = 0;
            mac.focused = Some(w(if predicted { 1 } else { 2 }));
            let ahead = predicted.then_some(&ahead);
            arrival(&mut mac, ahead, None);
            consistent(&mac);
            let want = if predicted { editor } else { browser };
            assert_eq!(mac.m.main_active_stage(), Some(want), "predicted {predicted}");
        }
    }

    #[test]
    fn another_desktop_arriving_undoes_it_and_a_shown_desktop_is_left_as_it_is() {
        // Arranged for Desktop 1, but Desktop 2 is recognized (the slide went elsewhere).
        let (mut mac, _, browser) = editor_on_both();
        let (ahead, actions) = mac.m.arrange_ahead(w(1), None).expect("arranged");
        mac.run(actions);
        let at = arrival(&mut mac, Some(&ahead), None);
        let off: Vec<&Action> =
            at.iter().filter(|a| matches!(a, Action::Raise { .. } | Action::Focus { .. }) && !mac.on_screen(id_of(a))).collect();
        assert!(off.is_empty(), "never a raise or focus on a Desktop not shown: {off:?}");
        assert_eq!(mac.m.active_stage_in(ahead.space), Some(browser));
        consistent(&mac);

        // Arrived as predicted: undoing then changes nothing (what shows is right).
        let (mut mac, editor, _) = editor_on_both();
        let (ahead, actions) = mac.m.arrange_ahead(w(1), None).expect("arranged");
        mac.run(actions);
        mac.shown = 0;
        mac.focused = Some(w(1));
        arrival(&mut mac, Some(&ahead), None);
        assert!(mac.m.undo_ahead(&ahead).is_empty());
        assert_eq!(mac.m.main_active_stage(), Some(editor));
    }

    fn id_of(a: &Action) -> u32 {
        match *a {
            Action::SetFrame { id, .. } | Action::Park { id, .. } | Action::Raise { id } | Action::Focus { id } => id.0,
            Action::StateChanged => 0,
        }
    }

    #[test]
    fn arranged_at_the_notice_the_stage_left_keeps_the_order_the_user_left_it_in() {
        // Desktop 1's browser stage holds two windows; the user brought window 6 to its front
        // before leaving. At the notice the window list shows that order: it is saved.
        let mut mac = two_desktops();
        mac.put(0, 6, 20, "com.example.browser", "Other wiki");
        mac.focused = Some(w(1));
        mac.go(0);
        let browser = mac.m.stage_of(w(2)).expect("browser stage");
        assert_eq!(mac.m.stage_of(w(6)), Some(browser), "one stage per app");
        mac.click(browser);
        assert_eq!(mac.m.stage(browser).and_then(|s| s.key), Some(w(2)), "window 2 is its key so far");
        mac.focused = Some(w(3));
        mac.go(1);
        mac.shown = 0;
        mac.focused = Some(w(1));
        let obs = Observation { frames: Default::default(), z_order: vec![w(6), w(2), w(1)], focused: None };
        let (_, actions) = mac.m.arrange_ahead(w(1), Some(&obs)).expect("arranged");
        mac.run(actions);
        let st = mac.m.stage(browser).expect("browser stage");
        assert_eq!(st.key, Some(w(6)), "the front window as the user left it is the key");
        assert_eq!(st.windows.first(), Some(&w(6)));
        assert!(mac.parked(2) && mac.parked(6));
    }

    #[test]
    fn at_the_arrival_the_prediction_beats_a_stale_ax_answer() {
        // AX still names the browser's window (the stage parked ahead) at the recognition.
        let (mut mac, editor, browser) = editor_on_both();
        mac.shown = 0;
        let (ahead, actions) = mac.m.arrange_ahead(w(1), None).expect("arranged");
        mac.run(actions);
        mac.arrive();
        let fin = mac.m.finish_ahead(&ahead);
        mac.run(fin);
        let held = Some(Wanted::Window { id: w(1), pid: 10 });
        // Taken at its word, AX would switch back to the stage parked ahead:
        let mut stale = mac.m.clone();
        let obs = Observation { focused: Some(w(2)), ..mac.obs() };
        stale.bring_focus_forward(Some(w(2)), held, &obs);
        assert_eq!(stale.main_active_stage(), Some(browser), "the hazard the controller avoids");
        // The controller asks with the predicted window as the focused one: nothing changes.
        let obs = Observation { focused: Some(w(1)), ..mac.obs() };
        assert!(mac.m.bring_focus_forward(Some(w(1)), held, &obs).is_empty());
        assert_eq!(mac.m.main_active_stage(), Some(editor));
    }

    #[test]
    fn the_window_gone_before_the_arrival_brings_back_the_stage_that_was_there() {
        let (mut mac, _, browser) = editor_on_both();
        let space = mac.m.space_of(w(1)).expect("space");
        let slots = mac.m.slots[&space].clone();
        mac.shown = 0;
        let (ahead, actions) = mac.m.arrange_ahead(w(1), None).expect("arranged");
        mac.run(actions);
        // The editor's window closes during the slide.
        let a = mac.m.remove_window(w(1));
        mac.run(a);
        mac.wins.remove(&1);
        mac.focused = None;
        mac.arrive();
        let fin = mac.m.finish_ahead(&ahead);
        assert!(!moves(&fin).is_empty());
        mac.run(fin);
        assert_eq!(mac.m.main_active_stage(), Some(browser));
        assert!(!mac.parked(2));
        assert_eq!(mac.m.slots[&space].iter().filter(|s| slots.contains(s)).count(), mac.m.slots[&space].len());
        consistent(&mac);
    }

    #[test]
    fn a_childs_focus_arranges_its_parents_stage() {
        let mut mac = two_desktops();
        let mut dialog = child(7, 10, Some(1));
        dialog.frame = Rect::new(300.0, 200.0, 300.0, 200.0);
        mac.wins.insert(7, (Some(0), dialog.frame, dialog));
        mac.focused = Some(w(1));
        mac.go(0);
        let editor = mac.m.stage_of(w(1)).expect("editor stage");
        let browser = mac.m.stage_of(w(2)).expect("browser stage");
        mac.click(browser);
        assert!(mac.parked(1) && mac.parked(7), "the dialog waits with its window");
        mac.focused = Some(w(3));
        mac.go(1);
        let (ahead, actions) = mac.m.arrange_ahead(w(7), None).expect("arranged for the dialog's window");
        assert_eq!((ahead.window, ahead.stage), (w(1), editor));
        mac.run(actions);
        assert!(!mac.parked(1) && !mac.parked(7) && mac.parked(2));
        consistent(&mac);
    }

    #[test]
    fn an_undo_changes_nothing_once_the_stage_left_is_gone() {
        let (mut mac, editor, _) = editor_on_both();
        let (ahead, actions) = mac.m.arrange_ahead(w(1), None).expect("arranged");
        mac.run(actions);
        // The browser quits meanwhile: its (parked) stage goes.
        let a = mac.m.remove_window(w(2));
        mac.run(a);
        mac.wins.remove(&2);
        assert!(mac.m.undo_ahead(&ahead).is_empty());
        assert_eq!(mac.m.active_stage_in(ahead.space), Some(editor), "the window arranged for stays shown");
        consistent(&mac);
    }

    /// Desktop 1: the editor (window 1), the browser (2, active) and mail (5, parked);
    /// Desktop 2 shown: the editor's window 3 and chat.
    fn mail_parked_on_desktop_1() -> Mac {
        let mut mac = two_desktops();
        mac.put(0, 5, 40, "com.example.mail", "Inbox");
        mac.focused = Some(w(1));
        mac.go(0);
        let browser = mac.m.stage_of(w(2)).expect("browser stage");
        mac.click(browser);
        assert!(mac.parked(5));
        mac.focused = Some(w(3));
        mac.go(1);
        mac.elsewhere.clear();
        mac
    }

    #[test]
    fn a_dock_click_predicts_the_window_macos_will_show_on_another_desktop() {
        use crate::engine::model::AppFacts;
        let mut mac = mail_parked_on_desktop_1();
        let none = AppFacts::default();
        assert_eq!(mac.m.dock_arrival(40, none, Some(w(5))), Ok(w(5)), "its focused window");
        assert_eq!(mac.m.dock_arrival(40, none, None), Ok(w(5)), "no answer: its stage's key window");
        assert_eq!(mac.m.dock_arrival(40, none, Some(w(4))), Ok(w(5)), "an answer of another app's window: the key");
        assert!(mac.m.dock_arrival(99, none, None).is_err(), "not managed (not running)");
        assert!(mac.m.dock_arrival(40, AppFacts { hidden: true, ..none }, Some(w(5))).is_err(), "hidden");
        assert!(mac.m.dock_arrival(40, AppFacts { unplaced: true, ..none }, None).is_err(), "no answer, unplaced windows");
        assert!(mac.m.dock_arrival(40, AppFacts { visible_here: true, ..none }, None).is_err(), "no answer, a window on screen here");
        let mut off = mac.m.clone();
        off.set_settings(crate::engine::settings::NativeSettings { switch_to_app_space: false, ..Default::default() });
        assert!(off.dock_arrival(40, none, Some(w(5))).is_err(), "switching to the app's Space is off");
        let mut everywhere = mail_parked_on_desktop_1();
        everywhere.put(0, 6, 40, "com.example.mail", "Sent");
        everywhere.go(0);
        everywhere.go(1);
        if let Some(s) = everywhere.m.windows.get_mut(&w(6)) {
            s.sticky = true;
        }
        assert_eq!(
            everywhere.m.dock_arrival(40, none, Some(w(5))),
            Err("the inactive app has a window on every Desktop: macOS activates it here"),
            "a window on every Desktop says so, not \"on the Desktop shown\""
        );
        // A mail window opens on the Desktop shown: with no answer, macOS stays.
        mac.put(1, 7, 40, "com.example.mail", "Drafts");
        mac.go(1);
        assert!(mac.m.dock_arrival(40, none, None).is_err(), "no answer and a window on the Desktop shown");
        assert!(
            mac.m.dock_arrival(40, none, Some(w(5))).is_err(),
            "inactive with a window here: macOS activates it here, whatever its answer (run go10)"
        );
        assert!(mac.m.dock_arrival(40, none, Some(w(7))).is_err(), "its answer names the window here");
    }

    #[test]
    fn the_owners_case_an_app_with_a_window_on_each_desktop() {
        use crate::engine::model::AppFacts;
        // The editor (pid 10): window 1 parked on Desktop 1, window 3 shown on Desktop 2.
        let (mac, _, _) = editor_on_both();
        let none = AppFacts::default();
        assert!(
            mac.m.dock_arrival(10, none, Some(w(1))).is_err(),
            "another app active, the editor's focused window on Desktop 1: macOS stays here (his log 09:55:50, run go10)"
        );
        assert!(mac.m.dock_arrival(10, none, Some(w(3))).is_err(), "its focused window here: macOS stays");
        assert!(mac.m.dock_arrival(10, none, None).is_err(), "no answer, a window here: on the safe side, none");
        let active = AppFacts { active: true, ..none };
        assert_eq!(
            mac.m.dock_arrival(10, active, Some(w(3))),
            Ok(w(1)),
            "the editor active (its focused window here): the click goes to its other Desktop (10-13)"
        );
        assert!(mac.m.dock_arrival(10, AppFacts { unplaced: true, ..active }, Some(w(3))).is_err(), "active, windows unplaced");
        assert!(mac.m.dock_arrival(30, active, None).is_err(), "chat lives only here: nothing elsewhere");
    }

    #[test]
    fn an_active_app_on_several_other_desktops_gets_no_prediction() {
        use crate::engine::model::AppFacts;
        let mut mac = two_desktops();
        mac.put(2, 8, 10, "com.example.editor", "Other notes");
        mac.focused = Some(w(1));
        mac.go(0);
        mac.focused = Some(w(8));
        mac.go(2);
        mac.focused = Some(w(3));
        mac.go(1);
        let active = AppFacts { active: true, ..AppFacts::default() };
        assert!(mac.m.dock_arrival(10, active, Some(w(3))).is_err(), "Desktops 1 and 3: which is next is not known");
        assert_eq!(
            mac.m.dock_arrival(10, AppFacts::default(), Some(w(8))),
            Err("the inactive app has a window on the Desktop shown: macOS activates it here"),
            "not active with window 3 here: macOS activates it here, whatever its answer (run go10)"
        );
    }

    #[test]
    fn a_dock_click_arranges_the_desktop_the_slide_shows_and_the_arrival_confirms_it() {
        let mut mac = mail_parked_on_desktop_1();
        let window = mac.m.dock_arrival(40, Default::default(), Some(w(5))).expect("predicted");
        let (ahead, actions) = mac.m.arrange_ahead(window, None).expect("arranged");
        mac.run(actions);
        assert!(!mac.parked(5) && mac.parked(2), "Desktop 1 arranged before it shows");
        mac.shown = 0;
        mac.focused = Some(w(5));
        let at = arrival(&mut mac, Some(&ahead), Some(Wanted::Window { id: w(5), pid: 40 }));
        assert!(moves(&at).is_empty(), "nothing moves at the arrival: {at:?}");
        assert_eq!(mac.m.stage_of(w(5)), mac.m.main_active_stage());
        consistent(&mac);
    }

    #[test]
    fn nothing_is_arranged_when_there_is_nothing_to_arrange_or_undo() {
        let (mut mac, _, browser) = editor_on_both();
        assert!(mac.m.arrange_ahead(w(3), None).is_none(), "a window of the Desktop shown");
        assert!(mac.m.arrange_ahead(w(2), None).is_none(), "a window whose stage is already active there");
        assert!(mac.m.arrange_ahead(w(99), None).is_none(), "an unknown window");
        // The stage active there holds no window: leaving it would drop it, nothing to undo to.
        let space = mac.m.space_of(w(1)).expect("space");
        let empty = mac.m.create_stage(space, None);
        mac.m.active.insert(space, empty);
        assert!(mac.m.arrange_ahead(w(1), None).is_none());
        mac.m.active.insert(space, browser);
        assert!(mac.m.arrange_ahead(w(1), None).is_some());
    }
}

/// Desktop identity across restarts and Desktops sliding (round 6Y, 10-13): made-up apps and
/// titles standing for the owner's two Desktops.
mod identity {
    use super::*;

    /// Desktop 0 (his "Desktop 2"): five apps, a stage each. Desktop 1: seven other windows
    /// (more than Desktop 0, as his 11 against 7).
    fn his_desktops() -> Mac {
        let mut mac = Mac::new();
        mac.put(0, 1, 10, "com.example.editor", "Work notes");
        mac.put(0, 2, 20, "com.example.browser", "Work wiki");
        mac.put(0, 3, 30, "com.example.chat", "Team");
        mac.put(0, 4, 40, "com.example.mail", "Inbox");
        mac.put(0, 5, 50, "com.example.terminal", "Shell");
        mac.put(1, 11, 10, "com.example.editor", "Home notes");
        mac.put(1, 12, 60, "com.example.calendar", "Week");
        mac.put(1, 13, 70, "com.example.git", "Repo");
        mac.put(1, 14, 80, "com.example.messages", "Family");
        mac.put(1, 15, 90, "com.example.notes", "List");
        mac.put(1, 16, 91, "com.example.music", "Player");
        mac.put(1, 17, 92, "com.example.photos", "Album");
        mac
    }

    /// The stages of a Space as sets of window ids, in creation order.
    fn grouping(mac: &Mac, space: SpaceId) -> Vec<Vec<u32>> {
        mac.m.stages_in(space).map(|s| s.windows.iter().map(|x| x.0).collect()).filter(|v: &Vec<u32>| !v.is_empty()).collect()
    }

    /// An arrival whose window list shows the windows of every Desktop in `desks` (a
    /// Desktop sliding out beside the one sliding in), nothing counted missing.
    fn arrive_seeing(mac: &mut Mac, desks: &[usize]) -> Vec<Action> {
        let live: Vec<WindowMeta> = mac
            .wins
            .values()
            .filter(|(d, _, _)| d.is_some_and(|d| desks.contains(&d)))
            .map(|(_, f, m)| WindowMeta { frame: *f, ..m.clone() })
            .collect();
        let hints = LaunchHints { z_order: live.iter().map(|m| m.id).collect(), frontmost: None };
        let a = mac.m.arrive(MAIN, &live, &[], &hints);
        mac.run(a)
    }

    /// The owner's 10-13 run: both Desktops saved, a relaunch on Desktop 0, which keeps its
    /// stages; returns the Mac after the relaunch with Desktop 0 recognized.
    fn relaunched() -> (Mac, SpaceId, SpaceId, Vec<Vec<u32>>) {
        let mut mac = his_desktops();
        mac.focused = Some(w(14));
        mac.go(1);
        let d1 = mac.space();
        mac.focused = Some(w(1));
        mac.go(0);
        let d0 = mac.space();
        let before = grouping(&mac, d0);
        assert_eq!(before.len(), 5, "a stage per app on Desktop 0");
        let json = mac.m.to_state().to_json().expect("encodes");
        // Relaunch: the windows keep their ids (they are other apps' windows).
        let saved = StateFile::from_json(&json).expect("parses");
        let mut again = his_desktops();
        again.m.restore_spaces(saved.spaces);
        again.focused = Some(w(1));
        again.go(0);
        assert_eq!(again.space(), d0, "Desktop 0 recognized at launch");
        assert_eq!(grouping(&again, d0), before, "with its stages");
        (again, d0, d1, before)
    }

    #[test]
    fn a_desktop_sliding_out_beside_the_one_arriving_keeps_its_stages_and_its_id() {
        // 16:40:37: the list read at the notice held Desktop 0's 7 known windows and Desktop
        // 1's 11 first-seen ones; Desktop 1 (saved) won and Desktop 0's windows were taken
        // as moved into it; 0.3 s later Desktop 0 alone got a new id, its windows in one stage.
        let (mut mac, d0, d1, before) = relaunched();
        let strip_before = mac.m.strip_slots(MAIN);
        assert!(mac.m.shows_two_desktops(MAIN, &{
            let mut l = mac.live();
            l.extend(mac.wins.values().filter(|(d, _, _)| *d == Some(1)).map(|(_, _, m)| m.clone()));
            l
        }, &[]));
        mac.shown = 1;
        arrive_seeing(&mut mac, &[0, 1]);
        assert_eq!(mac.space(), d1, "the saved Desktop 1 is rebuilt from its first-seen windows");
        assert_eq!(mac.m.last_arrival().beside, vec![d0.0], "Desktop 0 is still on screen beside it");
        assert_eq!(mac.m.last_arrival().moved, 0);
        for id in 1..=5 {
            assert_eq!(mac.m.space_of(w(id)), Some(d0), "window {id} stays Desktop 0's");
        }
        assert_eq!(grouping(&mac, d0), before, "Desktop 0's stages untouched");
        // The slide over: Desktop 0's windows alone.
        mac.shown = 0;
        mac.arrive();
        assert_eq!(mac.space(), d0, "Desktop 0 keeps its id");
        assert_eq!(grouping(&mac, d0), before, "and its stages");
        assert_eq!(mac.m.strip_slots(MAIN), strip_before, "and its strip");
        assert_eq!(mac.m.spaces().len(), 2, "no third Desktop");
        // Saved again: two Desktops, each with its own stages.
        let file = mac.m.to_state();
        assert_eq!(file.spaces.len(), 2);
        assert!(file.spaces.iter().any(|s| s.id == d0.0 && s.stages.iter().filter(|st| !st.windows.is_empty()).count() == 5));
    }

    #[test]
    fn windows_beside_the_desktop_arriving_count_as_moved_only_at_the_second_arrival() {
        // A real move of most of a Desktop's windows (Mission Control, one by one): the same
        // windows of another Desktop stay on screen, the platform arrives again a settle
        // later, and they are taken as moved then.
        let mut mac = Mac::new();
        mac.put(0, 1, 10, "com.example.editor", "Work notes");
        mac.put(0, 2, 20, "com.example.browser", "Work wiki");
        mac.put(0, 3, 30, "com.example.chat", "Team");
        mac.put(1, 4, 40, "com.example.mail", "Inbox");
        mac.put(1, 5, 50, "com.example.terminal", "Shell");
        mac.put(1, 6, 60, "com.example.calendar", "Week");
        mac.focused = Some(w(1));
        mac.go(0);
        let d0 = mac.space();
        mac.focused = Some(w(4));
        mac.go(1);
        let d1 = mac.space();
        for id in [1, 2] {
            mac.wins.get_mut(&id).expect("window").0 = Some(1);
        }
        mac.arrive();
        assert_eq!(mac.space(), d1);
        assert_eq!(mac.m.space_of(w(1)), Some(d0), "first sight: maybe a Desktop sliding out");
        mac.arrive();
        assert_eq!(mac.space(), d1);
        assert_eq!(mac.m.space_of(w(1)), Some(d1), "seen again: moved here");
        assert_eq!(mac.m.space_of(w(2)), Some(d1));
        // Two apps: a stage each (6Y2 M3); the frontmost moved window's (1) comes forward.
        assert_ne!(mac.m.stage_of(w(1)), mac.m.stage_of(w(2)), "never one stage for two apps");
        assert!(!mac.parked(1), "shown");
        assert_eq!(mac.m.stage_of(w(1)), mac.m.main_active_stage(), "in front");
        assert!(mac.parked(2) && mac.strip().contains(&vec![2]), "the other in the strip: {:?}", mac.strip());
        assert_eq!(mac.m.space_of(w(3)), Some(d0), "the window left behind stays");
    }

    #[test]
    fn whole_stages_moving_to_a_desktop_without_windows_are_carried_over() {
        // A known Desktop's windows show under a new id (its other windows missing): every
        // stage that moved whole keeps its grouping, order and active stage; none collapses
        // into one.
        let mut mac = Mac::new();
        mac.put(0, 1, 10, "com.example.editor", "Work notes");
        mac.put(0, 2, 20, "com.example.browser", "Work wiki");
        mac.put(0, 7, 20, "com.example.browser", "Work docs");
        mac.put(0, 3, 30, "com.example.chat", "Team");
        mac.put(0, 4, 40, "com.example.mail", "Inbox");
        mac.put(0, 5, 50, "com.example.terminal", "Shell");
        mac.put(0, 6, 60, "com.example.calendar", "Week");
        mac.put(0, 8, 70, "com.example.notes", "List");
        mac.focused = Some(w(1));
        mac.go(0);
        let d0 = mac.space();
        let editor = mac.m.stage_of(w(1)).expect("stage");
        let browser = mac.m.stage_of(w(2)).expect("stage");
        let chat = mac.m.stage_of(w(3)).expect("stage");
        assert_eq!(mac.m.stage_of(w(7)), Some(browser), "the browser's two windows share a stage");
        let order: Vec<StageId> = mac.m.strip_slots(MAIN).into_iter().filter(|s| [browser, chat].contains(s)).collect();
        // Mail, terminal and calendar go to a Desktop not shown: Desktop 0 no longer looks
        // like itself (as many windows missing as on screen).
        for id in [4, 5, 6, 8] {
            mac.wins.get_mut(&id).expect("window").0 = Some(2);
        }
        mac.arrive();
        let new = mac.space();
        assert_ne!(new, d0, "not recognized: a new id");
        assert_eq!(mac.m.last_arrival().carried, 3);
        assert_eq!(mac.m.main_active_stage(), Some(editor), "the active stage stays active");
        assert_eq!(mac.m.stage_of(w(2)), Some(browser));
        assert_eq!(mac.m.stage_of(w(7)), Some(browser), "grouping kept");
        assert_eq!(mac.m.stage_of(w(3)), Some(chat));
        assert_eq!(mac.m.stages_in(new).count(), 3, "three stages, not one");
        assert_eq!(mac.m.strip_slots(MAIN), order, "strip order kept");
        assert!(!mac.parked(1) && mac.parked(2) && mac.parked(7) && mac.parked(3), "shown and parked as before");
        assert_eq!(mac.m.space_of(w(4)), Some(d0), "the others stay with their Desktop");
    }

    /// Desktop 1 (his app's Space 3): Messages alone in a stage, parked; Desktop 0: the
    /// browser active, an editor in the strip.
    fn messages_on_desktop_1() -> (Mac, SpaceId, StageId) {
        let mut mac = Mac::new();
        mac.put(0, 2, 20, "com.example.browser", "Work wiki");
        mac.put(0, 1, 10, "com.example.editor", "Work notes");
        mac.put(1, 14, 80, "com.example.messages", "Family");
        mac.put(1, 11, 10, "com.example.editor", "Home notes");
        mac.focused = Some(w(2));
        mac.go(0);
        let d0 = mac.space();
        let browser = mac.m.stage_of(w(2)).expect("stage");
        assert_eq!(mac.m.main_active_stage(), Some(browser));
        mac.focused = Some(w(11));
        mac.go(1);
        assert!(mac.parked(14), "Messages waits in Desktop 1's strip");
        (mac, d0, browser)
    }

    #[test]
    fn a_window_moved_here_in_mission_control_shows_in_a_stage_of_its_own() {
        // 16:49 (mc-move.log): Messages dragged in Mission Control from Desktop 1 to Desktop
        // 0; the arrival filed it into the browser's (active) stage, and the next switch
        // parked it with the browser.
        let (mut mac, d0, browser) = messages_on_desktop_1();
        mac.wins.get_mut(&14).expect("window").0 = Some(0);
        mac.focused = None;
        mac.go(0);
        assert_eq!(mac.space(), d0);
        assert_eq!(mac.m.last_arrival().moved, 1, "a move");
        let stage = mac.m.stage_of(w(14)).expect("stage");
        assert_ne!(stage, browser, "not filed with the browser");
        assert_eq!(mac.m.stage(stage).map(|s| s.windows.clone()), Some(vec![w(14)]), "a stage of its own");
        assert_eq!(mac.m.main_active_stage(), Some(stage), "shown in front");
        assert!(!mac.parked(14), "never parked");
        assert!(mac.parked(2), "the browser's stage went to the strip");
        assert!(mac.strip().contains(&vec![2]));
        // The next switch leaves Messages in its own card.
        let editor = mac.m.stage_of(w(1)).expect("stage");
        mac.click(editor);
        assert!(mac.strip().contains(&vec![14]), "its own card: {:?}", mac.strip());
        assert_eq!(mac.m.stage_of(w(2)), Some(browser));
        assert!(mac.elsewhere.is_empty(), "{:?}", mac.elsewhere);
    }

    #[test]
    fn the_stage_a_moved_window_puts_away_keeps_its_frames_from_the_screen() {
        // E3: the arrival's switch parked the active stage without reading its frames; a
        // browser that posts no move notice came back where it was before the user moved it.
        let (mut mac, _, browser) = messages_on_desktop_1();
        let moved = Rect::new(500.0, 150.0, 600.0, 400.0);
        mac.wins.get_mut(&2).expect("window").1 = moved;
        mac.wins.get_mut(&14).expect("window").0 = Some(0);
        mac.focused = None;
        mac.go(0);
        assert!(mac.parked(2), "the browser's stage went to the strip");
        assert_eq!(mac.m.window(w(2)).map(|x| x.restore), Some(moved));
        mac.click(browser);
        assert_eq!(mac.wins[&2].1, moved, "back where the user left it");
    }

    #[test]
    fn a_window_moved_here_joins_the_active_stage_holding_its_app() {
        let mut mac = Mac::new();
        mac.put(0, 1, 10, "com.example.editor", "Work notes");
        mac.put(0, 2, 20, "com.example.browser", "Work wiki");
        mac.put(1, 11, 10, "com.example.editor", "Home notes");
        mac.put(1, 4, 40, "com.example.mail", "Inbox");
        mac.put(1, 5, 50, "com.example.terminal", "Shell");
        mac.focused = Some(w(1));
        mac.go(0);
        let d0 = mac.space();
        let editor = mac.m.stage_of(w(1)).expect("stage");
        mac.focused = Some(w(4));
        mac.go(1);
        // Desktop 1's editor window dragged to Desktop 0, where the editor's stage is active.
        mac.wins.get_mut(&11).expect("window").0 = Some(0);
        mac.focused = None;
        mac.go(0);
        assert_eq!(mac.space(), d0);
        assert_eq!(mac.m.stage_of(w(11)), Some(editor), "with its app's window, in front");
        assert_eq!(mac.m.main_active_stage(), Some(editor));
        assert!(!mac.parked(11) && !mac.parked(1));
    }

    #[test]
    fn a_restart_finds_each_desktops_windows_by_id_whatever_their_titles() {
        // Titles change all day (editors, browsers): a restart recognizes each Desktop and
        // rebuilds its stages by the windows' ids, on its first visit too.
        let mut mac = his_desktops();
        mac.focused = Some(w(1));
        mac.go(0);
        let d0 = mac.space();
        mac.focused = Some(w(12));
        mac.go(1);
        let d1 = mac.space();
        let groups0 = grouping(&mac, d0);
        let groups1 = grouping(&mac, d1);
        let active1 = mac.active();
        let json = mac.m.to_state().to_json().expect("encodes");
        assert!(json.contains("\"window\": 14"), "window ids are saved");
        let mut again = his_desktops();
        for (_, _, m) in again.wins.values_mut() {
            m.title = format!("{} (changed)", m.title);
        }
        again.m.restore_spaces(StateFile::from_json(&json).expect("parses").spaces);
        again.focused = Some(w(1));
        again.go(0);
        assert_eq!(again.space(), d0);
        assert_eq!(grouping(&again, d0), groups0);
        again.go(1);
        assert_eq!(again.space(), d1, "Desktop 1 recognized on its first visit");
        assert_eq!(again.m.last_arrival().rebuilt, Some(d1.0));
        assert_eq!(grouping(&again, d1), groups1);
        assert_eq!(again.active(), active1, "its saved active stage");
        assert_eq!(again.m.unvisited_saved_spaces(), 0);
    }

    #[test]
    fn windows_of_several_apps_moved_together_get_a_stage_each_the_frontmost_shown() {
        // 6Y2 M3: mail and the terminal dragged together to Desktop 0 in Mission Control
        // landed in one stage. Each app gets its own; the frontmost window's comes forward.
        let mut mac = Mac::new();
        mac.put(0, 1, 10, "com.example.editor", "Work notes");
        mac.put(0, 2, 20, "com.example.browser", "Work wiki");
        mac.put(1, 4, 40, "com.example.mail", "Inbox");
        mac.put(1, 5, 50, "com.example.terminal", "Shell");
        mac.put(1, 6, 60, "com.example.calendar", "Week");
        mac.put(1, 7, 70, "com.example.notes", "List");
        mac.focused = Some(w(2));
        mac.go(0);
        let d0 = mac.space();
        mac.focused = Some(w(4));
        mac.go(1);
        for id in [4, 5] {
            mac.wins.get_mut(&id).expect("window").0 = Some(0);
        }
        mac.focused = Some(w(5));
        mac.go(0);
        assert_eq!(mac.space(), d0);
        assert_eq!(mac.m.last_arrival().moved, 2);
        let mail = mac.m.stage_of(w(4)).expect("stage");
        let shell = mac.m.stage_of(w(5)).expect("stage");
        assert_ne!(mail, shell, "a stage per app");
        assert_eq!(mac.m.stage(mail).map(|s| s.windows.clone()), Some(vec![w(4)]));
        assert_eq!(mac.m.stage(shell).map(|s| s.windows.clone()), Some(vec![w(5)]));
        assert_eq!(mac.m.main_active_stage(), Some(shell), "the frontmost moved window's stage in front");
        assert!(!mac.parked(5), "shown");
        assert!(mac.parked(4) && mac.parked(2), "the other app and the stage that was shown wait");
        assert!(mac.strip().contains(&vec![4]) && mac.strip().contains(&vec![2]), "{:?}", mac.strip());
        assert!(mac.elsewhere.is_empty(), "{:?}", mac.elsewhere);
    }

    #[test]
    fn two_windows_of_one_app_moved_together_share_a_stage() {
        let mut mac = Mac::new();
        mac.put(0, 2, 20, "com.example.browser", "Work wiki");
        mac.put(1, 4, 40, "com.example.mail", "Inbox");
        mac.put(1, 5, 40, "com.example.mail", "Draft");
        mac.put(1, 6, 60, "com.example.calendar", "Week");
        mac.put(1, 7, 70, "com.example.notes", "List");
        mac.put(1, 8, 80, "com.example.chat", "Team");
        mac.focused = Some(w(2));
        mac.go(0);
        mac.focused = Some(w(6));
        mac.go(1);
        for id in [4, 5] {
            mac.wins.get_mut(&id).expect("window").0 = Some(0);
        }
        mac.focused = None;
        mac.go(0);
        let mail = mac.m.stage_of(w(4)).expect("stage");
        assert_eq!(mac.m.stage_of(w(5)), Some(mail));
        assert_eq!(mac.m.main_active_stage(), Some(mail));
        assert!(!mac.parked(4) && !mac.parked(5) && mac.parked(2));
    }

    #[test]
    fn a_window_moved_here_joins_its_apps_stage_in_the_strip_which_comes_forward() {
        // 6Y2 L1: as a window opening here does (`native_target`), not a stage of its own.
        let mut mac = Mac::new();
        mac.put(0, 1, 10, "com.example.editor", "Work notes");
        mac.put(0, 2, 20, "com.example.browser", "Work wiki");
        mac.put(1, 11, 10, "com.example.editor", "Home notes");
        mac.put(1, 4, 40, "com.example.mail", "Inbox");
        mac.put(1, 5, 50, "com.example.terminal", "Shell");
        mac.focused = Some(w(2));
        mac.go(0);
        let d0 = mac.space();
        let editor = mac.m.stage_of(w(1)).expect("stage");
        let browser = mac.m.stage_of(w(2)).expect("stage");
        assert_eq!(mac.m.main_active_stage(), Some(browser));
        assert!(mac.parked(1), "the editor waits in the strip");
        mac.focused = Some(w(4));
        mac.go(1);
        mac.wins.get_mut(&11).expect("window").0 = Some(0);
        mac.focused = None;
        mac.go(0);
        assert_eq!(mac.space(), d0);
        assert_eq!(mac.m.stage_of(w(11)), Some(editor), "with its app's window");
        assert_eq!(mac.m.stage(editor).map(|s| s.windows.clone()), Some(vec![w(11), w(1)]), "in front");
        assert_eq!(mac.m.main_active_stage(), Some(editor), "switched to");
        assert!(!mac.parked(11) && !mac.parked(1));
        assert!(mac.parked(2) && mac.strip().contains(&vec![2]), "{:?}", mac.strip());
        assert_eq!(mac.focused, Some(w(11)));
        assert!(mac.elsewhere.is_empty(), "{:?}", mac.elsewhere);
    }

    #[test]
    fn whole_stages_are_carried_to_a_desktop_with_new_windows_too() {
        // 6Y2 M4: a window seen for the first time on the Desktop the stages moved to no
        // longer stops the carry; it is placed after, in a stage of its own.
        let mut mac = Mac::new();
        mac.put(0, 1, 10, "com.example.editor", "Work notes");
        mac.put(0, 2, 20, "com.example.browser", "Work wiki");
        mac.put(0, 7, 20, "com.example.browser", "Work docs");
        mac.put(0, 3, 30, "com.example.chat", "Team");
        mac.put(0, 4, 40, "com.example.mail", "Inbox");
        mac.put(0, 5, 50, "com.example.terminal", "Shell");
        mac.put(0, 6, 60, "com.example.calendar", "Week");
        mac.put(0, 8, 70, "com.example.notes", "List");
        mac.focused = Some(w(1));
        mac.go(0);
        let d0 = mac.space();
        let editor = mac.m.stage_of(w(1)).expect("stage");
        let browser = mac.m.stage_of(w(2)).expect("stage");
        let chat = mac.m.stage_of(w(3)).expect("stage");
        for id in [4, 5, 6, 8] {
            mac.wins.get_mut(&id).expect("window").0 = Some(2);
        }
        mac.put(0, 9, 95, "com.example.viewer", "Photo");
        mac.arrive();
        let new = mac.space();
        assert_ne!(new, d0);
        assert_eq!(mac.m.last_arrival().carried, 3, "carried though a window is new here");
        assert_eq!(mac.m.last_arrival().moved, 0, "carried windows are not counted as moved (L4)");
        assert_eq!(mac.m.main_active_stage(), Some(editor), "the active stage stays active");
        assert_eq!(mac.m.stage_of(w(2)), Some(browser));
        assert_eq!(mac.m.stage_of(w(7)), Some(browser), "grouping kept");
        assert_eq!(mac.m.stage_of(w(3)), Some(chat));
        let viewer = mac.m.stage_of(w(9)).expect("stage");
        assert!(![editor, browser, chat].contains(&viewer), "the new window in a stage of its own");
        assert_eq!(mac.m.stage(viewer).map(|s| s.windows.clone()), Some(vec![w(9)]));
        assert_eq!(mac.m.stages_in(new).count(), 4);
        assert!(!mac.parked(1) && mac.parked(2) && mac.parked(3) && mac.parked(9));
    }

    #[test]
    fn moved_counts_only_windows_not_carried() {
        // 6Y2 L4: one stage carried whole (two windows) and a window whose stage stayed.
        let mut mac = Mac::new();
        mac.put(0, 1, 10, "com.example.editor", "Work notes");
        mac.put(0, 2, 20, "com.example.browser", "Work wiki");
        mac.put(0, 7, 20, "com.example.browser", "Work docs");
        mac.put(0, 3, 30, "com.example.chat", "Team");
        mac.put(0, 4, 30, "com.example.chat", "Random");
        mac.put(0, 5, 50, "com.example.terminal", "Shell");
        mac.put(0, 6, 60, "com.example.calendar", "Week");
        mac.put(0, 8, 70, "com.example.notes", "List");
        mac.focused = Some(w(1));
        mac.go(0);
        assert_eq!(mac.m.stage_of(w(3)), mac.m.stage_of(w(4)));
        // The browser's stage (2, 7) and one of chat's two windows (3) go to a new Desktop.
        for id in [1, 4, 5, 6, 8] {
            mac.wins.get_mut(&id).expect("window").0 = Some(2);
        }
        mac.go(0);
        let note = mac.m.last_arrival().clone();
        assert_eq!(note.carried, 1, "{note:?}");
        assert_eq!(note.moved, 1, "chat's window alone: {note:?}");
    }

    #[test]
    fn one_window_matching_a_saved_desktop_is_not_a_second_desktop_on_screen() {
        // 6Y2 L2: the arrival waits only when a saved Desktop really shows beside the one
        // arriving: two of its windows, or more than half of them.
        let (mac, _d0, _d1, _) = relaunched();
        let desk1: Vec<WindowMeta> = mac.wins.values().filter(|(d, _, _)| *d == Some(1)).map(|(_, _, m)| m.clone()).collect();
        assert_eq!(desk1.len(), 7);
        let with = |n: usize| {
            let mut l = mac.live();
            l.extend(desk1.iter().take(n).cloned());
            l
        };
        assert!(!mac.m.shows_two_desktops(MAIN, &with(1), &[]), "one of seven saved windows");
        assert!(mac.m.shows_two_desktops(MAIN, &with(2), &[]), "two");
        assert!(mac.m.shows_two_desktops(MAIN, &with(7), &[]));
    }
}

/// Fable H3 (1.1.0): the shown stage carried to a new Desktop in Mission Control leaves the
/// old Desktop a fresh empty active stage; its parked stage keeps its card, and ⌘-Tab to its
/// app brings it.
#[test]
fn the_desktop_left_by_its_shown_stage_keeps_every_card() {
    let mut mac = two_desktops();
    mac.focused = Some(w(1));
    mac.go(0);
    let d1 = mac.space();
    let browser = mac.m.stage_of(w(2)).expect("stage");
    assert!(mac.parked(2), "the browser waits in the strip");
    // The editor (shown) dragged to a Desktop never visited, then the user goes there.
    mac.wins.get_mut(&1).expect("window").0 = Some(1);
    mac.go(1);
    assert_ne!(mac.m.space_of(w(1)), Some(d1), "the editor's stage went with it");
    let left = mac.m.active_stage_in(d1).expect("Desktop 1 keeps an active stage");
    assert_ne!(left, browser, "never the parked stage, whose windows stay in the corner");
    assert!(mac.m.stage(left).is_some_and(|s| s.windows.is_empty()), "an empty one, as Apple's");
    assert!(mac.m.slots.get(&d1).is_some_and(|s| s.contains(&browser)), "the browser keeps its card");
    // Back on Desktop 1: an empty screen, the browser's card, and ⌘-Tab to it works.
    mac.focused = None;
    mac.go(0);
    assert_eq!(mac.space(), d1);
    assert_eq!(mac.strip(), vec![vec![2]]);
    assert!(mac.parked(2));
    let obs = mac.obs();
    let a = mac.m.app_activated(20, &obs);
    mac.run(a);
    assert_eq!(mac.m.main_active_stage(), Some(browser), "⌘-Tab brings the browser's stage");
    assert!(!mac.parked(2));
}

/// Fable's section 4, test 1: random moves of windows between Desktops (Mission Control),
/// Desktop changes, new windows, card clicks and app activations; after each step every
/// Desktop has an active stage of its own, no active stage holds a parked window, every
/// window of an inactive stage is parked, every member lives on its stage's Desktop, and
/// every inactive stage with windows has a card (or the strip is full). It would have caught
/// H3.
mod invariants {
    use super::*;

    /// SplitMix64, as the single-Space property test's.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: usize) -> usize {
            if n == 0 { 0 } else { (self.next() % n as u64) as usize }
        }
    }

    const APPS: [(Pid, &str); 4] =
        [(10, "com.example.editor"), (20, "com.example.browser"), (30, "com.example.chat"), (40, "com.example.mail")];

    fn check(mac: &Mac, step: &str) {
        let m = &mac.m;
        let cap = m.options().strip.capacity;
        for space in m.spaces().iter().map(|s| s.id) {
            let active = m.active.get(&space).copied();
            let Some(active) = active else { panic!("{step}: {space:?} has no active stage") };
            let stage = m.stage(active).unwrap_or_else(|| panic!("{step}: active stage of {space:?} missing"));
            assert_eq!(stage.space, space, "{step}: active stage on another Desktop");
            for s in m.stages_in(space) {
                let shown = s.id == active;
                for id in &s.windows {
                    let w = m.window(*id).unwrap_or_else(|| panic!("{step}: member {id:?} missing"));
                    assert_eq!(w.space, space, "{step}: {id:?} not on its stage's Desktop");
                    if w.sticky {
                        continue;
                    }
                    assert_eq!(w.parked, !shown, "{step}: {id:?} parked {} in a stage {}", w.parked, if shown { "shown" } else { "waiting" });
                }
                if !shown && !s.windows.is_empty() {
                    let slots = m.slots.get(&space).cloned().unwrap_or_default();
                    assert!(slots.contains(&s.id) || slots.len() >= cap, "{step}: stage {:?} parked without a card", s.id);
                }
            }
        }
        // On the Desktop shown, the screen agrees with the model.
        let space = mac.space();
        for (&id, _) in mac.wins.iter().filter(|(id, _)| mac.on_screen(**id)) {
            let Some(w) = m.window(WindowId(id)) else { continue };
            if w.space == space && !w.sticky && w.meta.role == WindowRole::Standard {
                assert_eq!(mac.parked(id), w.parked, "{step}: {id} on screen parked {} but model {}", mac.parked(id), w.parked);
            }
        }
    }

    #[test]
    fn no_sequence_of_desktop_moves_strands_a_stage() {
        for seed in 0..200u64 {
            let mut rng = Rng(seed);
            let mut mac = Mac::new();
            let mut next = 1u32;
            for desk in 0..3 {
                for _ in 0..(1 + rng.below(3)) {
                    let (pid, bundle) = APPS[rng.below(APPS.len())];
                    mac.put(desk, next, pid, bundle, &format!("Doc {next}"));
                    next += 1;
                }
            }
            mac.focused = Some(w(1));
            mac.go(0);
            check(&mac, &format!("seed {seed} start"));
            let mut log: Vec<String> = Vec::new();
            for op in 0..40 {
                let ids: Vec<u32> = mac.wins.keys().copied().collect();
                match rng.below(6) {
                    0 => {
                        let d = rng.below(3);
                        log.push(format!("go {d}"));
                        mac.go(d);
                    }
                    1 => {
                        // Mission Control: a window (the focused one's whole stage, or one window)
                        // dragged to another Desktop, then the change is seen.
                        let id = ids[rng.below(ids.len())];
                        let d = rng.below(3);
                        let whole = rng.below(2) == 0;
                        let group: Vec<u32> = match mac.m.stage_of(w(id)).and_then(|s| mac.m.stage(s)) {
                            Some(s) if whole => s.windows.iter().map(|x| x.0).collect(),
                            _ => vec![id],
                        };
                        log.push(format!("move {group:?} to {d}"));
                        for g in group {
                            if let Some(x) = mac.wins.get_mut(&g) {
                                x.0 = Some(d);
                            }
                        }
                        mac.arrive();
                    }
                    2 => {
                        let (pid, bundle) = APPS[rng.below(APPS.len())];
                        log.push(format!("open {next}"));
                        mac.open(next, pid, bundle);
                        next += 1;
                    }
                    3 => {
                        let cards: Vec<StageId> = mac.m.strip().iter().map(|c| c.stage).collect();
                        if !cards.is_empty() {
                            let s = cards[rng.below(cards.len())];
                            log.push(format!("click {s:?}"));
                            mac.click(s);
                        }
                    }
                    4 => {
                        let (pid, _) = APPS[rng.below(APPS.len())];
                        log.push(format!("activate {pid}"));
                        let obs = mac.obs();
                        let a = mac.m.app_activated(pid, &obs);
                        mac.run(a);
                    }
                    _ => {
                        log.push("arrive".into());
                        mac.arrive();
                    }
                }
                check(&mac, &format!("seed {seed} op {op} after {log:?}"));
            }
        }
    }
}
