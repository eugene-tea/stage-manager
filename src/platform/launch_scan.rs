//! The launch's recovery scan, one app at a time on threads of its own (the 1.7 review, L1).
//!
//! At the first Accessibility grant every app with a window is read for windows a crashed copy
//! left in the park zone ([`super::safety`]'s recovery scan, S4) before it is observed. Read
//! one app after another on the WM thread, an app that did not answer cost the
//! 0.25 s messaging timeout and more, and every app behind it waited: at a busy login a dozen
//! such apps held observation (and every queued command) about three seconds.
//!
//! Now [`read_all`] reads each app on a thread of its own and the WM thread waits at most
//! [`SCAN_WAIT`] for them: the apps read by then have their windows brought back at once and
//! are observed as before, at observation's start. An app read later reaches the WM thread on
//! its own ([`super::wm_thread::WmCommand::LaunchScan`]): its windows are brought back then
//! and only then is it observed ([`super::observe::Observer::hold_for_scan`]), so a window
//! still parked from a crash is back before or as its app is observed, and one slow app holds
//! up no other.
//!
//! The threads only read (as [`super::observe_probe`]'s do); every move is made on the WM
//! thread ([`Run::apply`]), leaving out the windows we park by then (the registry's).
//! Each app's scan logs, at debug, how long its read took and how long after the scan's start
//! the WM thread had it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::ax::AxError;
use super::enhanced_ui;
use super::safety::{self, Report, ScannedWindow};
use crate::engine::persist::StateFile;
use crate::engine::recovery::{Registered, ScanDisplays};
use crate::logbook::counters::{self, Count};

/// How long the WM thread waits for the apps' reads before observation starts: those read by
/// then are observed at the start, as before (as long as observation's own ask,
/// [`super::observe::START_WAIT`]).
pub const SCAN_WAIT: Duration = Duration::from_millis(300);

/// How long an app may hold its observation for its read: one that takes longer (an app
/// with many windows, each read waiting out the timeout) is observed without it, its windows
/// still brought back when the read ends.
pub const HOLD_MAX: Duration = Duration::from_secs(10);

/// One app's read, as it reaches the WM thread.
#[derive(Debug)]
pub struct Done<T> {
    /// The app's process.
    pub pid: i32,
    /// Its bundle id, when known.
    pub bundle: Option<String>,
    /// What the read found; `None` when it panicked.
    pub value: Option<T>,
    /// How long the read took on its thread.
    pub took: Duration,
}

/// Apps to read: each one's process and bundle id, when known.
pub type Apps = Vec<(i32, Option<String>)>;

/// The launch scan's read of one app: its windows, or the error of an app that did not answer.
pub type Read = Result<Vec<ScannedWindow>, AxError>;

/// The real read of app `pid`: its windows, every one (none is ours yet at the launch).
pub fn read(pid: i32, bundle: Option<&str>) -> Read {
    safety::read_app_windows(pid, bundle, &BTreeSet::new())
}

/// Reads every app of `apps` at the same time, each on a thread of its own, with `read`; waits
/// at most `wait`. Returns the reads done within it and the apps whose thread did not start
/// (the caller reads those itself); each read done later goes to `late`, from a thread of
/// its own, as it comes.
pub fn read_all<T: Send + 'static>(
    apps: Apps,
    read: fn(i32, Option<&str>) -> T,
    wait: Duration,
    late: impl Fn(Done<T>) + Send + 'static,
) -> (Vec<Done<T>>, Apps) {
    let (tx, rx) = mpsc::channel();
    let mut asked = 0usize;
    let mut not_started = Vec::new();
    for (pid, bundle) in apps {
        let tx = tx.clone();
        let theirs = bundle.clone();
        let spawned = std::thread::Builder::new().name("launch-scan".into()).spawn(move || {
            let t0 = Instant::now();
            let value = safety::guarded("a launch scan read", || read(pid, theirs.as_deref()));
            let _ = tx.send(Done { pid, bundle: theirs, value, took: t0.elapsed() });
        });
        match spawned {
            Ok(_) => asked += 1,
            Err(e) => {
                tracing::warn!(pid, "cannot start a thread to read an app for the launch scan: {e}");
                not_started.push((pid, bundle));
            }
        }
    }
    drop(tx);
    let deadline = Instant::now() + wait;
    let mut done = Vec::new();
    while done.len() < asked {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else { break };
        match rx.recv_timeout(left) {
            Ok(d) => done.push(d),
            Err(_) => break,
        }
    }
    if done.len() < asked {
        // The rest go to the WM thread as they come (the channel closes with the last read).
        let rx = Arc::new(Mutex::new(Some(rx)));
        let theirs = rx.clone();
        let forward = move || {
            let Some(rx) = theirs.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
            for d in rx {
                late(d);
            }
        };
        if let Err(e) = std::thread::Builder::new().name("launch-scan-late".into()).spawn(forward) {
            // Unlikely; the WM thread waits for them then, as it used to.
            tracing::warn!("cannot start a thread for the launch scan's late reads: {e}; waiting for them");
            if let Some(rx) = rx.lock().unwrap_or_else(|e| e.into_inner()).take() {
                done.extend(rx);
            }
        }
    }
    (done, not_started)
}

