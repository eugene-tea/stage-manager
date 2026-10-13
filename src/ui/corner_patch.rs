//! The wallpaper corner patch's panel and live capture (`[windows] corner_patch`, a
//! prototype, off by default; the geometry and the rules are
//! [`crate::engine::corner_patch`]).
//!
//! - **Panel**: borderless, non-activating, click-through, no shadow, opaque, at the normal
//!   window level, never key or main; collection behavior [`panels::corner_patch`]
//!   (`Transient`: hidden by Mission Control, App Exposé and Show Desktop; `IgnoresCycle`:
//!   out of ⌘`). Its sharing type is left at the default, so screenshots show it as the
//!   wallpaper it shows.
//! - **Picture**: a ScreenCaptureKit stream of the main display with a filter that includes
//!   only the windows below the normal level (the desktop picture, Finder's desktop icons,
//!   desktop widgets), cropped to the patch's rectangle (`sourceRect`), at most
//!   `[windows] corner_patch_fps` frames a second (a still wallpaper sends none: ScreenCaptureKit
//!   sends frames only when the content changes, so dynamic, light/dark, Aerial and rotating
//!   wallpapers follow on their own). Each frame's IOSurface becomes the layer's contents.
//!   The windows are looked up again on a Space change and at each new rectangle.
//! - **Ordering**: just above the topmost parked window under it
//!   (`orderWindow:relativeTo:` with that window's number); the WM thread checks the order
//!   in the window server's list at every plan (the panel's number: [`window_number`]) and
//!   shows the patch only where it may, sending the plan again to order it anew when it
//!   was right and is not any more.
//! - Hidden at once when the plan says so (no sliver, an overview, a full-screen Space, the
//!   fallback's moving windows), and the stream stops. The stream restarts only for a new
//!   rectangle (the WM thread keeps one that barely changed).

use std::sync::atomic::{AtomicU32, Ordering};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSBackingStoreType, NSColor, NSNormalWindowLevel, NSPanel, NSWindowOrderingMode, NSWindowStyleMask};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGMainDisplayID, CGPreflightScreenCaptureAccess};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::CVPixelBufferGetIOSurface;
use objc2_foundation::{MainThreadMarker, NSArray, NSError, NSPoint, NSRect, NSSize};
use objc2_io_surface::IOSurfaceRef;
use objc2_quartz_core::CALayer;
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType, SCWindow,
};

use super::panels;
use crate::engine::geometry::Rect;
use crate::platform::{capture, desktop};

/// What the WM thread asks of the patch.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchPlan {
    /// Show it, at this rectangle (global, top-left); `None` hides it.
    pub show: Option<Rect>,
    /// The topmost parked window under it, to order the patch just above.
    pub above: Option<u32>,
    /// Bumped to order the patch again with an otherwise equal plan (it was ordered right
    /// and is not any more).
    pub reorder: u32,
    /// Most frames a second.
    pub fps: u32,
}

/// The patch panel's window number once it exists (0 before).
static NUMBER: AtomicU32 = AtomicU32::new(0);

/// The patch panel's window number, for the WM thread's ordering check; `None` before the
/// panel exists.
pub fn window_number() -> Option<u32> {
    match NUMBER.load(Ordering::SeqCst) {
        0 => None,
        n => Some(n),
    }
}

/// A frame's surface, handed from the stream's queue to the main thread.
struct Surface(CFRetained<IOSurfaceRef>);
// SAFETY: an IOSurface is a thread-safe CF object; it is only read (as layer contents) on
// the main thread.
unsafe impl Send for Surface {}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the class has no ivars and no Drop.
    #[unsafe(super = NSObject)]
    #[name = "SMCornerPatchOutput"]
    struct PatchOutput;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for PatchOutput {}

    // SAFETY: the method signature matches SCStreamOutput's.
    unsafe impl SCStreamOutput for PatchOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // SAFETY: a live sample buffer for the call; an idle frame has no image buffer.
            let Some(image) = (unsafe { sample.image_buffer() }) else { return };
            let Some(surface) = CVPixelBufferGetIOSurface(Some(&image)) else { return };
            let surface = Surface(surface);
            DispatchQueue::main().exec_async(move || {
                let surface = surface;
                PATCH.with(|p| {
                    if let Some(p) = p.borrow().as_ref() {
                        p.show_surface(&surface.0);
                    }
                });
            });
        }
    }
);

