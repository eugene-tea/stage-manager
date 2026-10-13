//! The wallpaper corner patch's panel and picture (`[windows] corner_patch`, a prototype,
//! off by default; the geometry and the rules are [`crate::engine::corner_patch`]).
//!
//! - **Panel**: borderless, non-activating, click-through, no shadow, opaque, at the normal
//!   window level, never key or main; collection behavior [`panels::corner_patch`]
//!   (`Transient`: hidden by Mission Control, App Exposé and Show Desktop; `IgnoresCycle`:
//!   out of ⌘`). Its sharing type is left at the default, so screenshots show it as the
//!   wallpaper it shows.
//! - **Picture**: the desktop picture drawn from its file, never captured
//!   ([`crate::platform::wallpaper`], decided by [`crate::engine::wallpaper`]): no Screen
//!   Recording, no menu bar recording indicator. A block of the corner is rendered once on
//!   a render thread ([`picture_for`]); the patch shows a whole-pixel crop of it
//!   ([`crop_for`]), so a new rectangle inside the block needs no new render. Until the
//!   picture for the current wallpaper is ready, and whenever it cannot be drawn exactly,
//!   the patch stays hidden: the sliver is better than a wrong picture.
//! - **Redraws**: the wallpaper is read again (main thread) when the patch is first wanted,
//!   on a Space change (each Desktop may have its own picture), on a display change, every
//!   [`READ_EVERY`] while the patch is wanted (NSWorkspace's URL and options, the settings
//!   file's modification time, the picture's: cheap), at a time-of-day or solar frame's
//!   boundary, and at once when the appearance changes (checked every poll, each second).
//!   A read naming a different picture hides the patch until the new one is rendered.
//! - **Ordering**: just above the topmost parked window under it
//!   (`orderWindow:relativeTo:` with that window's number); the WM thread checks the order
//!   in the window server's list at every plan (the panel's number: [`window_number`]) and
//!   shows the patch only where it may, sending the plan again to order it anew when it
//!   was right and is not any more.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadOnly;
use objc2_app_kit::{NSBackingStoreType, NSColor, NSNormalWindowLevel, NSPanel, NSWindowOrderingMode, NSWindowStyleMask};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use objc2_quartz_core::{CALayer, CATransaction};

use super::panels;
use crate::engine::geometry::{Rect, Size};
use crate::engine::wallpaper as wp;
use crate::platform::desktop;
use crate::platform::wallpaper::{self, Job, Reader, Wall};

/// What the WM thread asks of the patch.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PatchPlan {
    /// Show it, at this rectangle (global, top-left); `None` hides it.
    pub show: Option<Rect>,
    /// The topmost parked window under it, to order the patch just above.
    pub above: Option<u32>,
    /// Bumped to order the patch again with an otherwise equal plan (it was ordered right
    /// and is not any more).
    pub reorder: u32,
    /// The WM thread's count of Space changes: a new one reads the wallpaper again.
    pub space: u64,
}

/// How often the wallpaper is read again while the patch is wanted.
pub const READ_EVERY: Duration = Duration::from_secs(2);

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

/// A rendered block of the corner.
struct Picture {
    key: String,
    region: Rect,
    scale: f64,
    image: CFRetained<CGImage>,
}

/// The screen-local region to render for a patch at `rect` (global) under `job` on the
/// screen at `screen` (global), and its render: the same path for the patch and the probe.
pub fn picture_for(job: &Job, screen: Rect, rect: Rect) -> Result<(Rect, CFRetained<CGImage>), String> {
    let region = wp::region(Size::new(screen.w, screen.h), wp::local(rect, screen));
    wallpaper::render(job, region).map(|image| (region, image))
}

/// The patch's pixels for `rect` (global) out of a render of `region` (screen-local) of the
/// screen at `screen` (global); `None` unless the region holds it.
pub fn crop_for(picture: &CGImage, region: Rect, scale: f64, screen: Rect, rect: Rect) -> Option<CFRetained<CGImage>> {
    let px = wp::crop_px(region, wp::local(rect, screen), scale)?;
    wallpaper::crop(picture, px)
}

