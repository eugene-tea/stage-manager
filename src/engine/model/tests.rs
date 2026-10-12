//! Model tests: transitions, children, pinning, activation, reveal, strip slots, and a
//! property test that no sequence of operations loses a window.

use super::*;
use crate::engine::geometry::Point;
use crate::engine::park::{clamp_to_visible, is_parked};
use crate::engine::rules::{Decision, Placement, RuleAction};
use crate::engine::settings::AppWindows;
use crate::engine::testkit::*;

/// A model plus a fake desktop that executes every action.
struct Env {
    m: Model,
    d: Desk,
}

impl Env {
    fn new() -> Self {
        Self {
            m: model(),
            d: Desk::default(),
        }
    }

    fn with(options: ModelOptions) -> Self {
        Self {
            m: model_with(options),
            d: Desk::default(),
        }
    }

    fn run(&mut self, actions: Vec<Action>) -> Vec<Action> {
        self.d.apply(&actions);
        actions
    }

    fn add(&mut self, meta: WindowMeta, action: RuleAction) -> Vec<Action> {
        self.add_placed(meta, action, Placement::AsIs)
    }

    fn add_placed(
        &mut self,
        meta: WindowMeta,
        action: RuleAction,
        placement: Placement,
    ) -> Vec<Action> {
        self.d.open(&meta);
        let obs = self.d.obs();
        let a = self
            .m
            .add_window(meta, &Decision { action, placement }, &obs);
        self.run(a)
    }

    fn switch(&mut self, n: usize) -> Vec<Action> {
        let obs = self.d.obs();
        let a = self.m.switch_to_number(n, &obs);
        self.run(a)
    }

    fn send(&mut self, id: u32, t: Target) -> Vec<Action> {
        let obs = self.d.obs();
        let a = self.m.send_window(w(id), t, &obs);
        self.run(a)
    }

    fn close(&mut self, id: u32) -> Vec<Action> {
        self.d.close(w(id));
        let a = self.m.remove_window(w(id));
        self.run(a)
    }

    fn reveal(&mut self) -> Vec<Action> {
        let obs = self.d.obs();
        let a = self.m.reveal_desktop(&obs);
        self.run(a)
    }

    fn number_of(&self, id: u32) -> usize {
        let s = self.m.stage_of(w(id)).expect("in a stage");
        self.m.stage_number(s).expect("numbered")
    }

    fn active_number(&self) -> usize {
        self.m
            .stage_number(self.m.main_active_stage().expect("active"))
            .expect("numbered")
    }

    fn members(&self, n: usize) -> Vec<u32> {
        self.m.stages()[n - 1].windows.iter().map(|x| x.0).collect()
    }

    fn slot_numbers(&self) -> Vec<usize> {
        self.m.strip().iter().map(|c| c.number).collect()
    }
}

fn first_index(actions: &[Action], pred: impl Fn(&Action) -> bool) -> Option<usize> {
    actions.iter().position(pred)
}

// ----- construction ----------------------------------------------------------------------

#[test]
fn new_model_has_one_empty_active_stage() {
    let m = model();
    assert_eq!(m.stages().len(), 1);
    assert!(m.stages()[0].windows.is_empty());
    assert_eq!(m.main_active_stage(), Some(m.stages()[0].id));
    assert!(m.strip().is_empty());
}

#[test]
fn declared_stages_exist_up_front_and_first_is_active() {
    let m = model_with(ModelOptions {
        named_stages: vec!["Work".into(), "Chat".into()],
        ..Default::default()
    });
    let names: Vec<_> = m
        .stages()
        .iter()
        .map(|s| s.name.clone().unwrap_or_default())
        .collect();
    assert_eq!(names, ["Work", "Chat"]);
    assert_eq!(m.main_active_stage(), Some(m.stages()[0].id));
    assert_eq!(
        m.strip().len(),
        1,
        "an empty declared stage still has a card"
    );
    assert_eq!(m.strip()[0].name.as_deref(), Some("Chat"));
}

#[test]
fn no_displays_falls_back_without_panicking() {
    let m = Model::new(vec![], ModelOptions::default());
    assert_eq!(m.displays().len(), 1);
    assert!(m.main_active_stage().is_some());
}

// ----- adding windows ---------------------------------------------------------------------

#[test]
fn current_joins_the_active_stage_at_the_front() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    assert_eq!(e.members(1), [2, 1]);
    assert_eq!(e.m.stages()[0].key, Some(w(2)));
}

#[test]
fn duplicate_notifications_are_deduped() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    let mut again = meta(1, 10, "com.example.a");
    again.title = "Renamed".into();
    let a = e.add(again, RuleAction::New);
    assert!(a.is_empty());
    assert_eq!(e.m.stages().len(), 1);
    assert_eq!(e.members(1), [1]);
    assert_eq!(
        e.m.window(w(1)).map(|x| x.meta.title.as_str()),
        Some("Renamed")
    );
}

#[test]
fn ignore_leaves_the_window_unmanaged() {
    let mut e = Env::new();
    let a = e.add(meta(1, 10, "com.example.a"), RuleAction::Ignore);
    assert!(a.is_empty());
    assert!(e.m.window(w(1)).is_none());
}

#[test]
fn new_creates_a_stage_and_switches_to_it() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    assert_eq!(e.m.stages().len(), 2);
    assert_eq!(e.active_number(), 2);
    assert!(e.d.parked(w(1)));
    assert!(!e.d.parked(w(2)));
    assert_eq!(e.d.focused, Some(w(2)));
}

#[test]
fn named_stage_parks_unfocused_windows_and_follows_focused_ones() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    // Window 2 opens in the background: it is parked in "Chat".
    let m2 = meta(2, 11, "com.example.chat");
    e.d.frames.insert(m2.id, m2.frame);
    let obs = e.d.obs();
    let a = e.m.add_window(
        m2,
        &Decision {
            action: RuleAction::Stage("Chat".into()),
            placement: Placement::AsIs,
        },
        &obs,
    );
    e.run(a);
    assert_eq!(e.active_number(), 1);
    assert!(e.d.parked(w(2)));
    assert_eq!(e.m.stages()[1].name.as_deref(), Some("Chat"));
    // Window 3 of the same rule opens focused: we follow it.
    e.add(
        meta(3, 11, "com.example.chat"),
        RuleAction::Stage("chat".into()),
    );
    assert_eq!(e.m.stages().len(), 2, "names match case-insensitively");
    assert_eq!(e.active_number(), 2);
    assert!(!e.d.parked(w(2)) && !e.d.parked(w(3)) && e.d.parked(w(1)));
}

#[test]
fn center_placement_moves_the_new_window() {
    let mut e = Env::new();
    let a = e.add_placed(
        meta(1, 10, "com.example.a"),
        RuleAction::Current,
        Placement::Center,
    );
    let expected = Rect::new(420.0, 263.0, 600.0, 400.0);
    assert_eq!(
        a.first(),
        Some(&Action::SetFrame {
            id: w(1),
            frame: expected
        })
    );
    assert_eq!(e.m.window(w(1)).map(|x| x.restore), Some(expected));
}

#[test]
fn cascade_placement_offsets_from_the_front_window() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    let front = e.d.frames[&w(1)];
    let a = e.add_placed(
        meta(2, 10, "com.example.a"),
        RuleAction::Current,
        Placement::Cascade,
    );
    let expected = Rect::new(front.x + 32.0, front.y + 32.0, 600.0, 400.0);
    assert_eq!(
        a.first(),
        Some(&Action::SetFrame {
            id: w(2),
            frame: expected
        })
    );
}

#[test]
fn as_is_placement_emits_no_frame_change() {
    let mut e = Env::new();
    let a = e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    assert_eq!(a, [Action::StateChanged]);
}

#[test]
fn a_new_window_born_in_the_park_zone_gets_a_safe_restore_frame() {
    let mut e = Env::new();
    let mut m1 = meta(1, 10, "com.example.a");
    m1.frame = Rect::new(1420.0, 880.0, 600.0, 400.0);
    e.add(m1, RuleAction::Current);
    let r = e.m.window(w(1)).map(|x| x.restore).expect("managed");
    assert!(!is_parked(r, FRAME));
    assert!(!e.d.parked(w(1)), "it is brought out of the corner");
}

// ----- children ---------------------------------------------------------------------------

#[test]
fn children_follow_their_parent_and_are_not_members() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.app"), RuleAction::Current);
    e.add(child(2, 10, Some(1)), RuleAction::New);
    assert_eq!(e.m.stages().len(), 1, "a child ignores its rule");
    assert_eq!(e.members(1), [1]);
    assert_eq!(e.m.stage_of(w(2)), e.m.stage_of(w(1)));
    e.add(meta(3, 11, "com.example.b"), RuleAction::New);
    assert!(e.d.parked(w(1)) && e.d.parked(w(2)));
    let a = e.switch(1);
    assert!(!e.d.parked(w(2)));
    // The child is raised right after its parent, so it ends in front of it.
    let r1 = first_index(&a, |x| *x == Action::Raise { id: w(1) }).expect("raise 1");
    let r2 = first_index(&a, |x| *x == Action::Raise { id: w(2) }).expect("raise 2");
    assert!(r2 > r1);
}

