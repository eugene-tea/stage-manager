//! The desktop: displays (from `NSScreen`, converted to top-left coordinates, every screen
//! with its identity and park corner) and wallpaper clicks (the global mouse-down monitor in
//! [`super::input`] plus a window-list hit test).
//!
//! A click counts as a wallpaper click when no window of layer 0 or above contains the
//! point: the desktop picture and Finder's desktop icons are desktop elements, which the
//! window list leaves out. A click on the Dock, the menu bar, our strip or any app window
//! hits a window and is ignored. macOS's own click-through overlays do not count
//! ([`click_through_overlay`], [`seen_through`]): every test of what lies under a point
//! looks through them (the wallpaper click here, the Dock press, the test input's path
//! checks), Notification Center's full-screen window only where the Accessibility element at
//! the point is not its own (a banner drawn inside it is). The inert slivers' tap, judging
//! presses from a list read before them, looks through the screenshot tool's alone.

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
            && !seen_through(w, p)
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

/// Notification Center's process (its banners, its panel and the widgets' editing).
pub const NOTIFICATION_CENTER_UI: &str = "com.apple.notificationcenterui";

/// Notification Center's app bundle: its executable lies inside it.
pub const NOTIFICATION_CENTER_APP: &str = "/System/Library/CoreServices/NotificationCenter.app/";

/// Who owns a listed window, as far as it could be read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Owner<'a> {
    /// The window list's owner name (the process's or app's name).
    pub name: &'a str,
    /// The owner's bundle id, when the process has one and it was read.
    pub bundle: Option<&'a str>,
    /// The owner's executable (`proc_pidpath`), read when no bundle id was.
    pub exe: Option<&'a str>,
}

/// How a listed window takes the clicks at its points ([`click_through_overlay`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Through {
    /// It covers what is below: every app's window, and any overlay not known here.
    Covers,
    /// Every click goes through it to what is below (the screenshot tool's window).
    Everywhere,
    /// A click goes through it only where the Accessibility element at the point is not its
    /// owner's ([`looked_through`]): Notification Center's full-screen window, which draws
    /// its banners, panel and alerts inside itself.
    OffItsElements,
}

/// How a window of macOS's own lets clicks through to what is below it, so a press under it
/// reaches what lies there (the wallpaper, a card, the Dock, a parked window's edge). Only
/// above layer 0, where no app's document window lives, and only by the owner's identity
/// (`owner`): its bundle id when read, else its executable or name. Everything else, an
/// unknown overlay included, covers what is below ([`Through::Covers`]).
///
/// - **The screenshot tool's** (bundle [`SCREENSHOT_UI`]; with none read, process
///   `screencaptureui` or app "Screenshot"), which after a screenshot leaves an opaque,
///   screen-sized window at layer 24 listed on screen for minutes (owner 11-09 10:02: every
///   wallpaper click after it was taken for a click on a window). Any size, every point
///   ([`Through::Everywhere`]).
/// - **Notification Center's full-screen window** (bundle [`NOTIFICATION_CENTER_UI`]; with
///   none read, an executable inside [`NOTIFICATION_CENTER_APP`]) whose `bounds` cover a
///   whole screen of `screens` (within a point): layer 23, alpha 1, 0,0 2560x1440 over the
///   main screen (11-09 14:45, for the suite's whole run, no banner shown: every card click
///   of the suite was refused under it). Measured 11-09 18:0x on macOS 26: Notification
///   Center has no window on screen before a banner; with one up its only window is this
///   full-screen one, the banner drawn inside it (no banner-sized window of its own), and
///   `AXUIElementCopyElementAtPosition` on the system-wide element answers Notification
///   Center's `AXGroup` (subrole `AXNotificationCenterBanner`) on the banner, and whatever
///   lies below (Finder's desktop, an app's window) off it. So it is looked through only
///   where that element is not Notification Center's ([`Through::OffItsElements`]): a
///   point on a banner, its close button, its panel, an alert's buttons or the widgets'
///   editing is its own, as an unknown window's would be (17:59, 1.8.1: closing banners at
///   the top right was taken for wallpaper clicks and revealed the desktop). A window of
///   Notification Center's smaller than a screen covers what is below.
pub fn click_through_overlay(layer: i64, bounds: Rect, owner: &Owner<'_>, screens: &[Rect]) -> Through {
    if layer <= 0 {
        return Through::Covers;
    }
    let screenshot = match owner.bundle {
        Some(b) => b == SCREENSHOT_UI,
        None => matches!(owner.name, "screencaptureui" | "Screenshot"),
    };
    let notification_center = match owner.bundle {
        Some(b) => b == NOTIFICATION_CENTER_UI,
        None => owner.exe.is_some_and(|e| e.starts_with(NOTIFICATION_CENTER_APP)),
    };
    if screenshot {
        Through::Everywhere
    } else if notification_center && screens.iter().any(|s| covers_screen(bounds, *s)) {
        Through::OffItsElements
    } else {
        Through::Covers
    }
}

