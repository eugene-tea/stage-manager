//! `probe-spaces space-flip BUNDLE-A BUNDLE-B [--rounds N]` (probe 2, the review's R3): when
//! does the window server's current Desktop change during a slide, against
//! `activeSpaceDidChangeNotification` and the window list?
//!
//! A thread reads, every 1 ms, each screen's "Current Space" from
//! `SLSCopyManagedDisplaySpaces`, `SLSGetActiveSpace`, and where the two fixture windows are
//! (`CGWindowListCreateDescriptionFromArray`): on screen at their own frame, or Alpha's
//! covering the display (full screen); it stamps each change. The main run loop stamps every
//! `activeSpaceDidChangeNotification` as it is delivered (keys, `open` and Accessibility calls
//! go out from a thread of their own meanwhile).
//!
//! Setup: Alpha's window (BUNDLE-A) and Bravo's (BUNDLE-B) on the Desktop shown; Bravo's is
//! moved to the next Desktop of the main screen (right, else left: the bridged
//! `SLSMoveWindowsToManagedSpace`, as the Spaces engine brings windows home), so Bravo has its
//! only window there. Cases, `--rounds` times each (default 3), each there and back:
//!
//! - the Desktop keys (Keyboard Shortcuts' "Move right/left a space") posted;
//! - Bravo opened by its path (an activation that slides), then Alpha (back);
//! - Mission Control opened by its shortcut, the Desktop key posted inside it, Escape 0.8 s
//!   later (as the live suite's `mission-desktop` phase);
//! - Alpha's window into full screen and out (`AXFullScreen`).
//!
//! The first three need a second Desktop on the main screen: with one they are skipped and
//! said. Every leg waits up to 4 s for the arriving window at its frame, then 0.8 s more. At
//! the end Bravo's window goes back to the Desktop the probe started on, which shows again.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{Args, Ctx, at_frame, cell, fixture, main_entry, main_frame, median, ms, off_main, post_plain_stamped, post_stamped, pump, pump_until, sight_of, table};
use crate::engine::fullscreen::covers_display;
use crate::engine::geometry::Rect;
use crate::engine::overview::Overview;
use crate::platform::ax::AxElement;
use crate::platform::space_keys::{Direction, ESCAPE_KEY, Expose, SpaceKey};
use crate::platform::spaces::ffi::{Route, Sky};
use crate::platform::{apps, permissions, windows};

const EVERY: Duration = Duration::from_millis(1);
const ARRIVE_LIMIT: Duration = Duration::from_secs(4);
const TAIL: Duration = Duration::from_millis(800);
const REST: Duration = Duration::from_millis(600);
/// Inside Mission Control: its opening settled before the key, Escape after it.
const MISSION_SETTLE: Duration = Duration::from_millis(700);
const MISSION_ESCAPE_AFTER: Duration = Duration::from_millis(800);

/// Flags of a read: where the fixture windows are.
const A_HOME: u8 = 1;
const A_FULL: u8 = 2;
const B_HOME: u8 = 4;

/// Every `activeSpaceDidChangeNotification` heard.
static NOTICES: Mutex<Vec<Instant>> = Mutex::new(Vec::new());

/// One read of the window server.
#[derive(Clone, Debug, PartialEq)]
struct Read {
    /// The main screen's "Current Space".
    main: u64,
    /// Every screen's, in the call's order.
    all: Vec<u64>,
    /// `SLSGetActiveSpace`.
    active: u64,
    flags: u8,
}

/// What a leg waits for.
#[derive(Clone, Copy)]
enum Want {
    Space(u64, u8),
    Not(u64, u8),
}

impl Want {
    fn met(self, main: u64, flags: u8) -> bool {
        match self {
            Want::Space(s, f) => main == s && flags & f != 0,
            Want::Not(s, f) => main != s && flags & f != 0,
        }
    }

    fn flag(self) -> u8 {
        match self {
            Want::Space(_, f) | Want::Not(_, f) => f,
        }
    }
}

/// One leg's stamps.
struct Leg {
    case: String,
    cause: Instant,
    escape: Option<Instant>,
    end: Instant,
    want: Want,
    note: String,
}

