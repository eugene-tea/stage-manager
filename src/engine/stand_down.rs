//! Standing down while Apple's Stage Manager is on.
//!
//! Both cannot run: Apple's would arrange the windows we park, and our strip would sit over
//! its strip. So while `GloballyEnabled` is 1 the app stands down: every parked window comes
//! back through the safe unpark path, the strip hides on every Desktop, observation stops and
//! no input arranges anything. When Apple's turns off again, the app starts afresh from the
//! windows on screen.
//!
//! This is the pure part: what to do when the setting is read, as an ordered plan of
//! [`Task`]s the WM thread (`platform::wm_thread`) carries out.
//!
//! - **Saved stages go at the release** ([`Task::DropSavedStages`]), the moment they become
//!   stale (Apple's arranges the windows from then on): a quit, crash or logout while
//!   standing down then leaves nothing for the next launch to re-park from.
//! - **The fresh controller gets the displays and settings** the WM thread last had
//!   ([`Task::ReplayDisplays`], [`Task::ReplaySettings`]); the main thread only sends them
//!   when they change, so a controller made later would otherwise guess the display.
//! - **Windows the release could not bring back** (another Space's: Accessibility cannot
//!   move them while their Space is not shown) stay on record ([`Task::HoldStranded`]): held
//!   apart from any controller's registry entries, so a resumed controller's first park does
//!   not drop them, and retried at every Space change and at the resume until all are back,
//!   whether the app is still standing down or managing again.
//!
//! **The logout's head start** ([`Logout`], [`logout_plan`]; the review of 8F, M3) stands down
//! the same way for a logout, restart or shutdown (`willPowerOffNotification`), without
//! quitting: every window back, the leftovers check and the stages saved (kept: the next
//! login restores them), so windows are at their real frames as early as possible before apps
//! save theirs. The app stays running so macOS's own quit (`applicationShouldTerminate:`, as
//! in 1.2.0) ends it: an app that quits before macOS asks may not be reopened at the next
//! login. That quit finds the work done and only exits ([`Quit::Done`]), but for windows the
//! head start could not bring back (another Desktop's): held as at a stand-down
//! ([`Task::HoldStranded`]), tried again at every Space change during the hold and once more
//! at the quit. A logout another app cancels sends no quit: after [`LOGOUT_WAIT`] the app
//! starts afresh, from the saved stages, as at a launch, the held windows still on record.

use std::time::{Duration, Instant};

/// What the WM thread must do after a reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing changes.
    Nothing,
    /// Apple's came on: carry out [`release_plan`].
    Release,
    /// Apple's went off: carry out [`resume_plan`].
    Resume,
}

/// One task of a release or a resume, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    /// The controller's unpark (the quit path), then the registry and a recovery scan.
    UnparkAll,
    /// Windows still parked are held apart from the controller's registry until back.
    HoldStranded,
    /// `state.json` is removed: its stages are stale from now on.
    DropSavedStages,
    /// Observation stops; its pending events are dropped.
    StopObserving,
    /// A fresh controller (no stages) replaces the old one.
    NewController,
    /// The fresh controller gets the last displays.
    ReplayDisplays,
    /// The fresh controller gets the last `com.apple.WindowManager` values.
    ReplaySettings,
    /// The strip hides on every Desktop.
    HideStrip,
    /// Held windows are tried again (their Space may show now).
    RetryHeld,
    /// Observation starts again (when Accessibility is granted): a fresh start.
    StartObserving,
    /// `state.json` is written now (the logout's head start: the next login restores it).
    SaveStages,
}

/// The release, in order. Displays and settings are replayed when the WM thread has them.
pub fn release_plan(have_displays: bool, have_settings: bool) -> Vec<Task> {
    let mut plan = vec![Task::UnparkAll, Task::HoldStranded, Task::DropSavedStages, Task::StopObserving, Task::NewController];
    if have_displays {
        plan.push(Task::ReplayDisplays);
    }
    if have_settings {
        plan.push(Task::ReplaySettings);
    }
    plan.push(Task::HideStrip);
    plan
}

/// The resume, in order: held windows first (they may be back where the fresh start looks).
pub fn resume_plan(stranded: bool) -> Vec<Task> {
    let mut plan = Vec::new();
    if stranded {
        plan.push(Task::RetryHeld);
    }
    plan.push(Task::StartObserving);
    plan
}

