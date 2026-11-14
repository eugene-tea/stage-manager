//! The model's one door to the window engine: every question the model asks about where
//! hidden windows are goes through here, to the pure half of the engine that runs
//! ([`ModelOptions::engine`](super::ModelOptions::engine); the Corner engine's is
//! [`crate::engine::corner`]). No other file of the model tests a park zone.
//!
//! The questions are of two kinds, kept apart because the engines answer them differently:
//!
//! - **Is this observed window hidden now?** ([`Model::listed_hidden`],
//!   [`Model::seen_hidden`], [`Model::listed_out_of_sight`], [`Model::shows_a_piece`],
//!   [`Model::clamped_at_hiding`]): asked of a frame the window server listed, at a launch, a
//!   restore, a Desktop change, a display change. The Corner engine reads it off the frame
//!   (its park zone).
//! - **May a window be brought back here?** ([`Model::no_restore`],
//!   [`Model::within_reach`], [`Model::lands_hidden`]): asked of a frame the model would
//!   record or restore. The Corner engine keeps restore frames out of its park zone; an
//!   engine that hides without moving has no such place.
//!
//! And one answer the engine gives about a window the model hid: where it left it
//! ([`Model::frame_left`]).

use super::{DisplayId, Model, WindowId, WindowMeta, WindowState};
use crate::engine::corner;
use crate::engine::geometry::Rect;
use crate::engine::recovery::EngineKind;

impl Model {
    /// True when a window the window server lists at `listed`, counted on `display`, is
    /// where the engine keeps hidden windows.
    pub fn listed_hidden(&self, display: DisplayId, listed: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::is_hidden(self.display(display), listed),
        }
    }

    /// True when the observed window `m` is hidden now ([`Model::listed_hidden`] of its
    /// frame on its display).
    pub(crate) fn seen_hidden(&self, m: &WindowMeta) -> bool {
        self.listed_hidden(m.display, m.frame)
    }

    /// True when a window listed at `listed` on `display` is out of sight: hidden
    /// ([`Model::listed_hidden`]) or showing only a piece where hidden windows are
    /// ([`Model::shows_a_piece`]). A window listed so at a launch or a restore is a card.
    pub(crate) fn listed_out_of_sight(&self, display: DisplayId, listed: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::out_of_sight(self.display(display), listed),
        }
    }

    /// True when a window at `frame` on `display` shows only a piece where hidden windows
    /// are: not hidden by the engine's own test, but no window anyone sees.
    pub(crate) fn shows_a_piece(&self, display: DisplayId, frame: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::shows_a_piece(self.display(display), frame),
        }
    }

    /// True when a shown window listed at `listed` on `display` came back squeezed against
    /// where hidden windows are (its app saved a hidden frame; counted at a restore).
    pub(crate) fn clamped_at_hiding(&self, display: DisplayId, listed: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::clamped_at_hiding(self.display(display), listed),
        }
    }

    /// True when `frame` is no place to bring a window of `display` back to: where the
    /// engine keeps hidden windows, so the window would look hidden there.
    pub(crate) fn no_restore(&self, display: DisplayId, frame: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::no_restore(self.display(display), frame),
        }
    }

    /// `frame` brought within reach on `display`: moved only as far as its title bar needs
    /// to be grabbed, never resized, and out of where the engine keeps hidden windows. Where
    /// every restore of the model lands.
    pub(crate) fn within_reach(&self, display: DisplayId, frame: Rect) -> Rect {
        match self.options.engine {
            EngineKind::Corner => corner::within_reach(self.display(display), frame),
        }
    }

    /// True when `frame` (no place to restore to, [`Model::no_restore`]) is where the
    /// engine's own hide of window `w` lands, not a frame the user gave the window there.
    pub(crate) fn lands_hidden(&self, w: &WindowState, frame: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::lands_hidden(w.restore, self.display(w.meta.display), self.options.park, frame),
        }
    }

    /// Where the model left window `id`: where the engine's hide leaves it while hidden
    /// (where bringing it back starts from), else its last frame. `meta.frame` alone does
    /// not say it for a hidden window: its frames are ignored while hidden
    /// ([`Model::window_frame_changed`]), so it keeps the frame the window had before.
    pub fn frame_left(&self, id: WindowId) -> Option<Rect> {
        let w = self.windows.get(&id)?;
        if !w.parked {
            return Some(w.meta.frame);
        }
        Some(match self.options.engine {
            EngineKind::Corner => corner::park_frame_of(w.restore, self.display(w.meta.display), self.options.park),
        })
    }
}
