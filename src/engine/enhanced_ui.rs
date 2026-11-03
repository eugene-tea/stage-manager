//! Our moves never animate in an app whose `AXEnhancedUserInterface` is on (round
//! enhanced-ui).
//!
//! An assistive client (a screen reader, a window manager, a password manager, a dictation
//! tool) may set `AXEnhancedUserInterface` on an app's application element. With it on,
//! AppKit and Chromium animate a frame change made through Accessibility: a parked window
//! slides from the park corner to its frame and back (his 11-03: Chrome's window "sliding in
//! from the bottom right corner, not from the strip"), seen outside the switch's snapshot,
//! which covers only the final frame. Apple's Stage Manager is no Accessibility client and
//! never shows this. Rectangle and yabai turn the flag off around their moves and back on
//! after; so do we, by default and with no key: it fixes a bug.
//!
//! The [`Ledger`] decides; the platform (`platform::enhanced_ui`) reads and writes the flag
//! through [`Flags`] and keeps one ledger per process. The rules:
//!
//! - **Cheap read**: an app read as off is not asked again for [`FRESH`] (and is asked at its
//!   next move after it activates: [`Ledger::forget`]). An app read as on is asked again
//!   before each batch, so a flag its client turned off since is never turned back on by us.
//!   Per batch an app costs nothing (off, read lately), one read (off, or read long ago), or
//!   one read and two writes (on).
//! - **One off/on per app per batch**: a batch (a switch from its first park to its landing,
//!   a restore pass, an unpark) turns an app's flag off before its first move and back on when
//!   the batch ends, whatever the number of its windows. Batches nest: an inner one finds the
//!   flag already off and leaves its restore to the batch that turned it off ([`Owner`]).
//! - **Never left off**: a batch's end restores what it turned off ([`Ledger::end`]; the
//!   platform's guard runs it on drop, a panic's unwinding included). A restore that fails (a
//!   busy app) is kept and retried by [`Ledger::sweep`], backing off from [`RETRY_FIRST`] to
//!   [`RETRY_MAX`]; a flag held off longer than [`MAX_OFF`] (a batch that never ended) is
//!   restored by it too; quit, the panic hook and the signals restore everything
//!   ([`Ledger::restore_all`]). An app gone has nothing to restore.
//! - **An error is read back**: a write that answers with an error may still have taken
//!   (Chromium's application element acts on the value in its legacy setter, then hands it to
//!   AppKit's, whose answer is the one we get, plausibly an error as Electron's was for
//!   `AXManualAccessibility`: 11-03, -25208 NotImplemented at every write to Chrome's, in the
//!   same millisecond, so no timeout). The flag is read back; reading what was written,
//!   the write counts as done.
//! - **A refusal backs off**: an app whose flag would not turn off is neither read nor written
//!   again before [`RETRY_FIRST`], doubling to [`RETRY_MAX`] at each refusal in a row
//!   ([`Before::BackedOff`]); its moves meanwhile may animate. Not ended by its activation.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// How long an app read as off is taken to be off without asking again.
pub const FRESH: Duration = Duration::from_secs(3);

/// The longest an app's flag stays off: longer than any switch from its first park to its
/// landing (the unveil's backstop, 3 s at most, and the landing's margin). The sweep restores
/// a flag held off longer, whatever holds it.
pub const MAX_OFF: Duration = Duration::from_secs(4);

/// The first retry of a failed restore, after the failure; each later one waits twice as long.
pub const RETRY_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between retries of a failed restore (a hung app: one timeout this often).
pub const RETRY_MAX: Duration = Duration::from_secs(30);

/// Who turned a flag off: one per thread in the platform (the WM thread's batches nest).
pub type Owner = u64;

/// Why writing the flag failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// The app is gone (quit, or its element is invalid): there is nothing to restore.
    Gone,
    /// The app did not take it (busy, a timeout): try again later.
    Failed,
}

/// Reads and writes an app's `AXEnhancedUserInterface`.
pub trait Flags {
    /// The app's flag; `None` when it cannot be read (no such attribute, a timeout): taken as
    /// off.
    fn read(&mut self, pid: i32) -> Option<bool>;
    /// Sets the app's flag.
    fn write(&mut self, pid: i32, on: bool) -> Result<(), WriteError>;
}

