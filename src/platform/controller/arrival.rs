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
//!   focus would be. No Space change within [`DOCK_SWITCH_CAP`] undoes it, whatever the
//!   engine. Under the
//!   Spaces engine the window waits off every Desktop, so its Desktop is arranged at the
//!   press instead; that arrangement stands until its click's own end: the release (the
//!   focus held then, nothing sent again), a press that is no click, or no Space change
//!   within the same cap. No tick, focus or activation in between undoes it
//!   ([`awaits_release`]). The click's activation is no word on it: it may come at the
//!   slide's start, before any window of the arriving Desktop is listed (round
//!   r2-lends-fix); one whose switch the model makes on the Desktop shown undoes it
//!   ([`Controller::forget_held`]).
//! - **At the notice**: an arrangement made at the click whose window the window server
//!   does not list on its screen now is undone (another Desktop slid in). With none,
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

use crate::platform::clock;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::slide_watch::LendNote;
use super::{Controller, cards_in};
use crate::engine::corner::park_frame;
use crate::engine::focus_replay::Wanted;
use crate::engine::fullscreen;
use crate::engine::lend_park;
use crate::engine::margin::{self, End};
use crate::engine::lend_wait::{Cap, LendWait, Outcome};
use crate::engine::geometry::Rect;
use crate::engine::model::{Action, Ahead, AheadMark, AppFacts, Display, DisplayId, Model, Observation, SpaceId, StageId, WindowId};
use crate::platform::ax::{AxElement, AxError, AxResult, names};
use crate::platform::dock::{self, DockItem, DockPress};
use crate::platform::enhanced_ui;
use crate::platform::observe::{Observer, WindowKind};
use crate::platform::safety::{self, ParkedEntry};
use crate::platform::spaces::ffi::ScreenDesks;
use crate::platform::test_press::{DOCK_PRESS_HOLD, dock_edge};
use crate::platform::windows::{self, CgWindow};
use crate::ui::strip_model::{self, StripSnapshot};

/// The Accessibility timeout for asking an app its focused window on the way to a Desktop
/// (at the notice, at a Dock click): an app busy with the switch must not hold it up. The
/// app goes back to the global timeout right after ([`AxElement::bounded`]).
const QUERY_AX_TIMEOUT_SECONDS: f32 = 0.05;


/// The cap on an arrangement made ahead on a Desktop not shown, with no Space change yet
/// ([`Trigger::away`]: at a Dock click, or for a focus the engine hid), whatever the engine
/// (round r2-lends for the Spaces engine's Dock click; every engine and trigger since round
/// arranged-ahead: what the cap waits for, a busy Mac's late Space change notice, is not the
/// engine's):
/// the Space change notice confirms it; a press that is no click refutes it at the release,
/// and the click's activation switching a stage on the Desktop shown, or the user acting
/// there, undoes it ([`Controller::forget_held`]). The activation alone refutes nothing:
/// round r2-lends took one with no Space change and none of the arriving Desktop's windows
/// listed for a click that went nowhere, but a Dock item's activation may come at the slide's
/// start (the live check of 11-21: handled ~30 ms before Desktop 1's windows were listed, the
/// notice ~300 ms after), and the arrangement undone there and made again for the hidden
/// focus a moment later showed the stage left as the Desktop slid in (round r2-lends-fix).
/// Time is left for a click with no notice; the arrangement is on a Desktop not shown
/// meanwhile. As long as the click's focus is held
/// ([`crate::engine::focus_replay::FOCUS_REPLAY_MAX`]), past which nothing held says it any
/// more. The 1 s it replaces (`DOCK_SWITCH_WAIT`, sized for a quiet Mac's notice 0.35–0.45 s
/// after the click) undid the arrangement before a busy Mac's notice, and the arrival
/// arranged the Desktop again in view (on the bench under the Corner engine: an app 0.4 s
/// slow to take the activation at load 3). Under the Corner engine the arrangement is real
/// moves of windows on the Desktop not shown: a click that goes nowhere leaves them 2 s
/// longer where nobody sees them, and the undo puts each back where it was.
pub const DOCK_SWITCH_CAP: Duration = Duration::from_millis(3000);

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
    /// At an activation of an app whose focused window the engine hides off its Desktop
    /// ([`crate::platform::window_engine::WindowEngine::off_every_desktop`]): macOS does
    /// not go to a Desktop for a window it does not count there, so the Desktop is arranged
    /// and the window focused, and macOS goes ([`Controller::arrange_for_hidden_focus`]).
    Hidden,
}

impl Trigger {
    fn name(self) -> &'static str {
        match self {
            Trigger::DockClick => "dock-click",
            Trigger::Notice => "notice",
            Trigger::NoticeAx => "notice-ax",
            Trigger::Settle => "settle",
            Trigger::Hidden => "hidden-focus",
        }
    }

    /// Arranged while its Desktop was not shown (its restores registered until seen, its
    /// moves checked while the Desktop slides in, undone when no Space change comes).
    fn away(self) -> bool {
        matches!(self, Trigger::DockClick | Trigger::Hidden)
    }
}

/// How long the moves of an arrangement made at a Dock click are checked against the window
/// server and sent again: the slide (~0.25–0.45 s) and a margin. What is still not taken
/// then is checked once more when the Desktop arrives ([`Controller::settle_ahead`]).
pub const VERIFY_FOR: Duration = Duration::from_millis(600);

/// How often they are checked (the WM thread's fast tick).
pub const VERIFY_EVERY: Duration = Duration::from_millis(20);

/// A move is sent again no sooner than this after its last resend, unless the window
/// server's listing of the window changed since: the budget lasts to the slide's end.
pub const RESEND_SPACING: Duration = Duration::from_millis(100);

/// One round's resends are held to this together (the WM thread's other work waits on
/// them): what is left goes in the next round.
pub const RESEND_ROUND_CAP: Duration = Duration::from_millis(100);

/// A move is sent only with this much of its round left: about the full timeout of its two
/// calls (the nudge and the exact move, 2 × 50 ms), less a slack so the round's first move
/// always goes. One with less left waits for the next round, not counted (R13 2).
pub const MOVE_NEEDS: Duration = Duration::from_millis(90);

/// After the predicted Desktop's arrival, its moves are looked at this much longer while
/// the list still shows the slide (the arrival reads it ~60 ms after the notice, maybe
/// mid-slide) or a move's window is not listed; then what is not taken goes once more.
pub const ARRIVAL_CHECK_FOR: Duration = Duration::from_millis(400);

/// When [`ARRIVAL_CHECK_FOR`] is up, the arrival's check goes on one round a tick while moves
/// the round's cap deferred or cut, or moves owed their exact frame, are left: for at most
/// this many ticks more…
pub const TAIL_OVER_TICKS: u32 = 10;

/// …and at most this long (room for every counted tick of a slow app: the round's cap plus a
/// tick each); then what stayed unsent is logged at warn (R15 1).
pub const TAIL_OVER_FOR: Duration = Duration::from_millis(1300);

/// One move of an arrangement made on a Desktop not shown, as the window server should show
/// it: parked (in the park zone), or at a frame; with how often it was sent again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Expect {
    id: u32,
    frame: Rect,
    /// The window's frame before the move (the model's), where a park not taken leaves it.
    before: Option<Rect>,
    park: bool,
    /// Times sent again (at most [`RESEND_MAX`]).
    tries: u32,
    /// A resend timed out or failed: not sent again while the slide runs.
    gave_up: bool,
    /// The 1 pt nudge went through, the exact move did not: the exact one is owed.
    nudged_only: bool,
    /// When it was last sent again, with the window server's bounds for it then.
    last: Option<(Instant, Option<Rect>)>,
    /// Sent again by the arrival's check (once), by a call the round's cap did not cut.
    final_sent: bool,
    /// Given its one exact move at the arrival's time-up ([`owed_exact`], R15 2).
    exact_last: bool,
    /// The window's stage as the arrangement left it (`None`: pinned, or not known): a move
    /// is checked only while the model still has the window there, parked as `park` says
    /// ([`Model::move_holds`]).
    stage: Option<StageId>,
}

impl Expect {
    fn new(id: u32, frame: Rect, before: Option<Rect>, park: bool) -> Self {
        Expect { id, frame, before, park, tries: 0, gave_up: false, nudged_only: false, last: None, final_sent: false, exact_last: false, stage: None }
    }
}

/// A move is sent again at most this many times while the slide runs (the measured case
/// needed one, when the Desktop showed); the arrival's final check is on top.
pub const RESEND_MAX: u32 = 3;

/// The moves of an arrangement made on a Desktop not shown, until the window server shows
/// them (measured 10-13: in one run of three it applied them only when that Desktop became
/// the shown one, at the end of the slide; the stage left showed all through it). Checked
/// on the fast tick while `polling`; kept after for the arrival's final check.
#[derive(Debug, Clone)]
pub(super) struct Verify {
    sent: Instant,
    next: Instant,
    rounds: u32,
    /// Rounds skipped because the list showed the Desktop sliding.
    slid: u32,
    /// The fast tick still checks them.
    polling: bool,
    /// A round found moves not taken and said so at info (the next ones at debug).
    told: bool,
    /// When the predicted Desktop arrived, and until when the arrival's check looks
    /// ([`ARRIVAL_CHECK_FOR`], [`Controller::arrival_round`]).
    tail: Option<(Instant, Instant)>,
    /// The arrival's check's ticks since its time was up, this one counted
    /// ([`tail_past_ceiling`], R15 1).
    over_ticks: u32,
    /// When the first counted tick ran: the time ceiling counts from it, not from the time
    /// up, so a late first tick (a full round just before) costs no later tick (R15 3).
    over_since: Option<Instant>,
    /// The stage arranged on its Desktop: the check ends once it is not the active one there
    /// or a switch touched it ([`Model::ahead_holds`], R13 1).
    mark: AheadMark,
    moves: Vec<Expect>,
}

/// What the window server's listing says of one move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    /// Not where it was before the move: taken (a restore out of the park zone, the size the
    /// app's; a park away from its old frame).
    Taken,
    /// Still as before the move: a restore in the park zone, a park at its old frame, or
    /// the window not listed.
    Missing,
    /// A park listed neither near its frame before the move nor in the park zone (macOS
    /// clamped it short, or the list is stale): sent once more ([`owed`]), not counted taken.
    Unsure,
    /// The Desktop slides: the list shows its windows shifted by up to the display's width
    /// (live 10-13: x 2740, 2457, 1746, 1203); nothing can be judged.
    Sliding,
}

/// True when `bounds` is `reference` moved sideways only, by more than 2 pt and at most
/// about the display's width (`width`): a Desktop slide's offset.
pub(crate) fn slid_from(bounds: Rect, reference: Rect, width: f64) -> bool {
    let dx = (bounds.x - reference.x).abs();
    (bounds.y - reference.y).abs() <= 2.0 && dx > 2.0 && dx <= width * 1.05
}

/// What the window server's `bounds` for move `e` say, read on a list not shifted by a
/// slide: a restore is taken out of the park zone (where exactly, and the size, are the
/// app's: a terminal's grid, a fixed-size window); a park in the park zone, missing near its
/// frame before the move (or anywhere out of the zone when that frame is not known), unsure
/// elsewhere. A move owed its exact frame after a nudge is missing.
fn state(e: &Expect, bounds: Rect, parks: impl Fn(Rect) -> bool) -> Seen {
    let near = |r: Rect| (bounds.x - r.x).abs() <= 2.0 && (bounds.y - r.y).abs() <= 2.0;
    let taken = |yes: bool| if yes { Seen::Taken } else { Seen::Missing };
    if e.nudged_only {
        Seen::Missing
    } else if !e.park {
        taken(!parks(bounds))
    } else if parks(bounds) {
        Seen::Taken
    } else {
        match e.before {
            Some(before) if !near(before) => Seen::Unsure,
            _ => Seen::Missing,
        }
    }
}

/// True when move `e`, judged `seen`, is still to be sent: missing, or unsure and not sent
/// again yet (once).
fn owed(e: &Expect, seen: Seen) -> bool {
    match seen {
        Seen::Missing => true,
        Seen::Unsure => e.tries == 0,
        Seen::Taken | Seen::Sliding => false,
    }
}

/// The window server's word on move `e` (`listed`: its window in the list, if any; `display`:
/// the main display's frame). A window listed on screen off the display, or shifted sideways
/// from where the move put it or from where it was, is caught mid-slide.
fn judge(e: &Expect, listed: Option<&CgWindow>, display: Rect, parks: impl Fn(Rect) -> bool) -> Seen {
    let Some(w) = listed else { return Seen::Missing };
    let b = w.bounds;
    let off_display = w.onscreen && w.layer == 0 && display.intersect(&b).is_none();
    if off_display || slid_from(b, e.frame, display.w) || e.before.is_some_and(|r| slid_from(b, r, display.w)) {
        Seen::Sliding
    } else {
        state(e, b, parks)
    }
}

/// What the arrival's check does with a listing ([`tail_step`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum TailStep {
    /// The list still shows the slide, or a move's window is not listed: look again.
    Wait,
    /// Send these moves again (the list settled, or the check's time is up; none: done).
    Send(Vec<usize>),
}

/// The arrival's check of `moves` on a listing (`seen`: each one judged, `None` when its
/// window is not listed); `over`: [`ARRIVAL_CHECK_FOR`] has run out. While the list shows
/// the slide or a window is missing it waits; then every move not taken and not sent by this
/// check yet goes (an unsure one only if never sent again; an unlisted one not: gone).
fn tail_step(moves: &[Expect], seen: &[Option<Seen>], over: bool) -> TailStep {
    if !over && seen.iter().any(|s| matches!(s, None | Some(Seen::Sliding))) {
        return TailStep::Wait;
    }
    let due = |i: usize| match seen[i] {
        None | Some(Seen::Taken) => false,
        Some(Seen::Missing | Seen::Sliding) => true,
        Some(Seen::Unsure) => moves[i].tries == 0,
    };
    TailStep::Send((0..moves.len()).filter(|&i| !moves[i].final_sent && due(i)).collect())
}

/// Sends the moves `due` in order (`send`: false when the round's time is up and nothing was
/// sent); returns the ones left for the next round.
fn send_round(due: &[usize], mut send: impl FnMut(usize) -> bool) -> Vec<usize> {
    for (k, &i) in due.iter().enumerate() {
        if !send(i) {
            return due[k..].to_vec();
        }
    }
    Vec::new()
}

/// True when the arrival's check, its time up, may not go on: past its `over_ticks`-th tick
/// since (the first counted 1, then [`TAIL_OVER_TICKS`] more) or [`TAIL_OVER_FOR`] past the
/// first counted tick.
fn tail_past_ceiling(over_ticks: u32, past: Duration) -> bool {
    // The time-up round itself always goes; a slow app's rounds take their cap plus a tick
    // each, so the time ceiling leaves room for every counted tick (R15 review).
    over_ticks > 1 && (over_ticks > TAIL_OVER_TICKS + 1 || past >= TAIL_OVER_FOR)
}

/// How a tick of the arrival's check ends ([`tail_round`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum TailEnd {
    /// Moves left: deferred or cut by the round's cap, or owed their exact frame at time-up.
    NextTick,
    /// Nothing left: the check ends.
    Done,
    /// Its ceiling reached with these moves still to send: the check ends (logged at warn).
    Ceiling(Vec<usize>),
}

/// What a tick of the arrival's check did ([`tail_round`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TailRound {
    /// The moves due the round's cap deferred (not sent).
    left: Vec<usize>,
    /// The moves due whose exact move the cap cut (sent again next tick).
    cut: usize,
    /// The move given its owed exact frame this tick, at time-up.
    exact: Option<usize>,
    end: TailEnd,
}

/// One tick of the arrival's check on `moves` (`due`: [`tail_step`]'s; `seen`: each judged;
/// `over`: [`ARRIVAL_CHECK_FOR`] is up; `ceiling`: [`tail_past_ceiling`]). `resend` sends a
/// move within the tick's round ([`Controller::resend`]). The due moves go first, as many as
/// the round's cap lets ([`send_round`]); those it deferred or cut go next tick, the time up
/// or not (R15 1). Before the time is up the check then ends; once it is, a move still owed
/// its exact frame ([`owed_exact`]) gets one exact move on its full timeout, on a tick with
/// nothing else to send and one a tick (R15 2). Past the ceiling nothing is sent: what is
/// left ends it.
fn tail_round(moves: &mut [Expect], due: &[usize], seen: &[Option<Seen>], over: bool, ceiling: bool, mut resend: impl FnMut(&mut Expect) -> Resent) -> TailRound {
    let pending_exact = |moves: &[Expect]| -> Vec<usize> { owed_exact(moves, seen).into_iter().filter(|&i| !moves[i].exact_last).collect() };
    if over && ceiling {
        let mut unsent = due.to_vec();
        unsent.extend(pending_exact(moves).into_iter().filter(|i| !due.contains(i)));
        let end = if unsent.is_empty() { TailEnd::Done } else { TailEnd::Ceiling(unsent) };
        return TailRound { left: Vec::new(), cut: 0, exact: None, end };
    }
    let left = send_round(due, |i| {
        let e = &mut moves[i];
        e.gave_up = false;
        let resent = resend(e);
        // Only a send the round's cap did not cut is the final one (R13 2).
        e.final_sent |= resent == Resent::Full;
        resent != Resent::Deferred
    });
    let cut = due.iter().filter(|&&i| !moves[i].final_sent).count() - left.len();
    let round = |end| TailRound { left: left.clone(), cut, exact: None, end };
    if cut > 0 || !left.is_empty() {
        return round(TailEnd::NextTick);
    }
    if !over {
        return round(TailEnd::Done);
    }
    let owed = pending_exact(moves);
    let Some(&i) = owed.first() else { return round(TailEnd::Done) };
    if !due.is_empty() {
        // This tick's round sent moves: the exact one waits for a round of its own.
        return round(TailEnd::NextTick);
    }
    let e = &mut moves[i];
    let resent = resend(e);
    e.exact_last = resent != Resent::Deferred;
    e.final_sent |= resent == Resent::Full;
    let end = if owed.len() > 1 { TailEnd::NextTick } else { TailEnd::Done };
    TailRound { left: Vec::new(), cut: 0, exact: Some(i), end }
}

