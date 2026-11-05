//! A snapshot's shadow on screen, drawn as macOS draws a window's (round ghost-shadow; the
//! model and its math: [`crate::ui::shadow_math`]).
//!
//! - **Why an image.** A Core Animation shadow is one Gaussian with no spread: it cannot
//!   take the window shadow's shape (a hairline rim and a soft drop). A shadow captured with
//!   the window would be the one it had when captured (key or not), baked into a picture
//!   that also shrinks to the card. So the model is drawn once into a nine-part image per
//!   shadow, corner radius and display scale ([`rendered`]), and [`ShadowLayers`] stretches it
//!   behind the snapshot's picture as the plane moves. The two shadows macOS draws (the key
//!   window's and an inactive one's) at a titled window's corner are drawn ahead, off the
//!   main thread ([`prepare`]); another corner radius met in a switch uses the nearest one
//!   drawn while its own is drawn for the next time.
//! - **The corner** is read from the snapshot itself ([`corner_of`]): how far in along the
//!   diagonal its picture turns opaque.
//! - **The layers.** On the plane, under the picture: a layer that fades the shadow at a
//!   handover ([`ShadowLayers::fade`]) holding the image's layer, whose opacity is the amount
//!   of the window's shadow shown (0 on a card, 1 at the window), moved on the switch's
//!   spring with the plane ([`ShadowLayers::spring_amount`]).
//! - **The inner rim** ([`crate::ui::shadow_math::InnerRim`]): over the picture, the same
//!   pair (a handover layer holding the rim's), so it comes and goes with the shadow. The rim
//!   is a nine-part image on macOS's corner curve ([`crate::ui::shadow_math::render_rim`]),
//!   laid out at the window's size and scaled to the plane as the picture is
//!   ([`crate::ui::shadow_math::rim_scale`]), so it keeps to the picture's edge and corner all
//!   the way; [`ShadowLayers::attach_rim`] puts it on top.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo};
use objc2_foundation::{NSNumber, NSString, NSValue};
use objc2_quartz_core::{CABasicAnimation, CACurrentMediaTime, CALayer, CAMediaTiming, CATransform3D, kCAFillModeBackwards};

use crate::platform::capture::Picture;
use crate::ui::motion_math::Spring;
pub use crate::ui::shadow_math::{DEFAULT_CORNER, NinePart, WindowShadow};
use crate::ui::shadow_math::{corner_from_diagonal, render, render_rim, rim_scale};

/// What an image is: the shadow under the picture, or the inner rim over it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Shadow,
    Rim,
}

/// What a rendered image is drawn for: its kind, the shadow's numbers, the corner (half
/// points) and the scale (hundredths), as bits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    kind: Kind,
    parts: [u64; 16],
    corner_half_pt: u32,
    scale_pct: u32,
}

impl Key {
    fn of(kind: Kind, shadow: &WindowShadow, corner: f64, scale: f64) -> Self {
        let (p, l, r) = (&shadow.parts, &shadow.line, &shadow.rim);
        let parts = [
            l.s_in, l.s_out, l.s_out45, l.aa, l.aa45, l.opacity, p[0].blur, p[0].opacity, p[0].offset_y, p[0].spread, p[1].blur, p[1].opacity, p[1].offset_y,
            p[1].spread, r.width, r.white,
        ]
        .map(f64::to_bits);
        Self { kind, parts, corner_half_pt: (corner.clamp(0.0, 40.0) * 2.0).round() as u32, scale_pct: (scale.clamp(0.5, 8.0) * 100.0).round() as u32 }
    }

    /// Draws the image.
    fn draw(&self, shadow: &WindowShadow) -> NinePart {
        match self.kind {
            Kind::Shadow => render(shadow, self.corner(), self.scale()),
            Kind::Rim => render_rim(shadow, self.corner(), self.scale()),
        }
    }

    fn corner(&self) -> f64 {
        f64::from(self.corner_half_pt) / 2.0
    }

