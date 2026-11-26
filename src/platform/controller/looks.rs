//! Mission Control looked for before the poll does, under the Corner engine (round
//! looks-forward; the Spaces engine follows it for its lends, [`super::mission_control`]).
//!
//! The Corner engine's hidden windows are on their Desktop, so nothing is put home for
//! Mission Control. But its upkeep leans on knowing that it shows: the inert slivers' shield
//! goes off for it, the park reconcile, the stills and the patch stop judging the window
//! list ([`super::corner`]). Found by the Space check alone it was known up to a second late
//! (two at rest), and meanwhile the reconcile read Mission Control's layout as parked
//! windows out of their corner.
//!
//! **What brings the look forward** ([`crate::engine::look_watch`]); none is a fact, each
//! makes [`Controller::look_for_overview`] run now and for a moment at the fast tick, and the
//! Space check's look stays behind them:
//!
//! - the window server's notice (1508; [`super::server_notices`], every engine's), while it
//!   is trusted ([`crate::platform::server_notices::works`]);
//! - the public triggers, only while it is not (no call to register it on this macOS, none
//!   came for the last opens and closes, or its notices rest): one of Mission Control's
//!   shortcuts heard by the key tap ([`crate::platform::mission_key_tap`]) or by the input
//!   monitor, and the Dock's `AXCreated` ([`Controller::looks_dock_notice`]; whether the
//!   Dock sends one as Mission Control opens is not measured: `ctl state` counts what each
//!   found).
//!
//! **While it shows** the key tap is on whatever tells of its opening, for what closes it:
//! its key or Escape, a press. Each brings the looks for its close at the fast tick, and
//! with `[windows] mission_control_key_keeps_stage` a close by key makes what macOS brings in
//! front next no pick ([`Controller::looks_pick`], [`crate::engine::mission_close`]): no
//! switch, and the stage in front's key window is focused again once Mission Control is
//! gone. With the option off nothing is judged: macOS's activation brings its stage, Apple's
//! way, as before.
//!
//! Every line in the log starts with `mission-control:`.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Controller;
use super::mission_control::{gives_focus_back, shortcut};
use crate::engine::look_watch::LookWatch;
use crate::engine::mission_close::{self, CloseWatch, Verdict};
use crate::engine::mission_control::{self as mc, Hint, Keys, Mods, PickBy};
use crate::engine::overview::Overview;
use crate::platform::input::InputEvent;
use crate::platform::mission_key_tap::{self, Heard, HeardAt};
use crate::platform::observe::Observer;
use crate::platform::space_keys::{Expose, SpaceKey};
use crate::platform::{costs, server_notices, windows};

/// How long Mission Control's shortcuts, read from the user's settings, are trusted.
const KEYS_FRESH: Duration = Duration::from_secs(30);

/// What the controller keeps for the looks ([module docs](self)).
#[derive(Debug, Default)]
pub(super) struct Looks {
    /// When the looks come at the fast tick.
    watch: LookWatch,
    /// Mission Control's shortcuts as last read, and when.
    keys: Option<(Keys, Instant)>,
    /// What its close came with: the key that closed it, a click.
    close: CloseWatch,
    /// An activation found no pick: the stage in front's key window is focused again once
    /// Mission Control is gone (since when it waits).
    refocus: Option<Instant>,
    /// Looks made at a hint or by its watch, and how many found Mission Control's opening
    /// or its close; by what announced an opening.
    looks: u64,
    found: u64,
    by_key: u64,
    by_dock: u64,
    by_server: u64,
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

impl Controller {
    /// True while Mission Control or App Exposé shows, as the last look had it.
    fn looks_showing(&self) -> bool {
        self.overview.showing() == Some(Overview::MissionControl)
    }

    /// True while the public triggers bring the looks: the window server's notices are not
    /// trusted to ([`server_notices::works`]).
    fn looks_public(&self) -> bool {
        !server_notices::works()
    }

