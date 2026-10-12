//! Persistence tests: JSON round trip, re-matching, and rebuilding a model. Made-up titles.

use super::*;
use crate::engine::model::{ModelOptions, Target};
use crate::engine::rules::RuleAction;
use crate::engine::testkit::*;

fn saved(bundle: &str, title: &str, z: usize) -> SavedWindow {
    SavedWindow {
        bundle_id: bundle.into(),
        title: title.into(),
        frame: Rect::new(10.0 * z as f64 + 50.0, 60.0, 500.0, 400.0),
        z,
        key: z == 0,
        pinned: false,
        role: SavedRole::Standard,
    }
}

fn live(id: u32, bundle: &str, title: &str) -> WindowMeta {
    WindowMeta {
        title: title.into(),
        bundle_id: bundle.into(),
        ..meta(id, 10, bundle)
    }
}

fn state(stages: Vec<(Option<&str>, Vec<SavedWindow>)>, active: Option<usize>) -> StateFile {
    StateFile {
        version: STATE_VERSION,
        stages: stages
            .into_iter()
            .map(|(name, windows)| SavedStage {
                name: name.map(str::to_string),
                windows,
            })
            .collect(),
        active_index: active,
        strip_slots: Vec::new(),
    }
}

fn ids(a: &AssignedStage) -> Vec<u32> {
    a.windows.iter().map(|w| w.id.0).collect()
}

#[test]
fn json_round_trip() {
    let s = state(
        vec![
            (Some("Work"), vec![saved("com.example.ed", "Plan.txt", 0)]),
            (None, vec![]),
        ],
        Some(0),
    );
    let back = StateFile::from_json(&s.to_json()).expect("parses");
    assert_eq!(back, s);
    assert!(s.to_json().contains("\"role\": \"standard\""));
}

#[test]
fn json_rejects_other_versions_and_garbage() {
    let mut s = state(vec![], None);
    s.version = 2;
    assert!(StateFile::from_json(&s.to_json()).is_err());
    assert!(StateFile::from_json("not json").is_err());
}

#[test]
fn json_without_strip_slots_still_parses() {
    let text = r#"{"version":1,"stages":[],"active_index":null}"#;
    assert!(
        StateFile::from_json(text)
            .expect("parses")
            .strip_slots
            .is_empty()
    );
}

#[test]
fn exact_title_matches_win_over_bundle_only() {
    let s = state(
        vec![
            (None, vec![saved("com.example.ed", "Alpha.txt", 0)]),
            (None, vec![saved("com.example.ed", "Beta.txt", 0)]),
        ],
        Some(0),
    );
    // Live order differs from saved order; titles decide.
    let live = [
        live(7, "com.example.ed", "Beta.txt"),
        live(8, "com.example.ed", "Alpha.txt"),
    ];
    let a = rematch(&s, &live);
    assert_eq!(ids(&a.stages[0]), [8]);
    assert_eq!(ids(&a.stages[1]), [7]);
    assert!(a.unmatched.is_empty());
}

#[test]
fn bundle_only_fills_in_saved_order_after_exact_matches() {
    let s = state(
        vec![
            (None, vec![saved("com.example.web", "Old tab", 0)]),
            (
                None,
                vec![
                    saved("com.example.web", "Recipes", 0),
                    saved("com.example.web", "Other", 1),
                ],
            ),
        ],
        Some(0),
    );
    let live = [
        live(1, "com.example.web", "News"),
        live(2, "com.example.web", "Recipes"),
        live(3, "com.example.web", "Weather"),
    ];
    let a = rematch(&s, &live);
    assert_eq!(
        ids(&a.stages[0]),
        [1],
        "first bundle-only match in saved order"
    );
    assert_eq!(ids(&a.stages[1]), [2, 3]);
}

#[test]
fn each_live_window_is_used_once_and_extras_are_unmatched() {
    let s = state(
        vec![(
            None,
            vec![
                saved("com.example.ed", "A", 0),
                saved("com.example.ed", "A", 1),
            ],
        )],
        Some(0),
    );
    let live = [
        live(1, "com.example.ed", "A"),
        live(2, "com.example.term", "Shell"),
    ];
    let a = rematch(&s, &live);
    assert_eq!(ids(&a.stages[0]), [1]);
    assert_eq!(a.unmatched, [WindowId(2)]);
}

