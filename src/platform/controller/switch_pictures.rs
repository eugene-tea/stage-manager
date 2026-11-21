//! A switch's outgoing pictures taken at the switch (1.9, S4b), for an engine that hides in one
//! step ([`crate::platform::window_engine::WindowEngine::pictures_at_switch`]: the Spaces
//! engine).
//!
//! Under the Corner engine the strip keeps the shown windows' full-size pictures fresh every
//! `[animation] snapshot_interval_ms`, because a switch nobody announced (a Dock click, ⌘Tab)
//! gives no time to take them. Under Spaces the outgoing windows can wait on screen: before the
//! transition goes to the strip, every outgoing window whose picture is not fresh (the
//! pointer's press on a card took one a moment ago) is captured, all in parallel, and the WM
//! thread waits for them up to [`WAIT_MAX`]. The strip then draws them over their windows as
//! before, and the windows hide behind them in one window-server step each.
//!
//! The bound: through the window server's own capture (S6b, [`crate::platform::capture_fast`])
//! a window takes ~13 ms (S4c's probe, 12-14 ms; up to 22-34 ms when called rarely), so 4
//! windows ~55 ms. Through ScreenCaptureKit, the fallback (measured 11-14 on a 5K display,
//! `probe-spaces capture`, a listing of the shareable content first): one window ~80-90 ms
//! from the call to its picture, each more ~18 ms, whatever their size; 4 windows ~145 ms, at
//! most 150 ms. [`WAIT_MAX`] lets 4 windows in at their slowest on the fallback; a picture later
//! than that still reaches the strip, and the moving one of its window cross-fades to it, the
//! window having hidden behind the older picture (or none: the window just goes, as before).
//!
//! **With the focus rules of S3c/S3d.** The order under Spaces is: an activation's early focus
//! (`Controller::focus_now`, the app already active: macOS decided any Desktop change
//! at the activation), then these pictures, then the transition to the strip; a card click's
//! focus is split after the strip's answer (`split_covered`), where
//! `focus_waits_for_show` holds a held window's focus back for its show when its app has a
//! window on a Desktop not shown. Neither path moves a window, and the pictures are taken by
//! window id, whatever is in front, so the early focus coming before them changes no picture,
//! and the wait for them only delays the strip's start, never a focus.

use std::time::Duration;

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

/// The outgoing windows of `t` to capture at the switch: not one already in the Dock
/// (minimized: its picture comes from its tile), not one whose picture is fresh (`age`).
pub(super) fn targets(t: &Transition, age: impl Fn(u32) -> Option<Duration>) -> Vec<Target> {
    t.outgoing
        .iter()
        .map(|w| w.window)
        .filter(|w| t.minimized.is_none_or(|m| m.window != w.id))
        .filter(|w| age(w.id).is_none_or(|a| a > FRESH))
        .map(|w| Target {
            id: w.id,
            width: (w.frame.w * ASK_SCALE).round().clamp(1.0, 16384.0) as u32,
            height: (w.frame.h * ASK_SCALE).round().clamp(1.0, 16384.0) as u32,
        })
        .collect()
}

/// Takes the outgoing pictures of `t` now and waits for them ([`WAIT_MAX`] at most); logged.
pub(super) fn take(t: &Transition) -> Taken {
    let targets = targets(t, strip_model::snapshot_age);
    let fresh = t.outgoing.len() - targets.len();
    let taken = switch_shots::take(targets, WAIT_MAX);
    if taken.skipped.is_none() && taken.asked > 0 {
        let how = if taken.answered < taken.asked { crate::engine::margin::End::Cap } else { crate::engine::margin::End::Met };
        crate::engine::margin::wait("switch_pictures::WAIT_MAX", Duration::from_millis(taken.ms), WAIT_MAX, how);
    }
    tracing::debug!(
        id = t.id,
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
    use crate::engine::geometry::Rect;
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
        let ids: Vec<u32> = targets(&t, age).iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 3], "never captured, or captured long ago; the incoming window is not taken");
        let first = targets(&t, age)[0];
        assert_eq!((first.width, first.height), (3200, 2400), "asked above any display's scale: the window's own pixels");
    }
}
