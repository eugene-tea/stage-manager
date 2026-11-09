//! Strip geometry, pure: where each card, thumbnail and app icon goes, and the perspective.
//! Points in the strip panel's own coordinates (top-left origin, `y` grows down; the panel
//! starts at the visible frame's top-left corner).
//!
//! Apple's model, measured on a 1800 × 1169 pt laptop and a 5K display (the measurement
//! record: `docs/UX-SPEC.md` as of commit 20665a3, "Strip geometry"), with every number from
//! `[cards]` and `[icons]` in config:
//!
//! - **Size.** Every window is scaled by one factor: `full_height` over the visible height
//!   (161 pt: a window as tall as the visible screen becomes a 161 pt card), drawn at least
//!   `min_height` tall, never wider than a card of the whole screen.
//! - **Turn.** Each window's thumbnail is its own plane, turned by `tilt` about its left
//!   edge (the right side away from the viewer), seen through one perspective whose eye is
//!   `perspective` points in front of (`perspective_x`, the display's vertical middle). So a
//!   card above the middle shows its top edge sloping down to the right, one below shows its
//!   bottom edge sloping up, and every far edge is shorter by `D / (D + u sin tilt)`.
//! - **Column.** Cards' left edges at `x`; `gap` between one card's left edge bottom and the
//!   next one's top; the column centered on the visible frame.
//! - **Stack.** Windows of one app step `stack_dx` to the right toward the front one (the
//!   window behind peeks out on the left, as on Apple's strip); a second app's windows sit
//!   `second_app_dx` right and `second_app_dy` up.
//! - **Icon.** Centered `center_dx` right and `center_dy` down from the bottom-left corner of
//!   the card's leftmost window.
//!
//! Every length is multiplied by the strip's length scale `k` (1 unless `[strip] scale` or
//! `scale_to_display` say otherwise).

use crate::config::StripStyle;
use crate::engine::geometry::{Point, Rect, Size};
use crate::ui::strip_model::CardSnapshot;

/// The two displays the automatic tilt and perspective were measured on: (visible width,
/// tilt in degrees, eye distance in points).
pub const MEASURED_PERSPECTIVE: [(f64, f64, f64); 2] = [(1800.0, 44.2, 1346.0), (2560.0, 47.9, 2260.0)];

/// Apple's tilt (degrees) and eye distance (points) for a visible width: linear between the
/// two measured displays, held at the nearer one outside them.
pub fn auto_perspective(visible_width: f64) -> (f64, f64) {
    let [(w0, t0, d0), (w1, t1, d1)] = MEASURED_PERSPECTIVE;
    let t = if visible_width.is_finite() { ((visible_width - w0) / (w1 - w0)).clamp(0.0, 1.0) } else { 1.0 };
    (t0 + (t1 - t0) * t, d0 + (d1 - d0) * t)
}

/// The style resolved for one display: every length scaled by `k`, auto values computed.
#[derive(Clone, Debug, PartialEq)]
pub struct Metrics {
    /// The length scale.
    pub k: f64,
    /// The visible frame's size.
    pub visible: Size,
    /// The column (and panel) width.
    pub width: f64,
    /// Cards' left edge.
    pub card_x: f64,
    pub full_height: f64,
    pub min_height: f64,
    /// Widest unturned card.
    pub max_width: f64,
    pub gap: f64,
    pub stack_dx: f64,
    pub behind_scale: f64,
    pub second_app_dx: f64,
    pub second_app_dy: f64,
    pub max_windows: usize,
    /// The turn, radians.
    pub tilt: f64,
    /// Eye distance.
    pub perspective: f64,
    /// The eye, panel coordinates.
    pub eye: Point,
    pub icon_size: f64,
    pub icon_dx: f64,
    pub icon_dy: f64,
    pub icon_step: f64,
    pub icon_max: usize,
    pub hover_scale: f64,
    /// How far a thumbnail's or icon's shadow reaches past it, sideways.
    pub shadow: f64,
}

impl Metrics {
    /// Resolves `style` for a display: `visible` is its visible frame's size, `k` the strip's
    /// length scale, `display_middle` the display's vertical middle in panel coordinates.
    pub fn new(style: &StripStyle, visible: Size, k: f64, display_middle: f64) -> Self {
        let c = &style.cards;
        let i = &style.icons;
        let (auto_tilt, auto_d) = auto_perspective(visible.w);
        let full_height = c.full_height * k;
        let screen_aspect = if visible.h > 1.0 { visible.w / visible.h } else { 16.0 / 10.0 };
        let max_width = if c.max_width > 0.0 { c.max_width * k } else { full_height * screen_aspect };
        Self {
            k,
            visible,
            width: (style.width * k).round(),
            card_x: c.x * k,
            full_height,
            min_height: c.min_height * k,
            max_width,
            gap: c.gap * k,
            stack_dx: c.stack_dx * k,
            behind_scale: c.behind_scale,
            second_app_dx: c.second_app_dx * k,
            second_app_dy: c.second_app_dy * k,
            max_windows: c.max_windows.max(1),
            tilt: c.tilt.or(auto_tilt).to_radians(),
            perspective: c.perspective.or(auto_d) * k,
            eye: Point::new(c.perspective_x * k, display_middle + c.perspective_y * k),
            icon_size: i.size * k,
            icon_dx: i.center_dx * k,
            icon_dy: i.center_dy * k,
            icon_step: i.step * k,
            icon_max: i.max,
            hover_scale: c.hover_scale,
            shadow: (c.shadow_radius + c.shadow_offset_x.abs()).max(i.shadow_radius) * k,
        }
    }

