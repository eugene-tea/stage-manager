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
//! [`DesktopEcho`] tells the echo from a deliberate act. After a wallpaper click, an
//! activation of the desktop's owner ([`DESKTOP_OWNER`]) or a focus of one of its windows
//! is the click's echo until the user does something else (a mouse-down anywhere but the
//! wallpaper, a key press) or [`WALLPAPER_ECHO`] has passed. A click on one of our strip's
//! cards is not "something else": the global input monitor never sees clicks on our own
//! panel, and a card picked right after a reveal must not be fought by Finder's late echo.

use std::time::{Duration, Instant};

/// The app that owns the desktop (its window is the wallpaper's click target).
pub const DESKTOP_OWNER: &str = "com.apple.finder";

/// How long after a wallpaper click the desktop owner's activation and focus count as its
/// echo (unless the user did something else first).
pub const WALLPAPER_ECHO: Duration = Duration::from_millis(1000);

/// What a desktop owner's activation or window focus is, after [`DesktopEcho::classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Not the desktop owner, or nothing pending: handle it as usual.
    Other,
    /// The echo of the last wallpaper click: switch nothing, unpark nothing.
    WallpaperEcho,
}

/// The pending echo of the last wallpaper click.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesktopEcho {
    clicked: Option<Instant>,
}

impl DesktopEcho {
    /// The user clicked the wallpaper (whether or not anything was revealed: Finder becomes
    /// frontmost either way).
    pub fn wallpaper_clicked(&mut self, now: Instant) {
        self.clicked = Some(now);
    }

    /// The user did something else (a mouse-down off the wallpaper, a key press): what the
    /// desktop owner does next is no echo. True when an echo was pending.
    pub fn user_acted(&mut self) -> bool {
        self.clicked.take().is_some()
    }

    /// True while a wallpaper click's echo is expected.
    pub fn pending(&self, now: Instant) -> bool {
        self.clicked.is_some_and(|t| now.saturating_duration_since(t) < WALLPAPER_ECHO)
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
        e.wallpaper_clicked(t0);
        assert_eq!(e.classify(Some(DESKTOP_OWNER), t0 + MS(150)), Cause::WallpaperEcho);
        assert_eq!(e.classify(Some(DESKTOP_OWNER), t0 + MS(450)), Cause::WallpaperEcho);
        assert_eq!(e.classify(Some("com.example.a"), t0 + MS(150)), Cause::Other);
        assert_eq!(e.classify(None, t0 + MS(150)), Cause::Other);
        assert_eq!(e.classify(Some(DESKTOP_OWNER), t0 + WALLPAPER_ECHO), Cause::Other, "expired");
    }

    #[test]
    fn any_other_input_ends_the_echo() {
        let t0 = Instant::now();
        let mut e = DesktopEcho::default();
        e.wallpaper_clicked(t0);
        assert!(e.user_acted());
        assert!(!e.user_acted(), "nothing pending any more");
        assert_eq!(e.classify(Some(DESKTOP_OWNER), t0 + MS(100)), Cause::Other);
        e.wallpaper_clicked(t0 + MS(200));
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

        /// A wallpaper click at `ms`.
        fn wallpaper_click(&mut self, ms: u64) -> Vec<Action> {
            self.echo.wallpaper_clicked(self.at(ms));
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
}
