//! The wallpaper corner patch's panel and picture (`[windows] corner_patch`, on by
//! default; the geometry and the rules are [`crate::engine::corner_patch`]).
//!
//! - **Panels, one per Desktop** ([`Sheet`]): borderless, non-activating, click-through, no
//!   shadow, opaque, at the normal window level, never key or main; collection behavior
//!   [`panels::corner_patch`] (no Spaces flag: AppKit keeps it on the Desktop it is first
//!   ordered in on, as the strip's panes; `Stationary`: left drawn and in place by Show
//!   Desktop, Mission Control and App Exposé, like the desktop picture; `IgnoresCycle`: out
//!   of ⌘`). Once ordered in, a Desktop's panel stays
//!   ordered in there, shown or hidden by its alpha (ordered out and in again it would go
//!   back to its Desktop, wherever the user is), so it slides in and out with its Desktop
//!   drawn as it was left: nothing moves on a Desktop while the user is away. Which panel
//!   is the shown Desktop's is read from the panels (`isOnActiveSpace`), never from the
//!   engine's Space ids. Its sharing type is left at the default, so screenshots show it as
//!   the wallpaper it shows.
//! - **Picture from a still** (round desktop-still-patch, [`crate::engine::patch_still`]):
//!   where Screen Recording is granted and window captures work, the WM thread's plan names a
//!   still of the desktop ([`PatchPlan::still`], [`crate::platform::desktop_shot`]): the
//!   desktop-level windows only, so icons, widgets and any wallpaper are exact. While the
//!   next one is on its way ([`PatchPlan::hold`]: a Desktop arrived, an icon moved, the
//!   picture changed) a panel a still drew keeps its picture, cropped anew for the plan's
//!   rectangle; a panel left drawn from a still on another Desktop stays while its icons, the
//!   appearance and the predicted frame are unchanged
//!   ([`crate::engine::patch_still::away_still_holds`]). Every wallpaper read tells the still
//!   when the picture changed and when the next frame is due.
//! - **Picture from the file** (no still: no Screen Recording, captures not working yet):
//!   the desktop picture drawn from its file, never captured
//!   ([`crate::platform::wallpaper`], decided by [`crate::engine::wallpaper`]): no Screen
//!   Recording, no menu bar recording indicator, only where no desktop icon lies under the
//!   corner ([`PatchPlan::file`]). Near a solar frame change the frame is one
//!   of a few candidates, settled by [`super::corner_settle`]: they render alike in the
//!   corner, or (only where window captures already work) one still of the desktop's own
//!   windows, never a stream, matches one of them; otherwise the patch stays off. On each
//!   arrival at a Desktop (and at the patch's first draw) one still checks even a solar
//!   frame the clock names exactly, when one may be taken, at most one a picture per
//!   [`wp::SOLAR_RECHECK`]; every still that saw clearly narrows the clock's model
//!   ([`Reader::observe`]) and the frame is decided again. The still picks the frame; the
//!   pixels drawn are still the file's. A block of the corner is rendered once on a render
//!   thread ([`picture_for`]); the patch shows a whole-pixel crop of it
//!   ([`crop_for`]), so a new rectangle inside the block needs no new render. Until the
//!   picture for the current wallpaper is ready, and whenever it cannot be drawn exactly,
//!   the patch stays hidden: the sliver is better than a wrong picture. A picture that
//!   cannot be rendered is not tried again until its key changes ([`RenderMemo`]).
//! - **Redraws**: the wallpaper is read again (main thread) when the patch is first wanted,
//!   on a display change, every [`READ_EVERY`] while the patch is wanted (NSWorkspace's URL
//!   and options, the settings file's modification time, the picture's: cheap), and at a
//!   time-of-day or solar frame's boundary. A read naming a different picture hides the
//!   patch until the new one is rendered.
//! - **Space changes** ([`space_changed`]): the Desktop left keeps its panel as it is; the
//!   arriving Desktop's panel (made on its first visit) is left as it slid in, untouched by
//!   any plan meant for the Desktop left ([`arrival_waits`]), until the WM thread's first
//!   plan for the new Desktop and the wallpaper's second read [`SETTLE`] after the change
//!   (NSWorkspace may still name the old Desktop's picture at the first); then it is drawn
//!   anew or hidden only if the picture, its frame, the rectangle or the order changed.
//!   A Desktop's panel left drawn is hidden while away once its frame is no longer the one
//!   macOS shows (the sun, the appearance: [`wp::still_drawn`]; near a solar frame change,
//!   unless the candidates look alike there) or a desktop icon lies under it, so it never
//!   slides back in wrong; arrived, its frame is settled again. Panels on other Desktops close on a display
//!   change, when the patch is turned off, and past [`MAX_SHEETS`]; two on the Desktop shown
//!   (a Desktop removed in Mission Control) leave one.
//! - **Appearance changes** ([`appearance_changed`]): the patch is hidden at once (the frame
//!   may change with the appearance), the wallpaper read again at once and once more
//!   [`SETTLE`] later (the app's appearance may lag the notification); nothing shows before
//!   that second read.
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

