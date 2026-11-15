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
//! ([`WindowEngine::lend_home`]); the selection moving on, Escape, a key not followed or ⌘
//! let go on another app take it back. ⌘ let go on that app keeps it for the activation,
//! whose switch takes it as after a Dock press ([`super::arrival::LEND_WAIT`] from the
//! release, then back unshown).
//!
//! Nothing is polled: the Dock is asked at a switcher key and at its own notices. The list
//! gone (`AXUIElementDestroyed` with the switcher up) decides nothing by itself: the list goes
//! ~1 ms after Escape's or ⌘'s key, before the monitor's event (live 11-15), so that key
//! decides; with none within [`switcher::GONE_WAIT`] (the fast tick, armed only then) it
//! counts as the release, and a window put home goes back [`super::arrival::LEND_WAIT`]
//! later unless its app's activation came. The
//! tick asks the live modifiers only while the switcher is up (a release the monitor
//! missed). The Corner engine: [`Model::dock_home`] answers nothing, nothing is read or put
//! home.
//!
//! A ⌘Tab let go before its Tab is handled on this thread gets nothing put home: macOS
//! decides at the activation, as before (`⌘ let go … lent=false` in the log, with the time
//! from the Tab).
//!
//! [`Model::dock_home`]: crate::engine::model::Model::dock_home
//! [`WindowEngine::lend_home`]: crate::platform::window_engine::WindowEngine::lend_home

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Controller;
use super::arrival::Lent;
use crate::engine::model::{AppFacts, WindowId};
use crate::engine::switcher::{self, Act, Heard, Key, Switcher};
use crate::platform::ctl::CmdTabProbe;
use crate::platform::input::{self, InputEvent, InputKind};
use crate::platform::observe::Observer;
use crate::platform::ax::{AxElement, AxObserver};
use crate::platform::{apps, dock, switcher_ax, switcher_probe, windows};

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
    /// `ctl debug-cmd-tab … nolend`: nothing put home for this switcher (the probe's measure
    /// of macOS's own way).
    lend_off: bool,
}

