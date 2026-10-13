//! The Dock, read through public Accessibility only: which app a click in the Dock is for.
//!
//! macOS reports a Dock click's activation, its window's focus and the Space change only
//! once the Desktop slide is over (measured 10-13: the slide from +90 to +330 ms after the
//! activation, the notice at +346 ms; the owner's Dock click: the focus at +419 ms after
//! the mouse-down). The click itself is the only earlier sign of where the user goes. So a
//! left mouse-down over the Dock's own window (layer [`DOCK_LAYER`], the topmost window at
//! the point) is hit-tested with `AXUIElementCopyElementAtPosition` on the Dock's
//! application element: an application item (role `AXDockItem`, subrole
//! `AXApplicationDockItem`) gives its bundle (`AXURL`) and its frame. The press counts as a
//! click on that item ([`is_click`]) when it is let go over the same item, soon and without
//! moving (a press held opens the item's menu; a drag moves or removes it) and without a
//! modifier (⌘ reveals the app in Finder, ⌥ hides the others, ⌃ opens the menu, ⇧ is the
//! Shift sugar's).

use std::time::{Duration, Instant};

use objc2_app_kit::NSRunningApplication;
use objc2_foundation::{NSBundle, NSString};

use super::ax::{AxElement, names};
use super::windows::CgWindow;
use crate::engine::geometry::{Point, Rect};

/// The Dock's bundle id.
pub const DOCK_BUNDLE: &str = "com.apple.dock";

/// The window layer of the Dock's bar (`kCGDockWindowLevel`).
pub const DOCK_LAYER: i64 = 20;

/// A press held longer than this is no click (the Dock opens the item's menu).
pub const CLICK_MAX: Duration = Duration::from_millis(600);

/// A press that moves farther than this (points) is a drag.
pub const CLICK_SLOP: f64 = 6.0;

/// The Accessibility timeout for asking the Dock (it must never hold a click up).
const DOCK_AX_TIMEOUT_SECONDS: f32 = 0.1;

/// An application item of the Dock.
#[derive(Debug, Clone, PartialEq)]
pub struct DockItem {
    /// The app bundle's path (`AXURL`).
    pub path: String,
    /// The item's frame (global, top-left).
    pub frame: Rect,
}

/// A left mouse-down on a Dock item, waiting for its mouse-up, with what its click would
/// do, worked out at the press (`T`: the controller's prediction).
#[derive(Debug, Clone, PartialEq)]
pub struct DockPress<T> {
    /// The item pressed.
    pub item: DockItem,
    /// Where.
    pub at: Point,
    /// When.
    pub when: Instant,
    /// The prediction made at the press.
    pub prediction: T,
}

/// True when the topmost window on screen at `p` is the Dock's (`dock`: its pid) above the
/// windows' layer: its bar ([`DOCK_LAYER`]), not its wallpaper (below every window).
pub fn dock_under(list: &[CgWindow], p: Point, dock: i32) -> bool {
    list.iter()
        .find(|w| w.onscreen && w.alpha > 0.0 && w.bounds.contains(p))
        .is_some_and(|w| w.pid == dock && w.layer > 0)
}

/// True when app `pid` has a window on screen (layer 0) out of the park zone (`parks`), on
/// any display: macOS activates it there and switches no Desktop, whatever the model knows
/// (a window ignored by a rule, one not tracked, a placement not caught up yet).
pub fn visible_window_of(list: &[CgWindow], pid: i32, parks: impl Fn(Rect) -> bool) -> bool {
    list.iter().any(|w| w.pid == pid && w.layer == 0 && w.onscreen && !parks(w.bounds))
}

/// True when a press on a Dock item let go at `up` (when `up_at`) is a click on it: over
/// the same item, within [`CLICK_SLOP`] of the press, before [`CLICK_MAX`], no modifier held.
pub fn is_click<T>(press: &DockPress<T>, up: Point, up_at: Instant, modifiers: bool) -> bool {
    !modifiers
        && press.item.frame.contains(up)
        && (up.x - press.at.x).abs().max((up.y - press.at.y).abs()) <= CLICK_SLOP
        && up_at.saturating_duration_since(press.when) <= CLICK_MAX
}

/// The Dock's pid, when it runs.
pub fn dock_pid() -> Option<i32> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(DOCK_BUNDLE))
        .iter()
        .next()
        .map(|a| a.processIdentifier())
}

/// The application item of the Dock (`dock`: its pid) at `p`, with its element; else what
/// the hit-test found there (its role), for the log.
pub fn item_at(dock: i32, p: Point) -> Result<(AxElement, DockItem), String> {
    AxElement::application(dock).bounded(DOCK_AX_TIMEOUT_SECONDS, |app| {
        let element = app.element_at(p).map_err(|e| format!("nothing ({e})"))?;
        // The hit-test may answer a part of the item: its parents are looked at too.
        let mut at = element.clone();
        for _ in 0..3 {
            if let Some(item) = application_item(&at) {
                return Ok((at, item));
            }
            let Ok(parent) = at.parent() else { break };
            at = parent;
        }
        Err(format!(
            "{} / {}",
            element.role().unwrap_or_else(|_| "?".into()),
            element.subrole().unwrap_or_else(|_| "?".into())
        ))
    })
}