#[test]
fn children_are_left_out_of_matching() {
    let mut c = saved("com.example.ed", "Save", 1);
    c.role = SavedRole::Child;
    let s = state(
        vec![(None, vec![saved("com.example.ed", "Doc", 0), c])],
        Some(0),
    );
    let live = [
        WindowMeta {
            role: WindowRole::Child {
                parent: Some(WindowId(1)),
            },
            ..live(2, "com.example.ed", "Save")
        },
        live(1, "com.example.ed", "Doc"),
    ];
    let a = rematch(&s, &live);
    assert_eq!(ids(&a.stages[0]), [1]);
    assert!(a.unmatched.is_empty());
}

#[test]
fn z_order_and_flags_survive_rematch() {
    let mut pinned = saved("com.example.clock", "Clock", 2);
    pinned.pinned = true;
    let s = state(
        vec![(
            None,
            vec![
                saved("com.example.b", "B", 1),
                pinned,
                saved("com.example.a", "A", 0),
            ],
        )],
        Some(0),
    );
    let live = [
        live(1, "com.example.a", "A"),
        live(2, "com.example.b", "B"),
        live(3, "com.example.clock", "Clock"),
    ];
    let a = rematch(&s, &live);
    assert_eq!(ids(&a.stages[0]), [1, 2, 3]);
    assert!(a.stages[0].windows[0].key);
    assert!(a.stages[0].windows[2].pinned);
}

#[test]
fn out_of_range_indices_are_dropped() {
    let mut s = state(vec![(None, vec![])], Some(5));
    s.strip_slots = vec![0, 9];
    let a = rematch(&s, &[]);
    assert_eq!(a.active_index, None);
    assert_eq!(a.strip_slots, [0]);
}

#[test]
fn to_state_saves_stages_frames_key_children_and_pins() {
    let mut m = model();
    let obs = Observation::default();
    let d = |a| Decision {
        action: a,
        placement: Default::default(),
    };
    m.add_window(live(1, "com.example.a", "A"), &d(RuleAction::Current), &obs);
    m.add_window(live(2, "com.example.b", "B"), &d(RuleAction::Current), &obs);
    m.add_window(child(5, 10, Some(1)), &d(RuleAction::Current), &obs);
    m.add_window(live(3, "com.example.c", "C"), &d(RuleAction::Float), &obs);
    m.send_window(WindowId(2), Target::Named("Mail".into()), &obs);
    let s = m.to_state();
    assert_eq!(s.stages.len(), 2);
    assert_eq!(s.active_index, Some(0));
    let first = &s.stages[0].windows;
    assert_eq!(
        first.iter().map(|w| w.title.as_str()).collect::<Vec<_>>(),
        ["A", "Save", "C"]
    );
    assert_eq!(first[1].role, SavedRole::Child);
    assert!(first[2].pinned);
    assert!(first[0].key);
    assert_eq!(s.stages[1].name.as_deref(), Some("Mail"));
    assert_eq!(s.strip_slots, [1]);
    assert!(
        !is_parked(s.stages[1].windows[0].frame, FRAME),
        "restore frame, not the park frame"
    );
}

