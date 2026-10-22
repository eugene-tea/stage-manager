//! The handoff of a guardian's watch, platform side (round 8N; the protocol and its decisions
//! are [`crate::engine::handoff`]):
//!
//! - [`offer`]: an old guardian hands the windows it still waits for to the running app
//!   (`ctl take-watch`);
//! - [`take`]: the app's answer, on the ctl connection's thread (never the WM thread, so a busy
//!   WM thread never makes a guardian wait on): the windows join its safety registry as taken
//!   ones ([`safety::take_watch`]), so its quit, its own guardian (the parked file names them)
//!   and its watch own them from then on. Refused while the app quits: the old guardian keeps
//!   them;
//! - [`TakenWatch`]: the app's watch of the windows it took, on the WM thread, by the
//!   guardian's own rules ([`Watch::registered_only`], [`guardian::reach_shown`]): a window
//!   seen back, gone, or in the model now is let go; one whose Desktop shows while it is still
//!   parked is brought back to the frame it was taken with, as the guardian would have. One
//!   whose app no longer runs, or that the window server's complete list no longer has, is
//!   dropped ([`TakenDrops`]): a handed-over window never travels from copy to copy forever.
//!   One still there and out of reach at a quit is passed on to this app's guardian with a
//!   warning ([`TakenWatch::last_look`]), never the error of a window this app parked.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::guardian::{self, Attempts};
use super::{ctl, safety, windows};
use crate::engine::guard_watch::{Event, Release, Watch};
use crate::engine::handoff::{Answer, Offer, TakenDrops, answer_from_reply};
use crate::engine::recovery::Registered;

/// How long a guardian waits for the answer to its offer (the app answers on the ctl thread).
const OFFER_TIMEOUT: Duration = Duration::from_secs(2);

/// The guardian of crashed app `app` offers `windows` (those it still waits for) to the
/// running Stage Manager.
pub fn offer(app: u32, windows: Vec<Registered>) -> Answer {
    let offer = Offer { app, guardian: std::process::id(), corners: safety::corners_used(), windows };
    let line = match offer.line() {
        Ok(line) => line,
        Err(e) => {
            tracing::warn!("guardian: cannot write the handoff: {e}");
            return Answer::Unanswered;
        }
    };
    match ctl::request(&line, OFFER_TIMEOUT) {
        Ok(reply) => answer_from_reply(&reply),
        Err(e) => {
            tracing::debug!("guardian: no answer to the handoff ({e})");
            Answer::Unanswered
        }
    }
}

/// The app's answer to an old guardian's offer (`ctl take-watch`): takes every window, unless
/// it is `quitting` (the guardian keeps them).
pub fn take(offer: &Offer, quitting: bool) -> Value {
    if quitting {
        tracing::info!(
            guardian = offer.guardian,
            windows = offer.windows.len(),
            "a crashed copy's guardian offered its watch while this app quits; not taken (it watches on)"
        );
        return ctl::error_reply("quitting: the watch is not taken");
    }
    let (ids, new) = safety::take_watch(&offer.windows, &offer.corners);
    tracing::info!(
        guardian = offer.guardian,
        crashed_app = offer.app,
        windows = ?ids,
        new,
        "took a crashed copy's guardian's watch: these windows are this app's to bring back now (its quit, its guardian, its watch)"
    );
    json!({ "ok": true, "taken": ids, "pid": std::process::id() })
}

/// The app's watch of the windows it took from an old guardian (WM thread).
#[derive(Default)]
pub struct TakenWatch {
    watch: Option<Watch>,
    attempts: HashMap<u32, Attempts>,
    drops: TakenDrops,
}

