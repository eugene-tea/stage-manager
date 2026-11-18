//! Mission Control and App Exposé under the Spaces engine, on the WM thread (round
//! spaces-mission-control): while one shows, the hidden stages' windows of the Desktops
//! shown are on their Desktop, parked at the screen's corner, so that it lays them out with
//! the stage in front, as Apple's Stage Manager and the Corner engine have it. The decisions
//! are [`crate::engine::mission_control`]'s; here they are carried out.
//!
//! - **A key** ([`Controller::mission_key`]): one of Mission Control's shortcuts heard by the
//!   input monitor (the user's own, read from `com.apple.symbolichotkeys` when a key could be
//!   one and at most every [`KEYS_FRESH`]; never written), or a notice of the Dock's
//!   ([`Controller::mission_dock_notice`]), makes the fast tick look for Mission Control
//!   for a moment. With `[windows] mission_control_stages = "key"` the key also starts the
//!   lends.
//! - **Shown** ([`Controller::mission_step`], the fast tick while there is work and every
//!   tick): the hidden windows are put home in batches, each parked first
//!   ([`Controller::lend_park`], never lent when its park does not take). A moment later the
//!   window list is read for what Mission Control did with them, and the log says how many
//!   it laid out.
//! - **Closed**: each goes back into its stage Space, its frame set again, but one whose
//!   stage came in front (picked in Mission Control: macOS activates its app and the
//!   activation's switch shows it), and for a moment those of the app in front.
//!
//! Every line in the log starts with `mission-control:`. The engine counts each window put
//! home as lent and parked, so a hide, a show, a quit's sweep and the guardian bring it
//! back whatever happens here.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Controller;
use crate::engine::mission_control::{self as mc, Edge, Fate, Hint, Keys, Lends, Mode, Mods, Out, Settled, Shortcut};
use crate::engine::overview::Overview;
use crate::platform::input::{self, InputEvent};
use crate::platform::observe::Observer;
use crate::platform::space_keys::{Expose, SpaceKey};
use crate::platform::{apps, windows};

/// How long Mission Control's shortcuts, read from the user's settings, are trusted.
const KEYS_FRESH: Duration = Duration::from_secs(30);

/// What the controller keeps for Mission Control's lends ([`super::spaces::SpacesUpkeep`]).
#[derive(Debug, Default)]
pub(super) struct MissionWatch {
    /// When the windows are put home and taken back.
    lends: Lends,
    /// Mission Control's shortcuts as last read, and when.
    keys: Option<(Keys, Instant)>,
    /// `ctl debug-mission-control mode …` (a test's): in place of the config's.
    mode: Option<Mode>,
}

/// A shortcut of the settings as the engine compares it.
fn shortcut(key: SpaceKey) -> Shortcut {
    Shortcut { keycode: key.keycode, mods: Mods::from_flags(key.modifiers), enabled: key.enabled }
}

/// Milliseconds of `d`.
fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

impl Controller {
    /// The mode that counts now: a test's, else the config's.
    fn mission_mode(&self) -> Mode {
        self.spaces.mission.mode.unwrap_or(self.store.config().windows.mission_control)
    }

    /// Mission Control's shortcuts, read again when older than [`KEYS_FRESH`].
    fn mission_keys(&mut self, now: Instant) -> Keys {
        if let Some((keys, at)) = self.spaces.mission.keys
            && now.saturating_duration_since(at) < KEYS_FRESH
        {
            return keys;
        }
        let keys = Keys {
            mission_control: shortcut(SpaceKey::read_expose(Expose::MissionControl)),
            app_windows: shortcut(SpaceKey::read_expose(Expose::AppWindows)),
        };
        if self.spaces.mission.keys.map(|(k, _)| k) != Some(keys) {
            tracing::debug!(?keys, "mission-control: its shortcuts as Keyboard Shortcuts has them");
        }
        self.spaces.mission.keys = Some((keys, now));
        keys
    }

    /// A key-down in another app: one of Mission Control's shortcuts is a hint
    /// ([`Lends::hint`]). Typing reads nothing.
    pub(super) fn mission_key(&mut self, ob: &Observer, event: &InputEvent, now: Instant) {
        let Some(code) = event.key else { return };
        let mods = Mods { control: event.control, option: event.option, command: event.cmd };
        if self.mission_mode() == Mode::Off || !mc::may_trigger(code, mods) {
            return;
        }
        let keys = self.mission_keys(now);
        let Some(trigger) = mc::trigger(&keys, code, mods) else { return };
        let mode = self.mission_mode();
        self.spaces.mission.lends.set_mode(mode);
        if self.spaces.mission.lends.hint(Hint::Key(trigger), now) {
            tracing::info!(key = code, mode = mode.name(), "mission-control: trigger seen: {}", trigger.name());
            // "key": the lends start now, before anything shows.
            self.mission_step(ob, false, now);
        }
    }

