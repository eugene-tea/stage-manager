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
//! at a window's frame looks exactly like the window. Each ghost's plane turns about its
//! middle (its anchor), as Apple's snapshots do in flight; it is given as a plane turned about
//! its left edge and placed at the pose that looks the same ([`Pose`]). A flight goes through
//! poses along Apple's path ([`Path`], [`flight_samples`]) as keyframes, the picture, its glass
//! and the window's shadow through the same sizes. The ghosts stack in the order they were
//! added, the last on top: in one shared container Core Animation would sort turned ghosts by
//! depth, as it did the card's planes (owner, 10-27), and a second app's ghost would start
//! over the front window's and flip under it as they flatten.
//!
//! **Groups.** A switch's incoming snapshots go into one [`GhostGroup`], a flat layer holding
//! their containers in the same order, so the landing fades them as one picture: at fade `a`
//! the stack shows as it was drawn, at `a`, over the real windows. Each snapshot faded on its
//! own let a lower one show through an upper one as both went translucent: over the front
//! window, the window behind's picture at `a·(1−a)`, a quarter at mid-fade (11-02, Photos
//! under VS Code, seen at every switch to VS Code).
//!
//! **Glass.** A card lets the wallpaper through ([`crate::ui::glass_math`]): each ghost holds
//! a flat colour under its picture, the under of the card it flies to or from
//! ([`GlassLook`]), so a snapshot shrinking into a card lands on the card's look, the picture
//! fading to the card's opacity and the under coming in on the flight's spring, and one
//! growing from a card leaves it as the window, opaque ([`Ghost::spring_glass`]).
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
use objc2_foundation::{MainThreadMarker, NSArray, NSNumber, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSValue};
use objc2_quartz_core::{
    CABasicAnimation, CAKeyframeAnimation, CALayer, CAMediaTiming, CASpringAnimation, CATransaction, CATransform3D,
    NSValueCATransform3DAdditions, kCAGravityResize,
};

use crate::engine::geometry::{Point, Rect, Size};
use crate::platform::capture::Picture;
use crate::platform::capture_frame::Extent;
use crate::ui::motion_math::{Eye, Path, Plane, Pose, Spring, Toward, flight_samples, path_duration, travel};
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

fn bounds(p: &Pose) -> CGRect {
    CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(p.size.w.max(1.0), p.size.h.max(1.0)))
}

/// A spring animation of `key` from `from` to `to`, `duration` seconds long
/// ([`crate::ui::motion_math::spring_duration`]: on until it rests on the curve, never Core
/// Animation's `settlingDuration`, which cut it short by a point on a long travel).
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

/// What a ghost's layers show at a pose (model values; no animation). The plane layer's
/// anchor is its middle: the pose turns about it.
struct Look {
    position: CGPoint,
    bounds: CGRect,
    transform: CATransform3D,
    corner: f64,
}

impl Look {
    fn of(p: &Pose) -> Self {
        Self { position: CGPoint::new(p.middle.x, p.middle.y), bounds: bounds(p), transform: turn(p.tilt), corner: p.corner }
    }
}

/// A keyframe animation of `key` through `values` at `key_times` (fractions of `duration`),
/// straight between them (a flight's poses: [`flight_samples`]).
pub(crate) fn keyframes(key: &str, values: &[Retained<AnyObject>], key_times: &NSArray<NSNumber>, duration: f64) -> Retained<CAKeyframeAnimation> {
    let anim = CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str(key)));
    let array = NSArray::from_retained_slice(values);
    // SAFETY: the values are of the animated property's type.
    unsafe { anim.setValues(Some(&array)) };
    anim.setKeyTimes(Some(key_times));
    anim.setDuration(duration.max(1e-3));
    anim
}

/// An object as a plain one, for a keyframe's values.
pub(crate) fn as_value<T: objc2::Message>(v: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every Objective-C object is an AnyObject.
    unsafe { Retained::cast_unchecked(v) }
}

