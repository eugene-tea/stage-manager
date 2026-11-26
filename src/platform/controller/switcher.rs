//! ⌘Tab's switcher on the WM thread (round cmd-tab): the app it selects gets its stage's
//! window put home before ⌘'s release, as a Dock press does ([`Model::dock_home`]).
//!
//! Under the Spaces engine a window in a stage Space is on no Desktop for macOS. ⌘Tab to an
//! app whose windows here all wait in stage Spaces, while it has a window on another Desktop,
//! took macOS to that Desktop and back (S3d gap 5; the owner's VS Code and Mail on 11-15).
//! The switcher shows while ⌘ is held after a Tab and activates its app at ⌘'s release; the
//! keys reach the input monitor while it shows ([`crate::engine::switcher::Switcher`]). The
//! first Tab selects the app used before the one in front, always; the Dock builds the
//! switcher's list only ~180 ms after it (live 11-15, macOS 27.2), and a quicker ⌘Tab never
//! shows it, while macOS activates the app 3-12 ms after ⌘'s release and picks the Desktop
//! then. So at the first Tab the app is predicted from the order of the apps' activations
//! ([`switcher::predict`]) and its window put home at once; once the Dock's list shows
//! (`AXCreated`) and at each change of its selection (`AXSelectedChildrenChanged`), both
//! observed on the Dock's application element, the switcher's own selection
//! ([`switcher_ax::read`]: `AXProcessSwitcherList`, `AXSelectedChildren`, the button's title
//! or its place in the order) corrects it. The log says which decided. When that app has a
//! stage here, every window of it here waits off every Desktop and macOS counts one of its
//! windows on another Desktop, the key window of its most recent stage here is put home
//! ([`WindowEngine::lend_home`]); the selection moving on, a key not followed or ⌘ let go on
//! another app take it back.
//!
//! **Its end heard** (round r2-lends): Escape never reaches the input monitor while the
//! switcher shows (live 11-15, the switcher takes it), nor does ⌘'s release always. The HID
//! key tap ([`crate::platform::mission_key_tap`]) is on from the Tab heard to the switcher's
//! end ([`mission_key_tap::switcher_up`], whatever Mission Control's mode) and forwards
//! Escape, ⌘'s release and a mouse-down ([`Controller::switcher_tap`]): Escape takes the
//! window back at once; ⌘'s release on its app starts the wait for that app's activation
//! ([`crate::engine::lend_wait::LendWait`]), which with its switch confirms it, while another
//! app's activation refutes it, and [`switcher::ACTIVATION_WAIT`] is only the cap (long: the
//! window waits parked off the screen); a mouse-down is a pick, its activation at the mouse's
//! release ([`switcher::UNREAD_WAIT`]). Its app already in front at the release (no
//! activation comes for it) counts as activated. A tap that does not hear (macOS refused
//! it, or keeps it off for secure input) leaves the time before it: the wait as short as
//! before ([`switcher::SILENT_ACTIVATION_WAIT`]), a missing activation taken for a cancel.
//!
//! **Parked while lent** (round cmdtab-park): at its full frame the window put home showed
//! under the front stage, and browsing the switcher made it blink in and out (the owner,
//! live 11-15). It is moved to its park frame (the Corner engine's geometry: its size at its
//! display's corner, a sliver at most) while still in its stage Space, then put home; taken
//! back, it goes into its stage Space first, then its frame is restored
//! ([`crate::engine::lend_park`]). The log's "put home while the switcher shows" says
//! `parked` and `park_ms`, "goes back" says `restored`. A park that does not take lends all
//! the same at the first Tab (a quick ⌘Tab's only chance), and not at a later selection
//! (logged "could not be parked off the screen"). Its move notices are dropped until its
//! frame is set again ([`WindowEngine::parked_lend`]); the activation's switch sets the
//! frame the model asks for.
//!
//! Nothing is polled: the Dock is asked at a switcher key and at its own notices. The list
//! gone (`AXUIElementDestroyed` with the switcher up) starts the activation wait; for the keys
//! it counts as the release when ⌘'s own release is not heard within
//! [`switcher::GONE_WAIT`]. Both waits run on the fast tick, armed only then. The
//! tick asks the live modifiers only while the switcher is up (a release the monitor
//! missed). The Corner engine: [`Model::dock_home`] answers nothing, nothing is read or put
//! home.
//!
//! A ⌘Tab let go before its Tab is handled on this thread gets nothing put home: macOS
//! decides at the activation, as before (`⌘ let go … lent=false` in the log, with the time
//! from the Tab).
//!
//! **A mouse pick** (round cmdtab-mouse-pick): with ⌘ held, hovering an icon moves the
//! switcher's selection (the Dock's notice is read as any other) and a click dismisses it,
//! activating that app. The keys know nothing of the mouse. Once the Dock's list was seen,
//! a selection step that finds it gone keeps the Dock's last reading and starts the
//! activation wait ([`switcher::choose`], [`switcher::Choice::Gone`],
//! [`switcher::UNREAD_WAIT`]); a guess from the keys there took the window put home back as
//! the click activated its app, and macOS slid to the other Desktop and back (live 11-18).
//!
//! [`Model::dock_home`]: crate::engine::model::Model::dock_home
//! [`WindowEngine::lend_home`]: crate::platform::window_engine::WindowEngine::lend_home
//! [`WindowEngine::parked_lend`]: crate::platform::window_engine::WindowEngine::parked_lend

