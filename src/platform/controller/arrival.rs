//! Arriving on a Desktop for a window: the Desktop arranged ahead of its recognition.
//!
//! A Dock click, ⌘Tab or an activation that lands on a window waiting in an inactive stage
//! of another Desktop makes macOS slide that Desktop in; Apple's Stage Manager slides it in
//! with that window's stage already active. The owner's log of 10-13 (VS Code with a window
//! on each Desktop): the window's focus at +0 ms (held: its Desktop is not the one shown,
//! [`crate::engine::focus_replay`]), the Space change notice at +4 ms, the Desktop
//! recognized at +64 ms (every app's window list read again first), the switch sent at
//! +80–86 ms, after the slide had begun: the Desktop came in with its previous stage and
//! switched once it showed.
//!
//! So the switch goes out as early as the facts allow ([`crate::engine::model::Model::arrange_ahead`]):
//!
//! - **At the notice** (always): the window server already lists the held window on screen,
//!   so its Desktop is the one sliding in. Its stage is arranged, and its strip redrawn in
//!   its final order, before the window lists are read again: ~60–80 ms earlier than the
//!   arrival's own switch. With no focus held yet (its event comes after the notice: the
//!   WM thread handles the notice through to the recognition before it sees that event),
//!   the frontmost app's focused window is asked for at once, with a short Accessibility
//!   timeout; one waiting on the Desktop sliding in is held and arranged for the same way.
//! - **While a Space change settles** (a full-screen animation, an overview closing): a
//!   focus held then is arranged for as soon as the window server lists its window, unless
//!   a window covers the display or an overview shows.
//! - **At the focus** ([`ARRANGE_AT_FOCUS`], off until the hands-free Spaces check proves
//!   that macOS carries out moves of windows on a Desktop not shown): the stage is arranged
//!   the moment the focus is held, before the slide.
//!
//! When the Desktop is recognized, the prediction is confirmed (the stage's windows raised,
//! the window focused; nothing moves) or undone ([`crate::engine::model::Model::undo_ahead`]);
//! another focus held instead, the user acting on the Desktop shown, a full-screen Space or
//! no Space change within the hold ([`crate::engine::focus_replay::FOCUS_REPLAY_MAX`]) undo
//! it too; a focus held meanwhile undoes it only when it names another known window, or
//! another app (an unknown window of the same app, a palette opening, does not). Every
//! window an arrangement or an undo brings back stays in the safety registry until the
//! window server shows it out of the park zone (macOS may not move a window of a Desktop
//! not shown). At a confirmed arrival the predicted window counts as the focused one: an
//! Accessibility answer still naming the previous app's window never switches back.
//! The arrival's own switch, when nothing was arranged ahead, is part of the arrival's batch
//! and never animated: the Desktop's slide is the motion (Apple's).

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::{Controller, Unrestored, on_main_screen};
use crate::engine::focus_replay::Wanted;
use crate::engine::fullscreen;
use crate::engine::geometry::Rect;
use crate::engine::model::{Action, Ahead, Observation, SpaceId, WindowId};
use crate::platform::ax::AxElement;
use crate::platform::observe::{Observer, WindowKind};
use crate::platform::safety::ParkedEntry;
use crate::platform::windows::{self, CgWindow};

/// Arrange the Desktop at the held focus itself, before macOS shows it (round 6T step 3).
/// Off until the hands-free Spaces check (`scripts/test-stages.sh spaces --auto`, its probe
/// of a window frame set on a Desktop not shown) proves macOS carries such moves out; the
/// arrangement at the Space change notice does not depend on it.
/// `STAGE_MANAGER_ARRANGE_AT_FOCUS=1` (or `0`) overrides it for a run.
pub const ARRANGE_AT_FOCUS: bool = false;

/// [`ARRANGE_AT_FOCUS`], or the environment's word for this run.
fn arrange_at_focus() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| match std::env::var("STAGE_MANAGER_ARRANGE_AT_FOCUS").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => ARRANGE_AT_FOCUS,
    })
}

