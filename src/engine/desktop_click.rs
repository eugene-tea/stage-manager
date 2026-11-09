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

    #[test]
    fn unknown_icons_are_not_guessed() {
        assert_eq!(desktop_press(Point::new(2432.0, 150.0), Icons::Unknown), DesktopPress::Wallpaper);
        assert_eq!(desktop_press(Point::new(2432.0, 150.0), Icons::Read(&[])), DesktopPress::Wallpaper, "an empty desktop");
    }
}
