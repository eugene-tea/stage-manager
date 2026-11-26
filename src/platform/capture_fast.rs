//! Window pictures straight from the window server (1.9, S6b): one call a window, no listing of
//! the shareable content first.
//!
//! ScreenCaptureKit lists every window of the system before each capture round (an
//! `SCRunningApplication` per window, LaunchServices lookups): ~65 % of the app's CPU at rest
//! with the timers of 1.8 (S6a, 11-14), 79-96 ms and ~45 ms of CPU across processes for one
//! window. The window server's own capture takes a window in 12-14 ms (~3 ms of CPU, mostly
//! replayd) and lists nothing (S4c, `probe-routes capture`). Two calls, by where the window is:
//!
//! - [`Route::Fast`]: `SLSHWCaptureWindowList(cid, &wid, 1, options)`, a +1 array of one
//!   `CGImage` (WebKit `CoreGraphicsSPI.h`, alt-tab-macos `SkyLight.framework.swift`, DockDoor
//!   `PrivateApis.swift`). A window on screen and one hidden in a stage Space not shown
//!   (current content, measured 11-14). Only the part on screen of a window partly off every
//!   display (the Corner engine's parked windows: 4 x 64 px).
//! - [`Route::Whole`]: `CGWindowListCreateImage(null rect, including window, wid, options)`
//!   with framing ignored at the best resolution, public until macOS 15's SDK and still
//!   exported (WebKit soft-links it, bug 277564): the whole window wherever it is, a parked
//!   one too, 10-11 ms.
//!
//! Each picture is the window alone, without its shadow or child windows (a popover is not in
//! it, where ScreenCaptureKit's picture carries it), at the window's own pixels, sized down
//! here when a smaller one was asked for (a card's thumbnail). The caller falls back to
//! ScreenCaptureKit ([`super::capture`]) for any window this source misses: the symbol missing,
//! the call handing back nothing, or a picture that cannot be the window (empty, the wrong
//! shape, or see-through all over). A route whose pictures of [`BLANK_STRIKES`] different
//! windows come back blank before any is the window rests for [`OFF_FOR`], then is tried again
//! (logged once); once one picture was the window, it is never turned off in that run.
//! `STAGE_MANAGER_CAPTURE_SCK` (honoured only with
//! `STAGE_MANAGER_ONLY`, a test run) turns this source off: every picture through
//! ScreenCaptureKit, as before 1.9.
//!
//! **Several windows in one call** ([`pictures`], round picture-wait). `SLSHWCaptureWindowList`
//! takes a list of windows and hands back an image for each. A switch's pictures are taken
//! one call a window, one after another; with [`LIST_ENV`] set in a test run the windows on
//! screen go in one call. Which is faster is to be measured (`probe-spaces capture BUNDLE
//! fast list`); until then the list is a test switch, and whatever it does not answer for
//! (not one image a window, or a picture that is not its window) is taken as before.
//!
//! The calls are synchronous: callers run them off the main thread (the capture workers, a
//! switch's own thread). Each counts as a capture in flight for [`super::capture::quiesce`].
//! Every route needs Screen Recording; callers check it first (without it the window server's
//! call makes macOS ask).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_foundation::NSArray;

use super::capture::{InFlight, Picture, Target};
use super::capture_frame::{self, Extent};
use super::capture_pixels::{self, PixelRect};
use super::spaces::ffi::{self, SKYLIGHT};
use crate::engine::geometry::Rect;

/// Set (with `STAGE_MANAGER_ONLY`): every picture through ScreenCaptureKit.
pub const FORCE_SCK_ENV: &str = "STAGE_MANAGER_CAPTURE_SCK";

/// Set (with `STAGE_MANAGER_ONLY`): a switch's windows on screen are taken in one call of
/// `SLSHWCaptureWindowList` with all their ids ([`pictures`]) instead of one call a window.
/// A test switch until both ways are measured (`probe-spaces capture BUNDLE fast list`).
pub const LIST_ENV: &str = "STAGE_MANAGER_CAPTURE_LIST";