/// The Accessibility timeout for asking the frontmost app its focused window at the
/// notice: an app busy with the Space change must not hold the arrival up.
const NOTICE_AX_TIMEOUT_SECONDS: f32 = 0.05;

/// When a Desktop was arranged ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Trigger {
    /// At the held focus, the Desktop not shown yet.
    Focus,
    /// At the Space change notice, for the focus held before it.
    Notice,
    /// At the Space change notice, for the frontmost app's focused window (its focus event
    /// not seen yet).
    NoticeAx,
    /// While the Space change settles, for a focus held meanwhile.
    Settle,
}

impl Trigger {
    fn name(self) -> &'static str {
        match self {
            Trigger::Focus => "focus",
            Trigger::Notice => "notice",
            Trigger::NoticeAx => "notice-ax",
            Trigger::Settle => "settle",
        }
    }
}

/// A Desktop arranged ahead and not recognized yet.
#[derive(Debug, Clone)]
pub(super) struct AheadSent {
    ahead: Ahead,
    /// The window's app.
    pid: i32,
    trigger: Trigger,
    /// When the focus it was arranged for came.
    focus_at: Instant,
    /// When the arrangement went out.
    sent_at: Instant,
}

/// Milliseconds from `then` to `now`.
fn ms(then: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(then).as_millis() as u64
}

/// The latest user input this recent counts as what started an arrival (a Dock click's
/// mouse-up, ⌘Tab's key): its timing is logged from there.
const INPUT_RECENT: Duration = Duration::from_secs(3);

/// True when window `id` is listed on the main screen (`shown_main`: [`on_main_screen`])
/// at layer 0 and does not cover the display (`main`): its Desktop is the one shown or
/// sliding in, not a full-screen Space.
fn listed_for_arrival(list: &[CgWindow], id: u32, shown_main: &HashSet<u32>, main: Rect) -> bool {
    list.iter().any(|w| w.id == id && w.layer == 0 && shown_main.contains(&w.id) && !fullscreen::covers_display(w.bounds, main))
}

/// True when a window on screen covers the whole display: a full-screen window, or the
/// stand-in macOS shows while a window enters or leaves full screen.
fn display_covered(list: &[CgWindow], main: Rect) -> bool {
    list.iter().any(|w| w.onscreen && w.layer == 0 && fullscreen::covers_display(w.bounds, main))
}

/// The app of the frontmost window on screen (layer 0) that is not ours (`own`).
fn front_pid(list: &[CgWindow], own: i32) -> Option<i32> {
    list.iter().find(|w| w.onscreen && w.layer == 0 && w.pid != own).map(|w| w.pid)
}

/// True when `held` (what is held now) no longer says the user goes to `window` (of app
/// `pid`): nothing held, another app's activation, or a focus of another known window
/// (`resolve` maps a window to the standard window it is or follows; `None`: unknown). A
/// focus of an unknown window of the same app (a palette, a window not placed yet) does not.
fn contradicts(held: Option<Wanted>, window: WindowId, pid: i32, resolve: impl Fn(WindowId) -> Option<WindowId>) -> bool {
    match held {
        None => true,
        Some(Wanted::App(p)) => p != pid,
        Some(Wanted::Window { id, pid: p }) => match resolve(id) {
            Some(standard) => standard != window,
            None => p != pid,
        },
    }
}

/// The window list's order of the managed windows on screen, for saving the stage left at
/// the notice (no frames: the model keeps those from move events; no focus: the front
/// window becomes the stage's key).
fn listed_order(list: &[CgWindow], managed: impl Fn(WindowId) -> bool) -> Observation {
    let z_order = list.iter().filter(|w| w.onscreen && w.layer == 0).map(|w| WindowId(w.id)).filter(|id| managed(*id)).collect();
    Observation { frames: Default::default(), z_order, focused: None }
}

