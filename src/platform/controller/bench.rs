//! The bench (`cfg(test)`): the real [`Controller`], the real [`Observer`] and the real window
//! engine, driven on a fake desk ([`crate::platform::fake`]) and a frozen clock
//! (docs/DEVELOPMENT.md, "The bench").
//!
//! A scenario scripts what the person and macOS do ([`Bench::click`], [`Bench::dock_click`],
//! [`Bench::card_click`], [`Bench::go_to_desktop`], …) and moves time on
//! ([`Bench::advance`]); the bench plays the WM thread: it hands each queued notice to the
//! handler the WM thread would call (`wm_thread::WmState::handle`, `ax_callback`), stamped as
//! there, and runs the tick and the fast tick when the real timers would fire
//! ([`crate::engine::tick_pace`]). Nothing here decides anything: every decision is the
//! controller's. What a scenario reads: the stage in front ([`Bench::front`]), what is hidden
//! by the model and on the desk ([`Bench::hidden`], [`Bench::on_screen`]), the calls made
//! ([`Bench::calls`]) and the time decisions taken ([`Bench::margins`]).
//!
//! **Stamps.** A notice of the main thread's (NSWorkspace, input, a Space change, the strip)
//! is stamped when macOS sent it, however late the WM thread gets to it; an Accessibility
//! notice is stamped when its callback runs on the WM thread, as in the app. A handler that
//! waits (`clock::sleep`) or asks many apps moves the clock on, so what came due meanwhile is
//! handled late: "the WM thread was busy" needs no knob beyond [`Bench::busy`].
//!
//! **Load.** [`Bench::new`] takes the load factor: every delay of the desk and every cost of
//! a platform call is multiplied by it. The script's own times (how long a person holds a
//! click) are not.

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant};

use objc2_application_services::{AXObserver, AXUIElement};
use objc2_core_foundation::CFString;

use super::Controller;
use crate::config::ConfigStore;
use crate::engine::geometry::{Point, Rect};
use crate::engine::margin;
use crate::engine::model::{SpaceId, WindowId};
use crate::engine::recovery::EngineKind;
use crate::engine::tick_pace::{self, Pace, TickPace};
use crate::platform::apps::AppEvent;
use crate::platform::ax::{AxElement, notifications as n};
use crate::platform::clock;
use crate::platform::fake::{self, Call, Desk, Notice, Queued};
use crate::platform::input::{InputEvent, InputKind};
use crate::platform::mission_key_tap::HeardAt;
use crate::platform::observe::Observer;
use crate::platform::observe_probe::Note;
use crate::platform::{safety, windows};
use crate::ui::strip_model::StripEvent;

mod causes;
mod desktops;
mod scenarios;
mod switch;

/// The fast tick's period (`wm_thread::FAST_TICK_SECONDS`).
const FAST_TICK: Duration = Duration::from_millis(20);

/// The config a scenario runs on unless it says otherwise: the shipped defaults.
pub(super) const DEFAULTS: &str = "";

/// The AX callback the observer registers: never called, the bench delivers the notices.
unsafe extern "C-unwind" fn no_callback(_: NonNull<AXObserver>, _: NonNull<AXUIElement>, _: NonNull<CFString>, _: *mut c_void) {}

/// The real controller on a fake desk (module docs).
pub(super) struct Bench {
    /// The controller under test.
    pub(super) c: Controller,
    ob: Observer,
    pace: TickPace,
    rest: Duration,
    next_tick: Instant,
    next_fast: Option<Instant>,
    started: Instant,
    /// What was handled, in order, with the ms since the start: the scenario's own log.
    pub(super) log: Vec<String>,
    /// The app's log on stderr while the bench lives (`BENCH_LOG=debug cargo test … --
    /// --nocapture`): a scenario read beside the lines the app would have logged.
    _log: Option<tracing::subscriber::DefaultGuard>,
    // Dropped in this order: the desk before the clock, the lock last.
    _desk: fake::Installed,
    _clock: clock::test::Frozen,
    _one: MutexGuard<'static, ()>,
}

// The bench's verbs and reads are its API: a scenario need not use each one yet.
#[allow(dead_code)]
impl Bench {
    /// A controller under `engine` on the desk `setup` fills (`desktops` Desktops on one
    /// screen, the first shown), with `config` as its `config.toml`, at load factor `load`;
    /// observation started and the stages built, the desk's first notices delivered.
    pub(super) fn new(engine: EngineKind, config: &str, load: f64, desktops: usize, setup: impl FnOnce(&mut Desk)) -> Self {
        Self::with_state(engine, config, load, desktops, None, setup)
    }

