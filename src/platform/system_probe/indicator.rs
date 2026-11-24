//! `probe-spaces indicator BUNDLE [--run S] [--rest S] [--by-eye]` (probe 4, the review's
//! "what can still start ScreenCaptureKit"): which of the captures the app has lights macOS's
//! purple screen-recording indicator in the menu bar?
//!
//! The fixture's first window stays on screen; its second is hidden in a stage Space by the
//! Spaces engine itself ([`crate::platform::spaces::engine`], as `probe-spaces capture
//! hidden` does) and brought back at the end. Then, after a rest, each kind runs `--run`
//! seconds (default 5) on both windows, with `--rest` seconds (default 5) of rest after:
//!
//! 1. `SLSHWCaptureWindowList`, every 100 ms (the app's fast route);
//! 2. `CGWindowListCreateImage`, every 100 ms (the app's whole-window route);
//! 3. a ScreenCaptureKit still, every 500 ms (a listing and a still per window, through the
//!    app's own switch path, [`crate::platform::switch_shots::issue`]);
//! 4. a ScreenCaptureKit stream of the shown window (the app has none; the probe's own, a
//!    small frame, no output read).
//!
//! The signatures of the two window-server calls are the ones [`crate::platform::capture_fast`]
//! names (WebKit's `CoreGraphicsSPI.h`, alt-tab-macos, DockDoor; `CGWindow.h`).
//!
//! Meanwhile a thread reads the on-screen window list every 100 ms for menu-bar-level windows
//! (layer 24 and up, under 80 points tall: owner, layer, size) and reports any that appears or
//! goes while a kind runs and not in the rest before it. A kind with such a change "lights
//! something"; one without is "could not tell from the list", and those (every kind with
//! `--by-eye`) run again one by one with a 3 s countdown, the owner watching the menu bar.
//!
//! The captures need Screen Recording (the probe checks and never asks); the ScreenCaptureKit
//! kinds may raise macOS's screen-capture alert, so the script runs this probe only by name.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, define_class, msg_send};
use objc2_app_kit::NSScreen;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGImage, CGPreflightScreenCaptureAccess};
use objc2_foundation::{NSError, NSObject};
use objc2_screen_capture_kit::{SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType};

use super::{Args, Ctx, fixture, off_main, pump, table};
use crate::platform::capture::Target;
use crate::platform::spaces::ffi::{self, SKYLIGHT};
use crate::platform::switch_shots;
use crate::platform::windows;

/// `SLSHWCaptureWindowList(cid, wids, count, options)`: a +1 CFArray of CGImage, or null.
type FnHw = unsafe extern "C" fn(i32, *const u32, u32, u32) -> *mut c_void;
/// `CGWindowListCreateImage(rect, list option, wid, image option)`: a +1 CGImage, or null.
type FnListImage = unsafe extern "C" fn(CGRect, u32, u32, u32) -> *mut c_void;
/// The options the app passes ([`crate::platform::capture_fast`]): best resolution and the
/// window's whole shape; a list of one window, framing ignored at the best resolution.
const HW_OPTIONS: u32 = (1 << 8) | (1 << 11);
const LIST_INCLUDING_WINDOW: u32 = 1 << 3;
const IMAGE_OPTIONS: u32 = (1 << 0) | (1 << 3);

/// The menu bar's window list read interval.
const EVERY: Duration = Duration::from_millis(100);
/// A ScreenCaptureKit answer waited for this long at most.
const ANSWER_LIMIT: Duration = Duration::from_secs(3);
/// The by-eye countdown.
const COUNTDOWN: u64 = 3;

/// A menu-bar-level window as the list shows it (no title).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Item {
    owner: String,
    layer: i64,
    w: i64,
    h: i64,
}

impl Item {
    fn describe(&self) -> String {
        format!("{} layer {} {}x{}", self.owner, self.layer, self.w, self.h)
    }
}

/// The kinds, in the order they run.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Hw,
    ListImage,
    SckStill,
    SckStream,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::Hw, Kind::ListImage, Kind::SckStill, Kind::SckStream];

    fn name(self) -> &'static str {
        match self {
            Kind::Hw => "SLSHWCaptureWindowList",
            Kind::ListImage => "CGWindowListCreateImage",
            Kind::SckStill => "ScreenCaptureKit still",
            Kind::SckStream => "ScreenCaptureKit stream",
        }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the class has no ivars and no Drop.
    #[unsafe(super = NSObject)]
    #[name = "SMProbeStreamOutput"]
    struct StreamOutput;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for StreamOutput {}

    // SAFETY: the protocol's one method is optional; the stream's frames are not read.
    unsafe impl SCStreamOutput for StreamOutput {}
);

