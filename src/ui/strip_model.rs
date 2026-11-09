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

use std::collections::BTreeMap;
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
    /// Frames of the active stage's onscreen windows, for auto-hide when one reaches the
    /// screen's left edge over the strip ([`crate::ui::strip_visibility::hides_strip`]; a
    /// window being dragged is left out).
    pub active_frames: Vec<Rect>,
    /// A managed window is being dragged by its title bar: while the pointer is over the strip
    /// area the strip shows (below the window, as Apple's) with a drop target (a placeholder
    /// card sized like the window's pre-drag frame) and the window's card follows the pointer. Releasing there sends the window to a stage of its
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
    /// The display whose strip this is (its `CGDirectDisplayID`; 0: the main display).
    /// Every display has a strip of its own (round displays).
    pub display: u32,
    /// The shown Space is a full-screen app's: no strip there.
    pub fullscreen: bool,
    /// The active stage's standard windows, front to back, at their live frames: the strip
    /// keeps full-size snapshots of them ready for the next switch (`[animation]`). Empty
    /// while the user has hidden the strip: no switch animates then, nothing is captured.
    pub active_windows: Vec<CardWindow>,
    /// The pinned windows shown on this Space, at their live frames: they stay shown through
    /// a switch, and a snapshot of each kept ready lets the switch's cover of the park corner
    /// draw one that reaches it (round 8L5). Empty while the user has hidden the strip.
    pub pinned_windows: Vec<CardWindow>,
    /// Windows are about to leave or arrive: animate them (this snapshot is the strip after
    /// the change).
    pub transition: Option<Transition>,
    /// The outgoing windows of this transition have parked behind their snapshots: the
    /// snapshots may start shrinking (before that a moving snapshot would uncover them).
    pub parked: Option<u64>,
    /// The incoming windows of this transition are in place, at these frames as read back
    /// from the windows (global, top-left): their snapshots land exactly there.
    pub arrived: Option<(u64, Vec<(u32, Rect)>)>,
    /// Late incoming windows of a transition ([`Transition::late`]) that show now.
    pub late: Option<LateNote>,
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
    /// The outgoing window that is key now (its stage's key window, else its front one): its
    /// snapshot has the key window's shadow, the others an inactive window's.
    pub key_out: Option<u32>,
    /// The incoming window the switch makes key (the batch's focus, else the front one; none
    /// when the focus goes to a window already in place).
    pub key_in: Option<u32>,
    /// Incoming windows that show only once their app is unhidden and active again after the
    /// unveil (`[windows] hide_parked_apps`: hidden in place, our unhide at the landing): their
    /// snapshots hand over and land when [`StripSnapshot::late`] says so, not at the unveil.
    pub late: Vec<u32>,
}

/// Late incoming windows of transition `id` ([`Transition::late`]) on their way to the screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LateNote {
    pub id: u64,
    /// On screen now (their app unhidden; the strip hands the shadow over once the window
    /// server shows them).
    pub shown: Vec<u32>,
    /// Drawn in their final look now (their app took the activation of its key window focused
    /// again, or none follows): their snapshots may land. Each is also shown.
    pub active: Vec<u32>,
}

impl LateNote {
    /// Adds `windows` of transition `id`, `active` or only shown; a note of an older
    /// transition is replaced.
    pub fn add(note: &mut Option<LateNote>, id: u64, windows: &[u32], active: bool) {
        let n = note.get_or_insert_with(|| LateNote { id, ..LateNote::default() });
        if n.id != id {
            *n = LateNote { id, ..LateNote::default() };
        }
        for &w in windows {
            if !n.shown.contains(&w) {
                n.shown.push(w);
            }
            if active && !n.active.contains(&w) {
                n.active.push(w);
            }
        }
    }
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

/// One card: a stage that is not active (in place, also the active stage's place:
/// [`CardSnapshot::active`], [`CardSnapshot::gap`]).
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
    /// The active stage, drawn highlighted (`[strip] order = "in_place"`, `active = "card"`).
    pub active: bool,
    /// The active stage's empty slot (`[strip] active = "gap"`): laid out at its card's size so
    /// no other card moves, never drawn, never hit.
    pub gap: bool,
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