impl TakenWatch {
    /// One look, about once a second while windows are taken (nothing when none is): every
    /// taken window `managed` by the model is let go (the model sees to it); then the
    /// guardian's rules: seen back or gone is let go, one listed on screen still parked is
    /// brought back to its taken frame. Each window let go leaves the taken ones (and the
    /// parked file).
    pub fn pass(&mut self, managed: &BTreeSet<u32>) {
        let taken = safety::taken();
        if taken.is_empty() {
            self.watch = None;
            self.attempts.clear();
            self.drops = TakenDrops::default();
            return;
        }
        let now = Instant::now();
        // The window server's complete list (every Desktop, ordered-out windows included).
        let list = windows::list_windows();
        let listed: HashSet<u32> = list.iter().map(|c| c.id).collect();
        let blind = !list.iter().any(|c| c.onscreen);
        let dropped = self.drops.look(now, &taken, &listed, blind, process_runs);
        if !dropped.is_empty() {
            let ids: Vec<u32> = dropped.iter().map(|(r, _)| r.id).collect();
            for (r, why) in &dropped {
                tracing::info!(id = r.id, pid = r.pid, "taken watch: a handed-over window dropped: {why}");
            }
            safety::taken_forget(&ids);
        }
        let taken = safety::taken();
        if taken.is_empty() {
            self.watch = None;
            self.attempts.clear();
            return;
        }
        let w = self.watch.get_or_insert_with(|| Watch::registered_only(&[]));
        let held: BTreeSet<u32> = taken.iter().map(|r| r.id).collect();
        // Another path saw to these (the quit's leftovers check, a hidden tab judged).
        let gone: Vec<u32> = w.waiting().map(|x| x.id).filter(|id| !held.contains(id)).collect();
        for id in gone {
            w.forget(id);
            self.attempts.remove(&id);
        }
        for r in taken {
            if w.add(r) {
                tracing::debug!(id = r.id, pid = r.pid, "taken watch: a window joins");
            }
        }
        let in_model: Vec<u32> = held.iter().copied().filter(|id| managed.contains(id)).collect();
        let mut events = w.let_go(&in_model, Release::Managed);
        let displays = safety::scan_displays();
        events.extend(w.observe(now, &guardian::listed_windows(&list), &displays));
        let reached = RefCell::new(Vec::new());
        guardian::reach_shown(w, &displays, &mut self.attempts, now, "taken watch", &|e| reached.borrow_mut().push(*e));
        events.extend(reached.into_inner());
        let mut done = Vec::new();
        for e in &events {
            log_event(e);
            if let Event::LetGo { id, .. } = e {
                done.push(*id);
            }
        }
        for id in &done {
            w.forget(*id);
            self.attempts.remove(id);
        }
        safety::taken_forget(&done);
    }

    /// The quit's last look, after its leftovers check: a pass with every attempt due again
    /// (a window shown is reached at once; one whose app quit is dropped). Returns how many
    /// taken windows are left (another Desktop's, an app that does not answer): the exit is
    /// unclean then, and this app's guardian takes them over (a warning: passed on).
    pub fn last_look(&mut self, managed: &BTreeSet<u32>) -> usize {
        self.attempts.clear();
        self.pass(managed);
        let left = safety::taken_len();
        if left > 0 {
            tracing::warn!(windows = ?safety::taken_ids(), "a handed-over window still away; passed on");
        }
        left
    }
}

/// True when process `pid` runs (signal 0: exists; `EPERM` is another user's, still running).
fn process_runs(pid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; it only checks that the process exists. The caller
    // never passes a pid below 1 (which would address a process group or every process).
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Logs one change of the taken watch.
fn log_event(e: &Event) {
    match e {
        Event::LetGo { id, pid, why } => tracing::info!(id, pid, "taken watch: let go of a window: {why}"),
        Event::Looks { id, pid, look } => tracing::debug!(id, pid, "taken watch: window {look}"),
        Event::Joined { id, pid, .. } => tracing::debug!(id, pid, "taken watch: a window joins"),
        Event::ScreenChanged { on_screen } => tracing::debug!(on_screen, "taken watch: the windows on screen changed"),
        Event::Blind { listed } => tracing::debug!(listed, "taken watch: the window server lists no window on screen"),
    }
}
