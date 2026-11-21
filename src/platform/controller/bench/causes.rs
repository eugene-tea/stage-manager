//! Who caused this focus or activation: every gate of `Controller::handle_event`'s Focused
//! and AppActivated arms, pinned by the incident its doc comment tells (round r4-causes).
//!
//! Each outcome reads what a person sees ([`seen`]) and, after it, what each focus and
//! activation notice came to, in the order handled ([`crate::engine::attribution::trail`]):
//! the gate that answered for it. Where a load changes either, the assertion is marked
//! `// TIME DECIDES:`.

use super::scenarios::{A, B, C, D1, D2, Run, Scenario, both_desktops_known, expect, seen, two_apps};
use super::*;
use crate::platform::fake::Desk;

/// What a person sees, then what each notice came to.
fn told(b: &mut Bench) -> String {
    format!("{}; {}", seen(b), b.trail().join(", "))
}

/// The launch's own moves and their echoes are over: the incident starts on a quiet desk.
fn settle(b: &mut Bench) {
    b.advance(3000);
    crate::engine::attribution::trail::clear();
}

/// The trail forgotten, with the counts a history left (the incident starts here).
fn start(b: &mut Bench, load: f64) {
    b.log.clear();
    crate::engine::attribution::trail::clear();
    b.desk(|d| {
        d.strip.transitions = 0;
        d.calls.clear();
        d.load = load;
    });
}

/// Alpha with two windows, each in a stage of its own, and Bravo with one, on one Desktop
/// (the Corner engine): Alpha's window 11's stage in front, 17's and Bravo's hidden. The
/// history runs on the Mac as measured; the load is the incident's.
fn alpha_in_two_stages(load: f64, edit: impl FnOnce(&mut Desk)) -> Bench {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, 1.0, 1, |d| {
        d.add_app(A, "com.example.alpha", "Alpha");
        d.add_app(B, "com.example.bravo", "Bravo");
        d.add_window(17, A, frame(7), D1);
        d.add_window(11, A, frame(1), D1);
        d.add_window(12, B, frame(2), D1);
        d.front = Some(B);
    });
    let alpha = b.stage_of(11);
    b.desk(|d| d.post(0.0, Notice::Strip(StripEvent::WindowToNewCard { stage: alpha, window: 17, before: None })));
    b.advance(3000);
    let first = b.stage_of(11);
    assert_ne!(first, b.stage_of(17), "Alpha's two windows in two stages:\n{}", b.story());
    b.card_click(first);
    b.advance(4000);
    assert_eq!((b.front(), b.front_app()), (Some(vec![11]), Some(A)), "the start:\n{}", b.story());
    b.desk(edit);
    start(&mut b, load);
    b
}

// ----- a focus of ours (`engine/causes.rs`, `focus_may_switch`, `focus_moved_on`) ---------------

/// `controller.rs`, `focus_may_switch` (11-02: with Accessibility slow, focus notices came
/// up to a second late, past the echo window, and each switched stages nobody asked for):
/// a card click for Alpha's stage, one for Bravo's 400 ms later; Alpha says its window was
/// focused 1 s after our focus, when Bravo is in front. It must not bring Alpha's stage back.
fn late_focus_of_an_app_left_behind(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, |d| {
        two_apps(d);
        d.apps.get_mut(&A).expect("Alpha").reports_focus_ms = Some(1000.0);
    });
    settle(&mut b);
    let (alpha, bravo) = (b.stage_of(11), b.stage_of(12));
    b.card_click(alpha);
    b.advance(400);
    b.card_click(bravo);
    b.advance(40_000);
    let out = told(&mut b);
    Run::of(&b, out)
}

/// `engine/stale_activation.rs`, `focus_moved_on` (round stale-activations, the 11-03 class
/// at the focus gate; review 11-21 `core.md` §5d 1): two stages of one app. A card click for
/// the stage of Alpha's window 17, one for window 11's `gap_ms` later; Alpha says 17 was
/// focused `late_ms` after our focus of it, when its focus is on 11 and Alpha is still the
/// app in front. The late notice must not bring 17's stage back.
fn late_focus_within_one_app(load: f64, gap_ms: u64, late_ms: f64, answers_ms: Option<f64>) -> Run {
    let mut b = alpha_in_two_stages(load, |d| {
        let a = d.apps.get_mut(&A).expect("Alpha");
        a.reports_focus_ms = Some(late_ms);
        a.answers_ms = answers_ms;
    });
    let (first, second) = (b.stage_of(11), b.stage_of(17));
    b.card_click(second);
    b.advance(gap_ms);
    b.card_click(first);
    b.advance(40_000);
    let out = told(&mut b);
    Run::of(&b, out)
}

