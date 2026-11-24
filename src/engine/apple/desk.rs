//! What the Apple mode reads and does on the desk, as one trait ([`AppleDesk`]): the platform
//! implements it over WindowManager's Accessibility, the window server's list and the AX
//! focus (`platform::apple::desk`), the tests over a fake WindowManager
//! (`engine::testkit::apple`). Everything above it (the mirror, following, the plans and
//! routes) runs the same against either.

use serde::Serialize;

use super::mirror::{Listed, ListedWindow};
use crate::engine::geometry::{Point, Rect};

/// The user's input as it stands, for the gate a posted step waits on
/// ([`super::gate`]): a posted click never fights the user. The keys and the pointer are the
/// person's own (the hardware's state), never a posted event's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct InputNow {
    /// A mouse button is down.
    pub button_down: bool,
    /// ⌘, ⌥ or ⌃ is held.
    pub command_keys: bool,
    /// Shift is held (by the person: a posted Shift-click then uses it as it is).
    pub shift: bool,
    /// How long the pointer has been still, in ms.
    pub pointer_still_ms: u64,
    /// How long since the last key event, in ms.
    pub key_idle_ms: u64,
    /// The screen is locked.
    pub locked: bool,
    /// Mission Control, App Exposé or Show Desktop shows.
    pub overview: bool,
}

impl InputNow {
    #[cfg(test)]
    /// The person idle: nothing held, the pointer and the keys still for a second.
    pub fn idle() -> Self {
        Self { pointer_still_ms: 1000, key_idle_ms: 1000, ..Self::default() }
    }
}

/// One Shift-click of a press: window `window`'s thumbnail at global point `at`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Press {
    /// The window whose thumbnail is clicked (the guard, [`press_lands`]: the topmost window
    /// there is its twin).
    pub window: u32,
    /// Where (global, top-left).
    pub at: Point,
    /// Its thumbnail's bounds as the read had them (`None`: not checked).
    pub frame: Option<Rect>,
}

/// The gesture's guard for one click of a Shift press, just before it (live 11-17: Apple's
/// lays a strip item out again around the thumbnails a press took, and a later click may meet
/// another window's thumbnail): `list` the window list now, front to back, each window's
/// `onscreen` saying whether it is drawn where it is listed (the platform counts clear ones and
/// macOS's own that let clicks through off), `manager` WindowManager's pid. The topmost window
/// at the point, WindowManager's app icons passed over (drawn at their middle only, which no
/// point aims at), must be WindowManager's twin of the press's window: listed right above it,
/// with its bounds, at the bounds the read had (`Press::frame`). `Err` says what is there
/// instead; the click is then left for the next read.
pub fn press_lands(list: &[ListedWindow], manager: i32, press: &Press) -> Result<(), String> {
    let (x, y) = (press.at.x, press.at.y);
    let icon = |w: &ListedWindow| w.pid == manager && w.layer == 0 && super::mirror::icon_sized(w.bounds);
    let Some(i) = list.iter().position(|w| w.onscreen && w.layer >= 0 && w.bounds.contains(press.at) && !icon(w)) else {
        return Err(format!("no window at {x:.0},{y:.0}"));
    };
    let top = &list[i];
    if top.pid != manager || top.layer != 0 {
        return Err(format!("the topmost window at {x:.0},{y:.0} is not the one meant (pid {}, layer {})", top.pid, top.layer));
    }
    if let Some(f) = press.frame
        && !super::mirror::same_bounds(f, top.bounds)
    {
        return Err(format!("window {}'s thumbnail moved since the read ({:.0},{:.0} {:.0}x{:.0} there now)", press.window, top.bounds.x, top.bounds.y, top.bounds.w, top.bounds.h));
    }
    match list.get(i + 1) {
        Some(w) if w.id == press.window && super::mirror::same_bounds(w.bounds, top.bounds) => Ok(()),
        Some(w) if super::mirror::same_bounds(w.bounds, top.bounds) => Err(format!("the thumbnail at {x:.0},{y:.0} is window {}'s, not {}'s", w.id, press.window)),
        _ => Err(format!("WindowManager's window at {x:.0},{y:.0} is over no thumbnail of window {}", press.window)),
    }
}

/// A posted gesture (the routes `shift_click` and `drag`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Posted {
    /// Shift-clicks on thumbnails within one Shift press (a probe of 11-16: a stage of two,
    /// both thumbnails Shift-clicked 50 ms apart from one settled read, both joined): a real
    /// Shift key-down about 12 ms before the first (none when the person holds Shift:
    /// `own_shift` false), each click with no hold and the pointer warped back at once, the
    /// next [`CLICK_GAP`] later, Shift up about 12 ms after the last.
    Clicks {
        /// The clicks, in order (back to front within an item, the items top to bottom: each
        /// lands its window on top, so the windows keep their order).
        presses: Vec<Press>,
        /// A Shift key-down and key-up of ours around them.
        own_shift: bool,
    },
    /// A drag of window `window`'s title bar onto the strip.
    Drag {
        /// The window dragged (the guard: the topmost window at `from` is it).
        window: u32,
        /// Where it starts (its title bar).
        from: Point,
        /// Where it ends (the strip's column).
        to: Point,
        /// Its frame before, set back through Accessibility when the person's own input ends
        /// the drag.
        frame: Rect,
    },
}

