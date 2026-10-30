//! The desktop in the park corner, as a still: the corner patch's picture whenever one can be
//! taken (round desktop-still-patch, [`crate::engine::patch_still`]: exact by construction,
//! icons, widgets and any wallpaper included), and a switch's cover where a Finder desktop icon
//! lies under the corner (round 8L3), so an activation (⌘Tab, a Dock click) grows from its card
//! on a desktop with icons there too.
//!
//! For the patch ([`for_patch`]) a still is kept fresh while slivers show, even with no input
//! (only a locked screen or a sleeping display rests it), and taken again on a Desktop's
//! arrival once its slide settled, when the icons in the corner change, [`AGAIN_AFTER_TOUCH`]
//! after a press on the desktop or a release in the corner ([`touched`]), [`FADE`] after the
//! wallpaper changed ([`wallpaper_changed`]) or the frame the clock predicts is due
//! ([`set_frame_due`]), else every [`REFRESH_EVERY`].
//!
//! The corner patch draws the desktop picture from its file, so it cannot stand for a corner
//! with icons on it ([`crate::engine::corner_patch`]), and 8L2 gave such a desktop the instant
//! arrival. This keeps a still of the corner block ([`block`]) taken through ScreenCaptureKit
//! with every window excluded ([`super::capture::capture_desktop`]: the picture and what lies
//! on it, nothing else), fresh while it is wanted ([`keep_fresh`], from the WM thread at every
//! corner patch plan): macOS raises the activated windows in the corner before the app hears
//! of the activation, and a capture started then would come after the overlay's first frame.
//!
//! A still stands for the corner ([`usable`]) only for the Desktop it was taken on (the WM
//! thread's count of Space changes), the same display frame, the same icons in the corner
//! ([`zone`]; when they are not known, by its age alone), and at most [`MAX_AGE`] old; an
//! appearance or display change drops it ([`forget`]). Quiet as every capture is: nothing is
//! taken while captures are off, Screen Recording is missing or window captures have not
//! worked yet ([`super::capture::desktop_may_capture`]); then a switch has no still and an
//! activation arrives at once (8L2).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;

use super::capture::{self, DesktopStill};
use super::{pixels, wallpaper};
use crate::engine::geometry::Rect;
use crate::engine::patch_still::{self as ps, AGAIN_AFTER_TOUCH, Block, FADE, Still};
use crate::engine::wallpaper as wp;

/// The side (points) of the square at the display's bottom-right corner always captured: the
/// most a switch's cover paints there ([`crate::engine::corner_patch::COVER_CAP`], round 8L6),
/// so a still holds any cover; it grows to hold the patch's rectangle when that is bigger
/// ([`block`]).
pub const BLOCK: f64 = crate::engine::corner_patch::COVER_CAP;

/// The side (points) of the square at the corner whose icons a still is keyed by ([`zone`]):
/// any cover lies inside it with room to spare.
pub const ZONE: f64 = 640.0;

/// A still this old is taken again while it is wanted.
pub const REFRESH_EVERY: Duration = Duration::from_secs(10);

/// A still older than this never stands for the corner (the picture may have moved on: a
/// dynamic desktop, an icon's label).
pub const MAX_AGE: Duration = Duration::from_secs(45);

/// After a capture that could not be taken or failed, the next try waits this long.
pub const RETRY: Duration = Duration::from_secs(3);

/// After a still was taken, the next one waits at least this long (a burst of reasons, an
/// icon dragged about the corner, takes one still a moment, not one each).
pub const MIN_GAP: Duration = Duration::from_millis(500);

/// A capture not answered after this long is given up (the next one may start).
const STALL: Duration = Duration::from_secs(5);

/// What a still is of, as the WM thread knows it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Key {
    /// The WM thread's count of Space changes when it was wanted.
    pub space: u64,
    /// The main display's frame (global, top-left).
    pub display: Rect,
    /// Finder's desktop icons (grown by their reach) that meet the corner's [`zone`], in the
    /// order read; `None` when they are not known.
    pub icons: Option<Vec<Rect>>,
}

impl Key {
    /// The key for `display`, Desktop count `space`, from the icons known (`None`: not known).
    pub fn new(space: u64, display: Rect, known: Option<&[Rect]>) -> Self {
        let zone = zone(display);
        let icons = known.map(|k| k.iter().copied().filter(|i| i.intersect(&zone).is_some()).collect());
        Self { space, display, icons }
    }
}

