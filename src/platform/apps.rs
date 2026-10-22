//! Running applications from `NSWorkspace`: a snapshot of regular apps and the launch /
//! terminate / activate / hide / unhide notifications, observed on the main thread and handed
//! on as plain Rust values; the active Space changing (`activeSpaceDidChangeNotification`);
//! and hiding or unhiding an app by its pid (`NSRunningApplication`, public API, any thread),
//! for `[windows] hide_parked_apps`; a logout, restart or shutdown coming or cancelled
//! ([`observe_logout`]: `loginwindow`'s distributed notifications and notify(3) names, and
//! `willPowerOffNotification`); when the login session began (`loginwindow`'s start).

use std::cell::RefCell;
use std::ffi::{CString, c_char, c_int};
use std::ptr::NonNull;
use std::time::Instant;

use block2::{Block, RcBlock};
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification,
    NSWorkspaceApplicationKey, NSWorkspaceDidActivateApplicationNotification, NSWorkspaceDidHideApplicationNotification,
    NSWorkspaceDidLaunchApplicationNotification, NSWorkspaceDidTerminateApplicationNotification,
    NSWorkspaceDidUnhideApplicationNotification, NSWorkspaceWillPowerOffNotification,
};
use objc2_foundation::{
    MainThreadMarker, NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSNotificationName,
    NSNotificationSuspensionBehavior, NSObject, NSOperationQueue, NSString,
};
use serde::Serialize;

use crate::engine::stand_down::{Channel, Signal, signals_to_observe};

/// A running application.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AppInfo {
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub name: Option<String>,
}

impl AppInfo {
    /// The bundle id, or `"?"` for logs.
    pub fn bundle_or_unknown(&self) -> &str {
        self.bundle_id.as_deref().unwrap_or("?")
    }
}

/// An `NSWorkspace` application event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppEvent {
    Launched(AppInfo),
    Terminated(AppInfo),
    /// The app became frontmost; the instant is when the main thread got the notice (the
    /// switch it causes logs how long after it went out: `since_activation_ms`).
    Activated(AppInfo, Instant),
    /// The app was hidden (⌘H, Hide Others, or our own hide).
    Hidden(AppInfo),
    /// The app was unhidden (an activation of a hidden app, Show All, or our own unhide).
    Unhidden(AppInfo),
}

fn info_of(app: &NSRunningApplication) -> AppInfo {
    AppInfo {
        pid: app.processIdentifier(),
        bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
        name: app.localizedName().map(|s| s.to_string()),
    }
}

fn is_regular(app: &NSRunningApplication) -> bool {
    app.activationPolicy() == NSApplicationActivationPolicy::Regular
}

/// Every running app with the regular activation policy (main thread).
pub fn running_regular_apps() -> Vec<AppInfo> {
    NSWorkspace::sharedWorkspace()
        .runningApplications()
        .iter()
        .filter(|a| is_regular(a) && !a.isTerminated())
        .map(|a| info_of(&a))
        .collect()
}

/// Keeps the workspace notification observers alive; dropping it unregisters them.
pub struct WorkspaceObservers {
    center: Retained<NSNotificationCenter>,
    tokens: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

impl Drop for WorkspaceObservers {
    fn drop(&mut self) {
        for token in &self.tokens {
            // SAFETY: each token came from this center's addObserverForName.
            unsafe { self.center.removeObserver(token.as_ref()) };
        }
    }
}

/// A notification name, the event it becomes, and whether only regular apps count.
type Route = (&'static NSNotificationName, fn(AppInfo) -> AppEvent, bool);

/// Observes launch / terminate / activate / hide / unhide on the main queue and calls `sink`
/// with plain values. All but terminate report regular apps only; terminate reports every app
/// (the receiver ignores pids it does not know).
pub fn observe_workspace(sink: impl Fn(AppEvent) + 'static) -> WorkspaceObservers {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let sink = std::rc::Rc::new(sink);
    let main_queue = NSOperationQueue::mainQueue();
    // SAFETY: the notification names are valid framework statics.
    let names: [Route; 5] = unsafe {
        [
            (NSWorkspaceDidLaunchApplicationNotification, AppEvent::Launched, true),
            (NSWorkspaceDidTerminateApplicationNotification, AppEvent::Terminated, false),
            (NSWorkspaceDidActivateApplicationNotification, |a| AppEvent::Activated(a, Instant::now()), true),
            (NSWorkspaceDidHideApplicationNotification, AppEvent::Hidden, true),
            (NSWorkspaceDidUnhideApplicationNotification, AppEvent::Unhidden, true),
        ]
    };
    let mut tokens = Vec::new();
    for (name, make, regular_only) in names {
        let sink = sink.clone();
        let block = RcBlock::new(move |note: NonNull<NSNotification>| {
            // SAFETY: AppKit passes a live notification for the duration of the block.
            let note = unsafe { note.as_ref() };
            let Some(app) = running_app_of(note) else { return };
            if regular_only && !is_regular(&app) {
                return;
            }
            sink(make(info_of(&app)));
        });
        // SAFETY: the block runs on the main queue only, where `sink` lives.
        let token =
            unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), None, Some(&main_queue), &block) };
        tokens.push(token);
    }
    WorkspaceObservers { center, tokens }
}

