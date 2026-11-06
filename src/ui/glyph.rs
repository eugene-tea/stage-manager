//! The menu bar glyph: small stage cards stacked at the left and a window at the right, the
//! Stage Manager idea at menu bar size.
//!
//! Original artwork drawn in code (no SF Symbol, no Apple image). It is a template image, so
//! the menu bar tints it for light, dark and highlighted states. The proportions follow SF
//! Symbols at the menu bar's default size, measured on the symbols themselves: 1 pt strokes, a
//! landscape window like `macwindow` (three dots at the top left) with a window's tight corners
//! rather than a case's round ones. Straight edges sit on whole pixels at 2x; the dots are 3 px
//! wide at 2x so they still read at 1x.
//!
//! Its height is its neighbours' (round 8N: the owner saw it shorter than every other menu bar
//! icon). Measured offscreen at the menu bar's default configuration (the 13 pt menu bar font,
//! which is also a symbol image's default; a 22 pt status bar), the ink of `macwindow`,
//! `rectangle.3.group`, `wifi`, `battery.100`, `switch.2` and `rectangle.stack` is 12, 12,
//! 10.5, 9, 12 and 14.5 pt tall: a median of 12 pt ([`INK_HEIGHT`]), where the glyph had 11.
//! The window grew in proportion (14 × 11 to 15.5 × 12 pt), the cards' stack with it (their
//! gaps 1 to 1.5 pt, so every edge stays on a whole pixel at 2x).
//!
//! [`shapes`] is the pure geometry (tested), parameterised by a [`Style`] so previews can
//! compare variants; [`status_image`] draws [`Style::DEFAULT`].

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{NSBezierPath, NSColor, NSImage};
use objc2_foundation::{NSPoint, NSRect, NSSize, ns_string};

/// The image's size in points (width, height): the ink and a 1.5 pt margin all round.
pub const SIZE: (f64, f64) = (25.0, 15.0);

/// The ink's height in points: the median of the SF Symbols beside it in the menu bar (module
/// docs).
pub const INK_HEIGHT: f64 = 12.0;

/// The SF Symbols stroke at the menu bar's default size.
pub const STROKE: f64 = 1.0;

/// The window frame's outer edge: x, y, width, height (points, y down).
const FRAME: (f64, f64, f64, f64) = (8.0, 1.5, 15.5, INK_HEIGHT);

/// The line the cards stack on, and the eye height of their perspective.
const MIDDLE_Y: f64 = FRAME.1 + FRAME.3 / 2.0;

/// The look's two knobs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    /// The window frame's outer corner radius.
    pub corner_radius: f64,
    /// The cards' far (right) edge over their near one: 1 is flat; below 1 the cards turn
    /// away about their left edge, seen from the stack's middle, as the strip's cards do.
    pub far_scale: f64,
}

impl Style {
    /// The shipped look: window corners as tight as `macwindow`'s; the cards turned like the
    /// strip's, a quarter softer than the first draft's 0.8.
    pub const DEFAULT: Style = Style { corner_radius: 2.0, far_scale: 0.85 };

    /// The cards' corner radius: in the same proportion to their height as the window's.
    pub fn card_radius(&self) -> f64 {
        self.corner_radius * CARD_HEIGHT / FRAME.3
    }
}

const CARD_WIDTH: f64 = 5.0;
const CARD_HEIGHT: f64 = 3.0;
/// Between two cards: three cards and two gaps span the window's height.
const CARD_GAP: f64 = (FRAME.3 - 3.0 * CARD_HEIGHT) / 2.0;

/// One shape of the glyph, in its coordinates (points, y down from the top).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// A card: its near (left) edge spans `y … y + height` at `x`; its far edge, at
    /// `x + width`, is the near one scaled by `far_scale` toward the stack's middle. Filled.
    Card { x: f64, y: f64, width: f64, height: f64, radius: f64, far_scale: f64 },
    /// A rounded rectangle, filled or, with `stroke`, outlined by a stroke of that width
    /// centred on its edge.
    Rect { x: f64, y: f64, width: f64, height: f64, radius: f64, stroke: Option<f64> },
    /// A filled dot (a title bar button).
    Dot { cx: f64, cy: f64, diameter: f64 },
}

impl Shape {
    /// The corners (top-left, top-right, bottom-right, bottom-left) of a card's outline.
    pub fn card_corners(x: f64, y: f64, width: f64, height: f64, far_scale: f64) -> [(f64, f64); 4] {
        let far = |v: f64| MIDDLE_Y + (v - MIDDLE_Y) * far_scale;
        [(x, y), (x + width, far(y)), (x + width, far(y + height)), (x, y + height)]
    }
}