    /// This card after its window `id` was dragged out alone (Apple's: each window of a
    /// combined card drags out on its own): its other windows in their order, and the apps
    /// they belong to. The strip draws it from the drop until the controller's next snapshot.
    pub fn without_window(&self, id: u32) -> CardSnapshot {
        let mut card = self.clone();
        card.windows.retain(|w| w.id != id);
        card.apps.retain(|pid| card.windows.iter().any(|w| w.pid == *pid));
        card
    }
}

/// Moves the item `id` says is `stage` right above `before`'s (the bottom for `None`): the
/// strip's own prediction of a card dragged to another slot, drawn at the release before the
/// controller's snapshot comes (the engine's [`crate::engine::model::Model::move_card`] comes
/// out the same in place: round 9C). True when the order changed.
pub fn move_before<T>(items: &mut Vec<T>, id: impl Fn(&T) -> u64, stage: u64, before: Option<u64>) -> bool {
    let Some(from) = items.iter().position(|c| id(c) == stage) else { return false };
    let old: Vec<u64> = items.iter().map(&id).collect();
    let item = items.remove(from);
    let to = before.and_then(|b| items.iter().position(|c| id(c) == b)).unwrap_or(items.len());
    items.insert(to, item);
    items.iter().map(&id).collect::<Vec<_>>() != old
}

/// What a card's layers need for a new snapshot of the same stage's card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardChange {
    /// Nothing drawn changed: the layers stay as they are.
    None,
    /// Only its hotkey number changed, and it is drawn (`[strip] show_numbers`): the badge's
    /// text changes in place.
    Number,
    /// Its content changed: the layers are built again.
    Rebuild,
}

