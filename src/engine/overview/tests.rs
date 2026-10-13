//! Recorded window lists (10-12, macOS 27, a 2560×1440 display, Apple's Stage Manager off;
//! the expose phase's fixture app, ids as listed, other apps' windows left out, no titles):
//! Mission Control, App Exposé and Show Desktop typed through the app, each left the way it
//! was entered. Times are milliseconds from the shortcut. The once-a-second check is replayed
//! over them at every phase a check can fall on.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::*;

const D: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);

/// One listed window: owner, layer, bounds, window id (0 for macOS's own).
#[derive(Clone, Copy, Debug)]
struct W(&'static str, i64, f64, f64, f64, f64, u32);

const FIX: &str = "SM Fixture Stages";
const MENU: W = W("Window Server", 24, 0.0, 0.0, 2560.0, 30.0, 0);
const STRIP: W = W("Stage Manager", -1, 0.0, 30.0, 200.0, 1410.0, 0);
const DOCK: W = W("Dock", 20, 0.0, 0.0, 2560.0, 1440.0, 0);
const WM19: W = W("WindowManager", 19, 0.0, 0.0, 2560.0, 1440.0, 0);
const WM18: W = W("WindowManager", 18, 0.0, 0.0, 2560.0, 1440.0, 0);
/// Mission Control's Spaces bar band.
const WM14: W = W("WindowManager", 14, 0.0, 0.0, 2560.0, 96.0, 0);
/// One of Mission Control's app icons under a window.
const ICON: W = W("WindowManager", 17, 1474.0, 507.0, 66.0, 82.0, 0);
/// One of App Exposé's window labels (layer 0, but macOS's own).
const LABEL: W = W("WindowManager", 0, 300.0, 867.0, 71.0, 32.0, 0);

fn fx(id: u32, x: f64, y: f64) -> W {
    W(FIX, 0, x, y, 600.0, 392.0, id)
}

/// A recorded moment: ms from the shortcut, the windows listed on screen.
type Frame = (u64, Vec<W>);

fn sightings(list: &[W]) -> Vec<Sighting<'static>> {
    list.iter()
        .map(|w| Sighting { owner: w.0, layer: w.1, bounds: Rect::new(w.2, w.3, w.4, w.5), onscreen: true })
        .collect()
}

fn seen(list: &[W]) -> Option<Overview> {
    overview_in(sightings(list), D)
}

/// App windows (layer 0, ours to know) listed on screen.
fn app_windows(list: &[W]) -> Vec<u32> {
    let mut ids: Vec<u32> = list.iter().filter(|w| w.1 == 0 && w.6 != 0).map(|w| w.6).collect();
    ids.sort_unstable();
    ids
}

/// Mission Control typed at 0 and left with Escape at 1518 (recorded 20:09:25.944).
fn mission_control() -> Vec<Frame> {
    vec![
        (0, vec![MENU, STRIP, fx(48717, 300.0, 218.0), fx(48715, 2559.0, 1408.0)]),
        (100, vec![DOCK, WM19, ICON, WM14, STRIP, fx(48717, 708.0, 332.0), fx(48715, 2508.0, 1279.0)]),
        (450, vec![DOCK, WM19, ICON, WM14, fx(48717, 2295.0, 775.0), fx(48715, 2310.0, 780.0)]),
        (1528, vec![DOCK, WM19, ICON, WM14, STRIP, fx(48717, 2162.0, 738.0), fx(48715, 2326.0, 822.0)]),
        (1779, vec![DOCK, WM19, ICON, WM14, STRIP, fx(48717, 327.0, 225.0), fx(48715, 2555.0, 1399.0)]),
        (1841, vec![DOCK, WM14, STRIP, fx(48717, 304.0, 219.0), fx(48715, 2558.0, 1406.0)]),
        (2014, vec![DOCK, STRIP, fx(48717, 300.0, 218.0), fx(48715, 2559.0, 1408.0)]),
        (2132, vec![MENU, STRIP, fx(48717, 300.0, 218.0), fx(48715, 2559.0, 1408.0)]),
        (6000, vec![MENU, STRIP, fx(48717, 300.0, 218.0), fx(48715, 2559.0, 1408.0)]),
    ]
}

