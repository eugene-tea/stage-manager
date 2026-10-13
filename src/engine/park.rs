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
//!
//! **Which corner.** A window parked against a side of the main display where another
//! display sits lands, whole, on that display (macOS lets a window span displays), so the
//! corner is chosen from the display arrangement ([`choose_corner`]): the first of
//! bottom-right, bottom-left, top-right, top-left whose offscreen side has no display beyond
//! it, else the one overlapping other displays least. The model's [`Display`] carries the
//! choice and every park test of the model uses it ([`is_parked_in_at`]); the safety paths
//! (recovery scan, guardian, quit check) accept every corner used this run plus
//! bottom-right. Top corners hide sideways only: macOS keeps a title bar below the menu bar,
//! so a window asked to sit at the display's top edge stays there, its body off the side.
//!
//! **How the park frame is asked** ([`ParkRequest`], `[windows] park_inside` and
//! `park_drop`): by default one point of the window inside the corner's side and its top one
//! point above the display's bottom edge, as since the first version. Nobody had measured
//! whether macOS accepts less (no point inside, or the window asked further out) or keeps a
//! different title strip when asked further below; `stage-manager probe-park` (driven by
//! `scripts/probe_park.py`, fixture windows only) asks a grid of requests and
//! [`best_request`] picks the one that leaves least on screen while the window stays listed
//! on screen (Desktops are recognized by the windows on screen, parked ones included) and in
//! the park zone.
//!
//! [`Display`]: super::model::Display

use serde::{Deserialize, Serialize};

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

/// A corner of a display where inactive stages park ([`choose_corner`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    /// The default, Apple's corner: no other display there.
    #[default]
    BottomRight,
    /// Mirrors bottom-right: the body off the left side.
    BottomLeft,
    /// The body off the right side, at the top (macOS keeps it below the menu bar).
    TopRight,
    /// The body off the left side, at the top.
    TopLeft,
}

impl Corner {
    /// Every corner, in order of preference.
    pub const ALL: [Corner; 4] = [Corner::BottomRight, Corner::BottomLeft, Corner::TopRight, Corner::TopLeft];

    /// True when windows park off the display's right side.
    pub fn right(self) -> bool {
        matches!(self, Corner::BottomRight | Corner::TopRight)
    }

    /// True when windows park at the display's bottom.
    pub fn bottom(self) -> bool {
        matches!(self, Corner::BottomRight | Corner::BottomLeft)
    }
}

/// The frame that parks a window of `size` in `display`'s bottom-right corner
/// ([`park_frame_at`] with [`Corner::BottomRight`]).
pub fn park_frame(size: Size, display: Rect) -> Rect {
    park_frame_at(size, display, Corner::BottomRight)
}

/// How a park frame is asked of macOS (`[windows] park_inside`, `park_drop`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParkRequest {
    /// Points of the window asked to stay inside the corner's side (0: none; negative: the
    /// window asked that far beyond the side).
    pub inside: f64,
    /// At a bottom corner, how far below the display's bottom edge the window's top is asked
    /// (-1: one point above it, the first version's request).
    pub drop: f64,
}

impl ParkRequest {
    /// The first version's request: one point inside, the top one point above the bottom.
    pub const FIRST: ParkRequest = ParkRequest { inside: 1.0, drop: -1.0 };

    /// The request clamped to what still parks: at most [`PARK_MARGIN`] inside, never above
    /// the bottom by more than that, never beyond a display's width or height out.
    pub fn clamped(self) -> ParkRequest {
        let finite = |v: f64, d: f64| if v.is_finite() { v } else { d };
        ParkRequest {
            inside: finite(self.inside, 1.0).clamp(-10_000.0, PARK_MARGIN / 2.0),
            drop: finite(self.drop, -1.0).clamp(-PARK_MARGIN / 2.0, 10_000.0),
        }
    }
}

impl Default for ParkRequest {
    fn default() -> Self {
        Self::FIRST
    }
}

