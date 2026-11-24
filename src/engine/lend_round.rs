//! Mission Control's lends made a round at a time (round mc-lends, the Spaces engine).
//!
//! One window at a time, each parked and then put home before the next, the hidden stages'
//! windows came onto the Desktop one by one during Mission Control's opening, and it laid
//! them out again at each (a log, 11-19 07:42:13: 7 windows in 237 ms, of which 145 ms in
//! the parks, each waiting on its app in turn). Apple's Stage Manager has every window there
//! from the start. So a round has two phases ([`super::lend_park`] keeps each window's order):
//!
//! 1. **The parks** ([`by_app`], [`park_in_turn`]): every window of the round is moved to its
//!    park while it is still in its stage Space, where nothing shows. Different apps are
//!    asked at once (the platform gives each a thread of its own), one app's windows in
//!    order, all under one deadline: a window whose turn comes after it is not asked (it
//!    waits for the next round, hidden and untouched).
//! 2. **Home** ([`home_all`]): every window whose park took goes onto its Desktop in one
//!    window-server call per Desktop, then each is read back. Mission Control sees one
//!    change. A window whose park did not take, or that did not go home, stays hidden, its
//!    move undone there ([`lend_park::settle`]).
//!
//! The two may be apart: at Mission Control's key the parks are made, and the windows go
//! home once it shows ([`super::mission_control::Lends`]); when none shows, the parks are
//! undone unseen, the windows still in their stage Spaces.
//!
//! **Take backs** ([`back_all`]): every window back into its stage Space, one call per Space,
//! then the frames set again, the apps at once.

use super::geometry::Rect;
use super::lend_park::{self, BackDone, LendDone, Parked, Parking};
use super::model::Pid;
use std::time::Instant;

/// `items` (each with its app) grouped by app, the apps in the order they first come and
/// each app's items in theirs.
pub fn by_app<T>(items: impl IntoIterator<Item = (Pid, T)>) -> Vec<(Pid, Vec<T>)> {
    group(items)
}

/// `items` grouped by their key (a Desktop, a stage Space), in the order the keys first come.
pub fn group<K: PartialEq, T>(items: impl IntoIterator<Item = (K, T)>) -> Vec<(K, Vec<T>)> {
    let mut out: Vec<(K, Vec<T>)> = Vec::new();
    for (k, t) in items {
        match out.iter_mut().find(|(g, _)| *g == k) {
            Some((_, v)) => v.push(t),
            None => out.push((k, vec![t])),
        }
    }
    out
}

/// One app's parks in turn: `windows` in order, each `park`ed while `now()` is before
/// `deadline`; those after it are not asked (`None`).
pub fn park_in_turn<W: Copy>(windows: &[W], deadline: Instant, now: impl Fn() -> Instant, mut park: impl FnMut(W) -> Parking) -> Vec<(W, Option<Parking>)> {
    windows.iter().map(|w| (*w, (now() < deadline).then(|| park(*w)))).collect()
}

/// A window of a round's second phase: parked, and where it goes home.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HomeAsk {
    /// The window.
    pub id: u32,
    /// Its Desktop.
    pub desktop: u64,
    /// Its park.
    pub parking: Parking,
}

/// The window-server calls of a round's second phase, and the end of each lend.
pub trait HomeSteps {
    /// Windows `ids` out of their stage Spaces onto Desktop `desktop`, in one call.
    fn put_home(&mut self, desktop: u64, ids: &[u32]);
    /// True when window `id` is listed out of every stage Space (read back).
    fn is_home(&mut self, id: u32) -> bool;
    /// Window `id`'s lend ended ([`lend_park::settle`]).
    fn settle(&mut self, id: u32, p: Parking, home: bool) -> LendDone;
}

/// What the second phase did: each window's lend (in the order asked), and when the first
/// call to put windows home was made and the last one returned.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeDone {
    /// Each window and its lend.
    pub lends: Vec<(u32, LendDone)>,
    /// The first call's start and the last call's end (`None`: no call made).
    pub calls: Option<(Instant, Instant)>,
}

