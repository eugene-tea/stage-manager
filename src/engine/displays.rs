//! Which display a window belongs to.
//!
//! Stages live on the main display only (README: "other displays are left alone"). The
//! platform asks [`on_other_display`] before it hands a window to the engine and once a
//! second for every managed window that shows: a window that shows more of itself on another
//! display than on the main one is not managed there (it waits, unmanaged, until it is back on
//! the main display). A window on no display at all (off screen, in a park zone's offscreen
//! part) belongs to the main display, so a window we parked never counts as elsewhere because
//! of its offscreen body.

use super::geometry::Rect;

/// True when `frame` shows more of itself on one of `others` than on `main` (ties go to the
/// main display).
pub fn on_other_display(frame: Rect, main: Rect, others: &[Rect]) -> bool {
    let area = |d: &Rect| frame.intersect(d).map_or(0.0, |r| r.w * r.h);
    let on_main = area(&main);
    others.iter().any(|d| area(d) > on_main)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);

    #[test]
    fn a_window_on_a_display_below_or_right_belongs_there() {
        // The review's case: a MacBook screen below a 5K main display.
        let below = Rect::new(500.0, 1440.0, 1512.0, 982.0);
        let mail = Rect::new(900.0, 1700.0, 1000.0, 800.0);
        assert!(on_other_display(mail, MAIN, &[below]));
        let right = Rect::new(2560.0, 0.0, 1920.0, 1080.0);
        assert!(on_other_display(Rect::new(3000.0, 200.0, 800.0, 600.0), MAIN, &[right]));
    }

    #[test]
    fn a_window_mostly_on_main_or_on_no_display_belongs_to_main() {
        let right = Rect::new(2560.0, 0.0, 1920.0, 1080.0);
        assert!(!on_other_display(Rect::new(2000.0, 100.0, 800.0, 600.0), MAIN, &[right]));
        assert!(!on_other_display(Rect::new(100.0, 100.0, 800.0, 600.0), MAIN, &[right]));
        // Parked: a sliver on main, the body below the main display where no display is.
        assert!(!on_other_display(Rect::new(2559.0, 1408.0, 600.0, 392.0), MAIN, &[right]));
        assert!(!on_other_display(Rect::new(9000.0, 9000.0, 100.0, 100.0), MAIN, &[right]));
        assert!(!on_other_display(Rect::new(100.0, 100.0, 800.0, 600.0), MAIN, &[]));
    }

    #[test]
    fn a_window_across_two_displays_goes_where_most_of_it_is() {
        let right = Rect::new(2560.0, 0.0, 1920.0, 1080.0);
        assert!(on_other_display(Rect::new(2460.0, 100.0, 800.0, 600.0), MAIN, &[right]));
        assert!(!on_other_display(Rect::new(2160.0, 100.0, 800.0, 600.0), MAIN, &[right]));
        // Exactly half and half: the main display keeps it.
        let narrow = Rect::new(2560.0, 0.0, 400.0, 1080.0);
        assert!(!on_other_display(Rect::new(2160.0, 100.0, 800.0, 600.0), MAIN, &[narrow]));
    }
}
