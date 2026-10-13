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

    #[test]
    fn a_quick_flip_releases_and_resumes_in_order() {
        let mut s = StandDown::default();
        let steps: Vec<Step> = [true, false, true, false].into_iter().map(|on| s.apple(on)).collect();
        assert_eq!(steps, vec![Step::Release, Step::Resume, Step::Release, Step::Resume]);
    }
}