/// Whether a click at a point goes through a window that lets clicks through as `through`
/// and is owned by `pid`. `element_pid`: the owner of the Accessibility element at that
/// point ([`element_pid_at`]), asked only for [`Through::OffItsElements`]; no answer
/// (`None`) and the window's own element both cover what is below.
pub fn looked_through(through: Through, pid: i32, element_pid: impl FnOnce() -> Option<i32>) -> bool {
    match through {
        Through::Covers => false,
        Through::Everywhere => true,
        Through::OffItsElements => element_pid().is_some_and(|e| e != pid),
    }
}

/// True when `bounds` holds the whole of `screen`, within a point.
fn covers_screen(bounds: Rect, screen: Rect) -> bool {
    !screen.is_degenerate()
        && bounds.x <= screen.x + 1.0
        && bounds.y <= screen.y + 1.0
        && bounds.max_x() >= screen.max_x() - 1.0
        && bounds.max_y() >= screen.max_y() - 1.0
}

/// [`click_through_overlay`] for window `w`: its owner read now (bundle id, else its
/// executable), against every active display. A window at layer 0 or below, or one not
/// drawn, is answered at once with no read ([`Through::Covers`]).
pub fn overlay_of(w: &CgWindow) -> Through {
    if w.layer <= 0 || !w.onscreen || w.alpha <= 0.0 {
        return Through::Covers;
    }
    let bundle = super::apps::bundle_of(w.pid);
    let exe = if bundle.is_none() { super::apps::exe_path(w.pid) } else { None };
    let owner = Owner { name: &w.owner, bundle: bundle.as_deref(), exe: exe.as_deref() };
    click_through_overlay(w.layer, w.bounds, &owner, &cg_display_bounds())
}

/// True when a click at `p` (top-left coordinates, within `w`'s bounds) goes through window
/// `w` to what is below ([`overlay_of`], [`looked_through`]). Accessibility is asked
/// ([`element_pid_at`]) only when `w` is Notification Center's full-screen window.
pub fn seen_through(w: &CgWindow, p: Point) -> bool {
    looked_through(overlay_of(w), w.pid, || element_pid_at(p))
}

/// The messaging timeout of [`element_pid_at`]: a hung Notification Center holds the caller
/// up this long at most, and the point then counts as its own.
pub const ELEMENT_AT_TIMEOUT_SECONDS: f32 = 0.1;

