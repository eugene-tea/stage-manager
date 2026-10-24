//! The desktop picture, read and drawn from its file: the platform half of
//! [`crate::engine::wallpaper`], for the corner patch ([`crate::ui::corner_patch`]) and
//! `stage-manager probe-patch`. No screen capture here: nothing here needs Screen Recording.
//! (Near a solar frame change, [`crate::ui::corner_settle`] may compare one still of the
//! desktop with the candidate frames' renders to pick the frame; what is drawn is still the
//! file's.)
//!
//! - **Read** ([`Reader::read`], main thread): the main screen (size, backing scale, color
//!   space), NSWorkspace's picture URL and options for it, macOS's wallpaper settings file
//!   (`~/Library/Application Support/com.apple.wallpaper/Store/Index.plist`: undocumented,
//!   read as data only when its modification time changes, never written; it alone names
//!   Tile and the style of a dynamic picture; a choice of one of Apple's pictures by its
//!   description, a `.madesktop` in `/System/Library/Desktop Pictures`, is put at the picture
//!   macOS downloaded for it at every read, so a download is seen), the picture's ImageIO facts and
//!   `apple_desktop` metadata (once per file version), the effective appearance, the clock
//!   and the time zone's place (`/usr/share/zoneinfo/zone.tab`, once). The engine decides.
//! - **Render** ([`render`], any thread): the chosen frame, decoded by ImageIO, drawn into
//!   a bitmap of the screen region at the display's backing scale and in the display's color
//!   space (so the window server shows its pixels as they are): the whole picture drawn
//!   where it lands on the screen, clipped to the region, with high-quality interpolation,
//!   which gives exactly the pixels of a draw over the whole screen.
//! - **Crop** ([`crop`]): the patch's whole pixels of a render, shared, not copied.
//! - **Thumb** ([`thumb`]): a render or a still as mean colours on a grid of points
//!   ([`crate::engine::solar_settle::Thumb`]), drawn in the display's colour space first, so
//!   a still in another colour space compares with the render.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use objc2::rc::Retained;
use objc2_app_kit::{
    NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSColor, NSScreen, NSWorkspace,
    NSWorkspaceDesktopImageAllowClippingKey, NSWorkspaceDesktopImageFillColorKey, NSWorkspaceDesktopImageScalingKey,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFData, CFDictionary, CFNumber, CFPropertyListCreateWithData, CFRetained, CFString, CFType, CFURL,
    CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColor, CGColorSpace, CGContext, CGImage,
    CGImageAlphaInfo, CGImageByteOrderInfo, CGInterpolationQuality,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSNumber, NSString, NSTimeZone};
use objc2_image_io::{
    CGImageDestination, CGImageMetadataTag, CGImageSource, kCGImagePropertyDPIHeight, kCGImagePropertyDPIWidth,
    kCGImagePropertyPixelHeight, kCGImagePropertyPixelWidth,
};

use crate::engine::geometry::{Rect, Size};
use crate::engine::wallpaper::{
    self as wp, Clock, Decision, ImageFacts, ImageSize, Inputs, Placement, Plist, StoreChoice,
};

/// macOS's wallpaper settings file, under the home folder.
const STORE: &str = "Library/Application Support/com.apple.wallpaper/Store/Index.plist";

/// The tz database's zone table (zone names to places).
const ZONE_TAB: &str = "/usr/share/zoneinfo/zone.tab";

/// One reading of what the main screen's desktop shows.
#[derive(Debug, Clone)]
pub struct Wall {
    /// The main screen's frame (global, top-left).
    pub screen: Rect,
    /// The picture's path as NSWorkspace reports it.
    pub path: Option<String>,
    /// What to draw, or why the picture cannot be drawn exactly.
    pub outcome: Result<(Decision, Job), String>,
    /// When the frame may change by the clock.
    pub recheck_at: Option<Instant>,
    /// The appearance read was dark.
    pub dark: bool,
    /// The picture's facts, when drawn: a Desktop's patch left drawn there tells by them
    /// whether its frame is still the one macOS shows ([`wp::still_drawn`]).
    pub image: Option<ImageFacts>,
}

