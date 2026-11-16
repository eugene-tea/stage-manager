//! `stage-manager probe-spaces pixels …` (round landing-order): what the screen shows of a
//! few fixture windows in a small region through a switch, read every few ms. Reads only: no
//! window is moved.
//!
//! The region is captured from its display through ScreenCaptureKit with only the named
//! windows included (each must belong to a fixture app: nothing of anyone else's is ever
//! read, our own motion overlay neither), so each read is the windows as the window server
//! composites them there, or nothing (alpha 0) where none of them shows. A capture is asked
//! every `--every` ms without waiting for the one before; each answer is kept with when it
//! was asked and when it came, in ms from the start of the command after `--then` (a card
//! click), which starts after a few asks.
//!
//! `pixels --rect X,Y,W,H --windows ID,ID… [--every MS] [--for MS] --then CMD ARG…`: one JSON
//! object on stdout: `samples` [{ `asked_ms`, `at_ms`, `rgba` }] in the order answered, the
//! command's own reply (its last line) as `reply`.

use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGImage, kCGColorClear};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration, SCWindow};
use serde_json::{Value, json};

use super::park_probe::FIXTURE_PREFIX;
use super::windows;
use crate::engine::geometry::Rect;

/// Captures asked before the command starts (the region before the switch).
const WARM_ASKS: u32 = 5;

/// How long the answers still out are waited for after the span.
const DRAIN: Duration = Duration::from_millis(600);

