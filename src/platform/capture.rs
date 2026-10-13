//! Window thumbnails through ScreenCaptureKit: stills of the windows shown on strip cards.
//!
//! A worker thread takes a still of every requested window each interval with
//! `SCScreenshotManager` and an `SCContentFilter(desktopIndependentWindow:)`, sized as asked
//! (the strip asks for 2× its thumbnail size) but always in the window's own proportions
//! ([`fit_to_content`]): asked for another shape, ScreenCaptureKit pads the image and pins the
//! window to its top-left corner (measured 10-12), so a snapshot stretched over the window's
//! frame drew its content smaller and off to one side. Each image goes to the main thread through
//! GCD, keyed by window id. Parked windows still render, so they capture too.
//!
//! Only the requested windows are captured; the caller replaces the request whenever the
//! strip changes and an empty request stops captures (the strip is hidden).
//!
//! Each capture counts in the day's log (ok or failed); a window's captures starting to fail,
//! and working again, are logged once each, not every round.
//!
//! Access: Screen Recording. When it is missing and prompts are allowed, the first capturer
//! asks through `SCShareableContent` once captures are switched on (on recent macOS this is
//! what lists the app in System Settings); rounds are skipped until the grant arrives.
//!
//! **Quiet by design.** macOS (replayd) may answer a capture with an alert ("… is requesting
//! to bypass the system private window picker …"). Three rules keep it to the one macOS
//! needs:
//! - Captures run only once switched on ([`set_enabled`]: `[strip] capture`, forced off by
//!   `STAGE_MANAGER_NO_CAPTURE`); until the config says so, nothing is captured.
//! - Every capturer of the process shares one [`Gate`]: the first capture goes alone, and a
//!   capture refused as user-declined stops them all for a long backoff (logged once);
//!   [`retry_now`] (the user asked for access) probes again at once.
//! - Quit waits for captures in flight ([`quiesce`]): replayd checks a capture's approval
//!   after answering it, and for a process that has just exited it cannot resolve the app's
//!   path, so it alerts as if the app were new (see README's Known limitations).

use std::collections::{HashMap, HashSet};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::AllocAnyThread;
use objc2::rc::autoreleasepool;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGImage, CGPreflightScreenCaptureAccess};
use objc2_foundation::{NSError, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration, SCStreamErrorCode,
    SCStreamErrorDomain,
};

use super::capture_gate::{Change, Gate, Outcome, Permit};
use super::permissions;
use crate::logbook::counters::{self, Count};

/// Windows whose last capture outcome is remembered; past this the memory starts over.
const MAX_OUTCOMES: usize = 4096;

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
/// After the last capture answered, how long [`quiesce`] still keeps the process alive, so
/// replayd finishes its approval bookkeeping while the app's path can be resolved.
const SETTLE: Duration = Duration::from_millis(250);

/// Any value turns captures off for the whole run, whatever the config says (test runs).
pub const NO_CAPTURE_ENV: &str = "STAGE_MANAGER_NO_CAPTURE";

/// [`MODE`]: no word from the config yet; nothing is captured.
const PENDING: u8 = 0;
/// [`MODE`]: captures on.
const ON: u8 = 1;
/// [`MODE`]: captures off (config or environment).
const OFF: u8 = 2;

/// Whether captures may run at all ([`set_enabled`]).
static MODE: AtomicU8 = AtomicU8::new(PENDING);
/// Quit has begun: no new capture is issued.
static CLOSING: AtomicBool = AtomicBool::new(false);
/// ScreenCaptureKit calls issued and not answered yet.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
/// When the last call was answered, in milliseconds since [`EPOCH`] (0: never).
static LAST_DONE_MS: AtomicU64 = AtomicU64::new(0);
/// Images delivered since launch.
static IMAGES: AtomicU64 = AtomicU64::new(0);
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
/// The one gate of the process: every capturer asks it before a round.
static GATE: LazyLock<Mutex<Gate>> = LazyLock::new(|| Mutex::new(Gate::default()));
/// One capturer's worker state and its wake-up.
type Worker = (Mutex<Shared>, Condvar);

/// Every capturer's worker state, to wake them when the mode or the gate changes.
static WORKERS: Mutex<Vec<Weak<Worker>>> = Mutex::new(Vec::new());

fn gate() -> MutexGuard<'static, Gate> {
    GATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// True when `STAGE_MANAGER_NO_CAPTURE` is set.
pub fn env_disabled() -> bool {
    std::env::var_os(NO_CAPTURE_ENV).is_some()
}

