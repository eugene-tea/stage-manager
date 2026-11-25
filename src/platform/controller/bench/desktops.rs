//! Which Desktop arrived and which Desktop a window is on (design r3, slices 2 and 3): the
//! window server's listing against the window list, in the orders macOS was measured to tell
//! them (`probe-system flip`, 11-21) and the ones a loaded Mac gives.
//!
//! Each outcome reads what a person sees ([`seen`]) and what the shadow counted
//! ([`Bench::disagreed`]: the model against the window server's listing, through the
//! bindings). Where a load changes an outcome the assertion is marked `// TIME DECIDES:`.

use super::scenarios::{A, B, C, D1, D2, LOADS, Run, Scenario, both_desktops_known, expect, seen, two_apps};
use super::*;

/// Time moves on, 10 ms at a time, until `done` (`max_ms` at most): how long it took.
fn until(b: &mut Bench, max_ms: u64, done: impl Fn(&mut Bench) -> bool) -> Option<u64> {
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

/// How long after a Desktop key, in words (the exact ms vary with the load's costs; a slide
/// takes 0.35 s).
fn after_the_key(ms: Option<u64>) -> &'static str {
    match ms {
        None => "never",
        Some(0..=450) => "by the slide's end",
        Some(451..=1000) => "within 1 s",
        Some(1001..=2000) => "within 2 s",
        Some(_) => "later",
    }
}

/// What the shadow counted since `base`: disagreements begun on the Desktop shown (and
/// those still open), and on a hidden window's Desktop.
fn shadow(b: &Bench, base: (u64, u64, u64)) -> String {
    let (shown, homes, open) = b.disagreed();
    format!("shadow {} shown ({open} open) {} homes", shown - base.0, homes - base.1)
}

/// How the listing was read since `base` ([`Bench::keyed`]): the notices that waited for the
/// flip, the changes a look took before their notice, the notices that came after one.
fn order(b: &Bench, base: (u64, u64, u64)) -> String {
    let (waited, early, late) = b.keyed();
    format!("waited for the flip {}, taken before the notice {}, notices after {}", waited - base.0, early - base.1, late - base.2)
}

// ----- the notice and the flip, in both orders ---------------------------------------------------

/// ⌃→ to Desktop 2 and ⌃← back, both Desktops known (`engine/slide_hold.rs`, 10-23).
/// `notice_first`: the Space change's notice is sent 5 ms before the listing's "Current
/// Space" flips, as measured 11-21 (24 of 24); else the flip comes first, with the leaving
/// Desktop's windows going from the list.
/// `server`: the window server tells its own notice of the change's end (1401), with the flip.
fn desktop_keys(engine: EngineKind, load: f64, notice_first: bool, server: bool) -> Run {
    let mut b = both_desktops_known(engine, load, |d| d.server_notices = server);
    let (base, read) = (b.disagreed(), b.keyed());
    b.desk(|d| d.flip_leads_notice = !notice_first);
    b.go_to_desktop(D2, true);
    b.advance(4000);
    let there = seen(&mut b);
    b.go_to_desktop(D1, true);
    b.advance(6000);
    let back = seen(&mut b);
    let out = format!("there: {there}; back: {back}; {}; {}", order(&b, read), shadow(&b, base));
    Run::of(&b, out)
}

fn desktop_keys_notice_first(load: f64) -> Run {
    desktop_keys(EngineKind::Spaces, load, true, false)
}

fn desktop_keys_notice_first_corner(load: f64) -> Run {
    desktop_keys(EngineKind::Corner, load, true, false)
}

fn desktop_keys_flip_first(load: f64) -> Run {
    desktop_keys(EngineKind::Spaces, load, false, false)
}

fn desktop_keys_flip_first_corner(load: f64) -> Run {
    desktop_keys(EngineKind::Corner, load, false, false)
}

fn desktop_keys_server_tells(load: f64) -> Run {
    desktop_keys(EngineKind::Spaces, load, true, true)
}

/// A Desktop change whose notice never comes (`controller.rs`, `check_spaces`: "a Space
/// change went unseen"): ⌃→ to Desktop 2, `activeSpaceDidChangeNotification` lost.
/// `server`: the window server tells its own notice of the change's end (1401).
fn flip_with_no_notice(engine: EngineKind, load: f64, server: bool) -> Run {
    let mut b = both_desktops_known(engine, load, |d| d.server_notices = server);
    let base = b.disagreed();
    b.go_to_desktop(D2, true);
    b.desk(|d| d.drop_queued(|n| matches!(n, Notice::SpaceChanged)));
    let took = until(&mut b, 8000, |b| b.space_shown() == b.space_of(13));
    b.advance(4000);
    let out = format!("Desktop 2 recognized {}; {}; {}", after_the_key(took), seen(&mut b), shadow(&b, base));
    Run::of(&b, out)
}

