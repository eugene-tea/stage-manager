//! Arriving on a Desktop for a window: the Desktop arranged ahead of its recognition.
//!
//! A Dock click, ⌘Tab or an activation that lands on a window waiting in an inactive stage
//! of another Desktop makes macOS slide that Desktop in; Apple's Stage Manager slides it in
//! with that window's stage already active. Every public notice of it comes at the **end**
//! of the slide (measured 10-13: from an `open` activation the arriving Desktop's windows on
//! screen from +90 ms, the leaving one's until +327 ms, the activation ~+324 ms, the Space
//! change notice +346 ms; the owner's Dock click: the focus +419 ms and the notice +423 ms
//! after the mouse-down). So the switch goes out as early as the facts allow
//! ([`crate::engine::model::Model::arrange_ahead`]):
//!
//! - **At the Dock click** ([`crate::platform::dock`]): a click on an application item of
//!   the Dock is the earliest sign of where the user goes. When macOS will switch Desktops
//!   for it ([`crate::engine::model::Model::dock_arrival`]: "switch to a Space with open
//!   windows for the application" on, the app with no window on the Desktop shown, its
//!   windows all placed, not hidden), the window macOS will show (the app's focused window,
//!   else its frontmost in the window server's order) has its Desktop arranged at once:
//!   macOS carries out moves of windows on a Desktop not shown (the 6T probe, 10-13), and
//!   that Desktop's own strip pane is redrawn in its final order. The window is held as its
//!   focus would be. No Space change within [`DOCK_SWITCH_WAIT`] undoes it.
//! - **At the notice**: an arrangement made at the click whose window the window server
//!   does not list on the main screen now is undone (another Desktop slid in). With none,
//!   when the held window, or with none held the activated (else frontmost) app's focused
//!   window asked at once (a bounded Accessibility query), waits on the Desktop listed now,
//!   its stage is arranged before every app's window list is read again.
//! - **While a Space change settles** (a full-screen animation, an overview closing): a
//!   focus held then is arranged for as soon as the window server lists its window, unless
//!   a window of that Desktop covers the display or an overview shows.
//!
//! When the Desktop is recognized, the prediction is confirmed (the stage's windows raised,
//! the window focused; nothing moves) or undone ([`crate::engine::model::Model::undo_ahead`]);
//! the user acting on the Desktop shown, a full-screen Space, or the hold running out undo
//! it too; a focus held meanwhile undoes it only when it names another known window, or
//! another app (an unknown window of the same app, a palette opening, does not). Every
//! window an arrangement made on a Desktop not shown, or an undo, brings back stays in the
//! safety registry until the window server shows it out of the park zone. At a confirmed
//! arrival the predicted window counts as the focused one (AX's own answer is logged beside
//! it): an answer still naming the previous app's window never switches back. The arrival's
//! own switch, when nothing was arranged ahead, is part of the arrival's batch and never
//! animated: the Desktop's slide is the motion (Apple's).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::{Controller, Unrestored, cards_in, on_main_screen};
use crate::engine::focus_replay::Wanted;
use crate::engine::fullscreen;
use crate::engine::geometry::Rect;
use crate::engine::model::{Action, Ahead, AppFacts, Observation, SpaceId, WindowId};
use crate::platform::ax::{AxElement, names};
use crate::platform::dock::{self, DockItem, DockPress};
use crate::platform::observe::{Observer, WindowKind};
use crate::platform::safety::{self, ParkedEntry};
use crate::platform::windows::{self, CgWindow};
use crate::ui::strip_model::{self, StripSnapshot};

/// The Accessibility timeout for asking an app its focused window on the way to a Desktop
/// (at the notice, at a Dock click): an app busy with the switch must not hold it up. The
/// app goes back to the global timeout right after ([`AxElement::bounded`]).
const QUERY_AX_TIMEOUT_SECONDS: f32 = 0.05;

/// An arrangement made at a Dock click is undone when no Space change has come this long
/// after it (the notice comes ~0.35–0.45 s after the click).
pub const DOCK_SWITCH_WAIT: Duration = Duration::from_millis(1000);

/// When a Desktop was arranged ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Trigger {
    /// At a click on the app's Dock item, the Desktop not shown yet.
    DockClick,
    /// At the Space change notice, for the focus held before it.
    Notice,
    /// At the Space change notice, for the app's focused window (its focus event not seen
    /// yet).
    NoticeAx,
    /// While the Space change settles, for a focus held meanwhile.
    Settle,
}

impl Trigger {
    fn name(self) -> &'static str {
        match self {
            Trigger::DockClick => "dock-click",
            Trigger::Notice => "notice",
            Trigger::NoticeAx => "notice-ax",
            Trigger::Settle => "settle",
        }
    }

    /// Arranged while its Desktop was not shown (its restores registered until seen).
    fn away(self) -> bool {
        self == Trigger::DockClick
    }
}

/// How long the moves of an arrangement made at a Dock click are checked against the window
/// server and sent again: the slide (~0.25–0.45 s) and a margin.
pub const VERIFY_FOR: Duration = Duration::from_millis(600);

/// How often they are checked (the WM thread's fast tick).
pub const VERIFY_EVERY: Duration = Duration::from_millis(20);

/// One move of an arrangement made on a Desktop not shown, as the window server should show
/// it: parked (in the park zone), or at a frame; with how often it was sent again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Expect {
    id: u32,
    frame: Rect,
    park: bool,
    /// Times sent again (at most [`RESEND_MAX`]).
    tries: u32,
    /// A resend timed out or failed: not sent again.
    gave_up: bool,
    /// The 1 pt nudge went through, the exact move did not: the exact one is owed.
    nudged_only: bool,
}

/// A move is sent again at most this many times (the measured case needed one, when the
/// Desktop showed).
pub const RESEND_MAX: u32 = 3;

