//! Settling a solar frame for the corner patch near a frame change (main thread state, its
//! render thread, its still): the platform half of [`crate::engine::solar_settle`], driven
//! by [`super::corner_patch`].
//!
//! - **Candidates** ([`Settler::settle`]): for the picture's candidate frames
//!   ([`crate::engine::wallpaper::Decision::candidates`]) the patch's block is rendered once
//!   per frame on a thread of its own, each render kept with its [`Thumb`]; whether they look
//!   alike is remembered by picture, block and frames, so another Desktop's panel left drawn
//!   at one of them is kept only while they do ([`Settler::alike_for`]).
//! - **A still** ([`Settler::settle`] taking one): when they differ, one still of the block
//!   through ScreenCaptureKit ([`crate::platform::capture::capture_desktop`]: the desktop's own
//!   windows only, never a window, never ours; only where window captures already work, and
//!   never asked for), drawn in the display's colour space and compared on the same grid
//!   ([`crate::engine::solar_settle::best_match`], Finder's icons masked out). At most one in
//!   flight; again after [`solar_settle::RECHECK`] while the frame is still ambiguous and the
//!   patch is wanted. The still only picks the frame: the patch draws the file's render.
//! - **Log**: which way the frame settled, once per change, at info; off and why through the
//!   patch's own line.

use std::collections::HashMap;
use std::time::Instant;

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;

use crate::engine::geometry::{Rect, Size};
use crate::engine::solar_settle::{self, Facts, Step, Thumb, Way};
use crate::engine::wallpaper as wp;
use crate::platform::capture;
use crate::platform::wallpaper::{self, Job};

/// The most alike verdicts remembered (one per picture, block and frames met).
const MAX_VERDICTS: usize = 64;

/// One candidate frame's render of the block.
struct Candidate {
    index: usize,
    image: CFRetained<CGImage>,
    thumb: Thumb,
}

/// The candidates' renders for one picture, block and set of frames.
pub struct Set {
    key: String,
    region: Rect,
    frames: Vec<Candidate>,
    /// They look alike ([`solar_settle::alike`]), judged as they came.
    alike: Option<bool>,
}

/// The last still for a picture: the frame it matched or why none, and when it was started.
struct LastStill {
    base: String,
    result: Result<usize, &'static str>,
    at: Instant,
}

/// What came back to the main thread ([`super::corner_patch`] hands it to [`Settler::news`]).
pub enum News {
    /// The candidates' renders for set `key`, or why not.
    Rendered {
        /// The set's key ([`set_key`]).
        key: String,
        /// The renders.
        result: Result<Set, String>,
    },
    /// A still started with `token` for set `key` of picture `base`: its thumb, or why none.
    Still {
        /// Which still.
        token: u64,
        /// The picture ([`Job::base_key`]).
        base: String,
        /// The set it was taken for.
        key: String,
        /// The block taken (global, top-left).
        global: Rect,
        /// The still as a thumb and its colour space's name, or why none.
        result: Result<(Option<Thumb>, String), String>,
    },
}

/// What to draw near a solar frame change.
pub enum Outcome {
    /// This frame (the job at it), with its render of the block (screen-local) when there is one.
    Draw {
        /// The job at the settled frame.
        job: Box<Job>,
        /// That frame's render and its region.
        picture: Option<(CFRetained<CGImage>, Rect)>,
    },
    /// Nothing yet: the candidates render.
    Wait,
    /// Off, and why.
    Off(String),
}

/// The set of `candidates` of picture `base` for `region`: the frames in order, so a set
/// whose first frame changed is the same set.
pub fn set_key(base: &str, region: Rect, candidates: &[usize]) -> String {
    let mut c = candidates.to_vec();
    c.sort_unstable();
    format!("{base}|{region:?}|{c:?}")
}

/// The frames as a log shows them.
fn frames(c: &[usize]) -> String {
    c.iter().map(usize::to_string).collect::<Vec<_>>().join(", ")
}

/// The settling state (main thread).
#[derive(Default)]
pub struct Settler {
    set: Option<Set>,
    /// The set whose renders are in flight, and the one whose renders failed.
    rendering: Option<String>,
    failed: Option<String>,
    /// Whether a set's candidates look alike, by [`set_key`].
    verdicts: HashMap<String, bool>,
    last_still: Option<LastStill>,
    /// The still in flight: its token and start.
    in_flight: Option<(u64, Instant)>,
    token: u64,
    /// The settled line last logged.
    said: Option<String>,
}