    /// Mission Control's shortcuts, read again when older than [`KEYS_FRESH`].
    fn looks_keys(&mut self, now: Instant) -> Keys {
        if let Some((keys, at)) = self.looks.keys
            && now.saturating_duration_since(at) < KEYS_FRESH
        {
            return keys;
        }
        let keys = Keys {
            mission_control: shortcut(SpaceKey::read_expose(Expose::MissionControl)),
            app_windows: shortcut(SpaceKey::read_expose(Expose::AppWindows)),
        };
        if self.looks.keys.map(|(k, _)| k) != Some(keys) {
            tracing::debug!(?keys, "mission-control: its shortcuts as Keyboard Shortcuts has them");
        }
        self.looks.keys = Some((keys, now));
        keys
    }

    /// One look for an overview now, brought by a trigger: the window list read, Mission
    /// Control's opening or close taken ([`Controller::look_for_overview`]), the key tap told
    /// what to listen for. Nothing is inferred from what a locked screen lists.
    pub(super) fn looks_look(&mut self, now: Instant) {
        if self.locked {
            return;
        }
        let Some(main) = self.model.as_ref().map(|m| *m.main_display()) else { return };
        let was = self.looks_showing();
        let list = windows::wake_on_screen();
        costs::bump(costs::Cost::TriggerLook);
        self.looks.looks += 1;
        self.looks.watch.looked(now);
        self.overview.look_at_notice(true);
        self.look_for_overview(&list, main.frame, now);
        self.overview.look_at_notice(false);
        if self.looks_showing() != was {
            self.looks.found += 1;
        }
        self.looks_publish(now);
    }

    /// The overview shown changed at a look (any look: a trigger's or the Space check's),
    /// `showing` Mission Control or App Exposé now: the watch learns it, and a Mission
    /// Control that shows anew starts with no close heard.
    pub(super) fn looks_saw(&mut self, showing: bool, now: Instant) {
        let hint = self.looks.watch.saw(showing, now);
        if !showing {
            return;
        }
        self.looks.close.opened();
        match hint {
            Some((Hint::Key(_), _)) => self.looks.by_key += 1,
            Some((Hint::Dock, _)) => self.looks.by_dock += 1,
            Some((Hint::Server, _)) => self.looks.by_server += 1,
            None => {}
        }
        tracing::info!(
            hint = hint.map(|(h, _)| h.name()),
            after_hint_ms = hint.map(|(_, d)| ms(d)),
            found_within_ms = self.overview.found_within().map(ms),
            "mission-control: shown"
        );
    }

    /// A hint that Mission Control may be opening (`by`: who heard it): the look now, and at
    /// the fast tick for its watch.
    fn looks_hint(&mut self, hint: Hint, by: &'static str, now: Instant) {
        if self.looks_showing() || !self.looks.watch.hint(hint, now) {
            return;
        }
        tracing::debug!(hint = hint.name(), by, "mission-control: it may be opening: looked for now");
        self.looks_look(now);
    }

    /// The window server's notice (1508): the look now, whatever shows (its close is then
    /// looked for at every fast tick, [`Controller::server_fast_tick`]).
    pub(super) fn looks_server_notice(&mut self, now: Instant) {
        if !self.looks_showing() {
            self.looks.watch.hint(Hint::Server, now);
        }
        self.looks_look(now);
    }

    /// A key-down in another app, heard by the input monitor: one of Mission Control's
    /// shortcuts is a hint, while the public triggers bring the looks. Typing reads nothing.
    /// macOS keeps its own shortcuts from the monitor as a rule: its key tap hears them
    /// ([`Self::looks_heard`]).
    pub(super) fn looks_key(&mut self, event: &InputEvent, now: Instant) {
        let Some(code) = event.key else { return };
        let mods = Mods { control: event.control, option: event.option, command: event.cmd };
        if !mc::may_trigger(code, mods) || !self.looks_public() {
            return;
        }
        self.looks_key_heard(code, mods, "the input monitor", now);
    }

    /// What Mission Control's key tap heard: one of its keys, or a press while it shows.
    pub(super) fn looks_heard(&mut self, heard: HeardAt) {
        match heard.what {
            Heard::Key { code, mods } => self.looks_key_heard(code, mods, "the key tap", heard.at),
            Heard::Click => self.looks_click(heard.at),
            // ⌘Tab's switcher's only.
            Heard::CommandUp => {}
        }
    }

