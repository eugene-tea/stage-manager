//! The guardian process (`stage-manager --guardian <pid>`), safety layer 5: spawned at
//! launch in its own session, it waits for the app to exit (kqueue `NOTE_EXIT`). After a
//! clean exit (the app's clean-exit marker) it has nothing to do. Otherwise:
//!
//! 1. It reads the windows the app had registered (the parked file the registry keeps,
//!    [`paths::parked_file`]) and runs the recovery scan.
//! 2. It then watches ([`guard_watch::Watch`]): every registered window, and any allowed
//!    app's window found in the park zone, on any Space, waits until the guardian has *seen it
//!    back* (read through Accessibility outside the park zone, or listed on screen outside it
//!    in settled passes a second apart) or *gone* (not listed on any Space for 3 s). A single
//!    listing never ends the watch: while a Space slides in the window server may list a
//!    window at an in-between frame or not at all. Accessibility reaches most apps' windows
//!    only on the Space shown, so a window parked on another Desktop is moved back once the
//!    window server lists it on screen (the user went there): looked at every [`POLL`],
//!    retried while it stays shown. Every change in how a waiting window looks is logged,
//!    and why each one is let go.
//! 3. It never exits while a window still waits, or it is told to stop (SIGTERM, SIGINT,
//!    SIGHUP: one last scan first). When Stage Manager runs again, the guardian asks it which
//!    waiting windows it has adopted (`ctl adopted`) and lets those go
//!    ([`Watch::hand_over`]); the crashed app's other windows keep waiting (the new app finds
//!    a window on another Desktop only once that Desktop shows), and windows found in the
//!    park zone are the new app's. An earlier version that cannot say takes over every
//!    window, as before. One that stays busy (no answer) for [`BUSY_CAP`] while a waiting
//!    window shows is taken as having adopted nothing. A panic inside a pass is logged and
//!    the watch goes on.
//! 4. A hidden tab (8J, [`hidden_tabs`]) found in the park zone never joins the watch, and
//!    one waiting is let go: nothing can move it, and it takes its group's frame once
//!    selected. The guardian waits only for windows it can bring back. A window the app
//!    registered is never judged one.

use std::collections::{BTreeSet, HashMap};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use super::ax::AxElement;
use super::safety::{self, guarded};
use super::{ctl, hidden_tabs, observe, paths, windows};
use crate::engine::guard_watch::{Event, Look, Watch, Waiting};
use crate::engine::recovery::{Known, Listed, Registered, ScanDisplays, is_stranded};
use crate::logbook::UNCLEAN_EXIT;
use crate::logbook::counters::{self, Count};

/// The AX messaging timeout used by the guardian.
const AX_TIMEOUT_SECONDS: f32 = 0.25;

/// How often the guardian looks at the window list while windows wait.
const POLL: Duration = Duration::from_millis(500);

/// Attempts at moving a shown window back at every pass, before slowing down to
/// [`SLOW_RETRY`] (a dialog that refuses to move, say).
const FAST_ATTEMPTS: u32 = 10;

/// The retry interval after [`FAST_ATTEMPTS`].
const SLOW_RETRY: Duration = Duration::from_secs(5);

/// How long after moving windows the window server's list is trusted again.
const SETTLE: Duration = Duration::from_millis(300);

/// How often the guardian asks a running Stage Manager what it adopted while no waiting
/// window shows.
const ASK_EVERY: Duration = Duration::from_secs(3);

/// How long a running Stage Manager may stay busy (consecutive [`Successor::Busy`] answers)
/// while a waiting window shows before the guardian treats it as having adopted nothing and
/// brings the crashed copy's registered windows back itself (the review of 8A, M2): a hung
/// successor must not keep them parked.
const BUSY_CAP: Duration = Duration::from_secs(30);

/// A stop signal received while watching (0: none).
static STOP_SIGNAL: AtomicI32 = AtomicI32::new(0);

