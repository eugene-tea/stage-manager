//! The contract between the window-manager controller and the strip panel.
//!
//! The controller (WM thread) builds a [`StripSnapshot`] from the engine after every state
//! change and hands it to the main thread through [`present`]. The strip reports user input
//! back through [`emit`]. Both ends are plain Rust values: no AX or AppKit types cross here.
//!
//! [`present`] and [`emit`] are hook points: each side registers its handler at startup
//! with [`set_presenter`] and [`set_event_sink`]. Until then they log and drop.
//!
//! **Transitions** (`[animation]`): before the controller moves windows for a switch, it
//! presents a snapshot carrying a [`Transition`] (the windows about to leave and arrive) and
//! waits ([`wait_cover`]) until the strip has drawn snapshots of them over the real windows
//! ([`cover`]); then outgoing windows park behind their snapshots (and the next snapshot says
//! so, [`StripSnapshot::parked`], so theirs start shrinking), and incoming ones are held
//! until the strip says their growing snapshots cover their frames ([`StripEvent::Unveil`]).

use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::StripStyle;

use crate::engine::corner_patch::SwitchCover;
use crate::engine::geometry::{Point, Rect};

/// Everything the strip needs to draw one frame of state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripSnapshot {
    /// Cards in slot order, top to bottom (already filtered to what should show).
    pub cards: Vec<CardSnapshot>,
    /// The stage that is active now (not normally shown as a card; Apple's strip doesn't).
    pub active_stage: Option<u64>,
    /// Apple's "Show recent apps" is off (`AutoHide = 1`): keep the strip hidden until the
    /// pointer reaches the left edge.
    pub hide_recent_apps: bool,
    /// The user's Show/Hide Strip toggle (menu or hotkey). `false` hides it outright.
    pub user_visible: bool,
    /// How the strip looks and moves (config; changes apply live).
    pub style: Arc<StripStyle>,
    /// Frames of the active stage's onscreen windows, for auto-hide when one overlaps the
    /// strip area (a window being dragged is left out).
    pub active_frames: Vec<Rect>,
    /// A managed window is being dragged by its title bar: the strip reveals itself while the
    /// pointer is over the strip area and shows a drop target (a placeholder card sized
    /// like the window's pre-drag frame). Releasing there sends the window to a stage of its
    /// own; the controller decides that from the mouse-up.
    pub window_drag: Option<CardWindow>,
    /// Where the button went down for that drag (global, top-left): the point of the window
    /// the pointer holds, so its card is held at the same point.
    pub window_drag_grab: Option<Point>,
    /// The launch's stages were just built and `[reveal] launch_reveal` is on: if a window
    /// covers the strip right now, show it for a moment anyway (once per launch).
    pub announce: bool,
    /// The Space (Desktop) these cards belong to (the engine's synthetic id). Every Space
    /// has its own strip: when it changes, the strip drops the old cards at once and shows
    /// itself on the Space now shown.
    pub space: u64,
    /// The shown Space is a full-screen app's: no strip there.
    pub fullscreen: bool,
    /// The active stage's standard windows, front to back, at their live frames: the strip
    /// keeps full-size snapshots of them ready for the next switch (`[animation]`).
    pub active_windows: Vec<CardWindow>,
    /// Windows are about to leave or arrive: animate them (this snapshot is the strip after
    /// the change).
    pub transition: Option<Transition>,
    /// The outgoing windows of this transition have parked behind their snapshots: the
    /// snapshots may start shrinking (before that a moving snapshot would uncover them).
    pub parked: Option<u64>,
    /// The incoming windows of this transition are in place, at these frames as read back
    /// from the windows (global, top-left): their snapshots land exactly there.
    pub arrived: Option<(u64, Vec<(u32, Rect)>)>,
    /// These are the cards of a Desktop not shown, arranged ahead of its arrival (a Dock
    /// click): only its own pane, if it has one, is redrawn with them, at once, so it slides
    /// in arranged; the strip shown is untouched.
    pub away: bool,
    /// The login is settling after a restart (the restore window): cards come and go in
    /// place, with no fade or glide, as windows go back to their stages one by one.
    pub quiet: bool,
}