/// Everything a render needs (sendable to a render thread).
#[derive(Debug, Clone)]
pub struct Job {
    /// The picture file.
    pub path: String,
    /// The file's version (modification time in nanoseconds, length).
    pub stamp: (u128, u64),
    /// The frame.
    pub index: usize,
    /// How it lands.
    pub placement: Placement,
    /// Its size.
    pub image: ImageSize,
    /// The screen's size, points.
    pub screen: Size,
    /// Pixels a point.
    pub scale: f64,
    /// The fill color around a picture that leaves gaps.
    pub fill: Option<Retained<CGColor>>,
    /// The fill color's description, for the key.
    pub fill_key: String,
    /// The display's color space.
    pub space: Option<Retained<CGColorSpace>>,
    /// The color space's name, for the key.
    pub space_key: String,
}

impl Job {
    /// What makes two renders the same picture.
    pub fn key(&self) -> String {
        format!("{}|frame {}", self.base_key(), self.index)
    }

    /// What makes two renders the same picture but for the frame: the candidate frames of
    /// one picture share it.
    pub fn base_key(&self) -> String {
        format!(
            "{}|{:?}|{}|{:?}|{}x{}@{}|{}|{}",
            self.path,
            self.stamp,
            self.placement.name(),
            (self.image.px_w, self.image.px_h, self.image.pt_w, self.image.pt_h),
            self.screen.w,
            self.screen.h,
            self.scale,
            self.fill_key,
            self.space_key
        )
    }

    /// The picture's file name for the log (never the folders: they name the user; see
    /// [`file_name`]).
    pub fn file_name(&self) -> String {
        file_name(&self.path)
    }
}

/// The last component of `path` as a log line may show it: Apple's pictures by name, the
/// user's own hashed unless `[log] titles` is on ([`crate::logbook::titles::picture_name`]).
pub fn file_name(path: &str) -> String {
    crate::logbook::titles::picture_name(path, super::logging::titles_shown())
}

/// Reads [`Wall`]s, keeping what only changes with a file (settings, picture facts, zone).
#[derive(Default)]
pub struct Reader {
    store: Option<(Option<SystemTime>, Option<Vec<StoreChoice>>)>,
    /// Apple's descriptions read (`/System`: they change only with macOS).
    descriptions: HashMap<String, Option<Plist>>,
    image: Option<(String, (u128, u64), Option<ImageFacts>)>,
    zone: Option<(String, Option<(f64, f64)>)>,
}

impl Reader {
    /// Reads what the main screen's desktop shows now (main thread).
    pub fn read(&mut self, mtm: MainThreadMarker) -> Wall {
        let dark = dark_appearance(mtm);
        let Some(screen) = NSScreen::screens(mtm).firstObject() else {
            return Wall { screen: Rect::default(), path: None, outcome: Err("no screen".into()), recheck_at: None, dark, image: None };
        };
        let f = screen.frame();
        let frame = Rect::new(0.0, 0.0, f.size.width, f.size.height);
        let workspace = NSWorkspace::sharedWorkspace();
        let path = workspace
            .desktopImageURLForScreen(&screen)
            .filter(|u| u.isFileURL())
            .and_then(|u| u.path())
            .map(|p| p.to_string());
        let options = workspace.desktopImageOptionsForScreen(&screen);
        // SAFETY: AppKit's own option-key statics.
        let (k_scale, k_clip, k_fill) =
            unsafe { (NSWorkspaceDesktopImageScalingKey, NSWorkspaceDesktopImageAllowClippingKey, NSWorkspaceDesktopImageFillColorKey) };
        let option = |k: &NSString| options.as_ref().and_then(|o| o.objectForKey(k));
        let number = |k: &NSString| option(k).and_then(|v| v.downcast::<NSNumber>().ok());
        let scaling = number(k_scale).map(|n| n.integerValue() as i64);
        let clipping = number(k_clip).map(|n| n.boolValue());
        let fill = option(k_fill).and_then(|v| v.downcast::<NSColor>().ok());
        let workspace_placement = Placement::from_workspace(scaling, clipping);
        let space = screen.colorSpace();
        let space_key = space.as_ref().and_then(|s| s.localizedName()).map_or_else(|| "unknown".into(), |n| n.to_string());
        let space = space.and_then(|s| s.CGColorSpace());
        let fill_key = fill.as_ref().map_or_else(|| "none".into(), |c| format!("{c:?}"));
        let fill = fill.map(|c| c.CGColor());
        let scale = screen.backingScaleFactor();

        let meta = path.as_deref().and_then(|p| std::fs::metadata(p).ok());
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        let stamp = meta.as_ref().map(stamp_of).unwrap_or_default();
        let store = self.store();
        let image = match path.as_deref() {
            Some(p) if !is_dir => self.image(p, stamp),
            _ => None,
        };
        let clock = self.clock();
        let inputs = Inputs {
            path: path.as_deref(),
            is_dir,
            workspace: workspace_placement,
            store: store.as_deref(),
            image: image.as_ref(),
            fill: fill.is_some(),
            dark,
            clock,
        };
        let decided = wp::decide(&inputs);
        let recheck_at = decided
            .as_ref()
            .ok()
            .and_then(|d| d.frame.recheck_in)
            .map(|s| Instant::now() + Duration::from_secs_f64(s.clamp(1.0, 86_400.0)));
        let facts = image.clone().filter(|_| decided.is_ok());
        let outcome = match (decided, path.clone(), image) {
            (Ok(d), Some(path), Some(image)) => Ok((
                d.clone(),
                Job {
                    path,
                    stamp,
                    index: d.frame.index,
                    placement: d.placement,
                    image: image.size,
                    screen: Size::new(frame.w, frame.h),
                    scale,
                    fill,
                    fill_key,
                    space,
                    space_key,
                },
            )),
            (Ok(_), _, _) => Err("the picture cannot be read".into()),
            (Err(why), _, _) => Err(why.into()),
        };
        Wall { screen: frame, path, outcome, recheck_at, dark, image: facts }
    }

