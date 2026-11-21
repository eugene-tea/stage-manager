//! The Apple mode's jobs end to end over the fake WindowManager
//! (`engine::testkit::apple`): every 50 ms of its clock the window list is folded into the
//! mirror, the job advanced, and its step run on the fake (a focus, a gesture posted and
//! landed at once), as the platform's controller does on the WM thread.

use std::collections::BTreeSet;

use super::desk::{AppleDesk, InputNow};
use super::gate::{GATE_WAIT, STRIP_WAIT};
use super::job::{Ctx, Job, Next, Report};
use super::mirror::{Arrangement, SETTLE, fold};
use super::plan::{self, Step};
use crate::engine::apple::desk::{Posted, Press};
use crate::engine::geometry::{Point, Rect};
use crate::engine::testkit::apple::FakeWindowManager;
use crate::engine::testkit::display;

fn read(f: &FakeWindowManager, a: &Arrangement) -> Arrangement {
    fold(a, Some(&f.reading()), &f.listed(), &f.window_ids(), &display(), f.now())
}

/// Reads every 50 ms until settled (at most 3 s).
fn settle(f: &mut FakeWindowManager, a: &Arrangement) -> Arrangement {
    let mut a = a.clone();
    for _ in 0..60 {
        f.advance(50);
        a = read(f, &a);
        if a.settled {
            return a;
        }
    }
    panic!("never settled: {a:?}");
}

/// What a drive saw.
#[derive(Debug, Default)]
struct Drive {
    /// Each step run, and when (ms from the drive's start).
    steps: Vec<(u64, Step)>,
    /// How it ended: `Ok` done, `Err` given up.
    end: Option<Result<Report, Report>>,
    /// The frames listed of the stage in front at the start, at every read.
    moved_in_front: BTreeSet<u32>,
}

/// Drives `job` over `f` for at most `max_ms`, `each` called before every read (a test's own
/// change at a given time). Stops when the job ends.
fn drive_with(f: &mut FakeWindowManager, a: &mut Arrangement, job: &mut Job, max_ms: u64, each: impl FnMut(&mut FakeWindowManager, u64)) -> Drive {
    drive_every(f, a, job, max_ms, 50, each)
}

/// The platform's read while a job runs (the fast cadence).
const FAST: u64 = 25;

/// [`drive_with`] reading every `every` ms.
fn drive_every(f: &mut FakeWindowManager, a: &mut Arrangement, job: &mut Job, max_ms: u64, every: u64, mut each: impl FnMut(&mut FakeWindowManager, u64)) -> Drive {
    let mut d = Drive::default();
    let start = f.now();
    let front: Vec<u32> = a.current.clone();
    let frames0 = a.frames.clone();
    for _ in 0..max_ms / every {
        f.advance(every);
        let t = f.now().duration_since(start).as_millis() as u64;
        each(f, t);
        *a = read(f, a);
        for w in &front {
            if let (Some(now), Some(then)) = (a.frames.get(w), frames0.get(w))
                && now != then
            {
                d.moved_in_front.insert(*w);
            }
        }
        let pids = f.pids();
        let next = {
            let cx = Ctx { mirror: a, input: f.input, screen: &display(), pids: &pids, in_flight: false };
            job.advance(&cx, f.now())
        };
        match next {
            Next::Run(step) => {
                let ok = match &step {
                    Step::Focus(w) => f.focus(*w).is_ok(),
                    Step::Post(p) => f.post(p).is_ok(),
                    Step::Unhide(pid) => f.unhide(*pid).is_ok(),
                    Step::AwaitSettle => true,
                };
                job.ran(&step, ok);
                if let Some(l) = f.landed() {
                    job.landed(&l);
                }
                a.step_began(f.now());
                d.steps.push((t, step));
            }
            Next::Wait(_) => {}
            Next::Done(r) => {
                d.end = Some(Ok(r));
                return d;
            }
            Next::GiveUp(r) => {
                d.end = Some(Err(r));
                return d;
            }
        }
    }
    d
}

fn drive(f: &mut FakeWindowManager, a: &mut Arrangement, job: &mut Job, max_ms: u64) -> Drive {
    drive_with(f, a, job, max_ms, |_, _| {})
}

/// Every Shift-click run, in order, across the presses.
fn clicks(steps: &[(u64, Step)]) -> Vec<u32> {
    presses(steps).into_iter().flatten().collect()
}

/// Each Shift press run: the windows it clicked.
fn presses(steps: &[(u64, Step)]) -> Vec<Vec<u32>> {
    steps.iter().filter_map(|(_, s)| if let Step::Post(Posted::Clicks { presses, .. }) = s { Some(presses.iter().map(|p| p.window).collect()) } else { None }).collect()
}

/// When (ms from the drive's start) each step of kind `name` ran.
fn times(steps: &[(u64, Step)], name: &str) -> Vec<u64> {
    steps.iter().filter(|(_, s)| super::job::step_name(s) == name).map(|(t, _)| *t).collect()
}

fn sorted(w: &[u32]) -> Vec<u32> {
    let mut v = w.to_vec();
    v.sort_unstable();
    v
}

/// Alpha (windows 1, 2), Bravo (3), Charlie (4) opened in turn: Charlie in front, the strip
/// Bravo then Alpha.
fn three() -> (FakeWindowManager, Arrangement) {
    let mut f = FakeWindowManager::new();
    f.open_app(10, &[1, 2]);
    f.open_app(20, &[3]);
    f.open_app(30, &[4]);
    let a = settle(&mut f, &Arrangement::default());
    (f, a)
}

/// The eighth probe of 11-17: only an item's front window takes a click, so the window
/// behind it in a stage of two has no point of its own, whatever part of it shows.
#[test]
fn the_mirror_gives_a_point_to_an_items_front_only() {
    let (f, a) = three();
    let listed = f.listed();
    let bounds = |id: u32| listed.windows.iter().find(|w| w.id == id).map(|w| w.bounds).expect("listed");
    assert!(bounds(1).contains(a.click_at[&1]), "{:?}", a.click_at);
    assert!(!a.click_at.contains_key(&2), "the window behind: no point: {:?}", a.click_at);
    assert!(a.click_at.contains_key(&3), "a stage of one: its front");
    assert!(!a.click_at.contains_key(&4), "the window in front has no thumbnail");
}

/// A stage of two in one item brought here: its front by one press, the other at a fresh read
/// once Apple's add motion has settled (the eighth probe: only an item's front takes a click),
/// each landing on top; then the order put back by a focus of the one wanted on top, the
/// stage in front whole.
#[test]
fn a_stage_of_two_comes_in_front_first_then_its_order_is_put_back() {
    let (mut f, mut a) = three();
    let mut job = plan::merge(&a, 2, f.now()).expect("stage 2 is Alpha's");
    let d = drive(&mut f, &mut a, &mut job, 8000);
    let report = d.end.expect("ended").expect("done");
    assert_eq!(presses(&d.steps), vec![vec![1], vec![2]], "the item's front, then the other");
    assert_eq!((report.clicks, report.presses, report.focuses), (2, 2, 1));
    assert_eq!(f.focused, vec![1], "the order put back: the one wanted on top focused");
    assert_eq!(f.current(), &[1, 2, 4], "the two in their order, on top");
    assert_eq!((f.ignored_mid_motion, f.back_refused), (0, 0), "no click mid-motion, none behind a front");
    assert!(matches!(f.posted[..], [Posted::Clicks { own_shift: true, .. }, Posted::Clicks { own_shift: true, .. }]), "a Shift of ours around each press: {:?}", f.posted);
}

/// Merge 1 of the live run of 11-17, a stage of three in one item: three presses, one window
/// each, each at a fresh read; then its order put back, back to front, the one wanted on top
/// last; done once the list shows it so.
#[test]
fn a_stage_of_three_in_one_item_comes_in_by_three_presses_and_its_order_is_put_back() {
    let mut f = FakeWindowManager::new();
    f.open_app(10, &[1, 2, 3]);
    f.open_app(20, &[4]);
    let mut a = settle(&mut f, &Arrangement::default());
    let points: Vec<_> = [1, 2, 3].iter().map(|w| a.click_at.get(w).copied()).collect();
    assert!(points[0].is_some() && points[1..].iter().all(Option::is_none), "the item's front alone has a point: {points:?}");
    let mut job = plan::merge(&a, 1, f.now()).expect("stage 1 is the app's of three");
    let d = drive(&mut f, &mut a, &mut job, 10_000);
    let r = d.end.expect("ended").expect("done");
    assert_eq!(presses(&d.steps), vec![vec![1], vec![2], vec![3]], "one per press, front first");
    // In front after the presses: 3, 2, 1, 4. Back to front: 2, then 1 on top.
    assert_eq!(f.focused, vec![2, 1]);
    assert_eq!(f.current(), &[1, 2, 3, 4], "in their order, on top");
    assert_eq!(r.focuses, 2);
    assert_eq!(f.back_refused, 0);
}

