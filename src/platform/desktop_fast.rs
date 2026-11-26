//! The desktop's still straight from the window server (round desktop-still, 11-25): one call,
//! no listing of the shareable content. The front every still of the desktop goes through
//! ([`take`]): the corner patch's picture and a switch's cover ([`super::desktop_shot`]) and
//! the still that settles a solar frame (`ui/corner_settle.rs`).
//!
//! ScreenCaptureKit's still ([`super::capture::capture_desktop`]) lists every window of the
//! system first and takes a screenshot: about 350 to 430 ms of the window server's time each,
//! one every 10 s while slivers show, about 3.5 % of a core at rest (UX-SPEC, "Window engines
//! measured"). The window server composites a set of windows named by id into one image cut
//! to a rectangle of the screen by itself:
//! `CGWindowListCreateImageFromArray(screenBounds, windowArray, imageOption)` (CGWindow.h:
//! "an image containing a composite of the specified set of windows contained within a
//! rectangular area"; deprecated in macOS 14, marked unavailable in the 15 SDK and still
//! exported, so loaded at run time as [`super::capture_fast`] loads
//! `CGWindowListCreateImage`). No private signature: `SLSHWCaptureWindowList` hands back an
//! image a window with no composite and no crop documented anywhere public, and what it does
//! with several ids is not known (`probe-spaces capture BUNDLE fast list` prints it).
//!
//! - **Which windows** ([`desktop_ids`]): the desktop-level windows the window server lists
//!   on screen now that meet the block ([`capture::desktop_level`]: below layer 0, never
//!   ours; the same rule as ScreenCaptureKit's filter), in its own order: one
//!   `CGWindowListCopyWindowInfo` a still. The image holds only the windows named, so a
//!   window macOS raises in the corner, a parked window's sliver or our own panel is never in
//!   it.
//! - **The check** ([`judge`]): a still counts only at the block's size at the display's own
//!   scale, with a colour space of its own, and with no see-through part: the header says
//!   "if no windows meet the specified criteria, or the windows can't be read, then a
//!   transparent black image will be returned", and a still with the desktop picture's window
//!   missing is see-through where only it draws. Anything else is not proven the desktop:
//!   ScreenCaptureKit takes that still, as before this round.
//! - **The proof** ([`prove`]): size and alpha cannot tell a wrong opaque picture (the
//!   windows composited in another order, a picture the call cannot read, a colour shift), so
//!   the first still on a screen is taken both ways and stands only when the two agree
//!   ([`compare`]: the same size, and on a grid drawn in one colour space a mean difference
//!   of at most [`MEAN_TOLERANCE`] and no cell further than [`CELL_TOLERANCE`]). Proven is
//!   kept by screen, display frame, scale and colour space, and forgotten when the wallpaper
//!   read changes ([`unproven`]); until then every still is the window server's alone.
//! - **Its rest** ([`Health`]): [`BAD_STRIKES`] bad stills in a row (nothing handed back, not
//!   proven the desktop, or a proof that fails) rest the call for [`OFF_FOR`], logged once
//!   at info; ScreenCaptureKit takes every still meanwhile, then the call is tried again. A
//!   good still clears the count.
//! - **Colour**: the image comes tagged with the display's colour space and is never
//!   redrawn: the patch and the cover crop it (`CGImageCreateWithImageInRect` keeps the
//!   pixels and the space) and their layers match it to the screen, as they do
//!   ScreenCaptureKit's image. A still without an RGB colour space is refused, and the proof
//!   compares in the still's own space, so a shift fails it.
//! - `STAGE_MANAGER_STILL_SCK` (or `STAGE_MANAGER_CAPTURE_SCK`: every picture), honoured only
//!   with `STAGE_MANAGER_ONLY` (a test run), turns the call off: every still through
//!   ScreenCaptureKit, as before this round.
//!
//! The call is synchronous: [`take`] runs it on a thread of its own, counted as a capture in
//! flight for [`capture::quiesce`]. It needs Screen Recording and follows the capture gate
//! ([`capture::desktop_may_capture`]) like every other capture.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use objc2::rc::autoreleasepool;
use objc2_core_foundation::{CFArray, CFIndex, CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGColorSpace, CGColorSpaceModel, CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode, CGImage, kCGColorSpaceSRGB,
};

use super::capture::{self, DesktopStill, InFlight};
use super::costs::{self, Cost};
use super::spaces::ffi;
use super::windows::{self, CgWindow};
use super::{capture_fast, pixels, wallpaper};
use crate::engine::geometry::Rect;

/// Set (with `STAGE_MANAGER_ONLY`): every still of the desktop through ScreenCaptureKit.
pub const FORCE_SCK_ENV: &str = "STAGE_MANAGER_STILL_SCK";

