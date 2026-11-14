//! The window engine: the one seam between what is shared (the model, the controller, the
//! strip, the rules, `state.json`, the guardian protocol) and how a stage's windows are
//! hidden, shown and brought back (docs/ARCHITECTURE.md, "Window engines").
//!
//! One strategy, [`WindowEngine`], with exactly two implementations planned: the Corner
//! engine ([`super::corner::engine::CornerEngine`], today's Accessibility park corner) and the
//! Spaces engine ([`super::spaces::engine::SpacesEngine`], 1.9). Shared code asks the engine and never which one
//! runs; what only one engine needs stays inside it.
//!
//! **Which one runs** is decided at every launch ([`choose_at_launch`]: the config, the
//! launch probe, the switches; [`crate::engine::engine_choice`]) and can change once while
//! running: a Spaces engine whose checks fail ([`WindowEngine::health`]) hands over to the
//! Corner engine for the rest of the run ([`fall_back`], called by the controller once every
//! window is back).
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

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use objc2_foundation::MainThreadMarker;

use super::ax::{AxElement, AxResult};
use super::safety::{Ours, Report, ScanScope};
use crate::engine::engine_choice::{self, Asked, Choice, ConfigEngine};
use crate::engine::geometry::Rect;
use crate::engine::model::{Display, Model, WindowId};
use crate::ui::strip_model::Transition;
pub use crate::engine::recovery::EngineKind;
use crate::engine::recovery::{Known, ParkedFile, ScanDisplays, StageSpace};

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

    /// True when window `id`, listed at `listed` on display `d`, is hidden by this engine
    /// now: what a check of a hide or a show the engine was asked for reads (an arrival's).
    /// By the frame unless the engine knows better ([`Self::is_hidden`]); the Spaces engine:
    /// by what it holds, as last read back from the window server.
    fn hides_now(&self, _id: WindowId, d: &Display, listed: Rect) -> bool {
        self.is_hidden(d, listed)
    }

    /// True when a window at `frame` is still where this engine hid it, out of anyone's
    /// reach, as far as what is `known` of the window allows; `displays`: the screens as the
    /// safety paths see them now. What the guardian's watch (and the app's watch of the
    /// windows it took from one) asks of every frame it reads. Any thread.
    fn stranded(&self, frame: Rect, displays: &ScanDisplays, known: Known) -> bool;

    /// The guardian, before its first scan: what the app's parked file says of where it hid
    /// its windows is taken as this process's own knowledge (the guardian never saw the
    /// app's screens or config).
    fn assume(&self, file: &ParkedFile);

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

    /// The windows this engine keeps hidden on a Desktop that shows now where the window
    /// server lists them off screen: present on their Desktop all the same, for whoever
    /// reads presence off the list (a window gone from the screen left its Desktop, a
    /// Desktop is recognized by its windows). The Corner engine: none, its hidden windows
    /// are listed on screen, in the corner.
    fn present_off_screen(&self) -> HashSet<u32>;

    /// The checks while running: `Some(why)` once this engine should not hide another window
    /// (a window not where a call put it after a retry, or misses set right too often:
    /// [`crate::engine::engine_choice::Health`]). The Corner engine never fails this way.
    fn health(&self) -> Option<String> {
        None
    }

    /// The places this engine keeps hidden windows that another process could not find by
    /// itself, for the parked file ([`ParkedFile::spaces`]): the Spaces engine's stage
    /// Spaces. The Corner engine: none (its park zones are in the file already).
    fn parked_spaces(&self) -> Vec<StageSpace> {
        Vec::new()
    }
}

static ENGINE: OnceLock<RwLock<Arc<dyn WindowEngine>>> = OnceLock::new();

fn slot() -> &'static RwLock<Arc<dyn WindowEngine>> {
    ENGINE.get_or_init(|| RwLock::new(first()))
}

/// The engine a process starts with: the Corner engine, until the launch chooses
/// ([`choose_at_launch`]) or the guardian installs its app's ([`install`]).
fn first() -> Arc<dyn WindowEngine> {
    Arc::new(super::corner::engine::CornerEngine)
}

/// The launch's choice and what happened to it since, for `ctl state` (`None` in a process
/// that did not choose: the guardian, the tools).
static CHOICE: Mutex<Option<Choice>> = Mutex::new(None);

fn lock_choice() -> std::sync::MutexGuard<'static, Option<Choice>> {
    CHOICE.lock().unwrap_or_else(|p| p.into_inner())
}

/// The app's launch: runs the launch probe unless `config` or the test switch asks for the
/// Corner engine, chooses ([`engine_choice::choose`]), installs the engine and logs the
/// choice with its reason. Main thread, once AppKit is up, before the WM thread starts.
pub fn choose_at_launch(mtm: MainThreadMarker, config: ConfigEngine) -> Choice {
    let asked = Asked { config, opted_in: opted_in(), test: super::spaces::engine::from_env() };
    let probe = asked.probes().then(|| super::spaces::probe::run(mtm));
    match &probe {
        Some(p) if p.ok => tracing::info!(ms = format!("{:.1}", p.ms), "window engine: launch probe passed: {}", p.reason),
        Some(p) => tracing::warn!(ms = format!("{:.1}", p.ms), "window engine: launch probe failed: {}", p.reason),
        None => tracing::info!(config = config.name(), "window engine: launch probe not run"),
    }
    let choice = engine_choice::choose(asked, probe);
    tracing::info!(engine = choice.kind.name(), "window engine: {}", choice.why);
    install(of(choice.kind));
    *lock_choice() = Some(choice.clone());
    choice
}

/// The hidden opt-in for the Spaces engine under `[windows] engine = "auto"`, until it is
/// measured: `STAGE_MANAGER_SPACES=1`.
fn opted_in() -> bool {
    std::env::var("STAGE_MANAGER_SPACES").is_ok_and(|v| v.trim() == "1")
}

/// The Corner engine from now on, for the rest of the run: a running engine's checks failed
/// (`why`) and every window it hid is back (the controller's part). The choice records it
/// for `ctl state`; the parked file names the engine from its next write.
pub fn fall_back(why: &str) {
    install(of(EngineKind::Corner));
    let mut c = lock_choice();
    match c.as_mut() {
        Some(choice) => {
            choice.kind = EngineKind::Corner;
            choice.fallback = Some(why.to_string());
        }
        None => *c = Some(Choice { kind: EngineKind::Corner, why: "fell back".into(), probe: None, fallback: Some(why.to_string()) }),
    }
}

/// The launch's choice as it stands ([`choose_at_launch`], [`fall_back`]).
pub fn choice() -> Option<Choice> {
    lock_choice().clone()
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
        EngineKind::Corner => Arc::new(super::corner::engine::CornerEngine),
        EngineKind::Spaces => super::spaces::engine::shared(),
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
