//! The day's event counters: the app counts what it does (switches, card clicks, captures,
//! warnings…) and writes them as one `COUNT` line every hour, at the day's rollover and at
//! quit (see [`super::summary`] for reading them back).
//!
//! Counters are process-wide atomics, cheap enough to bump from any thread.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// Something the app counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// The active stage changed (any cause: card click, focus, hotkey, ctl).
    Switch,
    /// A card or its icon was clicked.
    CardClick,
    /// A card dropped in the main area, or a window dropped on the strip.
    Drop,
    /// A card dragged to another slot.
    Reorder,
    /// A new window was placed.
    NewWindow,
    /// A stage was made.
    NewStage,
    /// The Space shown changed.
    SpaceChange,
    /// A full-screen app's Space came to show.
    FullScreen,
    /// A thumbnail was captured.
    CaptureOk,
    /// A thumbnail capture failed.
    CaptureFailed,
    /// A safety layer had to bring windows back (recovery scan, registry, panic resync).
    Recovery,
    /// A warning was logged.
    Warning,
    /// An error was logged.
    Error,
    /// A panic (caught or not).
    Panic,
}

/// Every counter, in the order they print.
pub const ALL: [Count; 14] = [
    Count::Switch,
    Count::CardClick,
    Count::Drop,
    Count::Reorder,
    Count::NewWindow,
    Count::NewStage,
    Count::SpaceChange,
    Count::FullScreen,
    Count::CaptureOk,
    Count::CaptureFailed,
    Count::Recovery,
    Count::Warning,
    Count::Error,
    Count::Panic,
];

impl Count {
    /// The key in a `COUNT` line.
    pub fn key(self) -> &'static str {
        match self {
            Count::Switch => "switches",
            Count::CardClick => "card_clicks",
            Count::Drop => "drops",
            Count::Reorder => "reorders",
            Count::NewWindow => "new_windows",
            Count::NewStage => "new_stages",
            Count::SpaceChange => "space_changes",
            Count::FullScreen => "full_screen",
            Count::CaptureOk => "captures_ok",
            Count::CaptureFailed => "captures_failed",
            Count::Recovery => "recoveries",
            Count::Warning => "warnings",
            Count::Error => "errors",
            Count::Panic => "panics",
        }
    }

    /// How it reads in a summary.
    pub fn label(self) -> &'static str {
        match self {
            Count::Switch => "switches",
            Count::CardClick => "card clicks",
            Count::Drop => "drops",
            Count::Reorder => "reorders",
            Count::NewWindow => "new windows",
            Count::NewStage => "new stages",
            Count::SpaceChange => "Space changes",
            Count::FullScreen => "full-screen entries",
            Count::CaptureOk => "captures ok",
            Count::CaptureFailed => "captures failed",
            Count::Recovery => "recoveries",
            Count::Warning => "warnings",
            Count::Error => "errors",
            Count::Panic => "panics",
        }
    }

    fn index(self) -> usize {
        ALL.iter().position(|c| *c == self).unwrap_or(0)
    }
}

/// A set of counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts(pub [u64; ALL.len()]);

impl Counts {
    /// One count.
    pub fn get(&self, c: Count) -> u64 {
        self.0[c.index()]
    }

    /// Adds `other` in.
    pub fn add(&mut self, other: &Counts) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a += b;
        }
    }

    /// `switches=3 card_clicks=1 …`, every key.
    pub fn fields(&self) -> String {
        let mut out = String::new();
        for c in ALL {
            if !out.is_empty() {
                out.push(' ');
            }
            let _ = write!(out, "{}={}", c.key(), self.get(c));
        }
        out
    }

    /// Reads `key=value` words (unknown keys and other words are skipped).
    pub fn parse_fields(text: &str) -> Counts {
        let mut counts = Counts::default();
        for word in text.split_whitespace() {
            let Some((k, v)) = word.split_once('=') else { continue };
            if let (Some(c), Ok(n)) = (ALL.iter().find(|c| c.key() == k), v.parse::<u64>()) {
                counts.0[c.index()] = n;
            }
        }
        counts
    }
}

/// Process-wide counters.
pub struct Counters([AtomicU64; ALL.len()]);

impl Counters {
    /// All zero.
    pub const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; ALL.len()])
    }

    /// Adds one.
    pub fn bump(&self, c: Count) {
        self.0[c.index()].fetch_add(1, Ordering::Relaxed);
    }

    /// The counts now.
    pub fn snapshot(&self) -> Counts {
        let mut out = Counts::default();
        for (o, a) in out.0.iter_mut().zip(&self.0) {
            *o = a.load(Ordering::Relaxed);
        }
        out
    }

    /// The counts now, every counter reset to zero (a day's rollover).
    pub fn take(&self) -> Counts {
        let mut out = Counts::default();
        for (o, a) in out.0.iter_mut().zip(&self.0) {
            *o = a.swap(0, Ordering::Relaxed);
        }
        out
    }
}

impl Default for Counters {
    fn default() -> Self {
        Self::new()
    }
}

static COUNTERS: Counters = Counters::new();

/// Counts one event in this process.
pub fn bump(c: Count) {
    COUNTERS.bump(c);
}

/// This process's counters.
pub fn global() -> &'static Counters {
    &COUNTERS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_print_every_key_and_read_back() {
        let c = Counters::new();
        c.bump(Count::Switch);
        c.bump(Count::Switch);
        c.bump(Count::Panic);
        let s = c.snapshot();
        assert_eq!(s.get(Count::Switch), 2);
        let text = s.fields();
        assert!(text.starts_with("switches=2 card_clicks=0"));
        assert!(text.ends_with("panics=1"));
        assert_eq!(Counts::parse_fields(&format!("counts kind=hour {text} bogus=3 x")), s);
        assert_eq!(c.take(), s);
        assert_eq!(c.snapshot(), Counts::default());
    }

    #[test]
    fn keys_are_unique() {
        for (i, a) in ALL.iter().enumerate() {
            assert_eq!(a.index(), i);
            assert!(ALL[..i].iter().all(|b| b.key() != a.key()));
        }
    }
}
