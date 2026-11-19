//! Windows keep the frames set for each screen setup (`[displays] frames_per_setup`): the
//! external screen unplugged, the laptop's frames come back; plugged in again, the external
//! screen's. The controller's tick is played here with a made-up clock: a snapshot kept after
//! its quiet, nothing taken within a display change's settle, the put-back once it settles.
//! Made-up titles only.

use std::time::{Duration, Instant};

use super::*;
use crate::engine::setup_frames::{PutBack, QUIET, Recorder, Settled, SetupKey};

const SETTLE: Duration = Duration::from_secs(10);

/// The laptop's screen alone, main now: global coordinates start at its top-left.
fn laptop_alone() -> Display {
    let (frame, visible) = (Rect::new(0.0, 0.0, 1800.0, 1169.0), Rect::new(0.0, 39.0, 1800.0, 1130.0));
    Display { id: LAPTOP, key: LAPTOP_KEY, frame, visible, corner: choose_corner(frame, visible, &[]).corner }
}

/// The controller's half, with a clock of its own.
struct Tick {
    r: Recorder,
    now: Instant,
}

impl Tick {
    fn new() -> Self {
        Self { r: Recorder::default(), now: Instant::now() }
    }

    fn wait(&mut self, d: Duration) {
        self.now += d;
    }

    /// One tick: the put-back once a change settled (its actions run on the desk), else a
    /// snapshot offered.
    fn tick(&mut self, e: &mut Env) -> Option<Settled> {
        if let Some(touched) = self.r.settled(self.now, SETTLE) {
            let (actions, settled) = e.m.setup_settled(&touched);
            e.run(actions);
            return Some(settled);
        }
        let setup = e.m.setup_key();
        let m = &e.m;
        if let Some(frames) = self.r.offer(setup.as_ref(), true, self.now, SETTLE, || setup.as_ref().map(|s| m.setup_frames_now(s)).unwrap_or_default()) {
            e.m.keep_setup_frames(frames);
        }
        None
    }

    /// Ticks until the frames now are kept (a snapshot, then its quiet).
    fn keep(&mut self, e: &mut Env) {
        for _ in 0..3 {
            self.tick(e);
            self.wait(QUIET);
        }
        self.tick(e);
    }

    /// The displays change to `displays` (the model's batch run on the desk), heard now.
    fn screens(&mut self, e: &mut Env, displays: Vec<Display>) {
        self.r.display_changed(self.now);
        let a = e.m.set_displays(displays);
        e.run(a);
    }

    /// Ticks through the settle; what it found.
    fn settle(&mut self, e: &mut Env) -> Settled {
        for _ in 0..3 {
            self.wait(SETTLE / 4);
            assert_eq!(self.tick(e), None, "nothing within the settle");
        }
        self.wait(SETTLE / 4);
        self.tick(e).expect("settled")
    }
}

/// A frame moved or resized: the desk shows it and the model hears it (as the user's or an
/// app's; macOS's within a settle alike).
fn frame(e: &mut Env, id: u32, f: Rect) {
    e.d.frames.insert(WindowId(id), f);
    e.m.window_frame_changed(WindowId(id), f);
}

fn kept(e: &Env, id: u32, setup: &SetupKey) -> Option<Rect> {
    e.m.setup_frames_of(WindowId(id)).iter().find(|f| f.setup == setup.text()).map(|f| f.frame)
}

fn restore(e: &Env, id: u32) -> Rect {
    e.m.window(WindowId(id)).map(|w| w.restore).expect("known")
}

/// The desk: on the external screen (the Studio, main) the editor in front (1); on the laptop
/// the browser in front (2), the notes a card (3).
fn desk(e: &mut Env) {
    e.open(1, 10, &studio(), (200.0, 100.0), RuleAction::Current);
    e.open(3, 21, &laptop(), (100.0, 100.0), RuleAction::Current);
    e.open(2, 20, &laptop(), (400.0, 200.0), RuleAction::New);
    assert!(e.m.window(WindowId(3)).is_some_and(|w| w.parked), "the notes a card");
}