/// The frame that parks a window of `size` in `display`'s `corner`
/// ([`park_frame_asked`] with [`ParkRequest::FIRST`]).
///
/// One point of the window stays inside the display's side; at the bottom the origin is one
/// point above the bottom edge (macOS clamps the window so its title bar stays onscreen), at
/// the top the window's top is the display's top edge (macOS keeps it below the menu bar).
/// The size is kept so the window is restored without a resize.
pub fn park_frame_at(size: Size, display: Rect, corner: Corner) -> Rect {
    park_frame_asked(size, display, corner, ParkRequest::FIRST)
}

/// The frame that parks a window of `size` in `display`'s `corner`, asked as `req` says
/// (clamped: [`ParkRequest::clamped`]).
pub fn park_frame_asked(size: Size, display: Rect, corner: Corner, req: ParkRequest) -> Rect {
    let req = req.clamped();
    let x = if corner.right() { display.max_x() - req.inside } else { display.x - size.w + req.inside };
    let y = if corner.bottom() { display.max_y() + req.drop } else { display.y };
    Rect::new(x, y, size.w, size.h)
}

/// One request of the park probe and where macOS left one window for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeSample {
    /// The request.
    pub req: ParkRequest,
    /// Where macOS left the window (the window server's bounds).
    pub landed: Rect,
    /// The window server still lists it on screen.
    pub onscreen: bool,
}

/// The area (pt²) of `landed` that shows on `display`.
pub fn shown_area(landed: Rect, display: Rect) -> f64 {
    landed.intersect(&display).map_or(0.0, |r| r.w * r.h)
}

/// The request that leaves least on screen across every probed window (`samples`: each
/// window's samples, any order), among those where every window stayed listed on screen and
/// in the park zone of `display` (`visible`, `corner`); the worst window counts. Ties go to
/// the request closest to [`ParkRequest::FIRST`]. `None` when no request qualifies.
pub fn best_request(samples: &[Vec<ProbeSample>], display: Rect, visible: Rect, corner: Corner) -> Option<(ParkRequest, f64)> {
    let mut reqs: Vec<ParkRequest> = samples.iter().flatten().map(|s| s.req).collect();
    reqs.dedup_by(|a, b| a == b);
    let mut unique: Vec<ParkRequest> = Vec::new();
    for r in reqs {
        if !unique.contains(&r) {
            unique.push(r);
        }
    }
    let worst = |r: ParkRequest| -> Option<f64> {
        let mut worst = 0.0f64;
        for window in samples {
            let s = window.iter().find(|s| s.req == r)?;
            if !s.onscreen || !is_parked_in_at(s.landed, display, visible, corner) {
                return None;
            }
            worst = worst.max(shown_area(s.landed, display));
        }
        Some(worst)
    };
    let distance = |r: ParkRequest| (r.inside - ParkRequest::FIRST.inside).abs() + (r.drop - ParkRequest::FIRST.drop).abs();
    unique
        .into_iter()
        .filter_map(|r| worst(r).map(|w| (r, w)))
        .min_by(|a, b| a.1.total_cmp(&b.1).then(distance(a.0).total_cmp(&distance(b.0))))
}

/// `frame` mirrored left to right across `display`: a left corner's frame becomes the
/// matching right corner's.
fn mirror_x(frame: Rect, display: Rect) -> Rect {
    Rect::new(display.x + display.max_x() - frame.max_x(), frame.y, frame.w, frame.h)
}

/// The corner chosen for a display arrangement, and how much of the chosen corner's reach
/// lies on other displays (0 when no display is beyond it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CornerChoice {
    /// The corner.
    pub corner: Corner,
    /// Area (pt²) of other displays a display-sized window parked there would cover.
    pub overlap: f64,
}

