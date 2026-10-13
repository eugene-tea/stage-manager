//! Window thumbnails through ScreenCaptureKit: stills of the windows shown on strip cards.
//!
//! A worker thread takes a still of every requested window each interval with
//! `SCScreenshotManager` and an `SCContentFilter(desktopIndependentWindow:)`, sized as asked
//! (the strip asks for 2× its thumbnail size). Each image goes to the main thread through
//! GCD, keyed by window id. Parked windows still render, so they capture too.
//!
//! Only the requested windows are captured; the caller replaces the request whenever the
//! strip changes and an empty request stops captures (the strip is hidden).
//!
//! Each capture counts in the day's log (ok or failed); a window's captures starting to fail,
//! and working again, are logged once each, not every round.
//!
//! Access: Screen Recording. At start, when it is missing and prompts are allowed, the
//! capturer asks through `SCShareableContent` (on recent macOS this is what lists the app
//! in System Settings); rounds are skipped until the grant arrives.

use std::collections::{HashMap, HashSet};
use std::ptr::NonNull;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::AllocAnyThread;
use objc2::rc::autoreleasepool;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGImage, CGPreflightScreenCaptureAccess};
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
};

use crate::logbook::counters::{self, Count};

/// Windows whose last capture outcome is remembered; past this the memory starts over.
const MAX_OUTCOMES: usize = 4096;

use super::permissions;

/// One window to capture, at a pixel size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    /// `CGWindowID`.
    pub id: u32,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
}

/// Receives each captured image on the main thread.
pub type Deliver = fn(u32, CFRetained<CGImage>);

/// How long one round may take before the worker moves on.
const ROUND_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest wait between checks while nothing is due (grant changes, shutdown).
const IDLE_CHECK: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Shared {
    targets: Vec<Target>,
    /// Window ids captured at least once since they were requested.
    captured: HashSet<u32>,
    interval: Duration,
    /// Capture every target now, whatever the interval says.
    force: bool,
    shutdown: bool,
}

/// The capture worker. Dropping it stops the thread after its current round.
pub struct Capturer {
    shared: Arc<(Mutex<Shared>, Condvar)>,
}

impl Capturer {
    /// Starts the worker with a refresh interval; images go to `deliver` on the main thread.
    /// Asks for Screen Recording when it is missing (and prompts are allowed).
    pub fn start(interval: Duration, deliver: Deliver) -> Self {
        Self::spawn(interval, deliver, true)
    }

    /// Like [`Capturer::start`], without asking for Screen Recording (a second capturer).
    pub fn start_quiet(interval: Duration, deliver: Deliver) -> Self {
        Self::spawn(interval, deliver, false)
    }

    fn spawn(interval: Duration, deliver: Deliver, ask: bool) -> Self {
        let shared = Arc::new((
            Mutex::new(Shared { interval, ..Shared::default() }),
            Condvar::new(),
        ));
        if ask && !CGPreflightScreenCaptureAccess() {
            if permissions::prompts_allowed() {
                tracing::info!("Screen Recording missing; asking through ScreenCaptureKit");
                request_access();
            } else {
                tracing::warn!("Screen Recording missing; thumbnails are off until it is granted");
            }
        }
        let worker = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || run(worker, deliver));
        if let Err(e) = spawned {
            tracing::error!("cannot start the capture thread: {e}");
        }
        Self { shared }
    }

    /// Replaces the windows to capture. New windows are captured right away; the rest on
    /// the next interval. An empty list pauses captures.
    pub fn set_targets(&self, targets: Vec<Target>) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut s) = lock.lock() {
            if s.targets == targets {
                return;
            }
            let ids: HashSet<u32> = targets.iter().map(|t| t.id).collect();
            s.captured.retain(|id| ids.contains(id));
            s.targets = targets;
            cv.notify_all();
        }
    }

    /// Captures every target as soon as the current round ends.
    pub fn capture_now(&self) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut s) = lock.lock() {
            s.force = true;
            cv.notify_all();
        }
    }

    /// Changes the refresh interval.
    pub fn set_interval(&self, interval: Duration) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut s) = lock.lock() {
            s.interval = interval;
            cv.notify_all();
        }
    }
}

