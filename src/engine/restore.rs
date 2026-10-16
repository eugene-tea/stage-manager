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
//! **Settling** (round 8F): after a restart the restore window is also when the login burst
//! settles. Launch-time activations, focus changes and new windows switch no stage and
//! animate nothing ([`crate::engine::model::Model::settling`]); cards come in quietly. Besides
//! a user action (a click of any button, a key press) and [`WINDOW`], it ends once apps have
//! been quiet for [`SETTLE_QUIET`] and at least [`SETTLE_AFTER_LOGIN`] after the login
//! ([`Settling`]). Quiet: no app launched, no window back from a saved entry, and no app with
//! saved windows still opening (running with no window yet: Xcode or an Electron app may take
//! 30-90 s to show its first, [`still_opening`]). A screen lock during the burst (a login that
//! locks at once) defers the end until the unlock: nothing is done for a screen nobody sees.
//!
//! A window matching no saved one (or a doubtful match) still goes by Apple's rule while the
//! login settles, only without the switch: into its app's stage, or a stage of its own with
//! its card. One exception: while the saved active stage is still empty, its windows still to
//! come, a window that opens a stage of its own takes the front (nothing is shown there to
//! animate away; the screen is not left empty while a slow app opens), and the saved stage
//! waits, inactive and with no card; its windows, when they come, are parked there and its
//! card shows. At the end the stage in front shows and its key window takes the keyboard
//! unless a window shown there has it already
//! ([`crate::engine::model::Model::focus_after_restore`]), and one line sums the restore up
//! ([`Summary`]).
//!
//! **One pool** (round 8I): after a restart a window that shows on another Desktop than the one
//! it was saved on (non-native apps reopen on the Desktop shown) goes into its saved stage made
//! again where it shows (`model/pool.rs`); "K from other Desktops" in the line.
//!
//! **Frames** come back from `state.json` only on a main display of the saved size
//! ([`Restore::saved_display`]); on another one a window keeps the frame its app gave it.
//!
//! **Off** (`[stages] restore_after_restart = false`): after a restart nothing waits
//! ([`Restore::off_after_restart`]): saved windows not on screen are dropped as 1.2.0 did.
//!
//! **Accepted:** an app that opens a window by itself, with no user action, during the
//! restore window (the first five minutes after a launch, until the first click or key
//! press) has a window matching a saved one in an inactive stage parked there (a card),
//! not switched to; the restore cannot tell it from the app reopening its saved windows.

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use super::geometry::Rect;
use super::model::StageId;
use super::park::Corner;

/// An app this long without a new window (after its first) has settled: its pending
/// windows are given up on.
pub const QUIET: Duration = Duration::from_secs(60);

/// How long before the last save an app's quit may come for its windows to wait in their
/// stages at the next session (round 8H): a logout's apps quit within seconds of the last
/// save; an app the user quit earlier is not reopened by a login.
pub const QUIT_KEEP: Duration = Duration::from_secs(120);

/// The restore window's longest life after the launch (the design's cap).
pub const WINDOW: Duration = Duration::from_secs(5 * 60);

/// Settling ends once apps have been this quiet (no launch, no window back from a saved
/// entry)...
pub const SETTLE_QUIET: Duration = Duration::from_secs(20);

/// ...and no sooner than this after the login (apps are slow to reopen windows at login).
pub const SETTLE_AFTER_LOGIN: Duration = Duration::from_secs(60);

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
    /// `[stages] restore_after_restart = false` after a restart: saved windows not on screen
    /// are not kept pending; their stages go as in 1.2.0.
    pub drop_missing: bool,
    /// The main display's size (points, rounded) when `state.json` was saved, if it says:
    /// saved frames are used only on a main display of this size.
    pub saved_display: Option<(i64, i64)>,
    /// Why saved frames are not used was logged already (once a run).
    pub frames_noted: bool,
    /// Windows placed by Apple's rule while the restore window was open (they matched no
    /// saved window), for the restore's line.
    pub by_apple: usize,
    /// Of those, the ones macOS reopened clamped against the park corner (an app that saved
    /// its frame while parked; round 8H): left where they are, counted for the line.
    pub corner: usize,
    /// Saved active stages that kept their place while empty, their windows still to come:
    /// emptied for good (given up), they still give way to a stage with windows
    /// ([`crate::engine::model::Model::front_after_restore`]).
    pub front_waits: Vec<StageId>,
    /// The Desktop shown when saved (its id) and its active stage's index in the file: the
    /// stage in front when the restore window ends by itself after a restart, if any of its
    /// windows came back ([`crate::engine::model::Model::front_of_saved_current`]).
    pub saved_front: Option<(u64, usize)>,
}

