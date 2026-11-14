//! The Spaces engine's pure half: how a hidden window looks to the model when the engine
//! hides a stage by moving its windows into a hidden window-server Space of its own
//! (docs/ARCHITECTURE.md, "Window engines"). Not the Desktops the user switches between
//! ([`super::model::spaces`]): a stage Space is never shown.
//!
//! A window in a stage Space is still listed by the window server, off screen, at its own
//! frame (spike S0, 11-14: same bounds, every round). So nothing about a frame says that a
//! window is hidden, and no frame is unfit to bring a window back to: there is no park zone.
//! This module answers the model's questions (`model/hiding.rs`, the model's one door to
//! the engine) that way; the Corner engine's answers are [`super::corner`]'s.
//!
//! - **Is this listed window hidden now?** Never from its frame. Membership in a stage
//!   Space is a fact the platform reads (`SLSCopySpacesForWindows`), not a frame; until the
//!   observation carries it, a known window is hidden when the model hid it
//!   ([`hidden_by_record`]) and a bare frame is never hidden. That is what a restore after
//!   a crash finds: the stage Spaces die with the app and their windows come back shown
//!   where they were.
//! - **May a window be brought back here?** Anywhere within reach: its title bar on screen.
//! - **Where does a hide leave it?** At its own frame.

use super::geometry::Rect;
use super::model::{Display, WindowState};
use super::corner::park::reach_in;

/// False: a window the window server lists at a frame is not hidden by that frame. A hidden
/// window is listed at its own frame, off screen.
pub fn is_hidden(_d: &Display, _listed: Rect) -> bool {
    false
}

/// False: no frame is out of sight where hidden windows are; they are in no place on screen.
pub fn out_of_sight(_d: &Display, _listed: Rect) -> bool {
    false
}

/// False: there is no corner a window could show a piece at.
pub fn shows_a_piece(_d: &Display, _frame: Rect) -> bool {
    false
}

/// False: no frame is where hidden windows are, so every frame may be a restore frame.
pub fn no_restore(_d: &Display, _frame: Rect) -> bool {
    false
}

/// `frame` brought within reach on display `d`: moved only as far as its title bar needs to
/// be grabbed, never resized ([`reach_in`] with no zone to keep out of).
pub fn within_reach(d: &Display, frame: Rect) -> Rect {
    reach_in(frame, d.visible, d.frame, |_| false)
}

/// False: a hide never moves a window, so no app saves a hidden frame to reopen at.
pub fn clamped_at_hiding(_d: &Display, _frame: Rect) -> bool {
    false
}

/// False: a hide leaves no frame of its own to tell from the user's.
pub fn lands_hidden(_frame: Rect) -> bool {
    false
}

/// True when window `w` (as the model knows it, `None` when it does not) is hidden now:
/// when the model hid it. A stage Space's window is listed at its own frame, and nothing
/// but the engine brings it back (spike S0: Mission Control, a Desktop switch, Command-Tab,
/// the Dock and an app's activation all leave it hidden), so the model's record stands
/// until the platform reads membership itself (S3). A window the model does not know (a
/// launch, after a crash) is shown: the stage Spaces died with the app.
pub fn hidden_by_record(w: Option<&WindowState>) -> bool {
    w.is_some_and(|w| w.parked)
}

/// False: a window in a stage Space is on no Desktop, so when the app in front quits macOS
/// activates none of the hidden stages' apps (live 11-14: 20 of 20 launches left the emptied
/// stage in front); the model brings the most recent stage in front itself, as Apple's Stage
/// Manager does ([`super::model::Model::front_after_quit`]).
pub fn macos_fronts_after_quit() -> bool {
    false
}

/// Where a hide leaves window `w`: its own frame, as last seen.
pub fn hidden_frame(w: &WindowState) -> Rect {
    w.meta.frame
}

#[cfg(test)]
mod tests {
    //! The Spaces engine's nuances: what only this engine answers this way. The general
    //! suite runs on both engines (`testkit::both_engines!`).
    use super::*;
    use crate::engine::geometry::Point;
    use crate::engine::model::{Action, ModelOptions, Observation};
    use crate::engine::recovery::EngineKind;
    use crate::engine::rules::{Decision, RuleAction};
    use crate::engine::testkit::{Desk, FRAME, VISIBLE, display, meta, model_with, w};

    fn spaces_model() -> crate::engine::model::Model {
        model_with(ModelOptions { engine: EngineKind::Spaces, ..ModelOptions::default() })
    }

