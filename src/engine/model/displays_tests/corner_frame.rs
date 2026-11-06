//! A window saved showing only a piece at its screen's park corner (his Mac 11-06: an older
//! version put the laptop's front at the laptop's park corner, -2864,1408, took it as shown,
//! and a click brought it in at -2785,1408, 80 by 32 pt of it on the laptop; saved so, the
//! 14:13 launch made it the laptop's front, invisible, with no card). Such a frame is never a
//! shown frame: at a launch the window is a card with an on-screen restore frame. A window
//! hung off an edge elsewhere stays where the user put it (S2). Made-up titles only.

use super::*;
use crate::engine::geometry::Size;
use crate::engine::park::GRAB_WIDTH;

/// Deep House's window size on his laptop.
const SIZE: (f64, f64) = (1065.0, 915.0);

/// Opens window `id` of `pid` on `display` at `frame` by `action`.
fn open_at(e: &mut Env, id: u32, pid: Pid, display: &Display, frame: Rect, action: RuleAction) {
    let meta = WindowMeta {
        id: WindowId(id),
        pid,
        bundle_id: format!("com.example.app{pid}"),
        title: format!("Doc {id}"),
        role: WindowRole::Standard,
        frame,
        display: display.id,
    };
    e.d.open(&meta);
    let obs = e.d.obs();
    let a = e.m.add_window(meta, &Decision { action, ..Decision::default() }, &obs);
    e.run(a);
}

/// On `display`: a card (window 7) and the front (window 5), the front left by an older
/// version at `at` and saved so; then a launch in the same session listing both where they
/// are. Returns the new model and the launch's actions.
fn launch_with_front_at(display: &Display, at: Rect) -> (Model, Vec<Action>) {
    let mut e = Env::both();
    open_at(&mut e, 7, 22, display, on(display, (300.0, 150.0)), RuleAction::Current);
    open_at(&mut e, 5, 20, display, on(display, (100.0, 100.0)), RuleAction::New);
    assert_eq!(e.m.active_stage(display.id), Some(e.stage_of(5)));
    // The older version: the front shown at `at`, taken as the user's frame.
    e.d.frames.insert(WindowId(5), at);
    e.m.window_frame_changed(WindowId(5), at);
    assert_eq!(e.m.window(WindowId(5)).map(|w| (w.parked, w.restore)), Some((false, at)), "saved so");
    let state = e.m.to_state();
    let live: Vec<WindowMeta> = [5, 7]
        .iter()
        .map(|&id| WindowMeta { frame: e.d.frames[&WindowId(id)], ..e.m.window(WindowId(id)).expect("managed").meta.clone() })
        .collect();
    let mut m = Model::new(vec![studio(), laptop()], ModelOptions::default());
    m.restore_spaces(state.spaces);
    let a = m.arrive(display.id, &live, &[], &LaunchHints::default());
    (m, a)
}

/// The launch made window 5 a card of `display` that comes back wholly on its screen at
/// its own size, and nothing it shows in front; window 7 a card still.
fn assert_a_card(m: &Model, a: &[Action], display: &Display) {
    let w = m.window(WindowId(5)).expect("managed");
    assert!(w.parked && w.meta.display == display.id, "a card of its screen: {w:?}");
    assert!(display.visible.contains_rect(&w.restore) && !display.parks(w.restore), "comes back on screen: {:?}", w.restore);
    assert_eq!((w.restore.w, w.restore.h), SIZE, "never resized");
    assert!(a.iter().any(|x| matches!(x, Action::Park { id, frame } if *id == WindowId(5) && display.parks(*frame))), "parked at the corner: {a:?}");
    let (front, card) = (m.stage_of(WindowId(5)).expect("staged"), m.stage_of(WindowId(7)).expect("staged"));
    let active = m.active_stage(display.id).expect("a front");
    assert_ne!(active, front, "not the front");
    assert!(m.stage(active).is_some_and(|s| s.windows.iter().all(|id| m.window(*id).is_some_and(|w| !w.parked))), "the front shows what it holds");
    let mut slots = m.strip_slots(display.id);
    slots.sort();
    let mut want = vec![front, card];
    want.retain(|s| *s != active);
    want.sort();
    assert_eq!(slots, want, "its card in the strip");
}

