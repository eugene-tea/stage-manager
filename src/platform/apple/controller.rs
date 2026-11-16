//! The Apple mode's conductor ([`AppleController`]): what the WM thread drives instead of the
//! app's own [`crate::platform::controller::Controller`] while Apple's Stage Manager is on
//! and `[windows] with_apple = "augment"` (`platform::conductor`).
//!
//! It arranges nothing itself. It reads Apple's arrangement (the window list at every read,
//! WindowManager's Accessibility as the slow check: [`crate::engine::apple::mirror`]), keeps
//! the app's model in step with it once it is settled ([`crate::engine::apple::follow`]),
//! saves that model to `state-apple.json`, and carries out its own steps as jobs
//! ([`crate::engine::apple::job`], planned by [`crate::engine::apple::plan`]), one at a time,
//! each step through its route: `ctl switch N` and the switch hotkeys by the focus route,
//! `ctl merge N` by a Shift-click per window, `ctl send N` and the send hotkeys by a switch,
//! a Shift-click and a switch back, `ctl new` and the new-stage hotkey by the drag; and "open
//! here" for a newly opened app (a rule's `current`, `[stages] new_apps_join_current`, Shift
//! held at the launch, `[sugar] shift_brings_here`): the app hidden at its launch
//! ([`super::launch_hide`]) so Apple's keeps the stage in front, its window Shift-clicked in
//! from the strip, then focused; the round trip when the hide came too late.
//!
//! Apple's posts no Accessibility notices, so it polls: a read at every activation, window
//! found or gone, focus, mouse-up near the strip and Desktop change; then every 50 ms while
//! anything moves or a job runs (the list only, on the fast tick); every 2 s at rest,
//! Accessibility with it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use super::desk::Desk;
use super::launch_hide::{self, HideOutcome, Hiding};
use super::routes::drag::Drag;
use super::routes::focus::Focus;
use super::routes::rejected;
use super::routes::shift_click::ShiftClick;
use super::routes::{Route, RouteStats};
use super::post;
use crate::config::{ConfigStore, HotkeyCommand};
use crate::engine::apple::desk::{AppleDesk, Landed, Posted};
use crate::engine::apple::follow;
use crate::engine::apple::job::{self, Ctx, Job, Next, Report};
use crate::engine::apple::mirror::{self, Arrangement};
use crate::engine::apple::plan::{self, Intent, Refusal, Step};
use crate::engine::apple::points;
use crate::engine::model::{Display, Model, WindowId, WindowMeta, WindowRole};
use crate::engine::recovery::EngineKind;
use crate::engine::rules::RuleAction;
use crate::engine::save_clock::SaveClock;
use crate::engine::settings::NativeSettings;
use crate::engine::stand_down::WithApple;
use crate::platform::apps::AppInfo;
use crate::platform::ctl::{AppleTry, Command};
use crate::platform::input::{self, InputEvent, InputKind, Modifiers, Triggers};
use crate::platform::native_settings::RawSettings;
use crate::platform::observe::{Observer, WinEvent, WindowKind};
use crate::platform::{logging, paths, session};

/// How often config.toml's modification time is checked (as the controller does).
const CONFIG_POLL: Duration = Duration::from_secs(1);

/// How often WindowManager's pid is looked up again (it restarts with the Dock's settings).
const MANAGER_CHECK: Duration = Duration::from_secs(10);

/// A mouse-up this close to the screen's left edge may have changed Apple's strip.
const NEAR_STRIP: f64 = 300.0;

/// Jobs waiting behind the one under way, at most.
const QUEUE: usize = 4;

/// How long reads go on at the motion's pace after an event that may change Apple's
/// arrangement: its motion starts about 100 ms after the cause and runs about 600 ms.
const WATCH: Duration = Duration::from_millis(1200);

/// The Apple mode's conductor. WM thread only.
pub struct AppleController {
    store: ConfigStore,
    config_error: Option<String>,
    config_mtime: Option<SystemTime>,
    config_checked: Instant,
    displays: Vec<Display>,
    settings: NativeSettings,
    /// The app's model, following Apple's (built at the first read with observation on).
    model: Option<Model>,
    mirror: Arrangement,
    /// The grouping last followed: a read that shows the same is not followed again.
    followed: Option<(Vec<u32>, Vec<Vec<u32>>)>,
    /// The windows last followed.
    followed_ids: BTreeSet<u32>,
    /// When the next read is due (`None`: at the tick's pace).
    read_due: Option<Instant>,
    /// Reads go on at the motion's pace until then ([`WATCH`] after an event).
    watch_until: Option<Instant>,
    /// The last read with Accessibility.
    ax_read: Option<Instant>,
    /// What the last read took, in ms.
    read_ms: f64,
    manager: Option<i32>,
    manager_checked: Option<Instant>,
    /// The steps of ours under way, the first one running.
    jobs: VecDeque<Job>,
    /// The gesture posted and not landed yet.
    posting: Option<Posted>,
    /// Newly opened apps being hidden at their launch, by pid.
    hidings: BTreeMap<i32, Hiding>,
    /// Each managed window's app, at the last read.
    pids: BTreeMap<u32, i32>,
    /// Each route's counts.
    stats: BTreeMap<&'static str, RouteStats>,
    last_refusal: Option<String>,
    /// The last job's account.
    last_report: Option<Report>,
    /// The person's latest action, for Shift held at a launch (`[sugar] shift_brings_here`).
    triggers: Triggers,
    save: SaveClock,
    seen_times: std::collections::HashMap<u64, u64>,
}

