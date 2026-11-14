//! `probe-spaces capture BUNDLE [--rounds N] [hidden file=PATH]` (1.9, S4b): what taking a
//! switch's pictures at the switch would cost, through the app's own path
//! ([`crate::platform::switch_shots`]: one listing of the shareable content, then a still per
//! window, in parallel, at full size).
//!
//! - Without `hidden`: for the first 1, 2, … of the fixture's windows (window id order), N
//!   rounds each of a capture of those windows from the call to the last picture, and N
//!   listings of the shareable content alone; the windows stay where they are.
//! - With `hidden` (the fixture started with `--color-file PATH --no-label`): the windows
//!   captured on screen, then hidden by the Spaces engine itself, captured there, the colour
//!   file turned dark and the windows captured again (does ScreenCaptureKit hand back a
//!   hidden window's current content?), N timed rounds of the hidden windows, the colour
//!   light again and a last capture, then the engine's bring-back.
//!
//! Each picture's middle (64 px a side at most) is read for its mean brightness.

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Opts, Sky, fixture_pids, fixture_windows, frames, ids_of, ms, pump, wait_onscreen};
use crate::platform::capture::Target;
use crate::platform::capture_pixels::{self, PixelRect};
use crate::platform::switch_shots;
use crate::platform::windows;

/// One window's answer: the window, ms from the call, the picture's size and middle mean.
type Answer = (u32, f64, Option<(usize, usize, Option<u8>)>);

/// How long one capture may take before the probe stops waiting for it.
const SHOT_LIMIT: Duration = Duration::from_secs(3);

/// Runs the probe (module docs). `scale`: the main screen's pixels per point.
pub(super) fn run(sky: &Sky, bundle: &str, o: &Opts, scale: f64) -> Result<Value, String> {
    if !objc2_core_graphics::CGPreflightScreenCaptureAccess() {
        return Err("no Screen Recording access for this process (run it through `open` from the dev app)".into());
    }
    let pids = fixture_pids(bundle);
    let list = fixture_windows(sky, &pids);
    let mut ids = ids_of(&list);
    ids.sort_unstable();
    if ids.is_empty() {
        return Err("the fixture has no window".into());
    }
    let targets: Vec<Target> = ids
        .iter()
        .filter_map(|id| list.iter().find(|w| w.id == *id))
        .map(|w| Target {
            id: w.id,
            width: (w.bounds.w * scale).round().max(1.0) as u32,
            height: (w.bounds.h * scale).round().max(1.0) as u32,
        })
        .collect();
    let hidden = o.words.iter().any(|w| w == "hidden");
    let cached = o.words.iter().any(|w| w == "cached");
    let file = o.words.iter().find_map(|w| w.strip_prefix("file=").map(String::from));
    let rounds = o.rounds;
    let windows_json = frames(&list);
    let worker = std::thread::spawn(move || {
        if cached {
            let file = file.ok_or("cached takes file=PATH (the fixture's --color-file)")?;
            Ok(cached_rounds(&targets, &file, rounds, hidden))
        } else if hidden {
            let file = file.ok_or("hidden takes file=PATH (the fixture's --color-file)")?;
            Ok(hidden_rounds(&targets, &file, rounds))
        } else {
            Ok(shown_rounds(&targets, rounds))
        }
    });
    while !worker.is_finished() {
        pump(Duration::from_millis(5));
    }
    let out: Result<Value, String> = worker.join().map_err(|_| "the probe's thread panicked".to_string())?;
    out.map(|v| json!({ "ok": true, "scale": scale, "windows": windows_json, "result": v }))
}

/// Captures `targets` once through the app's path; every answer, in arrival order.
fn shoot(targets: &[Target]) -> (f64, Vec<Answer>) {
    shoot_with(targets, None)
}

/// A listing kept for later captures.
struct Kept(objc2::rc::Retained<objc2_screen_capture_kit::SCShareableContent>);
// SAFETY: a listing is an immutable snapshot; the probe only reads it, from one thread at a time.
unsafe impl Send for Kept {}