/// Where a display-sized window parked in `corner` of the main display (`main` full frame,
/// `visible` frame) could reach: beyond the side, from where macOS leaves its title bar
/// (above a bottom Dock, below the menu bar) down.
pub fn corner_reach(main: Rect, visible: Rect, corner: Corner) -> Rect {
    let v = if visible.is_degenerate() { main } else { visible };
    let x = if corner.right() { main.max_x() - 1.0 } else { main.x - main.w + 1.0 };
    let (y, h) = if corner.bottom() { (v.max_y() - MACOS_KEEPS_TITLE, main.h) } else { (v.y, v.h) };
    Rect::new(x, y, main.w, h)
}

/// The park corner for the main display (`main`, `visible`) among the `others`: the first of
/// [`Corner::ALL`] that no other display lies beyond, else the one whose reach covers the
/// least of them (ties in [`Corner::ALL`] order).
pub fn choose_corner(main: Rect, visible: Rect, others: &[Rect]) -> CornerChoice {
    let overlap = |c: Corner| {
        let reach = corner_reach(main, visible, c);
        others.iter().filter_map(|d| reach.intersect(d)).map(|r| r.w * r.h).sum::<f64>()
    };
    let mut best = CornerChoice { corner: Corner::BottomRight, overlap: f64::INFINITY };
    for c in Corner::ALL {
        let o = overlap(c);
        if o <= 0.0 {
            return CornerChoice { corner: c, overlap: 0.0 };
        }
        if o < best.overlap {
            best = CornerChoice { corner: c, overlap: o };
        }
    }
    best
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
    // "Mostly offscreen" needs the frame to reach past the right or bottom edge: a small
    // window wholly on the display (a helper app's invisible 1×1, a 64×64 cursor window) is
    // not parked, however little of it there is (6X: 24 such windows made every quit warn).
    if frame.max_x() <= display.max_x() && frame.max_y() <= display.max_y() {
        return false;
    }
    match frame.intersect(&display) {
        None => frame.w > 0.0 && frame.h > 0.0,
        Some(seen) => {
            (seen.w <= PARK_MARGIN && seen.h <= PARK_MARGIN)
                || (seen.w.min(seen.h) <= PARK_SLIVER && seen.w.max(seen.h) <= PARK_SLIVER_LENGTH)
        }
    }
}

/// A parked window's sliver: at most this thick on one axis ...
///
/// (6X: after a display change macOS left a parked window at `2559,1374` of a 2560×1440
/// display, one point wide and 66 tall on screen, two points above the corner's margin: no
/// run counted it parked, so the next one adopted it in the corner.)
pub const PARK_SLIVER: f64 = 8.0;

/// ... and at most this long on the other.
pub const PARK_SLIVER_LENGTH: f64 = 4.0 * PARK_MARGIN;

/// True when `frame` sits in `display`'s park zone at `corner`: [`is_parked`] for the bottom
/// corners (mirrored for the left one); for the top ones, its origin within [`PARK_MARGIN`]
/// of the side's edge or beyond it, in the display's top half (or up to [`PARK_MARGIN`]
/// above it).
pub fn is_parked_at(frame: Rect, display: Rect, corner: Corner) -> bool {
    if !(frame.x.is_finite() && frame.y.is_finite() && frame.w.is_finite()) {
        return false;
    }
    let f = if corner.right() { frame } else { mirror_x(frame, display) };
    if corner.bottom() {
        is_parked(f, display)
    } else {
        f.x >= display.max_x() - PARK_MARGIN && f.y >= display.y - PARK_MARGIN && f.y <= display.center().y
    }
}

/// True when `frame` sits in the bottom-right park zone of a display whose full frame is
/// `display` and whose visible frame (no menu bar, no Dock) is `visible`
/// ([`is_parked_in_at`] with [`Corner::BottomRight`]).
pub fn is_parked_in(frame: Rect, display: Rect, visible: Rect) -> bool {
    is_parked_in_at(frame, display, visible, Corner::BottomRight)
}

