//! Mission Control and App Exposé with every stage's windows (round spaces-mission-control,
//! the Spaces engine).
//!
//! Under the Spaces engine a hidden stage's windows sit in a window-server Space that is on
//! no Desktop, and Mission Control lays out only the windows of the Desktop: it showed the
//! stage in front and nothing else (the owner, 11-18). Apple's Stage Manager shows every
//! stage's windows there, and so does the Corner engine, whose hidden windows are on their
//! Desktop, parked at the screen's corner.
//!
//! So while Mission Control or App Exposé shows, the hidden windows of the Desktops shown are
//! put on their Desktop the way ⌘Tab's switcher and a Dock press put one there
//! ([`super::lend_park`]): moved to the park corner while still in their stage Space, then
//! put home, still counted hidden. When it closes they go back into their stage Spaces,
//! their frames set again; a window picked there is activated by macOS, and the activation's
//! switch brings its stage in front as after a Dock press.
//!
//! **When** ([`Mode`]). Nothing public says that Mission Control is about to open. What
//! there is:
//!
//! - its shortcuts, heard by the input monitor as they are pressed ([`trigger`]: the user's
//!   own from Keyboard Shortcuts, and the keyboard's Mission Control key);
//! - a notice of the Dock's, which draws it ([`Hint::Dock`]; whether one comes as it opens
//!   is not measured: the log says which hint came before it showed, and how long before);
//! - macOS's own window over the display once it shows ([`super::overview`]), found by a
//!   look at the window list.
//!
//! A gesture, a hot corner and the Dock's Mission Control item give no hint at all. So the
//! lend is made when Mission Control is seen showing ([`Mode::Shown`], the default: nothing
//! moves on a guess), and a hint only makes the looks come at the fast tick for
//! [`Hint::watch`], so that it is seen within a few tens of ms of the key instead of at the
//! next tick. [`Mode::Key`] also lends at the shortcut's key itself, before anything shows;
//! with no Mission Control within [`KEY_WAIT`] those windows go back.
//!
//! Whether Mission Control lays out a window that arrives on the Desktop after it opened is
//! not known offline. The platform reads the window list [`MEASURE_AFTER`] after the lend
//! and says in the log how many of the windows put home Mission Control moved from their
//! park ([`laid_out`]); `scripts/test-stages.sh mission-probe` measures the same on fixture
//! windows, lent before the key, after it shows, and at the key.
//!
//! **Cost.** Each lend is one bounded move through the window's app (its park) and two
//! window-server calls. They are made in batches of [`BATCH`] a fast tick, most recent stage
//! first ([`hidden_here`]), so the WM thread answers between them; a window whose park does
//! not take is not put home (it would show at its frame), and stays hidden.
//!
//! **Never left out.** The engine counts every window put home as lent and parked: a hide,
//! a show, a sweep (quit, a stand-down, a fallback) and the guardian's scan of the park
//! corner bring each back as they do ⌘Tab's ([`super::lend_park`]). [`Lends`] only decides
//! when; what is out is the engine's to know.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::geometry::Rect;
use super::model::{Model, Pid, WindowId, WindowRole};

/// How long the looks for Mission Control come at the fast tick after one of its keys.
pub const KEY_WATCH: Duration = Duration::from_millis(800);

/// The same after a notice of the Dock's (it sends the same notice for ⌘Tab's switcher and
/// its menus, so the watch is short).
pub const DOCK_WATCH: Duration = Duration::from_millis(300);

/// The pause between two looks of a watch.
pub const WATCH_EVERY: Duration = Duration::from_millis(30);

/// A hint counts as what announced Mission Control when it shows within this long.
pub const HINT_COUNTS: Duration = Duration::from_millis(2000);

/// What one fast tick spends on lends, or on take backs, before the WM thread goes back to
/// its events.
pub const BATCH: Duration = Duration::from_millis(40);

/// Windows put home at a key ([`Mode::Key`]) go back when no Mission Control shows this long
/// after it (the shortcut was another app's, or macOS did not open it).
pub const KEY_WAIT: Duration = Duration::from_millis(1500);

/// A window of the app in front still counted hidden stays out this long after Mission
/// Control closed: picked there, its app is activated and the activation's switch shows it;
/// taken back first, the app's focus on a window off every Desktop could take macOS to
/// another Desktop ([`super::spaces::macos_counts_hidden`]).
pub const PICK_WAIT: Duration = Duration::from_millis(1500);

/// How long after the lend (and after Mission Control showed) the window list is read for
/// what Mission Control did with the windows put home ([`laid_out`]): its layout animates
/// for about half a second.
pub const MEASURE_AFTER: Duration = Duration::from_millis(700);

