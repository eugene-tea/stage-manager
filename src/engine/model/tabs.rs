//! Window tabs (round tab-lost): a tab selected takes the place of the tab it hides.
//!
//! macOS window tabs are separate windows; the tab not selected is ordered out where it was,
//! and the tab selected takes the group's frame ([`crate::engine::hidden_tab`]). So a stage
//! holds the tab group by its selected tab: when another tab is selected, that tab becomes
//! the stage's window in the same slot of the stage (front to back), with its key, and the
//! tab no longer selected leaves the model as a hidden tab. It is never "left this Space":
//! that let it go with no stage, and a tab selected later showed over every stage switch
//! (the owner's Terminal on 11-15). Selected again, it takes the place back the same way.
//!
//! The stage may be hidden (round tab-quit): a tab selected while its group waits hidden (a
//! tab closed there, or selected by its app) is hidden with the stage, and its frame to come
//! back to is the group's, the tab left's. Before, the tab left stayed the stage's window: a
//! quit put the hidden tab back and left the tab selected in the corner, for the leftovers
//! check to put at whatever frame `state.json` gave a window of its title.

use super::{Action, Home, Model, WindowId, WindowMeta, WindowRole, WindowState};

impl Model {
    /// Tab `selected` (on screen, not managed yet) took the place of managed tab `left` (the
    /// stage's window, off screen now as a hidden tab): `selected` joins `left`'s stage at
    /// `left`'s place in it, `left`'s key when `left` had it, and `left` (with its children,
    /// ordered out with it) leaves the model. Shown when `left` was shown; when `left` was
    /// hidden with its stage, `selected` is hidden too (a `Hide`: where the engine keeps it, it
    /// stays) with `left`'s frame to come back to, the group's. Nothing when `left` is not a
    /// managed standard window, or `selected` is managed already, of another app, or not a
    /// standard window. Returns the actions and the windows the model forgot: the platform
    /// keeps them waiting, offered again once they show.
    pub fn tab_selected(&mut self, left: WindowId, selected: WindowMeta) -> (Vec<Action>, Vec<WindowId>) {
        let Some(old) = self.windows.get(&left) else { return (Vec::new(), Vec::new()) };
        if old.meta.role != WindowRole::Standard
            || selected.role != WindowRole::Standard
            || selected.pid != old.meta.pid
            || self.windows.contains_key(&selected.id)
        {
            return (Vec::new(), Vec::new());
        }
        let home = old.home;
        let display = old.meta.display;
        let hidden = old.parked;
        // Hidden, it was found where the engine keeps the group (the park spot): the group's
        // frame is the tab left's.
        let frame = if hidden || selected.frame.is_degenerate() || self.listed_hidden(display, selected.frame) {
            old.restore
        } else {
            selected.frame
        };
        let state = WindowState {
            meta: WindowMeta { display, ..selected },
            home,
            restore: self.safe_restore(frame, display),
            parked: hidden,
            // The group's frame is the one `left` had: so is where it goes back at a quit.
            original: old.original,
            space: old.space,
            sticky: old.sticky,
        };
        let id = state.meta.id;
        let touched = self.touched.contains(&left);
        let mut gone = vec![left];
        gone.extend(self.children_of(left));
        let mut key = false;
        if let Home::Stage(s) = home
            && let Some(stage) = self.stage_mut(s)
        {
            let at = stage.windows.iter().position(|&w| w == left).unwrap_or(stage.windows.len());
            stage.windows.insert(at, id);
            key = stage.key == Some(left);
        }
        self.windows.insert(id, state);
        if touched {
            self.touched.insert(id);
        }
        // `left` leaves as a window gone (its stage keeps `selected`, so it never empties).
        let mut actions = self.remove_window(left);
        self.moved_from.remove(&left);
        self.hops.remove(&left);
        if key && let Home::Stage(s) = home
            && let Some(stage) = self.stage_mut(s)
        {
            stage.key = Some(id);
        }
        if hidden {
            actions.push(Action::Hide { id });
        }
        if !actions.contains(&Action::StateChanged) {
            actions.push(Action::StateChanged);
        }
        (actions, gone)
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::engine::rules::{Decision, RuleAction};
    use crate::engine::testkit::*;

    fn add(m: &mut Model, d: &mut Desk, meta: WindowMeta, action: RuleAction) {
        d.open(&meta);
        let obs = d.obs();
        let a = m.add_window(meta, &Decision { action, ..Decision::default() }, &obs);
        d.apply(m, &a);
    }

    /// Terminal (pid 10) with tabs 1 and 4 (4 hidden, not managed), Notes (pid 11) in the
    /// same stage, and Mail (pid 12) in a stage of its own, left behind.
    fn desk() -> (Model, Desk) {
        let mut m = model();
        let mut d = Desk::default();
        add(&mut m, &mut d, meta(12, 12, "com.example.mail"), RuleAction::Current);
        add(&mut m, &mut d, meta(1, 10, "com.example.terminal"), RuleAction::New);
        add(&mut m, &mut d, meta(2, 11, "com.example.notes"), RuleAction::Current);
        (m, d)
    }

    /// Tab 4 selected in tab 1's place: at tab 1's frame (the group's).
    fn tab4(m: &Model) -> WindowMeta {
        let frame = m.window(w(1)).expect("managed").restore;
        WindowMeta { frame, ..meta(4, 10, "com.example.terminal") }
    }

    both_engines! {
    #[test]
    fn a_tab_selected_takes_the_place_of_the_tab_it_hides_in_its_stage() {
        let (mut m, mut d) = desk();
        let stage = m.stage_of(w(1)).expect("in a stage");
        let before = m.stage(stage).expect("stage").windows.clone();
        let place = before.iter().position(|&x| x == w(1)).expect("member");
        let meta4 = tab4(&m);
        let (a, gone) = m.tab_selected(w(1), meta4.clone());
        d.apply(&m, &a);
        assert_eq!(gone, vec![w(1)]);
        assert!(m.window(w(1)).is_none(), "the hidden tab is not managed");
        assert_eq!(m.stage_of(w(4)), Some(stage), "the tab selected is the stage's window");
        let after = &m.stage(stage).expect("stage").windows;
        assert_eq!(after.len(), before.len());
        assert_eq!(after[place], w(4), "in the same place, front to back");
        let t = m.window(w(4)).expect("managed");
        assert!(!t.parked);
        assert_eq!(t.restore, meta4.frame);
        assert!(a.iter().all(|a| !matches!(a, Action::Hide { .. } | Action::SetFrame { .. } | Action::Focus { .. })), "nothing moves: {a:?}");
        assert!(a.contains(&Action::StateChanged));
    }

    /// The owner's 11-15 case: a switch after a tab was selected hides the tab selected
    /// with its stage; nothing of the stage left behind stays on screen.
    #[test]
    fn a_switch_after_a_tab_was_selected_hides_the_tab_selected() {
        let (mut m, mut d) = desk();
        let meta4 = tab4(&m);
        d.close(w(1));
        d.open(&meta4);
        let (a, _) = m.tab_selected(w(1), meta4);
        d.apply(&m, &a);
        let mail = m.stage_of(w(12)).expect("Mail's stage");
        let obs = d.obs();
        let a = m.switch_to(mail, &obs);
        d.apply(&m, &a);
        assert!(a.contains(&Action::Hide { id: w(4) }), "the tab selected goes with its stage: {a:?}");
        assert!(d.parked(w(4)) && d.parked(w(2)));
        assert!(!d.parked(w(12)));
    }

    /// Selected back and forth, the stage always holds the tab shown, in the same place.
    #[test]
    fn tabs_selected_back_and_forth_keep_one_place() {
        let (mut m, _) = desk();
        let stage = m.stage_of(w(1)).expect("in a stage");
        let place = m.stage(stage).expect("stage").windows.iter().position(|&x| x == w(1)).expect("member");
        let meta4 = tab4(&m);
        let meta1 = WindowMeta { frame: meta4.frame, ..meta(1, 10, "com.example.terminal") };
        let _ = m.tab_selected(w(1), meta4.clone());
        let _ = m.tab_selected(w(4), meta1);
        let _ = m.tab_selected(w(1), meta4);
        let ws = &m.stage(stage).expect("stage").windows;
        assert_eq!(ws[place], w(4));
        assert!(m.window(w(1)).is_none());
        assert_eq!(ws.len(), 2);
    }

    #[test]
    fn the_key_of_the_stage_follows_the_tab_selected() {
        let (mut m, _) = desk();
        let stage = m.stage_of(w(1)).expect("in a stage");
        m.stage_mut(stage).expect("stage").key = Some(w(1));
        let meta4 = tab4(&m);
        let _ = m.tab_selected(w(1), meta4);
        assert_eq!(m.stage(stage).expect("stage").key, Some(w(4)));
    }

    /// Round tab-quit: Mail's stage hidden (left behind), its tab 13 selected there, found at
    /// the park spot. It is Mail's stage's window, hidden, and comes back at Mail's frame (the
    /// group's), never the park spot's; a switch back shows it there, and a quit too.
    #[test]
    fn a_tab_selected_in_a_hidden_stage_is_hidden_with_it_and_comes_back_at_the_groups_frame() {
        let (mut m, mut d) = desk();
        let mail = m.stage_of(w(12)).expect("Mail's stage");
        assert!(m.window(w(12)).expect("managed").parked, "Mail's stage is hidden");
        let group = m.window(w(12)).expect("managed").restore;
        let at_spot = Rect::new(2559.0, 1408.0, group.w, group.h);
        let tab = WindowMeta { frame: at_spot, ..meta(13, 12, "com.example.mail") };
        d.close(w(12));
        d.open(&tab);
        let (a, gone) = m.tab_selected(w(12), tab);
        d.apply(&m, &a);
        assert_eq!(gone, vec![w(12)]);
        assert!(a.contains(&Action::Hide { id: w(13) }), "hidden with its stage: {a:?}");
        assert!(!a.iter().any(|a| matches!(a, Action::SetFrame { .. } | Action::Focus { .. })), "nothing shows: {a:?}");
        let t = m.window(w(13)).expect("managed");
        assert!(t.parked);
        assert_eq!(t.restore, group, "the group's frame, not the park spot's");
        assert_eq!(m.stage(mail).expect("stage").windows, vec![w(13)]);
        assert!(m.unpark_all().contains(&Action::SetFrame { id: w(13), frame: group }), "a quit puts it at the group's frame");
        let obs = d.obs();
        let a = m.switch_to(mail, &obs);
        assert!(a.contains(&Action::SetFrame { id: w(13), frame: group }), "back in front at the group's frame: {a:?}");
    }

    #[test]
    fn a_child_of_the_hidden_tab_leaves_with_it() {
        let (mut m, mut d) = desk();
        add(&mut m, &mut d, child(5, 10, Some(1)), RuleAction::Current);
        let meta4 = tab4(&m);
        let (_, mut gone) = m.tab_selected(w(1), meta4);
        gone.sort_unstable();
        assert_eq!(gone, vec![w(1), w(5)]);
        assert!(m.window(w(5)).is_none());
    }

    #[test]
    fn nothing_changes_for_another_apps_window_or_one_managed_already() {
        let (mut m, _) = desk();
        let other = WindowMeta { pid: 99, ..tab4(&m) };
        assert_eq!(m.tab_selected(w(1), other), (Vec::new(), Vec::new()), "another app");
        let notes = WindowMeta { pid: 10, ..meta(2, 10, "com.example.notes") };
        assert_eq!(m.tab_selected(w(1), notes), (Vec::new(), Vec::new()), "managed already");
        let sheet = WindowMeta { role: WindowRole::Child { parent: None }, ..tab4(&m) };
        assert_eq!(m.tab_selected(w(1), sheet), (Vec::new(), Vec::new()), "not a standard window");
        assert!(m.window(w(1)).is_some());
    }
    }
}
