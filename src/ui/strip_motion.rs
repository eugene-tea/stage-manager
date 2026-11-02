//! The strip's motion: full-size window snapshots, the switch animation and the card of a
//! window dragged over the strip, drawn in the [`Overlay`] above every window.
//!
//! - **Snapshots.** Apple animates the live windows; we cannot, so we move pictures of them.
//!   A second capturer keeps full-size snapshots of the active stage's windows (every
//!   `[animation] snapshot_interval_ms`, and at once when the pointer reaches a card or presses
//!   one, so they are fresh when a switch starts), plus the hovered card's windows. A window's
//!   last snapshot before it parks stays in the cache (`snapshot_cache_mb`) and is what grows
//!   back when its stage returns; a card's thumbnail stands in when there is none. With
//!   captures off (`[strip] capture = false`, `STAGE_MANAGER_NO_CAPTURE`) or refused by
//!   macOS, plain ghosts (a light card) move instead.
//! - **Transitions.** When the controller announces one ([`Transition`]), outgoing windows'
//!   snapshots appear exactly over them and shrink into their card's slot on the spring;
//!   incoming windows' snapshots grow from their card to their frames. The slot is the
//!   engine's ([`MovingWindow::slot`], `ui::transition_plan`), never one the strip works out
//!   on its own; once the incoming windows are in place their frames are read back and their
//!   snapshots finish exactly there. Each snapshot's start, end and slot are logged and kept
//!   for `ctl state` ([`strip_model::last_transition`]). The strip answers
//!   with what it covered ([`strip_model::cover`]); the controller parks the outgoing
//!   windows behind their snapshots and holds the incoming ones until
//!   [`StripEvent::Unveil`]: when the growing snapshots as shown are within a point of their
//!   windows on every edge (`motion_math::unveil_time`, by the farthest edge's travel; not
//!   before `[animation] unpark_at` of the motion), and the snapshots stay over the windows
//!   a moment after ([`AFTER_UNVEIL`]) while they show, then fade out over them
//!   ([`LANDING_FADE`]). Each hands its shadow over to its window's once the window server
//!   shows the window at its frame, polled from the unveil ([`Strip::poll_shown`]), and the
//!   landing waits for that too. The incoming key window is captured again once it is key
//!   ([`RECAPTURE_AFTER`]), its picture cross-fading in mid-flight. Late windows (an app we
//!   hid, unhidden after the unveil: [`Transition::late`]) show once the app is unhidden and
//!   land once drawn in their key look ([`Strip::late_shown`]). Meanwhile the park corner,
//!   where
//!   they wait (raised above the corner patch when an activation brought them), is covered on
//!   the overlay with the patch's picture (a still of the desktop where icons lie, 8L3),
//!   under every snapshot ([`Transition::corner`], round 8L). The outgoing stage's card
//!   stays hidden until its snapshots land, then everything goes in one frame, except a
//!   cover of the patch's picture while the corner patch is not drawn yet: it stays until
//!   the patch has been on screen a moment, a few hundred milliseconds at most
//!   ([`cover_holds`], 10-23: the slivers flashed between the cover and the patch). A new
//!   transition cuts a running one short, and takes over a cover still held.
//! - **A window dragged over the strip** is shown as a card-sized snapshot, turned like a
//!   card, under the pointer (following every drag event through a global monitor); dropped
//!   there, it shrinks into its new card's slot as the transition's outgoing snapshot.

use std::collections::HashMap;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask};
use objc2_core_graphics::CGImage;
use objc2_foundation::{MainThreadMarker, NSRunLoop, NSRunLoopCommonModes, NSTimer};

use super::{Ghost as CardGhost, Strip, with_strip};
use crate::config::SnapshotShadow;
use crate::engine::corner_patch::{CoverPicture, HoldFacts, SwitchCover, cover_holds};
use crate::engine::geometry::{Point, Rect, Size};
use crate::platform::capture::{self, Capturer, Picture, Target};
use crate::platform::windows;
use crate::ui::motion_math::{
    Plane, SHOWN_FADE, SnapshotCache, Spring, settle_time, shown_handover, shown_lands, shows_at, switch_spring, travel, unveil_time,
};
use crate::ui::overlay::{CardShadow, Ghost, GhostShadow, Overlay};
use crate::ui::shadow_math::handover;
use crate::ui::strip_layout::CardLayout;
use crate::ui::strip_model::{self, Cover, GhostReport, MovingWindow, StripEvent, Transition, TransitionReport};
use crate::ui::window_shadow::{self, DEFAULT_CORNER, WindowShadow};

/// The interval used when `snapshot_interval_ms = 0` (captures only on demand).
const ON_DEMAND: Duration = Duration::from_secs(24 * 3600);
/// How close to its target the spring must stay before a transition's snapshots go.
const SETTLED: f64 = 0.002;
/// A card dragged out keeps its ghost on screen at most this long while its windows'
/// transition is on its way.
const LANDING_MAX: f64 = 1.0;
/// Outgoing snapshots start shrinking after this at the latest, parked or not (seconds).
const PARK_WAIT_MAX: f64 = 0.2;
/// How often a landed switch's held cover looks whether the patch shows (seconds: a frame).
const HOLD_POLL: f64 = 1.0 / 60.0;
/// How long incoming snapshots stay over their windows after the unveil (seconds): the
/// windows are placed through the Accessibility API and reach the screen a presentation lag
/// or more later; the snapshots, within a point of them by then, go after that.
const AFTER_UNVEIL: f64 = 0.12;
/// How often the window server is asked whether the incoming windows show at their frames,
/// from the unveil on (seconds: half a 60 Hz frame).
const SHOWN_POLL: f64 = 1.0 / 120.0;
/// How long past the unveil an incoming snapshot waits for the window server to show its
/// window at its frame before it hands its shadow over anyway (seconds; a late window waits
/// [`LATE_MAX`]). The windows showed 30–70 ms after the unveil (11-01).
const SHOWN_MAX: f64 = 0.3;
/// The strip's thumbnail captures wait at most this long for a switch to land (seconds;
/// [`Capturer::hold_until`]).
const THUMBS_HOLD_MAX: f64 = 1.5;
/// How long past the unveil the snapshots of late windows ([`Transition::late`]: an app we hid,
/// unhidden after the unveil) wait for them to show before they hand over and land anyway
/// (seconds). Messages showed and took its activation ~115 ms after the unveil (11-01).
const LATE_MAX: f64 = 0.4;
/// How long after a switch starts the incoming key window is captured again (seconds): its
/// snapshot was taken while it was parked and not key (the inactive look; 11-01, Mail), and by
/// now the switch's focus has made it key and its app has redrawn it (Mail took its activation
/// ~130 ms in). The new picture cross-fades in while the snapshot still grows.
const RECAPTURE_AFTER: f64 = 0.18;
/// How long the incoming snapshots take to fade out at the landing (seconds): what still
/// differs from the window under them (a look captured before, the window's own late redraw)
/// melts instead of flipping in one frame. They sit within a tenth of a point of their windows
/// by then, so nothing moves.
const LANDING_FADE: f64 = 0.2;