/// A switch (or reveal, merge, drop, Cmd-M) about to happen, announced before any window
/// moves.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transition {
    /// Matches the strip's [`cover`] answer and [`StripEvent::Unveil`] to this transition.
    pub id: u64,
    /// Shown windows about to park, front to back, at their live frames; `stage` is the
    /// stage whose card (in this snapshot) they shrink into.
    pub outgoing: Vec<MovingWindow>,
    /// Parked windows about to come back, front to back, at the frames they will get;
    /// `stage` is the stage whose card (in the previous snapshot) they grow from.
    pub incoming: Vec<MovingWindow>,
    /// When the controller announced it (the strip logs how long the main thread took to
    /// get to it: a busy main thread delays the motion's start).
    pub sent: Option<std::time::Instant>,
    /// The frontmost window of the arriving stage already in place (a window just opened,
    /// an activated app's): the snapshots pass under it, not over it.
    pub under: Option<u32>,
    /// The park corner to cover while the incoming windows wait there behind their snapshots
    /// (global, top-left; the controller's [`crate::engine::corner_patch::switch_cover`]),
    /// with the corner patch's picture or a still of the desktop (round 8L3), whichever the
    /// controller found ready: macOS raises an activated app's windows above the patch.
    /// `None`: the corner is left as it is.
    pub corner: Option<SwitchCover>,
}

/// One window of a [`Transition`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MovingWindow {
    /// The window, with its frame on screen (outgoing: now; incoming: when it arrives).
    pub window: CardWindow,
    /// The stage whose card it leaves into or arrives from.
    pub stage: Option<u64>,
    /// That card's slot (0 = top), decided by the engine: for an outgoing window the slot
    /// its stage's card has after the change (this snapshot's `cards[slot]`), for an
    /// incoming one the slot its card had before (the previous snapshot's). `None`: no
    /// card (the stage is out of view).
    pub slot: Option<usize>,
}

/// The strip's answer to a [`Transition`]: what it covered with snapshots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cover {
    /// Snapshots are drawn over (some of) the outgoing windows: they may park now.
    pub outgoing: bool,
    /// Snapshots of (some of) the incoming windows are growing from their card: keep the
    /// windows parked until [`StripEvent::Unveil`].
    pub incoming: bool,
}

/// Where a window released over the strip goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StripDrop {
    /// Right above this stage's card; `None` is below the last card.
    pub before: Option<u64>,
}

/// One card: a stage that is not active.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardSnapshot {
    /// The engine's stage id (stable while the app runs).
    pub stage: u64,
    /// Hotkey number (1-based), as `⌃⌥N` addresses it.
    pub number: usize,
    /// The stage's name, if it has one.
    pub name: Option<String>,
    /// Up to three windows, front to back; the front one is drawn largest.
    pub windows: Vec<CardWindow>,
    /// App pids in icon order (first = front window's app), without duplicates.
    pub apps: Vec<i32>,
}

impl CardSnapshot {
    /// This card after `dropped`'s stage merged into it (`[strip] merge_on_drop`), as the
    /// engine's merge leaves it: its own windows in front, `dropped`'s behind them in their
    /// order, and `dropped`'s apps after its own. The strip draws it while a card is held
    /// over this one, and until the controller's next snapshot after the drop.
    pub fn merged_with(&self, dropped: &CardSnapshot) -> CardSnapshot {
        let mut card = self.clone();
        card.windows.extend(dropped.windows.iter().copied().filter(|w| !self.windows.iter().any(|o| o.id == w.id)));
        for pid in &dropped.apps {
            if !card.apps.contains(pid) {
                card.apps.push(*pid);
            }
        }
        card
    }
}

/// A window shown on a card.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CardWindow {
    /// `CGWindowID`, the key for its thumbnail.
    pub id: u32,
    /// Owning process.
    pub pid: i32,
    /// The window's saved frame (its aspect ratio sizes the thumbnail).
    pub frame: Rect,
}