/// Between two Shift-clicks of one press (the probe of 11-16 that brought a stage of two in
/// with one Shift press, 50 ms apart). Short enough that every click lands before Apple lays
/// out the strip item again around the thumbnails left: live 11-16 15:20:35, a stage of three
/// clicked 50 ms apart, the third click (100 ms in) missed its thumbnail, already moved.
pub const CLICK_GAP: std::time::Duration = std::time::Duration::from_millis(15);

impl Posted {
    /// One Shift-click on window `window`'s thumbnail at `at`.
    pub fn click(window: u32, at: Point, own_shift: bool) -> Self {
        Self::Clicks { presses: vec![Press { window, at, frame: None }], own_shift }
    }

    /// The window the gesture is for (a press's first).
    pub fn window(&self) -> u32 {
        match self {
            Self::Clicks { presses, .. } => presses.first().map_or(0, |p| p.window),
            Self::Drag { window, .. } => *window,
        }
    }
}

/// How a posted gesture ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Landed {
    /// Every event posted; Apple's answer is for the mirror to see.
    Posted,
    /// Nothing posted (the guard: the point is not where it should be, the button down).
    Refused(String),
    /// The person's own input ended it midway (a drag): nothing more posted, the window's
    /// frame set back.
    Interrupted(String),
}

/// The desk the Apple mode reads and acts on.
pub trait AppleDesk {
    /// The window server's list now.
    fn listed(&mut self) -> Listed;
    /// The user's input as it stands.
    fn input(&mut self) -> InputNow;
    /// Window `window` raised, made main and its app frontmost (the focus route): Apple's
    /// switches to its stage.
    fn focus(&mut self, window: u32) -> Result<(), String>;
    /// Starts a gesture: `Ok` once it is under way (the real desk posts it on a thread of
    /// its own; the platform takes how it ended), `Err` when it cannot start.
    fn post(&mut self, posted: &Posted) -> Result<(), String>;
    /// App `pid` shown again without activating it (hidden by us at its activation): Apple's
    /// puts the window it reopened meanwhile into the strip.
    fn unhide(&mut self, pid: i32) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::apple::mirror::{Arrangement, fold};
    use crate::engine::testkit::apple::{FakeWindowManager, MANAGER};
    use crate::engine::testkit::display;

    /// Four stages of one window each in the strip, top to bottom 1, 2, 3, 4, and 5 in front,
    /// read at rest.
    fn four() -> (FakeWindowManager, Arrangement) {
        let mut f = FakeWindowManager::new();
        for (pid, id) in [(40, 4), (30, 3), (20, 2), (10, 1), (50, 5)] {
            f.open_app(pid, &[id]);
        }
        let mut a = Arrangement::default();
        for _ in 0..40 {
            f.advance(50);
            a = fold(&a, None, &f.listed(), &f.window_ids(), &display(), f.now());
        }
        assert!(a.settled && a.current == vec![5], "{a:?}");
        (f, a)
    }

    fn press(a: &Arrangement, w: u32) -> Press {
        Press { window: w, at: a.click_at[&w], frame: a.marks.get(&w).map(|m| m.bounds) }
    }

    #[test]
    fn a_click_lands_only_on_the_thumbnail_the_read_had_there() {
        let (mut f, a) = four();
        let (one, three) = (press(&a, 1), press(&a, 3));
        assert_eq!(press_lands(&f.listed().windows, MANAGER, &one), Ok(()));
        assert_eq!(press_lands(&f.listed().windows, MANAGER, &three), Ok(()));
        // The first click takes window 1; Apple's lays the strip out again: 2, 3 and 4 move up
        // a slot each, and window 4's thumbnail is where window 3's was, at its bounds.
        f.relayout = true;
        f.post(&Posted::Clicks { presses: vec![one], own_shift: true }).expect("posted");
        f.advance(2000);
        let now = f.listed();
        let why = press_lands(&now.windows, MANAGER, &three).expect_err("not window 3's any more");
        assert!(why.contains("window 4's, not 3's"), "{why}");
        // Window 2's thumbnail, a slot up now: its twin there is not at the bounds the read had.
        let two = press(&a, 2);
        let at_two = Press { at: Point::new(two.at.x, two.at.y - 150.0), ..two };
        let why = press_lands(&now.windows, MANAGER, &at_two).expect_err("moved");
        assert!(why.contains("moved"), "{why}");
        // Another window over the point.
        let mut covered = now.clone();
        covered.windows.insert(0, ListedWindow { id: 77, pid: 7, layer: 0, bounds: Rect::new(0.0, 0.0, 400.0, 900.0), onscreen: true });
        assert!(press_lands(&covered.windows, MANAGER, &three).expect_err("covered").contains("not the one meant"));
    }
}
