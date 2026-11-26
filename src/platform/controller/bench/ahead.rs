//! ⌘Tab to an app on another Desktop (round arranged-ahead; audit engine-uniformity 11-25,
//! H5): the twin of `scenarios::dock_click_that_slides` for the app switcher, under both
//! engines.
//!
//! The desk: Alpha with a window on each Desktop (11, 13), Bravo likewise (12, 15), Charlie
//! with one window on Desktop 2 only (14). Charlie is the app used before Bravo, so one Tab
//! selects it, and Desktop 2 was left with Bravo's stage in front: Charlie's window waits
//! hidden there (in the corner, on its Desktop; or in a stage Space, on none). ⌘Tab to
//! Charlie makes macOS slide to Desktop 2.
//!
//! Each outcome reads what stood in front on Desktop 2 when the switcher ended (⌘ let go, or
//! Escape), which of Desktop 2's windows a person saw on the way (the window gone to alone,
//! when its Desktop slid in arranged; with the stage left, when it was arranged in view;
//! none, when macOS stayed), how many moves of Desktop 2's windows went out in all (through
//! Accessibility, or into and out of a Space), whether Desktop 2 ended as it was before
//! (every window of it at its frame and on its Desktop or off every one, as before), and what
//! shows at the end.

use std::collections::BTreeSet;

use super::scenarios::{A, B, C, D1, D2, Run, Scenario, expect, seen};
use super::*;
use crate::platform::fake::Desk;

/// Delta: a fourth app, its one window (17) on Desktop 2.
const D: i32 = 104;

/// The desk and its history (module docs), at `load` from the ⌘Tab on.
fn charlie_used_before(engine: EngineKind, load: f64, edit: impl FnOnce(&mut Desk)) -> Bench {
    let mut b = Bench::new(engine, DEFAULTS, 1.0, 2, |d| {
        d.add_app(A, "com.example.alpha", "Alpha");
        d.add_app(B, "com.example.bravo", "Bravo");
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_window(13, A, frame(3), D2);
        d.add_window(14, C, frame(4), D2);
        d.add_window(15, B, frame(5), D2);
        d.add_window(11, A, frame(1), D1);
        d.add_window(12, B, frame(2), D1);
        d.front = Some(B);
        edit(d);
    });
    b.go_to_desktop(D2, true);
    b.advance(4000);
    // Charlie's stage, then Bravo's: Charlie is the app used before Bravo.
    let charlie = b.stage_of(14);
    b.card_click(charlie);
    b.advance(4000);
    let bravo = b.stage_of(15);
    b.card_click(bravo);
    b.advance(4000);
    b.go_to_desktop(D1, true);
    b.advance(6000);
    let there = b.space_of(14).expect("Desktop 2");
    let start = (b.front(), b.desktop_shown(), b.front_app(), b.front_in(there), b.on_screen().into_iter().collect::<Vec<_>>());
    assert_eq!(start, (Some(vec![12]), D1, Some(B), Some(vec![15]), vec![12]), "the start:\n{}", b.story());
    let hidden_on = b.desktop_of(14);
    assert_eq!(hidden_on, (engine == EngineKind::Corner).then_some(D2), "Charlie's window waits in the corner on its Desktop, or in a stage Space on none");
    b.log.clear();
    crate::engine::attribution::trail::clear();
    b.desk(|d| {
        d.strip.transitions = 0;
        d.calls.clear();
        d.load = load;
    });
    b
}

/// How a ⌘Tab ends.
#[derive(Clone, Copy)]
enum Ends {
    /// ⌘ let go this many ms after the Tab (the Dock's list shows 180 ms after it, times the
    /// load: a quicker ⌘Tab never has it).
    LetGo(u64),
    /// Escape this many ms after the Tab, ⌘ let go 300 ms later.
    Escape(u64),
    /// A second Tab 300 ms after the first (the selection moves on), ⌘ let go 150 ms later.
    TabOn,
    /// ⌘ let go before the WM thread got to the Tab; macOS has picked its Desktop already.
    Unhandled,
    /// ⌘ let go this many ms after the Tab on macOS's own clock, while the Tab's handler may
    /// still be asking the app selected (a slow one).
    LetGoMeanwhile(u64),
}

