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
//! With `[windows] apple_stage_manager_on = "integrate"` the thread follows Apple's instead of standing down
//! ([`augment_plan`]): every window back and the stages set aside as at a release, then the
//! Apple mode's null-hide engine and its conductor ([`Conductor::Apples`]) in place of a fresh
//! controller, observation kept running; Apple's off, the launch's engine and a fresh
//! controller come back before observation starts afresh. A config reload while Apple's is on
//! swaps the two ([`WmState::follow_with_apple`]).
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
//!
//! The housekeeping tick is paced by what happens ([`crate::engine::tick_pace`], round
//! sm-tick): every 0.25 s for a few seconds after an event that may change windows ([`stirs`],
//! an AX notice but a title change) and while a duty waits on it ([`WmState::tick_waits`]),
//! else every 2 s (less while a duty caps it: [`Controller::tick_rest`]). The tick's duties
//! share one read of the window list ([`windows::wake`]).

use crate::platform::clock;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2_application_services::{AXObserver, AXUIElement};
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer, CFString, kCFRunLoopDefaultMode,
};

use super::apps::{AppEvent, AppInfo};
use super::ax::AxElement;
use super::conductor::Conductor;
use super::handoff::TakenWatch;
use super::ctl::Command;
use super::input::InputEvent;
use super::minimize_tap::{self, MinimizeAsk};
use super::mission_key_tap;
use super::launch_scan;
use super::native_settings::{self, RawSettings};
use super::observe::{self, Observer};
use super::observe_probe;
use super::{enhanced_ui, input, safety, window_engine, windows};
use crate::config::HotkeyCommand;
use crate::engine::model::Display;
use crate::engine::rescan::{Queued, Rescan, Why};
use crate::engine::leftovers::{self, UNPARK_SETTLE};
use crate::engine::slow_task::{self, SlowTasks};
use crate::engine::stand_down::{Heard, Logout, Quit, Signal, StandDown, Step, Task, WithApple, Work, augment_plan, logout_plan, release_plan, resume_plan, to_augment_plan, to_stand_down_plan};
use crate::engine::tick_pace::{self, Pace, TickPace};
use crate::ui::strip_model::{StripDrop, StripEvent};

/// Global AX messaging timeout, set on the system-wide element at thread start.
pub const AX_TIMEOUT_SECONDS: f32 = 0.25;

/// Housekeeping period of the WM run loop (saves, config polling, strip refresh) while
/// something happens ([`tick_pace::BRISK`]); at rest it is longer ([`tick_pace`]).
fn tick_seconds() -> f64 {
    tick_pace::BRISK.as_secs_f64()
}

/// The fast tick: work that must land within a Desktop slide (~0.25–0.45 s), such as
/// checking an arrangement made ahead against the window server
/// (`Controller::fast_tick`). Armed only while the controller wants it
/// (`Controller::wants_fast_tick`); otherwise its timer waits far in the future.
const FAST_TICK_SECONDS: f64 = 0.02;

thread_local! {
    /// The fast tick's timer, armed and disarmed by [`arm_fast_tick`].
    static FAST_TIMER: RefCell<Option<CFRetained<CFRunLoopTimer>>> = const { RefCell::new(None) };
    /// The housekeeping tick's timer, paced by [`pace_tick`].
    static TICK_TIMER: RefCell<Option<CFRetained<CFRunLoopTimer>>> = const { RefCell::new(None) };
}

/// The tick's pace as last set, for `ctl state` ([`tick_status`]).
#[derive(Debug, Clone, Copy, Default)]
struct TickStatus {
    brisk: u64,
    rest: u64,
    pace: Option<Pace>,
}

static TICK_STATUS: std::sync::Mutex<TickStatus> =
    std::sync::Mutex::new(TickStatus { brisk: 0, rest: 0, pace: None });

/// `ctl state`'s `tick`: how many ticks ran brisk and at rest since the launch, and the pace
/// set after the last one (`why` it is brisk, or the rest's period). A ctl command stirs the
/// tick itself, so read the counts across an untouched spell rather than the pace.
pub fn tick_status() -> serde_json::Value {
    let s = TICK_STATUS.lock().map(|s| *s).unwrap_or_default();
    let (pace, why, rest_ms) = match s.pace {
        Some(Pace::Brisk(why)) => ("brisk", why, None),
        Some(Pace::Rest(d)) => ("rest", None, Some(d.max(tick_pace::BRISK).as_millis() as u64)),
        None => ("none", None, None),
    };
    serde_json::json!({ "brisk_ticks": s.brisk, "rest_ticks": s.rest, "pace": pace, "why": why, "rest_ms": rest_ms })
}

/// Whether command `cmd` stirs the tick ([`TickPace::stir`]): everything that may change
/// windows or that a test waits on, but a plain key (no ⌘, ⌥ or ⌃: typing changes no window
/// the tick looks at, and a shortcut that does comes with its own notices).
fn stirs(cmd: &WmCommand) -> bool {
    match cmd {
        // ⌘ pressed or let go alone moves nothing; the Tab that opens ⌘Tab's switcher stirs.
        WmCommand::Input(e, _) if e.kind == input::InputKind::CommandKey => false,
        WmCommand::Input(e, _) => e.kind != input::InputKind::KeyDown || e.cmd || e.other_modifiers,
        // A notice brings its own look forward; the tick's pace follows what that look finds.
        WmCommand::ServerNotice(_) => false,
        _ => true,
    }
}

/// Sets the tick's next firing for its pace now ([`tick_pace::retime`]): after a tick
/// (`after_tick`), and after every event (the next tick comes sooner when it must). The
/// rest is asked after a tick only. The inert slivers' tap is told the pace each time: how
/// long it judges presses on the tick's list follows from it
/// ([`super::corner::sliver_tap::tick_paced`]; no lock, the tap tries its own at every press).
fn pace_tick(after_tick: bool) {
    let now = clock::now();
    let pace = STATE.with(|s| {
        let mut st = s.try_borrow_mut().ok()?;
        let st = st.as_mut()?;
        if after_tick {
            st.rest = st.conductor.tick_rest();
        }
        Some(st.pace.pace(now, st.tick_waits(now), st.rest))
    });
    let Some(pace) = pace else { return };
    super::corner::sliver_tap::tick_paced(pace, after_tick);
    let was = TICK_STATUS.lock().ok().and_then(|mut s| {
        if after_tick {
            match pace {
                Pace::Brisk(_) => s.brisk += 1,
                Pace::Rest(_) => s.rest += 1,
            }
        }
        s.pace.replace(pace)
    });
    TICK_TIMER.with(|t| {
        let Some(timer) = t.borrow().as_ref().cloned() else { return };
        if was.is_none_or(|w| w.tolerance() != pace.tolerance()) {
            // SAFETY: a valid timer; the tolerance only lets the system fire it a little late.
            unsafe { timer.set_tolerance(pace.tolerance().as_secs_f64()) };
        }
        let cf_now = CFAbsoluteTimeGetCurrent();
        if let Some(at) = tick_pace::retime(pace, cf_now, timer.next_fire_date(), after_tick) {
            timer.set_next_fire_date(at);
        }
    });
}

/// Arms the fast tick while the controller wants it, else parks its timer far in the future
/// (no wake-ups while nothing needs it).
fn arm_fast_tick() {
    let (wanted, soon) = STATE.with(|s| s.borrow().as_ref().map_or((false, None), |s| (s.conductor.wants_fast_tick(), s.conductor.fast_tick_soon())));
    // Sooner while the raise-order check reads (`Controller::fast_tick_soon`).
    let period = soon.map_or(FAST_TICK_SECONDS, |d| d.as_secs_f64().min(FAST_TICK_SECONDS));
    FAST_TIMER.with(|t| {
        let Some(timer) = t.borrow().as_ref().cloned() else { return };
        let now = CFAbsoluteTimeGetCurrent();
        let next = timer.next_fire_date();
        if wanted && next > now + period {
            timer.set_next_fire_date(now + period);
        } else if !wanted && next < now + 3600.0 {
            timer.set_next_fire_date(now + 1.0e9);
        }
    });
}

