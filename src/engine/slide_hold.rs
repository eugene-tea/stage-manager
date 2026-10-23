//! An activation (or a focus) that lands while a Desktop slides, before the Space change's
//! notice: held until the Space is recognized, never acted on for the Desktop being left.
//!
//! A trackpad swipe or ⌃→ to another Desktop makes macOS activate that Desktop's frontmost
//! app near the end of the slide, before the Space change notice (10-23, 11:57:50: Safari
//! activated 185 ms before the notice; 12:00:29: VS Code 100 ms before). The model still
//! shows the Desktop being left; an app with a window there in an inactive stage was switched
//! to, its window raised and focused **there**, and macOS slid back to it: the owner was
//! "quickly forcefully moved back to the original desktop". Apple's Stage Manager never
//! touches the Desktop being left.
//!
//! So an activation that would switch stages on the Desktop shown waits
//! ([`activation_waits`]) when:
//!
//! - the window server's list read for it shows a slide ([`slide_listed`]): a known window of
//!   another Desktop of the main display listed on screen (the Desktop sliding in), or this
//!   Desktop's parked windows listed out of the park zone, all moved sideways by the same
//!   distance ([`crate::engine::corner_patch::desktop_slides`]); or
//! - the activated app's focused window (asked of the app only when it has a window on a
//!   Desktop not shown, [`asks_focus`]) is a window of another Desktop: the activation belongs
//!   to that Desktop (12:00:29: no slide listed yet at the activation).
//!
//! The platform holds it with the focus replay ([`crate::engine::focus_replay`]), where the
//! Space change's arrival takes it: on the new Desktop Apple's rule applies (its stage there,
//! or nothing when macOS already shows it). With no Space change after it (a slide given up,
//! or an app's stale focus), it is decided as any activation once [`SLIDE_ACTIVATION_HOLD`]
//! has passed and the list shows no slide, never later than [`SLIDE_ACTIVATION_CAP`]
//! ([`SlideHold::step`]). An activation with no such sign is decided at once, as before.

use std::time::{Duration, Instant};

use super::focus_replay::Wanted;
use super::geometry::Rect;
use super::model::{Model, Pid, WindowId, WindowRole};

/// A held activation is decided this long after it came when no Space change was notified
/// meanwhile and the list shows no slide (the notice comes ~20–200 ms after the activation).
pub const SLIDE_ACTIVATION_HOLD: Duration = Duration::from_millis(500);

/// Never held longer than this, whatever the list shows (a swipe held half-way).
pub const SLIDE_ACTIVATION_CAP: Duration = Duration::from_millis(1500);

/// Why an activation that would switch stages waits for the Space to be recognized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// The window server's list shows a Desktop slide.
    Slides,
    /// The activated app's focused window is a window of another Desktop.
    FocusElsewhere(WindowId),
}

impl Wait {
    /// For the log.
    pub fn name(self) -> &'static str {
        match self {
            Wait::Slides => "the window list shows a Desktop slide",
            Wait::FocusElsewhere(_) => "the app's focused window is on another Desktop",
        }
    }
}

/// Whether the on-screen windows `listed` (window server id and bounds, layer 0 only) show a
/// Desktop of the main display sliding: a known standard window of another of its Desktops
/// on screen there (sticky ones are on every Desktop), or this Desktop's parked windows listed
/// slid aside together ([`crate::engine::corner_patch::desktop_slides`]; parked windows not
/// listed at all are not counted: a hidden app's are not listed either).
pub fn slide_listed(m: &Model, listed: &[(u32, Rect)]) -> bool {
    let main = *m.main_display();
    let here = m.current_space(main.id);
    let on_main = |b: &Rect| b.intersect(&main.frame).is_some_and(|i| !i.is_degenerate());
    let elsewhere = listed.iter().any(|(id, b)| {
        on_main(b)
            && m.window(WindowId(*id)).is_some_and(|w| {
                w.meta.role == WindowRole::Standard && !w.sticky && w.space != here && m.space_display(w.space) == main.id
            })
    });
    if elsewhere {
        return true;
    }
    let parked_here: Vec<Rect> = listed
        .iter()
        .filter(|(id, _)| m.window(WindowId(*id)).is_some_and(|w| w.parked && w.space == here && !w.sticky))
        .map(|(_, b)| *b)
        .collect();
    super::corner_patch::desktop_slides(0, &parked_here, false, main.frame, main.visible, main.corner)
}

/// Whether an activation of app `pid` asks the app for its focused window: only when the
/// app has a known standard window on a Desktop not shown (otherwise macOS cannot be taking
/// the user anywhere for it, and nothing is asked).
pub fn asks_focus(m: &Model, pid: Pid) -> bool {
    m.windows().any(|w| w.meta.pid == pid && w.meta.role == WindowRole::Standard && !w.sticky && !m.is_shown_space(w.space))
}

/// Whether an activation that would switch stages on the Desktop shown waits, and why:
/// `slides` is [`slide_listed`] for the list read for it, `focused` the app's focused window
/// when asked ([`asks_focus`]).
pub fn activation_waits(m: &Model, slides: bool, focused: Option<WindowId>) -> Option<Wait> {
    if slides {
        return Some(Wait::Slides);
    }
    focused.filter(|f| m.waits_on_another_space(*f)).map(Wait::FocusElsewhere)
}

/// What a held activation (or focus) does at a look ([`SlideHold::step`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Still held.
    Wait,
    /// A Space change was notified: its arrival replays it; nothing more here.
    Arrived,
    /// No Space change came: decided now, as any activation.
    Decide,
}

/// An activation or focus held for a slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlideHold {
    /// What the focus replay holds for it.
    pub wanted: Wanted,
    /// An activation (else a window focus).
    pub activation: bool,
    /// When it came.
    pub came: Instant,
    /// The Space changes notified when it came.
    pub changes: u64,
}

impl SlideHold {
    /// At `now` with `changes` Space changes notified: [`Step::Arrived`] once one came;
    /// [`Step::Decide`] after [`SLIDE_ACTIVATION_HOLD`] when the list shows no slide
    /// (`sliding`, asked only then), or after [`SLIDE_ACTIVATION_CAP`] whatever it shows.
    pub fn step(&self, now: Instant, changes: u64, sliding: impl FnOnce() -> bool) -> Step {
        let age = now.saturating_duration_since(self.came);
        if changes != self.changes {
            Step::Arrived
        } else if age >= SLIDE_ACTIVATION_CAP || (age >= SLIDE_ACTIVATION_HOLD && !sliding()) {
            Step::Decide
        } else {
            Step::Wait
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn a_hold_ends_at_the_space_change_or_after_the_slide_within_the_cap() {
        let t0 = Instant::now();
        let h = SlideHold { wanted: Wanted::App(7), activation: true, came: t0, changes: 3 };
        assert_eq!(h.step(ms(t0, 100), 3, || unreachable!("not asked before the hold")), Step::Wait);
        assert_eq!(h.step(ms(t0, 150), 4, || false), Step::Arrived, "the notice came");
        assert_eq!(h.step(ms(t0, 500), 3, || false), Step::Decide, "no change, no slide: decided");
        assert_eq!(h.step(ms(t0, 600), 3, || true), Step::Wait, "still sliding (a swipe held)");
        assert_eq!(h.step(t0 + SLIDE_ACTIVATION_CAP, 3, || true), Step::Decide, "never past the cap");
    }
}