/// Builds a model with three stages, saves it, and rebuilds it on "relaunch" with new IDs.
#[test]
fn save_rematch_and_apply_rebuilds_the_same_stages() {
    let mut m = model();
    let obs = Observation::default();
    let d = |a| Decision {
        action: a,
        placement: Default::default(),
    };
    m.add_window(
        live(1, "com.example.ed", "Plan.txt"),
        &d(RuleAction::Current),
        &obs,
    );
    m.add_window(
        live(2, "com.example.term", "Shell"),
        &d(RuleAction::Current),
        &obs,
    );
    m.add_window(
        live(3, "com.example.chat", "Team"),
        &d(RuleAction::Stage("Chat".into())),
        &obs,
    );
    m.add_window(
        live(4, "com.example.web", "Docs"),
        &d(RuleAction::New),
        &obs,
    );
    m.add_window(
        live(6, "com.example.clock", "Clock"),
        &d(RuleAction::Float),
        &obs,
    );
    let saved = m.to_state();
    let json = saved.to_json();

    // Relaunch: new window IDs, one window gone, one extra.
    let relive = vec![
        live(11, "com.example.web", "Docs"),
        live(12, "com.example.term", "Shell"),
        live(13, "com.example.ed", "Plan.txt"),
        live(14, "com.example.chat", "Team"),
        live(16, "com.example.clock", "Clock"),
        live(15, "com.example.notes", "Groceries"),
        WindowMeta {
            role: WindowRole::Child {
                parent: Some(WindowId(13)),
            },
            ..live(17, "com.example.ed", "Save")
        },
    ];
    let state = StateFile::from_json(&json).expect("parses");
    let a = rematch(&state, &relive);
    let mut fresh = model();
    let actions = fresh.apply_assignment(&relive, &a, false);

    let names: Vec<Option<&str>> = fresh.stages().iter().map(|s| s.name.as_deref()).collect();
    assert_eq!(names, [None, Some("Chat"), None]);
    let members: Vec<Vec<u32>> = fresh
        .stages()
        .iter()
        .map(|s| s.windows.iter().map(|w| w.0).collect())
        .collect();
    assert_eq!(
        members,
        [vec![12, 13], vec![14], vec![11, 15]],
        "unmatched windows join behind"
    );
    assert_eq!(fresh.main_active_stage(), Some(fresh.stages()[2].id));
    assert_eq!(fresh.pinned(), [WindowId(16)]);
    assert_eq!(fresh.stage_of(WindowId(17)), fresh.stage_of(WindowId(13)));
    // Inactive stages are parked, children included; the active one is untouched.
    for id in [12, 13, 14, 17] {
        assert!(
            actions
                .iter()
                .any(|x| matches!(x, Action::Park { id: i, .. } if i.0 == id)),
            "park {id}"
        );
    }
    assert!(!actions.iter().any(|x| matches!(x, Action::SetFrame { .. })));
    assert_eq!(actions.last(), Some(&Action::StateChanged));
    let slots: Vec<usize> = fresh.strip().iter().map(|c| c.number).collect();
    assert_eq!(slots.len(), 2);
}

#[test]
fn restore_frames_only_when_asked() {
    let mut s = state(vec![(None, vec![saved("com.example.a", "A", 0)])], Some(0));
    s.stages[0].windows[0].frame = Rect::new(222.0, 111.0, 640.0, 480.0);
    let relive = [live(1, "com.example.a", "A")];
    let a = rematch(&s, &relive);

    let mut keep = model();
    let acts = keep.apply_assignment(&relive, &a, false);
    assert!(!acts.iter().any(|x| matches!(x, Action::SetFrame { .. })));
    assert_eq!(
        keep.window(WindowId(1)).map(|w| w.restore),
        Some(relive[0].frame)
    );

    let mut restore = model();
    let acts = restore.apply_assignment(&relive, &a, true);
    assert!(acts.contains(&Action::SetFrame {
        id: WindowId(1),
        frame: Rect::new(222.0, 111.0, 640.0, 480.0)
    }));
    assert!(acts.contains(&Action::Focus { id: WindowId(1) }));
}

#[test]
fn a_live_window_found_in_the_park_zone_gets_its_saved_frame() {
    let s = state(
        vec![
            (None, vec![saved("com.example.a", "A", 0)]),
            (None, vec![saved("com.example.b", "B", 0)]),
        ],
        Some(1),
    );
    let mut stuck = live(1, "com.example.a", "A");
    stuck.frame = Rect::new(1439.0, 899.0, 500.0, 400.0);
    let relive = [stuck, live(2, "com.example.b", "B")];
    let a = rematch(&s, &relive);
    let mut m = model();
    m.apply_assignment(&relive, &a, false);
    assert_eq!(
        m.window(WindowId(1)).map(|w| w.restore),
        Some(s.stages[0].windows[0].frame)
    );
}