#[test]
fn a_child_of_a_parked_parent_is_parked_at_birth() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.app"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    let c = child(3, 10, Some(1));
    e.d.frames.insert(c.id, c.frame);
    let obs = e.d.obs();
    let a = e.m.add_window(c, &Decision::default(), &obs);
    e.run(a);
    assert!(e.d.parked(w(3)));
}

#[test]
fn a_child_of_an_unmanaged_parent_stays_unmanaged() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Ignore);
    e.add(child(2, 10, Some(1)), RuleAction::Current);
    assert!(e.m.window(w(2)).is_none());
}

#[test]
fn a_child_with_no_known_parent_joins_the_active_stage() {
    let mut e = Env::new();
    e.add(child(2, 10, None), RuleAction::Current);
    assert_eq!(e.m.stage_of(w(2)), e.m.main_active_stage());
    assert!(e.members(1).is_empty());
}

#[test]
fn a_child_never_keeps_a_stage_alive() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.app"), RuleAction::Current);
    e.add(child(2, 10, None), RuleAction::Current);
    e.add(meta(3, 11, "com.example.b"), RuleAction::New);
    assert!(e.d.parked(w(2)));
    // Closing window 1 empties stage 1 except for the orphan child: the stage goes away and
    // the child comes back on the active stage rather than staying parked.
    e.close(1);
    assert_eq!(e.m.stages().len(), 1);
    assert!(!e.d.parked(w(2)));
    assert_eq!(e.m.stage_of(w(2)), e.m.main_active_stage());
}

#[test]
fn removing_a_parent_forgets_its_children_and_restores_parked_ones() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.app"), RuleAction::Current);
    e.add(meta(4, 10, "com.example.app"), RuleAction::Current);
    e.add(child(2, 10, Some(1)), RuleAction::Current);
    e.add(meta(3, 11, "com.example.b"), RuleAction::New);
    assert!(e.d.parked(w(2)));
    let a = e.m.remove_window(w(1));
    e.d.close(w(1));
    e.run(a.clone());
    assert!(e.m.window(w(2)).is_none());
    assert!(
        a.iter()
            .any(|x| matches!(x, Action::SetFrame { id, .. } if *id == w(2)))
    );
    assert!(!e.d.parked(w(2)), "a forgotten window is never left parked");
}

#[test]
fn sending_a_child_sends_its_parent() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.app"), RuleAction::Current);
    e.add(child(2, 10, Some(1)), RuleAction::Current);
    e.send(2, Target::New);
    assert_eq!(e.members(2), [1]);
    assert_eq!(e.m.stage_of(w(2)), e.m.stage_of(w(1)));
    assert!(e.d.parked(w(1)) && e.d.parked(w(2)));
}

// ----- switching --------------------------------------------------------------------------

#[test]
fn switch_parks_restores_raises_and_focuses_in_order() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    e.add(meta(3, 12, "com.example.c"), RuleAction::New);
    let a = e.switch(1);
    let last_park = a
        .iter()
        .rposition(|x| matches!(x, Action::Park { .. }))
        .expect("parks");
    let first_set = first_index(&a, |x| matches!(x, Action::SetFrame { .. })).expect("sets");
    let first_raise = first_index(&a, |x| matches!(x, Action::Raise { .. })).expect("raises");
    let last_set = a
        .iter()
        .rposition(|x| matches!(x, Action::SetFrame { .. }))
        .expect("sets");
    let focus = first_index(&a, |x| matches!(x, Action::Focus { .. })).expect("focus");
    assert!(last_park < first_set && last_set < first_raise && focus > first_raise);
    assert_eq!(a.last(), Some(&Action::StateChanged));
    assert_eq!(a[a.len() - 2], Action::Focus { id: w(2) });
    // Back to front: 1 raised before 2.
    let r1 = first_index(&a, |x| *x == Action::Raise { id: w(1) }).expect("raise 1");
    let r2 = first_index(&a, |x| *x == Action::Raise { id: w(2) }).expect("raise 2");
    assert!(r1 < r2);
    assert_eq!(e.d.z[..2], [w(2), w(1)]);
}

#[test]
fn switch_saves_live_frames_z_and_key() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    // The user moves window 1 and clicks it to the front.
    let moved = Rect::new(333.0, 222.0, 700.0, 500.0);
    e.d.frames.insert(w(1), moved);
    e.d.raise(w(1));
    e.d.focused = Some(w(1));
    e.add(meta(3, 12, "com.example.c"), RuleAction::New);
    // `add` focused 3 first, so give the switch an observation where 1 is focused.
    assert_eq!(e.m.window(w(1)).map(|x| x.restore), Some(moved));
    assert_eq!(e.members(1), [1, 2]);
    e.switch(1);
    assert_eq!(e.d.frames[&w(1)], moved);
    assert_eq!(e.d.z[..2], [w(1), w(2)]);
}

#[test]
fn switch_remembers_the_focused_window_as_key() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    e.add(meta(3, 12, "com.example.c"), RuleAction::Current);
    e.send(3, Target::New);
    let two = e.m.stages()[1].id;
    // Window 1 is focused although 2 is in front (a focused window behind a floating one).
    let obs = Observation {
        focused: Some(w(1)),
        ..e.d.obs()
    };
    let a = e.m.switch_to(two, &obs);
    e.run(a);
    assert_eq!(e.m.stages()[0].key, Some(w(1)));
    let a = e.switch(1);
    assert_eq!(a[a.len() - 2], Action::Focus { id: w(1) });
    let obs = e.d.obs();
    assert!(e.m.send_window(w(99), Target::New, &obs).is_empty());
}

#[test]
fn switch_to_the_active_or_an_unknown_stage_is_a_no_op() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    assert!(e.switch(1).is_empty());
    assert!(e.switch(7).is_empty());
    assert!(e.switch(0).is_empty());
    let obs = e.d.obs();
    assert!(e.m.switch_to(StageId(999), &obs).is_empty());
}

#[test]
fn switch_never_records_a_parked_frame_as_restore() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    let good = e.d.frames[&w(1)];
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    // A stale observation claims window 2 sits in the park zone while active.
    e.d.frames
        .insert(w(2), Rect::new(1430.0, 890.0, 600.0, 400.0));
    e.switch(1);
    assert!(!is_parked(
        e.m.window(w(2)).map(|x| x.restore).expect("w2"),
        FRAME
    ));
    assert_eq!(e.d.frames[&w(1)], good);
}

#[test]
fn leaving_an_empty_active_stage_drops_it() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.send(1, Target::New);
    assert_eq!(e.m.stages().len(), 2);
    assert!(e.members(1).is_empty(), "the active stage may be empty");
    e.switch(2);
    assert_eq!(e.m.stages().len(), 1);
    assert_eq!(e.members(1), [1]);
}

#[test]
fn pinned_windows_are_untouched_by_switches() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Float);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    let a = e.add(meta(3, 12, "com.example.c"), RuleAction::New);
    assert!(!a.iter().any(
        |x| matches!(x, Action::Park { id, .. } | Action::SetFrame { id, .. } if *id == w(1))
    ));
    assert!(!e.d.parked(w(1)));
    assert!(e.m.pinned().contains(&w(1)));
    assert!(e.m.strip().iter().all(|c| !c.windows.contains(&w(1))));
}

// ----- removal ----------------------------------------------------------------------------

#[test]
fn closing_the_last_window_of_an_inactive_stage_removes_it() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    e.close(1);
    assert_eq!(e.m.stages().len(), 1);
    assert!(e.m.strip().is_empty());
}

#[test]
fn closing_the_last_window_of_the_active_stage_keeps_it_empty() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    e.close(2);
    assert_eq!(e.m.stages().len(), 2);
    assert_eq!(e.active_number(), 2);
    assert!(e.members(2).is_empty());
}

#[test]
fn closing_the_key_window_picks_the_next_front() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    e.close(2);
    assert_eq!(e.m.stages()[0].key, Some(w(1)));
    assert!(e.m.remove_window(w(42)).is_empty());
}

#[test]
fn declared_stages_survive_being_empty() {
    let mut e = Env::with(ModelOptions {
        named_stages: vec!["Work".into(), "Chat".into()],
        ..Default::default()
    });
    e.add(
        meta(1, 10, "com.example.a"),
        RuleAction::Stage("Chat".into()),
    );
    e.switch(1);
    e.close(1);
    assert_eq!(e.m.stages().len(), 2);
}

// ----- sending ----------------------------------------------------------------------------

