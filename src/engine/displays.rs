//! Which display a window belongs to, and a display's stable identity.
//!
//! **A strip and stages on every screen** (round displays, 11-05), as Apple's Stage Manager
//! gives every screen its own strip and stages. A window belongs to the display that shows
//! most of it ([`display_of`]: macOS's own rule for a window straddling two screens; ties go
//! to the first display listed, the main one). The platform asks it before it hands a window
//! to the engine and once a second for every managed window that shows: a window that shows
//! more of itself on another display than on its own has moved there
//! ([`crate::engine::model::Model::windows_moved_to_display`]). A window on no display at all
//! (off screen, in a park zone's offscreen part) keeps the display it had, so a window we
//! parked never counts as elsewhere because of its offscreen body.
//!
//! **Main screen only** (`[displays] main_only`, off): the first version's behaviour, kept
//! for those who want exactly that (FEATURE-REQUESTS R8): only the main screen is managed,
//! and a window that shows more of itself on another display ([`on_other_display`]) is not
//! managed there (it waits, unmanaged, until it is back on the main display).
//!
//! **A display's identity** ([`ScreenKey`]): `CGDirectDisplayID` changes when a display is
//! plugged in again, so stages and `state.json` are keyed by the display's vendor, model and
//! serial number instead (plus a count for identical displays with no serial), which survive
//! reconnects and restarts.
//!
//! **Off every display** ([`off_screen_not_ours`], round offscreen-windows, 11-03): a window
//! that overlaps no display at all is not the user's. Apps put such windows there themselves
//! (a headless browser's: Playwright's WebKit lists an untitled `AXUnknown` window at
//! `-10000,-10180`; offscreen render windows), and no user can drag a window there (macOS keeps
//! a title bar on screen). Apple's Stage Manager leaves them alone, so they are never managed:
//! no stage, no card, never parked, moved or saved. The platform's one gate for "shown on a
//! managed screen" (`on_managed_screens` in the controller) leaves them out, so every path
//! that takes a window in (a new window, the launch listing, an arrival on a Desktop, the
//! restore that rides on them) waits for one to show on a display, and then takes it as a new
//! window. A managed window its app moves off every display is let go where it is, never
//! brought back (the controller's off-screen check), except one we parked: a park zone's frame
//! may lie wholly beyond the edges (`[windows] park_inside` below 0).

use serde::{Deserialize, Serialize};

use super::geometry::Rect;
use super::model::DisplayId;
use super::park::is_parked_anywhere;

/// A display's identity that survives reconnects and restarts: its vendor, model and serial
/// number as CoreGraphics reports them, and `nth`, its place among the connected displays
/// that report the same three (identical displays with no serial number), 0 for the first.
/// [`ScreenKey::NONE`] when the platform could not tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub struct ScreenKey {
    /// `CGDisplayVendorNumber`.
    pub vendor: u32,
    /// `CGDisplayModelNumber`.
    pub model: u32,
    /// `CGDisplaySerialNumber` (0 for many displays).
    pub serial: u32,
    /// Its place among the connected displays with the same vendor, model and serial.
    pub nth: u32,
}

impl ScreenKey {
    /// No identity known (a test display, a platform that reported none).
    pub const NONE: ScreenKey = ScreenKey { vendor: 0, model: 0, serial: 0, nth: 0 };

    /// A key with the numbers given, the first of its kind.
    pub const fn new(vendor: u32, model: u32, serial: u32) -> Self {
        Self { vendor, model, serial, nth: 0 }
    }

    /// True when the key names a display (not [`ScreenKey::NONE`]).
    pub fn known(&self) -> bool {
        *self != Self::NONE
    }

    /// The key as `state.json` writes it: `vendor:model:serial:nth`, hex numbers.
    pub fn text(&self) -> String {
        format!("{:x}:{:x}:{:x}:{}", self.vendor, self.model, self.serial, self.nth)
    }

