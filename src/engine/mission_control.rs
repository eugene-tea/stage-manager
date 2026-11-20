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
//! **All at once** ([`super::lend_round`], round mc-lends). One at a time, the windows came
//! onto the Desktop one by one during Mission Control's opening, and it laid them out again
//! at each (a log, 11-19). A round parks every window first, still hidden (one bounded move
//! through its app, the apps at once, all within [`ROUND_WAIT`]: a window whose turn comes
//! later waits for the next fast tick's round), then puts all those parked home in one
//! window-server call per Desktop. A window whose park does not take is not put home (it
//! would show at its frame), and stays hidden; while Mission Control shows it is tried again
//! [`RETRY_AFTER`] later, up to [`PARK_TRIES`] parks in all ([`retry`]: 10 of 50 opens,
//! 11-18 to 11-20, left one to five windows out, never with a word of why).
//!
//! **Parks at the key.** In [`Mode::Shown`] one of the keys starts the parks at once
//! ([`By::KeyPark`]): the windows stay in their stage Spaces, so nothing shows, and they go
//! home once Mission Control is seen, with no park to wait for then. When none shows within
//! [`KEY_WAIT`] (or the mode is turned off) their frames are set again where they are
//! ([`Lends::early_back`]). A gesture, a hot corner and the Dock's item give no key: the
//! round is made whole once Mission Control shows.
//!
//! **A Desktop picked inside it** ([`scope`]). A Desktop change made in Mission Control (a
//! click on another Desktop's thumbnail) is recognized only once it has closed, so the model's
//! shown Space is the old one until then, and the next Mission Control, opened before that,
//! lent nothing (11-19 07:56:06: `shown out=0`, only the stage in front). While such a
//! change waits for the overview, the hidden windows are taken by the window server's word
//! instead: those whose Desktop (the one each was hidden from, and goes back to) is one it
//! shows now ([`hidden_on`]). Nothing else moves while the change waits; with the screen
//! locked or a full-screen Space nothing is lent.
//!
//! **A window picked** ([`to_frames`], [`Lends::pick_switch`], round mc-pick). Apple's grows
//! the window picked from its place in Mission Control to its own frame as Mission Control
//! closes, in one motion, while the stage it leaves shrinks into the strip (11-19). Ours was
//! carried to the park corner by the close, and came to its frame a second later, behind its
//! switch's pictures. macOS's activation of the picked window's app is the signal: it comes
//! about 150 ms after the press, with the release that starts the close (the input monitor
//! does not always see a press inside Mission Control: four picks of 11-19 came with none).
//! At that activation the stage it brings in front is known (a copy of the model is asked),
//! and the windows of it that are out get their own frames back at once: Mission Control's
//! close carries them there. They stay lent until the switch shows them. The switch itself
//! waits until Mission Control's own window is gone ([`PICK_HOLD`] at most), so the stage
//! left reaches its frames first; then it plays the stage left's shrink into its card, and
//! nothing for the windows already at their frames
//! ([`crate::ui::transition_plan::Shown::Placed`]). Should no switch come (another app in front by then), the windows
//! placed go back into their stage Spaces like any other, at their frames, unseen.
//!
//! **Never left out.** The engine counts every window put home as lent and parked: a hide,
//! a show, a sweep (quit, a stand-down, a fallback) and the guardian's scan of the park
//! corner bring each back as they do ⌘Tab's ([`super::lend_park`]). [`Lends`] only decides
//! when; what is out is the engine's to know.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::geometry::Rect;
use super::lend_round::Left;
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

/// How long a round's parks may take turns ([`super::lend_round::park_in_turn`]): each app's
/// windows are asked in turn while it lasts (parks took 5 to 20 ms a window, 11-15 to 11-19),
/// the apps at once; a window whose turn comes later waits for the next fast tick's round.
pub const ROUND_WAIT: Duration = Duration::from_millis(150);

/// How many parks a hidden window gets in all while one Mission Control shows: one that did
/// not take (or a window the window server did not take home) is tried again ([`retry`]).
pub const PARK_TRIES: u8 = 3;

/// The pause before a window left hidden by a round is tried again: a window in a stage Space
/// can be listed at its new place later than a frame (a take back's restore, 11-15), and a
/// park's read back waits 20 ms.
pub const RETRY_AFTER: Duration = Duration::from_millis(60);

/// Windows put home at a key ([`Mode::Key`]) go back when no Mission Control shows this long
/// after it (the shortcut was another app's, or macOS did not open it).
pub const KEY_WAIT: Duration = Duration::from_millis(1500);

/// A window of the app in front still counted hidden stays out this long after Mission
/// Control closed: picked there, its app is activated and the activation's switch shows it;
/// taken back first, the app's focus on a window off every Desktop could take macOS to
/// another Desktop ([`super::spaces::macos_counts_hidden`]).
pub const PICK_WAIT: Duration = Duration::from_millis(1500);

/// How long a picked stage's switch waits at most for Mission Control's own window to go
/// ([`Lends::pick_due`]): Apple's was gone 0.53 s after a pick, its Dock's cover 0.81 s
/// (`scripts/probe_apple.py mission-pick`, 11-19).
pub const PICK_HOLD: Duration = Duration::from_millis(1000);

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
    /// One of its keys was pressed ([`Mode::Shown`]): the windows parked, not put home.
    KeyPark,
    /// A test asked (`ctl debug-mission-control lend`): kept out until it asks them back.
    Asked,
}

