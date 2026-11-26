//! Mission Control's opening and close under the Corner engine: how soon the app knows, and
//! what it does to the parked windows meanwhile (round looks-forward).
//!
//! The Corner engine's upkeep leans on knowing that Mission Control shows: the inert slivers'
//! shield goes off for it, the park reconcile and the patch stop judging the window list. On
//! this desk Mission Control lists every window of the Desktop in its layout
//! ([`crate::platform::fake::Desk::mission_lays_out`]), the parked ones too, so a look that
//! comes late shows as the reconcile parking a window again while Mission Control has it.
//!
//! What can tell the app before its poll does ([`Tells`]): the window server's notice
//! (1508), Mission Control's key heard by the key tap, a notice of the Dock's. Each scenario
//! says which of them the desk gives.
//!
//! **Pinned** (`looks_before.txt`): every scenario as it read before any of them was heard
//! under the Corner engine (the poll alone; printed from the commit before the wiring). With
//! none of them working ([`Silenced`]) every scenario still reads so:
//! `with_nothing_telling_every_scenario_reads_as_before`. Print the file again only when a
//! scenario is added or its script changes (`BENCH_PRINT=1 cargo test --lib
//! looks::print_before -- --nocapture` prints them with nothing telling).

use std::cell::Cell;
use std::collections::BTreeMap;

use super::scenarios::{A, C, D1, LOADS, Run, Scenario, expect, seen, two_apps};
use super::*;
use crate::engine::overview::Overview;
use crate::platform::fake::Desk;

/// What tells the app on a desk.
#[derive(Clone, Copy)]
struct Tells {
    /// The window server's notices come.
    server: bool,
    /// The key tap hears.
    tap: bool,
    /// The Dock sends `AXCreated` as Mission Control opens.
    dock: bool,
}

const NOTHING: Tells = Tells { server: false, tap: false, dock: false };
const SERVER: Tells = Tells { server: true, tap: true, dock: false };
const TAP: Tells = Tells { server: false, tap: true, dock: false };
const DOCK: Tells = Tells { server: false, tap: false, dock: true };

thread_local! {
    /// While set, every scenario here runs on a desk where nothing tells.
    static SILENCED: Cell<bool> = const { Cell::new(false) };
}

/// The scenarios of this thread run with nothing telling until dropped.
struct Silenced(());

impl Silenced {
    fn on() -> Self {
        SILENCED.with(|s| s.set(true));
        Self(())
    }
}

impl Drop for Silenced {
    fn drop(&mut self) {
        SILENCED.with(|s| s.set(false));
    }
}

/// Three apps with a window each, Bravo's stage in front, Alpha's and Charlie's windows
/// parked (two, so that Mission Control's layout of them is not read as a Desktop sliding:
/// `corner_patch::desktop_slides`), Mission Control laying out the windows, and `tells`
/// (nothing while [`Silenced`]).
fn desk(d: &mut Desk, tells: Tells) {
    let tells = if SILENCED.with(Cell::get) { NOTHING } else { tells };
    d.add_app(C, "com.example.charlie", "Charlie");
    d.add_window(14, C, frame(4), D1);
    two_apps(d);
    d.mission_lays_out = true;
    d.server_notices = tells.server;
    d.tap_hears = tells.tap;
    d.dock_tells_mission = tells.dock;
}

/// Time moves on, 10 ms at a time, until `done` (`max_ms` at most): how long it took.
fn until(b: &mut Bench, max_ms: u64, done: impl Fn(&Bench) -> bool) -> Option<u64> {
    let mut waited = 0;
    while !done(b) {
        if waited >= max_ms {
            return None;
        }
        b.advance(10);
        waited += 10;
    }
    Some(waited)
}

/// How soon after the fact, in words: at a look something brought forward, or at the poll's
/// (a second's check on a tick that rests up to 2 s).
fn how_soon(ms: Option<u64>) -> &'static str {
    match ms {
        None => "never",
        Some(0..=150) => "at once",
        Some(151..=2600) => "at the poll",
        Some(_) => "later",
    }
}

/// How long before Mission Control opens the person last clicked: the tick is still brisk,
/// so the park reconcile looks at the window list several times before the poll's next look
/// for an overview.
const WORK_BEFORE_MS: u64 = 400;