impl Settler {
    /// Drops the renders (a display change); verdicts and the last still stay keyed.
    pub fn clear(&mut self) {
        self.set = None;
        self.rendering = None;
    }

    /// Whether the candidates `c` of picture `base` look alike over `region`, when known.
    pub fn alike_for(&self, base: &str, region: Rect, c: &[usize]) -> Option<bool> {
        self.verdicts.get(&set_key(base, region, c)).copied()
    }

    /// The frame to draw for `job` (its frame the first of `candidates`) over `region` of the
    /// screen at `screen` (global), starting the renders or a still as due; `may_take` says
    /// whether a still may be taken now.
    pub fn settle(&mut self, job: &Job, candidates: &[usize], screen: Rect, region: Rect, may_take: Result<(), &'static str>, now: Instant) -> Outcome {
        let base = job.base_key();
        let key = set_key(&base, region, candidates);
        if self.set.as_ref().is_none_or(|s| s.key != key) {
            if self.failed.as_deref() == Some(key.as_str()) {
                self.said = None;
                return Outcome::Off(format!("{} (frames {}); the candidate frames cannot be drawn", wp::NEAR_CHANGE, frames(candidates)));
            }
            if self.rendering.as_deref() != Some(key.as_str()) {
                self.rendering = Some(key.clone());
                start_renders(job.clone(), candidates.to_vec(), region, key);
            }
            return Outcome::Wait;
        }
        let Some((set_region, alike, renders)) =
            self.set.as_ref().map(|s| (s.region, s.alike, s.frames.iter().map(|c| (c.index, c.image.clone())).collect::<Vec<_>>()))
        else {
            return Outcome::Wait;
        };
        let in_flight = self.in_flight.is_some_and(|(_, at)| now.saturating_duration_since(at) < solar_settle::STALL);
        let last = self.last_still.as_ref().filter(|l| l.base == base).map(|l| (l.result, now.saturating_duration_since(l.at)));
        let step = solar_settle::next(&Facts { candidates, alike, last_still: last, in_flight, may_take });
        let take = matches!(step, Step::Draw { take: true, .. } | Step::Off { take: true, .. });
        let global = set_region.translate(screen.x, screen.y);
        let outcome = match step {
            Step::Render => Outcome::Wait,
            Step::Draw { index, way, .. } => {
                let line = match way {
                    Way::Alike => format!("a solar frame settled: frames {} look alike in the corner; drawing {index}", frames(candidates)),
                    Way::Still => format!("a solar frame settled by a still of the desktop: frame {index} of {}", frames(candidates)),
                };
                if self.said.as_deref() != Some(line.as_str()) {
                    tracing::info!("corner patch: {line}");
                    self.said = Some(line);
                }
                let picture = renders.into_iter().find(|(i, _)| *i == index).map(|(_, image)| (image, set_region));
                Outcome::Draw { job: Box::new(Job { index, ..job.clone() }), picture }
            }
            Step::Off { why, .. } => {
                self.said = None;
                Outcome::Off(format!("{} (frames {}); {why}", wp::NEAR_CHANGE, frames(candidates)))
            }
        };
        if take {
            self.take_still(job, base, key, global, now);
        }
        outcome
    }

    /// Takes one still of `global` (the set's block) for set `key` of picture `base`.
    fn take_still(&mut self, job: &Job, base: String, key: String, global: Rect, now: Instant) {
        self.token = self.token.wrapping_add(1);
        let token = self.token;
        self.in_flight = Some((token, now));
        let space = job.space.clone();
        let size = Size::new(global.w, global.h);
        let (b, k) = (base.clone(), key);
        let done = Box::new(move |still: capture::DesktopStill| {
            let result = still.map(|(image, _)| (wallpaper::thumb(&image, space.as_deref(), size), wallpaper::space_name(&image)));
            dispatch2::DispatchQueue::main().exec_async(move || super::corner_patch::settle_news(News::Still { token, base: b, key: k, global, result }));
        });
        match capture::capture_desktop(global, done) {
            Ok(()) => tracing::debug!(%global, "corner patch: a still of the desktop to settle a solar frame"),
            Err(why) => {
                self.in_flight = None;
                self.last_still = Some(LastStill { base, result: Err(why), at: now });
            }
        }
    }