/// A running transition.
struct Running {
    id: u64,
    ghosts: Vec<Ghost>,
    /// Outgoing snapshots waiting for their windows to park: (index in `ghosts`, card plane).
    waiting: Vec<(usize, Plane)>,
    /// Cards hidden until their snapshots land.
    held: Vec<u64>,
    timers: Vec<Retained<NSTimer>>,
    /// The window in place the snapshots pass under ([`Transition::under`]).
    under: Option<u32>,
    /// The park corner's cover while the incoming windows wait there ([`Transition::corner`]).
    corner: Option<Ghost>,
    /// Where that cover is and what it shows.
    corner_at: Option<(Rect, CoverPicture)>,
    /// The incoming windows' frames: aimed at, then read back once in place.
    arrived: Vec<(u32, Rect)>,
    /// The transition does not land before this (its incoming windows shown under their
    /// snapshots: the unveil plus [`AFTER_UNVEIL`]; for late windows, when they show plus
    /// as much).
    lands_after: Option<Instant>,
    /// When the strip unveils the incoming windows.
    unveil_at: Option<Instant>,
    /// Incoming windows the window server does not show at their frames yet: their snapshots
    /// keep their shadows, and the landing waits ([`Strip::poll_shown`]).
    unshown: Vec<u32>,
    /// The late incoming windows ([`Transition::late`]): they may take [`LATE_MAX`] to show.
    late: Vec<u32>,
    /// Late incoming windows not drawn in their final look yet: the landing waits for them.
    late_inactive: Vec<u32>,
    /// The landing is not before this once every incoming window shows (its shadow handed
    /// over: [`shown_lands`]).
    shown_land: Option<Instant>,
    /// Nor before this once every late window is in its final look.
    final_land: Option<Instant>,
    /// The landing was due once already (its outgoing snapshots settled): a landing moved
    /// earlier needs a timer of its own.
    settled: bool,
}

/// A landed switch's incoming snapshots fading out over their windows ([`LANDING_FADE`]).
struct Fading {
    ghosts: Vec<Ghost>,
    timer: Retained<NSTimer>,
}

/// A landed switch's cover of the park corner, kept until the corner patch shows
/// ([`cover_holds`]).
struct Hold {
    ghost: Ghost,
    facts: HoldFacts,
    since: Instant,
    timer: Retained<NSTimer>,
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
    /// Tells this landing from a later one (its timer clears only its own).
    seq: u64,
    timer: Retained<NSTimer>,
}

/// Motion state kept by the strip.
pub(super) struct Motion {
    overlay: Overlay,
    cache: SnapshotCache<Picture>,
    capturer: Capturer,
    interval_ms: Option<u64>,
    running: Option<Running>,
    window_card: Option<WindowCard>,
    landing: Option<Landing>,
    /// Counts landings.
    landing_seq: u64,
    /// The pointer when the current window drag was first seen, and the window.
    drag_seen: Option<(u32, Point)>,
    drag_monitor: Option<Retained<AnyObject>>,
    /// A landed switch's cover still over the corner.
    hold: Option<Hold>,
    /// A landed switch's incoming snapshots, fading out.
    fading: Option<Fading>,
}

impl Motion {
    /// The snapshot last taken of window `id` for a ghost, if still kept.
    pub(super) fn snapshot(&self, id: u32) -> Option<&Picture> {
        self.cache.get(id)
    }

    pub(super) fn new(mtm: MainThreadMarker) -> Self {
        Self {
            overlay: Overlay::new(mtm),
            cache: SnapshotCache::new(512 << 20),
            capturer: Capturer::start_quiet(ON_DEMAND, super::snapshot_arrived),
            interval_ms: None,
            running: None,
            window_card: None,
            landing: None,
            landing_seq: 0,
            drag_seen: None,
            drag_monitor: None,
            hold: None,
            fading: None,
        }
    }

    /// Nothing draws in the overlay: no transition, no dragged window's card, no held cover,
    /// no landed snapshot fading.
    fn overlay_idle(&self) -> bool {
        self.running.is_none() && self.window_card.is_none() && self.hold.is_none() && self.fading.is_none()
    }

    /// Takes a landed switch's fading snapshots off now (the overlay hidden if nothing else
    /// is in it).
    fn drop_fading(&mut self) {
        let Some(f) = self.fading.take() else { return };
        f.timer.invalidate();
        objc2_quartz_core::CATransaction::begin();
        objc2_quartz_core::CATransaction::setDisableActions(true);
        for g in &f.ghosts {
            g.remove();
        }
        objc2_quartz_core::CATransaction::commit();
        if self.overlay_idle() {
            self.overlay.hide();
        }
    }

    /// Takes a held cover off the corner now (the overlay hidden if nothing else is in it).
    fn drop_hold(&mut self) {
        let Some(h) = self.hold.take() else { return };
        h.timer.invalidate();
        objc2_quartz_core::CATransaction::begin();
        objc2_quartz_core::CATransaction::setDisableActions(true);
        h.ghost.remove();
        objc2_quartz_core::CATransaction::commit();
        if self.overlay_idle() {
            self.overlay.hide();
        }
    }

    /// A held cover looks again ([`HOLD_POLL`]): it goes once the patch is drawn or the hold
    /// is over, else it looks again a frame later.
    fn check_hold(&mut self) {
        let now = Instant::now();
        let Some(h) = self.hold.as_mut() else { return };
        h.facts.held_for = now.saturating_duration_since(h.since);
        h.facts.patch_shown_for = crate::ui::corner_patch::shown_for(now);
        if cover_holds(&h.facts) {
            h.timer = after(HOLD_POLL, poll_hold);
            return;
        }
        tracing::debug!(
            held_ms = h.facts.held_for.as_millis() as u64,
            patch_shown_ms = h.facts.patch_shown_for.map(|t| t.as_millis() as u64),
            "transition: the corner's cover leaves"
        );
        self.drop_hold();
    }

