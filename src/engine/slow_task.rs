//! Which slow tasks of the window thread get a log line (round plan-fixes): a task (a
//! command, a tick, an Accessibility callback) that held the thread at least [`SLOW`] gets
//! one DEBUG line with its name and how long it took, at most one per name per
//! [`LOG_APART`]; the lines held back meanwhile are counted into the next. After the 11-06
//! restarts, app activations waited up to 0.46 s in the window thread's queue (1.7 to 3.3 s
//! on that morning's builds) with nothing in the log naming what held the thread.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A task that held the window thread this long is logged.
pub const SLOW: Duration = Duration::from_millis(100);

/// One line per task name at most this often.
pub const LOG_APART: Duration = Duration::from_secs(1);

/// The last line per task name, and the slow runs held back since.
#[derive(Debug, Clone, Default)]
pub struct SlowTasks {
    last: HashMap<&'static str, (Instant, u32)>,
}

impl SlowTasks {
    /// Task `what` took `took`, ending at `now`. `Some(held)` when it gets a line (`held`:
    /// slow runs of it held back since its last line); `None` when it was quick, or a line
    /// for it went out less than [`LOG_APART`] ago (it is counted).
    pub fn note(&mut self, what: &'static str, took: Duration, now: Instant) -> Option<u32> {
        if took < SLOW {
            return None;
        }
        match self.last.get_mut(what) {
            Some((at, held)) if now.saturating_duration_since(*at) < LOG_APART => {
                *held += 1;
                None
            }
            Some((at, held)) => {
                let before = *held;
                *at = now;
                *held = 0;
                Some(before)
            }
            None => {
                self.last.insert(what, (now, 0));
                Some(0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quick_task_gets_no_line() {
        let mut s = SlowTasks::default();
        assert_eq!(s.note("tick", Duration::from_millis(99), Instant::now()), None);
    }

    #[test]
    fn a_slow_task_gets_one_line_a_second_with_the_ones_held_back() {
        let t0 = Instant::now();
        let mut s = SlowTasks::default();
        let slow = Duration::from_millis(150);
        assert_eq!(s.note("tick", slow, t0), Some(0));
        assert_eq!(s.note("tick", slow, t0 + Duration::from_millis(300)), None);
        assert_eq!(s.note("tick", slow, t0 + Duration::from_millis(600)), None);
        assert_eq!(s.note("command", slow, t0 + Duration::from_millis(600)), Some(0), "each name on its own");
        assert_eq!(s.note("tick", slow, t0 + Duration::from_millis(1000)), Some(2));
        assert_eq!(s.note("tick", slow, t0 + Duration::from_millis(2500)), Some(0));
    }
}