/// The frame and Desktop (`None`: on no Desktop) of each of the windows `of` on the desk.
fn desk_as_is(b: &mut Bench, of: &BTreeSet<u32>) -> Vec<(u32, Rect, Option<u64>)> {
    b.desk(|d| of.iter().filter_map(|id| d.windows.get(id)).map(|w| (w.id, w.frame, d.desktop_of(w.id))).collect())
}

/// A ⌘Tab from Desktop 1 with the switcher listing `order` (the app in front first; it
/// selects the second), ended as `ends` says (module docs for the outcome).
fn cmd_tab(engine: EngineKind, load: f64, order: Vec<i32>, ends: Ends, edit: impl FnOnce(&mut Desk)) -> Run {
    let mut b = charlie_used_before(engine, load, edit);
    if let Ends::LetGoMeanwhile(_) = ends {
        // Slow to answer, by the clock (whatever the load): its round takes about 90 ms.
        b.desk(|d| d.apps.get_mut(&C).expect("Charlie").answers_ms = Some(22.0 / d.load));
    }
    let there = b.space_of(14).expect("Desktop 2");
    // Desktop 2's windows: every one but Desktop 1's two.
    let theirs: BTreeSet<u32> = b.desk(|d| d.windows.keys().copied().filter(|id| ![11, 12].contains(id)).collect());
    let before = desk_as_is(&mut b, &theirs);
    let mut on_the_way: BTreeSet<u32> = BTreeSet::new();
    let mut pass = |b: &mut Bench, ms: u64| {
        for _ in 0..ms / 10 {
            b.advance(10);
            on_the_way.extend(b.on_screen().intersection(&theirs));
        }
    };
    if !matches!(ends, Ends::Unhandled) {
        b.cmd_tab(order.clone());
    }
    let at_end = match ends {
        Ends::Unhandled => {
            b.cmd_tab_let_go_unhandled(order);
            b.front_in(there)
        }
        Ends::LetGoMeanwhile(ms) => {
            b.cmd_tab_release_in(ms);
            pass(&mut b, ms + 150);
            b.front_in(there)
        }
        Ends::LetGo(ms) => {
            pass(&mut b, ms);
            let at_end = b.front_in(there);
            b.cmd_tab_release();
            at_end
        }
        Ends::Escape(ms) => {
            pass(&mut b, ms);
            let at_end = b.front_in(there);
            b.cmd_tab_escape_key();
            pass(&mut b, 300);
            b.cmd_tab_command_up();
            at_end
        }
        Ends::TabOn => {
            pass(&mut b, 300);
            b.cmd_tab_again();
            pass(&mut b, 150);
            let at_end = b.front_in(there);
            b.cmd_tab_release();
            at_end
        }
    };
    pass(&mut b, 12_000);
    let as_it_was = desk_as_is(&mut b, &theirs) == before;
    let moves = b
        .calls()
        .iter()
        .filter(|c| match c {
            fake::Call::Frame(id, _) => theirs.contains(id),
            fake::Call::IntoSpace(_, ids) => ids.iter().any(|id| theirs.contains(id)),
            _ => false,
        })
        .count();
    let out = format!(
        "Desktop 2 had {} in front at the switcher's end; seen of Desktop 2 on the way {:?}; {moves} moves there; Desktop 2 as it was {as_it_was}; {}",
        at_end.map_or("none".to_string(), |w| format!("{w:?}")),
        on_the_way.into_iter().collect::<Vec<_>>(),
        seen(&mut b)
    );
    Run::of(&b, out)
}

/// The switcher as the app expects it: Bravo in front, Charlie next.
fn as_used() -> Vec<i32> {
    vec![B, C, A]
}

/// The switcher with Alpha second: the first Tab selects Alpha, where the app's own order of
/// activations says Charlie (an app activated before this run started, say). Alpha has a
/// window on Desktop 1, hidden: macOS activates it here.
fn alpha_second() -> Vec<i32> {
    vec![B, A, C]
}