impl CardSnapshot {
    /// What `self`'s layers need to show `new`, with numbers drawn or not. A stage moved in
    /// place renumbers the stages between its old and new place (numbers follow creation
    /// order), so a number alone never rebuilds a card, nor moves one (round 9C).
    pub fn change_to(&self, new: &CardSnapshot, numbers_drawn: bool) -> CardChange {
        let same = CardSnapshot { number: new.number, ..self.clone() };
        if same != *new {
            CardChange::Rebuild
        } else if self.number != new.number && numbers_drawn {
            CardChange::Number
        } else {
            CardChange::None
        }
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
    /// (`x`, `y`) (global, top-left origin), so its title bar lands under the pointer. With
    /// `window` (a card of several windows, pressed on that one; Apple's), that window alone
    /// joins and goes there; its stage keeps the others.
    CardDropped { stage: u64, window: Option<u32>, x: f64, y: f64 },
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

/// The display id the main display's strip publishes under (it is the first screen,
/// whatever that screen's id).
pub const MAIN_STRIP: u32 = 0;

/// Each display's cards (stage id, global bounds), by the display they were published under.
type CardFrames = BTreeMap<u32, Vec<(u64, Rect)>>;

fn card_frames() -> &'static Mutex<CardFrames> {
    static F: OnceLock<Mutex<CardFrames>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// A display's strip publishes its cards' frames (stage id, global bounds, top to bottom;
/// empty while it is hidden) after every layout (main thread). `display`: [`MAIN_STRIP`]
/// for the main display's strip, else the display's id (round displays).
pub fn publish_cards(display: u32, frames: Vec<(u64, Rect)>) {
    if let Ok(mut f) = card_frames().lock() {
        f.insert(display, frames);
    }
}

/// The main display's cards' frames last published (any thread).
pub fn published_cards() -> Vec<(u64, Rect)> {
    published_cards_of(MAIN_STRIP)
}

/// Every strip's cards' frames last published, every screen's (any thread): a synthetic
/// press is safe on any of them (round displays-live-1: the test's click on the second
/// screen's card was refused, its pane counted click-through as off the main strip's cards).
pub fn every_published_card() -> Vec<(u64, Rect)> {
    card_frames().lock().map(|f| f.values().flatten().copied().collect()).unwrap_or_default()
}

/// The cards' frames a display's strip last published (any thread; [`publish_cards`]).
pub fn published_cards_of(display: u32) -> Vec<(u64, Rect)> {
    card_frames().lock().ok().and_then(|f| f.get(&display).cloned()).unwrap_or_default()
}

/// Forgets the cards of displays no longer there (`kept`: the strips still up).
pub fn retain_cards_of(kept: &[u32]) {
    if let Ok(mut f) = card_frames().lock() {
        f.retain(|d, _| kept.contains(d));
    }
}

/// Each display's strip's full-size snapshots (window ids), by display.
type SnapshotIds = BTreeMap<u32, std::collections::HashSet<u32>>;

fn snapshot_ids() -> &'static Mutex<SnapshotIds> {
    static S: OnceLock<Mutex<SnapshotIds>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// A display's strip publishes the windows it holds a full-size snapshot of (main thread,
/// whenever its snapshot cache changes; `display` as [`publish_cards`]).
pub fn publish_snapshots(display: u32, ids: impl IntoIterator<Item = u32>) {
    if let Ok(mut s) = snapshot_ids().lock() {
        s.insert(display, ids.into_iter().collect());
    }
}

/// Whether a strip holds a full-size snapshot of window `id` (any thread): a switch's cover
/// can draw a window that stays shown into the park corner from it (round 8L5).
pub fn has_snapshot(id: u32) -> bool {
    snapshot_ids().lock().is_ok_and(|s| s.values().any(|ids| ids.contains(&id)))
}

fn picture_ids() -> &'static Mutex<SnapshotIds> {
    static S: OnceLock<Mutex<SnapshotIds>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// A display's strip publishes the windows whose thumbnail it holds and draws on its cards
/// (main thread, whenever one arrives or goes; `display` as [`publish_cards`]): `ctl state`
/// lists them per screen (round displays-2).
pub fn publish_pictures(display: u32, ids: impl IntoIterator<Item = u32>) {
    if let Ok(mut s) = picture_ids().lock() {
        s.insert(display, ids.into_iter().collect());
    }
}

/// The windows `display`'s strip holds a thumbnail of (any thread), sorted.
pub fn pictures_of(display: u32) -> Vec<u32> {
    let mut ids: Vec<u32> = picture_ids().lock().ok().and_then(|s| s.get(&display).map(|i| i.iter().copied().collect())).unwrap_or_default();
    ids.sort_unstable();
    ids
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

fn panel_status_slot() -> &'static Mutex<BTreeMap<u32, PanelStatus>> {
    static S: OnceLock<Mutex<BTreeMap<u32, PanelStatus>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// A strip publishes its panel's state after every change (main thread); `display` as the
/// strip is keyed ([`MAIN_STRIP`] for the main display's; round displays-2).
pub fn publish_panel_status(display: u32, status: PanelStatus) {
    if let Ok(mut s) = panel_status_slot().lock() {
        s.insert(display, status);
    }
}

/// The main display's strip panel's state last published (any thread).
pub fn panel_status() -> PanelStatus {
    panel_status_of(MAIN_STRIP)
}

/// `display`'s strip panel's state last published (any thread; [`MAIN_STRIP`] or the main
/// display's own id for the main one, which publishes as [`MAIN_STRIP`]).
pub fn panel_status_of(display: u32) -> PanelStatus {
    panel_status_slot().lock().ok().and_then(|s| s.get(&display).copied()).unwrap_or_default()
}

/// The strips of displays gone forget their statuses (`kept`: the strips' display keys
/// still there).
pub fn retain_panel_statuses_of(kept: &[u32]) {
    if let Ok(mut s) = panel_status_slot().lock() {
        s.retain(|d, _| kept.contains(d));
    }
    if let Ok(mut s) = pane_status_slot().lock() {
        s.retain(|d, _| kept.contains(d));
    }
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

fn pane_status_slot() -> &'static Mutex<BTreeMap<u32, Vec<PaneStatus>>> {
    static S: OnceLock<Mutex<BTreeMap<u32, Vec<PaneStatus>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// A strip publishes every pane's state, the active one first (main thread); `display` as
/// the strip is keyed.
pub fn publish_pane_statuses(display: u32, panes: Vec<PaneStatus>) {
    if let Ok(mut s) = pane_status_slot().lock() {
        s.insert(display, panes);
    }
}

/// The main display's strip's panes' state last published (any thread).
pub fn pane_statuses() -> Vec<PaneStatus> {
    pane_statuses_of(MAIN_STRIP)
}

/// `display`'s strip's panes' state last published (any thread).
pub fn pane_statuses_of(display: u32) -> Vec<PaneStatus> {
    pane_status_slot().lock().ok().and_then(|s| s.get(&display).cloned()).unwrap_or_default()
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
    /// For an incoming window: where its snapshot was drawn when the strip unveiled it.
    pub at_unveil: Option<Rect>,
    /// For an incoming window: when the window server showed it at its frame (ms after the
    /// unveil; `None`: not seen, the wait ran out).
    pub shown_ms: Option<u64>,
    /// For an incoming window: when its snapshot's shadow began to fade (ms after the unveil);
    /// never before `shown_ms`.
    pub handover_ms: Option<u64>,
    /// The window shadow its snapshot carries ([`ShadowReport`]); `None` for a window
    /// dragged onto the strip (its card's shadow).
    pub shadow: Option<ShadowReport>,
}

/// The window shadow a snapshot carries, for the log and `ctl state`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct ShadowReport {
    /// `"light"`, `"dark"`, or `"custom"` (`[animation] shadow = "custom"`).
    pub look: &'static str,
    /// `"active"` (the key window's) or `"inactive"`.
    pub state: &'static str,
    /// Where the appearance came from: `"window"` (its own shadow, read
    /// [`Self::read_s_ago`]), or `"system"` (no reading of it).
    pub from: &'static str,
    /// How long before the switch its shadow was read (seconds), when it was.
    pub read_s_ago: Option<u64>,
}

impl std::fmt::Display for ShadowReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.read_s_ago {
            Some(s) => write!(f, "{} {} by the window's own shadow (read {s} s ago)", self.look, self.state),
            None if self.look == "custom" => write!(f, "custom"),
            None => write!(f, "{} {} by the system's appearance (no reading of the window)", self.look, self.state),
        }
    }
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
    /// The farthest an incoming snapshot's edge travels (points; 0 with none).
    pub travel: f64,
    /// When the strip unveils the incoming windows, after the motion starts (ms), if any.
    pub unveil_ms: Option<u64>,
    /// When the motion started, after the snapshots were drawn (ms): the outgoing and the
    /// incoming snapshots start on one frame, once the outgoing windows have parked behind
    /// theirs (at once when nothing leaves).
    pub motion_ms: Option<u64>,
    /// The motion started because the wait for the outgoing windows to park ran out (200 ms),
    /// not at their note.
    pub motion_waited_out: Option<bool>,
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

    /// Round displays-live-1: a card on another screen's strip is among every strip's cards
    /// (the synthetic click's safety check), and the main strip's own list stays its own.
    #[test]
    fn every_screens_cards_count_for_a_press() {
        let other = Rect::new(-1780.0, 830.0, 80.0, 98.0);
        publish_cards(9_101, vec![(4, other)]);
        assert!(every_published_card().contains(&(4, other)));
        assert!(!published_cards().contains(&(4, other)), "the main strip's cards are its own");
        publish_cards(9_101, Vec::new());
        assert!(!every_published_card().contains(&(4, other)), "a strip hidden publishes none");
    }

    /// A late window shown at the unhide, then active at its app's activation, in one note
    /// or two; a newer transition's note replaces an older one's.
    #[test]
    fn a_late_note_gathers_shown_and_active_windows() {
        let mut n = None;
        LateNote::add(&mut n, 4, &[7], false);
        assert_eq!(n, Some(LateNote { id: 4, shown: vec![7], active: vec![] }));
        LateNote::add(&mut n, 4, &[7, 8], true);
        assert_eq!(n, Some(LateNote { id: 4, shown: vec![7, 8], active: vec![7, 8] }), "active is shown too, once each");
        LateNote::add(&mut n, 5, &[9], false);
        assert_eq!(n, Some(LateNote { id: 5, shown: vec![9], active: vec![] }));
    }

    #[test]
    fn a_number_alone_never_rebuilds_a_card() {
        let win = CardWindow { id: 5, pid: 50, frame: Rect::new(0.0, 0.0, 800.0, 600.0) };
        let card = CardSnapshot { stage: 2, number: 5, windows: vec![win], apps: vec![50], ..Default::default() };
        let renumbered = CardSnapshot { number: 6, ..card.clone() };
        assert_eq!(card.change_to(&card, true), CardChange::None);
        assert_eq!(card.change_to(&renumbered, false), CardChange::None, "no number drawn: nothing to do");
        assert_eq!(card.change_to(&renumbered, true), CardChange::Number, "the badge's text only");
        let other = CardSnapshot { windows: vec![win, CardWindow { id: 6, ..win }], ..renumbered.clone() };
        assert_eq!(card.change_to(&other, false), CardChange::Rebuild, "a window more is a change");
        let named = CardSnapshot { name: Some("Work".into()), ..card.clone() };
        assert_eq!(card.change_to(&named, false), CardChange::Rebuild);
    }

    #[test]
    fn a_card_moves_right_above_the_one_it_is_dropped_before() {
        let mut v = vec![1u64, 2, 3, 4];
        assert!(move_before(&mut v, |x| *x, 4, Some(2)));
        assert_eq!(v, [1, 4, 2, 3]);
        assert!(move_before(&mut v, |x| *x, 1, None));
        assert_eq!(v, [4, 2, 3, 1]);
        assert!(!move_before(&mut v, |x| *x, 3, Some(1)), "already right above it");
        assert!(!move_before(&mut v, |x| *x, 9, None), "no such card");
    }

    #[test]
    fn a_merged_card_keeps_its_windows_in_front_and_adds_the_dropped_ones_behind() {
        let win = |id: u32, pid: i32| CardWindow { id, pid, frame: Rect::new(0.0, 0.0, 800.0, 600.0) };
        let target = CardSnapshot { stage: 2, number: 2, name: Some("Work".into()), windows: vec![win(5, 50), win(6, 60)], apps: vec![50, 60], ..Default::default() };
        let dropped = CardSnapshot { stage: 7, number: 3, name: None, windows: vec![win(9, 60), win(8, 80)], apps: vec![60, 80], ..Default::default() };
        let m = target.merged_with(&dropped);
        assert_eq!((m.stage, m.number, m.name.as_deref()), (2, 2, Some("Work")), "the target's identity");
        assert_eq!(m.windows.iter().map(|w| w.id).collect::<Vec<_>>(), [5, 6, 9, 8]);
        assert_eq!(m.apps, [50, 60, 80]);
    }

    #[test]
    fn a_window_dragged_out_alone_leaves_the_others_and_their_apps() {
        let win = |id: u32, pid: i32| CardWindow { id, pid, frame: Rect::new(0.0, 0.0, 800.0, 600.0) };
        let card = CardSnapshot { stage: 2, number: 2, windows: vec![win(5, 50), win(6, 60), win(7, 50)], apps: vec![50, 60], ..Default::default() };
        let left = card.without_window(6);
        assert_eq!((left.stage, left.number), (2, 2));
        assert_eq!(left.windows.iter().map(|w| w.id).collect::<Vec<_>>(), [5, 7]);
        assert_eq!(left.apps, [50], "the app with no window left loses its icon");
        assert_eq!(card.without_window(5).apps, [50, 60], "an app with a window left keeps it");
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
