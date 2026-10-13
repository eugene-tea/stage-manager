//! The WM thread: a dedicated thread with its own `CFRunLoop` that owns every
//! Accessibility call, the [`Observer`] and the [`Controller`] (the engine).
//!
//! Other threads talk to it through [`WmHandle`]: each command goes into an mpsc channel,
//! then `CFRunLoopPerformBlock` + `CFRunLoopWakeUp` schedule a drain on the WM run loop.
//! The send happens before the block is scheduled, so a wake-up is never lost; a drain that
//! finds the channel already empty is harmless.
//!
//! Every entry point (command drain, AX callback, timer) runs inside
//! [`safety::guarded`]: a panic is caught, logged and followed by a registry resync, instead
//! of killing the thread that owns the windows.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::time::Duration;

use block2::RcBlock;
use objc2_application_services::{AXObserver, AXUIElement};
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer, CFString, kCFRunLoopDefaultMode,
};

use super::apps::{AppEvent, AppInfo};
use super::ax::AxElement;
use super::controller::Controller;
use super::ctl::Command;
use super::input::InputEvent;
use super::native_settings::RawSettings;
use super::observe::{self, Observer};
use super::safety;
use crate::config::HotkeyCommand;
use crate::engine::model::Display;
use crate::ui::strip_model::{StripDrop, StripEvent};

/// Global AX messaging timeout, set on the system-wide element at thread start.
pub const AX_TIMEOUT_SECONDS: f32 = 0.25;

/// Housekeeping period of the WM run loop (saves, config polling, strip refresh).
const TICK_SECONDS: f64 = 0.25;

/// App-observation retries run every this many ticks (about once a second).
const RETRY_EVERY: u32 = 4;

/// Work for the WM thread.
pub enum WmCommand {
    /// The full list of running regular apps (sent once at launch).
    Apps(Vec<AppInfo>),
    /// A workspace event.
    App(AppEvent),
    /// Accessibility trust changed: start or stop observation. The first grant runs the
    /// recovery scan before observation starts.
    Accessibility(bool),
    /// Fresh `com.apple.WindowManager` values.
    NativeSettings(RawSettings),
    /// The main display (first) and its visible frame, top-left coordinates.
    Displays(Vec<Display>),
    /// Global input in another app (left mouse down/up, key down); for a mouse-up over the
    /// visible strip, the slot a window dropped there goes to.
    Input(InputEvent, Option<StripDrop>),
    /// A hotkey was pressed.
    Hotkey(HotkeyCommand),
    /// Input from the strip.
    Strip(StripEvent),
    /// The user went to another Space (`activeSpaceDidChangeNotification`).
    SpaceChanged,
    /// Show or hide the strip.
    ToggleStrip,
    /// Re-read config.toml.
    ReloadConfig,
    /// A ctl command for the engine; the reply is one JSON value.
    Ctl(Command, SyncSender<serde_json::Value>),
    /// Unpark every window, save state, drop every observer, stop the run loop, then reply
    /// with whether every window came back.
    Shutdown(SyncSender<bool>),
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
        let block = RcBlock::new(drain);
        // SAFETY: the default-mode constant is a valid static; the block is 'static and is
        // copied by CoreFoundation.
        unsafe { self.run_loop.0.perform_block(kCFRunLoopDefaultMode.map(|m| m.as_ref()), Some(&block)) };
        self.run_loop.0.wake_up();
        true
    }

    /// Runs a ctl command on the WM thread; `None` when it does not answer within `timeout`.
    pub fn ctl(&self, cmd: Command, timeout: Duration) -> Option<serde_json::Value> {
        let (tx, rx) = mpsc::sync_channel(1);
        if !self.send(WmCommand::Ctl(cmd, tx)) {
            return None;
        }
        rx.recv_timeout(timeout).ok()
    }

    /// Unparks every window and stops the WM thread. `Some(true)` when every window came
    /// back, `None` when the thread did not answer within `timeout`.
    pub fn shutdown(&self, timeout: Duration) -> Option<bool> {
        let (tx, rx) = mpsc::sync_channel(1);
        if !self.send(WmCommand::Shutdown(tx)) {
            return None;
        }
        rx.recv_timeout(timeout).ok()
    }
}

