//! A minimized window on its way to the strip (`[windows] minimize_to_strip`, Apple's way):
//! where it is put while it is still in the Dock, so that macOS's un-minimize never shows it
//! crossing the screen.
//!
//! macOS posts `AXWindowMiniaturized` once its genie has put the window in the Dock. The
//! window then has to be un-minimized to be parked (a parked window is a window on screen in
//! the corner, never one in the Dock), and the un-minimize always plays macOS's animation out
//! of the window's Dock tile toward the window's frame. Measured 11-09 with a probe on a
//! fixture window (the owner's 5K, a bottom Dock that auto-hides and minimizes into the app's
//! icon; the window server's bounds every 4 ms):
//!
//! - `AXPosition` on a minimized window is taken and kept: the window server lists the
//!   window there, off screen, until the un-minimize, which goes straight to it.
//! - The un-minimize animates whatever the frame: toward a frame on screen, toward one off
//!   every display, and with the app hidden (the window shows; hiding hides nothing of it).
//! - Toward the park frame (its top a point above the bottom edge, as before 11-09) the
//!   window rises out of the tile to its full height and slides along the bottom into the
//!   corner: a 1600×1132 window reached 230 pt from the top of the 1440 pt screen (the
//!   owner's VS Code on 11-09: 366 pt). That was the second motion he saw.
//! - Toward the park corner's column 2000 pt below the screen it never rises above the
//!   tile's top: it sinks out of the tile and is below the screen 0.2 s later, within the
//!   Dock's band. AppKit then sets it down where it keeps a title bar's height on screen,
//!   in the corner (2559,1408 for a park at 2559,1439, where the park itself lands), and
//!   the controller parks it again at once ([`crate::platform::observe::WinEvent::Deminiaturized`]).
//!
//! So the window is put beyond the screen edge the Dock is on, in line with its park frame
//! ([`dive_frame`]): its un-minimize sinks into that edge and comes up in the corner.

use crate::engine::geometry::Rect;

/// How far past every display a minimized window is put before it is un-minimized
/// (points): far enough that the un-minimize leaves the screen within the Dock's band
/// (measured 11-09: 2000 pt, below the screen 0.2 s into the animation).
pub const DIVE_DEPTH: f64 = 2000.0;

/// The screen edge the Dock is on, as a tile's place on its screen tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockEdge {
    Bottom,
    Left,
    Right,
}

/// The edge of `screen` nearest the middle of `tile` (the window's Dock tile, or the Dock's
/// bar): the Dock's edge. `None` when the tile is not on `screen` or is nearest its top,
/// where the Dock never is.
pub fn dock_edge(tile: Rect, screen: Rect) -> Option<DockEdge> {
    let c = tile.center();
    if tile.is_degenerate() || screen.is_degenerate() || !screen.contains(c) {
        return None;
    }
    let to_bottom = screen.max_y() - c.y;
    let to_left = c.x - screen.x;
    let to_right = screen.max_x() - c.x;
    let to_top = c.y - screen.y;
    let nearest = to_bottom.min(to_left).min(to_right);
    if to_top < nearest {
        None
    } else if nearest == to_bottom {
        Some(DockEdge::Bottom)
    } else if nearest == to_left {
        Some(DockEdge::Left)
    } else {
        Some(DockEdge::Right)
    }
}