impl By {
    /// Its name for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::Shown => "Mission Control showed",
            Self::Key => "its key was pressed",
            Self::KeyPark => "its key was pressed (parked only)",
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

/// The hidden windows of the Desktops the window server shows (`shown`), each found by
/// `home` (the Desktop it was hidden from, `None` for a window the engine does not hold), in
/// the order of [`hidden_here`] (the most recently used stage first), whatever Space the
/// model counts shown.
pub fn hidden_on(m: &Model, home: impl Fn(WindowId) -> Option<u64>, shown: &[u64], skip: impl Fn(WindowId) -> bool) -> Vec<WindowId> {
    let mut stages: Vec<_> = m.stages().iter().collect();
    stages.sort_by_key(|s| std::cmp::Reverse(s.last_active));
    stages
        .into_iter()
        .flat_map(|s| s.windows.iter().copied())
        .filter(|id| m.window(*id).is_some_and(|w| w.parked && !w.sticky && w.meta.role == WindowRole::Standard))
        .filter(|id| home(*id).is_some_and(|d| shown.contains(&d)))
        .filter(|id| !skip(*id))
        .collect()
}

/// What holds the stages when a lend is asked ([`scope`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Moment {
    /// The screen is locked.
    pub locked: bool,
    /// A full-screen app's Space shows, or a change from one has not settled.
    pub fullscreen: bool,
    /// A Space change waits to be recognized.
    pub changing: bool,
    /// What it waits for is the overview to close ([`super::phase::Hold::Overview`]).
    pub for_overview: bool,
    /// Mission Control or App Exposé is listed.
    pub showing: bool,
}

/// Where the hidden windows to lend are taken from ([`scope`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The model's shown Spaces ([`hidden_here`]).
    Model,
    /// The Desktops the window server shows ([`hidden_on`]): the model's shown Space is stale.
    Desktops(Vec<u64>),
    /// None: nothing is lent now.
    Nothing,
}

/// Where the windows to lend come from at `at`: `model` the Desktops of the hidden windows
/// the model counts on its shown Spaces (by the engine's word), `server` the Desktops the
/// window server shows. Locked or full screen: nothing. A Space change waiting for Mission
/// Control to close, while it shows: the window server's Desktops, unless the model's agree
/// with them. Any other change settling: nothing, as everything waits for it.
pub fn scope(at: Moment, model: &[u64], server: &[u64]) -> Scope {
    if at.locked || at.fullscreen {
        return Scope::Nothing;
    }
    if !at.changing {
        return Scope::Model;
    }
    if !(at.for_overview && at.showing) || server.is_empty() {
        return Scope::Nothing;
    }
    if !model.is_empty() && model.iter().all(|d| server.contains(d)) {
        return Scope::Model;
    }
    Scope::Desktops(server.to_vec())
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

/// True when a hidden window a round left hidden (`why`), `misses` times now while this
/// Mission Control shows (`showing`), is tried again: a park that did not take, or a window
/// the window server did not take home, up to [`PARK_TRIES`] parks in all. A window the
/// engine does not hold, one lent or in a switch, and one with no park made are not: nothing
/// a later round does changes that. Never at its frame: a retry is a park again, and only a
/// park that takes goes home.
pub fn retry(why: Left, misses: u8, showing: bool) -> bool {
    showing && misses < PARK_TRIES && matches!(why, Left::ParkMissed { .. } | Left::NotHome)
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

/// What asked for a picked stage's switch: macOS's activation of its app, or its focus of
/// one of its windows (the app in front already).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickBy {
    /// The app's activation.
    App(Pid),
    /// A window's focus, with its app.
    Window(WindowId, Pid),
}

impl PickBy {
    /// The app picked.
    pub fn pid(self) -> Pid {
        match self {
            Self::App(pid) | Self::Window(_, pid) => pid,
        }
    }
}

/// The windows put home (`out`) that a switch bringing in `arriving` (the windows its batch
/// shows) puts at their own frames now, while Mission Control closes: those out and not
/// `placed` already. A switch that brings in no window put home (the stage in front's app
/// activated, a stage never lent) gets none.
pub fn to_frames(out: &[Out], placed: &[WindowId], arriving: &[WindowId]) -> Vec<Out> {
    out.iter().filter(|o| arriving.contains(&o.id) && !placed.contains(&o.id)).copied().collect()
}

/// When a picked stage's switch plays ([`Lends::pick_switch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickSwitch {
    /// At once: Mission Control is gone, or the switch waited for it already.
    Now,
    /// Once Mission Control's own window is gone ([`Lends::pick_due`]).
    AfterClose,
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
    /// Those parked and left hidden for Mission Control to show ([`By::KeyPark`]).
    pub parked: usize,
    /// From its first lend to its last.
    pub took: Duration,
    /// The parks' moves added up (what one at a time would have taken).
    pub park_ms: u64,
    /// The parks, from the first asked to the last answer, the apps at once.
    pub parks_ms: u64,
    /// The window-server calls putting the windows home, with their read back.
    pub move_ms: u64,
    /// The most apps asked at once.
    pub apps: usize,
    /// From the first call putting windows home to the end of the last (`None`: none made):
    /// the gap between the first and the last window put home.
    pub first_to_last: Option<Duration>,
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
    parked: usize,
    park_ms: u64,
    parks_ms: u64,
    move_ms: u64,
    apps: usize,
    /// The first call putting windows home made, and the last one's end.
    calls: Option<(Instant, Instant)>,
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
    /// The windows parked at a key ([`By::KeyPark`]), still hidden, with their apps.
    early: Vec<(WindowId, Pid)>,
    /// When that key was pressed, until Mission Control shows.
    early_at: Option<Instant>,
    /// The layout was read for this Mission Control.
    measured: bool,
    back: usize,
    picked: Vec<WindowId>,
    missed: Vec<WindowId>,
    /// Windows out whose own frames were set again for a pick ([`to_frames`]): still lent,
    /// until their switch shows them.
    placed: Vec<WindowId>,
    /// A picked stage's switch waiting for Mission Control's close, and since when.
    pick: Option<(PickBy, Instant)>,
    /// The pick whose wait is over: its switch plays now, it waits no more.
    released: Option<PickBy>,
    /// Mission Control's own window (WindowManager's) was listed at the last look; the
    /// Dock's cover may stay listed a while after it for the exit.
    own_listed: bool,
    /// The windows left hidden by a round while this Mission Control shows, and how many
    /// times ([`retry`]).
    misses: Vec<(WindowId, u8)>,
    /// Those tried again once [`RETRY_AFTER`] has passed, and when.
    retry: Vec<WindowId>,
    retry_at: Option<Instant>,
}

