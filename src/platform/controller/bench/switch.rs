//! The switch's handshake with the strip and the raise-order check after it (round
//! r6-waits): a card click with the strip answering in time, late or never; a second click,
//! a person's click or an app's own activation while the switch waits for the strip's
//! pictures or checks the order of its raises; an app that reorders its windows late;
//! a switch's pictures taken at the switch (a Dock click, ⌘Tab, a card click whose press took
//! them).
//!
//! Each outcome reads what a person sees ([`seen`]), how many switches played with their
//! pictures and how many moved the windows without them, and the order of the calls made
//! of the platform from the click on ([`calls`]); the raise-order scenarios add the stage's
//! windows as the window server stacks them. Where a load changes the outcome, the assertion
//! is marked `// TIME DECIDES:`.

use std::time::Duration;

use super::scenarios::{A, B, C, D1, Run, Scenario, expect, seen, two_apps};
use super::*;
use crate::platform::fake::{Call, Desk};

/// The calls made of the platform so far, in short words, in order: `shots` (full-size
/// pictures of windows taken at the switch and waited for), `strip` (a transition presented),
/// `park` and `show` (a frame set into or out of the park zone), `space` (windows put into a
/// Space), `raise`, `main`, `front` (an app made frontmost).
fn calls(b: &mut Bench) -> String {
    let parks = b.desk(|d| d.screens[0].display);
    let words: Vec<String> = b
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            Call::Present(Some(_)) => Some("strip".to_string()),
            Call::Present(None) => None,
            Call::Shots(ids) => Some(format!("shots {ids:?}")),
            Call::Frame(id, r) if parks.parks(r) => Some(format!("park {id}")),
            Call::Frame(id, _) => Some(format!("show {id}")),
            Call::IntoSpace(_, ids) => Some(format!("space {ids:?}")),
            Call::Raise(id) => Some(format!("raise {id}")),
            Call::Main(id) => Some(format!("main {id}")),
            Call::Frontmost(pid) => Some(format!("front {pid}")),
            Call::Minimized(id, on) => Some(format!("minimized {id} {on}")),
        })
        .collect();
    words.join(", ")
}

/// What a person sees, the pictures, and the calls in order.
fn played(b: &mut Bench) -> String {
    let (covered, uncovered) = b.desk(|d| (d.strip.covered, d.strip.uncovered));
    format!("{}; pictures {covered}, at once {uncovered}; {}", seen(b), calls(b))
}

/// [`played`] with the windows `ids` as the window server stacks them, front to back.
fn played_in_order(b: &mut Bench, ids: &[u32]) -> String {
    let z: Vec<u32> = b.desk(|d| {
        d.list(false);
        d.z.iter().copied().filter(|id| ids.contains(id)).collect()
    });
    format!("{}; stacked {z:?}", played(b))
}

/// The calls and counts forgotten, the load set: the incident starts here (the history ran on
/// the Mac as measured).
fn start(b: &mut Bench, load: f64) {
    b.log.clear();
    crate::engine::attribution::trail::clear();
    b.desk(|d| {
        d.strip.transitions = 0;
        d.strip.covered = 0;
        d.strip.uncovered = 0;
        d.calls.clear();
        d.load = load;
    });
}

/// Alpha, Bravo and Charlie with a window each on one Desktop, each in a stage of its own,
/// Bravo's in front.
fn three_apps(d: &mut Desk) {
    two_apps(d);
    d.add_app(C, "com.example.charlie", "Charlie");
    d.add_window(13, C, frame(3), D1);
    d.front = Some(B);
}

/// [`three_apps`] under `engine`, `edit` applied to the desk, the launch's moves over.
fn bench(engine: EngineKind, load: f64, edit: impl FnOnce(&mut Desk)) -> Bench {
    let mut b = Bench::new(engine, DEFAULTS, 1.0, 1, three_apps);
    b.advance(3000);
    b.desk(edit);
    start(&mut b, load);
    b
}

