//! The guardian's watch, decided without touching anything: which windows it still waits for
//! after its recovery scan, and when each one may be let go.
//!
//! The guardian waits for the windows the crashed app had registered ([`Registered`]: parked,
//! or fitted clear of the strip) and for any allowed app's window it finds in the park zone,
//! on any Space. A waiting window is let go only once the guardian has **seen it back** or
//! seen it **gone** ([`Release`]):
//!
//! - read through Accessibility at a frame outside the park zone (the app's own frame; the
//!   platform reads it, after moving the window back when it had to);
//! - or listed by the window server **on screen** outside the park zone in settled passes at
//!   least [`SEEN_APART`] apart, with no other sighting in between;
//! - or not listed at all (on any Space: closed, or its app quit) in every pass for
//!   [`GONE_AFTER`].
//!
//! A pass whose listing shows no window on screen at all (the window server answered nothing,
//! or the session is not the console's: fast user switching, the login window, a sleeping
//! display) is **blind**: it is no observation, so nothing is let go and every streak starts
//! again after it. `GONE_AFTER` counts only passes that listed something. A blind pass also
//! forgets how each waiting window looked (the Codex review of 10-21, R3): nothing is
//! [`Watch::shown`], so nothing is reached, until a pass that sees again (registered frames
//! and ownership stay).
//!
//! The window server's list alone is never final. While a Space slides in it may list a
//! window at an in-between frame, or not at all, for a pass (hands-free Desktop run 2, 10-12:
//! the guardian took one such listing of a window still parked on Desktop 2 for "back" and
//! ended; only the next launch brought it back). So the list counts as settled only
//! [`LIST_SETTLE`] after the last change in the set of windows on screen (the controller's
//! rule after a Space change), and an outside-the-park-zone sighting off screen (a Desktop
//! not shown) never counts.
//!
//! A **hidden tab** ([`super::hidden_tab`], 8J) is never waited for: the platform judges the
//! windows found in the park zone each pass ([`Watch::hidden_tabs`]); one judged a hidden tab
//! never joins, and one already waiting is let go. Nothing can move a hidden tab, and it
//! takes its group's frame once selected. A window the app registered is never let go as one.
//!
//! Nor is **another app's off-screen window we never parked** (round 8M,
//! [`super::not_ours`]: Chrome's helpers, a hidden tab whose selected tab is on another
//! Desktop): judged by the platform the same way ([`Watch::leave_alone`]), it never joins, and
//! one waiting is let go. A registered window is never let go as one either.
//!
//! Round 8N: a guardian hands its registered windows to a running Stage Manager
//! ([`super::handoff`], [`Release::HandedOff`]); the new app watches the windows it took with
//! a watch of its own that never takes in a window found in the park zone
//! ([`Watch::registered_only`]), letting go of one its model manages ([`Release::Managed`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, Instant};

use super::geometry::Rect;
use super::recovery::{Known, Listed, Registered, ScanDisplays, is_stranded, safe_restore};
use super::stage_area::same_frame;

/// The window list counts as settled this long after the set of windows on screen changed.
pub const LIST_SETTLE: Duration = Duration::from_secs(1);

/// Two settled on-screen sightings outside the park zone at least this far apart see a
/// window back.
pub const SEEN_APART: Duration = Duration::from_secs(1);

/// A window not listed on any Space in every pass for this long is gone.
pub const GONE_AFTER: Duration = Duration::from_secs(3);

/// How a waiting window looked in the latest pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Look {
    /// Listed in the park zone; `shown` when on screen (its Desktop shows).
    Parked {
        /// On screen.
        shown: bool,
        /// The listed frame.
        frame: Rect,
    },
    /// Listed on screen outside the park zone while the list has not settled (a Space may be
    /// sliding in): not trusted.
    OutUnsettled {
        /// The listed frame.
        frame: Rect,
    },
    /// Listed on screen outside the park zone, settled: seen back once that holds for
    /// [`SEEN_APART`].
    OutShown {
        /// The listed frame.
        frame: Rect,
    },
    /// Listed outside the park zone on a Desktop not shown: not trusted, waits for its
    /// Desktop.
    OutAway {
        /// The listed frame.
        frame: Rect,
    },
    /// Not listed on any Space.
    Missing,
}

impl Look {
    /// On screen now (Accessibility can reach it).
    pub fn shown(&self) -> bool {
        match self {
            Self::Parked { shown, .. } => *shown,
            Self::OutUnsettled { .. } | Self::OutShown { .. } => true,
            Self::OutAway { .. } | Self::Missing => false,
        }
    }

    /// The same kind of look (frames aside): a change is logged.
    fn same_kind(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Parked { shown: a, .. }, Self::Parked { shown: b, .. }) => a == b,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

impl fmt::Display for Look {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parked { shown: false, frame } => write!(f, "parked at {frame} on a Desktop not shown"),
            Self::Parked { shown: true, frame } => write!(f, "parked at {frame}, its Desktop shows"),
            Self::OutUnsettled { frame } => {
                write!(f, "listed on screen at {frame} while the windows on screen change (not trusted yet)")
            }
            Self::OutShown { frame } => write!(f, "listed on screen at {frame}, outside the park zone (checking it holds)"),
            Self::OutAway { frame } => write!(f, "listed at {frame} on a Desktop not shown (not trusted)"),
            Self::Missing => write!(f, "not listed on any Desktop"),
        }
    }
}

/// Why a window was let go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Release {
    /// The guardian moved it back and read it back through Accessibility at `frame`.
    BroughtBack {
        /// The frame read back.
        frame: Rect,
    },
    /// Read through Accessibility outside the park zone (already back).
    ReadBack {
        /// The frame read.
        frame: Rect,
    },
    /// Listed on screen outside the park zone in settled passes `apart` apart.
    SeenBack {
        /// The latest frame listed.
        frame: Rect,
        /// Time between the first and the latest sighting.
        apart: Duration,
    },
    /// Not listed on any Space for `missing`.
    Gone {
        /// How long it was missing.
        missing: Duration,
    },
    /// Stage Manager runs again and reports the window adopted (in its model).
    Adopted,
    /// Found in the park zone (never registered), and a hidden tab (8J): nothing can move it.
    HiddenTab,
    /// Found in the park zone (never registered), another app's off-screen window we never
    /// parked (round 8M, [`super::not_ours`]).
    NotOurs,
    /// Found in the park zone (never registered) while Stage Manager runs again: the new app
    /// judges it (it may have parked it itself).
    Successor,
    /// Handed to the running Stage Manager with the rest of the watch (round 8N,
    /// [`super::handoff`]): it waits for the window now.
    HandedOff,
    /// A window the app took from an old guardian is in its model now (round 8N): the model
    /// brings it back, or parks it in its stage, itself.
    Managed,
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BroughtBack { frame } => write!(f, "brought back; read back through Accessibility at {frame}"),
            Self::ReadBack { frame } => write!(f, "read through Accessibility at {frame}, outside the park zone"),
            Self::SeenBack { frame, apart } => write!(
                f,
                "seen on screen outside the park zone (at {frame}) in settled passes {:.1} s apart",
                apart.as_secs_f64()
            ),
            Self::Gone { missing } => write!(
                f,
                "gone from the window list on every Desktop for {:.1} s (closed, or its app quit)",
                missing.as_secs_f64()
            ),
            Self::Adopted => write!(f, "Stage Manager runs again and has adopted it"),
            Self::HandedOff => write!(f, "handed to the running Stage Manager, which waits for it now"),
            Self::Managed => write!(f, "Stage Manager manages it now (in its model)"),
            Self::Successor => write!(f, "found in the park zone, never registered; Stage Manager runs again and judges it"),
            Self::NotOurs => write!(
                f,
                "another app's off-screen window in the zone, not ours (never registered, not saved, not listed through Accessibility by its app)"
            ),
            Self::HiddenTab => write!(
                f,
                "a hidden tab (off screen, not listed through Accessibility, beside its group's selected tab): nothing can move it, and it takes its group's frame once selected"
            ),
        }
    }
}