#[test]
fn send_to_an_existing_stage_parks_and_refocuses_the_source() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    e.add(meta(3, 12, "com.example.c"), RuleAction::Current);
    let a = e.send(3, Target::Number(1));
    assert_eq!(e.members(1), [3, 1]);
    assert_eq!(e.m.stages()[0].key, Some(w(3)));
    assert!(e.d.parked(w(3)));
    assert!(a.contains(&Action::Focus { id: w(2) }));
    assert_eq!(e.active_number(), 2, "sending does not switch");
}

#[test]
fn send_to_count_plus_one_creates_and_beyond_is_a_no_op() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    assert!(e.send(2, Target::Number(3)).is_empty());
    assert!(e.send(2, Target::Number(0)).is_empty());
    e.send(2, Target::Number(2));
    assert_eq!(e.m.stages().len(), 2);
    assert_eq!(e.number_of(2), 2);
    assert!(e.send(1, Target::Number(1)).is_empty(), "already there");
}

#[test]
fn send_the_last_window_leaves_the_active_stage_empty() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.send(1, Target::New);
    assert_eq!(e.active_number(), 1);
    assert!(e.members(1).is_empty());
    assert_eq!(e.number_of(1), 2);
}

#[test]
fn send_to_named_creates_once_and_reuses() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    e.add(meta(3, 12, "com.example.c"), RuleAction::Current);
    e.send(1, Target::Named("Mail".into()));
    e.send(2, Target::Named("Mail".into()));
    assert_eq!(e.m.stages().len(), 2);
    assert_eq!(e.members(2), [2, 1]);
    assert_eq!(e.m.stages()[1].name.as_deref(), Some("Mail"));
}

#[test]
fn send_from_an_inactive_stage_into_the_active_one_restores_it() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    e.add(meta(3, 12, "com.example.c"), RuleAction::New);
    assert!(e.d.parked(w(1)));
    let a = e.send(1, Target::Number(2));
    assert!(!e.d.parked(w(1)));
    assert!(a.contains(&Action::Raise { id: w(1) }));
    assert_eq!(e.members(1), [2]);
}

#[test]
fn send_unpins_a_pinned_window() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Float);
    e.send(1, Target::New);
    assert!(e.m.pinned().is_empty());
    assert!(e.d.parked(w(1)));
}

// ----- pinning ----------------------------------------------------------------------------

#[test]
fn toggle_pin_round_trip() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    let obs = e.d.obs();
    let a = e.m.toggle_pin(w(1), &obs);
    e.run(a);
    assert_eq!(e.m.pinned(), [w(1)]);
    assert_eq!(e.members(1), [2]);
    e.add(meta(3, 12, "com.example.c"), RuleAction::New);
    assert!(!e.d.parked(w(1)));
    let obs = e.d.obs();
    let a = e.m.toggle_pin(w(1), &obs);
    e.run(a);
    assert!(e.m.pinned().is_empty());
    assert_eq!(e.members(2), [1, 3]);
}

#[test]
fn pinning_a_parked_window_brings_it_back_and_drops_its_empty_stage() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(child(4, 10, Some(1)), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    assert!(e.d.parked(w(1)));
    let obs = e.d.obs();
    let a = e.m.toggle_pin(w(1), &obs);
    e.run(a);
    assert!(!e.d.parked(w(1)) && !e.d.parked(w(4)));
    assert_eq!(e.m.stages().len(), 1);
    assert_eq!(e.m.window(w(4)).map(|x| x.home), Some(Home::Pinned));
    let obs = e.d.obs();
    assert!(e.m.toggle_pin(w(77), &obs).is_empty());
}

// ----- activation and focus -----------------------------------------------------------------

#[test]
fn activating_an_app_in_the_active_stage_does_nothing() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 10, "com.example.a"), RuleAction::New);
    let obs = e.d.obs();
    assert!(e.m.app_activated(10, &obs).is_empty());
}

#[test]
fn activating_an_app_switches_to_its_most_recent_stage() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 10, "com.example.a"), RuleAction::New);
    e.add(meta(3, 11, "com.example.b"), RuleAction::New);
    // Stage 2 was active more recently than stage 1.
    let obs = Observation {
        focused: None,
        ..e.d.obs()
    };
    let a = e.m.app_activated(10, &obs);
    e.run(a);
    assert_eq!(e.active_number(), 2);
}

#[test]
fn activation_follows_the_focused_window_first() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 10, "com.example.a"), RuleAction::New);
    e.add(meta(3, 11, "com.example.b"), RuleAction::New);
    let obs = Observation {
        focused: Some(w(1)),
        ..e.d.obs()
    };
    let a = e.m.app_activated(10, &obs);
    e.run(a);
    assert_eq!(e.active_number(), 1);
}

#[test]
fn one_at_a_time_follows_the_focused_window_even_when_present() {
    let mut options = ModelOptions::default();
    options.settings.app_windows = AppWindows::OneAtATime;
    let mut e = Env::with(options);
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 10, "com.example.a"), RuleAction::New);
    let obs = Observation {
        focused: Some(w(1)),
        ..e.d.obs()
    };
    let a = e.m.app_activated(10, &obs);
    e.run(a);
    assert_eq!(e.active_number(), 1);
    // All at once ignores the focused window while the app is present.
    e.m.set_settings(NativeSettings::default());
    let obs = Observation {
        focused: Some(w(2)),
        ..e.d.obs()
    };
    let a = e.m.app_activated(10, &obs);
    assert!(a.iter().all(|x| *x == Action::StateChanged) || a.is_empty());
}

#[test]
fn an_app_with_a_pinned_window_does_not_pull_a_switch() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 10, "com.example.a"), RuleAction::Float);
    e.add(meta(3, 11, "com.example.b"), RuleAction::New);
    let obs = Observation {
        focused: Some(w(2)),
        ..e.d.obs()
    };
    assert!(e.m.app_activated(10, &obs).is_empty());
}

#[test]
fn focusing_a_parked_window_switches_to_its_stage() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    let obs = e.d.obs();
    let a = e.m.window_focused(w(1), &obs);
    e.run(a);
    assert_eq!(e.active_number(), 1);
    let obs = e.d.obs();
    assert!(e.m.window_focused(w(1), &obs).is_empty());
}

// ----- reveal desktop ---------------------------------------------------------------------

#[test]
fn reveal_desktop_parks_and_a_second_reveal_brings_it_back() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    e.reveal();
    assert!(e.d.parked(w(1)) && e.d.parked(w(2)));
    assert_eq!(e.m.stages().len(), 2);
    assert!(e.members(2).is_empty());
    assert_eq!(e.slot_numbers(), [1]);
    e.reveal();
    assert!(!e.d.parked(w(1)) && !e.d.parked(w(2)));
    assert_eq!(e.m.stages().len(), 1, "the empty desktop stage goes away");
    assert!(e.m.strip().is_empty());
}

#[test]
fn reveal_on_an_empty_stage_is_a_no_op() {
    let mut e = Env::new();
    assert!(e.reveal().is_empty());
}

#[test]
fn reveal_does_not_toggle_back_after_the_user_moved_on() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.reveal();
    // A new window opens on the revealed desktop: the next click reveals that one instead.
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    e.reveal();
    assert!(e.d.parked(w(1)) && e.d.parked(w(2)));
    assert_eq!(e.m.stages().len(), 3);
}

#[test]
fn reveal_then_switch_elsewhere_forgets_the_toggle() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    e.reveal(); // parks stage 2
    let one = e.m.stages()[0].id;
    let obs = e.d.obs();
    let a = e.m.switch_to(one, &obs);
    e.run(a);
    e.reveal(); // parks stage 1, a fresh desktop
    assert!(e.d.parked(w(1)) && e.d.parked(w(2)));
    e.reveal(); // back to stage 1, the one just revealed
    assert!(!e.d.parked(w(1)) && e.d.parked(w(2)));
}

// ----- frames and safety ------------------------------------------------------------------

#[test]
fn frame_changes_update_restore_frames_but_not_from_the_park_zone() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    let f = Rect::new(50.0, 60.0, 700.0, 500.0);
    e.m.window_frame_changed(w(1), f);
    assert_eq!(e.m.window(w(1)).map(|x| x.restore), Some(f));
    e.m.window_frame_changed(w(1), Rect::new(1439.0, 899.0, 700.0, 500.0));
    e.m.window_frame_changed(w(1), Rect::new(10.0, 10.0, 0.0, 0.0));
    assert_eq!(e.m.window(w(1)).map(|x| x.restore), Some(f));
    assert!(e.m.window_title_changed(w(1), "Notes 2"));
    assert!(!e.m.window_title_changed(w(5), "x"));
}

