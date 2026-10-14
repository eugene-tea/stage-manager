//! Restore after a restart (the design 8D, round 8E's engine part): what the model keeps
//! about it ([`Restore`]), when a pending window is given up on ([`AppQuiet`]), and where a
//! stage that comes back takes its strip slot ([`merge_order`]).
//!
//! **Pending windows.** A saved window not on screen yet is kept *pending* in its saved stage
//! (`state.json` writes it with `pending: {since}`), and the stage stays alive with no card,
//! so a window its app opens later goes back there ([`crate::engine::model::Model::add_window`]).
//! A pending window is given up on when:
//! - its app has been running and opened no window for [`QUIET`] (the window was closed inside
//!   the app's own session, or was an unsaved document): the platform asks [`AppQuiet`];
//! - it was pending through a whole window server session already (its app never brought it
//!   back): [`crate::engine::persist::StateFile::start_new_session`] drops it at the next one.
//!
//! **Restore mode** is a launch in another window server session than the file's (a restart
//! or a new login): a Desktop the model makes from an empty screen is then *provisional*, and
//! the first window that matches a saved Desktop's window with confidence makes it that
//! Desktop ([`crate::engine::model::Model::add_window`], adoption).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::model::StageId;

/// An app running this long without a new window has settled: its pending windows are
/// given up on.
pub const QUIET: Duration = Duration::from_secs(60);

/// What the model knows of the restore.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Restore {
    /// The window server session now, stamped on windows that become pending
    /// (`pending.since`); empty when unknown.
    pub session: String,
    /// Restore mode: `state.json` came from another session.
    pub restoring: bool,
}

/// When each app with pending windows was last busy: first seen running, or opened a
/// window. Pure: the platform feeds it times.
#[derive(Debug, Clone, Default)]
pub struct AppQuiet {
    last: HashMap<String, Instant>,
}

impl AppQuiet {
    /// App `bundle` is running at `now` (its clock starts the first time).
    pub fn running(&mut self, bundle: &str, now: Instant) {
        self.last.entry(bundle.to_string()).or_insert(now);
    }

    /// App `bundle` opened a window at `now`.
    pub fn window(&mut self, bundle: &str, now: Instant) {
        self.last.insert(bundle.to_string(), now);
    }

    /// App `bundle` is not running: a later launch starts its clock again.
    pub fn stopped(&mut self, bundle: &str) {
        self.last.remove(bundle);
    }

    /// The apps quiet for [`QUIET`] at `now`.
    pub fn settled(&self, now: Instant) -> Vec<String> {
        let mut out: Vec<String> =
            self.last.iter().filter(|(_, t)| now.saturating_duration_since(**t) >= QUIET).map(|(b, _)| b.clone()).collect();
        out.sort();
        out
    }
}

/// `current` (the strip's slots, top to bottom) with every stage of `saved` (the saved
/// order) that `keep` accepts and `current` lacks, each put right below the nearest stage
/// above it in `saved` that is in the result (at the top when none is). So stages coming
/// back one by one end in their saved order.
pub fn merge_order(current: &[StageId], saved: &[StageId], keep: impl Fn(StageId) -> bool) -> Vec<StageId> {
    let mut out = current.to_vec();
    for (i, &s) in saved.iter().enumerate() {
        if out.contains(&s) || !keep(s) {
            continue;
        }
        let at = saved[..i].iter().rev().find_map(|above| out.iter().position(|x| x == above)).map_or(0, |p| p + 1);
        out.insert(at, s);
    }
    out
}

/// Where stage `stage` enters `slots` to keep `saved`'s order: right above the first slot
/// that comes after it in `saved`, else at the bottom. `None` when `saved` does not hold it.
pub fn slot_for(slots: &[StageId], saved: &[StageId], stage: StageId) -> Option<usize> {
    let i = saved.iter().position(|&s| s == stage)?;
    let after = &saved[i + 1..];
    Some(slots.iter().position(|s| after.contains(s)).unwrap_or(slots.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(ids: &[u64]) -> Vec<StageId> {
        ids.iter().map(|&i| StageId(i)).collect()
    }

    #[test]
    fn an_app_settles_after_a_quiet_minute() {
        let t0 = Instant::now();
        let mut q = AppQuiet::default();
        q.running("com.example.a", t0);
        q.running("com.example.a", t0 + Duration::from_secs(30));
        assert!(q.settled(t0 + Duration::from_secs(59)).is_empty());
        assert_eq!(q.settled(t0 + QUIET), ["com.example.a"], "running does not restart the clock");
        q.window("com.example.a", t0 + Duration::from_secs(50));
        assert!(q.settled(t0 + QUIET).is_empty(), "a new window does");
        assert_eq!(q.settled(t0 + Duration::from_secs(110)), ["com.example.a"]);
        q.stopped("com.example.a");
        assert!(q.settled(t0 + Duration::from_secs(500)).is_empty(), "an app that quit is not settled");
    }

    #[test]
    fn stages_coming_back_take_their_saved_slots() {
        let saved = s(&[3, 1, 4, 2]);
        // None in the strip yet: each lands where the saved order says.
        let mut slots: Vec<StageId> = Vec::new();
        for stage in [4, 2, 3, 1] {
            let at = slot_for(&slots, &saved, StageId(stage)).expect("saved");
            slots.insert(at, StageId(stage));
        }
        assert_eq!(slots, saved);
        // A stage the saved order does not know.
        assert_eq!(slot_for(&slots, &saved, StageId(9)), None);
        // A new stage already in the strip keeps its place; the saved ones go around it.
        assert_eq!(merge_order(&s(&[9, 4]), &saved, |_| true), s(&[3, 1, 9, 4, 2]));
        assert_eq!(merge_order(&s(&[4]), &saved, |x| x != StageId(1)), s(&[3, 4, 2]));
    }
}
