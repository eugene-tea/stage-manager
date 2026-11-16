//! What a step of ours does in the Apple mode: an [`Intent`] against the mirror gives the
//! goals a [`Job`] meets one step at a time ([`super::job`]), or a [`Refusal`] with its
//! reason. Apple's answers; after every step the mirror is read again and the job goes on from
//! what is there, whether the step did what was planned or not.
//!
//! - **Switch** (`ctl switch N`, the switch hotkeys): the focus route on the stage's key
//!   window. It needs only a known window of the stage, works with the strip hidden and for a
//!   stage no longer listed, and moves no pointer.
//! - **Merge** (`ctl merge N`): stage N's windows into the stage in front, a Shift-click on
//!   each one's thumbnail (a Shift-click adds only the window clicked).
//! - **Send** (`ctl send N`, the send hotkeys): a window to stage N: stage N brought in front,
//!   the window Shift-clicked in from the strip, the stage left brought back in front.
//! - **New** (`ctl new`, the new-stage hotkey): a window out to a stage of its own, a drag of
//!   its title bar onto the strip.
//!
//! **Apple's rule for an app's windows** ([`apart`]): with "Show windows from an application:
//! All at Once" (`AppWindowGroupingBehavior = 1`, the default) Apple's never splits one app's
//! windows into two stages: a window dragged out while another window of its app stays only
//! moves (the fifth probe of 11-16, every drop point tried). Send and New for such a window
//! are refused up front with that reason; with "One at a Time" they are tried (unprobed).
//! - **Open here** (a rule's `current`, `[stages] new_apps_join_current`, Shift held at the
//!   launch): a newly opened app's windows into the stage in front ([`open_here`]).
//! - **Restore** (`ctl apple restore`, and by itself after a restart): the saved stages put
//!   back ([`super::restore`]).
//!
//! Only the Desktop shown is acted on: a window of a Desktop not shown (an away group of the
//! mirror) is refused, since a focus of it would show its Desktop.

use std::collections::BTreeMap;

use serde::Serialize;

use super::desk::Posted;
use crate::engine::settings::AppWindows;
use super::job::{Goal, Job};
use super::mirror::{Arrangement, Where};

/// What was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Stage N in front (1-based: the strip's slots top to bottom, then the remembered ones).
    Switch(usize),
    /// Stage N's windows into the stage in front.
    Merge(usize),
    /// Window `1` to stage N `0`.
    Send(usize, u32),
    /// Window `0` out to a stage of its own.
    New(u32),
    /// Newly opened app `0`'s windows into the stage in front.
    OpenHere(i32),
    /// The saved stages put back ([`super::restore`]).
    Restore,
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Window focused (the focus route).
    Focus(u32),
    /// A gesture posted (the Shift-click and drag routes).
    Post(Posted),
    /// Wait until the strip shows item `slot`.
    AwaitStrip(usize),
    /// Wait until the arrangement is settled.
    AwaitSettle,
}

/// Why an intent is not carried out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Refusal(pub String);

/// Stage `n` of `a`, or the refusal naming how many there are.
fn stage(a: &Arrangement, n: usize) -> Result<&super::mirror::Group, Refusal> {
    a.by_number(n).ok_or_else(|| {
        let count = a.strip.len() + a.remembered.len();
        Refusal(format!("no stage {n}: Apple's shows {count} besides the one in front"))
    })
}

/// The window of `windows` to focus for its stage: `key`'s (the model's key window) when it
/// is one of them, else the front one.
fn key_of(windows: &[u32], key: impl Fn(&[u32]) -> Option<u32>) -> Option<u32> {
    key(windows).filter(|w| windows.contains(w)).or_else(|| windows.first().copied())
}

/// The plan for switching to stage `n` of arrangement `a`: focus its key window (`key`: the
/// one the model remembers of the group's windows, else its front one), once the arrangement
/// is settled. Stage `n` in front already, or none: refused.
pub fn switch(a: &Arrangement, n: usize, key: impl Fn(&[u32]) -> Option<u32>) -> Result<Vec<Step>, Refusal> {
    let group = stage(a, n)?;
    let Some(window) = key_of(&group.windows, key) else {
        return Err(Refusal(format!("stage {n} has no window")));
    };
    let mut steps = Vec::new();
    if !a.settled {
        steps.push(Step::AwaitSettle);
    }
    steps.push(Step::Focus(window));
    Ok(steps)
}

/// The switch to stage `n` as a job: its key window in front.
pub fn switch_job(a: &Arrangement, n: usize, key: impl Fn(&[u32]) -> Option<u32>, now: std::time::Instant) -> Result<Job, Refusal> {
    let steps = switch(a, n, key)?;
    let Some(Step::Focus(window)) = steps.last().cloned() else { return Err(Refusal("no focus planned".into())) };
    Ok(Job::new(Intent::Switch(n), vec![Goal::Front(window)], now))
}

/// Stage `n`'s windows into the stage in front: a Shift-click on each, front to back. A stage
/// not listed (past the strip's room) is brought in front first and the stage left
/// Shift-clicked back.
pub fn merge(a: &Arrangement, n: usize, now: std::time::Instant) -> Result<Job, Refusal> {
    let group = stage(a, n)?;
    if a.current.is_empty() {
        return Err(Refusal("no stage in front to merge into: switch to it instead".into()));
    }
    let mut windows = a.current.clone();
    windows.extend(group.windows.iter().copied());
    Ok(Job::new(Intent::Merge(n), vec![Goal::Gather { windows, app: None, focus_app: false }], now))
}