/// Delta's window (17) on Desktop 2, in a stage of its own, hidden like Charlie's; behind
/// the others there, as before it was added (Bravo's stays each Desktop's front window).
fn with_delta(d: &mut Desk) {
    d.add_app(D, "com.example.delta", "Delta");
    d.add_window(17, D, frame(7), D2);
    for id in [15, 11, 12] {
        d.raise(id);
    }
}

// ----- released onto the app on another Desktop --------------------------------------------

/// A quick ⌘Tab: ⌘ let go 60 ms after the Tab, before the Dock ever lists the switcher.
fn quick(engine: EngineKind, load: f64) -> Run {
    cmd_tab(engine, load, as_used(), Ends::LetGo(60), |_| {})
}

/// A browsed ⌘Tab: ⌘ let go 400 ms after the Tab, past the Dock's list at load 1.
fn browsed(engine: EngineKind, load: f64) -> Run {
    cmd_tab(engine, load, as_used(), Ends::LetGo(400), |_| {})
}

fn quick_corner(load: f64) -> Run {
    quick(EngineKind::Corner, load)
}

fn quick_spaces(load: f64) -> Run {
    quick(EngineKind::Spaces, load)
}

fn browsed_corner(load: f64) -> Run {
    browsed(EngineKind::Corner, load)
}

fn browsed_spaces(load: f64) -> Run {
    browsed(EngineKind::Spaces, load)
}

/// The browsed ⌘Tab with the key tap refused by macOS: Escape would never be heard, so a
/// switcher's cancel could not be told from its release.
fn browsed_tap_refused_corner(load: f64) -> Run {
    cmd_tab(EngineKind::Corner, load, as_used(), Ends::LetGo(400), |d| d.tap_hears = false)
}

fn browsed_tap_refused_spaces(load: f64) -> Run {
    cmd_tab(EngineKind::Spaces, load, as_used(), Ends::LetGo(400), |d| d.tap_hears = false)
}

/// ⌘ let go before the WM thread got to the Tab: macOS picked the Desktop before anything
/// could be arranged. A window in a stage Space was on no Desktop then, so macOS goes
/// nowhere by itself.
fn unhandled_corner(load: f64) -> Run {
    cmd_tab(EngineKind::Corner, load, as_used(), Ends::Unhandled, |_| {})
}

fn unhandled_spaces(load: f64) -> Run {
    cmd_tab(EngineKind::Spaces, load, as_used(), Ends::Unhandled, |_| {})
}

/// Charlie slow to answer (22 ms an answer; the Tab's round with it takes about 90), and ⌘
/// let go 60 ms after the Tab: macOS picks the Desktop while the app is still being asked,
/// before anything is arranged. Under the Spaces engine Charlie's window was on no Desktop
/// then and macOS goes nowhere by itself.
fn let_go_while_asked(engine: EngineKind, load: f64) -> Run {
    // The race is the scenario's own: the app's slowness is set for the load it runs at.
    cmd_tab(engine, load, as_used(), Ends::LetGoMeanwhile(60), move |_| {})
}

fn let_go_while_asked_corner(load: f64) -> Run {
    let_go_while_asked(EngineKind::Corner, load)
}

fn let_go_while_asked_spaces(load: f64) -> Run {
    let_go_while_asked(EngineKind::Spaces, load)
}

// ----- not released onto it -----------------------------------------------------------------

/// Cancelled with Escape 400 ms after the Tab.
fn cancelled_corner(load: f64) -> Run {
    cmd_tab(EngineKind::Corner, load, as_used(), Ends::Escape(400), |_| {})
}

fn cancelled_spaces(load: f64) -> Run {
    cmd_tab(EngineKind::Spaces, load, as_used(), Ends::Escape(400), |_| {})
}

/// A second Tab moves the selection on from Charlie to Alpha, and ⌘ is let go there: macOS
/// activates Alpha, which has a window on Desktop 1.
fn tabbed_on_corner(load: f64) -> Run {
    cmd_tab(EngineKind::Corner, load, as_used(), Ends::TabOn, |_| {})
}

fn tabbed_on_spaces(load: f64) -> Run {
    cmd_tab(EngineKind::Spaces, load, as_used(), Ends::TabOn, |_| {})
}

// ----- the first Tab's guess wrong ----------------------------------------------------------