struct Opts {
    rect: Rect,
    windows: Vec<u32>,
    every: Duration,
    span: Duration,
    then: Vec<String>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { rect: Rect::new(0.0, 0.0, 0.0, 0.0), windows: Vec::new(), every: Duration::from_millis(10), span: Duration::from_millis(1500), then: Vec::new() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--rect" => {
                let v: Vec<f64> = value()?.split(',').map(|p| p.trim().parse::<f64>().map_err(|_| "--rect takes X,Y,W,H".to_string())).collect::<Result<_, _>>()?;
                let [x, y, w, h] = v[..] else { return Err("--rect takes X,Y,W,H".into()) };
                if w < 1.0 || h < 1.0 {
                    return Err("--rect is empty".into());
                }
                o.rect = Rect::new(x, y, w, h);
            }
            "--windows" => o.windows = value()?.split(',').filter(|p| !p.trim().is_empty()).map(|p| p.trim().parse::<u32>().map_err(|_| "--windows takes window ids".to_string())).collect::<Result<_, _>>()?,
            "--every" => o.every = Duration::from_millis(value()?.parse::<u64>().map_err(|_| "--every takes ms".to_string())?.max(1)),
            "--for" => o.span = Duration::from_millis(value()?.parse().map_err(|_| "--for takes ms".to_string())?),
            "--then" => {
                o.then = it.by_ref().cloned().collect();
                break;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if o.rect.w < 1.0 {
        return Err("--rect is missing".into());
    }
    if o.windows.is_empty() {
        return Err("--windows names no window".into());
    }
    if o.then.is_empty() {
        return Err("--then names no command".into());
    }
    Ok(o)
}

/// True when every window of `ids` is listed and belongs to a fixture app.
fn fixtures_only(ids: &[u32]) -> Result<(), String> {
    let listed = windows::describe(ids);
    for id in ids {
        let Some(w) = listed.iter().find(|w| w.id == *id) else { return Err(format!("window {id} is not listed")) };
        let bundle = NSRunningApplication::runningApplicationWithProcessIdentifier(w.pid).and_then(|a| a.bundleIdentifier()).map(|b| b.to_string()).unwrap_or_default();
        if !bundle.starts_with(FIXTURE_PREFIX) {
            return Err(format!("window {id} is not a fixture's"));
        }
    }
    Ok(())
}

/// The filter and configuration of the region: its display, only these windows.
struct Shot {
    filter: Retained<SCContentFilter>,
    config: Retained<SCStreamConfiguration>,
    _clear: Option<CFRetained<CGColor>>,
}

// SAFETY: ScreenCaptureKit's filter and configuration are immutable after they are built
// here and only handed to its own calls, which copy what they need.
unsafe impl Send for Shot {}
// SAFETY: as above.
unsafe impl Sync for Shot {}

fn shot(rect: Rect, ids: &[u32]) -> Result<Shot, String> {
    let (tx, rx) = mpsc::channel::<Result<Shot, String>>();
    let ids = ids.to_vec();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        // SAFETY: SCK passes a valid object or null for each argument.
        let (content, error) = unsafe { (content.as_ref(), error.as_ref()) };
        let Some(content) = content else {
            let why = error.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
            let _ = tx.send(Err(format!("no shareable content: {why}")));
            return;
        };
        let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        // SAFETY: plain getters on live SCK objects.
        let display = unsafe { content.displays() }.iter().find(|d| {
            let f = unsafe { d.frame() };
            cx >= f.origin.x && cx < f.origin.x + f.size.width && cy >= f.origin.y && cy < f.origin.y + f.size.height
        });
        let Some(display) = display else {
            let _ = tx.send(Err("the region is on no display".into()));
            return;
        };
        // SAFETY: plain getters on live SCWindows.
        let included: Vec<Retained<SCWindow>> = unsafe { content.windows() }.iter().filter(|w| ids.contains(&unsafe { w.windowID() })).collect();
        if included.len() != ids.len() {
            let _ = tx.send(Err(format!("{} of {} windows in the shareable content", included.len(), ids.len())));
            return;
        }
        let included = NSArray::from_retained_slice(&included);
        // SAFETY: a constant name CoreGraphics defines.
        let clear = CGColor::constant_color(Some(unsafe { kCGColorClear }));
        // SAFETY: designated initializer with a live SCDisplay and array; plain setters.
        let (filter, config) = unsafe {
            let filter = SCContentFilter::initWithDisplay_includingWindows(SCContentFilter::alloc(), &display, &included);
            let scale = f64::from(filter.pointPixelScale()).max(1.0);
            let frame = display.frame();
            let config = SCStreamConfiguration::new();
            config.setSourceRect(CGRect::new(CGPoint::new(rect.x - frame.origin.x, rect.y - frame.origin.y), CGSize::new(rect.w, rect.h)));
            config.setWidth((rect.w * scale).round().max(1.0) as usize);
            config.setHeight((rect.h * scale).round().max(1.0) as usize);
            config.setShowsCursor(false);
            if let Some(clear) = clear.as_deref() {
                config.setBackgroundColor(clear);
            }
            (filter, config)
        };
        let _ = tx.send(Ok(Shot { filter, config, _clear: clear }));
    });
    // SAFETY: the handler matches the documented signature; SCK copies the block. Windows off
    // screen are listed too: the incoming ones wait hidden when this is asked.
    unsafe { SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true, false, &handler) };
    rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "the shareable content did not answer".to_string())?
}

/// One answered capture.
#[derive(Debug, Clone, Copy)]
struct Sample {
    asked: Instant,
    at: Instant,
    rgba: Option<[f64; 4]>,
}

fn ask(shot: &Shot, out: &Arc<Mutex<Vec<Sample>>>) {
    let asked = Instant::now();
    let out = out.clone();
    let on_image = RcBlock::new(move |image: *mut CGImage, _error: *mut NSError| {
        let at = Instant::now();
        // SAFETY: SCK hands a valid CGImage (or null) for the duration of the block.
        let rgba = unsafe { image.as_ref() }.and_then(super::pixels::mean_rgba);
        if let Ok(mut v) = out.lock() {
            v.push(Sample { asked, at, rgba });
        }
    });
    // SAFETY: filter, configuration and handler are valid; SCK copies the block.
    unsafe { SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(&shot.filter, &shot.config, Some(&on_image)) };
}

fn ms_since(t0: Option<Instant>, t: Instant) -> Value {
    match t0 {
        Some(t0) if t >= t0 => json!((t.duration_since(t0).as_secs_f64() * 1e4).round() / 10.0),
        Some(t0) => json!(-((t0.duration_since(t).as_secs_f64() * 1e4).round() / 10.0)),
        None => Value::Null,
    }
}

