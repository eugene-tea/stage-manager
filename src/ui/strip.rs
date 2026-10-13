//! The strip panel: a borderless, non-activating `NSPanel` on the left of the main display
//! that draws each stage as a card with Core Animation layers, the way Apple's does.
//!
//! - Snapshots arrive through [`strip_model::present`] on any thread and hop to the main
//!   thread; everything here runs on the main thread. Each snapshot carries the
//!   [`StripStyle`] (config), so look and motion follow config edits live.
//! - **Cards** ([`strip_layout`]): each window's thumbnail is its own plane, turned about
//!   its left edge inside its card's perspective container, whose eye sits at the display's
//!   vertical middle. A card is **one unit**: its planes, app icons, badge and name sit in
//!   one 3D group (`CATransformLayer`, so the planes keep their perspective) that moves as a
//!   whole, so an icon never drifts from its thumbnail (with the icons in a separate flat
//!   layer they did, by the hover scale times the distance travelled, owner 10-12); the icons
//!   are flat at depth 0, where the perspective leaves them as they are.
//! - **Hover** grows the card about its center (eased); a click acts on mouse-up in the same
//!   card and goes out as [`StripEvent::CardClicked`], or [`StripEvent::IconClicked`] on its
//!   app icon.
//! - Cards glide to new slots on the spring; the strip **slides** out to the left when
//!   hidden, the cards moving in 3D (they widen as they leave, as Apple's do).
//! - Thumbnails come from [`capture`], refreshed every `thumbnail_interval_ms`.
//! - **Level** (`[strip] level`): Apple's strip sits below windows, so a window moved over
//!   it covers it (and auto-hide slides it away). It is lifted above windows only while a
//!   window is dragged over it, while a card is dragged, and while the edge reveals it over
//!   a covering window; a click on a card never lifts it.
//! - **Drag a card** (press, move more than `[drag] threshold`): within the strip column the
//!   card itself follows the pointer and the others make room on the spring
//!   (`[strip] reorder`; released there it goes out as [`StripEvent::CardMoved`]); past the
//!   column a ghost of its front thumbnail follows the pointer in its own click-through
//!   panel and grows toward the window's size; released outside the strip it goes out as
//!   [`StripEvent::CardDropped`].
//! - **Drop a card on a card** (`[strip] merge_on_drop`, an extension): over the middle part
//!   of another card (`[drag] merge_zone` of its height, measured on the cards at rest) the
//!   reorder gap closes and that card grows and shows the dragged card's windows stacked
//!   behind its own, as the merged card will look; leaving the zone undoes it on the spring.
//!   Released there, the dragged card shrinks into it and fades, and the drop goes out as
//!   [`StripEvent::CardMerged`].
//! - **Drop target**: while the controller reports a window drag
//!   ([`StripSnapshot::window_drag`]) and the pointer is over the strip column, the strip is
//!   lifted above windows, a card-sized snapshot of the window, turned like a card, follows
//!   the pointer, and the cards open a gap at the slot under the pointer
//!   ([`drop_slot`] tells the controller which). Auto-hide never fights either drag.
//! - **Transitions** (`[animation]`, see `strip_motion`): snapshots of the windows that
//!   leave and arrive move in the motion overlay above everything while the real windows
//!   jump behind them.
//! - **Spaces**: each Desktop has a strip panel of its own ([`Pane`]), as Apple's does: made
//!   on the Desktop's first visit while it shows, it lives there and slides with it (AppKit
//!   keeps a window that does not join every Space on the Space it was first ordered in;
//!   ordering it out and in again does not move it). When a snapshot for another Space
//!   arrives, the pane already on the Space shown (`isOnActiveSpace`) takes over with the
//!   cards it had, else a new one; the pane left keeps its cards on its Desktop. A full-screen
//!   app's Space gets none. The motion overlay gets a fresh panel on each Space change; card
//!   drag ghosts join every Space.
//! - Extensions, off by default (`[reveal]`): the **edge tab**, a slim translucent tab at the
//!   left edge while a window covers the strip; the **launch reveal**, a covered strip shown
//!   for a moment at launch.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSColor, NSEvent, NSEventModifierFlags, NSFloatingWindowLevel,
    NSNormalWindowLevel, NSPanel, NSRunningApplication, NSScreen, NSTrackingArea, NSTrackingAreaOptions, NSView,
    NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGImage};
use objc2_foundation::{MainThreadMarker, NSNumber, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer, NSValue};
use objc2_quartz_core::{
    CABasicAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction, CASpringAnimation, CATextLayer,
    CATransaction, CATransform3D, CATransformLayer, NSValueCATransform3DAdditions, kCAAlignmentCenter, kCAAlignmentLeft,
    kCAGravityResizeAspect, kCAGravityResizeAspectFill, kCAMediaTimingFunctionEaseOut, kCATruncationEnd,
};

use crate::config::{Rgba, StripLevel, StripStyle};
use crate::engine::geometry::{Point, Rect, Size};
use crate::engine::stage_area::length_scale;
use crate::platform::capture::{Capturer, Target};
use crate::ui::motion_math::{self, DropSpot};
use crate::ui::panels;
use crate::ui::strip_layout::{self, CardLayout, Metrics};
use crate::ui::strip_model::{self, CardSnapshot, StripDrop, StripEvent, StripSnapshot};
use crate::ui::strip_visibility::{self, Mode, Timing, Visibility};

#[path = "strip_motion.rs"]
mod motion;

/// The gap's stage id in the column while a window is dragged over it (never a real stage).
const DROP_STAGE: u64 = u64::MAX;
/// The strip's window level below windows (Apple's): just under normal windows.
const BELOW_WINDOWS: isize = NSNormalWindowLevel - 1;

/// After a Space change, when to check that the active pane is on the Space shown (seconds).
const REHOME_CHECKS: [f64; 2] = [0.4, 1.2];
/// A window dragged over the strip goes between cards only (dropping it onto a card's
/// stage is not built).
const WINDOW_DROP_BAND: f64 = 0.0;
/// A released card drawn pressed lets go after this if no transition took it (seconds).
const SQUEEZE_MAX: f64 = 0.6;
/// What a card dropped onto another shrinks to, about the target's middle.
const MERGE_SHRINK: f64 = 0.5;
/// The edge tab's window: width and height in points (the drawn pill is narrower).
const TAB_SIZE: (f64, f64) = (10.0, 72.0);
/// The drawn pill's width.
const TAB_PILL_WIDTH: f64 = 5.0;
/// The least distance a press must move to become a drag, whatever `[drag] threshold`
/// says: a click with a hand's tremor of a few points stays a click (owner 10-12: two clicks
/// with a 2–4 pt wobble started a reorder and were lost).
const MIN_DRAG_THRESHOLD: f64 = 5.0;
/// Depth of a card lifted above the others while it is dragged and while it glides into its
/// slot after the release (its whole unit: thumbnails, icons, badge).
const LIFTED_Z: f64 = 100.0;

// ---------------------------------------------------------------------------------------
// Main-thread state

thread_local! {
    static STRIP: RefCell<Option<Strip>> = const { RefCell::new(None) };
}

fn with_strip<R>(f: impl FnOnce(&mut Strip) -> R) -> Option<R> {
    STRIP.with(|s| match s.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().map(f),
        Err(_) => {
            tracing::debug!("strip busy; event dropped");
            None
        }
    })
}

/// Creates the strip (hidden until the first snapshot), registers it as the
/// [`strip_model`] presenter and starts thumbnail capture every `thumbnail_interval_ms`.
/// Main thread, once.
pub fn install(mtm: MainThreadMarker, thumbnail_interval_ms: u64) {
    let strip = Strip::new(mtm, Duration::from_millis(thumbnail_interval_ms.max(1)));
    STRIP.with(|s| *s.borrow_mut() = Some(strip));
    strip_model::set_presenter(|snapshot| {
        DispatchQueue::main().exec_async(move || {
            with_strip(|s| s.present(snapshot));
        });
    });
    tracing::info!(thumbnail_interval_ms, "strip installed");
}

/// The user went to another Space (`activeSpaceDidChangeNotification`). Main thread.
pub fn space_changed() {
    with_strip(Strip::space_changed);
}

/// Apple's Stage Manager took over: every other Desktop's pane closes, and the pane shown
/// hides with no cards (a drag in progress ends). The next snapshot after the stand-down
/// brings the strip back. Main thread.
pub fn stand_down() {
    with_strip(|s| {
        s.others.clear();
        s.card_drag = None;
        s.pressed = None;
        s.squeezed = None;
        let mut snapshot = s.snapshot.clone();
        snapshot.cards.clear();
        snapshot.user_visible = false;
        snapshot.transition = None;
        snapshot.parked = None;
        snapshot.arrived = None;
        snapshot.window_drag = None;
        snapshot.active_windows = Default::default();
        s.present(snapshot);
        tracing::info!("strip: hidden on every Desktop (Apple's Stage Manager is on)");
    });
}

/// Changes the thumbnail refresh interval (config reload). Main thread.
pub fn set_thumbnail_interval(ms: u64) {
    with_strip(|s| s.capturer.set_interval(Duration::from_millis(ms.max(1))));
}

/// Where a window released at `p` (global, top-left origin) goes: `None` when it is not over
/// the strip (the user hid the strip, or `p` is outside the strip column), else the slot under
/// `p`, the same one the strip's gap shows. Main thread.
pub fn drop_slot(p: Point) -> Option<StripDrop> {
    with_strip(|s| (s.snapshot.user_visible && s.geometry.global.contains(p)).then(|| s.drop_slot_at(p))).flatten()
}

/// The pointer moved to `p` (global, top-left origin) over another app's window or the
/// desktop (the global monitor). Main thread.
pub fn pointer_moved(p: Point) {
    with_strip(|s| s.pointer_moved(p));
}

/// What the strip draws now, for a scripted test (`ctl debug-strip`): the cards in slot
/// order with each card's front thumbnail and app icons where they are drawn this frame
/// (global, top-left, read back from Core Animation's presentation layers), each card's
/// depth, the lifted card, and the motion overlay's state. Main thread.
pub fn probe() -> serde_json::Value {
    with_strip(|s| s.probe()).unwrap_or(serde_json::Value::Null)
}

/// The motion overlay's window number and the global point of its top-left corner, for a
/// test's capture of what it draws. Main thread.
pub fn overlay_window() -> Option<(isize, Point)> {
    with_strip(|s| s.motion_overlay()).flatten()
}

/// Refreshes the full-size snapshots of the shown windows now: a switch may follow (the
/// app switcher was opened). Main thread.
pub fn prefetch() {
    with_strip(|s| s.prefetch(None));
}

/// Draws the strip, slid in, into `path` with the offscreen renderer (see
/// [`crate::ui::strip_render`]). Main thread.
#[cfg(feature = "debug-render")]
pub fn debug_render(path: &std::path::Path) -> Result<(), String> {
    with_strip(|s| {
        s.slide(true, false);
        let root = CALayer::new();
        root.setGeometryFlipped(true);
        root.setFrame(s.pane.root.bounds());
        root.setBackgroundColor(Some(&color(0.93, 0.55, 0.22, 1.0)));
        s.pane.cards_root.removeFromSuperlayer();
        root.addSublayer(&s.pane.cards_root);
        let result = crate::ui::strip_render::render_to_file(&root, 2.0, path);
        s.pane.cards_root.removeFromSuperlayer();
        s.pane.root.addSublayer(&s.pane.cards_root);
        result
    })
    .unwrap_or_else(|| Err("no strip".into()))
}

#[cfg(feature = "debug-render")]
thread_local! {
    static SCREEN_OVERRIDE: std::cell::Cell<Option<(NSRect, NSRect)>> = const { std::cell::Cell::new(None) };
}

/// Lays the strip out for another screen (Cocoa frame and visible frame), so a render can be
/// compared with a screenshot of another display. Main thread.
#[cfg(feature = "debug-render")]
pub fn set_screen_override(frame: NSRect, visible: NSRect) {
    SCREEN_OVERRIDE.with(|o| o.set(Some((frame, visible))));
}

/// Receives captured thumbnails on the main thread.
fn thumbnail_arrived(id: u32, image: CFRetained<CGImage>) {
    with_strip(|s| s.set_thumbnail(id, image));
}

