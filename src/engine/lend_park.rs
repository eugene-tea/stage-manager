//! ⌘Tab's lend parked off-screen (round cmdtab-park, the Spaces engine).
//!
//! While ⌘Tab's switcher selects an app whose windows here all wait in stage Spaces (and
//! which has a window on another Desktop), its stage's key window is put on this Desktop
//! ("lent", [`crate::engine::switcher`]) so that macOS activates the app here. At its full
//! frame it showed under the front stage's windows, and browsing the switcher made a big
//! window blink in and out (the owner, live 11-15). macOS counts a window on a Desktop
//! whatever its position, so the lend works as well with the window parked at the display's
//! corner as the Corner engine parks a hidden one ([`super::corner::park_frame_of`]: same
//! size, at most a sliver showing).
//!
//! The order is what keeps it unseen:
//!
//! - **Lend** ([`lend`]): the window is moved to its park frame while it is still in its
//!   stage Space (invisible), read back from the window server, then put on its Desktop. A
//!   park that did not take (the app refused or answered late) still lends when the lend is
//!   needed now ([`ParkAsk::must`]: the first Tab, a quick ⌘Tab's only chance); otherwise this
//!   selection gets nothing put home, logged (the window stays hidden and in place).
//! - **Take back** ([`take_back`]): back into its stage Space first, then its frame
//!   restored (invisible again). A window its app moved off the park meanwhile is only noted.
//! - **Kept for the activation's switch**: the model's show sets the frame the switch asks
//!   for (the platform never takes the window's place on screen for its frame while parked).
//!
//! The model never sees a parked frame: it ignores frames while it counts a window hidden,
//! and the platform drops the move notices of a window parked for a lend until its frame is
//! set again ([`crate::platform::window_engine::WindowEngine::parked_lend`]).
//!
//! **A Dock press** (round dock-put-home) lends the same way. Put home at its full frame it
//! showed under the front stage's windows from the press, and in front of them from the
//! release (macOS raises the activated app's windows), ~145 ms before its stage's switch
//! began (a recording, 11-18 10:59:48). Parked first, it is on its Desktop for macOS and
//! out of sight, and its stage comes in whole with the switch. The press gives less time
//! than the switcher (the release came 54 to 155 ms after the lend, 30 clicks 11-15 to
//! 11-18), so its park's move waits less for the app ([`DOCK_PARK_SECONDS`]) and the lend
//! is made whether the park took or not ([`ParkAsk::must`]): a window seen early is the
//! lesser harm, a lend after the release lets macOS slide to another Desktop.
//!
//! The calls go through [`LendSteps`] so the order is tested without a window server.

use super::geometry::Rect;
use super::model::Display;

/// How far (points) a window's listed origin may be from the one asked and still count as
/// there (Accessibility and the window server round).
pub const NEAR: f64 = 1.0;

/// How long a Dock press's park may wait for the app to move its window (seconds): ⌘Tab's
/// parks took 5 to 17 ms (93 lends, 11-15 to 11-18), and the Dock acts at the release, 54 ms
/// after the lend at the earliest (30 clicks, the same days). Past it the window is put home
/// where it is.
pub const DOCK_PARK_SECONDS: f32 = 0.03;

/// True when a window parked for a lend (`p`), listed at `listed`, is away from the frame it
/// had before the park: out of sight at the park, or on its way. Listed at that frame, the
/// park did not take (yet): put home, it is in view there.
pub fn away(listed: Rect, p: Parked) -> bool {
    !at_origin(listed, p.frame)
}

/// The moves a park and a restore make on one window (Accessibility through its app, and the
/// window server's list): all a park needs, so Mission Control's parks run on a thread per
/// app ([`super::lend_round`]).
pub trait Moves {
    /// Where the window server lists the window now.
    fn listed(&mut self) -> Option<Rect>;
    /// Moves the window's origin to `to`'s (Accessibility, bounded); where the window server
    /// lists it afterwards (`None`: the call failed or the window is not listed).
    fn move_to(&mut self, to: Rect) -> Option<Rect>;
}