/// `kCGWindowImageBestResolution`: the display's own pixels.
const BEST_RESOLUTION: u32 = 1 << 3;

/// `CGWindowListCreateImageFromArray(screenBounds, windowArray, imageOption)`: a +1 CGImage,
/// or null (CGWindow.h).
type FnArrayImage = unsafe extern "C" fn(CGRect, *const c_void, u32) -> *mut c_void;

/// This many bad stills in a row rest the call.
pub const BAD_STRIKES: usize = 3;
/// How long the call rests after [`BAD_STRIKES`] before it is tried again.
pub const OFF_FOR: Duration = Duration::from_secs(30 * 60);

/// A still whose least alpha is below this has a see-through part.
const OPAQUE_ALPHA: u8 = 250;

/// The longer side (cells) of the grid two stills are compared on ([`compare`]).
const GRID: usize = 64;
/// The most two stills of the same corner may differ on average (of 255, a cell's furthest
/// channel) and still count as the same picture.
pub const MEAN_TOLERANCE: f64 = 2.0;
/// The most any one cell may differ (of 255).
pub const CELL_TOLERANCE: f64 = 12.0;

/// How long [`compare_now`] waits for ScreenCaptureKit's still.
const COMPARE_WAIT: Duration = Duration::from_secs(5);

/// Which way a still of the desktop came.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    /// The window server's own call, no listing.
    WindowServer,
    /// ScreenCaptureKit: a listing of the shareable content, then a screenshot.
    ScreenCaptureKit,
}

impl Way {
    /// For `ctl state` and the log.
    pub fn as_str(self) -> &'static str {
        match self {
            Way::WindowServer => "window server",
            Way::ScreenCaptureKit => "ScreenCaptureKit",
        }
    }
}

/// What [`take`] calls with the still and the way it came, once (any thread).
pub type Done = Box<dyn FnOnce(DesktopStill, Way) + Send>;

/// Why the window server's call gave no still: ScreenCaptureKit takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Miss {
    /// The call is missing on this macOS.
    Unavailable,
    /// The window server listed no desktop-level window in the block (or its list came back
    /// empty: a failed read tells nothing of the call).
    NoWindow,
    /// The call handed back nothing.
    Nothing,
    /// A picture that is not proven the desktop ([`judge`]).
    NotTheDesktop(&'static str),
}

impl Miss {
    /// For the log and `ctl state`.
    pub fn why(self) -> &'static str {
        match self {
            Miss::Unavailable => "the call is missing",
            Miss::NoWindow => "no desktop-level window listed in the corner",
            Miss::Nothing => "nothing handed back",
            Miss::NotTheDesktop(why) => why,
        }
    }

    /// Whether it counts against the call ([`Health::bad`]): what the call itself did, not a
    /// list that came back empty.
    pub fn strikes(self) -> bool {
        matches!(self, Miss::Nothing | Miss::NotTheDesktop(_))
    }
}

/// What this run knows of the call: whether a still was proven yet, the bad ones in a row,
/// and a rest.
#[derive(Debug, Default)]
pub struct Health {
    works: bool,
    bad: usize,
    off_until: Option<Instant>,
}

impl Health {
    /// True unless the call rests at `now`; a rest over starts the count afresh.
    pub fn usable(&mut self, now: Instant) -> bool {
        match self.off_until {
            Some(until) if now < until => false,
            Some(_) => {
                self.off_until = None;
                self.bad = 0;
                true
            }
            None => true,
        }
    }

    /// A bad still: true when this rests the call (from `now`).
    pub fn bad(&mut self, now: Instant) -> bool {
        self.bad += 1;
        if self.bad < BAD_STRIKES {
            return false;
        }
        self.bad = 0;
        self.works = false;
        self.off_until = Some(now + OFF_FOR);
        true
    }

    /// A good still: true for the first since the launch or a rest.
    pub fn good(&mut self) -> bool {
        self.bad = 0;
        !std::mem::replace(&mut self.works, true)
    }

    fn word(&self) -> &'static str {
        match (self.off_until, self.works) {
            (Some(_), _) => "off",
            (None, true) => "works",
            (None, false) => "untried",
        }
    }
}

/// What a still was proven for: the display's frame, its pixels a point and the still's
/// colour space. Another of any of them is proven afresh.
#[derive(Clone, Debug, PartialEq)]
pub struct Proof {
    display: Rect,
    scale: f64,
    space: String,
}

/// How far two stills of the same corner are apart ([`diff`]), of 255.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Diff {
    /// The mean over the grid's cells of each cell's furthest channel.
    pub mean: f64,
    /// The furthest cell.
    pub worst: f64,
}