/// The late focus 1.2 s after ours: past the echo window of everything we did to the window.
fn late_focus_of_a_window_left_behind(load: f64) -> Run {
    late_focus_within_one_app(load, 400, 1200.0, None)
}

/// The same from an app that answers Accessibility slowly (6 ms a call on the Mac as
/// measured): asked where its focus is now, it answers past the 50 ms the question may take
/// at load 10.
fn late_focus_of_a_slow_app(load: f64) -> Run {
    late_focus_within_one_app(load, 400, 1200.0, Some(6.0))
}

/// A focus right after a hide of ours on the same window (review 11-21 `core.md` §5d 3,
/// "kind-blind"): the late focus comes 0.7 s after ours, within the echo window of the hide
/// that took its window away 0.4 s after it.
fn late_focus_right_after_our_hide(load: f64) -> Run {
    late_focus_within_one_app(load, 400, 700.0, None)
}

/// The same hide, and the user's own focus after it (`controller.rs`, "a parked window
/// grabbed"): a card click hides Alpha's window 11 into the park corner; as soon as the
/// switch has landed (60 ms after its window shows, about 0.6 s after the park) the user
/// clicks 11's sliver there. Alpha is in front already (its other window's stage came), so
/// no activation follows: the focus is all there is.
fn sliver_click_right_after_its_park(load: f64) -> Run {
    let mut b = alpha_in_two_stages(load, |_| {});
    let second = b.stage_of(17);
    b.card_click(second);
    b.until_shown(17);
    b.advance(60);
    let parked = b.desk(|d| d.windows[&11].frame);
    b.click(Point::new(parked.x + 0.5, parked.y + 6.0));
    b.advance(6000);
    let out = told(&mut b);
    Run::of(&b, out)
}

/// A user's click 300 ms after our switch on a window we just showed (review 11-21
/// `core.md` §6 1: "on any Mac a user act right after ours is swallowed"): Bravo's stage of
/// two windows comes by its card; 300 ms after they show, a click on the one not focused.
fn click_on_a_window_just_shown(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, 1.0, 1, |d| {
        d.add_app(A, "com.example.alpha", "Alpha");
        d.add_app(B, "com.example.bravo", "Bravo");
        d.add_window(11, A, frame(1), D1);
        d.add_window(18, B, frame(8), D1);
        d.add_window(12, B, frame(2), D1);
        d.front = Some(B);
    });
    let (alpha, bravo) = (b.stage_of(11), b.stage_of(12));
    b.card_click(alpha);
    b.advance(4000);
    start(&mut b, load);
    b.card_click(bravo);
    b.until_shown(18);
    b.advance(300);
    b.click(Point::new(1150.0, 740.0));
    b.advance(6000);
    let out = told(&mut b);
    Run::of(&b, out)
}

/// A user's drag right after a show (review 11-21 `core.md` §5d 3: "a user drag within
/// 700 ms of a show is not a drag"): Bravo's stage comes by its card; 200 ms after its
/// window shows the user drags it by its title bar, 240 by 120 in 0.48 s. When the app takes
/// it for a drag, and the frame it keeps for the window after.
fn drag_right_after_a_show(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, 1.0, 1, two_apps);
    let (alpha, bravo) = (b.stage_of(11), b.stage_of(12));
    b.card_click(alpha);
    b.advance(4000);
    start(&mut b, load);
    b.card_click(bravo);
    b.until_shown(12);
    b.advance(200);
    let from = b.desk(|d| d.windows[&12].frame);
    b.drag_window(12, Point::new(80.0, 40.0), 10);
    let early = b.dragging();
    b.drag_window(12, Point::new(160.0, 80.0), 20);
    let late = b.dragging();
    b.release();
    b.advance(6000);
    let kept = b.model_frame(12).map(|f| (f.x - from.x, f.y - from.y));
    let out = format!("a drag 220 ms in: {early}, at its end: {late}; the frame kept moved by {kept:?}; {}", told(&mut b));
    Run::of(&b, out)
}