/// Thumbnails stacked exactly (windows at one frame, live 11-16): the one behind shows no
/// part of its own, so the press takes the front one, and the next press, at the next settle,
/// the one it uncovered.
#[test]
fn a_thumbnail_fully_under_another_is_clicked_by_a_second_press_once_uncovered() {
    let mut f = FakeWindowManager::new();
    f.stack = 0.0;
    f.open_app(10, &[1, 2]);
    f.open_app(20, &[4]);
    let mut a = settle(&mut f, &Arrangement::default());
    assert!(a.click_at.contains_key(&1) && !a.click_at.contains_key(&2), "{:?}", a.click_at);
    let mut job = plan::merge(&a, 1, f.now()).expect("planned");
    let d = drive(&mut f, &mut a, &mut job, 6000);
    let r = d.end.expect("ended").expect("done");
    assert_eq!(presses(&d.steps), vec![vec![1], vec![2]]);
    assert_eq!((r.clicks, r.presses), (2, 2));
    assert_eq!(sorted(f.current()), vec![1, 2, 4]);
}

#[test]
fn a_click_is_held_back_while_apple_moves_windows() {
    let (mut f, a) = three();
    let mut job = plan::merge(&a, 2, f.now()).expect("planned at rest");
    // Apple's motion begins (an activation of Bravo's) just as the job starts.
    f.focus_window(3);
    let mut a = read(&f, &a);
    let d = drive(&mut f, &mut a, &mut job, 8000);
    d.end.expect("ended").expect("done");
    assert_eq!(f.ignored_mid_motion, 0, "nothing was posted mid-motion");
    assert!(d.steps[0].0 >= SETTLE.as_millis() as u64, "the first click after the motion's settle ({} ms)", d.steps[0].0);
    assert!([1, 2, 4].iter().all(|w| f.current().contains(w)), "Apple wins: the job goes on from what is there: {:?}", f.current());
}

#[test]
fn a_covered_strip_is_waited_for_until_it_shows() {
    let (mut f, a) = three();
    f.cover(true);
    let mut a = settle(&mut f, &a);
    let mut job = plan::merge(&a, 2, f.now()).expect("planned");
    let d = drive_with(&mut f, &mut a, &mut job, 8000, |f, t| {
        if t == 2000 {
            f.cover(false);
        }
    });
    d.end.expect("ended").expect("done once the strip showed");
    assert!(d.steps[0].0 > 2000, "nothing posted while covered");
    assert_eq!(sorted(f.current()), vec![1, 2, 4]);
}

#[test]
fn a_strip_that_stays_covered_gives_the_job_up_after_a_few_seconds() {
    let (mut f, a) = three();
    f.cover(true);
    let mut a = settle(&mut f, &a);
    let mut job = plan::merge(&a, 2, f.now()).expect("planned");
    let d = drive(&mut f, &mut a, &mut job, 10_000);
    let r = d.end.expect("ended").expect_err("given up");
    assert!(r.given_up.as_deref().is_some_and(|w| w.contains("covered")), "{r:?}");
    assert!(r.ms >= STRIP_WAIT.as_millis() as u64 && r.ms < STRIP_WAIT.as_millis() as u64 + 1000, "{} ms", r.ms);
    assert!(f.posted.is_empty());
}

#[test]
fn the_persons_input_holds_a_click_back_and_a_wait_past_its_bound_gives_up() {
    let held = [
        InputNow { button_down: true, ..InputNow::idle() },
        InputNow { command_keys: true, ..InputNow::idle() },
        InputNow { pointer_still_ms: 10, ..InputNow::idle() },
        InputNow { key_idle_ms: 20, ..InputNow::idle() },
        InputNow { locked: true, ..InputNow::idle() },
        InputNow { overview: true, ..InputNow::idle() },
    ];
    for input in held {
        let (mut f, mut a) = three();
        f.input = input;
        let mut job = plan::merge(&a, 2, f.now()).expect("planned");
        let d = drive(&mut f, &mut a, &mut job, 5000);
        let r = d.end.expect("ended").expect_err("given up");
        assert!(f.posted.is_empty(), "{input:?}: nothing posted");
        assert!(r.ms >= GATE_WAIT.as_millis() as u64 && r.ms < GATE_WAIT.as_millis() as u64 + 500, "{input:?}: bounded ({} ms)", r.ms);
    }
    // Let go in time: the click goes.
    let (mut f, mut a) = three();
    f.input = InputNow { button_down: true, ..InputNow::idle() };
    let mut job = plan::merge(&a, 2, f.now()).expect("planned");
    let d = drive_with(&mut f, &mut a, &mut job, 6000, |f, t| {
        if t == 800 {
            f.input = InputNow::idle();
        }
    });
    d.end.expect("ended").expect("done");
    assert!(d.steps[0].0 >= 800);
}

#[test]
fn a_shift_the_person_holds_goes_with_no_shift_of_ours() {
    let (mut f, mut a) = three();
    f.input = InputNow { shift: true, ..InputNow::idle() };
    let mut job = plan::merge(&a, 1, f.now()).expect("stage 1 is Bravo's");
    drive(&mut f, &mut a, &mut job, 4000).end.expect("ended").expect("done");
    assert!(matches!(f.posted[..], [Posted::Clicks { own_shift: false, .. }]), "{:?}", f.posted);
}

#[test]
fn a_step_apple_does_not_answer_runs_twice_then_the_job_is_given_up() {
    let (mut f, mut a) = three();
    f.deaf = true;
    let mut job = plan::merge(&a, 1, f.now()).expect("planned");
    let d = drive(&mut f, &mut a, &mut job, 6000);
    let r = d.end.expect("ended").expect_err("given up");
    assert_eq!(f.posted.len(), 2, "once more after the settle, never blindly again");
    assert!(r.given_up.as_deref().is_some_and(|w| w.contains("2 tries")), "{r:?}");
}

#[test]
fn a_drag_takes_a_window_out_to_a_stage_of_its_own() {
    let mut f = FakeWindowManager::new();
    f.open_app(20, &[3]);
    f.open_app(10, &[1, 2]);
    let mut a = settle(&mut f, &Arrangement::default());
    let mut job = plan::new_stage(&a, 1, f.now()).expect("planned");
    let d = drive(&mut f, &mut a, &mut job, 4000);
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.drags, 1);
    assert_eq!(f.current(), &[2]);
    assert!(f.stages().iter().any(|st| st == &vec![1]), "a stage of its own: {:?}", f.stages());
    let Some(Posted::Drag { from, to, .. }) = f.posted.first() else { panic!("a drag") };
    let frame = f.frame(1).expect("framed");
    assert!(from.y > frame.y && from.y < frame.y + 20.0, "the title bar: {from:?}");
    // Live 11-16: dropped at the grab's height, above the strip's items, the drag did
    // nothing; dropped among them it works (the first probe).
    let item = a.strip[0].frame;
    assert!(to.x > item.x && to.x < item.max_x() && to.y >= item.y && to.y <= item.max_y(), "among the strip's items: {to:?} in {item:?}");
}

/// A drag Apple's takes as a plain move leaves the window moved: given up after its tries,
/// the window is set back to its frame from before the first drag.
#[test]
fn a_drag_that_only_moved_the_window_sets_it_back_when_given_up() {
    let mut f = FakeWindowManager::new();
    f.open_app(20, &[3]);
    f.open_app(10, &[1, 2]);
    let mut a = settle(&mut f, &Arrangement::default());
    let before = a.frames[&1];
    f.deaf = true;
    let mut job = plan::new_stage(&a, 1, f.now()).expect("planned");
    let d = drive_with(&mut f, &mut a, &mut job, 6000, |f, _| {
        if !f.posted.is_empty() {
            f.set_frame(1, Rect::new(-121.0, 87.0, 700.0, 500.0));
        }
    });
    let r = d.end.expect("ended").expect_err("given up");
    assert_eq!(r.drags, 2);
    assert_eq!(r.set_back, Some((1, before)));
}

/// Live 11-16: the fixture windows open at one frame; the window to take out was under
/// another all along its title bar, and the drag's guard refused it twice ("the topmost window
/// is not the one meant"). It is raised first, then dragged once the list shows it on top.
#[test]
fn a_window_whose_title_bar_is_covered_is_raised_before_the_drag() {
    let mut f = FakeWindowManager::new();
    f.open_app(20, &[3]);
    f.open_app(10, &[1, 2]);
    let one = Rect::new(80.0, 87.0, 520.0, 392.0);
    f.set_frame(1, one);
    f.set_frame(2, one);
    let mut a = settle(&mut f, &Arrangement::default());
    assert_eq!(a.current, vec![1, 2], "window 2 under window 1");
    assert!(a.title_at.contains_key(&1) && !a.title_at.contains_key(&2), "{:?}", a.title_at);
    let mut job = plan::new_stage(&a, 2, f.now()).expect("planned");
    let d = drive(&mut f, &mut a, &mut job, 5000);
    let r = d.end.expect("ended").expect("done");
    let kinds: Vec<&str> = d.steps.iter().map(|(_, s)| super::job::step_name(s)).collect();
    assert_eq!(kinds, vec!["focus", "drag"], "raised, then dragged");
    assert_eq!((r.focuses, r.drags), (1, 1));
    assert_eq!(f.current(), &[1]);
    assert!(f.stages().iter().any(|st| st == &vec![2]), "a stage of its own: {:?}", f.stages());
}

