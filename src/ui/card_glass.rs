//! The wallpaper behind the strip's cards, for their glass (`[cards] window_opacity`, on by
//! default: Apple's cards let the wallpaper through; the maths is [`super::glass_math`]).
//! Main thread, but for the render. One backdrop per screen (round displays-2): each
//! screen's strip samples that screen's own desktop picture, keyed as the strip is
//! ([`strip_model::MAIN_STRIP`] for the main display's, else the display's id).
//!
//! - **Source**: the desktop picture drawn from its file by the corner patch's reader and
//!   renderer ([`crate::platform::wallpaper`]: no screen capture, no Screen Recording), the
//!   whole screen at [`SAMPLE_SCALE`] pixels a point, on a thread of its own, once per
//!   picture (its file, frame, placement, screen and colour space: the job's key). The
//!   pixels become a [`Backdrop`] in the display's colour space, so a card's under is the
//!   colour the window server shows.
//! - **Reads** while the glass is wanted: at the app's poll every [`READ_EVERY`] (cheap: the
//!   reader keeps what only changes with a file), when the clock's frame boundary passes or
//!   the appearance changed, and [`SETTLE`] after a Space change (at the change itself
//!   NSWorkspace may still name the Desktop left's picture; the arriving strip keeps the
//!   colours it had there meanwhile). A new backdrop recolours the strip shown
//!   ([`super::strip::glass_changed`]).
//! - **No picture** (one that cannot be drawn: a shuffle, a gap with no fill colour, an
//!   unreadable file): no backdrop, and the cards are drawn opaque, as before the glass.
//! - **Cost**: nothing a frame. A card's under is one flat colour (four lookups in the
//!   backdrop's summed table when a card is laid out); the render decodes the picture once
//!   per change, off the main thread.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2_app_kit::NSScreen;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGColor, CGColorSpace};
use objc2_foundation::MainThreadMarker;

use super::glass_math::Backdrop;
use super::strip_model;
use crate::engine::geometry::Rect;
use crate::platform::wallpaper::{self, Job, Reader};

/// Pixels a point the backdrop is drawn at: a card's thumbnail (about 100 × 60 points) spans
/// a few hundred cells, and the whole screen is a few hundred pixels wide.
pub const SAMPLE_SCALE: f64 = 0.25;
/// How often the wallpaper is read while the glass is wanted.
pub const READ_EVERY: Duration = Duration::from_secs(2);
/// How long after a Space change the wallpaper is read.
pub const SETTLE: Duration = Duration::from_millis(400);

/// The backdrop shown now: the wallpaper's grid and the display's colour space.
pub struct Shown {
    pub backdrop: Backdrop,
    space: Option<Retained<CGColorSpace>>,
}

impl Shown {
    /// `rgb` (0–1, the display's encoded values) as a colour in the display's space.
    pub fn color(&self, rgb: [f64; 3]) -> CFRetained<CGColor> {
        let components = [rgb[0], rgb[1], rgb[2], 1.0];
        // SAFETY: four components (RGB and alpha) for an RGB space, alive for the call.
        let made = self.space.as_deref().and_then(|s| unsafe { CGColor::new(Some(s), components.as_ptr()) });
        made.unwrap_or_else(|| CGColor::new_srgb(rgb[0], rgb[1], rgb[2], 1.0))
    }
}

/// One screen's glass: its reader, whether it is wanted, and the backdrop shown.
#[derive(Default)]
struct Screen {
    reader: Reader,
    /// Its strip's glass is on.
    wanted: bool,
    /// The last read: when, its clock boundary, its appearance.
    read: Option<(Instant, Option<Instant>, bool)>,
    /// The picture rendered or being rendered (the job's key at [`SAMPLE_SCALE`]).
    key: Option<String>,
    shown: Option<Rc<Shown>>,
}

/// Every screen's glass, by the strip's display key ([`strip_model::MAIN_STRIP`] for the
/// main display's).
#[derive(Default)]
struct State {
    screens: BTreeMap<u32, Screen>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// The backdrop `display`'s strip colours its cards from, when there is one.
pub fn current(display: u32) -> Option<Rc<Shown>> {
    STATE.with(|s| s.try_borrow().ok().and_then(|s| s.screens.get(&display).and_then(|d| d.shown.clone())))
}

/// `display`'s strip's glass is `on` (its style, at each snapshot): a first read happens at
/// the next poll.
pub fn set_wanted(display: u32, on: bool) {
    STATE.with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            s.screens.entry(display).or_default().wanted = on;
        }
    });
}

