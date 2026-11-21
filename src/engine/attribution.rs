//! Who caused this focus or activation: ours, the user's, its app's own or macOS's.
//!
//! macOS never says who caused a notice. `Controller::handle_event` gathers what is known
//! when one comes ([`FocusFacts`], [`ActivationFacts`]) and one function here says what it
//! is ([`focus`], [`activation`]): the gates each incident added, in one written order, with
//! nothing read and nothing done in between. The bench pins the whole chain's answer for
//! each scripted incident at every load ([`judged`]; docs/DEVELOPMENT.md, "The bench").
//!
//! # The order
//!
//! The first gate that answers wins. The two notices come through two queues (a focus from
//! Accessibility, on the WM thread; an activation from NSWorkspace, stamped on the main
//! thread), so some gates exist for one only, and two sit in a different place:
//!
//! | # | A window's focus ([`focus`]) | An app's activation ([`activation`]) |
//! |---|---|---|
//! | 1 | ours: the echo ledger's answer, unless an arrangement made ahead says the focus is the user's word ([`FocusVerdict::Ours`]) | ours: the echo ledger's answer ([`ActivationVerdict::Ours`]) |
//! | 2 | its app is hidden, or not the app in front, with no button down ([`FocusVerdict::AppBehind`]) | Finder's echo of a wallpaper press ([`ActivationVerdict::WallpaperEcho`]) |
//! | 3 | macOS's after a display change ([`FocusVerdict::AfterDisplayChange`]) | stale: older than a switch we began since, or another app is in front now ([`ActivationVerdict::Stale`]) |
//! | 4 | Finder's echo of a wallpaper press ([`FocusVerdict::WallpaperEcho`]) | macOS's after a display change ([`ActivationVerdict::AfterDisplayChange`]) |
//! | 5 | its app's own while the login settles, unless a button is down ([`FocusVerdict::Settling`]) | its app's own while the login settles, unless a click ends the settling ([`ActivationVerdict::Settling`]) |
//! | 6 | on a window the switch in flight brings in ([`FocusVerdict::LandsAsPlanned`]) | (a switch in flight is superseded) |
//! | 7 | held until its Desktop is recognized ([`FocusVerdict::Held`]) | held while frozen ([`ActivationVerdict::Frozen`]) |
//! | 8 | nothing while frozen ([`FocusVerdict::Frozen`]) | |
//! | 9 | a parked window grabbed: its stage comes at the release ([`FocusVerdict::Grabbed`]) | |
//! | 10 | held for a Desktop slide ([`FocusVerdict::SlideHeld`]) | (held for a slide: `Controller::decide_activation`) |
//! | 11 | stale: its app's focus is in another stage now ([`FocusVerdict::Stale`]) | |
//! | 12 | it acts ([`FocusVerdict::Acts`]): Shift's bring-here, a pick in Mission Control, its stage | it acts ([`ActivationVerdict::Acts`]): the same, or held with nothing of the app here |
//!
//! Where the two differ, each keeps its own, as the incidents left them:
//!
//! - **Finder's echo before or after the display change.** An activation is asked about the
//!   wallpaper press before staleness and the display change; a focus after the display
//!   change. Both drop the notice, so the order shows only in which gate is named.
//! - **Staleness.** An activation carries when it fired: it is stale early, by order against
//!   our latest switch and by the app in front (3). A focus carries no stamp: "its app is
//!   not in front" is asked early (2) and "its app's focus moved on" last (11), by a read of
//!   the app that only a focus about to switch stages pays for.
//! - **The login's settling.** A button down lets a focus through even when the click does
//!   not end the settling (Accessibility lists no window yet); an activation acts only once
//!   the settling has ended.
//! - **A switch in flight.** A focus is judged against it (6: on a window it brings in, or
//!   held: it lands as planned); an activation that passed 1 to 5 supersedes it.
//!
//! # What is read when
//!
//! The facts of the notice itself are read up front: none costs, none changes anything.
//! What the chain does on the way changes what the later gates see (a click ends the login's
//! settling, a focus lands the switch in flight), and two reads cost (the window list, the
//! app's focused window), so those facts are gathered in stages, each only for a notice the
//! earlier gates let through: the function answers [`FocusVerdict::Gather`] and is asked
//! again with the stage filled in. No fact is read twice, and none before today's chain
//! read it.

use std::time::{Duration, Instant};

use super::desktop_echo::{Cause, DESKTOP_OWNER, DesktopEcho};
use super::displays::macos_after_display_change;
use super::focus_elsewhere::{FocusOnSwitch, HiddenFocus, focus_on_switch};
use super::margin;
use super::stale_activation::{self, Stale};

