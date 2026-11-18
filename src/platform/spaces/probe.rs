//! The launch probe (1.9, docs/ARCHITECTURE.md "Window engines"): whether this Mac, at this
//! launch, can run the Spaces engine. Every call the engine needs is looked up, then one
//! trial with a window of the app's own (a tiny panel, off every screen, never drawn): a
//! Space made, the panel added into it and read back hidden, added back onto its Desktop
//! and read back shown, the Space ended and the panel closed. Each step read back from the
//! window server, as the engine reads back every hide and show; the first step that does not
//! read back as asked is the verdict's reason. No other app's window is touched.
//!
//! Main thread, once AppKit is up (the bridged operations need it) and before the WM thread
//! starts. Bounded: each read back waits at most [`STEP_WAIT`].

use std::time::{Duration, Instant};

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2::sel;
use objc2_app_kit::{NSBackingStoreType, NSColor, NSPanel, NSWindowStyleMask};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

use super::ffi::{ALL_SPACES_MASK, Route, Sky};
use crate::engine::engine_choice::ProbeVerdict;

/// How long one step waits for the window server to list the panel where the call put it
/// (the spike: within a millisecond or two).
const STEP_WAIT: Duration = Duration::from_millis(40);

/// The pause between two reads while waiting.
const STEP_PAUSE: Duration = Duration::from_micros(250);

/// Where the trial panel sits, bottom-left in AppKit's coordinates: far off every screen.
const OFF_SCREEN: f64 = -30_000.0;

/// The verdict's reason when every step read back as asked.
pub const WORKS: &str = "a trial Space with an own window works";

/// The C functions the Spaces engine cannot run without.
const NEEDED: &[&str] = &["SLSCopySpacesForWindows", "SLSCopyManagedDisplaySpaces", "SLSSpaceCreate", "SLSSpaceDestroy"];

