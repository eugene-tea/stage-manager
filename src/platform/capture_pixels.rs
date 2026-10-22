//! What a window's picture holds outside the window's own pixels, read as numbers only (round
//! 8M): the mean colour, alpha and flatness of a few boxes of the image, never its content.
//!
//! The owner's Calendar card on 10-21: the window and its event popup (a child window of the
//! app sitting over the window and 111 pt past its top) drawn inside a flat gray fill larger
//! than both. ScreenCaptureKit's `contentRect` said the window's own frame, so the picture was
//! taken as the window, yet its image held the union of the two, fitted (letterboxed) into the
//! window's proportions. Where the image had no window, the card's plane, gray, showed.
//!
//! - **Diagnostic**: for a capture whose window has another window of its app over it
//!   ([`Stats`]), the four corner boxes and the overlap region are measured and logged at
//!   debug, once per change ([`super::capture`]).
//! - **Guard** ([`holds_fill`]): while a window of its app overlaps it, a picture holds a fill,
//!   not the window, when its four corner boxes are all opaque, flat and of one colour
//!   ([`flat_fill`]: a window's own corners are rounded, so partly see-through), or, taken as
//!   the window alone, a corner box is mostly see-through (a letterbox band: a window's own
//!   rounded corner clears only a few pixels of its box, which stays nearly opaque). Such a
//!   picture is not used; the card keeps its last good picture.
//!
//! Pure arithmetic over a byte buffer, unit tested; [`read`] takes the buffer from a CGImage.

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGBitmapInfo, CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo};

/// A box's mean alpha at or above this is opaque.
pub const OPAQUE: u8 = 240;
/// A box whose brightness varies by at most this (standard deviation, 0-255) is flat.
pub const FLAT: u8 = 6;
/// Boxes whose mean brightness differs by at most this are one colour.
pub const SAME: u8 = 12;
/// A corner box whose mean alpha is below this is mostly see-through.
pub const SEE_THROUGH: u8 = 64;

/// A 32-bit image's bytes and layout.
#[derive(Debug, Clone)]
pub struct Pixels {
    /// The bytes, row after row.
    pub bytes: Vec<u8>,
    /// Bytes per row.
    pub row: usize,
    /// Width in pixels.
    pub w: usize,
    /// Height in pixels.
    pub h: usize,
    /// Which byte of a pixel is alpha (0 or 3).
    pub alpha: usize,
}

/// A rectangle in pixels (top-left origin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    /// Left.
    pub x: usize,
    /// Top.
    pub y: usize,
    /// Width.
    pub w: usize,
    /// Height.
    pub h: usize,
}

/// One box's numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    /// Mean alpha, 0-255.
    pub alpha: u8,
    /// Mean brightness (the colour bytes' mean), 0-255.
    pub mean: u8,
    /// Standard deviation of the brightness, 0-255: 0 for a flat fill.
    pub spread: u8,
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a{} m{} s{}", self.alpha, self.mean, self.spread)
    }
}

/// The four corner boxes of a `w` x `h` image (top-left, top-right, bottom-left, bottom-right),
/// each a twentieth of the image's side (at least 2 px), 1 px in from the edges.
pub fn corner_boxes(w: usize, h: usize) -> [PixelRect; 4] {
    let bw = (w / 20).max(2).min(w.saturating_sub(1).max(1));
    let bh = (h / 20).max(2).min(h.saturating_sub(1).max(1));
    let (l, t) = (1.min(w.saturating_sub(bw)), 1.min(h.saturating_sub(bh)));
    let r = w.saturating_sub(bw + 1);
    let b = h.saturating_sub(bh + 1);
    [
        PixelRect { x: l, y: t, w: bw, h: bh },
        PixelRect { x: r, y: t, w: bw, h: bh },
        PixelRect { x: l, y: b, w: bw, h: bh },
        PixelRect { x: r, y: b, w: bw, h: bh },
    ]
}

