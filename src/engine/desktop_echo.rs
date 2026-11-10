//! A wallpaper click's echo: the Finder activation and focus that follow it.
//!
//! macOS gives the desktop to Finder, so a click on the wallpaper makes Finder the active
//! app. A moment later (0.1 to 0.5 s in the owner's log of 10-12) the system reports Finder
//! activated and, when Finder has windows, its main window focused, even when that window
//! sits parked in a stage of the strip. Apple's Stage Manager leaves Finder frontmost
//! without bringing its windows back: they come back only when the user picks their card,
//! clicks one of them, or activates Finder on purpose (Dock, ⌘Tab). Taken at face value,
//! the echo made every reveal bring Finder's stage forward ("Finder comes back and leaves,
//! back and leaves, on each desktop click").
//!
//! [`DesktopEcho`] tells the echo from a deliberate act. From a press on the desktop (the
//! wallpaper or a desktop icon), an activation of the desktop's owner ([`DESKTOP_OWNER`]) or
//! a focus of one of its windows is the press's echo while the button is down and until
//! [`WALLPAPER_ECHO`] after its release, however the press ends (a click, a long press, a
//! drag, a rubber band), unless the user does something else first (a mouse-down anywhere
//! but the desktop, a key press). Since the reveal waits for the release, macOS's Finder
//! activation can come while the press is still down, about a second after it (11-09
//! 16:33:26.9 press, 16:33:27.92 Finder activated, right at the release of a 1 s press).
//! A click on one of our strip's cards is not "something else": the global input monitor
//! never sees clicks on our own panel, and a card picked right after a reveal must not be
//! fought by Finder's late echo.

use std::time::{Duration, Instant};

/// The app that owns the desktop (its window is the wallpaper's click target).
pub const DESKTOP_OWNER: &str = "com.apple.finder";

/// How long after a desktop press (and again after its release) the desktop owner's
/// activation and focus count as its echo (unless the user did something else first).
pub const WALLPAPER_ECHO: Duration = Duration::from_millis(1000);

/// How long a desktop press may stay down and still count as down: a release the input
/// monitor missed never leaves Finder's activations echoes for good.
pub const PRESS_HELD_MAX: Duration = Duration::from_secs(60);

/// What a desktop owner's activation or window focus is, after [`DesktopEcho::classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Not the desktop owner, or nothing pending: handle it as usual.
    Other,
    /// The echo of the last wallpaper click: switch nothing, unpark nothing.
    WallpaperEcho,
}

/// The pending echo of the last desktop press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesktopEcho {
    /// The press, or its release once it came.
    since: Option<Instant>,
    /// The press is still down (its release not seen yet).
    down: bool,
}

impl DesktopEcho {
    /// The user pressed the desktop: the wallpaper or a desktop icon (whether or not anything
    /// will be revealed: Finder becomes frontmost either way).
    pub fn desktop_pressed(&mut self, now: Instant) {
        self.since = Some(now);
        self.down = true;
    }

    /// A left mouse-up: a desktop press still pending is released now, and its echo runs
    /// [`WALLPAPER_ECHO`] from here. Nothing for any other release.
    pub fn released(&mut self, now: Instant) {
        if std::mem::take(&mut self.down) && self.since.is_some() {
            self.since = Some(now);
        }
    }

    /// The user did something else (a mouse-down off the desktop, a key press): what the
    /// desktop owner does next is no echo. True when an echo was pending.
    pub fn user_acted(&mut self) -> bool {
        self.down = false;
        self.since.take().is_some()
    }

    /// True while a desktop press's echo is expected: the press is down, or it was pressed or
    /// released less than [`WALLPAPER_ECHO`] ago.
    pub fn pending(&self, now: Instant) -> bool {
        self.since.is_some_and(|t| {
            let age = now.saturating_duration_since(t);
            age < WALLPAPER_ECHO || (self.down && age < PRESS_HELD_MAX)
        })
    }

    /// Classifies an activation of the app `bundle_id`, or a focus of one of its windows.
    pub fn classify(&self, bundle_id: Option<&str>, now: Instant) -> Cause {
        if bundle_id == Some(DESKTOP_OWNER) && self.pending(now) {
            Cause::WallpaperEcho
        } else {
            Cause::Other
        }
    }
}