/// The glyph: three cards (the strip) at the left, then the window: an outlined frame with
/// three title bar dots at its top left.
pub fn shapes(style: Style) -> Vec<Shape> {
    let (fx, fy, fw, fh) = FRAME;
    let card = |y: f64| Shape::Card {
        x: 1.5,
        y,
        width: CARD_WIDTH,
        height: CARD_HEIGHT,
        radius: style.card_radius(),
        far_scale: style.far_scale,
    };
    let half = STROKE / 2.0;
    // Dots 1.5 pt wide, 2.5 pt apart, a point in from the stroke's inner edges.
    let dot = |i: f64| Shape::Dot { cx: fx + STROKE + 1.75 + i * 2.5, cy: fy + STROKE + 1.75, diameter: 1.5 };
    vec![
        card(fy),
        card(fy + CARD_HEIGHT + CARD_GAP),
        card(fy + 2.0 * (CARD_HEIGHT + CARD_GAP)),
        Shape::Rect {
            x: fx + half,
            y: fy + half,
            width: fw - STROKE,
            height: fh - STROKE,
            radius: (style.corner_radius - half).max(0.0),
            stroke: Some(STROKE),
        },
        dot(0.0),
        dot(1.0),
        dot(2.0),
    ]
}

/// A closed path through `corners` with each corner rounded by `radius`.
fn rounded_polygon(corners: &[(f64, f64)], radius: f64) -> Retained<NSBezierPath> {
    let path = NSBezierPath::bezierPath();
    let n = corners.len();
    let mid = |a: (f64, f64), b: (f64, f64)| NSPoint::new((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    path.moveToPoint(mid(corners[n - 1], corners[0]));
    for i in 0..n {
        let corner = corners[i];
        let next = mid(corners[i], corners[(i + 1) % n]);
        path.appendBezierPathWithArcFromPoint_toPoint_radius(NSPoint::new(corner.0, corner.1), next, radius);
    }
    path.closePath();
    path
}

/// The status item's image in the shipped style ([`Style::DEFAULT`]).
pub fn status_image() -> Retained<NSImage> {
    image(Style::DEFAULT)
}

/// [`shapes`] as a resolution-independent template image (drawn at whatever scale the menu
/// bar needs). Main thread, as every AppKit drawing.
pub fn image(style: Style) -> Retained<NSImage> {
    let draw = RcBlock::new(move |_rect: NSRect| -> Bool {
        NSColor::blackColor().set();
        for shape in shapes(style) {
            match shape {
                Shape::Card { x, y, width, height, radius, far_scale } => {
                    rounded_polygon(&Shape::card_corners(x, y, width, height, far_scale), radius).fill();
                }
                Shape::Rect { x, y, width, height, radius, stroke } => {
                    let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
                    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius);
                    match stroke {
                        Some(w) => {
                            path.setLineWidth(w);
                            path.stroke();
                        }
                        None => path.fill(),
                    }
                }
                Shape::Dot { cx, cy, diameter } => {
                    let r = diameter / 2.0;
                    let rect = NSRect::new(NSPoint::new(cx - r, cy - r), NSSize::new(diameter, diameter));
                    NSBezierPath::bezierPathWithOvalInRect(rect).fill();
                }
            }
        }
        Bool::YES
    });
    // Flipped, so the handler draws in the y-down coordinates of `shapes`.
    let image = NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(SIZE.0, SIZE.1), true, &draw);
    image.setTemplate(true);
    image.setAccessibilityDescription(Some(ns_string!("Stage Manager")));
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shape's outer box (left, top, right, bottom); a stroke reaches half its width out.
    fn outer(s: &Shape) -> (f64, f64, f64, f64) {
        match *s {
            Shape::Card { x, y, width, height, .. } => (x, y, x + width, y + height),
            Shape::Rect { x, y, width, height, stroke, .. } => {
                let g = stroke.unwrap_or(0.0) / 2.0;
                (x - g, y - g, x + width + g, y + height + g)
            }
            Shape::Dot { cx, cy, diameter } => (cx - diameter / 2.0, cy - diameter / 2.0, cx + diameter / 2.0, cy + diameter / 2.0),
        }
    }

    fn frame(all: &[Shape]) -> (f64, f64, f64, f64) {
        all.iter().find(|s| matches!(s, Shape::Rect { stroke: Some(_), .. })).map(outer).unwrap_or_default()
    }

    #[test]
    fn the_ink_fits_the_image_with_a_symbols_margins() {
        let boxes: Vec<_> = shapes(Style::DEFAULT).iter().map(outer).collect();
        let top = boxes.iter().map(|b| b.1).fold(f64::MAX, f64::min);
        let bottom = boxes.iter().map(|b| b.3).fold(f64::MIN, f64::max);
        let left = boxes.iter().map(|b| b.0).fold(f64::MAX, f64::min);
        let right = boxes.iter().map(|b| b.2).fold(f64::MIN, f64::max);
        assert_eq!((top, SIZE.1 - bottom), (1.5, 1.5));
        assert_eq!((left, SIZE.0 - right), (1.5, 1.5));
    }

    /// Round 8N: as tall as its neighbours (the measured median), the cards' stack as tall as
    /// the window, the proportions kept (the window as wide for its height as before, ±5 %).
    #[test]
    fn the_ink_is_as_tall_as_the_symbols_beside_it() {
        let all = shapes(Style::DEFAULT);
        let boxes: Vec<_> = all.iter().map(outer).collect();
        let top = boxes.iter().map(|b| b.1).fold(f64::MAX, f64::min);
        let bottom = boxes.iter().map(|b| b.3).fold(f64::MIN, f64::max);
        assert_eq!(bottom - top, INK_HEIGHT);
        let cards: Vec<_> = all.iter().filter(|s| matches!(s, Shape::Card { .. })).map(outer).collect();
        let (_, ft, _, fb) = frame(&all);
        assert_eq!((cards[0].1, cards[2].3), (ft, fb), "the stack spans the window");
        let (l, t, r, b) = frame(&all);
        let aspect = (r - l) / (b - t);
        assert!((aspect / (14.0 / 11.0) - 1.0).abs() < 0.05, "the window's proportions kept: {aspect}");
    }

    #[test]
    fn the_window_is_landscape_with_tight_corners_like_macwindow() {
        let (l, t, r, b) = frame(&shapes(Style::DEFAULT));
        let (w, h) = (r - l, b - t);
        assert!(w / h > 1.2, "landscape, not a case: {w} × {h}");
        assert!(Style::DEFAULT.corner_radius / h < 0.2, "corners a window's, not a case's");
    }

    #[test]
    fn three_dots_sit_in_the_title_bar_at_the_top_left_apart() {
        let all = shapes(Style::DEFAULT);
        let (l, t, r, _) = frame(&all);
        let dots: Vec<_> = all.iter().filter(|s| matches!(s, Shape::Dot { .. })).map(outer).collect();
        assert_eq!(dots.len(), 3);
        for d in &dots {
            assert!(d.0 >= l + STROKE + 1.0 && d.1 >= t + STROKE + 0.5, "inside the frame, clear of the stroke");
            assert!(d.2 <= l + (r - l) * 0.65, "at the left");
        }
        for pair in dots.windows(2) {
            assert!(pair[1].0 - pair[0].2 >= 1.0, "a whole point between dots, so they read at 1x");
        }
    }

    #[test]
    fn three_cards_stack_left_of_the_window_with_a_gap() {
        let all = shapes(Style::DEFAULT);
        let cards: Vec<_> = all.iter().filter(|s| matches!(s, Shape::Card { .. })).map(outer).collect();
        assert_eq!(cards.len(), 3);
        let f = frame(&all);
        for c in &cards {
            assert!(f.0 - c.2 >= 1.25, "a clear gap between the strip and the window");
        }
        for pair in cards.windows(2) {
            assert!(pair[1].1 - pair[0].3 >= 1.0, "cards do not touch");
        }
    }

    #[test]
    fn cards_turn_like_the_strip_softly_with_corners_in_proportion() {
        let s = Style::DEFAULT;
        assert!(s.far_scale > 0.8 && s.far_scale < 0.9, "the strip's turn, a little softer than 0.8, not flat");
        let [_, tr, br, _] = Shape::card_corners(0.0, 1.5, 4.5, 3.0, s.far_scale);
        assert!(tr.1 > 1.5, "above the middle, the far edge drops toward it");
        assert!((br.1 - tr.1 - 3.0 * s.far_scale).abs() < 1e-9, "the far edge is shorter");
        let flat = Shape::card_corners(0.0, 1.5, 4.5, 3.0, 1.0);
        assert_eq!(flat[1].1, 1.5, "far_scale 1 is flat");
        assert!((s.card_radius() / CARD_HEIGHT - s.corner_radius / FRAME.3).abs() < 1e-9);
    }

    #[test]
    fn straight_edges_and_dots_land_on_whole_pixels_at_2x() {
        for s in shapes(Style::DEFAULT) {
            let (l, t, r, b) = outer(&s);
            for edge in [l, t, r, b] {
                assert_eq!((edge * 2.0).fract(), 0.0, "{s:?} edge {edge}");
            }
        }
    }
}