/// Forgets the move of window `id` (the user acted on it: nothing sends it again under
/// them). True when there was one.
fn drop_move(moves: &mut Vec<Expect>, id: u32) -> bool {
    let n = moves.len();
    moves.retain(|e| e.id != id);
    moves.len() < n
}

/// What is left of an arrangement's moves to check against the model (R13 1): none once the
/// stage arranged is no longer the active one of its Desktop or a switch touched it (`Err`:
/// why the check ends); else each move whose window the model no longer has as the move
/// left it (closed, parked or shown since, gone to another stage) is dropped (`Ok`: their
/// ids), and with none left the check ends too.
fn screen(m: &Model, mark: &AheadMark, moves: &mut Vec<Expect>) -> Result<Vec<u32>, &'static str> {
    if !m.ahead_holds(mark) {
        return Err("the stage arranged is no longer the one active on its Desktop (a switch, an undo)");
    }
    let mut dropped = Vec::new();
    moves.retain(|e| {
        let holds = m.move_holds(WindowId(e.id), e.park, e.stage);
        if !holds {
            dropped.push(e.id);
        }
        holds
    });
    if moves.is_empty() {
        return Err("no move made ahead is as the model has it any more");
    }
    Ok(dropped)
}

/// Whether the arrival's check runs when the predicted Desktop arrives (`hit`): only when the
/// stage arranged is still the window's (`present`). Without its window the arrival's batch
/// brings back the stage that was there: the moves made ahead are its to undo, never sent
/// again (R13 1). `Err`: why the check ends.
fn arrival_check(hit: bool, present: bool) -> Result<(), &'static str> {
    match (hit, present) {
        (true, true) => Ok(()),
        (true, false) => Err("the Desktop arranged ahead came without its window; the stage that was there comes back"),
        (false, _) => Err("another Desktop came"),
    }
}

/// Where the model left each managed window before an arrangement (a parked window's park
/// frame, [`Model::frame_left`]; never its home frame, which a parked window's `meta.frame`
/// still holds), read before the model arranges it.
fn frames_left(m: &Model) -> HashMap<u32, Rect> {
    m.windows().filter_map(|w| m.frame_left(w.meta.id).map(|r| (w.meta.id.0, r))).collect()
}

/// Every window's frame before an arrangement's moves, read before they go out: the window
/// server's bounds where `listed` (the windows the moves touch, described) has it, else
/// where the model left it (`left`, [`frames_left`]).
fn frames_before(left: &HashMap<u32, Rect>, listed: &[CgWindow]) -> HashMap<u32, Rect> {
    let mut before = left.clone();
    before.extend(listed.iter().map(|c| (c.id, c.bounds)));
    before
}

/// The windows `actions` move (park or put at a frame).
fn moved_ids(actions: &[Action]) -> Vec<u32> {
    actions
        .iter()
        .filter_map(|a| match *a {
            Action::Hide { id } | Action::SetFrame { id, .. } => Some(id.0),
            _ => None,
        })
        .collect()
}

/// True when move `e`, not taken, may be sent again now (`bounds`: the window server's for it
/// now): its budget not spent, and its listing changed since its last resend or that was
/// [`RESEND_SPACING`] ago.
fn resend_due(e: &Expect, bounds: Option<Rect>, now: Instant) -> bool {
    if e.gave_up || e.tries >= RESEND_MAX {
        return false;
    }
    match e.last {
        None => true,
        Some((at, seen)) => seen != bounds || now.saturating_duration_since(at) >= RESEND_SPACING,
    }
}

/// The moves among `actions` to check against the window server, with each window's frame
/// before them (`before`) and its stage after them (`stage`). A hide is checked against the
/// frame it parks its window at (`park`, [`crate::engine::corner::park_frame`]).
fn expects(
    actions: &[Action],
    park: impl Fn(u32) -> Option<Rect>,
    before: impl Fn(u32) -> Option<Rect>,
    stage: impl Fn(u32) -> Option<StageId>,
) -> Vec<Expect> {
    actions
        .iter()
        .filter_map(|a| match *a {
            Action::Hide { id } => Some(Expect { stage: stage(id.0), ..Expect::new(id.0, park(id.0)?, before(id.0), true) }),
            Action::SetFrame { id, frame } => Some(Expect { stage: stage(id.0), ..Expect::new(id.0, frame, before(id.0), false) }),
            _ => None,
        })
        .collect()
}

/// What one resend did ([`Controller::resend`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resent {
    /// Nothing sent: less than [`MOVE_NEEDS`] was left of the round (not counted).
    Deferred,
    /// Sent, every call with its full timeout (or answered): its outcome is the app's.
    Full,
    /// The nudge went through, the exact move was cut by the round's cap (not asked, or
    /// timed out on less than its full timeout): the exact move is owed, sent again.
    Cut,
}

/// True when a move may go `elapsed` into its round: [`MOVE_NEEDS`] is left of
/// [`RESEND_ROUND_CAP`].
fn move_fits(elapsed: Duration) -> bool {
    RESEND_ROUND_CAP.saturating_sub(elapsed) >= MOVE_NEEDS
}

/// Records what a resend of `e` did (`nudge`, `exact`: the calls' answers; `exact_full`: the
/// exact move had its full timeout): both through, nothing owed; the nudge only, the exact
/// move owed (cut by the cap: [`Resent::Cut`]); the nudge failed (always on its full
/// timeout), given up until the arrival's check.
fn record_resend(e: &mut Expect, nudge: AxResult<()>, exact: AxResult<()>, exact_full: bool) -> Resent {
    match (nudge, exact) {
        (Ok(()), Ok(())) => {
            e.nudged_only = false;
            Resent::Full
        }
        (Ok(()), Err(err)) => {
            e.nudged_only = true;
            if exact_full {
                tracing::debug!(id = e.id, "arrival: the exact move sent again failed; owed: {err}");
                Resent::Full
            } else {
                tracing::debug!(id = e.id, "arrival: the round's cap cut the exact move; sent again: {err}");
                Resent::Cut
            }
        }
        (Err(err), _) => {
            tracing::debug!(id = e.id, "arrival: a move sent again failed; given up: {err}");
            e.gave_up = true;
            Resent::Full
        }
    }
}

/// The moves of the arrival's check still owed their exact frame when its time is up (the
/// nudge through, the exact move not; their windows listed in `seen`): each gets one more
/// exact move on its full timeout.
fn owed_exact(moves: &[Expect], seen: &[Option<Seen>]) -> Vec<usize> {
    (0..moves.len()).filter(|&i| moves[i].nudged_only && !moves[i].gave_up && seen[i].is_some()).collect()
}

/// A short form of where the window server lists window `id` (`unlisted`: not at all).
fn listed_at(list: &[CgWindow], id: u32) -> String {
    format!("{id}: {}", list.iter().find(|w| w.id == id).map_or("unlisted".into(), |w| w.bounds.to_string()))
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

/// A window put on the Desktop shown at a Dock press, or while ⌘Tab's switcher selects its
/// app, still counted hidden, so macOS activates its app here
/// ([`crate::engine::model::Model::dock_home`]; the Spaces engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Lent {
    pub(super) window: WindowId,
    pub(super) pid: i32,
    /// When it was put home.
    pub(super) at: Instant,
    /// Put home for ⌘Tab's switcher (round cmd-tab, [`super::switcher`]); else at a Dock press.
    pub(super) switcher: bool,
    /// What ends it ([`LendWait`]): its app's activation and switch confirm it; another app's
    /// activation, a press that was no click, the next press, or the switcher's cancel refute
    /// it; its cap ends the rest. A Dock press's begins at the lend ([`LEND_WAIT`]); the
    /// switcher's is held while the switcher is up and begins at its end
    /// ([`crate::engine::switcher::activation_cap`]).
    pub(super) wait: LendWait,
}

/// The cap on a window put home at a Dock press: its stage not in front this long after the
/// press with no word either way (no activation of its app, no other app's, no other press),
/// it goes back into its stage Space. The click's activation comes ~100 ms after the press
/// and its switch at once, later on a busy Mac; its activation and switch end the wait, as
/// do another app's activation, a press that was no click and the next press, so this ends
/// only a click whose activation never comes (the app hung). The window waits parked off the
/// screen meanwhile (a sliver at its display's corner at most), so a long cap costs nothing
/// seen, where the 1 s wait it replaces took the window back before a busy app's activation
/// and macOS slid to the app's other Desktop, the slide the lend is for.
pub const LEND_WAIT: Duration = Duration::from_millis(5000);

/// [`LEND_WAIT`] as a [`Cap`].
pub(super) const LEND_CAP: Cap = Cap { name: "LEND_WAIT", limit: LEND_WAIT };

/// The look at a lend: `parked` while the model counts its window hidden, `in_front` once its
/// stage is the active one of its Desktop (the switch shows it: met); else its cap.
pub(super) fn lent_step(wait: &mut LendWait, parked: bool, in_front: bool, now: Instant) -> Option<Outcome> {
    if !parked || in_front { wait.shown(now) } else { wait.step(now) }
}

/// True when a Dock press takes back the window put home now (`lent`): always one put home
/// for an earlier press; one put home for ⌘Tab's switcher (`for_switcher`) only when this
/// press puts another window home (`lends_another`: one lend is kept at a time). A press that
/// was no click, or one kept from a missed release, never undoes the switcher's (its app's
/// activation after ⌘'s release would then go to its other Desktop, review B-6).
pub(super) fn dock_takes_back(for_switcher: bool, lends_another: bool) -> bool {
    !for_switcher || lends_another
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
    /// Made at the Dock press, before the release (the Spaces engine: the window waited off
    /// every Desktop, and the Dock acts at the release).
    at_press: bool,
}

/// Milliseconds from `then` to `now`.
fn ms(then: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(then).as_millis() as u64
}

/// The latest user input this recent counts as what started an arrival (a Dock click's
/// mouse-up, ⌘Tab's key): its timing is logged from there.
const INPUT_RECENT: Duration = Duration::from_secs(3);

/// True when window `id` is listed on a managed screen (`shown_main`:
/// [`Controller::shown_managed`]) at layer 0 and does not cover its own display (`main`:
/// that display's frame): its Desktop is the one shown or sliding in, not a full-screen
/// Space.
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

/// True while an arrangement made at a Dock press (`at_press`) waits for its click's release
/// (`press_down`: the press is still the one kept, no mouse-up yet). Its focus is held only at
/// the release, so until then nothing held says anything about it: it ends at the release (a
/// click holds its focus, a press that is no click undoes it), or with no Space change
/// [`DOCK_SWITCH_CAP`] after it (a release the monitor missed).
fn awaits_release(at_press: bool, press_down: bool) -> bool {
    at_press && press_down
}

/// [`contradicts`] for a standing arrangement, but never while it waits for its click's
/// release ([`awaits_release`]): read at a tick between a Dock press and its release, the
/// press's arrangement (no focus held yet) was undone 1 ms after it went out, and the release
/// arranged it again too late for macOS (live 11-15 09:53:19, Spaces, landing=live).
fn ahead_contradicted_by(
    held: Option<Wanted>,
    window: WindowId,
    pid: i32,
    at_press: bool,
    press_down: bool,
    resolve: impl Fn(WindowId) -> Option<WindowId>,
) -> bool {
    !awaits_release(at_press, press_down) && contradicts(held, window, pid, resolve)
}

/// What a Dock click's release does with the arrangement standing (`standing`: the window it
/// stands for and whether it was made at the press) for the window predicted at the press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseStep {
    /// Made at this press for this window: it stands, its focus is held now, nothing is sent
    /// again.
    HoldPress,
    /// Made earlier for this window (not at this press): it stands as it is.
    Stands,
    /// Another arrangement stands: no prediction.
    Another,
    /// None stands: the Desktop is arranged now.
    Arrange,
}