    /// [`Self::new`] with `state` as the `state.json` an earlier run left (a restart: the
    /// desk's window-server session is another one).
    pub(super) fn with_state(engine: EngineKind, config: &str, load: f64, desktops: usize, state: Option<&str>, setup: impl FnOnce(&mut Desk)) -> Self {
        // One bench at a time: the safety registry and the count of window writes are the
        // process's own.
        let one = fake::exclusive();
        let log = std::env::var("BENCH_LOG").ok().map(|level| {
            let level = level.parse().unwrap_or(tracing::Level::DEBUG);
            tracing::subscriber::set_default(tracing_subscriber::fmt().with_max_level(level).with_writer(std::io::stderr).without_time().with_target(false).finish())
        });
        let started = Instant::now();
        let frozen = clock::test::freeze(started);
        margin::test::start();
        crate::engine::attribution::trail::start();
        let mut desk = Desk::new(desktops, load);
        setup(&mut desk);
        let home = desk.home.clone();
        let displays = desk.displays();
        let apps = desk.running();
        let installed = fake::install(desk);
        safety::registry_replace(Default::default());
        let path = home.join("config.toml");
        std::fs::write(&path, config).expect("the scratch config");
        if let Some(state) = state {
            std::fs::write(home.join("state.json"), state).expect("the saved stages");
        }
        let (store, read) = ConfigStore::open(path.clone());
        let error = read.err().map(|e| e.to_string());
        assert_eq!(error, None, "the scenario's config");
        let mut c = Controller::with_store(store, &path, None);
        c.engine = match engine {
            EngineKind::Corner => Arc::new(crate::platform::corner::engine::CornerEngine),
            EngineKind::Spaces => crate::platform::spaces::engine::shared(),
            EngineKind::Apple => panic!("the bench runs the app's own stages"),
        };
        let style = c.store.config().strip_style();
        fake::with(|d| d.unveil_after = Duration::from_secs_f64(crate::ui::motion_math::unveil_time(&style, 600.0)));
        let mut ob = Observer::new(1, None);
        let now = clock::now();
        let mut bench = Self {
            pace: TickPace::new(now),
            rest: tick_pace::REST,
            next_tick: now + tick_pace::BRISK,
            next_fast: None,
            started,
            log: Vec::new(),
            c,
            ob: Observer::new(1, None),
            _log: log,
            _desk: installed,
            _clock: frozen,
            _one: one,
        };
        // The launch, in the WM thread's order: the screens, the apps, then Accessibility.
        ob.set_apps(apps, no_callback);
        bench.ob = ob;
        bench.c.set_displays(&mut bench.ob, displays, now);
        bench.ob.set_accessibility(true, no_callback);
        bench.pump();
        bench.after();
        bench.advance(50);
        bench
    }

    // ----- the WM thread, played ------------------------------------------------------------

    fn ms(&self) -> u64 {
        clock::now().saturating_duration_since(self.started).as_millis() as u64
    }

    fn note(&mut self, what: String) {
        let ms = self.ms();
        self.log.push(format!("{ms:>6} {what}"));
    }

    /// `WmState::pump`.
    fn pump(&mut self) {
        loop {
            let events = self.ob.take_stamped();
            if events.is_empty() {
                break;
            }
            self.c.on_events(&mut self.ob, events);
        }
        self.ob.set_fast_retry(self.c.restore_window_open());
    }

    /// After anything handled: `arm_fast_tick` and `pace_tick` (`after_tick` after a tick).
    fn retime(&mut self, after_tick: bool) {
        let now = clock::now();
        // Sooner while the raise-order check reads (`wm_thread::arm_fast_tick`).
        let period = self.c.fast_tick_soon().map_or(FAST_TICK, |d| d.min(FAST_TICK));
        self.next_fast = match (self.c.fast_tick_wanted(), self.next_fast) {
            (false, _) => None,
            (true, Some(at)) if at <= now + period => Some(at),
            (true, _) => Some(now + period),
        };
        if after_tick {
            self.rest = self.c.tick_rest();
        }
        let waits = self.ob.tick_waits().or_else(|| self.c.tick_waits(now));
        let pace = self.pace.pace(now, waits, self.rest);
        let secs = |t: Instant| t.saturating_duration_since(self.started).as_secs_f64();
        let next = if after_tick && matches!(pace, Pace::Brisk(_)) {
            // A repeating timer: the next firing a period after this one.
            Some(secs(now) + pace.period().as_secs_f64())
        } else {
            tick_pace::retime(pace, secs(now), secs(self.next_tick), after_tick)
        };
        if let Some(at) = next {
            self.next_tick = self.started + Duration::from_secs_f64(at);
        }
    }