/// A drag its guard refuses (another window topmost at the grab point) is tried once more,
/// then given up with the guard's words, not as Apple's silence.
#[test]
fn a_drag_its_guard_refuses_is_given_up_with_the_guards_words() {
    let mut f = FakeWindowManager::new();
    f.open_app(10, &[1, 2]);
    let a0 = settle(&mut f, &Arrangement::default());
    let mut job = plan::new_stage(&a0, 2, f.now()).expect("planned");
    // Every read takes a point of window 1's title bar for window 2's (a covering window the
    // list did not show): the fake's guard refuses it.
    let on_one = Point::new(a0.frames[&1].x + 40.0, a0.frames[&1].y + 8.0);
    let mut d = Drive::default();
    let mut a = a0;
    for _ in 0..100 {
        f.advance(50);
        a = read(&f, &a);
        a.title_at.insert(2, on_one);
        let pids = f.pids();
        let next = job.advance(&Ctx { mirror: &a, input: f.input, screen: &display(), pids: &pids, in_flight: false }, f.now());
        match next {
            Next::Run(step) => {
                let ok = match &step {
                    Step::Focus(w) => f.focus(*w).is_ok(),
                    Step::Post(p) => f.post(p).is_ok(),
                    Step::Unhide(pid) => f.unhide(*pid).is_ok(),
                    Step::AwaitSettle => true,
                };
                job.ran(&step, ok);
                if let Some(l) = f.landed() {
                    job.landed(&l);
                }
                a.step_began(f.now());
                d.steps.push((0, step));
            }
            Next::GiveUp(r) => {
                d.end = Some(Err(r));
                break;
            }
            Next::Done(r) => panic!("the guard should have refused: {r:?}"),
            Next::Wait(_) => {}
        }
    }
    let r = d.end.expect("ended").expect_err("given up");
    assert_eq!(f.posted.len(), 2, "tried once more");
    assert!(r.given_up.as_deref().is_some_and(|w| w.contains("not posted") && w.contains("topmost")), "{r:?}");
}

#[test]
fn a_drag_the_persons_own_input_ends_gives_the_job_up_at_once() {
    let mut f = FakeWindowManager::new();
    f.open_app(10, &[1, 2]);
    let mut a = settle(&mut f, &Arrangement::default());
    f.interrupt_drags = true;
    let mut job = plan::new_stage(&a, 1, f.now()).expect("planned");
    let d = drive(&mut f, &mut a, &mut job, 4000);
    let r = d.end.expect("ended").expect_err("given up");
    assert_eq!(f.posted.len(), 1, "never tried again");
    assert!(r.given_up.as_deref().is_some_and(|w| w.contains("own input")), "{r:?}");
    assert_eq!(sorted(f.current()), vec![1, 2], "the window stays");
}

#[test]
fn a_window_sent_to_another_stage_goes_by_a_switch_a_click_and_a_switch_back() {
    let mut f = FakeWindowManager::new();
    f.open_app(20, &[3]);
    f.open_app(10, &[1, 2]);
    let mut a = settle(&mut f, &Arrangement::default());
    let mut job = plan::send(&a, 1, 1, |_| None, f.now()).expect("window 1 to Bravo's stage");
    let d = drive(&mut f, &mut a, &mut job, 8000);
    d.end.expect("ended").expect("done");
    let kinds: Vec<&str> = d.steps.iter().map(|(_, s)| super::job::step_name(s)).collect();
    assert_eq!(kinds, vec!["focus", "shift_click", "focus"]);
    assert_eq!(f.current(), &[2], "back where the person was");
    assert!(f.stages().iter().any(|st| sorted(st) == vec![1, 3]), "{:?}", f.stages());
}

#[test]
fn a_new_app_hidden_at_its_launch_comes_in_with_one_click_and_the_stage_in_front_never_moves() {
    let (mut f, mut a) = three();
    let mut job = plan::open_here(&a, 40, f.now()).expect("a stage in front");
    f.launch(40, &[5], true);
    let d = drive(&mut f, &mut a, &mut job, 6000);
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("hide"));
    assert_eq!(clicks(&d.steps), vec![5]);
    assert_eq!(d.steps.last().map(|(_, s)| s.clone()), Some(Step::Focus(5)), "focused last: it has the keyboard");
    assert_eq!(sorted(f.current()), vec![4, 5]);
    assert!(d.moved_in_front.is_empty(), "the stage in front never moved: {:?}", d.moved_in_front);
    assert!(!f.hidden.contains(&40), "shown again");
    assert!(r.first_window_ms.is_some());
    // Live 11-16: the window joined about 780 ms after the click and the job said done 1.2 s
    // later (a settle, the focus, another settle). The focus goes at the first read that shows
    // it joined, and the job ends with it.
    let (click, focus) = (times(&d.steps, "shift_click")[0], times(&d.steps, "focus")[0]);
    assert!(focus - click <= SETTLE.as_millis() as u64 + 100, "focused {} ms after the click", focus - click);
    assert!(r.ms <= focus + 100, "done {} ms after the focus", r.ms - focus);
}

#[test]
fn a_new_app_whose_hide_came_too_late_comes_back_by_the_round_trip() {
    let (mut f, mut a) = three();
    let mut job = plan::open_here(&a, 40, f.now()).expect("a stage in front");
    f.launch(40, &[5], false);
    let d = drive(&mut f, &mut a, &mut job, 6000);
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("round_trip"));
    let kinds: Vec<&str> = d.steps.iter().map(|(_, s)| super::job::step_name(s)).collect();
    assert_eq!(kinds, vec!["focus", "shift_click", "focus"], "{:?}", d.steps);
    assert_eq!(d.steps[0].1, Step::Focus(4), "the stage pushed into the strip brought back by a switch");
    assert_eq!(clicks(&d.steps), vec![5], "then the app's window Shift-clicked in");
    assert_eq!(d.steps.last().map(|(_, s)| s.clone()), Some(Step::Focus(5)));
    assert_eq!(f.current(), &[5, 4], "the app on top of the stage");
}

#[test]
fn every_window_of_a_new_app_joins() {
    let (mut f, mut a) = three();
    let mut job = plan::open_here(&a, 40, f.now()).expect("a stage in front");
    f.launch(40, &[5, 6], true);
    let d = drive_with(&mut f, &mut a, &mut job, 8000, |f, t| {
        if t == 1500 {
            f.open_window(40, 7);
        }
    });
    d.end.expect("ended").expect("done");
    // One item of the app's two: its front, then the other once it is the item's front.
    assert_eq!(presses(&d.steps), vec![vec![5], vec![6]]);
    assert_eq!(f.current(), &[5, 6, 7, 4], "a window opened meanwhile joins too; the app's front on top, its order put back");
    assert_eq!(f.back_refused, 0, "no click on a window behind its item's front");
}

#[test]
fn a_new_app_waits_for_its_first_window_and_a_covered_strip_leaves_apples_own() {
    let (mut f, a) = three();
    f.cover(true);
    let mut a = settle(&mut f, &a);
    let mut job = plan::open_here(&a, 40, f.now()).expect("a stage in front");
    job.hidden = true;
    let d = drive_with(&mut f, &mut a, &mut job, 12_000, |f, t| {
        if t == 1000 {
            f.launch(40, &[5], true);
        }
    });
    let r = d.end.expect("ended").expect_err("given up");
    assert!(r.given_up.as_deref().is_some_and(|w| w.contains("covered")), "{r:?}");
    assert_eq!(r.undo, Some(Step::Focus(5)), "its stage brought in front, as Apple's own would");
    assert_eq!(r.first_window_ms.map(|ms| ms >= 1000), Some(true));
    assert!(f.posted.is_empty());
}

#[test]
fn a_new_app_that_never_shows_a_window_is_unhidden() {
    let (mut f, mut a) = three();
    let mut job = plan::open_here(&a, 40, f.now()).expect("a stage in front");
    job.hidden = true;
    let d = drive(&mut f, &mut a, &mut job, 20_000);
    let r = d.end.expect("ended").expect_err("given up");
    assert_eq!(r.unhide, Some(40));
    assert_eq!(r.undo, None);
}

// ── A running app's reopened window ─────────────────────────────────────────────────────

