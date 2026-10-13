//! The strip's motion: full-size window snapshots, the switch animation and the card of a
//! window dragged over the strip, drawn in the [`Overlay`] above every window.
//!
//! - **Snapshots.** Apple animates the live windows; we cannot, so we move pictures of them.
//!   A second capturer keeps full-size snapshots of the active stage's windows (every
//!   `[animation] snapshot_interval_ms`, and at once when the pointer reaches a card or presses
//!   one, so they are fresh when a switch starts), plus the hovered card's windows. A window's
//!   last snapshot before it parks stays in the cache (`snapshot_cache_mb`) and is what grows
//!   back when its stage returns; a card's thumbnail stands in when there is none.
//! - **Transitions.** When the controller announces one ([`Transition`]), outgoing windows'
//!   snapshots appear exactly over them and shrink into their card's slot on the spring;
//!   incoming windows' snapshots grow from their card to their frames. The slot is the
//!   engine's ([`MovingWindow::slot`], `ui::transition_plan`), never one the strip works out
//!   on its own; once the incoming windows are in place their frames are read back and their
//!   snapshots finish exactly there. Each snapshot's start, end and slot are logged and kept
//!   for `ctl state` ([`strip_model::last_transition`]). The strip answers
//!   with what it covered ([`strip_model::cover`]); the controller parks the outgoing
//!   windows behind their snapshots and holds the incoming ones until
//!   [`StripEvent::Unveil`] (at `unpark_at` of the motion). The outgoing stage's card stays
//!   hidden until its snapshots land, then everything goes in one frame. A new transition
//!   cuts a running one short.
//! - **A window dragged over the strip** is shown as a card-sized snapshot, turned like a
//!   card, under the pointer (following every drag event through a global monitor); dropped
//!   there, it shrinks into its new card's slot as the transition's outgoing snapshot.

use std::collections::HashMap;
use std::ptr::NonNull;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{MainThreadMarker, NSRunLoop, NSRunLoopCommonModes, NSTimer};

use super::{Ghost as CardGhost, Strip, with_strip};
use crate::engine::geometry::{Point, Rect, Size};
use crate::platform::capture::{Capturer, Target};
use crate::ui::motion_math::{Plane, SnapshotCache, Spring, settle_time, time_to_progress};
use crate::ui::overlay::{Ghost, Overlay};
use crate::ui::strip_layout::CardLayout;
use crate::ui::strip_model::{self, Cover, GhostReport, MovingWindow, StripEvent, Transition, TransitionReport};

/// The interval used when `snapshot_interval_ms = 0` (captures only on demand).
const ON_DEMAND: Duration = Duration::from_secs(24 * 3600);
/// How close to its target the spring must stay before a transition's snapshots go.
const SETTLED: f64 = 0.002;
/// A card dragged out keeps its ghost on screen at most this long while its windows'
/// transition is on its way.
const LANDING_MAX: f64 = 1.0;
/// Outgoing snapshots start shrinking after this at the latest, parked or not (seconds).
const PARK_WAIT_MAX: f64 = 0.2;

/// A running transition.
struct Running {
    id: u64,
    ghosts: Vec<Ghost>,
    /// Outgoing snapshots waiting for their windows to park: (index in `ghosts`, card plane).
    waiting: Vec<(usize, Plane)>,
    /// Cards hidden until their snapshots land.
    held: Vec<u64>,
    timers: Vec<Retained<NSTimer>>,
}

/// The card shown under the pointer while a window is dragged over the strip.
struct WindowCard {
    window: u32,
    ghost: Ghost,
    has_image: bool,
}

/// A card dragged out and released: its ghost stays until its windows' snapshots take over.
struct Landing {
    _ghost: CardGhost,
    window: u32,
    rect: Rect,
    _timer: Retained<NSTimer>,
}

