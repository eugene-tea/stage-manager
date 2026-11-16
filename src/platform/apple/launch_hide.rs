//! A newly opened app hidden at its launch ([`start`]), so Apple's Stage Manager keeps the
//! stage in front and puts the app's window into the strip as a stage of its own, from where
//! one Shift-click flies it into the stage in front (a probe of 11-16: a watcher on the launch
//! notices and a 2 ms poll; `hide()` returns false until the app is ready, retried until the
//! app is hidden, about 536 ms after `open`, after the will-launch notice at 422 ms and the
//! did-launch one at 492 ms; Apple's did not switch and the window went into the strip at
//! about 700 ms).
//!
//! On a thread of its own, polling every [`POLL`] for at most [`HIDE_WAIT`]: only an app with
//! the regular activation policy is ever hidden (a menu bar app or a helper never is), and
//! never once a window of it shows at full size (Apple's has switched to its stage then: the
//! hide stops, [`HideOutcome::TooLate`], and the job falls back to the round trip). How it
//! went is read from [`Hiding::outcome`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication};
use serde::Serialize;

/// The poll's pace (the probe's).
pub const POLL: Duration = Duration::from_millis(2);

/// How long the hide is tried before it is given up.
pub const HIDE_WAIT: Duration = Duration::from_millis(2500);

/// How often the window list is looked at for a window of the app shown already (a list read
/// costs about a millisecond: not at every poll).
const WINDOW_LOOK: Duration = Duration::from_millis(10);

/// True when app `pid` has a window on screen at full size: larger than a strip thumbnail, or
/// away from the strip's column (a window that went into the strip is no window shown).
fn shows_a_window(pid: i32) -> bool {
    use crate::engine::apple::THUMBNAIL_MAX;
    crate::platform::windows::on_screen().iter().any(|w| {
        w.pid == pid && w.layer == 0 && (w.bounds.w > THUMBNAIL_MAX || w.bounds.h > THUMBNAIL_MAX || w.bounds.x > 300.0)
    })
}

/// How the hide went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HideOutcome {
    /// Hidden, `ms` after the hide began, after `tries` asks.
    Hidden {
        /// From the start to the app hidden.
        ms: u64,
        /// `hide()` asked this many times.
        tries: u32,
    },
    /// Not hidden in [`HIDE_WAIT`] (never ready, or not a regular app).
    TimedOut {
        /// The wait.
        ms: u64,
    },
    /// A window of the app showed before the hide took: not hidden (the round trip then).
    TooLate {
        /// When the window was seen.
        ms: u64,
    },
    /// The app quit meanwhile.
    Gone,
    /// Called off (the Apple mode ended).
    Cancelled,
}

/// A hide under way or done.
#[derive(Debug, Clone)]
pub struct Hiding {
    /// The app.
    pub pid: i32,
    /// When it began.
    pub since: Instant,
    outcome: Arc<Mutex<Option<HideOutcome>>>,
    cancel: Arc<AtomicBool>,
}

impl Hiding {
    /// How it went, once it has.
    pub fn outcome(&self) -> Option<HideOutcome> {
        self.outcome.lock().ok().and_then(|o| *o)
    }

    /// Calls the hide off: nothing more is asked of the app.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Starts hiding app `pid` (see the module docs).
pub fn start(pid: i32) -> Result<Hiding, String> {
    let outcome = Arc::new(Mutex::new(None));
    let out = outcome.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let called_off = cancel.clone();
    let since = Instant::now();
    std::thread::Builder::new()
        .name("apple-launch-hide".into())
        .spawn(move || {
            let got = hide_loop(pid, since, &called_off);
            tracing::info!(pid, outcome = ?got, "apple mode: a newly opened app hidden at its launch");
            if let Ok(mut o) = out.lock() {
                *o = Some(got);
            }
        })
        .map_err(|e| format!("no thread for the hide: {e}"))?;
    Ok(Hiding { pid, since, outcome, cancel })
}

fn hide_loop(pid: i32, since: Instant, cancel: &AtomicBool) -> HideOutcome {
    let mut tries = 0;
    let mut looked: Option<Instant> = None;
    while since.elapsed() < HIDE_WAIT {
        if cancel.load(Ordering::SeqCst) {
            return HideOutcome::Cancelled;
        }
        // A window of the app on screen before the hide took: Apple's has switched to its
        // stage already, and a hide now would empty the stage in front. Left as it is: the
        // job falls back to the round trip.
        if looked.is_none_or(|t| t.elapsed() >= WINDOW_LOOK) {
            looked = Some(Instant::now());
            if shows_a_window(pid) {
                return HideOutcome::TooLate { ms: since.elapsed().as_millis() as u64 };
            }
        }
        if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
            if app.isTerminated() {
                return HideOutcome::Gone;
            }
            if app.activationPolicy() == NSApplicationActivationPolicy::Regular {
                if app.isHidden() {
                    return HideOutcome::Hidden { ms: since.elapsed().as_millis() as u64, tries };
                }
                tries += 1;
                // False until the app is ready (the probe): asked again at the next poll.
                let _ = app.hide();
            }
        }
        std::thread::sleep(POLL);
    }
    HideOutcome::TimedOut { ms: since.elapsed().as_millis() as u64 }
}

/// Shows app `pid` again (a hide whose window never came in; a job given up).
pub fn unhide(pid: i32) {
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        && app.isHidden()
    {
        let _ = app.unhide();
    }
}

/// True when app `pid` is hidden now.
pub fn is_hidden(pid: i32) -> bool {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid).is_some_and(|a| a.isHidden())
}