/// Alpha's and Charlie's windows in one stage (merged on the strip), Bravo's in front: a
/// click on the merged card raises two apps' windows, and the order check follows.
fn merged(engine: EngineKind, load: f64, edit: impl FnOnce(&mut Desk)) -> Bench {
    let mut b = Bench::new(engine, DEFAULTS, 1.0, 1, three_apps);
    let (alpha, charlie) = (b.stage_of(11), b.stage_of(13));
    b.desk(|d| d.post(0.0, Notice::Strip(StripEvent::CardMerged { stage: charlie, into: alpha })));
    b.advance(3000);
    let bravo = b.stage_of(12);
    b.card_click(bravo);
    b.advance(3000);
    assert_eq!((b.front(), b.stages()), (Some(vec![12]), vec![vec![11, 13], vec![12]]), "the start:\n{}", b.story());
    b.desk(edit);
    start(&mut b, load);
    b
}

/// When the script's next notice is due: `ms` from now (no load factor).
fn in_ms(ms: u64) -> std::time::Instant {
    clock::now() + Duration::from_millis(ms)
}

// ----- the strip's answer ----------------------------------------------------------------------

/// A card click for Alpha's stage, the strip answering as `edit` makes it, under `engine`.
fn card_click_with(engine: EngineKind, load: f64, edit: impl FnOnce(&mut Desk)) -> Run {
    let mut b = bench(engine, load, edit);
    let alpha = b.stage_of(11);
    b.card_click(alpha);
    b.advance(3000);
    let out = played(&mut b);
    Run::of(&b, out)
}

/// `controller.rs` `send_transition` (the strip's answer within `COVER_TIMEOUT`): the switch
/// plays with its pictures.
fn strip_in_time(load: f64) -> Run {
    card_click_with(EngineKind::Corner, load, |_| {})
}

/// The same under the Spaces engine (pictures taken at the switch, the hides into a Space).
fn strip_in_time_spaces(load: f64) -> Run {
    card_click_with(EngineKind::Spaces, load, |_| {})
}

/// `strip_model::wait_cover`: the strip answers 200 ms after the transition (a busy main
/// thread), past `COVER_TIMEOUT`.
fn strip_late(load: f64) -> Run {
    card_click_with(EngineKind::Corner, load, |d| d.strip.covers_ms = 200.0)
}

/// `strip_model::wait_cover`: the strip never answers.
fn strip_never(load: f64) -> Run {
    card_click_with(EngineKind::Corner, load, |d| d.strip.draws = false)
}

// ----- the pictures at the switch ------------------------------------------------------------------

/// `controller/switch_pictures.rs` (round switch-pictures): a Dock click on Alpha, its window
/// waiting in a hidden stage, the park corner's cover drawable (the activation grows from its
/// card, `[animation] activation_grows`). Nothing announced the switch: no picture of
/// Bravo's window, the one leaving, is fresh, so it is taken after the early focus and before
/// the strip is told, no window moved yet; the same calls under either engine but for the
/// hide (`park`, `space`).
fn dock_click_pictures(engine: EngineKind, load: f64) -> Run {
    let mut b = bench(engine, load, |d| d.corner_covers = true);
    b.dock_click(A, 90);
    b.advance(3000);
    let out = played(&mut b);
    Run::of(&b, out)
}

fn dock_click_pictures_corner(load: f64) -> Run {
    dock_click_pictures(EngineKind::Corner, load)
}

/// The same Dock click with window captures off or not allowed (no Screen Recording, the
/// way the app runs until it is granted): no picture can be taken, so none is waited for and
/// the strip is told before the early focus, as before round switch-pictures.
fn dock_click_captures_off(load: f64) -> Run {
    let mut b = bench(EngineKind::Corner, load, |d| {
        d.corner_covers = true;
        d.captures_off = true;
    });
    b.dock_click(A, 90);
    b.advance(3000);
    let out = played(&mut b);
    Run::of(&b, out)
}

fn dock_click_pictures_spaces(load: f64) -> Run {
    dock_click_pictures(EngineKind::Spaces, load)
}

