//! Saved frames across a change of the screens' arrangement (R2 of the review before
//! 1.7): `state.json` keeps global frames, and a screen moved in Displays settings while the
//! app was not running (the laptop from the 5K's left to its right) moves its Desktops'
//! saved frames along with it. Made-up titles only.

use super::*;
use crate::engine::persist::{SavedSpace, StateFile};
use crate::engine::restore::Restore;

/// The laptop's screen to the right of the 5K now, top edges level.
fn laptop_right() -> (Display, Display) {
    let frame = Rect::new(2560.0, 0.0, 1800.0, 1169.0);
    let visible = Rect::new(2560.0, 39.0, 1800.0, 1130.0);
    let s = studio();
    let l = Display { id: LAPTOP, key: LAPTOP_KEY, frame, visible, corner: choose_corner(frame, visible, &[s.frame]).corner };
    (Display { corner: choose_corner(s.frame, s.visible, &[frame]).corner, ..s }, l)
}

/// [`three_desktops`] saved in session "A", loaded in "B" with the laptop moved to the right;
/// `legacy`: a file of a build that did not keep the screens' origins.
fn restarted_moved(legacy: bool) -> (Model, Display) {
    let mut spaces: Vec<SavedSpace> = three_desktops();
    if legacy {
        for sc in spaces.iter_mut().filter_map(|s| s.screen.as_mut()) {
            (sc.x, sc.y) = (None, None);
        }
    }
    let mut file = StateFile { session: Some("A".into()), ..StateFile::single(SavedSpace::default()) };
    file.spaces = spaces;
    file.start_new_session();
    let (file, _) = file.for_session(Some("B"));
    let (s, l) = laptop_right();
    let mut m = Model::new(vec![s, l], ModelOptions::default());
    m.set_restore(Restore::after_restart("B"));
    m.set_desktop_counts([(STUDIO_KEY, 2), (LAPTOP_KEY, 1)]);
    m.restore_spaces(file.spaces);
    (m, l)
}

/// The launch on the 5K (the editor), then the laptop's player reopened by its app on the
/// laptop at `live`; the actions of its arrival.
fn player_back(m: &mut Model, live: Rect) -> Vec<Action> {
    let s = studio();
    let editor = [
        reopen(301, 30, EDITOR, "main.rs — proj", STUDIO, on(&s, (200.0, 60.0))),
        reopen(302, 30, EDITOR, "lib.rs — proj", STUDIO, on(&s, (240.0, 90.0))),
    ];
    m.arrive(STUDIO, &editor, &[], &LaunchHints::default());
    m.arrive(LAPTOP, &[], &[], &LaunchHints::default());
    add_plain(m, reopen(313, 33, PLAYER, "Player", LAPTOP, live))
}

#[test]
fn a_screen_moved_in_the_arrangement_takes_its_saved_frames_along() {
    let (mut m, l) = restarted_moved(false);
    // Saved at x -1500 (the laptop on the left); the same place on the laptop is x 2860 now.
    let saved = on(&l, (300.0, 150.0));
    assert!(saved.x > 2560.0, "on the laptop where it is now: {saved:?}");
    let a = player_back(&mut m, on(&l, (10.0, 10.0)));
    assert_eq!(m.stage_of(WindowId(313)).and_then(|s| m.stage(s)).map(|s| s.space.0), Some(17));
    assert!(a.contains(&Action::SetFrame { id: WindowId(313), frame: saved }), "its saved place on the laptop: {a:?}");
    assert_eq!(m.window(WindowId(313)).map(|w| w.restore), Some(saved));
    // Its cards keep the moved frames: saved again, the file names the laptop where it is.
    let file = m.to_state();
    let laptop_saved = file.spaces.iter().find(|s| s.id == 17).expect("saved");
    assert_eq!(laptop_saved.screen.as_ref().and_then(|sc| sc.origin()), Some((2560.0, 0.0)));
    let web = laptop_saved.stages.iter().flat_map(|st| st.windows.iter()).find(|w| w.bundle_id == WEB).expect("pending");
    assert_eq!(web.frame, on(&l, (40.0, 30.0)));
}

#[test]
fn a_screen_moved_while_running_takes_its_waiting_frames_along() {
    // The laptop's Desktop formed (its player back), the web browser still to come; then
    // the laptop is moved to the right in Displays settings.
    let (mut m, l) = restarted_moved(false);
    player_back(&mut m, on(&l, (10.0, 10.0)));
    let left = Display { frame: laptop_frame(), visible: laptop().visible, ..l };
    m.set_displays(vec![laptop_right().0, left]);
    m.set_displays(vec![laptop_right().0, l]);
    let pending = m.stages().iter().flat_map(|s| m.pending_of(s.id)).find(|w| w.bundle_id == WEB).expect("pending").clone();
    assert_eq!(pending.frame, on(&l, (40.0, 30.0)), "moved there and back again");
}

#[test]
fn a_file_without_the_screens_origins_uses_no_frame_off_its_screen() {
    // A file of a build before the origins: the laptop's frames at x -1500 lie on no screen
    // of now; the player stays where its app put it.
    let (mut m, l) = restarted_moved(true);
    let live = on(&l, (10.0, 10.0));
    let a = player_back(&mut m, live);
    assert_eq!(m.stage_of(WindowId(313)).and_then(|s| m.stage(s)).map(|s| s.space.0), Some(17), "its saved stage still");
    assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } if *id == WindowId(313))), "not moved: {a:?}");
    assert_eq!(m.window(WindowId(313)).map(|w| w.restore), Some(live));
    // The 5K's own frames are on it as they were: used.
    assert!(m.frames_fit(STUDIO, Some(2)));
}

#[test]
fn a_file_without_the_origins_keeps_frames_still_on_their_screen() {
    // The same file with the laptop where it was: its frames lie on it, and are used.
    let mut spaces = three_desktops();
    for sc in spaces.iter_mut().filter_map(|s| s.screen.as_mut()) {
        (sc.x, sc.y) = (None, None);
    }
    let mut file = StateFile { session: Some("A".into()), ..StateFile::single(SavedSpace::default()) };
    file.spaces = spaces;
    file.start_new_session();
    let (file, _) = file.for_session(Some("B"));
    let mut m = Model::new(vec![studio(), laptop()], ModelOptions::default());
    m.set_restore(Restore::after_restart("B"));
    m.set_desktop_counts([(STUDIO_KEY, 2), (LAPTOP_KEY, 1)]);
    m.restore_spaces(file.spaces);
    let l = laptop();
    let a = player_back(&mut m, on(&l, (10.0, 10.0)));
    assert!(a.contains(&Action::SetFrame { id: WindowId(313), frame: on(&l, (300.0, 150.0)) }), "{a:?}");
}