/// Spawns the guardian for this process. It runs in its own session, so it outlives the app
/// and its process group.
pub fn spawn() -> Option<u32> {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("cannot find our own executable for the guardian: {e}");
            return None;
        }
    };
    let mut cmd = Command::new(exe);
    cmd.arg("--guardian")
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches nothing shared with the parent.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            tracing::info!(pid, "guardian started");
            let _ = std::thread::Builder::new().name("guardian-reaper".into()).spawn(move || {
                match child.wait() {
                    Ok(status) => tracing::warn!(?status, "guardian exited while the app runs"),
                    Err(e) => tracing::warn!("guardian wait failed: {e}"),
                }
            });
            Some(pid)
        }
        Err(e) => {
            tracing::error!("cannot start the guardian: {e}");
            None
        }
    }
}

/// `stage-manager --guardian <pid>`: waits for `pid` to exit; unless it wrote its clean-exit
/// marker, runs the recovery scan and waits for every window left parked (module docs).
pub fn run(args: &[String]) -> ExitCode {
    super::logging::init(super::logging::Process::Guardian);
    std::panic::set_hook(Box::new(|info| {
        counters::bump(Count::Panic);
        tracing::error!("guardian: panic: {info}");
    }));
    let Some(parent) = args.first().and_then(|a| a.parse::<i32>().ok()).filter(|p| *p > 0) else {
        eprintln!("usage: stage-manager --guardian <pid>");
        return ExitCode::from(2);
    };
    if let Err(e) = AxElement::system_wide().set_messaging_timeout(AX_TIMEOUT_SECONDS) {
        tracing::debug!("guardian: cannot set the AX timeout: {e}");
    }
    tracing::debug!(parent, "guardian watching");
    wait_for_exit(parent);
    let marker = paths::clean_exit_marker(parent as u32);
    let parked_file = paths::parked_file(parent as u32);
    if marker.exists() {
        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(&parked_file);
        tracing::debug!(parent, "guardian: clean exit; nothing to do");
        return ExitCode::SUCCESS;
    }
    let file = safety::read_parked_file(parent as u32);
    // The park zone sits above a bottom Dock: the app's visible frame, not a guess.
    match file.visible {
        Some(v) => safety::assume_visible_frame(v),
        None => tracing::warn!("guardian: the app left no visible frame; guessing one (a Dock at the bottom may hide parked windows from the scan)"),
    }
    // Windows may sit at any corner the app parked them in this run.
    safety::assume_corners(&file.corners);
    // Apps the app hid instead of parking: shown again first (their windows sit at their own
    // frames, or parked, which the scan and the watch below cover).
    if !file.hidden.is_empty() {
        let n = safety::unhide_apps("guardian", file.hidden.iter().cloned());
        tracing::warn!(hidden = file.hidden.len(), unhidden = n, "guardian: apps the app hid are unhidden");
    }
    let registered = file.windows;
    tracing::warn!(parent, registered = registered.len(), visible = ?file.visible, corners = ?file.corners, "guardian: {UNCLEAN_EXIT}; running the recovery scan");
    let r = safety::recovery_scan("guardian");
    tracing::warn!(checked = r.checked, restored = r.restored, failed = r.failed, "guardian: recovery scan done");
    install_stop_handlers();
    // The window server lists moved windows at their new frames a moment later.
    std::thread::sleep(SETTLE);
    watch(&registered);
    let _ = std::fs::remove_file(&parked_file);
    ExitCode::SUCCESS
}

/// How a watch pass ended.
enum Pass {
    /// Keep watching.
    Wait,
    /// Nothing is left, or Stage Manager runs again.
    Done,
}

/// Restore attempts at one window: how many, and when the last one was.
#[derive(Default)]
struct Attempts {
    count: u32,
    last: Option<Instant>,
}

impl Attempts {
    fn due(&self, now: Instant) -> bool {
        self.count < FAST_ATTEMPTS || self.last.is_none_or(|t| now.duration_since(t) >= SLOW_RETRY)
    }
}

/// The watch after the scan: see the module docs.
fn watch(registered: &[Registered]) {
    let mut w = Watch::new(registered);
    let mut reported: Option<Vec<(u32, bool)>> = None;
    let mut attempts: HashMap<u32, Attempts> = HashMap::new();
    let mut asked: Option<Instant> = None;
    let mut busy = BusyStreak::default();
    loop {
        let sig = STOP_SIGNAL.load(Ordering::SeqCst);
        if sig != 0 {
            tracing::warn!(signal = sig, "guardian: told to stop; one last scan");
            let r = safety::recovery_scan("guardian: stopping");
            tracing::warn!(restored = r.restored, failed = r.failed, "guardian: stopped");
            return;
        }
        let pass = guarded("the guardian's watch", || watch_pass(&mut w, &mut reported, &mut attempts, &mut asked, &mut busy));
        if matches!(pass, Some(Pass::Done)) {
            return;
        }
        std::thread::sleep(POLL);
    }
}