fn flip_with_no_notice_server_tells(load: f64) -> Run {
    flip_with_no_notice(EngineKind::Spaces, load, true)
}

fn flip_with_no_notice_nothing_tells(load: f64) -> Run {
    flip_with_no_notice(EngineKind::Spaces, load, false)
}

fn flip_with_no_notice_corner(load: f64) -> Run {
    flip_with_no_notice(EngineKind::Corner, load, false)
}

/// A Space change's notice with no change (another screen's Desktop that is not managed, a
/// slide given up): nothing moves, and the stages are not held for long.
fn notice_with_no_flip_on(engine: EngineKind, load: f64) -> Run {
    let mut b = both_desktops_known(engine, load, |_| {});
    let base = b.disagreed();
    b.desk(|d| d.post(0.0, Notice::SpaceChanged));
    b.advance(1);
    let held = match until(&mut b, 5000, |b| !b.changing()) {
        None => "never",
        Some(0..=100) => "at once",
        Some(101..=500) => "within 0.5 s",
        Some(_) => "later",
    };
    b.advance(4000);
    let out = format!("settled {held}; {}; {}", seen(&mut b), shadow(&b, base));
    Run::of(&b, out)
}

fn notice_with_no_flip(load: f64) -> Run {
    notice_with_no_flip_on(EngineKind::Spaces, load)
}

fn notice_with_no_flip_corner(load: f64) -> Run {
    notice_with_no_flip_on(EngineKind::Corner, load)
}

/// The window list read at the arrival shows both Desktops: the leaving Desktop's windows
/// stay listed 250 ms past the flip (`controller.rs`, `arrival_waits`: "the list shows two
/// Desktops at once (one still sliding)").
fn both_desktops_listed_at_the_arrival_on(engine: EngineKind, load: f64) -> Run {
    let mut b = both_desktops_known(engine, load, |_| {});
    let base = b.disagreed();
    b.desk(|d| d.leaving_lingers_ms = 250.0);
    b.go_to_desktop(D2, true);
    let took = until(&mut b, 8000, |b| b.space_shown() == b.space_of(13));
    b.advance(4000);
    let at = seen(&mut b);
    b.go_to_desktop(D1, true);
    b.advance(6000);
    let out = format!("Desktop 2 recognized {}; there: {at}; back: {}; {}", after_the_key(took), seen(&mut b), shadow(&b, base));
    Run::of(&b, out)
}

fn both_desktops_listed_at_the_arrival(load: f64) -> Run {
    both_desktops_listed_at_the_arrival_on(EngineKind::Spaces, load)
}

fn both_desktops_listed_at_the_arrival_corner(load: f64) -> Run {
    both_desktops_listed_at_the_arrival_on(EngineKind::Corner, load)
}

/// The listing is not readable while Desktop 2 arrives (no "Current Space" in it), and is
/// again after: the arrival goes by the windows, and the next good read agrees or corrects.
fn listing_unreadable_for_one_arrival_on(engine: EngineKind, load: f64) -> Run {
    let mut b = both_desktops_known(engine, load, |_| {});
    let base = b.disagreed();
    b.desk(|d| d.listing_unreadable = true);
    b.go_to_desktop(D2, true);
    let took = until(&mut b, 8000, |b| b.space_shown() == b.space_of(13));
    b.desk(|d| d.listing_unreadable = false);
    b.advance(4000);
    let at = seen(&mut b);
    b.go_to_desktop(D1, true);
    b.advance(6000);
    let out = format!("Desktop 2 recognized {}; there: {at}; back: {}; {}", after_the_key(took), seen(&mut b), shadow(&b, base));
    Run::of(&b, out)
}

fn listing_unreadable_for_one_arrival(load: f64) -> Run {
    listing_unreadable_for_one_arrival_on(EngineKind::Spaces, load)
}

fn listing_unreadable_for_one_arrival_corner(load: f64) -> Run {
    listing_unreadable_for_one_arrival_on(EngineKind::Corner, load)
}

// ----- a Desktop never seen ------------------------------------------------------------------------

/// The harsher shape of "the first visit to a Desktop by its key" (the bench's author,
/// 11-21): the Desktop never visited has Charlie in front, an app the model knows no window
/// of. macOS activates Charlie as the Desktop slides in; its window, found then, must not be
/// placed on the Desktop being left.
fn first_visit_to_an_unknown_apps_desktop_on(engine: EngineKind, load: f64) -> Run {
    let mut b = Bench::new(engine, DEFAULTS, load, 2, |d| {
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_window(14, C, frame(4), D2);
        two_apps(d);
    });
    let home = b.space_of(12);
    b.go_to_desktop(D2, true);
    b.advance(6000);
    let there = seen(&mut b);
    let placed = if b.space_of(14) == home { "the Desktop left" } else { "its own Desktop" };
    b.go_to_desktop(D1, true);
    b.advance(6000);
    let out = format!("there: {there}; Charlie's window placed on {placed}; back: {}; {}", seen(&mut b), shadow(&b, (0, 0, 0)));
    Run::of(&b, out)
}

