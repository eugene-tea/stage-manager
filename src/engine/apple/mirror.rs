//! The mirror: Apple's arrangement as read from the window server's list and checked
//! against WindowManager's Accessibility ([`fold`]).
//!
//! Sources, in order of trust (two probes, 11-16, macOS 27.2):
//!
//! 1. **The window list** ([`Listed`]), the fast mirror. Apple animates the real windows, so
//!    their listed bounds move every frame and are final when the motion ends. WindowManager
//!    keeps a layer-0 window of its own exactly over every strip thumbnail (the same bounds as
//!    the window it shows) and one per app icon (66x82): a strip window is a managed window
//!    whose bounds equal a WindowManager window's; an item is the thumbnails nearest one icon.
//! 2. **WindowManager's Accessibility** ([`Reading`]: `sm.strip`'s items with `AXWindowsIDs`
//!    and `AXFrame`), the check on the list's grouping. It catches up about 785 ms after a
//!    change and reads empty for about 650 ms of it: an empty reading is no reading. Where the
//!    two disagree after a second settled, the list wins and the disagreement is counted.
//!
//! Rules of [`fold`]:
//!
//! - **Current stage**: managed full-size windows on screen with no WindowManager window over
//!   them, front to back.
//! - **In motion**: any managed window's bounds differ from the read before, or a step of
//!   ours began less than [`SETTLE`] ago. Groups are taken from still reads only; while in
//!   motion the groups read before stand.
//! - **Settled**: no motion for [`STILL_READS`] reads and at least [`SETTLE`] since the last
//!   change: what a posted step waits for (A3), and when the model follows.
//! - **Stages Apple no longer lists** (past the strip's room, on a Desktop not shown) stay as
//!   remembered groups with their last membership; a window gone from the list leaves its
//!   group, a window seen again leaves it for what is seen.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::engine::geometry::Rect;
use crate::engine::model::Display;

use super::{in_strip_column, thumbnail_sized};

/// How long a change keeps the arrangement unsettled: Apple's motion, about 700 ms
/// (a recording of 11-16: a focus at .305, motion from .404 to about .900; clicks before
/// ~700 ms are ignored or switch back).
pub const SETTLE: Duration = Duration::from_millis(700);

/// Reads in a row with no bounds changed before the arrangement counts settled.
pub const STILL_READS: u32 = 2;

/// How long the arrangement is settled before a disagreement with Accessibility counts
/// (Accessibility catches up about 785 ms after a change).
pub const AX_TRUST: Duration = Duration::from_secs(1);

/// The read's pace while in motion (the window list only, on the fast tick).
pub const MOTION_READ: Duration = Duration::from_millis(50);

/// The read's pace at rest (the Accessibility check with it).
pub const REST_READ: Duration = Duration::from_secs(2);

/// An app icon window of WindowManager's in the strip, in points (measured).
pub const ICON: (f64, f64) = (66.0, 82.0);

/// Bounds this close on every side are the same (the window server rounds to whole points).
const SAME: f64 = 1.0;

/// A window as the window server lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedWindow {
    /// `CGWindowID`.
    pub id: u32,
    /// Its process.
    pub pid: i32,
    /// Its layer (app windows and WindowManager's strip windows: 0).
    pub layer: i64,
    /// Its bounds, top-left origin.
    pub bounds: Rect,
    /// Listed on screen.
    pub onscreen: bool,
}

/// The window server's list at one read: every window, the on-screen ones front to back
/// first, and which process is WindowManager's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listed {
    /// The windows, front to back (on screen first).
    pub windows: Vec<ListedWindow>,
    /// WindowManager's pid, when it runs.
    pub manager: Option<i32>,
}

impl Listed {
    /// WindowManager's layer-0 windows: the strip's thumbnails' and icons' stand-ins.
    fn manager_windows(&self) -> impl Iterator<Item = &ListedWindow> {
        self.windows.iter().filter(move |w| Some(w.pid) == self.manager && w.layer == 0)
    }

    /// True when window `id` is listed at all (on screen or not): a window gone from the
    /// list is closed.
    pub fn has(&self, id: u32) -> bool {
        self.windows.iter().any(|w| w.id == id)
    }
}

