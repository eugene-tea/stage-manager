//! A step of ours carried out over several reads ([`Job`]): an [`Intent`] planned into goals
//! ([`super::plan`]), each goal met by the routes one step at a time against the mirror as
//! it is at that read, never as predicted. "Observations in, steps out": [`Job::advance`]
//! takes the mirror, the person's input and the screen and answers with the next step to run,
//! a wait, the end, or a give-up; the platform runs the step and says how it went
//! ([`Job::ran`], [`Job::landed`]); the mirror then shows what Apple's did, and the next
//! advance goes on from there, whether the step did what was planned or not.
//!
//! The goals:
//!
//! - [`Goal::Front`]: a window in the stage in front, by the focus route.
//! - [`Goal::Gather`]: windows (and, for a newly opened app, every window of the app as it
//!   shows) in the stage in front, by a Shift-click per window on its thumbnail (a
//!   Shift-click adds only the window clicked, a probe of 11-16). **Only an item's front
//!   window takes a click** (the eighth probe, 11-17: a window behind another of its item,
//!   of one app or two, took no click alone nor after the front's in one press; the front
//!   gone in, the one behind took it at a fresh read): a press takes one window per item, its
//!   front, the fronts of several items in one press; the windows behind come in later
//!   presses, each at a fresh read once Apple's add motion has settled (the mirror gives them
//!   no point till then). A window Apple draws no thumbnail of (an item shows three) comes the
//!   same way. Each click lands its window on top, so the stage in front ends in another order
//!   (an item's back window on top of its front): **the order is put back** once every window
//!   is in, by the focus route (raise, main, app frontmost: the eighth probe, a focus of a
//!   window in the stage in front puts it on top, the stage whole; AXRaise alone, and with
//!   AXMain, did nothing) on the windows out of place, back to front, the one wanted on top
//!   last ([`Job::reorder`]). The order wanted: for a merge or a send, the windows brought in
//!   on top in their order over the stage in front at the ask; for "open here", the app's
//!   windows (in their order in the strip, front first) over the stage it was asked into; for
//!   a restore, the saved order. A focus whose window has left the stage in front stops it,
//!   said; a raise Apple's does not answer after [`TRIES`] is left, said.
//!   **The stage that was in front left** (a newly opened app in front: Apple's switched to
//!   its stage) is brought back whole by one focus of its front window, never Shift-clicked
//!   back window by window: Apple's own switch puts back its windows in their order, the ones
//!   it draws no thumbnail of too, and the app's stage goes to the strip, whose windows are
//!   then Shift-clicked in (the round trip). A window whose stage is not listed is brought in
//!   front by a focus first. The goal is met at the first read that shows every window in
//!   the stage in front, settled or not; a newly opened app's front window is focused then
//!   and there, and the job ends with that focus.
//!   **An app hidden by us** at its launch or activation ([`Job::hidden`]) is taken in ahead
//!   of the settle ([`Job::pickup`]): by its pid, never by a window's id (a running app
//!   reopens the window it ordered out, an id seen before, read as `AXDialog` while hidden,
//!   live 11-16), from every window of it the list shows ([`super::mirror::Arrangement::seen`]).
//!   Nothing of it shows: [`UNHIDE_AFTER`] after the hide took (a launch: as soon as a window
//!   of it is known, if sooner) it is shown again without activating it ([`Step::Unhide`]), and
//!   Apple's puts its window into the strip (some apps make none while hidden: Reminders, live
//!   11-16). Its thumbnails, each with a twin and at rest for [`STILL_FOR`] reads, are
//!   Shift-clicked in, in one press; its windows in front at rest are focused; the rest of the
//!   arrangement may still move (the stage in front does not). No thumbnail [`REOPEN_THUMB`]
//!   after the unhide (a running app) or [`FIRST_WINDOW`] after the ask (a launch): given up,
//!   the app activated, as the person asked. The stage in front left meanwhile (a window shown
//!   before the hide took, or the app in front by itself): the round trip, after the settle,
//!   whose switch back leaves the app's windows in the strip, taken in the same way, ahead of
//!   the settle. A hide called off as too late that took all the same ([`Job::hid_late`]) is
//!   this route: the app is shown again at once and its thumbnail taken in.
//! - [`Goal::Out`]: a window out to a stage of its own, by a drag of its title bar onto the
//!   strip (it works with the strip covered, a probe of 11-16), grabbed where the title bar
//!   shows; one covered all along it is raised first (live 11-16: the fixture windows open at
//!   one frame, the one to drag under another, the drag's guard refused it twice).
//!
//! - [`Goal::Stage`]: a saved stage put back (the restore, [`super::restore`]): a stage of
//!   exactly its windows still listed, in front at the end when it is the saved active one.
//!   One of its windows brought in front by focus (the one whose stage holds the fewest
//!   windows of the other saved stages), each window of another saved stage in front taken out
//!   by the drag, then the rest gathered by one Shift press. A window of another saved stage
//!   whose app keeps another window in front stays (under All at Once Apple's would only move
//!   it): said in the report. A stage that is so already anywhere is left as it is; windows
//!   gone, and windows on a Desktop not shown, are skipped and said.
//!
//! No goal acts on a window of a Desktop not shown (a focus would show its Desktop), and no
//! step runs while a full-screen app's Space shows on the screen.
//!
//! A *lenient* job (the restore) skips a goal it cannot meet, says why in its report, and
//! goes on with the next; the deadline and the person's own input still end it.
//!
//! Whether a goal is met is asked of every read out of motion (the groups are read afresh);
//! a posted step, and a focus that switches stages, wait for the settle; every posted step
//! goes through the gate ([`super::gate`]). A step that ran and whose goal is still not met at
//! the next settle runs once more ([`TRIES`]), then the job is given up and reported. The
//! person's own input ending a drag gives it up at once.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::desk::{InputNow, Landed, Posted, Press};
use super::gate::{self, Hold, Waiting};
use super::mirror::{Arrangement, Seen, Where};
use super::plan::{Intent, Step};
use super::points;
use crate::engine::geometry::Rect;
use crate::engine::model::Display;

/// Runs of the same step for the same window before the job is given up.
pub const TRIES: u32 = 2;

/// How long a newly opened app may take to show its first window.
pub const FIRST_WINDOW: Duration = Duration::from_secs(10);

/// How long a running app activated with no window may take to reopen one (the probe's came
/// about 50 ms after the activation): a ⌘Tab to an app that opens none is not held up longer.
pub const REOPEN_WINDOW: Duration = Duration::from_secs(3);