#[cfg(test)]
mod tests {
    //! The guard alone, then the whole sequence with the stage model: a reveal, Finder's
    //! echo, a second and a third click, a card picked while the desktop shows, and a
    //! deliberate Finder activation.

    use super::*;
    use crate::engine::model::{Action, Model, StageId, StripOptions, ModelOptions};
    use crate::engine::rules::{Decision, Placement, RuleAction};
    use crate::engine::testkit::*;

    const FINDER_PID: i32 = 50;
    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn only_the_desktop_owner_echoes_and_only_for_a_while() {
        let t0 = Instant::now();
        let mut e = DesktopEcho::default();
        assert_eq!(e.classify(Some(DESKTOP_OWNER), t0), Cause::Other, "no click yet");
        e.desktop_pressed(t0);
        e.released(t0 + MS(100));
        assert_eq!(e.classify(Some(DESKTOP_OWNER), t0 + MS(150)), Cause::WallpaperEcho);
        assert_eq!(e.classify(Some(DESKTOP_OWNER), t0 + MS(450)), Cause::WallpaperEcho);
        assert_eq!(e.classify(Some("com.example.a"), t0 + MS(150)), Cause::Other);
        assert_eq!(e.classify(None, t0 + MS(150)), Cause::Other);
        assert_eq!(e.classify(Some(DESKTOP_OWNER), t0 + MS(100) + WALLPAPER_ECHO), Cause::Other, "expired");
    }

    #[test]
    fn any_other_input_ends_the_echo() {
        let t0 = Instant::now();
        let mut e = DesktopEcho::default();
        e.desktop_pressed(t0);
        assert!(e.user_acted());
        assert!(!e.user_acted(), "nothing pending any more");
        assert_eq!(e.classify(Some(DESKTOP_OWNER), t0 + MS(100)), Cause::Other);
        e.desktop_pressed(t0 + MS(200));
        e.released(t0 + MS(250));
        assert_eq!(e.classify(Some(DESKTOP_OWNER), t0 + MS(300)), Cause::WallpaperEcho, "a new click arms it again");
    }

    // ----- the sequence with the model --------------------------------------------------

    /// The model, a fake desk and the echo guard, wired as the controller wires them.
    struct Desktop {
        m: Model,
        d: Desk,
        echo: DesktopEcho,
        t0: Instant,
    }

    impl Desktop {
        /// Stage 1: Finder's window 1; stage 2: app A's window 2 (active); stage 3 (with
        /// `more`): app B's window 3, then stage 2 active again.
        fn new(more: bool, capacity: usize) -> Self {
            let options = ModelOptions { strip: StripOptions { capacity, ..Default::default() }, ..Default::default() };
            let mut s = Self { m: model_with(options), d: Desk::default(), echo: DesktopEcho::default(), t0: Instant::now() };
            s.add(meta(1, FINDER_PID, DESKTOP_OWNER), RuleAction::Current);
            s.add(meta(2, 10, "com.example.a"), RuleAction::New);
            if more {
                s.add(meta(3, 11, "com.example.b"), RuleAction::New);
                s.switch_to_window(2);
            }
            s
        }

        fn add(&mut self, meta: crate::engine::model::WindowMeta, action: RuleAction) {
            self.d.open(&meta);
            let obs = self.d.obs();
            let a = self.m.add_window(meta, &Decision { action, placement: Placement::AsIs, ..Decision::default() }, &obs);
            self.d.apply(&a);
        }

        fn at(&self, ms: u64) -> Instant {
            self.t0 + MS(ms)
        }

        fn stage(&self, id: u32) -> StageId {
            self.m.stage_of(w(id)).expect("in a stage")
        }

        fn switch_to_window(&mut self, id: u32) -> Vec<Action> {
            let obs = self.d.obs();
            let a = self.m.switch_to(self.stage(id), &obs);
            self.d.apply(&a);
            a
        }

        /// A wallpaper click at `ms` (pressed and released at once).
        fn wallpaper_click(&mut self, ms: u64) -> Vec<Action> {
            self.press(ms);
            self.release(ms, true)
        }

        /// A press on the desktop at `ms`: nothing reveals before its release.
        fn press(&mut self, ms: u64) {
            self.echo.desktop_pressed(self.at(ms));
        }

