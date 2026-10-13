//! About Stage Manager: AppKit's standard About panel (`orderFrontStandardAboutPanelWithOptions`)
//! with the app's icon, name, version and build, "By eugene-tea" linked to the app's info
//! page, and a link to the source on GitHub. Nothing custom.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, Message};
use objc2_app_kit::{
    NSAboutPanelOptionApplicationName, NSAboutPanelOptionApplicationVersion, NSAboutPanelOptionCredits,
    NSAboutPanelOptionVersion, NSApplication, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSLinkAttributeName, NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSTextAlignment, NSWindow,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSBundle, NSDictionary, NSMutableAttributedString, NSString, NSURL,
};

use crate::updates::{PAGES_URL, REPO_URL};

/// The copyright line.
pub const COPYRIGHT: &str = "© 2026 eugene-tea";

/// The version shown (`CFBundleShortVersionString`, else the crate's) and the build
/// (`CFBundleVersion`, when the bundle has one that differs).
pub fn versions() -> (String, Option<String>) {
    let info = |key: &str| {
        NSBundle::mainBundle()
            .objectForInfoDictionaryKey(&NSString::from_str(key))
            .and_then(|v| v.downcast::<NSString>().ok())
            .map(|s| s.to_string())
    };
    let version = info("CFBundleShortVersionString").unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let build = info("CFBundleVersion").filter(|b| *b != version);
    (version, build)
}

fn run(text: &str, link: Option<&str>, color: &NSColor) -> Retained<NSAttributedString> {
    let style = NSMutableParagraphStyle::new();
    style.setAlignment(NSTextAlignment::Center);
    let font = NSFont::systemFontOfSize(11.0);
    // SAFETY: AppKit's own attribute names.
    let (k_font, k_color, k_style, k_link) =
        unsafe { (NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName, NSLinkAttributeName) };
    let mut keys: Vec<&NSString> = vec![k_font, k_color, k_style];
    let mut values: Vec<Retained<AnyObject>> = vec![font.into(), color.retain().into(), style.into()];
    if let Some(url) = link.and_then(|u| NSURL::URLWithString(&NSString::from_str(u))) {
        keys.push(k_link);
        values.push(url.into());
    }
    let refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
    let attrs = NSDictionary::from_slices(&keys, &refs);
    // SAFETY: valid string and attribute dictionary of AppKit attributes.
    unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(text), Some(&attrs)) }
}

/// The credits: "By eugene-tea" (the info page) and "Source code on GitHub".
pub fn credits() -> Retained<NSAttributedString> {
    let out = NSMutableAttributedString::new();
    let grey = NSColor::secondaryLabelColor();
    out.appendAttributedString(&run("By ", None, &grey));
    out.appendAttributedString(&run("eugene-tea", Some(PAGES_URL), &grey));
    out.appendAttributedString(&run("\n", None, &grey));
    out.appendAttributedString(&run("Source code on GitHub", Some(REPO_URL), &grey));
    out.into_super()
}

/// The panel's options.
pub fn options() -> Retained<NSDictionary<NSString, AnyObject>> {
    let (version, build) = versions();
    let credits = credits();
    let name = NSString::from_str("Stage Manager");
    let version = NSString::from_str(&version);
    // An empty build hides the parentheses.
    let build = NSString::from_str(&build.unwrap_or_default());
    let copyright = NSString::from_str(COPYRIGHT);
    // SAFETY: AppKit's own option keys; "Copyright" is the documented string key.
    let keys: [&NSString; 5] = unsafe {
        [
            NSAboutPanelOptionApplicationName,
            NSAboutPanelOptionApplicationVersion,
            NSAboutPanelOptionVersion,
            NSAboutPanelOptionCredits,
            objc2_foundation::ns_string!("Copyright"),
        ]
    };
    let values: [&AnyObject; 5] = [&name, &version, &build, &credits, &copyright];
    NSDictionary::from_slices(&keys, &values)
}

/// Shows the About panel in front.
pub fn show(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    app.activate();
    // SAFETY: the dictionary holds the documented keys with values of the documented types.
    unsafe { app.orderFrontStandardAboutPanelWithOptions(&options()) };
}

/// Shows the panel and returns its window (the window that was not there before), for the
/// preview's render.
pub fn show_and_find(mtm: MainThreadMarker) -> Option<Retained<NSWindow>> {
    let app = NSApplication::sharedApplication(mtm);
    let before: Vec<usize> = app.windows().iter().map(|w| Retained::as_ptr(&w) as usize).collect();
    // SAFETY: as in `show`.
    unsafe { app.orderFrontStandardAboutPanelWithOptions(&options()) };
    app.windows().iter().find(|w| !before.contains(&(Retained::as_ptr(w) as usize)))
}
