//! Asking apps that do not answer Accessibility yet, each on a thread of its own, never on the
//! WM thread (round login-observe, [`crate::engine::observe_retry`]).
//!
//! At the 11-06 login a dozen apps did not answer for seconds; trying each on the WM thread
//! cost it 0.75 s an app, one after another. Now the [`Observer`](super::observe::Observer)
//! hands such an app to [`Probes::start`]: a thread asks it one cheap question (its
//! application element's `AXRole`) on the schedule of [`Retry`], and once it answers sends
//! [`Note::Answered`] to the WM thread, which observes it then (a registration that answers
//! at once). One app that does not answer waits on its own thread only: it delays no other
//! app, no window found and no notice.
//!
//! [`ask_once`] asks several apps at once and waits a bounded time: observation's start uses
//! it, so the apps that answer are observed before it reports, as before, and the rest go to
//! [`Probes::start`].
//!
//! Accessibility is asked from a helper thread as Finder's desktop icons are
//! ([`super::desktop_items`]): a read, at the messaging timeout the WM thread set for the
//! process. Nothing is moved or registered off the WM thread.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::ax::AxElement;
use crate::engine::observe_retry::{Next, Retry};

/// What one ask of an app found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// It answered Accessibility.
    Answers,
    /// No answer yet (a timeout, or Accessibility not up in it yet).
    NotYet,
    /// Its process is gone.
    Gone,
}

/// The question a probe asks of app `pid` (a function, so tests can stand in for the app).
pub type Ask = fn(i32) -> Answer;

/// The real question: does the app's application element answer `AXRole`? Any answer means
/// its Accessibility is up; any error (a timeout above all) is not yet.
pub fn ask_ax(pid: i32) -> Answer {
    if process_gone(pid) {
        return Answer::Gone;
    }
    match AxElement::application(pid).role() {
        Ok(_) => Answer::Answers,
        Err(_) => Answer::NotYet,
    }
}

/// What a probe tells the WM thread, once, when it stops. `probe` names the probe that sent
/// it ([`Probes::current`]): a note of a probe stopped before it came (observation stopped and
/// started again, a cancelled logout) is not the running probe's word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Note {
    /// App `pid` answers now: observe it. `retry` is its schedule so far.
    Answered { pid: i32, probe: u64, retry: Retry },
    /// App `pid` did not answer for [`crate::engine::observe_retry::GIVE_UP_AFTER`].
    GaveUp { pid: i32, probe: u64, retry: Retry },
    /// App `pid`'s process is gone.
    Gone { pid: i32, probe: u64 },
}

impl Note {
    /// The app the note is about.
    pub fn pid(&self) -> i32 {
        match *self {
            Note::Answered { pid, .. } | Note::GaveUp { pid, .. } | Note::Gone { pid, .. } => pid,
        }
    }

    /// The probe that sent it.
    pub fn probe(&self) -> u64 {
        match *self {
            Note::Answered { probe, .. } | Note::GaveUp { probe, .. } | Note::Gone { probe, .. } => probe,
        }
    }
}

/// Where probes send their [`Note`]s (the WM thread's inbox).
pub type Notify = Arc<dyn Fn(Note) + Send + Sync>;

/// The probes running, one per app that does not answer yet.
pub struct Probes {
    ask: Ask,
    /// The fast cadence (the login settles), shared with every probe.
    fast: Arc<AtomicBool>,
    notify: Option<Notify>,
    /// Each running probe's number and stop flag.
    running: HashMap<i32, (u64, Arc<AtomicBool>)>,
    /// The last probe's number: each probe started gets the next.
    last: u64,
}

impl Probes {
    /// Probes asking `ask`; the fast cadence on until told otherwise (observation starts as
    /// the login settles, the restore window opening a moment after).
    pub fn new(ask: Ask) -> Self {
        Self { ask, fast: Arc::new(AtomicBool::new(true)), notify: None, running: HashMap::new(), last: 0 }
    }

    /// Where notes go; until set, probes are not started (the caller retries on its own).
    pub fn set_notify(&mut self, notify: Notify) {
        self.notify = Some(notify);
    }

    /// The question asked.
    pub fn ask(&self) -> Ask {
        self.ask
    }

    /// The fast cadence on or off; true when it changed.
    pub fn set_fast(&self, fast: bool) -> bool {
        self.fast.swap(fast, Ordering::Relaxed) != fast
    }

    /// True while the fast cadence is on.
    pub fn fast(&self) -> bool {
        self.fast.load(Ordering::Relaxed)
    }

    /// True while a probe asks app `pid`.
    pub fn running(&self, pid: i32) -> bool {
        self.running.contains_key(&pid)
    }

    /// Whether `note` comes from the probe running for its app: false for one stopped before
    /// its note came (the 1.7 review: an old probe's GaveUp, after observation started again,
    /// stopped the fresh probe and gave the app up until it was activated).
    pub fn current(&self, note: &Note) -> bool {
        self.running.get(&note.pid()).is_some_and(|(probe, _)| *probe == note.probe())
    }