fn release_step(standing: Option<(WindowId, bool)>, window: WindowId) -> ReleaseStep {
    match standing {
        Some((w, true)) if w == window => ReleaseStep::HoldPress,
        Some((w, false)) if w == window => ReleaseStep::Stands,
        Some(_) => ReleaseStep::Another,
        None => ReleaseStep::Arrange,
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
    /// An arrangement stands and its window is listed on its screen.
    Keep,
    /// An arrangement stands and its window is not listed: another Desktop slid in. Undone,
    /// then as [`NoticeStep::Arrange`].
    Undo,
    /// None stands: look for one to make.
    Arrange,
}

/// The notice's step: `overview` shows, `ahead_listed` is whether a standing arrangement's
/// window is listed on its screen (`None`: none stands).
fn notice_step(overview: bool, ahead_listed: Option<bool>) -> NoticeStep {
    match (overview, ahead_listed) {
        (true, _) => NoticeStep::Nothing,
        (false, Some(true)) => NoticeStep::Keep,
        (false, Some(false)) => NoticeStep::Undo,
        (false, None) => NoticeStep::Arrange,
    }
}

/// Why an arrangement is undone at a tick, if it is: never while a Space change settles; one
/// made on a Desktop not shown with no Space change `cap` after it ([`DOCK_SWITCH_CAP`]);
/// any other whose focus is no longer held (or what is held says otherwise).
fn expire_step(trigger: Trigger, pending: bool, changed: bool, waited: Duration, cap: Duration, contradicted: bool) -> Option<&'static str> {
    if pending {
        None
    } else if trigger.away() && !changed && waited >= cap {
        Some("no Space change came after the Dock click (or the focus it was arranged for)")
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
/// settles (nobody acts on the Desktop shown then; stage commands are refused), nor while an
/// arrangement made at a Dock press waits for its release (`awaiting`, [`awaits_release`]:
/// the user's word is the click, still under way).
fn forget_undoes(pending: bool, awaiting: bool) -> bool {
    !pending && !awaiting
}

/// The press's whole round with the app is held to this ([`press_round`]): a mouse-down on
/// the WM thread, before the click's prediction.
const PRESS_ROUND_CAP: Duration = Duration::from_millis(100);

/// The timeout for a round's next question, `elapsed` into a round held to `cap`
/// ([`PRESS_ROUND_CAP`], [`RESEND_ROUND_CAP`]): at most [`QUERY_AX_TIMEOUT_SECONDS`], at most
/// what is left; `None` once less than 5 ms is left (the round stops).
fn round_timeout(cap: Duration, elapsed: Duration) -> Option<f32> {
    let left = cap.saturating_sub(elapsed);
    (left >= Duration::from_millis(5)).then(|| left.as_secs_f32().min(QUERY_AX_TIMEOUT_SECONDS))
}

/// A window element's `CGWindowID` asked within `seconds` on that element too (its app's
/// bounded timeout does not reach it); `None`: the round's time is up, not asked.
fn window_id_within(w: &AxElement, seconds: Option<f32>) -> AxResult<u32> {
    match seconds {
        Some(seconds) => w.window_id_within(seconds),
        None => Err(AxError::CANNOT_COMPLETE),
    }
}

/// What the app said at a Dock press (`None`: not asked, or no answer).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PressAnswer {
    hidden: Option<bool>,
    active: Option<bool>,
    focused: Option<u32>,
    main: Option<u32>,
}

/// The press's round with the app, in order: hidden, active, its focused window, its main
/// window only when it has no focused one (no value). It stops at the first failure (a
/// timeout: the app is busy) and when `over` says the round's time is up (`over` is asked
/// before every question after the first and sets the next one's timeout).
fn press_round(
    hidden: impl FnOnce() -> AxResult<bool>,
    active: impl FnOnce() -> AxResult<bool>,
    focused: impl FnOnce() -> AxResult<u32>,
    main: impl FnOnce() -> AxResult<u32>,
    mut over: impl FnMut() -> bool,
) -> PressAnswer {
    let mut a = PressAnswer::default();
    let Ok(h) = hidden() else { return a };
    a.hidden = Some(h);
    if over() {
        return a;
    }
    let Ok(act) = active() else { return a };
    a.active = Some(act);
    if over() {
        return a;
    }
    match focused() {
        Ok(id) => {
            a.focused = Some(id);
            return a;
        }
        Err(e) if e == AxError::NO_VALUE || e == AxError::ATTRIBUTE_UNSUPPORTED => {}
        Err(_) => return a,
    }
    if over() {
        return a;
    }
    a.main = main().ok();
    a
}

/// True when a window waiting with the windows not placed may be on a Desktop that does not
/// show: where a click on its app's Dock item may take macOS, the model not knowing it
/// ([`AppFacts::unplaced`]). `on`: the Spaces the window server lists it on
/// (the engine's `spaces_of`); `screens`: the listing of every screen's Spaces (its
/// `desktops`); either `None` (the reads do not work now, `platform/desk_reads.rs`): it may be.
/// Read, it is only when one of its Spaces is a Desktop or a full-screen Space of the
/// listing that no screen shows. A window listed on no Space, or on the engine's own Spaces
/// only, is on no Desktop: a tab not selected (the owner's log of 11-23: Terminal in front
/// on Desktop 2, its other window in a hidden stage of Desktop 1, three tabs not selected
/// waiting; every click on its Dock item was refused a prediction for them, Desktop 1 was
/// not arranged at the press, macOS counted no window of it there, and nothing happened).
fn waits_elsewhere(on: Option<&[u64]>, screens: Option<&[ScreenDesks]>) -> bool {
    let (Some(on), Some(screens)) = (on, screens) else { return true };
    let shown = |s: u64| screens.iter().any(|sc| sc.current == s);
    let listed = |s: u64| screens.iter().any(|sc| sc.desks.iter().any(|d| d.id == s) || sc.others.iter().any(|(o, _)| *o == s));
    on.iter().any(|s| listed(*s) && !shown(*s))
}

impl Controller {
    /// Milliseconds since the user's latest input, when it is recent (a Dock click, a key).
    pub(super) fn since_input_ms(&self, now: Instant) -> Option<u64> {
        self.triggers.last_at().filter(|t| margin::within("INPUT_RECENT", now.saturating_duration_since(*t), INPUT_RECENT)).map(|t| ms(t, now))
    }

    /// The Desktop arranged ahead, if any.
    pub(super) fn ahead_space(&self) -> Option<SpaceId> {
        self.ahead.as_ref().map(|s| s.ahead.space)
    }

    /// The Desktop window `id` lives on, when the model knows it.
    pub(super) fn predicted_space(&self, id: u32) -> Option<u64> {
        self.model.as_ref().and_then(|m| m.space_of(WindowId(id))).map(|s| s.0)
    }

    /// True when what is held now no longer says the user goes to the window arranged for
    /// ([`ahead_contradicted_by`]: never while an arrangement made at a Dock press waits for
    /// its release).
    fn ahead_contradicted(&self, now: Instant) -> bool {
        let Some(sent) = &self.ahead else { return false };
        let m = self.model.as_ref();
        ahead_contradicted_by(self.focus_replay.peek(now), sent.ahead.window, sent.pid, sent.at_press, self.dock_press.is_some(), |id| {
            m.and_then(|m| m.resolve_standard(id))
        })
    }

    /// True while an arrangement made at a Dock press waits for its click's release
    /// ([`awaits_release`]).
    fn ahead_awaits_release(&self) -> bool {
        self.ahead.as_ref().is_some_and(|s| awaits_release(s.at_press, self.dock_press.is_some()))
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
            hold_step(self.ahead_contradicted(now), self.ahead.is_some(), held.is_some(), self.phase.changing(), overview);
        if undo {
            self.undo_ahead(ob, "another focus or activation came");
        }
        let (true, Some(id)) = (arrange, held) else { return };
        if self.model.is_none() {
            return;
        }
        let list = windows::list_windows();
        if self.arrange_if_listed(ob, &list, id, Trigger::Settle, now) {
            // The Desktop sliding in shows its strip arranged too ([`Self::strip_space`]).
            self.present();
        }
    }

    /// The Space change notice ([`notice_step`]). An arrangement whose window the window
    /// server does not list on its screen now is undone (another Desktop slid in).
    /// With none, when the held window, or with none held the activated (else frontmost)
    /// app's focused window, waits on the Desktop listed now (the one sliding in), its stage
    /// is arranged before every app's window list is read again and the Desktop recognized.
    pub(super) fn arrange_at_notice(&mut self, ob: &mut Observer, now: Instant) {
        let Some(main) = self.model.as_ref().map(|m| *m.main_display()) else { return };
        let list = windows::list_windows();
        let overview = self.look_for_overview(&list, main.frame, now) || self.overview.holds_arrival(now);
        let shown = self.shown_managed(&list);
        // The arranged window's own screen (round displays-2).
        let listed = self.ahead.as_ref().map(|s| listed_for_arrival(&list, s.ahead.window.0, &shown, self.screen_of_space(s.ahead.space).frame));
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
                if !self.arrange_if_listed(ob, &list, id, Trigger::Notice, now) {
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
                if self.arrange_if_listed(ob, &list, id, Trigger::NoticeAx, now) {
                    // Held as its focus event would have been (replayed on arrival).
                    self.focus_replay.hold(Wanted::Window { id, pid }, now);
                }
            }
        }
    }

    /// The display a Space belongs to (the main one for an unknown Space).
    pub(super) fn screen_of_space(&self, space: SpaceId) -> Display {
        self.model.as_ref().map_or(Display::FALLBACK, |m| *m.display(m.display_of_space(space)))
    }

    /// Arranges window `id`'s Desktop when the window waits on a Desktop not shown (for the
    /// model) and the window server lists it on its Desktop's own screen (round displays-2)
    /// with no window of that Desktop covering that screen; the stage left is saved in the
    /// list's order. True when arranged.
    fn arrange_if_listed(&mut self, ob: &Observer, list: &[CgWindow], id: WindowId, trigger: Trigger, now: Instant) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        if !m.waits_on_another_space(id) {
            return false;
        }
        let Some(standard) = m.resolve_standard(id) else { return false };
        let target = m.space_of(standard);
        let screen = target.map_or(m.main_display().frame, |s| m.display(m.display_of_space(s)).frame);
        let elsewhere = |w: u32| m.space_of(WindowId(w)).is_some_and(|s| Some(s) != target);
        let shown = self.shown_managed(list);
        if !listed_for_arrival(list, standard.0, &shown, screen) || display_covered(list, screen, elsewhere) {
            return false;
        }
        let obs = listed_order(list, |w| m.window(w).is_some());
        self.arrange_ahead(ob, standard, trigger, now, Some(obs))
    }

    /// App `pid` activated with its focused window `id` on a Desktop not shown, held hidden
    /// there by the engine ([`crate::platform::window_engine::WindowEngine::off_every_desktop`]): macOS goes to a Desktop
    /// for an app's window only when it counts the window on that Desktop, and a window in a
    /// stage Space is on none (live 11-14: `open` of an app whose one window waited in an
    /// inactive stage of Desktop 1, from Desktop 2: no slide, nothing shown; the Corner
    /// engine's parked window is on its Desktop and macOS goes). So the window's Desktop is
    /// arranged as at a Dock click (its stage shown there, the one left hidden), then the
    /// window is raised and focused, which takes macOS to it. The focus is held for the
    /// arrival either way. True when arranged.
    pub(super) fn arrange_for_hidden_focus(&mut self, ob: &Observer, id: WindowId, pid: i32, now: Instant) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let Some(standard) = m.resolve_standard(id) else { return false };
        if !m.waits_on_another_space(standard) || !self.engine.off_every_desktop(standard) || self.ahead.is_some() {
            return false;
        }
        let list = windows::list_windows();
        let obs = listed_order(&list, |w| m.window(w).is_some());
        if !self.arrange_ahead(ob, standard, Trigger::Hidden, now, Some(obs)) {
            return false;
        }
        if let Some(space) = self.ahead_space() {
            self.present_away(space);
        }
        self.focus_replay.hold(Wanted::Window { id: standard, pid }, now);
        self.execute(ob, vec![Action::Raise { id: standard }, Action::Focus { id: standard }]);
        tracing::info!(window = standard.0, pid, "arrival: the activated app's window waits hidden on another Desktop; arranged there and focused, macOS goes");
        true
    }

    /// App `pid`'s own focus on its window `id`, held off every Desktop elsewhere
    /// ([`crate::engine::focus_elsewhere::hidden_focus`]): its Desktop arranged and the window
    /// focused, as at an activation ([`Self::arrange_for_hidden_focus`]), so macOS goes. With
    /// `switch_back`, the activation's switch on the Desktop shown is undone once macOS has
    /// left it ([`Self::switch_back_after_arrival`]). 11-20 07:53:39: the focus came 200 ms
    /// after the activation, mid-switch, and waited for a Space change that never came.
    pub(super) fn arrive_for_hidden_focus(&mut self, ob: &Observer, id: WindowId, pid: i32, switch_back: bool, now: Instant) {
        if !self.arrange_for_hidden_focus(ob, id, pid, now) {
            tracing::debug!(window = id.0, pid, ahead = self.ahead.is_some(), "a focus on a window hidden on another Desktop: nothing arranged; held for its Desktop");
            return;
        }
        let back = if switch_back { self.activation_switch.take() } else { None };
        tracing::info!(
            window = id.0,
            pid,
            switch_back = ?back.as_ref().map(|b| (b.undo.space.0, b.undo.stage.0, b.undo.previous.0)),
            "a focus of the app's own on its window hidden on another Desktop: arranged there and focused, macOS goes; the activation's switch here is undone once it has"
        );
        self.switch_back = back;
    }

    /// The activation's switch kept by [`Self::arrive_for_hidden_focus`], undone now that its
    /// Desktop is no longer shown (moves only, the windows brought back registered until
    /// seen): that Desktop shows as it was before the activation, as Apple's leaves it. Kept
    /// while its Desktop is still shown; dropped after a click or key press since the
    /// activation, or [`crate::engine::focus_elsewhere::WENT_ELSEWHERE_MAX`]. The actions
    /// are for the arrival's batch.
    pub(super) fn switch_back_after_arrival(&mut self, ob: &Observer) -> Vec<Action> {
        let Some(back) = self.switch_back.take() else { return Vec::new() };
        let now = clock::now();
        if !back.holds(crate::platform::input::since_last_press(), now) {
            tracing::debug!(space = back.undo.space.0, "the activation's switch is no longer undone: a click or key since, or too late");
            return Vec::new();
        }
        let Some(m) = self.model.as_mut() else { return Vec::new() };
        if m.is_shown_space(back.undo.space) {
            self.switch_back = Some(back);
            return Vec::new();
        }
        let actions = m.undo_ahead(&back.undo);
        tracing::info!(
            space = back.undo.space.0,
            stage = back.undo.previous.0,
            left = back.undo.stage.0,
            moves = actions.iter().filter(|a| !matches!(a, Action::StateChanged)).count(),
            since_activation_ms = ms(back.at, now),
            "arrival: macOS left the Desktop the activation switched; its switch undone unseen, the stage that was in front back"
        );
        self.keep_registered(ob, &actions);
        self.present_away(back.undo.space);
        actions
    }

    /// Arranges window `id`'s Desktop for it ([`crate::engine::model::Model::arrange_ahead`]);
    /// on a Desktop not shown the windows it brings back stay registered until seen out of
    /// the park zone. True when arranged.
    fn arrange_ahead(&mut self, ob: &Observer, id: WindowId, trigger: Trigger, now: Instant, obs: Option<Observation>) -> bool {
        let focus_at = self.focus_replay.came(now).unwrap_or(now);
        let changes_at = self.space_changes;
        // The check of the moves (`Verify`), unless the engine reads its own moves back.
        let verify = trigger.away() && !self.engine.verifies_own_moves();
        let Some(m) = self.model.as_mut() else { return false };
        let pid = m.window(id).map(|w| w.meta.pid).unwrap_or_default();
        // Where the model left each window, read before it arranges them.
        let left = if trigger.away() { frames_left(m) } else { HashMap::new() };
        let Some((ahead, actions)) = m.arrange_ahead(id, obs.as_ref()) else {
            tracing::debug!(id = id.0, trigger = trigger.name(), "nothing to arrange ahead for the window");
            return false;
        };
        let mark = m.ahead_mark(&ahead);
        let moves = actions.iter().filter(|a| !matches!(a, Action::StateChanged)).count();
        // Each moved window's frame before the moves, read before they go out: where a park
        // not taken leaves it, where a restore starts from (a parked window's park frame, or
        // where the window server lists it; R11 1). Only the windows moved are asked (R13 3).
        let (expected, before_us) = if verify {
            let asked = clock::now();
            let listed = windows::describe(&moved_ids(&actions));
            let before_us = asked.elapsed().as_micros() as u64;
            let before = frames_before(&left, &listed);
            (expects(&actions, |w| park_frame(m, WindowId(w)), |w| before.get(&w).copied(), |w| m.stage_of(WindowId(w))), Some(before_us))
        } else {
            (Vec::new(), None)
        };
        if trigger.away() {
            self.keep_registered(ob, &actions);
        }
        // The strip shows the Desktop being left until the arrival: its snapshot can wait.
        let deferred = std::mem::replace(&mut self.defer_present, true);
        self.execute(ob, actions);
        self.defer_present = deferred;
        let sent = clock::now();
        tracing::info!(
            window = id.0,
            predicted = ahead.space.0,
            stage = ahead.stage.0,
            left = ahead.previous.0,
            trigger = trigger.name(),
            moves,
            before_us,
            since_focus_ms = ms(focus_at, sent),
            since_input_ms = self.since_input_ms(sent),
            "arrival: the Desktop arranged ahead for the window it is shown for"
        );
        self.ahead = Some(AheadSent { ahead, pid, trigger, focus_at, sent_at: sent, changes_at, at_press: false });
        // A previous arrival's check still looking ends here (its tally logged).
        self.finish_verify("a new arrangement went out");
        // The first look a fast tick later: right after the calls the window server's list
        // may not show them yet even when they took (R9 1).
        self.verify = (!expected.is_empty()).then(|| Verify {
            sent,
            next: sent + VERIFY_EVERY,
            rounds: 0,
            slid: 0,
            polling: true,
            told: false,
            tail: None,
            over_ticks: 0,
            over_since: None,
            mark,
            moves: expected,
        });
        true
    }

    /// True while the WM thread's fast tick has work: an arrangement's moves to check, a held
    /// activation, the corner patch after a focus or through Show Desktop.
    pub fn wants_fast_tick(&self) -> bool {
        let now = clock::now();
        self.verify.as_ref().is_some_and(|v| v.polling)
            || self.slide_held()
            || self.upkeep_wants_fast_tick(now)
            || self.fresh_waiting()
            || self.switcher_wants_fast_tick()
            || self.flip_awaited()
            || self.asks_again()
    }

    /// The WM thread's fast tick: the moves of an arrangement made at a Dock click checked
    /// when due ([`Self::check_arrangement`]; after its Desktop's arrival,
    /// [`Self::arrival_round`]), an activation held for a Desktop slide looked at
    /// ([`Self::slide_hold_step`]), and the corner patch planned while Show Desktop is up
    /// ([`Self::reveal_fast`]: its close seen within a fast tick).
    pub fn fast_tick(&mut self, ob: &mut Observer) {
        // A Space change's notice that came before the listing's flip: looked at again.
        self.flip_fast_tick(ob);
        self.slide_hold_step(ob);
        // An activated app that did not say its focused window: asked again when due.
        self.ask_again_step(ob);
        let now = clock::now();
        // ⌘Tab's switcher list gone with no key for it yet: decided after its wait; a window
        // put home for it taken back when its short cap (the key tap not hearing) ran out.
        self.switcher_fast_tick(ob, now);
        if self.lent_awaits_activation() {
            self.expire_lent(now);
        }
        // A newly opened app's window seen by AX before its first frame: placed as it shows.
        self.offer_fresh(ob, now);
        // The engine's share (the Corner engine: its patch after a focus, through Show Desktop).
        self.upkeep_fast_tick(ob, now);
        if self.model.is_none() {
            self.verify = None;
        } else if let Some(v) = self.verify.as_ref().filter(|v| v.polling && now >= v.next) {
            if v.tail.is_some() {
                self.arrival_round(ob, &windows::list_windows(), now);
            } else {
                self.check_arrangement(ob, now);
            }
        }
    }

    /// The user acted on window `id` (`how`: grabbed its sliver, dragged it, clicked it): its
    /// move made ahead is no longer checked or sent again, so nothing parks it again (or puts
    /// it back) under them (R11 3).
    pub(super) fn user_took(&mut self, id: u32, how: &str) {
        let Some(v) = self.verify.as_mut() else { return };
        if drop_move(&mut v.moves, id) {
            tracing::debug!(id, how, "arrival: the user acted on a window moved ahead; its move is no longer checked");
        }
    }

    /// Ends the check of an arrangement's moves (`why`: it was undone, another Desktop
    /// came), logging what was sent again by then.
    pub(super) fn finish_verify(&mut self, why: &str) {
        let Some(v) = self.verify.take() else { return };
        let resent: u32 = v.moves.iter().map(|e| e.tries).sum();
        tracing::info!(
            why,
            rounds = v.rounds,
            slid = v.slid,
            resent,
            after_ms = ms(v.sent, clock::now()),
            "arrival: the check of the arrangement's moves ends"
        );
    }

    /// Screens the check's moves against the model before a round ([`screen`]): the whole
    /// check ends (true: it goes on) once the stage arranged is no longer the one active on
    /// its Desktop or a switch touched it; a move whose window the model has otherwise now is
    /// dropped. Nothing parks or brings back a window the model has moved on from (R13 1).
    fn verify_stands(&mut self) -> bool {
        let (Some(m), Some(v)) = (self.model.as_ref(), self.verify.as_mut()) else { return false };
        match screen(m, &v.mark, &mut v.moves) {
            Ok(dropped) => {
                if !dropped.is_empty() {
                    tracing::debug!(?dropped, "arrival: moves made ahead the model has moved on from; no longer checked");
                }
                true
            }
            Err(why) => {
                self.finish_verify(why);
                false
            }
        }
    }

    /// Stops the fast tick's checks (`what`: the tally said at info); the moves stay for the
    /// arrival's final check ([`Self::final_check`]).
    fn stop_polling(&mut self, what: &str, missing: &[String], now: Instant) {
        let Some(v) = self.verify.as_mut() else { return };
        v.polling = false;
        let resent: u32 = v.moves.iter().map(|e| e.tries).sum();
        tracing::info!(
            moves = v.moves.len(),
            ?missing,
            rounds = v.rounds,
            slid = v.slid,
            resent,
            after_ms = ms(v.sent, now),
            "arrival: {what}"
        );
    }

    /// Reads the window server's frames of an arrangement's moves ([`judge`]): those it does
    /// not show taken are sent again when due ([`resend_due`], [`Self::resend`]), every
    /// [`VERIFY_EVERY`] for [`VERIFY_FOR`] or until it shows them all; a round that finds the
    /// Desktop sliding is skipped. The first round with moves not taken and the last tally
    /// are logged at info, the rounds between at debug. Stops once its Desktop is arriving
    /// (a Space change settling: the user may hold its windows); the arrival checks the
    /// moves once more.
    fn check_arrangement(&mut self, ob: &Observer, now: Instant) {
        // Each app's enhanced UI off once for all these moves (round enhanced-ui).
        let _quiet = enhanced_ui::Batch::open();
        let live = self.ahead.as_ref().is_some_and(|s| s.trigger.away());
        if !live {
            self.finish_verify("the arrangement is settled");
            return;
        }
        if self.phase.changing() {
            self.stop_polling("its Desktop is arriving; the arrival checks the moves", &[], now);
            return;
        }
        if !self.verify_stands() {
            return;
        }
        let (screens, engine) = (self.screens_of_moves(), self.engine.clone());
        let Some(v) = self.verify.as_mut() else { return };
        let list = windows::list_windows();
        let listed = |id: u32| list.iter().find(|w| w.id == id);
        let seen: Vec<Seen> = v.moves.iter().map(|e| judge(e, listed(e.id), screens[&e.id].frame, |r| engine.hides_now(WindowId(e.id), &screens[&e.id], r))).collect();
        let elapsed = ms(v.sent, now);
        let over = now.saturating_duration_since(v.sent) >= VERIFY_FOR;
        if seen.contains(&Seen::Sliding) {
            v.slid += 1;
            v.next = now + VERIFY_EVERY;
            tracing::debug!(after_ms = elapsed, "arrival: the Desktop slides; the moves are judged when it stops");
            if over {
                self.stop_polling("the Desktop still slides; the arrival checks the moves", &[], now);
            }
            return;
        }
        let missing: Vec<usize> = (0..v.moves.len()).filter(|&i| owed(&v.moves[i], seen[i])).collect();
        let bounds: Vec<String> = missing.iter().map(|&i| listed_at(&list, v.moves[i].id)).collect();
        let spent = missing.iter().all(|&i| v.moves[i].gave_up || v.moves[i].tries >= RESEND_MAX);
        if missing.is_empty() {
            self.stop_polling("the window server shows the arrangement", &[], now);
            return;
        }
        if spent || over {
            self.stop_polling("the window server does not show some moves yet; the arrival checks them", &bounds, now);
            return;
        }
        let due: Vec<usize> = missing.iter().copied().filter(|&i| resend_due(&v.moves[i], listed(v.moves[i].id).map(|w| w.bounds), now)).collect();
        v.rounds += 1;
        v.next = now + VERIFY_EVERY;
        if due.is_empty() {
            tracing::debug!(missing = ?bounds, round = v.rounds, after_ms = elapsed, "arrival: moves not shown yet; none due to be sent again");
            return;
        }
        if std::mem::replace(&mut v.told, true) {
            tracing::debug!(missing = ?bounds, round = v.rounds, after_ms = elapsed, "arrival: moves not shown yet; sending the due ones again");
        } else {
            tracing::info!(missing = ?bounds, round = v.rounds, after_ms = elapsed, "arrival: the window server does not show every move yet; sending them again");
        }
        let mut moves = std::mem::take(&mut v.moves);
        let round = clock::now();
        let left = send_round(&due, |i| {
            let e = &mut moves[i];
            let bounds = listed(e.id).map(|w| w.bounds);
            let sent = self.resend(ob, e, now, round) != Resent::Deferred;
            if sent {
                e.last = Some((now, bounds));
            }
            sent
        });
        if !left.is_empty() {
            tracing::debug!(left = left.len(), took_ms = ms(round, clock::now()), "arrival: the round's time is up; the rest go next round");
        }
        if let Some(v) = self.verify.as_mut() {
            v.moves = moves;
        }
    }

    /// The screen of each of the arrangement's moves (the window's own; the main one for a
    /// window the model forgot), so a move is judged against its own screen's frame and park
    /// zone (round displays-2).
    fn screens_of_moves(&self) -> HashMap<u32, Display> {
        let Some(v) = self.verify.as_ref() else { return HashMap::new() };
        let main = self.model.as_ref().map_or(Display::FALLBACK, |m| *m.main_display());
        v.moves
            .iter()
            .map(|e| {
                let d = self.model.as_ref().and_then(|m| m.window(WindowId(e.id)).map(|w| *m.display(w.meta.display))).unwrap_or(main);
                (e.id, d)
            })
            .collect()
    }

    /// The predicted Desktop arrived (`list`: the window server's list read for the arrival,
    /// maybe still mid-slide: it is read ~60 ms after the notice). The fast tick goes on
    /// looking for [`ARRIVAL_CHECK_FOR`] more, whatever Space change settles meanwhile
    /// ([`Self::arrival_round`]), this list first.
    fn final_check(&mut self, ob: &Observer, list: &[CgWindow], now: Instant) {
        let Some(v) = self.verify.as_mut() else { return };
        v.tail = Some((now, now + ARRIVAL_CHECK_FOR));
        v.polling = true;
        self.arrival_round(ob, list, now);
    }

    /// One look of the arrival's check ([`tail_step`], [`tail_round`]): the moves the model
    /// has moved on from dropped first, the whole check ended by any switch of the stage
    /// arranged ([`Self::verify_stands`]); every move judged on `list`; while it shows the
    /// slide, or a move's window is not listed, the next fast tick looks again (until
    /// [`ARRIVAL_CHECK_FOR`] runs out); then every move not taken, or owed its exact frame
    /// after a nudge, is sent once more whatever the slide's budget left (an unsure park only
    /// when never sent again), held to [`RESEND_ROUND_CAP`] a round: one the cap deferred or
    /// cut goes next tick, the time up or not. Once the time is up, a move still owed its
    /// exact frame gets one more exact move on its full timeout, one a tick. Past the time by
    /// [`TAIL_OVER_TICKS`] ticks or [`TAIL_OVER_FOR`] the check ends with what is left logged
    /// at warn (R15 1, 2). The outcome is logged at info.
    fn arrival_round(&mut self, ob: &Observer, list: &[CgWindow], now: Instant) {
        // Each app's enhanced UI off once for all these moves (round enhanced-ui).
        let _quiet = enhanced_ui::Batch::open();
        if !self.verify_stands() {
            return;
        }
        let (screens, engine) = (self.screens_of_moves(), self.engine.clone());
        let Some(v) = self.verify.as_mut() else { return };
        let Some((arrived, until)) = v.tail else { return };
        let over = now >= until;
        if over {
            v.over_ticks += 1;
        }
        let ceiling = over && tail_past_ceiling(v.over_ticks, now.saturating_duration_since(*v.over_since.get_or_insert(now)));
        let listed = |id: u32| list.iter().find(|w| w.id == id);
        let seen: Vec<Option<Seen>> =
            v.moves.iter().map(|e| listed(e.id).map(|w| judge(e, Some(w), screens[&e.id].frame, |r| engine.hides_now(WindowId(e.id), &screens[&e.id], r)))).collect();
        v.rounds += 1;
        v.next = now + VERIFY_EVERY;
        let due = match tail_step(&v.moves, &seen, over) {
            TailStep::Wait => {
                v.slid += 1;
                tracing::debug!(since_arrival_ms = ms(arrived, now), "arrival: the list still shows the slide (or a window unlisted); looking again");
                return;
            }
            TailStep::Send(due) => due,
        };
        let resent: Vec<String> = due.iter().map(|&i| listed_at(list, v.moves[i].id)).collect();
        let sliding = due.iter().filter(|&&i| seen[i] == Some(Seen::Sliding)).count();
        let mut moves = std::mem::take(&mut v.moves);
        let round = clock::now();
        let out = tail_round(&mut moves, &due, &seen, over, ceiling, |e| self.resend(ob, e, now, round));
        let Some(v) = self.verify.as_mut() else { return };
        v.moves = moves;
        let unsent = match &out.end {
            TailEnd::NextTick => {
                tracing::debug!(
                    left = out.left.len(),
                    cut = out.cut,
                    exact = ?out.exact.map(|i| v.moves[i].id),
                    over,
                    "arrival: the final check's round time is up; the rest go next tick"
                );
                return;
            }
            TailEnd::Done => Vec::new(),
            TailEnd::Ceiling(left) => left.iter().map(|&i| v.moves[i].id).collect(),
        };
        let owed_still: Vec<u32> = v.moves.iter().filter(|e| e.nudged_only || e.gave_up).map(|e| e.id).collect();
        let unlisted = seen.iter().filter(|s| s.is_none()).count();
        let tries = v.moves.iter().map(|e| e.tries).sum::<u32>();
        let (after_ms, since_arrival_ms) = (ms(v.sent, now), ms(arrived, now));
        if unsent.is_empty() {
            tracing::info!(
                moves = v.moves.len(),
                ?resent,
                sliding,
                unlisted,
                ?owed_still,
                rounds = v.rounds,
                slid = v.slid,
                tries,
                after_ms,
                since_arrival_ms,
                "arrival: the final check of the arrangement's moves, {}",
                if over { "its time out" } else { "the Desktop settled" }
            );
        } else {
            tracing::warn!(
                moves = v.moves.len(),
                ?unsent,
                ?owed_still,
                over_ticks = v.over_ticks,
                rounds = v.rounds,
                tries,
                after_ms,
                since_arrival_ms,
                "arrival: the final check of the arrangement's moves ran past its time; these stay unsent"
            );
        }
        self.verify = None;
    }

    /// Sends move `e` again, bounded: once 1 pt off, then exactly (AppKit drops a move to
    /// where it thinks the window already is), or only exactly when the nudge went through
    /// last time and the exact move did not. Through the registry's element when the
    /// observer lost the window. A failure or a timeout gives the move up (until the final
    /// check); a nudge through with its exact move failed leaves the exact one owed.
    ///
    /// The round that started at `round` is held to [`RESEND_ROUND_CAP`]: a move goes only
    /// with [`MOVE_NEEDS`] of it left ([`Resent::Deferred`]: nothing sent, not counted), each
    /// call's timeout what is left; an exact move the cap cut is [`Resent::Cut`] (R13 2).
    fn resend(&mut self, ob: &Observer, e: &mut Expect, now: Instant, round: Instant) -> Resent {
        let Some(first) = round_timeout(RESEND_ROUND_CAP, clock::since(round)).filter(|_| move_fits(clock::since(round))) else {
            return Resent::Deferred;
        };
        // A fresh handle found for a dead one (stale-handles) wins over the tracked one.
        let element = ob.element(e.id).or_else(|| safety::registry_entry(e.id).map(|r| r.element));
        let Some(element) = element else {
            tracing::debug!(id = e.id, "arrival: a move to send again has no element; given up");
            e.gave_up = true;
            return Resent::Full;
        };
        e.tries += 1;
        self.causes.expect_move(e.id, e.frame, None, now);
        let (frame, park) = (e.frame, e.park);
        let nudged = Rect::new(frame.x + 1.0, frame.y, frame.w, frame.h);
        let send = |el: &AxElement, r: Rect| if park { el.set_position(r.origin()) } else { el.set_frame(r) };
        let skip_nudge = e.nudged_only;
        let full = |seconds: f32| seconds >= QUERY_AX_TIMEOUT_SECONDS - 1e-4;
        let (nudge, exact, exact_full) = element.bounded(first, |el| {
            if skip_nudge {
                return (Ok(()), send(el, frame), full(first));
            }
            let nudge = send(el, nudged);
            match (&nudge, round_timeout(RESEND_ROUND_CAP, clock::since(round))) {
                (Err(_), _) => (nudge, Ok(()), true),
                (Ok(()), Some(seconds)) => {
                    let _ = el.set_messaging_timeout(seconds);
                    (nudge, send(el, frame), full(seconds))
                }
                // The round's time ran out after the nudge: the exact move is owed.
                (Ok(()), None) => (nudge, Err(AxError::CANNOT_COMPLETE), false),
            }
        });
        record_resend(e, nudge, exact, exact_full)
    }

    /// A left mouse-down at `p` (`list`: the window list read for it): over the Dock's bar,
    /// the application item there is hit-tested and its click's prediction worked out now
    /// (the app's state does not change during a press), kept for the mouse-up
    /// ([`Self::dock_released`]). Anywhere else this costs the bar test on `list` only.
    pub(super) fn dock_pressed(&mut self, ob: &Observer, list: &[CgWindow], p: crate::engine::geometry::Point, now: Instant) {
        if self.dock_press.take().is_some() {
            // A press whose release the monitor missed: no click, and what it arranged at the
            // press goes (it would otherwise stand for this press too).
            tracing::debug!("dock: a press kept from before, its release never seen: no click");
            self.press_not_click(ob);
        }
        // The next press after a Dock press's lend, its app not activated yet: the click was
        // the user's last word for it, and this one is elsewhere. (⌘Tab's lend is left to its
        // activation: a press after ⌘'s release does not undo it, review B-6.)
        let heard = self.received_at(now);
        if self.lent.is_some_and(|l| !l.switcher && !l.wait.is_activated() && heard > l.at) {
            let outcome = self.lent.as_mut().and_then(|l| l.wait.refute("the next press came before its app's activation", now));
            self.lent_ended(outcome, now);
        }
        self.slide_watch_pressing();
        if self.model.is_none() || self.frozen() {
            return;
        }
        let Some(dock) = self.dock_pid(list) else { return };
        if !dock::dock_under(list, p, dock, &self.dock_bar_now(), &|w| crate::platform::desktop::seen_through(w, p)) {
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
            took_ms = ms(now, clock::now()),
            "dock: pressed"
        );
        if let Ok((_, window)) = prediction {
            self.arrange_at_press(ob, &item, window, now);
        }
        // Its click may switch stages: the pictures that switch would wait for are taken
        // now, while the button is down (`switch_pictures`, "Pictures taken ahead").
        self.switch_likely(crate::platform::switch_likely::Sign::DockPress);
        self.dock_press = Some(DockPress { item, at: p, when: now, prediction });
    }

    /// A press whose click goes to `window` on another Desktop, kept off every Desktop by the
    /// engine (the Spaces engine's stage Space): its Desktop is arranged now, before the
    /// release. The Dock activates the app at the release, and macOS goes to a Desktop for
    /// it only where it counts one of its windows; arranged at the release (4–18 ms after
    /// it), the window was still on no Desktop, and the first click did nothing (the
    /// owner's log of 11-15 06:32:34: an active app, its window in a stage of Desktop 1; the
    /// second click went). The Corner engine's parked window is on its Desktop all along:
    /// arranged at the release as before. A press that is no click undoes it at the release
    /// ([`Self::press_not_click`]); no Space change within [`DOCK_SWITCH_CAP`] as at a click.
    /// Until the release nothing else does ([`awaits_release`]): its focus is held only then.
    fn arrange_at_press(&mut self, ob: &Observer, item: &DockItem, window: WindowId, now: Instant) {
        if !self.engine.off_every_desktop(window) || self.frozen() || self.ahead.is_some() {
            return;
        }
        if !self.arrange_ahead(ob, window, Trigger::DockClick, now, None) {
            return;
        }
        if let Some(sent) = self.ahead.as_mut() {
            sent.at_press = true;
        }
        if let Some(space) = self.ahead_space() {
            self.present_away(space);
        }
        tracing::info!(
            path = item.path,
            window = window.0,
            since_press_ms = ms(now, clock::now()),
            "dock: the Desktop the press goes to is arranged at the press (its window waited off every Desktop)"
        );
    }

    /// A press on the Dock that was no click (a drag, a held press, a modifier): what was
    /// done at the press for its click is undone now: the Desktop arranged at the press, the
    /// window put home.
    fn press_not_click(&mut self, ob: &Observer) {
        if self.ahead.as_ref().is_some_and(|s| s.at_press) {
            self.undo_ahead(ob, "the Dock press was no click");
        }
        if self.lent.is_some_and(|l| dock_takes_back(l.switcher, false)) {
            let now = clock::now();
            let outcome = self.lent.as_mut().and_then(|l| l.wait.refute("the press was no click", now));
            self.lent_ended(outcome, now);
        }
    }

    /// About every tick (and at once after its app's activation is decided): a window put
    /// home at a Dock press or for ⌘Tab's switcher whose stage is in front, or that the model
    /// shows, is the switch's now (met); one whose cap ran out goes back into its stage Space
    /// ([`lent_step`]).
    pub(super) fn expire_lent(&mut self, now: Instant) {
        let Some(lent) = self.lent.as_mut() else { return };
        let (parked, in_front) = self.model.as_ref().map_or((false, false), |m| {
            (m.window(lent.window).is_some_and(|w| w.parked), m.stage_of(lent.window).is_some_and(|s| m.is_active(s)))
        });
        let outcome = lent_step(&mut lent.wait, parked, in_front, now);
        self.lent_ended(outcome, now);
    }

    /// The lend's wait ended `outcome` (`None`: it goes on): met, forgotten (the switch shows
    /// it); refuted or capped, taken back now, unshown.
    pub(super) fn lent_ended(&mut self, outcome: Option<Outcome>, now: Instant) {
        let (Some(outcome), Some(lent)) = (outcome, self.lent) else { return };
        self.lent = None;
        let who = if lent.switcher { "cmd-tab: the window put home for the switcher" } else { "dock: the window put home at the press" };
        let since_ms = ms(lent.at, now);
        let cap = lent.wait.cap().map(|c| c.name);
        let why = match outcome {
            Outcome::Met => {
                tracing::debug!(window = lent.window.0, since_ms, "{who} is the switch's now");
                return;
            }
            Outcome::Refuted(why) => why,
            Outcome::Cap => "no activation and switch came for it within its cap",
        };
        let back = self.engine.take_back(lent.window);
        tracing::info!(window = lent.window.0, pid = lent.pid, back = back.back, restored = ?back.restored, since_ms, cap, activated = lent.wait.is_activated(), "{who} goes back: {why}");
        if lent.switcher {
            crate::platform::switcher_notes::note("taken back", || format!("window={} why={why} restored={:?}", lent.window.0, back.restored));
        }
    }

    /// App `pid` activated (its notice stamped `fired`), before anything else is judged of it:
    /// a window put home for it has the first half of its confirmation (its switch shows it
    /// next).
    pub(super) fn lent_activated(&mut self, pid: i32, fired: Instant) {
        let now = clock::now();
        if let Some(lent) = self.lent.as_mut().filter(|l| l.pid == pid && !l.wait.is_activated()) {
            lent.wait.activation(pid, fired, now);
            let who = if lent.switcher { "cmd-tab: the app its window was put home for is activated" } else { "dock: the app its window was put home for at the press is activated" };
            tracing::info!(pid, window = lent.window.0, since_lend_ms = ms(lent.at, now), "{who}: kept for its switch");
        }
    }

    /// Another app than the one a window was put home for activated (its notice stamped
    /// `fired`, not an echo of ours, not stale): the user went elsewhere, or the switcher was
    /// let go on another app. The lend is refuted once its wait began, by an activation
    /// stamped after it began; taken back now.
    pub(super) fn lent_activation_elsewhere(&mut self, pid: i32, fired: Instant) {
        let now = clock::now();
        let Some(lent) = self.lent.as_mut().filter(|l| l.pid != pid) else { return };
        let outcome = lent.wait.activation(pid, fired, now);
        if outcome.is_some() {
            tracing::info!(pid, lent_pid = lent.pid, window = lent.window.0, "lend: another app was activated");
        }
        self.lent_ended(outcome, now);
    }

    /// App `pid`'s activation decided (its switch made, or held): a window put home for it
    /// whose stage is in front now is the switch's at once, not at the next tick.
    pub(super) fn lent_decided(&mut self, pid: i32) {
        if self.lent.is_some_and(|l| l.pid == pid) {
            self.expire_lent(clock::now());
        }
    }

    /// True when app `pid`'s activation finds a window of it put home at a Dock press
    /// ([`Lent`]): macOS counts the app on the Desktop shown.
    pub(super) fn lent_for(&self, pid: i32) -> bool {
        self.lent.is_some_and(|l| l.pid == pid)
    }

    /// A left mouse-up at `p` (`modifiers`: any of ⌘⌥⌃⇧ held): a click on the Dock item
    /// pressed has its prediction arranged ([`Self::dock_clicked`]).
    pub(super) fn dock_released(&mut self, ob: &Observer, p: crate::engine::geometry::Point, modifiers: bool, now: Instant) -> Option<WindowId> {
        let press = self.dock_press.take()?;
        let click = dock::is_click(&press, p, now, modifiers);
        self.slide_watch_released(click, ms(press.when, now), now);
        if click {
            self.dock_clicked(ob, &press, now)
        } else {
            tracing::debug!(path = press.item.path, held_ms = ms(press.when, now), modifiers, "dock: a press, not a click; no prediction");
            self.press_not_click(ob);
            None
        }
    }

    /// A click on a Dock item (`press`, let go at `released`): when macOS will take the user
    /// to another Desktop for it (the prediction made at the press), that Desktop is
    /// arranged now. Returns the window predicted.
    fn dock_clicked(&mut self, ob: &Observer, press: &DockPress<Prediction>, released: Instant) -> Option<WindowId> {
        let started = clock::now();
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
                } else {
                    match release_step(self.ahead.as_ref().map(|s| (s.ahead.window, s.at_press)), window) {
                        ReleaseStep::HoldPress => {
                            self.focus_replay.hold(Wanted::Window { id: window, pid }, started);
                            tracing::info!(
                                path = press.item.path,
                                window = window.0,
                                since_click_ms = ms(press.when, clock::now()),
                                "dock: the Desktop the click goes to was arranged at the press"
                            );
                            return Some(window);
                        }
                        ReleaseStep::Stands => return Some(window),
                        ReleaseStep::Another => "another arrangement stands",
                        ReleaseStep::Arrange if self.arrange_ahead(ob, window, Trigger::DockClick, started, None) => {
                            self.focus_replay.hold(Wanted::Window { id: window, pid }, started);
                            if let Some(space) = self.ahead_space() {
                                self.present_away(space);
                            }
                            let done = clock::now();
                            tracing::info!(
                                path = press.item.path,
                                window = window.0,
                                since_click_ms = ms(press.when, done),
                                since_release_ms = ms(released, done),
                                "dock: the Desktop the click goes to is arranged"
                            );
                            return Some(window);
                        }
                        ReleaseStep::Arrange => "its stage is already the one shown there",
                    }
                }
            }
            Err(why) => why,
        };
        let done = clock::now();
        tracing::info!(
            path = press.item.path,
            why,
            since_click_ms = ms(press.when, done),
            since_release_ms = ms(released, done),
            "dock: no prediction"
        );
        None
    }

    /// App `pid`'s windows waiting with the windows not placed: how many, and whether one may
    /// be on a Desktop that does not show ([`waits_elsewhere`]; [`AppFacts::unplaced`]). The
    /// screens' listing is read once, and only for an app with a window waiting.
    pub(super) fn unplaced_elsewhere(&self, ob: &Observer, pid: i32) -> (usize, bool) {
        let waiting: Vec<u32> = self.unplaced.iter().copied().filter(|id| ob.window(*id).is_some_and(|w| w.pid == pid)).collect();
        if waiting.is_empty() {
            return (0, false);
        }
        let screens = self.engine.desktops();
        let elsewhere = waiting.iter().any(|id| waits_elsewhere(self.engine.spaces_of(WindowId(*id)).as_deref(), screens.as_deref()));
        (waiting.len(), elsewhere)
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
        let displays = m.displays().to_vec();
        let visible_here = dock::visible_window_of(list, pid, |r| displays.iter().any(|d| self.engine.is_hidden(d, r)));
        let (waiting, unplaced) = self.unplaced_elsewhere(ob, pid);
        // One bounded round with the app (`press_round`): hidden, active, its focused (else
        // main) window; it stops at the first failure and within PRESS_ROUND_CAP.
        let app = AxElement::application(pid);
        let started = clock::now();
        let answer = app.bounded(QUERY_AX_TIMEOUT_SECONDS, |a| {
            press_round(
                || a.is_hidden(),
                || a.is_frontmost(),
                // The window element is on the global timeout: bounded too (R11 4).
                || a.focused_window().and_then(|w| window_id_within(&w, round_timeout(PRESS_ROUND_CAP, clock::since(started)))),
                || a.main_window().and_then(|w| window_id_within(&w, round_timeout(PRESS_ROUND_CAP, clock::since(started)))),
                || match round_timeout(PRESS_ROUND_CAP, clock::since(started)) {
                    Some(seconds) => {
                        let _ = a.set_messaging_timeout(seconds);
                        false
                    }
                    None => true,
                },
            )
        });
        let took_ms = ms(started, clock::now());
        let PressAnswer { hidden, active, focused, main: main_window } = answer;
        let Some(hidden) = hidden else {
            tracing::info!(bundle, pid, took_ms, "dock: the app did not say whether it is hidden");
            return Err("the app did not answer in 50 ms (hidden or not)");
        };
        let Some(active) = active else {
            tracing::info!(bundle, pid, took_ms, "dock: the app did not say whether it is active");
            return Err("the app did not say whether it is active");
        };
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
            desktop_shown = m.current_space(m.main_display().id).0,
            visible_here,
            unplaced,
            waiting,
            took_ms,
            "dock: the app at the press"
        );
        let ax_window = focused.or(main_window).map(WindowId);
        let facts = AppFacts { hidden, unplaced, active, visible_here };
        let home = m.dock_home(pid, facts);
        if let Some(mut prev) = self.lent.take_if(|l| dock_takes_back(l.switcher, home.is_some())) {
            // A press after a press: the window put home for the first goes back (one put
            // home for ⌘Tab's switcher only when this press puts another home).
            prev.wait.refute("another press puts another window home", clock::now());
            let back = self.engine.take_back(prev.window).back;
            tracing::info!(window = prev.window.0, back, "lend: the window put home before goes back (another press puts another window home)");
        }
        let mut noted = None;
        if let Some(home) = home {
            // Parked first, while still hidden (round dock-put-home): on its Desktop for
            // macOS, out of sight until its stage's switch. The lend is made either way.
            let park = self.lend_park(ob, home, true, Some(lend_park::DOCK_PARK_SECONDS));
            let asked = park.is_some();
            let lend = self.engine.lend_home(home, park);
            noted = Some(LendNote { home: lend.home, parked: lend.parked, asked, park_ms: lend.park_ms });
            if lend.home {
                let at = clock::now();
                self.lent = Some(Lent { window: home, pid, at, switcher: false, wait: LendWait::begun(pid, at, LEND_CAP) });
                tracing::info!(
                    bundle,
                    pid,
                    window = home.0,
                    parked = lend.parked.unwrap_or(false),
                    park_ms = lend.park_ms,
                    took_ms = ms(started, clock::now()),
                    "dock: the app's windows here wait off every Desktop and macOS counts one elsewhere: its stage's window is put home before the release"
                );
                if !lend.parked.unwrap_or(false) {
                    tracing::info!(bundle, pid, window = home.0, asked, park_ms = lend.park_ms, "dock: the window put home could not be parked off the screen first (its app did not move it in time): it shows at its frame until its stage's switch");
                }
                self.dock_seen = Some(DockSeen { at: self.received_at(clock::now()), pid, active, predicted: None });
                self.slide_watch_press(&bundle, pid, noted, None);
                return Err("the app's window here was put home at the press: macOS activates it here");
            }
            tracing::info!(bundle, pid, window = home.0, "dock: the app's window here could not be put home; macOS goes where it counts one");
        }
        let result = m.dock_arrival(pid, facts, ax_window);
        let predicted = result.ok().and_then(|w| m.space_of(w)).map(|s| s.0);
        self.dock_seen = Some(DockSeen { at: self.received_at(clock::now()), pid, active, predicted });
        self.slide_watch_press(&bundle, pid, noted, predicted);
        let window = result?;
        tracing::debug!(bundle, pid, window = window.0, "dock: the window macOS will show");
        Ok((pid, window))
    }

    /// A Space change after a Dock press: which Desktop came, against the prediction (the
    /// day's log tells whether the rule holds).
    pub(super) fn note_dock_arrival(&mut self, arrived: SpaceId, from: SpaceId) {
        let Some(seen) = self.dock_seen.take() else { return };
        // Measured to the Space change's notice when this arrival is its own.
        let heard = self.received_at(clock::now());
        if !margin::note("DOCK_SEEN_FOR", margin::age("DOCK_SEEN_FOR", heard, seen.at), DOCK_SEEN_FOR, heard.saturating_duration_since(seen.at) <= DOCK_SEEN_FOR) {
            return;
        }
        tracing::info!(
            pid = seen.pid,
            active = seen.active,
            predicted = ?seen.predicted,
            from = from.0,
            arrived = arrived.0,
            right = seen.predicted == Some(arrived.0),
            since_press_ms = ms(seen.at, heard),
            "dock: the Desktop a Dock click went to"
        );
    }

    /// Redraws the pane of `space`, a Desktop not shown (arranged ahead at a Dock click, or
    /// that arrangement undone), with its cards, so it slides in as it is.
    fn present_away(&mut self, space: SpaceId) {
        let Some(m) = self.model.as_ref() else { return };
        // Its own screen's strip (round displays-2).
        let display = m.display_of_space(space);
        if m.current_space(display) == space {
            return;
        }
        strip_model::present(StripSnapshot {
            cards: cards_in(m, space),
            active_stage: m.active_stage_in(space).map(|s| s.0),
            style: self.style.clone(),
            space: space.0,
            display: display.0,
            away: true,
            ..StripSnapshot::default()
        });
    }

    /// A Desktop was recognized on screen `display` (`arrived`): an arrangement made ahead on a
    /// Desktop of that screen is confirmed (its
    /// windows raised, the window focused; the stage that was there back if the window is
    /// gone) or undone; one made on another screen's Desktop stands until that screen's own
    /// arrival (every screen arrives in turn at a Space change: the main screen's arrival,
    /// its Desktop unchanged, undid the laptop's arrangement as "another Desktop came" before
    /// the laptop's Desktop was recognized). The actions are for the arrival's batch, with the window the arrival
    /// counts as focused (the predicted one, on a hit: AX may still name the previous
    /// app's); an undo's restores (a Desktop not shown) stay registered until seen. On a hit
    /// with the window still in its stage the moves made at a Dock click get their final
    /// check against `list`, the window server's list read for the arrival
    /// ([`Self::final_check`]); otherwise their check ends ([`arrival_check`]).
    pub(super) fn settle_ahead(
        &mut self,
        ob: &Observer,
        list: &[CgWindow],
        display: DisplayId,
        arrived: SpaceId,
        now: Instant,
    ) -> (Vec<Action>, Option<WindowId>) {
        if !self.ahead.as_ref().is_some_and(|s| self.screen_of_space(s.ahead.space).id == display) {
            return (Vec::new(), None);
        }
        let Some(sent) = self.ahead.take() else { return (Vec::new(), None) };
        let hit = sent.ahead.space == arrived;
        let present = self.model.as_ref().is_some_and(|m| m.stage_of(sent.ahead.window) == Some(sent.ahead.stage));
        match arrival_check(hit, present) {
            // The list is the settled Desktop's: a move it does not show taken goes again.
            Ok(()) => self.final_check(ob, list, now),
            // The batch brings back the stage that was there (or another Desktop came): the
            // moves made ahead are never sent again over it (R13 1).
            Err(why) => self.finish_verify(why),
        }
        let Some(m) = self.model.as_mut() else { return (Vec::new(), None) };
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
            if sent.trigger.away() {
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
            since_sent_ms = ms(sent.sent_at, clock::now()),
            moves = actions.iter().filter(|a| !matches!(a, Action::StateChanged)).count(),
            "arrival: the arrangement made ahead is undone ({why})"
        );
        self.keep_registered(ob, &actions);
        self.execute(ob, actions);
        if sent.trigger.away() {
            // The focus held for the click was the prediction's: it goes with it.
            if matches!(self.focus_replay.peek(clock::now()), Some(Wanted::Window { id, .. }) if id == sent.ahead.window) {
                self.focus_replay.clear();
            }
            self.present_away(sent.ahead.space);
        }
    }

    /// Forgets a held focus (the user acted on the Desktop shown) and undoes what was
    /// arranged ahead for it ([`forget_undoes`]).
    pub(super) fn forget_held(&mut self, ob: &Observer) {
        if !forget_undoes(self.phase.changing(), self.ahead_awaits_release()) {
            return;
        }
        self.focus_replay.clear();
        if self.ahead.is_some() {
            self.undo_ahead(ob, "the user acted on the Desktop shown");
        }
    }

    /// About every tick: an arrangement whose Desktop did not come is undone ([`expire_step`]).
    pub(super) fn expire_ahead(&mut self, ob: &Observer, now: Instant) {
        self.expire_lent(now);
        let Some(sent) = &self.ahead else { return };
        let changed = self.space_changes != sent.changes_at;
        let waited = now.saturating_duration_since(sent.sent_at);
        let away = sent.trigger.away() && !changed;
        let cap = DOCK_SWITCH_CAP;
        if let Some(why) = expire_step(sent.trigger, self.phase.changing(), changed, waited, cap, self.ahead_contradicted(now)) {
            // Undone for want of a Space change: the wait ended by its cap.
            if away && waited >= cap {
                margin::wait("DOCK_SWITCH_CAP", waited, cap, End::Cap);
            }
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
            let entry = ParkedEntry { element: w.element.clone(), restore: frame, child: w.kind == WindowKind::Child, fitted: None };
            tracing::debug!(id = id.0, restore = %frame, "brought back for an arrival: registered until seen out of the park zone");
            self.keep_until_seen(id.0, entry);
        }
    }

    /// `debug-dock-click BUNDLE` (test only): the Dock item of a `STAGE_MANAGER_ONLY` app is
    /// found (its frame, from the Dock's lists), then clicked the way a real click goes: the
    /// hit-test at its center ([`dock::item_at`]), the prediction at the press, the
    /// arrangement at the release; then the item the hit-test found is pressed (`AXPress`,
    /// which activates the app as a click does, the Dock auto-hidden or not). With `real`, a
    /// press and a release are posted at the item once the hit-test found it there
    /// ([`crate::platform::input::dock_item_click`]): this thread returns at once, so the
    /// press reaches the controller ~100 ms before the release, and the Dock activates the app
    /// on the release as for a person's click (the fed path above arranges before `AXPress`,
    /// which hid the race of the owner's log of 11-15 06:32:34).
    /// `hold`: how long the press is held before its release: the posted one's
    /// ([`DOCK_PRESS_HOLD`] unless given), or the fed one's (none unless given: this thread
    /// waits it out between the press and the release, so what the press starts, the
    /// pictures a switch would wait for, has a person's time to come).
    pub(super) fn debug_dock_click(&mut self, ob: &Observer, bundle: &str, real: bool, hold: Option<Duration>) -> serde_json::Value {
        self.debug_dock(ob, bundle, DockAct::Click { real, hold })
    }

    /// `debug-dock-hover BUNDLE` (test only, round picture-wait): the pointer moved onto the
    /// Dock item of a `STAGE_MANAGER_ONLY` app, found and hit-tested as for
    /// [`Self::debug_dock_click`], and left there; nothing is pressed or fed. The reply's
    /// `in_bar` says whether that point lies in the Dock's bar as this app last read it
    /// (an auto-hidden Dock's is read where it rests): only then can the pointer's sign of
    /// a likely switch come (`platform/switch_likely.rs`).
    pub(super) fn debug_dock_hover(&mut self, ob: &Observer, bundle: &str) -> serde_json::Value {
        self.debug_dock(ob, bundle, DockAct::Hover)
    }

    /// `debug-dock-lend BUNDLE` (test only, round r2-lends): [`Self::debug_dock_click`]'s fed
    /// click (the hit-test, the press, the release) with no press of the item: what the press
    /// put home waits for an activation that does not come, as for a busy app, so a live check
    /// can tell what ends it (another app's activation, the next press, the cap). The reply's
    /// `lent` names the window put home, if any.
    pub(super) fn debug_dock_lend(&mut self, ob: &Observer, bundle: &str) -> serde_json::Value {
        let mut r = self.debug_dock(ob, bundle, DockAct::Lend);
        if let Some(obj) = r.as_object_mut() {
            obj.insert("lent".into(), serde_json::json!(self.lent.filter(|l| !l.switcher).map(|l| l.window.0)));
        }
        r
    }

    /// [`Self::debug_dock_click`], [`Self::debug_dock_lend`] and [`Self::debug_dock_hover`]:
    /// the item found, shown and hit-tested, then `act`.
    fn debug_dock(&mut self, ob: &Observer, bundle: &str, act: DockAct) -> serde_json::Value {
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
        // The Dock's hit-test answers for an auto-hidden Dock too, at the item's place off the
        // display: a posted click there goes nowhere (the live pass of 11-15), so an item off
        // the display is shown first whatever the hit-test said.
        let off_display = main.is_some_and(|d| !d.contains(at));
        if (hit.is_err() || off_display)
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
        // A posted press, or the pointer alone: only on the item the hit-test found there.
        let posted = |post: &dyn Fn() -> Result<(), String>, real: bool| match &hit {
            Ok((_, item)) if item.path == listed.path => match post() {
                Ok(()) => json!({ "ok": true, "real": real, "at": [at.x, at.y] }),
                Err(e) => json!({ "ok": false, "error": e }),
            },
            Ok((_, other)) => json!({ "ok": false, "error": format!("the hit-test at {at:?} found another item ({})", other.path) }),
            Err(miss) => json!({ "ok": false, "error": format!("the hit-test at {at:?} found no application item: {}", miss.found) }),
        };
        let mut result = match act {
            DockAct::Click { real: true, hold } => posted(&|| crate::platform::input::dock_item_click(at, hold.unwrap_or(DOCK_PRESS_HOLD)), true),
            DockAct::Hover => {
                let mut r = posted(&|| crate::platform::input::dock_item_hover(at), false);
                if let Some(obj) = r.as_object_mut() {
                    obj.insert("in_bar".into(), json!(crate::platform::switch_likely::in_dock(&self.dock_bar_now().bars, at)));
                }
                r
            }
            DockAct::Click { real: false, hold } => self.debug_dock_press(ob, &list, bundle, &listed, at, hit, Some(hold.unwrap_or_default())),
            DockAct::Lend => self.debug_dock_press(ob, &list, bundle, &listed, at, hit, None),
        };
        // The pointer shown at the Dock's edge goes back, but a hover's: it stays on the item.
        if revealed && act != DockAct::Hover {
            crate::platform::input::pointer_back();
        }
        if let Some(obj) = result.as_object_mut() {
            obj.insert("revealed_dock".into(), json!(revealed));
        }
        result
    }

    /// The rest of [`Self::debug_dock_click`]: the hit-test's answer `hit` at `at` goes
    /// through the press and release path, the press held `activate` between them, then the
    /// item is pressed (`AXPress`); `None`: the click fed alone, the item not pressed.
    #[allow(clippy::too_many_arguments)]
    fn debug_dock_press(
        &mut self,
        ob: &Observer,
        list: &[CgWindow],
        bundle: &str,
        listed: &DockItem,
        at: crate::engine::geometry::Point,
        hit: Result<(AxElement, DockItem), dock::Miss>,
        activate: Option<Duration>,
    ) -> serde_json::Value {
        use serde_json::json;
        let (element, item) = match hit {
            Ok(hit) if hit.1.path == listed.path => hit,
            Ok((_, other)) => return json!({ "ok": false, "error": format!("the hit-test at {at:?} found another item ({})", other.path) }),
            Err(miss) => return json!({ "ok": false, "error": format!("the hit-test at {at:?} found no application item: {}", miss.found) }),
        };
        let pressed_at = clock::now();
        self.press_item(ob, list, item, at, pressed_at);
        // The press held, as a person's is (`hold=MS`; none: the release at once).
        if let Some(hold) = activate.filter(|h| !h.is_zero()) {
            clock::sleep(hold);
        }
        let predicted = self.dock_released(ob, at, false, clock::now());
        if activate.is_none() {
            tracing::info!(bundle, predicted = ?predicted.map(|w| w.0), "debug-dock-lend: the click fed, the item not pressed");
            return json!({ "ok": true, "predicted": predicted.map(|w| w.0) });
        }
        let pressed = element.bounded(QUERY_AX_TIMEOUT_SECONDS * 4.0, |e| e.perform_action(names::PRESS));
        tracing::info!(bundle, predicted = ?predicted.map(|w| w.0), pressed = pressed.is_ok(), "debug-dock-click");
        json!({ "ok": pressed.is_ok(), "predicted": predicted.map(|w| w.0), "error": pressed.err().map(|e| e.to_string()) })
    }
}

