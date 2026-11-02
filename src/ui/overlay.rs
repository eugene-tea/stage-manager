//! The motion overlay: a click-through, transparent panel over the whole main display, above
//! every normal window, where window snapshots ("ghosts") move while the real windows jump
//! behind them (`[animation]`). Main thread only.
//!
//! It stays below floating windows ([`OVER_WINDOWS`]): an always-on-top window (a music
//! player in a corner) stays on top through a switch, as with Apple's, which animates each
//! window at its own level. At the status level it drew the park corner's wallpaper cover
//! over such a window for the length of every switch (the owner's report, 10-22).
//!
//! The overlay shows only while something moves; it stays on the active Space (it moves
//! there when shown, it never joins every Space). Its layers repeat the strip's perspective:
//! each ghost has a container of its own whose eye is the strip's eye, so a ghost turned like
//! a card and placed on a card's thumbnail looks exactly like that thumbnail, and a flat ghost
//! at a window's frame looks exactly like the window. The ghosts stack in the order they were
//! added, the last on top: in one shared container Core Animation would sort turned ghosts by
//! depth, as it did the card's planes (owner, 10-27), and a second app's ghost would start
//! over the front window's and flip under it as they flatten.
//!
//! **Coordinates.** Planes are given top-left (global minus [`Overlay::origin`]); the
//! overlay's view is flipped ([`OverlayView`]), which is what keeps its root layer top-down.
//! A plain `NSView` hosting a layer marked `geometryFlipped` does not: AppKit resets the flag
//! to the view's own (unflipped) geometry, and every ghost was drawn mirrored about the
//! display's middle (measured 10-12: a layer at the top-left of a plain view rendered at the
//! bottom-left; with a flipped view at the top-left). Windows near the middle landed "close";
//! a card's snapshot shrank into the mirrored slot; a dragged window's card moved opposite the
//! pointer.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFloatingWindowLevel, NSNormalWindowLevel, NSPanel, NSStatusWindowLevel, NSView,
    NSWindowOrderingMode,
    NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGImage};
use objc2_foundation::{MainThreadMarker, NSNumber, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSValue};
use objc2_quartz_core::{
    CABasicAnimation, CALayer, CAMediaTiming, CASpringAnimation, CATransaction, CATransform3D, NSValueCATransform3DAdditions,
    kCAGravityResize,
};

use crate::engine::geometry::{Point, Rect, Size};
use crate::platform::capture::Picture;
use crate::platform::capture_frame::Extent;
use crate::ui::motion_math::{Plane, Spring, spring_duration};
use crate::ui::panels;
use crate::ui::window_shadow::{ShadowLayers, WindowShadow};

/// Every key path a ghost animates.
/// The overlay's level when no window is given to go under: above every normal window (so
/// the real windows jump behind the ghosts), below floating ones (always-on-top windows, and
/// the strip while it is raised over windows).
const OVER_WINDOWS: isize = NSFloatingWindowLevel - 1;

const KEYS: [&str; 4] = ["position", "bounds", "transform", "cornerRadius"];

fn as_object<T>(cf: &T) -> &AnyObject {
    // SAFETY: every CF type is a valid Objective-C object pointer.
    unsafe { &*(cf as *const T).cast::<AnyObject>() }
}

fn color(r: f64, g: f64, b: f64, a: f64) -> CFRetained<CGColor> {
    CGColor::new_srgb(r, g, b, a)
}

pub(crate) fn transform_value(t: CATransform3D) -> Retained<NSValue> {
    // SAFETY: QuartzCore's NSValue category; any transform is valid.
    unsafe { NSValue::valueWithCATransform3D(t) }
}

fn turn(tilt: f64) -> CATransform3D {
    CATransform3D::new_rotation(tilt, 0.0, 1.0, 0.0)
}

fn bounds(p: &Plane) -> CGRect {
    CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(p.size.w.max(1.0), p.size.h.max(1.0)))
}

