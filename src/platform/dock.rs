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
//!
//! "Follow the Dock" (`[strip] order = "dock"`) reads the order of the Dock's application
//! items, pinned and running alike, as the Dock shows them ([`app_order`]); the engine sorts
//! the strip by it.

use std::time::{Duration, Instant};

use std::collections::HashMap;

use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_foundation::{NSBundle, NSString};

use super::ax::{AxElement, AxError, AxResult, names};
use super::windows::CgWindow;
use crate::engine::DockApp;
use crate::engine::geometry::{Point, Rect};
use crate::engine::sliver::DockBar;

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

/// True when the topmost window on screen at `p` is the Dock's bar (`dock`: the Dock's pid;
/// layer [`DOCK_LAYER`]): not its wallpaper, nor Mission Control, Launchpad or a folder's
/// popup, which the Dock draws at other layers. The Dock's window spans its display, clear
/// away from the bar: there it is seen through (`bar`, [`DockBar::seen_through`]), so a press
/// in the middle of the screen asks the Dock nothing (the review's switching 3). macOS's
/// click-through overlays above the Dock (`through`: [`super::desktop::seen_through`] at `p`;
/// 11-09, Notification Center's full-screen layer 23 off its own elements) are seen through too.
pub fn dock_under(list: &[CgWindow], p: Point, dock: i32, bar: &DockBar, through: &dyn Fn(&CgWindow) -> bool) -> bool {
    list.iter()
        .find(|w| w.onscreen && w.alpha > 0.0 && w.bounds.contains(p) && !bar.seen_through(w.pid, w.layer, w.bounds, p) && !through(w))
        .is_some_and(|w| w.pid == dock && w.layer == DOCK_LAYER)
}

/// True when app `pid` has a window on screen (layer 0) out of the park zone (`parks`), on
/// any display: macOS activates it there and switches no Desktop, whatever the model knows
/// (a window ignored by a rule, one not tracked, a placement not caught up yet).
pub fn visible_window_of(list: &[CgWindow], pid: i32, parks: impl Fn(Rect) -> bool) -> bool {
    list.iter().any(|w| w.pid == pid && w.layer == 0 && w.onscreen && w.alpha > 0.0 && !parks(w.bounds))
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

/// What the Dock's hit-test found at a point that is no application item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Miss {
    /// The hit element's role and subrole (`"?"` when unknown), or why nothing was found.
    pub found: String,
    /// The hit element is a Dock item of some kind (a folder, the Trash, a minimized window)
    /// or nothing at all: worth a line in the day's log.
    pub notable: bool,
}

/// True when `role` is a Dock item's (`AXDockItem`, whatever its subrole).
fn dock_item_role(role: &str) -> bool {
    role == names::ROLE_DOCK_ITEM
}

/// The application item of the Dock (`dock`: its pid) at `p`, with its element; else what
/// the hit-test found there. Every call runs at the Dock's bounded timeout, and a Dock that
/// leaves one unanswered is asked nothing more (the review's K6: up to seven calls at the
/// timeout, on the WM thread, at every mouse-down on the Dock).
pub fn item_at(dock: i32, p: Point) -> Result<(AxElement, DockItem), Miss> {
    let t = DOCK_AX_TIMEOUT_SECONDS;
    let element = AxElement::application(dock)
        .bounded(t, |app| app.element_at(p))
        .map_err(|e| Miss { found: format!("nothing ({e})"), notable: true })?;
    // The hit-test may answer a part of the item: its parents are looked at too.
    match walk_up(element.clone(), ITEM_STEPS, |e| e.bounded(t, application_item), |e| e.bounded(t, AxElement::parent)) {
        Walk::Found(at, item) => Ok((at, item)),
        Walk::Unanswered => Err(Miss { found: "no answer (the Dock is busy)".into(), notable: true }),
        Walk::Missed => {
            let (role, subrole) = element.bounded(t, |e| (e.role().unwrap_or_else(|_| "?".into()), e.subrole().unwrap_or_else(|_| "?".into())));
            Err(Miss { notable: dock_item_role(&role), found: format!("{role} / {subrole}") })
        }
    }
}

