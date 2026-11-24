//! Pictures taken at a switch (1.9, S4b): full-size stills of a few windows, now, outside any
//! capturer's rounds.
//!
//! Under the Corner engine the switch's pictures come from a capturer that keeps every window
//! of the active stage fresh every `[animation] snapshot_interval_ms` (a Dock click or ⌘Tab gives
//! no warning). Under the Spaces engine a window leaves the screen in one window-server step
//! (~2 ms), so it can stay where it is until its picture is taken at the switch, and a window
//! in a stage Space not shown is still captured with its current content (measured 11-14,
//! `probe-spaces capture`): [`shoot`] takes both, in parallel, each answered once.
//!
//! It follows the capture module's rules ([`capture::desktop_may_capture`]): captures on, not
//! quitting, Screen Recording granted (never asked for here), and window captures already
//! worked in this run, so a switch is never the process's probe and never raises an alert.
//! Each still counts in the day's log and in [`capture::quiesce`]'s wait like any other.
//!
//! **The window server first** (S6b): each window is taken by [`super::capture_fast`] (one call,
//! ~13 ms, no listing), the windows one after another on a thread of the switch's own, so the
//! WM thread's wait stays bounded; a window that source misses goes through ScreenCaptureKit as
//! below. Four windows: ~55 ms instead of ~145 ms (S4c's timings; not measured live here). The
//! thread counts as one capture in flight for its whole run and issues nothing once quit has
//! begun, so [`capture::quiesce`] never sees none in flight between two of its windows (the
//! review's D-5).
//!
//! **The listing** (ScreenCaptureKit only). Every such capture starts from a listing of the
//! shareable content (every window of the system: ~50 ms on a busy screen, 11-14). The last listing any capture made
//! (the cards' rounds every 1.5 s, the pointer's prefetch, a switch's) is remembered
//! ([`remember`]); a switch reuses it when it lists every window asked for at the frame the
//! window server gives it now, and lists afresh otherwise, or when a window has a window of its
//! app over it (why the window server's call missed it, S6b): its picture is planned from the
//! app's other windows in the listing (a popover reaching out, [`capture::window_shot`]), which
//! the frames checked do not cover (the review's D-4). Measured: the pictures came with
//! the content the windows had at the capture (a listing up to 8 s old, 0 of 40 wrong), 3
//! windows in ~81 ms instead of ~110 ms.

use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::Message;
use objc2::rc::{Retained, autoreleasepool};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{SCScreenshotManager, SCShareableContent};

use super::capture::{self, Deliver, InFlight, Picture, Target, WindowShot};
use crate::engine::geometry::Rect;
use crate::logbook::counters::{self, Count};

/// A remembered listing older than this is never reused.
const LISTING_MAX_AGE: Duration = Duration::from_secs(60);

/// How far a window's frame may be from what a remembered listing says (points).
const FRAME_SLACK: f64 = 0.5;

/// A listing of the shareable content and when it was made.
struct Listing {
    at: Instant,
    content: Retained<SCShareableContent>,
}

// SAFETY: a listing is an immutable snapshot (ScreenCaptureKit hands it to its completion
// handlers on its own queues); only its getters are called, never a setter.
unsafe impl Send for Listing {}
// SAFETY: as above: shared reads of an immutable object.
unsafe impl Sync for Listing {}

/// The last listing a capture made ([`remember`]).
static LAST: Mutex<Option<Arc<Listing>>> = Mutex::new(None);

/// Remembers `content`, a listing just made by a capture (any thread).
pub(crate) fn remember(content: &SCShareableContent) {
    let listing = Arc::new(Listing { at: Instant::now(), content: content.retain() });
    if let Ok(mut last) = LAST.lock() {
        *last = Some(listing);
    }
}

