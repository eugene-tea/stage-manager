//! Where Mission Control's lends park a hidden window: at its stage's card (round
//! mc-card-park, the Spaces engine).
//!
//! Apple's Stage Manager starts a hidden stage's window's Mission Control flight at its strip
//! thumbnail, grows it over ~330 ms and settles it in a column on the left; ours, put home at
//! the park corner, first showed there and was laid out in a slot at the right
//! (`scripts/probe_apple.py mission-open` and `mission-open-ours`, fixtures, 11-20). Mission
//! Control flies a window put home from the frame it is listed at, and lays it out near where
//! that frame is. So the park of a window lent to Mission Control is at its card
//! ([`at_card`]), and the corner stays for every other lend
//! ([`super::lend_park::Zone::Card`]).
//!
//! **Where exactly.** A lend moves a window, it never resizes it (an app's window reflows its
//! content at a new size), so the frame Mission Control flies from is the window's own size.
//! Centered on the card, that frame is the card's thumbnail grown about its center, which is
//! where a flight out of the strip passes; its center is the card's, so the windows of the
//! strip are laid out in the strip's order, top to bottom. At the card's origin a big window's
//! center would sit half its size to the right and below, far into the stage area, and two
//! cards' windows of different heights could swap places.
//!
//! **Clamped onto its display's visible frame.** A window centered on a card at the left edge
//! sticks out past it: on a second display to the left it would show there, and macOS gives a
//! window to the display holding most of it (Mission Control lays it out on that one); above
//! the visible frame it would sit under the menu bar. A window larger than the visible frame
//! gets no card park: the corner, as before.

use super::geometry::{Point, Rect};

/// The park of a window at `frame` lent to Mission Control whose stage's card is at `card`
/// (global, top-left origin) on a display with `visible` as its visible frame: the window's
/// size centered on the card, moved as little as it takes to lie inside `visible`. `None` when
/// the window does not fit inside `visible` (it parks at the corner).
pub fn at_card(frame: Rect, card: Rect, visible: Rect) -> Option<Rect> {
    if frame.is_degenerate() || !card.is_finite() || visible.is_degenerate() || frame.w > visible.w || frame.h > visible.h {
        return None;
    }
    let centered = frame.centered_on(card.center());
    let x = centered.x.clamp(visible.x, visible.max_x() - frame.w);
    let y = centered.y.clamp(visible.y, visible.max_y() - frame.h);
    Some(frame.with_origin(Point { x, y }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture's display (2560x1440, the menu bar 25 pt) and Bravo's card, as measured.
    fn visible() -> Rect {
        Rect::new(0.0, 25.0, 2560.0, 1415.0)
    }

    fn card() -> Rect {
        Rect::new(16.0, 280.0, 76.0, 109.0)
    }

    /// A small window sits centered on its card, at its own size.
    #[test]
    fn a_window_is_centered_on_its_card_at_its_own_size() {
        let small = Rect::new(900.0, 500.0, 60.0, 80.0);
        let at = at_card(small, card(), visible()).expect("fits");
        assert_eq!((at.w, at.h), (60.0, 80.0), "never resized");
        assert_eq!(at.center(), card().center());
    }

    /// The fixture's window (520x424) is held inside the display at its left edge, its center
    /// level with the card's: Mission Control lays it out on the left.
    #[test]
    fn a_window_wider_than_the_strip_stays_on_its_display_level_with_its_card() {
        let w = Rect::new(2039.0, 984.0, 520.0, 424.0);
        let at = at_card(w, card(), visible()).expect("fits");
        assert_eq!(at, Rect::new(0.0, card().center().y - 212.0, 520.0, 424.0));
        assert!(visible().contains_rect(&at));
        assert!(at.center().x < visible().center().x, "on the left half");
    }

    /// Never under the menu bar nor past the bottom; on a second display, inside its own
    /// visible frame (never across onto the display beside it).
    #[test]
    fn the_park_stays_inside_its_displays_visible_frame() {
        let tall = Rect::new(400.0, 100.0, 700.0, 1300.0);
        let at = at_card(tall, Rect::new(16.0, 40.0, 76.0, 60.0), visible()).expect("fits");
        assert_eq!(at.y, 25.0, "below the menu bar");
        let low = at_card(tall, Rect::new(16.0, 1350.0, 76.0, 60.0), visible()).expect("fits");
        assert_eq!(low.max_y(), visible().max_y(), "above the bottom");
        let second = Rect::new(-1920.0, 0.0, 1920.0, 1080.0);
        let at = at_card(Rect::new(-1500.0, 200.0, 800.0, 600.0), Rect::new(-1904.0, 300.0, 70.0, 90.0), second).expect("fits");
        assert!(second.contains_rect(&at), "on its own display: {at}");
    }

    /// A window larger than the visible frame gets no card park (the corner then).
    #[test]
    fn a_window_too_big_for_the_display_parks_at_the_corner() {
        assert_eq!(at_card(Rect::new(0.0, 25.0, 2600.0, 800.0), card(), visible()), None);
        assert_eq!(at_card(Rect::new(0.0, 25.0, 800.0, 1500.0), card(), visible()), None);
        assert_eq!(at_card(Rect::new(f64::NAN, 25.0, 800.0, 600.0), card(), visible()), None);
    }
}