    /// A notice of the Dock's (it draws Mission Control): a hint, nothing more.
    pub(super) fn mission_dock_notice(&mut self, name: &str, now: Instant) {
        if name != "AXCreated" || self.mission_mode() == Mode::Off {
            return;
        }
        let mode = self.mission_mode();
        self.spaces.mission.lends.set_mode(mode);
        self.spaces.mission.lends.hint(Hint::Dock, now);
    }

    /// True while Mission Control's lends have work for the fast tick.
    pub(super) fn mission_wants_fast(&self, now: Instant) -> bool {
        self.spaces.mission.lends.wants_fast(now)
    }

    /// The fast tick: a look for Mission Control while a hint's watch lasts, then whatever
    /// is due.
    pub(super) fn mission_fast_tick(&mut self, ob: &Observer, now: Instant) {
        if !self.spaces.mission.lends.wants_fast(now) {
            return;
        }
        let look = self.spaces.mission.lends.look_due(now);
        self.mission_step(ob, look, now);
    }

    /// The tick: a look for Mission Control (the window list the tick's other duties read),
    /// then whatever is due. With the mode off and nothing out, nothing is read.
    pub(super) fn mission_tick(&mut self, ob: &Observer, now: Instant) {
        let mode = self.mission_mode();
        let idle = self.spaces.mission.lends.out().is_empty() && !self.spaces.mission.lends.up();
        if mode == Mode::Off && idle {
            self.spaces.mission.lends.set_mode(mode);
            return;
        }
        self.mission_step(ob, true, now);
    }

    /// True when hidden windows may be put home now: not while a Space change settles, the
    /// screen is locked, a full-screen app's Space shows, a switch plays or the user holds
    /// the button (a drag into Mission Control).
    fn mission_may_lend(&self) -> bool {
        !self.frozen() && self.phase.switching().is_none() && self.pending.is_none() && self.drag.is_none() && !input::left_button_down()
    }

    /// One step: the window list looked at (`look`), Mission Control's opening and closing
    /// taken, a round planned, a batch of lends or of take backs made, the layout read.
    pub(super) fn mission_step(&mut self, ob: &Observer, look: bool, now: Instant) {
        let mode = self.mission_mode();
        self.spaces.mission.lends.set_mode(mode);
        let Some(main) = self.model.as_ref().map(|m| *m.main_display()) else { return };
        if look {
            let list = windows::wake_on_screen();
            self.look_for_overview(&list, main.frame, now);
        }
        let showing = self.overview.showing() == Some(Overview::MissionControl);
        match self.spaces.mission.lends.saw(showing, now) {
            Some(Edge::Shown(hint)) if mode != Mode::Off => {
                tracing::info!(
                    mode = mode.name(),
                    hint = hint.map(|(h, _)| h.name()),
                    after_hint_ms = hint.map(|(_, d)| ms(d)),
                    out = self.spaces.mission.lends.out().len(),
                    "mission-control: shown"
                );
            }
            Some(Edge::Closed(up)) if mode != Mode::Off || !self.spaces.mission.lends.out().is_empty() => {
                tracing::info!(up_ms = ms(up), out = self.spaces.mission.lends.out().len(), "mission-control: closed");
            }
            _ => {}
        }
        if self.spaces.mission.lends.guess_over(now) {
            tracing::info!(
                out = self.spaces.mission.lends.out().len(),
                wait_ms = ms(mc::KEY_WAIT),
                "mission-control: no Mission Control showed after its key: the windows put home go back"
            );
        }
        if self.spaces.mission.lends.wanted().is_some() && self.mission_may_lend() {
            let skip = self.lent.map(|l| l.window);
            let hidden = self.model.as_ref().map(|m| mc::hidden_here(m, |id| Some(id) == skip)).unwrap_or_default();
            self.spaces.mission.lends.plan(hidden, now);
        }
        self.mission_lend_batch(ob, Some(mc::BATCH));
        if let Some(done) = self.spaces.mission.lends.round_done(Instant::now()) {
            tracing::info!(
                why = done.by.name(),
                hidden = done.asked,
                lent = done.lent,
                ms = ms(done.took),
                park_ms = done.park_ms,
                "mission-control: lent {} windows in {} ms",
                done.lent,
                ms(done.took)
            );
        }
        if self.spaces.mission.lends.measure_due(now) {
            self.mission_measure();
        }
        self.mission_back_batch(Some(mc::BATCH), now);
    }

