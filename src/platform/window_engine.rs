//! The window engine: the one seam between what is shared (the model, the controller, the
//! strip, the rules, `state.json`, the guardian protocol) and how a stage's windows are
//! hidden, shown and brought back (docs/ARCHITECTURE.md, "Window engines").
//!
//! One strategy, [`WindowEngine`], with exactly two implementations planned: the Corner
//! engine ([`super::corner_engine::CornerEngine`], today's Accessibility park corner) and the
//! Spaces engine (1.9, after its spike). Shared code asks the engine and never which one
//! runs; what only one engine needs stays inside it.
//!
//! **Threads.** The engine is one value for the whole process ([`current`]): the WM thread
//! hides and shows through it inside the controller's batches, and the safety paths (quit, a
//! stand-down, the menu's last resort; the panic hook and the signal watcher run on threads
//! of their own) bring everything back through the same value. Its methods therefore take
//! `&self` and the engine is `Send + Sync`; state an engine keeps is behind its own locks.
//! The window calls ([`WindowEngine::hide`], [`WindowEngine::show`]) run on the WM thread
//! only, inside the controller's write (the stale-handle retry, the echo and failure
//! bookkeeping are the controller's, the same for every engine).

use std::sync::{Arc, OnceLock, RwLock};

use super::ax::{AxElement, AxResult};
use crate::engine::geometry::Rect;
use crate::engine::model::{Display, Model, WindowId};
pub use crate::engine::recovery::EngineKind;

/// How a window is brought to its frame ([`WindowEngine::show`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowHow {
    /// One try (a switch, a new window).
    Once,
    /// A busy app is tried again quickly, within the restore budget (quit, a stand-down:
    /// [`crate::engine::leftovers::RESTORE_RETRY`]).
    Retrying,
}

/// How a stage's windows are hidden, shown and brought back.
pub trait WindowEngine: Send + Sync {
    /// Which engine this is (the parked file says it to the guardian, logs name it).
    fn kind(&self) -> EngineKind;

    /// Where a hide leaves window `id` of model `m`, as the window server will list it: what
    /// the controller checks a hide against, and where bringing the window back starts from.
    /// `None` for a window the model does not manage.
    fn hidden_at(&self, m: &Model, id: WindowId) -> Option<Rect>;

    /// Hides one window (an [`Action::Hide`](crate::engine::model::Action::Hide)) through
    /// `window`, its Accessibility element; `at` is what [`Self::hidden_at`] answered for it
    /// before the write (or the controller's own place for this one hide: a minimized
    /// window's dive). WM thread.
    fn hide(&self, id: WindowId, at: Rect, window: &AxElement) -> AxResult<()>;

    /// Brings one window to `frame`, shown (an
    /// [`Action::SetFrame`](crate::engine::model::Action::SetFrame): the model counts the
    /// window shown whether it was hidden or not, so the engine shows it either way).
    /// WM thread.
    fn show(&self, id: WindowId, frame: Rect, window: &AxElement, how: ShowHow) -> AxResult<()>;

    /// True when a window the window server lists at `listed` on display `d` is where this
    /// engine keeps hidden windows.
    fn is_hidden(&self, d: &Display, listed: Rect) -> bool;

    /// The last line of defence in this process: every hidden window brought back, whatever
    /// state the model and the WM thread are in. Any thread. True when nothing failed.
    fn bring_all_back(&self, why: &str) -> bool;
}

static ENGINE: OnceLock<RwLock<Arc<dyn WindowEngine>>> = OnceLock::new();

fn slot() -> &'static RwLock<Arc<dyn WindowEngine>> {
    ENGINE.get_or_init(|| RwLock::new(Arc::new(super::corner_engine::CornerEngine)))
}

/// The engine this process runs: the Corner engine until another is installed.
pub fn current() -> Arc<dyn WindowEngine> {
    match slot().read() {
        Ok(e) => e.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_runs_the_corner_engine() {
        assert_eq!(current().kind(), EngineKind::Corner);
        assert_eq!(current().kind().name(), "corner");
    }
}
