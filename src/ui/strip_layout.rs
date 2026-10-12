//! Strip geometry, pure: where each card, thumbnail and app icon goes, and the perspective
//! tilt. All values are points in the strip panel's own coordinates (top-left origin,
//! `y` grows down). Numbers come from `docs/UX-SPEC.md` ("Strip geometry").

use crate::engine::geometry::{Rect, Size};
use crate::ui::strip_model::CardSnapshot;

/// Width of the strip panel (the strip area Apple keeps clear is about 0–200 pt).
pub const STRIP_WIDTH: f64 = 200.0;
/// Left edge of every card.
pub const CARD_X: f64 = 15.0;
/// A card's arrangement of thumbnails fits this box (before the tilt shortens it).
pub const CARD_BOX: Size = Size::new(170.0, 177.0);
/// Space between card boxes.
pub const CARD_GAP: f64 = 22.0;
/// Space kept free at the top and bottom of the column.
pub const COLUMN_MARGIN: f64 = 16.0;
/// Horizontal step between windows of one app on a card.
pub const SAME_APP_DX: f64 = 17.0;
/// Offset of the second app's window from the first.
pub const SECOND_APP_DX: f64 = 68.0;
/// Upward offset of the second app's window (positive = up).
pub const SECOND_APP_UP: f64 = 14.0;
/// Each window behind another of its app is drawn this much smaller.
pub const BEHIND_SCALE: f64 = 0.94;
/// App icon frame (app icons carry a transparent margin; about 32 pt shows).
pub const ICON_SIZE: f64 = 36.0;
/// The icon frame's left edge sits this far left of the card (the visible icon about 6 pt).
pub const ICON_LEFT_OVERHANG: f64 = 8.0;
/// The icon frame's bottom sits this far below the card (the visible icon about 10 pt).
pub const ICON_BELOW: f64 = 12.0;
/// Horizontal step between app icons.
pub const ICON_STEP: f64 = 36.0;
/// Most app icons drawn on one card.
pub const MAX_ICONS: usize = 3;
/// Hovered card: scale and leftward shift.
pub const HOVER_SCALE: f64 = 1.065;
pub const HOVER_SHIFT: f64 = 5.0;

/// Perspective: eye distance (`m34 = -1 / d`).
pub const PERSPECTIVE_DISTANCE: f64 = 600.0;
/// Where a card's top and bottom edges meet when extended, measured from the card's left
/// edge. Measured on Apple's strip: about 2000 pt, at the display's vertical center, which
/// makes the far edge about 92–94 % of the near edge for a 130–150 pt wide card.
pub const VANISHING_DISTANCE: f64 = 2000.0;

/// One window's thumbnail on a card.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThumbSlot {
    /// `CGWindowID`.
    pub id: u32,
    /// Owning process.
    pub pid: i32,
    /// Position and size relative to the card's top-left corner.
    pub rect: Rect,
}

/// One card placed in the column.
#[derive(Clone, Debug, PartialEq)]
pub struct CardLayout {
    /// The stage this card shows.
    pub stage: u64,
    /// The card's box in panel coordinates, before the tilt (the left edge is exact; the
    /// tilt only shortens the right side).
    pub frame: Rect,
    /// Thumbnails, front window first (draw them back to front).
    pub thumbs: Vec<ThumbSlot>,
    /// App icons relative to the card's top-left corner: (pid, rect).
    pub icons: Vec<(i32, Rect)>,
}

/// Aspect-fits `frame`'s shape into `w × h`.
fn fit(frame: &Rect, w: f64, h: f64) -> Size {
    let aspect = if frame.w > 1.0 && frame.h > 1.0 { frame.w / frame.h } else { 4.0 / 3.0 };
    if aspect > w / h { Size::new(w, w / aspect) } else { Size::new(h * aspect, h) }
}