/// How long a park's move may wait for the window's app (seconds): parks took 5 to 17 ms
/// (93 lends for ⌘Tab, 11-15 to 11-18); a busy app's window stays hidden.
pub const PARK_SECONDS: f32 = 0.05;

/// The keyboard's own Mission Control key (F3 on Apple keyboards), as a key code.
pub const MISSION_CONTROL_KEY: u16 = 160;

/// A listed frame this far (points) from where a window was parked, or this much another
/// size, was moved by Mission Control ([`laid_out`]).
const MOVED: f64 = 8.0;

/// `[windows] mission_control_stages`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// Nothing: Mission Control shows the stage in front only.
    Off,
    /// The hidden windows are put home once Mission Control is seen showing.
    #[default]
    Shown,
    /// Also at its shortcut's key, before it shows.
    Key,
}

impl Mode {
    /// The config's word for it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shown => "shown",
            Self::Key => "key",
        }
    }

    /// The value for the config's `word` (the default for anything else: the schema has
    /// refused it already).
    pub fn parse(word: &str) -> Self {
        Self::named(word).unwrap_or_default()
    }

    /// The mode `word` names, if it names one.
    pub fn named(word: &str) -> Option<Self> {
        match word.trim() {
            "off" => Some(Self::Off),
            "shown" => Some(Self::Shown),
            "key" => Some(Self::Key),
            _ => None,
        }
    }
}

/// Which of macOS's overviews a shortcut opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Mission Control.
    MissionControl,
    /// App Exposé ("Application windows").
    AppWindows,
}

impl Trigger {
    /// Its name for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::MissionControl => "Mission Control's key",
            Self::AppWindows => "App Exposé's key",
        }
    }
}

/// The modifiers held with a key, or asked by a shortcut. Shift is left out: with it macOS
/// opens the same overview slowly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    /// ⌃ held.
    pub control: bool,
    /// ⌥ held.
    pub option: bool,
    /// ⌘ held.
    pub command: bool,
}

impl Mods {
    /// From modifier flags as `NSEvent`, `CGEvent` and `com.apple.symbolichotkeys` have them
    /// (the same bits in all three).
    pub fn from_flags(flags: u64) -> Self {
        Self { control: flags & 0x4_0000 != 0, option: flags & 0x8_0000 != 0, command: flags & 0x10_0000 != 0 }
    }
}

/// One shortcut as the user's settings have it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    /// Its key code.
    pub keycode: u16,
    /// Its modifiers.
    pub mods: Mods,
    /// Turned on in Keyboard Shortcuts.
    pub enabled: bool,
}

/// The two shortcuts that open an overview with windows in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    /// "Mission Control" (⌃↑ by default).
    pub mission_control: Shortcut,
    /// "Application windows" (⌃↓ by default).
    pub app_windows: Shortcut,
}

/// True when a key-down could be one of the shortcuts at all: held with ⌃, ⌥ or ⌘, or a
/// function, arrow or special key. Typing never is, so the settings are not read for it.
pub fn may_trigger(keycode: u16, mods: Mods) -> bool {
    mods.control || mods.option || mods.command || keycode >= 96
}

/// The overview a key-down opens, if it is one of the shortcuts: the user's own, or the
/// keyboard's Mission Control key ([`MISSION_CONTROL_KEY`]: alone Mission Control, with ⌃
/// App Exposé, with ⌘ Show Desktop, which has no windows to show).
pub fn trigger(keys: &Keys, keycode: u16, mods: Mods) -> Option<Trigger> {
    if keycode == MISSION_CONTROL_KEY {
        return match (mods.command, mods.control) {
            (true, _) => None,
            (false, true) => Some(Trigger::AppWindows),
            (false, false) => Some(Trigger::MissionControl),
        };
    }
    let is = |s: &Shortcut| s.enabled && s.keycode == keycode && s.mods == mods;
    if is(&keys.mission_control) {
        Some(Trigger::MissionControl)
    } else if is(&keys.app_windows) {
        Some(Trigger::AppWindows)
    } else {
        None
    }
}

/// What said that Mission Control may be about to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    /// One of its shortcuts was pressed.
    Key(Trigger),
    /// The Dock sent a notice (it draws Mission Control; also ⌘Tab's switcher and its menus).
    Dock,
}

impl Hint {
    /// Its name for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::Key(t) => t.name(),
            Self::Dock => "a notice of the Dock's",
        }
    }

    /// How long the looks come at the fast tick after it.
    pub fn watch(self) -> Duration {
        match self {
            Self::Key(_) => KEY_WATCH,
            Self::Dock => DOCK_WATCH,
        }
    }
}