/// The fixture windows and where they belong.
struct Setup {
    a: u32,
    a_frame: Rect,
    b: u32,
    b_frame: Rect,
    display: Rect,
    main_id: String,
}

fn read(sky: &Sky, s: &Setup) -> Read {
    let displays = sky.displays();
    let main = displays.iter().find(|d| d.0 == s.main_id).map_or(0, |d| d.1);
    let all = displays.iter().map(|d| d.1).collect();
    let active = sky.active_space().unwrap_or(0);
    let mut flags = 0;
    for w in windows::describe(&[s.a, s.b]) {
        if !w.onscreen {
            continue;
        }
        if w.id == s.a && at_frame(w.bounds, s.a_frame) {
            flags |= A_HOME;
        }
        if w.id == s.a && covers_display(w.bounds, s.display) {
            flags |= A_FULL;
        }
        if w.id == s.b && at_frame(w.bounds, s.b_frame) {
            flags |= B_HOME;
        }
    }
    Read { main, all, active, flags }
}

/// Reads every [`EVERY`] until `stop`; each change, and the latest main Space and flags.
fn sample(sky: &Sky, s: &Setup, stop: &AtomicBool, main: &AtomicU64, flags: &AtomicU8) -> (Vec<(Instant, Read)>, u64, f64) {
    let (mut out, mut reads, t0) = (Vec::<(Instant, Read)>::new(), 0u64, Instant::now());
    while !stop.load(Ordering::Relaxed) {
        let next = Instant::now() + EVERY;
        // A pool per read: the dictionaries' getters may autorelease, and this thread has none.
        let r = objc2::rc::autoreleasepool(|_| read(sky, s));
        let at = Instant::now();
        reads += 1;
        main.store(r.main, Ordering::Relaxed);
        flags.store(r.flags, Ordering::Relaxed);
        if out.last().is_none_or(|(_, last)| *last != r) {
            out.push((at, r));
        }
        if let Some(d) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }
    let every = if reads > 0 { t0.elapsed().as_secs_f64() * 1e3 / reads as f64 } else { 0.0 };
    (out, reads, every)
}

/// Moves `ids` onto Desktop `sid` (the bridged move, then the bridged add as the engine's
/// bring-back does); true when the window server lists them there alone.
fn put_on(sky: &Sky, ids: &[u32], sid: u64) -> bool {
    let there = || sky.spaces_of(ids).iter().all(|s| *s == sid) && !sky.spaces_of(ids).is_empty();
    let _ = sky.move_managed(Route::Bridged, ids, sid);
    pump(Duration::from_millis(200));
    if there() {
        return true;
    }
    let _ = sky.add_remove(Route::Bridged, sid, ids);
    pump(Duration::from_millis(200));
    there()
}

/// `open PATH` (the app by its path, never `open -b`); when it was spawned.
fn open_path(path: &str) -> Result<Instant, String> {
    let at = Instant::now();
    let mut child = std::process::Command::new("/usr/bin/open").arg(path).spawn().map_err(|e| format!("open: {e}"))?;
    let _ = child.wait();
    Ok(at)
}

/// Window `id` of `pid` into or out of full screen through Accessibility; when it was asked.
fn full_screen(pid: i32, id: u32, on: bool) -> Result<Instant, String> {
    let w = AxElement::application(pid)
        .windows()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|e| e.window_id().ok() == Some(id))
        .ok_or("the fixture window is not reachable through Accessibility")?;
    let at = Instant::now();
    if w.set_fullscreen(on).is_err() {
        w.press_fullscreen_button().map_err(|e| format!("full screen: {e}"))?;
    }
    Ok(at)
}

