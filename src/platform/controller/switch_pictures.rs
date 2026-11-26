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
//! **The order.** The corner's cover where it can go ahead (below), an activation's early
//! focus (`Controller::focus_now`, the app already
//! active: macOS decided any Desktop change at the activation), then these pictures, then the
//! transition to the strip; with no picture to take (every one fresh, nothing leaving, or
//! captures off or not allowed: `switch_shots::may_take`) the strip is told first and draws
//! while the keyboard goes. A card click's focus is split after the strip's answer
//! (`split_covered`), where `focus_waits_for_show` holds a held window's focus back for its
//! show when its app has a window on a Desktop not shown. Neither path moves a window, and the
//! pictures are taken by window id, whatever is in front, so the early focus coming before
//! them changes no picture, and the wait for them only delays the strip's start, never a
//! focus.
//!
//! **Pictures taken ahead** (round picture-wait). The wait above is only for a switch nothing
//! announced. The strip takes the pictures as the pointer reaches a card or presses it, and at
//! the app switcher's key; the controller takes them at the other signs that a switch is
//! likely ([`crate::platform::switch_likely`]: the pointer in the Dock's bar, a press on a
//! Dock item, a hotkey's modifiers going down): of the windows in front and the pinned ones
//! shown, on each screen where a stage waits hidden and a switch would play pictures, those
//! with no picture younger than [`AHEAD_OLDER`] ([`super::Controller::switch_likely`]).
//! Nothing waits for them; they reach the strip as any other, and the switch that follows
//! finds them fresh. A sign is an event, never a timer: with the pointer at rest, in the Dock
//! or anywhere, no window is pictured.
//!
//! **The corner's cover first** (round picture-wait). Where a hidden window waits in the park
//! corner (the Corner engine), macOS has raised an activated app's windows there before the
//! app hears of the activation, and the strip covers the corner for the switch
//! ([`Transition::corner`]). That cover is the wallpaper's render or the desktop's still: it
//! needs no picture of a window leaving. So it goes to the strip before the early focus and
//! the pictures ([`corner_ahead`], `strip_model::cover_corner_ahead`), and the transition
//! takes it over when it comes. Two cases keep it with the transition, as before: a window
//! leaving that reaches the cover's rectangle (its picture is drawn over the cover, and is
//! one of those being taken; the cover alone would paint the desktop over that window's
//! corner), and a window that stays shown into the corner (a pinned one, 8L5) whose picture
//! is being taken (the cover draws it from that picture; one fresh already is drawn at once).
//! Until the pictures come the corner then shows what it did before the round: the raised
//! windows' edge, for the pictures' time (19 to 61 ms as above).

use std::time::Duration;

use crate::engine::corner::corner_patch::SwitchCover;
use crate::engine::geometry::Rect;
use crate::platform::capture::Target;
use crate::platform::switch_shots::{self, Taken};
use crate::ui::strip_model::{self, Transition};

/// Longest wait for a switch's outgoing pictures before the transition goes to the strip
/// anyway (module docs).
pub(super) const WAIT_MAX: Duration = Duration::from_millis(160);

/// A picture that reached the strip this recently is not taken again
/// ([`switch_shots::FRESH`]).
const FRESH: Duration = switch_shots::FRESH;

/// When a switch is likely, a picture older than this is taken again ([`ahead_targets`]):
/// half the freshness, so a picture left as it is at a sign is still fresh for a switch
/// within the other half, and the next sign takes it.
pub(super) const AHEAD_OLDER: Duration = Duration::from_millis(FRESH.as_millis() as u64 / 2);

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
        .map(|(id, frame)| target(*id, *frame))
        .collect();
    let fresh = wanted.len() - stale.len();
    (stale, fresh)
}

/// The pictures to take when a switch is likely, of `windows` (the windows shown that a
/// switch would move away or draw on its cover: id and frame): those with none, or one older
/// than [`AHEAD_OLDER`] (`age`), each at its own pixels.
pub(super) fn ahead_targets(windows: &[(u32, Rect)], age: impl Fn(u32) -> Option<Duration>) -> Vec<Target> {
    let mut seen = Vec::with_capacity(windows.len());
    windows
        .iter()
        .filter(|(id, _)| {
            let new = !seen.contains(id);
            seen.push(*id);
            new && age(*id).is_none_or(|a| a > AHEAD_OLDER)
        })
        .map(|(id, frame)| target(*id, *frame))
        .collect()
}

/// A window's picture at its own pixels ([`ASK_SCALE`]).
fn target(id: u32, frame: Rect) -> Target {
    Target { id, width: (frame.w * ASK_SCALE).round().clamp(1.0, 16384.0) as u32, height: (frame.h * ASK_SCALE).round().clamp(1.0, 16384.0) as u32 }
}

