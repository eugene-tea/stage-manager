//! The model's one door to the window engine: every question the model asks about where
//! hidden windows are goes through here, to the pure half of the engine that runs
//! ([`ModelOptions::engine`](super::ModelOptions::engine); the Corner engine's is
//! [`crate::engine::corner`], the Spaces engine's [`crate::engine::spaces`], the Apple mode's
//! [`crate::engine::apple`]: a window in Apple's strip is listed at its thumbnail). No other
//! file of the model tests a park zone.
//!
//! The questions are of two kinds, kept apart because the engines answer them differently:
//!
//! - **Is this observed window hidden now?** ([`Model::listed_hidden`],
//!   [`Model::listed_hidden_window`], [`Model::seen_hidden`], [`Model::listed_out_of_sight`],
//!   [`Model::shows_a_piece`], [`Model::clamped_at_hiding`]): asked of a frame the window
//!   server listed, at a launch, a restore, a Desktop change, a display change. The Corner
//!   engine reads it off the frame (its park zone); the Spaces engine cannot (a hidden
//!   window is listed at its own frame), so where the window is known
//!   ([`Model::listed_hidden_window`]) it answers from the model's record, and from a bare
//!   frame never "hidden". Under Corner "hidden now" and "listed at a hiding frame" are one
//!   test; under Spaces they part: a match ([`Model::seen_hidden`]) asks only the second.
//! - **May a window be brought back here?** ([`Model::no_restore`],
//!   [`Model::within_reach`], [`Model::carried`], [`Model::lands_hidden`]): asked of a frame the model would
//!   record or restore. The Corner engine keeps restore frames out of its park zone; an
//!   engine that hides without moving has no such place.
//!
//! - **A window first seen** ([`Model::found_parked`], [`Model::found_out_of_sight`],
//!   [`Model::safe_restore`]): a frame seen for a window the model does not know yet (a
//!   launch, a Desktop shown for the first time since, a window born), or a saved frame
//!   about to be recorded. Under either engine the park corner's geometry answers: no window
//!   of a Spaces run is ever there, but a Corner copy's may be (one that crashed, or fell back
//!   to the Corner engine, before this run) and an app may reopen a window at a park frame it
//!   saved under one; counted shown there, it stayed a sliver in the corner, and its stage
//!   showed it back at that frame (the review of 1.9, C-2). A known window is never asked:
//!   the Spaces answers for known windows stand.
//!
//! And two answers the engine gives about a window the model hid: where it left it
//! ([`Model::frame_left`]), and whether it is listed off screen at its own frame there
//! ([`Model::hidden_at_own_frame`]); and two about macOS: whether it brings another app in front when
//! the app in front quits ([`Model::macos_fronts_after_quit`]), and whether it counts a hidden
//! window on its Desktop at an app's activation ([`Model::macos_counts_hidden`]).

use super::{Display, DisplayId, Model, WindowId, WindowMeta, WindowState};
use crate::engine::{apple, corner, spaces};
use crate::engine::corner::park::clamp_to_visible;
use crate::engine::geometry::Rect;
use crate::engine::recovery::EngineKind;

