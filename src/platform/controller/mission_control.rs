//! Mission Control and App Exposé under the Spaces engine, on the WM thread (round
//! spaces-mission-control): while one shows, the hidden stages' windows of the Desktops
//! shown are on their Desktop, parked at their stages' cards (round mc-card-park; the
//! screen's corner when they go home before it shows), so that it lays them out with
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
//!   tick): the hidden windows are put home a round at a time
//!   ([`crate::engine::lend_round`]): every window parked first ([`Controller::mission_park`]:
//!   at its stage's card when it goes home with Mission Control over it, else at the corner;
//!   the apps at once), then all those whose park took put home together. A moment later
//!   the window list is read for what Mission Control did with them, and the log says how
//!   many it laid out. In `"shown"` mode its key already starts the parks; the windows go
//!   home once it shows, and get their frames back when it does not.
//! - **Closed**: they go back into their stage Spaces together, their frames set again, but
//!   one whose stage came in front (picked in Mission Control: macOS activates its app and
//!   the activation's switch shows it), and for a moment those of the app in front.
//!
//! - **Picked** ([`Controller::mission_pick`], round mc-pick): macOS's activation of a
//!   picked window's app (or its focus of the window, the app in front already) comes with
//!   the release that closes Mission Control. The windows put home of the stage its switch
//!   brings in get their own frames back at once (the close carries them there), and the
//!   switch waits until Mission Control's own window is gone
//!   ([`crate::engine::mission_control::Lends::pick_switch`]); then it plays the stage
//!   left's shrink alone, the windows placed counting as shown at their frames.
//!
//! Every line in the log starts with `mission-control:`. The engine counts each window put
//! home as lent and parked, so a hide, a show, a quit's sweep and the guardian bring it
//! back whatever happens here.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Controller;
use crate::engine::mission_control::{self as mc, Edge, Fate, Hint, Keys, Lends, Mode, Mods, Out, PickBy, PickSwitch, Settled, Shortcut};
use crate::engine::card_park;
use crate::engine::geometry::Rect;
use crate::engine::lend_park::Zone;
use crate::engine::lend_round::Left;
use crate::engine::model::{Action, Model, WindowId};
use crate::engine::phase::Hold;
use crate::engine::overview::{self, Overview, Sighting};
use crate::platform::input::{self, InputEvent};
use crate::platform::observe::Observer;
use crate::platform::space_keys::{Expose, SpaceKey};
use crate::platform::window_engine::{LendPark, RoundAsk};
use crate::ui::strip_model;
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

