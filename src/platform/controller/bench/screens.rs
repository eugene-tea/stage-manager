//! A screen going and coming under the Spaces engine (design r3, slices 4 and 5): the
//! orders `engine/spill.rs` tells from the logs of 11-18, 11-19 and 11-21. Each outcome reads
//! the stages (every Desktop's), what a person sees, the hidden windows the window server has
//! on a Desktop all the same, and the shadow's open disagreements on a hidden window's Desktop
//! ([`Bench::disagreed`]: the engine's home against the model's Desktop for it).

use super::scenarios::{C, Run, Scenario, expect, two_apps};
use super::*;
use crate::platform::fake::Goes;

const D: i32 = 104;
const E: i32 = 105;
/// The second screen's Desktop.
const D50: u64 = 50;

/// A window's frame on the second screen.
fn far(n: u32) -> Rect {
    Rect::new(1600.0 + 40.0 * f64::from(n), 120.0 + 30.0 * f64::from(n), 700.0, 450.0)
}

/// Two screens: on the main one Alpha's window hidden behind Bravo's; on the second
/// (Desktop 50) Charlie's in front, Delta's and Echo's hidden, a stage each.
fn two_screens(load: f64, edit: impl FnOnce(&mut Desk)) -> Bench {
    let mut b = Bench::new(EngineKind::Spaces, DEFAULTS, 1.0, 1, |d| {
        d.screen_comes_with(vec![D50]);
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_app(D, "com.example.delta", "Delta");
        d.add_app(E, "com.example.echo", "Echo");
        d.add_window(22, D, far(2), D50);
        d.add_window(23, E, far(3), D50);
        d.add_window(21, C, far(1), D50);
        two_apps(d);
        edit(d);
    });
    b.advance(5000);
    let start = (b.all_stages(), b.on_screen().into_iter().collect::<Vec<_>>(), b.on_a_desktop());
    assert_eq!(start, (vec![vec![11], vec![12], vec![21], vec![22], vec![23]], vec![12, 21], vec![12, 21]), "the start:\n{}", b.story());
    b.log.clear();
    b.desk(|d| {
        d.strip.transitions = 0;
        d.calls.clear();
        d.load = load;
    });
    b
}

/// What came of it: the stages, what shows, the hidden windows out of their stage Space,
/// the Desktop each hidden window goes back to, and the shadow's open disagreements.
fn told(b: &mut Bench) -> String {
    let stages = b.all_stages();
    let shows: Vec<u32> = b.on_screen().into_iter().collect();
    let on: Vec<u32> = b.on_a_desktop();
    let out: Vec<u32> = b.hidden().into_iter().filter(|w| on.contains(w)).collect();
    let engine = crate::platform::spaces::engine::shared();
    let homes: Vec<(u32, Option<u64>)> = b.hidden().into_iter().map(|w| (w, crate::platform::window_engine::WindowEngine::home_of(&*engine, WindowId(w)))).collect();
    let g = &b.c.desks_json()["disagree"];
    format!("stages {stages:?}; shows {shows:?}; hidden but on a Desktop {out:?}; homes {homes:?}; disagree {} shown {} homes", g["shown"], g["homes"])
}

/// The second screen goes (`keeps_ids`: its Desktop moves to the main screen under its id;
/// else it ends there), the window server letting its hidden windows out at once and 20 s
/// later (11-18). 90 s on, past the watch.
fn second_screen_goes(load: f64, keeps_ids: bool) -> Run {
    let mut b = two_screens(load, |_| {});
    b.desk(|d| d.screen_goes_how(Goes { main: false, keeps_ids, lets_out_ms: vec![150.0, 20_000.0] }));
    b.advance(90_000);
    let out = told(&mut b);
    Run::of(&b, out)
}

fn second_goes_ids_kept(load: f64) -> Run {
    second_screen_goes(load, true)
}

fn second_goes_desktop_ended(load: f64) -> Run {
    second_screen_goes(load, false)
}

