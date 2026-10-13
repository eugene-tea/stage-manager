//! Park-zone math: where inactive stages hide, and how to bring windows back safely.
//!
//! Inactive stages are hidden by moving their windows to the bottom-right corner of their
//! display. macOS refuses to move a window entirely offscreen and clamps it so a sliver stays
//! visible; that sliver is expected. A window that never comes back from the park zone is the
//! worst bug this app can have, so [`is_parked`] is deliberately inclusive and
//! [`clamp_to_visible`] never returns a frame inside the park zone.
//!
//! **Where macOS leaves a parked window.** Measured (every parked window the test runs logged
//! on a 2560x1440 display whose visible frame reaches its bottom edge): the window keeps its
//! asked-for `x` (one point inside the right edge) but its `y` is clamped so that
//! [`MACOS_KEEPS_TITLE`] points of title bar stay above the bottom of the **visible** frame.
//! With a Dock at the bottom the window therefore sits above the Dock, up to ~110 pt above
//! the display's bottom edge, outside the full frame's park zone. So every "is it parked"
//! test goes through [`is_parked_in`], which accepts the park zone of the full frame *and*
//! of the visible frame: that covers a Dock at the bottom, on the left, on the right (whether
//! macOS clamps `x` against the full or the visible frame) and auto-hidden, on any display.

use super::geometry::{Rect, Size};

/// How close (in points) a window's origin must be to the display's bottom-right corner to
/// count as parked.
pub const PARK_MARGIN: f64 = 64.0;

/// Height of a standard title bar; [`clamp_to_visible`] keeps this strip inside the visible
/// frame so the window can always be grabbed.
pub const TITLE_BAR_HEIGHT: f64 = 28.0;

/// Points of title bar macOS keeps above the bottom of the visible frame when a window is
/// moved below it (measured: a window asked to sit at `max_y - 1` of a 1440 pt display with
/// no Dock at the bottom is listed at `y = 1408`).
pub const MACOS_KEEPS_TITLE: f64 = 32.0;

/// Size used when a restore frame is degenerate and must be invented.
pub const FALLBACK_SIZE: Size = Size::new(800.0, 600.0);

/// The frame that parks a window of `size` in `display`'s bottom-right corner.
///
/// The origin is one point inside the corner; macOS clamps the window so a small remainder
/// stays onscreen. The size is kept so the window is restored without a resize.
pub fn park_frame(size: Size, display: Rect) -> Rect {
    Rect::new(display.max_x() - 1.0, display.max_y() - 1.0, size.w, size.h)
}

/// True when `frame` sits in `display`'s park zone.
///
/// Inclusive on purpose (the recovery scan runs this on windows the engine never saw): a
/// frame is parked when its origin lies within [`PARK_MARGIN`] of the bottom-right corner or
/// beyond it, or when it is mostly offscreen to the bottom-right (its visible part is at
/// most `PARK_MARGIN` on both axes and its origin is right of and below the display center).
pub fn is_parked(frame: Rect, display: Rect) -> bool {
    if !(frame.x.is_finite() && frame.y.is_finite()) {
        return false;
    }
    let corner_x = display.max_x() - PARK_MARGIN;
    let corner_y = display.max_y() - PARK_MARGIN;
    if frame.x >= corner_x && frame.y >= corner_y {
        return true;
    }
    let center = display.center();
    if frame.x <= center.x || frame.y <= center.y {
        return false;
    }
    match frame.intersect(&display) {
        None => frame.w > 0.0 && frame.h > 0.0,
        Some(seen) => seen.w <= PARK_MARGIN && seen.h <= PARK_MARGIN,
    }
}

/// True when `frame` sits in the park zone of a display whose full frame is `display` and
/// whose visible frame (no menu bar, no Dock) is `visible`: the one park test every caller
/// uses (module docs, "Where macOS leaves a parked window").
pub fn is_parked_in(frame: Rect, display: Rect, visible: Rect) -> bool {
    is_parked(frame, display) || (!visible.is_degenerate() && is_parked(frame, visible))
}

/// A restore frame that is fully reachable inside `visible` (the display minus menu bar and
/// Dock).
///
/// - A degenerate frame (non-finite or non-positive size) becomes [`FALLBACK_SIZE`] (shrunk
///   to fit) centered in `visible`.
/// - A frame larger than `visible` is shrunk to fit.
/// - A frame in the park zone of `visible` is centered.
/// - Otherwise the frame moves only as far as needed to keep its title bar (full width, top
///   [`TITLE_BAR_HEIGHT`] points) inside `visible`; its body may extend below the bottom.
pub fn clamp_to_visible(frame: Rect, visible: Rect) -> Rect {
    if frame.is_degenerate() {
        let size = Size::new(
            FALLBACK_SIZE.w.min(visible.w),
            FALLBACK_SIZE.h.min(visible.h),
        );
        return center_size(size, visible);
    }
    let size = Size::new(frame.w.min(visible.w), frame.h.min(visible.h));
    if is_parked(frame, visible) {
        return center_size(size, visible);
    }
    let title = TITLE_BAR_HEIGHT.min(size.h);
    let x = frame.x.clamp(visible.x, visible.max_x() - size.w);
    let y = frame.y.clamp(visible.y, visible.max_y() - title);
    Rect::new(x, y, size.w, size.h)
}