    fn after(&mut self) {
        self.retime(false);
    }

    /// `wm_thread::tick`.
    fn tick(&mut self) {
        {
            let _wake = windows::wake();
            let full = self.pace.full_pass(clock::now());
            self.ob.reap_gone(full);
            if full {
                self.ob.retry_pending(no_callback);
                self.ob.sweep_closed();
            }
            self.pump();
            self.c.tick(&mut self.ob);
            self.pump();
        }
        self.retime(true);
    }

    /// `wm_thread::fast_tick`.
    fn fast_tick(&mut self) {
        self.c.run_fast_tick(&mut self.ob);
        self.pump();
        self.next_fast = None;
        self.after();
    }

    /// Hands one queued notice to the handler the WM thread calls for it.
    fn deliver(&mut self, q: Queued) {
        let now = clock::now();
        // The main thread stamped it when macOS sent it; the WM thread may get to it later.
        let sent = q.due.min(now);
        let late = now.saturating_duration_since(q.due).as_millis();
        let lag = if late > 0 { format!(" (+{late} ms late)") } else { String::new() };
        match q.notice {
            Notice::Step(step) => {
                fake::with(|d| d.step(step));
                return;
            }
            Notice::Ax { pid, el, name } => {
                self.note(format!("ax {name} pid={pid} {el:?}{lag}"));
                if name != n::TITLE_CHANGED {
                    self.pace.stir(now);
                }
                // The callback runs on the WM thread: stamped when it runs.
                self.ob.receiving(Some(now));
                self.ob.on_notification(pid, AxElement::fake(el), name);
                self.ob.receiving(None);
            }
            Notice::Activated(pid) => {
                self.note(format!("app activated pid={pid}{lag}"));
                self.pace.stir(now);
                let app = fake::with(|d| d.app_info(pid)).expect("a desk");
                self.ob.receiving(Some(sent));
                self.ob.on_app_event(AppEvent::Activated(app, sent), no_callback);
                self.ob.receiving(None);
            }
            Notice::Launched(pid) => {
                self.note(format!("app launched pid={pid}{lag}"));
                self.pace.stir(now);
                let app = fake::with(|d| d.app_info(pid)).expect("a desk");
                self.ob.receiving(Some(sent));
                self.ob.on_app_event(AppEvent::Launched(app), no_callback);
                self.ob.receiving(None);
                self.c.app_launched(&self.ob, pid);
            }
            Notice::Terminated(pid) => {
                self.note(format!("app terminated pid={pid}{lag}"));
                self.pace.stir(now);
                let app = fake::with(|d| d.app_info(pid)).expect("a desk");
                self.ob.receiving(Some(sent));
                self.ob.on_app_event(AppEvent::Terminated(app), no_callback);
                self.ob.receiving(None);
            }
            Notice::SpaceChanged => {
                self.note(format!("space change notice{lag}"));
                self.pace.stir(now);
                self.c.on_space_changed(&mut self.ob, sent);
            }
            Notice::Input(event) => {
                let event = InputEvent { heard: sent, ..event };
                self.note(format!("input {:?} key={:?}{lag}", event.kind, event.key));
                let stirs = match event.kind {
                    InputKind::CommandKey => false,
                    InputKind::KeyDown => event.cmd || event.other_modifiers,
                    _ => true,
                };
                if stirs {
                    self.pace.stir(now);
                }
                self.c.input(&mut self.ob, event, None);
            }
            Notice::Strip(event) => {
                self.note(format!("strip {event:?}{lag}"));
                self.pace.stir(now);
                self.c.strip_event(&mut self.ob, event);
            }
            Notice::Displays(displays) => {
                self.note(format!("displays: {}{lag}", displays.len()));
                self.pace.stir(now);
                self.c.set_displays(&mut self.ob, displays, sent);
            }
            Notice::Switcher(name) => {
                self.note(format!("switcher notice {name}{lag}"));
                self.c.switcher_notice(&self.ob, name);
            }
            Notice::Mission(what) => {
                self.note(format!("mission tap {what:?}{lag}"));
                self.pace.stir(now);
                self.c.mission_input(&mut self.ob, HeardAt { what, at: sent });
            }
            Notice::Server(burst) => {
                self.note(format!("window-server notice {} x{} {:?}{lag}", burst.kind.code(), burst.count, burst.members));
                // Stamped at its callback, when macOS sent it.
                self.c.server_notice(&mut self.ob, crate::engine::server_notice::Burst { at: sent, ..burst });
            }
            Notice::Answers(pid) => {
                let Some((probe, retry)) = fake::with(|d| d.probes.remove(&pid)).flatten() else { return };
                self.note(format!("app answers pid={pid}{lag}"));
                self.ob.on_probe(Note::Answered { pid, probe, retry }, no_callback);
            }
        }
        self.pump();
        self.after();
    }