/// App Exposé typed at 0 and left with Escape at 1496 (recorded 20:09:31.756).
fn app_expose() -> Vec<Frame> {
    vec![
        (0, vec![MENU, STRIP, fx(48717, 300.0, 218.0), fx(48715, 2559.0, 1408.0)]),
        (41, vec![DOCK, WM19, LABEL, STRIP, fx(48717, 137.0, 376.0), fx(48715, 1412.0, 829.0)]),
        (429, vec![DOCK, WM19, LABEL, fx(48717, 37.0, 474.0), fx(48715, 706.0, 474.0)]),
        (1575, vec![DOCK, WM19, LABEL, STRIP, fx(48717, 225.0, 290.0), fx(48715, 2031.0, 1142.0)]),
        (1738, vec![DOCK, WM19, LABEL, STRIP, fx(48717, 296.0, 221.0), fx(48715, 2533.0, 1395.0)]),
        (1789, vec![DOCK, STRIP, fx(48717, 298.0, 219.0), fx(48715, 2550.0, 1403.0)]),
        (2120, vec![MENU, STRIP, fx(48717, 300.0, 218.0), fx(48715, 2559.0, 1408.0)]),
        (6000, vec![MENU, STRIP, fx(48717, 300.0, 218.0), fx(48715, 2559.0, 1408.0)]),
    ]
}

/// Show Desktop typed at 0 and again at 4761 (recorded 20:09:37.013): the shown window slides
/// off the left edge and stays listed on screen there.
fn show_desktop() -> Vec<Frame> {
    vec![
        (0, vec![WM18, MENU, STRIP, fx(48717, 300.0, 218.0), fx(48715, 2559.0, 1408.0)]),
        (70, vec![WM18, MENU, W("Stage Manager", -1, -149.0, 39.0, 200.0, 1410.0, 0), fx(48717, -402.0, 78.0)]),
        (356, vec![WM18, MENU, fx(48717, -588.0, 42.0), fx(48715, 2559.0, 1408.0)]),
        (4800, vec![WM18, MENU, fx(48717, -39.0, 150.0), fx(48715, 2559.0, 1408.0)]),
        (5015, vec![WM18, MENU, fx(48717, 294.0, 216.0), fx(48715, 2559.0, 1408.0)]),
        (5072, vec![MENU, STRIP, fx(48717, 298.0, 217.0), fx(48715, 2559.0, 1408.0)]),
        (9000, vec![MENU, STRIP, fx(48717, 300.0, 218.0), fx(48715, 2559.0, 1408.0)]),
    ]
}

/// The run-5 failure (10-12, hands-free run on Desktop 2): App Exposé shows the app's windows
/// of Desktop 1 (48652, 48653, 48655) beside Desktop 2's (48666, 48668), and they are still
/// listed 121 ms after Escape (when the check fired). `late` keeps them listed that long after
/// macOS's last overview window goes (the worst case: the list lags the animation).
fn app_expose_from_desktop_2(late: u64) -> Vec<Frame> {
    let d2 = || vec![fx(48666, 300.0, 218.0), fx(48668, 260.0, 78.0)];
    let all = |overlays: Vec<W>| {
        let mut v = overlays;
        v.extend(d2());
        v.extend([fx(48652, 80.0, 78.0), fx(48653, 190.0, 148.0), fx(48655, 410.0, 288.0)]);
        v
    };
    let mut frames = vec![
        (0, [vec![MENU, STRIP], d2()].concat()),
        (41, all(vec![DOCK, WM19, LABEL])),
        (1617, all(vec![DOCK, WM19, LABEL, STRIP])),
        (1789, all(vec![DOCK, STRIP])),
        (2120, if late > 0 { all(vec![MENU, STRIP]) } else { [vec![MENU, STRIP], d2()].concat() }),
    ];
    if late > 0 {
        frames.push((2120 + late, [vec![MENU, STRIP], d2()].concat()));
    }
    frames.push((8000, [vec![MENU, STRIP], d2()].concat()));
    frames
}

/// The listing at `t` ms: the last frame recorded at or before it.
fn at(frames: &[Frame], t: u64) -> &[W] {
    &frames.iter().rev().find(|(ft, _)| *ft <= t).unwrap_or(&frames[0]).1
}