/// How long after the hide took an app hidden by us is shown again (without activating it):
/// time for it to order its window in (live 11-16: Activity Monitor's came 24 ms after the
/// hide took).
pub const UNHIDE_AFTER: Duration = Duration::from_millis(275);

/// How long after that unhide a running app's thumbnail may take to show in the strip (the
/// probe: about 0.5 s): none by then, the app is activated.
pub const REOPEN_THUMB: Duration = Duration::from_millis(1500);

/// Reads in a row an app window keeps its bounds before it counts at rest (out of Apple's
/// motion): two fast reads.
pub const STILL_FOR: u32 = 2;

/// After a Shift press on an app's thumbnails, how long before they are pressed again (Apple's
/// add motion starts about 100 ms after the click and runs about 600 ms).
pub const PRESS_AGAIN: Duration = Duration::from_millis(700);

/// What a job is to bring about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Goal {
    /// Window `0` in the stage in front (the focus route).
    Front(u32),
    /// `windows`, and with `app` every window of that app, in the stage in front (a
    /// Shift-click each); with `focus_app`, the app's front window focused once all are in.
    Gather {
        /// The windows to have in front.
        windows: Vec<u32>,
        /// A newly opened app whose windows join as they show.
        app: Option<i32>,
        /// The app's front window focused at the end (it has the keyboard).
        focus_app: bool,
    },
    /// Window `0` out to a stage of its own (the drag).
    Out(u32),
    /// A saved stage put back (module docs).
    Stage {
        /// Its windows, front to back (the key window first).
        windows: Vec<u32>,
        /// The windows of the other saved stages: taken out when in front with it.
        others: Vec<u32>,
        /// The saved active stage: in front at the end.
        front: bool,
        /// "Show windows from an application: All at Once": a window whose app keeps another
        /// window in front is never dragged out.
        all_at_once: bool,
    },
}

/// What the platform sees, for one advance.
#[derive(Debug, Clone, Copy)]
pub struct Ctx<'a> {
    /// The mirror at this read.
    pub mirror: &'a Arrangement,
    /// The person's input now.
    pub input: InputNow,
    /// The screen the strip is on.
    pub screen: &'a Display,
    /// Each managed window's app.
    pub pids: &'a BTreeMap<u32, i32>,
    /// A gesture of ours is in flight.
    pub in_flight: bool,
}

/// What to do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    /// Run this step now (then [`Job::ran`]).
    Run(Step),
    /// Nothing now: what it waits for.
    Wait(&'static str),
    /// Every goal met.
    Done(Report),
    /// Given up: the report says why; `undo`, when given, is run to leave the windows as
    /// Apple's own would have them (a newly opened app hidden for the hide route, in the
    /// strip: its stage brought in front).
    GiveUp(Report),
}

/// A job's account, for the log and `ctl state`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Report {
    /// What was asked.
    pub intent: Option<Intent>,
    /// The way "open here" went: `"hide"` (the app hidden at its launch, its window
    /// Shift-clicked in from the strip), `"hide_activation"` (a running app hidden at its
    /// activation, shown again a moment later, its thumbnails Shift-clicked in
    /// from the strip) or `"round_trip"` (the stage pushed into the strip Shift-clicked back),
    /// `None` for the rest.
    pub route: Option<&'static str>,
    /// From the ask to the end, in ms.
    pub ms: u64,
    /// From the ask to the newly opened app's first window seen, in ms.
    pub first_window_ms: Option<u64>,
    /// Why the round trip ran, said plainly in the log.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round_trip_why: Option<&'static str>,
    /// From the ask to the app shown again (the hide at its activation), in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unhide_ms: Option<u64>,
    /// Focuses run.
    pub focuses: u32,
    /// Shift-clicks posted.
    pub clicks: u32,
    /// Shift presses posted (each one or more Shift-clicks).
    pub presses: u32,
    /// Drags posted.
    pub drags: u32,
    /// Why it was given up.
    pub given_up: Option<String>,
    /// Run after a give-up to leave Apple's own arrangement.
    pub undo: Option<Step>,
    /// The app to unhide after a give-up of "open here": always (a hide asked may take after
    /// the job knew of it).
    pub unhide: Option<i32>,
    /// An app to activate after a give-up: hidden at its launch or activation, nothing of it
    /// came in (the activation the person asked for, given back).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activate: Option<i32>,
    /// A window to set back to its frame after a give-up: a drag that moved it without
    /// taking it out.
    pub set_back: Option<(u32, Rect)>,
    /// Saved stages met (the restore).
    pub stages: u32,
    /// Windows skipped, gone from the list (the restore).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gone: Vec<u32>,
    /// What could not be done and why, each said once (the restore).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub could_not: Vec<String>,
}

/// The step last run, for the retry count.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ran {
    /// Its kind and window.
    key: (u8, u32),
    /// Runs in a row.
    tries: u32,
}