impl Restore {
    /// A launch in another window server session than the file's, now `session`.
    pub fn after_restart(session: &str) -> Self {
        Self { session: session.to_string(), restoring: true, new_session: true, ..Self::default() }
    }

    /// A launch in the file's own session (or one that cannot be told apart), now `session`.
    pub fn same_session(session: &str) -> Self {
        Self { session: session.to_string(), restoring: true, new_session: false, ..Self::default() }
    }

    /// A launch after a restart with `[stages] restore_after_restart = false`: no restore
    /// window, nothing kept pending (1.2.0's behaviour), now `session`.
    pub fn off_after_restart(session: &str) -> Self {
        Self { session: session.to_string(), drop_missing: true, ..Self::default() }
    }

    /// The same, with the main display's size when saved (`w`×`h` points).
    pub fn with_display(mut self, saved: Option<(f64, f64)>) -> Self {
        self.saved_display = saved.filter(|(w, h)| w.is_finite() && h.is_finite()).map(|(w, h)| (w.round() as i64, h.round() as i64));
        self
    }

    /// True while a Desktop made from an empty screen is provisional and an empty saved
    /// active stage holds its place: the restore window after a restart.
    pub fn after_restart_window(&self) -> bool {
        self.restoring && self.new_session
    }

    /// True when saved frames apply on a main display `w`×`h` points: the file's display had
    /// that size, or the file does not say.
    pub fn frames_fit(&self, w: f64, h: f64) -> bool {
        self.saved_display.is_none_or(|d| d == (w.round() as i64, h.round() as i64))
    }
}

/// Why the restore window ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// A click, a key press, a card click, a hotkey, a ctl command that acts: the user's
    /// focus and stage stand.
    User,
    /// ⌘Tab or ⌘`: the app switch it starts brings its own stage (nothing comes forward on
    /// our own).
    Switcher,
    /// Apps went quiet ([`SETTLE_QUIET`]).
    Quiet,
    /// [`WINDOW`] after the launch.
    TimeUp,
}

impl End {
    /// True when the end is no user's doing: the saved key window takes the keyboard.
    pub fn by_itself(self) -> bool {
        matches!(self, End::Quiet | End::TimeUp)
    }

    /// True when a stage with windows may come forward for an empty saved active stage
    /// (not when an app switch is on its way: its own switch wins).
    pub fn brings_front(self) -> bool {
        self != End::Switcher
    }
}

/// The clock of the restore window's ends by itself (pure: the platform feeds it times).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settling {
    /// The launch.
    started: Instant,
    /// The login (the launch when unknown, or a login said to be later than the launch).
    login: Instant,
    /// The last app launch or window back from a saved entry (the launch at first).
    busy: Instant,
}

impl Settling {
    /// Opens at `launch`; `login`: when the login session began, if known.
    pub fn new(launch: Instant, login: Option<Instant>) -> Self {
        let login = login.filter(|l| *l <= launch).unwrap_or(launch);
        Self { started: launch, login, busy: launch }
    }

    /// An app launched, or a window went back to its saved stage, at `at`.
    pub fn busy(&mut self, at: Instant) {
        self.busy = self.busy.max(at);
    }

    /// Whether the window ends by itself at `now`, and why.
    pub fn due(&self, now: Instant) -> Option<End> {
        if now.saturating_duration_since(self.started) >= WINDOW {
            Some(End::TimeUp)
        } else if now.saturating_duration_since(self.busy) >= SETTLE_QUIET
            && now.saturating_duration_since(self.login) >= SETTLE_AFTER_LOGIN
        {
            Some(End::Quiet)
        } else {
            None
        }
    }

    /// How long since the launch, at `now`.
    pub fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
    }
}