impl Lends {
    /// The mode as the config has it now. Turned off, nothing new is wanted; what is out
    /// still goes back at the close.
    pub fn set_mode(&mut self, mode: Mode) {
        if mode == Mode::Off && self.mode != Mode::Off {
            self.wanted = None;
            self.queue.clear();
            self.hint = None;
            self.forget_misses();
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
        if matches!(hint, Hint::Key(_)) && self.round.is_none() && self.out.is_empty() && self.early.is_empty() {
            match self.mode {
                Mode::Key => {
                    self.wanted = Some(By::Key);
                    self.guess = Some(now);
                }
                Mode::Shown => {
                    self.wanted = Some(By::KeyPark);
                    self.early_at = Some(now);
                }
                Mode::Off => {}
            }
        }
        true
    }

    /// True when the watch wants a look at the window list at `now` (asked at the fast
    /// tick; the look is then counted made).
    pub fn look_due(&mut self, now: Instant) -> bool {
        let watching = self.hint.is_some_and(|(h, t)| now.saturating_duration_since(t) < h.watch()) || self.guess.is_some();
        // A pick's switch waits for the close: looked for at the fast tick while it shows.
        let opening = watching && self.up.is_none();
        if !(opening || self.pick.is_some()) || self.looked.is_some_and(|t| now.saturating_duration_since(t) < WATCH_EVERY) {
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
                self.early_at = None;
                // Opened again before a pick's switch played: it waits no more.
                self.pick = None;
                self.released = None;
                self.forget_misses();
                if self.mode != Mode::Off {
                    // Windows still out from the last one stay out for this one.
                    self.closed = None;
                    // A round of parks at the key goes home now, with what it did not reach.
                    if self.round.is_none_or(|r| r.by == By::KeyPark) {
                        self.wanted = Some(By::Shown);
                        self.round = None;
                        self.queue.clear();
                    }
                }
                Some(Edge::Shown(hint))
            }
            (false, Some(since)) => {
                self.up = None;
                self.wanted = None;
                self.queue.clear();
                self.forget_misses();
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
        self.forget_misses();
        self.round = None;
        self.closed = Some(now);
        true
    }

    /// The round wanted and not planned yet.
    pub fn wanted(&self) -> Option<By> {
        self.wanted
    }

    /// True while the round under way only parks ([`By::KeyPark`], Mission Control not seen
    /// yet): nothing goes home.
    pub fn parks_only(&self) -> bool {
        self.up.is_none() && self.round.is_some_and(|r| r.by == By::KeyPark)
    }

    /// Windows parked at the key, still hidden, for Mission Control to show.
    pub fn parked_early(&mut self, windows: impl IntoIterator<Item = (WindowId, Pid)>) {
        for (id, pid) in windows {
            if !self.early.iter().any(|(e, _)| *e == id) {
                self.early.push((id, pid));
                if let Some(r) = self.round.as_mut() {
                    r.parked += 1;
                }
            }
        }
    }

    /// The windows parked at the key, handed to the round that puts them home now.
    pub fn take_early(&mut self) -> Vec<(WindowId, Pid)> {
        std::mem::take(&mut self.early)
    }

    /// The windows parked at the key whose frames are set again now, once: no Mission Control
    /// within [`KEY_WAIT`] of the key, it closed before they went home, or the mode is off.
    pub fn early_back(&mut self, now: Instant) -> Option<Vec<(WindowId, Pid)>> {
        if self.early.is_empty() || (self.up.is_some() && self.mode != Mode::Off) {
            return None;
        }
        let waited = self.early_at.is_none_or(|t| now.saturating_duration_since(t) >= KEY_WAIT);
        if self.mode != Mode::Off && !waited {
            return None;
        }
        self.early_at = None;
        if self.round.is_some_and(|r| r.by == By::KeyPark) {
            self.round = None;
            self.queue.clear();
            self.wanted = None;
        }
        Some(self.take_early())
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
        self.forget_misses();
        self.round = None;
        // Long closed: nothing waits for a pick.
        self.closed = Some(now.checked_sub(PICK_WAIT).unwrap_or(now));
    }

    /// The round wanted is planned at `now`: `windows` to put home, in order
    /// ([`hidden_here`]); those out already are left out of it.
    pub fn plan(&mut self, windows: Vec<WindowId>, now: Instant) {
        let Some(by) = self.wanted.take() else { return };
        self.queue = windows.into_iter().filter(|id| !self.out.iter().any(|o| o.id == *id)).collect();
        self.round = Some(Round { by, started: now, asked: self.queue.len(), lent: 0, parked: 0, park_ms: 0, parks_ms: 0, move_ms: 0, apps: 0, calls: None, done: None });
    }

    /// The next window to put home.
    pub fn next_window(&mut self) -> Option<WindowId> {
        self.queue.pop_front()
    }

    /// Every window still to put home, for one round ([`super::lend_round`]).
    pub fn take_queue(&mut self) -> Vec<WindowId> {
        self.queue.drain(..).collect()
    }

    /// Windows whose turn came after the round's deadline ([`ROUND_WAIT`]): the next round's
    /// first, in their order.
    pub fn requeue(&mut self, windows: Vec<WindowId>) {
        for id in windows.into_iter().rev() {
            self.queue.push_front(id);
        }
    }

    /// Window `id` was left hidden by a round at `now`, `why`: tried again [`RETRY_AFTER`]
    /// later while Mission Control shows ([`retry`]). How many times it was left so far, and
    /// whether it is tried again.
    pub fn left(&mut self, id: WindowId, why: Left, now: Instant) -> (u8, bool) {
        let misses = match self.misses.iter_mut().find(|(w, _)| *w == id) {
            Some((_, n)) => {
                *n = n.saturating_add(1);
                *n
            }
            None => {
                self.misses.push((id, 1));
                1
            }
        };
        let again = retry(why, misses, self.up.is_some() && self.mode != Mode::Off);
        if again && !self.retry.contains(&id) {
            self.retry.push(id);
            self.retry_at = Some(now + RETRY_AFTER);
        }
        (misses, again)
    }

    /// The windows to try again, into the queue once their pause is over at `now` (while
    /// Mission Control shows): true when some went in.
    pub fn retries_due(&mut self, now: Instant) -> bool {
        if self.retry.is_empty() || self.up.is_none() || self.retry_at.is_some_and(|t| now < t) {
            return false;
        }
        self.retry_at = None;
        let out = &self.out;
        let again: Vec<WindowId> = std::mem::take(&mut self.retry).into_iter().filter(|id| !out.iter().any(|o| o.id == *id)).collect();
        let any = !again.is_empty();
        self.queue.extend(again);
        any
    }

    /// No window waits to be tried again (Mission Control closed or opened anew, the mode
    /// off, the lends ended).
    fn forget_misses(&mut self) {
        self.misses.clear();
        self.retry.clear();
        self.retry_at = None;
    }

    /// A round's parks were made: `parks_ms` from the first to the last answer, `sum_ms` the
    /// moves added up, `apps` asked at once.
    pub fn parks_made(&mut self, parks_ms: u64, sum_ms: u64, apps: usize) {
        if let Some(r) = self.round.as_mut() {
            r.parks_ms += parks_ms;
            r.park_ms += sum_ms;
            r.apps = r.apps.max(apps);
        }
    }

    /// A round's windows were put home: `move_ms` in the calls and their read back, `calls`
    /// the first call's start and the last one's end.
    pub fn homes_made(&mut self, move_ms: u64, calls: Option<(Instant, Instant)>) {
        if let Some(r) = self.round.as_mut() {
            r.move_ms += move_ms;
            if let Some((start, end)) = calls {
                r.calls = Some(r.calls.map_or((start, end), |(s, _)| (s, end)));
            }
        }
    }

    /// A window was put home.
    pub fn lent(&mut self, out: Out) {
        if let Some(r) = self.round.as_mut() {
            r.lent += 1;
        }
        self.out.retain(|o| o.id != out.id);
        self.out.push(out);
    }

    /// The round's summary, once: its last window is out (or was not put home).
    pub fn round_done(&mut self, now: Instant) -> Option<RoundDone> {
        if !self.queue.is_empty() || !self.retry.is_empty() {
            return None;
        }
        let r = self.round.as_mut().filter(|r| r.done.is_none())?;
        r.done = Some(now);
        Some(RoundDone {
            by: r.by,
            asked: r.asked,
            lent: r.lent,
            parked: r.parked,
            took: now.saturating_duration_since(r.started),
            park_ms: r.park_ms,
            parks_ms: r.parks_ms,
            move_ms: r.move_ms,
            apps: r.apps,
            first_to_last: r.calls.map(|(s, e)| e.saturating_duration_since(s)),
        })
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
        self.placed.retain(|p| *p != id);
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

    /// True when a switch at `now` may be a pick in Mission Control: windows are out, and
    /// it shows or closed less than [`PICK_WAIT`] ago.
    pub fn pick_open(&self, now: Instant) -> bool {
        !self.out.is_empty() && (self.up.is_some() || self.closed.is_some_and(|t| now.saturating_duration_since(t) < PICK_WAIT))
    }

    /// Windows out whose own frames were set again for a pick ([`to_frames`]).
    pub fn placed(&mut self, ids: impl IntoIterator<Item = WindowId>) {
        for id in ids {
            if self.out.iter().any(|o| o.id == id) && !self.placed.contains(&id) {
                self.placed.push(id);
            }
        }
    }

    /// The windows out at their own frames for a pick.
    pub fn placed_ids(&self) -> &[WindowId] {
        &self.placed
    }

    /// True when window `id` is out at its own frame for a pick: its switch plays no motion
    /// for it.
    pub fn is_placed(&self, id: WindowId) -> bool {
        self.placed.contains(&id)
    }

    /// What Mission Control's own window (WindowManager's) did at a look: `listed` or not.
    pub fn saw_own(&mut self, listed: bool) {
        self.own_listed = listed;
    }

    /// When the switch of a pick (`by`) at `now` plays: after Mission Control's close while
    /// its own window is listed (the pick is then held, [`Self::pick_due`]), else now. A pick
    /// of the app whose wait is over plays now; one of the app held already stays as held.
    pub fn pick_switch(&mut self, by: PickBy, now: Instant) -> PickSwitch {
        if self.released.is_some_and(|r| r.pid() == by.pid()) || self.up.is_none() || !self.own_listed {
            self.released = None;
            self.pick = None;
            return PickSwitch::Now;
        }
        // The same app's focus after its activation (or again) keeps the pick held as it was.
        if self.pick.is_none_or(|(held, _)| held.pid() != by.pid()) {
            self.pick = Some((by, now));
        }
        PickSwitch::AfterClose
    }

    /// The pick held for Mission Control's close.
    pub fn pick_held(&self) -> Option<PickBy> {
        self.pick.map(|(by, _)| by)
    }

    /// A held pick waits no more (another app was activated): its switch never plays; its
    /// windows go back as any other once Mission Control has closed.
    pub fn drop_pick(&mut self) -> Option<PickBy> {
        self.released = None;
        self.pick.take().map(|(by, _)| by)
    }

    /// The held pick whose switch plays now, once: Mission Control closed, its own window
    /// gone (the Dock's cover may still be listed for its exit), or [`PICK_HOLD`] over.
    pub fn pick_due(&mut self, now: Instant) -> Option<PickBy> {
        let (by, at) = self.pick?;
        let due = self.up.is_none() || !self.own_listed || now.saturating_duration_since(at) >= PICK_HOLD;
        if !due {
            return None;
        }
        self.pick = None;
        self.released = Some(by);
        Some(by)
    }

    /// True while there is work for the fast tick at `now`: a watch, a round under way, the
    /// layout to read, windows going back. A round wanted that cannot be planned yet (a
    /// switch plays, the button is down) is tried again at the tick.
    pub fn wants_fast(&self, now: Instant) -> bool {
        let watching = self.up.is_none() && self.hint.is_some_and(|(h, t)| now.saturating_duration_since(t) < h.watch());
        let measuring = self.up.is_some() && !self.measured && !self.out.is_empty() && self.round.is_some_and(|r| r.done.is_some());
        let early = !self.early.is_empty() && self.up.is_none();
        let retrying = !self.retry.is_empty() && self.up.is_some();
        watching || self.guess.is_some() || !self.queue.is_empty() || retrying || measuring || early || self.pick.is_some() || self.closing(now).is_some()
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

    /// 11-19 07:56: Mission Control opened again before a Desktop picked in it was
    /// recognized. The model's shown Space is the old Desktop's; the window server's Desktop
    /// is the new one, and its hidden windows are lent; with the two agreeing, as ever; with
    /// the screen locked or a full-screen Space, nothing.
    #[test]
    fn a_desktop_picked_inside_mission_control_lends_the_shown_desktops_windows() {
        let stale = Moment { changing: true, for_overview: true, showing: true, ..Moment::default() };
        assert_eq!(scope(stale, &[2], &[1]), Scope::Desktops(vec![1]), "the model still on Desktop 2, the window server on 1");
        assert_eq!(scope(stale, &[], &[1]), Scope::Desktops(vec![1]), "nothing hidden on the model's Space");
        assert_eq!(scope(stale, &[1], &[1, 9]), Scope::Model, "the two agree");
        assert_eq!(scope(Moment::default(), &[2], &[1]), Scope::Model, "no change waits: the model's");
        assert_eq!(scope(Moment { showing: true, ..Moment::default() }, &[1], &[1]), Scope::Model);
        for held in [Moment { locked: true, ..stale }, Moment { fullscreen: true, ..stale }] {
            assert_eq!(scope(held, &[2], &[1]), Scope::Nothing, "{held:?}");
        }
        assert_eq!(scope(Moment { for_overview: false, ..stale }, &[2], &[1]), Scope::Nothing, "another change settling");
        assert_eq!(scope(Moment { showing: false, ..stale }, &[2], &[1]), Scope::Nothing, "Mission Control not listed");
        assert_eq!(scope(stale, &[2], &[]), Scope::Nothing, "the window server said nothing");

        // The windows: every hidden one whose Desktop is shown, the stage used last first,
        // whatever the model counts in front.
        let mut m = model_with(ModelOptions { engine: EngineKind::Spaces, ..ModelOptions::default() });
        let obs = Observation::default();
        let new = Decision { action: RuleAction::New, ..Decision::default() };
        m.add_window(meta(1, 10, "com.example.a"), &Decision::default(), &obs);
        m.add_window(meta(3, 20, "com.example.b"), &new, &obs);
        m.add_window(meta(4, 30, "com.example.c"), &new, &obs);
        // 1 and 3 hidden (stage C in front); 1 hidden from Desktop 1, 3 from Desktop 2.
        let home = |id: WindowId| match id.0 {
            1 => Some(1),
            3 => Some(2),
            _ => None,
        };
        assert_eq!(hidden_on(&m, home, &[1], |_| false), vec![w(1)], "Desktop 1's only");
        assert_eq!(hidden_on(&m, home, &[2], |_| false), vec![w(3)]);
        assert_eq!(hidden_on(&m, home, &[1, 2], |_| false), vec![w(3), w(1)], "the stage used last first");
        assert_eq!(hidden_on(&m, home, &[1, 2], |id| id == w(3)), vec![w(1)], "one put home already is left alone");
        assert!(!hidden_on(&m, home, &[1, 2], |_| false).contains(&w(4)), "a shown window is never lent");
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
        // The Dock's notice gives no key: nothing is parked before Mission Control shows.
        assert!(l.hint(Hint::Dock, t));
        assert_eq!(l.wanted(), None, "nothing on a guess");
        assert!(l.wants_fast(t) && l.look_due(t), "the looks come at the fast tick");
        assert!(!l.look_due(t + ms(10)), "not more often than the watch's pause");
        assert!(l.look_due(t + ms(40)));
        assert_eq!(l.saw(false, t + ms(40)), None);
        assert_eq!(l.saw(true, t + ms(70)), Some(Edge::Shown(Some((Hint::Dock, ms(70))))));
        assert_eq!(l.wanted(), Some(By::Shown));
        assert!(!l.look_due(t + ms(120)), "no watch while it shows: the tick looks for its close");
        l.plan(vec![w(1), w(2), w(3)], t + ms(70));
        assert_eq!(l.wanted(), None);
        assert!(!l.parks_only());
        // One round: all three asked, the third's turn after the deadline.
        assert_eq!(l.take_queue(), vec![w(1), w(2), w(3)]);
        l.parks_made(20, 30, 2);
        l.requeue(vec![w(3)]);
        let home = t + ms(90);
        l.homes_made(2, Some((home, home + ms(1))));
        l.lent(out(1));
        // Its park did not take: not put home.
        assert_eq!(l.round_done(t + ms(92)), None, "one to go");
        assert_eq!(l.take_queue(), vec![w(3)], "the next round");
        l.parks_made(10, 10, 1);
        l.homes_made(1, Some((home + ms(15), home + ms(16))));
        l.lent(out(3));
        let done = l.round_done(t + ms(110)).expect("the round's summary");
        assert_eq!((done.by, done.asked, done.lent, done.took), (By::Shown, 3, 2, ms(40)));
        assert_eq!((done.park_ms, done.parks_ms, done.move_ms, done.apps), (40, 30, 3, 2));
        assert_eq!(done.first_to_last, Some(ms(16)), "from the first call to the end of the last");
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

    /// Which windows left hidden are tried again: a park that did not take and a window the
    /// window server kept in its stage Space, while Mission Control shows, up to
    /// [`PARK_TRIES`] parks; nothing a later round changes is not.
    #[test]
    fn a_missed_park_is_tried_again_while_mission_control_shows() {
        let missed = Left::ParkMissed { listed: Some(Rect::new(300.0, 120.0, 900.0, 600.0)), moved: false };
        for why in [missed, Left::ParkMissed { listed: None, moved: false }, Left::ParkMissed { listed: None, moved: true }, Left::NotHome] {
            assert!(retry(why, 1, true) && retry(why, PARK_TRIES - 1, true), "{why:?}");
            assert!(!retry(why, PARK_TRIES, true), "{why:?}: {PARK_TRIES} parks in all");
            assert!(!retry(why, 1, false), "{why:?}: not once Mission Control is gone");
        }
        for why in [Left::NotHeld, Left::Busy, Left::NotParked] {
            assert!(!retry(why, 1, true), "{why:?}");
        }
    }

    /// A round leaves two windows hidden: they wait [`RETRY_AFTER`], go into the next round's
    /// queue, and the round's summary waits for them; a third miss is the last; a close or a
    /// new opening forgets them.
    #[test]
    fn windows_left_hidden_go_into_a_later_round_and_the_summary_waits() {
        let t = Instant::now();
        let mut l = Lends::default();
        assert_eq!(l.saw(true, t), Some(Edge::Shown(None)));
        l.plan(vec![w(1), w(2), w(3)], t);
        assert_eq!(l.take_queue(), vec![w(1), w(2), w(3)]);
        l.lent(out(1));
        let missed = Left::ParkMissed { listed: None, moved: false };
        assert_eq!(l.left(w(2), missed, t + ms(30)), (1, true));
        assert_eq!(l.left(w(3), Left::NotHeld, t + ms(30)), (1, false), "the engine does not hold it: nothing to try");
        assert_eq!(l.round_done(t + ms(31)), None, "one to try again");
        assert!(l.wants_fast(t + ms(40)));
        assert!(!l.retries_due(t + ms(40)), "not before its pause");
        assert!(l.take_queue().is_empty());
        assert!(l.retries_due(t + ms(30) + RETRY_AFTER));
        assert_eq!(l.take_queue(), vec![w(2)], "the next round's");
        assert_eq!(l.left(w(2), missed, t + ms(100)), (2, true));
        assert!(l.retries_due(t + ms(100) + RETRY_AFTER));
        assert_eq!(l.take_queue(), vec![w(2)]);
        assert_eq!(l.left(w(2), missed, t + ms(200)), (3, false), "its last park");
        let done = l.round_done(t + ms(201)).expect("the summary once nothing is left to try");
        assert_eq!((done.asked, done.lent), (3, 1));
        // Taken on a later try: put home with the round.
        let mut l = Lends::default();
        l.saw(true, t);
        l.plan(vec![w(4)], t);
        l.take_queue();
        l.left(w(4), Left::NotHome, t);
        assert!(l.retries_due(t + RETRY_AFTER));
        assert_eq!(l.take_queue(), vec![w(4)]);
        l.lent(out(4));
        assert_eq!(l.round_done(t + RETRY_AFTER + ms(5)).map(|d| d.lent), Some(1));
        // Closed while one waits: forgotten, nothing goes into a queue after.
        let mut l = Lends::default();
        l.saw(true, t);
        l.plan(vec![w(5)], t);
        l.take_queue();
        l.left(w(5), missed, t);
        l.saw(false, t + ms(10));
        assert!(!l.retries_due(t + ms(500)) && l.take_queue().is_empty());
        assert!(!l.wants_fast(t + ms(500)));
        // Opened again: a window counts its misses afresh.
        l.saw(true, t + ms(600));
        assert_eq!(l.left(w(5), missed, t + ms(610)), (1, true));
        // The mode turned off: nothing is tried again.
        l.set_mode(Mode::Off);
        assert!(!l.retries_due(t + ms(2000)));
        assert_eq!(l.left(w(5), missed, t + ms(2000)), (1, false));
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
        assert_eq!(l.next_window(), Some(w(1)));
        l.lent(out(1));
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
        assert_eq!(l.next_window(), Some(w(1)));
        l.lent(out(1));
        l.round_done(t + ms(10));
        assert_eq!(l.saw(true, t + ms(60)), Some(Edge::Shown(Some((Hint::Key(Trigger::MissionControl), ms(60))))));
        assert_eq!(l.wanted(), None, "no second round for the same Mission Control");
        assert!(!l.guess_over(t + KEY_WAIT + ms(100)), "it showed");
        assert!(l.saw(false, t + ms(4000)).is_some());
        assert!(l.closing(t + ms(4000)).is_some());
    }

    /// "shown": the key parks the windows (nothing shows); Mission Control seen, they go home
    /// with no park to wait for; none seen within the key's wait, their frames are set again.
    #[test]
    fn shown_mode_parks_at_the_key_and_puts_home_once_mission_control_shows() {
        let t = Instant::now();
        let mut l = Lends::default();
        assert!(l.hint(Hint::Key(Trigger::MissionControl), t));
        assert_eq!(l.wanted(), Some(By::KeyPark), "the parks start at the key");
        l.plan(vec![w(1), w(2)], t);
        assert!(l.parks_only(), "nothing goes home before Mission Control shows");
        assert_eq!(l.take_queue(), vec![w(1), w(2)]);
        l.parked_early([(w(1), 10), (w(2), 20)]);
        let done = l.round_done(t + ms(20)).expect("the parks' summary");
        assert_eq!((done.by, done.parked, done.lent), (By::KeyPark, 2, 0));
        assert!(l.out().is_empty() && l.wants_fast(t + ms(900)), "nothing out; the wait is watched");
        assert_eq!(l.early_back(t + ms(900)), None, "the key's wait lasts");
        // Mission Control shows: a round of its own, the windows parked handed to it.
        assert!(l.saw(true, t + ms(80)).is_some());
        assert_eq!(l.wanted(), Some(By::Shown));
        assert_eq!(l.early_back(t + KEY_WAIT + ms(10)), None, "it showed: they go home");
        l.plan(vec![w(1), w(2), w(3)], t + ms(80));
        assert!(!l.parks_only());
        assert_eq!(l.take_early(), vec![(w(1), 10), (w(2), 20)]);
        assert_eq!(l.take_queue(), vec![w(1), w(2), w(3)], "w(3) hid since: parked now");
        // No Mission Control after the next key: the parks undone.
        let mut l = Lends::default();
        assert!(l.hint(Hint::Key(Trigger::MissionControl), t));
        l.plan(vec![w(1)], t);
        l.take_queue();
        l.parked_early([(w(1), 10)]);
        assert_eq!(l.early_back(t + KEY_WAIT - ms(1)), None);
        assert_eq!(l.early_back(t + KEY_WAIT), Some(vec![(w(1), 10)]));
        assert_eq!(l.early_back(t + KEY_WAIT + ms(10)), None, "once");
        assert!(!l.wants_fast(t + KEY_WAIT + ms(10)));
        // Turned off meanwhile: undone at once.
        let mut l = Lends::default();
        assert!(l.hint(Hint::Key(Trigger::MissionControl), t));
        l.plan(vec![w(1)], t);
        l.take_queue();
        l.parked_early([(w(1), 10)]);
        l.set_mode(Mode::Off);
        assert_eq!(l.early_back(t + ms(5)), Some(vec![(w(1), 10)]));
        // "key" lends at the key as before: nothing parked only.
        let mut l = Lends::default();
        l.set_mode(Mode::Key);
        assert!(l.hint(Hint::Key(Trigger::MissionControl), t));
        assert_eq!(l.wanted(), Some(By::Key));
        l.plan(vec![w(1)], t);
        assert!(!l.parks_only());
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
        assert_eq!(l.next_window(), Some(w(1)));
        l.lent(out(1));
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
        assert_eq!(l.next_window(), Some(w(1)));
        l.lent(out(1));
        l.set_mode(Mode::Off);
        assert_eq!(l.next_window(), None, "no more lends");
        assert!(l.saw(false, t + ms(900)).is_some());
        assert!(l.closing(t + ms(900)).is_some());
    }

    #[test]
    fn opened_again_before_the_take_backs_the_windows_stay_out() {
        let t = Instant::now();
        let mut l = Lends::default();
        l.saw(true, t);
        l.plan(vec![w(1)], t);
        l.next_window();
        l.lent(out(1));
        l.round_done(t + ms(10));
        l.saw(false, t + ms(1000));
        assert!(l.closing(t + ms(1000)).is_some());
        assert!(l.saw(true, t + ms(1020)).is_some());
        assert_eq!(l.closing(t + ms(1030)), None, "it shows again: nothing goes back");
        assert_eq!(l.wanted(), Some(By::Shown), "and what hid meanwhile is put home too");
        l.plan(vec![w(1), w(2)], t + ms(1030));
        assert_eq!(l.next_window(), Some(w(2)), "the one out already is not lent twice");
    }

    /// Out: 1 and 2 of the stage picked (app 10), 3 of another (app 30).
    fn three_out(t: Instant) -> Lends {
        let mut l = Lends::default();
        l.saw(true, t);
        l.plan(vec![w(1), w(2), w(3)], t);
        l.take_queue();
        for (id, pid) in [(1, 10), (2, 10), (3, 30)] {
            l.lent(Out { id: w(id), pid, parked: Some(Rect::new(2558.0, 1407.0, 520.0, 424.0)) });
        }
        l.round_done(t + ms(30));
        l
    }

    /// Round mc-pick: the windows put home that the activation's switch brings in go to their
    /// frames; none for a switch that brings in no window put home.
    #[test]
    fn the_switchs_arrivals_that_are_out_go_to_their_frames() {
        let l = three_out(Instant::now());
        let ids = |v: Vec<Out>| v.iter().map(|o| o.id).collect::<Vec<_>>();
        assert_eq!(ids(to_frames(l.out(), &[], &[w(1), w(2), w(7)])), vec![w(1), w(2)], "the stage picked: its windows out, not one never lent");
        assert_eq!(ids(to_frames(l.out(), &[w(1)], &[w(1), w(2)])), vec![w(2)], "one placed already is not set again");
        assert!(to_frames(l.out(), &[], &[w(7)]).is_empty(), "a stage never lent (the app in front's)");
        assert!(to_frames(l.out(), &[], &[]).is_empty(), "no switch");
    }

    /// The pick: its windows placed while Mission Control closes; its switch held until
    /// Mission Control's own window is gone, then played once, and not held again; the
    /// windows placed wait for it at the close (the app picked is in front) and are left to
    /// it once their stage is in front.
    #[test]
    fn a_picks_switch_waits_for_mission_controls_own_window_to_go() {
        let t = Instant::now();
        let mut l = three_out(t);
        let pick = t + ms(1000);
        assert!(l.pick_open(pick), "it shows: a switch now may be its pick");
        l.placed([w(1), w(2), w(9)]);
        assert_eq!(l.placed_ids(), &[w(1), w(2)], "only windows out");
        assert!(l.is_placed(w(1)) && !l.is_placed(w(3)));
        l.saw_own(true);
        let by = PickBy::App(10);
        assert_eq!(l.pick_switch(by, pick), PickSwitch::AfterClose, "the stage left reaches its frames first");
        assert_eq!(l.pick_held(), Some(by));
        assert!(l.wants_fast(pick + ms(10)) && l.look_due(pick + ms(10)), "its close looked for at the fast tick");
        assert!(!l.look_due(pick + ms(20)), "not more often than the watch's pause");
        assert_eq!(l.pick_switch(by, pick + ms(50)), PickSwitch::AfterClose, "a second activation of the same app: still held");
        assert_eq!(l.pick_switch(PickBy::Window(w(1), 10), pick + ms(60)), PickSwitch::AfterClose, "its window's focus");
        assert_eq!(l.pick_held(), Some(by), "held as it was, its wait not started again");
        assert_eq!(l.pick_due(pick + ms(300)), None, "its own window still listed");
        // Its own window gone, the Dock's cover still listed: the switch plays.
        l.saw_own(false);
        assert_eq!(l.pick_due(pick + ms(530)), Some(by));
        assert_eq!(l.pick_due(pick + ms(540)), None, "once");
        assert_eq!(l.pick_switch(by, pick + ms(540)), PickSwitch::Now, "its wait over: not held again");
        assert_eq!(l.pick_held(), None);
        // Closed: the windows placed wait for their switch (their app in front), the rest go back.
        assert!(l.saw(false, pick + ms(800)).is_some());
        assert_eq!(fate(false, true, ms(5)), Fate::Wait, "placed, the switch not shown yet");
        l.settle(w(3), Settled::Back);
        l.settle(w(1), Settled::Picked);
        l.settle(w(2), Settled::Picked);
        assert!(l.placed_ids().is_empty(), "left to the switch: placed no more");
        let back = l.back_done(pick + ms(900)).expect("the summary");
        assert_eq!((back.back, back.picked), (1, vec![w(1), w(2)]));
        assert!(!l.pick_open(pick + ms(900)), "nothing out");
    }

    #[test]
    fn a_pick_plays_at_once_once_mission_control_is_gone_or_its_wait_is_over() {
        let t = Instant::now();
        // Mission Control's own window gone already at the activation: no wait.
        let mut l = three_out(t);
        l.saw_own(false);
        assert_eq!(l.pick_switch(PickBy::App(10), t + ms(100)), PickSwitch::Now);
        // Closed (the activation after the close, within the pick's wait): no wait.
        let mut l = three_out(t);
        l.saw_own(true);
        assert!(l.saw(false, t + ms(200)).is_some());
        assert!(l.pick_open(t + ms(300)));
        assert_eq!(l.pick_switch(PickBy::Window(w(1), 10), t + ms(300)), PickSwitch::Now);
        assert!(!l.pick_open(t + ms(200) + PICK_WAIT), "long closed: no pick any more");
        // Its own window never seen gone: the switch plays after the hold all the same.
        let mut l = three_out(t);
        l.saw_own(true);
        assert_eq!(l.pick_switch(PickBy::App(10), t), PickSwitch::AfterClose);
        assert_eq!(l.pick_due(t + PICK_HOLD - ms(1)), None);
        assert_eq!(l.pick_due(t + PICK_HOLD), Some(PickBy::App(10)));
        // Another app activated meanwhile: the pick dropped, nothing plays for it.
        let mut l = three_out(t);
        l.saw_own(true);
        assert_eq!(l.pick_switch(PickBy::App(10), t), PickSwitch::AfterClose);
        assert_eq!(l.drop_pick(), Some(PickBy::App(10)));
        assert!(l.saw(false, t + ms(500)).is_some());
        assert_eq!(l.pick_due(t + ms(500)), None);
        // Opened again before it played: dropped.
        let mut l = three_out(t);
        l.saw_own(true);
        l.pick_switch(PickBy::App(10), t);
        l.saw(false, t + ms(100));
        l.saw(true, t + ms(120));
        assert_eq!(l.pick_held(), None);
        assert_eq!(PickBy::Window(w(4), 30).pid(), 30);
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
