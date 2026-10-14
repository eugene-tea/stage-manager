//! Restore after a restart (the design 8D, round 8E's engine part): what the model keeps
//! about it ([`Restore`]), when a pending window is given up on ([`AppQuiet`]), and where a
//! stage that comes back takes its strip slot ([`merge_order`]).
//!
//! **Pending windows.** A saved window not on screen yet is kept *pending* in its saved stage
//! (`state.json` writes it with `pending: {since}`), and the stage stays alive with no card,
//! so a window its app opens later goes back there ([`crate::engine::model::Model::add_window`]).
//! A pending window is given up on when:
//! - its app has shown a window and then opened none for [`QUIET`] (the window was closed
//!   inside the app's own session, or was an unsaved document): the platform asks
//!   [`AppQuiet`]; an app with no window yet is never quiet;
//! - it was pending through a whole window server session already (its app never brought it
//!   back): [`crate::engine::persist::StateFile::start_new_session`] drops it at the next one.
//!
//! **The restore window** ([`Restore::restoring`]) opens at a launch that finds saved stages
//! and closes at the first user action (a click, a key press, a card click, a Dock click, a
//! hotkey, a ctl command that acts) or [`WINDOW`] after the launch, whichever comes first
//! ([`crate::engine::model::Model::end_restore`]). While it is open a window going back to an
//! inactive stage is parked there quietly (its app is reopening it, or it is listed late),
//! and after a restart ([`Restore::new_session`]) a Desktop the model makes from an empty
//! screen is *provisional* (windows that match a saved Desktop's with confidence make it that
//! Desktop, adoption) and the saved active stage keeps its place while its windows are still
//! to come. After it, no Desktop becomes provisional, and a window matched to an inactive
//! stage brings that stage forward, as Apple's rule does for an app whose windows are in
//! another stage (the window shows because of the user: an activation, an un-minimize, a
//! Dock click). Pending windows are matched all session either way. At its end a saved
//! active stage still empty (its windows still to come) while other stages have windows gives
//! way to the focused window's stage, else the most recent one with windows
//! ([`crate::engine::model::Model::front_after_restore`]): never an empty front stage with
//! cards beside it.
//!
//! **Accepted:** an app that opens a window by itself, with no user action, during the
//! restore window (the first five minutes after a launch, until the first click or key
//! press) has a window matching a saved one in an inactive stage parked there (a card),
//! not switched to; the restore cannot tell it from the app reopening its saved windows.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::model::StageId;

/// An app this long without a new window (after its first) has settled: its pending
/// windows are given up on.
pub const QUIET: Duration = Duration::from_secs(60);

/// The restore window's longest life after the launch (the design's cap).
pub const WINDOW: Duration = Duration::from_secs(5 * 60);

/// What the model knows of the restore.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Restore {
    /// The window server session now, stamped on windows that become pending
    /// (`pending.since`); empty when unknown.
    pub session: String,
    /// The restore window is open (see the module docs): set at a launch that finds
    /// `state.json`, cleared for good at its end.
    pub restoring: bool,
    /// `state.json` came from another window server session (its window ids were dropped):
    /// saved Desktops are recognized at an arrival by confident matches (title, frame,
    /// fingerprint), all session.
    pub new_session: bool,
}

impl Restore {
    /// A launch in another window server session than the file's, now `session`.
    pub fn after_restart(session: &str) -> Self {
        Self { session: session.to_string(), restoring: true, new_session: true }
    }

    /// A launch in the file's own session (or one that cannot be told apart), now `session`.
    pub fn same_session(session: &str) -> Self {
        Self { session: session.to_string(), restoring: true, new_session: false }
    }

    /// True while a Desktop made from an empty screen is provisional and an empty saved
    /// active stage holds its place: the restore window after a restart.
    pub fn after_restart_window(&self) -> bool {
        self.restoring && self.new_session
    }
}

/// When each app was last busy: its last new window, or when it was first seen running
/// with a window. An app with no window yet has no clock (never settled). Pure: the
/// platform feeds it times.
#[derive(Debug, Clone, Default)]
pub struct AppQuiet {
    last: HashMap<String, Instant>,
}

impl AppQuiet {
    /// App `bundle` is running at `now`; `has_window`: it has a window already (one we
    /// manage or one waiting to show). Its clock starts the first time it has one.
    pub fn running(&mut self, bundle: &str, has_window: bool, now: Instant) {
        if has_window {
            self.last.entry(bundle.to_string()).or_insert(now);
        }
    }

    /// App `bundle` opened a window at `now`.
    pub fn window(&mut self, bundle: &str, now: Instant) {
        self.last.insert(bundle.to_string(), now);
    }

    /// App `bundle` is not running: a later launch starts its clock again.
    pub fn stopped(&mut self, bundle: &str) {
        self.last.remove(bundle);
    }

    /// Forgets the apps `running` says quit (a clock for every app ever seen would grow all
    /// session).
    pub fn prune(&mut self, running: impl Fn(&str) -> bool) {
        self.last.retain(|b, _| running(b));
    }

    /// How many apps have a clock.
    pub fn len(&self) -> usize {
        self.last.len()
    }

    /// True when no app has a clock.
    pub fn is_empty(&self) -> bool {
        self.last.is_empty()
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
        // M4: running with no window yet is no clock at all.
        q.running("com.example.a", false, t0);
        assert!(q.settled(t0 + Duration::from_secs(600)).is_empty(), "an app with no window yet is never settled");
        q.running("com.example.a", true, t0);
        q.running("com.example.a", true, t0 + Duration::from_secs(30));
        assert!(q.settled(t0 + Duration::from_secs(59)).is_empty());
        assert_eq!(q.settled(t0 + QUIET), ["com.example.a"], "running does not restart the clock");
        q.window("com.example.a", t0 + Duration::from_secs(50));
        assert!(q.settled(t0 + QUIET).is_empty(), "a new window does");
        assert_eq!(q.settled(t0 + Duration::from_secs(110)), ["com.example.a"]);
        q.stopped("com.example.a");
        assert!(q.settled(t0 + Duration::from_secs(500)).is_empty(), "an app that quit is not settled");
        // The first window starts the clock of an app seen running without one.
        q.running("com.example.b", false, t0);
        q.window("com.example.b", t0 + Duration::from_secs(200));
        assert!(q.settled(t0 + Duration::from_secs(259)).is_empty());
        assert_eq!(q.settled(t0 + Duration::from_secs(260)), ["com.example.b"]);
    }

    #[test]
    fn apps_that_quit_are_forgotten() {
        let t0 = Instant::now();
        let mut q = AppQuiet::default();
        for b in ["com.example.a", "com.example.b", "com.example.c"] {
            q.window(b, t0);
        }
        q.prune(|b| b == "com.example.b");
        assert_eq!(q.len(), 1);
        assert_eq!(q.settled(t0 + QUIET), ["com.example.b"]);
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