/// A card's glass behind one window's picture: the flat colour under it and the picture's
/// opacity there ([`crate::ui::glass_math`]). A card drawn opaque has none.
#[derive(Clone)]
pub struct GlassLook {
    /// The under, in the display's colour space.
    pub color: CFRetained<CGColor>,
    /// The under as numbers (0–1), to tell two looks apart.
    pub rgb: [f64; 3],
    /// The picture's opacity on the card.
    pub opacity: f32,
}

impl GlassLook {
    /// The same look (within a hundredth of a level).
    pub fn same(a: Option<&GlassLook>, b: Option<&GlassLook>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a.opacity == b.opacity && a.rgb.iter().zip(b.rgb).all(|(x, y)| (x - y).abs() < 1e-5),
            _ => false,
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
    /// The card's under, below the image on the body, over the window's part of the picture
    /// (the plane; never under child windows sticking out: the card's plane is the window's
    /// too) ([`GlassLook`]); clear while the ghost looks like its window.
    glass: Retained<CALayer>,
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
    /// How long its current flight runs (seconds; [`path_duration`] of its travel):
    /// its shadow's springs run as long.
    flight: f64,
    /// It stacks in a [`GhostGroup`] (it goes with the group).
    grouped: bool,
}

/// The name marking a [`GhostGroup`]'s layer among the ghosts' containers
/// ([`Overlay::place`] fits the containers inside it).
const GROUP: &str = "ghost-group";

/// Snapshots drawn as one picture: a flat layer over the whole overlay holding their
/// perspective containers, in the order they were added (see the module notes, "Groups").
/// Its opacity applies to what they draw together (group opacity), never to each.
pub struct GhostGroup {
    layer: Retained<CALayer>,
}

impl GhostGroup {
    /// Fades the group out over `duration` seconds from now, as one picture (the landing:
    /// whatever differs from the windows under it melts, and no snapshot shows through
    /// another). The caller removes it after.
    pub fn fade_out(&self, duration: f64) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let key = NSString::from_str("opacity");
        // SAFETY: presentationLayer returns a copy of the drawn state or nil.
        let from = unsafe { self.layer.presentationLayer() }.map_or(self.layer.opacity(), |l| l.opacity());
        self.layer.setOpacity(0.0);
        let anim = CABasicAnimation::animationWithKeyPath(Some(&key));
        // SAFETY: from/to are numbers, the property's type.
        unsafe {
            anim.setFromValue(Some(&NSNumber::new_f32(from)));
            anim.setToValue(Some(&NSNumber::new_f32(0.0)));
        }
        anim.setDuration(duration.max(1e-3));
        self.layer.addAnimation_forKey(&anim, Some(&key));
        CATransaction::commit();
    }

    /// Takes the group off the screen, with every snapshot in it.
    pub fn remove(&self) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.layer.removeFromSuperlayer();
        CATransaction::commit();
    }
}

/// How long a newer picture of the window takes to replace the one a flying snapshot shows
/// (seconds): a cross-fade, so a look that changed (a window made key while parked, captured
/// again) melts in instead of flipping in one frame.
pub const PICTURE_FADE: f64 = 0.15;