/// `SLSHWCaptureWindowList` options: best resolution (1 << 8) and the window's whole shape,
/// not its part left by the windows over it (ignore the global clip shape, 1 << 11), as alt-tab
/// passes them.
const HW_OPTIONS: u32 = (1 << 8) | (1 << 11);
/// `kCGWindowListOptionIncludingWindow`.
const LIST_INCLUDING_WINDOW: u32 = 1 << 3;
/// `kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution`: no shadow, own pixels.
const IMAGE_OPTIONS: u32 = (1 << 0) | (1 << 3);

/// `SLSMainConnectionID()`.
type FnMain = unsafe extern "C" fn() -> i32;
/// `SLSHWCaptureWindowList(cid, wids, count, options)`: a +1 CFArray of CGImage, or null.
type FnHw = unsafe extern "C" fn(i32, *const u32, u32, u32) -> *mut c_void;
/// `CGWindowListCreateImage(rect, list option, wid, image option)`: a +1 CGImage, or null.
type FnListImage = unsafe extern "C" fn(CGRect, u32, u32, u32) -> *mut c_void;

/// Which call takes a window's picture (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// `SLSHWCaptureWindowList`: on screen, or hidden in a stage Space.
    Fast,
    /// `CGWindowListCreateImage`: the whole window, partly off every display (parked).
    Whole,
}

impl Route {
    fn as_str(self) -> &'static str {
        match self {
            Route::Fast => "window server (SLSHWCaptureWindowList)",
            Route::Whole => "window list image (CGWindowListCreateImage)",
        }
    }

    fn health(self) -> MutexGuard<'static, Health> {
        let m = match self {
            Route::Fast => &FAST_HEALTH,
            Route::Whole => &WHOLE_HEALTH,
        };
        m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Blank pictures of this many different windows rest a route that has not brought the window
/// yet (the review's D-3: one blank first picture, a window still being
/// restored at login, turned the route off until the next launch).
pub const BLANK_STRIKES: usize = 3;
/// How long a route rests after [`BLANK_STRIKES`] before it is tried again.
pub const OFF_FOR: Duration = Duration::from_secs(30 * 60);

/// What this run knows of a route: whether a picture was the window yet, the windows whose
/// pictures came back blank since, and a rest.
#[derive(Debug, Default)]
pub struct Health {
    works: bool,
    /// Different windows whose pictures came back blank (none was the window yet).
    blanks: Vec<u32>,
    /// Resting until then.
    off_until: Option<Instant>,
}

impl Health {
    /// True unless the route rests at `now`; a rest over starts the count afresh.
    pub fn usable(&mut self, now: Instant) -> bool {
        match self.off_until {
            Some(until) if now < until => false,
            Some(_) => {
                self.off_until = None;
                self.blanks.clear();
                true
            }
            None => true,
        }
    }

    /// Window `id`'s picture came back blank: true when this rests the route (from `now`).
    pub fn blank(&mut self, id: u32, now: Instant) -> bool {
        if self.works {
            return false;
        }
        if !self.blanks.contains(&id) {
            self.blanks.push(id);
        }
        if self.blanks.len() < BLANK_STRIKES {
            return false;
        }
        self.blanks.clear();
        self.off_until = Some(now + OFF_FOR);
        true
    }

    /// A picture was the window: true for the route's first in this run.
    pub fn good(&mut self) -> bool {
        !std::mem::replace(&mut self.works, true)
    }

    fn word(&self) -> &'static str {
        match (self.works, self.off_until) {
            (true, _) => "works",
            (false, Some(_)) => "off",
            (false, None) => "untried",
        }
    }
}

static FAST_HEALTH: Mutex<Health> = Mutex::new(Health { works: false, blanks: Vec::new(), off_until: None });
static WHOLE_HEALTH: Mutex<Health> = Mutex::new(Health { works: false, blanks: Vec::new(), off_until: None });
/// A route's rest was logged at info once; later ones at debug.
static REST_SAID: AtomicBool = AtomicBool::new(false);

/// The calls, loaded once; each `None` when this macOS does not export it.
struct Calls {
    cid: i32,
    hw: Option<FnHw>,
    list_image: Option<FnListImage>,
}