/// Everything the WM thread owns.
struct WmState {
    observer: Observer,
    controller: Controller,
    scanned: bool,
    ticks: u32,
}

thread_local! {
    static STATE: RefCell<Option<WmState>> = const { RefCell::new(None) };
    static INBOX: RefCell<Option<Receiver<WmCommand>>> = const { RefCell::new(None) };
}

fn with_state(f: impl FnOnce(&mut WmState)) {
    STATE.with(|s| match s.try_borrow_mut() {
        Ok(mut guard) => {
            if let Some(state) = guard.as_mut() {
                f(state);
            }
        }
        Err(_) => tracing::warn!("WM state busy (re-entrant callback); event dropped"),
    });
}

/// Runs `f` on the state inside a guarded section; after a caught panic the registry is
/// resynced from the model.
fn guarded_state(what: &str, f: impl FnOnce(&mut WmState)) {
    if safety::guarded(what, || with_state(f)).is_none() {
        let _ = safety::guarded("panic recovery", || with_state(WmState::after_panic));
    }
}

/// Handles every queued command (scheduled by `WmHandle::send`). Once Quit is requested,
/// every command but `Shutdown` is dropped, so Quit never waits behind queued work (a
/// dropped ctl request gets no reply; its caller times out).
fn drain() {
    loop {
        let cmd = INBOX.with(|i| i.borrow().as_ref().and_then(|rx| rx.try_recv().ok()));
        let Some(cmd) = cmd else { break };
        if safety::quit_requested() && !matches!(cmd, WmCommand::Shutdown(_)) {
            tracing::debug!("quitting: a queued WM command is dropped");
            continue;
        }
        guarded_state("a WM command", |s| s.handle(cmd));
    }
}

/// Periodic housekeeping (none once Quit is requested).
fn tick() {
    if safety::quit_requested() {
        return;
    }
    guarded_state("the WM tick", WmState::tick);
}

/// The AX observer callback. `refcon` carries the app's pid.
unsafe extern "C-unwind" fn ax_callback(
    _observer: NonNull<AXObserver>,
    element: NonNull<AXUIElement>,
    notification: NonNull<CFString>,
    refcon: *mut c_void,
) {
    let pid = observe::pid_of_refcon(refcon);
    // SAFETY: AX passes a live element and notification name for the callback's duration.
    let element = unsafe { AxElement::from_borrowed(element) };
    // SAFETY: as above.
    let name = unsafe { notification.as_ref() }.to_string();
    guarded_state("an AX callback", |s| {
        s.observer.on_notification(pid, element, &name);
        if !safety::quit_requested() {
            s.pump();
        }
    });
}

impl WmState {
    /// Hands the observer's events to the controller.
    fn pump(&mut self) {
        loop {
            let events = self.observer.take_events();
            if events.is_empty() {
                break;
            }
            self.controller.on_events(&mut self.observer, events);
        }
    }