/// The numbers of box `r` of `px` (clipped to the image); `None` when nothing of it is in.
pub fn stats(px: &Pixels, r: PixelRect) -> Option<Stats> {
    let x1 = (r.x + r.w).min(px.w);
    let y1 = (r.y + r.h).min(px.h);
    if r.x >= x1 || r.y >= y1 {
        return None;
    }
    let (mut n, mut a_sum, mut m_sum, mut m_sq) = (0u64, 0u64, 0u64, 0u64);
    for y in r.y..y1 {
        for x in r.x..x1 {
            let at = y * px.row + x * 4;
            let p = px.bytes.get(at..at + 4)?;
            let colour: u64 = (0..4).filter(|i| *i != px.alpha).map(|i| u64::from(p[i])).sum::<u64>() / 3;
            n += 1;
            a_sum += u64::from(p[px.alpha]);
            m_sum += colour;
            m_sq += colour * colour;
        }
    }
    let mean = m_sum as f64 / n as f64;
    let var = (m_sq as f64 / n as f64 - mean * mean).max(0.0);
    Some(Stats { alpha: (a_sum / n) as u8, mean: mean.round() as u8, spread: var.sqrt().round().min(255.0) as u8 })
}

/// True when the four corner boxes are all opaque, flat and of one colour: the picture holds a
/// fill around what it shows (a window's own corners are rounded, so see-through).
pub fn flat_fill(corners: &[Stats; 4]) -> bool {
    let lo = corners.iter().map(|s| s.mean).min().unwrap_or(0);
    let hi = corners.iter().map(|s| s.mean).max().unwrap_or(0);
    corners.iter().all(|s| s.alpha >= OPAQUE && s.spread <= FLAT) && hi - lo <= SAME
}

/// True when a picture holds a fill around what it shows ([`flat_fill`]), or, taken as the
/// window alone (`window_alone`: no extent), a corner box is mostly see-through: the window's
/// own pixels would cover nearly all of it (module docs).
pub fn holds_fill(corners: &[Stats; 4], window_alone: bool) -> bool {
    flat_fill(corners) || (window_alone && corners.iter().any(|s| s.alpha < SEE_THROUGH))
}

/// The bytes of a 32-bit `image`; `None` for another pixel format or no data.
pub fn read(image: &CGImage) -> Option<Pixels> {
    let w = CGImage::width(Some(image));
    let h = CGImage::height(Some(image));
    if CGImage::bits_per_pixel(Some(image)) != 32 || w == 0 || h == 0 {
        return None;
    }
    let row = CGImage::bytes_per_row(Some(image));
    let alpha = alpha_byte(CGImage::alpha_info(Some(image)), CGImage::bitmap_info(Some(image)));
    let provider: Option<CFRetained<CGDataProvider>> = CGImage::data_provider(Some(image));
    let bytes = CGDataProvider::data(provider.as_deref())?.to_vec();
    Some(Pixels { bytes, row, w, h, alpha })
}

