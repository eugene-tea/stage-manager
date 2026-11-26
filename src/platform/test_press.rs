//! Test-only presses on macOS's own windows, for the live checks and probes on fixture apps:
//! a fixture's Dock item, under any engine (`ctl debug-dock-click` in the Apple mode), and a
//! fixture window laid out by Mission Control (`ctl debug-mission-pick ID`). The synthetic
//! input of [`super::input`] goes only where our own or a `STAGE_MANAGER_ONLY` app's window is
//! topmost; the Dock and Mission Control's cover are neither, so each press here checks its
//! own target instead: the Dock item is a `STAGE_MANAGER_ONLY` app's (by the Dock's hit-test),
//! and the point picked in Mission Control lies on that app's laid-out window and on no other
//! app's window. The callers check `STAGE_MANAGER_TEST=1`.

use std::time::Duration;

use serde_json::{Value, json};

use super::ax::names;
use super::dock;
use super::windows::{self, CgWindow};
use crate::engine::geometry::{Point, Rect};
use crate::engine::overview::{self, Overview, Sighting};

/// How long `ctl debug-dock-click BUNDLE real` holds its press (the owner's clicks: ~100 ms
/// from the press to the Dock's activation).
pub const DOCK_PRESS_HOLD: Duration = Duration::from_millis(100);

/// The Dock's Accessibility timeout for the item's press.
const PRESS_TIMEOUT_SECONDS: f32 = 0.2;

/// Where the pointer shows an auto-hidden Dock whose item sits at `item` (off the display
/// `display`): the middle of the display's edge the item is beyond, a point inside it.
pub fn dock_edge(item: Rect, display: Rect) -> Option<Point> {
    let (cx, cy) = (item.x + item.w / 2.0, item.y + item.h / 2.0);
    let x = cx.clamp(display.x + 1.0, display.x + display.w - 1.0);
    let y = cy.clamp(display.y + 1.0, display.y + display.h - 1.0);
    if cy >= display.y + display.h - 1.0 {
        Some(Point::new(x, display.y + display.h - 1.0))
    } else if cx <= display.x + 1.0 {
        Some(Point::new(display.x, y))
    } else if cx >= display.x + display.w - 1.0 {
        Some(Point::new(display.x + display.w - 1.0, y))
    } else {
        None
    }
}

/// True when `bundle` is on the `STAGE_MANAGER_ONLY` list.
fn on_only_list(bundle: &str) -> bool {
    std::env::var("STAGE_MANAGER_ONLY").unwrap_or_default().split(',').any(|b| b.trim() == bundle)
}

/// `debug-dock-click BUNDLE [real]` with no prediction to feed (the Apple mode): the Dock item
/// of a `STAGE_MANAGER_ONLY` app is found and hit-tested at its center (an auto-hidden Dock
/// shown first by the pointer at its edge, `main` the main display's frame); then pressed
/// (`AXPress`, which activates the app as a click does) or, with `real`, clicked: a press and
/// a release posted there `hold` apart ([`DOCK_PRESS_HOLD`] unless given), on a thread of
/// their own. The pointer goes back where it was.
pub fn dock_press(bundle: &str, real: bool, hold: Option<Duration>, main: Option<Rect>) -> Value {
    if !on_only_list(bundle) {
        return json!({ "ok": false, "error": format!("{bundle} is not on the STAGE_MANAGER_ONLY list") });
    }
    let Some(pid) = dock::dock_pid() else { return json!({ "ok": false, "error": "the Dock does not run" }) };
    let Some((_, listed)) = dock::item_for_bundle(pid, bundle) else {
        return json!({ "ok": false, "error": format!("no Dock item for {bundle}") });
    };
    let mut at = listed.frame.center();
    let mut hit = dock::item_at(pid, at);
    let mut revealed = false;
    let off_display = main.is_some_and(|d| !d.contains(at));
    if (hit.is_err() || off_display)
        && let Some(edge) = main.and_then(|d| dock_edge(listed.frame, d))
    {
        revealed = super::input::reveal_edge(edge, Duration::from_millis(900)).is_ok();
        if let Some((_, shown)) = dock::item_for_bundle(pid, bundle) {
            at = shown.frame.center();
            hit = dock::item_at(pid, at);
        }
    }
    let mut result = match hit {
        Ok((element, item)) if item.path == listed.path => {
            if real {
                match super::input::dock_item_click(at, hold.unwrap_or(DOCK_PRESS_HOLD)) {
                    Ok(()) => json!({ "ok": true, "real": true, "at": [at.x, at.y] }),
                    Err(e) => json!({ "ok": false, "error": e }),
                }
            } else {
                match element.bounded(PRESS_TIMEOUT_SECONDS, |e| e.perform_action(names::PRESS)) {
                    Ok(()) => json!({ "ok": true, "real": false, "at": [at.x, at.y] }),
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                }
            }
        }
        Ok((_, other)) => json!({ "ok": false, "error": format!("the hit-test at {at:?} found another item ({})", other.path) }),
        Err(miss) => json!({ "ok": false, "error": format!("the hit-test at {at:?} found no application item: {}", miss.found) }),
    };
    if revealed {
        // After a real click its own thread took the place to go back to: this does nothing.
        super::input::pointer_back();
    }
    tracing::info!(bundle, real, ok = result["ok"].as_bool(), revealed, "debug-dock-click (no prediction)");
    if let Some(obj) = result.as_object_mut() {
        obj.insert("revealed_dock".into(), json!(revealed));
    }
    result
}

