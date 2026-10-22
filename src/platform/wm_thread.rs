//! The WM thread: a dedicated thread with its own `CFRunLoop` that owns every
//! Accessibility call, the [`Observer`] and the [`Controller`] (the engine).
//!
//! Other threads talk to it through [`WmHandle`]: each command goes into an mpsc channel,
//! then `CFRunLoopPerformBlock` + `CFRunLoopWakeUp` schedule a drain on the WM run loop.
//! The send happens before the block is scheduled, so a wake-up is never lost; a drain that
//! finds the channel already empty is harmless.
//!
//! While Apple's Stage Manager is on the thread stands down ([`StandDown`],
//! [`release_plan`]): every parked window comes back through the controller's unpark (the
//! quit path), windows it cannot reach are held in the safety registry until they are back,
//! the stale `state.json` goes, the strip hides on every Desktop, observation stops and a
//! fresh controller (given the last displays and settings) waits; when Apple's turns off
//! ([`resume_plan`]), held windows are tried again and observation starts: the fresh
//! controller builds stages from the windows on screen.
//!
//! At a logout, restart or shutdown the thread stands down the same way ahead of macOS's quit,
//! at the first of the logout's signals ([`Logout`], [`logout_plan`]; each signal logged with
//! the ms since the first): every window back, the stages saved (kept for the next login),
//! observation stopped, input ignored, so nothing is arranged while apps quit (the freeze); the
//! quit that follows only exits. A cancel signal, or no quit within
//! [`crate::engine::stand_down::LOGOUT_WAIT`] of the latest signal, starts observation again:
//! the fresh controller restores the saved stages, as at a launch.
//!
//! Every entry point (command drain, AX callback, timer) runs inside
//! [`safety::guarded`]: a panic is caught, logged and followed by a registry resync, instead
//! of killing the thread that owns the windows.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2_application_services::{AXObserver, AXUIElement};
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer, CFString, kCFRunLoopDefaultMode,
};

use super::apps::{AppEvent, AppInfo};
use super::ax::AxElement;
use super::controller::Controller;
use super::handoff::TakenWatch;
use super::ctl::Command;
use super::input::InputEvent;
use super::native_settings::{self, RawSettings};
use super::observe::{self, Observer};
use super::safety;
use crate::config::HotkeyCommand;
use crate::engine::model::Display;
use crate::engine::rescan::{Queued, Rescan, Why};
use crate::engine::stand_down::{Heard, Logout, Quit, Signal, StandDown, Step, Task, Work, logout_plan, release_plan, resume_plan};
use crate::ui::strip_model::{StripDrop, StripEvent};

/// Global AX messaging timeout, set on the system-wide element at thread start.
pub const AX_TIMEOUT_SECONDS: f32 = 0.25;

/// Housekeeping period of the WM run loop (saves, config polling, strip refresh).
const TICK_SECONDS: f64 = 0.25;

/// The fast tick: work that must land within a Desktop slide (~0.25–0.45 s), such as
/// checking an arrangement made ahead against the window server
/// (`Controller::fast_tick`). Armed only while the controller wants it
/// (`Controller::wants_fast_tick`); otherwise its timer waits far in the future.
const FAST_TICK_SECONDS: f64 = 0.02;

thread_local! {
    /// The fast tick's timer, armed and disarmed by [`arm_fast_tick`].
    static FAST_TIMER: RefCell<Option<CFRetained<CFRunLoopTimer>>> = const { RefCell::new(None) };
}

/// Arms the fast tick while the controller wants it, else parks its timer far in the future
/// (no wake-ups while nothing needs it).
fn arm_fast_tick() {
    let wanted = STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.controller.wants_fast_tick()));
    FAST_TIMER.with(|t| {
        let Some(timer) = t.borrow().as_ref().cloned() else { return };
        let now = CFAbsoluteTimeGetCurrent();
        let next = timer.next_fire_date();
        if wanted && next > now + FAST_TICK_SECONDS {
            timer.set_next_fire_date(now + FAST_TICK_SECONDS);
        } else if !wanted && next < now + 3600.0 {
            timer.set_next_fire_date(now + 1.0e9);
        }
    });
}

