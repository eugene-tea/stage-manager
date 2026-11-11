//! The cards' app icons in the look the Dock shows them: since macOS 26 an app's icon has
//! appearance variants (System Settings → Appearance → Icon and widget style: Default, Dark,
//! Clear or Tinted, each light, dark or following the appearance), and Apple's cards show
//! the one the Dock shows (measured 11-09: Mail, Finder, Messages and Safari dark in Dark
//! Mode).
//!
//! The variant is IconServices' choice, from the global `AppleIconAppearanceTheme`
//! (`RegularAutomatic`, `RegularDark`, `TintedLight`, `ClearAutomatic`, …). Measured 11-09 in a
//! scratch process: drawing the image under another `NSAppearance`, or setting the app's own,
//! changes nothing; the theme given to the process (`-AppleIconAppearanceTheme RegularDark`)
//! does. Ours showed the light icons in Dark Mode because the strip kept each app's icon by
//! pid from the launch on, whatever came after. Now each is kept per look ([`IconCache`],
//! keyed by [`IconLook`]) and fetched afresh from the app's bundle, and the strip draws them
//! again when the appearance changes ([`super::strip::icons_changed`]).

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{CFPreferencesCopyAppValue, CFRetained, CFString, kCFPreferencesAnyApplication};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGInterpolationQuality, kCGColorSpaceSRGB,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

/// The icon and widget style's family: Default (`Regular`), Tinted or Clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconStyle {
    Regular,
    Tinted,
    Clear,
}

/// The look an app icon is drawn in: the style and whether its dark variant shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IconLook {
    pub style: IconStyle,
    pub dark: bool,
}

impl IconLook {
    /// The look of `theme` (`AppleIconAppearanceTheme`; absent: the default, Regular following
    /// the appearance), an `…Automatic` one dark when `system_dark`. An unknown family reads
    /// as Regular, an unknown variant as following the appearance.
    pub fn from_theme(theme: Option<&str>, system_dark: bool) -> Self {
        let theme = theme.unwrap_or("RegularAutomatic");
        let (style, variant) = [("Regular", IconStyle::Regular), ("Tinted", IconStyle::Tinted), ("Clear", IconStyle::Clear)]
            .into_iter()
            .find_map(|(name, style)| theme.strip_prefix(name).map(|rest| (style, rest)))
            .unwrap_or((IconStyle::Regular, theme));
        let dark = match variant {
            "Dark" => true,
            "Light" => false,
            _ => system_dark,
        };
        Self { style, dark }
    }
}

/// App icons by pid and look. An icon not found (an app still launching) is looked up again
/// next time; one found stays until [`Self::clear`].
#[derive(Debug)]
pub struct IconCache<T> {
    entries: HashMap<(i32, IconLook), Option<T>>,
}

impl<T> Default for IconCache<T> {
    fn default() -> Self {
        Self { entries: HashMap::new() }
    }
}

impl<T> IconCache<T> {
    /// App `pid`'s icon in `look`, from `fetch` when not cached yet or not found before.
    pub fn get_or_fetch(&mut self, pid: i32, look: IconLook, fetch: impl FnOnce() -> Option<T>) -> Option<&T> {
        let entry = self.entries.entry((pid, look)).or_insert(None);
        if entry.is_none() {
            *entry = fetch();
        }
        entry.as_ref()
    }

