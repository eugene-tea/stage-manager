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
//!   another Desktop of the main display listed on screen (the Desktop sliding in), a window
//!   not known yet that a saved Desktop not visited since launch names (the same, right after
//!   a relaunch), or this Desktop's parked windows listed out of the park zone, all moved
//!   sideways by the same distance ([`crate::engine::corner::corner_patch::desktop_slides`]); or
//! - the activated app's focused window (asked of the app only when it has a window on a
//!   Desktop not shown, known or saved on one not visited yet, [`asks_focus`]) is a window of
//!   another Desktop: the activation belongs to that Desktop (12:00:29: no slide listed yet at
//!   the activation).
//!
//! After a relaunch a Desktop not shown yet has no known windows until its first visit: only
//! the saved file names them (live 11-15 16:34:28, the relaunch's first ⌃← to Desktop 2: the
//! slide went unseen, the activation of Desktop 2's front app switched to its stage on
//! Desktop 1, and macOS slid back there).
//!
//! The platform holds it with the focus replay ([`crate::engine::focus_replay`]), where the
//! Space change's arrival takes it: on the new Desktop Apple's rule applies (its stage there,
//! or nothing when macOS already shows it). With no Space change after it (a slide given up,
//! or an app's stale focus), it is decided as any activation once [`SLIDE_ACTIVATION_HOLD`]
//! has passed and the list shows no slide, never later than [`SLIDE_ACTIVATION_CAP`]
//! ([`SlideHold::step`]). An activation with no such sign is decided at once, as before.
//!
//! **The question not answered** ([`AskAgain`]): the app is asked within 50 ms, on the WM
//! thread, and one waking for its activation may not say in time (the owner's Messages, live
//! 11-23 17:43:29: ⌘Tab to it from another Desktop, its one window hidden on Desktop 2; the
//! same day it took 55 to 79 ms at its activations). With nothing of the app on the Desktop
//! shown the activation was then held for a Space change that never came: a window the engine
//! keeps off every Desktop takes macOS nowhere, and no focus notice follows when the app's
//! key window is the same as before. The app was in front with no window. So the question is
//! asked again, [`ASK_AGAIN_AFTER`] later and then after twice as long each time
//! ([`ASK_TRIES`] in all), while the activation is still what is held and no Space change
//! was notified; answered, the activation is decided again with it.

use std::time::{Duration, Instant};

use super::focus_replay::Wanted;
use super::geometry::Rect;
use super::model::{Model, Pid, WindowId, WindowRole};

/// A held activation is decided this long after it came when no Space change was notified
/// meanwhile and the list shows no slide (the notice comes ~20–200 ms after the activation).
pub const SLIDE_ACTIVATION_HOLD: Duration = Duration::from_millis(500);

/// Never held longer than this, whatever the list shows (a swipe held half-way).
pub const SLIDE_ACTIVATION_CAP: Duration = Duration::from_millis(1500);

/// An activated app that did not say its focused window is asked again this long after its
/// activation was decided (the first question's 50 ms are over by then); each later pause is
/// twice the one before ([`AskAgain::missed`]).
pub const ASK_AGAIN_AFTER: Duration = Duration::from_millis(60);

/// How often it is asked again: 60, 180, 420 and 900 ms after the activation, a bounded call
/// each (four short calls in a second for an app that never answers).
pub const ASK_TRIES: u8 = 4;

/// From the activation to its last question ([`ASK_TRIES`]): the cap its margin is told
/// against (`FOCUS_ASK_AGAIN`).
pub const ASK_AGAIN_SPAN: Duration = Duration::from_millis(900);

/// An activation whose app did not say its focused window, asked again (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AskAgain {
    /// The app.
    pub pid: Pid,
    /// When its activation was decided.
    pub came: Instant,
    /// The Space changes notified then.
    pub changes: u64,
    /// The questions asked again so far.
    pub asked: u8,
    /// When the next one is due.
    pub next: Instant,
}

/// What an [`AskAgain`] does at a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// Not due yet.
    Wait,
    /// Ask the app now.
    Now,
    /// No longer asked, and why.
    Over(&'static str),
}

impl AskAgain {
    /// For app `pid`'s activation decided at `came`, with `changes` Space changes notified.
    pub fn new(pid: Pid, came: Instant, changes: u64) -> Self {
        Self { pid, came, changes, asked: 0, next: came + ASK_AGAIN_AFTER }
    }

    /// At `now` with `changes` Space changes notified; `held`: the focus replay still holds
    /// this app's activation (nothing else came since: no other activation, focus or act of
    /// the user's).
    pub fn step(&self, now: Instant, changes: u64, held: bool) -> Ask {
        if changes != self.changes {
            Ask::Over("a Space change was notified: its arrival asks")
        } else if !held {
            Ask::Over("its activation is no longer what is held")
        } else if now >= self.next {
            Ask::Now
        } else {
            Ask::Wait
        }
    }