    /// Time moves on by `ms` (the script's own time, no load factor): every notice due is
    /// handled in the order macOS sent them, and the tick and the fast tick run when their
    /// timers fire. A handler that takes time moves the clock itself; what came due
    /// meanwhile is handled after it, late.
    pub(super) fn advance(&mut self, ms: u64) {
        let until = clock::now() + Duration::from_millis(ms);
        loop {
            let notice = fake::with(|d| d.next_due()).flatten();
            let next = [notice, Some(self.next_tick), self.next_fast].into_iter().flatten().min().expect("a tick is always due");
            if next > until {
                break;
            }
            let now = clock::now();
            if next > now {
                clock::test::advance(next - now);
            }
            let now = clock::now();
            if let Some(q) = fake::with(|d| d.take_due(now)).flatten() {
                self.deliver(q);
            } else if self.next_fast.is_some_and(|at| at <= now) {
                self.fast_tick();
            } else if self.next_tick <= now {
                self.tick();
            }
            // Else an act of macOS came due: the desk ran it as it was asked (`Desk::at`).
        }
        let now = clock::now();
        if until > now {
            clock::test::advance(until - now);
        }
    }

    /// The WM thread is busy for `ms` (the script's own time): nothing is handled meanwhile.
    pub(super) fn busy(&mut self, ms: u64) {
        clock::test::advance(Duration::from_millis(ms));
    }

    /// Runs `f` on the desk.
    pub(super) fn desk<T>(&mut self, f: impl FnOnce(&mut Desk) -> T) -> T {
        fake::with(f).expect("the bench's desk")
    }

    // ----- what the person does ---------------------------------------------------------

    fn input(&mut self, kind: InputKind, key: Option<u16>, edit: impl FnOnce(&mut InputEvent)) {
        self.desk(|d| {
            let mut event = d.input(kind, key);
            edit(&mut event);
            d.post(0.0, Notice::Input(event));
        });
    }

    /// A press of the left button at `p`, in another app's window or on the desktop: the
    /// input monitor hears it; macOS activates the app of the window under it and makes that
    /// window its key window (nothing for the wallpaper here: [`Self::wallpaper_click`]).
    pub(super) fn press(&mut self, p: Point) {
        self.desk(|d| d.press(p));
    }

    /// The left button let go where the pointer is.
    pub(super) fn release(&mut self) {
        self.desk(|d| d.release());
    }

    /// The user drags `window` by its title bar, `by` in all, in `steps` steps 16 ms apart:
    /// the press (macOS raises it, makes it key and activates its app, as at any click),
    /// 60 ms held still, then the steps; the button stays down ([`Self::release`] ends it),
    /// and a second call drags on from where the first left it.
    pub(super) fn drag_window(&mut self, window: u32, by: Point, steps: u32) {
        let from = self.desk(|d| d.windows[&window].frame);
        let grab = Point::new(from.x + 60.0, from.y + 10.0);
        if !self.desk(|d| d.left_down) {
            self.press(grab);
            self.advance(60);
        }
        let grab = self.desk(|d| d.pointer);
        for i in 1..=steps {
            let k = f64::from(i) / f64::from(steps);
            let to = Rect::new(from.x + by.x * k, from.y + by.y * k, from.w, from.h);
            self.desk(|d| {
                d.pointer = Point::new(grab.x + by.x * k, grab.y + by.y * k);
                d.user_moves(window, to);
            });
            self.advance(16);
        }
    }

