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
//! stand-down, a logout, the menu's last resort, the launch's and a display change's scan;
//! the panic hook and the signal watcher run on threads of their own) bring everything back
//! through the same value. The guardian, a process of its own, runs the engine the app it
//! watched ran ([`of`] the parked file's [`EngineKind`], [`install`]ed before its first scan). Its methods therefore take
//! `&self` and the engine is `Send + Sync`; state an engine keeps is behind its own locks.
//! The window calls ([`WindowEngine::hide`], [`WindowEngine::show`]) run on the WM thread
//! only, inside the controller's write (the stale-handle retry, the echo and failure
//! bookkeeping are the controller's, the same for every engine).

use std::sync::{Arc, OnceLock, RwLock};

use super::ax::{AxElement, AxResult};
use super::safety::{Ours, Report, ScanScope};
use crate::engine::geometry::Rect;
use crate::engine::model::{Display, Model, WindowId};
use crate::ui::strip_model::Transition;
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

/// How a switch moves, as the engine describes it on the WM thread
/// ([`WindowEngine::animate`]). The controller runs the choreography it names around the
/// shared steps (the plan, the note to the strip, the wait for its answer and for its
/// unveil); the strip plays it on the main thread. The engine never calls UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// Pictures of the windows, full size, move between the stage and the cards above every
    /// window (the strip's snapshots), and the windows move behind them: the outgoing ones
    /// hide once their pictures cover them, the incoming ones stay hidden until the strip
    /// unveils them.
    Pictures,
}

/// How a stage's windows are hidden, shown, moved in a switch and brought back.
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

    /// How the switch planned as `t` moves ([`Motion`]): asked once a switch, on the WM
    /// thread, after the plan and before anything is sent to the strip or moved.
    fn animate(&self, t: &Transition) -> Motion;

    /// The last line of defence in this process: every hidden window brought back, whatever
    /// state the model and the WM thread are in. Any thread. True when nothing failed.
    fn bring_all_back(&self, why: &str) -> bool;

    /// An abort's bring-back (SIGABRT, from the signal watcher's thread while the handler
    /// waits a bounded moment): what can be brought back at once, no scan, no retries; the
    /// guardian covers the rest. `None` when the engine's own state was busy throughout.
    fn bring_back_briefly(&self) -> Option<Report>;

    /// Every bring-back's last look: the window server's word on it, since a move that
    /// "succeeded" is no proof. Each window of an allowed app still hidden is brought back;
    /// `None` when none is left, else what this look knew of the windows left (`ours`: what
    /// the caller knows of whose they are), for the next try. `moved`: windows moved a moment
    /// ago, so the list is given its settle first. Bounded
    /// ([`crate::engine::leftovers::LEFTOVERS_MAX`]). Any thread.
    fn bring_back_rest(&self, why: &str, ours: Ours, moved: bool) -> Option<Ours>;

    /// The recovery scan (a launch, a display change, the guardian): every window in `scope`
    /// found hidden with no running copy to answer for it goes back to a crashed copy's
    /// registered frame, else its saved frame, else centered. Also the apps that did not
    /// answer, to be scanned again.
    fn recover(&self, why: &str, scope: &ScanScope) -> (Report, Vec<i32>);
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

/// The engine of kind `kind`: how the guardian gets the engine the app it watched ran
/// ([`crate::engine::recovery::ParkedFile::engine`]).
pub fn of(kind: EngineKind) -> Arc<dyn WindowEngine> {
    match kind {
        EngineKind::Corner => Arc::new(super::corner_engine::CornerEngine),
    }
}

/// Makes `engine` the one this process runs from now on ([`current`]). The swap itself
/// cannot panic, so the lock is never left held (the panic hook reads it).
pub fn install(engine: Arc<dyn WindowEngine>) {
    match slot().write() {
        Ok(mut e) => *e = engine,
        Err(poisoned) => *poisoned.into_inner() = engine,
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

    #[test]
    fn a_parked_file_names_its_engine_and_an_older_one_is_the_corner_engines() {
        use crate::engine::recovery::ParkedFile;
        let older = ParkedFile::from_json(br#"{"visible":null,"windows":[]}"#).expect("an older file");
        assert_eq!(of(older.engine).kind(), EngineKind::Corner);
        install(of(older.engine));
        assert_eq!(current().kind(), EngineKind::Corner, "the guardian runs the engine the file names");
    }
}