/// The second phase: every window whose park took goes home, one call per Desktop, then each
/// is read back and its lend ended. A window whose park did not take is never put home.
pub fn home_all(steps: &mut impl HomeSteps, asks: &[HomeAsk]) -> HomeDone {
    let groups = group(asks.iter().filter(|a| a.parking.goes_home()).map(|a| (a.desktop, a.id)));
    let mut calls = None;
    for (desktop, ids) in &groups {
        let start = Instant::now();
        steps.put_home(*desktop, ids);
        let end = Instant::now();
        calls = Some(calls.map_or((start, end), |(s, _)| (s, end)));
    }
    let lends = asks
        .iter()
        .map(|a| {
            let home = a.parking.goes_home() && steps.is_home(a.id);
            (a.id, steps.settle(a.id, a.parking, home))
        })
        .collect();
    HomeDone { lends, calls }
}

/// Why a hidden window of a round was not put home (it stays hidden, in its stage Space): what
/// the log says of each, and what decides whether it is tried again
/// ([`super::mission_control::retry`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Left {
    /// The engine does not hold it in a stage Space of a Desktop shown (shown since, gone, or
    /// its Desktop not shown).
    NotHeld,
    /// Lent already (a Dock press or ⌘Tab's switcher has it), or flying a switch's last stretch.
    Busy,
    /// No park was made for it (its element missing, or the round's parks refused).
    NotParked,
    /// Its park did not take: `listed` where the window server listed it when the round went
    /// home (`None`: not listed), `moved` when that is away from its frame (out of the park
    /// zone all the same: in view, were it put home).
    ParkMissed {
        /// Where it was listed.
        listed: Option<Rect>,
        /// Away from its frame.
        moved: bool,
    },
    /// Parked and put home, and still listed in a stage Space (the window server's call did
    /// not take it out): its move undone there.
    NotHome,
}

impl Left {
    /// Its name for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::NotHeld => "not held in a stage Space of a Desktop shown",
            Self::Busy => "lent already, or in a switch",
            Self::NotParked => "no park made",
            Self::ParkMissed { moved: false, .. } => "its park did not take: listed at its frame",
            Self::ParkMissed { moved: true, .. } => "its park did not take: listed away from its frame, out of the park zone",
            Self::NotHome => "parked, and the window server did not take it out of its stage Space",
        }
    }
}

/// Why the window of `ask` was left hidden by its lend `done` (`None`: it went home).
pub fn left_why(ask: &HomeAsk, done: &LendDone) -> Option<Left> {
    if done.home {
        None
    } else if ask.parking.goes_home() {
        Some(Left::NotHome)
    } else {
        Some(Left::ParkMissed { listed: ask.parking.landed, moved: ask.parking.moved })
    }
}

/// A window put home that goes back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackAsk {
    /// The window.
    pub id: u32,
    /// Its app.
    pub pid: Pid,
    /// The stage Space it goes back into.
    pub space: u64,
    /// Its park, when it was parked: its frame is set again once it is back.
    pub parked: Option<Parked>,
}

/// The calls of the take backs.
pub trait BackSteps {
    /// Where the window server lists window `id` now.
    fn listed(&mut self, id: u32) -> Option<Rect>;
    /// Windows `ids` into stage Space `space`, in one call.
    fn put_back(&mut self, space: u64, ids: &[u32]);
    /// True when window `id` is listed in `space` and nowhere else (read back).
    fn is_back(&mut self, id: u32, space: u64) -> bool;
    /// The frames of `apps`' windows set again ([`lend_park::restore`]), the apps at once,
    /// one app's windows in order: whether each took it.
    fn restore_apps(&mut self, apps: &[(Pid, Vec<(u32, Parked)>)]) -> Vec<(u32, bool)>;
}

/// The take backs: every window into its stage Space, one call per Space; then the frames of
/// those back and parked set again, the apps at once ([`lend_park::take_back`] for one: a
/// window that did not go back keeps its park, restoring it on the Desktop would show it).
pub fn back_all(steps: &mut impl BackSteps, asks: &[BackAsk]) -> Vec<(u32, BackDone)> {
    let moved_off: Vec<bool> = asks
        .iter()
        .map(|a| {
            let listed = steps.listed(a.id);
            lend_park::moved_off(&mut Listed(listed), a.parked)
        })
        .collect();
    for (space, ids) in group(asks.iter().map(|a| (a.space, a.id))) {
        steps.put_back(space, &ids);
    }
    let back: Vec<bool> = asks.iter().map(|a| steps.is_back(a.id, a.space)).collect();
    let apps = by_app(asks.iter().zip(&back).filter(|(_, b)| **b).filter_map(|(a, _)| Some((a.pid, (a.id, a.parked?)))));
    let restored = steps.restore_apps(&apps);
    asks.iter()
        .zip(back)
        .zip(moved_off)
        .map(|((a, back), moved_off)| {
            let restored = a.parked.map(|_| back && restored.iter().any(|(id, ok)| *id == a.id && *ok));
            (a.id, BackDone { back, restored, moved_off })
        })
        .collect()
}