/// Observes `NSWorkspaceActiveSpaceDidChangeNotification` on the main queue: the user went
/// to another Space (Desktop or full-screen app), on any display. Calls `sink` each time.
pub fn observe_space_changes(sink: impl Fn() + 'static) -> WorkspaceObservers {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let main_queue = NSOperationQueue::mainQueue();
    let block = RcBlock::new(move |_note: NonNull<NSNotification>| sink());
    // SAFETY: the notification name is a valid framework static; the block runs on the
    // main queue only, where `sink` lives.
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceActiveSpaceDidChangeNotification),
            None,
            Some(&main_queue),
            &block,
        )
    };
    WorkspaceObservers { center, tokens: vec![token] }
}

/// A logout signal's handler: its name and meaning, on the main thread.
type LogoutSink = std::rc::Rc<dyn Fn(&str, Signal)>;

/// The distributed logout names listened for (with what each says) and their handler.
type LogoutNames = (Vec<(String, Signal)>, LogoutSink);

thread_local! {
    /// What [`LogoutListener`] hands its notifications to.
    static LOGOUT_SINK: RefCell<Option<LogoutNames>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; LogoutListener does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "SMLogoutListener"]
    struct LogoutListener;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for LogoutListener {}

    impl LogoutListener {
        // SAFETY: a notification observer's signature, `- (void)name:(NSNotification *)note`.
        #[unsafe(method(logoutSignal:))]
        fn logout_signal(&self, note: &NSNotification) {
            let name = note.name().to_string();
            let sink = LOGOUT_SINK.with(|s| {
                s.borrow().as_ref().and_then(|(names, sink)| names.iter().find(|(n, _)| *n == name).map(|(_, sig)| (*sig, sink.clone())))
            });
            if let Some((signal, sink)) = sink {
                sink(&name, signal);
            }
        }
    }
);