/// True when an app of `pending` (apps with saved windows still waiting) is running with no
/// window yet (`running`, `has_window`: one managed or one waiting to show): it is still
/// opening (Xcode or an Electron app may take 30-90 s to show its first window), so the
/// login is not quiet: the platform keeps [`Settling`] busy while it is, [`WINDOW`] still
/// bounding it. An app not running has nothing on its way.
pub fn still_opening<'a>(
    pending: impl IntoIterator<Item = &'a str>,
    running: impl Fn(&str) -> bool,
    has_window: impl Fn(&str) -> bool,
) -> bool {
    pending.into_iter().any(|b| running(b) && !has_window(b))
}

/// The restore summed up in one line, no titles (the log at its end, `log summary`):
/// "restore: 9 windows into 4 stages on 2 Desktops in 41 s; 2 pending; 1 by Apple's rule; 0 at
/// the corner; 3 from other Desktops".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Windows back from a saved entry.
    pub windows: usize,
    /// The stages they went into.
    pub stages: usize,
    /// The Desktops of those.
    pub desktops: usize,
    /// Seconds from the launch to the restore window's end.
    pub seconds: u64,
    /// Saved windows still waiting (all Desktops).
    pub pending: usize,
    /// Windows that matched no saved one, placed by Apple's rule meanwhile.
    pub by_apple: usize,
    /// Of those, the ones clamped against the park corner ([`clamped_at_corner`]).
    pub corner: usize,
    /// Windows back from a saved entry of another Desktop than the one they showed on (the
    /// pool after a restart, round 8I).
    pub pooled: usize,
}

/// True when `frame` is clamped against the park corner `corner` of the visible frame
/// `visible`: both of the corner's edges touch it within [`CORNER_TOUCH`] points (macOS
/// reopened a window whose app saved a parked frame, squeezed onto the screen; the 10-16
/// restart). Not a parked frame: the caller tells those apart first.
pub fn clamped_at_corner(frame: Rect, visible: Rect, corner: Corner) -> bool {
    if frame.is_degenerate() || visible.is_degenerate() {
        return false;
    }
    let near = |a: f64, b: f64| (a - b).abs() <= CORNER_TOUCH;
    let side = if corner.right() { near(frame.max_x(), visible.max_x()) } else { near(frame.x, visible.x) };
    let end = if corner.bottom() { near(frame.max_y(), visible.max_y()) } else { near(frame.y, visible.y) };
    side && end
}

/// How close (points) a window's edges must be to the visible frame's to count as clamped
/// against the corner.
pub const CORNER_TOUCH: f64 = 2.0;