use crate::platform::clock;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Controller;
use super::arrival::Lent;
use super::slide_watch::LendNote;
use crate::engine::corner;
use crate::engine::lend_park::{ParkAsk, Zone};
use crate::engine::model::{AppFacts, WindowId};
use crate::engine::lend_wait::LendWait;
use crate::engine::switcher::{self, Act, Choice, Heard, Key, List, Switcher};
use crate::platform::ctl::CmdTabProbe;
use crate::platform::input::{self, InputEvent, InputKind};
use crate::platform::observe::Observer;
use crate::platform::ax::{AxElement, AxObserver};
use crate::platform::window_engine::LendPark;
use crate::platform::mission_key_tap::{self, HeardAt};
use crate::platform::{apps, dock, space_keys, switcher_ax, switcher_notes, windows};

/// The Dock's notices about the switcher (observed on its application element, live 11-15):
/// the list made (its selection already set), its selection changed, an element gone.
const NOTICES: [&str; 3] = ["AXCreated", "AXSelectedChildrenChanged", "AXUIElementDestroyed"];

/// A switcher up this long with no key is forgotten (its release missed and ⌘ not seen
/// held either is caught sooner, at the tick).
const SWITCHER_MAX: Duration = Duration::from_secs(60);

/// A Space change this long after ⌘'s release counts as the switcher's activation's (logged).
const RELEASE_SEEN_FOR: Duration = Duration::from_millis(2000);

/// Who said which app the switcher selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum By {
    /// The Dock's Accessibility (the switcher's own selection).
    Dock,
    /// The keys and the order of the apps' activations ([`switcher::predict`]).
    Keys,
}

impl By {
    fn name(self) -> &'static str {
        match self {
            By::Dock => "the Dock's switcher",
            By::Keys => "the keys and the order of activations",
        }
    }
}

/// ⌘Tab's switcher as the controller follows it.
#[derive(Debug, Default)]
pub(super) struct SwitcherWatch {
    /// The switcher from the keys.
    keys: Switcher,
    /// The app it selects, and who said so.
    selected: Option<(i32, By)>,
    /// The Dock's pid, read when the switcher came up.
    dock: Option<i32>,
    /// When it came up (the first Tab), and its last key.
    opened: Option<Instant>,
    last_key: Option<Instant>,
    /// The observer of the Dock's notices, and the Dock's pid it was made for (kept once made;
    /// the Dock sends them only while the switcher shows, or for its own menus).
    ax: Option<(i32, AxObserver)>,
    /// The Dock's pid an observer was last tried for (one try per Dock process).
    ax_tried: Option<i32>,
    /// ⌘ let go with the switcher up: when, and whether a window was put home by then.
    released: Option<(Instant, bool)>,
    /// The Dock's list was seen shown since the first Tab: a step that finds it gone then is
    /// the switcher's end, not a selection ([`switcher::choose`]).
    listed: bool,
    /// The last app activated, and when: the slide watch names the app a ⌘Tab activated
    /// when it is not the one selected ([`Self::switcher_activated`]).
    activated: Option<(i32, Instant)>,
    /// `ctl debug-cmd-tab … nolend`: nothing put home for this switcher (the probe's measure
    /// of macOS's own way).
    lend_off: bool,
    /// The key tap heard the switcher's end (Escape, ⌘'s release, a click) for this switcher:
    /// its activation's wait takes the long cap ([`switcher::activation_cap`]).
    tap_heard: bool,
}