#[test]
fn unpark_all_restores_every_parked_window_clamped() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    e.add(meta(3, 12, "com.example.c"), RuleAction::New);
    let off = Rect::new(-500.0, -100.0, 600.0, 400.0);
    e.d.frames.insert(w(3), off);
    e.m.window_frame_changed(w(3), off);
    e.add(meta(4, 13, "com.example.d"), RuleAction::New);
    assert_eq!(e.m.parked_windows().len(), 3);
    let a = e.m.unpark_all();
    e.run(a.clone());
    assert_eq!(a.len(), 3);
    for id in [1, 2, 3, 4] {
        assert!(!e.d.parked(w(id)), "window {id}");
    }
    assert_eq!(
        e.d.frames[&w(3)],
        clamp_to_visible(Rect::new(-500.0, -100.0, 600.0, 400.0), VISIBLE)
    );
    assert!(e.m.parked_windows().is_empty());
}

#[test]
fn removed_display_moves_its_stages_to_main() {
    let second = Display {
        id: DisplayId(2),
        frame: Rect::new(1440.0, 0.0, 1920.0, 1080.0),
        visible: Rect::new(1440.0, 0.0, 1920.0, 1080.0),
    };
    let mut e = Env::new();
    let actions = e.m.set_displays(vec![display(), second]);
    e.run(actions);
    let mut m5 = meta(5, 20, "com.example.e");
    m5.display = DisplayId(2);
    m5.frame = Rect::new(1600.0, 100.0, 500.0, 400.0);
    e.add(m5, RuleAction::Current);
    assert_eq!(e.m.stages().len(), 2);
    let a = e.m.set_displays(vec![display()]);
    e.run(a);
    assert!(
        e.d.parked(w(5)),
        "the second display's active stage is parked on main"
    );
    assert!(e.m.stages().iter().all(|s| s.display == MAIN));
    let a = e.m.unpark_all();
    e.run(a);
    assert!(VISIBLE.contains_rect(&Rect {
        h: 28.0,
        ..e.d.frames[&w(5)]
    }));
}

#[test]
fn renamed_declarations_release_empty_stages() {
    let mut m = model_with(ModelOptions {
        named_stages: vec!["Work".into(), "Chat".into()],
        ..Default::default()
    });
    m.set_named_stages(vec!["Work".into(), "Music".into()]);
    let names: Vec<_> = m.stages().iter().filter_map(|s| s.name.clone()).collect();
    assert_eq!(names, ["Work", "Music"]);
}

// ----- strip ------------------------------------------------------------------------------

/// Builds stages 1..=n, each holding one window (IDs 1..=n), ending with stage n active.
fn stages(n: u32, capacity: usize) -> Env {
    let mut options = ModelOptions::default();
    options.strip.capacity = capacity;
    let mut e = Env::with(options);
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    for i in 2..=n {
        e.add(meta(i, 10 + i as i32, "com.example.x"), RuleAction::New);
    }
    e
}

#[test]
fn strip_lists_other_stages_never_the_active_one() {
    let e = stages(3, 6);
    assert_eq!(e.slot_numbers(), [1, 2]);
    assert!(e.m.strip().iter().all(|c| !c.active));
}

#[test]
fn clicking_a_card_swaps_slots() {
    let mut e = stages(4, 6);
    assert_eq!(e.slot_numbers(), [1, 2, 3]);
    e.switch(1);
    assert_eq!(
        e.slot_numbers(),
        [4, 2, 3],
        "the stage left takes the vacated slot"
    );
    e.switch(3);
    assert_eq!(e.slot_numbers(), [4, 2, 1]);
}

#[test]
fn a_stage_entering_a_full_strip_evicts_the_least_recently_used() {
    let mut e = stages(4, 2);
    // Stages 1..3 were active in order; the strip holds two of them.
    assert_eq!(e.slot_numbers().len(), 2);
    let before = e.slot_numbers();
    e.reveal(); // stage 4 enters with no free slot
    let after = e.slot_numbers();
    assert_eq!(after.len(), 2);
    assert!(after.contains(&4));
    // The evicted card is the one used least recently.
    let evicted: Vec<_> = before.iter().filter(|n| !after.contains(n)).collect();
    assert_eq!(evicted.len(), 1);
    let kept = before
        .iter()
        .find(|n| after.contains(n))
        .copied()
        .expect("one kept");
    let age = |n: usize| e.m.stages()[n - 1].last_active;
    assert!(age(*evicted[0]) < age(kept));
    // It takes the evicted card's slot, not the bottom.
    let pos = before.iter().position(|n| n == evicted[0]).expect("pos");
    assert_eq!(after[pos], 4);
}

#[test]
fn switching_by_hotkey_to_an_out_of_view_stage_enters_the_leaving_one() {
    let mut e = stages(4, 2);
    let out = (1..=3)
        .find(|n| !e.slot_numbers().contains(n))
        .expect("one out of view");
    e.switch(out);
    assert!(e.slot_numbers().contains(&4));
    assert!(!e.slot_numbers().contains(&out));
}

#[test]
fn a_closed_card_frees_its_slot_and_an_out_of_view_stage_returns() {
    let mut e = stages(4, 2);
    let out = (1..=3)
        .find(|n| !e.slot_numbers().contains(n))
        .expect("one out of view");
    let shown = e.slot_numbers()[0];
    // Close the only window of a shown stage.
    let id = e.members(shown)[0];
    e.close(id);
    assert_eq!(e.slot_numbers().len(), 2);
    let out_id =
        e.m.stages()
            .iter()
            .position(|s| s.windows.contains(&w(out as u32)));
    assert!(out_id.is_some());
}

#[test]
fn capacity_shrinks_by_evicting_the_oldest_and_grows_back() {
    let mut e = stages(5, 6);
    assert_eq!(e.slot_numbers(), [1, 2, 3, 4]);
    e.m.set_strip_capacity(2);
    assert_eq!(e.slot_numbers(), [3, 4]);
    e.m.set_strip_capacity(0);
    assert!(e.slot_numbers().is_empty());
    e.m.set_strip_capacity(6);
    assert_eq!(e.slot_numbers().len(), 4);
}

#[test]
fn fixed_order_lists_creation_order_and_can_show_the_active_stage() {
    let mut e = stages(4, 6);
    e.switch(2);
    e.m.set_strip_options(StripOptions {
        order: StripOrder::Fixed,
        show_active: false,
        capacity: 6,
    });
    assert_eq!(e.slot_numbers(), [1, 3, 4]);
    e.m.set_strip_options(StripOptions {
        order: StripOrder::Fixed,
        show_active: true,
        capacity: 6,
    });
    let cards = e.m.strip();
    assert_eq!(
        cards.iter().map(|c| c.number).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(cards[1].active);
    e.m.set_strip_options(StripOptions {
        order: StripOrder::Native,
        show_active: true,
        capacity: 6,
    });
    assert!(
        e.m.strip().iter().all(|c| !c.active),
        "native never shows the active stage"
    );
}

#[test]
fn cards_list_windows_front_to_back_and_apps_once() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    e.add(meta(3, 10, "com.example.a"), RuleAction::Current);
    e.add(child(4, 10, Some(3)), RuleAction::Current);
    e.add(meta(5, 12, "com.example.c"), RuleAction::New);
    let card = &e.m.strip()[0];
    assert_eq!(card.windows, [w(3), w(2), w(1)]);
    assert_eq!(card.apps, [10, 11]);
    assert_eq!(card.number, 1);
}

#[test]
fn strip_auto_hide_follows_apple_setting() {
    let mut m = model();
    assert!(!m.strip_auto_hides());
    m.set_settings(NativeSettings {
        hide_recent_apps: true,
        ..NativeSettings::default()
    });
    assert!(m.strip_auto_hides());
}

#[test]
fn moves_reported_for_a_parked_window_never_change_its_restore_frame() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    let saved = e.m.window(w(1)).map(|x| x.restore).expect("known");
    // macOS clamped the parked window somewhere that does not look parked.
    e.m.window_frame_changed(w(1), Rect::new(1200.0, 700.0, 600.0, 400.0));
    assert_eq!(e.m.window(w(1)).map(|x| x.restore), Some(saved));
    e.switch(1);
    assert_eq!(e.d.frames[&w(1)], saved);
}

// ----- merge (shift-click on a card) ---------------------------------------------------------

#[test]
fn merge_brings_a_stage_into_the_active_one_in_front() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    e.add(meta(3, 12, "com.example.c"), RuleAction::Current);
    // Stage 2 holds [3, 2] and is active; go back to 1 and merge 2 into it.
    e.switch(1);
    assert!(e.d.parked(w(2)) && e.d.parked(w(3)));
    let saved3 = e.m.window(w(3)).map(|x| x.restore).expect("known");
    let stage2 = e.m.stages()[1].id;
    let obs = e.d.obs();
    let a = e.m.merge_into_active(stage2, &obs);
    e.run(a.clone());
    assert_eq!(e.m.stages().len(), 1, "the emptied stage is gone");
    assert_eq!(e.members(1), [3, 2, 1]);
    assert!(!e.d.parked(w(2)) && !e.d.parked(w(3)) && !e.d.parked(w(1)));
    assert_eq!(e.d.frames[&w(3)], saved3, "restored exactly");
    assert_eq!(e.d.focused, Some(w(3)), "the merged stage's key is focused");
    assert_eq!(e.d.z[..3], [w(3), w(2), w(1)]);
    assert_eq!(a.last(), Some(&Action::StateChanged));
    assert!(e.m.strip().is_empty());
}

