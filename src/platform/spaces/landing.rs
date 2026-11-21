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
//! **Several at once** (round landing-order): a switch's stretches fly as one convoy
//! ([`landing_order`]), ranked by the stage's order: each landing Space is shown after those
//! behind it, sets each step's transform a millisecond or so after them, and its window goes
//! onto its Desktop only once the windows behind it have (bounded waits, never on a window in
//! front). Live 11-15, two overlapping windows of two apps traded places while they flew and
//! the one behind, landing last, was listed on top until the switch's focus. The level the
//! Spaces are made with is logged once a run; the test switch's
//! `STAGE_MANAGER_LANDING_LEVEL=ranked` gives them levels by rank instead.
//!
//! **The transform's route.** The stills of S4 set it with the bridged call on a bridged
//! Space; this Space is a classic one (it must end with us), so the classic call is tried
//! first and read back once; if the read-back differs, the bridged call is used for the rest
//! of the run.

use crate::platform::clock;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use objc2::sel;
use objc2_core_foundation::CGAffineTransform;

use super::super::ffi::{ALL_SPACES_MASK, Route, Sky, answers};
use super::{SpacesEngine, hidden_in, shared};
use crate::engine::landing_order::{self, Leg, LevelMode, Waited};
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

/// How long the WM thread (or the strip's) waits for the stretches' lock before it goes on
/// without it ([`SpacesEngine::stretches_within`]): a stretch holds it for a few window
/// server calls and at most two read-backs ([`super::READ_BACK`] each), so longer means a
/// stretch's thread is stuck; a show, a hide or a quit is never held up by it.
const LOCK_WAIT: Duration = Duration::from_millis(250);

/// The landing Space a hide is told when the stretches could not be asked
/// ([`SpacesEngine::stop_stretch`]): unknown, so the window goes out of every Space.
pub(super) const UNKNOWN_SPACE: u64 = 0;

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
    /// The Desktop the window was hidden from, where it lands (0: not known). Kept here:
    /// the unveil's show, which comes mid-flight, finds the window out of every stage Space
    /// and forgets it as hidden (its Desktop with it), and two displays have two Desktops
    /// shown (review A-2).
    pub(super) home: u64,
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
        let now = clock::now();
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

/// True when a window whose Spaces are `now` is still in its landing Space `space` at the
/// last step, so it goes home. A hide that could not ask the stretches (a stretch's thread
/// stuck past [`LOCK_WAIT`]) takes the window out of every Space without stopping its
/// stretch: moved home then, the hidden window would show (review A-6).
fn lands_home(now: &[u64], space: u64) -> bool {
    now.contains(&space)
}

/// True when a last stretch can be flown: a landing Space can be shown (`show`,
/// `SLSShowSpaces`) and transformed, by the classic call with its read back (`classic`) or
/// by the bridged operation (`bridged`). Without, the window would wait in a Space not shown,
/// or fly at full size, after its picture went (review A-10).
fn can_land(show: bool, classic: bool, bridged: bool) -> bool {
    show && (classic || bridged)
}

/// True when two transforms are the same (read back).
fn same(a: [f64; 6], b: Affine) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

/// The landing Spaces' level ([`LevelMode`]): the test switch's
/// [`landing_order::LEVEL_ENV`], read once; a stretch flies only in a test or review run.
fn level_mode() -> LevelMode {
    static M: std::sync::OnceLock<LevelMode> = std::sync::OnceLock::new();
    *M.get_or_init(|| {
        let gated = crate::ui::landing_math::Landing::current().live().is_some();
        let mode = LevelMode::from(gated, std::env::var(landing_order::LEVEL_ENV).ok().as_deref());
        if mode != LevelMode::AsMade {
            tracing::info!(mode = mode.name(), "spaces engine: the landing Spaces are given levels by rank");
        }
        mode
    })
}

