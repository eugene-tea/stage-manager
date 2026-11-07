//! The guardian's wait after the app quit for a logout (round logout-cancel-relaunch, the
//! rule in [`crate::engine::logout_watch`]): it listens for the same logout signals the app
//! does ([`apps::observe_logout`]: `loginwindow`'s distributed notifications and notify(3)
//! names, or a test's own note), logs every one that arrives with the seconds since the quit,
//! and at a cancel opens the installed bundle it runs from again (`open -g -a`: LaunchServices,
//! in the background, nothing brought forward). A stop signal (the session ending: the logout
//! goes on), [`WATCH`] with no cancel, or Stage Manager running again ends it with nothing
//! opened. A test run (`STAGE_MANAGER_TEST` or `STAGE_MANAGER_HOME`) never opens anything: it
//! logs what it would open (an app opened by LaunchServices would not inherit the test's
//! environment and would manage the real windows).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFRunLoop, CFRunLoopRunResult, kCFRunLoopDefaultMode};
use objc2_foundation::MainThreadMarker;

use super::{apps, instance, paths};
use crate::engine::logout_watch::{End, Heard, Next, WATCH, Watch};
use crate::engine::stand_down::Signal;

/// One slice of the run loop between two looks (a stop signal is seen within it).
const SLICE: Duration = Duration::from_millis(100);

/// How long `open` may take before the guardian stops waiting for it.
const OPEN_WAIT: Duration = Duration::from_secs(10);

/// Waits after the app (`parent`) quit for a logout (module docs). `stopped`: the stop signal
/// the guardian received, if any. Returns once the watch has ended.
pub fn run(parent: u32, stopped: &dyn Fn() -> Option<i32>) {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!(parent, "guardian: not on the main thread; a cancelled logout cannot open Stage Manager again");
        return;
    };
    let quit = Instant::now();
    tracing::info!(
        parent,
        watch_s = WATCH.as_secs(),
        "guardian: the app quit for a logout, restart or shutdown; waiting for a cancel (one opens Stage Manager again)"
    );
    let heard: Rc<RefCell<Vec<(String, Signal)>>> = Rc::default();
    let sink = heard.clone();
    let _observers = apps::observe_logout(mtm, move |name, signal| sink.borrow_mut().push((name.to_string(), signal)));
    let mut watch = Watch::new(quit);
    let mut said_idle = false;
    loop {
        // SAFETY: the default mode is a valid framework static.
        let result = CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, SLICE.as_secs_f64(), false);
        if result == CFRunLoopRunResult::Finished {
            // No source in the mode yet: the slice is slept instead of spun.
            if !said_idle {
                tracing::debug!("guardian: the run loop has no source; sleeping between looks");
                said_idle = true;
            }
            std::thread::sleep(SLICE);
        }
        let now = Instant::now();
        let mut events: Vec<Heard> = heard
            .borrow_mut()
            .drain(..)
            .map(|(name, signal)| {
                tracing::info!(
                    signal = name.as_str(),
                    cancel = signal == Signal::Cancelled,
                    since_quit_ms = now.duration_since(quit).as_millis() as u64,
                    "guardian: logout signal after the quit"
                );
                Heard::Signal(signal)
            })
            .collect();
        if let Some(sig) = stopped() {
            tracing::info!(signal = sig, since_quit_ms = now.duration_since(quit).as_millis() as u64, "guardian: told to stop");
            events.push(Heard::Stopped);
        }
        if events.is_empty() {
            events.push(Heard::Nothing);
        }
        for h in events {
            match watch.step(h, now, instance::running) {
                Next::Wait => {}
                Next::Relaunch => {
                    tracing::info!(
                        since_quit_s = now.duration_since(quit).as_secs(),
                        "guardian: the logout was cancelled after the app quit: opening Stage Manager again (it restores the stages saved at the logout)"
                    );
                    relaunch();
                    return;
                }
                Next::End(why) => {
                    log_end(why, now.duration_since(quit));
                    return;
                }
            }
        }
    }
}

/// The end of a watch with nothing opened, in plain words.
fn log_end(why: End, since_quit: Duration) {
    tracing::info!(since_quit_s = since_quit.as_secs(), "guardian: {}", why.words());
}

/// Opens the bundle this guardian runs from again, unless this is a test run.
fn relaunch() {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("guardian: cannot find our own executable; Stage Manager is not opened again: {e}");
            return;
        }
    };
    let Some(bundle) = bundle_of(&exe) else {
        tracing::warn!(exe = %exe.display(), "guardian: not running from an app bundle; Stage Manager is not opened again");
        return;
    };
    if test_run() {
        tracing::info!(bundle = %bundle.display(), "guardian: a test run: would open the bundle again (nothing opened)");
        return;
    }
    let child = Command::new("/usr/bin/open")
        .arg("-g")
        .arg("-a")
        .arg(&bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(bundle = %bundle.display(), "guardian: cannot run open; Stage Manager is not opened again: {e}");
            return;
        }
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                tracing::info!(bundle = %bundle.display(), "guardian: Stage Manager opened again after the cancelled logout");
                return;
            }
            Ok(Some(status)) => {
                tracing::error!(bundle = %bundle.display(), ?status, "guardian: open failed; Stage Manager is not running");
                return;
            }
            Ok(None) if start.elapsed() < OPEN_WAIT => std::thread::sleep(SLICE),
            Ok(None) => {
                tracing::warn!(bundle = %bundle.display(), "guardian: open has not returned; not waiting for it");
                return;
            }
            Err(e) => {
                tracing::warn!("guardian: cannot wait for open: {e}");
                return;
            }
        }
    }
}

/// The app bundle holding executable `exe` (`…/Stage Manager.app/Contents/MacOS/…`).
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let named = macos.file_name()? == "MacOS" && contents.file_name()? == "Contents";
    (named && bundle.extension().is_some_and(|e| e == "app")).then(|| bundle.to_path_buf())
}

/// A test run (`STAGE_MANAGER_TEST` or `STAGE_MANAGER_HOME`): nothing is opened.
fn test_run() -> bool {
    std::env::var_os("STAGE_MANAGER_TEST").is_some() || paths::custom_home().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_the_one_the_executable_sits_in() {
        let p = |s: &str| bundle_of(Path::new(s));
        assert_eq!(p("/Applications/Stage Manager.app/Contents/MacOS/stage-manager"), Some(PathBuf::from("/Applications/Stage Manager.app")));
        assert_eq!(p("/Users/someone/Applications/Stage Manager.app/Contents/MacOS/stage-manager"), Some(PathBuf::from("/Users/someone/Applications/Stage Manager.app")));
        assert_eq!(p("/Users/someone/src/stage-manager/target/release/stage-manager"), None, "a bare binary");
        assert_eq!(p("/Applications/Stage Manager/Contents/MacOS/stage-manager"), None, "not an .app");
        assert_eq!(p("stage-manager"), None);
    }
}