/// One look at the window list: done when nothing waits; otherwise logs what changed and
/// reaches every waiting window on screen through Accessibility (moving it back when it is
/// still parked).
fn watch_pass(
    w: &mut Watch,
    reported: &mut Option<Vec<(u32, bool)>>,
    attempts: &mut HashMap<u32, Attempts>,
    asked: &mut Option<Instant>,
    busy: &mut BusyStreak,
) -> Pass {
    let displays = safety::scan_displays();
    let now = Instant::now();
    let list = windows::list_windows();
    let listed = listed_windows(&list);
    // Hidden tabs first (8J): none joins, a waiting one is let go.
    for e in w.hidden_tabs(&hidden_tabs_found(w, &list, &listed, &displays)) {
        log_event(&e);
    }
    for e in w.observe(now, &listed, &displays) {
        log_event(&e);
    }
    if w.is_empty() {
        tracing::info!("guardian: no window is left waiting; done");
        return Pass::Done;
    }
    let ids: Vec<u32> = w.waiting().map(|x| x.id).collect();
    // Asked every pass while a waiting window shows (it may be moved now), else every
    // ASK_EVERY: a window on a Desktop never visited keeps the guardian waiting for long.
    let shows = w.waiting().any(|x| x.look.is_some_and(|l| l.shown()));
    let due = shows || asked.is_none_or(|t| now.duration_since(t) >= ASK_EVERY);
    let answer = if due {
        *asked = Some(now);
        busy.note(successor(&ids), shows, now)
    } else {
        Successor::None
    };
    match answer {
        Successor::None => {}
        Successor::Older => {
            tracing::info!(windows = ?ids, "guardian: an earlier Stage Manager runs again (it cannot say what it adopted) and takes over these windows");
            return Pass::Done;
        }
        Successor::Busy => {
            // It runs but does not answer yet (its launch scan): nothing moves this pass, so
            // the guardian never fights it over a window.
            if !w.handed_over() {
                tracing::info!("guardian: Stage Manager runs again; watching until it adopts each window");
            }
            for e in w.hand_over(&[]) {
                log_event(&e);
            }
            return if w.is_empty() { Pass::Done } else { Pass::Wait };
        }
        Successor::Adopted(adopted) => {
            if !w.handed_over() {
                tracing::info!("guardian: Stage Manager runs again; watching until it adopts each window");
            }
            for e in w.hand_over(&adopted) {
                log_event(&e);
            }
            if w.is_empty() {
                tracing::info!("guardian: Stage Manager runs again and has adopted every window; done");
                return Pass::Done;
            }
        }
    }
    let now_state: Vec<(u32, bool)> = w.waiting().map(|x| (x.id, x.look.is_some_and(|l| l.shown()))).collect();
    if reported.as_ref() != Some(&now_state) {
        report(w);
        *reported = Some(now_state);
    }
    attempts.retain(|id, _| w.waiting().any(|x| x.id == *id));
    reach_shown(w, &displays, attempts, now);
    if w.is_empty() {
        tracing::info!("guardian: no window is left waiting; done");
        return Pass::Done;
    }
    Pass::Wait
}

/// Whether Stage Manager runs again, and which of the waiting windows it has adopted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Successor {
    /// Nothing answers on the socket.
    None,
    /// It is there but did not answer in time (busy with its launch).
    Busy,
    /// An earlier version: it does not know `adopted`.
    Older,
    /// These of the ids asked about are in its model.
    Adopted(Vec<u32>),
}

/// Asks a running Stage Manager which of `ids` it has adopted (`ctl adopted`).
fn successor(ids: &[u32]) -> Successor {
    let words: Vec<String> = std::iter::once("adopted".to_string()).chain(ids.iter().map(u32::to_string)).collect();
    match ctl::request(&words.join(" "), Duration::from_millis(500)) {
        Ok(reply) => successor_from_reply(&reply),
        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Successor::Busy,
        Err(_) => Successor::None,
    }
}

