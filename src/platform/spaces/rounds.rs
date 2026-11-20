//! Mission Control's lends a round at a time (round mc-lends): the parks of every window
//! first, the apps at once, then all of them home in one window-server call per Desktop;
//! the take backs the same way round. The order is [`crate::engine::lend_round`]'s.
//!
//! **Threads.** A park is one bounded move through the window's app and its read back
//! ([`Mover`]: Accessibility and the window list, no window-server connection), so each app's
//! windows are asked on a thread of its own, as the launch's scan asks apps that do not
//! answer, and one busy app holds up no other. Each thread opens its own
//! [`enhanced_ui::Batch`]: the app's flag is off once for all its moves and on again before
//! the thread ends. The WM thread waits for every thread (each window bounded by its
//! park's timeout, one deadline for the round's turns), then makes the window-server calls
//! and keeps the state itself: the engine's maps are written on the WM thread only, after
//! the threads are joined, so the parks' move notices, which the observer hears on the WM
//! thread afterwards, find each window counted parked ([`WindowEngine::parked_lend`]).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::super::ffi::{ALL_SPACES_MASK, Route, Sky};
use super::{Call, Mover, PARK_AX_SECONDS, ParkedLend, SpacesEngine, hidden_in, out_of};
use crate::engine::geometry::Rect;
use crate::engine::lend_park::{self, LendDone, Parked, Parking, Zone};
use crate::engine::lend_round::{self, BackAsk, BackSteps, HomeAsk, HomeSteps, Left};
use crate::engine::model::WindowId;
use crate::platform::ax::AxElement;
use crate::platform::enhanced_ui;
use crate::platform::safety;
use crate::platform::window_engine::{Back, Homes, Parks, RoundAsk};
use crate::platform::windows;

/// Milliseconds of `d`.
fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// Runs `each` for every app's share of `apps`, the apps at once: a thread each (none for a
/// single app, nor when the system makes none: then on this thread), each inside an
/// [`enhanced_ui::Batch`] of its own. One answer per app, in order; `None` for an app whose
/// thread panicked.
fn per_app<W: Sync, T: Send>(apps: &[(i32, Vec<W>)], each: impl Fn(&[W]) -> T + Sync) -> Vec<Option<T>> {
    let run = |ws: &[W]| {
        let _batch = enhanced_ui::Batch::open();
        each(ws)
    };
    if apps.len() <= 1 {
        return apps.iter().map(|(_, ws)| Some(run(ws))).collect();
    }
    std::thread::scope(|scope| {
        let started: Vec<Result<_, &[W]>> = apps
            .iter()
            .map(|(pid, ws)| std::thread::Builder::new().name(format!("mc-park-{pid}")).spawn_scoped(scope, || run(ws)).map_err(|_| ws.as_slice()))
            .collect();
        started
            .into_iter()
            .map(|s| match s {
                Ok(handle) => handle.join().ok(),
                Err(ws) => {
                    tracing::debug!("spaces engine: no thread for an app's parks: asked here");
                    Some(run(ws))
                }
            })
            .collect()
    })
}

/// A window whose frame is set again: its id, its park, its element.
type Unpark = (u32, Parked, AxElement);

/// The frames of `apps`' windows set again ([`lend_park::restore`]), the apps at once: whether
/// each took it.
fn restore_apps(apps: &[(i32, Vec<Unpark>)]) -> Vec<(u32, bool)> {
    let answers = per_app(apps, |ws| ws.iter().map(|(id, p, e)| (*id, lend_park::restore(&mut Mover::restore(*id, e), *p))).collect::<Vec<_>>());
    apps.iter()
        .zip(answers)
        .flat_map(|((_, ws), a)| a.unwrap_or_else(|| ws.iter().map(|(id, _, _)| (*id, false)).collect()))
        .collect()
}

/// How long a pick's move waits for the window's element to report its frame
/// ([`AxMover`]).
const PICK_READ_BACK: Duration = Duration::from_millis(60);

/// The pause between two reads of a pick's move.
const PICK_READ_STEP: Duration = Duration::from_millis(5);

/// A window's moves through its element, read back there too (Accessibility): while Mission
/// Control shows or closes, the window list gives where its layout draws the window, never
/// its frame (round mc-pick).
struct AxMover<'a> {
    element: &'a AxElement,
}

impl lend_park::Moves for AxMover<'_> {
    fn listed(&mut self) -> Option<Rect> {
        self.element.bounded(PARK_AX_SECONDS, |e| e.frame()).ok()
    }

    fn move_to(&mut self, to: Rect) -> Option<Rect> {
        self.element.bounded(PARK_AX_SECONDS, |e| e.set_position(to.origin())).ok()?;
        let by = Instant::now() + PICK_READ_BACK;
        loop {
            let now = self.listed();
            if now.is_some_and(|r| lend_park::at_origin(r, to)) || Instant::now() >= by {
                return now;
            }
            std::thread::sleep(PICK_READ_STEP);
        }
    }
}