    /// A click at `p`: a press, held 80 ms, and its release.
    pub(super) fn click(&mut self, p: Point) {
        self.press(p);
        self.advance(80);
        self.release();
    }

    /// A key pressed (`control`: with ⌃; the desk's ⌘ and ⇧ as held).
    pub(super) fn key(&mut self, code: u16, control: bool) {
        self.input(InputKind::KeyDown, Some(code), |e| {
            e.control = control;
            e.other_modifiers = control;
        });
    }

    /// ⌘Tab: ⌘ down, then Tab. `order`: the apps as the switcher lists them, the one in front
    /// first (it selects the second).
    pub(super) fn cmd_tab(&mut self, order: Vec<i32>) {
        self.command(true);
        self.advance(30);
        self.key(crate::engine::switcher::KEY_TAB, false);
        self.desk(|d| d.switcher_opens(order));
    }

    /// ⌘ let go with the switcher up: it closes and macOS activates the app selected.
    pub(super) fn cmd_tab_release(&mut self) {
        self.command(false);
        self.desk(|d| {
            d.tap_forwards(crate::platform::mission_key_tap::Heard::CommandUp);
            d.switcher_closes(true);
        });
    }

    /// Escape with the switcher up: it closes, nothing is activated; the input monitor does
    /// not hear the key (`engine/switcher.rs`, live 11-15), the key tap does while it is
    /// armed for the switcher and macOS lets it run (`mission_key_tap.rs`, round r2-lends).
    /// ⌘ is let go `after_ms` later.
    pub(super) fn cmd_tab_escape(&mut self, after_ms: u64) {
        self.cmd_tab_escape_key();
        self.advance(after_ms);
        self.cmd_tab_command_up();
    }

    /// [`Self::cmd_tab_escape`]'s Escape alone, ⌘ still held.
    pub(super) fn cmd_tab_escape_key(&mut self) {
        let mods = crate::engine::mission_control::Mods { control: false, option: false, command: true };
        self.desk(|d| {
            d.last_press = Some(clock::now());
            d.tap_forwards(crate::platform::mission_key_tap::Heard::Key { code: crate::engine::switcher::KEY_ESCAPE, mods });
            d.switcher_closes(false);
        });
    }

    /// ⌘ let go after the switcher closed (the input monitor and, while armed, the tap).
    pub(super) fn cmd_tab_command_up(&mut self) {
        self.command(false);
        self.desk(|d| d.tap_forwards(crate::platform::mission_key_tap::Heard::CommandUp));
    }

    /// ⌘ pressed or let go.
    pub(super) fn command(&mut self, held: bool) {
        self.desk(|d| d.cmd = held);
        self.input(InputKind::CommandKey, None, |e| e.cmd = held);
    }

    /// A click on stage `stage`'s card in our strip: its press, then its click 80 ms later
    /// (the strip's own events; the input monitor hears nothing of our own windows, while
    /// the session's "since the last press", `input::since_last_press`, counts every click).
    pub(super) fn card_click(&mut self, stage: u64) {
        self.desk(|d| {
            d.last_press = Some(clock::now());
            d.last_click = d.last_press;
            d.post(0.0, Notice::Strip(StripEvent::CardPressed { stage }));
        });
        self.advance(80);
        self.desk(|d| d.post(0.0, Notice::Strip(StripEvent::CardClicked { stage, shift: false })));
    }

    /// The left button pressed on app `pid`'s Dock item.
    pub(super) fn dock_press(&mut self, pid: i32) {
        let p = self.desk(|d| d.dock_point(pid));
        self.desk(|d| d.pointer = p);
        self.input(InputKind::MouseDown, None, |_| {});
    }

    /// The button let go on the Dock item pressed: the click; macOS activates the app, every
    /// window of it forward, and slides to its Desktop when its windows are all on another.
    pub(super) fn dock_release(&mut self, pid: i32) {
        self.input(InputKind::MouseUp, None, |_| {});
        self.desk(|d| d.activate(pid, true));
    }

    /// A Dock click on app `pid`: the press, held `hold_ms`, the release.
    pub(super) fn dock_click(&mut self, pid: i32, hold_ms: u64) {
        self.dock_press(pid);
        self.advance(hold_ms);
        self.dock_release(pid);
    }

