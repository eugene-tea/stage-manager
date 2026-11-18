//! Scenarios of the paced WM tick (round sm-tick, [`crate::engine::tick_pace`]): each duty
//! that catches a case no event tells of, on the fake desk, with the tick paced as the WM
//! thread paces it, against the same scenario with the tick brisk throughout (the pace
//! before 1.9). Each case must still be caught, at most a rest later at rest; a quiet minute
//! must take few ticks; an event must never wait longer than a brisk period for its tick.
//!
//! The duties' decisions are the real ones ([`Vanished`], [`GoneCheck`], [`Sightings`],
//! [`park_reconcile::step`]); the clock simulates the timer ([`tick_pace::retime`], each rest
//! tick fired its whole tolerance late); the window server's list is the desk's.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::{Duration, Instant};

use super::spaces::TIDY_EVERY;
use super::upkeep::SLIVER_REST;
use super::SPACE_POLL;
use crate::engine::corner::park_reconcile::{self, Facts, Stray, Verdict};
use crate::engine::gone::{GoneCheck, gone};
use crate::engine::model::{Action, Model, WindowId};
use crate::engine::recovery::EngineKind;
use crate::engine::rules::{Decision, RuleAction};
use crate::engine::stale_handle::Sightings;
use crate::engine::testkit::{Desk, both_engines, corner_nuance, engine, meta, model, w};
use crate::engine::tick_pace::{self, BRISK, FULL_EVERY, REST, TickPace};
use crate::engine::vanished::Vanished;
use crate::engine::geometry::Rect;

/// Seconds from the scenario's start.
type Secs = f64;

fn secs(d: Duration) -> Secs {
    d.as_secs_f64()
}

/// The tick's timer and pace, simulated.
struct Clock {
    t0: Instant,
    pace: TickPace,
    /// When the timer fires next.
    next: Secs,
    /// Every tick so far.
    ticks: Vec<Secs>,
    /// The pace before 1.9: brisk throughout.
    old: bool,
}

impl Clock {
    /// A clock untouched since long before the scenario, so at rest from its start (`old`:
    /// brisk throughout instead).
    fn new(old: bool) -> Self {
        Self { t0: Instant::now(), pace: TickPace::default(), next: secs(BRISK), ticks: Vec::new(), old }
    }

    fn at(&self, s: Secs) -> Instant {
        self.t0 + Duration::from_secs_f64(s)
    }