/// The moves of an arrangement made on a Desktop not shown, until the window server shows
/// them (measured 10-13: in one run of three it applied them only when that Desktop became
/// the shown one, at the end of the slide; the stage left showed all through it).
#[derive(Debug, Clone)]
pub(super) struct Verify {
    sent: Instant,
    next: Instant,
    rounds: u32,
    moves: Vec<Expect>,
}

/// True when the window server's `bounds` for a move are what it asked: in the park zone
/// (`parks`) for a park; else out of it, within 2 pt of the frame's origin (the size is the
/// app's: a terminal or a fixed-size window keeps its own).
fn shows(e: &Expect, bounds: Rect, parks: impl Fn(Rect) -> bool) -> bool {
    if e.park {
        parks(bounds)
    } else {
        !parks(bounds) && (bounds.x - e.frame.x).abs() <= 2.0 && (bounds.y - e.frame.y).abs() <= 2.0
    }
}

/// The moves among `actions` to check against the window server.
fn expects(actions: &[Action]) -> Vec<Expect> {
    actions
        .iter()
        .filter_map(|a| match *a {
            Action::Park { id, frame } => Some(Expect { id: id.0, frame, park: true, tries: 0, gave_up: false, nudged_only: false }),
            Action::SetFrame { id, frame } => {
                Some(Expect { id: id.0, frame, park: false, tries: 0, gave_up: false, nudged_only: false })
            }
            _ => None,
        })
        .collect()
}

/// A Dock press's prediction and what the app said, until the next Space change shows
/// which Desktop the click went to (logged).
#[derive(Debug, Clone, Copy)]
pub(super) struct DockSeen {
    at: Instant,
    pid: i32,
    active: bool,
    predicted: Option<u64>,
}

/// A Space change this long after a Dock press counts as its click's.
const DOCK_SEEN_FOR: Duration = Duration::from_millis(2000);

/// A Dock click's prediction: the app and the window macOS will show, or why none.
pub(super) type Prediction = Result<(i32, WindowId), &'static str>;

/// A Desktop arranged ahead and not recognized yet.
#[derive(Debug, Clone)]
pub(super) struct AheadSent {
    ahead: Ahead,
    /// The window's app.
    pid: i32,
    trigger: Trigger,
    /// When the focus (or the click) it was arranged for came.
    focus_at: Instant,
    /// When the arrangement went out.
    sent_at: Instant,
    /// Space changes notified before it went out.
    changes_at: u64,
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

/// True when a window on screen covers the whole display (a full-screen window, or the
/// stand-in macOS shows while a window enters or leaves full screen), leaving out the
/// windows known to live on another Desktop than the arriving one (`elsewhere`: a zoomed
/// window of the Desktop being left).
fn display_covered(list: &[CgWindow], main: Rect, elsewhere: impl Fn(u32) -> bool) -> bool {
    list.iter().any(|w| w.onscreen && w.layer == 0 && fullscreen::covers_display(w.bounds, main) && !elsewhere(w.id))
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

/// What a focus held now does to the arrangement: undo it (`.0`: what is held contradicts
/// one that stands), arrange for the held window (`.1`: a window focus held while a Space
/// change settles, no overview showing, nothing standing after the undo).
fn hold_step(contradicted: bool, ahead: bool, held_window: bool, pending: bool, overview: bool) -> (bool, bool) {
    let undo = ahead && contradicted;
    (undo, (!ahead || undo) && held_window && pending && !overview)
}

/// What the Space change notice does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeStep {
    /// An overview shows: its list is not the Desktop's.
    Nothing,
    /// An arrangement stands and its window is listed on the main screen.
    Keep,
    /// An arrangement stands and its window is not listed: another Desktop slid in. Undone,
    /// then as [`NoticeStep::Arrange`].
    Undo,
    /// None stands: look for one to make.
    Arrange,
}

/// The notice's step: `overview` shows, `ahead_listed` is whether a standing arrangement's
/// window is listed on the main screen (`None`: none stands).
fn notice_step(overview: bool, ahead_listed: Option<bool>) -> NoticeStep {
    match (overview, ahead_listed) {
        (true, _) => NoticeStep::Nothing,
        (false, Some(true)) => NoticeStep::Keep,
        (false, Some(false)) => NoticeStep::Undo,
        (false, None) => NoticeStep::Arrange,
    }
}

/// Why an arrangement is undone at a tick, if it is: never while a Space change settles; one
/// made at a Dock click with no Space change [`DOCK_SWITCH_WAIT`] after it; any other whose
/// focus is no longer held (or what is held says otherwise).
fn expire_step(trigger: Trigger, pending: bool, changed: bool, waited: Duration, contradicted: bool) -> Option<&'static str> {
    if pending {
        None
    } else if trigger == Trigger::DockClick && !changed && waited >= DOCK_SWITCH_WAIT {
        Some("no Space change came after the Dock click")
    } else if contradicted {
        Some("its Desktop did not come")
    } else {
        None
    }
}

/// What a focus within the echo window of an arrangement's own moves is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EchoVerdict {
    /// An echo: the predicted window's own focus, another app's, an unknown window's.
    Echo,
    /// The user's word: another known window of the predicted app, in another stage (macOS
    /// shows another window than predicted): it undoes the arrangement.
    Beats,
    /// Another window of the stage arranged: the arrangement is right, that window is the
    /// one focused at the arrival.
    Retarget(WindowId),
}

/// The verdict for a focus of a window of app `pid` (the standard window it is or follows
/// `resolved`, in stage `stage`) while an arrangement stands for `window` (in `ahead_stage`)
/// of app `ahead_pid`.
fn echo_verdict(
    window: WindowId,
    ahead_stage: crate::engine::model::StageId,
    ahead_pid: i32,
    pid: Option<i32>,
    resolved: Option<WindowId>,
    stage: Option<crate::engine::model::StageId>,
) -> EchoVerdict {
    match resolved {
        Some(r) if pid == Some(ahead_pid) && r != window => {
            if stage == Some(ahead_stage) { EchoVerdict::Retarget(r) } else { EchoVerdict::Beats }
        }
        _ => EchoVerdict::Echo,
    }
}