/// An app that focuses a different window of its own after our focus (review 11-21
/// `core.md` §5d 4): a card click for the stage of Alpha's window 17 while Bravo is in
/// front; 150 ms later Alpha makes its other window, 11, its key window by itself.
fn app_focuses_its_other_window_after_ours(load: f64) -> Run {
    let mut b = alpha_in_two_stages(1.0, |_| {});
    let (first, bravo) = (b.stage_of(17), b.stage_of(12));
    b.card_click(bravo);
    b.advance(4000);
    start(&mut b, load);
    b.card_click(first);
    b.advance(150);
    b.app_focuses(11);
    b.advance(8000);
    let out = told(&mut b);
    Run::of(&b, out)
}

/// The user's own focus by a key (`USER_FOCUS`: after a key, time is all there is): ⌘` in
/// Alpha, whose next window is in another stage; Alpha focuses it 30 ms after the key.
fn next_window_key_to_another_stage(load: f64) -> Run {
    let mut b = alpha_in_two_stages(load, |_| {});
    b.command(true);
    b.advance(40);
    b.key(crate::platform::input::KEY_GRAVE, false);
    b.advance(30);
    b.app_focuses(17);
    b.advance(60);
    b.command(false);
    b.advance(8000);
    let out = told(&mut b);
    Run::of(&b, out)
}

// ----- an activation of ours (`Causes::is_app_echo`, `stale_activation`) -------------------------

/// `engine/causes.rs`, `Causes::is_app_echo` (11-14, the live landing: "the fixture's activation
/// never came, Alpha's switch 1.8 s later was reported, and the user's activation of the
/// fixture 1.3 s after that was taken for the late echo"): a card click for Charlie's
/// stage, whose activation is never reported; one for Alpha's 1.8 s later; 1.3 s after that
/// macOS activates Charlie (an `open`, no press). Its stage comes.
fn awaited_activation_that_never_came(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, |d| {
        two_apps(d);
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_window(14, C, frame(4), D1);
        d.raise(12);
        d.apps.get_mut(&C).expect("Charlie").activates_ms = Some(10_000_000.0);
    });
    settle(&mut b);
    let (alpha, charlie) = (b.stage_of(11), b.stage_of(14));
    b.card_click(charlie);
    b.advance(1800);
    b.card_click(alpha);
    b.advance(1300);
    b.desk(|d| d.apps.get_mut(&C).expect("Charlie").activates_ms = None);
    b.activate(C);
    b.advance(20_000);
    let out = told(&mut b);
    Run::of(&b, out)
}

/// `engine/slow_task.rs` (after the 11-06 restarts: "app activations waited up to 0.46 s in
/// the window thread's queue (1.7 to 3.3 s on that morning's builds)"): a Dock click on
/// Alpha, whose stage is hidden, while the WM thread is held up `late_ms` from the release.
/// The activation is the user's however late it is handled: Alpha's stage comes.
fn activation_handled_late(load: f64, late_ms: u64) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, two_apps);
    settle(&mut b);
    b.dock_click(A, 90);
    b.busy(late_ms);
    b.advance(20_000);
    let out = told(&mut b);
    Run::of(&b, out)
}

fn activation_handled_half_a_second_late(load: f64) -> Run {
    activation_handled_late(load, 460)
}

fn activation_handled_three_seconds_late(load: f64) -> Run {
    activation_handled_late(load, 3300)
}

/// Two queues delivering a focus and an activation in either order (review 11-21 `core.md`
/// §5d 6; `controller/arrival.rs` `held_is_of`, R5 2): macOS activates Alpha, whose stage is
/// hidden (an `open`); `focus_first`: its window's focus notice comes before NSWorkspace's
/// activation notice.
fn activation_and_focus(load: f64, focus_first: bool) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, |d| {
        two_apps(d);
        d.apps.get_mut(&A).expect("Alpha").focus_before_activation = focus_first;
    });
    settle(&mut b);
    b.activate(A);
    b.advance(8000);
    let out = told(&mut b);
    Run::of(&b, out)
}

