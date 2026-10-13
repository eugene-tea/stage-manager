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
//!   the patch stays hidden: the sliver is better than a wrong picture. A picture that
//!   cannot be rendered is not tried again until its key changes ([`RenderMemo`]).
//! - **Redraws**: the wallpaper is read again (main thread) when the patch is first wanted,
//!   on a display change, every [`READ_EVERY`] while the patch is wanted (NSWorkspace's URL
//!   and options, the settings file's modification time, the picture's: cheap), and at a
//!   time-of-day or solar frame's boundary. A read naming a different picture hides the
//!   patch until the new one is rendered.
//! - **Space and appearance changes** ([`space_changed`], [`appearance_changed`]): the patch
//!   is ordered out at once (each Desktop may have its own picture; the frame may change
//!   with the appearance), the Space's picture dropped, the wallpaper read again at once
//!   and once more [`SETTLE`] later (NSWorkspace may still name the old Desktop's picture,
//!   and the app's appearance may lag the notification, at the first read); nothing shows
//!   before that second read.
//! - **Ordering**: just above the topmost parked window under it
//!   (`orderWindow:relativeTo:` with that window's number); the WM thread checks the order
//!   in the window server's list at every plan (the panel's number: [`window_number`]) and
//!   shows the patch only where it may, sending the plan again to order it anew when it
//!   was right and is not any more.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
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

/// How long after a Space or appearance change the wallpaper is read the second time, the
/// patch hidden until then.
pub const SETTLE: Duration = Duration::from_millis(300);

/// The patch panel's window number once it exists (0 before).
static NUMBER: AtomicU32 = AtomicU32::new(0);

/// What the shown panel was last ordered against: the window number above the reorder
/// count; [`NOT_ORDERED`] while hidden or ordered to the front.
static ORDERED_AGAINST: AtomicU64 = AtomicU64::new(NOT_ORDERED);
const NOT_ORDERED: u64 = u64::MAX;

/// The window the shown panel was last ordered just above, and the plan's reorder count
/// then; `None` while it is hidden (or was ordered to the front, nothing under it).
pub fn ordered_against() -> Option<(u32, u32)> {
    match ORDERED_AGAINST.load(Ordering::SeqCst) {
        NOT_ORDERED => None,
        v => Some(((v >> 32) as u32, v as u32)),
    }
}

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
    /// The render in flight and the key that failed.
    renders: RenderMemo,
    /// Nothing shows before this (a Space or appearance change settling, [`SETTLE`]).
    settle_until: Option<Instant>,
    /// What the log last said (a reason it is off, or what it draws).
    said: Option<String>,
}

/// The render in flight, and the picture key whose render failed (not tried again until
/// the key changes: a new file version, frame, scale or screen).
#[derive(Debug, Default, PartialEq)]
struct RenderMemo {
    rendering: Option<(String, Rect)>,
    failed: Option<String>,
}

/// What [`RenderMemo::want`] says to do.
#[derive(Debug, PartialEq)]
enum Want {
    /// Start this render.
    Start,
    /// It is in flight.
    Wait,
    /// This key failed: no render.
    Failed,
}

impl RenderMemo {
    /// A render of `key` for `region` is wanted.
    fn want(&mut self, key: &str, region: Rect) -> Want {
        if self.failed.as_deref() == Some(key) {
            return Want::Failed;
        }
        if self.rendering.as_ref().is_some_and(|(k, r)| k == key && *r == region) {
            return Want::Wait;
        }
        self.rendering = Some((key.to_owned(), region));
        Want::Start
    }

    /// The render of `key` finished (`ok`); false when it is not the one wanted any more.
    fn finished(&mut self, key: &str, ok: bool) -> bool {
        if self.rendering.as_ref().map(|(k, _)| k.as_str()) != Some(key) {
            return false;
        }
        self.rendering = None;
        if !ok {
            self.failed = Some(key.to_owned());
        }
        true
    }