use super::corner_settle::{Check, News, Outcome, Settler};
use super::panels;
use crate::engine::geometry::{Rect, Size};
use crate::engine::wallpaper as wp;
use crate::platform::wallpaper::{self, Job, Reader, Wall};
use crate::platform::{desktop, desktop_shot};

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
    /// `[windows] corner_patch` and the app managing windows: off, every other Desktop's
    /// panel closes.
    pub on: bool,
    /// Finder's desktop icons as last read well (`None`: not known): they show on every
    /// Desktop, so a panel left drawn on another one over an icon is hidden.
    pub items: Option<Vec<Rect>>,
    /// The still of the desktop to draw ([`crate::platform::desktop_shot::patch_still`]'s
    /// serial), first.
    pub still: Option<u64>,
    /// A still is on its way: a panel a still drew keeps its picture meanwhile.
    pub hold: bool,
    /// The file's picture may stand for the corner (no desktop icon under it): the fallback.
    pub file: bool,
}

/// How often the wallpaper is read again while the patch is wanted.
pub const READ_EVERY: Duration = Duration::from_secs(2);

/// How long after a Space or appearance change the wallpaper is read the second time, the
/// patch hidden (or, arriving on a Desktop, left as it slid in) until then.
pub const SETTLE: Duration = Duration::from_millis(300);

/// The longest an arriving Desktop's panel waits for the WM thread's first plan for that
/// Desktop ([`arrival_waits`]); then the latest plan applies.
pub const ARRIVAL_WAIT: Duration = Duration::from_secs(3);

/// The most Desktops' panels kept (one per Desktop visited with the patch shown).
pub const MAX_SHEETS: usize = 16;

/// Whether an arriving Desktop's panel still waits for a plan meant for it: the arrival was
/// noted when the latest plan applied carried the Space-change count `after`, and the WM
/// thread counts the change only after the main thread hears of it, so a plan for the new
/// Desktop carries a higher count. At most [`ARRIVAL_WAIT`].
fn arrival_waits(arrival: Option<(u64, Instant)>, plan_space: u64, now: Instant) -> bool {
    arrival.is_some_and(|(after, since)| plan_space <= after && now.saturating_duration_since(since) < ARRIVAL_WAIT)
}

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

/// Where the shown Desktop's panel was last put (global, top-left; its alpha hides it there),
/// for the WM thread's look at where macOS lists it during Show Desktop
/// ([`crate::engine::show_desktop::panel_place`]); `None` before it is ordered in.
pub fn placed_at() -> Option<Rect> {
    PLACED.lock().ok().and_then(|p| *p)
}

/// [`placed_at`]'s value, published with the panel's number.
static PLACED: Mutex<Option<Rect>> = Mutex::new(None);

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

/// What a Desktop's panel draws: the picture's facts, style and frame, to tell while away
/// whether that frame is still the one macOS shows ([`wp::still_drawn`]).
#[derive(Debug, Clone)]
struct Drawn {
    facts: wp::ImageFacts,
    style: Option<wp::Style>,
    index: usize,
    /// The picture but for its frame ([`Job::base_key`]) and the block rendered: near a solar
    /// frame change, whether its candidates look alike there ([`Settler::alike_for`]).
    base: String,
    region: Rect,
    /// The picture's file: its clock's model ([`Reader::solar_model`]).
    path: String,
}

/// What a Desktop's panel drawn from a still of the desktop shows: the still (kept to hold
/// while the next one comes), and what it was taken under, to tell while away whether it is
/// still right ([`crate::engine::patch_still::away_still_holds`]).
struct StillDrawn {
    serial: u64,
    /// The whole still, its block (global, top-left points) and scale.
    block: CFRetained<CGImage>,
    region: Rect,
    scale: f64,
    /// Finder's icons meeting the rectangle drawn, as known then.
    icons: Option<Vec<Rect>>,
    /// The appearance then.
    dark: bool,
    /// When the clock predicted the next frame then.
    frame_due: Option<Instant>,
}