/// The sixth probe of 11-16 (the owner's smoke test): a running app with no window, activated
/// under a `current` rule, hidden at its activation; its reopened window shows nowhere until
/// the app is shown again without activating it, then comes in from the strip with one
/// Shift-click and is focused. The stage in front never moves.
#[test]
fn a_running_apps_reopened_window_comes_in_hidden_at_its_activation_and_the_stage_in_front_never_moves() {
    let (mut f, mut a) = three();
    let mut job = plan::reopen_here(&a, 40, &f.pids(), f.now()).expect("a stage in front, no window of the app");
    // The platform's hide thread: the hide took at the activation, the window came after.
    job.hide_took(f.now());
    let d = drive_with(&mut f, &mut a, &mut job, 6000, |f, t| {
        if t == 100 {
            f.reopen(40, 5, true);
        }
    });
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("hide_activation"));
    let kinds: Vec<&str> = d.steps.iter().map(|(_, s)| super::job::step_name(s)).collect();
    assert_eq!(kinds, vec!["unhide", "shift_click", "focus"], "{:?}", d.steps);
    assert_eq!(f.unhidden, vec![40], "shown again once, by us");
    assert_eq!(clicks(&d.steps), vec![5]);
    assert_eq!(d.steps.last().map(|(_, s)| s.clone()), Some(Step::Focus(5)), "focused last: it has the keyboard");
    assert_eq!(sorted(f.current()), vec![4, 5]);
    assert!(d.moved_in_front.is_empty(), "the stage in front never moved: {:?}", d.moved_in_front);
    assert!(!f.hidden.contains(&40));
    assert!(r.unhide_ms.is_some_and(|ms| ms >= 100), "{r:?}");
    assert!(r.first_window_ms.is_some());
}

/// The owner's smoke test of 11-16 (Activity Monitor, Finder): the app reopens the window it
/// ordered out, an id seen before, and while the app is hidden Accessibility reads it as a
/// dialog, so the app manages no window of its own. The app is shown again a moment after the
/// hide took, never waiting for a window it can recognise; the thumbnail of its pid is
/// Shift-clicked in, whatever its id or subrole, then focused. The stage in front never moves.
#[test]
fn a_reopened_window_with_a_reused_id_read_as_a_dialog_while_hidden_still_comes_in() {
    let (mut f, a) = three();
    // Delta's window 5 was open once, then ordered out: the app runs with no window.
    f.open_app(40, &[5]);
    let a = settle(&mut f, &a);
    f.focus_window(4);
    let a = settle(&mut f, &a);
    f.close(5);
    let mut a = settle(&mut f, &a);
    let mut job = plan::reopen_here(&a, 40, &f.pids(), f.now()).expect("a stage in front, no window of the app");
    job.hide_took(f.now());
    // When its thumbnail's twin is first listed (Apple's motion into the strip over).
    let mut twin: Option<u64> = None;
    let d = drive_every(&mut f, &mut a, &mut job, 6000, FAST, |f, t| {
        if t == 25 {
            // The same id again, read as a dialog while the app is hidden.
            f.loose.insert(5);
            f.reopen(40, 5, true);
        }
        if twin.is_none() && f.listed().windows.iter().any(|w| w.id == 9005) {
            twin = Some(t);
        }
    });
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("hide_activation"));
    let kinds: Vec<&str> = d.steps.iter().map(|(_, s)| super::job::step_name(s)).collect();
    assert_eq!(kinds, vec!["unhide", "shift_click", "focus"], "{:?}", d.steps);
    let unhide = times(&d.steps, "unhide")[0];
    assert!(unhide <= 300, "shown again {unhide} ms after the hide took");
    let (twin, click) = (twin.expect("its twin listed"), times(&d.steps, "shift_click")[0]);
    assert!(click >= twin && click - twin <= 100, "pressed {} ms after its twin came", click - twin);
    assert_eq!(presses(&d.steps), vec![vec![5]], "its thumbnail, by its pid, in one press");
    assert_eq!(d.steps.last().map(|(_, s)| s.clone()), Some(Step::Focus(5)), "focused last");
    let focus = times(&d.steps, "focus")[0];
    assert!(focus - click <= super::job::PRESS_AGAIN.as_millis() as u64 + 100, "focused {} ms after the press", focus - click);
    assert_eq!(sorted(f.current()), vec![4, 5]);
    assert!(d.moved_in_front.is_empty(), "the stage in front never moved: {:?}", d.moved_in_front);
    assert!(r.given_up.is_none() && f.unhidden == vec![40]);
}

/// A launch under a rule's `current` of an app that makes no window while hidden (Reminders,
/// live 11-16: given up after 10 s): shown again without activating it a moment after the hide
/// took, never waiting for a first window, then its thumbnail taken in by its pid as a running
/// app's is.
#[test]
fn a_launch_that_makes_no_window_while_hidden_is_shown_again_and_its_thumbnail_taken_in() {
    let (mut f, mut a) = three();
    let mut job = plan::open_here(&a, 40, f.now()).expect("a stage in front");
    f.launch_windowless(40, &[5]);
    job.hide_took(f.now());
    let mut twin: Option<u64> = None;
    let d = drive_every(&mut f, &mut a, &mut job, 6000, FAST, |f, t| {
        if twin.is_none() && f.listed().windows.iter().any(|w| w.id == 9005) {
            twin = Some(t);
        }
    });
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("hide"));
    let kinds: Vec<&str> = d.steps.iter().map(|(_, s)| super::job::step_name(s)).collect();
    assert_eq!(kinds, vec!["unhide", "shift_click", "focus"], "{:?}", d.steps);
    assert!(times(&d.steps, "unhide")[0] <= 300);
    let (twin, click) = (twin.expect("its twin listed"), times(&d.steps, "shift_click")[0]);
    assert!(click - twin <= 100, "pressed {} ms after its twin came", click - twin);
    assert_eq!(sorted(f.current()), vec![4, 5]);
    assert!(d.moved_in_front.is_empty());
    assert!(r.ms < 2500, "done in {} ms", r.ms);
}

/// The fallback: the window showed before the hide took, Apple's switched to the app's stage
/// (the stage in front into the strip); after the settle the stage that left comes back by a
/// switch, all its windows in their order, and the app's window, in the strip now, in one
/// Shift press, so the result is one stage with both, the app on top.
#[test]
fn a_running_apps_window_shown_before_the_hide_took_comes_back_by_the_round_trip() {
    let mut f = FakeWindowManager::new();
    f.open_app(20, &[3]);
    f.open_app(10, &[1, 2]);
    let mut a = settle(&mut f, &Arrangement::default());
    let mut job = plan::reopen_here(&a, 40, &f.pids(), f.now()).expect("a stage in front");
    // The activation stirs the mirror (in motion from it, as the platform's watch has it);
    // the window shows at once, the hide not taken.
    a.step_began(f.now());
    f.reopen(40, 5, false);
    let d = drive(&mut f, &mut a, &mut job, 6000);
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("round_trip"));
    assert_eq!(d.steps[0].1, Step::Focus(1), "the stage that left brought back by a switch to its front window");
    assert_eq!(presses(&d.steps), vec![vec![5]], "the app's window in one press");
    assert!(f.unhidden.is_empty(), "nothing hidden, nothing shown again");
    assert_eq!(d.steps.last().map(|(_, s)| s.clone()), Some(Step::Focus(5)));
    assert_eq!(f.current(), &[5, 1, 2], "one stage with both, in its order, the app on top");
}

/// Live 11-17 (Contacts, running with no window, a `current` rule): Apple's switched at the
/// activation itself, the stage in front into the strip and none in front, and the hide took
/// after. The app is shown again all the same, its window comes in front, and the round trip
/// brings the stage that left back with the app's window on top; never a 3 s wait for a window
/// of an app still hidden.
#[test]
fn a_running_app_apple_switched_to_before_the_hide_took_is_shown_again_for_the_round_trip() {
    let (mut f, mut a) = three();
    let before = f.current().to_vec();
    let mut job = plan::reopen_here(&a, 40, &f.pids(), f.now()).expect("a stage in front, no window of the app");
    a.step_began(f.now());
    f.switch_to_empty();
    job.hide_took(f.now());
    f.reopen_hidden(40, 5);
    let d = drive_every(&mut f, &mut a, &mut job, 6000, FAST, |_, _| {});
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("round_trip"), "{r:?}");
    assert!(r.round_trip_why.is_some(), "{r:?}");
    assert_eq!(f.unhidden, vec![40], "shown again, once");
    assert!(times(&d.steps, "unhide")[0] <= 400, "{:?}", d.steps);
    assert_eq!(d.steps.last().map(|(_, s)| s.clone()), Some(Step::Focus(5)), "{:?}", d.steps);
    let mut want = before.clone();
    want.insert(0, 5);
    assert_eq!(f.current(), want.as_slice(), "one stage with both, in its order, the app on top");
    assert!(r.ms < 3000, "done in {} ms", r.ms);
}