/// A still's facts, without its pixels (for [`due`] and [`usable`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Meta {
    /// What it is of.
    pub key: Key,
    /// The block taken (global, top-left points).
    pub region: Rect,
    /// Its pixels a point.
    pub scale: f64,
    /// When its capture started (what it shows is at least that old).
    pub taken: Instant,
    /// No see-through part: the desktop picture's own window is in it. Otherwise it holds
    /// the icons only and is laid over the patch's picture of the wallpaper ([`Layers`]).
    pub opaque: bool,
    /// Counts the stills kept in this run: the patch redraws when it changes.
    pub serial: u64,
}

/// How a still stands for the corner ([`usable`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layers {
    /// On its own.
    Alone,
    /// Laid over the corner patch's picture of the wallpaper (it holds the icons only).
    OverWallpaper,
}

/// [`usable`]'s reason when there is no still at all.
const NO_STILL: &str = "no still of the desktop yet";

/// The square of side `side` at `display`'s bottom-right corner, on the display.
fn corner_square(display: Rect, side: f64) -> Rect {
    let w = side.min(display.w);
    let h = side.min(display.h);
    Rect::new(display.max_x() - w, display.max_y() - h, w, h)
}

/// The corner whose icons key a still: [`ZONE`] square.
pub fn zone(display: Rect) -> Rect {
    corner_square(display, ZONE)
}

/// The block a still takes: the [`BLOCK`] square at the corner, grown to hold `rect` (the
/// patch's rectangle) when given, on the display, in whole points.
pub fn block(display: Rect, rect: Option<Rect>) -> Rect {
    let square = corner_square(display, BLOCK);
    let r = rect.and_then(|r| r.intersect(&display)).map_or(square, |r| square.union(&r));
    let (x, y) = (r.x.floor().max(display.x), r.y.floor().max(display.y));
    let (mx, my) = (r.max_x().ceil().min(display.max_x()), r.max_y().ceil().min(display.max_y()));
    Rect::new(x, y, (mx - x).max(0.0), (my - y).max(0.0))
}

/// Whether a new still is due for `key` and `region`: none yet, another Desktop, display or
/// icons, a smaller block, [`REFRESH_EVERY`] old, or taken before one of the moments `again`
/// that has come (a press on the desktop, a wallpaper change or a predicted frame change and
/// its fade).
pub fn due(meta: Option<&Meta>, key: &Key, region: Rect, again: &[Instant], now: Instant) -> bool {
    let Some(m) = meta else { return true };
    m.key != *key
        || !m.region.contains_rect(&region)
        || now.saturating_duration_since(m.taken) >= REFRESH_EVERY
        || again.iter().any(|a| now >= *a && m.taken < *a)
}

/// Whether a capture may start now after the last try (`tried`, and whether it `failed`):
/// [`RETRY`] after one that failed or could not be taken, [`MIN_GAP`] after one taken.
pub fn may_try(tried: Option<Instant>, failed: bool, now: Instant) -> bool {
    let gap = if failed { RETRY } else { MIN_GAP };
    tried.is_none_or(|t| now.saturating_duration_since(t) >= gap)
}

/// Whether the still `meta` stands for the desktop at `rect` (global, top-left) now, for `key`
/// (what the WM thread knows now), and how ([`Layers`]); why not.
pub fn usable(meta: Option<&Meta>, rect: Rect, key: &Key, now: Instant) -> Result<Layers, &'static str> {
    let m = meta.ok_or(NO_STILL)?;
    if m.key.space != key.space || m.key.display != key.display {
        return Err("the desktop's still is of another Desktop or display");
    }
    if key.icons.is_some() && m.key.icons != key.icons {
        return Err("the desktop's icons changed since its still");
    }
    if now.saturating_duration_since(m.taken) > MAX_AGE {
        return Err("the desktop's still is too old");
    }
    wp::crop_px(m.region, rect, m.scale).ok_or("the desktop's still does not hold the corner")?;
    Ok(if m.opaque { Layers::Alone } else { Layers::OverWallpaper })
}

/// No still is taken after this long with no input and no switch: the cover is only needed
/// at a switch, and the first input after brings a fresh one (round 8L4).
pub const IDLE: Duration = Duration::from_secs(120);

/// A still's take is logged at most this often (its skips once per reason).
const TAKE_LOG_EVERY: Duration = Duration::from_secs(60);

/// A still whose least alpha is below this has see-through parts: the desktop picture's own
/// window was not drawn in it, and it is laid over the patch's picture of the wallpaper.
const OPAQUE_ALPHA: u8 = 250;

/// What the Mac is doing, for [`rest`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Moment {
    /// The screen is locked (or the session is not on the console).
    pub locked: bool,
    /// The main display sleeps.
    pub asleep: bool,
    /// Since the last keyboard, mouse or trackpad event.
    pub since_input: Duration,
    /// Since the last switch (`None`: none yet in this run).
    pub since_switch: Option<Duration>,
}

