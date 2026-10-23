//! The wallpaper corner patch's panel and picture (`[windows] corner_patch`, on by
//! default; the geometry and the rules are [`crate::engine::corner_patch`]).
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
//! - **A switch's end** ([`shown_for`]): a switch's cover of the corner, on the motion
//!   overlay, stays past the switch until the patch has been on screen a moment
//!   ([`crate::engine::corner_patch::cover_holds`]), so the corner is never bare between the
//!   two.
//! - **Ordering**: just above the topmost parked window under it
//!   (`orderWindow:relativeTo:` with that window's number); the WM thread checks the order
//!   in the window server's list at every plan (the panel's number: [`window_number`]) and
//!   shows the patch only where it may, sending the plan again to order it anew when it
//!   was right and is not any more.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
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
    /// While hidden, keep the picture rendered for this rectangle anyway (round 8L4): a
    /// switch's cover lays the desktop's still, when it holds the icons only, over it.
    pub picture: Option<Rect>,
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

    /// The render of `key` for `region` finished (`ok`); false when it is not the one wanted
    /// any more (another key, or the same key for another region).
    fn finished(&mut self, key: &str, region: Rect, ok: bool) -> bool {
        if !self.rendering.as_ref().is_some_and(|(k, r)| k == key && *r == region) {
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
    /// When the panel was last ordered in at its rectangle (moved, or shown after hidden);
    /// `None` while hidden. Apart from [`PATCH`], which may be busy in a call into AppKit.
    static SHOWN_AT: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// How long the patch has been on screen at its rectangle by `now` (main thread); `None`
/// while hidden. A switch's cover of the corner waits on it ([`crate::engine::corner_patch::cover_holds`]).
pub fn shown_for(now: Instant) -> Option<Duration> {
    SHOWN_AT.with(|s| s.get()).map(|t| now.saturating_duration_since(t))
}

thread_local! {
    static PATCH: std::cell::RefCell<Option<Patch>> = const { std::cell::RefCell::new(None) };
    /// The latest plan that found the patch busy, applied [`BUSY_RETRY`] later unless a newer
    /// one is applied first.
    static WAITING: std::cell::RefCell<Option<PatchPlan>> = const { std::cell::RefCell::new(None) };
}

/// How long a call that found the patch busy (a call into AppKit that ran the main queue)
/// waits before it runs again.
const BUSY_RETRY: Duration = Duration::from_millis(20);

/// Runs `f` on the main queue [`BUSY_RETRY`] from now.
fn again_shortly(f: impl FnOnce() + Send + 'static) {
    let when = dispatch2::DispatchTime::try_from(BUSY_RETRY).unwrap_or(dispatch2::DispatchTime::NOW);
    let _ = dispatch2::DispatchQueue::main().after(when, f);
}

/// Applies a plan (main thread); while the patch is busy, [`BUSY_RETRY`] later. Plans come
/// in order, so the latest wins: one waiting is dropped when a newer one is applied.
pub fn apply(mtm: MainThreadMarker, plan: PatchPlan) {
    PATCH.with(|p| {
        let Ok(mut p) = p.try_borrow_mut() else {
            if WAITING.with(|w| w.replace(Some(plan))).is_none() {
                again_shortly(apply_waiting);
            }
            return;
        };
        WAITING.with(|w| w.take());
        let patch = p.get_or_insert_with(|| Patch::new(mtm));
        let wants = |p: &PatchPlan| p.show.or(p.picture).is_some();
        let fresh = wants(&plan) && (!wants(&patch.plan) || plan.space != patch.plan.space);
        patch.plan = plan;
        if fresh {
            patch.wall = None;
        }
        patch.reconcile(mtm);
    });
}

/// The plan that found the patch busy, if no newer one was applied meanwhile.
fn apply_waiting() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    if let Some(plan) = WAITING.with(|w| w.take()) {
        apply(mtm, plan);
    }
}

/// The app's poll (each second, main thread): reads the wallpaper again while the patch is
/// wanted and the last read is [`READ_EVERY`] old, a frame boundary passed, or the
/// appearance differs from the read's (a backstop: [`appearance_changed`] comes first).
pub fn poll(mtm: MainThreadMarker) {
    PATCH.with(|p| {
        let Ok(mut p) = p.try_borrow_mut() else { return };
        let Some(patch) = p.as_mut() else { return };
        if patch.plan.show.is_none() && patch.plan.picture.is_none() {
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
    // A switch's still of the desktop is of the old Desktop or appearance (8L3).
    crate::platform::desktop_shot::forget();
    let until = Instant::now() + SETTLE;
    let busy = PATCH.with(|p| {
        let Ok(mut p) = p.try_borrow_mut() else { return true };
        if let Some(patch) = p.as_mut() {
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
        false
    });
    if busy {
        // Busy (a call into AppKit that ran the main queue): the change is not lost; once
        // more shortly, the settling counted from then.
        again_shortly(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                settle(mtm, why);
            }
        });
        return;
    }
    let when = dispatch2::DispatchTime::try_from(SETTLE).unwrap_or(dispatch2::DispatchTime::NOW);
    let _ = dispatch2::DispatchQueue::main().after(when, settled);
}

/// [`SETTLE`] after a change: the second read, and the patch may show again.
fn settled() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    PATCH.with(|p| {
        let Ok(mut p) = p.try_borrow_mut() else {
            // Busy: once more, shortly.
            again_shortly(settled);
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

/// Where a switch's cover can come from (round 8L2): the rendered block of the picture the
/// patch would show now (the wallpaper read, its picture rendered for that read), and until
/// when a Space or appearance change settles. Published by the main thread after every
/// [`Patch::reconcile`]; read by the WM thread before a switch ([`cover_ready`]) and by the
/// strip as it draws one ([`cover_image`]).
#[derive(Debug, Clone, Copy, PartialEq)]
struct CoverSource {
    /// The block rendered (screen-local), its scale, and the screen (global).
    region: Rect,
    scale: f64,
    screen: Rect,
}

/// The cover's source and the settling deadline, for any thread.
fn cover_slot() -> &'static Mutex<(Option<CoverSource>, Option<Instant>)> {
    static S: OnceLock<Mutex<(Option<CoverSource>, Option<Instant>)>> = OnceLock::new();
    S.get_or_init(|| Mutex::new((None, None)))
}

thread_local! {
    /// The picture of [`cover_slot`]'s source (main thread): the strip crops it without
    /// borrowing the patch, which may be busy in a call into AppKit.
    static COVER_PICTURE: std::cell::RefCell<Option<CFRetained<CGImage>>> = const { std::cell::RefCell::new(None) };
}

/// Why `rect` (global, top-left) could not be covered with the patch's picture now.
fn cover_from(source: Option<CoverSource>, settle_until: Option<Instant>, rect: Rect, now: Instant) -> Result<CoverSource, &'static str> {
    if settle_until.is_some_and(|t| now < t) {
        return Err("a Space or appearance change settles");
    }
    let c = source.ok_or("the corner patch has no picture of the wallpaper now")?;
    wp::crop_px(c.region, wp::local(rect, c.screen), c.scale).ok_or("the corner patch's picture does not hold the corner")?;
    Ok(c)
}

/// Whether the switch's cover of `rect` (global, top-left, whole points) can be drawn now
/// (any thread; round 8L2): the picture the patch would show holds it, and no Space or
/// appearance change settles. The reason when not: an activation then arrives at once
/// instead of growing (the WM thread decides before the strip is told).
pub fn cover_ready(rect: Rect) -> Result<(), &'static str> {
    let (source, settle) = cover_slot().lock().map(|g| *g).map_err(|_| "the corner patch's state is not readable")?;
    cover_from(source, settle, rect, Instant::now()).map(|_| ())
}

/// The patch's picture of `rect` (global, top-left, whole points) and its scale, for a
/// switch's motion overlay to cover the park corner while incoming windows wait there (round
/// 8L, [`crate::engine::corner_patch::switch_cover`]); main thread. Only the picture the patch
/// would show now ([`cover_ready`]). `None` otherwise: the corner is left as it is.
pub fn cover_image(rect: Rect) -> Option<(CFRetained<CGImage>, f64)> {
    let (source, settle) = cover_slot().lock().map(|g| *g).ok()?;
    let c = cover_from(source, settle, rect, Instant::now()).ok()?;
    let picture = COVER_PICTURE.with(|p| p.borrow().clone())?;
    let image = crop_for(&picture, c.region, c.scale, c.screen, rect)?;
    Some((image, c.scale))
}

/// The main display changed (size, scale, color space): read the wallpaper again (while the
/// patch is busy, [`BUSY_RETRY`] later).
pub fn display_changed(mtm: MainThreadMarker) {
    crate::platform::desktop_shot::forget();
    PATCH.with(|p| match p.try_borrow_mut() {
        Ok(mut p) => {
            if let Some(patch) = p.as_mut() {
                patch.wall = None;
                patch.reconcile(mtm);
            }
        }
        Err(_) => again_shortly(|| {
            if let Some(mtm) = MainThreadMarker::new() {
                display_changed(mtm);
            }
        }),
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
            SHOWN_AT.with(|s| s.set(None));
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

    /// Brings the panel to the plan, then publishes where a switch's cover can come from
    /// ([`cover_ready`]).
    fn reconcile(&mut self, mtm: MainThreadMarker) {
        self.reconcile_panel(mtm);
        self.publish_cover();
    }

    /// The picture the patch would show now, for a switch's cover ([`CoverSource`]).
    fn publish_cover(&self) {
        let wall = self.wall.as_ref().and_then(|(_, w)| w.outcome.as_ref().ok().map(|(_, job)| (w.screen, job.key())));
        let picture = wall.and_then(|(screen, key)| self.picture.as_ref().filter(|p| p.key == key).map(|p| (screen, p)));
        let source = picture.map(|(screen, p)| CoverSource { region: p.region, scale: p.scale, screen });
        COVER_PICTURE.with(|c| {
            if let Ok(mut c) = c.try_borrow_mut() {
                *c = picture.map(|(_, p)| p.image.clone());
            }
        });
        if let Ok(mut slot) = cover_slot().lock() {
            *slot = (source, self.settle_until);
        }
    }

    fn reconcile_panel(&mut self, mtm: MainThreadMarker) {
        let Some(rect) = self.plan.show else {
            self.hide();
            self.prepare(mtm);
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
                start_render(job, wall.screen, rect, region);
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

    /// Hidden, keeps the picture rendered for the plan's `picture` rectangle (round 8L4: a
    /// switch's cover lays the desktop's still of the icons over it); nothing is shown.
    fn prepare(&mut self, mtm: MainThreadMarker) {
        let Some(rect) = self.plan.picture else { return };
        if self.wall.is_none() {
            let wall = self.reader.read(mtm);
            self.wall = Some((Instant::now(), wall));
        }
        let Some((_, wall)) = self.wall.clone() else { return };
        let Ok((_, job)) = &wall.outcome else { return };
        let key = job.key();
        if self.picture.as_ref().is_some_and(|p| p.key != key) {
            self.picture = None;
        }
        let local = wp::local(rect, wall.screen);
        if self.picture.as_ref().is_some_and(|p| wp::crop_px(p.region, local, p.scale).is_some()) {
            return;
        }
        let region = wp::region(Size::new(wall.screen.w, wall.screen.h), local);
        if self.renders.want(&key, region) == Want::Start {
            start_render(job.clone(), wall.screen, rect, region);
        }
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
            SHOWN_AT.with(|s| s.set(Some(Instant::now())));
            tracing::debug!(%rect, above = ?self.plan.above, "corner patch: shown");
        }
        self.shown = Some(rect);
        NUMBER.store(self.panel.windowNumber() as u32, Ordering::SeqCst);
    }

    /// A render of `region` finished (main thread): keep it when it is still the one wanted.
    fn rendered(&mut self, mtm: MainThreadMarker, key: String, region: Rect, scale: f64, result: Result<(Rect, CFRetained<CGImage>), String>) {
        if !self.renders.finished(&key, region, result.is_ok()) {
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

/// Hands a finished render of `region` to the patch (main thread); while the patch is busy
/// (a call into AppKit that ran the main queue), once more [`BUSY_RETRY`] later.
fn deliver(key: String, region: Rect, scale: f64, result: Result<(Rect, CFRetained<CGImage>), String>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    PATCH.with(|p| match p.try_borrow_mut() {
        Ok(mut p) => {
            if let Some(p) = p.as_mut() {
                p.rendered(mtm, key, region, scale, result);
            }
        }
        Err(_) => again_shortly(move || deliver(key, region, scale, result)),
    });
}

/// Renders on a thread of its own (decoding a large HEIC frame takes a moment) and hands
/// the result to the main thread, with the `region` it was started for.
fn start_render(job: Job, screen: Rect, rect: Rect, region: Rect) {
    let key = job.key();
    let spawned = std::thread::Builder::new().name("corner-patch-render".into()).spawn(move || {
        let started = Instant::now();
        // A panic in the decoder (an odd HEIC, say) must not take the app down (Fable M2):
        // it reads as a failed render, so the patch stays off until the picture changes.
        let result = crate::platform::safety::guarded("the corner patch's render", || picture_for(&job, screen, rect))
            .unwrap_or_else(|| Err("the render panicked".to_string()));
        tracing::debug!(ms = started.elapsed().as_millis() as u64, ok = result.is_ok(), "corner patch: rendered");
        let scale = job.scale;
        dispatch2::DispatchQueue::main().exec_async(move || deliver(key, region, scale, result));
    });
    if let Err(e) = spawned {
        tracing::warn!("corner patch: cannot start a render: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switch_cover_needs_the_picture_the_patch_would_show_holding_the_corner() {
        // 8L2: decided on the WM thread before the switch; not ready means the activation
        // arrives at once instead of growing.
        let screen = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let source = CoverSource { region: Rect::new(2000.0, 900.0, 560.0, 540.0), scale: 2.0, screen };
        let corner = Rect::new(2440.0, 1270.0, 120.0, 170.0);
        let now = Instant::now();
        assert_eq!(cover_from(Some(source), None, corner, now), Ok(source));
        assert_eq!(cover_from(None, None, corner, now), Err("the corner patch has no picture of the wallpaper now"));
        let outside = Rect::new(1900.0, 1270.0, 660.0, 170.0);
        assert_eq!(cover_from(Some(source), None, outside, now), Err("the corner patch's picture does not hold the corner"));
        let settling = Some(now + Duration::from_millis(200));
        assert_eq!(cover_from(Some(source), settling, corner, now), Err("a Space or appearance change settles"));
        assert_eq!(cover_from(Some(source), Some(now), corner, now), Ok(source), "settled");
    }

    #[test]
    fn a_failed_render_is_not_tried_again_until_its_key_changes() {
        let region = Rect::new(1320.0, 609.0, 480.0, 560.0);
        let mut m = RenderMemo::default();
        assert_eq!(m.want("a", region), Want::Start);
        assert_eq!(m.want("a", region), Want::Wait, "in flight");
        assert!(m.finished("a", region, false));
        assert_eq!(m.want("a", region), Want::Failed, "failed: no new render every read");
        assert_eq!(m.want("a", Rect::new(0.0, 0.0, 10.0, 10.0)), Want::Failed, "nor for another region");
        assert_eq!(m.want("b", region), Want::Start, "a new key renders");
        assert!(m.finished("b", region, true));
        assert_eq!(m.want("b", region), Want::Start, "a success is kept as the picture, not here");
    }

    #[test]
    fn an_answer_no_longer_wanted_is_dropped() {
        let region = Rect::new(0.0, 0.0, 10.0, 10.0);
        let mut m = RenderMemo::default();
        assert_eq!(m.want("a", region), Want::Start);
        assert_eq!(m.want("b", region), Want::Start, "the picture changed meanwhile");
        assert!(!m.finished("a", region, false), "a's answer is stale");
        assert_eq!(m.failed, None, "and a stale failure is not remembered");
        m.drop_in_flight();
        assert!(!m.finished("b", region, true), "dropped at a Space change");
    }

    #[test]
    fn an_answer_for_another_region_is_not_the_one_wanted() {
        let (old, new) = (Rect::new(0.0, 0.0, 10.0, 10.0), Rect::new(0.0, 0.0, 20.0, 30.0));
        let mut m = RenderMemo::default();
        assert_eq!(m.want("a", old), Want::Start);
        // The rectangle grew past the block while it rendered: the same key, a new region.
        assert_eq!(m.want("a", new), Want::Start);
        assert!(!m.finished("a", old, true), "the old region's picture is not kept");
        assert!(!m.finished("a", old, false), "nor its failure remembered");
        assert_eq!(m.failed, None);
        assert_eq!(m.want("a", new), Want::Wait, "the new one is still in flight");
        assert!(m.finished("a", new, true));
    }
}