fn first_visit_to_an_unknown_apps_desktop(load: f64) -> Run {
    first_visit_to_an_unknown_apps_desktop_on(EngineKind::Spaces, load)
}

fn first_visit_to_an_unknown_apps_desktop_corner(load: f64) -> Run {
    first_visit_to_an_unknown_apps_desktop_on(EngineKind::Corner, load)
}

// ----- membership ------------------------------------------------------------------------------------

/// A window moved to another Desktop by hand (Mission Control's drag; `controller.rs`,
/// `check_spaces`: "the windows were moved to another Desktop by hand"): Bravo's window, in
/// front on Desktop 1, is put on Desktop 2. It leaves its stage here; on Desktop 2 it joins
/// Bravo's stage there.
fn window_moved_to_another_desktop(engine: EngineKind, load: f64, server: bool) -> Run {
    let mut b = both_desktops_known(engine, load, |d| d.server_notices = server);
    let base = b.disagreed();
    let home = b.space_of(12);
    b.desk(|d| d.space_moves(12, D2));
    let left = match until(&mut b, 9000, |b| b.space_of(12) != home) {
        None => "never",
        Some(0..=100) => "at once",
        Some(101..=2500) => "within 2.5 s",
        Some(2501..=3500) => "within 3.5 s",
        Some(_) => "later",
    };
    b.advance(4000);
    let here = seen(&mut b);
    b.go_to_desktop(D2, true);
    b.advance(6000);
    let out = format!("left its stage {left}; here: {here}; there: stages {:?} {}; {}", b.stages(), seen(&mut b), shadow(&b, base));
    Run::of(&b, out)
}

fn window_moved_server_tells(load: f64) -> Run {
    window_moved_to_another_desktop(EngineKind::Spaces, load, true)
}

fn window_moved_nothing_tells(load: f64) -> Run {
    window_moved_to_another_desktop(EngineKind::Spaces, load, false)
}

fn window_moved_corner(load: f64) -> Run {
    window_moved_to_another_desktop(EngineKind::Corner, load, false)
}

/// A window on every Desktop ("Assign To: All Desktops"; `engine/model/spaces.rs`,
/// `STICKY_HOPS`): Charlie's window, in front on Desktop 1, is listed on both Desktops. Each
/// Desktop key: whether the model counts it on every Desktop, and the stages of the Desktop
/// shown.
fn window_on_every_desktop_on(engine: EngineKind, load: f64) -> Run {
    let mut b = Bench::new(engine, DEFAULTS, load, 2, |d| {
        d.add_app(A, "com.example.alpha", "Alpha");
        d.add_app(B, "com.example.bravo", "Bravo");
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_window(13, A, frame(3), D2);
        d.add_window(15, B, frame(5), D2);
        d.add_window(11, A, frame(1), D1);
        d.add_window(14, C, frame(4), D1);
        d.on_every_desktop(14);
        d.front = Some(C);
    });
    let mut hops = Vec::new();
    for to in [D2, D1, D2, D1] {
        b.go_to_desktop(to, true);
        b.advance(6000);
        let shows: Vec<u32> = b.on_screen().into_iter().collect();
        hops.push(format!("{to}: {} stages {:?} shows {shows:?}", if b.sticky(14) { "every" } else { "one" }, b.stages()));
    }
    let out = format!("{}; {}", hops.join("; "), shadow(&b, (0, 0, 0)));
    Run::of(&b, out)
}

fn window_on_every_desktop(load: f64) -> Run {
    window_on_every_desktop_on(EngineKind::Spaces, load)
}

fn window_on_every_desktop_corner(load: f64) -> Run {
    window_on_every_desktop_on(EngineKind::Corner, load)
}

/// A hidden window moved to another Desktop by hand, under the Corner engine: Alpha's window
/// on Desktop 1, in the park corner, is on its Desktop for Mission Control, which shows it
/// and lets it be dragged to Desktop 2 (UX-SPEC, "Known gaps": "in the default mode it
/// moves"). It leaves its stage here; on Desktop 2 it joins Alpha's stage there.
fn hidden_window_moved_corner(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Corner, load, |_| {});
    let base = b.disagreed();
    let home = b.space_of(11);
    assert!(b.hidden().contains(&11), "the start: Alpha's window on Desktop 1 is hidden:\n{}", b.story());
    b.desk(|d| d.space_moves(11, D2));
    let left = match until(&mut b, 9000, |b| b.space_of(11) != home) {
        None => "never",
        Some(0..=2500) => "within 2.5 s",
        Some(2501..=3500) => "within 3.5 s",
        Some(_) => "later",
    };
    b.advance(4000);
    let here = format!("stages {:?} {}", b.stages(), seen(&mut b));
    b.go_to_desktop(D2, true);
    b.advance(6000);
    let out = format!("left its stage {left}; here: {here}; there: stages {:?} {}; {}", b.stages(), seen(&mut b), shadow(&b, base));
    Run::of(&b, out)
}