impl StreamOutput {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// The two window-server calls, when exported.
struct Calls {
    cid: i32,
    hw: Option<FnHw>,
    list_image: Option<FnListImage>,
}

fn calls() -> Calls {
    let mut names = Vec::new();
    // SAFETY: a system framework path; RTLD_NOW resolves everything at load.
    let h = unsafe { libc::dlopen(SKYLIGHT.as_ptr(), libc::RTLD_NOW) };
    // SAFETY (each `load`): the types follow the public signatures in the module docs.
    let hw = if h.is_null() { None } else { unsafe { ffi::load(h, c"SLSHWCaptureWindowList", &mut names) } };
    let list_image = unsafe { ffi::load(libc::RTLD_DEFAULT, c"CGWindowListCreateImage", &mut names) };
    let main: Option<ffi::FnMain> = if h.is_null() { None } else { unsafe { ffi::load(h, c"SLSMainConnectionID", &mut names) } };
    // SAFETY: no arguments; this process's connection id.
    Calls { cid: main.map_or(0, |m| unsafe { m() }), hw, list_image }
}

/// Runs `kind` on `targets` until `until`; how many pictures came back, and an error if it could not run.
fn capture(kind: Kind, c: &Calls, targets: &[Target], until: Instant) -> (usize, Option<String>) {
    let mut got = 0;
    match kind {
        Kind::Hw | Kind::ListImage => {
            while Instant::now() < until {
                for t in targets {
                    let p = match (kind, c.hw, c.list_image) {
                        // SAFETY: one window id that outlives the call; a +1 CFArray or null.
                        (Kind::Hw, Some(f), _) => unsafe { ffi::take_array(f(c.cid, &t.id, 1, HW_OPTIONS)) }.map(|a| a.count() > 0),
                        (Kind::ListImage, _, Some(f)) => {
                            let null = CGRect::new(CGPoint::new(f64::INFINITY, f64::INFINITY), CGSize::new(0.0, 0.0));
                            // SAFETY: the public signature (CGWindow.h); a +1 CGImage or null.
                            let p = unsafe { f(null, LIST_INCLUDING_WINDOW, t.id, IMAGE_OPTIONS) };
                            // SAFETY: a +1 CGImage (or null), released as it drops.
                            std::ptr::NonNull::new(p.cast::<CGImage>()).map(|p| drop(unsafe { CFRetained::from_raw(p) })).map(|_| true)
                        }
                        _ => return (0, Some(format!("{} is not exported on this macOS", kind.name()))),
                    };
                    got += usize::from(p == Some(true));
                }
                std::thread::sleep(EVERY);
            }
        }
        Kind::SckStill => {
            while Instant::now() < until {
                let (tx, rx) = mpsc::channel::<bool>();
                let tx = Mutex::new(tx);
                let done: switch_shots::ShotDone = Arc::new(move |_id, picture| {
                    if let Ok(tx) = tx.lock() {
                        let _ = tx.send(picture.is_some());
                    }
                });
                switch_shots::issue(targets.to_vec(), done);
                for _ in targets {
                    if let Ok(p) = rx.recv_timeout(ANSWER_LIMIT) {
                        got += usize::from(p);
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        Kind::SckStream => match stream(targets[0].id, until) {
            Ok(()) => got = 1,
            Err(e) => return (0, Some(e)),
        },
    }
    (got, None)
}

/// A ScreenCaptureKit stream of window `id` from now until `until`.
fn stream(id: u32, until: Instant) -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<Option<Retained<SCShareableContent>>>();
    let tx = Mutex::new(tx);
    let handler = RcBlock::new(move |content: *mut SCShareableContent, _e: *mut NSError| {
        // SAFETY: SCK passes a valid object or null; retaining keeps it past the block.
        let content = unsafe { Retained::retain(content) };
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(content);
        }
    });
    // SAFETY: the handler matches the documented signature; SCK copies the block.
    unsafe { SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true, false, &handler) };
    let content = rx.recv_timeout(ANSWER_LIMIT).ok().flatten().ok_or("no shareable content")?;
    // SAFETY: plain getters on a live listing.
    let window = unsafe { content.windows() }.iter().find(|w| unsafe { w.windowID() } == id).ok_or("the window is not in the shareable content")?;
    // SAFETY: the designated initializer with a live SCWindow; plain setters on a new configuration.
    let (filter, config) = unsafe {
        let filter = SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window);
        let config = SCStreamConfiguration::new();
        config.setWidth(320);
        config.setHeight(200);
        config.setQueueDepth(3);
        (filter, config)
    };
    // SAFETY: a live filter and configuration; no delegate.
    let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None) };
    let output = StreamOutput::new();
    let queue = DispatchQueue::new("dev.stagemanager.probe.stream", None);
    // SAFETY: the output lives until the stream is stopped below; the queue is a serial queue of ours.
    unsafe { stream.addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), SCStreamOutputType::Screen, Some(&queue)) }
        .map_err(|e| format!("the stream refused its output: {}", e.localizedDescription()))?;
    let answer = |start: bool| -> Result<(), String> {
        let (tx, rx) = mpsc::channel::<Option<String>>();
        let tx = Mutex::new(tx);
        let done = RcBlock::new(move |e: *mut NSError| {
            // SAFETY: SCK passes a valid NSError or null for the length of the block.
            let why = unsafe { e.as_ref() }.map(|e| e.localizedDescription().to_string());
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(why);
            }
        });
        // SAFETY: a live stream; SCK copies the block.
        unsafe {
            if start {
                stream.startCaptureWithCompletionHandler(Some(&done));
            } else {
                stream.stopCaptureWithCompletionHandler(Some(&done));
            }
        }
        match rx.recv_timeout(ANSWER_LIMIT) {
            Ok(None) => Ok(()),
            Ok(Some(why)) => Err(format!("the stream did not {}: {why}", if start { "start" } else { "stop" })),
            Err(_) => Err(format!("the stream's {} did not answer", if start { "start" } else { "stop" })),
        }
    };
    answer(true)?;
    if let Some(d) = until.checked_duration_since(Instant::now()) {
        std::thread::sleep(d);
    }
    let stopped = answer(false);
    drop(output);
    stopped
}

