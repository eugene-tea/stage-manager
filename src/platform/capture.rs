//! Window thumbnails through ScreenCaptureKit: stills of the windows shown on strip cards.
//!
//! A worker thread takes a still of every requested window each interval with
//! `SCScreenshotManager` and an `SCContentFilter(desktopIndependentWindow:)`, sized as asked
//! (the strip asks for 2× its thumbnail size) but always in the window's own proportions
//! ([`capture_frame::fit_to_content`]): asked for another shape, ScreenCaptureKit pads the image
//! and pins the window to its top-left corner (measured 10-12), so a snapshot stretched over the
//! window's frame drew its content smaller and off to one side. The filter takes the window with
//! the child windows ScreenCaptureKit includes (a popover), as Apple's thumbnails show them; each picture says where the window is in
//! it ([`Picture::extent`], [`window_shot`]), so a popup sticking out is drawn beside the window,
//! never squeezed into its frame (10-16). Each picture goes to the main thread through
//! GCD, keyed by window id. Parked windows still render, so they capture too.
//!
//! **A child over the window, not in its content** (round 8M, the owner's Calendar card on
//! 10-21): when another window of the app overlaps the window and reaches past it, yet the
//! filter's `contentRect` is the window's own frame, the image still holds the union (fitted
//! into the window's proportions, a fill where no window is) and nothing tells where the window
//! is in it. Then the child windows are left out (`SCStreamConfiguration.includeChildWindows`,
//! macOS 14.2: "show the child windows … Child windows are included by default"), so the
//! picture is the window's own pixels. A picture whose corners are an opaque flat fill while a
//! window of its app overlaps it is not used either ([`capture_pixels::flat_fill`]); such
//! captures log their corner and overlap boxes' numbers at debug, once per change.
//!
//! Only the requested windows are captured; the caller replaces the request whenever the
//! strip changes and an empty request stops captures (the strip is hidden).
//!
//! Each capture counts in the day's log (ok or failed), and each round that brought nothing
//! back once more (`capture_rounds_failed`, round 8M: the day line reads rounds, not windows
//! times rounds); a window's captures starting to fail, and working again, are logged once
//! each, not every round.
//!
//! **Paced** (round 8M, [`capture_pace`]): rounds that keep failing (ScreenCaptureKit
//! answering "Failed to start stream…" or nothing at all, for minutes, 10-18) back off,
//! doubling up to 30 s, until one works; no round goes while the screen is locked or the
//! display sleeps ([`set_away`], told by the controller). The cards keep their last pictures.
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
//! - The one still that is not of a window, the desktop in the park corner for a switch's
//!   cover ([`capture_desktop`], round 8L3), follows the same rules and goes only once the
//!   gate is open (window captures already worked): it never asks and is never the probe.
//! - Quit waits for captures in flight ([`quiesce`]): replayd checks a capture's approval
//!   after answering it, and for a process that has just exited it cannot resolve the app's
//!   path, so it alerts as if the app were new (see the guide's Known limitations).

use std::collections::{HashMap, HashSet};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, Once, Weak};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::AllocAnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGImage, CGPreflightScreenCaptureAccess, kCGColorClear};
use objc2_foundation::{NSArray, NSError, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration, SCStreamErrorCode,
    SCStreamErrorDomain, SCWindow,
};

use super::capture_frame::{self, Extent};
use super::capture_pixels::{self, PixelRect, Stats};
use super::capture_gate::{Change, Gate, Outcome, Permit};
use super::capture_pace::{self, Away, Pace};
use super::permissions;
use crate::engine::geometry::Rect;
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

/// A window's picture: the image and where the window is in it.
#[derive(Clone, Debug)]
pub struct Picture {
    /// The window with its child windows, on a clear background.
    pub image: CFRetained<CGImage>,
    /// How far the image reaches past the window (child windows sticking out); none for most.
    pub extent: Extent,
}

/// Receives each picture on the main thread.
pub type Deliver = fn(u32, Picture);

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

/// The one pace of the process ([`capture_pace`]): every capturer's rounds back off together
/// and pause together.
static PACE: LazyLock<Mutex<Pace>> = LazyLock::new(|| Mutex::new(Pace::default()));