// ----- the reads' own check --------------------------------------------------------------------------

/// The listing is there and wrong (never measured: what a macOS update might do): its
/// "Current Space" stays Desktop 1 while the person goes to Desktop 2 by its key and stays.
/// The reads disagree with the windows on screen at three looks and rest; Desktop 2 is then
/// recognized by its windows, as before the reads. Ten minutes on the listing is right
/// again: it decides again once a look agreed.
fn listing_stuck_corner(load: f64) -> Run {
    let mut b = both_desktops_known(EngineKind::Corner, load, |_| {});
    let reads = |b: &Bench| b.c.desks_json()["reads"]["state"].as_str().unwrap_or("none").to_string();
    let start = reads(&b);
    b.desk(|d| d.listing_stuck = true);
    b.go_to_desktop(D2, true);
    let took = until(&mut b, 20_000, |b| b.space_shown() == b.space_of(13));
    let rested = reads(&b);
    b.advance(4000);
    let there = seen(&mut b);
    b.desk(|d| {
        d.listing_stuck = false;
        d.screens[0].current = D2;
    });
    b.advance(crate::engine::desk_reads::OFF_FOR.as_millis() as u64 + 5000);
    let after = reads(&b);
    b.go_to_desktop(D1, true);
    b.advance(6000);
    let recognized = match took {
        None => "never",
        Some(0..=1000) => "within 1 s",
        Some(1001..=6000) => "within 6 s",
        Some(6001..=12_000) => "within 12 s",
        Some(_) => "later",
    };
    let out = format!("reads {start}; Desktop 2 recognized {recognized}, reads {rested}; there: {there}; ten minutes on reads {after}; back: {}", seen(&mut b));
    Run::of(&b, out)
}

// ----- full screen -----------------------------------------------------------------------------------

/// A window into full screen and out (`engine/fullscreen.rs`, measured 10-12: "new synthetic
/// Space ids minted on every toggle, the Desktop's id lost, its stages merged"): Bravo's
/// window, in front, takes its green button twice. Inside: a full-screen app's Space (no
/// strip). After: the Desktop it left, its stages as they were.
fn full_screen_in_and_out_on(engine: EngineKind, load: f64) -> Run {
    let mut b = Bench::new(engine, DEFAULTS, load, 1, two_apps);
    b.advance(3000);
    let (space, stages) = (b.space_shown(), b.stages());
    b.fullscreen(12, true);
    let came = match until(&mut b, 8000, |b| b.in_fullscreen() && !b.changing()) {
        None => "never",
        Some(0..=1000) => "within 1 s",
        Some(_) => "later",
    };
    b.advance(5000);
    let inside = b.in_fullscreen();
    b.fullscreen(12, false);
    let left = match until(&mut b, 8000, |b| !b.in_fullscreen() && !b.changing()) {
        None => "never",
        Some(0..=1000) => "within 1 s",
        Some(_) => "later",
    };
    b.advance(6000);
    let kept = b.space_shown() == space && b.stages() == stages;
    let out = format!("full screen seen {came}, still {inside}; the Desktop back {left}, its stages kept {kept}; {}; {}", seen(&mut b), shadow(&b, (0, 0, 0)));
    Run::of(&b, out)
}

fn full_screen_in_and_out(load: f64) -> Run {
    full_screen_in_and_out_on(EngineKind::Spaces, load)
}

fn full_screen_in_and_out_corner(load: f64) -> Run {
    full_screen_in_and_out_on(EngineKind::Corner, load)
}

// ----- two screens -----------------------------------------------------------------------------------