/// The remembered listing, when it is young enough and lists every one of `targets` at the
/// frame the window server gives it now (`now`: id and frame, top-left points).
fn reusable(targets: &[Target], now: &[(u32, Rect)]) -> Option<Arc<Listing>> {
    let listing = LAST.lock().ok()?.clone()?;
    if listing.at.elapsed() > LISTING_MAX_AGE {
        return None;
    }
    // SAFETY: plain getters on a live SCShareableContent and its SCWindows.
    let windows = unsafe { listing.content.windows() };
    let fits = targets.iter().all(|t| {
        let Some(frame) = now.iter().find(|(id, _)| *id == t.id).map(|(_, f)| *f) else { return false };
        // SAFETY: plain getters on a live SCWindow.
        windows.iter().find(|w| unsafe { w.windowID() } == t.id).is_some_and(|w| same_frame(capture::to_rect(unsafe { w.frame() }), frame))
    });
    fits.then_some(listing)
}

/// Two frames within [`FRAME_SLACK`] of each other on every edge.
fn same_frame(a: Rect, b: Rect) -> bool {
    [(a.x, b.x), (a.y, b.y), (a.w, b.w), (a.h, b.h)].iter().all(|(p, q)| (p - q).abs() <= FRAME_SLACK)
}

/// What [`shoot`] calls for each window it was asked for, exactly once, on a ScreenCaptureKit
/// queue: the picture, or none (gone, failed, refused, or a fill not to be shown).
pub type ShotDone = Arc<dyn Fn(u32, Option<Picture>) + Send + Sync>;

/// Where [`take`] hands each picture (the strip's full-size pictures, on the main thread).
static DELIVER: OnceLock<Deliver> = OnceLock::new();

/// The strip registers where a switch's pictures go (once, at startup).
pub fn set_deliver(deliver: Deliver) {
    let _ = DELIVER.set(deliver);
}

/// What [`take`] did, for the switch's log line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Taken {
    /// Windows asked for.
    pub asked: usize,
    /// Pictures handed to the strip before the wait ended.
    pub got: usize,
    /// Answers (a picture or none) before the wait ended.
    pub answered: usize,
    /// How long the wait took.
    pub ms: u64,
    /// Why nothing was asked (captures off, refused, not tried yet, no strip).
    pub skipped: Option<&'static str>,
}

/// Captures `targets` ([`shoot`]) and waits up to `max` for every answer (the WM thread, a
/// switch before it is sent to the strip). Each picture goes to the strip on the main thread
/// before the wait hears of it, so a snapshot presented after this returns finds every
/// picture that came in time; one later still reaches the strip (a moving picture of its
/// window cross-fades to it).
pub fn take(targets: Vec<Target>, max: Duration) -> Taken {
    let asked = targets.len();
    #[cfg(test)]
    if let Some(ms) = super::fake::with(|d| d.take_shots(asked, max)) {
        return Taken { asked, got: asked, answered: asked, ms, skipped: None };
    }
    let Some(&deliver) = DELIVER.get() else { return Taken { asked, skipped: Some("no strip"), ..Taken::default() } };
    if asked == 0 {
        return Taken::default();
    }
    let t0 = Instant::now();
    // (answers, pictures)
    let tally: Arc<(Mutex<(usize, usize)>, Condvar)> = Arc::new((Mutex::new((0, 0)), Condvar::new()));
    let heard = tally.clone();
    let done: ShotDone = Arc::new(move |id, picture| {
        let got = picture.is_some();
        if let Some(p) = picture {
            DispatchQueue::main().exec_async(move || deliver(id, p));
        }
        let (lock, cv) = &*heard;
        if let Ok(mut n) = lock.lock() {
            n.0 += 1;
            n.1 += usize::from(got);
            cv.notify_all();
        }
    });
    if let Err(why) = shoot(targets, done) {
        return Taken { asked, skipped: Some(why), ..Taken::default() };
    }
    let (lock, cv) = &*tally;
    let left = max.saturating_sub(t0.elapsed());
    let n = match lock.lock() {
        Ok(guard) => cv.wait_timeout_while(guard, left, |n| n.0 < asked).map_or((0, 0), |(n, _)| *n),
        Err(_) => (0, 0),
    };
    Taken { asked, got: n.1, answered: n.0, ms: t0.elapsed().as_millis() as u64, skipped: None }
}