/// Takes the pictures of `windows` that are not fresh enough for a switch likely now
/// ([`ahead_targets`]), waiting for none; `sign` for the log. How many were asked for.
pub(super) fn take_ahead(sign: &'static str, windows: &[(u32, Rect)]) -> usize {
    let targets = ahead_targets(windows, strip_model::snapshot_age);
    if targets.is_empty() {
        tracing::trace!(sign, shown = windows.len(), "a switch is likely: every picture it would play is fresh");
        return 0;
    }
    if let Err(why) = switch_shots::may_take() {
        tracing::trace!(sign, why, "a switch is likely: no picture can be taken now");
        return 0;
    }
    let ids: Vec<u32> = targets.iter().map(|t| t.id).collect();
    match switch_shots::take_ahead(targets) {
        Ok(asked) => {
            tracing::debug!(sign, ?ids, fresh = windows.len().saturating_sub(asked), "a switch is likely: its pictures are taken ahead");
            asked
        }
        Err(why) => {
            tracing::debug!(sign, why, "a switch is likely: no picture taken");
            0
        }
    }
}

/// What a switch's pictures need now ([`targets`], by how long ago each reached the strip).
pub(super) struct Wanted {
    /// The windows to capture.
    targets: Vec<Target>,
    /// Those left out: a picture of each reached the strip within [`FRESH`].
    fresh: usize,
    /// Why none of the targets can be captured now ([`switch_shots::may_take`]: captures
    /// off, refused, not tried yet in this run, no Screen Recording).
    skipped: Option<&'static str>,
}

impl Wanted {
    /// The pictures transition `t` needs taken now.
    pub(super) fn of(t: &Transition) -> Self {
        let (targets, fresh) = targets(t, strip_model::snapshot_age);
        let skipped = if targets.is_empty() { None } else { switch_shots::may_take().err() };
        Self { targets, fresh, skipped }
    }

    /// No picture to take, or none can be taken: the transition goes to the strip at once.
    pub(super) fn none(&self) -> bool {
        self.targets.is_empty() || self.skipped.is_some()
    }
}

/// The park corner's cover of transition `t`, to send to the strip ahead of the pictures
/// `wanted` (module docs, "The corner's cover first"), or why it waits for the transition:
/// no cover, a window leaving that reaches the cover's rectangle (its picture goes over the
/// cover, and is one of those being taken), or a window that stays shown into the corner
/// whose picture is being taken (the cover draws it from that picture).
pub(super) fn corner_ahead(t: &Transition, wanted: &Wanted) -> Result<SwitchCover, &'static str> {
    let cover = t.corner.as_ref().ok_or("no cover of the corner")?;
    let leaving = |w: &&crate::ui::strip_model::MovingWindow| t.minimized.is_none_or(|m| m.window != w.window.id);
    if t.outgoing.iter().filter(leaving).any(|w| w.window.frame.intersect(&cover.rect).is_some()) {
        return Err("a window leaving reaches the corner: its picture goes over the cover");
    }
    if cover.over.iter().any(|(id, _)| wanted.targets.iter().any(|t| t.id == *id)) {
        return Err("a window that stays shown into the corner waits for its picture");
    }
    Ok(cover.clone())
}

/// Takes the pictures `wanted` of transition `id` now and waits for them ([`WAIT_MAX`] at
/// most); logged, also when every one was fresh or none can be taken.
pub(super) fn take(id: u64, wanted: Wanted) -> Taken {
    let Wanted { targets, fresh, skipped } = wanted;
    if targets.is_empty() && fresh == 0 {
        return Taken::default();
    }
    let taken = match skipped {
        Some(why) => Taken { asked: targets.len(), skipped: Some(why), ..Taken::default() },
        None => switch_shots::take(targets, WAIT_MAX),
    };
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

/// Why no picture is taken ahead on a screen, if so ([`super::Controller::switch_likely`]):
/// no switch would play pictures there now (`animates`: animation on; `strip_visible`: the
/// user has not hidden the strip; `frozen`: a full-screen Space shows or a Desktop change
/// settles), or no stage waits hidden to switch to (`hidden_stage`).
pub(super) fn ahead_refused(animates: bool, strip_visible: bool, frozen: bool, hidden_stage: bool) -> Option<&'static str> {
    match (animates, strip_visible, frozen, hidden_stage) {
        (false, ..) => Some("animation is off"),
        (_, false, ..) => Some("the strip is hidden: no switch plays pictures"),
        (_, _, true, _) => Some("a full-screen Space shows or a Desktop change settles"),
        (_, _, _, false) => Some("no stage is hidden to switch to"),
        _ => None,
    }
}

