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
//! shape, or see-through all over). A route whose first picture of the run comes back blank is
//! not used again in this run (logged once). `STAGE_MANAGER_CAPTURE_SCK` (honoured only with
//! `STAGE_MANAGER_ONLY`, a test run) turns this source off: every picture through
//! ScreenCaptureKit, as before 1.9.
//!
//! The calls are synchronous: callers run them off the main thread (the capture workers, a
//! switch's own thread). Each counts as a capture in flight for [`super::capture::quiesce`].
//! Every route needs Screen Recording; callers check it first (without it the window server's
//! call makes macOS ask).

use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{LazyLock, OnceLock};

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

    fn state(self) -> &'static AtomicU8 {
        match self {
            Route::Fast => &FAST_STATE,
            Route::Whole => &WHOLE_STATE,
        }
    }
}

/// A route not tried yet in this run.
const UNTRIED: u8 = 0;
/// A route whose first picture was the window.
const WORKS: u8 = 1;
/// A route not used in this run (its first picture came back blank).
const OFF: u8 = 2;

static FAST_STATE: AtomicU8 = AtomicU8::new(UNTRIED);
static WHOLE_STATE: AtomicU8 = AtomicU8::new(UNTRIED);

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

/// True when this source may take pictures at all in this run (not forced off, the calls
/// loaded, a route not turned off).
pub fn enabled() -> bool {
    !forced_off() && calls().is_some_and(|c| (c.hw.is_some() && usable(Route::Fast)) || (c.list_image.is_some() && usable(Route::Whole)))
}

fn usable(route: Route) -> bool {
    route.state().load(Ordering::Relaxed) != OFF
}

/// What this source says of itself, for the ctl dump.
pub fn status() -> serde_json::Value {
    let word = |r: Route| match r.state().load(Ordering::Relaxed) {
        WORKS => "works",
        OFF => "off",
        _ => "untried",
    };
    serde_json::json!({
        "forced_off": forced_off(),
        "fast": calls().and_then(|c| c.hw).map(|_| word(Route::Fast)).unwrap_or("missing"),
        "whole": calls().and_then(|c| c.list_image).map(|_| word(Route::Whole)).unwrap_or("missing"),
    })
}

/// Why a window has no picture from this source: the caller takes it through ScreenCaptureKit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Miss {
    /// The source is off, or the call for this window's route is missing or turned off.
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
    let (w, h) = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
    let blank = w == 0 || h == 0 || see_through(&alphas(&image, w, h));
    if blank || !fits_window(w, h, frame) {
        // The route's first picture of the run blank: not used again in this run.
        if blank && route.state().compare_exchange(UNTRIED, OFF, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            tracing::info!(route = route.as_str(), w, h, "window pictures: the first picture of this call came back blank; ScreenCaptureKit takes those pictures in this run");
        } else {
            tracing::debug!(window = t.id, route = route.as_str(), w, h, frame = ?frame, blank, "window picture: not the window; ScreenCaptureKit takes it");
        }
        return Err(Miss::NotTheWindow);
    }
    if route.state().compare_exchange(UNTRIED, WORKS, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
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
/// before (live 11-14: a card lost its popup). One query of the windows on screen.
pub fn with_children(ids: &[u32]) -> std::collections::HashSet<u32> {
    if ids.is_empty() {
        return Default::default();
    }
    let on = super::windows::on_screen();
    ids.iter().copied().filter(|id| on.iter().find(|w| w.id == *id).is_some_and(|w| has_child_over(w.id, w.pid, w.bounds, &on))).collect()
}

/// True when a window of `pid` other than `id`, on screen and not see-through, overlaps
/// `frame` (`on`: the windows on screen). A same-app window merely overlapping counts too: the
/// slower picture is right either way.
pub fn has_child_over(id: u32, pid: i32, frame: Rect, on: &[super::windows::CgWindow]) -> bool {
    on.iter().any(|w| w.id != id && w.pid == pid && w.alpha > 0.0 && w.bounds.intersect(&frame).is_some_and(|i| i.w >= 1.0 && i.h >= 1.0))
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
    let first = list.iter().next()?;
    let p = std::ptr::NonNull::new(Retained::as_ptr(&first).cast_mut().cast::<CGImage>())?;
    // SAFETY: the array's members are CGImages (the call's contract), alive in the array.
    Some(unsafe { CFRetained::retain(p) })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_window_of_its_app_over_it_sends_the_picture_to_screencapturekit() {
        let w = |id: u32, pid: i32, x: f64, alpha: f64| crate::platform::windows::CgWindow {
            id,
            pid,
            owner: String::new(),
            name: None,
            layer: 0,
            bounds: Rect::new(x, 141.0, 276.0, 298.0),
            onscreen: true,
            alpha,
        };
        let frame = Rect::new(80.0, 78.0, 520.0, 392.0);
        assert!(super::has_child_over(1, 7, frame, &[w(2, 7, 500.0, 1.0)]), "a popup reaching out of it");
        assert!(super::has_child_over(1, 7, frame, &[w(2, 7, 100.0, 1.0)]), "a sheet inside it");
        assert!(!super::has_child_over(1, 7, frame, &[w(2, 8, 500.0, 1.0)]), "another app's window");
        assert!(!super::has_child_over(1, 7, frame, &[w(2, 7, 500.0, 0.0)]), "a see-through one");
        assert!(!super::has_child_over(1, 7, frame, &[w(2, 7, 900.0, 1.0)]), "one beside it");
        assert!(!super::has_child_over(1, 7, frame, &[w(1, 7, 80.0, 1.0)]), "itself");
    }

    use super::*;

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

    #[test]
    fn the_test_switch_counts_only_in_a_test_run() {
        assert!(forced_by(true, true));
        assert!(!forced_by(true, false), "never for an ordinary run");
        assert!(!forced_by(false, true));
    }
}
