//! Where the window is in its picture: a capture's size and the window's place within it.
//!
//! `SCContentFilter(desktopIndependentWindow:)` takes the window **with its child windows**
//! (a popover, a sheet): when a child sticks out past the window, the picture is the union
//! of the two. Apple's Stage Manager shows exactly that (the popup sticking out of the
//! thumbnail, the owner checked 10-16), so the union is kept; what changed is how it is
//! drawn: the picture fitted into the window's own frame drew the window shrunk inside a
//! dark box (Calendar with a popup open, 10-16). Now each picture says where the window is
//! in it ([`Extent`]): how far it reaches past the window on each side, in window sizes, so a
//! card (or a ghost) draws the window at its true size where it is and the child's part
//! beside it ([`Extent::around`]). The capture's background is clear, so no area outside a
//! window is ever opaque.
//!
//! Where the window sits within the filter's content: the content rectangle is global when it
//! holds the window's frame; else the union of the window and the windows of its app touching
//! it (its children) that has the content's size gives the content's origin. When neither
//! tells, the picture is taken as the window, as before.
//!
//! Pure geometry in points (global, top-left origin), unit tested.

use crate::engine::geometry::Rect;

/// Rounding between the window server's frames and ScreenCaptureKit's content, in points.
const SLACK: f64 = 1.0;

/// How far a picture reaches past its window on each side, in the window's own width
/// (left, right) and height (top, bottom). All zero: the picture is the window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Extent {
    /// Past the window's left edge, in window widths.
    pub left: f64,
    /// Past its top edge, in window heights.
    pub top: f64,
    /// Past its right edge, in window widths.
    pub right: f64,
    /// Past its bottom edge, in window heights.
    pub bottom: f64,
}

impl Extent {
    /// The picture is the window.
    pub const NONE: Extent = Extent { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };

    /// True when the picture is the window.
    pub fn is_none(&self) -> bool {
        *self == Self::NONE
    }

    /// Where the picture goes when its window is drawn at `window` (any coordinates, any
    /// scale): the window at that rectangle, the children's parts around it.
    pub fn around(&self, window: Rect) -> Rect {
        Rect::new(
            window.x - self.left * window.w,
            window.y - self.top * window.h,
            window.w * (1.0 + self.left + self.right),
            window.h * (1.0 + self.top + self.bottom),
        )
    }

    /// The inverse of [`Extent::around`]: where the window is when the picture is drawn at
    /// `picture`.
    pub fn window_in(&self, picture: Rect) -> Rect {
        let w = picture.w / (1.0 + self.left + self.right);
        let h = picture.h / (1.0 + self.top + self.bottom);
        Rect::new(picture.x + self.left * w, picture.y + self.top * h, w, h)
    }
}

/// How to capture one window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// Where the window is in the picture.
    pub extent: Extent,
    /// The picture's size in pixels.
    pub size: (u32, u32),
}

/// How to capture a window whose frame is `window` (global points) through a filter whose
/// content is `content` (points) at `scale` pixels per point, the window asked at `asked`
/// pixels. `others`: the frames of the window's app's other windows (its children among
/// them). The window's part of the picture gets the size asked (never more than its own
/// pixels), in its proportions; the children's parts come at the same scale around it.
pub fn plan(asked: (u32, u32), content: Rect, scale: f64, window: Rect, others: &[Rect]) -> Plan {
    let scale = if scale.is_finite() { scale.max(1.0) } else { 1.0 };
    let Some(inside) = window_in_content(content, window, others) else {
        return Plan { extent: Extent::NONE, size: fit_to_content(asked, (content.w * scale, content.h * scale)) };
    };
    let (w, h) = (window.w * scale, window.h * scale);
    let k = (f64::from(asked.0.max(1)) / w).min(f64::from(asked.1.max(1)) / h).min(1.0);
    let size = ((content.w * scale * k).round().max(1.0) as u32, (content.h * scale * k).round().max(1.0) as u32);
    let extent = Extent {
        left: inside.x / window.w,
        top: inside.y / window.h,
        right: ((content.w - inside.max_x()) / window.w).max(0.0),
        bottom: ((content.h - inside.max_y()) / window.h).max(0.0),
    };
    Plan { extent, size }
}