impl super::Controller {
    /// A switch is likely (`sign`, [`crate::platform::switch_likely`]): the pictures it would
    /// wait for are taken now, waited for by nothing, so the switch finds them fresh
    /// ([`take_ahead`]). On every screen where a switch would play pictures and a stage
    /// waits hidden ([`ahead_refused`]): the windows of the stage in front and the pinned
    /// ones shown, those a switch moves away or draws on its cover of the park corner. The
    /// same under every engine.
    pub fn switch_likely(&mut self, sign: crate::platform::switch_likely::Sign) {
        use crate::engine::model::Home;
        let Some(m) = self.model.as_ref() else { return };
        let animates = self.store.config().style.animation.animates();
        let mut windows: Vec<(u32, Rect)> = Vec::new();
        let mut refused = None;
        for d in m.displays() {
            let space = m.current_space(d.id);
            let active = m.active_stage_in(space);
            let hidden_stage = m.stages().iter().any(|s| s.space == space && Some(s.id) != active && !s.windows.is_empty());
            if let Some(why) = ahead_refused(animates, self.strip_visible, self.frozen_on(d.id), hidden_stage) {
                refused = Some(why);
                continue;
            }
            let shown = m.windows().filter(|w| {
                !w.parked && ((w.home == Home::Pinned && (w.space == space || w.sticky)) || active.is_some_and(|a| w.home == Home::Stage(a)))
            });
            windows.extend(shown.map(|w| (w.meta.id.0, w.meta.frame)));
        }
        if windows.is_empty() {
            tracing::trace!(sign = sign.as_str(), why = refused, "a switch is likely: no picture to take");
            return;
        }
        take_ahead(sign.as_str(), &windows);
    }
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

    /// Round picture-wait: the corner's cover goes to the strip ahead of the pictures only
    /// while it needs none of them: no window leaving under it, no window staying shown into
    /// it whose picture is being taken.
    #[test]
    fn the_corners_cover_goes_ahead_only_while_it_needs_no_picture_being_taken() {
        let rect = Rect::new(2499.0, 1348.0, 61.0, 92.0);
        let cap = Rect::new(2240.0, 1120.0, 320.0, 320.0);
        let cover = |over| Some(SwitchCover { screen: 1, rect, cap, picture: CoverPicture::Wallpaper, over });
        let old = |_| Some(Duration::from_secs(40));
        let wanted = |t: &Transition, age: &dyn Fn(u32) -> Option<Duration>| {
            let (targets, fresh) = targets(t, age);
            Wanted { targets, fresh, skipped: None }
        };
        // A window leaving in the middle of the screen: the cover goes ahead.
        let t = Transition { outgoing: vec![moving(1)], incoming: vec![moving(9)], corner: cover(Vec::new()), ..Transition::default() };
        assert_eq!(corner_ahead(&t, &wanted(&t, &old)).map(|c| c.rect), Ok(rect));
        // One that reaches the cover's rectangle (a window filling the screen): with the transition.
        let wide = MovingWindow { window: CardWindow { id: 2, pid: 1, frame: Rect::new(0.0, 25.0, 2560.0, 1385.0) }, ..MovingWindow::default() };
        let filled = Transition { outgoing: vec![wide], ..t.clone() };
        assert!(corner_ahead(&filled, &wanted(&filled, &old)).is_err());
        // Minimized already (in the Dock): it covers nothing.
        let minimized = Transition { minimized: Some(Minimized { window: 2, tile: None }), ..filled.clone() };
        assert!(corner_ahead(&minimized, &wanted(&minimized, &old)).is_ok());
        // A pinned window reaching the corner: ahead with its picture fresh, else with the transition.
        let pinned = Transition { corner: cover(vec![(7, Rect::new(1800.0, 900.0, 740.0, 520.0))]), ..t.clone() };
        assert!(corner_ahead(&pinned, &wanted(&pinned, &old)).is_err(), "its picture is being taken");
        let fresh = |id| if id == 7 { Some(Duration::from_millis(80)) } else { old(id) };
        assert!(corner_ahead(&pinned, &wanted(&pinned, &fresh)).is_ok(), "drawn from the picture just taken");
        // No cover (nothing arrives in the corner, the Spaces engine).
        let none = Transition { corner: None, ..t };
        assert!(corner_ahead(&none, &wanted(&none, &old)).is_err());
    }

    /// Round picture-wait: when a switch is likely the pictures taken are those a switch
    /// about now would not find fresh: none yet, or older than half the freshness; each
    /// window once.
    #[test]
    fn a_likely_switch_takes_only_the_pictures_not_fresh_enough() {
        let frame = Rect::new(0.0, 0.0, 800.0, 600.0);
        let shown = [(1, frame), (2, frame), (3, frame), (2, frame)];
        let age = |id| match id {
            1 => None,
            2 => Some(AHEAD_OLDER + Duration::from_millis(1)),
            _ => Some(AHEAD_OLDER),
        };
        let taken = ahead_targets(&shown, age);
        assert_eq!(taken.iter().map(|t| t.id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!((taken[0].width, taken[0].height), (3200, 2400), "the window's own pixels, as at a switch");
        assert!(ahead_targets(&shown, |_| Some(Duration::from_millis(50))).is_empty(), "all fresh: none");
        assert!(AHEAD_OLDER < FRESH, "one left at a sign is still fresh for a while");
    }

    /// Nothing is taken ahead where no switch would play pictures, or none could come.
    #[test]
    fn nothing_is_taken_ahead_where_no_switch_would_play_pictures() {
        assert_eq!(ahead_refused(true, true, false, true), None);
        assert!(ahead_refused(false, true, false, true).is_some(), "animation off");
        assert!(ahead_refused(true, false, false, true).is_some(), "the strip hidden by the user");
        assert!(ahead_refused(true, true, true, true).is_some(), "a full-screen Space");
        assert!(ahead_refused(true, true, false, false).is_some(), "one stage only");
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
