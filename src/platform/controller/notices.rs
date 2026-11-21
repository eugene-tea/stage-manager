//! A window's focus and an app's activation: the facts gathered, the verdict, then what it
//! asks ([`crate::engine::attribution`], whose module doc has the gates' one written order).
//!
//! Each arm reads the notice's own facts up front ([`Controller::heard`]), asks the pure
//! verdict, and fills in a later stage of the facts only when the verdict asks for it
//! ([`FocusVerdict::Gather`], [`ActivationVerdict::Gather`]), after doing what the chain
//! does between its gates (a click ends the login's settling, a focus lands the switch in
//! flight). Nothing here decides what a notice is.

use std::time::Instant;

use super::{Controller, DISPLAY_SETTLE, USER_FOCUS, arrival, listed_bounds};
use crate::engine::attribution::{self, ActivationFacts, ActivationStage, ActivationVerdict, AppNow, FocusFacts, FocusStage, FocusVerdict, Heard, Notice, Place, Scene};
use crate::engine::desktop_echo::Told;
use crate::engine::focus_elsewhere::{self, FocusOnSwitch, WentElsewhere};
use crate::engine::focus_replay::Wanted;
use crate::engine::mission_control::PickBy;
use crate::engine::model::{Model, WindowId};
use crate::engine::restore::End;
use crate::engine::slide_hold;
use crate::engine::stale_activation::{self, Stale};
use crate::engine::switch_landing::Landing;
use crate::platform::observe::Observer;
use crate::platform::{apps, clock, enhanced_ui, input, windows};

impl Controller {
    /// What is known of any notice of app `bundle` (`told`: its activation, or a focus of
    /// one of its windows), received at `at`, read now: no read here costs. The desktop
    /// press's echo counts the notice, as the cause ledger does.
    fn heard(&mut self, bundle: Option<&str>, told: Told, at: Instant) -> Heard {
        Heard {
            at,
            now: clock::now(),
            since_press: input::since_last_press(),
            button_down: input::left_button_down(),
            display_changed: self.displays_changed_at,
            display_settle: DISPLAY_SETTLE,
            wallpaper_echo: self.causes.desktop_heard(bundle, told, at),
            settling: self.model.as_ref().is_some_and(Model::settling),
        }
    }

    /// Where window `id` (of app `pid`) stands for a focus that is neither ours nor macOS's
    /// ([`FocusStage::Place`]), and the standard window it stands for (itself, or the one
    /// it is a child of).
    fn focus_place(&self, id: u32, pid: Option<i32>, now: Instant) -> (Place, u32) {
        let unknown = self.model.as_ref().is_some_and(|m| m.window(WindowId(id)).is_none());
        let elsewhere = self.model.as_ref().is_some_and(|m| m.waits_on_another_space(WindowId(id)));
        // Its own screen's answer: a full-screen app on the other screen stops nothing here.
        let frozen = self.frozen_for_window(id);
        // No switch's start known while one is held: any input counts, as before.
        let input_since_switch = self.phase.switching().is_none_or(|s| input::since_last_press() < now.saturating_duration_since(s.since));
        // A window the switch still brings in (or a child of one).
        let standard = self.model.as_ref().and_then(|m| m.resolve_standard(WindowId(id))).map_or(id, |w| w.0);
        let arriving = self.pending.as_ref().is_some_and(|p| p.entries.contains_key(&id) || p.entries.contains_key(&standard));
        // Held off every Desktop (a stage Space): macOS goes nowhere for it by itself.
        let hidden = pid.map(|pid| {
            let since_press = input::since_last_press();
            focus_elsewhere::hidden_focus(focus_elsewhere::FocusFacts {
                elsewhere,
                off_every_desktop: elsewhere && self.engine.off_every_desktop(WindowId(standard)),
                frozen,
                activation_switched: self.activation_switch.as_ref().is_some_and(|a| a.pid == pid && a.holds(since_press, now)),
            })
        });
        let place = Place { unknown, elsewhere, frozen, unplaced: self.unplaced.contains(&id), input_since_switch, arriving, hidden: hidden.unwrap_or_default() };
        (place, standard)
    }