/// The window's frame within the content (from the content's top-left corner), or `None`
/// when the content is the window or where the window sits is not known.
pub fn window_in_content(content: Rect, window: Rect, others: &[Rect]) -> Option<Rect> {
    if content.is_degenerate() || window.is_degenerate() {
        return None;
    }
    if near(content.w, window.w) && near(content.h, window.h) {
        return None;
    }
    let origin = children_union(content, window, others)
        .or_else(|| holds(&content, &window).then_some(content))?
        .origin();
    let local = Rect::new(window.x - origin.x, window.y - origin.y, window.w, window.h);
    // Inside the content, give or take the rounding: snapped to it.
    let clip = Rect::new(0.0, 0.0, content.w, content.h);
    let inside = local.intersect(&clip)?;
    (near(inside.w, window.w) && near(inside.h, window.h)).then(|| {
        let x = local.x.clamp(0.0, (content.w - window.w).max(0.0));
        let y = local.y.clamp(0.0, (content.h - window.h).max(0.0));
        Rect::new(x, y, window.w.min(content.w), window.h.min(content.h))
    })
}

/// The union of the window and the app's windows touching it (all of them, then each alone)
/// that has the content's size: where the content is, globally.
fn children_union(content: Rect, window: Rect, others: &[Rect]) -> Option<Rect> {
    let reach = Rect::new(window.x - SLACK, window.y - SLACK, window.w + 2.0 * SLACK, window.h + 2.0 * SLACK);
    let touching: Vec<&Rect> = others.iter().filter(|o| !o.is_degenerate() && o.intersect(&reach).is_some()).collect();
    let fits = |u: &Rect| near(u.w, content.w) && near(u.h, content.h);
    let all = touching.iter().fold(window, |u, o| u.union(o));
    if !touching.is_empty() && fits(&all) {
        return Some(all);
    }
    touching.iter().map(|o| window.union(o)).find(fits)
}

/// `outer` holds `inner`, give or take the rounding.
fn holds(outer: &Rect, inner: &Rect) -> bool {
    inner.x >= outer.x - SLACK
        && inner.y >= outer.y - SLACK
        && inner.max_x() <= outer.max_x() + SLACK
        && inner.max_y() <= outer.max_y() + SLACK
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= SLACK
}