/// ⌘Tab to Alpha, ⌘ let go `held_ms` after Tab. The switcher's key has the strip take every
/// shown window's picture (`main.rs` `monitor_input`, `strip::prefetch`: the bench runs no
/// strip, so the scenario says it): let go within `switch_pictures`' `FRESH` the switch takes
/// none and the strip is told first; past it the picture is old and taken again.
fn cmd_tab_pictures(engine: EngineKind, load: f64, held_ms: u64) -> Run {
    let mut b = bench(engine, load, |d| d.corner_covers = true);
    b.cmd_tab(vec![B, A, C]);
    b.desk(|d| d.pictured(&[12]));
    b.advance(held_ms);
    b.cmd_tab_release();
    b.advance(3000);
    let out = played(&mut b);
    Run::of(&b, out)
}

fn cmd_tab_quick(load: f64) -> Run {
    cmd_tab_pictures(EngineKind::Corner, load, 150)
}

/// The same under the Spaces engine: until round switch-pictures its early focus went before
/// the strip was told also with no picture to take (`main 11, front 101, strip`).
fn cmd_tab_quick_spaces(load: f64) -> Run {
    cmd_tab_pictures(EngineKind::Spaces, load, 150)
}

fn cmd_tab_held(load: f64) -> Run {
    cmd_tab_pictures(EngineKind::Corner, load, 700)
}

/// A card click for Alpha's stage whose press had the strip take the leaving window's
/// picture (`ui/strip.rs` `mouse_down`, `prefetch`; said by the scenario, as above): still
/// fresh at the click 80 ms later, none is taken. (The other card clicks here are ones whose
/// press took none: [`Bench::card_click`] says nothing of pictures.)
fn card_click_pictures_fresh(load: f64) -> Run {
    let mut b = bench(EngineKind::Corner, load, |_| {});
    let alpha = b.stage_of(11);
    b.desk(|d| {
        d.last_press = Some(clock::now());
        d.last_click = d.last_press;
        d.post(0.0, Notice::Strip(StripEvent::CardPressed { stage: alpha }));
        d.pictured(&[12]);
    });
    b.advance(80);
    b.desk(|d| d.post(0.0, Notice::Strip(StripEvent::CardClicked { stage: alpha, shift: false })));
    b.advance(3000);
    let out = played(&mut b);
    Run::of(&b, out)
}

// ----- notices during the cover wait --------------------------------------------------------------

/// A card click for Alpha's stage, and the act `make` makes of the bench 10 ms after the
/// click reaches the app (inside the cover wait at every load), under `engine`.
fn during_the_cover_wait<F: FnOnce(&mut Desk) + 'static>(engine: EngineKind, load: f64, make: impl FnOnce(&Bench) -> F) -> Run {
    let mut b = bench(engine, load, |_| {});
    let alpha = b.stage_of(11);
    let act = make(&b);
    b.desk(|d| {
        d.last_press = Some(clock::now());
        d.last_click = d.last_press;
        d.post(0.0, Notice::Strip(StripEvent::CardPressed { stage: alpha }));
    });
    b.advance(80);
    b.desk(|d| {
        d.post(0.0, Notice::Strip(StripEvent::CardClicked { stage: alpha, shift: false }));
        d.at(in_ms(10), act);
    });
    b.advance(3000);
    let out = played(&mut b);
    Run::of(&b, out)
}

/// A card click for Charlie's stage 10 ms into the cover wait of Alpha's.
fn second_click(engine: EngineKind, load: f64) -> Run {
    during_the_cover_wait(engine, load, |b| {
        let stage = b.stage_of(13);
        move |d: &mut Desk| {
            d.last_press = Some(clock::now());
            d.last_click = d.last_press;
            d.post(0.0, Notice::Strip(StripEvent::CardClicked { stage, shift: false }));
        }
    })
}

fn second_click_corner(load: f64) -> Run {
    second_click(EngineKind::Corner, load)
}

fn second_click_spaces(load: f64) -> Run {
    second_click(EngineKind::Spaces, load)
}

/// A press on Bravo's window, the stage leaving, 10 ms into the cover wait (macOS raises it
/// and activates Bravo at once), released 80 ms later.
fn click_on_a_leaving_window(load: f64) -> Run {
    let p = Point::new(frame(2).x + 300.0, frame(2).y + 10.0);
    during_the_cover_wait(EngineKind::Corner, load, |_| {
        move |d: &mut Desk| {
            d.press(p);
            let up = clock::now() + Duration::from_millis(80);
            d.at(up, |d| d.release());
        }
    })
}