/// What is known of any notice when it comes, read up front.
#[derive(Clone, Copy, Debug)]
pub struct Heard {
    /// When macOS sent it (a focus: when its callback ran).
    pub at: Instant,
    /// When these facts were read.
    pub now: Instant,
    /// Since the last press of a mouse button or a key, as of [`Self::now`].
    pub since_press: Duration,
    /// The left button is down.
    pub button_down: bool,
    /// When the displays last changed (`None`: not since the launch).
    pub display_changed: Option<Instant>,
    /// How long after a display change an activation or focus with no press since is
    /// macOS's (`DISPLAY_SETTLE`).
    pub display_settle: Duration,
    /// The pending echo of the last desktop press.
    pub desktop_echo: DesktopEcho,
    /// The notice's app owns the desktop (Finder).
    pub desktop_owner: bool,
    /// The login's settling (the restore window) is open.
    pub settling: bool,
}

impl Heard {
    /// How long before the notice the last press came, for time window `name` (zero, and
    /// counted, when it came after it: [`margin::before`]).
    fn press_before(&self, name: &'static str) -> Duration {
        margin::before(name, self.since_press, self.now.saturating_duration_since(self.at))
    }

    /// macOS's own after a display change: within the settle of it, no press since, both
    /// measured to the notice.
    fn after_display_change(&self) -> bool {
        let since = self.display_changed.map(|t| margin::age("DISPLAY_SETTLE", self.at, t));
        let macos = macos_after_display_change(since, self.press_before("DISPLAY_SETTLE"), self.display_settle);
        if let Some(since) = since {
            margin::note("DISPLAY_SETTLE", since, self.display_settle, macos);
        }
        macos
    }

    /// Finder's echo of a wallpaper press.
    fn wallpaper_echo(&self) -> bool {
        self.desktop_echo.classify(self.desktop_owner.then_some(DESKTOP_OWNER), self.at) == Cause::WallpaperEcho
    }
}

/// The focused window's app, as NSWorkspace has it now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppNow {
    /// Hidden (`None`: not running).
    pub hidden: Option<bool>,
    /// The active app (`None`: not running).
    pub active: Option<bool>,
}

/// Where a focus's window stands, read once the notice is neither ours nor macOS's
/// ([`FocusStage::Place`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    /// The model has no such window.
    pub unknown: bool,
    /// It waits on a Desktop not shown.
    pub elsewhere: bool,
    /// Its screen is frozen (a full-screen app in front, a Space change settling, the lock).
    pub frozen: bool,
    /// It is not placed in a stage yet.
    pub unplaced: bool,
    /// A click or key came since the switch in flight began (true with none in flight).
    pub input_since_switch: bool,
    /// The switch in flight brings it in (or a window it is a child of).
    pub arriving: bool,
    /// What its focus asks when it waits off every Desktop ([`super::focus_elsewhere::hidden_focus`]).
    pub hidden: HiddenFocus,
}

/// Where a focus's window shows, read once the switch in flight has landed and what was
/// held is forgotten ([`FocusStage::Scene`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scene {
    /// The model has it parked.
    pub parked: bool,
    /// Its stage is not the one in front on its screen.
    pub in_another_stage: bool,
}

/// What the Focused arm knows of one focus notice.
#[derive(Clone, Copy, Debug)]
pub struct FocusFacts {
    /// The notice itself.
    pub heard: Heard,
    /// The echo ledger: we touched this window within the echo window.
    pub echo: bool,
    /// An arrangement made ahead says the focus is the user's word all the same.
    pub beats_echo: bool,
    /// Its app (`None`: the window is not tracked).
    pub app: Option<AppNow>,
    /// How long after a press a focus is the user's (`USER_FOCUS`).
    pub user_focus: Duration,
    /// [`FocusStage::Place`].
    pub place: Option<Place>,
    /// [`FocusStage::Scene`].
    pub scene: Option<Scene>,
    /// [`FocusStage::Slide`]: the window list shows a Desktop sliding.
    pub slides: Option<bool>,
    /// [`FocusStage::AppFocus`]: its app's focused window is in another stage now.
    pub moved_on: Option<bool>,
}

/// The facts a focus's verdict still needs, each read only for a notice that got so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusStage {
    /// [`Place`], after a click ended the login's settling.
    Place,
    /// [`Scene`], after the switch in flight landed and what was held is forgotten.
    Scene,
    /// The window list, for a slide ([`FocusFacts::slides`]).
    Slide,
    /// The app's focused window now ([`FocusFacts::moved_on`]).
    AppFocus,
}