    /// Test only: the number of the probe asking `pid`.
    #[cfg(test)]
    pub fn probe_of(&self, pid: i32) -> Option<u64> {
        self.running.get(&pid).map(|(probe, _)| *probe)
    }

    /// Starts a probe of app `pid` keeping `retry`, first asking at `due`. False when none
    /// could start (no notify set, or the thread did not spawn); true when one runs (a
    /// probe already running keeps its own schedule).
    pub fn start(&mut self, pid: i32, retry: Retry, due: Instant) -> bool {
        if self.running.contains_key(&pid) {
            return true;
        }
        let Some(notify) = self.notify.clone() else { return false };
        let stop = Arc::new(AtomicBool::new(false));
        let id = self.last + 1;
        let (ask, fast, flag) = (self.ask, self.fast.clone(), stop.clone());
        let spawned = std::thread::Builder::new().name("observe-probe".into()).spawn(move || {
            let note = super::safety::guarded("an observe probe", || probe(pid, id, retry, due, ask, &fast, &flag));
            match note {
                Some(Some(note)) => notify(note),
                Some(None) => {}
                // A panic: given up, so the WM thread forgets it (an activation tries again).
                None => notify(Note::GaveUp { pid, probe: id, retry }),
            }
        });
        match spawned {
            Ok(_) => {
                self.last = id;
                self.running.insert(pid, (id, stop));
                true
            }
            Err(e) => {
                tracing::warn!(pid, "cannot start a thread to ask an app that does not answer yet: {e}");
                false
            }
        }
    }