/// The Dock's application item for the app with bundle id `bundle` (any of the Dock's
/// lists): where the test hook clicks.
pub fn item_for_bundle(dock: i32, bundle: &str) -> Option<(AxElement, DockItem)> {
    AxElement::application(dock).bounded(DOCK_AX_TIMEOUT_SECONDS, |app| {
        let lists = app.children().ok()?;
        lists.iter().flat_map(|list| list.children().unwrap_or_default()).find_map(|e| {
            application_item(&e).filter(|i| bundle_id_at(&i.path).as_deref() == Some(bundle)).map(|i| (e, i))
        })
    })
}

/// `element` as an application item of the Dock.
fn application_item(element: &AxElement) -> Option<DockItem> {
    if element.role().ok()? != names::ROLE_DOCK_ITEM || element.subrole().ok()? != names::SUBROLE_APPLICATION_DOCK_ITEM {
        return None;
    }
    Some(DockItem { path: element.url_path().ok()?, frame: element.frame().ok()? })
}

/// The bundle id of the app bundle at `path`.
pub fn bundle_id_at(path: &str) -> Option<String> {
    NSBundle::bundleWithPath(&NSString::from_str(path))?.bundleIdentifier().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(pid: i32, layer: i64, bounds: Rect) -> CgWindow {
        CgWindow { id: 1, pid, owner: String::new(), name: None, layer, bounds, onscreen: true, alpha: 1.0 }
    }

    #[test]
    fn only_the_docks_bar_on_top_is_the_dock() {
        let bar = win(7, DOCK_LAYER, Rect::new(400.0, 1100.0, 1000.0, 69.0));
        let app = win(9, 0, Rect::new(0.0, 0.0, 1800.0, 1169.0));
        let p = Point::new(500.0, 1130.0);
        assert!(dock_under(&[bar.clone(), app.clone()], p, 7));
        assert!(!dock_under(&[app.clone(), bar.clone()], p, 7), "a window above it");
        assert!(!dock_under(std::slice::from_ref(&bar), Point::new(100.0, 1130.0), 7), "off the bar");
        assert!(!dock_under(&[win(7, -2147483624, Rect::new(0.0, 0.0, 1800.0, 1169.0))], p, 7), "the Dock's wallpaper");
        let hidden = CgWindow { alpha: 0.0, ..bar };
        assert!(!dock_under(&[hidden, app], p, 7), "an auto-hidden bar's transparent window is not it");
    }

    #[test]
    fn an_apps_window_on_screen_out_of_the_park_zone_keeps_macos_here() {
        let parks = |r: Rect| r.x >= 1700.0 && r.y >= 1100.0;
        let shown = CgWindow { id: 2, ..win(9, 0, Rect::new(100.0, 100.0, 600.0, 400.0)) };
        let parked = CgWindow { id: 3, ..win(9, 0, Rect::new(1780.0, 1150.0, 600.0, 400.0)) };
        let away = CgWindow { onscreen: false, ..shown.clone() };
        assert!(visible_window_of(std::slice::from_ref(&shown), 9, parks), "a window here (any display)");
        assert!(!visible_window_of(&[parked], 9, parks), "a parked sliver: the model's gate decides");
        assert!(!visible_window_of(&[away], 9, parks), "a window of another Desktop");
        assert!(!visible_window_of(std::slice::from_ref(&shown), 8, parks), "another app's");
        let panel = CgWindow { layer: 3, ..shown };
        assert!(!visible_window_of(&[panel], 9, parks), "not a window's layer");
    }

    #[test]
    fn a_click_is_short_still_and_on_the_item_with_no_modifier() {
        let t0 = Instant::now();
        let press = DockPress {
            item: DockItem { path: "/Applications/Example.app".into(), frame: Rect::new(500.0, 1110.0, 60.0, 60.0) },
            at: Point::new(530.0, 1140.0),
            when: t0,
            prediction: (),
        };
        let ms = |n| t0 + Duration::from_millis(n);
        assert!(is_click(&press, Point::new(531.0, 1141.0), ms(120), false));
        assert!(!is_click(&press, Point::new(531.0, 1141.0), ms(120), true), "a modifier: Finder, hide others, the menu");
        assert!(!is_click(&press, Point::new(531.0, 1141.0), ms(900), false), "held: the item's menu");
        assert!(!is_click(&press, Point::new(545.0, 1141.0), ms(120), false), "moved: a drag");
        assert!(!is_click(&press, Point::new(620.0, 1141.0), ms(120), false), "let go over another item");
    }
}