#[test]
fn merge_of_the_active_or_an_unknown_stage_is_a_no_op() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    let active = e.m.main_active_stage().expect("active");
    let obs = e.d.obs();
    assert!(e.m.merge_into_active(active, &obs).is_empty());
    assert!(e.m.merge_into_active(StageId(99), &obs).is_empty());
}

#[test]
fn merge_keeps_a_declared_stage_and_moves_children_along() {
    let mut e = Env::with(ModelOptions {
        named_stages: vec!["Work".into(), "Chat".into()],
        ..Default::default()
    });
    e.add(meta(1, 10, "com.example.a"), RuleAction::Current);
    e.add(
        meta(2, 11, "com.example.chat"),
        RuleAction::Stage("Chat".into()),
    );
    // Window 2 was focused at birth, so Chat is active; add a child, then go back to Work.
    let c = child(5, 11, Some(2));
    e.d.frames.insert(c.id, c.frame);
    let obs = e.d.obs();
    let a = e.m.add_window(c, &Decision::default(), &obs);
    e.run(a);
    e.switch(1);
    assert!(e.d.parked(w(5)));
    let chat = e.m.stages()[1].id;
    let obs = e.d.obs();
    let a = e.m.merge_into_active(chat, &obs);
    e.run(a);
    assert_eq!(e.m.stages().len(), 2, "a declared stage survives empty");
    assert!(e.members(2).is_empty());
    assert_eq!(e.members(1), [2, 1]);
    assert!(!e.d.parked(w(5)), "the child came along");
    assert_eq!(e.m.stage_of(w(5)), e.m.main_active_stage());
}

#[test]
fn merge_leaves_pinned_windows_alone() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Float);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Current);
    e.add(meta(3, 12, "com.example.c"), RuleAction::New);
    let stage1 = e.m.stages()[0].id;
    let obs = e.d.obs();
    let a = e.m.merge_into_active(stage1, &obs);
    e.run(a);
    assert_eq!(e.m.window(w(1)).map(|x| x.home), Some(Home::Pinned));
    assert_eq!(e.members(1), [2, 3]);
}

// ----- property test ----------------------------------------------------------------------

/// Tiny deterministic generator (SplitMix64).
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
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> Option<T> {
        (!items.is_empty()).then(|| items[self.below(items.len())])
    }
}

fn check_invariants(e: &Env, step: &str) {
    let m = &e.m;
    // Every live window the desk shows parked is known to the model as parked.
    for (&id, &f) in &e.d.frames {
        if is_parked(f, FRAME) {
            assert!(
                m.window(id).is_some_and(|w| w.parked),
                "{step}: {id:?} parked on the desk but not in the model"
            );
        }
    }
    // Restore frames are never in the park zone.
    for w in m.windows() {
        assert!(
            !is_parked(w.restore, FRAME),
            "{step}: restore frame of {:?} is parked",
            w.meta.id
        );
    }
    // Standard windows: exactly one home; members exist and are standard.
    let mut seen = std::collections::HashSet::new();
    for s in m.stages() {
        for id in &s.windows {
            assert!(seen.insert(*id), "{step}: {id:?} in two stages");
            let w = m.window(*id).expect("member exists");
            assert_eq!(w.meta.role, WindowRole::Standard, "{step}");
            assert_eq!(w.home, Home::Stage(s.id), "{step}");
        }
        if let Some(k) = s.key {
            assert!(s.windows.contains(&k), "{step}: key {k:?} not a member");
        }
        let active = m.active_stage(s.display) == Some(s.id);
        let declared = s.name.as_deref().is_some_and(|n| {
            m.options()
                .named_stages
                .iter()
                .any(|d| d.eq_ignore_ascii_case(n))
        });
        assert!(
            active || declared || !s.windows.is_empty(),
            "{step}: dead stage {:?} kept",
            s.id
        );
    }
    for w in m.windows() {
        match (w.meta.role, w.home) {
            (WindowRole::Standard, Home::Stage(_)) => assert!(seen.contains(&w.meta.id), "{step}"),
            (_, Home::Stage(s)) => assert!(m.stage(s).is_some(), "{step}: child home missing"),
            (_, Home::Pinned) => assert!(!w.parked, "{step}: pinned window parked"),
        }
    }
    // Visible stages are visible, inactive ones parked (on the desk).
    for w in m.windows() {
        let Some(&f) = e.d.frames.get(&w.meta.id) else {
            continue;
        };
        let shown = match w.home {
            Home::Pinned => true,
            Home::Stage(s) => m.active_stage(MAIN) == Some(s),
        };
        if shown {
            assert!(
                !is_parked(f, FRAME),
                "{step}: {:?} should be visible",
                w.meta.id
            );
        } else {
            assert!(
                is_parked(f, FRAME) && w.parked,
                "{step}: {:?} should be parked",
                w.meta.id
            );
        }
    }
    // Strip slots: unique, eligible, within capacity, and full when there is room.
    let slots = m.strip_slots(MAIN);
    let cap = m.options().strip.capacity;
    assert!(slots.len() <= cap, "{step}");
    let mut uniq = slots.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), slots.len(), "{step}: duplicate slot");
    let eligible = m
        .stages()
        .iter()
        .filter(|s| {
            m.active_stage(MAIN) != Some(s.id)
                && (!s.windows.is_empty()
                    || s.name
                        .as_deref()
                        .is_some_and(|n| m.options().named_stages.iter().any(|d| d == n)))
        })
        .count();
    assert_eq!(slots.len(), eligible.min(cap), "{step}: slots not filled");
    assert!(
        m.active_stage(MAIN).is_some_and(|a| !slots.contains(&a)),
        "{step}: active in strip"
    );
}