/// Captures `targets` now, each at its size: through the window server where it can, one
/// window after another on a thread of its own ([`shoot_fast`]), the rest through
/// ScreenCaptureKit in parallel (one listing of the shareable content, or the remembered one,
/// then one still per window). `Err` (nothing issued, `done` never called) when captures may
/// not run now ([`capture::desktop_may_capture`]).
pub fn shoot(targets: Vec<Target>, done: ShotDone) -> Result<(), &'static str> {
    capture::desktop_may_capture()?;
    let ids: Vec<u32> = targets.iter().map(|t| t.id).collect();
    let now: Vec<(u32, Rect)> = super::windows::describe(&ids).iter().map(|w| (w.id, w.bounds)).collect();
    if super::capture_fast::enabled() {
        let (t, n, d) = (targets.clone(), now.clone(), done.clone());
        let spawned = spawn_counted(move || {
            autoreleasepool(|_| {
                let missed = shoot_fast(&t, &n, &d);
                shoot_sck(missed.targets, &n, d, missed.children);
            });
        });
        match spawned {
            Ok(_) => return Ok(()),
            Err(e) => tracing::debug!("switch pictures: no thread for the window server's capture ({e}); ScreenCaptureKit takes them"),
        }
    }
    shoot_sck(targets, &now, done, false);
    Ok(())
}

/// Runs `f` on a switch's own thread, counted as one capture in flight from before the thread
/// starts until `f` returns ([`capture::quiesce`] waits for it).
fn spawn_counted(f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    let flight = InFlight::issue();
    std::thread::Builder::new()
        .name("switch-pictures".into())
        .spawn(move || {
            let _whole_run = flight;
            f();
        })
        .map(|_| ())
}

/// What a switch's thread does with one window next ([`shoot_fast`]).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    /// Answered without a picture: quit has begun, or the window is closed.
    Answer,
    /// Left for ScreenCaptureKit: a window of its app over it.
    Left,
    /// Taken through the window server at this frame.
    Take(Rect),
}

/// The step for a window, read just before it is taken: `closing` (quit has begun), `children`
/// (a window of its app over it), its frame as the window server describes it now (`None`:
/// closed).
fn step(closing: bool, children: bool, frame: Option<Rect>) -> Step {
    match (closing, children, frame) {
        (true, ..) => Step::Answer,
        (false, true, _) => Step::Left,
        (false, false, None) => Step::Answer,
        (false, false, Some(f)) => Step::Take(f),
    }
}

/// The remembered listing to start from: none for windows with a window of their app over them
/// (`children`: their plan reads the app's other windows from the listing, which [`reusable`]
/// does not check), else what `reusable` finds.
fn kept_listing<L>(children: bool, reusable: impl FnOnce() -> Option<L>) -> Option<L> {
    if children { None } else { reusable() }
}

/// Takes `targets` through ScreenCaptureKit: from the remembered listing when it lists every
/// one where the window server has it now (`now`) and none has a window of its app over it
/// (`children`), else from a new listing. Nothing once quit has begun: each answers none.
fn shoot_sck(targets: Vec<Target>, now: &[(u32, Rect)], done: ShotDone, children: bool) {
    if targets.is_empty() {
        return;
    }
    if capture::closing() {
        targets.iter().for_each(|t| done(t.id, None));
        return;
    }
    match kept_listing(children, || reusable(&targets, now)) {
        Some(listing) => autoreleasepool(|_| shoot_from(&listing.content, &targets, &done)),
        None => issue(targets, done),
    }
}