/// The shareable content listed once and kept.
fn keep_listing() -> Option<Kept> {
    let (tx, rx) = mpsc::channel::<Kept>();
    let tx = std::sync::Mutex::new(tx);
    let handler = block2::RcBlock::new(move |c: *mut objc2_screen_capture_kit::SCShareableContent, _e: *mut objc2_foundation::NSError| {
        // SAFETY: SCK passes a valid object or null; retaining keeps it past the block.
        if let Some(c) = unsafe { objc2::rc::Retained::retain(c) }
            && let Ok(tx) = tx.lock()
        {
            let _ = tx.send(Kept(c));
        }
    });
    // SAFETY: the handler matches the documented signature; SCK copies the block.
    unsafe {
        objc2_screen_capture_kit::SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, false, &handler,
        );
    }
    rx.recv_timeout(SHOT_LIMIT).ok()
}

/// [`shoot`], with a kept listing instead of a new one when given.
fn shoot_with(targets: &[Target], kept: Option<&Kept>) -> (f64, Vec<Answer>) {
    let (tx, rx) = mpsc::channel::<Answer>();
    let t0 = Instant::now();
    let tx = std::sync::Mutex::new(tx);
    let done: switch_shots::ShotDone = Arc::new(move |id, picture: Option<crate::platform::capture::Picture>| {
            let at = ms(t0);
            let read = picture.map(|p| {
                let (w, h) = (objc2_core_graphics::CGImage::width(Some(&p.image)), objc2_core_graphics::CGImage::height(Some(&p.image)));
                let side = 64.min(w).min(h);
                let middle = PixelRect { x: (w - side) / 2, y: (h - side) / 2, w: side, h: side };
                let mean = capture_pixels::box_stats(&p.image, &[middle]).first().copied().flatten().map(|s| s.mean);
                (w, h, mean)
            });
            if let Ok(tx) = tx.lock() {
                let _ = tx.send((id, at, read));
            }
        });
    match kept {
        Some(k) => switch_shots::shoot_from(&k.0, targets, &done),
        None => switch_shots::issue(targets.to_vec(), done),
    }
    let mut got = Vec::new();
    while got.len() < targets.len() {
        match rx.recv_timeout(SHOT_LIMIT) {
            Ok(a) => got.push(a),
            Err(_) => break,
        }
    }
    let total = got.iter().map(|a| a.1).fold(0.0, f64::max);
    (total, got)
}

/// The shareable content listed once (what every capture starts with); ms.
fn list_once() -> Option<f64> {
    let (tx, rx) = mpsc::channel::<f64>();
    let t0 = Instant::now();
    let tx = std::sync::Mutex::new(tx);
    let handler = block2::RcBlock::new(move |_c: *mut objc2_screen_capture_kit::SCShareableContent, _e: *mut objc2_foundation::NSError| {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(ms(t0));
        }
    });
    // SAFETY: the handler matches the documented signature; SCK copies the block.
    unsafe {
        objc2_screen_capture_kit::SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, false, &handler,
        );
    }
    rx.recv_timeout(SHOT_LIMIT).ok()
}

/// Median, 90th percentile and largest of `v`, rounded to 0.1.
fn spread(v: &[f64]) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let at = |q: f64| (s[((s.len() - 1) as f64 * q).round() as usize] * 10.0).round() / 10.0;
    json!({ "n": s.len(), "median": at(0.5), "p90": at(0.9), "max": at(1.0), "min": at(0.0) })
}

fn shown_rounds(targets: &[Target], rounds: usize) -> Value {
    // One capture first: ScreenCaptureKit's first call of a process is slower.
    let _ = shoot(targets);
    let mut per_count = Vec::new();
    for k in 1..=targets.len() {
        let (mut totals, mut missing) = (Vec::new(), 0);
        for _ in 0..rounds {
            let (total, got) = shoot(&targets[..k]);
            missing += k - got.iter().filter(|a| a.2.is_some()).count();
            totals.push(total);
            std::thread::sleep(Duration::from_millis(150));
        }
        let px: Vec<(usize, usize)> = targets[..k].iter().map(|t| (t.width as usize, t.height as usize)).collect();
        per_count.push(json!({ "windows": k, "pixels": px, "ms": spread(&totals), "missing": missing }));
    }
    let listings: Vec<f64> = (0..rounds)
        .filter_map(|_| {
            let l = list_once();
            std::thread::sleep(Duration::from_millis(100));
            l
        })
        .collect();
    json!({ "captures": per_count, "listing_ms": spread(&listings) })
}

