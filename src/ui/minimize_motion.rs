//! Where the strip's picture of a minimized window starts on its way to the strip
//! ([`crate::ui::strip_model::Minimized`]).
//!
//! macOS's genie has put the window in the Dock before Stage Manager hears of the minimize,
//! so the picture never starts at the window's old frame (11-09: a full-size picture there,
//! after the genie, was a second motion and a third with the window's own un-minimize). It
//! comes out of the window's Dock tile, the size the genie left the window at (the window's
//! shape fitted in the tile), and flies to its card on the shrink spring: the genie and the
//! flight read as one motion. Without a tile on this strip's screen, or with no picture of the
//! window, its card fades in where it lands; under Reduce Motion the card's fade is all.

use crate::engine::geometry::{Rect, Size};

/// How a minimized window's card comes into the strip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Start {
    /// A picture flies from here (global, top-left): the window's shape fitted in its tile.
    Tile(Rect),
    /// The card fades in at its slot; no picture moves.
    Fade,
}

/// The start for a window of `frame`'s shape whose Dock tile is `tile` (`None`: not read),
/// with a picture of it at hand (`picture`), on a strip whose overlay covers `screen`;
/// `reduced`: Reduce Motion.
pub fn start(tile: Option<Rect>, frame: Size, picture: bool, reduced: bool, screen: Rect) -> Start {
    match tile {
        Some(t) if !reduced && picture && !t.is_degenerate() && screen.contains(t.center()) && frame.w > 0.0 && frame.h > 0.0 => {
            Start::Tile(fitted(t, frame))
        }
        _ => Start::Fade,
    }
}

/// A window of `frame`'s shape fitted inside `tile`, centred on it: the picture's start.
pub fn fitted(tile: Rect, frame: Size) -> Rect {
    let k = (tile.w / frame.w).min(tile.h / frame.h);
    Rect::new(0.0, 0.0, frame.w * k, frame.h * k).centered_on(tile.center())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    /// The probe fixture's app icon in the owner's bottom Dock (11-09).
    const TILE: Rect = Rect::new(1899.0, 1351.0, 68.0, 84.0);
    const VS_CODE: Size = Size::new(1914.0, 1258.0);

    #[test]
    fn the_picture_comes_out_of_the_dock_tile() {
        let Start::Tile(r) = start(Some(TILE), VS_CODE, true, false, SCREEN) else { panic!("a tile start") };
        assert!(TILE.contains_rect(&r), "inside the tile: {r}");
        assert!((r.w - TILE.w).abs() < 1e-9, "as wide as the tile (the window is wider than tall)");
        assert!((r.w / r.h - VS_CODE.w / VS_CODE.h).abs() < 1e-9, "the window's shape");
        assert!((r.center().x - TILE.center().x).abs() < 1e-9 && (r.center().y - TILE.center().y).abs() < 1e-9);
    }

    #[test]
    fn a_tall_window_fits_by_its_height() {
        let r = fitted(TILE, Size::new(400.0, 1000.0));
        assert!((r.h - TILE.h).abs() < 1e-9 && r.w < TILE.w);
    }

    #[test]
    fn without_a_tile_on_this_screen_the_card_fades_in() {
        assert_eq!(start(None, VS_CODE, true, false, SCREEN), Start::Fade, "not read");
        assert_eq!(start(Some(TILE.translate(-3000.0, 0.0)), VS_CODE, true, false, SCREEN), Start::Fade, "another screen's Dock");
        assert_eq!(start(Some(Rect::default()), VS_CODE, true, false, SCREEN), Start::Fade, "an empty frame");
        assert_eq!(start(Some(TILE), VS_CODE, false, false, SCREEN), Start::Fade, "no picture of the window");
        assert_eq!(start(Some(TILE), Size::default(), true, false, SCREEN), Start::Fade, "no frame");
    }

    #[test]
    fn under_reduce_motion_only_the_card_fades() {
        assert_eq!(start(Some(TILE), VS_CODE, true, true, SCREEN), Start::Fade);
    }
}
