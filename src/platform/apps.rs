//! Running applications from `NSWorkspace`: a snapshot of regular apps and the launch /
//! terminate / activate / hide / unhide notifications, observed on the main thread and handed
//! on as plain Rust values; the active Space changing (`activeSpaceDidChangeNotification`);
//! and hiding or unhiding an app by its pid (`NSRunningApplication`, public API, any thread),
//! for `[windows] hide_parked_apps`.

use std::ptr::NonNull;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_app_kit::{
    NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification,
    NSWorkspaceApplicationKey, NSWorkspaceDidActivateApplicationNotification, NSWorkspaceDidHideApplicationNotification,
    NSWorkspaceDidLaunchApplicationNotification, NSWorkspaceDidTerminateApplicationNotification,
    NSWorkspaceDidUnhideApplicationNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSNotificationName, NSOperationQueue};
use serde::Serialize;

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