/// Receives full-size window snapshots (for transitions) on the main thread.
fn snapshot_arrived(id: u32, image: CFRetained<CGImage>) {
    with_strip(|s| s.set_snapshot(id, image));
}

// ---------------------------------------------------------------------------------------
// The view: flipped coordinates, first mouse, mouse events

define_class!(
    // SAFETY: NSView has no subclassing requirements; StripView does not implement Drop.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[name = "SMStripView"]
    pub struct StripView;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for StripView {}

    impl StripView {
        // SAFETY (all methods): signatures match NSView / NSResponder.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.local_point(event);
            with_strip(|s| s.mouse_down(p));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _event: &NSEvent) {
            with_strip(Strip::mouse_dragged);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let p = self.local_point(event);
            let shift = event.modifierFlags().contains(NSEventModifierFlags::Shift);
            // Emit outside the borrow: the sink may call back into the strip.
            if let Some(Some(e)) = with_strip(|s| s.mouse_up(p, shift)) {
                tracing::debug!(?e, "strip click");
                strip_model::emit(e);
            }
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let p = self.local_point(event);
            with_strip(|s| s.hover_at(Some(p)));
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, event: &NSEvent) {
            let p = self.local_point(event);
            with_strip(|s| s.hover_at(Some(p)));
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            with_strip(|s| s.hover_at(None));
        }
    }
);

impl StripView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's designated initializer on a fresh instance.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// The event's location in view coordinates (top-left origin).
    fn local_point(&self, event: &NSEvent) -> Point {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        Point::new(p.x, p.y)
    }
}

define_class!(
    // SAFETY: NSView has no subclassing requirements; EdgeTabView does not implement Drop.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[name = "SMEdgeTabView"]
    pub struct EdgeTabView;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for EdgeTabView {}

    impl EdgeTabView {
        // SAFETY (all methods): signatures match NSView / NSResponder.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        // Taken here so the release comes to this view too.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {}

        // The tab acts on release, once the click is over: ordered out while its own click
        // was still being handled, the window server put it back on screen at the release
        // (measured 10-12: gone for 60 ms, then listed again over the revealed strip).
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            DispatchQueue::main().exec_async(|| {
                with_strip(Strip::tab_clicked);
            });
        }
    }
);

impl EdgeTabView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's designated initializer on a fresh instance.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// The edge tab: a slim pill at the left edge in a small non-activating panel of its own.
struct EdgeTab {
    panel: Retained<NSPanel>,
    _view: Retained<EdgeTabView>,
    pill: Retained<CALayer>,
    shown: bool,
}

impl EdgeTab {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Self {
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setLevel(NSFloatingWindowLevel);
        // Lives on its pane's Desktop (the one it is first ordered in on), never full screen;
        // hidden by Mission Control, App Exposé and Show Desktop.
        panel.setCollectionBehavior(panels::desktop_panel());
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        // SAFETY: the panel is owned by the tab for the life of the app.
        unsafe { panel.setReleasedWhenClosed(false) };
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
        let view = EdgeTabView::new(mtm, bounds);
        let root = CALayer::new();
        view.setLayer(Some(&root));
        view.setWantsLayer(true);
        let pill = CALayer::new();
        pill.setCornerRadius(TAB_PILL_WIDTH / 2.0);
        pill.setBorderWidth(0.5);
        root.addSublayer(&pill);
        panel.setContentView(Some(&view));
        let tab = Self { panel, _view: view, pill, shown: false };
        tab.place(frame);
        tab
    }

    /// Sizes the panel and its pill (flush left, full height).
    fn place(&self, frame: NSRect) {
        self.panel.setFrame_display(frame, false);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.pill.setFrame(CGRect::new(CGPoint::new(1.0, 0.0), CGSize::new(TAB_PILL_WIDTH, frame.size.height)));
        CATransaction::commit();
    }

    /// Translucent colors for the current light or dark appearance.
    fn paint(&self, mtm: MainThreadMarker) {
        let name = NSApplication::sharedApplication(mtm).effectiveAppearance().name().to_string();
        let dark = name.contains("Dark");
        let (fill, edge) = if dark { (color(1.0, 1.0, 1.0, 0.42), color(0.0, 0.0, 0.0, 0.25)) } else { (color(0.0, 0.0, 0.0, 0.28), color(1.0, 1.0, 1.0, 0.45)) };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.pill.setBackgroundColor(Some(&fill));
        self.pill.setBorderColor(Some(&edge));
        CATransaction::commit();
    }

    fn show(&mut self, on: bool, mtm: MainThreadMarker) {
        if on == self.shown {
            return;
        }
        self.shown = on;
        if on {
            self.paint(mtm);
            self.panel.orderFrontRegardless();
        } else {
            self.panel.orderOut(None);
        }
        tracing::debug!(shown = on, "edge tab");
    }
}

// ---------------------------------------------------------------------------------------
// Geometry of the panel on the main display

#[derive(Clone, Debug, PartialEq)]
struct Geometry {
    /// The main display's frame, Cocoa coordinates.
    screen: NSRect,
    /// Panel frame, Cocoa coordinates (bottom-left origin).
    panel: NSRect,
    /// The strip area in global top-left coordinates.
    global: Rect,
    /// Height of the primary display (for flipping Cocoa y).
    primary_height: f64,
    /// The style resolved for this display.
    m: Metrics,
}

impl Geometry {
    fn current(mtm: MainThreadMarker, style: &StripStyle) -> Self {
        let screens = NSScreen::screens(mtm);
        let main = screens.firstObject();
        let (frame, visible) = match &main {
            Some(s) => (s.frame(), s.visibleFrame()),
            None => {
                let r = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0));
                (r, r)
            }
        };
        #[cfg(feature = "debug-render")]
        let (frame, visible) = SCREEN_OVERRIDE.with(|o| o.get()).unwrap_or((frame, visible));
        let primary_height = frame.size.height;
        let size = Size::new(visible.size.width, visible.size.height);
        let k = length_scale(size, style);
        let top = primary_height - (visible.origin.y + visible.size.height);
        let display_middle = primary_height - (frame.origin.y + frame.size.height / 2.0);
        let m = Metrics::new(style, size, k, display_middle - top);
        let panel = NSRect::new(visible.origin, NSSize::new(m.width, visible.size.height));
        let global = Rect::new(visible.origin.x, top, m.width, visible.size.height);
        Self { screen: frame, panel, global, primary_height, m }
    }

    /// The edge tab's frame, global (top-left origin): flush left, vertically centered on
    /// the strip.
    fn tab(&self) -> Rect {
        let (w, h) = TAB_SIZE;
        Rect::new(self.global.x, self.global.y + (self.global.h - h) / 2.0, w, h)
    }

    /// A global rect as a Cocoa screen rect.
    fn cocoa(&self, r: Rect) -> NSRect {
        NSRect::new(NSPoint::new(r.x, self.primary_height - (r.y + r.h)), NSSize::new(r.w, r.h))
    }

    fn height(&self) -> f64 {
        self.panel.size.height
    }

    /// Converts a Cocoa screen point to global top-left coordinates.
    fn global_point(&self, p: NSPoint) -> Point {
        Point::new(p.x, self.primary_height - p.y)
    }
}

// ---------------------------------------------------------------------------------------
// Layers

/// One card's layers.
struct CardLayers {
    /// The card's perspective container: covers the panel, anchored at the eye; its
    /// `sublayerTransform` is the slide and the perspective, its `transform` the hover.
    card: Retained<CALayer>,
    /// The card's content as one unit (planes, icons, badge, name), a 3D group inside the
    /// container: its `position` is the card's offset from where its content was built, so a
    /// glide moves everything together.
    unit: Retained<CATransformLayer>,
    /// Thumbnail planes per window id (back to front), each turned about its left edge.
    planes: Vec<(u32, Retained<CALayer>)>,
    /// Image layer per window id.
    images: Vec<(u32, Retained<CALayer>)>,
    /// App icon layers, in icon order.
    icons: Vec<Retained<CALayer>>,
    /// The card's top-left corner when its content was built.
    built: Point,
    /// What the layers were built from.
    snapshot: CardSnapshot,
    layout: CardLayout,
}

fn color(r: f64, g: f64, b: f64, a: f64) -> CFRetained<CGColor> {
    CGColor::new_srgb(r, g, b, a)
}

fn rgba(c: Rgba) -> CFRetained<CGColor> {
    color(c.r, c.g, c.b, c.a)
}

fn cg_rect(r: &Rect) -> CGRect {
    CGRect::new(CGPoint::new(r.x, r.y), CGSize::new(r.w, r.h))
}

/// Views a CF object as an Objective-C object (CF types are toll-free bridged).
fn as_object<T>(cf: &T) -> &AnyObject {
    // SAFETY: every CF type is a valid Objective-C object pointer.
    unsafe { &*(cf as *const T).cast::<AnyObject>() }
}

fn transform_value(t: CATransform3D) -> Retained<NSValue> {
    // SAFETY: QuartzCore's NSValue category; any transform is valid.
    unsafe { NSValue::valueWithCATransform3D(t) }
}

fn identity() -> CATransform3D {
    CATransform3D::new_scale(1.0, 1.0, 1.0)
}

/// A scale by `s` about `c`, for a layer anchored at `anchor` (both panel coordinates).
fn scale_about(s: f64, c: Point, anchor: Point) -> CATransform3D {
    let (dx, dy) = (c.x - anchor.x, c.y - anchor.y);
    CATransform3D::new_translation(-dx, -dy, 0.0)
        .concat(CATransform3D::new_scale(s, s, 1.0))
        .concat(CATransform3D::new_translation(dx, dy, 0.0))
}

/// A card container's `sublayerTransform`: slid `dx` in 3D, then the perspective.
fn perspective(m: &Metrics, dx: f64) -> CATransform3D {
    let mut p = identity();
    p.m34 = -1.0 / m.perspective.max(1.0);
    CATransform3D::new_translation(dx, 0.0, 0.0).concat(p)
}

/// A thumbnail plane's turn about its left edge.
fn turn(m: &Metrics) -> CATransform3D {
    CATransform3D::new_rotation(m.tilt, 0.0, 1.0, 0.0)
}

fn spring(s: motion_math::Spring, key: &str, from: &AnyObject, to: &AnyObject) -> Retained<CASpringAnimation> {
    let anim = CASpringAnimation::animationWithKeyPath(Some(&NSString::from_str(key)));
    anim.setMass(s.mass);
    anim.setStiffness(s.stiffness);
    anim.setDamping(s.damping);
    anim.setDuration(anim.settlingDuration());
    // SAFETY: from/to are NSValues of the animated property's type.
    unsafe {
        anim.setFromValue(Some(from));
        anim.setToValue(Some(to));
    }
    anim
}

fn ease(key: &str, from: &AnyObject, to: &AnyObject, seconds: f64) -> Retained<CABasicAnimation> {
    let anim = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str(key)));
    anim.setDuration(seconds);
    // SAFETY: the timing function name is a framework constant.
    anim.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(unsafe { kCAMediaTimingFunctionEaseOut })));
    // SAFETY: from/to are values of the animated property's type.
    unsafe {
        anim.setFromValue(Some(from));
        anim.setToValue(Some(to));
    }
    anim
}

/// Moves `layer` to `to` along the spring, starting from where it is drawn now.
fn spring_position(s: motion_math::Spring, layer: &CALayer, to: CGPoint) {
    // SAFETY: presentationLayer returns a copy of the layer's drawn state or nil.
    let from = unsafe { layer.presentationLayer() }.map(|p| p.position()).unwrap_or_else(|| layer.position());
    layer.setPosition(to);
    if (from.x - to.x).abs() < 0.25 && (from.y - to.y).abs() < 0.25 {
        return;
    }
    let anim = spring(s, "position", &NSValue::new(from), &NSValue::new(to));
    layer.addAnimation_forKey(&anim, Some(&NSString::from_str("position")));
}

/// Moves `layer` to `to` on the spring `glide`, or at once (`immediate`, or no animation:
/// cancelling any glide).
fn move_to(glide: Option<motion_math::Spring>, layer: &CALayer, to: CGPoint, immediate: bool) {
    match glide.filter(|_| !immediate) {
        Some(s) => spring_position(s, layer, to),
        None => {
            layer.removeAnimationForKey(&NSString::from_str("position"));
            layer.setPosition(to);
        }
    }
}