/// Finder (live 11-16): never hidden (it owns the Desktop; hidden, its stage read empty), or
/// its window showed before the hide took (`TooLate`). It reuses the window it ordered out,
/// an id seen before and never tracked anew as a standard window, so no first window is ever
/// recognised: the app's window in front is taken by its pid, the stage that left brought
/// back by a switch, the window Shift-clicked in from the strip by its pid, then focused.
#[test]
fn finders_reused_window_shown_before_any_hide_comes_back_by_the_round_trip_by_its_pid() {
    let (mut f, a) = three();
    f.open_app(40, &[5]);
    let a = settle(&mut f, &a);
    f.focus_window(4);
    let a = settle(&mut f, &a);
    f.close(5);
    let mut a = settle(&mut f, &a);
    assert!(!plan::may_hide("com.apple.finder"));
    let mut job = plan::reopen_here(&a, 40, &f.pids(), f.now()).expect("a stage in front, no window of the app");
    // Never hidden: the window shows at once, Apple's switches to a stage of its own.
    a.step_began(f.now());
    f.loose.insert(5);
    f.reopen(40, 5, false);
    let d = drive_every(&mut f, &mut a, &mut job, 6000, FAST, |_, _| {});
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("round_trip"));
    assert!(r.round_trip_why.is_some_and(|w| w.contains("before it was hidden")), "{r:?}");
    assert_eq!(d.steps[0].1, Step::Focus(4), "the stage that left brought back by a switch");
    assert_eq!(presses(&d.steps), vec![vec![5]], "the app's window by its pid, in one press");
    assert_eq!(d.steps.last().map(|(_, s)| s.clone()), Some(Step::Focus(5)));
    assert_eq!(f.current(), &[5, 4], "one stage with both");
    assert!(f.unhidden.is_empty());
    assert!(r.ms < 3000, "done in {} ms", r.ms);
}

/// Apple's draws three thumbnails an item (live 11-17): a stage's fourth window is listed off
/// screen within the item. It is of that item, at its back, with no point of its own, never a
/// stage of its own (the mirror had it as a remembered stage, counted by the hotkeys).
#[test]
fn a_window_apple_draws_no_thumbnail_of_is_of_its_item_not_a_stage_of_its_own() {
    let mut f = FakeWindowManager::new();
    f.open_app(10, &[1, 2, 3, 4]);
    f.open_app(20, &[5]);
    let a = settle(&mut f, &Arrangement::default());
    assert_eq!(a.strip.iter().map(|g| g.windows.clone()).collect::<Vec<_>>(), vec![vec![1, 2, 3, 4]], "front to back, the undrawn one at the back");
    assert!(a.remembered.is_empty(), "{:?}", a.remembered);
    assert!(a.click_at.contains_key(&1) && a.click_at.len() == 1, "the item's front alone has a point: {:?}", a.click_at);
    assert!(a.by_number(2).is_none(), "one stage to count");
}

/// The owner's smoke test of 11-17 (Activity Monitor under a `current` rule): a stage of four
/// in front, its fourth window drawn in no thumbnail once in the strip; the new app's window
/// showed before any hide, so Apple's switched to it. The stage comes back whole by one
/// switch to its front window, in its order, the undrawn one with it, and the app's window in
/// one Shift press, on top (live: three presses, 7.3 s, and the stage came back reversed).
#[test]
fn a_stage_of_four_left_for_a_new_app_comes_back_whole_in_its_order_with_the_app_on_top() {
    let mut f = FakeWindowManager::new();
    f.open_app(20, &[5]);
    f.open_app(10, &[1, 2, 3, 4]);
    let mut a = settle(&mut f, &Arrangement::default());
    let mut job = plan::open_here(&a, 40, f.now()).expect("a stage in front");
    a.step_began(f.now());
    f.launch(40, &[9], false);
    let d = drive_every(&mut f, &mut a, &mut job, 6000, FAST, |_, _| {});
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("round_trip"));
    let kinds: Vec<&str> = d.steps.iter().map(|(_, s)| super::job::step_name(s)).collect();
    assert_eq!(kinds, vec!["focus", "shift_click", "focus"], "{:?}", d.steps);
    assert_eq!(d.steps[0].1, Step::Focus(1), "the stage that left, by a switch to its front window");
    assert_eq!(presses(&d.steps), vec![vec![9]], "one press: the app's window alone");
    assert_eq!((r.clicks, r.presses), (1, 1));
    assert_eq!(f.current(), &[9, 1, 2, 3, 4], "the stage in its order, the app on top");
    assert!(r.ms < 3000, "done in {} ms", r.ms);
}

/// The eighth probe of 11-17: a click on the part of its own a window behind its item's front
/// shows takes nothing (the fake refuses it as Apple's does); the merge clicks each window once
/// it is its item's front, the item laid out again after each press (live 11-17), and the
/// stage ends in its order.
#[test]
fn a_click_behind_an_items_front_takes_nothing_and_the_merge_waits_for_each_to_be_front() {
    let mut f = FakeWindowManager::new();
    f.relayout = true;
    f.open_app(10, &[1, 2, 3]);
    f.open_app(20, &[4]);
    let mut a = settle(&mut f, &Arrangement::default());
    // The part of its own the window behind shows: right of the front one.
    let listed = f.listed();
    let bounds = |id: u32| listed.windows.iter().find(|w| w.id == id).map(|w| w.bounds).expect("listed");
    let (front, back) = (bounds(1), bounds(2));
    let at = Point::new((front.max_x() + back.max_x()) / 2.0, back.center().y);
    assert!(back.contains(at) && !front.contains(at));
    let mut before = f.clone();
    before.post(&Posted::Clicks { presses: vec![Press { window: 2, at }], own_shift: true }).expect("posted");
    assert_eq!((before.current(), before.back_refused), (&[4][..], 1), "nothing joined: {:?}", before.stages());
    let mut job = plan::merge(&a, 1, f.now()).expect("planned");
    let d = drive(&mut f, &mut a, &mut job, 10_000);
    let r = d.end.expect("ended").expect("done");
    assert_eq!(presses(&d.steps), vec![vec![1], vec![2], vec![3]]);
    assert_eq!((r.presses, f.back_refused), (3, 0));
    assert_eq!(f.current(), &[1, 2, 3, 4], "in their order, on top");
}

/// "Open here" of an app with two windows, hidden at its launch: its item's front by one
/// press, the other by the next, then the order put back, the app's front window focused last
/// (the focus asked), on top of the stage it was asked into, which never moved.
#[test]
fn an_app_of_two_windows_opened_here_comes_in_by_two_presses_its_front_on_top() {
    let (mut f, mut a) = three();
    let mut job = plan::open_here(&a, 40, f.now()).expect("a stage in front");
    job.hide_took(f.now());
    f.launch(40, &[5, 6], true);
    let d = drive_every(&mut f, &mut a, &mut job, 8000, FAST, |_, _| {});
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("hide"));
    assert_eq!(presses(&d.steps), vec![vec![5], vec![6]], "front first, the other at a later read");
    assert_eq!(f.current(), &[5, 6, 4], "the app's windows in their order on top");
    assert_eq!(d.steps.last().map(|(_, s)| s.clone()), Some(Step::Focus(5)), "the app's front window focused last");
    assert_eq!(r.focuses, 1, "the order put back by the focus asked: {:?}", d.steps);
    assert!(!d.moved_in_front.contains(&4), "the stage in front never moved");
}

/// The round trip with an app of two windows (live 11-17): Apple's switched to the app's
/// stage; one focus brings the stage that left back whole and in its order, then only the
/// app's windows are pressed, its item's front first, the other at a later read, and the order
/// put back, the app's front on top.
#[test]
fn the_round_trip_brings_the_stage_back_by_one_focus_then_presses_the_apps_windows_one_by_one() {
    let mut f = FakeWindowManager::new();
    f.open_app(20, &[5]);
    f.open_app(10, &[1, 2]);
    let mut a = settle(&mut f, &Arrangement::default());
    let mut job = plan::open_here(&a, 40, f.now()).expect("a stage in front");
    a.step_began(f.now());
    f.launch(40, &[8, 9], false);
    let d = drive_every(&mut f, &mut a, &mut job, 8000, FAST, |_, _| {});
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("round_trip"));
    assert_eq!(d.steps[0].1, Step::Focus(1), "the stage that left, by one switch to its front window");
    assert_eq!(presses(&d.steps), vec![vec![8], vec![9]], "only the app's windows, its item's front first");
    assert_eq!(f.current(), &[8, 9, 1, 2], "the stage in its order, the app's in theirs on top");
    assert_eq!(d.steps.last().map(|(_, s)| s.clone()), Some(Step::Focus(8)));
    assert_eq!(f.back_refused, 0);
}