    /// The settings file's choices, parsed again only when its modification time changed;
    /// Apple's pictures named by their descriptions put at their downloaded files
    /// ([`wp::resolve_system_pictures`]) at every read.
    fn store(&mut self) -> Option<Vec<StoreChoice>> {
        let home = std::env::var_os("HOME")?;
        let path = Path::new(&home).join(STORE);
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let fresh = self.store.as_ref().is_some_and(|(at, _)| *at == mtime);
        if !fresh {
            let choices = std::fs::read(&path).ok().and_then(|b| plist_from_bytes(&b)).map(|p| wp::store_choices(&p));
            if choices.is_none() {
                tracing::debug!("corner patch: macOS's wallpaper settings are unreadable; NSWorkspace alone");
            }
            self.store = Some((mtime, choices));
        }
        let mut choices = self.store.as_ref().and_then(|(_, c)| c.clone())?;
        let assets = Path::new(&home).join(wp::ASSET_FOLDER);
        let descriptions = &mut self.descriptions;
        let describe = |p: &str| descriptions.entry(p.to_owned()).or_insert_with(|| read_description(p)).clone();
        wp::resolve_system_pictures(&mut choices, &assets.to_string_lossy(), describe, |p| Path::new(p).is_file());
        Some(choices)
    }

    /// The picture's facts, read again only for a new file or version.
    fn image(&mut self, path: &str, stamp: (u128, u64)) -> Option<ImageFacts> {
        if let Some((p, s, facts)) = &self.image
            && p == path
            && *s == stamp
        {
            return facts.clone();
        }
        let facts = image_facts(path);
        self.image = Some((path.to_owned(), stamp, facts.clone()));
        facts
    }

    /// The clock, with the local time zone's place.
    pub fn clock(&mut self) -> Clock {
        let unix = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        let zone = NSTimeZone::localTimeZone().name().to_string();
        if self.zone.as_ref().is_none_or(|(z, _)| *z != zone) {
            let place = std::fs::read_to_string(ZONE_TAB).ok().and_then(|tab| wp::zone_place(&tab, &zone));
            self.zone = Some((zone, place));
        }
        Clock { unix, day_fraction: local_day_fraction(unix), place: self.zone.as_ref().and_then(|(_, p)| *p) }
    }
}

/// The largest description read (they are a few hundred bytes).
const DESCRIPTION_MAX: u64 = 64 * 1024;