/// A listing read already, as [`lend_park::Moves`] (no move is made through it).
struct Listed(Option<Rect>);

impl lend_park::Moves for Listed {
    fn listed(&mut self) -> Option<Rect> {
        self.0
    }
    fn move_to(&mut self, _to: Rect) -> Option<Rect> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::corner::park::ParkRequest;
    use crate::engine::corner::park_frame_of;
    use crate::engine::lend_park::{Moves, ParkAsk, Zone, park, settle};
    use crate::engine::model::Display;
    use crate::engine::testkit::display;
    use std::collections::BTreeMap;
    use std::time::Duration;

    /// The window server and the apps as a round's calls see them, and every call in order.
    #[derive(Default)]
    struct Mac {
        /// Each window: where it is listed, in a stage Space or not.
        wins: BTreeMap<u32, (Rect, bool)>,
        /// Apps that do not move their windows.
        stuck: Vec<u32>,
        /// Windows the window server will not move out of (or back into) their Space.
        refused: Vec<u32>,
        calls: Vec<String>,
        /// Frames listed on the Desktop while a round ran (what anyone could see).
        seen: Vec<(u32, Rect)>,
    }

    impl Mac {
        fn look(&mut self) {
            let shown: Vec<(u32, Rect)> = self.wins.iter().filter(|(_, (_, staged))| !staged).map(|(id, (r, _))| (*id, *r)).collect();
            self.seen.extend(shown);
        }
    }

