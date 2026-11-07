//! A window its app reopens much smaller than saved gets its saved frame back, once (round
//! restore-saved-size, his go 11-06: after a restart VS Code reopened a 2082x1253 window at
//! 665x401, the restore matched it strongly and kept the app's frame, and he resized it back
//! by hand).
//!
//! The launch's arrival on a saved Desktop keeps the frames apps give their windows
//! (`LaunchFrames::Live` in `persist.rs`). The one exception ([`owes`]): after a restart only
//! (`state.json` from another window server session), a strong match (the title, not just
//! the app and size), saved frames applying on the window's screen (its saved screen at its
//! saved size, [`Model::frames_fit`]), the app's frame under half the saved one's area and
//! the saved size fitting that screen's visible frame. The saved frame is then *owed*
//! ([`Model::owe_saved_size`]) and paid when the window is first shown
//! ([`Model::pay_saved_size`], from [`Model::restore_window`]: in front at the restore, or at
//! its card's first click), through [`super::Display::reach`] as every restore frame; one INFO
//! line names it. A move or resize the model takes as the user's ends the debt
//! ([`Model::forgive_saved_size`]), and so does the window's leaving or another screen.
//! Meanwhile `state.json` keeps the owed frame ([`Model::owed_frame`]): a second restart
//! before it shows still knows the size. Off (`[restore] saved_size = false`,
//! [`super::ModelOptions::keep_app_frames`]): every window keeps the frame its app gives it,
//! Apple's plain behaviour.

use super::{DisplayId, Home, Model, WindowId, WindowMeta};
use crate::engine::geometry::Rect;
use crate::engine::persist::AssignedWindow;
use crate::engine::window_match::Grade;

/// A saved frame owed to a window ([`Model::owe_saved_size`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Owed {
    /// The saved frame.
    pub(crate) frame: Rect,
    /// Its saved pre-fit frame, which applies with it.
    pub(crate) original: Option<Rect>,
    /// The screen it was owed on: the saved frame is that screen's.
    pub(crate) display: DisplayId,
}

/// True when a window its app opened at `live` is owed its saved frame `saved` on a screen
/// whose visible frame is `visible`: both real, `live` under half `saved`'s area, and
/// `saved`'s size fitting `visible`. The match and the session are the caller's
/// ([`Model::owe_saved_size`]).
pub(crate) fn owes(live: Rect, saved: Rect, visible: Rect) -> bool {
    const SLACK: f64 = 1.0;
    !live.is_degenerate()
        && !saved.is_degenerate()
        && live.area() * 2.0 < saved.area()
        && saved.w <= visible.w + SLACK
        && saved.h <= visible.h + SLACK
}

impl Model {
    /// Window `meta`, just placed into a stage from its saved entry `saved` at its app's
    /// frame (`origin`: the saved Desktop the entry comes from), is owed its saved frame when
    /// [`owes`] says so after a restart on a strong match, saved frames applying on its screen.
    pub(crate) fn owe_saved_size(&mut self, meta: &WindowMeta, saved: &AssignedWindow, origin: Option<u64>) {
        if self.options.keep_app_frames || !self.restore.new_session || saved.grade != Grade::Strong {
            return;
        }
        let Some(w) = self.windows.get(&meta.id) else { return };
        if !matches!(w.home, Home::Stage(_)) {
            return;
        }
        let display = w.meta.display;
        let d = self.display(display);
        let visible = if d.visible.is_degenerate() { d.frame } else { d.visible };
        if !owes(meta.frame, saved.frame, visible) || !self.frames_fit(display, origin) {
            return;
        }
        self.owed.insert(meta.id, Owed { frame: saved.frame, original: saved.original, display });
    }

    /// True while window `id` is owed its saved frame ([`Model::owe_saved_size`]).
    pub(crate) fn owes_saved_size(&self, id: WindowId) -> bool {
        self.owed.contains_key(&id)
    }

    /// The saved frame window `id` is owed, for `state.json` while it waits.
    pub(crate) fn owed_frame(&self, id: WindowId) -> Option<Rect> {
        self.owed.get(&id).map(|o| o.frame)
    }

    /// Window `id` is shown now ([`Model::restore_window`]): an owed saved frame becomes its
    /// restore frame, once. Dropped unpaid when the switch is off now or the window is on
    /// another screen than it was owed on.
    pub(crate) fn pay_saved_size(&mut self, id: WindowId) {
        let Some(owed) = self.owed.remove(&id) else { return };
        let keep = self.options.keep_app_frames;
        let Some(w) = self.windows.get_mut(&id) else { return };
        if keep || w.meta.display != owed.display {
            return;
        }
        let from = w.restore;
        w.restore = owed.frame;
        w.original = owed.original;
        tracing::info!(
            id = id.0,
            bundle = %w.meta.bundle_id,
            from = %format_args!("{:.0},{:.0} {:.0}x{:.0}", from.x, from.y, from.w, from.h),
            to = %format_args!("{:.0},{:.0} {:.0}x{:.0}", owed.frame.x, owed.frame.y, owed.frame.w, owed.frame.h),
            "restore: a window its app reopened much smaller than saved gets its saved size back"
        );
    }

    /// The user moved or resized window `id` (a frame taken as theirs): it keeps their frame,
    /// the saved one is owed no more.
    pub(crate) fn forgive_saved_size(&mut self, id: WindowId) {
        if self.owed.remove(&id).is_some() {
            tracing::debug!(id = id.0, "restore: a window owed its saved size was moved or resized first; it keeps the user's frame");
        }
    }

    /// `[restore] saved_size` as config says now (off: [`super::ModelOptions::keep_app_frames`]).
    pub fn set_saved_size(&mut self, on: bool) {
        self.options.keep_app_frames = !on;
    }
}

#[cfg(test)]
mod tests;