impl Controller {
    /// Milliseconds since the user's latest input, when it is recent (a Dock click, a key).
    pub(super) fn since_input_ms(&self, now: Instant) -> Option<u64> {
        self.triggers.last_at().filter(|t| now.saturating_duration_since(*t) < INPUT_RECENT).map(|t| ms(t, now))
    }

    /// The Desktop arranged ahead, if any.
    pub(super) fn ahead_space(&self) -> Option<SpaceId> {
        self.ahead.as_ref().map(|s| s.ahead.space)
    }

    /// The Desktop window `id` lives on, when the model knows it.
    pub(super) fn predicted_space(&self, id: u32) -> Option<u64> {
        self.model.as_ref().and_then(|m| m.space_of(WindowId(id))).map(|s| s.0)
    }

    /// True when what is held now no longer says the user goes to the window arranged for.
    fn ahead_contradicted(&self, now: Instant) -> bool {
        let Some(sent) = &self.ahead else { return false };
        let m = self.model.as_ref();
        contradicts(self.focus_replay.peek(now), sent.ahead.window, sent.pid, |id| m.and_then(|m| m.resolve_standard(id)))
    }

    /// A focus or activation was just held: an arrangement it contradicts is undone; a held
    /// window's Desktop is arranged now while a Space change settles (as soon as the window
    /// server lists the window), or with [`ARRANGE_AT_FOCUS`] at once (`may_arrange`: the
    /// window waits on a Desktop not shown, nothing settling).
    pub(super) fn after_hold(&mut self, ob: &Observer, now: Instant, may_arrange: bool) {
        if self.ahead_contradicted(now) {
            self.undo_ahead(ob, "another focus or activation came");
        }
        if self.ahead.is_some() {
            return;
        }
        let Some(Wanted::Window { id, .. }) = self.focus_replay.peek(now) else { return };
        if self.space_pending {
            let Some(main) = self.model.as_ref().map(|m| *m.main_display()) else { return };
            let list = windows::list_windows();
            if self.overview.showing().is_some() || self.overview.holds_arrival(now) {
                return;
            }
            if self.arrange_if_listed(ob, &list, main.frame, id, Trigger::Settle, now) {
                // The Desktop sliding in shows its strip arranged too ([`Self::strip_space`]).
                self.present();
            }
        } else if may_arrange && arrange_at_focus() {
            let Some(standard) = self.model.as_ref().and_then(|m| m.resolve_standard(id)) else { return };
            self.arrange_ahead(ob, standard, Trigger::Focus, now, None);
        }
    }

    /// The Space change notice. An arrangement made at the focus whose window is not on
    /// screen now (the user went elsewhere) is undone. Else, when the held window, or with
    /// none held the frontmost app's focused window, waits on the Desktop the window server
    /// lists now (the one sliding in), its stage is arranged now, before every app's window
    /// list is read again and the Desktop recognized.
    pub(super) fn arrange_at_notice(&mut self, ob: &mut Observer, now: Instant) {
        let Some(main) = self.model.as_ref().map(|m| *m.main_display()) else { return };
        let list = windows::list_windows();
        if self.look_for_overview(&list, main.frame, now) || self.overview.holds_arrival(now) {
            return;
        }
        if let Some(sent) = &self.ahead {
            let shown = on_main_screen(&list, main.frame);
            if sent.trigger == Trigger::Focus && !listed_for_arrival(&list, sent.ahead.window.0, &shown, main.frame) {
                self.undo_ahead(ob, "its window is not on screen at the Space change notice");
            }
            return;
        }
        match self.focus_replay.peek(now) {
            Some(Wanted::Window { id, .. }) => {
                if !self.arrange_if_listed(ob, &list, main.frame, id, Trigger::Notice, now) {
                    tracing::debug!(id = id.0, "the held window is not on screen at the Space change notice; its Desktop is arranged on arrival");
                }
            }
            held => {
                // Its focus may not have reached us yet: the app activated, else the
                // frontmost one, is asked now.
                let pid = match held {
                    Some(Wanted::App(pid)) => Some(pid),
                    _ => front_pid(&list, std::process::id() as i32),
                };
                let Some(pid) = pid else { return };
                let app = AxElement::application(pid);
                let _ = app.set_messaging_timeout(NOTICE_AX_TIMEOUT_SECONDS);
                let Some(id) = app.focused_window().ok().and_then(|w| w.window_id().ok()).map(WindowId) else { return };
                if self.arrange_if_listed(ob, &list, main.frame, id, Trigger::NoticeAx, now) {
                    // Held as its focus event would have been (replayed on arrival).
                    self.focus_replay.hold(Wanted::Window { id, pid }, now);
                }
            }
        }
    }

