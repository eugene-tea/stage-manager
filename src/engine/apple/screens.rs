//! More than one screen in the Apple mode: one mirror per screen. WindowManager's
//! Accessibility has one `sm.space` per screen (the main screen first), each with its own
//! strip ([`reading_for`]); the window list is one for all screens, so each managed window is
//! given to one screen before the folds ([`split`]). Nothing is assumed about a second
//! screen's layout beyond its frame: what each screen's mirror sees is logged.
//!
//! Which screen a window is on:
//!
//! - its bounds' middle on a screen's frame: that screen; but a thumbnail-sized window with
//!   no twin (a hidden strip's, listed off its screen's left edge, which may lie on the screen
//!   to the left) stays with the screen it was on;
//! - off every screen (a hidden strip's thumbnail, a window listed off screen): the screen it
//!   was on, else the screen whose left edge is nearest right of it at its height, else the
//!   one it overlaps most, else the main one.

use std::collections::{BTreeMap, BTreeSet};

use super::mirror::{Listed, Reading, has_twin};
use super::thumbnail_sized;
use crate::engine::geometry::Rect;
use crate::engine::model::{Display, DisplayId};

/// A screen's frame and an `sm.space`'s are the same within this (points).
const SAME: f64 = 2.0;

/// The managed windows of `listed` given to each of `displays` (the main one first; every
/// display gets an entry), `before` saying which screen each window was on at the read
/// before. See the module docs.
pub fn split(listed: &Listed, managed: &BTreeSet<u32>, displays: &[Display], before: &BTreeMap<u32, DisplayId>) -> BTreeMap<DisplayId, BTreeSet<u32>> {
    let mut out: BTreeMap<DisplayId, BTreeSet<u32>> = displays.iter().map(|d| (d.id, BTreeSet::new())).collect();
    let Some(main) = displays.first() else { return out };
    let mut placed: BTreeSet<u32> = BTreeSet::new();
    for (i, w) in listed.windows.iter().enumerate() {
        if !managed.contains(&w.id) || !placed.insert(w.id) {
            continue;
        }
        let was = before.get(&w.id).filter(|d| out.contains_key(d)).copied();
        let mid = w.bounds.center();
        let on = displays.iter().find(|d| d.frame.contains(mid)).map(|d| d.id);
        let lone_thumb = thumbnail_sized(w.bounds) && !has_twin(listed, i);
        let d = match (on, was) {
            (Some(_), Some(was)) if lone_thumb => was,
            (Some(on), _) => on,
            (None, Some(was)) => was,
            (None, None) => left_of(displays, w.bounds).or_else(|| most_of(displays, w.bounds)).unwrap_or(main.id),
        };
        out.entry(d).or_default().insert(w.id);
    }
    out
}

/// The screen whose left edge is nearest right of `r` at `r`'s height (a hidden strip's
/// thumbnails are listed off their screen's left edge).
fn left_of(displays: &[Display], r: Rect) -> Option<DisplayId> {
    let y = r.center().y;
    displays
        .iter()
        .filter(|d| y >= d.frame.y && y <= d.frame.max_y() && d.frame.x >= r.max_x() - SAME)
        .min_by(|a, b| a.frame.x.total_cmp(&b.frame.x))
        .map(|d| d.id)
}

/// The screen `r` overlaps most, when it overlaps one.
fn most_of(displays: &[Display], r: Rect) -> Option<DisplayId> {
    let area = |d: &Display| d.frame.intersect(&r).map_or(0.0, |i| i.w.max(0.0) * i.h.max(0.0));
    displays.iter().filter(|d| area(d) > 0.0).max_by(|a, b| area(a).total_cmp(&area(b))).map(|d| d.id)
}