/// One strip item as WindowManager's Accessibility shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Item {
    /// Its `AXFrame`.
    pub frame: Rect,
    /// Its `AXWindowsIDs`: the windows of that stage.
    pub windows: Vec<u32>,
}

/// One screen's strip as WindowManager's Accessibility shows it (`sm.space` > `sm.strip`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Reading {
    /// The items, as listed.
    pub items: Vec<Item>,
}

impl Reading {
    /// True when the strip reads empty: Apple's rebuilds it for about 650 ms after every
    /// change, so an empty reading is no reading.
    pub fn is_blank(&self) -> bool {
        self.items.iter().all(|i| i.windows.is_empty())
    }
}

/// Where a stage is in Apple's arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Where {
    /// On screen, in front.
    Current,
    /// An item of the strip.
    Strip,
    /// Not listed any more (past the strip's room, on a Desktop not shown): its last
    /// membership.
    Remembered,
}

/// A stage of Apple's: its windows, front to back, and where its item was.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Group {
    /// Its windows, front to back.
    pub windows: Vec<u32>,
    /// Its item's place (the union of its thumbnails and its icon); for a remembered group,
    /// where it was last seen.
    pub frame: Rect,
}

/// Apple's arrangement on one screen as last read ([`fold`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Arrangement {
    /// The current stage's windows, front to back.
    pub current: Vec<u32>,
    /// The strip's items, top to bottom (the slot is the index).
    pub strip: Vec<Group>,
    /// Stages no longer listed, with their last membership.
    pub remembered: Vec<Group>,
    /// Whether the strip shows: `Some(false)` with its windows off the left edge (a window
    /// over it, or Apple's recent apps hidden), `None` with nothing in it.
    pub strip_shown: Option<bool>,
    /// A managed window's bounds changed since the read before, or a step of ours began less
    /// than [`SETTLE`] ago.
    pub in_motion: bool,
    /// Still for [`STILL_READS`] reads and [`SETTLE`] since the last change.
    pub settled: bool,
    /// Every managed window's listed bounds at this read.
    pub frames: BTreeMap<u32, Rect>,
    /// Each window's bounds when last seen at full size in the current stage: the frame the
    /// model records, never a thumbnail's.
    pub full: BTreeMap<u32, Rect>,
    /// When a managed window's bounds last changed.
    pub changed_at: Option<Instant>,
    /// Reads in a row with no bounds changed.
    pub still_reads: u32,
    /// This read.
    pub read_at: Option<Instant>,
    /// When a step of ours last began ([`Arrangement::step_began`]).
    pub step_at: Option<Instant>,
    /// The last Accessibility reading that was not blank.
    pub ax_at: Option<Instant>,
    /// Disagreements of the list with Accessibility counted (each spell once).
    pub misses: u64,
    /// The list and Accessibility disagree now (settled a second).
    pub disagree: bool,
}

impl Arrangement {
    /// Where window `id` is: its stage's place and, in the strip, its slot.
    pub fn where_is(&self, id: u32) -> Option<(Where, Option<usize>)> {
        if self.current.contains(&id) {
            return Some((Where::Current, None));
        }
        if let Some(slot) = self.strip.iter().position(|g| g.windows.contains(&id)) {
            return Some((Where::Strip, Some(slot)));
        }
        self.remembered.iter().any(|g| g.windows.contains(&id)).then_some((Where::Remembered, None))
    }

    /// Every group: the current stage (when it holds a window), the strip's items top to
    /// bottom, the remembered ones.
    pub fn groups(&self) -> Vec<(Where, &[u32])> {
        let mut out = Vec::new();
        if !self.current.is_empty() {
            out.push((Where::Current, self.current.as_slice()));
        }
        out.extend(self.strip.iter().map(|g| (Where::Strip, g.windows.as_slice())));
        out.extend(self.remembered.iter().map(|g| (Where::Remembered, g.windows.as_slice())));
        out
    }

    /// Stage `n` (1-based) for the hotkeys and `ctl switch`: the strip's slots top to bottom,
    /// then the remembered groups.
    pub fn by_number(&self, n: usize) -> Option<&Group> {
        let i = n.checked_sub(1)?;
        if i < self.strip.len() { self.strip.get(i) } else { self.remembered.get(i - self.strip.len()) }
    }