/// What a pass changed, for the log.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// The set of windows on screen changed (a Space change, a window opened or closed):
    /// listings are not trusted for [`LIST_SETTLE`].
    ScreenChanged {
        /// Windows on screen now.
        on_screen: usize,
    },
    /// A window joined the watch.
    Joined {
        /// `CGWindowID`.
        id: u32,
        /// The owning process.
        pid: i32,
        /// Registered by the app (else found in the park zone).
        registered: bool,
    },
    /// A waiting window looks different.
    Looks {
        /// `CGWindowID`.
        id: u32,
        /// The owning process.
        pid: i32,
        /// How it looks now.
        look: Look,
    },
    /// The listing showed no window on screen (or nothing at all): the pass observed nothing.
    /// Sent once per blind stretch.
    Blind {
        /// Windows listed (on any Space).
        listed: usize,
    },
    /// A window was let go.
    LetGo {
        /// `CGWindowID`.
        id: u32,
        /// The owning process.
        pid: i32,
        /// Why.
        why: Release,
    },
}

/// A window the guardian waits for.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiting {
    /// `CGWindowID`.
    pub id: u32,
    /// The owning process.
    pub pid: i32,
    /// The registered restore frame (as written; [`Watch::restore_frame`] makes it safe);
    /// `None` for a window found in the park zone (the recovery scan decides).
    pub registered: Option<Rect>,
    /// A window we had fitted clear of the strip ([`Registered::fitted`]): back only at its
    /// registered pre-fit frame, or once the guardian has moved it there (L3).
    pub fitted: bool,
    /// Taken from a guardian not signed like this app ([`Registered::unvouched`], R4):
    /// reached only while listed on screen in the park zone, by any app's park test.
    pub unvouched: bool,
    /// How it looked in the latest pass (`None` before the first, and since a blind one).
    pub look: Option<Look>,
    /// The first settled on-screen sighting outside the park zone of the current streak.
    out_since: Option<Instant>,
    /// The first pass of the current streak it was not listed in.
    missing_since: Option<Instant>,
}

/// The watch: every waiting window and the window list's settle state.
#[derive(Debug, Clone, Default)]
pub struct Watch {
    registered: Vec<Registered>,
    waiting: BTreeMap<u32, Waiting>,
    on_screen: Option<BTreeSet<u32>>,
    changed_at: Option<Instant>,
    blind: bool,
    /// Stage Manager runs again ([`Self::hand_over`]): no window found in the park zone joins.
    successor: bool,
    /// The windows judged hidden tabs or not ours in the latest pass ([`Self::leave_alone`]),
    /// with why: none joins.
    tabs: BTreeMap<u32, Release>,
}

impl Watch {
    /// A watch over the app's registered windows; others join when found in the park zone.
    pub fn new(registered: &[Registered]) -> Self {
        let mut w = Self { registered: registered.to_vec(), ..Self::default() };
        for r in registered {
            w.waiting.insert(r.id, waiting_for(r));
        }
        w
    }

    /// A watch over registered windows only (round 8N: the windows an app took from an old
    /// guardian): no window found in the park zone ever joins.
    pub fn registered_only(registered: &[Registered]) -> Self {
        Self { successor: true, ..Self::new(registered) }
    }

    /// Adds a registered window (taken later); false when it is registered already.
    pub fn add(&mut self, r: Registered) -> bool {
        if self.is_registered(r.id) {
            return false;
        }
        self.registered.push(r);
        self.waiting.insert(r.id, waiting_for(&r));
        true
    }

    /// Drops window `id` from the watch, registered or not (another path saw to it).
    pub fn forget(&mut self, id: u32) {
        self.registered.retain(|r| r.id != id);
        self.waiting.remove(&id);
    }

    /// Lets go of every waiting window of `ids`, for `why`.
    pub fn let_go(&mut self, ids: &[u32], why: Release) -> Vec<Event> {
        ids.iter().filter_map(|id| self.waiting.remove(id).map(|w| Event::LetGo { id: w.id, pid: w.pid, why })).collect()
    }

    /// The registered windows still waiting, as registered (what a handoff offers).
    pub fn registered_waiting(&self) -> Vec<Registered> {
        self.registered.iter().filter(|r| self.waiting.contains_key(&r.id)).copied().collect()
    }

    /// True when the app registered window `id` (its parked file).
    pub fn is_registered(&self, id: u32) -> bool {
        self.registered.iter().any(|r| r.id == id)
    }

    /// The windows the platform judged hidden tabs this pass (8J; [`super::hidden_tab`],
    /// only ever among windows the app never registered): none of them joins the watch at
    /// [`Self::observe`], and a waiting one is let go. A registered window is never let go
    /// as one. Replaces the latest pass's set.
    pub fn hidden_tabs(&mut self, tabs: &BTreeSet<u32>) -> Vec<Event> {
        self.leave_alone(&tabs.iter().map(|id| (*id, Release::HiddenTab)).collect())
    }

    /// The windows the platform judged left alone this pass, with why: hidden tabs (8J) and
    /// another app's off-screen windows we never parked ([`Release::NotOurs`], round 8M), only
    /// ever among windows the app never registered. None of them joins the watch at
    /// [`Self::observe`], and a waiting one is let go; a registered window never is. Replaces
    /// the latest pass's set.
    pub fn leave_alone(&mut self, judged: &BTreeMap<u32, Release>) -> Vec<Event> {
        self.tabs = judged.iter().filter(|(id, _)| !self.is_registered(**id)).map(|(id, why)| (*id, *why)).collect();
        let go: Vec<(u32, i32, Release)> = self
            .waiting
            .values()
            .filter(|w| w.registered.is_none())
            .filter_map(|w| self.tabs.get(&w.id).map(|why| (w.id, w.pid, *why)))
            .collect();
        go.into_iter()
            .map(|(id, pid, why)| {
                self.waiting.remove(&id);
                Event::LetGo { id, pid, why }
            })
            .collect()
    }

    /// True when no window waits.
    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    /// Every waiting window, by id.
    pub fn waiting(&self) -> impl Iterator<Item = &Waiting> {
        self.waiting.values()
    }