/// A spring animation of `key` from `from` to `to`, `duration` seconds long
/// ([`spring_duration`]: on until it rests on the curve, never Core Animation's
/// `settlingDuration`, which cut it short by a point on a long travel).
pub(crate) fn spring_anim(spring: Spring, key: &str, from: &AnyObject, to: &AnyObject, duration: f64) -> Retained<CASpringAnimation> {
    let anim = CASpringAnimation::animationWithKeyPath(Some(&NSString::from_str(key)));
    anim.setMass(spring.mass);
    anim.setStiffness(spring.stiffness);
    anim.setDamping(spring.damping);
    anim.setDuration(duration.max(1e-3));
    // SAFETY: from/to are values of the animated property's type.
    unsafe {
        anim.setFromValue(Some(from));
        anim.setToValue(Some(to));
    }
    anim
}

/// What a ghost's layers show at a plane (model values; no animation).
struct Look {
    position: CGPoint,
    bounds: CGRect,
    transform: CATransform3D,
    corner: f64,
}

impl Look {
    fn of(p: &Plane) -> Self {
        Self {
            position: CGPoint::new(p.left_mid.x, p.left_mid.y),
            bounds: bounds(p),
            transform: turn(p.tilt),
            corner: p.corner,
        }
    }
}

/// The card's shadow on a snapshot (`[cards] shadow_*`, at the strip's scale): a Core
/// Animation shadow, as the strip draws its thumbnails'.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardShadow {
    pub color: (f64, f64, f64, f64),
    pub opacity: f64,
    pub radius: f64,
    pub offset: (f64, f64),
}

impl CardShadow {
    /// No shadow.
    pub const NONE: CardShadow = CardShadow { color: (0.0, 0.0, 0.0, 1.0), opacity: 0.0, radius: 0.0, offset: (0.0, 0.0) };
}

/// How a snapshot's shadow looks: the window's at full size, the card's on a card, and how
/// far along between the two it starts (0 a card, 1 the window).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GhostShadow {
    /// The window's shadow (the key window's or an inactive one's).
    pub window: WindowShadow,
    /// The window's corner radius (points; the shadow follows it).
    pub corner: f64,
    /// The window's size (points; the inner rim is laid out at it).
    pub window_size: (f64, f64),
    pub card: CardShadow,
    pub amount: f64,
}

/// One moving snapshot: a plane (the window's rectangle) holding the window's shadow
/// ([`ShadowLayers`]) and a body that carries the card's shadow and holds a masked image (the
/// window's picture, reaching past the plane where child windows stick out).
pub struct Ghost {
    /// The window it shows.
    pub window: u32,
    /// Its perspective container, in the overlay's stack.
    depth: Retained<CALayer>,
    plane: Retained<CALayer>,
    /// The image's frame on the plane; its Core Animation shadow is the card's.
    body: Retained<CALayer>,
    image: Retained<CALayer>,
    /// The window's shadow under the body (none on the corner's cover).
    shadow: Option<ShadowLayers>,
    /// The card shadow's opacity on a card (it fades out toward the window).
    card_opacity: f64,
    /// Where the window is in the picture.
    extent: Extent,
    /// A picture whose window sits elsewhere in it, arrived mid-flight: shown at the next
    /// placement, so the flying image keeps its animation.
    pending: Option<Picture>,
    /// Where it is headed (or sits).
    pub target: Plane,
    /// How long its current flight's spring runs (seconds; [`spring_duration`] of its travel):
    /// its shadow's springs run as long.
    flight: f64,
}

/// How long a newer picture of the window takes to replace the one a flying snapshot shows
/// (seconds): a cross-fade, so a look that changed (a window made key while parked, captured
/// again) melts in instead of flipping in one frame.
pub const PICTURE_FADE: f64 = 0.15;

/// The farthest any edge of a plane of `from_bounds` at `from_position` (its left edge's
/// middle) is from the same edge of one of `to_bounds` at `to_position` (points, before the
/// turn).
fn plane_travel(from_position: CGPoint, from_bounds: CGRect, to_position: CGPoint, to_bounds: CGRect) -> f64 {
    let edges = |p: CGPoint, b: CGRect| {
        let (w, h) = (b.size.width, b.size.height);
        Rect::new(p.x, p.y - h / 2.0, w, h)
    };
    crate::ui::motion_math::travel(edges(from_position, from_bounds), edges(to_position, to_bounds))
}