/// What [`Summary`]'s line starts with.
pub const SUMMARY_PREFIX: &str = "restore: ";

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = |n: usize, one: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") };
        write!(
            f,
            "{SUMMARY_PREFIX}{} into {} on {} in {} s; {} pending; {} by Apple's rule; {} at the corner; {} from other Desktops",
            n(self.windows, "window"),
            n(self.stages, "stage"),
            n(self.desktops, "Desktop"),
            self.seconds,
            self.pending,
            self.by_apple,
            self.corner,
            self.pooled
        )
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
    fn settling_ends_after_a_quiet_spell_past_the_login_minute() {
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        // Launched 5 s after the login: quiet from the start, but not before a minute after it.
        let c = Settling::new(s(5), Some(t0));
        assert_eq!(c.due(s(30)), None, "quiet 25 s, but only 30 s after the login");
        assert_eq!(c.due(s(60)), Some(End::Quiet));
        // Apps launching (or windows coming back) push the quiet end on.
        let mut c = Settling::new(s(5), Some(t0));
        c.busy(s(50));
        assert_eq!(c.due(s(69)), None);
        assert_eq!(c.due(s(70)), Some(End::Quiet));
        c.busy(s(40));
        assert_eq!(c.due(s(70)), Some(End::Quiet), "an earlier event never moves the clock back");
        // Busy all along: the cap.
        let mut c = Settling::new(s(5), Some(t0));
        for k in (10..400).step_by(10) {
            c.busy(s(k));
        }
        assert_eq!(c.due(s(5) + WINDOW - Duration::from_secs(1)), None);
        assert_eq!(c.due(s(5) + WINDOW), Some(End::TimeUp));
        // Login unknown (or said to be after the launch): the launch stands for it.
        let c = Settling::new(s(100), None);
        assert_eq!(c.due(s(120)), None, "20 s quiet, but a minute after the launch is needed");
        assert_eq!(c.due(s(160)), Some(End::Quiet));
        assert_eq!(Settling::new(s(100), Some(s(500))).due(s(160)), Some(End::Quiet));
        // A relaunch long after the login: quiet 20 s is enough.
        let c = Settling::new(s(4000), Some(t0));
        assert_eq!(c.due(s(4019)), None);
        assert_eq!(c.due(s(4020)), Some(End::Quiet));
        assert_eq!(c.age(s(4020)), Duration::from_secs(20));
    }

    #[test]
    fn an_app_still_opening_its_first_window_keeps_the_login_busy() {
        // The review of 8F (M2): Xcode running 80 s with no window yet; settling would have
        // ended at 60 s and its windows then switched stages after the end.
        let pending = ["com.example.ide", "com.example.chat", "com.example.gone"];
        let running = |b: &str| b != "com.example.gone";
        let mut windows = vec!["com.example.chat"];
        assert!(still_opening(pending, running, |b| windows.contains(&b)), "the IDE is running, no window yet");
        windows.push("com.example.ide");
        assert!(!still_opening(pending, running, |b| windows.contains(&b)), "every running app has a window; one not running counts for nothing");
        assert!(!still_opening([], running, |_| false), "nothing pending");

        // The platform's tick: busy while the IDE opens, then quiet 20 s after its window.
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        let mut c = Settling::new(t0, Some(t0));
        let mut ended = None;
        for k in 0..=400 {
            let now = s(k);
            let ide_window = k >= 80;
            if still_opening(["com.example.ide"], |_| true, |_| ide_window) {
                c.busy(now);
            }
            if let Some(end) = c.due(now) {
                ended = Some((k, end));
                break;
            }
        }
        assert_eq!(ended, Some((99, End::Quiet)), "busy until 79 s, quiet 20 s later");

        // An app that never shows a window: the cap still ends it.
        let mut c = Settling::new(t0, Some(t0));
        let mut ended = None;
        for k in 0..=400 {
            let now = s(k);
            if still_opening(["com.example.ide"], |_| true, |_| false) {
                c.busy(now);
            }
            if let Some(end) = c.due(now) {
                ended = Some((k, end));
                break;
            }
        }
        assert_eq!(ended, Some((300, End::TimeUp)), "five minutes at most");
    }

    #[test]
    fn how_the_window_ended_decides_the_focus_and_the_front() {
        assert!(End::Quiet.by_itself() && End::TimeUp.by_itself());
        assert!(!End::User.by_itself() && !End::Switcher.by_itself(), "the user's focus is never taken");
        assert!(End::User.brings_front() && End::Quiet.brings_front() && End::TimeUp.brings_front());
        assert!(!End::Switcher.brings_front(), "an app switch brings its own stage");
    }

    #[test]
    fn saved_frames_only_on_a_display_of_the_saved_size() {
        let r = Restore::after_restart("s").with_display(Some((2560.0, 1440.0)));
        assert!(r.frames_fit(2560.0, 1440.0));
        assert!(r.frames_fit(2560.2, 1439.8), "rounded points");
        assert!(!r.frames_fit(1512.0, 982.0));
        assert!(Restore::after_restart("s").frames_fit(1512.0, 982.0), "a file that does not say: as before");
        assert!(Restore::after_restart("s").with_display(Some((f64::NAN, 1.0))).frames_fit(1.0, 1.0));
    }

    #[test]
    fn the_summary_line_has_counts_and_no_titles() {
        let s = Summary { windows: 9, stages: 4, desktops: 2, seconds: 41, pending: 2, by_apple: 1, corner: 1, pooled: 3 };
        assert_eq!(
            s.to_string(),
            "restore: 9 windows into 4 stages on 2 Desktops in 41 s; 2 pending; 1 by Apple's rule; 1 at the corner; 3 from other Desktops"
        );
        let one = Summary { windows: 1, stages: 1, desktops: 1, seconds: 0, pending: 0, by_apple: 0, corner: 0, pooled: 0 };
        assert_eq!(
            one.to_string(),
            "restore: 1 window into 1 stage on 1 Desktop in 0 s; 0 pending; 0 by Apple's rule; 0 at the corner; 0 from other Desktops"
        );
    }

    #[test]
    fn off_after_a_restart_keeps_nothing_waiting() {
        let r = Restore::off_after_restart("s");
        assert!(!r.restoring && !r.new_session && r.drop_missing && !r.after_restart_window());
        assert!(!Restore::after_restart("s").drop_missing && !Restore::same_session("s").drop_missing);
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
