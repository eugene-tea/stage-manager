//! The Apple mode's conductor ([`AppleController`]): what the WM thread drives instead of the
//! app's own [`crate::platform::controller::Controller`] while Apple's Stage Manager is on
//! and `[windows] with_apple = "augment"` (`platform::conductor`).
//!
//! It arranges nothing itself. It reads Apple's arrangement (the window list at every read,
//! WindowManager's Accessibility as the slow check: [`crate::engine::apple::mirror`]), keeps
//! the app's model in step with it once it is settled ([`crate::engine::apple::follow`]),
//! saves that model to `state-apple.json`, and carries out the switch through the focus
//! route (`ctl switch N`, the switch hotkeys: [`crate::engine::apple::plan`]). Apple's posts
//! no Accessibility notices, so it polls: a read at every activation, window found or gone,
//! focus, mouse-up near the strip and Desktop change; then every 50 ms while anything moves
//! (the list only, on the fast tick); every 2 s at rest, Accessibility with it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use super::desk::Desk;
use super::routes::focus::Focus;
use super::routes::{Route, RouteStats};
use crate::config::{ConfigStore, HotkeyCommand};
use crate::engine::apple::desk::AppleDesk;
use crate::engine::apple::follow;
use crate::engine::apple::mirror::{self, Arrangement};
use crate::engine::apple::plan::{self, Intent, Refusal, Step};
use crate::engine::model::{Display, Model, WindowId, WindowMeta, WindowRole};
use crate::engine::recovery::EngineKind;
use crate::engine::save_clock::SaveClock;
use crate::engine::settings::NativeSettings;
use crate::engine::stand_down::WithApple;
use crate::platform::ctl::Command;
use crate::platform::input::{InputEvent, InputKind};
use crate::platform::native_settings::RawSettings;
use crate::platform::observe::{Observer, WinEvent, WindowKind};
use crate::platform::{logging, paths, session};

/// How often config.toml's modification time is checked (as the controller does).
const CONFIG_POLL: Duration = Duration::from_secs(1);

/// How often WindowManager's pid is looked up again (it restarts with the Dock's settings).
const MANAGER_CHECK: Duration = Duration::from_secs(10);

/// A mouse-up this close to the screen's left edge may have changed Apple's strip.
const NEAR_STRIP: f64 = 300.0;

/// A switch not seen done is tried once more after the next settle, then given up.
const SWITCH_TRIES: u32 = 2;

/// A switch waiting longer than this for a settle is given up.
const SWITCH_WAIT: Duration = Duration::from_millis(1500);

/// How long reads go on at the motion's pace after an event that may change Apple's
/// arrangement: its motion starts about 100 ms after the cause and runs about 600 ms.
const WATCH: Duration = Duration::from_millis(1200);