/// Charlie activates itself (an `open`, a notification clicked) 10 ms into the cover wait.
fn activation_during_the_cover_wait(engine: EngineKind, load: f64) -> Run {
    during_the_cover_wait(engine, load, |_| |d: &mut Desk| d.activate(C, false))
}

fn activation_during_the_cover_wait_corner(load: f64) -> Run {
    activation_during_the_cover_wait(EngineKind::Corner, load)
}

fn activation_during_the_cover_wait_spaces(load: f64) -> Run {
    activation_during_the_cover_wait(EngineKind::Spaces, load)
}

// ----- the raise-order check ------------------------------------------------------------------

/// A click on the card of Alpha's and Charlie's stage, `edit` applied to the desk first.
fn order_check(engine: EngineKind, load: f64, edit: impl FnOnce(&mut Desk)) -> Run {
    let mut b = merged(engine, load, edit);
    let stage = b.stage_of(11);
    b.card_click(stage);
    b.advance(10_000);
    let out = played_in_order(&mut b, &[11, 13]);
    Run::of(&b, out)
}

/// `engine/raise_order.rs`: two apps' windows raised, both in order at once.
fn order_right_at_once(load: f64) -> Run {
    order_check(EngineKind::Corner, load, |_| {})
}

/// `engine/raise_order.rs` (the fixture's 40 ms lag): Alpha's window reaches the window
/// server's order 40 ms after its raise.
fn order_late_40(load: f64) -> Run {
    order_check(EngineKind::Corner, load, |d| d.apps.get_mut(&A).expect("Alpha").reorders_ms = Some(40.0))
}

/// `engine/raise_order.rs` (`WATCH_AFTER_FIX`: "a slow app's need not"): Alpha's reorder
/// 300 ms after its raise.
fn order_late_300(load: f64) -> Run {
    order_check(EngineKind::Corner, load, |d| d.apps.get_mut(&A).expect("Alpha").reorders_ms = Some(300.0))
}

/// The same 40 ms lag under the Spaces engine.
fn order_late_40_spaces(load: f64) -> Run {
    order_check(EngineKind::Spaces, load, |d| d.apps.get_mut(&A).expect("Alpha").reorders_ms = Some(40.0))
}

/// Bravo activated by its Dock item 20 ms after the landing's raise of Charlie's window (the
/// key), Alpha's reorder 40 ms late: another app's window comes in front during the check.
fn activation_during_the_check(load: f64) -> Run {
    order_check(EngineKind::Corner, load, |d| {
        d.apps.get_mut(&A).expect("Alpha").reorders_ms = Some(40.0);
        // The landing's raise of the key window is its second (the first went with the parks).
        d.on_raise(13, 2, Duration::from_millis(20), |d| d.activate(B, true));
    })
}

/// A click on Alpha's window 20 ms after the landing's raise of the key (Charlie's), Alpha's
/// reorder 40 ms late: the person's click ends the check.
fn click_during_the_check(load: f64) -> Run {
    let p = Point::new(frame(1).x + 20.0, frame(1).y + 10.0);
    order_check(EngineKind::Corner, load, move |d| {
        d.apps.get_mut(&A).expect("Alpha").reorders_ms = Some(40.0);
        d.on_raise(13, 2, Duration::from_millis(20), move |d| {
            d.press(p);
            let up = clock::now() + Duration::from_millis(80);
            d.at(up, |d| d.release());
        });
    })
}

// ----- the outcomes, pinned --------------------------------------------------------------------

/// a card click, the strip in time.
#[test]
fn pinned_strip_in_time() {
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 1, at once 0; shots [12], strip, park 12, raise 11, main 11, front 101, show 11, raise 11, raise 11, main 11, front 101";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(strip_in_time, [at_1, at_1, "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; shots [12], strip, park 12, show 11, raise 11, raise 11, main 11, front 101"]);
}

/// a card click, the strip in time (Spaces).
#[test]
fn pinned_strip_in_time_spaces() {
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 1, at once 0; shots [12], strip, space [12], raise 11, main 11, front 101, space [11], raise 11, raise 11, main 11, front 101";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(strip_in_time_spaces, [at_1, at_1, "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; shots [12], strip, space [12], space [11], raise 11, raise 11, main 11, front 101"]);
}