impl Controller {
    /// A key event from the input monitor: ⌘ pressed or let go, or a key pressed. Anything but
    /// a switcher key with the switcher down returns at once.
    pub(super) fn switcher_key(&mut self, ob: &Observer, event: &InputEvent, now: Instant) {
        let key = match (event.kind, event.key) {
            (InputKind::CommandKey, _) => Key::Command(event.cmd),
            (InputKind::KeyDown, Some(code)) => Key::Down { code, cmd: event.cmd, shift: event.shift },
            _ => return,
        };
        // Only where a hidden window is on no Desktop for macOS (the Spaces engine): under
        // the Corner engine nothing is read, observed or put home.
        if !self.model.as_ref().is_some_and(|m| !m.macos_counts_hidden()) {
            if matches!(key, Key::Command(false) | Key::Down { code: switcher::KEY_TAB, cmd: true, .. }) {
                switcher_notes::note("heard", || format!("{key:?} (nothing done: no model, or the Corner engine)"));
            }
            return;
        }
        if key == Key::Command(true) && self.switcher.ax.is_none() && self.switcher.ax_tried.is_none() {
            // The Dock's notices observed before the first switcher shows, so a Tab the
            // monitor misses is still seen when the Dock lists the switcher.
            self.switcher.dock = dock::dock_pid();
            self.watch_dock();
        }
        let heard = self.switcher.keys.hear_at(key, now);
        if heard != Heard::Nothing || self.switcher.keys.is_open() {
            switcher_notes::note("heard", || format!("{key:?} -> {heard:?}"));
        }
        self.switcher_heard(ob, heard, true, now);
        self.switcher_arm_tap();
    }

    /// What the key tap heard while the switcher is up ([`mission_key_tap`]): Escape (the
    /// switcher's cancel, never heard by the input monitor while it shows), ⌘ let go, or a
    /// mouse-down (a pick: the switcher activates the app under the pointer at the mouse's
    /// release). Fed to the switcher as the keys it already has names for; told at `heard.at`,
    /// when the tap got it. Nothing while the switcher is down: a cancel or release the
    /// monitor told first leaves the tap's copy nothing to do.
    pub(super) fn switcher_tap(&mut self, ob: &Observer, heard: HeardAt) {
        if !self.switcher.keys.is_open() || !self.model.as_ref().is_some_and(|m| !m.macos_counts_hidden()) {
            return;
        }
        let at = heard.at;
        let key = match heard.what {
            mission_key_tap::Heard::Key { code: switcher::KEY_ESCAPE, mods } => Key::Down { code: switcher::KEY_ESCAPE, cmd: mods.command, shift: false },
            mission_key_tap::Heard::CommandUp => Key::Command(false),
            mission_key_tap::Heard::Click => {
                self.switcher.tap_heard = true;
                self.switcher_picked(at);
                return;
            }
            mission_key_tap::Heard::Key { .. } => return,
        };
        self.switcher.tap_heard = true;
        let heard = self.switcher.keys.hear_at(key, at);
        switcher_notes::note("key tap", || format!("{key:?} -> {heard:?}"));
        tracing::info!(key = ?key, heard = ?heard, since_tab_ms = self.switcher.opened.map(|t| ms(t, at)), "cmd-tab: the key tap heard the switcher's end");
        self.switcher_heard(ob, heard, true, at);
        self.switcher_arm_tap();
    }

    /// A mouse-down heard by the key tap at `at` with the switcher up: a pick (the switcher
    /// activates the app under the pointer at the mouse's release, its selection the Dock's
    /// last reading) or a click outside it. The window put home waits from now for its app's
    /// activation ([`switcher::UNREAD_WAIT`]); another app's activation refutes it. The keys
    /// still have the switcher up: ⌘'s release, or the list gone, closes it for them.
    fn switcher_picked(&mut self, at: Instant) {
        let cap = switcher::activation_cap(true, mission_key_tap::hears());
        if let Some(l) = self.lent.as_mut().filter(|l| l.switcher) {
            l.wait.begin(at, cap);
            tracing::info!(pid = l.pid, window = l.window.0, cap = cap.name, "cmd-tab: a click while the switcher shows (heard by the key tap): the window put home stays for its app's activation at the mouse's release");
            switcher_notes::note("picked", || format!("window={} cap={}", l.window.0, cap.name));
        }
    }

    /// The key tap on while the switcher is up, off once it closes (unless Mission Control
    /// wants it).
    fn switcher_arm_tap(&self) {
        mission_key_tap::switcher_up(self.switcher.keys.is_open());
    }