fn center_size(size: Size, visible: Rect) -> Rect {
    let x = (visible.x + (visible.w - size.w) / 2.0).round();
    let y = (visible.y + (visible.h - size.h) / 2.0).round();
    Rect::new(x, y, size.w, size.h)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: Rect = Rect::new(0.0, 0.0, 1440.0, 900.0);
    const VISIBLE: Rect = Rect::new(0.0, 25.0, 1440.0, 800.0);

    /// Where macOS leaves a window asked to sit at `asked`: `y` clamped so
    /// [`MACOS_KEEPS_TITLE`] points stay above the visible frame's bottom; `x` clamped the
    /// same way against the visible frame's right edge when `x_by_visible` (unmeasured: a
    /// Dock on the right may or may not do this), else kept.
    fn macos_places(asked: Rect, visible: Rect, x_by_visible: bool) -> Rect {
        let y = asked.y.min(visible.max_y() - MACOS_KEEPS_TITLE);
        let x = if x_by_visible { asked.x.min(visible.max_x() - 1.0) } else { asked.x };
        Rect::new(x, y, asked.w, asked.h)
    }

    /// Visible frames for a display: menu bar on top, and a Dock at the bottom, on the left,
    /// on the right, auto-hidden (a 4 pt strip) or on another display.
    fn dock_layouts(d: Rect, menu: f64) -> Vec<(&'static str, Rect)> {
        let top = d.y + menu;
        let h = d.h - menu;
        vec![
            ("no Dock", Rect::new(d.x, top, d.w, h)),
            ("Dock at the bottom, 76 pt", Rect::new(d.x, top, d.w, h - 76.0)),
            ("Dock at the bottom, largest", Rect::new(d.x, top, d.w, h - 140.0)),
            ("Dock on the left", Rect::new(d.x + 80.0, top, d.w - 80.0, h)),
            ("Dock on the right", Rect::new(d.x, top, d.w - 80.0, h)),
            ("Dock auto-hidden at the bottom", Rect::new(d.x, top, d.w, h - 4.0)),
            ("Dock auto-hidden on the right", Rect::new(d.x, top, d.w - 4.0, h)),
        ]
    }

    #[test]
    fn a_parked_window_is_parked_for_every_dock_position_on_every_display() {
        let displays = [
            ("built-in", Rect::new(0.0, 0.0, 1512.0, 982.0), 37.0),
            ("5K", Rect::new(0.0, 0.0, 2560.0, 1440.0), 30.0),
            ("second, right of main", Rect::new(2560.0, 200.0, 1920.0, 1080.0), 25.0),
            ("second, left of main, higher", Rect::new(-1440.0, -300.0, 1440.0, 900.0), 25.0),
            ("second, below main", Rect::new(300.0, 1440.0, 1512.0, 982.0), 0.0),
        ];
        for (name, d, menu) in displays {
            for (dock, visible) in dock_layouts(d, menu) {
                for size in [Size::new(600.0, 392.0), Size::new(300.0, 120.0), Size::new(2400.0, 1300.0)] {
                    for x_by_visible in [false, true] {
                        let at = macos_places(park_frame(size, d), visible, x_by_visible);
                        assert!(
                            is_parked_in(at, d, visible),
                            "{name}, {dock}, {size:?}, x by visible {x_by_visible}: {at:?} not parked"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_full_frame_alone_misses_a_window_parked_above_a_bottom_dock() {
        // The review's case: 76 pt Dock at the bottom of the 5K.
        let d = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let visible = Rect::new(0.0, 30.0, 2560.0, 1334.0);
        let at = Rect::new(2559.0, visible.max_y() - MACOS_KEEPS_TITLE, 600.0, 392.0);
        assert!(!is_parked(at, d), "the old test missed it");
        assert!(is_parked_in(at, d, visible));
    }

    #[test]
    fn ordinary_windows_are_not_parked_whatever_the_dock() {
        let d = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        for (dock, visible) in dock_layouts(d, 30.0) {
            for f in [
                Rect::new(100.0, 100.0, 800.0, 600.0),
                Rect::new(1600.0, 700.0, 800.0, 600.0),
                // Dragged low, most of it above the Dock.
                Rect::new(1200.0, visible.max_y() - 300.0, 800.0, 600.0),
                // A small window tucked in the corner above the Dock, mostly visible.
                Rect::new(visible.max_x() - 300.0, visible.max_y() - 200.0, 280.0, 180.0),
                Rect::new(-2000.0, -2000.0, 100.0, 100.0),
            ] {
                assert!(!is_parked_in(f, d, visible), "{dock}: {f:?}");
            }
        }
    }

    #[test]
    fn a_degenerate_visible_frame_falls_back_to_the_full_frame() {
        let at = park_frame(Size::new(400.0, 300.0), DISPLAY);
        assert!(is_parked_in(at, DISPLAY, Rect::default()));
        assert!(!is_parked_in(Rect::new(10.0, 10.0, 400.0, 300.0), DISPLAY, Rect::default()));
    }

    #[test]
    fn park_frame_sits_in_the_corner_and_keeps_size() {
        let f = park_frame(Size::new(600.0, 400.0), DISPLAY);
        assert_eq!(f, Rect::new(1439.0, 899.0, 600.0, 400.0));
        assert!(is_parked(f, DISPLAY));
    }

    #[test]
    fn park_frame_on_a_secondary_display() {
        let second = Rect::new(1440.0, -200.0, 1920.0, 1080.0);
        let f = park_frame(Size::new(300.0, 300.0), second);
        assert_eq!((f.x, f.y), (3359.0, 879.0));
        assert!(is_parked(f, second));
        assert!(!is_parked(f.translate(-1000.0, -500.0), second));
    }

    #[test]
    fn macos_clamped_park_is_still_parked() {
        // macOS often keeps ~40 pt of the window onscreen.
        let clamped = Rect::new(1400.0, 860.0, 600.0, 400.0);
        assert!(is_parked(clamped, DISPLAY));
    }

    #[test]
    fn mostly_offscreen_bottom_right_is_parked() {
        let f = Rect::new(1380.0, 850.0, 800.0, 600.0);
        assert!(is_parked(f, DISPLAY));
        let beyond = Rect::new(2000.0, 1200.0, 100.0, 100.0);
        assert!(is_parked(beyond, DISPLAY));
    }

    #[test]
    fn ordinary_frames_are_not_parked() {
        assert!(!is_parked(Rect::new(100.0, 100.0, 800.0, 600.0), DISPLAY));
        // A large window reaching into the corner but mostly visible.
        assert!(!is_parked(Rect::new(800.0, 400.0, 800.0, 600.0), DISPLAY));
        // Offscreen to the top-left is not the park zone.
        assert!(!is_parked(
            Rect::new(-2000.0, -2000.0, 100.0, 100.0),
            DISPLAY
        ));
        // Offscreen bottom-left is not the park zone either.
        assert!(!is_parked(Rect::new(10.0, 880.0, 100.0, 100.0), DISPLAY));
    }

    #[test]
    fn clamp_keeps_a_reachable_frame_unchanged() {
        let f = Rect::new(200.0, 100.0, 800.0, 600.0);
        assert_eq!(clamp_to_visible(f, VISIBLE), f);
        // Body below the bottom edge is allowed while the title bar is inside.
        let low = Rect::new(200.0, 700.0, 800.0, 600.0);
        assert_eq!(clamp_to_visible(low, VISIBLE), low);
    }

    #[test]
    fn clamp_pulls_the_title_bar_inside() {
        let above = Rect::new(100.0, -50.0, 400.0, 300.0);
        assert_eq!(clamp_to_visible(above, VISIBLE).y, 25.0);
        let right = Rect::new(1300.0, 100.0, 400.0, 300.0);
        assert_eq!(clamp_to_visible(right, VISIBLE).x, 1040.0);
        let left = Rect::new(-300.0, 100.0, 400.0, 300.0);
        assert_eq!(clamp_to_visible(left, VISIBLE).x, 0.0);
        let below = Rect::new(100.0, 820.0, 400.0, 300.0);
        assert_eq!(clamp_to_visible(below, VISIBLE).y, 825.0 - TITLE_BAR_HEIGHT);
    }

    #[test]
    fn clamp_shrinks_oversized_frames() {
        let big = Rect::new(-10.0, 0.0, 3000.0, 2000.0);
        assert_eq!(clamp_to_visible(big, VISIBLE), VISIBLE);
    }

    #[test]
    fn clamp_centers_parked_and_degenerate_frames() {
        let parked = park_frame(Size::new(400.0, 200.0), DISPLAY);
        assert_eq!(
            clamp_to_visible(parked, VISIBLE),
            Rect::new(520.0, 325.0, 400.0, 200.0)
        );
        let nan = Rect::new(f64::NAN, 0.0, 100.0, 100.0);
        assert_eq!(
            clamp_to_visible(nan, VISIBLE),
            Rect::new(320.0, 125.0, 800.0, 600.0)
        );
        let empty = Rect::new(10.0, 10.0, 0.0, 0.0);
        assert!(!clamp_to_visible(empty, VISIBLE).is_degenerate());
    }

    #[test]
    fn clamp_never_returns_a_parked_frame() {
        let mut seed = 7u64;
        for _ in 0..2000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let a = ((seed >> 33) % 4000) as f64 - 1000.0;
            let b = ((seed >> 13) % 3000) as f64 - 1000.0;
            let w = ((seed >> 40) % 2500) as f64;
            let h = ((seed >> 20) % 1800) as f64;
            let out = clamp_to_visible(Rect::new(a, b, w, h), VISIBLE);
            assert!(!is_parked(out, VISIBLE), "{out:?}");
            assert!(!is_parked(out, DISPLAY), "{out:?}");
            assert!(VISIBLE.contains_rect(&Rect::new(
                out.x,
                out.y,
                out.w,
                TITLE_BAR_HEIGHT.min(out.h)
            )));
        }
    }
}