    fn handle(&mut self, cmd: WmCommand) {
        match cmd {
            WmCommand::Apps(list) => self.observer.set_apps(list, ax_callback),
            WmCommand::App(event) => self.observer.on_app_event(event, ax_callback),
            WmCommand::Accessibility(trusted) => {
                if trusted && !self.scanned {
                    self.scanned = true;
                    let r = safety::recovery_scan("launch");
                    if r.restored > 0 {
                        tracing::warn!(restored = r.restored, "windows left parked by an earlier run were brought back");
                    }
                }
                self.observer.set_accessibility(trusted, ax_callback);
            }
            WmCommand::NativeSettings(settings) => {
                self.controller.set_settings(&settings);
                self.observer.set_settings(settings);
            }
            WmCommand::Displays(displays) => self.controller.set_displays(&mut self.observer, displays),
            // Without Accessibility no window can move: a stage change now would leave the
            // model out of step with the screen (its parked windows unregistered), so the
            // user's input waits for observation (R1-platform 9).
            WmCommand::Input(..) | WmCommand::Hotkey(_) | WmCommand::Strip(_)
                if self.controller.managing() && !self.observer.observing() =>
            {
                tracing::debug!("Accessibility is off: input ignored");
            }
            WmCommand::Input(event, drop) => self.controller.input(&mut self.observer, event, drop),
            WmCommand::Hotkey(command) => self.controller.hotkey(&mut self.observer, command),
            WmCommand::Strip(event) => self.controller.strip_event(&mut self.observer, event),
            WmCommand::SpaceChanged => self.controller.on_space_changed(&mut self.observer),
            WmCommand::ToggleStrip => self.controller.toggle_strip(),
            WmCommand::ReloadConfig => self.controller.reload_config(&mut self.observer),
            WmCommand::Ctl(cmd, reply) => {
                let value = self.controller.ctl(&mut self.observer, cmd);
                let _ = reply.send(value);
            }
            WmCommand::Shutdown(reply) => {
                let failed = self.controller.unpark_all(&self.observer);
                // A window still in a park zone on any Space (one AX could not reach there)
                // leaves the exit unclean, so the guardian watches until it is back.
                let stranded = safety::stranded_allowed();
                if !stranded.is_empty() {
                    tracing::warn!(windows = ?stranded.iter().map(|w| w.id).collect::<Vec<_>>(), "windows still parked after unparking (another Space?)");
                }
                let ok = failed == 0 && safety::registry_len() == 0 && stranded.is_empty();
                self.controller.save_now();
                self.observer.stop();
                let _ = self.observer.take_events();
                if let Some(rl) = CFRunLoop::current() {
                    rl.stop();
                }
                let _ = reply.send(ok);
            }
        }
        if !safety::quit_requested() {
            self.pump();
        }
    }

    fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(RETRY_EVERY) {
            self.observer.retry_pending(ax_callback);
        }
        self.pump();
        self.controller.tick(&mut self.observer);
    }

    fn after_panic(&mut self) {
        crate::logbook::counters::bump(crate::logbook::counters::Count::Recovery);
        tracing::error!("recovering from a panic on the WM thread: resyncing the parked-window registry");
        let _ = self.observer.take_events();
        self.controller.resync_registry(&self.observer);
    }
}

fn run(rx: Receiver<WmCommand>, run_loop: &CFRunLoop) {
    match AxElement::system_wide().set_messaging_timeout(AX_TIMEOUT_SECONDS) {
        Ok(()) => tracing::debug!("AX messaging timeout set to {AX_TIMEOUT_SECONDS} s"),
        Err(e) => tracing::warn!("could not set the AX messaging timeout: {e}"),
    }
    let only = observe::only_from_env();
    if let Some(only) = &only {
        let mut list: Vec<_> = only.iter().cloned().collect();
        list.sort();
        tracing::info!("management restricted to {}", list.join(", "));
    }
    let state = WmState {
        observer: Observer::new(std::process::id() as i32, only),
        controller: Controller::new(),
        scanned: false,
        ticks: 0,
    };
    STATE.with(|s| *s.borrow_mut() = Some(state));
    INBOX.with(|i| *i.borrow_mut() = Some(rx));

    // The timer keeps the run loop alive (CFRunLoopRun returns at once with no sources)
    // and drives housekeeping.
    let tick_block = RcBlock::new(|_timer: *mut CFRunLoopTimer| tick());
    let first_fire = CFAbsoluteTimeGetCurrent() + TICK_SECONDS;
    // SAFETY: valid arguments; the block is copied by CoreFoundation.
    let timer = unsafe { CFRunLoopTimer::with_handler(None, first_fire, TICK_SECONDS, 0, 0, Some(&tick_block)) };
    match &timer {
        // SAFETY: the default-mode constant is a valid static.
        Some(t) => run_loop.add_timer(Some(t), unsafe { kCFRunLoopDefaultMode }),
        None => tracing::error!("could not create the WM tick timer"),
    }

    tracing::debug!("WM thread running");
    CFRunLoop::run();
    tracing::debug!("WM thread stopped");
}