/// The icons of `items` meeting `rect`.
fn icons_at(items: Option<&[Rect]>, rect: Rect) -> Option<Vec<Rect>> {
    items.map(|i| i.iter().copied().filter(|r| r.intersect(&rect).is_some()).collect())
}

/// What tells that the wallpaper read changed for a still of the desktop: the picture, how it
/// lands and its frame (or why it cannot be drawn), and the appearance.
fn wall_identity(w: &Wall) -> String {
    let what = match &w.outcome {
        Ok((_, job)) => job.key(),
        Err(why) => why.clone(),
    };
    format!("{:?}|{what}|{}", w.path, w.dark)
}

/// One Desktop's patch panel (main thread): ordered in once on its Desktop, then shown or
/// hidden there by its alpha.
struct Sheet {
    panel: Retained<NSPanel>,
    layer: Retained<CALayer>,
    /// Ordered in once: it lives on that Desktop from then on.
    placed: bool,
    /// Drawn (alpha 1) at this rectangle.
    shown: Option<Rect>,
    /// Where it was last put (global, top-left): hidden by its alpha, it stays there.
    frame: Option<Rect>,
    /// What it shows (picture key and the plan), to skip a redundant show.
    showing: Option<(String, PatchPlan)>,
    /// The rendered block its crops come from (its Desktop's picture).
    picture: Option<Picture>,
    /// [`ORDERED_AGAINST`]'s value for it.
    ordered: u64,
    /// When it was shown at its rectangle ([`shown_for`]).
    shown_at: Option<Instant>,
    /// What it draws, while drawn from the file.
    drawn: Option<Drawn>,
    /// What it draws, while drawn from a still of the desktop.
    still: Option<StillDrawn>,
}

impl Sheet {
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
        // One Desktop: the one it is first ordered in on (no Spaces flag).
        panel.setCollectionBehavior(panels::corner_patch());
        panel.setHidesOnDeactivate(false);
        panel.setAlphaValue(0.0);
        // SAFETY: the panel lives as long as its sheet, which closes it when dropped.
        unsafe { panel.setReleasedWhenClosed(false) };
        let layer = CALayer::new();
        if let Some(view) = panel.contentView() {
            view.setWantsLayer(true);
            view.setLayer(Some(&layer));
        }
        Self { panel, layer, placed: false, shown: None, frame: None, showing: None, picture: None, ordered: NOT_ORDERED, shown_at: None, drawn: None, still: None }
    }

    /// Ordered in and on the Desktop shown now.
    fn on_space_shown(&self) -> bool {
        self.placed && self.panel.isVisible() && self.panel.isOnActiveSpace()
    }

    /// Hidden where it is (its alpha), still ordered in on its Desktop. True when it showed.
    fn hide(&mut self) -> bool {
        self.showing = None;
        self.ordered = NOT_ORDERED;
        self.shown_at = None;
        self.drawn = None;
        self.still = None;
        if self.shown.take().is_some() {
            self.panel.setAlphaValue(0.0);
            return true;
        }
        false
    }

    /// Shows `image` at `rect`, ordered just above `above` (or in front), and orders the panel
    /// in on the Desktop shown when it never was. Only for the Desktop shown's sheet.
    fn show(&mut self, mtm: MainThreadMarker, rect: Rect, image: &CGImage, scale: f64, above: Option<u32>, reorder: u32) {
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
        self.frame = Some(rect);
        match above {
            Some(id) => {
                self.panel.orderWindow_relativeTo(NSWindowOrderingMode::Above, id as isize);
                self.ordered = (u64::from(id) << 32) | u64::from(reorder);
            }
            None => {
                self.panel.orderFrontRegardless();
                self.ordered = NOT_ORDERED;
            }
        }
        self.panel.setAlphaValue(1.0);
        self.placed = true;
        if self.shown != Some(rect) {
            self.shown_at = Some(Instant::now());
            tracing::debug!(%rect, ?above, "corner patch: shown");
        }
        self.shown = Some(rect);
    }
}

impl Drop for Sheet {
    fn drop(&mut self) {
        self.panel.orderOut(None);
        self.panel.close();
    }
}