/// An intent carried out, or waiting for the arrangement to settle.
#[derive(Debug, Clone)]
struct Pending {
    /// What was asked.
    intent: Intent,
    /// The window the focus route focuses.
    window: u32,
    /// Focuses run so far.
    tries: u32,
    /// What it waits for (`ctl state`).
    waits_for: &'static str,
    /// When it was asked.
    since: Instant,
}

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
    pending: Option<Pending>,
    focus_stats: RouteStats,
    last_refusal: Option<String>,
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
        tracing::info!("apple mode: following Apple's Stage Manager (its stages read into the model; switches through the focus route)");
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
            pending: None,
            focus_stats: RouteStats::default(),
            last_refusal: None,
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

    /// Input in another app: a mouse-up near the strip may have changed it.
    pub fn input(&mut self, ob: &mut Observer, event: InputEvent) {
        let near = self.screen().is_some_and(|d| event.at.x < d.frame.x + NEAR_STRIP);
        if event.kind == InputKind::MouseUp && near {
            self.watch();
            self.read(ob, false);
        }
    }

    /// A hotkey: switch to stage N through the focus route; the others are not in this mode
    /// yet (said once in the log).
    pub fn hotkey(&mut self, ob: &mut Observer, command: HotkeyCommand) {
        match command {
            HotkeyCommand::SwitchTo(n) => {
                let r = self.switch(ob, n);
                if let Err(Refusal(why)) = r {
                    tracing::info!(n, why, "apple mode: the switch hotkey did nothing");
                }
            }
            other => tracing::info!(?other, "apple mode: this hotkey does nothing while Apple's Stage Manager arranges the windows"),
        }
    }

    /// A Desktop change: the arrangement is read afresh (the stages of the Desktop left are
    /// remembered).
    pub fn on_space_changed(&mut self, ob: &mut Observer) {
        self.watch();
        self.read(ob, true);
    }

    /// An app just opened: its stage shows soon.
    pub fn app_launched(&mut self) {
        self.watch();
        self.read_due = Some(Instant::now());
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

    /// The fast tick: the list read every [`mirror::MOTION_READ`] while anything moves.
    pub fn fast_tick(&mut self, ob: &mut Observer) {
        let now = Instant::now();
        if self.read_due.is_some_and(|t| now >= t) {
            self.read(ob, false);
        }
    }

    /// True while a read is due soon (in motion, not yet settled, a switch waiting).
    pub fn wants_fast_tick(&self) -> bool {
        self.read_due.is_some()
    }

    /// Why the tick stays brisk for this mode.
    pub fn tick_waits(&self) -> Option<&'static str> {
        if self.pending.is_some() {
            Some("a switch in Apple's Stage Manager")
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
        let mut desk = Desk { ob, manager: self.manager };
        let listed = desk.listed();
        let reading = if with_ax { desk.strip(&screen) } else { None };
        let managed: BTreeSet<u32> = managed(ob, &listed);
        let misses = self.mirror.misses;
        self.mirror = mirror::fold(&self.mirror, reading.as_ref(), &listed, &managed, &screen, now);
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
        self.read_due = (!self.mirror.settled || self.pending.is_some() || watching).then(|| now + mirror::MOTION_READ);
        if self.mirror.settled {
            self.follow(ob, &screen, &managed);
            self.after_settle(ob, now);
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

    /// A settled read with a switch waiting: run it, or see it done, or try once more, or
    /// give it up.
    fn after_settle(&mut self, ob: &Observer, now: Instant) {
        let Some(mut p) = self.pending.take() else { return };
        if p.tries > 0 && self.mirror.current.contains(&p.window) {
            tracing::info!(window = p.window, tries = p.tries, ms = now.duration_since(p.since).as_millis() as u64, "apple mode: switched");
            return;
        }
        if p.tries >= SWITCH_TRIES || now.duration_since(p.since) > SWITCH_WAIT + mirror::SETTLE * SWITCH_TRIES {
            let why = format!("{:?}: window {} not in front after {} tries", p.intent, p.window, p.tries);
            tracing::warn!(why, "apple mode: a switch given up");
            self.focus_stats.refused += 1;
            self.last_refusal = Some(why);
            return;
        }
        match self.run_focus(ob, p.window, now) {
            Ok(()) => {
                p.tries += 1;
                p.waits_for = "the window in front";
                self.pending = Some(p);
            }
            Err(Refusal(why)) => {
                self.focus_stats.refused += 1;
                self.last_refusal = Some(why);
            }
        }
    }

    /// The focus route for `window` now; the mirror counts in motion from here.
    fn run_focus(&mut self, ob: &Observer, window: u32, now: Instant) -> Result<(), Refusal> {
        let step = Step::Focus(window);
        let mut desk = Desk { ob, manager: self.manager };
        let input = desk.input();
        Focus.can(&step, &self.mirror, &input)?;
        let t0 = Instant::now();
        let ran = Focus.run(&step, &mut desk);
        self.focus_stats.last_ms = t0.elapsed().as_millis() as u64;
        match ran {
            Ok(()) => {
                self.focus_stats.runs += 1;
                self.mirror.step_began(now);
                self.watch();
                self.read_due = Some(now + mirror::MOTION_READ);
                Ok(())
            }
            Err(e) => Err(Refusal(e)),
        }
    }

    /// Switches to stage `n` (the strip's slots, then the remembered stages) through the
    /// focus route: at once when settled, else after the settle.
    fn switch(&mut self, ob: &mut Observer, n: usize) -> Result<Value, Refusal> {
        let now = Instant::now();
        let key = |windows: &[u32]| {
            let m = self.model.as_ref()?;
            let stage = m.stage_of(WindowId(*windows.first()?))?;
            m.stage(stage)?.key.map(|k| k.0)
        };
        let steps = match plan::switch(&self.mirror, n, key) {
            Ok(s) => s,
            Err(r) => {
                self.focus_stats.refused += 1;
                self.last_refusal = Some(r.0.clone());
                return Err(r);
            }
        };
        let Some(Step::Focus(window)) = steps.last().copied() else { return Err(Refusal("no focus planned".into())) };
        let waits = steps.contains(&Step::AwaitSettle);
        let mut pending = Pending { intent: Intent::Switch(n), window, tries: 0, waits_for: "the settle", since: now };
        if !waits {
            if let Err(r) = self.run_focus(ob, window, now) {
                self.focus_stats.refused += 1;
                self.last_refusal = Some(r.0.clone());
                return Err(r);
            }
            pending.tries = 1;
            pending.waits_for = "the window in front";
        }
        self.pending = Some(pending);
        self.read_due.get_or_insert(now + mirror::MOTION_READ);
        Ok(json!({ "ok": true, "route": Focus.name(), "window": window, "waits_for_settle": waits, "ms": self.focus_stats.last_ms }))
    }

    // ── ctl ─────────────────────────────────────────────────────────────────────────────

    /// A ctl command in this mode: `state`, `switch` and `reload`; the rest answer that they
    /// do nothing here.
    pub fn ctl(&mut self, ob: &mut Observer, cmd: Command) -> Value {
        match cmd {
            Command::State => self.state_json(),
            Command::Switch(n) => match self.switch(ob, n) {
                Ok(v) => v,
                Err(Refusal(e)) => json!({ "ok": false, "error": e }),
            },
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
            "pending": self.pending.iter().map(|p| json!({ "intent": p.intent, "window": p.window, "tries": p.tries, "waits_for": p.waits_for })).collect::<Vec<_>>(),
            "routes": { "focus": self.focus_stats },
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