/// One of Apple's picture descriptions (a `.madesktop` property list); `None` when it cannot
/// be read or is not one.
fn read_description(path: &str) -> Option<Plist> {
    std::fs::metadata(path).ok().filter(|m| m.is_file() && m.len() <= DESCRIPTION_MAX)?;
    let bytes = std::fs::read(path).ok().filter(|b| b.len() as u64 <= DESCRIPTION_MAX)?;
    plist_from_bytes(&bytes).filter(|p| matches!(p, Plist::Dict(_)))
}

fn stamp_of(m: &std::fs::Metadata) -> (u128, u64) {
    let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    (mtime, m.len())
}

/// The local time of `unix` as a fraction of the day.
fn local_day_fraction(unix: f64) -> f64 {
    let t = unix as libc::time_t;
    // SAFETY: `tm` is plain data written by localtime_r; both pointers are valid.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return (unix / 86_400.0).fract();
        }
        tm
    };
    let secs = f64::from(tm.tm_hour) * 3600.0 + f64::from(tm.tm_min) * 60.0 + f64::from(tm.tm_sec) + unix.fract();
    (secs / 86_400.0).clamp(0.0, 1.0)
}

/// The effective appearance is dark (main thread; follows System Settings, Auto included).
pub fn dark_appearance(mtm: MainThreadMarker) -> bool {
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: AppKit's own appearance-name statics.
    let (light, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    let names = NSArray::from_slice(&[light, dark]);
    app.effectiveAppearance().bestMatchFromAppearancesWithNames(&names).is_some_and(|n| &*n == dark)
}

/// Parses a property list (XML or binary); `data` values that are binary plists themselves
/// are parsed in place.
pub fn plist_from_bytes(bytes: &[u8]) -> Option<Plist> {
    let data = CFData::from_bytes(bytes);
    // SAFETY: valid data; null format and error pointers are allowed.
    let list = unsafe { CFPropertyListCreateWithData(None, Some(&data), 0, std::ptr::null_mut(), std::ptr::null_mut()) }?;
    Some(convert(&list, 0))
}

fn convert(v: &CFType, depth: usize) -> Plist {
    if depth > 24 {
        return Plist::Other;
    }
    if let Some(d) = v.downcast_ref::<CFDictionary>() {
        // SAFETY: the generic parameters are phantom; keys and values are type-checked below.
        let d = unsafe { &*(d as *const CFDictionary).cast::<CFDictionary<CFType, CFType>>() };
        let (keys, values) = d.to_vecs();
        return Plist::Dict(
            keys.iter()
                .zip(values.iter())
                .filter_map(|(k, v)| Some((k.downcast_ref::<CFString>()?.to_string(), convert(v, depth + 1))))
                .collect(),
        );
    }
    if let Some(a) = v.downcast_ref::<CFArray>() {
        // SAFETY: as above; elements are type-checked by `convert`.
        let a = unsafe { &*(a as *const CFArray).cast::<CFArray<CFType>>() };
        return Plist::Array(a.iter().map(|e| convert(&e, depth + 1)).collect());
    }
    if let Some(s) = v.downcast_ref::<CFString>() {
        return Plist::Str(s.to_string());
    }
    if let Some(b) = v.downcast_ref::<CFBoolean>() {
        return Plist::Num(if b.as_bool() { 1.0 } else { 0.0 });
    }
    if let Some(n) = v.downcast_ref::<CFNumber>() {
        return n.as_f64().map_or(Plist::Other, Plist::Num);
    }
    if let Some(d) = v.downcast_ref::<CFData>() {
        let bytes = d.to_vec();
        if bytes.starts_with(b"bplist") || bytes.starts_with(b"<?xml") {
            return plist_from_bytes(&bytes).unwrap_or(Plist::Data(bytes));
        }
        return Plist::Data(bytes);
    }
    Plist::Other
}

fn open(path: &str) -> Option<CFRetained<CGImageSource>> {
    let url = CFURL::from_file_path(path)?;
    // SAFETY: a file URL; no options.
    unsafe { CGImageSource::with_url(&url, None) }
}

/// The picture's size, frames and `apple_desktop` schedule (ImageIO; no pixels decoded).
pub fn image_facts(path: &str) -> Option<ImageFacts> {
    let src = open(path)?;
    // SAFETY: plain getters on a live image source.
    let (count, primary) = unsafe { (src.count(), src.primary_image_index()) };
    if count == 0 {
        return None;
    }
    // SAFETY: an index the source has; no options.
    let props = unsafe { src.properties_at_index(primary, None) }?;
    // SAFETY: image properties map CFString keys to CF values (type-checked below).
    let props = unsafe { &*(&*props as *const CFDictionary).cast::<CFDictionary<CFString, CFType>>() };
    let num = |k: &CFString| props.get(k).and_then(|v| v.downcast_ref::<CFNumber>().and_then(|n| n.as_f64()));
    // SAFETY: ImageIO's own property-key statics.
    let (kw, kh, kdw, kdh) = unsafe { (kCGImagePropertyPixelWidth, kCGImagePropertyPixelHeight, kCGImagePropertyDPIWidth, kCGImagePropertyDPIHeight) };
    let size = ImageSize::new(num(kw)?, num(kh)?, num(kdw), num(kdh));
    let (mut apr, mut h24, mut solar) = (None, None, None);
    for index in [primary, 0] {
        // SAFETY: an index the source has; no options.
        let Some(meta) = (unsafe { src.metadata_at_index(index, None) }) else { continue };
        // SAFETY: a live metadata object.
        let Some(tags) = (unsafe { meta.tags() }) else { continue };
        // SAFETY: the tag list is documented as an array of CGImageMetadataTag.
        let tags = unsafe { &*(&*tags as *const CFArray).cast::<CFArray<CGImageMetadataTag>>() };
        for tag in tags.iter() {
            // SAFETY: plain getters on a live tag.
            let (prefix, name, value) = unsafe { (tag.prefix(), tag.name(), tag.value()) };
            if prefix.is_none_or(|p| p.to_string() != "apple_desktop") {
                continue;
            }
            let Some(text) = value.as_ref().and_then(|v| v.downcast_ref::<CFString>()).map(|s| s.to_string()) else { continue };
            let parsed = wp::base64_decode(&text).and_then(|b| plist_from_bytes(&b));
            match name.map(|n| n.to_string()).as_deref() {
                Some("apr") => apr = parsed,
                Some("h24") => h24 = parsed,
                Some("solar") => solar = parsed,
                _ => {}
            }
        }
        if apr.is_some() || h24.is_some() || solar.is_some() {
            break;
        }
    }
    Some(ImageFacts { size, count, primary, schedule: wp::schedule(apr.as_ref(), h24.as_ref(), solar.as_ref()) })
}

/// Renders the screen-local `region` of what the desktop shows under `job`: the frame's
/// pixels on the whole-screen mapping, in the display's color space, at its backing scale.
pub fn render(job: &Job, region: Rect) -> Result<CFRetained<CGImage>, String> {
    let src = open(&job.path).ok_or("the picture cannot be opened")?;
    // SAFETY: an index the decision checked against the source's count; no options.
    let image = unsafe { src.image_at_index(job.index, None) }.ok_or("the frame cannot be decoded")?;
    let (px_w, px_h) = (CGImage::width(Some(&image)) as f64, CGImage::height(Some(&image)) as f64);
    // The frame's own size wins over the primary's (they match in Apple's pictures).
    let size = ImageSize { px_w, px_h, pt_w: job.image.pt_w * px_w / job.image.px_w, pt_h: job.image.pt_h * px_h / job.image.px_h };
    let (w, h) = ((region.w * job.scale).round(), (region.h * job.scale).round());
    if !(w >= 1.0 && h >= 1.0 && w <= 16_384.0 && h <= 16_384.0) {
        return Err(format!("a region of {w}x{h} pixels"));
    }
    let fallback = CGColorSpace::new_device_rgb();
    let space: &CGColorSpace = job.space.as_deref().or(fallback.as_deref()).ok_or("no color space")?;
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    // SAFETY: CG allocates the bitmap (null data, bytes per row computed); 8-bit BGRA.
    let ctx = unsafe { CGBitmapContextCreate(std::ptr::null_mut::<c_void>(), w as usize, h as usize, 8, 0, Some(space), info) }
        .ok_or("no bitmap context")?;
    CGContext::set_interpolation_quality(Some(&ctx), CGInterpolationQuality::High);
    let full = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h));
    let draws = wp::draws(job.placement, job.screen, size, region);
    match &job.fill {
        Some(fill) => {
            CGContext::set_fill_color_with_color(Some(&ctx), Some(fill));
            CGContext::fill_rect(Some(&ctx), full);
        }
        // A clear bitmap shows black in the opaque panel: only where the picture covers it all.
        None if wp::covered(&draws, Size::new(region.w, region.h)) => CGContext::clear_rect(Some(&ctx), full),
        None => return Err("the picture leaves a gap and macOS names no fill colour".into()),
    }
    let s = job.scale;
    for d in draws {
        // CG's origin is the bottom-left: flip the top-left destination.
        let dst = CGRect::new(CGPoint::new(d.dst.x * s, h - (d.dst.y + d.dst.h) * s), CGSize::new(d.dst.w * s, d.dst.h * s));
        CGContext::draw_image(Some(&ctx), dst, Some(&image));
    }
    CGBitmapContextCreateImage(Some(&ctx)).ok_or_else(|| "no image from the bitmap".into())
}

