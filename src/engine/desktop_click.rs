//! Whether a click on the desktop is a click on the bare wallpaper or on one of Finder's
//! desktop icons.
//!
//! "Click wallpaper to reveal desktop" counts only clicks on bare wallpaper on Apple's: a
//! press on a file or folder on the desktop selects it, and a drag from it moves it (onto a
//! window, say), the stage staying where it is. Ours took every press with no window under
//! it for a wallpaper click (11-09: a file pressed to drag it onto a window sent the stage
//! to the strip). The icons are the ones read for the corner patch, each grown by its label
//! ([`super::corner_patch::item_reach`]), here a few points more for a press at its edge
//! ([`ICON_CLICK_SLOP`]).
//!
//! Nor is a press just outside a window: macOS gives every resizable window a band a few
//! points wide outside its frame where a press takes the window's edge (11-09: VS Code's right
//! edge pressed 0.3 pt outside its frame, taken for a wallpaper click; the window parked while
//! he kept dragging and was resized to its minimum in the park corner, folding its panels).
//! A press in that band ([`EdgeZone::Band`]) is the window's, never a wallpaper click. One a
//! little further out ([`EdgeZone::Near`]) may still be the edge where the band is wider than
//! known: its reveal waits for the release, and only a release where the press was
//! ([`still_release`]) reveals; a press that turns into a drag (a resize, a rubber-band
//! selection) reveals nothing. A press clear of every window reveals on the press, at once.

use super::geometry::{Point, Rect};

/// How far past an icon's reach (its image and label) a press still counts as on the icon.
pub const ICON_CLICK_SLOP: f64 = 4.0;

/// Where Finder's desktop icons lie, as far as known for a click.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Icons<'a> {
    /// A read of them (grown by their labels).
    Read(&'a [Rect]),
    /// None could be read (Finder not answering, more icons than are read): nothing is
    /// guessed, a press with no window under it reveals as before.
    Unknown,
}

/// What a press with no window under it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopPress {
    /// On the bare wallpaper (or where the icons are not known): the wallpaper click.
    Wallpaper,
    /// On a desktop icon or its label: Finder's (select, drag, open); nothing is revealed.
    Icon,
}

/// How far outside a window's edge macOS's resize band reaches, along the edge. Heger's
/// measure of the corner's 19 × 19 target, 62 % of it inside a square-cornered window,
/// puts it about 4 pt outside the frame; this is a little more.
pub const EDGE_BAND: f64 = 6.0;

/// How far outside a corner the band reaches: there the target is a square of its own,
/// larger than the edge's strip ([`CORNER_SPAN`] of the corner, along each edge).
pub const CORNER_BAND: f64 = 10.0;

/// How close to a corner, along an edge, a press is in the corner's target.
pub const CORNER_SPAN: f64 = 16.0;

/// How far outside every window's frame a press must be for its reveal to run on the press
/// itself; closer, the reveal waits for a still release.
pub const NEAR_EDGE: f64 = 16.0;

/// How far the pointer may move between the press and the release of a click (a person's
/// hand); further, it was a drag.
pub const STILL: f64 = 4.0;

/// Where a press with no window under it lies against the windows' edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeZone {
    /// In a window's resize band: the window's edge, no wallpaper click.
    Band,
    /// Just past the band: a wallpaper click only on a still release.
    Near,
    /// Clear of every window: the wallpaper click, on the press.
    Clear,
}

/// How far `p` lies outside `frame` horizontally and vertically (0 along an axis where it is
/// within the frame's span).
fn outside(p: Point, frame: Rect) -> (f64, f64) {
    let dx = (frame.x - p.x).max(p.x - (frame.x + frame.w)).max(0.0);
    let dy = (frame.y - p.y).max(p.y - (frame.y + frame.h)).max(0.0);
    (dx, dy)
}

/// True when a press at `p` lies in `frame`'s resize band: outside it, within [`EDGE_BAND`]
/// of an edge, or within [`CORNER_BAND`] of it near a corner ([`CORNER_SPAN`] along either
/// edge). A press inside the frame is on the window itself, not in its band.
pub fn in_resize_band(p: Point, frame: Rect) -> bool {
    let (dx, dy) = outside(p, frame);
    if dx == 0.0 && dy == 0.0 {
        return false;
    }
    let along_x = (p.x - frame.x).abs().min((p.x - (frame.x + frame.w)).abs());
    let along_y = (p.y - frame.y).abs().min((p.y - (frame.y + frame.h)).abs());
    let band = if along_x <= CORNER_SPAN && along_y <= CORNER_SPAN { CORNER_BAND } else { EDGE_BAND };
    dx <= band && dy <= band
}

/// Where a press at `p` with no window under it lies against `frames`, every real window's
/// frame (any app's, shown anywhere on screen, not only the front stage's).
pub fn edge_zone(p: Point, frames: impl IntoIterator<Item = Rect>) -> EdgeZone {
    let mut zone = EdgeZone::Clear;
    for f in frames {
        if in_resize_band(p, f) {
            return EdgeZone::Band;
        }
        let (dx, dy) = outside(p, f);
        if dx <= NEAR_EDGE && dy <= NEAR_EDGE {
            zone = EdgeZone::Near;
        }
    }
    zone
}