/// What a goal needs now.
enum Need {
    Met,
    Step(Step),
    Hold(Hold),
    Wait(&'static str),
    Fail(String),
}

/// A step of ours under way.
#[derive(Debug, Clone)]
pub struct Job {
    /// What was asked.
    pub intent: Intent,
    /// When it was asked.
    pub since: Instant,
    goals: Vec<Goal>,
    deadline: Duration,
    last: Option<Ran>,
    waiting: Waiting,
    report: Report,
    /// The newly opened app was hidden by us at its launch or activation (the hide took).
    pub hidden: bool,
    /// When the hide took.
    hidden_at: Option<Instant>,
    /// A running app with no window, activated: hidden at its activation, shown again once
    /// its reopened window exists ([`super::plan::reopen_here`]).
    pub reopen: bool,
    /// The app was shown again ([`Step::Unhide`] ran, or the platform did at a late hide).
    unhidden: bool,
    /// A hide called off as too late took all the same ([`Job::hid_late`]).
    pub late: bool,
    /// The stage that was in front was brought back by a focus (the round trip): the app's
    /// windows it left in the strip are taken in ahead of the settle ([`Job::pickup`]).
    back: bool,
    /// The step asked is the round trip's switch back: at rest is enough for it
    /// ([`Arrangement::at_rest`]), the settle not waited.
    switch_back: bool,
    /// When the unhide was asked.
    unhide_at: Option<Instant>,
    /// When the app's thumbnails were last pressed ([`Job::pickup`]).
    pressed_at: Option<Instant>,
    /// The app's windows seen so far.
    app_seen: BTreeSet<u32>,
    focused_app: bool,
    /// The app's front window, asked to be focused at the end.
    focus_pending: Option<u32>,
    interrupted: Option<String>,
    /// Why the gesture last posted was not (its guard), for the give-up's words.
    refused: Option<String>,
    /// The window dragged and its frame before the first drag.
    dragged_from: Option<(u32, Rect)>,
    /// The step last asked for and what it waits on, for `ctl state`.
    pub waits_for: &'static str,
    /// A goal that cannot be met is skipped and said, the job going on (the restore).
    pub lenient: bool,
    /// The screen the job acts on (`None`: the main one).
    pub screen: Option<crate::engine::model::DisplayId>,
    /// The order wanted in front ([`Job::reorder`]): the goal's windows it was taken for, the
    /// windows wanted on top (front first), and the ones kept under them.
    order_for: Vec<u32>,
    order_top: Vec<u32>,
    order_under: Vec<u32>,
    /// A focus putting the order back ran: a window leaving the stage in front then stops the
    /// job, said.
    reordered: bool,
    /// The order is left as it is (a raise not answered), said.
    order_left: bool,
}

impl Job {
    /// A job for `intent` with `goals`, asked at `now`.
    pub fn new(intent: Intent, goals: Vec<Goal>, now: Instant) -> Self {
        let windows: usize = goals
            .iter()
            .map(|g| match g {
                Goal::Gather { windows, .. } | Goal::Stage { windows, .. } => windows.len(),
                _ => 1,
            })
            .sum();
        let deadline = match intent {
            Intent::Switch(_) => Duration::from_millis(3000),
            Intent::OpenHere(_) => FIRST_WINDOW + Duration::from_secs(8),
            Intent::Restore => Duration::from_secs(6) * goals.len() as u32 + Duration::from_millis(1500) * windows as u32 + gate::STRIP_WAIT,
            _ => Duration::from_secs(6) + Duration::from_millis(1500) * windows as u32 + gate::STRIP_WAIT,
        };
        Self {
            intent,
            since: now,
            goals,
            deadline,
            last: None,
            waiting: Waiting::default(),
            report: Report { intent: Some(intent), ..Report::default() },
            hidden: false,
            hidden_at: None,
            reopen: false,
            unhidden: false,
            late: false,
            back: false,
            switch_back: false,
            unhide_at: None,
            pressed_at: None,
            app_seen: BTreeSet::new(),
            focused_app: false,
            focus_pending: None,
            interrupted: None,
            refused: None,
            dragged_from: None,
            waits_for: "the settle",
            lenient: false,
            screen: None,
            order_for: Vec::new(),
            order_top: Vec::new(),
            order_under: Vec::new(),
            reordered: false,
            order_left: false,
        }
    }

    /// The hide at the app's launch or activation took at `now` (the platform's hide thread
    /// said so).
    pub fn hide_took(&mut self, now: Instant) {
        if !self.hidden {
            self.hidden = true;
            self.hidden_at = Some(now);
        }
    }

    /// A hide called off as too late (a window of the app showed first) took all the same at
    /// `now`, and the platform showed the app again at once (live 11-17: Activity Monitor hid
    /// 35 ms after its hide was called off, and Apple's put its window into the strip): the
    /// hide route from here, its thumbnail taken in ahead of the settle, as an app shown
    /// again by [`Step::Unhide`].
    pub fn hid_late(&mut self, now: Instant) {
        if self.hidden {
            return;
        }
        self.hidden = true;
        self.late = true;
        self.hidden_at = Some(now);
        self.unhidden = true;
        self.unhide_at = Some(now);
        self.report.unhide_ms.get_or_insert(ms(now, self.since));
    }

    /// When the job next wants an advance whatever the reads: the unhide's moment
    /// ([`UNHIDE_AFTER`] after the hide took), while it is to come.
    pub fn due(&self) -> Option<Instant> {
        if self.hidden && !self.unhidden && matches!(self.intent, Intent::OpenHere(_)) { self.hidden_at.map(|t| t + UNHIDE_AFTER) } else { None }
    }

    /// `why` said in the report's `could_not`, once.
    pub fn note(&mut self, why: String) {
        if !self.report.could_not.contains(&why) {
            self.report.could_not.push(why);
        }
    }

    /// The goals left.
    pub fn goals(&self) -> &[Goal] {
        &self.goals
    }

    /// The account so far.
    pub fn report(&self) -> &Report {
        &self.report
    }

    /// The next step against what the platform sees now (`cx`) at `now`.
    pub fn advance(&mut self, cx: &Ctx, now: Instant) -> Next {
        if let Some(why) = self.interrupted.take() {
            return self.give_up(cx, now, why);
        }
        // A last step that met the goals as it ran (the newly opened app's focus): done now,
        // no settle waited.
        if self.goals.is_empty() {
            self.report.ms = ms(now, self.since);
            return Next::Done(self.report.clone());
        }
        if now.saturating_duration_since(self.since) > self.deadline {
            let why = format!("not done in {} s", self.deadline.as_secs());
            return self.give_up(cx, now, why);
        }
        // An app hidden by us: taken in by its pid, ahead of the settle.
        if let Some(next) = self.pickup(cx, now) {
            return next;
        }
        // The settle and a gesture in flight are the mirror's own waits: they never count
        // toward the person's (the job's deadline bounds them).
        if cx.in_flight {
            return self.wait(Hold::InFlight.why());
        }
        // In motion the groups read before stand: nothing to tell from them. Out of motion
        // they are read afresh, settled or not: a goal is met at the first such read.
        if cx.mirror.in_motion {
            return self.wait(Hold::InMotion.why());
        }
        if cx.mirror.full_screen {
            if self.waiting.hold(Hold::FullScreen, now) {
                let why = format!("{} for {} ms", Hold::FullScreen.why(), self.waiting.waited(now).as_millis());
                return self.give_up(cx, now, why);
            }
            return self.wait(Hold::FullScreen.why());
        }
        while let Some(goal) = self.goals.first().cloned() {
            match self.need(&goal, cx, now) {
                Need::Met => {
                    if matches!(goal, Goal::Stage { .. }) {
                        self.report.stages += 1;
                    }
                    self.goals.remove(0);
                    self.last = None;
                    self.waiting.clear();
                }
                Need::Wait(why) => return self.wait(why),
                Need::Fail(why) => match self.skip(cx, why) {
                    Ok(()) => continue,
                    Err(why) => return self.give_up(cx, now, why),
                },
                Need::Hold(hold) => {
                    if self.waiting.hold(hold, now) {
                        let why = format!("{} for {} ms", hold.why(), self.waiting.waited(now).as_millis());
                        match self.skip(cx, why) {
                            Ok(()) => continue,
                            Err(why) => return self.give_up(cx, now, why),
                        }
                    }
                    return self.wait(hold.why());
                }
                Need::Step(step) => return self.try_step(step, cx, now),
            }
        }
        self.report.ms = ms(now, self.since);
        Next::Done(self.report.clone())
    }

