//! Running applications from `NSWorkspace`: a snapshot of regular apps and the launch /
//! terminate / activate notifications, observed on the main thread and handed on as plain
//! Rust values.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_app_kit::{
    NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey,
    NSWorkspaceDidActivateApplicationNotification, NSWorkspaceDidLaunchApplicationNotification,
    NSWorkspaceDidTerminateApplicationNotification,
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
    Activated(AppInfo),
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

/// Observes launch / terminate / activate on the main queue and calls `sink` with plain
/// values. Launch and activate report regular apps only; terminate reports every app (the
/// receiver ignores pids it does not know).
pub fn observe_workspace(sink: impl Fn(AppEvent) + 'static) -> WorkspaceObservers {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let sink = std::rc::Rc::new(sink);
    let main_queue = NSOperationQueue::mainQueue();
    // SAFETY: the notification names are valid framework statics.
    let names: [Route; 3] = unsafe {
        [
            (NSWorkspaceDidLaunchApplicationNotification, AppEvent::Launched, true),
            (NSWorkspaceDidTerminateApplicationNotification, AppEvent::Terminated, false),
            (NSWorkspaceDidActivateApplicationNotification, AppEvent::Activated, true),
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

fn running_app_of(note: &NSNotification) -> Option<Retained<NSRunningApplication>> {
    let info = note.userInfo()?;
    // SAFETY: NSWorkspaceApplicationKey is a valid framework static.
    let key = unsafe { NSWorkspaceApplicationKey };
    let value = info.objectForKey(key)?;
    value.downcast::<NSRunningApplication>().ok()
}