    /// The stages saved now, as `state.json` holds them.
    pub(super) fn saved(&mut self) -> String {
        self.c.save_now();
        let home = self.desk(|d| d.home.clone());
        std::fs::read_to_string(home.join("state.json")).expect("a saved state")
    }

    /// One of Mission Control's keys, as its key tap hears it (⌃↑ by default); the input
    /// monitor never hears them (`controller/mission_control.rs`).
    pub(super) fn mission_key(&mut self) {
        let mods = crate::engine::mission_control::Mods { control: true, option: false, command: false };
        self.desk(|d| {
            d.last_press = Some(clock::now());
            if !d.tap_hears {
                return;
            }
            d.post(0.0, Notice::Mission(crate::platform::mission_key_tap::Heard::Key { code: crate::platform::input::KEY_UP, mods }));
        });
    }

    /// Mission Control opened by its key.
    pub(super) fn mission_open(&mut self) {
        self.mission_key();
        self.desk(|d| d.mission_opens());
    }

    /// Mission Control closed by its key with the pointer over a window of app `over`:
    /// macOS activates that app as it closes (`controller/mission_control.rs`, round mc-key).
    pub(super) fn mission_close_by_key(&mut self, over: Option<i32>) {
        self.mission_key();
        self.desk(|d| {
            d.mission_closes();
            if let Some(pid) = over {
                d.post(30.0, Notice::Step(fake::Step::Activation { pid, raise_all: false }));
            }
        });
    }

    /// A click on `window` in Mission Control: the tap hears the press; macOS activates the
    /// window's app about 150 ms later, with the release that starts the close
    /// (`engine/mission_control.rs`, round mc-pick).
    pub(super) fn mission_pick(&mut self, window: u32) {
        self.desk(|d| {
            d.last_press = Some(clock::now());
            d.last_click = d.last_press;
            if d.tap_hears {
                d.post(0.0, Notice::Mission(crate::platform::mission_key_tap::Heard::Click));
            }
        });
        self.advance(150);
        self.desk(|d| {
            let pid = d.windows[&window].pid;
            d.make_key(window, fake::delays::FOCUS);
            d.post(0.0, Notice::Step(fake::Step::Activation { pid, raise_all: false }));
            d.mission_closes();
        });
    }

    /// Every stage's windows on the Desktop shown, each sorted, the stages sorted.
    pub(super) fn stages(&self) -> Vec<Vec<u32>> {
        let Some(m) = self.c.model.as_ref() else { return Vec::new() };
        let here = m.current_space(m.main_display().id);
        let mut out: Vec<Vec<u32>> = m
            .stages_in(here)
            .map(|s| {
                let mut ids: Vec<u32> = s.windows.iter().map(|w| w.0).collect();
                ids.sort_unstable();
                ids
            })
            .filter(|s| !s.is_empty())
            .collect();
        out.sort();
        out
    }

    /// App `pid` opens window `id` at `frame` on the Desktop shown.
    pub(super) fn open_window(&mut self, id: u32, pid: i32, frame: Rect) {
        self.desk(|d| d.open_window(id, pid, frame));
    }

    /// App `pid` quits.
    pub(super) fn quit(&mut self, pid: i32) {
        self.desk(|d| d.quit(pid));
    }

    /// A second screen comes.
    pub(super) fn screen_comes(&mut self) {
        self.desk(|d| d.screen_comes());
    }

    /// The second screen goes.
    pub(super) fn screen_goes(&mut self) {
        self.desk(|d| d.screen_goes());
    }

    /// macOS activates app `pid` (an `open`, an app's own doing).
    pub(super) fn activate(&mut self, pid: i32) {
        self.desk(|d| d.activate(pid, false));
    }

    /// App `pid` makes `window` its key window by itself.
    pub(super) fn app_focuses(&mut self, window: u32) {
        self.desk(|d| d.make_key(window, 0.0));
    }

    /// An app brings `window` forward by itself (a link opened in it): its key window, in
    /// front; when the app is frontmost and the window on a Desktop not shown, macOS slides
    /// there (`engine/focus_elsewhere.rs`, 11-19 13:13).
    pub(super) fn app_brings_forward(&mut self, window: u32) {
        self.desk(|d| d.app_brings_forward(window));
    }