impl LogoutListener {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

unsafe extern "C" {
    /// notify(3): calls `handler` on `queue` each time `name` is posted.
    fn notify_register_dispatch(name: *const c_char, out_token: *mut c_int, queue: &DispatchQueue, handler: &Block<dyn Fn(c_int)>) -> u32;
    /// notify(3): stops a registration.
    fn notify_cancel(token: c_int) -> u32;
}

/// Keeps the logout observers alive; dropping it unregisters them.
pub struct LogoutObservers {
    distributed: Option<(Retained<NSDistributedNotificationCenter>, Retained<LogoutListener>)>,
    workspace: Option<WorkspaceObservers>,
    notify: Vec<c_int>,
}

impl Drop for LogoutObservers {
    fn drop(&mut self) {
        if let Some((center, listener)) = &self.distributed {
            // SAFETY: the listener was added to this center; it is removed from every name.
            unsafe { center.removeObserver_name_object(listener, None, None) };
        }
        for token in &self.notify {
            // SAFETY: each token came from notify_register_dispatch and is cancelled once.
            unsafe { notify_cancel(*token) };
        }
        LOGOUT_SINK.with(|s| s.borrow_mut().take());
    }
}

/// The test's own logout note: `STAGE_MANAGER_LOGOUT_NOTE`, honoured only with
/// `STAGE_MANAGER_TEST` set (a logout simulated without touching other apps).
fn logout_note(test: Option<std::ffi::OsString>, note: Option<String>) -> Option<String> {
    test?;
    note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
}

/// Observes every logout signal on the main thread ([`signals_to_observe`]: `loginwindow`'s
/// distributed notifications, delivered at once even while the app is not active; its notify(3)
/// names; `NSWorkspaceWillPowerOffNotification`), or only the test's own note
/// (`STAGE_MANAGER_LOGOUT_NOTE` with `STAGE_MANAGER_TEST`). Calls `sink` with the signal's
/// name and meaning each time one arrives. A name that cannot be registered is logged and
/// skipped.
pub fn observe_logout(mtm: MainThreadMarker, sink: impl Fn(&str, Signal) + 'static) -> LogoutObservers {
    let note = logout_note(std::env::var_os("STAGE_MANAGER_TEST"), std::env::var("STAGE_MANAGER_LOGOUT_NOTE").ok());
    if let Some(note) = &note {
        tracing::info!(note = %note, "logout signals: the test's own note only (STAGE_MANAGER_LOGOUT_NOTE)");
    }
    let sink: LogoutSink = std::rc::Rc::new(sink);
    let signals = signals_to_observe(note.as_deref());
    let mut out = LogoutObservers { distributed: None, workspace: None, notify: Vec::new() };

    let distributed: Vec<(String, Signal)> =
        signals.iter().filter(|(c, _, _)| *c == Channel::Distributed).map(|(_, n, s)| (n.clone(), *s)).collect();
    if !distributed.is_empty() {
        let center = NSDistributedNotificationCenter::defaultCenter();
        let listener = LogoutListener::new(mtm);
        for (name, _) in &distributed {
            // SAFETY: the listener answers `logoutSignal:` with a notification observer's
            // signature; it is kept alive (and removed) by the returned value.
            unsafe {
                center.addObserver_selector_name_object_suspensionBehavior(
                    &listener,
                    sel!(logoutSignal:),
                    Some(&NSString::from_str(name)),
                    None,
                    NSNotificationSuspensionBehavior::DeliverImmediately,
                );
            }
        }
        LOGOUT_SINK.with(|s| *s.borrow_mut() = Some((distributed, sink.clone())));
        out.distributed = Some((center, listener));
    }

    for (channel, name, signal) in &signals {
        match channel {
            Channel::Distributed => {}
            Channel::Notify => match register_notify(name, *signal, sink.clone()) {
                Some(token) => out.notify.push(token),
                None => tracing::warn!(name = %name, "cannot listen for this logout signal (notify)"),
            },
            Channel::Workspace => {
                let center = NSWorkspace::sharedWorkspace().notificationCenter();
                let main_queue = NSOperationQueue::mainQueue();
                let (sink, name, signal) = (sink.clone(), name.clone(), *signal);
                let block = RcBlock::new(move |_note: NonNull<NSNotification>| sink(&name, signal));
                // SAFETY: the notification name is a valid framework static; the block runs on
                // the main queue only, where `sink` lives.
                let token = unsafe {
                    center.addObserverForName_object_queue_usingBlock(Some(NSWorkspaceWillPowerOffNotification), None, Some(&main_queue), &block)
                };
                out.workspace = Some(WorkspaceObservers { center, tokens: vec![token] });
            }
        }
    }
    out
}

/// Registers notify(3) name `name` on the main queue; the handler calls `sink`. The token, or
/// `None` when notify refused it.
fn register_notify(name: &str, signal: Signal, sink: LogoutSink) -> Option<c_int> {
    let c_name = CString::new(name).ok()?;
    let owned = name.to_string();
    // The block runs on the main queue only (the main thread), where `sink` lives; notify
    // copies it, so the Rc inside is only ever touched there.
    let block = RcBlock::new(move |_token: c_int| sink(&owned, signal));
    let mut token: c_int = 0;
    // SAFETY: a valid C string, an out pointer to a live int, the main queue (a static) and a
    // block notify copies for the registration's life.
    let status = unsafe { notify_register_dispatch(c_name.as_ptr(), &mut token, DispatchQueue::main(), &block) };
    (status == 0).then_some(token)
}

/// Observes `AppleInterfaceThemeChangedNotification` (the distributed notification macOS
/// posts when Light, Dark or Auto flips the appearance) on the main queue; calls `sink`
/// each time. The app's effective appearance may still be the old one at the call.
pub fn observe_appearance(sink: impl Fn() + 'static) -> WorkspaceObservers {
    let center = NSDistributedNotificationCenter::defaultCenter();
    let main_queue = NSOperationQueue::mainQueue();
    let block = RcBlock::new(move |_note: NonNull<NSNotification>| sink());
    let name = NSString::from_str("AppleInterfaceThemeChangedNotification");
    // SAFETY: the block runs on the main queue only, where `sink` lives.
    let token = unsafe { center.addObserverForName_object_queue_usingBlock(Some(&name), None, Some(&main_queue), &block) };
    WorkspaceObservers { center: center.into_super(), tokens: vec![token] }
}

fn running_app_of(note: &NSNotification) -> Option<Retained<NSRunningApplication>> {
    let info = note.userInfo()?;
    // SAFETY: NSWorkspaceApplicationKey is a valid framework static.
    let key = unsafe { NSWorkspaceApplicationKey };
    let value = info.objectForKey(key)?;
    value.downcast::<NSRunningApplication>().ok()
}

/// The running app with process id `pid`, if any.
fn running(pid: i32) -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid).filter(|a| !a.isTerminated())
}

