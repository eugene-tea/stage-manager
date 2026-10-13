//! Finder's desktop icons, read through public Accessibility (WM thread): the corner patch
//! draws the desktop picture only, so it stays off while an icon lies under it
//! ([`crate::engine::corner_patch::item_reach`]).
//!
//! Finder's application element lists one `AXScrollArea` per display's desktop, holding an
//! `AXGroup` whose children are the icons (`AXImage`, the icon's image frame; the label
//! below it is not an element). Where Finder keeps icon places on disk is not public, and
//! neither is where desktop widgets sit (WindowManager draws them in one full-screen window
//! and lists no element for them), so widgets are only logged as possibly covered.

use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;

use super::ax::AxElement;
use super::windows::FINDER_BUNDLE_ID;
use crate::engine::geometry::Rect;

/// The Accessibility timeout for asking Finder (it must never hold the WM thread up long).
const FINDER_AX_TIMEOUT_SECONDS: f32 = 0.1;

/// Finder's desktop icons' image frames (global, top-left); `None` when Finder does not
/// answer (not running, no Accessibility, a timeout: the first failure ends the read, so a
/// hung Finder costs one timeout). Empty when the desktop shows none (no items, or
/// Finder's desktop is off).
pub fn finder_icons() -> Option<Vec<Rect>> {
    let pid = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(FINDER_BUNDLE_ID))
        .iter()
        .next()
        .map(|a| a.processIdentifier())?;
    AxElement::application(pid).bounded(FINDER_AX_TIMEOUT_SECONDS, |finder| {
        let mut out = Vec::new();
        for area in finder.children().ok()?.iter().filter(|e| e.role().is_ok_and(|r| r == "AXScrollArea")) {
            for group in area.children().ok()? {
                for icon in group.children().ok()? {
                    out.push(icon.frame().ok()?);
                }
            }
        }
        Some(out.into_iter().filter(|r| !r.is_degenerate()).collect())
    })
}