    /// A key read back from [`ScreenKey::text`]; `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let vendor = u32::from_str_radix(parts.next()?, 16).ok()?;
        let model = u32::from_str_radix(parts.next()?, 16).ok()?;
        let serial = u32::from_str_radix(parts.next()?, 16).ok()?;
        let nth = parts.next()?.parse().ok()?;
        parts.next().is_none().then_some(Self { vendor, model, serial, nth })
    }
}

/// Gives every display of `keys` (in connection order) its `nth` among those with the same
/// vendor, model and serial: the first keeps 0, the next is 1, and so on. Unknown keys
/// ([`ScreenKey::NONE`]) stay as they are.
pub fn number_duplicates(keys: &mut [ScreenKey]) {
    for i in 0..keys.len() {
        if !keys[i].known() {
            continue;
        }
        let same = |k: &ScreenKey| (k.vendor, k.model, k.serial) == (keys[i].vendor, keys[i].model, keys[i].serial);
        keys[i].nth = keys[..i].iter().filter(|k| same(k)).count() as u32;
    }
}

/// The display (its id and frame, in the order given, the main one first) that shows most of
/// `frame`: macOS's rule for a window across two screens. Ties go to the earlier display.
/// `None` when no display shows one point of it (then the window keeps the display it had).
pub fn display_of(frame: Rect, displays: &[(DisplayId, Rect)]) -> Option<DisplayId> {
    let mut best: Option<(DisplayId, f64)> = None;
    for (id, d) in displays {
        let area = frame.intersect(d).map_or(0.0, |r| r.w * r.h);
        if area > 0.0 && best.is_none_or(|(_, a)| area > a) {
            best = Some((*id, area));
        }
    }
    best.map(|(id, _)| id)
}

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
    fn a_screen_key_round_trips_through_its_text_and_numbers_twins() {
        let k = ScreenKey::new(0x610, 0xae2f, 0x4d6);
        assert_eq!(ScreenKey::parse(&k.text()), Some(k));
        assert_eq!(k.text(), "610:ae2f:4d6:0");
        assert!(k.known() && !ScreenKey::NONE.known());
        assert_eq!(ScreenKey::parse("nonsense"), None);
        assert_eq!(ScreenKey::parse("1:2:3:4:5"), None);
        // Two identical displays with no serial: the second is told from the first.
        let twin = ScreenKey::new(0x1234, 0x5678, 0);
        let mut keys = [twin, ScreenKey::new(0x610, 0xae2f, 0x4d6), twin, ScreenKey::NONE];
        number_duplicates(&mut keys);
        assert_eq!(keys.iter().map(|k| k.nth).collect::<Vec<_>>(), [0, 0, 1, 0]);
        assert!(!keys[3].known());
    }

    #[test]
    fn a_window_belongs_to_the_display_showing_most_of_it() {
        // The owner's arrangement: the Studio Display main, the MacBook screen to its left.
        let studio = (DisplayId(1), MAIN);
        let laptop = (DisplayId(2), Rect::new(-1800.0, 271.0, 1800.0, 1169.0));
        let both = [studio, laptop];
        assert_eq!(display_of(Rect::new(100.0, 100.0, 800.0, 600.0), &both), Some(DisplayId(1)));
        assert_eq!(display_of(Rect::new(-1500.0, 400.0, 800.0, 600.0), &both), Some(DisplayId(2)));
        // Straddling the seam: most of it on the laptop, then most of it on the Studio.
        assert_eq!(display_of(Rect::new(-500.0, 400.0, 800.0, 600.0), &both), Some(DisplayId(2)));
        assert_eq!(display_of(Rect::new(-300.0, 400.0, 800.0, 600.0), &both), Some(DisplayId(1)));
        // Exactly half and half: the main display (listed first).
        assert_eq!(display_of(Rect::new(-400.0, 400.0, 800.0, 600.0), &both), Some(DisplayId(1)));
        // Parked beyond the Studio's right edge, or headless: no display shows it.
        assert_eq!(display_of(Rect::new(2561.0, 1407.0, 800.0, 600.0), &both), None);
        assert_eq!(display_of(HEADLESS, &both), None);
        assert_eq!(display_of(Rect::new(100.0, 100.0, 800.0, 600.0), &[]), None);
    }

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