/// The guess wrong and never corrected: ⌘ let go on Alpha 60 ms after the Tab.
fn wrong_guess_quick_corner(load: f64) -> Run {
    cmd_tab(EngineKind::Corner, load, alpha_second(), Ends::LetGo(60), |_| {})
}

fn wrong_guess_quick_spaces(load: f64) -> Run {
    cmd_tab(EngineKind::Spaces, load, alpha_second(), Ends::LetGo(60), |_| {})
}

/// The guess wrong, corrected by the Dock's list: ⌘ let go on Alpha 400 ms after the Tab.
fn wrong_guess_browsed_corner(load: f64) -> Run {
    cmd_tab(EngineKind::Corner, load, alpha_second(), Ends::LetGo(400), |_| {})
}

fn wrong_guess_browsed_spaces(load: f64) -> Run {
    cmd_tab(EngineKind::Spaces, load, alpha_second(), Ends::LetGo(400), |_| {})
}

/// The guess wrong, never corrected, and the app let go on (Delta) lives on the guessed
/// app's Desktop: macOS slides to Desktop 2 for Delta, 60 ms after the Tab. The Corner
/// engine (Delta's window in a stage Space takes macOS nowhere).
fn wrong_guess_quick_same_desktop_corner(load: f64) -> Run {
    cmd_tab(EngineKind::Corner, load, vec![B, D, C, A], Ends::LetGo(60), with_delta)
}

/// The same, corrected by the Dock's list: ⌘ let go on Delta 400 ms after the Tab.
fn wrong_guess_browsed_same_desktop_corner(load: f64) -> Run {
    cmd_tab(EngineKind::Corner, load, vec![B, D, C, A], Ends::LetGo(400), with_delta)
}

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

/// Every scenario here, for the load table.
pub(super) const ALL: &[Scenario] = &[
    ("Cmd-Tab let go at once onto an app on another Desktop (Corner)", "audit 11-25 H5, the twin of the Dock click that slides", quick_corner),
    ("Cmd-Tab let go at once onto an app hidden on another Desktop (Spaces)", "audit 11-25 H5", quick_spaces),
    ("Cmd-Tab held 0.4 s, let go onto an app on another Desktop (Corner)", "audit 11-25 H5, the Dock's list read", browsed_corner),
    ("Cmd-Tab held 0.4 s, let go onto an app hidden on another Desktop (Spaces)", "audit 11-25 H5, the Dock's list read", browsed_spaces),
    ("the same, the key tap refused (Corner)", "mission_key_tap.rs, a cancel never heard", browsed_tap_refused_corner),
    ("the same, the key tap refused (Spaces)", "mission_key_tap.rs, a cancel never heard", browsed_tap_refused_spaces),
    ("Cmd-Tab let go before its Tab was handled, the app on another Desktop (Corner)", "controller/switcher.rs, a Tab handled late", unhandled_corner),
    ("Cmd-Tab let go before its Tab was handled, the app hidden on another Desktop (Spaces)", "controller/arrival.rs, late_for_macos", unhandled_spaces),
    ("Cmd-Tab let go while the app selected is still asked, on another Desktop (Corner)", "controller/arrival.rs, late_for_macos", let_go_while_asked_corner),
    ("Cmd-Tab let go while the app selected is still asked, hidden on another Desktop (Spaces)", "controller/arrival.rs, late_for_macos", let_go_while_asked_spaces),
    ("Cmd-Tab to an app on another Desktop, cancelled with Escape (Corner)", "audit 11-25 H5, undone at Escape", cancelled_corner),
    ("Cmd-Tab to an app hidden on another Desktop, cancelled with Escape (Spaces)", "audit 11-25 H5, undone at Escape", cancelled_spaces),
    ("Cmd-Tab past an app on another Desktop, let go on one here (Corner)", "audit 11-25 H5, undone when the selection moves on", tabbed_on_corner),
    ("Cmd-Tab past an app hidden on another Desktop, let go on one here (Spaces)", "audit 11-25 H5, undone when the selection moves on", tabbed_on_spaces),
    ("Cmd-Tab let go at once, the first Tab's guess wrong, the app here (Corner)", "audit 11-25 H5, a wrong guess", wrong_guess_quick_corner),
    ("Cmd-Tab let go at once, the first Tab's guess wrong, the app here (Spaces)", "audit 11-25 H5, a wrong guess", wrong_guess_quick_spaces),
    ("Cmd-Tab held 0.4 s, the first Tab's guess wrong, the app here (Corner)", "audit 11-25 H5, the Dock's list corrects the guess", wrong_guess_browsed_corner),
    ("Cmd-Tab held 0.4 s, the first Tab's guess wrong, the app here (Spaces)", "audit 11-25 H5, the Dock's list corrects the guess", wrong_guess_browsed_spaces),
    ("Cmd-Tab let go at once, the guess wrong, the app on the guessed app's Desktop (Corner)", "audit 11-25 H5, a wrong guess that comes into view", wrong_guess_quick_same_desktop_corner),
    ("Cmd-Tab held 0.4 s, the guess wrong, the app on the guessed app's Desktop (Corner)", "audit 11-25 H5, the Dock's list corrects the guess", wrong_guess_browsed_same_desktop_corner),
];