/// The main screen goes (the second one is the main screen from then on), as above.
/// `locked`: while the screen is locked, unlocked 30 s later (`controller.rs`,
/// `spill_at_unlock`).
fn main_screen_goes(load: f64, keeps_ids: bool, locked: bool) -> Run {
    let mut b = two_screens(load, |_| {});
    b.desk(|d| {
        d.locked = locked;
        d.screen_goes_how(Goes { main: true, keeps_ids, lets_out_ms: vec![150.0, 20_000.0] });
    });
    b.advance(30_000);
    b.desk(|d| d.locked = false);
    b.advance(90_000);
    let out = told(&mut b);
    Run::of(&b, out)
}

fn main_goes_ids_kept(load: f64) -> Run {
    main_screen_goes(load, true, false)
}

fn main_goes_desktop_ended(load: f64) -> Run {
    main_screen_goes(load, false, false)
}

fn main_goes_while_locked(load: f64) -> Run {
    main_screen_goes(load, false, true)
}

// ----- today's outcomes (the engine's own record of each window's Desktop, `spill::hide_home`) -----

const STAGES: &str = "stages [[11], [12], [21], [22], [23]]";

#[test]
fn a_screen_goes_and_its_desktop_ends() {
    // The gone screen's front stage comes in front on the screen left, the others are its
    // cards, hidden from that screen's Desktop; nothing disagrees.
    let second = format!("{STAGES}; shows [21]; hidden but on a Desktop []; homes [(11, Some(1)), (12, Some(1)), (22, Some(1)), (23, Some(1))]; disagree 0 shown 0 homes");
    expect(second_goes_desktop_ended, [&second, &second, &second]);
    let main = format!("{STAGES}; shows [12]; hidden but on a Desktop []; homes [(11, Some(50)), (21, Some(50)), (22, Some(50)), (23, Some(50))]; disagree 0 shown 0 homes");
    expect(main_goes_desktop_ended, [&main, &main, &main]);
    expect(main_goes_while_locked, [&main, &main, &main]);
}

#[test]
fn a_screen_goes_and_its_desktop_moves_under_its_id() {
    // Not right at any load, and TIME DECIDES which way (spill::WATCH's looks against the
    // let-outs): the model takes the gone screen's Desktop into the one shown (the
    // adoption) while macOS keeps it as a Desktop of its own; the windows let out stay out,
    // or go back hidden from a Desktop the model does not have them on. Design r3, slice 5.
    expect(
        second_goes_ids_kept,
        [
            format!("{STAGES}; shows [21]; hidden but on a Desktop [22, 23]; homes [(11, Some(1)), (12, Some(1)), (22, None), (23, None)]; disagree 0 shown 0 homes"),
            format!("{STAGES}; shows [21]; hidden but on a Desktop []; homes [(11, Some(1)), (12, Some(1)), (22, Some(50)), (23, Some(50))]; disagree 0 shown 2 homes"),
            format!("{STAGES}; shows [21]; hidden but on a Desktop [23]; homes [(11, Some(1)), (12, Some(1)), (22, Some(50)), (23, None)]; disagree 0 shown 1 homes"),
        ],
    );
    let out = format!("{STAGES}; shows [12]; hidden but on a Desktop [11]; homes [(11, None), (21, Some(50)), (22, Some(50)), (23, Some(50))]; disagree 0 shown 0 homes");
    let back = format!("{STAGES}; shows [12]; hidden but on a Desktop []; homes [(11, Some(1)), (21, Some(50)), (22, Some(50)), (23, Some(50))]; disagree 0 shown 1 homes");
    expect(main_goes_ids_kept, [&out, &out, &back]);
}

/// Every scenario here, for the load table.
pub(super) const ALL: &[Scenario] = &[
    ("the second screen goes, its Desktop kept under its id (Spaces)", "engine/spill.rs, 11-18 and 11-21", second_goes_ids_kept),
    ("the second screen goes, its Desktop ended (Spaces)", "engine/spill.rs, a virtual screen 11-18", second_goes_desktop_ended),
    ("the main screen goes, its Desktop kept under its id (Spaces)", "engine/spill.rs, 11-18", main_goes_ids_kept),
    ("the main screen goes, its Desktop ended (Spaces)", "live display-gone main, 11-21", main_goes_desktop_ended),
    ("the main screen goes while locked, its Desktop ended (Spaces)", "controller.rs spill_at_unlock", main_goes_while_locked),
];
