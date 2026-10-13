//! The menu bar glyph: small stage thumbnails stacked at the left and a window at the right,
//! the Stage Manager idea at menu bar size.
//!
//! Original artwork drawn in code (no SF Symbol, no Apple image). It is a template image, so
//! the menu bar tints it for light, dark and highlighted states. The proportions follow SF
//! Symbols at the menu bar's default size, measured on the symbols themselves: the image is
//! 14 pt tall with 12 pt of ink (`rectangle`, `macwindow`, `sidebar.left`), strokes are 1 pt,
//! and a 12 pt rectangle's corners are about 3 pt. Edges sit on whole pixels at 2x.
//!
//! [`shapes`] is the pure geometry (tested); [`status_image`] draws it.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{NSBezierPath, NSColor, NSImage};
use objc2_foundation::{NSPoint, NSRect, NSSize, ns_string};

/// The image's size in points (width, height).
pub const SIZE: (f64, f64) = (21.0, 14.0);

/// The SF Symbols stroke at the menu bar's default size.
pub const STROKE: f64 = 1.0;

/// The thumbnails' far (right) edge over their near one: each is a card turned away about its
/// left edge and seen from the stack's middle, as the strip draws its cards.
pub const FAR_SCALE: f64 = 0.8;

/// The line the thumbnails stack on, and the eye height of their perspective.
const MIDDLE_Y: f64 = 7.0;

/// One shape of the glyph, in its coordinates (points, y down from the top).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// A thumbnail: its near (left) edge spans `y … y + height` at `x`; its far edge, at
    /// `x + width`, is the near one seen [`FAR_SCALE`] smaller toward [`MIDDLE_Y`]. Filled.
    Card { x: f64, y: f64, width: f64, height: f64, radius: f64 },
    /// A rounded rectangle, filled or, with `stroke`, outlined by a stroke of that width
    /// centred on its edge.
    Rect { x: f64, y: f64, width: f64, height: f64, radius: f64, stroke: Option<f64> },
}

impl Shape {
    /// The corners (top-left, top-right, bottom-right, bottom-left) of a card's outline.
    pub fn card_corners(x: f64, y: f64, width: f64, height: f64) -> [(f64, f64); 4] {
        let far = |v: f64| MIDDLE_Y + (v - MIDDLE_Y) * FAR_SCALE;
        [(x, y), (x + width, far(y)), (x + width, far(y + height)), (x, y + height)]
    }
}

/// The glyph: three tilted thumbnails (the strip) at the left, then the window: an outlined
/// frame with a title bar rule.
pub fn shapes() -> Vec<Shape> {
    let card = |y: f64| Shape::Card { x: 1.5, y, width: 4.5, height: 3.0, radius: 0.75 };
    // The frame's outer edge is 7.5 … 19.5 × 1 … 13; the stroke is centred half a stroke in.
    let half = STROKE / 2.0;
    vec![
        card(1.0),
        card(5.5),
        card(10.0),
        Shape::Rect { x: 7.5 + half, y: 1.0 + half, width: 12.0 - STROKE, height: 12.0 - STROKE, radius: 2.75, stroke: Some(STROKE) },
        Shape::Rect { x: 8.0, y: 4.0, width: 11.0, height: STROKE, radius: 0.0, stroke: None },
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

/// The status item's image: [`shapes`] as a resolution-independent template image (drawn at
/// whatever scale the menu bar needs). Main thread, as every AppKit drawing.
pub fn status_image() -> Retained<NSImage> {
    let draw = RcBlock::new(|_rect: NSRect| -> Bool {
        NSColor::blackColor().set();
        for shape in shapes() {
            match shape {
                Shape::Card { x, y, width, height, radius } => {
                    rounded_polygon(&Shape::card_corners(x, y, width, height), radius).fill();
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
        }
    }

    #[test]
    fn the_ink_is_twelve_points_tall_inside_the_image_like_the_symbols() {
        let boxes: Vec<_> = shapes().iter().map(outer).collect();
        let top = boxes.iter().map(|b| b.1).fold(f64::MAX, f64::min);
        let bottom = boxes.iter().map(|b| b.3).fold(f64::MIN, f64::max);
        let left = boxes.iter().map(|b| b.0).fold(f64::MAX, f64::min);
        let right = boxes.iter().map(|b| b.2).fold(f64::MIN, f64::max);
        assert_eq!((top, bottom), (1.0, 13.0));
        assert_eq!((left, SIZE.0 - right), (1.5, 1.5), "a symbol's side margins");
    }

    #[test]
    fn three_thumbnails_stack_left_of_the_window_with_a_gap() {
        let all = shapes();
        let cards: Vec<_> = all.iter().filter(|s| matches!(s, Shape::Card { .. })).map(outer).collect();
        assert_eq!(cards.len(), 3);
        let frame = all.iter().find(|s| matches!(s, Shape::Rect { stroke: Some(_), .. })).map(outer);
        let frame = frame.unwrap_or_default();
        for c in &cards {
            assert!(frame.0 - c.2 >= 1.25, "a clear gap between the strip and the window");
        }
        for pair in cards.windows(2) {
            assert!(pair[1].1 - pair[0].3 >= 1.25, "thumbnails do not touch");
        }
    }

    #[test]
    fn thumbnails_turn_away_toward_the_middle_like_the_strip() {
        let [_, top_right, bottom_right, _] = Shape::card_corners(0.0, 1.0, 4.0, 3.0);
        assert!(top_right.1 > 1.0 && bottom_right.1 > 4.0 - 1.0, "above the middle, both far ends drop");
        assert!((bottom_right.1 - top_right.1 - 3.0 * FAR_SCALE).abs() < 1e-9, "the far edge is shorter");
        let [_, tr, br, _] = Shape::card_corners(0.0, 5.5, 4.0, 3.0);
        assert!((tr.1 - 5.5 - (8.5 - br.1)).abs() < 1e-9, "the middle card is symmetric");
    }

    #[test]
    fn straight_edges_land_on_whole_pixels_at_2x() {
        for s in shapes() {
            let (l, t, r, b) = outer(&s);
            for edge in [l, t, r, b] {
                assert_eq!((edge * 2.0).fract(), 0.0, "{s:?} edge {edge}");
            }
        }
    }
}