/// a card click, the strip 200 ms late.
#[test]
fn pinned_strip_late() {
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; shots [12], strip, park 12, show 11, raise 11, raise 11, main 11, front 101";
    expect(strip_late, [at_1, at_1, at_1]);
}

/// a card click, the strip never answers.
#[test]
fn pinned_strip_never() {
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; shots [12], strip, park 12, show 11, raise 11, raise 11, main 11, front 101";
    expect(strip_never, [at_1, at_1, at_1]);
}

/// a Dock click, its pictures (Corner).
#[test]
fn pinned_dock_click_pictures_corner() {
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 1, at once 0; main 11, front 101, shots [12], strip, park 12, raise 11, main 11, front 101, show 11, raise 11, raise 11, main 11, front 101";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(dock_click_pictures_corner, [at_1, at_1, "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; main 11, front 101, shots [12], strip, park 12, show 11, raise 11, raise 11, main 11, front 101"]);
}

/// a Dock click, captures off (Corner).
#[test]
fn pinned_dock_click_captures_off() {
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 1, at once 0; strip, main 11, front 101, park 12, raise 11, main 11, front 101, show 11, raise 11, raise 11, main 11, front 101";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(dock_click_captures_off, [at_1, at_1, "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; strip, main 11, front 101, park 12, show 11, raise 11, raise 11, main 11, front 101"]);
}

/// a Dock click, its pictures (Spaces).
#[test]
fn pinned_dock_click_pictures_spaces() {
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 1, at once 0; main 11, front 101, shots [12], strip, space [12], raise 11, main 11, front 101, space [11], raise 11, raise 11, main 11, front 101";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(dock_click_pictures_spaces, [at_1, at_1, "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; main 11, front 101, shots [12], strip, space [12], space [11], raise 11, raise 11, main 11, front 101"]);
}

/// ⌘Tab let go at once, its pictures.
#[test]
fn pinned_cmd_tab_quick() {
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 1, at once 0; strip, main 11, front 101, park 12, raise 11, main 11, front 101, show 11, raise 11, raise 11, main 11, front 101";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(cmd_tab_quick, [at_1, at_1, "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; strip, main 11, front 101, park 12, show 11, raise 11, raise 11, main 11, front 101"]);
}

/// ⌘Tab let go at once, its pictures (Spaces).
#[test]
fn pinned_cmd_tab_quick_spaces() {
    // The strip first, as under the Corner engine: before round switch-pictures the early
    // focus went ahead of it here ('main 11, front 101, strip').
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 1, at once 0; strip, main 11, front 101, space [12], raise 11, main 11, front 101, space [11], raise 11, raise 11, main 11, front 101";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(cmd_tab_quick_spaces, [at_1, at_1, "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; strip, main 11, front 101, space [12], space [11], raise 11, raise 11, main 11, front 101"]);
}

/// ⌘Tab held 0.7 s, its pictures.
#[test]
fn pinned_cmd_tab_held() {
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 1, at once 0; main 11, front 101, shots [12], strip, park 12, raise 11, main 11, front 101, show 11, raise 11, raise 11, main 11, front 101";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(cmd_tab_held, [at_1, at_1, "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; main 11, front 101, shots [12], strip, park 12, show 11, raise 11, raise 11, main 11, front 101"]);
}

/// a card click, its pictures fresh from the press.
#[test]
fn pinned_card_click_pictures_fresh() {
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 1, at once 0; strip, park 12, raise 11, main 11, front 101, show 11, raise 11, raise 11, main 11, front 101";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(card_click_pictures_fresh, [at_1, at_1, "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; strip, park 12, show 11, raise 11, raise 11, main 11, front 101"]);
}