/// A second screen with two Desktops of its own; its Desktop changes (no notice names the
/// screen whose Desktop changed: `controller.rs`, `arrive_others`). Only that screen
/// arrives; the main screen's Desktop, its stages and its front stay as they are.
fn another_screens_desktop_changes_on(engine: EngineKind, load: f64) -> Run {
    let far = |n: u32| Rect::new(1600.0 + 40.0 * f64::from(n), 120.0 + 30.0 * f64::from(n), 700.0, 450.0);
    let mut b = Bench::new(engine, DEFAULTS, 1.0, 1, |d| {
        d.screen_comes_with(vec![50, 51]);
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_window(14, C, far(1), 50);
        d.add_window(16, C, far(2), 51);
        two_apps(d);
    });
    b.advance(4000);
    // The second screen's other Desktop seen once, and back: both known.
    for to in [51, 50] {
        b.desk(|d| d.slide(1, to));
        b.advance(5000);
    }
    let (main, second) = (b.space_of(12), b.space_of(14));
    let start = (b.front(), b.space_of(16).is_some() && b.space_of(16) != second, main != second);
    assert_eq!(start, (Some(vec![12]), true, true), "the start:\n{}", b.story());
    let arrivals = b.c.desks_json()["keyed"]["arrivals"].as_u64().unwrap_or(0);
    b.log.clear();
    b.desk(|d| {
        d.strip.transitions = 0;
        d.calls.clear();
        d.load = load;
        d.slide(1, 51);
    });
    b.advance(6000);
    let arrived = b.c.desks_json()["keyed"]["arrivals"].as_u64().unwrap_or(0) - arrivals;
    let there = b.c.model.as_ref().map(|m| m.current_space(crate::engine::model::DisplayId(2)));
    let out = format!(
        "the second screen shows window 16's Desktop {}; arrivals {arrived}; the main screen's Desktop kept {}; {}; {}",
        there == b.space_of(16),
        b.space_shown() == main,
        seen(&mut b),
        shadow(&b, (0, 0, 0))
    );
    Run::of(&b, out)
}

fn another_screens_desktop_changes(load: f64) -> Run {
    another_screens_desktop_changes_on(EngineKind::Spaces, load)
}

fn another_screens_desktop_changes_corner(load: f64) -> Run {
    another_screens_desktop_changes_on(EngineKind::Corner, load)
}

#[test]
fn another_screens_desktop() {
    // Window 16 shows on the second screen; the main screen's front is as it was.
    let only = "the second screen shows window 16's Desktop true; arrivals 1; the main screen's Desktop kept true; front [12] shows [12, 16] desktop 1 app 102 switches 0 slides 1; shadow 0 shown (0 open) 0 homes";
    expect(another_screens_desktop_changes, [only, only, only]);
}

// ----- the outcomes: the listing decides ------------------------------------------------------------

#[test]
fn desktop_keys_in_both_orders_of_notice_and_flip() {
    let keys = "there: front [13] shows [13] desktop 2 app 101 switches 0 slides 1; back: front [12] shows [12] desktop 1 app 102 switches 0 slides 2";
    let agreed = "shadow 0 shown (0 open) 0 homes";
    // The same at every load, whichever comes first; only how the listing was read differs.
    let read_at_once = format!("{keys}; waited for the flip 0, taken before the notice 0, notices after 0; {agreed}");
    expect(desktop_keys_flip_first, [&read_at_once, &read_at_once, &read_at_once]);
    // On the Mac as measured each notice finds the listing not flipped yet and waits for it
    // on the fast tick (before: the Desktop left was bound to the Space arriving and its
    // hidden windows counted present, 3 disagreements). Under load the notice is handled
    // after the flip.
    let waited = format!("{keys}; waited for the flip 2, taken before the notice 0, notices after 0; {agreed}");
    expect(desktop_keys_notice_first, [&waited, &read_at_once, &read_at_once]);
    // With the window server's own notice at the change's end, a loaded Mac takes each
    // change at that look, before NSWorkspace's notice, which then does nothing more.
    let early = format!("{keys}; waited for the flip 0, taken before the notice 2, notices after 2; {agreed}");
    expect(desktop_keys_server_tells, [&waited, &early, &early]);
}

#[test]
fn a_flip_with_no_notice_and_a_notice_with_no_flip() {
    // Taken at the look the window server's notice brings forward (before: FOREIGN_SETTLE,
    // two looks a second apart).
    let told = "Desktop 2 recognized by the slide's end; front [13] shows [13] desktop 2 app 101 switches 0 slides 0; shadow 0 shown (0 open) 0 homes";
    expect(flip_with_no_notice_server_tells, [told, told, told]);
    // With nothing telling, at the tick's next look (SPACE_POLL). Before: the activation
    // of Desktop 2's front app, held for the slide, was decided for the Desktop left and
    // macOS slid back there.
    let ticked = "Desktop 2 recognized within 1 s; front [13] shows [13] desktop 2 app 101 switches 0 slides 0; shadow 0 shown (0 open) 0 homes";
    expect(flip_with_no_notice_nothing_tells, [ticked, ticked, ticked]);
    // FLIP_WAIT (a cap, 300 ms): the listing never names another Space, nothing arrives.
    let nothing = "settled within 0.5 s; front [12] shows [12] desktop 1 app 102 switches 0 slides 1; shadow 0 shown (0 open) 0 homes";
    expect(notice_with_no_flip, [nothing, nothing, nothing]);
}

