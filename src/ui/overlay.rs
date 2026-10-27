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
    CALayer, CAMediaTiming, CASpringAnimation, CATransaction, CATransform3D, NSValueCATransform3DAdditions,
    kCAGravityResize,
};

use crate::config::AnimationStyle;
use crate::engine::geometry::{Point, Rect};
use crate::platform::capture::Picture;
use crate::platform::capture_frame::Extent;
use crate::ui::motion_math::{Plane, Spring};
use crate::ui::panels;

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

fn transform_value(t: CATransform3D) -> Retained<NSValue> {
    // SAFETY: QuartzCore's NSValue category; any transform is valid.
    unsafe { NSValue::valueWithCATransform3D(t) }
}

fn turn(tilt: f64) -> CATransform3D {
    CATransform3D::new_rotation(tilt, 0.0, 1.0, 0.0)
}

fn bounds(p: &Plane) -> CGRect {
    CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(p.size.w.max(1.0), p.size.h.max(1.0)))
}

/// A spring animation of `key` from `from` to `to`.
fn spring_anim(spring: Spring, key: &str, from: &AnyObject, to: &AnyObject) -> Retained<CASpringAnimation> {
    let anim = CASpringAnimation::animationWithKeyPath(Some(&NSString::from_str(key)));
    anim.setMass(spring.mass);
    anim.setStiffness(spring.stiffness);
    anim.setDamping(spring.damping);
    anim.setDuration(anim.settlingDuration());
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

/// One moving snapshot: a shadowed plane (the window's rectangle) holding a masked image
/// (the window's picture, reaching past the plane where child windows stick out).
pub struct Ghost {
    /// The window it shows.
    pub window: u32,
    /// Its perspective container, in the overlay's stack.
    depth: Retained<CALayer>,
    plane: Retained<CALayer>,
    image: Retained<CALayer>,
    /// Where the window is in the picture.
    extent: Extent,
    /// A picture whose window sits elsewhere in it, arrived mid-flight: shown at the next
    /// placement, so the flying image keeps its animation.
    pending: Option<Picture>,
    /// Where it is headed (or sits).
    pub target: Plane,
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
    pub fn set_image(&mut self, picture: &Picture) {
        let b = self.plane.bounds();
        let same = picture.extent.same_as(&self.extent, b.size.width, b.size.height);
        if !same && self.flying() {
            self.pending = Some(picture.clone());
            return;
        }
        self.pending = None;
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.show(picture, !same);
        CATransaction::commit();
    }

    /// True while a spring moves the ghost's image.
    fn flying(&self) -> bool {
        self.image.animationKeys().is_some_and(|keys| keys.count() > 0)
    }

    /// Shows `picture`; `place`: at its own extent on the plane (in a transaction).
    fn show(&mut self, picture: &Picture, place: bool) {
        // SAFETY: a CGImage is valid layer contents.
        unsafe { self.image.setContents(Some(as_object(&*picture.image))) };
        if place {
            self.extent = picture.extent;
            let (position, bounds) = image_frame(&self.extent, self.plane.bounds());
            self.image.setPosition(position);
            self.image.setBounds(bounds);
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
        for layer in [&self.plane, &self.image] {
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
        self.image.setBounds(bounds);
        self.image.setPosition(position);
        self.image.setCornerRadius(look.corner);
        CATransaction::commit();
        self.target = p;
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
        let from_corner = shown_image.as_ref().map_or(self.image.cornerRadius(), |l| l.cornerRadius());
        let from_image = shown_image.as_ref().map_or((self.image.position(), self.image.bounds()), |l| (l.position(), l.bounds()));
        let to = Look::of(&p);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.take_pending();
        let to_image = image_frame(&self.extent, to.bounds);
        self.plane.setPosition(to.position);
        self.plane.setBounds(to.bounds);
        self.plane.setTransform(to.transform);
        self.image.setPosition(to_image.0);
        self.image.setBounds(to_image.1);
        self.image.setCornerRadius(to.corner);
        let add = |layer: &CALayer, key: &str, from: &AnyObject, to: &AnyObject| {
            let anim = spring_anim(spring, key, from, to);
            layer.addAnimation_forKey(&anim, Some(&NSString::from_str(key)));
        };
        add(&self.plane, "position", &NSValue::new(from_position), &NSValue::new(to.position));
        add(&self.plane, "bounds", &NSValue::new(from_bounds), &NSValue::new(to.bounds));
        add(&self.plane, "transform", &transform_value(from_transform), &transform_value(to.transform));
        add(&self.image, "bounds", &NSValue::new(from_image.1), &NSValue::new(to_image.1));
        if from_image.0 != to_image.0 {
            add(&self.image, "position", &NSValue::new(from_image.0), &NSValue::new(to_image.0));
        }
        add(&self.image, "cornerRadius", &NSNumber::new_f64(from_corner), &NSNumber::new_f64(to.corner));
        CATransaction::commit();
        self.target = p;
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

    /// A new ghost of window `window` showing `image` (or a plain card when there is none),
    /// placed at `at`, above the ghosts added before it.
    pub fn ghost(&self, window: u32, picture: Option<&Picture>, at: Plane, style: &AnimationStyle, opacity: f32) -> Ghost {
        let plane = CALayer::new();
        plane.setAnchorPoint(CGPoint::new(0.0, 0.5));
        plane.setShadowColor(Some(&color(0.0, 0.0, 0.0, 1.0)));
        plane.setShadowOpacity(style.shadow_opacity as f32);
        plane.setShadowRadius(style.shadow_radius);
        plane.setShadowOffset(CGSize::new(0.0, style.shadow_offset_y));
        plane.setAllowsEdgeAntialiasing(true);
        plane.setOpacity(opacity);
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
        plane.addSublayer(&img);
        let depth = self.stack(&plane, false);
        let extent = picture.map_or(Extent::NONE, |p| p.extent);
        let mut g = Ghost { window, depth, plane, image: img, extent, pending: None, target: at };
        g.set(at);
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
        plane.addSublayer(&img);
        let depth = self.stack(&plane, true);
        let mut g = Ghost { window: 0, depth, plane, image: img, extent: Extent::NONE, pending: None, target: at };
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