/// User input on the strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StripEvent {
    /// A card was clicked (acts on mouse-up, as Apple's does). `shift` adds the card's
    /// windows to the active stage instead of switching.
    CardClicked { stage: u64, shift: bool },
    /// A card was pressed (mouse-down): a click is likely, so the controller reads the
    /// windows' frames now rather than after the release (the motion starts sooner).
    CardPressed { stage: u64 },
    /// A card was dragged out of the strip and released in the main area: its windows join
    /// the active stage, and the stage's front window goes with its top-left corner at
    /// (`x`, `y`) (global, top-left origin), so its title bar lands under the pointer.
    CardDropped { stage: u64, x: f64, y: f64 },
    /// A card's app icon was clicked: switch to the card's stage and bring every window of
    /// that app there (Apple: clicking the icon shows all of the app's windows).
    IconClicked { stage: u64, pid: i32 },
    /// A card was dragged to another slot (`[strip] reorder`): right above `before`'s card,
    /// or below the last card when `before` is `None`.
    CardMoved { stage: u64, before: Option<u64> },
    /// A card was dropped onto another card (`[strip] merge_on_drop`): `stage`'s windows join
    /// `into`'s stage, which keeps its slot.
    CardMerged { stage: u64, into: u64 },
    /// The incoming windows of transition `id` are covered by their growing snapshots: put
    /// them in place now.
    Unveil { id: u64 },
}

type Presenter = Box<dyn Fn(StripSnapshot) + Send + Sync>;
type EventSink = Box<dyn Fn(StripEvent) + Send + Sync>;

fn presenter() -> &'static Mutex<Option<Presenter>> {
    static P: OnceLock<Mutex<Option<Presenter>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(None))
}

fn event_sink() -> &'static Mutex<Option<EventSink>> {
    static S: OnceLock<Mutex<Option<EventSink>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// Registers the strip's handler for new snapshots (called by the UI at startup). The
/// handler is invoked on the caller's thread; it must hop to the main thread itself.
pub fn set_presenter(f: impl Fn(StripSnapshot) + Send + Sync + 'static) {
    if let Ok(mut p) = presenter().lock() {
        *p = Some(Box::new(f));
    }
}

/// Registers the controller's handler for strip input (called by the controller at
/// startup). Invoked on the main thread; it must not block (send a command and return).
pub fn set_event_sink(f: impl Fn(StripEvent) + Send + Sync + 'static) {
    if let Ok(mut s) = event_sink().lock() {
        *s = Some(Box::new(f));
    }
}

/// Hands a snapshot to the strip (called by the controller on any thread).
pub fn present(snapshot: StripSnapshot) {
    match presenter().lock() {
        Ok(p) => match p.as_ref() {
            Some(f) => f(snapshot),
            None => tracing::trace!(cards = snapshot.cards.len(), "strip snapshot dropped: no strip"),
        },
        Err(_) => tracing::warn!("strip presenter lock poisoned"),
    }
}

/// Reports strip input to the controller (called by the strip on the main thread).
pub fn emit(event: StripEvent) {
    match event_sink().lock() {
        Ok(s) => match s.as_ref() {
            Some(f) => f(event),
            None => tracing::debug!(?event, "strip event dropped: no controller"),
        },
        Err(_) => tracing::warn!("strip event sink lock poisoned"),
    }
}

// ---------------------------------------------------------------------------------------
// Where the cards are (for `ctl state` and scripted tests)

fn card_frames() -> &'static Mutex<Vec<(u64, Rect)>> {
    static F: OnceLock<Mutex<Vec<(u64, Rect)>>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(Vec::new()))
}

/// The strip publishes its cards' frames (stage id, global bounds, top to bottom; empty
/// while it is hidden) after every layout (main thread).
pub fn publish_cards(frames: Vec<(u64, Rect)>) {
    if let Ok(mut f) = card_frames().lock() {
        *f = frames;
    }
}

/// The cards' frames last published (any thread).
pub fn published_cards() -> Vec<(u64, Rect)> {
    card_frames().lock().map(|f| f.clone()).unwrap_or_default()
}