    /// Step `step` ran (`ok`: its calls went; a posted one's end comes with
    /// [`Job::landed`]).
    pub fn ran(&mut self, step: &Step, ok: bool) {
        if !ok {
            return;
        }
        match step {
            Step::Focus(w) => {
                self.report.focuses += 1;
                // The stage that was in front brought back (the round trip's switch back).
                if let Some(Goal::Gather { windows, app: Some(_), .. }) = self.goals.first()
                    && windows.contains(w)
                {
                    self.back = true;
                }
                if self.focus_pending == Some(*w) {
                    self.focused_app = true;
                    // Every window was in front when the focus was asked: the gather is met,
                    // and the job ends at the next advance.
                    if matches!(self.goals.first(), Some(Goal::Gather { focus_app: true, .. })) {
                        self.goals.remove(0);
                        self.last = None;
                    }
                }
            }
            Step::Post(Posted::Clicks { presses, .. }) => {
                self.report.clicks += presses.len() as u32;
                self.report.presses += 1;
            }
            Step::Post(Posted::Drag { .. }) => self.report.drags += 1,
            Step::Unhide(_) => self.unhidden = true,
            Step::AwaitSettle | Step::AwaitStrip(_) => {}
        }
    }

    /// How the gesture last posted ended: the person's own input ending it gives the job up
    /// at the next advance; a refusal counts as a run (tried once more at the next settle).
    pub fn landed(&mut self, landed: &Landed) {
        match landed {
            Landed::Interrupted(why) => self.interrupted = Some(format!("the person's own input ended the gesture: {why}")),
            Landed::Refused(why) => self.refused = Some(why.clone()),
            Landed::Posted => {}
        }
    }

    fn wait(&mut self, why: &'static str) -> Next {
        self.waits_for = why;
        Next::Wait(why)
    }

    /// A lenient job's goal that cannot be met (`why`) skipped and said: `Ok` to go on with
    /// the next; any other job's `Err(why)` to give up.
    fn skip(&mut self, cx: &Ctx, why: String) -> Result<(), String> {
        if !self.lenient || self.goals.is_empty() {
            return Err(why);
        }
        // A drag Apple's took as a plain move: the window set back (the platform does it at
        // the end).
        if let Some((w, frame)) = self.dragged_from.take()
            && cx.mirror.current.contains(&w)
            && cx.mirror.frames.get(&w).is_some_and(|f| *f != frame)
        {
            self.report.set_back.get_or_insert((w, frame));
        }
        let goal = self.goals.remove(0);
        let what = match &goal {
            Goal::Stage { windows, .. } => format!("the stage of windows {windows:?}"),
            other => format!("{other:?}"),
        };
        self.note(format!("{what}: {why}"));
        self.last = None;
        self.waiting.clear();
        self.refused = None;
        Ok(())
    }

    /// What goal `goal` needs now.
    fn need(&mut self, goal: &Goal, cx: &Ctx, now: Instant) -> Need {
        let a = cx.mirror;
        match goal {
            Goal::Front(w) => match a.where_is(*w) {
                None => Need::Fail(format!("window {w} is in no stage Apple's shows or showed")),
                Some((Where::Away, _)) => Need::Fail(super::plan::away(*w).0),
                Some((Where::Current, _)) => Need::Met,
                Some(_) => Need::Step(Step::Focus(*w)),
            },
            Goal::Out(w) => match a.where_is(*w) {
                None => Need::Fail(format!("window {w} is in no stage Apple's shows or showed")),
                Some((Where::Away, _)) => Need::Fail(super::plan::away(*w).0),
                Some((Where::Current, _)) if a.current.len() <= 1 => Need::Met,
                Some((Where::Current, _)) => self.out_of_front(*w, cx),
                Some(_) => {
                    let alone = a.strip.iter().chain(&a.remembered).any(|g| g.windows == [*w]);
                    if alone { Need::Met } else { Need::Step(Step::Focus(*w)) }
                }
            },
            Goal::Gather { windows, app, focus_app } => self.gather(windows, *app, *focus_app, None, cx, now),
            Goal::Stage { windows, others, front, all_at_once } => self.stage(windows, others, *front, *all_at_once, cx, now),
        }
    }

    /// Window `w`, in front with others, dragged out to a stage of its own: grabbed where its
    /// title bar shows, raised first when it is covered all along.
    fn out_of_front(&mut self, w: u32, cx: &Ctx) -> Need {
        let a = cx.mirror;
        let Some(frame) = a.frames.get(&w).copied() else { return Need::Fail(format!("window {w} is not listed")) };
        let Some(from) = a.title_at.get(&w).copied() else {
            // Covered all along its title bar: raised first (in the stage in front a focus
            // raises it and switches nothing), dragged once the list shows it on top.
            return Need::Step(Step::Focus(w));
        };
        let items: Vec<_> = a.strip.iter().map(|g| g.frame).collect();
        let to = points::strip_drop(cx.screen, &items);
        // The frame before the first drag: a drag Apple's took as a plain move leaves the
        // window moved (live 11-16), set back when the job is given up.
        if self.dragged_from.is_none_or(|(d, _)| d != w) {
            self.dragged_from = Some((w, a.full.get(&w).copied().unwrap_or(frame)));
        }
        let restore = self.dragged_from.map_or(frame, |(_, f)| f);
        Need::Step(Step::Post(Posted::Drag { window: w, from, to, frame: restore }))
    }