both_engines! {
#[test]
fn the_laptops_frames_come_back_alone_and_the_externals_when_it_comes_back() {
    let mut e = Env::both();
    let mut t = Tick::new();
    desk(&mut e);
    let a = e.m.setup_key().expect("both screens known");
    t.keep(&mut e);
    let a1 = kept(&e, 1, &a).expect("the editor's frame on the Studio");
    assert_eq!(a1, Rect::new(200.0, 125.0, 800.0, 600.0), "relative to the Studio's corner");
    let (a2, a3) = (on(&laptop(), (400.0, 200.0)), on(&laptop(), (100.0, 100.0)));

    // The Studio unplugged: the laptop alone, main. macOS squeezes the editor onto it; within
    // the settle that is never kept.
    t.screens(&mut e, vec![laptop_alone()]);
    let b = e.m.setup_key().expect("the laptop known");
    assert_ne!(a, b);
    frame(&mut e, 1, Rect::new(500.0, 300.0, 800.0, 600.0));
    assert_eq!(t.settle(&mut e), Settled::New(1), "never seen: macOS's way");
    assert_eq!(kept(&e, 1, &a), Some(a1), "the squeeze was never the Studio setup's frame");
    assert_eq!(kept(&e, 1, &b), Some(Rect::new(500.0, 300.0, 800.0, 600.0)), "macOS's frame, kept from the settle");

    // On the laptop alone the user sets the editor's frame, and the notes' (its stage shown,
    // resized, sent back to a card).
    let b1 = Rect::new(50.0, 80.0, 1200.0, 900.0);
    let b3 = Rect::new(900.0, 400.0, 700.0, 500.0);
    frame(&mut e, 1, b1);
    let (editor, notes) = (e.stage_of(1), e.stage_of(3));
    e.switch(notes);
    frame(&mut e, 3, b3);
    e.switch(editor);
    assert!(e.m.window(WindowId(3)).is_some_and(|w| w.parked));
    t.keep(&mut e);
    assert_eq!(kept(&e, 3, &b), Some(b3), "a hidden stage's window counts by its kept frame");

    // The Studio plugged in again: macOS puts the editor back on it, somewhere.
    t.screens(&mut e, vec![studio(), laptop()]);
    e.moved(&[(1, &studio(), (700.0, 500.0))]);
    let Settled::Known(2, note) = t.settle(&mut e) else { panic!("the Studio setup seen before") };
    assert_eq!(note, PutBack { put_back: 3, ..PutBack::default() });
    assert_eq!(e.d.frames.get(&WindowId(1)), Some(&on(&studio(), (200.0, 100.0))), "the editor at its Studio frame");
    assert_eq!(restore(&e, 1), on(&studio(), (200.0, 100.0)));
    assert_eq!(e.m.window(WindowId(1)).map(|w| w.meta.display), Some(STUDIO), "on the same screen");
    for (id, at) in [(2, a2), (3, a3)] {
        let w = e.m.window(WindowId(id)).expect("known");
        assert_eq!(w.restore, at, "w{id} back at its laptop frame of the two-screen setup");
        if !w.parked {
            assert_eq!(e.d.frames.get(&WindowId(id)), Some(&at), "w{id} shown there");
        }
    }

    // Unplugged again: the frames set on the laptop alone come back.
    t.screens(&mut e, vec![laptop_alone()]);
    frame(&mut e, 1, Rect::new(500.0, 300.0, 800.0, 600.0));
    let Settled::Known(1, note) = t.settle(&mut e) else { panic!("the laptop alone seen before") };
    assert_eq!(note.touched, 0);
    assert_eq!(restore(&e, 1), b1, "the editor at its laptop frame");
    assert_eq!(e.d.frames.get(&WindowId(1)), Some(&b1));
    let w3 = e.m.window(WindowId(3)).expect("the notes");
    assert!(w3.parked, "still a card: nothing shows it");
    assert_eq!(w3.restore, b3, "the notes' restore frame is the laptop one, for when it shows");
}

#[test]
fn a_window_the_user_touched_since_the_change_stays_where_it_is() {
    let mut e = Env::both();
    let mut t = Tick::new();
    desk(&mut e);
    t.keep(&mut e);
    t.screens(&mut e, vec![laptop_alone()]);
    t.settle(&mut e);
    frame(&mut e, 1, Rect::new(50.0, 80.0, 1200.0, 900.0));
    t.keep(&mut e);
    t.screens(&mut e, vec![studio(), laptop()]);
    e.moved(&[(1, &studio(), (700.0, 500.0))]);
    // The user drags the editor within the settle.
    t.wait(Duration::from_secs(2));
    let theirs = on(&studio(), (900.0, 600.0));
    frame(&mut e, 1, theirs);
    t.r.touch(WindowId(1), t.now, SETTLE);
    let Settled::Known(_, note) = t.settle(&mut e) else { panic!("seen before") };
    assert_eq!((note.touched, note.put_back), (1, 2));
    assert_eq!(restore(&e, 1), theirs, "left where the user put it");
    assert_eq!(e.d.frames.get(&WindowId(1)), Some(&theirs));
    assert_eq!(kept(&e, 1, &e.m.setup_key().expect("known")), Some(relative_to_studio(theirs)), "and kept there for the setup");
}

#[test]
fn a_window_on_another_screen_than_its_frames_is_never_moved_across() {
    let mut e = Env::both();
    let mut t = Tick::new();
    desk(&mut e);
    t.keep(&mut e);
    t.screens(&mut e, vec![laptop_alone()]);
    t.settle(&mut e);
    t.keep(&mut e);
    // Plugged in again, macOS leaves the editor on the laptop this time.
    t.screens(&mut e, vec![studio(), laptop()]);
    let before = restore(&e, 1);
    let Settled::Known(_, note) = t.settle(&mut e) else { panic!("seen before") };
    assert_eq!(note.elsewhere, 1, "{note:?}");
    assert_eq!(e.m.window(WindowId(1)).map(|w| w.meta.display), Some(LAPTOP), "still on the laptop");
    assert_eq!(restore(&e, 1), before, "never moved to the Studio for its frame there");
}
}

fn relative_to_studio(f: Rect) -> Rect {
    crate::engine::setup_frames::relative(f, studio().frame)
}

#[test]
fn a_window_gone_takes_its_frames_and_the_file_keeps_them_with_its_entry() {
    let mut e = Env::both();
    let mut t = Tick::new();
    desk(&mut e);
    t.keep(&mut e);
    assert!(!e.m.setup_frames_of(WindowId(2)).is_empty());
    let a = e.m.remove_window(WindowId(2));
    e.run(a);
    assert!(e.m.setup_frames_of(WindowId(2)).is_empty(), "gone with its window");
    let file = e.m.to_state();
    let saved = file.all_stages().flat_map(|s| s.windows.iter()).find(|w| w.window == Some(1)).expect("the editor saved");
    assert_eq!(saved.setups, e.m.setup_frames_of(WindowId(1)).to_vec());
    assert_eq!(saved.setups.len(), 1);
}