    /// The grouping alone, comparable across reads: each group's windows sorted, the current
    /// one first.
    pub fn grouping(&self) -> (Vec<u32>, Vec<Vec<u32>>) {
        let sorted = |w: &[u32]| {
            let mut v = w.to_vec();
            v.sort_unstable();
            v
        };
        let mut rest: Vec<Vec<u32>> = self.strip.iter().chain(&self.remembered).map(|g| sorted(&g.windows)).collect();
        rest.sort();
        (sorted(&self.current), rest)
    }

    /// A step of ours began at `now` (a focus, a posted click): the arrangement counts in
    /// motion for [`SETTLE`], whatever the list shows meanwhile.
    pub fn step_began(&mut self, now: Instant) {
        self.step_at = Some(now);
        self.settled = false;
    }

    /// When the next read is due after this one: soon while in motion or not yet settled,
    /// at rest otherwise.
    pub fn next_read(&self) -> Duration {
        if self.in_motion || !self.settled { MOTION_READ } else { REST_READ }
    }
}

/// True when `a` and `b` are the same bounds within [`SAME`] on every side.
fn same(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= SAME && (a.y - b.y).abs() <= SAME && (a.w - b.w).abs() <= SAME && (a.h - b.h).abs() <= SAME
}

/// True when `r` is an app icon window's size.
fn icon_sized(r: Rect) -> bool {
    (r.w - ICON.0).abs() <= 2.0 && (r.h - ICON.1).abs() <= 2.0
}

/// The next arrangement from the one before (`prev`), a reading of Accessibility (`reading`,
/// `None` when not read this time), the window list (`listed`) and the windows the app
/// manages (`managed`), on screen `screen`, at `now`. See the module docs for the rules.
pub fn fold(prev: &Arrangement, reading: Option<&Reading>, listed: &Listed, managed: &BTreeSet<u32>, screen: &Display, now: Instant) -> Arrangement {
    let manager: Vec<&ListedWindow> = listed.manager_windows().collect();
    let icons: Vec<Rect> = manager.iter().map(|w| w.bounds).filter(|b| icon_sized(*b)).collect();
    let overlays: Vec<Rect> = manager.iter().map(|w| w.bounds).filter(|b| !icon_sized(*b)).collect();

    let mut frames = BTreeMap::new();
    let mut current = Vec::new();
    let mut thumbs: Vec<(u32, Rect)> = Vec::new();
    for w in listed.windows.iter().filter(|w| managed.contains(&w.id)) {
        if frames.insert(w.id, w.bounds).is_some() {
            continue;
        }
        let under_overlay = overlays.iter().any(|o| same(*o, w.bounds));
        // Off the left edge, thumbnail-sized: the strip hidden (its stand-ins may not follow).
        let off_left = thumbnail_sized(w.bounds) && w.bounds.max_x() <= screen.frame.x;
        if under_overlay || off_left {
            thumbs.push((w.id, w.bounds));
        } else if w.onscreen && !w.bounds.is_degenerate() && w.bounds.intersect(&screen.frame).is_some_and(|r| r.w > 0.0 && r.h > 0.0) {
            current.push(w.id);
        }
    }

    let step_moving = prev.step_at.is_some_and(|t| now.saturating_duration_since(t) < SETTLE);
    let moved = prev.read_at.is_some() && frames.iter().any(|(id, b)| prev.frames.get(id).is_some_and(|p| !same(*p, *b)));
    let in_motion = moved || step_moving;
    let changed_at = if moved { Some(now) } else { prev.changed_at };
    let still_reads = if moved { 0 } else { prev.still_reads.saturating_add(1) };
    let since_change = changed_at.is_none_or(|t| now.saturating_duration_since(t) >= SETTLE);
    let settled = !in_motion && still_reads >= STILL_READS && since_change;

    let mut next = Arrangement {
        frames,
        changed_at,
        still_reads,
        in_motion,
        settled,
        read_at: Some(now),
        step_at: prev.step_at,
        ax_at: prev.ax_at,
        misses: prev.misses,
        disagree: prev.disagree,
        full: prev.full.clone(),
        ..Arrangement::default()
    };
    // Closed windows leave what is kept of them.
    next.full.retain(|id, _| listed.has(*id) && managed.contains(id));

    if moved {
        // Mid-motion bounds say nothing of the groups: the ones read before stand, less the
        // windows closed meanwhile.
        next.current = prev.current.iter().copied().filter(|id| listed.has(*id) && managed.contains(id)).collect();
        next.strip = kept(&prev.strip, listed, managed);
        next.remembered = kept(&prev.remembered, listed, managed);
        next.strip_shown = prev.strip_shown;
    } else {
        for id in &current {
            if let Some(b) = next.frames.get(id)
                && !thumbnail_sized(*b)
            {
                next.full.insert(*id, *b);
            }
        }
        next.strip_shown = match thumbs.is_empty() {
            true => None,
            false => Some(thumbs.iter().any(|(_, b)| in_strip_column(screen, *b))),
        };
        next.strip = items(&thumbs, &icons);
        next.current = current;
        next.remembered = remembered(prev, &next, listed, managed);
    }

    // The check against Accessibility: a blank reading is no reading.
    if let Some(r) = reading.filter(|r| !r.is_blank()) {
        next.ax_at = Some(now);
        let settled_long = next.settled && changed_at.is_none_or(|t| now.saturating_duration_since(t) >= SETTLE + AX_TRUST);
        let agree = same_groups(&next.strip, r, managed);
        if agree {
            next.disagree = false;
        } else if settled_long {
            if !next.disagree {
                next.misses += 1;
            }
            next.disagree = true;
        }
    }
    next
}