impl AppleController {
    /// Loads config (as the controller does) and tells the main thread (the hotkeys stay
    /// registered for this mode's switch).
    pub fn new() -> Self {
        let path = paths::config_file();
        let (store, result) = ConfigStore::open(path.clone());
        let config_error = match result {
            Ok((warnings, _)) => {
                for w in warnings {
                    tracing::warn!("config: {w}");
                }
                None
            }
            Err(e) => {
                tracing::error!("config: {e}; using the last good config");
                Some(e.to_string())
            }
        };
        let this = Self::with_store(store, &path, config_error);
        crate::platform::controller::notify_config(this.store.config(), this.config_error.clone());
        tracing::info!("apple mode: following Apple's Stage Manager (its stages read into the model; its own steps through the focus, Shift-click and drag routes)");
        this
    }

    fn with_store(store: ConfigStore, path: &Path, config_error: Option<String>) -> Self {
        Self {
            store,
            config_error,
            config_mtime: std::fs::metadata(path).and_then(|m| m.modified()).ok(),
            config_checked: Instant::now(),
            displays: Vec::new(),
            settings: NativeSettings::default(),
            model: None,
            mirror: Arrangement::default(),
            followed: None,
            followed_ids: BTreeSet::new(),
            read_due: Some(Instant::now()),
            watch_until: None,
            ax_read: None,
            read_ms: 0.0,
            manager: None,
            manager_checked: None,
            jobs: VecDeque::new(),
            posting: None,
            hidings: BTreeMap::new(),
            pids: BTreeMap::new(),
            stats: BTreeMap::new(),
            last_refusal: None,
            last_report: None,
            triggers: Triggers::default(),
            save: SaveClock::default(),
            seen_times: Default::default(),
        }
    }

    /// `[windows] with_apple` as the config in force has it.
    pub fn with_apple(&self) -> WithApple {
        self.store.config().windows.with_apple
    }

    /// True once the model follows Apple's (observation on, a first read made).
    pub fn managing(&self) -> bool {
        self.model.is_some()
    }

    /// Every window the model follows.
    pub fn model_window_ids(&self) -> BTreeSet<u32> {
        self.model.as_ref().map(|m| m.windows().map(|w| w.meta.id.0).collect()).unwrap_or_default()
    }

    /// The main screen, as last reported.
    fn screen(&self) -> Option<Display> {
        self.displays.first().copied()
    }

    // ── What the WM thread tells it ─────────────────────────────────────────────────────

    /// The observer's events: each one that may change Apple's arrangement asks a read now.
    pub fn on_events(&mut self, ob: &mut Observer, events: Vec<WinEvent>) {
        let stirs = events.iter().any(|e| !matches!(e, WinEvent::Retitled(_) | WinEvent::Moved(_)));
        let moved = events.iter().any(|e| matches!(e, WinEvent::Moved(_)));
        if stirs {
            self.watch();
            self.read(ob, false);
        } else if moved {
            // Apple's motion moves the windows: the fast tick reads on.
            self.read_due.get_or_insert(Instant::now());
        }
    }

    /// Input in another app: the person's latest action is kept for Shift at a launch (a
    /// gesture of ours and its echoes never count), and a mouse-up near the strip may have
    /// changed it.
    pub fn input(&mut self, ob: &mut Observer, event: InputEvent) {
        let now = Instant::now();
        if event.kind != InputKind::CommandKey && !post::echo(now) {
            let mods = Modifiers { shift: event.shift, cmd: event.cmd };
            if event.kind == InputKind::MouseUp {
                self.triggers.record_release(now, mods);
            } else {
                self.triggers.record(now, mods);
            }
        }
        let near = self.screen().is_some_and(|d| event.at.x < d.frame.x + NEAR_STRIP);
        if event.kind == InputKind::MouseUp && near {
            self.watch();
            self.read(ob, false);
        }
    }

    /// A hotkey: switch to stage N (the focus route), send the focused window to stage N or
    /// to a stage of its own (the posted routes); the pin and the strip are not in this mode
    /// (said in the log).
    pub fn hotkey(&mut self, ob: &mut Observer, command: HotkeyCommand) {
        let asked = match command {
            HotkeyCommand::SwitchTo(n) => self.ask(ob, Ask::Switch(n)),
            HotkeyCommand::SendTo(n) => self.ask(ob, Ask::Send(n, None)),
            HotkeyCommand::NewStage => self.ask(ob, Ask::New(None)),
            other => {
                tracing::info!(?other, "apple mode: this hotkey does nothing while Apple's Stage Manager arranges the windows");
                return;
            }
        };
        if let Err(Refusal(why)) = asked {
            tracing::info!(?command, why, "apple mode: the hotkey did nothing");
        }
    }

    /// A Desktop change: the arrangement is read afresh (the stages of the Desktop left are
    /// remembered).
    pub fn on_space_changed(&mut self, ob: &mut Observer) {
        self.watch();
        self.read(ob, true);
    }

    /// An app just opened: its stage shows soon; a newly opened app the rules want here is
    /// hidden now if the will-launch notice did not start that already.
    pub fn app_launched(&mut self, ob: &Observer, pid: i32) {
        if ob.admits_pid(pid) {
            let bundle = ob.bundle_id(pid).unwrap_or_default().to_string();
            self.arrival(pid, &bundle, Instant::now());
        }
        self.watch();
        self.read_due = Some(Instant::now());
    }

