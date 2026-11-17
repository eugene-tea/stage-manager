//! The mirror: Apple's arrangement as read from the window server's list and checked
//! against WindowManager's Accessibility ([`fold`]).
//!
//! Sources, in order of trust (two probes, 11-16, macOS 27.2):
//!
//! 1. **The window list** ([`Listed`]), the fast mirror. Apple animates the real windows, so
//!    their listed bounds move every frame and are final when the motion ends. WindowManager
//!    keeps a layer-0 window of its own exactly over every strip thumbnail, listed right above
//!    it with the same bounds (its twin), and one per app icon (66x82 at x = -7): a strip
//!    window is a managed window with a twin ([`has_twin`]); a window of the stage in front
//!    has none. No column or size is asked (the stage in front sits at x = 221 on a laptop
//!    screen, live 11-16). An item is the thumbnails that overlap (a stage's windows are drawn
//!    stacked, 16-30 points apart), never the thumbnails nearest one icon: a stage of two
//!    apps has an icon for each, and was read as two stages (live 11-16).
//! 2. **WindowManager's Accessibility** ([`Reading`]: `sm.strip`'s items with `AXWindowsIDs`
//!    and `AXFrame`), the word on membership. It catches up about 785 ms after a change and
//!    reads empty for about 650 ms of it: an empty reading is no reading. A reading taken a
//!    second after the settle, with no change since, decides which strip windows make one
//!    stage until the next change ([`Arrangement::ax_groups`]); the list still gives the
//!    places, the twins, the motion and the stage in front. A list that groups otherwise is
//!    corrected, and the correction counted.
//!
//! Rules of [`fold`]:
//!
//! - **Current stage**: managed windows on screen with no twin, front to back.
//! - **Windows Apple draws no thumbnail of**: an item shows three; a stage's fourth window
//!   and on is listed off screen, thumbnail-sized, within the item (live 11-17: a stage of
//!   four, its fourth at 69x63 inside the item's thumbnails). It belongs to that item, at its
//!   back, with no point of its own ([`undrawn`]), never a stage of its own.
//! - **In motion**: any managed window's bounds differ from the read before, or a step of
//!   ours or an event that may change the arrangement (an activation, a window found or
//!   gone) came less than [`SETTLE`] ago ([`Arrangement::step_began`]): Apple's motion starts
//!   about 100 ms after its cause, and a read before it shows the stage coming in beside the
//!   one leaving. Groups are taken from reads out of motion only; in motion the groups read
//!   before stand.
//! - **Settled**: no motion for [`STILL_READS`] reads and at least [`SETTLE`] since the last
//!   change: what a posted step waits for (A3), and when the model follows.
//! - **Stages Apple no longer lists** (past the strip's room) stay as remembered groups with
//!   their last membership; a window gone from the list leaves its group, a window seen again
//!   leaves it for what is seen.
//! - **Other Desktops** ([`Arrangement::desktop_left`]): at a Desktop change every group known
//!   goes *away*, tagged with the change, since each Desktop has stages of its own and the
//!   window list cannot tell a stage of another Desktop from one past the strip's room (both
//!   are listed off screen). Away groups are kept, followed by the model and never acted on:
//!   no number counts them, no step focuses or clicks a window of theirs (a focus would show
//!   their Desktop). Once a window of a change's groups is seen again (that Desktop shows
//!   again), the rest of them are remembered groups of this Desktop once more.
//! - **Full screen** ([`Arrangement::full_screen`], set by the platform from Accessibility):
//!   a full-screen app's Space shows on this screen; no step runs there.
//! - **Every app window by its pid** ([`Arrangement::seen`]), managed or not, read in motion
//!   too: a thumbnail (a twin over it) or on screen at full size, how many reads in a row it
//!   has kept its bounds, and its thumbnail's point. They make no group and no motion. An app
//!   opened here is taken in by them, whatever its windows' ids or subroles (live 11-16: a
//!   running app reopened an id seen before, read as `AXDialog` while it was hidden), and
//!   without waiting for the whole arrangement to settle: its own thumbnail resting is enough.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::engine::geometry::{Point, Rect};
use crate::engine::model::Display;

use super::{points, thumbnail_sized};

/// How long a change keeps the arrangement unsettled: Apple's motion, about 700 ms
/// (a recording of 11-16: a focus at .305, motion from .404 to about .900; clicks before
/// ~700 ms are ignored or switch back).
pub const SETTLE: Duration = Duration::from_millis(700);

/// Reads in a row with no bounds changed before the arrangement counts settled.
pub const STILL_READS: u32 = 2;

/// How long the arrangement is settled before Accessibility's reading is trusted with the
/// strip's groups (it catches up about 785 ms after a change).
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
    /// Listed on screen and drawn (a window above the windows' layer that is fully clear
    /// counts off screen: it covers nothing, as the gestures' guard counts it).
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
    /// Not listed any more (past the strip's room): its last membership.
    Remembered,
    /// On a Desktop not shown (left at a Desktop change): its last membership, never acted
    /// on.
    Away,
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

/// A window listed in front of a thumbnail that shows no part of its own, or an app icon
/// over it ([`Arrangement::covered`]): what the log says when a press waits on one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Cover {
    /// The covering window (0 for an app icon).
    pub id: u32,
    /// Its process.
    pub pid: i32,
    /// WindowManager's own.
    pub manager: bool,
    /// Its layer.
    pub layer: i64,
    /// Its bounds.
    pub bounds: Rect,
}