/// What [`Ledger::before`] did about a move's app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Before {
    /// The flag is off (or unreadable): nothing to do.
    Nothing,
    /// We hold it off already (this batch, an outer one, another thread's, or a restore that
    /// waits for its retry).
    AlreadyOff,
    /// Found on and turned off now, until the batch ends; `first` the first time this app was
    /// found on (logged once).
    TurnedOff {
        /// The first time this app (pid) was found on.
        first: bool,
    },
    /// Found on, but it would not turn off: the move may animate. Not tried again for this
    /// app until its backoff is over ([`Before::BackedOff`]).
    Refused,
    /// Refused lately: neither read nor written until the backoff is over (from
    /// [`RETRY_FIRST`], doubling to [`RETRY_MAX`]); the move may animate.
    BackedOff,
}

/// What a restore did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Restored {
    /// Flags turned back on.
    pub on: usize,
    /// Apps gone: nothing to restore.
    pub gone: usize,
    /// Flags still off: retried by the sweep.
    pub kept: usize,
}

impl Restored {
    fn add(&mut self, o: Outcome) {
        match o {
            Outcome::On => self.on += 1,
            Outcome::Gone => self.gone += 1,
            Outcome::Kept => self.kept += 1,
        }
    }

    /// Nothing was touched.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    On,
    Gone,
    Kept,
}

/// An app's flag as last read.
#[derive(Clone, Copy, Debug)]
struct Seen {
    on: bool,
    at: Instant,
}

/// An app's flag we hold off.
#[derive(Clone, Copy, Debug)]
struct Off {
    /// The batch's owner that restores it at its end; `None` once a restore failed (the
    /// sweep's, or the next batch's that moves one of its windows).
    owner: Option<Owner>,
    /// When it was turned off.
    since: Instant,
    /// Failed restores so far, and when the last one was.
    failures: u32,
    last_try: Option<Instant>,
}

impl Off {
    fn retry_due(&self, now: Instant) -> bool {
        let wait = RETRY_FIRST.saturating_mul(1u32 << self.failures.saturating_sub(1).min(5)).min(RETRY_MAX);
        self.last_try.is_none_or(|t| now.saturating_duration_since(t) >= wait)
    }
}

/// An app whose flag would not turn off: not asked again before `until`.
#[derive(Clone, Copy, Debug)]
struct Refusal {
    times: u32,
    until: Instant,
}

/// How long after its `times`-th refusal in a row an app is left alone.
fn backoff(times: u32) -> Duration {
    RETRY_FIRST.saturating_mul(1u32 << times.saturating_sub(1).min(5)).min(RETRY_MAX)
}

/// The flags read and the flags held off, per app (pid).
#[derive(Debug, Default)]
pub struct Ledger {
    seen: BTreeMap<i32, Seen>,
    off: BTreeMap<i32, Off>,
    logged: BTreeSet<i32>,
    refused: BTreeMap<i32, Refusal>,
}

impl Ledger {
    /// An empty ledger.
    pub const fn new() -> Self {
        Self { seen: BTreeMap::new(), off: BTreeMap::new(), logged: BTreeSet::new(), refused: BTreeMap::new() }
    }

    /// Before a move of a window of app `pid` in `owner`'s batch: turns the app's flag off
    /// when it is on (reading it unless read off lately).
    pub fn before(&mut self, io: &mut impl Flags, pid: i32, owner: Owner, now: Instant) -> Before {
        if let Some(off) = self.off.get_mut(&pid) {
            if off.owner.is_none() {
                // A restore that failed: this batch's end tries again.
                off.owner = Some(owner);
            }
            return Before::AlreadyOff;
        }
        if self.seen.get(&pid).is_some_and(|s| !s.on && now.saturating_duration_since(s.at) < FRESH) {
            return Before::Nothing;
        }
        if self.refused.get(&pid).is_some_and(|r| now < r.until) {
            return Before::BackedOff;
        }
        let on = io.read(pid) == Some(true);
        self.seen.insert(pid, Seen { on, at: now });
        if !on {
            return Before::Nothing;
        }
        let took = match io.write(pid, false) {
            Ok(()) => true,
            Err(WriteError::Gone) => false,
            // An error is no proof it did not take: Chromium's setter acts on the value, then
            // hands it to AppKit's (11-03: -25208 NotImplemented at every write to Chrome's).
            // Read back, it tells.
            Err(WriteError::Failed) => io.read(pid) == Some(false),
        };
        if !took {
            let times = self.refused.get(&pid).map_or(1, |r| r.times.saturating_add(1));
            self.refused.insert(pid, Refusal { times, until: now + backoff(times) });
            return Before::Refused;
        }
        self.refused.remove(&pid);
        self.off.insert(pid, Off { owner: Some(owner), since: now, failures: 0, last_try: None });
        Before::TurnedOff { first: self.logged.insert(pid) }
    }