    /// Puts the round's next windows home, each parked first, for at most `budget`.
    fn mission_lend_batch(&mut self, ob: &Observer, budget: Option<Duration>) {
        let started = Instant::now();
        while budget.is_none_or(|b| started.elapsed() < b) {
            let Some(id) = self.spaces.mission.lends.next() else { break };
            let Some(pid) = self.model.as_ref().and_then(|m| m.window(id)).map(|w| w.meta.pid) else { continue };
            // Never at its frame: a window whose park cannot be asked or does not take
            // stays hidden.
            let Some(park) = self.lend_park(ob, id, false, Some(mc::PARK_SECONDS)) else {
                tracing::debug!(window = id.0, "mission-control: a hidden window has no element to park it through: left hidden");
                continue;
            };
            let lend = self.engine.lend_home(id, Some(park));
            if lend.home {
                let parked = windows::describe(&[id.0]).first().map(|w| w.bounds);
                self.spaces.mission.lends.lent(Out { id, pid, parked }, lend.park_ms);
            } else {
                tracing::debug!(window = id.0, parked = ?lend.parked, park_ms = lend.park_ms, "mission-control: a hidden window was not put home (its park did not take, or the engine does not hold it): left hidden");
            }
        }
    }

    /// Reads where the window server lists the windows put home, Mission Control showing:
    /// those moved from their park were laid out ([`mc::laid_out`]).
    fn mission_measure(&mut self) {
        self.spaces.mission.lends.measured();
        let out = self.spaces.mission.lends.out().to_vec();
        let ids: Vec<u32> = out.iter().map(|o| o.id.0).collect();
        let listed = windows::describe(&ids);
        let moved = |o: &Out| {
            let now = listed.iter().find(|w| w.id == o.id.0).map(|w| w.bounds)?;
            Some(mc::laid_out(now, o.parked?))
        };
        let laid: Vec<u32> = out.iter().filter(|o| moved(o) == Some(true)).map(|o| o.id.0).collect();
        let left: Vec<u32> = out.iter().filter(|o| moved(o) == Some(false)).map(|o| o.id.0).collect();
        tracing::info!(
            lent = out.len(),
            laid_out = laid.len(),
            at_park = ?left,
            "mission-control: Mission Control lays out {} of {} windows put home",
            laid.len(),
            out.len()
        );
    }

    /// After a close: each window out is left to its switch, waits for one, or goes back
    /// ([`mc::fate`]), for at most `budget`; the summary once none is out.
    fn mission_back_batch(&mut self, budget: Option<Duration>, now: Instant) {
        let Some(since) = self.spaces.mission.lends.closing(now) else { return };
        let started = Instant::now();
        let front = apps::frontmost_pid();
        for out in self.spaces.mission.lends.out().to_vec() {
            if budget.is_some_and(|b| started.elapsed() >= b) {
                break;
            }
            // Shown by the model, its stage in front (the switch shows it), or gone.
            let shown = self.model.as_ref().is_none_or(|m| {
                m.window(out.id).is_none_or(|w| !w.parked) || m.stage_of(out.id).is_some_and(|s| m.stage_in_front(s))
            });
            match mc::fate(shown, front == Some(out.pid), since) {
                Fate::Wait => {}
                Fate::Forget => self.spaces.mission.lends.settle(out.id, Settled::Picked),
                Fate::TakeBack => {
                    if self.lent.is_some_and(|l| l.window == out.id) {
                        // A Dock press or ⌘Tab's switcher has it now: theirs to take back.
                        self.spaces.mission.lends.settle(out.id, Settled::Missed);
                        continue;
                    }
                    let back = self.engine.take_back(out.id);
                    tracing::debug!(window = out.id.0, back = back.back, restored = ?back.restored, "mission-control: a window put home goes back");
                    self.spaces.mission.lends.settle(out.id, if back.back { Settled::Back } else { Settled::Missed });
                }
            }
        }
        if let Some(done) = self.spaces.mission.lends.back_done(Instant::now()) {
            tracing::info!(
                back = done.back,
                picked = ?done.picked.iter().map(|w| w.0).collect::<Vec<_>>(),
                not_back = ?done.missed.iter().map(|w| w.0).collect::<Vec<_>>(),
                ms = ms(done.took),
                "mission-control: put back {} windows; picked: {}",
                done.back,
                if done.picked.is_empty() { "none" } else { "a hidden stage's window, its stage comes in front" }
            );
        }
    }

