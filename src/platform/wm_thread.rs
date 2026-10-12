//! The WM thread: a dedicated thread with its own `CFRunLoop`; it will own every
//! Accessibility call and observer.
//!
//! Other threads talk to it through [`WmHandle`]: each command goes into an mpsc channel,
//! then `CFRunLoopPerformBlock` + `CFRunLoopWakeUp` schedule a drain on the WM run loop.
//! The send happens before the block is scheduled, so a wake-up is never lost; a drain that
//! finds the channel already empty is harmless.

use std::sync::Arc;
use std::sync::mpsc::{self, Sender, SyncSender};
use std::time::Duration;

use block2::RcBlock;
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer, kCFRunLoopDefaultMode,
};

use super::apps::{AppEvent, AppInfo};
use super::native_settings::RawSettings;
use super::observe;

/// Housekeeping period of the WM run loop (retries of apps whose AX was not ready).
const TICK_SECONDS: f64 = 1.0;

/// Work for the WM thread.
pub enum WmCommand {
    /// The full list of running regular apps (sent once at launch).
    Apps(Vec<AppInfo>),
    /// A workspace event.
    App(AppEvent),
    /// Accessibility trust changed: start or stop observation.
    Accessibility(bool),
    /// Fresh `com.apple.WindowManager` values.
    NativeSettings(RawSettings),
    /// Reply with the observed apps and windows as JSON.
    Dump(SyncSender<serde_json::Value>),
    /// Drop every observer, stop the run loop, then reply.
    Shutdown(SyncSender<()>),
}

/// A `CFRunLoop` reference that may be used from other threads.
struct RemoteRunLoop(CFRetained<CFRunLoop>);

// SAFETY: CFRunLoopPerformBlock and CFRunLoopWakeUp are documented as callable from any
// thread; nothing else is called through this wrapper.
unsafe impl Send for RemoteRunLoop {}
// SAFETY: as above.
unsafe impl Sync for RemoteRunLoop {}

/// A cloneable, thread-safe handle to the WM thread.
#[derive(Clone)]
pub struct WmHandle {
    tx: Sender<WmCommand>,
    run_loop: Arc<RemoteRunLoop>,
}

impl WmHandle {
    /// Starts the WM thread and waits until its run loop exists.
    pub fn spawn() -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<WmCommand>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<RemoteRunLoop>(1);
        std::thread::Builder::new().name("wm".into()).spawn(move || {
            let Some(run_loop) = CFRunLoop::current() else {
                tracing::error!("WM thread has no run loop");
                return;
            };
            let _ = ready_tx.send(RemoteRunLoop(run_loop.clone()));
            run(rx, &run_loop);
        })?;
        let run_loop = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| std::io::Error::other("WM thread did not start"))?;
        Ok(Self { tx, run_loop: Arc::new(run_loop) })
    }

    /// Queues a command and wakes the WM run loop. Returns false when the thread is gone.
    pub fn send(&self, cmd: WmCommand) -> bool {
        if self.tx.send(cmd).is_err() {
            return false;
        }
        let block = RcBlock::new(observe::drain);
        // SAFETY: the default-mode constant is a valid static; the block is 'static and is
        // copied by CoreFoundation.
        unsafe { self.run_loop.0.perform_block(kCFRunLoopDefaultMode.map(|m| m.as_ref()), Some(&block)) };
        self.run_loop.0.wake_up();
        true
    }

    /// Asks for a JSON dump; `None` when the WM thread does not answer within `timeout`.
    pub fn dump(&self, timeout: Duration) -> Option<serde_json::Value> {
        let (tx, rx) = mpsc::sync_channel(1);
        if !self.send(WmCommand::Dump(tx)) {
            return None;
        }
        rx.recv_timeout(timeout).ok()
    }

    /// Stops observation and the WM thread; true when it confirmed within `timeout`.
    pub fn shutdown(&self, timeout: Duration) -> bool {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(WmCommand::Shutdown(tx)) && rx.recv_timeout(timeout).is_ok()
    }
}

fn run(rx: mpsc::Receiver<WmCommand>, run_loop: &CFRunLoop) {
    observe::install(rx);

    // The timer keeps the run loop alive (CFRunLoopRun returns at once with no sources)
    // and drives retries.
    let tick = RcBlock::new(|_timer: *mut CFRunLoopTimer| observe::tick());
    let first_fire = CFAbsoluteTimeGetCurrent() + TICK_SECONDS;
    // SAFETY: valid arguments; the block is copied by CoreFoundation.
    let timer = unsafe { CFRunLoopTimer::with_handler(None, first_fire, TICK_SECONDS, 0, 0, Some(&tick)) };
    match &timer {
        // SAFETY: the default-mode constant is a valid static.
        Some(t) => run_loop.add_timer(Some(t), unsafe { kCFRunLoopDefaultMode }),
        None => tracing::error!("could not create the WM tick timer"),
    }

    tracing::debug!("WM thread running");
    CFRunLoop::run();
    tracing::debug!("WM thread stopped");
}