static CALLS: OnceLock<Option<Calls>> = OnceLock::new();

fn calls() -> Option<&'static Calls> {
    CALLS
        .get_or_init(|| {
            // SAFETY: a system framework path; RTLD_NOW resolves everything at load.
            let h = unsafe { libc::dlopen(SKYLIGHT.as_ptr(), libc::RTLD_NOW) };
            if h.is_null() {
                tracing::info!("window pictures: SkyLight did not load; ScreenCaptureKit takes every picture");
                return None;
            }
            let mut names = Vec::new();
            // SAFETY (each `load`): the types follow the public signatures in the module docs.
            let main: Option<FnMain> = unsafe { ffi::load(h, c"SLSMainConnectionID", &mut names) };
            let hw: Option<FnHw> = unsafe { ffi::load(h, c"SLSHWCaptureWindowList", &mut names) };
            let list_image: Option<FnListImage> = unsafe { ffi::load(libc::RTLD_DEFAULT, c"CGWindowListCreateImage", &mut names) };
            // SAFETY: no arguments; this process's connection id.
            let cid = main.map(|m| unsafe { m() });
            let missing: Vec<&str> = names.iter().filter(|(_, found)| !found).map(|(n, _)| *n).collect();
            if !missing.is_empty() {
                tracing::info!(?missing, "window pictures: calls missing on this macOS; ScreenCaptureKit takes those pictures");
            }
            cid.map(|cid| Calls { cid, hw, list_image })
        })
        .as_ref()
}

/// True when a test run asked for ScreenCaptureKit alone ([`FORCE_SCK_ENV`] with
/// `STAGE_MANAGER_ONLY`).
fn forced_off() -> bool {
    static FORCED: LazyLock<bool> = LazyLock::new(|| {
        let forced = forced_by(std::env::var_os(FORCE_SCK_ENV).is_some(), std::env::var_os("STAGE_MANAGER_ONLY").is_some());
        if forced {
            tracing::info!("window pictures: {FORCE_SCK_ENV} set (a test run): ScreenCaptureKit takes every picture");
        }
        forced
    });
    *FORCED
}

/// The test switch counts only in a test run (`only`: `STAGE_MANAGER_ONLY` set).
fn forced_by(asked: bool, only: bool) -> bool {
    asked && only
}

/// True when a test run asked for one call for several windows ([`LIST_ENV`] with
/// `STAGE_MANAGER_ONLY`).
pub fn list_capture() -> bool {
    static LIST: LazyLock<bool> = LazyLock::new(|| {
        let on = forced_by(std::env::var_os(LIST_ENV).is_some_and(|v| !v.is_empty()), std::env::var_os("STAGE_MANAGER_ONLY").is_some());
        if on {
            tracing::info!("window pictures: {LIST_ENV} set (a test run): a switch's windows on screen are taken in one call");
        }
        on
    });
    *LIST
}

/// True when this source may take pictures at all in this run (not forced off, the calls
/// loaded, a route not resting).
pub fn enabled() -> bool {
    !forced_off() && calls().is_some_and(|c| (c.hw.is_some() && usable(Route::Fast)) || (c.list_image.is_some() && usable(Route::Whole)))
}

fn usable(route: Route) -> bool {
    route.health().usable(Instant::now())
}

/// What this source says of itself, for the ctl dump.
pub fn status() -> serde_json::Value {
    let word = |r: Route| r.health().word();
    serde_json::json!({
        "forced_off": forced_off(),
        "list": list_capture(),
        "fast": calls().and_then(|c| c.hw).map(|_| word(Route::Fast)).unwrap_or("missing"),
        "whole": calls().and_then(|c| c.list_image).map(|_| word(Route::Whole)).unwrap_or("missing"),
    })
}

/// Why a window has no picture from this source: the caller takes it through ScreenCaptureKit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Miss {
    /// The source is off, or the call for this window's route is missing or resting.
    Unavailable,
    /// The call handed back nothing.
    Nothing,
    /// A picture that cannot be the window (empty, the wrong shape, see-through all over).
    NotTheWindow,
}