/// Arranges a card's thumbnails inside [`CARD_BOX`] scaled by `scale`; returns the card's
/// size and its thumbnails (relative to the card's top-left corner).
pub fn arrange_card(card: &CardSnapshot, scale: f64) -> (Size, Vec<ThumbSlot>) {
    let windows = &card.windows[..card.windows.len().min(3)];
    if windows.is_empty() {
        let s = Size::new(CARD_BOX.w * 0.6 * scale, CARD_BOX.h * 0.5 * scale);
        return (s, Vec::new());
    }
    // App group of each window: its pid's place in `apps` (first appearance as fallback),
    // capped at the second app; and its depth among earlier windows of the same group.
    let mut seen: Vec<i32> = Vec::new();
    let mut placed = Vec::with_capacity(windows.len());
    for w in windows {
        let group = match card.apps.iter().position(|&p| p == w.pid) {
            Some(i) => i,
            None => {
                if !seen.contains(&w.pid) {
                    seen.push(w.pid);
                }
                card.apps.len() + seen.iter().position(|&p| p == w.pid).unwrap_or(0)
            }
        };
        placed.push(group);
    }
    // Normalize groups to 0 (front window's app) / 1 (any other app).
    let front_group = placed[0];
    let groups: Vec<usize> = placed.iter().map(|&g| usize::from(g != front_group)).collect();
    let mut depth = vec![0usize; windows.len()];
    for i in 0..windows.len() {
        depth[i] = (0..i).filter(|&j| groups[j] == groups[i]).count();
    }
    let dx: Vec<f64> = (0..windows.len())
        .map(|i| groups[i] as f64 * SECOND_APP_DX + depth[i] as f64 * SAME_APP_DX)
        .collect();
    let up: Vec<f64> = groups.iter().map(|&g| g as f64 * SECOND_APP_UP).collect();
    let span_x = dx.iter().cloned().fold(0.0, f64::max);
    let span_y = up.iter().cloned().fold(0.0, f64::max);
    let box_w = (CARD_BOX.w - span_x).max(40.0);
    let box_h = (CARD_BOX.h - span_y).max(40.0);

    let sizes: Vec<Size> = windows
        .iter()
        .zip(&depth)
        .map(|(w, &d)| {
            let s = fit(&w.frame, box_w, box_h);
            let k = BEHIND_SCALE.powi(d as i32);
            Size::new(s.w * k, s.h * k)
        })
        .collect();
    // Vertical: every window centered on one line, the second app's raised by `up`.
    let center = sizes.iter().map(|s| s.h).fold(0.0, f64::max) / 2.0 + span_y;
    let mut rects: Vec<Rect> = (0..windows.len())
        .map(|i| Rect::new(dx[i], center - up[i] - sizes[i].h / 2.0, sizes[i].w, sizes[i].h))
        .collect();
    // Normalize to the union's top-left, then scale.
    let min_y = rects.iter().map(|r| r.y).fold(f64::INFINITY, f64::min);
    let max_x = rects.iter().map(|r| r.max_x()).fold(0.0, f64::max);
    let max_y = rects.iter().map(|r| r.max_y()).fold(0.0, f64::max);
    for r in &mut rects {
        *r = Rect::new(r.x * scale, (r.y - min_y) * scale, r.w * scale, r.h * scale);
    }
    let size = Size::new(max_x * scale, (max_y - min_y) * scale);
    let thumbs = windows.iter().zip(rects).map(|(w, rect)| ThumbSlot { id: w.id, pid: w.pid, rect }).collect();
    (size, thumbs)
}

/// The app icons of a card of `size`, relative to its top-left corner.
fn icons_for(card: &CardSnapshot, size: Size) -> Vec<(i32, Rect)> {
    let mut pids: Vec<i32> = card.apps.clone();
    for w in &card.windows {
        if !pids.contains(&w.pid) {
            pids.push(w.pid);
        }
    }
    pids.iter()
        .take(MAX_ICONS)
        .enumerate()
        .map(|(i, &pid)| {
            let x = -ICON_LEFT_OVERHANG + i as f64 * ICON_STEP;
            let y = size.h + ICON_BELOW - ICON_SIZE;
            (pid, Rect::new(x, y, ICON_SIZE, ICON_SIZE))
        })
        .collect()
}

/// Lays out the column: cards top to bottom, [`CARD_GAP`] apart, centered on `center_y`
/// and kept inside `0..height` (scaled down uniformly when they would not fit).
pub fn layout_column(cards: &[CardSnapshot], height: f64, center_y: f64) -> Vec<CardLayout> {
    if cards.is_empty() {
        return Vec::new();
    }
    let natural: Vec<f64> = cards.iter().map(|c| arrange_card(c, 1.0).0.h).collect();
    let gaps = CARD_GAP * (cards.len() - 1) as f64;
    let room = (height - 2.0 * COLUMN_MARGIN - ICON_BELOW - gaps).max(1.0);
    let sum: f64 = natural.iter().sum();
    let scale = if sum > room { (room / sum).max(0.2) } else { 1.0 };

    let arranged: Vec<(Size, Vec<ThumbSlot>)> = cards.iter().map(|c| arrange_card(c, scale)).collect();
    let total = arranged.iter().map(|(s, _)| s.h).sum::<f64>() + gaps;
    let top_min = COLUMN_MARGIN;
    let top_max = (height - COLUMN_MARGIN - ICON_BELOW - total).max(top_min);
    let mut y = (center_y - total / 2.0).clamp(top_min, top_max);

    let mut out = Vec::with_capacity(cards.len());
    for (card, (size, thumbs)) in cards.iter().zip(arranged) {
        let frame = Rect::new(CARD_X, y, size.w, size.h);
        out.push(CardLayout { stage: card.stage, frame, thumbs, icons: icons_for(card, size) });
        y += size.h + CARD_GAP;
    }
    out
}

/// The y-rotation (radians) that puts the vanishing point [`VANISHING_DISTANCE`] right of
/// the card's left edge, given the projection center's x (`center_x`, panel coordinates).
pub fn tilt_angle(center_x: f64) -> f64 {
    (PERSPECTIVE_DISTANCE / (VANISHING_DISTANCE + CARD_X - center_x)).atan()
}