/// Which byte of a 32-bit pixel holds alpha.
pub fn alpha_byte(info: CGImageAlphaInfo, bitmap: CGBitmapInfo) -> usize {
    let first = matches!(info, CGImageAlphaInfo::First | CGImageAlphaInfo::PremultipliedFirst | CGImageAlphaInfo::NoneSkipFirst);
    let little = (bitmap.0 & CGBitmapInfo::ByteOrderInfoMask.0) == CGImageByteOrderInfo::Order32Little.0;
    match (first, little) {
        (true, false) | (false, true) => 0,
        (true, true) | (false, false) => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A BGRA (alpha last) image filled by `f(x, y) -> [b, g, r, a]`.
    fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 4]) -> Pixels {
        let mut bytes = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                bytes.extend_from_slice(&f(x, y));
            }
        }
        Pixels { bytes, row: w * 4, w, h, alpha: 3 }
    }

    fn corners(px: &Pixels) -> [Stats; 4] {
        corner_boxes(px.w, px.h).map(|r| stats(px, r).unwrap_or_default())
    }

    /// The 10-21 card: the union of Calendar's window and its popup letterboxed into the
    /// window's proportions on an opaque gray: all four corners opaque, flat, one colour.
    #[test]
    fn a_letterboxed_union_on_a_gray_fill_is_a_flat_fill() {
        let (w, h) = (347, 276);
        let px = image(w, h, |x, y| {
            let inside = (20..327).contains(&x) && (24..276).contains(&y);
            if inside { [((x * 7 + y * 3) % 255) as u8, 90, 160, 255] } else { [61, 56, 56, 255] }
        });
        let c = corners(&px);
        assert!(c[0].alpha == 255 && c[0].spread == 0, "{}", c[0]);
        assert!(flat_fill(&c), "{c:?}");
    }

    /// A window's own picture: its rounded corners are see-through.
    #[test]
    fn a_window_s_own_picture_is_not_a_flat_fill() {
        let (w, h) = (347, 276);
        let px = image(w, h, |x, y| {
            let corner = (x < 12 || x >= w - 12) && (y < 12 || y >= h - 12);
            if corner { [0, 0, 0, 0] } else { [40, 40, 40, 255] }
        });
        assert!(!flat_fill(&corners(&px)));
        // 8K: the union with a clear background (a popup sticking out): corners clear.
        let px = image(w, h, |x, _| if x < 250 { [40, 40, 40, 255] } else { [0, 0, 0, 0] });
        assert!(!flat_fill(&corners(&px)));
    }

    /// The same union letterboxed on a clear background: the card's gray plane showed through.
    /// Taken as the window alone, its see-through top band is a fill; a window's own rounded
    /// corners (a 6 px radius at thumbnail size, 52 px at a full-size snapshot) are not.
    #[test]
    fn a_see_through_band_in_a_picture_taken_as_the_window_is_a_fill() {
        let (w, h) = (347, 276);
        let band = image(w, h, |_, y| if y < 26 { [0, 0, 0, 0] } else { [50, 50, 50, 255] });
        assert!(holds_fill(&corners(&band), true));
        assert!(!holds_fill(&corners(&band), false), "a picture with an extent is clear past the window by design");
        for (w, h, r) in [(347usize, 276usize, 6usize), (2866, 2278, 52)] {
            let rounded = image(w, h, |x, y| {
                let dx = x.min(w - 1 - x);
                let dy = y.min(h - 1 - y);
                let out = dx < r && dy < r && (r - dx) * (r - dx) + (r - dy) * (r - dy) > r * r;
                if out { [0, 0, 0, 0] } else { [((x + y) % 200) as u8, 60, 60, 255] }
            });
            let c = corners(&rounded);
            assert!(!holds_fill(&c, true), "{w}x{h} r{r}: {c:?}");
        }
    }

    #[test]
    fn a_textured_or_two_coloured_border_is_not_one_fill() {
        let (w, h) = (200, 100);
        let busy = image(w, h, |x, y| [((x * 37 + y * 11) % 255) as u8, 0, 0, 255]);
        assert!(!flat_fill(&corners(&busy)), "not flat");
        let two = image(w, h, |_, y| if y < h / 2 { [30, 30, 30, 255] } else { [200, 200, 200, 255] });
        assert!(!flat_fill(&corners(&two)), "two colours");
    }

    #[test]
    fn boxes_clip_to_the_image_and_tiny_images_work() {
        let px = image(3, 3, |_, _| [10, 20, 30, 255]);
        let s = stats(&px, PixelRect { x: 2, y: 2, w: 10, h: 10 }).unwrap_or_default();
        assert_eq!(s, Stats { alpha: 255, mean: 20, spread: 0 });
        assert_eq!(stats(&px, PixelRect { x: 5, y: 0, w: 1, h: 1 }), None);
        for b in corner_boxes(3, 3) {
            assert!(b.x + b.w <= 3 && b.y + b.h <= 3, "{b:?}");
        }
    }
}