/// App-observation retries run every this many ticks (about once a second).
const RETRY_EVERY: u32 = 4;

/// After the main display changes, how long until every allowed app's windows are scanned
/// for the park zone: macOS puts windows back where they were the last time that display
/// arrangement was in use, parked ones included, a moment after the change (6X).
const DISPLAY_SWEEP_AFTER: Duration = Duration::from_secs(3);

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
    /// with whether every window came back. After the logout's head start: only stop and
    /// reply with what the head start found.
    Shutdown(SyncSender<bool>),
    /// A logout signal (its name, what it says, when the main thread got it): the first starts
    /// the head start ([`logout_plan`], no quit), a cancel resumes ([`Logout::signal`]).
    LogoutSignal(String, Signal, Instant),
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

/// How many windows were parked when a logout was announced ([`WmHandle::logout_signal`]): a
/// quit that comes while the head start still brings them back waits as long as their unpark
/// may take. Back to 0 when the logout is cancelled.
static LOGOUT_WINDOWS: AtomicUsize = AtomicUsize::new(0);

/// How many windows were parked when the logout now held was announced (0: none held).
pub fn logout_windows() -> usize {
    LOGOUT_WINDOWS.load(Ordering::SeqCst)
}

impl WmHandle {
    /// A logout signal `name` saying `signal`, got at `at`: when a logout is coming, notes
    /// how many windows are parked now ([`logout_windows`]); the WM thread decides the rest
    /// ([`WmCommand::LogoutSignal`]).
    pub fn logout_signal(&self, name: &str, signal: Signal, at: Instant) -> bool {
        if signal == Signal::Coming {
            LOGOUT_WINDOWS.fetch_max(safety::registry_len(), Ordering::SeqCst);
        }
        self.send(WmCommand::LogoutSignal(name.to_string(), signal, at))
    }

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
    /// Accessibility as last reported by the main thread.
    trusted: bool,
    /// Apple's Stage Manager is on: nothing is arranged.
    stand_down: StandDown,
    /// The last displays and `com.apple.WindowManager` values, for a fresh controller.
    displays: Option<Vec<Display>>,
    settings: Option<RawSettings>,
    /// Apps a recovery scan could not read, and a display change's sweep, scanned in slices
    /// ([`Rescan`]).
    rescan: Rescan,
    /// Apps a rescan found not answering, for one line at each end of their episode (8M).
    rescan_unanswered: crate::engine::busy_apps::Unanswered,
    /// A stand-down's leftovers check left windows in the park zone: they are looked for
    /// again whenever a Desktop shows, as the held ones are, with what that check knew of
    /// them ([`safety::Ours`], each window left judged ours): by then `state.json` is dropped
    /// and the controller fresh (the review of 6X2, 2). `None`: nothing left.
    stand_down_leftovers: Option<safety::Ours>,
    /// When the scan after a main display change is due.
    display_sweep: Option<Instant>,
    /// The logout's head start, while it holds.
    logout: Logout,
    /// The windows taken from an old guardian (round 8N), watched until each is back.
    taken: TakenWatch,
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

/// The fast tick (none once Quit is requested).
fn fast_tick() {
    if safety::quit_requested() {
        return;
    }
    guarded_state("the WM fast tick", |s| s.controller.fast_tick(&s.observer));
    arm_fast_tick();
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
    arm_fast_tick();
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
            WmCommand::Input(..) | WmCommand::Hotkey(_) | WmCommand::Strip(_) | WmCommand::ToggleStrip
                if !self.logout.admits(Work::Input) =>
            {
                tracing::debug!("a logout is coming: input ignored");
            }
            // Nothing is arranged during the hold, but a window the head start could not reach
            // may show on this Space now (as while standing down).
            WmCommand::SpaceChanged if !self.logout.admits(Work::SpaceChange) => self.retry_held(),
            WmCommand::LogoutSignal(name, signal, at) => self.logout_signal(&name, signal, at),
            WmCommand::Shutdown(reply) if matches!(self.logout.quit(), Quit::Done { .. }) => {
                // The head start did the work: nothing is unparked or saved again (the
                // controller is a fresh one with no stages; saving it would write none).
                let Quit::Done { all_back } = self.logout.quit() else { return };
                // Windows it could not bring back (another Desktop's) are tried once more,
                // knowing what its check knew, as the quit after a stand-down does.
                let back = if self.stand_down.stranded() {
                    let held_back = safety::retry_held();
                    let ours = with_kept(self.stand_down_leftovers.take(), self.controller.safety_ours());
                    let left_back = safety::leftovers_back_knowing("quit", ours).is_none();
                    tracing::info!(held_back, left_back, "quitting after the logout's head start: windows it left were tried again");
                    held_back && left_back
                } else {
                    tracing::info!(all_back, "quitting after the logout's head start: every window was brought back then");
                    all_back
                };
                self.observer.stop();
                let _ = self.observer.take_events();
                if let Some(rl) = CFRunLoop::current() {
                    rl.stop();
                }
                let _ = reply.send(back && safety::registry_len() == 0);
                return;
            }
            WmCommand::Apps(list) => self.observer.set_apps(list, ax_callback),
            WmCommand::App(event) => self.observer.on_app_event(event, ax_callback),
            WmCommand::Accessibility(trusted) => {
                if trusted && !self.scanned {
                    self.scanned = true;
                    let (r, unanswered) = safety::recovery_scan_with("launch", &safety::ScanScope::default());
                    if r.restored > 0 {
                        tracing::warn!(restored = r.restored, "windows left parked by an earlier run were brought back");
                    }
                    if !unanswered.is_empty() {
                        tracing::info!(apps = ?unanswered, "apps that did not answer the recovery scan are scanned again for a while");
                        // Their episodes start here: the rescans log only their ends (8M).
                        for &pid in &unanswered {
                            self.rescan_unanswered.missed(pid, Instant::now());
                        }
                        self.rescan.unanswered(unanswered, Why::Launch, Instant::now());
                    }
                }
                self.trusted = trusted;
                if (self.stand_down.admits(Work::Observe) && self.logout.admits(Work::Observe)) || !trusted {
                    self.observer.set_accessibility(trusted, ax_callback);
                } else if self.logout.holding() {
                    tracing::info!("a logout, restart or shutdown is coming: observation waits (it starts again if the logout is cancelled)");
                } else {
                    tracing::info!("Apple's Stage Manager is on: observation waits until it is off");
                }
            }
            WmCommand::NativeSettings(settings) => {
                let apple_on = native_settings::apple_stage_manager_enabled(&settings);
                self.settings = Some(settings.clone());
                self.controller.set_settings(&settings);
                self.observer.set_settings(settings);
                if self.logout.holding() {
                    // Read again when the hold ends (a cancelled logout).
                    tracing::debug!(apple_on, "a logout is coming: Apple's Stage Manager is looked at again if it is cancelled");
                } else {
                    match self.stand_down.apple(apple_on) {
                        Step::Release => self.release(),
                        Step::Resume => self.resume(),
                        Step::Nothing => {}
                    }
                }
            }
            WmCommand::Input(..) | WmCommand::Hotkey(_) | WmCommand::Strip(_) | WmCommand::ToggleStrip
                if !self.stand_down.admits(Work::Input) =>
            {
                tracing::debug!("Apple's Stage Manager is on: input ignored");
            }
            WmCommand::SpaceChanged if !self.stand_down.admits(Work::SpaceChange) => self.retry_held(),
            WmCommand::Displays(displays) => {
                let moved = self.displays.as_ref().and_then(|d| d.first()).map(|d| d.frame) != displays.first().map(|d| d.frame);
                if moved && self.displays.is_some() {
                    self.display_sweep = Some(Instant::now() + DISPLAY_SWEEP_AFTER);
                }
                self.displays = Some(displays.clone());
                self.controller.set_displays(&mut self.observer, displays);
            }
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
            WmCommand::SpaceChanged => {
                // A window a stand-down could not reach may show on this Space now.
                self.retry_held();
                self.controller.on_space_changed(&mut self.observer);
            }
            WmCommand::ToggleStrip => self.controller.toggle_strip(),
            WmCommand::ReloadConfig => self.controller.reload_config(&mut self.observer),
            WmCommand::Ctl(cmd, reply) => {
                let value = self.controller.ctl(&mut self.observer, cmd);
                let _ = reply.send(value);
            }
            WmCommand::Shutdown(reply) => {
                let failed = self.controller.unpark_all(&self.observer);
                // Every window of an allowed app still in a park zone on any Space comes back
                // now, known to this run or not (6X); one that cannot (another Space's, an app
                // that does not answer) leaves the exit unclean, so the guardian watches until
                // it is back.
                // A quit while stood down still knows whose windows the stand-down left
                // (its model and state.json are gone by then; the review of 6X3, 2).
                let ours = with_kept(self.stand_down_leftovers.take(), self.controller.safety_ours());
                let back = safety::leftovers_back_knowing("quit", ours).is_none();
                // A restore that failed but went through late is seen back now.
                let unrestored = self.controller.account_unrestored(&self.observer);
                if failed > 0 {
                    tracing::info!(failed, unrestored, "restores that failed at quit, after the leftovers check");
                }
                // Windows taken from an old guardian (round 8N): those shown come back now;
                // one still away leaves the exit unclean, so this app's guardian waits for it.
                let taken_left = self.taken.last_look(&self.controller.model_window_ids());
                let ok = back && unrestored == 0 && safety::registry_len() == 0 && taken_left == 0;
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

    /// Apple's Stage Manager came on: carries out [`release_plan`].
    fn release(&mut self) {
        tracing::warn!("Apple's Stage Manager is on: standing down (every window back, no strip, nothing arranged)");
        for task in release_plan(self.displays.is_some(), self.settings.is_some()) {
            match task {
                Task::UnparkAll => {
                    let failed = self.controller.unpark_all(&self.observer);
                    if failed > 0 || safety::registry_len() > 0 {
                        safety::emergency_unpark("stand-down");
                    }
                    // The window server's word, as at quit (the review of 6X): every allowed
                    // app's window still in a park zone comes back now, known to this run or
                    // not; one that cannot is looked for again whenever a Desktop shows.
                    // An earlier stand-down's or logout's leftovers not yet back stay known.
                    let ours = with_kept(self.stand_down_leftovers.take(), self.controller.safety_ours());
                    let left = safety::leftovers_back_knowing("stand-down", ours);
                    let back = left.is_none();
                    let unrestored = self.controller.account_unrestored(&self.observer);
                    if failed > 0 || !back {
                        tracing::info!(failed, unrestored, back, "restores that failed at the stand-down, after the leftovers check");
                    }
                    self.stand_down_leftovers = left;
                }
                Task::HoldStranded => self.hold_stranded("standing down"),
                Task::DropSavedStages => drop_saved_stages(),
                Task::StopObserving => {
                    self.observer.stop();
                    let _ = self.observer.take_events();
                }
                Task::NewController => self.controller = Controller::new(),
                Task::ReplayDisplays => {
                    if let Some(d) = self.displays.clone() {
                        self.controller.set_displays(&mut self.observer, d);
                    }
                }
                Task::ReplaySettings => {
                    if let Some(raw) = &self.settings {
                        self.controller.set_settings(raw);
                    }
                }
                Task::HideStrip => {
                    dispatch2::DispatchQueue::main().exec_async(|| crate::ui::strip::stand_down("Apple's Stage Manager is on"));
                }
                Task::RetryHeld | Task::StartObserving | Task::SaveStages => {}
            }
        }
    }

    /// A logout signal (round 8H): logged every time with the ms since the logout's first, then
    /// the first starts the head start, a cancel while held resumes at once, and repeats do
    /// nothing more. While standing down nothing is parked: the quit does as before.
    fn logout_signal(&mut self, name: &str, signal: Signal, at: Instant) {
        let stood_down = self.stand_down.stood_down();
        let (heard, since_first_ms) = self.logout.signal(signal, at, !stood_down);
        let queued_ms = at.elapsed().as_millis() as u64;
        tracing::info!(signal = name, cancel = signal == Signal::Cancelled, since_first_ms, queued_ms, then = heard.name(), "logout signal");
        match heard {
            Heard::HeadStart => self.logout_head_start(),
            Heard::Resume => self.logout_cancelled("a cancel signal"),
            Heard::Nothing if stood_down && signal == Signal::Coming => {
                tracing::info!("logout, restart or shutdown coming: standing down already, nothing parked");
            }
            Heard::Repeat | Heard::Nothing => {}
        }
    }

    /// The logout's head start: carries out [`logout_plan`] once (the review of 8F, M3), so
    /// windows are at their real frames as early as possible before apps save theirs, and the
    /// app stays running for macOS's own quit. From here until the quit or a resume nothing is
    /// arranged (the freeze): the controller that saved the stages is set aside and
    /// observation stops, so an activation, a focus change or an app's quit switches no stage,
    /// moves no card and is saved by no one.
    fn logout_head_start(&mut self) {
        tracing::info!("logout, restart or shutdown coming: every window back and the stages saved now; the app quits when macOS asks");
        let mut all_back = true;
        for task in logout_plan(self.displays.is_some(), self.settings.is_some()) {
            match task {
                Task::UnparkAll => {
                    // As at quit: the controller's unpark, then the window server's word on
                    // every allowed app's window still in a park zone (6X).
                    let failed = self.controller.unpark_all(&self.observer);
                    let ours = with_kept(self.stand_down_leftovers.take(), self.controller.safety_ours());
                    let left = safety::leftovers_back_knowing("logout", ours);
                    let back = left.is_none();
                    let unrestored = self.controller.account_unrestored(&self.observer);
                    all_back = back && unrestored == 0 && safety::registry_len() == 0;
                    if !all_back {
                        tracing::warn!(failed, unrestored, back, "not every window came back at the logout's head start; held and tried again at each Space change and at the quit");
                    }
                    // Kept past the controller (as at a stand-down): what the check knew of them.
                    self.stand_down_leftovers = left;
                }
                // The registry's windows still parked (failed restores among them) outlive
                // the controller that parked them: a cancelled logout's fresh one keeps them.
                Task::HoldStranded => self.hold_stranded("a logout is coming"),
                Task::SaveStages => self.controller.save_now(),
                Task::StopObserving => {
                    self.observer.stop();
                    let _ = self.observer.take_events();
                }
                Task::NewController => self.controller = Controller::new(),
                Task::ReplayDisplays => {
                    if let Some(d) = self.displays.clone() {
                        self.controller.set_displays(&mut self.observer, d);
                    }
                }
                Task::ReplaySettings => {
                    if let Some(raw) = &self.settings {
                        self.controller.set_settings(raw);
                    }
                }
                Task::HideStrip => dispatch2::DispatchQueue::main().exec_async(|| {
                    crate::ui::strip::stand_down("a logout, restart or shutdown is coming");
                    if let Some(mtm) = objc2_foundation::MainThreadMarker::new() {
                        crate::ui::corner_patch::apply(mtm, crate::ui::corner_patch::PatchPlan::default());
                    }
                }),
                Task::DropSavedStages | Task::RetryHeld | Task::StartObserving => {}
            }
        }
        self.logout.released(all_back);
    }

    /// The logout was cancelled after the head start (`why`: a cancel signal, or no quit came
    /// in time): the app starts afresh as at a launch (the fresh controller restores the saved
    /// stages and parks their windows again), unless Apple's Stage Manager came on meanwhile
    /// (then it stands down).
    fn logout_cancelled(&mut self, why: &str) {
        tracing::info!(why, "the logout was cancelled after its head start: starting afresh from the saved stages");
        // A later quit waits for the windows parked then, not for this logout's.
        LOGOUT_WINDOWS.store(0, Ordering::SeqCst);
        let apple_on = self.settings.as_ref().is_some_and(native_settings::apple_stage_manager_enabled);
        match self.stand_down.apple(apple_on) {
            Step::Release => self.release(),
            Step::Resume => self.resume(),
            Step::Nothing => {
                if self.trusted {
                    self.observer.set_accessibility(true, ax_callback);
                }
            }
        }
    }

    /// [`Task::HoldStranded`] (a stand-down's or a logout's, `why`): every window still in the
    /// registry is held apart from any controller's entries, and with the leftovers check's
    /// windows retried at every Space change until all are back.
    fn hold_stranded(&mut self, why: &str) {
        let held = safety::hold_remaining();
        let leftovers = self.stand_down_leftovers.is_some();
        if held > 0 || leftovers {
            tracing::warn!(held, leftovers, why, "windows of another Space come back when their Space shows");
        }
        self.stand_down.released(held == 0 && !leftovers);
    }

    /// Apple's Stage Manager went off: carries out [`resume_plan`] (a fresh start).
    fn resume(&mut self) {
        tracing::info!("Apple's Stage Manager is off: starting fresh from the windows on screen");
        for task in resume_plan(self.stand_down.stranded()) {
            match task {
                Task::RetryHeld => self.retry_held(),
                Task::StartObserving if self.trusted => self.observer.set_accessibility(true, ax_callback),
                _ => {}
            }
        }
    }

    /// Tries the held windows again, when there are any, and the stand-down's leftovers
    /// (windows still in the park zone that no registry held), knowing what the stand-down's
    /// check knew of them ([`retry_leftovers`]).
    fn retry_held(&mut self) {
        if self.stand_down.stranded() {
            let mut all_back = safety::retry_held();
            if let Some(kept) = self.stand_down_leftovers.take() {
                self.stand_down_leftovers = retry_leftovers(
                    kept,
                    || safety::Ours::now(Default::default()),
                    |ours| safety::leftovers_back_knowing("stand-down, a Desktop shows", ours),
                );
                all_back &= self.stand_down_leftovers.is_none();
            }
            self.stand_down.released(all_back);
        }
    }

    fn tick(&mut self) {
        if self.logout.cancelled(Instant::now()) {
            self.logout_cancelled("no quit came in time");
        }
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(RETRY_EVERY) {
            self.observer.retry_pending(ax_callback);
            // Windows closed while their app did not answer (8M): no destroyed notice came.
            self.observer.sweep_closed();
            // Windows taken from an old guardian (round 8N): its rules, about once a second.
            if self.trusted {
                self.taken.pass(&self.controller.model_window_ids());
            }
        }
        self.sweep_after_display_change();
        self.rescan_pass();
        self.pump();
        self.controller.tick(&mut self.observer);
    }

    /// One slice of the scans waiting ([`Rescan`]): apps a scan could not read, tried again
    /// for 30 s, and a display change's sweep, two apps a pass at most; windows we park are
    /// left alone (the registry's). A launch's app the observer reads by now needs no scan:
    /// the controller brings back a window it finds in the park zone.
    fn rescan_pass(&mut self) {
        if self.rescan.is_empty() {
            return;
        }
        let now = Instant::now();
        let observed: std::collections::HashSet<i32> = self.observer.windows().values().map(|w| w.pid).collect();
        let (pass, expired) = self.rescan.due(now, |p| observed.contains(&p));
        if !expired.is_empty() {
            let apps: Vec<i32> = expired.iter().map(|q| q.pid).collect();
            for q in &expired {
                self.rescan_unanswered.ended(q.pid, now);
            }
            tracing::warn!(?apps, "apps still do not answer: their windows are not scanned for the park zone (the quit's check and the next launch are)");
        }
        if pass.is_empty() {
            return;
        }
        let pids: Vec<i32> = pass.iter().map(|q| q.pid).collect();
        let reason = if pass.iter().all(|q| q.why == Why::Launch) { "launch, apps that did not answer" } else { "display change" };
        let scope = safety::ScanScope { pids: Some(pids.clone()), skip: safety::registry_ids(), quiet: true };
        let (r, unanswered) = safety::recovery_scan_with(reason, &scope);
        if r.restored > 0 {
            tracing::warn!(reason, restored = r.restored, "windows left in the park zone were brought back");
        }
        // Each app's episode of not answering, once at each end (8M: a line every ~3.5 s).
        let now = Instant::now();
        for &pid in &pids {
            if unanswered.contains(&pid) {
                if self.rescan_unanswered.missed(pid, now) {
                    tracing::info!(pid, reason, "an app did not answer the park-zone scan: scanned again for a while, nothing waits on it (logged again when it answers)");
                }
            } else if let Some(lasted) = self.rescan_unanswered.ended(pid, now) {
                tracing::info!(pid, lasted_s = lasted.as_secs(), "an app that did not answer the park-zone scan answers again");
            }
        }
        let missed: Vec<Queued> = pass.into_iter().filter(|q| unanswered.contains(&q.pid)).collect();
        self.rescan.done(&missed, Instant::now());
    }

    /// After a main display change ([`DISPLAY_SWEEP_AFTER`]): every allowed app is queued
    /// for a scan ([`Rescan::sweep`], in slices): each window macOS left in the park zone comes
    /// back, except those we park (the registry's).
    fn sweep_after_display_change(&mut self) {
        let Some(due) = self.display_sweep else { return };
        if Instant::now() < due {
            return;
        }
        self.display_sweep = None;
        if !self.observer.observing() || !self.stand_down.admits(Work::Observe) {
            return;
        }
        let pids = safety::scan_pids();
        tracing::info!(apps = pids.len(), "display change: every app is scanned for windows in the park zone, a few at a time");
        self.rescan.sweep(pids);
    }

    fn after_panic(&mut self) {
        crate::logbook::counters::bump(crate::logbook::counters::Count::Recovery);
        tracing::error!("recovering from a panic on the WM thread: resyncing the parked-window registry");
        let _ = self.observer.take_events();
        self.controller.resync_registry(&self.observer);
    }
}

/// Removes `state.json`: its stages are stale once Apple's Stage Manager arranges windows.
/// One more try at a stand-down's leftovers: `check` (the leftovers check) with what the
/// earlier checks knew (`kept`: the model's windows, `state.json`, the windows left judged
/// ours) and what is known `now`; what to keep for the try after, `None` once every window
/// is back. A window we parked, left as a sliver by a busy app, stays ours on every retry,
/// though the stand-down dropped `state.json` and the controller (the review of 6X2, 2).
fn retry_leftovers(kept: safety::Ours, now: impl FnOnce() -> safety::Ours, check: impl FnOnce(safety::Ours) -> Option<safety::Ours>) -> Option<safety::Ours> {
    check(kept.merged(now()))
}

/// What a check knows: what an earlier check left on record (`kept`), else nothing more, with
/// what is known `now` ([`safety::Ours::merged`]).
fn with_kept(kept: Option<safety::Ours>, now: safety::Ours) -> safety::Ours {
    match kept {
        Some(kept) => kept.merged(now),
        None => now,
    }
}

fn drop_saved_stages() {
    let state = super::paths::state_file();
    match std::fs::remove_file(&state) {
        Ok(()) => tracing::info!("saved stages dropped (Apple's Stage Manager arranges the windows now)"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("cannot remove {}: {e}", state.display()),
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
        trusted: false,
        stand_down: StandDown::default(),
        displays: None,
        settings: None,
        rescan: Rescan::default(),
        rescan_unanswered: Default::default(),
        stand_down_leftovers: None,
        display_sweep: None,
        logout: Logout::default(),
        taken: TakenWatch::default(),
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

    let fast_block = RcBlock::new(|_timer: *mut CFRunLoopTimer| fast_tick());
    let first_fast = CFAbsoluteTimeGetCurrent() + 1.0e9;
    // SAFETY: valid arguments; the block is copied by CoreFoundation.
    let fast = unsafe { CFRunLoopTimer::with_handler(None, first_fast, FAST_TICK_SECONDS, 0, 0, Some(&fast_block)) };
    match &fast {
        Some(t) => {
            // SAFETY: a valid timer; a small tolerance lets the system coalesce its wake-ups.
            unsafe { t.set_tolerance(0.005) };
            // SAFETY: the default-mode constant is a valid static.
            run_loop.add_timer(Some(t), unsafe { kCFRunLoopDefaultMode });
            FAST_TIMER.with(|f| *f.borrow_mut() = Some(t.clone()));
        }
        None => tracing::error!("could not create the WM fast tick timer"),
    }

    tracing::debug!("WM thread running");
    CFRunLoop::run();
    tracing::debug!("WM thread stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::leftovers;

    /// The leftovers' time bounds count each Accessibility call at the timeout set here.
    #[test]
    fn the_leftovers_know_the_messaging_timeout() {
        assert_eq!(Duration::from_secs_f32(AX_TIMEOUT_SECONDS), leftovers::AX_CALL_MAX);
    }

    /// The review of 8F2, 1: the quit after the logout's head start checks with what the head
    /// start's check knew (its leftovers, kept past the controller), merged with what is known
    /// then, as the quit after a stand-down does; with nothing kept, only what is known then.
    #[test]
    fn the_quit_after_a_head_start_knows_its_leftovers() {
        use crate::engine::recovery::Known;
        use std::collections::BTreeSet;

        let (left, managed_then, held) = (31, 5, 40);
        let kept = safety::Ours::from_parts(BTreeSet::new(), [managed_then].into(), None).kept_for_retry(&[left]);
        // At the quit the controller is a fresh one (no model); the registry holds the held one.
        let now = safety::Ours::from_parts([held].into(), BTreeSet::new(), None);
        let ours = with_kept(Some(kept), now.clone());
        assert_eq!(ours.known(left, None), Known::Registered, "the window the head start could not move");
        assert_eq!(ours.known(held, None), Known::Registered, "the registry's held window");
        assert_eq!(ours.known(managed_then, None), Known::Managed, "the old controller's window");

        let fresh = with_kept(None, now);
        assert_eq!(fresh.known(left, None), Known::Unknown, "nothing kept: only what is known then");
        assert_eq!(fresh.known(held, None), Known::Registered);
    }

    /// The review of 6X2, 2: a managed window a busy app left as a sliver at the stand-down
    /// stays ours on every retry, though `state.json` and the model are gone by then, until
    /// a check finds it back.
    #[test]
    fn a_stand_downs_retries_remember_whose_windows_are_left() {
        use crate::engine::geometry::Rect;
        use crate::engine::park::Corner;
        use crate::engine::persist::{SavedRole, SavedSpace, SavedStage, SavedWindow, StateFile};
        use crate::engine::recovery::{Known, ScanDisplays, is_stranded};
        use std::collections::BTreeSet;

        let displays = ScanDisplays::new(Rect::new(0.0, 0.0, 1440.0, 900.0), Rect::new(0.0, 25.0, 1440.0, 875.0), vec![], [Corner::BottomRight]);
        let sliver = Rect::new(1439.0, 820.0, 600.0, 392.0);
        let (id, bundle) = (4242, Some("com.example.mail"));
        let saved = SavedWindow {
            bundle_id: "com.example.mail".into(),
            title: "Inbox".into(),
            frame: Rect::new(100.0, 100.0, 600.0, 392.0),
            z: 0,
            key: false,
            pinned: false,
            role: SavedRole::Standard,
            original: None,
            window: None,
            ..Default::default()
        };
        let state = StateFile::single(SavedSpace { id: 1, current: true, stages: vec![SavedStage { name: None, windows: vec![saved], ..Default::default() }], active_index: Some(0), ..Default::default() });

        // After the stand-down (state.json dropped, a fresh controller) nothing knows it: a
        // sliver of an unknown window is never brought back.
        let after = safety::Ours::default();
        assert_eq!(after.known(id, bundle), Known::Unknown);
        assert!(!is_stranded(sliver, &displays, after.known(id, bundle)));

        // The first check knew it (the model's window, its app in state.json) and left it.
        let first = safety::Ours::from_parts(BTreeSet::new(), [id].into(), Some(state.clone()));
        assert_eq!(first.known(id, bundle), Known::Managed);
        let mut kept = Some(first.kept_for_retry(&[id]));

        // Two retries while the app is busy, then it answers.
        let mut seen = Vec::new();
        for round in 0..3 {
            let k = kept.take().expect("the windows left are kept until each is back");
            kept = retry_leftovers(k, safety::Ours::default, |ours| {
                seen.push((ours.known(id, bundle), is_stranded(sliver, &displays, ours.known(id, bundle)), ours.state.clone()));
                (round < 2).then(|| ours.kept_for_retry(&[id]))
            });
        }
        assert!(kept.is_none(), "back on the third retry: nothing more to keep");
        assert_eq!(seen.len(), 3);
        for (known, stranded, st) in seen {
            assert_eq!(known, Known::Registered);
            assert!(stranded, "the sliver still counts as parked");
            assert_eq!(st.as_ref(), Some(&state), "the frames it goes back to");
        }
    }
}