    /// An app about to launch: a newly opened app the rules want here is hidden at once.
    pub fn app_will_launch(&mut self, _ob: &Observer, app: &AppInfo) {
        self.arrival(app.pid, app.bundle_or_unknown(), Instant::now());
        self.read_due = Some(Instant::now());
    }

    /// Newly opened app `pid` (`bundle`): "open here" when a rule's `current`,
    /// `[stages] new_apps_join_current` or Shift held (`[sugar] shift_brings_here`) asks for
    /// the stage in front, as the app's own mode decides for a newly opened app's first window
    /// (`Decision::for_new_window`; a rule's title pattern is matched against no title, the
    /// app having no window yet). With no stage in front, or Apple's own rule asked, nothing.
    fn arrival(&mut self, pid: i32, bundle: &str, now: Instant) {
        if self.hidings.contains_key(&pid) || self.jobs.iter().any(|j| j.intent == Intent::OpenHere(pid)) {
            return;
        }
        if self.model.as_ref().is_some_and(|m| !m.app_is_new(pid)) {
            return;
        }
        let config = self.store.config();
        let decision = config.rules.decide(bundle, "");
        let sugar = &config.sugar;
        let shift = sugar.shift_brings_here
            && decision.action.shift_counts()
            && self.triggers.shift_here(now, Duration::from_millis(sugar.launch_window_ms), input::live_modifiers());
        let (decision, effect) = decision.for_new_window(true, config.stages.new_apps_join_current, shift);
        let here = decision.action == RuleAction::Current || decision.new_app_joins_current;
        if !here {
            if shift {
                tracing::info!(pid, bundle, ?effect, "apple mode: Shift held at the launch: the newly opened app gets a stage of its own (Apple's)");
                self.triggers.consume();
            }
            return;
        }
        if shift {
            self.triggers.consume();
        }
        let Some(job) = plan::open_here(&self.mirror, pid, now) else {
            tracing::debug!(pid, bundle, "apple mode: a newly opened app to open here, no stage in front: Apple's puts it in front");
            return;
        };
        if self.jobs.len() > QUEUE {
            tracing::warn!(pid, bundle, "apple mode: a newly opened app to open here, too many steps waiting: Apple's own");
            return;
        }
        match launch_hide::start(pid) {
            Ok(h) => {
                self.hidings.insert(pid, h);
            }
            Err(e) => tracing::warn!(pid, "apple mode: {e}; the round trip instead"),
        }
        let why = if shift { "Shift held" } else if decision.action == RuleAction::Current { "a rule's current" } else { "[stages] new_apps_join_current" };
        tracing::info!(pid, bundle, why, "apple mode: a newly opened app opens here: hidden at its launch, then brought into the stage in front");
        self.jobs.push_back(job);
        self.watch();
    }

    /// Reads at the motion's pace for [`WATCH`] from now: Apple's motion follows its cause.
    /// The arrangement counts in motion from now ([`Arrangement::step_began`]): a read in
    /// the moment before Apple's motion starts shows the stage coming in beside the one
    /// leaving (live 11-16: an activation followed that way put both in one stage).
    fn watch(&mut self) {
        let now = Instant::now();
        self.watch_until = Some(now + WATCH);
        self.mirror.step_began(now);
    }

    /// The displays (the main one first).
    pub fn set_displays(&mut self, displays: Vec<Display>) {
        if displays.is_empty() {
            return;
        }
        if let Some(m) = self.model.as_mut() {
            let _ = m.set_displays(displays.iter().take(1).copied().collect());
        }
        self.displays = displays;
        self.read_due = Some(Instant::now());
    }

    /// Fresh `com.apple.WindowManager` values.
    pub fn set_settings(&mut self, raw: &RawSettings) {
        let s = crate::platform::controller::native_settings(raw);
        if s == self.settings {
            return;
        }
        self.settings = s;
        if let Some(m) = self.model.as_mut() {
            m.set_settings(s);
        }
    }

    /// Re-reads config.toml; a bad file keeps the last good config.
    pub fn reload_config(&mut self) {
        self.config_mtime = std::fs::metadata(self.store.path()).and_then(|m| m.modified()).ok();
        match self.store.reload() {
            Ok(warnings) => {
                for w in warnings {
                    tracing::warn!("config: {w}");
                }
                tracing::info!("config reloaded");
                self.config_error = None;
                logging::apply_config(&self.store.config().log);
            }
            Err(e) => {
                tracing::error!("config: {e}; keeping the last good config");
                self.config_error = Some(e.to_string());
            }
        }
        crate::platform::controller::notify_config(self.store.config(), self.config_error.clone());
    }

    // ── Ticks ───────────────────────────────────────────────────────────────────────────

    /// Housekeeping: config polling, the read at rest (Accessibility with it), the save.
    pub fn tick(&mut self, ob: &mut Observer) {
        let now = Instant::now();
        if now.duration_since(self.config_checked) >= CONFIG_POLL {
            self.config_checked = now;
            let current = std::fs::metadata(self.store.path()).and_then(|m| m.modified()).ok();
            if current.is_some() && current != self.config_mtime {
                tracing::info!("config.toml changed; reloading");
                self.reload_config();
            }
        }
        let ax_due = self.ax_read.is_none_or(|t| now.duration_since(t) >= mirror::REST_READ);
        if ax_due || self.read_due.is_some_and(|t| now >= t) {
            self.read(ob, ax_due);
        }
        if self.save.due(now) {
            self.save_now();
        }
    }