#[test]
fn property_no_sequence_of_operations_loses_a_window() {
    let names = ["Work", "Chat", "Music"];
    for seed in 0..300u64 {
        let mut rng = Rng(seed);
        let mut options = ModelOptions::default();
        options.strip.capacity = 1 + rng.below(4);
        if seed % 3 == 0 {
            options.named_stages = vec!["Work".into()];
        }
        if seed % 5 == 0 {
            options.settings.app_windows = AppWindows::OneAtATime;
        }
        let mut e = Env::with(options);
        let mut next_id = 1u32;
        let mut log = Vec::new();
        for _ in 0..150 {
            let alive: Vec<WindowId> = e.d.frames.keys().copied().collect();
            let managed: Vec<WindowId> = e.m.windows().map(|w| w.meta.id).collect();
            let op = rng.below(19);
            let step = format!("seed {seed} op {op} after {log:?}");
            match op {
                0..=2 => {
                    let action = match rng.below(8) {
                        6 | 7 => RuleAction::Native,
                        0 => RuleAction::New,
                        1 => RuleAction::Stage(names[rng.below(3)].to_string()),
                        2 => RuleAction::Float,
                        3 => RuleAction::Ignore,
                        _ => RuleAction::Current,
                    };
                    let placement =
                        [Placement::AsIs, Placement::Center, Placement::Cascade][rng.below(3)];
                    let mut m = meta(next_id, 10 + rng.below(4) as i32, "com.example.p");
                    if rng.below(8) == 0 && action != RuleAction::Ignore {
                        m.frame = Rect::new(1420.0, 880.0, 500.0, 300.0); // born in the corner
                    }
                    let focused = rng.below(2) == 0;
                    if focused {
                        e.d.open(&m);
                    } else {
                        e.d.frames.insert(m.id, m.frame);
                        e.d.z.push(m.id);
                    }
                    log.push(format!("add {next_id} {action}"));
                    next_id += 1;
                    let obs = e.d.obs();
                    let a = e.m.add_window(m, &Decision { action, placement }, &obs);
                    e.run(a);
                }
                3 => {
                    let parent = rng.pick(&managed).map(|p| p.0);
                    let parent = if rng.below(5) == 0 { None } else { parent };
                    let c = child(next_id, 10, parent);
                    e.d.frames.insert(c.id, c.frame);
                    e.d.z.insert(0, c.id);
                    log.push(format!("child {next_id} of {parent:?}"));
                    next_id += 1;
                    let obs = e.d.obs();
                    let a = e.m.add_window(c, &Decision::default(), &obs);
                    e.run(a);
                }
                4 => {
                    if let Some(id) = rng.pick(&alive) {
                        log.push(format!("close {}", id.0));
                        e.close(id.0);
                        // Sometimes the app closes the children too.
                        if rng.below(2) == 0 {
                            for c in alive
                                .iter()
                                .filter(|c| e.m.window(**c).is_none() && **c != id)
                            {
                                e.d.close(*c);
                            }
                        }
                    }
                }
                5 | 6 => {
                    let n = 1 + rng.below(e.m.stages().len() + 1);
                    log.push(format!("switch {n}"));
                    e.switch(n);
                }
                7 => {
                    if let Some(id) = rng.pick(&managed) {
                        let t = match rng.below(3) {
                            0 => Target::New,
                            1 => Target::Named(names[rng.below(3)].to_string()),
                            _ => Target::Number(1 + rng.below(e.m.stages().len() + 2)),
                        };
                        log.push(format!("send {} {t:?}", id.0));
                        e.send(id.0, t);
                    }
                }
                8 => {
                    if let Some(id) = rng.pick(&managed) {
                        log.push(format!("pin {}", id.0));
                        let obs = e.d.obs();
                        let a = e.m.toggle_pin(id, &obs);
                        e.run(a);
                    }
                }
                9 => {
                    let pid = 10 + rng.below(4) as i32;
                    let focused = rng.pick(&alive);
                    e.d.focused = focused;
                    log.push(format!("activate {pid} focused {focused:?}"));
                    let obs = e.d.obs();
                    let a = e.m.app_activated(pid, &obs);
                    e.run(a);
                }
                10 => {
                    log.push("reveal".into());
                    e.reveal();
                }
                11 => {
                    // The user moves a visible window.
                    let visible: Vec<WindowId> = alive
                        .iter()
                        .copied()
                        .filter(|id| !e.d.parked(*id))
                        .collect();
                    if let Some(id) = rng.pick(&visible) {
                        let f = Rect::new(
                            rng.below(1200) as f64,
                            25.0 + rng.below(700) as f64,
                            300.0 + rng.below(900) as f64,
                            200.0 + rng.below(700) as f64,
                        );
                        e.d.frames.insert(id, f);
                        e.d.raise(id);
                        e.d.focused = Some(id);
                        log.push(format!("move {}", id.0));
                        e.m.window_frame_changed(id, f);
                    }
                }
                12 => {
                    if let Some(id) = rng.pick(&alive) {
                        log.push(format!("focus {}", id.0));
                        let obs = Observation {
                            focused: Some(id),
                            ..e.d.obs()
                        };
                        let a = e.m.window_focused(id, &obs);
                        e.run(a);
                    }
                }
                13 => {
                    if let Some(id) = rng.pick(&managed) {
                        // Duplicate notification.
                        let m = WindowMeta {
                            id,
                            ..meta(id.0, 10, "com.example.p")
                        };
                        let obs = e.d.obs();
                        let a = e.m.add_window(
                            m,
                            &Decision {
                                action: RuleAction::New,
                                placement: Placement::Center,
                            },
                            &obs,
                        );
                        assert!(a.is_empty(), "duplicate produced actions");
                    }
                }
                14 => {
                    if let Some(stage) = e.m.stages().get(rng.below(4)).map(|s| s.id) {
                        log.push(format!("merge {}", stage.0));
                        let obs = e.d.obs();
                        let a = e.m.merge_into_active(stage, &obs);
                        e.run(a);
                    }
                }
                15 => {
                    let cap = rng.below(5);
                    log.push(format!("capacity {cap}"));
                    e.m.set_strip_capacity(cap);
                }
                16 => {
                    let pid = 10 + rng.below(4) as i32;
                    log.push(format!("bring {pid} here"));
                    let obs = e.d.obs();
                    let a = e.m.bring_app_here(pid, &obs);
                    e.run(a);
                }
                17 => {
                    if let Some(stage) = e.m.stages().get(rng.below(4)).map(|s| s.id) {
                        let at = Point::new(rng.below(1600) as f64 - 100.0, rng.below(1000) as f64);
                        log.push(format!("drop card {} at {at:?}", stage.0));
                        let obs = e.d.obs();
                        let a = e.m.merge_into_active_at(stage, at, &obs);
                        e.run(a);
                    }
                }
                _ => {
                    // The user drags a visible standard window onto the strip.
                    let draggable: Vec<WindowId> = managed
                        .iter()
                        .copied()
                        .filter(|id| {
                            !e.d.parked(*id)
                                && e.m.window(*id).is_some_and(|w| w.meta.role == WindowRole::Standard)
                        })
                        .collect();
                    if let Some(id) = rng.pick(&draggable) {
                        let pre = e.m.window(id).map(|w| w.restore).unwrap_or(FRAME);
                        if let Some(f) = e.d.frames.get_mut(&id) {
                            *f = Rect::new(40.0, 300.0, f.w, f.h);
                        }
                        log.push(format!("drop window {} on the strip", id.0));
                        let obs = e.d.obs();
                        let a = e.m.drop_window_on_strip(id, pre, None, &obs);
                        e.run(a);
                    }
                }
            }
            if log.len() > 12 {
                log.remove(0);
            }
            check_invariants(&e, &step);
        }
        // The final guarantee: unpark_all brings back every parked window.
        let a = e.m.unpark_all();
        e.run(a);
        for (&id, &f) in &e.d.frames {
            assert!(
                !is_parked(f, FRAME),
                "seed {seed}: {id:?} still parked after unpark_all"
            );
        }
    }
}

// ----- Apple's defaults: native new windows, bring here, drag and drop ---------------------

fn titles(e: &Env, n: usize) -> Vec<u32> {
    e.members(n)
}

#[test]
fn native_a_window_of_an_app_in_the_active_stage_joins_it() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Native);
    e.add(meta(2, 10, "com.example.a"), RuleAction::Native);
    assert_eq!(e.m.stages().len(), 1, "the empty first stage was used");
    assert_eq!(titles(&e, 1), [2, 1]);
    assert_eq!(e.active_number(), 1);
}

#[test]
fn native_a_new_app_opens_a_new_stage_and_switches() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Native);
    let a = e.add(meta(2, 11, "com.example.b"), RuleAction::Native);
    assert_eq!(e.m.stages().len(), 2);
    assert_eq!(e.number_of(2), 2);
    assert_eq!(e.active_number(), 2);
    assert!(e.d.parked(w(1)), "the stage left behind is parked");
    assert!(a.contains(&Action::Focus { id: w(2) }));
    assert_eq!(e.slot_numbers(), [1], "the stage left enters the strip");
}

#[test]
fn native_a_new_window_of_a_parked_app_brings_its_stage_forward() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Native);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Native);
    assert_eq!(e.active_number(), 2);
    let a = e.add(meta(3, 10, "com.example.a"), RuleAction::Native);
    assert_eq!(e.active_number(), 1, "switched to the app's stage");
    assert_eq!(titles(&e, 1), [3, 1]);
    assert!(!e.d.parked(w(1)) && !e.d.parked(w(3)));
    assert!(e.d.parked(w(2)));
    assert_eq!(a.iter().rev().nth(1), Some(&Action::Focus { id: w(3) }));
}

#[test]
fn native_picks_the_apps_most_recent_stage() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Native);
    e.add(meta(2, 10, "com.example.a"), RuleAction::Native);
    e.send(2, Target::New);
    e.add(meta(3, 11, "com.example.b"), RuleAction::Native);
    // Stage 2 (window 2) was used more recently than stage 1? Visit it, then leave.
    e.switch(2);
    e.switch(3);
    e.add(meta(4, 10, "com.example.a"), RuleAction::Native);
    assert_eq!(e.number_of(4), 2);
    assert_eq!(e.active_number(), 2);
}

#[test]
fn native_a_pinned_app_counts_as_present() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Float);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Native);
    e.add(meta(3, 10, "com.example.a"), RuleAction::Native);
    assert_eq!(e.m.stages().len(), 1);
    assert_eq!(e.number_of(3), 1);
}

#[test]
fn native_with_one_at_a_time_gives_a_parked_apps_new_window_its_own_stage() {
    let mut options = ModelOptions::default();
    options.settings.app_windows = AppWindows::OneAtATime;
    let mut e = Env::with(options);
    e.add(meta(1, 10, "com.example.a"), RuleAction::Native);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Native);
    e.add(meta(3, 10, "com.example.a"), RuleAction::Native);
    assert_eq!(e.m.stages().len(), 3);
    assert_eq!(e.number_of(3), 3);
    assert_eq!(e.active_number(), 3);
    // An app present in the active stage still joins it.
    e.add(meta(4, 10, "com.example.a"), RuleAction::Native);
    assert_eq!(e.number_of(4), 3);
}