/// The owner's smoke test of 11-17: Activity Monitor's window, shown again into the strip,
/// came in front by itself (no step of ours ran) before it was pressed, and Apple's switched to
/// it. The round trip goes at the settle by one switch back, no thumbnail of the stage that
/// left waited for, then the app's window in one press, on top.
#[test]
fn an_app_in_front_by_itself_once_shown_again_goes_by_one_switch_back() {
    let mut f = FakeWindowManager::new();
    f.open_app(20, &[3]);
    f.open_app(10, &[1, 2]);
    let mut a = settle(&mut f, &Arrangement::default());
    let mut job = plan::reopen_here(&a, 40, &f.pids(), f.now()).expect("a stage in front, no window of the app");
    job.hide_took(f.now());
    let mut came: Option<u64> = None;
    let d = drive_every(&mut f, &mut a, &mut job, 8000, FAST, |f, t| {
        if t == 25 {
            f.reopen(40, 5, true);
        }
        // Its thumbnail listed with its twin: the app comes in front by itself.
        if came.is_none() && f.listed().windows.iter().any(|w| w.id == 9005) {
            came = Some(t);
            f.focus_window(5);
        }
    });
    let r = d.end.expect("ended").expect("done");
    let came = came.expect("it came in front");
    let kinds: Vec<&str> = d.steps.iter().map(|(_, s)| super::job::step_name(s)).collect();
    assert_eq!(kinds, vec!["unhide", "focus", "shift_click", "focus"], "{:?}", d.steps);
    assert_eq!(d.steps[1].1, Step::Focus(1), "the stage that left, by a switch");
    assert!(d.steps[1].0 - came <= SETTLE.as_millis() as u64 + 150, "switched back {} ms after Apple's switch", d.steps[1].0 - came);
    assert_eq!(presses(&d.steps), vec![vec![5]]);
    assert_eq!(f.current(), &[5, 1, 2], "the stage in its order, the app on top");
    assert!(r.round_trip_why.is_some_and(|w| w.contains("by itself")), "{r:?}");
    assert!(r.ms - came <= 2600, "done {} ms after the app came in front", r.ms - came);
}

/// The owner's smoke test of 11-17: the hide at Activity Monitor's activation was called off
/// as too late (its window had shown, 44 ms), yet the app hid a moment on; shown again at once,
/// Apple's put its window into the strip. That is the hide route: the thumbnail taken in once
/// it rests, the whole arrangement's settle never waited for (live: 955 ms waited for it).
#[test]
fn a_hide_called_off_that_took_late_is_the_hide_route_taken_in_ahead_of_the_settle() {
    let mut f = FakeWindowManager::new();
    f.open_app(20, &[3]);
    f.open_app(10, &[1, 2]);
    let mut a = settle(&mut f, &Arrangement::default());
    let mut job = plan::reopen_here(&a, 40, &f.pids(), f.now()).expect("a stage in front, no window of the app");
    a.step_began(f.now());
    // It hid all the same; the platform shows it again at once.
    f.reopen(40, 5, true);
    job.hid_late(f.now());
    f.show_app(40);
    let mut twin: Option<u64> = None;
    let d = drive_every(&mut f, &mut a, &mut job, 6000, FAST, |f, t| {
        if twin.is_none() && f.listed().windows.iter().any(|w| w.id == 9005) {
            twin = Some(t);
        }
        // A window of the stage in front moves all along: the arrangement never settles.
        f.set_frame(2, Rect::new(330.0 + (t / 25) as f64, 150.0, 700.0, 500.0));
    });
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("hide_activation"));
    let kinds: Vec<&str> = d.steps.iter().map(|(_, s)| super::job::step_name(s)).collect();
    assert_eq!(kinds, vec!["shift_click", "focus"], "shown again by the platform, not by a step: {:?}", d.steps);
    let (twin, click) = (twin.expect("its twin listed"), times(&d.steps, "shift_click")[0]);
    assert!(click >= twin && click - twin <= 100, "pressed {} ms after its twin came", click - twin);
    assert!(!a.settled, "never settled");
    assert_eq!(f.current(), &[5, 1, 2]);
    assert!(r.unhide_ms.is_some());
}

/// Every give-up of "open here" shows the app again as well as activating it: a `hide()`
/// asked before may take after the job knew of it (Finder, live 11-16, left hidden).
#[test]
fn every_give_up_of_open_here_shows_the_app_again_and_activates_it() {
    let (mut f, mut a) = three();
    // Not hidden by the job's account (the hide came too late), and nothing of it ever shows.
    let mut job = plan::reopen_here(&a, 40, &f.pids(), f.now()).expect("planned");
    let r = drive(&mut f, &mut a, &mut job, 6000).end.expect("ended").expect_err("given up");
    assert_eq!((r.unhide, r.activate), (Some(40), Some(40)), "{r:?}");
    // A launch hidden by us that never shows a window: the same.
    let mut job = plan::open_here(&a, 41, f.now()).expect("planned");
    job.hide_took(f.now());
    let r = drive(&mut f, &mut a, &mut job, 20_000).end.expect("ended").expect_err("given up");
    assert_eq!((r.unhide, r.activate), (Some(41), Some(41)), "{r:?}");
}

/// An app with a window in some stage: no job, nothing hidden; Apple's activation switches to
/// that stage, as it should.
#[test]
fn a_running_app_with_a_window_in_a_stage_is_left_to_apples_switch() {
    let (mut f, a) = three();
    assert!(plan::reopen_here(&a, 20, &f.pids(), f.now()).is_none(), "Bravo has window 3 in the strip");
    assert!(plan::reopen_here(&a, 30, &f.pids(), f.now()).is_none(), "Charlie's window is in front");
    assert!(plan::reopen_here(&Arrangement::default(), 40, &f.pids(), f.now()).is_none(), "no stage in front: Apple's own");
    // Apple's own answer to the activation: Bravo's stage in front.
    f.focus_window(3);
    let a = settle(&mut f, &a);
    assert_eq!(a.current, vec![3]);
    assert!(f.posted.is_empty() && f.unhidden.is_empty());
}

/// `[sugar] shift_brings_here`: Shift held on the Dock click of a running app with no window
/// asks the stage in front for its window (`Decision::for_new_window`); the person still
/// holding Shift when the window comes, the Shift-click uses that Shift (none of ours).
#[test]
fn shift_held_at_a_running_apps_activation_brings_its_window_here_with_the_persons_own_shift() {
    use crate::engine::rules::{Decision, RuleAction};
    let (decision, _) = Decision::default().for_new_window(true, false, true);
    assert_eq!(decision.action, RuleAction::Current, "Shift held: the next window here");
    let (mut f, mut a) = three();
    f.input.shift = true;
    let mut job = plan::reopen_here(&a, 40, &f.pids(), f.now()).expect("planned");
    job.hide_took(f.now());
    let d = drive_with(&mut f, &mut a, &mut job, 6000, |f, t| {
        if t == 50 {
            f.reopen(40, 5, true);
        }
    });
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.route, Some("hide_activation"));
    assert!(matches!(f.posted[..], [Posted::Clicks { own_shift: false, .. }]), "the person's Shift: {:?}", f.posted);
    assert_eq!(sorted(f.current()), vec![4, 5]);
}

/// A ⌘Tab (under a rule) to a running app that reopens no window: hidden at its activation,
/// shown again a moment later, and activated once no thumbnail of it shows, as the person
/// asked.
#[test]
fn a_running_app_hidden_at_its_activation_that_reopens_no_window_is_activated_again() {
    let (mut f, mut a) = three();
    let mut job = plan::reopen_here(&a, 40, &f.pids(), f.now()).expect("planned");
    job.hide_took(f.now());
    let d = drive(&mut f, &mut a, &mut job, 6000);
    let r = d.end.expect("ended").expect_err("given up");
    assert_eq!((r.unhide, r.activate), (Some(40), Some(40)), "{r:?}");
    let wait = (super::job::UNHIDE_AFTER + super::job::REOPEN_THUMB).as_millis() as u64;
    assert!(r.ms >= wait && r.ms <= wait + 200, "given up {} ms in", r.ms);
    assert_eq!(f.unhidden, vec![40], "shown again, not activated, before the give-up");
    assert!(f.posted.is_empty());
}

/// The fifth probe of 11-16: under "All at Once" Apple's never splits one app's windows (a
/// drag of one while its sibling stays only moves it). Send and New for such a window are
/// refused up front, nothing posted; a window of another app goes; "One at a Time" tries.
#[test]
fn under_all_at_once_a_window_whose_app_stays_in_its_stage_is_refused_up_front() {
    use crate::engine::settings::AppWindows;
    let mut f = FakeWindowManager::new();
    f.all_at_once = true;
    f.open_app(10, &[1, 2]);
    f.open_app(20, &[3]);
    let mut a = settle(&mut f, &Arrangement::default());
    // Bravo's window 3 merged into Alpha's stage of two.
    let mut job = plan::merge(&a, 1, f.now()).expect("planned");
    drive(&mut f, &mut a, &mut job, 4000).end.expect("ended").expect("merged");
    a = settle(&mut f, &a);
    let pids = f.pids();
    let r = plan::apart(&a, 1, &pids, AppWindows::AllAtOnce).expect_err("Alpha's other window stays");
    assert!(r.0.contains("All at Once"), "{r:?}");
    assert!(plan::apart(&a, 3, &pids, AppWindows::AllAtOnce).is_ok(), "Bravo's window: another app's");
    assert!(plan::apart(&a, 1, &pids, AppWindows::OneAtATime).is_ok(), "One at a Time: tried");
    // What the refusal spares: the fake's Apple only moves such a window.
    let posted = f.posted.len();
    let mut job = plan::new_stage(&a, 1, f.now()).expect("planned without the rule");
    let d = drive(&mut f, &mut a, &mut job, 5000);
    d.end.expect("ended").expect_err("Apple's never takes it out");
    assert!(f.posted.len() > posted);
    // Bravo out of the Alpha and Bravo stage: done.
    let mut job = plan::new_stage(&a, 3, f.now()).expect("planned");
    drive(&mut f, &mut a, &mut job, 5000).end.expect("ended").expect("taken out");
    assert!(f.stages().iter().any(|st| st == &vec![3]), "{:?}", f.stages());
}