fn ms(t0: Instant, n: u64) -> Instant {
    t0 + Duration::from_millis(n)
}

/// The once-a-second check's Space inference replayed over `frames` from `phase` ms on, up
/// to `until`: the watch decides whether it looks; when it does, windows of another Space on
/// screen (by `homes`, against `current`) must persist a settle apart. Returns the times at
/// which it would recognize the Space again.
fn replay(frames: &[Frame], homes: &BTreeMap<u32, u8>, current: u8, phase: u64, until: u64) -> Vec<u64> {
    let t0 = Instant::now();
    let mut watch = OverviewWatch::default();
    let mut foreign = Persisting::default();
    let mut fired = Vec::new();
    let mut t = phase;
    while t <= until {
        let list = at(frames, t);
        let now = ms(t0, t);
        watch.see(seen(list), now);
        if watch.freezes(now) {
            foreign.clear();
        } else {
            let ids: Vec<u32> =
                app_windows(list).into_iter().filter(|id| homes.get(id).is_some_and(|s| *s != current)).collect();
            if foreign.observe(ids, now, FOREIGN_SETTLE) {
                fired.push(t);
                foreign.clear();
            }
        }
        t += 1000;
    }
    fired
}

#[test]
fn nothing_shows_on_a_plain_desktop() {
    // Recorded with nothing up: the menu bar, the Dock's own bar (hidden here, so unlisted),
    // and macOS's desktop windows far below layer 0, one of them WindowManager's covering
    // the display.
    let plain = [
        MENU,
        W("WindowManager", -2147483624, 0.0, 0.0, 2560.0, 1440.0, 0),
        W("Window Server", -2147483626, 0.0, 0.0, 2560.0, 1440.0, 0),
        W("Wallpaper", -2147483625, 0.0, 0.0, 2560.0, 1440.0, 0),
        W("Finder", -2147483603, 0.0, 0.0, 2560.0, 1440.0, 0),
        W("Dock", 20, 0.0, 1360.0, 2560.0, 80.0, 0),
        // Our own motion overlay covers the display at layer 25 during a switch.
        W("Stage Manager", 25, 0.0, 0.0, 2560.0, 1440.0, 0),
        STRIP,
        fx(48717, 300.0, 218.0),
    ];
    assert_eq!(seen(&plain), None);
    // Listed but off screen does not count.
    let mut off = sightings(&[DOCK, WM19]);
    for s in &mut off {
        s.onscreen = false;
    }
    assert_eq!(overview_in(off, D), None);
}

#[test]
fn the_recorded_overviews_are_told() {
    let mc = mission_control();
    assert_eq!(seen(at(&mc, 0)), None);
    for t in [100, 450, 1528, 1779, 1841, 2014] {
        assert_eq!(seen(at(&mc, t)), Some(Overview::MissionControl), "Mission Control at {t} ms");
    }
    assert_eq!(seen(at(&mc, 2132)), None);
    let ax = app_expose();
    for t in [41, 429, 1575, 1738, 1789] {
        assert_eq!(seen(at(&ax, t)), Some(Overview::MissionControl), "App Exposé at {t} ms");
    }
    assert_eq!(seen(at(&ax, 2120)), None);
    let sd = show_desktop();
    for t in [0, 70, 356, 4800, 5015] {
        assert_eq!(seen(at(&sd, t)), Some(Overview::ShowDesktop), "Show Desktop at {t} ms");
    }
    assert_eq!(seen(at(&sd, 5072)), None);
    // On another display's frame nothing covers.
    assert_eq!(overview_in(sightings(&[DOCK, WM19]), Rect::new(2560.0, 0.0, 1920.0, 1080.0)), None);
}