/// True when `frame` sits in the park zone at `corner` of a display whose full frame is
/// `display` and whose visible frame (no menu bar, no Dock) is `visible`: the one park test
/// every caller uses (module docs, "Where macOS leaves a parked window").
pub fn is_parked_in_at(frame: Rect, display: Rect, visible: Rect, corner: Corner) -> bool {
    is_parked_at(frame, display, corner) || (!visible.is_degenerate() && is_parked_at(frame, visible, corner))
}

/// True when `frame` sits in the park zone of any corner of `display`.
pub fn is_parked_anywhere(frame: Rect, display: Rect) -> bool {
    Corner::ALL.iter().any(|c| is_parked_at(frame, display, *c))
}

/// A restore frame that is fully reachable inside `visible` (the display minus menu bar and
/// Dock).
///
/// - A degenerate frame (non-finite or non-positive size) becomes [`FALLBACK_SIZE`] (shrunk
///   to fit) centered in `visible`.
/// - A frame larger than `visible` is shrunk to fit.
/// - A frame in the park zone of any corner of `visible` is centered.
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
    if is_parked_anywhere(frame, visible) {
        return center_size(size, visible);
    }
    let title = TITLE_BAR_HEIGHT.min(size.h);
    let x = frame.x.clamp(visible.x, visible.max_x() - size.w);
    let y = frame.y.clamp(visible.y, visible.max_y() - title);
    let out = Rect::new(x, y, size.w, size.h);
    // A small window slid against a side could still sit in a corner's zone.
    if is_parked_anywhere(out, visible) { center_size(size, visible) } else { out }
}