fn activation_then_its_focus(load: f64) -> Run {
    activation_and_focus(load, false)
}

fn focus_then_its_activation(load: f64) -> Run {
    activation_and_focus(load, true)
}

// ----- held for a Desktop (`engine/focus_replay.rs`, `engine/focus_elsewhere.rs`) ----------------

/// `engine/focus_replay.rs` (10-12: "the window's focus arrived ~40 ms before the Space
/// change notice and ~0.7 s before the Desktop was recognized ... and it was lost"): macOS
/// activates Charlie, whose only window is on Desktop 2, and slides there; the activation and
/// the focus are held and replayed once the Desktop is recognized. `focus_first`: the two
/// queues the other way round.
fn held_through_a_space_change(load: f64, focus_first: bool) -> Run {
    let mut b = both_desktops_known(EngineKind::Corner, load, |d| {
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_window(14, C, frame(4), D2);
        d.raise(13);
        d.raise(15);
        d.raise(11);
        d.raise(12);
        d.apps.get_mut(&C).expect("Charlie").focus_before_activation = focus_first;
    });
    b.activate(C);
    b.advance(8000);
    let out = told(&mut b);
    Run::of(&b, out)
}

fn held_activation_then_focus(load: f64) -> Run {
    held_through_a_space_change(load, false)
}

fn held_focus_then_activation(load: f64) -> Run {
    held_through_a_space_change(load, true)
}

/// `engine/focus_elsewhere.rs` (11-19 13:13: a link clicked in Mail; "Safari activated, the
/// switch to its stage here began ... and Safari opened the link in its window of Desktop 1
/// and focused it; macOS went there"): macOS activates Alpha, whose stage here is hidden;
/// 33 ms after the notice Alpha brings its window of Desktop 2 forward. macOS goes to
/// Desktop 2 and stays there.
fn link_opens_in_the_apps_window_elsewhere(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Corner, load, |_| {});
    b.activate(A);
    let notice = b.desk(|d| d.at_load(fake::delays::ACTIVATION).as_millis() as u64);
    b.advance(notice + 33);
    b.app_brings_forward(13);
    b.advance(10_000);
    let here = b.front_in(b.space_of(11).expect("window 11's Space"));
    let out = format!("{}; Desktop 1 keeps {here:?}", told(&mut b));
    Run::of(&b, out)
}

/// `engine/focus_elsewhere.rs`, `hidden_focus` (11-20 07:53:39, the Spaces engine: "a link
/// clicked in Mail on Desktop 1 opened in Safari's window hidden in a stage of Desktop 2;
/// Safari's activation switched to its work window's stage here, its focus on the other
/// window came 200 ms later and waited for a Space change that never came"): Alpha's window
/// here and its window on Desktop 2 both wait in hidden stages; macOS activates Alpha, and
/// 200 ms after the notice Alpha brings its window of Desktop 2 forward. Desktop 2 comes with
/// that window's stage in front; Desktop 1 is left as it was.
fn link_opens_in_a_window_hidden_elsewhere(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Spaces, DEFAULTS, 1.0, 2, |d| {
        d.add_app(A, "com.example.alpha", "Alpha");
        d.add_app(B, "com.example.bravo", "Bravo");
        d.add_window(13, A, frame(3), D2);
        d.add_window(15, B, frame(5), D2);
        d.add_window(11, A, frame(1), D1);
        d.add_window(12, B, frame(2), D1);
        d.front = Some(B);
    });
    b.go_to_desktop(D2, true);
    b.advance(4000);
    b.go_to_desktop(D1, true);
    b.advance(6000);
    let begins = (b.front(), b.desktop_shown(), b.front_app(), b.desktop_of(13), b.desktop_of(11));
    assert_eq!(begins, (Some(vec![12]), D1, Some(B), None, None), "the start:
{}", b.story());
    start(&mut b, load);
    b.activate(A);
    let notice = b.desk(|d| d.at_load(fake::delays::ACTIVATION).as_millis() as u64);
    b.advance(notice + 200);
    b.app_brings_forward(13);
    b.advance(12_000);
    let here = b.front_in(b.space_of(12).expect("window 12's Space"));
    let out = format!("{}; Desktop 1 keeps {here:?}", told(&mut b));
    Run::of(&b, out)
}