    /// Forgets every icon (a new size, or an appearance change that may have redrawn them).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// The look the Dock shows icons in now: the icon and widget style, its `…Automatic`
/// variants by the system's appearance.
pub fn current_look(mtm: MainThreadMarker) -> IconLook {
    let key = CFString::from_static_str("AppleIconAppearanceTheme");
    // SAFETY: a framework static.
    let domain = unsafe { kCFPreferencesAnyApplication };
    let theme = CFPreferencesCopyAppValue(&key, domain).and_then(|v| v.downcast_ref::<CFString>().map(|s| s.to_string()));
    IconLook::from_theme(theme.as_deref(), crate::platform::wallpaper::dark_appearance(mtm))
}

/// App `pid`'s icon as an image for a layer `points` wide on a display of `scale` (its backing
/// scale factor), fetched afresh from its bundle (a running app's `icon` is the one it was
/// first asked for), else the running app's.
///
/// Drawn at the layer's exact pixel size with high-quality scaling, as Apple's cards show it.
/// AppKit hands over the next larger stored size (256 px for a 38 pt icon at 2x), and a layer
/// shrinking that 3.4 times filters it bilinearly without mipmaps: the edges came out stepped
/// and the glyph grainy (seen 11-10 beside Apple's).
pub fn render(pid: i32, points: f64, scale: f64) -> Option<CFRetained<CGImage>> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    let icon = match app.bundleURL().and_then(|u| u.path()) {
        Some(path) => NSWorkspace::sharedWorkspace().iconForFile(&path),
        None => app.icon()?,
    };
    let side = (points * scale.max(1.0)).round().clamp(16.0, 1024.0);
    let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(side, side));
    // SAFETY: a valid proposed rect pointer and no context or hints.
    let image = unsafe { icon.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
    // SAFETY: the Retained<CGImage> owns one reference; re-wrap it as CFRetained.
    let ptr = NonNull::new(Retained::into_raw(image))?;
    // SAFETY: as above; the pointer came from `into_raw`, so its reference moves over.
    let source: CFRetained<CGImage> = unsafe { CFRetained::from_raw(ptr) };
    let px = side as usize;
    if CGImage::width(Some(&source)) == px && CGImage::height(Some(&source)) == px {
        return Some(source);
    }
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    // SAFETY: CG allocates the bitmap (null data, bytes per row computed); 8-bit BGRA.
    let ctx = unsafe { CGBitmapContextCreate(std::ptr::null_mut::<c_void>(), px, px, 8, 0, Some(&space), info) }?;
    CGContext::set_interpolation_quality(Some(&ctx), CGInterpolationQuality::High);
    let full = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(side, side));
    CGContext::draw_image(Some(&ctx), full, Some(&source));
    CGBitmapContextCreateImage(Some(&ctx))
}

/// The apps [`probe`] reads, where running (Apple's own, each with dark, tinted and clear
/// variants).
const PROBED: [&str; 4] = ["com.apple.mail", "com.apple.finder", "com.apple.Safari", "com.apple.MobileSMS"];

/// `stage-manager probe-icons [--for S]`: a live check for the main thread, nothing shown
/// and nothing changed. Every second for `S` seconds (default 90) one line: the look
/// ([`current_look`]) and the mean colour of each of [`PROBED`]'s icons fetched afresh
/// ([`render`]), so a switch of Light/Dark or of the icon and widget style made meanwhile
/// shows whether this process's fetches follow it; plus each appearance notification heard
/// (`AppleInterfaceThemeChangedNotification`, and any distributed one whose name speaks of
/// icons, themes, appearance, tint or colour). `-Default value` pairs after it are AppKit's
/// argument-domain defaults, e.g. `-AppleIconAppearanceTheme RegularDark` for a dark look to
/// compare with.
pub fn probe(args: &[String]) -> std::process::ExitCode {
    let mut seconds = 90;
    for pair in args.chunks(2) {
        match pair {
            [flag, s] if flag == "--for" => match s.parse::<u64>() {
                Ok(s) => seconds = s,
                Err(_) => return probe_usage(),
            },
            // A `-Default value` pair, which AppKit reads as an argument-domain default (a
            // look to compare: `-AppleIconAppearanceTheme RegularDark`).
            [key, _] if key.starts_with('-') && !key.starts_with("--") => {}
            _ => return probe_usage(),
        }
    }
    let Some(mtm) = MainThreadMarker::new() else { return probe_usage() };
    let _ = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let _themes = crate::platform::apps::observe_appearance(|| println!("heard AppleInterfaceThemeChangedNotification"));
    let _any = observe_named_like(&["icon", "theme", "appearance", "tint", "color", "colour"]);
    for _ in 0..seconds {
        let look = current_look(mtm);
        let mut line = format!("{:?} dark={}", look.style, look.dark);
        for bundle in PROBED {
            let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&objc2_foundation::NSString::from_str(bundle));
            let Some(app) = apps.firstObject() else { continue };
            let mean = render(app.processIdentifier(), 16.0, 2.0).and_then(|i| mean_rgb(&i));
            match mean {
                Some((r, g, b)) => line.push_str(&format!("  {bundle} {r:.2} {g:.2} {b:.2}")),
                None => line.push_str(&format!("  {bundle} none")),
            }
        }
        println!("{line}");
        let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(1.0);
        objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(&until);
    }
    std::process::ExitCode::SUCCESS
}

fn probe_usage() -> std::process::ExitCode {
    eprintln!("usage: stage-manager probe-icons [--for SECONDS] [-AppleIconAppearanceTheme THEME]");
    std::process::ExitCode::from(2)
}