    /// Takes in what came back; `icons` are Finder's desktop icons as last read (global,
    /// grown by their labels).
    pub fn news(&mut self, news: News, icons: Option<&[Rect]>) {
        match news {
            News::Rendered { key, result } => {
                if self.rendering.as_deref() != Some(key.as_str()) {
                    return;
                }
                self.rendering = None;
                match result {
                    Ok(mut set) => {
                        let thumbs: Vec<&Thumb> = set.frames.iter().map(|c| &c.thumb).collect();
                        let alike = solar_settle::alike(&thumbs);
                        let spread = thumbs.windows(2).filter_map(|w| solar_settle::max_diff(w[0], w[1])).fold(0.0, f64::max);
                        set.alike = Some(alike);
                        tracing::debug!(alike, spread, frames = set.frames.len(), "corner patch: the candidate frames rendered");
                        if self.verdicts.len() >= MAX_VERDICTS {
                            self.verdicts.clear();
                        }
                        self.verdicts.insert(key, alike);
                        self.set = Some(set);
                    }
                    Err(why) => {
                        tracing::info!("corner patch: the candidate frames cannot be drawn (not tried again until they change): {why}");
                        self.failed = Some(key);
                    }
                }
            }
            News::Still { token, base, key, global, result } => {
                if self.in_flight.is_none_or(|(t, _)| t != token) {
                    return;
                }
                let Some((_, at)) = self.in_flight.take() else { return };
                let Some(set) = self.set.as_ref().filter(|s| s.key == key) else {
                    tracing::debug!("corner patch: a still of the desktop for candidates no longer wanted; dropped");
                    return;
                };
                let matched = match result {
                    Err(why) => {
                        tracing::debug!(%why, "corner patch: no still of the desktop");
                        Err("no still of the desktop came")
                    }
                    Ok((None, _)) => Err("the desktop's still cannot be read"),
                    Ok((Some(thumb), space)) => match icons {
                        None => Err("Finder's desktop icons are not known"),
                        Some(icons) => {
                            let mask = solar_settle::mask(thumb.cols, thumb.rows, global, icons);
                            let cands: Vec<(usize, &Thumb)> = set.frames.iter().map(|c| (c.index, &c.thumb)).collect();
                            let m = solar_settle::best_match(&thumb, &cands, &mask);
                            match &m {
                                Ok(m) => tracing::debug!(frame = m.index, best = m.best, next = m.next, %space, "corner patch: the desktop's still matched"),
                                Err(why) => tracing::debug!(why, %space, "corner patch: the desktop's still matched no frame clearly"),
                            }
                            m.map(|m| m.index)
                        }
                    },
                };
                self.last_still = Some(LastStill { base, result: matched, at });
            }
        }
    }
}

/// Renders the block once per candidate frame on a thread of its own, each with its thumb,
/// and hands the set to the main thread.
fn start_renders(job: Job, candidates: Vec<usize>, region: Rect, key: String) {
    let k = key.clone();
    let spawned = std::thread::Builder::new().name("corner-patch-candidates".into()).spawn(move || {
        let started = Instant::now();
        let size = Size::new(region.w, region.h);
        let result = crate::platform::safety::guarded("the corner patch's candidate renders", || {
            let mut out = Vec::with_capacity(candidates.len());
            for index in candidates {
                let image = wallpaper::render(&Job { index, ..job.clone() }, region)?;
                let thumb = wallpaper::thumb(&image, job.space.as_deref(), size).ok_or("a candidate's render cannot be read")?;
                out.push(Candidate { index, image, thumb });
            }
            Ok::<_, String>(Set { key: k.clone(), region, frames: out, alike: None })
        })
        .unwrap_or_else(|| Err("the render panicked".to_string()));
        tracing::debug!(ms = started.elapsed().as_millis() as u64, ok = result.is_ok(), "corner patch: candidate frames rendered");
        dispatch2::DispatchQueue::main().exec_async(move || super::corner_patch::settle_news(News::Rendered { key: k, result }));
    });
    if let Err(e) = spawned {
        tracing::warn!("corner patch: cannot start the candidate renders: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_is_its_picture_block_and_frames_in_any_order() {
        let r = Rect::new(2080.0, 880.0, 480.0, 560.0);
        assert_eq!(set_key("p", r, &[4, 3]), set_key("p", r, &[3, 4]), "the place's own frame changed: the same set");
        assert_ne!(set_key("p", r, &[3, 4]), set_key("p", r, &[3, 4, 5]));
        assert_ne!(set_key("p", r, &[3, 4]), set_key("q", r, &[3, 4]));
        assert_ne!(set_key("p", r, &[3, 4]), set_key("p", Rect::new(2000.0, 880.0, 560.0, 560.0), &[3, 4]));
        assert_eq!(frames(&[3, 4]), "3, 4");
    }
}
