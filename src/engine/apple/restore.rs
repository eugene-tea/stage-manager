//! Restore in the Apple mode: a saved arrangement (`state-apple.json`, the model's stages as
//! they followed Apple's) put back on Apple's Stage Manager as a job ([`plan`]), and when it
//! runs by itself after a restart ([`AutoRestore`]).
//!
//! **What is wanted** ([`wanted`]): the saved Desktop's stages with the live windows matched
//! into them by the restore's own matcher ([`crate::engine::persist::rematch_on`]: the same
//! window id within a window server session, else the app, the title's fingerprint and the
//! frame), confident matches only (a doubtful one is left where Apple's has it). A window in
//! Apple's strip says nothing by its frame (it is listed at its thumbnail's bounds). Windows
//! saved but not shown are counted and left out; windows shown but not saved are left where
//! they are.
//!
//! **Apple's rule for an app's windows** ([`keep_apps_together`]): under "Show windows from an
//! application: All at Once" Apple's never splits one app's windows into two stages (the
//! fifth probe of 11-16), so a saved arrangement that did (saved under "One at a Time") is
//! asked with the app's windows in the saved stage holding most of them, and the report says
//! so. No split is ever asked that Apple's refuses.
//!
//! **The job** ([`plan`]): for each saved stage a [`Goal::Stage`] (`engine/apple/job.rs`):
//! one of its windows in front by focus, the windows of other saved stages in front with it
//! out by the drag, its other windows gathered by one Shift press (their thumbnails showing:
//! a covered strip is waited for). The saved active stage comes last, so it ends in front.
//! A stage that is so already is left as it is: a restore of an arrangement Apple's still
//! shows moves nothing. The job is lenient: a stage that cannot be put back is skipped and
//! said, and the rest go on. It runs through the job queue and the gate like every step of
//! the mode; windows gone meanwhile are skipped.
//!
//! **By itself after a restart** ([`AutoRestore`], `[stages] restore_with_apple`, on by
//! default: functionality Apple's never built, it changes nothing Apple's does until its
//! stages are put back as they were): when the mode starts in a new window server session
//! (the saved file's session is another), it waits until the restore window's windows have
//! shown, then runs once: every saved window matched and no new one for [`ALL_SHOWN_QUIET`],
//! or the login settled ([`Settling`]: apps quiet for 20 s and a minute after the login),
//! or the restore window's five minutes up.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use super::job::{Goal, Job};
use super::mirror::Arrangement;
use super::plan::{Intent, Refusal};
use crate::engine::model::WindowMeta;
use crate::engine::persist::{SavedRole, SavedSpace, StateFile, rematch_on};
use crate::engine::restore::{End, Settling};

/// Every saved window shown and no new one for this long: the restore after a restart runs.
pub const ALL_SHOWN_QUIET: Duration = Duration::from_secs(3);

/// One saved stage as wanted now: the live windows matched into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Its windows, front to back, the saved key window first.
    pub windows: Vec<u32>,
    /// Its saved name, if any.
    pub name: Option<String>,
}

/// What a saved arrangement wants of the windows shown now ([`wanted`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wanted {
    /// The saved stages with a window matched, in saved order.
    pub stages: Vec<Target>,
    /// Which of them was the saved active one.
    pub front: Option<usize>,
    /// Standard windows saved.
    pub saved: usize,
    /// Of them, matched with confidence to a window shown.
    pub matched: usize,
    /// What will not be asked, and why ([`keep_apps_together`]).
    pub could_not: Vec<String>,
}

impl Wanted {
    /// Every window wanted somewhere.
    pub fn windows(&self) -> BTreeSet<u32> {
        self.stages.iter().flat_map(|t| t.windows.iter().copied()).collect()
    }
}

/// The saved Desktop to put back: the one flagged shown when saved, else the first with a
/// stage.
pub fn space(file: &StateFile) -> Option<&SavedSpace> {
    file.spaces.iter().find(|s| s.current && !s.stages.is_empty()).or_else(|| file.spaces.iter().find(|s| !s.stages.is_empty()))
}