/// Runs `probe-spaces pixels` (see the module docs).
pub fn run(args: &[String]) -> ExitCode {
    let usage = "usage: probe-spaces pixels --rect X,Y,W,H --windows ID,ID… [--every MS] [--for MS] --then CMD ARG…";
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            println!("{}", json!({ "ok": false, "error": format!("{e}; {usage}") }));
            return ExitCode::from(2);
        }
    };
    if let Err(e) = fixtures_only(&o.windows) {
        println!("{}", json!({ "ok": false, "error": e }));
        return ExitCode::from(2);
    }
    let shot = match shot(o.rect, &o.windows) {
        Ok(s) => s,
        Err(e) => {
            println!("{}", json!({ "ok": false, "error": e }));
            return ExitCode::FAILURE;
        }
    };
    let samples: Arc<Mutex<Vec<Sample>>> = Arc::new(Mutex::new(Vec::new()));
    let start = Instant::now();
    let (mut t0, mut child, mut asks, mut due) = (None::<Instant>, None, 0u32, start);
    loop {
        ask(&shot, &samples);
        asks += 1;
        if t0.is_none() && asks >= WARM_ASKS {
            let mut cmd = Command::new(&o.then[0]);
            cmd.args(&o.then[1..]).stdout(Stdio::piped()).stderr(Stdio::null());
            t0 = Some(Instant::now());
            match cmd.spawn() {
                Ok(c) => child = Some(c),
                Err(e) => {
                    println!("{}", json!({ "ok": false, "error": format!("the command did not start: {e}") }));
                    return ExitCode::FAILURE;
                }
            }
        }
        if t0.is_some_and(|t0| t0.elapsed() >= o.span) {
            break;
        }
        due += o.every;
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        } else {
            due = now;
        }
    }
    let by = Instant::now() + DRAIN;
    while Instant::now() < by && samples.lock().map_or(0, |v| v.len()) < asks as usize {
        std::thread::sleep(Duration::from_millis(5));
    }
    let reply = child
        .and_then(|c| c.wait_with_output().ok())
        .map(|out| String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or_default().to_string())
        .unwrap_or_default();
    let reply: Value = serde_json::from_str(&reply).unwrap_or(Value::String(reply));
    let got = samples.lock().map(|v| v.clone()).unwrap_or_default();
    let rows: Vec<Value> = got
        .iter()
        .map(|s| json!({ "asked_ms": ms_since(t0, s.asked), "at_ms": ms_since(t0, s.at), "rgba": s.rgba.map(|c| c.map(|v| v.round())) }))
        .collect();
    println!("{}", json!({ "ok": true, "asked": asks, "answered": got.len(), "rect": o.rect, "windows": o.windows, "samples": rows, "reply": reply }));
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn options_parse_and_refuse() {
        let o = parse(&args("--rect 10,20,24,24 --windows 5,6 --every 12 --for 900 --then ctl debug-click 5 5")).expect("parsed");
        assert_eq!(o.rect, Rect::new(10.0, 20.0, 24.0, 24.0));
        assert_eq!(o.windows, vec![5, 6]);
        assert_eq!((o.every.as_millis(), o.span.as_millis(), o.then.len()), (12, 900, 4));
        assert!(parse(&args("--windows 5 --then ctl state")).is_err(), "no region");
        assert!(parse(&args("--rect 1,2,0,4 --windows 5 --then ctl state")).is_err(), "an empty region");
        assert!(parse(&args("--rect 1,2,3 --windows 5 --then ctl state")).is_err());
        assert!(parse(&args("--rect 1,2,3,4 --then ctl state")).is_err(), "no window");
        assert!(parse(&args("--rect 1,2,3,4 --windows x --then ctl state")).is_err());
        assert!(parse(&args("--rect 1,2,3,4 --windows 5")).is_err(), "no command");
    }

    #[test]
    fn times_count_from_the_command() {
        let t0 = Instant::now();
        assert_eq!(ms_since(None, t0), Value::Null);
        assert_eq!(ms_since(Some(t0), t0 + Duration::from_millis(15)), json!(15.0));
        assert_eq!(ms_since(Some(t0 + Duration::from_millis(5)), t0), json!(-5.0));
    }
}
