//! Settling a solar frame for the corner patch (main thread state, its render thread, its
//! still): the platform half of [`crate::engine::solar_settle`], driven by
//! [`super::corner_patch`].
//!
//! - **Frames** ([`Settler::settle`], [`Settler::check`]): every frame the picture names by
//!   the sun ([`crate::engine::wallpaper::BySun::frames`]) is rendered once for the patch's
//!   block on a thread of its own, kept as a [`Thumb`] only (the frame settled is drawn by
//!   the patch's own render). Whether a set of candidates looks alike is remembered by
//!   picture, block and frames, so another Desktop's panel left drawn at one of them is kept
//!   only while they do ([`Settler::alike_for`]).
//! - **A still** ([`Settler::settle`] when the candidates differ; [`Settler::check`] on
//!   arrival at a Desktop, even for a frame the clock names exactly): one still of the block
//!   through ScreenCaptureKit ([`crate::platform::capture::capture_desktop`]: the desktop's
//!   own windows only, never a window, never ours; only where window captures already work,
//!   and never asked for), drawn in the display's colour space and compared with every
//!   frame on the same grid ([`crate::engine::solar_settle::seen`], Finder's icons masked
//!   out). At most one in flight; at most one a picture per [`solar_settle::RECHECK`]. The
//!   still only picks the frame: the patch draws the file's render. What it saw clearly
//!   comes back as an [`Observation`] that narrows the clock's model
//!   ([`crate::platform::wallpaper::Reader::observe`]).
//! - **Log**: which way the frame settled, once per change, at info; off and why through the
//!   patch's own line.

use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::engine::geometry::{Rect, Size};
use crate::engine::solar_settle::{self, Facts, Step, Thumb, Way};
use crate::engine::wallpaper as wp;
use crate::platform::capture;
use crate::platform::wallpaper::{self, Job};

/// The most alike verdicts remembered (one per picture, block and frames met).
const MAX_VERDICTS: usize = 64;

/// Every frame's render of the block for one picture, as thumbs.
pub struct Set {
    key: String,
    region: Rect,
    frames: Vec<(usize, Thumb)>,
}

/// The last still for a picture: what it saw or why nothing clearly, and when it was started.
struct LastStill {
    base: String,
    result: Result<Vec<usize>, &'static str>,
    at: Instant,
}

/// What came back to the main thread ([`super::corner_patch`] hands it to [`Settler::news`]).
pub enum News {
    /// Every frame's thumb for set `key`, or why not.
    Rendered {
        /// The set's key ([`frames_key`]).
        key: String,
        /// The thumbs.
        result: Result<Set, String>,
    },
    /// A still started with `token` for set `key` of picture `base`: its thumb, or why none.
    Still {
        /// Which still.
        token: u64,
        /// The picture ([`Job::base_key`]).
        base: String,
        /// The picture's file.
        path: String,
        /// The set it was taken for.
        key: String,
        /// The block taken (global, top-left).
        global: Rect,
        /// When it was started (seconds since 1970).
        unix: f64,
        /// The still as a thumb and its colour space's name, or why none.
        result: Result<(Option<Thumb>, String), String>,
    },
}

/// A still that saw clearly: the picture's file, when, and the frames it may show (the best
/// first), for the clock's model.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// The picture's file.
    pub path: String,
    /// When the still was started (seconds since 1970).
    pub unix: f64,
    /// What it saw ([`solar_settle::Seen::frames`]).
    pub seen: Vec<usize>,
}

/// What to draw near a solar frame change.
pub enum Outcome {
    /// This frame (the job at it).
    Draw(Box<Job>),
    /// Nothing yet: the frames render.
    Wait,
    /// Off, and why.
    Off(String),
}

/// Whether an arrival's check of a frame the clock names exactly ([`Settler::check`]) is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Taken, or a still of this picture is recent enough.
    Done,
    /// Not yet: the frames render, a still is in flight, or none may be taken now.
    Pending,
}

/// The set of every frame of picture `base` for `region`.
pub fn frames_key(base: &str, region: Rect, all: &[usize]) -> String {
    format!("{base}|{region:?}|all {all:?}")
}

/// The set of `candidates` of picture `base` for `region`: the frames in order, so a set
/// whose first frame changed is the same set.
pub fn set_key(base: &str, region: Rect, candidates: &[usize]) -> String {
    let mut c = candidates.to_vec();
    c.sort_unstable();
    format!("{base}|{region:?}|{c:?}")
}

