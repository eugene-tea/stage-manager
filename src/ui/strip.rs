//! The strip panel: a borderless, non-activating `NSPanel` on the left of the main display
//! that draws one tilted card per stage with Core Animation layers.
//!
//! - Snapshots arrive through [`strip_model::present`] on any thread and hop to the main
//!   thread; everything here runs on the main thread.
//! - Cards are layers under one perspective (`sublayerTransform.m34`) centered at the
//!   display's vertical center, each turned about its left edge (see [`strip_layout`]).
//!   App icons, badges and names live in a flat overlay above them.
//! - Hover grows a card and shifts it left (eased, ~150 ms); a click acts on mouse-up in
//!   the same card and goes out as [`StripEvent::CardClicked`].
//! - Cards glide to new slots on the measured spring; the strip slides out to the left when
//!   hidden (see [`strip_visibility`]).
//! - Thumbnails come from [`capture`], refreshed every `thumbnail_interval_ms`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSEventModifierFlags, NSFloatingWindowLevel, NSPanel,
    NSRunningApplication, NSScreen, NSTrackingArea, NSTrackingAreaOptions, NSView,
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

use crate::engine::geometry::{Point, Rect};
use crate::platform::capture::{Capturer, Target};
use crate::ui::strip_layout::{
    self, CardLayout, HOVER_SCALE, HOVER_SHIFT, ICON_BELOW, ICON_LEFT_OVERHANG, ICON_STEP, STRIP_WIDTH,
};
use crate::ui::strip_model::{self, CardSnapshot, StripEvent, StripSnapshot};
use crate::ui::strip_visibility::{self, Mode, POLL_INTERVAL, Visibility};

/// Spring for card motion and the slide (UX-SPEC "Transitions").
const SPRING_MASS: f64 = 1.0;
const SPRING_STIFFNESS: f64 = 240.0;
const SPRING_DAMPING: f64 = 31.0;
/// Hover ease duration.
const HOVER_SECONDS: f64 = 0.15;
/// New cards fade in over this long.
const FADE_SECONDS: f64 = 0.2;
/// Thumbnails are captured at this multiple of their point size.
const CAPTURE_SCALE: f64 = 2.0;
/// Corner radius of a thumbnail.
const THUMB_RADIUS: f64 = 5.0;

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

/// Receives captured thumbnails on the main thread.
fn thumbnail_arrived(id: u32, image: CFRetained<CGImage>) {
    with_strip(|s| s.set_thumbnail(id, image));
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

// ---------------------------------------------------------------------------------------
// Geometry of the panel on the main display

#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    /// Panel frame, Cocoa coordinates (bottom-left origin).
    panel: NSRect,
    /// The strip area in global top-left coordinates.
    global: Rect,
    /// Height of the primary display (for flipping Cocoa y).
    primary_height: f64,
    /// The display's vertical center in panel coordinates.
    center_y: f64,
}

impl Geometry {
    fn current(mtm: MainThreadMarker) -> Self {
        let screens = NSScreen::screens(mtm);
        let main = screens.firstObject();
        let (frame, visible) = match &main {
            Some(s) => (s.frame(), s.visibleFrame()),
            None => {
                let r = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0));
                (r, r)
            }
        };
        let primary_height = frame.size.height;
        let panel = NSRect::new(visible.origin, NSSize::new(STRIP_WIDTH, visible.size.height));
        let top = primary_height - (visible.origin.y + visible.size.height);
        let global = Rect::new(visible.origin.x, top, STRIP_WIDTH, visible.size.height);
        let display_center = primary_height - (frame.origin.y + frame.size.height / 2.0);
        Self { panel, global, primary_height, center_y: display_center - top }
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
    /// Tilted container of the thumbnails.
    card: Retained<CALayer>,
    /// Flat twin in the overlay (icons, badge, name); same anchor and position.
    flat: Retained<CALayer>,
    /// Image layer per window id.
    images: Vec<(u32, Retained<CALayer>)>,
    /// What the layers were built from.
    snapshot: CardSnapshot,
    layout: CardLayout,
}