    /// What the switcher did (`heard`; `by_key`: told by a key, else by the Dock or the tick)
    /// carried out: the selection read, the window put home or taken back.
    fn switcher_heard(&mut self, ob: &Observer, heard: Heard, by_key: bool, now: Instant) {
        match heard {
            Heard::Nothing => return,
            Heard::Selected { steps, first } => {
                if first {
                    self.switcher.opened = Some(now);
                    self.switcher.selected = None;
                    self.switcher.released = None;
                    self.switcher.listed = false;
                    self.switcher.tap_heard = false;
                    self.switcher.dock = dock::dock_pid();
                    self.watch_dock();
                }
                self.switcher.last_key = Some(now);
                match self.switcher_selection(ob, steps, by_key, now) {
                    Choice::Dock(pid) => self.switcher.selected = Some((pid, By::Dock)),
                    Choice::Guess(pid) => self.switcher.selected = pid.map(|p| (p, By::Keys)),
                    Choice::Keep => return,
                    Choice::Gone => {
                        self.switcher_list_gone(now, true);
                        return;
                    }
                }
            }
            Heard::Released { steps } => {
                let lent = self.lent.is_some_and(|l| l.switcher);
                // When the monitor heard the release: a Space change is measured to its notice.
                self.switcher.released = Some((self.received_at(now), lent));
                tracing::info!(
                    steps,
                    selected = ?self.switcher.selected.map(|(pid, _)| pid),
                    by = self.switcher.selected.map(|(_, by)| by.name()),
                    since_tab_ms = self.switcher.opened.map(|t| ms(t, now)),
                    lent,
                    "cmd-tab: ⌘ let go with the switcher up"
                );
            }
            Heard::Cancelled | Heard::Lost => {}
        }
        let selected = self.switcher.selected.map(|(pid, _)| pid);
        let lent = self.lent.filter(|l| l.switcher).map(|l| (l.pid, l.window));
        let acts = switcher::plan(heard, selected, lent, |pid| self.switcher_home(ob, pid));
        // The first Tab's lend is a quick ⌘Tab's only chance: made even when its park does
        // not take ([`crate::engine::lend_park::ParkAsk::must`]).
        let first = matches!(heard, Heard::Selected { first: true, .. });
        for act in acts {
            self.switcher_act(ob, act, first, by_key, now);
        }
        if let (Heard::Released { .. }, Some(pid)) = (heard, selected) {
            // Watched for a Desktop slide from ⌘'s release ([`Self::slide_watch_cmd_tab`]).
            let lent = self.lent.is_some_and(|l| l.pid == pid && l.switcher);
            let since_tab_ms = self.switcher.opened.map(|t| ms(t, now));
            let opened = self.switcher.opened;
            let activated = self.switcher.activated.filter(|(_, t)| opened.is_some_and(|o| *t >= o)).map(|(p, _)| p);
            self.slide_watch_cmd_tab(ob, pid, lent, since_tab_ms, activated, now);
        }
        match heard {
            Heard::Released { .. } | Heard::Cancelled => {
                self.switcher.selected = None;
                self.switcher.opened = None;
                self.switcher.lend_off = false;
            }
            // Still up, its selection unknown until a key the Dock answers for.
            Heard::Lost => self.switcher.selected = None,
            _ => {}
        }
    }

    /// Whose word the selection `steps` from the app in front takes (`by_key`: the step came
    /// from a key, else from a Dock notice): the Dock's answer when it gives one (the
    /// button's title as a running app's name, else its place in the order of activations),
    /// else the prediction from the keys, unless the Dock's list already showed
    /// ([`switcher::choose`]); logged with who decided.
    fn switcher_selection(&mut self, ob: &Observer, steps: Option<i32>, by_key: bool, now: Instant) -> Choice {
        let read = self.switcher.dock.map_or(switcher_ax::Read::NoSwitcher, switcher_ax::read);
        let (order, running) = self.switcher_order(ob);
        let predicted = steps.and_then(|s| switcher::predict(&order, running, s));
        let docked = read.pid().or_else(|| read.index().and_then(|i| order.get(i).copied()));
        let list = match read {
            switcher_ax::Read::NoSwitcher => List::Gone,
            switcher_ax::Read::Unanswered => List::Unanswered,
            switcher_ax::Read::Shown { .. } => List::Shown,
        };
        let choice = switcher::choose(docked, list, self.switcher.listed, by_key, predicted);
        self.switcher.listed |= list == List::Shown;
        let chosen = match choice {
            Choice::Dock(pid) => Some((pid, By::Dock)),
            Choice::Guess(pid) => pid.map(|p| (p, By::Keys)),
            Choice::Keep | Choice::Gone => self.switcher.selected,
        };
        let took_ms = ms(now, clock::now());
        tracing::info!(
            steps,
            step = if by_key { "a key" } else { "the Dock's notice" },
            selected = ?chosen.map(|(pid, _)| pid),
            by = chosen.map(|(_, by)| by.name()),
            choice = choice.name(),
            dock = read.name(),
            dock_pid = ?docked,
            dock_index = ?read.index(),
            predicted = ?predicted,
            agree = docked.zip(predicted).map(|(a, b)| a == b),
            known_order = order.len(),
            running,
            took_ms,
            "cmd-tab: the switcher selects an app"
        );
        switcher_notes::note("selected", || format!("{chosen:?} {} dock={} predicted={predicted:?} took_ms={took_ms}", choice.name(), read.name()));
        choice
    }

    /// The apps most recently activated first, the app in front first, as far as this run saw
    /// them activated; and how many apps the switcher lists (the running regular apps).
    fn switcher_order(&self, ob: &Observer) -> (Vec<i32>, usize) {
        let mut order = ob.apps_by_recent_activation();
        let running = ob.regular_app_count();
        match apps::frontmost_pid() {
            Some(front) if order.first() != Some(&front) => {
                if !ob.is_regular_app(front) {
                    // Our own Settings, or an app the switcher does not list: the order from
                    // here is not the switcher's.
                    return (Vec::new(), running);
                }
                order.retain(|p| *p != front);
                order.insert(0, front);
            }
            _ => {}
        }
        (order, running)
    }