/// The calls a lend and a take back make on one window, in the order this module decides.
pub trait LendSteps: Moves {
    /// Out of its stage Space onto its Desktop; true when listed out of every stage Space.
    fn put_home(&mut self) -> bool;
    /// Back into its stage Space; true when listed there and nowhere else.
    fn put_back(&mut self) -> bool;
}

/// A park asked for a lend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParkAsk {
    /// The park frame ([`super::corner::park_frame_of`] of the window's frame).
    pub at: Rect,
    /// The window's display: its park zone tells a park that took.
    pub display: Display,
    /// The window's frame as the model knows it: where it goes back when the window server
    /// does not list it before the park, or lists it at another size (Mission Control's
    /// layout, shrunk: a lend moves the window, it never resizes it).
    pub frame: Rect,
    /// Lend even when the park does not take (the first Tab: a quick ⌘Tab has no other
    /// chance, and a slide is worse than a window seen for a moment).
    pub must: bool,
}

/// A window parked for a lend, until its frame is set again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Parked {
    /// Where it was asked to park.
    pub at: Rect,
    /// Its frame before the park: where a take back restores it.
    pub frame: Rect,
    /// Its display (the park zone).
    pub display: Display,
}

/// What a lend did. A park's move, once sent, may still be carried out after it was read
/// back (the app answers after the bound, or the window server lists it later): so whenever
/// one was sent its restore is owed, unless the window was seen moved and then back at its
/// frame (review A-4); a restore is a no-op once the window is listed at its frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LendDone {
    /// On its Desktop now (lent).
    pub home: bool,
    /// The park asked: whether it took (`None`: none asked).
    pub parked: Option<bool>,
    /// Its frame owed: a park was sent (also when the lend was not made and the frame could
    /// not be seen restored).
    pub owed: Option<Parked>,
}

/// What a take back did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackDone {
    /// Back in its stage Space.
    pub back: bool,
    /// Its frame restored after a park (`None`: it was not parked).
    pub restored: Option<bool>,
    /// The window server listed it off the park before the take back (its app moved it).
    pub moved_off: bool,
}

/// True when `listed` has `want`'s origin (within [`NEAR`]).
pub fn at_origin(listed: Rect, want: Rect) -> bool {
    (listed.x - want.x).abs() <= NEAR && (listed.y - want.y).abs() <= NEAR
}

/// True when `a` and `b` have one size (within [`NEAR`]).
fn same_size(a: Rect, b: Rect) -> bool {
    (a.w - b.w).abs() <= NEAR && (a.h - b.h).abs() <= NEAR
}

/// A park made while the window is still hidden in its stage Space, before it goes home
/// ([`park`]); [`settle`] ends the lend once it is known whether it went.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Parking {
    /// Where it parked, and the frame it goes back to.
    pub parked: Parked,
    /// Listed in the park zone after the move: out of sight once home.
    pub took: bool,
    /// Listed away from its frame after the move (in the zone or not).
    pub moved: bool,
    /// Lent whether the park took or not ([`ParkAsk::must`]).
    pub must: bool,
    /// Where the window server listed the window after the move (`None`: not listed, or the
    /// move was not taken): what the log says of a park that did not take.
    pub landed: Option<Rect>,
}

impl Parking {
    /// True when the window goes home after this park: it took, or the lend is needed now.
    pub fn goes_home(&self) -> bool {
        self.took || self.must
    }

    /// The park as the window server lists the window now (`listed`), for a window parked
    /// a while ago and still hidden ([`super::lend_round`]: parked at Mission Control's key,
    /// put home once it shows): an app may have moved it since.
    pub fn now(parked: Parked, listed: Option<Rect>) -> Self {
        Self { parked, took: listed.is_some_and(|r| parked.display.parks(r)), moved: listed.is_some_and(|r| !at_origin(r, parked.frame)), must: false, landed: listed }
    }
}