impl PatchOutput {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: NSObject's designated initializer on a fresh instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// The patch's panel, its layer and its stream (main thread).
struct Patch {
    panel: Retained<NSPanel>,
    layer: Retained<CALayer>,
    stream: Option<(Retained<SCStream>, Retained<PatchOutput>, Rect)>,
    shown: Option<Rect>,
}

thread_local! {
    static PATCH: std::cell::RefCell<Option<Patch>> = const { std::cell::RefCell::new(None) };
}

/// Applies a plan (main thread).
pub fn apply(mtm: MainThreadMarker, plan: PatchPlan) {
    PATCH.with(|p| {
        let mut p = p.borrow_mut();
        let patch = p.get_or_insert_with(|| Patch::new(mtm));
        patch.apply(mtm, plan);
    });
}

impl Patch {
    fn new(mtm: MainThreadMarker) -> Self {
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setOpaque(true);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setLevel(NSNormalWindowLevel);
        panel.setIgnoresMouseEvents(true);
        panel.setCollectionBehavior(panels::corner_patch());
        panel.setHidesOnDeactivate(false);
        // SAFETY: the panel lives for the process (kept in PATCH).
        unsafe { panel.setReleasedWhenClosed(false) };
        let layer = CALayer::new();
        if let Some(view) = panel.contentView() {
            view.setWantsLayer(true);
            view.setLayer(Some(&layer));
        }
        Self { panel, layer, stream: None, shown: None }
    }

    fn apply(&mut self, mtm: MainThreadMarker, plan: PatchPlan) {
        let Some(rect) = plan.show else {
            if self.shown.take().is_some() {
                self.panel.orderOut(None);
                tracing::debug!("corner patch: hidden");
            }
            self.stop();
            return;
        };
        if !(CGPreflightScreenCaptureAccess() && capture::pictures_expected()) {
            // No live picture: an empty or stale patch would be worse than the sliver.
            self.panel.orderOut(None);
            self.shown = None;
            self.stop();
            return;
        }
        let h = desktop::primary_height(mtm);
        let frame = NSRect::new(NSPoint::new(rect.x, h - rect.y - rect.h), NSSize::new(rect.w, rect.h));
        self.panel.setFrame_display(frame, false);
        if self.stream.as_ref().is_none_or(|(_, _, r)| *r != rect) {
            self.stop();
            self.start(rect, plan.fps);
        }
        match plan.above {
            Some(id) => self.panel.orderWindow_relativeTo(NSWindowOrderingMode::Above, id as isize),
            None => self.panel.orderFrontRegardless(),
        }
        if self.shown != Some(rect) {
            tracing::debug!(%rect, above = ?plan.above, "corner patch: shown");
        }
        self.shown = Some(rect);
        NUMBER.store(self.panel.windowNumber() as u32, Ordering::SeqCst);
    }

    fn show_surface(&self, surface: &IOSurfaceRef) {
        // SAFETY: an IOSurface is accepted as a layer's contents; the layer retains it.
        let contents = unsafe { &*(surface as *const IOSurfaceRef).cast::<AnyObject>() };
        // SAFETY: as above.
        unsafe { self.layer.setContents(Some(contents)) };
    }

    fn stop(&mut self) {
        if let Some((stream, _, _)) = self.stream.take() {
            // SAFETY: a running stream; no completion handler needed.
            unsafe { stream.stopCaptureWithCompletionHandler(None) };
        }
    }