    /// The window to put home for app `pid` selected in the switcher
    /// ([`crate::engine::model::Model::dock_home`]): `None` under the Corner engine (asked
    /// before anything is read), for the probe's `nolend`, or while a Space change settles.
    fn switcher_home(&self, ob: &Observer, pid: i32) -> Option<WindowId> {
        let m = self.model.as_ref()?;
        if self.switcher.lend_off || m.macos_counts_hidden() || self.frozen() {
            return None;
        }
        m.dock_gates(pid).ok()?;
        let list = windows::list_windows();
        let displays = m.displays().to_vec();
        let visible_here = dock::visible_window_of(&list, pid, |r| displays.iter().any(|d| self.engine.is_hidden(d, r)));
        let (_, unplaced) = self.unplaced_elsewhere(ob, pid);
        let facts = AppFacts {
            hidden: apps::is_hidden(pid).unwrap_or(false),
            unplaced,
            active: apps::frontmost_pid() == Some(pid),
            visible_here,
        };
        m.dock_home(pid, facts)
    }

    /// The park asked for window `window`'s lend (round cmdtab-park): its frame's size at its
    /// display's park corner ([`corner::park_frame_of`], the Corner engine's geometry and
    /// request), moved through its element; `must`: lend even when the park does not take;
    /// `ax_seconds`: how long its move may wait for the app (`None`: the engine's bound).
    /// `None` when the model or the observer does not know the window (lent unparked then).
    pub(super) fn lend_park(&self, ob: &Observer, window: WindowId, must: bool, ax_seconds: Option<f32>) -> Option<LendPark> {
        let m = self.model.as_ref()?;
        let w = m.window(window)?;
        let display = *m.display(w.meta.display);
        let element = ob.element(window.0).or_else(|| ob.window(window.0).map(|o| o.element.clone()))?;
        let at = corner::park_frame_of(w.meta.frame, &display, m.options().park);
        Some(LendPark { ask: ParkAsk { at, zone: Zone::Corner, display, frame: w.meta.frame, must, kept: false }, window: element, ax_seconds })
    }

    /// Carries out one act of the switcher's plan (`first`: the switcher's first selection;
    /// `by_key`: told by a key the monitor or the key tap heard, not taken from the Dock's
    /// list gone or the live modifiers).
    fn switcher_act(&mut self, ob: &Observer, act: Act, first: bool, by_key: bool, now: Instant) {
        match act {
            Act::Lend { pid, window } => {
                if self.lent.is_some() {
                    // A Dock press's window still home goes back first.
                    let outcome = self.lent.as_mut().and_then(|l| l.wait.refute("⌘Tab's switcher selects another app", now));
                    self.lent_ended(outcome, now);
                }
                let park = self.lend_park(ob, window, first, None);
                let asked = park.is_some();
                let lend = self.engine.lend_home(window, park);
                self.slide_watch_lend(pid, LendNote { home: lend.home, parked: lend.parked, asked, park_ms: lend.park_ms });
                let parked = lend.parked.unwrap_or(false);
                if lend.home {
                    self.lent = Some(Lent { window, pid, at: now, switcher: true, wait: LendWait::held(pid) });
                    tracing::info!(
                        pid,
                        window = window.0,
                        parked,
                        park_ms = lend.park_ms,
                        since_tab_ms = self.switcher.opened.map(|t| ms(t, clock::now())),
                        "cmd-tab: the app's windows here wait off every Desktop and macOS counts one elsewhere: its stage's window is put home while the switcher shows"
                    );
                    if asked && !parked {
                        tracing::info!(pid, window = window.0, park_ms = lend.park_ms, "cmd-tab: the window put home could not be parked off the screen (its app did not move it): put home at its frame for the first Tab");
                    }
                    switcher_notes::note("lent", || format!("pid={pid} window={} parked={parked} park_ms={}", window.0, lend.park_ms));
                } else if asked && lend.parked == Some(false) {
                    tracing::info!(
                        pid,
                        window = window.0,
                        parked,
                        park_ms = lend.park_ms,
                        "cmd-tab: the app's window here could not be parked off the screen (its app did not move it): nothing put home for this selection; macOS goes where it counts one"
                    );
                    switcher_notes::note("not lent", || format!("pid={pid} window={} parked=false park_ms={}", window.0, lend.park_ms));
                } else {
                    tracing::info!(pid, window = window.0, "cmd-tab: the app's window here could not be put home; macOS goes where it counts one");
                }
            }
            Act::TakeBack { window, why } => {
                if self.lent.is_some_and(|l| l.window == window) {
                    let outcome = self.lent.as_mut().and_then(|l| l.wait.refute(why, now));
                    self.lent_ended(outcome, now);
                }
            }
            Act::Release { window } => {
                let heard = by_key && mission_key_tap::hears();
                let heard = heard || (self.switcher.tap_heard && mission_key_tap::hears());
                let cap = switcher::activation_cap(false, heard);
                // Its app already in front at the release: no activation comes for it (or its
                // notice is on its way): counted as activated, its switch shows it.
                let front = apps::frontmost_pid();
                // From when the release was heard (the monitor's or the tap's stamp): another
                // app's activation stamped after it refutes the lend.
                let at = self.received_at(now);
                if let Some(l) = self.lent.as_mut().filter(|l| l.window == window && l.switcher) {
                    l.wait.begin(at, cap);
                    let in_front = front == Some(l.pid);
                    if in_front {
                        l.wait.activation(l.pid, now, now);
                    }
                    tracing::info!(
                        window = window.0,
                        activated = l.wait.is_activated(),
                        app_in_front = in_front,
                        cap = cap.name,
                        cap_ms = cap.limit.as_millis() as u64,
                        "cmd-tab: ⌘ let go on the app its window was put home for: kept for that app's activation and switch; another app's activation takes it back"
                    );
                    switcher_notes::note("released", || format!("window={} cap={} app_in_front={in_front}", window.0, cap.name));
                }
            }
        }
    }