/// Takes each of `targets` through the window server ([`super::capture_fast`]) at its frame in
/// `now`, one after another; each picture, and each window no longer described (closed),
/// answers `done`. The windows it misses, for ScreenCaptureKit; all of them when the window
/// server described none ([`super::capture_fast::closed`]: a failed read).
pub(crate) fn shoot_fast(targets: &[Target], now: &[(u32, Rect)], done: &ShotDone) -> Missed {
    let ids: Vec<u32> = targets.iter().map(|t| t.id).collect();
    if super::capture_fast::closed(&ids, &now.iter().map(|(id, _)| *id).collect::<Vec<_>>()).is_none() {
        tracing::debug!(windows = ids.len(), "switch pictures: the window server described none of the windows; ScreenCaptureKit takes them");
        return Missed { targets: targets.to_vec(), children: false };
    }
    let displays = super::desktop::cg_display_bounds();
    let children = super::capture_fast::with_children(&ids, &displays);
    let mut left = Vec::new();
    for t in targets {
        let frame = now.iter().find(|(id, _)| *id == t.id).map(|(_, f)| *f);
        let frame = match step(capture::closing(), children.contains(&t.id), frame) {
            Step::Take(frame) => frame,
            Step::Left => {
                left.push(*t);
                continue;
            }
            Step::Answer => {
                tracing::trace!(window = t.id, "switch pictures: quitting, or the window server no longer describes the window");
                done(t.id, None);
                continue;
            }
        };
        match super::capture_fast::picture(*t, frame, &displays) {
            Ok(picture) => {
                counters::bump(Count::CaptureOk);
                capture::IMAGES.fetch_add(1, Ordering::Relaxed);
                done(t.id, Some(picture));
            }
            Err(miss) => {
                tracing::trace!(window = t.id, why = miss.as_str(), "switch picture: ScreenCaptureKit takes it");
                left.push(*t);
            }
        }
    }
    Missed { children: left.iter().any(|t| children.contains(&t.id)), targets: left }
}

/// The windows [`shoot_fast`] left for ScreenCaptureKit.
pub(crate) struct Missed {
    /// The windows.
    pub targets: Vec<Target>,
    /// One of them has a window of its app over it: a new listing for them ([`kept_listing`]).
    pub children: bool,
}

/// [`shoot`] without its checks, always with a new listing (remembered for the next one).
pub(crate) fn issue(targets: Vec<Target>, done: ShotDone) {
    if targets.is_empty() {
        return;
    }
    let listing = InFlight::issue();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let _answered = &listing;
        autoreleasepool(|_| {
            // SAFETY: SCK passes a valid object or null for each argument.
            let (content, error) = unsafe { (content.as_ref(), error.as_ref()) };
            let Some(content) = content else {
                let why = error.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
                tracing::debug!(why, "switch pictures: no shareable content");
                for t in &targets {
                    counters::bump(Count::CaptureFailed);
                    done(t.id, None);
                }
                return;
            };
            remember(content);
            if capture::closing() {
                targets.iter().for_each(|t| done(t.id, None));
                return;
            }
            shoot_from(content, &targets, &done);
        });
    });
    capture::listing_asked();
    // SAFETY: the handler matches the documented signature; SCK copies the block.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true, false, &handler);
    }
}