/// How many elements a hit-test's walk looks at: the one hit and up to two parents.
const ITEM_STEPS: usize = 3;

/// What a walk up from a hit element found.
#[derive(Debug, PartialEq, Eq)]
enum Walk<E, I> {
    /// An item, at this element.
    Found(E, I),
    /// No item within the steps.
    Missed,
    /// A call went unanswered (`CannotComplete` at the timeout): the walk stopped there.
    Unanswered,
}

/// Looks for an item (`item`) at `start` and up to `steps - 1` of its parents (`parent`),
/// stopping at the first unanswered call. An error of any other kind is no item here (an
/// attribute an element lacks); a parent that cannot be read ends the walk.
fn walk_up<E, I>(start: E, steps: usize, item: impl Fn(&E) -> AxResult<Option<I>>, parent: impl Fn(&E) -> AxResult<E>) -> Walk<E, I> {
    let mut at = start;
    for step in 0..steps {
        match item(&at) {
            Ok(Some(found)) => return Walk::Found(at, found),
            Err(AxError::CANNOT_COMPLETE) => return Walk::Unanswered,
            Ok(None) | Err(_) => {}
        }
        if step + 1 == steps {
            break;
        }
        match parent(&at) {
            Ok(p) => at = p,
            Err(AxError::CANNOT_COMPLETE) => return Walk::Unanswered,
            Err(_) => break,
        }
    }
    Walk::Missed
}

/// Where the Dock's bar is (`dock`: its pid): the frame around its lists (the Dock app's
/// children), top-left origin; `None` when Accessibility gives none. The window list cannot
/// tell it: the Dock's window spans its whole display ([`crate::engine::sliver::DockBar`]).
pub fn bar_frame(dock: i32) -> Option<Rect> {
    AxElement::application(dock).bounded(DOCK_AX_TIMEOUT_SECONDS, |app| {
        let t = DOCK_AX_TIMEOUT_SECONDS;
        let frames = app.children().ok()?.iter().filter_map(|list| list.bounded(t, AxElement::frame).ok()).collect::<Vec<_>>();
        frames.into_iter().filter(|f| !f.is_degenerate()).reduce(|a, b| a.union(&b))
    })
}

/// The Dock's application item for the app with bundle id `bundle` (any of the Dock's
/// lists): where the test hook clicks.
pub fn item_for_bundle(dock: i32, bundle: &str) -> Option<(AxElement, DockItem)> {
    AxElement::application(dock).bounded(DOCK_AX_TIMEOUT_SECONDS, |app| {
        let lists = app.children().ok()?;
        let t = DOCK_AX_TIMEOUT_SECONDS;
        lists.iter().flat_map(|list| list.bounded(t, AxElement::children).unwrap_or_default()).find_map(|e| {
            e.bounded(t, application_item).ok().flatten().filter(|i| bundle_id_at(&i.path).as_deref() == Some(bundle)).map(|i| (e, i))
        })
    })
}

/// `element` as an application item of the Dock: `None` for another element; an error as
/// the Dock gave it (so an unanswered call stops a walk).
fn application_item(element: &AxElement) -> AxResult<Option<DockItem>> {
    if element.role()? != names::ROLE_DOCK_ITEM || element.subrole()? != names::SUBROLE_APPLICATION_DOCK_ITEM {
        return Ok(None);
    }
    Ok(Some(DockItem { path: element.url_path()?, frame: element.frame()? }))
}