/// An app's window as the window list shows it at one read ([`Arrangement::seen`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seen {
    /// Its process.
    pub pid: i32,
    /// Its listed bounds.
    pub bounds: Rect,
    /// Reads in a row before this one at the same bounds (0: new or moved). Apple's twin
    /// comes once the motion ends; a window flying into the strip has none and is not in
    /// front either.
    pub still: u32,
    /// A strip thumbnail (a WindowManager twin over it, or listed off the left edge).
    pub thumb: bool,
    /// On screen at full size: no twin, larger than a thumbnail. A thumbnail-sized window with
    /// no twin is neither (in Apple's motion into the strip, its twin not come yet).
    pub full: bool,
    /// Where its thumbnail shows a part of its own ([`Arrangement::click_at`]'s rule).
    pub click_at: Option<Point>,
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
    /// Where each strip window's thumbnail shows a part of its own, on screen, at this read
    /// (none in motion): the point a Shift-click on it presses ([`points::visible_point`]).
    pub click_at: BTreeMap<u32, Point>,
    /// Where each window of the stage in front shows a part of its title bar of its own, at
    /// this read (none in motion): the point a drag grabs ([`points::title_free`]); a window
    /// missing here is covered all along its title bar and is raised before the drag.
    pub title_at: BTreeMap<u32, Point>,
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
    /// Times the list's grouping of the strip was corrected by Accessibility's (each spell
    /// once).
    pub corrections: u64,
    /// The list's grouping differs from Accessibility's now (corrected).
    pub disagree: bool,
    /// The strip's stages as the last trusted Accessibility reading had them (managed windows
    /// only), and when it was read: they decide the strip's groups until the next change.
    pub ax_groups: Vec<Vec<u32>>,
    /// When [`Arrangement::ax_groups`] was read.
    pub ax_groups_at: Option<Instant>,
    /// The groups of Desktops not shown, each with the Desktop change that took it away
    /// ([`Arrangement::desktop_left`]).
    pub away: Vec<(u64, Group)>,
    /// Desktop changes seen ([`Arrangement::desktop_left`]).
    pub desktop_changes: u64,
    /// A full-screen app's Space shows on this screen (the platform's reading): no step runs.
    pub full_screen: bool,
    /// Every app window on this screen, managed or not, front to back, read at every read (in
    /// motion too): what "open here" takes an app's windows in by ([`Seen`]).
    pub seen: Vec<(u32, Seen)>,
    /// What covers each thumbnail on screen that shows no part of its own at this read (a
    /// strip window's out of motion, an app window's by its pid at every read): said in the
    /// log when a press waits on one, so a live run tells what was over it.
    pub covered: BTreeMap<u32, Vec<Cover>>,
}

