//! A window closed while its app did not answer (round 8M), decided without touching anything.
//!
//! The owner's 10-21 18:34: Home (a Catalyst app that stops answering Accessibility) opened
//! window 35007 and he closed it; no destroyed notification came (every Accessibility call
//! answered `CannotComplete`). The model kept the window: its stage became a gray card with no
//! picture, and his click on it ran a switch whose moves all failed before the card went.
//!
//! The window server is the witness: its complete list (`CGWindowListCopyWindowInfo` with
//! `kCGWindowListOptionAll`) names every window that exists, on every Desktop, minimized ones
//! (ordered out, in the Dock), a hidden app's (ordered out) and a full-screen app's on its own
//! Space included; a window it no longer lists is closed. So a tracked window **missing from
//! that list on two looks at least [`LOOK_APART`] apart, listed on neither between** is closed
//! ([`Vanished::look`]), whatever Accessibility says, and goes the way of a destroyed
//! notification.
//!
//! Never on one look (a list read while a window is being replaced, say). A look whose list
//! shows no window on screen at all is **blind** (the window server answered nothing, or the
//! session is not the console's): it is no observation, and every streak starts again after
//! it. Only windows of apps still observed are looked at (an app that quit drops its windows
//! itself), and only windows tracked, which the window server listed when they were tracked,
//! so an app still launching (its windows not listed yet) is never judged.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Two looks missing a window at least this far apart close it.
pub const LOOK_APART: Duration = Duration::from_millis(500);

/// Tracked windows missing from the window server's complete list, with their first miss.
#[derive(Debug, Clone, Default)]
pub struct Vanished {
    missing: HashMap<u32, Instant>,
}

impl Vanished {
    /// One look at `now`: `tracked`, the windows watched (of apps still observed); `listed`, the
    /// ids of the window server's complete list; `blind`, that list shows no window on screen.
    /// Returns the windows closed (each once: it is forgotten here; the caller drops it).
    pub fn look(&mut self, now: Instant, tracked: impl IntoIterator<Item = u32>, listed: &HashSet<u32>, blind: bool) -> Vec<u32> {
        if blind {
            self.missing.clear();
            return Vec::new();
        }
        let mut closed = Vec::new();
        let mut still = HashMap::new();
        for id in tracked {
            if listed.contains(&id) {
                continue;
            }
            match self.missing.get(&id) {
                Some(&first) if now.saturating_duration_since(first) >= LOOK_APART => closed.push(id),
                Some(&first) => {
                    still.insert(id, first);
                }
                None => {
                    still.insert(id, now);
                }
            }
        }
        // Windows listed again, or no longer tracked (destroyed the usual way), start over.
        self.missing = still;
        closed.sort_unstable();
        closed
    }

    /// Windows missing from the latest look (for the ctl dump).
    pub fn missing(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.missing.keys().copied().collect();
        v.sort_unstable();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    fn set(ids: &[u32]) -> HashSet<u32> {
        ids.iter().copied().collect()
    }

    /// The 10-21 sequence: Home's window 35007 opens (listed), is closed with no destroyed
    /// notification (Accessibility answers nothing); the next look misses it, a look 250 ms
    /// later still waits, the one a second after the first miss closes it, once. Window 9 (on
    /// another Desktop, minimized or of a hidden app: still listed) never is.
    #[test]
    fn a_window_missing_on_two_looks_apart_is_closed_once() {
        let t0 = Instant::now();
        let mut v = Vanished::default();
        let tracked = [35007, 9];
        assert!(v.look(t0, tracked, &set(&[35007, 9, 4]), false).is_empty(), "open");
        let gone = set(&[9, 4]);
        assert!(v.look(ms(t0, 1000), tracked, &gone, false).is_empty(), "one missing look: never");
        assert!(v.look(ms(t0, 1250), tracked, &gone, false).is_empty(), "too soon after the first");
        assert_eq!(v.look(ms(t0, 2000), tracked, &gone, false), vec![35007], "closed");
        assert!(v.look(ms(t0, 3000), [9], &gone, false).is_empty(), "dropped by the caller: said once");
    }

    #[test]
    fn a_window_listed_again_between_two_misses_starts_over() {
        let t0 = Instant::now();
        let mut v = Vanished::default();
        assert!(v.look(t0, [1], &set(&[4]), false).is_empty());
        assert!(v.look(ms(t0, 600), [1], &set(&[1, 4]), false).is_empty(), "listed again");
        assert!(v.look(ms(t0, 1200), [1], &set(&[4]), false).is_empty(), "a new first miss");
        assert_eq!(v.look(ms(t0, 1800), [1], &set(&[4]), false), vec![1]);
    }

    /// A list that shows nothing on screen (another user's session, the login window, no
    /// answer) observes nothing: no window is closed by it, and a miss before it does not count.
    #[test]
    fn a_blind_look_closes_nothing_and_resets() {
        let t0 = Instant::now();
        let mut v = Vanished::default();
        assert!(v.look(t0, [1, 2], &set(&[2]), false).is_empty());
        assert!(v.look(ms(t0, 1000), [1, 2], &set(&[]), true).is_empty(), "blind");
        assert!(v.missing().is_empty());
        assert!(v.look(ms(t0, 2000), [1, 2], &set(&[2]), false).is_empty(), "the first miss again");
        assert_eq!(v.look(ms(t0, 3000), [1, 2], &set(&[2]), false), vec![1]);
    }

    #[test]
    fn a_window_no_longer_tracked_is_forgotten() {
        let t0 = Instant::now();
        let mut v = Vanished::default();
        v.look(t0, [1], &set(&[]), false);
        assert_eq!(v.missing(), vec![1]);
        assert!(v.look(ms(t0, 1000), [], &set(&[]), false).is_empty(), "destroyed the usual way meanwhile");
        assert!(v.missing().is_empty());
    }
}