#[test]
fn declared_stages_merge_with_saved_ones() {
    let s = state(
        vec![
            (Some("Chat"), vec![saved("com.example.chat", "Team", 0)]),
            (None, vec![saved("com.example.a", "A", 0)]),
        ],
        Some(1),
    );
    let relive = [
        live(1, "com.example.chat", "Team"),
        live(2, "com.example.a", "A"),
    ];
    let a = rematch(&s, &relive);
    let mut m = model_with(ModelOptions {
        named_stages: vec!["Work".into(), "Chat".into()],
        ..Default::default()
    });
    m.apply_assignment(&relive, &a, false);
    let names: Vec<Option<&str>> = m.stages().iter().map(|s| s.name.as_deref()).collect();
    assert_eq!(names, [Some("Work"), Some("Chat"), None]);
    assert_eq!(m.stage_of(WindowId(1)), Some(m.stages()[1].id));
    assert_eq!(m.main_active_stage(), Some(m.stages()[2].id));
}

#[test]
fn empty_saved_stages_are_dropped_unless_active() {
    let s = state(
        vec![
            (None, vec![saved("com.example.gone", "X", 0)]),
            (None, vec![]),
        ],
        Some(1),
    );
    let relive = [live(1, "com.example.a", "A")];
    let a = rematch(&s, &relive);
    let mut m = model();
    m.apply_assignment(&relive, &a, false);
    assert_eq!(m.stages().len(), 1);
    assert_eq!(
        m.stages()[0].windows,
        [WindowId(1)],
        "unmatched joins the active stage"
    );
}

#[test]
fn a_stuck_unmatched_window_on_the_active_stage_is_brought_back() {
    let s = state(vec![(None, vec![])], Some(0));
    let mut stuck = live(1, "com.example.a", "A");
    stuck.frame = Rect::new(1439.0, 899.0, 500.0, 400.0);
    let relive = [stuck];
    let a = rematch(&s, &relive);
    let mut m = model();
    let acts = m.apply_assignment(&relive, &a, false);
    let set = acts.iter().find_map(|x| match x {
        Action::SetFrame { id, frame } if id.0 == 1 => Some(*frame),
        _ => None,
    });
    assert!(set.is_some_and(|f| !is_parked(f, FRAME)));
}

// ----- first start: Apple's grouping ------------------------------------------------------

fn app_window(id: u32, pid: i32, bundle: &str) -> WindowMeta {
    WindowMeta {
        title: format!("{bundle} {id}"),
        ..meta(id, pid, bundle)
    }
}

fn first_start(m: &mut Model, live: &[WindowMeta], z: &[u32], front: Option<i32>) -> Vec<Action> {
    let a = rematch(&state(vec![], None), live);
    let hints = LaunchHints {
        z_order: z.iter().map(|&i| WindowId(i)).collect(),
        frontmost: front,
    };
    m.apply_launch(live, &a, false, &hints)
}

fn members_of(m: &Model, id: u32) -> Vec<u32> {
    let s = m.stage_of(WindowId(id)).expect("staged");
    m.stage(s).expect("stage").windows.iter().map(|w| w.0).collect()
}

#[test]
fn first_start_gives_every_app_its_own_stage_frontmost_active_recent_first() {
    let live = [
        app_window(1, 10, "com.example.a"),
        app_window(2, 10, "com.example.a"),
        app_window(3, 11, "com.example.b"),
        app_window(4, 12, "com.example.c"),
    ];
    let mut m = model();
    // Front to back: b, a (2 above 1), c.
    let acts = first_start(&mut m, &live, &[3, 2, 1, 4], Some(11));
    assert_eq!(m.stages().len(), 3);
    assert_eq!(members_of(&m, 3), [3]);
    assert_eq!(members_of(&m, 1), [2, 1], "one stage for both of a's windows, front first");
    assert_eq!(m.main_active_stage(), m.stage_of(WindowId(3)), "the frontmost app is active");
    let cards: Vec<Vec<u32>> = m
        .strip()
        .iter()
        .map(|c| c.windows.iter().map(|w| w.0).collect())
        .collect();
    assert_eq!(cards, [vec![2, 1], vec![4]], "most recently used first");
    for id in [1, 2, 4] {
        assert!(
            acts.iter().any(|x| matches!(x, Action::Park { id: i, .. } if i.0 == id)),
            "park {id}"
        );
    }
    assert!(!acts.iter().any(|x| matches!(x, Action::Park { id: i, .. } if i.0 == 3)));
}

