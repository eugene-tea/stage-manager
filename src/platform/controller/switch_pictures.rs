//! A switch's pictures taken at the switch (1.9, S4b; every engine since round
//! switch-pictures, 11-25).
//!
//! No window has moved when a switch is sent to the strip, whatever hides it after (a
//! window-server step under the Spaces engine, an Accessibility move its app carries out under
//! the Corner engine): the windows leaving can wait on screen for their pictures. Before the
//! transition goes to the strip, every outgoing window whose picture is not fresh (the
//! pointer's press on a card took one a moment ago, or the app switcher's key) is captured,
//! and the WM thread waits for them up to [`WAIT_MAX`]. The strip then draws them over their
//! windows as before, and the windows hide behind them. With them, a window that stays shown
//! into the park corner the switch covers ([`Transition::corner`], round 8L5: a pinned
//! window), which the cover draws from its picture.
//!
//! Until 11-25 only the Spaces engine did this, and under the Corner engine the strip kept
//! the shown windows' pictures fresh every `[animation] snapshot_interval_ms` (2 s) instead,
//! from when a picture took 80 to 150 ms and a Dock click gave no time for one; that timer was
//! most of what the Corner engine cost at rest. The timer is now an option, off, the same
//! under every engine ([`crate::ui::strip_motion`]).
//!
//! The bound: through the window server's own capture (S6b, [`crate::platform::capture_fast`])
//! a window took 19 ms at the median over 627 switches of one window (26 ms at the 90th
//! percentile), two 40 ms (61 ms), three 68 ms (the app's own lines, the Spaces engine,
//! 11-14 to 11-24); one switch in 956 ended at the cap. Through ScreenCaptureKit, the fallback
//! (measured 11-14 on a 5K display, `probe-spaces capture`, a listing of the shareable content
//! first): one window ~80-90 ms from the call to its picture, each more ~18 ms, whatever
//! their size; 4 windows ~145 ms, at most 150 ms. [`WAIT_MAX`] lets 4 windows in at their
//! slowest on the fallback; a picture later than that still reaches the strip, and the moving
//! one of its window cross-fades to it, the window having hidden behind the older picture (or
//! none: the window just goes, as before).
//!
//! **The order.** An activation's early focus (`Controller::focus_now`, the app already
//! active: macOS decided any Desktop change at the activation), then these pictures, then the
//! transition to the strip; with no picture to take the strip is told first and draws while
//! the keyboard goes. A card click's focus is split after the strip's answer
//! (`split_covered`), where `focus_waits_for_show` holds a held window's focus back for its
//! show when its app has a window on a Desktop not shown. Neither path moves a window, and the
//! pictures are taken by window id, whatever is in front, so the early focus coming before
//! them changes no picture, and the wait for them only delays the strip's start, never a
//! focus.
//!
//! **Under the Corner engine** macOS has raised an activated app's windows in the park corner
//! before the app hears of the activation, and the strip's cover of the corner comes with the
//! transition: after a Dock click (nothing announced it) they show there for the pictures'
//! time longer than before, 19 to 61 ms as above. ⌘Tab's pictures are taken at the switcher's
//! key and a card's at its press, so those wait for none while they are fresh.

use std::time::Duration;

use crate::engine::geometry::Rect;
use crate::platform::capture::Target;
use crate::platform::switch_shots::{self, Taken};
use crate::ui::strip_model::{self, Transition};

/// Longest wait for a switch's outgoing pictures before the transition goes to the strip
/// anyway (module docs).
pub(super) const WAIT_MAX: Duration = Duration::from_millis(160);

/// A picture that reached the strip this recently is not taken again: the pointer's press on a
/// card took it (a Dock click or ⌘Tab has none this fresh).
const FRESH: Duration = Duration::from_millis(500);

/// Pixels a point asked for: more than any display has, so each picture is the window's own
/// pixels ([`crate::platform::capture_frame::fit_to_content`] never makes one larger).
const ASK_SCALE: f64 = 4.0;

/// The windows of `t` to capture at the switch, and how many were left out as fresh: its
/// outgoing ones, not one already in the Dock (minimized: its picture comes out of its tile),
/// and the windows that stay shown into the park corner its cover draws
/// ([`Transition::corner`]); none whose picture is fresh (`age`).
pub(super) fn targets(t: &Transition, age: impl Fn(u32) -> Option<Duration>) -> (Vec<Target>, usize) {
    let leaving = t.outgoing.iter().map(|w| (w.window.id, w.window.frame)).filter(|(id, _)| t.minimized.is_none_or(|m| m.window != *id));
    let staying = t.corner.iter().flat_map(|c| c.over.iter().copied());
    let wanted: Vec<(u32, Rect)> = leaving.chain(staying).collect();
    let stale: Vec<Target> = wanted
        .iter()
        .filter(|(id, _)| age(*id).is_none_or(|a| a > FRESH))
        .map(|(id, frame)| Target {
            id: *id,
            width: (frame.w * ASK_SCALE).round().clamp(1.0, 16384.0) as u32,
            height: (frame.h * ASK_SCALE).round().clamp(1.0, 16384.0) as u32,
        })
        .collect();
    let fresh = wanted.len() - stale.len();
    (stale, fresh)
}

/// What a switch's pictures need now ([`targets`], by how long ago each reached the strip).
pub(super) struct Wanted {
    /// The windows to capture.
    targets: Vec<Target>,
    /// Those left out: a picture of each reached the strip within [`FRESH`].
    fresh: usize,
}