/// The second phase's calls ([`HomeSteps`]).
struct HomeMac<'a> {
    engine: &'a SpacesEngine,
    sky: &'a Sky,
    ours: &'a std::collections::BTreeSet<u64>,
    elements: &'a BTreeMap<u32, AxElement>,
}

impl HomeSteps for HomeMac<'_> {
    fn put_home(&mut self, desktop: u64, ids: &[u32]) {
        if let Err(e) = self.sky.add_remove_from(Route::Bridged, desktop, ids, ALL_SPACES_MASK) {
            tracing::warn!(?ids, desktop, "spaces engine: Mission Control's windows could not be put home: {e}");
        }
    }

    fn is_home(&mut self, id: u32) -> bool {
        let ours = self.ours;
        let (ok, after) = self.engine.read_back(self.sky, id, |s| out_of(s, ours));
        if !ok {
            tracing::warn!(id, listed = ?after, "spaces engine: a window put home is still in a stage Space");
        }
        ok
    }

    fn settle(&mut self, id: u32, p: Parking, home: bool) -> LendDone {
        match self.elements.get(&id) {
            Some(e) => lend_park::settle(&mut Mover::park(id, e, PARK_AX_SECONDS), p, home),
            None => LendDone { home, parked: Some(p.took), owed: Some(p.parked) },
        }
    }
}

/// The take backs' calls ([`BackSteps`]).
struct BackMac<'a> {
    engine: &'a SpacesEngine,
    sky: &'a Sky,
    elements: &'a BTreeMap<u32, AxElement>,
}

impl BackSteps for BackMac<'_> {
    fn listed(&mut self, id: u32) -> Option<Rect> {
        windows::describe(&[id]).first().map(|w| w.bounds)
    }

    fn put_back(&mut self, space: u64, ids: &[u32]) {
        if !self.engine.blocked()
            && let Err(e) = self.sky.add_remove(Route::Bridged, space, ids)
        {
            tracing::warn!(?ids, space, "spaces engine: windows put home could not be taken back: {e}");
        }
    }

    fn is_back(&mut self, id: u32, space: u64) -> bool {
        let (ok, after) = self.engine.read_back(self.sky, id, |s| hidden_in(s, space));
        if !ok {
            tracing::warn!(id, space, listed = ?after, "spaces engine: a window put home did not go back into its stage Space");
        }
        ok
    }

    fn restore_apps(&mut self, apps: &[(i32, Vec<(u32, Parked)>)]) -> Vec<(u32, bool)> {
        let with: Vec<(i32, Vec<Unpark>)> = apps
            .iter()
            .map(|(pid, ws)| (*pid, ws.iter().filter_map(|(id, p)| Some((*id, *p, self.elements.get(id)?.clone()))).collect()))
            .collect();
        restore_apps(&with)
    }
}

impl SpacesEngine {
    /// [`crate::platform::window_engine::WindowEngine::park_round`].
    pub(super) fn park_round_now(&self, asks: Vec<RoundAsk>, deadline: Instant) -> Parks {
        let began = Instant::now();
        let mut parks = Parks::default();
        let Some(sky) = self.sky() else { return parks };
        if self.blocked() {
            return parks;
        }
        let Some(stretching) = self.stretches_within("a round of lends").map(|l| l.keys().copied().collect::<Vec<u32>>()) else { return parks };
        let (desktops, _) = Self::desktops(sky);
        // A test's "park" failure: the apps do not move their windows (nothing is asked).
        let refused = self.failing(Call::Park);
        let asks: Vec<RoundAsk> = {
            let st = self.state();
            asks.into_iter()
                .filter(|a| {
                    let why = if stretching.contains(&a.id.0) || st.lent.contains(&a.id.0) {
                        Some(Left::Busy)
                    } else if !st.held.get(&a.id.0).is_some_and(|h| desktops.contains(&h.home)) {
                        Some(Left::NotHeld)
                    } else {
                        None
                    };
                    if let Some(why) = why {
                        parks.skipped.push((a.id, why));
                    }
                    why.is_none()
                })
                .map(|mut a| {
                    // Parked already (a take back whose restore is owed): its frame is the
                    // one kept then.
                    if let Some(p) = st.parked.get(&a.id.0) {
                        a.park.ask.frame = p.park.frame;
                        a.park.ask.kept = true;
                    }
                    a
                })
                .collect()
        };
        if refused {
            parks.missed = asks.iter().map(|a| a.id).collect();
            return parks;
        }
        let apps = lend_round::by_app(asks.iter().map(|a| (a.pid, a)));
        parks.apps = apps.len();
        let answers = per_app(&apps, |ws| {
            let mut sum = Duration::ZERO;
            let parks = lend_round::park_in_turn(ws, deadline, Instant::now, |a| {
                let mut mover = Mover::park(a.id.0, &a.park.window, a.park.ax_seconds.unwrap_or(PARK_AX_SECONDS));
                let p = lend_park::park(&mut mover, a.park.ask);
                sum += mover.moves;
                p
            });
            (parks.into_iter().map(|(a, p)| (a.id, p)).collect::<Vec<_>>(), sum)
        });
        let mut st = self.state();
        for ((_, ws), answer) in apps.iter().zip(answers) {
            let Some((done, sum)) = answer else {
                // Its thread panicked mid-park: a move may have been sent, so each restore is
                // owed; judged as listed when the round goes home.
                for a in ws {
                    let park = Parked { at: a.park.ask.at, zone: a.park.ask.zone, frame: a.park.ask.frame, display: a.park.ask.display };
                    st.parked.insert(a.id.0, ParkedLend { park, element: a.park.window.clone() });
                    parks.missed.push(a.id);
                }
                continue;
            };
            parks.sum_ms += ms(sum);
            for (id, p) in done {
                let Some(p) = p else {
                    parks.late.push(id);
                    continue;
                };
                if let Some(a) = ws.iter().find(|a| a.id == id) {
                    st.parked.insert(id.0, ParkedLend { park: p.parked, element: a.park.window.clone() });
                }
                parks.landed.push((id, p.landed));
                if p.took { parks.took.push(id) } else { parks.missed.push(id) }
            }
        }
        drop(st);
        self.lends_to_file();
        parks.ms = ms(began.elapsed());
        parks
    }