/// The image layer's position and bounds (anchor at its top-left corner) on a plane of
/// `bounds`: the window's part on the plane, the rest around it.
fn image_frame(extent: &Extent, bounds: CGRect) -> (CGPoint, CGRect) {
    let whole = extent.around(Rect::new(0.0, 0.0, bounds.size.width, bounds.size.height));
    (CGPoint::new(whole.x, whole.y), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(whole.w, whole.h)))
}

impl Ghost {
    /// Sets the picture (a newer snapshot arrived); the window keeps its place on the plane.
    /// One whose window sits elsewhere in it (by
    /// [`SAME_PT`](crate::platform::capture_frame::SAME_PT) or more as drawn) waits while the
    /// ghost flies, and shows at its next placement ([`Ghost::set`], [`Ghost::spring_to`]), so
    /// a flight is never cut short.
    ///
    /// `fade`: the new picture cross-fades in over [`PICTURE_FADE`] (a switch's snapshot,
    /// whose window may look different now: made key since it was captured); else at once.
    pub fn set_image(&mut self, picture: &Picture, fade: bool) {
        let b = self.plane.bounds();
        let same = picture.extent.same_as(&self.extent, b.size.width, b.size.height);
        if !same && self.flying() {
            self.pending = Some(picture.clone());
            return;
        }
        self.pending = None;
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        // What is drawn now, before the new picture replaces it.
        // SAFETY: presentationLayer returns a copy of the drawn state or nil; its contents are
        // the CGImage set on the layer, or nil.
        let before = if fade && same { unsafe { self.image.presentationLayer().and_then(|l| l.contents()) } } else { None };
        self.show(picture, !same);
        if let Some(before) = before {
            let key = NSString::from_str("contents");
            let anim = CABasicAnimation::animationWithKeyPath(Some(&key));
            // SAFETY: both are the layer's contents (CGImages).
            unsafe {
                anim.setFromValue(Some(&before));
                anim.setToValue(Some(as_object(&*picture.image)));
            }
            anim.setDuration(PICTURE_FADE);
            self.image.addAnimation_forKey(&anim, Some(&key));
        }
        CATransaction::commit();
    }

    /// True while a spring moves the ghost's image.
    fn flying(&self) -> bool {
        self.body.animationKeys().is_some_and(|keys| keys.count() > 0)
    }

    /// Puts the image at `position` with `bounds` on the plane (the body there, the image
    /// filling it), at once.
    fn place_image(&self, position: CGPoint, bounds: CGRect) {
        self.body.setPosition(position);
        self.body.setBounds(bounds);
        self.image.setPosition(CGPoint::new(0.0, 0.0));
        self.image.setBounds(bounds);
    }

    /// Shows `picture`; `place`: at its own extent on the plane (in a transaction).
    fn show(&mut self, picture: &Picture, place: bool) {
        // SAFETY: a CGImage is valid layer contents.
        unsafe { self.image.setContents(Some(as_object(&*picture.image))) };
        if place {
            self.extent = picture.extent;
            let (position, bounds) = image_frame(&self.extent, self.plane.bounds());
            self.place_image(position, bounds);
        }
    }

    /// A picture that waited for the flight to end takes its place (in a transaction; the
    /// caller places the image after).
    fn take_pending(&mut self) {
        if let Some(picture) = self.pending.take() {
            // SAFETY: a CGImage is valid layer contents.
            unsafe { self.image.setContents(Some(as_object(&*picture.image))) };
            self.extent = picture.extent;
        }
    }