    /// A window's thumbnail before the turn: the window scaled by `full_height / visible
    /// height`, at least `min_height` tall, at most `max_width` wide (aspect kept).
    pub fn thumb_size(&self, frame: &Rect) -> Size {
        let (w, h) = if frame.w > 1.0 && frame.h > 1.0 { (frame.w, frame.h) } else { (800.0, 600.0) };
        let aspect = w / h;
        let mut v = (self.full_height * h / self.visible.h.max(1.0)).max(self.min_height);
        let mut u = v * aspect;
        if u > self.max_width {
            u = self.max_width;
            v = u / aspect;
        }
        Size::new(u, v)
    }

    /// How much the far edge of a plane `u` wide shrinks (`D / (D + u sin tilt)`).
    pub fn far_ratio(&self, u: f64) -> f64 {
        self.far_ratio_turned(u, self.tilt)
    }

    /// [`Metrics::far_ratio`] for a plane turned by `tilt` (radians), not the cards' turn.
    fn far_ratio_turned(&self, u: f64, tilt: f64) -> f64 {
        let d = self.perspective.max(1.0);
        d / (d + u * tilt.sin())
    }

    /// The four corners of a thumbnail whose left edge runs from (`x`, `top`) down `v`, `u`
    /// wide before the turn: top-left, top-right, bottom-right, bottom-left.
    pub fn project(&self, x: f64, top: f64, u: f64, v: f64) -> [Point; 4] {
        self.project_turned(x, top, u, v, self.tilt)
    }

    /// [`Metrics::project`] for a plane turned by `tilt` (radians) about its left edge, seen
    /// from the cards' eye: a snapshot on its way between a card and its window.
    pub fn project_turned(&self, x: f64, top: f64, u: f64, v: f64, tilt: f64) -> [Point; 4] {
        let f = self.far_ratio_turned(u, tilt);
        let rx = self.eye.x + (x + u * tilt.cos() - self.eye.x) * f;
        let ry = |y: f64| self.eye.y + (y - self.eye.y) * f;
        [Point::new(x, top), Point::new(rx, ry(top)), Point::new(rx, ry(top + v)), Point::new(x, top + v)]
    }

    /// The depth (toward the eye) of the turned plane whose left edge is at `x`, at the point
    /// it covers `px` across before the projection: `-(px - x) tan tilt`. Of two planes over
    /// the same point the one with the greater depth is nearer the eye, and the turn is the
    /// same for every plane, so the one whose left edge is further right always is.
    pub fn depth_at(&self, x: f64, px: f64) -> f64 {
        -(px - x) * self.tilt.tan()
    }

    /// The bounding box of [`Metrics::project`].
    pub fn projected_bounds(&self, x: f64, top: f64, u: f64, v: f64) -> Rect {
        self.projected_bounds_turned(x, top, u, v, self.tilt)
    }

    /// The bounding box of [`Metrics::project_turned`].
    pub fn projected_bounds_turned(&self, x: f64, top: f64, u: f64, v: f64, tilt: f64) -> Rect {
        let q = self.project_turned(x, top, u, v, tilt);
        let (x0, x1) = (q.iter().map(|p| p.x).fold(f64::INFINITY, f64::min), q.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max));
        let (y0, y1) = (q.iter().map(|p| p.y).fold(f64::INFINITY, f64::min), q.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max));
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

impl Metrics {
    /// Every place a window's left edge can take on a card, before the turn, right of the
    /// card's left edge ([`arrange_card`]): the front app's windows step `stack_dx` (the
    /// deepest at 0), a second app's start `second_app_dx` right (at most `max_windows - 1`
    /// of them). A column too full shrinks them toward 0.
    fn plane_offsets(&self) -> Vec<f64> {
        let n = self.max_windows;
        let front = (0..n).map(|j| j as f64 * self.stack_dx);
        let second = (0..n.saturating_sub(1)).map(|j| self.second_app_dx + j as f64 * self.stack_dx);
        front.chain(second).chain(std::iter::once(0.0)).collect()
    }

    /// The far edge of a window's plane whose left edge is at `x`, `u` wide before the turn
    /// (the right side of [`Metrics::project`]).
    fn far_x(&self, x: f64, u: f64) -> f64 {
        self.eye.x + (x + u * self.tilt.cos() - self.eye.x) * self.far_ratio(u)
    }

    /// The rightmost point any card can draw at rest in its slot (panel coordinates): the far
    /// edge of the widest window (`max_width`; the far edge only moves right as a plane
    /// widens, or never past its left edge) at the rightmost left edge, or an app icon's edge.
    fn card_right(&self) -> f64 {
        let x = self.card_x + self.plane_offsets().into_iter().fold(f64::NEG_INFINITY, f64::max);
        let plane = x.max(self.far_x(x, self.max_width));
        plane.max(self.icons_right())
    }

    /// The right edge of a card's last app icon at most (the icons start at the leftmost
    /// window's corner, never right of the card's left edge: the front app's deepest window
    /// sits there).
    fn icons_right(&self) -> f64 {
        if self.icon_size <= 0.0 || self.icon_max == 0 {
            return f64::NEG_INFINITY;
        }
        // A column too full scales the icons' offsets and size toward the corner.
        let last = self.icon_dx + (self.icon_max - 1) as f64 * self.icon_step;
        self.card_x + (last + self.icon_size / 2.0).max(0.0)
    }

    /// The leftmost point any card can draw at rest (panel coordinates): its leftmost
    /// window's left edge, or the icon on its corner.
    fn card_left(&self) -> f64 {
        let x = self.card_x + self.plane_offsets().into_iter().fold(f64::INFINITY, f64::min);
        if self.icon_size <= 0.0 || self.icon_max == 0 {
            return x;
        }
        x + (self.icon_dx - self.icon_size / 2.0).min(0.0)
    }