    /// Time moves on, 10 ms at a time, until a person sees `window` (5 s at most).
    pub(super) fn until_shown(&mut self, window: u32) {
        for _ in 0..500 {
            if self.on_screen().contains(&window) {
                return;
            }
            self.advance(10);
        }
        panic!("window {window} never showed:\n{}", self.story());
    }

    /// Window `id` goes into full screen (`on`) or leaves it: its green button.
    pub(super) fn fullscreen(&mut self, id: u32, on: bool) {
        self.desk(|d| if on { d.enters_fullscreen(id) } else { d.leaves_fullscreen(id) });
    }

    /// The person goes to Desktop `to` (⌃→ or a swipe; `heard`: the input monitor hears the
    /// key, which macOS may keep to itself).
    pub(super) fn go_to_desktop(&mut self, to: u64, heard: bool) {
        if heard {
            self.key(crate::platform::input::KEY_LEFT + 1, true);
        }
        self.desk(|d| d.go_to_desktop(to));
    }

    // ----- what a scenario reads --------------------------------------------------------

    /// The windows of the stage in front on the Desktop shown, by id (`None`: no stage is).
    pub(super) fn front(&self) -> Option<Vec<u32>> {
        let m = self.c.model.as_ref()?;
        let stage = m.stage(m.main_active_stage()?)?;
        let mut ids: Vec<u32> = stage.windows.iter().map(|w| w.0).collect();
        ids.sort_unstable();
        Some(ids)
    }

    /// The windows of the stage in front on the model's Space `space` (`None`: none is).
    pub(super) fn front_in(&self, space: SpaceId) -> Option<Vec<u32>> {
        let m = self.c.model.as_ref()?;
        let stage = m.stage(m.active_stage_in(space)?)?;
        let mut ids: Vec<u32> = stage.windows.iter().map(|w| w.0).collect();
        ids.sort_unstable();
        Some(ids)
    }

    /// The model's Space of window `id`.
    pub(super) fn space_of(&self, id: u32) -> Option<SpaceId> {
        self.c.model.as_ref()?.space_of(WindowId(id))
    }

    /// The model's Space shown on the main display.
    pub(super) fn space_shown(&self) -> Option<SpaceId> {
        let m = self.c.model.as_ref()?;
        Some(m.current_space(m.main_display().id))
    }

    /// Every stage's windows on the model's Space of window `of`, each sorted, the stages
    /// sorted (empty: the model does not know the window).
    pub(super) fn stages_with(&self, of: u32) -> Vec<Vec<u32>> {
        let Some(m) = self.c.model.as_ref() else { return Vec::new() };
        let Some(space) = m.space_of(WindowId(of)) else { return Vec::new() };
        let mut out: Vec<Vec<u32>> = m
            .stages_in(space)
            .map(|s| {
                let mut ids: Vec<u32> = s.windows.iter().map(|w| w.0).collect();
                ids.sort_unstable();
                ids
            })
            .filter(|s| !s.is_empty())
            .collect();
        out.sort();
        out
    }

    /// True when the model counts window `id` on every Desktop.
    pub(super) fn sticky(&self, id: u32) -> bool {
        self.c.model.as_ref().is_some_and(|m| m.is_sticky(WindowId(id)))
    }

    /// True while the controller counts a full-screen app's Space shown on the main screen.
    pub(super) fn in_fullscreen(&self) -> bool {
        self.c.phase.fullscreen()
    }

    /// True while a Space change waits to be recognized.
    pub(super) fn changing(&self) -> bool {
        self.c.phase.changing()
    }

    /// The shadow's counts since the launch (`ctl state`'s `desks.disagree`): the
    /// disagreements begun on the Desktop shown and on a hidden window's Desktop, and those
    /// on the Desktop shown still open.
    pub(super) fn disagreed(&self) -> (u64, u64, u64) {
        let g = &self.c.desks_json()["disagree"];
        (g["shown_begun"].as_u64().unwrap_or(0), g["homes_begun"].as_u64().unwrap_or(0), g["shown"].as_u64().unwrap_or(0))
    }

    /// What the listing decided since the launch (`ctl state`'s `desks.keyed`): the notices
    /// that waited for the flip, the changes taken at a look before their notice, and the
    /// notices that came after one.
    pub(super) fn keyed(&self) -> (u64, u64, u64) {
        let k = &self.c.desks_json()["keyed"];
        (k["flips_waited"].as_u64().unwrap_or(0), k["unannounced"].as_u64().unwrap_or(0), k["announced_late"].as_u64().unwrap_or(0))
    }

