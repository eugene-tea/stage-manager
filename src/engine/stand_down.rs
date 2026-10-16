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
//! the same way for a logout, restart or shutdown, without quitting, at the first of its
//! signals ([`SIGNALS`]; round 8H): `loginwindow`'s distributed notifications and notify(3)
//! names, with `willPowerOffNotification` the last fallback (the owner's restart of 10-16 had
//! it last, 4 ms before macOS's quit, after every app had quit). Each signal is logged with its
//! name and the time since the first, so a real restart shows their order. From the first
//! signal nothing is arranged (the freeze: activations, focus changes and app terminations
//! switch no stage, animate no strip, add or remove no card, and no save drops a stage): the
//! controller that saved the stages is set aside and observation stops. The head start: every
//! window back, the leftovers check and the stages saved (kept: the next login restores them),
//! so windows are at their real frames as early as possible before apps save theirs. The app stays running so macOS's own quit (`applicationShouldTerminate:`, as
//! in 1.2.0) ends it: an app that quits before macOS asks may not be reopened at the next
//! login. That quit finds the work done and only exits ([`Quit::Done`]), but for windows the
//! head start could not bring back (another Desktop's): held as at a stand-down
//! ([`Task::HoldStranded`]), tried again at every Space change during the hold and once more
//! at the quit. A cancel signal (`logoutCancelled`) ends the hold at once: the app starts
//! afresh from the saved stages, as at a launch (windows parked again per the model), the held
//! windows still on record. A logout cancelled with no such signal (an app refused to quit)
//! sends no quit: [`LOGOUT_WAIT`] after the latest signal the app starts afresh the same way.

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

/// How long the logout's head start waits after the latest logout signal for macOS's quit
/// before it takes the logout for cancelled with no cancel signal (an app refused to quit) and
/// starts afresh. Two minutes, not one: an early signal may come when the confirmation
/// dialog shows (a minute's countdown), and a hold that ended then would park the windows
/// again just as the apps save their frames.
pub const LOGOUT_WAIT: Duration = Duration::from_secs(120);

/// What a logout signal says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// A logout, restart or shutdown is coming (or goes on).
    Coming,
    /// The logout was cancelled.
    Cancelled,
}

/// Where a logout signal comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// `NSDistributedNotificationCenter` (`loginwindow`'s distributed notifications).
    Distributed,
    /// notify(3) (`notify_register_dispatch`).
    Notify,
    /// `NSWorkspace`'s `willPowerOffNotification` (the last fallback).
    Workspace,
}

/// Every logout signal listened for, by channel, name and meaning: `loginwindow` carries these
/// names (we do not know yet which, if any, comes before the apps quit: the log tells). The
/// notify(3) names keep `loginwindow`'s own spelling.
pub const SIGNALS: [(Channel, &str, Signal); 10] = [
    (Channel::Distributed, "com.apple.logoutInitiated", Signal::Coming),
    (Channel::Distributed, "com.apple.restartInitiated", Signal::Coming),
    (Channel::Distributed, "com.apple.shutdownInitiated", Signal::Coming),
    (Channel::Distributed, "com.apple.logoutContinued", Signal::Coming),
    (Channel::Distributed, "com.apple.logoutCancelled", Signal::Cancelled),
    (Channel::Notify, "com.apple.system.loginwindow.logoutInitiated", Signal::Coming),
    (Channel::Notify, "com.apple.system.loginwindow.restartinitiated", Signal::Coming),
    (Channel::Notify, "com.apple.system.loginwindow.shutdownInitiated", Signal::Coming),
    (Channel::Notify, "com.apple.system.loginwindow.logoutcancelled", Signal::Cancelled),
    (Channel::Workspace, "NSWorkspaceWillPowerOffNotification", Signal::Coming),
];

/// The suffix of the test's cancel note: with `STAGE_MANAGER_LOGOUT_NOTE=<name>` the app
/// listens for the distributed notifications `<name>` (coming) and `<name>.cancelled`.
pub const TEST_CANCEL_SUFFIX: &str = ".cancelled";

