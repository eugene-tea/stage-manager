//! The incidents, replayed through the real handlers (docs/DEVELOPMENT.md, "The bench").
//!
//! Each scenario is a function of the load factor that scripts the incident as its log
//! tells it and returns what came of it ([`Run`]: an outcome in a few words, the time
//! decisions taken). Its test runs it at load 1, 3 and 10 and asserts today's outcome at
//! each. Where a load changes the outcome the assertion is marked `// TIME DECIDES:` with
//! the constant that decided; [`load_table`] prints every scenario's three outcomes and the
//! constants whose limits were crossed only under load (judged outside, or a wait ended by
//! its cap, at load 3 or 10 and not at load 1), whether or not the outcome changed.

use super::*;
use crate::platform::fake::Desk;

/// The load factors every scenario runs at.
pub(super) const LOADS: [f64; 3] = [1.0, 3.0, 10.0];

/// What a scenario's run came to.
pub(super) struct Run {
    /// The outcome, in a few words: what a person would see.
    pub(super) outcome: String,
    /// The time decisions and waits' ends, in order.
    pub(super) margins: Vec<margin::test::Seen>,
    /// The notices handled, in order.
    pub(super) story: String,
    /// What each focus and activation notice came to, in order.
    pub(super) trail: Vec<String>,
    /// The shadow's count since the launch: how often the model's Desktop shown and the
    /// window server's listing disagreed (`ctl state`'s `desks.disagree.shown_begun`).
    pub(super) disagreed: u64,
    /// The shadow's open disagreements at the end on a hidden window's Desktop: the engine's
    /// home against the model's Desktop for it (`desks.disagree.homes`).
    pub(super) homes: u64,
}

impl Run {
    pub(super) fn of(b: &Bench, outcome: String) -> Self {
        Self { outcome, margins: b.margins(), story: b.story(), trail: b.trail(), disagreed: b.disagreed().0, homes: b.c.desks_json()["disagree"]["homes"].as_u64().unwrap_or(0) }
    }
}

/// A scenario: its name, the doc comment or commit that tells its incident, and its script.
pub(super) type Scenario = (&'static str, &'static str, fn(f64) -> Run);

/// Asserts a scenario's outcome at load 1, 3 and 10, printing the story of the first that
/// differs.
pub(super) fn expect<S: AsRef<str>>(script: fn(f64) -> Run, want: [S; 3]) {
    for (load, want) in LOADS.iter().zip(want) {
        let run = script(*load);
        assert_eq!(run.outcome, want.as_ref(), "at load {load}:\n{}", run.story);
        // Design r3: while the listing decides, the model never shows another Desktop than
        // the window server lists (the shadow, kept as the check).
        assert_eq!(run.disagreed, 0, "at load {load}: the model and the window server's listing disagreed on the Desktop shown:\n{}", run.story);
        // Slice 4: the engine hides each window from the Desktop the model has it on.
        assert_eq!(run.homes, 0, "at load {load}: the engine and the model disagree on a hidden window's Desktop at the end:\n{}", run.story);
    }
}

pub(super) const A: i32 = 101;
pub(super) const B: i32 = 102;
pub(super) const C: i32 = 103;
pub(super) const D1: u64 = fake::FIRST_DESKTOP;
pub(super) const D2: u64 = fake::FIRST_DESKTOP + 1;

/// Two apps with a window each on one Desktop, Bravo in front: a stage each, Bravo's shown.
pub(super) fn two_apps(d: &mut Desk) {
    d.add_app(A, "com.example.alpha", "Alpha");
    d.add_app(B, "com.example.bravo", "Bravo");
    d.add_window(11, A, frame(1), D1);
    d.add_window(12, B, frame(2), D1);
    d.front = Some(B);
}

/// Alpha with a window on each of two Desktops and Bravo with one on the first, Bravo in
/// front there: on Desktop 1 Alpha's window waits in a hidden stage.
pub(super) fn alpha_on_two_desktops(d: &mut Desk) {
    d.add_app(A, "com.example.alpha", "Alpha");
    d.add_app(B, "com.example.bravo", "Bravo");
    d.add_window(13, A, frame(3), D2);
    d.add_window(11, A, frame(1), D1);
    d.add_window(12, B, frame(2), D1);
    d.front = Some(B);
}

/// Alpha and Bravo with a window each on each of two Desktops, Bravo in front on both, and
/// both Desktops visited once (the model knows a Desktop's windows from its first visit):
/// Desktop 2 was left with Alpha's stage in front there (a card click); back on Desktop 1
/// Bravo's stage is in front and Alpha's window there hidden.
pub(super) fn both_desktops_known(engine: EngineKind, load: f64, edit: impl FnOnce(&mut Desk)) -> Bench {
    // The history runs on the Mac as measured; the load is the incident's.
    let mut b = Bench::new(engine, DEFAULTS, 1.0, 2, |d| {
        d.add_app(A, "com.example.alpha", "Alpha");
        d.add_app(B, "com.example.bravo", "Bravo");
        d.add_window(13, A, frame(3), D2);
        d.add_window(15, B, frame(5), D2);
        d.add_window(11, A, frame(1), D1);
        d.add_window(12, B, frame(2), D1);
        d.front = Some(B);
        edit(d);
    });
    b.go_to_desktop(D2, true);
    b.advance(4000);
    let alpha_there = b.stage_of(13);
    b.card_click(alpha_there);
    b.advance(4000);
    b.go_to_desktop(D1, true);
    b.advance(6000);
    let start = (b.front(), b.desktop_shown(), b.front_app(), b.on_screen().into_iter().collect::<Vec<_>>());
    assert_eq!(start, (Some(vec![12]), D1, Some(B), vec![12]), "the start:\n{}", b.story());
    b.log.clear();
    crate::engine::attribution::trail::clear();
    b.desk(|d| {
        d.strip.transitions = 0;
        d.calls.clear();
        d.load = load;
    });
    b
}

/// What a person sees: the stage in front, the Desktop, the app in front, and how many
/// animated switches and Desktop slides it took.
pub(super) fn seen(b: &mut Bench) -> String {
    let slides = slides(b);
    let front = b.front().map_or("none".to_string(), |w| format!("{w:?}"));
    let shows: Vec<u32> = b.on_screen().into_iter().collect();
    format!("front {front} shows {shows:?} desktop {} app {:?} switches {} slides {slides}", b.desktop_shown(), b.front_app().unwrap_or(0), b.transitions())
}