/// The park of a lend, the window still hidden in its stage Space: moved to `ask.at`, read
/// back. Its frame is the one listed before (at the window's size, and not in the park zone:
/// a window parked already keeps the frame of the model), else the model's.
pub fn park(mac: &mut impl Moves, ask: ParkAsk) -> Parking {
    let before = mac.listed().filter(|r| same_size(*r, ask.frame) && !ask.display.parks(*r));
    let parked = Parked { at: ask.at, frame: before.unwrap_or(ask.frame), display: ask.display };
    let landed = mac.move_to(ask.at);
    Parking { parked, took: landed.is_some_and(|r| ask.display.parks(r)), moved: landed.is_some_and(|r| !at_origin(r, parked.frame)), must: ask.must, landed }
}

/// The lend after its park, `home` saying whether the window went home. Not lent (the park
/// did not take, or the window did not go home): still hidden in its stage Space, a move seen
/// is undone there, unseen; one not seen yet (refused, answered late, timed out) may still
/// come: owed.
pub fn settle(mac: &mut impl Moves, p: Parking, home: bool) -> LendDone {
    if home {
        return LendDone { home, parked: Some(p.took), owed: Some(p.parked) };
    }
    let undone = p.moved && mac.move_to(p.parked.frame).is_some_and(|r| at_origin(r, p.parked.frame));
    LendDone { home, parked: Some(p.took), owed: (!undone).then_some(p.parked) }
}

/// Lends the window: parked first while still hidden when `park` asks it, then put home.
/// Without a park only home.
pub fn lend(mac: &mut impl LendSteps, park_ask: Option<ParkAsk>) -> LendDone {
    let Some(ask) = park_ask else {
        return LendDone { home: mac.put_home(), parked: None, owed: None };
    };
    let p = park(mac, ask);
    let home = p.goes_home() && mac.put_home();
    settle(mac, p, home)
}

/// True when a window parked for a lend (`parked`) is listed off the park and not at its
/// frame: its app moved it (one listed at its frame never took the park, its restore owed
/// only in case the move came late).
pub fn moved_off(mac: &mut impl Moves, parked: Option<Parked>) -> bool {
    parked.is_some_and(|p| mac.listed().is_some_and(|r| !p.display.parks(r) && !at_origin(r, p.frame)))
}

/// Takes the lent window back: into its stage Space first, then (when it was parked) its
/// frame restored there, unseen. A window that did not go back keeps its park (restoring it
/// on the Desktop would show it): its frame stays owed until it hides or shows.
pub fn take_back(mac: &mut impl LendSteps, parked: Option<Parked>) -> BackDone {
    let moved_off = moved_off(mac, parked);
    let back = mac.put_back();
    let restored = match parked {
        Some(p) if back => Some(restore(mac, p)),
        Some(_) => Some(false),
        None => None,
    };
    BackDone { back, restored, moved_off }
}