/// The patch's panel, its layer and its picture (main thread).
struct Patch {
    panel: Retained<NSPanel>,
    layer: Retained<CALayer>,
    plan: PatchPlan,
    shown: Option<Rect>,
    /// What the panel shows now (picture key and the plan), to skip a redundant show.
    showing: Option<(String, PatchPlan)>,
    reader: Reader,
    wall: Option<(Instant, Wall)>,
    picture: Option<Picture>,
    /// The key and region of the render in flight.
    rendering: Option<(String, Rect)>,
    /// What the log last said (a reason it is off, or what it draws).
    said: Option<String>,
}

thread_local! {
    static PATCH: std::cell::RefCell<Option<Patch>> = const { std::cell::RefCell::new(None) };
}

/// Applies a plan (main thread).
pub fn apply(mtm: MainThreadMarker, plan: PatchPlan) {
    PATCH.with(|p| {
        let mut p = p.borrow_mut();
        let patch = p.get_or_insert_with(|| Patch::new(mtm));
        let fresh = plan.show.is_some() && (patch.plan.show.is_none() || plan.space != patch.plan.space);
        patch.plan = plan;
        if fresh {
            patch.wall = None;
        }
        patch.reconcile(mtm);
    });
}

/// The app's poll (each second, main thread): reads the wallpaper again while the patch is
/// wanted and the last read is [`READ_EVERY`] old, a frame boundary passed, or the
/// appearance changed.
pub fn poll(mtm: MainThreadMarker) {
    PATCH.with(|p| {
        let Ok(mut p) = p.try_borrow_mut() else { return };
        let Some(patch) = p.as_mut() else { return };
        if patch.plan.show.is_none() {
            return;
        }
        let now = Instant::now();
        let stale = patch.wall.as_ref().is_none_or(|(at, w)| {
            now.duration_since(*at) >= READ_EVERY
                || w.recheck_at.is_some_and(|r| now >= r)
                || w.dark != wallpaper::dark_appearance(mtm)
        });
        if stale {
            patch.wall = None;
            patch.reconcile(mtm);
        }
    });
}