#[test]
fn bring_app_here_moves_every_window_of_the_app() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Native);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Native);
    e.add(meta(3, 11, "com.example.b"), RuleAction::Native);
    e.send(3, Target::New);
    e.switch(1);
    assert_eq!(e.m.stages().len(), 3);
    let obs = e.d.obs();
    let a = e.m.bring_app_here(11, &obs);
    e.run(a.clone());
    assert_eq!(e.m.stages().len(), 1, "emptied stages go away");
    assert_eq!(e.active_number(), 1);
    let mut members = titles(&e, 1);
    members.sort_unstable();
    assert_eq!(members, [1, 2, 3]);
    for id in [1, 2, 3] {
        assert!(!e.d.parked(w(id)), "{id} shown");
    }
    assert!(a.iter().any(|x| matches!(x, Action::Focus { .. })));
    // Nothing elsewhere: no actions.
    let obs = e.d.obs();
    assert!(e.m.bring_app_here(11, &obs).is_empty());
}

#[test]
fn bring_app_here_one_at_a_time_takes_only_the_most_recent_window() {
    let mut options = ModelOptions::default();
    options.settings.app_windows = AppWindows::OneAtATime;
    let mut e = Env::with(options);
    e.add(meta(1, 10, "com.example.a"), RuleAction::Native);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Native);
    e.add(meta(3, 11, "com.example.b"), RuleAction::Native);
    e.switch(1);
    let obs = e.d.obs();
    let a = e.m.bring_app_here(11, &obs);
    e.run(a);
    assert_eq!(titles(&e, 1), [3, 1]);
    assert!(e.d.parked(w(2)));
}

#[test]
fn a_dropped_card_lands_its_front_window_at_the_drop_point() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Native);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Native);
    e.add(meta(3, 11, "com.example.b"), RuleAction::Native);
    e.switch(1);
    let other = e.m.window(w(2)).map(|x| x.restore);
    let stage = e.m.stages()[1].id;
    let obs = e.d.obs();
    let a = e.m.merge_into_active_at(stage, Point::new(500.0, 300.0), &obs);
    e.run(a);
    assert_eq!(e.m.stages().len(), 1);
    let front = e.d.frames[&w(3)];
    assert_eq!((front.x, front.y), (500.0, 300.0));
    assert_eq!(e.m.window(w(3)).map(|x| x.restore), Some(front));
    assert_eq!(e.d.frames.get(&w(2)).copied(), other, "others keep their frames");
    assert_eq!(e.m.main_active_stage().and_then(|s| e.m.stage(s)).and_then(|s| s.key), Some(w(3)));
    // A drop near the edge is clamped onto the visible frame.
    e.add(meta(4, 12, "com.example.c"), RuleAction::Native);
    e.switch(1);
    let stage = e.m.stage_of(w(4)).expect("staged");
    let obs = e.d.obs();
    let a = e.m.merge_into_active_at(stage, Point::new(1400.0, -50.0), &obs);
    e.run(a);
    let f = e.d.frames[&w(4)];
    assert!(VISIBLE.contains_rect(&f), "{f:?} on screen");
}

#[test]
fn a_window_dropped_on_the_strip_gets_its_own_stage_and_keeps_its_pre_drag_frame() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Native);
    e.add(meta(2, 10, "com.example.a"), RuleAction::Native);
    let pre = e.d.frames[&w(2)];
    let dropped = Rect::new(30.0, 400.0, pre.w, pre.h);
    e.d.frames.insert(w(2), dropped);
    e.m.window_frame_changed(w(2), dropped);
    let obs = e.d.obs();
    let a = e.m.drop_window_on_strip(w(2), pre, None, &obs);
    e.run(a);
    assert_eq!(e.m.stages().len(), 2);
    assert_eq!(e.active_number(), 1, "the view does not switch");
    assert_eq!(titles(&e, 1), [1]);
    assert!(e.d.parked(w(2)));
    assert_eq!(e.m.window(w(2)).map(|x| x.restore), Some(pre));
    assert_eq!(e.slot_numbers(), [2]);
    e.switch(2);
    assert_eq!(e.d.frames[&w(2)], pre, "comes back where it was before the drag");
}

/// Four apps, four stages; stage 1 active, stages 2, 3, 4 in the strip.
fn four_stages() -> Env {
    let mut e = Env::new();
    for (id, pid) in [(1, 10), (2, 11), (3, 12), (4, 13)] {
        e.add(meta(id, pid, "com.example.x"), RuleAction::New);
    }
    e.switch(1);
    e
}

fn stage_id(e: &Env, n: usize) -> StageId {
    e.m.stages()[n - 1].id
}

#[test]
fn a_window_dropped_between_two_cards_takes_that_slot() {
    let mut e = four_stages();
    let before: Vec<usize> = e.slot_numbers();
    assert_eq!(before.len(), 3);
    e.add(meta(5, 14, "com.example.y"), RuleAction::Current);
    let pre = e.d.frames[&w(5)];
    let below = stage_id(&e, before[1]);
    let obs = e.d.obs();
    let a = e.m.drop_window_on_strip(w(5), pre, Some(below), &obs);
    e.run(a);
    let new = e.number_of(5);
    assert_eq!(e.slot_numbers(), [before[0], new, before[1], before[2]], "right above the card it was dropped on");
    assert!(e.d.parked(w(5)));
}

#[test]
fn a_dropped_window_in_a_full_strip_keeps_its_slot_and_the_oldest_card_leaves() {
    let mut e = four_stages();
    e.m.set_strip_capacity(3);
    let before = e.slot_numbers();
    assert_eq!(before.len(), 3);
    e.add(meta(5, 14, "com.example.y"), RuleAction::Current);
    let pre = e.d.frames[&w(5)];
    let obs = e.d.obs();
    let a = e.m.drop_window_on_strip(w(5), pre, Some(stage_id(&e, before[2])), &obs);
    e.run(a);
    let now = e.slot_numbers();
    let new = e.number_of(5);
    assert_eq!(now.len(), 3);
    let at = now.iter().position(|&n| n == new).expect("in the strip");
    assert_eq!(now.get(at + 1), Some(&before[2]), "right above the card it was dropped on ({now:?})");
}

#[test]
fn a_card_dragged_to_another_slot_stays_there_and_the_swap_rule_carries_on() {
    let mut e = four_stages();
    let [a, b, c] = e.slot_numbers()[..] else { panic!("three cards") };
    let moved = e.m.move_card(stage_id(&e, c), Some(stage_id(&e, a)));
    assert_eq!(moved, [Action::StateChanged]);
    assert_eq!(e.slot_numbers(), [c, a, b]);
    assert!(e.m.move_card(stage_id(&e, c), Some(stage_id(&e, a))).is_empty(), "already there");
    assert!(e.m.move_card(stage_id(&e, c), Some(stage_id(&e, c))).is_empty(), "onto itself");
    let moved = e.m.move_card(stage_id(&e, c), None);
    assert_eq!(moved, [Action::StateChanged]);
    assert_eq!(e.slot_numbers(), [a, b, c], "to the bottom");
    // Apple's swap: the stage left takes the clicked card's slot.
    e.switch(b);
    assert_eq!(e.slot_numbers(), [a, 1, c]);
    // The order is part of the saved state.
    let state = e.m.to_state();
    let ids: Vec<StageId> = state.spaces[0].strip_slots.iter().filter_map(|i| e.m.stages().get(*i).map(|s| s.id)).collect();
    assert_eq!(ids, e.m.strip_slots(MAIN));
}

#[test]
fn in_fixed_order_a_dragged_card_moves_in_creation_order() {
    let mut e = Env::with(ModelOptions {
        strip: StripOptions { order: StripOrder::Fixed, show_active: false, capacity: 6 },
        ..ModelOptions::default()
    });
    for (id, pid) in [(1, 10), (2, 11), (3, 12), (4, 13)] {
        e.add(meta(id, pid, "com.example.x"), RuleAction::New);
    }
    e.switch(1);
    assert_eq!(e.slot_numbers(), [2, 3, 4]);
    let four = e.m.stage_of(w(4)).expect("staged");
    let two = e.m.stage_of(w(2)).expect("staged");
    assert_eq!(e.m.move_card(four, Some(two)), [Action::StateChanged]);
    assert_eq!(e.slot_numbers(), [2, 3, 4], "numbers follow the strip");
    assert_eq!(e.number_of(4), 2, "the moved stage is now number 2");
    assert_eq!(e.number_of(2), 3);
}

// ----- keeping windows clear of the strip ------------------------------------------------

mod fitting {
    use super::*;
    use crate::engine::stage_area::FitOptions;

    /// A 216 pt inset (the 5K strip column plus its margin) on the 1440 pt test display.
    const INSET: f64 = 216.0;

    fn env(shrink: bool) -> Env {
        Env::with(ModelOptions { fit: FitOptions { inset: INSET, shrink }, ..ModelOptions::default() })
    }

    fn at(id: u32, frame: Rect) -> WindowMeta {
        WindowMeta { frame, ..meta(id, 10 + id as i32, &format!("com.example.app{id}")) }
    }

