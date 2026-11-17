//! The WM thread's one door to whoever conducts the windows ([`Conductor`]): the app's own
//! [`Controller`] (its stages, Corner or Spaces), or the Apple mode's [`AppleController`]
//! while Apple's Stage Manager is on and `[windows] with_apple = "augment"`
//! (docs/STRATEGIES.md, "Apple-augmented"). Each call the WM thread makes is one `match`
//! here, as the model's door (`engine/model/hiding.rs`) and the controller's upkeep door
//! (`controller/upkeep.rs`) are; no other file asks which one conducts. The controller is
//! untouched by the Apple mode: what would fight Apple's (batches, pictures, the strip, the
//! park upkeep) never runs while Apple's arranges.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::apple::controller::AppleController;
use super::apps::AppInfo;
use super::controller::Controller;
use super::ctl::Command;
use super::input::InputEvent;
use super::minimize_tap::MinimizeAsk;
use super::native_settings::RawSettings;
use super::observe::{Observer, WinEvent};
use super::safety;
use crate::config::HotkeyCommand;
use crate::engine::model::Display;
use crate::engine::stand_down::WithApple;
use crate::ui::strip_model::{StripDrop, StripEvent};

/// Who conducts the windows now.
pub enum Conductor {
    /// The app's own stages.
    Ours(Box<Controller>),
    /// Apple's Stage Manager's, followed.
    Apples(Box<AppleController>),
}

impl Conductor {
    /// A fresh controller of the app's own (no stages yet).
    pub fn ours() -> Self {
        Self::Ours(Box::default())
    }

    /// The Apple mode's conductor.
    pub fn apples() -> Self {
        Self::Apples(Box::default())
    }

    /// True while the Apple mode conducts.
    pub fn is_apples(&self) -> bool {
        matches!(self, Self::Apples(_))
    }

    /// `[windows] with_apple` as the config in force has it.
    pub fn with_apple(&self) -> WithApple {
        match self {
            Self::Ours(c) => c.with_apple(),
            Self::Apples(a) => a.with_apple(),
        }
    }

    /// The observer's events.
    pub fn on_events(&mut self, ob: &mut Observer, events: Vec<WinEvent>) {
        match self {
            Self::Ours(c) => c.on_events(ob, events),
            Self::Apples(a) => a.on_events(ob, events),
        }
    }

    /// Input in another app.
    pub fn input(&mut self, ob: &mut Observer, event: InputEvent, drop: Option<StripDrop>) {
        match self {
            Self::Ours(c) => c.input(ob, event, drop),
            Self::Apples(a) => a.input(ob, event),
        }
    }

    /// A minimize the tap caught: ours to the strip; Apple's, macOS's own.
    pub fn minimize_direct(&mut self, ob: &mut Observer, ask: MinimizeAsk) {
        match self {
            Self::Ours(c) => c.minimize_direct(ob, ask),
            Self::Apples(_) => ask.as_macos("Apple's Stage Manager arranges the windows"),
        }
    }

    /// A hotkey.
    pub fn hotkey(&mut self, ob: &mut Observer, command: HotkeyCommand) {
        match self {
            Self::Ours(c) => c.hotkey(ob, command),
            Self::Apples(a) => a.hotkey(ob, command),
        }
    }

    /// Input from our strip (none shows in the Apple mode).
    pub fn strip_event(&mut self, ob: &mut Observer, event: StripEvent) {
        match self {
            Self::Ours(c) => c.strip_event(ob, event),
            Self::Apples(_) => tracing::debug!("apple mode: a strip event while our strip is hidden; ignored"),
        }
    }

    /// The strip shown or hidden (ours; Apple's own in the Apple mode).
    pub fn toggle_strip(&mut self) {
        match self {
            Self::Ours(c) => c.toggle_strip(),
            Self::Apples(_) => tracing::info!("apple mode: Apple's Stage Manager draws the strip; ours stays hidden"),
        }
    }

    /// A Desktop change.
    pub fn on_space_changed(&mut self, ob: &mut Observer) {
        match self {
            Self::Ours(c) => c.on_space_changed(ob),
            Self::Apples(a) => a.on_space_changed(ob),
        }
    }

    /// An app just opened.
    pub fn app_launched(&mut self, ob: &Observer, pid: i32) {
        match self {
            Self::Ours(c) => c.app_launched(ob, pid),
            Self::Apples(a) => a.app_launched(ob, pid),
        }
    }

    /// An app about to launch (the Apple mode's "open here" hides it this early; ours waits
    /// for its windows).
    pub fn app_will_launch(&mut self, ob: &Observer, app: &AppInfo) {
        match self {
            Self::Ours(_) => {}
            Self::Apples(a) => a.app_will_launch(ob, app),
        }
    }

    /// An app activated (the Apple mode hides a running app with no window that the rules
    /// want here, before the window it reopens shows; ours places that window when it comes).
    pub fn app_activated(&mut self, ob: &Observer, app: &AppInfo) {
        match self {
            Self::Ours(_) => {}
            Self::Apples(a) => a.app_activated(ob, app),
        }
    }