/// The displays changed (`kept`: the strips' display keys still there): the glass of a
/// screen gone is forgotten.
pub fn displays_changed(kept: &[u32]) {
    STATE.with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            s.screens.retain(|d, _| kept.contains(d));
        }
    });
}

/// The screens whose glass is wanted now.
fn wanted() -> Vec<u32> {
    STATE.with(|s| s.try_borrow().map(|s| s.screens.iter().filter(|(_, d)| d.wanted).map(|(k, _)| *k).collect()).unwrap_or_default())
}

/// The app's poll (each second): reads each screen's wallpaper again when its last read is
/// stale.
pub fn poll(mtm: MainThreadMarker) {
    let now = Instant::now();
    let dark = wallpaper::dark_appearance(mtm);
    for display in wanted() {
        let stale = STATE.with(|s| {
            s.try_borrow().is_ok_and(|s| {
                s.screens.get(&display).is_some_and(|d| {
                    d.read.is_none_or(|(at, recheck, was_dark)| now.duration_since(at) >= READ_EVERY || recheck.is_some_and(|r| now >= r) || was_dark != dark)
                })
            })
        });
        if stale {
            read(mtm, display);
        }
    }
}

/// The user went to another Space: every screen's wallpaper is read [`SETTLE`] later. The
/// same picture there repaints the strip shown at once (cards laid out ahead while its
/// Desktop was away are painted then); another one repaints it once rendered.
pub fn space_changed() {
    let when = dispatch2::DispatchTime::try_from(SETTLE).unwrap_or(dispatch2::DispatchTime::NOW);
    let _ = dispatch2::DispatchQueue::main().after(when, || {
        let Some(mtm) = MainThreadMarker::new() else { return };
        for display in wanted() {
            if read(mtm, display) == Read::Same {
                super::strip::glass_changed(display);
            }
        }
    });
}

/// The screen of a strip's display key: the first screen for [`strip_model::MAIN_STRIP`],
/// else the screen with that `CGDirectDisplayID` ([`crate::platform::desktop::screen_number`]).
fn screen_for(mtm: MainThreadMarker, display: u32) -> Option<Retained<NSScreen>> {
    let screens = NSScreen::screens(mtm);
    if display == strip_model::MAIN_STRIP {
        return screens.firstObject();
    }
    crate::platform::desktop::screen_with_id(mtm, display)
}

/// What a read of the wallpaper found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// The picture of the backdrop kept (or none, as before).
    Same,
    /// Another picture: rendering, or cleared.
    Changed,
    /// Not read (busy).
    Busy,
}

/// Reads `display`'s wallpaper; a new picture is rendered, one that cannot be drawn clears
/// the backdrop.
fn read(mtm: MainThreadMarker, strip: u32) -> Read {
    let mut outcome = Read::Busy;
    let screen = screen_for(mtm, strip);
    let job = STATE.with(|s| {
        let Ok(mut s) = s.try_borrow_mut() else { return None };
        let d = s.screens.get_mut(&strip)?;
        outcome = Read::Same;
        let wall = match &screen {
            Some(screen) => d.reader.read_screen(mtm, screen),
            None => d.reader.read(mtm),
        };
        d.read = Some((Instant::now(), wall.recheck_at, wall.dark));
        match wall.outcome {
            Ok((_, job)) => {
                let job = Job { scale: SAMPLE_SCALE, ..job };
                let key = job.key();
                if d.key.as_deref() == Some(key.as_str()) {
                    return None;
                }
                outcome = Read::Changed;
                tracing::debug!(screen = strip, picture = %job.file_name(), frame = job.index, "card glass: rendering the wallpaper");
                d.key = Some(key);
                Some((job, wall.screen))
            }
            Err(why) => {
                if d.key.take().is_some() || d.shown.is_some() {
                    outcome = Read::Changed;
                    tracing::debug!(screen = strip, why, "card glass: the wallpaper cannot be drawn; the cards are opaque");
                    d.shown = None;
                    dispatch2::DispatchQueue::main().exec_async(move || super::strip::glass_changed(strip));
                }
                None
            }
        }
    });
    if let Some((job, screen)) = job {
        start_render(strip, job, screen);
    }
    outcome
}