    /// The Dock's notices observed for Dock process `pid` (round looks-forward: the Corner
    /// engine's look for Mission Control, while its public triggers serve): as
    /// [`Self::watch_dock`], once per Dock process.
    pub(super) fn watch_dock_of(&mut self, pid: i32) {
        if self.switcher.ax.as_ref().is_some_and(|(p, _)| *p == pid) || self.switcher.ax_tried == Some(pid) {
            return;
        }
        self.switcher.dock = Some(pid);
        self.watch_dock();
    }

    /// The Dock's notices observed no longer (the Corner engine, its public triggers at rest
    /// again): its observer goes, and may be made again for the same Dock later.
    pub(super) fn unwatch_dock(&mut self) {
        if self.switcher.ax.take().is_some() {
            self.switcher.ax_tried = None;
            tracing::debug!("mission-control: the Dock's notices are observed no longer");
        }
    }

    /// Makes the observer of the Dock's switcher notices, once per Dock process
    /// ([`NOTICES`], [`crate::platform::wm_thread::switcher_ax_callback`]).
    fn watch_dock(&mut self) {
        let Some(dock) = self.switcher.dock else { return };
        if self.switcher.ax.as_ref().is_some_and(|(pid, _)| *pid == dock) || self.switcher.ax_tried == Some(dock) {
            return;
        }
        self.switcher.ax = None;
        self.switcher.ax_tried = Some(dock);
        let observer = match AxObserver::new(dock, crate::platform::wm_thread::switcher_ax_callback) {
            Ok(o) => o,
            Err(e) => {
                tracing::info!(dock, "cmd-tab: the Dock's notices cannot be observed ({e}); the switcher is followed from the keys");
                return;
            }
        };
        let app = AxElement::application(dock);
        let added: Vec<String> = NOTICES
            .iter()
            .map(|n| match app.bounded(switcher_ax::SWITCHER_AX_SECONDS, |a| observer.add(a, n, 0)) {
                Ok(()) => format!("{n}: ok"),
                Err(e) => format!("{n}: {e}"),
            })
            .collect();
        tracing::info!(dock, ?added, "cmd-tab: the Dock's switcher notices are observed");
        self.switcher.ax = Some((dock, observer));
    }