#[test]
fn both_desktops_listed_and_a_listing_not_readable() {
    let keys = "there: front [13] shows [13] desktop 2 app 101 switches 0 slides 1; back: front [12] shows [12] desktop 1 app 102 switches 0 slides 2; shadow 0 shown (0 open) 0 homes";
    let at_the_end = format!("Desktop 2 recognized by the slide's end; {keys}");
    // At load 10 the notice itself is handled 0.2 s after the slide.
    let late = format!("Desktop 2 recognized within 1 s; {keys}");
    // Each window is asked for its Desktop: the Desktop left, still listed, is left out
    // (before: MIXED_WAIT, then its windows taken for moved and more slides, at every load).
    expect(both_desktops_listed_at_the_arrival, [&at_the_end, &at_the_end, &late]);
    // The arrival goes by the windows (a new Space: the window engine cannot count the
    // Desktop's hidden windows present), and the next good read finds that Space bound to
    // no Desktop: the Desktop arrives again, by the listing. The way back is one slide.
    expect(listing_unreadable_for_one_arrival, [&at_the_end, &at_the_end, &late]);
}

#[test]
fn the_first_visit_to_an_unknown_apps_desktop() {
    // Charlie's window, found as Desktop 2 slides in, is on Desktop 2 by the window server's
    // word: it waits for that Desktop's arrival (before: placed on the Desktop being left,
    // its stage made front there; back on Desktop 1 an empty stage was in front).
    let right = "there: front [14] shows [14] desktop 2 app 103 switches 0 slides 1; Charlie's window placed on its own Desktop; back: front [12] shows [12] desktop 1 app 102 switches 0 slides 2; shadow 0 shown (0 open) 0 homes";
    expect(first_visit_to_an_unknown_apps_desktop, [right, right, right]);
}

#[test]
fn a_window_moved_and_a_window_on_every_desktop() {
    let moved = "here: front [] shows [] desktop 1 app 102 switches 0 slides 0; there: stages [[12, 15], [13]] front [12, 15] shows [12, 15] desktop 2 app 102 switches 0 slides 1; shadow 0 shown (0 open) 0 homes";
    // One read of its Desktop at the look its membership notice brings forward.
    let at_once = format!("left its stage at once; {moved}");
    expect(window_moved_server_tells, [&at_once, &at_once, &at_once]);
    // Or at the tick's next look (at rest, 2 s apart): one look, where SPACE_CHECKS took two.
    let ticked = format!("left its stage within 2.5 s; {moved}");
    expect(window_moved_nothing_tells, [&ticked, &ticked, &ticked]);
    // Listed on both Desktops: on every Desktop from the first arrival that sees it, never
    // taken for moved (before: STICKY_HOPS, a stage of its own on each Desktop in turn for
    // two arrivals).
    let every = "2: every stages [[13], [15]] shows [14, 15]; 1: every stages [[11]] shows [14]; 2: every stages [[13], [15]] shows [14, 15]; 1: every stages [[11]] shows [14]; shadow 0 shown (0 open) 0 homes";
    expect(window_on_every_desktop, [every, every, every]);
}

#[test]
fn full_screen() {
    let want = "full screen seen within 1 s, still true; the Desktop back within 1 s, its stages kept true; front [12] shows [12] desktop 1 app 102 switches 0 slides 2; shadow 0 shown (0 open) 0 homes";
    expect(full_screen_in_and_out, [want, want, want]);
}

// ----- the same under the Corner engine (round corner-desktops) -------------------------------------
//
// Each reads as its Spaces twin does. What each read before, with the Desktops guessed from
// the windows on screen, is pinned in `guessed.txt` (the fallback) and told here.

#[test]
fn corner_desktop_keys_and_changes_with_no_notice() {
    let keys = "there: front [13] shows [13] desktop 2 app 101 switches 0 slides 1; back: front [12] shows [12] desktop 1 app 102 switches 0 slides 2";
    let agreed = "shadow 0 shown (0 open) 0 homes";
    // Before: the same for a person.
    let read_at_once = format!("{keys}; waited for the flip 0, taken before the notice 0, notices after 0; {agreed}");
    expect(desktop_keys_flip_first_corner, [&read_at_once, &read_at_once, &read_at_once]);
    let waited = format!("{keys}; waited for the flip 2, taken before the notice 0, notices after 0; {agreed}");
    expect(desktop_keys_notice_first_corner, [&waited, &read_at_once, &read_at_once]);
    // Before: FOREIGN_SETTLE, two looks a second apart ("within 2 s"), and at load 1 and 3 the
    // activation of Desktop 2's front app, held for the slide, was decided for the Desktop
    // left: a switch there and macOS slid back (front [11] on Desktop 1).
    let ticked = "Desktop 2 recognized within 1 s; front [13] shows [13] desktop 2 app 101 switches 0 slides 0; shadow 0 shown (0 open) 0 homes";
    // At load 10 a look falls at the slide's end: sooner, the same end.
    let sooner = "Desktop 2 recognized by the slide's end; front [13] shows [13] desktop 2 app 101 switches 0 slides 0; shadow 0 shown (0 open) 0 homes";
    expect(flip_with_no_notice_corner, [ticked, ticked, sooner]);
    // Before: "settled at once" (the arrival guessed the same Desktop again). Now the
    // listing is waited for, FLIP_WAIT (a cap, 300 ms), and nothing arrives.
    let nothing = "settled within 0.5 s; front [12] shows [12] desktop 1 app 102 switches 0 slides 1; shadow 0 shown (0 open) 0 homes";
    expect(notice_with_no_flip_corner, [nothing, nothing, nothing]);
}