#[test]
fn the_plans_refuse_what_has_nothing_to_do() {
    let (f, a) = three();
    assert!(plan::merge(&a, 3, f.now()).is_err(), "no stage 3");
    assert!(plan::send(&a, 2, 1, |_| None, f.now()).is_err(), "window 1 is in stage 2 already");
    assert!(plan::new_stage(&a, 4, f.now()).is_err(), "alone in front already");
    assert!(plan::new_stage(&a, 3, f.now()).is_err(), "alone in its stage already");
    assert!(plan::new_stage(&a, 1, f.now()).is_ok(), "out of Alpha's stage of two");
    assert!(plan::open_here(&Arrangement::default(), 40, f.now()).is_none(), "no stage in front: Apple's puts it in front");
    let empty = Arrangement { current: Vec::new(), ..a.clone() };
    assert!(plan::merge(&empty, 1, f.now()).is_err(), "nothing in front to merge into");
}

#[test]
fn a_switch_is_a_job_of_one_focus() {
    let (mut f, mut a) = three();
    let mut job = plan::switch_job(&a, 2, |_| None, f.now()).expect("planned");
    let d = drive(&mut f, &mut a, &mut job, 4000);
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.focuses, 1);
    assert!(f.posted.is_empty(), "no pointer");
    assert_eq!(sorted(f.current()), vec![1, 2]);
}

// ── Restore (A5) ────────────────────────────────────────────────────────────────────────

use super::job::Goal;
use super::restore::{self, Target, Wanted};
use crate::engine::model::{Display, DisplayId, WindowMeta};
use crate::engine::persist::{SavedRole, SavedSpace, SavedStage, SavedWindow};

/// Wanted: the stages `stages` (each front to back), the one at `front` the saved active one.
fn wanting(stages: &[&[u32]], front: usize) -> Wanted {
    Wanted { stages: stages.iter().map(|w| Target { windows: w.to_vec(), name: None }).collect(), front: Some(front), ..Wanted::default() }
}

/// The fake's arrangement as sets: the stage in front, then the others sorted.
fn arrangement(f: &FakeWindowManager) -> (Vec<u32>, Vec<Vec<u32>>) {
    let mut rest: Vec<Vec<u32>> = f.stages().iter().map(|s| sorted(s)).collect();
    rest.sort();
    (sorted(f.current()), rest)
}

/// Every drag posted, by window.
fn dragged(f: &FakeWindowManager) -> Vec<u32> {
    f.posted.iter().filter_map(|p| if let Posted::Drag { window, .. } = p { Some(*window) } else { None }).collect()
}

/// The saved stages through the restore's own matcher (app and title), as `state-apple.json`
/// has them: Alpha's two windows with Bravo's, and Charlie's alone, the active one.
fn saved_alpha_bravo_and_charlie(f: &FakeWindowManager, a: &Arrangement) -> Wanted {
    let saved = |bundle: &str, title: &str, z: usize| SavedWindow { bundle_id: bundle.into(), title: title.into(), frame: Rect::new(300.0, 120.0, 700.0, 500.0), z, key: z == 0, role: SavedRole::Standard, ..SavedWindow::default() };
    let space = SavedSpace {
        id: 1,
        current: true,
        stages: vec![
            SavedStage { windows: vec![saved("a", "Alpha 1", 0), saved("a", "Alpha 2", 1), saved("b", "Bravo 1", 2)], ..SavedStage::default() },
            SavedStage { windows: vec![saved("c", "Charlie 1", 0)], ..SavedStage::default() },
        ],
        active_index: Some(1),
        ..SavedSpace::default()
    };
    let titled = |id: u32, pid: i32, bundle: &str, title: &str| WindowMeta { title: title.into(), frame: f.frame(id).unwrap_or_default(), ..crate::engine::testkit::meta(id, pid, bundle) };
    let live = [titled(1, 10, "a", "Alpha 1"), titled(2, 10, "a", "Alpha 2"), titled(3, 20, "b", "Bravo 1"), titled(4, 30, "c", "Charlie 1")];
    let mut w = restore::wanted(&space, &live, |m| !a.current.contains(&m.id.0));
    restore::keep_apps_together(&mut w, &f.pids(), true);
    w
}

/// The live check's scramble, by Apple's own answers: Alpha in front, Charlie merged into it
/// (a Shift-click), Bravo alone in the strip.
fn scrambled(all_at_once: bool) -> (FakeWindowManager, Arrangement) {
    let (mut f, a) = three();
    f.all_at_once = all_at_once;
    f.focus_window(1);
    let mut a = settle(&mut f, &a);
    let n = a.strip.iter().position(|g| g.windows == [4]).expect("Charlie's stage") + 1;
    let mut job = plan::merge(&a, n, f.now()).expect("planned");
    drive(&mut f, &mut a, &mut job, 6000).end.expect("ended").expect("merged");
    let a = settle(&mut f, &a);
    assert_eq!(arrangement(&f), (vec![1, 2, 4], vec![vec![3]]), "scrambled");
    f.posted.clear();
    f.focused.clear();
    (f, a)
}

/// A5: two saved stages (Alpha's two windows with Bravo's; Charlie's alone, the active one)
/// put back under All at Once: Charlie, another app's window, dragged out of Alpha's stage,
/// Bravo Shift-clicked in, Charlie's stage brought in front last; Alpha's sibling never dragged.
#[test]
fn a_restore_puts_two_saved_stages_back_under_all_at_once() {
    let (mut f, mut a) = scrambled(true);
    let w = saved_alpha_bravo_and_charlie(&f, &a);
    assert_eq!((w.saved, w.matched), (4, 4));
    assert!(w.could_not.is_empty(), "{:?}", w.could_not);
    let mut job = restore::plan(&w, true, f.now()).expect("planned");
    let d = drive(&mut f, &mut a, &mut job, 15_000);
    let r = d.end.expect("ended").expect("done");
    assert_eq!(arrangement(&f), (vec![4], vec![vec![1, 2, 3]]), "as saved, the active one in front: {:?}", d.steps);
    assert_eq!(r.stages, 2);
    assert_eq!(dragged(&f), vec![4], "Charlie out of Alpha's stage, never one of Alpha's");
    assert_eq!(clicks(&d.steps), vec![3], "Bravo in by one Shift-click");
    assert!(r.could_not.is_empty() && r.gone.is_empty(), "{r:?}");
    assert_eq!(f.ignored_mid_motion, 0);
    // A restore of what Apple's shows already moves nothing.
    let a2 = settle(&mut f, &a);
    assert!(restore::in_place(&a2, &w));
    let mut again = restore::plan(&w, true, f.now()).expect("planned");
    let posted = f.posted.len();
    let d = drive(&mut f, &mut a, &mut again, 4000);
    d.end.expect("ended").expect("done");
    assert!(d.steps.is_empty() && f.posted.len() == posted, "nothing to do: {:?}", d.steps);
}

/// A saved arrangement that split one app's windows (saved under One at a Time) is asked with
/// the app's windows together under All at Once: no drag of a window whose sibling stays, and
/// the report says why.
#[test]
fn a_restore_never_asks_for_a_split_all_at_once_refuses() {
    let (mut f, mut a) = scrambled(true);
    // Alpha 1 saved with Bravo, Alpha 2 with Charlie (the active one).
    let mut w = wanting(&[&[1, 3], &[2, 4]], 1);
    restore::keep_apps_together(&mut w, &f.pids(), true);
    assert_eq!(w.stages.iter().map(|t| sorted(&t.windows)).collect::<Vec<_>>(), vec![vec![3], vec![1, 2, 4]]);
    let mut job = restore::plan(&w, true, f.now()).expect("planned");
    let d = drive(&mut f, &mut a, &mut job, 15_000);
    let r = d.end.expect("ended").expect("done");
    assert!(!dragged(&f).iter().any(|w| *w == 1 || *w == 2), "no window of Alpha's dragged: {:?}", f.posted);
    assert_eq!(arrangement(&f), (vec![1, 2, 4], vec![vec![3]]));
    assert_eq!(r.could_not.len(), 1, "{:?}", r.could_not);
}