/// Why no still is taken now (round 8L4): the screen locked, the display asleep, or, for a
/// switch's cover alone (`patch` false: the patch does not stand on one), no input and no
/// switch for [`IDLE`]. `None`: one may be taken.
pub fn rest(m: Moment, patch: bool) -> Option<&'static str> {
    if m.locked {
        return Some("the screen is locked");
    }
    if m.asleep {
        return Some("the display sleeps");
    }
    if !patch && m.since_input >= IDLE && m.since_switch.is_none_or(|s| s >= IDLE) {
        return Some("no input and no switch for 2 min");
    }
    None
}

/// Who wants the still kept fresh ([`keep_fresh`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Wants {
    /// A switch's cover may need it (8L3).
    pub cover: bool,
    /// The corner patch draws it (slivers show).
    pub patch: bool,
}

/// Whether a still may be taken now: the capture gate first (captures on, Screen Recording,
/// window captures working: [`capture::desktop_may_capture`]), the Mac not at [`rest`], then
/// what the WM thread knows (`safety`, [`ps::may_take`]).
fn take_now(moment: Moment, patch: bool, safety: Result<(), Block>) -> Result<(), Block> {
    capture::desktop_may_capture().map_err(Block::No)?;
    if let Some(why) = rest(moment, patch) {
        return Err(Block::No(why));
    }
    safety
}

/// Whether an answer to a capture started at `started` is kept over the still `kept`: only
/// one started after it (a stalled capture answering late never replaces a newer still).
pub fn newer(kept: Option<&Meta>, started: Instant) -> bool {
    kept.is_none_or(|k| started > k.taken)
}

/// A still and its facts.
struct Shot {
    meta: Meta,
    image: CFRetained<CGImage>,
}

/// The still kept, the capture in flight and the last try.
#[derive(Default)]
struct State {
    shot: Option<Shot>,
    /// A capture in flight: when it started.
    taking: Option<Instant>,
    /// When the last capture was tried (taken or not).
    tried: Option<Instant>,
    /// Bumped by [`forget`]: a capture started before is not kept.
    generation: u64,
    /// The last reason no still could be taken, logged once.
    said: Option<String>,
    /// The last reason no still could be taken or is taken now, as a switch's `at_once`
    /// gives it when there is no still at all ([`ready`]).
    why_none: Option<&'static str>,
    /// A still was taken once (logged once).
    had: bool,
    /// When a take was last logged ([`TAKE_LOG_EVERY`]).
    take_logged: Option<Instant>,
    /// The last try failed or could not be taken ([`may_try`]).
    failed: bool,
    /// The stills kept in this run ([`Meta::serial`]).
    serial: u64,
    /// When the still was last dropped ([`forget`], [`wallpaper_changed`]).
    dropped_at: Option<Instant>,
    /// Moments a still taken before is taken again once they come ([`due`]): a press on the
    /// desktop ([`touched`]), a wallpaper change's fade.
    again: Vec<Instant>,
    /// The predicted frame change's moment, its fade included ([`set_frame_due`]).
    frame_at: Option<Instant>,
    /// Since when no still has stood for the patch's corner ([`ps::state`]).
    missing_since: Option<Instant>,
    /// What the patch found at the last plan, for `ctl state`.
    patch: Option<Still>,
}

static STATE: Mutex<State> = Mutex::new(State {
    shot: None,
    taking: None,
    tried: None,
    generation: 0,
    said: None,
    why_none: None,
    had: false,
    take_logged: None,
    failed: false,
    serial: 0,
    dropped_at: None,
    again: Vec::new(),
    frame_at: None,
    missing_since: None,
    patch: None,
});

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.lock().ok().map(|mut s| f(&mut s))
}

/// Logs why no still could be taken, once per reason (until one is taken).
fn say(s: &mut State, why: &str) {
    if s.said.as_deref() != Some(why) {
        tracing::debug!(why, "the desktop's still (the corner patch's picture, a switch's cover) is not taken now");
        s.said = Some(why.to_string());
    }
}

/// The moments a still taken before is due again ([`due`]): [`State::again`] and the
/// predicted frame change.
fn marks(s: &State) -> Vec<Instant> {
    s.again.iter().copied().chain(s.frame_at).collect()
}