/// a second card click during the cover wait.
#[test]
fn pinned_second_click_corner() {
    let at_1 = "front [13] shows [13] desktop 1 app 103 switches 2 slides 0; pictures 2, at once 0; shots [12], strip, park 12, raise 11, main 11, front 101, show 11, raise 11, shots [11], strip, park 11, raise 13, main 13, front 103, show 13, raise 13, raise 13, main 13, front 103";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(second_click_corner, [at_1, at_1, "front [13] shows [13] desktop 1 app 103 switches 2 slides 0; pictures 0, at once 2; shots [12], strip, park 12, show 11, raise 11, raise 11, main 11, front 101, shots [11], strip, park 11, show 13, raise 13, raise 13, main 13, front 103"]);
}

/// a second card click during the cover wait (Spaces).
#[test]
fn pinned_second_click_spaces() {
    let at_1 = "front [13] shows [13] desktop 1 app 103 switches 2 slides 0; pictures 2, at once 0; shots [12], strip, space [12], raise 11, main 11, front 101, space [11], raise 11, shots [11], strip, space [11], raise 13, main 13, front 103, space [13], raise 13, raise 13, main 13, front 103";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(second_click_spaces, [at_1, at_1, "front [13] shows [13] desktop 1 app 103 switches 2 slides 0; pictures 0, at once 2; shots [12], strip, space [12], space [11], raise 11, raise 11, main 11, front 101, shots [11], strip, space [11], space [13], raise 13, raise 13, main 13, front 103"]);
}

/// a click on a leaving window during the cover wait.
#[test]
fn pinned_click_on_a_leaving_window() {
    let at_1 = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 1, at once 0; shots [12], strip, park 12, raise 11, main 11, front 101, show 11, raise 11, raise 11, main 11, front 101";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(click_on_a_leaving_window, [at_1, at_1, "front [11] shows [11] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; shots [12], strip, park 12, show 11, raise 11, raise 11, main 11, front 101"]);
}

/// an app's activation during the cover wait.
#[test]
fn pinned_activation_during_the_cover_wait_corner() {
    let at_1 = "front [13] shows [13] desktop 1 app 103 switches 2 slides 0; pictures 2, at once 0; shots [12], strip, park 12, raise 11, main 11, front 101, show 11, raise 11, show 13, shots [11], strip, park 11, raise 13, main 13, front 103, raise 13, raise 13, main 13, front 103";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 no pictures, and Alpha ends frontmost under Charlie's stage (our focus of Alpha, sent before the wait, lands after Charlie's activation).
    expect(activation_during_the_cover_wait_corner, [at_1, at_1, "front [13] shows [13] desktop 1 app 101 switches 2 slides 0; pictures 0, at once 2; shots [12], strip, park 12, show 11, raise 11, raise 11, main 11, front 101, show 13, shots [11], strip, park 11, raise 13, raise 13, main 13, front 103"]);
}

/// an app's activation during the cover wait (Spaces).
#[test]
fn pinned_activation_during_the_cover_wait_spaces() {
    let at_1 = "front [13] shows [13] desktop 1 app 103 switches 2 slides 0; pictures 2, at once 0; shots [12], strip, space [12], raise 11, main 11, front 101, space [11], raise 11, main 13, front 103, shots [11], strip, space [11], raise 13, main 13, front 103, space [13], raise 13, raise 13, main 13, front 103";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 no pictures, and Alpha ends frontmost under Charlie's stage (our focus of Alpha, sent before the wait, lands after Charlie's activation).
    expect(activation_during_the_cover_wait_spaces, [at_1, at_1, "front [13] shows [13] desktop 1 app 101 switches 2 slides 0; pictures 0, at once 2; shots [12], strip, space [12], space [11], raise 11, raise 11, main 11, front 101, main 13, front 103, shots [11], strip, space [11], space [13], raise 13, raise 13, main 13, front 103"]);
}

/// two apps' raises in order at once.
#[test]
fn pinned_order_right_at_once() {
    let at_1 = "front [11, 13] shows [11, 13] desktop 1 app 103 switches 1 slides 0; pictures 1, at once 0; shots [12], strip, park 12, raise 13, main 13, front 103, show 11, show 13, raise 11, raise 13, raise 13, main 13, front 103; stacked [13, 11]";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(order_right_at_once, [at_1, at_1, "front [11, 13] shows [11, 13] desktop 1 app 103 switches 1 slides 0; pictures 0, at once 1; shots [12], strip, park 12, show 11, show 13, raise 11, raise 13, raise 13, main 13, front 103; stacked [13, 11]"]);
}