/// A plane's rectangle before the turn (its left edge's middle and its size).
fn unturned(p: &Plane) -> Rect {
    Rect::new(p.left_mid.x, p.left_mid.y - p.size.h / 2.0, p.size.w, p.size.h)
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
        // The glass covers the plane (the window), wherever the picture reaches.
        self.glass.setPosition(glass_position(position));
        self.glass.setBounds(self.plane.bounds());
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

    /// The perspective the ghost is seen through: its container's eye and distance
    /// ([`Overlay::fit`]), so it is the one in use even after the overlay moved.
    fn eye(&self) -> Eye {
        let at = self.depth.position();
        let m34 = self.depth.sublayerTransform().m34;
        let distance = if m34 < 0.0 { -1.0 / m34 } else { 1.0e6 };
        Eye { at: Point::new(at.x, at.y), distance }
    }

    /// Puts the ghost at `p` at once.
    pub fn set(&mut self, p: Plane) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for layer in [&self.plane, &self.body, &self.image, &self.glass] {
            for key in KEYS {
                layer.removeAnimationForKey(&NSString::from_str(key));
            }
        }
        self.take_pending();
        let look = Look::of(&p.pose(self.eye()));
        self.plane.setPosition(look.position);
        self.plane.setBounds(look.bounds);
        self.plane.setTransform(look.transform);
        let (position, bounds) = image_frame(&self.extent, look.bounds);
        self.place_image(position, bounds);
        self.image.setCornerRadius(look.corner);
        self.glass.setCornerRadius(look.corner);
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
    /// nothing is drawn yet), as a plane turned about its left edge.
    pub fn drawn(&self) -> Plane {
        let mut plane = self.drawn_pose().plane(self.eye());
        plane.corner = self.target.corner;
        plane
    }

    /// Where the ghost is drawn now, as its pose (turned about its middle).
    fn drawn_pose(&self) -> Pose {
        // SAFETY: presentationLayer returns a copy of the drawn state or nil.
        let Some(shown) = (unsafe { self.plane.presentationLayer() }) else { return self.target.pose(self.eye()) };
        let (position, bounds, t) = (shown.position(), shown.bounds(), shown.transform());
        // SAFETY: as above.
        let corner = unsafe { self.image.presentationLayer() }.map_or(self.image.cornerRadius(), |l| l.cornerRadius());
        Pose {
            middle: Point::new(position.x, position.y),
            size: Size::new(bounds.size.width, bounds.size.height),
            // A turn about the vertical axis: m11 = cos, m31 = sin.
            tilt: t.m31.atan2(t.m11),
            corner,
        }
    }

    /// Moves the ghost to `p` on the spring, from wherever it is drawn now, along Apple's path
    /// toward `toward` ([`Path`]: sideways and flat ahead of the spring at a window's end, up
    /// and down on it), turned about its middle. Every layer that follows the plane's size
    /// (the picture, its glass, the window's shadow) goes through the same poses.
    pub fn spring_to(&mut self, p: Plane, spring: Spring, toward: Toward) {
        let eye = self.eye();
        let from = self.drawn_pose();
        let to = p.pose(eye);
        // Every part runs as long as the farthest edge needs to come to rest on the path.
        let duration = path_duration(spring, travel(unturned(&from.plane(eye)), unturned(&p)), toward);
        self.flight = duration;
        let samples = flight_samples(&Path { from, to, toward }, spring, duration);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.take_pending();
        let key_times: Vec<Retained<NSNumber>> = samples.iter().map(|(f, _)| NSNumber::new_f64(*f)).collect();
        let key_times = NSArray::from_retained_slice(&key_times);
        let looks: Vec<Look> = samples.iter().map(|(_, pose)| Look::of(pose)).collect();
        let images: Vec<(CGPoint, CGRect)> = looks.iter().map(|l| image_frame(&self.extent, l.bounds)).collect();
        let point = |p: CGPoint| as_value(NSValue::new(p));
        let rect = |r: CGRect| as_value(NSValue::new(r));
        let add = |layer: &CALayer, key: &str, values: Vec<Retained<AnyObject>>| {
            layer.addAnimation_forKey(&keyframes(key, &values, &key_times, duration), Some(&NSString::from_str(key)));
        };
        add(&self.plane, "position", looks.iter().map(|l| point(l.position)).collect());
        add(&self.plane, "bounds", looks.iter().map(|l| rect(l.bounds)).collect());
        add(&self.plane, "transform", looks.iter().map(|l| as_value(transform_value(l.transform))).collect());
        add(&self.body, "bounds", images.iter().map(|i| rect(i.1)).collect());
        add(&self.image, "bounds", images.iter().map(|i| rect(i.1)).collect());
        add(&self.glass, "bounds", looks.iter().map(|l| rect(l.bounds)).collect());
        if images.iter().any(|i| i.0 != images[0].0) {
            add(&self.body, "position", images.iter().map(|i| point(i.0)).collect());
            add(&self.glass, "position", images.iter().map(|i| point(glass_position(i.0))).collect());
        } else {
            self.body.removeAnimationForKey(&NSString::from_str("position"));
            self.glass.removeAnimationForKey(&NSString::from_str("position"));
        }
        add(&self.image, "cornerRadius", looks.iter().map(|l| as_value(NSNumber::new_f64(l.corner))).collect());
        add(&self.glass, "cornerRadius", looks.iter().map(|l| as_value(NSNumber::new_f64(l.corner))).collect());
        let end = Look::of(&to);
        self.plane.setPosition(end.position);
        self.plane.setBounds(end.bounds);
        self.plane.setTransform(end.transform);
        let to_image = image_frame(&self.extent, end.bounds);
        self.place_image(to_image.0, to_image.1);
        self.image.setCornerRadius(end.corner);
        self.glass.setCornerRadius(end.corner);
        if let Some(s) = &self.shadow {
            let sizes: Vec<CGSize> = looks.iter().map(|l| l.bounds.size).collect();
            s.path_layout(&sizes, &key_times, duration);
        }
        CATransaction::commit();
        self.target = p;
    }

    /// Shows `amount` of the card's look `look` (0 the window: the picture opaque, no under;
    /// 1 the card: the under opaque, the picture at the card's opacity), at once. `None`: a
    /// card drawn opaque, the window's look whatever the amount.
    pub fn set_glass(&self, look: Option<&GlassLook>, amount: f64) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let (picture, under) = glass_values(look, amount);
        for (layer, value) in [(&self.image, picture), (&self.glass, under)] {
            layer.removeAnimationForKey(&NSString::from_str("opacity"));
            layer.setOpacity(value);
        }
        if let Some(l) = look {
            self.glass.setBackgroundColor(Some(&l.color));
        }
        CATransaction::commit();
    }

    /// Moves toward `amount` of the card's look `look` ([`Self::set_glass`]) on `spring`, from
    /// what is drawn now, as long as the flight started with it (call after
    /// [`Self::spring_to`]).
    pub fn spring_glass(&self, look: Option<&GlassLook>, amount: f64, spring: Spring) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if let Some(l) = look {
            self.glass.setBackgroundColor(Some(&l.color));
        }
        let (picture, under) = glass_values(look, amount);
        let key = NSString::from_str("opacity");
        for (layer, to) in [(&self.image, picture), (&self.glass, under)] {
            // SAFETY: presentationLayer returns a copy of the drawn state or nil.
            let from = unsafe { layer.presentationLayer() }.map_or(layer.opacity(), |l| l.opacity());
            layer.setOpacity(to);
            if (from - to).abs() > 1e-3 {
                let anim = spring_anim(spring, "opacity", &NSNumber::new_f32(from), &NSNumber::new_f32(to), self.flight);
                layer.addAnimation_forKey(&anim, Some(&key));
            } else {
                layer.removeAnimationForKey(&key);
            }
        }
        CATransaction::commit();
    }

    /// True when it stacks in a [`GhostGroup`].
    pub fn grouped(&self) -> bool {
        self.grouped
    }

    /// Takes the ghost off the screen.
    pub fn remove(&self) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.depth.removeFromSuperlayer();
        CATransaction::commit();
    }
}

