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
//!   arrival's own switch.
//! - **At the focus** ([`ARRANGE_AT_FOCUS`], off until the hands-free Spaces check proves
//!   that macOS carries out moves of windows on a Desktop not shown): the stage is arranged
//!   the moment the focus is held, before the slide.
//!
//! When the Desktop is recognized, the prediction is confirmed (the stage's windows raised,
//! the window focused; nothing moves) or undone ([`crate::engine::model::Model::undo_ahead`]);
//! another focus held instead, the user acting on the Desktop shown, a full-screen Space or
//! no Space change within the hold ([`crate::engine::focus_replay::FOCUS_REPLAY_MAX`]) undo
//! it too. Windows an undo brings back on a Desktop not shown stay in the safety registry
//! until the window server shows them out of the park zone (macOS may not have moved them).
//! The arrival's own switch, when nothing was arranged ahead, is part of the arrival's batch
//! and never animated: the Desktop's slide is the motion (Apple's).

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::{Controller, Unrestored, on_main_screen};
use crate::engine::focus_replay::Wanted;
use crate::engine::fullscreen;
use crate::engine::model::{Action, Ahead, SpaceId, WindowId};
use crate::platform::observe::{Observer, WindowKind};
use crate::platform::safety::ParkedEntry;
use crate::platform::windows;

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

/// When a Desktop was arranged ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Trigger {
    /// At the held focus, the Desktop not shown yet.
    Focus,
    /// At the Space change notice, the window listed on screen.
    Notice,
}

impl Trigger {
    fn name(self) -> &'static str {
        match self {
            Trigger::Focus => "focus",
            Trigger::Notice => "notice",
        }
    }
}

/// A Desktop arranged ahead and not recognized yet.
#[derive(Debug, Clone)]
pub(super) struct AheadSent {
    ahead: Ahead,
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

    /// A focus or activation was just held: an arrangement made for another window is
    /// undone, and with [`ARRANGE_AT_FOCUS`] a held window's Desktop is arranged now
    /// (`may_arrange`: the model is not settling).
    pub(super) fn after_hold(&mut self, ob: &Observer, now: Instant, may_arrange: bool) {
        let held = match self.focus_replay.peek(now) {
            Some(Wanted::Window { id, .. }) => self.model.as_ref().and_then(|m| m.resolve_standard(id)).or(Some(id)),
            _ => None,
        };
        if self.ahead.as_ref().is_some_and(|s| Some(s.ahead.window) != held) {
            self.undo_ahead(ob, "another focus or activation came");
        }
        if may_arrange
            && arrange_at_focus()
            && self.ahead.is_none()
            && let Some(id) = held
        {
            self.arrange_ahead(ob, id, Trigger::Focus, now);
        }
    }

    /// The Space change notice: when the held window is listed on screen (its Desktop is the
    /// one sliding in), its stage is arranged now, before every app's window list is read
    /// again and the Desktop recognized.
    pub(super) fn arrange_at_notice(&mut self, ob: &mut Observer, now: Instant) {
        if self.ahead.is_some() || self.settle.waiting(now) {
            return;
        }
        let Some(Wanted::Window { id, .. }) = self.focus_replay.peek(now) else { return };
        let Some(m) = self.model.as_ref() else { return };
        if !m.waits_on_another_space(id) {
            return;
        }
        let main = *m.main_display();
        let list = windows::list_windows();
        if self.look_for_overview(&list, main.frame, now) || self.overview.holds_arrival(now) {
            return;
        }
        let Some(standard) = self.model.as_ref().and_then(|m| m.resolve_standard(id)) else { return };
        let shown = on_main_screen(&list, main.frame);
        let listed = list.iter().find(|w| w.id == standard.0 && w.layer == 0);
        match listed {
            Some(w) if shown.contains(&w.id) && !fullscreen::covers_display(w.bounds, main.frame) => {
                self.arrange_ahead(ob, standard, Trigger::Notice, now);
            }
            _ => tracing::debug!(id = standard.0, "the held window is not on screen at the Space change notice; its Desktop is arranged on arrival"),
        }
    }

    /// Arranges window `id`'s Desktop for it ([`crate::engine::model::Model::arrange_ahead`]).
    fn arrange_ahead(&mut self, ob: &Observer, id: WindowId, trigger: Trigger, now: Instant) {
        let focus_at = self.focus_replay.came(now).unwrap_or(now);
        let Some(m) = self.model.as_mut() else { return };
        let Some((ahead, actions)) = m.arrange_ahead(id) else {
            tracing::debug!(id = id.0, trigger = trigger.name(), "nothing to arrange ahead for the held window");
            return;
        };
        let moves = actions.iter().filter(|a| !matches!(a, Action::StateChanged)).count();
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
        self.ahead = Some(AheadSent { ahead, trigger, focus_at, sent_at: sent });
    }

    /// A Desktop was recognized (`arrived`): an arrangement made ahead is confirmed (its
    /// windows raised, the window focused) or undone. The actions are for the arrival's
    /// batch; an undo's restores stay registered (`Unrestored`) until seen.
    pub(super) fn settle_ahead(&mut self, ob: &Observer, arrived: SpaceId, now: Instant) -> Vec<Action> {
        let Some(sent) = self.ahead.take() else { return Vec::new() };
        let Some(m) = self.model.as_mut() else { return Vec::new() };
        let hit = sent.ahead.space == arrived;
        let actions = if hit { m.finish_ahead(&sent.ahead) } else { m.undo_ahead(&sent.ahead) };
        tracing::info!(
            predicted = sent.ahead.space.0,
            arrived = arrived.0,
            hit,
            trigger = sent.trigger.name(),
            sent_before_ms = ms(sent.sent_at, now),
            since_focus_ms = ms(sent.focus_at, now),
            "arrival: {}",
            if hit { "the Desktop arranged ahead came" } else { "another Desktop came; the arrangement made ahead is undone" }
        );
        if !hit {
            self.keep_registered(ob, &actions);
        }
        actions
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
    /// arranged ahead for it.
    pub(super) fn forget_held(&mut self, ob: &Observer) {
        self.focus_replay.clear();
        if self.ahead.is_some() {
            self.undo_ahead(ob, "the user acted on the Desktop shown");
        }
    }

    /// About every tick: an arrangement made ahead whose focus is no longer held and whose
    /// Desktop never came (no Space change settling) is undone.
    pub(super) fn expire_ahead(&mut self, ob: &Observer, now: Instant) {
        let Some(sent) = &self.ahead else { return };
        if self.space_pending {
            return;
        }
        let window = sent.ahead.window;
        let still = matches!(self.focus_replay.peek(now), Some(Wanted::Window { id, .. })
            if self.model.as_ref().and_then(|m| m.resolve_standard(id)).unwrap_or(id) == window);
        if !still {
            self.undo_ahead(ob, "its Desktop did not come");
        }
    }

    /// The windows an undo brings back on a Desktop not shown stay in the safety registry
    /// until the window server shows them out of the park zone: macOS may leave a Desktop
    /// not shown as it is.
    fn keep_registered(&mut self, ob: &Observer, actions: &[Action]) {
        for a in actions {
            let Action::SetFrame { id, frame } = *a else { continue };
            let Some(w) = ob.window(id.0) else { continue };
            let entry = ParkedEntry { element: w.element.clone(), restore: frame, child: w.kind == WindowKind::Child, fitted: false };
            tracing::debug!(id = id.0, restore = %frame, "brought back on a Desktop not shown: registered until seen there");
            self.unrestored.insert(id.0, Unrestored { entry, tries: 0 });
        }
    }
}
