//! The shadow's verdicts in the order of events of the 11-21 screen change (a log: the
//! external screen back after a day on the laptop alone, Desktops 1 and 4 under the same ids,
//! a Dock click 36 s after the change). Made-up titles only.

use std::time::{Duration, Instant};

use super::*;
use crate::engine::model::WindowMeta;
use crate::engine::model::desks::Desk;
use crate::engine::persist::LaunchHints;
use crate::engine::testkit::{MAIN, meta, model, w};

/// Terminal's window 1 on Desktop 1; on Desktop 4 the editor's window 2 (in front) and
/// Terminal's window 3; Desktop 1 shown at the end.
fn two_desktops() -> Model {
    let mut m = model();
    let mut arrive = |d: u64, live: &[WindowMeta], absent: &[u32]| {
        let absent: Vec<WindowId> = absent.iter().map(|&id| w(id)).collect();
        m.arrive_on(MAIN, Some(&Desk { id: d, uuid: format!("U{d}") }), None, live, &absent, &LaunchHints::default());
    };
    let terminal = |id| meta(id, 30, "com.example.terminal");
    arrive(1, &[terminal(1)], &[]);
    arrive(4, &[meta(2, 20, "com.example.editor"), terminal(3)], &[1]);
    arrive(1, &[terminal(1)], &[2, 3]);
    m
}

fn at(t0: Instant, s: f64) -> Instant {
    t0 + Duration::from_secs_f64(s)
}

#[test]
fn the_11_21_screen_change_the_hide_on_the_desktop_shown_disagrees_until_the_window_goes() {
    let mut m = two_desktops();
    let (one, four) = (m.space_of_desk(1).expect("bound"), m.space_of_desk(4).expect("bound"));
    assert_ne!(one, four);
    let mut shadow = Shadow::default();
    let t0 = Instant::now();
    // The laptop goes (0 s) and the screen unlocks (6.5 s): Desktop 1 shown, windows 2 and 3
    // held hidden from Desktop 4. Nothing disagrees.
    for s in [0.0, 6.5, 30.0] {
        let seen = disagreements(&m, &[(MAIN, 1)], &[(w(2), 4), (w(3), 4)]);
        assert!(shadow.look(seen, at(t0, s)).is_empty());
    }
    // 36 s: a Dock click to Terminal, whose window 3 waits on Desktop 4: the editor's window,
    // shown there, is hidden while 1 still shows; the engine took the Desktop shown as its
    // home (the 11-21 incident, before its fix). The model's Desktop for it is 4.
    let seen = disagreements(&m, &[(MAIN, 1)], &[(w(2), 1), (w(3), 4)]);
    let changes = shadow.look(seen, at(t0, 36.0));
    let home = Sight { subject: Subject::Home(w(2)), space: four, model: Some(4), server: 1 };
    assert_eq!(changes, vec![Change::Began(home)]);
    // 36.4 s: macOS slid to 4 and a look comes before the arrival: the model still on 1.
    let seen = disagreements(&m, &[(MAIN, 4)], &[(w(2), 1)]);
    let shown = Sight { subject: Subject::Shown(MAIN), space: one, model: Some(1), server: 4 };
    assert_eq!(shadow.look(seen, at(t0, 36.4)), vec![Change::Began(shown)], "the window's own goes on, said once");
    assert_eq!((shadow.shown_open(), shadow.homes_open()), (1, 1));
    // The arrival on 4 (Terminal's window 3 on screen): the model and the listing agree again.
    m.arrive_on(MAIN, Some(&Desk { id: 4, uuid: "U4".into() }), None, &[meta(3, 30, "com.example.terminal")], &[w(1)], &LaunchHints::default());
    assert_eq!(m.last_arrival().desk.map(|d| d.1), Some(crate::engine::model::desks::Agree::Yes));
    let seen = disagreements(&m, &[(MAIN, 4)], &[(w(2), 1), (w(1), 1)]);
    let ended = shadow.look(seen, at(t0, 36.5));
    assert_eq!(ended.len(), 1);
    assert!(matches!(ended[0], Change::Ended { sight, lasted } if sight == shown && lasted == Duration::from_millis(100)));
    // 39 s: the editor's window counted gone from Desktop 4 (it is on 1 for the window
    // server): the model lets it go, so nothing is compared for it any more.
    let _ = m.window_left_space(w(2));
    let seen = disagreements(&m, &[(MAIN, 4)], &[(w(2), 1), (w(1), 1)]);
    let ended = shadow.look(seen, at(t0, 39.0));
    assert!(matches!(ended[..], [Change::Ended { sight, lasted }] if sight == home && lasted == Duration::from_secs(3)));
    assert_eq!((shadow.shown_open(), shadow.homes_open(), shadow.shown_begun, shadow.homes_begun), (0, 0, 1, 1));
    assert_eq!(shadow.longest(at(t0, 60.0)), Duration::from_secs(3));
}

#[test]
fn the_11_21_screen_change_with_the_fix_the_hide_keeps_its_own_desktop() {
    let m = two_desktops();
    let mut shadow = Shadow::default();
    let t0 = Instant::now();
    // 36 s: the same hide, on Desktop 4, its own (the 11-21 fix): nothing disagrees.
    let seen = disagreements(&m, &[(MAIN, 1)], &[(w(2), 4), (w(3), 4)]);
    assert!(shadow.look(seen, at(t0, 36.0)).is_empty());
    // A Desktop neither side knows (a full-screen app's Space would be left out by the
    // caller; an unbound Desktop here) says nothing either way.
    let seen = disagreements(&m, &[], &[(w(9), 77)]);
    assert!(shadow.look(seen, at(t0, 37.0)).is_empty());
    assert_eq!(shadow.longest(at(t0, 40.0)), Duration::ZERO);
}