/// Mission Control by its shortcut, shown (the list), the Desktop key inside it, Escape after.
fn mission_pick(key: &SpaceKey, display: Rect) -> Result<(Instant, Option<Instant>), String> {
    post_stamped(&SpaceKey::read_expose(Expose::MissionControl))?;
    let end = Instant::now() + Duration::from_secs(3);
    while sight_of(&windows::on_screen(), display).overview != Some(Overview::MissionControl) {
        if Instant::now() > end {
            return Err("Mission Control did not show in 3 s".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(MISSION_SETTLE);
    let cause = post_stamped(key)?;
    std::thread::sleep(MISSION_ESCAPE_AFTER);
    let escape = post_plain_stamped(ESCAPE_KEY)?;
    Ok((cause, Some(escape)))
}

pub(super) fn run(ctx: &Ctx, args: &Args) -> Result<(), String> {
    let usage = "space-flip BUNDLE-A BUNDLE-B [--rounds N]";
    let (bundle_a, bundle_b) = (args.bundle(0, usage)?, args.bundle(1, usage)?);
    if !permissions::accessibility_trusted() {
        return Err("no Accessibility for this process (launch it through `open` from the dev app)".into());
    }
    let sky = &ctx.sky;
    if sky.copy_managed.is_none() || sky.active.is_none() {
        return Err("SLSCopyManagedDisplaySpaces or SLSGetActiveSpace is missing on this macOS".into());
    }
    let display = main_frame().ok_or("no main display")?;
    let displays = sky.displays();
    let main = main_entry(&displays).ok_or("no entry of SLSCopyManagedDisplaySpaces is the main screen's")?;
    let (main_id, start, spaces) = displays[main].clone();
    let desktops: Vec<u64> = spaces.iter().filter(|s| s.1 == 0).map(|s| s.0).collect();
    let at = desktops.iter().position(|d| *d == start).ok_or("the main screen shows no Desktop (a full-screen app's Space?): show a Desktop first")?;
    let next = if at + 1 < desktops.len() {
        Some((desktops[at + 1], Direction::Right))
    } else if at > 0 {
        Some((desktops[at - 1], Direction::Left))
    } else {
        None
    };
    let first_on = |bundle: &str| -> Result<(i32, u32, Rect, String), String> {
        let (pid, list) = fixture(sky, bundle);
        let pid = pid.ok_or(format!("{bundle} is not running"))?;
        let w = list.iter().find(|w| w.onscreen).ok_or(format!("{bundle} has no window on screen"))?;
        Ok((pid, w.id, w.bounds, super::app_path(pid).ok_or(format!("{bundle}'s app path is unknown"))?))
    };
    let (a_pid, a, a_frame, a_path) = first_on(bundle_a)?;
    let (_, b, b_frame, b_path) = first_on(bundle_b)?;
    println!("== space-flip: the window server's current Desktop during a slide (read every {} ms) ==", EVERY.as_millis());
    println!(
        "  the main screen has {} Desktops; the probe starts on number {}; {} screen(s) in the window server's list",
        desktops.len(),
        at + 1,
        displays.len()
    );
    let setup = Setup { a, a_frame, b, b_frame, display, main_id };
    let moved = match next {
        Some((sid, _)) => {
            let ok = put_on(sky, &[b], sid);
            println!("  Bravo's window moved to the {} Desktop: {}", if next.is_some_and(|n| n.1 == Direction::Right) { "next" } else { "previous" }, if ok { "yes" } else { "NO (the slide cases are skipped)" });
            ok
        }
        None => {
            println!("  SKIPPED the Desktop keys, the activation and Mission Control's pick: the main screen has one Desktop (add one in Mission Control and run again)");
            false
        }
    };
    let label = |id: u64| -> String {
        if id == start {
            "start".into()
        } else if next.is_some_and(|n| n.0 == id) {
            "next".into()
        } else {
            format!("S{id}")
        }
    };
    let _observer = apps::observe_space_changes(|| {
        if let Ok(mut v) = NOTICES.lock() {
            v.push(Instant::now());
        }
    });
    let stop = AtomicBool::new(false);
    let (latest_main, latest_flags) = (AtomicU64::new(start), AtomicU8::new(0));
    let mut legs: Vec<Leg> = Vec::new();
    let (reads, (count, every)) = std::thread::scope(|s| {
        let sampler = s.spawn(|| sample(sky, &setup, &stop, &latest_main, &latest_flags));
        pump(Duration::from_millis(300));
        let leg = |case: String, want: Want, act: &(dyn Fn() -> Result<(Instant, Option<Instant>), String> + Sync)| -> Leg {
            match off_main(act) {
                Ok((cause, escape)) => {
                    let arrived = pump_until(ARRIVE_LIMIT, || want.met(latest_main.load(Ordering::Relaxed), latest_flags.load(Ordering::Relaxed)));
                    pump(TAIL);
                    let leg = Leg { case, cause, escape, end: Instant::now(), want, note: if arrived { String::new() } else { "not arrived in 4 s".into() } };
                    pump(REST);
                    leg
                }
                Err(e) => Leg { case, cause: Instant::now(), escape: None, end: Instant::now(), want, note: e },
            }
        };
        // Back on the start Desktop with Alpha out of full screen: true when it is.
        let recover = || -> bool {
            let home = || latest_main.load(Ordering::Relaxed) == start && latest_flags.load(Ordering::Relaxed) & A_FULL == 0;
            if home() {
                return true;
            }
            if latest_flags.load(Ordering::Relaxed) & A_FULL != 0 {
                let _ = off_main(|| full_screen(a_pid, a, false));
                pump_until(ARRIVE_LIMIT, home);
            }
            if !home() {
                let _ = off_main(|| open_path(&a_path));
                pump_until(ARRIVE_LIMIT, home);
            }
            if !home()
                && let Some((_, d)) = next
            {
                let back = SpaceKey::read(if d == Direction::Right { Direction::Left } else { Direction::Right });
                let _ = off_main(|| post_stamped(&back));
                pump_until(ARRIVE_LIMIT, home);
            }
            pump(REST);
            home()
        };
        let mut lost = false;
        if let (true, Some((sid, d))) = (moved, next) {
            let to = SpaceKey::read(d);
            let back = SpaceKey::read(if d == Direction::Right { Direction::Left } else { Direction::Right });
            let (to_name, back_name) = (format!("Desktop key {} ({})", d.name(), to.describe()), format!("Desktop key back ({})", back.describe()));
            for _ in 0..args.rounds {
                legs.push(leg(to_name.clone(), Want::Space(sid, B_HOME), &|| post_stamped(&to).map(|t| (t, None))));
                legs.push(leg(back_name.clone(), Want::Space(start, A_HOME), &|| post_stamped(&back).map(|t| (t, None))));
                lost |= !recover();
            }
            for _ in 0..args.rounds {
                if lost {
                    break;
                }
                legs.push(leg("Bravo opened (its only window there)".into(), Want::Space(sid, B_HOME), &|| open_path(&b_path).map(|t| (t, None))));
                legs.push(leg("Alpha opened (back)".into(), Want::Space(start, A_HOME), &|| open_path(&a_path).map(|t| (t, None))));
                lost |= !recover();
            }
            if !SpaceKey::read_expose(Expose::MissionControl).enabled {
                println!("  SKIPPED Mission Control's pick: its shortcut is turned off in Keyboard Shortcuts");
            } else {
                for _ in 0..args.rounds {
                    if lost {
                        break;
                    }
                    legs.push(leg("Mission Control, Desktop key inside, Escape".into(), Want::Space(sid, B_HOME), &|| mission_pick(&to, display)));
                    legs.push(leg("Mission Control, key back inside, Escape".into(), Want::Space(start, A_HOME), &|| mission_pick(&back, display)));
                    lost |= !recover();
                }
            }
        }
        for _ in 0..args.rounds {
            if lost {
                break;
            }
            legs.push(leg("Alpha into full screen".into(), Want::Not(start, A_FULL), &|| full_screen(a_pid, a, true).map(|t| (t, None))));
            // The full-screen animation over before the way out.
            pump(Duration::from_millis(500));
            legs.push(leg("Alpha out of full screen".into(), Want::Space(start, A_HOME), &|| full_screen(a_pid, a, false).map(|t| (t, None))));
            lost |= !recover();
        }
        if lost {
            println!("  STOPPED early: the probe could not get back to the start Desktop with Alpha out of full screen");
        }
        stop.store(true, Ordering::Relaxed);
        let out = sampler.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
        (out.0, (out.1, out.2))
    });
    let back = if moved { put_on(sky, &[b], start) } else { true };
    let shown = sky.displays().iter().find(|d| d.0 == setup.main_id).map(|d| d.1) == Some(start);
    println!("  the window server was read {count} times, every {every:.2} ms on average");
    println!("  after: Bravo's window back on the start Desktop: {}; the start Desktop shows: {}", yes(back), yes(shown));
    let notices = NOTICES.lock().map(|v| v.clone()).unwrap_or_default();
    report(&legs, &reads, &notices, &label);
    if legs.is_empty() {
        return Err("no case could run".into());
    }
    Ok(())
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "NO" }
}

/// What one leg measured, ms from its cause.
#[derive(Debug, Default, PartialEq)]
struct Measured {
    flip: Option<f64>,
    active: Option<f64>,
    notice: Option<f64>,
    notices: usize,
    listed: Option<f64>,
    /// The main screen's Current Space at the cause, then each value it took.
    values: Vec<u64>,
    /// A value other than the first and the last, or a return to the first.
    in_between: bool,
    /// Another screen's Current Space changed.
    other_screen: bool,
}

fn measure(leg: &Leg, reads: &[(Instant, Read)], notices: &[Instant]) -> Measured {
    let mut m = Measured::default();
    let Some(before) = reads.iter().rev().find(|(at, _)| *at <= leg.cause).or(reads.first()).map(|(_, r)| r.clone()) else { return m };
    let span: Vec<&(Instant, Read)> = reads.iter().filter(|(at, _)| *at > leg.cause && *at <= leg.end).collect();
    m.values.push(before.main);
    for (_, r) in &span {
        if m.values.last() != Some(&r.main) {
            m.values.push(r.main);
        }
    }
    m.in_between = m.values.len() > 2;
    m.flip = span.iter().find(|(_, r)| r.main != before.main).map(|(at, _)| ms(leg.cause, *at));
    m.active = span.iter().find(|(_, r)| r.active != before.active).map(|(at, _)| ms(leg.cause, *at));
    let after: Vec<&Instant> = notices.iter().filter(|at| **at > leg.cause && **at <= leg.end).collect();
    m.notices = after.len();
    m.notice = after.first().map(|at| ms(leg.cause, **at));
    m.listed = span.iter().find(|(_, r)| r.flags & leg.want.flag() != 0).map(|(at, _)| ms(leg.cause, *at));
    let others = |r: &Read| r.all.iter().copied().filter(|s| *s != r.main).collect::<Vec<_>>();
    m.other_screen = span.iter().any(|(_, r)| others(r) != others(&before));
    m
}

fn report(legs: &[Leg], reads: &[(Instant, Read)], notices: &[Instant], label: &dyn Fn(u64) -> String) {
    let measured: Vec<Measured> = legs.iter().map(|l| measure(l, reads, notices)).collect();
    let rows: Vec<Vec<String>> = legs
        .iter()
        .zip(&measured)
        .map(|(l, m)| {
            let mut note = l.note.clone();
            if m.notices > 1 {
                note.push_str(&format!(" {} notices", m.notices));
            }
            if m.other_screen {
                note.push_str(" another screen's Current Space changed too");
            }
            vec![
                l.case.clone(),
                l.escape.map_or("-".into(), |e| format!("{:.0}", ms(l.cause, e))),
                cell(m.flip),
                cell(m.active),
                cell(m.notice),
                cell(m.listed),
                m.values.iter().map(|v| label(*v)).collect::<Vec<_>>().join("→"),
                note.trim().to_string(),
            ]
        })
        .collect();
    println!("\n  ms from the cause (the key's key-down, the `open`, the Accessibility call); Mission Control's: from the Desktop key inside it");
    table(&["case", "Escape", "→Current Space", "→ActiveSpace", "→notice", "→listed at frame", "Current Space values", "note"], &rows);
    println!();
    let mut cases: Vec<&str> = Vec::new();
    for l in legs {
        if !cases.contains(&l.case.as_str()) {
            cases.push(&l.case);
        }
    }
    let mut sound = true;
    for case in &cases {
        let mine: Vec<&Measured> = legs.iter().zip(&measured).filter(|(l, _)| l.case == *case).map(|(_, m)| m).collect();
        let flips: Vec<f64> = mine.iter().filter_map(|m| m.flip).collect();
        let to_notice: Vec<f64> = mine.iter().filter_map(|m| Some(m.notice? - m.flip?)).collect();
        let to_list: Vec<f64> = mine.iter().filter_map(|m| Some(m.listed? - m.flip?)).collect();
        let active_lag: Vec<f64> = mine.iter().filter_map(|m| Some(m.active? - m.flip?)).collect();
        let between = mine.iter().filter(|m| m.in_between).count();
        if flips.len() < mine.len() || between > 0 {
            sound = false;
        }
        println!(
            "VERDICT {case}: Current Space flipped in {}/{} legs, a median {} ms after the cause; the notice {}; the arriving window listed at its frame {}; SLSGetActiveSpace {}; in-between values in {between} legs",
            flips.len(),
            mine.len(),
            cell(median(&flips)),
            after_words(&to_notice, "the flip"),
            after_words(&to_list, "the flip"),
            after_words(&active_lag, "the flip"),
        );
    }
    let to_notice: Vec<f64> = measured.iter().filter_map(|m| Some(m.notice? - m.flip?)).collect();
    let before = to_notice.iter().filter(|d| **d > 2.0).count();
    let after = to_notice.iter().filter(|d| **d < -2.0).count();
    let at = to_notice.len() - before - after;
    let when = if after == 0 && before > 0 { "before" } else if before == 0 && after > 0 { "after" } else if before == 0 && after == 0 { "at" } else { "before or after (both seen)" };
    println!(
        "VERDICT overall: the flip comes {when} the notice ({before} before, {at} within 2 ms, {after} after, of {} pairs); it {} an in-between value; so the flip {} a sound \"the Desktop is X now\" signal",
        to_notice.len(),
        if measured.iter().any(|m| m.in_between) { "SHOWS" } else { "never shows" },
        if sound && !to_notice.is_empty() { "IS" } else { "is NOT" }
    );
}

/// "a median N ms after X (k later, j within 2 ms, i earlier)" from per-leg differences.
fn after_words(diffs: &[f64], other: &str) -> String {
    let Some(m) = median(diffs) else { return format!("not set against {other} (no pair)") };
    let later = diffs.iter().filter(|d| **d > 2.0).count();
    let earlier = diffs.iter().filter(|d| **d < -2.0).count();
    let with = diffs.len() - later - earlier;
    let order = if m >= 0.0 { format!("a median {m:.0} ms after {other}") } else { format!("a median {:.0} ms BEFORE {other}", -m) };
    format!("{order} ({later} later, {with} within 2 ms, {earlier} earlier)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(main: u64, active: u64, flags: u8) -> Read {
        Read { main, all: vec![main], active, flags }
    }

    #[test]
    fn a_leg_is_measured_from_its_cause() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let leg = Leg { case: "k".into(), cause: at(100), escape: None, end: at(2000), want: Want::Space(7, B_HOME), note: String::new() };
        let reads = vec![(at(0), r(5, 5, A_HOME)), (at(150), r(5, 7, 0)), (at(160), r(7, 7, 0)), (at(400), r(7, 7, B_HOME))];
        let notices = vec![at(50), at(180)];
        let m = measure(&leg, &reads, &notices);
        assert_eq!((m.flip, m.active, m.notice, m.listed), (Some(60.0), Some(50.0), Some(80.0), Some(300.0)));
        assert_eq!((m.values.clone(), m.in_between, m.notices), (vec![5, 7], false, 1));
    }

    #[test]
    fn an_in_between_value_is_seen() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let leg = Leg { case: "k".into(), cause: at(0), escape: None, end: at(1000), want: Want::Not(5, A_FULL), note: String::new() };
        let reads = vec![(at(0), r(5, 5, 0)), (at(10), r(9, 9, 0)), (at(20), r(11, 11, A_FULL))];
        let m = measure(&leg, &reads, &[]);
        assert_eq!(m.values, vec![5, 9, 11]);
        assert!(m.in_between);
        assert!(Want::Not(5, A_FULL).met(11, A_FULL) && !Want::Space(5, A_HOME).met(11, A_HOME));
    }

    #[test]
    fn differences_read_as_words() {
        assert!(after_words(&[10.0, 20.0, 30.0], "the flip").starts_with("a median 20 ms after the flip (3 later"));
        assert!(after_words(&[-5.0], "the flip").contains("BEFORE"));
        assert!(after_words(&[], "x").contains("no pair"));
    }
}