impl Diff {
    /// Whether the two are the same picture ([`MEAN_TOLERANCE`], [`CELL_TOLERANCE`]).
    pub fn same(self) -> bool {
        self.mean <= MEAN_TOLERANCE && self.worst <= CELL_TOLERANCE
    }

    fn json(self) -> serde_json::Value {
        serde_json::json!({ "mean": (self.mean * 100.0).round() / 100.0, "worst": self.worst, "same": self.same() })
    }
}

/// One screen's proof, its last comparison and the last reason a still went the other way.
#[derive(Default)]
struct Screen {
    proof: Option<Proof>,
    diff: Option<Diff>,
    why: Option<&'static str>,
}

/// The call's health and every screen's proof.
#[derive(Default)]
struct Route {
    health: Health,
    screens: BTreeMap<u32, Screen>,
    /// A rest was logged at info once; later ones at debug.
    rest_said: bool,
}

static ROUTE: LazyLock<Mutex<Route>> = LazyLock::new(|| Mutex::new(Route::default()));

fn route() -> MutexGuard<'static, Route> {
    ROUTE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The call, loaded once; `None` when this macOS does not export it.
fn call() -> Option<FnArrayImage> {
    static CALL: OnceLock<Option<FnArrayImage>> = OnceLock::new();
    *CALL.get_or_init(|| {
        let mut names = Vec::new();
        // SAFETY: the type follows the public signature (CGWindow.h, module docs).
        let f: Option<FnArrayImage> = unsafe { ffi::load(libc::RTLD_DEFAULT, c"CGWindowListCreateImageFromArray", &mut names) };
        if f.is_none() {
            tracing::info!("the desktop's still: CGWindowListCreateImageFromArray is missing on this macOS; ScreenCaptureKit takes every still");
        }
        f
    })
}

/// True when a test run asked for ScreenCaptureKit alone ([`FORCE_SCK_ENV`], or every
/// picture's switch, with `STAGE_MANAGER_ONLY`).
fn forced_off() -> bool {
    static FORCED: LazyLock<bool> = LazyLock::new(|| {
        let forced = capture_fast::forced_by(std::env::var_os(FORCE_SCK_ENV).is_some(), std::env::var_os("STAGE_MANAGER_ONLY").is_some());
        if forced {
            tracing::info!("the desktop's still: {FORCE_SCK_ENV} set (a test run): ScreenCaptureKit takes every still");
        }
        forced || capture_fast::forced_off()
    });
    *FORCED
}

/// True when the call may take a still now (not forced off, exported, not resting).
fn enabled() -> bool {
    !forced_off() && call().is_some() && route().health.usable(Instant::now())
}

/// The desktop-level windows of `listed` (the window server's on-screen list, the desktop's
/// own windows in it, front to back) that a still of `region` shows, in that order: on
/// screen, below layer 0, never of process `own` ([`capture::desktop_level`]), meeting the
/// block.
pub fn desktop_ids(listed: &[CgWindow], region: Rect, own: i32) -> Vec<u32> {
    listed
        .iter()
        .filter(|w| w.onscreen && capture::desktop_level(w.layer as isize, Some(w.pid), own) && w.bounds.intersect(&region).is_some())
        .map(|w| w.id)
        .collect()
}

/// What [`judge`] reads of a picture the call handed back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Facts {
    /// Its pixels.
    pub w: usize,
    /// Its pixels.
    pub h: usize,
    /// The block asked for (points).
    pub region: Rect,
    /// The display's pixels a point, when its mode can be read.
    pub display_scale: Option<f64>,
    /// Its least alpha ([`pixels::min_alpha`]); `None` when it cannot be read.
    pub min_alpha: Option<u8>,
    /// It carries an RGB colour space.
    pub rgb: bool,
}

/// Whether a picture the call handed back is proven a still of the block (module docs, "The
/// check"): its pixels a point, or why it is not the desktop.
pub fn judge(f: &Facts) -> Result<f64, &'static str> {
    if f.w == 0 || f.h == 0 || f.region.w < 1.0 || f.region.h < 1.0 {
        return Err("an empty picture");
    }
    let scale = f.display_scale.unwrap_or_else(|| (f.w as f64 / f.region.w).round()).max(1.0);
    let fits = |px: usize, pt: f64| (px as f64 - pt * scale).abs() <= 1.0;
    if !(fits(f.w, f.region.w) && fits(f.h, f.region.h)) {
        return Err("not the block's size at the display's scale");
    }
    if !f.rgb {
        return Err("no RGB colour space with the picture");
    }
    match f.min_alpha {
        None => Err("the picture cannot be read"),
        Some(a) if a < OPAQUE_ALPHA => Err("see-through: a desktop window is not in it"),
        Some(_) => Ok(scale),
    }
}