/// After the main display changes, how long until every allowed app's windows are scanned
/// for the park zone: macOS puts windows back where they were the last time that display
/// arrangement was in use, parked ones included, a moment after the change (6X).
const DISPLAY_SWEEP_AFTER: Duration = Duration::from_secs(3);

/// Work for the WM thread.
pub enum WmCommand {
    /// The full list of running regular apps (sent once at launch).
    Apps(Vec<AppInfo>),
    /// A workspace event, and when the main thread's NSWorkspace handler got it (an
    /// activation also carries its own: [`AppEvent::Activated`]).
    App(AppEvent, Instant),
    /// Accessibility trust changed: start or stop observation. The first grant starts the
    /// recovery scan: each app is observed once its windows are back ([`launch_scan`]).
    Accessibility(bool),
    /// Fresh `com.apple.WindowManager` values.
    NativeSettings(RawSettings),
    /// Every display (the main one first), top-left coordinates, and when the main thread
    /// heard the change (AppKit's screen-parameters notice, or the poll).
    Displays(Vec<Display>, Instant),
    /// Global input in another app (left mouse down/up, key down); for a mouse-up over the
    /// visible strip, the slot a window dropped there goes to.
    Input(InputEvent, Option<StripDrop>),
    /// A minimize the minimize tap caught before macOS's genie (the yellow button, ⌘M):
    /// the window goes to the strip, or is minimized as macOS would when Stage Manager cannot
    /// take it now ([`super::minimize_tap`]).
    Minimize(MinimizeAsk),
    /// One of Mission Control's keys, or a press while it may show, heard by its key tap
    /// ([`super::mission_key_tap`]).
    MissionInput(mission_key_tap::HeardAt),
    /// A hotkey was pressed.
    Hotkey(HotkeyCommand),
    /// Input from the strip.
    Strip(StripEvent),
    /// The user went to another Space (`activeSpaceDidChangeNotification`), and when the main
    /// thread's handler got the notice.
    SpaceChanged(Instant),
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
    /// An app that did not answer Accessibility answers now, or was given up
    /// ([`super::observe_probe`]).
    Probe(observe_probe::Note),
    /// The launch's recovery scan read an app after observation's start ([`launch_scan`]).
    LaunchScan(launch_scan::Done<launch_scan::Read>),
    /// The window server's notices of one kind heard in one turn of the main run loop
    /// ([`super::server_notices`], every engine of ours): a trigger for a look, never a fact.
    ServerNotice(crate::engine::server_notice::Burst),
}

impl WmCommand {
    /// The command's name, for the slow-task line ([`slow_task`]).
    fn name(&self) -> &'static str {
        match self {
            WmCommand::Apps(_) => "command Apps",
            WmCommand::App(..) => "command App",
            WmCommand::Accessibility(_) => "command Accessibility",
            WmCommand::NativeSettings(_) => "command NativeSettings",
            WmCommand::Displays(..) => "command Displays",
            WmCommand::Input(..) => "command Input",
            WmCommand::Minimize(_) => "command Minimize",
            WmCommand::MissionInput(_) => "command MissionInput",
            WmCommand::Hotkey(_) => "command Hotkey",
            WmCommand::Strip(_) => "command Strip",
            WmCommand::SpaceChanged(_) => "command SpaceChanged",
            WmCommand::ToggleStrip => "command ToggleStrip",
            WmCommand::ReloadConfig => "command ReloadConfig",
            WmCommand::Ctl(..) => "command Ctl",
            WmCommand::Shutdown(_) => "command Shutdown",
            WmCommand::LogoutSignal(..) => "command LogoutSignal",
            WmCommand::Probe(_) => "command Probe",
            WmCommand::LaunchScan(_) => "command LaunchScan",
            WmCommand::ServerNotice(_) => "command ServerNotice",
        }
    }
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

/// Set when the quit came while the logout's head start held (round logout-cancel-relaunch):
/// the guardian then waits for the logout's cancel.
static QUIT_AFTER_HEAD_START: AtomicBool = AtomicBool::new(false);

