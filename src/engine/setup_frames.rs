//! Windows keep the frames set for each screen setup (`[displays] frames_per_setup`; the pure
//! half: the model keeps and puts back the frames, `model/setup_frames.rs`; the controller
//! drives them from its tick).
//!
//! **The case.** An external screen unplugged, macOS only squeezes the windows onto the screen
//! left: the sizes and places set on the laptop's screen alone earlier are lost, and so are the
//! external screen's when it comes back to a setup macOS does not remember.
//!
//! **A screen setup** ([`SetupKey`]) is the set of connected screens by identity
//! ([`ScreenKey`](super::displays::ScreenKey)), each with its size in points: the same screens
//! in another order are the same setup, a resolution change is another one. A setup with a
//! screen of no known identity is none: nothing is kept for it.
//!
//! **Remembered.** Each standard window keeps one frame per setup ([`SetupFrame`]): the screen
//! it is on (by identity) and its frame relative to that screen's top-left corner, so an
//! arrangement moved keeps it. A hidden stage's window counts by its kept frame (its restore
//! frame). Frames are taken on the tick while the setup stands ([`Recorder::offer`]): a
//! snapshot is kept only once [`QUIET`] has passed with the same setup and the live screens
//! still the model's, so a squeeze macOS makes just before the change is heard never becomes
//! the old setup's frame. Nothing is taken while a display change settles: macOS's moves then
//! are not the person's. At most [`KEEP_SETUPS`] setups per window, the most recent first.
//!
//! **Put back.** When a display change settles ([`Recorder::settled`]) into a setup seen
//! before, each window with a frame for it goes back there, on the same screen: a shown one is
//! moved, a hidden one gets it as its restore frame (shown there next time). A window the
//! person moved, resized or focused by a click since the change (until the put-back, which
//! waits past the settle while a switch, a drag or a full-screen app's Space holds it) stays
//! where it is, and so does one on another screen or on a Desktop not shown: nothing moves a
//! window between screens, Desktops or stages to fit a frame. A setup not seen before keeps
//! macOS's way, and its frames are remembered from there.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::geometry::Rect;
use super::model::{Display, WindowId};

/// `[displays] frames_per_setup`'s default: on. Apple's keeps no frame per setup (measured
/// 11-19, `scripts/probe_apple.py frames-setup`; UX-SPEC): a size set on one set of screens
/// is lost at the next change, which is a gap this fills (the owner's word, 11-26).
pub const ON_BY_DEFAULT: bool = true;

/// The most setups a window keeps a frame for; the least recent goes first.
pub const KEEP_SETUPS: usize = 8;

/// How long a snapshot of the frames waits before it is kept: a change heard within that
/// time drops it (macOS moves windows as a screen goes, and the change may be heard a moment
/// later).
pub const QUIET: Duration = Duration::from_secs(3);

/// A screen setup: the connected screens by identity, each with its size, in a fixed order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SetupKey(String);

impl SetupKey {
    /// The setup of `displays`; `None` when one has no known identity, or none is given.
    pub fn of(displays: &[Display]) -> Option<Self> {
        if displays.is_empty() || displays.iter().any(|d| !d.key.known() || !d.frame.is_finite()) {
            return None;
        }
        let mut screens: Vec<String> = displays
            .iter()
            .map(|d| format!("{}@{}x{}", d.key.text(), d.frame.w.round() as i64, d.frame.h.round() as i64))
            .collect();
        screens.sort();
        Some(Self(screens.join(" ")))
    }

    /// The key as `state.json` writes it.
    pub fn text(&self) -> &str {
        &self.0
    }

    /// How many screens it has.
    pub fn screens(&self) -> usize {
        self.0.split(' ').count()
    }
}

/// A window's frame in one setup, as `state.json` keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupFrame {
    /// The setup ([`SetupKey::text`]).
    pub setup: String,
    /// The screen the window is on ([`crate::engine::displays::ScreenKey::text`]).
    pub screen: String,
    /// Its frame relative to that screen's top-left corner.
    pub frame: Rect,
}