/// Round arranged-ahead: the Desktop is arranged while the switcher shows, from the first
/// Tab's guess on, under both engines. (Before: under the Corner engine at the Space change's
/// notice, "Desktop 2 had [15] in front at the switcher's end; seen of Desktop 2 on the way
/// [14, 15]": it slid in with the stage left in front and changed in view; under Spaces at
/// the activation, "had [15] ...; seen ... [14]".)
#[test]
fn cmd_tab_to_an_app_on_another_desktop() {
    let ahead = "Desktop 2 had [14] in front at the switcher's end; seen of Desktop 2 on the way [14]; 2 moves there; Desktop 2 as it was false; front [14] shows [14] desktop 2 app 103 switches 0 slides 1";
    // A quick ⌘Tab, from the guess alone; a browsed one, the Dock's list agreeing.
    expect(quick_corner, [ahead, ahead, ahead]);
    expect(quick_spaces, [ahead, ahead, ahead]);
    expect(browsed_corner, [ahead, ahead, ahead]);
    expect(browsed_spaces, [ahead, ahead, ahead]);
    // The key tap refused: nothing is arranged ahead, each engine as before the round.
    let in_view = "Desktop 2 had [15] in front at the switcher's end; seen of Desktop 2 on the way [14, 15]; 2 moves there; Desktop 2 as it was false; front [14] shows [14] desktop 2 app 103 switches 0 slides 1";
    let at_activation = "Desktop 2 had [15] in front at the switcher's end; seen of Desktop 2 on the way [14]; 2 moves there; Desktop 2 as it was false; front [14] shows [14] desktop 2 app 103 switches 0 slides 1";
    expect(browsed_tap_refused_corner, [in_view, in_view, in_view]);
    expect(browsed_tap_refused_spaces, [at_activation, at_activation, at_activation]);
    // ⌘ let go before the Tab was handled: the app is in front by then and the keys guess
    // the app left (which has a window here): nothing arranged, as before the round.
    expect(unhandled_corner, [in_view, in_view, in_view]);
    expect(unhandled_spaces, [at_activation, at_activation, at_activation]);
}

/// ⌘ let go while the Tab's handler still asks a slow app: the arrangement goes out after
/// macOS picked the Desktop. Under the Corner engine macOS slides anyway and finds it
/// arranged. Under the Spaces engine the window was on no Desktop when macOS picked: the
/// arrangement stands, and its window is focused at the app's activation, which takes macOS
/// there (`late_for_macos`; without it the app stayed in front with no window, Desktop 1
/// shown). When the app is found in front already while it is asked, nothing is arranged for
/// it and its activation does both, as before the round (the same outcome but for what stood
/// at the switcher's end).
#[test]
fn cmd_tab_let_go_while_the_app_selected_is_still_asked() {
    let ahead = "Desktop 2 had [14] in front at the switcher's end; seen of Desktop 2 on the way [14]; 2 moves there; Desktop 2 as it was false; front [14] shows [14] desktop 2 app 103 switches 0 slides 1";
    expect(let_go_while_asked_corner, [ahead, ahead, ahead]);
    expect(let_go_while_asked_spaces, [ahead, ahead, ahead]);
}