impl SpacesEngine {
    /// The stretches, behind their own lock (taken before the state's when both are, never
    /// the other way).
    pub(super) fn stretches(&self) -> MutexGuard<'_, BTreeMap<u32, Fly>> {
        self.stretches.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The stretches for a caller that is not a stretch's own thread (the WM thread, the
    /// strip's): `None` when the lock stays busy past [`LOCK_WAIT`] (`what` names the caller
    /// in the log, once per episode); then no later caller waits until the lock is free once
    /// more. The caller goes on as when there is no stretch.
    pub(super) fn stretches_within(&self, what: &str) -> Option<MutexGuard<'_, BTreeMap<u32, Fly>>> {
        let wait = if self.stretches_busy.load(Ordering::Relaxed) { Duration::ZERO } else { LOCK_WAIT };
        let by = clock::now() + wait;
        loop {
            match self.stretches.try_lock() {
                Ok(l) => {
                    if self.stretches_busy.swap(false, Ordering::Relaxed) {
                        tracing::info!(what, "spaces engine: the stretches are free again");
                    }
                    return Some(l);
                }
                Err(std::sync::TryLockError::Poisoned(p)) => return Some(p.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) if clock::now() < by => std::thread::sleep(Duration::from_millis(1)),
                Err(std::sync::TryLockError::WouldBlock) => {
                    if !self.stretches_busy.swap(true, Ordering::Relaxed) {
                        tracing::warn!(what, wait_ms = LOCK_WAIT.as_millis() as u64, "spaces engine: the stretches stayed busy (a stretch's thread is stuck); gone on without them");
                    }
                    return None;
                }
            }
        }
    }

    /// Sets landing Space `space`'s transform to `m` by the route that works
    /// ([`TRANSFORM_ROUTE`]); false when that call could not be sent (a missing function or
    /// class).
    fn set_landing_transform(&self, sky: &Sky, space: u64, m: Affine) -> bool {
        match TRANSFORM_ROUTE.load(Ordering::Relaxed) {
            ROUTE_CLASSIC => sky.set_transform(Route::Classic, space, affine(m)).is_ok(),
            ROUTE_BRIDGED => sky.set_transform(Route::Bridged, space, affine(m)).is_ok(),
            _ => {
                let _ = sky.set_transform(Route::Classic, space, affine(m));
                let classic = sky.get_transform(space).is_some_and(|t| same(t, m));
                let (route, sent) = if classic {
                    (ROUTE_CLASSIC, true)
                } else {
                    (ROUTE_BRIDGED, sky.set_transform(Route::Bridged, space, affine(m)).is_ok())
                };
                tracing::info!(route = if classic { "classic" } else { "bridged" }, sent, readback = ?sky.get_transform(space), "spaces engine: the landing Spaces' transform is set by this call");
                TRANSFORM_ROUTE.store(route, Ordering::Relaxed);
                sent
            }
        }
    }

    /// Gives landing Space `space` (window `id`'s, rank `rank` of `count`) its level when the
    /// test switch asks for one ([`landing_order::level`]); the level it was made with is
    /// logged once a run (what the window server gives a classic Space by default).
    fn set_landing_level(sky: &Sky, id: u32, space: u64, rank: usize, count: usize) {
        static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let mode = level_mode();
        let made = sky.get_level(space);
        let want = landing_order::level(mode, rank, count);
        let set = want.map(|level| sky.set_level(Route::Classic, space, level));
        let now = if want.is_some() { sky.get_level(space) } else { made };
        if !SAID.swap(true, Ordering::Relaxed) {
            tracing::info!(id, space, ?made, mode = mode.name(), "spaces engine: the landing Spaces are made at this level");
        }
        if want.is_some() {
            tracing::debug!(id, space, rank, count, ?made, ?want, ?set, readback = ?now, "spaces engine: a landing Space's level set by its rank");
        }
    }

    /// True when this Mac's SkyLight has what a last stretch needs ([`can_land`]): asked once.
    pub(super) fn landing_calls(&self) -> bool {
        static CAN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let Some(sky) = self.sky() else { return false };
        *CAN.get_or_init(|| {
            let bridged = Sky::class(c"SLSBridgedSpaceSetTransformOperation").is_some_and(|c| answers(c, sel!(initWithSpaceID:transform:options:)).is_ok());
            let can = can_land(sky.show.is_some(), sky.set_transform.is_some() && sky.get_transform.is_some(), bridged);
            if !can {
                tracing::warn!(show = sky.show.is_some(), classic = sky.set_transform.is_some(), read_back = sky.get_transform.is_some(), bridged, "spaces engine: SkyLight lacks a call the last stretch needs; incoming windows land as pictures");
            }
            can
        })
    }

    /// Plans window `s.window`'s stretch on a thread of its own (see the module notes); false
    /// when the engine does not hold it in a stage Space or the stretch has no steps.
    pub(super) fn begin_stretch(&self, s: Stretch, done: StretchDone) -> bool {
        // A window put home for a Dock press shows already: its picture lands on it. One
        // parked for a lend whose frame is owed is not where its picture flies from (its show
        // sets the frame: review A-4).
        if s.steps.is_empty() || !self.holds(s.window) || self.lent(s.window) || self.state().parked.contains_key(&s.window) || self.sky().is_none() {
            return false;
        }
        {
            let Some(mut l) = self.stretches_within("a stretch planned") else { return false };
            if l.contains_key(&s.window) {
                return false;
            }
            let home = self.state().held.get(&s.window).map_or(0, |h| h.home);
            l.insert(s.window, Fly { phase: Phase::Waiting, stop: false, ended: false, space: 0, home });
        }
        self.state().stretch.planned += 1;
        let id = s.window;
        let spawned = std::thread::Builder::new().name(format!("landing-{id}")).spawn(move || {
            // SAFETY: sets this thread's own QoS class; no pointers.
            unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0) };
            let engine = shared();
            let note = engine.fly(&s, &done);
            // However it ended, nothing in front waits for it any more.
            Self::gone(&s);
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
        if !self.set_landing_transform(sky, space, first) {
            return self.skip(id, Some(space), "its Space's transform could not be set");
        }
        let count = s.convoy.as_ref().map_or(1, |c| c.count());
        Self::set_landing_level(sky, id, space, s.rank, count);
        // Shown back to front: the Spaces behind it first (round landing-order).
        if let Some(c) = &s.convoy {
            let waited = c.wait_behind(s.rank, Leg::Shown, clock::now() + landing_order::SHOW_WAIT, || false);
            if !matches!(waited, Waited::Ready(d) if d < Duration::from_millis(1)) {
                tracing::debug!(id, space, rank = s.rank, ?waited, "spaces engine: a landing Space waited for those behind it to show");
            }
        }
        if let Err(e) = sky.show_hide(Route::Classic, &[space], true) {
            tracing::debug!(id, space, "spaces engine: a landing Space could not be shown: {e}");
            return self.skip(id, Some(space), "its Space could not be shown");
        }
        if let Some(c) = &s.convoy {
            c.mark(s.rank, Leg::Shown);
        }
        // Within each step the front's transform follows those behind it.
        let lag = landing_order::stagger(s.rank, count);
        if !self.claim_space(id, space) {
            return self.skip(id, Some(space), "stopped before the handover");
        }
        sleep_until(first_at + lag);
        // The handover: only while nothing stopped it and the window is still in its stage's
        // Space (the controller has not shown or hidden it meanwhile).
        let took = {
            let mut l = self.stretches();
            let stopped = l.get(&id).is_none_or(|f| f.stop);
            let held = self.state().held.get(&id).copied();
            match held.map(|h| (h.space, h.home)) {
                Some((stage, home)) if !stopped && hidden_in(&sky.spaces_of(&[id]), stage) => {
                    if let Some(f) = l.get_mut(&id) {
                        f.home = home;
                    }
                    windows::set_transformed(id, Some(s.frame));
                    let late = first_at.elapsed();
                    // The pose for now, not the handover's, when the thread woke late.
                    let now_m = s.steps.iter().rev().find(|(at, _)| *at <= clock::now()).map_or(first, |(_, m)| *m);
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
        let shown_by = clock::now() + SHOWN_WAIT;
        while clock::now() < shown_by && !windows::describe(&[id]).first().is_some_and(|w| w.onscreen) {
            std::thread::sleep(Duration::from_micros(500));
        }
        self.state().stretch.shown += 1;
        done(id, StretchNote::Shown);
        // The rest of the path.
        let mut stopped = false;
        for &(at, m) in s.steps.iter().skip(1) {
            sleep_until(at + lag);
            let l = self.stretches();
            if l.get(&id).is_none_or(|f| f.stop) {
                stopped = true;
                break;
            }
            self.set_landing_transform(sky, space, m);
        }
        if stopped {
            Self::gone(s);
            return self.stopped(sky, id, space);
        }
        // Landed back to front: a window goes onto its Desktop (listed on top there) only
        // once the windows behind it have; it waits at its frame, at the identity.
        if let Some(c) = &s.convoy {
            let waited = c.wait_behind(s.rank, Leg::Gone, clock::now() + landing_order::LAND_WAIT, || self.stretches().get(&id).is_none_or(|f| f.stop));
            match waited {
                Waited::Ready(d) if d < Duration::from_millis(1) => {}
                Waited::Ready(d) => tracing::debug!(id, rank = s.rank, waited_ms = d.as_millis() as u64, "spaces engine: a window waited at its frame for the windows behind it to land"),
                Waited::TimedOut => tracing::debug!(id, rank = s.rank, wait_ms = landing_order::LAND_WAIT.as_millis() as u64, "spaces engine: a window behind still flew; this one lands first"),
                Waited::Stopped => {}
            }
        }
        // Home: the identity (the last step), onto its Desktop as a show puts it, the Space
        // ended.
        let mut l = self.stretches();
        if l.get(&id).is_none_or(|f| f.stop) || !lands_home(&sky.spaces_of(&[id]), space) {
            drop(l);
            Self::gone(s);
            return self.stopped(sky, id, space);
        }
        self.set_landing_transform(sky, space, IDENTITY);
        let home = self.home_of(id, l.get(&id));
        let (desktops, _) = Self::desktops(sky);
        let home = home.filter(|h| desktops.contains(h)).or_else(|| self.shown_on_own_screen(sky, id, None, None));
        if let Some(home) = home {
            let _ = sky.add_remove_from(Route::Bridged, home, &[id], ALL_SPACES_MASK);
        }
        // Onto its Desktop: the window in front may follow.
        Self::gone(s);
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

    /// Stretch `s` lands no more in its landing Space (on its Desktop, stopped or skipped):
    /// the windows in front of it no longer wait for it ([`landing_order::Convoy`]).
    fn gone(s: &Stretch) {
        if let Some(c) = &s.convoy {
            c.mark(s.rank, Leg::Gone);
        }
    }

    /// Stretch `id`'s landing Space `space`, made, noted; false when the stretch stopped
    /// meanwhile (a show before the handover) or is gone. The stretches' lock is let go
    /// before this returns: the caller's [`Self::skip`] takes it again. (Held across the
    /// skip, as a `match` on the guard holds it, the thread locked itself out for good, the
    /// stretches' lock with it, and every later call of the WM thread that asks for the
    /// stretches waited forever: the quit of live 11-15 16:34:32, a show at 16:34:28.815
    /// had stopped a stretch whose Space was being made.)
    fn claim_space(&self, id: u32, space: u64) -> bool {
        let mut l = self.stretches();
        match l.get_mut(&id) {
            Some(f) if !f.stop => {
                f.space = space;
                true
            }
            _ => false,
        }
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
        let (ended, fly) = {
            let l = self.stretches();
            (l.get(&id).is_some_and(|f| f.ended), l.get(&id).copied())
        };
        if !ended {
            let by = clock::now() + STOP_WAIT;
            while clock::now() < by && sky.spaces_of(&[id]).contains(&space) {
                std::thread::sleep(Duration::from_millis(2));
            }
            if sky.spaces_of(&[id]).contains(&space) {
                // Nothing took it: home, as a show would.
                let _ = sky.set_transform(Route::Classic, space, affine(IDENTITY));
                let home = self.home_of(id, fly.as_ref());
                if let Some(home) = home.or_else(|| self.shown_on_own_screen(sky, id, None, None)) {
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

    /// The Desktop window `id` lands on: the one it is held for, else the one its stretch
    /// `fly` noted as it was planned (the unveil's show forgets the window as held once it
    /// flies); `None` when neither is known.
    pub(super) fn home_of(&self, id: u32, fly: Option<&Fly>) -> Option<u64> {
        self.state().held.get(&id).map(|h| h.home).or_else(|| fly.map(|f| f.home).filter(|h| *h != 0))
    }

    /// Window `id`'s stretch stops (a show or a hide of the window is about to go): one not
    /// taken over yet never takes over. The guard holds the stretches while the caller moves
    /// the window when it flies (`Some`: its landing Space, which the caller's move must take
    /// it out of: every Space, [`ALL_SPACES_MASK`]); `None` when it is not flying. With the
    /// stretches stuck ([`Self::stretches_within`]) nothing is held, and a hide is told
    /// [`UNKNOWN_SPACE`] so that it takes the window out of every Space all the same.
    pub(super) fn stop_stretch(&self, id: u32, flying_too: bool) -> (Option<MutexGuard<'_, BTreeMap<u32, Fly>>>, Option<u64>) {
        let Some(mut l) = self.stretches_within(if flying_too { "a hide" } else { "a show" }) else {
            return (None, flying_too.then_some(UNKNOWN_SPACE));
        };
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
        self.stretches_within("a window's Desktop asked").is_some_and(|l| l.get(&id).is_some_and(|f| !f.stop && !f.ended))
    }

    /// Window `id`'s stretch, when one not taken over yet is no longer wanted.
    pub(super) fn cancel(&self, id: u32) {
        let Some(mut l) = self.stretches_within("a stretch cancelled") else { return };
        if let Some(f) = l.get_mut(&id).filter(|f| f.phase == Phase::Waiting) {
            f.stop = true;
        }
    }

    /// Every stretch ends now (a bring-back, any thread): a flying window at the identity
    /// and onto its Desktop, its Space ended; one not taken over never does. Skipped when the
    /// stretches stay busy past [`END_WAIT`] (the Spaces are classic: a process that dies
    /// puts their windows back by itself).
    pub(super) fn end_stretches(&self, why: &str) {
        let by = clock::now() + END_WAIT;
        let mut l = loop {
            match self.stretches.try_lock() {
                Ok(l) => break l,
                Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) if clock::now() < by => std::thread::sleep(Duration::from_millis(1)),
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
        for (id, f) in l.iter_mut() {
            f.stop = true;
            if f.phase != Phase::Flying || f.ended {
                continue;
            }
            let _ = sky.set_transform(Route::Classic, f.space, affine(IDENTITY));
            let home = self.home_of(*id, Some(&*f)).or_else(|| self.shown_on_own_screen(sky, *id, None, None));
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
        SpacesEngine {
            sky: std::sync::OnceLock::new(),
            state: std::sync::Mutex::new(super::super::State::default()),
            stretches: std::sync::Mutex::new(BTreeMap::new()),
            stretches_busy: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// A stretch is planned only for a window the engine holds hidden; a show before the
    /// handover stops it, one mid-flight leaves it to land; a hide mid-flight stops it and
    /// holds the stretches while the window is moved out of its landing Space; a cancel only
    /// reaches one not taken over.
    #[test]
    fn a_stretch_stops_for_a_show_or_a_hide() {
        let e = engine();
        let s = Stretch { window: 7, frame: crate::engine::geometry::Rect::new(0.0, 0.0, 800.0, 600.0), steps: vec![(Instant::now(), IDENTITY)], rank: 0, convoy: None };
        let done: StretchDone = std::sync::Arc::new(|_, _| {});
        assert!(!e.begin_stretch(s, done), "not hidden by the engine: nothing planned");
        e.stretches().insert(7, Fly { phase: Phase::Waiting, stop: false, ended: false, space: 0, home: 0 });
        let (guard, space) = e.stop_stretch(7, false);
        assert!(guard.is_none() && space.is_none());
        assert!(e.stretches()[&7].stop, "a show before the handover: it never takes over");
        e.stretches().insert(8, Fly { phase: Phase::Flying, stop: false, ended: false, space: 900, home: 0 });
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
        e.stretches().insert(1, Fly { phase: Phase::Waiting, stop: false, ended: false, space: 0, home: 0 });
        e.stretches().insert(2, Fly { phase: Phase::Flying, stop: false, ended: false, space: 900, home: 0 });
        e.stretches().insert(3, Fly { phase: Phase::Flying, stop: true, ended: false, space: 901, home: 0 });
        assert_eq!(e.stretching(), [1, 2].into());
    }

    /// Live 11-15 16:34:28: a show stopped a stretch while its Space was being made; the
    /// stretch's thread then ended it with the lock still held by its own check and locked
    /// itself out for good. The check lets the lock go whatever it finds.
    #[test]
    fn a_stretch_stopped_while_its_space_is_made_lets_the_lock_go() {
        let e = engine();
        e.stretches().insert(7, Fly { phase: Phase::Waiting, stop: true, ended: false, space: 0, home: 0 });
        assert!(!e.claim_space(7, 900), "stopped meanwhile: skipped");
        assert!(e.stretches.try_lock().is_ok(), "the lock is free for the skip that follows");
        assert!(!e.claim_space(8, 901), "gone: skipped");
        assert!(e.stretches.try_lock().is_ok());
        e.stretches().insert(9, Fly { phase: Phase::Waiting, stop: false, ended: false, space: 0, home: 0 });
        assert!(e.claim_space(9, 902));
        assert_eq!(e.stretches()[&9].space, 902, "its Space noted");
        assert!(e.stretches.try_lock().is_ok());
    }

    /// A quit (a show of every window) while a stretch's thread is stuck holding the
    /// stretches: the shows and hides go on after a bounded wait, the next ones without
    /// waiting, and the stretches are asked again once free.
    #[test]
    fn a_quit_while_a_stretch_is_stuck_is_never_held_up_by_it() {
        let e = engine();
        e.stretches().insert(7, Fly { phase: Phase::Waiting, stop: false, ended: false, space: 0, home: 0 });
        let (held, release) = (std::sync::mpsc::channel::<()>(), std::sync::mpsc::channel::<()>());
        std::thread::scope(|scope| {
            let (held_tx, release_rx) = (held.0, release.1);
            let e = &e;
            scope.spawn(move || {
                let _stuck = e.stretches();
                held_tx.send(()).expect("told");
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
            });
            held.1.recv().expect("the stretch holds the lock");
            let began = Instant::now();
            let (guard, space) = e.stop_stretch(7, false);
            let first = began.elapsed();
            assert!(guard.is_none() && space.is_none(), "a show goes on as with no stretch");
            assert!(first >= LOCK_WAIT && first < LOCK_WAIT * 4, "bounded: {first:?}");
            let again = Instant::now();
            let (guard, space) = e.stop_stretch(7, true);
            assert!(guard.is_none());
            assert_eq!(space, Some(UNKNOWN_SPACE), "a hide takes the window out of every Space");
            assert!(!e.in_flight(7));
            assert!(e.stretches_within("a test").is_none());
            assert!(again.elapsed() < LOCK_WAIT, "no second wait: {:?}", again.elapsed());
            release.0.send(()).expect("released");
        });
        assert!(e.in_flight(7), "free again: asked as before");
        assert!(!e.stretches_busy.load(Ordering::Relaxed));
    }

    /// Review A-2: the unveil's show comes mid-flight and forgets the window as held; the
    /// Desktop it lands on (and a hide mid-flight takes it back to) is the one its stretch
    /// noted, not the first display's.
    #[test]
    fn a_flying_window_lands_on_the_desktop_it_was_hidden_from() {
        let e = engine();
        let fly = Fly { phase: Phase::Flying, stop: false, ended: false, space: 900, home: 7 };
        assert_eq!(e.home_of(5, Some(&fly)), Some(7), "no longer held: the stretch's Desktop");
        e.state().held.insert(5, super::super::Held { space: 836, home: 3 });
        assert_eq!(e.home_of(5, Some(&fly)), Some(3), "held (hidden again): where it was hidden from");
        assert_eq!(e.home_of(6, Some(&Fly { home: 0, ..fly })), None, "neither known");
        assert_eq!(e.home_of(6, None), None);
    }

    /// Review A-6: at the last step a window still in its landing Space goes home; one a hide
    /// took into its stage Space meanwhile (the stretches stuck, the stretch not stopped)
    /// does not.
    #[test]
    fn only_a_window_still_in_its_landing_space_goes_home() {
        assert!(lands_home(&[900], 900));
        assert!(!lands_home(&[836], 900), "hidden again in its stage Space: left there");
        assert!(!lands_home(&[], 900), "in no Space (closed): nothing to move");
    }

    /// Review A-10: a last stretch is flown only with a Space that can be shown and
    /// transformed.
    #[test]
    fn a_stretch_needs_a_space_shown_and_transformed() {
        assert!(can_land(true, true, false));
        assert!(can_land(true, false, true), "the bridged transform alone");
        assert!(!can_land(false, true, true), "no SLSShowSpaces: the window would wait unseen");
        assert!(!can_land(true, false, false), "no transform: it would fly at full size");
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