    #[test]
    fn a_hidden_window_keeps_its_frame() {
        let mut m = spaces_model();
        let mut desk = Desk::default();
        let obs = Observation::default();
        let a = meta(1, 10, "com.example.a");
        desk.open(&a);
        let acts = m.add_window(a.clone(), &Decision::default(), &obs);
        desk.apply(&m, &acts);
        let b = meta(2, 20, "com.example.b");
        desk.open(&b);
        let acts = m.add_window(b, &Decision { action: RuleAction::New, ..Decision::default() }, &desk.obs());
        assert!(acts.contains(&Action::Hide { id: w(1) }), "the old stage hides: {acts:?}");
        desk.apply(&m, &acts);
        assert!(desk.parked(w(1)), "in a stage Space");
        assert_eq!(desk.frames[&w(1)], a.frame, "where it was");
        assert_eq!(m.frame_left(w(1)), Some(a.frame), "where the model counts it left");
        assert!(!desk.obs().z_order.contains(&w(1)), "off screen: not in the on-screen order");
        assert!(!desk.obs().frames.contains_key(&w(1)), "off screen: no frame read");
    }

    #[test]
    fn nothing_is_ever_in_a_park_zone() {
        let m = spaces_model();
        let d = display();
        // The Corner engine's park zone on the test display: the far bottom-right corner.
        let corner = Rect::new(FRAME.max_x() - 1.0, FRAME.max_y() - 1.0, 600.0, 400.0);
        assert!(d.parks(corner), "the Corner engine would call it parked");
        assert!(!m.listed_hidden(d.id, corner), "a listed frame never says hidden");
        assert!(!is_hidden(&d, corner) && !out_of_sight(&d, corner) && !shows_a_piece(&d, corner));
        assert!(!no_restore(&d, corner) && !clamped_at_hiding(&d, corner) && !lands_hidden(corner));
        let shown = Rect::new(200.0, 120.0, 600.0, 400.0);
        assert_eq!(within_reach(&d, shown), shown, "a frame on screen stays");
        let reached = within_reach(&d, corner);
        assert!(VISIBLE.contains(Point::new(reached.x, reached.y)), "only its title bar brought in: {reached}");
        assert_eq!(reached.size(), corner.size(), "never resized");
    }

    #[test]
    fn a_restore_after_a_crash_finds_windows_at_their_frames() {
        // The app died with stage B hidden: the window server ended its stage Spaces and
        // B's window came back shown where it was. Relaunched, the model finds both windows
        // at their own frames, takes neither for hidden, keeps the frames it finds, and
        // hides B's again for its stage.
        let mut m = spaces_model();
        let obs = Observation::default();
        let a = meta(1, 10, "com.example.a");
        let b = meta(2, 20, "com.example.b");
        m.add_window(a.clone(), &Decision::default(), &obs);
        m.add_window(b.clone(), &Decision { action: RuleAction::New, ..Decision::default() }, &obs);
        m.switch_to(m.stage_of(w(1)).expect("staged"), &obs);
        assert!(m.window(w(2)).is_some_and(|x| x.parked), "B hidden at the crash");
        let saved = m.to_state().spaces.remove(0);
        // Relaunched with new ids; B's window is where the user had it, even at the
        // Corner engine's corner: no frame is a stuck park here.
        let mut b_back = meta(12, 20, "com.example.b");
        b_back.title = b.title.clone();
        b_back.frame = Rect::new(FRAME.max_x() - 1.0, FRAME.max_y() - 1.0, 600.0, 400.0);
        let mut a_back = meta(11, 10, "com.example.a");
        a_back.title = a.title.clone();
        a_back.frame = a.frame;
        let relive = [a_back.clone(), b_back.clone()];
        let found = crate::engine::persist::rematch(&saved, &relive);
        let mut back = spaces_model();
        let acts = back.apply_assignment(&relive, &found, false);
        for x in &relive {
            assert!(!spaces_model().listed_hidden_window(x.id, x.display, x.frame), "not known, listed at its frame: shown");
            assert!(!back.seen_hidden(x), "its frame is its own: a match counts it");
        }
        assert_eq!(back.window(w(11)).map(|x| x.restore), Some(a.frame), "A's frame kept");
        assert_eq!(back.window(w(12)).map(|x| x.restore), Some(b_back.frame), "B's found frame kept, corner or not");
        assert!(acts.contains(&Action::Hide { id: w(12) }), "B hides again for its stage: {acts:?}");
        assert!(!acts.iter().any(|x| matches!(x, Action::SetFrame { .. })), "nothing moved: {acts:?}");
    }
}