    /// The strip panel's width: the column, and room right of it for the widest thing a card
    /// can draw, so nothing is cut off at the column's edge (owner 11-09: a second app's
    /// window was). A card grown under the pointer (`hover_scale`, about its middle) spans
    /// `hover_scale` times the widest card; dragged within the column it follows the pointer,
    /// pressed at its left edge at the most, up to the column's right edge, so its right part
    /// reaches that span past the column. Shadows add theirs. Never wider than the screen.
    pub fn panel_width(&self) -> f64 {
        let (left, right) = (self.card_left(), self.card_right());
        let grow = self.hover_scale.max(1.0);
        let span = (right - left).max(0.0) * grow;
        // A card grown in its slot: its middle lies within it, so it reaches no further right
        // than its right edge plus its growth.
        let grown = right + (right - left).max(0.0) * (grow - 1.0);
        let reach = (self.width + span).max(grown) + self.shadow;
        reach.ceil().max(self.width).min(self.visible.w.max(self.width))
    }

    /// How far left the cards slide to leave the screen whole when the strip hides: the far
    /// edge of the widest card moves `dx` times its far-edge ratio (the slide is in 3D, before
    /// the perspective), its left edge and icons by `dx`.
    pub fn hide_distance(&self) -> f64 {
        let x = self.card_x + self.plane_offsets().into_iter().fold(f64::NEG_INFINITY, f64::max);
        // Every point of a plane moves at least as far as its far edge does.
        let far = x.max(self.far_x(x, self.max_width)) / self.far_ratio(self.max_width);
        far.max(self.icons_right()).max(0.0)
    }
}

/// One window's thumbnail on a card.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThumbSlot {
    /// `CGWindowID`.
    pub id: u32,
    /// Owning process.
    pub pid: i32,
    /// Its left edge's top relative to the card's top-left corner, and its size before the
    /// turn.
    pub rect: Rect,
}

/// One card placed in the column.
#[derive(Clone, Debug, PartialEq)]
pub struct CardLayout {
    /// The stage this card shows.
    pub stage: u64,
    /// The card before the turn, panel coordinates: `x` is the cards' left edge, `y` the
    /// top of its highest left edge, `h` the span of its left edges.
    pub frame: Rect,
    /// Thumbnails, front window first (draw them back to front).
    pub thumbs: Vec<ThumbSlot>,
    /// App icons relative to the card's top-left corner: (pid, rect).
    pub icons: Vec<(i32, Rect)>,
}

impl CardLayout {
    /// The thumbnails in the order the card draws them, back to front: the list reversed, so
    /// the front window is on top, then the others in their order, within an app's group and
    /// across apps. The turn alone would stack them by depth instead: wherever two overlap,
    /// the one whose left edge is further right is nearer the eye
    /// ([`Metrics::depth_at`]), and a second app's windows sit `second_app_dx` right.
    pub fn draw_order(&self) -> Vec<ThumbSlot> {
        self.thumbs.iter().rev().copied().collect()
    }

    /// What the card covers on screen (panel coordinates): its turned thumbnails and icons.
    pub fn bounds(&self, m: &Metrics) -> Rect {
        let mut r: Option<Rect> = None;
        let mut add = |b: Rect| r = Some(r.map_or(b, |a| a.union(&b)));
        for t in &self.thumbs {
            add(m.projected_bounds(self.frame.x + t.rect.x, self.frame.y + t.rect.y, t.rect.w, t.rect.h));
        }
        for (_, i) in &self.icons {
            add(i.translate(self.frame.x, self.frame.y));
        }
        r.unwrap_or(self.frame)
    }

    /// The thumbnail under `p` (panel coordinates, the card as laid out, unscaled), as its
    /// index in [`CardLayout::thumbs`]: the frontmost drawn whose turned plane holds `p`
    /// (the card draws the list back to front, so the first that holds it is on top).
    pub fn thumb_at(&self, m: &Metrics, p: Point) -> Option<usize> {
        self.thumbs.iter().position(|t| in_quad(&m.project(self.frame.x + t.rect.x, self.frame.y + t.rect.y, t.rect.w, t.rect.h), p))
    }

    /// The center hover grows the card about: the middle of its turned thumbnails.
    pub fn hover_center(&self, m: &Metrics) -> Point {
        let mut r: Option<Rect> = None;
        for t in &self.thumbs {
            let b = m.projected_bounds(self.frame.x + t.rect.x, self.frame.y + t.rect.y, t.rect.w, t.rect.h);
            r = Some(r.map_or(b, |a| a.union(&b)));
        }
        let r = r.unwrap_or(self.frame);
        Point::new(r.x + r.w / 2.0, r.y + r.h / 2.0)
    }
}