/// The logout's head start, in order: windows back and the leftovers check, windows it could
/// not bring back held (as at a stand-down: a cancelled logout's fresh controller keeps them
/// on record, and every Space change tries them again), the stages saved (never dropped),
/// then the controller set aside as at a stand-down.
pub fn logout_plan(have_displays: bool, have_settings: bool) -> Vec<Task> {
    let mut plan = vec![Task::UnparkAll, Task::HoldStranded, Task::SaveStages, Task::StopObserving, Task::NewController];
    if have_displays {
        plan.push(Task::ReplayDisplays);
    }
    if have_settings {
        plan.push(Task::ReplaySettings);
    }
    plan.push(Task::HideStrip);
    plan
}

/// How long the logout's head start waits for macOS's quit before it takes the logout for
/// cancelled (an app refused to quit, or the user cancelled its dialog) and starts afresh.
pub const LOGOUT_WAIT: Duration = Duration::from_secs(60);

/// What the app's quit has left to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quit {
    /// Everything: unpark, the leftovers check, save (no head start ran).
    Unpark,
    /// The head start did it: only exit (`all_back`: every window came back then).
    Done {
        /// Every window came back at the head start.
        all_back: bool,
    },
}

/// The logout's head start (see the module docs): held from `willPowerOffNotification` until
/// macOS's quit, or [`LOGOUT_WAIT`] when no quit comes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Logout {
    /// When the head start ran; `None` when none is held.
    since: Option<Instant>,
    /// Every window came back at the head start.
    all_back: bool,
}

impl Logout {
    /// A logout, restart or shutdown is coming at `now`: true when the head start runs now
    /// (once: a repeated notice while held does nothing).
    pub fn coming(&mut self, now: Instant) -> bool {
        if self.since.is_some() {
            return false;
        }
        self.since = Some(now);
        self.all_back = false;
        true
    }

    /// The head start's work is done; `all_back`: every window came back.
    pub fn released(&mut self, all_back: bool) {
        self.all_back = all_back;
    }

    /// True while the head start holds (nothing is arranged; input is ignored).
    pub fn holding(&self) -> bool {
        self.since.is_some()
    }

    /// What the app's quit has left to do now.
    pub fn quit(&self) -> Quit {
        if self.holding() { Quit::Done { all_back: self.all_back } } else { Quit::Unpark }
    }

    /// True (once) when no quit came within [`LOGOUT_WAIT`] of the head start at `now`: the
    /// logout was cancelled, the hold ends and the app starts afresh.
    pub fn cancelled(&mut self, now: Instant) -> bool {
        if self.since.is_some_and(|t| now.saturating_duration_since(t) >= LOGOUT_WAIT) {
            self.since = None;
            return true;
        }
        false
    }

    /// Whether `work` may run now (only housekeeping while held).
    pub fn admits(&self, work: Work) -> bool {
        !self.holding() || work == Work::Housekeeping
    }
}

/// Something the user or the system asks of the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    /// Clicks, drags, the strip's events, hotkeys.
    Input,
    /// A Space change.
    SpaceChange,
    /// Starting observation (Accessibility granted).
    Observe,
    /// Config reloads, settings, displays, ctl reads, quit.
    Housekeeping,
}

/// The stand-down state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StandDown {
    stood_down: bool,
    /// Windows a release could not bring back yet: held and retried on every Space change,
    /// through a resume, until all are back.
    stranded: bool,
}

impl StandDown {
    /// A reading of Apple's `GloballyEnabled` (`on`): the step it calls for.
    pub fn apple(&mut self, on: bool) -> Step {
        match (self.stood_down, on) {
            (false, true) => {
                self.stood_down = true;
                Step::Release
            }
            (true, false) => {
                self.stood_down = false;
                Step::Resume
            }
            _ => Step::Nothing,
        }
    }

    /// A release or a retry ran; `all_back` when no held window is left parked.
    pub fn released(&mut self, all_back: bool) {
        self.stranded = !all_back;
    }

    /// Whether the app is standing down.
    pub fn stood_down(&self) -> bool {
        self.stood_down
    }

    /// Whether held windows still wait to come back (retry when a Space shows).
    pub fn stranded(&self) -> bool {
        self.stranded
    }

    /// Whether `work` may run now.
    pub fn admits(&self, work: Work) -> bool {
        !self.stood_down || work == Work::Housekeeping
    }
}

