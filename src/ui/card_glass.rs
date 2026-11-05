//! The wallpaper behind the strip's cards, for their glass (`[cards] window_opacity`, on by
//! default: Apple's cards let the wallpaper through; the maths is [`super::glass_math`]).
//! Main thread, but for the render.
//!
//! - **Source**: the desktop picture drawn from its file by the corner patch's reader and
//!   renderer ([`crate::platform::wallpaper`]: no screen capture, no Screen Recording), the
//!   whole main screen at [`SAMPLE_SCALE`] pixels a point, on a thread of its own, once per
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
use std::rc::Rc;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGColor, CGColorSpace};
use objc2_foundation::MainThreadMarker;

use super::glass_math::Backdrop;
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

#[derive(Default)]
struct State {
    reader: Reader,
    /// The strip's glass is on.
    wanted: bool,
    /// The last read: when, its clock boundary, its appearance.
    read: Option<(Instant, Option<Instant>, bool)>,
    /// The picture rendered or being rendered (the job's key at [`SAMPLE_SCALE`]).
    key: Option<String>,
    shown: Option<Rc<Shown>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// The backdrop to colour cards from, when there is one.
pub fn current() -> Option<Rc<Shown>> {
    STATE.with(|s| s.try_borrow().ok().and_then(|s| s.shown.clone()))
}

/// The strip's glass is `on` (its style, at each snapshot): a first read happens at the next
/// poll.
pub fn set_wanted(on: bool) {
    STATE.with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            s.wanted = on;
        }
    });
}

/// The app's poll (each second): reads the wallpaper again when the last read is stale.
pub fn poll(mtm: MainThreadMarker) {
    let now = Instant::now();
    let stale = STATE.with(|s| {
        s.try_borrow().is_ok_and(|s| {
            s.wanted
                && s.read.is_none_or(|(at, recheck, dark)| {
                    now.duration_since(at) >= READ_EVERY || recheck.is_some_and(|r| now >= r) || dark != wallpaper::dark_appearance(mtm)
                })
        })
    });
    if stale {
        read(mtm);
    }
}

/// The user went to another Space: the wallpaper is read [`SETTLE`] later. The same picture
/// there repaints the strip shown at once (cards laid out ahead while its Desktop was away are
/// painted then); another one repaints it once rendered.
pub fn space_changed() {
    let when = dispatch2::DispatchTime::try_from(SETTLE).unwrap_or(dispatch2::DispatchTime::NOW);
    let _ = dispatch2::DispatchQueue::main().after(when, || {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let wanted = STATE.with(|s| s.try_borrow().is_ok_and(|s| s.wanted));
        if wanted && read(mtm) == Read::Same {
            super::strip::glass_changed();
        }
    });
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

/// Reads the wallpaper; a new picture is rendered, one that cannot be drawn clears the
/// backdrop.
fn read(mtm: MainThreadMarker) -> Read {
    let mut outcome = Read::Busy;
    let job = STATE.with(|s| {
        let Ok(mut s) = s.try_borrow_mut() else { return None };
        outcome = Read::Same;
        let wall = s.reader.read(mtm);
        s.read = Some((Instant::now(), wall.recheck_at, wall.dark));
        match wall.outcome {
            Ok((_, job)) => {
                let job = Job { scale: SAMPLE_SCALE, ..job };
                let key = job.key();
                if s.key.as_deref() == Some(key.as_str()) {
                    return None;
                }
                outcome = Read::Changed;
                tracing::debug!(picture = %job.file_name(), frame = job.index, "card glass: rendering the wallpaper");
                s.key = Some(key);
                Some((job, wall.screen))
            }
            Err(why) => {
                if s.key.take().is_some() || s.shown.is_some() {
                    outcome = Read::Changed;
                    tracing::debug!(why, "card glass: the wallpaper cannot be drawn; the cards are opaque");
                    s.shown = None;
                    dispatch2::DispatchQueue::main().exec_async(super::strip::glass_changed);
                }
                None
            }
        }
    });
    if let Some((job, screen)) = job {
        start_render(job, screen);
    }
    outcome
}

/// Renders the backdrop on a thread of its own and hands it to the main thread.
fn start_render(job: Job, screen: Rect) {
    let key = job.key();
    let spawned = std::thread::Builder::new().name("card-glass-render".into()).spawn(move || {
        let started = Instant::now();
        let result = crate::platform::safety::guarded("the card glass's render", || backdrop_of(&job, screen))
            .unwrap_or_else(|| Err("the render panicked".to_string()));
        tracing::debug!(ms = started.elapsed().as_millis() as u64, ok = result.is_ok(), "card glass: rendered");
        let space = job.space.clone();
        dispatch2::DispatchQueue::main().exec_async(move || deliver(key, result.map(|backdrop| Shown { backdrop, space })));
    });
    if let Err(e) = spawned {
        tracing::warn!("card glass: cannot start a render: {e}");
    }
}

/// The whole screen's picture under `job` as a backdrop (any thread).
fn backdrop_of(job: &Job, screen: Rect) -> Result<Backdrop, String> {
    let image = wallpaper::render(job, Rect::new(0.0, 0.0, screen.w, screen.h))?;
    let (bytes, w, h) = crate::platform::pixels::bgra(&image, job.space.as_deref()).ok_or("the render cannot be read")?;
    Backdrop::from_bgra(&bytes, w, h, w * 4, screen).ok_or_else(|| "an empty render".to_string())
}

/// A render arrived (main thread): kept when it is still the picture read last.
fn deliver(key: String, result: Result<Shown, String>) {
    let changed = STATE.with(|s| {
        let Ok(mut s) = s.try_borrow_mut() else { return false };
        if s.key.as_deref() != Some(key.as_str()) {
            return false;
        }
        match result {
            Ok(shown) => s.shown = Some(Rc::new(shown)),
            Err(why) => {
                tracing::debug!(why, "card glass: the wallpaper's render failed; the cards are opaque");
                s.shown = None;
            }
        }
        true
    });
    if changed {
        super::strip::glass_changed();
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