    /// Arranges window `id`'s Desktop when the window waits on a Desktop not shown (for the
    /// model) and the window server lists it on the main screen with nothing covering the
    /// display; the stage left is saved in the list's order. True when arranged.
    fn arrange_if_listed(&mut self, ob: &Observer, list: &[CgWindow], main: Rect, id: WindowId, trigger: Trigger, now: Instant) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        if !m.waits_on_another_space(id) {
            return false;
        }
        let Some(standard) = m.resolve_standard(id) else { return false };
        let shown = on_main_screen(list, main);
        if !listed_for_arrival(list, standard.0, &shown, main) || display_covered(list, main) {
            return false;
        }
        let obs = listed_order(list, |w| m.window(w).is_some());
        self.arrange_ahead(ob, standard, trigger, now, Some(obs))
    }

    /// Arranges window `id`'s Desktop for it ([`crate::engine::model::Model::arrange_ahead`]);
    /// at the focus (a Desktop not shown) the windows it brings back stay registered until
    /// seen out of the park zone. True when arranged.
    fn arrange_ahead(&mut self, ob: &Observer, id: WindowId, trigger: Trigger, now: Instant, obs: Option<Observation>) -> bool {
        let focus_at = self.focus_replay.came(now).unwrap_or(now);
        let Some(m) = self.model.as_mut() else { return false };
        let pid = m.window(id).map(|w| w.meta.pid).unwrap_or_default();
        let Some((ahead, actions)) = m.arrange_ahead(id, obs.as_ref()) else {
            tracing::debug!(id = id.0, trigger = trigger.name(), "nothing to arrange ahead for the held window");
            return false;
        };
        let moves = actions.iter().filter(|a| !matches!(a, Action::StateChanged)).count();
        if trigger == Trigger::Focus {
            self.keep_registered(ob, &actions);
        }
        // The strip shows the Desktop being left until the arrival: its snapshot can wait.
        let deferred = std::mem::replace(&mut self.defer_present, true);
        self.execute(ob, actions);
        self.defer_present = deferred;
        let sent = Instant::now();
        tracing::info!(
            window = id.0,
            predicted = ahead.space.0,
            stage = ahead.stage.0,
            left = ahead.previous.0,
            trigger = trigger.name(),
            moves,
            since_focus_ms = ms(focus_at, sent),
            since_input_ms = self.since_input_ms(sent),
            "arrival: the Desktop arranged ahead for the window it is shown for"
        );
        self.ahead = Some(AheadSent { ahead, pid, trigger, focus_at, sent_at: sent });
        true
    }

    /// A Desktop was recognized (`arrived`): an arrangement made ahead is confirmed (its
    /// windows raised, the window focused; the stage that was there back if the window is
    /// gone) or undone. The actions are for the arrival's batch, with the window the arrival
    /// counts as focused (the predicted one, on a hit: AX may still name the previous
    /// app's); an undo's restores (a Desktop not shown) stay registered until seen.
    pub(super) fn settle_ahead(&mut self, ob: &Observer, arrived: SpaceId, now: Instant) -> (Vec<Action>, Option<WindowId>) {
        let Some(sent) = self.ahead.take() else { return (Vec::new(), None) };
        let Some(m) = self.model.as_mut() else { return (Vec::new(), None) };
        let hit = sent.ahead.space == arrived;
        let present = m.stage_of(sent.ahead.window) == Some(sent.ahead.stage);
        let actions = if hit { m.finish_ahead(&sent.ahead) } else { m.undo_ahead(&sent.ahead) };
        tracing::info!(
            predicted = sent.ahead.space.0,
            arrived = arrived.0,
            hit,
            window_still_there = present,
            trigger = sent.trigger.name(),
            sent_before_ms = ms(sent.sent_at, now),
            since_focus_ms = ms(sent.focus_at, now),
            "arrival: {}",
            match (hit, present) {
                (true, true) => "the Desktop arranged ahead came",
                (true, false) => "the Desktop arranged ahead came without its window; the stage that was there comes back",
                (false, _) => "another Desktop came; the arrangement made ahead is undone",
            }
        );
        if !hit {
            self.keep_registered(ob, &actions);
        }
        (actions, (hit && present).then_some(sent.ahead.window))
    }

    /// Undoes an arrangement made ahead (`why`): the prediction no longer holds.
    pub(super) fn undo_ahead(&mut self, ob: &Observer, why: &str) {
        let Some(sent) = self.ahead.take() else { return };
        let Some(m) = self.model.as_mut() else { return };
        let actions = m.undo_ahead(&sent.ahead);
        tracing::info!(
            predicted = sent.ahead.space.0,
            window = sent.ahead.window.0,
            trigger = sent.trigger.name(),
            since_sent_ms = ms(sent.sent_at, Instant::now()),
            moves = actions.iter().filter(|a| !matches!(a, Action::StateChanged)).count(),
            "arrival: the arrangement made ahead is undone ({why})"
        );
        self.keep_registered(ob, &actions);
        self.execute(ob, actions);
    }

    /// Forgets a held focus (the user acted on the Desktop shown) and undoes what was
    /// arranged ahead for it. While a Space change settles nobody acts on the Desktop shown
    /// (stage commands are refused then): the arrangement stays.
    pub(super) fn forget_held(&mut self, ob: &Observer) {
        if self.space_pending {
            return;
        }
        self.focus_replay.clear();
        if self.ahead.is_some() {
            self.undo_ahead(ob, "the user acted on the Desktop shown");
        }
    }

    /// About every tick: an arrangement made ahead whose focus is no longer held (or what is
    /// held says otherwise) and whose Desktop never came (no Space change settling) is undone.
    pub(super) fn expire_ahead(&mut self, ob: &Observer, now: Instant) {
        if self.ahead.is_some() && !self.space_pending && self.ahead_contradicted(now) {
            self.undo_ahead(ob, "its Desktop did not come");
        }
    }

    /// Windows an arrangement or an undo brings back stay in the safety registry until the
    /// window server shows them out of the park zone: macOS may leave a Desktop not shown as
    /// it is, and the model counts them shown from now on.
    fn keep_registered(&mut self, ob: &Observer, actions: &[Action]) {
        for a in actions {
            let Action::SetFrame { id, frame } = *a else { continue };
            let Some(w) = ob.window(id.0) else { continue };
            let entry = ParkedEntry { element: w.element.clone(), restore: frame, child: w.kind == WindowKind::Child, fitted: false };
            tracing::debug!(id = id.0, restore = %frame, "brought back for an arrival: registered until seen out of the park zone");
            self.unrestored.insert(id.0, Unrestored { entry, tries: 0 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = Rect::new(0.0, 0.0, 1800.0, 1169.0);

    fn win(id: u32, pid: i32, layer: i64, onscreen: bool, bounds: Rect) -> CgWindow {
        CgWindow { id, pid, owner: String::new(), name: None, layer, bounds, onscreen, alpha: 1.0 }
    }

    fn shown(list: &[CgWindow]) -> HashSet<u32> {
        list.iter().filter(|w| w.onscreen && w.layer == 0).map(|w| w.id).collect()
    }

    #[test]
    fn a_window_counts_for_its_arrival_only_listed_on_screen_and_not_covering_the_display() {
        let parked = Rect::new(1780.0, 1150.0, 600.0, 400.0);
        let list = vec![
            win(1, 10, 0, true, parked),
            win(2, 20, 0, false, Rect::new(100.0, 100.0, 600.0, 400.0)),
            win(3, 30, 0, true, MAIN),
            win(4, 40, 3, true, Rect::new(0.0, 0.0, 200.0, 900.0)),
        ];
        let on = shown(&list);
        assert!(listed_for_arrival(&list, 1, &on, MAIN), "a parked sliver on screen: its Desktop slides in");
        assert!(!listed_for_arrival(&list, 2, &on, MAIN), "off screen: another Desktop");
        assert!(!listed_for_arrival(&list, 3, &on, MAIN), "covering the display: a full-screen Space");
        assert!(!listed_for_arrival(&list, 4, &on, MAIN), "not a window's layer");
        assert!(!listed_for_arrival(&list, 9, &on, MAIN), "not listed");
        assert!(display_covered(&list, MAIN), "a full-screen window or stand-in");
        assert!(!display_covered(&list[..2], MAIN));
    }

    #[test]
    fn the_front_app_is_the_frontmost_window_on_screen_not_ours() {
        let r = Rect::new(100.0, 100.0, 600.0, 400.0);
        let list = vec![win(1, 99, 0, true, r), win(2, 5, 3, true, r), win(3, 10, 0, false, r), win(4, 20, 0, true, r)];
        assert_eq!(front_pid(&list, 99), Some(20), "ours, a panel's layer and off-screen windows skipped");
        assert_eq!(front_pid(&list[..3], 99), None);
    }

    #[test]
    fn only_another_known_window_or_another_app_contradicts_the_prediction() {
        let window = WindowId(1);
        // Window 7 is a dialog of window 1; window 2 another known window of the app.
        let resolve = |id: WindowId| match id.0 {
            1 | 7 => Some(WindowId(1)),
            2 => Some(WindowId(2)),
            _ => None,
        };
        let held = |id: u32, pid: i32| Some(Wanted::Window { id: WindowId(id), pid });
        assert!(!contradicts(held(1, 10), window, 10, resolve), "the window itself");
        assert!(!contradicts(held(7, 10), window, 10, resolve), "its dialog");
        assert!(!contradicts(held(42, 10), window, 10, resolve), "an unknown window of the same app (L1)");
        assert!(!contradicts(Some(Wanted::App(10)), window, 10, resolve), "its app's activation");
        assert!(contradicts(held(2, 10), window, 10, resolve), "another known window");
        assert!(contradicts(held(42, 20), window, 10, resolve), "an unknown window of another app");
        assert!(contradicts(Some(Wanted::App(20)), window, 10, resolve), "another app");
        assert!(contradicts(None, window, 10, resolve), "nothing held any more: the hold expired");
    }

    #[test]
    fn the_stage_left_is_saved_in_the_window_lists_order() {
        let r = Rect::new(100.0, 100.0, 600.0, 400.0);
        let list = vec![win(6, 20, 0, true, r), win(8, 50, 0, true, r), win(2, 20, 0, true, r), win(5, 20, 0, false, r), win(3, 20, 3, true, r)];
        let obs = listed_order(&list, |id| id.0 != 8);
        assert_eq!(obs.z_order, vec![WindowId(6), WindowId(2)], "managed windows on screen, front to back");
        assert_eq!(obs.focused, None, "the front window becomes the key");
        assert!(obs.frames.is_empty());
    }
}
