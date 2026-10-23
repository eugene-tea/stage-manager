//! An app whose process is gone though no termination notice came (round missed-quit),
//! decided without touching anything.
//!
//! The owner's 10-23 10:53: Home (`com.apple.Home`, a Catalyst app) quit with its last
//! window. LaunchServices told the system (`loginwindow` logged the app's death, as for
//! Reminders a minute later), but no "app terminated" reached the WM thread, for Home on any
//! day: its window stayed in the model, every Accessibility call to it failed
//! (`CannotComplete`), its stage stayed a gray card, the busy-app check asked it every few
//! seconds, and its `state.json` entry never got `quit_at`, so each later launch put it back
//! into its old stage. (The 10-21 "closed while its app did not answer" was this quit too.)
//!
//! A process that is gone is gone: the platform tests the pids it has reason to doubt (the
//! kernel answers "no such process") and runs the same path as a real termination. Cheap by
//! construction ([`GoneCheck::candidates`]): only the pids whose Accessibility calls failed or
//! timed out since the last pass, every pass, and the pids with tracked windows on a full pass
//! (about once a second); never a scan of every process.

use std::collections::BTreeSet;

/// The pids to test for a gone process.
#[derive(Debug, Clone, Default)]
pub struct GoneCheck {
    /// Pids whose Accessibility calls failed or timed out since the last pass.
    suspects: BTreeSet<i32>,
}

impl GoneCheck {
    /// An Accessibility call to app `pid` failed or timed out: its process is tested at the
    /// next pass.
    pub fn suspect(&mut self, pid: i32) {
        if pid > 0 {
            self.suspects.insert(pid);
        }
    }

    /// True when app `pid` waits for the next pass.
    pub fn is_suspect(&self, pid: i32) -> bool {
        self.suspects.contains(&pid)
    }

    /// The pids to test this pass, each once, in order: every suspect (forgotten here: a live
    /// one is suspected again at its next failure), and on a `full` pass every pid of
    /// `window_pids` (the apps with tracked windows), all filtered by `known` (apps the observer
    /// knows: one already handled as quit is never handled twice).
    pub fn candidates(&mut self, window_pids: impl IntoIterator<Item = i32>, full: bool, known: impl Fn(i32) -> bool) -> Vec<i32> {
        let mut pids = std::mem::take(&mut self.suspects);
        if full {
            pids.extend(window_pids.into_iter().filter(|&p| p > 0));
        }
        pids.into_iter().filter(|&p| known(p)).collect()
    }

    /// Forgets everything (observation stopped).
    pub fn clear(&mut self) {
        self.suspects.clear();
    }
}

/// The pids of `candidates` whose process `is_gone` says no longer exists, in order.
pub fn gone(candidates: Vec<i32>, is_gone: impl Fn(i32) -> bool) -> Vec<i32> {
    candidates.into_iter().filter(|&p| is_gone(p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspects_are_tested_every_pass_and_window_pids_on_a_full_pass_only() {
        let mut c = GoneCheck::default();
        c.suspect(41);
        assert_eq!(c.candidates([10, 20], false, |_| true), [41], "a quick pass: the suspect only");
        assert!(!c.is_suspect(41), "forgotten once tested");
        assert!(c.candidates([10, 20], false, |_| true).is_empty(), "no failure since: nothing to test");
        c.suspect(41);
        assert_eq!(c.candidates([20, 10, 20, 41], true, |_| true), [10, 20, 41], "a full pass: each pid once");
    }

    #[test]
    fn only_known_real_pids_are_tested() {
        let mut c = GoneCheck::default();
        c.suspect(0);
        c.suspect(-1);
        c.suspect(7);
        c.suspect(8);
        // 8 already handled as quit (the observer no longer knows it): never twice.
        assert_eq!(c.candidates([-1, 0, 9], true, |p| p != 8), [7, 9]);
    }

    #[test]
    fn only_gone_processes_are_handled_as_quit() {
        // Home (41341) is gone; the busy app (11697) answers slowly but lives.
        let mut c = GoneCheck::default();
        c.suspect(11697);
        let pids = c.candidates([41341, 11697], true, |_| true);
        assert_eq!(gone(pids, |p| p == 41341), [41341]);
        c.suspect(5);
        c.clear();
        assert!(c.candidates([], false, |_| true).is_empty());
    }
}