#[test]
fn corner_both_desktops_listed_a_listing_not_readable_and_a_first_visit() {
    let keys = "there: front [13] shows [13] desktop 2 app 101 switches 0 slides 1; back: front [12] shows [12] desktop 1 app 102 switches 0 slides 2; shadow 0 shown (0 open) 0 homes";
    let at_the_end = format!("Desktop 2 recognized by the slide's end; {keys}");
    // Before: MIXED_WAIT ("within 1 s"). The windows of the Desktop left, parked ones too,
    // are on their own Desktop by the window server's word and left out.
    expect(both_desktops_listed_at_the_arrival_corner, [&at_the_end, &at_the_end, &at_the_end]);
    // The arrival goes by the windows, as before; the next good read agrees (a parked window
    // is listed on screen with its Desktop, so the guess finds the Space it knows).
    expect(listing_unreadable_for_one_arrival_corner, [&at_the_end, &at_the_end, &at_the_end]);
    // Before: Charlie's stage came in front with a switch in view after the arrival, and
    // another on the way back (switches 1 each).
    let right = "there: front [14] shows [14] desktop 2 app 103 switches 0 slides 1; Charlie's window placed on its own Desktop; back: front [12] shows [12] desktop 1 app 102 switches 0 slides 2; shadow 0 shown (0 open) 0 homes";
    expect(first_visit_to_an_unknown_apps_desktop_corner, [right, right, right]);
}

#[test]
fn corner_a_window_moved_and_a_window_on_every_desktop() {
    // Before: SPACE_CHECKS, two looks ("within 3.5 s").
    let moved = "left its stage within 2.5 s; here: front [] shows [] desktop 1 app 102 switches 0 slides 0; there: stages [[12, 15], [13]] front [12, 15] shows [12, 15] desktop 2 app 102 switches 0 slides 1; shadow 0 shown (0 open) 0 homes";
    expect(window_moved_corner, [moved, moved, moved]);
    // A hidden one, in the park corner of its Desktop: Mission Control shows it and it can be
    // dragged. Asked for its Desktop as a shown one is (before: SPACE_CHECKS too).
    let hidden = "left its stage within 2.5 s; here: stages [[12]] front [12] shows [12] desktop 1 app 102 switches 0 slides 0; there: stages [[11, 13], [15]] front [11, 13] shows [11, 13] desktop 2 app 101 switches 0 slides 1; shadow 0 shown (0 open) 0 homes";
    expect(hidden_window_moved_corner, [hidden, hidden, hidden]);
    // Before: STICKY_HOPS, a stage of its own on each Desktop in turn for two arrivals.
    let every = "2: every stages [[13], [15]] shows [14, 15]; 1: every stages [[11]] shows [14]; 2: every stages [[13], [15]] shows [14, 15]; 1: every stages [[11]] shows [14]; shadow 0 shown (0 open) 0 homes";
    expect(window_on_every_desktop_corner, [every, every, every]);
}

#[test]
fn corner_full_screen_and_another_screens_desktop() {
    // Before: the same (the kind of Space was asked of the window, through Accessibility).
    let want = "full screen seen within 1 s, still true; the Desktop back within 1 s, its stages kept true; front [12] shows [12] desktop 1 app 102 switches 0 slides 2; shadow 0 shown (0 open) 0 homes";
    expect(full_screen_in_and_out_corner, [want, want, want]);
    // Before: every screen arrived at the notice, the second screen's Desktop was not
    // recognized by its one window, and the main screen's hidden stage was brought out
    // (shows [11, 12]).
    let only = "the second screen shows window 16's Desktop true; arrivals 1; the main screen's Desktop kept true; front [12] shows [12, 16] desktop 1 app 102 switches 0 slides 1; shadow 0 shown (0 open) 0 homes";
    expect(another_screens_desktop_changes_corner, [only, only, only]);
}