/// What saved Desktop `space` wants of the standard windows shown now (`live`; `in_strip`: a
/// window Apple's shows in its strip or not at all, whose frame says nothing). See the module
/// docs.
pub fn wanted(space: &SavedSpace, live: &[WindowMeta], in_strip: impl Fn(&WindowMeta) -> bool) -> Wanted {
    let assignment = rematch_on(space, live, in_strip);
    let saved = space.standard_windows();
    let mut out = Wanted { saved, ..Wanted::default() };
    for (i, st) in assignment.stages.iter().enumerate() {
        let mut ws: Vec<_> = st.windows.iter().filter(|w| w.grade.confident()).collect();
        ws.sort_by_key(|w| (!w.key, w.z));
        if ws.is_empty() {
            continue;
        }
        out.matched += ws.len();
        if assignment.active_index == Some(i) {
            out.front = Some(out.stages.len());
        }
        out.stages.push(Target { windows: ws.iter().map(|w| w.id.0).collect(), name: st.name.clone() });
    }
    out
}

/// Under All at Once (`all_at_once`), each app's windows (`pids`) in one saved stage: an app
/// whose windows `w` puts in several goes whole to the stage holding most of them (of equals,
/// the saved active one, else the first), and `w.could_not` says so. Stages left empty go.
pub fn keep_apps_together(w: &mut Wanted, pids: &BTreeMap<u32, i32>, all_at_once: bool) {
    if !all_at_once {
        return;
    }
    let mut apps: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    for (i, t) in w.stages.iter().enumerate() {
        for id in &t.windows {
            if let Some(pid) = pids.get(id) {
                let stages = apps.entry(*pid).or_default();
                if !stages.contains(&i) {
                    stages.push(i);
                }
            }
        }
    }
    for (pid, stages) in apps.into_iter().filter(|(_, s)| s.len() > 1) {
        let count = |i: usize| w.stages[i].windows.iter().filter(|id| pids.get(id) == Some(&pid)).count();
        let best = stages.iter().copied().max_by_key(|i| (count(*i), Some(*i) == w.front, std::cmp::Reverse(*i))).unwrap_or(stages[0]);
        let mut moved = Vec::new();
        for i in stages.into_iter().filter(|i| *i != best) {
            let (go, stay): (Vec<u32>, Vec<u32>) = w.stages[i].windows.iter().partition(|id| pids.get(id) == Some(&pid));
            w.stages[i].windows = stay;
            moved.extend(go);
        }
        w.could_not.push(format!(
            "windows {moved:?} saved in another stage than their app's other windows: with \"Show windows from an application: All at Once\" Apple's Stage Manager keeps an app's windows in one stage, so they go with them"
        ));
        w.stages[best].windows.extend(moved);
    }
    // Stages left empty go, the saved active one's index kept pointing at it.
    let front = w.front;
    let mut keep = Vec::new();
    let mut new_front = None;
    for (i, t) in std::mem::take(&mut w.stages).into_iter().enumerate() {
        if t.windows.is_empty() {
            continue;
        }
        if front == Some(i) {
            new_front = Some(keep.len());
        }
        keep.push(t);
    }
    w.stages = keep;
    w.front = new_front;
}

/// The restore of `w` as a job: one [`Goal::Stage`] per saved stage, the saved active one
/// last, each met against the mirror as it is at each read (module docs). Nothing wanted:
/// refused.
pub fn plan(w: &Wanted, all_at_once: bool, now: Instant) -> Result<Job, Refusal> {
    if w.stages.is_empty() {
        return Err(Refusal(format!("no saved window is shown ({} saved)", w.saved)));
    }
    let all = w.windows();
    let mut order: Vec<usize> = (0..w.stages.len()).filter(|i| Some(*i) != w.front).collect();
    order.extend(w.front);
    let goals = order
        .into_iter()
        .map(|i| {
            let windows = w.stages[i].windows.clone();
            let others = all.iter().copied().filter(|id| !windows.contains(id)).collect();
            Goal::Stage { windows, others, front: Some(i) == w.front, all_at_once }
        })
        .collect();
    let mut job = Job::new(Intent::Restore, goals, now);
    job.lenient = true;
    for why in &w.could_not {
        job.note(why.clone());
    }
    Ok(job)
}