impl Drop for Capturer {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut s) = lock.lock() {
            s.shutdown = true;
            cv.notify_all();
        }
    }
}

/// Asks ScreenCaptureKit for shareable content once, which makes macOS ask for Screen
/// Recording and list the app in System Settings.
fn request_access() {
    let handler = RcBlock::new(|_content: *mut SCShareableContent, error: *mut NSError| {
        // SAFETY: SCK passes a valid NSError or null.
        if let Some(e) = unsafe { error.as_ref() } {
            tracing::info!("ScreenCaptureKit access request: {}", e.localizedDescription());
        }
    });
    // SAFETY: the handler matches the documented signature; SCK copies the block.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
}

/// The worker loop: waits until a round is due, runs it, repeats.
fn run(shared: Arc<(Mutex<Shared>, Condvar)>, deliver: Deliver) {
    let (lock, cv) = &*shared;
    let mut last_full: Option<Instant> = None;
    let mut round_no: u64 = 0;
    let mut outcomes: HashMap<u32, bool> = HashMap::new();
    loop {
        let batch = {
            let Ok(mut s) = lock.lock() else {
                tracing::error!("capture state lock poisoned; thumbnails stop");
                return;
            };
            loop {
                if s.shutdown {
                    return;
                }
                let granted = CGPreflightScreenCaptureAccess();
                let due_in = last_full.map_or(Duration::ZERO, |t| s.interval.saturating_sub(t.elapsed()));
                if granted && !s.targets.is_empty() {
                    if due_in.is_zero() || s.force {
                        s.force = false;
                        last_full = Some(Instant::now());
                        break s.targets.clone();
                    }
                    let fresh: Vec<Target> =
                        s.targets.iter().filter(|t| !s.captured.contains(&t.id)).copied().collect();
                    if !fresh.is_empty() {
                        break fresh;
                    }
                }
                let wait = if granted && !s.targets.is_empty() { due_in.min(IDLE_CHECK) } else { IDLE_CHECK };
                s = match cv.wait_timeout(s, wait) {
                    Ok((guard, _)) => guard,
                    Err(_) => {
                        tracing::error!("capture state lock poisoned; thumbnails stop");
                        return;
                    }
                };
            }
        };
        round_no += 1;
        let done = autoreleasepool(|_| capture_round(round_no, &batch, deliver, &mut outcomes));
        if let Ok(mut s) = lock.lock() {
            s.captured.extend(done);
        }
    }
}

/// Messages from ScreenCaptureKit's queues back to the worker.
enum Msg {
    /// The shareable content arrived and `n` captures were issued.
    Issued { round: u64, n: usize },
    /// One capture finished (`ok` = an image was delivered; else why not).
    Captured { round: u64, id: u32, ok: bool, why: Option<String> },
    /// A requested window is not in the shareable content.
    Missing { round: u64, id: u32 },
}

