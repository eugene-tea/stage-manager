//! The strip panel: a borderless, non-activating `NSPanel` on the left of the main display
//! that draws each stage as a card with Core Animation layers, the way Apple's does.
//!
//! - Snapshots arrive through [`strip_model::present`] on any thread and hop to the main
//!   thread; everything here runs on the main thread. Each snapshot carries the
//!   [`StripStyle`] (config), so look and motion follow config edits live.
//! - **Cards** ([`strip_layout`]): each window's thumbnail is its own plane, turned about
//!   its left edge inside its card's perspective container, whose eye sits at the display's
//!   vertical middle; app icons, badges and names live in a flat overlay above.
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
//! - **Drop target**: while the controller reports a window drag
//!   ([`StripSnapshot::window_drag`]) and the pointer is over the strip column, the strip is
//!   lifted above windows, a card-sized snapshot of the window, turned like a card, follows
//!   the pointer, and the cards open a gap at the slot under the pointer
//!   ([`drop_slot`] tells the controller which). Auto-hide never fights either drag.
//! - **Transitions** (`[animation]`, see `strip_motion`): snapshots of the windows that
//!   leave and arrive move in the motion overlay above everything while the real windows
//!   jump behind them.
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
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGImage};
use objc2_foundation::{MainThreadMarker, NSNumber, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer, NSValue};
use objc2_quartz_core::{
    CABasicAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction, CASpringAnimation, CATextLayer,
    CATransaction, CATransform3D, NSValueCATransform3DAdditions, kCAAlignmentCenter, kCAAlignmentLeft,
    kCAGravityResizeAspect, kCAGravityResizeAspectFill, kCAMediaTimingFunctionEaseOut, kCATruncationEnd,
};

use crate::config::{MotionStyle, Rgba, StripLevel, StripStyle};
use crate::engine::geometry::{Point, Rect, Size};
use crate::engine::stage_area::length_scale;
use crate::platform::capture::{Capturer, Target};
use crate::ui::motion_math::{self, DropSpot};
use crate::ui::strip_layout::{self, CardLayout, Metrics};
use crate::ui::strip_model::{self, CardSnapshot, StripDrop, StripEvent, StripSnapshot};
use crate::ui::strip_visibility::{self, Mode, Timing, Visibility};

#[path = "strip_motion.rs"]
mod motion;

/// The gap's stage id in the column while a window is dragged over it (never a real stage).
const DROP_STAGE: u64 = u64::MAX;
/// The strip's window level below windows (Apple's): just under normal windows.
const BELOW_WINDOWS: isize = NSNormalWindowLevel - 1;
/// A band of a card's height that will mean "onto this card" (a later merge); none yet.
const MERGE_BAND: f64 = 0.0;
/// The edge tab's window: width and height in points (the drawn pill is narrower).
const TAB_SIZE: (f64, f64) = (10.0, 72.0);
/// The drawn pill's width.
const TAB_PILL_WIDTH: f64 = 5.0;

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
        root.setFrame(s.root.bounds());
        root.setBackgroundColor(Some(&color(0.93, 0.55, 0.22, 1.0)));
        s.cards_root.removeFromSuperlayer();
        s.overlay.removeFromSuperlayer();
        root.addSublayer(&s.cards_root);
        root.addSublayer(&s.overlay);
        let result = crate::ui::strip_render::render_to_file(&root, 2.0, path);
        s.cards_root.removeFromSuperlayer();
        s.overlay.removeFromSuperlayer();
        s.root.addSublayer(&s.cards_root);
        s.root.addSublayer(&s.overlay);
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

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            with_strip(Strip::tab_clicked);
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
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
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
    /// Flat twin in the overlay (icons, badge, name), covering the panel; hover scales it
    /// about the same center.
    flat: Retained<CALayer>,
    /// Thumbnail planes per window id (back to front), each turned about its left edge.
    planes: Vec<(u32, Retained<CALayer>)>,
    /// Image layer per window id.
    images: Vec<(u32, Retained<CALayer>)>,
    /// The card's top-left corner when the flat twin's content was drawn.
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