/// The patch: the shown Desktop's panel, the others left on their Desktops, and what they
/// share (main thread).
struct Patch {
    /// The panel of the Desktop shown (one never ordered in until it shows).
    active: Sheet,
    /// Other Desktops' panels, each ordered in on its own Desktop as it was left.
    others: Vec<Sheet>,
    plan: PatchPlan,
    reader: Reader,
    wall: Option<(Instant, Wall)>,
    /// The render in flight and the key that failed.
    renders: RenderMemo,
    /// Nothing new shows before this (a Space or appearance change settling, [`SETTLE`]).
    settle_until: Option<Instant>,
    /// A Desktop arrived: the latest plan's Space-change count then, and when
    /// ([`arrival_waits`]).
    arrival: Option<(u64, Instant)>,
    /// What the log last said (a reason it is off, or what it draws).
    said: Option<String>,
    /// Near a solar frame change: the candidates' renders, verdicts and still.
    settler: Settler,
    /// The key of the job at the frame settled near a solar frame change (`None` while not).
    settled: Option<String>,
    /// A Desktop arrived (or the patch is new): one still checks a solar frame the clock
    /// names exactly ([`Settler::check`]).
    check_arrival: bool,
    /// The last wallpaper read's identity ([`wall_identity`]): a change takes a new still.
    identity: Option<String>,
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
        if !plan.on && !patch.others.is_empty() {
            tracing::debug!(closed = patch.others.len(), "corner patch: off; the other Desktops' panels close");
            patch.others.clear();
        }
        patch.plan = plan;
        if fresh {
            patch.wall = None;
        }
        patch.check_others(mtm);
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
        patch.check_others(mtm);
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

/// The user went to another Space (main thread, at the notification): the Desktop left
/// keeps its panel as it is, the arriving one's panel takes over as it slid in
/// ([`Patch::arrive`]); the wallpaper is read again now and once more [`SETTLE`] later,
/// nothing new shown before.
pub fn space_changed(mtm: MainThreadMarker) {
    // A switch's still of the desktop is of the old Desktop (8L3).
    crate::platform::desktop_shot::forget();
    let busy = PATCH.with(|p| {
        let Ok(mut p) = p.try_borrow_mut() else { return true };
        if let Some(patch) = p.as_mut() {
            patch.arrive(mtm, "a Space change");
        }
        false
    });
    if busy {
        // Busy (a call into AppKit that ran the main queue): once more shortly.
        again_shortly(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                space_changed(mtm);
            }
        });
    }
}

/// The appearance changed (main thread, at `AppleInterfaceThemeChangedNotification`): the
/// patch hides until the wallpaper is read again, the picture kept (a frame's key tells
/// whether it is still the one); other Desktops' panels whose frame changed hide.
pub fn appearance_changed(mtm: MainThreadMarker) {
    settle(mtm, "an appearance change");
}