    /// True while `stage`'s card waits for its transition's snapshots to land.
    pub(super) fn holds(&self, stage: u64) -> bool {
        self.running.as_ref().is_some_and(|r| r.held.contains(&stage))
    }

    /// The user went to another Space: an idle overlay gets a fresh panel, which goes to the
    /// Space shown when it is first ordered in (a panel ordered in before keeps going back
    /// to the Space it was on).
    pub(super) fn space_changed(&mut self, mtm: MainThreadMarker) {
        // A cover held on the Desktop left goes with it.
        self.drop_hold();
        self.drop_fading();
        if self.running.is_none() && self.window_card.is_none() {
            self.overlay.renew(mtm);
        }
    }

    /// A thumbnail arrived: the dragged window's card uses it while it has no snapshot.
    pub(super) fn thumbnail_arrived(&mut self, id: u32, picture: &Picture) {
        if let Some(c) = self.window_card.as_mut().filter(|c| c.window == id && !c.has_image) {
            c.ghost.set_image(picture, false);
            c.has_image = true;
        }
    }

    /// Keeps a card's drag ghost (released in the main area at `rect`, showing `window`) until
    /// the transition of its windows takes over.
    pub(super) fn keep_landing(&mut self, ghost: Option<CardGhost>, window: u32, rect: Rect) {
        let Some(ghost) = ghost else { return };
        if let Some(old) = self.landing.take() {
            old.timer.invalidate();
        }
        self.landing_seq += 1;
        let seq = self.landing_seq;
        // Clears this landing only, never a newer one (review R1-ui 6).
        let timer = after(LANDING_MAX, move || {
            with_strip(|s| {
                if s.motion.landing.as_ref().is_some_and(|l| l.seq == seq) {
                    s.motion.landing = None;
                }
            });
        });
        self.landing = Some(Landing { _ghost: ghost, window, rect, seq, timer });
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

/// A held cover's look ([`Motion::check_hold`]), from its timer.
fn poll_hold() {
    with_strip(|s| s.motion.check_hold());
}

/// True when a transition is drawn: animation on, and not on a full-screen Space, where the
/// overlay cannot show (it is no full-screen auxiliary window), so it would cover nothing
/// while the controller waited behind it (the review's U8). Not drawn, nothing is covered
/// and the switch runs at once.
fn draws_motion(animates: bool, fullscreen: bool) -> bool {
    animates && !fullscreen
}

/// Runs `f` once on the main run loop after `seconds`, in every mode (also while a menu
/// tracks the pointer or a card is dragged: the review's U10, one-shots in the default mode
/// left a lifted card lifted and a merged card's layer behind).
pub(super) fn after(seconds: f64, f: impl Fn() + 'static) -> Retained<NSTimer> {
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| f());
    // SAFETY: the block runs on the main run loop only.
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(seconds.max(0.0), false, &block) };
    // SAFETY: the main run loop and a framework mode constant.
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    timer
}

impl Strip {
    /// True when the motion overlay draws top-down (see `overlay`).
    /// True while a transition's snapshots are on their way.
    pub(super) fn motion_running(&self) -> bool {
        self.motion.running.is_some()
    }

    pub(super) fn motion_top_down(&self) -> bool {
        self.motion.overlay.top_down()
    }

    /// The motion overlay's window number and its top-left corner (global), while it shows.
    pub(super) fn motion_overlay(&self) -> Option<(isize, Point)> {
        self.motion.overlay.shown().then(|| (self.motion.overlay.window_number(), self.motion.overlay.origin()))
    }

    /// The windows' spring (`[animation]` at its speed).
    fn spring(&self) -> Spring {
        switch_spring(&self.style)
    }

    /// The shadows a snapshot can carry at full size (`[animation] shadow`): the key window's
    /// and an inactive one's in the appearance windows are drawn in now (`dark`), or the
    /// custom one for both.
    fn window_shadows(&self, dark: bool) -> [WindowShadow; 2] {
        let a = &self.style.animation;
        match a.shadow {
            SnapshotShadow::Window => [WindowShadow::of_window(true, dark), WindowShadow::of_window(false, dark)],
            SnapshotShadow::Custom => [WindowShadow::custom(a.shadow_radius, a.shadow_opacity, a.shadow_offset_y); 2],
        }
    }

    /// True when windows are drawn in the dark appearance now: the system's, read at each
    /// snapshot so a change of Light, Dark or Auto is followed at the next switch (macOS
    /// draws a window's shadow by the window's own appearance, which follows the system's
    /// unless its app sets one).
    fn dark_now() -> bool {
        MainThreadMarker::new().is_some_and(crate::platform::wallpaper::dark_appearance)
    }

    /// How a window's snapshot (`picture`, the window at `frame`) is shadowed: the key
    /// window's shadow when `key`, an inactive window's otherwise, around its own corner
    /// (read from the picture, else a titled window's; `[animation] corner_radius` when set),
    /// and the card's shadow on a card; starting `amount` of the way to the window's (0 a
    /// card, 1 the window).
    fn ghost_shadow(&self, picture: Option<&Picture>, frame: Rect, key: bool, amount: f64) -> GhostShadow {
        let [active, inactive] = self.window_shadows(Self::dark_now());
        let set = self.style.animation.corner_radius;
        let corner = if set > 0.0 { set } else { picture.and_then(|p| window_shadow::corner_of(p, (frame.w, frame.h))).unwrap_or(DEFAULT_CORNER) };
        let c = &self.style.cards;
        let k = self.m().k;
        let card = CardShadow {
            color: (c.shadow_color.r, c.shadow_color.g, c.shadow_color.b, c.shadow_color.a),
            opacity: c.shadow_opacity,
            radius: c.shadow_radius * k,
            offset: (c.shadow_offset_x * k, c.shadow_offset_y * k),
        };
        GhostShadow { window: if key { active } else { inactive }, corner, window_size: (frame.w, frame.h), card, amount }
    }