/// Switches captures on or off (`[strip] capture`); `STAGE_MANAGER_NO_CAPTURE` keeps them
/// off. Until the first call nothing is captured.
pub fn set_enabled(on: bool) {
    let mode = if on && !env_disabled() { ON } else { OFF };
    let was = MODE.swap(mode, Ordering::SeqCst);
    if was != mode {
        match (mode, env_disabled()) {
            (ON, _) => tracing::info!("window captures on"),
            (_, true) => tracing::info!("window captures off ({NO_CAPTURE_ENV}): cards and switch ghosts are drawn without pictures"),
            _ => tracing::info!("window captures off ([strip] capture = false): cards and switch ghosts are drawn without pictures"),
        }
        wake_all();
    }
}

/// True unless captures are off or refused: pictures of windows may still come. When false,
/// the strip draws plain ghosts instead of waiting for snapshots.
pub fn pictures_expected() -> bool {
    MODE.load(Ordering::SeqCst) != OFF && !gate().refused()
}

/// True while captures rest after macOS refused one.
pub fn refused() -> bool {
    gate().refused()
}

/// The user asked for screen access: a refused gate tries once more now.
pub fn retry_now() {
    gate().user_asked(Instant::now());
    wake_all();
}

/// Quit has begun: no capture is issued from now on (those in flight finish).
pub fn begin_shutdown() {
    CLOSING.store(true, Ordering::SeqCst);
}

/// Stops issuing captures and waits (up to `timeout`) until every one in flight has been
/// answered, plus [`SETTLE`]; call it right before the process exits. A process that exits
/// with a capture in flight makes replayd alert ("… bypass the system private window
/// picker …") even for an approved app.
pub fn quiesce(timeout: Duration) {
    begin_shutdown();
    let start = Instant::now();
    let pending = IN_FLIGHT.load(Ordering::SeqCst);
    while IN_FLIGHT.load(Ordering::SeqCst) > 0 && start.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(10));
    }
    let left = IN_FLIGHT.load(Ordering::SeqCst);
    let last = LAST_DONE_MS.load(Ordering::SeqCst);
    if last > 0 {
        let since = EPOCH.elapsed().saturating_sub(Duration::from_millis(last));
        if since < SETTLE {
            std::thread::sleep(SETTLE - since);
        }
    }
    if pending > 0 || left > 0 {
        tracing::info!(pending, left, waited_ms = start.elapsed().as_millis() as u64, "captures quiesced before exit");
    }
}

/// The capture state for the ctl dump.
pub fn status() -> serde_json::Value {
    let mode = match MODE.load(Ordering::SeqCst) {
        ON => "on",
        OFF => "off",
        _ => "pending",
    };
    serde_json::json!({
        "mode": mode,
        "env_off": env_disabled(),
        "gate": gate().describe(),
        "in_flight": IN_FLIGHT.load(Ordering::SeqCst),
        "images": IMAGES.load(Ordering::SeqCst),
    })
}

fn wake_all() {
    let Ok(mut workers) = WORKERS.lock() else { return };
    workers.retain(|w| w.strong_count() > 0);
    for w in workers.iter().filter_map(Weak::upgrade) {
        let (lock, cv) = &*w;
        // Taking the lock orders the wake-up after a worker's check.
        drop(lock.lock());
        cv.notify_all();
    }
}

/// Counts one ScreenCaptureKit call from issue to answer (dropped in its completion).
struct InFlight;

impl InFlight {
    fn issue() -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        InFlight
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let ms = (EPOCH.elapsed().as_millis() as u64).max(1);
        LAST_DONE_MS.store(ms, Ordering::SeqCst);
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// True for ScreenCaptureKit's "user declined" (not permitted) error.
fn declined(error: &NSError) -> bool {
    // SAFETY: a framework constant string.
    let domain: &NSString = unsafe { SCStreamErrorDomain };
    error.code() == SCStreamErrorCode::UserDeclined.0 && *error.domain() == *domain
}

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
    shared: Arc<Worker>,
}

impl Capturer {
    /// Starts the worker with a refresh interval; images go to `deliver` on the main thread.
    /// Asks for Screen Recording when it is missing (and prompts are allowed) once captures
    /// are switched on.
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
        if let Ok(mut workers) = WORKERS.lock() {
            workers.push(Arc::downgrade(&shared));
        }
        let worker = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || run(worker, deliver, ask));
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
    let flight = InFlight::issue();
    let handler = RcBlock::new(move |_content: *mut SCShareableContent, error: *mut NSError| {
        let _answered = &flight;
        // SAFETY: SCK passes a valid NSError or null.
        if let Some(e) = unsafe { error.as_ref() } {
            tracing::info!("ScreenCaptureKit access request: {}", e.localizedDescription());
        }
    });
    // SAFETY: the handler matches the documented signature; SCK copies the block.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
}