impl Arrangement {
    /// App `pid`'s windows at this read, front to back.
    pub fn seen_of(&self, pid: i32) -> impl Iterator<Item = (u32, Seen)> + '_ {
        self.seen.iter().filter(move |(_, s)| s.pid == pid).copied()
    }

    /// Where window `id` is: its stage's place and, in the strip, its slot.
    pub fn where_is(&self, id: u32) -> Option<(Where, Option<usize>)> {
        if self.current.contains(&id) {
            return Some((Where::Current, None));
        }
        if let Some(slot) = self.strip.iter().position(|g| g.windows.contains(&id)) {
            return Some((Where::Strip, Some(slot)));
        }
        if self.remembered.iter().any(|g| g.windows.contains(&id)) {
            return Some((Where::Remembered, None));
        }
        self.away.iter().any(|(_, g)| g.windows.contains(&id)).then_some((Where::Away, None))
    }

    /// True when window `id` is in a group of a Desktop not shown.
    pub fn is_away(&self, id: u32) -> bool {
        matches!(self.where_is(id), Some((Where::Away, _)))
    }

    /// The Desktop shown is left (a Desktop change, a full-screen app's Space coming or
    /// going): every group known goes away with this change, and the arrangement counts in
    /// motion for [`SETTLE`]. The next reads out of motion find the groups of the Desktop
    /// shown afresh; see the module docs.
    pub fn desktop_left(&mut self, now: Instant) {
        self.desktop_changes += 1;
        let batch = self.desktop_changes;
        let current = (!self.current.is_empty()).then(|| Group { windows: std::mem::take(&mut self.current), frame: Rect::new(0.0, 0.0, 0.0, 0.0) });
        let left: Vec<Group> = current.into_iter().chain(std::mem::take(&mut self.strip)).chain(std::mem::take(&mut self.remembered)).collect();
        self.away.extend(left.into_iter().map(|g| (batch, g)));
        self.click_at.clear();
        self.title_at.clear();
        self.ax_groups.clear();
        self.ax_groups_at = None;
        self.strip_shown = None;
        self.step_began(now);
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
        out.extend(self.away.iter().map(|(_, g)| (Where::Away, g.windows.as_slice())));
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
        let mut rest: Vec<Vec<u32>> = self.strip.iter().chain(&self.remembered).chain(self.away.iter().map(|(_, g)| g)).map(|g| sorted(&g.windows)).collect();
        rest.sort();
        (sorted(&self.current), rest)
    }

    /// A step of ours began at `now` (a focus, a posted click), or an event came that may
    /// change the arrangement (an activation, a window found or gone): the arrangement counts
    /// in motion for [`SETTLE`], whatever the list shows meanwhile.
    pub fn step_began(&mut self, now: Instant) {
        self.step_at = Some(now);
        self.settled = false;
    }

    /// At rest: out of motion (nothing begun less than [`SETTLE`] ago, no bounds changed since
    /// the read before) for [`STILL_READS`] reads, the [`SETTLE`] after the last frame of
    /// motion not waited (that is for a click's point to be final; a focus aims at nothing on
    /// screen).
    pub fn at_rest(&self) -> bool {
        !self.in_motion && self.still_reads >= STILL_READS
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

/// True when the window listed at `i` has a twin: the window listed right above it is one of
/// WindowManager's layer-0 windows with the same bounds (within a point). Every strip
/// thumbnail has one; a window of the stage in front never does (live 11-16).
pub fn has_twin(listed: &Listed, i: usize) -> bool {
    let Some(w) = listed.windows.get(i) else { return false };
    i.checked_sub(1).and_then(|j| listed.windows.get(j)).is_some_and(|above| Some(above.pid) == listed.manager && above.layer == 0 && same(above.bounds, w.bounds))
}

/// True when `r` is thumbnail-sized and wholly left of the screen: the strip hidden, its
/// thumbnails listed off the left edge (x = -258 measured), with or without their twins.
fn off_left(screen: &Display, r: Rect) -> bool {
    thumbnail_sized(r) && r.max_x() <= screen.frame.x
}

/// The next arrangement from the one before (`prev`), a reading of Accessibility (`reading`,
/// `None` when not read this time), the window list (`listed`) and the windows the app
/// manages (`managed`), on screen `screen`, at `now`. See the module docs for the rules.
pub fn fold(prev: &Arrangement, reading: Option<&Reading>, listed: &Listed, managed: &BTreeSet<u32>, screen: &Display, now: Instant) -> Arrangement {
    let icons: Vec<Rect> = listed.manager_windows().map(|w| w.bounds).filter(|b| icon_sized(*b)).collect();

    let mut frames = BTreeMap::new();
    let mut current = Vec::new();
    let mut thumbs: Vec<(u32, Rect)> = Vec::new();
    let mut thumb_at: Vec<(u32, usize)> = Vec::new();
    let mut current_at: Vec<(u32, usize)> = Vec::new();
    let mut hidden_thumbs: Vec<(u32, Rect)> = Vec::new();
    for (i, w) in listed.windows.iter().enumerate() {
        if !managed.contains(&w.id) || frames.insert(w.id, w.bounds).is_some() {
            continue;
        }
        let on_screen = w.bounds.intersect(&screen.frame).is_some_and(|r| r.w > 0.0 && r.h > 0.0);
        if has_twin(listed, i) || off_left(screen, w.bounds) {
            thumbs.push((w.id, w.bounds));
            thumb_at.push((w.id, i));
        } else if w.onscreen && !w.bounds.is_degenerate() && on_screen {
            current.push(w.id);
            current_at.push((w.id, i));
        } else if !w.onscreen && thumbnail_sized(w.bounds) && on_screen {
            hidden_thumbs.push((w.id, w.bounds));
        }
    }

    // Every app window by its pid, managed or not, in motion too (module docs).
    let mut seen_at: Vec<(u32, usize, bool)> = Vec::new();
    for (i, w) in listed.windows.iter().enumerate() {
        if w.layer != 0 || Some(w.pid) == listed.manager || seen_at.iter().any(|(id, _, _)| *id == w.id) {
            continue;
        }
        let on = w.onscreen && w.bounds.intersect(&screen.frame).is_some_and(|r| r.w > 0.0 && r.h > 0.0);
        if (has_twin(listed, i) && on) || off_left(screen, w.bounds) {
            seen_at.push((w.id, i, true));
        } else if on && !w.bounds.is_degenerate() {
            seen_at.push((w.id, i, false));
        }
    }
    let seen_thumbs: Vec<(u32, usize)> = seen_at.iter().filter(|(_, _, t)| *t).map(|(id, i, _)| (*id, *i)).collect();
    let (seen_points, seen_covered) = click_points(listed, &seen_thumbs, &icons, screen);
    let seen: Vec<(u32, Seen)> = seen_at
        .iter()
        .filter_map(|&(id, i, thumb)| {
            let w = listed.windows.get(i)?;
            let still = prev.seen.iter().find(|(p, s)| *p == id && s.thumb == thumb && same(s.bounds, w.bounds)).map_or(0, |(_, s)| s.still.saturating_add(1));
            let full = !thumb && !thumbnail_sized(w.bounds);
            Some((id, Seen { pid: w.pid, bounds: w.bounds, still, thumb, full, click_at: seen_points.get(&id).copied() }))
        })
        .collect();

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
        corrections: prev.corrections,
        disagree: prev.disagree,
        ax_groups: prev.ax_groups.clone(),
        ax_groups_at: prev.ax_groups_at,
        full: prev.full.clone(),
        desktop_changes: prev.desktop_changes,
        full_screen: prev.full_screen,
        seen,
        covered: seen_covered,
        ..Arrangement::default()
    };
    // Closed windows leave what is kept of them.
    next.full.retain(|id, _| listed.has(*id) && managed.contains(id));

    if in_motion {
        // Mid-motion bounds say nothing of the groups, nor does a read in the moment between
        // a cause and Apple's motion (an activation: the stage coming in is listed full size
        // beside the one leaving, live 11-16): the groups read before stand, less the windows
        // closed meanwhile.
        next.current = prev.current.iter().copied().filter(|id| listed.has(*id) && managed.contains(id)).collect();
        next.strip = kept(&prev.strip, listed, managed);
        next.remembered = kept(&prev.remembered, listed, managed);
        next.away = kept_away(&prev.away, listed, managed, &BTreeSet::new());
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
            false => Some(thumbs.iter().any(|(_, b)| b.max_x() > screen.frame.x)),
        };
        next.strip = items(&thumbs, &icons);
        undrawn(&mut next.strip, &hidden_thumbs);
        let (points, covered) = click_points(listed, &thumb_at, &icons, screen);
        next.click_at = points;
        next.covered.extend(covered);
        next.title_at = title_points(listed, &current_at, screen);
        next.current = current;
        // A Desktop shown again: a window of a change's away groups seen now brings the rest of
        // them back as remembered groups of this Desktop (past the strip's room).
        let seen: BTreeSet<u32> = next.current.iter().chain(next.strip.iter().flat_map(|g| g.windows.iter())).copied().collect();
        let back: BTreeSet<u64> = prev.away.iter().filter(|(_, g)| g.windows.iter().any(|w| seen.contains(w))).map(|(b, _)| *b).collect();
        let mut before = prev.clone();
        before.remembered.extend(prev.away.iter().filter(|(b, _)| back.contains(b)).map(|(_, g)| g.clone()));
        next.remembered = remembered(&before, &next, listed, managed);
        next.away = kept_away(&prev.away, listed, managed, &back);
        next.away.retain(|(_, g)| !g.windows.is_empty());
        for (_, g) in &mut next.away {
            g.windows.retain(|w| !seen.contains(w));
        }
        next.away.retain(|(_, g)| !g.windows.is_empty());
    }

    // Accessibility on membership: a blank reading is no reading; a reading a second after
    // the settle is trusted until the next change.
    let last_change = changed_at.max(next.step_at);
    if let Some(r) = reading.filter(|r| !r.is_blank()) {
        next.ax_at = Some(now);
        let trusted = next.settled && last_change.is_none_or(|t| now.saturating_duration_since(t) >= SETTLE + AX_TRUST);
        if trusted {
            next.ax_groups = r.items.iter().map(|i| i.windows.iter().copied().filter(|id| managed.contains(id)).collect::<Vec<u32>>()).filter(|g| !g.is_empty()).collect();
            next.ax_groups_at = Some(now);
        }
    }
    let ax_fresh = next.ax_groups_at.is_some_and(|at| last_change.is_none_or(|t| at > t));
    if !in_motion && ax_fresh {
        let corrected = by_ax(&next.strip, &next.ax_groups);
        if corrected != next.strip {
            if !next.disagree {
                next.corrections += 1;
            }
            next.disagree = true;
            next.strip = corrected;
        } else {
            next.disagree = false;
        }
    }
    next
}