impl Model {
    /// True when a window the window server lists at `listed`, counted on `display`, is
    /// where the engine keeps hidden windows.
    pub fn listed_hidden(&self, display: DisplayId, listed: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::is_hidden(self.display(display), listed),
            EngineKind::Spaces => spaces::is_hidden(self.display(display), listed),
            EngineKind::Apple => apple::is_hidden(self.display(display), listed),
        }
    }

    /// True when window `id`, listed at `listed` and counted on `display`, is hidden now.
    /// The Corner engine reads it off the frame ([`Model::listed_hidden`]). The Spaces
    /// engine's hidden window is listed at its own frame, so the frame says nothing: the
    /// window is hidden when the model hid it ([`spaces::hidden_by_record`]).
    pub(crate) fn listed_hidden_window(&self, id: WindowId, display: DisplayId, listed: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => self.listed_hidden(display, listed),
            EngineKind::Spaces => spaces::hidden_by_record(self.windows.get(&id)),
            EngineKind::Apple => apple::hidden_by_record(self.windows.get(&id)),
        }
    }

    /// True when the observed window `m` is listed where the engine keeps hidden windows
    /// ([`Model::listed_hidden`] of its frame on its display): its frame is a hiding place,
    /// not the user's, so a match leaves it out. Asked of frames only: a Spaces hidden
    /// window's frame is its own, and counts ([`Model::listed_hidden_window`] asks whether
    /// a window is hidden).
    pub(crate) fn seen_hidden(&self, m: &WindowMeta) -> bool {
        self.listed_hidden(m.display, m.frame)
    }

    /// True when a window listed at `listed` on `display` is out of sight: hidden
    /// ([`Model::listed_hidden`]) or showing only a piece where hidden windows are
    /// ([`Model::shows_a_piece`]). A window listed so at a launch or a restore is a card.
    pub(crate) fn listed_out_of_sight(&self, display: DisplayId, listed: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::out_of_sight(self.display(display), listed),
            EngineKind::Spaces => spaces::out_of_sight(self.display(display), listed),
            EngineKind::Apple => apple::out_of_sight(self.display(display), listed),
        }
    }

    /// True when a window at `frame` on `display` shows only a piece where hidden windows
    /// are: not hidden by the engine's own test, but no window anyone sees.
    pub(crate) fn shows_a_piece(&self, display: DisplayId, frame: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::shows_a_piece(self.display(display), frame),
            EngineKind::Spaces => spaces::shows_a_piece(self.display(display), frame),
            EngineKind::Apple => apple::shows_a_piece(self.display(display), frame),
        }
    }

    /// True when a shown window listed at `listed` on `display` came back squeezed against
    /// where hidden windows are (its app saved a hidden frame; counted at a restore).
    pub(crate) fn clamped_at_hiding(&self, display: DisplayId, listed: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::clamped_at_hiding(self.display(display), listed),
            EngineKind::Spaces => spaces::clamped_at_hiding(self.display(display), listed),
            EngineKind::Apple => apple::clamped_at_hiding(self.display(display), listed),
        }
    }

    /// True when `frame` is no place to bring a window of `display` back to: where the
    /// engine keeps hidden windows, so the window would look hidden there.
    pub(crate) fn no_restore(&self, display: DisplayId, frame: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::no_restore(self.display(display), frame),
            EngineKind::Spaces => spaces::no_restore(self.display(display), frame),
            EngineKind::Apple => apple::no_restore(self.display(display), frame),
        }
    }

    /// `frame` brought within reach on `display`: moved only as far as its title bar needs
    /// to be grabbed, never resized, and out of where the engine keeps hidden windows. Where
    /// every restore of the model lands.
    pub(crate) fn within_reach(&self, display: DisplayId, frame: Rect) -> Rect {
        match self.options.engine {
            EngineKind::Corner => corner::within_reach(self.display(display), frame),
            EngineKind::Spaces => spaces::within_reach(self.display(display), frame),
            EngineKind::Apple => apple::within_reach(self.display(display), frame),
        }
    }

    /// `frame`, a window's frame on display `from`, carried onto display `to`
    /// ([`Display::carry`]) and brought within reach there as `engine` has it
    /// ([`Model::within_reach`]). Where a window of a display removed (the lid closed), or
    /// moved to another display, is remembered until it is seen there. Not a method: asked
    /// while a window is being changed, with the engine and the displays copied out first.
    pub(crate) fn carried(engine: EngineKind, frame: Rect, to: &Display, from: &Display) -> Rect {
        let moved = to.carry(frame, from);
        match engine {
            EngineKind::Corner => corner::within_reach(to, moved),
            EngineKind::Spaces => spaces::within_reach(to, moved),
            EngineKind::Apple => apple::within_reach(to, moved),
        }
    }

    /// True when `frame` (no place to restore to, [`Model::no_restore`]) is where the
    /// engine's own hide of window `w` lands, not a frame the user gave the window there.
    pub(crate) fn lands_hidden(&self, w: &WindowState, frame: Rect) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::lands_hidden(w.restore, self.display(w.meta.display), self.options.park, frame),
            EngineKind::Spaces => spaces::lands_hidden(frame),
            EngineKind::Apple => apple::lands_hidden(frame),
        }
    }

    /// True when macOS itself brings another app in front when the app in front quits: it
    /// activates an app with a window on screen, and the Corner engine's hidden windows are
    /// listed on screen (in the corner), so that app's activation switches as any does. The
    /// Spaces engine's hidden windows are on no Desktop, so macOS activates none of their
    /// apps and the emptied stage would stay in front ([`Model::front_after_quit`]).
    pub(crate) fn macos_fronts_after_quit(&self) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::macos_fronts_after_quit(),
            EngineKind::Spaces => spaces::macos_fronts_after_quit(),
            EngineKind::Apple => apple::macos_fronts_after_quit(),
        }
    }

    /// True when macOS counts a window the engine hides on its Desktop when it picks a
    /// Desktop for an app's activation: the Corner engine's parked window is on its Desktop
    /// (in the corner); the Spaces engine's is in a stage Space, on no Desktop
    /// ([`Model::dock_arrival`], [`Model::dock_home`]).
    pub(crate) fn macos_counts_hidden(&self) -> bool {
        match self.options.engine {
            EngineKind::Corner => corner::macos_counts_hidden(),
            EngineKind::Spaces => spaces::macos_counts_hidden(),
            EngineKind::Apple => apple::macos_counts_hidden(),
        }
    }

    /// True when a window this engine hides stays on its Desktop for the window server (the
    /// park corner): its Spaces read as a shown window's do, so one listed on another Desktop
    /// was moved there (Mission Control shows it and lets it be dragged), never let out of
    /// its hiding place. False for a window hidden in a stage Space, on no Desktop: listed on
    /// a Desktop, something let it out, and it is hidden again. The same fact as
    /// [`Model::macos_counts_hidden`], asked by the window server's Desktops (`desks.rs`).
    pub(crate) fn hidden_on_their_desktop(&self) -> bool {
        self.macos_counts_hidden()
    }

    /// True when a window the model does not know yet, first seen at `listed` on `display`,
    /// is in the park corner's zone: hidden there by a Corner copy, or reopened there by its
    /// app. The Corner engine's test under either engine (module docs, "A window first seen").
    pub(crate) fn found_parked(&self, display: DisplayId, listed: Rect) -> bool {
        corner::is_hidden(self.display(display), listed)
    }

    /// True when a window the model does not know yet (or has just put in from a saved entry),
    /// listed at `listed` on `display`, is out of sight at the park corner: in its zone or only
    /// a piece showing there. The Corner engine's test under either engine (module docs, "A
    /// window first seen").
    pub(crate) fn found_out_of_sight(&self, display: DisplayId, listed: Rect) -> bool {
        corner::out_of_sight(self.display(display), listed)
    }

    /// A restore frame that is safe to record for a new window: `frame`, a centered one if
    /// `frame` is unusable (degenerate, or in the park corner's zone), brought on screen if it
    /// shows only a piece at the park corner. The Corner engine's geometry under either engine
    /// (module docs, "A window first seen"): under the Corner engine its own answers, under
    /// Spaces a frame a Corner copy left.
    pub(crate) fn safe_restore(&self, frame: Rect, display: DisplayId) -> Rect {
        let d = self.display(display);
        if frame.is_degenerate() || corner::no_restore(d, frame) {
            clamp_to_visible(frame, d.visible)
        } else if corner::shows_a_piece(d, frame) {
            corner::within_reach(d, frame)
        } else {
            frame
        }
    }

    /// True when window `id` is hidden by an engine that leaves a hidden window listed off
    /// screen at its own frame (the Spaces engine's stage Space), so that look says nothing
    /// of a tab ordered out ([`crate::engine::hidden_tab::tabs_selected`], the review of 1.9,
    /// C-1). The Corner engine's hidden window is a sliver on screen: off screen, it is not
    /// where the engine left it.
    pub fn hidden_at_own_frame(&self, id: WindowId) -> bool {
        match self.options.engine {
            EngineKind::Corner => false,
            EngineKind::Spaces => spaces::hidden_by_record(self.windows.get(&id)),
            // A strip window is listed at its thumbnail, never off screen at its own frame.
            EngineKind::Apple => false,
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
            EngineKind::Spaces => spaces::hidden_frame(w),
            EngineKind::Apple => apple::hidden_frame(w),
        })
    }
}