fn spring(motion: &MotionStyle, key: &str, from: &AnyObject, to: &AnyObject) -> Retained<CASpringAnimation> {
    let anim = CASpringAnimation::animationWithKeyPath(Some(&NSString::from_str(key)));
    anim.setMass(motion.spring_mass);
    anim.setStiffness(motion.spring_stiffness);
    anim.setDamping(motion.spring_damping);
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
fn spring_position(motion: &MotionStyle, layer: &CALayer, to: CGPoint) {
    // SAFETY: presentationLayer returns a copy of the layer's drawn state or nil.
    let from = unsafe { layer.presentationLayer() }.map(|p| p.position()).unwrap_or_else(|| layer.position());
    layer.setPosition(to);
    if (from.x - to.x).abs() < 0.25 && (from.y - to.y).abs() < 0.25 {
        return;
    }
    let anim = spring(motion, "position", &NSValue::new(from), &NSValue::new(to));
    layer.addAnimation_forKey(&anim, Some(&NSString::from_str("position")));
}

/// Moves `layer` to `to` on the spring, or at once (cancelling any glide).
fn move_to(motion: &MotionStyle, layer: &CALayer, to: CGPoint, immediate: bool) {
    if immediate {
        layer.removeAnimationForKey(&NSString::from_str("position"));
        layer.setPosition(to);
    } else {
        spring_position(motion, layer, to);
    }
}

/// Moves `layer`'s `sublayerTransform` to `to` along the spring, from what is drawn now.
fn spring_sublayer_transform(motion: &MotionStyle, layer: &CALayer, to: CATransform3D) {
    // SAFETY: presentationLayer returns a copy of the layer's drawn state or nil.
    let from = unsafe { layer.presentationLayer() }
        .map(|p| p.sublayerTransform())
        .unwrap_or_else(|| layer.sublayerTransform());
    layer.setSublayerTransform(to);
    let anim = spring(motion, "sublayerTransform", &transform_value(from), &transform_value(to));
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
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
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
    /// Following the pointer within the strip column to another slot (`[strip] reorder`).
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

// ---------------------------------------------------------------------------------------
// The strip

struct Strip {
    mtm: MainThreadMarker,
    panel: Retained<NSPanel>,
    _view: Retained<StripView>,
    root: Retained<CALayer>,
    /// The cards' perspective containers.
    cards_root: Retained<CALayer>,
    /// Flat layer for icons, badges, names and the drop placeholder; slides in 2D.
    overlay: Retained<CALayer>,
    style: Arc<StripStyle>,
    geometry: Geometry,
    scale: f64,
    cards: HashMap<u64, CardLayers>,
    layouts: Vec<CardLayout>,
    snapshot: StripSnapshot,
    has_snapshot: bool,
    hovered: Option<u64>,
    pressed: Option<u64>,
    thumbs: HashMap<u32, CFRetained<CGImage>>,
    icons: HashMap<i32, Option<CFRetained<CGImage>>>,
    visibility: Visibility,
    shown: bool,
    poll_timer: Option<Retained<NSTimer>>,
    capturer: Capturer,
    card_drag: Option<CardDrag>,
    /// A window drag is over the strip: the gap shows (at `drop_before`'s card).
    drop_over: bool,
    drop_before: Option<u64>,
    /// The edge tab shown while a window hides the strip (`[reveal] edge_tab`).
    tab: EdgeTab,
    /// The launch reveal happened (once per launch).
    announced: bool,
    /// The panel is above windows now (`None` before the first decision).
    lifted: Option<bool>,
    /// Snapshots, transitions and the dragged window's card (`strip_motion`).
    motion: motion::Motion,
}

impl Strip {
    fn new(mtm: MainThreadMarker, interval: Duration) -> Self {
        let style = Arc::new(StripStyle::default());
        let geometry = Geometry::current(mtm, &style);
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
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setAcceptsMouseMovedEvents(true);
        // SAFETY: the panel is owned by `Strip` for the life of the app.
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
        let overlay = CALayer::new();
        root.addSublayer(&cards_root);
        root.addSublayer(&overlay);

        let scale = panel.backingScaleFactor();
        let capturer = Capturer::start(interval, thumbnail_arrived);
        let tab = EdgeTab::new(mtm, geometry.cocoa(geometry.tab()));
        let mut strip = Self {
            mtm,
            panel,
            _view: view,
            root,
            cards_root,
            overlay,
            style,
            geometry,
            scale,
            cards: HashMap::new(),
            layouts: Vec::new(),
            snapshot: StripSnapshot::default(),
            has_snapshot: false,
            hovered: None,
            pressed: None,
            thumbs: HashMap::new(),
            icons: HashMap::new(),
            visibility: Visibility::default(),
            shown: false,
            poll_timer: None,
            capturer,
            card_drag: None,
            drop_over: false,
            drop_before: None,
            tab,
            announced: false,
            lifted: None,
            motion: motion::Motion::new(mtm),
        };
        strip.apply_geometry();
        strip.slide(false, false);
        strip
    }

    fn m(&self) -> &Metrics {
        &self.geometry.m
    }

    /// How far left the cards sit when hidden.
    fn hidden_dx(&self) -> f64 {
        -(self.style.motion.slide_distance * self.m().k).max(self.geometry.panel.size.width + 30.0)
    }

    fn current_dx(&self) -> f64 {
        if self.shown { 0.0 } else { self.hidden_dx() }
    }

    /// Sizes the panel and the layer tree for the current geometry; cards are rebuilt.
    fn apply_geometry(&mut self) {
        let g = &self.geometry;
        self.panel.setFrame_display(g.panel, false);
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(g.panel.size.width, g.panel.size.height));
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.root.setFrame(bounds);
        self.cards_root.setFrame(bounds);
        self.overlay.setBounds(bounds);
        CATransaction::commit();
        for layers in self.cards.values() {
            layers.card.removeFromSuperlayer();
            layers.flat.removeFromSuperlayer();
        }
        self.cards.clear();
        self.icons.clear();
        self.visibility.set_timing(Timing::from_style(&self.style.reveal));
    }

    // -- snapshots ------------------------------------------------------------------------

    fn present(&mut self, snapshot: StripSnapshot) {
        if snapshot.style != self.style {
            self.style = snapshot.style.clone();
        }
        let geometry = Geometry::current(self.mtm, &self.style);
        if geometry != self.geometry {
            tracing::debug!(k = geometry.m.k, tilt = geometry.m.tilt.to_degrees(), d = geometry.m.perspective, "strip geometry");
            self.geometry = geometry;
            self.apply_geometry();
            self.tab.place(self.geometry.cocoa(self.geometry.tab()));
            let shown = self.shown;
            self.slide(shown, false);
        }
        let scale = self.panel.backingScaleFactor();
        let rescaled = scale != self.scale;
        self.scale = scale;

        let force = rescaled || snapshot.style != self.snapshot.style;
        let transition = snapshot.transition.clone();
        // Where incoming windows grow from: their cards before this snapshot replaces them.
        let starts = transition.as_ref().map(|t| self.incoming_starts(t));
        self.snapshot = snapshot;
        if self.snapshot.window_drag.is_none() {
            self.drop_over = false;
        }
        self.layout_cards(force);
        if let (Some(t), Some(starts)) = (transition, starts) {
            self.begin_transition(t, starts);
        }
        if let Some(id) = self.snapshot.parked {
            self.outgoing_parked(id);
        }
        self.after_snapshot();
        if !self.has_snapshot {
            self.has_snapshot = true;
            self.panel.orderFrontRegardless();
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

    /// The slot a release at `p` (global) goes to among `layouts`, leaving out `skip`'s card.
    fn slot_at(&self, layouts: &[CardLayout], p: Point, skip: Option<u64>) -> Option<u64> {
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
        match motion_math::drop_spot(&bands, y, MERGE_BAND) {
            DropSpot::Gap(i) | DropSpot::OnCard(i) => others.get(i).map(|l| l.stage),
        }
    }

    /// The slot for a window released at `p` (global).
    fn drop_slot_at(&self, p: Point) -> StripDrop {
        StripDrop { before: self.slot_at(&self.resting_layouts(), p, None) }
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
        self.cards.retain(|stage, layers| {
            let kept = keep.contains(stage);
            if !kept {
                layers.card.removeFromSuperlayer();
                layers.flat.removeFromSuperlayer();
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
        self.publish_cards();
    }

    /// Publishes where the cards are (global), for `ctl state`.
    fn publish_cards(&self) {
        let g = self.geometry.global;
        let frames = if self.shown {
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

    /// The hover transform of a card's container (anchored at the eye) and of its flat twin
    /// (anchored at `flat_anchor`, its position), for `layout`.
    fn hover_transforms(&self, layout: &CardLayout, hovered: bool, flat_anchor: Point) -> (CATransform3D, CATransform3D) {
        if !hovered {
            return (identity(), identity());
        }
        let m = self.m();
        let c = layout.hover_center(m);
        let s = m.hover_scale;
        (scale_about(s, c, m.eye), scale_about(s, c, flat_anchor))
    }

    /// Where a card's flat twin sits: its content was drawn for the card at `built`, and the
    /// card is now at `layout`.
    fn flat_position(&self, layout: &CardLayout, built: Point) -> Point {
        let size = self.geometry.panel.size;
        Point::new(size.width / 2.0 + layout.frame.x - built.x, size.height / 2.0 + layout.frame.y - built.y)
    }

    /// Creates or updates one card's layers and moves them to the card's slot: on the spring,
    /// or at once (`immediate`, a card following the pointer).
    fn place_card(&mut self, layout: &CardLayout, card: &CardSnapshot, force_rebuild: bool, immediate: bool) {
        let size = self.geometry.panel.size;
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size.width, size.height));
        let hovered = self.hovered == Some(card.stage);
        let rebuild = match self.cards.get(&card.stage) {
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
                let flat = CALayer::new();
                flat.setBounds(bounds);
                flat.setPosition(CGPoint::new(size.width / 2.0, size.height / 2.0));
                self.cards_root.addSublayer(&card_layer);
                self.overlay.addSublayer(&flat);
                let fade = self.style.cards.fade_ms as f64 / 1000.0;
                if self.motion.holds(card.stage) {
                    // A transition's snapshots are landing here; the card shows when they do.
                    card_layer.setOpacity(0.0);
                    flat.setOpacity(0.0);
                } else if self.has_snapshot {
                    fade_in(&card_layer, fade);
                    fade_in(&flat, fade);
                }
                let layers = CardLayers {
                    card: card_layer,
                    flat,
                    planes: Vec::new(),
                    images: Vec::new(),
                    built: Point::new(layout.frame.x, layout.frame.y),
                    snapshot: card.clone(),
                    layout: layout.clone(),
                };
                self.cards.insert(card.stage, layers);
                true
            }
        };
        if rebuild {
            self.fill_card(card.stage, layout, card);
        }
        let motion = self.style.motion.clone();
        let Some(built) = self.cards.get(&card.stage).map(|l| l.built) else { return };
        let flat_at = self.flat_position(layout, built);
        let (hover_3d, hover_flat) = self.hover_transforms(layout, hovered, flat_at);
        if let Some(layers) = self.cards.get_mut(&card.stage) {
            // Each plane glides to its new place; the icons follow with the flat twin.
            for (slot, (_, plane)) in layout.thumbs.iter().rev().zip(&layers.planes) {
                let to = CGPoint::new(layout.frame.x + slot.rect.x, layout.frame.y + slot.rect.y + slot.rect.h / 2.0);
                move_to(&motion, plane, to, immediate);
            }
            move_to(&motion, &layers.flat, CGPoint::new(flat_at.x, flat_at.y), immediate);
            layers.card.setTransform(hover_3d);
            layers.flat.setTransform(hover_flat);
            layers.snapshot = card.clone();
            layers.layout = layout.clone();
        }
    }

    /// (Re)builds a card's thumbnail planes, icons, badge and name.
    fn fill_card(&mut self, stage: u64, layout: &CardLayout, card: &CardSnapshot) {
        let Some(layers) = self.cards.get(&stage) else { return };
        let (card_layer, flat) = (layers.card.clone(), layers.flat.clone());
        let m = self.m().clone();
        let style = self.style.clone();
        let c = &style.cards;
        let k = m.k;
        // SAFETY: setting no sublayers removes the old children.
        unsafe {
            card_layer.setSublayers(None);
            flat.setSublayers(None);
        }
        let size = self.geometry.panel.size;
        flat.setPosition(CGPoint::new(size.width / 2.0, size.height / 2.0));
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
            card_layer.addSublayer(&plane);
            planes.push((slot.id, plane));
            images.push((slot.id, image));
        }
        // App icons, flat, in panel coordinates.
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
            let image = self.icons.entry(*pid).or_insert_with(|| app_icon(*pid, side));
            if let Some(img) = image {
                // SAFETY: a CGImage is valid layer contents.
                unsafe { icon.setContents(Some(as_object(&**img))) };
            }
            flat.addSublayer(&icon);
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
            flat.addSublayer(&badge);
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
            flat.addSublayer(&label);
        }
        if let Some(layers) = self.cards.get_mut(&stage) {
            layers.planes = planes;
            layers.images = images;
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
        for layers in self.cards.values() {
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
        let targets = if self.shown {
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

    /// The card under `p` (panel coordinates): its turned thumbnails and icons.
    fn card_at(&self, p: Point) -> Option<u64> {
        let m = self.m();
        self.layouts.iter().filter(|l| l.stage != DROP_STAGE).find(|l| l.bounds(m).contains(p)).map(|l| l.stage)
    }

    /// The app whose icon is under `p` on `stage`'s card.
    fn icon_at(&self, stage: u64, p: Point) -> Option<i32> {
        let l = self.layouts.iter().find(|l| l.stage == stage)?;
        l.icons.iter().find(|(_, r)| r.translate(l.frame.x, l.frame.y).contains(p)).map(|(pid, _)| *pid)
    }

    fn mouse_down(&mut self, p: Point) {
        self.pressed = if self.shown { self.card_at(p) } else { None };
        self.card_drag = self.pressed.and_then(|stage| self.drag_for(stage));
        if let Some(stage) = self.pressed {
            // A click is likely: fresh snapshots of what leaves and what arrives.
            self.prefetch(Some(stage));
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
        if let Some(layers) = self.cards.get(&stage) {
            layers.card.setOpacity(opacity);
            layers.flat.setOpacity(opacity);
        }
    }

    fn mouse_dragged(&mut self) {
        let p = self.pointer();
        let strip_right = self.geometry.global.max_x();
        let primary_height = self.geometry.primary_height;
        let mtm = self.mtm;
        let style = self.style.drag.clone();
        let reorder = self.style.reorder;
        let Some(drag) = self.card_drag.as_ref() else { return };
        if drag.mode == DragMode::Pressed && (p.x - drag.press.x).hypot(p.y - drag.press.y) <= style.threshold {
            return;
        }
        let stage = drag.stage;
        let in_column = self.geometry.global.contains(p);
        let mode = if reorder && in_column { DragMode::Reorder } else { DragMode::Out };
        let before = if mode == DragMode::Reorder { self.slot_at(&self.resting_layouts(), p, Some(stage)) } else { None };
        let image = self.thumbs.get(&drag.front).cloned();
        let was = drag.mode;
        if was == DragMode::Pressed {
            tracing::debug!(stage, ?mode, "card drag started");
        }
        {
            let Some(drag) = self.card_drag.as_mut() else { return };
            drag.mode = mode;
            drag.before = before;
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
                    self.set_hover(Some(stage));
                }
                self.layout_cards(false);
            }
            DragMode::Out | DragMode::Pressed => {
                if was != DragMode::Out {
                    self.set_card_opacity(stage, style.lifted_opacity as f32);
                    // The card goes back to its slot while its ghost is out.
                    self.layout_cards(false);
                }
            }
        }
        self.update_visibility();
    }

    fn mouse_up(&mut self, p: Point, shift: bool) -> Option<StripEvent> {
        if let Some(drag) = self.card_drag.take()
            && drag.mode != DragMode::Pressed
        {
            self.pressed = None;
            self.set_card_opacity(drag.stage, 1.0);
            let at = self.pointer();
            if drag.mode == DragMode::Reorder {
                // Put the card in its new slot now; the controller's snapshot agrees.
                let moved = self.reorder_locally(drag.stage, drag.before);
                self.layout_cards(false);
                self.update_visibility();
                return moved.then_some(StripEvent::CardMoved { stage: drag.stage, before: drag.before });
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
        let pressed = self.pressed.take()?;
        if !self.shown || self.card_at(p) != Some(pressed) {
            return None;
        }
        match self.icon_at(pressed, p) {
            Some(pid) if !shift => Some(StripEvent::IconClicked { stage: pressed, pid }),
            _ => Some(StripEvent::CardClicked { stage: pressed, shift }),
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

    fn hover_at(&mut self, p: Option<Point>) {
        let card = if self.shown { p.and_then(|p| self.card_at(p)) } else { None };
        self.set_hover(card);
    }

    fn set_hover(&mut self, card: Option<u64>) {
        if card == self.hovered {
            return;
        }
        if card.is_some() {
            // The pointer is on a card: a switch may follow.
            self.prefetch(card);
        }
        let previous = std::mem::replace(&mut self.hovered, card);
        let seconds = self.style.cards.hover_ms as f64 / 1000.0;
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for (stage, on) in [(previous, false), (card, true)] {
            let Some(stage) = stage else { continue };
            let Some(layout) = self.layouts.iter().find(|l| l.stage == stage).cloned() else { continue };
            let Some(built) = self.cards.get(&stage).map(|l| l.built) else { continue };
            let (t3, tf) = self.hover_transforms(&layout, on, self.flat_position(&layout, built));
            if let Some(layers) = self.cards.get(&stage) {
                ease_transform(&layers.card, t3, seconds);
                ease_transform(&layers.flat, tf, seconds);
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

    /// A click on the edge tab reveals the strip at once.
    fn tab_clicked(&mut self) {
        tracing::debug!("edge tab clicked");
        self.visibility.reveal_now();
        self.update_visibility();
    }

    fn update_visibility(&mut self) {
        let overlapped = strip_visibility::overlaps(&self.snapshot.active_frames, &self.geometry.global);
        let mode = Mode::of(self.snapshot.user_visible, self.snapshot.hide_recent_apps, overlapped);
        self.visibility.set_tab(self.tab.shown.then(|| self.geometry.tab()));
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
        let shown = decision.shown || drop_over || (card_dragging && self.shown);
        if shown != self.shown {
            tracing::debug!(shown, ?mode, "strip visibility");
            self.slide(shown, true);
            self.update_targets();
        }
        // Below windows (Apple's) unless something needs it on top: a window or a card being
        // dragged, or the edge revealing it over a window that covers it.
        let lift = self.style.level == StripLevel::AboveWindows
            || drop_over
            || card_dragging
            || (self.shown && mode == Mode::EdgeOnly);
        self.set_lifted(lift);
        self.window_drag_card(over);
        self.set_polling(decision.poll || window_drag || card_dragging);
        let tab = self.style.reveal.edge_tab && self.covered() && !self.shown && !window_drag && !card_dragging;
        self.tab.show(tab, self.mtm);
    }

    /// Puts the panel above windows or back below them.
    fn set_lifted(&mut self, lift: bool) {
        if self.lifted == Some(lift) {
            return;
        }
        self.lifted = Some(lift);
        self.panel.setLevel(if lift { NSFloatingWindowLevel } else { BELOW_WINDOWS });
        if lift {
            self.panel.orderFrontRegardless();
        }
        tracing::debug!(lift, "strip level");
    }

    /// Slides the strip in or out (cards in 3D, icons flat) and lets clicks through while it
    /// is out.
    fn slide(&mut self, shown: bool, animated: bool) {
        self.shown = shown;
        self.publish_cards();
        self.panel.setIgnoresMouseEvents(!shown);
        if !shown {
            self.set_hover(None);
            self.pressed = None;
            self.card_drag = None;
        }
        let dx = self.current_dx();
        let size = self.geometry.panel.size;
        let to = CGPoint::new(size.width / 2.0 + dx, size.height / 2.0);
        let motion = self.style.motion.clone();
        let m = self.m().clone();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for layers in self.cards.values() {
            if animated {
                spring_sublayer_transform(&motion, &layers.card, perspective(&m, dx));
            } else {
                layers.card.setSublayerTransform(perspective(&m, dx));
            }
        }
        if animated {
            spring_position(&motion, &self.overlay, to);
        } else {
            self.overlay.setPosition(to);
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