/// Error replies to `adopted` that mean "running, but nothing adopted yet" (the review of
/// 8A, L1): the ctl server's word when the WM thread is busy (its launch scan, a slow app;
/// `main.rs`) and the engine's when it manages no window yet (Accessibility missing;
/// `controller.rs`, `not_managing`).
const NOTHING_ADOPTED: [&str; 2] =
    ["WM thread did not answer", "not managing windows yet (Accessibility missing or observation not started)"];

/// A reply to `adopted` as a [`Successor`]: the list it adopted; an earlier version's
/// "unknown command"; one of the [`NOTHING_ADOPTED`] errors. Anything else (another error, an
/// unreadable reply) is [`Successor::Busy`]: nothing moves, and [`BUSY_CAP`] bounds it.
fn successor_from_reply(reply: &str) -> Successor {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(reply) else { return Successor::Busy };
    let ok = v.get("ok").and_then(serde_json::Value::as_bool);
    if ok == Some(true)
        && let Some(list) = v.get("adopted").and_then(serde_json::Value::as_array)
    {
        return Successor::Adopted(list.iter().filter_map(|x| x.as_u64()).filter_map(|x| u32::try_from(x).ok()).collect());
    }
    match (ok, v.get("error").and_then(serde_json::Value::as_str)) {
        (Some(false), Some(e)) if e.starts_with("unknown command: adopted") => Successor::Older,
        (Some(false), Some(e)) if NOTHING_ADOPTED.contains(&e) => Successor::Adopted(Vec::new()),
        _ => Successor::Busy,
    }
}

/// Consecutive [`Successor::Busy`] answers (the review of 8A, M2).
#[derive(Debug, Default)]
struct BusyStreak {
    /// The first Busy answer of the streak.
    since: Option<Instant>,
    /// The cap was logged for this streak.
    capped: bool,
}

impl BusyStreak {
    /// The answer to act on. A Busy answer stays Busy until the streak has lasted
    /// [`BUSY_CAP`] and a waiting window `shows`; then it is `Adopted([])` (nothing adopted:
    /// the guardian brings the registered windows back; windows found in the park zone were
    /// let go at the first hand-over). Any other answer ends the streak.
    fn note(&mut self, answer: Successor, shows: bool, now: Instant) -> Successor {
        if answer != Successor::Busy {
            *self = Self::default();
            return answer;
        }
        let since = *self.since.get_or_insert(now);
        if !shows || now.duration_since(since) < BUSY_CAP {
            return Successor::Busy;
        }
        if !self.capped {
            self.capped = true;
            tracing::warn!(
                busy_s = now.duration_since(since).as_secs(),
                "guardian: Stage Manager runs again but has not answered for a long time; bringing the waiting windows back"
            );
        }
        Successor::Adopted(Vec::new())
    }
}