/// A still the call handed back that passed [`judge`].
struct Fast {
    image: CFRetained<CGImage>,
    scale: f64,
    proof: Proof,
}

/// The pixels a point of display `screen`, from its mode.
fn display_scale(screen: u32) -> Option<f64> {
    let mode = CGDisplayCopyDisplayMode(screen)?;
    let (px, pt) = (CGDisplayMode::pixel_width(Some(&mode)), CGDisplayMode::width(Some(&mode)));
    (px > 0 && pt > 0).then(|| px as f64 / pt as f64)
}

/// A still of `region` (global, top-left points) of display `screen` through the call, now
/// (synchronous: off the main and the WM thread), judged.
fn still(screen: u32, region: Rect) -> Result<Fast, Miss> {
    let f = call().ok_or(Miss::Unavailable)?;
    let own = std::process::id() as i32;
    let ids = desktop_ids(&windows::on_screen_with_desktop(), region, own);
    if ids.is_empty() {
        return Err(Miss::NoWindow);
    }
    // The array holds the CGWindowIDs themselves as its values (no callbacks), as the call
    // expects (`windows::describe`).
    let mut values: Vec<*const c_void> = ids.iter().map(|&id| id as usize as *const c_void).collect();
    let count = CFIndex::try_from(values.len()).map_err(|_| Miss::NoWindow)?;
    // SAFETY: `values` holds `count` pointer-sized values and outlives the call; null
    // callbacks make CF store them as they are, never retained or released.
    let array = unsafe { CFArray::new(None, values.as_mut_ptr(), count, std::ptr::null()) }.ok_or(Miss::Nothing)?;
    let rect = CGRect::new(CGPoint::new(region.x, region.y), CGSize::new(region.w, region.h));
    let flight = InFlight::issue();
    // SAFETY: the public signature (CGWindow.h): a rectangle of the screen, a live array of
    // window ids, an image option; a +1 CGImage or null.
    let p = unsafe { f(rect, std::ptr::from_ref::<CFArray>(&array).cast::<c_void>(), BEST_RESOLUTION) };
    drop(flight);
    // SAFETY: a +1 CGImage (or null), owned from here.
    let image = NonNull::new(p.cast::<CGImage>()).map(|p| unsafe { CFRetained::from_raw(p) }).ok_or(Miss::Nothing)?;
    let space = CGImage::color_space(Some(&image));
    let facts = Facts {
        w: CGImage::width(Some(&image)),
        h: CGImage::height(Some(&image)),
        region,
        display_scale: display_scale(screen),
        min_alpha: pixels::min_alpha(&image),
        rgb: space.as_deref().is_some_and(|s| CGColorSpace::model(Some(s)) == CGColorSpaceModel::RGB),
    };
    let scale = judge(&facts).map_err(Miss::NotTheDesktop)?;
    let bounds = CGDisplayBounds(screen);
    let display = Rect::new(bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height);
    let proof = Proof { display, scale, space: wallpaper::space_name(&image) };
    Ok(Fast { image, scale, proof })
}

/// The grid a `w` x `h` px picture is compared on: [`GRID`] cells on its longer side, its
/// proportions kept, never more cells than pixels.
pub fn grid(w: usize, h: usize) -> (usize, usize) {
    let long = w.max(h).max(1);
    let cells = |px: usize| (px * GRID).div_ceil(long).clamp(1, px.max(1));
    (cells(w), cells(h))
}

/// How far two pictures drawn on the same grid are apart (`a`, `b`: four bytes a cell):
/// each cell by its furthest byte. `None` unless they hold the same cells.
pub fn diff(a: &[u8], b: &[u8]) -> Option<Diff> {
    if a.is_empty() || a.len() != b.len() || !a.len().is_multiple_of(4) {
        return None;
    }
    let (mut sum, mut worst) = (0u64, 0u8);
    for (p, q) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        let d = p.iter().zip(q).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        sum += u64::from(d);
        worst = worst.max(d);
    }
    Some(Diff { mean: sum as f64 / (a.len() / 4) as f64, worst: f64::from(worst) })
}