/// Keeps a still of `region` (global, top-left; [`block`]) fresh for `key` (WM thread, at every
/// corner patch plan while the patch or a switch's cover would need it, `wants`): starts a
/// capture when one is [`due`], none is in flight, the last try allows it ([`may_try`]), and
/// it may be taken now (the capture gate, the Mac not at [`rest`], `safety` from the WM
/// thread: [`ps::may_take`]).
pub fn keep_fresh(key: Key, region: Rect, moment: Moment, wants: Wants, safety: Result<(), Block>, now: Instant) {
    if !(wants.cover || wants.patch) {
        return;
    }
    let start = with_state(|s| {
        // The capture gate first (pure and cheap): with captures off that is the reason the
        // switch logs, whatever else holds (the scripts expect exactly it).
        if let Err(block) = take_now(moment, wants.patch, safety) {
            if let Block::No(why) = block {
                s.why_none = Some(why);
            }
            say(s, block.why());
            return None;
        }
        if s.taking.is_some_and(|t| now.saturating_duration_since(t) < STALL) {
            return None;
        }
        if !due(s.shot.as_ref().map(|x| &x.meta), &key, region, &marks(s), now) {
            return None;
        }
        if !may_try(s.tried, s.failed, now) {
            return None;
        }
        s.tried = Some(now);
        s.taking = Some(now);
        Some(s.generation)
    })
    .flatten();
    let Some(generation) = start else { return };
    let wanted = key.clone();
    let done = Box::new(move |still: DesktopStill| arrived(still, wanted, region, generation, now));
    if let Err(why) = capture::capture_desktop(region, done) {
        with_state(|s| {
            s.taking = None;
            s.failed = true;
            s.why_none = Some(why);
            say(s, why);
        });
    }
}

/// The still for the corner patch at `rect` (WM thread, at every plan while slivers show):
/// one that stands for it now ([`usable`], whole: the desktop picture's window in it), else
/// whether one is coming ([`ps::state`]: it may be taken now or shortly, for at most
/// [`ps::HOLD_MAX`]). Call after [`keep_fresh`] with the same `key`.
pub fn for_patch(rect: Rect, key: &Key, moment: Moment, safety: Result<(), Block>, now: Instant) -> Still {
    let take = take_now(moment, true, safety);
    with_state(|s| {
        let meta = s.shot.as_ref().map(|x| &x.meta);
        let ready = match usable(meta, rect, key, now) {
            Ok(Layers::Alone) => meta.map(|m| m.serial),
            // A still of the icons only is laid over the file's picture by a switch's cover;
            // the patch draws the file's picture then, as without one.
            Ok(Layers::OverWallpaper) => None,
            Err(_) => None,
        };
        let still = match ready {
            Some(serial) => {
                s.missing_since = None;
                Still::Ready(serial)
            }
            None => {
                let since = *s.missing_since.get_or_insert(now);
                let opaque_known = meta.is_none_or(|m| m.opaque);
                if opaque_known { ps::state(None, take, now.saturating_duration_since(since)) } else { Still::Absent }
            }
        };
        if s.patch.as_ref().map(std::mem::discriminant) != Some(std::mem::discriminant(&still)) {
            tracing::debug!(?still, why = take.err().map(Block::why), "corner patch: the desktop's still for the patch");
        }
        s.patch = Some(still);
        still
    })
    .unwrap_or_default()
}

/// The still kept, its block (global, top-left points) and its pixels a point, for the corner
/// patch (main thread), when it is the one with `serial` and stands alone; the patch crops it
/// ([`crop`]) and keeps it to hold while the next one comes.
pub fn patch_still(serial: u64) -> Option<(CFRetained<CGImage>, Rect, f64)> {
    with_state(|s| s.shot.as_ref().filter(|x| x.meta.serial == serial && x.meta.opaque).map(|x| (x.image.clone(), x.meta.region, x.meta.scale)))
        .flatten()
}

/// The block (global, top-left points) of the still kept, when it stands alone (the patch's
/// picture can be cropped from it): what the patch may cover while Show Desktop is up, when
/// no new still is taken.
pub fn kept_region() -> Option<Rect> {
    with_state(|s| s.shot.as_ref().filter(|x| x.meta.opaque).map(|x| x.meta.region)).flatten()
}

/// The pixels of `rect` (global, top-left, whole points) out of a still of `region` at
/// `scale`; `None` unless the still holds it.
pub fn crop(still: &CGImage, region: Rect, scale: f64, rect: Rect) -> Option<CFRetained<CGImage>> {
    let px = wp::crop_px(region, rect, scale)?;
    wallpaper::crop(still, px)
}

/// A press on the desktop, or a release in the corner (WM thread): a still taken before is
/// taken again [`AGAIN_AFTER_TOUCH`] later (an icon selected, deselected or dropped).
pub fn touched(now: Instant) {
    with_state(|s| add_mark(s, now + AGAIN_AFTER_TOUCH));
}

