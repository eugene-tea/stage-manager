//! When to look for Mission Control before the poll does, for an engine whose hidden windows
//! stay on their Desktop (round looks-forward; the Corner engine).
//!
//! The Spaces engine follows Mission Control for its lends
//! ([`super::mission_control::Lends`]), and its hints bring the looks forward there. An
//! engine with nothing to lend still leans on knowing that Mission Control shows (the Corner
//! engine's shield, park reconcile, stills and patch all stop for it), so the same hints
//! ([`Hint`]: its key, a notice of the Dock's, the window server's notice) bring its look
//! forward too. A hint is never a fact: it makes the window list be looked at now and, for a
//! moment ([`Hint::watch`]), at the fast tick, every [`WATCH_EVERY`]; what the look finds
//! decides, and the poll's look stays behind it.
//!
//! **Its close** ([`LookWatch::close_hint`]): while it shows, a key that closes it or a press
//! makes the looks come at the fast tick until it is seen closed, [`OVERVIEW_EXIT_CARRY`] at
//! most (the Dock's cover goes about 0.6 s after the key).

use std::time::{Duration, Instant};

use super::mission_control::{HINT_COUNTS, Hint, WATCH_EVERY};
use super::overview::OVERVIEW_EXIT_CARRY;

/// The looks a hint brings ([module docs](self)).
#[derive(Debug, Clone, Copy, Default)]
pub struct LookWatch {
    /// The latest hint of an opening, until Mission Control shows or its watch is over.
    hint: Option<(Hint, Instant)>,
    /// When something that may close it was last heard while it showed.
    closing: Option<Instant>,
    /// The watch's last look.
    looked: Option<Instant>,
}

impl LookWatch {
    /// A hint at `now` that Mission Control may be about to show. True when it is taken (the
    /// caller looks now, and the fast tick for its watch); false while a key's watch stands:
    /// a key is not replaced by the notices that follow it, nor by its own repeats.
    pub fn hint(&mut self, hint: Hint, now: Instant) -> bool {
        if self.hint.is_some_and(|(h, t)| matches!(h, Hint::Key(_)) && now.saturating_duration_since(t) < h.watch()) {
            return false;
        }
        self.hint = Some((hint, now));
        true
    }

    /// Something that may close the overview shown was heard at `now` (its key, Escape, a
    /// press): its close is looked for at the fast tick from now.
    pub fn close_hint(&mut self, now: Instant) {
        self.closing = Some(now);
    }

    /// True while a watch lasts at `now`, `showing` Mission Control or not: an opening's
    /// while none shows, a close's while one does.
    pub fn watching(&self, showing: bool, now: Instant) -> bool {
        if showing {
            self.closing.is_some_and(|t| now.saturating_duration_since(t) < OVERVIEW_EXIT_CARRY)
        } else {
            self.hint.is_some_and(|(h, t)| now.saturating_duration_since(t) < h.watch())
        }
    }

    /// True when the watch wants a look at the window list at `now` (asked at the fast tick;
    /// the look is then counted made).
    pub fn look_due(&mut self, showing: bool, now: Instant) -> bool {
        if !self.watching(showing, now) || self.looked.is_some_and(|t| now.saturating_duration_since(t) < WATCH_EVERY) {
            return false;
        }
        self.looked = Some(now);
        true
    }

    /// A look was made at `now` (the caller's own, at a hint): the watch's next comes
    /// [`WATCH_EVERY`] later.
    pub fn looked(&mut self, now: Instant) {
        self.looked = Some(now);
    }

    /// What a look at `now` found, `showing` Mission Control or not. Shown: the hint that
    /// announced it and how long before, when one came within [`HINT_COUNTS`]; it is spent.
    /// Not shown: a close's watch is over.
    pub fn saw(&mut self, showing: bool, now: Instant) -> Option<(Hint, Duration)> {
        if showing {
            return self.hint.take().map(|(h, t)| (h, now.saturating_duration_since(t))).filter(|(_, d)| *d < HINT_COUNTS);
        }
        self.closing = None;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::mission_control::{DOCK_WATCH, KEY_WATCH, SERVER_WATCH, Trigger};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    const KEY: Hint = Hint::Key(Trigger::MissionControl);

    #[test]
    fn a_hint_brings_looks_at_the_fast_tick_until_it_shows_or_its_watch_is_over() {
        let t0 = Instant::now();
        let mut w = LookWatch::default();
        assert!(!w.watching(false, t0) && !w.look_due(false, t0), "nothing heard: the poll's look alone");
        assert!(w.hint(Hint::Server, t0));
        w.looked(t0);
        assert!(w.watching(false, t0 + ms(10)));
        assert!(!w.look_due(false, t0 + ms(20)), "a look a moment ago");
        assert!(w.look_due(false, t0 + WATCH_EVERY));
        assert!(!w.look_due(false, t0 + WATCH_EVERY + ms(10)));
        assert!(w.look_due(false, t0 + WATCH_EVERY * 2));
        // Found: the hint that announced it, spent; no opening is watched for while it shows.
        assert_eq!(w.saw(true, t0 + ms(70)), Some((Hint::Server, ms(70))));
        assert!(!w.watching(true, t0 + ms(80)) && !w.look_due(true, t0 + ms(120)));
        assert_eq!(w.saw(true, t0 + ms(90)), None);
        // Never shown: the watch ends by itself.
        let mut w = LookWatch::default();
        assert!(w.hint(Hint::Dock, t0));
        assert!(w.watching(false, t0 + DOCK_WATCH - ms(1)) && !w.watching(false, t0 + DOCK_WATCH));
        assert!(w.hint(Hint::Server, t0 + ms(400)));
        assert!(!w.watching(false, t0 + ms(400) + SERVER_WATCH));
    }

    #[test]
    fn a_key_stands_for_its_watch() {
        let t0 = Instant::now();
        let mut w = LookWatch::default();
        assert!(w.hint(KEY, t0));
        assert!(!w.hint(Hint::Dock, t0 + ms(40)), "the Dock's notice after the key");
        assert!(!w.hint(Hint::Server, t0 + ms(40)), "the window server's after the key");
        assert!(!w.hint(KEY, t0 + ms(300)), "its own repeat");
        assert_eq!(w.saw(true, t0 + ms(45)), Some((KEY, ms(45))), "the key announced it");
        assert!(w.hint(Hint::Dock, t0 + KEY_WATCH), "its watch over: the next hint counts");
        // A hint long before it shows did not announce it.
        assert_eq!(w.saw(true, t0 + KEY_WATCH + HINT_COUNTS), None);
    }

    #[test]
    fn a_closing_key_or_a_press_brings_the_looks_for_its_close() {
        let t0 = Instant::now();
        let mut w = LookWatch::default();
        assert!(!w.watching(true, t0), "it shows and nothing was heard: the poll's look");
        w.close_hint(t0);
        assert!(w.look_due(true, t0 + ms(5)));
        assert!(!w.look_due(true, t0 + ms(20)) && w.look_due(true, t0 + ms(5) + WATCH_EVERY));
        assert!(w.watching(true, t0 + OVERVIEW_EXIT_CARRY - ms(1)));
        assert!(!w.watching(true, t0 + OVERVIEW_EXIT_CARRY), "a press that closed nothing: watched no longer");
        // Seen closed: over.
        w.close_hint(t0 + ms(2000));
        assert_eq!(w.saw(false, t0 + ms(2600)), None);
        assert!(!w.watching(true, t0 + ms(2610)) && !w.watching(false, t0 + ms(2610)));
    }
}
