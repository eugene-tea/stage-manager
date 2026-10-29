//! The still of the desktop in the park corner, on the WM thread (round desktop-still-patch,
//! [`crate::engine::patch_still`]; round 8L3 for a switch's cover): kept fresh at every corner
//! patch plan while the patch or a switch's cover wants it, and what the patch finds of it
//! ([`Controller::corner_still`]); a press on the desktop or a release in the corner takes a
//! new one shortly ([`Controller::corner_touched`]).

use std::time::Instant;

use super::{Controller, PATCH_REVEAL_HOLD, cover_desktop_forced, input};
use crate::engine::geometry::{Point, Rect};
use crate::engine::patch_still::{self as ps, Still, TakeFacts};
use crate::platform::{desktop, desktop_shot, windows};

/// What [`Controller::corner_still`] found.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct CornerStill {
    /// The still for the patch: ready, coming or absent.
    pub still: Still,
    /// A switch's cover may need the patch's picture under a still of the icons only (8L4):
    /// the patch keeps it rendered while hidden.
    pub picture: bool,
}

impl Controller {
    /// Keeps the still of the desktop in the park corner fresh, at every corner patch plan
    /// with slivers there (`rect`, the patch's rectangle; `items`, Finder's icons as last
    /// read), and says what the patch finds of it.
    ///
    /// - **The patch** (`[windows] corner_patch`) wants it whenever slivers show: drawn from
    ///   a still where one can be taken (Screen Recording, window captures working), it is
    ///   exact whatever the wallpaper and whatever lies on the desktop.
    /// - **A switch's cover** (an activation may grow: `[animation] activation_grows`, motion
    ///   on, the strip shown) wants it where the file's picture may not stand for the corner a
    ///   switch covers (8L3): a desktop icon in the still's block around the corner, not just
    ///   under the patch (10-16), the icons not known, or no picture of it now.
    ///
    /// Never taken while a switch runs, an overview is up, a Space change is being recognized
    /// or a Desktop slides, nor just after a Space change or a drop ([`ps::may_take`]); none on
    /// a full-screen Space, and none at rest ([`desktop_shot::rest`]: the screen locked, the
    /// display asleep; for a cover alone, also no input and no switch for 2 min).
    pub(super) fn corner_still(&self, rect: Option<Rect>, items: Option<&[Rect]>, now: Instant) -> CornerStill {
        let config = self.store.config();
        let Some(rect) = rect.filter(|_| config.windows.corner_patch) else { return CornerStill::default() };
        let (Some(key), Some(m)) = (self.shot_key(), self.model.as_ref()) else { return CornerStill::default() };
        let block = desktop_shot::block(m.main_display().frame, Some(rect));
        let anim = &config.style.animation;
        let grows = anim.activation_grows && anim.animates() && self.strip_visible;
        let icons = items.is_none_or(|i| i.iter().any(|i| i.intersect(&block).is_some()));
        let cover = grows && (icons || cover_desktop_forced() || crate::ui::corner_patch::cover_ready(rect).is_err());
        let overview = self.overview.showing().is_some() || self.patch_reveal_hold.is_some_and(|t| now.duration_since(t) < PATCH_REVEAL_HOLD);
        let safety = ps::may_take(&TakeFacts {
            switching: self.pending.is_some(),
            overview,
            space_pending: self.space_pending,
            slide: self.patch_slide_since.is_some(),
            fullscreen: self.phase.fullscreen(),
            since_space_change: self.space_changed_at.map(|t| now.saturating_duration_since(t)),
            since_dropped: desktop_shot::since_dropped(now),
        });
        let moment = desktop_shot::Moment {
            locked: self.locked,
            asleep: objc2_core_graphics::CGDisplayIsAsleep(objc2_core_graphics::CGMainDisplayID()),
            since_input: input::since_last_input(),
            since_switch: self.last_switch.map(|t| now.saturating_duration_since(t)),
        };
        desktop_shot::keep_fresh(key.clone(), block, moment, desktop_shot::Wants { cover, patch: true }, safety, now);
        let still = desktop_shot::for_patch(rect, &key, moment, safety, now);
        // As before this round: the file's picture is kept rendered for a cover's still of the
        // icons only, never during an overview, a Space change, a full-screen Space or a
        // switch, nor at rest.
        let busy = overview || self.space_pending || self.phase.fullscreen() || self.pending.is_some();
        let picture = cover && !busy && desktop_shot::rest(moment, false).is_none();
        CornerStill { still, picture }
    }

    /// A mouse-up at `at` (WM thread): while the patch draws a still, one in the corner or on
    /// the desktop (no window there) takes a new still shortly: an icon selected, deselected
    /// or dropped there shows as it is.
    pub(super) fn corner_touched(&self, at: Point, now: Instant) {
        if !self.patch_sent.as_ref().is_some_and(|p| p.still.is_some() || p.hold) {
            return;
        }
        let Some(m) = self.model.as_ref() else { return };
        let in_corner = desktop_shot::zone(m.main_display().frame).contains(at);
        let on_desktop = in_corner || {
            let own = std::process::id() as i32;
            let list: Vec<_> = windows::list_windows().into_iter().filter(|w| w.pid != own).collect();
            desktop::is_wallpaper_click(&list, at)
        };
        if on_desktop {
            desktop_shot::touched(now);
        }
    }
}