/// an app reordering 40 ms late.
#[test]
fn pinned_order_late_40() {
    let at_1 = "front [11, 13] shows [11, 13] desktop 1 app 103 switches 1 slides 0; pictures 1, at once 0; shots [12], strip, park 12, raise 13, main 13, front 103, show 11, show 13, raise 11, raise 13, raise 13, main 13, front 103, raise 13, raise 13, main 13, front 103; stacked [13, 11]";
    // TIME DECIDES: WATCH_AFTER_FIX: at load 3 Alpha's reorder (120 ms) lands after the watch, over the key; COVER_TIMEOUT at 10.
    expect(order_late_40, [at_1, "front [11, 13] shows [11, 13] desktop 1 app 103 switches 1 slides 0; pictures 1, at once 0; shots [12], strip, park 12, raise 13, main 13, front 103, show 11, show 13, raise 11, raise 13, raise 13, main 13, front 103; stacked [11, 13]", "front [11, 13] shows [11, 13] desktop 1 app 103 switches 1 slides 0; pictures 0, at once 1; shots [12], strip, park 12, show 11, show 13, raise 11, raise 13, raise 13, main 13, front 103; stacked [11, 13]"]);
}

/// an app reordering 300 ms late.
#[test]
fn pinned_order_late_300() {
    let at_1 = "front [11, 13] shows [11, 13] desktop 1 app 103 switches 1 slides 0; pictures 1, at once 0; shots [12], strip, park 12, raise 13, main 13, front 103, show 11, show 13, raise 11, raise 13, raise 13, main 13, front 103; stacked [11, 13]";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(order_late_300, [at_1, at_1, "front [11, 13] shows [11, 13] desktop 1 app 103 switches 1 slides 0; pictures 0, at once 1; shots [12], strip, park 12, show 11, show 13, raise 11, raise 13, raise 13, main 13, front 103; stacked [11, 13]"]);
}

/// an app reordering 40 ms late (Spaces).
#[test]
fn pinned_order_late_40_spaces() {
    let at_1 = "front [11, 13] shows [11, 13] desktop 1 app 103 switches 1 slides 0; pictures 1, at once 0; shots [12], strip, space [12], raise 13, main 13, front 103, space [11], space [13], raise 11, raise 13, raise 13, main 13, front 103, raise 13, raise 13, main 13, front 103; stacked [13, 11]";
    // TIME DECIDES: WATCH_AFTER_FIX: at load 3 Alpha's reorder (120 ms) lands after the watch, over the key; COVER_TIMEOUT at 10.
    expect(order_late_40_spaces, [at_1, "front [11, 13] shows [11, 13] desktop 1 app 103 switches 1 slides 0; pictures 1, at once 0; shots [12], strip, space [12], raise 13, main 13, front 103, space [11], space [13], raise 11, raise 13, raise 13, main 13, front 103; stacked [11, 13]", "front [11, 13] shows [11, 13] desktop 1 app 103 switches 1 slides 0; pictures 0, at once 1; shots [12], strip, space [12], space [11], space [13], raise 11, raise 13, raise 13, main 13, front 103; stacked [11, 13]"]);
}

/// an app's activation during the order check.
#[test]
fn pinned_activation_during_the_check() {
    // Load 1 changed by r6-waits (the calls only): the check reads between notices, so
    // Bravo's windows in front stop it before its fix; the blocking check fixed the order
    // first, Bravo's activation held behind it.
    let at_1 = "front [12] shows [12] desktop 1 app 102 switches 2 slides 0; pictures 2, at once 0; shots [12], strip, park 12, raise 13, main 13, front 103, show 11, show 13, raise 11, raise 13, raise 13, main 13, front 103, show 12, shots [11, 13], strip, park 13, park 11, raise 12, main 12, front 102, raise 12, raise 12, main 12, front 102; stacked [11, 13]";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(activation_during_the_check, [at_1, at_1, "front [12] shows [12] desktop 1 app 102 switches 2 slides 0; pictures 0, at once 2; shots [12], strip, park 12, show 11, show 13, raise 11, raise 13, raise 13, main 13, front 103, show 12, shots [11, 13], strip, park 13, park 11, raise 12, raise 12, main 12, front 102; stacked [11, 13]"]);
}

