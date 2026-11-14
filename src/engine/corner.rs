//! The Corner engine's pure half: where a hidden window goes, and how a frame tells whether
//! a window is hidden there.
//!
//! The model says only that a window hides ([`Action::Hide`](super::model::Action::Hide));
//! how is the window engine's (`platform::window_engine`). The Corner engine hides a window
//! by moving it into its display's park corner, and this module says where (the park frame
//! of a window as the model knows it now) and answers the model's questions about frames
//! (`model/hiding.rs`, the model's one door to the engine): is a window listed here hidden,
//! may a restore frame be here, where is it within reach. The geometry itself (zones,
//! corners, how macOS clamps a request) is [`super::park`].

use super::geometry::Rect;
use super::model::{Display, Model, WindowId};
use super::park::{ParkRequest, park_frame_asked};
use super::restore::clamped_at_corner;

/// How far (points) a frame's side at the park corner may be from where a park asks it and
/// still be that park's landing ([`lands_hidden`]): AX and the window server round.
const PARK_LANDED_SLACK: f64 = 2.0;

/// True when a window the window server lists at `listed` on display `d` is hidden: in the
/// display's park zone ([`Display::parks`]).
pub fn is_hidden(d: &Display, listed: Rect) -> bool {
    d.parks(listed)
}

/// True when a window listed at `listed` on display `d` is out of sight at the park corner:
/// hidden ([`is_hidden`]) or only a piece showing there ([`Display::hides`]).
pub fn out_of_sight(d: &Display, listed: Rect) -> bool {
    d.hides(listed)
}

/// True when a window at `frame` on display `d` shows only a piece at the park corner
/// ([`Display::corner_piece`]).
pub fn shows_a_piece(d: &Display, frame: Rect) -> bool {
    d.corner_piece(frame)
}

/// True when `frame` is no place to bring a window back to on display `d`: the park zone is
/// where hidden windows are, so a restore frame in it would leave the window looking hidden
/// ([`Display::parks`]).
pub fn no_restore(d: &Display, frame: Rect) -> bool {
    d.parks(frame)
}

/// `frame` brought within reach on display `d`, out of its park zone ([`Display::reach`]).
pub fn within_reach(d: &Display, frame: Rect) -> Rect {
    d.reach(frame)
}

/// True when a shown window at `frame` on display `d` came back squeezed against the park
/// corner ([`clamped_at_corner`]): its app saved a parked frame and macOS reopened it on
/// screen. Not a hidden window ([`is_hidden`]).
pub fn clamped_at_hiding(d: &Display, frame: Rect) -> bool {
    !d.parks(frame) && clamped_at_corner(frame, d.visible, d.corner)
}

/// True when `frame` (in the park zone of `d`) is where a park of a window whose restore
/// frame is `restore` lands: its side at the corner as far out as a park asks it
/// ([`park_frame_of`]), or as one point inside the display's or the visible frame's side
/// (where macOS may clamp a request asked further out), within [`PARK_LANDED_SLACK`]. macOS
/// keeps the asked `x` (measured) but moves `y` (a title strip kept above the visible bottom
/// or below the menu bar, by the app's title height), and an app may resize a parked window,
/// so only that side is compared. A user's drag stops short of it: the pointer stops at the
/// screen's edge, and the window the grab's offset inside it. When unsure the frame counts
/// as a park: not recording a user's frame keeps the older one, recording a park would bring
/// the window back at the corner.
pub fn lands_hidden(restore: Rect, d: &Display, asked: ParkRequest, frame: Rect) -> bool {
    let park = park_frame_of(restore, d, asked);
    let visible = if d.visible.is_degenerate() { d.frame } else { d.visible };
    if d.corner.right() {
        let edge = park.x.min(d.frame.max_x() - 1.0).min(visible.max_x() - 1.0);
        frame.x >= edge - PARK_LANDED_SLACK
    } else {
        let edge = park.max_x().max(d.frame.x + 1.0).max(visible.x + 1.0);
        frame.max_x() <= edge + PARK_LANDED_SLACK
    }
}

/// The frame that parks a window whose restore frame is `restore` on display `d`: its
/// restore size in the display's park corner, asked as `asked` (`[windows] park_inside`,
/// `park_drop`).
pub fn park_frame_of(restore: Rect, d: &Display, asked: ParkRequest) -> Rect {
    park_frame_asked(restore.size(), d.frame, d.corner, asked)
}

/// The frame that parks window `id` of model `m`, as the model knows the window now (its
/// restore size, its display, the park request of the model's options); `None` for a window
/// the model does not manage.
pub fn park_frame(m: &Model, id: WindowId) -> Option<Rect> {
    let w = m.window(id)?;
    Some(park_frame_of(w.restore, m.display(w.meta.display), m.options().park))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::{Action, Observation};
    use crate::engine::park::park_frame_at;
    use crate::engine::rules::{Decision, RuleAction};
    use crate::engine::testkit::{display, meta, model, w};

    #[test]
    fn a_hidden_window_parks_at_its_restore_size_in_its_displays_corner() {
        let mut m = model();
        let obs = Observation::default();
        m.add_window(meta(1, 10, "com.example.a"), &Decision::default(), &obs);
        let a = m.add_window(meta(2, 20, "com.example.b"), &Decision { action: RuleAction::New, ..Decision::default() }, &obs);
        assert!(a.contains(&Action::Hide { id: w(1) }), "the model says only that it hides: {a:?}");
        let d = display();
        let restore = m.window(w(1)).expect("managed").restore;
        let park = park_frame(&m, w(1)).expect("managed");
        assert_eq!(park, park_frame_at(restore.size(), d.frame, d.corner));
        assert!(d.parks(park) && !d.parks(restore), "in the park zone, its restore frame out of it");
        assert_eq!(m.frame_left(w(1)), Some(park), "where the model counts a parked window left");
        assert_eq!(park_frame(&m, w(99)), None, "not a managed window");
    }
}