// ----- Finder's echo of a wallpaper press (`engine/desktop_echo.rs`) -----------------------------

/// `engine/desktop_echo.rs`: a wallpaper click, Finder's echo, and 2.5 s after the click a
/// Dock click on Finder: deliberate, its stage comes.
fn wallpaper_click_then_finder_from_the_dock(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, |d| {
        d.add_window(16, fake::FINDER_PID, frame(6), D1);
        two_apps(d);
    });
    settle(&mut b);
    b.click(Point::new(1400.0, 300.0));
    b.advance(2500);
    let echo = told(&mut b);
    b.dock_click(fake::FINDER_PID, 90);
    b.advance(15_000);
    let out = format!("after the click: {echo}; after the Dock click: {}", told(&mut b));
    Run::of(&b, out)
}

/// The echo with what a person sees and what each notice came to (the scenario
/// "a wallpaper click and Finder's echo" with its trail).
fn wallpaper_click_and_its_notices(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, |d| {
        d.add_window(16, fake::FINDER_PID, frame(6), D1);
        two_apps(d);
    });
    settle(&mut b);
    b.click(Point::new(1400.0, 300.0));
    b.advance(15_000);
    let out = told(&mut b);
    Run::of(&b, out)
}

// ----- macOS's own after a display change (`engine/displays.rs`) ---------------------------------

/// `controller.rs`, `macos_after_display_change` (11-05 20:25:46.8: the lid closed, macOS
/// activated Mail, "whose switch sent the laptop's front to a card"): a second screen comes
/// and goes; 3 s after, macOS activates Alpha, whose stage is hidden: no switch. 20 s after,
/// another activation of Alpha is the user's.
fn activation_after_a_display_change(load: f64) -> Run {
    let mut b = Bench::new(EngineKind::Corner, DEFAULTS, load, 1, |d| {
        two_apps(d);
        d.add_app(C, "com.example.charlie", "Charlie");
    });
    settle(&mut b);
    b.screen_comes();
    b.advance(2000);
    b.screen_goes();
    b.advance(3000);
    b.activate(A);
    b.advance(5000);
    let soon = told(&mut b);
    b.activate(C);
    b.advance(12_000);
    b.activate(A);
    b.advance(8000);
    let out = format!("3 s after: {soon}; 20 s after: {}", told(&mut b));
    Run::of(&b, out)
}

/// Every scenario of this file, for the load table.
pub(super) const ALL: &[Scenario] = &[
    ("a late focus notice of an app no longer in front", "controller.rs focus_may_switch, 11-02", late_focus_of_an_app_left_behind),
    ("a late focus notice of a window left behind, its app in front", "engine/stale_activation.rs focus_moved_on", late_focus_of_a_window_left_behind),
    ("the same from an app slow to answer", "engine/stale_activation.rs focus_moved_on, the 50 ms question", late_focus_of_a_slow_app),
    ("a late focus notice right after our hide of its window", "review 11-21 core.md 5d 3", late_focus_right_after_our_hide),
    ("a parked window's sliver clicked as the switch that parked it lands", "controller.rs, a parked window grabbed", sliver_click_right_after_its_park),
    ("a click on a window 300 ms after our switch showed it", "review 11-21 core.md 6 1", click_on_a_window_just_shown),
    ("a drag of a window 200 ms after our switch showed it", "review 11-21 core.md 5d 3", drag_right_after_a_show),
    ("an app focuses its other window after our focus", "review 11-21 core.md 5d 4", app_focuses_its_other_window_after_ours),
    ("Cmd-` to a window in another stage", "controller.rs USER_FOCUS", next_window_key_to_another_stage),
    ("an awaited activation that never came, then the user's", "engine/causes.rs is_app_echo, 11-14", awaited_activation_that_never_came),
    ("a Dock click's activation handled 0.46 s late", "engine/slow_task.rs, 11-06", activation_handled_half_a_second_late),
    ("a Dock click's activation handled 3.3 s late", "engine/slow_task.rs, 11-06", activation_handled_three_seconds_late),
    ("an activation, then its window's focus", "review 11-21 core.md 5d 6", activation_then_its_focus),
    ("a window's focus, then its app's activation", "review 11-21 core.md 5d 6", focus_then_its_activation),
    ("an activation and focus held through a Space change (Corner)", "engine/focus_replay.rs, 10-12", held_activation_then_focus),
    ("a focus and activation held through a Space change (Corner)", "engine/focus_replay.rs, 10-12", held_focus_then_activation),
    ("a link opens in the app's window on another Desktop (Corner)", "engine/focus_elsewhere.rs, 11-19", link_opens_in_the_apps_window_elsewhere),
    ("a link opens in the app's window hidden on another Desktop (Spaces)", "engine/focus_elsewhere.rs hidden_focus, 11-20", link_opens_in_a_window_hidden_elsewhere),
    ("a wallpaper click: what each notice came to", "engine/desktop_echo.rs, 11-09", wallpaper_click_and_its_notices),
    ("a wallpaper click, then Finder from the Dock", "engine/desktop_echo.rs", wallpaper_click_then_finder_from_the_dock),
    ("an activation after a display change", "controller.rs macos_after_display_change, 11-05", activation_after_a_display_change),
];