/// True when arrangement `a` already shows every stage `w` wants (each a stage of exactly its
/// windows still listed, the saved active one in front): a restore would move nothing.
pub fn in_place(a: &Arrangement, w: &Wanted) -> bool {
    w.stages.iter().enumerate().all(|(i, t)| {
        let set: BTreeSet<u32> = t.windows.iter().copied().filter(|id| a.where_is(*id).is_some()).collect();
        let same = |g: &[u32]| g.len() == set.len() && g.iter().all(|id| set.contains(id));
        set.is_empty() || same(&a.current) || (Some(i) != w.front && a.strip.iter().chain(&a.remembered).any(|g| same(&g.windows)))
    })
}

/// How many standard windows `file` saved in its Desktop to restore.
pub fn saved_windows(file: &StateFile) -> usize {
    space(file).map_or(0, |s| s.stages.iter().flat_map(|st| st.windows.iter()).filter(|w| w.role == SavedRole::Standard).count())
}

/// The restore after a restart, waiting for the restore window's windows to show (module
/// docs). Pure: the platform feeds it the times and the matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoRestore {
    settling: Settling,
    /// The last time a saved window was matched for the first time (the launch at first).
    last_new: Instant,
    /// Saved windows matched at the last look.
    matched: usize,
    /// Standard windows saved.
    saved: usize,
}

impl AutoRestore {
    /// Opens at the mode's start `launch`; `login`: when the login session began, if known.
    pub fn new(launch: Instant, login: Option<Instant>) -> Self {
        Self { settling: Settling::new(launch, login), last_new: launch, matched: 0, saved: 0 }
    }

    /// An app launched or a window showed at `at`: the login is not quiet yet.
    pub fn busy(&mut self, at: Instant) {
        self.settling.busy(at);
    }

    /// At `at`, `matched` of the `saved` windows match a window shown.
    pub fn seen(&mut self, matched: usize, saved: usize, at: Instant) {
        if matched > self.matched {
            self.last_new = at;
            self.settling.placed(at);
        }
        self.matched = matched;
        self.saved = saved;
    }

