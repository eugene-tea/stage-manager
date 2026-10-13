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
//! The window server's list alone is never final. While a Space slides in it may list a
//! window at an in-between frame, or not at all, for a pass (hands-free Desktop run 2, 10-12:
//! the guardian took one such listing of a window still parked on Desktop 2 for "back" and
//! ended; only the next launch brought it back). So the list counts as settled only
//! [`LIST_SETTLE`] after the last change in the set of windows on screen (the controller's
//! rule after a Space change), and an outside-the-park-zone sighting off screen (a Desktop
//! not shown) never counts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, Instant};

use super::geometry::Rect;
use super::recovery::{Listed, Registered, ScanDisplays, is_stranded, safe_restore};

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
    /// How it looked in the latest pass (`None` before the first).
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
}

impl Watch {
    /// A watch over the app's registered windows; others join when found in the park zone.
    pub fn new(registered: &[Registered]) -> Self {
        let mut w = Self { registered: registered.to_vec(), ..Self::default() };
        for r in registered {
            w.waiting.insert(r.id, waiting(r.id, r.pid, Some(r.restore)));
        }
        w
    }

    /// True when no window waits.
    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    /// Every waiting window, by id.
    pub fn waiting(&self) -> impl Iterator<Item = &Waiting> {
        self.waiting.values()
    }

    /// Waiting windows on screen in the latest pass (Accessibility can reach them).
    pub fn shown(&self) -> Vec<Waiting> {
        self.waiting.values().filter(|w| w.look.is_some_and(|l| l.shown())).cloned().collect()
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
        if self.on_screen.as_ref() != Some(&on_screen) {
            // The first pass counts as a change too: nothing is known about what came before.
            self.changed_at = Some(now);
            events.push(Event::ScreenChanged { on_screen: on_screen.len() });
            self.on_screen = Some(on_screen);
        }
        let settled = self.settled(now);
        for l in listed.iter().filter(|l| is_stranded(l.bounds, displays)) {
            if self.waiting.contains_key(&l.id) {
                continue;
            }
            let reg = self.registered.iter().find(|r| r.id == l.id);
            if reg.is_some() || l.allowed {
                self.waiting.insert(l.id, waiting(l.id, l.pid, reg.map(|r| r.restore)));
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
                    if is_stranded(l.bounds, displays) {
                        w.out_since = None;
                        Look::Parked { shown: l.onscreen, frame: l.bounds }
                    } else if !l.onscreen {
                        w.out_since = None;
                        Look::OutAway { frame: l.bounds }
                    } else if !settled {
                        w.out_since = None;
                        Look::OutUnsettled { frame: l.bounds }
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
    pub fn read_through_ax(&mut self, id: u32, frame: Rect, moved: bool, displays: &ScanDisplays) -> Option<Event> {
        if is_stranded(frame, displays) {
            return None;
        }
        let w = self.waiting.remove(&id)?;
        let why = if moved { Release::BroughtBack { frame } } else { Release::ReadBack { frame } };
        Some(Event::LetGo { id, pid: w.pid, why })
    }
}

fn waiting(id: u32, pid: i32, registered: Option<Rect>) -> Waiting {
    Waiting { id, pid, registered, look: None, out_since: None, missing_since: None }
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
        ScanDisplays { main: MAIN, visible: VISIBLE, others: vec![] }
    }

    fn parked() -> Rect {
        park_frame(Size::new(600.0, 392.0), MAIN)
    }

    fn reg() -> Vec<Registered> {
        vec![Registered { id: 1, pid: 7, restore: RESTORE, child: false }]
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
        let ev = w.observe(t0, &[allowed, not_allowed], &displays());
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

    #[test]
    fn registered_restore_frames_are_made_safe() {
        let r = [Registered { id: 1, pid: 7, restore: parked(), child: false }];
        let w = Watch::new(&r);
        let f = w.waiting().next().and_then(|x| Watch::restore_frame(x, &displays())).unwrap_or_default();
        assert!(!is_stranded(f, &displays()) && VISIBLE.contains_rect(&f), "{f:?}");
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
}
