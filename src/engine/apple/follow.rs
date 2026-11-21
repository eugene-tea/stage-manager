//! The model follows Apple's arrangement ([`follow`]): an [`Arrangement`] against the app's
//! own [`Model`] gives the model's existing ops to call, with the actions they return dropped
//! (Apple's arranges the windows; nothing of ours moves one). The model never runs ahead of
//! Apple's: it only follows. So `state.json`'s format, stage numbers and names, `ctl state`,
//! the restore match and the rules are the shared ones, unchanged.
//!
//! The ops: [`Model::remove_window`] for a window gone, [`Model::add_window`] (joining the
//! stage in front, a rule's `current`) for one first seen, [`Model::switch_to`] for the stage
//! in front, [`Model::send_window`] for a window in another stage than Apple's shows it in
//! (to a stage of the model's by number, or to a new one). Every group of Apple's gets one
//! stage of the model's: the one already holding most of its windows that no group before it
//! took, else a new one; the current stage's group goes first and takes the stage in front.
//!
//! What the model records of a window is never a thumbnail's frame: a window first seen in
//! the strip comes in at the last full-size frame the mirror saw ([`Arrangement::full`]), the
//! frames of the stage in front are its listed bounds, and the strip's are not observed.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::mirror::Arrangement;
use crate::engine::model::{Model, Observation, StageId, Target, WindowId, WindowMeta, WindowRole};
use crate::engine::rules::{Decision, Placement, RuleAction};

/// What a [`follow`] changed in the model, for the log and `ctl state`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Followed {
    /// Windows first seen, put in.
    pub added: usize,
    /// Windows gone, taken out.
    pub removed: usize,
    /// Windows sent to another stage of the model's.
    pub moved: usize,
    /// The stage in front changed.
    pub switched: bool,
}