    /// `owner`'s batch ended: every flag it turned off goes back on.
    pub fn end(&mut self, io: &mut impl Flags, owner: Owner, now: Instant) -> Restored {
        let pids: Vec<i32> = self.off.iter().filter(|(_, o)| o.owner == Some(owner)).map(|(p, _)| *p).collect();
        let mut r = Restored::default();
        for pid in pids {
            r.add(self.restore(io, pid, now));
        }
        r
    }

    /// The periodic check: a failed restore is tried again when due, and a flag held off
    /// longer than [`MAX_OFF`] is restored whatever holds it.
    pub fn sweep(&mut self, io: &mut impl Flags, now: Instant) -> Restored {
        let pids: Vec<i32> = self
            .off
            .iter()
            .filter(|(_, o)| match o.owner {
                None => o.retry_due(now),
                Some(_) => now.saturating_duration_since(o.since) >= MAX_OFF,
            })
            .map(|(p, _)| *p)
            .collect();
        let mut r = Restored::default();
        for pid in pids {
            r.add(self.restore(io, pid, now));
        }
        r
    }

    /// Every flag held off goes back on (quit, a panic, a signal): one try each.
    pub fn restore_all(&mut self, io: &mut impl Flags, now: Instant) -> Restored {
        let pids: Vec<i32> = self.off.keys().copied().collect();
        let mut r = Restored::default();
        for pid in pids {
            r.add(self.restore(io, pid, now));
        }
        r
    }

    fn restore(&mut self, io: &mut impl Flags, pid: i32, now: Instant) -> Outcome {
        let written = match io.write(pid, true) {
            // Read back: it may have taken though it answered with an error (see `before`).
            Err(WriteError::Failed) if io.read(pid) == Some(true) => Ok(()),
            other => other,
        };
        match written {
            Ok(()) => {
                self.off.remove(&pid);
                self.seen.insert(pid, Seen { on: true, at: now });
                Outcome::On
            }
            Err(WriteError::Gone) => {
                self.gone(pid);
                Outcome::Gone
            }
            Err(WriteError::Failed) => {
                if let Some(o) = self.off.get_mut(&pid) {
                    o.owner = None;
                    o.failures = o.failures.saturating_add(1);
                    o.last_try = Some(now);
                }
                Outcome::Kept
            }
        }
    }

    /// App `pid` activated: its flag is read again at its next move (a client may have turned
    /// it on with the activation).
    pub fn forget(&mut self, pid: i32) {
        self.seen.remove(&pid);
    }

    /// App `pid` quit: nothing of it is kept.
    pub fn gone(&mut self, pid: i32) {
        self.seen.remove(&pid);
        self.off.remove(&pid);
        self.logged.remove(&pid);
        self.refused.remove(&pid);
    }