#[test]
fn a_launch_with_the_laptops_front_saved_at_its_corner_makes_it_a_card() {
    // His numbers: the laptop -1800,271 1800x1169, its corner bottom-left; the window
    // 1065x915 at -2785,1408 (80 by 32 pt on the laptop).
    let at = Rect::new(-2785.0, 1408.0, SIZE.0, SIZE.1);
    assert!(!laptop().parks(at), "out of the park zone by its test: the bug");
    let (m, a) = launch_with_front_at(&laptop(), at);
    assert_a_card(&m, &a, &laptop());
    assert_eq!(m.window(WindowId(5)).map(|w| w.restore), Some(Rect::new(-1800.0, 1440.0 - SIZE.1, SIZE.0, SIZE.1)), "in the corner, on screen");
}

#[test]
fn a_launch_with_the_5ks_front_saved_at_its_corner_makes_it_a_card() {
    // The 5K parks bottom-right: the same piece, 80 by 32 pt at its bottom-right corner.
    assert_eq!(studio().corner, Corner::BottomRight);
    let at = Rect::new(2560.0 - GRAB_WIDTH, 1440.0 - 32.0, SIZE.0, SIZE.1);
    assert!(!studio().parks(at));
    let (m, a) = launch_with_front_at(&studio(), at);
    assert_a_card(&m, &a, &studio());
}

#[test]
fn a_window_hung_off_the_laptops_left_edge_in_the_upper_half_is_shown_where_it_is() {
    // S2 at the corner's own side: a grab's width showing at the left edge, but high up.
    for shown in [GRAB_WIDTH, SIZE.0 / 2.0] {
        let at = Rect::new(-1800.0 - SIZE.0 + shown, 400.0, SIZE.0, SIZE.1);
        assert_eq!(laptop().reach(at), at, "within reach as it is");
        let (m, a) = launch_with_front_at(&laptop(), at);
        let front = m.stage_of(WindowId(5)).expect("staged");
        assert_eq!(m.active_stage(LAPTOP), Some(front), "still the front");
        assert_eq!(m.window(WindowId(5)).map(|w| (w.parked, w.restore)), Some((false, at)));
        assert!(!a.iter().any(|x| matches!(x, Action::SetFrame { id, .. } | Action::Park { id, .. } if *id == WindowId(5))), "not moved: {a:?}");
    }
}

#[test]
fn a_corner_piece_comes_back_on_screen_and_an_edge_hang_elsewhere_stays() {
    let d = laptop();
    // His frame, and the 5K's mirror of it: wholly on screen, as near as it fits.
    let piece = Rect::new(-2785.0, 1408.0, SIZE.0, SIZE.1);
    assert_eq!(d.reach(piece), Rect::new(-1800.0, 525.0, SIZE.0, SIZE.1));
    let s = studio();
    let mirror = Rect::new(2480.0, 1408.0, SIZE.0, SIZE.1);
    assert_eq!(s.reach(mirror), Rect::new(2560.0 - SIZE.0, 1440.0 - SIZE.1, SIZE.0, SIZE.1));
    // The laptop's park corner itself, as before: centered.
    let parked = park_frame_at(Size::new(SIZE.0, SIZE.1), d.frame, d.corner);
    assert!(d.parks(parked) && d.visible.contains_rect(&d.reach(parked)));
    // Hung off the bottom edge in the middle, or off the left low but showing a good part.
    for f in [Rect::new(-1200.0, 1408.0, SIZE.0, SIZE.1), Rect::new(-2500.0, 1300.0, SIZE.0, SIZE.1)] {
        assert_eq!(d.reach(f), f, "{f:?}");
    }
    // The other corner of the laptop is the 5K's side: not the laptop's corner.
    let right = Rect::new(-80.0, 1408.0, SIZE.0, SIZE.1);
    assert_eq!(d.reach(right), right);
}

#[test]
fn a_restore_never_lands_at_the_screens_corner() {
    // Whatever the frame, where the model brings a window back is neither in the park zone
    // nor a piece at the corner, on either screen, and is never resized.
    let mut seed = 11u64;
    for d in [laptop(), studio()] {
        for _ in 0..3000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let x = d.frame.x - 1500.0 + ((seed >> 33) % 5000) as f64;
            let y = d.frame.y - 800.0 + ((seed >> 13) % 3000) as f64;
            let w = 20.0 + ((seed >> 40) % 2000) as f64;
            let h = 20.0 + ((seed >> 20) % 1400) as f64;
            let out = d.reach(Rect::new(x, y, w, h));
            assert!(!d.parks(out) && !d.corner_piece(out), "{:?} {out:?}", d.id);
            assert_eq!((out.w, out.h), (w, h));
        }
    }
}