    fn scale(&self) -> f64 {
        f64::from(self.scale_pct) / 100.0
    }
}

/// The images drawn, and those being drawn off the main thread.
#[derive(Default)]
struct Rendered {
    done: HashMap<Key, Arc<NinePart>>,
    drawing: Vec<Key>,
}

static RENDERED: Mutex<Option<Rendered>> = Mutex::new(None);

fn with_rendered<T>(f: impl FnOnce(&mut Rendered) -> T) -> T {
    let mut guard = RENDERED.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Rendered::default))
}

/// Draws `key`'s image off the main thread, unless it is drawn or being drawn.
fn draw_later(key: Key, shadow: WindowShadow) {
    let start = with_rendered(|r| {
        let start = !r.done.contains_key(&key) && !r.drawing.contains(&key);
        if start {
            r.drawing.push(key);
        }
        start
    });
    if !start {
        return;
    }
    let spawned = std::thread::Builder::new().name("snapshot-shadow".into()).spawn(move || {
        let img = Arc::new(key.draw(&shadow));
        with_rendered(|r| {
            r.drawing.retain(|k| *k != key);
            r.done.insert(key, img);
        });
    });
    if spawned.is_err() {
        with_rendered(|r| r.drawing.retain(|k| *k != key));
    }
}

/// Draws `shadows` (the key window's and an inactive one's) at a titled window's corner for a
/// display of `scale`, off the main thread, so the first switch finds them drawn.
pub fn prepare(shadows: &[WindowShadow], scale: f64) {
    for shadow in shadows {
        draw_later(Key::of(Kind::Shadow, shadow, DEFAULT_CORNER, scale), *shadow);
        if !shadow.rim.is_none() {
            draw_later(Key::of(Kind::Rim, shadow, DEFAULT_CORNER, scale), *shadow);
        }
    }
}

/// The image of `shadow` at `corner` and `scale`: drawn already, else the one drawn for the
/// nearest corner at that scale while this one is drawn off the main thread for the next
/// time, else drawn now (the first of its kind).
pub fn rendered(shadow: &WindowShadow, corner: f64, scale: f64) -> Arc<NinePart> {
    rendered_kind(Kind::Shadow, shadow, corner, scale)
}

/// The inner rim's image of `shadow` at `corner` and `scale`, as [`rendered`] the shadow's.
pub fn rendered_rim(shadow: &WindowShadow, corner: f64, scale: f64) -> Arc<NinePart> {
    rendered_kind(Kind::Rim, shadow, corner, scale)
}

fn rendered_kind(kind: Kind, shadow: &WindowShadow, corner: f64, scale: f64) -> Arc<NinePart> {
    let key = Key::of(kind, shadow, corner, scale);
    let near = with_rendered(|r| {
        if let Some(img) = r.done.get(&key) {
            return Ok(img.clone());
        }
        let nearest = r
            .done
            .iter()
            .filter(|(k, _)| k.kind == key.kind && k.parts == key.parts && k.scale_pct == key.scale_pct)
            .min_by_key(|(k, _)| k.corner_half_pt.abs_diff(key.corner_half_pt))
            .map(|(_, img)| img.clone());
        Err(nearest)
    });
    match near {
        Ok(img) => img,
        Err(Some(img)) => {
            draw_later(key, *shadow);
            img
        }
        Err(None) => {
            let img = Arc::new(key.draw(shadow));
            with_rendered(|r| {
                r.done.insert(key, img.clone());
            });
            img
        }
    }
}

/// A nine-part image as a CGImage (premultiplied black, top row first).
fn cg_image(img: &NinePart) -> Option<CFRetained<CGImage>> {
    let space = CGColorSpace::new_device_rgb()?;
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    let mut bytes = img.bytes.clone();
    // SAFETY: `bytes` holds h rows of w * 4 bytes and outlives the context; the image made
    // from it is a copy.
    let ctx = unsafe { CGBitmapContextCreate(bytes.as_mut_ptr().cast(), img.w, img.h, 8, img.w * 4, Some(&space), info) }?;
    CGBitmapContextCreateImage(Some(&ctx))
}