        /// Its release at `ms`: a still short click reveals (`click`); a long press or a
        /// drag reveals nothing.
        fn release(&mut self, ms: u64, click: bool) -> Vec<Action> {
            self.echo.released(self.at(ms));
            if !click {
                return Vec::new();
            }
            let obs = self.d.obs();
            let a = self.m.reveal_desktop(&obs);
            self.d.apply(&a);
            a
        }

        /// Finder activated at `ms`, as the controller handles it.
        fn finder_activated(&mut self, ms: u64) -> (Cause, Vec<Action>) {
            let cause = self.echo.classify(Some(DESKTOP_OWNER), self.at(ms));
            if cause == Cause::WallpaperEcho {
                return (cause, Vec::new());
            }
            let obs = self.d.obs();
            let a = self.m.app_activated(FINDER_PID, &obs);
            self.d.apply(&a);
            (cause, a)
        }

        /// Finder's window 1 reported focused at `ms`, as the controller handles it.
        fn finder_focused(&mut self, ms: u64) -> (Cause, Vec<Action>) {
            let cause = self.echo.classify(Some(DESKTOP_OWNER), self.at(ms));
            if cause == Cause::WallpaperEcho {
                return (cause, Vec::new());
            }
            let obs = self.d.obs();
            let a = self.m.window_focused(w(1), &obs);
            self.d.apply(&a);
            (cause, a)
        }

        /// A click on stage `stage`'s card (our own panel: the guard never hears of it).
        fn card_click(&mut self, stage: StageId) -> Vec<Action> {
            let obs = self.d.obs();
            let a = self.m.switch_to(stage, &obs);
            self.d.apply(&a);
            a
        }

        fn slots(&self) -> Vec<StageId> {
            self.m.strip().iter().map(|c| c.stage).collect()
        }
    }

    #[test]
    fn finder_echo_after_a_reveal_is_ignored_and_the_reveal_holds() {
        let mut s = Desktop::new(false, 6);
        let finder = s.stage(1);
        let a = s.stage(2);
        assert!(s.d.parked(w(1)) && !s.d.parked(w(2)));
        s.wallpaper_click(0);
        assert!(s.m.desktop_revealed());
        assert!(s.d.parked(w(1)) && s.d.parked(w(2)), "every window goes to the strip");
        assert_eq!(s.slots(), [finder, a], "the revealed stage enters the strip at once, in the free slot");

        // Without the guard, Finder's echo would bring Finder's stage: the reported bug.
        let mut naive = s.m.clone();
        assert!(!naive.app_activated(FINDER_PID, &s.d.obs()).is_empty(), "the echo taken at face value switches");

        let (cause, actions) = s.finder_activated(150);
        assert_eq!(cause, Cause::WallpaperEcho);
        assert!(actions.is_empty());
        let (cause, actions) = s.finder_focused(450);
        assert_eq!(cause, Cause::WallpaperEcho);
        assert!(actions.is_empty());
        assert!(s.m.desktop_revealed(), "the reveal holds");
        assert!(s.d.parked(w(1)) && s.d.parked(w(2)), "Finder's window stays parked");
    }

    #[test]
    fn second_click_brings_back_the_revealed_stage_never_finders_and_a_third_reveals_again() {
        let mut s = Desktop::new(false, 6);
        let a = s.stage(2);
        s.wallpaper_click(0);
        s.finder_activated(150);
        s.finder_focused(450);
        // Second click (UX-SPEC: a second wallpaper click brings the revealed stage back).
        s.wallpaper_click(1600);
        assert_eq!(s.m.main_active_stage(), Some(a), "the stage the click revealed comes back");
        assert!(!s.d.parked(w(2)) && s.d.parked(w(1)), "Finder's window stays in the strip");
        let (cause, _) = s.finder_focused(1900);
        assert_eq!(cause, Cause::WallpaperEcho);
        assert!(s.d.parked(w(1)));
        // Third click: the desktop again, Finder still parked.
        s.wallpaper_click(3200);
        s.finder_activated(3350);
        assert!(s.m.desktop_revealed());
        assert!(s.d.parked(w(1)) && s.d.parked(w(2)));
    }

