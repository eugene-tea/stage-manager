//! Which display a window belongs to.
//!
//! Stages live on the main display only (docs/GUIDE.md: "other displays are left alone"). The
//! platform asks [`on_other_display`] before it hands a window to the engine and once a
//! second for every managed window that shows: a window that shows more of itself on another
//! display than on the main one is not managed there (it waits, unmanaged, until it is back on
//! the main display). A window on no display at all (off screen, in a park zone's offscreen
//! part) belongs to the main display, so a window we parked never counts as elsewhere because
//! of its offscreen body.
//!
//! **Off every display** ([`off_screen_not_ours`], round offscreen-windows, 11-03): a window
//! that overlaps no display at all is not the user's. Apps put such windows there themselves
//! (a headless browser's: Playwright's WebKit lists an untitled `AXUnknown` window at
//! `-10000,-10180`; offscreen render windows), and no user can drag a window there (macOS keeps
//! a title bar on screen). Apple's Stage Manager leaves them alone, so they are never managed:
//! no stage, no card, never parked, moved or saved. The platform's one gate for "shown on the
//! main display" (`on_main_screen` in the controller) leaves them out, so every path that
//! takes a window in (a new window, the launch listing, an arrival on a Desktop, the restore
//! that rides on them) waits for one to show on a display, and then takes it as a new window.
//! A managed window its app moves off every display is let go where it is, never brought
//! back (the controller's off-screen check), except one we parked: a park zone's frame may lie
//! wholly beyond the edges (`[windows] park_inside` below 0).

use super::geometry::Rect;
use super::park::is_parked_anywhere;

/// True when `frame` shows more of itself on one of `others` than on `main` (ties go to the
/// main display).
pub fn on_other_display(frame: Rect, main: Rect, others: &[Rect]) -> bool {
    let area = |d: &Rect| frame.intersect(d).map_or(0.0, |r| r.w * r.h);
    let on_main = area(&main);
    others.iter().any(|d| area(d) > on_main)
}

/// True when `frame` overlaps none of `displays` (every active display's full frame, global
/// top-left points): not one point of the window can show on any screen. A frame that only
/// touches a display's edge shows nothing of itself there. Nothing known (`displays` empty)
/// is never off every display.
pub fn off_every_display(frame: Rect, displays: &[Rect]) -> bool {
    !displays.is_empty() && !displays.iter().any(|d| frame.intersect(d).is_some())
}

/// True when a window the window server lists at `frame` is off every display and so not the
/// user's (module docs): never taken in, and let go when managed. `displays` is every active
/// display, the main one first; `managed`: the model manages the window, whose frame in a
/// park zone of the main display is ours (a parked window, wholly beyond the edges with a
/// negative `park_inside`), never judged here. Only where the window is counts: an untitled
/// `AXUnknown` window on a display is managed as before (a child, by Apple's rule); one that
/// comes onto a display by itself is taken in then, as a new window.
pub fn off_screen_not_ours(frame: Rect, displays: &[Rect], managed: bool) -> bool {
    let Some(main) = displays.first() else { return false };
    off_every_display(frame, displays) && !(managed && is_parked_anywhere(frame, *main))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's 11-03 log: a headless Playwright WebKit window (untitled, `AXUnknown`).
    const HEADLESS: Rect = Rect::new(-10_000.0, -10_180.0, 1440.0, 900.0);
    const RIGHT: Rect = Rect::new(2560.0, 0.0, 1920.0, 1080.0);

    #[test]
    fn a_window_off_every_display_is_not_the_users() {
        assert!(off_screen_not_ours(HEADLESS, &[MAIN], false));
        assert!(off_screen_not_ours(HEADLESS, &[MAIN, RIGHT], false));
        // Its app moved a managed one there: let go too.
        assert!(off_screen_not_ours(HEADLESS, &[MAIN, RIGHT], true));
        // Touching the left edge shows nothing of it.
        assert!(off_screen_not_ours(Rect::new(-800.0, 100.0, 800.0, 600.0), &[MAIN], false));
        // Past the right edge with no display there; on the display there with one.
        let beyond = Rect::new(2600.0, 100.0, 800.0, 600.0);
        assert!(off_screen_not_ours(beyond, &[MAIN], false));
        assert!(!off_screen_not_ours(beyond, &[MAIN, RIGHT], false));
    }

    #[test]
    fn a_window_partly_on_a_display_is_the_users() {
        // One point of it on the main display is enough.
        assert!(!off_screen_not_ours(Rect::new(-799.0, 100.0, 800.0, 600.0), &[MAIN], false));
        assert!(!off_screen_not_ours(Rect::new(2000.0, 1400.0, 800.0, 600.0), &[MAIN], false));
        // Wholly on another display: that display's rule ([`on_other_display`]), not this one.
        assert!(!off_screen_not_ours(Rect::new(3000.0, 100.0, 800.0, 600.0), &[MAIN, RIGHT], false));
        // Nothing known about the displays: no judgement.
        assert!(!off_screen_not_ours(HEADLESS, &[], false));
    }

    #[test]
    fn an_untitled_axunknown_window_counts_only_by_where_it_is() {
        // The rule reads the frame alone: the same untitled `AXUnknown` window is left alone
        // at the headless spot and managed (a child, by Apple's rule) on the display.
        assert!(off_screen_not_ours(HEADLESS, &[MAIN], false));
        assert!(!off_screen_not_ours(Rect::new(560.0, 285.0, 1440.0, 900.0), &[MAIN], false));
    }

    #[test]
    fn a_window_that_comes_on_screen_later_is_taken_in_then() {
        let seen = [HEADLESS, Rect::new(-10_000.0, -10_180.0, 1440.0, 900.0), Rect::new(100.0, 100.0, 1440.0, 900.0)];
        let taken: Vec<bool> = seen.iter().map(|f| !off_screen_not_ours(*f, &[MAIN], false)).collect();
        assert_eq!(taken, vec![false, false, true]);
    }

    #[test]
    fn a_window_we_parked_beyond_the_edges_stays_ours() {
        // `park_inside` below 0: the park frame lies wholly past the right edge.
        let parked = Rect::new(2561.0, 1407.0, 800.0, 600.0);
        assert!(off_every_display(parked, &[MAIN]));
        assert!(!off_screen_not_ours(parked, &[MAIN], true));
        // Never managed: the safety paths' park-zone rules decide, not the model.
        assert!(off_screen_not_ours(parked, &[MAIN], false));
        // Top-left corner, the body off the left side.
        assert!(!off_screen_not_ours(Rect::new(-801.0, 40.0, 800.0, 600.0), &[MAIN], true));
    }

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