#[test]
fn the_watch_freezes_while_it_shows_and_a_settle_after() {
    let t0 = Instant::now();
    let mut w = OverviewWatch::default();
    assert_eq!(w.see(None, t0), Change::None);
    assert!(!w.freezes(t0) && !w.holds_arrival(t0));
    assert_eq!(w.see(Some(Overview::MissionControl), ms(t0, 100)), Change::Began(Overview::MissionControl));
    assert_eq!(w.see(Some(Overview::MissionControl), ms(t0, 1100)), Change::None);
    assert!(w.freezes(ms(t0, 1100)) && w.holds_arrival(ms(t0, 1100)));
    assert_eq!(w.see(None, ms(t0, 2000)), Change::Ended(Overview::MissionControl));
    assert!(w.freezes(ms(t0, 2000)) && w.holds_arrival(ms(t0, 2000)));
    assert!(w.freezes(ms(t0, 2299)) && w.holds_arrival(ms(t0, 2299)));
    assert!(w.freezes(ms(t0, 2300)) && !w.holds_arrival(ms(t0, 2300)), "a notified change goes first");
    assert!(w.freezes(ms(t0, 2999)));
    assert!(!w.freezes(ms(t0, 3000)), "inference resumes a settle after it closed");
    // Show Desktop right after Mission Control: frozen again at once.
    assert_eq!(w.see(Some(Overview::ShowDesktop), ms(t0, 3100)), Change::Began(Overview::ShowDesktop));
    assert!(w.freezes(ms(t0, 3100)));
}

#[test]
fn an_overview_listed_for_ever_stops_freezing() {
    let t0 = Instant::now();
    let mut w = OverviewWatch::default();
    w.see(Some(Overview::ShowDesktop), t0);
    let late = t0 + OVERVIEW_STUCK;
    assert_eq!(w.see(Some(Overview::ShowDesktop), late - Duration::from_millis(1)), Change::None);
    assert!(w.freezes(late - Duration::from_millis(1)));
    assert_eq!(w.see(Some(Overview::ShowDesktop), late), Change::Stuck(Overview::ShowDesktop));
    assert!(!w.freezes(late) && w.showing().is_none());
    assert_eq!(w.see(Some(Overview::ShowDesktop), late + Duration::from_secs(1)), Change::None, "said once");
    // Once it goes and comes back it counts again.
    w.see(None, late + Duration::from_secs(2));
    assert_eq!(w.see(Some(Overview::ShowDesktop), late + Duration::from_secs(3)), Change::Began(Overview::ShowDesktop));
    assert!(w.freezes(late + Duration::from_secs(3)));
}

#[test]
fn evidence_counts_only_when_it_holds_a_settle_apart() {
    let t0 = Instant::now();
    let mut p = Persisting::default();
    let s = FOREIGN_SETTLE;
    assert!(!p.observe(vec![1, 2], t0, s), "never from one listing");
    assert!(!p.observe(vec![1, 2], ms(t0, 400), s), "two listings too close together");
    assert!(p.observe(vec![1, 2], ms(t0, 1000), s));
    // Other evidence starts over; none forgets it.
    let mut p = Persisting::default();
    p.observe(vec![1, 2], t0, s);
    assert!(!p.observe(vec![1], ms(t0, 1000), s));
    assert!(!p.observe(vec![1], ms(t0, 1500), s));
    assert!(p.observe(vec![1], ms(t0, 2000), s));
    let mut p = Persisting::default();
    p.observe(vec![1], t0, s);
    assert!(!p.observe(vec![], ms(t0, 1000), s));
    assert!(!p.observe(vec![1], ms(t0, 2000), s));
    p.clear();
    assert!(!p.observe(vec![1], ms(t0, 3000), s));
    assert!(p.observe(vec![1], ms(t0, 4000), s));
}

#[test]
fn app_expose_never_recognizes_the_desktop_again() {
    // Desktop 2 shows; three of the app's windows live on Desktop 1.
    let homes: BTreeMap<u32, u8> = [(48666, 2), (48668, 2), (48652, 1), (48653, 1), (48655, 1)].into();
    for late in [0, 120, 400, 900] {
        let frames = app_expose_from_desktop_2(late);
        // Every phase the once-a-second check can fall on, 50 ms apart.
        for phase in (0..1000).step_by(50) {
            let fired = replay(&frames, &homes, 2, phase, 8000);
            assert!(fired.is_empty(), "late {late} ms, checks from {phase} ms: recognized again at {fired:?}");
        }
    }
}