/// What a focus notice is (module docs, "The order").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusVerdict {
    /// The echo of an action of ours: dropped.
    Ours,
    /// Its app is hidden or not in front: its app's own, or a late echo of ours. No switch.
    AppBehind,
    /// macOS's after a display change: no switch.
    AfterDisplayChange,
    /// Finder's, after a press on the wallpaper: no switch.
    WallpaperEcho,
    /// Its app's own while the login settles: no switch.
    Settling,
    /// Not decided yet: this stage of the facts is needed.
    Gather(FocusStage),
    /// On a window the switch in flight brings in: the switch lands as planned.
    LandsAsPlanned,
    /// Held until its window's Desktop is recognized, then replayed.
    Held {
        /// What it does to the switch in flight.
        on_switch: FocusOnSwitch,
        /// What it asks when its window waits off every Desktop.
        hidden: HiddenFocus,
    },
    /// Nothing while its screen is frozen.
    Frozen,
    /// A parked window grabbed by its sliver: its stage comes when the button is up.
    Grabbed,
    /// Held for a Desktop sliding in.
    SlideHeld,
    /// Stale: its app's focus is in another stage now.
    Stale,
    /// The user's, or its app's own: it acts (Shift's bring-here, a pick, its stage).
    Acts,
}

impl FocusVerdict {
    /// The verdict in a few words, for the trail ([`judged`]); `None` for one that acts
    /// (the arm says what it did) or is not decided yet.
    pub fn name(self) -> Option<&'static str> {
        Some(match self {
            Self::Ours => "ours",
            Self::AppBehind => "its app is behind",
            Self::AfterDisplayChange => "macOS's after a display change",
            Self::WallpaperEcho => "the wallpaper press's echo",
            Self::Settling => "the app's own while the login settles",
            Self::LandsAsPlanned => "on a window the switch brings in",
            Self::Held { .. } => "held for its Desktop",
            Self::Frozen => "nothing while frozen",
            Self::Grabbed => "a parked window grabbed",
            Self::SlideHeld => "held for a slide",
            Self::Stale => "stale",
            Self::Gather(_) | Self::Acts => return None,
        })
    }

    /// True once the notice got past the cause gates (1 to 5): not ours, not macOS's.
    pub fn passed_causes(self) -> bool {
        !matches!(self, Self::Ours | Self::AppBehind | Self::AfterDisplayChange | Self::WallpaperEcho | Self::Settling)
    }
}

/// Whether a window's focus notice may switch stages: its app is not `hidden` and is the
/// `active` app now (`None`: not running), or the left button is down (a sliver grabbed, its
/// app becoming active). A hidden app's window cannot be the user's choice, and a focus in an
/// app behind is the app's own (an app's key window changes as it is hidden, unhidden or
/// passed over) or a late echo of ours: 11-02, with Accessibility slow, such notices came up
/// to a second late, past the echo window, and each switched stages nobody asked for. An
/// activation of the app, the user's way to it, decides through its own notice.
pub fn focus_may_switch(hidden: Option<bool>, active: Option<bool>, button_down: bool) -> bool {
    hidden != Some(true) && (active == Some(true) || (active.is_some() && button_down))
}

/// True when a focus that is not ours follows a press closely enough to be the user's
/// (`USER_FOCUS`, measured to the notice): its window is touched. Asked once, for a notice
/// past gate 1.
pub fn users_focus(f: &FocusFacts) -> bool {
    margin::within("USER_FOCUS", f.heard.press_before("USER_FOCUS"), f.user_focus)
}

/// True when a focus is held until its window's Desktop is recognized: its screen frozen,
/// the window on a Desktop not shown, or not placed yet.
fn held(app_known: bool, p: &Place) -> bool {
    app_known && (p.frozen || p.elsewhere || (p.unknown && p.unplaced))
}

/// What a focus does to the switch in flight ([`focus_on_switch`]); `None` before
/// [`FocusStage::Place`] is known.
pub fn on_switch(f: &FocusFacts) -> Option<FocusOnSwitch> {
    f.place.map(|p| focus_on_switch(p.arriving, held(f.app.is_some(), &p), p.input_since_switch))
}