fn pace() -> MutexGuard<'static, Pace> {
    PACE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether anybody can see the screen (the controller's word, every tick): while the screen
/// is locked or the display sleeps (`Some`), no capture round goes; the first one after goes
/// at once. Each change is logged once.
pub fn set_away(away: Option<Away>) {
    let change = pace().set_away(away, Instant::now());
    log_pace(change);
    if matches!(change, capture_pace::Change::Resumed { .. }) {
        wake_all();
    }
}

/// Logs a change of the pace, once.
fn log_pace(change: capture_pace::Change) {
    use capture_pace::Change as C;
    match change {
        C::None => {}
        C::BackingOff { failed, wait } => tracing::info!(
            failed,
            wait_s = wait.as_secs(),
            cap_s = capture_pace::BACKOFF_CAP.as_secs(),
            "window captures keep failing: rounds back off (twice as long after each failure, up to the cap) until one works; cards keep their last pictures"
        ),
        C::BackoffOver { failed, lasted } => {
            tracing::info!(failed, lasted_s = lasted.as_secs(), "window captures work again: the back-off is over")
        }
        C::Paused(away) => tracing::info!("window captures pause: {}", away.as_str()),
        C::Resumed { lasted } => tracing::info!(paused_s = lasted.as_secs(), "window captures resume"),
    }
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
        "pace": pace().describe(),
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
            .spawn(move || {
                // A panic in a capture round must not take the app down (Fable M2): the
                // thumbnails stop updating (the cards keep their last pictures).
                if super::safety::guarded("the capture thread", || run(worker, deliver, ask)).is_none() {
                    tracing::error!("window captures stopped after a panic; thumbnails stay as they are until the next launch");
                }
            });
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
                // Paused (nobody sees the screen: woken when that ends) or backing off.
                let paced = if ready { pace().wait(Instant::now()) } else { Some(Duration::ZERO) };
                if let Some(left) = paced.filter(|d| !d.is_zero()) {
                    wait = left.min(IDLE_CHECK);
                }
                if ready && paced.is_some_and(|d| d.is_zero()) && rest_until.is_none_or(|t| Instant::now() >= t) {
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
        if result.timed_out {
            // Loud for the first of a streak only: the pace logs the back-off once.
            if pace().failed_in_a_row() == 0 {
                tracing::warn!(round = round_no, "capture round timed out");
            } else {
                tracing::debug!(round = round_no, "capture round timed out (failing already)");
            }
        }
        if let Some(ok) = result.paced {
            if !ok {
                counters::bump(Count::CaptureRoundFailed);
            }
            log_pace(pace().finished(ok, Instant::now()));
        }
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

/// One window's capture, set up: the filter, the configuration, and what goes with them.
pub(crate) struct WindowShot {
    pub filter: Retained<SCContentFilter>,
    pub config: Retained<SCStreamConfiguration>,
    /// Pixels per point.
    pub scale: f64,
    /// Where the window is in the picture.
    pub extent: Extent,
    /// The configuration's background color: it does not retain it, so the caller keeps this
    /// until the capture answered.
    pub clear: Option<CFRetained<CGColor>>,
    /// Where other windows of the app overlap the window, in the window's own points (top-left
    /// origin); none for most (round 8M: the picture's numbers are read for these).
    pub overlaps: Vec<Rect>,
    /// The window's size in points.
    pub size_pt: (f64, f64),
    /// Child windows were left out of the picture (a child over the window that the content
    /// rectangle does not show, round 8M).
    pub children_left_out: bool,
}

/// Logs at info, once per process, how the first capture whose content is larger than its
/// window (a child included) found the window in it (global or local `contentRect`), for the
/// owner's live run to settle. A content of the window's own size never logs it (the 10-16
/// run: the fixture's `addChildWindow` panel was not in the content); [`log_reading`] shows
/// every reading at debug.
static FOUND_LOGGED: Once = Once::new();

/// What a window's last capture read (its frame, the filter's content, the plan), per window:
/// logged at debug when it changes, so every reading shows once, not every round.
static LAST_READ: LazyLock<Mutex<HashMap<u32, Reading>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Windows remembered in [`LAST_READ`] before it starts over.
const LAST_READ_CAP: usize = 512;

/// One capture's reading of where its window is.
#[derive(Clone, Copy, PartialEq)]
struct Reading {
    frame: Rect,
    content: Rect,
    scale: f64,
    plan: capture_frame::Plan,
}

/// Logs at debug the window's frame, the filter's `contentRect`, its scale and the picture's
/// extent (ids and points only, never titles) when they differ from the window's last capture.
/// `reaching`: the frames of the app's windows overlapping the window and reaching past it
/// (its children among them).
fn log_reading(id: u32, read: Reading, reaching: &[Rect]) {
    let Ok(mut last) = LAST_READ.lock() else { return };
    if last.get(&id) == Some(&read) {
        return;
    }
    if last.len() >= LAST_READ_CAP {
        last.clear();
    }
    last.insert(id, read);
    let Reading { frame, content, scale, plan } = read;
    tracing::debug!(
        window = id,
        frame = %fmt_rect(frame),
        content = %fmt_rect(content),
        scale,
        extent = ?plan.extent,
        source = ?plan.source.map(fmt_rect),
        found = plan.found.map(|f| f.as_str()),
        size = ?plan.size,
        reaching = ?reaching.iter().map(|r| fmt_rect(*r)).collect::<Vec<_>>(),
        "capture: window frame, contentRect and extent"
    );
}

/// `x,y wxh` in points.
fn fmt_rect(r: Rect) -> String {
    format!("{},{} {}x{}", r.x, r.y, r.w, r.h)
}

/// Every display's frame in the shareable content (global, top-left origin, points).
pub(crate) fn display_frames(content: &SCShareableContent) -> Vec<Rect> {
    // SAFETY: plain getters on a live SCShareableContent and its SCDisplays.
    unsafe { content.displays() }.iter().map(|d| to_rect(unsafe { d.frame() })).collect()
}

/// The capture of `window` (one of `windows`, the shareable content, on `displays`), the
/// window's part at `asked` pixels: the window with its child windows as ScreenCaptureKit
/// takes them, on a clear background with alpha (no area outside a window is ever opaque),
/// and where the window is in the picture ([`capture_frame`]). When that is not known, the
/// picture is taken as the window, as before; when the children reach too far (a palette
/// far off, or on another display), the window alone, cropped to its frame. Every capture of
/// a window uses it.
pub(crate) fn window_shot(window: &SCWindow, windows: &NSArray<SCWindow>, displays: &[Rect], asked: (u32, u32)) -> WindowShot {
    // SAFETY: plain getters on live SCK objects.
    let (frame, pid) = unsafe { (window.frame(), window.owningApplication().map(|a| a.processID())) };
    let id = unsafe { window.windowID() };
    let frame = to_rect(frame);
    let others: Vec<Rect> = match pid {
        Some(pid) => windows
            .iter()
            // SAFETY: plain getters on live SCWindows.
            .filter(|w| unsafe { w.windowID() != id && w.owningApplication().is_some_and(|a| a.processID() == pid) })
            // SAFETY: a plain getter on a live SCWindow.
            .map(|w| to_rect(unsafe { w.frame() }))
            .collect(),
        None => Vec::new(),
    };
    // SAFETY: designated initializer with a live SCWindow; plain getters.
    let (filter, content, scale) = unsafe {
        let filter = SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window);
        let content = to_rect(filter.contentRect());
        let scale = f64::from(filter.pointPixelScale());
        (filter, content, scale)
    };
    let plan = capture_frame::plan(asked, content, scale, frame, &others, displays);
    if let Some(found) = plan.found {
        FOUND_LOGGED.call_once(|| tracing::info!("capture: contentRect is {}", found.as_str()));
    }
    let reaching: Vec<Rect> =
        others.iter().filter(|o| o.intersect(&frame).is_some() && o.union(&frame) != frame).copied().collect();
    log_reading(id, Reading { frame, content, scale, plan }, &reaching);
    let overlaps: Vec<Rect> = others
        .iter()
        .filter_map(|o| o.intersect(&frame))
        .map(|i| Rect::new(i.x - frame.x, i.y - frame.y, i.w, i.h))
        .collect();
    // A child over the window and past it that the content rectangle does not show (10-21):
    // the image would hold the union squeezed into the window's shape; the window alone.
    let children_left_out = !reaching.is_empty() && plan.found.is_none() && plan.extent.is_none();
    // SAFETY: reading an immutable CoreGraphics constant.
    let clear = CGColor::constant_color(Some(unsafe { kCGColorClear }));
    // SAFETY: a plain initializer, then plain setters on the live configuration; the
    // background color is kept alive by the caller (`clear`) until the capture answered.
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(plan.size.0 as usize);
        config.setHeight(plan.size.1 as usize);
        config.setPixelFormat(PIXEL_BGRA);
        if let Some(r) = plan.source {
            config.setSourceRect(CGRect::new(CGPoint::new(r.x, r.y), CGSize::new(r.w, r.h)));
        }
        if let Some(clear) = clear.as_deref() {
            config.setBackgroundColor(clear);
        }
    }
    if children_left_out {
        leave_children_out(&config, id);
    }
    WindowShot {
        filter,
        config,
        scale: if scale.is_finite() { scale.max(1.0) } else { 1.0 },
        extent: plan.extent,
        clear,
        overlaps,
        size_pt: (frame.w, frame.h),
        children_left_out,
    }
}

/// Windows whose children were left out, logged once each (this process).
static LEFT_OUT_SAID: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Leaves child windows out of `config`'s capture (macOS 14.2 and later; before, the
/// picture's corners are still checked, [`capture_pixels::flat_fill`]).
fn leave_children_out(config: &SCStreamConfiguration, id: u32) {
    let sel = objc2::sel!(setIncludeChildWindows:);
    // SAFETY: asking whether a live object responds to a selector.
    let can: bool = unsafe { objc2::msg_send![config, respondsToSelector: sel] };
    if can {
        // SAFETY: a plain setter, present (checked above).
        unsafe { config.setIncludeChildWindows(false) };
    }
    if LEFT_OUT_SAID.lock().is_ok_and(|mut s| s.len() < LAST_READ_CAP && s.insert(id)) {
        tracing::debug!(window = id, applied = can, "capture: a child window over this window and past it is not in its content: child windows left out");
    }
}

/// The numbers a picture with another window of its app over it last read, per window: logged
/// when they change (round 8M).
static LAST_FILL: LazyLock<Mutex<HashMap<u32, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// For a picture of window `id` with other windows of its app over it (`overlaps`, window
/// points; the window `size_pt`): the four corner boxes' and each overlap's mean alpha,
/// brightness and spread (numbers only), logged at debug when they change. True when the
/// picture holds a fill ([`capture_pixels::holds_fill`]): it is not used.
fn fill_check(id: u32, image: &CGImage, extent: Extent, overlaps: &[Rect], size_pt: (f64, f64), left_out: bool) -> bool {
    let Some(px) = capture_pixels::read(image) else { return false };
    let corners = capture_pixels::corner_boxes(px.w, px.h).map(|r| capture_pixels::stats(&px, r).unwrap_or_default());
    // The window's part of the picture, then each overlap within it, in pixels.
    let whole = Rect::new(0.0, 0.0, px.w as f64, px.h as f64);
    let win = extent.window_in(whole);
    let (kx, ky) = (win.w / size_pt.0.max(1.0), win.h / size_pt.1.max(1.0));
    let over: Vec<Stats> = overlaps
        .iter()
        .filter_map(|o| {
            let r = PixelRect {
                x: (win.x + o.x * kx).max(0.0) as usize,
                y: (win.y + o.y * ky).max(0.0) as usize,
                w: (o.w * kx).round().max(1.0) as usize,
                h: (o.h * ky).round().max(1.0) as usize,
            };
            capture_pixels::stats(&px, r)
        })
        .collect();
    let flat = capture_pixels::holds_fill(&corners, extent.is_none());
    let line = format!(
        "corners=[{}] overlaps=[{}] fill={flat}",
        corners.iter().map(Stats::to_string).collect::<Vec<_>>().join(", "),
        over.iter().map(Stats::to_string).collect::<Vec<_>>().join(", ")
    );
    if let Ok(mut last) = LAST_FILL.lock() {
        if last.len() >= LAST_READ_CAP {
            last.clear();
        }
        if last.get(&id) != Some(&line) {
            tracing::debug!(window = id, w = px.w, h = px.h, children_left_out = left_out, "capture: a window of its app over it: {line}");
            last.insert(id, line);
        }
    }
    flat
}

/// 32-bit BGRA with alpha ('BGRA'), ScreenCaptureKit's default, set so it stays so.
const PIXEL_BGRA: u32 = u32::from_be_bytes(*b"BGRA");

fn to_rect(r: CGRect) -> Rect {
    Rect::new(r.origin.x, r.origin.y, r.size.width, r.size.height)
}

/// What one round did.
struct RoundResult {
    /// Window ids that produced an image or were found missing.
    done: Vec<u32>,
    /// For the gate.
    outcome: Outcome,
    /// No answer came in time.
    timed_out: bool,
    /// For the pace ([`capture_pace`]): `Some(true)` an image came back, `Some(false)` the
    /// round failed (captures issued and none brought an image back, or no answer in time),
    /// `None` it tells nothing (cut short by quit, refused, or only windows gone).
    paced: Option<bool>,
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
        let displays = display_frames(content);
        let mut issued = 0;
        for t in &targets {
            // SAFETY: plain getter on a live SCWindow.
            let Some(window) = windows.iter().find(|w| unsafe { w.windowID() } == t.id) else {
                tracing::trace!(window = t.id, "window not in shareable content");
                let _ = content_tx.send(Msg::Missing { round, id: t.id });
                continue;
            };
            let shot = window_shot(&window, &windows, &displays, (t.width, t.height));
            // SAFETY: plain setters on a live configuration.
            unsafe {
                shot.config.setShowsCursor(false);
                shot.config.setIgnoreShadowsSingleWindow(true);
                shot.config.setScalesToFit(true);
            }
            let WindowShot { filter, config, clear, extent, overlaps, size_pt, children_left_out, .. } = shot;
            let id = t.id;
            let done_tx = content_tx.clone();
            let flight = InFlight::issue();
            let on_image = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
                let _answered = &flight;
                // The configuration's background, alive until the capture answered.
                let _background = &clear;
                let (ok, why) = match NonNull::new(image) {
                    Some(ptr) => {
                        // SAFETY: SCK hands a valid CGImage for the duration of the block;
                        // retaining keeps it alive for the main thread.
                        let image = unsafe { CFRetained::retain(ptr) };
                        // A window of its app over it: the numbers, and no fill on a card (8M).
                        let fill = !overlaps.is_empty() && fill_check(id, &image, extent, &overlaps, size_pt, children_left_out);
                        if !fill {
                            IMAGES.fetch_add(1, Ordering::Relaxed);
                            DispatchQueue::main().exec_async(move || deliver(id, Picture { image, extent }));
                        }
                        // ScreenCaptureKit answered: a picture not used is no failure.
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
    let mut timed_out = false;
    let mut errors = 0usize;
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
                    errors += 1;
                    failed.insert(id, why.unwrap_or_else(|| "no image".into()));
                }
            }
            Ok(Msg::Missing { round: r, id }) if r == round => {
                failed.insert(id, "not in the shareable content".into());
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {
                // Logged by the worker (loud once a streak, [`capture_pace`]).
                timed_out = true;
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
    let paced = if CLOSING.load(Ordering::SeqCst) || outcome == Outcome::Declined {
        None
    } else if outcome == Outcome::Captured {
        Some(true)
    } else if timed_out || errors > 0 {
        Some(false)
    } else {
        None
    };
    RoundResult { done: all, outcome, timed_out, paced }
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

/// What [`capture_desktop`] hands back: the still and its pixels a point, or why there is none.
pub type DesktopStill = Result<(CFRetained<CGImage>, f64), String>;

/// What [`capture_desktop`] calls with the still, once.
pub type DesktopDone = Box<dyn FnOnce(DesktopStill) + Send>;

/// Whether a still of the desktop may be taken now ([`capture_desktop`]): captures on, not
/// quitting, Screen Recording granted (never asked for here), and the gate open: window
/// captures already work, so this one goes with them and is never the process's probe.
pub fn desktop_may_capture() -> Result<(), &'static str> {
    if MODE.load(Ordering::SeqCst) != ON {
        return Err("window captures are off");
    }
    if CLOSING.load(Ordering::SeqCst) {
        return Err("quitting");
    }
    if !CGPreflightScreenCaptureAccess() {
        return Err("no Screen Recording access");
    }
    match gate().state() {
        super::capture_gate::State::Open => Ok(()),
        _ => Err("window captures have not worked yet in this run (or rest after a refusal)"),
    }
}

/// Whether a window ScreenCaptureKit lists at `layer`, of process `pid`, is part of the
/// desktop a still may show ([`capture_desktop`], round 8L4): below the normal level (the
/// desktop picture, Finder's desktop icons, desktop widgets, the window server's backdrop)
/// and never this app's. Everything else, every window at the normal level or above (an
/// app's, a notification, the Dock, the menu bar) and any window that appears after the
/// list was read, is left out by construction.
pub fn desktop_level(layer: isize, pid: Option<i32>, own: i32) -> bool {
    layer < 0 && pid != Some(own)
}

/// The desktop windows' layers were logged once (which ones a still holds: the desktop
/// picture's own window among them or not, [`capture_desktop`]).
static DESKTOP_LAYERS_SAID: AtomicBool = AtomicBool::new(false);

/// A still of `region` (global, top-left points) of the main display showing the desktop
/// only: only the desktop-level windows listed when it is taken ([`desktop_level`]: the
/// desktop picture and what lies on it, Finder's icons, widgets) are included (round 8L4:
/// the filter names the windows it shows, so a window macOS raises in the corner, or a
/// notification, between the list and the picture is never in it), on a clear background.
/// Apple's filter documentation says the desktop background is left out of such a filter:
/// when the desktop picture's window is not drawn, the still holds the icons only and is see-
/// through elsewhere; [`super::desktop_shot`] then lays it over the corner patch's own
/// picture of the wallpaper. The switch's cover in the park corner when a desktop icon lies
/// there.
///
/// Quiet by design as the window captures are ([`desktop_may_capture`]: nothing is asked,
/// nothing goes before the gate is open); a refusal closes the gate for every capturer. `done`
/// runs once, on a ScreenCaptureKit queue (`Err` when no desktop-level window is listed).
/// `Err` when it is not taken at all.
pub fn capture_desktop(region: Rect, done: DesktopDone) -> Result<(), &'static str> {
    desktop_may_capture()?;
    let done = Arc::new(Mutex::new(Some(done)));
    let finish = move |slot: &Mutex<Option<DesktopDone>>, still: DesktopStill| {
        if let Some(f) = slot.lock().ok().and_then(|mut d| d.take()) {
            f(still);
        }
    };
    let own = std::process::id() as i32;
    let content_flight = InFlight::issue();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let _answered = &content_flight;
        // SAFETY: SCK passes a valid object or null for each argument.
        let (content, error) = unsafe { (content.as_ref(), error.as_ref()) };
        let Some(content) = content else {
            if error.is_some_and(declined) {
                desktop_declined();
            }
            let why = error.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
            finish(&done, Err(format!("no shareable content: {why}")));
            return;
        };
        if CLOSING.load(Ordering::SeqCst) {
            finish(&done, Err("quitting".into()));
            return;
        }
        let main = objc2_core_graphics::CGMainDisplayID();
        // SAFETY: plain getters on a live SCShareableContent and its SCDisplays.
        let display = unsafe { content.displays() }.iter().find(|d| unsafe { d.displayID() } == main);
        let Some(display) = display else {
            finish(&done, Err("the main display is not in the shareable content".into()));
            return;
        };
        // SAFETY: plain getters on live SCWindows and their apps.
        let included: Vec<Retained<SCWindow>> = unsafe { content.windows() }
            .iter()
            .filter(|w| unsafe { desktop_level(w.windowLayer(), w.owningApplication().map(|a| a.processID()), own) })
            .collect();
        if included.is_empty() {
            finish(&done, Err("no desktop-level window in the shareable content".into()));
            return;
        }
        if !DESKTOP_LAYERS_SAID.swap(true, Ordering::SeqCst) {
            // SAFETY: a plain getter on live SCWindows.
            let layers: Vec<isize> = included.iter().map(|w| unsafe { w.windowLayer() }).collect();
            tracing::debug!(?layers, "the desktop's still shows only these desktop-level windows (logged once)");
        }
        let included = NSArray::from_retained_slice(&included);
        // Kept alive until the picture is answered (the configuration does not retain it).
        // SAFETY: a constant name CoreGraphics defines.
        let clear = CGColor::constant_color(Some(unsafe { kCGColorClear }));
        // SAFETY: designated initializer with a live SCDisplay and array; plain setters and getters.
        let (filter, config, scale) = unsafe {
            let filter = SCContentFilter::initWithDisplay_includingWindows(SCContentFilter::alloc(), &display, &included);
            let scale = f64::from(filter.pointPixelScale()).max(1.0);
            let frame = display.frame();
            let config = SCStreamConfiguration::new();
            config.setSourceRect(CGRect::new(
                CGPoint::new(region.x - frame.origin.x, region.y - frame.origin.y),
                CGSize::new(region.w, region.h),
            ));
            config.setWidth((region.w * scale).round().max(1.0) as usize);
            config.setHeight((region.h * scale).round().max(1.0) as usize);
            config.setShowsCursor(false);
            if let Some(clear) = clear.as_deref() {
                config.setBackgroundColor(clear);
            }
            (filter, config, scale)
        };
        let flight = InFlight::issue();
        let done = done.clone();
        let on_image = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
            let _answered = (&flight, &clear);
            match NonNull::new(image) {
                Some(ptr) => {
                    // SAFETY: SCK hands a valid CGImage for the duration of the block;
                    // retaining keeps it alive past it.
                    let image = unsafe { CFRetained::retain(ptr) };
                    counters::bump(Count::CaptureOk);
                    finish(&done, Ok((image, scale)));
                }
                None => {
                    // SAFETY: SCK passes a valid NSError or null.
                    let error = unsafe { error.as_ref() };
                    if error.is_some_and(declined) {
                        desktop_declined();
                    }
                    counters::bump(Count::CaptureFailed);
                    let why = error.map_or_else(|| "no image".to_string(), |e| e.localizedDescription().to_string());
                    finish(&done, Err(why));
                }
            }
        });
        // SAFETY: filter, configuration and handler are valid; SCK copies the block.
        unsafe { SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(&filter, &config, Some(&on_image)) };
    });
    // SAFETY: the handler matches the documented signature; SCK copies the block. Desktop
    // windows are listed (only they are included in the still).
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(false, true, &handler);
    }
    Ok(())
}