/// Keeps a moment in [`State::again`] (a few at most: the older ones a still has passed go).
fn add_mark(s: &mut State, at: Instant) {
    let taken = s.shot.as_ref().map(|x| x.meta.taken);
    s.again.retain(|a| taken.is_none_or(|t| *a > t));
    if !s.again.contains(&at) {
        s.again.push(at);
    }
    if s.again.len() > 8 {
        s.again.remove(0);
    }
}

/// The wallpaper read names another picture, placement or look (main thread): the still is
/// of the old one; dropped, and taken again once the fade is over ([`FADE`]).
pub fn wallpaper_changed(now: Instant) {
    forget();
    with_state(|s| add_mark(s, now + FADE));
}

/// When the clock predicts the next frame of a dynamic wallpaper (main thread, at every
/// wallpaper read; `None`: none): a still taken before is taken again once it and its fade
/// ([`FADE`]) are past ([`next_frame_mark`]).
pub fn set_frame_due(due: Option<Instant>) {
    let now = Instant::now();
    with_state(|s| {
        let taken = s.shot.as_ref().map(|x| x.meta.taken);
        s.frame_at = next_frame_mark(s.frame_at, due.map(|d| d + FADE), now, taken);
    });
}

/// The predicted frame change to wait for: one still ahead (`kept`, later than `now`) stays
/// unless the new prediction (`new`) is sooner, so a read every few seconds of a clock that
/// says "look again in a minute" (a solar frame) does not push it ahead for ever; one come
/// but not yet served (no still `taken` after it) stays until a still is.
pub fn next_frame_mark(kept: Option<Instant>, new: Option<Instant>, now: Instant, taken: Option<Instant>) -> Option<Instant> {
    match kept {
        Some(k) if k > now => Some(new.map_or(k, |n| k.min(n))),
        Some(k) if taken.is_none_or(|t| t < k) => Some(k),
        _ => new,
    }
}

/// Since the still was last dropped (a Space, appearance, display or picture change), for
/// [`ps::TakeFacts::since_dropped`].
pub fn since_dropped(now: Instant) -> Option<Duration> {
    with_state(|s| s.dropped_at.map(|t| now.saturating_duration_since(t))).flatten()
}

/// A capture started at `started` answered (a ScreenCaptureKit queue): kept unless [`forget`]
/// came since or a newer still is kept ([`newer`]); stamped with its start (what it shows is
/// at least that old).
fn arrived(still: DesktopStill, key: Key, region: Rect, generation: u64, started: Instant) {
    // Judged before the lock: a small draw.
    let still = still.map(|(image, scale)| {
        let opaque = pixels::min_alpha(&image).is_some_and(|a| a >= OPAQUE_ALPHA);
        (image, scale, opaque)
    });
    with_state(|s| {
        if s.taking == Some(started) {
            s.taking = None;
        }
        s.failed = still.is_err();
        match still {
            Ok(_) if s.generation != generation => tracing::trace!("a desktop still from before a change: dropped"),
            Ok(_) if !newer(s.shot.as_ref().map(|x| &x.meta), started) => {
                tracing::debug!("a desktop still older than the one kept: dropped");
            }
            Ok((image, scale, opaque)) => {
                if !s.had {
                    s.had = true;
                    tracing::info!(%region, opaque, "a still of the desktop stands for the park corner (the corner patch's picture, a switch's cover)");
                }
                let now = Instant::now();
                if s.take_logged.is_none_or(|t| now.saturating_duration_since(t) >= TAKE_LOG_EVERY) {
                    s.take_logged = Some(now);
                    tracing::debug!(%region, opaque, took_ms = now.saturating_duration_since(started).as_millis() as u64, "the desktop's still taken (logged at most once a minute)");
                }
                s.said = None;
                s.why_none = None;
                s.serial = s.serial.wrapping_add(1);
                s.again.retain(|a| *a > started);
                s.shot = Some(Shot { meta: Meta { key, region, scale, taken: started, opaque, serial: s.serial }, image });
            }
            Err(why) => say(s, &why),
        }
    });
}