/// Prints each distributed notification whose name holds one of `words` (any case): names
/// only, never their contents. The observer lasts while the returned token is held (the
/// probe's life; it exits without removing it).
fn observe_named_like(words: &'static [&'static str]) -> impl Sized {
    use block2::RcBlock;
    use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSOperationQueue};
    let center = NSDistributedNotificationCenter::defaultCenter();
    let block = RcBlock::new(move |note: NonNull<NSNotification>| {
        // SAFETY: the center passes a live notification for the block's call.
        let name = unsafe { note.as_ref() }.name().to_string();
        let lower = name.to_lowercase();
        if words.iter().any(|w| lower.contains(w)) {
            println!("heard {name}");
        }
    });
    // SAFETY: the block runs on the main queue only; it touches nothing but its own copy.
    let token = unsafe { center.addObserverForName_object_queue_usingBlock(None, None, Some(&NSOperationQueue::mainQueue()), &block) };
    (center, token)
}

/// The mean red, green and blue (0–1, alpha-weighted) of `image`.
fn mean_rgb(image: &CGImage) -> Option<(f64, f64, f64)> {
    let (bytes, _, _) = crate::platform::pixels::bgra(image, None)?;
    let (mut sum, mut alpha) = ([0.0f64; 3], 0.0f64);
    for p in bytes.as_chunks::<4>().0 {
        // Premultiplied: the colour sums weigh by alpha already.
        sum[0] += f64::from(p[2]);
        sum[1] += f64::from(p[1]);
        sum[2] += f64::from(p[0]);
        alpha += f64::from(p[3]);
    }
    (alpha > 0.0).then(|| (sum[0] / alpha, sum[1] / alpha, sum[2] / alpha))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHT: IconLook = IconLook { style: IconStyle::Regular, dark: false };
    const DARK: IconLook = IconLook { style: IconStyle::Regular, dark: true };

    #[test]
    fn the_theme_reads_as_the_look_the_dock_shows() {
        assert_eq!(IconLook::from_theme(None, false), LIGHT, "absent: Default, following the appearance");
        assert_eq!(IconLook::from_theme(None, true), DARK);
        assert_eq!(IconLook::from_theme(Some("RegularAutomatic"), true), DARK);
        assert_eq!(IconLook::from_theme(Some("RegularDark"), false), DARK, "Dark whatever the appearance");
        assert_eq!(IconLook::from_theme(Some("RegularLight"), true), LIGHT);
        assert_eq!(IconLook::from_theme(Some("TintedAutomatic"), false), IconLook { style: IconStyle::Tinted, dark: false });
        assert_eq!(IconLook::from_theme(Some("ClearDark"), false), IconLook { style: IconStyle::Clear, dark: true });
        assert_eq!(IconLook::from_theme(Some("Sparkly"), true), DARK, "unknown: Default, following the appearance");
    }

    #[test]
    fn the_cache_keeps_an_icon_per_app_and_look() {
        let mut cache = IconCache::default();
        let fetched = std::cell::RefCell::new(Vec::new());
        let get = |cache: &mut IconCache<String>, pid: i32, look: IconLook| {
            cache
                .get_or_fetch(pid, look, || {
                    fetched.borrow_mut().push((pid, look));
                    Some(format!("{pid} {}", if look.dark { "dark" } else { "light" }))
                })
                .cloned()
        };
        assert_eq!(get(&mut cache, 7, LIGHT).as_deref(), Some("7 light"));
        assert_eq!(get(&mut cache, 7, DARK).as_deref(), Some("7 dark"), "another look is another icon");
        assert_eq!(get(&mut cache, 7, LIGHT).as_deref(), Some("7 light"), "each look kept");
        assert_eq!(get(&mut cache, 8, LIGHT).as_deref(), Some("8 light"));
        let tinted = IconLook { style: IconStyle::Tinted, dark: false };
        assert_eq!(get(&mut cache, 7, tinted).as_deref(), Some("7 light"), "the style is part of the key");
        assert_eq!(*fetched.borrow(), vec![(7, LIGHT), (7, DARK), (8, LIGHT), (7, tinted)]);
        cache.clear();
        assert_eq!(get(&mut cache, 7, LIGHT).as_deref(), Some("7 light"));
        assert_eq!(fetched.borrow().len(), 5, "cleared: fetched again");
    }

    #[test]
    fn an_icon_not_found_is_looked_up_again() {
        let mut cache: IconCache<u8> = IconCache::default();
        assert_eq!(cache.get_or_fetch(3, LIGHT, || None), None);
        assert_eq!(cache.get_or_fetch(3, LIGHT, || Some(1)), Some(&1));
        assert_eq!(cache.get_or_fetch(3, LIGHT, || Some(2)), Some(&1), "found: kept");
    }
}
