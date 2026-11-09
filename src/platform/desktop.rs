//! The desktop: displays (from `NSScreen`, converted to top-left coordinates, every screen
//! with its identity and park corner) and wallpaper clicks (the global mouse-down monitor in
//! [`super::input`] plus a window-list hit test).
//!
//! A click counts as a wallpaper click when no window of layer 0 or above contains the
//! point: the desktop picture and Finder's desktop icons are desktop elements, which the
//! window list leaves out. A click on the Dock, the menu bar, our strip or any app window
//! hits a window and is ignored. macOS's own click-through overlays do not count
//! ([`click_through_overlay`]).

use objc2::runtime::AnyObject;
use objc2_app_kit::NSScreen;
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayModelNumber, CGDisplaySerialNumber, CGDisplayVendorNumber, CGGetActiveDisplayList,
    CGMainDisplayID,
};
use objc2_foundation::{MainThreadMarker, NSNumber, NSRect, NSString};

use super::windows::CgWindow;
use crate::engine::displays::{ScreenKey, number_duplicates};
use crate::engine::geometry::{Point, Rect};
use crate::engine::model::{Display, DisplayId};
use crate::engine::park::{CornerChoice, choose_corner};
use crate::engine::sliver::DockBar;

/// Converts an AppKit rectangle (bottom-left origin of the primary screen, `y` up) into
/// global top-left coordinates, given the primary screen's height.
pub fn to_top_left(r: Rect, primary_height: f64) -> Rect {
    Rect::new(r.x, primary_height - (r.y + r.h), r.w, r.h)
}

/// Converts an AppKit point into global top-left coordinates.
pub fn point_to_top_left(p: Point, primary_height: f64) -> Point {
    Point::new(p.x, primary_height - p.y)
}

fn rect(r: NSRect) -> Rect {
    Rect::new(r.origin.x, r.origin.y, r.size.width, r.size.height)
}

/// The main display (the one with the menu bar): full and visible frames, top-left
/// coordinates. `None` when AppKit reports no screen.
pub fn main_display(mtm: MainThreadMarker) -> Option<Display> {
    displays(mtm).into_iter().next()
}

/// Every display, the main one (the menu bar's) first: full and visible frames in top-left
/// coordinates, the `CGDirectDisplayID`, the identity ([`screen_key`]) and the park corner
/// chosen against the other displays ([`choose_corner`]). Empty when AppKit reports no screen.
pub fn displays(mtm: MainThreadMarker) -> Vec<Display> {
    let screens = NSScreen::screens(mtm);
    let Some(primary) = screens.firstObject() else { return Vec::new() };
    let primary_height = primary.frame().size.height;
    let raw: Vec<(u32, Rect, Rect)> = screens
        .iter()
        .map(|s| {
            let frame = to_top_left(rect(s.frame()), primary_height);
            let visible = to_top_left(rect(s.visibleFrame()), primary_height);
            (screen_number(&s).unwrap_or_else(|| CGMainDisplayID()), frame, visible)
        })
        .collect();
    let mut keys: Vec<ScreenKey> = raw.iter().map(|(id, _, _)| screen_key(*id)).collect();
    number_duplicates(&mut keys);
    raw.iter()
        .zip(keys)
        .map(|(&(id, frame, visible), key)| {
            let others: Vec<Rect> = raw.iter().filter(|(o, _, _)| *o != id).map(|(_, f, _)| *f).collect();
            Display { id: DisplayId(id), key, frame, visible, corner: choose_corner(frame, visible, &others).corner }
        })
        .collect()
}

/// The screen with `CGDirectDisplayID` `id`, when AppKit reports it (round displays-2).
pub fn screen_with_id(mtm: MainThreadMarker, id: u32) -> Option<objc2::rc::Retained<NSScreen>> {
    NSScreen::screens(mtm).iter().find(|s| screen_number(s) == Some(id))
}

/// A screen's `CGDirectDisplayID` (`NSScreenNumber` in its device description).
pub(crate) fn screen_number(screen: &NSScreen) -> Option<u32> {
    let description = screen.deviceDescription();
    let value: objc2::rc::Retained<AnyObject> = description.objectForKey(&NSString::from_str("NSScreenNumber"))?;
    let number = value.downcast::<NSNumber>().ok()?;
    Some(number.unsignedIntValue())
}

/// A display's identity across reconnects and restarts: vendor, model and serial number
/// from CoreGraphics ([`ScreenKey`]; the serial is 0 for many displays, and identical ones
/// are told apart by their order, [`number_duplicates`]).
pub fn screen_key(id: u32) -> ScreenKey {
    ScreenKey::new(CGDisplayVendorNumber(id), CGDisplayModelNumber(id), CGDisplaySerialNumber(id))
}