    /// A mouse-down at `now` (the input monitor's or the key tap's): while Mission Control
    /// shows it may close it, and what follows its close is then a pick.
    pub(super) fn looks_click(&mut self, now: Instant) {
        if self.looks_showing() {
            self.looks.close.click(now);
            self.looks.watch.close_hint(now);
        }
    }

    /// One of Mission Control's keys heard (`by`: the key tap or the input monitor): while it
    /// shows, a key that closes it; else one of its shortcuts is a hint. Mission Control
    /// opened without a key and not yet seen is looked for first, so that its own shortcut
    /// pressed to close it is not taken for an opening.
    fn looks_key_heard(&mut self, code: u16, mods: Mods, by: &'static str, now: Instant) {
        let keys = self.looks_keys(now);
        let trigger = mc::trigger(&keys, code, mods);
        if trigger.is_some() && !self.looks_showing() {
            self.looks_look(now);
        }
        if self.looks_showing() {
            if mission_close::closes(&keys, code, mods) {
                self.looks.close.key(now);
                self.looks.watch.close_hint(now);
                let keeps = self.store.config().windows.mission_key_keeps_stage;
                let then = if keeps { "an app macOS brings in front now is no pick" } else { "an app macOS brings in front now brings its stage, as Apple's" };
                tracing::info!(key = code, by, keeps_stage = keeps, "mission-control: a key that closes it: {then}");
            }
            return;
        }
        let Some(trigger) = trigger else { return };
        if self.looks.watch.hint(Hint::Key(trigger), now) {
            tracing::info!(key = code, by, "mission-control: trigger seen: {}", trigger.name());
        }
    }

    /// A notice of the Dock's (it draws Mission Control; the same notice comes for ⌘Tab's
    /// switcher and its menus): a hint while the public triggers bring the looks.
    pub(super) fn looks_dock_notice(&mut self, name: &str, now: Instant) {
        if name != "AXCreated" || !self.looks_public() {
            return;
        }
        self.looks_hint(Hint::Dock, "the Dock's notice", now);
    }

    /// True while the looks come at the fast tick: a hint's watch, a close's, or a focus
    /// waiting for Mission Control to go.
    pub(super) fn looks_wants_fast(&self, now: Instant) -> bool {
        self.looks.watch.watching(self.looks_showing(), now) || self.looks.refocus.is_some()
    }

    /// The fast tick: a look when the watch wants one, the focus given back when due.
    pub(super) fn looks_fast_tick(&mut self, ob: &Observer, now: Instant) {
        let showing = self.looks_showing();
        if self.looks.watch.look_due(showing, now) || (self.looks.refocus.is_some() && showing) {
            self.looks_look(now);
        }
        self.looks_refocus_due(ob, now);
    }

    /// The tick: the Dock observed while its notice may serve (its observer stays once made:
    /// ⌘Tab's switcher is followed through it under every engine since round arranged-ahead,
    /// [`super::switcher`]; a notice of it is a hint here only while the public triggers
    /// bring the looks, [`Self::looks_dock_notice`]), the focus given back when
    /// due, a close long over forgotten, the key tap told what to listen for. The look itself
    /// is the Space check's, as before ([`Controller::check_spaces`]).
    pub(super) fn looks_tick(&mut self, ob: &Observer, now: Instant) {
        server_notices::trust_tick(now);
        if self.looks_public() {
            self.looks_watch_dock();
        }
        self.looks_refocus_due(ob, now);
        if !self.looks_showing() && self.looks.refocus.is_none() && self.looks.close.stale(now) {
            self.looks.close.over();
        }
        self.looks_publish(now);
    }

    /// The Dock's notices observed, once per Dock process ([`Controller::watch_dock`]): its
    /// pid from the bar the tick already reads for the inert slivers, so nothing is asked of
    /// LaunchServices here.
    fn looks_watch_dock(&mut self) {
        let Some(pid) = self.dock_bar.as_ref().and_then(|(_, b)| b.pid) else { return };
        self.watch_dock_of(pid);
    }

