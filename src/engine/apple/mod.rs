//! The Apple mode's pure half: what the app does while Apple's Stage Manager is on and
//! `[windows] apple_stage_manager_on = "integrate"` (docs/STRATEGIES.md, "Apple-augmented"). Apple's
//! arranges the windows; this app hides nothing, reads what Apple's shows ([`mirror`]), and
//! keeps its own model in step with it ([`follow`]), so the model's stages, `ctl state`, the
//! hotkeys and the saved stages (`state-apple.json`) work on Apple's stages. A step of its own
//! (a switch, a merge, a window sent, a newly opened app brought here) is planned ([`plan`])
//! into a [`job`] that runs one route at a time, each posted one through the [`gate`]; the
//! mirror then says what happened, and the job and the model follow that, whatever was
//! planned.
//!
//! This module also answers the model's door (`model/hiding.rs`) for
//! [`EngineKind::Apple`](super::recovery::EngineKind::Apple): Apple's strip shows the real
//! windows scaled down, so the window server lists a window in the strip at its thumbnail's
//! bounds (79x104 at x = 15 on a recording of 11-16), or off the screen's left edge while
//! the strip hides (x = -258). Such a frame is a hiding place, never the user's: no frame
//! the model records or saves is ever a thumbnail's.
//!
//! - **Is this listed window hidden now?** By a frame alone only when it is thumbnail-sized
//!   and wholly off the left edge (the strip hidden, [`is_hidden`]); a known window, when the
//!   model followed it into the strip ([`hidden_by_record`]).
//! - **May a window be brought back here?** Anywhere within reach but a thumbnail's place.
//! - **Where does a hide leave it?** This app never hides: the model's parked flag is what
//!   Apple's strip holds, and the window's last frame is kept.

pub(crate) mod cause;
pub(crate) mod desk;
pub(crate) mod follow;
pub(crate) mod gate;
pub(crate) mod job;
pub(crate) mod mirror;
pub(crate) mod plan;
pub(crate) mod points;
pub(crate) mod restore;
pub(crate) mod screens;
#[cfg(test)]
mod scenarios;

use super::corner::park::reach_in;
use super::geometry::Rect;
use super::model::{Display, WindowState};

/// The largest side of a strip thumbnail, in points (79x104 to 162x174 measured).
pub const THUMBNAIL_MAX: f64 = 240.0;

/// True when `listed`, on display `d`, is a hidden strip's thumbnail: thumbnail-sized and
/// wholly left of the screen. A thumbnail of a strip that shows is told by its twin in the
/// window list ([`mirror::has_twin`]), never by a place on screen: the stage in front can sit
/// where the thumbnails are (x = 221 on a laptop screen, live 11-16), so no frame on screen
/// is a hiding place by itself.
pub fn is_hidden(d: &Display, listed: Rect) -> bool {
    thumbnail_sized(listed) && listed.max_x() <= d.frame.x
}

/// True when `r` is no bigger than a strip thumbnail on either side.
pub fn thumbnail_sized(r: Rect) -> bool {
    r.w > 0.0 && r.h > 0.0 && r.w <= THUMBNAIL_MAX && r.h <= THUMBNAIL_MAX
}

/// [`is_hidden`]: a thumbnail is out of the user's sight in the sense the model asks.
pub fn out_of_sight(d: &Display, listed: Rect) -> bool {
    is_hidden(d, listed)
}

/// False: no corner a window could show a piece at.
pub fn shows_a_piece(_d: &Display, _frame: Rect) -> bool {
    false
}

/// [`is_hidden`]: a thumbnail's place is no frame to bring a window back to.
pub fn no_restore(d: &Display, frame: Rect) -> bool {
    is_hidden(d, frame)
}

/// `frame` brought within reach on display `d`: moved only as far as its title bar needs to
/// be grabbed, never resized, out of the strip's thumbnail places.
pub fn within_reach(d: &Display, frame: Rect) -> Rect {
    reach_in(frame, d.visible, d.frame, |r| is_hidden(d, r))
}

/// False: this app moves no window to hide it, so no app saves a hidden frame to reopen at.
pub fn clamped_at_hiding(_d: &Display, _frame: Rect) -> bool {
    false
}

/// False: this app's hide leaves no frame of its own.
pub fn lands_hidden(_frame: Rect) -> bool {
    false
}

/// True when window `w` is in Apple's strip as the model last followed it (its parked flag).
pub fn hidden_by_record(w: Option<&WindowState>) -> bool {
    w.is_some_and(|w| w.parked)
}

/// True: Apple's own Stage Manager brings a stage in front when the app in front quits; the
/// model only follows.
pub fn macos_fronts_after_quit() -> bool {
    true
}

/// True: a window in Apple's strip is on its Desktop (listed there at its thumbnail).
pub fn macos_counts_hidden() -> bool {
    true
}

/// Where the model's window `w` was left: its last frame seen at full size.
pub fn hidden_frame(w: &WindowState) -> Rect {
    w.meta.frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testkit::display;

    #[test]
    fn by_its_frame_alone_only_a_thumbnail_off_the_left_edge_is_hidden() {
        let d = display();
        assert!(!is_hidden(&d, Rect::new(15.0, 300.0, 79.0, 104.0)), "a thumbnail on screen is told by its twin, not its place");
        assert!(is_hidden(&d, Rect::new(-258.0, 300.0, 79.0, 104.0)), "the strip hidden");
        assert!(!is_hidden(&d, Rect::new(221.0, 87.0, 520.0, 392.0)), "the stage in front on a laptop screen");
        assert!(!is_hidden(&d, Rect::new(15.0, 300.0, 600.0, 400.0)), "a full-size window at the left");
        assert!(!is_hidden(&d, Rect::new(-50.0, 300.0, 600.0, 400.0)), "a window pushed past the left edge");
    }

    #[test]
    fn no_restore_frame_is_a_hidden_thumbnails_and_a_window_in_reach_stays() {
        let d = display();
        assert!(no_restore(&d, Rect::new(-258.0, 300.0, 79.0, 104.0)));
        assert!(!no_restore(&d, Rect::new(200.0, 100.0, 800.0, 600.0)));
        let window = Rect::new(300.0, 100.0, 800.0, 600.0);
        assert_eq!(within_reach(&d, window), window, "a window in reach stays");
    }
}