    /// Re-read config.toml.
    pub fn reload_config(&mut self, ob: &mut Observer) {
        match self {
            Self::Ours(c) => c.reload_config(ob),
            Self::Apples(a) => a.reload_config(),
        }
    }

    /// A ctl command.
    pub fn ctl(&mut self, ob: &mut Observer, cmd: Command) -> Value {
        match self {
            Self::Ours(c) => c.ctl(ob, cmd),
            Self::Apples(a) => a.ctl(ob, cmd),
        }
    }

    /// The displays.
    pub fn set_displays(&mut self, ob: &mut Observer, displays: Vec<Display>, heard: Instant) {
        match self {
            Self::Ours(c) => c.set_displays(ob, displays, heard),
            Self::Apples(a) => a.set_displays(displays),
        }
    }

    /// Fresh `com.apple.WindowManager` values.
    pub fn set_settings(&mut self, raw: &RawSettings) {
        match self {
            Self::Ours(c) => c.set_settings(raw),
            Self::Apples(a) => a.set_settings(raw),
        }
    }

    /// The tick.
    pub fn tick(&mut self, ob: &mut Observer) {
        match self {
            Self::Ours(c) => c.tick(ob),
            Self::Apples(a) => a.tick(ob),
        }
    }

    /// The fast tick.
    pub fn fast_tick(&mut self, ob: &mut Observer) {
        match self {
            Self::Ours(c) => c.fast_tick(ob),
            Self::Apples(a) => a.fast_tick(ob),
        }
    }

    /// True while the fast tick has work.
    pub fn wants_fast_tick(&self) -> bool {
        match self {
            Self::Ours(c) => c.wants_fast_tick(),
            Self::Apples(a) => a.wants_fast_tick(),
        }
    }

    /// Why the tick stays brisk.
    pub fn tick_waits(&self, now: Instant) -> Option<&'static str> {
        match self {
            Self::Ours(c) => c.tick_waits(now),
            Self::Apples(a) => a.tick_waits(),
        }
    }

    /// The longest the tick may rest.
    pub fn tick_rest(&self) -> Duration {
        match self {
            Self::Ours(c) => c.tick_rest(),
            Self::Apples(a) => a.tick_rest(),
        }
    }

    /// True while the restore window is open (ours only).
    pub fn restore_window_open(&self) -> bool {
        match self {
            Self::Ours(c) => c.restore_window_open(),
            Self::Apples(_) => false,
        }
    }

    /// True once stages are built (ours) or followed (Apple's).
    pub fn managing(&self) -> bool {
        match self {
            Self::Ours(c) => c.managing(),
            Self::Apples(a) => a.managing(),
        }
    }

    /// The ⌘Tab switcher's notices (ours only: Apple's is Apple's).
    pub fn switcher_notice(&mut self, ob: &Observer, name: &str) {
        match self {
            Self::Ours(c) => c.switcher_notice(ob, name),
            Self::Apples(_) => {}
        }
    }

    /// Every parked window back and the stages saved (ours); nothing is parked in the Apple
    /// mode: its stages saved, 0 failed.
    pub fn unpark_all(&mut self, ob: &mut Observer) -> usize {
        match self {
            Self::Ours(c) => c.unpark_all(ob),
            Self::Apples(a) => {
                a.save_now();
                0
            }
        }
    }

    /// The shows still owed after the last look (ours); none in the Apple mode.
    pub fn account_shows_owed(&mut self, ob: &Observer) -> usize {
        match self {
            Self::Ours(c) => c.account_shows_owed(ob),
            Self::Apples(_) => 0,
        }
    }

    /// The windows still owed a show (ours); none in the Apple mode.
    pub fn shows_owed_ids(&self) -> BTreeSet<u32> {
        match self {
            Self::Ours(c) => c.shows_owed_ids(),
            Self::Apples(_) => BTreeSet::new(),
        }
    }

    /// What the safety paths know of whose windows are ours: the Apple mode hides none, so
    /// only the registry (a release's held windows) and the saved stages.
    pub fn safety_ours(&self) -> safety::Ours {
        match self {
            Self::Ours(c) => c.safety_ours(),
            Self::Apples(_) => safety::Ours::now(BTreeSet::new()),
        }
    }

    /// Every window the model manages or follows.
    pub fn model_window_ids(&self) -> BTreeSet<u32> {
        match self {
            Self::Ours(c) => c.model_window_ids(),
            Self::Apples(a) => a.model_window_ids(),
        }
    }

    /// The stages saved now (`state.json`; `state-apple.json` in the Apple mode).
    pub fn save_now(&mut self) {
        match self {
            Self::Ours(c) => c.save_now(),
            Self::Apples(a) => a.save_now(),
        }
    }

    /// After a panic: the registry resynced from the model (ours; nothing in the Apple mode).
    pub fn resync_registry(&self, ob: &Observer) {
        match self {
            Self::Ours(c) => c.resync_registry(ob),
            Self::Apples(_) => {}
        }
    }
}