/// Not let go on it: the arrangement is undone on the Desktop nobody sees, the same moves
/// back, every window where it was. (Before: nothing arranged, "0 moves there".)
#[test]
fn cmd_tab_past_an_app_on_another_desktop() {
    let cancelled = "Desktop 2 had [14] in front at the switcher's end; seen of Desktop 2 on the way []; 4 moves there; Desktop 2 as it was true; front [12] shows [12] desktop 1 app 102 switches 0 slides 0";
    expect(cancelled_corner, [cancelled, cancelled, cancelled]);
    expect(cancelled_spaces, [cancelled, cancelled, cancelled]);
    // Undone at the second Tab (the stage left in front again by ⌘'s release); Alpha's stage
    // comes here, animated as before.
    let here = "Desktop 2 had [15] in front at the switcher's end; seen of Desktop 2 on the way []; 4 moves there; Desktop 2 as it was true; front [11] shows [11] desktop 1 app 101 switches 1 slides 0";
    expect(tabbed_on_corner, [here, here, here]);
    expect(tabbed_on_spaces, [here, here, here]);
}

/// The first Tab's guess wrong (the logs of 11-18 to 11-24: 7 of 442 where the Dock's list
/// could tell). The Desktop arranged for the guess is put back as it was, unseen: by the
/// Dock's list while the switcher shows, else by the activation of the app let go on.
#[test]
fn cmd_tab_with_the_first_tabs_guess_wrong() {
    // Let go before the Dock's list: still arranged for the guess at ⌘'s release, undone at
    // Alpha's activation, after its switch here went out (animated, as before).
    let at_activation = "Desktop 2 had [14] in front at the switcher's end; seen of Desktop 2 on the way []; 4 moves there; Desktop 2 as it was true; front [11] shows [11] desktop 1 app 101 switches 1 slides 0";
    expect(wrong_guess_quick_corner, [at_activation, at_activation, at_activation]);
    expect(wrong_guess_quick_spaces, [at_activation, at_activation, at_activation]);
    // TIME DECIDES: the Dock's list (180 ms after the Tab, times the load) against ⌘ held
    // 0.4 s. At load 1 the list corrects the guess while the switcher shows; at load 3 and
    // 10 it never shows, and the activation undoes it.
    let by_the_dock = "Desktop 2 had [15] in front at the switcher's end; seen of Desktop 2 on the way []; 4 moves there; Desktop 2 as it was true; front [11] shows [11] desktop 1 app 101 switches 1 slides 0";
    expect(wrong_guess_browsed_corner, [by_the_dock, at_activation, at_activation]);
    expect(wrong_guess_browsed_spaces, [by_the_dock, at_activation, at_activation]);
}

/// The one wrong guess that comes into view: never corrected (⌘ let go before the Dock's
/// list), and the app let go on has its windows on the Desktop arranged for the guess.
/// macOS slides there: the guessed app's stage (14) shows as the Desktop slides in, then the
/// stage left (15, the undo at the other app's activation), then the app's own (17). Before
/// the round: the stage left, then the app's own ("seen ... [15, 17]; 2 moves there").
#[test]
fn cmd_tab_with_a_wrong_guess_that_comes_into_view() {
    let seen_wrong = "Desktop 2 had [14] in front at the switcher's end; seen of Desktop 2 on the way [14, 15, 17]; 6 moves there; Desktop 2 as it was false; front [17] shows [17] desktop 2 app 104 switches 0 slides 1";
    expect(wrong_guess_quick_same_desktop_corner, [seen_wrong, seen_wrong, seen_wrong]);
    // TIME DECIDES: the Dock's list, as above. Corrected while the switcher shows, the
    // Desktop slides in arranged for the app let go on.
    let corrected = "Desktop 2 had [17] in front at the switcher's end; seen of Desktop 2 on the way [17]; 6 moves there; Desktop 2 as it was false; front [17] shows [17] desktop 2 app 104 switches 0 slides 1";
    expect(wrong_guess_browsed_same_desktop_corner, [corrected, seen_wrong, seen_wrong]);
}