/// True when `p` lies in the convex quadrilateral `q` (its corners in order, either way
/// round), its edges included.
fn in_quad(q: &[Point; 4], p: Point) -> bool {
    let sides: Vec<f64> = (0..4)
        .map(|i| {
            let (a, b) = (q[i], q[(i + 1) % 4]);
            (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
        })
        .collect();
    sides.iter().all(|&c| c >= 0.0) || sides.iter().all(|&c| c <= 0.0)
}

/// Arranges a card's thumbnails; `scale` shrinks everything further (a column too full).
/// Returns the card's size (widest reach before the turn, span of the left edges) and its
/// thumbnails relative to its top-left corner.
pub fn arrange_card(card: &CardSnapshot, m: &Metrics, scale: f64) -> (Size, Vec<ThumbSlot>) {
    let windows = &card.windows[..card.windows.len().min(m.max_windows)];
    if windows.is_empty() {
        let s = m.thumb_size(&Rect::new(0.0, 0.0, 800.0, 600.0));
        return (Size::new(s.w * scale, s.h * scale), Vec::new());
    }
    // App group of each window: 0 for the front window's app, 1 for any other; and its depth
    // among earlier windows of the same group.
    let groups: Vec<usize> = windows.iter().map(|w| usize::from(w.pid != windows[0].pid)).collect();
    let depth: Vec<usize> = (0..windows.len()).map(|i| (0..i).filter(|&j| groups[j] == groups[i]).count()).collect();
    let sizes: Vec<Size> = windows
        .iter()
        .zip(&depth)
        .map(|(w, &d)| {
            let s = m.thumb_size(&w.frame);
            let b = m.behind_scale.powi(d as i32);
            Size::new(s.w * b, s.h * b)
        })
        .collect();
    // Apple steps toward the front: the deepest window of a group sits leftmost.
    let in_group = |g: usize| groups.iter().filter(|&&x| x == g).count();
    let dx: Vec<f64> = (0..windows.len())
        .map(|i| groups[i] as f64 * m.second_app_dx + (in_group(groups[i]) - 1 - depth[i]) as f64 * m.stack_dx)
        .collect();
    let up: Vec<f64> = groups.iter().map(|&g| g as f64 * m.second_app_dy).collect();
    // Every window centered on one line, the second app's raised by `up`.
    let line = sizes.iter().map(|s| s.h).fold(0.0, f64::max) / 2.0 + up.iter().cloned().fold(0.0, f64::max);
    let rects: Vec<Rect> = (0..windows.len()).map(|i| Rect::new(dx[i], line - up[i] - sizes[i].h / 2.0, sizes[i].w, sizes[i].h)).collect();
    let min_y = rects.iter().map(|r| r.y).fold(f64::INFINITY, f64::min);
    let max_x = rects.iter().map(|r| r.max_x()).fold(0.0, f64::max);
    let max_y = rects.iter().map(|r| r.max_y()).fold(0.0, f64::max);
    let thumbs = windows
        .iter()
        .zip(rects)
        .map(|(w, r)| ThumbSlot {
            id: w.id,
            pid: w.pid,
            rect: Rect::new(r.x * scale, (r.y - min_y) * scale, r.w * scale, r.h * scale),
        })
        .collect();
    (Size::new(max_x * scale, (max_y - min_y) * scale), thumbs)
}

/// The app icons of a card, relative to its top-left corner: centered `icon_dx` right and
/// `icon_dy` down from the bottom-left corner of the card's leftmost window (the front-most
/// of those at the left edge), the next app `icon_step` right.
fn icons_for(card: &CardSnapshot, thumbs: &[ThumbSlot], size: Size, m: &Metrics, scale: f64) -> Vec<(i32, Rect)> {
    if m.icon_size <= 0.0 {
        return Vec::new();
    }
    let mut pids: Vec<i32> = card.apps.clone();
    for w in &card.windows {
        if !pids.contains(&w.pid) {
            pids.push(w.pid);
        }
    }
    let left = thumbs.iter().map(|t| t.rect.x).fold(f64::INFINITY, f64::min);
    let corner = thumbs
        .iter()
        .find(|t| t.rect.x <= left + 1e-9)
        .map_or(Point::new(0.0, size.h), |t| Point::new(t.rect.x, t.rect.max_y()));
    let s = m.icon_size * scale;
    pids.iter()
        .take(m.icon_max)
        .enumerate()
        .map(|(i, &pid)| {
            let cx = corner.x + (m.icon_dx + i as f64 * m.icon_step) * scale;
            let cy = corner.y + m.icon_dy * scale;
            (pid, Rect::new(cx - s / 2.0, cy - s / 2.0, s, s))
        })
        .collect()
}

/// Lays out the column in a panel `height` tall: cards top to bottom, `gap` apart, centered
/// on the panel (the visible frame), scaled down uniformly when they would not fit.
pub fn layout_column(cards: &[CardSnapshot], height: f64, m: &Metrics) -> Vec<CardLayout> {
    if cards.is_empty() {
        return Vec::new();
    }
    let natural: f64 = cards.iter().map(|c| arrange_card(c, m, 1.0).0.h).sum();
    let gaps = m.gap * (cards.len() - 1) as f64;
    let margin = 8.0 * m.k;
    let room = (height - 2.0 * margin - gaps).max(1.0);
    let scale = if natural > room { (room / natural).max(0.2) } else { 1.0 };
    let arranged: Vec<(Size, Vec<ThumbSlot>)> = cards.iter().map(|c| arrange_card(c, m, scale)).collect();
    let total = arranged.iter().map(|(s, _)| s.h).sum::<f64>() + gaps;
    let mut y = ((height - total) / 2.0).max(margin);
    let mut out = Vec::with_capacity(cards.len());
    for (card, (size, thumbs)) in cards.iter().zip(arranged) {
        let icons = icons_for(card, &thumbs, size, m, scale);
        out.push(CardLayout { stage: card.stage, frame: Rect::new(m.card_x, y, size.w, size.h), thumbs, icons });
        y += size.h + m.gap;
    }
    out
}

/// Where a card dragged out holds its window of `full` size: the pointer `grab` (`[drag]
/// window_grab_x`, `window_grab_y`) right of its left edge and below its top, at most half its
/// width and height. Apple's holds the window there wherever the card was pressed (measured
/// 11-09: 96 and 100 pt, pressed at 47 % and at 71 % of a card's width).
pub fn window_grab(full: Size, grab: (f64, f64)) -> (f64, f64) {
    (grab.0.clamp(0.0, full.w.max(0.0) / 2.0), grab.1.clamp(0.0, full.h.max(0.0) / 2.0))
}

/// The ghost of a card dragged out of the strip, in global coordinates: inside the strip it
/// is the front thumbnail `thumb` (where it was pressed, grabbed at fractions `grab` of its
/// size); past `strip_right` it grows toward the window's `full` size, reached `grow` further
/// (`[drag] grow_distance`), the grab point sliding to where the window is held (`hold`,
/// [`window_grab`]). The pointer `p` stays at the grab point throughout.
pub fn ghost_frame(thumb: Rect, grab: (f64, f64), full: Size, strip_right: f64, grow: f64, hold: (f64, f64), p: Point) -> Rect {
    let t = ((p.x - strip_right) / grow.max(1.0)).clamp(0.0, 1.0);
    let lerp = |a: f64, b: f64| a + (b - a) * t;
    let w = lerp(thumb.w, full.w.max(thumb.w));
    let h = lerp(thumb.h, full.h.max(thumb.h));
    let ox = lerp(grab.0 * thumb.w, hold.0);
    let oy = lerp(grab.1 * thumb.h, hold.1);
    Rect::new(p.x - ox, p.y - oy, w, h)
}

/// Where a dropped card's window goes (its top-left corner, global): held at `hold`
/// ([`window_grab`]) under the pointer `p`, as the grown ghost was.
pub fn drop_origin(hold: (f64, f64), p: Point) -> Point {
    Point::new(p.x - hold.0, p.y - hold.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Auto, StripStyle};
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
        CardSnapshot { stage, number: stage as usize, name: None, windows, apps, ..Default::default() }
    }

    /// His laptop: visible 0,39 1800 × 1130 in a 1800 × 1169 display; the panel starts at
    /// y = 39, so the display's middle (584.5) is 545.5 in panel coordinates.
    fn laptop() -> Metrics {
        Metrics::new(&StripStyle::default(), Size::new(1800.0, 1130.0), 1.0, 584.5 - 39.0)
    }

    /// The 5K display: visible 0,30 2560 × 1410 in 2560 × 1440.
    fn five_k() -> Metrics {
        Metrics::new(&StripStyle::default(), Size::new(2560.0, 1410.0), 1.0, 720.0 - 30.0)
    }

    /// The bounding box Apple's window list reported for a card of a `w × h` window whose
    /// box top was at global `y`, from our model (global coordinates, panel at `top`).
    fn apple_box(m: &Metrics, panel_top: f64, w: f64, h: f64, y: f64) -> (f64, f64) {
        let s = m.thumb_size(&Rect::new(0.0, 0.0, w, h));
        let f = m.far_ratio(s.w);
        let eye = m.eye.y + panel_top;
        // Above the eye the box top is the left edge's top; below it the far edge's top.
        let top = if y < eye { y } else { eye + (y - eye) / f } - panel_top;
        let b = m.projected_bounds(m.card_x, top, s.w, s.h);
        (b.w, b.h)
    }

    /// The widest card: a screen-sized window of one app, two of a second app.
    fn widest(m: &Metrics) -> CardLayout {
        let (w, h) = (m.visible.w, m.visible.h);
        let c = card(1, &[(1, 10, w, h), (2, 20, w, h), (3, 20, w, h)]);
        layout_column(&[c], m.visible.h, m).remove(0)
    }

    /// `l`'s box grown by `s` about its hover center (as the strip draws a hovered card).
    fn grown(l: &CardLayout, m: &Metrics, s: f64) -> Rect {
        let (b, c) = (l.bounds(m), l.hover_center(m));
        Rect::new(c.x + (b.x - c.x) * s, c.y + (b.y - c.y) * s, b.w * s, b.h * s)
    }

    #[test]
    fn the_panel_has_room_for_the_widest_card_grown_and_dragged_to_the_columns_edge() {
        for m in [laptop(), five_k()] {
            let l = widest(&m);
            let rest = l.bounds(&m);
            assert!(rest.max_x() > m.width + 40.0, "the second app's window reaches past the column ({} > {})", rest.max_x(), m.width);
            let panel = m.panel_width();
            let hovered = grown(&l, &m, m.hover_scale);
            assert!(hovered.max_x() + m.shadow <= panel, "grown under the pointer: {} in {panel}", hovered.max_x());
            // Dragged in the column: pressed at its left edge, the pointer at the column's edge.
            let dragged = hovered.max_x() + (m.width - hovered.x);
            assert!(dragged + m.shadow <= panel, "following the pointer: {dragged} in {panel}");
            assert!(panel < m.width + 2.0 * rest.w, "and no more than that: {panel}");
        }
        // A config's wider second app gets more room; a screen narrower than it caps the panel.
        let mut style = StripStyle::default();
        style.cards.second_app_dx = 200.0;
        let wide = Metrics::new(&style, Size::new(1800.0, 1130.0), 1.0, 545.5);
        assert!(wide.panel_width() > laptop().panel_width() + 100.0);
        let tiny = Metrics::new(&StripStyle::default(), Size::new(400.0, 300.0), 1.0, 150.0);
        assert!(tiny.panel_width() <= 400.0);
    }

    #[test]
    fn the_hidden_slide_takes_the_widest_card_off_the_screen() {
        for (m, apple) in [(laptop(), true), (five_k(), false)] {
            let l = widest(&m);
            let d = m.hide_distance();
            for t in &l.thumbs {
                let x = l.frame.x + t.rect.x;
                // Slid in 3D before the perspective: the far edge moves `d` times its ratio.
                let far = m.eye.x + (x - d + t.rect.w * m.tilt.cos() - m.eye.x) * m.far_ratio(t.rect.w);
                assert!(far <= 1e-6, "far edge at {far} after a slide of {d}");
            }
            if apple {
                // Apple's measured 286 pt is just what the widest card needs on the laptop.
                assert!((d - 286.0).abs() < 1.0, "{d}");
            }
        }
    }

    #[test]
    fn a_press_finds_the_frontmost_window_whose_turned_plane_holds_it() {
        let m = laptop();
        let l = widest(&m);
        let (front, second) = (l.thumbs[0], l.thumbs[1]);
        let mid = |t: &ThumbSlot, fx: f64| {
            let q = m.project(l.frame.x + t.rect.x, l.frame.y + t.rect.y, t.rect.w, t.rect.h);
            Point::new(q[0].x + (q[1].x - q[0].x) * fx, (q[0].y + q[3].y) / 2.0)
        };
        assert_eq!(l.thumb_at(&m, mid(&front, 0.5)), Some(0), "the front window over the others");
        // Past the front window's far edge only the second app's window is drawn.
        let past = mid(&second, 0.97);
        assert!(past.x > m.project(l.frame.x + front.rect.x, l.frame.y + front.rect.y, front.rect.w, front.rect.h)[1].x);
        assert_eq!(l.thumb_at(&m, past), Some(1));
        assert_eq!(l.thumb_at(&m, Point::new(l.bounds(&m).max_x() + 5.0, past.y)), None);
        // Above the front window's top, where the raised second app's window peeks out.
        let top = m.project(l.frame.x + second.rect.x, l.frame.y + second.rect.y, second.rect.w, second.rect.h);
        let peek = Point::new(top[0].x + 30.0, l.frame.y + second.rect.y + 2.0);
        assert_eq!(l.thumb_at(&m, peek), Some(1), "{peek:?}");
    }

    #[test]
    fn a_card_draws_its_front_window_on_top_then_the_others_in_order() {
        // The front window (app 10), a second app's window (20), then another of app 10's.
        let m = laptop();
        let c = card(1, &[(1, 10, 1200.0, 900.0), (2, 20, 1200.0, 900.0), (3, 10, 1000.0, 800.0)]);
        let l = &layout_column(&[c], 1130.0, &m)[0];
        let drawn: Vec<u32> = l.draw_order().iter().map(|t| t.id).collect();
        assert_eq!(drawn, [3, 2, 1], "back to front: the list reversed, the front window last");
        // Why the order has to be given: the turn alone puts the second app's plane nearer
        // the eye wherever it overlaps the front one (its left edge is further right).
        let (front, second) = (l.thumbs[0], l.thumbs[1]);
        assert!(second.rect.x > front.rect.x);
        let x = l.frame.x;
        let px = x + second.rect.x + 1.0;
        assert!(px < x + front.rect.x + front.rect.w, "the two overlap there");
        assert!(m.depth_at(x + second.rect.x, px) > m.depth_at(x + front.rect.x, px));
    }

    #[test]
    fn cards_match_apples_measured_boxes_on_the_laptop() {
        // (window w, h, box top y, Apple's box w, h), from the window list at ~60 Hz.
        let seen = [
            (1280.0, 940.0, 685.0, 120.0, 144.0),
            (1753.0, 1130.0, 505.0, 159.0, 161.0),
            (1800.0, 1130.0, 826.0, 162.0, 193.0),
            (1040.0, 1130.0, 157.0, 99.0, 180.0),
            (1040.0, 1130.0, 272.0, 99.0, 172.0),
            (1621.0, 1130.0, 347.0, 148.0, 169.0),
            (1157.0, 1023.0, 355.0, 109.0, 152.0),
            (1157.0, 1023.0, 686.0, 109.0, 155.0),
            (1196.0, 758.0, 557.0, 113.0, 109.0),
            (920.0, 436.0, 379.0, 133.0, 107.0),
        ];
        let m = laptop();
        for (w, h, y, aw, ah) in seen {
            let (bw, bh) = apple_box(&m, 39.0, w, h, y);
            assert!((bw - aw).abs() <= 1.5 && (bh - ah).abs() <= 1.5, "{w}x{h} at {y}: ours {bw:.1}x{bh:.1}, Apple's {aw}x{ah}");
        }
    }

    #[test]
    fn cards_match_apples_measured_boxes_on_the_5k_display() {
        let seen = [
            (1357.0, 955.0, 461.0, 99.0, 116.0),
            (1357.0, 955.0, 597.0, 99.0, 110.0),
            (1621.0, 1183.0, 725.0, 117.0, 135.0),
            (1621.0, 1183.0, 266.0, 117.0, 153.0),
            (1157.0, 1023.0, 882.0, 85.0, 124.0),
            (1157.0, 1023.0, 710.0, 85.0, 117.0),
            (2103.0, 1293.0, 420.0, 149.0, 159.0),
            (1102.0, 643.0, 599.0, 106.0, 98.0),
        ];
        let m = five_k();
        for (w, h, y, aw, ah) in seen {
            let (bw, bh) = apple_box(&m, 30.0, w, h, y);
            assert!((bw - aw).abs() <= 1.5 && (bh - ah).abs() <= 1.5, "{w}x{h} at {y}: ours {bw:.1}x{bh:.1}, Apple's {aw}x{ah}");
        }
    }

    #[test]
    fn merge_zones_and_gaps_are_both_wide_on_his_displays() {
        use crate::ui::motion_math::{DropSpot, drop_spot};
        let band = StripStyle::default().drag.merge_zone;
        // His usual cards: a full-height browser, a terminal, a small Finder window (the
        // smallest card Apple draws), and a tall narrow window.
        for (name, m, (w, h)) in [("laptop", laptop(), (1800.0, 1130.0)), ("5K", five_k(), (2560.0, 1410.0))] {
            let cards = [
                card(1, &[(1, 10, w, h)]),
                card(2, &[(2, 20, 1280.0, 940.0)]),
                card(3, &[(3, 30, 920.0, 436.0)]),
                card(4, &[(4, 40, 1040.0, h)]),
            ];
            let bands: Vec<(f64, f64)> = layout_column(&cards, h, &m)
                .iter()
                .map(|l| {
                    let b = l.bounds(&m);
                    (b.y, b.max_y())
                })
                .collect();
            // Walk the column a point at a time: every run of one answer is long enough to
            // land in on purpose.
            let (top, bottom) = (bands[0].0 - 40.0, bands[3].1 + 40.0);
            let mut runs: Vec<(DropSpot, f64)> = Vec::new();
            let mut y = top;
            while y <= bottom {
                let spot = drop_spot(&bands, y, band);
                match runs.last_mut() {
                    Some((s, len)) if *s == spot => *len += 1.0,
                    _ => runs.push((spot, 1.0)),
                }
                y += 1.0;
            }
            let kinds: Vec<DropSpot> = runs.iter().map(|r| r.0).collect();
            assert_eq!(
                kinds,
                [
                    DropSpot::Gap(0), DropSpot::OnCard(0), DropSpot::Gap(1), DropSpot::OnCard(1),
                    DropSpot::Gap(2), DropSpot::OnCard(2), DropSpot::Gap(3), DropSpot::OnCard(3),
                    DropSpot::Gap(4),
                ],
                "{name}: gaps and cards alternate"
            );
            for (spot, len) in &runs[1..runs.len() - 1] {
                let least = if matches!(spot, DropSpot::OnCard(_)) { 45.0 } else { 70.0 };
                assert!(*len >= least, "{name}: {spot:?} only {len} pt tall");
            }
        }
    }

    #[test]
    fn small_windows_are_drawn_at_least_min_height_and_wide_ones_capped() {
        let m = laptop();
        // Apple drew a 920 × 436 Finder window 97 pt tall on the laptop (and a 1102 × 643 one
        // as tall on the 5K display).
        assert!((m.thumb_size(&Rect::new(0.0, 0.0, 920.0, 436.0)).h - 97.0).abs() < 1e-9);
        assert!((five_k().thumb_size(&Rect::new(0.0, 0.0, 1102.0, 643.0)).h - 97.0).abs() < 1e-9);
        let full = m.thumb_size(&Rect::new(0.0, 0.0, 1800.0, 1130.0));
        assert!((full.h - 161.0).abs() < 1e-9);
        let banner = m.thumb_size(&Rect::new(0.0, 0.0, 3000.0, 200.0));
        assert!(banner.w <= m.max_width + 1e-9 && (banner.w / banner.h - 15.0).abs() < 1e-9);
    }

    #[test]
    fn the_perspective_is_one_eye_at_the_displays_middle() {
        let m = laptop();
        // A card above the middle: its top edge slopes down to the right; below: its bottom
        // edge slopes up; the far edge is shorter by the same ratio everywhere.
        let above = m.project(15.0, 100.0, 200.0, 140.0);
        let below = m.project(15.0, 800.0, 200.0, 140.0);
        assert!(above[1].y > above[0].y, "top edge slopes down");
        assert!(below[2].y < below[3].y, "bottom edge slopes up");
        let ratio = |q: &[Point; 4]| (q[2].y - q[1].y) / (q[3].y - q[0].y);
        assert!((ratio(&above) - ratio(&below)).abs() < 1e-9);
        assert!((0.85..0.95).contains(&ratio(&above)), "{}", ratio(&above));
        // The measured edges meet about 1390 pt right of the cards on the laptop.
        let (t, _) = auto_perspective(1800.0);
        assert!((t - 44.2).abs() < 1e-9);
        let v = m.perspective / m.tilt.tan();
        assert!((1350.0..1430.0).contains(&v), "{v}");
        assert_eq!(auto_perspective(5000.0), (47.9, 2260.0));
        assert_eq!(auto_perspective(1000.0), (44.2, 1346.0));
    }

    /// 11-01: a snapshot a hair from flat (the spring not quite at rest at the unveil) was
    /// measured with the cards' turn, 1217 pt wide reading 355. Its own turn: as wide as flat.
    #[test]
    fn a_plane_is_measured_with_its_own_turn() {
        let m = five_k();
        let b = m.projected_bounds_turned(813.0, 215.0, 1217.0, 790.0, 1e-6);
        assert!((b.x - 813.0).abs() < 0.1 && (b.w - 1217.0).abs() < 0.1 && (b.y - 215.0).abs() < 0.1 && (b.h - 790.0).abs() < 0.1, "{b:?}");
        let card = m.projected_bounds(15.0, 100.0, 200.0, 140.0);
        assert_eq!(m.projected_bounds_turned(15.0, 100.0, 200.0, 140.0, m.tilt), card, "the cards' turn as before");
        assert!(card.w < 200.0 * 0.8, "a card is narrower than flat: {card:?}");
    }

    #[test]
    fn config_overrides_the_turn() {
        let mut style = StripStyle::default();
        style.cards.tilt = Auto::Value(0.0);
        let m = Metrics::new(&style, Size::new(1800.0, 1130.0), 1.0, 545.5);
        let b = m.projected_bounds(15.0, 100.0, 200.0, 100.0);
        assert!((b.w - 200.0).abs() < 1e-9 && (b.h - 100.0).abs() < 1e-9, "flat cards");
    }

    #[test]
    fn same_app_windows_step_right_toward_the_front_and_a_second_app_sits_right_and_up() {
        let m = laptop();
        let (_, t) = arrange_card(&card(1, &[(1, 10, 900.0, 700.0), (2, 10, 900.0, 700.0), (3, 10, 900.0, 700.0)]), &m, 1.0);
        // Front first in `t`: the front window is rightmost, the deepest at the card's left.
        assert_eq!(t[0].rect.x - t[1].rect.x, 17.0);
        assert_eq!(t[1].rect.x - t[2].rect.x, 17.0);
        assert_eq!(t[2].rect.x, 0.0);
        let mid = |r: &Rect| r.y + r.h / 2.0;
        assert!((mid(&t[0].rect) - mid(&t[2].rect)).abs() < 1e-9);
        let c = card(1, &[(1, 10, 900.0, 700.0), (2, 20, 900.0, 700.0)]);
        let (_, t) = arrange_card(&c, &m, 1.0);
        assert_eq!(t[1].rect.x - t[0].rect.x, 68.0);
        assert!((mid(&t[0].rect) - mid(&t[1].rect) - 14.0).abs() < 1e-9);
    }

    #[test]
    fn the_icon_sits_on_the_front_windows_bottom_left_corner() {
        let m = laptop();
        let c = card(1, &[(1, 10, 1280.0, 940.0), (2, 20, 900.0, 700.0)]);
        let l = layout_column(&[c], 1130.0, &m);
        let front = l[0].thumbs[0].rect;
        let icons = &l[0].icons;
        assert_eq!(icons.len(), 2);
        let center = |r: &Rect| Point::new(r.x + r.w / 2.0, r.y + r.h / 2.0);
        assert!((center(&icons[0].1).x - (front.x + 10.5)).abs() < 1e-9);
        assert!((center(&icons[0].1).y - (front.max_y() - 10.5)).abs() < 1e-9);
        assert_eq!(icons[1].1.x - icons[0].1.x, 36.0);
        assert_eq!(icons[0].1.w, 38.0);
    }

    #[test]
    fn the_column_is_centered_on_the_visible_frame_with_apples_gap() {
        let m = laptop();
        let cards: Vec<_> = (1..=5).map(|i| card(i, &[(i as u32, 1, 1280.0, 940.0)])).collect();
        let l = layout_column(&cards, 1130.0, &m);
        assert_eq!(l.len(), 5);
        assert!((l[1].frame.y - l[0].frame.max_y() - 29.5).abs() < 1e-9);
        let mid = (l[0].frame.y + l[4].frame.max_y()) / 2.0;
        assert!((mid - 565.0).abs() < 1e-6);
        assert!(l.iter().all(|c| c.frame.x == 15.0));
        // Too many: scaled down to fit.
        let many: Vec<_> = (1..=12).map(|i| card(i, &[(i as u32, 1, 600.0, 900.0)])).collect();
        let l = layout_column(&many, 900.0, &m);
        assert!(l[0].frame.y >= 8.0 - 1e-9 && l.last().map_or(0.0, |c| c.frame.max_y()) <= 900.0 - 8.0 + 1e-6);
    }

    #[test]
    fn the_scale_option_scales_every_length() {
        let m = Metrics::new(&StripStyle::default(), Size::new(1800.0, 1130.0), 0.5, 545.5);
        let full = m.thumb_size(&Rect::new(0.0, 0.0, 1800.0, 1130.0));
        assert!((full.h - 80.5).abs() < 1e-9);
        assert_eq!((m.card_x, m.gap, m.icon_size, m.width), (7.5, 14.75, 19.0, 100.0));
        let ratio_small = m.far_ratio(full.w);
        let big = laptop();
        let ratio_big = big.far_ratio(big.thumb_size(&Rect::new(0.0, 0.0, 1800.0, 1130.0)).w);
        assert!((ratio_small - ratio_big).abs() < 1e-9, "same shape");
    }

    #[test]
    fn the_ghost_grows_from_the_thumbnail_to_the_window_past_the_strip() {
        let thumb = Rect::new(20.0, 500.0, 150.0, 100.0);
        let full = Size::new(900.0, 600.0);
        let grab = (0.5, 0.5);
        let style = StripStyle::default();
        let (grow, d) = (style.drag.grow_distance, &style.drag);
        let hold = window_grab(full, (d.window_grab_x, d.window_grab_y));
        assert_eq!(hold, (96.0, 100.0), "Apple's, measured 11-09");
        let inside = ghost_frame(thumb, grab, full, 200.0, grow, hold, Point::new(95.0, 550.0));
        assert_eq!(inside, thumb, "unchanged inside the strip");
        let far = ghost_frame(thumb, grab, full, 200.0, grow, hold, Point::new(200.0 + grow + 50.0, 550.0));
        assert_eq!((far.w, far.h), (900.0, 600.0));
        assert_eq!((far.x, far.y), (200.0 + grow + 50.0 - 96.0, 450.0), "the pointer 96 pt in from the left edge, 100 below the top");
        let mid = ghost_frame(thumb, grab, full, 200.0, grow, hold, Point::new(200.0 + grow / 2.0, 550.0));
        assert!(mid.w > thumb.w && mid.w < full.w);
        let at = drop_origin(hold, Point::new(200.0 + grow + 50.0, 550.0));
        assert_eq!((at.x, at.y), (far.x, far.y), "the drop lands where the grown ghost was");
        // Wherever the card was pressed (Apple's 03 at 47 % of the card, 06 at 71 %), the same.
        let other = ghost_frame(thumb, (0.71, 0.67), full, 200.0, grow, hold, Point::new(200.0 + grow + 50.0, 550.0));
        assert_eq!((other.x, other.y), (far.x, far.y));
        // A small window is held at most at its middle.
        assert_eq!(window_grab(Size::new(150.0, 120.0), (96.0, 100.0)), (75.0, 60.0));
    }
}