/// macOS refused a desktop still as user-declined: every capturer rests ([`Gate`]).
fn desktop_declined() {
    let change = gate().finished(Outcome::Declined, Instant::now());
    if let Change::Refused(rest) | Change::RefusedAgain(rest) = change {
        tracing::warn!(retry_in_s = rest.as_secs(), "macOS refused a capture of the desktop (not permitted); window captures rest");
    }
    wake_all();
}

#[cfg(test)]
mod tests {
    use super::desktop_level;

    #[test]
    fn a_still_of_the_desktop_holds_only_desktop_level_windows_never_ours() {
        // The window server's list on a plain desktop (10-16, as the overview tests record
        // it): the desktop picture's, WindowManager's, the window server's backdrop and
        // Finder's icons far below layer 0; the Dock, the menu bar, our overlay, a fixture.
        let own = 900;
        let listed: [(isize, Option<i32>, bool); 10] = [
            (-2147483625, Some(101), true),  // Wallpaper
            (-2147483624, Some(102), true),  // WindowManager
            (-2147483626, None, true),       // Window Server's backdrop
            (-2147483603, Some(103), true),  // Finder's desktop icons
            (-2147483603, Some(own), false), // ours, even at a desktop level
            (0, Some(104), false),           // an app's window (raised in the corner)
            (20, Some(105), false),          // the Dock
            (24, Some(106), false),          // the menu bar
            (25, Some(own), false),          // our motion overlay
            (101, Some(107), false),         // a notification
        ];
        for (layer, pid, want) in listed {
            assert_eq!(desktop_level(layer, pid, own), want, "layer {layer}, pid {pid:?}");
        }
        assert!(!listed.iter().filter(|(l, p, _)| desktop_level(*l, *p, own)).any(|(l, _, _)| *l >= 0));
    }
}