/// Every waiting window listed on screen, through Accessibility: one already outside the
/// park zone is let go; a registered one still parked goes back to its registered frame and
/// is read back; any other is the recovery scan's (`state.json`, else centered), once per
/// pass, then read back.
fn reach_shown(w: &mut Watch, displays: &ScanDisplays, attempts: &mut HashMap<u32, Attempts>, now: Instant) {
    let mut scanned: Vec<Waiting> = Vec::new();
    for x in w.shown() {
        let a = attempts.entry(x.id).or_default();
        if !a.due(now) {
            continue;
        }
        let element = match safety::ax_window(x.pid, x.id) {
            Ok(e) => e,
            Err(why) => {
                a.count += 1;
                a.last = Some(now);
                log_attempt(&x, a.count, &why);
                continue;
            }
        };
        match element.frame() {
            Ok(f) if !is_stranded(f, displays, x.known()) => {
                if let Some(e) = w.read_through_ax(x.id, f, false, displays) {
                    log_event(&e);
                    continue;
                }
                // A fitted window still at its fitted frame: moved to its pre-fit frame below.
                tracing::debug!(id = x.id, at = %f, "guardian: a fitted window; putting back its pre-fit frame");
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(id = x.id, "guardian: cannot read its frame ({e})"),
        }
        a.count += 1;
        a.last = Some(now);
        match Watch::restore_frame(&x, displays) {
            Some(frame) => match element.set_frame(frame) {
                Ok(()) => match element.frame() {
                    Ok(f) if !is_stranded(f, displays, x.known()) => {
                        tracing::warn!(id = x.id, pid = x.pid, to = %f, "guardian: its Desktop shows; window brought back");
                        if let Some(e) = w.read_through_ax(x.id, f, true, displays) {
                            log_event(&e);
                        }
                    }
                    Ok(f) => log_attempt(&x, a.count, &format!("it reads {f} after the move")),
                    Err(e) => log_attempt(&x, a.count, &format!("its frame cannot be read after the move ({e})")),
                },
                Err(e) => log_attempt(&x, a.count, &format!("it refused the move ({e})")),
            },
            None => scanned.push(x),
        }
    }
    if scanned.is_empty() {
        return;
    }
    let r = safety::recovery_scan("guardian: a Desktop with parked windows shows");
    tracing::info!(restored = r.restored, failed = r.failed, "guardian: scanned the shown Desktop");
    for x in scanned {
        match safety::ax_window(x.pid, x.id).and_then(|e| e.frame().map_err(|e| format!("its frame cannot be read ({e})"))) {
            Ok(f) if !is_stranded(f, displays, x.known()) => {
                if let Some(e) = w.read_through_ax(x.id, f, true, displays) {
                    log_event(&e);
                }
            }
            Ok(f) => log_attempt(&x, attempts.get(&x.id).map_or(1, |a| a.count), &format!("it reads {f} after the scan")),
            Err(why) => log_attempt(&x, attempts.get(&x.id).map_or(1, |a| a.count), &why),
        }
    }
}

/// A failed attempt at a shown window: loud at the first, the last fast one and every 20th.
fn log_attempt(x: &Waiting, attempt: u32, why: &str) {
    if attempt == 1 || attempt == FAST_ATTEMPTS || attempt.is_multiple_of(20) {
        tracing::warn!(id = x.id, pid = x.pid, attempt, "guardian: its Desktop shows, but {why}; retrying");
    } else {
        tracing::debug!(id = x.id, attempt, "guardian: {why}; retrying");
    }
}

/// Logs one change of the watch.
fn log_event(e: &Event) {
    match e {
        Event::ScreenChanged { on_screen } => {
            tracing::debug!(on_screen, "guardian: the windows on screen changed; listings are trusted again once that settles");
        }
        Event::Joined { id, pid, registered } => {
            let kind = if *registered { "registered" } else { "found in the park zone" };
            tracing::warn!(id, pid, "guardian: a window {kind} joins the watch");
        }
        Event::Looks { id, pid, look } => match look {
            Look::Parked { .. } | Look::OutShown { .. } => tracing::info!(id, pid, "guardian: window {look}"),
            Look::OutUnsettled { .. } | Look::OutAway { .. } | Look::Missing => {
                tracing::warn!(id, pid, "guardian: window {look}")
            }
        },
        Event::LetGo { id, pid, why } => tracing::warn!(id, pid, "guardian: let go of a window: {why}"),
        Event::Blind { listed } => tracing::warn!(
            listed,
            "guardian: the window server lists no window on screen (another user's session, the login window, or no answer); nothing counts until it does"
        ),
    }
}

/// Logs every waiting window: on a Desktop not shown (waiting for it to show), shown (being
/// moved back or checked), or not listed.
fn report(w: &Watch) {
    let pick = |f: fn(&Look) -> bool| -> Vec<String> {
        w.waiting().filter(|x| x.look.is_some_and(|l| f(&l))).map(describe).collect()
    };
    let away = pick(|l| !l.shown() && *l != Look::Missing);
    let here = pick(|l| l.shown());
    let missing = pick(|l| *l == Look::Missing);
    tracing::warn!(
        on_desktops_not_shown = ?away,
        shown_now = ?here,
        not_listed = ?missing,
        "guardian: waiting for {} window(s) not yet seen back; each comes back once its Desktop shows",
        w.waiting().count()
    );
}

fn describe(w: &Waiting) -> String {
    let bundle = safety::bundle_id_of(w.pid).unwrap_or_else(|| "?".into());
    let kind = if w.registered.is_some() { "registered" } else { "found in the park zone" };
    format!("{} (pid {} {bundle}, {kind})", w.id, w.pid)
}

/// The windows found in the park zone this pass (layer 0, off screen, of an allowed app,
/// never registered: those that would join, or wait, on suspicion alone) that are hidden tabs
/// (8J, [`hidden_tabs`]): their group's selected tab is listed through Accessibility (its
/// Desktop shows), registered, or saved in `state.json` this session (the crashed app
/// managed it). A registered window is never judged one.
fn hidden_tabs_found(w: &Watch, list: &[windows::CgWindow], listed: &[Listed], displays: &ScanDisplays) -> BTreeSet<u32> {
    let found: Vec<&windows::CgWindow> = list
        .iter()
        .filter(|c| {
            c.layer == 0
                && !c.onscreen
                && !w.is_registered(c.id)
                && listed.iter().any(|l| l.id == c.id && l.allowed)
                && is_stranded(c.bounds, displays, Known::Unknown)
        })
        .collect();
    if found.is_empty() {
        return BTreeSet::new();
    }
    let registered: BTreeSet<u32> = list.iter().map(|c| c.id).filter(|id| w.is_registered(*id)).collect();
    let ours = safety::Ours::from_parts(registered, BTreeSet::new(), safety::read_state("guardian"));
    let mut tabs = hidden_tabs::Tabs::new(list);
    found.into_iter().filter(|c| tabs.is_hidden_tab(c, ours.candidate(c.id), |id| ours.knows(id))).map(|c| c.id).collect()
}

/// The window server's layer-0 windows on every Space (of `list`), with whether their app
/// may be managed.
fn listed_windows(list: &[windows::CgWindow]) -> Vec<Listed> {
    let only = observe::only_from_env();
    let own = std::process::id() as i32;
    let mut allowed: HashMap<i32, bool> = HashMap::new();
    list.iter()
        .filter(|w| w.layer == 0)
        .map(|w| {
            let ok = *allowed
                .entry(w.pid)
                .or_insert_with(|| observe::app_allowed(only.as_ref(), own, w.pid, safety::bundle_id_of(w.pid).as_deref()));
            // A transparent window (a helper's) never joins the watch on its own.
            Listed { id: w.id, pid: w.pid, bounds: w.bounds, onscreen: w.onscreen, allowed: ok && w.alpha > 0.0 }
        })
        .collect()
}

extern "C" fn on_stop(sig: libc::c_int) {
    STOP_SIGNAL.store(sig, Ordering::SeqCst);
}

/// SIGTERM, SIGINT and SIGHUP stop the watch after one last scan (logged), instead of
/// killing the guardian silently.
fn install_stop_handlers() {
    let handler: extern "C" fn(libc::c_int) = on_stop;
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: a zeroed sigaction is a valid starting point; the handler only stores an
        // atomic, which is async-signal-safe.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler as usize;
        action.sa_flags = libc::SA_RESTART;
        // SAFETY: `action.sa_mask` is valid storage.
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        // SAFETY: valid signal number and action; the old action is not needed.
        if unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) } != 0 {
            tracing::warn!(sig, "guardian: cannot handle signal: {}", std::io::Error::last_os_error());
        }
    }
}

