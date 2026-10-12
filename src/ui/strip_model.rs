//! The contract between the window-manager controller and the strip panel.
//!
//! The controller (WM thread) builds a [`StripSnapshot`] from the engine after every state
//! change and hands it to the main thread through [`present`]. The strip reports user input
//! back through [`emit`]. Both ends are plain Rust values: no AX or AppKit types cross here.
//!
//! [`present`] and [`emit`] are hook points: each side registers its handler at startup
//! with [`set_presenter`] and [`set_event_sink`]. Until then they log and drop.

use std::sync::{Mutex, OnceLock};

use crate::engine::geometry::Rect;

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
    /// Draw stage numbers on cards (config `strip.show_numbers`).
    pub show_numbers: bool,
    /// Frames of the active stage's onscreen windows, for auto-hide when one overlaps the
    /// strip area (a window being dragged is left out).
    pub active_frames: Vec<Rect>,
    /// A managed window is being dragged by its title bar: the strip reveals itself while the
    /// pointer is over the strip area and shows a drop target (a placeholder card sized
    /// like the window's pre-drag frame). Releasing there sends the window to a stage of its
    /// own; the controller decides that from the mouse-up.
    pub window_drag: Option<CardWindow>,
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
    /// A card was dragged out of the strip and released in the main area: its windows join
    /// the active stage, and the stage's front window goes with its top-left corner at
    /// (`x`, `y`) (global, top-left origin), so its title bar lands under the pointer.
    CardDropped { stage: u64, x: f64, y: f64 },
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