    /// Forgets the render in flight (its answer is dropped when it comes).
    fn drop_in_flight(&mut self) {
        self.rendering = None;
    }
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
/// appearance differs from the read's (a backstop: [`appearance_changed`] comes first).
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

/// The user went to another Space (main thread, at the notification): the patch goes at
/// once, with the picture it showed (the new Desktop may have another); the wallpaper is
/// read again now and once more [`SETTLE`] later, nothing shown before.
pub fn space_changed(mtm: MainThreadMarker) {
    settle(mtm, "a Space change");
}

/// The appearance changed (main thread, at `AppleInterfaceThemeChangedNotification`): as
/// [`space_changed`], the picture kept (a frame's key tells whether it is still the one).
pub fn appearance_changed(mtm: MainThreadMarker) {
    settle(mtm, "an appearance change");
}

fn settle(mtm: MainThreadMarker, why: &'static str) {
    let until = Instant::now() + SETTLE;
    PATCH.with(|p| {
        if let Ok(mut p) = p.try_borrow_mut()
            && let Some(patch) = p.as_mut()
        {
            tracing::debug!(why, "corner patch: hidden until the wallpaper is read again");
            patch.hide();
            if why == "a Space change" {
                patch.picture = None;
                patch.renders.drop_in_flight();
            }
            patch.settle_until = Some(until);
            patch.wall = None;
            patch.reconcile(mtm);
        }
    });
    let when = dispatch2::DispatchTime::try_from(SETTLE).unwrap_or(dispatch2::DispatchTime::NOW);
    let _ = dispatch2::DispatchQueue::main().after(when, settled);
}

/// [`SETTLE`] after a change: the second read, and the patch may show again.
fn settled() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    PATCH.with(|p| {
        let Ok(mut p) = p.try_borrow_mut() else {
            // Busy: once more, shortly.
            let _ = dispatch2::DispatchQueue::main().after(dispatch2::DispatchTime::try_from(Duration::from_millis(20)).unwrap_or(dispatch2::DispatchTime::NOW), settled);
            return;
        };
        let Some(patch) = p.as_mut() else { return };
        // A later change settles later.
        if patch.settle_until.is_some_and(|t| Instant::now() + Duration::from_millis(5) >= t) {
            patch.settle_until = None;
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
            renders: RenderMemo::default(),
            settle_until: None,
            said: None,
        }
    }

    fn hide(&mut self) {
        self.showing = None;
        ORDERED_AGAINST.store(NOT_ORDERED, Ordering::SeqCst);
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
            if self.renders.want(&key, region) == Want::Start {
                start_render(job, wall.screen, rect);
            }
            return;
        };
        if self.settle_until.is_some_and(|t| Instant::now() < t) {
            // Rendered ahead; shown once the second read confirms the picture.
            self.hide();
            return;
        }
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
            Some(id) => {
                self.panel.orderWindow_relativeTo(NSWindowOrderingMode::Above, id as isize);
                ORDERED_AGAINST.store((u64::from(id) << 32) | u64::from(self.plan.reorder), Ordering::SeqCst);
            }
            None => {
                self.panel.orderFrontRegardless();
                ORDERED_AGAINST.store(NOT_ORDERED, Ordering::SeqCst);
            }
        }
        if self.shown != Some(rect) {
            tracing::debug!(%rect, above = ?self.plan.above, "corner patch: shown");
        }
        self.shown = Some(rect);
        NUMBER.store(self.panel.windowNumber() as u32, Ordering::SeqCst);
    }

    /// A render finished (main thread): keep it when it is still the one wanted.
    fn rendered(&mut self, mtm: MainThreadMarker, key: String, scale: f64, result: Result<(Rect, CFRetained<CGImage>), String>) {
        if !self.renders.finished(&key, result.is_ok()) {
            return;
        }
        match result {
            Ok((region, image)) => {
                self.picture = Some(Picture { key, region, scale, image });
                self.reconcile(mtm);
            }
            Err(why) => {
                self.hide();
                self.say(format!("the picture cannot be drawn (not tried again until it changes): {why}"), true);
            }
        }
    }
}

/// Hands a finished render to the patch (main thread); while the patch is busy (a call
/// into AppKit that ran the main queue), once more on the next turn of the main queue.
fn deliver(key: String, scale: f64, result: Result<(Rect, CFRetained<CGImage>), String>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    PATCH.with(|p| match p.try_borrow_mut() {
        Ok(mut p) => {
            if let Some(p) = p.as_mut() {
                p.rendered(mtm, key, scale, result);
            }
        }
        Err(_) => dispatch2::DispatchQueue::main().exec_async(move || deliver(key, scale, result)),
    });
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
        dispatch2::DispatchQueue::main().exec_async(move || deliver(key, scale, result));
    });
    if let Err(e) = spawned {
        tracing::warn!("corner patch: cannot start a render: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_render_is_not_tried_again_until_its_key_changes() {
        let region = Rect::new(1320.0, 609.0, 480.0, 560.0);
        let mut m = RenderMemo::default();
        assert_eq!(m.want("a", region), Want::Start);
        assert_eq!(m.want("a", region), Want::Wait, "in flight");
        assert!(m.finished("a", false));
        assert_eq!(m.want("a", region), Want::Failed, "failed: no new render every read");
        assert_eq!(m.want("a", Rect::new(0.0, 0.0, 10.0, 10.0)), Want::Failed, "nor for another region");
        assert_eq!(m.want("b", region), Want::Start, "a new key renders");
        assert!(m.finished("b", true));
        assert_eq!(m.want("b", region), Want::Start, "a success is kept as the picture, not here");
    }

    #[test]
    fn an_answer_no_longer_wanted_is_dropped() {
        let region = Rect::new(0.0, 0.0, 10.0, 10.0);
        let mut m = RenderMemo::default();
        assert_eq!(m.want("a", region), Want::Start);
        assert_eq!(m.want("b", region), Want::Start, "the picture changed meanwhile");
        assert!(!m.finished("a", false), "a's answer is stale");
        assert_eq!(m.failed, None, "and a stale failure is not remembered");
        m.drop_in_flight();
        assert!(!m.finished("b", true), "dropped at a Space change");
    }
}