    /// A window's focus notice, received at `at` (module docs).
    pub(super) fn on_focused(&mut self, ob: &mut Observer, id: u32, at: Instant, now: Instant) {
        if self.unplaced.contains(&id) {
            self.offer_waiting(ob, vec![id]);
        }
        let said = |verdict: &'static str| attribution::judged(Notice::Focus, i64::from(id), verdict);
        let pid = ob.window(id).map(|w| w.pid);
        let bundle = pid.and_then(|pid| ob.bundle_id(pid)).map(str::to_owned);
        // 1: the cause ledger (the answer to a focus or raise of ours), or a window an
        // arrangement made ahead just moved (macOS's own focus at the arrival it arranged
        // for: its rules are the arrival's, by the echo window of its moves as before); and
        // the arrangement that may overrule either.
        let echo = self.causes.focus_heard(id, pid, at) || (self.ahead.is_some() && self.causes.wrote_before(id, at));
        let beats_echo = echo && self.focus_beats_echo(ob, id, now);
        let heard = self.heard(bundle.as_deref(), Told::Focus, at);
        let mut facts = FocusFacts {
            heard,
            echo,
            beats_echo,
            pressed_on: self.causes.pressed_on(id, heard.now.checked_sub(heard.since_press)),
            app: pid.map(|pid| AppNow { hidden: apps::is_hidden(pid), active: apps::is_active(pid) }),
            user_focus: USER_FOCUS,
            place: None,
            scene: None,
            slides: None,
            moved_on: None,
        };
        let button_down = facts.heard.button_down;
        let mut verdict = attribution::focus(&facts);
        // What the gates passed leave behind, in the chain's order: past 1, a focus that
        // follows a press is the user's (the press measured to the focus's receipt).
        let users = verdict != FocusVerdict::Ours && attribution::users_focus(&facts);
        if users {
            // Focused by a click or a key press on it: a late title re-matches it no more
            // (a focus an app makes by itself at login leaves it).
            if let Some(m) = self.model.as_mut() {
                m.touch_window(WindowId(id));
            }
            self.frames_touched(id);
        }
        if !matches!(verdict, FocusVerdict::Ours | FocusVerdict::AppBehind | FocusVerdict::AfterDisplayChange) {
            // Past 3: not our echo, the user's (or an app's own), remembered for the reconciler.
            self.causes.focus_not_ours(id, now);
        }
        if !verdict.passed_causes() {
            match verdict {
                FocusVerdict::Ours => tracing::debug!(id, "focus echo of our own action ignored"),
                FocusVerdict::AppBehind => {
                    let app = facts.app.unwrap_or(AppNow { hidden: None, active: None });
                    tracing::debug!(id, ?pid, hidden = ?app.hidden, active = ?app.active, "a focus in an app not in front (hidden, or not the active app): its own, or a late echo of ours; no switch");
                }
                FocusVerdict::AfterDisplayChange => tracing::debug!(id, "a focus macOS made after a display change (no click or key since): no switch"),
                FocusVerdict::WallpaperEcho => tracing::debug!(id, "Finder window focus after a wallpaper click: ignored (the desktop is Finder's)"),
                // At login a focus is the app's (a click or key press would have ended the
                // settling): no switch, nothing held for later.
                _ => tracing::debug!(id, "a focus while the login settles: no switch"),
            }
            said(verdict.name().unwrap_or("dropped"));
            return;
        }
        // Past 5. A click whose press the input monitor has not told us about yet ends the
        // login's settling; then where the window stands.
        if facts.heard.settling {
            self.end_restore(ob, "a click", End::User);
        }
        let (place, standard) = self.focus_place(id, pid, now);
        facts.place = Some(place);
        verdict = attribution::focus(&facts);
        // 6: the switch in flight.
        match attribution::on_switch(&facts).unwrap_or(FocusOnSwitch::Supersede) {
            FocusOnSwitch::LandAsPlanned => {
                // The switch already brings it in front: shown now, the window would sit at
                // its frame under its picture still growing from the card.
                let switch = self.pending.as_ref().map(|p| p.id);
                tracing::debug!(id, ?switch, "a focus on a window the switch brings in: the switch lands as planned, behind its picture");
                said(verdict.name().unwrap_or("on a window the switch brings in"));
                return;
            }
            FocusOnSwitch::Supersede => self.flush_superseded(ob, "a focus", Landing::Superseded),
            FocusOnSwitch::LandWithoutFocus => {
                // The app put its focus on a window of another Desktop (a link opened in its
                // window there, 11-19): macOS goes there, and the switch's focus or a raise
                // of the app's window here would pull it back.
                if let Some(pid) = pid {
                    let space = self.model.as_ref().and_then(|m| m.space_of(WindowId(standard)));
                    self.went_elsewhere = Some(WentElsewhere { pid, window: WindowId(standard), space, at: now });
                    if let Some(p) = self.pending.as_mut() {
                        let pid_of = |w: WindowId| ob.window(w.0).map(|w| w.pid);
                        let (kept, withdrawn) = focus_elsewhere::without_keyboard(std::mem::take(&mut p.actions), pid, pid_of);
                        p.actions = kept;
                        tracing::debug!(id, switch = p.id, dropped = ?withdrawn.focus, raises_dropped = ?withdrawn.raises, "a focus held for its Desktop, no click or key since the switch: the switch lands as planned, without its focus or its raises of the app's windows");
                    }
                }
            }
        }
        self.show_strip_after_desktop("a window was focused");
        match (verdict, pid) {
            (FocusVerdict::Held { hidden, .. }, Some(pid)) => {
                // 7. Its Desktop is not recognized yet (a Dock click or ⌘Tab to a window of
                // another Desktop, often before the Space change notice): replayed once it
                // is, never dropped.
                let predicted = self.predicted_space(id);
                tracing::debug!(id, pid, settling = self.phase.changing(), elsewhere = place.elsewhere, unknown = place.unknown, predicted, "focus held until its Desktop is recognized");
                self.focus_replay.hold(Wanted::Window { id: WindowId(id), pid }, now);
                self.after_hold(ob, now);
                if hidden.arrive {
                    self.arrive_for_hidden_focus(ob, WindowId(standard), pid, hidden.switch_back, now);
                }
                said("held for its Desktop");
                return;
            }
            (FocusVerdict::Frozen, _) => {
                // 8.
                said("nothing while frozen");
                return;
            }
            _ => {}
        }
        // Past 8: what was held is forgotten, then where the window shows.
        self.forget_held(ob);
        if button_down {
            // Clicked or grabbed by the user: a move made ahead for it is no longer sent
            // again under them (R11 3). A focus alone may be macOS's own at an arrival, and
            // keeps it.
            self.user_took(id, "clicked");
        }
        let parked = self.model.as_ref().and_then(|m| m.window(WindowId(id))).is_some_and(|w| w.parked);
        let in_another_stage = self.window_elsewhere(id);
        facts.scene = Some(Scene { parked, in_another_stage });
        verdict = attribution::focus(&facts);
        if verdict == FocusVerdict::Grabbed {
            // 9. Grabbed by its sliver: nothing moves under the user's drag; its stage comes
            // once the button is up (Apple's parked windows cannot be grabbed).
            tracing::debug!(id, "a parked window was grabbed; its stage comes when the button is up");
            self.focus_after_press = Some(id);
            said("a parked window grabbed");
            return;
        }
        // The window list, read once: for a slide (10) and for the switch.
        let list = windows::list_windows();
        if verdict == FocusVerdict::Gather(FocusStage::Slide) {
            let slides = pid.is_some()
                && in_another_stage
                && self.model.as_ref().is_some_and(|m| slide_hold::slide_listed(m, &self.slide_bounds(&listed_bounds(&list))));
            facts.slides = Some(slides);
            verdict = attribution::focus(&facts);
        }
        if let (FocusVerdict::SlideHeld, Some(pid)) = (verdict, pid) {
            // A Desktop slides before its notice: a stage switch here would raise and focus
            // a window of the Desktop being left, and macOS would slide back.
            tracing::info!(id, pid, "focus held: a Desktop slides; decided once the Space is recognized");
            self.hold_for_slide(ob, Wanted::Window { id: WindowId(id), pid }, false, now);
            said("held for a slide");
            return;
        }
        if let (FocusVerdict::Gather(FocusStage::AppFocus), Some(pid)) = (verdict, pid) {
            // 11. A focus that would switch stages, still true? Its notice carries no stamp,
            // so its app is asked (bounded) where its focus is now: a late focus of a window
            // its app has left since never switches (round stale-activations).
            let now_focused = arrival::focused_window_of(pid);
            let stages = self.model.as_ref().map(|m| {
                let stage = |w: WindowId| m.resolve_standard(w).and_then(|s| m.stage_of(s));
                (stage(WindowId(id)), now_focused.and_then(stage))
            });
            facts.moved_on = Some(stages.is_some_and(|(was, is)| stale_activation::focus_moved_on(was, is)));
            verdict = attribution::focus(&facts);
            if verdict == FocusVerdict::Stale {
                tracing::info!(id, pid, now_focused = ?now_focused.map(|w| w.0), reason = "its app's focus is in another stage now", "focus dropped: stale, no switch");
                said("stale");
                return;
            }
        }
        if verdict != FocusVerdict::Acts {
            tracing::error!(id, ?verdict, "a focus's verdict is not decided with every fact gathered; nothing done");
            return;
        }
        // 12. It acts: Shift's bring-here, a pick in Mission Control, its stage.
        let obs = self.observe_with(ob, &list);
        if let Some(pid) = pid
            && in_another_stage
            && self.bring_here_if_shift(ob, pid, &obs, now)
        {
            said("the user's: Shift brings it here");
            return;
        }
        if let Some(pid) = pid
            && self.upkeep_pick(PickBy::Window(WindowId(id), pid), |m| m.window_focused(WindowId(id), &obs), clock::now())
        {
            said("the user's: a pick in Mission Control");
            return;
        }
        if let Some(m) = self.model.as_mut() {
            let actions = m.window_focused(WindowId(id), &obs);
            if !actions.is_empty() {
                tracing::info!(id, "focused window is in another stage; switching");
            }
            said(if actions.is_empty() { "the user's: nothing to switch" } else { "the user's: its stage comes" });
            if let Some(pid) = pid {
                self.activation = self.activation_for(pid, &actions, ob);
            }
            // A switch made by the user's own focus is the user's by its cause.
            let outer = self.users_word;
            self.users_word = outer || users;
            self.execute_animated(ob, actions);
            self.users_word = outer;
        }
    }