/// `frame` relative to the top-left corner of `screen` (a display's full frame).
pub fn relative(frame: Rect, screen: Rect) -> Rect {
    frame.translate(-screen.x, -screen.y)
}

/// A frame kept relative to a screen ([`relative`]) back in global coordinates on `screen`.
pub fn absolute(frame: Rect, screen: Rect) -> Rect {
    frame.translate(screen.x, screen.y)
}

/// Keeps `entry` in a window's list, the most recent first: its setup's older entry goes, and
/// past [`KEEP_SETUPS`] the least recent. True when the list changed.
pub fn keep(list: &mut Vec<SetupFrame>, entry: SetupFrame) -> bool {
    if list.first() == Some(&entry) {
        return false;
    }
    list.retain(|e| e.setup != entry.setup);
    list.insert(0, entry);
    list.truncate(KEEP_SETUPS);
    true
}

/// The frame kept in `list` for `setup`, if any.
pub fn kept<'a>(list: &'a [SetupFrame], setup: &SetupKey) -> Option<&'a SetupFrame> {
    list.iter().find(|e| e.setup == setup.text())
}

/// What a put-back did, for its log line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PutBack {
    /// Windows put back at their frame (moved, or given it as their restore frame).
    pub put_back: usize,
    /// Windows left alone: the person moved, resized or focused them since the change.
    pub touched: usize,
    /// Windows left alone: on another screen than their frame's, or on a Desktop not shown.
    pub elsewhere: usize,
    /// Windows with no frame kept for the setup.
    pub none_kept: usize,
}

/// What a display change settling found ([`crate::engine::model::Model::setup_settled`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// A screen of no known identity: no setup, nothing kept.
    Unknown,
    /// A setup not seen before (its screens): macOS's frames stay, and are kept from now.
    New(usize),
    /// A setup seen before (its screens): the windows were put back.
    Known(usize, PutBack),
}

/// A snapshot of the frames waiting its [`QUIET`] before it is kept.
#[derive(Debug, Clone)]
struct Snapshot {
    setup: SetupKey,
    taken: Instant,
    frames: Vec<(WindowId, SetupFrame)>,
}

/// The timing half: when frames are taken, when a display change settles, and which windows
/// the person touched since. Pure: the caller passes the time.
#[derive(Debug, Clone, Default)]
pub struct Recorder {
    waiting: Option<Snapshot>,
    changed_at: Option<Instant>,
    touched: HashSet<WindowId>,
}

impl Recorder {
    /// The displays changed (heard at `heard`): the waiting snapshot is dropped and the settle
    /// starts again. The windows touched since an earlier change not settled yet stay touched
    /// (a screen plugged in often comes as two changes: the screen, then its mode).
    pub fn display_changed(&mut self, heard: Instant) {
        self.waiting = None;
        self.changed_at = Some(heard);
    }

    /// True within `settle` of the last display change.
    pub fn settling(&self, now: Instant, settle: Duration) -> bool {
        self.changed_at.is_some_and(|t| now.saturating_duration_since(t) < settle)
    }

    /// The person moved, resized or focused by a click window `id`: from a display change
    /// until its put-back ([`Self::settled`]), the put-back leaves it alone. Past the settle
    /// too: the put-back waits for a switch, a Desktop change, a drag or a full-screen app's
    /// Space, and a window grabbed meanwhile is the person's.
    pub fn touch(&mut self, id: WindowId) {
        if self.changed_at.is_some() {
            self.touched.insert(id);
        }
    }

    /// Once, when the settle of a change is over at `now`: the windows touched since the
    /// change. `None` otherwise.
    pub fn settled(&mut self, now: Instant, settle: Duration) -> Option<HashSet<WindowId>> {
        let at = self.changed_at?;
        if now.saturating_duration_since(at) < settle {
            return None;
        }
        self.changed_at = None;
        self.waiting = None;
        Some(std::mem::take(&mut self.touched))
    }