impl Followed {
    /// True when nothing changed.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A window first seen joins the stage in front; its stage is Apple's, set right after.
fn joins_in_front() -> Decision {
    Decision { action: RuleAction::Current, placement: Placement::AsIs, new_app_joins_current: false }
}

/// The model `m` brought in line with arrangement `a`: `metas` are the standard windows the
/// app manages now (by id; others are none of the model's), `focused` the focused one. See
/// the module docs.
pub fn follow(m: &mut Model, a: &Arrangement, metas: &BTreeMap<u32, WindowMeta>, focused: Option<u32>) -> Followed {
    let mut done = Followed::default();
    let managed: BTreeSet<u32> = metas.iter().filter(|(_, meta)| meta.role == WindowRole::Standard).map(|(id, _)| *id).collect();

    // Windows gone.
    let gone: Vec<WindowId> = m.windows().filter(|w| w.meta.role == WindowRole::Standard && !managed.contains(&w.meta.id.0)).map(|w| w.meta.id).collect();
    for id in gone {
        let _ = m.remove_window(id);
        done.removed += 1;
    }

    // What the platform sees of the stage in front: its listed bounds, front to back.
    let obs = Observation {
        frames: a.current.iter().filter_map(|id| a.frames.get(id).map(|f| (WindowId(*id), *f))).collect::<HashMap<_, _>>(),
        z_order: a.current.iter().map(|id| WindowId(*id)).collect(),
        focused: focused.filter(|f| a.current.contains(f)).map(WindowId),
    };

    // Windows first seen, at a full-size frame.
    for id in &managed {
        if m.window(WindowId(*id)).is_some() {
            continue;
        }
        let mut meta = metas[id].clone();
        if let Some(full) = a.full.get(id) {
            meta.frame = *full;
        }
        let _ = m.add_window(meta, &joins_in_front(), &obs);
        done.added += 1;
    }

    // The stage in front first: Apple's current stage's windows in it.
    let mut taken: BTreeSet<StageId> = BTreeSet::new();
    let current: Vec<u32> = a.current.iter().copied().filter(|id| managed.contains(id)).collect();
    if let Some(target) = most_held(m, &current, &taken)
        && m.main_active_stage() != Some(target)
    {
        let _ = m.switch_to(target, &obs);
        done.switched = m.main_active_stage() == Some(target);
    }
    if let Some(front) = m.main_active_stage() {
        taken.insert(front);
        done.moved += gather(m, &current, front, &obs);
    }

    // Each other group: the stage holding most of it that no group took, else a new one.
    // A Desktop not shown keeps its stages in the model too (its groups away, never acted on).
    for group in a.strip.iter().chain(&a.remembered).chain(a.away.iter().map(|(_, g)| g)) {
        let windows: Vec<u32> = group.windows.iter().copied().filter(|id| managed.contains(id)).collect();
        let Some(first) = windows.first().copied() else { continue };
        let target = match most_held(m, &windows, &taken) {
            Some(s) => s,
            None => {
                let _ = m.send_window(WindowId(first), Target::New, &obs);
                done.moved += 1;
                match m.stage_of(WindowId(first)) {
                    Some(s) => s,
                    None => continue,
                }
            }
        };
        taken.insert(target);
        done.moved += gather(m, &windows, target, &obs);
    }
    done
}

/// The stage of the model's holding most of `windows` that is not `taken` (ties: the first
/// window's).
fn most_held(m: &Model, windows: &[u32], taken: &BTreeSet<StageId>) -> Option<StageId> {
    let mut counts: Vec<(StageId, usize)> = Vec::new();
    for id in windows {
        let Some(s) = m.stage_of(WindowId(*id)).filter(|s| !taken.contains(s)) else { continue };
        match counts.iter_mut().find(|(c, _)| *c == s) {
            Some((_, n)) => *n += 1,
            None => counts.push((s, 1)),
        }
    }
    let best = counts.iter().map(|(_, n)| *n).max()?;
    counts.into_iter().find(|(_, n)| *n == best).map(|(s, _)| s)
}

/// Every window of `windows` not in stage `target` sent there; how many were.
fn gather(m: &mut Model, windows: &[u32], target: StageId, obs: &Observation) -> usize {
    let mut moved = 0;
    for id in windows {
        if m.stage_of(WindowId(*id)) == Some(target) {
            continue;
        }
        let Some(n) = m.stage_number(target) else { break };
        let _ = m.send_window(WindowId(*id), Target::Number(n), obs);
        moved += 1;
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::apple::mirror::{Arrangement, fold};
    use crate::engine::model::ModelOptions;
    use crate::engine::recovery::EngineKind;
    use crate::engine::testkit::apple::FakeWindowManager;
    use crate::engine::testkit::{display, meta, model_with};

    fn apple_model() -> Model {
        model_with(ModelOptions { engine: EngineKind::Apple, ..ModelOptions::default() })
    }

    fn metas(f: &FakeWindowManager) -> BTreeMap<u32, WindowMeta> {
        let pid = |id: u32| f.listed().windows.iter().find(|w| w.id == id).map_or(0, |w| w.pid);
        f.window_ids().into_iter().map(|id| (id, WindowMeta { frame: f.frame(id).unwrap_or_default(), ..meta(id, pid(id), "com.example.app") })).collect()
    }

    fn settled(f: &mut FakeWindowManager, a: &Arrangement) -> Arrangement {
        let mut a = a.clone();
        for _ in 0..60 {
            f.advance(50);
            a = fold(&a, Some(&f.reading()), &f.listed(), &f.window_ids(), &display(), f.now());
            if a.settled && !f.busy() {
                return a;
            }
        }
        panic!("never settled");
    }

    /// The model's stages as sets of windows, the one in front first.
    fn model_groups(m: &Model) -> (Vec<u32>, Vec<Vec<u32>>) {
        let set = |s: StageId| {
            let mut v: Vec<u32> = m.stage(s).map(|s| s.windows.iter().map(|w| w.0).collect()).unwrap_or_default();
            v.sort_unstable();
            v
        };
        let front = m.main_active_stage();
        let mut rest: Vec<Vec<u32>> = m.stages().iter().filter(|s| Some(s.id) != front).map(|s| set(s.id)).filter(|v| !v.is_empty()).collect();
        rest.sort();
        (front.map(set).unwrap_or_default(), rest)
    }

    fn three_apps() -> (FakeWindowManager, Arrangement, Model) {
        let mut f = FakeWindowManager::new();
        f.open_app(10, &[1, 2]);
        f.open_app(20, &[3]);
        f.open_app(30, &[4]);
        let a = settled(&mut f, &Arrangement::default());
        let mut m = apple_model();
        let d = follow(&mut m, &a, &metas(&f), Some(4));
        assert_eq!(d.added, 4);
        (f, a, m)
    }

    #[test]
    fn the_model_takes_apples_stages_as_they_are() {
        let (_, a, m) = three_apps();
        assert_eq!(model_groups(&m), a.grouping(), "one stage of the model's per group of Apple's");
        assert_eq!(model_groups(&m).0, vec![4]);
        for id in [1, 2, 3] {
            assert!(m.window(WindowId(id)).is_some_and(|w| w.parked), "a strip window counts hidden");
        }
        assert!(!m.window(WindowId(4)).is_some_and(|w| w.parked));
    }

    #[test]
    fn a_focus_switch_by_apples_is_followed_and_following_again_changes_nothing() {
        let (mut f, a, mut m) = three_apps();
        f.focus_window(1);
        let b = settled(&mut f, &a);
        let d = follow(&mut m, &b, &metas(&f), Some(1));
        assert!(d.switched);
        assert_eq!(model_groups(&m), b.grouping());
        assert_eq!(model_groups(&m).0, vec![1, 2]);
        assert_eq!(f.current(), &[1, 2], "Apple's brought the focused window's stage in front");
        assert_eq!(f.stages().len(), 2);
        let again = follow(&mut m, &b, &metas(&f), Some(1));
        assert!(again.is_empty(), "nothing to change: {again:?}");
    }

    #[test]
    fn a_new_app_gets_its_stage_and_a_closed_window_leaves() {
        let (mut f, a, mut m) = three_apps();
        f.open_app(40, &[5]);
        f.close(3);
        let b = settled(&mut f, &a);
        let d = follow(&mut m, &b, &metas(&f), Some(5));
        assert_eq!((d.added, d.removed), (1, 1));
        assert_eq!(model_groups(&m), b.grouping());
        assert!(m.window(WindowId(3)).is_none());
    }

    #[test]
    fn windows_merged_or_split_by_apples_are_regrouped() {
        let mut m = apple_model();
        let mut a = Arrangement::default();
        let g = |w: &[u32]| crate::engine::apple::mirror::Group { windows: w.to_vec(), frame: crate::engine::geometry::Rect::new(15.0, 100.0, 79.0, 104.0) };
        a.current = vec![1];
        a.strip = vec![g(&[2]), g(&[3])];
        let all: BTreeMap<u32, WindowMeta> = [1, 2, 3].into_iter().map(|id| (id, meta(id, 10 * id as i32, "com.example.app"))).collect();
        follow(&mut m, &a, &all, Some(1));
        assert_eq!(model_groups(&m), a.grouping());
        // A Shift-click brought window 3 in: one stage of 1 and 3.
        a.current = vec![3, 1];
        a.strip = vec![g(&[2])];
        follow(&mut m, &a, &all, Some(3));
        assert_eq!(model_groups(&m), a.grouping());
        // Window 1 dragged out to a stage of its own.
        a.current = vec![3];
        a.strip = vec![g(&[1]), g(&[2])];
        follow(&mut m, &a, &all, Some(3));
        assert_eq!(model_groups(&m), a.grouping());
        // A stage past the strip's room stays a stage of the model's.
        a.strip = vec![g(&[1])];
        a.remembered = vec![g(&[2])];
        let d = follow(&mut m, &a, &all, Some(3));
        assert!(d.is_empty(), "remembered is still a stage: {d:?}");
    }

    #[test]
    fn no_frame_the_model_records_is_a_thumbnails() {
        let (_, _, m) = three_apps();
        for w in m.windows() {
            assert!(!crate::engine::apple::thumbnail_sized(w.restore), "window {}: {:?}", w.meta.id.0, w.restore);
        }
    }
}
