//! Windows keep the frames set for each screen setup (`[displays] frames_per_setup`): the
//! controller's half, on the tick. The rules are [`crate::engine::setup_frames`]'s; the
//! frames are the model's (`engine/model/setup_frames.rs`).
//!
//! - **A display change** ([`Controller::set_displays`]) starts the settle
//!   ([`super::DISPLAY_SETTLE`], from when the change was heard): a snapshot waiting is
//!   dropped, nothing is taken until it settles, and a window the person moves, resizes or
//!   focuses by a click meanwhile is left alone by the put-back ([`Controller::frames_touched`]).
//! - **Once it settles** the windows go back to their frames for the setup, when it was seen
//!   before, and their frames are taken for it at once (macOS's, for a setup not seen
//!   before). Stages are whole by then: the screen gone was adopted at the change, and under
//!   the Spaces engine its hidden windows were kept in their stages.
//! - **Otherwise** a snapshot is taken on each tick and kept after its quiet
//!   ([`crate::engine::setup_frames::QUIET`]), while the screens CoreGraphics lists are the
//!   model's.

use std::time::Instant;

use super::{Controller, DISPLAY_SETTLE};
use crate::engine::geometry::Rect;
use crate::engine::model::WindowId;
use crate::engine::setup_frames::{Recorder, Settled};
use crate::platform::{desktop, input};
use crate::platform::observe::Observer;

impl Controller {
    /// True when `[displays] frames_per_setup` is on.
    fn frames_per_setup(&self) -> bool {
        self.store.config().displays.frames_per_setup
    }

    /// The displays changed, heard at `heard`: the settle of the frames starts again.
    pub(super) fn frames_display_changed(&mut self, heard: Instant) {
        self.setups.display_changed(heard);
    }

    /// The person moved, resized or focused by a click window `id`: left alone by the
    /// put-back of a change settling now.
    pub(super) fn frames_touched(&mut self, id: u32, now: Instant) {
        self.setups.touch(WindowId(id), now, DISPLAY_SETTLE);
    }

    /// The tick's step: the put-back once a change settled, else a snapshot of the frames.
    pub(super) fn frames_tick(&mut self, ob: &Observer, now: Instant) {
        if !self.frames_per_setup() {
            // Off: nothing waits to be put back when it is turned on.
            self.setups = Recorder::default();
            return;
        }
        if self.locked || self.model.is_none() {
            return;
        }
        // A switch, a Desktop change or a drag going on: the put-back and the snapshot wait
        // for the next tick, so no window moves under them.
        if self.phase.switching().is_some() || self.pending.is_some() || self.phase.changing() || input::left_button_down() {
            return;
        }
        if let Some(touched) = self.setups.settled(now, DISPLAY_SETTLE) {
            self.frames_settled(ob, &touched);
            return;
        }
        let known: Vec<Rect> = self.displays.iter().map(|d| d.frame).collect();
        let mut live = desktop::cg_display_bounds();
        if self.main_only() {
            // The other screens are not the model's: the main one alone counts.
            live.truncate(1);
        }
        let steady = screens_steady(&known, &live);
        let Some(m) = self.model.as_ref() else { return };
        let setup = m.setup_key();
        let ready = self.setups.offer(setup.as_ref(), steady, now, DISPLAY_SETTLE, || setup.as_ref().map(|s| m.setup_frames_now(s)).unwrap_or_default());
        if let Some(frames) = ready
            && self.model.as_mut().is_some_and(|m| m.keep_setup_frames(frames))
        {
            self.mark_dirty();
        }
    }

    /// A display change settled: the windows go back to their frames for the setup when it
    /// was seen before (but those in `touched`), and every window's frame is kept for it.
    fn frames_settled(&mut self, ob: &Observer, touched: &std::collections::HashSet<WindowId>) {
        let Some(m) = self.model.as_mut() else { return };
        let (actions, settled) = m.setup_settled(touched);
        match settled {
            Settled::Unknown => tracing::info!("display: a screen of no known identity: no frames kept for this screen setup"),
            Settled::New(screens) => tracing::info!(screens, "display: a new screen setup: windows stay where macOS put them; their frames are kept from now"),
            Settled::Known(screens, note) => tracing::info!(
                screens,
                put_back = note.put_back,
                touched = note.touched,
                elsewhere = note.elsewhere,
                none_kept = note.none_kept,
                "display: a known screen setup: windows put back at their frames"
            ),
        }
        if settled != Settled::Unknown {
            self.mark_dirty();
        }
        if !actions.is_empty() {
            self.execute(ob, actions);
        }
    }
}

/// True when the screens CoreGraphics lists now (`live`, main first) are the ones the model
/// has (`known`, main first), in any order past the main one: a change not heard yet holds the
/// snapshot.
fn screens_steady(known: &[Rect], live: &[Rect]) -> bool {
    known.len() == live.len() && known.first() == live.first() && known.iter().all(|k| live.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screen_listed_but_not_heard_yet_holds_the_snapshot() {
        let main = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let laptop = Rect::new(-1800.0, 271.0, 1800.0, 1169.0);
        assert!(screens_steady(&[main, laptop], &[main, laptop]));
        assert!(!screens_steady(&[main, laptop], &[main]), "a screen gone, not heard yet");
        assert!(!screens_steady(&[main], &[main, laptop]), "a screen come, not heard yet");
        assert!(!screens_steady(&[main, laptop], &[laptop, main]), "the main screen changed");
        let scaled = Rect::new(0.0, 0.0, 1512.0, 982.0);
        assert!(!screens_steady(&[main], &[scaled]), "a resolution changed");
    }
}