#[test]
fn mission_control_and_show_desktop_leave_inference_alone() {
    // Seen from a Desktop that holds only 48717: 48715 is another Desktop's (as a window
    // moved there by hand would be) and shows in Mission Control's layout.
    let homes: BTreeMap<u32, u8> = [(48717, 1), (48715, 2)].into();
    let mc: Vec<Frame> = mission_control()
        .into_iter()
        .map(|(t, list)| (t, list.into_iter().filter(|w| w.6 != 48715 || (100..2014).contains(&t)).collect()))
        .collect();
    for phase in (0..1000).step_by(50) {
        assert!(replay(&mc, &homes, 1, phase, 6000).is_empty(), "Mission Control, checks from {phase} ms");
    }
    // Show Desktop: the shown window slides off the display but stays listed; nothing
    // infers while it shows.
    let t0 = Instant::now();
    let sd = show_desktop();
    let mut w = OverviewWatch::default();
    for t in (0..=5050).step_by(50) {
        w.see(seen(at(&sd, t)), ms(t0, t));
        assert!(w.freezes(ms(t0, t)), "frozen at {t} ms");
    }
    w.see(seen(at(&sd, 5100)), ms(t0, 5100));
    assert!(w.freezes(ms(t0, 6050)) && !w.freezes(ms(t0, 6100)));
}

#[test]
fn a_missed_desktop_change_is_still_recognized() {
    // No overview: Desktop 1's windows listed on screen at two checks a second apart (a
    // Space change whose notification never came) are recognized at the second.
    let homes: BTreeMap<u32, u8> = [(48666, 2), (48652, 1), (48653, 1)].into();
    let frames: Vec<Frame> = vec![
        (0, vec![MENU, STRIP, fx(48666, 300.0, 218.0)]),
        (500, vec![MENU, fx(48652, 80.0, 78.0), fx(48653, 190.0, 148.0)]),
    ];
    assert_eq!(replay(&frames, &homes, 2, 0, 3000), vec![2000]);
    // One listing alone (it went back at once) is not enough.
    let blip: Vec<Frame> = vec![
        (0, vec![MENU, STRIP, fx(48666, 300.0, 218.0)]),
        (900, vec![MENU, fx(48652, 80.0, 78.0)]),
        (1100, vec![MENU, STRIP, fx(48666, 300.0, 218.0)]),
    ];
    assert!(replay(&blip, &homes, 2, 0, 5000).is_empty());
}

#[test]
fn a_desktop_clicked_in_mission_control_is_handled_once_it_has_closed() {
    // Mission Control's recorded exit, with the clicked Desktop's window (48720) coming in
    // where the shown Desktop's went: the Space change is notified at 1600 ms, while
    // Mission Control still animates out. (Constructed from the recording: no run here
    // switches Desktops.)
    let frames: Vec<Frame> = vec![
        (0, vec![DOCK, WM19, ICON, WM14, fx(48717, 2295.0, 775.0)]),
        (1528, vec![DOCK, WM19, ICON, WM14, fx(48717, 2162.0, 738.0), fx(48720, 1200.0, 500.0)]),
        (1779, vec![DOCK, WM19, ICON, WM14, fx(48720, 600.0, 300.0)]),
        (1841, vec![DOCK, WM14, fx(48720, 520.0, 260.0)]),
        (2014, vec![DOCK, fx(48720, 500.0, 250.0)]),
        (2132, vec![MENU, fx(48720, 500.0, 250.0)]),
    ];
    let t0 = Instant::now();
    let mut w = OverviewWatch::default();
    w.see(seen(at(&frames, 0)), t0);
    // The arrival waits while Mission Control shows, looked at again every tick (100 ms).
    let mut arrived = None;
    for t in (1600..4000).step_by(100) {
        w.see(seen(at(&frames, t)), ms(t0, t));
        if !w.holds_arrival(ms(t0, t)) {
            arrived = Some(t);
            break;
        }
    }
    let arrived = arrived.expect("the notified change is handled");
    assert!(arrived >= 2132 + 300, "handled at {arrived} ms, after Mission Control closed and a settle");
    assert!(arrived <= 2200 + 300, "handled at {arrived} ms, not held longer");
    assert_eq!(app_windows(at(&frames, arrived)), vec![48720], "the list shows only the clicked Desktop's window");
    // Inference stays frozen a little longer than the arrival waits.
    assert!(w.freezes(ms(t0, arrived)));
}