/// Whether the still kept stands for the desktop at `rect` now ([`usable`]; any thread),
/// with `wallpaper` (whether the patch's picture holds `rect`) for a still that holds the
/// icons only. With no still at all, why none was taken ([`decide`]).
pub fn ready(rect: Rect, key: &Key, wallpaper: impl FnOnce(Rect) -> Result<(), &'static str>) -> Result<(), &'static str> {
    let (found, why_none) = with_state(|s| (usable(s.shot.as_ref().map(|x| &x.meta), rect, key, Instant::now()), s.why_none))
        .ok_or("the desktop's still is not readable")?;
    decide(found, why_none, || wallpaper(rect))
}

/// [`ready`]'s answer from [`usable`]'s (`found`), why no still was taken (`why_none`) and
/// whether the patch's picture holds the corner (`wallpaper`, asked only for a still that
/// holds the icons only).
fn decide(
    found: Result<Layers, &'static str>,
    why_none: Option<&'static str>,
    wallpaper: impl FnOnce() -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    match found {
        Ok(Layers::Alone) => Ok(()),
        Ok(Layers::OverWallpaper) => {
            wallpaper().map_err(|_| "the desktop's still holds the icons only and the patch has no picture of the wallpaper")
        }
        Err(NO_STILL) => Err(why_none.unwrap_or(NO_STILL)),
        Err(why) => Err(why),
    }
}

/// The still's pixels of `rect` (global, top-left, whole points) and its scale, when it holds
/// them (the strip, main thread, drawing a switch's cover the WM thread found [`ready`]); one
/// that holds the icons only is laid over the patch's picture of the wallpaper. Logs why
/// not when it finds nothing.
pub fn image(rect: Rect) -> Option<(CFRetained<CGImage>, f64)> {
    let found = with_state(|s| s.shot.as_ref().map(|x| (x.image.clone(), x.meta.region, x.meta.scale, x.meta.opaque))).flatten();
    let picture = (|| {
        let (image, region, scale, opaque) = found.ok_or("no still of the desktop now (dropped since it was ready)")?;
        let px = wp::crop_px(region, rect, scale).ok_or("the still does not hold the corner")?;
        let top = wallpaper::crop(&image, px).ok_or("the still's crop failed")?;
        if opaque {
            return Ok((top, scale));
        }
        let (base, base_scale) = crate::ui::corner_patch::cover_image(rect).ok_or("no picture of the wallpaper to lay the icons on")?;
        pixels::over(&base, &top).map(|i| (i, base_scale)).ok_or("the icons could not be laid on the wallpaper")
    })();
    picture
        .inspect_err(|why| tracing::debug!(%rect, why, "the desktop's still was ready for the switch but gives no picture of the corner now"))
        .ok()
}

/// Whether a switch's cover may need the corner patch's picture under the still: none taken
/// yet (it may hold the icons only), or the one kept does (WM thread: the patch keeps its
/// picture rendered while hidden then).
pub fn needs_wallpaper() -> bool {
    with_state(|s| s.shot.as_ref().is_none_or(|x| !x.meta.opaque)).unwrap_or(false)
}

/// Drops the still (a Space, appearance or display change); a capture in flight is not kept.
/// The patch holds what a still drew for a moment ([`ps::HOLD_MAX`]) while the next one comes
/// ([`ps::SETTLE`] later).
pub fn forget() {
    with_state(|s| {
        s.shot = None;
        s.generation = s.generation.wrapping_add(1);
        s.tried = None;
        s.failed = false;
        s.dropped_at = Some(Instant::now());
        s.missing_since = None;
    });
}

/// The still for `ctl state` (tests): its block, age, whether it stands alone, why none.
pub fn status() -> serde_json::Value {
    with_state(|s| {
        serde_json::json!({
            "region": s.shot.as_ref().map(|x| x.meta.region),
            "age_ms": s.shot.as_ref().map(|x| x.meta.taken.elapsed().as_millis() as u64),
            "opaque": s.shot.as_ref().map(|x| x.meta.opaque),
            "taking": s.taking.is_some(),
            "why_none": s.why_none,
            "serial": s.shot.as_ref().map(|x| x.meta.serial),
            // What the corner patch found at the last plan: ready (drawn from the still),
            // coming (held) or absent (the file's picture, as without Screen Recording).
            "patch": s.patch.map(|p| match p {
                Still::Ready(_) => "ready",
                Still::Coming => "coming",
                Still::Absent => "absent",
            }),
        })
    })
    .unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);

    fn meta(key: Key, taken: Instant) -> Meta {
        Meta { key, region: block(MAIN, None), scale: 2.0, taken, opaque: true, serial: 1 }
    }

    #[test]
    fn the_still_takes_the_corner_block_grown_to_hold_the_patch() {
        assert_eq!(block(MAIN, None), Rect::new(2240.0, 1120.0, 320.0, 320.0));
        let big = Rect::new(2100.5, 1300.0, 480.0, 140.0);
        let b = block(MAIN, Some(big));
        let on = big.intersect(&MAIN).expect("on the display");
        assert!(b.contains_rect(&on) && b.contains_rect(&block(MAIN, None)), "{b:?}");
        assert_eq!((b.x, b.max_x(), b.max_y()), (2100.0, 2560.0, 1440.0));
        // A display smaller than the block: the display.
        let small = Rect::new(0.0, 0.0, 200.0, 150.0);
        assert_eq!(block(small, None), small);
    }

    #[test]
    fn the_key_holds_only_the_icons_in_the_corner() {
        let near = Rect::new(2400.0, 1300.0, 100.0, 100.0);
        let far = Rect::new(100.0, 100.0, 100.0, 100.0);
        assert_eq!(Key::new(3, MAIN, Some(&[far, near])).icons, Some(vec![near]));
        assert_eq!(Key::new(3, MAIN, None).icons, None);
    }

    #[test]
    fn a_still_is_due_when_missing_stale_or_for_another_key() {
        let now = Instant::now();
        let key = Key::new(1, MAIN, Some(&[]));
        let region = block(MAIN, None);
        assert!(due(None, &key, region, &[], now));
        let m = meta(key.clone(), now);
        assert!(!due(Some(&m), &key, region, &[], now + Duration::from_secs(2)));
        assert!(due(Some(&m), &key, region, &[], now + REFRESH_EVERY));
        assert!(due(Some(&m), &Key { space: 2, ..key.clone() }, region, &[], now));
        let moved = Key::new(1, MAIN, Some(&[Rect::new(2400.0, 1300.0, 100.0, 100.0)]));
        assert!(due(Some(&m), &moved, region, &[], now), "an icon came into the corner");
        assert!(due(Some(&m), &key, Rect::new(2000.0, 1120.0, 560.0, 320.0), &[], now), "a bigger block");
    }

    #[test]
    fn a_still_stands_for_the_corner_only_for_its_desktop_icons_and_age() {
        let now = Instant::now();
        let icon = Rect::new(2400.0, 1300.0, 100.0, 100.0);
        let key = Key::new(1, MAIN, Some(&[icon]));
        let m = meta(key.clone(), now);
        let corner = Rect::new(2499.0, 1348.0, 61.0, 92.0);
        assert_eq!(usable(Some(&m), corner, &key, now), Ok(Layers::Alone));
        assert_eq!(usable(Some(&Meta { opaque: false, ..m.clone() }), corner, &key, now), Ok(Layers::OverWallpaper), "the icons only");
        assert_eq!(usable(None, corner, &key, now), Err("no still of the desktop yet"));
        assert_eq!(
            usable(Some(&m), corner, &Key { space: 2, ..key.clone() }, now),
            Err("the desktop's still is of another Desktop or display")
        );
        assert_eq!(usable(Some(&m), corner, &Key::new(1, MAIN, Some(&[])), now), Err("the desktop's icons changed since its still"));
        // Icons not known now: its age alone.
        assert_eq!(usable(Some(&m), corner, &Key::new(1, MAIN, None), now), Ok(Layers::Alone));
        assert_eq!(usable(Some(&m), corner, &key, now + MAX_AGE + Duration::from_secs(1)), Err("the desktop's still is too old"));
        let outside = Rect::new(1900.0, 1348.0, 660.0, 92.0);
        assert_eq!(usable(Some(&m), outside, &key, now), Err("the desktop's still does not hold the corner"));
    }

    #[test]
    fn no_still_while_locked_asleep_or_idle_and_one_at_the_first_input_after() {
        let busy = Moment { locked: false, asleep: false, since_input: Duration::from_secs(1), since_switch: None };
        assert_eq!(rest(busy, false), None);
        assert_eq!(rest(Moment { locked: true, ..busy }, false), Some("the screen is locked"));
        assert_eq!(rest(Moment { asleep: true, ..busy }, false), Some("the display sleeps"));
        let idle = Moment { since_input: IDLE, ..busy };
        assert_eq!(rest(idle, false), Some("no input and no switch for 2 min"), "no switch yet in the run");
        assert_eq!(rest(Moment { since_switch: Some(IDLE + Duration::from_secs(5)), ..idle }, false), Some("no input and no switch for 2 min"));
        // A switch (a ctl switch, a test's) counts as being in use.
        assert_eq!(rest(Moment { since_switch: Some(Duration::from_secs(30)), ..idle }, false), None);
        // The first input after: a still again (the old one is past REFRESH_EVERY, so due).
        assert_eq!(rest(Moment { since_input: Duration::from_millis(200), ..idle }, false), None);
        // The patch drawn from a still is kept fresh with no input (a film watched), never
        // while the screen is locked or the display sleeps.
        assert_eq!(rest(idle, true), None);
        assert_eq!(rest(Moment { locked: true, ..idle }, true), Some("the screen is locked"));
        assert_eq!(rest(Moment { asleep: true, ..idle }, true), Some("the display sleeps"));
    }

    #[test]
    fn a_still_is_taken_again_after_a_touch_a_wallpaper_change_or_a_predicted_frame() {
        let t0 = Instant::now();
        let key = Key::new(1, MAIN, Some(&[]));
        let region = block(MAIN, None);
        let m = meta(key.clone(), t0);
        let soon = t0 + Duration::from_secs(1);
        // A press on the desktop at t0 + 1 s: due once its moment came, not before.
        let touch = soon + crate::engine::patch_still::AGAIN_AFTER_TOUCH;
        assert!(!due(Some(&m), &key, region, &[touch], soon));
        assert!(due(Some(&m), &key, region, &[touch], touch));
        // Taken after the moment: not due again for it.
        let after = meta(key.clone(), touch + Duration::from_millis(1));
        assert!(!due(Some(&after), &key, region, &[touch], touch + Duration::from_secs(2)));
        // A predicted frame change in an hour, its fade after it.
        let frame = t0 + Duration::from_secs(3600) + crate::engine::patch_still::FADE;
        assert!(!due(Some(&m), &key, region, &[frame], t0 + Duration::from_secs(5)));
        assert!(due(Some(&m), &key, region, &[frame], frame));
    }

    #[test]
    fn a_predicted_frame_change_is_not_pushed_ahead_by_every_read() {
        let t0 = Instant::now();
        let at = |s: u64| Some(t0 + Duration::from_secs(s));
        let taken = at(0);
        assert_eq!(next_frame_mark(None, at(60), t0, taken), at(60));
        // Read again 2 s later: "in a minute" again; the first stands.
        assert_eq!(next_frame_mark(at(60), at(62), t0 + Duration::from_secs(2), taken), at(60));
        assert_eq!(next_frame_mark(at(60), at(30), t0 + Duration::from_secs(2), taken), at(30), "a sooner one");
        assert_eq!(next_frame_mark(at(60), None, t0 + Duration::from_secs(2), taken), at(60));
        // Come, but no still since (a switch ran): it stays due.
        assert_eq!(next_frame_mark(at(60), at(121), t0 + Duration::from_secs(61), taken), at(60));
        // Served by a still: the next prediction.
        assert_eq!(next_frame_mark(at(60), at(121), t0 + Duration::from_secs(62), at(61)), at(121));
        assert_eq!(next_frame_mark(at(60), None, t0 + Duration::from_secs(62), at(61)), None);
    }

    #[test]
    fn a_capture_waits_long_after_a_failure_and_briefly_after_a_still() {
        let t0 = Instant::now();
        assert!(may_try(None, false, t0));
        assert!(!may_try(Some(t0), false, t0 + Duration::from_millis(100)), "a burst of reasons: one still");
        assert!(may_try(Some(t0), false, t0 + MIN_GAP), "an icon moved right after a refresh: at once");
        assert!(!may_try(Some(t0), true, t0 + MIN_GAP), "after a failure");
        assert!(may_try(Some(t0), true, t0 + RETRY));
    }

    #[test]
    fn an_answer_older_than_the_still_kept_is_dropped() {
        let t0 = Instant::now();
        let kept = meta(Key::new(1, MAIN, Some(&[])), t0 + Duration::from_secs(6));
        assert!(newer(None, t0));
        assert!(!newer(Some(&kept), t0), "a stalled capture started before the kept still");
        assert!(!newer(Some(&kept), kept.taken));
        assert!(newer(Some(&kept), t0 + Duration::from_secs(7)));
    }

    #[test]
    fn with_no_still_the_reason_none_was_taken_is_given() {
        let ok = || Ok(());
        let no = || Err("an aerial");
        // A scripted run with captures off: the switch's at_once names it.
        assert_eq!(decide(Err(NO_STILL), Some("window captures are off"), ok), Err("window captures are off"));
        assert_eq!(decide(Err(NO_STILL), None, ok), Err(NO_STILL));
        assert_eq!(decide(Err("the desktop's still is too old"), Some("the screen is locked"), ok), Err("the desktop's still is too old"));
        assert_eq!(decide(Ok(Layers::Alone), None, no), Ok(()), "a whole still needs no wallpaper");
        assert_eq!(decide(Ok(Layers::OverWallpaper), None, ok), Ok(()));
        assert_eq!(
            decide(Ok(Layers::OverWallpaper), None, no),
            Err("the desktop's still holds the icons only and the patch has no picture of the wallpaper")
        );
    }
}