    /// [`Goal::Stage`]'s need (module docs).
    fn stage(&mut self, windows: &[u32], others: &[u32], front: bool, all_at_once: bool, cx: &Ctx, now: Instant) -> Need {
        let a = cx.mirror;
        let mut t: Vec<u32> = Vec::new();
        for w in windows {
            match a.where_is(*w) {
                None => {
                    if !self.report.gone.contains(w) {
                        self.report.gone.push(*w);
                    }
                }
                Some((Where::Away, _)) => self.note(format!("window {w} is on a Desktop not shown: left there")),
                Some(_) => t.push(*w),
            }
        }
        if t.is_empty() {
            return Need::Met;
        }
        let set: BTreeSet<u32> = t.iter().copied().collect();
        let same = |g: &[u32]| g.len() == set.len() && g.iter().all(|w| set.contains(w));
        if same(&a.current) || (!front && a.strip.iter().chain(&a.remembered).any(|g| same(&g.windows))) {
            return Need::Met;
        }
        let others: BTreeSet<u32> = others.iter().copied().filter(|w| !set.contains(w)).collect();
        if !a.current.iter().any(|w| set.contains(w)) {
            // In front by a focus: the window whose stage holds the fewest windows of the other
            // saved stages (of equals, the first: the key window).
            let cost = |w: u32| a.groups().into_iter().find(|(_, g)| g.contains(&w)).map_or(usize::MAX, |(_, g)| g.iter().filter(|x| others.contains(x)).count());
            return match t.iter().copied().min_by_key(|w| cost(*w)) {
                Some(seed) => Need::Step(Step::Focus(seed)),
                None => Need::Met,
            };
        }
        // Windows of the other saved stages in front: out by the drag, one at a time; under
        // All at Once one whose app keeps another window in front stays (Apple's would only
        // move it).
        let extras: Vec<u32> = a.current.iter().copied().filter(|w| others.contains(w)).collect();
        let pid = |w: &u32| cx.pids.get(w).copied();
        let movable = |e: u32| !all_at_once || pid(&e).is_none() || !a.current.iter().any(|x| *x != e && pid(x) == pid(&e));
        if let Some(e) = extras.iter().copied().find(|e| movable(*e)) {
            return self.out_of_front(e, cx);
        }
        if !extras.is_empty() {
            self.note(format!(
                "windows {extras:?} stay with the stage of windows {t:?}: with \"Show windows from an application: All at Once\" Apple's Stage Manager keeps an app's windows together"
            ));
        }
        self.gather(&t, None, false, Some(&t), cx, now)
    }

    /// The order wanted in front for the gather of `windows` ([`Job::reorder`], module docs)
    /// taken at its first read: for a newly opened app (`app`) the windows asked are the ones
    /// kept under its windows; else the ones in front then are kept under the rest.
    fn seed_order(&mut self, windows: &[u32], app: Option<i32>, a: &Arrangement) {
        if self.order_for == windows {
            return;
        }
        self.order_for = windows.to_vec();
        self.reordered = false;
        self.order_left = false;
        self.order_under = match app {
            Some(_) => windows.to_vec(),
            None => a.current.iter().copied().filter(|w| windows.contains(w)).collect(),
        };
        self.order_top = match app {
            Some(_) => Vec::new(),
            None => windows.iter().copied().filter(|w| !self.order_under.contains(w)).collect(),
        };
    }

    /// The app's windows `ids` (front to back as the strip shows them), noted on top of the
    /// order wanted after those noted before.
    fn note_app_order(&mut self, ids: impl IntoIterator<Item = u32>) {
        for id in ids {
            if !self.order_top.contains(&id) {
                self.order_top.push(id);
            }
        }
    }

    /// The order wanted in front, front to back: `order` when given (a restore's saved
    /// order); else the windows wanted on top, then any other window of `app` in front, then
    /// the ones kept under them.
    fn wanted(&self, app: Option<i32>, order: Option<&[u32]>, a: &Arrangement) -> Vec<u32> {
        if let Some(o) = order {
            return o.to_vec();
        }
        let mut w = self.order_top.clone();
        if let Some(pid) = app {
            w.extend(a.seen.iter().filter(|(id, s)| s.full && s.pid == pid && !self.order_top.contains(id)).map(|(id, _)| *id));
        }
        w.extend(self.order_under.iter().copied().filter(|id| !self.order_top.contains(id)));
        w
    }

    /// The next window to focus to put the stage in front in `wanted`'s order (module docs):
    /// of the windows wanted that show in front at full size, those over the longest tail of
    /// `wanted` already in its order there, the lowest first, so the one wanted on top goes
    /// last; `None` once in order, or when the order is left (a raise Apple's did not answer
    /// after [`TRIES`], said).
    fn reorder(&mut self, wanted: &[u32], a: &Arrangement) -> Option<u32> {
        if self.order_left {
            return None;
        }
        let shown: Vec<u32> = a.seen.iter().filter(|(id, s)| s.full && wanted.contains(id)).map(|(id, _)| *id).collect();
        let want: Vec<u32> = wanted.iter().copied().filter(|w| shown.contains(w)).collect();
        let in_order = |tail: &[u32]| {
            let mut it = shown.iter();
            tail.iter().all(|w| it.any(|x| x == w))
        };
        let k = (0..=want.len()).find(|&k| in_order(&want[k..])).unwrap_or(want.len());
        let next = *want.get(k.checked_sub(1)?)?;
        if self.last.is_some_and(|r| r.key == (0, next) && r.tries >= TRIES) {
            self.order_left = true;
            self.note(format!("the order in front not put back: window {next} was not raised in {TRIES} focuses"));
            return None;
        }
        self.reordered = true;
        Some(next)
    }