/// How far the window server's still `fast` and ScreenCaptureKit's `other` of the same block
/// are apart: both drawn on one grid ([`grid`]) in `fast`'s own colour space (sRGB when no
/// bitmap can be made in it), so a colour shift shows as a difference. `Err` when they are
/// not the same size or cannot be read.
pub fn compare(fast: &CGImage, other: &CGImage) -> Result<Diff, &'static str> {
    let (w, h) = (CGImage::width(Some(fast)), CGImage::height(Some(fast)));
    if w.abs_diff(CGImage::width(Some(other))) > 1 || h.abs_diff(CGImage::height(Some(other))) > 1 {
        return Err("the two stills are not the same size");
    }
    let (gw, gh) = grid(w, h);
    let own = CGImage::color_space(Some(fast));
    // SAFETY: a constant name CoreGraphics defines.
    let srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
    for space in [own.as_deref(), srgb.as_deref()].into_iter().flatten() {
        if let (Some(a), Some(b)) = (pixels::drawn(fast, gw, gh, Some(space)), pixels::drawn(other, gw, gh, Some(space))) {
            return diff(&a, &b).ok_or("the two stills cannot be compared");
        }
    }
    Err("the stills cannot be read")
}

/// Whether the call's stills are proven for `proof` on `screen`.
fn proven(screen: u32, proof: &Proof) -> bool {
    route().screens.get(&screen).is_some_and(|s| s.proof.as_ref() == Some(proof))
}

/// The wallpaper read of `screen` names another picture, frame or look (main thread,
/// [`super::desktop_shot::wallpaper_changed`]): the next still there is proven afresh.
pub fn unproven(screen: u32) {
    if let Some(s) = route().screens.get_mut(&screen) {
        s.proof = None;
    }
}

/// A good still through the call: counted, the bad ones in a row forgotten.
fn good(screen: u32, first_said: &str) {
    costs::bump(Cost::DesktopStillFast);
    let mut r = route();
    r.screens.entry(screen).or_default().why = None;
    if r.health.good() {
        tracing::info!(screen, "the desktop's still: taken straight from the window server, no listing ({first_said})");
    }
}

/// A bad still through the call (`strike`), or one it could not try: why is kept for `ctl
/// state`, and [`BAD_STRIKES`] in a row rest the call.
fn bad(screen: u32, why: &'static str, strike: bool) {
    let mut r = route();
    r.screens.entry(screen).or_default().why = Some(why);
    if strike && r.health.bad(Instant::now()) {
        let (strikes, rest_min) = (BAD_STRIKES, OFF_FOR.as_secs() / 60);
        if std::mem::replace(&mut r.rest_said, true) {
            tracing::debug!(screen, why, strikes, rest_min, "the desktop's still: the window server's call still gives no good still; ScreenCaptureKit takes them for a while");
        } else {
            tracing::info!(screen, why, strikes, rest_min, "the desktop's still: the window server's call gave no good still a few times in a row; ScreenCaptureKit takes them for a while, then the call is tried again");
        }
    } else {
        tracing::debug!(screen, why, strike, "the desktop's still: not through the window server this time; ScreenCaptureKit takes it");
    }
}

/// The first still on a screen (or after its wallpaper changed), taken both ways: `fast`
/// stands when ScreenCaptureKit's (`sck`) is the same picture ([`compare`]) and is proven for
/// the stills after it; otherwise ScreenCaptureKit's is the answer, as before this round, and
/// the call has a bad still against it. With no still from ScreenCaptureKit nothing is
/// proven and nothing counted.
fn prove(screen: u32, fast: Fast, sck: DesktopStill) -> (DesktopStill, Way) {
    let (image, scale) = match sck {
        Ok(still) => still,
        Err(why) => return (Err(why), Way::ScreenCaptureKit),
    };
    let found = if (scale - fast.scale).abs() > 0.01 { Err("the two stills are not at the same scale") } else { compare(&fast.image, &image) };
    if let Ok(d) = found {
        route().screens.entry(screen).or_default().diff = Some(d);
    }
    match found {
        Ok(d) if d.same() => {
            tracing::debug!(screen, mean = d.mean, worst = d.worst, space = %fast.proof.space, "the desktop's still: the window server's is the same picture as ScreenCaptureKit's; proven for this screen");
            route().screens.entry(screen).or_default().proof = Some(fast.proof);
            good(screen, "the first one the same picture as ScreenCaptureKit's");
            (Ok((fast.image, fast.scale)), Way::WindowServer)
        }
        Ok(d) => {
            tracing::debug!(screen, mean = d.mean, worst = d.worst, "the desktop's still: the window server's differs from ScreenCaptureKit's of the same corner");
            bad(screen, "its pixels differ from ScreenCaptureKit's still of the same corner", true);
            (Ok((image, scale)), Way::ScreenCaptureKit)
        }
        Err(why) => {
            bad(screen, why, true);
            (Ok((image, scale)), Way::ScreenCaptureKit)
        }
    }
}