/// The registry after a controller replaces its entries: its own, plus the held windows it
/// did not take over (a held window the controller parks itself is its own again).
pub fn merge_held<T: Clone>(own: std::collections::BTreeMap<u32, T>, held: &std::collections::BTreeMap<u32, T>) -> std::collections::BTreeMap<u32, T> {
    let mut out = own;
    for (id, entry) in held {
        out.entry(*id).or_insert_with(|| entry.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn apples_coming_on_releases_once_and_going_off_resumes_once() {
        let mut s = StandDown::default();
        assert_eq!(s.apple(false), Step::Nothing, "off at launch: nothing to do");
        assert_eq!(s.apple(true), Step::Release);
        assert!(s.stood_down());
        assert_eq!(s.apple(true), Step::Nothing, "every later reading while on does nothing");
        assert_eq!(s.apple(false), Step::Resume);
        assert!(!s.stood_down());
        assert_eq!(s.apple(false), Step::Nothing);
    }

    #[test]
    fn on_at_launch_stands_down_before_anything_is_observed() {
        let mut s = StandDown::default();
        assert_eq!(s.apple(true), Step::Release);
        assert!(!s.admits(Work::Observe), "Accessibility arriving later starts nothing");
        assert!(!s.admits(Work::Input) && !s.admits(Work::SpaceChange));
        assert!(s.admits(Work::Housekeeping), "config, settings and quit still work");
    }

    #[test]
    fn everything_runs_while_managing() {
        let s = StandDown::default();
        for w in [Work::Input, Work::SpaceChange, Work::Observe, Work::Housekeeping] {
            assert!(s.admits(w), "{w:?}");
        }
    }

    /// R8 S1: a window stranded by the release stays on record through the resume.
    #[test]
    fn stranded_windows_stay_on_record_through_a_resume() {
        let mut s = StandDown::default();
        s.apple(true);
        s.released(false);
        assert!(s.stranded(), "another Space's window waits for its Space");
        assert_eq!(s.apple(false), Step::Resume);
        assert!(s.stranded(), "resuming does not forget it");
        assert_eq!(resume_plan(s.stranded()), vec![Task::RetryHeld, Task::StartObserving], "tried again before the fresh start");
        s.released(false);
        assert!(s.stranded(), "still parked: retried at the next Space change");
        s.released(true);
        assert!(!s.stranded());
        assert_eq!(resume_plan(s.stranded()), vec![Task::StartObserving]);
    }

    /// R8 S1: a resumed controller replacing the registry keeps the held windows.
    #[test]
    fn a_controllers_registry_keeps_held_windows_it_did_not_take_over() {
        let held: BTreeMap<u32, &str> = [(7, "held 7"), (9, "held 9")].into();
        let own: BTreeMap<u32, &str> = [(1, "own 1"), (9, "own 9")].into();
        let merged = merge_held(own, &held);
        assert_eq!(merged, [(1, "own 1"), (7, "held 7"), (9, "own 9")].into());
        assert_eq!(merge_held(BTreeMap::new(), &held), held, "an empty replace keeps them");
    }

    /// R8 S2: the fresh controller gets the displays and settings the WM thread had.
    #[test]
    fn the_fresh_controller_gets_the_last_displays_and_settings() {
        let plan = release_plan(true, true);
        let at = |t: Task| plan.iter().position(|p| *p == t).unwrap_or_else(|| panic!("{t:?} planned"));
        assert!(at(Task::NewController) < at(Task::ReplayDisplays));
        assert!(at(Task::NewController) < at(Task::ReplaySettings));
        assert!(!release_plan(false, false).contains(&Task::ReplayDisplays), "nothing to replay before the first display");
    }

    /// R8 S3: the saved stages go at the release, before anything can quit or crash.
    #[test]
    fn saved_stages_go_at_the_release_not_at_the_resume() {
        let plan = release_plan(true, true);
        let at = |t: Task| plan.iter().position(|p| *p == t).unwrap_or_else(|| panic!("{t:?} planned"));
        assert!(at(Task::UnparkAll) < at(Task::DropSavedStages), "windows come back first");
        assert!(at(Task::HoldStranded) < at(Task::NewController));
        for stranded in [false, true] {
            assert!(!resume_plan(stranded).contains(&Task::DropSavedStages));
        }
    }

    /// The review of 8F, M3: the head start brings every window back and saves without
    /// quitting; macOS's quit then only exits.
    #[test]
    fn the_logout_head_start_saves_the_stages_and_leaves_the_quit_to_macos() {
        let plan = logout_plan(true, true);
        let at = |t: Task| plan.iter().position(|p| *p == t).unwrap_or_else(|| panic!("{t:?} planned"));
        assert!(at(Task::UnparkAll) < at(Task::SaveStages), "saved after the windows are back, as at quit");
        assert!(at(Task::UnparkAll) < at(Task::HoldStranded), "what the unpark and its check left is held");
        assert!(at(Task::HoldStranded) < at(Task::NewController), "held before the controller that parked them goes");
        assert!(at(Task::SaveStages) < at(Task::NewController), "saved by the controller that knows the stages");
        assert!(at(Task::NewController) < at(Task::ReplayDisplays) && at(Task::NewController) < at(Task::ReplaySettings));
        assert!(plan.contains(&Task::HideStrip) && plan.contains(&Task::StopObserving));
        assert!(!plan.contains(&Task::DropSavedStages), "the next login restores them");
        assert!(!logout_plan(false, false).contains(&Task::ReplayDisplays));

        let t0 = Instant::now();
        let mut l = Logout::default();
        assert_eq!(l.quit(), Quit::Unpark, "no head start: the quit does it all");
        assert!(l.admits(Work::Input) && l.admits(Work::Observe));
        assert!(l.coming(t0));
        assert!(!l.coming(t0 + Duration::from_secs(1)), "once");
        assert!(l.holding() && !l.admits(Work::Input) && !l.admits(Work::SpaceChange) && !l.admits(Work::Observe));
        assert!(l.admits(Work::Housekeeping), "settings, displays, ctl reads and the quit still work");
        assert_eq!(l.quit(), Quit::Done { all_back: false }, "under way: nothing said back yet");
        l.released(true);
        assert_eq!(l.quit(), Quit::Done { all_back: true }, "the quit only exits");
        l.released(false);
        assert_eq!(l.quit(), Quit::Done { all_back: false }, "a window left: the quit's own fallback");
    }

    /// The review of 8F2, 1: a window the head start could not move (another Desktop's) stays
    /// on record through a cancelled logout: held, so the fresh controller's first batch (a
    /// registry replace with only its own windows) keeps it, and it is retried until back.
    #[test]
    fn a_window_the_logout_could_not_move_stays_held_through_a_cancel() {
        let plan = logout_plan(false, false);
        assert!(plan.contains(&Task::HoldStranded));
        assert!(!plan.contains(&Task::DropSavedStages));

        // The head start's HoldStranded: one window left on another Desktop.
        let mut s = StandDown::default();
        s.released(false);
        assert!(s.stranded() && !s.stood_down(), "held, though nothing stands down");
        assert!(s.admits(Work::SpaceChange), "a Space change during or after the hold retries it");

        // The cancel's fresh controller parks its own windows: the held one is kept.
        let held: BTreeMap<u32, &str> = [(31, "another Desktop's")].into();
        assert_eq!(merge_held(BTreeMap::new(), &held), held, "the first batch replaces nothing it does not own");
        assert_eq!(merge_held([(5, "own 5")].into(), &held), [(5, "own 5"), (31, "another Desktop's")].into());

        // Its Desktop shows: back, nothing held.
        s.released(true);
        assert!(!s.stranded());
    }

    #[test]
    fn a_cancelled_logout_starts_afresh_after_the_wait() {
        let t0 = Instant::now();
        let mut l = Logout::default();
        assert!(!l.cancelled(t0 + LOGOUT_WAIT), "nothing held");
        l.coming(t0);
        l.released(true);
        assert!(!l.cancelled(t0 + LOGOUT_WAIT - Duration::from_millis(1)));
        assert!(l.cancelled(t0 + LOGOUT_WAIT));
        assert!(!l.holding() && l.quit() == Quit::Unpark, "managing again: a later quit does everything");
        assert!(!l.cancelled(t0 + LOGOUT_WAIT * 2), "once");
        assert!(l.coming(t0 + LOGOUT_WAIT * 3), "a later logout gets its own head start");
    }

    #[test]
    fn a_quick_flip_releases_and_resumes_in_order() {
        let mut s = StandDown::default();
        let steps: Vec<Step> = [true, false, true, false].into_iter().map(|on| s.apple(on)).collect();
        assert_eq!(steps, vec![Step::Release, Step::Resume, Step::Release, Step::Resume]);
    }
}