fn cg_rect(r: Rect) -> CGRect {
    CGRect::new(CGPoint::new(r.x, r.y), CGSize::new(r.w, r.h))
}

/// The pixels `px` (top-left) of a render.
pub fn crop(picture: &CGImage, px: Rect) -> Option<CFRetained<CGImage>> {
    CGImage::with_image_in_rect(Some(picture), cg_rect(px))
}

/// `image`, a picture of a block of `size` points, as mean colours on the comparison grid,
/// drawn in `space` (the display's) first; `None` when it cannot be read.
pub fn thumb(image: &CGImage, space: Option<&CGColorSpace>, size: Size) -> Option<crate::engine::solar_settle::Thumb> {
    let (bytes, w, h) = super::pixels::bgra(image, space)?;
    crate::engine::solar_settle::Thumb::from_bgra(&bytes, w, h, w * 4, size)
}

/// The name of `image`'s colour space, for the log.
pub fn space_name(image: &CGImage) -> String {
    CGImage::color_space(Some(image))
        .and_then(|s| CGColorSpace::name(Some(&s)))
        .map_or_else(|| "unnamed".into(), |n| n.to_string())
}

/// Writes `image` to a PNG at `path` (its color space embedded).
pub fn write_png(image: &CGImage, path: &Path) -> Result<(), String> {
    let url = CFURL::from_file_path(path).ok_or("a bad path")?;
    let png = CFString::from_static_str("public.png");
    // SAFETY: a file URL, a known type, one image, no options.
    let dest = unsafe { CGImageDestination::with_url(&url, &png, 1, None) }.ok_or("cannot create the PNG")?;
    // SAFETY: a live destination and image.
    let done = unsafe {
        dest.add_image(image, None);
        dest.finalize()
    };
    if done { Ok(()) } else { Err("the PNG was not written".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::pixels::tests::picture;

    #[test]
    fn a_thumb_reads_a_picture_on_the_grid_in_the_space_asked() {
        // 40 px for 20 points (2x): a 5 x 5 grid of 4-point cells.
        let red = picture(40, 40, [1.0, 0.0, 0.0, 1.0], false);
        let t = thumb(&red, None, Size::new(20.0, 20.0)).expect("a thumb");
        assert_eq!((t.cols, t.rows), (5, 5));
        assert!(t.rgb.iter().all(|c| c[0] > 250.0 && c[1] < 5.0 && c[2] < 5.0), "{:?}", t.rgb[0]);
        assert!(t.alpha.iter().all(|a| *a == 255));
        // Clear in its left half: those cells see-through.
        let half = thumb(&picture(40, 40, [1.0, 0.0, 0.0, 1.0], true), None, Size::new(20.0, 20.0)).expect("a thumb");
        assert_eq!((half.alpha[0], half.alpha[4]), (0, 255));
        // Drawn in another colour space, the same red reads differently: both sides of a
        // comparison are drawn in the display's.
        // SAFETY: a constant name CoreGraphics defines.
        let p3 = CGColorSpace::with_name(Some(unsafe { objc2_core_graphics::kCGColorSpaceDisplayP3 })).expect("P3");
        let in_p3 = thumb(&red, Some(&p3), Size::new(20.0, 20.0)).expect("a thumb");
        assert!(in_p3.rgb[0][0] < t.rgb[0][0] - 5.0, "{:?} vs {:?}", in_p3.rgb[0], t.rgb[0]);
    }
}