    #[test]
    fn a_deliberate_finder_activation_still_brings_its_stage() {
        // A Dock click or ⌘Tab: input the monitor sees, then Finder's activation.
        let mut s = Desktop::new(false, 6);
        let finder = s.stage(1);
        s.wallpaper_click(0);
        s.finder_activated(150);
        assert!(s.echo.user_acted(), "the Dock click ends the echo");
        let (cause, actions) = s.finder_activated(400);
        assert_eq!(cause, Cause::Other);
        assert!(!actions.is_empty());
        assert_eq!(s.m.main_active_stage(), Some(finder));
        assert!(!s.d.parked(w(1)) && s.d.parked(w(2)), "Finder's window comes back, A's stays in the strip");
        assert!(!s.m.desktop_revealed());

        // Scripted (`open -b com.apple.finder`): no input, but after the echo's time is up.
        let mut s = Desktop::new(false, 6);
        s.wallpaper_click(0);
        let (cause, _) = s.finder_focused(WALLPAPER_ECHO.as_millis() as u64 + 50);
        assert_eq!(cause, Cause::Other);
        assert_eq!(s.m.main_active_stage(), Some(finder));
        assert!(!s.d.parked(w(1)));
    }

    #[test]
    fn a_card_click_while_revealed_brings_exactly_that_stage_and_the_echo_does_not_fight_it() {
        let mut s = Desktop::new(true, 6);
        let (finder, a, b) = (s.stage(1), s.stage(2), s.stage(3));
        let before = s.slots();
        s.wallpaper_click(0);
        let revealed = s.slots();
        assert_eq!(revealed.len(), before.len() + 1);
        assert_eq!(&revealed[..before.len()], &before[..], "the cards already there keep their slots");
        assert_eq!(revealed.last(), Some(&a), "the revealed stage takes the free slot at once");

        // The card is picked before Finder's echo arrives.
        s.card_click(b);
        assert_eq!(s.m.main_active_stage(), Some(b));
        let (cause, actions) = s.finder_activated(300);
        assert_eq!(cause, Cause::WallpaperEcho);
        assert!(actions.is_empty(), "no bounce, no second switch");
        let (cause, _) = s.finder_focused(500);
        assert_eq!(cause, Cause::WallpaperEcho);
        assert_eq!(s.m.main_active_stage(), Some(b), "exactly the picked stage");
        assert!(!s.d.parked(w(3)) && s.d.parked(w(1)) && s.d.parked(w(2)));
        let after = s.slots();
        let others: Vec<StageId> = revealed.iter().copied().filter(|&x| x != b).collect();
        assert_eq!(after, others, "the other cards keep their order; nothing takes over a slot");
        assert!(after.contains(&finder) && after.contains(&a));
    }

    #[test]
    fn the_just_revealed_stage_card_brings_it_back_as_it_was() {
        let mut s = Desktop::new(true, 6);
        let a = s.stage(2);
        let frame = s.d.frames[&w(2)];
        s.wallpaper_click(0);
        s.finder_activated(150);
        let actions = s.card_click(a);
        assert_eq!(s.m.main_active_stage(), Some(a));
        assert_eq!(s.d.frames[&w(2)], frame, "at exactly its frame");
        assert_eq!(s.d.focused, Some(w(2)), "its key window focused");
        assert!(actions.iter().any(|x| matches!(x, Action::Focus { id } if *id == w(2))));
        assert!(s.d.parked(w(1)) && s.d.parked(w(3)));
        let (cause, _) = s.finder_focused(400);
        assert_eq!(cause, Cause::WallpaperEcho);
        assert_eq!(s.m.main_active_stage(), Some(a));
        assert!(!s.m.desktop_revealed());
    }

    #[test]
    fn a_full_strip_gives_the_revealed_stage_the_least_recent_slot_at_once() {
        // Capacity 2: Finder's and B's cards fill it; A is revealed.
        let mut s = Desktop::new(true, 2);
        let (finder, a, b) = (s.stage(1), s.stage(2), s.stage(3));
        let before = s.slots();
        assert_eq!(before.len(), 2);
        assert!(before.contains(&finder) && before.contains(&b));
        let lru = *before.iter().min_by_key(|x| s.m.stage(**x).map(|st| st.last_active)).expect("a card");
        let lru_slot = before.iter().position(|x| *x == lru).expect("its slot");
        s.wallpaper_click(0);
        let after = s.slots();
        assert_eq!(after[lru_slot], a, "the revealed stage takes the least recently used card's slot");
        assert!(!after.contains(&lru));
    }