    /// An app's activation notice, fired at `fired` and received at `at` (module docs).
    pub(super) fn on_activated(&mut self, ob: &mut Observer, pid: i32, fired: Instant, at: Instant, now: Instant) {
        crate::platform::switcher_probe::note("activated", format!("pid={pid} notice_age_ms={}", fired.elapsed().as_millis()));
        // A window put home for this app (a Dock press, ⌘Tab's switcher) has its activation:
        // kept for its switch.
        self.switcher_activated(pid, fired);
        self.slide_watch_activated(ob, pid);
        // Its enhanced UI is read again at its next move (round enhanced-ui).
        enhanced_ui::forget(pid);
        // Before the echo check: a window of the app that shows now is placed whoever
        // activated it. One still away is left alone ([`super::offered_at_notice`]).
        let waiting: Vec<u32> = self.unplaced.iter().copied().filter(|id| ob.window(*id).is_some_and(|w| w.pid == pid)).collect();
        self.offer_waiting(ob, waiting);
        let said = |verdict: &'static str| attribution::judged(Notice::Activation, i64::from(pid), verdict);
        let bundle = ob.bundle_id(pid).map(str::to_owned);
        let mut facts = ActivationFacts {
            heard: self.heard(bundle.as_deref(), Told::Activation, at),
            pid,
            // 1: the cause ledger (`at` is the notice's `fired`: when the main thread got it).
            echo: self.causes.is_app_echo(pid, at),
            fired,
            our_switch: self.our_switch,
            front: apps::frontmost_pid(),
            still_settling: None,
            frozen: None,
        };
        let mut verdict = attribution::activation(&facts);
        if verdict.passed_staleness() {
            // Past 3. Another app's: a lend for another app is refuted (round r2-lends).
            self.lent_activation_elsewhere(pid, fired);
        }
        if verdict == ActivationVerdict::Gather(ActivationStage::Restore) {
            // 5. A Dock click whose press the input monitor has not told us about yet.
            self.end_restore(ob, "a click (or a Dock click)", End::User);
            facts.still_settling = Some(self.model.as_ref().is_some_and(Model::settling));
            verdict = attribution::activation(&facts);
        }
        if verdict == ActivationVerdict::Gather(ActivationStage::Frozen) {
            // Past 5. The user switched apps: a late echo of an earlier focus of ours is no
            // longer one, and the switch in flight is superseded.
            self.user_switched();
            self.flush_superseded(ob, "an activation", Landing::Superseded);
            self.show_strip_after_desktop("an app was activated");
            facts.frozen = Some(self.activation_frozen(pid));
            verdict = attribution::activation(&facts);
        }
        match verdict {
            ActivationVerdict::Ours => tracing::debug!(pid, "activation echo of our own action ignored"),
            ActivationVerdict::WallpaperEcho => tracing::debug!(pid, "Finder activation after a wallpaper click: ignored (the desktop is Finder's)"),
            ActivationVerdict::Stale(why) => {
                // Late under load: no longer true, or older than a switch we began since
                // (round stale-activations). Not the owner's: nothing below runs for it.
                let age_ms = fired.elapsed().as_millis() as u64;
                let before_switch_ms = match why {
                    Stale::BeforeOurSwitch { by } => Some(by.as_millis() as u64),
                    Stale::NotFrontmost { .. } => None,
                };
                tracing::info!(pid, age_ms, reason = why.reason(), front = ?facts.front, before_switch_ms, "activation dropped: stale, no switch");
            }
            // The lid closed or opened: macOS activates an app of its choosing (11-05
            // 20:25:46.8, Mail, whose switch sent the laptop's front to a card and slid the
            // 5K back to the Desktop it had left).
            ActivationVerdict::AfterDisplayChange => tracing::info!(pid, "an activation macOS made after a display change (no click or key since): no switch"),
            // A launch-time activation at login is the app's, not the user's (or a click the
            // restore window outlives while Accessibility lists no window): no switch,
            // nothing held for later.
            ActivationVerdict::Settling => tracing::debug!(pid, "an activation while the login settles: no switch"),
            ActivationVerdict::Frozen => {
                tracing::debug!(pid, "activation held until the Desktop is recognized");
                self.focus_replay.hold(Wanted::App(pid), now);
                self.after_hold(ob, now);
            }
            ActivationVerdict::Acts => {
                self.decide_activation(ob, pid, now, true);
                // Its switch decided: a window put home for it is the switch's at once.
                self.lent_decided(pid);
                return;
            }
            ActivationVerdict::Gather(stage) => {
                tracing::error!(pid, ?stage, "an activation's verdict is not decided with every fact gathered; nothing done");
                return;
            }
        }
        if let Some(name) = verdict.name() {
            said(name);
        }
    }
}