    /// [`crate::platform::window_engine::WindowEngine::home_round`].
    pub(super) fn home_round_now(&self, ids: &[WindowId]) -> Homes {
        let began = Instant::now();
        let mut homes = Homes::default();
        let Some(sky) = self.sky() else { return homes };
        if self.blocked() {
            homes.left = ids.iter().map(|w| (*w, Left::NotHome)).collect();
            return homes;
        }
        let stretching: Vec<u32> = self.stretches_within("a round of lends").map(|l| l.keys().copied().collect()).unwrap_or_default();
        let (desktops, _) = Self::desktops(sky);
        let wanted: Vec<u32> = ids.iter().map(|w| w.0).collect();
        let listed: BTreeMap<u32, Rect> = windows::describe(&wanted).into_iter().map(|w| (w.id, w.bounds)).collect();
        let mut elements = BTreeMap::new();
        let mut why: BTreeMap<u32, Left> = BTreeMap::new();
        let asks: Vec<HomeAsk> = {
            let st = self.state();
            wanted
                .iter()
                .filter_map(|id| {
                    let Some(held) = st.held.get(id).filter(|h| desktops.contains(&h.home)) else {
                        why.insert(*id, Left::NotHeld);
                        return None;
                    };
                    if st.lent.contains(id) || stretching.contains(id) {
                        why.insert(*id, Left::Busy);
                        return None;
                    }
                    let Some(p) = st.parked.get(id) else {
                        why.insert(*id, Left::NotParked);
                        return None;
                    };
                    elements.insert(*id, p.element.clone());
                    Some(HomeAsk { id: *id, desktop: held.home, parking: Parking::now(p.park, listed.get(id).copied()) })
                })
                .collect()
        };
        let ours = self.ours();
        let done = lend_round::home_all(&mut HomeMac { engine: self, sky, ours: &ours, elements: &elements }, &asks);
        for (a, (_, l)) in asks.iter().zip(&done.lends) {
            if let Some(w) = lend_round::left_why(a, l) {
                why.insert(a.id, w);
            }
        }
        let lent: Vec<u32> = done.lends.iter().filter(|(_, l)| l.home).map(|(id, _)| *id).collect();
        let at: BTreeMap<u32, Rect> = windows::describe(&lent).into_iter().map(|w| (w.id, w.bounds)).collect();
        let at_card: Vec<u32> = asks.iter().filter(|a| a.parking.parked.zone == Zone::Card).map(|a| a.id).collect();
        {
            let mut st = self.state();
            for (id, l) in &done.lends {
                if l.home {
                    st.lent.insert(*id);
                    st.counts.lent += 1;
                }
                if l.owed.is_none() {
                    st.parked.remove(id);
                }
            }
        }
        for w in ids {
            if lent.contains(&w.0) {
                homes.lent.push((*w, at.get(&w.0).copied(), at_card.contains(&w.0)));
            } else if !self.lent(w.0) {
                homes.left.push((*w, why.get(&w.0).copied().unwrap_or(Left::NotHome)));
            }
        }
        self.lends_to_file();
        homes.calls = done.calls;
        homes.ms = ms(began.elapsed());
        homes
    }