fn color(r: f64, g: f64, b: f64, a: f64) -> CFRetained<CGColor> {
    CGColor::new_srgb(r, g, b, a)
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

/// The card's resting transform: the tilt, plus the hover growth when hovered.
fn card_transform(theta: f64, hovered: bool) -> CATransform3D {
    let tilt = CATransform3D::new_rotation(theta, 0.0, 1.0, 0.0);
    if hovered { tilt.concat(hover_transform()) } else { tilt }
}

fn hover_transform() -> CATransform3D {
    CATransform3D::new_scale(HOVER_SCALE, HOVER_SCALE, 1.0).concat(CATransform3D::new_translation(-HOVER_SHIFT, 0.0, 0.0))
}

fn flat_transform(hovered: bool) -> CATransform3D {
    if hovered { hover_transform() } else { CATransform3D::new_scale(1.0, 1.0, 1.0) }
}

fn spring(key: &str, from: &AnyObject, to: &AnyObject) -> Retained<CASpringAnimation> {
    let anim = CASpringAnimation::animationWithKeyPath(Some(&NSString::from_str(key)));
    anim.setMass(SPRING_MASS);
    anim.setStiffness(SPRING_STIFFNESS);
    anim.setDamping(SPRING_DAMPING);
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
fn spring_position(layer: &CALayer, to: CGPoint) {
    // SAFETY: presentationLayer returns a copy of the layer's drawn state or nil.
    let from = unsafe { layer.presentationLayer() }.map(|p| p.position()).unwrap_or_else(|| layer.position());
    layer.setPosition(to);
    if (from.x - to.x).abs() < 0.25 && (from.y - to.y).abs() < 0.25 {
        return;
    }
    let anim = spring("position", &NSValue::new(from), &NSValue::new(to));
    layer.addAnimation_forKey(&anim, Some(&NSString::from_str("position")));
}

/// Eases `layer`'s transform to `to` from what is drawn now.
fn ease_transform(layer: &CALayer, to: CATransform3D) {
    // SAFETY: presentationLayer returns a copy of the layer's drawn state or nil.
    let from = unsafe { layer.presentationLayer() }.map(|p| p.transform()).unwrap_or_else(|| layer.transform());
    layer.setTransform(to);
    let anim = ease("transform", &transform_value(from), &transform_value(to), HOVER_SECONDS);
    layer.addAnimation_forKey(&anim, Some(&NSString::from_str("transform")));
}

fn fade_in(layer: &CALayer) {
    let anim = ease("opacity", &NSNumber::new_f32(0.0), &NSNumber::new_f32(1.0), FADE_SECONDS);
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

/// An app's icon as an image for a layer.
fn app_icon(pid: i32) -> Option<CFRetained<CGImage>> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    let icon = app.icon()?;
    let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(64.0, 64.0));
    // SAFETY: a valid proposed rect pointer and no context or hints.
    let image = unsafe { icon.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
    // SAFETY: the Retained<CGImage> owns one reference; re-wrap it as CFRetained.
    let ptr = NonNull::new(Retained::into_raw(image))?;
    // SAFETY: as above; the pointer came from `into_raw`, so its reference moves over.
    Some(unsafe { CFRetained::from_raw(ptr) })
}

// ---------------------------------------------------------------------------------------
// The strip

struct Strip {
    mtm: MainThreadMarker,
    panel: Retained<NSPanel>,
    _view: Retained<StripView>,
    root: Retained<CALayer>,
    /// Slides left to hide.
    content: Retained<CALayer>,
    /// Perspective container of the tilted cards.
    stage: Retained<CALayer>,
    /// Flat layer for icons, badges and names.
    overlay: Retained<CALayer>,
    geometry: Geometry,
    theta: f64,
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
}

impl Strip {
    fn new(mtm: MainThreadMarker, interval: Duration) -> Self {
        let geometry = Geometry::current(mtm);
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            geometry.panel,
            style,
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
        panel.setFloatingPanel(true);
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

        let content = CALayer::new();
        let stage = CALayer::new();
        let overlay = CALayer::new();
        content.addSublayer(&stage);
        content.addSublayer(&overlay);
        root.addSublayer(&content);

        let scale = panel.backingScaleFactor();
        let capturer = Capturer::start(interval, thumbnail_arrived);
        let mut strip = Self {
            mtm,
            panel,
            _view: view,
            root,
            content,
            stage,
            overlay,
            geometry,
            theta: 0.0,
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
        };
        strip.apply_geometry();
        strip.slide(false, false);
        strip
    }

    /// Sizes the panel and the layer tree for the current geometry.
    fn apply_geometry(&mut self) {
        let g = self.geometry;
        self.panel.setFrame_display(g.panel, false);
        let size = CGSize::new(g.panel.size.width, g.panel.size.height);
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), size);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.root.setFrame(bounds);
        self.content.setBounds(bounds);
        self.overlay.setFrame(bounds);
        // The perspective is centered at (center_x, the display's vertical center): put the
        // stage layer's anchor there while keeping its frame equal to the bounds.
        let center_x = STRIP_WIDTH / 2.0;
        let cy = g.center_y.clamp(0.0, size.height.max(1.0));
        self.stage.setBounds(bounds);
        self.stage.setAnchorPoint(CGPoint::new(center_x / size.width.max(1.0), cy / size.height.max(1.0)));
        self.stage.setPosition(CGPoint::new(center_x, cy));
        let mut perspective = CATransform3D::new_scale(1.0, 1.0, 1.0);
        perspective.m34 = -1.0 / strip_layout::PERSPECTIVE_DISTANCE;
        self.stage.setSublayerTransform(perspective);
        CATransaction::commit();
        self.theta = strip_layout::tilt_angle(center_x);
    }

    // -- snapshots ------------------------------------------------------------------------

    fn present(&mut self, snapshot: StripSnapshot) {
        let geometry = Geometry::current(self.mtm);
        if geometry != self.geometry {
            self.geometry = geometry;
            self.apply_geometry();
            let shown = self.shown;
            self.slide(shown, false);
        }
        let scale = self.panel.backingScaleFactor();
        let rescaled = scale != self.scale;
        self.scale = scale;

        let layouts = strip_layout::layout_column(&snapshot.cards, self.geometry.height(), self.geometry.center_y);
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
        for (layout, card) in layouts.iter().zip(&snapshot.cards) {
            let force = rescaled || snapshot.show_numbers != self.snapshot.show_numbers;
            self.place_card(layout, card, snapshot.show_numbers, force);
        }
        CATransaction::commit();

        self.layouts = layouts;
        self.snapshot = snapshot;
        if !self.has_snapshot {
            self.has_snapshot = true;
            self.panel.orderFrontRegardless();
        }
        self.update_visibility();
        self.update_targets();
    }

    /// Creates or updates one card's layers and moves them to the card's slot.
    fn place_card(&mut self, layout: &CardLayout, card: &CardSnapshot, show_numbers: bool, force_rebuild: bool) {
        let f = layout.frame;
        let position = CGPoint::new(f.x, f.y + f.h / 2.0);
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(f.w, f.h));
        let rebuild = match self.cards.get(&card.stage) {
            Some(layers) => {
                force_rebuild
                    || layers.snapshot != *card
                    || layers.layout.frame.size() != f.size()
                    || layers.layout.thumbs != layout.thumbs
                    || layers.layout.icons != layout.icons
            }
            None => {
                let hovered = self.hovered == Some(card.stage);
                let card_layer = CALayer::new();
                card_layer.setAnchorPoint(CGPoint::new(0.0, 0.5));
                card_layer.setPosition(position);
                card_layer.setTransform(card_transform(self.theta, hovered));
                card_layer.setAllowsEdgeAntialiasing(true);
                let flat = CALayer::new();
                flat.setAnchorPoint(CGPoint::new(0.0, 0.5));
                flat.setPosition(position);
                flat.setTransform(flat_transform(hovered));
                self.stage.addSublayer(&card_layer);
                self.overlay.addSublayer(&flat);
                if self.has_snapshot {
                    fade_in(&card_layer);
                    fade_in(&flat);
                }
                let layers = CardLayers {
                    card: card_layer,
                    flat,
                    images: Vec::new(),
                    snapshot: card.clone(),
                    layout: layout.clone(),
                };
                self.cards.insert(card.stage, layers);
                true
            }
        };
        let images = if rebuild { Some(self.fill_card(card.stage, layout, card, show_numbers)) } else { None };
        if let Some(layers) = self.cards.get_mut(&card.stage) {
            if let Some(images) = images {
                layers.images = images;
            }
            layers.card.setBounds(bounds);
            layers.flat.setBounds(bounds);
            spring_position(&layers.card, position);
            spring_position(&layers.flat, position);
            layers.snapshot = card.clone();
            layers.layout = layout.clone();
        }
    }

    /// (Re)builds a card's thumbnails, icons, badge and name; returns the image layers.
    fn fill_card(
        &mut self,
        stage: u64,
        layout: &CardLayout,
        card: &CardSnapshot,
        show_numbers: bool,
    ) -> Vec<(u32, Retained<CALayer>)> {
        let mut images = Vec::new();
        let Some(layers) = self.cards.get(&stage) else { return images };
        let (card_layer, flat) = (layers.card.clone(), layers.flat.clone());
        // SAFETY: setting no sublayers removes the old children.
        unsafe {
            card_layer.setSublayers(None);
            flat.setSublayers(None);
        }
        // Thumbnails back to front.
        for slot in layout.thumbs.iter().rev() {
            let holder = CALayer::new();
            holder.setFrame(cg_rect(&slot.rect));
            holder.setCornerRadius(THUMB_RADIUS);
            holder.setBackgroundColor(Some(&color(0.22, 0.22, 0.24, 0.92)));
            holder.setBorderWidth(0.5);
            holder.setBorderColor(Some(&color(1.0, 1.0, 1.0, 0.18)));
            holder.setShadowColor(Some(&color(0.0, 0.0, 0.0, 1.0)));
            holder.setShadowOpacity(0.35);
            holder.setShadowRadius(6.0);
            holder.setShadowOffset(CGSize::new(0.0, 3.0));
            holder.setAllowsEdgeAntialiasing(true);
            let image = CALayer::new();
            image.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(slot.rect.w, slot.rect.h)));
            image.setCornerRadius(THUMB_RADIUS);
            image.setMasksToBounds(true);
            // SAFETY: framework constant.
            image.setContentsGravity(unsafe { kCAGravityResizeAspectFill });
            image.setAllowsEdgeAntialiasing(true);
            if let Some(img) = self.thumbs.get(&slot.id) {
                // SAFETY: a CGImage is valid layer contents.
                unsafe { image.setContents(Some(as_object(&**img))) };
            }
            holder.addSublayer(&image);
            card_layer.addSublayer(&holder);
            images.push((slot.id, image));
        }
        // App icons, flat.
        for (pid, rect) in &layout.icons {
            let icon = CALayer::new();
            icon.setFrame(cg_rect(rect));
            // SAFETY: framework constant.
            icon.setContentsGravity(unsafe { kCAGravityResizeAspect });
            icon.setShadowColor(Some(&color(0.0, 0.0, 0.0, 1.0)));
            icon.setShadowOpacity(0.3);
            icon.setShadowRadius(3.0);
            icon.setShadowOffset(CGSize::new(0.0, 1.5));
            let image = self.icons.entry(*pid).or_insert_with(|| app_icon(*pid));
            if let Some(img) = image {
                // SAFETY: a CGImage is valid layer contents.
                unsafe { icon.setContents(Some(as_object(&**img))) };
            }
            flat.addSublayer(&icon);
        }
        let size = layout.frame.size();
        if show_numbers {
            let text = card.number.to_string();
            let w = 18.0_f64.max(8.0 + 7.0 * text.len() as f64);
            let badge = CALayer::new();
            badge.setFrame(CGRect::new(CGPoint::new(-6.0, -7.0), CGSize::new(w, 18.0)));
            badge.setCornerRadius(9.0);
            badge.setBackgroundColor(Some(&color(0.08, 0.08, 0.1, 0.78)));
            badge.setBorderWidth(0.5);
            badge.setBorderColor(Some(&color(1.0, 1.0, 1.0, 0.35)));
            let label = text_layer(&text, 11.0, self.scale, true);
            label.setFrame(CGRect::new(CGPoint::new(0.0, 2.0), CGSize::new(w, 14.0)));
            badge.addSublayer(&label);
            flat.addSublayer(&badge);
        }
        if let Some(name) = card.name.as_deref().filter(|n| !n.is_empty()) {
            let icons_w = layout.icons.len() as f64 * ICON_STEP - ICON_LEFT_OVERHANG;
            let x = icons_w.max(0.0) + 2.0;
            let label = text_layer(name, 11.0, self.scale, false);
            label.setFrame(CGRect::new(CGPoint::new(x, size.h + ICON_BELOW - 16.0), CGSize::new((STRIP_WIDTH - 20.0 - x).max(30.0), 15.0)));
            label.setShadowColor(Some(&color(0.0, 0.0, 0.0, 1.0)));
            label.setShadowOpacity(0.6);
            label.setShadowRadius(2.0);
            label.setShadowOffset(CGSize::new(0.0, 1.0));
            flat.addSublayer(&label);
        }
        images
    }

    // -- thumbnails -------------------------------------------------------------------------

    fn set_thumbnail(&mut self, id: u32, image: CFRetained<CGImage>) {
        let shown = self.layouts.iter().any(|l| l.thumbs.iter().any(|t| t.id == id));
        if !shown {
            return;
        }
        for layers in self.cards.values() {
            for (wid, layer) in &layers.images {
                if *wid == id {
                    // SAFETY: a CGImage is valid layer contents.
                    unsafe { layer.setContents(Some(as_object(&*image))) };
                }
            }
        }
        self.thumbs.insert(id, image);
    }

    /// Tells the capturer what to capture: every shown thumbnail while the strip shows.
    fn update_targets(&mut self) {
        let shown_ids: Vec<u32> = self.layouts.iter().flat_map(|l| l.thumbs.iter().map(|t| t.id)).collect();
        self.thumbs.retain(|id, _| shown_ids.contains(id));
        let targets = if self.shown {
            self.layouts
                .iter()
                .flat_map(|l| l.thumbs.iter())
                .map(|t| Target {
                    id: t.id,
                    width: (t.rect.w * HOVER_SCALE * CAPTURE_SCALE).round() as u32,
                    height: (t.rect.h * HOVER_SCALE * CAPTURE_SCALE).round() as u32,
                })
                .collect()
        } else {
            Vec::new()
        };
        self.capturer.set_targets(targets);
    }

    // -- pointer ----------------------------------------------------------------------------

    /// The card under `p` (panel coordinates): its box shortened by the tilt, plus the icon
    /// row below it.
    fn card_at(&self, p: Point) -> Option<u64> {
        self.layouts.iter().find_map(|l| {
            let (w, _) = strip_layout::projected(l.frame.w, STRIP_WIDTH / 2.0);
            let hit = Rect::new(l.frame.x - ICON_LEFT_OVERHANG, l.frame.y, w + ICON_LEFT_OVERHANG, l.frame.h + ICON_BELOW);
            hit.contains(p).then_some(l.stage)
        })
    }

    fn mouse_down(&mut self, p: Point) {
        self.pressed = if self.shown { self.card_at(p) } else { None };
        tracing::trace!(x = p.x, y = p.y, card = ?self.pressed, "strip mouse down");
    }

    fn mouse_up(&mut self, p: Point, shift: bool) -> Option<StripEvent> {
        let pressed = self.pressed.take()?;
        (self.shown && self.card_at(p) == Some(pressed)).then_some(StripEvent::CardClicked { stage: pressed, shift })
    }

    fn hover_at(&mut self, p: Option<Point>) {
        let card = if self.shown { p.and_then(|p| self.card_at(p)) } else { None };
        self.set_hover(card);
    }

    fn set_hover(&mut self, card: Option<u64>) {
        if card == self.hovered {
            return;
        }
        let previous = std::mem::replace(&mut self.hovered, card);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for (stage, on) in [(previous, false), (card, true)] {
            if let Some(layers) = stage.and_then(|s| self.cards.get(&s)) {
                ease_transform(&layers.card, card_transform(self.theta, on));
                ease_transform(&layers.flat, flat_transform(on));
            }
        }
        CATransaction::commit();
    }

    // -- visibility -------------------------------------------------------------------------

    fn pointer(&self) -> Point {
        self.geometry.global_point(NSEvent::mouseLocation())
    }

    fn update_visibility(&mut self) {
        let overlapped = strip_visibility::overlaps(&self.snapshot.active_frames, &self.geometry.global);
        let mode = Mode::of(self.snapshot.user_visible, self.snapshot.hide_recent_apps, overlapped);
        let pointer = (mode == Mode::EdgeOnly).then(|| self.pointer());
        let decision = self.visibility.step(mode, pointer, &self.geometry.global, Instant::now());
        if decision.shown != self.shown {
            tracing::debug!(shown = decision.shown, ?mode, "strip visibility");
            self.slide(decision.shown, true);
            self.update_targets();
        }
        self.set_polling(decision.poll);
    }

    /// Slides the strip in or out and lets clicks through while it is out.
    fn slide(&mut self, shown: bool, animated: bool) {
        self.shown = shown;
        self.panel.setIgnoresMouseEvents(!shown);
        if !shown {
            self.set_hover(None);
            self.pressed = None;
        }
        let size = self.geometry.panel.size;
        let x = if shown { size.width / 2.0 } else { size.width / 2.0 - (size.width + 30.0) };
        let to = CGPoint::new(x, size.height / 2.0);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if animated {
            spring_position(&self.content, to);
        } else {
            self.content.setPosition(to);
        }
        CATransaction::commit();
    }

    fn set_polling(&mut self, on: bool) {
        match (on, self.poll_timer.is_some()) {
            (true, false) => {
                let tick = RcBlock::new(|_t: NonNull<NSTimer>| {
                    with_strip(Strip::update_visibility);
                });
                // SAFETY: the block runs on the main run loop only.
                let timer = unsafe {
                    NSTimer::scheduledTimerWithTimeInterval_repeats_block(POLL_INTERVAL.as_secs_f64(), true, &tick)
                };
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
