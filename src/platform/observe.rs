//! Observation state on the WM thread: running apps, the Accessibility flag and the macOS
//! settings. Window observation arrives with milestone 2.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::Receiver;

use objc2_core_foundation::CFRunLoop;
use serde_json::json;

use super::apps::{AppEvent, AppInfo};
use super::native_settings::RawSettings;
use super::wm_thread::WmCommand;

/// The `STAGE_MANAGER_ONLY` filter: a set of bundle ids, or `None` for every app.
pub fn parse_only(value: Option<&str>) -> Option<HashSet<String>> {
    let set: HashSet<String> =
        value?.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    if set.is_empty() { None } else { Some(set) }
}

/// Everything the WM thread knows so far.
pub struct Observation {
    own_pid: i32,
    only: Option<HashSet<String>>,
    accessibility: bool,
    settings: RawSettings,
    apps: BTreeMap<i32, AppInfo>,
}

thread_local! {
    static STATE: RefCell<Option<Observation>> = const { RefCell::new(None) };
    static INBOX: RefCell<Option<Receiver<WmCommand>>> = const { RefCell::new(None) };
}

/// Installs the state and command receiver on the current (WM) thread.
pub(crate) fn install(rx: Receiver<WmCommand>) {
    let only = parse_only(std::env::var("STAGE_MANAGER_ONLY").ok().as_deref());
    STATE.with(|s| *s.borrow_mut() = Some(Observation::new(std::process::id() as i32, only)));
    INBOX.with(|i| *i.borrow_mut() = Some(rx));
}

fn with_state(f: impl FnOnce(&mut Observation)) {
    STATE.with(|s| match s.try_borrow_mut() {
        Ok(mut guard) => {
            if let Some(state) = guard.as_mut() {
                f(state);
            }
        }
        Err(_) => tracing::warn!("observation state busy; event dropped"),
    });
}

/// Handles every queued command (scheduled by `WmHandle::send`).
pub(crate) fn drain() {
    loop {
        let cmd = INBOX.with(|i| i.borrow().as_ref().and_then(|rx| rx.try_recv().ok()));
        let Some(cmd) = cmd else { break };
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| with_state(|s| s.handle(cmd))));
        if result.is_err() {
            tracing::error!("panic while handling a WM command");
        }
    }
}

/// Periodic housekeeping (nothing yet).
pub(crate) fn tick() {}

impl Observation {
    fn new(own_pid: i32, only: Option<HashSet<String>>) -> Self {
        Self { own_pid, only, accessibility: false, settings: RawSettings::new(), apps: BTreeMap::new() }
    }

    fn handle(&mut self, cmd: WmCommand) {
        match cmd {
            WmCommand::Apps(list) => self.apps = list.into_iter().map(|a| (a.pid, a)).collect(),
            WmCommand::App(event) => self.on_app_event(event),
            WmCommand::Accessibility(trusted) => self.accessibility = trusted,
            WmCommand::NativeSettings(settings) => self.settings = settings,
            WmCommand::Dump(reply) => {
                let _ = reply.send(self.dump());
            }
            WmCommand::Shutdown(reply) => {
                if let Some(rl) = CFRunLoop::current() {
                    rl.stop();
                }
                let _ = reply.send(());
            }
        }
    }

    fn allowed(&self, app: &AppInfo) -> bool {
        app.pid != self.own_pid
            && match app.bundle_id.as_deref() {
                Some(b) => self.only.as_ref().is_none_or(|only| only.contains(b)),
                None => self.only.is_none(),
            }
    }

    fn on_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Launched(app) => {
                self.log_app("app launched", &app);
                self.apps.insert(app.pid, app);
            }
            AppEvent::Terminated(app) => {
                if self.apps.remove(&app.pid).is_some() {
                    self.log_app("app terminated", &app);
                }
            }
            AppEvent::Activated(app) => {
                self.log_app("app activated", &app);
                self.apps.entry(app.pid).or_insert(app);
            }
        }
    }

    fn log_app(&self, what: &str, app: &AppInfo) {
        let name = app.name.as_deref().unwrap_or("?");
        if self.allowed(app) {
            tracing::info!(pid = app.pid, bundle = app.bundle_or_unknown(), name, "{what}");
        } else {
            tracing::debug!(pid = app.pid, bundle = app.bundle_or_unknown(), name, "{what} (not observed)");
        }
    }

    fn dump(&self) -> serde_json::Value {
        let apps: Vec<_> = self.apps.values().filter(|a| self.allowed(a)).collect();
        let note = if self.accessibility {
            "window observation arrives with milestone 2"
        } else {
            "Accessibility not granted: windows are not observed"
        };
        json!({
            "ok": true,
            "accessibility": self.accessibility,
            "observing": false,
            "note": note,
            "macos_settings": self.settings,
            "apps": apps,
            "windows": [],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_filter_parses_comma_lists() {
        assert_eq!(parse_only(None), None);
        assert_eq!(parse_only(Some(" , ")), None);
        let set = parse_only(Some("a.b, c.d,,")).unwrap_or_default();
        assert!(set.contains("a.b") && set.contains("c.d") && set.len() == 2);
    }

    #[test]
    fn dump_without_accessibility_says_so() {
        let s = Observation::new(1, None);
        let d = s.dump();
        assert_eq!(d["accessibility"], false);
        assert!(d["note"].as_str().unwrap_or_default().contains("Accessibility not granted"));
    }
}
