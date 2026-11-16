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
    /// The step last asked for and what it waits on, for `ctl state`.
    pub waits_for: &'static str,
}

impl Job {
    /// A job for `intent` with `goals`, asked at `now`.
    pub fn new(intent: Intent, goals: Vec<Goal>, now: Instant) -> Self {
        let windows: usize = goals.iter().map(|g| if let Goal::Gather { windows, .. } = g { windows.len() } else { 1 }).sum();
        let deadline = match intent {
            Intent::Switch(_) => Duration::from_millis(3000),
            Intent::OpenHere(_) => FIRST_WINDOW + Duration::from_secs(8),
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
            waits_for: "the settle",
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
        while let Some(goal) = self.goals.first().cloned() {
            match self.need(&goal, cx, now) {
                Need::Met => {
                    self.goals.remove(0);
                    self.last = None;
                }
                Need::Wait(why) => return self.wait(why),
                Need::Fail(why) => return self.give_up(cx, now, why),
                Need::Hold(hold) => {
                    if self.waiting.hold(hold, now) {
                        let why = format!("{} for {} ms", hold.why(), self.waiting.waited(now).as_millis());
                        return self.give_up(cx, now, why);
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

    /// What goal `goal` needs now.
    fn need(&mut self, goal: &Goal, cx: &Ctx, now: Instant) -> Need {
        let a = cx.mirror;
        match goal {
            Goal::Front(w) => match a.where_is(*w) {
                None => Need::Fail(format!("window {w} is in no stage Apple's shows or showed")),
                Some((Where::Current, _)) => Need::Met,
                Some(_) => Need::Step(Step::Focus(*w)),
            },
            Goal::Out(w) => match a.where_is(*w) {
                None => Need::Fail(format!("window {w} is in no stage Apple's shows or showed")),
                Some((Where::Current, _)) if a.current.len() <= 1 => Need::Met,
                Some((Where::Current, _)) => {
                    let Some(frame) = a.frames.get(w).copied() else { return Need::Fail(format!("window {w} is not listed")) };
                    let Some(from) = a.title_at.get(w).copied() else {
                        // Covered all along its title bar: raised first (in the stage in front a
                        // focus raises it and switches nothing), dragged once the list shows it
                        // on top.
                        return Need::Step(Step::Focus(*w));
                    };
                    let to = points::strip_drop(cx.screen, from);
                    let restore = a.full.get(w).copied().unwrap_or(frame);
                    Need::Step(Step::Post(Posted::Drag { window: *w, from, to, frame: restore }))
                }
                Some(_) => {
                    let alone = a.strip.iter().chain(&a.remembered).any(|g| g.windows == [*w]);
                    if alone { Need::Met } else { Need::Step(Step::Focus(*w)) }
                }
            },
            Goal::Gather { windows, app, focus_app } => self.gather(windows, *app, *focus_app, cx, now),
        }
    }

    /// [`Goal::Gather`]'s need.
    fn gather(&mut self, windows: &[u32], app: Option<i32>, focus_app: bool, cx: &Ctx, now: Instant) -> Need {
        let a = cx.mirror;
        let mut want: Vec<u32> = windows.iter().copied().filter(|w| a.where_is(*w).is_some()).collect();
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
                return self.give_up(cx, now, why);
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
            return self.give_up(cx, now, why);
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