/// The signals listened for: Apple's ([`SIGNALS`]), or with a test note (`note`: the test's
/// `STAGE_MANAGER_LOGOUT_NOTE`, honoured only with `STAGE_MANAGER_TEST`) only the test's two
/// distributed names, so a scripted test can post them without touching other apps.
pub fn signals_to_observe(note: Option<&str>) -> Vec<(Channel, String, Signal)> {
    match note.map(str::trim).filter(|n| !n.is_empty()) {
        Some(note) => vec![
            (Channel::Distributed, note.to_string(), Signal::Coming),
            (Channel::Distributed, format!("{note}{TEST_CANCEL_SUFFIX}"), Signal::Cancelled),
        ],
        None => SIGNALS.iter().map(|(c, n, s)| (*c, (*n).to_string(), *s)).collect(),
    }
}

/// What the WM thread does with a logout signal ([`Logout::signal`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
    /// The first signal of a logout: the head start runs now.
    HeadStart,
    /// The logout is held already: nothing more (the wait counts from this one).
    Repeat,
    /// The logout was cancelled while held: the app starts afresh now.
    Resume,
    /// Nothing to do (a cancel with nothing held, a signal while standing down).
    Nothing,
}

impl Heard {
    /// Lower-case name, for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::HeadStart => "head start",
            Self::Repeat => "repeat",
            Self::Resume => "resume",
            Self::Nothing => "nothing",
        }
    }
}

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

/// The logout's head start (see the module docs): held from the first logout signal until
/// macOS's quit, a cancel signal, or [`LOGOUT_WAIT`] after the latest signal when neither
/// comes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Logout {
    /// When the head start ran; `None` when none is held.
    since: Option<Instant>,
    /// The latest signal of the logout held (the wait counts from it).
    last: Option<Instant>,
    /// The first signal of this logout, for the log (ms since it).
    first: Option<Instant>,
    /// Every window came back at the head start.
    all_back: bool,
}