    /// The tick's offer at `now` with the setup standing (`setup`, `None` when unknown) and
    /// `steady` when the live screens are the ones the model has: the snapshot taken at least
    /// [`QUIET`] before for the same setup comes back to be kept, and `take` makes the next
    /// one. Nothing is kept while settling, unsteady or with no setup: the waiting snapshot is
    /// dropped then.
    pub fn offer(
        &mut self,
        setup: Option<&SetupKey>,
        steady: bool,
        now: Instant,
        settle: Duration,
        take: impl FnOnce() -> Vec<(WindowId, SetupFrame)>,
    ) -> Option<Vec<(WindowId, SetupFrame)>> {
        let Some(setup) = setup.filter(|_| steady && !self.settling(now, settle)) else {
            self.waiting = None;
            return None;
        };
        match &self.waiting {
            Some(w) if w.setup == *setup && now.saturating_duration_since(w.taken) < QUIET => None,
            Some(w) if w.setup == *setup => {
                let ready = self.waiting.take().map(|w| w.frames);
                self.waiting = Some(Snapshot { setup: setup.clone(), taken: now, frames: take() });
                ready
            }
            _ => {
                self.waiting = Some(Snapshot { setup: setup.clone(), taken: now, frames: take() });
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::corner::park::Corner;
    use crate::engine::displays::ScreenKey;
    use crate::engine::model::DisplayId;

    const SETTLE: Duration = Duration::from_secs(10);

    fn screen(id: u32, key: ScreenKey, frame: Rect) -> Display {
        Display { id: DisplayId(id), key, frame, visible: frame, corner: Corner::BottomRight }
    }

    fn external() -> Display {
        screen(1, ScreenKey::new(0x610, 0xae2f, 0x4d6), Rect::new(0.0, 0.0, 2560.0, 1440.0))
    }

    fn laptop() -> Display {
        screen(2, ScreenKey::new(0x610, 0xa050, 0), Rect::new(-1800.0, 271.0, 1800.0, 1169.0))
    }

    fn entry(setup: &str, x: f64) -> SetupFrame {
        SetupFrame { setup: setup.into(), screen: "s".into(), frame: Rect::new(x, 0.0, 100.0, 100.0) }
    }

    #[test]
    fn the_same_screens_in_another_order_are_the_same_setup_and_a_resolution_another() {
        let a = SetupKey::of(&[external(), laptop()]).expect("known screens");
        assert_eq!(SetupKey::of(&[laptop(), external()]), Some(a.clone()), "order does not count");
        assert_eq!(a.screens(), 2);
        let mut scaled = laptop();
        scaled.frame.w = 1512.0;
        scaled.frame.h = 982.0;
        assert_ne!(SetupKey::of(&[external(), scaled]), Some(a.clone()), "a resolution change is another setup");
        let mut moved = laptop();
        moved.frame.x = 2560.0;
        assert_eq!(SetupKey::of(&[external(), moved]), Some(a.clone()), "the arrangement does not count");
        assert_ne!(SetupKey::of(&[laptop()]), Some(a), "the laptop alone is another setup");
        let unknown = screen(3, ScreenKey::NONE, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert_eq!(SetupKey::of(&[laptop(), unknown]), None, "a screen of no known identity: no setup");
        assert_eq!(SetupKey::of(&[]), None);
    }

    #[test]
    fn a_frame_is_kept_relative_to_its_screen() {
        let f = Rect::new(-1700.0, 400.0, 900.0, 700.0);
        let rel = relative(f, laptop().frame);
        assert_eq!(rel, Rect::new(100.0, 129.0, 900.0, 700.0));
        let mut moved = laptop();
        moved.frame.x = 2560.0;
        assert_eq!(absolute(rel, moved.frame), Rect::new(2660.0, 400.0, 900.0, 700.0), "the screen moved, the frame with it");
    }

    #[test]
    fn a_window_keeps_the_eight_most_recent_setups() {
        let mut list = Vec::new();
        for i in 0..10 {
            assert!(keep(&mut list, entry(&format!("s{i}"), 0.0)));
        }
        assert_eq!(list.len(), KEEP_SETUPS);
        assert_eq!(list[0].setup, "s9", "the most recent first");
        assert!(list.iter().all(|e| e.setup != "s0" && e.setup != "s1"), "the least recent went");
        assert!(!keep(&mut list, entry("s9", 0.0)), "the same frame again changes nothing");
        assert!(keep(&mut list, entry("s3", 5.0)), "an older setup's new frame comes first");
        assert_eq!((list[0].setup.as_str(), list.len()), ("s3", KEEP_SETUPS));
        assert_eq!(list.iter().filter(|e| e.setup == "s3").count(), 1, "one frame per setup");
        let s3 = SetupKey("s3".into());
        assert_eq!(kept(&list, &s3).map(|e| e.frame.x), Some(5.0));
    }

    #[test]
    fn a_snapshot_is_kept_only_after_its_quiet_with_the_setup_standing() {
        let t0 = Instant::now();
        let a = SetupKey("a".into());
        let mut r = Recorder::default();
        let frames = |x: f64| vec![(WindowId(1), entry("a", x))];
        assert_eq!(r.offer(Some(&a), true, t0, SETTLE, || frames(1.0)), None, "the first snapshot waits");
        assert_eq!(r.offer(Some(&a), true, t0 + Duration::from_secs(1), SETTLE, || frames(2.0)), None, "still in its quiet");
        let kept = r.offer(Some(&a), true, t0 + QUIET, SETTLE, || frames(3.0));
        assert_eq!(kept, Some(frames(1.0)), "the first, once its quiet passed");
        // A change heard drops the snapshot waiting: it may hold macOS's squeeze.
        r.display_changed(t0 + QUIET + Duration::from_secs(1));
        assert!(r.settling(t0 + QUIET + Duration::from_secs(2), SETTLE));
        assert_eq!(r.offer(Some(&a), true, t0 + QUIET * 3, SETTLE, || frames(4.0)), None, "nothing within the settle");
        let after = t0 + QUIET + Duration::from_secs(1) + SETTLE;
        assert_eq!(r.settled(after, SETTLE), Some(HashSet::new()), "the settle is over, once");
        assert_eq!(r.settled(after, SETTLE), None);
        assert_eq!(r.offer(Some(&a), true, after, SETTLE, || frames(5.0)), None, "a new snapshot waits again");
        assert_eq!(r.offer(Some(&a), false, after + QUIET, SETTLE, || frames(6.0)), None, "live screens not the model's: dropped");
        assert_eq!(r.offer(Some(&a), true, after + QUIET * 2, SETTLE, || frames(7.0)), None);
        let b = SetupKey("b".into());
        assert_eq!(r.offer(Some(&b), true, after + QUIET * 4, SETTLE, || frames(8.0)), None, "another setup's snapshot starts over");
        assert_eq!(r.offer(None, true, after + QUIET * 6, SETTLE, || frames(9.0)), None, "no setup: nothing");
    }

    #[test]
    fn windows_touched_from_a_change_until_its_put_back_are_left_alone() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.touch(WindowId(9));
        r.display_changed(t0);
        r.touch(WindowId(1));
        // Past the settle, the put-back held back by a drag: a window grabbed then counts.
        r.touch(WindowId(2));
        let touched = r.settled(t0 + SETTLE + Duration::from_secs(2), SETTLE).expect("settled");
        assert_eq!(touched, HashSet::from([WindowId(1), WindowId(2)]), "the touch before the change does not count");
        r.touch(WindowId(4));
        r.display_changed(t0 + SETTLE * 2);
        assert_eq!(r.settled(t0 + SETTLE * 3, SETTLE), Some(HashSet::new()), "a touch with no change under way does not count");
    }

    #[test]
    fn a_second_change_keeps_the_windows_touched_since_the_first() {
        // A screen plugged in: the screen, then its mode, a few seconds apart.
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.display_changed(t0);
        r.touch(WindowId(3));
        r.display_changed(t0 + Duration::from_secs(2));
        assert_eq!(r.settled(t0 + SETTLE, SETTLE), None, "the settle runs from the last change");
        assert_eq!(r.settled(t0 + SETTLE + Duration::from_secs(2), SETTLE), Some(HashSet::from([WindowId(3)])));
    }
}