/// Reads the list every [`EVERY`] until `stop`: the menu-bar-level windows at each read.
fn sample(stop: &AtomicBool) -> Vec<(Instant, Vec<Item>)> {
    let mut out = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let next = Instant::now() + EVERY;
        let mut items: Vec<Item> = windows::on_screen()
            .into_iter()
            .filter(|w| w.layer >= 24 && w.layer < 2000 && w.bounds.h < 80.0)
            .map(|w| Item { owner: w.owner, layer: w.layer, w: w.bounds.w.round() as i64, h: w.bounds.h.round() as i64 })
            .collect();
        items.sort();
        out.push((Instant::now(), items));
        if let Some(d) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }
    out
}

/// One run of a kind: when, what it got.
struct Ran {
    kind: Kind,
    start: Instant,
    end: Instant,
    pictures: usize,
    error: Option<String>,
}

/// What the list showed of one run: items that appeared during it (with ms from its start and,
/// when they went in the rest after, ms after its end) and items that went during it.
#[derive(Debug, Default, PartialEq)]
struct Seen {
    appeared: Vec<(Item, f64, Option<f64>)>,
    went: Vec<(Item, f64)>,
}

/// The run's change against the rest before it: an item in none of the rest's reads and in a
/// read during the run (or the second after) appeared; one of the menu bar before any capture
/// (`initial`: never a kind's own item still up from the kind before) in every rest read and
/// missing from two reads during the run went.
fn seen(run: &Ran, rest: Duration, reads: &[(Instant, Vec<Item>)], initial: &[Item]) -> Seen {
    let before: Vec<&Vec<Item>> = reads.iter().filter(|(at, _)| *at < run.start && *at + rest >= run.start).map(|(_, i)| i).collect();
    let during: Vec<&(Instant, Vec<Item>)> = reads.iter().filter(|(at, _)| *at >= run.start && *at <= run.end + Duration::from_secs(1)).collect();
    let after: Vec<&(Instant, Vec<Item>)> = reads.iter().filter(|(at, _)| *at > run.end).collect();
    let mut s = Seen::default();
    if before.is_empty() {
        return s;
    }
    for (at, items) in &during {
        for i in items {
            if before.iter().all(|b| !b.contains(i)) && !s.appeared.iter().any(|(a, _, _)| a == i) {
                let gone = after.iter().find(|(_, later)| !later.contains(i)).map(|(t, _)| super::ms(run.end, *t));
                s.appeared.push((i.clone(), super::ms(run.start, *at), gone));
            }
        }
    }
    for i in before[0].iter().filter(|i| initial.contains(i) && before.iter().all(|b| b.contains(i))) {
        let missing: Vec<&&(Instant, Vec<Item>)> = during.iter().filter(|(_, items)| !items.contains(i)).collect();
        if missing.len() >= 2 && !s.went.iter().any(|(w, _)| w == i) {
            s.went.push((i.clone(), super::ms(run.start, missing[0].0)));
        }
    }
    s
}