#[test]
fn a_listing_that_is_there_and_wrong_rests_and_the_guess_takes_over() {
    // The notice finds no flip (FLIP_WAIT), which brings the reads' check to the next looks:
    // three disagree, the reads rest, and the Desktop is recognized by its windows as before
    // the reads (FOREIGN_SETTLE). Once the listing is right again it decides again.
    let want = "reads working; Desktop 2 recognized within 6 s, reads resting; there: front [13] shows [13] desktop 2 app 101 switches 0 slides 1; ten minutes on reads working; back: front [12] shows [12] desktop 1 app 102 switches 0 slides 2";
    for load in LOADS {
        let run = listing_stuck_corner(load);
        assert_eq!(run.outcome, want, "at load {load}:\n{}", run.story);
        assert_eq!(run.rests, 1, "at load {load}: one rest");
    }
}

/// Every scenario here, for the load table.
pub(super) const ALL: &[Scenario] = &[
    ("Desktop keys, the listing flips before the notice (Spaces)", "a slow main thread; the desk before 11-21", desktop_keys_flip_first),
    ("Desktop keys, the notice 5 ms before the flip (Spaces)", "probe-system flip, 11-21", desktop_keys_notice_first),
    ("Desktop keys, the window server's notice of the change's end heard (Spaces)", "server_notices.rs 1401; design r3 decision 2", desktop_keys_server_tells),
    ("a Desktop change whose notice never comes, the window server tells (Spaces)", "controller/desktops.rs sync_desktops; server_notices.rs 1401", flip_with_no_notice_server_tells),
    ("a Desktop change whose notice never comes, nothing tells (Spaces)", "controller/desktops.rs sync_desktops, SPACE_POLL", flip_with_no_notice_nothing_tells),
    ("a Space change's notice with no change (Spaces)", "design r3, decision 2", notice_with_no_flip),
    ("both Desktops listed at the arrival (Spaces)", "controller/desktops.rs keyed_live (before: MIXED_WAIT)", both_desktops_listed_at_the_arrival),
    ("the listing not readable for one arrival (Spaces)", "design r3, decision 1's fallback", listing_unreadable_for_one_arrival),
    ("the first visit to a Desktop with an unknown app in front (Spaces)", "the bench's author, 11-21", first_visit_to_an_unknown_apps_desktop),
    ("a window moved to another Desktop by hand, the window server tells (Spaces)", "controller/desktops.rs desk_membership; server_notices.rs 1325", window_moved_server_tells),
    ("a window moved to another Desktop by hand, nothing tells (Spaces)", "controller/desktops.rs desk_membership, the tick", window_moved_nothing_tells),
    ("a window on every Desktop (Spaces)", "engine/model/desks.rs Listed::every (before: STICKY_HOPS)", window_on_every_desktop),
    ("a window into full screen and out (Spaces)", "engine/fullscreen.rs, 10-12", full_screen_in_and_out),
    ("another screen's Desktop changes (Spaces)", "design r3 decision 9 (before: every screen arrived)", another_screens_desktop_changes),
    // The same under the Corner engine (round corner-desktops): what each read with the
    // Desktops guessed from the windows on screen is pinned in `guessed.txt`.
    ("Desktop keys, the listing flips before the notice (Corner)", "the Spaces scenario's twin", desktop_keys_flip_first_corner),
    ("Desktop keys, the notice 5 ms before the flip (Corner)", "the Spaces scenario's twin", desktop_keys_notice_first_corner),
    ("a Desktop change whose notice never comes (Corner)", "the Spaces scenario's twin (before: FOREIGN_SETTLE)", flip_with_no_notice_corner),
    ("a Space change's notice with no change (Corner)", "the Spaces scenario's twin", notice_with_no_flip_corner),
    ("both Desktops listed at the arrival (Corner)", "the Spaces scenario's twin (before: MIXED_WAIT)", both_desktops_listed_at_the_arrival_corner),
    ("the listing not readable for one arrival (Corner)", "the Spaces scenario's twin", listing_unreadable_for_one_arrival_corner),
    ("the first visit to a Desktop with an unknown app in front (Corner)", "the Spaces scenario's twin", first_visit_to_an_unknown_apps_desktop_corner),
    ("a window moved to another Desktop by hand (Corner)", "the Spaces scenario's twin (before: SPACE_CHECKS)", window_moved_corner),
    ("a window on every Desktop (Corner)", "the Spaces scenario's twin (before: STICKY_HOPS)", window_on_every_desktop_corner),
    ("a window into full screen and out (Corner)", "the Spaces scenario's twin", full_screen_in_and_out_corner),
    ("another screen's Desktop changes (Corner)", "the Spaces scenario's twin (before: every screen arrived)", another_screens_desktop_changes_corner),
    ("a hidden window moved to another Desktop by hand (Corner)", "UX-SPEC, Known gaps (before: SPACE_CHECKS)", hidden_window_moved_corner),
    ("the listing stuck on the Desktop left (Corner)", "engine/desk_reads.rs, the reads' check", listing_stuck_corner),
];