#[test]
fn first_start_without_a_frontmost_app_uses_the_frontmost_window() {
    let live = [app_window(1, 10, "com.example.a"), app_window(2, 11, "com.example.b")];
    let mut m = model();
    first_start(&mut m, &live, &[2, 1], Some(99));
    assert_eq!(m.main_active_stage(), m.stage_of(WindowId(2)));
    assert_eq!(m.stages().len(), 2);
}

#[test]
fn first_start_one_at_a_time_gives_every_window_a_stage() {
    let live = [
        app_window(1, 10, "com.example.a"),
        app_window(2, 10, "com.example.a"),
        app_window(3, 11, "com.example.b"),
    ];
    let mut options = ModelOptions::default();
    options.settings.app_windows = crate::engine::settings::AppWindows::OneAtATime;
    let mut m = model_with(options);
    first_start(&mut m, &live, &[2, 3, 1], Some(10));
    assert_eq!(m.stages().len(), 3);
    assert_eq!(m.main_active_stage(), m.stage_of(WindowId(2)));
    let cards: Vec<u32> = m.strip().iter().map(|c| c.windows[0].0).collect();
    assert_eq!(cards, [1, 3], "the frontmost app's other window first, then by z-order");
}

#[test]
fn first_start_keeps_declared_stages_and_activates_the_frontmost_app() {
    let live = [app_window(1, 10, "com.example.a")];
    let mut m = model_with(ModelOptions {
        named_stages: vec!["Work".into()],
        ..Default::default()
    });
    first_start(&mut m, &live, &[1], Some(10));
    assert_eq!(m.stages().len(), 2);
    assert_eq!(m.main_active_stage(), m.stage_of(WindowId(1)));
    assert_eq!(m.strip()[0].name.as_deref(), Some("Work"));
}

#[test]
fn relaunch_gives_unmatched_apps_their_own_stages_and_known_apps_their_stage() {
    let s = state(
        vec![
            (None, vec![saved("com.example.a", "com.example.a 1", 0)]),
            (None, vec![saved("com.example.b", "com.example.b 2", 0)]),
        ],
        Some(0),
    );
    let live = [
        app_window(1, 10, "com.example.a"),
        app_window(2, 11, "com.example.b"),
        app_window(3, 11, "com.example.b"), // an extra window of a known app
        app_window(4, 12, "com.example.c"), // a new app
    ];
    let a = rematch(&s, &live);
    let mut m = model();
    m.apply_launch(
        &live,
        &a,
        false,
        &LaunchHints {
            z_order: vec![WindowId(4), WindowId(1), WindowId(3), WindowId(2)],
            frontmost: Some(12),
        },
    );
    assert_eq!(m.main_active_stage(), m.stage_of(WindowId(1)), "the saved active stage wins");
    assert_eq!(members_of(&m, 2), [2, 3]);
    assert_eq!(members_of(&m, 4), [4]);
    assert_eq!(m.stages().len(), 3);
}

#[test]
fn an_empty_saved_active_stage_gives_way_to_the_frontmost_new_app() {
    let s = state(
        vec![(None, vec![saved("com.example.a", "com.example.a 1", 0)]), (None, vec![])],
        Some(1),
    );
    let live = [app_window(1, 10, "com.example.a"), app_window(2, 11, "com.example.b")];
    let a = rematch(&s, &live);
    let mut m = model();
    m.apply_launch(
        &live,
        &a,
        false,
        &LaunchHints {
            z_order: vec![WindowId(2), WindowId(1)],
            frontmost: Some(11),
        },
    );
    assert_eq!(m.main_active_stage(), m.stage_of(WindowId(2)));
    assert_eq!(m.stages().len(), 2);
}