/// The picture's and the under's opacities at `amount` of `look` (the window's with none).
fn glass_values(look: Option<&GlassLook>, amount: f64) -> (f32, f32) {
    match look {
        Some(l) => {
            let (picture, under) = crate::ui::glass_math::ghost_mix(amount, f64::from(l.opacity));
            (picture as f32, under as f32)
        }
        None => (1.0, 0.0),
    }
}

/// Where the glass sits on the body (anchored at its top-left): over the plane, the body
/// being at `body` on it.
fn glass_position(body: CGPoint) -> CGPoint {
    CGPoint::new(-body.x, -body.y)
}

/// A clear layer for a ghost's glass ([`Ghost::set_glass`]).
fn glass_layer() -> Retained<CALayer> {
    let glass = CALayer::new();
    glass.setAnchorPoint(CGPoint::new(0.0, 0.0));
    glass.setOpacity(0.0);
    glass.setAllowsEdgeAntialiasing(true);
    glass
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
        // SAFETY: reading the sublayers array; every one is a ghost's container or a group of them.
        if let Some(depths) = unsafe { self.space.sublayers() } {
            for depth in depths.iter() {
                if depth.name().is_some_and(|n| n.to_string() == GROUP) {
                    depth.setFrame(all);
                    // SAFETY: as above; a group holds ghosts' containers only.
                    for inner in unsafe { depth.sublayers() }.iter().flat_map(|a| a.iter()) {
                        self.fit(&inner);
                    }
                } else {
                    self.fit(&depth);
                }
            }
        }
        CATransaction::commit();
    }

    /// Makes `depth` a ghost's perspective container: the whole overlay, the eye where
    /// [`Self::place`] put it (a group covers the whole overlay too, so it is the same inside
    /// one).
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

    /// The perspective of the ghosts' containers ([`Self::fit`]).
    fn perspective(&self) -> Eye {
        Eye { at: self.eye.0, distance: self.eye.1.max(1.0) }
    }

    /// A ghost's perspective container holding `plane`, on top of the stack (of `group`'s,
    /// when given), or at the bottom (`bottom`).
    fn stack(&self, plane: &CALayer, bottom: bool, group: Option<&GhostGroup>) -> Retained<CALayer> {
        let depth = CALayer::new();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.fit(&depth);
        depth.addSublayer(plane);
        match group {
            Some(g) => g.layer.addSublayer(&depth),
            None if bottom => self.space.insertSublayer_atIndex(&depth, 0),
            None => self.space.addSublayer(&depth),
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

    /// A new group of ghosts on top of the stack ([`GhostGroup`]): the ghosts made in it
    /// stack inside it, above the ones added before it.
    pub fn group(&self) -> GhostGroup {
        let layer = CALayer::new();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        layer.setName(Some(&NSString::from_str(GROUP)));
        // Its opacity fades what its snapshots draw together (Core Animation's default on
        // macOS too; stated, since the landing depends on it).
        layer.setAllowsGroupOpacity(true);
        layer.setFrame(self.space.bounds());
        self.space.addSublayer(&layer);
        CATransaction::commit();
        GhostGroup { layer }
    }

    /// A new ghost of window `window` showing `image` (or a plain card when there is none),
    /// placed at `at` with `shadow`, above the ghosts added before it (in `group` when given:
    /// above the ones in it).
    pub fn ghost(&self, window: u32, picture: Option<&Picture>, at: Plane, shadow: &GhostShadow, opacity: f32, group: Option<&GhostGroup>) -> Ghost {
        let plane = CALayer::new();
        // Its middle: a snapshot turns about it ([`Pose`]).
        plane.setAnchorPoint(CGPoint::new(0.5, 0.5));
        plane.setAllowsEdgeAntialiasing(true);
        plane.setOpacity(opacity);
        let mut layers = ShadowLayers::new(&plane);
        let window_size = CGSize::new(shadow.window_size.0, shadow.window_size.1);
        layers.set_shadow(&shadow.window, shadow.corner, self.scale(), bounds(&at.pose(self.perspective())).size, window_size);
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
        let glass = glass_layer();
        body.addSublayer(&glass);
        body.addSublayer(&img);
        plane.addSublayer(&body);
        layers.attach_rim(&plane);
        let depth = self.stack(&plane, false, group);
        let extent = picture.map_or(Extent::NONE, |p| p.extent);
        let mut g = Ghost {
            window,
            depth,
            plane,
            body,
            image: img,
            glass,
            shadow: Some(layers),
            card_opacity: shadow.card.opacity,
            extent,
            pending: None,
            target: at,
            flight: 0.0,
            grouped: group.is_some(),
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
        plane.setAnchorPoint(CGPoint::new(0.5, 0.5));
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
        let depth = self.stack(&plane, true, None);
        let glass = glass_layer();
        let mut g = Ghost { window: 0, depth, plane, body, image: img, glass, shadow: None, card_opacity: 0.0, extent: Extent::NONE, pending: None, target: at, flight: 0.0, grouped: false };
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
