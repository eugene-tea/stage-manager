//! Moves made with an app's `AXEnhancedUserInterface` off for a moment, so that they never
//! animate (round enhanced-ui; the rules are [`crate::engine::enhanced_ui`]'s).
//!
//! Every frame write goes through here: [`AxElement::set_frame`], [`AxElement::set_position`]
//! and [`AxElement::set_size`] call [`around`], so no writer can miss it (the switch's parks
//! and unparks, the restore, the fit, the guardian's and the safety paths' unparks, the
//! arrival's arranging, the hide's moves, the probes). Alone, a write is its own batch (one
//! off/on). A writer moving several windows opens a [`Batch`] around them: every app is
//! turned off once before its first move and on again when the outermost batch on the thread
//! ends (batches nest per thread). A switch's batch lasts from its first park to its
//! landing: the controller keeps it with the held arrivals.
//!
//! Restored on every path: the batch's drop (a panic's unwinding included), the WM tick's
//! [`sweep`] (a failed restore, a flag off longer than [`crate::engine::enhanced_ui::MAX_OFF`]),
//! and [`restore_all`] at quit, in the panic hook, on SIGABRT and when the guardian ends. A
//! crash with no unwinding (SIGKILL) between an off and its on leaves the flag off until its
//! client sets it again; the window for that is the length of a batch.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use super::ax::{AxElement, AxError};
use crate::engine::enhanced_ui::{Before, Flags, Ledger, Owner, Restored, WriteError};

/// This process's ledger.
static LEDGER: Mutex<Ledger> = Mutex::new(Ledger::new());

static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Open batches on this thread.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    /// This thread's owner token.
    static OWNER: Owner = NEXT_OWNER.fetch_add(1, Ordering::Relaxed);
}

/// How long [`restore_all`] waits for the ledger when another thread holds it (in the panic
/// hook the panicking thread itself may).
const RESTORE_ALL_WAIT: Duration = Duration::from_millis(300);

/// The read's messaging timeout: an app answers it at once, and a busy one must not hold a
/// switch up for the global timeout (it is taken as off: its move may animate).
const READ_TIMEOUT_SECONDS: f32 = 0.1;

fn lock() -> MutexGuard<'static, Ledger> {
    LEDGER.lock().unwrap_or_else(|p| p.into_inner())
}

/// The flag through Accessibility, on the app's application element.
struct AxFlags;

impl Flags for AxFlags {
    fn read(&mut self, pid: i32) -> Option<bool> {
        if pid == std::process::id() as i32 {
            return None;
        }
        AxElement::application(pid).bounded(READ_TIMEOUT_SECONDS, |app| app.enhanced_user_interface()).ok()
    }

    fn write(&mut self, pid: i32, on: bool) -> Result<(), WriteError> {
        match AxElement::application(pid).set_enhanced_user_interface(on) {
            Ok(()) => Ok(()),
            Err(e) if e == AxError::INVALID_ELEMENT || e == AxError::ATTRIBUTE_UNSUPPORTED || !alive(pid) => {
                Err(WriteError::Gone)
            }
            Err(e) => {
                tracing::debug!(pid, on, "enhanced UI: cannot set it ({e})");
                Err(WriteError::Failed)
            }
        }
    }
}

/// The process still runs.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A group of moves done with each app's flag off once (see the module docs). Ends on drop,
/// on the thread that opened it.
pub struct Batch {
    /// Bound to its thread: the depth and the owner are per thread.
    _thread: PhantomData<*const ()>,
}

impl Batch {
    /// Opens a batch on this thread.
    pub fn open() -> Self {
        DEPTH.with(|d| d.set(d.get() + 1));
        Self { _thread: PhantomData }
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        let left = DEPTH.with(|d| {
            let n = d.get().saturating_sub(1);
            d.set(n);
            n
        });
        if left > 0 {
            return;
        }
        let owner = OWNER.with(|o| *o);
        let r = lock().end(&mut AxFlags, owner, Instant::now());
        if r.kept > 0 {
            tracing::warn!(kept = r.kept, "enhanced UI: an app did not take it back on; retried");
        }
    }
}

impl std::fmt::Debug for Batch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("enhanced_ui::Batch")
    }
}

/// Runs `write` (a frame write to `window`) with its app's flag off: inside the batch open on
/// this thread, else in one of its own.
pub fn around<T>(window: &AxElement, write: impl FnOnce() -> T) -> T {
    let _batch = Batch::open();
    before(window);
    write()
}

/// Turns the flag of `window`'s app off for the batch, when it is on.
fn before(window: &AxElement) {
    let Ok(pid) = window.pid() else { return };
    let owner = OWNER.with(|o| *o);
    let done = lock().before(&mut AxFlags, pid, owner, Instant::now());
    match done {
        Before::TurnedOff { first: true } => {
            let app = super::safety::bundle_id_of(pid).unwrap_or_default();
            tracing::info!(pid, app, "enhanced UI on: moves done with it off for a moment");
        }
        Before::Refused => tracing::debug!(pid, "enhanced UI on, and it would not turn off: this move may animate"),
        _ => {}
    }
}

/// The periodic check (the WM tick, the guardian's pass): failed restores again, and a flag
/// held off too long.
pub fn sweep() {
    let r = lock().sweep(&mut AxFlags, Instant::now());
    if !r.is_empty() {
        tracing::info!(on = r.on, gone = r.gone, kept = r.kept, "enhanced UI: flags held off put back");
    }
}

/// App `pid` activated: its flag is read again at its next move.
pub fn forget(pid: i32) {
    lock().forget(pid);
}

/// App `pid` quit.
pub fn gone(pid: i32) {
    lock().gone(pid);
}

/// Every flag held off goes back on (quit, a panic, a signal, the guardian's end). Waits a
/// bounded moment for the ledger, never deadlocks on it.
pub fn restore_all(reason: &str) {
    let end = Instant::now() + RESTORE_ALL_WAIT;
    let mut ledger = loop {
        match LEDGER.try_lock() {
            Ok(g) => break g,
            Err(TryLockError::Poisoned(p)) => break p.into_inner(),
            Err(TryLockError::WouldBlock) if Instant::now() < end => std::thread::sleep(Duration::from_millis(5)),
            Err(TryLockError::WouldBlock) => {
                tracing::warn!(reason, "enhanced UI: ledger busy; flags held off (if any) not put back");
                return;
            }
        }
    };
    let r: Restored = ledger.restore_all(&mut AxFlags, Instant::now());
    if !r.is_empty() {
        tracing::warn!(reason, on = r.on, gone = r.gone, kept = r.kept, "enhanced UI: flags held off put back");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depth() -> usize {
        DEPTH.with(Cell::get)
    }

    #[test]
    fn a_panic_inside_nested_batches_ends_them_all() {
        let r = std::panic::catch_unwind(|| {
            let _outer = Batch::open();
            let _inner = Batch::open();
            assert_eq!(depth(), 2);
            panic!("a pass panics with the flag off");
        });
        assert!(r.is_err());
        assert_eq!(depth(), 0);
    }

    #[test]
    fn a_kept_batch_outlives_the_one_that_opened_the_switch() {
        // The switch's batch, then the held arrivals' (kept in `Pending`) past its end.
        let switch = Batch::open();
        let held = Batch::open();
        drop(switch);
        assert_eq!(depth(), 1);
        drop(held);
        assert_eq!(depth(), 0);
    }
}