/// Asserts a scenario at load 1, 3 and 10: what a person sees (`saw`) and what each notice
/// came to (`trail`), as [`told`] joins them.
fn pin(script: fn(f64) -> Run, saw: [&str; 3], trail: [&str; 3]) {
    let want: Vec<String> = saw.iter().zip(trail).map(|(s, t)| format!("{s}; {t}")).collect();
    expect(script, [&want[0], &want[1], &want[2]]);
}

#[test]
fn a_late_focus_notice_is_ours_however_late() {
    // Since the cause ledger (round r4-causes) a focus is expected of the window we focused,
    // and its first notice after our request is ours by order at every load. Before: by
    // the 700 ms since anything touched the window, then by the gates after it.
    // Two apps: the late notice's app is no longer in front. (Before: ours by our hide of
    // the window at load 1, "its app is behind" at load 3 and 10.)
    let bravo = "front [12] shows [12] desktop 1 app 102 switches 2 slides 0";
    let ours = "activation 101: ours, activation 102: ours, focus 12: ours, focus 11: ours";
    pin(late_focus_of_an_app_left_behind, [bravo, bravo, bravo], [ours, ours, ours]);

    // One app, two stages: its app is in front. (Before: the app was asked where its focus
    // is now, "stale"; and the second focus's own echo was taken for the user's under load.)
    let first = "front [11] shows [11] desktop 1 app 101 switches 2 slides 0";
    let ours = "focus 17: ours, focus 11: ours";
    pin(late_focus_of_a_window_left_behind, [first, first, first], [ours, ours, ours]);
    // A focus right after a hide of ours on the same window. (Before: ours by the hide.)
    pin(late_focus_right_after_our_hide, [first, first, first], [ours, ours, ours]);
    // An app slow to answer. (Before, TIME DECIDED: the 50 ms the question "where is your
    // focus now" may take; at load 10 the app answered past it, the late notice was taken
    // for the user's and the stage left came back, three switches for two clicks.)
    pin(late_focus_of_a_slow_app, [first, first, first], [ours, ours, ours]);
}

#[test]
fn the_users_own_act_right_after_ours() {
    // The click on the sliver of a window parked 0.6 s before: the user's (a hide of ours
    // expects no focus), and its stage comes at the release. Before the cause ledger its
    // focus was taken for the echo of our hide at every load and nothing came: the bug.
    let comes = "front [11] shows [11] desktop 1 app 101 switches 2 slides 0";
    let grabbed = "focus 17: ours, focus 11: a parked window grabbed";
    // At load 10 the focus is told after the button is up: the same stage, by the focus.
    let after = "focus 17: ours, focus 11: the user's: its stage comes";
    pin(sliver_click_right_after_its_park, [comes, comes, comes], [grabbed, grabbed, after]);
    // The click on a window shown 300 ms before: the user's (a press on the window itself;
    // before: the echo of our show). Its stage is in front already.
    let both = "front [12, 18] shows [12, 18] desktop 1 app 102 switches 1 slides 0";
    let users = "activation 102: ours, focus 12: ours, focus 18: the user's: nothing to switch";
    pin(click_on_a_window_just_shown, [both, both, both], [users, users, users]);
    // The drag begun 200 ms after the show is a drag from its first move (a move under the
    // user's hand that leaves the window elsewhere than we put it; before: none until
    // 700 ms after the show).
    let drag = "a drag 220 ms in: true, at its end: true; the frame kept moved by Some((240.0, 120.0)); front [12] shows [12] desktop 1 app 102 switches 1 slides 0; activation 102: ours, focus 12: ours";
    expect(drag_right_after_a_show, [drag, drag, drag]);
}

