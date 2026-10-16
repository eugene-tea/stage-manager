//! Test only (`ctl debug-overlay`): what our own motion overlay draws, read back as pixels.
//!
//! A scripted test asks where a ghost really is on screen (not where our code thinks it put
//! it): the overlay window is captured through ScreenCaptureKit at its native size and the
//! bounds of its opaque pixels (alpha above half, so the soft shadows are left out) come back
//! in global top-left points. Only our own overlay window is ever captured, and no image is
//! kept. Also reads a held picture's corner pixels for `ctl debug-strip` ([`corners`]).

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGBitmapInfo, CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo};
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{SCScreenshotManager, SCShareableContent};

use super::capture::{WindowShot, display_frames, window_shot};
use super::capture_frame::Extent;

use crate::engine::geometry::{Point, Rect};

/// The bounds of what window `window` (our overlay, top-left corner at `origin`, global)
/// draws, within `region` (global) when given; `None` when nothing opaque is there.
pub fn opaque_bounds(window: u32, origin: Point, region: Option<Rect>, timeout: Duration) -> Result<Option<Rect>, String> {
    let (image, scale, extent) = capture(window, timeout)?;
    let w = CGImage::width(Some(&image));
    let h = CGImage::height(Some(&image));
    // The image's top-left corner, global: the window's, less what reaches past it.
    let window_pts = extent.window_in(Rect::new(0.0, 0.0, w as f64 / scale, h as f64 / scale));
    let origin = Point::new(origin.x - window_pts.x, origin.y - window_pts.y);
    let bpp = CGImage::bits_per_pixel(Some(&image));
    let row = CGImage::bytes_per_row(Some(&image));
    if bpp != 32 {
        return Err(format!("unexpected pixel format ({bpp} bits per pixel)"));
    }
    let alpha = alpha_byte(CGImage::alpha_info(Some(&image)), CGImage::bitmap_info(Some(&image)));
    let provider: Option<CFRetained<CGDataProvider>> = CGImage::data_provider(Some(&image));
    let data = CGDataProvider::data(provider.as_deref()).ok_or("no pixel data")?;
    let bytes = data.to_vec();
    // The region in image pixels.
    let (x0, y0, x1, y1) = match region {
        Some(r) => (
            ((r.x - origin.x) * scale).floor().max(0.0) as usize,
            ((r.y - origin.y) * scale).floor().max(0.0) as usize,
            (((r.max_x() - origin.x) * scale).ceil().max(0.0) as usize).min(w),
            (((r.max_y() - origin.y) * scale).ceil().max(0.0) as usize).min(h),
        ),
        None => (0, 0, w, h),
    };
    let mut found: Option<(usize, usize, usize, usize)> = None;
    for y in y0..y1 {
        for x in x0..x1 {
            if bytes.get(y * row + x * 4 + alpha).copied().unwrap_or(0) > 128 {
                found = Some(match found {
                    None => (x, y, x, y),
                    Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x), d.max(y)),
                });
            }
        }
    }
    Ok(found.map(|(a, b, c, d)| {
        Rect::new(origin.x + a as f64 / scale, origin.y + b as f64 / scale, (c + 1 - a) as f64 / scale, (d + 1 - b) as f64 / scale)
    }))
}

/// The corner pixels of `image` (2 px in from each corner: top-left, top-right, bottom-left,
/// bottom-right), each `[alpha, mean of the color bytes]`, for a test asking whether the area
/// a capture holds outside any window is clear; `None` for a pixel format it does not read.
pub fn corners(image: &CGImage) -> Option<[[u8; 2]; 4]> {
    let w = CGImage::width(Some(image));
    let h = CGImage::height(Some(image));
    if CGImage::bits_per_pixel(Some(image)) != 32 || w == 0 || h == 0 {
        return None;
    }
    let row = CGImage::bytes_per_row(Some(image));
    let alpha = alpha_byte(CGImage::alpha_info(Some(image)), CGImage::bitmap_info(Some(image)));
    let provider: Option<CFRetained<CGDataProvider>> = CGImage::data_provider(Some(image));
    let bytes = CGDataProvider::data(provider.as_deref())?.to_vec();
    let (l, r) = (2.min(w - 1), w.saturating_sub(3));
    let (t, b) = (2.min(h - 1), h.saturating_sub(3));
    let pixel = |x: usize, y: usize| -> Option<[u8; 2]> {
        let at = y * row + x * 4;
        let px = bytes.get(at..at + 4)?;
        let color: u32 = (0..4).filter(|i| *i != alpha).map(|i| u32::from(px[i])).sum();
        Some([px[alpha], (color / 3) as u8])
    };
    Some([pixel(l, t)?, pixel(r, t)?, pixel(l, b)?, pixel(r, b)?])
}

/// Which byte of a 32-bit pixel holds alpha.
fn alpha_byte(info: CGImageAlphaInfo, bitmap: CGBitmapInfo) -> usize {
    let first = matches!(info, CGImageAlphaInfo::First | CGImageAlphaInfo::PremultipliedFirst | CGImageAlphaInfo::NoneSkipFirst);
    let little = (bitmap.0 & CGBitmapInfo::ByteOrderInfoMask.0) == CGImageByteOrderInfo::Order32Little.0;
    match (first, little) {
        (true, false) | (false, true) => 0,
        (true, true) | (false, false) => 3,
    }
}

/// A still of window `window` at its native pixel size, its points-to-pixels scale, and where
/// the window is in it.
fn capture(window: u32, timeout: Duration) -> Result<(CFRetained<CGImage>, f64, Extent), String> {
    let (tx, rx) = mpsc::channel::<Result<(CFRetained<CGImage>, f64, Extent), String>>();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        // SAFETY: SCK passes a valid object or null for each argument.
        let (content, error) = unsafe { (content.as_ref(), error.as_ref()) };
        let Some(content) = content else {
            let why = error.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
            let _ = tx.send(Err(format!("no shareable content: {why}")));
            return;
        };
        // SAFETY: plain getters on live SCK objects.
        let windows = unsafe { content.windows() };
        let found = windows.iter().find(|w| unsafe { w.windowID() } == window);
        let Some(found) = found else {
            let _ = tx.send(Err(format!("window {window} is not in the shareable content")));
            return;
        };
        // At its native size, set up as every window capture is (child windows included).
        let WindowShot { filter, config, scale, extent, clear } = window_shot(&found, &windows, &display_frames(content), (u32::MAX, u32::MAX));
        // SAFETY: plain setters on a live configuration.
        unsafe {
            config.setShowsCursor(false);
            config.setScalesToFit(false);
        }
        let done = tx.clone();
        let on_image = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
            let _background = &clear;
            let reply = match std::ptr::NonNull::new(image) {
                // SAFETY: SCK hands a valid CGImage for the duration of the block.
                Some(ptr) => Ok((unsafe { CFRetained::retain(ptr) }, scale, extent)),
                // SAFETY: SCK passes a valid NSError or null.
                None => Err(unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string()).unwrap_or_default()),
            };
            let _ = done.send(reply);
        });
        // SAFETY: filter, configuration and handler are valid; SCK copies the block.
        unsafe { SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(&filter, &config, Some(&on_image)) };
    });
    // SAFETY: the handler matches the documented signature; SCK copies the block.
    unsafe { SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(false, true, &handler) };
    rx.recv_timeout(timeout).map_err(|_| "the capture did not answer".to_string())?
}