    /// [`Goal::Gather`]'s need, after the settle (an app hidden by us is taken in ahead of it:
    /// [`Job::pickup`]); `order`, when given, the order wanted in front at the end.
    fn gather(&mut self, windows: &[u32], app: Option<i32>, focus_app: bool, order: Option<&[u32]>, cx: &Ctx, now: Instant) -> Need {
        let a = cx.mirror;
        self.seed_order(windows, app, a);
        // Windows gone, and windows of a Desktop not shown (a focus would show it), are left.
        let mut want: Vec<u32> = windows.iter().copied().filter(|w| a.where_is(*w).is_some_and(|(p, _)| p != Where::Away)).collect();
        // The windows to keep, front to back (the stage in front at the ask, for "open here").
        let base = want.clone();
        let mut app_windows: Vec<u32> = Vec::new();
        // The app's windows the window list shows by its pid, front to back.
        let mut pid_order: Vec<u32> = Vec::new();
        // The app's windows the app does not manage, by its pid, whatever their ids or
        // subroles (a reused id, never tracked anew: Finder, live 11-16): at full size in front,
        // or a thumbnail with its point.
        let mut by_pid: BTreeMap<u32, Seen> = BTreeMap::new();
        if let Some(pid) = app {
            // Front to back: the stage in front's order, then the strip's.
            let order = a.current.iter().chain(a.strip.iter().flat_map(|g| g.windows.iter())).chain(a.remembered.iter().flat_map(|g| g.windows.iter()));
            app_windows = order.copied().filter(|w| cx.pids.get(w) == Some(&pid)).collect();
            for (w, seen) in a.seen_of(pid).filter(|(w, s)| (s.full || s.thumb) && !cx.pids.contains_key(w)) {
                by_pid.insert(w, seen);
                app_windows.push(w);
                pid_order.push(w);
            }
            for w in &app_windows {
                if self.app_seen.insert(*w) {
                    self.report.first_window_ms.get_or_insert(ms(now, self.since));
                }
            }
            if app_windows.is_empty() {
                let first = if self.reopen { REOPEN_WINDOW } else { FIRST_WINDOW };
                if now.saturating_duration_since(self.since) > first {
                    return Need::Fail(format!("app {pid} showed no window in {} s", first.as_secs()));
                }
                return Need::Wait("the app's first window");
            }
            if self.report.route.is_none() {
                let app_in_front = app_windows.iter().any(|w| a.current.contains(w) || by_pid.get(w).is_some_and(|s| s.full));
                let before_in_front = want.iter().all(|w| a.current.contains(w));
                self.report.route = Some(match (app_in_front && !before_in_front, self.reopen) {
                    (true, _) => "round_trip",
                    (false, true) => "hide_activation",
                    (false, false) => "hide",
                });
                if app_in_front && !before_in_front && !self.hidden {
                    self.report.round_trip_why.get_or_insert("a window of the app showed before it was hidden: the round trip");
                }
            }
            for w in &app_windows {
                if !want.contains(w) {
                    want.push(*w);
                }
            }
        }
        let in_front = |w: &u32| a.current.contains(w) || by_pid.get(w).is_some_and(|s| s.full);
        let missing: BTreeSet<u32> = want.iter().copied().filter(|w| !in_front(w)).collect();
        if self.reordered && !missing.is_empty() {
            // A focus putting the order back took a window out of the stage in front: stopped.
            return Need::Fail(format!("windows {missing:?} left the stage in front as its order was put back"));
        }
        if missing.is_empty() {
            // The order put back first, the window wanted on top last (for "open here" the
            // app's front window: its focus is the one asked).
            let wanted = self.wanted(app, order, a);
            if let Some(w) = self.reorder(&wanted, a) {
                if focus_app && !self.focused_app && wanted.first() == Some(&w) {
                    self.focus_pending = Some(w);
                }
                return Need::Step(Step::Focus(w));
            }
            // Every window in front at this read: the app's front window focused at once (a
            // window of the stage in front: it switches nothing, no settle waited).
            if focus_app && !self.focused_app {
                let front = app_windows.iter().copied().find(in_front).or_else(|| want.first().copied());
                if let Some(front) = front {
                    self.focus_pending = Some(front);
                    return Need::Step(Step::Focus(front));
                }
            }
            return Need::Met;
        }
        // The stage that was in front left (Apple's switched to the app's stage): brought back
        // whole by a focus of its front window. Apple's own switch puts back its windows in
        // their order, the ones it draws no thumbnail of too, and sends the app's stage to the
        // strip; the app's windows are then Shift-clicked in (live 11-17: a stage of four
        // Shift-clicked back took three presses, 7.3 s, and came back reversed).
        if let Some(front) = base.first().copied()
            && !base.iter().any(|w| a.current.contains(w))
        {
            self.switch_back = app.is_some();
            return Need::Step(Step::Focus(front));
        }
        // The app's windows in the strip, front first: on top of the order wanted.
        if let Some(pid) = app {
            let strip: Vec<u32> = a.strip.iter().flat_map(|g| g.windows.iter().copied()).filter(|w| cx.pids.get(w) == Some(&pid)).collect();
            self.note_app_order(strip.into_iter().chain(pid_order.iter().copied().filter(|w| by_pid.get(w).is_some_and(|s| s.thumb))));
        }
        // Every window that shows a point of its own at this read, in one Shift press: each
        // item's front only (module docs), the items top to bottom; the windows behind at a
        // later read. Each point was taken with the windows listed in front of it counted.
        let in_strip: Vec<u32> = a
            .strip
            .iter()
            .flat_map(|g| g.windows.iter().rev().copied())
            .chain(pid_order.iter().rev().copied().filter(|w| by_pid.get(w).is_some_and(|s| s.thumb)))
            .filter(|w| missing.contains(w))
            .collect();
        let at = |w: &u32| a.click_at.get(w).copied().or_else(|| by_pid.get(w).and_then(|s| s.click_at));
        let presses: Vec<Press> = in_strip.iter().filter_map(|w| at(w).map(|at| Press { window: *w, at })).collect();
        if !presses.is_empty() {
            return Need::Step(Step::Post(Posted::Clicks { presses, own_shift: true }));
        }
        if !in_strip.is_empty() {
            return Need::Hold(if a.strip_shown == Some(true) { Hold::NoPoint } else { Hold::StripCovered });
        }
        // None of them listed (a stage past the strip's room): its stage brought in front by a
        // focus, the stage left Shift-clicked back after the settle.
        match missing.iter().copied().next() {
            Some(w) => Need::Step(Step::Focus(w)),
            None => Need::Met,
        }
    }