/// Moves `layer`'s `sublayerTransform` to `to` along the spring, from what is drawn now.
fn spring_sublayer_transform(s: motion_math::Spring, layer: &CALayer, to: CATransform3D) {
    // SAFETY: presentationLayer returns a copy of the layer's drawn state or nil.
    let from = unsafe { layer.presentationLayer() }
        .map(|p| p.sublayerTransform())
        .unwrap_or_else(|| layer.sublayerTransform());
    layer.setSublayerTransform(to);
    let anim = spring(s, "sublayerTransform", &transform_value(from), &transform_value(to));
    layer.addAnimation_forKey(&anim, Some(&NSString::from_str("sublayerTransform")));
}

/// Eases `layer`'s transform to `to` from what is drawn now.
fn ease_transform(layer: &CALayer, to: CATransform3D, seconds: f64) {
    // SAFETY: presentationLayer returns a copy of the layer's drawn state or nil.
    let from = unsafe { layer.presentationLayer() }.map(|p| p.transform()).unwrap_or_else(|| layer.transform());
    layer.setTransform(to);
    if seconds <= 0.0 {
        return;
    }
    let anim = ease("transform", &transform_value(from), &transform_value(to), seconds);
    layer.addAnimation_forKey(&anim, Some(&NSString::from_str("transform")));
}

fn fade_in(layer: &CALayer, seconds: f64) {
    if seconds <= 0.0 {
        return;
    }
    let anim = ease("opacity", &NSNumber::new_f32(0.0), &NSNumber::new_f32(1.0), seconds);
    layer.addAnimation_forKey(&anim, Some(&NSString::from_str("fade")));
}

fn text_layer(text: &str, size: f64, scale: f64, center: bool) -> Retained<CATextLayer> {
    let layer = CATextLayer::new();
    // SAFETY: an NSString is a valid `string` value.
    unsafe { layer.setString(Some(&NSString::from_str(text))) };
    let font = NSString::from_str("HelveticaNeue-Medium");
    // SAFETY: a font name string is a valid `font` value.
    unsafe { layer.setFont(Some(as_object_cf(&font))) };
    layer.setFontSize(size);
    layer.setForegroundColor(Some(&color(1.0, 1.0, 1.0, 1.0)));
    // SAFETY: framework constants.
    unsafe {
        layer.setAlignmentMode(if center { kCAAlignmentCenter } else { kCAAlignmentLeft });
        layer.setTruncationMode(kCATruncationEnd);
    }
    layer.setContentsScale(scale);
    layer
}

/// Views an NSString as the CFType that `CATextLayer.font` takes (toll-free bridged).
fn as_object_cf(s: &NSString) -> &objc2_core_foundation::CFType {
    // SAFETY: NSString is toll-free bridged with CFString, a CFType.
    unsafe { &*(s as *const NSString).cast::<objc2_core_foundation::CFType>() }
}

/// An app's icon as an image for a layer, rendered at `points` × 2 pixels.
fn app_icon(pid: i32, points: f64) -> Option<CFRetained<CGImage>> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    let icon = app.icon()?;
    let side = (points * 2.0).clamp(32.0, 1024.0);
    let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(side, side));
    // SAFETY: a valid proposed rect pointer and no context or hints.
    let image = unsafe { icon.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
    // SAFETY: the Retained<CGImage> owns one reference; re-wrap it as CFRetained.
    let ptr = NonNull::new(Retained::into_raw(image))?;
    // SAFETY: as above; the pointer came from `into_raw`, so its reference moves over.
    Some(unsafe { CFRetained::from_raw(ptr) })
}

// ---------------------------------------------------------------------------------------
// Card drag

/// The ghost of a dragged card: its front thumbnail in a click-through panel of its own.
struct Ghost {
    panel: Retained<NSPanel>,
    layer: Retained<CALayer>,
}

impl Ghost {
    fn new(mtm: MainThreadMarker, image: Option<&CGImage>, opacity: f32) -> Self {
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setLevel(NSFloatingWindowLevel + 1);
        panel.setIgnoresMouseEvents(true);
        panel.setCollectionBehavior(panels::drag_ghost());
        // SAFETY: the panel is owned by the ghost and closed by it.
        unsafe { panel.setReleasedWhenClosed(false) };
        let view = NSView::initWithFrame(NSView::alloc(mtm), rect);
        let layer = CALayer::new();
        view.setLayer(Some(&layer));
        view.setWantsLayer(true);
        layer.setCornerRadius(9.0);
        layer.setMasksToBounds(true);
        layer.setBorderWidth(0.5);
        layer.setBorderColor(Some(&color(1.0, 1.0, 1.0, 0.35)));
        layer.setBackgroundColor(Some(&color(0.22, 0.22, 0.24, 0.95)));
        layer.setOpacity(opacity);
        // SAFETY: framework constant.
        layer.setContentsGravity(unsafe { kCAGravityResizeAspectFill });
        if let Some(img) = image {
            // SAFETY: a CGImage is valid layer contents.
            unsafe { layer.setContents(Some(as_object(img))) };
        }
        panel.setContentView(Some(&view));
        Self { panel, layer }
    }

    /// Places the ghost at `r` (global, top-left origin).
    fn set_frame(&self, r: Rect, primary_height: f64) {
        let cocoa = NSRect::new(NSPoint::new(r.x, primary_height - (r.y + r.h)), NSSize::new(r.w, r.h));
        self.panel.setFrame_display(cocoa, true);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.layer.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(r.w, r.h)));
        CATransaction::commit();
        self.panel.orderFrontRegardless();
    }

    fn set_image(&self, image: &CGImage) {
        // SAFETY: a CGImage is valid layer contents.
        unsafe { self.layer.setContents(Some(as_object(image))) };
    }
}

impl Drop for Ghost {
    fn drop(&mut self) {
        self.panel.orderOut(None);
        self.panel.close();
    }
}

/// What a pressed card is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragMode {
    /// Pressed, not moved past the threshold: a click on release.
    Pressed,
    /// Following the pointer within the strip column: to another slot (`[strip] reorder`) or
    /// onto another card (`[strip] merge_on_drop`).
    Reorder,
    /// Dragged out: the ghost follows the pointer and grows.
    Out,
}

/// A press on a card that may become a drag.
struct CardDrag {
    stage: u64,
    mode: DragMode,
    /// The card's layout when pressed (it follows the pointer from there).
    origin: CardLayout,
    /// While reordering: the stage whose card it goes above (`None`: the bottom).
    before: Option<u64>,
    /// While reordering: the card it would merge into, when held over its middle.
    onto: Option<u64>,
    /// Where the press was (global).
    press: Point,
    /// The front thumbnail where it was pressed (global).
    thumb: Rect,
    /// The press point as fractions of the thumbnail's size.
    grab: (f64, f64),
    /// The front window's size.
    full: Size,
    /// The front window's id (its thumbnail is the ghost's image).
    front: u32,
    /// The ghost, once the press moved past the threshold.
    ghost: Option<Ghost>,
}

/// Where a card or window released over the column goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot {
    /// Into the gap right above this stage's card (`None`: below the last card).
    Before(Option<u64>),
    /// Onto this stage's card (a merge).
    Onto(u64),
}

// ---------------------------------------------------------------------------------------
// One Desktop's panel

/// One Desktop's strip: a panel that belongs to the Desktop it was first ordered in on and
/// slides with it, as Apple's strip does, with its own cards, slide, level and edge tab.
///
/// AppKit keeps a window that does not join every Space on the Space it was first ordered
/// in (ordering it out and in again brings it back there), so the strip keeps one pane per
/// Desktop instead of moving one panel around. Which pane a Desktop has is read from the
/// panels themselves (`isOnActiveSpace`), never from the engine's synthetic Space ids.
struct Pane {
    panel: Retained<NSPanel>,
    _view: Retained<StripView>,
    root: Retained<CALayer>,
    /// The cards' perspective containers (each card whole: planes, icons, badge, name).
    cards_root: Retained<CALayer>,
    cards: HashMap<u64, CardLayers>,
    /// Slid in.
    shown: bool,
    /// The panel is above windows now (`None` before the first decision).
    lifted: Option<bool>,
    /// The panel takes mouse events now (on a card, or a press or drag of one); else they
    /// go through to what lies below (`None` before the first decision).
    catches: Option<bool>,
    /// The edge tab shown while a window hides the strip (`[reveal] edge_tab`).
    tab: EdgeTab,
    /// The engine's id of the Space whose cards it showed last.
    space: u64,
    /// Ordered in once: from then on it lives on that Desktop.
    placed: bool,
    /// The geometry its panel and layers were sized for.
    geometry: Option<Geometry>,
    /// The style and backing scale its cards were built with.
    built: Option<(Arc<StripStyle>, f64)>,
}

impl Pane {
    /// A new pane (not ordered in, so not on any Desktop yet), sized for `geometry`.
    fn new(mtm: MainThreadMarker, geometry: &Geometry) -> Self {
        let style_mask = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            geometry.panel,
            style_mask,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setLevel(BELOW_WINDOWS);
        // One Desktop (the one it is first ordered in on), never a full-screen app's Space;
        // hidden by Mission Control, App Exposé and Show Desktop, as Apple's strip is.
        panel.setCollectionBehavior(panels::desktop_panel());
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setAcceptsMouseMovedEvents(true);
        // SAFETY: the panel is owned by its pane and closed when the pane goes.
        unsafe { panel.setReleasedWhenClosed(false) };

        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), geometry.panel.size);
        let view = StripView::new(mtm, bounds);
        let root = CALayer::new();
        root.setGeometryFlipped(true);
        // Layer-hosting: set the layer before turning layers on.
        view.setLayer(Some(&root));
        view.setWantsLayer(true);
        let options = NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the view owns the tracking area and outlives it; no user info.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(NSTrackingArea::alloc(), bounds, options, Some(&view), None)
        };
        view.addTrackingArea(&area);
        panel.setContentView(Some(&view));

        let cards_root = CALayer::new();
        root.addSublayer(&cards_root);
        let tab = EdgeTab::new(mtm, geometry.cocoa(geometry.tab()));
        Self {
            panel,
            _view: view,
            root,
            cards_root,
            cards: HashMap::new(),
            shown: false,
            lifted: None,
            catches: None,
            tab,
            space: 0,
            placed: false,
            geometry: None,
            built: None,
        }
    }

    /// Ordered in and on the Space shown now.
    fn on_space_shown(&self) -> bool {
        self.placed && self.panel.isVisible() && self.panel.isOnActiveSpace()
    }

    fn status(&self, active: bool) -> strip_model::PaneStatus {
        strip_model::PaneStatus {
            space: self.space,
            active,
            ordered_in: self.panel.isVisible(),
            on_active_space: self.panel.isOnActiveSpace(),
            shown: self.shown,
            behavior: self.panel.collectionBehavior().0,
        }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.tab.panel.orderOut(None);
        self.tab.panel.close();
        self.panel.orderOut(None);
        self.panel.close();
    }
}

// ---------------------------------------------------------------------------------------
// The strip

struct Strip {
    mtm: MainThreadMarker,
    /// The pane of the Desktop shown (or last shown, on a full-screen app's Space).
    pane: Pane,
    /// Other Desktops' panes, each on its own Desktop with the cards it had when left.
    others: Vec<Pane>,
    style: Arc<StripStyle>,
    geometry: Geometry,
    scale: f64,
    layouts: Vec<CardLayout>,
    snapshot: StripSnapshot,
    has_snapshot: bool,
    hovered: Option<u64>,
    pressed: Option<u64>,
    /// The card drawn pressed (`[cards] press_scale`): from the mouse-down until its
    /// transition takes it, the press turns into a drag, or nothing came of the click.
    squeezed: Option<u64>,
    thumbs: HashMap<u32, CFRetained<CGImage>>,
    icons: HashMap<i32, Option<CFRetained<CGImage>>>,
    visibility: Visibility,
    poll_timer: Option<Retained<NSTimer>>,
    capturer: Capturer,
    card_drag: Option<CardDrag>,
    /// A card lifted above the others (dragged, or gliding into its slot after the release),
    /// with the timer that puts it back once it has settled.
    lifted_card: Option<(u64, Option<Retained<NSTimer>>)>,
    /// The card order last logged (stage ids, top to bottom).
    logged_order: Vec<u64>,
    /// A window drag is over the strip: the gap shows (at `drop_before`'s card).
    drop_over: bool,
    drop_before: Option<u64>,
    /// The launch reveal happened (once per launch).
    announced: bool,
    /// Counts the Space changes macOS reported (`activeSpaceDidChangeNotification`).
    space_epoch: u64,
    /// Snapshots, transitions and the dragged window's card (`strip_motion`).
    motion: motion::Motion,
}

