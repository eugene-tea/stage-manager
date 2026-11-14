//! The Corner engine's pure half: where a hidden window goes.
//!
//! The model says only that a window hides ([`Action::Hide`](super::model::Action::Hide));
//! how is the window engine's (`platform::window_engine`). The Corner engine hides a window
//! by moving it into its display's park corner, and this module says where: the park frame
//! of a window as the model knows it now. The geometry itself (zones, corners, how macOS
//! clamps a request) is [`super::park`].

use super::geometry::Rect;
use super::model::{Display, Model, WindowId};
use super::park::{ParkRequest, park_frame_asked};

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