/// Renders the backdrop on a thread of its own and hands it to the main thread.
fn start_render(strip: u32, job: Job, screen: Rect) {
    let key = job.key();
    let spawned = std::thread::Builder::new().name("card-glass-render".into()).spawn(move || {
        let started = Instant::now();
        let result = crate::platform::safety::guarded("the card glass's render", || backdrop_of(&job, screen))
            .unwrap_or_else(|| Err("the render panicked".to_string()));
        tracing::debug!(ms = started.elapsed().as_millis() as u64, ok = result.is_ok(), "card glass: rendered");
        let space = job.space.clone();
        dispatch2::DispatchQueue::main().exec_async(move || deliver(strip, key, result.map(|backdrop| Shown { backdrop, space })));
    });
    if let Err(e) = spawned {
        tracing::warn!("card glass: cannot start a render: {e}");
    }
}

/// The whole screen's picture under `job` as a backdrop (any thread); `screen` is the
/// screen's frame (global, top-left), which the backdrop answers in.
fn backdrop_of(job: &Job, screen: Rect) -> Result<Backdrop, String> {
    let image = wallpaper::render(job, Rect::new(0.0, 0.0, screen.w, screen.h))?;
    let (bytes, w, h) = crate::platform::pixels::bgra(&image, job.space.as_deref()).ok_or("the render cannot be read")?;
    Backdrop::from_bgra(&bytes, w, h, w * 4, screen).ok_or_else(|| "an empty render".to_string())
}

/// A render arrived for `display` (main thread): kept when it is still the picture read
/// last.
fn deliver(strip: u32, key: String, result: Result<Shown, String>) {
    let changed = STATE.with(|s| {
        let Ok(mut s) = s.try_borrow_mut() else { return false };
        let Some(d) = s.screens.get_mut(&strip) else { return false };
        if d.key.as_deref() != Some(key.as_str()) {
            return false;
        }
        match result {
            Ok(shown) => d.shown = Some(Rc::new(shown)),
            Err(why) => {
                tracing::debug!(screen = strip, why, "card glass: the wallpaper's render failed; the cards are opaque");
                d.shown = None;
            }
        }
        true
    });
    if changed {
        super::strip::glass_changed(strip);
    }
}

/// `stage-manager probe-glass --rect X,Y,W,H [--rect ...]`: the wallpaper behind each
/// rectangle (global points, top-left) as the cards' glass sees it (the same read and render,
/// from the picture's file; nothing captured, nothing shown, no app needs to run), and the
/// under the default glass makes of it, 0–255 in the display's values, one JSON line.
/// `scripts/measure-apple-card-glass.py` sets it beside Apple's cards.
pub fn probe(args: &[String]) -> std::process::ExitCode {
    use serde_json::json;
    let usage = || {
        eprintln!("usage: stage-manager probe-glass --rect X,Y,W,H [--rect X,Y,W,H ...]");
        std::process::ExitCode::from(2)
    };
    let mut rects = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (a.as_str(), it.next()) {
            ("--rect", Some(r)) => {
                let v: Vec<f64> = r.split(',').filter_map(|n| n.trim().parse().ok()).collect();
                let [x, y, w, h] = v[..] else { return usage() };
                rects.push(Rect::new(x, y, w, h));
            }
            _ => return usage(),
        }
    }
    if rects.is_empty() {
        return usage();
    }
    let Some(mtm) = MainThreadMarker::new() else { return std::process::ExitCode::FAILURE };
    let _app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let wall = Reader::default().read(mtm);
    let (job, screen) = match wall.outcome {
        Ok((_, job)) => (Job { scale: SAMPLE_SCALE, ..job }, wall.screen),
        Err(why) => {
            println!("{}", json!({ "ok": false, "why": why }));
            return std::process::ExitCode::FAILURE;
        }
    };
    let backdrop = match backdrop_of(&job, screen) {
        Ok(b) => b,
        Err(why) => {
            println!("{}", json!({ "ok": false, "why": why }));
            return std::process::ExitCode::FAILURE;
        }
    };
    let levels = |c: [f64; 3]| c.map(|v| (v * 255.0 * 100.0).round() / 100.0);
    let glass = super::glass_math::Glass::APPLE;
    let out: Vec<serde_json::Value> = rects
        .iter()
        .map(|r| {
            let mean = backdrop.mean(*r);
            json!({ "rect": r, "wallpaper": mean.map(levels), "under": mean.map(|m| levels(glass.under(m))) })
        })
        .collect();
    let space = job.space.as_deref().map(|s| CGColorSpace::name(Some(s)).map_or_else(|| "unnamed".to_string(), |n| n.to_string()));
    println!(
        "{}",
        json!({ "ok": true, "picture": job.file_name(), "frame": job.index, "dark": wall.dark, "space": space, "screen": screen, "rects": out })
    );
    std::process::ExitCode::SUCCESS
}