/// The list's strip items (`strip`) regrouped as Accessibility's `ax` has the stages: each
/// Accessibility stage's windows that the list shows in the strip make one item, in the list's
/// order, its frame the union of theirs; a strip window Accessibility has in no stage keeps the
/// list's item. Top to bottom.
fn by_ax(strip: &[Group], ax: &[Vec<u32>]) -> Vec<Group> {
    let order: Vec<u32> = strip.iter().flat_map(|g| g.windows.iter().copied()).collect();
    let frame_of = |id: u32| strip.iter().find(|g| g.windows.contains(&id)).map(|g| g.frame);
    let mut out: Vec<Group> = Vec::new();
    let mut placed: BTreeSet<u32> = BTreeSet::new();
    for item in ax {
        let windows: Vec<u32> = order.iter().copied().filter(|id| item.contains(id)).collect();
        let Some(first) = windows.first().and_then(|id| frame_of(*id)) else { continue };
        let frame = windows.iter().filter_map(|id| frame_of(*id)).fold(first, |u, f| u.union(&f));
        placed.extend(windows.iter().copied());
        out.push(Group { windows, frame });
    }
    for g in strip {
        let windows: Vec<u32> = g.windows.iter().copied().filter(|id| !placed.contains(id)).collect();
        if !windows.is_empty() {
            out.push(Group { windows, frame: g.frame });
        }
    }
    out.sort_by(|a, b| a.frame.y.total_cmp(&b.frame.y));
    out
}