/// Captures `batch` once; returns the window ids that produced an image or were found
/// missing (so they are not retried as "fresh" in a tight loop). Counts every outcome and
/// logs the windows whose outcome changed since `outcomes` last saw them.
fn capture_round(round: u64, batch: &[Target], deliver: Deliver, outcomes: &mut HashMap<u32, bool>) -> Vec<u32> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let targets: Vec<Target> = batch.to_vec();
    let content_tx = tx.clone();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        // SAFETY: SCK passes a valid object or null for each argument.
        let (content, error) = unsafe { (content.as_ref(), error.as_ref()) };
        let Some(content) = content else {
            let why = error.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
            tracing::warn!("no shareable content: {why}");
            let _ = content_tx.send(Msg::Issued { round, n: 0 });
            return;
        };
        // SAFETY: plain getters on a live SCShareableContent.
        let windows = unsafe { content.windows() };
        let mut issued = 0;
        for t in &targets {
            // SAFETY: plain getter on a live SCWindow.
            let Some(window) = windows.iter().find(|w| unsafe { w.windowID() } == t.id) else {
                tracing::trace!(window = t.id, "window not in shareable content");
                let _ = content_tx.send(Msg::Missing { round, id: t.id });
                continue;
            };
            // SAFETY: designated initializer with a live SCWindow; plain setters.
            let (filter, config) = unsafe {
                let filter = SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window);
                let config = SCStreamConfiguration::new();
                config.setWidth(t.width.max(1) as usize);
                config.setHeight(t.height.max(1) as usize);
                config.setShowsCursor(false);
                config.setIgnoreShadowsSingleWindow(true);
                config.setScalesToFit(true);
                (filter, config)
            };
            let id = t.id;
            let done_tx = content_tx.clone();
            let on_image = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
                let (ok, why) = match NonNull::new(image) {
                    Some(ptr) => {
                        // SAFETY: SCK hands a valid CGImage for the duration of the block;
                        // retaining keeps it alive for the main thread.
                        let image = unsafe { CFRetained::retain(ptr) };
                        DispatchQueue::main().exec_async(move || deliver(id, image));
                        (true, None)
                    }
                    None => {
                        // SAFETY: SCK passes a valid NSError or null.
                        let why = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
                        tracing::trace!(window = id, ?why, "capture failed");
                        (false, why)
                    }
                };
                let _ = done_tx.send(Msg::Captured { round, id, ok, why });
            });
            // SAFETY: filter, configuration and handler are valid; SCK copies the block.
            unsafe {
                SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
                    &filter,
                    &config,
                    Some(&on_image),
                );
            }
            issued += 1;
        }
        let _ = content_tx.send(Msg::Issued { round, n: issued });
    });
    // SAFETY: the handler matches the documented signature; SCK copies the block.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, false, &handler,
        );
    }
    drop(tx);

    let deadline = Instant::now() + ROUND_TIMEOUT;
    let mut expected: Option<usize> = None;
    let mut finished = 0usize;
    let mut handled: Vec<u32> = Vec::new();
    let mut failed: HashMap<u32, String> = HashMap::new();
    while expected.is_none_or(|n| finished < n) {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Msg::Issued { round: r, n }) if r == round => expected = Some(n),
            Ok(Msg::Captured { round: r, id, ok, why }) if r == round => {
                finished += 1;
                if ok {
                    handled.push(id);
                } else {
                    failed.insert(id, why.unwrap_or_else(|| "no image".into()));
                }
            }
            Ok(Msg::Missing { round: r, id }) if r == round => {
                failed.insert(id, "not in the shareable content".into());
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {
                tracing::warn!(round, "capture round timed out");
                break;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // Windows SCK does not know are marked handled too, so they wait for the next interval.
    let missing = batch.iter().map(|t| t.id).filter(|id| !handled.contains(id));
    let mut all = handled.clone();
    if expected.is_some() {
        all.extend(missing);
    }
    tracing::trace!(round, captured = handled.len(), requested = batch.len(), "capture round");
    if expected.is_some() {
        note_outcomes(batch, &handled, &failed, outcomes);
    }
    all
}

/// Counts a round's outcomes and logs each window whose captures start failing or work
/// again (once per change, not every round).
fn note_outcomes(batch: &[Target], handled: &[u32], failed: &HashMap<u32, String>, outcomes: &mut HashMap<u32, bool>) {
    if outcomes.len() > MAX_OUTCOMES {
        outcomes.clear();
    }
    for t in batch {
        let ok = handled.contains(&t.id);
        counters::bump(if ok { Count::CaptureOk } else { Count::CaptureFailed });
        match (outcomes.insert(t.id, ok), ok) {
            (Some(true) | None, false) => {
                let why = failed.get(&t.id).map_or("no answer in time", String::as_str);
                tracing::debug!(window = t.id, why, "capture failing");
            }
            (Some(false), true) => tracing::debug!(window = t.id, "capture works again"),
            _ => {}
        }
    }
}