/// Motion state kept by the strip.
pub(super) struct Motion {
    overlay: Overlay,
    cache: SnapshotCache<CFRetained<CGImage>>,
    capturer: Capturer,
    interval_ms: Option<u64>,
    running: Option<Running>,
    window_card: Option<WindowCard>,
    landing: Option<Landing>,
    /// The pointer when the current window drag was first seen, and the window.
    drag_seen: Option<(u32, Point)>,
    drag_monitor: Option<Retained<AnyObject>>,
}

impl Motion {
    pub(super) fn new(mtm: MainThreadMarker) -> Self {
        Self {
            overlay: Overlay::new(mtm),
            cache: SnapshotCache::new(512 << 20),
            capturer: Capturer::start_quiet(ON_DEMAND, super::snapshot_arrived),
            interval_ms: None,
            running: None,
            window_card: None,
            landing: None,
            drag_seen: None,
            drag_monitor: None,
        }
    }

    /// True while `stage`'s card waits for its transition's snapshots to land.
    pub(super) fn holds(&self, stage: u64) -> bool {
        self.running.as_ref().is_some_and(|r| r.held.contains(&stage))
    }

    /// The user went to another Space: an idle overlay gets a fresh panel, which goes to the
    /// Space shown when it is first ordered in (a panel ordered in before keeps going back
    /// to the Space it was on).
    pub(super) fn space_changed(&mut self, mtm: MainThreadMarker) {
        if self.running.is_none() && self.window_card.is_none() {
            self.overlay.renew(mtm);
        }
    }

    /// A thumbnail arrived: the dragged window's card uses it while it has no snapshot.
    pub(super) fn thumbnail_arrived(&mut self, id: u32, image: &CGImage) {
        if let Some(c) = self.window_card.as_mut().filter(|c| c.window == id && !c.has_image) {
            c.ghost.set_image(image);
            c.has_image = true;
        }
    }

    /// Keeps a card's drag ghost (released in the main area at `rect`, showing `window`) until
    /// the transition of its windows takes over.
    pub(super) fn keep_landing(&mut self, ghost: Option<CardGhost>, window: u32, rect: Rect) {
        let Some(ghost) = ghost else { return };
        let timer = after(LANDING_MAX, || {
            with_strip(|s| s.motion.landing = None);
        });
        self.landing = Some(Landing { _ghost: ghost, window, rect, _timer: timer });
    }
}

impl Drop for Motion {
    fn drop(&mut self) {
        if let Some(token) = self.drag_monitor.take() {
            // SAFETY: the token came from addGlobalMonitorForEventsMatchingMask.
            unsafe { NSEvent::removeMonitor(&token) };
        }
    }
}

/// Runs `f` once on the main run loop after `seconds`, in every mode (also while a menu
/// tracks the pointer).
fn after(seconds: f64, f: impl Fn() + 'static) -> Retained<NSTimer> {
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| f());
    // SAFETY: the block runs on the main run loop only.
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(seconds.max(0.0), false, &block) };
    // SAFETY: the main run loop and a framework mode constant.
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    timer
}

impl Strip {
    /// True when the motion overlay draws top-down (see `overlay`).
    pub(super) fn motion_top_down(&self) -> bool {
        self.motion.overlay.top_down()
    }

    /// The motion overlay's window number and its top-left corner (global), while it shows.
    pub(super) fn motion_overlay(&self) -> Option<(isize, Point)> {
        self.motion.overlay.shown().then(|| (self.motion.overlay.window_number(), self.motion.overlay.origin()))
    }

    fn spring(&self) -> Spring {
        let a = &self.style.animation;
        Spring { stiffness: a.spring_stiffness, damping: a.spring_damping, mass: a.spring_mass }
    }

    /// Lays the overlay over the main display with the strip's eye.
    fn place_overlay(&mut self) {
        let g = &self.geometry;
        let eye = Point::new(g.global.x + g.m.eye.x, g.global.y + g.m.eye.y);
        self.motion.overlay.place(g.screen, g.primary_height, eye, g.m.perspective);
    }

