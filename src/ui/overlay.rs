//! The motion overlay: a click-through, transparent panel over the whole main display, above
//! every window, where window snapshots ("ghosts") move while the real windows jump behind
//! them (`[animation]`). Main thread only.
//!
//! The overlay shows only while something moves; it stays on the active Space (it moves
//! there when shown, it never joins every Space). Its layers repeat the strip's perspective:
//! one container whose eye is the strip's eye, so a ghost turned like a card and placed on a
//! card's thumbnail looks exactly like that thumbnail, and a flat ghost at a window's frame
//! looks exactly like the window.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPanel, NSStatusWindowLevel, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGImage};
use objc2_foundation::{MainThreadMarker, NSNumber, NSPoint, NSRect, NSSize, NSString, NSValue};
use objc2_quartz_core::{
    CALayer, CAMediaTiming, CASpringAnimation, CATransaction, CATransform3D, NSValueCATransform3DAdditions,
    kCAGravityResize,
};

use crate::config::AnimationStyle;
use crate::engine::geometry::Point;
use crate::ui::motion_math::{Plane, Spring};

/// Every key path a ghost animates.
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

/// One moving snapshot: a shadowed plane holding a masked image.
pub struct Ghost {
    /// The window it shows.
    pub window: u32,
    plane: Retained<CALayer>,
    image: Retained<CALayer>,
    /// Where it is headed (or sits).
    pub target: Plane,
}

impl Ghost {
    /// Sets the image (a newer snapshot arrived).
    pub fn set_image(&self, image: &CGImage) {
        // SAFETY: a CGImage is valid layer contents.
        unsafe { self.image.setContents(Some(as_object(image))) };
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
        let look = Look::of(&p);
        self.plane.setPosition(look.position);
        self.plane.setBounds(look.bounds);
        self.plane.setTransform(look.transform);
        self.image.setBounds(look.bounds);
        self.image.setPosition(CGPoint::new(0.0, 0.0));
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
        let to = Look::of(&p);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.plane.setPosition(to.position);
        self.plane.setBounds(to.bounds);
        self.plane.setTransform(to.transform);
        self.image.setBounds(to.bounds);
        self.image.setCornerRadius(to.corner);
        let add = |layer: &CALayer, key: &str, from: &AnyObject, to: &AnyObject| {
            let anim = spring_anim(spring, key, from, to);
            layer.addAnimation_forKey(&anim, Some(&NSString::from_str(key)));
        };
        add(&self.plane, "position", &NSValue::new(from_position), &NSValue::new(to.position));
        add(&self.plane, "bounds", &NSValue::new(from_bounds), &NSValue::new(to.bounds));
        add(&self.plane, "transform", &transform_value(from_transform), &transform_value(to.transform));
        add(&self.image, "bounds", &NSValue::new(from_bounds), &NSValue::new(to.bounds));
        add(&self.image, "cornerRadius", &NSNumber::new_f64(from_corner), &NSNumber::new_f64(to.corner));
        CATransaction::commit();
        self.target = p;
    }

    /// Takes the ghost off the screen.
    pub fn remove(&self) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.plane.removeFromSuperlayer();
        CATransaction::commit();
    }
}

/// The overlay panel and its perspective container.
pub struct Overlay {
    panel: Retained<NSPanel>,
    _view: Retained<NSView>,
    root: Retained<CALayer>,
    space: Retained<CALayer>,
    /// The overlay's top-left corner, global coordinates.
    origin: Point,
    shown: bool,
}

impl Overlay {
    /// Creates the overlay (hidden).
    pub fn new(mtm: MainThreadMarker) -> Self {
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0));
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
        panel.setLevel(NSStatusWindowLevel);
        panel.setIgnoresMouseEvents(true);
        // On the active Space only: it moves there when shown.
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::MoveToActiveSpace
                | NSWindowCollectionBehavior::Transient
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        panel.setHidesOnDeactivate(false);
        // SAFETY: the panel is owned by the overlay for the life of the app.
        unsafe { panel.setReleasedWhenClosed(false) };
        let view = NSView::initWithFrame(NSView::alloc(mtm), rect);
        let root = CALayer::new();
        root.setGeometryFlipped(true);
        view.setLayer(Some(&root));
        view.setWantsLayer(true);
        let space = CALayer::new();
        root.addSublayer(&space);
        panel.setContentView(Some(&view));
        Self { panel, _view: view, root, space, origin: Point::new(0.0, 0.0), shown: false }
    }

    /// Covers the display `frame` (Cocoa coordinates; `primary_height` flips them) and sets
    /// the perspective: the eye at `eye` (global), `distance` points away.
    pub fn place(&mut self, frame: NSRect, primary_height: f64, eye: Point, distance: f64) {
        if self.panel.frame() != frame {
            self.panel.setFrame_display(frame, false);
        }
        self.origin = Point::new(frame.origin.x, primary_height - (frame.origin.y + frame.size.height));
        let size = frame.size;
        let local_eye = Point::new(eye.x - self.origin.x, eye.y - self.origin.y);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let all = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size.width, size.height));
        self.root.setFrame(all);
        self.space.setBounds(all);
        self.space.setAnchorPoint(CGPoint::new(
            local_eye.x / size.width.max(1.0),
            local_eye.y / size.height.max(1.0),
        ));
        self.space.setPosition(CGPoint::new(local_eye.x, local_eye.y));
        let mut p = CATransform3D::new_scale(1.0, 1.0, 1.0);
        p.m34 = -1.0 / distance.max(1.0);
        self.space.setSublayerTransform(p);
        CATransaction::commit();
    }

    /// The overlay's top-left corner (global): planes are relative to it.
    pub fn origin(&self) -> Point {
        self.origin
    }

    /// Shows the overlay (above every window, on the active Space).
    pub fn show(&mut self) {
        if !self.shown {
            self.shown = true;
            self.panel.orderFrontRegardless();
        }
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
    pub fn ghost(&self, window: u32, image: Option<&CGImage>, at: Plane, style: &AnimationStyle, opacity: f32) -> Ghost {
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
        match image {
            // SAFETY: a CGImage is valid layer contents.
            Some(i) => unsafe { img.setContents(Some(as_object(i))) },
            None => {
                img.setBackgroundColor(Some(&color(1.0, 1.0, 1.0, 0.22)));
                img.setBorderWidth(1.5);
                img.setBorderColor(Some(&color(1.0, 1.0, 1.0, 0.85)));
            }
        }
        plane.addSublayer(&img);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.space.addSublayer(&plane);
        CATransaction::commit();
        let mut g = Ghost { window, plane, image: img, target: at };
        g.set(at);
        g
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