/// The strip panel's state as the window server and AppKit see it, for `ctl state` and the
/// guided Spaces check (a card click that found no window there).
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct PanelStatus {
    /// Slid in (not hidden by auto-hide or the user).
    pub shown: bool,
    /// Lifted above windows.
    pub lifted: bool,
    /// Ordered in (`isVisible`).
    pub ordered_in: bool,
    /// On the Space shown now (`isOnActiveSpace`).
    pub on_active_space: bool,
    /// The panel's frame, global top-left coordinates.
    pub frame: Rect,
    /// The Space id of the snapshot it shows.
    pub space: u64,
    /// The panel's window number (`CGWindowID`): the window server lists it on screen only
    /// while its Desktop shows (0: unknown).
    pub window: u32,
}

fn panel_status_slot() -> &'static Mutex<PanelStatus> {
    static S: OnceLock<Mutex<PanelStatus>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(PanelStatus::default()))
}

/// The strip publishes its panel's state after every change (main thread).
pub fn publish_panel_status(status: PanelStatus) {
    if let Ok(mut s) = panel_status_slot().lock() {
        *s = status;
    }
}

/// The panel's state last published (any thread).
pub fn panel_status() -> PanelStatus {
    panel_status_slot().lock().map(|s| *s).unwrap_or_default()
}

/// One Desktop's strip panel (the strip keeps one per Desktop visited), for `ctl state`.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct PaneStatus {
    /// The Space id of the cards it showed last.
    pub space: u64,
    /// The pane of the Desktop shown (or last shown, on a full-screen app's Space).
    pub active: bool,
    /// Ordered in (`isVisible`).
    pub ordered_in: bool,
    /// On the Space shown now (`isOnActiveSpace`).
    pub on_active_space: bool,
    /// Slid in.
    pub shown: bool,
    /// Its panel's `NSWindowCollectionBehavior` bits as AppKit reports them (see
    /// [`crate::ui::panels`]): 8 is `Transient` (hidden by Mission Control), 16 `Stationary`.
    pub behavior: usize,
}

fn pane_status_slot() -> &'static Mutex<Vec<PaneStatus>> {
    static S: OnceLock<Mutex<Vec<PaneStatus>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// The strip publishes every pane's state, the active one first (main thread).
pub fn publish_pane_statuses(panes: Vec<PaneStatus>) {
    if let Ok(mut s) = pane_status_slot().lock() {
        *s = panes;
    }
}

/// Every pane's state last published, the active one first (any thread).
pub fn pane_statuses() -> Vec<PaneStatus> {
    pane_status_slot().lock().map(|s| s.clone()).unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// What the last transition drew (for `ctl state`, the logs and scripted tests)

/// One snapshot of a transition: where it started and where it was sent, global top-left
/// coordinates. A card's plane is given by its projected bounds (what shows on screen).
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct GhostReport {
    /// The window it shows.
    pub window: u32,
    /// True for a window leaving into the strip, false for one arriving.
    pub outgoing: bool,
    /// The stage of the card it leaves into or arrives from.
    pub stage: Option<u64>,
    /// The engine's slot for that card ([`MovingWindow::slot`]).
    pub slot: Option<usize>,
    /// The slot of the card the strip aimed at (its drawn column, top = 0); equals `slot`
    /// unless the strip disagreed with the engine.
    pub aimed_slot: Option<usize>,
    /// Its first frame.
    pub start: Rect,
    /// Its last frame.
    pub end: Rect,
    /// For an incoming window: its frame read back from the window once in place.
    pub real: Option<Rect>,
}

/// The ghosts of the last transition the strip drew.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct TransitionReport {
    /// The transition.
    pub id: u64,
    /// Its snapshots.
    pub ghosts: Vec<GhostReport>,
    /// The strip's cards (stage ids, top to bottom) after it.
    pub order: Vec<u64>,
    /// The park corner its overlay covered (the patch's picture or the desktop's) while the incoming
    /// windows waited there (global, top-left), if it did.
    pub corner: Option<Rect>,
    /// What stood for the corner there: `"wallpaper"` (the patch's picture) or `"desktop"` (a
    /// still of the desktop, icons and all; round 8L3).
    pub corner_picture: Option<&'static str>,
}