impl Controller {
    /// True while ⌘Tab's switcher is up as the keys show it.
    pub(super) fn switcher_up(&self) -> bool {
        self.switcher.keys.is_open()
    }

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
                switcher_probe::note("heard", format!("{key:?} (nothing done: no model, or the Corner engine)"));
            }
            return;
        }
        if key == Key::Command(true) && self.switcher.ax.is_none() && self.switcher.ax_tried.is_none() {
            // The Dock's notices observed before the first switcher shows, so a Tab the
            // monitor misses is still seen when the Dock lists the switcher.
            self.switcher.dock = dock::dock_pid();
            self.watch_dock();
        }
        let heard = self.switcher.keys.hear(key);
        if heard != Heard::Nothing || self.switcher.keys.is_open() {
            switcher_probe::note("heard", format!("{key:?} -> {heard:?}"));
        }
        self.switcher_heard(ob, heard, now);
    }

    /// What the switcher did (`heard`) carried out: the selection read, the window put home or
    /// taken back.
    fn switcher_heard(&mut self, ob: &Observer, heard: Heard, now: Instant) {
        match heard {
            Heard::Nothing => return,
            Heard::Selected { steps, first } => {
                if first {
                    self.switcher.opened = Some(now);
                    self.switcher.selected = None;
                    self.switcher.released = None;
                    self.switcher.dock = dock::dock_pid();
                    self.watch_dock();
                }
                self.switcher.last_key = Some(now);
                self.switcher.selected = self.switcher_selection(ob, steps, now);
            }
            Heard::Released { steps } => {
                let lent = self.lent.is_some_and(|l| l.switcher);
                self.switcher.released = Some((now, lent));
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
        for act in acts {
            self.switcher_act(act, now);
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

    /// The app the switcher selects `steps` from the app in front: the Dock's answer when it
    /// gives one (the button's title as a running app's name, else its place in the order of
    /// activations), else the prediction from the keys; logged with who decided.
    fn switcher_selection(&mut self, ob: &Observer, steps: Option<i32>, now: Instant) -> Option<(i32, By)> {
        let read = self.switcher.dock.map_or(switcher_ax::Read::NoSwitcher, switcher_ax::read);
        let (order, running) = self.switcher_order(ob);
        let predicted = steps.and_then(|s| switcher::predict(&order, running, s));
        let docked = read.pid().or_else(|| read.index().and_then(|i| order.get(i).copied()));
        let chosen = match (docked, predicted) {
            (Some(pid), _) => Some((pid, By::Dock)),
            (None, Some(pid)) => Some((pid, By::Keys)),
            (None, None) => None,
        };
        let took_ms = ms(now, Instant::now());
        tracing::info!(
            steps,
            selected = ?chosen.map(|(pid, _)| pid),
            by = chosen.map(|(_, by)| by.name()),
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
        switcher_probe::note("selected", format!("{chosen:?} dock={} predicted={predicted:?} took_ms={took_ms}", read.name()));
        chosen
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
        let unplaced = self.unplaced.iter().any(|id| ob.window(*id).is_some_and(|w| w.pid == pid));
        let facts = AppFacts {
            hidden: apps::is_hidden(pid).unwrap_or(false),
            unplaced,
            active: apps::frontmost_pid() == Some(pid),
            visible_here,
        };
        m.dock_home(pid, facts)
    }

    /// Carries out one act of the switcher's plan.
    fn switcher_act(&mut self, act: Act, now: Instant) {
        match act {
            Act::Lend { pid, window } => {
                if let Some(prev) = self.lent.take() {
                    // A Dock press's window still home goes back first.
                    let _ = self.engine.take_back(prev.window);
                }
                if self.engine.lend_home(window) {
                    self.lent = Some(Lent { window, pid, at: now, switcher: true });
                    tracing::info!(
                        pid,
                        window = window.0,
                        since_tab_ms = self.switcher.opened.map(|t| ms(t, Instant::now())),
                        "cmd-tab: the app's windows here wait off every Desktop and macOS counts one elsewhere: its stage's window is put home while the switcher shows"
                    );
                    switcher_probe::note("lent", format!("pid={pid} window={}", window.0));
                } else {
                    tracing::info!(pid, window = window.0, "cmd-tab: the app's window here could not be put home; macOS goes where it counts one");
                }
            }
            Act::TakeBack { window, why } => {
                if self.lent.is_some_and(|l| l.window == window) {
                    self.lent = None;
                    let back = self.engine.take_back(window);
                    tracing::info!(window = window.0, back, why, "cmd-tab: the window put home for the switcher goes back");
                    switcher_probe::note("taken back", format!("window={} why={why}", window.0));
                }
            }
            Act::Release { window } => {
                if let Some(l) = self.lent.as_mut().filter(|l| l.window == window) {
                    l.at = now;
                    l.switcher = false;
                    tracing::info!(window = window.0, "cmd-tab: ⌘ let go on the app its window was put home for: the activation's switch takes it");
                    switcher_probe::note("released", format!("window={}", window.0));
                }
            }
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
        if !self.model.as_ref().is_some_and(|m| !m.macos_counts_hidden()) {
            return;
        }
        let now = Instant::now();
        switcher_probe::note("dock notice", name);
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
                self.switcher_heard(ob, Heard::Selected { steps, first: false }, now);
            }
            "AXUIElementDestroyed" if self.switcher.keys.is_open() => {
                if self.switcher.dock.is_some_and(|d| switcher_ax::read(d) != switcher_ax::Read::NoSwitcher) {
                    return;
                }
                // Escape or ⌘'s release is on its way (the list goes ~1 ms after the key, the
                // monitor's event comes later): it decides; else the fast tick after GONE_WAIT.
                self.switcher.keys.list_gone(now);
                tracing::debug!("cmd-tab: the Dock's switcher list is gone; the key on its way decides");
            }
            _ => {}
        }
    }

    /// True while the Dock's list is gone and its key not heard yet (the fast tick, at most
    /// [`switcher::GONE_WAIT`]).
    pub(super) fn switcher_wants_fast_tick(&self) -> bool {
        self.switcher.keys.waits_gone()
    }

    /// The fast tick (and the tick): the Dock's list gone [`switcher::GONE_WAIT`] ago with no
    /// key since is taken as ⌘'s release (a window put home then goes back
    /// [`super::arrival::LEND_WAIT`] later unless its app's activation came).
    pub(super) fn switcher_fast_tick(&mut self, ob: &Observer, now: Instant) {
        let heard = self.switcher.keys.gone_due(now);
        if heard != Heard::Nothing {
            tracing::info!("cmd-tab: the Dock's switcher is gone and no key came for it: taken as the release");
            self.switcher_heard(ob, heard, now);
        }
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
        // A probe's posted ⌘ may not show in the session's state: its own release is posted.
        if (!input::command_held() && !switcher_probe::posting()) || quiet {
            let steps = self.switcher.keys.steps();
            self.switcher.keys.reset();
            let heard = if quiet { Heard::Lost } else { Heard::Released { steps } };
            tracing::info!(quiet, "cmd-tab: the switcher is taken for closed (⌘ not held, or no key for a minute), its release unheard");
            self.switcher_heard(ob, heard, now);
            if quiet {
                self.switcher.selected = None;
                self.switcher.opened = None;
                self.switcher.lend_off = false;
            }
        }
    }

    /// A Space change: logged with the time since ⌘'s release when it came right after one
    /// (how long macOS took to slide away for the switcher's activation, or that it slid
    /// although a window was put home).
    pub(super) fn switcher_space_changed(&mut self, now: Instant) {
        let Some((at, lent)) = self.switcher.released else { return };
        if now.saturating_duration_since(at) > RELEASE_SEEN_FOR {
            self.switcher.released = None;
            return;
        }
        let after = ms(at, now);
        tracing::info!(after_release_ms = after, lent, "cmd-tab: the Space changed after ⌘ was let go on the switcher");
        switcher_probe::note("space changed", format!("after_release_ms={after} lent={lent}"));
    }

    /// `ctl debug-cmd-tab` (test only): a posted ⌘Tab ([`switcher_probe`]), or its report.
    /// A release is refused unless the app the keys predict for that many Tabs is a
    /// `STAGE_MANAGER_ONLY` app; the probe also cancels at the release when the Dock names
    /// another.
    pub(super) fn debug_cmd_tab(&mut self, ob: &Observer, probe: CmdTabProbe) -> Value {
        let (release, tabs, hold_ms, lend) = match probe {
            CmdTabProbe::Result => return switcher_probe::result(),
            CmdTabProbe::Start { release, tabs, hold_ms, lend } => (release, tabs, hold_ms, lend),
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
        let req = switcher_probe::Request { release, tabs, hold: Duration::from_millis(hold_ms), expected_test };
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