    /// Puts the ghost at `p` at once.
    pub fn set(&mut self, p: Plane) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for layer in [&self.plane, &self.body, &self.image] {
            for key in KEYS {
                layer.removeAnimationForKey(&NSString::from_str(key));
            }
        }
        self.take_pending();
        let look = Look::of(&p);
        self.plane.setPosition(look.position);
        self.plane.setBounds(look.bounds);
        self.plane.setTransform(look.transform);
        let (position, bounds) = image_frame(&self.extent, look.bounds);
        self.place_image(position, bounds);
        self.image.setCornerRadius(look.corner);
        if let Some(s) = &self.shadow {
            s.layout(look.bounds.size);
        }
        CATransaction::commit();
        self.target = p;
    }

    /// Shows `amount` of the window's shadow (0: the card's shadow alone, 1: the window's
    /// alone), at once.
    pub fn set_shadow_amount(&self, amount: f64) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if let Some(s) = &self.shadow {
            s.set_amount(amount);
        }
        let key = NSString::from_str("shadowOpacity");
        self.body.removeAnimationForKey(&key);
        self.body.setShadowOpacity((self.card_opacity * (1.0 - amount.clamp(0.0, 1.0))) as f32);
        CATransaction::commit();
    }

    /// Moves the shadow toward `amount` of the window's (0 the card's, 1 the window's) on
    /// `spring`, with the plane's motion when started with it.
    pub fn spring_shadow(&self, amount: f64, spring: Spring) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if let Some(s) = &self.shadow {
            s.spring_amount(amount, spring, self.flight);
        }
        let key = NSString::from_str("shadowOpacity");
        // SAFETY: presentationLayer returns a copy of the drawn state or nil.
        let from = unsafe { self.body.presentationLayer() }.map_or(self.body.shadowOpacity(), |l| l.shadowOpacity());
        let to = (self.card_opacity * (1.0 - amount.clamp(0.0, 1.0))) as f32;
        self.body.setShadowOpacity(to);
        if (from - to).abs() > 1e-3 {
            let anim = spring_anim(spring, "shadowOpacity", &NSNumber::new_f32(from), &NSNumber::new_f32(to), self.flight);
            self.body.addAnimation_forKey(&anim, Some(&key));
        } else {
            self.body.removeAnimationForKey(&key);
        }
        CATransaction::commit();
    }

    /// The handover of the window's shadow: from `from` to `to` (1 shown, 0 gone), linear,
    /// `begin` seconds from now for `duration` (`shadow_math::handover`).
    pub fn fade_shadow(&self, from: f64, to: f64, begin: f64, duration: f64) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if let Some(s) = &self.shadow {
            s.fade(from, to, begin, duration);
        }
        CATransaction::commit();
    }

    /// Where the ghost is drawn now (Core Animation's presentation of it; its target when
    /// nothing is drawn yet).
    pub fn drawn(&self) -> Plane {
        // SAFETY: presentationLayer returns a copy of the drawn state or nil.
        let Some(shown) = (unsafe { self.plane.presentationLayer() }) else { return self.target };
        let (position, bounds, t) = (shown.position(), shown.bounds(), shown.transform());
        Plane {
            left_mid: Point::new(position.x, position.y),
            size: Size::new(bounds.size.width, bounds.size.height),
            // A turn about the vertical axis: m11 = cos, m31 = sin.
            tilt: t.m31.atan2(t.m11),
            corner: self.target.corner,
        }
    }

    /// Moves the ghost to `p` on the spring, from wherever it is drawn now.
    pub fn spring_to(&mut self, p: Plane, spring: Spring) {
        // SAFETY: presentationLayer returns a copy of the drawn state or nil.
        let shown = unsafe { self.plane.presentationLayer() };
        // SAFETY: as above.
        let shown_image = unsafe { self.image.presentationLayer() };
        let from_position = shown.as_ref().map_or(self.plane.position(), |l| l.position());
        let from_bounds = shown.as_ref().map_or(self.plane.bounds(), |l| l.bounds());
        let from_transform = shown.as_ref().map_or(self.plane.transform(), |l| l.transform());
        // SAFETY: as above.
        let shown_body = unsafe { self.body.presentationLayer() };
        let from_corner = shown_image.as_ref().map_or(self.image.cornerRadius(), |l| l.cornerRadius());
        let from_image = shown_body.as_ref().map_or((self.body.position(), self.body.bounds()), |l| (l.position(), l.bounds()));
        let to = Look::of(&p);
        // Every key runs as long as the farthest edge needs to come to rest on the curve.
        let duration = spring_duration(spring, plane_travel(from_position, from_bounds, to.position, to.bounds));
        self.flight = duration;
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.take_pending();
        let to_image = image_frame(&self.extent, to.bounds);
        self.plane.setPosition(to.position);
        self.plane.setBounds(to.bounds);
        self.plane.setTransform(to.transform);
        self.place_image(to_image.0, to_image.1);
        self.image.setCornerRadius(to.corner);
        let add = |layer: &CALayer, key: &str, from: &AnyObject, to: &AnyObject| {
            let anim = spring_anim(spring, key, from, to, duration);
            layer.addAnimation_forKey(&anim, Some(&NSString::from_str(key)));
        };
        add(&self.plane, "position", &NSValue::new(from_position), &NSValue::new(to.position));
        add(&self.plane, "bounds", &NSValue::new(from_bounds), &NSValue::new(to.bounds));
        add(&self.plane, "transform", &transform_value(from_transform), &transform_value(to.transform));
        add(&self.body, "bounds", &NSValue::new(from_image.1), &NSValue::new(to_image.1));
        add(&self.image, "bounds", &NSValue::new(from_image.1), &NSValue::new(to_image.1));
        if from_image.0 != to_image.0 {
            add(&self.body, "position", &NSValue::new(from_image.0), &NSValue::new(to_image.0));
        }
        add(&self.image, "cornerRadius", &NSNumber::new_f64(from_corner), &NSNumber::new_f64(to.corner));
        if let Some(s) = &self.shadow {
            s.spring_layout(from_bounds.size, to.bounds.size, spring, duration);
        }
        CATransaction::commit();
        self.target = p;
    }

    /// Fades the whole snapshot out over `duration` seconds from now, where it is (the
    /// landing: whatever differs from the window under it melts instead of flipping). The
    /// caller removes it after.
    pub fn fade_out(&self, duration: f64) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let key = NSString::from_str("opacity");
        // SAFETY: presentationLayer returns a copy of the drawn state or nil.
        let from = unsafe { self.plane.presentationLayer() }.map_or(self.plane.opacity(), |l| l.opacity());
        self.plane.setOpacity(0.0);
        let anim = CABasicAnimation::animationWithKeyPath(Some(&key));
        // SAFETY: from/to are numbers, the property's type.
        unsafe {
            anim.setFromValue(Some(&NSNumber::new_f32(from)));
            anim.setToValue(Some(&NSNumber::new_f32(0.0)));
        }
        anim.setDuration(duration.max(1e-3));
        self.plane.addAnimation_forKey(&anim, Some(&key));
        CATransaction::commit();
    }

    /// Takes the ghost off the screen.
    pub fn remove(&self) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.depth.removeFromSuperlayer();
        CATransaction::commit();
    }
}