thread_local! {
    /// The CGImages made from rendered images (main thread, as the layers are).
    static IMAGES: std::cell::RefCell<Vec<(Arc<NinePart>, CFRetained<CGImage>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// `img` as a CGImage, made once.
fn image_for(img: &Arc<NinePart>) -> Option<CFRetained<CGImage>> {
    IMAGES.with(|cell| {
        let mut images = cell.borrow_mut();
        if let Some((_, i)) = images.iter().find(|(k, _)| Arc::ptr_eq(k, img)) {
            return Some(i.clone());
        }
        let made = cg_image(img)?;
        // A few shadows and corners at most; never more than a few dozen kept.
        if images.len() >= 24 {
            images.remove(0);
        }
        images.push((img.clone(), made.clone()));
        Some(made)
    })
}

/// The corner radius of the window in `picture` (points), its frame `size` points: the curve
/// its shadow follows, read from how much of the diagonal from its top-left corner the
/// picture leaves clear ([`corner_from_diagonal`]). `None` when it cannot be read.
pub fn corner_of(picture: &Picture, size: (f64, f64)) -> Option<f64> {
    let image = &picture.image;
    let (iw, ih) = (CGImage::width(Some(image)) as f64, CGImage::height(Some(image)) as f64);
    let e = picture.extent;
    let whole = (size.0 * (1.0 + e.left + e.right), size.1 * (1.0 + e.top + e.bottom));
    if whole.0 < 64.0 || whole.1 < 64.0 || iw < 1.0 {
        return None;
    }
    let scale = iw / whole.0;
    // A card's thumbnail standing in is too coarse to tell.
    if scale < 1.0 {
        return None;
    }
    // 16 pt square from the window's top-left corner.
    let (x, y) = ((e.left * size.0 * scale).round(), (e.top * size.1 * scale).round());
    let side = (16.0 * scale).round().min(iw - x).min(ih - y);
    if side < 4.0 {
        return None;
    }
    let crop = CGImage::with_image_in_rect(Some(image), CGRect::new(CGPoint::new(x, y), CGSize::new(side, side)))?;
    let (bytes, w, _) = crate::platform::pixels::bgra(&crop, None)?;
    let n = side as usize;
    let diagonal: Vec<u8> = (0..n).filter_map(|d| bytes.get((d * w + d) * 4 + 3).copied()).collect();
    corner_from_diagonal(&diagonal, scale)
}

fn as_object<T>(cf: &T) -> &AnyObject {
    // SAFETY: every CF type is a valid Objective-C object pointer.
    unsafe { &*(cf as *const T).cast::<AnyObject>() }
}

/// A snapshot's window shadow on screen: the nine-part image in a layer that follows the
/// plane, inside a layer that fades it at a handover, under everything else on the plane;
/// and the inner rim the same way over the picture. Main thread only.
pub struct ShadowLayers {
    /// Fades the shadow at a handover ([`Self::fade`]).
    handover: Retained<CALayer>,
    /// The image, the plane's size grown by the extent on every side; its opacity is the
    /// amount of the window's shadow shown.
    image: Retained<CALayer>,
    extent: f64,
    /// Fades the inner rim at a handover, as `handover` the shadow.
    rim_handover: Retained<CALayer>,
    /// The inner rim: a border at the window's size, scaled to the plane; its opacity is the
    /// amount, as the image's.
    rim: Retained<CALayer>,
    /// The window's size (points) the rim is laid out at.
    window: CGSize,
}

fn scale_transform((sx, sy): (f64, f64)) -> CATransform3D {
    CATransform3D { m11: sx, m12: 0.0, m13: 0.0, m14: 0.0, m21: 0.0, m22: sy, m23: 0.0, m24: 0.0, m31: 0.0, m32: 0.0, m33: 1.0, m34: 0.0, m41: 0.0, m42: 0.0, m43: 0.0, m44: 1.0 }
}

impl ShadowLayers {
    /// New layers at the bottom of `plane`, showing nothing until [`Self::set_shadow`]; the
    /// rim's go on top with [`Self::attach_rim`].
    pub fn new(plane: &CALayer) -> Self {
        let zero = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0));
        let handover = CALayer::new();
        handover.setAnchorPoint(CGPoint::new(0.0, 0.0));
        handover.setPosition(CGPoint::new(0.0, 0.0));
        handover.setBounds(zero);
        let image = CALayer::new();
        image.setAnchorPoint(CGPoint::new(0.0, 0.0));
        handover.addSublayer(&image);
        plane.insertSublayer_atIndex(&handover, 0);
        let rim_handover = CALayer::new();
        rim_handover.setAnchorPoint(CGPoint::new(0.0, 0.0));
        rim_handover.setPosition(CGPoint::new(0.0, 0.0));
        rim_handover.setBounds(zero);
        let rim = CALayer::new();
        rim.setAnchorPoint(CGPoint::new(0.0, 0.0));
        rim.setPosition(CGPoint::new(0.0, 0.0));
        rim.setAllowsEdgeAntialiasing(true);
        rim.setHidden(true);
        rim_handover.addSublayer(&rim);
        Self { handover, image, extent: 0.0, rim_handover, rim, window: CGSize::new(0.0, 0.0) }
    }

    /// Puts the inner rim on top of `plane`'s layers (call once the picture is on it).
    pub fn attach_rim(&self, plane: &CALayer) {
        plane.addSublayer(&self.rim_handover);
    }

    /// Shows `shadow` for a window of `window` size (points) with corners of radius `corner`
    /// on a display of `scale` pixels a point, around a plane of `size` (in the caller's
    /// transaction).
    pub fn set_shadow(&mut self, shadow: &WindowShadow, corner: f64, scale: f64, size: CGSize, window: CGSize) {
        self.window = window;
        let rim = &shadow.rim;
        let rim_image = (!rim.is_none()).then(|| rendered_rim(shadow, corner, scale)).and_then(|img| image_for(&img).map(|cg| (img, cg)));
        self.rim.setHidden(rim_image.is_none());
        if let Some((img, cg)) = rim_image {
            self.rim.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), window));
            // SAFETY: a CGImage is valid layer contents.
            unsafe { self.rim.setContents(Some(as_object(&*cg))) };
            self.rim.setContentsScale(img.scale);
            let [x, y, w, h] = img.slice;
            self.rim.setContentsCenter(CGRect::new(CGPoint::new(x, y), CGSize::new(w, h)));
        }
        let img = rendered(shadow, corner, scale);
        let Some(cg) = image_for(&img) else {
            tracing::debug!("snapshot shadow: no image; none shown");
            self.layout(size);
            return;
        };
        self.extent = img.extent;
        // SAFETY: a CGImage is valid layer contents.
        unsafe { self.image.setContents(Some(as_object(&*cg))) };
        self.image.setContentsScale(img.scale);
        let [x, y, w, h] = img.slice;
        self.image.setContentsCenter(CGRect::new(CGPoint::new(x, y), CGSize::new(w, h)));
        self.layout(size);
    }

    /// The image's bounds around a plane of `size`.
    fn bounds(&self, size: CGSize) -> CGRect {
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size.width + 2.0 * self.extent, size.height + 2.0 * self.extent))
    }

    /// The rim's scale on a plane of `size`.
    fn rim_transform(&self, size: CGSize) -> CATransform3D {
        scale_transform(rim_scale((size.width, size.height), (self.window.width, self.window.height)))
    }

    /// Lays the shadow around a plane of `size` at once (in the caller's transaction).
    pub fn layout(&self, size: CGSize) {
        self.image.removeAnimationForKey(&NSString::from_str("bounds"));
        self.image.setPosition(CGPoint::new(-self.extent, -self.extent));
        self.image.setBounds(self.bounds(size));
        self.rim.removeAnimationForKey(&NSString::from_str("transform"));
        self.rim.setTransform(self.rim_transform(size));
    }

    /// Moves the shadow from around a plane of `from` to around one of `to` on `spring`, as
    /// the plane's bounds move, `duration` seconds long as they do (in the caller's
    /// transaction).
    pub fn spring_layout(&self, from: CGSize, to: CGSize, spring: Spring, duration: f64) {
        self.image.setPosition(CGPoint::new(-self.extent, -self.extent));
        self.image.setBounds(self.bounds(to));
        let anim = crate::ui::overlay::spring_anim(spring, "bounds", &NSValue::new(self.bounds(from)), &NSValue::new(self.bounds(to)), duration);
        self.image.addAnimation_forKey(&anim, Some(&NSString::from_str("bounds")));
        self.rim.setTransform(self.rim_transform(to));
        let (a, b) = (crate::ui::overlay::transform_value(self.rim_transform(from)), crate::ui::overlay::transform_value(self.rim_transform(to)));
        let anim = crate::ui::overlay::spring_anim(spring, "transform", &a, &b, duration);
        self.rim.addAnimation_forKey(&anim, Some(&NSString::from_str("transform")));
    }

    /// The layers whose opacity is the amount shown: the image and the rim.
    fn amount_layers(&self) -> [&CALayer; 2] {
        [&self.image, &self.rim]
    }

    /// Shows `amount` of the window's shadow (0 on a card, 1 at the window) at once.
    pub fn set_amount(&self, amount: f64) {
        for layer in self.amount_layers() {
            layer.removeAnimationForKey(&NSString::from_str("opacity"));
            layer.setOpacity(amount.clamp(0.0, 1.0) as f32);
        }
    }

    /// The amount shown now (Core Animation's presentation of it).
    pub fn amount(&self) -> f64 {
        // SAFETY: presentationLayer returns a copy of the drawn state or nil.
        let shown = unsafe { self.image.presentationLayer() };
        f64::from(shown.map_or(self.image.opacity(), |l| l.opacity()))
    }

    /// Moves the amount from what is shown to `to` on `spring`, with the plane's motion
    /// (`duration` seconds, as long as its flight; in the caller's transaction).
    pub fn spring_amount(&self, to: f64, spring: Spring, duration: f64) {
        let from = self.amount();
        let to = to.clamp(0.0, 1.0);
        for layer in self.amount_layers() {
            layer.setOpacity(to as f32);
            if (from - to).abs() < 1e-3 {
                layer.removeAnimationForKey(&NSString::from_str("opacity"));
                continue;
            }
            let anim = crate::ui::overlay::spring_anim(spring, "opacity", &NSNumber::new_f64(from), &NSNumber::new_f64(to), duration);
            layer.addAnimation_forKey(&anim, Some(&NSString::from_str("opacity")));
        }
    }

    /// The handover: the shadow and the rim go from `from` to `to` (1 shown, 0 gone),
    /// linear, from `begin` seconds from now for `duration`, showing `from` until then (in
    /// the caller's transaction).
    pub fn fade(&self, from: f64, to: f64, begin: f64, duration: f64) {
        let key = NSString::from_str("opacity");
        let start = CACurrentMediaTime() + begin.max(0.0);
        for layer in [&self.handover, &self.rim_handover] {
            layer.removeAnimationForKey(&key);
            layer.setOpacity(to as f32);
            if duration <= 0.0 && begin <= 0.0 {
                continue;
            }
            let anim = CABasicAnimation::animationWithKeyPath(Some(&key));
            // SAFETY: from/to are numbers, the property's type.
            unsafe {
                anim.setFromValue(Some(&NSNumber::new_f64(from)));
                anim.setToValue(Some(&NSNumber::new_f64(to)));
            }
            anim.setBeginTime(start);
            anim.setDuration(duration.max(1e-3));
            // SAFETY: framework constant.
            anim.setFillMode(unsafe { kCAFillModeBackwards });
            layer.addAnimation_forKey(&anim, Some(&key));
        }
    }
}