/// The frame of a window parked for a lend set again (it is hidden, or about to show): true
/// when the window server lists it there.
pub fn restore(mac: &mut impl Moves, p: Parked) -> bool {
    if mac.listed().is_some_and(|r| at_origin(r, p.frame)) {
        return true;
    }
    mac.move_to(p.frame).is_some_and(|r| at_origin(r, p.frame))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::corner::park_frame_of;
    use crate::engine::corner::park::ParkRequest;
    use crate::engine::recovery::{EngineKind, Known, ScanDisplays, is_stranded};
    use crate::engine::testkit::display;

    /// A window as the window server lists it, and every call in order.
    #[derive(Debug, Default)]
    struct Fake {
        at: Option<Rect>,
        staged: bool,
        /// The app moves its window (false: Accessibility refused or answered late).
        moves: bool,
        /// Accessibility's call times out: the move is not taken now and nothing is read back.
        times_out: bool,
        /// The window server takes it home / back.
        homes: bool,
        backs: bool,
        /// Where macOS lands a park: clamped above the visible bottom, as it does on screen.
        clamp_y: Option<f64>,
        calls: Vec<String>,
        /// Frames listed while on its Desktop (what anyone could see).
        seen: Vec<Rect>,
    }

    impl Fake {
        fn new(frame: Rect) -> Self {
            Self { at: Some(frame), staged: true, moves: true, homes: true, backs: true, ..Self::default() }
        }

        fn look(&mut self) {
            if !self.staged
                && let Some(r) = self.at
            {
                self.seen.push(r);
            }
        }
    }

    impl Moves for Fake {
        fn listed(&mut self) -> Option<Rect> {
            self.at
        }
        fn move_to(&mut self, to: Rect) -> Option<Rect> {
            self.calls.push(format!("move {},{} staged={}", to.x, to.y, self.staged));
            if self.times_out {
                return None;
            }
            if self.moves
                && let Some(r) = self.at.as_mut()
            {
                r.x = to.x;
                r.y = self.clamp_y.filter(|_| to.y > 1000.0).unwrap_or(to.y);
            }
            self.look();
            self.at
        }
    }

    impl LendSteps for Fake {
        fn put_home(&mut self) -> bool {
            self.calls.push("home".into());
            if self.homes {
                self.staged = false;
            }
            self.look();
            self.homes
        }
        fn put_back(&mut self) -> bool {
            self.calls.push("back".into());
            if self.backs {
                self.staged = true;
            }
            self.look();
            self.backs
        }
    }

    fn frame() -> Rect {
        Rect::new(300.0, 120.0, 1100.0, 800.0)
    }

    fn ask(must: bool) -> ParkAsk {
        let d = display();
        ParkAsk { at: park_frame_of(frame(), &d, ParkRequest::default()), display: d, frame: frame(), must }
    }

    /// Parked while still in its stage Space, then home; back into its stage Space, then its
    /// frame: never listed on its Desktop at its frame.
    #[test]
    fn a_lend_parks_before_it_goes_home_and_restores_after_it_goes_back() {
        let mut mac = Fake::new(frame());
        let a = ask(false);
        let done = lend(&mut mac, Some(a));
        assert!(done.home);
        assert_eq!(done.parked, Some(true));
        assert_eq!(done.owed, Some(Parked { at: a.at, frame: frame(), display: a.display }));
        assert_eq!(mac.calls, vec![format!("move {},{} staged=true", a.at.x, a.at.y), "home".to_string()], "the park first, unseen");
        let back = take_back(&mut mac, done.owed);
        assert_eq!(back, BackDone { back: true, restored: Some(true), moved_off: false });
        assert_eq!(mac.calls[2..], ["back".to_string(), format!("move {},{} staged=true", frame().x, frame().y)], "back first, then the frame");
        assert_eq!(mac.at, Some(frame()));
        assert!(mac.seen.iter().all(|r| a.display.parks(*r)), "only ever seen parked: {:?}", mac.seen);
    }

    /// macOS clamps a park above the visible bottom (a sliver of title bar): still a park.
    #[test]
    fn a_park_macos_clamps_still_counts() {
        let mut mac = Fake::new(frame());
        let d = display();
        mac.clamp_y = Some(d.visible.max_y() - 20.0);
        let done = lend(&mut mac, Some(ask(false)));
        assert_eq!((done.home, done.parked), (true, Some(true)));
    }

    /// A park that does not take: the first Tab lends all the same (a quick ⌘Tab's only
    /// chance); a later selection gets nothing put home, the window untouched and hidden.
    #[test]
    fn a_park_refused_lends_only_when_the_lend_is_needed_now() {
        let a = ask(false);
        let owed = Some(Parked { at: a.at, frame: frame(), display: a.display });
        let mut mac = Fake::new(frame());
        mac.moves = false;
        let done = lend(&mut mac, Some(a));
        assert_eq!(done, LendDone { home: false, parked: Some(false), owed }, "the move may come late: its restore owed");
        assert!(mac.staged && mac.seen.is_empty(), "not lent: still hidden");
        let mut mac = Fake::new(frame());
        mac.moves = false;
        let done = lend(&mut mac, Some(ask(true)));
        assert_eq!(done, LendDone { home: true, parked: Some(false), owed }, "lent at its frame, its restore owed");
        assert_eq!(take_back(&mut mac, done.owed), BackDone { back: true, restored: Some(true), moved_off: false }, "listed at its frame: restored at once");
        assert_eq!(mac.calls.iter().filter(|c| c.starts_with("move")).count(), 1, "the restore asked nothing of the app");
    }

    /// A park that lands off the park zone (the app re-centred it) and no lend: put back where
    /// it was, still hidden.
    #[test]
    fn a_park_off_the_zone_without_a_lend_is_undone_unseen() {
        let mut mac = Fake::new(frame());
        mac.clamp_y = Some(200.0);
        let d = display();
        let off = ParkAsk { at: Rect::new(d.frame.max_x() - 700.0, 1500.0, 1100.0, 800.0), ..ask(false) };
        let done = lend(&mut mac, Some(off));
        assert_eq!((done.home, done.parked, done.owed), (false, Some(false), None));
        assert_eq!(mac.at, Some(frame()));
        assert!(mac.seen.is_empty());
    }

    /// A Dock press's lend (round dock-put-home): parked while still hidden, so it is never
    /// listed on its Desktop at its frame before its stage's switch; an app that does not
    /// move its window in time (refused, or the bounded call timed out) is lent all the same,
    /// in view at its frame, which the switch's plan is told ([`away`]).
    #[test]
    fn a_dock_press_lend_parks_first_and_lends_whatever_the_park_did() {
        let a = ask(true);
        let mut mac = Fake::new(frame());
        let done = lend(&mut mac, Some(a));
        assert_eq!((done.home, done.parked), (true, Some(true)));
        assert!(!mac.seen.is_empty() && mac.seen.iter().all(|r| a.display.parks(*r)), "only ever seen parked: {:?}", mac.seen);
        let parked = done.owed.expect("its frame owed");
        assert!(away(mac.at.expect("listed"), parked), "out of sight: its picture grows with its stage's");
        // The press was no click: back unseen, its frame restored.
        assert_eq!(take_back(&mut mac, done.owed), BackDone { back: true, restored: Some(true), moved_off: false });
        assert!(mac.seen.iter().all(|r| a.display.parks(*r)), "never seen at its frame: {:?}", mac.seen);
        for (moves, times_out) in [(false, false), (true, true)] {
            let mut mac = Fake::new(frame());
            (mac.moves, mac.times_out) = (moves, times_out);
            let done = lend(&mut mac, Some(a));
            assert_eq!((done.home, done.parked), (true, Some(false)), "the Dock acts at the release: lent in time, seen or not");
            let parked = done.owed.expect("the move may come late: its restore owed");
            assert!(!away(mac.at.expect("listed"), parked), "in view at its frame: no picture grows over it");
        }
    }

    /// A lend with no park asked: home at its frame.
    #[test]
    fn a_lend_without_a_park_is_only_home() {
        let mut mac = Fake::new(frame());
        assert_eq!(lend(&mut mac, None), LendDone { home: true, parked: None, owed: None }, "no park sent: nothing owed");
        assert_eq!(mac.calls, ["home"]);
        assert_eq!(take_back(&mut mac, None), BackDone { back: true, restored: None, moved_off: false });
    }

    /// A take back that did not go back keeps the park (restoring on the Desktop would show
    /// it); one its app moved off the park is noted.
    #[test]
    fn a_take_back_that_failed_keeps_the_park_and_a_move_off_is_noted() {
        let mut mac = Fake::new(frame());
        let done = lend(&mut mac, Some(ask(false)));
        mac.backs = false;
        let back = take_back(&mut mac, done.owed);
        assert_eq!(back, BackDone { back: false, restored: Some(false), moved_off: false });
        assert!(mac.seen.iter().all(|r| display().parks(*r)), "never shown at its frame");
        let mut mac = Fake::new(frame());
        let done = lend(&mut mac, Some(ask(false)));
        mac.at = Some(Rect::new(500.0, 300.0, 1100.0, 800.0));
        assert!(take_back(&mut mac, done.owed).moved_off, "its app moved it off the park");
    }

    /// A lend that could not go home after its park: its frame back in its stage Space.
    #[test]
    fn a_park_whose_lend_failed_is_restored_in_its_stage_space() {
        let mut mac = Fake::new(frame());
        mac.homes = false;
        let done = lend(&mut mac, Some(ask(false)));
        assert_eq!(done, LendDone { home: false, parked: Some(true), owed: None }, "seen moved, then seen back: nothing owed");
        assert_eq!(mac.at, Some(frame()));
        assert!(mac.seen.is_empty());
    }

    /// Review A-4: a park whose Accessibility call times out (the app busy) is carried out
    /// late, after the lend: its restore is owed, so a take back, a hide or a show sets the
    /// frame again instead of leaving the window at the park.
    #[test]
    fn a_park_that_timed_out_is_owed_its_restore() {
        let mut mac = Fake::new(frame());
        mac.times_out = true;
        let a = ask(true);
        let done = lend(&mut mac, Some(a));
        assert_eq!(done, LendDone { home: true, parked: Some(false), owed: Some(Parked { at: a.at, frame: frame(), display: a.display }) });
        // The app answers now: the window goes to the park.
        mac.at = Some(a.at);
        mac.times_out = false;
        let back = take_back(&mut mac, done.owed);
        assert_eq!(back.restored, Some(true));
        assert_eq!(mac.at, Some(frame()), "its frame again");
        // Not lent (a later Tab), the park timed out: owed all the same.
        let mut mac = Fake::new(frame());
        mac.times_out = true;
        assert!(lend(&mut mac, Some(ask(false))).owed.is_some());
    }

    /// The model's frame is the fallback when the window server does not list the window
    /// before the park.
    #[test]
    fn the_frame_restored_is_the_listed_one_else_the_models() {
        let mut mac = Fake::new(Rect::new(310.0, 130.0, 1100.0, 800.0));
        let done = lend(&mut mac, Some(ask(false)));
        assert_eq!(done.owed.map(|p| p.frame), Some(Rect::new(310.0, 130.0, 1100.0, 800.0)), "where it was listed");
        let mut mac = Fake { at: None, ..Fake::new(frame()) };
        let done = lend(&mut mac, Some(ask(true)));
        assert_eq!(done.parked, Some(false));
    }

    /// A window listed at its park already (parked at Mission Control's key, then lent for ⌘Tab
    /// before it showed): the park is never taken as its frame, the model's is.
    #[test]
    fn a_window_parked_already_keeps_the_models_frame() {
        let a = ask(false);
        let mut mac = Fake::new(a.at);
        let done = lend(&mut mac, Some(a));
        assert_eq!(done.owed.map(|p| p.frame), Some(frame()), "never the park as its frame");
        assert!(done.home);
    }

    /// Listed at another size before the park (Mission Control's layout shrinks the windows
    /// it shows, 11-18: 1949x1288 listed at 718x474), the listing is not the window's frame:
    /// a take back restores the model's, so a window put back ends hidden at its own frame.
    #[test]
    fn a_listing_at_another_size_is_not_the_frame_restored() {
        let shrunk = Rect::new(18.0, 780.0, 718.0, 474.0);
        let mut mac = Fake::new(shrunk);
        let done = lend(&mut mac, Some(ask(false)));
        assert_eq!(done.owed.map(|p| p.frame), Some(frame()), "the model's frame, never the layout's");
        let back = take_back(&mut mac, done.owed);
        assert_eq!(back.restored, Some(true));
        assert_eq!(mac.at.map(|r| (r.x, r.y)), Some((frame().x, frame().y)), "its own origin again");
    }

    /// After a crash with a parked lend on the Desktop (no stage Space holds it), the
    /// guardian's and the launch's recovery scan find it stranded in the park zone and bring
    /// it to its registered frame (the model's, never the park); the guardian's watch of a
    /// Spaces run waits for it there (review A-8: it judged every frame not stranded).
    #[test]
    fn a_parked_lend_is_stranded_for_the_recovery_scan() {
        let d = display();
        let a = ask(false);
        let displays = ScanDisplays::new(d.frame, d.visible, Vec::new(), [d.corner]);
        assert!(is_stranded(a.at, &displays, Known::Registered), "the park frame is stranded");
        assert!(!is_stranded(frame(), &displays, Known::Registered), "its frame is not");
        let spaces = EngineKind::Spaces;
        assert!(spaces.stranded(a.at, &displays, Known::Registered), "the Spaces run's watch waits for it at the park");
        assert!(!spaces.stranded(frame(), &displays, Known::Registered));
        assert!(!spaces.stranded(a.at, &displays, Known::Managed), "a window it did not register: never by its frame");
        assert!(!spaces.stranded(a.at, &displays, Known::Unknown));
    }
}