/// Blocks until `pid` exits (kqueue `EVFILT_PROC` / `NOTE_EXIT`); returns at once when it is
/// already gone. Falls back to polling when kqueue fails.
fn wait_for_exit(pid: i32) {
    // SAFETY: kqueue() takes no arguments.
    let kq = unsafe { libc::kqueue() };
    if kq >= 0 {
        // SAFETY: a zeroed kevent is valid; the fields are set below.
        let mut change: libc::kevent = unsafe { std::mem::zeroed() };
        change.ident = pid as usize;
        change.filter = libc::EVFILT_PROC;
        change.flags = libc::EV_ADD | libc::EV_ONESHOT;
        change.fflags = libc::NOTE_EXIT;
        // SAFETY: as above.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: one change in, room for one event out, no timeout.
            let n = unsafe { libc::kevent(kq, &change, 1, &mut event, 1, std::ptr::null()) };
            if n >= 0 {
                // A registration error comes back as an EV_ERROR event: ESRCH means the
                // process is already gone; anything else falls back to polling.
                let (flags, data) = (event.flags, event.data);
                if n == 1 && flags & libc::EV_ERROR != 0 && data != libc::ESRCH as isize {
                    tracing::warn!(error = data, "guardian: kevent registration failed; polling");
                    poll_for_exit(pid);
                }
                break;
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                if err.raw_os_error() != Some(libc::ESRCH) {
                    tracing::warn!("guardian: kevent failed ({err}); polling");
                    poll_for_exit(pid);
                }
                break;
            }
        }
        // SAFETY: closing our own descriptor.
        unsafe { libc::close(kq) };
    } else {
        poll_for_exit(pid);
    }
}