/// Where each strip window (`thumbs`: its id and its place in `listed`) shows a part of its
/// own: what covers it is every window listed in front of it on screen but its own twin
/// (the click target, listed right above it with its bounds), and every app icon. With the
/// points, what covers each one that shows none ([`Cover`]).
fn click_points(listed: &Listed, thumbs: &[(u32, usize)], icons: &[Rect], screen: &Display) -> (BTreeMap<u32, Point>, BTreeMap<u32, Vec<Cover>>) {
    let mut out = BTreeMap::new();
    let mut covered = BTreeMap::new();
    for &(id, i) in thumbs {
        let Some(w) = listed.windows.get(i) else { continue };
        if !w.onscreen {
            continue;
        }
        let twin = has_twin(listed, i).then(|| i - 1);
        let in_front: Vec<&ListedWindow> = listed.windows[..i].iter().enumerate().filter(|(j, o)| o.onscreen && Some(*j) != twin).map(|(_, o)| o).collect();
        let mut over: Vec<Rect> = in_front.iter().map(|o| o.bounds).collect();
        over.extend(icons.iter().copied());
        match points::visible_point(w.bounds, &over, screen.frame) {
            Some(p) => {
                out.insert(id, p);
            }
            None => {
                let meets = |r: &Rect| r.intersect(&w.bounds).is_some_and(|x| x.w > 0.0 && x.h > 0.0);
                let mut by: Vec<Cover> = in_front
                    .iter()
                    .filter(|o| meets(&o.bounds))
                    .map(|o| Cover { id: o.id, pid: o.pid, manager: Some(o.pid) == listed.manager, layer: o.layer, bounds: o.bounds })
                    .collect();
                by.extend(icons.iter().filter(|r| meets(r)).map(|r| Cover { id: 0, pid: listed.manager.unwrap_or(0), manager: true, layer: 0, bounds: *r }));
                covered.insert(id, by);
            }
        }
    }
    (out, covered)
}

/// Windows Apple draws no thumbnail of (`hidden`: managed, listed off screen, thumbnail-sized,
/// on this screen) put in the strip item whose thumbnails they lie within, at its back (module
/// docs). One within no item is left out (a stage past the strip's room, or a window shown
/// nowhere).
fn undrawn(strip: &mut [Group], hidden: &[(u32, Rect)]) {
    for (id, b) in hidden {
        if let Some(g) = strip.iter_mut().filter(|g| overlap(&g.frame, b) > 0.0).max_by(|x, y| overlap(&x.frame, b).total_cmp(&overlap(&y.frame, b))) {
            g.windows.push(*id);
        }
    }
}

/// Where each window of the stage in front (`current`: its id and its place in `listed`)
/// shows a part of its title bar of its own: what covers it is every window listed in front
/// of it on screen, at the windows' layer or above (as the drag's guard counts them).
fn title_points(listed: &Listed, current: &[(u32, usize)], screen: &Display) -> BTreeMap<u32, Point> {
    let mut out = BTreeMap::new();
    for &(id, i) in current {
        let Some(w) = listed.windows.get(i) else { continue };
        let over: Vec<Rect> = listed.windows[..i].iter().filter(|o| o.onscreen && o.layer >= 0).map(|o| o.bounds).collect();
        if let Some(p) = points::title_free(w.bounds, &over, screen.frame) {
            out.insert(id, p);
        }
    }
    out
}

/// `groups` less the windows closed (or no longer managed); groups left empty go.
fn kept(groups: &[Group], listed: &Listed, managed: &BTreeSet<u32>) -> Vec<Group> {
    groups
        .iter()
        .map(|g| Group { windows: g.windows.iter().copied().filter(|id| listed.has(*id) && managed.contains(id)).collect(), frame: g.frame })
        .filter(|g| !g.windows.is_empty())
        .collect()
}

/// `away` less the windows closed (or no longer managed) and the changes in `back` (their
/// Desktop shows again); groups left empty go.
fn kept_away(away: &[(u64, Group)], listed: &Listed, managed: &BTreeSet<u32>, back: &BTreeSet<u64>) -> Vec<(u64, Group)> {
    away.iter()
        .filter(|(b, _)| !back.contains(b))
        .map(|(b, g)| (*b, Group { windows: g.windows.iter().copied().filter(|id| listed.has(*id) && managed.contains(id)).collect(), frame: g.frame }))
        .filter(|(_, g)| !g.windows.is_empty())
        .collect()
}

/// The strip's items from its thumbnails (`thumbs`, front to back) and WindowManager's icon
/// windows (`icons`): thumbnails that overlap, directly or through another, make one item (a
/// stage's windows are drawn stacked; a stage of two apps has two icons, so the icon nearest a
/// thumbnail says nothing of its stage: live 11-16); each item's frame takes in the icons that
/// overlap its thumbnails most. Top to bottom.
fn items(thumbs: &[(u32, Rect)], icons: &[Rect]) -> Vec<Group> {
    let mut groups: Vec<(Group, Rect)> = Vec::new();
    for (id, b) in thumbs {
        let joined: Vec<usize> = groups.iter().enumerate().filter(|(_, (_, t))| overlap(t, b) > 0.0).map(|(i, _)| i).collect();
        match joined.as_slice() {
            [] => groups.push((Group { windows: vec![*id], frame: *b }, *b)),
            [first, rest @ ..] => {
                let first = *first;
                for &i in rest.iter().rev() {
                    let (g, t) = groups.remove(i);
                    groups[first].0.windows.extend(g.windows);
                    groups[first].1 = groups[first].1.union(&t);
                }
                groups[first].0.windows.push(*id);
                groups[first].1 = groups[first].1.union(b);
            }
        }
    }
    let mut out: Vec<Group> = groups.into_iter().map(|(mut g, t)| {
        g.frame = t;
        g
    }).collect();
    for icon in icons {
        let best = out.iter().enumerate().map(|(i, g)| (i, overlap(&g.frame, icon))).filter(|(_, a)| *a > 0.0).max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = best {
            out[i].frame = out[i].frame.union(icon);
        }
    }
    out.sort_by(|a, b| a.frame.y.total_cmp(&b.frame.y));
    out
}

