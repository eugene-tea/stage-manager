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
use crate::engine::apple::desk::Posted;
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
fn drive_with(f: &mut FakeWindowManager, a: &mut Arrangement, job: &mut Job, max_ms: u64, mut each: impl FnMut(&mut FakeWindowManager, u64)) -> Drive {
    let mut d = Drive::default();
    let start = f.now();
    let front: Vec<u32> = a.current.clone();
    let frames0 = a.frames.clone();
    for _ in 0..max_ms / 50 {
        f.advance(50);
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
                    Step::AwaitSettle | Step::AwaitStrip(_) => true,
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

#[test]
fn the_mirror_finds_a_part_of_its_own_on_each_thumbnail_of_a_stage_of_two() {
    let (f, a) = three();
    let listed = f.listed();
    let bounds = |id: u32| listed.windows.iter().find(|w| w.id == id).map(|w| w.bounds).expect("listed");
    let (front, back) = (bounds(1), bounds(2));
    let p1 = a.click_at[&1];
    let p2 = a.click_at[&2];
    assert!(front.contains(p1), "{p1:?}");
    assert!(back.contains(p2) && !front.contains(p2), "the back one where it shows: {p2:?}");
    assert!(!a.click_at.contains_key(&4), "the window in front has no thumbnail");
}

/// The fourth probe of 11-16: a stage of two settled in the strip, both thumbnails
/// Shift-clicked within one Shift press, both joined; no settle waited between them (live
/// 11-16 before: a stage of three took 3.9 s, a settle after each click).
#[test]
fn a_stage_of_two_is_brought_here_in_one_shift_press_front_to_back() {
    let (mut f, mut a) = three();
    let mut job = plan::merge(&a, 2, f.now()).expect("stage 2 is Alpha's");
    let d = drive(&mut f, &mut a, &mut job, 6000);
    let report = d.end.expect("ended").expect("done");
    assert_eq!(presses(&d.steps), vec![vec![1, 2]], "one press, a Shift-click per window, the front one first");
    assert_eq!((report.clicks, report.presses), (2, 1));
    assert_eq!(sorted(f.current()), vec![1, 2, 4]);
    assert_eq!(f.ignored_mid_motion, 0, "no click mid-motion");
    assert!(matches!(f.posted[..], [Posted::Clicks { own_shift: true, .. }]), "a Shift of ours around the press: {:?}", f.posted);
}

/// Live 11-16 (merge 1, a stage of three): one press for the three thumbnails of one item,
/// each at a point of its own with the ones in front of it counted; and "done" at the first
/// read that shows them in front, not a settle after it.
#[test]
fn a_stage_of_three_comes_in_one_press_and_the_merge_is_done_once_the_list_shows_it_joined() {
    let mut f = FakeWindowManager::new();
    f.open_app(10, &[1, 2, 3]);
    f.open_app(20, &[4]);
    let mut a = settle(&mut f, &Arrangement::default());
    let points: Vec<_> = [1, 2, 3].iter().map(|w| a.click_at.get(w).copied()).collect();
    assert!(points.iter().all(Option::is_some), "each of the three shows a part of its own: {points:?}");
    let mut job = plan::merge(&a, 1, f.now()).expect("stage 1 is the app's of three");
    let d = drive(&mut f, &mut a, &mut job, 6000);
    let r = d.end.expect("ended").expect("done");
    assert_eq!(presses(&d.steps), vec![vec![1, 2, 3]]);
    assert_eq!(sorted(f.current()), vec![1, 2, 3, 4]);
    let pressed = d.steps[0].0;
    assert!(r.ms <= pressed + SETTLE.as_millis() as u64 + 100, "done {} ms after the press: Apple's motion and a read, no settle after it", r.ms - pressed);
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
    assert!(from.y > frame.y && from.y < frame.y + 20.0 && to.x < 40.0, "the title bar onto the strip");
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
                    Step::AwaitSettle | Step::AwaitStrip(_) => true,
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
    assert_eq!(clicks(&d.steps), vec![4], "the stage pushed into the strip Shift-clicked back");
    assert_eq!(d.steps.last().map(|(_, s)| s.clone()), Some(Step::Focus(5)));
    assert_eq!(sorted(f.current()), vec![4, 5]);
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
    assert_eq!(sorted(&clicks(&d.steps)), vec![5, 6]);
    assert_eq!(sorted(f.current()), vec![4, 5, 6, 7], "a window opened meanwhile joins too");
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