    /// The probe of `pid` stopped (its note came) or must stop (its app quit, or was observed).
    pub fn stop(&mut self, pid: i32) {
        if let Some((_, flag)) = self.running.remove(&pid) {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Every probe stops (observation stopped).
    pub fn stop_all(&mut self) {
        for (_, (_, flag)) in self.running.drain() {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

/// The longest a probe sleeps between looks at its stop flag.
const NAP: Duration = Duration::from_millis(50);

/// One probe's life (probe number `id`): asks `pid` at `due`, then on `retry`'s schedule,
/// until it answers, its process goes, the schedule gives up, or `stop` is set (no note then,
/// also when it is set during an ask).
fn probe(pid: i32, id: u64, mut retry: Retry, mut due: Instant, ask: Ask, fast: &AtomicBool, stop: &AtomicBool) -> Option<Note> {
    loop {
        loop {
            if stop.load(Ordering::Relaxed) {
                return None;
            }
            let now = Instant::now();
            if now >= due {
                break;
            }
            std::thread::sleep((due - now).min(NAP));
        }
        let answer = ask(pid);
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        match answer {
            Answer::Answers => return Some(Note::Answered { pid, probe: id, retry }),
            Answer::Gone => return Some(Note::Gone { pid, probe: id }),
            Answer::NotYet => match retry.missed(Instant::now(), fast.load(Ordering::Relaxed)) {
                Next::At(at) => due = at,
                Next::GiveUp => return Some(Note::GaveUp { pid, probe: id, retry }),
            },
        }
    }
}

/// Asks every app of `pids` once, all at the same time, each on a thread of its own, and
/// waits at most `wait`: the apps that answered within it. One that answers late is not in it.
pub fn ask_once(pids: &[i32], ask: Ask, wait: Duration) -> HashSet<i32> {
    let (tx, rx) = mpsc::channel();
    let mut asked = 0usize;
    for &pid in pids {
        let tx = tx.clone();
        let spawned = std::thread::Builder::new().name("observe-ask".into()).spawn(move || {
            let answer = super::safety::guarded("an observe ask", || ask(pid)).unwrap_or(Answer::NotYet);
            let _ = tx.send((pid, answer));
        });
        if spawned.is_ok() {
            asked += 1;
        }
    }
    drop(tx);
    let deadline = Instant::now() + wait;
    let mut answered = HashSet::new();
    for _ in 0..asked {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else { break };
        match rx.recv_timeout(left) {
            Ok((pid, Answer::Answers)) => {
                answered.insert(pid);
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    answered
}

/// True when process `pid` no longer exists ("not permitted" means it exists).
fn process_gone(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists; nothing is sent.
    let r = unsafe { libc::kill(pid, 0) };
    r == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::observe_retry::LAUNCH_EVERY;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU32;

    /// The stuck app: every ask waits out the 0.25 s timeout and never answers.
    const STUCK: i32 = 4001;
    /// The app that answers on its third ask.
    const LATE: i32 = 4002;
    static LATE_ASKS: AtomicU32 = AtomicU32::new(0);

    fn fake(pid: i32) -> Answer {
        match pid {
            STUCK => {
                std::thread::sleep(Duration::from_millis(250));
                Answer::NotYet
            }
            LATE => {
                if LATE_ASKS.fetch_add(1, Ordering::SeqCst) >= 2 { Answer::Answers } else { Answer::NotYet }
            }
            _ => Answer::Answers,
        }
    }

    /// The notes a test's probes sent, each with when it came.
    type Notes = Arc<Mutex<Vec<(Note, Instant)>>>;

    fn collecting() -> (Notify, Notes) {
        let notes = Arc::new(Mutex::new(Vec::new()));
        let sink = notes.clone();
        let notify: Notify = Arc::new(move |n| sink.lock().unwrap_or_else(|e| e.into_inner()).push((n, Instant::now())));
        (notify, notes)
    }

    #[test]
    fn a_stuck_apps_retries_do_not_hold_up_another_app() {
        let (notify, notes) = collecting();
        let mut probes = Probes::new(fake);
        probes.set_notify(notify);
        let t0 = Instant::now();
        // The stuck one first, as at the 11-06 login (Mail's asks before Slack's).
        assert!(probes.start(STUCK, Retry::new(t0), t0));
        assert!(probes.start(LATE, Retry::new(t0), t0));
        let started = t0.elapsed();
        assert!(started < Duration::from_millis(50), "starting probes waits for no app ({started:?})");
        let deadline = t0 + Duration::from_secs(3);
        let answered = loop {
            let found = notes.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(n, _)| n.pid() == LATE).copied();
            if let Some(found) = found {
                break found;
            }
            assert!(Instant::now() < deadline, "the late app's answer came");
            std::thread::sleep(Duration::from_millis(5));
        };
        let (note, at) = answered;
        assert!(matches!(note, Note::Answered { pid: LATE, retry, .. } if retry.misses() == 2));
        // Two misses at a just-opened app's cadence: about 2 × 40 ms, never behind the stuck
        // app's 250 ms waits (it would be 750 ms and more if they took turns).
        let took = at.duration_since(t0);
        assert!(took >= LAUNCH_EVERY * 2, "asked on the schedule ({took:?})");
        assert!(took < Duration::from_millis(600), "not behind the stuck app ({took:?})");
        assert!(probes.running(STUCK));
        probes.stop(STUCK);
        probes.stop(LATE);
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            notes.lock().unwrap_or_else(|e| e.into_inner()).iter().all(|(n, _)| n.pid() != STUCK),
            "a stopped probe sends nothing"
        );
    }

    /// The 1.7 review: a probe stopped before its note came (observation stopped and started
    /// again) is not the running probe; a new probe of the same app is.
    #[test]
    fn only_the_running_probes_note_is_current() {
        let (notify, _notes) = collecting();
        let mut probes = Probes::new(fake);
        probes.set_notify(notify);
        let t0 = Instant::now();
        assert!(probes.start(STUCK, Retry::new(t0), t0));
        let old = probes.running[&STUCK].0;
        let old_note = Note::GaveUp { pid: STUCK, probe: old, retry: Retry::new(t0) };
        assert!(probes.current(&old_note));
        probes.stop_all();
        assert!(!probes.current(&old_note), "stopped: no probe runs");
        assert!(probes.start(STUCK, Retry::new(t0), t0));
        assert!(!probes.current(&old_note), "the old probe's note is not the new one's");
        let new = probes.running[&STUCK].0;
        assert!(probes.current(&Note::GaveUp { pid: STUCK, probe: new, retry: Retry::new(t0) }));
        probes.stop_all();
    }

    #[test]
    fn no_probe_starts_without_an_inbox() {
        let mut probes = Probes::new(fake);
        let now = Instant::now();
        assert!(!probes.start(LATE, Retry::new(now), now));
        assert!(!probes.running(LATE));
    }

    /// Like [`fake`], the stuck app hanging a second: the bound below keeps room for a slow,
    /// shared CI machine (a 250 ms hang left 120 ms and failed there).
    fn fake_hanging(pid: i32) -> Answer {
        if pid == STUCK {
            std::thread::sleep(Duration::from_millis(1000));
            return Answer::NotYet;
        }
        fake(pid)
    }

    #[test]
    fn asking_at_once_waits_a_bounded_time_and_keeps_who_answered() {
        let t0 = Instant::now();
        let answered = ask_once(&[STUCK, 4003, 4004], fake_hanging, Duration::from_millis(120));
        let took = t0.elapsed();
        assert_eq!(answered, [4003, 4004].into_iter().collect());
        assert!(took < Duration::from_millis(600), "the stuck app's hang is not waited out ({took:?})");
    }

    #[test]
    fn the_cadence_flag_reports_its_changes() {
        let probes = Probes::new(fake);
        assert!(probes.fast(), "fast as observation starts");
        assert!(!probes.set_fast(true));
        assert!(probes.set_fast(false));
        assert!(!probes.fast());
    }
}