/// Captures `targets` with the windows as `content` lists them (a listing made now, or one
/// remembered whose windows are where it says); each answers `done` once.
pub(crate) fn shoot_from(content: &SCShareableContent, targets: &[Target], done: &ShotDone) {
    // SAFETY: plain getters on a live SCShareableContent.
    let windows = unsafe { content.windows() };
    let displays = capture::display_frames(content);
    for t in targets {
        // SAFETY: a plain getter on a live SCWindow.
        let Some(window) = windows.iter().find(|w| unsafe { w.windowID() } == t.id) else {
            tracing::trace!(window = t.id, "switch pictures: window not in the shareable content");
            done(t.id, None);
            continue;
        };
        let shot = capture::window_shot(&window, &windows, &displays, (t.width, t.height));
        // SAFETY: plain setters on a live configuration.
        unsafe {
            shot.config.setShowsCursor(false);
            shot.config.setIgnoreShadowsSingleWindow(true);
            shot.config.setScalesToFit(true);
        }
        let WindowShot { filter, config, clear, extent, overlaps, size_pt, children_left_out, .. } = shot;
        let (id, done) = (t.id, done.clone());
        let flight = InFlight::issue();
        let on_image = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
            let _answered = &flight;
            // The configuration's background, alive until the capture answered.
            let _background = &clear;
            match NonNull::new(image) {
                Some(ptr) => {
                    // SAFETY: SCK hands a valid CGImage for the duration of the block;
                    // retaining keeps it alive past it.
                    let image = unsafe { CFRetained::retain(ptr) };
                    counters::bump(Count::CaptureOk);
                    let fill = !overlaps.is_empty() && capture::fill_check(id, &image, extent, &overlaps, size_pt, children_left_out);
                    if fill {
                        done(id, None);
                    } else {
                        capture::IMAGES.fetch_add(1, Ordering::Relaxed);
                        done(id, Some(Picture { image, extent }));
                    }
                }
                None => {
                    counters::bump(Count::CaptureFailed);
                    // SAFETY: SCK passes a valid NSError or null.
                    let why = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
                    tracing::trace!(window = id, ?why, "switch picture failed");
                    done(id, None);
                }
            }
        });
        // SAFETY: filter, configuration and handler are valid; SCK copies the block.
        unsafe {
            SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(&filter, &config, Some(&on_image));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-5: once quit has begun a switch's thread takes no further window, whatever the window;
    /// otherwise a window of its app over it goes to ScreenCaptureKit and a closed one answers.
    #[test]
    fn a_switch_thread_takes_nothing_once_quit_has_begun() {
        let f = Rect::new(0.0, 0.0, 800.0, 600.0);
        assert_eq!(step(true, false, Some(f)), Step::Answer);
        assert_eq!(step(true, true, Some(f)), Step::Answer);
        assert_eq!(step(false, true, Some(f)), Step::Left);
        assert_eq!(step(false, false, None), Step::Answer);
        assert_eq!(step(false, false, Some(f)), Step::Take(f));
    }

    /// D-5: the thread counts as a capture in flight while it runs, so a quit waits for it.
    #[test]
    fn a_switch_thread_counts_as_a_capture_in_flight_while_it_runs() {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_counted(move || {
            let _ = tx.send(capture::in_flight());
        })
        .expect("a thread");
        let seen = rx.recv_timeout(Duration::from_secs(5)).expect("the thread ran");
        assert!(seen >= 1, "in flight while it runs: {seen}");
    }

    /// D-4: windows with a window of their app over them never start from a remembered listing
    /// (it is not even looked at); the others do when it fits.
    #[test]
    fn a_window_with_a_child_over_it_gets_a_new_listing() {
        assert_eq!(kept_listing(true, || -> Option<u8> { panic!("not looked at") }), None);
        assert_eq!(kept_listing(false, || Some(7)), Some(7));
        assert_eq!(kept_listing(false, || None::<u8>), None);
    }

    /// A remembered listing is reused only while every window asked for is where it says,
    /// to half a point.
    #[test]
    fn a_window_moved_since_the_listing_needs_a_new_one() {
        let at = Rect::new(100.0, 50.0, 800.0, 600.0);
        assert!(same_frame(at, Rect::new(100.4, 50.0, 800.0, 600.0)));
        assert!(!same_frame(at, Rect::new(101.0, 50.0, 800.0, 600.0)), "moved a point");
        assert!(!same_frame(at, Rect::new(100.0, 50.0, 800.0, 640.0)), "resized");
    }
}