/// The park corner for the main display (`frame`, `visible`) among the other active
/// displays ([`choose_corner`]: no display beyond it, else the least overlap).
pub fn park_corner(frame: Rect, visible: Rect) -> CornerChoice {
    let others: Vec<Rect> = cg_display_bounds().into_iter().skip(1).collect();
    choose_corner(frame, visible, &others)
}

/// The primary screen's height (for converting AppKit coordinates); 0 without a screen.
pub fn primary_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm).firstObject().map_or(0.0, |s| s.frame().size.height)
}

/// Every active display's bounds from CoreGraphics (global top-left coordinates), main first.
pub fn cg_display_bounds() -> Vec<Rect> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    // SAFETY: `ids` holds 16 entries and both pointers are valid for the call.
    let err = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    let main = CGMainDisplayID();
    let mut out = vec![bounds_of(main)];
    if err.0 == 0 {
        for &id in ids.iter().take(count as usize) {
            if id != main {
                out.push(bounds_of(id));
            }
        }
    }
    out
}

fn bounds_of(id: u32) -> Rect {
    let r = CGDisplayBounds(id);
    Rect::new(r.origin.x, r.origin.y, r.size.width, r.size.height)
}

/// True when a click at `p` (top-left coordinates) lands on no window: the window list is
/// front to back; windows below layer 0 are desktop elements and are not in it. Fully
/// transparent windows do not count, nor the Dock's window away from its bar (`dock`): it
/// spans its whole display, clear and click-through there ([`DockBar`]; the review's
/// switching 3: no click on the Dock's screen was a wallpaper click).
pub fn is_wallpaper_click(list: &[CgWindow], p: Point, dock: &DockBar) -> bool {
    !list.iter().any(|w| {
        w.onscreen
            && w.layer >= 0
            && w.alpha > 0.0
            && w.bounds.contains(p)
            && !dock.seen_through(w.pid, w.layer, w.bounds, p)
            && !(w.layer > 0 && click_through_overlay(w.layer, &w.owner, super::apps::bundle_of(w.pid).as_deref()))
    })
}

/// The frames of the real windows in `list` whose resize band a press may be in
/// ([`crate::engine::desktop_click::edge_zone`]): every app's window on screen at a normal or
/// floating level, below the Dock's, any stage's; not the Dock, the menu bar, macOS's
/// overlays or a transparent window. The caller leaves our own out.
pub fn resizable_frames(list: &[CgWindow]) -> Vec<Rect> {
    list.iter()
        .filter(|w| w.onscreen && w.alpha > 0.0 && (0..super::dock::DOCK_LAYER).contains(&w.layer))
        .map(|w| w.bounds)
        .collect()
}

/// The screenshot tool's process (⇧⌘3/4/5, the Screenshot app).
pub const SCREENSHOT_UI: &str = "com.apple.screencaptureui";