/// Projects a card's right edge: returns (projected width, far edge height ratio) for a
/// card of width `w` whose left edge is at [`CARD_X`], seen through the perspective
/// centered at `center_x`.
pub fn projected(w: f64, center_x: f64) -> (f64, f64) {
    let theta = tilt_angle(center_x);
    let depth = 1.0 + w * theta.sin() / PERSPECTIVE_DISTANCE;
    let x = CARD_X + w * theta.cos();
    let projected_x = center_x + (x - center_x) / depth;
    (projected_x - CARD_X, 1.0 / depth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::strip_model::CardWindow;

    fn card(stage: u64, windows: &[(u32, i32, f64, f64)]) -> CardSnapshot {
        let mut apps = Vec::new();
        let windows = windows
            .iter()
            .map(|&(id, pid, w, h)| {
                if !apps.contains(&pid) {
                    apps.push(pid);
                }
                CardWindow { id, pid, frame: Rect::new(0.0, 0.0, w, h) }
            })
            .collect();
        CardSnapshot { stage, number: stage as usize, name: None, windows, apps }
    }

    #[test]
    fn a_single_window_keeps_its_aspect_inside_the_box() {
        let (size, thumbs) = arrange_card(&card(1, &[(1, 10, 1600.0, 1000.0)]), 1.0);
        assert_eq!(thumbs.len(), 1);
        assert!((size.w - CARD_BOX.w).abs() < 1e-9);
        assert!((size.w / size.h - 1.6).abs() < 1e-6);
        let (size, _) = arrange_card(&card(1, &[(1, 10, 600.0, 1000.0)]), 1.0);
        assert!((size.h - CARD_BOX.h).abs() < 1e-9);
        assert!(size.w <= CARD_BOX.w);
    }

    #[test]
    fn same_app_windows_step_right_and_shrink_behind_the_front() {
        let (_, t) = arrange_card(&card(1, &[(1, 10, 800.0, 600.0), (2, 10, 800.0, 600.0), (3, 10, 800.0, 600.0)]), 1.0);
        assert_eq!(t[1].rect.x - t[0].rect.x, SAME_APP_DX);
        assert_eq!(t[2].rect.x - t[1].rect.x, SAME_APP_DX);
        assert!(t[0].rect.w > t[1].rect.w && t[1].rect.w > t[2].rect.w);
        let mid = |r: &Rect| r.y + r.h / 2.0;
        assert!((mid(&t[0].rect) - mid(&t[2].rect)).abs() < 1e-9);
    }

    #[test]
    fn a_second_app_sits_right_and_up_and_the_card_fits_the_box() {
        let c = card(1, &[(1, 10, 800.0, 600.0), (2, 20, 800.0, 600.0)]);
        let (size, t) = arrange_card(&c, 1.0);
        assert_eq!(t[1].rect.x - t[0].rect.x, SECOND_APP_DX);
        let mid = |r: &Rect| r.y + r.h / 2.0;
        assert!((mid(&t[0].rect) - mid(&t[1].rect) - SECOND_APP_UP).abs() < 1e-9);
        assert!(size.w <= CARD_BOX.w + 1e-9 && size.h <= CARD_BOX.h + 1e-9);
        let icons = icons_for(&c, size);
        assert_eq!(icons.len(), 2);
        assert_eq!(icons[1].1.x - icons[0].1.x, ICON_STEP);
        assert_eq!(icons[0].1.x, -ICON_LEFT_OVERHANG);
        assert_eq!(icons[0].1.max_y(), size.h + ICON_BELOW);
    }

    #[test]
    fn the_column_is_centered_with_gaps_and_scales_down_when_full() {
        let cards: Vec<_> = (1..=3).map(|i| card(i, &[(i as u32, 1, 800.0, 600.0)])).collect();
        let l = layout_column(&cards, 1400.0, 700.0);
        assert_eq!(l.len(), 3);
        assert!((l[1].frame.y - l[0].frame.max_y() - CARD_GAP).abs() < 1e-9);
        let mid = (l[0].frame.y + l[2].frame.max_y()) / 2.0;
        assert!((mid - 700.0).abs() < 1e-6);
        assert!(l.iter().all(|c| c.frame.x == CARD_X));

        let many: Vec<_> = (1..=12).map(|i| card(i, &[(i as u32, 1, 600.0, 900.0)])).collect();
        let l = layout_column(&many, 900.0, 450.0);
        assert!(l[0].frame.y >= COLUMN_MARGIN - 1e-9);
        assert!(l.last().map(|c| c.frame.max_y() + ICON_BELOW).unwrap_or(0.0) <= 900.0 - COLUMN_MARGIN + 1e-6);
    }

    #[test]
    fn the_tilt_makes_the_far_edge_about_92_percent() {
        let (w, ratio) = projected(150.0, STRIP_WIDTH / 2.0);
        assert!(w < 150.0 && w > 120.0, "projected width {w}");
        assert!((0.90..0.95).contains(&ratio), "far edge ratio {ratio}");
        // The edges meet at the vanishing point: height shrinks linearly with projected x.
        assert!(((1.0 - ratio) * VANISHING_DISTANCE - w).abs() < 1.0);
    }
}