impl Miss {
    /// For the log.
    pub fn as_str(self) -> &'static str {
        match self {
            Miss::Unavailable => "unavailable",
            Miss::Nothing => "nothing handed back",
            Miss::NotTheWindow => "a picture that is not the window",
        }
    }
}

/// The route for a window at `frame` (top-left points) with these `displays`: [`Route::Fast`]
/// while the window lies within the displays (every corner, a point in, on one of them, or no
/// display known), [`Route::Whole`] when part of it is off every display (a parked window, or
/// one dragged past a screen's edge), where the fast call sees only the part on screen.
pub fn route_for(frame: Rect, displays: &[Rect]) -> Route {
    if displays.is_empty() {
        return Route::Fast;
    }
    let inset = 1.0_f64.min(frame.w / 2.0).min(frame.h / 2.0);
    let (l, t, r, b) = (frame.x + inset, frame.y + inset, frame.x + frame.w - inset, frame.y + frame.h - inset);
    let on_one = |x: f64, y: f64| displays.iter().any(|d| x >= d.x && x <= d.x + d.w && y >= d.y && y <= d.y + d.h);
    if [(l, t), (r, t), (l, b), (r, b)].iter().all(|&(x, y)| on_one(x, y)) { Route::Fast } else { Route::Whole }
}

/// True when a `w` x `h` px picture can be the whole window at `frame` (points): not empty, at
/// least one pixel a point (a display at 1x), and the window's shape to a tenth (the part on
/// screen of a window partly off it is narrower or shorter).
pub fn fits_window(w: usize, h: usize, frame: Rect) -> bool {
    if w == 0 || h == 0 || frame.w < 1.0 || frame.h < 1.0 {
        return w > 0 && h > 0;
    }
    let (w, h) = (w as f64, h as f64);
    let big_enough = w >= frame.w * 0.9 && h >= frame.h * 0.9;
    let shape = (w / h) / (frame.w / frame.h);
    big_enough && (0.9..=1.1).contains(&shape)
}

/// True when every box read of a picture is see-through (alpha 2 or less): nothing of the
/// window is in it. A box not read tells nothing.
pub fn see_through(alphas: &[Option<u8>]) -> bool {
    let read: Vec<u8> = alphas.iter().flatten().copied().collect();
    !read.is_empty() && read.iter().all(|a| *a <= 2)
}

/// Window `t.id`'s picture now, at `frame` (top-left points, as the window server describes the
/// window) with these `displays`, sized to `t`'s pixels at most (the window's proportions kept).
pub fn picture(t: Target, frame: Rect, displays: &[Rect]) -> Result<Picture, Miss> {
    if forced_off() {
        return Err(Miss::Unavailable);
    }
    let calls = calls().ok_or(Miss::Unavailable)?;
    let route = route_for(frame, displays);
    if !usable(route) {
        return Err(Miss::Unavailable);
    }
    let image = match route {
        Route::Fast => {
            let f = calls.hw.ok_or(Miss::Unavailable)?;
            let flight = InFlight::issue();
            // SAFETY: one window id that outlives the call; a +1 CFArray of CGImage or null.
            let list: Option<Retained<NSArray>> = unsafe { ffi::take_array(f(calls.cid, &t.id, 1, HW_OPTIONS)) };
            drop(flight);
            list.and_then(|l| first_image(&l))
        }
        Route::Whole => {
            let f = calls.list_image.ok_or(Miss::Unavailable)?;
            let null = CGRect::new(CGPoint::new(f64::INFINITY, f64::INFINITY), CGSize::new(0.0, 0.0));
            let flight = InFlight::issue();
            // SAFETY: the public signature (CGWindow.h); a +1 CGImage or null.
            let p = unsafe { f(null, LIST_INCLUDING_WINDOW, t.id, IMAGE_OPTIONS) };
            drop(flight);
            // SAFETY: a +1 CGImage (or null), owned from here.
            std::ptr::NonNull::new(p.cast::<CGImage>()).map(|p| unsafe { CFRetained::from_raw(p) })
        }
    };
    let Some(image) = image else {
        tracing::trace!(window = t.id, route = route.as_str(), "window picture: nothing handed back");
        return Err(Miss::Nothing);
    };
    judge(route, t, frame, image)
}