    /// One window's moves on the fake Mac.
    struct Win<'a>(&'a mut Mac, u32);

    impl Moves for Win<'_> {
        fn listed(&mut self) -> Option<Rect> {
            self.0.wins.get(&self.1).map(|w| w.0)
        }
        fn move_to(&mut self, to: Rect) -> Option<Rect> {
            self.0.calls.push(format!("move {}", self.1));
            if !self.0.stuck.contains(&self.1)
                && let Some(w) = self.0.wins.get_mut(&self.1)
            {
                w.0 = Rect { w: w.0.w, h: w.0.h, ..to };
            }
            self.0.look();
            self.listed()
        }
    }

    impl HomeSteps for Mac {
        fn put_home(&mut self, desktop: u64, ids: &[u32]) {
            self.calls.push(format!("home {desktop} {ids:?}"));
            for id in ids {
                if !self.refused.contains(id)
                    && let Some(w) = self.wins.get_mut(id)
                {
                    w.1 = false;
                }
            }
            self.look();
        }
        fn is_home(&mut self, id: u32) -> bool {
            self.wins.get(&id).is_some_and(|w| !w.1)
        }
        fn settle(&mut self, id: u32, p: Parking, home: bool) -> LendDone {
            settle(&mut Win(self, id), p, home)
        }
    }

    impl BackSteps for Mac {
        fn listed(&mut self, id: u32) -> Option<Rect> {
            self.wins.get(&id).map(|w| w.0)
        }
        fn put_back(&mut self, space: u64, ids: &[u32]) {
            self.calls.push(format!("back {space} {ids:?}"));
            for id in ids {
                if !self.refused.contains(id)
                    && let Some(w) = self.wins.get_mut(id)
                {
                    w.1 = true;
                }
            }
        }
        fn is_back(&mut self, id: u32, _space: u64) -> bool {
            self.wins.get(&id).is_some_and(|w| w.1)
        }
        fn restore_apps(&mut self, apps: &[(Pid, Vec<(u32, Parked)>)]) -> Vec<(u32, bool)> {
            apps.iter().flat_map(|(_, ws)| ws.iter().copied()).map(|(id, p)| (id, lend_park::restore(&mut Win(self, id), p))).collect()
        }
    }

    fn frame(id: u32) -> Rect {
        Rect::new(100.0 + 40.0 * f64::from(id), 120.0, 900.0, 600.0)
    }

    fn ask(d: Display, id: u32) -> ParkAsk {
        ParkAsk { at: park_frame_of(frame(id), &d, ParkRequest::default()), zone: Zone::Corner, display: d, frame: frame(id), must: false, kept: false }
    }

    /// Windows 1 to 4 hidden at their frames; 1 and 2 of one app, 3 and 4 of two others.
    fn mac() -> Mac {
        Mac { wins: (1..=4).map(|id| (id, (frame(id), true))).collect(), ..Mac::default() }
    }

    fn pid_of(id: u32) -> Pid {
        if id <= 2 { 10 } else { 10 * id as Pid }
    }

    /// The whole round as the platform runs it: the parks app by app, then home.
    fn round(mac: &mut Mac, ids: &[u32], desktop: impl Fn(u32) -> u64) -> HomeDone {
        let d = display();
        let far = Instant::now() + Duration::from_secs(60);
        let mut asks = Vec::new();
        for (_, ws) in by_app(ids.iter().map(|id| (pid_of(*id), *id))) {
            for (id, p) in park_in_turn(&ws, far, Instant::now, |id| park(&mut Win(mac, id), ask(d, id))) {
                asks.push(HomeAsk { id, desktop: desktop(id), parking: p.expect("asked before the deadline") });
            }
        }
        home_all(mac, &asks)
    }

    #[test]
    fn apps_and_spaces_group_in_the_order_they_first_come() {
        assert_eq!(by_app([(10, 1), (20, 3), (10, 2), (30, 4)]), vec![(10, vec![1, 2]), (20, vec![3]), (30, vec![4])]);
        assert_eq!(group([(7u64, 1), (7, 2)]), vec![(7, vec![1, 2])]);
        assert!(by_app(Vec::<(Pid, u32)>::new()).is_empty());
    }

    /// Every park comes before any window goes home, and all of a Desktop's go home in one
    /// call: nothing is seen at its frame, and Mission Control sees one change.
    #[test]
    fn a_round_parks_every_window_first_then_puts_them_home_in_one_call() {
        let mut mac = mac();
        let done = round(&mut mac, &[3, 1, 4, 2], |_| 5);
        // Apps in the order they first come (3's, then 1's and 2's, then 4's), each app's
        // windows in order: the platform asks the apps at once.
        assert_eq!(mac.calls, ["move 3", "move 1", "move 2", "move 4", "home 5 [3, 1, 2, 4]"].map(String::from));
        assert!(done.lends.iter().all(|(_, l)| l.home && l.parked == Some(true)), "{done:?}");
        assert!(done.calls.is_some());
        let d = display();
        assert!(mac.seen.iter().all(|(_, r)| d.parks(*r)), "only ever seen parked: {:?}", mac.seen);
    }

    /// Each window left hidden says why: its park missed (where it was listed, at its frame or
    /// away from it), or the window server did not take it home.
    #[test]
    fn a_window_left_hidden_says_why() {
        let mut mac = mac();
        mac.stuck = vec![2];
        mac.refused = vec![3];
        let d = display();
        let asks: Vec<HomeAsk> = [1, 2, 3]
            .into_iter()
            .map(|id| HomeAsk { id, desktop: 5, parking: park(&mut Win(&mut mac, id), ask(d, id)) })
            .collect();
        let done = home_all(&mut mac, &asks);
        let why: Vec<Option<Left>> = asks.iter().zip(&done.lends).map(|(a, (_, l))| left_why(a, l)).collect();
        assert_eq!(why, [None, Some(Left::ParkMissed { listed: Some(frame(2)), moved: false }), Some(Left::NotHome)]);
        let off = Rect::new(500.0, 300.0, 900.0, 600.0);
        let away = HomeAsk { id: 4, desktop: 5, parking: Parking::now(asks[1].parking.parked, Some(off)) };
        let not = LendDone { home: false, parked: Some(false), owed: None };
        assert_eq!(left_why(&away, &not), Some(Left::ParkMissed { listed: Some(off), moved: true }));
        assert_eq!(left_why(&HomeAsk { parking: Parking::now(asks[1].parking.parked, None), ..away }, &not), Some(Left::ParkMissed { listed: None, moved: false }), "not listed");
        assert_ne!(Left::ParkMissed { listed: None, moved: true }.name(), Left::ParkMissed { listed: None, moved: false }.name());
    }

    /// Windows of two Desktops: one call each, back to back after every park.
    #[test]
    fn one_call_per_desktop() {
        let mut mac = mac();
        round(&mut mac, &[1, 2, 3], |id| if id == 3 { 6 } else { 5 });
        assert_eq!(mac.calls[3..], ["home 5 [1, 2]".to_string(), "home 6 [3]".to_string()]);
    }

    /// A window whose app did not move it is never put home (it would show at its frame); one
    /// the window server would not move stays hidden and gets its frame back there, unseen.
    #[test]
    fn a_park_that_did_not_take_or_a_window_that_did_not_go_home_stays_hidden() {
        let mut mac = mac();
        mac.stuck = vec![2];
        mac.refused = vec![3];
        let done = round(&mut mac, &[1, 2, 3], |_| 5);
        assert_eq!(mac.calls.iter().filter(|c| c.starts_with("home")).collect::<Vec<_>>(), ["home 5 [1, 3]"], "the stuck one is not sent");
        let lend = |id: u32| done.lends.iter().find(|(i, _)| *i == id).map(|(_, l)| *l).expect("asked");
        assert!(lend(1).home);
        assert_eq!((lend(2).home, lend(2).parked), (false, Some(false)));
        assert_eq!((lend(3).home, lend(3).owed), (false, None), "its move undone, seen back at its frame");
        assert!(mac.wins[&2].1 && mac.wins[&3].1, "both still in their stage Spaces");
        assert_eq!(mac.wins[&3].0, frame(3));
        let d = display();
        assert!(mac.seen.iter().all(|(_, r)| d.parks(*r)), "only ever seen parked: {:?}", mac.seen);
    }

    /// The deadline: an app's windows whose turn comes after it are not asked.
    #[test]
    fn windows_after_the_deadline_are_not_asked() {
        let t = Instant::now();
        let clock = std::cell::Cell::new(t);
        let parks = park_in_turn(&[1u32, 2, 3], t + Duration::from_millis(50), || clock.get(), |id| {
            clock.set(clock.get() + Duration::from_millis(30));
            let d = display();
            Parking { parked: Parked { at: ask(d, id).at, zone: Zone::Corner, frame: frame(id), display: d }, took: true, moved: true, must: false, landed: Some(ask(d, id).at) }
        });
        assert_eq!(parks.iter().map(|(id, p)| (*id, p.is_some())).collect::<Vec<_>>(), [(1, true), (2, true), (3, false)]);
    }

    /// The take backs: every window into its stage Space first (one call per Space), then the
    /// frames; one that did not go back keeps its park.
    #[test]
    fn take_backs_go_back_first_one_call_per_space_then_the_frames() {
        let mut mac = mac();
        round(&mut mac, &[1, 2, 3, 4], |_| 5);
        mac.calls.clear();
        mac.seen.clear();
        mac.refused = vec![4];
        let d = display();
        let parked = |id: u32| Some(Parked { at: ask(d, id).at, zone: Zone::Corner, frame: frame(id), display: d });
        let asks: Vec<BackAsk> = [(1, 70), (2, 70), (3, 71), (4, 72)].map(|(id, space)| BackAsk { id, pid: pid_of(id), space, parked: parked(id) }).to_vec();
        let done = back_all(&mut mac, &asks);
        assert_eq!(mac.calls[..3], ["back 70 [1, 2]".to_string(), "back 71 [3]".to_string(), "back 72 [4]".to_string()]);
        assert!(mac.calls[3..].iter().all(|c| c.starts_with("move")), "the frames after: {:?}", mac.calls);
        let of = |id: u32| done.iter().find(|(i, _)| *i == id).map(|(_, b)| *b).expect("asked");
        for id in 1..=3 {
            assert_eq!(of(id), BackDone { back: true, restored: Some(true), moved_off: false });
            assert_eq!(mac.wins[&id], (frame(id), true), "window {id} hidden at its frame");
        }
        assert_eq!(of(4), BackDone { back: false, restored: Some(false), moved_off: false }, "not back: its park kept");
        assert!(d.parks(mac.wins[&4].0));
        assert!(mac.seen.iter().all(|(_, r)| d.parks(*r)), "never seen at a frame: {:?}", mac.seen);
    }

    /// A window parked earlier (at Mission Control's key) is judged by where it is listed when
    /// it goes home: an app that moved it off the park since keeps it hidden.
    #[test]
    fn a_park_made_earlier_is_judged_as_listed_at_home_time() {
        let d = display();
        let p = Parked { at: ask(d, 1).at, zone: Zone::Corner, frame: frame(1), display: d };
        assert!(Parking::now(p, Some(p.at)).took);
        let moved = Parking::now(p, Some(Rect::new(400.0, 300.0, 900.0, 600.0)));
        assert!(!moved.took && moved.moved && !moved.goes_home());
        let back = Parking::now(p, Some(frame(1)));
        assert!(!back.took && !back.moved);
        assert!(!Parking::now(p, None).took);
    }
}