    /// A notice from the Dock (`name`, [`NOTICES`]): the switcher's list made or its selection
    /// changed reads the selection again and corrects the window put home; the list gone
    /// with the switcher still up waits for the key on its way (Escape: back; ⌘ up: kept for
    /// the activation), and with none within [`switcher::GONE_WAIT`] counts as the release
    /// ([`Self::switcher_fast_tick`]). The list made while the keys missed the Tab opens the switcher, its selection
    /// the Dock's only. Nothing when the switcher is down otherwise (the Dock's menus).
    pub fn switcher_notice(&mut self, ob: &Observer, name: &str) {
        crate::platform::costs::bump(crate::platform::costs::Cost::DockNotice);
        let now = clock::now();
        if !self.model.as_ref().is_some_and(|m| !m.macos_counts_hidden()) {
            // Where macOS counts the hidden windows on their Desktop (the Corner engine) the
            // switcher is not followed; the Dock draws Mission Control too, and its notice
            // may come as it opens.
            if self.model.is_some() {
                self.upkeep_dock_notice(name, now);
            }
            return;
        }
        switcher_notes::note("dock notice", || name.to_string());
        // The Dock draws Mission Control too: its notice may come as it opens.
        self.upkeep_dock_notice(name, now);
        match name {
            "AXCreated" | "AXSelectedChildrenChanged" => {
                if !self.switcher.keys.is_open() {
                    if name != "AXCreated" || self.switcher.dock.is_none() {
                        return;
                    }
                    // Its list says it shows: the keys missed its Tab.
                    if switcher_ax::read(self.switcher.dock.unwrap_or_default()) == switcher_ax::Read::NoSwitcher {
                        return;
                    }
                    self.switcher.keys.open_unfollowed();
                    self.switcher.opened = Some(now);
                    tracing::info!("cmd-tab: the Dock shows the switcher although its Tab was not heard");
                }
                let steps = self.switcher.keys.steps();
                self.switcher_heard(ob, Heard::Selected { steps, first: false }, false, now);
                self.switcher_arm_tap();
            }
            "AXUIElementDestroyed" if self.switcher.keys.is_open() => {
                if self.switcher.dock.is_some_and(|d| switcher_ax::read(d) != switcher_ax::Read::NoSwitcher) {
                    return;
                }
                self.switcher_list_gone(now, false);
            }
            _ => {}
        }
    }

    /// The Dock's switcher list is gone with the switcher up for the keys: at its own notice
    /// (`unread` false), or found gone at a selection step ([`Choice::Gone`], `unread`: a
    /// mouse pick dismissed it with ⌘ held; its notice may come late or not at all, live
    /// 11-18). ⌘'s release may be on its way (the list goes ~1 ms after the key, the
    /// monitor's event comes later; Escape is never heard): it decides the keys; else the
    /// fast tick after GONE_WAIT. The window put home waits for its app's activation from now
    /// on (or from ⌘'s release, the later), [`switcher::UNREAD_WAIT`] when unread.
    fn switcher_list_gone(&mut self, now: Instant, unread: bool) {
        self.switcher.keys.list_gone(now);
        let cap = switcher::activation_cap(unread, self.switcher.tap_heard && mission_key_tap::hears());
        if let Some(l) = self.lent.as_mut().filter(|l| l.switcher) {
            l.wait.begin(now, cap);
            if unread {
                tracing::info!(
                    pid = l.pid,
                    window = l.window.0,
                    activated = l.wait.is_activated(),
                    cap = cap.name,
                    "cmd-tab: the Dock's switcher list is gone (a pick with the mouse): the window put home stays for its app's activation"
                );
            }
        }
        tracing::debug!(unread, "cmd-tab: the Dock's switcher list is gone; the key on its way decides");
    }

    /// True while the Dock's list is gone and its key not heard yet (at most
    /// [`switcher::GONE_WAIT`]), or a window put home for a closed switcher waits for its
    /// app's activation on a short cap (the key tap not hearing,
    /// [`switcher::SILENT_ACTIVATION_WAIT`]): the fast tick. The long caps are the tick's.
    pub(super) fn switcher_wants_fast_tick(&self) -> bool {
        self.switcher.keys.waits_gone() || self.lent_awaits_activation()
    }

    /// True while a window put home for the switcher waits for its app's activation on a
    /// short cap (under a second), the switcher closed.
    pub(super) fn lent_awaits_activation(&self) -> bool {
        self.lent.is_some_and(|l| {
            l.switcher && !l.wait.is_activated() && l.wait.cap().is_some_and(|c| c.limit < Duration::from_secs(1))
        })
    }

    /// App `pid` activated (its notice stamped `fired`), before anything else is judged of
    /// it: the slide watch's record of it, and a window put home for that app kept for the
    /// activation's switch ([`Self::lent_activated`]).
    pub(super) fn switcher_activated(&mut self, pid: i32, fired: Instant) {
        self.switcher.activated = Some((pid, clock::now()));
        self.lent_activated(pid, fired);
    }

    /// The fast tick (and the tick): the Dock's list gone [`switcher::GONE_WAIT`] ago with no
    /// key since is taken as ⌘'s release (a window put home then waits for its app's
    /// activation, [`switcher::ACTIVATION_WAIT`]).
    pub(super) fn switcher_fast_tick(&mut self, ob: &Observer, now: Instant) {
        let heard = self.switcher.keys.gone_due(now);
        if heard != Heard::Nothing {
            // The key tap hears Escape and ⌘'s release as they happen: none within the wait
            // means it did not hear (macOS refused it, or keeps it off).
            tracing::info!(
                tap_hears = mission_key_tap::hears(),
                "cmd-tab: the Dock's switcher is gone and no key came for it: taken as the release (the fallback for a key tap that does not hear)"
            );
            self.switcher_heard(ob, heard, false, now);
        }
        self.switcher_arm_tap();
    }