    /// The app did not say again, at `now`: true when it is asked once more (twice the pause
    /// before), false after [`ASK_TRIES`].
    pub fn missed(&mut self, now: Instant) -> bool {
        self.asked += 1;
        self.next = now.max(self.next) + ASK_AGAIN_AFTER * (1u32 << u32::from(self.asked.min(ASK_TRIES)));
        self.asked < ASK_TRIES
    }
}

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
/// on screen there (sticky ones are on every Desktop), a window not known yet that a saved
/// Desktop of it not visited since launch names ([`Model::saved_on_unvisited_desktop`]: its
/// windows are known only from its first visit), or this Desktop's parked windows listed
/// slid aside together ([`crate::engine::corner::corner_patch::desktop_slides`]; parked windows not
/// listed at all are not counted: a hidden app's are not listed either).
pub fn slide_listed(m: &Model, listed: &[(u32, Rect)]) -> bool {
    let main = *m.main_display();
    let here = m.current_space(main.id);
    let on_main = |b: &Rect| b.intersect(&main.frame).is_some_and(|i| !i.is_degenerate());
    let elsewhere = listed.iter().any(|(id, b)| {
        on_main(b)
            && (m.window(WindowId(*id)).is_some_and(|w| {
                w.meta.role == WindowRole::Standard && !w.sticky && w.space != here && m.space_display(w.space) == main.id
            }) || m.saved_on_unvisited_desktop(WindowId(*id), main.id))
    });
    if elsewhere {
        return true;
    }
    let parked_here: Vec<Rect> = listed
        .iter()
        .filter(|(id, _)| m.window(WindowId(*id)).is_some_and(|w| w.parked && w.space == here && !w.sticky))
        .map(|(_, b)| *b)
        .collect();
    super::corner::corner_patch::desktop_slides(0, &parked_here, false, main.frame, main.visible, main.corner)
}

/// The listed windows `listed` without those out for Mission Control (`out`): Mission
/// Control lays them out on screen, near an edge too, and they are no Desktop's slide.
/// Live 11-19 (mission-pick-ours): a hidden stage's window picked in Mission Control, laid out
/// at the bottom right, read as this Desktop's parked windows slid aside; its app's activation
/// was held 0.5 s and Mission Control's close carried the window to its park first.
pub fn besides_out(listed: &[(u32, Rect)], out: impl Fn(WindowId) -> bool) -> Vec<(u32, Rect)> {
    listed.iter().filter(|(id, _)| !out(WindowId(*id))).copied().collect()
}

/// Whether an activation of app `pid` asks the app for its focused window: only when the
/// app has a known standard window on a Desktop not shown, or one saved on a Desktop of the
/// main display not visited since launch ([`Model::app_saved_on_unvisited_desktop`]);
/// otherwise macOS cannot be taking the user anywhere for it, and nothing is asked.
pub fn asks_focus(m: &Model, pid: Pid) -> bool {
    m.windows().any(|w| w.meta.pid == pid && w.meta.role == WindowRole::Standard && !w.sticky && !m.is_shown_space(w.space))
        || m.app_saved_on_unvisited_desktop(pid, m.main_display().id)
}

/// Whether an activation that would switch stages on the Desktop shown waits, and why:
/// `slides` is [`slide_listed`] for the list read for it, `focused` the app's focused window
/// when asked ([`asks_focus`]): a known window of a Desktop not shown, or one not known yet
/// that a saved Desktop not visited since launch names.
pub fn activation_waits(m: &Model, slides: bool, focused: Option<WindowId>) -> Option<Wait> {
    if slides {
        return Some(Wait::Slides);
    }
    let main = m.main_display().id;
    focused
        .filter(|f| m.waits_on_another_space(*f) || m.saved_on_unvisited_desktop(*f, main))
        .map(Wait::FocusElsewhere)
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
        use crate::engine::margin::{End, wait};
        let age = now.saturating_duration_since(self.came);
        if changes != self.changes {
            wait("SLIDE_ACTIVATION_HOLD", age, SLIDE_ACTIVATION_HOLD, End::Met);
            Step::Arrived
        } else if age >= SLIDE_ACTIVATION_CAP {
            wait("SLIDE_ACTIVATION_CAP", age, SLIDE_ACTIVATION_CAP, End::Cap);
            Step::Decide
        } else if age >= SLIDE_ACTIVATION_HOLD && !sliding() {
            wait("SLIDE_ACTIVATION_HOLD", age, SLIDE_ACTIVATION_HOLD, End::Cap);
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

    #[test]
    fn an_unanswered_focus_question_is_asked_again_four_times_within_its_span() {
        let t0 = Instant::now();
        let mut a = AskAgain::new(7, t0, 3);
        assert_eq!(a.step(ms(t0, 59), 3, true), Ask::Wait);
        let mut asked = Vec::new();
        // Asked the moment each is due (the tick may be later; the pause counts from then).
        loop {
            assert_eq!(a.step(a.next, 3, true), Ask::Now);
            asked.push(a.next.duration_since(t0).as_millis() as u64);
            let at = a.next;
            if !a.missed(at) {
                break;
            }
            assert_eq!(a.step(at, 3, true), Ask::Wait, "not before its pause");
        }
        assert_eq!(asked, [60, 180, 420, 900]);
        assert_eq!(Duration::from_millis(*asked.last().expect("asked")), ASK_AGAIN_SPAN);
        assert_eq!(usize::from(ASK_TRIES), asked.len());
        // A Space change, or anything else held since, ends it.
        let a = AskAgain::new(7, t0, 3);
        assert!(matches!(a.step(ms(t0, 100), 4, true), Ask::Over(_)));
        assert!(matches!(a.step(ms(t0, 100), 3, false), Ask::Over(_)));
    }
}