/// True when forgetting a held focus also undoes the arrangement: never while a Space change
/// settles (nobody acts on the Desktop shown then; stage commands are refused).
fn forget_undoes(pending: bool) -> bool {
    !pending
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

    /// True when a focus of window `id`, within the echo window of an arrangement's own
    /// moves, is still the user's word ([`echo_verdict`]): macOS focused another known window
    /// of the predicted app in another stage. One in the same stage retargets the
    /// arrangement (that window is focused at the arrival) and stays an echo.
    pub(super) fn focus_beats_echo(&mut self, ob: &Observer, id: u32, now: Instant) -> bool {
        let Some(sent) = &self.ahead else { return false };
        let Some(m) = self.model.as_ref() else { return false };
        let pid = ob.window(id).map(|w| w.pid);
        let resolved = m.resolve_standard(WindowId(id));
        let stage = resolved.and_then(|r| m.stage_of(r));
        match echo_verdict(sent.ahead.window, sent.ahead.stage, sent.pid, pid, resolved, stage) {
            EchoVerdict::Echo => false,
            EchoVerdict::Beats => {
                tracing::debug!(id, predicted = sent.ahead.window.0, "a focus of another stage's window of the predicted app, not an echo");
                true
            }
            EchoVerdict::Retarget(window) => {
                let pid = sent.pid;
                tracing::debug!(id, predicted = sent.ahead.window.0, now_for = window.0, "a focus of another window of the stage arranged: it is the one focused at the arrival");
                if let Some(sent) = self.ahead.as_mut() {
                    sent.ahead.window = window;
                }
                self.focus_replay.hold(Wanted::Window { id: window, pid }, now);
                false
            }
        }
    }

    /// True when a held window focus should survive an activation of app `pid` that changed
    /// nothing here (R5 2: macOS reports the focus and the activation in either order).
    pub(super) fn held_is_of(&self, pid: i32, now: Instant) -> bool {
        matches!(self.focus_replay.peek(now), Some(Wanted::Window { pid: p, .. }) if p == pid)
    }

    /// A focus or activation was just held ([`hold_step`]): an arrangement it contradicts is
    /// undone; while a Space change settles, a held window's Desktop is arranged as soon as
    /// the window server lists the window.
    pub(super) fn after_hold(&mut self, ob: &Observer, now: Instant) {
        let held = match self.focus_replay.peek(now) {
            Some(Wanted::Window { id, .. }) => Some(id),
            _ => None,
        };
        let overview = self.overview.showing().is_some() || self.overview.holds_arrival(now);
        let (undo, arrange) =
            hold_step(self.ahead_contradicted(now), self.ahead.is_some(), held.is_some(), self.space_pending, overview);
        if undo {
            self.undo_ahead(ob, "another focus or activation came");
        }
        let (true, Some(id)) = (arrange, held) else { return };
        let Some(main) = self.model.as_ref().map(|m| *m.main_display()) else { return };
        let list = windows::list_windows();
        if self.arrange_if_listed(ob, &list, main.frame, id, Trigger::Settle, now) {
            // The Desktop sliding in shows its strip arranged too ([`Self::strip_space`]).
            self.present();
        }
    }

    /// The Space change notice ([`notice_step`]). An arrangement whose window the window
    /// server does not list on the main screen now is undone (another Desktop slid in).
    /// With none, when the held window, or with none held the activated (else frontmost)
    /// app's focused window, waits on the Desktop listed now (the one sliding in), its stage
    /// is arranged before every app's window list is read again and the Desktop recognized.
    pub(super) fn arrange_at_notice(&mut self, ob: &mut Observer, now: Instant) {
        let Some(main) = self.model.as_ref().map(|m| *m.main_display()) else { return };
        let list = windows::list_windows();
        let overview = self.look_for_overview(&list, main.frame, now) || self.overview.holds_arrival(now);
        let shown = on_main_screen(&list, main.frame);
        let listed = self.ahead.as_ref().map(|s| listed_for_arrival(&list, s.ahead.window.0, &shown, main.frame));
        match notice_step(overview, listed) {
            NoticeStep::Nothing | NoticeStep::Keep => return,
            NoticeStep::Undo => {
                // Then as with none: the Desktop that did slide in may be arranged now.
                self.undo_ahead(ob, "its window is not on screen at the Space change notice");
            }
            NoticeStep::Arrange => {}
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
                let Some(id) = focused_window_of(pid) else { return };
                if self.arrange_if_listed(ob, &list, main.frame, id, Trigger::NoticeAx, now) {
                    // Held as its focus event would have been (replayed on arrival).
                    self.focus_replay.hold(Wanted::Window { id, pid }, now);
                }
            }
        }
    }

    /// Arranges window `id`'s Desktop when the window waits on a Desktop not shown (for the
    /// model) and the window server lists it on the main screen with no window of that
    /// Desktop covering the display; the stage left is saved in the list's order. True when
    /// arranged.
    fn arrange_if_listed(&mut self, ob: &Observer, list: &[CgWindow], main: Rect, id: WindowId, trigger: Trigger, now: Instant) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        if !m.waits_on_another_space(id) {
            return false;
        }
        let Some(standard) = m.resolve_standard(id) else { return false };
        let target = m.space_of(standard);
        let elsewhere = |w: u32| m.space_of(WindowId(w)).is_some_and(|s| Some(s) != target);
        let shown = on_main_screen(list, main);
        if !listed_for_arrival(list, standard.0, &shown, main) || display_covered(list, main, elsewhere) {
            return false;
        }
        let obs = listed_order(list, |w| m.window(w).is_some());
        self.arrange_ahead(ob, standard, trigger, now, Some(obs))
    }

    /// Arranges window `id`'s Desktop for it ([`crate::engine::model::Model::arrange_ahead`]);
    /// on a Desktop not shown the windows it brings back stay registered until seen out of
    /// the park zone. True when arranged.
    fn arrange_ahead(&mut self, ob: &Observer, id: WindowId, trigger: Trigger, now: Instant, obs: Option<Observation>) -> bool {
        let focus_at = self.focus_replay.came(now).unwrap_or(now);
        let changes_at = self.space_changes;
        let Some(m) = self.model.as_mut() else { return false };
        let pid = m.window(id).map(|w| w.meta.pid).unwrap_or_default();
        let Some((ahead, actions)) = m.arrange_ahead(id, obs.as_ref()) else {
            tracing::debug!(id = id.0, trigger = trigger.name(), "nothing to arrange ahead for the window");
            return false;
        };
        let moves = actions.iter().filter(|a| !matches!(a, Action::StateChanged)).count();
        let expected = if trigger.away() { expects(&actions) } else { Vec::new() };
        if trigger.away() {
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
        self.ahead = Some(AheadSent { ahead, pid, trigger, focus_at, sent_at: sent, changes_at });
        if !expected.is_empty() {
            // The first look a fast tick later: right after the calls the window server's
            // list may not show them yet even when they took (R9 1).
            self.verify = Some(Verify { sent, next: sent + VERIFY_EVERY, rounds: 0, moves: expected });
        }
        true
    }

    /// True while the WM thread's fast tick has work: an arrangement's moves to check.
    pub fn wants_fast_tick(&self) -> bool {
        self.verify.is_some()
    }

    /// The WM thread's fast tick: the moves of an arrangement made at a Dock click checked
    /// when due ([`Self::check_arrangement`]).
    pub fn fast_tick(&mut self, ob: &Observer) {
        let now = Instant::now();
        if self.model.is_none() {
            self.verify = None;
        } else if self.verify.as_ref().is_some_and(|v| now >= v.next) {
            self.check_arrangement(ob, now);
        }
    }

    /// Ends the check of an arrangement's moves (`why`: it was confirmed, undone, the Desktop
    /// is arriving), logging what the window server showed by then.
    pub(super) fn finish_verify(&mut self, why: &str) {
        let Some(v) = self.verify.take() else { return };
        let resent: u32 = v.moves.iter().map(|e| e.tries).sum();
        tracing::info!(why, rounds = v.rounds, resent, after_ms = ms(v.sent, Instant::now()), "arrival: the check of the arrangement's moves ends");
    }

    /// Reads the window server's frames of an arrangement's moves ([`shows`]): those it does
    /// not show yet are sent again ([`Self::resend`]), every [`VERIFY_EVERY`] for
    /// [`VERIFY_FOR`] or until it shows them all; what it showed is logged. Never once its
    /// Desktop is arriving (a Space change settling: the user may hold its windows).
    fn check_arrangement(&mut self, ob: &Observer, now: Instant) {
        let live = self.ahead.as_ref().is_some_and(|s| s.trigger == Trigger::DockClick);
        if !live {
            self.finish_verify("the arrangement is settled");
            return;
        }
        if self.space_pending {
            self.finish_verify("its Desktop is arriving");
            return;
        }
        let Some(main) = self.model.as_ref().map(|m| *m.main_display()) else { return };
        let Some(v) = self.verify.as_mut() else { return };
        let list = windows::list_windows();
        let shown_by_server = |e: &Expect| {
            !e.nudged_only && list.iter().find(|w| w.id == e.id).is_some_and(|w| shows(e, w.bounds, |r| main.parks(r)))
        };
        let missing: Vec<usize> = (0..v.moves.len()).filter(|&i| !shown_by_server(&v.moves[i])).collect();
        let elapsed = ms(v.sent, now);
        let bounds: Vec<String> = missing
            .iter()
            .map(|&i| {
                let id = v.moves[i].id;
                format!("{id}: {}", list.iter().find(|w| w.id == id).map_or("unlisted".into(), |w| w.bounds.to_string()))
            })
            .collect();
        let sendable = missing.iter().any(|&i| !v.moves[i].gave_up && v.moves[i].tries < RESEND_MAX);
        if missing.is_empty() || !sendable || now.saturating_duration_since(v.sent) >= VERIFY_FOR {
            if missing.is_empty() {
                tracing::info!(moves = v.moves.len(), rounds = v.rounds, after_ms = elapsed, "arrival: the window server shows the arrangement");
            } else {
                tracing::info!(missing = ?bounds, rounds = v.rounds, after_ms = elapsed, "arrival: the window server never showed some moves of the arrangement");
            }
            self.verify = None;
            return;
        }
        tracing::info!(missing = ?bounds, round = v.rounds + 1, after_ms = elapsed, "arrival: the window server does not show every move yet; sending them again");
        v.rounds += 1;
        v.next = now + VERIFY_EVERY;
        let mut moves = std::mem::take(&mut v.moves);
        for i in missing {
            let e = &mut moves[i];
            if e.gave_up || e.tries >= RESEND_MAX {
                continue;
            }
            self.resend(ob, e, now);
        }
        if let Some(v) = self.verify.as_mut() {
            v.moves = moves;
        }
    }

    /// Sends move `e` again, bounded: once 1 pt off, then exactly (AppKit drops a move to
    /// where it thinks the window already is), or only exactly when the nudge went through
    /// last time and the exact move did not. Through the registry's element when the
    /// observer lost the window. A failure or a timeout gives the move up.
    fn resend(&mut self, ob: &Observer, e: &mut Expect, now: Instant) {
        let element = ob.window(e.id).map(|w| w.element.clone()).or_else(|| safety::registry_entry(e.id).map(|r| r.element));
        let Some(element) = element else {
            tracing::debug!(id = e.id, "arrival: a move to send again has no element; given up");
            e.gave_up = true;
            return;
        };
        e.tries += 1;
        self.echoes.touch_window(e.id, now);
        let (frame, park) = (e.frame, e.park);
        let nudged = Rect::new(frame.x + 1.0, frame.y, frame.w, frame.h);
        let send = |el: &AxElement, r: Rect| if park { el.set_position(r.origin()) } else { el.set_frame(r) };
        let skip_nudge = e.nudged_only;
        let (nudge, exact) = element.bounded(QUERY_AX_TIMEOUT_SECONDS, |el| {
            let nudge = if skip_nudge { Ok(()) } else { send(el, nudged) };
            let exact = if nudge.is_ok() { send(el, frame) } else { Ok(()) };
            (nudge, exact)
        });
        match (nudge, exact) {
            (Ok(()), Ok(())) => e.nudged_only = false,
            (Ok(()), Err(err)) => {
                tracing::debug!(id = e.id, "arrival: the exact move sent again failed; owed: {err}");
                e.nudged_only = true;
            }
            (Err(err), _) => {
                tracing::debug!(id = e.id, "arrival: a move sent again failed; given up: {err}");
                e.gave_up = true;
            }
        }
    }

    /// A left mouse-down at `p` (`list`: the window list read for it): over the Dock's bar,
    /// the application item there is hit-tested and its click's prediction worked out now
    /// (the app's state does not change during a press), kept for the mouse-up
    /// ([`Self::dock_released`]). Anywhere else this costs the bar test on `list` only.
    pub(super) fn dock_pressed(&mut self, ob: &Observer, list: &[CgWindow], p: crate::engine::geometry::Point, now: Instant) {
        self.dock_press = None;
        if self.model.is_none() || self.frozen() {
            return;
        }
        let Some(dock) = self.dock_pid(list) else { return };
        if !dock::dock_under(list, p, dock) {
            return;
        }
        match dock::item_at(dock, p) {
            Ok((_, item)) => self.press_item(ob, list, item, p, now),
            Err(miss) if miss.notable => {
                tracing::info!(x = p.x, y = p.y, found = miss.found, "dock: pressed, but the hit-test found no application item")
            }
            Err(miss) => tracing::debug!(x = p.x, y = p.y, found = miss.found, "dock: pressed on no item"),
        }
    }

    /// The Dock's pid: the one cached while it owns a window in `list`, else looked up (the
    /// Dock relaunched).
    fn dock_pid(&mut self, list: &[CgWindow]) -> Option<i32> {
        if let Some(pid) = self.dock_pid.filter(|pid| list.iter().any(|w| w.pid == *pid)) {
            return Some(pid);
        }
        self.dock_pid = dock::dock_pid();
        self.dock_pid
    }

    /// A press on `item` at `p`: its click's prediction, kept with the press.
    fn press_item(&mut self, ob: &Observer, list: &[CgWindow], item: DockItem, p: crate::engine::geometry::Point, now: Instant) {
        let prediction = self.dock_prediction(ob, list, &item);
        tracing::debug!(
            x = p.x,
            y = p.y,
            path = item.path,
            prediction = ?prediction.map(|(_, w)| w.0),
            took_ms = ms(now, Instant::now()),
            "dock: pressed"
        );
        self.dock_press = Some(DockPress { item, at: p, when: now, prediction });
    }

    /// A left mouse-up at `p` (`modifiers`: any of ⌘⌥⌃⇧ held): a click on the Dock item
    /// pressed has its prediction arranged ([`Self::dock_clicked`]).
    pub(super) fn dock_released(&mut self, ob: &Observer, p: crate::engine::geometry::Point, modifiers: bool, now: Instant) -> Option<WindowId> {
        let press = self.dock_press.take()?;
        if dock::is_click(&press, p, now, modifiers) {
            self.dock_clicked(ob, &press, now)
        } else {
            tracing::debug!(path = press.item.path, held_ms = ms(press.when, now), modifiers, "dock: a press, not a click; no prediction");
            None
        }
    }

    /// A click on a Dock item (`press`, let go at `released`): when macOS will take the user
    /// to another Desktop for it (the prediction made at the press), that Desktop is
    /// arranged now. Returns the window predicted.
    fn dock_clicked(&mut self, ob: &Observer, press: &DockPress<Prediction>, released: Instant) -> Option<WindowId> {
        let started = Instant::now();
        let why = match press.prediction {
            Ok((pid, window)) => {
                let regated = self.model.as_ref().map_or(Err("not managing"), |m| m.dock_gates(pid));
                let gone = self.model.as_ref().is_none_or(|m| m.window(window).is_none());
                if self.frozen() {
                    "a Space change is settling"
                } else if let Err(why) = regated {
                    why
                } else if gone {
                    "the window predicted at the press is gone"
                } else if let Some(sent) = &self.ahead {
                    if sent.ahead.window == window {
                        return Some(window);
                    }
                    "another arrangement stands"
                } else if self.arrange_ahead(ob, window, Trigger::DockClick, started, None) {
                    self.focus_replay.hold(Wanted::Window { id: window, pid }, started);
                    if let Some(space) = self.ahead_space() {
                        self.present_away(space);
                    }
                    let done = Instant::now();
                    tracing::info!(
                        path = press.item.path,
                        window = window.0,
                        since_click_ms = ms(press.when, done),
                        since_release_ms = ms(released, done),
                        "dock: the Desktop the click goes to is arranged"
                    );
                    return Some(window);
                } else {
                    "its stage is already the one shown there"
                }
            }
            Err(why) => why,
        };
        let done = Instant::now();
        tracing::info!(
            path = press.item.path,
            why,
            since_click_ms = ms(press.when, done),
            since_release_ms = ms(released, done),
            "dock: no prediction"
        );
        None
    }

    /// The app a Dock item is for and the window macOS will show for it on another
    /// Desktop, or why there is no prediction: the model's gates and the window list
    /// (`list`) first, the app asked (bounded) only when they pass.
    fn dock_prediction(&mut self, ob: &Observer, list: &[CgWindow], item: &DockItem) -> Prediction {
        let m = self.model.as_ref().ok_or("not managing")?;
        let bundle = dock::bundle_id_at(&item.path).ok_or("the item's bundle has no id")?;
        let pid = match ob.pids_with_bundle(&bundle)[..] {
            [] => return Err("the app is not running (or not managed)"),
            [pid] => pid,
            _ => return Err("several instances of the app run"),
        };
        m.dock_gates(pid)?;
        let main = *m.main_display();
        let visible_here = dock::visible_window_of(list, pid, |r| main.parks(r));
        let unplaced = self.unplaced.iter().any(|id| ob.window(*id).is_some_and(|w| w.pid == pid));
        // One bounded round with the app: hidden, active, its focused (else main) window.
        let app = AxElement::application(pid);
        let answer = app.bounded(QUERY_AX_TIMEOUT_SECONDS, |a| {
            let hidden = a.is_hidden();
            let active = a.is_frontmost();
            let focused = a.focused_window().ok().and_then(|w| w.window_id().ok());
            let main = a.main_window().ok().and_then(|w| w.window_id().ok());
            (hidden, active, focused, main)
        });
        let (hidden, active, focused, main_window) = answer;
        let Ok(hidden) = hidden else { return Err("the app did not answer in 50 ms (hidden or not)") };
        let active = active.unwrap_or(false);
        let space_of = |id: Option<u32>| id.and_then(|id| m.space_of(WindowId(id))).map(|s| s.0);
        tracing::info!(
            bundle,
            pid,
            active,
            hidden,
            ax_focused = ?focused,
            ax_focused_desktop = ?space_of(focused),
            ax_main = ?main_window,
            ax_main_desktop = ?space_of(main_window),
            desktop_shown = m.current_space(main.id).0,
            visible_here,
            unplaced,
            "dock: the app at the press"
        );
        let ax_window = focused.or(main_window).map(WindowId);
        let facts = AppFacts { hidden, unplaced, active, visible_here };
        let result = m.dock_arrival(pid, facts, ax_window);
        let predicted = result.ok().and_then(|w| m.space_of(w)).map(|s| s.0);
        self.dock_seen = Some(DockSeen { at: Instant::now(), pid, active, predicted });
        let window = result?;
        tracing::debug!(bundle, pid, window = window.0, "dock: the window macOS will show");
        Ok((pid, window))
    }

    /// A Space change after a Dock press: which Desktop came, against the prediction (the
    /// day's log tells whether the rule holds).
    pub(super) fn note_dock_arrival(&mut self, arrived: SpaceId, from: SpaceId) {
        let Some(seen) = self.dock_seen.take() else { return };
        if seen.at.elapsed() > DOCK_SEEN_FOR {
            return;
        }
        tracing::info!(
            pid = seen.pid,
            active = seen.active,
            predicted = ?seen.predicted,
            from = from.0,
            arrived = arrived.0,
            right = seen.predicted == Some(arrived.0),
            since_press_ms = ms(seen.at, Instant::now()),
            "dock: the Desktop a Dock click went to"
        );
    }

    /// Redraws the pane of `space`, a Desktop not shown (arranged ahead at a Dock click, or
    /// that arrangement undone), with its cards, so it slides in as it is.
    fn present_away(&mut self, space: SpaceId) {
        let Some(m) = self.model.as_ref() else { return };
        if m.current_space(m.main_display().id) == space {
            return;
        }
        strip_model::present(StripSnapshot {
            cards: cards_in(m, space),
            active_stage: m.active_stage_in(space).map(|s| s.0),
            style: self.style.clone(),
            space: space.0,
            away: true,
            ..StripSnapshot::default()
        });
    }

    /// A Desktop was recognized (`arrived`): an arrangement made ahead is confirmed (its
    /// windows raised, the window focused; the stage that was there back if the window is
    /// gone) or undone. The actions are for the arrival's batch, with the window the arrival
    /// counts as focused (the predicted one, on a hit: AX may still name the previous
    /// app's); an undo's restores (a Desktop not shown) stay registered until seen.
    pub(super) fn settle_ahead(&mut self, ob: &Observer, arrived: SpaceId, now: Instant) -> (Vec<Action>, Option<WindowId>) {
        let Some(sent) = self.ahead.take() else { return (Vec::new(), None) };
        let hit = sent.ahead.space == arrived;
        self.finish_verify(if hit { "the Desktop arranged came" } else { "another Desktop came" });
        let Some(m) = self.model.as_mut() else { return (Vec::new(), None) };
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
            if sent.trigger == Trigger::DockClick {
                // Its pane drew the prediction: it shows the Desktop as it is again.
                self.present_away(sent.ahead.space);
            }
        }
        (actions, (hit && present).then_some(sent.ahead.window))
    }

    /// Undoes an arrangement made ahead (`why`): the prediction no longer holds.
    pub(super) fn undo_ahead(&mut self, ob: &Observer, why: &str) {
        let Some(sent) = self.ahead.take() else { return };
        self.finish_verify("the arrangement is undone");
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
        if sent.trigger == Trigger::DockClick {
            // The focus held for the click was the prediction's: it goes with it.
            if matches!(self.focus_replay.peek(Instant::now()), Some(Wanted::Window { id, .. }) if id == sent.ahead.window) {
                self.focus_replay.clear();
            }
            self.present_away(sent.ahead.space);
        }
    }

    /// Forgets a held focus (the user acted on the Desktop shown) and undoes what was
    /// arranged ahead for it ([`forget_undoes`]).
    pub(super) fn forget_held(&mut self, ob: &Observer) {
        if !forget_undoes(self.space_pending) {
            return;
        }
        self.focus_replay.clear();
        if self.ahead.is_some() {
            self.undo_ahead(ob, "the user acted on the Desktop shown");
        }
    }

    /// About every tick: an arrangement whose Desktop did not come is undone ([`expire_step`]).
    pub(super) fn expire_ahead(&mut self, ob: &Observer, now: Instant) {
        let Some(sent) = &self.ahead else { return };
        let changed = self.space_changes != sent.changes_at;
        let waited = now.saturating_duration_since(sent.sent_at);
        if let Some(why) = expire_step(sent.trigger, self.space_pending, changed, waited, self.ahead_contradicted(now)) {
            self.undo_ahead(ob, why);
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

    /// `debug-dock-click BUNDLE` (test only): the Dock item of a `STAGE_MANAGER_ONLY` app is
    /// found (its frame, from the Dock's lists), then clicked the way a real click goes: the
    /// hit-test at its center ([`dock::item_at`]), the prediction at the press, the
    /// arrangement at the release; then the item the hit-test found is pressed (`AXPress`,
    /// which activates the app as a click does, the Dock auto-hidden or not).
    pub(super) fn debug_dock_click(&mut self, ob: &Observer, bundle: &str) -> serde_json::Value {
        use serde_json::json;
        let only = std::env::var("STAGE_MANAGER_ONLY").unwrap_or_default();
        if !only.split(',').any(|b| b.trim() == bundle) {
            return json!({ "ok": false, "error": format!("{bundle} is not on the STAGE_MANAGER_ONLY list") });
        }
        let list = windows::list_windows();
        let Some(dock) = self.dock_pid(&list) else { return json!({ "ok": false, "error": "the Dock does not run" }) };
        let Some((_, listed)) = dock::item_for_bundle(dock, bundle) else {
            return json!({ "ok": false, "error": format!("no Dock item for {bundle}") });
        };
        let center = |r: Rect| crate::engine::geometry::Point::new(r.x + r.w / 2.0, r.y + r.h / 2.0);
        let mut at = center(listed.frame);
        let mut hit = dock::item_at(dock, at);
        let main = self.model.as_ref().map(|m| m.main_display().frame);
        let mut revealed = false;
        if hit.is_err()
            && let Some(edge) = main.and_then(|d| dock_edge(listed.frame, d))
        {
            // An auto-hidden Dock: the pointer to its edge shows it (a move, no click), then
            // the item is looked up where it shows and hit-tested again.
            revealed = crate::platform::input::reveal_edge(edge, Duration::from_millis(900)).is_ok();
            if let Some((_, shown)) = dock::item_for_bundle(dock, bundle) {
                at = center(shown.frame);
                hit = dock::item_at(dock, at);
            }
        }
        let mut result = self.debug_dock_press(ob, &list, bundle, &listed, at, hit);
        if revealed {
            crate::platform::input::pointer_back();
        }
        if let Some(obj) = result.as_object_mut() {
            obj.insert("revealed_dock".into(), json!(revealed));
        }
        result
    }

    /// The rest of [`Self::debug_dock_click`]: the hit-test's answer `hit` at `at` goes
    /// through the press and release path, then the item is pressed.
    fn debug_dock_press(
        &mut self,
        ob: &Observer,
        list: &[CgWindow],
        bundle: &str,
        listed: &DockItem,
        at: crate::engine::geometry::Point,
        hit: Result<(AxElement, DockItem), dock::Miss>,
    ) -> serde_json::Value {
        use serde_json::json;
        let (element, item) = match hit {
            Ok(hit) if hit.1.path == listed.path => hit,
            Ok((_, other)) => return json!({ "ok": false, "error": format!("the hit-test at {at:?} found another item ({})", other.path) }),
            Err(miss) => return json!({ "ok": false, "error": format!("the hit-test at {at:?} found no application item: {}", miss.found) }),
        };
        let pressed_at = Instant::now();
        self.press_item(ob, list, item, at, pressed_at);
        let predicted = self.dock_released(ob, at, false, Instant::now());
        let pressed = element.bounded(QUERY_AX_TIMEOUT_SECONDS * 4.0, |e| e.perform_action(names::PRESS));
        tracing::info!(bundle, predicted = ?predicted.map(|w| w.0), pressed = pressed.is_ok(), "debug-dock-click");
        json!({ "ok": pressed.is_ok(), "predicted": predicted.map(|w| w.0), "error": pressed.err().map(|e| e.to_string()) })
    }
}

/// Where the pointer shows an auto-hidden Dock whose item sits at `item` (off the display
/// `display`): the middle of the display's edge the item is beyond, a point inside it.
fn dock_edge(item: Rect, display: Rect) -> Option<crate::engine::geometry::Point> {
    use crate::engine::geometry::Point;
    let (cx, cy) = (item.x + item.w / 2.0, item.y + item.h / 2.0);
    let x = cx.clamp(display.x + 1.0, display.x + display.w - 1.0);
    let y = cy.clamp(display.y + 1.0, display.y + display.h - 1.0);
    if cy >= display.y + display.h - 1.0 {
        Some(Point::new(x, display.y + display.h - 1.0))
    } else if cx <= display.x + 1.0 {
        Some(Point::new(display.x, y))
    } else if cx >= display.x + display.w - 1.0 {
        Some(Point::new(display.x + display.w - 1.0, y))
    } else {
        None
    }
}

/// App `pid`'s focused window, asked with a bounded timeout (the app goes back to the global
/// one after).
pub(super) fn focused_window_of(pid: i32) -> Option<WindowId> {
    AxElement::application(pid)
        .bounded(QUERY_AX_TIMEOUT_SECONDS, |app| app.focused_window().ok().and_then(|w| w.window_id().ok()))
        .map(WindowId)
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
        assert!(display_covered(&list, MAIN, |_| false), "a full-screen window or stand-in");
        assert!(!display_covered(&list[..2], MAIN, |_| false));
        assert!(!display_covered(&list, MAIN, |id| id == 3), "a zoomed window of the Desktop being left (R5 5)");
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
    fn a_focus_held_while_a_change_settles_is_arranged_for_unless_an_overview_shows() {
        // (contradicted, ahead, held window, pending, overview) -> (undo, arrange)
        assert_eq!(hold_step(false, false, true, true, false), (false, true), "settling: arranged");
        assert_eq!(hold_step(false, false, true, false, false), (false, false), "no change settling: at the notice");
        assert_eq!(hold_step(false, false, true, true, true), (false, false), "an overview: its list is not the Desktop's");
        assert_eq!(hold_step(false, false, false, true, false), (false, false), "an activation alone");
        assert_eq!(hold_step(false, true, true, true, false), (false, false), "the same window: the arrangement stands");
        assert_eq!(hold_step(true, true, true, true, false), (true, true), "another window: undone, then arranged for it");
        assert_eq!(hold_step(true, true, false, false, false), (true, false), "another app's activation: undone");
    }

    #[test]
    fn the_notice_undoes_an_arrangement_whose_window_is_not_listed() {
        assert_eq!(notice_step(false, Some(false)), NoticeStep::Undo, "another Desktop slid in (L3, every trigger)");
        assert_eq!(notice_step(false, Some(true)), NoticeStep::Keep);
        assert_eq!(notice_step(false, None), NoticeStep::Arrange);
        assert_eq!(notice_step(true, Some(false)), NoticeStep::Nothing, "an overview's list proves nothing");
    }

    #[test]
    fn nothing_acting_while_a_change_settles_keeps_the_arrangement() {
        // V4: refused hotkeys and strip input never reach `forget_held`; it keeps it anyway.
        assert!(!forget_undoes(true));
        assert!(forget_undoes(false));
    }

    #[test]
    fn a_dock_click_with_no_space_change_after_it_is_undone() {
        let ms = Duration::from_millis;
        let dock = Trigger::DockClick;
        assert_eq!(expire_step(dock, false, false, ms(400), false), None, "the slide is under way");
        assert!(expire_step(dock, false, false, DOCK_SWITCH_WAIT, false).is_some(), "no Space change in a second");
        assert_eq!(expire_step(dock, false, true, ms(1500), false), None, "the change came: the arrival decides");
        assert_eq!(expire_step(dock, true, false, ms(1500), true), None, "never while a change settles");
        assert!(expire_step(Trigger::Notice, false, true, ms(10), true).is_some(), "the hold is gone");
        assert_eq!(expire_step(Trigger::Notice, false, false, ms(5000), false), None);
    }

    #[test]
    fn another_window_of_the_predicted_app_beats_the_echo_or_retargets_it() {
        use crate::engine::model::StageId;
        let (b, x, y) = (WindowId(1), StageId(7), StageId(8));
        assert_eq!(echo_verdict(b, x, 10, Some(10), Some(WindowId(2)), Some(y)), EchoVerdict::Beats, "window C of another stage");
        assert_eq!(
            echo_verdict(b, x, 10, Some(10), Some(WindowId(2)), Some(x)),
            EchoVerdict::Retarget(WindowId(2)),
            "window C of the stage arranged: kept, C focused at the arrival (R7 1)"
        );
        assert_eq!(echo_verdict(b, x, 10, Some(10), Some(b), Some(x)), EchoVerdict::Echo, "B's own focus");
        assert_eq!(echo_verdict(b, x, 10, Some(20), Some(WindowId(5)), Some(y)), EchoVerdict::Echo, "another app's window");
        assert_eq!(echo_verdict(b, x, 10, Some(10), None, None), EchoVerdict::Echo, "an unknown window of the app");
    }

    #[test]
    fn an_auto_hidden_docks_edge_is_where_its_item_is_beyond_the_display() {
        use crate::engine::geometry::Point;
        let below = Rect::new(800.0, 1175.0, 60.0, 60.0);
        assert_eq!(dock_edge(below, MAIN), Some(Point::new(830.0, 1168.0)), "the bottom edge");
        let left = Rect::new(-70.0, 500.0, 60.0, 60.0);
        assert_eq!(dock_edge(left, MAIN), Some(Point::new(0.0, 530.0)), "the left edge");
        let right = Rect::new(1810.0, 500.0, 60.0, 60.0);
        assert_eq!(dock_edge(right, MAIN), Some(Point::new(1799.0, 530.0)), "the right edge");
        assert_eq!(dock_edge(Rect::new(800.0, 1100.0, 60.0, 60.0), MAIN), None, "on screen: nothing to reveal");
    }

    #[test]
    fn a_move_is_shown_when_the_window_server_lists_it_there() {
        let parks = |r: Rect| r.x >= 1700.0 && r.y >= 1100.0;
        let fresh = |id, frame, park| Expect { id, frame, park, tries: 0, gave_up: false, nudged_only: false };
        let park = fresh(1, Rect::new(1785.0, 1155.0, 600.0, 400.0), true);
        assert!(shows(&park, Rect::new(1786.0, 1150.0, 600.0, 400.0), parks), "anywhere in the park zone");
        assert!(!shows(&park, Rect::new(80.0, 78.0, 600.0, 400.0), parks), "still where it was");
        let back = fresh(2, Rect::new(80.0, 78.0, 520.0, 392.0), false);
        assert!(shows(&back, Rect::new(81.0, 78.0, 520.0, 392.0), parks));
        assert!(shows(&back, Rect::new(80.0, 78.0, 507.0, 388.0), parks), "a size the app chose (a terminal's grid) (R9 3)");
        assert!(!shows(&back, Rect::new(1785.0, 1155.0, 520.0, 392.0), parks), "still parked");
        let actions = [
            Action::Park { id: WindowId(1), frame: park.frame },
            Action::SetFrame { id: WindowId(2), frame: back.frame },
            Action::Raise { id: WindowId(2) },
            Action::StateChanged,
        ];
        assert_eq!(expects(&actions), vec![park, back], "moves only");
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