/// The worker loop: waits until a round is due and the gate lets it go, runs it, repeats.
/// `ask`: this capturer asks for Screen Recording when it is missing.
fn run(shared: Arc<Worker>, deliver: Deliver, ask: bool) {
    let (lock, cv) = &*shared;
    let mut last_full: Option<Instant> = None;
    let mut round_no: u64 = 0;
    let mut outcomes: HashMap<u32, bool> = HashMap::new();
    let mut asked = false;
    // After a probe that told nothing, the next one waits this long.
    let mut rest_until: Option<Instant> = None;
    loop {
        let (batch, permit) = {
            let Ok(mut s) = lock.lock() else {
                tracing::error!("capture state lock poisoned; thumbnails stop");
                return;
            };
            loop {
                if s.shutdown {
                    return;
                }
                let on = MODE.load(Ordering::SeqCst) == ON && !CLOSING.load(Ordering::SeqCst);
                if on && ask && !asked {
                    asked = true;
                    if !CGPreflightScreenCaptureAccess() {
                        if permissions::prompts_allowed() {
                            tracing::info!("Screen Recording missing; asking through ScreenCaptureKit");
                            request_access();
                        } else {
                            tracing::warn!("Screen Recording missing; thumbnails are off until it is granted");
                        }
                    }
                }
                let ready = on && !s.targets.is_empty() && CGPreflightScreenCaptureAccess();
                let due_in = last_full.map_or(Duration::ZERO, |t| s.interval.saturating_sub(t.elapsed()));
                let mut wait = IDLE_CHECK;
                if ready && rest_until.is_none_or(|t| Instant::now() >= t) {
                    let full = due_in.is_zero() || s.force;
                    let want: Vec<Target> = if full {
                        s.targets.clone()
                    } else {
                        s.targets.iter().filter(|t| !s.captured.contains(&t.id)).copied().collect()
                    };
                    if want.is_empty() {
                        wait = due_in.min(IDLE_CHECK);
                    } else {
                        match gate().permit(Instant::now()) {
                            Permit::All => {
                                if full {
                                    s.force = false;
                                    last_full = Some(Instant::now());
                                }
                                break (want, Permit::All);
                            }
                            // Alone: the round after it captures the rest.
                            Permit::Probe => break (want[..1].to_vec(), Permit::Probe),
                            Permit::Wait => {}
                        }
                    }
                }
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
        let result = autoreleasepool(|_| capture_round(round_no, &batch, deliver, &mut outcomes));
        let change = gate().finished(result.outcome, Instant::now());
        rest_until = (permit == Permit::Probe && result.outcome == Outcome::Inconclusive).then(|| Instant::now() + IDLE_CHECK);
        match change {
            Change::Refused(rest) => tracing::warn!(
                retry_in_s = rest.as_secs(),
                "macOS refused window captures (not permitted); thumbnails and switch snapshots are off, \
                 cards and ghosts are drawn without pictures; retrying later or from the menu"
            ),
            Change::RefusedAgain(rest) => tracing::debug!(retry_in_s = rest.as_secs(), "window captures still refused"),
            Change::Recovered => tracing::info!("window captures work again"),
            Change::None => {}
        }
        if change != Change::None {
            wake_all();
        }
        if let Ok(mut s) = lock.lock() {
            s.captured.extend(result.done);
        }
    }
}

/// The image size for a capture asked at `asked` pixels of a window whose content is
/// `content` pixels: the largest size of the content's proportions that fits in `asked`,
/// never larger than the content itself (so the image is the window, edge to edge, with no
/// padding). An unknown content size keeps what was asked.
pub fn fit_to_content(asked: (u32, u32), content: (f64, f64)) -> (u32, u32) {
    let (cw, ch) = content;
    if !(cw >= 1.0 && ch >= 1.0) {
        return (asked.0.max(1), asked.1.max(1));
    }
    let k = (f64::from(asked.0.max(1)) / cw).min(f64::from(asked.1.max(1)) / ch).min(1.0);
    ((cw * k).round().max(1.0) as u32, (ch * k).round().max(1.0) as u32)
}

/// What one round did.
struct RoundResult {
    /// Window ids that produced an image or were found missing.
    done: Vec<u32>,
    /// For the gate.
    outcome: Outcome,
}

/// Messages from ScreenCaptureKit's queues back to the worker.
enum Msg {
    /// The shareable content arrived and `n` captures were issued.
    Issued { round: u64, n: usize },
    /// One capture finished (`ok` = an image was delivered; else why not).
    Captured { round: u64, id: u32, ok: bool, why: Option<String> },
    /// A requested window is not in the shareable content.
    Missing { round: u64, id: u32 },
    /// macOS refused a call as user-declined.
    Declined { round: u64 },
}

/// Captures `batch` once; returns the window ids that produced an image or were found
/// missing (so they are not retried as "fresh" in a tight loop) and the outcome for the gate.
/// Counts every outcome and logs the windows whose outcome changed since `outcomes` last saw
/// them.
fn capture_round(round: u64, batch: &[Target], deliver: Deliver, outcomes: &mut HashMap<u32, bool>) -> RoundResult {
    let (tx, rx) = mpsc::channel::<Msg>();
    let targets: Vec<Target> = batch.to_vec();
    let content_tx = tx.clone();
    let content_flight = InFlight::issue();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let _answered = &content_flight;
        // SAFETY: SCK passes a valid object or null for each argument.
        let (content, error) = unsafe { (content.as_ref(), error.as_ref()) };
        let Some(content) = content else {
            let why = error.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
            if error.is_some_and(declined) {
                tracing::debug!("shareable content declined: {why}");
                let _ = content_tx.send(Msg::Declined { round });
            } else {
                tracing::warn!("no shareable content: {why}");
            }
            let _ = content_tx.send(Msg::Issued { round, n: 0 });
            return;
        };
        if CLOSING.load(Ordering::SeqCst) {
            let _ = content_tx.send(Msg::Issued { round, n: 0 });
            return;
        }
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
            // SAFETY: designated initializer with a live SCWindow; plain setters and getters.
            let (filter, config) = unsafe {
                let filter = SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window);
                let content = filter.contentRect();
                let scale = f64::from(filter.pointPixelScale()).max(1.0);
                let (width, height) = fit_to_content(
                    (t.width, t.height),
                    (content.size.width * scale, content.size.height * scale),
                );
                let config = SCStreamConfiguration::new();
                config.setWidth(width as usize);
                config.setHeight(height as usize);
                config.setShowsCursor(false);
                config.setIgnoreShadowsSingleWindow(true);
                config.setScalesToFit(true);
                (filter, config)
            };
            let id = t.id;
            let done_tx = content_tx.clone();
            let flight = InFlight::issue();
            let on_image = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
                let _answered = &flight;
                let (ok, why) = match NonNull::new(image) {
                    Some(ptr) => {
                        // SAFETY: SCK hands a valid CGImage for the duration of the block;
                        // retaining keeps it alive for the main thread.
                        let image = unsafe { CFRetained::retain(ptr) };
                        IMAGES.fetch_add(1, Ordering::Relaxed);
                        DispatchQueue::main().exec_async(move || deliver(id, image));
                        (true, None)
                    }
                    None => {
                        // SAFETY: SCK passes a valid NSError or null.
                        let error = unsafe { error.as_ref() };
                        if error.is_some_and(declined) {
                            let _ = done_tx.send(Msg::Declined { round });
                        }
                        let why = error.map(|e| e.localizedDescription().to_string());
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
    let mut refused = false;
    let mut failed: HashMap<u32, String> = HashMap::new();
    while expected.is_none_or(|n| finished < n) {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Msg::Issued { round: r, n }) if r == round => expected = Some(n),
            Ok(Msg::Declined { round: r }) if r == round => refused = true,
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
    // A round cut short by quit is not a failure.
    if expected.is_some() && !CLOSING.load(Ordering::SeqCst) {
        note_outcomes(batch, &handled, &failed, outcomes);
    }
    let outcome = if !handled.is_empty() {
        Outcome::Captured
    } else if refused {
        Outcome::Declined
    } else {
        Outcome::Inconclusive
    };
    RoundResult { done: all, outcome }
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

#[cfg(test)]
mod tests {
    use super::fit_to_content;

    #[test]
    fn a_capture_keeps_the_windows_proportions_and_never_grows_past_it() {
        // Asked at its own size: as asked.
        assert_eq!(fit_to_content((4202, 2540), (4202.0, 2540.0)), (4202, 2540));
        // Asked wider than the window (a stale frame): the window's shape, no padding.
        assert_eq!(fit_to_content((5042, 2540), (4202.0, 2540.0)), (4202, 2540));
        // Asked smaller and narrower: scaled down in the window's shape.
        assert_eq!(fit_to_content((300, 300), (4202.0, 2540.0)), (300, 181));
        // Unknown content: as asked.
        assert_eq!(fit_to_content((10, 0), (0.0, 0.0)), (10, 1));
    }
}