    /// A card thumbnail's plane in overlay coordinates (slid out with the strip when hidden).
    fn thumb_plane(&self, layout: &CardLayout, index: usize) -> Option<Plane> {
        let slot = layout.thumbs.get(index)?;
        let m = self.m();
        let o = self.motion.overlay.origin();
        let g = self.geometry.global;
        let x = g.x - o.x + layout.frame.x + slot.rect.x + self.current_dx();
        let top = g.y - o.y + layout.frame.y + slot.rect.y;
        let mut plane = Plane::thumb(x, top, slot.rect.size(), m.tilt, self.style.cards.corner_radius * m.k);
        if self.hovered == Some(layout.stage) {
            let c = layout.hover_center(m);
            let c = Point::new(g.x - o.x + c.x + self.current_dx(), g.y - o.y + c.y);
            let s = m.hover_scale;
            plane.left_mid = Point::new(c.x + (plane.left_mid.x - c.x) * s, c.y + (plane.left_mid.y - c.y) * s);
            plane.size = Size::new(plane.size.w * s, plane.size.h * s);
        }
        Some(plane)
    }

    /// The card a moving window leaves into or arrives from in `layouts`: the engine's slot
    /// for it ([`MovingWindow::slot`]) when the card there is its stage's, else the card
    /// showing the window, else its stage's card (a disagreement is logged: it means the
    /// strip draws another order than the engine decided). With the card's slot.
    fn card_for<'a>(&self, layouts: &'a [CardLayout], w: &MovingWindow) -> Option<(usize, &'a CardLayout)> {
        let cards: Vec<(usize, &CardLayout)> = layouts.iter().filter(|l| l.stage != super::DROP_STAGE).enumerate().collect();
        if let Some(&(i, l)) = w.slot.and_then(|s| cards.get(s)).filter(|(_, l)| Some(l.stage) == w.stage) {
            return Some((i, l));
        }
        let found = cards
            .iter()
            .find(|(_, l)| l.thumbs.iter().any(|t| t.id == w.window.id))
            .or_else(|| cards.iter().find(|(_, l)| Some(l.stage) == w.stage))
            .copied();
        if w.slot.is_some() || found.is_some() {
            tracing::warn!(
                window = w.window.id,
                stage = ?w.stage,
                engine_slot = ?w.slot,
                strip_slot = ?found.map(|f| f.0),
                order = ?cards.iter().map(|(_, l)| l.stage).collect::<Vec<_>>(),
                "transition: the strip's card for a window is not in the engine's slot"
            );
        }
        found
    }

    /// Where window `id` sits on `layout`: its own thumbnail, else the card's front one.
    fn plane_on(&self, layout: &CardLayout, id: u32) -> Option<Plane> {
        let i = layout.thumbs.iter().position(|t| t.id == id).unwrap_or(0);
        self.thumb_plane(layout, i)
    }

    /// A plane's bounds on screen (global, top-left): a flat one exactly, a turned one as
    /// projected from the strip's eye.
    fn plane_rect(&self, p: &Plane) -> Rect {
        let o = self.motion.overlay.origin();
        let top = p.left_mid.y - p.size.h / 2.0;
        if p.tilt == 0.0 {
            return Rect::new(p.left_mid.x + o.x, top + o.y, p.size.w, p.size.h);
        }
        let g = self.geometry.global;
        let b = self.m().projected_bounds(p.left_mid.x + o.x - g.x, top + o.y - g.y, p.size.w, p.size.h);
        b.translate(g.x, g.y)
    }

    /// A card-sized plane for a window with no card in view: at the strip's cards' left edge,
    /// level with the window, slid out to the left.
    fn offscreen_plane(&self, frame: Rect) -> Plane {
        let m = self.m();
        let o = self.motion.overlay.origin();
        let g = self.geometry.global;
        let size = m.thumb_size(&frame);
        let middle = (frame.y + frame.h / 2.0).clamp(g.y + size.h / 2.0, g.max_y() - size.h / 2.0);
        Plane::thumb(g.x - o.x + m.card_x + self.hidden_dx(), middle - o.y - size.h / 2.0, size, m.tilt, self.style.cards.corner_radius * m.k)
    }

    /// The planes incoming windows grow from, with the slot of the card each grows from:
    /// their cards as drawn now (before the transition's snapshot replaces them), or the
    /// ghost of a card just dragged out.
    pub(super) fn incoming_starts(&mut self, t: &Transition) -> HashMap<u32, (Plane, Option<usize>)> {
        self.place_overlay();
        let o = self.motion.overlay.origin();
        let landing = self.motion.landing.as_ref().map(|l| (l.window, l.rect));
        t.incoming
            .iter()
            .map(|w| {
                let id = w.window.id;
                let start = match landing {
                    Some((lw, rect)) if lw == id => (Plane::window(rect, o, self.style.cards.corner_radius * self.m().k), None),
                    _ => match self.card_for(&self.layouts, w) {
                        Some((slot, l)) => match self.plane_on(l, id) {
                            Some(p) => (p, Some(slot)),
                            None => (self.offscreen_plane(w.window.frame), None),
                        },
                        None => (self.offscreen_plane(w.window.frame), None),
                    },
                };
                (id, start)
            })
            .collect()
    }

    /// Starts transition `t`: snapshots over the outgoing windows shrink into their cards,
    /// snapshots of the incoming ones grow from `starts`; tells the controller what is covered.
    pub(super) fn begin_transition(&mut self, t: Transition, starts: HashMap<u32, (Plane, Option<usize>)>) {
        self.finish_transition();
        if !self.style.animation.enabled {
            strip_model::cover(t.id, Cover::default());
            return;
        }
        self.place_overlay();
        let style = self.style.animation.clone();
        let spring = self.spring();
        let o = self.motion.overlay.origin();
        let mut cover = Cover::default();
        let mut ghosts = Vec::new();
        let mut waiting = Vec::new();
        let mut held = Vec::new();
        let mut report = Vec::new();
        for w in &t.outgoing {
            let id = w.window.id;
            let (end, card, aimed) = match self.card_for(&self.layouts, w).and_then(|(slot, l)| self.plane_on(l, id).map(|p| (p, l.stage, slot))) {
                Some((p, stage, slot)) => (p, Some(stage), Some(slot)),
                None => (self.offscreen_plane(w.window.frame), None, None),
            };
            // A window dropped on the strip: its card under the pointer becomes the snapshot.
            let dragged = self.motion.window_card.take_if(|c| c.window == id);
            let ghost = match dragged {
                Some(c) => c.ghost,
                None => {
                    let Some(image) = self.motion.cache.get(id).cloned() else {
                        tracing::debug!(id, "no snapshot of an outgoing window; it just goes");
                        continue;
                    };
                    self.motion.overlay.ghost(id, Some(&image), Plane::window(w.window.frame, o, style.corner_radius), &style, 1.0)
                }
            };
            // It holds still over the window until the window has parked behind it.
            let start = Plane::window(w.window.frame, o, style.corner_radius);
            report.push(GhostReport {
                window: id,
                outgoing: true,
                stage: w.stage,
                slot: w.slot,
                aimed_slot: aimed,
                start: self.plane_rect(&start),
                end: self.plane_rect(&end),
                real: None,
            });
            waiting.push((ghosts.len(), end));
            ghosts.push(ghost);
            cover.outgoing = true;
            if let Some(stage) = card.filter(|s| !held.contains(s)) {
                held.push(stage);
            }
        }
        for w in &t.incoming {
            let id = w.window.id;
            let image = self.motion.cache.get(id).cloned().or_else(|| self.thumbs.get(&id).cloned());
            let (Some(image), Some((start, from))) = (image, starts.get(&id).copied()) else {
                tracing::debug!(id, "no snapshot of an incoming window; it just comes");
                continue;
            };
            let mut ghost = self.motion.overlay.ghost(id, Some(&image), start, &style, 1.0);
            let end = Plane::window(w.window.frame, o, style.corner_radius);
            ghost.spring_to(end, spring);
            report.push(GhostReport {
                window: id,
                outgoing: false,
                stage: w.stage,
                slot: w.slot,
                aimed_slot: from,
                start: self.plane_rect(&start),
                end: self.plane_rect(&end),
                real: None,
            });
            ghosts.push(ghost);
            cover.incoming = true;
        }
        if ghosts.is_empty() {
            strip_model::cover(t.id, cover);
            return;
        }
        for stage in &held {
            if let Some(layers) = self.pane.cards.get(stage) {
                layers.card.removeAnimationForKey(&objc2_foundation::NSString::from_str("fade"));
                layers.card.setOpacity(0.0);
            }
        }
        self.motion.overlay.show();
        self.motion.overlay.flush();
        self.motion.landing = None;
        strip_model::cover(t.id, cover);
        let id = t.id;
        let mut timers = Vec::new();
        if cover.incoming {
            let unveil_at = time_to_progress(spring, style.unpark_at);
            timers.push(after(unveil_at, move || strip_model::emit(StripEvent::Unveil { id })));
        }
        if waiting.is_empty() {
            timers.push(after(settle_time(spring, SETTLED), move || {
                with_strip(|s| s.end_transition(id));
            }));
        } else {
            timers.push(after(PARK_WAIT_MAX, move || {
                with_strip(|s| s.outgoing_parked(id));
            }));
        }
        tracing::debug!(id, ghosts = ghosts.len(), held = ?held, ?cover, "transition");
        for g in &report {
            tracing::debug!(
                id,
                window = g.window,
                outgoing = g.outgoing,
                stage = ?g.stage,
                from_slot = ?if g.outgoing { None } else { g.aimed_slot },
                to_slot = ?if g.outgoing { g.aimed_slot } else { None },
                engine_slot = ?g.slot,
                start = %g.start,
                end = %g.end,
                "transition ghost"
            );
        }
        let order: Vec<u64> = self.snapshot.cards.iter().map(|c| c.stage).collect();
        strip_model::publish_transition(TransitionReport { id, ghosts: report, order });
        self.motion.running = Some(Running { id, ghosts, waiting, held, timers });
    }

    /// The incoming windows of transition `id` are in place at `frames` (read back from the
    /// windows): a snapshot still headed elsewhere goes the rest of the way there, so it
    /// lands exactly on its window.
    pub(super) fn incoming_arrived(&mut self, id: u64, frames: &[(u32, Rect)]) {
        let spring = self.spring();
        let o = self.motion.overlay.origin();
        let corner = self.style.animation.corner_radius;
        if let Some(r) = self.motion.running.as_mut().filter(|r| r.id == id) {
            for (window, real) in frames {
                let to = Plane::window(*real, o, corner);
                for g in r.ghosts.iter_mut().filter(|g| g.window == *window && g.target.tilt == 0.0) {
                    let t = g.target;
                    let off = (t.left_mid.x - to.left_mid.x).abs().max((t.left_mid.y - to.left_mid.y).abs()).max((t.size.w - to.size.w).abs()).max((t.size.h - to.size.h).abs());
                    tracing::debug!(id, window, real = %real, off, "transition: ghost end vs the window's real frame");
                    if off > 0.25 {
                        g.spring_to(to, spring);
                    }
                }
            }
        }
        strip_model::update_transition(id, |r| {
            for g in r.ghosts.iter_mut().filter(|g| !g.outgoing) {
                if let Some((_, real)) = frames.iter().find(|(w, _)| *w == g.window) {
                    g.real = Some(*real);
                    g.end = *real;
                }
            }
        });
    }

    /// The outgoing windows of transition `id` have parked: their snapshots shrink into their
    /// cards now, and the transition ends when they land.
    pub(super) fn outgoing_parked(&mut self, id: u64) {
        let spring = self.spring();
        let Some(r) = self.motion.running.as_mut().filter(|r| r.id == id && !r.waiting.is_empty()) else { return };
        for (i, end) in std::mem::take(&mut r.waiting) {
            if let Some(g) = r.ghosts.get_mut(i) {
                g.spring_to(end, spring);
            }
        }
        r.timers.push(after(settle_time(spring, SETTLED), move || {
            with_strip(|s| s.end_transition(id));
        }));
    }

    /// Transition `id` has landed: the snapshots go and the held cards show, in one frame.
    fn end_transition(&mut self, id: u64) {
        if self.motion.running.as_ref().is_some_and(|r| r.id == id) {
            self.finish_transition();
        }
    }

    /// Ends the running transition now (it landed, or a new one starts).
    fn finish_transition(&mut self) {
        let Some(r) = self.motion.running.take() else { return };
        for t in &r.timers {
            t.invalidate();
        }
        objc2_quartz_core::CATransaction::begin();
        objc2_quartz_core::CATransaction::setDisableActions(true);
        for g in &r.ghosts {
            g.remove();
        }
        for stage in &r.held {
            if let Some(layers) = self.pane.cards.get(stage) {
                layers.card.setOpacity(1.0);
            }
        }
        objc2_quartz_core::CATransaction::commit();
        if self.motion.window_card.is_none() {
            self.motion.overlay.hide();
        }
    }

    /// After every snapshot: what to keep snapshots of, the drag monitor, a stale landing.
    pub(super) fn after_snapshot(&mut self) {
        let a = self.style.animation.clone();
        let interval = (a.snapshot_interval_ms > 0).then_some(a.snapshot_interval_ms);
        if self.motion.interval_ms != Some(interval.unwrap_or(0)) {
            self.motion.interval_ms = Some(interval.unwrap_or(0));
            self.motion.capturer.set_interval(interval.map_or(ON_DEMAND, Duration::from_millis));
        }
        self.motion.cache.set_budget((a.snapshot_cache_mb as usize) << 20);
        let known: Vec<u32> = self
            .snapshot
            .active_windows
            .iter()
            .chain(self.snapshot.cards.iter().flat_map(|c| c.windows.iter()))
            .map(|w| w.id)
            .collect();
        let running: Vec<u32> = self.motion.running.as_ref().map(|r| r.ghosts.iter().map(|g| g.window).collect()).unwrap_or_default();
        self.motion.cache.retain(|id| known.contains(&id) || running.contains(&id));
        self.update_snapshot_targets(None, false);
        if let Some(l) = &self.motion.landing
            && !self.snapshot.cards.iter().any(|c| c.windows.iter().any(|w| w.id == l.window))
            && self.motion.running.is_none()
            && self.snapshot.transition.is_none()
        {
            // The card's windows joined the stage without a transition (animation off).
            self.motion.landing = None;
        }
        let dragging = self.snapshot.window_drag.is_some();
        match (dragging, self.motion.drag_monitor.is_some()) {
            (true, false) => {
                self.motion.drag_seen = self.snapshot.window_drag.map(|w| (w.id, self.pointer()));
                let block = RcBlock::new(|_e: NonNull<NSEvent>| {
                    with_strip(Strip::update_visibility);
                });
                self.motion.drag_monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::LeftMouseDragged, &block);
            }
            (false, true) => {
                if let Some(token) = self.motion.drag_monitor.take() {
                    // SAFETY: the token came from addGlobalMonitorForEventsMatchingMask.
                    unsafe { NSEvent::removeMonitor(&token) };
                }
                self.motion.drag_seen = None;
            }
            _ => {}
        }
    }

    /// Captures the windows that may move next: the active stage's, and `card`'s (hovered or
    /// pressed), now.
    pub(super) fn prefetch(&mut self, card: Option<u64>) {
        self.update_snapshot_targets(card, true);
    }

    /// Tells the snapshot capturer what to keep: the active stage's windows and those of
    /// `card` (or the hovered card), at full size. `now` captures them at once.
    fn update_snapshot_targets(&mut self, card: Option<u64>, now: bool) {
        if !self.style.animation.enabled {
            self.motion.capturer.set_targets(Vec::new());
            return;
        }
        let scale = self.scale.max(1.0);
        let next = card.or(self.hovered).and_then(|h| self.snapshot.cards.iter().find(|c| c.stage == h));
        let targets: Vec<Target> = self
            .snapshot
            .active_windows
            .iter()
            .chain(next.into_iter().flat_map(|c| c.windows.iter().take(self.m().max_windows)))
            .map(|w| Target {
                id: w.id,
                width: (w.frame.w * scale).round().clamp(1.0, 16384.0) as u32,
                height: (w.frame.h * scale).round().clamp(1.0, 16384.0) as u32,
            })
            .collect();
        self.motion.capturer.set_targets(targets);
        if now {
            self.motion.capturer.capture_now();
        }
    }

    /// A full-size snapshot arrived.
    pub(super) fn set_snapshot(&mut self, id: u32, image: CFRetained<CGImage>) {
        if !self.style.animation.enabled {
            return;
        }
        let bytes = CGImage::width(Some(&image)) * CGImage::height(Some(&image)) * 4;
        if let Some(r) = &self.motion.running {
            for g in r.ghosts.iter().filter(|g| g.window == id) {
                g.set_image(&image);
            }
        }
        if let Some(c) = self.motion.window_card.as_mut().filter(|c| c.window == id) {
            c.ghost.set_image(&image);
            c.has_image = true;
        }
        self.motion.cache.insert(id, image, bytes);
    }

    /// While a window is dragged with the pointer over the strip column (`over`, global), its
    /// card follows the pointer above everything; elsewhere it goes.
    pub(super) fn window_drag_card(&mut self, over: Option<Point>) {
        let (Some(p), Some(w)) = (over, self.snapshot.window_drag) else {
            if self.motion.window_card.take().is_some_and(|c| {
                c.ghost.remove();
                true
            }) && self.motion.running.is_none()
            {
                self.motion.overlay.hide();
            }
            return;
        };
        let m = self.m().clone();
        let seen = self.motion.drag_seen.filter(|s| s.0 == w.id).map_or(p, |s| s.1);
        let f = w.frame;
        let grab = if f.w > 1.0 && f.h > 1.0 { ((seen.x - f.x) / f.w, (seen.y - f.y) / f.h) } else { (0.5, 0.05) };
        let corner = self.style.cards.corner_radius * m.k;
        if self.motion.window_card.as_ref().is_none_or(|c| c.window != w.id) {
            self.place_overlay();
            let image = self.motion.cache.get(w.id).cloned().or_else(|| self.thumbs.get(&w.id).cloned());
            let o = self.motion.overlay.origin();
            let at = Plane::under_pointer(Point::new(p.x - o.x, p.y - o.y), m.thumb_size(&f), grab, m.tilt, corner);
            let style = self.style.animation.clone();
            let ghost = self.motion.overlay.ghost(w.id, image.as_deref(), at, &style, self.style.drag.ghost_opacity as f32);
            self.motion.window_card = Some(WindowCard { window: w.id, ghost, has_image: image.is_some() });
            self.motion.overlay.show();
        }
        let o = self.motion.overlay.origin();
        let at = Plane::under_pointer(Point::new(p.x - o.x, p.y - o.y), m.thumb_size(&f), grab, m.tilt, corner);
        if let Some(c) = self.motion.window_card.as_mut() {
            c.ghost.set(at);
        }
    }

}