fn opens(b: &mut Bench, by_key: bool) {
    if by_key {
        b.mission_open();
    } else {
        b.desk(|d| d.mission_opens());
    }
}

fn closes(b: &mut Bench, by_key: bool) {
    if by_key {
        b.mission_close_by_key(None);
    } else {
        b.desk(|d| d.mission_closes());
    }
}

/// How many parks the park reconcile has asked of the hidden windows it found out of the
/// park zone (`ctl state`'s `strays`, their `tries`; the Corner engine asks a window already
/// at its park frame nothing more, so the desk's calls do not show them).
fn parked_again(b: &Bench) -> u32 {
    b.c.corner.strays.values().map(|s| s.tries).sum()
}

/// Mission Control opens (`by_key`: by its shortcut, else with no key: a gesture, a hot
/// corner, the Dock's item) 0.4 s after a click, stays 3 s and closes the same way. `rounds`
/// earlier opens and closes come first (the app learns from them whether the window server
/// tells). How soon the app knows it shows, counted from macOS's window being listed; how
/// often the park reconcile asked the hidden windows to park again while Mission Control
/// had them laid out; how soon it knows it closed, counted from the Dock's cover going;
/// when it infers from the window list again.
fn mission_control(load: f64, tells: Tells, by_key: bool, rounds: usize) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, |d| desk(d, tells));
    for _ in 0..rounds {
        b.advance(6000);
        opens(&mut b, by_key);
        b.advance(3000);
        closes(&mut b, by_key);
    }
    b.advance(6000);
    assert_eq!(b.c.overview.showing(), None, "the start: nothing shows:\n{}", b.story());
    // The person was at work a moment before: a click in the window in front.
    b.click(Point::new(600.0, 400.0));
    b.advance(WORK_BEFORE_MS);
    opens(&mut b, by_key);
    b.advance(fake::delays::MISSION_SHOWS as u64);
    let known = until(&mut b, 4000, |b| b.c.overview.showing() == Some(Overview::MissionControl));
    b.advance(3000);
    let parks = parked_again(&b);
    closes(&mut b, by_key);
    b.advance(fake::delays::MISSION_DOCK_GOES as u64);
    let closed = until(&mut b, 4000, |b| b.c.overview.showing().is_none());
    let resumed = until(&mut b, 4000, |b| !b.c.overview.freezes(clock::now()));
    b.advance(4000);
    let out = format!(
        "shown: known {}, the hidden windows asked to park again {parks} times; closed: known {}, the list trusted again {}; {}",
        how_soon(known),
        how_soon(closed),
        match resumed {
            Some(0) => "at once",
            Some(_) => "after its settle",
            None => "never",
        },
        seen(&mut b)
    );
    Run::of(&b, out)
}

fn by_key_server_tells(load: f64) -> Run {
    mission_control(load, SERVER, true, 0)
}

fn no_key_server_tells(load: f64) -> Run {
    mission_control(load, SERVER, false, 0)
}

fn no_key_nothing_tells(load: f64) -> Run {
    mission_control(load, NOTHING, false, 0)
}

fn by_key_tap_hears_first(load: f64) -> Run {
    mission_control(load, TAP, true, 0)
}

fn by_key_tap_hears_later(load: f64) -> Run {
    mission_control(load, TAP, true, 2)
}

fn no_key_dock_tells_later(load: f64) -> Run {
    mission_control(load, DOCK, false, 2)
}

