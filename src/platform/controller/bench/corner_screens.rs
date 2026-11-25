//! A screen going and coming under the Corner engine, with the window server's Desktops read
//! (round corner-desktops): the orders `screens.rs` plays for the Spaces engine. A parked
//! window stays on its Desktop, so macOS moves it with that Desktop as it moves a shown one.
//! Each outcome reads the stages (every Desktop's), the stage in front on the main screen,
//! what a person sees, and the windows the model counts hidden. What each read with the
//! Desktops guessed from the windows on screen is pinned in `guessed.txt`.

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

/// The windows a person sees: listed on screen, in no screen's park zone.
fn shows(b: &mut Bench) -> Vec<u32> {
    b.desk(|d| d.windows.values().filter(|w| w.layer == 0 && d.on_screen(w) && !d.screens.iter().any(|s| s.display.parks(w.frame))).map(|w| w.id).collect())
}

/// Two screens: on the main one Alpha's window hidden behind Bravo's; on the second
/// (Desktop 50) Charlie's in front, Delta's and Echo's hidden, a stage each.
fn two_screens(load: f64) -> Bench {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, 1.0, 1, |d| {
        d.screen_comes_with(vec![D50]);
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_app(D, "com.example.delta", "Delta");
        d.add_app(E, "com.example.echo", "Echo");
        d.add_window(22, D, far(2), D50);
        d.add_window(23, E, far(3), D50);
        d.add_window(21, C, far(1), D50);
        two_apps(d);
    });
    b.advance(5000);
    let start = (b.all_stages(), shows(&mut b));
    assert_eq!(start, (vec![vec![11], vec![12], vec![21], vec![22], vec![23]], vec![12, 21]), "the start:\n{}", b.story());
    b.log.clear();
    b.desk(|d| {
        d.strip.transitions = 0;
        d.calls.clear();
        d.load = load;
    });
    b
}

/// What came of it.
fn told(b: &mut Bench) -> String {
    let hidden: Vec<u32> = b.hidden().into_iter().collect();
    format!("stages {:?}; front {:?}; shows {:?}; hidden {hidden:?}", b.all_stages(), b.front(), shows(b))
}

/// The second screen goes (`keeps_ids`: its Desktop moves to the main screen under its id;
/// else it ends there, its windows on the Desktop shown). 90 s on.
fn second_screen_goes(load: f64, keeps_ids: bool) -> Run {
    let mut b = two_screens(load);
    b.desk(|d| d.screen_goes_how(Goes { main: false, keeps_ids, ..Goes::default() }));
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

/// The main screen goes (the second one is the main screen from then on). `ends_own`: the
/// screen left shows the gone screen's Desktop and its own ends.
fn main_screen_goes(load: f64, keeps_ids: bool, ends_own: bool) -> Run {
    let mut b = two_screens(load);
    b.desk(|d| d.screen_goes_how(Goes { main: true, keeps_ids, ends_own, lets_out_ms: Vec::new() }));
    b.advance(120_000);
    let out = told(&mut b);
    Run::of(&b, out)
}

fn main_goes_ids_kept(load: f64) -> Run {
    main_screen_goes(load, true, false)
}

fn main_goes_other_desktop_ends(load: f64) -> Run {
    main_screen_goes(load, true, true)
}

fn main_goes_desktop_ended(load: f64) -> Run {
    main_screen_goes(load, false, false)
}

/// The second screen goes and comes back 40 s later: its Desktop back on it under its id
/// (`keeps_ids`), else a new one.
fn second_screen_goes_and_comes_back(load: f64, keeps_ids: bool) -> Run {
    let mut b = two_screens(load);
    b.desk(|d| d.screen_goes_how(Goes { main: false, keeps_ids, ..Goes::default() }));
    b.advance(40_000);
    b.desk(|d| d.screen_comes_back());
    b.advance(90_000);
    let out = told(&mut b);
    Run::of(&b, out)
}

fn back_ids_kept(load: f64) -> Run {
    second_screen_goes_and_comes_back(load, true)
}

fn back_desktop_ended(load: f64) -> Run {
    second_screen_goes_and_comes_back(load, false)
}

/// Every scenario here, for the load table.
pub(super) const ALL: &[Scenario] = &[
    ("the second screen goes, its Desktop kept under its id (Corner)", "screens.rs's twin", second_goes_ids_kept),
    ("the second screen goes, its Desktop ended (Corner)", "screens.rs's twin", second_goes_desktop_ended),
    ("the second screen goes and comes back, its Desktop kept (Corner)", "screens.rs's twin", back_ids_kept),
    ("the second screen goes and comes back, its Desktop ended (Corner)", "screens.rs's twin", back_desktop_ended),
    ("the main screen goes, its Desktop kept under its id (Corner)", "screens.rs's twin", main_goes_ids_kept),
    ("the main screen goes, its Desktop ended (Corner)", "screens.rs's twin", main_goes_desktop_ended),
    ("the main screen goes, its Desktop moves over and the other screen's ends (Corner)", "screens.rs's twin", main_goes_other_desktop_ends),
];

/// A display change is taken by the windows on screen under the Corner engine, with the
/// reads working too (`Controller::display_change_guessed`): each scenario reads as it does
/// with the reads off (`guessed.txt`), nothing disagrees and the reads never rest.
///
/// Known and unchanged: with the gone screen's Desktop kept under its id, the windows of
/// the Desktop left on the screen that stayed leave their stages while macOS shows the
/// other one (stages [[21], [22], [23]]), and come back as new windows when theirs shows.
#[test]
fn a_display_change_reads_as_with_the_desktops_guessed() {
    let pins = super::guessed::pins();
    for (name, _, script) in ALL {
        let want = pins.get(name).unwrap_or_else(|| panic!("{name}: no pin"));
        expect(*script, *want);
    }
}

/// Prints each scenario with the reads off and on: `BENCH_PRINT=1 cargo test --lib
/// corner_screens::print_them -- --nocapture`.
#[test]
fn print_them() {
    if std::env::var("BENCH_PRINT").is_err() {
        return;
    }
    for (name, _, script) in ALL {
        for load in super::scenarios::LOADS {
            let off = {
                let _guess = super::guessed::Forced::on();
                script(load)
            };
            let on = script(load);
            println!("## {name} at {load}\n   guessed: {}\n   read:    {} (rests {}, shadow {})", off.outcome, on.outcome, on.rests, on.disagreed);
        }
    }
}