/// a click during the order check.
#[test]
fn pinned_click_during_the_check() {
    let at_1 = "front [11, 13] shows [11, 13] desktop 1 app 101 switches 1 slides 0; pictures 1, at once 0; shots [12], strip, park 12, raise 13, main 13, front 103, show 11, show 13, raise 11, raise 13, raise 13, main 13, front 103; stacked [11, 13]";
    // TIME DECIDES: COVER_TIMEOUT: at load 10 the strip answers (200 ms) past the cap; the windows move at once, no pictures.
    expect(click_during_the_check, [at_1, at_1, "front [11, 13] shows [11, 13] desktop 1 app 101 switches 1 slides 0; pictures 0, at once 1; shots [12], strip, park 12, show 11, show 13, raise 11, raise 13, raise 13, main 13, front 103; stacked [11, 13]"]);
}

/// Every scenario, for the load table.
pub(super) const ALL: &[Scenario] = &[
    ("a card click, the strip in time", "controller.rs send_transition, COVER_TIMEOUT", strip_in_time),
    ("a card click, the strip in time (Spaces)", "controller.rs send_transition, switch_pictures", strip_in_time_spaces),
    ("a card click, the strip 200 ms late", "strip_model::wait_cover, COVER_TIMEOUT", strip_late),
    ("a Dock click, its pictures (Corner)", "controller/switch_pictures.rs, round switch-pictures", dock_click_pictures_corner),
    ("a Dock click, its pictures (Spaces)", "controller/switch_pictures.rs, round switch-pictures", dock_click_pictures_spaces),
    ("a Dock click, captures off (Corner)", "platform/switch_shots.rs may_take", dock_click_captures_off),
    ("⌘Tab let go at once, its pictures", "controller/switch_pictures.rs FRESH", cmd_tab_quick),
    ("⌘Tab let go at once, its pictures (Spaces)", "controller/switch_pictures.rs FRESH", cmd_tab_quick_spaces),
    ("⌘Tab held 0.7 s, its pictures", "controller/switch_pictures.rs FRESH", cmd_tab_held),
    ("a card click, its pictures fresh from the press", "controller/switch_pictures.rs FRESH", card_click_pictures_fresh),
    ("a card click, the strip never answers", "strip_model::wait_cover, COVER_TIMEOUT", strip_never),
    ("a second card click during the cover wait", "controller.rs flush_superseded, r6-waits", second_click_corner),
    ("a second card click during the cover wait (Spaces)", "controller.rs flush_superseded, r6-waits", second_click_spaces),
    ("a click on a leaving window during the cover wait", "controller.rs handle_event, r6-waits", click_on_a_leaving_window),
    ("an app's activation during the cover wait", "controller.rs handle_event, r6-waits", activation_during_the_cover_wait_corner),
    ("an app's activation during the cover wait (Spaces)", "controller.rs handle_event, r6-waits", activation_during_the_cover_wait_spaces),
    ("two apps' raises in order at once", "engine/raise_order.rs WATCH", order_right_at_once),
    ("an app reordering 40 ms late", "engine/raise_order.rs WATCH_AFTER_FIX, the fixture's lag", order_late_40),
    ("an app reordering 300 ms late", "engine/raise_order.rs WATCH_AFTER_FIX, a slow app", order_late_300),
    ("an app reordering 40 ms late (Spaces)", "engine/raise_order.rs, the fixture's lag", order_late_40_spaces),
    ("an app's activation during the order check", "engine/raise_order.rs front_moved", activation_during_the_check),
    ("a click during the order check", "engine/raise_order.rs UserWatch", click_during_the_check),
];

/// Prints every scenario's outcome at each load, for the assertions below:
/// `BENCH_PRINT=1 cargo test --lib switch::print_them -- --nocapture`.
#[test]
fn print_them() {
    if std::env::var("BENCH_PRINT").is_err() {
        return;
    }
    for (name, _, script) in ALL {
        for load in super::scenarios::LOADS {
            println!("{name} @{load}: {}", script(load).outcome);
        }
    }
}