    /// `ctl state`'s and `ctl debug-mission-control status`'s view.
    pub(super) fn mission_status(&self) -> Value {
        let l = &self.spaces.mission.lends;
        json!({
            "mode": self.mission_mode().name(),
            "test_mode": self.spaces.mission.mode.map(Mode::name),
            "up": l.up(),
            "out": l.out().iter().map(|o| json!({ "id": o.id.0, "pid": o.pid, "parked": o.parked.map(|r| [r.x, r.y, r.w, r.h]) })).collect::<Vec<_>>(),
            "keys": self.spaces.mission.keys.map(|(k, _)| json!({
                "mission_control": [u64::from(k.mission_control.keycode), u64::from(k.mission_control.enabled)],
                "app_windows": [u64::from(k.app_windows.keycode), u64::from(k.app_windows.enabled)],
            })),
        })
    }

    /// `ctl debug-mission-control …` (a test's, `STAGE_MANAGER_TEST=1`): `status`; `mode
    /// off|shown|key|config` sets the mode for this run; `lend` puts every hidden window of
    /// the Desktops shown home now, as Mission Control's lend does, and keeps them out until
    /// `back` takes them back (the probe's rounds: lent before the key, and after it shows).
    pub(super) fn debug_mission_control(&mut self, ob: &Observer, words: &[String]) -> Value {
        let now = Instant::now();
        match words.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["status"] => json!({ "ok": true, "mission_control": self.mission_status() }),
            ["mode", "config"] => {
                self.spaces.mission.mode = None;
                json!({ "ok": true, "mode": self.mission_mode().name() })
            }
            ["mode", word] => match Mode::named(word) {
                Some(mode) => {
                    self.spaces.mission.mode = Some(mode);
                    json!({ "ok": true, "mode": mode.name() })
                }
                None => json!({ "ok": false, "error": "debug-mission-control mode takes off, shown, key or config" }),
            },
            ["lend"] => {
                if self.model.as_ref().is_none_or(|m| m.macos_counts_hidden()) {
                    return json!({ "ok": false, "error": "only under the Spaces engine: the Corner engine's hidden windows are on their Desktop already" });
                }
                if !self.mission_may_lend() {
                    return json!({ "ok": false, "error": "not now: a Space change settles, a switch plays, or the button is down" });
                }
                let skip = self.lent.map(|l| l.window);
                let hidden = self.model.as_ref().map(|m| mc::hidden_here(m, |id| Some(id) == skip)).unwrap_or_default();
                self.spaces.mission.lends.ask();
                self.spaces.mission.lends.plan(hidden.clone(), now);
                self.mission_lend_batch(ob, None);
                let done = self.spaces.mission.lends.round_done(Instant::now());
                let took = done.map_or(0, |d| ms(d.took));
                tracing::info!(hidden = hidden.len(), lent = done.map_or(0, |d| d.lent), ms = took, park_ms = done.map_or(0, |d| d.park_ms), "mission-control: lent for a test");
                json!({
                    "ok": true,
                    "hidden": hidden.iter().map(|w| w.0).collect::<Vec<_>>(),
                    "lent": self.spaces.mission.lends.out().iter().map(|o| o.id.0).collect::<Vec<_>>(),
                    "ms": took,
                    "park_ms": done.map_or(0, |d| d.park_ms),
                })
            }
            ["back"] => {
                let out: Vec<u32> = self.spaces.mission.lends.out().iter().map(|o| o.id.0).collect();
                self.spaces.mission.lends.ask_back(now);
                self.mission_back_batch(None, now);
                let left: Vec<u32> = self.spaces.mission.lends.out().iter().map(|o| o.id.0).collect();
                json!({ "ok": true, "were_out": out, "still_out": left })
            }
            _ => json!({ "ok": false, "error": "debug-mission-control takes status, lend, back, or mode off|shown|key|config" }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::space_keys::DEFAULT_MODIFIERS;

    #[test]
    fn a_shortcut_of_the_settings_keeps_its_key_and_modifiers() {
        // macOS's default for Mission Control, as the settings store it: Control and Fn, ↑.
        let s = shortcut(SpaceKey { keycode: 126, modifiers: DEFAULT_MODIFIERS, enabled: true, from_settings: false });
        assert_eq!(s, Shortcut { keycode: 126, mods: Mods { control: true, option: false, command: false }, enabled: true });
        let keys = Keys { mission_control: s, app_windows: Shortcut { keycode: 125, ..s } };
        assert_eq!(mc::trigger(&keys, 126, Mods { control: true, ..Mods::default() }), Some(mc::Trigger::MissionControl));
        // Turned off in Keyboard Shortcuts.
        let off = shortcut(SpaceKey { keycode: 126, modifiers: DEFAULT_MODIFIERS, enabled: false, from_settings: true });
        assert!(!off.enabled);
    }
}