    /// The tick, while the switcher is up: ⌘ found let go without its event (the monitor
    /// missed it) counts as the release; a switcher with no key for [`SWITCHER_MAX`] is
    /// forgotten. Nothing is read while it is down.
    pub(super) fn switcher_tick(&mut self, ob: &Observer, now: Instant) {
        self.switcher_fast_tick(ob, now);
        if !self.switcher.keys.is_open() {
            return;
        }
        let quiet = self.switcher.last_key.is_some_and(|t| now.saturating_duration_since(t) >= SWITCHER_MAX);
        // A ⌘ this process posted (the ⌘Tab probe's, `ctl debug-cmd-tab`) may not show in the
        // session's state: its own release is posted. Only a test run posts one, so only a
        // test run asks.
        let probe_posting = std::env::var_os("STAGE_MANAGER_TEST").is_some() && space_keys::posted_command_down();
        if (!input::command_held() && !probe_posting) || quiet {
            let steps = self.switcher.keys.steps();
            self.switcher.keys.reset();
            let heard = if quiet { Heard::Lost } else { Heard::Released { steps } };
            tracing::info!(quiet, "cmd-tab: the switcher is taken for closed (⌘ not held, or no key for a minute), its release unheard");
            self.switcher_heard(ob, heard, false, now);
            if quiet {
                self.switcher.selected = None;
                self.switcher.opened = None;
                self.switcher.lend_off = false;
            }
        }
        self.switcher_arm_tap();
    }

    /// A Space change: logged with the time since ⌘'s release when it came right after one
    /// (how long macOS took to slide away for the switcher's activation, or that it slid
    /// although a window was put home).
    pub(super) fn switcher_space_changed(&mut self, now: Instant) {
        let Some((at, lent)) = self.switcher.released else { return };
        let seen = now.saturating_duration_since(at);
        if !crate::engine::margin::note("RELEASE_SEEN_FOR", crate::engine::margin::age("RELEASE_SEEN_FOR", now, at), RELEASE_SEEN_FOR, seen <= RELEASE_SEEN_FOR) {
            self.switcher.released = None;
            return;
        }
        let after = ms(at, now);
        tracing::info!(after_release_ms = after, lent, "cmd-tab: the Space changed after ⌘ was let go on the switcher");
        switcher_notes::note("space changed", || format!("after_release_ms={after} lent={lent}"));
    }

    /// `ctl debug-cmd-tab` in a build without the `probes` feature: refused.
    #[cfg(not(feature = "probes"))]
    pub(super) fn debug_cmd_tab(&mut self, _ob: &Observer, _probe: CmdTabProbe) -> Value {
        json!({ "ok": false, "error": "debug-cmd-tab is not in this build (built without the probes feature)" })
    }

    /// `ctl debug-cmd-tab` (test only): a posted ⌘Tab ([`crate::platform::switcher_probe`]),
    /// or its report. A release is refused unless the app the keys predict for that many Tabs
    /// is a `STAGE_MANAGER_ONLY` app; the probe also cancels at the release when the Dock
    /// names another.
    #[cfg(feature = "probes")]
    pub(super) fn debug_cmd_tab(&mut self, ob: &Observer, probe: CmdTabProbe) -> Value {
        use crate::platform::switcher_probe;
        let (release, tabs, hold_ms, gap_ms, lend) = match probe {
            CmdTabProbe::Result => return switcher_probe::result(),
            CmdTabProbe::Start { release, tabs, hold_ms, gap_ms, lend } => (release, tabs, hold_ms, gap_ms, lend),
        };
        let (order, running) = self.switcher_order(ob);
        let expected = switcher::predict(&order, running, i32::from(tabs));
        let tests = input::test_pids();
        let expected_test = expected.is_some_and(|p| tests.contains(&p));
        if release && !expected_test {
            return json!({
                "ok": false,
                "error": format!("after {tabs} Tab(s) the switcher is expected to select pid {expected:?}, not a STAGE_MANAGER_ONLY app; a release is refused (cancel never activates)"),
                "order": order,
                "running": running,
            });
        }
        let Some(dock) = dock::dock_pid() else { return json!({ "ok": false, "error": "the Dock does not run" }) };
        self.switcher.lend_off = !lend;
        let req = switcher_probe::Request { release, tabs, hold: Duration::from_millis(hold_ms), gap: Duration::from_millis(gap_ms), expected_test };
        match switcher_probe::start(req, dock, move |pid| tests.contains(&pid)) {
            Ok(()) => json!({
                "ok": true,
                "expected": expected,
                "expected_bundle": expected.and_then(apps::bundle_of),
                "order": order,
                "running": running,
                "lend": lend,
            }),
            Err(e) => json!({ "ok": false, "error": e }),
        }
    }
}

/// Milliseconds from `then` to `now`.
fn ms(then: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(then).as_millis() as u64
}