/// A frame as the log writes it: `x,y wxh`.
fn fmt_rect(r: Rect) -> String {
    format!("{:.0},{:.0} {:.0}x{:.0}", r.x, r.y, r.w, r.h)
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
            // "key": the lends start now, before anything shows; "shown": the parks.
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

    /// The fast tick: a look for Mission Control while a hint's watch lasts (or a pick's
    /// switch waits for its close), then whatever is due.
    pub(super) fn mission_fast_tick(&mut self, ob: &mut Observer, now: Instant) {
        if !self.spaces.mission.lends.wants_fast(now) {
            return;
        }
        let look = self.spaces.mission.lends.look_due(now);
        self.mission_step(ob, look, now);
        self.mission_pick_due(ob, now);
    }

    /// The tick: a look for Mission Control (the window list the tick's other duties read),
    /// then whatever is due. With the mode off and nothing out, nothing is read.
    pub(super) fn mission_tick(&mut self, ob: &mut Observer, now: Instant) {
        let mode = self.mission_mode();
        let idle = self.spaces.mission.lends.out().is_empty() && !self.spaces.mission.lends.up();
        if mode == Mode::Off && idle {
            self.spaces.mission.lends.set_mode(mode);
            return;
        }
        self.mission_step(ob, true, now);
        self.mission_pick_due(ob, now);
    }

    /// True when Mission Control's own window (WindowManager's, not the Dock's cover that
    /// stays for its exit) is in `list`, over the main display `main`.
    fn mission_own_listed(list: &[windows::CgWindow], main: Rect) -> bool {
        let sightings = list.iter().map(|w| Sighting { owner: &w.owner, layer: w.layer, bounds: w.bounds, onscreen: w.onscreen });
        overview::overview_in(sightings, main).overview == Some(Overview::MissionControl)
    }

    /// A switch for `by` is about to be made (an activation's, or a focus's), `predict` its
    /// model change: on a copy of the model, so that nothing changes when it waits. While
    /// windows are out for Mission Control (it shows, or closed within
    /// [`mc::PICK_WAIT`]), the stage it brings in may be one picked there: its windows out get
    /// their own frames back now, still lent ([`mc::to_frames`]; Mission Control's close
    /// carries them there), and the switch waits for Mission Control's own window to go
    /// ([`Lends::pick_switch`]). True when it waits: the caller makes no switch now.
    pub(super) fn mission_pick(&mut self, by: PickBy, predict: impl FnOnce(&mut Model) -> Vec<Action>, now: Instant) -> bool {
        if let Some(held) = self.spaces.mission.lends.pick_held()
            && held.pid() != by.pid()
        {
            // Another app activated meanwhile: the pick's switch never plays; its windows go
            // back with the others once Mission Control has closed.
            self.spaces.mission.lends.drop_pick();
            tracing::info!(held = held.pid(), now = by.pid(), "mission-control: another app comes in front before the picked stage's switch: it is dropped");
        }
        if !self.spaces.mission.lends.pick_open(now) {
            return false;
        }
        let Some(mut copy) = self.model.clone() else { return false };
        let arriving: Vec<WindowId> = predict(&mut copy)
            .into_iter()
            .filter_map(|a| match a {
                Action::SetFrame { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        let lends = &self.spaces.mission.lends;
        let new = mc::to_frames(lends.out(), lends.placed_ids(), &arriving);
        let placed_before = arriving.iter().any(|id| lends.is_placed(*id));
        if new.is_empty() && !placed_before {
            return false;
        }
        let up = lends.up();
        let since_close_ms = lends.closing(now).map(ms);
        if !new.is_empty() {
            let started = Instant::now();
            let asked: Vec<(WindowId, i32)> = new.iter().map(|o| (o.id, o.pid)).collect();
            let set = self.engine.frames_back(&asked);
            tracing::info!(
                pid = by.pid(),
                ?by,
                asked = ?asked.iter().map(|(w, _)| w.0).collect::<Vec<_>>(),
                set = ?set.iter().map(|w| w.0).collect::<Vec<_>>(),
                up,
                since_close_ms,
                ms = ms(started.elapsed()),
                "mission-control: picked: the stage's windows out take their own frames now, Mission Control's close carries them there"
            );
            self.spaces.mission.lends.placed(set);
        }
        if !arriving.iter().any(|id| self.spaces.mission.lends.is_placed(*id)) {
            // None took its frame: the switch brings them as ever.
            return false;
        }
        let own = up && self.model.as_ref().is_some_and(|m| Self::mission_own_listed(&windows::on_screen(), m.main_display().frame));
        self.spaces.mission.lends.saw_own(own);
        match self.spaces.mission.lends.pick_switch(by, now) {
            PickSwitch::AfterClose => {
                tracing::info!(pid = by.pid(), hold_ms = ms(mc::PICK_HOLD), "mission-control: the picked stage's switch waits for Mission Control's close: the stage left reaches its frames first");
                true
            }
            PickSwitch::Now => false,
        }
    }

    /// The picked stage's switch, once Mission Control's own window is gone (or its wait is
    /// over), when its app is still in front ([`Lends::pick_due`]); otherwise none, and its
    /// windows go back as any other.
    fn mission_pick_due(&mut self, ob: &mut Observer, now: Instant) {
        let Some(by) = self.spaces.mission.lends.pick_due(now) else { return };
        let front = apps::frontmost_pid();
        if front != Some(by.pid()) {
            tracing::info!(pid = by.pid(), ?front, "mission-control: the picked app is no longer in front: no switch for the pick");
            return;
        }
        tracing::info!(?by, up = self.spaces.mission.lends.up(), "mission-control: Mission Control's own window is gone: the picked stage's switch");
        match by {
            PickBy::App(pid) => self.decide_activation(ob, pid, now, false),
            PickBy::Window(id, pid) => {
                let list = windows::list_windows();
                let obs = self.observe_with(ob, &list);
                if let Some(m) = self.model.as_mut() {
                    let actions = m.window_focused(id, &obs);
                    self.activation = self.activation_for(pid, &actions, ob);
                    self.execute_animated(ob, actions);
                }
            }
        }
    }

    /// True when window `id` is out at its own frame for a pick: its switch plays no motion
    /// for it ([`crate::ui::transition_plan::Shown::Placed`]).
    pub(super) fn mission_placed(&self, id: u32) -> bool {
        self.spaces.mission.lends.is_placed(WindowId(id))
    }

    /// True when window `id` is out for Mission Control (put home, laid out by it).
    pub(super) fn mission_out(&self, id: WindowId) -> bool {
        self.spaces.mission.lends.out().iter().any(|o| o.id == id)
    }

    /// True when hidden windows may be put home now: not while a Space change settles, the
    /// screen is locked, a full-screen app's Space shows, a switch plays or the user holds
    /// the button (a drag into Mission Control).
    fn mission_may_lend(&self) -> bool {
        !self.frozen() && self.mission_unbusy()
    }

    /// No switch plays, none waits, no drag, the button up.
    fn mission_unbusy(&self) -> bool {
        self.phase.switching().is_none() && self.pending.is_none() && self.drag.is_none() && !input::left_button_down()
    }

    /// The hidden windows to put home now ([`mc::scope`]), `showing` Mission Control: the
    /// model's shown Spaces' as a rule; while a Desktop picked inside Mission Control waits
    /// for it to close, the window server's shown Desktops'; `None` while nothing may be lent.
    fn mission_hidden(&self, showing: bool) -> Option<Vec<WindowId>> {
        if !self.mission_unbusy() {
            return None;
        }
        let m = self.model.as_ref()?;
        let skip = self.lent.map(|l| l.window);
        let here = mc::hidden_here(m, |id| Some(id) == skip);
        let at = mc::Moment {
            locked: self.locked,
            fullscreen: self.phase.fullscreen(),
            changing: self.phase.changing(),
            for_overview: self.phase.hold() == Some(Hold::Overview),
            showing,
        };
        let (model, server) = if at.changing {
            let model: Vec<u64> = here.iter().filter_map(|id| self.engine.home_of(*id)).collect();
            (model, self.engine.shown_desktops().unwrap_or_default())
        } else {
            (Vec::new(), Vec::new())
        };
        match mc::scope(at, &model, &server) {
            mc::Scope::Model => Some(here),
            mc::Scope::Nothing => None,
            mc::Scope::Desktops(shown) => {
                let hidden = mc::hidden_on(m, |id| self.engine.home_of(id), &shown, |id| Some(id) == skip);
                tracing::info!(
                    desktops = ?shown,
                    hidden = hidden.len(),
                    "mission-control: a Desktop picked inside Mission Control is not recognized yet: the hidden windows of the Desktop the window server shows are put home"
                );
                Some(hidden)
            }
        }
    }

    /// One step: the window list looked at (`look`), Mission Control's opening and closing
    /// taken, a round planned, a batch of lends or of take backs made, the layout read.
    pub(super) fn mission_step(&mut self, ob: &Observer, look: bool, now: Instant) {
        let mode = self.mission_mode();
        self.spaces.mission.lends.set_mode(mode);
        let Some(main) = self.model.as_ref().map(|m| *m.main_display()) else { return };
        // Nothing is inferred from what a locked screen lists.
        if look && !self.locked {
            let list = windows::wake_on_screen();
            self.look_for_overview(&list, main.frame, now);
            self.spaces.mission.lends.saw_own(Self::mission_own_listed(&list, main.frame));
        }
        let showing = self.overview.showing() == Some(Overview::MissionControl);
        match self.spaces.mission.lends.saw(showing, now) {
            Some(Edge::Shown(hint)) if mode != Mode::Off => {
                tracing::info!(
                    mode = mode.name(),
                    hint = hint.map(|(h, _)| h.name()),
                    after_hint_ms = hint.map(|(_, d)| ms(d)),
                    found_within_ms = self.overview.found_within().map(ms),
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
        if let Some(early) = self.spaces.mission.lends.early_back(now) {
            let restored = self.engine.unpark_round(&early);
            tracing::info!(
                parked = early.len(),
                restored,
                wait_ms = ms(mc::KEY_WAIT),
                "mission-control: no Mission Control to put the windows parked at its key home: their frames are set again, hidden"
            );
        }
        if self.spaces.mission.lends.wanted().is_some()
            && let Some(hidden) = self.mission_hidden(showing)
        {
            self.spaces.mission.lends.plan(hidden, now);
        }
        self.spaces.mission.lends.retries_due(now);
        self.mission_round(ob, Some(mc::ROUND_WAIT));
        self.mission_round_said();
        if self.spaces.mission.lends.measure_due(now) {
            self.mission_measure();
        }
        self.mission_back_round(now);
    }

    /// The round's summary in the log, once its last window is out.
    fn mission_round_said(&mut self) -> Option<mc::RoundDone> {
        let done = self.spaces.mission.lends.round_done(Instant::now())?;
        let first_to_last_ms = done.first_to_last.map(|d| d.as_secs_f64() * 1000.0);
        if done.by == mc::By::KeyPark {
            tracing::info!(
                hidden = done.asked,
                parked = done.parked,
                ms = ms(done.took),
                parks_ms = done.parks_ms,
                park_sum_ms = done.park_ms,
                apps = done.apps,
                "mission-control: parked {} windows at its key in {} ms, hidden until it shows",
                done.parked,
                ms(done.took)
            );
        } else {
            tracing::info!(
                why = done.by.name(),
                hidden = done.asked,
                lent = done.lent,
                ms = ms(done.took),
                parks_ms = done.parks_ms,
                park_sum_ms = done.park_ms,
                move_ms = done.move_ms,
                apps = done.apps,
                first_to_last_ms,
                "mission-control: lent {} windows in {} ms",
                done.lent,
                ms(done.took)
            );
        }
        Some(done)
    }

    /// One round ([`crate::engine::lend_round`]): every window still to put home parked
    /// first, the apps at once, each app's windows in turn while `wait` lasts (those after it
    /// wait for the next round), then all of them put home together, or (parks only, at the
    /// key) left parked and hidden for Mission Control to show. Windows parked at the key
    /// go home with the round without a park. Each window left hidden is said in the log
    /// with why and where it was listed, and a missed park is tried again in a later round
    /// while Mission Control shows ([`mc::retry`]).
    fn mission_round(&mut self, ob: &Observer, wait: Option<Duration>) {
        let ids = self.spaces.mission.lends.take_queue();
        if ids.is_empty() {
            return;
        }
        let parks_only = self.spaces.mission.lends.parks_only();
        let at_cards = self.spaces.mission.lends.park_at_cards();
        let early = if parks_only { Vec::new() } else { self.spaces.mission.lends.take_early() };
        let mut asks = Vec::new();
        let mut parked = Vec::new();
        let mut pids = Vec::new();
        let mut unasked = Vec::new();
        for id in ids {
            let Some(pid) = self.model.as_ref().and_then(|m| m.window(id)).map(|w| w.meta.pid) else { continue };
            pids.push((id, pid));
            if early.iter().any(|(e, _)| *e == id) {
                parked.push(id);
                continue;
            }
            // Never at its frame: a window whose park cannot be asked or does not take
            // stays hidden.
            match self.mission_park(ob, id, at_cards) {
                Some(park) => asks.push(RoundAsk { id, pid, park }),
                // No element to park it through: said with the others left hidden.
                None => unasked.push((id, Left::NotParked)),
            }
        }
        // A test's round waits as long as it takes.
        let deadline = Instant::now() + wait.unwrap_or(Duration::from_secs(5));
        let parks = self.engine.park_round(asks, deadline);
        self.spaces.mission.lends.parks_made(parks.ms, parks.sum_ms, parks.apps);
        if !parks.late.is_empty() {
            tracing::debug!(late = ?parks.late.iter().map(|w| w.0).collect::<Vec<_>>(), "mission-control: windows whose park's turn came after the round's wait: the next round's");
            self.spaces.mission.lends.requeue(parks.late.clone());
        }
        let pid_of = |id: crate::engine::model::WindowId| pids.iter().find(|(w, _)| *w == id).map(|(_, p)| *p);
        if parks_only {
            let early: Vec<_> = parks.took.iter().chain(&parks.missed).filter_map(|id| Some((*id, pid_of(*id)?))).collect();
            self.spaces.mission.lends.parked_early(early);
            return;
        }
        parked.extend(parks.took.iter().chain(&parks.missed).copied());
        let homes = self.engine.home_round(&parked);
        self.spaces.mission.lends.homes_made(homes.ms, homes.calls);
        for (id, at, at_card) in homes.lent {
            if let Some(pid) = pid_of(id) {
                self.spaces.mission.lends.lent(Out { id, pid, parked: at, at_card });
            }
        }
        let now = Instant::now();
        let landed = |id: WindowId| parks.landed.iter().find(|(w, _)| *w == id).and_then(|(_, r)| *r);
        for (id, why) in homes.left.iter().chain(&parks.skipped).chain(&unasked) {
            let (misses, again) = self.spaces.mission.lends.left(*id, *why, now);
            let listed = match why {
                Left::ParkMissed { listed, .. } => *listed,
                _ => None,
            };
            let model = self.model.as_ref().and_then(|m| m.window(*id)).map(|w| w.restore);
            tracing::info!(
                window = id.0,
                why = why.name(),
                after_park = ?landed(*id).map(fmt_rect),
                listed = ?listed.map(fmt_rect),
                frame = ?model.map(fmt_rect),
                misses,
                again,
                retry_ms = again.then(|| ms(mc::RETRY_AFTER)),
                "mission-control: a hidden window left hidden: {}{}",
                why.name(),
                if again { "; tried again" } else { "" }
            );
        }
        // Parked at the key and out of this round (shown since, gone): their frames again.
        let stray: Vec<_> = early.into_iter().filter(|(id, _)| !parked.contains(id)).collect();
        if !stray.is_empty() {
            self.engine.unpark_round(&stray);
        }
    }

    /// The park of window `id` lent to Mission Control: at its stage's card when `at_cards`
    /// ([`mc::Lends::park_at_cards`]) and its card is known and holds it
    /// ([`card_park::at_card`]), else at the corner ([`Controller::lend_park`]).
    fn mission_park(&self, ob: &Observer, id: WindowId, at_cards: bool) -> Option<LendPark> {
        let mut park = self.lend_park(ob, id, false, Some(mc::PARK_SECONDS))?;
        if at_cards {
            match self.card_park_of(id, park.ask.frame) {
                Some(at) => {
                    park.ask.at = at;
                    park.ask.zone = Zone::Card;
                }
                None => tracing::debug!(window = id.0, "mission-control: no card to park the window at (its strip out of sight, or the window too big): the corner"),
            }
        }
        Some(park)
    }

    /// Where window `id` at `frame` parks at its stage's card: the card its display's strip
    /// last laid out ([`strip_model::published_cards_of`]: none while the strip is out of
    /// sight), the window centered on it inside the display's visible frame.
    fn card_park_of(&self, id: WindowId, frame: Rect) -> Option<Rect> {
        let m = self.model.as_ref()?;
        let w = m.window(id)?;
        let stage = m.stage_of(id)?;
        let d = m.display(w.meta.display);
        let key = if d.id == m.main_display().id { strip_model::MAIN_STRIP } else { d.id.0 };
        let card = strip_model::published_cards_of(key).into_iter().find(|(s, _)| *s == stage.0).map(|(_, r)| r)?;
        card_park::at_card(frame, card, d.visible)
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
    /// ([`mc::fate`]), those going back together ([`crate::engine::lend_round::back_all`]);
    /// the summary once none is out.
    fn mission_back_round(&mut self, now: Instant) {
        let Some(since) = self.spaces.mission.lends.closing(now) else { return };
        let front = apps::frontmost_pid();
        let mut going = Vec::new();
        for out in self.spaces.mission.lends.out().to_vec() {
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
                    going.push((out.id, out.pid));
                }
            }
        }
        if !going.is_empty() {
            let started = Instant::now();
            let backs = self.engine.back_round(&going);
            tracing::debug!(windows = going.len(), ms = ms(started.elapsed()), "mission-control: windows put home go back together");
            for (id, back) in backs {
                tracing::debug!(window = id.0, back = back.back, restored = ?back.restored, "mission-control: a window put home goes back");
                self.spaces.mission.lends.settle(id, if back.back { Settled::Back } else { Settled::Missed });
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
            "out": l.out().iter().map(|o| json!({ "id": o.id.0, "pid": o.pid, "parked": o.parked.map(|r| [r.x, r.y, r.w, r.h]), "at_card": o.at_card })).collect::<Vec<_>>(),
            "keys": self.spaces.mission.keys.map(|(k, _)| json!({
                "mission_control": [u64::from(k.mission_control.keycode), u64::from(k.mission_control.enabled)],
                "app_windows": [u64::from(k.app_windows.keycode), u64::from(k.app_windows.enabled)],
            })),
        })
    }

    /// `ctl debug-mission-control …` (a test's, `STAGE_MANAGER_TEST=1`): `status`; `mode
    /// off|shown|key|config` sets the mode for this run; `lend` puts every hidden window of
    /// the Desktops shown home now, as Mission Control's lend does, and keeps them out until
    /// `back` takes them back (the probe's rounds: lent before the key, and after it shows);
    /// parked at the corner, or with `lend card` at their cards, as once Mission Control
    /// shows (in view with none over them: the `mission` phase's kill -9).
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
            ["lend"] | ["lend", "card"] => {
                let at_cards = words.len() == 2;
                if self.model.as_ref().is_none_or(|m| m.macos_counts_hidden()) {
                    return json!({ "ok": false, "error": "only under the Spaces engine: the Corner engine's hidden windows are on their Desktop already" });
                }
                if !self.mission_may_lend() {
                    return json!({ "ok": false, "error": "not now: a Space change settles, a switch plays, or the button is down" });
                }
                let skip = self.lent.map(|l| l.window);
                let hidden = self.model.as_ref().map(|m| mc::hidden_here(m, |id| Some(id) == skip)).unwrap_or_default();
                self.spaces.mission.lends.ask(at_cards);
                self.spaces.mission.lends.plan(hidden.clone(), now);
                self.mission_round(ob, None);
                let done = self.mission_round_said();
                let took = done.map_or(0, |d| ms(d.took));
                tracing::info!(hidden = hidden.len(), lent = done.map_or(0, |d| d.lent), ms = took, park_ms = done.map_or(0, |d| d.park_ms), "mission-control: lent for a test");
                json!({
                    "ok": true,
                    "hidden": hidden.iter().map(|w| w.0).collect::<Vec<_>>(),
                    "lent": self.spaces.mission.lends.out().iter().map(|o| o.id.0).collect::<Vec<_>>(),
                    "at_card": self.spaces.mission.lends.out().iter().filter(|o| o.at_card).map(|o| o.id.0).collect::<Vec<_>>(),
                    "ms": took,
                    "park_ms": done.map_or(0, |d| d.park_ms),
                })
            }
            ["back"] => {
                let out: Vec<u32> = self.spaces.mission.lends.out().iter().map(|o| o.id.0).collect();
                self.spaces.mission.lends.ask_back(now);
                self.mission_back_round(now);
                let left: Vec<u32> = self.spaces.mission.lends.out().iter().map(|o| o.id.0).collect();
                json!({ "ok": true, "were_out": out, "still_out": left })
            }
            _ => json!({ "ok": false, "error": "debug-mission-control takes status, lend [card], back, or mode off|shown|key|config" }),
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