/// `groups` less the windows closed (or no longer managed); groups left empty go.
fn kept(groups: &[Group], listed: &Listed, managed: &BTreeSet<u32>) -> Vec<Group> {
    groups
        .iter()
        .map(|g| Group { windows: g.windows.iter().copied().filter(|id| listed.has(*id) && managed.contains(id)).collect(), frame: g.frame })
        .filter(|g| !g.windows.is_empty())
        .collect()
}

/// The strip's items from its thumbnails (`thumbs`, front to back) and WindowManager's icon
/// windows (`icons`): each thumbnail goes with the icon nearest it; with no icon, thumbnails
/// that overlap make one item. Top to bottom.
fn items(thumbs: &[(u32, Rect)], icons: &[Rect]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    if !icons.is_empty() {
        let mut by_icon: BTreeMap<usize, Group> = BTreeMap::new();
        for (id, b) in thumbs {
            let c = b.center();
            let nearest = icons
                .iter()
                .enumerate()
                .min_by(|(_, x), (_, y)| {
                    let d = |r: &Rect| (r.center().x - c.x).powi(2) + (r.center().y - c.y).powi(2);
                    d(x).total_cmp(&d(y))
                })
                .map(|(i, _)| i)
                .unwrap_or(0);
            let g = by_icon.entry(nearest).or_insert_with(|| Group { windows: Vec::new(), frame: icons[nearest] });
            g.windows.push(*id);
            g.frame = g.frame.union(b);
        }
        groups.extend(by_icon.into_values());
    } else {
        for (id, b) in thumbs {
            let joined: Vec<usize> = groups.iter().enumerate().filter(|(_, g)| g.frame.intersect(b).is_some_and(|r| r.w > 0.0 && r.h > 0.0)).map(|(i, _)| i).collect();
            match joined.as_slice() {
                [] => groups.push(Group { windows: vec![*id], frame: *b }),
                [first, rest @ ..] => {
                    let first = *first;
                    for &i in rest.iter().rev() {
                        let g = groups.remove(i);
                        groups[first].windows.extend(g.windows);
                        groups[first].frame = groups[first].frame.union(&g.frame);
                    }
                    groups[first].windows.push(*id);
                    groups[first].frame = groups[first].frame.union(b);
                }
            }
        }
    }
    groups.sort_by(|a, b| a.frame.y.total_cmp(&b.frame.y));
    groups
}

