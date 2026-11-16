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
//!   Shift-click adds only the window clicked, a probe of 11-16), every window that shows a
//!   point of its own at one settled read clicked in one Shift press, front to back (the
//!   fourth probe of 11-16: a stage of two, both clicked 50 ms apart, both joined); a window
//!   fully under the ones clicked is clicked at the next settle. A window whose stage is not
//!   listed is brought in front by a focus first (the round trip: the stage in front goes to
//!   the strip and its windows are Shift-clicked back after the settle). The goal is met at
//!   the first read that shows every window in the stage in front, settled or not; a newly
//!   opened app's front window is focused then and there, and the job ends with that focus.
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
use super::mirror::{Arrangement, Where};
use super::plan::{Intent, Step};
use super::points;
use crate::engine::geometry::Rect;
use crate::engine::model::Display;

/// Runs of the same step for the same window before the job is given up.
pub const TRIES: u32 = 2;

/// How long a newly opened app may take to show its first window.
pub const FIRST_WINDOW: Duration = Duration::from_secs(10);

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
    /// Shift-clicked in from the strip) or `"round_trip"` (the stage pushed into the strip
    /// Shift-clicked back), `None` for the rest.
    pub route: Option<&'static str>,
    /// From the ask to the end, in ms.
    pub ms: u64,
    /// From the ask to the newly opened app's first window seen, in ms.
    pub first_window_ms: Option<u64>,
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
    /// A newly opened app to unhide after a give-up (the hide route took and its window
    /// never came in).
    pub unhide: Option<i32>,
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
    /// The newly opened app was hidden by us at its launch (the hide route took).
    pub hidden: bool,
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
            app_seen: BTreeSet::new(),
            focused_app: false,
            focus_pending: None,
            interrupted: None,
            refused: None,
            dragged_from: None,
            waits_for: "the settle",
            lenient: false,
            screen: None,
        }
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
            Goal::Gather { windows, app, focus_app } => self.gather(windows, *app, *focus_app, cx, now),
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
        self.gather(&t, None, false, cx, now)
    }

    /// [`Goal::Gather`]'s need.
    fn gather(&mut self, windows: &[u32], app: Option<i32>, focus_app: bool, cx: &Ctx, now: Instant) -> Need {
        let a = cx.mirror;
        // Windows gone, and windows of a Desktop not shown (a focus would show it), are left.
        let mut want: Vec<u32> = windows.iter().copied().filter(|w| a.where_is(*w).is_some_and(|(p, _)| p != Where::Away)).collect();
        let mut app_windows: Vec<u32> = Vec::new();
        if let Some(pid) = app {
            // Front to back: the stage in front's order, then the strip's.
            let order = a.current.iter().chain(a.strip.iter().flat_map(|g| g.windows.iter())).chain(a.remembered.iter().flat_map(|g| g.windows.iter()));
            app_windows = order.copied().filter(|w| cx.pids.get(w) == Some(&pid)).collect();
            for w in &app_windows {
                if self.app_seen.insert(*w) {
                    self.report.first_window_ms.get_or_insert(ms(now, self.since));
                }
            }
            if app_windows.is_empty() {
                if now.saturating_duration_since(self.since) > FIRST_WINDOW {
                    return Need::Fail(format!("app {pid} showed no window in {} s", FIRST_WINDOW.as_secs()));
                }
                return Need::Wait("the app's first window");
            }
            if self.report.route.is_none() {
                let app_in_front = app_windows.iter().any(|w| a.current.contains(w));
                let before_in_front = want.iter().all(|w| a.current.contains(w));
                self.report.route = Some(if app_in_front && !before_in_front { "round_trip" } else { "hide" });
            }
            for w in &app_windows {
                if !want.contains(w) {
                    want.push(*w);
                }
            }
        }
        let missing: BTreeSet<u32> = want.iter().copied().filter(|w| !a.current.contains(w)).collect();
        if missing.is_empty() {
            // Every window in front at this read: the app's front window focused at once (a
            // window of the stage in front: it switches nothing, no settle waited).
            if focus_app && !self.focused_app {
                let front = app_windows.iter().copied().find(|w| a.current.contains(w)).or_else(|| want.first().copied());
                if let Some(front) = front {
                    self.focus_pending = Some(front);
                    return Need::Step(Step::Focus(front));
                }
            }
            return Need::Met;
        }
        // Every window that shows a point of its own at this read, in one Shift press: front
        // to back within each item, the items top to bottom. Each point was taken with the
        // windows listed in front of it counted (the gather's own not clicked yet too): the
        // clicks before it in the press take those away, never onto it.
        let in_strip: Vec<u32> = a.strip.iter().flat_map(|g| g.windows.iter().copied()).filter(|w| missing.contains(w)).collect();
        let presses: Vec<Press> = in_strip.iter().filter_map(|w| a.click_at.get(w).map(|at| Press { window: *w, at: *at })).collect();
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

    /// `step` through the gate and the retry count.
    fn try_step(&mut self, mut step: Step, cx: &Ctx, now: Instant) -> Next {
        let gated = match &mut step {
            // A focus that switches stages waits for the settle; one in the stage in front (a
            // raise, the newly opened app's focus) goes at once.
            Step::Focus(w) if !cx.mirror.settled && !cx.mirror.current.contains(w) => Err(Hold::InMotion),
            Step::Focus(_) => gate::focus_gate(&cx.input, cx.in_flight),
            Step::Post(posted) => gate::gate(&cx.input, cx.mirror.settled, cx.in_flight).map(|go| {
                if let Posted::Clicks { own_shift, .. } = posted {
                    *own_shift = go.own_shift;
                }
            }),
            Step::AwaitSettle | Step::AwaitStrip(_) => Ok(()),
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
            let in_front = a.current.iter().any(|w| cx.pids.get(w) == Some(&pid));
            let waiting = a.strip.iter().chain(&a.remembered).flat_map(|g| g.windows.iter()).copied().find(|w| cx.pids.get(w) == Some(&pid));
            if !in_front {
                self.report.undo = waiting.map(Step::Focus);
                if self.hidden && waiting.is_none() {
                    self.report.unhide = Some(pid);
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
    }
}

/// A step's route name.
pub fn step_name(step: &Step) -> &'static str {
    match step {
        Step::Focus(_) => "focus",
        Step::Post(Posted::Clicks { .. }) => "shift_click",
        Step::Post(Posted::Drag { .. }) => "drag",
        Step::AwaitSettle | Step::AwaitStrip(_) => "wait",
    }
}

fn ms(now: Instant, since: Instant) -> u64 {
    now.saturating_duration_since(since).as_millis() as u64
}