    /// The windows the model counts hidden.
    pub(super) fn hidden(&self) -> BTreeSet<u32> {
        self.c.model.as_ref().map(|m| m.windows().filter(|w| w.parked).map(|w| w.meta.id.0).collect()).unwrap_or_default()
    }

    /// The windows macOS counts on a Desktop now (shown or not): not those in a stage Space.
    pub(super) fn on_a_desktop(&mut self) -> Vec<u32> {
        self.desk(|d| d.windows.keys().copied().filter(|id| d.desktop_of(*id).is_some()).collect())
    }

    /// The Desktop the window server lists window `id` on (`None`: on none, hidden).
    pub(super) fn desktop_of(&mut self, id: u32) -> Option<u64> {
        self.desk(|d| d.desktop_of(id))
    }

    /// The windows a person sees on the desk now: listed on screen and not in the park zone.
    pub(super) fn on_screen(&mut self) -> BTreeSet<u32> {
        self.desk(|d| {
            let display = d.screens[0].display;
            d.windows.values().filter(|w| w.layer == 0 && d.on_screen(w) && !display.parks(w.frame)).map(|w| w.id).collect()
        })
    }

    /// The desk's frontmost app.
    pub(super) fn front_app(&mut self) -> Option<i32> {
        self.desk(|d| d.front)
    }

    /// The Desktop the desk shows.
    pub(super) fn desktop_shown(&mut self) -> u64 {
        self.desk(|d| d.screens[0].shown)
    }

    /// The calls the app made of the platform so far, in order.
    pub(super) fn calls(&mut self) -> Vec<Call> {
        self.desk(|d| d.calls.clone()).into_iter().map(|(_, c)| c).collect()
    }

    /// How many transitions the strip was sent so far (animated switches).
    pub(super) fn transitions(&mut self) -> usize {
        self.desk(|d| d.strip.transitions)
    }

    /// What each focus and activation notice handled so far came to, in order
    /// ([`crate::engine::attribution::trail`]).
    pub(super) fn trail(&self) -> Vec<String> {
        crate::engine::attribution::trail::so_far()
    }

    /// True while the app counts a drag of a window by the user under way.
    pub(super) fn dragging(&self) -> bool {
        self.c.drag.is_some()
    }

    /// The frame the model keeps for window `id` (where it shows when its stage is in front).
    pub(super) fn model_frame(&self, id: u32) -> Option<Rect> {
        self.c.model.as_ref()?.window(WindowId(id)).map(|w| w.restore)
    }

    /// The time decisions and waits' ends noted so far, in order ([`margin::test`]).
    pub(super) fn margins(&self) -> Vec<margin::test::Seen> {
        margin::test::so_far()
    }

    /// The stage ids by the windows they hold, for a card click.
    pub(super) fn stage_of(&self, window: u32) -> u64 {
        self.c.model.as_ref().and_then(|m| m.stage_of(WindowId(window))).expect("the window's stage").0
    }

    /// The scenario's log so far: one line per notice handled and, indented, per call the
    /// app made of the platform, in order.
    pub(super) fn story(&self) -> String {
        let mut lines: Vec<(u64, usize, String)> =
            self.log.iter().enumerate().map(|(i, l)| (l.trim_start().split(' ').next().and_then(|ms| ms.parse().ok()).unwrap_or(0), i, l.clone())).collect();
        let calls = fake::with(|d| d.calls.clone()).unwrap_or_default();
        lines.extend(calls.into_iter().enumerate().map(|(i, (ms, call))| (ms, usize::MAX / 2 + i, format!("{ms:>6}     -> {call:?}"))));
        lines.sort_by_key(|(ms, i, _)| (*ms, *i));
        lines.into_iter().map(|(_, _, l)| l).collect::<Vec<_>>().join("\n")
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        // Nothing of this run stays in the process's registry for the next bench.
        safety::registry_replace(Default::default());
        let home = fake::with(|d| d.home.clone());
        if let Some(home) = home {
            let _ = std::fs::remove_dir_all(home);
        }
    }
}

/// A window's frame on the test screen, by a small number.
pub(super) fn frame(n: u32) -> Rect {
    let k = f64::from(n);
    Rect::new(200.0 + 40.0 * k, 100.0 + 30.0 * k, 700.0, 450.0)
}