/// Why a round of lends is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// Mission Control was seen showing.
    Shown,
    /// One of its keys was pressed ([`Mode::Key`]).
    Key,
    /// A test asked (`ctl debug-mission-control lend`): kept out until it asks them back.
    Asked,
}

impl By {
    /// Its name for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::Shown => "Mission Control showed",
            Self::Key => "its key was pressed",
            Self::Asked => "a test asked",
        }
    }
}

/// The hidden standard windows of the Desktops shown, in the order they are put home: the
/// most recently used stage first, each stage's windows front to back. `skip`: windows left
/// alone (one put home already for a Dock press or ⌘Tab's switcher).
pub fn hidden_here(m: &Model, skip: impl Fn(WindowId) -> bool) -> Vec<WindowId> {
    let mut stages: Vec<_> = m.stages().iter().filter(|s| m.is_shown_space(s.space) && !m.stage_in_front(s.id)).collect();
    stages.sort_by_key(|s| std::cmp::Reverse(s.last_active));
    stages
        .into_iter()
        .flat_map(|s| s.windows.iter().copied())
        .filter(|id| m.window(*id).is_some_and(|w| w.parked && !w.sticky && w.meta.role == WindowRole::Standard))
        .filter(|id| !skip(*id))
        .collect()
}

/// True when Mission Control moved a window put home: listed away from where it was listed
/// parked, or at another size (its layout scales the windows down). Left at its park, it
/// was not taken into the layout.
pub fn laid_out(listed: Rect, parked: Rect) -> bool {
    (listed.x - parked.x).abs() > MOVED
        || (listed.y - parked.y).abs() > MOVED
        || (listed.w - parked.w).abs() > MOVED
        || (listed.h - parked.h).abs() > MOVED
}

/// A window put home for Mission Control.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Out {
    /// The window.
    pub id: WindowId,
    /// Its app.
    pub pid: Pid,
    /// Where the window server listed it once parked and home (`None`: not listed then).
    pub parked: Option<Rect>,
}

/// What becomes of a window put home once Mission Control has closed ([`fate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// The model shows it, or its stage is in front: the switch has it (it was picked).
    Forget,
    /// Its app is in front and its stage is not yet: the activation's switch may still come.
    Wait,
    /// Back into its stage Space.
    TakeBack,
}

/// What becomes of one window put home, `since_close` after Mission Control closed: `shown`
/// when the model no longer counts it hidden or its stage is in front, `front_app` when its
/// app is the one in front ([`PICK_WAIT`]).
pub fn fate(shown: bool, front_app: bool, since_close: Duration) -> Fate {
    if shown {
        Fate::Forget
    } else if front_app && since_close < PICK_WAIT {
        Fate::Wait
    } else {
        Fate::TakeBack
    }
}

/// How a window put home left the list ([`Lends::settle`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// Its stage came in front: picked in Mission Control (or switched to otherwise).
    Picked,
    /// Back in its stage Space.
    Back,
    /// It did not go back (shown or hidden since by something else, or the call missed: the
    /// tick's check of hidden windows has it then).
    Missed,
}

/// What a look at the window list changed ([`Lends::saw`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Mission Control shows now; the hint that came before it, and how long before.
    Shown(Option<(Hint, Duration)>),
    /// It closed, after this long.
    Closed(Duration),
}

/// A round of lends, as the log says it once its last window is out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundDone {
    /// Why it was made.
    pub by: By,
    /// The hidden windows there were.
    pub asked: usize,
    /// Those put home.
    pub lent: usize,
    /// From its first lend to its last.
    pub took: Duration,
    /// Of that, in the parks' moves.
    pub park_ms: u64,
}

/// The take backs after a close, as the log says them once no window is out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackDone {
    /// Windows back in their stage Spaces.
    pub back: usize,
    /// Windows whose stage came in front meanwhile.
    pub picked: Vec<WindowId>,
    /// Windows that did not go back.
    pub missed: Vec<WindowId>,
    /// Since Mission Control closed.
    pub took: Duration,
}

#[derive(Debug, Clone, Copy)]
struct Round {
    by: By,
    started: Instant,
    asked: usize,
    lent: usize,
    park_ms: u64,
    /// When its last window was out (the round said in the log).
    done: Option<Instant>,
}