    // ----- presses whose reveal waits for the release (round finder-press) --------------

    /// Finder's stage never came: nothing switched, A's stage in front, Finder's window parked.
    fn finder_stayed_away(s: &Desktop, a: StageId) {
        assert_eq!(s.m.main_active_stage(), Some(a), "A's stage stays in front");
        assert!(s.d.parked(w(1)) && !s.d.parked(w(2)), "Finder's window stays in the strip");
    }

    #[test]
    fn a_long_press_on_the_wallpaper_brings_no_finder_stage() {
        // 11-09 16:33:26.899 press, Finder activated 1025 ms later while still down (handled
        // before the release reached us), released after 1024 ms: no click, nothing revealed.
        let mut s = Desktop::new(false, 6);
        let a = s.stage(2);
        s.press(0);
        let (cause, actions) = s.finder_activated(1025);
        assert_eq!(cause, Cause::WallpaperEcho, "the press is down: the desktop's");
        assert!(actions.is_empty());
        assert!(s.release(1116, false).is_empty(), "a long press reveals nothing");
        let (cause, _) = s.finder_focused(1500);
        assert_eq!(cause, Cause::WallpaperEcho, "just after the release: still the desktop's");
        finder_stayed_away(&s, a);
        assert!(!s.m.desktop_revealed());
    }

    #[test]
    fn a_rubber_band_or_a_file_drag_from_the_desktop_brings_no_finder_stage() {
        let mut s = Desktop::new(false, 6);
        let a = s.stage(2);
        s.press(0);
        assert_eq!(s.finder_activated(200).0, Cause::WallpaperEcho);
        assert_eq!(s.finder_focused(2500).0, Cause::WallpaperEcho, "still dragging");
        s.release(4000, false);
        let (cause, actions) = s.finder_activated(4600);
        assert_eq!(cause, Cause::WallpaperEcho, "Finder's late activation at the drop");
        assert!(actions.is_empty());
        finder_stayed_away(&s, a);
        assert_eq!(s.finder_focused(4000 + WALLPAPER_ECHO.as_millis() as u64 + 50).0, Cause::Other, "the echo ends");
    }

    #[test]
    fn a_quick_click_reveals_and_finders_late_echo_switches_nothing() {
        let mut s = Desktop::new(false, 6);
        s.press(0);
        assert!(!s.m.desktop_revealed(), "nothing reveals on the press");
        assert!(!s.release(150, true).is_empty());
        assert!(s.m.desktop_revealed());
        let (cause, actions) = s.finder_activated(1100);
        assert_eq!(cause, Cause::WallpaperEcho, "within the echo of the release");
        assert!(actions.is_empty());
        assert!(s.m.desktop_revealed(), "the reveal holds");
        assert!(s.d.parked(w(1)) && s.d.parked(w(2)));
    }

    #[test]
    fn a_dock_click_on_finder_after_a_desktop_press_brings_its_stage() {
        // Later than the echo window: the Dock's press ends nothing pending, Finder comes.
        let mut s = Desktop::new(false, 6);
        let finder = s.stage(1);
        s.press(0);
        s.release(1100, false);
        let dock = 1100 + WALLPAPER_ECHO.as_millis() as u64 + 200;
        assert!(!s.echo.pending(s.at(dock)), "the echo is over");
        s.echo.user_acted(); // the Dock's mouse-down
        let (cause, actions) = s.finder_activated(dock + 50);
        assert_eq!(cause, Cause::Other);
        assert!(!actions.is_empty());
        assert_eq!(s.m.main_active_stage(), Some(finder));
        assert!(!s.d.parked(w(1)));

        // Within the window: the Dock's mouse-down (off the desktop) makes it deliberate.
        let mut s = Desktop::new(false, 6);
        let finder = s.stage(1);
        s.press(0);
        s.release(1100, false);
        assert!(s.echo.user_acted(), "the Dock click ends the echo");
        s.release(1300, false);
        let (cause, actions) = s.finder_activated(1350);
        assert_eq!(cause, Cause::Other, "the Dock's release arms nothing");
        assert!(!actions.is_empty());
        assert_eq!(s.m.main_active_stage(), Some(finder));
    }
}