    /// Whether it runs now, and why.
    pub fn due(&self, now: Instant) -> Option<&'static str> {
        if self.saved > 0 && self.matched >= self.saved && now.saturating_duration_since(self.last_new) >= ALL_SHOWN_QUIET {
            return Some("every saved window shown");
        }
        match self.settling.due(now) {
            Some(End::TimeUp) => Some("the restore window's five minutes are up"),
            Some(_) => Some("the login settled"),
            None => None,
        }
    }

    /// Saved windows matched at the last look, and saved.
    pub fn counts(&self) -> (usize, usize) {
        (self.matched, self.saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Rect;
    use crate::engine::model::WindowRole;
    use crate::engine::persist::{SavedStage, SavedWindow};
    use crate::engine::testkit::meta;

    fn saved(bundle: &str, title: &str, z: usize, key: bool) -> SavedWindow {
        SavedWindow { bundle_id: bundle.into(), title: title.into(), frame: Rect::new(300.0, 100.0, 600.0, 400.0), z, key, role: SavedRole::Standard, ..SavedWindow::default() }
    }

    fn live(id: u32, pid: i32, bundle: &str, title: &str) -> WindowMeta {
        WindowMeta { title: title.into(), role: WindowRole::Standard, frame: Rect::new(300.0, 100.0, 600.0, 400.0), ..meta(id, pid, bundle) }
    }

    /// Alpha's two windows and Bravo's in one stage, Charlie's alone and active.
    fn space_of_two() -> SavedSpace {
        SavedSpace {
            id: 1,
            current: true,
            stages: vec![
                SavedStage { windows: vec![saved("a", "Alpha 1", 0, true), saved("a", "Alpha 2", 1, false), saved("b", "Bravo 1", 2, false)], ..SavedStage::default() },
                SavedStage { windows: vec![saved("c", "Charlie 1", 0, true)], ..SavedStage::default() },
            ],
            active_index: Some(1),
            ..SavedSpace::default()
        }
    }

    #[test]
    fn the_saved_stages_are_matched_to_the_windows_shown_by_app_and_title() {
        let shown = [live(11, 1, "a", "Alpha 1"), live(12, 1, "a", "Alpha 2"), live(13, 2, "b", "Bravo 1"), live(14, 3, "c", "Charlie 1"), live(15, 4, "d", "Delta 1")];
        let w = wanted(&space_of_two(), &shown, |_| false);
        assert_eq!(w.stages.iter().map(|t| t.windows.clone()).collect::<Vec<_>>(), vec![vec![11, 12, 13], vec![14]], "the key window first; Delta not saved, left out");
        assert_eq!(w.front, Some(1));
        assert_eq!((w.saved, w.matched), (4, 4));
    }

    #[test]
    fn a_saved_window_not_shown_is_counted_and_left_out() {
        let shown = [live(11, 1, "a", "Alpha 1"), live(14, 3, "c", "Charlie 1")];
        let w = wanted(&space_of_two(), &shown, |_| false);
        assert_eq!((w.saved, w.matched), (4, 2));
        assert_eq!(w.stages[0].windows, vec![11]);
    }

    #[test]
    fn under_all_at_once_an_app_split_across_saved_stages_goes_whole_to_the_one_holding_most() {
        let mut w = Wanted {
            stages: vec![Target { windows: vec![1, 2, 3], name: None }, Target { windows: vec![4, 5], name: None }],
            front: Some(1),
            ..Wanted::default()
        };
        // Window 4 is Alpha's, as 1 and 2 are.
        let pids: BTreeMap<u32, i32> = [(1, 10), (2, 10), (3, 20), (4, 10), (5, 30)].into();
        let mut one_at_a_time = w.clone();
        keep_apps_together(&mut one_at_a_time, &pids, false);
        assert_eq!(one_at_a_time, w, "One at a Time: asked as saved");
        keep_apps_together(&mut w, &pids, true);
        assert_eq!(w.stages.iter().map(|t| t.windows.clone()).collect::<Vec<_>>(), vec![vec![1, 2, 3, 4], vec![5]]);
        assert_eq!(w.front, Some(1));
        assert_eq!(w.could_not.len(), 1, "{:?}", w.could_not);
        // An app wholly moved out of a stage of its own: that stage goes.
        let mut w = Wanted { stages: vec![Target { windows: vec![1, 2], name: None }, Target { windows: vec![4], name: None }], front: Some(0), ..Wanted::default() };
        keep_apps_together(&mut w, &pids, true);
        assert_eq!(w.stages.len(), 1);
        assert_eq!(w.front, Some(0));
    }

    #[test]
    fn the_job_puts_the_saved_active_stage_last_and_nothing_wanted_is_refused() {
        let w = Wanted { stages: vec![Target { windows: vec![1], name: None }, Target { windows: vec![2, 3], name: None }, Target { windows: vec![4], name: None }], front: Some(0), ..Wanted::default() };
        let job = plan(&w, true, Instant::now()).expect("planned");
        let fronts: Vec<(Vec<u32>, bool)> = job.goals().iter().map(|g| match g {
            Goal::Stage { windows, front, .. } => (windows.clone(), *front),
            other => panic!("{other:?}"),
        }).collect();
        assert_eq!(fronts, vec![(vec![2, 3], false), (vec![4], false), (vec![1], true)]);
        assert!(job.lenient);
        assert!(plan(&Wanted::default(), true, Instant::now()).is_err());
    }

    #[test]
    fn after_a_restart_it_waits_for_every_saved_window_then_a_quiet_moment() {
        let t0 = Instant::now();
        let mut r = AutoRestore::new(t0, Some(t0));
        r.seen(1, 3, t0 + Duration::from_secs(2));
        assert_eq!(r.due(t0 + Duration::from_secs(10)), None, "two saved windows still to come");
        r.seen(3, 3, t0 + Duration::from_secs(12));
        assert_eq!(r.due(t0 + Duration::from_secs(14)), None, "the last one just came");
        assert_eq!(r.due(t0 + Duration::from_secs(15)), Some("every saved window shown"));
        // A window that never comes: the login settles (quiet 20 s, a minute after the login).
        let mut r = AutoRestore::new(t0, Some(t0));
        r.seen(2, 3, t0 + Duration::from_secs(5));
        assert_eq!(r.due(t0 + Duration::from_secs(40)), None, "under a minute after the login");
        assert_eq!(r.due(t0 + Duration::from_secs(61)), Some("the login settled"));
    }
}