/// Follows Mission Control for the lends: when a look for it is due, when the hidden windows
/// are put home, which are out, and when they go back.
#[derive(Debug, Default)]
pub struct Lends {
    mode: Mode,
    /// The latest hint, until Mission Control shows or its time is up.
    hint: Option<(Hint, Instant)>,
    /// The watch's last look.
    looked: Option<Instant>,
    /// Mission Control shows, since when.
    up: Option<Instant>,
    /// It closed (or a key's guess ended) with windows out, when: they go back.
    closed: Option<Instant>,
    /// A round is wanted and not planned yet.
    wanted: Option<By>,
    /// The key a [`By::Key`] round was made at, until Mission Control shows or [`KEY_WAIT`].
    guess: Option<Instant>,
    /// The windows still to put home.
    queue: VecDeque<WindowId>,
    round: Option<Round>,
    /// The windows put home.
    out: Vec<Out>,
    /// The layout was read for this Mission Control.
    measured: bool,
    back: usize,
    picked: Vec<WindowId>,
    missed: Vec<WindowId>,
}

impl Lends {
    /// The mode as the config has it now. Turned off, nothing new is wanted; what is out
    /// still goes back at the close.
    pub fn set_mode(&mut self, mode: Mode) {
        if mode == Mode::Off && self.mode != Mode::Off {
            self.wanted = None;
            self.queue.clear();
            self.hint = None;
        }
        self.mode = mode;
    }

    /// The mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// A hint at `now`. True when it is taken (the looks come at the fast tick for its
    /// watch; false for a key's repeats and for anything while Mission Control shows); in
    /// [`Mode::Key`] one of the keys also wants a round at once.
    pub fn hint(&mut self, hint: Hint, now: Instant) -> bool {
        if self.mode == Mode::Off || self.up.is_some() {
            return false;
        }
        // A key stands for its watch: not replaced by the Dock's notice that follows it, nor
        // by its own repeats while it is held.
        if self.hint.is_some_and(|(h, t)| matches!(h, Hint::Key(_)) && now.saturating_duration_since(t) < h.watch()) {
            return false;
        }
        self.hint = Some((hint, now));
        if self.mode == Mode::Key && matches!(hint, Hint::Key(_)) && self.round.is_none() && self.out.is_empty() {
            self.wanted = Some(By::Key);
            self.guess = Some(now);
        }
        true
    }

    /// True when the watch wants a look at the window list at `now` (asked at the fast
    /// tick; the look is then counted made).
    pub fn look_due(&mut self, now: Instant) -> bool {
        let watching = self.hint.is_some_and(|(h, t)| now.saturating_duration_since(t) < h.watch()) || self.guess.is_some();
        if !watching || self.up.is_some() || self.looked.is_some_and(|t| now.saturating_duration_since(t) < WATCH_EVERY) {
            return false;
        }
        self.looked = Some(now);
        true
    }

    /// What a look at the window list at `now` says: Mission Control or App Exposé `showing`
    /// or not.
    pub fn saw(&mut self, showing: bool, now: Instant) -> Option<Edge> {
        match (showing, self.up) {
            (true, None) => {
                self.up = Some(now);
                self.measured = false;
                self.guess = None;
                let hint = self.hint.take().map(|(h, t)| (h, now.saturating_duration_since(t))).filter(|(_, d)| *d < HINT_COUNTS);
                if self.mode != Mode::Off {
                    // Windows still out from the last one stay out for this one.
                    self.closed = None;
                    if self.round.is_none() {
                        self.wanted = Some(By::Shown);
                    }
                }
                Some(Edge::Shown(hint))
            }
            (false, Some(since)) => {
                self.up = None;
                self.wanted = None;
                self.queue.clear();
                // A test's round waits for its own back; everything else out goes back.
                if !self.round.is_some_and(|r| r.by == By::Asked) {
                    self.round = None;
                    if !self.out.is_empty() {
                        self.closed = Some(now);
                    }
                }
                Some(Edge::Closed(now.saturating_duration_since(since)))
            }
            _ => None,
        }
    }

    /// True once, when windows were put home at a key and no Mission Control showed within
    /// [`KEY_WAIT`]: they go back.
    pub fn guess_over(&mut self, now: Instant) -> bool {
        if self.up.is_some() || !self.guess.is_some_and(|t| now.saturating_duration_since(t) >= KEY_WAIT) {
            return false;
        }
        self.guess = None;
        self.wanted = None;
        self.queue.clear();
        self.round = None;
        self.closed = Some(now);
        true
    }

    /// The round wanted and not planned yet.
    pub fn wanted(&self) -> Option<By> {
        self.wanted
    }

    /// A test's round, whatever the mode (`ctl debug-mission-control lend`).
    pub fn ask(&mut self) {
        self.wanted = Some(By::Asked);
        self.closed = None;
    }