/// The pictures of several windows from one call: `SLSHWCaptureWindowList` with every id of
/// `windows` (each with its frame as the window server describes it now), which hands back
/// one image a window, in the order asked (round picture-wait; to be measured against one
/// call a window, [`LIST_ENV`]). Only windows of [`Route::Fast`] may be asked for. Each
/// window's picture or why it has none, in `windows`' order; `None` when the call cannot
/// stand for them all (the source off, the call missing or resting, nothing handed back, or
/// not one image a window): the caller takes them one by one.
pub fn pictures(windows: &[(Target, Rect)], displays: &[Rect]) -> Option<Vec<Result<Picture, Miss>>> {
    if windows.is_empty() || forced_off() || !usable(Route::Fast) {
        return None;
    }
    if windows.iter().any(|(_, frame)| route_for(*frame, displays) != Route::Fast) {
        return None;
    }
    let calls = calls()?;
    let f = calls.hw?;
    let ids: Vec<u32> = windows.iter().map(|(t, _)| t.id).collect();
    let flight = InFlight::issue();
    // SAFETY: `ids` outlives the call, with its own count; a +1 CFArray of CGImage or null.
    let list: Option<Retained<NSArray>> = unsafe { ffi::take_array(f(calls.cid, ids.as_ptr(), ids.len() as u32, HW_OPTIONS)) };
    drop(flight);
    let list = list?;
    let images = images_of(&list);
    if !one_each(images.len(), windows.len()) {
        tracing::debug!(asked = windows.len(), images = images.len(), "window pictures: the list capture did not hand back one image a window; they are taken one by one");
        return None;
    }
    Some(windows.iter().zip(images).map(|((t, frame), image)| judge(Route::Fast, *t, *frame, image)).collect())
}

/// True when a list capture's `images` can be matched to the `asked` windows: one each.
pub fn one_each(images: usize, asked: usize) -> bool {
    asked > 0 && images == asked
}

/// `image`, handed back by `route` for window `t.id` at `frame`, as its picture: checked to
/// be the window (not empty, its shape, not see-through all over: [`fits_window`],
/// [`see_through`]), the route's health noted, sized to `t`'s pixels at most.
fn judge(route: Route, t: Target, frame: Rect, image: CFRetained<CGImage>) -> Result<Picture, Miss> {
    let (w, h) = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
    let blank = w == 0 || h == 0 || see_through(&alphas(&image, w, h));
    if blank || !fits_window(w, h, frame) {
        // Blank pictures of a few windows before any was the window: the route rests.
        if blank && route.health().blank(t.id, Instant::now()) {
            let strikes = BLANK_STRIKES;
            let rest_min = OFF_FOR.as_secs() / 60;
            if REST_SAID.swap(true, Ordering::Relaxed) {
                tracing::debug!(route = route.as_str(), strikes, rest_min, "window pictures: this call's pictures still come back blank; ScreenCaptureKit takes them for a while");
            } else {
                tracing::info!(route = route.as_str(), w, h, strikes, rest_min, "window pictures: this call's pictures of a few windows came back blank; ScreenCaptureKit takes them for a while, then the call is tried again");
            }
        } else {
            tracing::debug!(window = t.id, route = route.as_str(), w, h, frame = ?frame, blank, "window picture: not the window; ScreenCaptureKit takes it");
        }
        return Err(Miss::NotTheWindow);
    }
    if route.health().good() {
        tracing::info!(route = route.as_str(), w, h, "window pictures: taken straight from the window server, no listing");
    }
    let (fw, fh) = capture_frame::fit_to_content((t.width, t.height), (w as f64, h as f64));
    let image = if needs_scaling((w, h), (fw, fh)) { super::pixels::scaled(&image, fw as usize, fh as usize).unwrap_or(image) } else { image };
    Ok(Picture { image, extent: Extent::NONE })
}