/// The pid owning the Accessibility element at `p` (top-left coordinates), from
/// `AXUIElementCopyElementAtPosition` on the system-wide element; `None` without an answer
/// within [`ELEMENT_AT_TIMEOUT_SECONDS`]. The system-wide element's timeout is the
/// process's global one, so it is set for the call and put back to
/// [`super::wm_thread::AX_TIMEOUT_SECONDS`] after: another thread's Accessibility call made
/// meanwhile only gets the shorter budget.
pub fn element_pid_at(p: Point) -> Option<i32> {
    let system = super::ax::AxElement::system_wide();
    let _ = system.set_messaging_timeout(ELEMENT_AT_TIMEOUT_SECONDS);
    let pid = system.element_at(p).and_then(|e| e.pid());
    let _ = system.set_messaging_timeout(super::wm_thread::AX_TIMEOUT_SECONDS);
    match pid {
        Ok(pid) => Some(pid),
        Err(e) => {
            tracing::debug!(x = p.x, y = p.y, "no Accessibility element at the point ({e}): Notification Center's window covers it");
            None
        }
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
        let full = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let screens = [full];
        let named = |name| Owner { name, ..Owner::default() };
        let of = |name, bundle| Owner { name, bundle: Some(bundle), exe: None };
        let shot = |layer| click_through_overlay(layer, full, &of("Screenshot", SCREENSHOT_UI), &screens);
        assert_eq!(shot(24), Through::Everywhere);
        assert_eq!(click_through_overlay(24, full, &named("Screenshot"), &screens), Through::Everywhere, "its app name, no bundle read");
        assert_eq!(click_through_overlay(24, full, &named("screencaptureui"), &screens), Through::Everywhere);
        assert_eq!(click_through_overlay(24, Rect::new(100.0, 100.0, 300.0, 200.0), &named("Screenshot"), &screens), Through::Everywhere, "any size");
        assert_eq!(shot(0), Through::Covers, "never a layer-0 window");
        assert_eq!(click_through_overlay(24, full, &of("Screenshot", "com.example.screenshot"), &screens), Through::Covers, "another app named so");
        assert_eq!(click_through_overlay(24, full, &named("Window Server"), &screens), Through::Covers, "the menu bar");
        assert_eq!(click_through_overlay(25, full, &of("Control Center", "com.apple.controlcenter"), &screens), Through::Covers);
        assert_eq!(click_through_overlay(3, full, &of("Some App", "com.example.app"), &screens), Through::Covers, "a floating panel");
        // Everywhere, whatever element lies at the point: Accessibility is never asked.
        assert!(looked_through(Through::Everywhere, 5, || panic!("asked")));
        assert!(!looked_through(Through::Covers, 5, || panic!("asked")));
        // In the hit test: the overlay alone is the wallpaper; an app window under it is not.
        let shot = CgWindow { owner: "Screenshot".into(), pid: -1, ..w(24, full, true) };
        assert!(is_wallpaper_click(std::slice::from_ref(&shot), Point::new(900.0, 600.0), &DockBar::default()));
        let app = w(0, Rect::new(800.0, 500.0, 400.0, 300.0), true);
        assert!(!is_wallpaper_click(&[shot, app], Point::new(900.0, 600.0), &DockBar::default()));
    }

    /// Notification Center's full-screen window at layer 23 (11-09 14:45: over the whole main
    /// screen for the suite's whole run; 18:0x: its only window with a banner up, the banner
    /// drawn inside it) is told by its bundle id or its executable, and only over a whole
    /// screen; a smaller window of its own, and an unknown app's full-screen window, cover
    /// what is below.
    #[test]
    fn notification_centers_full_screen_window_is_known_by_its_owner_and_size() {
        let main = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let laptop = Rect::new(-1800.0, 271.0, 1800.0, 1169.0);
        let screens = [main, laptop];
        let nc = Owner { name: "Notification Center", bundle: Some(NOTIFICATION_CENTER_UI), exe: None };
        let by_exe = Owner {
            name: "Notification Center",
            bundle: None,
            exe: Some("/System/Library/CoreServices/NotificationCenter.app/Contents/MacOS/NotificationCenter"),
        };
        let off = Through::OffItsElements;
        assert_eq!(click_through_overlay(23, main, &nc, &screens), off, "its full-screen window");
        assert_eq!(click_through_overlay(23, main, &by_exe, &screens), off, "by its executable, no bundle read");
        assert_eq!(click_through_overlay(23, laptop, &nc, &screens), off, "over the other screen");
        assert_eq!(click_through_overlay(23, Rect::new(-0.5, 0.0, 2560.5, 1440.0), &nc, &screens), off, "within a point");
        // A window of its own smaller than a screen covers what is below.
        assert_eq!(click_through_overlay(23, Rect::new(2190.0, 38.0, 360.0, 76.0), &nc, &screens), Through::Covers, "banner-sized");
        assert_eq!(click_through_overlay(23, Rect::new(2160.0, 30.0, 400.0, 1410.0), &nc, &screens), Through::Covers, "panel-sized");
        assert_eq!(click_through_overlay(23, Rect::new(0.0, 30.0, 2560.0, 1410.0), &nc, &screens), Through::Covers, "short of the whole screen");
        assert_eq!(click_through_overlay(0, main, &nc, &screens), Through::Covers, "never at layer 0");
        // Anyone else's full-screen window covers, whatever its name.
        let named = |name| Owner { name, ..Owner::default() };
        assert_eq!(click_through_overlay(23, main, &named("Notification Center"), &screens), Through::Covers, "the name alone");
        let other = Owner { name: "Notification Center", bundle: Some("com.example.notes"), exe: None };
        assert_eq!(click_through_overlay(23, main, &other, &screens), Through::Covers, "another app named so");
        let stranger = Owner { name: "Overlay", bundle: None, exe: Some("/Applications/Overlay.app/Contents/MacOS/Overlay") };
        assert_eq!(click_through_overlay(23, main, &stranger, &screens), Through::Covers, "an unknown app's full-screen window");
        assert_eq!(click_through_overlay(23, main, &nc, &[]), Through::Covers, "no screen known");
    }

    /// 11-09 17:59 (1.8.1): closing banners at the top right was taken for wallpaper clicks.
    /// Measured 18:0x: on a banner the Accessibility element at the point is Notification
    /// Center's (`AXNotificationCenterBanner`), off it Finder's desktop or the app window
    /// below. Its full-screen window is looked through only where the element is not its own.
    #[test]
    fn notification_centers_window_is_looked_through_only_off_its_own_elements() {
        let (nc, finder, app) = (894, 520, 77);
        let off = Through::OffItsElements;
        assert!(!looked_through(off, nc, || Some(nc)), "a banner, its close button, its panel: its own element");
        assert!(looked_through(off, nc, || Some(finder)), "Finder's desktop below");
        assert!(looked_through(off, nc, || Some(app)), "another app's window below");
        assert!(!looked_through(off, nc, || None), "no answer: it covers");
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