    /// Lays the overlay over the main display with the strip's eye, and has the snapshots'
    /// shadows drawn for its scale ahead of the first switch.
    fn place_overlay(&mut self) {
        let g = &self.geometry;
        let eye = Point::new(g.global.x + g.m.eye.x, g.global.y + g.m.eye.y);
        self.motion.overlay.place(g.screen, g.primary_height, eye, g.m.perspective);
        // Both appearances', so a change of appearance finds them drawn too.
        let [a, b] = self.window_shadows(false);
        let [c, d] = self.window_shadows(true);
        window_shadow::prepare(&[a, b, c, d], self.motion.overlay.scale());
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
        // As the card is drawn: grown under the pointer, or pressed (the click's card).
        let s = self.card_scale(layout.stage);
        if s != 1.0 {
            let c = layout.hover_center(m);
            let c = Point::new(g.x - o.x + c.x + self.current_dx(), g.y - o.y + c.y);
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
        // The engine's slots count the active stage's empty slot (`[strip] active = "gap"`).
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
        // Its own turn: a snapshot a hair from flat (its spring not quite at rest) is about
        // as wide as its window, never measured as a card (11-01: 1217 pt read as 355).
        let g = self.geometry.global;
        let b = self.m().projected_bounds_turned(p.left_mid.x + o.x - g.x, top + o.y - g.y, p.size.w, p.size.h, p.tilt);
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
    /// A running one cut short, or a cover still held from the last switch, goes once this
    /// one is up: the overlay stays shown between the two (hidden after if nothing is in it).
    pub(super) fn begin_transition(&mut self, t: Transition, starts: HashMap<u32, (Plane, Option<usize>)>) {
        let held = self.motion.hold.take();
        let fading = self.motion.fading.take();
        self.start_transition(t, starts);
        if let Some(h) = held {
            self.motion.hold = Some(h);
            self.motion.drop_hold();
        }
        if let Some(f) = fading {
            self.motion.fading = Some(f);
            self.motion.drop_fading();
        }
        if self.motion.overlay_idle() {
            self.motion.overlay.hide();
        }
    }

    fn start_transition(&mut self, t: Transition, starts: HashMap<u32, (Plane, Option<usize>)>) {
        self.finish_transition(false);
        if !draws_motion(self.style.animation.animates(), self.snapshot.fullscreen) {
            strip_model::cover(t.id, Cover::default());
            return;
        }
        self.place_overlay();
        let under = t.under;
        let below = self.overlay_below(under);
        let style = self.style.animation.clone();
        let spring = self.spring();
        let o = self.motion.overlay.origin();
        let mut cover = Cover::default();
        let mut ghosts = Vec::new();
        let mut waiting = Vec::new();
        let mut held = Vec::new();
        let mut report = Vec::new();
        // With captures off or refused no snapshot is coming: plain ghosts stand in.
        let plain = !capture::pictures_expected();
        // The key window's shadow on the window that is key (outgoing) or becomes key
        // (incoming), an inactive one's on the others.
        let (key_out, key_in) = (t.key_out, t.key_in);
        // The outgoing windows leave `cover_ms` after their snapshots are drawn: the
        // snapshots' shadows come in around then, as the windows' go.
        let (out_begin, out_duration) = handover(style.cover_ms as f64 / 1000.0, 2.0 * style.cover_ms as f64 / 1000.0);
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
                    let image = self.motion.cache.get(id).cloned();
                    if image.is_none() && !plain {
                        tracing::debug!(id, "no snapshot of an outgoing window; it just goes");
                        continue;
                    }
                    let at = Plane::window(w.window.frame, o, style.corner_radius);
                    let shadow = self.ghost_shadow(image.as_ref(), w.window.frame, key_out == Some(id), 1.0);
                    let ghost = self.motion.overlay.ghost(id, image.as_ref(), at, &shadow, 1.0);
                    ghost.fade_shadow(0.0, 1.0, out_begin, out_duration);
                    ghost
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
                at_unveil: None,
                shown_ms: None,
                handover_ms: None,
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
            let (true, Some((start, from))) = (image.is_some() || plain, starts.get(&id).copied()) else {
                tracing::debug!(id, "no snapshot of an incoming window; it just comes");
                continue;
            };
            let shadow = self.ghost_shadow(image.as_ref(), w.window.frame, key_in == Some(id), 0.0);
            let mut ghost = self.motion.overlay.ghost(id, image.as_ref(), start, &shadow, 1.0);
            let end = Plane::window(w.window.frame, o, style.corner_radius);
            ghost.spring_to(end, spring);
            ghost.spring_shadow(1.0, spring);
            report.push(GhostReport {
                window: id,
                outgoing: false,
                stage: w.stage,
                slot: w.slot,
                aimed_slot: from,
                start: self.plane_rect(&start),
                end: self.plane_rect(&end),
                real: None,
                at_unveil: None,
                shown_ms: None,
                handover_ms: None,
            });
            ghosts.push(ghost);
            cover.incoming = true;
        }
        if ghosts.is_empty() {
            strip_model::cover(t.id, cover);
            return;
        }
        // The park corner, under every snapshot, while the incoming windows wait there (8L).
        let corner = if cover.incoming { t.corner.clone().and_then(|c| self.corner_cover(t.id, c)) } else { None };
        let corner_at = corner.as_ref().and(t.corner.as_ref()).map(|c| (c.rect, c.picture));
        let arrived = t.incoming.iter().map(|w| (w.window.id, w.window.frame)).collect();
        // Gone at once, never an implicit fade (review R1-ui 11).
        objc2_quartz_core::CATransaction::begin();
        objc2_quartz_core::CATransaction::setDisableActions(true);
        for stage in &held {
            if let Some(layers) = self.pane.cards.get(stage) {
                layers.card.removeAnimationForKey(&objc2_foundation::NSString::from_str("fade"));
                layers.card.setOpacity(0.0);
            }
        }
        objc2_quartz_core::CATransaction::commit();
        self.show_overlay(below);
        self.motion.overlay.flush();
        self.motion.landing = None;
        // The cards' thumbnails are not captured while the snapshots move.
        self.capturer.hold_until(Some(Instant::now() + Duration::from_secs_f64(THUMBS_HOLD_MAX)));
        strip_model::cover(t.id, cover);
        let id = t.id;
        let mut timers = Vec::new();
        // The incoming windows take their place once their snapshots as shown are within a
        // point of them: the farthest edge's travel decides when.
        let travel = report.iter().filter(|g| !g.outgoing).fold(0.0, |m: f64, g| m.max(travel(g.start, g.end)));
        let unveil = cover.incoming.then(|| unveil_time(&self.style, travel));
        let started = Instant::now();
        let unveil_at = unveil.map(|u| started + Duration::from_secs_f64(u));
        // Late windows (an app we hid, unhidden after the unveil) show when their app says so,
        // bounded: their snapshots wait for that ([`Self::late_shown`]).
        let late: Vec<u32> = t.late.iter().copied().filter(|w| ghosts.iter().any(|g| g.window == *w)).collect();
        // The handover: each incoming snapshot keeps its shadow until the window server shows
        // its window at its frame, polled from the unveil ([`Self::poll_shown`]); the landing
        // waits for every one (the latest it can be until then).
        let unshown: Vec<u32> =
            if unveil.is_some() { ghosts.iter().filter(|g| t.incoming.iter().any(|w| w.window.id == g.window)).map(|g| g.window).collect() } else { Vec::new() };
        let wait = if late.is_empty() { SHOWN_MAX } else { LATE_MAX.max(SHOWN_MAX) };
        let lands_after = unveil.map(|u| started + Duration::from_secs_f64(u + wait + SHOWN_FADE + AFTER_UNVEIL));
        if let Some(u) = unveil.filter(|_| !late.is_empty()) {
            timers.push(after(u + LATE_MAX, move || {
                with_strip(|s| s.late_shown(id, None, None));
            }));
        }
        // The key window's snapshot, taken parked and inactive, again once it is key.
        if let Some(key) = t.key_in.filter(|k| !late.contains(k) && ghosts.iter().any(|g| g.window == *k) && !plain) {
            timers.push(after(RECAPTURE_AFTER, move || {
                with_strip(|s| s.recapture_key(id, key));
            }));
        }
        if let Some(unveil) = unveil {
            timers.push(after(unveil, move || {
                // The strip busy (never expected from a timer): the unveil still goes.
                if with_strip(|s| s.unveil(id)).is_none() {
                    strip_model::emit(StripEvent::Unveil { id });
                }
            }));
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
        let unveil_ms = unveil.map(|u| (u * 1000.0).round() as u64);
        tracing::debug!(id, ghosts = ghosts.len(), held = ?held, ?cover, travel = travel.round(), unveil_ms, ?late, "transition");
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
        let covered = corner.as_ref().and(t.corner.as_ref());
        let corner_picture = covered.map(|c| match c.picture {
            CoverPicture::Wallpaper => "wallpaper",
            CoverPicture::Desktop => "desktop",
        });
        strip_model::publish_transition(TransitionReport {
            id,
            ghosts: report,
            order,
            corner: covered.map(|c| c.rect),
            corner_picture,
            travel,
            unveil_ms,
        });
        self.motion.running =
            Some(Running {
                id,
                ghosts,
                waiting,
                held,
                timers,
                under,
                corner,
                corner_at,
                arrived,
                lands_after,
                unveil_at,
                unshown,
                late: late.clone(),
                late_inactive: late,
                shown_land: None,
                final_land: None,
                settled: false,
            });
    }

    /// The park corner's cover for transition `id` (global): the corner patch's picture of it,
    /// or the still of the desktop where icons lie (round 8L3), flat on the overlay under every
    /// snapshot (round 8L), with the snapshots of the windows that stay shown into it drawn
    /// over the picture where they are (8L5). `None` (logged) when that picture, or one of
    /// those snapshots, is not at hand now: the corner is left as it is.
    fn corner_cover(&self, id: u64, cover: SwitchCover) -> Option<Ghost> {
        let rect = cover.rect;
        let image = match cover.picture {
            CoverPicture::Wallpaper => crate::ui::corner_patch::cover_image(rect),
            CoverPicture::Desktop => crate::platform::desktop_shot::image(rect),
        };
        let Some((image, scale)) = image else {
            tracing::debug!(id, %rect, picture = ?cover.picture, "transition: no picture of the corner now; it is not covered");
            return None;
        };
        let mut over = Vec::with_capacity(cover.over.len());
        for (window, bounds) in &cover.over {
            let Some(snapshot) = self.motion.cache.get(*window) else {
                tracing::debug!(id, window, "transition: no snapshot of a window that stays shown into the corner now; it is not covered");
                return None;
            };
            over.push((snapshot.clone(), Rect::new(bounds.x - rect.x, bounds.y - rect.y, bounds.w, bounds.h)));
        }
        let o = self.motion.overlay.origin();
        let staying: Vec<u32> = cover.over.iter().map(|(w, _)| *w).collect();
        tracing::debug!(id, %rect, cap = %cover.cap, picture = ?cover.picture, ?staying, "transition: the park corner is covered while the incoming windows wait there");
        Some(self.motion.overlay.cover(&image, scale, &over, Plane::window(rect, o, 0.0)))
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
                if let Some(a) = r.arrived.iter_mut().find(|(w, _)| w == window) {
                    a.1 = *real;
                }
                let to = Plane::window(*real, o, corner);
                for g in r.ghosts.iter_mut().filter(|g| g.window == *window && g.target.tilt == 0.0) {
                    let t = g.target;
                    let off = (t.left_mid.x - to.left_mid.x).abs().max((t.left_mid.y - to.left_mid.y).abs()).max((t.size.w - to.size.w).abs()).max((t.size.h - to.size.h).abs());
                    tracing::debug!(id, window, real = %real, off, "transition: ghost end vs the window's real frame");
                    if off > 0.25 {
                        g.spring_to(to, spring);
                        g.spring_shadow(1.0, spring);
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
                g.spring_shadow(0.0, spring);
            }
        }
        r.timers.push(after(settle_time(spring, SETTLED), move || {
            with_strip(|s| s.end_transition(id));
        }));
    }

    /// Transition `id`'s incoming snapshots are within a point of their windows as shown:
    /// the controller places the windows now. Where each snapshot is drawn is kept for
    /// `ctl state` ([`GhostReport::at_unveil`]).
    fn unveil(&mut self, id: u64) {
        if let Some(r) = self.motion.running.as_ref().filter(|r| r.id == id) {
            let drawn: Vec<(u32, Rect)> =
                r.ghosts.iter().filter(|g| r.arrived.iter().any(|(w, _)| *w == g.window)).map(|g| (g.window, self.plane_rect(&g.drawn()))).collect();
            for (window, at) in &drawn {
                tracing::debug!(id, window, at = %at, "transition: unveiled; the snapshot is drawn here");
            }
            strip_model::update_transition(id, |t| {
                for g in t.ghosts.iter_mut().filter(|g| !g.outgoing) {
                    g.at_unveil = drawn.iter().find(|(w, _)| *w == g.window).map(|(_, at)| *at);
                }
            });
        }
        strip_model::emit(StripEvent::Unveil { id });
        // The controller places the windows now: from here the window server is asked when
        // each shows at its frame.
        self.poll_shown(id);
    }

    /// Asks the window server whether transition `id`'s incoming windows not shown yet show
    /// at their frames now (one query for all of them); those that do hand their shadow over
    /// ([`Self::windows_shown`]). Again each [`SHOWN_POLL`] while any is left; past
    /// [`SHOWN_MAX`] after the unveil ([`LATE_MAX`] for a late window) one left hands over
    /// anyway (logged).
    fn poll_shown(&mut self, id: u64) {
        let Some(r) = self.motion.running.as_mut().filter(|r| r.id == id && !r.unshown.is_empty()) else { return };
        let now = Instant::now();
        let since = r.unveil_at.map_or(0.0, |u| now.saturating_duration_since(u).as_secs_f64());
        let described = windows::describe(&r.unshown);
        let shown: Vec<u32> = r
            .unshown
            .iter()
            .copied()
            .filter(|w| {
                let frame = r.arrived.iter().find(|(a, _)| a == w).map(|(_, f)| *f);
                let seen = described.iter().find(|d| d.id == *w);
                frame.zip(seen).is_some_and(|(f, d)| shows_at(d.onscreen, d.bounds, f))
            })
            .collect();
        let waited: Vec<u32> = r
            .unshown
            .iter()
            .copied()
            .filter(|w| !shown.contains(w) && since >= if r.late.contains(w) { LATE_MAX.max(SHOWN_MAX) } else { SHOWN_MAX })
            .collect();
        for w in &waited {
            let seen = described.iter().find(|d| d.id == *w).map(|d| format!("{} onscreen={}", d.bounds, d.onscreen));
            tracing::debug!(id, window = w, ?seen, since_unveil_ms = (since * 1000.0).round() as u64, "transition: the window server never showed the incoming window at its frame; its snapshot hands over anyway");
        }
        self.windows_shown(id, &shown, true);
        self.windows_shown(id, &waited, false);
        if self.motion.running.as_ref().is_some_and(|r| r.id == id && !r.unshown.is_empty()) {
            let timer = after(SHOWN_POLL, move || {
                with_strip(|s| s.poll_shown(id));
            });
            if let Some(r) = self.motion.running.as_mut() {
                r.timers.push(timer);
            }
        }
    }

    /// Transition `id`'s incoming windows `windows` show at their frames now (`seen`: the
    /// window server said so; else the wait ran out): each snapshot's shadow fades from
    /// [`shown_handover`] on, and once every incoming window shows the landing is planned
    /// ([`Self::replan_landing`]). Kept for `ctl state` ([`GhostReport::shown_ms`]).
    fn windows_shown(&mut self, id: u64, windows: &[u32], seen: bool) {
        let Some(r) = self.motion.running.as_mut().filter(|r| r.id == id) else { return };
        let windows: Vec<u32> = windows.iter().copied().filter(|w| r.unshown.contains(w)).collect();
        if windows.is_empty() {
            return;
        }
        r.unshown.retain(|w| !windows.contains(w));
        let now = Instant::now();
        let since = r.unveil_at.map_or(0.0, |u| now.saturating_duration_since(u).as_secs_f64());
        let begin = shown_handover(since);
        for g in r.ghosts.iter().filter(|g| windows.contains(&g.window)) {
            g.fade_shadow(1.0, 0.0, begin, SHOWN_FADE);
        }
        let land = now + Duration::from_secs_f64(shown_lands(since));
        r.shown_land = Some(r.shown_land.map_or(land, |l| l.max(land)));
        let (shown_ms, handover_ms) = ((since * 1000.0).round() as u64, ((since + begin) * 1000.0).round() as u64);
        tracing::debug!(id, ?windows, seen, shown_ms, handover_ms, "transition: the incoming windows show at their frames; their snapshots hand the shadow over");
        strip_model::update_transition(id, |t| {
            for g in t.ghosts.iter_mut().filter(|g| !g.outgoing && windows.contains(&g.window)) {
                g.shown_ms = seen.then_some(shown_ms);
                g.handover_ms = Some(handover_ms);
            }
        });
        Self::replan_landing(r, now);
    }

    /// Plans transition `r`'s landing once every incoming window shows and every late one is
    /// in its final look: not before [`AFTER_UNVEIL`] past the unveil, each shadow handed over
    /// ([`Running::shown_land`]) and each late window [`AFTER_UNVEIL`] past its final look
    /// ([`Running::final_land`]); a timer of its own when the landing was due already.
    fn replan_landing(r: &mut Running, now: Instant) {
        if !r.unshown.is_empty() || !r.late_inactive.is_empty() {
            return;
        }
        let Some(floor) = r.unveil_at.map(|u| u + Duration::from_secs_f64(AFTER_UNVEIL)) else { return };
        let at = [r.shown_land, r.final_land].into_iter().flatten().fold(floor, Instant::max);
        if r.lands_after == Some(at) {
            return;
        }
        r.lands_after = Some(at);
        if r.settled {
            let id = r.id;
            r.timers.push(after(at.saturating_duration_since(now).as_secs_f64(), move || {
                with_strip(|s| s.end_transition(id));
            }));
        }
    }

    /// Late incoming windows of transition `id` on their way to the screen, as their app's
    /// notices tell: `shown` are unhidden (logged: the window server's word decides when their
    /// shadow hands over, [`Self::poll_shown`]), `active` drawn in their final look; `None`
    /// for both: the wait is over ([`LATE_MAX`] past the unveil), every one counts as final.
    /// Once every late window is in its final look the transition lands [`AFTER_UNVEIL`] after
    /// that, as after an unveil (and not before every window shows).
    pub(super) fn late_shown(&mut self, id: u64, shown: Option<&[u32]>, active: Option<&[u32]>) {
        let Some(r) = self.motion.running.as_mut().filter(|r| r.id == id) else { return };
        let now = Instant::now();
        let all = shown.is_none() && active.is_none();
        let final_look = |w: &u32| all || active.is_some_and(|a| a.contains(w));
        let on_screen: Vec<u32> = shown.unwrap_or_default().iter().copied().filter(|w| r.late.contains(w)).collect();
        let done: Vec<u32> = r.late_inactive.iter().copied().filter(final_look).collect();
        if done.is_empty() && on_screen.iter().all(|w| !r.unshown.contains(w)) {
            return;
        }
        r.late_inactive.retain(|w| !done.contains(w));
        let since_unveil_ms = r.unveil_at.map(|u| now.saturating_duration_since(u).as_millis() as u64);
        let unshown = r.unshown.clone();
        tracing::debug!(id, on_screen = ?on_screen, final_look = ?done, waited_out = all, since_unveil_ms, ?unshown, "transition: late windows show");
        if done.is_empty() || !r.late_inactive.is_empty() {
            return;
        }
        // Not before the unveil: a window placed then shows a presentation lag later.
        let from_now = r.unveil_at.map_or(0.0, |u| u.saturating_duration_since(now).as_secs_f64());
        let land = now + Duration::from_secs_f64(from_now + AFTER_UNVEIL);
        r.final_land = Some(r.final_land.map_or(land, |l| l.max(land)));
        Self::replan_landing(r, now);
    }

    /// Captures transition `id`'s incoming key window `key` again (its snapshot was taken
    /// while it was parked and not key); the picture cross-fades into its growing snapshot
    /// ([`Self::set_snapshot`]).
    fn recapture_key(&mut self, id: u64, key: u32) {
        if self.motion.running.as_ref().is_none_or(|r| r.id != id) {
            return;
        }
        tracing::debug!(id, key, "transition: the incoming key window is captured again in its key look");
        self.update_snapshot_targets(None, false);
        self.motion.capturer.recapture(&[key]);
    }

    /// Transition `id` has landed: the snapshots go and the held cards show, in one frame
    /// (the incoming snapshots fade out over their windows, [`LANDING_FADE`]); not before its
    /// incoming windows show under their snapshots (else it looks again then).
    fn end_transition(&mut self, id: u64) {
        let Some(r) = self.motion.running.as_mut().filter(|r| r.id == id) else { return };
        r.settled = true;
        let now = Instant::now();
        if let Some(wait) = r.lands_after.map(|at| at.saturating_duration_since(now)).filter(|w| !w.is_zero()) {
            r.timers.push(after(wait.as_secs_f64(), move || {
                with_strip(|s| s.end_transition(id));
            }));
            return;
        }
        self.finish_transition(true);
    }

    /// Ends the running transition now (it `landed`, or a new one starts).
    /// Shows the cards the running transition holds hidden on the pane being left and drops
    /// the hold: it belongs to that pane, whose snapshots land on the overlay left there. The
    /// transition runs on (its snapshots, its timers, the controller's unveil); the pane
    /// taking over draws its cards as usual, pressable.
    pub(super) fn release_held_cards(&mut self) {
        let Some(r) = self.motion.running.as_mut() else { return };
        let held = std::mem::take(&mut r.held);
        objc2_quartz_core::CATransaction::begin();
        objc2_quartz_core::CATransaction::setDisableActions(true);
        for stage in &held {
            if let Some(layers) = self.pane.cards.get(stage) {
                layers.card.setOpacity(1.0);
            }
        }
        objc2_quartz_core::CATransaction::commit();
    }

    /// Where the overlay goes for a transition (`under`: its window in place): just under the
    /// strip's pane while the strip is raised over windows (the edge revealing it, or its
    /// level above windows), so snapshots grow from and shrink into the cards from behind
    /// them (drawn over the cards and gone at the end, the cards popped back to the front:
    /// 10-13, a laptop with windows over the strip); else just under `under`; above every
    /// window while a dragged window's card shows, or with neither.
    fn overlay_below(&self, under: Option<u32>) -> Option<(u32, isize)> {
        if self.motion.window_card.is_some() {
            return None;
        }
        if self.pane.lifted == Some(true) {
            return u32::try_from(self.pane.panel.windowNumber()).ok().map(|n| (n, objc2_app_kit::NSFloatingWindowLevel));
        }
        under.map(|id| (id, objc2_app_kit::NSNormalWindowLevel))
    }

    /// The strip was raised or lowered: a running transition's overlay goes where it now
    /// belongs ([`Self::overlay_below`]).
    pub(super) fn reorder_overlay(&mut self) {
        let Some(under) = self.motion.running.as_ref().map(|r| r.under) else { return };
        let below = self.overlay_below(under);
        self.show_overlay(below);
    }

    /// Shows the overlay where [`Self::overlay_below`] puts it: above every window while a
    /// dragged window's card shows (it passes over the raised strip's cards), else under
    /// `below`, or above every normal window and below floating ones.
    fn show_overlay(&mut self, below: Option<(u32, isize)>) {
        if self.motion.window_card.is_some() {
            self.motion.overlay.show_on_top();
        } else {
            self.motion.overlay.show_below(below);
        }
    }

    /// Ends the running transition: it `landed` (its corner's cover may stay a moment,
    /// [`cover_holds`]), or a new one starts and hides the overlay itself if it draws nothing.
    fn finish_transition(&mut self, landed: bool) {
        let Some(mut r) = self.motion.running.take() else { return };
        self.capturer.hold_until(None);
        for t in &r.timers {
            t.invalidate();
        }
        // Landed with the corner covered by the patch's picture: the cover stays until the
        // patch shows (10-23), unless nothing is to wait for.
        let facts = match (landed, &r.corner, r.corner_at) {
            (true, Some(_), Some((cover, picture))) => Some(HoldFacts {
                cover,
                picture,
                arrived: r.arrived.iter().map(|(_, f)| *f).collect(),
                patch_shown_for: crate::ui::corner_patch::shown_for(Instant::now()),
                held_for: Duration::ZERO,
            }),
            _ => None,
        };
        let hold = facts.filter(cover_holds).and_then(|f| r.corner.take().map(|g| (g, f)));
        // Landed, the incoming snapshots fade out over their windows; the rest go at once.
        let (fade, gone): (Vec<Ghost>, Vec<Ghost>) =
            std::mem::take(&mut r.ghosts).into_iter().partition(|g| landed && r.arrived.iter().any(|(w, _)| *w == g.window));
        objc2_quartz_core::CATransaction::begin();
        objc2_quartz_core::CATransaction::setDisableActions(true);
        for g in gone.iter().chain(&r.corner) {
            g.remove();
        }
        for g in &fade {
            g.fade_out(LANDING_FADE);
        }
        for stage in &r.held {
            if let Some(layers) = self.pane.cards.get(stage) {
                layers.card.setOpacity(1.0);
            }
        }
        objc2_quartz_core::CATransaction::commit();
        if !fade.is_empty() {
            // One landing fades at a time: an older one still fading goes now.
            if let Some(old) = self.motion.fading.take() {
                old.timer.invalidate();
                for g in &old.ghosts {
                    g.remove();
                }
            }
            let timer = after(LANDING_FADE, || {
                with_strip(|s| s.motion.drop_fading());
            });
            self.motion.fading = Some(Fading { ghosts: fade, timer });
        }
        if let Some((ghost, facts)) = hold {
            tracing::debug!(id = r.id, cover = %facts.cover, "transition: landed; the corner's cover stays until the corner patch shows");
            let timer = after(HOLD_POLL, poll_hold);
            let old = self.motion.hold.replace(Hold { ghost, facts, since: Instant::now(), timer });
            if let Some(old) = old {
                // Never two (a new switch takes the last one's over), but never one left behind.
                old.timer.invalidate();
                old.ghost.remove();
            }
        }
        // Cut short, the next transition hides it if it draws nothing ([`Self::begin_transition`]).
        if landed && self.motion.overlay_idle() {
            self.motion.overlay.hide();
        }
        // A released card it held, still drawn pressed (its own click's transition, or
        // another that took it before the press let go: R3 6), lets go now that it shows.
        if landed && self.pressed.is_none() && self.squeezed.is_some_and(|s| r.held.contains(&s)) {
            self.squeeze(None);
        }
        // A strip held raised for the transition goes back below windows if nothing needs it
        // up any more.
        if landed && self.pane.lifted == Some(true) {
            self.update_visibility();
        }
        // A card shown now grows if the pointer rests on it.
        if !r.held.is_empty() && self.pane.shown {
            let p = self.pointer();
            let g = self.geometry.global;
            self.hover_at(Some(Point::new(p.x - g.x, p.y - g.y)));
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
            .chain(&self.snapshot.pinned_windows)
            .chain(self.snapshot.cards.iter().flat_map(|c| c.windows.iter()))
            .map(|w| w.id)
            .collect();
        let running: Vec<u32> = self.motion.running.as_ref().map(|r| r.ghosts.iter().map(|g| g.window).collect()).unwrap_or_default();
        self.motion.cache.retain(|id| known.contains(&id) || running.contains(&id));
        strip_model::publish_snapshots(self.motion.cache.ids());
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

    /// Tells the snapshot capturer what to keep: the active stage's windows, the pinned ones
    /// shown (for a switch's cover of the park corner, 8L5) and those of `card` (or the
    /// hovered card), at full size. `now` captures them at once.
    fn update_snapshot_targets(&mut self, card: Option<u64>, now: bool) {
        if !self.style.animation.animates() {
            self.motion.capturer.set_targets(Vec::new());
            return;
        }
        let scale = self.scale.max(1.0);
        let next = card.or(self.hovered).and_then(|h| self.snapshot.cards.iter().find(|c| c.stage == h));
        let targets: Vec<Target> = self
            .snapshot
            .active_windows
            .iter()
            .chain(&self.snapshot.pinned_windows)
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
    pub(super) fn set_snapshot(&mut self, id: u32, picture: Picture) {
        if !self.style.animation.animates() {
            return;
        }
        let bytes = CGImage::width(Some(&picture.image)) * CGImage::height(Some(&picture.image)) * 4;
        // A switch's snapshot cross-fades to the newer picture: its window may look different
        // now (made key since it was captured).
        if let Some(r) = self.motion.running.as_mut() {
            for g in r.ghosts.iter_mut().filter(|g| g.window == id) {
                g.set_image(&picture, true);
            }
        }
        if let Some(c) = self.motion.window_card.as_mut().filter(|c| c.window == id) {
            c.ghost.set_image(&picture, false);
            c.has_image = true;
        }
        self.motion.cache.insert(id, picture, bytes);
        strip_model::publish_snapshots(self.motion.cache.ids());
    }

    /// While a window is dragged with the pointer over the strip column (`over`, global), its
    /// card follows the pointer above everything; elsewhere it goes.
    pub(super) fn window_drag_card(&mut self, over: Option<Point>) {
        let (Some(p), Some(w)) = (over, self.snapshot.window_drag) else {
            if self.motion.window_card.take().is_some_and(|c| {
                c.ghost.remove();
                true
            }) && self.motion.overlay_idle()
            {
                self.motion.overlay.hide();
            }
            return;
        };
        let m = self.m().clone();
        // The grab point: where the button went down on the window (review R1-ui 7), else
        // where the pointer was when the strip first heard of the drag.
        let seen = self.snapshot.window_drag_grab.unwrap_or_else(|| self.motion.drag_seen.filter(|s| s.0 == w.id).map_or(p, |s| s.1));
        let f = w.frame;
        let grab = if f.w > 1.0 && f.h > 1.0 { ((seen.x - f.x) / f.w, (seen.y - f.y) / f.h) } else { (0.5, 0.05) };
        let corner = self.style.cards.corner_radius * m.k;
        if self.motion.window_card.as_ref().is_none_or(|c| c.window != w.id) {
            // The card goes above every window: a cover held over the corner, and landed
            // snapshots still fading, go first.
            self.motion.drop_hold();
            self.motion.drop_fading();
            self.place_overlay();
            let image = self.motion.cache.get(w.id).cloned().or_else(|| self.thumbs.get(&w.id).cloned());
            let o = self.motion.overlay.origin();
            let at = Plane::under_pointer(Point::new(p.x - o.x, p.y - o.y), m.thumb_size(&f), grab, m.tilt, corner);
            let shadow = self.ghost_shadow(image.as_ref(), f, false, 0.0);
            let ghost = self.motion.overlay.ghost(w.id, image.as_ref(), at, &shadow, self.style.drag.ghost_opacity as f32);
            self.motion.window_card = Some(WindowCard { window: w.id, ghost, has_image: image.is_some() });
            self.motion.overlay.show_on_top();
        }
        let o = self.motion.overlay.origin();
        let at = Plane::under_pointer(Point::new(p.x - o.x, p.y - o.y), m.thumb_size(&f), grab, m.tilt, corner);
        if let Some(c) = self.motion.window_card.as_mut() {
            c.ghost.set(at);
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// U8: no transition is drawn on a full-screen Space (the overlay cannot show there).
    #[test]
    fn no_transition_is_drawn_on_a_full_screen_space() {
        assert!(draws_motion(true, false));
        assert!(!draws_motion(true, true));
        assert!(!draws_motion(false, false));
    }
}