    /// Runs every tick due up to `until`: `tick(world, now, full)` runs the duties and says
    /// what waits and how long the rest may be.
    fn run<W>(&mut self, w: &mut W, until: Secs, tick: &mut impl FnMut(&mut W, Instant, bool) -> (Option<&'static str>, Duration)) {
        while self.next <= until {
            let t = self.next;
            self.ticks.push(t);
            let now = self.at(t);
            let full = self.pace.full_pass(now);
            let (waits, rest) = tick(w, now, full);
            let waits = if self.old { Some("before 1.9") } else { waits };
            let pace = self.pace.pace(now, waits, rest);
            // The repeating timer's own next firing, then the pace's.
            let repeat = t + secs(BRISK);
            self.next = tick_pace::retime(pace, t, repeat, true).unwrap_or(repeat);
            if pace.at_rest() {
                // The worst the system may do with the rest's tolerance.
                self.next += secs(pace.tolerance());
            }
        }
    }

    /// An event at `at` (after the last tick run) that stirs the tick.
    fn stir(&mut self, at: Secs, rest: Duration) {
        let now = self.at(at);
        self.pace.stir(now);
        let pace = self.pace.pace(now, None, rest);
        if let Some(next) = tick_pace::retime(pace, at, self.next, false) {
            self.next = next;
        }
    }

    /// Ticks in `[from, to)`.
    fn ticks_in(&self, from: Secs, to: Secs) -> usize {
        self.ticks.iter().filter(|t| **t >= from && **t < to).count()
    }
}

/// The longest the tick rests on `engine` with windows hidden: the inert slivers' cap under
/// the Corner engine (on by default, and its tap judges presses while windows are parked).
fn rest_on(engine: EngineKind) -> Duration {
    match engine {
        EngineKind::Corner => SLIVER_REST,
        EngineKind::Spaces | EngineKind::Apple => REST,
    }
}

/// The most a case caught at rest may come after it was caught before 1.9: a rest, its
/// tolerance, and the brisk ticks and full-pass alignment it then needs (the old tick's
/// grid fell anywhere in its period).
fn late_by_at_most(rest: Duration) -> Secs {
    secs(rest) + secs(tick_pace::Pace::Rest(rest).tolerance()) + secs(FULL_EVERY) + secs(BRISK)
}

/// A model and the fake desk: app 10's two windows in a stage hidden behind app 20's stage
/// (shown), and app 30's window in a stage of its own, hidden.
struct World {
    m: Model,
    d: Desk,
    /// Apps whose process died, no notice sent.
    died: BTreeSet<i32>,
    /// Windows their app ordered out, their handles dead, no notice sent.
    dead: BTreeSet<u32>,
}

impl World {
    fn new() -> Self {
        let mut world = Self { m: model(), d: Desk::default(), died: BTreeSet::new(), dead: BTreeSet::new() };
        for (id, pid, bundle) in [(1, 10, "com.example.mail"), (2, 10, "com.example.mail"), (5, 30, "com.example.notes"), (3, 20, "com.example.text")] {
            let mut meta = meta(id, pid, bundle);
            meta.frame = Rect::new(200.0 + 20.0 * id as f64, 120.0, 700.0, 500.0);
            world.d.open(&meta);
            let obs = world.d.obs();
            let actions = world.m.add_window(meta, &Decision { action: RuleAction::Native, ..Decision::default() }, &obs);
            world.d.apply(&world.m, &actions);
        }
        world
    }

    fn apply(&mut self, actions: Vec<Action>) {
        self.d.apply(&self.m, &actions);
    }

    /// The window server's complete list: every window the desk has, shown or hidden.
    fn listed(&self) -> HashSet<u32> {
        self.d.frames.keys().map(|k| k.0).collect()
    }