/// Mission Control opened by its key and closed by its key 1.5 s later with the pointer over
/// Alpha's window, whose stage is hidden: macOS activates Alpha as it closes
/// (`engine/mission_close.rs`). Apple's: that stage comes in front; with `[windows]
/// mission_control_key_keeps_stage` the stage in front stays.
fn closed_by_its_key(load: f64, config: &str, tells: Tells, rounds: usize) -> Run {
    let mut b = Bench::new(EngineKind::Corner, config, load, 1, |d| desk(d, tells));
    for _ in 0..rounds {
        b.advance(6000);
        b.mission_open();
        b.advance(3000);
        b.mission_close_by_key(None);
    }
    b.advance(6000);
    b.desk(|d| d.strip.transitions = 0);
    b.mission_open();
    b.advance(1500);
    b.mission_close_by_key(Some(A));
    b.advance(8000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

const KEEPS: &str = "[windows]\nmission_control_key_keeps_stage = true\n";

fn key_close_apples(load: f64) -> Run {
    closed_by_its_key(load, DEFAULTS, SERVER, 0)
}

fn key_close_keeps_stage(load: f64) -> Run {
    closed_by_its_key(load, KEEPS, SERVER, 0)
}

fn key_close_keeps_stage_tap_alone(load: f64) -> Run {
    closed_by_its_key(load, KEEPS, TAP, 2)
}

/// Every scenario here, for the load table.
pub(super) const ALL: &[Scenario] = &[
    ("Mission Control by its key, the window server tells (Corner)", "looks-forward, 1508", by_key_server_tells),
    ("Mission Control with no key, the window server tells (Corner)", "looks-forward, 1508", no_key_server_tells),
    ("Mission Control with no key, nothing tells (Corner)", "the poll, SPACE_POLL", no_key_nothing_tells),
    ("Mission Control by its key, only the key tap could tell, the first time (Corner)", "looks-forward, the fallback not yet on", by_key_tap_hears_first),
    ("Mission Control by its key, only the key tap tells, the third time (Corner)", "looks-forward, the public fallback", by_key_tap_hears_later),
    ("Mission Control with no key, only the Dock tells, the third time (Corner)", "looks-forward, the public fallback", no_key_dock_tells_later),
    ("Mission Control closed by its key over another app (Corner)", "engine/mission_close.rs, mc-key", key_close_apples),
    ("the same with mission_control_key_keeps_stage (Corner)", "audit 11-25 H6", key_close_keeps_stage),
    ("the same, only the key tap telling, the third time (Corner)", "audit 11-25 H6", key_close_keeps_stage_tap_alone),
];

/// The pinned outcomes of before: a scenario's name, then its outcome at each load.
const BEFORE: &str = include_str!("looks_before.txt");

fn before() -> BTreeMap<&'static str, [&'static str; 3]> {
    BEFORE
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 4, "a pin is a name and three outcomes: {l}");
            (f[0], [f[1], f[2], f[3]])
        })
        .collect()
}

/// The limit of the brief: with the triggers not working the Corner engine behaves exactly
/// as before them.
#[test]
fn with_nothing_telling_every_scenario_reads_as_before() {
    let pins = before();
    assert_eq!(pins.len(), ALL.len(), "every scenario has its pin");
    for (name, _, script) in ALL {
        let want = pins.get(name).unwrap_or_else(|| panic!("{name}: no pin (module docs)"));
        for (i, load) in LOADS.iter().enumerate() {
            let run = {
                let _quiet = Silenced::on();
                script(*load)
            };
            assert_eq!(run.outcome, want[i], "{name} at load {load}, nothing telling:\n{}", run.story);
        }
    }
}

const FRONT_STAYS: &str = "front [12] shows [12] desktop 1 app 102 switches 0 slides 0";
const ALPHA_COMES: &str = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0";

fn told(shown: &str, parks: u32, closed: &str, trusted: &str) -> String {
    format!("shown: known {shown}, the hidden windows asked to park again {parks} times; closed: known {closed}, the list trusted again {trusted}; {FRONT_STAYS}")
}

/// The window server's notice (1508) brings the look under the Corner engine too: Mission
/// Control is known within a fast tick of its window being listed, however it was opened,
/// the park reconcile never judges its layout, and the look that finds its window and the
/// Dock's cover gone ends the freeze with no settle after. Before: `looks_before.txt`.
#[test]
fn the_window_servers_notice_brings_the_look() {
    let at_once = told("at once", 0, "at once", "at once");
    expect(by_key_server_tells, [&at_once, &at_once, &at_once]);
    expect(no_key_server_tells, [&at_once, &at_once, &at_once]);
}

/// With no notice from the window server the first opens are found by the poll, as before;
/// what closes Mission Control is heard by the key tap from then on (it is on while Mission
/// Control shows), so its close is looked for at once, with the settle after as before.
/// Three opens and closes with no notice, and the public triggers bring the opening's look
/// too: its key heard by the key tap, or the Dock's notice (should the Dock send one).
#[test]
fn the_public_triggers_take_over_when_the_notices_do_not_come() {
    let before = told("at the poll", 2, "at the poll", "after its settle");
    expect(no_key_nothing_tells, [&before, &before, &before]);
    let close_heard = told("at the poll", 2, "at once", "after its settle");
    expect(by_key_tap_hears_first, [&close_heard, &close_heard, &close_heard]);
    let by_tap = told("at once", 0, "at once", "after its settle");
    expect(by_key_tap_hears_later, [&by_tap, &by_tap, &by_tap]);
    let by_dock = told("at once", 0, "at the poll", "after its settle");
    expect(no_key_dock_tells_later, [&by_dock, &by_dock, &by_dock]);
}