define_class!(
    // SAFETY: NSView has no subclassing requirements; OverlayView does not implement Drop.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[name = "SMOverlayView"]
    pub struct OverlayView;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for OverlayView {}

    impl OverlayView {
        // SAFETY: the signature matches NSView's.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl OverlayView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's designated initializer on a fresh instance.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// The overlay's panel: click-through, transparent, above every normal window.
fn make_panel(mtm: MainThreadMarker, rect: NSRect) -> Retained<NSPanel> {
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
    panel.setLevel(OVER_WINDOWS);
    panel.setIgnoresMouseEvents(true);
    // On the active Space only: it moves there when shown, and is renewed on a Space change;
    // hidden by Mission Control.
    panel.setCollectionBehavior(panels::motion_overlay());
    panel.setHidesOnDeactivate(false);
    // SAFETY: the panel is owned by the overlay, which closes it when it is replaced.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel
}

/// The overlay panel and its stack of ghosts.
pub struct Overlay {
    panel: Retained<NSPanel>,
    view: Retained<OverlayView>,
    root: Retained<CALayer>,
    /// The ghosts' perspective containers, flat, bottom to top.
    space: Retained<CALayer>,
    /// The eye in the overlay's coordinates, and its distance.
    eye: (Point, f64),
    /// The overlay's top-left corner, global coordinates.
    origin: Point,
    shown: bool,
    /// The window the overlay is ordered just under (at the normal window level), while a
    /// transition's snapshots pass under a window already in place ([`Self::show_under`]).
    under: Option<u32>,
    /// Above every window, floating ones and the raised strip included ([`Self::show_on_top`]):
    /// only while a window dragged over the strip shows its card.
    top: bool,
}

impl Overlay {
    /// Creates the overlay (hidden).
    pub fn new(mtm: MainThreadMarker) -> Self {
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0));
        let panel = make_panel(mtm, rect);
        // Flipped, so its root layer stays top-down (see the module notes).
        let view = OverlayView::new(mtm, rect);
        let root = CALayer::new();
        root.setGeometryFlipped(true);
        view.setLayer(Some(&root));
        view.setWantsLayer(true);
        let space = CALayer::new();
        root.addSublayer(&space);
        panel.setContentView(Some(&view));
        let eye = (Point::new(0.0, 0.0), 1.0);
        Self { panel, view, root, space, eye, origin: Point::new(0.0, 0.0), shown: false, under: None, top: false }
    }

    /// Covers the display `frame` (Cocoa coordinates; `primary_height` flips them) and sets
    /// the perspective: the eye at `eye` (global), `distance` points away.
    pub fn place(&mut self, frame: NSRect, primary_height: f64, eye: Point, distance: f64) {
        if self.panel.frame() != frame {
            self.panel.setFrame_display(frame, false);
        }
        // The frame the panel really has (AppKit may constrain a window's frame).
        let frame = self.panel.frame();
        self.origin = Point::new(frame.origin.x, primary_height - (frame.origin.y + frame.size.height));
        let size = frame.size;
        let local_eye = Point::new(eye.x - self.origin.x, eye.y - self.origin.y);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let all = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size.width, size.height));
        self.root.setFrame(all);
        self.space.setFrame(all);
        self.eye = (local_eye, distance);
        // SAFETY: reading the sublayers array; every one is a ghost's container.
        if let Some(depths) = unsafe { self.space.sublayers() } {
            for depth in depths.iter() {
                self.fit(&depth);
            }
        }
        CATransaction::commit();
    }

    /// Makes `depth` a ghost's perspective container: the whole overlay, the eye where
    /// [`Self::place`] put it.
    fn fit(&self, depth: &CALayer) {
        let all = self.space.bounds();
        let (eye, distance) = self.eye;
        depth.setBounds(all);
        depth.setAnchorPoint(CGPoint::new(eye.x / all.size.width.max(1.0), eye.y / all.size.height.max(1.0)));
        depth.setPosition(CGPoint::new(eye.x, eye.y));
        let mut p = CATransform3D::new_scale(1.0, 1.0, 1.0);
        p.m34 = -1.0 / distance.max(1.0);
        depth.setSublayerTransform(p);
    }

    /// A ghost's perspective container holding `plane`, on top of the stack, or at the bottom
    /// (`bottom`).
    fn stack(&self, plane: &CALayer, bottom: bool) -> Retained<CALayer> {
        let depth = CALayer::new();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.fit(&depth);
        depth.addSublayer(plane);
        if bottom {
            self.space.insertSublayer_atIndex(&depth, 0);
        } else {
            self.space.addSublayer(&depth);
        }
        CATransaction::commit();
        depth
    }

    /// The overlay's top-left corner (global): planes are relative to it.
    pub fn origin(&self) -> Point {
        self.origin
    }

    /// Shows the overlay (above every normal window, on the active Space).
    pub fn show(&mut self) {
        self.show_under(None);
    }

    /// Shows the overlay just under window `under` (another app's, at the normal level), so
    /// snapshots pass behind a window already in place, as Apple's do behind a window just
    /// opened or activated; `None`: above every normal window.
    pub fn show_under(&mut self, under: Option<u32>) {
        self.show_below(under.map(|id| (id, NSNormalWindowLevel)));
    }

    /// Shows the overlay just under window `below.0` at level `below.1` (the window's own:
    /// ordering is per level); `None`: above every normal window.
    pub fn show_below(&mut self, below: Option<(u32, isize)>) {
        if let Some((id, level)) = below {
            self.top = false;
            self.panel.setLevel(level);
            self.panel.orderWindow_relativeTo(NSWindowOrderingMode::Below, id as isize);
            if self.panel.isVisible() {
                self.shown = true;
                self.under = Some(id);
                return;
            }
            // The window is gone (closed, or off the Space shown): above every normal window.
            tracing::debug!(id, "motion overlay: cannot go under that window; above every normal window");
            self.under = Some(id);
            self.shown = false;
        }
        if self.under.take().is_some() || self.top {
            self.top = false;
            self.panel.setLevel(OVER_WINDOWS);
            self.shown = false;
        }
        self.order_front();
    }

    /// Shows the overlay above every window, floating ones and the raised strip included:
    /// for the card of a window dragged over the strip, which follows the pointer over the
    /// cards.
    pub fn show_on_top(&mut self) {
        if self.under.take().is_some() || !self.top {
            self.top = true;
            self.panel.setLevel(NSStatusWindowLevel);
            self.shown = false;
        }
        self.order_front();
    }

    fn order_front(&mut self) {
        if !self.shown {
            self.shown = true;
            self.panel.orderFrontRegardless();
            if !self.root.isGeometryFlipped() {
                tracing::warn!("the motion overlay's layers are not top-down; ghosts would be mirrored");
            }
        }
    }

    /// A hidden overlay moves its view to a fresh panel (never ordered in yet, so it goes to
    /// the Space shown when it is), keeping its frame; a shown one stays as it is.
    pub fn renew(&mut self, mtm: MainThreadMarker) {
        if self.shown {
            return;
        }
        let panel = make_panel(mtm, self.panel.frame());
        panel.setContentView(Some(&self.view));
        let old = std::mem::replace(&mut self.panel, panel);
        old.close();
        self.under = None;
        self.top = false;
    }

    /// Hides the overlay.
    pub fn hide(&mut self) {
        if self.shown {
            self.shown = false;
            self.panel.orderOut(None);
        }
    }

    /// The overlay's pixels per point.
    pub fn scale(&self) -> f64 {
        self.panel.backingScaleFactor()
    }

    /// A new ghost of window `window` showing `image` (or a plain card when there is none),
    /// placed at `at` with `shadow`, above the ghosts added before it.
    pub fn ghost(&self, window: u32, picture: Option<&Picture>, at: Plane, shadow: &GhostShadow, opacity: f32) -> Ghost {
        let plane = CALayer::new();
        plane.setAnchorPoint(CGPoint::new(0.0, 0.5));
        plane.setAllowsEdgeAntialiasing(true);
        plane.setOpacity(opacity);
        let mut layers = ShadowLayers::new(&plane);
        let window_size = CGSize::new(shadow.window_size.0, shadow.window_size.1);
        layers.set_shadow(&shadow.window, shadow.corner, self.scale(), bounds(&at).size, window_size);
        let body = CALayer::new();
        body.setAnchorPoint(CGPoint::new(0.0, 0.0));
        let (r, g, b, a) = shadow.card.color;
        body.setShadowColor(Some(&color(r, g, b, a)));
        body.setShadowRadius(shadow.card.radius);
        body.setShadowOffset(CGSize::new(shadow.card.offset.0, shadow.card.offset.1));
        let img = CALayer::new();
        img.setAnchorPoint(CGPoint::new(0.0, 0.0));
        img.setMasksToBounds(true);
        img.setAllowsEdgeAntialiasing(true);
        // SAFETY: framework constant.
        img.setContentsGravity(unsafe { kCAGravityResize });
        match picture {
            // SAFETY: a CGImage is valid layer contents.
            Some(p) => unsafe { img.setContents(Some(as_object(&*p.image))) },
            None => {
                img.setBackgroundColor(Some(&color(1.0, 1.0, 1.0, 0.22)));
                img.setBorderWidth(1.5);
                img.setBorderColor(Some(&color(1.0, 1.0, 1.0, 0.85)));
            }
        }
        body.addSublayer(&img);
        plane.addSublayer(&body);
        layers.attach_rim(&plane);
        let depth = self.stack(&plane, false);
        let extent = picture.map_or(Extent::NONE, |p| p.extent);
        let mut g = Ghost {
            window,
            depth,
            plane,
            body,
            image: img,
            shadow: Some(layers),
            card_opacity: shadow.card.opacity,
            extent,
            pending: None,
            target: at,
            flight: 0.0,
        };
        g.set(at);
        g.set_shadow_amount(shadow.amount);
        g
    }

    /// The park corner's cover for a switch (round 8L): `image` (the corner patch's picture,
    /// `scale` pixels a point) flat at `at`, with no shadow, under every ghost, so the
    /// incoming windows waiting in the corner (raised there by an activation) never show,
    /// while every snapshot passes over it. `over`: snapshots of the windows that stay shown
    /// into the corner, back to front, each window at its frame relative to the cover's
    /// top-left (points) and its child windows' parts beside it by the picture's extent
    /// (round 8K), drawn over the picture and clipped to the cover (round 8L5). Removed like a
    /// ghost.
    pub fn cover(&self, image: &CGImage, scale: f64, over: &[(Picture, Rect)], at: Plane) -> Ghost {
        let plane = CALayer::new();
        plane.setAnchorPoint(CGPoint::new(0.0, 0.5));
        let img = CALayer::new();
        img.setAnchorPoint(CGPoint::new(0.0, 0.0));
        img.setMasksToBounds(true);
        // SAFETY: framework constant.
        img.setContentsGravity(unsafe { kCAGravityResize });
        img.setContentsScale(scale);
        // SAFETY: a CGImage is valid layer contents.
        unsafe { img.setContents(Some(as_object(image))) };
        for (snapshot, frame) in over {
            // The window's part at its frame, the rest of the picture around it.
            let whole = snapshot.extent.around(Rect::new(frame.x, frame.y, frame.w.max(1.0), frame.h.max(1.0)));
            let window = CALayer::new();
            window.setAnchorPoint(CGPoint::new(0.0, 0.0));
            window.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(whole.w, whole.h)));
            window.setPosition(CGPoint::new(whole.x, whole.y));
            // SAFETY: framework constant.
            window.setContentsGravity(unsafe { kCAGravityResize });
            // SAFETY: a CGImage is valid layer contents.
            unsafe { window.setContents(Some(as_object(&*snapshot.image))) };
            img.addSublayer(&window);
        }
        let body = CALayer::new();
        body.setAnchorPoint(CGPoint::new(0.0, 0.0));
        body.addSublayer(&img);
        plane.addSublayer(&body);
        let depth = self.stack(&plane, true);
        let mut g = Ghost { window: 0, depth, plane, body, image: img, shadow: None, card_opacity: 0.0, extent: Extent::NONE, pending: None, target: at, flight: 0.0 };
        g.set(at);
        g
    }

    /// True when the root layer is top-down (what every plane assumes); checked after
    /// ordering in, since AppKit sets it from the view.
    pub fn top_down(&self) -> bool {
        self.root.isGeometryFlipped()
    }

    /// True while the overlay is ordered in.
    pub fn shown(&self) -> bool {
        self.shown
    }

    /// The overlay panel's window number (for a test's capture of what it draws).
    pub fn window_number(&self) -> isize {
        self.panel.windowNumber()
    }

    /// Makes sure what was just committed reaches the screen before the caller moves windows.
    pub fn flush(&self) {
        CATransaction::flush();
        self.panel.displayIfNeeded();
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.panel.orderOut(None);
    }
}
