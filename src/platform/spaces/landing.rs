//! The real window's last stretch (1.9, round sm-landing): an incoming window, hidden in its
//! stage's Space, takes over from its flying picture near the end of the flight, in a Space
//! of its own that shows, drawn through that Space's transform at the picture's pose
//! ([`crate::ui::landing_math`]), and flies the rest itself: its own shadow, its own place in
//! the window order, its live content. At the end the transform is the identity, the window
//! goes onto its Desktop and the Space is ended.
//!
//! **One thread a window** (QoS user-interactive): the Space is made, its transform set to
//! the first pose and shown, empty, as soon as the stretch is planned (S4: four Spaces made,
//! filled and shown in 5–8 ms); at the handover the window goes into it (the bridged add,
//! out of every other Space), read back; once the window server lists it on screen the strip
//! hears [`StretchNote::Shown`] and its picture goes; each step's transform at its instant
//! (~20 µs a call, S4); then the identity, the move home (as a show), the Space ended.
//!
//! **Safe by construction.** The Space is made with the classic call, so it is this
//! process's and ends with it: a crash puts the window back on the Desktop shown. The
//! controller still counts the window hidden until its unveil ([`SpacesEngine::holds`]), and
//! its show then finds the window out of every stage Space and at its frame (the window list
//! reports a window in a transformed Space at its frame: [`windows::set_transformed`]), so it
//! asks the app nothing. A show before the handover stops the stretch (the window never takes
//! over); a hide mid-flight stops it and takes the window out of the landing Space with every
//! other Space ([`SpacesEngine::stop_stretch`]); a bring-back ends every stretch first
//! ([`SpacesEngine::end_stretches`]).
//!
//! **The transform's route.** The stills of S4 set it with the bridged call on a bridged
//! Space; this Space is a classic one (it must end with us), so the classic call is tried
//! first and read back once; if the read-back differs, the bridged call is used for the rest
//! of the run.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use objc2_core_foundation::CGAffineTransform;

use super::super::ffi::{ALL_SPACES_MASK, Route, Sky};
use super::{SpacesEngine, hidden_in, shared};
use crate::platform::window_engine::{Stretch, StretchDone, StretchNote};
use crate::platform::windows;
use crate::ui::landing_math::{Affine, IDENTITY};

/// How long the strip may wait for the window server to list a window that took over on
/// screen before its picture goes anyway (a frame or two is usual).
const SHOWN_WAIT: Duration = Duration::from_millis(60);

/// How long a stretch stopped by a hide waits for the hide to take its window out of the
/// landing Space before the Space is ended (ending a Space with a window in it puts the
/// window on the Desktop shown).
const STOP_WAIT: Duration = Duration::from_millis(300);

/// How a bring-back on another thread waits for the stretches' lock (a stretch holds it for
/// a call or two at a time).
const END_WAIT: Duration = Duration::from_millis(50);

/// Which call sets the landing Spaces' transform: not tried yet, the classic one, the
/// bridged one ([`SpacesEngine::set_landing_transform`]).
static TRANSFORM_ROUTE: AtomicU8 = AtomicU8::new(ROUTE_UNTRIED);
const ROUTE_UNTRIED: u8 = 0;
const ROUTE_CLASSIC: u8 = 1;
const ROUTE_BRIDGED: u8 = 2;

/// Where a stretch is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    /// Planned, its Space made or being made; the window still in its stage's Space.
    Waiting,
    /// The window flies in its landing Space.
    Flying,
}

/// One window's stretch, as the engine keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Fly {
    pub(super) phase: Phase,
    /// No more steps: a show before the handover, a hide mid-flight, a bring-back.
    pub(super) stop: bool,
    /// A bring-back took the window home and ended the Space already.
    pub(super) ended: bool,
    /// The landing Space (0 until made).
    pub(super) space: u64,
}

/// What the stretches counted, for `ctl state`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct StretchCounts {
    pub(super) planned: u64,
    pub(super) shown: u64,
    pub(super) skipped: u64,
    pub(super) landed: u64,
    pub(super) stopped: u64,
}