/// Whether window `w` may leave its stage for another (send, new) under Apple's grouping
/// `app_windows`, `pids` giving each window's app: refused under All at Once while another
/// window of its app is in its stage (module docs).
pub fn apart(a: &Arrangement, w: u32, pids: &BTreeMap<u32, i32>, app_windows: AppWindows) -> Result<(), Refusal> {
    if app_windows != AppWindows::AllAtOnce {
        return Ok(());
    }
    let Some(pid) = pids.get(&w) else { return Ok(()) };
    let stage: &[u32] = match a.where_is(w) {
        None => return Ok(()),
        Some((Where::Current, _)) => &a.current,
        Some(_) => a.strip.iter().chain(&a.remembered).find(|g| g.windows.contains(&w)).map_or(&[][..], |g| &g.windows),
    };
    if stage.iter().any(|x| *x != w && pids.get(x) == Some(pid)) {
        return Err(Refusal(format!(
            "window {w}'s app has another window in its stage: with \"Show windows from an application: All at Once\" Apple's Stage Manager keeps an app's windows in one stage"
        )));
    }
    Ok(())
}

/// Window `w` to stage `n`: stage `n` brought in front (its key window, `key`), `w`
/// Shift-clicked in from the strip, then the stage `w` left brought back in front (when it
/// keeps a window).
pub fn send(a: &Arrangement, n: usize, w: u32, key: impl Fn(&[u32]) -> Option<u32>, now: std::time::Instant) -> Result<Job, Refusal> {
    let group = stage(a, n)?;
    if group.windows.contains(&w) {
        return Err(Refusal(format!("window {w} is in stage {n} already")));
    }
    let Some((place, _)) = a.where_is(w) else {
        return Err(Refusal(format!("window {w} is in no stage Apple's shows or showed")));
    };
    if place == Where::Away {
        return Err(away(w));
    }
    let Some(target) = key_of(&group.windows, key) else {
        return Err(Refusal(format!("stage {n} has no window")));
    };
    let mut windows = group.windows.clone();
    windows.push(w);
    let mut goals = vec![Goal::Front(target), Goal::Gather { windows, app: None, focus_app: false }];
    // Back to where the person was: the stage in front when it keeps a window besides `w`.
    let back = match place {
        Where::Current => a.current.iter().copied().find(|x| *x != w),
        Where::Strip | Where::Remembered | Where::Away => a.current.first().copied(),
    };
    if let Some(back) = back {
        goals.push(Goal::Front(back));
    }
    Ok(Job::new(Intent::Send(n, w), goals, now))
}

/// Window `w` out to a stage of its own: brought in front when it is not, then its title bar
/// dragged onto the strip. A window alone in its stage already: refused.
pub fn new_stage(a: &Arrangement, w: u32, now: std::time::Instant) -> Result<Job, Refusal> {
    let alone = match a.where_is(w) {
        None => return Err(Refusal(format!("window {w} is in no stage Apple's shows or showed"))),
        Some((Where::Away, _)) => return Err(away(w)),
        Some((Where::Current, _)) => a.current.len() <= 1,
        Some(_) => a.strip.iter().chain(&a.remembered).any(|g| g.windows == [w]),
    };
    if alone {
        return Err(Refusal(format!("window {w} is alone in its stage already")));
    }
    Ok(Job::new(Intent::New(w), vec![Goal::Out(w)], now))
}

/// The refusal for window `w` on a Desktop not shown.
pub fn away(w: u32) -> Refusal {
    Refusal(format!("window {w} is on a Desktop not shown: nothing is done there"))
}

/// Newly opened app `pid`'s windows into the stage in front of `a` (asked at its launch,
/// before its first window): the stage in front's windows kept there and every window of the
/// app gathered in as it shows, the app's front window focused at the end. `None` with no
/// stage in front (Apple's own then puts the app in front, as asked).
pub fn open_here(a: &Arrangement, pid: i32, now: std::time::Instant) -> Option<Job> {
    if a.current.is_empty() {
        return None;
    }
    Some(Job::new(Intent::OpenHere(pid), vec![Goal::Gather { windows: a.current.clone(), app: Some(pid), focus_app: true }], now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::apple::mirror::Group;
    use crate::engine::geometry::Rect;

    fn arrangement(settled: bool) -> Arrangement {
        let g = |w: &[u32]| Group { windows: w.to_vec(), frame: Rect::new(15.0, 100.0, 79.0, 104.0) };
        Arrangement { current: vec![4], strip: vec![g(&[3]), g(&[1, 2])], remembered: vec![g(&[7])], settled, ..Arrangement::default() }
    }

    #[test]
    fn a_switch_focuses_the_stages_key_window_counting_the_strip_then_the_remembered() {
        let a = arrangement(true);
        assert_eq!(switch(&a, 1, |_| None), Ok(vec![Step::Focus(3)]));
        assert_eq!(switch(&a, 2, |_| Some(2)), Ok(vec![Step::Focus(2)]), "the model's key window");
        assert_eq!(switch(&a, 2, |_| Some(9)), Ok(vec![Step::Focus(1)]), "a key of another group: the front one");
        assert_eq!(switch(&a, 3, |_| None), Ok(vec![Step::Focus(7)]), "a remembered stage: focus needs only a known window");
        assert!(switch(&a, 4, |_| None).is_err());
        assert!(switch(&a, 0, |_| None).is_err());
    }

    #[test]
    fn a_switch_mid_motion_waits_for_the_settle() {
        assert_eq!(switch(&arrangement(false), 1, |_| None), Ok(vec![Step::AwaitSettle, Step::Focus(3)]));
    }
}