    fn parked(&self, id: u32) -> bool {
        self.m.window(w(id)).is_some_and(|x| x.parked)
    }
}

/// When a scenario's case was caught, in seconds from its start.
fn caught_after(at: Option<Instant>, clock: &Clock) -> Secs {
    at.map(|t| t.duration_since(clock.t0).as_secs_f64()).expect("the case was caught")
}

/// A window closed while its app did not answer (no destroyed notice): the full pass's sweep
/// sees it missing from the complete list twice ([`Vanished`]) and drops it, its card with it.
fn closed_without_notice(old: bool) -> (Secs, Clock) {
    let mut world = World::new();
    assert!(world.parked(1) && !world.parked(3), "app 10's stage hidden, app 20's shown");
    let mut clock = Clock::new(old);
    let mut vanished = Vanished::default();
    let mut tracked: BTreeSet<u32> = world.m.windows().map(|x| x.meta.id.0).collect();
    let mut caught = None;
    let rest = rest_on(world.m.options().engine);
    let mut tick = |world: &mut World, now: Instant, full: bool| {
        if full {
            for id in vanished.look(now, tracked.iter().copied(), &world.listed(), false) {
                tracked.remove(&id);
                let actions = world.m.remove_window(WindowId(id));
                world.apply(actions);
                caught.get_or_insert(now);
            }
        }
        (vanished.missing().first().map(|_| "a window missing from the list"), rest)
    };
    clock.run(&mut world, 30.0, &mut tick);
    // App 10 closes its hidden window 2 and says nothing.
    world.d.close(w(2));
    clock.run(&mut world, 60.0, &mut tick);
    assert!(world.m.window(w(2)).is_none(), "dropped, its stage with one window left");
    assert!(world.m.window(w(1)).is_some());
    (caught_after(caught, &clock) - 30.0, clock)
}

/// An app gone without its quit notice: its pid is tested on the full pass ([`GoneCheck`])
/// and its windows go as at its quit.
fn app_gone_without_notice(old: bool) -> (Secs, Clock) {
    let mut world = World::new();
    let mut clock = Clock::new(old);
    let mut check = GoneCheck::default();
    let mut known: BTreeSet<i32> = [10, 20, 30].into();
    let mut caught = None;
    let rest = rest_on(world.m.options().engine);
    let mut tick = |world: &mut World, now: Instant, full: bool| {
        let pids: Vec<i32> = world.m.windows().map(|x| x.meta.pid).collect();
        let candidates = check.candidates(pids, full, |p| known.contains(&p));
        for pid in gone(candidates, |p| world.died.contains(&p)) {
            known.remove(&pid);
            let ids: Vec<WindowId> = world.m.windows().filter(|x| x.meta.pid == pid).map(|x| x.meta.id).collect();
            for id in ids {
                world.d.close(id);
                let actions = world.m.remove_window(id);
                world.apply(actions);
            }
            caught.get_or_insert(now);
        }
        ((!check.is_empty()).then_some("an app whose call failed"), rest)
    };
    clock.run(&mut world, 30.0, &mut tick);
    // App 30 (window 5, its own hidden stage) dies with no notice.
    world.died.insert(30);
    clock.run(&mut world, 60.0, &mut tick);
    assert!(world.m.window(w(5)).is_none(), "its window and its card are gone");
    (caught_after(caught, &clock) - 30.0, clock)
}

/// A shown window whose handle died and which its app ordered out (Music's closed main
/// window, kept to reuse): the Space check (about once a second) judges it closed on two looks
/// [`crate::engine::stale_handle::LOOK_APART`] apart ([`Sightings`]) and lets it go.
fn stale_handle_without_notice(old: bool) -> (Secs, Secs, Clock) {
    let mut world = World::new();
    let mut clock = Clock::new(old);
    let mut sightings = Sightings::default();
    let mut space_checked: Option<Instant> = None;
    let mut first_look = None;
    let mut caught = None;
    let rest = rest_on(world.m.options().engine);
    let mut tick = |world: &mut World, now: Instant, _full: bool| {
        if space_checked.is_none_or(|t| now.duration_since(t) >= SPACE_POLL) {
            space_checked = Some(now);
            // Managed windows of the Desktop shown, off screen, with a dead handle.
            let candidates: Vec<u32> =
                world.m.windows().filter(|x| !x.parked && world.dead.contains(&x.meta.id.0)).map(|x| x.meta.id.0).collect();
            for id in candidates {
                if sightings.closed(id, now) {
                    let actions = world.m.remove_window(WindowId(id));
                    world.apply(actions);
                    caught.get_or_insert(now);
                } else {
                    first_look.get_or_insert(now);
                }
            }
        }
        ((!sightings.waiting().is_empty()).then_some("windows counted missing"), rest)
    };
    clock.run(&mut world, 30.0, &mut tick);
    // App 20 orders its shown window 3 out; its handle is dead from now, no notice comes.
    world.dead.insert(3);
    clock.run(&mut world, 60.0, &mut tick);
    assert!(world.m.window(w(3)).is_none(), "let go");
    (caught_after(caught, &clock) - 30.0, caught_after(first_look, &clock), clock)
}

both_engines! {
    /// The closed window's sweep catches it at rest at most a rest later than it did before.
    #[test]
    fn a_window_closed_with_no_notice_is_still_dropped() {
        let (new, _) = closed_without_notice(false);
        let (old, _) = closed_without_notice(true);
        let rest = rest_on(engine().unwrap_or_default());
        assert!(old <= 2.0, "before 1.9: two full passes, about a second apart ({old:.2} s)");
        assert!(new <= old + late_by_at_most(rest), "now {new:.2} s, before {old:.2} s");
    }

    /// The gone app's pid test catches it at rest within a rest (every rest tick is a full
    /// pass).
    #[test]
    fn an_app_gone_with_no_notice_is_still_handled_as_its_quit() {
        let (new, _) = app_gone_without_notice(false);
        let (old, _) = app_gone_without_notice(true);
        let rest = rest_on(engine().unwrap_or_default());
        assert!(old <= 1.0, "before 1.9: the next full pass ({old:.2} s)");
        assert!(new <= secs(rest) + secs(tick_pace::Pace::Rest(rest).tolerance()), "now {new:.2} s, before {old:.2} s");
    }

    /// The dead handle's two looks: the first at the next rest tick, the second at the old
    /// pace (the look waiting keeps the tick brisk).
    #[test]
    fn a_window_whose_handle_died_is_still_let_go() {
        let (new, first_look, clock) = stale_handle_without_notice(false);
        let (old, _, _) = stale_handle_without_notice(true);
        let rest = rest_on(engine().unwrap_or_default());
        assert!(old <= 2.0, "before 1.9 ({old:.2} s)");
        assert!(new <= old + late_by_at_most(rest), "now {new:.2} s, before {old:.2} s");
        // Between the two looks the tick ran brisk.
        assert!(clock.ticks_in(first_look - 1e-6, first_look + 1.0) >= 4, "brisk while the second look waits");
    }

    /// A quiet minute (no event, nothing waiting): a tick every rest, not every quarter second.
    #[test]
    fn a_quiet_minute_takes_few_ticks() {
        let rest = rest_on(engine().unwrap_or_default());
        let mut clock = Clock::new(false);
        clock.run(&mut (), 60.0, &mut |_: &mut (), _, _| (None, rest));
        let ticks = clock.ticks_in(0.0, 60.0);
        let most = (60.0 / secs(rest)).ceil() as usize;
        assert!(ticks <= most, "{ticks} ticks in a quiet minute (at most {most})");
        assert!(ticks + 1 >= (60.0 / (secs(rest) + secs(tick_pace::Pace::Rest(rest).tolerance()))) as usize);
        let mut old = Clock::new(true);
        old.run(&mut (), 60.0, &mut |_: &mut (), _, _| (None, rest));
        assert!((239..=240).contains(&old.ticks_in(0.0, 60.0)), "before 1.9: every quarter second");
    }

    /// After any event, the next tick comes within a brisk period (as on the old fixed grid),
    /// and the tick stays brisk for a while after it.
    #[test]
    fn an_event_never_waits_longer_than_a_brisk_period_for_its_tick() {
        let rest = rest_on(engine().unwrap_or_default());
        let mut clock = Clock::new(false);
        let mut quiet = |_: &mut (), _, _| (None, rest);
        // Events at awkward moments: just after a rest tick, between brisk ticks, long after.
        let mut at = 0.0;
        for gap in [7.31, 0.01, 0.6, 2.95, 3.4, 11.0, 0.13, 5.5] {
            at += gap;
            clock.run(&mut (), at, &mut quiet);
            clock.stir(at, rest);
            clock.run(&mut (), at + 4.0, &mut quiet);
            let next = clock.ticks.iter().copied().find(|t| *t >= at).expect("a tick after the event");
            assert!(next - at <= secs(BRISK) + 1e-9, "event at {at:.2} s waited {:.3} s", next - at);
            assert!(clock.ticks_in(at, at + secs(tick_pace::BRISK_FOR)) >= 11, "brisk for a while after the event at {at:.2} s");
            at += 4.0;
        }
    }
}

corner_nuance! {
    /// A parked window its app moved out of the corner with no notice (the moved notice
    /// missed): the park reconcile sees it at the next tick (a rest is at most the inert
    /// slivers' cap) and parks it again two looks later, at the old pace.
    #[test]
    fn a_parked_window_moved_out_of_the_corner_with_no_notice_is_parked_again() {
        let run = |old: bool| {
            let mut world = World::new();
            let mut clock = Clock::new(old);
            let mut strays: BTreeMap<u32, Stray> = BTreeMap::new();
            let mut caught = None;
            let mut tick = |world: &mut World, now: Instant, _full: bool| {
                let parked: Vec<u32> = world.m.windows().filter(|x| x.parked).map(|x| x.meta.id.0).collect();
                strays.retain(|id, _| parked.contains(id));
                for id in parked {
                    let Some(frame) = world.d.frames.get(&w(id)).copied() else { continue };
                    if crate::engine::testkit::parked_at(frame) {
                        strays.remove(&id);
                        continue;
                    }
                    let s = strays.entry(id).or_default();
                    if park_reconcile::step(s, Facts::out_at(frame), now) == Verdict::Park {
                        world.apply(vec![Action::Hide { id: w(id) }]);
                        caught.get_or_insert(now);
                    }
                }
                ((!strays.is_empty()).then_some("parked windows out of the corner"), SLIVER_REST)
            };
            clock.run(&mut world, 30.0, &mut tick);
            // App 30 moves its parked window 5 back on screen, no moved notice.
            world.d.frames.insert(w(5), Rect::new(300.0, 200.0, 700.0, 500.0));
            clock.run(&mut world, 60.0, &mut tick);
            assert!(world.d.parked(w(5)), "parked again");
            caught_after(caught, &clock) - 30.0
        };
        let (mut new, mut old) = (0.0, 0.0);
        crate::engine::testkit::on(EngineKind::Corner, || (new, old) = (run(false), run(true)));
        assert!(old <= 0.5 + 1e-9, "before 1.9: two ticks ({old:.2} s)");
        assert!(new <= old + secs(SLIVER_REST) + secs(tick_pace::Pace::Rest(SLIVER_REST).tolerance()), "now {new:.2} s, before {old:.2} s");
    }
}

/// The Spaces engine: a window in a stage's Space that nothing holds (its app ordered it out
/// while hidden, the engine forgot it, the app ordered it in again) is let out at the tidy,
/// every [`TIDY_EVERY`]: at rest at most a rest later.
#[test]
fn a_stranger_in_a_stage_space_is_still_let_out() {
    let run = |old: bool| {
        let mut world = World::new();
        let mut clock = Clock::new(old);
        let mut tidied: Option<Instant> = None;
        let mut caught = None;
        let mut tick = |world: &mut World, now: Instant, _full: bool| {
            if tidied.is_none_or(|t| now.duration_since(t) >= TIDY_EVERY) {
                tidied = Some(now);
                let strangers: Vec<WindowId> =
                    world.d.stage_space.iter().copied().filter(|id| world.m.window(*id).is_none_or(|x| !x.parked)).collect();
                for id in strangers {
                    world.d.stage_space.remove(&id);
                    caught.get_or_insert(now);
                }
            }
            (None, REST)
        };
        clock.run(&mut world, 30.0, &mut tick);
        // Window 9 of app 10, unknown to the model, turns up in the stage Space.
        world.d.frames.insert(w(9), Rect::new(250.0, 150.0, 600.0, 400.0));
        world.d.stage_space.insert(w(9));
        clock.run(&mut world, 60.0, &mut tick);
        assert!(!world.d.stage_space.contains(&w(9)), "let out");
        caught_after(caught, &clock) - 30.0
    };
    let (mut new, mut old) = (0.0, 0.0);
    crate::engine::testkit::on(EngineKind::Spaces, || (new, old) = (run(false), run(true)));
    assert!(old <= secs(TIDY_EVERY) + secs(BRISK), "before 1.9 ({old:.2} s)");
    assert!(new <= secs(TIDY_EVERY) + secs(REST) + secs(tick_pace::Pace::Rest(REST).tolerance()), "now {new:.2} s, before {old:.2} s");
}