pub(super) fn run(ctx: &Ctx, args: &Args) -> Result<(), String> {
    let bundle = args.bundle(0, "indicator BUNDLE [--run S] [--rest S] [--by-eye]")?;
    if !CGPreflightScreenCaptureAccess() {
        return Err("no Screen Recording for this process (launch it through `open` from the dev app; the probe never asks)".into());
    }
    let (_, list) = fixture(&ctx.sky, bundle);
    let on: Vec<&windows::CgWindow> = list.iter().filter(|w| w.onscreen).collect();
    if on.len() < 2 {
        return Err(format!("{bundle} needs two windows on screen ({} found)", on.len()));
    }
    let scale = NSScreen::mainScreen(ctx.mtm).map_or(2.0, |s| s.backingScaleFactor());
    let targets: Vec<Target> = on[..2]
        .iter()
        .map(|w| Target { id: w.id, width: (w.bounds.w * scale).round().max(1.0) as u32, height: (w.bounds.h * scale).round().max(1.0) as u32 })
        .collect();
    let (run_for, rest) = (Duration::from_secs_f64(args.run_s), Duration::from_secs_f64(args.rest_s));
    println!("== indicator: which capture lights the purple screen-recording indicator ==");
    let hidden = targets[1].id;
    let engine = crate::platform::spaces::engine::shared();
    let hid = off_main(|| engine.hide_window(hidden));
    pump(Duration::from_millis(400));
    let in_stage = windows::describe(&[hidden]).first().is_some_and(|w| !w.onscreen);
    println!("  window 1 on screen; window 2 hidden in a stage Space by the Spaces engine: {}", if hid && in_stage { "yes" } else { "NO (both kinds still run; the second window is wherever it is)" });
    let c = calls();
    let stop = AtomicBool::new(false);
    let (runs, reads) = std::thread::scope(|s| {
        let sampler = s.spawn(|| sample(&stop));
        println!("  resting {:.0} s first (the baseline), then each kind {:.0} s with {:.0} s of rest after; hands off the Mac", rest.as_secs_f64(), run_for.as_secs_f64(), rest.as_secs_f64());
        pump(rest);
        let mut runs = Vec::new();
        for kind in Kind::ALL {
            println!("  {} …", kind.name());
            let start = Instant::now();
            let (pictures, error) = off_main(|| capture(kind, &c, &targets, start + run_for));
            let end = Instant::now();
            runs.push(Ran { kind, start, end, pictures, error });
            pump(rest);
        }
        stop.store(true, Ordering::Relaxed);
        (runs, sampler.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
    });
    let swept = off_main(|| engine.sweep("probe", true));
    pump(Duration::from_millis(400));
    let back = windows::describe(&[hidden]).first().is_some_and(|w| w.onscreen);
    println!("  window 2 brought back: {} (restored {}, failed {})", if back { "yes" } else { "NO" }, swept.restored, swept.failed);
    let baseline: Vec<Item> = reads.first().map(|r| r.1.clone()).unwrap_or_default();
    println!("\n  menu-bar-level windows at the start ({} of them; owner, layer, size):", baseline.len());
    for i in &baseline {
        println!("    {}", i.describe());
    }
    let mut rows = Vec::new();
    let mut unclear = Vec::new();
    let mut verdicts = Vec::new();
    // Items a kind brought that were still up when the next one started.
    let mut lingering: Vec<Item> = Vec::new();
    for r in &runs {
        let s = seen(r, rest, &reads, &baseline);
        let appeared: Vec<String> = s.appeared.iter().map(|(i, at, gone)| format!("{} at +{at:.0} ms, {}", i.describe(), gone.map_or("still there after the rest".into(), |g| format!("gone {g:.0} ms after the end")))).collect();
        let went: Vec<String> = s.went.iter().map(|(i, at)| format!("{} at +{at:.0} ms", i.describe())).collect();
        rows.push(vec![
            r.kind.name().to_string(),
            r.pictures.to_string(),
            r.error.clone().unwrap_or_default(),
            if appeared.is_empty() { "-".into() } else { appeared.join("; ") },
            if went.is_empty() { "-".into() } else { went.join("; ") },
        ]);
        let verdict = if r.error.is_some() || r.pictures == 0 {
            "could not run (no picture)".to_string()
        } else if !s.appeared.is_empty() || !s.went.is_empty() {
            format!("LIGHTS something in the menu bar ({})", appeared.iter().chain(&went).cloned().collect::<Vec<_>>().join("; "))
        } else if !lingering.is_empty() {
            unclear.push(r.kind);
            format!("could not tell: what the kind before brought was still up ({})", lingering.iter().map(Item::describe).collect::<Vec<_>>().join("; "))
        } else {
            unclear.push(r.kind);
            "could not tell from the list (no menu-bar window came or went)".into()
        };
        verdicts.push((r.kind, verdict));
        let up_at_next = |gone: Option<f64>| gone.is_none_or(|g| g >= rest.as_secs_f64() * 1e3);
        lingering = s.appeared.iter().filter(|(_, _, gone)| up_at_next(*gone)).map(|(i, _, _)| i.clone()).collect();
    }
    println!();
    table(&["kind", "pictures", "error", "appeared while it ran", "went while it ran"], &rows);
    println!();
    for (k, v) in &verdicts {
        println!("VERDICT {}: {v}", k.name());
    }
    let eye: Vec<Kind> = if args.by_eye { Kind::ALL.to_vec() } else { unclear };
    if !eye.is_empty() {
        by_eye(&eye, &c, &targets, run_for, rest, &engine);
    }
    Ok(())
}

/// The kinds the list could not tell, again one by one with a countdown, the owner watching.
fn by_eye(kinds: &[Kind], c: &Calls, targets: &[Target], run_for: Duration, rest: Duration, engine: &crate::platform::spaces::engine::SpacesEngine) {
    println!("\n== by eye: watch the right end of the menu bar ==");
    println!("  macOS shows a purple screen-recording indicator there (a purple pill or icon by Control Center) while something records the screen; note for each kind whether it appears");
    let hidden = targets[1].id;
    let _ = off_main(|| engine.hide_window(hidden));
    for k in kinds {
        for n in (1..=COUNTDOWN).rev() {
            println!("  {}: starts in {n}", k.name());
            pump(Duration::from_secs(1));
        }
        println!("  {}: NOW, for {:.0} s", k.name(), run_for.as_secs_f64());
        let start = Instant::now();
        let (pictures, error) = off_main(|| capture(*k, c, targets, start + run_for));
        println!("  {}: stopped ({pictures} pictures{}); watch whether the indicator goes; {:.0} s of rest", k.name(), error.map(|e| format!(", {e}")).unwrap_or_default(), rest.as_secs_f64());
        pump(rest);
    }
    let _ = off_main(|| engine.sweep("probe", true));
    println!("VERDICT by eye: the owner's word per kind (the probe cannot see the indicator)");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(owner: &str, w: i64) -> Item {
        Item { owner: owner.into(), layer: 25, w, h: 24 }
    }

    #[test]
    fn an_item_that_comes_with_a_run_is_seen() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let run = Ran { kind: Kind::SckStill, start: at(5000), end: at(10_000), pictures: 4, error: None };
        let base = vec![item("Control Center", 30), item("SystemUIServer", 40)];
        let lit = vec![item("Control Center", 30), item("Control Center", 60), item("SystemUIServer", 40)];
        let mut reads: Vec<(Instant, Vec<Item>)> = (0..50).map(|i| (at(i * 100), base.clone())).collect();
        reads.extend((50..100).map(|i| (at(i * 100), lit.clone())));
        reads.extend((100..150).map(|i| (at(i * 100), base.clone())));
        let s = seen(&run, Duration::from_secs(5), &reads, &base);
        assert_eq!(s.appeared.len(), 1);
        assert_eq!(s.appeared[0].0, item("Control Center", 60));
        assert_eq!(s.appeared[0].1, 0.0);
        assert!(s.went.is_empty());
    }

    #[test]
    fn nothing_changed_is_nothing_seen() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let run = Ran { kind: Kind::Hw, start: at(5000), end: at(10_000), pictures: 4, error: None };
        let reads: Vec<(Instant, Vec<Item>)> = (0..150).map(|i| (at(i * 100), vec![item("Control Center", 30)])).collect();
        assert_eq!(seen(&run, Duration::from_secs(5), &reads, &[item("Control Center", 30)]), Seen::default());
    }
}