    /// The apps whose flag we hold off now.
    pub fn held(&self) -> Vec<i32> {
        self.off.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apps' flags in memory, with every call counted and failures on demand.
    #[derive(Default)]
    struct Fake {
        flags: BTreeMap<i32, bool>,
        reads: usize,
        writes: Vec<(i32, bool)>,
        refuse: BTreeSet<i32>,
        gone: BTreeSet<i32>,
        /// Takes the write but answers with an error (Chromium's setter).
        lie: BTreeSet<i32>,
    }

    impl Fake {
        fn with(apps: &[(i32, bool)]) -> Self {
            Self { flags: apps.iter().copied().collect(), ..Self::default() }
        }
    }

    impl Flags for Fake {
        fn read(&mut self, pid: i32) -> Option<bool> {
            self.reads += 1;
            self.flags.get(&pid).copied()
        }

        fn write(&mut self, pid: i32, on: bool) -> Result<(), WriteError> {
            if self.gone.contains(&pid) {
                return Err(WriteError::Gone);
            }
            if self.refuse.contains(&pid) {
                return Err(WriteError::Failed);
            }
            self.writes.push((pid, on));
            self.flags.insert(pid, on);
            if self.lie.contains(&pid) { Err(WriteError::Failed) } else { Ok(()) }
        }
    }

    const CHROME: i32 = 10;
    const SAFARI: i32 = 20;

    #[test]
    fn one_off_and_on_per_app_per_batch_whatever_its_windows() {
        let mut io = Fake::with(&[(CHROME, true), (SAFARI, false)]);
        let mut l = Ledger::new();
        let t = Instant::now();
        assert_eq!(l.before(&mut io, CHROME, 1, t), Before::TurnedOff { first: true });
        for _ in 0..4 {
            assert_eq!(l.before(&mut io, CHROME, 1, t), Before::AlreadyOff);
            assert_eq!(l.before(&mut io, SAFARI, 1, t), Before::Nothing);
        }
        assert!(!io.flags[&CHROME]);
        let r = l.end(&mut io, 1, t);
        assert_eq!(r, Restored { on: 1, ..Restored::default() });
        assert_eq!(io.writes, vec![(CHROME, false), (CHROME, true)]);
        assert!(io.flags[&CHROME]);
        assert!(l.held().is_empty());
        // Chrome read once, Safari once (then cached off).
        assert_eq!(io.reads, 2);
    }

    #[test]
    fn an_app_read_off_is_not_asked_again_until_stale_or_activated() {
        let mut io = Fake::with(&[(SAFARI, false)]);
        let mut l = Ledger::new();
        let t = Instant::now();
        l.before(&mut io, SAFARI, 1, t);
        l.before(&mut io, SAFARI, 1, t + Duration::from_secs(2));
        assert_eq!(io.reads, 1);
        l.before(&mut io, SAFARI, 1, t + FRESH);
        assert_eq!(io.reads, 2);
        l.forget(SAFARI);
        l.before(&mut io, SAFARI, 1, t + FRESH);
        assert_eq!(io.reads, 3);
        // Turned on by a client meanwhile: found at the next read.
        io.flags.insert(SAFARI, true);
        assert_eq!(l.before(&mut io, SAFARI, 1, t + FRESH * 2), Before::TurnedOff { first: true });
        l.end(&mut io, 1, t + FRESH * 2);
        // Logged once per app.
        assert_eq!(l.before(&mut io, SAFARI, 1, t + FRESH * 2), Before::TurnedOff { first: false });
    }

    #[test]
    fn an_app_read_on_is_read_again_and_a_flag_its_client_turned_off_stays_off() {
        let mut io = Fake::with(&[(CHROME, true)]);
        let mut l = Ledger::new();
        let t = Instant::now();
        l.before(&mut io, CHROME, 1, t);
        l.end(&mut io, 1, t);
        io.flags.insert(CHROME, false);
        assert_eq!(l.before(&mut io, CHROME, 1, t + Duration::from_millis(10)), Before::Nothing);
        l.end(&mut io, 1, t + Duration::from_millis(10));
        assert!(!io.flags[&CHROME]);
    }

    #[test]
    fn a_nested_or_other_owner_leaves_the_restore_to_the_one_that_turned_it_off() {
        let mut io = Fake::with(&[(CHROME, true)]);
        let mut l = Ledger::new();
        let t = Instant::now();
        l.before(&mut io, CHROME, 1, t);
        assert_eq!(l.before(&mut io, CHROME, 2, t), Before::AlreadyOff);
        assert!(l.end(&mut io, 2, t).is_empty());
        assert!(!io.flags[&CHROME]);
        l.end(&mut io, 1, t);
        assert!(io.flags[&CHROME]);
    }

    #[test]
    fn a_failed_restore_is_kept_and_retried_with_backoff_never_left_off() {
        let mut io = Fake::with(&[(CHROME, true)]);
        let mut l = Ledger::new();
        let t = Instant::now();
        l.before(&mut io, CHROME, 1, t);
        io.refuse.insert(CHROME);
        assert_eq!(l.end(&mut io, 1, t), Restored { kept: 1, ..Restored::default() });
        assert_eq!(l.held(), vec![CHROME]);
        // Not due yet; then due after a second; then after two.
        assert!(l.sweep(&mut io, t + Duration::from_millis(500)).is_empty());
        assert_eq!(l.sweep(&mut io, t + RETRY_FIRST).kept, 1);
        assert!(l.sweep(&mut io, t + RETRY_FIRST + Duration::from_millis(1500)).is_empty());
        io.refuse.clear();
        assert_eq!(l.sweep(&mut io, t + RETRY_FIRST * 3).on, 1);
        assert!(io.flags[&CHROME]);
        assert!(l.held().is_empty());
    }

    #[test]
    fn a_failed_restore_is_adopted_by_the_next_batch_that_moves_its_app() {
        let mut io = Fake::with(&[(CHROME, true)]);
        let mut l = Ledger::new();
        let t = Instant::now();
        l.before(&mut io, CHROME, 1, t);
        io.refuse.insert(CHROME);
        l.end(&mut io, 1, t);
        io.refuse.clear();
        assert_eq!(l.before(&mut io, CHROME, 1, t), Before::AlreadyOff);
        assert_eq!(l.end(&mut io, 1, t).on, 1);
        assert!(io.flags[&CHROME]);
    }

    #[test]
    fn a_batch_that_never_ends_is_restored_by_the_sweep() {
        let mut io = Fake::with(&[(CHROME, true)]);
        let mut l = Ledger::new();
        let t = Instant::now();
        l.before(&mut io, CHROME, 1, t);
        assert!(l.sweep(&mut io, t + MAX_OFF - Duration::from_millis(1)).is_empty());
        assert_eq!(l.sweep(&mut io, t + MAX_OFF).on, 1);
        assert!(io.flags[&CHROME]);
        // The batch's end later finds nothing left.
        assert!(l.end(&mut io, 1, t + MAX_OFF).is_empty());
    }

    #[test]
    fn restore_all_turns_every_held_flag_on_and_drops_gone_apps() {
        let mut io = Fake::with(&[(CHROME, true), (SAFARI, true)]);
        let mut l = Ledger::new();
        let t = Instant::now();
        l.before(&mut io, CHROME, 1, t);
        l.before(&mut io, SAFARI, 2, t);
        io.gone.insert(SAFARI);
        let r = l.restore_all(&mut io, t);
        assert_eq!(r, Restored { on: 1, gone: 1, kept: 0 });
        assert!(l.held().is_empty());
        assert!(io.flags[&CHROME]);
    }

    #[test]
    fn a_refused_app_is_left_alone_for_a_while_not_asked_at_every_move() {
        let mut io = Fake::with(&[(CHROME, true)]);
        io.refuse.insert(CHROME);
        let mut l = Ledger::new();
        let t = Instant::now();
        assert_eq!(l.before(&mut io, CHROME, 1, t), Before::Refused);
        let asked = io.reads;
        // Every move in the next second: nothing asked, nothing written.
        for n in 1..10 {
            assert_eq!(l.before(&mut io, CHROME, 1, t + Duration::from_millis(100 * n)), Before::BackedOff);
        }
        assert_eq!(io.reads, asked);
        // An activation does not end the backoff (Chrome activated at every switch, 11-03).
        l.forget(CHROME);
        assert_eq!(l.before(&mut io, CHROME, 1, t + Duration::from_millis(999)), Before::BackedOff);
        // Tried again after a second, then after two more, doubling.
        assert_eq!(l.before(&mut io, CHROME, 1, t + RETRY_FIRST), Before::Refused);
        assert_eq!(l.before(&mut io, CHROME, 1, t + RETRY_FIRST * 2), Before::BackedOff);
        assert_eq!(l.before(&mut io, CHROME, 1, t + RETRY_FIRST * 3), Before::Refused);
        // Once it takes, the backoff is forgotten.
        io.refuse.clear();
        assert_eq!(l.before(&mut io, CHROME, 1, t + RETRY_FIRST * 7), Before::TurnedOff { first: true });
        l.end(&mut io, 1, t + RETRY_FIRST * 7);
        assert!(io.flags[&CHROME]);
        assert!(l.refused.is_empty());
    }

    #[test]
    fn a_write_that_took_but_answered_an_error_counts_as_done() {
        // Chromium acts on the value and answers with its superclass's error.
        let mut io = Fake::with(&[(CHROME, true)]);
        io.lie.insert(CHROME);
        let mut l = Ledger::new();
        let t = Instant::now();
        assert_eq!(l.before(&mut io, CHROME, 1, t), Before::TurnedOff { first: true });
        assert_eq!(l.held(), vec![CHROME]);
        assert_eq!(l.end(&mut io, 1, t), Restored { on: 1, ..Restored::default() });
        assert!(io.flags[&CHROME]);
        assert!(l.held().is_empty());
    }

    #[test]
    fn an_unreadable_or_refused_flag_changes_nothing() {
        let mut io = Fake::default();
        let mut l = Ledger::new();
        let t = Instant::now();
        assert_eq!(l.before(&mut io, CHROME, 1, t), Before::Nothing);
        io.flags.insert(SAFARI, true);
        io.refuse.insert(SAFARI);
        assert_eq!(l.before(&mut io, SAFARI, 1, t), Before::Refused);
        assert!(l.held().is_empty());
        assert!(l.end(&mut io, 1, t).is_empty());
        assert!(io.flags[&SAFARI]);
    }
}