/// The area `a` and `b` share.
fn overlap(a: &Rect, b: &Rect) -> f64 {
    a.intersect(b).map_or(0.0, |r| r.w.max(0.0) * r.h.max(0.0))
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
    fn the_blank_accessibility_read_is_no_reading_and_a_trusted_one_corrects_the_groups_once() {
        let (mut f, a) = start();
        f.focus_window(3);
        f.advance(100);
        assert!(f.reading().is_blank(), "the fake rebuilds the strip for 650 ms");
        let b = fold(&a, Some(&f.reading()), &f.listed(), &managed(&f), &display(), f.now());
        assert_eq!(b.corrections, 0);
        let b = settle(&mut f, b);
        f.advance(1100);
        let b = fold(&b, Some(&f.reading()), &f.listed(), &managed(&f), &display(), f.now());
        assert_eq!(b.corrections, 0, "they agree");
        assert!(b.ax_at == Some(f.now()) && b.ax_groups_at == Some(f.now()), "trusted a second after the settle");
        // Accessibility groups the strip otherwise: its groups taken, one correction for the
        // spell, kept on the reads without Accessibility until the next change.
        let mut wrong = f.reading();
        let all: Vec<u32> = wrong.items.iter().flat_map(|i| i.windows.clone()).collect();
        wrong.items = vec![Item { frame: Rect::new(0.0, 0.0, 1.0, 1.0), windows: all.clone() }];
        f.advance(50);
        let c = fold(&b, Some(&wrong), &f.listed(), &managed(&f), &display(), f.now());
        f.advance(50);
        let c = fold(&c, None, &f.listed(), &managed(&f), &display(), f.now());
        assert_eq!(c.corrections, 1);
        assert!(c.disagree);
        let mut one = c.strip.iter().flat_map(|g| g.windows.clone()).collect::<Vec<_>>();
        one.sort_unstable();
        let mut want = all;
        want.sort_unstable();
        assert_eq!((c.strip.len(), one), (1, want), "Accessibility's groups");
        assert_eq!(c.current, b.current, "the stage in front is the list's");
        f.advance(50);
        let d = fold(&c, Some(&f.reading()), &f.listed(), &managed(&f), &display(), f.now());
        assert!(!d.disagree && d.corrections == 1);
        assert_eq!(d.strip, b.strip);
        // A change: the trusted reading no longer decides.
        let mut e = d.clone();
        e.step_began(f.now());
        e.ax_groups = vec![all_ids(&d)];
        f.advance(800);
        let e = fold(&e, None, &f.listed(), &managed(&f), &display(), f.now());
        assert_eq!(e.strip, b.strip, "a reading from before a change is not taken");
    }

    fn all_ids(a: &Arrangement) -> Vec<u32> {
        a.strip.iter().flat_map(|g| g.windows.clone()).collect()
    }

    /// Live 11-16 (bb58705): Charlie sent to Bravo's stage, then Alpha's in front: the strip's
    /// one item of two apps, its thumbnails overlapping, was read as two stages (each thumbnail
    /// nearest an icon of its own app), and the list won against Accessibility's one item.
    #[test]
    fn a_stage_of_two_apps_in_the_strip_reads_as_one_by_its_overlapping_thumbnails() {
        let screen = Display { frame: Rect::new(0.0, 0.0, 1800.0, 1169.0), visible: Rect::new(0.0, 39.0, 1800.0, 1130.0), ..display() };
        let managed: BTreeSet<u32> = [78904, 78903, 78892, 78886].into();
        let w = |id: u32, pid: i32, x: f64, y: f64, ww: f64, h: f64| ListedWindow { id, pid, layer: 0, bounds: Rect::new(x, y, ww, h), onscreen: true };
        let list = Listed {
            windows: vec![
                w(78904, 1, 360.0, 107.0, 520.0, 424.0),
                w(78903, 1, 300.0, 47.0, 520.0, 424.0),
                w(9001, 99, 44.0, 258.0, 68.0, 94.0),
                w(78892, 3, 44.0, 258.0, 68.0, 94.0),
                w(9002, 99, 16.0, 243.0, 69.0, 96.0),
                w(78886, 2, 16.0, 243.0, 69.0, 96.0),
                // An icon for each app of the item, each nearest one thumbnail.
                w(9003, 99, -7.0, 250.0, 66.0, 82.0),
                w(9004, 99, 46.0, 275.0, 66.0, 82.0),
            ],
            manager: Some(99),
        };
        let t0 = Instant::now();
        let mut a = fold(&Arrangement::default(), None, &list, &managed, &screen, t0);
        for ms in [800, 850, 2000] {
            a = fold(&a, None, &list, &managed, &screen, t0 + Duration::from_millis(ms));
        }
        assert_eq!(a.current, vec![78904, 78903]);
        assert_eq!(a.strip.iter().map(|g| g.windows.clone()).collect::<Vec<_>>(), vec![vec![78892, 78886]], "one stage");
        let r = Reading { items: vec![Item { frame: Rect::new(-7.0, 243.0, 120.0, 114.0), windows: vec![78892, 78886] }] };
        let b = fold(&a, Some(&r), &list, &managed, &screen, t0 + Duration::from_millis(2100));
        assert!(!b.disagree && b.corrections == 0, "the list and Accessibility agree");
        // Had the list split it, Accessibility's item puts it together again.
        let split = Arrangement { strip: vec![Group { windows: vec![78886], frame: Rect::new(16.0, 243.0, 69.0, 96.0) }, Group { windows: vec![78892], frame: Rect::new(44.0, 258.0, 68.0, 94.0) }], ..b.clone() };
        assert_eq!(by_ax(&split.strip, &[vec![78892, 78886]]).iter().map(|g| g.windows.clone()).collect::<Vec<_>>(), vec![vec![78886, 78892]]);
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

    /// The live capture of 11-16 (a 1512x982 pt laptop screen; three fixture apps among the
    /// owner's own, whose strip items are WindowManager windows with no managed window under
    /// them): the window list front to back, layer 0, as `owner x y w h`; `WM` is
    /// WindowManager. Each WindowManager stand-in is listed right above its thumbnail, with
    /// the same bounds; app icons are 66x82 at x = -7.
    const CAPTURE: [&str; 3] = [
        "charlie 221 87 520 392|WM -7 312 66 82|WM 15 220 119 156|WM -7 502 66 82|WM 32 385 149 165|WM 15 385 152 165|WM -7 628 66 82|WM 15 575 87 97|bravo 15 575 87 97|WM -7 818 66 82|WM 15 688 162 174|WM -7 944 66 82|WM 32 872 85 116|alpha 32 872 85 116|WM 15 872 87 116|alpha 15 872 87 116",
        "alpha 221 157 560 422|alpha 221 87 520 392|WM -7 312 66 82|WM 15 220 119 156|WM -7 502 66 82|WM 32 385 149 165|WM 15 385 152 165|WM -7 628 66 82|WM 15 575 87 97|bravo 15 575 87 97|WM -7 818 66 82|WM 15 688 162 174|WM -7 944 66 82|WM 15 872 87 116|charlie 15 872 87 116",
        "bravo 221 87 520 392|WM -7 312 66 82|WM 15 220 119 156|WM -7 502 66 82|WM 32 385 149 165|WM 15 385 152 165|WM -7 628 66 82|WM 32 575 85 97|alpha 32 575 85 97|WM 15 575 87 97|alpha 15 575 87 97|WM -7 818 66 82|WM 15 688 162 174|WM -7 944 66 82|WM 15 872 87 116|charlie 15 872 87 116",
    ];

    /// The capture's rows as a window list: alpha's two windows 73 (listed first) and 74,
    /// bravo's 80, charlie's 86; WindowManager's windows numbered from 1000.
    fn captured(rows: &str) -> Listed {
        let mut alpha = [73, 74].into_iter();
        let mut wm = 1000..;
        let windows = rows
            .split('|')
            .map(|row| {
                let f: Vec<&str> = row.split(' ').collect();
                let n = |i: usize| f[i].parse::<f64>().expect("a number");
                let (id, pid) = match f[0] {
                    "WM" => (wm.next().expect("ids"), 99),
                    "alpha" => (alpha.next().expect("two alpha windows"), 1),
                    "bravo" => (80, 2),
                    _ => (86, 3),
                };
                ListedWindow { id, pid, layer: 0, bounds: Rect::new(n(1), n(2), n(3), n(4)), onscreen: true }
            })
            .collect();
        Listed { windows, manager: Some(99) }
    }

    #[test]
    fn the_live_capture_of_three_activations_reads_as_apple_showed_it() {
        let screen = Display { frame: Rect::new(0.0, 0.0, 1512.0, 982.0), visible: Rect::new(0.0, 33.0, 1512.0, 949.0), ..display() };
        let managed: BTreeSet<u32> = [73, 74, 80, 86].into();
        let sorted = |a: &Arrangement| -> (Vec<u32>, Vec<Vec<u32>>) {
            let mut c = a.current.clone();
            c.sort_unstable();
            (c, a.strip.iter().map(|g| { let mut w = g.windows.clone(); w.sort_unstable(); w }).collect())
        };
        let want = [
            (vec![86], vec![vec![80], vec![73, 74]]),
            (vec![73, 74], vec![vec![80], vec![86]]),
            (vec![80], vec![vec![73, 74], vec![86]]),
        ];
        let t0 = Instant::now();
        let mut a = Arrangement::default();
        for (k, (rows, want)) in CAPTURE.iter().zip(want).enumerate() {
            let at = t0 + Duration::from_secs(3 * k as u64);
            let list = captured(rows);
            // Read twice: the first after a change is in motion; the second, still, is read.
            a = fold(&a, None, &list, &managed, &screen, at);
            a = fold(&a, None, &list, &managed, &screen, at + Duration::from_millis(800));
            a = fold(&a, None, &list, &managed, &screen, at + Duration::from_millis(850));
            assert_eq!(sorted(&a), want, "capture {k}: the stage in front and the strip, top to bottom");
            assert!(a.settled, "capture {k}");
            assert_eq!(a.strip_shown, Some(true));
            assert!(a.remembered.is_empty(), "capture {k}: nothing past the room: {:?}", a.remembered);
        }
        // The capture's Accessibility after bravo's activation: the same two items.
        let r = Reading { items: vec![Item { frame: Rect::new(15.0, 575.0, 87.0, 97.0), windows: vec![74, 73] }, Item { frame: Rect::new(15.0, 872.0, 87.0, 116.0), windows: vec![86] }] };
        let b = fold(&a, Some(&r), &captured(CAPTURE[2]), &managed, &screen, t0 + Duration::from_secs(9));
        assert!(!b.disagree && b.corrections == 0, "the list and Accessibility agree");
    }

    /// Live 11-16: a read in the moment after an activation, before Apple's motion starts,
    /// lists the stage coming in at full size beside the one leaving; the model followed
    /// that read and put both in one stage. An event marks the arrangement in motion for
    /// [`SETTLE`]: the groups read before stand until Apple's has moved.
    #[test]
    fn a_read_right_after_an_activation_keeps_the_groups_until_apples_motion_is_over() {
        let screen = Display { frame: Rect::new(0.0, 0.0, 1512.0, 982.0), visible: Rect::new(0.0, 33.0, 1512.0, 949.0), ..display() };
        let managed: BTreeSet<u32> = [73, 74, 80, 86].into();
        let t0 = Instant::now();
        let list = captured(CAPTURE[1]);
        let mut a = fold(&Arrangement::default(), None, &list, &managed, &screen, t0);
        a = fold(&a, None, &list, &managed, &screen, t0 + Duration::from_millis(800));
        a = fold(&a, None, &list, &managed, &screen, t0 + Duration::from_millis(850));
        assert!(a.settled);
        // Bravo's activation: bravo listed full size, alpha not moved yet.
        let at = t0 + Duration::from_secs(2);
        a.step_began(at);
        let mixed = Listed {
            windows: [ListedWindow { id: 80, pid: 2, layer: 0, bounds: Rect::new(221.0, 87.0, 520.0, 392.0), onscreen: true }]
                .into_iter()
                .chain(list.windows.iter().filter(|w| w.id != 80).cloned())
                .collect(),
            manager: Some(99),
        };
        // Several reads within that moment (one per notice: the activation, the app's own,
        // a focus) list the same: still reads, yet no settled one.
        let mut b = a.clone();
        for ms in [10, 20, 30, 40] {
            b = fold(&b, None, &mixed, &managed, &screen, at + Duration::from_millis(ms));
        }
        assert!(b.in_motion && !b.settled, "an activation's moment is no settled read");
        let mut front = b.current.clone();
        front.sort_unstable();
        assert_eq!(front, vec![73, 74], "the groups read before stand");
    }

    /// Live 11-16 (bf510a8): the stage in front sits at x = 221 on the laptop screen, where
    /// a strip column took it for strip content and the mirror read no stage in front. Only a
    /// twin listed right above makes a strip window: a WindowManager window of the same bounds
    /// elsewhere in the list (another app's item, a stand-in below) does not, nor does the
    /// place or the size.
    #[test]
    fn only_a_twin_right_above_makes_a_strip_window_never_a_place_on_screen() {
        let laptop = Display { frame: Rect::new(0.0, 0.0, 1512.0, 982.0), visible: Rect::new(0.0, 33.0, 1512.0, 949.0), ..display() };
        let wide = Display { frame: Rect::new(0.0, 0.0, 1800.0, 1169.0), visible: Rect::new(0.0, 39.0, 1800.0, 1130.0), ..display() };
        let managed: BTreeSet<u32> = [73, 74, 80, 86].into();
        let t0 = Instant::now();
        for screen in [laptop, wide] {
            let a = fold(&Arrangement::default(), None, &captured(CAPTURE[1]), &managed, &screen, t0);
            let mut front = a.current.clone();
            front.sort_unstable();
            assert_eq!(front, vec![73, 74], "alpha at x = 221 is in front on a {} pt screen", screen.frame.w);
        }
        let w = |id: u32, pid: i32, x: f64, y: f64, ww: f64, h: f64| ListedWindow { id, pid, layer: 0, bounds: Rect::new(x, y, ww, h), onscreen: true };
        let list = Listed {
            windows: vec![
                w(80, 2, 80.0, 87.0, 520.0, 392.0),
                // A WindowManager window of the same bounds, but below it: no twin.
                w(1000, 99, 80.0, 87.0, 520.0, 392.0),
                // A small window of the stage in front, inside where thumbnails sit.
                w(86, 3, 20.0, 300.0, 150.0, 120.0),
                w(1001, 99, -7.0, 600.0, 66.0, 82.0),
                w(1002, 99, 15.0, 575.0, 87.0, 97.0),
                w(73, 1, 15.0, 575.0, 87.0, 97.0),
            ],
            manager: Some(99),
        };
        let a = fold(&Arrangement::default(), None, &list, &managed, &laptop, t0);
        assert_eq!(a.current, vec![80, 86], "no twin right above: in front, wherever and however small");
        assert_eq!(a.strip.iter().map(|g| g.windows.clone()).collect::<Vec<_>>(), vec![vec![73]]);
        assert!(has_twin(&list, 5) && !has_twin(&list, 0) && !has_twin(&list, 2));
    }

    #[test]
    fn with_no_icon_window_overlapping_thumbnails_make_one_item() {
        let t = |id, y| (id, Rect::new(15.0, y, 79.0, 104.0));
        let g = items(&[t(1, 300.0), t(2, 306.0), t(3, 500.0)], &[]);
        assert_eq!(g.iter().map(|g| g.windows.clone()).collect::<Vec<_>>(), vec![vec![1, 2], vec![3]]);
    }
}