    /// An app hidden by us at its launch or activation, or one the round trip's switch back
    /// left in the strip, taken in ahead of the settle (module docs): `None` hands the job to
    /// the goals after the settle (neither, a full-screen app's Space, or the stage in front
    /// left: the round trip).
    fn pickup(&mut self, cx: &Ctx, now: Instant) -> Option<Next> {
        let Intent::OpenHere(pid) = self.intent else { return None };
        let Some(Goal::Gather { windows, app: Some(_), focus_app }) = self.goals.first().cloned() else { return None };
        let a = cx.mirror;
        if !(self.hidden || self.back) || a.full_screen {
            return None;
        }
        let mine: Vec<(u32, Seen)> = a.seen_of(pid).collect();
        for (w, _) in &mine {
            if self.app_seen.insert(*w) {
                self.report.first_window_ms.get_or_insert(ms(now, self.since));
            }
        }
        // The stage in front left (a window of the app shown before the hide took): the round
        // trip, after the settle.
        // Read as a thumbnail, or in a strip item at the mirror's last grouping.
        let left = windows.iter().any(|w| {
            !a.current.contains(w) && (a.seen.iter().any(|(id, s)| id == w && s.thumb) || a.strip.iter().chain(&a.remembered).any(|g| g.windows.contains(w)))
        });
        if left {
            // Hidden all the same (Apple switched at the activation itself, before the hide
            // took: live 11-17, Contacts running with no window, the stage in front to the
            // strip and an empty one in front, the job then waiting 3 s for a window of an app
            // still hidden): shown again when due, so its window comes for the round trip.
            if self.hidden && !self.unhidden {
                if cx.in_flight {
                    return Some(self.wait(Hold::InFlight.why()));
                }
                if self.hidden_at.is_some_and(|t| now >= t + UNHIDE_AFTER) {
                    self.unhide_at = Some(now);
                    self.report.unhide_ms.get_or_insert(ms(now, self.since));
                    self.report.round_trip_why.get_or_insert("Apple switched at the activation, before the hide took: shown again, the round trip");
                    return Some(self.try_step_ahead(Step::Unhide(pid), cx, now));
                }
                return Some(self.wait("the app to order its window in"));
            }
            if self.unhidden {
                // Some apps activate themselves when shown again (1Password, live 11-16).
                self.report.round_trip_why.get_or_insert("the app came in front by itself after its unhide: the round trip");
            }
            return None;
        }
        if cx.in_flight {
            return Some(self.wait(Hold::InFlight.why()));
        }
        let thumbs: Vec<(u32, Seen)> = mine.iter().filter(|(_, s)| s.thumb).copied().collect();
        let front: Vec<(u32, Seen)> = mine.iter().filter(|(_, s)| s.full).copied().collect();
        self.seed_order(&windows, Some(pid), a);
        // The app's thumbnails, front first: on top of the order wanted.
        self.note_app_order(thumbs.iter().map(|(w, _)| *w));
        if self.hidden && !self.unhidden && thumbs.is_empty() {
            if !front.is_empty() {
                return None;
            }
            // Nothing of it shows: shown again once it has had time to order its window in (a
            // launch: as soon as a window of it is known).
            let due = self.hidden_at.is_some_and(|t| now >= t + UNHIDE_AFTER);
            if due || (!self.reopen && cx.pids.values().any(|p| *p == pid)) {
                if !windows.iter().all(|w| a.seen.iter().any(|(id, s)| id == w && s.full)) {
                    self.report.round_trip_why.get_or_insert("Apple switched at the activation, before the hide took: shown again, the round trip");
                }
                self.unhide_at = Some(now);
                self.report.unhide_ms.get_or_insert(ms(now, self.since));
                return Some(self.try_step_ahead(Step::Unhide(pid), cx, now));
            }
            return Some(self.wait("the app to order its window in"));
        }
        if !thumbs.is_empty() {
            if self.pressed_at.is_some_and(|t| now.saturating_duration_since(t) < PRESS_AGAIN) {
                return Some(self.wait("Apple's add motion"));
            }
            if thumbs.iter().any(|(_, s)| s.still < STILL_FOR) {
                return Some(self.wait("the app's thumbnail to rest"));
            }
            // Each item's front of the app's thumbnails in one Shift press (the windows behind
            // have no point till then: module docs), the rest at a later read.
            let presses: Vec<Press> = thumbs.iter().filter_map(|(w, s)| s.click_at.map(|at| Press { window: *w, at })).collect();
            if presses.is_empty() {
                let hold = if thumbs.iter().any(|(_, s)| s.bounds.max_x() <= cx.screen.frame.x) { Hold::StripCovered } else { Hold::NoPoint };
                if self.waiting.hold(hold, now) {
                    let why = format!("{} for {} ms", hold.why(), self.waiting.waited(now).as_millis());
                    return Some(self.give_up(cx, now, why));
                }
                return Some(self.wait(hold.why()));
            }
            self.report.route.get_or_insert(if self.reopen { "hide_activation" } else { "hide" });
            let next = self.try_step_ahead(Step::Post(Posted::Clicks { presses, own_shift: true }), cx, now);
            if matches!(next, Next::Run(_)) {
                self.pressed_at = Some(now);
            }
            return Some(next);
        }
        // A window of the app on its way between the strip and the stage in front (a window
        // behind its item's front, laid out again once the front went in): waited for, so no
        // window of the app is left in the strip.
        if !front.is_empty() && mine.iter().any(|(_, s)| !s.full) {
            return Some(self.wait("the app's thumbnail to rest"));
        }
        if !front.is_empty() {
            // Not with the stage it was asked into (Apple's switched at the activation itself,
            // the stage in front leaving at once, live 11-17): the round trip after the settle.
            if !windows.iter().all(|w| a.seen.iter().any(|(id, s)| id == w && s.full)) {
                self.report.round_trip_why.get_or_insert("the app came in front by itself after its unhide: the round trip");
                return None;
            }
            // In the stage in front: focused once at rest (a window there switches nothing).
            if front.iter().any(|(_, s)| s.still < STILL_FOR) {
                return Some(self.wait("the app's window into the stage in front"));
            }
            // The order put back, the app's front window focused last (module docs).
            let wanted = self.wanted(Some(pid), None, a);
            if let Some(w) = self.reorder(&wanted, a) {
                if focus_app && !self.focused_app && wanted.first() == Some(&w) {
                    self.focus_pending = Some(w);
                }
                return Some(self.try_step_ahead(Step::Focus(w), cx, now));
            }
            if focus_app && !self.focused_app {
                let w = front[0].0;
                self.focus_pending = Some(w);
                return Some(self.try_step_ahead(Step::Focus(w), cx, now));
            }
            self.goals.remove(0);
            self.last = None;
            self.waiting.clear();
            if self.goals.is_empty() {
                self.report.ms = ms(now, self.since);
                return Some(Next::Done(self.report.clone()));
            }
            return None;
        }
        // Nothing of it shows after the switch back: the goals after the settle say.
        if !self.hidden {
            return None;
        }
        // Shown again, nothing of it shows yet.
        let (limit, words) = match self.reopen {
            true => (self.unhide_at.map(|t| t + REOPEN_THUMB), format!("app {pid} showed no thumbnail in {} ms of its unhide: activated", REOPEN_THUMB.as_millis())),
            false => (Some(self.since + FIRST_WINDOW), format!("app {pid} showed no window in {} s: activated", FIRST_WINDOW.as_secs())),
        };
        // Once a window of it showed (the round trip's switch back sends it to the strip
        // anew), the job's deadline alone bounds the wait.
        if self.app_seen.is_empty() && limit.is_some_and(|l| now > l) {
            return Some(self.give_up(cx, now, words));
        }
        Some(self.wait("the app's thumbnail"))
    }