/// Of `ids`, the windows the window server no longer describes (closed), given those it
/// described (`described`). `None` when it described none of them: a failed read (its call
/// answers nothing under load), not every window closed; the caller leaves them all to
/// ScreenCaptureKit, which tells a closed window from one it cannot take (the review's D-7: the
/// whole batch was taken as closed, no picture, no retry).
pub fn closed(ids: &[u32], described: &[u32]) -> Option<Vec<u32>> {
    if !ids.is_empty() && described.is_empty() {
        return None;
    }
    Some(ids.iter().copied().filter(|id| !described.contains(id)).collect())
}

/// Of `ids`, the windows with another window of their app on screen over them or reaching out
/// of them (a popover, a menu, a sheet, a child panel: ScreenCaptureKit's picture carries those,
/// the window server's call gives the window alone): ScreenCaptureKit takes their pictures, as
/// before (live 11-14: a card lost its popup). One query of the windows on screen; `displays`
/// as for [`route_for`].
pub fn with_children(ids: &[u32], displays: &[Rect]) -> std::collections::HashSet<u32> {
    if ids.is_empty() {
        return Default::default();
    }
    let on = super::windows::on_screen();
    ids.iter().copied().filter(|id| on.iter().find(|w| w.id == *id).is_some_and(|w| has_child_over(w.id, w.pid, w.bounds, &on, displays))).collect()
}

/// An overlap thinner than this (points) on a side is no child over the window: the sliver of a
/// parked window left on screen by macOS (a point or two at the park corner).
const SLIVER: f64 = 4.0;

/// True when a window of `pid` other than `id`, on screen and not see-through, overlaps
/// `frame` (`on`: the windows on screen) by more than a [`SLIVER`]. A same-app window merely
/// overlapping counts too: the slower picture is right either way. A window partly off every
/// display ([`Route::Whole`]: parked by the Corner engine, which lists it on screen at its full
/// frame in the corner, or pushed past a screen's edge) is no child: a popover, a menu or a
/// sheet lies on the displays (the review's D-2: under the Corner engine an app's parked
/// windows counted as each other's children, and a maximized window as a parked one's, through
/// their corner, so the active stage went through ScreenCaptureKit every 2 s at rest).
pub fn has_child_over(id: u32, pid: i32, frame: Rect, on: &[super::windows::CgWindow], displays: &[Rect]) -> bool {
    on.iter().any(|w| {
        w.id != id
            && w.pid == pid
            && w.alpha > 0.0
            && route_for(w.bounds, displays) == Route::Fast
            && w.bounds.intersect(&frame).is_some_and(|i| i.w >= SLIVER && i.h >= SLIVER)
    })
}

/// True when a picture of `have` pixels is worth drawing smaller to `want` (a card's thumbnail
/// of a large window): a tenth smaller or more on a side.
pub fn needs_scaling(have: (usize, usize), want: (u32, u32)) -> bool {
    (want.0 as f64) < have.0 as f64 * 0.9 || (want.1 as f64) < have.1 as f64 * 0.9
}

/// The four corner boxes' and the middle's mean alpha of a `w` x `h` picture (each box drawn on
/// its own: only their pixels are read).
fn alphas(image: &CGImage, w: usize, h: usize) -> Vec<Option<u8>> {
    let mut boxes = capture_pixels::corner_boxes(w, h).to_vec();
    boxes.push(capture_pixels::middle(PixelRect { x: 0, y: 0, w, h }));
    capture_pixels::box_stats(image, &boxes).into_iter().map(|s| s.map(|s| s.alpha)).collect()
}

/// The first image of a capture's array, retained.
fn first_image(list: &NSArray) -> Option<CFRetained<CGImage>> {
    images_of(list).into_iter().next()
}

