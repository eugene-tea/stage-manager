//! Window tabs (round tab-lost): a tab selected takes the place of the tab it hides.
//!
//! macOS window tabs are separate windows; the tab not selected is ordered out where it was,
//! and the tab selected takes the group's frame ([`crate::engine::hidden_tab`]). So a stage
//! holds the tab group by its selected tab: when another tab is selected, that tab becomes
//! the stage's window in the same slot of the stage (front to back), with its key, and the
//! tab no longer selected leaves the model as a hidden tab. It is never "left this Space":
//! that let it go with no stage, and a tab selected later showed over every stage switch
//! (the owner's Terminal on 11-15). Selected again, it takes the place back the same way.

use super::{Action, Home, Model, WindowId, WindowMeta, WindowRole, WindowState};

impl Model {
    /// Tab `selected` (on screen, not managed yet) took the place of managed tab `left` (the
    /// stage's window, off screen now as a hidden tab): `selected` joins `left`'s stage at
    /// `left`'s place in it, shown, `left`'s key when `left` had it, and `left` (with its
    /// children, ordered out with it) leaves the model. Nothing when `left` is not a managed
    /// standard window shown by the model, or `selected` is managed already, of another app,
    /// or not a standard window. Returns the actions and the windows the model forgot: the
    /// platform keeps them waiting, offered again once they show.
    pub fn tab_selected(&mut self, left: WindowId, selected: WindowMeta) -> (Vec<Action>, Vec<WindowId>) {
        let Some(old) = self.windows.get(&left) else { return (Vec::new(), Vec::new()) };
        if old.meta.role != WindowRole::Standard
            || old.parked
            || selected.role != WindowRole::Standard
            || selected.pid != old.meta.pid
            || self.windows.contains_key(&selected.id)
        {
            return (Vec::new(), Vec::new());
        }
        let home = old.home;
        let display = old.meta.display;
        let frame = if selected.frame.is_degenerate() || self.listed_hidden(display, selected.frame) { old.restore } else { selected.frame };
        let state = WindowState {
            meta: WindowMeta { display, ..selected },
            home,
            restore: self.safe_restore(frame, display),
            parked: false,
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
    fn nothing_changes_for_a_parked_window_another_apps_window_or_one_managed_already() {
        let (mut m, _) = desk();
        // Mail is parked (its stage was left).
        let mail_tab = WindowMeta { frame: m.window(w(12)).expect("managed").restore, ..meta(13, 12, "com.example.mail") };
        assert_eq!(m.tab_selected(w(12), mail_tab), (Vec::new(), Vec::new()), "parked: not the stage shown");
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