    /// `step` through the gate and the retry count.
    fn try_step(&mut self, mut step: Step, cx: &Ctx, now: Instant) -> Next {
        // The round trip's switch back goes once at rest: a focus aims at nothing on screen, so
        // the settle's wait after the last frame of Apple's motion is not its own.
        let rest_enough = std::mem::take(&mut self.switch_back) && cx.mirror.at_rest();
        let gated = match &mut step {
            // A focus that switches stages waits for the settle; one in the stage in front (a
            // raise, the newly opened app's focus) goes at once.
            Step::Focus(w) if !cx.mirror.settled && !rest_enough && !cx.mirror.current.contains(w) => Err(Hold::InMotion),
            Step::Focus(_) => gate::focus_gate(&cx.input, cx.in_flight),
            Step::Post(posted) => gate::gate(&cx.input, cx.mirror.settled, cx.in_flight).map(|go| {
                if let Posted::Clicks { own_shift, .. } = posted {
                    *own_shift = go.own_shift;
                }
            }),
            // Nothing posted, no pointer: the app shown again at once.
            Step::Unhide(_) | Step::AwaitSettle | Step::AwaitStrip(_) => Ok(()),
        };
        if let Err(hold) = gated {
            if self.waiting.hold(hold, now) {
                let why = format!("{} for {} ms", hold.why(), self.waiting.waited(now).as_millis());
                return match self.skip(cx, why) {
                    Ok(()) => self.wait("the next goal"),
                    Err(why) => self.give_up(cx, now, why),
                };
            }
            return self.wait(hold.why());
        }
        self.counted(step, cx, now)
    }

    /// `step`, gated already, through the retry count.
    fn counted(&mut self, step: Step, cx: &Ctx, now: Instant) -> Next {
        self.waiting.clear();
        let key = step_key(&step);
        let tries = match self.last {
            Some(r) if r.key == key => r.tries + 1,
            _ => 1,
        };
        if tries > TRIES {
            let why = match self.refused.take() {
                Some(r) => format!("{} for window {}: not posted in {TRIES} tries ({r})", step_name(&step), key.1),
                None => format!("{} for window {}: Apple's did not answer after {TRIES} tries", step_name(&step), key.1),
            };
            return match self.skip(cx, why) {
                Ok(()) => self.wait("the next goal"),
                Err(why) => self.give_up(cx, now, why),
            };
        }
        self.refused = None;
        self.last = Some(Ran { key, tries });
        self.waits_for = "the step's answer";
        Next::Run(step)
    }

    /// `step` through the person's side of the gate and the retry count, with no settle waited
    /// ([`Job::pickup`]: the app's own windows at rest are enough).
    fn try_step_ahead(&mut self, mut step: Step, cx: &Ctx, now: Instant) -> Next {
        let gated = match &mut step {
            Step::Focus(_) => gate::focus_gate(&cx.input, cx.in_flight),
            Step::Post(posted) => gate::gate(&cx.input, true, cx.in_flight).map(|go| {
                if let Posted::Clicks { own_shift, .. } = posted {
                    *own_shift = go.own_shift;
                }
            }),
            Step::Unhide(_) | Step::AwaitSettle | Step::AwaitStrip(_) => Ok(()),
        };
        if let Err(hold) = gated {
            if self.waiting.hold(hold, now) {
                let why = format!("{} for {} ms", hold.why(), self.waiting.waited(now).as_millis());
                return self.give_up(cx, now, why);
            }
            return self.wait(hold.why());
        }
        self.counted(step, cx, now)
    }

    /// The job given up at `now` for `why`: for "open here", what leaves Apple's own
    /// arrangement (the app's stage brought in front, or the app unhidden).
    fn give_up(&mut self, cx: &Ctx, now: Instant, why: String) -> Next {
        self.report.ms = ms(now, self.since);
        self.report.given_up = Some(why);
        if let Some((w, frame)) = self.dragged_from
            && cx.mirror.current.contains(&w)
            && cx.mirror.frames.get(&w).is_some_and(|f| *f != frame)
        {
            self.report.set_back = Some((w, frame));
        }
        if let Intent::OpenHere(pid) = self.intent {
            let a = cx.mirror;
            let in_front = a.current.iter().any(|w| cx.pids.get(w) == Some(&pid)) || a.seen_of(pid).any(|(_, s)| s.full && s.still >= STILL_FOR);
            let waiting = a.strip.iter().chain(&a.remembered).flat_map(|g| g.windows.iter()).copied().find(|w| cx.pids.get(w) == Some(&pid));
            // Shown again whatever happened (a hide asked may take after the job knew of it:
            // Finder, live 11-16, left hidden with its stage empty).
            self.report.unhide = Some(pid);
            if !in_front {
                self.report.undo = waiting.map(Step::Focus);
                if waiting.is_none() {
                    // The activation the person asked for, given back.
                    self.report.activate = Some(pid);
                }
            }
        }
        self.goals.clear();
        Next::GiveUp(self.report.clone())
    }
}

/// A step's kind and window, for the retry count.
fn step_key(step: &Step) -> (u8, u32) {
    match step {
        Step::Focus(w) => (0, *w),
        Step::Post(p @ Posted::Clicks { .. }) => (1, p.window()),
        Step::Post(p @ Posted::Drag { .. }) => (2, p.window()),
        Step::AwaitSettle => (3, 0),
        Step::AwaitStrip(slot) => (4, *slot as u32),
        Step::Unhide(pid) => (5, *pid as u32),
    }
}

/// A step's route name.
pub fn step_name(step: &Step) -> &'static str {
    match step {
        Step::Focus(_) => "focus",
        Step::Post(Posted::Clicks { .. }) => "shift_click",
        Step::Post(Posted::Drag { .. }) => "drag",
        Step::Unhide(_) => "unhide",
        Step::AwaitSettle | Step::AwaitStrip(_) => "wait",
    }
}

fn ms(now: Instant, since: Instant) -> u64 {
    now.saturating_duration_since(since).as_millis() as u64
}