/// True for a window of macOS's own that lets every click through to what is below it, so
/// a click under it can still be on the wallpaper: the screenshot tool's (owner
/// "Screenshot", process `screencaptureui`), which after a screenshot leaves an opaque,
/// screen-sized window at layer 24 listed on screen for minutes (owner 11-09 10:02: every
/// wallpaper click after it was taken for a click on a window). Only above layer 0, where
/// no app's document window lives, and only by the owner's identity: its bundle id
/// (`bundle`, when the process has one) or, with none read, its process or app name.
/// Everything else, an unknown overlay included, still covers the wallpaper.
pub fn click_through_overlay(layer: i64, owner: &str, bundle: Option<&str>) -> bool {
    if layer <= 0 {
        return false;
    }
    match bundle {
        Some(b) => b == SCREENSHOT_UI,
        None => matches!(owner, "screencaptureui" | "Screenshot"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(layer: i64, bounds: Rect, onscreen: bool) -> CgWindow {
        CgWindow { id: 1, pid: 2, owner: String::new(), name: None, layer, bounds, onscreen, alpha: 1.0 }
    }

    #[test]
    fn resize_bands_are_read_of_app_windows_only() {
        let app = Rect::new(100.0, 100.0, 500.0, 400.0);
        let panel = Rect::new(700.0, 100.0, 200.0, 200.0);
        let mut clear = w(0, Rect::new(0.0, 0.0, 50.0, 50.0), true);
        clear.alpha = 0.0;
        let list = [
            w(0, app, true),
            w(3, panel, true),
            w(0, Rect::new(900.0, 900.0, 100.0, 100.0), false),
            w(crate::platform::dock::DOCK_LAYER, Rect::new(0.0, 0.0, 1440.0, 900.0), true),
            w(25, Rect::new(0.0, 0.0, 1440.0, 25.0), true),
            w(-2_147_483_603, Rect::new(0.0, 0.0, 1440.0, 900.0), true),
            clear,
        ];
        assert_eq!(resizable_frames(&list), vec![app, panel]);
    }

    #[test]
    fn appkit_rects_flip_to_top_left() {
        // A 1440 × 900 primary screen; its visible frame without a 25 pt menu bar.
        let visible = Rect::new(0.0, 0.0, 1440.0, 875.0);
        assert_eq!(to_top_left(visible, 900.0), Rect::new(0.0, 25.0, 1440.0, 875.0));
        // A display above the primary one.
        let above = Rect::new(0.0, 900.0, 1920.0, 1080.0);
        assert_eq!(to_top_left(above, 900.0), Rect::new(0.0, -1080.0, 1920.0, 1080.0));
        assert_eq!(point_to_top_left(Point::new(10.0, 890.0), 900.0), Point::new(10.0, 10.0));
    }

    #[test]
    fn wallpaper_clicks_hit_no_window() {
        let app = Rect::new(100.0, 100.0, 500.0, 400.0);
        let dock = Rect::new(0.0, 850.0, 1440.0, 50.0);
        let list = [w(20, dock, true), w(0, app, true), w(0, Rect::new(0.0, 0.0, 1440.0, 900.0), false)];
        assert!(!is_wallpaper_click(&list, Point::new(200.0, 200.0), &DockBar::default()), "an app window");
        assert!(!is_wallpaper_click(&list, Point::new(200.0, 870.0), &DockBar::default()), "the Dock");
        assert!(is_wallpaper_click(&list, Point::new(900.0, 600.0), &DockBar::default()), "nothing there");
        let mut clear = w(30, Rect::new(0.0, 0.0, 1440.0, 900.0), true);
        clear.alpha = 0.0;
        assert!(is_wallpaper_click(&[clear], Point::new(900.0, 600.0), &DockBar::default()), "transparent overlay");
    }

    /// 11-09: the screenshot tool's screen-sized window at layer 24 lets clicks through to
    /// the desktop; real windows, and any other overlay, still cover it.
    #[test]
    fn only_macos_click_through_overlays_are_seen_through() {
        assert!(click_through_overlay(24, "Screenshot", Some(SCREENSHOT_UI)));
        assert!(click_through_overlay(24, "Screenshot", None), "its app name, no bundle read");
        assert!(click_through_overlay(24, "screencaptureui", None));
        assert!(!click_through_overlay(0, "Screenshot", Some(SCREENSHOT_UI)), "never a layer-0 window");
        assert!(!click_through_overlay(24, "Screenshot", Some("com.example.screenshot")), "another app named so");
        assert!(!click_through_overlay(24, "Window Server", None), "the menu bar");
        assert!(!click_through_overlay(25, "Control Center", Some("com.apple.controlcenter")));
        assert!(!click_through_overlay(3, "Some App", Some("com.example.app")), "a floating panel");
        // In the hit test: the overlay alone is the wallpaper; an app window under it is not.
        let full = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let shot = CgWindow { owner: "Screenshot".into(), pid: -1, ..w(24, full, true) };
        assert!(is_wallpaper_click(std::slice::from_ref(&shot), Point::new(900.0, 600.0), &DockBar::default()));
        let app = w(0, Rect::new(800.0, 500.0, 400.0, 300.0), true);
        assert!(!is_wallpaper_click(&[shot, app], Point::new(900.0, 600.0), &DockBar::default()));
    }

    #[test]
    fn the_docks_display_sized_window_counts_only_on_its_bar() {
        // The Dock's window spans its display (seen 11-05 at 2560x1440 with auto-hide on).
        let dock = CgWindow { pid: 7, ..w(crate::platform::dock::DOCK_LAYER, Rect::new(0.0, 0.0, 2560.0, 1440.0), true) };
        let list = [dock];
        let bar = DockBar { pid: Some(7), bars: vec![Rect::new(700.0, 1370.0, 1100.0, 66.0)] };
        assert!(is_wallpaper_click(&list, Point::new(1200.0, 600.0), &bar), "away from the bar: the wallpaper");
        assert!(!is_wallpaper_click(&list, Point::new(1200.0, 1400.0), &bar), "on the bar");
        assert!(!is_wallpaper_click(&list, Point::new(1200.0, 1360.0), &bar), "on the bar's edge");
        assert!(!is_wallpaper_click(&list, Point::new(1200.0, 600.0), &DockBar { pid: Some(7), bars: Vec::new() }), "the bar unknown: the Dock's");
        assert!(!is_wallpaper_click(&list, Point::new(1200.0, 600.0), &DockBar { pid: Some(8), ..bar.clone() }), "another app's window");
        let elsewhere = DockBar { pid: Some(7), bars: vec![Rect::new(-1500.0, 1100.0, 900.0, 60.0)] };
        assert!(!is_wallpaper_click(&list, Point::new(1200.0, 600.0), &elsewhere), "the bar read on another screen: the Dock's");
    }
}