/// The image size for a capture asked at `asked` pixels of a window whose content is
/// `content` pixels: the largest size of the content's proportions that fits in `asked`,
/// never larger than the content itself (so the image is the window, edge to edge, with no
/// padding). An unknown content size keeps what was asked.
pub fn fit_to_content(asked: (u32, u32), content: (f64, f64)) -> (u32, u32) {
    let (cw, ch) = content;
    if !(cw >= 1.0 && ch >= 1.0) {
        return (asked.0.max(1), asked.1.max(1));
    }
    let k = (f64::from(asked.0.max(1)) / cw).min(f64::from(asked.1.max(1)) / ch).min(1.0);
    ((cw * k).round().max(1.0) as u32, (ch * k).round().max(1.0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 10-16 Calendar window and its popup.
    const CAL: Rect = Rect::new(861.0, 214.0, 1433.0, 1139.0);
    const POPUP: Rect = Rect::new(2125.0, 378.0, 324.0, 726.0);

    fn close(a: Rect, b: Rect) -> bool {
        [(a.x, b.x), (a.y, b.y), (a.w, b.w), (a.h, b.h)].iter().all(|(p, q)| (p - q).abs() < 1e-6)
    }

    #[test]
    fn a_capture_keeps_the_windows_proportions_and_never_grows_past_it() {
        // Asked at its own size: as asked.
        assert_eq!(fit_to_content((4202, 2540), (4202.0, 2540.0)), (4202, 2540));
        // Asked wider than the window (a stale frame): the window's shape, no padding.
        assert_eq!(fit_to_content((5042, 2540), (4202.0, 2540.0)), (4202, 2540));
        // Asked smaller and narrower: scaled down in the window's shape.
        assert_eq!(fit_to_content((300, 300), (4202.0, 2540.0)), (300, 181));
        // Unknown content: as asked.
        assert_eq!(fit_to_content((10, 0), (0.0, 0.0)), (10, 1));
    }

    #[test]
    fn no_child_the_picture_is_the_window() {
        assert_eq!(window_in_content(CAL, CAL, &[]), None);
        // Rounding between the two is not a child.
        let content = Rect::new(861.0, 214.0, 1433.5, 1138.6);
        assert_eq!(window_in_content(content, CAL, &[]), None);
        // A local content rectangle of the window's size: the same.
        assert_eq!(window_in_content(Rect::new(0.0, 0.0, 1433.0, 1139.0), CAL, &[]), None);
        let p = plan((600, 600), CAL, 2.0, CAL, &[]);
        assert_eq!(p, Plan { extent: Extent::NONE, size: (600, 477) });
        // Drawn where the window is drawn.
        let card = Rect::new(10.0, 20.0, 160.0, 127.0);
        assert_eq!(p.extent.around(card), card);
    }

    #[test]
    fn a_child_to_the_right_sticks_out_right() {
        // The 10-16 case: the popup sticks out 155 pt past the right edge.
        let content = CAL.union(&POPUP);
        assert_eq!(content.w, 1588.0);
        let at = Some(Rect::new(0.0, 0.0, 1433.0, 1139.0));
        // A global content rectangle.
        assert_eq!(window_in_content(content, CAL, &[]), at);
        // A local one (from 0,0): the popup among the app's windows tells where it is.
        assert_eq!(window_in_content(Rect::new(0.0, 0.0, 1588.0, 1139.0), CAL, &[POPUP]), at);
        let p = plan((600, 600), content, 2.0, CAL, &[POPUP]);
        assert_eq!(p.extent, Extent { left: 0.0, top: 0.0, right: 155.0 / 1433.0, bottom: 0.0 });
        // The window's part gets the size asked (600 x 477); the popup's part beside it.
        assert_eq!(p.size, (665, 477));
        // On a card the window keeps the window's rectangle; the picture reaches past it.
        let card = Rect::new(10.0, 20.0, 143.3, 113.9);
        let drawn = p.extent.around(card);
        assert!(close(drawn, Rect::new(10.0, 20.0, 158.8, 113.9)), "{drawn:?}");
        assert!(close(p.extent.window_in(drawn), card));
    }

    #[test]
    fn a_child_to_the_left_sticks_out_left() {
        let child = Rect::new(800.0, 400.0, 200.0, 300.0);
        let content = CAL.union(&child);
        let at = Some(Rect::new(61.0, 0.0, 1433.0, 1139.0));
        assert_eq!(window_in_content(content, CAL, &[child]), at);
        assert_eq!(window_in_content(Rect::new(0.0, 0.0, content.w, content.h), CAL, &[child]), at);
        let e = plan((1433, 1139), content, 1.0, CAL, &[child]).extent;
        assert_eq!(e, Extent { left: 61.0 / 1433.0, top: 0.0, right: 0.0, bottom: 0.0 });
        let drawn = e.around(Rect::new(100.0, 100.0, 1433.0, 1139.0));
        assert!(close(drawn, Rect::new(39.0, 100.0, 1494.0, 1139.0)), "{drawn:?}");
    }

    #[test]
    fn a_child_above_sticks_out_above() {
        let child = Rect::new(1000.0, 150.0, 300.0, 200.0);
        let content = CAL.union(&child);
        let at = Some(Rect::new(0.0, 64.0, 1433.0, 1139.0));
        assert_eq!(window_in_content(content, CAL, &[child]), at);
        assert_eq!(window_in_content(Rect::new(0.0, 0.0, content.w, content.h), CAL, &[child]), at);
        let e = plan((1433, 1139), content, 1.0, CAL, &[child]).extent;
        assert_eq!(e, Extent { left: 0.0, top: 64.0 / 1139.0, right: 0.0, bottom: 0.0 });
    }

    #[test]
    fn a_child_below_sticks_out_below() {
        let child = Rect::new(1000.0, 1200.0, 300.0, 250.0);
        let content = CAL.union(&child);
        let at = Some(Rect::new(0.0, 0.0, 1433.0, 1139.0));
        assert_eq!(window_in_content(content, CAL, &[child]), at);
        assert_eq!(window_in_content(Rect::new(0.0, 0.0, content.w, content.h), CAL, &[child]), at);
        let e = plan((1433, 1139), content, 1.0, CAL, &[child]).extent;
        assert_eq!(e, Extent { left: 0.0, top: 0.0, right: 0.0, bottom: 97.0 / 1139.0 });
    }

    #[test]
    fn a_child_inside_the_window_changes_nothing() {
        let child = Rect::new(1000.0, 300.0, 300.0, 200.0);
        assert_eq!(window_in_content(CAL, CAL, &[child]), None);
        assert!(plan((600, 600), CAL, 2.0, CAL, &[child]).extent.is_none());
    }

    #[test]
    fn retina_sizes_the_windows_part_in_pixels() {
        let content = CAL.union(&POPUP);
        // Asked larger than the window: the window's own pixels at 2x, the popup's beside.
        let p = plan((4000, 4000), content, 2.0, CAL, &[POPUP]);
        assert_eq!(p.size, (3176, 2278));
        // At 1x.
        let p = plan((4000, 4000), content, 1.0, CAL, &[POPUP]);
        assert_eq!(p.size, (1588, 1139));
        // A scale ScreenCaptureKit did not give: 1x.
        assert_eq!(plan((4000, 4000), content, f64::NAN, CAL, &[POPUP]).size, (1588, 1139));
        // The extent does not depend on the scale.
        assert_eq!(plan((300, 300), content, 2.0, CAL, &[POPUP]).extent, p.extent);
    }

    #[test]
    fn a_window_partly_off_screen_is_found_the_same() {
        // Parked in the bottom-right corner: most of it past the screen's edges.
        let parked = Rect::new(3000.0, 1900.0, 800.0, 600.0);
        let child = Rect::new(3700.0, 2000.0, 300.0, 200.0);
        let content = parked.union(&child);
        assert_eq!(window_in_content(content, parked, &[child]), Some(Rect::new(0.0, 0.0, 800.0, 600.0)));
        assert_eq!(plan((800, 600), content, 1.0, parked, &[child]).extent.right, 0.25);
        // Off the left edge, a child further left.
        let left = Rect::new(-300.0, 100.0, 800.0, 600.0);
        let child = Rect::new(-450.0, 200.0, 200.0, 100.0);
        let content = left.union(&child);
        assert_eq!(window_in_content(content, left, &[child]), Some(Rect::new(150.0, 0.0, 800.0, 600.0)));
    }

    #[test]
    fn where_the_window_sits_unknown_takes_the_picture_as_the_window() {
        // A local content rectangle larger than the window and no child seen.
        assert_eq!(window_in_content(Rect::new(0.0, 0.0, 1588.0, 1139.0), CAL, &[]), None);
        // A child seen, but its union is not the content's size.
        let far = Rect::new(2300.0, 378.0, 324.0, 726.0);
        assert_eq!(window_in_content(Rect::new(0.0, 0.0, 1588.0, 1139.0), CAL, &[far]), None);
        // No frame known.
        assert_eq!(window_in_content(CAL.union(&POPUP), Rect::default(), &[POPUP]), None);
        let p = plan((600, 600), Rect::new(0.0, 0.0, 1588.0, 1139.0), 2.0, Rect::default(), &[]);
        assert_eq!(p, Plan { extent: Extent::NONE, size: (600, 430) });
    }

    #[test]
    fn another_window_of_the_app_beside_it_is_not_taken_for_the_child() {
        // Two windows of the app overlap the window; only the popup makes the content's size.
        let other = Rect::new(500.0, 100.0, 600.0, 400.0);
        let content = CAL.union(&POPUP);
        let local = Rect::new(0.0, 0.0, content.w, content.h);
        assert_eq!(window_in_content(local, CAL, &[other, POPUP]), Some(Rect::new(0.0, 0.0, 1433.0, 1139.0)));
    }
}