/// What a focus notice is, from what is known of it (module docs, "The order").
pub fn focus(f: &FocusFacts) -> FocusVerdict {
    use FocusVerdict::*;
    let h = &f.heard;
    // 1. Ours.
    if f.echo && !f.beats_echo {
        return Ours;
    }
    // 2. Its app is behind.
    if let Some(app) = f.app
        && !focus_may_switch(app.hidden, app.active, h.button_down)
    {
        return AppBehind;
    }
    // 3. macOS's after a display change.
    if h.after_display_change() {
        return AfterDisplayChange;
    }
    // 4. Finder's echo of a wallpaper press.
    if h.wallpaper_echo() {
        return WallpaperEcho;
    }
    // 5. The login settles: a click whose press is not heard yet ends it.
    if h.settling && !h.button_down {
        return Settling;
    }
    let Some(p) = f.place else { return Gather(FocusStage::Place) };
    let held = held(f.app.is_some(), &p);
    // 6. The switch in flight.
    let on_switch = focus_on_switch(p.arriving, held, p.input_since_switch);
    if on_switch == FocusOnSwitch::LandAsPlanned {
        return LandsAsPlanned;
    }
    // 7. Held for its Desktop.
    if held {
        return Held { on_switch, hidden: p.hidden };
    }
    // 8. Frozen (a window with no app known is not held).
    if p.frozen {
        return Frozen;
    }
    let Some(s) = f.scene else { return Gather(FocusStage::Scene) };
    // 9. A parked window grabbed.
    if s.parked && h.button_down {
        return Grabbed;
    }
    if f.app.is_some() && s.in_another_stage {
        // 10. A Desktop slides.
        match f.slides {
            None => return Gather(FocusStage::Slide),
            Some(true) => return SlideHeld,
            Some(false) => {}
        }
        // 11. Its app's focus moved on.
        if !h.button_down {
            match f.moved_on {
                None => return Gather(FocusStage::AppFocus),
                Some(true) => return Stale,
                Some(false) => {}
            }
        }
    } else if f.slides.is_none() {
        // The window list is read for what follows all the same.
        return Gather(FocusStage::Slide);
    }
    Acts
}

/// What the AppActivated arm knows of one activation notice.
#[derive(Clone, Copy, Debug)]
pub struct ActivationFacts {
    /// The notice itself.
    pub heard: Heard,
    /// The app.
    pub pid: i32,
    /// The echo ledger: ours.
    pub echo: bool,
    /// When the main thread got the notice.
    pub fired: Instant,
    /// When the latest switch we began started.
    pub our_switch: Option<Instant>,
    /// The frontmost app now (`None`: unknown).
    pub front: Option<i32>,
    /// [`ActivationStage::Restore`]: the login still settles after the click.
    pub still_settling: Option<bool>,
    /// [`ActivationStage::Frozen`]: the app's screens are frozen.
    pub frozen: Option<bool>,
}

/// The facts an activation's verdict still needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationStage {
    /// Whether the login still settles once the click (a button is down) tried to end it.
    Restore,
    /// Whether the app's screens are frozen, once the switch in flight is superseded.
    Frozen,
}

/// What an activation notice is (module docs, "The order").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationVerdict {
    /// The echo of an action of ours: dropped.
    Ours,
    /// Finder's, after a press on the wallpaper: no switch.
    WallpaperEcho,
    /// Stale: no longer true, or older than a switch we began since.
    Stale(Stale),
    /// macOS's after a display change: no switch.
    AfterDisplayChange,
    /// Its app's own while the login settles: no switch.
    Settling,
    /// Not decided yet: this stage of the facts is needed.
    Gather(ActivationStage),
    /// Held until the Desktop is recognized.
    Frozen,
    /// The user's (or its app's own): decided by `Controller::decide_activation`.
    Acts,
}

impl ActivationVerdict {
    /// The verdict in a few words, for the trail ([`judged`]); `None` for one that acts or
    /// is not decided yet.
    pub fn name(self) -> Option<&'static str> {
        Some(match self {
            Self::Ours => "ours",
            Self::WallpaperEcho => "the wallpaper press's echo",
            Self::Stale(Stale::BeforeOurSwitch { .. }) => "stale: older than our switch",
            Self::Stale(Stale::NotFrontmost { .. }) => "stale: another app is in front",
            Self::AfterDisplayChange => "macOS's after a display change",
            Self::Settling => "the app's own while the login settles",
            Self::Frozen => "held: frozen",
            Self::Gather(_) | Self::Acts => return None,
        })
    }

    /// True once the notice got past staleness (1 to 3): a lend made for another app is
    /// refuted by it.
    pub fn passed_staleness(self) -> bool {
        !matches!(self, Self::Ours | Self::WallpaperEcho | Self::Stale(_))
    }
}

/// What an activation notice is, from what is known of it (module docs, "The order").
pub fn activation(f: &ActivationFacts) -> ActivationVerdict {
    use ActivationVerdict::*;
    let h = &f.heard;
    // 1. Ours.
    if f.echo {
        return Ours;
    }
    // 2. Finder's echo of a wallpaper press.
    if h.wallpaper_echo() {
        return WallpaperEcho;
    }
    // 3. Stale.
    if let Some(why) = stale_activation::judge(f.pid, f.fired, f.our_switch, f.front) {
        return Stale(why);
    }
    // 4. macOS's after a display change.
    if h.after_display_change() {
        return AfterDisplayChange;
    }
    // 5. The login settles: only a click that ends it lets the activation act.
    if h.settling {
        if !h.button_down {
            return Settling;
        }
        match f.still_settling {
            None => return Gather(ActivationStage::Restore),
            Some(true) => return Settling,
            Some(false) => {}
        }
    }
    // 7. Frozen.
    match f.frozen {
        None => Gather(ActivationStage::Frozen),
        Some(true) => Frozen,
        Some(false) => Acts,
    }
}