    /// Mission Control's key tap told what to listen for: wanted while Mission Control shows
    /// (what closes it) or while the public triggers bring the looks (what opens it); armed
    /// (Escape and presses heard) while it shows. Its shortcuts are read only while wanted.
    fn looks_publish(&mut self, now: Instant) {
        let armed = self.looks_showing();
        let wanted = armed || self.looks_public();
        let keys = wanted.then(|| self.looks_keys(now));
        mission_key_tap::publish(wanted, keys, armed);
    }

    /// A switch for `by` is about to be made (an activation's, or a focus's). With `[windows]
    /// mission_control_key_keeps_stage`: one that comes just after the key that closed Mission
    /// Control, with no click since, is macOS's, not a pick
    /// ([`mission_close::verdict`]): nothing switches, and the stage in front's key window is
    /// focused again once Mission Control is gone. That key window's own focus, and its app's
    /// activation, are the focus given back and are never refused. True when the caller makes
    /// no switch.
    pub(super) fn looks_pick(&mut self, by: PickBy, now: Instant) -> bool {
        if !self.store.config().windows.mission_key_keeps_stage {
            return false;
        }
        let watch = self.looks.close;
        if watch == CloseWatch::default() {
            return false;
        }
        let pid = by.pid();
        let front_key = self.mission_front_key();
        if gives_focus_back(by, front_key) {
            return false;
        }
        let own_front = self.model.as_ref().is_some_and(|m| {
            m.windows().any(|w| w.meta.pid == pid && !w.parked && m.stage_of(w.meta.id).is_some_and(|s| m.stage_in_front(s)))
        });
        // The key is stamped by the key tap; the activation or focus at its notice's receipt.
        let at = self.received_at(now);
        match mission_close::verdict(&watch, pid, at, own_front, false) {
            Verdict::Pick => false,
            Verdict::NoPick { own_front } => {
                let first = watch.refused() != Some(pid);
                self.looks.close.refuse(pid);
                self.looks.refocus.get_or_insert(now);
                if first {
                    tracing::info!(
                        pid,
                        ?by,
                        own_front,
                        front_key = ?front_key.map(|(w, _)| w.0),
                        up = self.looks_showing(),
                        "mission-control: closed by its key, no click: the app macOS brings in front is no pick; no switch, the stage in front keeps its focus"
                    );
                } else {
                    tracing::debug!(pid, ?by, "mission-control: the app found no pick at this close: still none");
                }
                true
            }
        }
    }

    /// The stage in front's key window focused again after a close by key, once Mission
    /// Control is gone (or [`mc::PICK_HOLD`] has passed).
    fn looks_refocus_due(&mut self, ob: &Observer, now: Instant) {
        let Some(since) = self.looks.refocus else { return };
        if self.looks_showing() && now.saturating_duration_since(since) < mc::PICK_HOLD {
            return;
        }
        self.looks.refocus = None;
        self.refocus_front_stage(ob, since, now);
    }

    /// `ctl state`'s `mission_control` and `ctl debug-mission-control status` under the
    /// Corner engine: whether it shows, the key tap, the shortcuts read, and what the looks
    /// brought forward found.
    pub(super) fn looks_status(&self) -> Value {
        let l = &self.looks;
        json!({
            "mode": Value::Null,
            "up": self.looks_showing(),
            "out": Vec::<u32>::new(),
            "tap": mission_key_tap::status(),
            "refused": l.close.refused(),
            "keys": l.keys.map(|(k, _)| json!({
                "mission_control": [u64::from(k.mission_control.keycode), u64::from(k.mission_control.enabled)],
                "app_windows": [u64::from(k.app_windows.keycode), u64::from(k.app_windows.enabled)],
            })),
            "looks": {
                "public_triggers": self.looks_public(),
                "made": l.looks,
                "found": l.found,
                "shown_after_its_key": l.by_key,
                "shown_after_the_docks_notice": l.by_dock,
                "shown_after_the_window_servers_notice": l.by_server,
            },
        })
    }
}