/// The launch scan under way: what it brings windows back to, the apps not read yet, and
/// what it did so far.
pub struct Run {
    started: Instant,
    waiting: BTreeSet<i32>,
    crashed: BTreeMap<u32, Registered>,
    state: Option<StateFile>,
    displays: ScanDisplays,
    report: Report,
    unanswered: Vec<i32>,
}

impl Run {
    /// A scan of the apps `pids`, bringing windows back to what a crashed copy registered
    /// (its parked file), else `state.json`'s frames, read once now.
    pub fn new(pids: impl IntoIterator<Item = i32>) -> Self {
        Self {
            started: Instant::now(),
            waiting: pids.into_iter().collect(),
            crashed: safety::crashed_registered(),
            state: safety::read_state("recovery scan"),
            displays: safety::scan_displays(),
            report: Report::default(),
            unanswered: Vec::new(),
        }
    }

    /// One app's read: its windows in the park zone are brought back now (on the WM thread),
    /// those we park by now (the registry's) left alone. True when the app answered.
    pub fn apply(&mut self, done: Done<Read>) -> bool {
        let Done { pid, bundle, value, took } = done;
        self.waiting.remove(&pid);
        let since_ms = self.started.elapsed().as_millis() as u64;
        let read_ms = took.as_millis() as u64;
        let list = match value {
            Some(Ok(list)) => list,
            Some(Err(e)) => {
                tracing::debug!(pid, bundle, read_ms, since_ms, "launch scan: the app does not answer: {e}");
                self.unanswered.push(pid);
                return false;
            }
            None => {
                tracing::debug!(pid, bundle, read_ms, since_ms, "launch scan: the read of the app failed");
                self.unanswered.push(pid);
                return false;
            }
        };
        let ours = safety::registry_ids();
        let list: Vec<ScannedWindow> = list.into_iter().filter(|w| !w.live.id.is_some_and(|id| ours.contains(&id))).collect();
        let before = self.report;
        let _quiet = enhanced_ui::Batch::open();
        safety::bring_back_scanned("launch", list, &self.crashed, self.state.as_ref(), &self.displays, &mut self.report);
        tracing::debug!(
            pid,
            bundle,
            read_ms,
            since_ms,
            windows = self.report.checked - before.checked,
            restored = self.report.restored - before.restored,
            failed = self.report.failed - before.failed,
            "launch scan: app scanned"
        );
        true
    }

    /// True once every app was read.
    pub fn finished(&self) -> bool {
        self.waiting.is_empty()
    }

    /// The apps not read yet.
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// What the scan did, and the apps that did not answer; logged as the one-pass scan's
    /// summary was, with the time it took.
    pub fn finish(self) -> (Report, Vec<i32>) {
        let r = self.report;
        let took_ms = self.started.elapsed().as_millis() as u64;
        if r.restored > 0 {
            counters::bump(Count::Recovery);
        }
        if self.unanswered.is_empty() {
            tracing::info!(reason = "launch", checked = r.checked, restored = r.restored, failed = r.failed, took_ms, "recovery scan");
        } else {
            tracing::info!(reason = "launch", checked = r.checked, restored = r.restored, failed = r.failed, unanswered = ?self.unanswered, took_ms, "recovery scan (some apps did not answer)");
        }
        (r, self.unanswered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stuck apps: each read waits out the 0.25 s timeout.
    const STUCK: [i32; 2] = [6001, 6003];
    const READY: i32 = 6002;

    fn fake(pid: i32, _bundle: Option<&str>) -> i32 {
        if STUCK.contains(&pid) {
            std::thread::sleep(Duration::from_millis(250));
        }
        pid
    }

    /// L1: the apps read in turn on the WM thread, a responsive app waited for every slow one
    /// before it (twelve at a login: about three seconds). Read each on its own thread, the
    /// responsive one is read within the wait and the stuck ones come later, side by side.
    #[test]
    fn a_stuck_apps_read_holds_up_neither_the_others_nor_the_wait() {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let apps = vec![(STUCK[0], None), (READY, Some("com.example.ready".to_string())), (STUCK[1], None)];
        let t0 = Instant::now();
        let (done, not_started) = read_all(apps, fake, Duration::from_millis(100), move |d| {
            let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send((d, Instant::now()));
        });
        let took = t0.elapsed();
        assert!(not_started.is_empty());
        assert!(took < Duration::from_millis(200), "the wait is bounded, not the stuck apps' sum ({took:?})");
        let read: Vec<i32> = done.iter().map(|d| d.pid).collect();
        assert_eq!(read, vec![READY], "the ready app is read within the wait");
        assert_eq!(done[0].bundle.as_deref(), Some("com.example.ready"));
        assert_eq!(done[0].value, Some(READY));
        let mut late = Vec::new();
        for _ in 0..2 {
            let (d, at) = rx.recv_timeout(Duration::from_secs(2)).expect("the stuck apps' reads come later");
            assert!(at.duration_since(t0) < Duration::from_millis(450), "read side by side, not in turn");
            late.push(d.pid);
        }
        late.sort_unstable();
        assert_eq!(late, STUCK.to_vec());
    }

    #[test]
    fn a_panicking_read_still_reports() {
        let (done, _) = read_all(vec![(7001, None)], |_, _| -> i32 { panic!("a stand-in read panics") }, Duration::from_millis(500), |_| {});
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].value, None, "reported, so the app is not held");
    }
}