/// A step a test run makes fail (`STAGE_MANAGER_PROBE_FAIL=hide|show`, honoured only with
/// `STAGE_MANAGER_ONLY`): its call is not sent, so its read back finds the panel where it
/// was, as on a Mac where the call is refused.
fn failing_step() -> Option<String> {
    std::env::var_os("STAGE_MANAGER_ONLY")?;
    std::env::var("STAGE_MANAGER_PROBE_FAIL").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Each step's time, for the verdict ([`ProbeVerdict::steps`]).
struct Clock {
    last: Instant,
    steps: Vec<String>,
}

impl Clock {
    fn lap(&mut self, step: &str) {
        let now = Instant::now();
        self.steps.push(format!("{step} {:.1}", now.duration_since(self.last).as_secs_f64() * 1000.0));
        self.last = now;
    }
}

/// Runs the probe; the verdict says why not when it fails.
pub fn run(mtm: MainThreadMarker) -> ProbeVerdict {
    let began = Instant::now();
    let mut clock = Clock { last: began, steps: Vec::new() };
    let result = trial(mtm, failing_step().as_deref(), &mut clock);
    let ms = began.elapsed().as_secs_f64() * 1000.0;
    let steps = clock.steps.join(", ");
    match result {
        Ok(()) => ProbeVerdict { ok: true, reason: WORKS.into(), ms, steps },
        Err(reason) => ProbeVerdict { ok: false, reason, ms, steps },
    }
}

/// Reads the panel's Spaces until `done` says so or [`STEP_WAIT`] is over; the last read.
fn wait_for(sky: &Sky, id: u32, done: impl Fn(&[u64]) -> bool) -> Result<Vec<u64>, Vec<u64>> {
    let began = Instant::now();
    loop {
        let now = sky.spaces_of(&[id]);
        if done(&now) {
            return Ok(now);
        }
        if began.elapsed() >= STEP_WAIT {
            return Err(now);
        }
        std::thread::sleep(STEP_PAUSE);
    }
}

/// The calls looked up: what is missing, if anything.
fn missing(sky: &Sky) -> Option<String> {
    let mut gone: Vec<&str> = sky.names.iter().filter(|(n, found)| !found && NEEDED.contains(n)).map(|(n, _)| *n).collect();
    if sky.msg_send.is_null() {
        gone.push("objc_msgSend");
    }
    let add = c"SLSBridgedSpaceAddWindowsAndRemoveFromSpacesOperation";
    match Sky::class(add) {
        None => gone.push("SLSBridgedSpaceAddWindowsAndRemoveFromSpacesOperation"),
        Some(cls) => {
            if !cls.responds_to(sel!(initWithSpaceID:windows:options:)) || !cls.responds_to(sel!(performWithWMBridgeDelegate)) {
                gone.push("SLSBridgedSpaceAddWindowsAndRemoveFromSpacesOperation's methods");
            }
        }
    }
    (!gone.is_empty()).then(|| format!("missing: {}", gone.join(", ")))
}

/// The tiny panel of the trial: borderless, transparent, off every screen, never key, not
/// on every Desktop (it opens on the one shown).
fn panel(mtm: MainThreadMarker) -> Retained<NSPanel> {
    let rect = NSRect::new(NSPoint::new(OFF_SCREEN, OFF_SCREEN), NSSize::new(4.0, 4.0));
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        rect,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setIgnoresMouseEvents(true);
    panel.setHidesOnDeactivate(false);
    panel.setAlphaValue(0.0);
    // SAFETY: the panel is closed and dropped at the end of the trial; nothing else keeps it.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel
}

fn trial(mtm: MainThreadMarker, fail: Option<&str>, clock: &mut Clock) -> Result<(), String> {
    let sky = Sky::open()?;
    if let Some(why) = missing(&sky) {
        return Err(why);
    }
    clock.lap("calls");
    let desktops: Vec<u64> = sky.displays().iter().flat_map(|(_, _, spaces)| spaces.iter().map(|(id, _)| *id)).collect();
    if desktops.is_empty() {
        return Err("the window server listed no Desktop".into());
    }
    clock.lap("desktops");
    let panel = panel(mtm);
    clock.lap("panel");
    panel.orderFrontRegardless();
    // AppKit hands the order-in to the window server with its next transaction, at the run
    // loop, which has not run yet at the launch: flushed now, or the window is on no Space
    // (measured 11-14: on none after 40 ms unflushed; on its Desktop at once flushed).
    objc2_quartz_core::CATransaction::flush();
    let id = u32::try_from(panel.windowNumber()).unwrap_or(0);
    clock.lap("order-in");
    let outcome = if id == 0 { Err("the trial window has no window number".to_string()) } else { steps(&sky, id, &desktops, fail, clock) };
    panel.orderOut(None);
    panel.close();
    clock.lap("close");
    outcome
}

/// The trial's steps on window `id`, the Space ended whatever happened after it was made.
fn steps(sky: &Sky, id: u32, desktops: &[u64], fail: Option<&str>, clock: &mut Clock) -> Result<(), String> {
    let on_desktop = |s: &[u64]| s.iter().any(|x| desktops.contains(x));
    let home = match wait_for(sky, id, on_desktop) {
        Ok(now) => now.into_iter().find(|x| desktops.contains(x)).unwrap_or(0),
        Err(now) => return Err(format!("the trial window is on no Desktop (listed on {now:?})")),
    };
    clock.lap("listed");
    let space = match sky.create(Route::Classic, false) {
        Ok(0) => return Err("the window server made no trial Space".into()),
        Ok(space) => space,
        Err(e) => return Err(format!("no trial Space: {e}")),
    };
    clock.lap("space");
    let result = (|| {
        if fail != Some("hide") {
            sky.add_remove(Route::Bridged, space, &[id]).map_err(|e| format!("the hide could not be sent: {e}"))?;
        }
        wait_for(sky, id, |s| s == [space]).map_err(|now| format!("the trial window did not go into the trial Space (listed on {now:?})"))?;
        clock.lap("hide");
        if fail != Some("show") {
            sky.add_remove_from(Route::Bridged, home, &[id], ALL_SPACES_MASK).map_err(|e| format!("the show could not be sent: {e}"))?;
        }
        wait_for(sky, id, |s| s.contains(&home) && !s.contains(&space))
            .map_err(|now| format!("the trial window did not come back onto its Desktop (listed on {now:?})"))?;
        clock.lap("show");
        Ok(())
    })();
    let _ = sky.destroy(Route::Classic, space);
    clock.lap("end");
    result
}