/// The Dock's application items in its order (`dock`: its pid), left to right (top to bottom
/// on the side), each with its bundle id and, when it runs, its pid: the order "Follow the
/// Dock" sorts the strip by. Read through Accessibility only (the Dock's lists, `AXChildren`,
/// each item's subrole and `AXURL`), never its preferences, so running apps that are not
/// pinned are in it too. Every call runs at the Dock's bounded timeout; `None` when the Dock
/// does not answer (the last order read stays).
pub fn app_order(dock: i32) -> Option<Vec<DockApp>> {
    let t = DOCK_AX_TIMEOUT_SECONDS;
    let lists = AxElement::application(dock).bounded(t, AxElement::children).ok()?;
    let mut paths = Vec::new();
    for list in &lists {
        let items = match list.bounded(t, AxElement::children) {
            Ok(items) => items,
            Err(AxError::CANNOT_COMPLETE) => return None,
            Err(_) => continue,
        };
        for item in items {
            match item.bounded(t, |e| e.subrole().map(|s| (s == names::SUBROLE_APPLICATION_DOCK_ITEM).then(|| e.url_path()))) {
                Ok(Some(Ok(path))) => paths.push(path),
                // A Dock that stops answering: no half-read order.
                Err(AxError::CANNOT_COMPLETE) | Ok(Some(Err(AxError::CANNOT_COMPLETE))) => return None,
                _ => {}
            }
        }
    }
    let running = running_by_path();
    Some(
        paths
            .into_iter()
            .map(|path| DockApp { bundle: bundle_id_at(&path), pid: running.get(path.trim_end_matches('/')).copied() })
            .collect(),
    )
}

/// Where a window just minimized sits in the Dock (`dock`: its pid), read through
/// Accessibility at the Dock's bounded timeout: the minimized-window item titled `title`
/// (the last such one when several are: the Dock adds each window minimized at the end of
/// its minimized items, just before the Trash), else the icon of its app (`app_path`, the
/// app bundle's path), where macOS's "Minimize windows into application icon" puts it (seen
/// 11-09: the un-minimize starts from the app's icon, 61×61 inside its 68×84 item). `None`
/// when the Dock has neither. A Dock that leaves a call unanswered is asked nothing more
/// ([`tile_among`]): what was read by then is the answer.
pub fn minimized_tile(dock: i32, app_path: Option<&str>, title: &str) -> Option<Rect> {
    let t = DOCK_AX_TIMEOUT_SECONDS;
    let lists = AxElement::application(dock).bounded(t, AxElement::children).ok()?;
    let app_path = app_path.map(|p| p.trim_end_matches('/'));
    // Each list's items are asked for only once the list before is read through.
    let items = lists
        .iter()
        .map(|list| list.bounded(t, AxElement::children))
        .take_while(|items| !matches!(items, Err(AxError::CANNOT_COMPLETE)))
        .flat_map(Result::unwrap_or_default);
    tile_among(items, |item, want_icon| {
        let subrole = item.bounded(t, AxElement::subrole)?;
        if subrole == names::SUBROLE_MINIMIZED_WINDOW_DOCK_ITEM {
            if item.bounded(t, AxElement::title)? == title {
                return item.bounded(t, AxElement::frame).map(TileItem::Own);
            }
        } else if subrole == names::SUBROLE_APPLICATION_DOCK_ITEM
            && want_icon
            && let Some(want) = app_path
            && item.bounded(t, AxElement::url_path)?.trim_end_matches('/') == want
        {
            return item.bounded(t, AxElement::frame).map(TileItem::Icon);
        }
        Ok(TileItem::Other)
    })
}

/// What one Dock item is to a window just minimized ([`minimized_tile`]).
#[derive(Debug, Clone, Copy, PartialEq)]
enum TileItem {
    /// The window's own minimized-window item, with its frame.
    Own(Rect),
    /// Its app's icon, with its frame.
    Icon(Rect),
    /// Neither.
    Other,
}

/// The tile among the Dock's `items`, each read by `read` (told whether the app's icon is
/// still looked for) only when the one before is read: the last [`TileItem::Own`], else the
/// first [`TileItem::Icon`]. The first call the Dock leaves unanswered (`CannotComplete` at
/// its timeout) ends the reading, as in [`item_at`] and [`app_order`]: every further item
/// would hold the WM thread a whole timeout more (a Dock of forty items: four seconds). An
/// error of any other kind is an item with no such attribute.
fn tile_among<I>(items: impl IntoIterator<Item = I>, mut read: impl FnMut(&I, bool) -> AxResult<TileItem>) -> Option<Rect> {
    let (mut own, mut icon) = (None, None);
    for item in items {
        match read(&item, icon.is_none()) {
            Ok(TileItem::Own(frame)) => own = Some(frame),
            Ok(TileItem::Icon(frame)) => icon = icon.or(Some(frame)),
            Err(AxError::CANNOT_COMPLETE) => break,
            Ok(TileItem::Other) | Err(_) => {}
        }
    }
    own.or(icon).filter(|f| !f.is_degenerate())
}

