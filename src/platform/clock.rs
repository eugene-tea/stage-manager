//! The platform layer's one clock (docs/ARCHITECTURE.md, "Time"). Every instant the
//! controller, the observers, the input monitor, the WM thread and the engines read comes
//! from [`now`]: the real `Instant::now()` in a build, and under `cfg(test)` a per-thread
//! override a test can freeze and move ([`test::freeze`], [`test::advance`]), so a handler
//! judged "600 ms after the notice" can be driven without sleeping. A frozen clock never
//! ends a "read until the deadline" loop: a test freezes it around handlers only.

use std::time::Instant;

/// The time now: `Instant::now()`, or under `cfg(test)` the frozen instant of this thread
/// when a test froze it.
#[cfg(not(test))]
#[inline]
pub fn now() -> Instant {
    Instant::now()
}

/// The time now: `Instant::now()`, or under `cfg(test)` the frozen instant of this thread
/// when a test froze it.
#[cfg(test)]
pub fn now() -> Instant {
    test::frozen().unwrap_or_else(Instant::now)
}

/// The test clock: frozen per thread, moved by hand, restored when the guard drops.
#[cfg(test)]
pub mod test {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    thread_local! {
        static FROZEN: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    /// The frozen instant of this thread, if a test froze it.
    pub(super) fn frozen() -> Option<Instant> {
        FROZEN.with(Cell::get)
    }

    /// Restores the clock this thread had before [`freeze`] when dropped.
    #[must_use = "the clock is restored when the guard drops"]
    pub struct Frozen {
        before: Option<Instant>,
    }

    impl Drop for Frozen {
        fn drop(&mut self) {
            FROZEN.with(|f| f.set(self.before));
        }
    }

    /// Stops this thread's clock at `at` until the guard drops.
    pub fn freeze(at: Instant) -> Frozen {
        let before = FROZEN.with(|f| f.replace(Some(at)));
        Frozen { before }
    }

    /// Moves this thread's frozen clock on by `d`. Panics when the clock is not frozen: a
    /// test that moves time first freezes it, so the guard restores it.
    pub fn advance(d: Duration) {
        FROZEN.with(|f| {
            let at = f.get().expect("clock::test::advance needs clock::test::freeze first");
            f.set(Some(at + d));
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn frozen_clock_stands_moves_and_restores() {
            let at = Instant::now();
            {
                let _g = freeze(at);
                assert_eq!(super::super::now(), at);
                advance(Duration::from_millis(600));
                assert_eq!(super::super::now(), at + Duration::from_millis(600));
                {
                    let _inner = freeze(at);
                    assert_eq!(super::super::now(), at);
                }
                assert_eq!(super::super::now(), at + Duration::from_millis(600), "the inner guard restores the outer");
            }
            assert!(frozen().is_none(), "the outer guard restores the real clock");
        }
    }
}