/// Every image of a capture's array, in its order, retained.
fn images_of(list: &NSArray) -> Vec<CFRetained<CGImage>> {
    list.iter()
        .filter_map(|member| {
            let p = std::ptr::NonNull::new(Retained::as_ptr(&member).cast_mut().cast::<CGImage>())?;
            // SAFETY: the array's members are CGImages (the call's contract), alive in the array.
            Some(unsafe { CFRetained::retain(p) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    fn listed(id: u32, pid: i32, bounds: Rect, alpha: f64) -> crate::platform::windows::CgWindow {
        crate::platform::windows::CgWindow { id, pid, owner: String::new(), name: None, layer: 0, bounds, onscreen: true, alpha }
    }

    #[test]
    fn a_window_of_its_app_over_it_sends_the_picture_to_screencapturekit() {
        let w = |id: u32, pid: i32, x: f64, alpha: f64| listed(id, pid, Rect::new(x, 141.0, 276.0, 298.0), alpha);
        let frame = Rect::new(80.0, 78.0, 520.0, 392.0);
        let d = [SCREEN];
        assert!(super::has_child_over(1, 7, frame, &[w(2, 7, 500.0, 1.0)], &d), "a popup reaching out of it");
        assert!(super::has_child_over(1, 7, frame, &[w(2, 7, 100.0, 1.0)], &d), "a sheet inside it");
        assert!(!super::has_child_over(1, 7, frame, &[w(2, 8, 500.0, 1.0)], &d), "another app's window");
        assert!(!super::has_child_over(1, 7, frame, &[w(2, 7, 500.0, 0.0)], &d), "a see-through one");
        assert!(!super::has_child_over(1, 7, frame, &[w(2, 7, 900.0, 1.0)], &d), "one beside it");
        assert!(!super::has_child_over(1, 7, frame, &[w(1, 7, 80.0, 1.0)], &d), "itself");
    }

    /// D-2, the Corner engine: an app's parked windows sit listed on screen at their full frames
    /// in the park corner; they are no children of each other, nor of the app's maximized window
    /// whose corner they reach, and the parked window's sliver under a shown one is none either.
    #[test]
    fn parked_windows_of_one_app_are_no_children() {
        let d = [SCREEN];
        let parked = |id| listed(id, 7, Rect::new(2559.0, 1408.0, 1000.0 + f64::from(id), 640.0), 1.0);
        let maximized = Rect::new(0.0, 25.0, 2560.0, 1415.0);
        assert!(!has_child_over(1, 7, parked(1).bounds, &[parked(1), parked(2)], &d), "two parked windows of the app");
        assert!(!has_child_over(3, 7, maximized, &[parked(1)], &d), "the maximized window and a parked one");
        assert!(!has_child_over(1, 7, parked(1).bounds, &[listed(3, 7, maximized, 1.0)], &d), "a parked window's sliver under the maximized one");
        assert!(has_child_over(3, 7, maximized, &[listed(4, 7, Rect::new(2300.0, 1200.0, 240.0, 200.0), 1.0)], &d), "a popover in its corner still counts");
    }

    use super::*;

    /// D-3: blank pictures rest a route only when a few different windows' come back so, and
    /// only for a while; once a picture was the window, never.
    #[test]
    fn a_route_rests_only_after_blank_pictures_of_a_few_windows() {
        let t0 = Instant::now();
        let mut h = Health::default();
        assert!(!h.blank(1, t0), "one window still being drawn at login");
        assert!(!h.blank(1, t0), "the same window again");
        assert!(!h.blank(2, t0));
        assert!(h.usable(t0));
        assert!(h.blank(3, t0), "a third window: the route rests");
        assert!(!h.usable(t0 + OFF_FOR - Duration::from_secs(1)));
        assert!(h.usable(t0 + OFF_FOR), "tried again after the rest");
        assert!(!h.blank(4, t0 + OFF_FOR), "the count starts afresh");
        let mut good = Health::default();
        good.blank(1, t0);
        good.blank(2, t0);
        assert!(good.good(), "the first good picture");
        assert!(!good.good());
        for id in 3..10 {
            assert!(!good.blank(id, t0), "a route that brought the window is never rested");
        }
        assert!(good.usable(t0) && good.word() == "works");
    }

    /// D-7: a description that comes back empty for windows asked is a failed read; a window
    /// missing from an answer that describes others is closed.
    #[test]
    fn an_empty_description_is_a_failed_read_not_every_window_closed() {
        assert_eq!(closed(&[1, 2, 3], &[]), None, "nothing described");
        assert_eq!(closed(&[1, 2, 3], &[1, 3]), Some(vec![2]));
        assert_eq!(closed(&[1, 2], &[2, 1]), Some(vec![]));
        assert_eq!(closed(&[], &[]), Some(vec![]));
    }

    const SCREEN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    const RIGHT: Rect = Rect::new(2560.0, 0.0, 1920.0, 1080.0);

    /// A window on a display, hidden in a stage Space (its frame where it was), or across two
    /// displays side by side goes the fast way; a parked one, or one past a screen's edge, the
    /// whole-window way.
    #[test]
    fn a_window_partly_off_every_display_is_taken_whole() {
        assert_eq!(route_for(Rect::new(300.0, 100.0, 1000.0, 640.0), &[SCREEN]), Route::Fast);
        assert_eq!(route_for(Rect::new(0.0, 0.0, 2560.0, 1440.0), &[SCREEN]), Route::Fast, "a full-screen window");
        assert_eq!(route_for(Rect::new(2000.0, 100.0, 1000.0, 640.0), &[SCREEN, RIGHT]), Route::Fast, "across two displays");
        assert_eq!(route_for(Rect::new(2558.0, 1438.0, 1000.0, 640.0), &[SCREEN]), Route::Whole, "parked in the corner, 2 pt on screen");
        assert_eq!(route_for(Rect::new(-400.0, 100.0, 1000.0, 640.0), &[SCREEN]), Route::Whole, "dragged past the left edge");
        assert_eq!(route_for(Rect::new(2000.0, 900.0, 1000.0, 640.0), &[SCREEN, RIGHT]), Route::Whole, "below the shorter display");
        assert_eq!(route_for(Rect::new(300.0, 100.0, 1000.0, 640.0), &[]), Route::Fast, "no display known");
    }

    /// The part on screen of a parked window (S4c: 4 x 64 px) is not the window; its own pixels at
    /// 1x or 2x are.
    #[test]
    fn a_picture_of_the_wrong_shape_is_not_the_window() {
        let frame = Rect::new(0.0, 0.0, 640.0, 452.0);
        assert!(fits_window(1280, 904, frame), "2x");
        assert!(fits_window(640, 452, frame), "1x");
        assert!(!fits_window(4, 64, frame), "the sliver on screen");
        assert!(!fits_window(1280, 600, frame), "cut short");
        assert!(!fits_window(320, 226, frame), "smaller than the window");
        assert!(!fits_window(0, 904, frame), "empty");
    }

    #[test]
    fn a_picture_see_through_all_over_is_blank() {
        assert!(see_through(&[Some(0), Some(0), Some(1), Some(0), Some(0)]));
        assert!(!see_through(&[Some(0), Some(0), Some(0), Some(0), Some(255)]), "the middle drawn");
        assert!(!see_through(&[Some(250), Some(250), Some(250), Some(250), Some(255)]));
        assert!(!see_through(&[None, None]), "nothing read tells nothing");
    }

    /// A card's thumbnail of a large window is drawn smaller; a full-size snapshot (asked at the
    /// window's own pixels or more) is kept as it came.
    #[test]
    fn only_a_much_smaller_ask_is_scaled() {
        assert!(needs_scaling((2800, 1800), (420, 270)));
        assert!(!needs_scaling((2800, 1800), (2800, 1800)));
        assert!(!needs_scaling((2800, 1800), (2700, 1736)), "within a tenth");
    }

    /// Round picture-wait: a list capture stands for its windows only with one image each.
    #[test]
    fn a_list_capture_counts_only_with_one_image_a_window() {
        assert!(one_each(3, 3));
        assert!(!one_each(1, 3), "one image of them all, or some missing");
        assert!(!one_each(4, 3));
        assert!(!one_each(0, 0), "nothing asked");
    }

    #[test]
    fn the_test_switch_counts_only_in_a_test_run() {
        assert!(forced_by(true, true));
        assert!(!forced_by(true, false), "never for an ordinary run");
        assert!(!forced_by(false, true));
    }
}
