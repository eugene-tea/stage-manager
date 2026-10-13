//! Offscreen rendering of a layer tree (feature `debug-render`): Core Animation's
//! `CARenderer` draws into a Metal texture, so the strip can be checked by eye or by script
//! while the screen is locked or without Screen Recording. Output: raw BGRA rows, preceded by
//! a one-line header `BGRA <width> <height>\n`.

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CGRect;
use objc2_metal::{
    MTLCreateSystemDefaultDevice, MTLDevice, MTLOrigin, MTLPixelFormat, MTLRegion, MTLSize, MTLStorageMode,
    MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use objc2_quartz_core::{CALayer, CARenderer, CATransaction};

/// Renders `layer` (its bounds, at `scale` pixels per point) into `path`.
pub fn render_to_file(layer: &CALayer, scale: f64, path: &std::path::Path) -> Result<(), String> {
    let bounds: CGRect = layer.bounds();
    let (w, h) = ((bounds.size.width * scale).round() as usize, (bounds.size.height * scale).round() as usize);
    if w == 0 || h == 0 {
        return Err("empty layer".into());
    }
    let device: Retained<ProtocolObject<dyn MTLDevice>> = MTLCreateSystemDefaultDevice().ok_or("no Metal device")?;
    // SAFETY: a descriptor for a plain 2D texture; sizes are non-zero.
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(MTLPixelFormat::BGRA8Unorm, w, h, false)
    };
    desc.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite);
    desc.setStorageMode(MTLStorageMode::Shared);
    let texture = device.newTextureWithDescriptor(&desc).ok_or("cannot make a texture")?;
    // SAFETY: the texture outlives the renderer; no options.
    let renderer = unsafe { CARenderer::rendererWithMTLTexture_options(&texture, None) };
    let saved = (layer.transform(), layer.anchorPoint(), layer.position());
    layer.setAnchorPoint(objc2_core_foundation::CGPoint::new(0.0, 0.0));
    layer.setPosition(objc2_core_foundation::CGPoint::new(0.0, 0.0));
    layer.setTransform(objc2_quartz_core::CATransform3D::new_scale(scale, scale, 1.0));
    renderer.setLayer(Some(layer));
    let target = CGRect::new(objc2_core_foundation::CGPoint::new(0.0, 0.0), objc2_core_foundation::CGSize::new(w as f64, h as f64));
    renderer.setBounds(target);
    for _ in 0..3 {
        CATransaction::flush();
        // SAFETY: no timestamp is a valid argument.
        unsafe { renderer.beginFrameAtTime_timeStamp(objc2_quartz_core::CACurrentMediaTime() + 1.0, std::ptr::null_mut()) };
        renderer.addUpdateRect(target);
        renderer.render();
        renderer.endFrame();
    }
    let mut bytes = vec![0u8; w * h * 4];
    let region = MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: w, height: h, depth: 1 } };
    let ptr = NonNull::new(bytes.as_mut_ptr().cast()).ok_or("null buffer")?;
    // SAFETY: `bytes` holds exactly h rows of w * 4 bytes.
    unsafe { texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(ptr, w * 4, region, 0) };
    renderer.setLayer(None);
    layer.setTransform(saved.0);
    layer.setAnchorPoint(saved.1);
    layer.setPosition(saved.2);
    // The texture's first row is the layer's bottom: write rows top first.
    let mut out = format!("BGRA {w} {h}\n").into_bytes();
    for row in bytes.chunks_exact(w * 4).rev() {
        out.extend_from_slice(row);
    }
    std::fs::write(path, out).map_err(|e| e.to_string())
}