/// Takes `slot`'s value, once.
fn claim<T>(slot: &Mutex<Option<T>>) -> Option<T> {
    slot.lock().ok().and_then(|mut s| s.take())
}

/// A still through ScreenCaptureKit, `then` called with it once: also when it is not taken
/// at all (the capture gate closed since).
fn through_sck(screen: u32, region: Rect, then: impl FnOnce(DesktopStill) + Send + 'static) {
    costs::bump(Cost::DesktopStillSck);
    let slot = Arc::new(Mutex::new(Some(then)));
    let theirs = slot.clone();
    let done = Box::new(move |still: DesktopStill| {
        if let Some(then) = claim(&theirs) {
            then(still);
        }
    });
    if let Err(why) = capture::capture_desktop(screen, region, done)
        && let Some(then) = claim(&slot)
    {
        then(Err(why.to_string()));
    }
}

/// ScreenCaptureKit alone, exactly as before this round.
fn sck_only(screen: u32, region: Rect, done: Done) -> Result<(), &'static str> {
    costs::bump(Cost::DesktopStillSck);
    capture::capture_desktop(screen, region, Box::new(move |still| done(still, Way::ScreenCaptureKit)))
}

/// The window server's still first (the call's own thread): handed on when it is proven for
/// this screen, proven against ScreenCaptureKit's when it is not yet, and ScreenCaptureKit's
/// alone when the call misses.
fn first(screen: u32, region: Rect, done: Done) {
    if capture::closing() {
        done(Err("quitting".into()), Way::WindowServer);
        return;
    }
    match still(screen, region) {
        Ok(fast) if proven(screen, &fast.proof) => {
            good(screen, "proven");
            done(Ok((fast.image, fast.scale)), Way::WindowServer);
        }
        Ok(fast) => through_sck(screen, region, move |sck| {
            let (still, way) = prove(screen, fast, sck);
            done(still, way);
        }),
        Err(miss) => {
            bad(screen, miss.why(), miss.strikes());
            through_sck(screen, region, move |sck| done(sck, Way::ScreenCaptureKit));
        }
    }
}

/// A still of `region` (global, top-left points) of display `screen` showing the desktop
/// only (the desktop picture and what lies on it: Finder's icons, widgets), the window
/// server's own call first and ScreenCaptureKit as the fallback (module docs). `done` runs
/// once, on a thread of the capture's, with the still and the way it came. `Err` (nothing
/// issued, `done` never called) when captures may not run now
/// ([`capture::desktop_may_capture`]: quiet by design, as every capture).
pub fn take(screen: u32, region: Rect, done: Done) -> Result<(), &'static str> {
    capture::desktop_may_capture()?;
    if !enabled() {
        return sck_only(screen, region, done);
    }
    let flight = InFlight::issue();
    let slot = Arc::new(Mutex::new(Some(done)));
    let theirs = slot.clone();
    let spawned = std::thread::Builder::new().name("desktop-still".into()).spawn(move || {
        let _whole_run = flight;
        if let Some(done) = claim(&theirs) {
            autoreleasepool(|_| first(screen, region, done));
        }
    });
    match spawned {
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::debug!("the desktop's still: no thread for the window server's call ({e}); ScreenCaptureKit takes it");
            claim(&slot).map_or(Ok(()), |done| sck_only(screen, region, done))
        }
    }
}

/// What the call says of itself on `screen`, for `ctl state`'s `desktop_still.window_server`.
pub fn status(screen: u32) -> serde_json::Value {
    let (forced, exported) = (forced_off(), call().is_some());
    let r = route();
    let s = r.screens.get(&screen);
    serde_json::json!({
        "forced_off": forced,
        "call": if exported { r.health.word() } else { "missing" },
        "proven": s.is_some_and(|s| s.proof.is_some()),
        "space": s.and_then(|s| s.proof.as_ref()).map(|p| p.space.clone()),
        "diff": s.and_then(|s| s.diff).map(Diff::json),
        "why": s.and_then(|s| s.why),
    })
}

/// One picture's facts for [`compare_now`].
fn describe(image: &CGImage, scale: f64, ms: u128) -> serde_json::Value {
    serde_json::json!({
        "w": CGImage::width(Some(image)),
        "h": CGImage::height(Some(image)),
        "scale": scale,
        "space": wallpaper::space_name(image),
        "opaque": pixels::min_alpha(image).is_some_and(|a| a >= OPAQUE_ALPHA),
        "ms": ms as u64,
    })
}