fn center_size(size: Size, visible: Rect) -> Rect {
    let x = (visible.x + (visible.w - size.w) / 2.0).round();
    let y = (visible.y + (visible.h - size.h) / 2.0).round();
    Rect::new(x, y, size.w, size.h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::displays::on_other_display;

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

    /// Where macOS leaves a window asked to sit at `asked` parked in `corner` with no display
    /// beyond it: at the bottom, `y` clamped so [`MACOS_KEEPS_TITLE`] points stay above the
    /// visible frame's bottom; at the top, `y` kept below the menu bar; `x` kept.
    fn macos_places_at(asked: Rect, visible: Rect, corner: Corner) -> Rect {
        let y = if corner.bottom() { asked.y.min(visible.max_y() - MACOS_KEEPS_TITLE) } else { asked.y.max(visible.y) };
        Rect::new(asked.x, y, asked.w, asked.h)
    }

    #[test]
    fn every_corner_parks_for_every_dock_position() {
        let displays = [
            ("built-in", Rect::new(0.0, 0.0, 1512.0, 982.0), 37.0),
            ("5K", Rect::new(0.0, 0.0, 2560.0, 1440.0), 30.0),
            ("second, left of main, higher", Rect::new(-1440.0, -300.0, 1440.0, 900.0), 25.0),
        ];
        for corner in Corner::ALL {
            for (name, d, menu) in displays {
                for (dock, visible) in dock_layouts(d, menu) {
                    for size in [Size::new(600.0, 392.0), Size::new(300.0, 120.0), Size::new(2400.0, 1300.0)] {
                        let at = macos_places_at(park_frame_at(size, d, corner), visible, corner);
                        assert!(is_parked_in_at(at, d, visible, corner), "{corner:?}, {name}, {dock}, {size:?}: {at:?} not parked");
                        // Never parked on the other side's corners: the model's test is exact.
                        for other in Corner::ALL.into_iter().filter(|c| c.right() != corner.right()) {
                            assert!(!is_parked_in_at(at, d, visible, other), "{corner:?} at {at:?} also counts as {other:?}");
                        }
                        // Hidden but for a sliver: at most one point of it across the side.
                        let seen = at.intersect(&d).map_or(0.0, |r| r.w);
                        assert!(seen <= 1.0, "{corner:?}, {name}, {dock}: {seen} pt show");
                    }
                }
            }
        }
    }

    #[test]
    fn ordinary_windows_are_parked_at_no_corner() {
        let d = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        for (dock, visible) in dock_layouts(d, 30.0) {
            for f in [
                Rect::new(100.0, 100.0, 800.0, 600.0),
                Rect::new(1600.0, 700.0, 800.0, 600.0),
                Rect::new(1200.0, visible.max_y() - 300.0, 800.0, 600.0),
                Rect::new(40.0, 40.0, 800.0, 600.0),
                Rect::new(visible.max_x() - 820.0, visible.y, 800.0, 600.0),
                Rect::new(-2000.0, -2000.0, 100.0, 100.0),
            ] {
                for corner in Corner::ALL {
                    assert!(!is_parked_in_at(f, d, visible, corner), "{dock}, {corner:?}: {f:?}");
                }
            }
        }
    }

    /// The main display of the arrangement tests: a 5K with its Dock at the bottom.
    const MAIN_5K: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    const MAIN_5K_VISIBLE: Rect = Rect::new(0.0, 30.0, 2560.0, 1334.0);

    /// Display arrangements (Displays → Arrange) and the corner each must park in.
    fn arrangements() -> Vec<(&'static str, Vec<Rect>, Corner)> {
        let laptop = Size::new(1512.0, 982.0);
        let at = |x: f64, y: f64| Rect::new(x, y, laptop.w, laptop.h);
        vec![
            ("one display", vec![], Corner::BottomRight),
            ("right, bottoms aligned", vec![at(2560.0, 1440.0 - 982.0)], Corner::BottomLeft),
            ("right, tops aligned (clear of the bottom corner)", vec![at(2560.0, 0.0)], Corner::BottomRight),
            ("below, under the right edge", vec![at(1400.0, 1440.0)], Corner::BottomLeft),
            ("below, centered (clear of both corners)", vec![at(524.0, 1440.0)], Corner::BottomRight),
            ("left, bottoms aligned", vec![at(-1512.0, 1440.0 - 982.0)], Corner::BottomRight),
            ("above", vec![at(500.0, -982.0)], Corner::BottomRight),
            ("diagonal, below right", vec![at(2560.0, 1440.0)], Corner::BottomLeft),
            ("diagonal, below left", vec![at(-1512.0, 1440.0)], Corner::BottomRight),
            (
                "two externals: right and a wide one below",
                vec![at(2560.0, 1440.0 - 982.0), Rect::new(-300.0, 1440.0, 3440.0, 1440.0)],
                Corner::TopLeft,
            ),
            (
                "two externals: diagonal below right, and left with bottoms aligned",
                vec![at(2560.0, 1440.0), at(-1512.0, 1440.0 - 982.0)],
                Corner::TopRight,
            ),
            (
                "two externals: right and left, tops aligned",
                vec![at(2560.0, 0.0), at(-1512.0, 0.0)],
                Corner::BottomRight,
            ),
        ]
    }

    #[test]
    fn the_park_corner_has_no_display_beyond_it() {
        for (name, others, want) in arrangements() {
            let choice = choose_corner(MAIN_5K, MAIN_5K_VISIBLE, &others);
            assert_eq!(choice.corner, want, "{name}");
            assert_eq!(choice.overlap, 0.0, "{name}");
            for size in [Size::new(600.0, 392.0), Size::new(1800.0, 1200.0), Size::new(2560.0, 1334.0)] {
                let at = macos_places_at(park_frame_at(size, MAIN_5K, want), MAIN_5K_VISIBLE, want);
                assert!(is_parked_in_at(at, MAIN_5K, MAIN_5K_VISIBLE, want), "{name}: {at:?}");
                assert!(!on_other_display(at, MAIN_5K, &others), "{name}: {at:?} lands on another display");
                assert!(others.iter().all(|o| at.intersect(o).is_none()), "{name}: {at:?} shows on another display");
            }
        }
    }

    #[test]
    fn the_bottom_right_corner_would_show_parked_windows_on_a_display_beyond_it() {
        // The review's V1: a display right of the main one, bottoms aligned. Bottom-right
        // parking puts the whole window there.
        let right = Rect::new(2560.0, 458.0, 1512.0, 982.0);
        let at = macos_places_at(park_frame(Size::new(600.0, 392.0), MAIN_5K), MAIN_5K_VISIBLE, Corner::BottomRight);
        assert!(on_other_display(at, MAIN_5K, &[right]));
        assert_eq!(choose_corner(MAIN_5K, MAIN_5K_VISIBLE, &[right]).corner, Corner::BottomLeft);
    }

    #[test]
    fn with_a_display_beyond_every_corner_the_least_overlap_wins_and_says_so() {
        // Full-height externals on both sides.
        let left = Rect::new(-2560.0, 0.0, 2560.0, 1440.0);
        let right = Rect::new(2560.0, 0.0, 2560.0, 1440.0);
        let choice = choose_corner(MAIN_5K, MAIN_5K_VISIBLE, &[left, right]);
        assert!(choice.overlap > 0.0);
        assert_eq!(choice.corner, Corner::BottomRight, "the bottom corners reach least (only below the Dock's top), ties go right");
        let worst = Corner::ALL
            .into_iter()
            .map(|c| corner_reach(MAIN_5K, MAIN_5K_VISIBLE, c))
            .map(|r| [left, right].iter().filter_map(|d| r.intersect(d)).map(|i| i.w * i.h).sum::<f64>())
            .fold(0.0, f64::max);
        assert!(choice.overlap < worst);
    }

    #[test]
    fn the_full_frame_alone_misses_a_window_parked_above_a_bottom_dock() {
        // The review's case: 76 pt Dock at the bottom of the 5K.
        let d = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let visible = Rect::new(0.0, 30.0, 2560.0, 1334.0);
        let at = Rect::new(2559.0, visible.max_y() - MACOS_KEEPS_TITLE, 600.0, 392.0);
        assert!(is_parked_in(at, d, visible));
        // Since 6X the full frame's sliver rule sees it too (1 pt wide, 108 tall on screen);
        // a wider one above the Dock still needs the visible frame.
        assert!(is_parked(at, d));
        let wider = Rect::new(2496.0, visible.max_y() - MACOS_KEEPS_TITLE, 600.0, 392.0);
        assert!(!is_parked(wider, d), "the full frame alone misses it");
        assert!(is_parked_in(wider, d, visible));
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
    fn the_first_request_is_the_old_park_frame() {
        let size = Size::new(600.0, 400.0);
        for corner in Corner::ALL {
            assert_eq!(park_frame_asked(size, DISPLAY, corner, ParkRequest::default()), park_frame_at(size, DISPLAY, corner));
        }
        assert_eq!(park_frame(size, DISPLAY), Rect::new(1439.0, 899.0, 600.0, 400.0));
    }

    #[test]
    fn every_request_in_range_still_parks_for_every_dock() {
        for inside in [1.0, 0.0, -1.0, -50.0, -600.0, 500.0] {
            for drop in [-1.0, 0.0, 50.0, 2000.0, -500.0] {
                let req = ParkRequest { inside, drop };
                for corner in Corner::ALL {
                    for (dock, visible) in dock_layouts(DISPLAY, 25.0) {
                        let at = macos_places_at(park_frame_asked(Size::new(600.0, 400.0), DISPLAY, corner, req), visible, corner);
                        assert!(is_parked_in_at(at, DISPLAY, visible, corner), "{req:?} {corner:?} {dock}: {at:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_best_request_shows_least_and_stays_listed() {
        let visible = Rect::new(0.0, 25.0, 1440.0, 875.0);
        let first = ParkRequest::FIRST;
        let zero = ParkRequest { inside: 0.0, drop: -1.0 };
        let out = ParkRequest { inside: -50.0, drop: -1.0 };
        let sample = |req: ParkRequest, x: f64, kept: f64, onscreen: bool| ProbeSample {
            req,
            landed: Rect::new(x, visible.max_y() - kept, 600.0, 400.0),
            onscreen,
        };
        // Two windows: macOS clamps x back to 1439 for the plain one at 0 inside, accepts it
        // for the other; 50 beyond takes both off the list.
        let a = vec![sample(first, 1439.0, 32.0, true), sample(zero, 1439.0, 32.0, true), sample(out, 1490.0, 32.0, false)];
        let b = vec![sample(first, 1439.0, 52.0, true), sample(zero, 1440.0, 52.0, true), sample(out, 1490.0, 52.0, false)];
        let (best, worst) = best_request(&[a.clone(), b.clone()], DISPLAY, visible, Corner::BottomRight).expect("one qualifies");
        // The worst window shows 32 pt at 0 inside (a) against 52 at the first request (b).
        assert_eq!(best, zero);
        assert_eq!(worst, 32.0);
        // When nothing is gained, the first request stays.
        let a2 = vec![sample(first, 1439.0, 32.0, true), sample(zero, 1439.0, 32.0, true)];
        assert_eq!(best_request(&[a2], DISPLAY, visible, Corner::BottomRight).map(|b| b.0), Some(first));
        // A window probed without some request rules that request out.
        let partial = vec![sample(first, 1439.0, 32.0, true)];
        assert_eq!(best_request(&[a, partial], DISPLAY, visible, Corner::BottomRight).map(|b| b.0), Some(first));
        assert_eq!(best_request(&[], DISPLAY, visible, Corner::BottomRight), None);
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

    /// 6X, the owner's log: helper apps' invisible windows (1×1 at the 5K's center seen on
    /// the built-in display, a 64×64 cursor window) sat wholly on the display, right of and
    /// below its center, and counted as parked: every quit warned about 24 of them.
    #[test]
    fn a_small_window_wholly_on_the_display_is_not_parked() {
        let laptop = Rect::new(0.0, 0.0, 1800.0, 1169.0);
        assert!(!is_parked(Rect::new(1280.0, 720.0, 1.0, 1.0), laptop));
        assert!(!is_parked(Rect::new(1300.0, 900.0, 64.0, 64.0), laptop));
        assert!(!is_parked(Rect::new(1736.0, 1100.0, 64.0, 64.0), laptop), "touching the corner, not past it");
        // A parked one still is: past the edge, one point inside.
        assert!(is_parked(Rect::new(1799.0, 1137.0, 1157.0, 1023.0), laptop));
    }

    /// 6X, the owner's log: after a display change macOS left a parked Telegram window at
    /// `2559,1374 1157x1023` of a 2560×1440 display, a 1×66 sliver two points above the
    /// corner's margin; no run counted it parked.
    #[test]
    fn a_thin_sliver_at_the_right_edge_is_parked() {
        let five_k = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let visible = Rect::new(0.0, 30.0, 2560.0, 1410.0);
        let telegram = Rect::new(2559.0, 1374.0, 1157.0, 1023.0);
        assert!(is_parked(telegram, five_k));
        assert!(is_parked_in(telegram, five_k, visible));
        // A sliver at the bottom edge, the same.
        assert!(is_parked(Rect::new(2400.0, 1436.0, 800.0, 600.0), five_k));
        // A window dragged mostly off the right edge, half way down, is no sliver of ours.
        assert!(!is_parked(Rect::new(2550.0, 700.0, 800.0, 600.0), five_k));
        // Nor one showing 100 pt.
        assert!(!is_parked(Rect::new(2460.0, 1300.0, 800.0, 600.0), five_k));
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
            for corner in Corner::ALL {
                assert!(!is_parked_in_at(out, DISPLAY, VISIBLE, corner), "{corner:?}: {out:?}");
            }
            assert!(VISIBLE.contains_rect(&Rect::new(
                out.x,
                out.y,
                out.w,
                TITLE_BAR_HEIGHT.min(out.h)
            )));
        }
    }
}