/// Audit 11-25, H6: the option is honoured under the Corner engine. Off (Apple's way, the
/// default): the app macOS brings in front brings its stage, as before. On: the stage in
/// front stays, with the closing key heard by the key tap, whatever tells of the opening.
#[test]
fn a_close_by_its_key_keeps_the_stage_with_the_option() {
    expect(key_close_apples, [ALPHA_COMES, ALPHA_COMES, ALPHA_COMES]);
    expect(key_close_keeps_stage, [FRONT_STAYS, FRONT_STAYS, FRONT_STAYS]);
    expect(key_close_keeps_stage_tap_alone, [FRONT_STAYS, FRONT_STAYS, FRONT_STAYS]);
}

/// The check's third part (`engine/notice_trust.rs`): notices with nothing to find rest.
/// Twenty in a row, each with its look and its short watch, then none brings a look for ten
/// minutes; Mission Control opened by its key meanwhile is found at once all the same, by
/// the key tap (the public triggers serve while the notices rest).
#[test]
fn notices_with_nothing_to_find_rest_and_the_public_triggers_serve() {
    use crate::engine::server_notice::Kind;
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, 1.0, 1, |d| desk(d, SERVER));
    b.advance(6000);
    let trust = |b: &Bench| b.c.status_engine()["notices"]["trust"]["state"].as_str().map(str::to_string);
    let made = |b: &Bench| b.c.looks_status()["looks"]["made"].as_u64().unwrap_or(0);
    assert_eq!(trust(&b).as_deref(), Some("working"));
    for _ in 0..crate::engine::notice_trust::FRUITLESS {
        b.desk(|d| d.server_notice(0.0, Kind::Overview, Vec::new()));
        b.advance(400);
    }
    assert_eq!(trust(&b).as_deref(), Some("resting"), "{}", b.story());
    let looked = made(&b);
    assert!(looked >= u64::from(crate::engine::notice_trust::FRUITLESS), "each notice brought its look until then ({looked})");
    for _ in 0..10 {
        b.desk(|d| d.server_notice(0.0, Kind::Overview, Vec::new()));
        b.advance(400);
    }
    assert_eq!(made(&b), looked, "resting: no look for a notice");
    assert_eq!(b.c.status_engine()["notices"]["uses"]["rested"].as_u64(), Some(10));
    // Its key, heard by the tap: found at once.
    b.advance(2000);
    b.mission_open();
    b.advance(fake::delays::MISSION_SHOWS as u64);
    let known = until(&mut b, 4000, |b| b.c.overview.showing() == Some(Overview::MissionControl));
    assert_eq!(how_soon(known), "at once", "{}", b.story());
    assert_eq!(b.c.looks_status()["looks"]["shown_after_its_key"].as_u64(), Some(1));
}

/// Prints the pins of before (module docs).
#[test]
fn print_before() {
    if std::env::var("BENCH_PRINT").is_err() {
        return;
    }
    let _quiet = Silenced::on();
    for (name, _, script) in ALL {
        let runs = LOADS.map(script);
        println!("PIN\t{name}\t{}\t{}\t{}", runs[0].outcome, runs[1].outcome, runs[2].outcome);
    }
}

/// Each scenario with what its desk tells, beside the same with nothing telling:
/// `BENCH_PRINT=1 cargo test --lib looks::print_them -- --nocapture`.
#[test]
fn print_them() {
    if std::env::var("BENCH_PRINT").is_err() {
        return;
    }
    for (name, _, script) in ALL {
        for load in LOADS {
            let quiet = {
                let _quiet = Silenced::on();
                script(load)
            };
            let told = script(load);
            println!("## {name} at {load}\n   nothing telling: {}\n   as the desk tells: {}", quiet.outcome, told.outcome);
        }
    }
}