/// Whether the quit came while the logout's head start held.
pub fn quit_after_head_start() -> bool {
    QUIT_AFTER_HEAD_START.load(Ordering::SeqCst)
}

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
        let probe_tx = tx.clone();
        std::thread::Builder::new().name("wm".into()).spawn(move || {
            let Some(run_loop) = CFRunLoop::current() else {
                tracing::error!("WM thread has no run loop");
                return;
            };
            let _ = ready_tx.send(RemoteRunLoop(run_loop.clone()));
            // The probes of apps that do not answer yet post their notes to this thread.
            let own = WmHandle { tx: probe_tx, run_loop: Arc::new(RemoteRunLoop(run_loop.clone())) };
            run(rx, &run_loop, own);
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
    /// Who conducts the windows: the app's own controller, or the Apple mode's while Apple's
    /// Stage Manager is on and `[windows] apple_stage_manager_on = "integrate"`.
    conductor: Conductor,
    scanned: bool,
    /// The tick's clock: when an event last stirred it, the last full pass ([`tick_pace`]).
    pace: TickPace,
    /// The longest rest, as the controller answered after the last tick
    /// ([`Controller::tick_rest`]).
    rest: Duration,
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
    /// When the next look for those leftovers is due: a moment after a Desktop change
    /// ([`WmState::look_for_leftovers`]). `None`: no look waits.
    leftovers_look: Option<Instant>,
    /// When the scan after a main display change is due.
    display_sweep: Option<Instant>,
    /// The logout's head start, while it holds.
    logout: Logout,
    /// The windows taken from an old guardian (round 8N), watched until each is back.
    taken: TakenWatch,
    /// The launch's recovery scan while apps are still read ([`launch_scan`]).
    launch_scan: Option<launch_scan::Run>,
    /// This thread's own handle: the launch scan's late reads come back through it.
    own: WmHandle,
}

thread_local! {
    static STATE: RefCell<Option<WmState>> = const { RefCell::new(None) };
    static INBOX: RefCell<Option<Receiver<WmCommand>>> = const { RefCell::new(None) };
    /// Slow tasks' log lines, rate-limited ([`slow_task`]).
    static SLOW: RefCell<SlowTasks> = RefCell::new(SlowTasks::default());
    /// The slowest step ([`step`]) of the task running, for its slow-task line.
    static SLOWEST_STEP: Cell<Option<(&'static str, Duration)>> = const { Cell::new(None) };
}

/// Runs one named step of a task (the tick's parts), keeping the slowest for the task's
/// slow-task line ([`guarded_state`]).
fn step<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
    let start = clock::now();
    let out = f();
    let took = start.elapsed();
    SLOWEST_STEP.with(|s| {
        if s.get().is_none_or(|(_, t)| took > t) {
            s.set(Some((name, took)));
        }
    });
    out
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
/// resynced from the model. A run that held the thread [`slow_task::SLOW`] or longer is
/// logged as task `task` (`detail`: what it handled, or empty), rate-limited.
fn guarded_state(what: &str, task: &'static str, detail: &str, f: impl FnOnce(&mut WmState)) {
    let start = clock::now();
    SLOWEST_STEP.with(|s| s.set(None));
    if safety::guarded(what, || with_state(f)).is_none() {
        let _ = safety::guarded("panic recovery", || with_state(WmState::after_panic));
    }
    let took = start.elapsed();
    if took < slow_task::SLOW {
        return;
    }
    let Some(held) = SLOW.with(|s| s.borrow_mut().note(task, took, clock::now())) else { return };
    let ms = took.as_millis() as u64;
    let (slowest, slowest_ms) = SLOWEST_STEP.with(|s| s.take()).map_or(("", 0), |(n, t)| (n, t.as_millis() as u64));
    tracing::debug!(task, detail, ms, slowest, slowest_ms, held, "slow WM task");
}

/// The fast tick (none once Quit is requested).
fn fast_tick() {
    if safety::quit_requested() {
        return;
    }
    guarded_state("the WM fast tick", "fast tick", "", |s| s.conductor.fast_tick(&mut s.observer));
    arm_fast_tick();
    pace_tick(false);
}

/// Handles every queued command (scheduled by `WmHandle::send`). Once Quit is requested,
/// every command but `Shutdown` is dropped, so Quit never waits behind queued work (a
/// dropped ctl request gets no reply; its caller times out).
fn drain() {
    let mut stirred = false;
    loop {
        let cmd = INBOX.with(|i| i.borrow().as_ref().and_then(|rx| rx.try_recv().ok()));
        let Some(cmd) = cmd else { break };
        if safety::quit_requested() && !matches!(cmd, WmCommand::Shutdown(_)) {
            tracing::debug!("quitting: a queued WM command is dropped");
            continue;
        }
        stirred |= stirs(&cmd);
        guarded_state("a WM command", cmd.name(), "", |s| s.handle(cmd));
    }
    if stirred {
        with_state(|s| s.pace.stir(clock::now()));
    }
    arm_fast_tick();
    pace_tick(false);
}

/// Periodic housekeeping (none once Quit is requested).
fn tick() {
    if safety::quit_requested() {
        return;
    }
    {
        // One read of the window list for the tick's duties, until a write of ours.
        let _wake = windows::wake();
        guarded_state("the WM tick", "tick", "", WmState::tick);
    }
    arm_fast_tick();
    pace_tick(true);
}

/// The AX observer callback. `refcon` carries the app's pid.
unsafe extern "C-unwind" fn ax_callback(
    _observer: NonNull<AXObserver>,
    element: NonNull<AXUIElement>,
    notification: NonNull<CFString>,
    refcon: *mut c_void,
) {
    // When macOS's notice reached us: its events are judged by it, not by when the
    // controller gets to them (the reads below and a batch's earlier events take time).
    let received = clock::now();
    let pid = observe::pid_of_refcon(refcon);
    // SAFETY: AX passes a live element and notification name for the callback's duration.
    let element = unsafe { AxElement::from_borrowed(element) };
    // SAFETY: as above.
    let name = unsafe { notification.as_ref() }.to_string();
    guarded_state("an AX callback", "AX callback", &name, |s| {
        // A title change moves no window: it does not stir the tick (the strip's label it
        // may change is presented on the next tick, brought in by the strip's wait).
        if name != super::ax::notifications::TITLE_CHANGED {
            s.pace.stir(clock::now());
        }
        s.observer.receiving(Some(received));
        s.observer.on_notification(pid, element, &name);
        s.observer.receiving(None);
        if !safety::quit_requested() {
            s.pump();
        }
    });
    // A focus held for a Desktop slide is looked at on the fast tick.
    arm_fast_tick();
    pace_tick(false);
}

/// The Dock's Accessibility notices about ⌘Tab's switcher (round cmd-tab): the list made,
/// its selection changed, an element gone ([`Controller::switcher_notice`]). Registered on
/// the Dock's application element on the WM run loop.
pub(super) unsafe extern "C-unwind" fn switcher_ax_callback(
    _observer: NonNull<AXObserver>,
    _element: NonNull<AXUIElement>,
    notification: NonNull<CFString>,
    _refcon: *mut c_void,
) {
    // SAFETY: AX passes a live notification name for the callback's duration.
    let name = unsafe { notification.as_ref() }.to_string();
    guarded_state("the switcher's AX callback", "switcher AX callback", &name, |s| {
        if !safety::quit_requested() {
            s.conductor.switcher_notice(&s.observer, &name);
        }
    });
    arm_fast_tick();
    pace_tick(false);
}

impl WmState {
    /// Hands the observer's events to the controller; then the retry cadence of apps that do
    /// not answer yet follows the restore window (fast while it is open).
    fn pump(&mut self) {
        loop {
            let events = self.observer.take_stamped();
            if events.is_empty() {
                break;
            }
            self.conductor.on_events(&mut self.observer, events);
        }
        self.observer.set_fast_retry(self.conductor.restore_window_open());
    }

    fn handle(&mut self, cmd: WmCommand) {
        match cmd {
            WmCommand::Input(..) | WmCommand::MissionInput(_) | WmCommand::Hotkey(_) | WmCommand::Strip(_) | WmCommand::ToggleStrip
                if !self.logout.admits(Work::Input) =>
            {
                tracing::debug!("a logout is coming: input ignored");
            }
            // Nothing is arranged during the hold, but a window the head start could not reach
            // may show on this Space now (as while standing down).
            WmCommand::SpaceChanged(_) if !self.logout.admits(Work::SpaceChange) => self.retry_held(),
            // Nothing is looked at for a notice while nothing is arranged; the polls resume.
            WmCommand::ServerNotice(_) if !self.logout.admits(Work::SpaceChange) || !self.stand_down.admits(Work::SpaceChange) => {}
            WmCommand::ServerNotice(burst) => self.conductor.server_notice(&mut self.observer, burst),
            WmCommand::LogoutSignal(name, signal, at) => self.logout_signal(&name, signal, at),
            // The tap swallowed the person's minimize: never lost. While nothing is arranged
            // it is macOS's, genie and all.
            WmCommand::Minimize(ask) => match self.arranging() {
                true => self.conductor.minimize_direct(&mut self.observer, ask),
                false => ask.as_macos("Stage Manager is not arranging windows now"),
            },
            WmCommand::Shutdown(reply) if matches!(self.logout.quit(), Quit::Done { .. }) => {
                // The head start did the work: nothing is unparked or saved again (the
                // controller is a fresh one with no stages; saving it would write none).
                let Quit::Done { all_back } = self.logout.quit() else { return };
                QUIT_AFTER_HEAD_START.store(true, Ordering::SeqCst);
                // Windows it could not bring back (another Desktop's) are tried once more,
                // knowing what its check knew, as the quit after a stand-down does.
                let back = if self.stand_down.stranded() {
                    let held_back = safety::retry_held();
                    let ours = with_kept(self.stand_down_leftovers.take(), self.conductor.safety_ours());
                    let left_back = rest_engine().bring_back_rest("quit", ours, true).is_none();
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
            WmCommand::App(event, received) => {
                // An app quitting during a logout's hold: the logout is still under way.
                if matches!(event, AppEvent::Terminated(_)) {
                    self.logout.app_quit(clock::now());
                }
                let launched = match &event {
                    AppEvent::Launched(app) => Some(app.pid),
                    _ => None,
                };
                // An app about to launch: the Apple mode may hide it at once (open here).
                if let AppEvent::WillLaunch(app) = &event
                    && self.arranging()
                    && self.observer.admits(app)
                {
                    self.conductor.app_will_launch(&self.observer, app);
                }
                // An app activated: the Apple mode may hide a running app with no window at
                // once, before the window it reopens shows.
                if let AppEvent::Activated(app, fired) = &event
                    && self.arranging()
                    && self.observer.admits(app)
                {
                    self.conductor.app_activated(&self.observer, app, *fired);
                }
                self.observer.receiving(Some(received));
                self.observer.on_app_event(event, ax_callback);
                self.observer.receiving(None);
                // A just-opened app: the engine may hold its new windows until placed.
                if let Some(pid) = launched
                    && self.arranging()
                {
                    self.conductor.app_launched(&self.observer, pid);
                }
            }
            WmCommand::Probe(note) => self.observer.on_probe(note, ax_callback),
            WmCommand::LaunchScan(done) => self.launch_scanned(done),
            WmCommand::Accessibility(trusted) => {
                if trusted && !self.scanned {
                    self.scanned = true;
                    self.start_launch_scan();
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
                self.conductor.set_settings(&settings);
                self.observer.set_settings(settings);
                if self.logout.holding() {
                    // Read again when the hold ends (a cancelled logout).
                    tracing::debug!(apple_on, "a logout is coming: Apple's Stage Manager is looked at again if it is cancelled");
                } else {
                    let step = self.stand_down.reading(apple_on, self.conductor.with_apple());
                    self.carry_out(step);
                }
            }
            WmCommand::Input(..) | WmCommand::MissionInput(_) | WmCommand::Hotkey(_) | WmCommand::Strip(_) | WmCommand::ToggleStrip
                if !self.stand_down.admits(Work::Input) =>
            {
                tracing::debug!("Apple's Stage Manager is on: input ignored");
            }
            WmCommand::SpaceChanged(_) if !self.stand_down.admits(Work::SpaceChange) => self.retry_held(),
            WmCommand::Displays(displays, heard) => {
                let moved = self.displays.as_ref().and_then(|d| d.first()).map(|d| d.frame) != displays.first().map(|d| d.frame);
                if moved && self.displays.is_some() {
                    self.display_sweep = Some(clock::now() + DISPLAY_SWEEP_AFTER);
                }
                self.displays = Some(displays.clone());
                self.conductor.set_displays(&mut self.observer, displays, heard);
            }
            // Without Accessibility no window can move: a stage change now would leave the
            // model out of step with the screen (its parked windows unregistered), so the
            // user's input waits for observation (R1-platform 9).
            WmCommand::Input(..) | WmCommand::MissionInput(_) | WmCommand::Hotkey(_) | WmCommand::Strip(_)
                if self.conductor.managing() && !self.observer.observing() =>
            {
                tracing::debug!("Accessibility is off: input ignored");
            }
            WmCommand::Input(event, drop) => self.conductor.input(&mut self.observer, event, drop),
            WmCommand::MissionInput(heard) => self.conductor.mission_input(&mut self.observer, heard),
            WmCommand::Hotkey(command) => self.conductor.hotkey(&mut self.observer, command),
            WmCommand::Strip(event) => self.conductor.strip_event(&mut self.observer, event),
            WmCommand::SpaceChanged(heard) => {
                // A window a stand-down could not reach may show on this Space now.
                self.retry_held();
                self.conductor.on_space_changed(&mut self.observer, heard);
            }
            WmCommand::ToggleStrip => self.conductor.toggle_strip(),
            WmCommand::ReloadConfig => {
                self.conductor.reload_config(&mut self.observer);
                self.follow_with_apple();
            }
            WmCommand::Ctl(cmd, reply) => {
                let value = self.conductor.ctl(&mut self.observer, cmd);
                let _ = reply.send(value);
            }
            WmCommand::Shutdown(reply) => {
                // The unpark saves the stages before the leftovers check reads them (S1).
                let failed = self.conductor.unpark_all(&mut self.observer);
                // Every window of an allowed app still in a park zone on any Space comes back
                // now, known to this run or not (6X); one that cannot (another Space's, an app
                // that does not answer) leaves the exit unclean, so the guardian watches until
                // it is back.
                // A quit while stood down still knows whose windows the stand-down left
                // (its model and state.json are gone by then; the review of 6X3, 2).
                let ours = with_kept(self.stand_down_leftovers.take(), self.conductor.safety_ours());
                let back = rest_engine().bring_back_rest("quit", ours, true).is_none();
                // A restore that failed but went through late is seen back now.
                let unrestored = self.conductor.account_shows_owed(&self.observer);
                if failed > 0 {
                    tracing::info!(failed, unrestored, "restores that failed at quit, after the leftovers check");
                }
                // Windows taken from an old guardian (round 8N): those shown come back now;
                // one still away leaves the exit unclean, so this app's guardian waits for it.
                self.taken.last_look(&self.conductor.model_window_ids());
                // What is left, counted: children their apps ordered out while parked aside
                // (they stay on record and the guardian waits for them, but they do not make
                // the quit unclean), and the windows the leftovers check left that were
                // handed over (none this app parked) apart from this app's own.
                let end = leftovers::quit_end(leftovers::quit_left(
                    back,
                    safety::left_not_taken(),
                    &self.conductor.shows_owed_ids(),
                    &safety::registry_ids(),
                    &safety::taken_ids(),
                    &safety::ordered_out(),
                ));
                let ok = end.marks_clean();
                // Handed-over windows still away and nothing else: passed on, a warning.
                safety::set_passed_on(end == leftovers::QuitEnd::PassedOn);
                safety::set_only_ordered_out(end == leftovers::QuitEnd::Waits);
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

    /// The launch's recovery scan (S4; the 1.7 review, L1): every app with a window is read on
    /// a thread of its own, for [`launch_scan::SCAN_WAIT`] at most; those read by then have
    /// their windows a crashed copy left in the park zone brought back now, and are observed
    /// at observation's start. Each one read later is held from observation until then
    /// ([`Observer::hold_for_scan`]): one slow app holds up no other.
    fn start_launch_scan(&mut self) {
        let apps = safety::scan_targets();
        let pids: Vec<i32> = apps.iter().map(|(pid, _)| *pid).collect();
        self.launch_scan = Some(launch_scan::Run::new(pids.iter().copied()));
        self.observer.hold_for_scan(pids);
        let own = self.own.clone();
        let (done, not_started) = launch_scan::read_all(apps, launch_scan::read, launch_scan::SCAN_WAIT, move |d| {
            own.send(WmCommand::LaunchScan(d));
        });
        for d in done {
            self.launch_scanned(d);
        }
        // An app whose thread did not start is read here, as every app used to be.
        for (pid, bundle) in not_started {
            let t0 = clock::now();
            let value = Some(launch_scan::read(pid, bundle.as_deref()));
            self.launch_scanned(launch_scan::Done { pid, bundle, value, took: t0.elapsed() });
        }
        match &self.launch_scan {
            // No app to read.
            Some(run) if run.finished() => self.end_launch_scan(),
            Some(run) => tracing::info!(waiting = run.waiting(), "launch scan: apps not read yet are observed once they are"),
            None => {}
        }
    }

    /// The launch scan read one app: its windows in the park zone come back now; then it is
    /// observed (if observation wants it), or asked again on the schedule and scanned again
    /// for a while when it did not answer. The last app read ends the scan with its summary.
    fn launch_scanned(&mut self, done: launch_scan::Done<launch_scan::Read>) {
        let pid = done.pid;
        let Some(run) = self.launch_scan.as_mut() else {
            self.observer.scan_done(pid, false, ax_callback);
            return;
        };
        let answered = run.apply(done);
        let finished = run.finished();
        if !answered {
            let now = clock::now();
            // Its episode starts here: the rescans log only its end (8M).
            self.rescan_unanswered.missed(pid, now);
            self.rescan.unanswered([pid], Why::Launch, now);
        }
        self.observer.scan_done(pid, answered, ax_callback);
        if finished {
            self.end_launch_scan();
        }
    }

    /// Every app was read: the scan's summary.
    fn end_launch_scan(&mut self) {
        let Some(run) = self.launch_scan.take() else { return };
        let (r, unanswered) = run.finish();
        if r.restored > 0 {
            tracing::warn!(restored = r.restored, "windows left parked by an earlier run were brought back");
        }
        if !unanswered.is_empty() {
            tracing::info!(apps = ?unanswered, "apps that did not answer the recovery scan are scanned again for a while");
        }
    }

    /// An app whose launch-scan read takes [`launch_scan::HOLD_MAX`] is observed without it
    /// (its windows still come back when the read ends).
    fn release_overdue_scans(&mut self) {
        if self.launch_scan.is_none() {
            return;
        }
        for pid in self.observer.scan_overdue(clock::now(), launch_scan::HOLD_MAX) {
            tracing::warn!(pid, "launch scan: the app's read takes long; it is observed without waiting for it");
            self.observer.scan_done(pid, false, ax_callback);
        }
    }

    /// Apple's Stage Manager came on: carries out [`release_plan`].
    fn release(&mut self) {
        tracing::warn!("Apple's Stage Manager is on: standing down (every window back, no strip, nothing arranged)");
        for task in release_plan(self.displays.is_some(), self.settings.is_some()) {
            match task {
                Task::UnparkAll => self.release_windows(),
                Task::HoldStranded => self.hold_stranded("standing down"),
                Task::DropSavedStages => drop_saved_stages(),
                Task::StopObserving => {
                    self.observer.stop();
                    let _ = self.observer.take_events();
                }
                Task::NewController => self.replace_conductor(Conductor::ours()),
                Task::ReplayDisplays => {
                    if let Some(d) = self.displays.clone() {
                        self.conductor.set_displays(&mut self.observer, d, clock::now());
                    }
                }
                Task::ReplaySettings => {
                    if let Some(raw) = &self.settings {
                        self.conductor.set_settings(raw);
                    }
                }
                Task::HideStrip => self.replay(Task::HideStrip, "Apple's Stage Manager is on"),
                Task::RetryHeld | Task::StartObserving | Task::SaveStages | Task::InstallAppleEngine | Task::AppleConductor | Task::InstallLaunchEngine => {}
            }
        }
    }

    /// Carries out the step a reading of Apple's Stage Manager and `[windows] apple_stage_manager_on`
    /// called for ([`StandDown::reading`]).
    fn carry_out(&mut self, step: Step) {
        match step {
            Step::Release => self.release(),
            Step::Augment => self.augment(),
            Step::Resume { from } => self.resume(from),
            Step::ToAugment => self.swap(true),
            Step::ToStandDown => self.swap(false),
            Step::Nothing => {}
        }
    }

    /// `[windows] apple_stage_manager_on` as the config in force has it now, against what the app does
    /// while Apple's is on: a changed value swaps standing down and augmenting (after a
    /// reload, and at every tick: the conductor polls the file).
    fn follow_with_apple(&mut self) {
        if !self.stand_down.apple_on() || self.logout.holding() {
            return;
        }
        let step = self.stand_down.reading(true, self.conductor.with_apple());
        if step != Step::Nothing {
            self.carry_out(step);
        }
    }

    /// Apple's Stage Manager came on with `apple_stage_manager_on = "integrate"`: carries out
    /// [`augment_plan`]. Every window back as at a release; then the Apple mode follows
    /// Apple's, observation kept running.
    fn augment(&mut self) {
        tracing::warn!("Apple's Stage Manager is on: following it (every window back, no strip of ours; its stages read into the model)");
        for task in augment_plan(self.displays.is_some(), self.settings.is_some()) {
            match task {
                Task::UnparkAll => self.release_windows(),
                Task::HoldStranded => self.hold_stranded("augmenting Apple's"),
                Task::DropSavedStages => drop_saved_stages(),
                Task::InstallAppleEngine => window_engine::install(window_engine::of(window_engine::EngineKind::Apple)),
                Task::AppleConductor => self.replace_conductor(Conductor::apples()),
                Task::ReplayDisplays | Task::ReplaySettings | Task::HideStrip => self.replay(task, "Apple's Stage Manager is on"),
                _ => {}
            }
        }
    }

    /// While Apple's is on, `apple_stage_manager_on` changed: to augmenting (`to_augment`, from standing
    /// down) or back to standing down. Nothing is hidden by either, so nothing comes back.
    fn swap(&mut self, to_augment: bool) {
        let plan = match to_augment {
            true => to_augment_plan(self.displays.is_some(), self.settings.is_some()),
            false => to_stand_down_plan(self.displays.is_some(), self.settings.is_some()),
        };
        tracing::info!(apple_stage_manager_on = if to_augment { "integrate" } else { "pause" }, "config: [windows] apple_stage_manager_on changed while Apple's Stage Manager is on");
        for task in plan {
            match task {
                Task::InstallAppleEngine => window_engine::install(window_engine::of(window_engine::EngineKind::Apple)),
                Task::InstallLaunchEngine => install_launch_engine(),
                Task::AppleConductor => self.replace_conductor(Conductor::apples()),
                Task::NewController => self.replace_conductor(Conductor::ours()),
                Task::StopObserving => {
                    self.observer.stop();
                    let _ = self.observer.take_events();
                }
                Task::StartObserving if self.trusted => self.observer.set_accessibility(true, ax_callback),
                Task::ReplayDisplays | Task::ReplaySettings | Task::HideStrip => self.replay(task, "Apple's Stage Manager is on"),
                _ => {}
            }
        }
    }

    /// [`Task::ReplayDisplays`], [`Task::ReplaySettings`] and [`Task::HideStrip`] (`why`: the
    /// strip's reason) for whoever conducts now.
    fn replay(&mut self, task: Task, why: &'static str) {
        match task {
            Task::ReplayDisplays => {
                if let Some(d) = self.displays.clone() {
                    self.conductor.set_displays(&mut self.observer, d, clock::now());
                }
            }
            Task::ReplaySettings => {
                if let Some(raw) = &self.settings {
                    self.conductor.set_settings(raw);
                }
            }
            Task::HideStrip => {
                // The corner patch too (it covers a park corner, and nothing is parked now):
                // its wallpaper is read no more until a controller of ours plans it again.
                dispatch2::DispatchQueue::main().exec_async(move || {
                    crate::ui::strip::stand_down(why);
                    if let Some(mtm) = objc2_foundation::MainThreadMarker::new() {
                        crate::ui::corner_patch::stand_down(mtm);
                    }
                });
            }
            _ => {}
        }
    }

    /// Who conducts the windows from now on (a release, an augment, a swap, a logout's hold,
    /// a resume: every window of the one before is back by then). The inert slivers' tap is
    /// switched off with the old one's shield: the next controller of ours publishes its own
    /// once it parks windows, and none in the Apple mode or standing down.
    fn replace_conductor(&mut self, next: Conductor) {
        super::corner::sliver_tap::off();
        self.conductor = next;
    }

    /// [`Task::UnparkAll`] of a release: the controller's unpark, the registry's last resort,
    /// then the window server's word (the review of 6X), as at quit.
    fn release_windows(&mut self) {
        let failed = self.conductor.unpark_all(&mut self.observer);
        if failed > 0 || safety::registry_len() > 0 {
            rest_engine().bring_all_back("stand-down");
        }
        let ours = with_kept(self.stand_down_leftovers.take(), self.conductor.safety_ours());
        let left = rest_engine().bring_back_rest("stand-down", ours, true);
        let back = left.is_none();
        let unrestored = self.conductor.account_shows_owed(&self.observer);
        if failed > 0 || !back {
            tracing::info!(failed, unrestored, back, "restores that failed at the stand-down, after the leftovers check");
        }
        self.stand_down_leftovers = left;
    }

    /// A logout signal (round 8H): logged every time with the ms since the logout's first, then
    /// the first starts the head start, a cancel while held resumes at once, and repeats do
    /// nothing more. While standing down nothing is parked: the quit does as before.
    fn logout_signal(&mut self, name: &str, signal: Signal, at: Instant) {
        // Nothing of ours is parked while Apple's is on (standing down or augmenting): the
        // quit does as before.
        let stood_down = self.stand_down.apple_on();
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
                    let failed = self.conductor.unpark_all(&mut self.observer);
                    let ours = with_kept(self.stand_down_leftovers.take(), self.conductor.safety_ours());
                    let left = rest_engine().bring_back_rest("logout", ours, true);
                    let back = left.is_none();
                    let unrestored = self.conductor.account_shows_owed(&self.observer);
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
                Task::SaveStages => self.conductor.save_now(),
                Task::StopObserving => {
                    self.observer.stop();
                    let _ = self.observer.take_events();
                }
                Task::NewController => self.replace_conductor(Conductor::ours()),
                Task::ReplayDisplays => {
                    if let Some(d) = self.displays.clone() {
                        self.conductor.set_displays(&mut self.observer, d, clock::now());
                    }
                }
                Task::ReplaySettings => {
                    if let Some(raw) = &self.settings {
                        self.conductor.set_settings(raw);
                    }
                }
                Task::HideStrip => dispatch2::DispatchQueue::main().exec_async(|| {
                    crate::ui::strip::stand_down("a logout, restart or shutdown is coming");
                    if let Some(mtm) = objc2_foundation::MainThreadMarker::new() {
                        crate::ui::corner_patch::stand_down(mtm);
                    }
                }),
                Task::DropSavedStages | Task::RetryHeld | Task::StartObserving | Task::InstallAppleEngine | Task::AppleConductor | Task::InstallLaunchEngine => {}
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
        match self.stand_down.reading(apple_on, self.conductor.with_apple()) {
            Step::Nothing => {
                if self.trusted {
                    self.observer.set_accessibility(true, ax_callback);
                }
            }
            step => self.carry_out(step),
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

    /// Apple's Stage Manager went off: carries out [`resume_plan`] (the stages set aside come back
    /// as the fresh controller loads `state.json`).
    fn resume(&mut self, from: WithApple) {
        tracing::info!(from = from.name(), "Apple's Stage Manager is off: managing again, the stages set aside taken back (else fresh from the windows on screen)");
        for task in resume_plan(self.stand_down.stranded(), from, self.displays.is_some(), self.settings.is_some()) {
            match task {
                Task::StopObserving => {
                    // The Apple mode's stages, saved once more before its conductor goes.
                    self.conductor.save_now();
                    self.observer.stop();
                    let _ = self.observer.take_events();
                }
                Task::NewController => self.replace_conductor(Conductor::ours()),
                Task::ReplayDisplays | Task::ReplaySettings => self.replay(task, "Apple's Stage Manager is off"),
                Task::RetryHeld => {
                    self.retry_held();
                    // Before observation starts: a window back now is where the fresh start looks.
                    self.look_for_leftovers(true);
                }
                Task::InstallLaunchEngine => install_launch_engine(),
                Task::StartObserving if self.trusted => self.observer.set_accessibility(true, ax_callback),
                _ => {}
            }
        }
    }

    /// Tries the held windows again, when there are any (a Desktop shows); the stand-down's
    /// leftovers (windows still in the park zone that no registry held) are looked for once
    /// the window list has settled, on a tick ([`Self::look_for_leftovers`]): the wait used
    /// to be slept out here, a second or two of the window thread at every Desktop change
    /// for as long as one window stayed away.
    fn retry_held(&mut self) {
        if self.stand_down.stranded() {
            let all_back = safety::retry_held();
            if self.stand_down_leftovers.is_some() {
                self.leftovers_look = Some(clock::now() + UNPARK_SETTLE);
            }
            self.stand_down.released(all_back && self.stand_down_leftovers.is_none());
        }
    }

    /// Looks for the stand-down's leftovers again, knowing what the stand-down's check knew
    /// of them ([`retry_leftovers`]): on the tick once a Desktop change's look is due
    /// ([`look_due`]: the list has settled by then, so it is read as it stands), or
    /// `at_once` (a resume: nothing is arranged yet, and the list gets its settle as after
    /// an unpark). The app may arrange windows again by now (a resume, a cancelled logout):
    /// the windows its controller parks stay where they are ([`leftovers_now`]).
    fn look_for_leftovers(&mut self, at_once: bool) {
        if !at_once && !look_due(self.leftovers_look, clock::now()) {
            return;
        }
        self.leftovers_look = None;
        let Some(kept) = self.stand_down_leftovers.take() else { return };
        self.stand_down_leftovers = retry_leftovers(
            kept,
            || leftovers_now(safety::Ours::now(Default::default()), safety::parked_on_purpose()),
            |ours| rest_engine().bring_back_rest("stand-down, a Desktop shows", ours, at_once),
        );
        self.stand_down.released(safety::held_len() == 0 && self.stand_down_leftovers.is_none());
    }

    fn tick(&mut self) {
        if self.logout.cancelled(clock::now()) {
            self.logout_cancelled("no quit came in time");
        }
        // About once a second by the clock: every fourth tick while brisk, every tick at rest.
        let full = self.pace.full_pass(clock::now());
        // Apps gone without a termination notice (round missed-quit): the pids whose calls
        // failed each tick, the apps with tracked windows about once a second.
        step("reap gone apps", || self.observer.reap_gone(full));
        if full {
            step("release overdue scans", || self.release_overdue_scans());
            step("retry pending apps", || self.observer.retry_pending(ax_callback));
            // Windows closed while their app did not answer (8M): no destroyed notice came.
            if sweep_now(Sweep::BeforeConductor, self.conductor.is_apples(), false) {
                step("sweep closed windows", || self.observer.sweep_closed());
            }
            // Windows taken from an old guardian (round 8N): its rules, about once a second.
            if self.trusted {
                step("taken windows", || self.taken.pass(&self.conductor.model_window_ids()));
            }
        }
        step("leftovers look", || self.look_for_leftovers(false));
        step("display change sweep", || self.sweep_after_display_change());
        step("rescan", || self.rescan_pass());
        step("events", || self.pump());
        step("controller tick", || self.conductor.tick(&mut self.observer));
        if full && sweep_now(Sweep::AfterConductor, self.conductor.is_apples(), windows::wake_has_list()) {
            // The Apple mode's: on the list its own read took this tick, never one of its own.
            step("sweep closed windows", || self.observer.sweep_closed());
        }
        step("apple_stage_manager_on", || self.follow_with_apple());
        // The minimize tap passes everything while nothing is arranged.
        minimize_tap::admit(self.arranging() && !self.conductor.is_apples());
        step("minimize tap", minimize_tap::refresh);
        mission_key_tap::admit(self.arranging() && !self.conductor.is_apples());
        step("mission key tap", mission_key_tap::refresh);
        // An app's enhanced UI a restore could not put back, or held off too long: on again.
        step("enhanced UI sweep", enhanced_ui::sweep);
    }

    /// Why the tick stays brisk ([`tick_pace`]): the first duty of the thread's own, the
    /// observer's or the controller's that waits on it ([`Observer::tick_waits`],
    /// [`Controller::tick_waits`]); `None` when the tick may rest. The thread's own: the
    /// launch's recovery scan, the rescans queued, a display change's sweep or a leftovers
    /// look due, a logout's hold, windows taken from an old guardian, an event tap macOS keeps
    /// off for secure input (switched on again by the tick), an app's enhanced UI held off.
    fn tick_waits(&self, now: Instant) -> Option<&'static str> {
        if self.launch_scan.is_some() {
            Some("the launch scan")
        } else if self.rescan.wants_brisk() || self.display_sweep.is_some() {
            Some("a rescan")
        } else if self.leftovers_look.is_some() {
            Some("a leftovers look")
        } else if self.logout.holding() {
            Some("a logout's hold")
        } else if safety::taken_len() > 0 {
            Some("windows taken from an old guardian")
        } else if minimize_tap::off_for_secure_input() || super::corner::sliver_tap::off_for_secure_input() || mission_key_tap::off_for_secure_input() {
            Some("an event tap off for secure input")
        } else if enhanced_ui::held_off() {
            Some("an app's enhanced UI held off")
        } else {
            self.observer.tick_waits().or_else(|| self.conductor.tick_waits(now))
        }
    }

    /// Whether windows are arranged now: not standing down for Apple's Stage Manager, no
    /// logout coming, and Accessibility on when managing (the gates the user's input meets
    /// in [`Self::handle`]).
    fn arranging(&self) -> bool {
        self.logout.admits(Work::Input)
            && self.stand_down.admits(Work::Input)
            && (!self.conductor.managing() || self.observer.observing())
    }

    /// One slice of the scans waiting ([`Rescan`]): apps a scan could not read, tried again
    /// for 30 s, and a display change's sweep, two apps a pass at most; windows we park are
    /// left alone (the registry's). A launch's app the observer reads by now needs no scan:
    /// the controller brings back a window it finds in the park zone.
    fn rescan_pass(&mut self) {
        if self.rescan.is_empty() {
            return;
        }
        let now = clock::now();
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
        let scope = safety::ScanScope {
            pids: Some(pids.clone()),
            skip: safety::registry_ids(),
            quiet: true,
            registered: safety::crashed_registered(),
        };
        let (r, unanswered) = window_engine::current().recover(reason, &scope);
        if r.restored > 0 {
            tracing::warn!(reason, restored = r.restored, "windows left in the park zone were brought back");
        }
        // Each app's episode of not answering, once at each end (8M: a line every ~3.5 s).
        let now = clock::now();
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
        self.rescan.done(&missed, clock::now());
    }

    /// After a main display change ([`DISPLAY_SWEEP_AFTER`]): every allowed app is queued
    /// for a scan ([`Rescan::sweep`], in slices): each window macOS left in the park zone comes
    /// back, except those we park (the registry's).
    fn sweep_after_display_change(&mut self) {
        let Some(due) = self.display_sweep else { return };
        if clock::now() < due {
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
        self.conductor.resync_registry(&self.observer);
    }
}

/// Where in the tick the closed windows' sweep may run ([`sweep_now`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sweep {
    /// Its old place, before the events and the controller's tick (whose duties then share
    /// the list it read).
    BeforeConductor,
    /// After the conductor's tick.
    AfterConductor,
}

/// Whether the closed windows' sweep ([`Observer::sweep_closed`]) runs at `at` in a full
/// pass. Our own engines: before the conductor, as always (the Space check and the engine's
/// upkeep read the same list after it). The Apple mode (`apple`): after the conductor, and
/// only when the window list was read this tick already (`list_read`: the mirror's read at
/// rest, every 2 s), so the sweep never reads one of its own. A window closed while its app
/// did not answer leaves the mirror at its next read anyway (the mirror keeps only windows
/// the list shows); the sweep only drops it from the observer, a read or two later than in
/// our engines. Until 1.9 it read the list about once a second in every mode, the Apple mode's
/// reads aside.
fn sweep_now(at: Sweep, apple: bool, list_read: bool) -> bool {
    match at {
        Sweep::BeforeConductor => !apple,
        Sweep::AfterConductor => apple && list_read,
    }
}

/// The engine that brings back what a release or a logout left (a window still in a park
/// corner): the one running, or under the Apple mode (which hides nothing) the launch's.
fn rest_engine() -> std::sync::Arc<dyn window_engine::WindowEngine> {
    let now = window_engine::current();
    if now.kind() != window_engine::EngineKind::Apple {
        return now;
    }
    window_engine::of(window_engine::choice().map_or(window_engine::EngineKind::Corner, |c| c.kind))
}

/// The launch's engine installed again (Apple's is off, or the app stands down).
fn install_launch_engine() {
    let kind = window_engine::choice().map_or(window_engine::EngineKind::Corner, |c| c.kind);
    window_engine::install(window_engine::of(kind));
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

/// Whether the look for a stand-down's leftovers set for `look` (a Desktop change plus the
/// list's settle, [`UNPARK_SETTLE`]) is due at `now`: the wait runs out between ticks, never
/// as a sleep on the window thread.
fn look_due(look: Option<Instant>, now: Instant) -> bool {
    look.is_some_and(|due| now >= due)
}

/// What a later look for a stand-down's leftovers knows now: `now` (the registry, the taken
/// windows, `state.json`), leaving alone the windows `parked` on purpose
/// ([`safety::parked_on_purpose`]: the controller's own, once the app arranges windows again
/// after a resume or a cancelled logout). The registry's windows are ours to every check, so
/// without this the look took each stage parked since for a window left in the corner and
/// brought all of them back on screen, at every Desktop change, while one window of the
/// stand-down was still away.
fn leftovers_now(now: safety::Ours, parked: std::collections::BTreeSet<u32>) -> safety::Ours {
    now.leaving(parked)
}

/// What a check knows: what an earlier check left on record (`kept`), else nothing more, with
/// what is known `now` ([`safety::Ours::merged`]).
fn with_kept(kept: Option<safety::Ours>, now: safety::Ours) -> safety::Ours {
    match kept {
        Some(kept) => kept.merged(now),
        None => now,
    }
}

/// The stages saved set aside while Apple's Stage Manager is on ([`paths::set_aside_state`]);
/// when that fails, dropped as before (no stale `state.json` while Apple's arranges).
fn drop_saved_stages() {
    let (state, aside) = (super::paths::state_file(), super::paths::stood_down_state_file());
    match super::paths::set_aside_state(&state, &aside) {
        Ok(super::paths::SetAside::Nothing) => {}
        Ok(_) => tracing::info!("saved stages set aside while Apple's Stage Manager arranges the windows: back when it is off"),
        Err(e) => {
            tracing::warn!("cannot set {} aside: {e}; dropped", state.display());
            if let Err(e) = std::fs::remove_file(&state) {
                tracing::warn!("cannot remove {}: {e}", state.display());
            }
        }
    }
}

fn run(rx: Receiver<WmCommand>, run_loop: &CFRunLoop, own: WmHandle) {
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
    let mut observer = Observer::new(std::process::id() as i32, only);
    minimize_tap::set_handle(own.clone());
    mission_key_tap::set_handle(own.clone());
    let probes = own.clone();
    observer.set_probe_notify(Arc::new(move |note| {
        probes.send(WmCommand::Probe(note));
    }));
    let state = WmState {
        observer,
        conductor: Conductor::ours(),
        scanned: false,
        pace: TickPace::new(clock::now()),
        rest: tick_pace::REST,
        trusted: false,
        stand_down: StandDown::default(),
        displays: None,
        settings: None,
        rescan: Rescan::default(),
        rescan_unanswered: Default::default(),
        stand_down_leftovers: None,
        leftovers_look: None,
        display_sweep: None,
        logout: Logout::default(),
        taken: TakenWatch::default(),
        launch_scan: None,
        own,
    };
    STATE.with(|s| *s.borrow_mut() = Some(state));
    INBOX.with(|i| *i.borrow_mut() = Some(rx));

    // The timer keeps the run loop alive (CFRunLoopRun returns at once with no sources)
    // and drives housekeeping.
    let tick_block = RcBlock::new(|_timer: *mut CFRunLoopTimer| tick());
    let first_fire = CFAbsoluteTimeGetCurrent() + tick_seconds();
    // SAFETY: valid arguments; the block is copied by CoreFoundation.
    let timer = unsafe { CFRunLoopTimer::with_handler(None, first_fire, tick_seconds(), 0, 0, Some(&tick_block)) };
    match &timer {
        Some(t) => {
            // SAFETY: the default-mode constant is a valid static.
            run_loop.add_timer(Some(t), unsafe { kCFRunLoopDefaultMode });
            TICK_TIMER.with(|f| *f.borrow_mut() = Some(t.clone()));
        }
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

    /// Round sm-tick: what stirs the tick. Typing does not; a click, a shortcut and every
    /// other command do.
    #[test]
    fn the_apple_modes_closed_window_sweep_reads_no_list_of_its_own() {
        // Our engines: before the conductor, whatever was read.
        assert!(sweep_now(Sweep::BeforeConductor, false, false));
        assert!(!sweep_now(Sweep::AfterConductor, false, true), "once a tick");
        // The Apple mode: after its tick, on the list it read.
        assert!(!sweep_now(Sweep::BeforeConductor, true, false));
        assert!(sweep_now(Sweep::AfterConductor, true, true));
        assert!(!sweep_now(Sweep::AfterConductor, true, false), "no read this tick: no sweep, no read");
        // A minute of brisk ticks (a person working: every 250 ms, a full pass about once a
        // second), the mirror reading the list every 2 s on the tick: the sweep's own reads.
        let ticks = 240;
        let full = |i: usize| i.is_multiple_of(4);
        let mirror_read = |i: usize| i.is_multiple_of(8);
        let before = (0..ticks).filter(|&i| full(i) && !mirror_read(i)).count();
        let after = (0..ticks).filter(|&i| full(i) && sweep_now(Sweep::AfterConductor, true, mirror_read(i)) && !mirror_read(i)).count();
        assert_eq!((before, after), (30, 0), "window list reads a minute the sweep added in the Apple mode");
    }

    #[test]
    fn a_plain_key_does_not_stir_the_tick_a_click_or_a_shortcut_does() {
        use crate::engine::geometry::Point;
        use input::{InputEvent, InputKind};
        let event = |kind, cmd, other_modifiers| {
            WmCommand::Input(
                InputEvent { kind, at: Point::new(10.0, 10.0), shift: false, cmd, other_modifiers, control: false, option: false, clicks: 0, key: None, stamp: 0.0, heard: Instant::now() },
                None,
            )
        };
        assert!(!stirs(&event(InputKind::KeyDown, false, false)), "typing");
        assert!(stirs(&event(InputKind::KeyDown, true, false)), "a ⌘ shortcut");
        assert!(stirs(&event(InputKind::KeyDown, false, true)), "a ⌃ or ⌥ key (Desktop switches, Mission Control)");
        assert!(!stirs(&event(InputKind::CommandKey, true, false)), "⌘ pressed alone");
        assert!(!stirs(&event(InputKind::CommandKey, false, false)), "⌘ let go alone");
        assert!(stirs(&event(InputKind::MouseDown, false, false)));
        assert!(stirs(&event(InputKind::MouseUp, false, false)));
        assert!(stirs(&event(InputKind::OtherMouseDown, false, false)));
        assert!(stirs(&WmCommand::SpaceChanged(Instant::now())));
        assert!(stirs(&WmCommand::ReloadConfig));
    }

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
        assert_eq!(ours.known(left), Known::Registered, "the window the head start could not move");
        assert_eq!(ours.known(held), Known::Registered, "the registry's held window");
        assert_eq!(ours.known(managed_then), Known::Managed, "the old controller's window");

        let fresh = with_kept(None, now);
        assert_eq!(fresh.known(left), Known::Unknown, "nothing kept: only what is known then");
        assert_eq!(fresh.known(held), Known::Registered);
    }

    /// A Desktop change while a window of the stand-down is still away: the look for it waits
    /// for the window list to settle (the Desktop slides in) without holding the thread.
    #[test]
    fn the_look_for_leftovers_waits_out_the_desktop_change_on_the_tick() {
        let changed = Instant::now();
        let look = Some(changed + UNPARK_SETTLE);
        assert!(!look_due(look, changed), "not at the Desktop change itself");
        assert!(!look_due(look, changed + UNPARK_SETTLE / 2));
        assert!(look_due(look, changed + UNPARK_SETTLE));
        assert!(!look_due(None, changed + UNPARK_SETTLE), "no look set: nothing to do on a tick");
    }

    /// After a resume (or a cancelled logout) with a window of the stand-down still away, the
    /// app arranges windows again: the look at every Desktop change must find only what the
    /// stand-down left, never the windows the controller parks now (they are the registry's,
    /// so ours to every check: each was brought back on screen).
    #[test]
    fn a_later_look_for_a_stand_downs_leftovers_leaves_the_parked_stages_alone() {
        use std::collections::BTreeSet;

        let (left, held, parked_since) = (31, 40, 7);
        // The stand-down's check left one window in the corner (kept for the retries).
        let kept = safety::Ours::from_parts(BTreeSet::new(), [left, parked_since].into(), None).kept_for_retry(&[left]);
        // Now: the registry holds the held window and one the fresh controller parked.
        let now = safety::Ours::from_parts([held, parked_since].into(), BTreeSet::new(), None);
        let parked: BTreeSet<u32> = [parked_since].into();
        let mut seen = None;
        let _ = retry_leftovers(kept.clone(), || leftovers_now(now.clone(), parked.clone()), |ours| {
            seen = Some(ours);
            None
        });
        let ours = seen.expect("the check ran");
        assert!(ours.left_alone(parked_since), "a stage's window parked since: stays parked");
        assert!(!ours.left_alone(left), "the window the stand-down left is still looked for");
        assert!(!ours.left_alone(held), "a held window too");
        // A check right after an unpark (quit, stand-down) leaves nothing alone, whatever an
        // earlier look did.
        let at_quit = with_kept(Some(kept.leaving(parked)), now);
        assert!(!at_quit.left_alone(parked_since));
    }

    /// The review of 6X2, 2: a managed window a busy app left as a sliver at the stand-down
    /// stays ours on every retry, though `state.json` and the model are gone by then, until
    /// a check finds it back.
    #[test]
    fn a_stand_downs_retries_remember_whose_windows_are_left() {
        use crate::engine::geometry::Rect;
        use crate::engine::corner::park::Corner;
        use crate::engine::persist::{SavedRole, SavedSpace, SavedStage, SavedWindow, StateFile};
        use crate::engine::recovery::{Known, ScanDisplays, is_stranded};
        use std::collections::BTreeSet;

        let displays = ScanDisplays::new(Rect::new(0.0, 0.0, 1440.0, 900.0), Rect::new(0.0, 25.0, 1440.0, 875.0), vec![], [Corner::BottomRight]);
        let sliver = Rect::new(1439.0, 820.0, 600.0, 392.0);
        let id = 4242;
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
        assert_eq!(after.known(id), Known::Unknown);
        assert!(!is_stranded(sliver, &displays, after.known(id)));

        // The first check knew it (the model's window, its app in state.json) and left it.
        let first = safety::Ours::from_parts(BTreeSet::new(), [id].into(), Some(state.clone()));
        assert_eq!(first.known(id), Known::Managed);
        let mut kept = Some(first.kept_for_retry(&[id]));

        // Two retries while the app is busy, then it answers.
        let mut seen = Vec::new();
        for round in 0..3 {
            let k = kept.take().expect("the windows left are kept until each is back");
            kept = retry_leftovers(k, safety::Ours::default, |ours| {
                seen.push((ours.known(id), is_stranded(sliver, &displays, ours.known(id)), ours.state.clone()));
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