#[test]
fn an_apps_own_focus_of_another_window_brings_its_stage() {
    // The card clicked was window 17's: the app's own focus of 11, 150 ms later, brings 11's
    // stage back (an app that brings a window forward shows its stage, as a link opened in
    // it does).
    let other = "front [11] shows [11] desktop 1 app 101 switches 2 slides 0";
    let at1 = "activation 101: ours, focus 17: ours, focus 17: ours, focus 11: the user's: its stage comes";
    let at3 = "activation 101: ours, focus 17: ours, focus 11: the user's: its stage comes, focus 17: ours";
    // At load 10 the app's focus comes before its activation is in force (its app is
    // behind), and again once it is.
    let at10 = "focus 11: its app is behind, activation 101: ours, focus 17: ours, focus 11: the user's: its stage comes";
    pin(app_focuses_its_other_window_after_ours, [other, other, other], [at1, at3, at10]);
    let next = "front [17] shows [17] desktop 1 app 101 switches 1 slides 0";
    let users = "focus 17: the user's: its stage comes";
    pin(next_window_key_to_another_stage, [next, next, next], [users, users, users]);
}

#[test]
fn an_activation_is_ours_or_the_users() {
    let charlie = "front [14] shows [14] desktop 1 app 103 switches 3 slides 0";
    let trail = "activation 101: ours, focus 11: ours, activation 103: the user's: its stage comes, focus 14: ours";
    pin(awaited_activation_that_never_came, [charlie, charlie, charlie], [trail, trail, trail]);
    let alpha = "front [11] shows [11] desktop 1 app 101 switches 1 slides 0";
    let users = "activation 101: the user's: its stage comes, focus 11: ours";
    pin(activation_handled_half_a_second_late, [alpha, alpha, alpha], [users, users, users]);
    pin(activation_handled_three_seconds_late, [alpha, alpha, alpha], [users, users, users]);
    // The two queues, either order: whichever comes first switches, the other is our echo.
    pin(activation_then_its_focus, [alpha, alpha, alpha], [users, users, users]);
    let focus_first = "focus 11: the user's: its stage comes, activation 101: ours";
    pin(focus_then_its_activation, [alpha, alpha, alpha], [focus_first, focus_first, focus_first]);
}

#[test]
fn held_for_a_desktop() {
    let there = "front [14] shows [14] desktop 2 app 103 switches 0 slides 1";
    let held = "activation 103: held for a slide, focus 14: held for its Desktop";
    pin(held_activation_then_focus, [there, there, there], [held, held, held]);
    let held = "focus 14: held for its Desktop, activation 103: held for a slide";
    pin(held_focus_then_activation, [there, there, there], [held, held, held]);
    // The app's own focus elsewhere mid-switch: macOS goes there and stays; the switch here
    // landed without its keyboard.
    let link = "front [13] shows [13] desktop 2 app 101 switches 1 slides 1; activation 101: the user's: its stage comes, focus 11: ours, focus 13: held for its Desktop; Desktop 1 keeps Some([11])";
    expect(link_opens_in_the_apps_window_elsewhere, [link, link, link]);
    // Under the Spaces engine the window waits off every Desktop: its Desktop is arranged
    // and macOS taken there; the activation's switch here is undone unseen.
    let hidden = "front [13] shows [13] desktop 2 app 101 switches 1 slides 1; activation 101: the user's: its stage comes, focus 11: ours, focus 13: held for its Desktop; Desktop 1 keeps Some([12])";
    expect(link_opens_in_a_window_hidden_elsewhere, [hidden, hidden, hidden]);
}