/// Reads the wallpaper again at once and once more after [`SETTLE`] ([`settled`]).
fn settle_later() {
    let when = dispatch2::DispatchTime::try_from(SETTLE).unwrap_or(dispatch2::DispatchTime::NOW);
    let _ = dispatch2::DispatchQueue::main().after(when, settled);
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
            patch.settle_until = Some(until);
            patch.wall = None;
            patch.check_others(mtm);
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
    settle_later();
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
                // Drawn for the old arrangement: other Desktops' panels go.
                patch.others.clear();
                patch.settler.clear();
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
        Self {
            active: Sheet::new(mtm),
            others: Vec::new(),
            plan: PatchPlan::default(),
            reader: Reader::default(),
            wall: None,
            renders: RenderMemo::default(),
            settle_until: None,
            arrival: None,
            said: None,
            settler: Settler::default(),
            settled: None,
            check_arrival: true,
            identity: None,
        }
    }

    /// Publishes the active panel's number, order and showing time for the WM thread's
    /// order check and a switch's cover ([`window_number`], [`ordered_against`], [`shown_for`]).
    fn publish_active(&self) {
        let number = if self.active.placed { self.active.panel.windowNumber() as u32 } else { 0 };
        NUMBER.store(number, Ordering::SeqCst);
        ORDERED_AGAINST.store(self.active.ordered, Ordering::SeqCst);
        if let Ok(mut p) = PLACED.lock() {
            *p = if self.active.placed { self.active.frame } else { None };
        }
        SHOWN_AT.with(|s| s.set(self.active.shown_at));
    }

    fn hide(&mut self) {
        if self.active.hide() {
            tracing::debug!("corner patch: hidden");
        }
        self.publish_active();
    }

    /// Another Desktop shows (`why`): the panel of the Desktop left stays there as it is; the
    /// one already on the Desktop shown takes over as it slid in, else a new one (ordered in
    /// once it shows). Until a plan for this Desktop comes ([`arrival_waits`]) and the
    /// wallpaper is read a second time ([`SETTLE`]), the panel taking over is left as it is.
    fn arrive(&mut self, mtm: MainThreadMarker, why: &'static str) {
        let now = Instant::now();
        let left = self.active.placed && !self.active.on_space_shown();
        if let Some(i) = self.others.iter().position(Sheet::on_space_shown) {
            let next = self.others.swap_remove(i);
            let old = std::mem::replace(&mut self.active, next);
            if old.placed {
                self.others.push(old);
            }
            tracing::debug!(why, shown = ?self.active.shown, panels = self.others.len() + 1, "corner patch: the Desktop shown's panel takes over");
        } else if left {
            let old = std::mem::replace(&mut self.active, Sheet::new(mtm));
            self.others.push(old);
            tracing::debug!(why, panels = self.others.len() + 1, "corner patch: a Desktop with no panel yet");
        } else {
            tracing::debug!(why, shown = ?self.active.shown, "corner patch: the Desktop shown keeps its panel");
        }
        // Two on the Desktop shown (a Desktop removed in Mission Control hands its windows
        // to another): the active one stays.
        let before = self.others.len();
        self.others.retain(|s| !s.on_space_shown());
        if self.others.len() != before {
            tracing::info!(closed = before - self.others.len(), "corner patch: another panel on the Desktop shown; closed");
        }
        while self.others.len() > MAX_SHEETS - 1 {
            self.others.remove(0);
        }
        self.renders.drop_in_flight();
        self.check_arrival = true;
        // The first read names this Desktop's picture: no change to tell the still.
        self.identity = None;
        self.arrival = Some((self.plan.space, now));
        self.settle_until = Some(now + SETTLE);
        self.wall = None;
        self.publish_active();
        self.reconcile(mtm);
        settle_later();
    }

    /// Hides other Desktops' panels that would slide back in wrong: their frame is no longer
    /// the one macOS shows ([`wp::still_drawn`]), or a desktop icon lies under them (or the
    /// icons are not known).
    fn check_others(&mut self, mtm: MainThreadMarker) {
        if self.others.iter().all(|s| s.shown.is_none()) {
            return;
        }
        let dark = wallpaper::dark_appearance(mtm);
        let clock = self.reader.clock();
        let items = self.plan.items.clone();
        let (settler, reader) = (&self.settler, &self.reader);
        // Panels drawing the same picture, frame and block share one verdict (a solar one
        // weighs every hypothesis of the clock).
        let mut verdicts: Vec<(String, usize, Rect, bool)> = Vec::new();
        let now = Instant::now();
        for s in self.others.iter_mut() {
            let Some(rect) = s.shown else { continue };
            if let Some(d) = &s.still {
                // Drawn from a still: its Desktop as it was then, icons and all.
                let icons_now = icons_at(items.as_deref(), rect);
                let frame_due = d.frame_due.is_some_and(|t| now >= t);
                if !crate::engine::patch_still::away_still_holds(d.icons.as_deref(), icons_now.as_deref(), d.dark != dark, frame_due) {
                    s.hide();
                    tracing::debug!(frame_due, "corner patch: another Desktop's panel (a still) hidden while away");
                }
                continue;
            }
            let frame_right = s.drawn.as_ref().is_some_and(|d| {
                if let Some((.., v)) = verdicts.iter().find(|(b, i, r, _)| *b == d.base && *i == d.index && *r == d.region) {
                    return *v;
                }
                let v = match wp::still_drawn(&d.facts, d.style, dark, &clock, d.index, reader.solar_model(&d.path)) {
                    wp::StillShown::Yes => true,
                    wp::StillShown::No => false,
                    // Near a solar frame change: only while its candidates look alike there.
                    wp::StillShown::IfAlike(c) => settler.alike_for(&d.base, d.region, &c) == Some(true),
                };
                verdicts.push((d.base.clone(), d.index, d.region, v));
                v
            });
            let icons_clear = items.as_ref().is_some_and(|i| !i.iter().any(|r| r.intersect(&rect).is_some()));
            if !(frame_right && icons_clear) {
                s.hide();
                tracing::debug!(frame_right, icons_clear, "corner patch: another Desktop's panel hidden while away");
            }
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
        self.publish_active();
        self.publish_cover();
    }

    /// The picture the patch would show now, for a switch's cover ([`CoverSource`]): near a
    /// solar frame change, only the frame settled.
    fn publish_cover(&self) {
        let wall = self.wall.as_ref().and_then(|(_, w)| {
            let (d, job) = w.outcome.as_ref().ok()?;
            let key = if d.candidates.is_empty() { Some(job.key()) } else { self.settled.clone() };
            key.map(|k| (w.screen, k))
        });
        let picture = wall.and_then(|(screen, key)| self.active.picture.as_ref().filter(|p| p.key == key).map(|p| (screen, p)));
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
        let now = Instant::now();
        if arrival_waits(self.arrival, self.plan.space, now) {
            // The plan is the Desktop left's: the arriving panel stays as it slid in.
            return;
        }
        self.arrival = None;
        let Some(rect) = self.plan.show else {
            self.hide();
            self.prepare(mtm);
            return;
        };
        let settling = self.settle_until.is_some_and(|t| now < t);
        if settling && self.active.shown.is_some() {
            // Arrived drawn as it was left: changed only once the second read confirms.
            return;
        }
        if self.wall.is_none() {
            let wall = self.read_wall(mtm);
            self.wall = Some((Instant::now(), wall));
        }
        // A still of the desktop first; while one comes, what a still drew stays.
        if self.plan.still.is_some_and(|serial| self.draw_still(mtm, rect, serial)) {
            return;
        }
        if (self.plan.hold || self.plan.still.is_some()) && self.hold_still(mtm, rect) {
            return;
        }
        if !self.plan.file {
            // A desktop icon under the corner (or the icons not known): only a still may
            // stand for it.
            self.hide();
            return;
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
        let job = match self.settle_frame(&decision, job, wall.screen, rect, settling) {
            Ok(job) => job,
            Err(why) => {
                self.hide();
                if let Some(why) = why {
                    let name = wall.path.as_deref().map(wallpaper::file_name).unwrap_or_default();
                    self.say(format!("{why} ({name})"), true);
                }
                return;
            }
        };
        let key = job.key();
        if self.active.showing.as_ref().is_some_and(|(k, plan)| *k == key && *plan == self.plan) {
            return;
        }
        if self.active.picture.as_ref().is_some_and(|p| p.key != key) {
            // Another picture or frame: never show the old one meanwhile.
            self.active.picture = None;
            self.hide();
        }
        let shown = self.active.picture.as_ref().and_then(|p| crop_for(&p.image, p.region, p.scale, wall.screen, rect));
        let Some(image) = shown else {
            self.hide();
            let region = wp::region(Size::new(wall.screen.w, wall.screen.h), wp::local(rect, wall.screen));
            if self.renders.want(&key, region) == Want::Start {
                start_render(job, wall.screen, rect, region);
            }
            return;
        };
        if settling {
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
                job.index,
                decision.placement.name(),
                job.scale,
                job.space_key
            ),
            false,
        );
        let was_placed = self.active.placed;
        self.active.show(mtm, rect, &image, job.scale, self.plan.above, self.plan.reorder);
        self.active.showing = Some((key, self.plan.clone()));
        let region = self.active.picture.as_ref().map_or_else(Rect::default, |p| p.region);
        let base = job.base_key();
        let path = job.path.clone();
        self.active.drawn = wall.image.clone().map(|facts| Drawn { facts, style: decision.style, index: job.index, base, region, path });
        self.active.still = None;
        self.went_elsewhere(mtm, was_placed);
    }

    /// A panel ordered in for the first time while a Desktop still slid went to another
    /// Desktop: hidden there, it waits as that Desktop's; a new one for this Desktop.
    fn went_elsewhere(&mut self, mtm: MainThreadMarker, was_placed: bool) {
        if !was_placed && !self.active.panel.isOnActiveSpace() {
            tracing::debug!("corner patch: a new panel went to another Desktop; hidden there, a new one for this one");
            self.active.hide();
            let away = std::mem::replace(&mut self.active, Sheet::new(mtm));
            self.others.push(away);
        }
    }

    /// Draws the still of the desktop `serial` at `rect`; false when it is not the one kept
    /// (a newer one came, or it was dropped) or does not hold the rectangle.
    fn draw_still(&mut self, mtm: MainThreadMarker, rect: Rect, serial: u64) -> bool {
        let key = format!("still:{serial}");
        if self.active.showing.as_ref().is_some_and(|(k, plan)| *k == key && *plan == self.plan) {
            return true;
        }
        let Some((block, region, scale)) = desktop_shot::patch_still(serial) else { return false };
        let Some(image) = desktop_shot::crop(&block, region, scale, rect) else { return false };
        self.say("drawing a still of the desktop (Screen Recording: the wallpaper, icons and widgets as shown)".into(), false);
        let (dark, frame_due) = self.wall.as_ref().map_or((false, None), |(_, w)| (w.dark, w.frame_due));
        let icons = icons_at(self.plan.items.as_deref(), rect);
        let was_placed = self.active.placed;
        self.active.show(mtm, rect, &image, scale, self.plan.above, self.plan.reorder);
        self.active.showing = Some((key, self.plan.clone()));
        self.active.drawn = None;
        self.active.still = Some(StillDrawn { serial, block, region, scale, icons, dark, frame_due });
        self.went_elsewhere(mtm, was_placed);
        true
    }

    /// While a new still comes, a panel a still drew stays drawn (at the plan's rectangle and
    /// order, cropped anew from the still it holds); false when it shows none or the still
    /// does not hold the rectangle.
    fn hold_still(&mut self, mtm: MainThreadMarker, rect: Rect) -> bool {
        if self.active.shown.is_none() {
            return false;
        }
        let Some(d) = &self.active.still else { return false };
        let key = format!("hold:{}", d.serial);
        if self.active.showing.as_ref().is_some_and(|(k, plan)| *k == key && *plan == self.plan) {
            return true;
        }
        let Some(image) = desktop_shot::crop(&d.block, d.region, d.scale, rect) else { return false };
        let scale = d.scale;
        self.active.show(mtm, rect, &image, scale, self.plan.above, self.plan.reorder);
        self.active.showing = Some((key, self.plan.clone()));
        true
    }

    /// Reads the wallpaper (main thread) and tells the desktop's still what changed: another
    /// picture, frame or look since the last read takes a new one once the fade is over, and
    /// the next frame the clock predicts takes one then.
    fn read_wall(&mut self, mtm: MainThreadMarker) -> Wall {
        let wall = self.reader.read(mtm);
        let identity = wall_identity(&wall);
        if self.identity.as_ref().is_some_and(|was| *was != identity) {
            tracing::debug!("corner patch: the wallpaper read changed; the desktop's still is taken again");
            desktop_shot::wallpaper_changed(Instant::now());
        }
        self.identity = Some(identity);
        desktop_shot::set_frame_due(wall.frame_due);
        wall
    }

    /// Hidden, keeps the picture rendered for the plan's `picture` rectangle (round 8L4: a
    /// switch's cover lays the desktop's still of the icons over it); nothing is shown.
    fn prepare(&mut self, mtm: MainThreadMarker) {
        let Some(rect) = self.plan.picture else { return };
        if self.wall.is_none() {
            let wall = self.read_wall(mtm);
            self.wall = Some((Instant::now(), wall));
        }
        let Some((_, wall)) = self.wall.clone() else { return };
        let Ok((decision, job)) = &wall.outcome else { return };
        let settling = self.settle_until.is_some_and(|t| Instant::now() < t);
        let Ok(job) = self.settle_frame(decision, job.clone(), wall.screen, rect, settling) else { return };
        let key = job.key();
        if self.active.picture.as_ref().is_some_and(|p| p.key != key) {
            self.active.picture = None;
        }
        let local = wp::local(rect, wall.screen);
        if self.active.picture.as_ref().is_some_and(|p| wp::crop_px(p.region, local, p.scale).is_some()) {
            return;
        }
        let region = wp::region(Size::new(wall.screen.w, wall.screen.h), local);
        if self.renders.want(&key, region) == Want::Start {
            start_render(job, wall.screen, rect, region);
        }
    }

    /// The job to draw for `decision`: `job` itself when its frame is known (a solar one
    /// checked by one still on arrival, [`Settler::check`]); near a solar frame change
    /// ([`wp::Decision::candidates`]) the job at the frame settled ([`Settler::settle`]).
    /// `Err(Some(why))` when off, `Err(None)` while the frames render.
    fn settle_frame(&mut self, decision: &wp::Decision, job: Job, screen: Rect, rect: Rect, settling: bool) -> Result<Job, Option<String>> {
        self.settled = None;
        let Some(sun) = &decision.sun else { return Ok(job) };
        let region = wp::region(Size::new(screen.w, screen.h), wp::local(rect, screen));
        let now = Instant::now();
        if decision.candidates.is_empty() {
            if self.check_arrival && self.settler.check(&job, &sun.frames, screen, region, self.may_take_still(settling), now) == Check::Done {
                self.check_arrival = false;
            }
            return Ok(job);
        }
        let may_take = self.may_take_still(settling);
        match self.settler.settle(&job, &decision.candidates, &sun.frames, sun.surviving > 0, screen, region, may_take, now) {
            Outcome::Draw(job) => {
                self.settled = Some(job.key());
                Ok(*job)
            }
            Outcome::Wait => Err(None),
            Outcome::Off(why) => Err(Some(why)),
        }
    }

    /// A still that saw clearly ([`Settler::news`]): the clock's model for its picture
    /// narrowed, logged by the count of hypotheses standing (never a place), and the frame
    /// decided again.
    fn narrow(&mut self, seen: super::corner_settle::Observation) {
        let Some(n) = self.reader.observe(&seen.path, seen.unix, &seen.seen) else { return };
        let all = wp::SolarModel::all();
        let name = wallpaper::file_name(&seen.path);
        if n.after == 0 {
            if n.before > 0 {
                tracing::info!("corner patch: {} ({name}; the still saw frame {})", wp::CLOCK_MISFIT, super::corner_settle::frames(&seen.seen));
            }
        } else if n.after < n.before {
            tracing::info!("corner patch: a still of the desktop narrowed the clock ({name}): {} of {all} hypotheses stand", n.after);
        } else {
            tracing::debug!("corner patch: a still of the desktop fits the clock ({name}): {} of {all} hypotheses stand", n.after);
        }
        self.wall = None;
    }

    /// Whether a still of the desktop may settle a solar frame now: no Space or appearance
    /// change settling, the capture gate open ([`crate::platform::capture::desktop_may_capture`],
    /// the rules of every desktop still), the screen unlocked and awake, Finder's icons known
    /// (they are masked out of the comparison).
    fn may_take_still(&self, settling: bool) -> Result<(), &'static str> {
        if settling {
            return Err("a Space or appearance change settles");
        }
        crate::platform::capture::desktop_may_capture()?;
        if crate::platform::session::screen_locked() {
            return Err("the screen is locked");
        }
        if objc2_core_graphics::CGDisplayIsAsleep(objc2_core_graphics::CGMainDisplayID()) {
            return Err("the display sleeps");
        }
        if self.plan.items.is_none() {
            return Err("Finder's desktop icons are not known");
        }
        Ok(())
    }

    /// A render of `region` finished (main thread): keep it when it is still the one wanted.
    fn rendered(&mut self, mtm: MainThreadMarker, key: String, region: Rect, scale: f64, result: Result<(Rect, CFRetained<CGImage>), String>) {
        if !self.renders.finished(&key, region, result.is_ok()) {
            return;
        }
        match result {
            Ok((region, image)) => {
                self.active.picture = Some(Picture { key, region, scale, image });
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

/// Hands what came back for settling a solar frame ([`News`]) to the patch (main thread);
/// while the patch is busy, once more [`BUSY_RETRY`] later.
pub(super) fn settle_news(news: News) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    PATCH.with(|p| match p.try_borrow_mut() {
        Ok(mut p) => {
            if let Some(patch) = p.as_mut() {
                let items = patch.plan.items.clone();
                if let Some(seen) = patch.settler.news(news, items.as_deref()) {
                    patch.narrow(seen);
                }
                patch.reconcile(mtm);
            }
        }
        Err(_) => again_shortly(move || settle_news(news)),
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
    fn an_arriving_panel_waits_only_for_a_plan_meant_for_its_desktop() {
        let t0 = Instant::now();
        // Arrived when the latest plan counted 7 Space changes.
        let arrival = Some((7, t0));
        assert!(arrival_waits(arrival, 7, t0), "the Desktop left's plan, sent before the WM thread counted the change");
        assert!(arrival_waits(arrival, 6, t0 + Duration::from_millis(100)), "an older one in flight");
        assert!(!arrival_waits(arrival, 8, t0), "the WM thread's first plan for the new Desktop");
        assert!(!arrival_waits(arrival, 7, t0 + ARRIVAL_WAIT), "never longer");
        assert!(!arrival_waits(None, 7, t0));
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