/// Screen `d`'s strip out of WindowManager's `sm.space` readings (`spaces`: each with its
/// frame when it has one, in WindowManager's order), `displays` the screens (the main one
/// first): the reading whose frame is `d`'s, else, when no reading has a frame, the one at
/// `d`'s place among the screens.
pub fn reading_for(spaces: &[(Option<Rect>, Reading)], displays: &[Display], d: &Display) -> Option<Reading> {
    let same = |r: &Rect| (r.x - d.frame.x).abs() <= SAME && (r.y - d.frame.y).abs() <= SAME && (r.w - d.frame.w).abs() <= SAME && (r.h - d.frame.h).abs() <= SAME;
    if let Some((_, r)) = spaces.iter().find(|(f, _)| f.as_ref().is_some_and(same)) {
        return Some(r.clone());
    }
    if spaces.iter().any(|(f, _)| f.is_some()) {
        return None;
    }
    let at = displays.iter().position(|x| x.id == d.id)?;
    spaces.get(at).map(|(_, r)| r.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::apple::mirror::{Item, ListedWindow};
    use crate::engine::testkit::display;

    fn two() -> Vec<Display> {
        let main = display();
        let right = Display { id: DisplayId(2), frame: Rect::new(main.frame.max_x(), 0.0, 1920.0, 1080.0), visible: Rect::new(main.frame.max_x(), 25.0, 1920.0, 1055.0), ..main };
        vec![main, right]
    }

    fn w(id: u32, pid: i32, b: Rect) -> ListedWindow {
        ListedWindow { id, pid, layer: 0, bounds: b, onscreen: true }
    }

    #[test]
    fn each_window_goes_to_the_screen_its_middle_is_on() {
        let d = two();
        let x2 = d[1].frame.x;
        let listed = Listed {
            windows: vec![
                w(1, 1, Rect::new(300.0, 100.0, 600.0, 400.0)),
                w(2, 2, Rect::new(x2 + 300.0, 100.0, 600.0, 400.0)),
                // Screen 2's strip: a thumbnail and its twin, at its own left edge.
                w(9003, 99, Rect::new(x2 + 15.0, 300.0, 79.0, 104.0)),
                w(3, 3, Rect::new(x2 + 15.0, 300.0, 79.0, 104.0)),
            ],
            manager: Some(99),
        };
        let managed: BTreeSet<u32> = [1, 2, 3].into();
        let s = split(&listed, &managed, &d, &BTreeMap::new());
        assert_eq!(s[&d[0].id], [1].into());
        assert_eq!(s[&d[1].id], [2, 3].into());
    }

    #[test]
    fn a_hidden_strips_thumbnail_stays_with_its_screen() {
        let d = two();
        let x2 = d[1].frame.x;
        // Screen 2's strip hidden: its thumbnail listed off its left edge, on screen 1, no twin.
        let listed = Listed { windows: vec![w(3, 3, Rect::new(x2 - 258.0, 300.0, 79.0, 104.0))], manager: Some(99) };
        let managed: BTreeSet<u32> = [3].into();
        let before: BTreeMap<u32, DisplayId> = [(3, d[1].id)].into();
        assert_eq!(split(&listed, &managed, &d, &before)[&d[1].id], [3].into(), "the screen it was on");
        // Off every screen, never seen: the screen right of it.
        let off = Listed { windows: vec![w(4, 4, Rect::new(-258.0, 300.0, 79.0, 104.0))], manager: Some(99) };
        assert_eq!(split(&off, &[4].into(), &d, &BTreeMap::new())[&d[0].id], [4].into());
    }

    #[test]
    fn a_screens_strip_is_the_sm_space_of_its_frame_else_of_its_place() {
        let d = two();
        let r = |id: u32| Reading { items: vec![Item { frame: Rect::new(15.0, 300.0, 79.0, 104.0), windows: vec![id] }] };
        let framed = [(Some(d[1].frame), r(2)), (Some(d[0].frame), r(1))];
        assert_eq!(reading_for(&framed, &d, &d[0]), Some(r(1)), "by frame, whatever the order");
        assert_eq!(reading_for(&framed, &d, &d[1]), Some(r(2)));
        let bare = [(None, r(1)), (None, r(2))];
        assert_eq!(reading_for(&bare, &d, &d[1]), Some(r(2)), "no frames: by place, the main screen first");
        assert_eq!(reading_for(&[(Some(d[0].frame), r(1))], &d, &d[1]), None, "framed, none of this screen");
    }
}