impl Strip {
    fn new(mtm: MainThreadMarker, interval: Duration) -> Self {
        let style = Arc::new(StripStyle::default());
        let geometry = Geometry::current(mtm, &style);
        let pane = Pane::new(mtm, &geometry);
        let scale = pane.panel.backingScaleFactor();
        let capturer = Capturer::start(interval, thumbnail_arrived);
        let mut strip = Self {
            mtm,
            pane,
            others: Vec::new(),
            style,
            geometry,
            scale,
            layouts: Vec::new(),
            snapshot: StripSnapshot::default(),
            has_snapshot: false,
            hovered: None,
            pressed: None,
            squeezed: None,
            thumbs: HashMap::new(),
            icons: HashMap::new(),
            visibility: Visibility::default(),
            poll_timer: None,
            capturer,
            card_drag: None,
            lifted_card: None,
            logged_order: Vec::new(),
            drop_over: false,
            drop_before: None,
            announced: false,
            space_epoch: 0,
            motion: motion::Motion::new(mtm),
        };
        strip.apply_geometry();
        strip.slide(false, false);
        strip
    }

    fn m(&self) -> &Metrics {
        &self.geometry.m
    }

    /// The cards' glide (`[motion] spring_*` at `[animation] speed`); `None`: no animation.
    fn glide(&self) -> Option<motion_math::Spring> {
        self.style.animation.animates().then(|| motion_math::glide_spring(&self.style))
    }

    /// The strip's slide (`[motion] slide_*` at `[animation] speed`); `None`: no animation.
    fn slide_spring(&self) -> Option<motion_math::Spring> {
        self.style.animation.animates().then(|| motion_math::slide_spring(&self.style))
    }

    /// How far left the cards sit when hidden.
    fn hidden_dx(&self) -> f64 {
        -(self.style.motion.slide_distance * self.m().k).max(self.geometry.panel.size.width + 30.0)
    }

    fn current_dx(&self) -> f64 {
        if self.pane.shown { 0.0 } else { self.hidden_dx() }
    }

