//! The last seconds of detailed log lines, kept in memory so a problem mark can include them
//! while the file stays at a moderate level.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Lines younger than `keep`, at most `max` of them.
#[derive(Debug)]
pub struct Ring {
    lines: VecDeque<(Instant, String)>,
    keep: Duration,
    max: usize,
}

impl Ring {
    /// An empty ring.
    pub fn new(keep: Duration, max: usize) -> Self {
        Self { lines: VecDeque::new(), keep, max: max.max(1) }
    }

    /// Adds a line seen at `at`, dropping lines too old or too many.
    pub fn push(&mut self, at: Instant, line: String) {
        self.lines.push_back((at, line));
        while self.lines.len() > self.max {
            self.lines.pop_front();
        }
        self.expire(at);
    }

    fn expire(&mut self, now: Instant) {
        while self.lines.front().is_some_and(|(t, _)| now.saturating_duration_since(*t) > self.keep) {
            self.lines.pop_front();
        }
    }

    /// The lines of the last `keep` before `now`, oldest first.
    pub fn recent(&mut self, now: Instant) -> Vec<String> {
        self.expire(now);
        self.lines.iter().map(|(_, l)| l.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_last_seconds_and_at_most_max_lines() {
        let t0 = Instant::now();
        let mut r = Ring::new(Duration::from_secs(10), 3);
        r.push(t0, "a".into());
        r.push(t0 + Duration::from_secs(5), "b".into());
        assert_eq!(r.recent(t0 + Duration::from_secs(9)), ["a", "b"]);
        assert_eq!(r.recent(t0 + Duration::from_secs(12)), ["b"]);
        for (i, s) in ["c", "d", "e"].into_iter().enumerate() {
            r.push(t0 + Duration::from_secs(13 + i as u64), s.into());
        }
        assert_eq!(r.recent(t0 + Duration::from_secs(15)), ["c", "d", "e"]);
        assert!(r.recent(t0 + Duration::from_secs(60)).is_empty());
    }
}