#[test]
fn finders_echo_of_a_wallpaper_press() {
    let ignored = "front [12] shows [12] desktop 1 app 91 switches 0 slides 0";
    let taken = "front [16] shows [16] desktop 1 app 91 switches 1 slides 0";
    let echo = "activation 91: the wallpaper press's echo, focus 16: the wallpaper press's echo";
    let users = "activation 91: the user's: its stage comes, focus 16: ours";
    // TIME DECIDES: WALLPAPER_ECHO (1 s after the release): Finder's activation 3 s after
    // the press is taken for a deliberate one.
    pin(wallpaper_click_and_its_notices, [ignored, taken, taken], [echo, users, users]);
    // A Dock click on Finder 2.5 s after the wallpaper click is deliberate at every load.
    // TIME DECIDES: at load 3 and 10 what brings Finder's stage is the wallpaper's own late
    // echo, come after the Dock press.
    let at1 = "after the click: front [12] shows [12] desktop 1 app 91 switches 0 slides 0; activation 91: the wallpaper press's echo, focus 16: the wallpaper press's echo; after the Dock click: front [16] shows [16] desktop 1 app 91 switches 1 slides 0; activation 91: the wallpaper press's echo, focus 16: the wallpaper press's echo, activation 91: the user's: its stage comes, focus 16: ours";
    let at3 = "after the click: front [12] shows [12] desktop 1 app 102 switches 0 slides 0; ; after the Dock click: front [16] shows [16] desktop 1 app 91 switches 1 slides 0; activation 91: the user's: its stage comes, focus 16: ours, activation 91: ours, focus 16: ours";
    let at10 = "after the click: front [12] shows [12] desktop 1 app 102 switches 0 slides 0; ; after the Dock click: front [16] shows [16] desktop 1 app 91 switches 1 slides 0; activation 91: the user's: its stage comes, focus 16: ours, activation 91: the user's: nothing to switch, focus 16: the user's: nothing to switch";
    expect(wallpaper_click_then_finder_from_the_dock, [at1, at3, at10]);
}

#[test]
fn macos_own_activation_after_a_display_change() {
    // TIME DECIDES, as meant: DISPLAY_SETTLE (10 s, with no click or key since) is all that
    // tells macOS's own activation from the user's.
    let want = "3 s after: front [12] shows [12] desktop 1 app 101 switches 0 slides 0; activation 101: macOS's after a display change, focus 11: macOS's after a display change; 20 s after: front [11] shows [11] desktop 1 app 101 switches 1 slides 0; activation 101: macOS's after a display change, focus 11: macOS's after a display change, activation 103: macOS's after a display change, activation 101: the user's: its stage comes, focus 11: ours";
    expect(activation_after_a_display_change, [want, want, want]);
}

/// The login's settling (`engine/restore.rs`; the scenario "the restore after a restart"):
/// while the restore window is open an app's activation and focus are its own.
#[test]
fn the_apps_own_while_the_login_settles() {
    let settles = "activation 101: the app's own while the login settles, focus 21: the app's own while the login settles, activation 103: the app's own while the login settles, focus 24: the app's own while the login settles, activation 102: the app's own while the login settles, focus 22: the app's own while the login settles, focus 27: the app's own while the login settles";
    // TIME DECIDES: the restore window's own end (`engine/restore.rs`, its quiet times): at
    // load 10 the last window's focus comes after it, the user's with nothing to switch.
    let slow = "activation 101: the app's own while the login settles, activation 103: the app's own while the login settles, activation 102: the app's own while the login settles, focus 21: its app is behind, focus 24: its app is behind, focus 22: the app's own while the login settles, focus 27: the user's: nothing to switch";
    for (load, want) in super::scenarios::LOADS.iter().zip([settles, settles, slow]) {
        assert_eq!(super::scenarios::restore_after_a_restart(*load).trail.join(", "), want, "at load {load}");
    }
}

/// Every scenario of this file at each load, printed (what to assert after a change):
/// `BENCH_PRINT=1 cargo test --lib causes::print_them -- --nocapture`. Nothing without
/// `BENCH_PRINT`.
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
    for load in super::scenarios::LOADS {
        println!("the restore after a restart @{load}: {}", super::scenarios::restore_after_a_restart(load).trail.join(", "));
    }
}