    /// The fast tick: the list read every [`mirror::MOTION_READ`] while anything moves or a
    /// job runs.
    pub fn fast_tick(&mut self, ob: &mut Observer) {
        let now = Instant::now();
        if self.read_due.is_some_and(|t| now >= t) || (self.read_due.is_none() && !self.jobs.is_empty()) {
            self.read(ob, false);
        }
    }

    /// True while a read is due soon (in motion, not yet settled, a job under way).
    pub fn wants_fast_tick(&self) -> bool {
        self.read_due.is_some() || !self.jobs.is_empty() || self.posting.is_some()
    }

    /// Why the tick stays brisk for this mode.
    pub fn tick_waits(&self) -> Option<&'static str> {
        if !self.jobs.is_empty() || self.posting.is_some() {
            Some("a step of ours in Apple's Stage Manager")
        } else if !self.mirror.settled && self.model.is_some() {
            Some("Apple's Stage Manager settles")
        } else if self.save.is_dirty() {
            Some("a save is due")
        } else {
            None
        }
    }

    /// The longest the tick may rest: the read at rest.
    pub fn tick_rest(&self) -> Duration {
        mirror::REST_READ.min(crate::engine::tick_pace::REST)
    }

    // ── The read ────────────────────────────────────────────────────────────────────────

    /// One read of Apple's arrangement (Accessibility too with `with_ax`), folded into the
    /// mirror; settled, the model follows it and a waiting switch is looked at.
    fn read(&mut self, ob: &mut Observer, with_ax: bool) {
        let now = Instant::now();
        let Some(screen) = self.screen() else { return };
        if !ob.observing() {
            return;
        }
        let t0 = Instant::now();
        if self.manager_checked.is_none_or(|t| now.duration_since(t) >= MANAGER_CHECK) || self.manager.is_none() {
            self.manager = super::strip_ax::manager_pid();
            self.manager_checked = Some(now);
        }
        let mut desk = Desk { ob, manager: self.manager, screen: Some(screen.frame) };
        let listed = desk.listed();
        let reading = if with_ax { desk.strip(&screen) } else { None };
        let managed: BTreeSet<u32> = managed(ob, &listed);
        self.pids = managed.iter().filter_map(|id| ob.window(*id).map(|w| (*id, w.pid))).collect();
        let misses = self.mirror.misses;
        let before = self.mirror.grouping();
        self.mirror = mirror::fold(&self.mirror, reading.as_ref(), &listed, &managed, &screen, now);
        if self.mirror.grouping() != before {
            // What the read saw of each managed window (ids and bounds only, never a title):
            // what a live run's log needs to tell a misread from Apple's own.
            let seen: Vec<String> = listed
                .windows
                .iter()
                .enumerate()
                .filter(|(_, w)| managed.contains(&w.id))
                .map(|(i, w)| {
                    let b = w.bounds;
                    format!("{}@{},{} {}x{}{}{}", w.id, b.x, b.y, b.w, b.h, if mirror::has_twin(&listed, i) { " twin" } else { "" }, if w.onscreen { "" } else { " off" })
                })
                .collect();
            tracing::debug!(
                current = ?self.mirror.current,
                strip = ?self.mirror.strip.iter().map(|g| &g.windows).collect::<Vec<_>>(),
                remembered = ?self.mirror.remembered.iter().map(|g| &g.windows).collect::<Vec<_>>(),
                in_motion = self.mirror.in_motion,
                settled = self.mirror.settled,
                screen = ?(screen.frame.x, screen.frame.y, screen.frame.w, screen.frame.h),
                seen = ?seen,
                "apple mode: the mirror's groups changed"
            );
        }
        if with_ax {
            self.ax_read = Some(now);
        }
        self.read_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if self.mirror.misses > misses {
            tracing::info!(misses = self.mirror.misses, "apple mode: the window list and WindowManager's Accessibility disagree on the strip's groups; the list wins");
        }
        let watching = self.watch_until.is_some_and(|t| now < t);
        if !watching {
            self.watch_until = None;
        }
        let busy = !self.jobs.is_empty() || self.posting.is_some();
        self.read_due = (!self.mirror.settled || busy || watching).then(|| now + mirror::MOTION_READ);
        if self.mirror.settled {
            self.follow(ob, &screen, &managed);
        }
        if busy {
            self.pump(ob, &screen, now);
        }
    }

    /// The model brought in line with the mirror when what Apple's shows changed.
    fn follow(&mut self, ob: &Observer, screen: &Display, managed: &BTreeSet<u32>) {
        let grouping = self.mirror.grouping();
        if self.model.is_some() && self.followed.as_ref() == Some(&grouping) && self.followed_ids == *managed {
            return;
        }
        let config = self.store.config().clone();
        let model = self.model.get_or_insert_with(|| {
            let capacity = crate::platform::controller::strip_capacity(screen.visible, &config);
            let mut options = config.model_options(self.settings, capacity);
            options.engine = EngineKind::Apple;
            options.named_stages = Vec::new();
            options.main_only = true;
            Model::new(vec![*screen], options)
        });
        let metas: BTreeMap<u32, WindowMeta> = managed
            .iter()
            .filter_map(|id| ob.window(*id))
            .map(|w| {
                let meta = WindowMeta {
                    id: WindowId(w.id),
                    pid: w.pid,
                    bundle_id: ob.bundle_id(w.pid).unwrap_or_default().to_string(),
                    title: w.title.clone(),
                    role: WindowRole::Standard,
                    frame: w.frame,
                    display: screen.id,
                };
                (w.id, meta)
            })
            .collect();
        let focused = crate::platform::controller::focused_window();
        let done = follow::follow(model, &self.mirror, &metas, focused);
        if !done.is_empty() {
            tracing::debug!(added = done.added, removed = done.removed, moved = done.moved, switched = done.switched, "apple mode: the model follows Apple's stages");
            self.save.mark(Instant::now());
        }
        self.followed = Some(grouping);
        self.followed_ids = managed.clone();
    }

    /// The jobs moved on at a read: a gesture that landed taken in, a hide's outcome noted,
    /// then the first job advanced against the mirror and its step run, or its end logged.
    fn pump(&mut self, ob: &Observer, screen: &Display, now: Instant) {
        if let Some(landed) = post::take_landed() {
            self.gesture_landed(ob, &landed, now);
        }
        for j in self.jobs.iter_mut() {
            if let Intent::OpenHere(pid) = j.intent
                && let Some(HideOutcome::Hidden { .. }) = self.hidings.get(&pid).and_then(Hiding::outcome)
            {
                j.hidden = true;
            }
        }
        // A step at a time: each advance runs at most one.
        let Some(job) = self.jobs.front_mut() else { return };
        let mut desk = Desk { ob, manager: self.manager, screen: Some(screen.frame) };
        let cx = Ctx { mirror: &self.mirror, input: desk.input(), screen, pids: &self.pids, in_flight: post::in_flight() || self.posting.is_some() };
        let next = job.advance(&cx, now);
        match next {
            Next::Wait(_) => {}
            Next::Run(step) => {
                let ran = self.run_step(ob, screen, &step, now);
                if let Some(job) = self.jobs.front_mut() {
                    job.ran(&step, ran.is_ok());
                }
                if let Err(e) = ran {
                    tracing::info!(step = job::step_name(&step), error = e, "apple mode: a step did not run; tried again at the next settle");
                    self.last_refusal = Some(e);
                }
            }
            Next::Done(report) => self.job_ended(ob, report, now),
            Next::GiveUp(report) => {
                if let Some(Step::Focus(w)) = report.undo {
                    tracing::info!(window = w, "apple mode: given up: the newly opened app's stage brought in front, as Apple's own would");
                    let _ = self.run_step(ob, screen, &Step::Focus(w), now);
                }
                if let Some(pid) = report.unhide {
                    launch_hide::unhide(pid);
                }
                self.job_ended(ob, report, now);
            }
        }
    }

    /// The first job ended with `report`: logged (the route and its times), counted, the next
    /// one started at the next read.
    fn job_ended(&mut self, _ob: &Observer, report: Report, now: Instant) {
        let Some(job) = self.jobs.pop_front() else { return };
        let hide = match job.intent {
            Intent::OpenHere(pid) => self.hidings.remove(&pid).and_then(|h| h.outcome()),
            _ => None,
        };
        match &report.given_up {
            None => tracing::info!(
                intent = ?job.intent,
                route = report.route,
                ms = report.ms,
                first_window_ms = report.first_window_ms,
                hide = ?hide,
                focuses = report.focuses,
                clicks = report.clicks,
                drags = report.drags,
                "apple mode: done"
            ),
            Some(why) => {
                tracing::warn!(intent = ?job.intent, route = report.route, ms = report.ms, hide = ?hide, why, "apple mode: given up");
                self.last_refusal = Some(why.clone());
            }
        }
        self.last_report = Some(report);
        self.read_due = Some(now + mirror::MOTION_READ);
    }

    /// Runs `step` through its route now; the mirror counts in motion from here.
    fn run_step(&mut self, ob: &Observer, screen: &Display, step: &Step, now: Instant) -> Result<(), String> {
        let mut desk = Desk { ob, manager: self.manager, screen: Some(screen.frame) };
        let route: &dyn Route = match step {
            Step::Focus(_) => &Focus,
            Step::Post(Posted::Clicks { .. }) => &ShiftClick,
            Step::Post(Posted::Drag { .. }) => &Drag,
            Step::AwaitSettle | Step::AwaitStrip(_) => return Ok(()),
        };
        let t0 = Instant::now();
        let ran = route.run(step, &mut desk);
        let stats = self.stats.entry(route.name()).or_default();
        stats.last_ms = t0.elapsed().as_millis() as u64;
        match &ran {
            Ok(()) => stats.runs += 1,
            Err(_) => stats.refused += 1,
        }
        if ran.is_ok() {
            if let Step::Post(p) = step {
                self.posting = Some(p.clone());
            }
            self.mirror.step_began(now);
            self.watch();
            self.read_due = Some(now + mirror::MOTION_READ);
        }
        ran
    }

    /// The gesture in flight landed: the job told; the person's own input ending a drag sets
    /// the window's frame back; a gesture not posted counted as refused.
    fn gesture_landed(&mut self, ob: &Observer, landed: &Landed, now: Instant) {
        let posted = self.posting.take();
        if let Some(job) = self.jobs.front_mut() {
            job.landed(landed);
        }
        match landed {
            Landed::Posted => {}
            Landed::Refused(why) => {
                let name = match posted {
                    Some(Posted::Drag { .. }) => Drag.name(),
                    _ => ShiftClick.name(),
                };
                self.stats.entry(name).or_default().refused += 1;
                self.last_refusal = Some(why.clone());
            }
            Landed::Interrupted(why) => {
                if let Some(Posted::Drag { window, frame, .. }) = posted
                    && let Some(w) = ob.window(window)
                {
                    let back = w.element.set_frame(frame);
                    tracing::info!(window, why, back = back.is_ok(), "apple mode: the drag was ended by the person's own input; the window's frame set back");
                }
                self.last_refusal = Some(why.clone());
            }
        }
        self.mirror.step_began(now);
        self.watch();
    }

    /// Asks for `ask`: planned against the mirror into a job, queued, and run at once when
    /// it can (the first and settled).
    fn ask(&mut self, ob: &mut Observer, ask: Ask) -> Result<Value, Refusal> {
        let now = Instant::now();
        let refused = |this: &mut Self, r: Refusal| {
            this.stats.entry("plan").or_default().refused += 1;
            this.last_refusal = Some(r.0.clone());
            Err(r)
        };
        if self.model.is_none() {
            return refused(self, Refusal("not following Apple's Stage Manager yet (no settled read)".into()));
        }
        if self.jobs.len() > QUEUE {
            return refused(self, Refusal(format!("{} steps of ours are waiting already", self.jobs.len())));
        }
        let focused = crate::platform::controller::focused_window();
        let window = |id: Option<u32>| id.or(focused).ok_or_else(|| Refusal("no window is focused".into()));
        let planned = {
            let model = self.model.as_ref();
            let key = |windows: &[u32]| {
                let m = model?;
                let stage = m.stage_of(WindowId(*windows.first()?))?;
                m.stage(stage)?.key.map(|k| k.0)
            };
            match ask {
                Ask::Switch(n) => plan::switch_job(&self.mirror, n, key, now),
                Ask::Merge(n) => plan::merge(&self.mirror, n, now),
                Ask::Send(n, id) => window(id).and_then(|w| plan::send(&self.mirror, n, w, key, now)),
                Ask::New(id) => window(id).and_then(|w| plan::new_stage(&self.mirror, w, now)),
            }
        };
        let job = match planned {
            Ok(j) => j,
            Err(r) => return refused(self, r),
        };
        let intent = job.intent;
        let goals = serde_json::to_value(job.goals()).unwrap_or(Value::Null);
        let first = match job.goals().first() {
            Some(job::Goal::Front(w)) => json!({ "route": Focus.name(), "window": w }),
            Some(job::Goal::Gather { .. }) => json!({ "route": ShiftClick.name() }),
            Some(job::Goal::Out(w)) => json!({ "route": Drag.name(), "window": w }),
            None => json!({}),
        };
        let behind = self.jobs.len();
        self.jobs.push_back(job);
        if let Some(screen) = self.screen() {
            self.pump(ob, &screen, now);
        }
        let mut reply = json!({ "ok": true, "intent": intent, "goals": goals, "behind": behind, "waits_for_settle": !self.mirror.settled });
        if let (Some(r), Some(f)) = (reply.as_object_mut(), first.as_object()) {
            r.extend(f.clone());
        }
        Ok(reply)
    }

    /// A tripwire's Accessibility action on the strip item holding window `w` (one the mode
    /// follows: never an item of an app it does not manage).
    fn ax_try(&self, manager: i32, w: u32, action: &str) -> Result<Value, String> {
        if self.mirror.where_is(w).map(|(p, _)| p) != Some(mirror::Where::Strip) {
            return Err(format!("window {w} is not in Apple's strip"));
        }
        rejected::ax_action(manager, w, action).map(|()| json!({ "window": w }))
    }

    /// `ctl apple try`: one route run once by hand, the live check's diagnostics and
    /// tripwires (needs `STAGE_MANAGER_TEST=1`). Nothing waits for a settle or retries; the
    /// check reads the mirror afterwards.
    fn try_route(&mut self, ob: &Observer, t: AppleTry) -> Value {
        if std::env::var_os("STAGE_MANAGER_TEST").is_none() {
            return json!({ "ok": false, "error": "apple try needs STAGE_MANAGER_TEST=1" });
        }
        let Some(screen) = self.screen() else { return json!({ "ok": false, "error": "no screen yet" }) };
        let Some(manager) = self.manager else { return json!({ "ok": false, "error": "WindowManager is not running" }) };
        let now = Instant::now();
        let mut desk = Desk { ob, manager: Some(manager), screen: Some(screen.frame) };
        let input = desk.input();
        let point = |w: u32| self.mirror.click_at.get(&w).copied().ok_or(format!("window {w}'s thumbnail shows no part of its own"));
        let t0 = Instant::now();
        let ran: Result<Value, String> = match t {
            AppleTry::Focus(w) => Focus.can(&Step::Focus(w), &self.mirror, &input).map_err(|r| r.0).and_then(|()| Focus.run(&Step::Focus(w), &mut desk)).map(|()| json!({})),
            AppleTry::ShiftClick(w) => point(w).and_then(|at| {
                let step = Step::Post(Posted::click(w, at, !input.shift));
                ShiftClick.can(&step, &self.mirror, &input).map_err(|r| r.0)?;
                ShiftClick.run(&step, &mut desk)?;
                self.posting = if let Step::Post(p) = step { Some(p) } else { None };
                Ok(json!({ "at": at }))
            }),
            AppleTry::Drag(w) => match self.mirror.frames.get(&w).copied() {
                None => Err(format!("window {w} is not listed")),
                Some(frame) => {
                    let from = self.mirror.title_at.get(&w).copied().unwrap_or_else(|| points::title_point(frame));
                    let step = Step::Post(Posted::Drag { window: w, from, to: points::strip_drop(&screen, from), frame });
                    Drag.can(&step, &self.mirror, &input).map_err(|r| r.0).and_then(|()| Drag.run(&step, &mut desk)).map(|()| {
                        self.posting = if let Step::Post(p) = step { Some(p) } else { None };
                        json!({ "from": from })
                    })
                }
            },
            AppleTry::AxPress(w) => self.ax_try(manager, w, "AXPress"),
            AppleTry::AxAddToStage(w) => self.ax_try(manager, w, "AXAddToStage"),
            AppleTry::PidClick(w) => point(w).and_then(|at| rejected::pid_click(manager, at).map(|()| json!({ "at": at }))),
            AppleTry::ShiftFocus(w) => match ob.window(w) {
                Some(tw) => rejected::shift_focus(&tw.element, tw.pid).map(|()| json!({})),
                None => Err(format!("window {w} is not observed")),
            },
        };
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        tracing::info!(route = t.name(), ok = ran.is_ok(), ms, "apple mode: a route tried by hand");
        self.mirror.step_began(now);
        self.watch();
        self.read_due = Some(now + mirror::MOTION_READ);
        match ran {
            Ok(mut v) => {
                if let Some(o) = v.as_object_mut() {
                    o.insert("ok".into(), json!(true));
                    o.insert("route".into(), json!(t.name()));
                    o.insert("ms".into(), json!((ms * 10.0).round() / 10.0));
                }
                v
            }
            Err(e) => json!({ "ok": false, "route": t.name(), "error": e }),
        }
    }

    // ── ctl ─────────────────────────────────────────────────────────────────────────────

    /// A ctl command in this mode: `state`, `switch`, `merge`, `send`, `new`, `apple try` and
    /// `reload`; the rest answer that they do nothing here.
    pub fn ctl(&mut self, ob: &mut Observer, cmd: Command) -> Value {
        let asked = |r: Result<Value, Refusal>| r.unwrap_or_else(|Refusal(e)| json!({ "ok": false, "error": e }));
        match cmd {
            Command::State => self.state_json(),
            Command::Switch(n) => asked(self.ask(ob, Ask::Switch(n))),
            Command::Merge(n) => asked(self.ask(ob, Ask::Merge(n))),
            Command::Send(n, id) => asked(self.ask(ob, Ask::Send(n, id))),
            Command::New(id) => asked(self.ask(ob, Ask::New(id))),
            Command::AppleTry(t) => self.try_route(ob, t),
            Command::Reload => {
                self.reload_config();
                json!({ "ok": true })
            }
            Command::Adopted(ids) => {
                let known = self.model_window_ids();
                json!({ "ok": true, "adopted": ids.into_iter().filter(|id| known.contains(id)).collect::<Vec<_>>() })
            }
            other => json!({ "ok": false, "error": format!("{} does nothing while Apple's Stage Manager arranges the windows (with_apple = \"augment\")", ctl_name(&other)) }),
        }
    }

    /// `ctl state` in this mode: the engine, the model's stages with where Apple's shows each
    /// (`where`, `slot`), its windows, and the mirror's own numbers (`apple`).
    pub fn state_json(&self) -> Value {
        let engine = json!({
            "kind": EngineKind::Apple.name(),
            "why": "Apple's Stage Manager is on and [windows] with_apple = \"augment\"",
            "with_apple": self.with_apple().name(),
        });
        let now = Instant::now();
        let apple = json!({
            "settled": self.mirror.settled,
            "age_ms": self.mirror.read_at.map(|t| now.duration_since(t).as_millis() as u64),
            "read_ms": (self.read_ms * 10.0).round() / 10.0,
            "in_motion": self.mirror.in_motion,
            "list_vs_ax_misses": self.mirror.misses,
            "ax_age_ms": self.mirror.ax_at.map(|t| now.duration_since(t).as_millis() as u64),
            "strip_shown": self.mirror.strip_shown,
            "manager": self.manager,
            "current": self.mirror.current,
            "strip": self.mirror.strip.iter().map(|g| &g.windows).collect::<Vec<_>>(),
            "remembered": self.mirror.remembered.iter().map(|g| &g.windows).collect::<Vec<_>>(),
            "click_at": self.mirror.click_at,
            "pending": self.jobs.iter().map(|j| json!({ "intent": j.intent, "goals": j.goals(), "waits_for": j.waits_for, "age_ms": now.duration_since(j.since).as_millis() as u64, "report": j.report() })).collect::<Vec<_>>(),
            "posting": self.posting,
            "hiding": self.hidings.values().map(|h| json!({ "pid": h.pid, "age_ms": now.duration_since(h.since).as_millis() as u64, "outcome": h.outcome() })).collect::<Vec<_>>(),
            "routes": {
                "focus": self.stats.get("focus").copied().unwrap_or_default(),
                "shift_click": self.stats.get("shift_click").copied().unwrap_or_default(),
                "drag": self.stats.get("drag").copied().unwrap_or_default(),
            },
            "last": self.last_report,
            "last_refusal": self.last_refusal,
        });
        let Some(m) = &self.model else {
            return json!({ "ok": false, "error": "not following Apple's Stage Manager yet (no settled read)", "engine": engine, "apple": apple });
        };
        let active = m.main_active_stage();
        let stages: Vec<Value> = m
            .stages()
            .iter()
            .filter(|s| !s.windows.is_empty())
            .map(|s| {
                let place = s.windows.first().and_then(|w| self.mirror.where_is(w.0));
                json!({
                    "id": s.id.0,
                    "space": s.space.0,
                    "number": m.stage_number(s.id),
                    "name": s.name,
                    "active": active == Some(s.id),
                    "key": s.key.map(|k| k.0),
                    "windows": s.windows.iter().map(|w| w.0).collect::<Vec<_>>(),
                    "where": place.map(|(w, _)| w),
                    "slot": place.and_then(|(_, slot)| slot),
                })
            })
            .collect();
        let windows: Vec<Value> = m
            .windows()
            .map(|w| {
                json!({
                    "id": w.meta.id.0,
                    "pid": w.meta.pid,
                    "bundle_id": w.meta.bundle_id,
                    "title": w.meta.title,
                    "stage": m.stage_of(w.meta.id).map(|s| s.0),
                    "restore": w.restore,
                    "listed": self.mirror.frames.get(&w.meta.id.0),
                    "parked": w.parked,
                    "where": self.mirror.where_is(w.meta.id.0).map(|(w, _)| w),
                })
            })
            .collect();
        json!({
            "ok": true,
            "engine": engine,
            "active": active.map(|s| s.0),
            "stages": stages,
            "windows": windows,
            "display": self.screen().map(|d| json!({ "id": d.id.0, "frame": d.frame, "visible": d.visible })),
            "apple": apple,
        })
    }

    // ── Persistence ─────────────────────────────────────────────────────────────────────

    /// Writes `state-apple.json` now (the model as it follows Apple's, in `state.json`'s
    /// format), atomically and durably; `state.json` stays set aside meanwhile.
    pub fn save_now(&mut self) {
        let Some(m) = self.model.as_ref() else {
            self.save.saved();
            return;
        };
        let path = paths::apple_state_file();
        let now = crate::platform::controller::unix_now();
        let mut file = m.to_state();
        file.session = session::window_session().map(str::to_string);
        file.stamp(now, &mut self.seen_times);
        let text = match file.to_json() {
            Ok(t) => t,
            Err(e) => {
                let wait = self.save.failed(Instant::now());
                tracing::error!(retry_s = wait.as_secs(), "{e}; state-apple.json on disk is kept");
                return;
            }
        };
        let result = crate::private_files::ensure_dir(&paths::config_dir())
            .and_then(|_| crate::platform::durable::replace(&path, text.as_bytes(), crate::platform::durable::Flush::Full));
        match result {
            Ok(()) => {
                self.save.saved();
                tracing::debug!(bytes = text.len(), "apple mode: stages saved to {}", path.display());
            }
            Err(e) => {
                let wait = self.save.failed(Instant::now());
                tracing::warn!(retry_s = wait.as_secs(), "cannot save {}: {e}; tried again later", path.display());
            }
        }
    }
}