/// True when a release at `up` ends a click pressed at `down`, not a drag: the pointer
/// within [`STILL`] of the press.
pub fn still_release(down: Point, up: Point) -> bool {
    (up.x - down.x).hypot(up.y - down.y) <= STILL
}

/// What a press at `p` with no window under it is, the icons as known (`icons`).
pub fn desktop_press(p: Point, icons: Icons) -> DesktopPress {
    let on_icon = match icons {
        Icons::Read(icons) => icons.iter().any(|r| {
            Rect::new(r.x - ICON_CLICK_SLOP, r.y - ICON_CLICK_SLOP, r.w + 2.0 * ICON_CLICK_SLOP, r.h + 2.0 * ICON_CLICK_SLOP).contains(p)
        }),
        Icons::Unknown => false,
    };
    if on_icon { DesktopPress::Icon } else { DesktopPress::Wallpaper }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::corner_patch::item_reach;

    #[test]
    fn a_press_on_a_desktop_file_or_its_label_is_no_wallpaper_click() {
        // A file's icon image at 2400,120 (64 pt), its label below it.
        let icons = [item_reach(Rect::new(2400.0, 120.0, 64.0, 64.0))];
        let icons = Icons::Read(&icons);
        assert_eq!(desktop_press(Point::new(2432.0, 150.0), icons), DesktopPress::Icon, "the image");
        assert_eq!(desktop_press(Point::new(2432.0, 200.0), icons), DesktopPress::Icon, "its label, below it");
        assert_eq!(desktop_press(Point::new(2370.0, 150.0), icons), DesktopPress::Icon, "a long label's edge, beside it");
        assert_eq!(desktop_press(Point::new(2432.0, 230.0), icons), DesktopPress::Icon, "just past the reach: the slop");
        assert_eq!(desktop_press(Point::new(2432.0, 260.0), icons), DesktopPress::Wallpaper, "bare wallpaper below it");
        assert_eq!(desktop_press(Point::new(1200.0, 700.0), icons), DesktopPress::Wallpaper, "bare wallpaper elsewhere");
        // A drag that starts on the file starts with this press: nothing reveals either.
    }

    /// 11-09: VS Code's frame 315,58 2055 × 1263 (right edge at x 2370), pressed at x 2370.3.
    #[test]
    fn a_press_just_outside_a_windows_edge_is_the_edge() {
        let code = Rect::new(315.0, 58.0, 2055.0, 1263.0);
        assert!(in_resize_band(Point::new(2370.296875, 74.01171875), code), "his press, 0.3 pt out");
        assert!(in_resize_band(Point::new(2375.5, 700.0), code), "5.5 pt right of the edge");
        assert!(!in_resize_band(Point::new(2377.0, 700.0), code), "7 pt right, mid edge: past the band");
        assert!(in_resize_band(Point::new(2379.0, 66.0), code), "9 pt right, near the top corner: the corner's target");
        assert!(in_resize_band(Point::new(2378.0, 50.0), code), "out past the corner, both ways");
        assert!(!in_resize_band(Point::new(2382.0, 50.0), code), "12 pt out past the corner");
        assert!(in_resize_band(Point::new(310.0, 700.0), code), "left edge");
        assert!(in_resize_band(Point::new(1200.0, 1324.0), code), "bottom edge");
        assert!(in_resize_band(Point::new(1200.0, 54.0), code), "top edge");
        assert!(!in_resize_band(Point::new(1200.0, 700.0), code), "on the window itself: not its band");
    }

    #[test]
    fn a_press_is_zoned_against_every_window() {
        let front = Rect::new(315.0, 58.0, 2055.0, 1263.0);
        let other = Rect::new(2440.0, 300.0, 100.0, 400.0);
        let frames = [front, other];
        assert_eq!(edge_zone(Point::new(2370.3, 74.0), frames), EdgeZone::Band, "the front window's edge");
        assert_eq!(edge_zone(Point::new(2437.0, 500.0), frames), EdgeZone::Band, "another window's edge, not the front stage's");
        assert_eq!(edge_zone(Point::new(2380.0, 700.0), frames), EdgeZone::Near, "10 pt out: the reveal waits for the release");
        assert_eq!(edge_zone(Point::new(2405.0, 700.0), frames), EdgeZone::Clear, "35 pt out: on the press");
        assert_eq!(edge_zone(Point::new(1200.0, 1400.0), frames), EdgeZone::Clear);
        assert_eq!(edge_zone(Point::new(1200.0, 1400.0), []), EdgeZone::Clear, "no window at all");
    }

    #[test]
    fn a_click_releases_where_it_pressed_a_drag_does_not() {
        let down = Point::new(2380.0, 700.0);
        assert!(still_release(down, down));
        assert!(still_release(down, Point::new(2382.0, 702.0)), "a hand's wobble");
        assert!(!still_release(down, Point::new(2386.0, 700.0)), "a short drag");
        assert!(!still_release(down, Point::new(2380.0, 900.0)), "a rubber-band selection");
    }

    #[test]
    fn unknown_icons_are_not_guessed() {
        assert_eq!(desktop_press(Point::new(2432.0, 150.0), Icons::Unknown), DesktopPress::Wallpaper);
        assert_eq!(desktop_press(Point::new(2432.0, 150.0), Icons::Read(&[])), DesktopPress::Wallpaper, "an empty desktop");
    }
}