#[test]
fn a_restore_skips_a_window_gone_meanwhile() {
    let (mut f, mut a) = scrambled(true);
    let w = wanting(&[&[1, 2, 3], &[4]], 1);
    let mut job = restore::plan(&w, true, f.now()).expect("planned");
    f.close(3);
    let d = drive(&mut f, &mut a, &mut job, 15_000);
    let r = d.end.expect("ended").expect("done");
    assert_eq!(r.gone, vec![3]);
    assert_eq!(arrangement(&f), (vec![4], vec![vec![1, 2]]));
}

#[test]
fn a_restore_waits_for_a_covered_strip_to_show() {
    let (mut f, a) = three();
    f.cover(true);
    let mut a = settle(&mut f, &a);
    // Bravo saved with Charlie (the active one), Alpha's two alone: a Shift-click is needed.
    let w = wanting(&[&[1, 2], &[4, 3]], 1);
    let mut job = restore::plan(&w, true, f.now()).expect("planned");
    let d = drive_with(&mut f, &mut a, &mut job, 12_000, |f, t| {
        if t == 2500 {
            f.cover(false);
        }
    });
    let r = d.end.expect("ended").expect("done once the strip showed");
    assert!(times(&d.steps, "shift_click").iter().all(|t| *t > 2500), "no click while covered: {:?}", d.steps);
    assert_eq!(arrangement(&f), (vec![3, 4], vec![vec![1, 2]]));
    assert_eq!(r.stages, 2);
}

// ── Screens and Desktops (A6) ───────────────────────────────────────────────────────────

/// A6: two screens, a fake WindowManager each (the second right of the main one, its windows
/// numbered from 101): the list joined, split by screen, each folded into its own mirror; a
/// switch on the second screen focuses its window and moves nothing on the main one.
#[test]
fn two_screens_each_have_a_mirror_of_their_own_and_a_step_acts_on_one() {
    use super::screens;
    let main = display();
    let right = Display { id: DisplayId(2), frame: Rect::new(main.frame.max_x(), 0.0, 1440.0, 900.0), visible: Rect::new(main.frame.max_x(), 25.0, 1440.0, 800.0), ..main };
    let displays = [main, right];
    let mut one = FakeWindowManager::new();
    one.open_app(10, &[1, 2]);
    one.open_app(30, &[4]);
    let mut two = FakeWindowManager::new().on_screen_at(right.frame.x, 9950);
    two.open_app(50, &[101]);
    two.open_app(60, &[102]);
    let joined = |one: &FakeWindowManager, two: &FakeWindowManager| super::mirror::Listed { windows: one.listed().windows.into_iter().chain(two.listed().windows).collect(), manager: one.listed().manager };
    let managed: BTreeSet<u32> = one.window_ids().into_iter().chain(two.window_ids()).collect();
    let (mut m1, mut m2) = (Arrangement::default(), Arrangement::default());
    let mut on = std::collections::BTreeMap::new();
    let mut read_both = |one: &mut FakeWindowManager, two: &mut FakeWindowManager, m1: &mut Arrangement, m2: &mut Arrangement| {
        one.advance(50);
        two.advance(50);
        let listed = joined(one, two);
        let split = screens::split(&listed, &managed, &displays, &on);
        on = split.iter().flat_map(|(d, ws)| ws.iter().map(move |w| (*w, *d))).collect();
        *m1 = fold(m1, None, &listed, &split[&main.id], &main, one.now());
        *m2 = fold(m2, None, &listed, &split[&right.id], &right, two.now());
    };
    for _ in 0..40 {
        read_both(&mut one, &mut two, &mut m1, &mut m2);
    }
    assert!(m1.settled && m2.settled);
    assert_eq!((m1.current.clone(), m1.strip.iter().map(|g| sorted(&g.windows)).collect::<Vec<_>>()), (vec![4], vec![vec![1, 2]]), "the main screen's own");
    assert_eq!((m2.current.clone(), m2.strip.iter().map(|g| g.windows.clone()).collect::<Vec<_>>()), (vec![102], vec![vec![101]]), "the second screen's own");
    assert!(m2.click_at.get(&101).is_some_and(|p| p.x > right.frame.x), "its thumbnail on its own screen");
    // A switch on the second screen: its job runs against its mirror and its WindowManager.
    let mut job = plan::switch_job(&m2, 1, |_| None, two.now()).expect("planned");
    let front1 = m1.current.clone();
    for _ in 0..60 {
        read_both(&mut one, &mut two, &mut m1, &mut m2);
        let pids = two.pids();
        let next = job.advance(&Ctx { mirror: &m2, input: two.input, screen: &right, pids: &pids, in_flight: false }, two.now());
        match next {
            Next::Run(Step::Focus(w)) => {
                job.ran(&Step::Focus(w), two.focus(w).is_ok());
                m2.step_began(two.now());
            }
            Next::Done(_) => break,
            Next::Run(other) => panic!("only a focus: {other:?}"),
            Next::GiveUp(r) => panic!("{r:?}"),
            Next::Wait(_) => {}
        }
    }
    assert_eq!(two.current(), &[101]);
    assert!(one.focused.is_empty() && one.posted.is_empty(), "nothing on the main screen");
    assert_eq!(m1.current, front1);
}

/// A6: a Desktop not shown is left alone: its stages go away at the change, no number counts
/// them, a send or a restore of their windows does nothing there (a focus would show that
/// Desktop), and they come back once their Desktop shows again.
#[test]
fn a_desktop_not_shown_is_remembered_and_left_alone() {
    let (mut f, a) = three();
    f.new_desktop();
    let mut a = a;
    a.desktop_left(f.now());
    f.open_app(40, &[5]);
    let mut a = settle(&mut f, &a);
    assert_eq!(a.current, vec![5]);
    let mut away: Vec<Vec<u32>> = a.away.iter().map(|(_, g)| sorted(&g.windows)).collect();
    away.sort();
    assert_eq!(away, vec![vec![1, 2], vec![3], vec![4]], "the Desktop left's stages, away");
    assert!(a.strip.is_empty() && a.remembered.is_empty());
    assert!(plan::switch_job(&a, 1, |_| None, f.now()).is_err(), "no number counts them");
    assert!(plan::send(&a, 1, 3, |_| None, f.now()).is_err() && plan::new_stage(&a, 1, f.now()).is_err(), "refused: another Desktop");
    // A restore wanting a window of each Desktop: the one shown done, the other left there.
    let mut job = restore::plan(&wanting(&[&[1, 2], &[5]], 1), true, f.now()).expect("planned");
    let d = drive(&mut f, &mut a, &mut job, 6000);
    let r = d.end.expect("ended").expect("done");
    assert!(r.could_not.iter().any(|w| w.contains("Desktop not shown")), "{:?}", r.could_not);
    assert_eq!(f.desktop_jumps, 0, "no focus of a window on another Desktop");
    assert!(f.posted.is_empty());
    // A gather of a window there is left too.
    let mut gather = Job::new(plan::Intent::Merge(1), vec![Goal::Gather { windows: vec![5, 3], app: None, focus_app: false }], f.now());
    drive(&mut f, &mut a, &mut gather, 3000).end.expect("ended").expect("nothing to gather here");
    assert_eq!(f.desktop_jumps, 0);
    // That Desktop shown again: its stages back as they were, the other one's away.
    f.show_desktop(0);
    a.desktop_left(f.now());
    let a = settle(&mut f, &a);
    assert_eq!(a.current, vec![4]);
    assert_eq!(a.strip.iter().map(|g| sorted(&g.windows)).collect::<Vec<_>>(), vec![vec![3], vec![1, 2]]);
    assert_eq!(a.away.iter().map(|(_, g)| g.windows.clone()).collect::<Vec<_>>(), vec![vec![5]]);
}

/// A full-screen app's Space on the screen: no step runs; given up after a few seconds.
#[test]
fn no_step_runs_while_a_full_screen_app_shows() {
    let (mut f, mut a) = three();
    a.full_screen = true;
    let mut job = plan::merge(&a, 2, f.now()).expect("planned");
    let mut d = Drive::default();
    for _ in 0..200 {
        f.advance(50);
        a = read(&f, &a);
        let pids = f.pids();
        match job.advance(&Ctx { mirror: &a, input: f.input, screen: &display(), pids: &pids, in_flight: false }, f.now()) {
            Next::Run(step) => d.steps.push((0, step)),
            Next::GiveUp(r) => {
                d.end = Some(Err(r));
                break;
            }
            Next::Done(r) => panic!("{r:?}"),
            Next::Wait(_) => {}
        }
    }
    assert!(d.steps.is_empty());
    let r = d.end.expect("ended").expect_err("given up");
    assert!(r.given_up.as_deref().is_some_and(|w| w.contains("full-screen")), "{r:?}");
}