impl Default for AppleController {
    fn default() -> Self {
        Self::new()
    }
}

/// The mode ending (Apple's turned off, a quit) mid-step leaves nothing of ours behind: a
/// gesture in flight is ended, and a newly opened app hidden at its launch whose window never
/// came in is shown again.
impl Drop for AppleController {
    fn drop(&mut self) {
        post::end_in_flight("the Apple mode ended");
        for h in self.hidings.values() {
            h.cancel();
            if matches!(h.outcome(), Some(HideOutcome::Hidden { .. })) {
                tracing::info!(pid = h.pid, "apple mode: ended mid-step: an app hidden at its launch shown again");
                launch_hide::unhide(h.pid);
            }
        }
    }
}

/// What a ctl command or a hotkey asks of the Apple mode.
#[derive(Debug, Clone, Copy)]
enum Ask {
    /// Stage N in front.
    Switch(usize),
    /// Stage N's windows into the stage in front.
    Merge(usize),
    /// A window (the focused one when `None`) to stage N.
    Send(usize, Option<u32>),
    /// A window (the focused one when `None`) out to a stage of its own.
    New(Option<u32>),
}

/// The windows the Apple mode follows: the observer's standard windows the window server
/// lists (a window minimized is Apple's to put in the strip).
fn managed(ob: &Observer, listed: &mirror::Listed) -> BTreeSet<u32> {
    ob.windows().values().filter(|w| w.kind == WindowKind::Standard && listed.has(w.id)).map(|w| w.id).collect()
}

/// A ctl command's name for a refusal.
fn ctl_name(cmd: &Command) -> String {
    let debug = format!("{cmd:?}");
    let name = debug.split(['(', ' ', '{']).next().unwrap_or("this command").to_string();
    name.to_lowercase()
}