/// Where a minimized window whose park frame is `park` is put before its un-minimize: the
/// park frame moved beyond `screen`'s Dock edge (the edge `tile` is at), [`DIVE_DEPTH`] past
/// every display in `displays`, keeping its place along that edge. `None` (the window is
/// un-minimized toward `park`, as before) when the Dock's edge is unknown, or when the way
/// from the screen's edge out to that frame passes another display: the window would show
/// crossing it.
pub fn dive_frame(park: Rect, tile: Rect, screen: Rect, displays: &[Rect]) -> Option<Rect> {
    let edge = dock_edge(tile, screen)?;
    let all = displays.iter().fold(screen, |u, d| u.union(d));
    let (target, way) = match edge {
        DockEdge::Bottom => {
            let t = Rect::new(park.x, all.max_y() + DIVE_DEPTH, park.w, park.h);
            let (x, max_x) = (tile.x.min(t.x), tile.max_x().max(t.max_x()));
            (t, Rect::new(x, screen.max_y(), max_x - x, t.max_y() - screen.max_y()))
        }
        DockEdge::Right => {
            let t = Rect::new(all.max_x() + DIVE_DEPTH, park.y, park.w, park.h);
            let (y, max_y) = (tile.y.min(t.y), tile.max_y().max(t.max_y()));
            (t, Rect::new(screen.max_x(), y, t.max_x() - screen.max_x(), max_y - y))
        }
        DockEdge::Left => {
            let t = Rect::new(all.x - DIVE_DEPTH - park.w, park.y, park.w, park.h);
            let (y, max_y) = (tile.y.min(t.y), tile.max_y().max(t.max_y()));
            (t, Rect::new(t.x, y, screen.x - t.x, max_y - y))
        }
    };
    let crosses = displays.iter().filter(|d| **d != screen).any(|d| d.intersect(&way).is_some());
    (!crosses && park.is_finite()).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's 5K and the laptop screen to its left (11-09).
    const MAIN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    const LAPTOP: Rect = Rect::new(-1800.0, 0.0, 1800.0, 1169.0);
    /// The probe fixture's app icon in the owner's bottom Dock.
    const TILE: Rect = Rect::new(1899.0, 1351.0, 68.0, 84.0);

    #[test]
    fn the_docks_edge_is_the_one_its_tile_sits_at() {
        assert_eq!(dock_edge(TILE, MAIN), Some(DockEdge::Bottom));
        assert_eq!(dock_edge(Rect::new(4.0, 600.0, 68.0, 68.0), MAIN), Some(DockEdge::Left));
        assert_eq!(dock_edge(Rect::new(2488.0, 600.0, 68.0, 68.0), MAIN), Some(DockEdge::Right));
        assert_eq!(dock_edge(Rect::new(1200.0, 2.0, 68.0, 68.0), MAIN), None, "no Dock at the top");
        assert_eq!(dock_edge(TILE.translate(-3000.0, 0.0), MAIN), None, "a tile on another screen");
        assert_eq!(dock_edge(Rect::default(), MAIN), None, "no tile read");
    }

    #[test]
    fn a_bottom_dock_sends_the_window_down_in_its_park_column() {
        // Measured 11-09: the park at 2559,1439, the dive at 2559,3440.
        let park = Rect::new(2559.0, 1439.0, 1600.0, 1132.0);
        let d = dive_frame(park, TILE, MAIN, &[MAIN, LAPTOP]).expect("a dive");
        assert_eq!(d, Rect::new(2559.0, 1440.0 + DIVE_DEPTH, 1600.0, 1132.0));
        assert!(MAIN.intersect(&d).is_none() && LAPTOP.intersect(&d).is_none(), "off every display");
    }

    #[test]
    fn a_side_dock_sends_it_out_sideways_at_its_park_height() {
        let park = Rect::new(2559.0, 1439.0, 800.0, 600.0);
        let right = dive_frame(park, Rect::new(2488.0, 600.0, 68.0, 68.0), MAIN, &[MAIN]).expect("right");
        assert_eq!((right.x, right.y), (2560.0 + DIVE_DEPTH, 1439.0));
        let left = dive_frame(park, Rect::new(4.0, 600.0, 68.0, 68.0), MAIN, &[MAIN]).expect("left");
        assert_eq!((left.x, left.y), (-DIVE_DEPTH - 800.0, 1439.0));
    }

    #[test]
    fn no_dive_across_another_display() {
        let park = Rect::new(2559.0, 1439.0, 1600.0, 1132.0);
        // A screen below the 5K: the window would sink across it.
        let below = Rect::new(1000.0, 1440.0, 1800.0, 1169.0);
        assert_eq!(dive_frame(park, TILE, MAIN, &[MAIN, below]), None);
        // A screen to the left, as the owner's laptop: out of the way.
        assert!(dive_frame(park, TILE, MAIN, &[MAIN, LAPTOP]).is_some());
        // A left Dock with the laptop on that side: none.
        let left_tile = Rect::new(4.0, 600.0, 68.0, 68.0);
        assert_eq!(dive_frame(park, left_tile, MAIN, &[MAIN, LAPTOP]), None);
        // The Dock's edge unknown: none (the un-minimize goes to the park frame).
        assert_eq!(dive_frame(park, Rect::default(), MAIN, &[MAIN]), None);
    }
}