    /// [`crate::platform::window_engine::WindowEngine::back_round`].
    pub(super) fn back_round_now(&self, ids: &[(WindowId, i32)]) -> Vec<(WindowId, Back)> {
        let Some(sky) = self.sky() else { return ids.iter().map(|(id, _)| (*id, Back::default())).collect() };
        let mut elements = BTreeMap::new();
        let asks: Vec<BackAsk> = {
            let mut st = self.state();
            ids.iter()
                .filter_map(|(id, pid)| {
                    if !st.lent.remove(&id.0) {
                        return None;
                    }
                    let held = st.held.get(&id.0).copied()?;
                    let parked = st.parked.get(&id.0).map(|p| {
                        elements.insert(id.0, p.element.clone());
                        p.park
                    });
                    Some(BackAsk { id: id.0, pid: *pid, space: held.space, parked })
                })
                .collect()
        };
        let done = lend_round::back_all(&mut BackMac { engine: self, sky, elements: &elements }, &asks);
        let mut st = self.state();
        let mut out: Vec<(WindowId, Back)> = ids.iter().map(|(id, _)| (*id, Back::default())).collect();
        for (id, b) in done {
            if b.moved_off {
                tracing::info!(id, "spaces engine: a window parked for a lend was moved off the park by its app");
            }
            if b.back {
                st.counts.taken_back += 1;
            } else {
                st.held.remove(&id);
            }
            if b.restored == Some(true) {
                st.parked.remove(&id);
            }
            if let Some(o) = out.iter_mut().find(|(w, _)| w.0 == id) {
                o.1 = Back { back: b.back, restored: b.restored };
            }
        }
        drop(st);
        self.lends_to_file();
        out
    }

    /// [`crate::platform::window_engine::WindowEngine::frames_back`]: the parks of windows
    /// lent (put home) undone through their elements, the apps at once; a window that took it
    /// is parked no more (its move notices are the model's again), and stays lent.
    pub(super) fn frames_back_now(&self, ids: &[(WindowId, i32)]) -> Vec<WindowId> {
        let parked: Vec<(i32, Unpark)> = {
            let st = self.state();
            ids.iter()
                .filter(|(id, _)| st.lent.contains(&id.0))
                .filter_map(|(id, pid)| st.parked.get(&id.0).map(|p| (*pid, (id.0, p.park, p.element.clone()))))
                .collect()
        };
        let apps = lend_round::by_app(parked);
        let answers = per_app(&apps, |ws| ws.iter().map(|(id, p, e)| (*id, lend_park::restore(&mut AxMover { element: e }, *p))).collect::<Vec<_>>());
        let mut st = self.state();
        let mut set = Vec::new();
        for ((_, ws), answer) in apps.iter().zip(answers) {
            for (id, ok) in answer.unwrap_or_else(|| ws.iter().map(|(id, _, _)| (*id, false)).collect()) {
                if !ok {
                    tracing::info!(id, "spaces engine: a picked window did not take its frame back: it comes with its switch");
                } else if st.lent.contains(&id) {
                    st.parked.remove(&id);
                    set.push(WindowId(id));
                }
            }
        }
        drop(st);
        self.lends_to_file();
        set
    }

    /// [`crate::platform::window_engine::WindowEngine::unpark_round`].
    pub(super) fn unpark_round_now(&self, ids: &[(WindowId, i32)]) -> usize {
        let parked: Vec<(i32, Unpark)> = {
            let st = self.state();
            ids.iter()
                .filter(|(id, _)| !st.lent.contains(&id.0))
                .filter_map(|(id, pid)| st.parked.get(&id.0).map(|p| (*pid, (id.0, p.park, p.element.clone()))))
                .collect()
        };
        let restored = restore_apps(&lend_round::by_app(parked));
        let mut st = self.state();
        let mut n = 0;
        for (id, ok) in restored {
            if ok {
                st.parked.remove(&id);
                n += 1;
            }
        }
        drop(st);
        self.lends_to_file();
        n
    }

    /// The card parks of the windows parked for a lend now ([`lend_park::Zone::Card`]): what
    /// the parked file names for the guardian ([`crate::engine::recovery::ParkedFile::lends`]).
    pub(super) fn card_parks(&self) -> Vec<Rect> {
        self.state().parked.values().filter(|p| p.park.zone == Zone::Card).map(|p| p.park.at).collect()
    }

    /// The parked file written again when the card parks changed ([`Self::card_parks`]): after
    /// a kill -9 with windows out at their cards, the guardian finds them there. The file is
    /// rewritten only when it would differ.
    fn lends_to_file(&self) {
        let lends = self.card_parks();
        if safety::set_lends(lends) {
            safety::rewrite_parked_file();
        }
    }
}