fn poll_for_exit(pid: i32) {
    // SAFETY: signal 0 only checks that the process exists.
    while unsafe { libc::kill(pid, 0) } == 0 {
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_apps_answer_says_what_it_adopted() {
        assert_eq!(successor_from_reply(r#"{"ok":true,"adopted":[4,9]}"#), Successor::Adopted(vec![4, 9]));
        assert_eq!(successor_from_reply(r#"{"ok":true,"adopted":[]}"#), Successor::Adopted(vec![]));
        assert_eq!(successor_from_reply(r#"{"ok":false,"error":"unknown command: adopted 4"}"#), Successor::Older);
        assert_eq!(
            successor_from_reply(r#"{"ok":false,"error":"WM thread did not answer"}"#),
            Successor::Adopted(vec![]),
            "busy: nothing adopted yet"
        );
        assert_eq!(successor_from_reply("garbage"), Successor::Busy);
    }

    /// The review of 8A, L1: only the known errors are answers; any other is Busy (bounded).
    #[test]
    fn only_known_errors_count_as_answers() {
        for e in NOTHING_ADOPTED {
            let reply = serde_json::json!({ "ok": false, "error": e }).to_string();
            assert_eq!(successor_from_reply(&reply), Successor::Adopted(vec![]), "{e}");
        }
        for reply in [
            r#"{"ok":false,"error":"not allowed"}"#,
            r#"{"ok":false,"error":"WM thread did not answer in time"}"#,
            r#"{"ok":false,"error":"unknown command: tile"}"#,
            r#"{"ok":false}"#,
            r#"{"ok":true}"#,
            r#"{"ok":false,"adopted":[4]}"#,
        ] {
            assert_eq!(successor_from_reply(reply), Successor::Busy, "{reply}");
        }
    }

    /// The review of 8A, M2: a successor busy for [`BUSY_CAP`] while a waiting window shows
    /// is taken as having adopted nothing; any answer starts the count again.
    #[test]
    fn a_hung_successor_is_capped() {
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        let mut b = BusyStreak::default();
        assert_eq!(b.note(Successor::Busy, true, s(0)), Successor::Busy);
        assert_eq!(b.note(Successor::Busy, true, s(29)), Successor::Busy);
        assert_eq!(b.note(Successor::Busy, true, s(30)), Successor::Adopted(vec![]));
        assert_eq!(b.note(Successor::Busy, false, s(31)), Successor::Busy, "nothing shows: nothing to move");
        assert_eq!(b.note(Successor::Busy, true, s(32)), Successor::Adopted(vec![]), "still the same streak");
        assert_eq!(b.note(Successor::Adopted(vec![4]), true, s(33)), Successor::Adopted(vec![4]));
        assert_eq!(b.note(Successor::Busy, true, s(34)), Successor::Busy, "a new streak");
        assert_eq!(b.note(Successor::None, true, s(40)), Successor::None);
        assert_eq!(b.note(Successor::Busy, true, s(70)), Successor::Busy, "nothing answered between: a new streak");
        // Busy while nothing showed counts: a window shown after the cap comes back at once.
        let mut b = BusyStreak::default();
        assert_eq!(b.note(Successor::Busy, false, s(0)), Successor::Busy);
        assert_eq!(b.note(Successor::Busy, true, s(45)), Successor::Adopted(vec![]));
    }

    #[test]
    fn retries_are_fast_then_slow() {
        let t0 = Instant::now();
        let mut a = Attempts::default();
        assert!(a.due(t0));
        a.count = FAST_ATTEMPTS;
        a.last = Some(t0);
        assert!(!a.due(t0 + Duration::from_secs(1)));
        assert!(a.due(t0 + SLOW_RETRY));
    }
}