/// What a test does at a fixture's Dock item ([`Controller::debug_dock`]).
#[derive(Clone, Copy, Debug, PartialEq)]
enum DockAct {
    /// A click: posted (`real`, the press held `hold`, [`DOCK_PRESS_HOLD`] unless given), or
    /// fed to the controller and the item pressed through Accessibility (`hold` waited out
    /// between the fed press and its release; none: the release at once).
    Click { real: bool, hold: Option<Duration> },
    /// The fed click alone, the item not pressed.
    Lend,
    /// The pointer moved onto the item and left there.
    Hover,
}

/// App `pid`'s focused window, asked with a bounded timeout (the app goes back to the global
/// one after).
pub(super) fn focused_window_of(pid: i32) -> Option<WindowId> {
    AxElement::application(pid)
        .bounded(QUERY_AX_TIMEOUT_SECONDS, |app| app.focused_window().ok().and_then(|w| w.window_id_within(QUERY_AX_TIMEOUT_SECONDS).ok()))
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

    /// The controller's fields an arrangement made at a Dock press meets, driven through the
    /// real steps ([`ahead_contradicted_by`], [`expire_step`], [`hold_step`],
    /// [`forget_undoes`], [`release_step`]) in the order of the live pass of 11-15 09:53:19.
    struct PressRun {
        /// The window the arrangement stands for, and whether it was made at the press.
        ahead: Option<(WindowId, bool)>,
        /// The press kept, no mouse-up yet.
        press_down: bool,
        held: Option<Wanted>,
        /// Arrangements sent.
        sent: u32,
        sent_at: Duration,
        changed: bool,
        undone: Option<&'static str>,
    }

    const PID: i32 = 10;
    const WINDOW: WindowId = WindowId(52765);

    impl PressRun {
        fn resolve(id: WindowId) -> Option<WindowId> {
            matches!(id.0, 52765 | 52766 | 52781).then_some(id)
        }

        /// A press on the Dock whose click goes to `WINDOW`, off every Desktop (`spaces`: the
        /// Spaces engine), at 0 ms; the press's arrangement went out at `at`.
        fn press(spaces: bool, at: Duration) -> Self {
            let mut run = Self { ahead: None, press_down: true, held: None, sent: 0, sent_at: at, changed: false, undone: None };
            if spaces {
                run.ahead = Some((WINDOW, true));
                run.sent = 1;
            }
            run
        }

        fn contradicted(&self) -> bool {
            let Some((window, at_press)) = self.ahead else { return false };
            ahead_contradicted_by(self.held, window, PID, at_press, self.press_down, Self::resolve)
        }

        fn undo(&mut self, why: &'static str) {
            self.ahead = None;
            self.undone = Some(why);
        }

        /// A tick at `t` ([`Controller::expire_ahead`]).
        fn tick(&mut self, t: Duration) {
            let Some(_) = self.ahead else { return };
            let waited = t.saturating_sub(self.sent_at);
            if let Some(why) = expire_step(Trigger::DockClick, false, self.changed, waited, DOCK_SWITCH_CAP, self.contradicted()) {
                self.undo(why);
            }
        }

        /// A focus or activation held at `t` ([`Controller::after_hold`]).
        fn hold(&mut self, wanted: Wanted) {
            self.held = Some(wanted);
            let (undo, _) = hold_step(self.contradicted(), self.ahead.is_some(), matches!(wanted, Wanted::Window { .. }), false, false);
            if undo {
                self.undo("another focus or activation came");
            }
        }

        /// The user acting on the Desktop shown ([`Controller::forget_held`]).
        fn forget(&mut self) {
            let awaiting = self.ahead.is_some_and(|(_, at_press)| awaits_release(at_press, self.press_down));
            if forget_undoes(false, awaiting) {
                self.held = None;
                if self.ahead.is_some() {
                    self.undo("the user acted on the Desktop shown");
                }
            }
        }

        /// The release at `t`, a click ([`Controller::dock_clicked`]).
        fn release(&mut self, t: Duration) {
            self.press_down = false;
            match release_step(self.ahead, WINDOW) {
                ReleaseStep::HoldPress => self.held = Some(Wanted::Window { id: WINDOW, pid: PID }),
                ReleaseStep::Stands | ReleaseStep::Another => {}
                ReleaseStep::Arrange => {
                    self.ahead = Some((WINDOW, false));
                    self.held = Some(Wanted::Window { id: WINDOW, pid: PID });
                    self.sent += 1;
                    self.sent_at = t;
                }
            }
        }
    }

    #[test]
    fn an_arrangement_made_at_a_dock_press_stands_through_a_tick_before_its_release_and_goes_out_once() {
        let ms = Duration::from_millis;
        // 09:53:19: the press at 0, its arrangement out at +30 ms, a tick 1 ms later, the
        // release at +117 ms.
        let mut run = PressRun::press(true, ms(30));
        run.tick(ms(31));
        assert_eq!(run.undone, None, "the tick 1 ms after the arrangement: nothing held yet, and it stands");
        assert_eq!(run.ahead, Some((WINDOW, true)));
        run.tick(ms(85));
        assert_eq!(run.ahead, Some((WINDOW, true)), "a brisk tick within the press stands it too");
        run.release(ms(117));
        assert_eq!(run.held, Some(Wanted::Window { id: WINDOW, pid: PID }), "the click's focus held at the release");
        assert_eq!(run.ahead, Some((WINDOW, true)), "the release keeps it as it is");
        run.tick(ms(118));
        run.tick(ms(368));
        assert_eq!(run.undone, None, "the held focus is its own");
        // The Space change notice ~0.4 s after the press: the arrival decides from here.
        run.changed = true;
        run.tick(ms(1500));
        assert_eq!(run.undone, None);
        assert_eq!(run.sent, 1, "sent once, at the press: never again at the release");
    }

    #[test]
    fn before_the_fix_the_same_tick_undid_it_and_the_release_sent_it_again() {
        let ms = Duration::from_millis;
        // The cause: nothing is held between the press and the release, and nothing held
        // contradicts any arrangement.
        assert!(contradicts(None, WINDOW, PID, PressRun::resolve));
        assert_eq!(expire_step(Trigger::DockClick, false, false, ms(1), DOCK_SWITCH_CAP, true), Some("its Desktop did not come"));
        // Replayed with the press not counted (the old reading): undone at +31 ms, sent again
        // at the release, too late for macOS.
        let mut run = PressRun::press(true, ms(30));
        run.press_down = false;
        run.tick(ms(31));
        assert_eq!(run.undone, Some("its Desktop did not come"));
        run.release(ms(117));
        assert_eq!(run.sent, 2);
    }

    #[test]
    fn an_arrangement_made_at_a_dock_press_with_no_space_change_goes_after_the_wait() {
        let ms = Duration::from_millis;
        let mut run = PressRun::press(true, ms(30));
        run.release(ms(120));
        run.tick(ms(30) + DOCK_SWITCH_CAP - ms(1));
        assert_eq!(run.undone, None);
        run.tick(ms(30) + DOCK_SWITCH_CAP);
        assert_eq!(run.undone, Some("no Space change came after the Dock click (or the focus it was arranged for)"));
        assert_eq!(run.sent, 1);
        // A release the monitor never saw: the wait ends it all the same.
        let mut run = PressRun::press(true, ms(30));
        run.tick(ms(500));
        assert_eq!(run.undone, None, "still pressed for all we know");
        run.tick(ms(30) + DOCK_SWITCH_CAP);
        assert!(run.undone.is_some(), "no Space change: undone with the button still counted down");
    }

    #[test]
    fn nothing_held_or_done_between_a_dock_press_and_its_release_undoes_its_arrangement() {
        let ms = Duration::from_millis;
        let mut run = PressRun::press(true, ms(30));
        run.hold(Wanted::App(20));
        assert_eq!(run.undone, None, "another app's activation during the press");
        run.hold(Wanted::Window { id: WindowId(52781), pid: PID });
        assert_eq!(run.undone, None, "a focus of the app's window on the Desktop shown during the press");
        run.forget();
        assert_eq!(run.undone, None, "the user's act on the Desktop shown waits for the click's end");
        run.tick(ms(60));
        assert_eq!(run.undone, None);
        run.release(ms(110));
        run.tick(ms(111));
        assert_eq!(run.undone, None, "the click's own focus replaced what was held");
        assert_eq!(run.sent, 1);
        // After the release the old rules hold again: another known window's focus undoes it.
        run.hold(Wanted::Window { id: WindowId(52781), pid: PID });
        assert_eq!(run.undone, Some("another focus or activation came"));
    }

    #[test]
    fn a_release_keeps_the_arrangement_made_at_its_press_and_arranges_only_when_none_stands() {
        let (w, other) = (WINDOW, WindowId(9));
        assert_eq!(release_step(Some((w, true)), w), ReleaseStep::HoldPress, "made at the press: kept, its focus held");
        assert_eq!(release_step(Some((w, false)), w), ReleaseStep::Stands);
        assert_eq!(release_step(Some((other, true)), w), ReleaseStep::Another);
        assert_eq!(release_step(None, w), ReleaseStep::Arrange, "the Corner engine: arranged at the release as before");
        // The Corner engine arranges at the release only: at_press never set, the rules as
        // before for every arrangement it makes.
        let mut run = PressRun::press(false, Duration::ZERO);
        run.release(Duration::from_millis(100));
        assert_eq!((run.sent, run.ahead), (1, Some((w, false))));
        assert!(!awaits_release(false, true));
        let resolve = PressRun::resolve;
        for held in [None, Some(Wanted::App(20)), Some(Wanted::Window { id: WindowId(52781), pid: PID })] {
            assert_eq!(ahead_contradicted_by(held, w, PID, false, true, resolve), contradicts(held, w, PID, resolve));
            assert_eq!(ahead_contradicted_by(held, w, PID, true, false, resolve), contradicts(held, w, PID, resolve), "released");
        }
        assert!(!forget_undoes(false, true));
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
        assert!(!forget_undoes(true, false));
        assert!(forget_undoes(false, false));
    }

    #[test]
    fn a_dock_click_with_no_space_change_after_it_is_undone() {
        let ms = Duration::from_millis;
        let dock = Trigger::DockClick;
        let cap = DOCK_SWITCH_CAP;
        assert_eq!(expire_step(dock, false, false, ms(400), cap, false), None, "the slide is under way");
        assert_eq!(expire_step(dock, false, false, ms(1500), cap, false), None, "a busy Mac's notice still to come, whatever the engine");
        assert!(expire_step(dock, false, false, cap, cap, false).is_some(), "the cap");
        assert_eq!(expire_step(dock, false, true, ms(4500), cap, false), None, "the change came: the arrival decides");
        assert_eq!(expire_step(dock, true, false, ms(4500), cap, true), None, "never while a change settles");
        assert!(expire_step(Trigger::Notice, false, true, ms(10), cap, true).is_some(), "the hold is gone");
        assert_eq!(expire_step(Trigger::Notice, false, false, ms(5000), cap, false), None);
        // Arranged for a focus the engine hid off its Desktop: away as a Dock click's is,
        // and on the same cap.
        assert!(Trigger::Hidden.away());
        assert_eq!(expire_step(Trigger::Hidden, false, false, ms(1500), cap, false), None);
        assert!(expire_step(Trigger::Hidden, false, false, cap, cap, false).is_some(), "no Space change came for the focus");
    }

    #[test]
    fn a_window_put_home_at_a_dock_press_goes_back_unless_its_stage_comes_in_front() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let wait = || LendWait::begun(40, t0, LEND_CAP);
        assert_eq!(lent_step(&mut wait(), true, false, ms(100)), None, "the activation not here yet");
        assert_eq!(lent_step(&mut wait(), true, true, ms(100)), Some(Outcome::Met), "its stage in front: the switch shows it");
        assert_eq!(lent_step(&mut wait(), false, false, ms(100)), Some(Outcome::Met), "the model shows it");
        assert_eq!(lent_step(&mut wait(), true, true, ms(0) + LEND_WAIT * 2), Some(Outcome::Met), "the switch's show at the unveil may come late");
        assert_eq!(lent_step(&mut wait(), true, false, ms(1500)), None, "a busy Mac: the old 1 s gone, still waiting");
        assert_eq!(lent_step(&mut wait(), true, false, ms(0) + LEND_WAIT), Some(Outcome::Cap), "no word either way: back unshown");
        // Another app activated after the press: back at once.
        let mut w = wait();
        assert!(matches!(w.activation(77, ms(120), ms(120)), Some(Outcome::Refuted(_))));
    }

    /// Review B-6: a Dock press that was no click, or one kept from a missed release, leaves
    /// the window put home for ⌘Tab's switcher; a press that puts another window home takes
    /// it back (one lend at a time); an earlier press's lend always goes.
    #[test]
    fn a_dock_press_leaves_the_switchers_lend_unless_it_lends_another() {
        assert!(!dock_takes_back(true, false), "no click, or a press that puts nothing home: the switcher's stays");
        assert!(dock_takes_back(true, true), "another window put home: the switcher's goes back");
        assert!(dock_takes_back(false, false), "an earlier press's: back");
        assert!(dock_takes_back(false, true));
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

    /// The park zone of the tests: the bottom-right corner of [`MAIN`].
    fn parks(r: Rect) -> bool {
        r.x >= 1700.0 && r.y >= 1100.0
    }

    #[test]
    fn a_restore_is_missing_only_while_parked_and_unjudged_while_the_desktop_slides() {
        let parked = Rect::new(1785.0, 1155.0, 520.0, 392.0);
        let back = Expect::new(2, Rect::new(80.0, 78.0, 520.0, 392.0), Some(parked), false);
        let at = |r: Rect, onscreen| win(2, 20, 0, onscreen, r);
        let judge_at = |r: Rect, onscreen| judge(&back, Some(&at(r, onscreen)), MAIN, parks);
        assert_eq!(judge_at(parked, false), Seen::Missing, "still parked");
        assert_eq!(judge_at(Rect::new(1786.0, 1150.0, 520.0, 392.0), false), Seen::Missing, "anywhere in the park zone");
        assert_eq!(judge_at(Rect::new(81.0, 78.0, 520.0, 392.0), false), Seen::Taken);
        assert_eq!(judge_at(Rect::new(80.0, 78.0, 507.0, 388.0), false), Seen::Taken, "a size the app chose (a terminal's grid) (R9 3)");
        assert_eq!(judge_at(Rect::new(300.0, 240.0, 520.0, 392.0), false), Seen::Taken, "anywhere out of the park zone (R10 1)");
        // The live run of 10-13: the arriving Desktop's windows listed shifted, shrinking to 0.
        for x in [1880.0, 1597.0, 886.0, 343.0] {
            assert_eq!(judge_at(Rect::new(x, 78.0, 520.0, 392.0), true), Seen::Sliding, "slid by {} from its frame", x - 80.0);
        }
        assert_eq!(judge_at(Rect::new(-1500.0, 78.0, 520.0, 392.0), true), Seen::Sliding, "sliding in from the left");
        assert_eq!(judge_at(Rect::new(985.0, 1155.0, 520.0, 392.0), true), Seen::Sliding, "still parked, slid from the park");
        assert_eq!(judge_at(Rect::new(2400.0, 400.0, 520.0, 392.0), true), Seen::Sliding, "on screen, off the display");
        assert_eq!(judge(&back, None, MAIN, parks), Seen::Missing, "not listed");
        let owed = Expect { nudged_only: true, ..back };
        assert_eq!(judge(&owed, Some(&at(Rect::new(81.0, 78.0, 520.0, 392.0), false)), MAIN, parks), Seen::Missing, "1 pt off: the exact move is owed");
    }

    #[test]
    fn a_park_is_missing_only_while_at_its_frame_before_the_move() {
        let was = Rect::new(80.0, 78.0, 600.0, 400.0);
        let park = Expect::new(1, Rect::new(1785.0, 1155.0, 600.0, 400.0), Some(was), true);
        let judge_at = |r: Rect| judge(&park, Some(&win(1, 10, 0, false, r)), MAIN, parks);
        assert_eq!(judge_at(was), Seen::Missing, "still where it was");
        assert_eq!(judge_at(Rect::new(81.0, 79.0, 600.0, 400.0)), Seen::Missing, "within 2 pt of it");
        assert_eq!(judge_at(Rect::new(1786.0, 1150.0, 600.0, 400.0)), Seen::Taken, "in the park zone");
        assert_eq!(judge_at(Rect::new(1200.0, 78.0, 600.0, 400.0)), Seen::Sliding, "its old frame slid sideways");
        let unknown = Expect { before: None, ..park };
        assert_eq!(state(&unknown, was, parks), Seen::Missing, "no frame before: out of the park zone is not taken");
        assert_eq!(state(&unknown, Rect::new(1786.0, 1150.0, 600.0, 400.0), parks), Seen::Taken);
    }

    #[test]
    fn a_park_neither_near_its_frame_before_nor_in_the_zone_is_sent_once_more() {
        // R11 5: macOS clamped it short of the zone, or the list is stale: not counted taken.
        let was = Rect::new(80.0, 78.0, 600.0, 400.0);
        let park = Expect::new(1, Rect::new(1785.0, 1155.0, 600.0, 400.0), Some(was), true);
        let short = Rect::new(1600.0, 1000.0, 600.0, 400.0);
        let seen = judge(&park, Some(&win(1, 10, 0, false, short)), MAIN, parks);
        assert_eq!(seen, Seen::Unsure);
        assert!(owed(&park, seen), "never sent again: once more");
        let sent = Expect { tries: 1, ..park };
        assert!(!owed(&sent, seen), "sent once: left as it is");
        assert!(owed(&sent, Seen::Missing), "still at its frame before: owed as ever");
        assert!(!owed(&park, Seen::Taken) && !owed(&park, Seen::Sliding));
        // The arrival's check: an unsure park goes once, only if never sent again.
        assert_eq!(tail_step(&[park], &[Some(Seen::Unsure)], false), TailStep::Send(vec![0]));
        assert_eq!(tail_step(&[sent], &[Some(Seen::Unsure)], false), TailStep::Send(vec![]));
    }

    #[test]
    fn a_parked_windows_restore_starts_from_its_park_frame_as_production_reads_it() {
        use crate::engine::rules::{Decision, Placement, RuleAction};
        use crate::engine::testkit::{display, macos_places, meta, model};
        // R11 1: window 1 parked in its stage, window 2's stage active; going back to 1's.
        let mut m = model();
        let obs = Observation::default();
        let new = Decision { action: RuleAction::New, placement: Placement::AsIs, ..Decision::default() };
        m.add_window(meta(1, 10, "com.example.a"), &new, &obs);
        m.add_window(meta(2, 11, "com.example.b"), &new, &obs);
        let d = display();
        let home = m.window(WindowId(1)).expect("managed").meta.frame;
        assert!(m.window(WindowId(1)).is_some_and(|w| w.parked) && !d.parks(home), "its meta frame is still its home frame");
        let park = m.frame_left(WindowId(1)).expect("managed");
        assert!(d.parks(park), "the model's park frame: {park}");
        // Where macOS left it (its title bar above the Dock), as the window server lists it.
        let placed = macos_places(park);
        // As `Controller::arrange_ahead` does: the frames before read first, the moves after.
        let left = frames_left(&m);
        let unlisted = frames_before(&left, &[]);
        let before = frames_before(&left, &[win(1, 10, 0, false, placed), win(2, 11, 0, true, home)]);
        let stage = m.stage_of(WindowId(1)).expect("staged");
        let actions = m.switch_to(stage, &obs);
        assert_eq!(unlisted.get(&1), Some(&park), "not listed: the model's park frame");
        let expected = expects(&actions, |w| park_frame(&m, WindowId(w)), |w| before.get(&w).copied(), |_| None);
        let restore = *expected.iter().find(|e| e.id == 1 && !e.park).expect("window 1 brought back");
        assert_eq!(restore.before, Some(placed), "listed: the window server's bounds at send time");
        let judge_at = |e: &Expect, r: Rect, onscreen| judge(e, Some(&win(1, 10, 0, onscreen, r)), d.frame, |r| d.parks(r));
        // Mid-slide: listed at (x - dx, y), partly on the display, out of the park zone.
        let mid = Rect::new(placed.x - 700.0, placed.y, placed.w, placed.h);
        assert!(d.frame.intersect(&mid).is_some() && !d.parks(mid));
        assert_eq!(judge_at(&restore, mid, true), Seen::Sliding, "slid from where it was parked, not taken");
        let as_before_r11 = Expect { before: Some(home), ..restore };
        assert_eq!(judge_at(&as_before_r11, mid, true), Seen::Taken, "the home frame as `before`: a false taken");
        assert_eq!(judge_at(&restore, placed, false), Seen::Missing, "still parked");
        assert_eq!(judge_at(&restore, restore.frame, true), Seen::Taken);
    }

    #[test]
    fn the_arrivals_check_waits_out_the_slide_and_then_sends_what_is_not_taken() {
        // R11 2: the arrival's list may be mid-slide.
        let r = Rect::new(80.0, 78.0, 520.0, 392.0);
        let moves = [Expect::new(1, r, None, false), Expect::new(2, r, None, true), Expect::new(3, r, None, false)];
        let (t, m, sl) = (Some(Seen::Taken), Some(Seen::Missing), Some(Seen::Sliding));
        assert_eq!(tail_step(&moves, &[t, m, sl], false), TailStep::Wait, "the list shows the slide: look again");
        assert_eq!(tail_step(&moves, &[t, m, None], false), TailStep::Wait, "a window not listed: look again");
        assert_eq!(tail_step(&moves, &[t, m, t], false), TailStep::Send(vec![1]), "settled: what is not taken");
        assert_eq!(tail_step(&moves, &[m, m, sl], true), TailStep::Send(vec![0, 1, 2]), "time out: sliding ones go too");
        assert_eq!(tail_step(&moves, &[m, None, t], true), TailStep::Send(vec![0]), "time out: an unlisted window is gone");
        let mut sent = moves;
        sent[1].final_sent = true;
        assert_eq!(tail_step(&sent, &[m, m, t], false), TailStep::Send(vec![0]), "once each");
        assert_eq!(tail_step(&moves, &[t, t, t], false), TailStep::Send(vec![]), "all taken: done");
        assert!(ARRIVAL_CHECK_FOR <= Duration::from_millis(400));
    }

    #[test]
    fn a_window_the_user_acted_on_is_no_longer_checked() {
        // R11 3: a sliver grab unparks it; nothing parks it again.
        let r = Rect::new(80.0, 78.0, 520.0, 392.0);
        let mut moves = vec![Expect::new(1, r, None, true), Expect::new(2, r, None, false)];
        assert!(drop_move(&mut moves, 1));
        assert_eq!(moves.iter().map(|e| e.id).collect::<Vec<_>>(), vec![2]);
        assert!(!drop_move(&mut moves, 1), "only once");
        assert_eq!(tail_step(&moves, &[Some(Seen::Missing)], false), TailStep::Send(vec![0]), "window 2 only");
    }

    #[test]
    fn a_rounds_resends_share_100_ms_and_the_rest_wait() {
        // R11 4: each send is false once the round's time is up.
        let ms = Duration::from_millis;
        let mut clock = ms(0);
        let mut sent = Vec::new();
        let left = send_round(&[0, 1, 2, 3], |i| {
            let Some(_) = round_timeout(RESEND_ROUND_CAP, clock) else { return false };
            sent.push(i);
            clock += ms(45);
            true
        });
        assert_eq!(sent, vec![0, 1, 2], "0, 45 and 90 ms: under the cap");
        assert_eq!(left, vec![3], "135 ms: next round");
        assert_eq!(send_round(&[4, 5], |_| true), Vec::<usize>::new());
        let at90 = round_timeout(RESEND_ROUND_CAP, ms(90)).expect("10 ms left");
        assert!((at90 - 0.01).abs() < 1e-4, "the last send has what is left: {at90}");
    }

    #[test]
    fn a_window_element_is_not_asked_once_the_round_is_over() {
        // R11 4: the window element gets the round's timeout; with none left it is not asked.
        let app = AxElement::application(std::process::id() as i32);
        assert_eq!(window_id_within(&app, None), Err(AxError::CANNOT_COMPLETE));
    }

    #[test]
    fn a_resend_counts_when_the_listing_changed_or_after_its_spacing() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let here = Some(Rect::new(1785.0, 1155.0, 520.0, 392.0));
        let fresh = Expect::new(2, Rect::new(80.0, 78.0, 520.0, 392.0), None, false);
        assert!(resend_due(&fresh, here, t0), "never sent again");
        let sent = Expect { tries: 1, last: Some((t0, here)), ..fresh };
        assert!(!resend_due(&sent, here, at(20)), "the same listing a tick later: not again");
        assert!(!resend_due(&sent, here, at(99)));
        assert!(resend_due(&sent, here, at(100)), "spaced 100 ms");
        assert!(resend_due(&sent, Some(Rect::new(1790.0, 1155.0, 520.0, 392.0)), at(20)), "the listing changed");
        assert!(!resend_due(&Expect { tries: RESEND_MAX, ..sent }, here, at(500)), "the budget spent");
        assert!(!resend_due(&Expect { gave_up: true, ..sent }, here, at(500)), "given up");
        // The slide's ~0.45 s: four 20 ms rounds of an unchanged listing spend one try, not four.
        let mut e = fresh;
        for round in 0..23u64 {
            let now = at(20 * round);
            if resend_due(&e, here, now) {
                e.tries += 1;
                e.last = Some((now, here));
            }
        }
        assert_eq!(e.tries, RESEND_MAX, "spread over the slide: 0, 100 and 200 ms");
        assert_eq!(e.last.map(|(t, _)| t), Some(at(200)));
    }

    #[test]
    fn the_moves_carry_their_frames_before() {
        let park = Rect::new(1785.0, 1155.0, 600.0, 400.0);
        let back = Rect::new(80.0, 78.0, 520.0, 392.0);
        let actions = [
            Action::Hide { id: WindowId(1) },
            Action::SetFrame { id: WindowId(2), frame: back },
            Action::Raise { id: WindowId(2) },
            Action::StateChanged,
        ];
        let before = |id: u32| (id == 1).then_some(back);
        assert_eq!(expects(&actions, |id| (id == 1).then_some(park), before, |_| None), vec![Expect::new(1, park, Some(back), true), Expect::new(2, back, None, false)], "moves only");
        assert_eq!(moved_ids(&actions), vec![1, 2], "the windows asked of the window server before the moves (R13 3)");
    }

    #[test]
    fn a_window_waiting_counts_for_a_dock_click_only_on_a_desktop_not_shown() {
        use crate::engine::model::desks::Desk;
        let desk = |id: u64| Desk { id, uuid: String::new() };
        // One screen: Desktops 1 and 4, Desktop 4 shown, a full-screen Space 30.
        let screens = [ScreenDesks { screen: "Main".into(), current: 4, current_kind: 0, desks: vec![desk(1), desk(4)], others: vec![(30, 4)] }];
        let waits = |on: &[u64]| waits_elsewhere(Some(on), Some(&screens));
        assert!(!waits(&[]), "listed on no Space: a tab not selected (live 11-23)");
        assert!(!waits(&[5732]), "in a Space of the engine's own: on no Desktop");
        assert!(!waits(&[4]), "on the Desktop shown");
        assert!(waits(&[1]), "on a Desktop not shown: minimized there, or never visited");
        assert!(waits(&[30]), "in a full-screen Space not shown");
        assert!(waits(&[5732, 1]), "listed on a Desktop too");
        assert!(waits_elsewhere(None, Some(&screens)), "the engine reads no Spaces (Corner): it may be");
        assert!(waits_elsewhere(Some(&[]), None), "the screens' listing not read: it may be");
    }

    #[test]
    fn the_press_round_stops_at_the_first_failure_and_skips_main_when_focused_answers() {
        use std::cell::Cell;
        let timeout: AxResult<bool> = Err(AxError::CANNOT_COMPLETE);
        let asked = Cell::new(0);
        let ask = |v: AxResult<u32>| {
            asked.set(asked.get() + 1);
            v
        };
        let never = || false;
        let a = press_round(|| timeout, || panic!("asked after a failure"), || panic!(), || panic!(), never);
        assert_eq!(a, PressAnswer::default(), "hidden failed: nothing more (no prediction)");
        let a = press_round(|| Ok(false), || Err(AxError::CANNOT_COMPLETE), || panic!(), || panic!(), never);
        assert_eq!(a, PressAnswer { hidden: Some(false), ..Default::default() }, "active failed: stop");
        let a = press_round(|| Ok(false), || Ok(true), || ask(Ok(7)), || ask(Ok(8)), never);
        assert_eq!(a, PressAnswer { hidden: Some(false), active: Some(true), focused: Some(7), main: None });
        assert_eq!(asked.get(), 1, "the main window not asked when the focused one answered");
        let a = press_round(|| Ok(false), || Ok(false), || ask(Err(AxError::NO_VALUE)), || ask(Ok(8)), never);
        assert_eq!(a.main, Some(8), "no focused window: the main one");
        let a = press_round(|| Ok(false), || Ok(false), || Err(AxError::CANNOT_COMPLETE), || panic!("asked after a timeout"), never);
        assert_eq!((a.focused, a.main), (None, None), "the focused window timed out: stop");
        let calls = Cell::new(0);
        let up_after_one = || {
            calls.set(calls.get() + 1);
            calls.get() > 1
        };
        let a = press_round(|| Ok(true), || Ok(false), || panic!("over the round's cap"), || panic!(), up_after_one);
        assert_eq!(a, PressAnswer { hidden: Some(true), active: Some(false), ..Default::default() }, "the cap reached");
    }

    #[test]
    fn the_press_rounds_questions_share_100_ms() {
        let ms = Duration::from_millis;
        let press = |t| round_timeout(PRESS_ROUND_CAP, t);
        assert_eq!(press(ms(0)), Some(QUERY_AX_TIMEOUT_SECONDS));
        assert_eq!(press(ms(30)), Some(QUERY_AX_TIMEOUT_SECONDS));
        let left = press(ms(70)).expect("30 ms left");
        assert!((left - 0.03).abs() < 1e-4, "what is left: {left}");
        assert_eq!(press(ms(96)), None, "under 5 ms left: the round stops");
        assert_eq!(press(ms(250)), None);
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

    /// Desktop 1: an editor's window (1) in its stage, parked; a browser's (2) in the stage
    /// active there. Desktop 2 shown (windows 3 and 4). The editor's stage is arranged ahead
    /// on Desktop 1 (a Dock click on the editor): the model, the arrangement, its moves to
    /// check and its mark.
    fn arranged() -> (Model, Ahead, Vec<Expect>, AheadMark) {
        use crate::engine::persist::LaunchHints;
        use crate::engine::testkit::{MAIN, meta, model};
        let hints = |z: &[u32], pid| LaunchHints { z_order: z.iter().map(|&i| WindowId(i)).collect(), frontmost: Some(pid) };
        let mut m = model();
        m.arrive(MAIN, &[meta(1, 10, "com.example.editor"), meta(2, 20, "com.example.browser")], &[], &hints(&[1, 2], 10));
        let browser = m.stage_of(WindowId(2)).expect("browser stage");
        m.switch_to(browser, &Observation::default());
        let d2 = [meta(3, 10, "com.example.editor"), meta(4, 30, "com.example.chat")];
        m.arrive(MAIN, &d2, &[WindowId(1), WindowId(2)], &hints(&[3, 4], 10));
        assert!(m.waits_on_another_space(WindowId(1)));
        let (ahead, actions) = m.arrange_ahead(WindowId(1), None).expect("arranged");
        let moves = expects(&actions, |w| park_frame(&m, WindowId(w)), |_| None, |w| m.stage_of(WindowId(w)));
        let mark = m.ahead_mark(&ahead);
        assert_eq!(moves.iter().map(|e| (e.id, e.park)).collect::<Vec<_>>(), vec![(2, true), (1, false)]);
        (m, ahead, moves, mark)
    }

    /// The arrival on Desktop 1 (`live`: its windows on screen), as the controller runs it:
    /// recognized, then the arrangement confirmed.
    fn arrive_on_desktop_1(m: &mut Model, ahead: &Ahead, live: &[u32]) -> Vec<Action> {
        use crate::engine::persist::LaunchHints;
        use crate::engine::testkit::{MAIN, meta};
        let bundle = |id: u32| if id == 1 { (10, "com.example.editor") } else { (20, "com.example.browser") };
        let live: Vec<_> = live.iter().map(|&id| meta(id, bundle(id).0, bundle(id).1)).collect();
        let hints = LaunchHints { z_order: live.iter().map(|w| w.id).collect(), frontmost: Some(10) };
        let mut actions = m.arrive(MAIN, &live, &[WindowId(3), WindowId(4)], &hints);
        assert_eq!(m.space_of(WindowId(2)), Some(ahead.space));
        assert!(!m.waits_on_another_space(WindowId(2)), "Desktop 1 is the one shown");
        actions.extend(m.finish_ahead(ahead));
        actions
    }

    #[test]
    fn the_desktop_arranged_ahead_arriving_without_its_window_sends_no_move_again() {
        // R13 1: the arrival's batch brings back the stage that was there; the tail must not
        // park it and bring back the stage arranged over it.
        assert_eq!(arrival_check(true, true), Ok(()));
        assert!(arrival_check(true, false).is_err(), "hit, the window gone from its stage: no tail");
        assert!(arrival_check(false, true).is_err() && arrival_check(false, false).is_err(), "another Desktop");
        let (mut m, ahead, mut moves, mark) = arranged();
        assert_eq!(screen(&m, &mark, &mut moves.clone()), Ok(vec![]), "as arranged: every move checked");
        // The editor's window closes during the slide.
        m.remove_window(WindowId(1));
        let present = m.stage_of(ahead.window) == Some(ahead.stage);
        let batch = arrive_on_desktop_1(&mut m, &ahead, &[2]);
        assert!(!present && arrival_check(true, present).is_err());
        assert!(batch.iter().any(|a| matches!(a, Action::SetFrame { id, .. } if id.0 == 2)), "the browser's stage comes back: {batch:?}");
        // Even a tail left running sends nothing: the stage arranged is not active any more,
        // and no move is as the model has it.
        assert!(screen(&m, &mark, &mut moves).is_err());
        let (_, _, again, _) = arranged();
        assert!(again.iter().all(|e| !m.move_holds(WindowId(e.id), e.park, e.stage)), "the browser's park and the editor's restore both moved on");
    }

    #[test]
    fn a_switch_during_the_arrivals_check_ends_it_without_touching_the_stages_windows() {
        // R13 1: a card click, a sliver grab or an activation within the tail switches the
        // stage: the check ends, whichever window was clicked.
        let (mut m, ahead, moves, mark) = arranged();
        arrive_on_desktop_1(&mut m, &ahead, &[1, 2]);
        assert_eq!(m.active_stage_in(ahead.space), Some(ahead.stage));
        assert_eq!(screen(&m, &mark, &mut moves.clone()), Ok(vec![]), "the arrival confirmed it: the tail checks every move");
        let mut switched = m.clone();
        switched.switch_to(ahead.previous, &Observation::default());
        assert!(screen(&switched, &mark, &mut moves.clone()).is_err(), "the browser's stage switched to");
        // Away and back within one tick: the stage arranged is active again, but switched.
        switched.switch_to(ahead.stage, &Observation::default());
        assert_eq!(switched.active_stage_in(ahead.space), Some(ahead.stage));
        assert!(screen(&switched, &mark, &mut moves.clone()).is_err(), "a switch touched it: its windows are the switch's");
        // An undo (the stage that was there back) ends it too.
        let mut undone = m.clone();
        undone.switch_to(ahead.previous, &Observation::default());
        assert!(screen(&undone, &mark, &mut moves.clone()).is_err());
    }

    #[test]
    fn a_move_whose_window_the_model_moved_on_from_is_dropped_from_the_check() {
        let (mut m, ahead, mut moves, mark) = arranged();
        arrive_on_desktop_1(&mut m, &ahead, &[1, 2]);
        // The browser's window (parked ahead) closes: its park is no longer checked.
        m.remove_window(WindowId(2));
        assert_eq!(screen(&m, &mark, &mut moves), Ok(vec![2]));
        assert_eq!(moves.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1], "the editor's restore only");
        assert!(!m.move_holds(WindowId(1), true, moves[0].stage), "parked or shown as the move left it");
        assert!(!m.move_holds(WindowId(1), false, None), "in the stage the move left it in");
        assert!(m.move_holds(WindowId(1), false, moves[0].stage));
    }

    #[test]
    fn a_move_the_rounds_cap_would_cut_waits_for_the_next_round() {
        // R13 2: a move goes only with ~2 × 50 ms of the round left; the first always does.
        let ms = Duration::from_millis;
        assert!(move_fits(ms(0)) && move_fits(ms(10)));
        assert!(!move_fits(ms(11)) && !move_fits(ms(100)));
        let mut clock = ms(0);
        let mut sent = Vec::new();
        let left = send_round(&[0, 1, 2], |i| {
            if !move_fits(clock) {
                return false;
            }
            sent.push(i);
            clock += if i == 0 { ms(4) } else { ms(30) };
            true
        });
        assert_eq!((sent, left), (vec![0, 1], vec![2]), "34 ms in, 66 left: the third is not sent, not counted");
    }

    #[test]
    fn an_exact_move_the_cap_cut_is_sent_again_by_the_arrivals_check() {
        // R13 2: the nudge through, the exact move cut by the round's cap: owed, not final.
        let r = Rect::new(80.0, 78.0, 520.0, 392.0);
        let mut e = Expect::new(1, r, None, false);
        let cut = Err(AxError::CANNOT_COMPLETE);
        assert_eq!(record_resend(&mut e, Ok(()), cut, false), Resent::Cut);
        assert!(e.nudged_only && !e.gave_up && !e.final_sent, "owed, and not the final send");
        let nudged = win(1, 10, 0, false, Rect::new(81.0, 78.0, 520.0, 392.0));
        let seen = judge(&e, Some(&nudged), MAIN, parks);
        assert_eq!(seen, Seen::Missing, "1 pt off: the exact move owed");
        assert_eq!(tail_step(&[e], &[Some(seen)], false), TailStep::Send(vec![0]), "sent again next tick");
        assert_eq!(owed_exact(&[e], &[Some(seen)]), vec![0], "and once more on its full timeout when the time is up");
        assert_eq!(owed_exact(&[e], &[None]), Vec::<usize>::new(), "not a window gone");
        // On its full timeout, a failure is the app's: final, still owed for the time-up send.
        let mut busy = Expect::new(2, r, None, false);
        assert_eq!(record_resend(&mut busy, Ok(()), Err(AxError::CANNOT_COMPLETE), true), Resent::Full);
        assert!(busy.nudged_only);
        let mut ok = Expect { nudged_only: true, ..Expect::new(3, r, None, false) };
        assert_eq!(record_resend(&mut ok, Ok(()), Ok(()), false), Resent::Full);
        assert!(!ok.nudged_only, "through: nothing owed");
        let mut failed = Expect::new(4, r, None, true);
        assert_eq!(record_resend(&mut failed, Err(AxError::CANNOT_COMPLETE), Ok(()), true), Resent::Full);
        assert!(failed.gave_up && owed_exact(&[failed], &[Some(Seen::Missing)]).is_empty());
    }

    /// Drives the arrival's check from its time-up as the controller does: each tick's due
    /// moves from [`tail_step`], one [`tail_round`] with `send` as the round's resend (`k`:
    /// the move's place in the tick's round), the ceiling counted. Returns each tick's sends
    /// (ids) and the last tick's outcome.
    fn run_tail(moves: &mut [Expect], seen: &[Option<Seen>], mut send: impl FnMut(&mut Expect, usize) -> Resent) -> (Vec<Vec<u32>>, TailRound) {
        let mut ticks = Vec::new();
        for over_ticks in 1u32.. {
            let TailStep::Send(due) = tail_step(moves, seen, true) else { panic!("the time is up: no wait") };
            let ceiling = tail_past_ceiling(over_ticks, (RESEND_ROUND_CAP + VERIFY_EVERY) * (over_ticks - 1));
            let mut sent = Vec::new();
            let out = tail_round(moves, &due, seen, true, ceiling, |e| {
                let r = send(e, sent.len());
                if r != Resent::Deferred {
                    sent.push(e.id);
                }
                r
            });
            ticks.push(sent);
            if out.end != TailEnd::NextTick {
                return (ticks, out);
            }
        }
        unreachable!()
    }

    #[test]
    fn moves_the_rounds_cap_deferred_at_time_up_go_over_the_next_ticks() {
        // R15 1: three moves due at time-up, each ~100 ms: the cap lets one a round; the
        // rest go on the following ticks, not dropped as unsent.
        let r = Rect::new(80.0, 78.0, 520.0, 392.0);
        let mut moves: Vec<Expect> = (1..=3).map(|id| Expect::new(id, r, None, false)).collect();
        let seen = vec![Some(Seen::Missing); 3];
        let (ticks, out) = run_tail(&mut moves, &seen, |_, k| if k == 0 { Resent::Full } else { Resent::Deferred });
        assert_eq!(ticks, vec![vec![1], vec![2], vec![3]], "one a tick");
        assert_eq!(out.end, TailEnd::Done);
        assert!(moves.iter().all(|e| e.final_sent), "every move sent once more");
        // Before the time is up the same: the deferred ones go next tick.
        let mut fresh: Vec<Expect> = (1..=2).map(|id| Expect::new(id, r, None, false)).collect();
        let out = tail_round(&mut fresh, &[0, 1], &seen[..2], false, false, |e| if e.id == 1 { Resent::Full } else { Resent::Deferred });
        assert_eq!((out.left, out.end), (vec![1], TailEnd::NextTick));
    }

    #[test]
    fn the_owed_exact_moves_at_time_up_go_one_a_tick() {
        // R15 2: each owed exact move takes its full 50 ms: one a tick, after the round's
        // sends, never stacked on top of a round.
        let r = Rect::new(80.0, 78.0, 520.0, 392.0);
        let owed = |id| Expect { nudged_only: true, final_sent: true, tries: 1, ..Expect::new(id, r, None, false) };
        let mut moves = vec![owed(1), owed(2), Expect::new(3, r, None, false)];
        let seen = vec![Some(Seen::Missing); 3];
        // Move 3 is due this tick; the owed exacts of 1 and 2 wait for ticks of their own.
        // Each exact move fails on its full timeout: given once, never again.
        let (ticks, out) = run_tail(&mut moves, &seen, |e, _| {
            if e.nudged_only {
                assert!(!e.exact_last, "one exact move each");
            }
            Resent::Full
        });
        assert_eq!(ticks, vec![vec![3], vec![1], vec![2]], "the round, then one owed exact a tick");
        assert_eq!(out.end, TailEnd::Done);
        assert!(moves[..2].iter().all(|e| e.exact_last && e.nudged_only), "still owed (the app's), not sent again");
        let mut once = vec![owed(1), owed(2)];
        let out = tail_round(&mut once, &[], &seen[..2], true, false, |_| Resent::Full);
        assert_eq!((out.exact, out.end), (Some(0), TailEnd::NextTick));
        let out = tail_round(&mut once, &[], &seen[..2], true, false, |_| Resent::Full);
        assert_eq!((out.exact, out.end), (Some(1), TailEnd::Done));
        let out = tail_round(&mut once, &[], &seen[..2], false, false, |_| panic!("before the time-up: no exact pass"));
        assert_eq!(out.end, TailEnd::Done);
    }

    #[test]
    fn the_arrivals_check_past_its_ceiling_ends_with_what_stayed_unsent() {
        // R15 1: an app whose exact move the cap always cuts: the check goes on 10 ticks past
        // its time, then ends, the move unsent (logged at warn).
        assert!(!tail_past_ceiling(1, Duration::ZERO) && !tail_past_ceiling(11, Duration::from_millis(200)));
        assert!(tail_past_ceiling(12, Duration::ZERO) && tail_past_ceiling(2, TAIL_OVER_FOR));
        assert!(!tail_past_ceiling(1, Duration::from_secs(5)), "the time-up round always goes");
        assert!(
            !tail_past_ceiling(TAIL_OVER_TICKS + 1, (RESEND_ROUND_CAP + VERIFY_EVERY) * TAIL_OVER_TICKS),
            "a slow app's rounds (cap plus a tick each) get every counted tick"
        );
        let r = Rect::new(80.0, 78.0, 520.0, 392.0);
        let mut moves = vec![Expect::new(1, r, None, false), Expect { nudged_only: true, final_sent: true, ..Expect::new(2, r, None, false) }];
        let seen = vec![Some(Seen::Missing); 2];
        let (ticks, out) = run_tail(&mut moves, &seen, |e, _| if e.id == 1 { Resent::Cut } else { Resent::Full });
        assert_eq!(ticks.len(), 12, "the time-up tick, 10 more, and the one that ends it");
        assert!(ticks[..11].iter().all(|t| t == &vec![1]) && ticks[11].is_empty(), "nothing sent past the ceiling: {ticks:?}");
        assert_eq!(out.end, TailEnd::Ceiling(vec![0, 1]), "the cut move and the exact one never reached");
        assert!(!moves[1].exact_last);
        let mut done = vec![Expect { final_sent: true, ..Expect::new(1, r, None, false) }];
        assert_eq!(tail_round(&mut done, &[], &seen[..1], true, true, |_| panic!()).end, TailEnd::Done, "nothing left: no warn");
    }
}