fn transition_report_slot() -> &'static Mutex<Option<TransitionReport>> {
    static S: OnceLock<Mutex<Option<TransitionReport>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// The strip publishes the transition it draws (main thread).
pub fn publish_transition(report: TransitionReport) {
    if let Ok(mut s) = transition_report_slot().lock() {
        *s = Some(report);
    }
}

/// Updates the last transition's report in place (main thread), when it is still `id`.
pub fn update_transition(id: u64, f: impl FnOnce(&mut TransitionReport)) {
    if let Ok(mut s) = transition_report_slot().lock()
        && let Some(r) = s.as_mut().filter(|r| r.id == id)
    {
        f(r);
    }
}

/// The last transition drawn (any thread).
pub fn last_transition() -> Option<TransitionReport> {
    transition_report_slot().lock().ok().and_then(|s| s.clone())
}

// ---------------------------------------------------------------------------------------
// Transition handshake

/// The latest answer (transition id, what was covered) and its wake-up.
type CoverSlot = (Mutex<Option<(u64, Cover)>>, Condvar);

fn covers() -> &'static CoverSlot {
    static C: OnceLock<CoverSlot> = OnceLock::new();
    C.get_or_init(|| (Mutex::new(None), Condvar::new()))
}

/// The strip has drawn (or decided not to draw) snapshots for transition `id` (main thread).
pub fn cover(id: u64, answer: Cover) {
    let (lock, cv) = covers();
    if let Ok(mut c) = lock.lock() {
        *c = Some((id, answer));
        cv.notify_all();
    }
}

/// Waits up to `timeout` for the strip's answer to transition `id` (WM thread). No answer
/// (no strip, or a busy main thread) counts as nothing covered, so windows move at once.
pub fn wait_cover(id: u64, timeout: Duration) -> Cover {
    let (lock, cv) = covers();
    let deadline = Instant::now() + timeout;
    let Ok(mut c) = lock.lock() else { return Cover::default() };
    loop {
        if let Some((got, answer)) = *c
            && got == id
        {
            return answer;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            tracing::debug!(id, "no answer from the strip for a transition; windows move at once");
            return Cover::default();
        }
        c = match cv.wait_timeout(c, left) {
            Ok((guard, _)) => guard,
            Err(_) => return Cover::default(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merged_card_keeps_its_windows_in_front_and_adds_the_dropped_ones_behind() {
        let win = |id: u32, pid: i32| CardWindow { id, pid, frame: Rect::new(0.0, 0.0, 800.0, 600.0) };
        let target = CardSnapshot { stage: 2, number: 2, name: Some("Work".into()), windows: vec![win(5, 50), win(6, 60)], apps: vec![50, 60] };
        let dropped = CardSnapshot { stage: 7, number: 3, name: None, windows: vec![win(9, 60), win(8, 80)], apps: vec![60, 80] };
        let m = target.merged_with(&dropped);
        assert_eq!((m.stage, m.number, m.name.as_deref()), (2, 2, Some("Work")), "the target's identity");
        assert_eq!(m.windows.iter().map(|w| w.id).collect::<Vec<_>>(), [5, 6, 9, 8]);
        assert_eq!(m.apps, [50, 60, 80]);
    }

    #[test]
    fn a_cover_answer_is_matched_by_id_and_a_missing_one_times_out() {
        cover(7, Cover { outgoing: true, incoming: false });
        assert_eq!(wait_cover(7, Duration::from_millis(10)), Cover { outgoing: true, incoming: false });
        let t0 = Instant::now();
        assert_eq!(wait_cover(8, Duration::from_millis(30)), Cover::default(), "an old answer does not count");
        assert!(t0.elapsed() >= Duration::from_millis(30));
        let waiter = std::thread::spawn(|| wait_cover(9, Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(20));
        cover(9, Cover { outgoing: false, incoming: true });
        assert_eq!(waiter.join().ok(), Some(Cover { outgoing: false, incoming: true }));
    }
}