impl Logout {
    /// A logout signal `signal` at `at`; `may_hold`: the head start may run (not while standing
    /// down: nothing is parked then, the quit does as before). Returns what to do and how many
    /// ms after the first signal of this logout it came (0 for the first). Any first signal
    /// starts the head start; later ones only push the wait back; a cancel while held resumes.
    pub fn signal(&mut self, signal: Signal, at: Instant, may_hold: bool) -> (Heard, u64) {
        // A first signal long gone (a logout cancelled while standing down) starts the count
        // afresh.
        let first = match self.first {
            Some(f) if self.since.is_some() || at.saturating_duration_since(f) < LOGOUT_WAIT => f,
            _ => at,
        };
        self.first = Some(first);
        let ms = at.saturating_duration_since(first).as_millis() as u64;
        let heard = match signal {
            Signal::Coming if self.since.is_some() => {
                self.last = Some(at);
                Heard::Repeat
            }
            Signal::Coming if may_hold => {
                self.since = Some(at);
                self.last = Some(at);
                self.all_back = false;
                Heard::HeadStart
            }
            Signal::Coming => Heard::Nothing,
            Signal::Cancelled => {
                let held = self.since.take().is_some();
                self.last = None;
                self.first = None;
                if held { Heard::Resume } else { Heard::Nothing }
            }
        };
        (heard, ms)
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

    /// True (once) when no quit and no cancel signal came within [`LOGOUT_WAIT`] of the
    /// latest signal at `now`: the logout was cancelled, the hold ends and the app starts
    /// afresh.
    pub fn cancelled(&mut self, now: Instant) -> bool {
        let Some(since) = self.since else { return false };
        if now.saturating_duration_since(self.last.unwrap_or(since)) >= LOGOUT_WAIT {
            self.since = None;
            self.last = None;
            self.first = None;
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
        assert_eq!(l.signal(Signal::Coming, t0, true), (Heard::HeadStart, 0));
        assert_eq!(l.signal(Signal::Coming, t0 + Duration::from_secs(1), true), (Heard::Repeat, 1000), "once");
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
        l.signal(Signal::Coming, t0, true);
        l.released(true);
        assert!(!l.cancelled(t0 + LOGOUT_WAIT - Duration::from_millis(1)));
        assert!(l.cancelled(t0 + LOGOUT_WAIT));
        assert!(!l.holding() && l.quit() == Quit::Unpark, "managing again: a later quit does everything");
        assert!(!l.cancelled(t0 + LOGOUT_WAIT * 2), "once");
        assert_eq!(l.signal(Signal::Coming, t0 + LOGOUT_WAIT * 3, true), (Heard::HeadStart, 0), "a later logout gets its own head start");
    }

    /// Round 8H: any first signal starts the head start; repeats are ignored (each only pushes
    /// the wait back) and timed from the first; a cancel resumes at once.
    #[test]
    fn the_first_signal_starts_it_repeats_are_ignored_and_a_cancel_resumes() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut l = Logout::default();
        // The 10-16 order, early names first this time: whichever comes first starts it.
        assert_eq!(l.signal(Signal::Coming, ms(0), true), (Heard::HeadStart, 0));
        assert!(l.holding());
        assert_eq!(l.signal(Signal::Coming, ms(30), true), (Heard::Repeat, 30), "another name for the same logout");
        assert_eq!(l.signal(Signal::Coming, ms(7_250), true), (Heard::Repeat, 7_250), "willPowerOff last: ignored");
        // The wait counts from the latest signal, not the first.
        assert!(!l.cancelled(ms(7_250) + LOGOUT_WAIT - Duration::from_millis(1)));
        assert_eq!(l.quit(), Quit::Done { all_back: false });
        // Cancelled: resumed at once, long before the wait.
        assert_eq!(l.signal(Signal::Cancelled, ms(9_000), true), (Heard::Resume, 9_000));
        assert!(!l.holding() && l.quit() == Quit::Unpark, "managing again");
        assert!(!l.cancelled(ms(9_000) + LOGOUT_WAIT), "nothing held: no second resume");
        assert_eq!(l.signal(Signal::Cancelled, ms(9_100), true), (Heard::Nothing, 0), "a second cancel does nothing");
        // A later logout is a new one, timed from its own first signal.
        assert_eq!(l.signal(Signal::Coming, ms(20_000), true), (Heard::HeadStart, 0));
        assert_eq!(l.signal(Signal::Coming, ms(20_004), true), (Heard::Repeat, 4));
    }

    #[test]
    fn a_signal_while_standing_down_holds_nothing() {
        let t0 = Instant::now();
        let mut l = Logout::default();
        assert_eq!(l.signal(Signal::Coming, t0, false), (Heard::Nothing, 0), "nothing parked: the quit does as before");
        assert!(!l.holding() && l.quit() == Quit::Unpark);
        assert_eq!(l.signal(Signal::Coming, t0 + Duration::from_millis(5), false), (Heard::Nothing, 5), "still timed for the log");
        // Long after, a new logout's first signal counts from itself.
        assert_eq!(l.signal(Signal::Coming, t0 + LOGOUT_WAIT * 2, true), (Heard::HeadStart, 0));
    }

    #[test]
    fn every_signal_is_listened_for_or_only_the_tests_note() {
        let all = signals_to_observe(None);
        assert_eq!(all.len(), SIGNALS.len());
        for name in ["com.apple.logoutInitiated", "com.apple.restartInitiated", "com.apple.shutdownInitiated", "com.apple.logoutContinued"] {
            assert!(all.contains(&(Channel::Distributed, name.to_string(), Signal::Coming)), "{name}");
        }
        assert!(all.contains(&(Channel::Distributed, "com.apple.logoutCancelled".to_string(), Signal::Cancelled)));
        assert!(all.contains(&(Channel::Notify, "com.apple.system.loginwindow.logoutcancelled".to_string(), Signal::Cancelled)));
        assert_eq!(all.iter().filter(|s| s.0 == Channel::Notify).count(), 4);
        assert_eq!(all.last(), Some(&(Channel::Workspace, "NSWorkspaceWillPowerOffNotification".to_string(), Signal::Coming)), "the last fallback");
        let test = signals_to_observe(Some("dev.example.test.logout"));
        assert_eq!(
            test,
            vec![
                (Channel::Distributed, "dev.example.test.logout".to_string(), Signal::Coming),
                (Channel::Distributed, "dev.example.test.logout.cancelled".to_string(), Signal::Cancelled),
            ],
            "the test's own names instead of Apple's"
        );
        assert_eq!(signals_to_observe(Some("  ")).len(), SIGNALS.len(), "an empty note is no note");
    }

    #[test]
    fn a_quick_flip_releases_and_resumes_in_order() {
        let mut s = StandDown::default();
        let steps: Vec<Step> = [true, false, true, false].into_iter().map(|on| s.apple(on)).collect();
        assert_eq!(steps, vec![Step::Release, Step::Resume, Step::Release, Step::Resume]);
    }
}