    /// Waiting windows on screen in the latest pass (Accessibility can reach them): none
    /// while blind (R3: no observation never authorizes a move), and an unvouched one only
    /// while listed in the park zone (R4).
    pub fn shown(&self) -> Vec<Waiting> {
        self.waiting.values().filter(|w| !self.blind && w.reachable()).cloned().collect()
    }

    /// True when a waiting window may be reached now ([`Self::shown`] is not empty).
    pub fn shows(&self) -> bool {
        !self.blind && self.waiting.values().any(Waiting::reachable)
    }

    /// Where a waiting window goes back: its registered frame made safe; `None` for one found
    /// in the park zone.
    pub fn restore_frame(w: &Waiting, displays: &ScanDisplays) -> Option<Rect> {
        w.registered.map(|r| safe_restore(r, displays))
    }

    /// True when the window list counts as settled at `now`.
    pub fn settled(&self, now: Instant) -> bool {
        self.changed_at.is_none_or(|t| now.duration_since(t) >= LIST_SETTLE)
    }

    /// One look at the window server's list (layer-0 windows on every Space) at `now`.
    pub fn observe(&mut self, now: Instant, listed: &[Listed], displays: &ScanDisplays) -> Vec<Event> {
        let mut events = Vec::new();
        let on_screen: BTreeSet<u32> = listed.iter().filter(|l| l.onscreen).map(|l| l.id).collect();
        if on_screen.is_empty() {
            // No observation: no streak may run on through it, and whatever is listed next
            // counts as a change (it settles first).
            if !self.blind {
                events.push(Event::Blind { listed: listed.len() });
            }
            self.blind = true;
            self.on_screen = None;
            // Nor does an earlier look stand (R3): a window seen on screen before is not
            // reachable on its strength; registered frames and ownership stay.
            for w in self.waiting.values_mut() {
                w.missing_since = None;
                w.out_since = None;
                w.look = None;
            }
            return events;
        }
        self.blind = false;
        if self.on_screen.as_ref() != Some(&on_screen) {
            // The first pass counts as a change too: nothing is known about what came before.
            self.changed_at = Some(now);
            events.push(Event::ScreenChanged { on_screen: on_screen.len() });
            self.on_screen = Some(on_screen);
        }
        let settled = self.settled(now);
        for l in listed {
            if self.waiting.contains_key(&l.id) || self.tabs.contains_key(&l.id) {
                continue;
            }
            let reg = self.registered.iter().find(|r| r.id == l.id);
            // A window the app registered is its own, however small, sliver or not (L4).
            let known = reg.map_or(Known::Unknown, |r| if r.unvouched { Known::Unknown } else { Known::Registered });
            if !is_stranded(l.bounds, displays, known) {
                continue;
            }
            // While Stage Manager runs again, a window in the park zone may be one it parked:
            // only the crashed app's own registered windows join.
            if reg.is_some() || (l.allowed && !self.successor) {
                self.waiting.insert(l.id, reg.map_or_else(|| waiting(l.id, l.pid, None), waiting_for));
                events.push(Event::Joined { id: l.id, pid: l.pid, registered: reg.is_some() });
            }
        }
        let mut done = Vec::new();
        for w in self.waiting.values_mut() {
            let look = match listed.iter().find(|l| l.id == w.id) {
                None => {
                    w.out_since = None;
                    let since = *w.missing_since.get_or_insert(now);
                    let missing = now.duration_since(since);
                    if missing >= GONE_AFTER {
                        done.push((w.id, w.pid, Release::Gone { missing }));
                    }
                    Look::Missing
                }
                Some(l) => {
                    w.missing_since = None;
                    if is_stranded(l.bounds, displays, w.known()) {
                        w.out_since = None;
                        Look::Parked { shown: l.onscreen, frame: l.bounds }
                    } else if !l.onscreen {
                        w.out_since = None;
                        Look::OutAway { frame: l.bounds }
                    } else if !settled {
                        w.out_since = None;
                        Look::OutUnsettled { frame: l.bounds }
                    } else if w.fitted && !at_pre_fit(w, l.bounds, displays) {
                        // Shown, but still at the fitted frame: the guardian moves it back.
                        w.out_since = None;
                        Look::OutShown { frame: l.bounds }
                    } else {
                        let since = *w.out_since.get_or_insert(now);
                        let apart = now.duration_since(since);
                        if apart >= SEEN_APART {
                            done.push((w.id, w.pid, Release::SeenBack { frame: l.bounds, apart }));
                        }
                        Look::OutShown { frame: l.bounds }
                    }
                }
            };
            if !w.look.is_some_and(|old| old.same_kind(&look)) {
                events.push(Event::Looks { id: w.id, pid: w.pid, look });
            }
            w.look = Some(look);
        }
        for (id, pid, why) in done {
            self.waiting.remove(&id);
            events.push(Event::LetGo { id, pid, why });
        }
        events
    }

    /// Window `id`'s frame read through Accessibility (after moving it, when `moved`): let go
    /// when outside the park zone. A frame in the park zone changes nothing.
    /// A fitted window read away from its pre-fit frame is let go only after the guardian
    /// moved it (`moved`): never a fight with the user over a frame macOS adjusted.
    pub fn read_through_ax(&mut self, id: u32, frame: Rect, moved: bool, displays: &ScanDisplays) -> Option<Event> {
        let known = self.waiting.get(&id).map_or(Known::Unknown, Waiting::known);
        if is_stranded(frame, displays, known) {
            return None;
        }
        if !moved && self.waiting.get(&id).is_some_and(|w| w.fitted && !at_pre_fit(w, frame, displays)) {
            return None;
        }
        let w = self.waiting.remove(&id)?;
        let why = if moved { Release::BroughtBack { frame } } else { Release::ReadBack { frame } };
        Some(Event::LetGo { id, pid: w.pid, why })
    }
}

impl Watch {
    /// Stage Manager runs again (Grok M3, 1.1.0): the windows it reports `adopted` (in its
    /// model: it brings them back, or parks them in their stage, itself) are let go, and so
    /// is every waiting window found in the park zone that the crashed app never registered
    /// (the new app judges those). The crashed app's registered windows the new app has not
    /// adopted yet keep waiting: on a Desktop not shown, the new app finds them only when
    /// that Desktop shows, and until it does the guardian still brings each back. From now on
    /// no window found in the park zone joins the watch.
    pub fn hand_over(&mut self, adopted: &[u32]) -> Vec<Event> {
        self.successor = true;
        let go: Vec<(u32, i32, Release)> = self
            .waiting
            .values()
            .filter_map(|w| {
                if adopted.contains(&w.id) {
                    Some((w.id, w.pid, Release::Adopted))
                } else if w.registered.is_none() {
                    Some((w.id, w.pid, Release::Successor))
                } else {
                    None
                }
            })
            .collect();
        go.into_iter()
            .map(|(id, pid, why)| {
                self.waiting.remove(&id);
                Event::LetGo { id, pid, why }
            })
            .collect()
    }

    /// True once Stage Manager was seen running again ([`Self::hand_over`]).
    pub fn handed_over(&self) -> bool {
        self.successor
    }
}