/// The groups Apple no longer lists: what `prev` had (its remembered groups, then its strip
/// items and current stage that are gone), less the windows seen in `next` or closed.
fn remembered(prev: &Arrangement, next: &Arrangement, listed: &Listed, managed: &BTreeSet<u32>) -> Vec<Group> {
    let seen: BTreeSet<u32> = next.current.iter().chain(next.strip.iter().flat_map(|g| g.windows.iter())).copied().collect();
    let prev_current = (!prev.current.is_empty()).then(|| Group { windows: prev.current.clone(), frame: Rect::new(0.0, 0.0, 0.0, 0.0) });
    let mut out: Vec<Group> = Vec::new();
    for g in prev.remembered.iter().chain(prev.strip.iter()).chain(prev_current.iter()) {
        let windows: Vec<u32> = g
            .windows
            .iter()
            .copied()
            .filter(|id| !seen.contains(id) && listed.has(*id) && managed.contains(id) && !out.iter().any(|o| o.windows.contains(id)))
            .collect();
        if !windows.is_empty() {
            out.push(Group { windows, frame: g.frame });
        }
    }
    out
}

/// True when the list's strip items and Accessibility's (`r`) hold the same windows in the
/// same groups, counting only the managed windows.
fn same_groups(strip: &[Group], r: &Reading, managed: &BTreeSet<u32>) -> bool {
    let norm = |ws: &mut dyn Iterator<Item = u32>| {
        let mut v: Vec<u32> = ws.filter(|id| managed.contains(id)).collect();
        v.sort_unstable();
        v
    };
    let mut list: Vec<Vec<u32>> = strip.iter().map(|g| norm(&mut g.windows.iter().copied())).filter(|v| !v.is_empty()).collect();
    let mut ax: Vec<Vec<u32>> = r.items.iter().map(|i| norm(&mut i.windows.iter().copied())).filter(|v| !v.is_empty()).collect();
    list.sort();
    ax.sort();
    list == ax
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testkit::apple::{FakeWindowManager, THUMB};
    use crate::engine::testkit::display;

    fn managed(f: &FakeWindowManager) -> BTreeSet<u32> {
        f.window_ids()
    }

    /// Reads the fake every 50 ms of its clock until settled (at most 3 s).
    fn settle(f: &mut FakeWindowManager, a: Arrangement) -> Arrangement {
        let mut a = a;
        for _ in 0..60 {
            f.advance(50);
            a = fold(&a, Some(&f.reading()), &f.listed(), &managed(f), &display(), f.now());
            if a.settled {
                return a;
            }
        }
        panic!("never settled: {a:?}");
    }

    fn start() -> (FakeWindowManager, Arrangement) {
        let mut f = FakeWindowManager::new();
        f.open_app(10, &[1, 2]);
        f.open_app(20, &[3]);
        f.open_app(30, &[4]);
        f.advance(1000);
        let a = fold(&Arrangement::default(), None, &f.listed(), &managed(&f), &display(), f.now());
        let a = settle(&mut f, a);
        (f, a)
    }

    #[test]
    fn the_list_alone_gives_the_current_stage_and_the_strips_items() {
        let (_, a) = start();
        assert_eq!(a.current, vec![4], "the app opened last is in front");
        let groups: Vec<Vec<u32>> = a.strip.iter().map(|g| { let mut w = g.windows.clone(); w.sort(); w }).collect();
        assert_eq!(groups, vec![vec![3], vec![1, 2]], "top to bottom: the stage left last on top");
        assert_eq!(a.strip_shown, Some(true));
        assert!(a.remembered.is_empty());
        assert_eq!(a.where_is(2), Some((Where::Strip, Some(1))));
        assert_eq!(a.by_number(1).map(|g| g.windows.clone()), Some(vec![3]));
        assert!(a.by_number(3).is_none());
        assert!(a.full.contains_key(&4), "the current window's full-size frame is kept");
        assert!(!a.full.values().any(|r| r.w == THUMB.w && r.h == THUMB.h), "never a thumbnail's");
    }

    #[test]
    fn a_change_is_in_motion_until_the_list_is_still_for_two_reads_and_700_ms() {
        let (mut f, a) = start();
        f.focus_window(3);
        f.advance(50);
        let b = fold(&a, None, &f.listed(), &managed(&f), &display(), f.now());
        assert!(b.in_motion && !b.settled, "the windows move every frame");
        assert_eq!(b.current, a.current, "mid-motion bounds say nothing of the groups");
        let c = settle(&mut f, b);
        assert!(c.changed_at.is_some_and(|t| f.now().saturating_duration_since(t) >= SETTLE));
        assert_eq!(c.current, vec![3]);
        assert!(c.strip.iter().any(|g| g.windows == vec![4]), "the stage left goes to the strip");
    }

    #[test]
    fn a_step_of_ours_counts_as_motion_for_700_ms() {
        let (mut f, mut a) = start();
        a.step_began(f.now());
        f.advance(50);
        let b = fold(&a, None, &f.listed(), &managed(&f), &display(), f.now());
        assert!(b.in_motion && !b.settled, "nothing moved yet, but a step began");
        f.advance(700);
        let c = fold(&b, None, &f.listed(), &managed(&f), &display(), f.now());
        assert!(!c.in_motion);
    }

    #[test]
    fn the_blank_accessibility_read_is_no_reading_and_a_settled_disagreement_counts_once() {
        let (mut f, a) = start();
        f.focus_window(3);
        f.advance(100);
        assert!(f.reading().is_blank(), "the fake rebuilds the strip for 650 ms");
        let b = fold(&a, Some(&f.reading()), &f.listed(), &managed(&f), &display(), f.now());
        assert_eq!(b.misses, 0);
        let b = settle(&mut f, b);
        f.advance(1100);
        let b = fold(&b, Some(&f.reading()), &f.listed(), &managed(&f), &display(), f.now());
        assert_eq!(b.misses, 0, "they agree");
        assert!(b.ax_at == Some(f.now()));
        // Accessibility says otherwise for a while: one miss for the spell, the list wins.
        let wrong = Reading { items: vec![Item { frame: Rect::new(0.0, 0.0, 1.0, 1.0), windows: vec![1, 2, 4] }] };
        f.advance(50);
        let c = fold(&b, Some(&wrong), &f.listed(), &managed(&f), &display(), f.now());
        f.advance(50);
        let c = fold(&c, Some(&wrong), &f.listed(), &managed(&f), &display(), f.now());
        assert_eq!(c.misses, 1);
        assert!(c.disagree);
        assert_eq!(c.strip, b.strip, "the list wins");
        f.advance(50);
        let d = fold(&c, Some(&f.reading()), &f.listed(), &managed(&f), &display(), f.now());
        assert!(!d.disagree && d.misses == 1);
    }

    #[test]
    fn a_stage_past_the_strips_room_is_remembered_and_comes_back_when_seen() {
        let (mut f, a) = start();
        f.set_room(1);
        let b = settle(&mut f, a);
        assert_eq!(b.strip.len(), 1);
        assert_eq!(b.remembered.iter().map(|g| { let mut w = g.windows.clone(); w.sort(); w }).collect::<Vec<_>>(), vec![vec![1, 2]], "dropped from the strip: remembered");
        assert_eq!(b.by_number(2).map(|g| g.windows.len()), Some(2), "N counts the strip's slots, then the remembered");
        f.set_room(4);
        let c = settle(&mut f, b);
        assert!(c.remembered.is_empty(), "seen again: overwritten by what is seen");
        assert_eq!(c.strip.len(), 2);
    }

    #[test]
    fn the_hidden_strip_keeps_its_items_off_the_left_edge() {
        let (mut f, a) = start();
        f.cover(true);
        let b = settle(&mut f, a);
        assert_eq!(b.strip_shown, Some(false));
        assert_eq!(b.strip.len(), 2, "its items still read from the list");
        assert_eq!(b.current, vec![4]);
    }

    #[test]
    fn a_closed_window_leaves_its_group_and_an_emptied_group_goes() {
        let (mut f, a) = start();
        f.close(3);
        let b = settle(&mut f, a);
        assert!(b.where_is(3).is_none());
        assert_eq!(b.strip.len(), 1);
        f.set_room(0);
        let c = settle(&mut f, b);
        assert_eq!(c.remembered.len(), 1);
        f.close(1);
        f.close(2);
        let d = settle(&mut f, c);
        assert!(d.remembered.is_empty(), "closed windows leave even a remembered group");
    }

    #[test]
    fn with_no_icon_window_overlapping_thumbnails_make_one_item() {
        let t = |id, y| (id, Rect::new(15.0, y, 79.0, 104.0));
        let g = items(&[t(1, 300.0), t(2, 306.0), t(3, 500.0)], &[]);
        assert_eq!(g.iter().map(|g| g.windows.clone()).collect::<Vec<_>>(), vec![vec![1, 2], vec![3]]);
    }
}