/// Sleeps until `at` (a millisecond short by sleeping, the rest in short naps).
fn sleep_until(at: Instant) {
    loop {
        let now = Instant::now();
        if now >= at {
            return;
        }
        let left = at - now;
        if left > Duration::from_millis(2) {
            std::thread::sleep(left - Duration::from_millis(1));
        } else {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

fn affine(m: Affine) -> CGAffineTransform {
    CGAffineTransform { a: m[0], b: m[1], c: m[2], d: m[3], tx: m[4], ty: m[5] }
}

/// True when two transforms are the same (read back).
fn same(a: [f64; 6], b: Affine) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

impl SpacesEngine {
    /// The stretches, behind their own lock (taken before the state's when both are, never
    /// the other way).
    pub(super) fn stretches(&self) -> MutexGuard<'_, BTreeMap<u32, Fly>> {
        self.stretches.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Sets landing Space `space`'s transform to `m` by the route that works ([`TRANSFORM_ROUTE`]).
    fn set_landing_transform(&self, sky: &Sky, space: u64, m: Affine) {
        match TRANSFORM_ROUTE.load(Ordering::Relaxed) {
            ROUTE_CLASSIC => {
                let _ = sky.set_transform(Route::Classic, space, affine(m));
            }
            ROUTE_BRIDGED => {
                let _ = sky.set_transform(Route::Bridged, space, affine(m));
            }
            _ => {
                let _ = sky.set_transform(Route::Classic, space, affine(m));
                let classic = sky.get_transform(space).is_some_and(|t| same(t, m));
                let route = if classic {
                    ROUTE_CLASSIC
                } else {
                    let _ = sky.set_transform(Route::Bridged, space, affine(m));
                    ROUTE_BRIDGED
                };
                tracing::info!(route = if classic { "classic" } else { "bridged" }, readback = ?sky.get_transform(space), "spaces engine: the landing Spaces' transform is set by this call");
                TRANSFORM_ROUTE.store(route, Ordering::Relaxed);
            }
        }
    }

    /// Plans window `s.window`'s stretch on a thread of its own (see the module notes); false
    /// when the engine does not hold it in a stage Space or the stretch has no steps.
    pub(super) fn begin_stretch(&self, s: Stretch, done: StretchDone) -> bool {
        // A window put home for a Dock press shows already: its picture lands on it.
        if s.steps.is_empty() || !self.holds(s.window) || self.lent(s.window) || self.sky().is_none() {
            return false;
        }
        {
            let mut l = self.stretches();
            if l.contains_key(&s.window) {
                return false;
            }
            l.insert(s.window, Fly { phase: Phase::Waiting, stop: false, ended: false, space: 0 });
        }
        self.state().stretch.planned += 1;
        let id = s.window;
        let spawned = std::thread::Builder::new().name(format!("landing-{id}")).spawn(move || {
            // SAFETY: sets this thread's own QoS class; no pointers.
            unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0) };
            let engine = shared();
            let note = engine.fly(&s, &done);
            {
                let mut st = engine.state();
                match note {
                    StretchNote::Landed => st.stretch.landed += 1,
                    StretchNote::Stopped => st.stretch.stopped += 1,
                    StretchNote::Skipped(_) => st.stretch.skipped += 1,
                    StretchNote::Shown => {}
                }
            }
            done(id, note);
        });
        if spawned.is_err() {
            self.stretches().remove(&id);
            return false;
        }
        true
    }

    /// The stretch's thread: what became of it at the end.
    fn fly(&self, s: &Stretch, done: &StretchDone) -> StretchNote {
        let id = s.window;
        let Some(sky) = self.sky() else { return self.skip(id, None, "no window server calls") };
        let Some(&(first_at, first)) = s.steps.first() else { return self.skip(id, None, "no steps") };
        // The Space, ahead: made, its transform the first pose, shown empty.
        let space = match sky.create(Route::Classic, false) {
            Ok(space) if space != 0 => space,
            _ => return self.skip(id, None, "no Space made"),
        };
        self.set_landing_transform(sky, space, first);
        let _ = sky.show_hide(Route::Classic, &[space], true);
        match self.stretches().get_mut(&id) {
            Some(f) if !f.stop => f.space = space,
            _ => return self.skip(id, Some(space), "stopped before the handover"),
        }
        sleep_until(first_at);
        // The handover: only while nothing stopped it and the window is still in its stage's
        // Space (the controller has not shown or hidden it meanwhile).
        let took = {
            let mut l = self.stretches();
            let stopped = l.get(&id).is_none_or(|f| f.stop);
            let stage = self.state().held.get(&id).map(|h| h.space);
            match stage {
                Some(stage) if !stopped && hidden_in(&sky.spaces_of(&[id]), stage) => {
                    windows::set_transformed(id, Some(s.frame));
                    let late = first_at.elapsed();
                    // The pose for now, not the handover's, when the thread woke late.
                    let now_m = s.steps.iter().rev().find(|(at, _)| *at <= Instant::now()).map_or(first, |(_, m)| *m);
                    self.set_landing_transform(sky, space, now_m);
                    if let Err(e) = sky.add_remove_from(Route::Bridged, space, &[id], ALL_SPACES_MASK) {
                        tracing::debug!(id, space, "spaces engine: a stretch's window could not be sent into its Space: {e}");
                    }
                    let (ok, listed) = self.read_back(sky, id, |now| hidden_in(now, space));
                    if ok {
                        if let Some(f) = l.get_mut(&id) {
                            f.phase = Phase::Flying;
                        }
                        tracing::debug!(id, space, late_us = late.as_micros() as u64, "spaces engine: the window flies the last stretch itself");
                        Ok(())
                    } else {
                        // Back where it was, out of the landing Space too.
                        let _ = sky.add_remove_from(Route::Bridged, stage, &[id], ALL_SPACES_MASK);
                        windows::set_transformed(id, None);
                        tracing::debug!(id, space, ?listed, "spaces engine: a stretch's window did not go into its Space; it stays hidden");
                        Err("the window did not go into its Space")
                    }
                }
                _ if stopped => Err("stopped before the handover"),
                _ => Err("the window was no longer in its stage's Space"),
            }
        };
        if let Err(why) = took {
            return self.skip(id, Some(space), why);
        }
        // On screen at its first pose: the picture goes.
        let shown_by = Instant::now() + SHOWN_WAIT;
        while Instant::now() < shown_by && !windows::describe(&[id]).first().is_some_and(|w| w.onscreen) {
            std::thread::sleep(Duration::from_micros(500));
        }
        self.state().stretch.shown += 1;
        done(id, StretchNote::Shown);
        // The rest of the path.
        let mut stopped = false;
        for &(at, m) in s.steps.iter().skip(1) {
            sleep_until(at);
            let l = self.stretches();
            if l.get(&id).is_none_or(|f| f.stop) {
                stopped = true;
                break;
            }
            self.set_landing_transform(sky, space, m);
        }
        if stopped {
            return self.stopped(sky, id, space);
        }
        // Home: the identity (the last step), onto its Desktop as a show puts it, the Space
        // ended.
        let mut l = self.stretches();
        if l.get(&id).is_none_or(|f| f.stop) {
            drop(l);
            return self.stopped(sky, id, space);
        }
        self.set_landing_transform(sky, space, IDENTITY);
        let home = self.state().held.get(&id).map(|h| h.home);
        let (desktops, shown) = Self::desktops(sky);
        let home = home.filter(|h| desktops.contains(h)).or(shown.first().copied());
        if let Some(home) = home {
            let _ = sky.add_remove_from(Route::Bridged, home, &[id], ALL_SPACES_MASK);
        }
        let (out, listed) = self.read_back(sky, id, |now| !now.contains(&space));
        windows::set_transformed(id, None);
        let _ = sky.destroy(Route::Classic, space);
        l.remove(&id);
        drop(l);
        if !out {
            tracing::warn!(id, space, ?listed, "spaces engine: a landed window was still in its landing Space; the Space ended (it shows on the Desktop shown)");
        }
        tracing::debug!(id, ?home, "spaces engine: the window landed on its Desktop");
        StretchNote::Landed
    }

    /// A stretch that never took over: its Space (if made) ended, its entry gone.
    fn skip(&self, id: u32, space: Option<u64>, why: &'static str) -> StretchNote {
        if let (Some(space), Some(sky)) = (space, self.sky()) {
            let _ = sky.destroy(Route::Classic, space);
        }
        self.stretches().remove(&id);
        tracing::debug!(id, why, "spaces engine: a stretch did not take over; the picture lands");
        StretchNote::Skipped(why)
    }

    /// A stretch stopped mid-flight: once its window is out of the landing Space (a hide
    /// takes it out; a bring-back took it home already), the Space is ended.
    fn stopped(&self, sky: &Sky, id: u32, space: u64) -> StretchNote {
        let ended = self.stretches().get(&id).is_some_and(|f| f.ended);
        if !ended {
            let by = Instant::now() + STOP_WAIT;
            while Instant::now() < by && sky.spaces_of(&[id]).contains(&space) {
                std::thread::sleep(Duration::from_millis(2));
            }
            if sky.spaces_of(&[id]).contains(&space) {
                // Nothing took it: home, as a show would.
                let _ = sky.set_transform(Route::Classic, space, affine(IDENTITY));
                let home = self.state().held.get(&id).map(|h| h.home);
                let (_, shown) = Self::desktops(sky);
                if let Some(home) = home.or(shown.first().copied()) {
                    let _ = sky.add_remove_from(Route::Bridged, home, &[id], ALL_SPACES_MASK);
                }
            }
            let _ = sky.destroy(Route::Classic, space);
        }
        windows::set_transformed(id, None);
        self.stretches().remove(&id);
        tracing::debug!(id, space, "spaces engine: a stretch stopped mid-flight");
        StretchNote::Stopped
    }

    /// Window `id`'s stretch stops (a show or a hide of the window is about to go): one not
    /// taken over yet never takes over. The guard holds the stretches while the caller moves
    /// the window when it flies (`Some`: its landing Space, which the caller's move must take
    /// it out of: every Space, [`ALL_SPACES_MASK`]); `None` when it is not flying.
    pub(super) fn stop_stretch(&self, id: u32, flying_too: bool) -> (Option<MutexGuard<'_, BTreeMap<u32, Fly>>>, Option<u64>) {
        let mut l = self.stretches();
        let Some(f) = l.get_mut(&id) else { return (None, None) };
        match f.phase {
            Phase::Waiting => {
                f.stop = true;
                (None, None)
            }
            Phase::Flying if flying_too => {
                f.stop = true;
                let space = f.space;
                (Some(l), Some(space))
            }
            Phase::Flying => (None, None),
        }
    }

    /// True while window `id` has a stretch planned or flying that has not stopped: until it
    /// lands it is in its stage Space or its landing Space, on no Desktop for macOS.
    pub(super) fn in_flight(&self, id: u32) -> bool {
        self.stretches().get(&id).is_some_and(|f| !f.stop && !f.ended)
    }

    /// Window `id`'s stretch, when one not taken over yet is no longer wanted.
    pub(super) fn cancel(&self, id: u32) {
        if let Some(f) = self.stretches().get_mut(&id).filter(|f| f.phase == Phase::Waiting) {
            f.stop = true;
        }
    }

    /// Every stretch ends now (a bring-back, any thread): a flying window at the identity
    /// and onto its Desktop, its Space ended; one not taken over never does. Skipped when the
    /// stretches stay busy past [`END_WAIT`] (the Spaces are classic: a process that dies
    /// puts their windows back by itself).
    pub(super) fn end_stretches(&self, why: &str) {
        let by = Instant::now() + END_WAIT;
        let mut l = loop {
            match self.stretches.try_lock() {
                Ok(l) => break l,
                Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() < by => std::thread::sleep(Duration::from_millis(1)),
                Err(std::sync::TryLockError::WouldBlock) => {
                    tracing::warn!(why, "spaces engine: the stretches stayed busy; their windows were not taken home");
                    return;
                }
            }
        };
        if l.is_empty() {
            return;
        }
        let Some(sky) = self.sky() else { return };
        let (_, shown) = Self::desktops(sky);
        for (id, f) in l.iter_mut() {
            f.stop = true;
            if f.phase != Phase::Flying || f.ended {
                continue;
            }
            let _ = sky.set_transform(Route::Classic, f.space, affine(IDENTITY));
            let home = self.state().held.get(id).map(|h| h.home).or(shown.first().copied());
            if let Some(home) = home {
                let _ = sky.add_remove_from(Route::Bridged, home, &[*id], ALL_SPACES_MASK);
            }
            let _ = sky.destroy(Route::Classic, f.space);
            windows::set_transformed(*id, None);
            f.ended = true;
            tracing::info!(why, id, "spaces engine: a window flying its last stretch was taken home");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transform read back as set is the same one; another is not.
    #[test]
    fn a_transform_reads_back_as_set() {
        let m = [0.5, -0.1, 0.0, 0.52, 120.0, -40.0];
        assert!(same(m, m));
        assert!(!same([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], m));
        assert!(same(IDENTITY, IDENTITY));
    }

    fn engine() -> SpacesEngine {
        SpacesEngine { sky: std::sync::OnceLock::new(), state: std::sync::Mutex::new(super::super::State::default()), stretches: std::sync::Mutex::new(BTreeMap::new()) }
    }

    /// A stretch is planned only for a window the engine holds hidden; a show before the
    /// handover stops it, one mid-flight leaves it to land; a hide mid-flight stops it and
    /// holds the stretches while the window is moved out of its landing Space; a cancel only
    /// reaches one not taken over.
    #[test]
    fn a_stretch_stops_for_a_show_or_a_hide() {
        let e = engine();
        let s = Stretch { window: 7, frame: crate::engine::geometry::Rect::new(0.0, 0.0, 800.0, 600.0), steps: vec![(Instant::now(), IDENTITY)] };
        let done: StretchDone = std::sync::Arc::new(|_, _| {});
        assert!(!e.begin_stretch(s, done), "not hidden by the engine: nothing planned");
        e.stretches().insert(7, Fly { phase: Phase::Waiting, stop: false, ended: false, space: 0 });
        let (guard, space) = e.stop_stretch(7, false);
        assert!(guard.is_none() && space.is_none());
        assert!(e.stretches()[&7].stop, "a show before the handover: it never takes over");
        e.stretches().insert(8, Fly { phase: Phase::Flying, stop: false, ended: false, space: 900 });
        let (guard, space) = e.stop_stretch(8, false);
        assert!(guard.is_none() && space.is_none());
        assert!(!e.stretches()[&8].stop, "a show mid-flight: it lands by itself");
        e.cancel(8);
        assert!(!e.stretches()[&8].stop, "a cancel mid-flight: it lands");
        let (guard, space) = e.stop_stretch(8, true);
        assert_eq!(space, Some(900), "a hide mid-flight takes the window out of its landing Space");
        assert!(guard.is_some_and(|g| g[&8].stop));
        assert_eq!(e.stop_stretch(99, true).1, None, "no stretch: nothing");
        assert!(e.sky.get().is_none(), "no window server asked");
    }

    /// The controller owes a focus to the windows whose stretch still tells its end: planned
    /// or flying, not one already stopped.
    #[test]
    fn stretching_names_the_stretches_not_stopped() {
        use crate::platform::window_engine::WindowEngine;
        let e = engine();
        e.stretches().insert(1, Fly { phase: Phase::Waiting, stop: false, ended: false, space: 0 });
        e.stretches().insert(2, Fly { phase: Phase::Flying, stop: false, ended: false, space: 900 });
        e.stretches().insert(3, Fly { phase: Phase::Flying, stop: true, ended: false, space: 901 });
        assert_eq!(e.stretching(), [1, 2].into());
    }

    /// Waits end at their instant, never before.
    #[test]
    fn a_step_waits_for_its_instant() {
        let at = Instant::now() + Duration::from_millis(5);
        sleep_until(at);
        assert!(Instant::now() >= at);
        sleep_until(Instant::now() - Duration::from_millis(1));
    }
}
