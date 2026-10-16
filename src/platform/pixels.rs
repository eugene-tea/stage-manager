//! Small CoreGraphics pixel helpers for the desktop's still (round 8L4,
//! [`super::desktop_shot`]) and the overlay's read-back ([`super::self_capture`]): whether a
//! picture is opaque, one picture laid over another, and a picture's mean colour. Every one
//! draws into a bitmap of its own (8-bit BGRA, premultiplied), so the source's pixel format
//! does not matter.

use std::ffi::c_void;

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGInterpolationQuality,
};

/// The side (pixels) a picture is sampled down to for [`min_alpha`] and [`mean_rgba`].
const SAMPLE: usize = 64;

/// Draws `image` into an owned `w` x `h` bitmap (in `space`, or device RGB) and hands back its
/// bytes (B, G, R, A per pixel, premultiplied); `None` when no bitmap can be made.
fn draw_into(images: &[&CGImage], w: usize, h: usize, space: Option<&CGColorSpace>) -> Option<Vec<u8>> {
    if w == 0 || h == 0 {
        return None;
    }
    let fallback = CGColorSpace::new_device_rgb();
    let space = space.or(fallback.as_deref())?;
    let mut bytes = vec![0u8; w * h * 4];
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    {
        // SAFETY: `bytes` holds w*h*4 bytes, rows of w*4; it outlives the context (dropped at
        // the end of this block, before the bytes are read).
        let ctx = unsafe { CGBitmapContextCreate(bytes.as_mut_ptr().cast::<c_void>(), w, h, 8, w * 4, Some(space), info) }?;
        CGContext::set_interpolation_quality(Some(&ctx), CGInterpolationQuality::Medium);
        let full = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64));
        for image in images {
            CGContext::draw_image(Some(&ctx), full, Some(image));
        }
    }
    Some(bytes)
}

/// The size `image` is sampled at: at most [`SAMPLE`] a side.
fn sample_size(image: &CGImage) -> (usize, usize) {
    (CGImage::width(Some(image)).clamp(1, SAMPLE), CGImage::height(Some(image)).clamp(1, SAMPLE))
}

/// The least alpha (0-255) of `bgra` pixels; 0 when empty.
fn least_alpha(bgra: &[u8]) -> u8 {
    bgra.as_chunks::<4>().0.iter().map(|p| p[3]).min().unwrap_or(0)
}

/// The mean red, green, blue and alpha (0-255, premultiplied) of `bgra` pixels.
fn mean_of(bgra: &[u8]) -> [f64; 4] {
    let n = (bgra.len() / 4).max(1) as f64;
    let mut sum = [0.0; 4];
    for p in bgra.as_chunks::<4>().0 {
        sum[0] += f64::from(p[2]);
        sum[1] += f64::from(p[1]);
        sum[2] += f64::from(p[0]);
        sum[3] += f64::from(p[3]);
    }
    sum.map(|s| s / n)
}

/// The least alpha of `image` sampled at most [`SAMPLE`] pixels a side: 255 for a picture
/// with no see-through part.
pub fn min_alpha(image: &CGImage) -> Option<u8> {
    let (w, h) = sample_size(image);
    draw_into(&[image], w, h, None).map(|b| least_alpha(&b))
}

/// The mean red, green, blue and alpha (0-255) of `image`, sampled at most [`SAMPLE`] pixels
/// a side, in device RGB.
pub fn mean_rgba(image: &CGImage) -> Option<[f64; 4]> {
    let (w, h) = sample_size(image);
    draw_into(&[image], w, h, None).map(|b| mean_of(&b))
}

/// `top` laid over `base`, both stretched to `base`'s pixel size, in `base`'s colour space.
pub fn over(base: &CGImage, top: &CGImage) -> Option<CFRetained<CGImage>> {
    let (w, h) = (CGImage::width(Some(base)), CGImage::height(Some(base)));
    if w == 0 || h == 0 || w > 16_384 || h > 16_384 {
        return None;
    }
    let space = CGImage::color_space(Some(base));
    let fallback = CGColorSpace::new_device_rgb();
    let space = space.as_deref().or(fallback.as_deref())?;
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    // SAFETY: CG allocates the bitmap (null data, bytes per row computed); 8-bit BGRA.
    let ctx = unsafe { CGBitmapContextCreate(std::ptr::null_mut::<c_void>(), w, h, 8, 0, Some(space), info) }?;
    CGContext::set_interpolation_quality(Some(&ctx), CGInterpolationQuality::High);
    let full = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64));
    CGContext::draw_image(Some(&ctx), full, Some(base));
    CGContext::draw_image(Some(&ctx), full, Some(top));
    CGBitmapContextCreateImage(Some(&ctx))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A `w` x `h` picture: `rgba` (0-1) everywhere, and fully clear in its left half when
    /// `clear_left`.
    pub(crate) fn picture(w: usize, h: usize, rgba: [f64; 4], clear_left: bool) -> CFRetained<CGImage> {
        let space = CGColorSpace::new_device_rgb().expect("device RGB");
        let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
        // SAFETY: CG allocates the bitmap.
        let ctx = unsafe { CGBitmapContextCreate(std::ptr::null_mut::<c_void>(), w, h, 8, 0, Some(&space), info) }.expect("bitmap");
        CGContext::set_rgb_fill_color(Some(&ctx), rgba[0], rgba[1], rgba[2], rgba[3]);
        CGContext::fill_rect(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64)));
        if clear_left {
            CGContext::clear_rect(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64 / 2.0, h as f64)));
        }
        CGBitmapContextCreateImage(Some(&ctx)).expect("image")
    }

    #[test]
    fn the_pure_sums_read_bgra() {
        let px = [10, 20, 30, 255, 30, 40, 50, 128];
        assert_eq!(least_alpha(&px), 128);
        assert_eq!(mean_of(&px), [40.0, 30.0, 20.0, 191.5]);
        assert_eq!(least_alpha(&[]), 0);
    }

    #[test]
    fn an_opaque_picture_and_one_with_a_clear_part() {
        assert_eq!(min_alpha(&picture(40, 30, [0.2, 0.4, 0.6, 1.0], false)), Some(255));
        assert_eq!(min_alpha(&picture(200, 100, [0.2, 0.4, 0.6, 1.0], true)), Some(0));
        let m = mean_rgba(&picture(8, 8, [1.0, 0.0, 0.0, 1.0], false)).expect("a mean");
        assert!(m[0] > 250.0 && m[1] < 5.0 && m[2] < 5.0 && m[3] > 254.0, "{m:?}");
    }

    #[test]
    fn a_picture_with_a_clear_part_laid_over_another_shows_it_there() {
        let base = picture(20, 10, [0.0, 0.0, 1.0, 1.0], false);
        let top = picture(20, 10, [1.0, 0.0, 0.0, 1.0], true);
        let both = over(&base, &top).expect("laid over");
        assert_eq!(min_alpha(&both), Some(255), "opaque where the top is clear");
        let m = mean_rgba(&both).expect("a mean");
        // Half red, half blue.
        assert!((m[0] - 127.5).abs() < 20.0 && (m[2] - 127.5).abs() < 20.0 && m[1] < 5.0, "{m:?}");
    }
}
