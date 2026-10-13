//! `stage-manager probe-park <fixture bundle id> [--corner bottom-right|bottom-left]
//! [--settle MS]`: measures what macOS keeps on screen of a parked window for each way of
//! asking the park frame ([`ParkRequest`]), on the fixture's windows only.
//!
//! For each standard window of the fixture app it asks a grid of requests (points kept
//! inside the corner's side: 1, 0, -1, -50, -2000; the top below the bottom edge: -1, 0, 50,
//! 2000) the way a park does (a position only), waits for the window server, and records
//! where the window landed, whether it is still listed on screen, and how much of it shows;
//! then the window goes back to its frame. The reply (JSON on stdout) carries every sample,
//! each window's best request, and [`best_request`] over all of them, with the config lines
//! that would use it. `scripts/probe_park.py` opens the fixture with every window chrome and
//! runs this.
//!
//! Refuses any bundle id but a fixture's (`dev.eugene.stagemanager.fixture…`): it moves
//! windows. Needs Accessibility for the process that runs it.

use std::process::ExitCode;
use std::time::Duration;

use objc2_app_kit::NSRunningApplication;
use objc2_foundation::{MainThreadMarker, NSString};
use serde_json::{Value, json};

use super::ax::{AxElement, names};
use super::{desktop, permissions, windows};
use crate::engine::geometry::{Point, Rect};
use crate::engine::park::{Corner, ParkRequest, ProbeSample, best_request, park_frame_asked, shown_area};

/// The bundle id prefix of the fixture apps (scripts/bundle.sh, scripts/test-stages.sh).
pub const FIXTURE_PREFIX: &str = "dev.eugene.stagemanager.fixture";

/// The requests asked, by points inside the side and by drop below the bottom edge.
const INSIDE: [f64; 5] = [1.0, 0.0, -1.0, -50.0, -2000.0];
const DROP: [f64; 4] = [-1.0, 0.0, 50.0, 2000.0];

fn fail(code: u8, error: &str) -> ExitCode {
    println!("{}", json!({ "ok": false, "error": error }));
    ExitCode::from(code)
}

/// Runs the probe (see the module docs).
pub fn run(args: &[String]) -> ExitCode {
    let mut bundle = None;
    let mut corner = Corner::BottomRight;
    let mut settle = Duration::from_millis(250);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--corner" => match it.next().map(String::as_str) {
                Some("bottom-right") => corner = Corner::BottomRight,
                Some("bottom-left") => corner = Corner::BottomLeft,
                _ => return fail(2, "--corner takes bottom-right or bottom-left"),
            },
            "--settle" => match it.next().and_then(|v| v.parse::<u64>().ok()) {
                Some(ms) => settle = Duration::from_millis(ms.clamp(20, 5000)),
                None => return fail(2, "--settle takes milliseconds"),
            },
            b if bundle.is_none() => bundle = Some(b.to_string()),
            _ => return fail(2, "usage: stage-manager probe-park <fixture bundle id> [--corner bottom-right|bottom-left] [--settle MS]"),
        }
    }
    let Some(bundle) = bundle else { return fail(2, "usage: stage-manager probe-park <fixture bundle id>") };
    if !bundle.starts_with(FIXTURE_PREFIX) {
        return fail(2, "probe-park moves windows: only a fixture app's (dev.eugene.stagemanager.fixture…)");
    }
    if !permissions::accessibility_trusted() {
        return fail(3, "Accessibility is not granted to this process (or the terminal that runs it)");
    }
    let main = MainThreadMarker::new().and_then(desktop::main_display);
    let Some(main) = main else { return fail(4, "no main display") };
    let (display, visible) = (main.frame, main.visible);
    let pids: Vec<i32> = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(&bundle))
        .iter()
        .map(|a| a.processIdentifier())
        .collect();
    if pids.is_empty() {
        return fail(5, "the fixture is not running");
    }
    let mut out = Vec::new();
    let mut all: Vec<Vec<ProbeSample>> = Vec::new();
    for pid in pids {
        let Ok(list) = AxElement::application(pid).windows() else { continue };
        for w in list {
            if w.subrole().ok().as_deref() != Some(names::SUBROLE_STANDARD) {
                continue;
            }
            let (Ok(id), Ok(orig)) = (w.window_id(), w.frame()) else { continue };
            let title = w.title().unwrap_or_default();
            let (samples, json_samples) = probe_window(&w, id, orig, display, corner, settle);
            if let Err(e) = w.set_frame(orig) {
                eprintln!("window {id}: cannot put it back at {orig}: {e}");
            }
            std::thread::sleep(settle);
            let back = w.frame().ok();
            let mine = best_request(std::slice::from_ref(&samples), display, visible, corner);
            out.push(json!({
                "id": id,
                "title": title,
                "frame": orig,
                "back": back,
                "samples": json_samples,
                "best": mine.map(|(r, a)| json!({ "inside": r.inside, "drop": r.drop, "shown_area": a })),
            }));
            all.push(samples);
        }
    }
    let first = best_request(
        &all.iter().map(|w| w.iter().copied().filter(|s| s.req == ParkRequest::FIRST).collect()).collect::<Vec<_>>(),
        display,
        visible,
        corner,
    );
    let best = best_request(&all, display, visible, corner);
    let config = best.map(|(r, _)| format!("[windows]\npark_inside = {:?}\npark_drop = {:?}", r.inside, r.drop));
    println!(
        "{}",
        json!({
            "ok": true,
            "bundle": bundle,
            "display": display,
            "visible": visible,
            "corner": corner,
            "windows": out,
            "first_request_worst_area": first.map(|f| f.1),
            "best": best.map(|(r, a)| json!({ "inside": r.inside, "drop": r.drop, "worst_area": a })),
            "config": config,
        })
    );
    ExitCode::SUCCESS
}

/// Asks every request of the grid of window `id` (frame `orig`) and records where it landed.
fn probe_window(
    w: &AxElement,
    id: u32,
    orig: Rect,
    display: Rect,
    corner: Corner,
    settle: Duration,
) -> (Vec<ProbeSample>, Vec<Value>) {
    let mut samples = Vec::new();
    let mut json_samples = Vec::new();
    for inside in INSIDE {
        for drop in DROP {
            let req = ParkRequest { inside, drop };
            let asked = park_frame_asked(orig.size(), display, corner, req);
            let set = w.set_position(Point::new(asked.x, asked.y));
            std::thread::sleep(settle);
            let listed = windows::list_windows().into_iter().find(|c| c.id == id);
            let ax = w.frame().ok();
            let (landed, onscreen) = match &listed {
                Some(c) => (c.bounds, c.onscreen),
                None => (ax.unwrap_or(asked), false),
            };
            let seen = landed.intersect(&display);
            json_samples.push(json!({
                "inside": inside,
                "drop": drop,
                "asked": asked,
                "set": set.err().map(|e| e.to_string()),
                "landed": landed,
                "ax": ax,
                "onscreen": onscreen,
                "listed": listed.is_some(),
                "shown_w": seen.map_or(0.0, |r| r.w),
                "shown_h": seen.map_or(0.0, |r| r.h),
                "shown_area": shown_area(landed, display),
            }));
            samples.push(ProbeSample { req, landed, onscreen });
        }
    }
    (samples, json_samples)
}