    /// A test asks its windows back (`ctl debug-mission-control back`), or everything out
    /// goes back now (the mode turned off, the engine left).
    pub fn ask_back(&mut self, now: Instant) {
        self.wanted = None;
        self.guess = None;
        self.queue.clear();
        self.round = None;
        // Long closed: nothing waits for a pick.
        self.closed = Some(now.checked_sub(PICK_WAIT).unwrap_or(now));
    }

    /// The round wanted is planned at `now`: `windows` to put home, in order
    /// ([`hidden_here`]); those out already are left out of it.
    pub fn plan(&mut self, windows: Vec<WindowId>, now: Instant) {
        let Some(by) = self.wanted.take() else { return };
        self.queue = windows.into_iter().filter(|id| !self.out.iter().any(|o| o.id == *id)).collect();
        self.round = Some(Round { by, started: now, asked: self.queue.len(), lent: 0, park_ms: 0, done: None });
    }

    /// The next window to put home.
    pub fn next(&mut self) -> Option<WindowId> {
        self.queue.pop_front()
    }

    /// A window was put home; its park's moves took `park_ms`.
    pub fn lent(&mut self, out: Out, park_ms: u64) {
        if let Some(r) = self.round.as_mut() {
            r.lent += 1;
            r.park_ms += park_ms;
        }
        self.out.retain(|o| o.id != out.id);
        self.out.push(out);
    }

    /// The round's summary, once: its last window is out (or was not put home).
    pub fn round_done(&mut self, now: Instant) -> Option<RoundDone> {
        if !self.queue.is_empty() {
            return None;
        }
        let r = self.round.as_mut().filter(|r| r.done.is_none())?;
        r.done = Some(now);
        Some(RoundDone { by: r.by, asked: r.asked, lent: r.lent, took: now.saturating_duration_since(r.started), park_ms: r.park_ms })
    }

    /// True when the window list is to be read at `now` for what Mission Control did with
    /// the windows put home: once per Mission Control, [`MEASURE_AFTER`] after it showed and
    /// after the round's last lend.
    pub fn measure_due(&self, now: Instant) -> bool {
        let (Some(up), Some(done)) = (self.up, self.round.and_then(|r| r.done)) else { return false };
        !self.measured && !self.out.is_empty() && now.saturating_duration_since(up.max(done)) >= MEASURE_AFTER
    }

    /// The layout was read.
    pub fn measured(&mut self) {
        self.measured = true;
    }

    /// The windows put home.
    pub fn out(&self) -> &[Out] {
        &self.out
    }

    /// Since when the windows out go back ([`fate`]'s `since_close`), while they do.
    pub fn closing(&self, now: Instant) -> Option<Duration> {
        self.closed.filter(|_| !self.out.is_empty()).map(|t| now.saturating_duration_since(t))
    }

    /// A window put home is no longer out.
    pub fn settle(&mut self, id: WindowId, how: Settled) {
        let before = self.out.len();
        self.out.retain(|o| o.id != id);
        if self.out.len() == before {
            return;
        }
        match how {
            Settled::Picked => self.picked.push(id),
            Settled::Back => self.back += 1,
            Settled::Missed => self.missed.push(id),
        }
    }

    /// The take backs' summary, once: Mission Control closed and no window is out any more.
    pub fn back_done(&mut self, now: Instant) -> Option<BackDone> {
        let closed = self.closed.filter(|_| self.out.is_empty())?;
        self.closed = None;
        let done = BackDone {
            back: std::mem::take(&mut self.back),
            picked: std::mem::take(&mut self.picked),
            missed: std::mem::take(&mut self.missed),
            took: now.saturating_duration_since(closed),
        };
        (done.back > 0 || !done.picked.is_empty() || !done.missed.is_empty()).then_some(done)
    }

    /// True while Mission Control is listed.
    pub fn up(&self) -> bool {
        self.up.is_some()
    }