/// The kind of notice judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A window's focus (`AXFocusedWindowChanged`), by window id.
    Focus,
    /// An app's activation (NSWorkspace), by pid.
    Activation,
}

impl Notice {
    /// The word a trail line starts with.
    pub fn word(self) -> &'static str {
        match self {
            Self::Focus => "focus",
            Self::Activation => "activation",
        }
    }
}

/// A notice of `kind` about `id` (a window id, or a pid) came to `verdict`: one trace line,
/// and under test one line of this thread's trail ([`trail`]).
pub fn judged(kind: Notice, id: i64, verdict: &'static str) {
    tracing::trace!(kind = kind.word(), id, verdict, "notice judged");
    #[cfg(test)]
    trail::note(format!("{} {id}: {verdict}", kind.word()));
}

/// Test only: what the notices handled on this thread came to, in order (the bench reads it
/// beside what a person would see).
#[cfg(test)]
pub mod trail {
    use std::cell::RefCell;

    thread_local! {
        static TRAIL: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    /// Starts keeping this thread's trail (dropping what was kept).
    pub fn start() {
        TRAIL.with(|t| *t.borrow_mut() = Some(Vec::new()));
    }

    /// The trail since [`start`], in order, still kept.
    pub fn so_far() -> Vec<String> {
        TRAIL.with(|t| t.borrow().clone().unwrap_or_default())
    }

    /// Forgets the trail so far and keeps on (a scenario's history is not its incident).
    pub fn clear() {
        TRAIL.with(|t| {
            if let Some(kept) = t.borrow_mut().as_mut() {
                kept.clear();
            }
        });
    }

    pub(super) fn note(line: String) {
        TRAIL.with(|t| {
            if let Some(kept) = t.borrow_mut().as_mut() {
                kept.push(line);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    //! The verdicts in the incidents' orders: each test sets the facts its incident's log
    //! tells and reads the gate that answers.

    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    /// A notice heard on a quiet Mac: no press for an hour, no display change, no desktop
    /// press, the login long over.
    fn heard(t0: Instant) -> Heard {
        Heard {
            at: t0,
            now: t0,
            since_press: Duration::from_secs(3600),
            button_down: false,
            display_changed: None,
            display_settle: Duration::from_secs(10),
            desktop_echo: DesktopEcho::default(),
            desktop_owner: false,
            settling: false,
        }
    }

    const FRONT: AppNow = AppNow { hidden: Some(false), active: Some(true) };
    const BEHIND: AppNow = AppNow { hidden: Some(false), active: Some(false) };
    const HERE: Place = Place { unknown: false, elsewhere: false, frozen: false, unplaced: false, input_since_switch: true, arriving: false, hidden: HiddenFocus { arrive: false, switch_back: false } };
    const IN_FRONT: Scene = Scene { parked: false, in_another_stage: false };
    const HIDDEN_STAGE: Scene = Scene { parked: true, in_another_stage: true };

    /// A focus of a window of the app in front, nothing else known yet.
    fn a_focus(t0: Instant) -> FocusFacts {
        FocusFacts { heard: heard(t0), echo: false, beats_echo: false, app: Some(FRONT), user_focus: MS(1000), place: None, scene: None, slides: None, moved_on: None }
    }

    /// Asks [`focus`] as the arm does: each stage it needs filled from `full`, in the order
    /// asked; the verdict and the stages gathered.
    fn walk(mut f: FocusFacts, full: &FocusFacts) -> (FocusVerdict, Vec<FocusStage>) {
        let mut asked = Vec::new();
        loop {
            match focus(&f) {
                FocusVerdict::Gather(stage) => {
                    assert!(!asked.contains(&stage), "{stage:?} asked twice");
                    asked.push(stage);
                    match stage {
                        FocusStage::Place => f.place = Some(full.place.expect("a place")),
                        FocusStage::Scene => f.scene = Some(full.scene.expect("a scene")),
                        FocusStage::Slide => f.slides = Some(full.slides.expect("the list")),
                        FocusStage::AppFocus => f.moved_on = Some(full.moved_on.expect("the app's focus")),
                    }
                }
                v => return (v, asked),
            }
        }
    }

    fn all(place: Place, scene: Scene, slides: bool, moved_on: bool, t0: Instant) -> FocusFacts {
        FocusFacts { place: Some(place), scene: Some(scene), slides: Some(slides), moved_on: Some(moved_on), ..a_focus(t0) }
    }

    /// 11-02: Telegram, hidden by us, posted its window's focus a second late, and the stage
    /// switched to it; Mail's, an app behind, the same. A focus switches only in the active app
    /// (or with the button down on a sliver), never in a hidden one.
    #[test]
    fn only_a_focus_in_the_app_in_front_switches() {
        assert!(focus_may_switch(Some(false), Some(true), false), "the active app's: ⌘`, a click");
        assert!(!focus_may_switch(Some(false), Some(false), false), "an app behind: its own, or a late echo");
        assert!(focus_may_switch(Some(false), Some(false), true), "a sliver grabbed, its app on its way to the front");
        assert!(!focus_may_switch(Some(true), Some(true), true), "a hidden app's window is never the user's");
        assert!(!focus_may_switch(Some(true), Some(false), false));
        assert!(!focus_may_switch(None, None, true), "the app is gone");
    }

    #[test]
    fn our_echo_is_dropped_before_anything_is_read() {
        let t0 = Instant::now();
        let f = FocusFacts { echo: true, ..a_focus(t0) };
        assert_eq!(walk(f, &f), (FocusVerdict::Ours, vec![]));
        // An arrangement made ahead: macOS focused another stage's window of the predicted
        // app within the echo window of our own moves. The user's word.
        let f = FocusFacts { echo: true, beats_echo: true, ..a_focus(t0) };
        assert_eq!(focus(&f), FocusVerdict::Gather(FocusStage::Place));
    }

    /// 11-02: with Accessibility slow, a focus notice of an app we had left came a second
    /// late, past the echo window. Its app is behind: no switch, nothing read for it.
    #[test]
    fn a_late_focus_of_an_app_behind_never_acts() {
        let t0 = Instant::now();
        let f = FocusFacts { app: Some(BEHIND), ..a_focus(t0) };
        assert_eq!(walk(f, &f), (FocusVerdict::AppBehind, vec![]));
        let hidden = FocusFacts { app: Some(AppNow { hidden: Some(true), active: Some(true) }), ..a_focus(t0) };
        assert_eq!(focus(&hidden), FocusVerdict::AppBehind);
        // A sliver grabbed: its app is becoming active, the button is down.
        let grabbed = FocusFacts { app: Some(BEHIND), heard: Heard { button_down: true, ..heard(t0) }, ..a_focus(t0) };
        assert_eq!(walk(grabbed, &all(HERE, HIDDEN_STAGE, false, false, t0)).0, FocusVerdict::Grabbed);
        // A window the observer does not track has no app to ask about: it goes on.
        let untracked = FocusFacts { app: None, ..a_focus(t0) };
        assert_eq!(focus(&untracked), FocusVerdict::Gather(FocusStage::Place));
    }

    /// 11-05 20:25:46.8: the lid closed, macOS activated and focused an app of its choosing.
    /// Within the settle of the change with no press since it is macOS's; a press after the
    /// change makes it the user's; so does the settle's end.
    #[test]
    fn a_focus_after_a_display_change_is_macos_until_a_press() {
        let t0 = Instant::now();
        let changed = |ago: u64, press_ago: u64| FocusFacts {
            heard: Heard { display_changed: Some(t0 - MS(ago)), since_press: MS(press_ago), ..heard(t0) },
            ..a_focus(t0)
        };
        assert_eq!(focus(&changed(3000, 60_000)), FocusVerdict::AfterDisplayChange);
        assert_eq!(focus(&changed(3000, 500)), FocusVerdict::Gather(FocusStage::Place), "a click since the change");
        assert_eq!(focus(&changed(11_000, 60_000)), FocusVerdict::Gather(FocusStage::Place), "past the settle");
    }

    /// The wallpaper press's echo is asked after the display change for a focus, before it
    /// (and before staleness) for an activation: the order table's first difference.
    #[test]
    fn finders_echo_sits_on_either_side_of_the_display_change() {
        let t0 = Instant::now();
        let mut echo = DesktopEcho::default();
        echo.desktop_pressed(t0 - MS(300));
        echo.released(t0 - MS(200));
        let h = Heard { desktop_echo: echo, desktop_owner: true, display_changed: Some(t0 - MS(4000)), ..heard(t0) };
        assert_eq!(focus(&FocusFacts { heard: h, ..a_focus(t0) }), FocusVerdict::AfterDisplayChange);
        let a = ActivationFacts { heard: h, ..an_activation(t0) };
        assert_eq!(activation(&a), ActivationVerdict::WallpaperEcho);
        // Another app's notice is never the echo.
        let other = Heard { desktop_owner: false, display_changed: None, ..h };
        assert_eq!(activation(&ActivationFacts { heard: other, ..an_activation(t0) }), ActivationVerdict::Gather(ActivationStage::Frozen));
    }

    /// At login a focus is its app's own; a button down (a click whose press is not heard
    /// yet) lets it through, and the arm ends the settling before the place is read.
    #[test]
    fn a_focus_while_the_login_settles_is_the_apps_own_unless_a_button_is_down() {
        let t0 = Instant::now();
        let f = FocusFacts { heard: Heard { settling: true, ..heard(t0) }, ..a_focus(t0) };
        assert_eq!(walk(f, &f), (FocusVerdict::Settling, vec![]));
        let click = FocusFacts { heard: Heard { settling: true, button_down: true, ..heard(t0) }, ..a_focus(t0) };
        assert_eq!(focus(&click), FocusVerdict::Gather(FocusStage::Place));
    }

    /// 11-19 13:17: Safari activated, the switch to its stage began, and Safari focused the
    /// window the switch brings in 11 ms later: the switch lands as planned. 11-19 13:13: it
    /// focused its window of another Desktop instead: held, the switch landing without its
    /// keyboard; after a click or key since the switch began it is superseded.
    #[test]
    fn a_focus_against_the_switch_in_flight() {
        let t0 = Instant::now();
        let arriving = Place { arriving: true, input_since_switch: false, ..HERE };
        let f = all(arriving, IN_FRONT, false, false, t0);
        assert_eq!(walk(a_focus(t0), &f), (FocusVerdict::LandsAsPlanned, vec![FocusStage::Place]));
        let elsewhere = Place { elsewhere: true, input_since_switch: false, ..HERE };
        let v = walk(a_focus(t0), &all(elsewhere, IN_FRONT, false, false, t0)).0;
        assert_eq!(v, FocusVerdict::Held { on_switch: FocusOnSwitch::LandWithoutFocus, hidden: HiddenFocus::default() });
        let after_input = Place { elsewhere: true, input_since_switch: true, ..HERE };
        let v = walk(a_focus(t0), &all(after_input, IN_FRONT, false, false, t0)).0;
        assert_eq!(v, FocusVerdict::Held { on_switch: FocusOnSwitch::Supersede, hidden: HiddenFocus::default() });
    }

    /// 11-20 07:53:39 (the Spaces engine): the window focused waits off every Desktop; its
    /// focus is held and asks for its Desktop's arrangement and the switch back.
    #[test]
    fn a_held_focus_carries_what_a_hidden_window_asks() {
        let t0 = Instant::now();
        let hidden = HiddenFocus { arrive: true, switch_back: true };
        let p = Place { elsewhere: true, hidden, ..HERE };
        let v = walk(a_focus(t0), &all(p, IN_FRONT, false, false, t0)).0;
        assert_eq!(v, FocusVerdict::Held { on_switch: FocusOnSwitch::Supersede, hidden });
    }

    #[test]
    fn frozen_holds_a_known_apps_focus_and_drops_an_unknown_ones() {
        let t0 = Instant::now();
        let p = Place { frozen: true, ..HERE };
        let v = walk(a_focus(t0), &all(p, IN_FRONT, false, false, t0)).0;
        assert!(matches!(v, FocusVerdict::Held { .. }), "{v:?}");
        let untracked = FocusFacts { app: None, ..a_focus(t0) };
        assert_eq!(walk(untracked, &all(p, IN_FRONT, false, false, t0)).0, FocusVerdict::Frozen);
    }

    /// 10-23 (`slide_hold`): a focus that would switch stages while the list shows a Desktop
    /// sliding is held for the Space change; the app is not asked.
    #[test]
    fn a_focus_during_a_slide_is_held_before_its_app_is_asked() {
        let t0 = Instant::now();
        let f = all(HERE, HIDDEN_STAGE, true, true, t0);
        assert_eq!(walk(a_focus(t0), &f), (FocusVerdict::SlideHeld, vec![FocusStage::Place, FocusStage::Scene, FocusStage::Slide]));
    }

    /// Round stale-activations (the 11-03 class at the focus gate): a late focus of a window
    /// in a hidden stage, its app in front and its focus in another stage now, never
    /// switches; with the app's focus still there, it acts. A window of the stage in front
    /// costs no question.
    #[test]
    fn a_stale_focus_is_told_by_asking_its_app() {
        let t0 = Instant::now();
        let every = vec![FocusStage::Place, FocusStage::Scene, FocusStage::Slide, FocusStage::AppFocus];
        assert_eq!(walk(a_focus(t0), &all(HERE, HIDDEN_STAGE, false, true, t0)), (FocusVerdict::Stale, every.clone()));
        assert_eq!(walk(a_focus(t0), &all(HERE, HIDDEN_STAGE, false, false, t0)), (FocusVerdict::Acts, every));
        let in_front = walk(a_focus(t0), &all(HERE, IN_FRONT, false, true, t0));
        assert_eq!(in_front, (FocusVerdict::Acts, vec![FocusStage::Place, FocusStage::Scene, FocusStage::Slide]));
        // The button down (a click on it): the user's, the app is not asked.
        let click = FocusFacts { heard: Heard { button_down: true, ..heard(t0) }, ..a_focus(t0) };
        let shown_elsewhere = Scene { parked: false, in_another_stage: true };
        let v = walk(click, &all(HERE, shown_elsewhere, false, true, t0));
        assert_eq!(v, (FocusVerdict::Acts, vec![FocusStage::Place, FocusStage::Scene, FocusStage::Slide]));
    }

    #[test]
    fn a_focus_is_the_users_within_a_second_of_a_press_measured_to_the_notice() {
        let t0 = Instant::now();
        let pressed = |ago: u64| FocusFacts { heard: Heard { since_press: MS(ago), ..heard(t0) }, ..a_focus(t0) };
        assert!(users_focus(&pressed(300)));
        assert!(!users_focus(&pressed(1200)));
        // Handled 2 s late: the press 2.3 s ago came 300 ms before the notice.
        let late = FocusFacts { heard: Heard { at: t0, now: t0 + MS(2000), since_press: MS(2300), ..heard(t0) }, ..a_focus(t0) };
        assert!(users_focus(&late));
    }

    /// An activation of app 7 heard on a quiet Mac, app 7 in front.
    fn an_activation(t0: Instant) -> ActivationFacts {
        ActivationFacts { heard: heard(t0), pid: 7, echo: false, fired: t0, our_switch: None, front: Some(7), still_settling: None, frozen: None }
    }

    /// 11-03 14:09:57 (`stale_activation`): the Mac's load at ~110, a card click's own
    /// activation handled after the next card's switch began. Older than our switch: stale,
    /// whatever else is true of it. One that fired after the switch, its app in front, acts.
    #[test]
    fn an_activation_older_than_our_switch_is_stale() {
        let t0 = Instant::now();
        let late = ActivationFacts { our_switch: Some(t0 + MS(400)), ..an_activation(t0) };
        assert_eq!(activation(&late), ActivationVerdict::Stale(Stale::BeforeOurSwitch { by: MS(400) }));
        let newer = ActivationFacts { our_switch: Some(t0 - MS(400)), frozen: Some(false), ..an_activation(t0) };
        assert_eq!(activation(&newer), ActivationVerdict::Acts);
        let passed_over = ActivationFacts { front: Some(9), ..an_activation(t0) };
        assert_eq!(activation(&passed_over), ActivationVerdict::Stale(Stale::NotFrontmost { front: 9 }));
    }

    /// 11-14 (the echo ledger's awaited path) and the order: ours wins over everything, a
    /// stale notice over the display change (the lend's refutation runs for neither).
    #[test]
    fn ours_first_then_stale_then_the_display_change() {
        let t0 = Instant::now();
        let h = Heard { display_changed: Some(t0 - MS(2000)), ..heard(t0) };
        let ours = ActivationFacts { heard: h, echo: true, front: Some(9), ..an_activation(t0) };
        assert_eq!(activation(&ours), ActivationVerdict::Ours);
        assert!(!activation(&ours).passed_staleness());
        let stale = ActivationFacts { heard: h, front: Some(9), ..an_activation(t0) };
        assert!(matches!(activation(&stale), ActivationVerdict::Stale(_)));
        let macos = ActivationFacts { heard: h, ..an_activation(t0) };
        assert_eq!(activation(&macos), ActivationVerdict::AfterDisplayChange);
        assert!(activation(&macos).passed_staleness());
    }

    /// At login an activation is its app's own. A Dock click whose press is not heard yet
    /// (the button is down) tries to end the settling; the activation acts only when it did.
    #[test]
    fn an_activation_while_the_login_settles_acts_only_once_a_click_ended_it() {
        let t0 = Instant::now();
        let settling = ActivationFacts { heard: Heard { settling: true, ..heard(t0) }, ..an_activation(t0) };
        assert_eq!(activation(&settling), ActivationVerdict::Settling);
        let click = ActivationFacts { heard: Heard { settling: true, button_down: true, ..heard(t0) }, ..an_activation(t0) };
        assert_eq!(activation(&click), ActivationVerdict::Gather(ActivationStage::Restore));
        assert_eq!(activation(&ActivationFacts { still_settling: Some(true), ..click }), ActivationVerdict::Settling);
        let ended = ActivationFacts { still_settling: Some(false), ..click };
        assert_eq!(activation(&ended), ActivationVerdict::Gather(ActivationStage::Frozen));
        assert_eq!(activation(&ActivationFacts { frozen: Some(true), ..ended }), ActivationVerdict::Frozen);
        assert_eq!(activation(&ActivationFacts { frozen: Some(false), ..ended }), ActivationVerdict::Acts);
    }
}