impl Wanted {
    /// The pictures transition `t` needs taken now.
    pub(super) fn of(t: &Transition) -> Self {
        let (targets, fresh) = targets(t, strip_model::snapshot_age);
        Self { targets, fresh }
    }

    /// No picture to take: the transition goes to the strip at once.
    pub(super) fn none(&self) -> bool {
        self.targets.is_empty()
    }
}

/// Takes the pictures `wanted` of transition `id` now and waits for them ([`WAIT_MAX`] at
/// most); logged, also when every one was fresh.
pub(super) fn take(id: u64, wanted: Wanted) -> Taken {
    let Wanted { targets, fresh } = wanted;
    if targets.is_empty() && fresh == 0 {
        return Taken::default();
    }
    let taken = switch_shots::take(targets, WAIT_MAX);
    if taken.skipped.is_none() && taken.asked > 0 {
        let how = if taken.answered < taken.asked { crate::engine::margin::End::Cap } else { crate::engine::margin::End::Met };
        crate::engine::margin::wait("switch_pictures::WAIT_MAX", Duration::from_millis(taken.ms), WAIT_MAX, how);
    }
    tracing::debug!(
        id,
        asked = taken.asked,
        fresh,
        got = taken.got,
        answered = taken.answered,
        ms = taken.ms,
        skipped = taken.skipped,
        "transition: outgoing pictures taken at the switch"
    );
    taken
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::corner::corner_patch::{CoverPicture, SwitchCover};
    use crate::ui::strip_model::{CardWindow, Minimized, MovingWindow};

    fn moving(id: u32) -> MovingWindow {
        MovingWindow { window: CardWindow { id, pid: 1, frame: Rect::new(0.0, 0.0, 800.0, 600.0) }, ..MovingWindow::default() }
    }

    /// A Dock click's switch takes every outgoing window's picture, at its own pixels; a
    /// picture the press on a card just took, and a minimized window's, are not taken again.
    #[test]
    fn only_stale_outgoing_pictures_are_taken_at_the_switch() {
        let t = Transition {
            outgoing: vec![moving(1), moving(2), moving(3), moving(4)],
            incoming: vec![moving(9)],
            minimized: Some(Minimized { window: 4, tile: None }),
            ..Transition::default()
        };
        let age = |id| match id {
            1 => None,
            2 => Some(Duration::from_millis(120)),
            _ => Some(Duration::from_secs(5)),
        };
        let (targets, fresh) = targets(&t, age);
        let ids: Vec<u32> = targets.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 3], "never captured, or captured long ago; the incoming window is not taken");
        assert_eq!(fresh, 1, "the one the press took; the minimized one is not counted");
        assert_eq!((targets[0].width, targets[0].height), (3200, 2400), "asked above any display's scale: the window's own pixels");
    }

    /// Round switch-pictures: no timer keeps a pinned window's picture fresh, so one that
    /// stays shown into the park corner the switch covers is taken with the outgoing ones
    /// (the cover draws it from its picture, 8L5), unless a picture of it just came.
    #[test]
    fn a_window_that_stays_shown_into_the_covered_corner_is_taken_with_the_outgoing_ones() {
        let rect = Rect::new(2499.0, 1348.0, 61.0, 92.0);
        let cap = Rect::new(2240.0, 1120.0, 320.0, 320.0);
        let over = vec![(7, Rect::new(1800.0, 900.0, 740.0, 520.0)), (8, Rect::new(1200.0, 700.0, 1340.0, 720.0))];
        let t = Transition {
            outgoing: vec![moving(1)],
            incoming: vec![moving(9)],
            corner: Some(SwitchCover { screen: 1, rect, cap, picture: CoverPicture::Wallpaper, over }),
            ..Transition::default()
        };
        let old = |_| Some(Duration::from_secs(40));
        let (all, fresh) = targets(&t, old);
        assert_eq!(all.iter().map(|t| t.id).collect::<Vec<_>>(), vec![1, 7, 8]);
        assert_eq!((all[1].width, all[1].height, fresh), (2960, 2080, 0), "the whole window, at its own pixels");
        let (some, fresh) = targets(&t, |id| (id == 7).then_some(Duration::from_millis(80)).or(old(id)));
        assert_eq!((some.iter().map(|t| t.id).collect::<Vec<_>>(), fresh), (vec![1, 8], 1));
        // No cover of the corner (nothing arrives there, the Spaces engine): the outgoing ones only.
        let (none, _) = targets(&Transition { corner: None, ..t }, old);
        assert_eq!(none.iter().map(|t| t.id).collect::<Vec<_>>(), vec![1]);
    }

    /// With every picture fresh, or nothing leaving, nothing is waited for: the strip is told
    /// at once.
    #[test]
    fn nothing_is_taken_with_every_picture_fresh_or_nothing_leaving() {
        let t = Transition { outgoing: vec![moving(1)], incoming: vec![moving(9)], ..Transition::default() };
        assert!(targets(&t, |_| Some(Duration::from_millis(300))).0.is_empty());
        assert!(!targets(&t, |_| Some(Duration::from_millis(501))).0.is_empty(), "past FRESH");
        let arriving = Transition { incoming: vec![moving(9)], ..Transition::default() };
        assert_eq!(targets(&arriving, |_| None), (Vec::new(), 0));
    }
}