    fn live(e: &Env, id: u32) -> Rect {
        e.d.frames[&w(id)]
    }

    #[test]
    fn a_new_window_over_the_strip_moves_right_without_resizing() {
        let mut e = env(true);
        e.add(at(1, Rect::new(40.0, 100.0, 600.0, 400.0)), RuleAction::Native);
        assert_eq!(live(&e, 1), Rect::new(INSET, 100.0, 600.0, 400.0));
        assert_eq!(e.m.window(w(1)).map(|x| x.restore), Some(Rect::new(INSET, 100.0, 600.0, 400.0)));
        assert_eq!(e.m.window(w(1)).and_then(|x| x.original), Some(Rect::new(40.0, 100.0, 600.0, 400.0)));
        // A window clear of the strip is left alone.
        let a = e.add(at(2, Rect::new(300.0, 100.0, 600.0, 400.0)), RuleAction::Current);
        assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { .. })));
        assert_eq!(e.m.window(w(2)).and_then(|x| x.original), None);
    }

    #[test]
    fn a_too_wide_window_is_narrowed_and_quit_gives_its_exact_size_back() {
        let mut e = env(true);
        let wide = Rect::new(10.0, 60.0, 1400.0, 700.0);
        e.add(at(1, wide), RuleAction::Native);
        assert_eq!(live(&e, 1), Rect::new(INSET, 60.0, 1440.0 - INSET, 700.0), "narrowed, height kept");
        assert_eq!(e.m.safety_targets(), vec![(w(1), wide)], "safety paths know the pre-fit frame");
        let a = e.m.unpark_all();
        e.run(a);
        assert_eq!(live(&e, 1), wide, "quit restores the exact original frame");
        assert!(e.m.safety_targets().is_empty());
    }

    #[test]
    fn nudge_only_leaves_a_too_wide_window_where_it_is() {
        let mut e = env(false);
        let wide = Rect::new(10.0, 60.0, 1400.0, 700.0);
        let a = e.add(at(1, wide), RuleAction::Native);
        assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { .. })));
        assert_eq!(live(&e, 1), wide);
        e.add(at(2, Rect::new(100.0, 60.0, 900.0, 700.0)), RuleAction::Current);
        assert_eq!(live(&e, 2).x, INSET, "one that fits still moves");
        assert_eq!(live(&e, 2).w, 900.0);
    }

    #[test]
    fn restoring_a_stage_fits_again_after_the_user_moved_a_window_over_the_strip() {
        let mut e = env(true);
        let wide = Rect::new(10.0, 60.0, 1400.0, 700.0);
        e.add(at(1, wide), RuleAction::Native);
        e.add(at(2, Rect::new(400.0, 100.0, 600.0, 400.0)), RuleAction::New);
        e.switch(1);
        let fitted = live(&e, 1);
        // The user drags it 100 pt left over the strip: their frame stands.
        let user = fitted.translate(-100.0, 0.0);
        e.d.frames.insert(w(1), user);
        e.m.window_frame_changed(w(1), user);
        assert_eq!(e.m.window(w(1)).map(|x| x.restore), Some(user));
        assert_eq!(e.m.window(w(1)).and_then(|x| x.original), Some(wide.translate(-100.0, 0.0)), "the move carries the pre-fit frame along");
        // Leaving and coming back places it again: clear of the strip.
        e.switch(2);
        e.switch(1);
        assert_eq!(live(&e, 1), fitted);
        // A resize by the user forgets the pre-fit size.
        let resized = Rect::new(300.0, 60.0, 800.0, 700.0);
        e.d.frames.insert(w(1), resized);
        e.m.window_frame_changed(w(1), resized);
        assert_eq!(e.m.window(w(1)).and_then(|x| x.original), None);
    }

    #[test]
    fn our_own_clamped_frames_keep_the_pre_fit_frame() {
        let mut e = env(true);
        let wide = Rect::new(10.0, 60.0, 1400.0, 700.0);
        e.add(at(1, wide), RuleAction::Native);
        // The app refused to get narrower than 1300 pt.
        let clamped = Rect::new(INSET, 60.0, 1300.0, 700.0);
        e.m.window_frame_settled(w(1), clamped);
        assert_eq!(e.m.window(w(1)).and_then(|x| x.original), Some(wide));
        assert_eq!(e.m.window(w(1)).map(|x| x.restore), Some(clamped));
    }

    #[test]
    fn children_and_pinned_windows_are_never_fitted() {
        let mut e = env(true);
        e.add(at(1, Rect::new(300.0, 60.0, 600.0, 400.0)), RuleAction::Native);
        let c = WindowMeta { frame: Rect::new(20.0, 100.0, 200.0, 100.0), ..child(5, 11, Some(1)) };
        e.add(c, RuleAction::Native);
        assert_eq!(live(&e, 5).x, 20.0);
        e.add(at(3, Rect::new(20.0, 60.0, 300.0, 300.0)), RuleAction::Float);
        assert_eq!(live(&e, 3).x, 20.0);
    }

    #[test]
    fn fitting_is_off_by_default() {
        let mut e = Env::new();
        e.add(at(1, Rect::new(10.0, 60.0, 1400.0, 700.0)), RuleAction::Native);
        assert_eq!(live(&e, 1).x, 10.0);
    }
}

// ----- parity: the card's app icon, Cmd-M, One at a Time ----------------------------------

#[test]
fn an_icon_click_switches_and_brings_every_window_of_the_app() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Native);
    e.add(meta(2, 11, "com.example.b"), RuleAction::Native);
    e.add(meta(3, 11, "com.example.b"), RuleAction::Native);
    e.send(3, Target::New);
    e.switch(1);
    let stage = e.m.stage_of(w(2)).expect("staged");
    let obs = e.d.obs();
    let a = e.m.show_app_everywhere(stage, 11, &obs);
    e.run(a.clone());
    assert_eq!(e.m.main_active_stage(), Some(stage));
    let mut members = titles(&e, e.active_number());
    members.sort_unstable();
    assert_eq!(members, [2, 3], "the app's window from the other stage came too");
    assert!(!e.d.parked(w(2)) && !e.d.parked(w(3)) && e.d.parked(w(1)));
    assert_eq!(a.iter().filter(|x| **x == Action::StateChanged).count(), 1);
    assert_eq!(a.last(), Some(&Action::StateChanged));
}

#[test]
fn minimize_sends_the_window_to_the_strip_and_keeps_the_view() {
    let mut e = Env::new();
    e.add(meta(1, 10, "com.example.a"), RuleAction::Native);
    e.add(meta(2, 10, "com.example.a"), RuleAction::Native);
    let before = e.m.window(w(1)).map(|x| x.restore);
    let active = e.m.main_active_stage();
    let obs = e.d.obs();
    let a = e.m.minimize_to_strip(w(1), &obs);
    e.run(a);
    assert!(e.d.parked(w(1)) && !e.d.parked(w(2)));
    assert_eq!(e.m.main_active_stage(), active, "the view does not switch");
    let own = e.m.stage_of(w(1)).expect("staged");
    assert_ne!(Some(own), active);
    assert!(e.m.strip().iter().any(|c| c.stage == own), "a card of its own");
    assert_eq!(e.m.window(w(1)).map(|x| x.restore), before, "its frame waits for its return");
    let obs = e.d.obs();
    assert!(e.m.minimize_to_strip(w(1), &obs).is_empty(), "a parked window is left alone");
}

#[test]
fn one_at_a_time_shows_an_app_once_and_its_card_cycles_through_its_windows() {
    let mut options = ModelOptions::default();
    options.settings.app_windows = AppWindows::OneAtATime;
    let mut e = Env::with(options);
    e.add(meta(1, 11, "com.example.b"), RuleAction::New);
    e.add(meta(2, 11, "com.example.b"), RuleAction::New);
    e.add(meta(3, 11, "com.example.b"), RuleAction::New);
    e.add(meta(4, 10, "com.example.a"), RuleAction::New);
    let strip = e.m.strip();
    assert_eq!(strip.len(), 1, "one card for the app's three waiting windows: {strip:?}");
    assert_eq!(strip[0].windows.len(), 3);
    let mut seen = Vec::new();
    for _ in 0..3 {
        let card = e.m.strip().into_iter().find(|c| c.windows.iter().any(|x| [1, 2, 3].contains(&x.0))).expect("card");
        let obs = e.d.obs();
        let a = e.m.switch_to(card.stage, &obs);
        e.run(a);
        let shown = titles(&e, e.active_number());
        assert_eq!(shown.len(), 1);
        seen.push(shown[0]);
        assert_eq!(e.m.strip().len(), 2, "the app's card and the stage left behind");
    }
    seen.sort_unstable();
    assert_eq!(seen, [1, 2, 3], "clicks cycle through every window");
}