/// Asks app `pid` to hide (as ⌘H does: its windows on every Desktop leave the screen, no
/// animation). False when there is no such app or macOS refused the request; the hide itself
/// is asynchronous ([`is_hidden`] tells when it took).
pub fn hide(pid: i32) -> bool {
    running(pid).is_some_and(|a| a.hide())
}

/// Asks app `pid` to unhide, without activating it. False when there is no such app or macOS
/// refused the request.
pub fn unhide(pid: i32) -> bool {
    running(pid).is_some_and(|a| a.unhide())
}

/// Whether app `pid` is hidden now; `None` when it is not running.
pub fn is_hidden(pid: i32) -> Option<bool> {
    running(pid).map(|a| a.isHidden())
}

/// Whether app `pid` is the active (frontmost) app now; `None` when it is not running.
pub fn is_active(pid: i32) -> Option<bool> {
    running(pid).map(|a| a.isActive())
}

/// The bundle id of running app `pid`, if any.
pub fn bundle_of(pid: i32) -> Option<String> {
    running(pid).and_then(|a| a.bundleIdentifier()).map(|s| s.to_string())
}

/// When this login session began: the start of this user's `loginwindow` (it is relaunched
/// at each login), as an instant on our clock. `None` when it cannot be read. The restore's
/// settling waits at least a minute after it ([`crate::engine::restore::SETTLE_AFTER_LOGIN`]).
pub fn login_started() -> Option<Instant> {
    let uid = my_uid();
    let list = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str("com.apple.loginwindow"));
    let start = list
        .iter()
        .filter_map(|a| process_info(a.processIdentifier()))
        .filter(|i| i.pbi_uid == uid)
        .map(|i| std::time::UNIX_EPOCH + std::time::Duration::new(i.pbi_start_tvsec, (i.pbi_start_tvusec.min(999_999) * 1000) as u32))
        .max()?;
    let ago = std::time::SystemTime::now().duration_since(start).ok()?;
    Instant::now().checked_sub(ago)
}

/// This process's user id.
fn my_uid() -> libc::uid_t {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// `proc_pidinfo(PROC_PIDTBSDINFO)` of process `pid` (its user, start time); `None` when it
/// cannot be read (gone, or another user's).
fn process_info(pid: i32) -> Option<libc::proc_bsdinfo> {
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer is a proc_bsdinfo of exactly `size` bytes, ours for the call.
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
    // SAFETY: the kernel filled the whole struct when it returned its size; it was zeroed
    // before, and every field is plain data.
    (n == size).then(|| unsafe { info.assume_init() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tests_logout_note_needs_test_mode() {
        let test = Some(std::ffi::OsString::from("1"));
        assert_eq!(logout_note(test.clone(), Some(" dev.example.logout ".into())), Some("dev.example.logout".into()));
        assert_eq!(logout_note(None, Some("dev.example.logout".into())), None, "never outside a test");
        assert_eq!(logout_note(test, Some("  ".into())), None);
    }

    #[test]
    fn our_own_start_time_reads_back_in_the_past() {
        let me = process_info(std::process::id() as i32).expect("our own process");
        assert_eq!(me.pbi_uid, my_uid());
        let started = std::time::UNIX_EPOCH + std::time::Duration::from_secs(me.pbi_start_tvsec);
        assert!(started <= std::time::SystemTime::now());
        assert!(process_info(-1).is_none() && process_info(0).is_none());
    }
}