    /// Sizes the pane's panel, its layer tree and its edge tab for the current geometry;
    /// its cards are rebuilt.
    fn apply_geometry(&mut self) {
        let g = &self.geometry;
        self.pane.panel.setFrame_display(g.panel, false);
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(g.panel.size.width, g.panel.size.height));
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.pane.root.setFrame(bounds);
        self.pane.cards_root.setFrame(bounds);
        CATransaction::commit();
        self.pane.tab.place(g.cocoa(g.tab()));
        self.pane.geometry = Some(g.clone());
        self.clear_cards();
        self.icons.clear();
        self.visibility.set_timing(Timing::from_style(&self.style.reveal));
    }

    /// Drops every card's layers at once (no animation).
    fn clear_cards(&mut self) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for layers in self.pane.cards.values() {
            layers.card.removeFromSuperlayer();
        }
        CATransaction::commit();
        self.pane.cards.clear();
    }

    // -- snapshots ------------------------------------------------------------------------

    fn present(&mut self, snapshot: StripSnapshot) {
        if snapshot.style != self.style {
            self.style = snapshot.style.clone();
            // The reveal's delays are no part of the geometry: a new `[reveal]` (the config
            // read at launch, a reload) takes effect here, not only when the display changes
            // (the strip was built with the defaults, so `hide_delay_ms` never applied).
            self.visibility.set_timing(Timing::from_style(&self.style.reveal));
        }
        let geometry = Geometry::current(self.mtm, &self.style);
        if geometry != self.geometry {
            tracing::debug!(k = geometry.m.k, tilt = geometry.m.tilt.to_degrees(), d = geometry.m.perspective, "strip geometry");
            self.geometry = geometry;
        }
        let transition = snapshot.transition.clone();
        if let Some(sent) = transition.as_ref().and_then(|t| t.sent) {
            tracing::debug!(id = transition.as_ref().map(|t| t.id), queued_ms = sent.elapsed().as_millis() as u64, "transition reached the strip");
        }
        // Where incoming windows grow from: their cards before this snapshot replaces them
        // (slid as the strip is now; moved to its new slide state below).
        let dx_before = self.current_dx();
        let starts = transition.as_ref().map(|t| self.incoming_starts(t));
        // Another Space (or a full-screen one): the pane of the Desktop shown takes over (it
        // was left there with that Desktop's cards, or is new); the one left stays on its
        // Desktop and slides away with it.
        let rehome = !self.has_snapshot
            || snapshot.space != self.snapshot.space
            || snapshot.fullscreen != self.snapshot.fullscreen;
        if rehome && self.has_snapshot {
            tracing::debug!(space = snapshot.space, fullscreen = snapshot.fullscreen, "strip: another Space");
            self.leave_pane();
        }
        let fullscreen = snapshot.fullscreen;
        if rehome && !fullscreen {
            self.select_pane();
        }
        if fullscreen {
            self.keep_off_fullscreen();
        } else {
            self.pane.space = snapshot.space;
        }
        let force = self.sync_pane(&snapshot.style);
        self.snapshot = snapshot;
        if self.snapshot.window_drag.is_none() {
            self.drop_over = false;
        }
        self.layout_cards(force);
        if let (Some(t), Some(starts)) = (transition, starts) {
            // Decide whether this snapshot shows the strip before aiming: a switch that
            // reveals (or hides) the strip lands its snapshots where the cards will be, not
            // where they were (review R1-ui 1).
            self.update_visibility();
            // Incoming windows grow from where their cards will be too (review R2 V2): a
            // start on a card follows the slide this snapshot decided.
            let starts = slide_card_starts(starts, self.current_dx() - dx_before);
            self.begin_transition(t, starts);
        }
        if let Some(id) = self.snapshot.parked {
            self.outgoing_parked(id);
        }
        if let Some((id, frames)) = self.snapshot.arrived.clone() {
            self.incoming_arrived(id, &frames);
        }
        self.after_snapshot();
        // Cards moved under a resting pointer: the pane catches clicks where they are now.
        self.update_pass_through(None);
        if rehome {
            self.has_snapshot = true;
            if !fullscreen {
                self.order_in();
                // Ordered in for the first time while the Space still slides, a new pane
                // could land on the Desktop being left: look again once the slide is over.
                let epoch = self.space_epoch;
                for seconds in REHOME_CHECKS {
                    let check = RcBlock::new(move |_t: NonNull<NSTimer>| {
                        with_strip(|s| s.ensure_on_active_space(epoch));
                    });
                    // SAFETY: the block runs once on the main run loop.
                    let _once = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, false, &check) };
                }
            }
        }
        if !fullscreen {
            self.drop_duplicates();
        }
        if self.snapshot.announce && !self.announced {
            self.announced = true;
            if self.covered() {
                tracing::info!("launch: a window covers the strip; showing it for a moment");
                self.visibility.announce(Instant::now(), Duration::from_millis(self.style.reveal.launch_reveal_ms));
            }
        }
        self.update_visibility();
        self.update_targets();
        self.publish_panel();
    }

    // -- panes ----------------------------------------------------------------------------

    /// The Desktop shown is being left: what the user was doing on its strip ends, its tab
    /// goes and its panel goes back below windows; its cards stay as they are, so it slides
    /// back in with them when the Desktop shows again.
    fn leave_pane(&mut self) {
        self.pressed = None;
        self.squeezed = None;
        self.set_hover(None);
        self.card_drag = None;
        self.drop_over = false;
        self.pane.tab.show(false, self.mtm);
        let resting = self.style.level == StripLevel::AboveWindows;
        if self.pane.lifted.is_some_and(|l| l != resting) {
            // Only the level: ordering it in now could take it to another Space.
            self.pane.panel.setLevel(if resting { NSFloatingWindowLevel } else { BELOW_WINDOWS });
            self.pane.lifted = Some(resting);
        }
    }

    /// Makes the pane of the Desktop shown the active one: the pane on it already, else one
    /// never ordered in, else a new one (it inherits the slide, so the strip simply shows).
    fn select_pane(&mut self) {
        if self.pane.on_space_shown() {
            return;
        }
        let next = if let Some(i) = self.others.iter().position(Pane::on_space_shown) {
            self.others.swap_remove(i)
        } else if !self.pane.placed {
            return;
        } else if let Some(i) = self.others.iter().position(|p| !p.placed) {
            self.others.swap_remove(i)
        } else {
            let mut pane = Pane::new(self.mtm, &self.geometry);
            pane.shown = self.pane.shown;
            pane
        };
        let old = std::mem::replace(&mut self.pane, next);
        tracing::info!(
            left = old.space,
            panes = self.others.len() + 2,
            reused = self.pane.placed,
            "strip: the pane of the Desktop shown takes over"
        );
        self.others.push(old);
        self.layouts.clear();
        self.hovered = None;
    }

    /// Brings the active pane up to date with the current geometry and `style`; true when its
    /// cards must be rebuilt (the style or the backing scale changed since they were built).
    fn sync_pane(&mut self, style: &Arc<StripStyle>) -> bool {
        if self.pane.geometry.as_ref() != Some(&self.geometry) {
            self.apply_geometry();
            let shown = self.pane.shown;
            self.slide(shown, false);
        }
        self.scale = self.pane.panel.backingScaleFactor();
        let force = self.pane.built.as_ref().is_some_and(|(s, k)| s != style || *k != self.scale);
        self.pane.built = Some((style.clone(), self.scale));
        force
    }

    /// Orders the active pane in if it never was: it then lives on the Desktop shown.
    fn order_in(&mut self) {
        if self.pane.placed || self.snapshot.fullscreen {
            return;
        }
        self.pane.panel.orderFrontRegardless();
        self.pane.placed = true;
        tracing::debug!(space = self.pane.space, on_active_space = self.pane.panel.isOnActiveSpace(), "strip: a pane ordered in");
    }

    /// A full-screen app's Space has no strip: a pane found there (none should be) goes.
    fn keep_off_fullscreen(&mut self) {
        let before = self.others.len();
        self.others.retain(|p| !p.on_space_shown());
        let mut gone = before - self.others.len();
        if self.pane.on_space_shown() {
            let fresh = Pane::new(self.mtm, &self.geometry);
            drop(std::mem::replace(&mut self.pane, fresh));
            self.layouts.clear();
            gone += 1;
        }
        if gone > 0 {
            tracing::warn!(gone, "strip: a pane was on a full-screen Space; closed");
        }
    }

    /// Two panes on the Desktop shown (a Desktop removed in Mission Control hands its
    /// windows to another): the active one stays, the others go.
    fn drop_duplicates(&mut self) {
        if !self.pane.on_space_shown() {
            return;
        }
        let before = self.others.len();
        self.others.retain(|p| !p.on_space_shown());
        if self.others.len() != before {
            tracing::info!(closed = before - self.others.len(), "strip: another pane on the Desktop shown; closed");
        }
    }

    /// The cards in the order shown now: the snapshot's, with a card being dragged moved to
    /// the slot it would take, and a gap where a window dragged over the strip would go.
    fn shown_cards(&self) -> Vec<CardSnapshot> {
        let mut cards = self.snapshot.cards.clone();
        if let Some(d) = self.card_drag.as_ref().filter(|d| d.mode == DragMode::Reorder)
            && let Some(from) = cards.iter().position(|c| c.stage == d.stage)
        {
            let card = cards.remove(from);
            let to = d.before.and_then(|b| cards.iter().position(|c| c.stage == b)).unwrap_or(cards.len());
            // Held over another card: that card shows the merge (the dragged card keeps its
            // own slot meanwhile; `before` is that slot).
            if let Some(target) = d.onto.and_then(|o| cards.iter_mut().find(|c| c.stage == o)) {
                *target = target.merged_with(&card);
            }
            cards.insert(to, card);
        }
        if let Some(w) = self.snapshot.window_drag.filter(|_| self.drop_over) {
            let gap = CardSnapshot { stage: DROP_STAGE, number: 0, name: None, windows: vec![w], apps: Vec::new() };
            let to = self.drop_before.and_then(|b| cards.iter().position(|c| c.stage == b)).unwrap_or(cards.len());
            cards.insert(to, gap);
        }
        cards
    }

    /// The snapshot's cards laid out as they are without any drag (what drops are measured
    /// against, so the gap opening does not move the target).
    fn resting_layouts(&self) -> Vec<CardLayout> {
        strip_layout::layout_column(&self.snapshot.cards, self.geometry.height(), self.m())
    }

    /// Where a release at `p` (global) goes among `layouts`, leaving out `skip`'s card: the
    /// gap right above a stage's card (`None`: the bottom), or onto a card when `p` is within
    /// the middle `band` of its height.
    fn spot_at(&self, layouts: &[CardLayout], p: Point, skip: Option<u64>, band: f64) -> Spot {
        let m = self.m();
        let others: Vec<&CardLayout> = layouts.iter().filter(|l| Some(l.stage) != skip && l.stage != DROP_STAGE).collect();
        let bands: Vec<(f64, f64)> = others
            .iter()
            .map(|l| {
                let b = l.bounds(m);
                (b.y, b.max_y())
            })
            .collect();
        let y = p.y - self.geometry.global.y;
        match motion_math::drop_spot(&bands, y, band) {
            DropSpot::Gap(i) => Spot::Before(others.get(i).map(|l| l.stage)),
            DropSpot::OnCard(i) => others.get(i).map_or(Spot::Before(None), |l| Spot::Onto(l.stage)),
        }
    }

    /// The slot for a window released at `p` (global).
    fn drop_slot_at(&self, p: Point) -> StripDrop {
        let before = match self.spot_at(&self.resting_layouts(), p, None, WINDOW_DROP_BAND) {
            Spot::Before(b) => b,
            Spot::Onto(_) => None,
        };
        StripDrop { before }
    }

    /// Where `stage`'s card, dragged within the column, goes at `p` (global): (the stage whose
    /// card it goes above, the card it merges into). Measured on the cards at rest with its
    /// own slot left out, so the cards making room never move the target. Over the middle of
    /// another card (`[strip] merge_on_drop`) it merges and keeps its own slot meanwhile; so
    /// it does in a gap without `[strip] reorder`.
    fn card_spot(&self, p: Point, stage: u64) -> (Option<u64>, Option<u64>) {
        let band = if self.style.merge_on_drop { self.style.drag.merge_zone.clamp(0.0, 1.0) } else { 0.0 };
        let cards = &self.snapshot.cards;
        let own = cards.iter().position(|c| c.stage == stage).and_then(|i| cards.get(i + 1)).map(|c| c.stage);
        match self.spot_at(&self.resting_layouts(), p, Some(stage), band) {
            // A card a transition is still landing in is not drawn yet: nothing to merge into
            // (review R2 V5), as a press there is nothing.
            Spot::Onto(into) if self.motion.holds(into) => (own, None),
            Spot::Onto(into) => (own, Some(into)),
            Spot::Before(before) if self.style.reorder => (before, None),
            Spot::Before(_) => (own, None),
        }
    }

    /// The card a dragged card would merge into now.
    fn merge_onto(&self) -> Option<u64> {
        self.card_drag.as_ref().filter(|d| d.mode == DragMode::Reorder).and_then(|d| d.onto)
    }

    /// A card is drawn grown: under the pointer, or about to take a dragged card.
    fn grown(&self, stage: u64) -> bool {
        self.hovered == Some(stage) || self.merge_onto() == Some(stage)
    }

    /// The size a card is drawn at, of its resting size: pressed (`[cards] press_scale`),
    /// grown (`hover_scale`) or at rest.
    fn card_scale(&self, stage: u64) -> f64 {
        if self.squeezed == Some(stage) {
            self.style.cards.press_scale
        } else if self.grown(stage) {
            self.m().hover_scale
        } else {
            1.0
        }
    }

    /// Lays out the column ([`Strip::shown_cards`]) and moves every card's layers to its slot;
    /// a card being reordered follows the pointer instead.
    fn layout_cards(&mut self, force: bool) {
        let cards = self.shown_cards();
        let mut layouts = strip_layout::layout_column(&cards, self.geometry.height(), self.m());
        let following = self.card_drag.as_ref().filter(|d| d.mode == DragMode::Reorder).map(|d| {
            let p = self.pointer();
            let mut l = d.origin.clone();
            l.frame = l.frame.translate(p.x - d.press.x, p.y - d.press.y);
            (d.stage, l)
        });
        for l in &layouts {
            tracing::trace!(stage = l.stage, frame = %l.frame, "card layout");
        }
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let keep: Vec<u64> = layouts.iter().map(|l| l.stage).collect();
        self.pane.cards.retain(|stage, layers| {
            let kept = keep.contains(stage);
            if !kept {
                layers.card.removeFromSuperlayer();
            }
            kept
        });
        if self.hovered.is_some_and(|h| !keep.contains(&h)) {
            self.hovered = None;
        }
        for (i, card) in cards.iter().enumerate() {
            if card.stage == DROP_STAGE {
                continue; // the gap: the window's own card follows the pointer above
            }
            match &following {
                Some((stage, l)) if *stage == card.stage => {
                    let l = l.clone();
                    self.place_card(&l, card, force, true);
                    layouts[i] = l;
                }
                _ => {
                    let l = layouts[i].clone();
                    self.place_card(&l, card, force, false);
                }
            }
        }
        CATransaction::commit();
        self.layouts = layouts;
        let order: Vec<u64> = self.layouts.iter().map(|l| l.stage).filter(|s| *s != DROP_STAGE).collect();
        if order != self.logged_order {
            tracing::debug!(?order, "strip order");
            self.logged_order = order;
        }
        self.publish_cards();
    }

    /// See [`probe`].
    fn probe(&self) -> serde_json::Value {
        let g = self.geometry.global;
        let root = &self.pane.root;
        // SAFETY: presentationLayer returns a copy of the drawn state or nil.
        let drawn = |l: &CALayer| unsafe { l.presentationLayer() };
        let to_global = |r: CGRect| Rect::new(g.x + r.origin.x, g.y + r.origin.y, r.size.width, r.size.height);
        let root_drawn = drawn(root);
        let in_root = |layer: &CALayer| {
            let b = layer.bounds();
            match (drawn(layer), root_drawn.as_deref()) {
                (Some(l), Some(r)) => to_global(l.convertRect_toLayer(b, Some(r))),
                _ => to_global(layer.convertRect_toLayer(b, Some(root))),
            }
        };
        // A plane's left edge (its turn's axis, at depth 0: no perspective moves it), top and
        // bottom, global.
        let left_edge = |layer: &CALayer| {
            let b = layer.bounds();
            let ends = [CGPoint::new(0.0, 0.0), CGPoint::new(0.0, b.size.height)];
            ends.map(|p| {
                let q = match (drawn(layer), root_drawn.as_deref()) {
                    (Some(l), Some(r)) => l.convertPoint_toLayer(p, Some(r)),
                    _ => layer.convertPoint_toLayer(p, Some(root)),
                };
                Point::new(g.x + q.x, g.y + q.y)
            })
        };
        let cards: Vec<serde_json::Value> = self
            .snapshot
            .cards
            .iter()
            .filter_map(|c| {
                let l = self.pane.cards.get(&c.stage)?;
                let front = l.planes.last().map(|(_, p)| in_root(p));
                let edge = l.planes.last().map(|(_, p)| left_edge(p));
                let icons: Vec<Rect> = l.icons.iter().map(|x| in_root(x)).collect();
                Some(serde_json::json!({
                    "stage": c.stage,
                    "z": l.card.zPosition(),
                    "opacity": l.card.opacity(),
                    "thumb": front,
                    "thumb_left_edge": edge,
                    "icons": icons,
                }))
            })
            .collect();
        serde_json::json!({
            "order": self.snapshot.cards.iter().map(|c| c.stage).collect::<Vec<_>>(),
            "cards": cards,
            "lifted": self.lifted_card.as_ref().map(|l| l.0),
            "dragging": self.card_drag.as_ref().map(|d| format!("{:?}", d.mode)),
            "merge_onto": self.merge_onto(),
            "shown": self.pane.shown,
            "overlay_top_down": self.motion_top_down(),
        })
    }

    /// Publishes where the cards are (global), for `ctl state`.
    fn publish_cards(&self) {
        let g = self.geometry.global;
        let frames = if self.pane.shown {
            self.layouts
                .iter()
                .filter(|l| l.stage != DROP_STAGE)
                .map(|l| (l.stage, l.bounds(self.m()).translate(g.x, g.y)))
                .collect()
        } else {
            Vec::new()
        };
        strip_model::publish_cards(frames);
    }

    /// The transform of a card's container (anchored at the eye): a scale about the card's
    /// center at `layout` by [`Strip::card_scale`].
    fn card_transform(&self, layout: &CardLayout) -> CATransform3D {
        let s = self.card_scale(layout.stage);
        if s == 1.0 {
            return identity();
        }
        let m = self.m();
        scale_about(s, layout.hover_center(m), m.eye)
    }

    /// Where a card's unit sits: its content was built for the card at `built`, and the card
    /// is now at `layout`.
    fn unit_offset(layout: &CardLayout, built: Point) -> CGPoint {
        CGPoint::new(layout.frame.x - built.x, layout.frame.y - built.y)
    }

    /// Creates or updates one card's layers and moves them to the card's slot: on the spring,
    /// or at once (`immediate`, a card following the pointer).
    fn place_card(&mut self, layout: &CardLayout, card: &CardSnapshot, force_rebuild: bool, immediate: bool) {
        let size = self.geometry.panel.size;
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size.width, size.height));
        // Planes a merge preview adds fade in.
        let fresh: Vec<u32> = match self.pane.cards.get(&card.stage) {
            Some(l) if self.merge_onto() == Some(card.stage) => {
                layout.thumbs.iter().map(|t| t.id).filter(|id| !l.layout.thumbs.iter().any(|o| o.id == *id)).collect()
            }
            _ => Vec::new(),
        };
        let rebuild = match self.pane.cards.get(&card.stage) {
            Some(layers) => {
                force_rebuild
                    || layers.snapshot != *card
                    || layers.layout.thumbs.len() != layout.thumbs.len()
                    || layers.layout.thumbs.iter().zip(&layout.thumbs).any(|(a, b)| a.id != b.id || a.rect.size() != b.rect.size())
                    || layers.layout.icons.len() != layout.icons.len()
            }
            None => {
                let m = self.m().clone();
                let card_layer = CALayer::new();
                card_layer.setBounds(bounds);
                card_layer.setAnchorPoint(CGPoint::new(m.eye.x / size.width.max(1.0), m.eye.y / size.height.max(1.0)));
                card_layer.setPosition(CGPoint::new(m.eye.x, m.eye.y));
                card_layer.setSublayerTransform(perspective(&m, self.current_dx()));
                let unit = CATransformLayer::new();
                unit.setBounds(bounds);
                unit.setAnchorPoint(CGPoint::new(0.0, 0.0));
                unit.setPosition(CGPoint::new(0.0, 0.0));
                card_layer.addSublayer(&unit);
                self.pane.cards_root.addSublayer(&card_layer);
                let fade = self.style.animation.seconds(self.style.cards.fade_ms);
                if self.motion.holds(card.stage) {
                    // A transition's snapshots are landing here; the card shows when they do.
                    card_layer.setOpacity(0.0);
                } else if self.has_snapshot {
                    fade_in(&card_layer, fade);
                }
                let layers = CardLayers {
                    card: card_layer,
                    unit,
                    planes: Vec::new(),
                    images: Vec::new(),
                    icons: Vec::new(),
                    built: Point::new(layout.frame.x, layout.frame.y),
                    snapshot: card.clone(),
                    layout: layout.clone(),
                };
                self.pane.cards.insert(card.stage, layers);
                true
            }
        };
        if rebuild {
            // Rebuilt for the new layout: the unit starts from where the card is drawn now and
            // glides from there.
            let drawn = self.pane.cards.get(&card.stage).map(|l| {
                // SAFETY: presentationLayer returns a copy of the drawn state or nil.
                let at = unsafe { l.unit.presentationLayer() }.map_or_else(|| l.unit.position(), |p| p.position());
                Point::new(l.built.x + at.x, l.built.y + at.y)
            });
            self.fill_card(card.stage, layout, card);
            if let (Some(d), Some(l)) = (drawn, self.pane.cards.get(&card.stage)) {
                CATransaction::begin();
                CATransaction::setDisableActions(true);
                l.unit.removeAnimationForKey(&NSString::from_str("position"));
                l.unit.setPosition(CGPoint::new(d.x - l.built.x, d.y - l.built.y));
                CATransaction::commit();
                let fade = self.style.animation.seconds(self.style.cards.fade_ms);
                for (_, plane) in l.planes.iter().filter(|(id, _)| fresh.contains(id)) {
                    fade_in(plane, fade);
                }
            }
        }
        let glide = self.glide();
        let hover = self.card_transform(layout);
        if let Some(layers) = self.pane.cards.get_mut(&card.stage) {
            // The whole card glides to its new place as one unit.
            let to = Self::unit_offset(layout, layers.built);
            move_to(glide, &layers.unit, to, immediate);
            layers.card.setTransform(hover);
            layers.snapshot = card.clone();
            layers.layout = layout.clone();
        }
    }

    /// (Re)builds a card's thumbnail planes, icons, badge and name.
    fn fill_card(&mut self, stage: u64, layout: &CardLayout, card: &CardSnapshot) {
        let Some(layers) = self.pane.cards.get(&stage) else { return };
        let unit = layers.unit.clone();
        let m = self.m().clone();
        let style = self.style.clone();
        let c = &style.cards;
        let k = m.k;
        // SAFETY: setting no sublayers removes the old children.
        unsafe { unit.setSublayers(None) };
        unit.removeAnimationForKey(&NSString::from_str("position"));
        unit.setPosition(CGPoint::new(0.0, 0.0));
        let mut planes = Vec::new();
        let mut images = Vec::new();
        // Thumbnails back to front, each a plane turned about its left edge.
        for slot in layout.thumbs.iter().rev() {
            let plane = CALayer::new();
            plane.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(slot.rect.w, slot.rect.h)));
            plane.setAnchorPoint(CGPoint::new(0.0, 0.5));
            plane.setPosition(CGPoint::new(layout.frame.x + slot.rect.x, layout.frame.y + slot.rect.y + slot.rect.h / 2.0));
            plane.setTransform(turn(&m));
            plane.setCornerRadius(c.corner_radius * k);
            plane.setBackgroundColor(Some(&color(0.22, 0.22, 0.24, 1.0)));
            plane.setBorderWidth(c.border_width * k);
            plane.setBorderColor(Some(&rgba(c.border_color)));
            plane.setShadowColor(Some(&rgba(c.shadow_color)));
            plane.setShadowOpacity(c.shadow_opacity as f32);
            plane.setShadowRadius(c.shadow_radius * k);
            plane.setShadowOffset(CGSize::new(c.shadow_offset_x * k, c.shadow_offset_y * k));
            plane.setAllowsEdgeAntialiasing(true);
            let image = CALayer::new();
            image.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(slot.rect.w, slot.rect.h)));
            image.setCornerRadius(c.corner_radius * k);
            image.setMasksToBounds(true);
            // SAFETY: framework constant.
            image.setContentsGravity(unsafe { kCAGravityResizeAspectFill });
            image.setAllowsEdgeAntialiasing(true);
            if let Some(img) = self.thumbs.get(&slot.id) {
                // SAFETY: a CGImage is valid layer contents.
                unsafe { image.setContents(Some(as_object(&**img))) };
            }
            plane.addSublayer(&image);
            unit.addSublayer(&plane);
            planes.push((slot.id, plane));
            images.push((slot.id, image));
        }
        // App icons, flat (depth 0), in panel coordinates, above the card's planes.
        let mut icon_layers = Vec::new();
        let icon_style = &style.icons;
        for (pid, rect) in &layout.icons {
            let icon = CALayer::new();
            icon.setFrame(cg_rect(&rect.translate(layout.frame.x, layout.frame.y)));
            // SAFETY: framework constant.
            icon.setContentsGravity(unsafe { kCAGravityResizeAspect });
            icon.setShadowColor(Some(&color(0.0, 0.0, 0.0, 1.0)));
            icon.setShadowOpacity(icon_style.shadow_opacity as f32);
            icon.setShadowRadius(icon_style.shadow_radius * k);
            icon.setShadowOffset(CGSize::new(0.0, icon_style.shadow_offset_y * k));
            let side = rect.w;
            // An icon not found (an app still launching) is looked up again next time.
            if self.icons.get(pid).is_none_or(|i| i.is_none()) {
                let found = app_icon(*pid, side);
                self.icons.insert(*pid, found);
            }
            if let Some(Some(img)) = self.icons.get(pid) {
                // SAFETY: a CGImage is valid layer contents.
                unsafe { icon.setContents(Some(as_object(&**img))) };
            }
            icon.setZPosition(1.0);
            unit.addSublayer(&icon);
            icon_layers.push(icon);
        }
        let origin = Point::new(layout.frame.x, layout.frame.y);
        if style.show_numbers {
            let text = card.number.to_string();
            let w = 18.0_f64.max(8.0 + 7.0 * text.len() as f64);
            let badge = CALayer::new();
            badge.setFrame(CGRect::new(CGPoint::new(origin.x - 6.0, origin.y - 7.0), CGSize::new(w, 18.0)));
            badge.setCornerRadius(9.0);
            badge.setBackgroundColor(Some(&color(0.08, 0.08, 0.1, 0.78)));
            badge.setBorderWidth(0.5);
            badge.setBorderColor(Some(&color(1.0, 1.0, 1.0, 0.35)));
            let label = text_layer(&text, 11.0, self.scale, true);
            label.setFrame(CGRect::new(CGPoint::new(0.0, 2.0), CGSize::new(w, 14.0)));
            badge.addSublayer(&label);
            badge.setZPosition(1.0);
            unit.addSublayer(&badge);
        }
        if style.show_names
            && let Some(name) = card.name.as_deref().filter(|n| !n.is_empty())
        {
            let icons_right = layout.icons.last().map_or(origin.x, |(_, r)| origin.x + r.max_x());
            let x = icons_right + 4.0;
            let base = layout.thumbs.first().map_or(layout.frame.h, |t| t.rect.max_y());
            let label = text_layer(name, 11.0, self.scale, false);
            let width = (self.geometry.panel.size.width - 8.0 - x).max(30.0);
            label.setFrame(CGRect::new(CGPoint::new(x, origin.y + base - 2.0), CGSize::new(width, 15.0)));
            label.setShadowColor(Some(&color(0.0, 0.0, 0.0, 1.0)));
            label.setShadowOpacity(0.6);
            label.setShadowRadius(2.0);
            label.setShadowOffset(CGSize::new(0.0, 1.0));
            label.setZPosition(1.0);
            unit.addSublayer(&label);
        }
        if let Some(layers) = self.pane.cards.get_mut(&stage) {
            layers.planes = planes;
            layers.images = images;
            layers.icons = icon_layers;
            layers.built = origin;
        }
    }

    // -- thumbnails -------------------------------------------------------------------------

    fn set_thumbnail(&mut self, id: u32, image: CFRetained<CGImage>) {
        let shown = self.layouts.iter().any(|l| l.thumbs.iter().any(|t| t.id == id));
        if !shown {
            return;
        }
        self.motion.thumbnail_arrived(id, &image);
        for layers in self.pane.cards.values() {
            for (wid, layer) in &layers.images {
                if *wid == id {
                    // SAFETY: a CGImage is valid layer contents.
                    unsafe { layer.setContents(Some(as_object(&*image))) };
                }
            }
        }
        if let Some(ghost) = self.card_drag.as_ref().filter(|d| d.front == id).and_then(|d| d.ghost.as_ref()) {
            ghost.set_image(&image);
        }
        self.thumbs.insert(id, image);
    }

    /// Tells the capturer what to capture: every shown thumbnail while the strip shows.
    fn update_targets(&mut self) {
        let shown_ids: Vec<u32> = self.layouts.iter().flat_map(|l| l.thumbs.iter().map(|t| t.id)).collect();
        self.thumbs.retain(|id, _| shown_ids.contains(id));
        let resolution = self.style.thumbnail_resolution * self.style.cards.hover_scale;
        let targets = if self.pane.shown {
            self.layouts
                .iter()
                .flat_map(|l| l.thumbs.iter())
                .map(|t| Target {
                    id: t.id,
                    width: (t.rect.w * resolution).round().max(1.0) as u32,
                    height: (t.rect.h * resolution).round().max(1.0) as u32,
                })
                .collect()
        } else {
            Vec::new()
        };
        self.capturer.set_targets(targets);
    }

    // -- pointer ----------------------------------------------------------------------------

    /// The card under `p` (panel coordinates): its turned thumbnails and icons, where the
    /// card is drawn now (a card still gliding to its slot is hit where it is seen, never at
    /// the slot it is headed for).
    fn card_at(&self, p: Point) -> Option<u64> {
        let mut hits = self
            .layouts
            .iter()
            .filter(|l| l.stage != DROP_STAGE)
            .map(|l| (l.stage, self.drawn_bounds(l)))
            .filter(|(_, b)| b.contains(p))
            .map(|(s, _)| s);
        // A lifted card is drawn above the others.
        let first = hits.next()?;
        let lifted = self.lifted_card.as_ref().map(|l| l.0);
        Some(if Some(first) == lifted { first } else { hits.find(|s| Some(*s) == lifted).unwrap_or(first) })
    }

    /// A card's box where it is drawn: on its way to its slot, and grown when hovered.
    fn drawn_bounds(&self, l: &CardLayout) -> Rect {
        let m = self.m();
        let d = self.drawn_offset(l.stage);
        let b = l.bounds(m).translate(d.x, d.y);
        let s = self.card_scale(l.stage);
        if s == 1.0 {
            return b;
        }
        let c = l.hover_center(m);
        let (cx, cy) = (c.x + d.x, c.y + d.y);
        Rect::new(cx + (b.x - cx) * s, cy + (b.y - cy) * s, b.w * s, b.h * s)
    }

    /// How far `stage`'s card is drawn from its slot (on its way there).
    fn drawn_offset(&self, stage: u64) -> Point {
        let Some(l) = self.pane.cards.get(&stage) else { return Point::new(0.0, 0.0) };
        let model = l.unit.position();
        // SAFETY: presentationLayer returns a copy of the drawn state or nil.
        let drawn = unsafe { l.unit.presentationLayer() }.map_or(model, |p| p.position());
        Point::new(drawn.x - model.x, drawn.y - model.y)
    }

    /// Lifts `stage`'s card above the others (its whole unit).
    fn lift_card(&mut self, stage: u64) {
        if let Some((s, timer)) = self.lifted_card.take() {
            if let Some(t) = timer {
                t.invalidate();
            }
            if s != stage {
                self.set_card_z(s, 0.0);
            }
        }
        self.set_card_z(stage, LIFTED_Z);
        self.lifted_card = Some((stage, None));
    }

    /// Puts the lifted card back among the others once its glide has settled.
    fn settle_lifted(&mut self) {
        let settle = self.glide().map_or(0.0, |s| motion_math::settle_time(s, 0.002));
        let Some((stage, timer)) = self.lifted_card.as_mut() else { return };
        if let Some(t) = timer.take() {
            t.invalidate();
        }
        let stage = *stage;
        let block = RcBlock::new(move |_t: NonNull<NSTimer>| {
            with_strip(|s| {
                if s.lifted_card.as_ref().is_some_and(|l| l.0 == stage && l.1.is_some()) {
                    s.lifted_card = None;
                    s.set_card_z(stage, 0.0);
                }
            });
        });
        // SAFETY: the block runs once on the main run loop.
        let t = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(settle, false, &block) };
        if let Some(l) = self.lifted_card.as_mut() {
            l.1 = Some(t);
        }
    }

    fn set_card_z(&self, stage: u64, z: f64) {
        if let Some(l) = self.pane.cards.get(&stage) {
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            l.card.setZPosition(z);
            CATransaction::commit();
        }
    }

    /// The app whose icon is under `p` on `stage`'s card.
    fn icon_at(&self, stage: u64, p: Point) -> Option<i32> {
        let l = self.layouts.iter().find(|l| l.stage == stage)?;
        let d = self.drawn_offset(stage);
        l.icons.iter().find(|(_, r)| r.translate(l.frame.x + d.x, l.frame.y + d.y).contains(p)).map(|(pid, _)| *pid)
    }

    fn mouse_down(&mut self, p: Point) {
        // A card a transition is still landing in is not drawn yet: a press there is nothing
        // (it would switch straight back; review R1-ui 3).
        self.pressed = if self.pane.shown { self.card_at(p).filter(|s| !self.motion.holds(*s)) } else { None };
        self.card_drag = self.pressed.and_then(|stage| self.drag_for(stage));
        if let Some(stage) = self.pressed {
            // A click is likely: fresh snapshots of what leaves and what arrives, and the
            // engine's look at the windows taken now rather than at the release.
            self.prefetch(Some(stage));
            strip_model::emit(StripEvent::CardPressed { stage });
            // Apple's card answers the press at once: it eases back from its hover growth to
            // a little under its resting size.
            self.squeeze(Some(stage));
        }
        tracing::trace!(x = p.x, y = p.y, card = ?self.pressed, "strip mouse down");
    }

    /// A possible drag of `stage`'s card from the current pointer.
    fn drag_for(&self, stage: u64) -> Option<CardDrag> {
        let layout = self.layouts.iter().find(|l| l.stage == stage)?;
        let card = self.snapshot.cards.iter().find(|c| c.stage == stage)?;
        let slot = layout.thumbs.first()?;
        let front = card.windows.first()?;
        let g = self.geometry.global;
        let b = self.m().projected_bounds(layout.frame.x + slot.rect.x, layout.frame.y + slot.rect.y, slot.rect.w, slot.rect.h);
        let thumb = Rect::new(g.x + b.x, g.y + b.y, b.w.max(1.0), b.h.max(1.0));
        let press = self.pointer();
        let grab = (((press.x - thumb.x) / thumb.w).clamp(0.0, 1.0), ((press.y - thumb.y) / thumb.h).clamp(0.0, 1.0));
        Some(CardDrag {
            stage,
            mode: DragMode::Pressed,
            origin: layout.clone(),
            before: None,
            onto: None,
            press,
            thumb,
            grab,
            full: front.frame.size(),
            front: front.id,
            ghost: None,
        })
    }

    /// Sets a dragged card's opacity in the strip (lifted out, or back).
    fn set_card_opacity(&self, stage: u64, opacity: f32) {
        if let Some(layers) = self.pane.cards.get(&stage) {
            layers.card.setOpacity(opacity);
        }
    }

    fn mouse_dragged(&mut self) {
        let p = self.pointer();
        let strip_right = self.geometry.global.max_x();
        let primary_height = self.geometry.primary_height;
        let mtm = self.mtm;
        let style = self.style.drag.clone();
        let follow = self.style.reorder || self.style.merge_on_drop;
        let Some(drag) = self.card_drag.as_ref() else { return };
        if drag.mode == DragMode::Pressed && (p.x - drag.press.x).hypot(p.y - drag.press.y) <= style.threshold.max(MIN_DRAG_THRESHOLD) {
            return;
        }
        let stage = drag.stage;
        let in_column = self.geometry.global.contains(p);
        let mode = if follow && in_column { DragMode::Reorder } else { DragMode::Out };
        let (before, onto) = if mode == DragMode::Reorder { self.card_spot(p, stage) } else { (None, None) };
        let image = self.thumbs.get(&drag.front).cloned();
        let was = drag.mode;
        let was_onto = self.merge_onto();
        if was == DragMode::Pressed {
            tracing::debug!(stage, ?mode, "card drag started");
            self.squeeze(None);
        }
        {
            let Some(drag) = self.card_drag.as_mut() else { return };
            drag.mode = mode;
            drag.before = before;
            drag.onto = onto;
            if mode == DragMode::Reorder {
                drag.ghost = None;
            } else {
                let frame = strip_layout::ghost_frame(drag.thumb, drag.grab, drag.full, strip_right, style.grow_distance, p);
                let ghost = drag.ghost.get_or_insert_with(|| Ghost::new(mtm, image.as_deref(), style.ghost_opacity as f32));
                ghost.set_frame(frame, primary_height);
            }
        }
        match mode {
            DragMode::Reorder => {
                self.set_card_opacity(stage, 1.0);
                if was != DragMode::Reorder {
                    self.lift_card(stage);
                    self.set_hover(Some(stage));
                }
                self.layout_cards(false);
            }
            DragMode::Out | DragMode::Pressed => {
                if was == DragMode::Reorder {
                    self.settle_lifted();
                }
                if was != DragMode::Out {
                    self.set_card_opacity(stage, style.lifted_opacity as f32);
                    // The card goes back to its slot while its ghost is out.
                    self.layout_cards(false);
                }
            }
        }
        if onto != was_onto {
            tracing::debug!(stage, ?onto, "card held over a card");
            self.ease_growth([was_onto, onto]);
        }
        self.update_visibility();
    }

    fn mouse_up(&mut self, p: Point, shift: bool) -> Option<StripEvent> {
        let event = self.release(p, shift);
        self.update_pass_through(Some(p));
        event
    }

    /// The button came up at `p` (panel coordinates): the click, drop or merge it ends.
    fn release(&mut self, p: Point, shift: bool) -> Option<StripEvent> {
        if let Some(drag) = self.card_drag.take()
            && drag.mode != DragMode::Pressed
        {
            self.pressed = None;
            self.set_card_opacity(drag.stage, 1.0);
            let at = self.pointer();
            if drag.mode == DragMode::Reorder
                && let Some(into) = drag.onto
                && self.merge_locally(drag.stage, into)
            {
                // The target shows the merge already; the dropped card goes into it. The
                // controller's snapshot agrees.
                tracing::debug!(stage = drag.stage, into, "card dropped on a card");
                self.set_hover(Some(into));
                self.vanish_into(drag.stage, into);
                self.layout_cards(false);
                self.update_visibility();
                self.update_targets();
                return Some(StripEvent::CardMerged { stage: drag.stage, into });
            }
            if drag.mode == DragMode::Reorder {
                // Put the card in its new slot now; the controller's snapshot agrees. It stays
                // above the others until it has glided in.
                let moved = self.reorder_locally(drag.stage, drag.before);
                self.layout_cards(false);
                self.settle_lifted();
                self.update_visibility();
                if moved {
                    tracing::debug!(stage = drag.stage, before = ?drag.before, "card reordered");
                    return Some(StripEvent::CardMoved { stage: drag.stage, before: drag.before });
                }
                // Back in its own slot: nothing moved. Released over the card, it was a click
                // with a wobble.
                if self.layouts.iter().find(|l| l.stage == drag.stage).is_some_and(|l| l.bounds(self.m()).contains(p)) {
                    tracing::debug!(stage = drag.stage, "a drag that ended in its own slot over the card: a click");
                    return Some(self.click_event(drag.stage, p, shift));
                }
                return None;
            }
            let out = !self.geometry.global.contains(at);
            if out {
                let right = self.geometry.global.max_x();
                let rect = strip_layout::ghost_frame(drag.thumb, drag.grab, drag.full, right, self.style.drag.grow_distance, at);
                self.motion.keep_landing(drag.ghost, drag.front, rect);
            } else {
                drop(drag.ghost);
            }
            self.layout_cards(false);
            self.update_visibility();
            if !out {
                tracing::debug!(stage = drag.stage, "card drag cancelled over the strip");
                return None;
            }
            let origin = strip_layout::drop_origin(drag.full, drag.grab.0, at);
            return Some(StripEvent::CardDropped { stage: drag.stage, x: origin.x, y: origin.y });
        }
        let Some(pressed) = self.pressed.take() else {
            self.squeeze(None);
            return None;
        };
        if !self.pane.shown || self.card_at(p) != Some(pressed) {
            self.squeeze(None);
            return None;
        }
        // The card stays pressed until its transition takes it (the incoming snapshots grow
        // from it as drawn); if none comes, it lets go.
        Self::release_squeeze_later(pressed);
        Some(self.click_event(pressed, p, shift))
    }

    /// In [`SQUEEZE_MAX`]: a released card still drawn pressed lets go, unless a transition
    /// holds it (then it is looked at again after as long, so one that a later transition
    /// took never stays pressed: R3 6). A new press of any card ends the watch.
    fn release_squeeze_later(stage: u64) {
        let block = RcBlock::new(move |_t: NonNull<NSTimer>| {
            with_strip(|s| {
                if s.squeezed != Some(stage) || s.pressed.is_some() {
                    return;
                }
                if s.motion.holds(stage) {
                    Self::release_squeeze_later(stage);
                } else {
                    s.squeeze(None);
                }
            });
        });
        // SAFETY: the block runs once on the main run loop.
        let _once = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(SQUEEZE_MAX, false, &block) };
    }

    /// Draws `stage`'s card pressed (`None`: no card), easing the card that was and the one
    /// that is now over `[cards] press_ms`.
    fn squeeze(&mut self, stage: Option<u64>) {
        if self.squeezed == stage {
            return;
        }
        let was = std::mem::replace(&mut self.squeezed, stage);
        let seconds = self.style.animation.seconds(if stage.is_some() { self.style.cards.press_ms } else { self.style.cards.hover_ms });
        self.ease_cards([was, stage], seconds);
    }

    /// A click on `stage`'s card at `p`: on an app icon (without shift) or the card.
    fn click_event(&self, stage: u64, p: Point, shift: bool) -> StripEvent {
        let slot = self.layouts.iter().filter(|l| l.stage != DROP_STAGE).position(|l| l.stage == stage);
        tracing::debug!(stage, ?slot, "card click in slot");
        match self.icon_at(stage, p) {
            Some(pid) if !shift => StripEvent::IconClicked { stage, pid },
            _ => StripEvent::CardClicked { stage, shift },
        }
    }

    /// Moves `stage`'s card above `before`'s (the bottom for `None`) in the snapshot; true when
    /// the order changed.
    fn reorder_locally(&mut self, stage: u64, before: Option<u64>) -> bool {
        let cards = &mut self.snapshot.cards;
        let Some(from) = cards.iter().position(|c| c.stage == stage) else { return false };
        let old: Vec<u64> = cards.iter().map(|c| c.stage).collect();
        let card = cards.remove(from);
        let to = before.and_then(|b| cards.iter().position(|c| c.stage == b)).unwrap_or(cards.len());
        cards.insert(to, card);
        cards.iter().map(|c| c.stage).collect::<Vec<_>>() != old
    }

    /// Merges `stage`'s card into `into`'s in the snapshot (see
    /// [`CardSnapshot::merged_with`]); false when either card is not there.
    fn merge_locally(&mut self, stage: u64, into: u64) -> bool {
        let cards = &mut self.snapshot.cards;
        if stage == into || !cards.iter().any(|c| c.stage == into) {
            return false;
        }
        let Some(from) = cards.iter().position(|c| c.stage == stage) else { return false };
        let card = cards.remove(from);
        if let Some(target) = cards.iter_mut().find(|c| c.stage == into) {
            *target = target.merged_with(&card);
        }
        true
    }

    /// The card dropped onto `into`'s goes into it: it shrinks toward the target's middle and
    /// fades out, then its layers go.
    fn vanish_into(&mut self, stage: u64, into: u64) {
        if self.lifted_card.as_ref().is_some_and(|l| l.0 == stage)
            && let Some((_, Some(timer))) = self.lifted_card.take()
        {
            timer.invalidate();
        }
        let Some(layers) = self.pane.cards.remove(&stage) else { return };
        let m = self.m().clone();
        let seconds = self.style.animation.seconds(self.style.drag.merge_ms);
        let toward = self.layouts.iter().find(|l| l.stage == into).map(|l| {
            let c = l.hover_center(&m);
            let d = self.drawn_offset(into);
            Point::new(c.x + d.x, c.y + d.y)
        });
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        layers.card.setOpacity(0.0);
        // At speed 0 no fade: a zero duration would mean Core Animation's default (R3 5).
        if seconds > 0.0 {
            let fade = ease("opacity", &NSNumber::new_f32(1.0), &NSNumber::new_f32(0.0), seconds);
            layers.card.addAnimation_forKey(&fade, Some(&NSString::from_str("vanish")));
        }
        if let Some(c) = toward {
            ease_transform(&layers.card, scale_about(MERGE_SHRINK, c, m.eye), seconds);
        }
        CATransaction::commit();
        let card = layers.card.clone();
        let gone = RcBlock::new(move |_t: NonNull<NSTimer>| card.removeFromSuperlayer());
        // SAFETY: the block runs once on the main run loop.
        let _once = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(if seconds > 0.0 { seconds + 0.05 } else { 0.0 }, false, &gone) };
    }

    fn hover_at(&mut self, p: Option<Point>) {
        let card = if self.pane.shown { p.and_then(|p| self.card_at(p)) } else { None };
        self.set_hover(card);
        self.update_pass_through(p);
    }

    /// The pointer moved over another app's window or the desktop to `p` (global): over the
    /// strip column the pane lets clicks through, so its own tracking sees nothing there;
    /// the cards hover from here and the pane catches clicks again once it is on one.
    fn pointer_moved(&mut self, p: Point) {
        let g = self.geometry.global;
        if g.contains(p) {
            self.hover_at(Some(Point::new(p.x - g.x, p.y - g.y)));
        } else if self.hovered.is_some() || self.pane.catches == Some(true) {
            self.hover_at(None);
        }
    }

    /// Lets clicks through the pane except where they are its own (R1-ui 8): the strip's
    /// empty column is the desktop's (a wallpaper click, Finder's icons) or the window's
    /// under it; the pane takes the mouse only on a card (where it is drawn now) or while a
    /// card is pressed or dragged. `local` is the pointer in panel coordinates (`None`: read
    /// it now). AppKit decides per window, so the flag follows the pointer.
    fn update_pass_through(&mut self, local: Option<Point>) {
        let local = local.or_else(|| {
            let g = self.geometry.global;
            let p = self.pointer();
            g.contains(p).then(|| Point::new(p.x - g.x, p.y - g.y))
        });
        let busy = self.pressed.is_some() || self.card_drag.is_some();
        let catch = self.pane.shown && (busy || local.and_then(|p| self.card_at(p)).is_some());
        if self.pane.catches != Some(catch) {
            self.pane.catches = Some(catch);
            self.pane.panel.setIgnoresMouseEvents(!catch);
            tracing::trace!(catch, "strip pane: clicks caught on a card, else let through");
        }
    }

    fn set_hover(&mut self, card: Option<u64>) {
        // A card not drawn yet (its transition's snapshots are landing there) does not grow:
        // it shows at the size they land at, and grows once shown if the pointer is still on
        // it (review R1-ui 4).
        let card = card.filter(|s| !self.motion.holds(*s));
        if card == self.hovered {
            return;
        }
        if card.is_some() {
            // The pointer is on a card: a switch may follow.
            self.prefetch(card);
        }
        let previous = std::mem::replace(&mut self.hovered, card);
        if self.squeezed.is_some() && self.squeezed != card && self.pressed.is_none() {
            // The pointer left a released card that was drawn pressed.
            self.squeezed = None;
        }
        self.ease_growth([previous, card]);
    }

    /// Eases each of `stages`' cards to its size now ([`Strip::card_scale`]) over
    /// `[cards] hover_ms`.
    fn ease_growth(&self, stages: [Option<u64>; 2]) {
        self.ease_cards(stages, self.style.animation.seconds(self.style.cards.hover_ms));
    }

    /// Eases each of `stages`' cards to its size now over `seconds` (0: at once).
    fn ease_cards(&self, stages: [Option<u64>; 2], seconds: f64) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for stage in stages.into_iter().flatten() {
            let Some(layout) = self.layouts.iter().find(|l| l.stage == stage).cloned() else { continue };
            let t = self.card_transform(&layout);
            if let Some(layers) = self.pane.cards.get(&stage) {
                ease_transform(&layers.card, t, seconds);
            }
        }
        CATransaction::commit();
    }

    // -- visibility -------------------------------------------------------------------------

    fn pointer(&self) -> Point {
        self.geometry.global_point(NSEvent::mouseLocation())
    }

    /// True when the strip is on but a window of the active stage covers it.
    fn covered(&self) -> bool {
        self.snapshot.user_visible
            && !self.snapshot.hide_recent_apps
            && strip_visibility::overlaps(&self.snapshot.active_frames, &self.geometry.global)
    }

    /// A click on the edge tab reveals the strip at once (on release).
    fn tab_clicked(&mut self) {
        tracing::debug!("edge tab clicked");
        self.visibility.reveal_now();
        self.update_visibility();
    }

    /// After a Space change (`epoch` is the count of Space changes then): the active pane
    /// must be on the Space shown (unless a full-screen Space shows). A new pane ordered in
    /// while the Space still slid can land on the Desktop being left; then the pane of the
    /// Desktop shown takes over (a new one if there is none) and the stray one stays where it
    /// is, as that Desktop's second pane, closed when that Desktop shows again.
    fn ensure_on_active_space(&mut self, epoch: u64) {
        if !self.has_snapshot || self.snapshot.fullscreen {
            return;
        }
        if epoch != self.space_epoch {
            // The Space changed again since: its snapshot decides.
            return;
        }
        if !self.pane.on_space_shown() {
            tracing::warn!(space = self.snapshot.space, "strip: the pane is not on the Space shown; another takes over");
            self.leave_pane();
            self.select_pane();
            self.pane.space = self.snapshot.space;
            let style = self.style.clone();
            let force = self.sync_pane(&style);
            self.layout_cards(force);
            self.order_in();
            self.drop_duplicates();
            self.update_visibility();
            self.update_targets();
            tracing::info!(on_active_space = self.pane.panel.isOnActiveSpace(), "strip: the pane of the Space shown");
        }
        self.publish_panel();
    }

    /// The user went to another Space (main thread, from the Space-change notification).
    fn space_changed(&mut self) {
        self.space_epoch += 1;
        self.motion.space_changed(self.mtm);
    }

    /// Publishes the panes' state for `ctl state` ([`strip_model::panel_status`]).
    fn publish_panel(&self) {
        strip_model::publish_panel_status(strip_model::PanelStatus {
            shown: self.pane.shown,
            lifted: self.pane.lifted.unwrap_or(false),
            ordered_in: self.pane.panel.isVisible(),
            on_active_space: self.pane.panel.isOnActiveSpace(),
            frame: self.geometry.global,
            space: self.snapshot.space,
            window: u32::try_from(self.pane.panel.windowNumber()).unwrap_or(0),
        });
        let panes = std::iter::once(self.pane.status(true)).chain(self.others.iter().map(|p| p.status(false))).collect();
        strip_model::publish_pane_statuses(panes);
    }

    fn update_visibility(&mut self) {
        let overlapped = strip_visibility::overlaps(&self.snapshot.active_frames, &self.geometry.global);
        let mode = Mode::of(self.snapshot.user_visible, self.snapshot.hide_recent_apps, overlapped);
        self.visibility.set_tab(self.pane.tab.shown.then(|| self.geometry.tab()));
        let window_drag = self.snapshot.window_drag.is_some();
        let pointer = (mode == Mode::EdgeOnly || window_drag).then(|| self.pointer());
        let edge_pointer = pointer.filter(|_| mode == Mode::EdgeOnly);
        let decision = self.visibility.step(mode, edge_pointer, &self.geometry.global, Instant::now());
        // A window dragged over the strip column reveals it and opens the gap under the pointer.
        let over = pointer.filter(|p| window_drag && self.snapshot.user_visible && self.geometry.global.contains(*p));
        let drop_over = over.is_some();
        let before = over.and_then(|p| self.drop_slot_at(p).before);
        if drop_over != self.drop_over || (drop_over && before != self.drop_before) {
            if drop_over != self.drop_over {
                tracing::debug!(drop_over, "window drag over the strip");
            }
            self.drop_over = drop_over;
            self.drop_before = before;
            self.layout_cards(false);
            self.update_targets();
        }
        // Never hide under a card being dragged.
        let card_dragging = self.card_drag.as_ref().is_some_and(|d| d.mode != DragMode::Pressed);
        let shown = decision.shown || drop_over || (card_dragging && self.pane.shown);
        if shown != self.pane.shown {
            tracing::debug!(shown, ?mode, "strip visibility");
            self.slide(shown, true);
            self.update_targets();
        }
        // Below windows (Apple's) unless something needs it on top: a window or a card being
        // dragged, or the edge revealing it over a window that covers it.
        let lift = self.style.level == StripLevel::AboveWindows
            || drop_over
            || card_dragging
            || (self.pane.shown && mode == Mode::EdgeOnly);
        self.set_lifted(lift);
        self.window_drag_card(over);
        self.set_polling(decision.poll || window_drag || card_dragging);
        let tab = self.style.reveal.edge_tab
            && !self.snapshot.fullscreen
            && self.covered()
            && !self.pane.shown
            && !window_drag
            && !card_dragging;
        self.pane.tab.show(tab, self.mtm);
    }

    /// Puts the panel above windows or back below them.
    fn set_lifted(&mut self, lift: bool) {
        if self.pane.lifted == Some(lift) {
            return;
        }
        self.pane.lifted = Some(lift);
        self.pane.panel.setLevel(if lift { NSFloatingWindowLevel } else { BELOW_WINDOWS });
        // Only a pane already on its Desktop: a first order-in decides the Desktop it lives on.
        if lift && self.pane.placed && !self.snapshot.fullscreen {
            self.pane.panel.orderFrontRegardless();
        }
        tracing::debug!(lift, "strip level");
        self.publish_panel();
    }

    /// Slides the strip in or out (each card whole, in 3D) and lets clicks through while it
    /// is out.
    fn slide(&mut self, shown: bool, animated: bool) {
        self.pane.shown = shown;
        self.publish_cards();
        self.publish_panel();
        self.update_pass_through(None);
        if !shown {
            self.pressed = None;
            self.squeezed = None;
            self.set_hover(None);
            self.card_drag = None;
        }
        let dx = self.current_dx();
        let slide = self.slide_spring().filter(|_| animated);
        let m = self.m().clone();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for layers in self.pane.cards.values() {
            match slide {
                Some(s) => spring_sublayer_transform(s, &layers.card, perspective(&m, dx)),
                None => {
                    layers.card.removeAnimationForKey(&NSString::from_str("sublayerTransform"));
                    layers.card.setSublayerTransform(perspective(&m, dx));
                }
            }
        }
        CATransaction::commit();
    }

    fn set_polling(&mut self, on: bool) {
        match (on, self.poll_timer.is_some()) {
            (true, false) => {
                let tick = RcBlock::new(|_t: NonNull<NSTimer>| {
                    with_strip(Strip::update_visibility);
                });
                let seconds = (self.style.reveal.poll_ms.max(10) as f64) / 1000.0;
                // SAFETY: the block runs on the main run loop only.
                let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, true, &tick) };
                self.poll_timer = Some(timer);
            }
            (false, true) => {
                if let Some(t) = self.poll_timer.take() {
                    t.invalidate();
                }
            }
            _ => {}
        }
    }
}

/// Where incoming windows grow from, by window: the plane and the card slot, if any.
type Starts = HashMap<u32, (motion_math::Plane, Option<usize>)>;

/// Moves the incoming windows' starts that sit on a card (those with a slot) by `dx`, the
/// strip's slide since they were measured; window and off-screen starts stay.
fn slide_card_starts(mut starts: Starts, dx: f64) -> Starts {
    if dx != 0.0 {
        for (plane, slot) in starts.values_mut() {
            if slot.is_some() {
                plane.left_mid.x += dx;
            }
        }
    }
    starts
}