/// The main display changed (size, scale, color space): read the wallpaper again.
pub fn display_changed(mtm: MainThreadMarker) {
    PATCH.with(|p| {
        if let Ok(mut p) = p.try_borrow_mut()
            && let Some(patch) = p.as_mut()
        {
            patch.wall = None;
            patch.reconcile(mtm);
        }
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
        Self {
            panel,
            layer,
            plan: PatchPlan::default(),
            shown: None,
            showing: None,
            reader: Reader::default(),
            wall: None,
            picture: None,
            rendering: None,
            said: None,
        }
    }

    fn hide(&mut self) {
        self.showing = None;
        if self.shown.take().is_some() {
            self.panel.orderOut(None);
            tracing::debug!("corner patch: hidden");
        }
    }

    /// Logs `line` once until something else is said.
    fn say(&mut self, line: String, off: bool) {
        if self.said.as_deref() != Some(line.as_str()) {
            if off {
                tracing::info!("corner patch: off: {line}");
            } else {
                tracing::info!("corner patch: {line}");
            }
            self.said = Some(line);
        }
    }

    fn reconcile(&mut self, mtm: MainThreadMarker) {
        let Some(rect) = self.plan.show else {
            self.hide();
            return;
        };
        if self.wall.is_none() {
            let wall = self.reader.read(mtm);
            self.wall = Some((Instant::now(), wall));
        }
        let Some((_, wall)) = self.wall.clone() else { return };
        let (decision, job) = match &wall.outcome {
            Ok(ok) => ok.clone(),
            Err(why) => {
                self.hide();
                let name = wall.path.as_deref().map(wallpaper::file_name).unwrap_or_default();
                self.say(format!("{why} ({name})"), true);
                return;
            }
        };
        let key = job.key();
        if self.showing.as_ref().is_some_and(|(k, plan)| *k == key && *plan == self.plan) {
            return;
        }
        if self.picture.as_ref().is_some_and(|p| p.key != key) {
            // Another picture or frame: never show the old one meanwhile.
            self.picture = None;
            self.hide();
        }
        let shown = self.picture.as_ref().and_then(|p| crop_for(&p.image, p.region, p.scale, wall.screen, rect));
        let Some(image) = shown else {
            self.hide();
            let region = wp::region(Size::new(wall.screen.w, wall.screen.h), wp::local(rect, wall.screen));
            if self.rendering.as_ref() != Some(&(key.clone(), region)) {
                self.rendering = Some((key, region));
                start_render(job, wall.screen, rect);
            }
            return;
        };
        let style = decision.style.map_or_else(|| "-".into(), |s| format!("{s:?}").to_lowercase());
        self.say(
            format!(
                "drawing {} ({}, frame {}, style {style}, {}, {}x scale, {})",
                job.file_name(),
                decision.kind,
                decision.frame.index,
                decision.placement.name(),
                job.scale,
                job.space_key
            ),
            false,
        );
        self.show(mtm, rect, &image, job.scale);
        self.showing = Some((key, self.plan.clone()));
    }

    fn show(&mut self, mtm: MainThreadMarker, rect: Rect, image: &CGImage, scale: f64) {
        let h = desktop::primary_height(mtm);
        let frame = NSRect::new(NSPoint::new(rect.x, h - rect.y - rect.h), NSSize::new(rect.w, rect.h));
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        // SAFETY: a CGImage is accepted as a layer's contents; the layer retains it.
        let contents = unsafe { &*(image as *const CGImage).cast::<AnyObject>() };
        // SAFETY: as above.
        unsafe { self.layer.setContents(Some(contents)) };
        self.layer.setContentsScale(scale);
        CATransaction::commit();
        self.panel.setFrame_display(frame, false);
        match self.plan.above {
            Some(id) => self.panel.orderWindow_relativeTo(NSWindowOrderingMode::Above, id as isize),
            None => self.panel.orderFrontRegardless(),
        }
        if self.shown != Some(rect) {
            tracing::debug!(%rect, above = ?self.plan.above, "corner patch: shown");
        }
        self.shown = Some(rect);
        NUMBER.store(self.panel.windowNumber() as u32, Ordering::SeqCst);
    }

    /// A render finished (main thread): keep it when it is still the one wanted.
    fn rendered(&mut self, mtm: MainThreadMarker, key: String, scale: f64, result: Result<(Rect, CFRetained<CGImage>), String>) {
        if self.rendering.as_ref().map(|(k, _)| k) != Some(&key) {
            return;
        }
        self.rendering = None;
        match result {
            Ok((region, image)) => {
                self.picture = Some(Picture { key, region, scale, image });
                self.reconcile(mtm);
            }
            Err(why) => {
                self.hide();
                self.say(format!("the picture cannot be drawn: {why}"), true);
            }
        }
    }
}

/// Renders on a thread of its own (decoding a large HEIC frame takes a moment) and hands
/// the result to the main thread.
fn start_render(job: Job, screen: Rect, rect: Rect) {
    let key = job.key();
    let spawned = std::thread::Builder::new().name("corner-patch-render".into()).spawn(move || {
        let started = Instant::now();
        let result = picture_for(&job, screen, rect);
        tracing::debug!(ms = started.elapsed().as_millis() as u64, ok = result.is_ok(), "corner patch: rendered");
        let scale = job.scale;
        dispatch2::DispatchQueue::main().exec_async(move || {
            let Some(mtm) = MainThreadMarker::new() else { return };
            PATCH.with(|p| {
                if let Some(p) = p.borrow_mut().as_mut() {
                    p.rendered(mtm, key, scale, result);
                }
            });
        });
    });
    if let Err(e) = spawned {
        tracing::warn!("corner patch: cannot start a render: {e}");
    }
}
