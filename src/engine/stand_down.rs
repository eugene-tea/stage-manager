//! Standing down while Apple's Stage Manager is on.
//!
//! Both cannot run: Apple's would arrange the windows we park, and our strip would sit over
//! its strip. So while `GloballyEnabled` is 1 the app stands down: every parked window comes
//! back through the safe unpark path, the strip hides on every Desktop, observation stops and
//! no input arranges anything. When Apple's turns off again, the app starts afresh from the
//! windows on screen (the stages saved before are stale: Apple's arranged the windows since).
//!
//! This is the pure part: what to do when the setting is read. The WM thread
//! (`platform::wm_thread`) carries the steps out.

/// What the WM thread must do after a reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing changes.
    Nothing,
    /// Apple's came on: bring every window back, hide the strip, stop observing.
    Release,
    /// Apple's went off: start afresh from the windows on screen.
    Resume,
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
    /// Windows the release could not bring back yet (another Space's): retried on every
    /// Space change while standing down.
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
                self.stranded = false;
                Step::Resume
            }
            _ => Step::Nothing,
        }
    }

    /// The release ran; `all_back` when no window is left parked anywhere.
    pub fn released(&mut self, all_back: bool) {
        self.stranded = self.stood_down && !all_back;
    }

    /// Whether the app is standing down.
    pub fn stood_down(&self) -> bool {
        self.stood_down
    }

    /// Whether windows still wait to come back (retry when their Space shows).
    pub fn stranded(&self) -> bool {
        self.stranded
    }

    /// Whether `work` may run now.
    pub fn admits(&self, work: Work) -> bool {
        !self.stood_down || work == Work::Housekeeping
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn windows_left_parked_by_the_release_are_remembered_until_resume() {
        let mut s = StandDown::default();
        s.apple(true);
        s.released(false);
        assert!(s.stranded(), "another Space's window waits for its Space");
        s.released(true);
        assert!(!s.stranded());
        s.released(false);
        assert_eq!(s.apple(false), Step::Resume);
        assert!(!s.stranded(), "resuming manages them again");
        s.released(false);
        assert!(!s.stranded(), "nothing is stranded while managing");
    }

    #[test]
    fn a_quick_flip_releases_and_resumes_in_order() {
        let mut s = StandDown::default();
        let steps: Vec<Step> = [true, false, true, false].into_iter().map(|on| s.apple(on)).collect();
        assert_eq!(steps, vec![Step::Release, Step::Resume, Step::Release, Step::Resume]);
    }
}
