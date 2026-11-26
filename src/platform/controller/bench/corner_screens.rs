//! A screen going and coming under the Corner engine, with the window server's Desktops read
//! (round corner-desktops): the orders `screens.rs` plays for the Spaces engine. A parked
//! window stays on its Desktop, so macOS moves it with that Desktop as it moves a shown one.
//! Each outcome reads the stages (every Desktop's), the stage in front on the main screen,
//! what a person sees, and the windows the model counts hidden. What each read with the
//! Desktops guessed from the windows on screen is pinned in `guessed.txt`.

use super::scenarios::{A, B, C, D1, D2, Run, Scenario, expect, two_apps};
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
    b.desk(|d| d.screen_goes_how(Goes { main: true, keeps_ids, ends_own, ..Goes::default() }));
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

/// The live phase's apps (`scripts/test_display_gone.py`, 11-25): Fixture with windows on
/// both screens, Charlie too.
const F: i32 = 106;
/// The third Desktop of the screen left.
const D3: u64 = D2 + 1;

/// The live phase's setup, as the virtual screen leaves it: the screen that stays has three
/// Desktops, the second shown, with Fixture's window 31 in front and Charlie's 32 hidden; the
/// screen that goes shows a Desktop with Fixture's 41 and 42 in front (one stage) and
/// Alpha's 43, Bravo's 44 and Charlie's 45 hidden. `main_goes`: the screen that goes is the
/// main one and holds the three Desktops (macOS moved them there when it took the main
/// role), the one that stays a Desktop of its own (50).
fn live_phase(load: f64, main_goes: bool) -> Bench {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, 1.0, 3, |d| {
        d.starts_on(D2);
        d.screen_comes_with(vec![D50]);
        d.add_app(A, "com.example.alpha", "Alpha");
        d.add_app(B, "com.example.bravo", "Bravo");
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_app(F, "com.example.fixture", "Fixture");
        let (going, staying) = if main_goes { (D2, D50) } else { (D50, D2) };
        let on = |going: bool, n: u32| if going == main_goes { frame(n) } else { far(n) };
        let (near, away) = (|n| on(true, n), |n| on(false, n));
        d.add_window(32, C, away(2), staying);
        d.add_window(31, F, away(1), staying);
        d.add_window(45, C, near(5), going);
        d.add_window(44, B, near(4), going);
        d.add_window(43, A, near(3), going);
        d.add_window(42, F, near(2), going);
        d.add_window(41, F, near(1), going);
        d.front = Some(F);
    });
    b.advance(5000);
    let start = (b.all_stages(), shows(&mut b));
    assert_eq!(start, (vec![vec![31], vec![32], vec![41, 42], vec![43], vec![44], vec![45]], vec![31, 41, 42]), "the start:\n{}", b.story());
    b.log.clear();
    b.desk(|d| {
        d.strip.transitions = 0;
        d.calls.clear();
        d.load = load;
    });
    b
}

/// Each window's stage id, by window.
fn stage_ids(b: &Bench) -> Vec<(u32, u64)> {
    [31, 32, 41, 42, 43, 44, 45].into_iter().filter_map(|w| b.c.model.as_ref().and_then(|m| m.stage_of(WindowId(w))).map(|s| (w, s.0))).collect()
}

/// What the live phase reads (`look`): the windows whose stage is another than before the
/// screen went, 60 s on and after a visit to each Desktop of the screen left, and what shows
/// on the Desktop shown at the end.
fn live_told(b: &mut Bench, before: &[(u32, u64)]) -> String {
    let left = |b: &Bench| -> Vec<u32> {
        let now = stage_ids(b);
        before.iter().filter(|(w, s)| !now.contains(&(*w, *s))).map(|(w, _)| *w).collect()
    };
    let watched = left(b);
    let shown_after = shows(b);
    for to in [D1, D2, D3, D2] {
        b.go_to_desktop(to, true);
        b.advance(4000);
    }
    let toured = left(b);
    format!("left their stage {watched:?}, after the tour {toured:?}; shows {shown_after:?}, then {:?}; stages {:?}", shows(b), b.all_stages())
}

/// The live phase's `secondary` round: the screen right of the main one goes; its Desktop
/// ends, its windows land on the main screen's first Desktop, and the main screen slides to
/// that Desktop (live 11-25, every round). The main screen's own Desktop shown before (Fixture
/// 31 in front, Charlie 32 hidden) is a Desktop not shown from then on.
fn live_secondary(load: f64) -> Run {
    let mut b = live_phase(load, false);
    let before = stage_ids(&b);
    b.desk(|d| d.screen_goes_how(Goes { main: false, keeps_ids: false, lands_on: Some(D1), shows_landing: true, ..Goes::default() }));
    b.advance(60_000);
    let out = live_told(&mut b, &before);
    Run::of(&b, out)
}

/// The live phase's `main` round: the main screen goes; its three Desktops move to the screen
/// left under their ids, the one it showed shown there; the screen left's own Desktop ends and
/// its windows (Fixture 31, Charlie 32) land on the first of them, not shown (live 11-25,
/// every round).
fn live_main(load: f64) -> Run {
    let mut b = live_phase(load, true);
    let before = stage_ids(&b);
    b.desk(|d| d.screen_goes_how(Goes { main: true, keeps_ids: true, ends_own: true, lands_on: Some(D1), ..Goes::default() }));
    b.advance(60_000);
    let out = live_told(&mut b, &before);
    Run::of(&b, out)
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
    ("the live phase's screen right of the main one goes, its windows onto the first Desktop (Corner)", "scripts/test_display_gone.py secondary, 11-25", live_secondary),
    ("the live phase's main screen goes, the other's windows onto the first Desktop (Corner)", "scripts/test_display_gone.py main, 11-25", live_main),
];

/// A display change is taken from the window server's listing under the Corner engine as
/// any Space change is (round display-gone): every scenario reads as it does with the reads
/// off (`guessed.txt`, 1.9.2's way: the change taken by the windows on screen for its
/// settle) but where the screen left stops showing its own Desktop. There, before, that
/// Desktop's windows left their stages as macOS showed the other one and came back as new
/// stages when theirs showed (live 11-25, every round of `display-gone` under the Corner
/// engine); now every stage keeps its windows. Nothing disagrees and the reads never rest.
#[test]
fn a_display_change_is_taken_from_the_listing() {
    let pins = super::guessed::pins();
    let all = "stages [[11], [12], [21], [22], [23]]; front Some([21]); shows [21]; hidden [11, 12, 22, 23]";
    let kept = "left their stage [], after the tour []; shows [41, 42], then [31]; stages [[31], [32], [41, 42], [43], [44], [45]]";
    let main = "left their stage [], after the tour []; shows [41, 42], then [41, 42]; stages [[31], [32], [41, 42], [43], [44], [45]]";
    let read: [(&str, &str); 3] = [
        // Before: stages [[21], [22], [23]], Alpha's and Bravo's windows out of their stages.
        ("the second screen goes, its Desktop kept under its id (Corner)", all),
        ("the live phase's screen right of the main one goes, its windows onto the first Desktop (Corner)", kept),
        ("the live phase's main screen goes, the other's windows onto the first Desktop (Corner)", main),
    ];
    for (name, _, script) in ALL {
        let pinned = pins.get(name).unwrap_or_else(|| panic!("{name}: no pin"));
        match read.iter().find(|(n, _)| n == name) {
            Some((_, want)) => {
                assert!(pinned.iter().all(|p| p != want), "{name}: read as with the reads off");
                expect(*script, [*want; 3]);
            }
            None => expect(*script, *pinned),
        }
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