impl Waiting {
    /// On screen in the latest pass (none since a blind one).
    pub fn shows(&self) -> bool {
        self.look.is_some_and(|l| l.shown())
    }

    /// May be reached through Accessibility now: on screen in the latest pass; an unvouched
    /// one (R4) only while listed there in the park zone.
    pub fn reachable(&self) -> bool {
        if self.unvouched { matches!(self.look, Some(Look::Parked { shown: true, .. })) } else { self.shows() }
    }

    /// What the park test knows of it: a window the app registered is its own (any size,
    /// slivers count), one found in the park zone or taken unvouched (R4) is any app's.
    pub fn known(&self) -> Known {
        if self.registered.is_some() && !self.unvouched { Known::Registered } else { Known::Unknown }
    }
}

fn waiting(id: u32, pid: i32, registered: Option<Rect>) -> Waiting {
    Waiting { id, pid, registered, fitted: false, unvouched: false, look: None, out_since: None, missing_since: None }
}

/// A registered window, waiting: fitted only when vouched for (R4).
fn waiting_for(r: &Registered) -> Waiting {
    Waiting { fitted: r.fitted && !r.unvouched, unvouched: r.unvouched, ..waiting(r.id, r.pid, Some(r.restore)) }
}

/// True when a waiting window sits at its registered frame (as registered, or as the
/// guardian would put it back).
fn at_pre_fit(w: &Waiting, frame: Rect, displays: &ScanDisplays) -> bool {
    w.registered.is_some_and(|r| same_frame(frame, r) || same_frame(frame, safe_restore(r, displays)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Size;
    use crate::engine::park::park_frame;

    const MAIN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    const VISIBLE: Rect = Rect::new(0.0, 30.0, 2560.0, 1410.0);
    const RESTORE: Rect = Rect::new(300.0, 218.0, 600.0, 392.0);
    /// Another window on Desktop 2 (never parked): its arrival on screen is the Space change.
    const OTHER: u32 = 50;

    fn displays() -> ScanDisplays {
        ScanDisplays::new(MAIN, VISIBLE, vec![], [])
    }

    fn parked() -> Rect {
        park_frame(Size::new(600.0, 392.0), MAIN)
    }

    fn reg() -> Vec<Registered> {
        vec![Registered { id: 1, pid: 7, restore: RESTORE, child: false, fitted: false, unvouched: false }]
    }

    fn at(id: u32, bounds: Rect, onscreen: bool) -> Listed {
        Listed { id, pid: 7, bounds, onscreen, allowed: false }
    }

    /// A pass at `t0 + ms`, returning whether window 1 was let go and why.
    fn pass(w: &mut Watch, t0: Instant, ms: u64, listed: &[Listed]) -> Option<Release> {
        w.observe(t0 + Duration::from_millis(ms), listed, &displays()).into_iter().find_map(|e| match e {
            Event::LetGo { id: 1, why, .. } => Some(why),
            _ => None,
        })
    }

    /// Desktop 1 shows: window 1 parked on Desktop 2 (off screen), window 9 on screen.
    fn on_desktop_1() -> Vec<Listed> {
        vec![at(1, parked(), false), at(OTHER, Rect::new(80.0, 78.0, 520.0, 392.0), false), at(9, RESTORE, true)]
    }

    /// Desktop 2 shows, window 1 at `frame` on screen.
    fn on_desktop_2(frame: Rect) -> Vec<Listed> {
        vec![at(1, frame, true), at(OTHER, Rect::new(80.0, 78.0, 520.0, 392.0), true), at(9, RESTORE, false)]
    }

    /// 6X, the owner's log: helper apps' invisible windows (1×1 at 1280,720 seen on the
    /// built-in display, 64×64 ones) counted as parked; the guardian would have waited for
    /// them. Only a real parked window of an allowed app joins.
    #[test]
    fn helper_windows_never_join_the_watch() {
        let laptop = Rect::new(0.0, 0.0, 1800.0, 1169.0);
        let d = ScanDisplays::new(laptop, Rect::new(0.0, 39.0, 1800.0, 1130.0), vec![], []);
        let mut w = Watch::new(&[]);
        let allowed = |id, bounds, onscreen| Listed { id, pid: 8, bounds, onscreen, allowed: true };
        let listed = [
            allowed(20, Rect::new(1280.0, 720.0, 1.0, 1.0), false),
            allowed(21, Rect::new(1700.0, 1100.0, 64.0, 64.0), false),
            allowed(22, Rect::new(1799.0, 1168.0, 1.0, 1.0), false),
            allowed(23, Rect::new(1799.0, 1137.0, 1051.0, 906.0), false),
            allowed(24, Rect::new(200.0, 100.0, 800.0, 600.0), true),
        ];
        let events = w.observe(Instant::now(), &listed, &d);
        let joined: Vec<u32> = events
            .iter()
            .filter_map(|e| match e {
                Event::Joined { id, .. } => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(joined, vec![23], "only the real parked window");
    }

    /// The review of 6X, L4: a small window the app registered (a 40 pt palette it parked)
    /// joins and waits like any other, while another app's window that small never joins.
    #[test]
    fn a_small_registered_window_is_never_lost() {
        let d = displays();
        let small = Rect::new(2559.0, 1408.0, 40.0, 30.0);
        let reg = Registered { id: 7, pid: 8, restore: Rect::new(300.0, 300.0, 40.0, 30.0), child: false, fitted: false, unvouched: false };
        let mut w = Watch::new(&[reg]);
        let listed = [
            Listed { id: 7, pid: 8, bounds: small, onscreen: false, allowed: true },
            Listed { id: 9, pid: 8, bounds: small, onscreen: false, allowed: true },
            at(OTHER, Rect::new(80.0, 78.0, 520.0, 392.0), true),
        ];
        let t0 = Instant::now();
        w.observe(t0, &listed, &d);
        let ids: Vec<u32> = w.waiting().map(|w| w.id).collect();
        assert_eq!(ids, vec![7], "the registered one waits, the stranger's does not join");
        assert_eq!(w.waiting().next().and_then(|w| w.look), Some(Look::Parked { shown: false, frame: small }));
        // Read through Accessibility still in the corner: not let go.
        assert_eq!(w.read_through_ax(7, small, true, &d), None);
        assert!(w.read_through_ax(7, Rect::new(300.0, 300.0, 40.0, 30.0), true, &d).is_some());
    }

    #[test]
    fn a_parked_window_on_a_desktop_not_shown_waits() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        for ms in (0..20_000).step_by(500) {
            assert_eq!(pass(&mut w, t0, ms, &on_desktop_1()), None);
        }
        assert!(!w.is_empty());
        assert!(w.shown().is_empty(), "not reachable while its Desktop is not shown");
    }

    #[test]
    fn the_in_between_frame_of_a_slide_never_ends_the_watch() {
        // The 10-12 run: Desktop 2 slides in; one listing has the parked window at an
        // in-between frame outside the park zone, then it is listed parked again.
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        assert_eq!(pass(&mut w, t0, 0, &on_desktop_1()), None);
        assert_eq!(pass(&mut w, t0, 1500, &on_desktop_1()), None);
        let sliding = Rect::new(1279.0, 1408.0, 600.0, 392.0);
        assert_eq!(pass(&mut w, t0, 2000, &on_desktop_2(sliding)), None, "the in-between frame is not trusted");
        assert!(matches!(w.waiting().next().and_then(|x| x.look), Some(Look::OutUnsettled { .. })));
        assert_eq!(w.shown().len(), 1, "but the guardian may reach it now");
        assert_eq!(pass(&mut w, t0, 2500, &on_desktop_2(parked())), None);
        for ms in (3000..10_000).step_by(500) {
            assert_eq!(pass(&mut w, t0, ms, &on_desktop_2(parked())), None, "still parked at {ms} ms");
        }
    }

    #[test]
    fn an_in_between_frame_held_through_a_slide_is_not_seen_back_before_the_list_settles() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        pass(&mut w, t0, 0, &on_desktop_1());
        pass(&mut w, t0, 1500, &on_desktop_1());
        let sliding = Rect::new(1279.0, 1408.0, 600.0, 392.0);
        // Listed outside the park zone for 0.9 s right after the change: still unsettled.
        assert_eq!(pass(&mut w, t0, 2000, &on_desktop_2(sliding)), None);
        assert_eq!(pass(&mut w, t0, 2500, &on_desktop_2(sliding)), None);
        assert_eq!(pass(&mut w, t0, 2900, &on_desktop_2(sliding)), None);
    }

    #[test]
    fn missing_for_one_pass_never_ends_the_watch() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        pass(&mut w, t0, 0, &on_desktop_1());
        let gone_for_a_pass: Vec<Listed> = on_desktop_2(parked()).into_iter().filter(|l| l.id != 1).collect();
        assert_eq!(pass(&mut w, t0, 1500, &gone_for_a_pass), None);
        assert!(matches!(w.waiting().next().and_then(|x| x.look), Some(Look::Missing)));
        assert_eq!(pass(&mut w, t0, 2000, &on_desktop_2(parked())), None);
        // Missing again later: the count starts over.
        assert_eq!(pass(&mut w, t0, 2500, &gone_for_a_pass), None);
        assert_eq!(pass(&mut w, t0, 5000, &on_desktop_2(parked())), None);
        assert_eq!(pass(&mut w, t0, 5500, &gone_for_a_pass), None);
        assert!(!w.is_empty());
    }

    #[test]
    fn missing_in_every_pass_for_three_seconds_is_gone() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        let none: Vec<Listed> = on_desktop_1().into_iter().filter(|l| l.id != 1).collect();
        for ms in (0..3000).step_by(500) {
            assert_eq!(pass(&mut w, t0, ms, &none), None, "{ms} ms");
        }
        assert!(matches!(pass(&mut w, t0, 3000, &none), Some(Release::Gone { .. })));
        assert!(w.is_empty());
    }

    #[test]
    fn seen_back_takes_two_settled_passes_a_second_apart() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        pass(&mut w, t0, 0, &on_desktop_1());
        pass(&mut w, t0, 1500, &on_desktop_1());
        // Desktop 2 shows, the window already back (someone moved it): the list settles 1 s
        // after the change, then the sighting must hold for 1 s.
        assert_eq!(pass(&mut w, t0, 2000, &on_desktop_2(RESTORE)), None);
        assert_eq!(pass(&mut w, t0, 2500, &on_desktop_2(RESTORE)), None);
        assert_eq!(pass(&mut w, t0, 3000, &on_desktop_2(RESTORE)), None, "settled: the first sighting");
        assert_eq!(pass(&mut w, t0, 3500, &on_desktop_2(RESTORE)), None);
        assert!(matches!(pass(&mut w, t0, 4000, &on_desktop_2(RESTORE)), Some(Release::SeenBack { .. })));
    }

    #[test]
    fn a_parked_sighting_between_starts_the_count_over() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        pass(&mut w, t0, 0, &on_desktop_2(RESTORE));
        assert_eq!(pass(&mut w, t0, 1000, &on_desktop_2(RESTORE)), None);
        assert_eq!(pass(&mut w, t0, 1500, &on_desktop_2(parked())), None);
        assert_eq!(pass(&mut w, t0, 2000, &on_desktop_2(RESTORE)), None);
        assert_eq!(pass(&mut w, t0, 2500, &on_desktop_2(RESTORE)), None);
        assert!(pass(&mut w, t0, 3000, &on_desktop_2(RESTORE)).is_some());
    }

    #[test]
    fn a_change_on_screen_starts_the_count_over() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        pass(&mut w, t0, 0, &on_desktop_2(RESTORE));
        assert_eq!(pass(&mut w, t0, 1000, &on_desktop_2(RESTORE)), None);
        let mut more = on_desktop_2(RESTORE);
        more.push(at(77, Rect::new(10.0, 40.0, 100.0, 100.0), true));
        assert_eq!(pass(&mut w, t0, 1500, &more), None, "a window opened: the list is not settled");
        assert_eq!(pass(&mut w, t0, 2500, &more), None, "settled again: a new first sighting");
        assert!(pass(&mut w, t0, 3500, &more).is_some());
    }

    #[test]
    fn outside_the_park_zone_on_a_desktop_not_shown_never_counts() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        let away = vec![at(1, RESTORE, false), at(9, Rect::new(0.0, 40.0, 100.0, 100.0), true)];
        for ms in (0..10_000).step_by(500) {
            assert_eq!(pass(&mut w, t0, ms, &away), None);
        }
        assert!(matches!(w.waiting().next().and_then(|x| x.look), Some(Look::OutAway { .. })));
    }

    #[test]
    fn accessibility_outside_the_park_zone_lets_go_at_once() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        pass(&mut w, t0, 0, &on_desktop_2(parked()));
        assert_eq!(w.read_through_ax(1, parked(), false, &displays()), None, "still parked");
        assert!(!w.is_empty());
        let e = w.read_through_ax(1, RESTORE, true, &displays());
        assert_eq!(e, Some(Event::LetGo { id: 1, pid: 7, why: Release::BroughtBack { frame: RESTORE } }));
        assert!(w.is_empty());
    }

    #[test]
    fn unregistered_park_zone_windows_follow_the_same_rule() {
        let t0 = Instant::now();
        let mut w = Watch::new(&[]);
        let mut allowed = at(2, parked(), false);
        allowed.allowed = true;
        let not_allowed = at(3, parked(), false);
        let ev = w.observe(t0, &[allowed, not_allowed, at(9, RESTORE, true)], &displays());
        assert!(ev.contains(&Event::Joined { id: 2, pid: 7, registered: false }));
        assert_eq!(w.waiting().map(|x| x.id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(w.waiting().next().and_then(|x| Watch::restore_frame(x, &displays())), None);
        // Its Desktop slides in at an in-between frame: not let go.
        let sliding = Listed { bounds: Rect::new(1279.0, 1408.0, 600.0, 392.0), onscreen: true, ..allowed };
        let ev = w.observe(t0 + Duration::from_millis(500), &[sliding], &displays());
        assert!(!ev.iter().any(|e| matches!(e, Event::LetGo { .. })));
        let back = Listed { bounds: RESTORE, ..sliding };
        for ms in [1500, 2000] {
            let ev = w.observe(t0 + Duration::from_millis(ms), &[back], &displays());
            assert!(!ev.iter().any(|e| matches!(e, Event::LetGo { .. })), "{ms} ms");
        }
        let ev = w.observe(t0 + Duration::from_millis(2500), &[back], &displays());
        assert!(ev.iter().any(|e| matches!(e, Event::LetGo { id: 2, why: Release::SeenBack { .. }, .. })));
    }

    /// The review's S3: three (or thirty) empty listings in a row (another user's session, the
    /// login window) are no observation; a window is gone only after `GONE_AFTER` of passes
    /// that listed something.
    #[test]
    fn empty_listings_never_let_a_window_go() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        assert_eq!(pass(&mut w, t0, 0, &on_desktop_1()), None);
        let mut blind = 0;
        for ms in (500..20_000).step_by(500) {
            let ev = w.observe(t0 + Duration::from_millis(ms), &[], &displays());
            blind += ev.iter().filter(|e| matches!(e, Event::Blind { .. })).count();
            assert!(!ev.iter().any(|e| matches!(e, Event::LetGo { .. })), "{ms} ms");
        }
        assert_eq!(blind, 1, "reported once per blind stretch");
        // Windows listed, none on screen (the login window): blind too.
        let off: Vec<Listed> = on_desktop_1().into_iter().map(|l| Listed { onscreen: false, ..l }).collect();
        assert_eq!(pass(&mut w, t0, 20_000, &off), None);
        // Back, and window 1 really is gone now: only after GONE_AFTER of real passes.
        let without = vec![at(9, RESTORE, true)];
        assert_eq!(pass(&mut w, t0, 20_500, &without), None, "the blind stretch does not count");
        assert_eq!(pass(&mut w, t0, 22_500, &without), None);
        assert!(matches!(pass(&mut w, t0, 23_500, &without), Some(Release::Gone { .. })));
    }

    /// The Codex review of 10-21, R3: a window seen parked on screen is not reachable on the
    /// strength of that look once a pass is blind (nothing listed, or nothing on screen); it
    /// is again after a pass that sees it, and it keeps its registered frame throughout.
    #[test]
    fn a_blind_pass_leaves_nothing_reachable() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        assert_eq!(pass(&mut w, t0, 0, &on_desktop_2(parked())), None);
        assert_eq!(w.shown().len(), 1, "shown parked: reachable");
        assert!(w.shows());
        assert_eq!(pass(&mut w, t0, 500, &[]), None);
        assert!(w.shown().is_empty(), "blind (nothing listed): nothing reachable");
        assert!(!w.shows());
        let off: Vec<Listed> = on_desktop_2(parked()).into_iter().map(|l| Listed { onscreen: false, ..l }).collect();
        assert_eq!(pass(&mut w, t0, 1000, &off), None);
        assert!(w.shown().is_empty(), "blind (only off screen): nothing reachable");
        assert!(!w.shows());
        let x = w.waiting().next().cloned();
        assert_eq!(x.as_ref().map(|x| x.registered), Some(Some(RESTORE)), "the registered frame stays");
        assert!(w.is_registered(1));
        assert_eq!(pass(&mut w, t0, 1500, &on_desktop_2(parked())), None);
        let shown = w.shown();
        assert_eq!(shown.len(), 1, "seen again: reachable again");
        assert_eq!(Watch::restore_frame(&shown[0], &displays()), Some(RESTORE));
    }

    /// The review's S1 for the guardian: with a Dock at the bottom macOS keeps the parked
    /// window above it; an allowed app's window there joins the watch and is never "seen
    /// back" there.
    #[test]
    fn a_window_parked_above_a_bottom_dock_joins_and_waits() {
        let d = ScanDisplays::new(MAIN, Rect::new(0.0, 30.0, 2560.0, 1334.0), vec![], []);
        let above_dock = Rect::new(2559.0, d.visible.max_y() - crate::engine::park::MACOS_KEEPS_TITLE, 600.0, 392.0);
        let t0 = Instant::now();
        let mut w = Watch::new(&[]);
        let list = [Listed { allowed: true, ..at(2, above_dock, true) }, at(9, RESTORE, true)];
        for ms in (0..10_000).step_by(500) {
            let ev = w.observe(t0 + Duration::from_millis(ms), &list, &d);
            assert!(!ev.iter().any(|e| matches!(e, Event::LetGo { .. })), "{ms} ms");
        }
        assert!(matches!(w.waiting().next().and_then(|x| x.look), Some(Look::Parked { shown: true, .. })));
    }

    /// L3: a window we had fitted clear of the strip, on a Desktop not shown at the crash, is
    /// back only at its pre-fit frame, or once the guardian has moved it there.
    #[test]
    fn a_fitted_window_waits_for_its_pre_fit_frame() {
        let fitted_at = Rect::new(216.0, 218.0, 600.0, 392.0);
        let r = [Registered { id: 1, pid: 7, restore: RESTORE, child: false, fitted: true, unvouched: false }];
        let t0 = Instant::now();
        let mut w = Watch::new(&r);
        pass(&mut w, t0, 0, &on_desktop_1());
        for ms in (1500..8000).step_by(500) {
            assert_eq!(pass(&mut w, t0, ms, &on_desktop_2(fitted_at)), None, "{ms} ms: still fitted");
        }
        assert_eq!(w.shown().len(), 1, "the guardian reaches it");
        assert_eq!(w.read_through_ax(1, fitted_at, false, &displays()), None, "read at the fitted frame");
        let e = w.read_through_ax(1, Rect::new(301.0, 218.0, 600.0, 390.0), true, &displays());
        assert!(matches!(e, Some(Event::LetGo { why: Release::BroughtBack { .. }, .. })), "moved once: let go, macOS's adjustment accepted");
        // Seen at its pre-fit frame (the user put it back): let go too.
        let mut w = Watch::new(&r);
        pass(&mut w, t0, 0, &on_desktop_1());
        pass(&mut w, t0, 1500, &on_desktop_2(RESTORE));
        pass(&mut w, t0, 2500, &on_desktop_2(RESTORE));
        assert!(matches!(pass(&mut w, t0, 3500, &on_desktop_2(RESTORE)), Some(Release::SeenBack { .. })));
    }

    /// The Codex review of 10-21, R4: a window taken unvouched (from a sender not signed like
    /// this app) is reached only while listed on screen in the park zone. On screen anywhere
    /// else it is never reached (seen back there, it is let go unmoved), and even an offer
    /// that said fitted never makes it one.
    #[test]
    fn an_unvouched_window_is_reached_only_in_the_park_zone() {
        let r = Registered { id: 1, pid: 7, restore: RESTORE, child: false, fitted: true, unvouched: true };
        let t0 = Instant::now();
        let mut w = Watch::registered_only(&[r]);
        assert!(!w.waiting().next().is_some_and(|x| x.fitted), "never fitted");
        assert_eq!(w.waiting().next().map(Waiting::known), Some(Known::Unknown));
        pass(&mut w, t0, 0, &on_desktop_1());
        assert!(w.shown().is_empty(), "on a Desktop not shown: waits");
        assert_eq!(pass(&mut w, t0, 1500, &on_desktop_2(parked())), None);
        assert_eq!(w.shown().len(), 1, "listed on screen in the park zone: reachable");
        let ordinary = Rect::new(300.0, 150.0, 700.0, 500.0);
        assert_eq!(pass(&mut w, t0, 2000, &on_desktop_2(ordinary)), None);
        assert!(w.shown().is_empty() && !w.shows(), "on screen outside the park zone: never reached");
        assert_eq!(pass(&mut w, t0, 3000, &on_desktop_2(ordinary)), None);
        assert!(matches!(pass(&mut w, t0, 4000, &on_desktop_2(ordinary)), Some(Release::SeenBack { .. })), "let go unmoved");
    }

    /// Grok M3 (1.1.0): Stage Manager running again is no reason to stop watching. A new app
    /// that answers `ping` but has not adopted the crashed app's window (it is on a Desktop
    /// not shown, or the new app has not scanned yet) leaves it waiting; once adopted, it is
    /// let go. Windows found in the park zone are the new app's to judge, and none joins.
    #[test]
    fn a_new_app_running_takes_over_only_the_windows_it_adopted() {
        let t0 = Instant::now();
        let mut w = Watch::new(&[
            Registered { id: 1, pid: 7, restore: RESTORE, child: false, fitted: false, unvouched: false },
            Registered { id: 2, pid: 7, restore: RESTORE, child: false, fitted: false, unvouched: false },
        ]);
        let found = Listed { id: 30, pid: 8, bounds: parked(), onscreen: false, allowed: true };
        let mut listed = on_desktop_1();
        listed.push(at(2, parked(), true));
        listed.push(found);
        let events = w.observe(t0, &listed, &displays());
        assert!(events.contains(&Event::Joined { id: 30, pid: 8, registered: false }));
        // The new app answers but has adopted nothing yet: its launch scan is still running.
        let events = w.hand_over(&[]);
        assert_eq!(events, vec![Event::LetGo { id: 30, pid: 8, why: Release::Successor }], "the park-zone find is the new app's");
        assert_eq!(w.waiting().map(|x| x.id).collect::<Vec<_>>(), vec![1, 2], "the crashed app's windows keep waiting");
        assert!(w.handed_over());
        // The new app adopts window 2 (on the Desktop shown); window 1 stays on Desktop 2.
        let events = w.hand_over(&[2]);
        assert_eq!(events, vec![Event::LetGo { id: 2, pid: 7, why: Release::Adopted }]);
        assert_eq!(w.waiting().map(|x| x.id).collect::<Vec<_>>(), vec![1]);
        // While the new app runs, nothing found in the park zone joins (it may have parked it).
        let mut later = on_desktop_1();
        later.push(Listed { id: 31, pid: 8, bounds: parked(), onscreen: true, allowed: true });
        let events = w.observe(t0 + Duration::from_secs(1), &later, &displays());
        assert!(!events.iter().any(|e| matches!(e, Event::Joined { .. })), "{events:?}");
        // Desktop 2 shows and the new app finds window 1 there: let go.
        assert_eq!(w.hand_over(&[1]), vec![Event::LetGo { id: 1, pid: 7, why: Release::Adopted }]);
        assert!(w.is_empty());
    }

    /// Round 8N: a guardian hands its registered windows over and lets each one go; a watch
    /// of taken windows only (the new app's) never takes in a window found in the park zone,
    /// grows as more are taken, and drops one another path saw to.
    #[test]
    fn a_watch_hands_over_and_a_taken_watch_holds_only_what_it_took() {
        let t0 = Instant::now();
        let r1 = Registered { id: 1, pid: 7, restore: RESTORE, child: false, fitted: false, unvouched: false };
        let r2 = Registered { id: 2, pid: 7, restore: RESTORE, child: true, fitted: true, unvouched: false };
        let mut w = Watch::new(&[r1, r2]);
        let found = Listed { id: 30, pid: 8, bounds: parked(), onscreen: false, allowed: true };
        let mut listed = on_desktop_1();
        listed.push(found);
        w.observe(t0, &listed, &displays());
        assert_eq!(w.waiting().count(), 3);
        assert_eq!(w.registered_waiting(), vec![r1, r2], "only registered windows are offered");
        assert!(w.hand_over(&[]).iter().all(|e| matches!(e, Event::LetGo { id: 30, .. })));
        let ev = w.let_go(&[1, 2, 99], Release::HandedOff);
        assert_eq!(ev, vec![
            Event::LetGo { id: 1, pid: 7, why: Release::HandedOff },
            Event::LetGo { id: 2, pid: 7, why: Release::HandedOff }
        ]);
        assert!(w.is_empty());
        assert!(w.registered_waiting().is_empty());

        let mut taken = Watch::registered_only(&[r1]);
        let ev = taken.observe(t0, &listed, &displays());
        assert!(!ev.iter().any(|e| matches!(e, Event::Joined { .. })), "{ev:?}");
        assert_eq!(taken.waiting().map(|x| x.id).collect::<Vec<_>>(), vec![1]);
        assert!(taken.add(r2));
        assert!(!taken.add(r2), "once");
        assert_eq!(taken.waiting().find(|x| x.id == 2).map(|x| x.fitted), Some(true));
        taken.forget(1);
        assert!(!taken.is_registered(1));
        assert_eq!(taken.waiting().map(|x| x.id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(taken.let_go(&[2], Release::Managed).len(), 1);
        assert!(taken.is_empty());
    }

    #[test]
    fn registered_restore_frames_are_made_safe() {
        let r = [Registered { id: 1, pid: 7, restore: parked(), child: false, fitted: false, unvouched: false }];
        let w = Watch::new(&r);
        let f = w.waiting().next().and_then(|x| Watch::restore_frame(x, &displays())).unwrap_or_default();
        assert!(!is_stranded(f, &displays(), Known::Registered) && VISIBLE.contains_rect(&f), "{f:?}");
    }

    #[test]
    fn every_change_of_look_is_reported_once() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        let looks = |ev: Vec<Event>| ev.into_iter().filter(|e| matches!(e, Event::Looks { .. })).count();
        assert_eq!(looks(w.observe(t0, &on_desktop_1(), &displays())), 1);
        assert_eq!(looks(w.observe(t0 + Duration::from_millis(500), &on_desktop_1(), &displays())), 0);
        assert_eq!(looks(w.observe(t0 + Duration::from_millis(1000), &on_desktop_2(parked()), &displays())), 1);
    }

    /// 8J, the 10-16 quit: Terminal's hidden tabs 83 and 84 at the park spot, off screen,
    /// beside the selected tab 86 on another Desktop. Judged hidden tabs by the platform,
    /// they never join; a hidden tab already waiting (found before it was judged) is let go;
    /// the watch ends with nothing to wait for.
    #[test]
    fn hidden_tabs_never_join_and_are_let_go() {
        let t0 = Instant::now();
        let spot = Rect::new(2559.0, 1408.0, 1497.0, 1045.0);
        let terminal = |id, bounds, onscreen| Listed { id, pid: 501, bounds, onscreen, allowed: true };
        let listing = [terminal(83, spot, false), terminal(84, spot, false), terminal(86, Rect::new(532.0, 213.0, 1497.0, 1045.0), false), at(9, RESTORE, true)];
        let mut w = Watch::new(&[]);
        assert!(w.hidden_tabs(&[83, 84].into()).is_empty(), "nothing waits yet");
        let ev = w.observe(t0, &listing, &displays());
        assert!(!ev.iter().any(|e| matches!(e, Event::Joined { .. })), "{ev:?}");
        assert!(w.is_empty(), "the guardian has nothing to wait for");
        // Judged too late (its app answered only now): it joined, and is let go.
        let mut w = Watch::new(&[]);
        w.observe(t0, &listing, &displays());
        assert_eq!(w.waiting().count(), 2, "found in the park zone");
        let ev = w.hidden_tabs(&[83, 84].into());
        assert_eq!(ev.len(), 2);
        assert!(ev.iter().all(|e| matches!(e, Event::LetGo { why: Release::HiddenTab, .. })));
        assert!(w.is_empty());
        w.observe(t0 + Duration::from_millis(500), &listing, &displays());
        assert!(w.is_empty(), "and never joins again while it is one");
    }

    /// The review of 8J, Medium: two real windows of the same size parked on a Desktop not
    /// shown, the parked file lost (the guardian knows neither), both saved in `state.json`.
    /// Accessibility lists neither, and nothing else vouches: neither is judged a hidden
    /// tab (the guardian's [`crate::engine::hidden_tab::Candidate::Unknown`]), so both wait.
    #[test]
    fn two_same_size_windows_on_another_desktop_both_wait() {
        use crate::engine::hidden_tab::{Candidate, TabListed, is_hidden_tab};
        let t0 = Instant::now();
        let spot = Rect::new(2559.0, 1408.0, 800.0, 600.0);
        let saved = |id| Listed { id, pid: 501, bounds: spot, onscreen: false, allowed: true };
        // Window 9, another app's, shows on the Desktop shown.
        let listing = [saved(21), saved(22), at(9, RESTORE, true)];
        let tabs: Vec<TabListed> = listing.iter().map(|l| TabListed { id: l.id, pid: l.pid, layer: 0, onscreen: l.onscreen, bounds: l.bounds }).collect();
        let judged: BTreeSet<u32> = tabs.iter().filter(|t| is_hidden_tab(t, &tabs, Candidate::Unknown, Some(&[]), |_| true)).map(|t| t.id).collect();
        assert!(judged.is_empty(), "{judged:?}");
        let mut w = Watch::new(&[]);
        assert!(w.hidden_tabs(&judged).is_empty());
        w.observe(t0, &listing, &displays());
        assert_eq!(w.waiting().map(|x| x.id).collect::<Vec<_>>(), vec![21, 22], "both wait");
    }

    /// The review of 8J, High, in the guardian: window 1, parked, left its Space (dragged to
    /// Desktop 2 in Mission Control) beside window 9 of the same size, shown. It stays
    /// registered (its parked file): never let go as a hidden tab, pass after pass, whatever
    /// the platform judged.
    #[test]
    fn a_registered_window_that_left_its_space_still_waits() {
        let t0 = Instant::now();
        let listing = on_desktop_1();
        assert!(crate::engine::hidden_tab::same_size(listing[0].bounds, listing[2].bounds), "9 is 1's same-size sibling");
        let mut w = Watch::new(&reg());
        for ms in [0, 500, 1000, 5000] {
            assert!(w.hidden_tabs(&[1].into()).is_empty());
            w.observe(t0 + Duration::from_millis(ms), &listing, &displays());
            assert_eq!(w.waiting().map(|x| x.id).collect::<Vec<_>>(), vec![1], "still waits");
        }
    }

    /// Round 8M, the week's quits: Chrome's helper windows (116, 117) and Terminal's hidden
    /// tabs whose selected tab is on another Desktop (83, 84), never registered, judged not
    /// ours by the platform: none joins, one already waiting is let go; a registered window
    /// parked on another Desktop judged the same way still waits.
    #[test]
    fn windows_not_ours_never_join_and_a_registered_one_still_waits() {
        let t0 = Instant::now();
        let chrome = |id, h| Listed { id, pid: 502, bounds: Rect::new(2658.0, 1424.0, 1516.0, h), onscreen: false, allowed: true };
        let spot = Rect::new(2559.0, 1408.0, 1497.0, 1045.0);
        let terminal = |id| Listed { id, pid: 501, bounds: spot, onscreen: false, allowed: true };
        let foreign = [chrome(116, 139.0), chrome(117, 89.0), terminal(83), terminal(84)];
        let listing: Vec<Listed> = foreign.iter().copied().chain([at(9, RESTORE, true)]).collect();
        let judged: BTreeMap<u32, Release> = [116, 117, 83, 84].into_iter().map(|id| (id, Release::NotOurs)).collect();
        let mut w = Watch::new(&[]);
        assert!(w.leave_alone(&judged).is_empty());
        let ev = w.observe(t0, &listing, &displays());
        assert!(!ev.iter().any(|e| matches!(e, Event::Joined { .. })), "{ev:?}");
        assert!(w.is_empty(), "the guardian has nothing to wait for");
        // Judged after it joined (its app answered only now): let go.
        let mut w = Watch::new(&[]);
        w.observe(t0, &listing, &displays());
        assert_eq!(w.waiting().count(), 4);
        let ev = w.leave_alone(&judged);
        assert_eq!(ev.len(), 4);
        assert!(ev.iter().all(|e| matches!(e, Event::LetGo { why: Release::NotOurs, .. })));
        assert!(w.is_empty());
        // A registered window parked on another Desktop: waits, whatever the platform said.
        let listing: Vec<Listed> = foreign.iter().copied().chain(on_desktop_1()).collect();
        let mut w = Watch::new(&reg());
        let mut all = judged.clone();
        all.insert(1, Release::NotOurs);
        for ms in [0, 500, 1000, 5000] {
            assert!(w.leave_alone(&all).is_empty());
            w.observe(t0 + Duration::from_millis(ms), &listing, &displays());
            assert_eq!(w.waiting().map(|x| x.id).collect::<Vec<_>>(), vec![1], "still waits");
        }
    }

    /// 8J: a window the app registered is never let go as a hidden tab (it waits as before).
    #[test]
    fn a_registered_window_is_never_let_go_as_a_hidden_tab() {
        let t0 = Instant::now();
        let mut w = Watch::new(&reg());
        assert!(w.hidden_tabs(&[1].into()).is_empty());
        w.observe(t0, &on_desktop_1(), &displays());
        assert_eq!(w.waiting().map(|x| x.id).collect::<Vec<_>>(), vec![1], "still waits");
        assert!(w.hidden_tabs(&[1].into()).is_empty());
        assert!(!w.is_empty());
    }
}