/// `ctl debug-desktop-still` (a test's): a still of `region` of display `screen` taken both
/// ways now, whatever the test switch and the call's rest say, and how far apart they are
/// ([`compare`]). Waits for ScreenCaptureKit's, [`COMPARE_WAIT`] at most. No picture leaves
/// the process: sizes, colour spaces, times and the difference only.
pub fn compare_now(screen: u32, region: Rect) -> serde_json::Value {
    use serde_json::json;
    if let Err(why) = capture::desktop_may_capture() {
        return json!({ "ok": false, "error": why });
    }
    let t0 = Instant::now();
    let fast = autoreleasepool(|_| still(screen, region));
    let fast_ms = t0.elapsed().as_millis();
    let (tx, rx) = std::sync::mpsc::channel::<DesktopStill>();
    let t1 = Instant::now();
    through_sck(screen, region, move |sck| {
        let _ = tx.send(sck);
    });
    let sck = rx.recv_timeout(COMPARE_WAIT).unwrap_or_else(|_| Err("ScreenCaptureKit's still did not come in time".into()));
    let sck_ms = t1.elapsed().as_millis();
    let found = match (&fast, &sck) {
        (Ok(f), Ok((image, _))) => compare(&f.image, image).map(Diff::json).map_err(str::to_string),
        _ => Err("one of the two stills is missing".to_string()),
    };
    json!({
        "ok": true,
        "region": region,
        "window_server": match &fast {
            Ok(f) => describe(&f.image, f.scale, fast_ms),
            Err(miss) => json!({ "miss": miss.why(), "ms": fast_ms as u64 }),
        },
        "screencapturekit": match &sck {
            Ok((image, scale)) => describe(image, *scale, sck_ms),
            Err(why) => json!({ "miss": why, "ms": sck_ms as u64 }),
        },
        "same": found.as_ref().is_ok_and(|d| d["same"] == json!(true)),
        "diff": found.as_ref().ok(),
        "why_not_compared": found.as_ref().err(),
        "tolerance": { "mean": MEAN_TOLERANCE, "worst": CELL_TOLERANCE },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    const BLOCK: Rect = Rect::new(2240.0, 1120.0, 320.0, 320.0);

    fn listed(id: u32, pid: i32, layer: i64, bounds: Rect, onscreen: bool) -> CgWindow {
        CgWindow { id, pid, owner: String::new(), name: None, layer, bounds, onscreen, alpha: 1.0 }
    }

    /// The still names the desktop's own windows in the corner, in the window server's order,
    /// and nothing else: no app's window, no parked sliver, none of ours, none elsewhere.
    #[test]
    fn a_still_names_only_the_desktop_windows_in_the_corner() {
        let own = 77;
        let list = [
            listed(1, 10, 0, Rect::new(2559.0, 1408.0, 900.0, 600.0), true),
            listed(2, own, -100, Rect::new(2500.0, 1350.0, 60.0, 90.0), true),
            listed(3, 20, -2_147_483_603, MAIN, true),
            listed(4, 30, -2_147_483_600, Rect::new(100.0, 100.0, 300.0, 300.0), true),
            listed(5, 30, -2_147_483_600, Rect::new(2300.0, 1200.0, 160.0, 160.0), true),
            listed(6, 40, -2_147_483_624, MAIN, true),
            listed(7, 40, -2_147_483_624, MAIN, false),
            listed(8, 40, -2_147_483_624, Rect::new(2560.0, 0.0, 1920.0, 1080.0), true),
        ];
        assert_eq!(desktop_ids(&list, BLOCK, own), vec![3, 5, 6], "Finder's desktop, a widget in the block, the desktop picture");
        assert!(desktop_ids(&[], BLOCK, own).is_empty(), "a failed read");
    }

    fn facts(w: usize, h: usize) -> Facts {
        Facts { w, h, region: BLOCK, display_scale: Some(2.0), min_alpha: Some(255), rgb: true }
    }

    #[test]
    fn a_still_counts_only_at_the_blocks_size_opaque_and_with_its_colour_space() {
        assert_eq!(judge(&facts(640, 640)), Ok(2.0));
        assert_eq!(judge(&facts(641, 640)), Ok(2.0), "a pixel of rounding");
        assert_eq!(judge(&facts(320, 320)), Err("not the block's size at the display's scale"), "1x on a 2x display");
        assert_eq!(judge(&facts(640, 600)), Err("not the block's size at the display's scale"));
        assert_eq!(judge(&facts(0, 640)), Err("an empty picture"));
        // The display's mode not readable: the picture's own whole scale.
        assert_eq!(judge(&Facts { display_scale: None, ..facts(640, 640) }), Ok(2.0));
        assert_eq!(judge(&Facts { display_scale: None, ..facts(320, 320) }), Ok(1.0));
        assert_eq!(judge(&Facts { display_scale: None, ..facts(500, 640) }), Err("not the block's size at the display's scale"));
        // "A transparent black image" when the windows cannot be read; see-through in part
        // when the desktop picture's window is not in it.
        assert_eq!(judge(&Facts { min_alpha: Some(0), ..facts(640, 640) }), Err("see-through: a desktop window is not in it"));
        assert_eq!(judge(&Facts { min_alpha: Some(249), ..facts(640, 640) }), Err("see-through: a desktop window is not in it"));
        assert_eq!(judge(&Facts { min_alpha: None, ..facts(640, 640) }), Err("the picture cannot be read"));
        assert_eq!(judge(&Facts { rgb: false, ..facts(640, 640) }), Err("no RGB colour space with the picture"));
    }

    #[test]
    fn a_few_bad_stills_in_a_row_rest_the_call_for_a_while() {
        let t0 = Instant::now();
        let mut h = Health::default();
        assert_eq!(h.word(), "untried");
        assert!(!h.bad(t0) && !h.bad(t0));
        assert!(h.good(), "the first good still");
        assert!(!h.good());
        assert_eq!(h.word(), "works");
        assert!(!h.bad(t0) && !h.bad(t0), "a good one in between cleared the count");
        assert!(h.usable(t0));
        assert!(h.bad(t0), "the third in a row: it rests, though it worked before");
        assert_eq!(h.word(), "off");
        assert!(!h.usable(t0 + OFF_FOR - Duration::from_secs(1)));
        assert!(h.usable(t0 + OFF_FOR), "tried again after the rest");
        assert_eq!(h.word(), "untried");
        assert!(!h.bad(t0 + OFF_FOR), "the count starts afresh");
        assert!(h.good(), "logged again after a rest");
    }

    #[test]
    fn only_what_the_call_itself_did_counts_against_it() {
        assert!(Miss::Nothing.strikes() && Miss::NotTheDesktop("see-through").strikes());
        assert!(!Miss::NoWindow.strikes(), "a list that came back empty");
        assert!(!Miss::Unavailable.strikes());
        assert_eq!(Miss::NotTheDesktop("an empty picture").why(), "an empty picture");
    }

    #[test]
    fn two_stills_are_compared_cell_by_cell() {
        let a = [10u8, 20, 30, 255, 10, 20, 30, 255];
        assert_eq!(diff(&a, &a), Some(Diff { mean: 0.0, worst: 0.0 }));
        let b = [12u8, 20, 29, 255, 10, 60, 30, 255];
        let d = diff(&a, &b).expect("the same cells");
        assert_eq!(d, Diff { mean: 21.0, worst: 40.0 });
        assert!(!d.same(), "one cell far off: an icon in one and not the other");
        assert!(Diff { mean: 1.5, worst: 6.0 }.same(), "rounding");
        assert!(!Diff { mean: 4.0, worst: 6.0 }.same(), "a shift all over");
        assert!(!Diff { mean: 0.1, worst: 40.0 }.same());
        assert_eq!(diff(&a, &a[..4]), None);
        assert_eq!(diff(&[], &[]), None);
    }

    #[test]
    fn the_grid_keeps_the_pictures_proportions() {
        assert_eq!(grid(640, 640), (64, 64));
        assert_eq!(grid(960, 640), (64, 43));
        assert_eq!(grid(82, 168), (32, 64));
        assert_eq!(grid(10, 4), (10, 4), "never more cells than pixels");
        assert_eq!(grid(0, 0), (1, 1));
    }

    /// The same picture drawn twice is the same; a picture shifted in colour all over, or
    /// with a part missing, is not; another size is refused.
    #[test]
    fn a_still_is_proven_only_by_the_same_picture() {
        let a = pixels::tests::picture(640, 640, [0.2, 0.5, 0.8, 1.0], false);
        let same = pixels::tests::picture(640, 640, [0.2, 0.5, 0.8, 1.0], false);
        assert!(compare(&a, &same).expect("compared").same());
        let shifted = pixels::tests::picture(640, 640, [0.26, 0.5, 0.8, 1.0], false);
        assert!(!compare(&a, &shifted).expect("compared").same(), "a colour shift");
        let holed = pixels::tests::picture(640, 640, [0.2, 0.5, 0.8, 1.0], true);
        assert!(!compare(&a, &holed).expect("compared").same(), "the icons only");
        let small = pixels::tests::picture(320, 320, [0.2, 0.5, 0.8, 1.0], false);
        assert_eq!(compare(&a, &small), Err("the two stills are not the same size"));
    }

    #[test]
    fn a_way_has_its_word() {
        assert_eq!(Way::WindowServer.as_str(), "window server");
        assert_eq!(Way::ScreenCaptureKit.as_str(), "ScreenCaptureKit");
    }
}