    /// True while there is work for the fast tick at `now`: a watch, a round under way, the
    /// layout to read, windows going back. A round wanted that cannot be planned yet (a
    /// switch plays, the button is down) is tried again at the tick.
    pub fn wants_fast(&self, now: Instant) -> bool {
        let watching = self.up.is_none() && self.hint.is_some_and(|(h, t)| now.saturating_duration_since(t) < h.watch());
        let measuring = self.up.is_some() && !self.measured && !self.out.is_empty() && self.round.is_some_and(|r| r.done.is_some());
        watching || self.guess.is_some() || !self.queue.is_empty() || measuring || self.closing(now).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::{ModelOptions, Observation};
    use crate::engine::recovery::EngineKind;
    use crate::engine::rules::{Decision, RuleAction};
    use crate::engine::testkit::{meta, model_with, w};

    fn keys() -> Keys {
        Keys {
            mission_control: Shortcut { keycode: 126, mods: Mods { control: true, ..Mods::default() }, enabled: true },
            app_windows: Shortcut { keycode: 125, mods: Mods { control: true, ..Mods::default() }, enabled: true },
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn out(id: u32) -> Out {
        Out { id: w(id), pid: 10, parked: Some(Rect::new(1439.0, 899.0, 600.0, 400.0)) }
    }

    #[test]
    fn the_modes_parse_and_shown_is_the_default() {
        for m in [Mode::Off, Mode::Shown, Mode::Key] {
            assert_eq!(Mode::parse(m.name()), m);
        }
        assert_eq!(Mode::default(), Mode::Shown);
        assert_eq!(Mode::parse("sometimes"), Mode::Shown);
        assert_eq!(Mode::named("sometimes"), None);
    }

    #[test]
    fn the_users_shortcuts_and_the_keyboards_key_trigger() {
        let k = keys();
        let ctrl = Mods { control: true, ..Mods::default() };
        assert_eq!(trigger(&k, 126, ctrl), Some(Trigger::MissionControl), "⌃↑");
        assert_eq!(trigger(&k, 125, ctrl), Some(Trigger::AppWindows), "⌃↓");
        assert_eq!(trigger(&k, 126, Mods::default()), None, "a plain arrow");
        assert_eq!(trigger(&k, 126, Mods { control: true, option: true, command: false }), None, "⌃⌥↑ is another shortcut");
        assert_eq!(trigger(&k, 124, ctrl), None, "⌃→ switches Desktops");
        assert_eq!(trigger(&k, MISSION_CONTROL_KEY, Mods::default()), Some(Trigger::MissionControl));
        assert_eq!(trigger(&k, MISSION_CONTROL_KEY, ctrl), Some(Trigger::AppWindows));
        assert_eq!(trigger(&k, MISSION_CONTROL_KEY, Mods { command: true, ..Mods::default() }), None, "Show Desktop");
        // Turned off in Keyboard Shortcuts, or set to another key.
        let mut off = k;
        off.mission_control.enabled = false;
        assert_eq!(trigger(&off, 126, ctrl), None);
        let mut f3 = k;
        f3.mission_control = Shortcut { keycode: 99, mods: Mods::default(), enabled: true };
        assert_eq!(trigger(&f3, 99, Mods::default()), Some(Trigger::MissionControl));
        assert_eq!(trigger(&f3, 126, ctrl), None);
        // The flags as macOS stores the default (Control and Fn).
        assert_eq!(Mods::from_flags(0x84_0000), ctrl);
        assert_eq!(Mods::from_flags(0x12_0000), Mods { command: true, ..Mods::default() }, "Shift is left out");
    }

    #[test]
    fn typing_is_never_looked_up() {
        assert!(!may_trigger(0, Mods::default()), "a letter");
        assert!(!may_trigger(49, Mods::default()), "the space bar");
        assert!(may_trigger(126, Mods::default()), "an arrow");
        assert!(may_trigger(99, Mods::default()), "F3");
        assert!(may_trigger(MISSION_CONTROL_KEY, Mods::default()));
        assert!(may_trigger(0, Mods { control: true, ..Mods::default() }));
    }

    #[test]
    fn hidden_windows_come_most_recent_stage_first_and_the_front_stage_never() {
        let mut m = model_with(ModelOptions { engine: EngineKind::Spaces, ..ModelOptions::default() });
        let obs = Observation::default();
        let new = Decision { action: RuleAction::New, ..Decision::default() };
        m.add_window(meta(1, 10, "com.example.a"), &Decision::default(), &obs);
        m.add_window(meta(2, 10, "com.example.a"), &Decision::default(), &obs);
        m.add_window(meta(3, 20, "com.example.b"), &new, &obs);
        m.add_window(meta(4, 30, "com.example.c"), &new, &obs);
        // Stage C in front; B was in front after A.
        let order = hidden_here(&m, |_| false);
        assert_eq!(order.first(), Some(&w(3)), "the stage used last comes first: {order:?}");
        assert_eq!(order.len(), 3);
        assert!(order.contains(&w(1)) && order.contains(&w(2)) && !order.contains(&w(4)), "never the stage in front: {order:?}");
        assert_eq!(hidden_here(&m, |id| id == w(3)), order[1..].to_vec(), "one put home already is left alone");
        // Under the Corner engine the model is the same; the platform never asks there.
        m.switch_to(m.stage_of(w(1)).expect("staged"), &obs);
        let order = hidden_here(&m, |_| false);
        assert_eq!(order, vec![w(4), w(3)], "A in front now, C used last: {order:?}");
    }

    #[test]
    fn a_window_left_at_its_park_was_not_laid_out() {
        let parked = Rect::new(1439.0, 868.0, 600.0, 400.0);
        assert!(!laid_out(parked, parked));
        assert!(!laid_out(Rect::new(1440.0, 867.0, 600.0, 400.0), parked), "rounding");
        assert!(laid_out(Rect::new(820.0, 300.0, 300.0, 200.0), parked), "scaled into the layout");
        assert!(laid_out(Rect::new(900.0, 400.0, 600.0, 400.0), parked), "a small window moved, not scaled");
    }

    #[test]
    fn shown_mode_lends_only_once_mission_control_shows() {
        let t = Instant::now();
        let mut l = Lends::default();
        assert!(l.hint(Hint::Key(Trigger::MissionControl), t));
        assert_eq!(l.wanted(), None, "nothing on a guess");
        assert!(l.wants_fast(t) && l.look_due(t), "the looks come at the fast tick");
        assert!(!l.look_due(t + ms(10)), "not more often than the watch's pause");
        assert!(l.look_due(t + ms(40)));
        assert_eq!(l.saw(false, t + ms(40)), None);
        assert_eq!(l.saw(true, t + ms(70)), Some(Edge::Shown(Some((Hint::Key(Trigger::MissionControl), ms(70))))));
        assert_eq!(l.wanted(), Some(By::Shown));
        assert!(!l.look_due(t + ms(120)), "no watch while it shows: the tick looks for its close");
        l.plan(vec![w(1), w(2), w(3)], t + ms(70));
        assert_eq!(l.wanted(), None);
        assert_eq!(l.next(), Some(w(1)));
        l.lent(out(1), 6);
        assert_eq!(l.round_done(t + ms(80)), None, "two to go");
        assert_eq!(l.next(), Some(w(2)));
        // Its park did not take: not put home.
        assert_eq!(l.next(), Some(w(3)));
        l.lent(out(3), 9);
        assert_eq!(l.next(), None);
        let done = l.round_done(t + ms(110)).expect("the round's summary");
        assert_eq!((done.by, done.asked, done.lent, done.took, done.park_ms), (By::Shown, 3, 2, ms(40), 15));
        assert_eq!(l.round_done(t + ms(120)), None, "said once");
        assert!(!l.measure_due(t + ms(500)));
        assert!(l.measure_due(t + ms(110) + MEASURE_AFTER));
        l.measured();
        assert!(!l.measure_due(t + ms(2000)) && !l.wants_fast(t + ms(2000)), "at rest while it shows");
        // It closes: both go back.
        assert_eq!(l.closing(t + ms(3000)), None);
        assert_eq!(l.saw(false, t + ms(3000)), Some(Edge::Closed(ms(2930))));
        assert_eq!(l.closing(t + ms(3010)), Some(ms(10)));
        assert!(l.wants_fast(t + ms(3010)));
        assert_eq!(l.back_done(t + ms(3010)), None, "two still out");
        l.settle(w(1), Settled::Back);
        l.settle(w(3), Settled::Picked);
        l.settle(w(3), Settled::Back);
        let back = l.back_done(t + ms(3030)).expect("the take backs' summary");
        assert_eq!((back.back, back.picked.clone(), back.missed.len(), back.took), (1, vec![w(3)], 0, ms(30)));
        assert_eq!(l.back_done(t + ms(3040)), None);
        assert!(!l.wants_fast(t + ms(3040)) && l.out().is_empty());
    }

    #[test]
    fn key_mode_lends_at_the_key_and_takes_back_when_nothing_shows() {
        let t = Instant::now();
        let mut l = Lends::default();
        l.set_mode(Mode::Key);
        assert!(l.hint(Hint::Dock, t));
        assert_eq!(l.wanted(), None, "the Dock's notice is no key");
        assert!(l.hint(Hint::Key(Trigger::AppWindows), t + ms(5)));
        assert_eq!(l.wanted(), Some(By::Key));
        assert!(!l.hint(Hint::Dock, t + ms(8)), "the key stays the hint");
        assert!(!l.hint(Hint::Key(Trigger::AppWindows), t + ms(300)), "nor do its repeats while held replace it");
        l.plan(vec![w(1)], t + ms(5));
        assert_eq!(l.next(), Some(w(1)));
        l.lent(out(1), 7);
        assert_eq!(l.round_done(t + ms(15)).map(|d| d.by), Some(By::Key));
        assert!(!l.guess_over(t + ms(900)));
        assert!(l.look_due(t + ms(900)), "still looked for past the hint's watch, while the guess lasts");
        assert!(l.guess_over(t + ms(5) + KEY_WAIT), "no Mission Control came");
        assert!(!l.guess_over(t + ms(5) + KEY_WAIT + ms(20)), "said once");
        assert!(l.closing(t + ms(5) + KEY_WAIT).is_some());
        l.settle(w(1), Settled::Back);
        assert_eq!(l.back_done(t + ms(1600)).map(|b| b.back), Some(1));
        // The next key's round: Mission Control shows, the round made at the key stands.
        let t = t + ms(5000);
        assert!(l.hint(Hint::Key(Trigger::MissionControl), t));
        l.plan(vec![w(1)], t);
        assert_eq!(l.next(), Some(w(1)));
        l.lent(out(1), 7);
        l.round_done(t + ms(10));
        assert_eq!(l.saw(true, t + ms(60)), Some(Edge::Shown(Some((Hint::Key(Trigger::MissionControl), ms(60))))));
        assert_eq!(l.wanted(), None, "no second round for the same Mission Control");
        assert!(!l.guess_over(t + KEY_WAIT + ms(100)), "it showed");
        assert!(l.saw(false, t + ms(4000)).is_some());
        assert!(l.closing(t + ms(4000)).is_some());
    }

    #[test]
    fn off_follows_nothing_and_a_tests_round_waits_for_its_own_back() {
        let t = Instant::now();
        let mut l = Lends::default();
        l.set_mode(Mode::Off);
        assert!(!l.hint(Hint::Key(Trigger::MissionControl), t));
        assert_eq!(l.saw(true, t), Some(Edge::Shown(None)));
        assert_eq!(l.wanted(), None);
        assert!(!l.wants_fast(t));
        assert!(l.saw(false, t + ms(500)).is_some());
        // A test's round before the key: Mission Control opening and closing leaves it out.
        l.ask();
        assert_eq!(l.wanted(), Some(By::Asked));
        l.plan(vec![w(1)], t + ms(600));
        assert_eq!(l.next(), Some(w(1)));
        l.lent(out(1), 5);
        assert_eq!(l.round_done(t + ms(610)).map(|d| d.by), Some(By::Asked));
        assert!(l.saw(true, t + ms(700)).is_some());
        assert!(l.measure_due(t + ms(700) + MEASURE_AFTER), "its layout is read all the same");
        assert!(l.saw(false, t + ms(2000)).is_some());
        assert_eq!(l.closing(t + ms(2000)), None, "kept out");
        l.ask_back(t + ms(2100));
        assert!(l.closing(t + ms(2100)).is_some_and(|d| d >= PICK_WAIT), "nothing waits for a pick");
        l.settle(w(1), Settled::Missed);
        assert_eq!(l.back_done(t + ms(2110)).map(|b| b.missed), Some(vec![w(1)]));
    }

    #[test]
    fn turned_off_while_windows_are_out_they_still_go_back_at_the_close() {
        let t = Instant::now();
        let mut l = Lends::default();
        assert!(l.saw(true, t).is_some());
        l.plan(vec![w(1), w(2)], t);
        assert_eq!(l.next(), Some(w(1)));
        l.lent(out(1), 5);
        l.set_mode(Mode::Off);
        assert_eq!(l.next(), None, "no more lends");
        assert!(l.saw(false, t + ms(900)).is_some());
        assert!(l.closing(t + ms(900)).is_some());
    }

    #[test]
    fn opened_again_before_the_take_backs_the_windows_stay_out() {
        let t = Instant::now();
        let mut l = Lends::default();
        l.saw(true, t);
        l.plan(vec![w(1)], t);
        l.next();
        l.lent(out(1), 5);
        l.round_done(t + ms(10));
        l.saw(false, t + ms(1000));
        assert!(l.closing(t + ms(1000)).is_some());
        assert!(l.saw(true, t + ms(1020)).is_some());
        assert_eq!(l.closing(t + ms(1030)), None, "it shows again: nothing goes back");
        assert_eq!(l.wanted(), Some(By::Shown), "and what hid meanwhile is put home too");
        l.plan(vec![w(1), w(2)], t + ms(1030));
        assert_eq!(l.next(), Some(w(2)), "the one out already is not lent twice");
    }

    #[test]
    fn a_picked_windows_stage_is_left_to_its_switch() {
        assert_eq!(fate(true, true, ms(0)), Fate::Forget, "its stage is in front");
        assert_eq!(fate(true, false, ms(5000)), Fate::Forget);
        assert_eq!(fate(false, false, ms(0)), Fate::TakeBack, "another app's: back at once");
        assert_eq!(fate(false, true, ms(200)), Fate::Wait, "the app in front: its switch may still come");
        assert_eq!(fate(false, true, PICK_WAIT), Fate::TakeBack, "none came");
    }
}