/// The path of app `pid`'s bundle (no trailing slash), when it runs.
pub fn app_path(pid: i32) -> Option<String> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    Some(app.bundleURL()?.path()?.to_string().trim_end_matches('/').to_string())
}

/// The running apps by their bundle's path (no trailing slash).
fn running_by_path() -> HashMap<String, i32> {
    NSWorkspace::sharedWorkspace()
        .runningApplications()
        .iter()
        .filter_map(|a| {
            let path = a.bundleURL()?.path()?.to_string();
            Some((path.trim_end_matches('/').to_string(), a.processIdentifier()))
        })
        .collect()
}

/// The bundle id of the app bundle at `path`.
pub fn bundle_id_at(path: &str) -> Option<String> {
    NSBundle::bundleWithPath(&NSString::from_str(path))?.bundleIdentifier().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Dock that stops answering mid-read is asked nothing more: each further item would
    /// wait its whole timeout on the WM thread. What was read by then stands.
    #[test]
    fn a_minimized_tile_read_stops_at_the_docks_first_unanswered_call() {
        let icon = Rect::new(900.0, 1380.0, 68.0, 84.0);
        let own = Rect::new(1200.0, 1380.0, 68.0, 84.0);
        let asked = std::cell::Cell::new(0);
        let items = |answers: Vec<AxResult<TileItem>>| {
            asked.set(0);
            tile_among(answers, |a, _| {
                asked.set(asked.get() + 1);
                *a
            })
        };
        let mut busy = vec![Ok(TileItem::Other), Ok(TileItem::Icon(icon)), Err(AxError::CANNOT_COMPLETE)];
        busy.extend(std::iter::repeat_n(Err(AxError::CANNOT_COMPLETE), 40));
        assert_eq!(items(busy), Some(icon), "the icon read before the Dock stopped answering");
        assert_eq!(asked.get(), 3, "nothing asked after the first unanswered call");
        // An item lacking an attribute is passed over; the window's own tile (the last of
        // them) wins over its app's icon.
        let all = vec![Ok(TileItem::Icon(icon)), Err(AxError::NO_VALUE), Ok(TileItem::Own(icon)), Ok(TileItem::Own(own)), Ok(TileItem::Other)];
        assert_eq!(items(all), Some(own));
        assert_eq!(asked.get(), 5);
        assert_eq!(items(vec![Ok(TileItem::Other)]), None);
    }

    fn win(pid: i32, layer: i64, bounds: Rect) -> CgWindow {
        CgWindow { id: 1, pid, owner: String::new(), name: None, layer, bounds, onscreen: true, alpha: 1.0 }
    }

    #[test]
    fn only_the_docks_bar_on_top_is_the_dock() {
        let bar = win(7, DOCK_LAYER, Rect::new(400.0, 1100.0, 1000.0, 69.0));
        let app = win(9, 0, Rect::new(0.0, 0.0, 1800.0, 1169.0));
        let p = Point::new(500.0, 1130.0);
        let none = DockBar::default();
        let no_overlay = |_: &CgWindow| false;
        assert!(dock_under(&[bar.clone(), app.clone()], p, 7, &none, &no_overlay));
        assert!(!dock_under(&[app.clone(), bar.clone()], p, 7, &none, &no_overlay), "a window above it");
        assert!(!dock_under(std::slice::from_ref(&bar), Point::new(100.0, 1130.0), 7, &none, &no_overlay), "off the bar");
        assert!(!dock_under(&[win(7, -2147483624, Rect::new(0.0, 0.0, 1800.0, 1169.0))], p, 7, &none, &no_overlay), "the Dock's wallpaper");
        assert!(!dock_under(&[win(7, 27, Rect::new(0.0, 0.0, 1800.0, 1169.0))], p, 7, &none, &no_overlay), "Mission Control, Launchpad, a popup");
        let hidden = CgWindow { alpha: 0.0, ..bar };
        assert!(!dock_under(&[hidden, app], p, 7, &none, &no_overlay), "an auto-hidden bar's transparent window is not it");
    }

    #[test]
    fn the_docks_display_sized_window_is_the_dock_only_on_its_bar() {
        // One window as large as the display, clear away from the bar (seen 11-05).
        let whole = win(7, DOCK_LAYER, Rect::new(0.0, 0.0, 2560.0, 1440.0));
        let app = win(9, 0, Rect::new(200.0, 200.0, 1200.0, 800.0));
        let known = DockBar { pid: Some(7), bars: vec![Rect::new(700.0, 1370.0, 1100.0, 66.0)] };
        let list = [whole, app];
        let no_overlay = |_: &CgWindow| false;
        assert!(dock_under(&list, Point::new(1000.0, 1400.0), 7, &known, &no_overlay), "on the bar");
        assert!(!dock_under(&list, Point::new(1180.0, 600.0), 7, &known, &no_overlay), "mid-screen: the app's window below, no Dock ask");
        assert!(!dock_under(&list, Point::new(2000.0, 100.0), 7, &known, &no_overlay), "the wallpaper below");
        assert!(dock_under(&list, Point::new(1180.0, 600.0), 7, &DockBar::default(), &no_overlay), "the bar unknown: asked, as before");
        // Notification Center's full-screen layer above it (11-09): the bar under it is the
        // Dock's; another app's window over the bar is not.
        let nc = win(894, 23, Rect::new(0.0, 0.0, 2560.0, 1440.0));
        let overlay = |w: &CgWindow| w.pid == 894;
        let covered = [nc.clone(), list[0].clone(), list[1].clone()];
        assert!(dock_under(&covered, Point::new(1000.0, 1400.0), 7, &known, &overlay), "seen through");
        assert!(!dock_under(&covered, Point::new(1000.0, 1400.0), 7, &known, &no_overlay), "not seen through: it covers the bar");
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
        let clear = CgWindow { alpha: 0.0, ..shown.clone() };
        assert!(!visible_window_of(&[clear], 9, parks), "a fully transparent window");
        let panel = CgWindow { layer: 3, ..shown };
        assert!(!visible_window_of(&[panel], 9, parks), "not a window's layer");
    }

    /// K6: a walk up from the hit element stops at the first unanswered call; another error
    /// is no item there, and the walk goes on to the parent.
    #[test]
    fn a_dock_that_does_not_answer_is_asked_nothing_more() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let busy = || AxError::CANNOT_COMPLETE;
        let walk = walk_up(
            0u32,
            ITEM_STEPS,
            |_| {
                calls.set(calls.get() + 1);
                Err::<Option<&str>, _>(busy())
            },
            |e| Ok(e + 1),
        );
        assert_eq!(walk, Walk::Unanswered);
        assert_eq!(calls.get(), 1, "one call, not seven");
        // A part of the item hit: no item there (an attribute it lacks), its parent is one.
        let found = walk_up(0u32, ITEM_STEPS, |e| if *e == 1 { Ok(Some("item")) } else { Err(AxError::NO_VALUE) }, |e| Ok(e + 1));
        assert_eq!(found, Walk::Found(1, "item"));
        // A parent unanswered stops it too; nothing within the steps is a miss.
        assert_eq!(walk_up(0u32, ITEM_STEPS, |_| Ok(None::<&str>), |_| Err(busy())), Walk::Unanswered);
        let parents = Cell::new(0);
        let missed = walk_up(
            0u32,
            ITEM_STEPS,
            |_| Ok(None::<&str>),
            |e| {
                parents.set(parents.get() + 1);
                Ok(e + 1)
            },
        );
        assert_eq!((missed, parents.get()), (Walk::Missed, 2), "no parent read past the last step");
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
