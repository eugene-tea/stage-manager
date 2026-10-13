//! Draws the menu bar glyph ([`stage_manager::ui::glyph::status_image`], the code the status
//! item uses) into PNGs, black on transparent, for previews and the docs:
//!
//!     cargo run --example glyph_png -- <out-dir>     # glyph@1x.png, glyph@2x.png, glyph@8x.png
//!     cargo run --example glyph_png -- <out-dir> <corner radius> <far scale>   # a variant:
//!         glyph-r<radius>-s<scale>@{1,2,8}x.png, to compare looks before changing the default
//!
//! Offscreen: no window, no screen access.

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSDeviceRGBColorSpace, NSGraphicsContext};
    use objc2_foundation::{MainThreadMarker, NSDictionary, NSPoint, NSRect, NSSize};
    use stage_manager::ui::glyph;

    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&out)?;
    let _mtm = MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("not on the main thread"))?;
    let mut args = std::env::args().skip(2);
    let (style, prefix) = match (args.next(), args.next()) {
        (Some(r), Some(s)) => {
            let style = glyph::Style { corner_radius: r.parse()?, far_scale: s.parse()? };
            (style, format!("glyph-r{r}-s{s}"))
        }
        _ => (glyph::Style::DEFAULT, "glyph".to_string()),
    };
    let image = glyph::image(style);
    let (w, h) = glyph::SIZE;
    for scale in [1usize, 2, 8] {
        let (pw, ph) = ((w as usize * scale) as isize, (h as usize * scale) as isize);
        // SAFETY: null planes ask AppKit to allocate the buffer; the sizes are positive.
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(), std::ptr::null_mut(), pw, ph, 8, 4, true, false, NSDeviceRGBColorSpace, 0, 0,
            )
        }
        .ok_or_else(|| anyhow::anyhow!("no bitmap"))?;
        rep.setSize(NSSize::new(w, h));
        let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).ok_or_else(|| anyhow::anyhow!("no context"))?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&ctx));
        image.drawInRect(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)));
        NSGraphicsContext::restoreGraphicsState_class();
        // SAFETY: PNG with no properties.
        let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }
            .ok_or_else(|| anyhow::anyhow!("no PNG"))?;
        let path = out.join(format!("{prefix}@{scale}x.png"));
        std::fs::write(&path, data.to_vec())?;
        println!("{}", path.display());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {}