/// `debug-mission-pick ID`: a click on window `ID`'s place in Mission Control's layout (the
/// middle of its bounds as the window list gives them while Mission Control shows), only when
/// [`pick_point`] finds it safe. The pointer goes back where it was. With `hover` the
/// pointer is only moved there, no button (`mission-close-key`: a close by key with the
/// pointer over a hidden stage's window); `debug-pointer-back` puts it back.
pub fn mission_pick(id: u32, hover: bool) -> Value {
    let list = windows::list_windows();
    let pids = super::input::test_pids();
    let own = std::process::id() as i32;
    let display = super::desktop::cg_display_bounds().first().copied();
    let through = |w: &CgWindow, p: Point| super::desktop::seen_through(w, p);
    match pick_point(&list, id, &pids, own, display, &through) {
        Ok((p, bounds)) => match if hover { super::input::cover_hover(p) } else { super::input::cover_click(p) } {
            Ok(()) => {
                tracing::info!(id, x = p.x, y = p.y, hover, "debug-mission-pick: {}", if hover { "the pointer over it" } else { "clicked" });
                json!({ "ok": true, "at": [p.x, p.y], "bounds": bounds })
            }
            Err(e) => json!({ "ok": false, "error": e }),
        },
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

/// Where a pick of window `id` clicks in `list` (front to back) and the bounds it was read
/// at, or why it is refused: `id` is a window of one of `pids` on screen at layer 0; Mission
/// Control shows over `display` (WindowManager's window at its layer, or the Dock's cover:
/// without either the click lands on the fixture's own window, which is harmless); and every
/// other window on screen at the point is one of `pids`, ours (`own`), macOS's Dock or
/// WindowManager above the windows' layer, below the windows' layer, or seen through there
/// (`through`: a click-through overlay). Mission Control lays every window out apart, so no
/// other app's window at the point means the click picks `id`'s.
pub fn pick_point(
    list: &[CgWindow],
    id: u32,
    pids: &[i32],
    own: i32,
    display: Option<Rect>,
    through: &dyn Fn(&CgWindow, Point) -> bool,
) -> Result<(Point, Rect), String> {
    let target = list
        .iter()
        .find(|w| w.id == id && w.onscreen && w.layer == 0)
        .ok_or(format!("window {id} is not listed on screen at layer 0"))?;
    if !pids.contains(&target.pid) {
        return Err(format!("window {id} is not a STAGE_MANAGER_ONLY app's"));
    }
    let display = display.ok_or("no main display")?;
    let sight = overview::overview_in(
        list.iter().map(|w| Sighting { owner: &w.owner, layer: w.layer, bounds: w.bounds, onscreen: w.onscreen }),
        display,
    );
    if sight.overview != Some(Overview::MissionControl) && !sight.dock_cover {
        return Err("Mission Control does not show (neither WindowManager's window nor the Dock's covers the main display)".into());
    }
    let p = target.bounds.center();
    if !display.contains(p) {
        return Err(format!("window {id}'s middle {:.0},{:.0} is off the main display", p.x, p.y));
    }
    let system = |w: &CgWindow| w.layer > 0 && (w.owner == "Dock" || w.owner == "WindowManager");
    if let Some(w) = list.iter().find(|w| {
        w.id != id
            && w.onscreen
            && w.alpha > 0.0
            && w.bounds.contains(p)
            && !pids.contains(&w.pid)
            && w.pid != own
            && w.layer >= 0
            && !system(w)
            && !(w.layer > 0 && through(w, p))
    }) {
        return Err(format!("another app's window (pid {}, layer {}) lies at {:.0},{:.0}", w.pid, w.layer, p.x, p.y));
    }
    Ok((p, target.bounds))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = Rect::new(0.0, 0.0, 1800.0, 1169.0);

    fn win(id: u32, pid: i32, owner: &str, layer: i64, bounds: Rect) -> CgWindow {
        CgWindow { id, pid, owner: owner.into(), name: None, layer, bounds, onscreen: true, alpha: 1.0 }
    }

    fn mission_control() -> Vec<CgWindow> {
        vec![win(900, 80, "WindowManager", 19, MAIN), win(901, 81, "Dock", 20, MAIN)]
    }

    fn never(_: &CgWindow, _: Point) -> bool {
        false
    }

    #[test]
    fn an_auto_hidden_docks_edge_is_where_its_item_is_beyond_the_display() {
        let below = Rect::new(800.0, 1175.0, 60.0, 60.0);
        assert_eq!(dock_edge(below, MAIN), Some(Point::new(830.0, 1168.0)), "the bottom edge");
        let left = Rect::new(-70.0, 500.0, 60.0, 60.0);
        assert_eq!(dock_edge(left, MAIN), Some(Point::new(0.0, 530.0)), "the left edge");
        let right = Rect::new(1810.0, 500.0, 60.0, 60.0);
        assert_eq!(dock_edge(right, MAIN), Some(Point::new(1799.0, 530.0)), "the right edge");
        assert_eq!(dock_edge(Rect::new(800.0, 1100.0, 60.0, 60.0), MAIN), None, "on screen: nothing to reveal");
    }

    #[test]
    fn a_pick_clicks_the_middle_of_a_fixtures_laid_out_window_under_mission_control() {
        let mut list = mission_control();
        list.push(win(7, 500, "SM Fixture", 0, Rect::new(100.0, 200.0, 300.0, 200.0)));
        list.push(win(8, 600, "Other", 0, Rect::new(600.0, 200.0, 300.0, 200.0)));
        let got = pick_point(&list, 7, &[500], 1, Some(MAIN), &never);
        assert_eq!(got, Ok((Point::new(250.0, 300.0), Rect::new(100.0, 200.0, 300.0, 200.0))));
    }

    #[test]
    fn a_pick_is_refused_without_mission_control_on_another_apps_window_or_for_another_apps_id() {
        let fixture = win(7, 500, "SM Fixture", 0, Rect::new(100.0, 200.0, 300.0, 200.0));
        assert!(pick_point(std::slice::from_ref(&fixture), 7, &[500], 1, Some(MAIN), &never).is_err(), "no Mission Control");
        let mut list = mission_control();
        list.push(win(8, 600, "Other", 0, Rect::new(150.0, 250.0, 300.0, 200.0)));
        list.push(fixture.clone());
        assert!(pick_point(&list, 7, &[500], 1, Some(MAIN), &never).is_err(), "another app's window at the point");
        assert!(pick_point(&list, 8, &[500], 1, Some(MAIN), &never).is_err(), "another app's window picked");
        let mut list = mission_control();
        list.push(fixture.clone());
        list.push(win(9, 700, "Banner", 25, Rect::new(200.0, 250.0, 300.0, 100.0)));
        assert!(pick_point(&list, 7, &[500], 1, Some(MAIN), &never).is_err(), "a banner over the point");
        assert!(pick_point(&list, 7, &[500], 1, Some(MAIN), &|_, _| true).is_ok(), "a click-through overlay");
        let mut list = mission_control();
        list.push(win(7, 500, "SM Fixture", 0, Rect::new(2000.0, 200.0, 300.0, 200.0)));
        assert!(pick_point(&list, 7, &[500], 1, Some(MAIN), &never).is_err(), "off the main display");
    }
}