/// The frames as a log shows them.
pub fn frames(c: &[usize]) -> String {
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

/// Whether the frames are ready ([`Settler::ensure`]).
enum Ready {
    Yes,
    Rendering,
    Failed,
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

    /// The renders of every frame `all` of `job`'s picture over `region`: started when not
    /// there yet.
    fn ensure(&mut self, job: &Job, all: &[usize], region: Rect) -> Ready {
        let key = frames_key(&job.base_key(), region, all);
        if self.set.as_ref().is_some_and(|s| s.key == key) {
            return Ready::Yes;
        }
        if self.failed.as_deref() == Some(key.as_str()) {
            return Ready::Failed;
        }
        if self.rendering.as_deref() != Some(key.as_str()) {
            self.rendering = Some(key.clone());
            start_renders(job.clone(), all.to_vec(), region, key);
        }
        Ready::Rendering
    }

    /// Whether `candidates` look alike in the set rendered (judged once, then remembered).
    fn alike(&mut self, base: &str, candidates: &[usize]) -> Option<bool> {
        let set = self.set.as_ref()?;
        let key = set_key(base, set.region, candidates);
        if let Some(v) = self.verdicts.get(&key) {
            return Some(*v);
        }
        let thumbs: Vec<&Thumb> = candidates.iter().filter_map(|c| set.frames.iter().find(|(i, _)| i == c).map(|(_, t)| t)).collect();
        if thumbs.len() != candidates.len() {
            return None;
        }
        let alike = solar_settle::alike(&thumbs);
        let spread = thumbs.windows(2).filter_map(|w| solar_settle::max_diff(w[0], w[1])).fold(0.0, f64::max);
        tracing::debug!(alike, spread, frames = %frames(candidates), "corner patch: candidate frames compared");
        if self.verdicts.len() >= MAX_VERDICTS {
            self.verdicts.clear();
        }
        self.verdicts.insert(key, alike);
        Some(alike)
    }

    /// The last still of picture `base`: what it saw or why nothing clearly, and its age.
    fn last(&self, base: &str, now: Instant) -> Option<(Result<Vec<usize>, &'static str>, std::time::Duration)> {
        self.last_still.as_ref().filter(|l| l.base == base).map(|l| (l.result.clone(), now.saturating_duration_since(l.at)))
    }

    /// A still is in flight (for at most [`solar_settle::STALL`]).
    fn flying(&self, now: Instant) -> bool {
        self.in_flight.is_some_and(|(_, at)| now.saturating_duration_since(at) < solar_settle::STALL)
    }

    /// The frame to draw for `job` (its frame the first of `candidates`; `all` every frame the
    /// picture names by the sun; `fits`: the clock's model still fits the screen) over
    /// `region` of the screen at `screen` (global), starting the renders or a still as due;
    /// `may_take` says whether a still may be taken now.
    #[allow(clippy::too_many_arguments)]
    pub fn settle(&mut self, job: &Job, candidates: &[usize], all: &[usize], fits: bool, screen: Rect, region: Rect, may_take: Result<(), &'static str>, now: Instant) -> Outcome {
        let prefix = if fits { wp::NEAR_CHANGE } else { wp::CLOCK_MISFIT };
        match self.ensure(job, all, region) {
            Ready::Yes => {}
            Ready::Rendering => return Outcome::Wait,
            Ready::Failed => {
                self.said = None;
                return Outcome::Off(format!("{prefix} (frames {}); the picture's frames cannot be drawn", frames(candidates)));
            }
        }
        let base = job.base_key();
        let Some(alike) = self.alike(&base, candidates) else {
            self.said = None;
            return Outcome::Off(format!("{prefix} (frames {}); a candidate is not among the picture's frames", frames(candidates)));
        };
        let step = solar_settle::next(&Facts { candidates, alike: Some(alike), last_still: self.last(&base, now), in_flight: self.flying(now), may_take });
        let take = matches!(step, Step::Draw { take: true, .. } | Step::Off { take: true, .. });
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
                Outcome::Draw(Box::new(Job { index, ..job.clone() }))
            }
            Step::Off { why, .. } => {
                self.said = None;
                Outcome::Off(format!("{prefix} (frames {}); {why}", frames(candidates)))
            }
        };
        if take {
            self.take_still(job, base, screen, now);
        }
        outcome
    }

    /// On arrival at a Desktop showing `job` (a frame the clock names exactly; `all` every
    /// frame the picture names by the sun): one still of `region` checks it, at most one a
    /// picture per [`solar_settle::RECHECK`], when one may be taken now (`may_take`).
    pub fn check(&mut self, job: &Job, all: &[usize], screen: Rect, region: Rect, may_take: Result<(), &'static str>, now: Instant) -> Check {
        let base = job.base_key();
        let age = self.last(&base, now).map(|(_, a)| a);
        if age.is_some_and(|a| a < solar_settle::RECHECK) {
            return Check::Done;
        }
        if !solar_settle::check_due(age, self.flying(now), may_take) {
            return Check::Pending;
        }
        match self.ensure(job, all, region) {
            Ready::Yes => {}
            Ready::Rendering => return Check::Pending,
            Ready::Failed => return Check::Done,
        }
        tracing::debug!(frame = job.index, "corner patch: a still of the desktop checks the frame the clock names on arrival");
        self.take_still(job, base, screen, now);
        Check::Done
    }

    /// Takes one still of the set's block for picture `base` (the screen at `screen`).
    fn take_still(&mut self, job: &Job, base: String, screen: Rect, now: Instant) {
        let Some((key, region)) = self.set.as_ref().map(|s| (s.key.clone(), s.region)) else { return };
        let global = region.translate(screen.x, screen.y);
        self.token = self.token.wrapping_add(1);
        let token = self.token;
        self.in_flight = Some((token, now));
        let space = job.space.clone();
        let size = Size::new(global.w, global.h);
        let unix = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        let (b, path) = (base.clone(), job.path.clone());
        let done = Box::new(move |still: capture::DesktopStill| {
            let result = still.map(|(image, _)| (wallpaper::thumb(&image, space.as_deref(), size), wallpaper::space_name(&image)));
            dispatch2::DispatchQueue::main()
                .exec_async(move || super::corner_patch::settle_news(News::Still { token, base: b, path, key, global, unix, result }));
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
    /// grown by their labels). A still that saw clearly comes back as an [`Observation`].
    pub fn news(&mut self, news: News, icons: Option<&[Rect]>) -> Option<Observation> {
        match news {
            News::Rendered { key, result } => {
                if self.rendering.as_deref() != Some(key.as_str()) {
                    return None;
                }
                self.rendering = None;
                match result {
                    Ok(set) => self.set = Some(set),
                    Err(why) => {
                        tracing::info!("corner patch: the picture's frames cannot be drawn (not tried again until they change): {why}");
                        self.failed = Some(key);
                    }
                }
                None
            }
            News::Still { token, base, path, key, global, unix, result } => {
                if self.in_flight.is_none_or(|(t, _)| t != token) {
                    return None;
                }
                let (_, at) = self.in_flight.take()?;
                let Some(set) = self.set.as_ref().filter(|s| s.key == key) else {
                    tracing::debug!("corner patch: a still of the desktop for frames no longer wanted; dropped");
                    return None;
                };
                let seen = match result {
                    Err(why) => {
                        tracing::debug!(%why, "corner patch: no still of the desktop");
                        Err("no still of the desktop came")
                    }
                    Ok((None, _)) => Err("the desktop's still cannot be read"),
                    Ok((Some(thumb), space)) => match icons {
                        None => Err("Finder's desktop icons are not known"),
                        Some(icons) => {
                            let mask = solar_settle::mask(thumb.cols, thumb.rows, global, icons);
                            let all: Vec<(usize, &Thumb)> = set.frames.iter().map(|(i, t)| (*i, t)).collect();
                            let s = solar_settle::seen(&thumb, &all, &mask);
                            match &s {
                                Ok(s) => tracing::debug!(frames = %frames(&s.frames), best = s.best, next = s.next, %space, "corner patch: the desktop's still saw"),
                                Err(why) => tracing::debug!(why, %space, "corner patch: the desktop's still matched no frame clearly"),
                            }
                            s.map(|s| s.frames)
                        }
                    },
                };
                let observed = seen.as_ref().ok().map(|seen| Observation { path, unix, seen: seen.clone() });
                self.last_still = Some(LastStill { base, result: seen, at });
                observed
            }
        }
    }
}

/// Renders the block once per frame on a thread of its own, keeping each one's thumb, and
/// hands the set to the main thread.
fn start_renders(job: Job, all: Vec<usize>, region: Rect, key: String) {
    let k = key.clone();
    let spawned = std::thread::Builder::new().name("corner-patch-frames".into()).spawn(move || {
        let started = Instant::now();
        let size = Size::new(region.w, region.h);
        let result = crate::platform::safety::guarded("the corner patch's frame renders", || {
            let mut out = Vec::with_capacity(all.len());
            for index in all {
                let image = wallpaper::render(&Job { index, ..job.clone() }, region)?;
                let thumb = wallpaper::thumb(&image, job.space.as_deref(), size).ok_or("a frame's render cannot be read")?;
                out.push((index, thumb));
            }
            Ok::<_, String>(Set { key: k.clone(), region, frames: out })
        })
        .unwrap_or_else(|| Err("the render panicked".to_string()));
        tracing::debug!(ms = started.elapsed().as_millis() as u64, ok = result.is_ok(), "corner patch: the picture's frames rendered");
        dispatch2::DispatchQueue::main().exec_async(move || super::corner_patch::settle_news(News::Rendered { key: k, result }));
    });
    if let Err(e) = spawned {
        tracing::warn!("corner patch: cannot start the frame renders: {e}");
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
        assert_ne!(frames_key("p", r, &[0, 1]), frames_key("q", r, &[0, 1]));
        assert_ne!(frames_key("p", r, &[0, 1]), frames_key("p", Rect::new(0.0, 0.0, 480.0, 560.0), &[0, 1]));
        assert_eq!(frames(&[3, 4]), "3, 4");
    }
}