    /// Starts the stream for `rect` once the shareable content (the desktop windows) is
    /// known; on the main thread again when it is.
    fn start(&mut self, rect: Rect, fps: u32) {
        let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
            // SAFETY: SCK passes a valid object or null for each.
            let Some(content) = (unsafe { content.as_ref() }) else {
                let why = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
                tracing::warn!(?why, "corner patch: no shareable content; no picture");
                return;
            };
            let Some(filter) = desktop_filter(content) else {
                tracing::warn!("corner patch: no desktop windows found to capture");
                return;
            };
            let filter = SendFilter(filter);
            DispatchQueue::main().exec_async(move || {
                let filter = filter;
                PATCH.with(|p| {
                    if let Some(p) = p.borrow_mut().as_mut()
                        && p.shown == Some(rect)
                        && p.stream.is_none()
                    {
                        p.stream = run_stream(&filter.0, rect, fps);
                    }
                });
            });
        });
        // SAFETY: the handler has the documented signature; SCK copies the block.
        unsafe { SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(false, true, &handler) };
    }
}

struct SendFilter(Retained<SCContentFilter>);
// SAFETY: a content filter is an immutable value object once made.
unsafe impl Send for SendFilter {}

/// The main display with only the windows below the normal level on it (the desktop
/// picture, the desktop icons, widgets).
fn desktop_filter(content: &SCShareableContent) -> Option<Retained<SCContentFilter>> {
    let main = CGMainDisplayID();
    let own = std::process::id() as i32;
    // SAFETY: plain getters of live objects.
    unsafe {
        let display = content.displays().iter().find(|d| d.displayID() == main)?;
        let below: Vec<Retained<SCWindow>> = content
            .windows()
            .iter()
            .filter(|w| w.windowLayer() < 0 && w.isOnScreen())
            .filter(|w| w.owningApplication().is_none_or(|a| a.processID() != own))
            .collect();
        if below.is_empty() {
            return None;
        }
        let list = NSArray::from_retained_slice(&below);
        Some(SCContentFilter::initWithDisplay_includingWindows(SCContentFilter::alloc(), &display, &list))
    }
}

/// Starts a stream of `filter` cropped to `rect` (global, top-left: the main display's
/// origin is the global origin).
fn run_stream(filter: &SCContentFilter, rect: Rect, fps: u32) -> Option<(Retained<SCStream>, Retained<PatchOutput>, Rect)> {
    let scale = 2.0;
    let output = PatchOutput::new();
    // SAFETY: plain setters on a fresh configuration; the stream keeps the output (we keep it
    // too); the queue outlives the stream.
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setSourceRect(CGRect::new(CGPoint::new(rect.x, rect.y), CGSize::new(rect.w, rect.h)));
        config.setWidth((rect.w * scale) as usize);
        config.setHeight((rect.h * scale) as usize);
        config.setMinimumFrameInterval(CMTime::new(1, fps.clamp(1, 60) as i32));
        config.setShowsCursor(false);
        config.setQueueDepth(3);
        config.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
        let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), filter, &config, None);
        let queue = DispatchQueue::new("dev.eugene.stagemanager.corner-patch", None);
        if let Err(e) = stream.addStreamOutput_type_sampleHandlerQueue_error(
            ProtocolObject::from_ref(&*output),
            SCStreamOutputType::Screen,
            Some(&queue),
        ) {
            tracing::warn!("corner patch: cannot add the stream output: {}", e.localizedDescription());
            return None;
        }
        let started = RcBlock::new(|error: *mut NSError| {
            if let Some(e) = error.as_ref() {
                tracing::warn!("corner patch: the stream did not start: {}", e.localizedDescription());
            }
        });
        stream.startCaptureWithCompletionHandler(Some(&started));
        tracing::info!(%rect, fps, "corner patch: live capture of the desktop under the corner");
        Some((stream, output, rect))
    }
}