/// Time moves on until window `id` is on no Desktop again (a window put home taken back),
/// `max_ms` at most; how long that took (`None`: it is still there).
fn ms_until_back(b: &mut Bench, id: u32, max_ms: u64) -> Option<u64> {
    let mut waited = 0;
    while b.desktop_of(id).is_some() {
        if waited >= max_ms {
            return None;
        }
        b.advance(10);
        waited += 10;
    }
    Some(waited)
}

/// How soon, in words (the exact ms vary with the load's costs).
fn soon(ms: Option<u64>) -> &'static str {
    match ms {
        None => "never",
        Some(0..=100) => "at once",
        Some(101..=1000) => "within 1 s",
        Some(1001..=4500) => "within 4.5 s",
        Some(_) => "at the 5 s cap",
    }
}

/// [`ms_until_back`] in words.
fn taken_back(b: &mut Bench, id: u32, max_ms: u64) -> &'static str {
    soon(ms_until_back(b, id, max_ms))
}

/// How many Space change notices the story holds.
pub(super) fn slides(b: &Bench) -> usize {
    b.log.iter().filter(|l| l.contains("space change notice")).count()
}

// ----- 1. a card click's own late focus echo -------------------------------------------------

/// `engine/stale_activation.rs` (the owner's 11-03 14:09:57, the Mac's load at ~110, notices
/// 1-2.6 s late): a card click for Alpha's stage, then one for Bravo's 400 ms later, while the
/// WM thread is held up 1.5 s; Alpha's activation and focus are handled long after Bravo's
/// switch began. They must not bring Alpha's stage back.
fn card_clicks_under_a_busy_wm_thread(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, two_apps);
    let (alpha, bravo) = (b.stage_of(11), b.stage_of(12));
    b.card_click(alpha);
    b.advance(400);
    b.card_click(bravo);
    b.advance(5);
    b.busy(1500);
    b.advance(6000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

#[test]
fn a_card_clicks_own_late_echo_is_not_taken_for_the_users() {
    let want = "front [12] shows [12] desktop 1 app 102 switches 2 slides 0";
    expect(card_clicks_under_a_busy_wm_thread, [want, want, want]);
}

// ----- 2. our activation reported late by a busy app ------------------------------------------

/// `engine/causes.rs`, `Causes::is_app_echo` (11-14, the awaited path): a card click for Alpha's
/// stage; Alpha, busy, reports the activation 1.2 s later, past the echo window. It is ours
/// (the first activation of the app since we asked, nothing pressed between): no second
/// switch, and Bravo's stage, clicked 2 s after, stays.
fn our_activation_reported_late(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, |d| {
        two_apps(d);
        d.apps.get_mut(&A).expect("Alpha").activates_ms = Some(1200.0);
    });
    let alpha = b.stage_of(11);
    b.card_click(alpha);
    b.advance(20_000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

#[test]
fn our_activation_reported_late_by_a_busy_app_is_still_ours() {
    let want = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0";
    expect(our_activation_reported_late, [want, want, want]);
}

// ----- 3, 4. a Dock click on an app whose stage is hidden here (Spaces) ------------------------

/// `controller/arrival.rs` (the Dock press's lend, rounds dock-put-home and lends): under
/// the Spaces engine Alpha's window here waits in a stage Space, on no Desktop, and Alpha
/// shows a window on Desktop 2. A Dock click on Alpha: its window here is put home at the
/// press, macOS activates Alpha here, its stage comes in front; no Desktop slides.
fn dock_click_on_a_stage_hidden_here(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Spaces, load, |_| {});
    b.dock_click(A, 90);
    b.advance(8000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

#[test]
fn a_dock_click_on_an_app_whose_stage_is_hidden_here_brings_it_with_no_slide() {
    let want = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0";
    expect(dock_click_on_a_stage_hidden_here, [want, want, want]);
}

/// The same with Alpha taking the activation 1.2 s after the click (review 11-21, R2: "a
/// busy app that activates after 1 s gets the slide the lend exists to stop"; since round
/// r2-lends the lend ends by events, `LEND_WAIT` a 5 s cap).
fn dock_click_with_a_late_activation(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Spaces, load, |_| {});
    b.desk(|d| d.apps.get_mut(&A).expect("Alpha").activates_ms = Some(1200.0));
    b.dock_click(A, 90);
    b.advance(30_000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

/// The same with Alpha taking it 0.4 s after the click (before r2-lends: inside the lend's
/// 1 s on the Mac as measured, outside it three times slower).
fn dock_click_with_a_slow_activation(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Spaces, load, |_| {});
    b.desk(|d| d.apps.get_mut(&A).expect("Alpha").activates_ms = Some(400.0));
    b.dock_click(A, 90);
    b.advance(30_000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

#[test]
fn a_dock_click_whose_activation_comes_late() {
    let here = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0";
    let slid = "front [13] shows [13] desktop 2 app 101 switches 0 slides 1";
    // Since r2-lends the lend waits for its app's activation (before: back after 1 s, and
    // the slide at load 3 and 10).
    expect(dock_click_with_a_slow_activation, [here, here, here]);
    // TIME DECIDES: LEND_WAIT (a 5 s cap since r2-lends; before, 1 s and the slide at every
    // load). At load 10 Alpha takes the activation 12 s after the click: the cap took the
    // window back, and macOS slides to Alpha's other Desktop.
    expect(dock_click_with_a_late_activation, [here, here, slid]);
    let refuted = "put home true, back at once after the other activation; front [12] shows [12] desktop 1 app 91 switches 0 slides 0";
    expect(dock_click_then_another_apps_activation, [refuted, refuted, refuted]);
    // TIME DECIDES: LEND_WAIT, as meant: with no activation and no press, its 5 s cap is
    // the only end.
    let capped = "put home true, back at the 5 s cap; front [12] shows [12] desktop 1 app 102 switches 0 slides 0";
    expect(dock_click_whose_activation_never_comes, [capped, capped, capped]);
}

/// `engine/lend_wait.rs`, `controller/arrival.rs` `lent_activation_elsewhere` (round
/// r2-lends): a Dock click on Alpha, which never takes the activation; 300 ms after the
/// release another app (Finder) is activated. That refutes the lend: the window put home goes
/// back at once, nothing switches.
fn dock_click_then_another_apps_activation(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Spaces, load, |_| {});
    b.desk(|d| d.apps.get_mut(&A).expect("Alpha").activates_ms = Some(10_000_000.0));
    b.dock_click(A, 90);
    b.advance(300);
    let lent = b.desktop_of(11).is_some();
    b.activate(fake::FINDER_PID);
    // The other app's own activation delay comes first.
    let notice = b.desk(|d| d.at_load(fake::delays::ACTIVATION).as_millis() as u64) + 1;
    b.advance(notice);
    let back = taken_back(&mut b, 11, 8000);
    b.advance(8000);
    let out = format!("put home {lent}, back {back} after the other activation; {}", seen(&mut b));
    Run::of(&b, out)
}

/// The same with nothing after the click: no activation, no press. Only the lend's cap ends
/// it (`LEND_WAIT`, 5 s since r2-lends).
fn dock_click_whose_activation_never_comes(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Spaces, load, |_| {});
    b.desk(|d| d.apps.get_mut(&A).expect("Alpha").activates_ms = Some(10_000_000.0));
    b.dock_click(A, 90);
    b.advance(300);
    let lent = b.desktop_of(11).is_some();
    let back = taken_back(&mut b, 11, 9000);
    b.advance(8000);
    let out = format!("put home {lent}, back {back}; {}", seen(&mut b));
    Run::of(&b, out)
}

// ----- 5. ⌘Tab to such an app ----------------------------------------------------------------

/// `controller/switcher.rs` (round cmd-tab, the owner's VS Code and Mail on 11-15): ⌘Tab to
/// Alpha, whose window here is hidden and which shows one on Desktop 2; ⌘ let go 400 ms
/// after the Tab. Its window is put home while the switcher shows; macOS activates Alpha
/// here; no slide.
fn cmd_tab_released(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Spaces, load, |_| {});
    b.cmd_tab(vec![B, A]);
    b.advance(400);
    b.cmd_tab_release();
    b.advance(8000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

/// The same, cancelled with Escape (never heard by the input monitor) and ⌘ let go 300 ms
/// later: the window put home goes back, nothing switches.
fn cmd_tab_cancelled(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Spaces, load, |_| {});
    b.cmd_tab(vec![B, A]);
    b.advance(400);
    b.cmd_tab_escape(300);
    b.advance(8000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

/// `controller/switcher.rs`, `mission_key_tap.rs` (round r2-lends): ⌘Tab to Alpha, Escape
/// 400 ms after the Tab, ⌘ let go 300 ms later. `tap`: the key tap hears (else macOS refused
/// it, and only the missing activation tells the cancel: the silent waits).
fn cmd_tab_escape(load: f64, tap: bool) -> Run {
    let mut b = both_desktops_known(EngineKind::Spaces, load, |d| d.tap_hears = tap);
    b.cmd_tab(vec![B, A]);
    b.advance(400);
    let lent = b.desktop_of(11).is_some();
    b.cmd_tab_escape_key();
    let held = ms_until_back(&mut b, 11, 300);
    b.cmd_tab_command_up();
    // Not back while ⌘ was held: how long after Escape in all.
    let back = soon(held.or_else(|| ms_until_back(&mut b, 11, 2000).map(|ms| ms + 300)));
    b.advance(8000);
    let out = format!("put home {lent}, back {back}; {}", seen(&mut b));
    Run::of(&b, out)
}

/// ⌘Tab let go onto Alpha, which takes the activation 0.4 s later (`engine/switcher.rs`,
/// `ACTIVATION_WAIT`: a 3 s cap while the tap hears the release, `SILENT_ACTIVATION_WAIT`
/// 200 ms when it does not).
fn cmd_tab_onto_a_slow_app(load: f64, tap: bool) -> Run {
    let mut b = both_desktops_known(EngineKind::Spaces, load, |d| d.tap_hears = tap);
    b.desk(|d| d.apps.get_mut(&A).expect("Alpha").activates_ms = Some(400.0));
    b.cmd_tab(vec![B, A]);
    b.advance(400);
    b.cmd_tab_release();
    b.advance(30_000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

fn cmd_tab_onto_a_slow_app_heard(load: f64) -> Run {
    cmd_tab_onto_a_slow_app(load, true)
}

fn cmd_tab_onto_a_slow_app_tap_refused(load: f64) -> Run {
    cmd_tab_onto_a_slow_app(load, false)
}

fn cmd_tab_escape_heard(load: f64) -> Run {
    cmd_tab_escape(load, true)
}

fn cmd_tab_escape_tap_refused(load: f64) -> Run {
    cmd_tab_escape(load, false)
}

#[test]
fn cmd_tab_cancelled_with_and_without_the_key_tap() {
    // The tap hears Escape: the window put home goes back at its key-down.
    let heard = "put home true, back at once; front [12] shows [12] desktop 1 app 102 switches 0 slides 0";
    expect(cmd_tab_escape_heard, [heard, heard, heard]);
    // TIME DECIDES: GONE_WAIT (150 ms) then SILENT_ACTIVATION_WAIT (200 ms). With the tap
    // refused no key says the switcher was cancelled: its list gone with no key is taken for
    // the release, and the missing activation for the cancel, about 350 ms after Escape.
    let silent = "put home true, back within 1 s; front [12] shows [12] desktop 1 app 102 switches 0 slides 0";
    expect(cmd_tab_escape_tap_refused, [silent, silent, silent]);
    let here = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0";
    let slid = "front [13] shows [13] desktop 2 app 101 switches 0 slides 1";
    // TIME DECIDES: ACTIVATION_WAIT (a 3 s cap since r2-lends). The release heard, the window
    // waits for its app's activation; at load 10 that comes 4 s after the release, past the
    // cap, and macOS slides to Alpha's other Desktop.
    expect(cmd_tab_onto_a_slow_app_heard, [here, here, slid]);
    // TIME DECIDES: SILENT_ACTIVATION_WAIT (200 ms). With the tap refused the old rule
    // stands: an app 0.4 s slow has its window taken back before it activates, and macOS
    // slides to its other Desktop, on the Mac as measured already.
    expect(cmd_tab_onto_a_slow_app_tap_refused, [slid, slid, slid]);
}

/// Live 11-23 17:43 (the owner's Messages): ⌘Tab from Desktop 1 to Alpha, whose only window
/// waits in a hidden stage of Desktop 2 (in a stage Space, on no Desktop: macOS goes nowhere
/// for it). The activation asks the app its focused window, arranges that window's Desktop
/// and focuses it, and macOS goes (`arrange_for_hidden_focus`). `wakes_ms`: how long after
/// its activation the app answers nothing; asked within its first 50 ms it did not say, the
/// activation was held for nothing, no focus notice followed (its key window had not
/// changed), and the app was in front with no window: "clicking on Messages from the app
/// switcher wasn't working".
fn cmd_tab_to_an_app_hidden_on_another_desktop(load: f64, wakes_ms: Option<f64>) -> Run {
    let mut b = Bench::new(EngineKind::Spaces, DEFAULTS, 1.0, 2, |d| {
        d.add_app(A, "com.example.alpha", "Alpha");
        d.add_app(B, "com.example.bravo", "Bravo");
        d.add_window(13, A, frame(3), D2);
        d.add_window(15, B, frame(5), D2);
        d.add_window(12, B, frame(2), D1);
        d.front = Some(B);
    });
    b.go_to_desktop(D2, true);
    b.advance(4000);
    b.go_to_desktop(D1, true);
    b.advance(6000);
    let start = (b.front(), b.desktop_shown(), b.front_app(), b.desktop_of(13), b.desktop_of(15));
    assert_eq!(start, (Some(vec![12]), D1, Some(B), None, Some(D2)), "the start:\n{}", b.story());
    b.log.clear();
    crate::engine::attribution::trail::clear();
    b.desk(|d| {
        d.strip.transitions = 0;
        d.calls.clear();
        d.load = load;
        let alpha = d.apps.get_mut(&A).expect("Alpha");
        alpha.wakes_ms = wakes_ms;
        // Its key window the same before and after: it says nothing of it.
        alpha.quiet_key = true;
    });
    b.cmd_tab(vec![B, A]);
    b.advance(400);
    b.cmd_tab_release();
    b.advance(8000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

fn cmd_tab_to_an_app_hidden_elsewhere(load: f64) -> Run {
    cmd_tab_to_an_app_hidden_on_another_desktop(load, None)
}

fn cmd_tab_to_an_app_hidden_elsewhere_waking(load: f64) -> Run {
    cmd_tab_to_an_app_hidden_on_another_desktop(load, Some(70.0))
}

#[test]
fn cmd_tab_to_an_app_whose_only_window_is_hidden_on_another_desktop() {
    let there = "front [13] shows [13] desktop 2 app 101 switches 0 slides 1";
    expect(cmd_tab_to_an_app_hidden_elsewhere, [there, there, there]);
    expect(cmd_tab_to_an_app_hidden_elsewhere_waking, [there, there, there]);
}

#[test]
fn cmd_tab_to_an_app_whose_stage_is_hidden_here() {
    let here = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0";
    expect(cmd_tab_released, [here, here, here]);
    let stays = "front [12] shows [12] desktop 1 app 102 switches 0 slides 0";
    expect(cmd_tab_cancelled, [stays, stays, stays]);
}

// ----- 6, 7. Mission Control (Spaces) ---------------------------------------------------------

/// `controller/mission_control.rs` (round mc-key): Mission Control opened by its key and
/// closed by its key 1.5 s later with the pointer over Alpha's window, whose stage is hidden.
/// macOS activates Alpha as it closes. Apple's: that app's stage comes in front (the
/// default); with `[windows] mission_control_key_keeps_stage` the stage in front stays.
fn mission_control_closed_by_its_key(load: f64, config: &str) -> Run {
    let mut b = Bench::new(EngineKind::Spaces, config, load, 1, two_apps);
    b.mission_open();
    b.advance(1500);
    let out_while = b.on_a_desktop();
    b.mission_close_by_key(Some(A));
    b.advance(8000);
    let out = format!("in Mission Control {out_while:?}; after: {}", seen(&mut b));
    Run::of(&b, out)
}

fn mission_control_key_close(load: f64) -> Run {
    mission_control_closed_by_its_key(load, DEFAULTS)
}

fn mission_control_key_close_keeps_stage(load: f64) -> Run {
    mission_control_closed_by_its_key(load, "[windows]\nmission_control_key_keeps_stage = true\n")
}

/// `engine/mission_control.rs` (round mc-pick, 11-19): in Mission Control, a click on the
/// hidden stage's window: macOS activates its app with the release; its stage comes in front.
fn mission_control_pick(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Spaces, DEFAULTS, load, 1, two_apps);
    b.mission_open();
    b.advance(1500);
    b.mission_pick(11);
    b.advance(8000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

/// `engine/mission_control.rs`, the retries (live 11-22, the owner's Mac): with
/// `mission_control_stages = "key"` the hidden windows are parked and put home at the key,
/// before Mission Control is seen. The window server lists the parks' moves `lag_ms` late
/// (more than a park's read back waits): each window is looked at again until it is listed
/// parked, and is in Mission Control all the same.
fn mission_control_at_the_key_moves_listed_late(load: f64, lag_ms: f64) -> Run {
    let mut b = Bench::new(EngineKind::Spaces, "[windows]\nmission_control_stages = \"key\"\n", load, 1, two_apps);
    b.desk(|d| d.lists_moves_ms = Some(lag_ms));
    b.mission_open();
    b.advance(1500);
    let out_while = b.on_a_desktop();
    b.mission_close_by_key(None);
    b.advance(8000);
    let out = format!("in Mission Control {out_while:?}; after: {}", seen(&mut b));
    Run::of(&b, out)
}

fn mission_control_at_the_key_moves_listed_40_ms_late(load: f64) -> Run {
    mission_control_at_the_key_moves_listed_late(load, 40.0)
}

#[test]
fn mission_control_at_the_key_with_late_listing() {
    let all = "in Mission Control [11, 12]; after: front [12] shows [12] desktop 1 app 102 switches 0 slides 0";
    expect(mission_control_at_the_key_moves_listed_40_ms_late, [all, all, all]);
}

#[test]
fn mission_control() {
    // While it shows, the hidden stage's window is on its Desktop too (lent).
    let apples = "in Mission Control [11, 12]; after: front [11] shows [11] desktop 1 app 101 switches 1 slides 0";
    expect(mission_control_key_close, [apples, apples, apples]);
    let keeps = "in Mission Control [11, 12]; after: front [12] shows [12] desktop 1 app 102 switches 0 slides 0";
    expect(mission_control_key_close_keeps_stage, [keeps, keeps, keeps]);
    let picked = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0";
    expect(mission_control_pick, [picked, picked, picked]);
}

// ----- 8. a wallpaper click and Finder's echo --------------------------------------------------

/// `engine/desktop_echo.rs` (11-09 16:33:26: Finder activated 1025 ms after the press):
/// a click on the wallpaper; macOS hands the desktop to Finder, whose activation follows.
/// Finder has a window in a hidden stage: the echo must not bring it in front.
fn wallpaper_click(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, |d| {
        d.add_window(16, fake::FINDER_PID, frame(6), D1);
        two_apps(d);
    });
    b.click(Point::new(1400.0, 300.0));
    b.advance(15_000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

#[test]
fn a_wallpaper_click_and_finders_echo() {
    let ignored = "front [12] shows [12] desktop 1 app 91 switches 0 slides 0";
    // By order since round r4-causes: Finder's first activation and first focus after the
    // press are its echo however late (`WALLPAPER_ECHO` is a 15 s cap). Before, TIME
    // DECIDED: 1 s after the release; three times slower the activation came 3 s after the
    // press, was taken for a deliberate one, and Finder's stage came in front.
    expect(wallpaper_click, [ignored, ignored, ignored]);
}

// ----- 9. Space changes ----------------------------------------------------------------------

/// ⌃→ to Desktop 2 and ⌃← back, both Desktops known: each arrives with its own stage in
/// front (`engine/slide_hold.rs`: the arriving Desktop's app is activated before the notice
/// and must not switch stages on the Desktop being left).
fn desktop_keys_between_known_desktops(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Spaces, load, |_| {});
    b.go_to_desktop(D2, true);
    b.advance(4000);
    let there = seen(&mut b);
    b.go_to_desktop(D1, true);
    b.advance(6000);
    let back = seen(&mut b);
    Run::of(&b, format!("there: {there}; back: {back}"))
}

/// `controller/arrival.rs` (measured 10-13, an `open` activation): macOS activates Charlie,
/// whose only window is on Desktop 2, and slides there; under the Corner engine. Its stage
/// is in front when the Desktop arrives.
fn activation_that_slides(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Corner, load, |d| {
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_window(14, C, frame(4), D2);
        d.raise(13);
        d.raise(15);
        d.raise(11);
        d.raise(12);
    });
    b.activate(C);
    b.advance(8000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

/// The same by a Dock click (the Desktop arranged ahead at the click, `DOCK_SWITCH_WAIT`).
fn dock_click_that_slides(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Corner, load, |d| {
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_window(14, C, frame(4), D2);
        d.raise(13);
        d.raise(15);
        d.raise(11);
        d.raise(12);
    });
    b.dock_click(C, 90);
    b.advance(8000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

/// Live 11-21, `spaces --auto`, "Dock arrival N of 3" (round r2-lends-fix): a Dock click on
/// Alpha, whose only window waits in a hidden stage of Desktop 1, from Desktop 2; Desktop 1
/// is arranged at the press (Alpha's stage in front there, Bravo's, the stage left, hidden).
/// The activation's notice comes at the slide's start, before Desktop 1's windows are listed
/// ([`fake::Desk::activation_leads_slide`]). Taken at the activation for a click that went
/// nowhere, the arrangement was undone (Bravo's window put back on Desktop 1) and made again
/// for the activation's hidden focus a moment later, and the window server listed the stage
/// left on screen as Desktop 1 slid in (+1015 and +1041 ms). It must be hidden once, never
/// put back.
fn dock_click_whose_activation_leads_the_slide(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Spaces, DEFAULTS, 1.0, 2, |d| {
        d.add_app(A, "com.example.alpha", "Alpha");
        d.add_app(B, "com.example.bravo", "Bravo");
        d.add_window(15, B, frame(5), D2);
        d.add_window(11, A, frame(1), D1);
        d.add_window(12, B, frame(2), D1);
        d.front = Some(B);
        d.activation_leads_slide = true;
    });
    b.go_to_desktop(D2, true);
    b.advance(4000);
    let start = (b.desktop_shown(), b.desktop_of(11), b.desktop_of(12));
    assert_eq!(start, (D2, None, Some(D1)), "the start:\n{}", b.story());
    b.log.clear();
    b.desk(|d| {
        d.calls.clear();
        d.load = load;
    });
    b.dock_click(A, 90);
    b.advance(8000);
    let back = b.calls().iter().filter(|c| matches!(c, fake::Call::IntoSpace(s, ids) if *s == D1 && ids.contains(&12))).count();
    let out = format!("the stage left put back on Desktop 1 {back} times; {}", seen(&mut b));
    Run::of(&b, out)
}

/// Live 11-23 09:11 (the owner's Terminal): a Dock click on the app in front on Desktop 2,
/// whose other window waits in a hidden stage of Desktop 1, with a tab not selected of the
/// window here waiting with the windows not placed (`tab`: off screen, listed on no Space).
/// The click's prediction was refused for the tab ("the active app has windows not placed"),
/// Desktop 1 was not arranged at the press, macOS counted no window of the app there, and
/// ten clicks did nothing. The tab is on no Desktop: Desktop 1 is arranged at the press and
/// macOS goes there.
fn dock_click_on_the_app_in_front(load: f64, tab: bool) -> Run {
    let mut b = Bench::new(EngineKind::Spaces, DEFAULTS, 1.0, 2, |d| {
        d.add_app(A, "com.example.alpha", "Alpha");
        d.add_app(B, "com.example.bravo", "Bravo");
        d.add_window(16, A, frame(5), D2);
        d.add_window(15, A, frame(5), D2);
        d.add_window(11, A, frame(1), D1);
        d.add_window(12, B, frame(2), D1);
        d.front = Some(B);
        if tab {
            // Found with its app's windows, never on screen: it waits.
            d.windows.get_mut(&16).expect("the tab").ordered_in = false;
        } else {
            d.windows.remove(&16);
            d.z.retain(|id| *id != 16);
        }
    });
    b.go_to_desktop(D2, true);
    b.advance(4000);
    b.activate(A);
    b.advance(4000);
    if tab {
        // A tab not selected is listed on no Space.
        b.desk(|d| d.windows.get_mut(&16).expect("the tab").spaces.clear());
        b.advance(1500);
    }
    let waiting = b.c.unplaced.contains(&16);
    let start = (b.desktop_shown(), b.front_app(), b.desktop_of(11), b.desktop_of(12), waiting);
    assert_eq!(start, (D2, Some(A), None, Some(D1), tab), "the start:\n{}", b.story());
    b.log.clear();
    b.desk(|d| {
        d.calls.clear();
        d.load = load;
    });
    b.dock_click(A, 90);
    b.advance(8000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

fn dock_click_on_the_app_in_front_with_a_tab_waiting(load: f64) -> Run {
    dock_click_on_the_app_in_front(load, true)
}

fn dock_click_on_the_app_in_front_no_tab(load: f64) -> Run {
    dock_click_on_the_app_in_front(load, false)
}

#[test]
fn a_dock_click_on_the_app_in_front_goes_to_its_other_desktop() {
    let arrived = "front [11] shows [11] desktop 1 app 101 switches 0 slides 1";
    expect(dock_click_on_the_app_in_front_no_tab, [arrived, arrived, arrived]);
    expect(dock_click_on_the_app_in_front_with_a_tab_waiting, [arrived, arrived, arrived]);
}

#[test]
fn a_dock_click_whose_activation_leads_the_slide() {
    let want = "the stage left put back on Desktop 1 0 times; front [11] shows [11] desktop 1 app 101 switches 0 slides 1";
    expect(dock_click_whose_activation_leads_the_slide, [want, want, want]);
}

#[test]
fn space_changes() {
    let keys = "there: front [13] shows [13] desktop 2 app 101 switches 0 slides 1; back: front [12] shows [12] desktop 1 app 102 switches 0 slides 2";
    expect(desktop_keys_between_known_desktops, [keys, keys, keys]);
    let arrived = "front [14] shows [14] desktop 2 app 103 switches 0 slides 1";
    expect(activation_that_slides, [arrived, arrived, arrived]);
    expect(dock_click_that_slides, [arrived, arrived, arrived]);
}

// ----- 12. a new app --------------------------------------------------------------------------

/// `controller.rs`, "Shift sugar" (`[sugar] shift_brings_here`): Charlie opened from the
/// Dock, its first window 400 ms after the launch; with Shift held at the click it joins the
/// stage in front, without it gets a stage of its own.
fn new_app(load: f64, shift: bool) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, two_apps);
    let item = Rect::new(900.0, 838.0, 50.0, 50.0);
    b.desk(|d| {
        d.shift = shift;
        d.dock.items.push(("/Applications/Charlie.app/".into(), item));
    });
    b.click(Point::new(925.0, 860.0));
    b.desk(|d| d.launch(C, "com.example.charlie", "Charlie", 400.0, &[(14, frame(4), None)]));
    b.advance(300);
    b.desk(|d| d.shift = false);
    b.advance(15_000);
    let out = format!("stages {:?}; {}", b.stages(), seen(&mut b));
    Run::of(&b, out)
}

fn new_app_with_shift(load: f64) -> Run {
    new_app(load, true)
}

fn new_app_plain(load: f64) -> Run {
    new_app(load, false)
}

#[test]
fn a_new_app() {
    let joined = "stages [[11], [12, 14]]; front [12, 14] shows [12, 14] desktop 1 app 103 switches 0 slides 0";
    expect(new_app_with_shift, [joined, joined, joined]);
    let own = "stages [[11], [12], [14]]; front [14] shows [14] desktop 1 app 103 switches 1 slides 0";
    expect(new_app_plain, [own, own, own]);
}

// ----- 13. the restore after a restart --------------------------------------------------------

/// `engine/restore.rs` (the restore window: `SETTLE_QUIET`, `QUIET`, `WINDOW`): Alpha's and
/// Bravo's windows share a stage, Charlie's has its own; after a restart the apps reopen
/// their windows over 30 s. Each goes back to its saved stage.
pub(super) fn restore_after_a_restart(load: f64) -> Run {
    let state = {
        let mut b = Bench::new(EngineKind::Corner, DEFAULTS, 1.0, 1, |d| {
            two_apps(d);
            d.add_app(C, "com.example.charlie", "Charlie");
            d.add_window(14, C, frame(4), D1);
        });
        let (alpha, bravo) = (b.stage_of(11), b.stage_of(12));
        b.desk(|d| d.post(0.0, Notice::Strip(StripEvent::CardMerged { stage: alpha, into: bravo })));
        b.advance(4000);
        b.desk(|d| d.open_window(17, B, frame(7)));
        b.advance(4000);
        assert_eq!(b.stages(), vec![vec![11, 12, 17], vec![14]], "before the restart:\n{}", b.story());
        b.saved()
    };
    let mut b = Bench::with_state(EngineKind::Corner, DEFAULTS, load, 1, Some(&state), |d| {
        d.session = "after-the-restart";
        d.login_started = Some(clock::now());
    });
    b.advance(1000);
    b.desk(|d| d.launch(A, "com.example.alpha", "Alpha", 2000.0, &[(21, frame(1), Some("Doc 11"))]));
    b.advance(8000);
    b.desk(|d| d.launch(C, "com.example.charlie", "Charlie", 3000.0, &[(24, frame(4), Some("Doc 14"))]));
    b.advance(12_000);
    // Bravo's second window comes 7 s after its first (70 s at load 10).
    b.desk(|d| {
        d.launch(B, "com.example.bravo", "Bravo", 5000.0, &[(22, frame(2), Some("Doc 12"))]);
        d.post(12_000.0, Notice::Step(fake::Step::Opens { id: 27, pid: B, frame: frame(7), title: Some("Doc 17") }));
    });
    b.advance(600_000);
    let out = format!("stages {:?} front {:?}", b.stages(), b.front());
    Run::of(&b, out)
}

#[test]
fn the_restore_after_a_restart() {
    let restored = "stages [[21, 22, 27], [24]] front Some([21, 22, 27])";
    expect(restore_after_a_restart, [restored, restored, restored]);
}

/// `engine/slide_hold.rs` (live 11-15 16:34:28, fixed there for Desktops a saved file names):
/// the first ⌃→ ever to a Desktop the app has not seen, whose front app (Alpha) has a window
/// in a hidden stage on the Desktop being left.
fn first_visit_to_a_desktop(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Spaces, DEFAULTS, load, 2, alpha_on_two_desktops);
    b.go_to_desktop(D2, true);
    b.advance(6000);
    let out = seen(&mut b);
    Run::of(&b, out)
}

/// `spaces/engine.rs`, `spill::home_for` (a log, 11-21: the external screen back, Dock
/// clicks 36 s and 61 s after the change hid a Desktop's front window into a stage Space of
/// the Desktop still shown; the live check `display-gone ahead`): Alpha and Bravo on
/// Desktop 2 only, a stage each, Bravo's in front; on Desktop 1 a screen comes and goes;
/// 36 s later Alpha's Dock item is clicked. macOS slides to Desktop 2; Alpha's stage is in
/// front there and Bravo's window hidden on its own Desktop.
fn dock_click_after_a_screen_came_and_went(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Spaces, DEFAULTS, 1.0, 2, |d| {
        d.add_app(A, "com.example.alpha", "Alpha");
        d.add_app(B, "com.example.bravo", "Bravo");
        d.add_window(13, A, frame(3), D2);
        d.add_window(15, B, frame(5), D2);
        d.front = Some(B);
        d.starts_on(D2);
    });
    b.go_to_desktop(D1, true);
    b.advance(5000);
    b.log.clear();
    b.desk(|d| {
        d.strip.transitions = 0;
        d.calls.clear();
        d.load = load;
        d.screen_comes();
    });
    b.advance(4000);
    b.desk(|d| d.screen_goes());
    b.advance(36_000);
    b.dock_click(A, 90);
    b.advance(10_000);
    let bravo_home = crate::platform::window_engine::WindowEngine::home_of(&*crate::platform::spaces::engine::shared(), WindowId(15));
    let out = format!("{}; Bravo's window hidden from Desktop {bravo_home:?}, listed on {:?}", seen(&mut b), b.desktop_of(15));
    Run::of(&b, out)
}

#[test]
fn a_dock_click_after_a_screen_came_and_went() {
    let there = "front [13] shows [13] desktop 2 app 101 switches 0 slides 1; Bravo's window hidden from Desktop Some(2), listed on None";
    expect(dock_click_after_a_screen_came_and_went, [there, there, there]);
}

#[test]
fn the_first_visit_to_a_desktop() {
    // The person pressed ⌃→ and is on Desktop 2, one slide, Alpha's window there in front.
    // The Desktop's windows are unknown before its first visit, but the window server says
    // which Desktop Alpha's focused window is on: its activation is the slide's, held for the
    // arrival (`controller/desktops.rs`, `focus_on_another_desk`). Before: not seen as a
    // slide, the activation switched stages on the Desktop being left and macOS slid back
    // (front [11] on Desktop 1, two slides, at every load).
    let there = "front [13] shows [13] desktop 2 app 101 switches 0 slides 1";
    expect(first_visit_to_a_desktop, [there, there, there]);
}

// ----- the window server's notices (Spaces) --------------------------------------------------

/// `controller/server_notices.rs` (round r5-notices; `probe-spaces cgs` 11-21: a window put
/// into a Space and back, its notices 1326 and 1325 3 ms after): Alpha's window hidden in its
/// stage's Space, the app at rest; then something other than the app (macOS, its app ordering
/// it out and in) puts it onto the Desktop shown. With the window server's notices it goes
/// back at the notice's look; without them, at the tick's (at rest, up to `STRAY_LOOK`).
fn window_taken_out_of_its_stage_space(load: f64, notices: bool) -> Run {
    let mut b = Bench::new(EngineKind::Spaces, DEFAULTS, load, 1, |d| {
        two_apps(d);
        d.server_notices = notices;
    });
    b.advance(6000);
    let hidden = b.desktop_of(11).is_none();
    b.desk(|d| d.space_moves(11, D1));
    let back = taken_back(&mut b, 11, 5000);
    let out = format!("hidden at the start {hidden}; hidden again {back}; {}", seen(&mut b));
    Run::of(&b, out)
}

fn window_taken_out_noticed(load: f64) -> Run {
    window_taken_out_of_its_stage_space(load, true)
}

fn window_taken_out_unnoticed(load: f64) -> Run {
    window_taken_out_of_its_stage_space(load, false)
}

/// `engine/overview.rs`, `controller/server_notices.rs` (round r5-notices; review 11-21 R4:
/// Mission Control opened by a gesture, a hot corner or the Dock's item gave no hint and was
/// found at the tick, up to 2.2 s late): Mission Control opens with no key heard. With the
/// window server's notice (1508, 40 ms) it is found at its look and the hidden stage's window
/// is lent at once; without, at the tick at rest.
fn mission_control_without_its_key(load: f64, notices: bool) -> Run {
    let mut b = Bench::new(EngineKind::Spaces, DEFAULTS, load, 1, |d| {
        two_apps(d);
        d.server_notices = notices;
    });
    b.advance(6000);
    b.desk(|d| d.mission_opens());
    b.advance(300);
    let soon = b.on_a_desktop();
    b.advance(2700);
    let later = b.on_a_desktop();
    let out = format!("on the Desktop 0.3 s after it opened {soon:?}, 3 s after {later:?}");
    Run::of(&b, out)
}

fn mission_control_by_gesture_noticed(load: f64) -> Run {
    mission_control_without_its_key(load, true)
}

fn mission_control_by_gesture_unnoticed(load: f64) -> Run {
    mission_control_without_its_key(load, false)
}

#[test]
fn the_window_servers_notices() {
    let at_once = "hidden at the start true; hidden again at once; front [12] shows [12] desktop 1 app 102 switches 0 slides 0";
    expect(window_taken_out_noticed, [at_once, at_once, at_once]);
    let lent = "on the Desktop 0.3 s after it opened [11, 12], 3 s after [11, 12]";
    expect(mission_control_by_gesture_noticed, [lent, lent, lent]);
    // Without them, as before: the tick's look.
    let ticked = "hidden at the start true; hidden again within 1 s; front [12] shows [12] desktop 1 app 102 switches 0 slides 0";
    expect(window_taken_out_unnoticed, [ticked, ticked, ticked]);
    let late = "on the Desktop 0.3 s after it opened [12], 3 s after [11, 12]";
    expect(mission_control_by_gesture_unnoticed, [late, late, late]);
}

/// Every scenario, for the load table.
pub(super) const ALL: &[Scenario] = &[
    ("card clicks while the WM thread is held up", "engine/stale_activation.rs, 11-03", card_clicks_under_a_busy_wm_thread),
    ("our activation reported 1.2 s late", "engine/causes.rs is_app_echo, 11-14", our_activation_reported_late),
    ("Dock click, stage hidden here (Spaces)", "controller/arrival.rs, dock-put-home", dock_click_on_a_stage_hidden_here),
    ("Dock click, activation 0.4 s late (Spaces)", "review 11-21 R2, LEND_WAIT", dock_click_with_a_slow_activation),
    ("Dock click, activation 1.2 s late (Spaces)", "review 11-21 R2, LEND_WAIT", dock_click_with_a_late_activation),
    ("Dock click, then another app's activation (Spaces)", "engine/lend_wait.rs, r2-lends", dock_click_then_another_apps_activation),
    ("Dock click, its activation never comes (Spaces)", "engine/lend_wait.rs, LEND_WAIT cap", dock_click_whose_activation_never_comes),
    ("Cmd-Tab released (Spaces)", "controller/switcher.rs, 11-15", cmd_tab_released),
    ("Cmd-Tab cancelled with Escape (Spaces)", "controller/switcher.rs, 11-15", cmd_tab_cancelled),
    ("Cmd-Tab, Escape heard by the key tap (Spaces)", "mission_key_tap.rs, r2-lends", cmd_tab_escape_heard),
    ("Cmd-Tab, Escape with the key tap refused (Spaces)", "engine/switcher.rs SILENT_* waits", cmd_tab_escape_tap_refused),
    ("Cmd-Tab let go onto an app 0.4 s slow, the tap hears (Spaces)", "engine/switcher.rs ACTIVATION_WAIT", cmd_tab_onto_a_slow_app_heard),
    ("Cmd-Tab let go onto an app 0.4 s slow, the tap refused (Spaces)", "engine/switcher.rs SILENT_ACTIVATION_WAIT", cmd_tab_onto_a_slow_app_tap_refused),
    ("Cmd-Tab to an app whose only window is hidden on another Desktop (Spaces)", "controller/arrival.rs, arrange_for_hidden_focus", cmd_tab_to_an_app_hidden_elsewhere),
    ("the same, the app answering nothing for 70 ms after its activation", "engine/slide_hold.rs, AskAgain, live 11-23", cmd_tab_to_an_app_hidden_elsewhere_waking),
    ("Mission Control closed by its key over another app", "controller/mission_control.rs, mc-key", mission_control_key_close),
    ("the same with mission_control_key_keeps_stage", "controller/mission_control.rs, mc-key", mission_control_key_close_keeps_stage),
    ("Mission Control: a hidden stage's window picked", "engine/mission_control.rs, mc-pick 11-19", mission_control_pick),
    ("Mission Control at the key, the parks' moves listed 40 ms late", "engine/mission_control.rs, the retries, live 11-22", mission_control_at_the_key_moves_listed_40_ms_late),
    ("a wallpaper click and Finder's echo", "engine/desktop_echo.rs, 11-09", wallpaper_click),
    ("Desktop keys between two known Desktops (Spaces)", "engine/slide_hold.rs, 10-23", desktop_keys_between_known_desktops),
    ("the first visit to a Desktop by its key (Spaces)", "engine/slide_hold.rs, 11-15", first_visit_to_a_desktop),
    ("an activation that slides to another Desktop (Corner)", "controller/arrival.rs, 10-13", activation_that_slides),
    ("a Dock click that slides to another Desktop (Corner)", "controller/arrival.rs, DOCK_SWITCH_WAIT", dock_click_that_slides),
    ("a Dock click whose activation leads the slide (Spaces)", "live 11-21 Dock arrivals, r2-lends-fix", dock_click_whose_activation_leads_the_slide),
    ("a Dock click on the app in front, its other window in a hidden stage of another Desktop (Spaces)", "controller/arrival.rs, arrange_at_press", dock_click_on_the_app_in_front_no_tab),
    ("the same, a tab not selected of it waiting", "controller/arrival.rs, waits_elsewhere, live 11-23", dock_click_on_the_app_in_front_with_a_tab_waiting),
    ("a screen comes and goes, a Dock click 36 s later (Spaces)", "engine/spill.rs home_for, 11-21", dock_click_after_a_screen_came_and_went),
    ("a new app opened with Shift held", "controller.rs, Shift sugar", new_app_with_shift),
    ("a new app opened", "controller.rs, new-window intake", new_app_plain),
    ("the restore after a restart", "engine/restore.rs", restore_after_a_restart),
    ("a hidden window taken out of its stage Space, notices on (Spaces)", "controller/server_notices.rs, r5-notices", window_taken_out_noticed),
    ("a hidden window taken out of its stage Space, no notices (Spaces)", "controller/spaces.rs STRAY_LOOK", window_taken_out_unnoticed),
    ("Mission Control with no key, notices on (Spaces)", "engine/overview.rs, r5-notices", mission_control_by_gesture_noticed),
    ("Mission Control with no key, no notices (Spaces)", "review 11-21 R4, OVERVIEW_LOOK", mission_control_by_gesture_unnoticed),
];

/// The load table: every scenario's outcome at load 1, 3 and 10, and the constants whose
/// limits were crossed only under load (`cargo test --lib load_table -- --nocapture`).
#[test]
fn load_table() {
    println!("{}", table());
}

/// One scenario at one load, its story printed: `BENCH_ONE="Dock click" BENCH_LOAD=10
/// BENCH_LOG=debug cargo test --lib bench_one -- --nocapture` (the app's own log lines
/// beside it with `BENCH_LOG`). Nothing without `BENCH_ONE`.
#[test]
fn bench_one() {
    let Ok(want) = std::env::var("BENCH_ONE") else { return };
    let load: f64 = std::env::var("BENCH_LOAD").ok().and_then(|l| l.parse().ok()).unwrap_or(1.0);
    for (name, told, script) in ALL.iter().chain(super::causes::ALL).chain(super::switch::ALL).chain(super::desktops::ALL).chain(super::screens::ALL).filter(|(name, _, _)| name.contains(&want)) {
        let run = script(load);
        println!("== {name} ({told}) at load {load}\n{}\n=> {}", run.story, run.outcome);
        for m in &run.margins {
            println!("   {m:?}");
        }
    }
}

/// The verdicts of each constant in a run, as text: the decisions inside and outside, the
/// waits met and capped.
fn verdicts(run: &Run) -> std::collections::BTreeMap<&'static str, (usize, usize, usize, usize)> {
    let mut out = std::collections::BTreeMap::new();
    for s in &run.margins {
        let e: &mut (usize, usize, usize, usize) = out.entry(s.name).or_default();
        match (s.inside, s.ended) {
            (Some(true), _) => e.0 += 1,
            (Some(false), _) => e.1 += 1,
            (_, Some(margin::End::Cap)) => e.3 += 1,
            (_, Some(_)) => e.2 += 1,
            _ => {}
        }
    }
    out
}

/// The load table as Markdown.
pub(super) fn table() -> String {
    let mut out = String::from("| Scenario | Told by | Load 1 | Load 3 | Load 10 | Limits crossed only under load |\n|---|---|---|---|---|---|\n");
    for (name, told, script) in ALL.iter().chain(super::causes::ALL).chain(super::switch::ALL).chain(super::desktops::ALL).chain(super::screens::ALL) {
        let runs = LOADS.map(script);
        let base = verdicts(&runs[0]);
        let mut changed: Vec<String> = Vec::new();
        for (i, run) in runs.iter().enumerate().skip(1) {
            let v = verdicts(run);
            let names: BTreeSet<&str> = base.keys().chain(v.keys()).copied().collect();
            for n in names {
                let (a, b) = (base.get(n).copied().unwrap_or_default(), v.get(n).copied().unwrap_or_default());
                // A limit crossed at this load and not at load 1: judged outside, or a wait
                // ended by its cap.
                if (b.1 > 0 && a.1 == 0) || (b.3 > 0 && a.3 == 0) {
                    let word = format!("{n} (x{})", LOADS[i]);
                    if !changed.iter().any(|c| c.starts_with(n)) {
                        changed.push(word);
                    }
                }
            }
        }
        assert_eq!(runs.iter().map(|r| r.disagreed).sum::<u64>(), 0, "{name}: the model and the window server's listing disagreed on the Desktop shown");
        let same = |i: usize| if runs[i].outcome == runs[0].outcome { "same".to_string() } else { format!("**{}**", runs[i].outcome) };
        out.push_str(&format!("| {name} | {told} | {} | {} | {} | {} |\n", runs[0].outcome, same(1), same(2), if changed.is_empty() { "none".into() } else { changed.join(", ") }));
    }
    out
}