/// One capture's answers as `[id, ms, mean]` rows.
fn rows(got: &[Answer]) -> Value {
    json!(got.iter().map(|(id, at, r)| json!([id, at, r.and_then(|r| r.2)])).collect::<Vec<_>>())
}

fn hidden_rounds(targets: &[Target], file: &str, rounds: usize) -> Value {
    let ids: Vec<u32> = targets.iter().map(|t| t.id).collect();
    let paint = |c: &str| {
        let _ = std::fs::write(file, c);
        // The fixture reads the file every 200 ms.
        std::thread::sleep(Duration::from_millis(700));
    };
    paint("f0f0f0");
    let (_, shown_light) = shoot(targets);
    let engine = crate::platform::spaces::engine::shared();
    let t0 = Instant::now();
    let hid: Vec<bool> = ids.iter().map(|id| engine.hide_window(*id)).collect();
    let hide_ms = ms(t0);
    let (off_ms, _) = wait_onscreen(&ids, false, Duration::from_millis(500));
    let (_, hidden_light) = shoot(targets);
    paint("101010");
    let (_, hidden_dark) = shoot(targets);
    let mut totals = Vec::new();
    for _ in 0..rounds {
        let (total, _) = shoot(targets);
        totals.push(total);
        std::thread::sleep(Duration::from_millis(150));
    }
    paint("f0f0f0");
    let (_, hidden_light_again) = shoot(targets);
    let still_hidden = windows::describe(&ids).iter().all(|w| !w.onscreen);
    let swept = engine.sweep("probe", true);
    let (back_ms, after) = wait_onscreen(&ids, true, Duration::from_millis(500));
    json!({
        "hidden": hid,
        "hide_ms": hide_ms,
        "off_screen_ms": off_ms,
        "shown_light": rows(&shown_light),
        "hidden_light": rows(&hidden_light),
        "hidden_dark": rows(&hidden_dark),
        "hidden_light_again": rows(&hidden_light_again),
        "hidden_all_ms": spread(&totals),
        "stayed_hidden": still_hidden,
        "sweep": { "restored": swept.restored, "failed": swept.failed, "on_screen_ms": back_ms },
        "after": frames(&after),
    })
}

/// Captures with a listing made once, before: each round the colour flips, the listing ages
/// (0.7 s more a round) and the windows' middles must follow the colour (`hidden`: the
/// windows in a stage Space meanwhile).
fn cached_rounds(targets: &[Target], file: &str, rounds: usize, hidden: bool) -> Value {
    let ids: Vec<u32> = targets.iter().map(|t| t.id).collect();
    let engine = crate::platform::spaces::engine::shared();
    let Some(kept) = keep_listing() else { return json!({ "error": "no listing" }) };
    if hidden {
        for id in &ids {
            engine.hide_window(*id);
        }
        let _ = wait_onscreen(&ids, false, Duration::from_millis(500));
    }
    let (mut one, mut all, mut wrong) = (Vec::new(), Vec::new(), 0);
    let mut seen = Vec::new();
    for r in 0..rounds {
        let (colour, want) = if r % 2 == 0 { ("101010", 16u8) } else { ("f0f0f0", 240u8) };
        let _ = std::fs::write(file, colour);
        std::thread::sleep(Duration::from_millis(700));
        let (t1, g1) = shoot_with(&targets[..1], Some(&kept));
        std::thread::sleep(Duration::from_millis(100));
        let (tn, gn) = shoot_with(targets, Some(&kept));
        one.push(t1);
        all.push(tn);
        let means: Vec<Option<u8>> = g1.iter().chain(&gn).map(|a| a.2.and_then(|r| r.2)).collect();
        wrong += means.iter().filter(|m| m.is_none_or(|m| m.abs_diff(want) > 8)).count();
        seen.push(json!([want, means]));
    }
    let swept = hidden.then(|| engine.sweep("probe", true).restored);
    let (_, fresh) = shoot(targets);
    json!({ "hidden": hidden, "one_ms": spread(&one), "all_ms": spread(&all), "wrong": wrong, "means": seen, "swept": swept, "fresh_listing_all": rows(&fresh) })
}
