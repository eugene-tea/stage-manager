//! Test only (`ctl debug-overlay`): what our own motion overlay draws, read back as pixels.
//!
//! A scripted test asks where a ghost really is on screen (not where our code thinks it put
//! it): the overlay window is captured through ScreenCaptureKit at its native size and the
//! bounds of its opaque pixels (alpha above half, so the soft shadows are left out) come back
//! in global top-left points. Only our own overlay window is ever captured, and no image is
//! kept.

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::AllocAnyThread;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGBitmapInfo, CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo};
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration};

use crate::engine::geometry::{Point, Rect};

/// The bounds of what window `window` (our overlay, top-left corner at `origin`, global)
/// draws, within `region` (global) when given; `None` when nothing opaque is there.
pub fn opaque_bounds(window: u32, origin: Point, region: Option<Rect>, timeout: Duration) -> Result<Option<Rect>, String> {
    let (image, scale) = capture(window, timeout)?;
    let w = CGImage::width(Some(&image));
    let h = CGImage::height(Some(&image));
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

/// Which byte of a 32-bit pixel holds alpha.
fn alpha_byte(info: CGImageAlphaInfo, bitmap: CGBitmapInfo) -> usize {
    let first = matches!(info, CGImageAlphaInfo::First | CGImageAlphaInfo::PremultipliedFirst | CGImageAlphaInfo::NoneSkipFirst);
    let little = (bitmap.0 & CGBitmapInfo::ByteOrderInfoMask.0) == CGImageByteOrderInfo::Order32Little.0;
    match (first, little) {
        (true, false) | (false, true) => 0,
        (true, true) | (false, false) => 3,
    }
}

/// A still of window `window` at its native pixel size, and its points-to-pixels scale.
fn capture(window: u32, timeout: Duration) -> Result<(CFRetained<CGImage>, f64), String> {
    let (tx, rx) = mpsc::channel::<Result<(CFRetained<CGImage>, f64), String>>();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        // SAFETY: SCK passes a valid object or null for each argument.
        let (content, error) = unsafe { (content.as_ref(), error.as_ref()) };
        let Some(content) = content else {
            let why = error.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
            let _ = tx.send(Err(format!("no shareable content: {why}")));
            return;
        };
        // SAFETY: plain getters on live SCK objects.
        let found = unsafe { content.windows() }.iter().find(|w| unsafe { w.windowID() } == window);
        let Some(found) = found else {
            let _ = tx.send(Err(format!("window {window} is not in the shareable content")));
            return;
        };
        // SAFETY: designated initializer with a live SCWindow; plain setters and getters.
        let (filter, config, scale) = unsafe {
            let filter = SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &found);
            let rect = filter.contentRect();
            let scale = f64::from(filter.pointPixelScale()).max(1.0);
            let config = SCStreamConfiguration::new();
            config.setWidth((rect.size.width * scale).round().max(1.0) as usize);
            config.setHeight((rect.size.height * scale).round().max(1.0) as usize);
            config.setShowsCursor(false);
            config.setScalesToFit(false);
            (filter, config, scale)
        };
        let done = tx.clone();
        let on_image = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
            let reply = match std::ptr::NonNull::new(image) {
                // SAFETY: SCK hands a valid CGImage for the duration of the block.
                Some(ptr) => Ok((unsafe { CFRetained::retain(ptr) }, scale)),
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
