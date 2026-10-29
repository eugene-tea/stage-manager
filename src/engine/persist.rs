//! Persistence: `state.json` and re-matching saved windows to live ones on launch.
//!
//! The file keeps stages **per Space** ([`SavedSpace`], keyed by the model's synthetic
//! [`SpaceId`]). A saved window is found again by the scored matcher
//! ([`crate::engine::window_match::assign`]): its window id (ids outlive our app, not the
//! window's own app), its exact title, the frame its app reopened it at, its title's
//! fingerprint, its app alone; each live window used at most once. Only a strong or medium
//! match puts a window into an inactive (parked) stage; a low one joins its saved stage only
//! when that is the active stage, and leaves the saved window pending. Live windows that match
//! nothing are grouped the way Apple's Stage Manager does when it is turned on: one stage
//! per app (see [`Model::apply_launch`]). Saved windows not on screen stay **pending** in
//! their stage ([`crate::engine::restore`]). A Space is recognized by its windows: the
//! saved Space whose windows match the ones on screen (window id, or exact bundle ID +
//! title) is the one shown ([`Model::arrive`]); other saved Spaces wait, untouched, for
//! their first visit.
//!
//! Fields added after version 2 first shipped (`saved_at`, `display`, a Space's
//! `last_seen`, a stage's `last_active`, a window's `fp`, `pending` and `quit_at`) are
//! optional both ways: older files load without them, and older builds of version 2 read
//! newer version-2 files, ignoring them. A file of a later version is not read
//! ([`StateFile::from_json`]): the build starts without saved stages, as from no file.
//!
//! **Windows of apps that quit** (round 8H) stay in the file a while: an app's quit takes its
//! windows out of the model (Apple's behaviour, no card), but their saved entries are written
//! with `quit_at` in their stages (a stage gone with them is written too), at their last real
//! frame. At a load in a new session the ones that quit within [`QUIT_KEEP`] before the save
//! wait in their stages as any saved window not on screen does; older ones, and every one at
//! a load in the same session, are dropped ([`StateFile::settle_quit`]). A logout whose early
//! signal never came (the apps quit first, the strip losing their stages one by one) still
//! leaves the stages it had for the next login.
//!
//! Version 1 files (one Space) are read as a single Space.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::geometry::Rect;
use super::model::{
    Action, Home, Model, Observation, Pid, SpaceId, StageId, WindowId, WindowMeta, WindowRole,
    WindowState,
};
use super::restore::{QUIT_KEEP, merge_order};
use super::rules::Decision;
use super::settings::AppWindows;
use super::stage_area::same_frame;
use super::window_match::{Grade, Live, LowRule, Saved, assign, fingerprint};

/// The state file format version this build writes (it also reads version 1).
pub const STATE_VERSION: u32 = 2;

/// A saved window's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedRole {
    /// A standard window: matched on launch.
    #[default]
    Standard,
    /// A child window: saved for completeness, never matched (children follow their parent).
    Child,
}

/// A saved window not seen in this window server session yet ([`crate::engine::restore`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// The last session the window was known in (its file's session when it became
    /// pending at a load, or the session it went missing in): pending through a whole
    /// later session, it is given up ([`StateFile::start_new_session`]). Empty when unknown.
    pub since: String,
}

/// One saved window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    /// The app's bundle ID.
    pub bundle_id: String,
    /// The title when saved.
    pub title: String,
    /// The restore frame.
    pub frame: Rect,
    /// Position in the stage, 0 = front.
    pub z: usize,
    /// True for the stage's key window.
    pub key: bool,
    /// True for a pinned window (saved with the active stage, restored pinned).
    pub pinned: bool,
    /// Standard or child.
    pub role: SavedRole,
    /// The frame before we fitted the window clear of the strip, when we did (quit and the
    /// recovery scan put it back there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<Rect>,
    /// The window's `CGWindowID` when saved. Window ids outlive our app (the windows belong to
    /// other apps), so a restart finds the very window by it whatever its title is now; a
    /// relaunched app's windows get new ids and are matched by bundle ID and title instead.
    /// Absent in files written before 10-13.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
    /// The title's fingerprint when saved ([`fingerprint`]), for readers of the file; the
    /// matcher computes it from `title` (so a change of the title rules applies to old files
    /// at once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp: Option<String>,
    /// Set while the window has not been seen in this window server session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Pending>,
    /// When its app quit (Unix seconds): the window left the model then and its entry is kept
    /// for the next session only ([`StateFile::settle_quit`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quit_at: Option<u64>,
}

impl SavedWindow {
    /// True when `live` is this very window: the same `CGWindowID` and the same app (an id
    /// reused after a reboot by another app's window never matches).
    pub fn is_window(&self, live: &WindowMeta) -> bool {
        self.window == Some(live.id.0) && !self.bundle_id.is_empty() && self.bundle_id == live.bundle_id
    }

    /// True when `live` has this window's app and title.
    pub fn same_title(&self, live: &WindowMeta) -> bool {
        self.bundle_id == live.bundle_id && self.title == live.title
    }

    /// The window as the scored matcher sees it, in group `group` (its stage or Desktop).
    pub fn scorer_key(&self, group: u64) -> Saved<'_> {
        Saved {
            bundle: &self.bundle_id,
            title: &self.title,
            fp: fingerprint(&self.title),
            id: self.window,
            frames: [Some(self.frame), self.original],
            group,
        }
    }
}

/// A live window as the scored matcher sees it; `parked`: its frame is in a park zone.
pub fn live_key(meta: &WindowMeta, parked: bool) -> Live<'_> {
    Live {
        bundle: &meta.bundle_id,
        title: &meta.title,
        fp: fingerprint(&meta.title),
        id: meta.id.0,
        frame: meta.frame,
        frame_usable: !parked && !meta.frame.is_degenerate(),
    }
}

/// One saved stage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedStage {
    /// The stage's name, if any.
    pub name: Option<String>,
    /// Its windows, front to back (pending ones after the ones on screen when saved).
    pub windows: Vec<SavedWindow>,
    /// The model's logical time the stage was last active, so "the app's most recent stage"
    /// and the strip's least-recently-used eviction survive a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active: Option<u64>,
}

/// One saved Space: its stages, active stage and strip, like a whole Stage Manager.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedSpace {
    /// The model's synthetic Space id (never Apple's).
    pub id: u64,
    /// The Space shown when saved.
    #[serde(default)]
    pub current: bool,
    /// Stages in creation order (hotkey numbers).
    pub stages: Vec<SavedStage>,
    /// Index into `stages` of the Space's active stage.
    pub active_index: Option<usize>,
    /// Indices into `stages` of the strip slots, top to bottom (pending stages in the place
    /// their card had).
    #[serde(default)]
    pub strip_slots: Vec<usize>,
    /// When the Space was last shown (Unix seconds), as far as saves saw it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<u64>,
}

impl SavedSpace {
    /// Removes the stages at `indices` (ascending), keeping `active_index` and `strip_slots`
    /// pointing at the same stages (an active one removed leaves none active).
    pub fn remove_stages(&mut self, indices: &[usize]) {
        if indices.is_empty() {
            return;
        }
        let new_index = |i: usize| (!indices.contains(&i)).then(|| i - indices.iter().filter(|&&r| r < i).count());
        self.active_index = self.active_index.and_then(new_index);
        self.strip_slots = self.strip_slots.iter().filter_map(|&i| new_index(i)).collect();
        let mut i = 0;
        self.stages.retain(|_| {
            let keep = !indices.contains(&i);
            i += 1;
            keep
        });
    }

    /// How many standard windows it saved (children follow their parents and are never
    /// matched).
    pub fn standard_windows(&self) -> usize {
        self.stages.iter().flat_map(|s| s.windows.iter()).filter(|w| w.role == SavedRole::Standard).count()
    }
}

/// The contents of `state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateFile {
    /// Always [`STATE_VERSION`] when written.
    pub version: u32,
    /// The window server session the saved window ids belong to (the boot and the login
    /// session, as the platform reads them). Window ids start over after a reboot or a new
    /// login, so ids saved in another session name other windows: [`StateFile::for_session`]
    /// drops them at load. Absent in files written before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// When the file was written (Unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<u64>,
    /// The main display's size when saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<SavedDisplay>,
    /// Every Space with stages, the shown one flagged `current`.
    pub spaces: Vec<SavedSpace>,
}

/// The main display when saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedDisplay {
    /// Width in points.
    pub w: f64,
    /// Height in points.
    pub h: f64,
    /// Reserved: the display arrangement (each display's vendor, model and serial), for
    /// frames per arrangement later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Version 1: one Space, no ids.
#[derive(Deserialize)]
struct StateFileV1 {
    stages: Vec<SavedStage>,
    active_index: Option<usize>,
    #[serde(default)]
    strip_slots: Vec<usize>,
}

impl StateFile {
    /// A file holding one Space.
    pub fn single(space: SavedSpace) -> Self {
        Self {
            version: STATE_VERSION,
            session: None,
            saved_at: None,
            display: None,
            spaces: vec![space],
        }
    }

    /// True when the file was written in window server session `current` (both known).
    pub fn same_session(&self, current: Option<&str>) -> bool {
        current.is_some() && self.session.as_deref() == current
    }

    /// A load in another window server session than the file's (a restart, a new login):
    /// every saved window becomes pending, since the file's session; one already pending
    /// since an earlier session than the file's was pending through that whole session (its
    /// app never brought it back) and is given up, as is one pending since a session that
    /// could not be read (`""`: at least that whole session went by); a saved Space left
    /// without a standard window is dropped. Returns how many windows were given up.
    pub fn start_new_session(&mut self) -> usize {
        let since = self.session.clone().unwrap_or_default();
        let mut expired = 0;
        for stage in self.spaces.iter_mut().flat_map(|s| s.stages.iter_mut()) {
            let before = stage.windows.len();
            stage.windows.retain(|w| w.pending.as_ref().is_none_or(|p| !p.since.is_empty() && p.since == since));
            expired += before - stage.windows.len();
            for w in &mut stage.windows {
                w.pending.get_or_insert_with(|| Pending { since: since.clone() });
            }
        }
        self.spaces.retain(|s| s.standard_windows() > 0);
        expired
    }

    /// The entries of windows whose app quit (`quit_at`), as a load must take them before
    /// anything else reads the file: at a load in a new session (`new_session`, the restore
    /// on) those that quit within [`QUIT_KEEP`] before `saved_at` become ordinary saved windows
    /// (pending in their stages from then on, as any saved window not on screen); older ones,
    /// any when the file does not say when it was saved, and every one at a load in the same
    /// session (the app was quit by the user: its next windows go by the usual rules) are
    /// dropped, with the stages left with no window by that. Returns (kept, dropped).
    pub fn settle_quit(&mut self, new_session: bool) -> (usize, usize) {
        let cutoff = self.saved_at.map(|t| t.saturating_sub(QUIT_KEEP.as_secs()));
        let (mut kept, mut dropped) = (0, 0);
        for space in &mut self.spaces {
            let mut emptied = Vec::new();
            for (i, stage) in space.stages.iter_mut().enumerate() {
                let before = stage.windows.len();
                stage.windows.retain(|w| match (w.quit_at, cutoff) {
                    (None, _) => true,
                    (Some(q), Some(c)) => new_session && q >= c,
                    (Some(_), None) => false,
                });
                dropped += before - stage.windows.len();
                for w in &mut stage.windows {
                    if w.quit_at.take().is_some() {
                        kept += 1;
                    }
                }
                if before > 0 && stage.windows.is_empty() {
                    emptied.push(i);
                }
            }
            space.remove_stages(&emptied);
        }
        (kept, dropped)
    }

    /// Stamps the save time `now` (Unix seconds) and each Space's `last_seen`: now for the
    /// Space shown, else what `seen` (the times kept from earlier saves and the loaded file)
    /// or the Space itself says. `seen` learns the shown Space's time.
    pub fn stamp(&mut self, now: u64, seen: &mut HashMap<u64, u64>) {
        self.saved_at = Some(now);
        for space in &mut self.spaces {
            if space.current {
                seen.insert(space.id, now);
            }
            space.last_seen = seen.get(&space.id).copied().or(space.last_seen);
        }
    }

    /// Each saved Space's `last_seen`, by id (to seed [`StateFile::stamp`]).
    pub fn seen_times(&self) -> HashMap<u64, u64> {
        self.spaces.iter().filter_map(|s| Some((s.id, s.last_seen?))).collect()
    }

    /// How many saved windows are pending.
    pub fn pending_windows(&self) -> usize {
        self.all_stages().flat_map(|s| s.windows.iter()).filter(|w| w.pending.is_some()).count()
    }

    /// The file as read in window server session `current`: saved window ids are kept only
    /// when the file was written in that very session; otherwise (a reboot or a new login
    /// since, a file that does not say, a session that cannot be read now) every saved
    /// window id is dropped, so a reused id never names another window and matching falls
    /// back to bundle ID and title. Returns how many ids were dropped too.
    pub fn for_session(mut self, current: Option<&str>) -> (Self, usize) {
        let same = current.is_some() && self.session.as_deref() == current;
        let mut dropped = 0;
        if !same {
            for w in self.spaces.iter_mut().flat_map(|s| s.stages.iter_mut()).flat_map(|s| s.windows.iter_mut()) {
                if w.window.take().is_some() {
                    dropped += 1;
                }
            }
        }
        (self, dropped)
    }

    /// True when the frames saved here apply on a main display of frame `main`: the file's
    /// display had that size (points, rounded), or the file does not say (as
    /// [`crate::engine::restore::Restore::frames_fit`] decides for a restore; S6).
    pub fn frames_fit(&self, main: Rect) -> bool {
        self.display.as_ref().is_none_or(|d| {
            !d.w.is_finite() || !d.h.is_finite() || (d.w.round() as i64, d.h.round() as i64) == (main.w.round() as i64, main.h.round() as i64)
        })
    }

    /// Every saved stage of every Space.
    pub fn all_stages(&self) -> impl Iterator<Item = &SavedStage> {
        self.spaces.iter().flat_map(|s| s.stages.iter())
    }

    /// Pretty JSON for writing to disk, or why it cannot be written: a frame that is not
    /// finite (JSON has no NaN or infinity; `serde_json` writes `null`, which no load reads
    /// back, so the whole file would be lost), or any encoding error. Never a stand-in such
    /// as `{}`: the caller keeps the file it has.
    pub fn to_json(&self) -> Result<String, String> {
        if let Some(w) = self.all_stages().flat_map(|s| s.windows.iter()).find(|w| !w.frame.is_finite() || w.original.is_some_and(|o| !o.is_finite())) {
            return Err(format!(
                "state.json: a frame of window {} of {} is not finite; not written",
                w.window.map_or_else(|| "?".to_string(), |id| id.to_string()),
                if w.bundle_id.is_empty() { "an unknown app" } else { &w.bundle_id }
            ));
        }
        serde_json::to_string_pretty(self).map_err(|e| format!("state.json: {e}; not written"))
    }

    /// Parses JSON: version 2, or version 1 read as one current Space; other versions are
    /// rejected.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("state.json: {e}"))?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(1) => {
                let v1: StateFileV1 =
                    serde_json::from_value(value).map_err(|e| format!("state.json: {e}"))?;
                Ok(Self::single(SavedSpace {
                    id: 1,
                    current: true,
                    stages: v1.stages,
                    active_index: v1.active_index,
                    strip_slots: v1.strip_slots,
                    last_seen: None,
                }))
            }
            Some(v) if v == u64::from(STATE_VERSION) => {
                serde_json::from_value(value).map_err(|e| format!("state.json: {e}"))
            }
            Some(v) => Err(format!(
                "state.json: version {v} is not supported (expected {STATE_VERSION})"
            )),
            None => Err("state.json: no version".to_string()),
        }
    }
}

/// One live window placed into a saved stage.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignedWindow {
    /// The live window.
    pub id: WindowId,
    /// The saved restore frame.
    pub frame: Rect,
    /// It was the stage's key window.
    pub key: bool,
    /// It was pinned.
    pub pinned: bool,
    /// Its saved pre-fit frame.
    pub original: Option<Rect>,
    /// How sure the match is (a low one keeps its live frame and no flags).
    pub grade: Grade,
    /// Its saved place in the stage, 0 = front.
    pub z: usize,
}

/// A saved stage with the live windows matched into it, front to back.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignedStage {
    /// The saved name.
    pub name: Option<String>,
    /// Matched windows, front to back (pinned ones included, flagged).
    pub windows: Vec<AssignedWindow>,
    /// Its saved standard windows that matched nothing: pending.
    pub pending: Vec<SavedWindow>,
    /// The saved recency.
    pub last_active: Option<u64>,
}

/// The result of [`rematch`]: saved stages (same order and indices as the state file) filled
/// with live windows.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// One entry per saved stage, in saved order.
    pub stages: Vec<AssignedStage>,
    /// The saved active stage index.
    pub active_index: Option<usize>,
    /// The saved strip slots (indices into `stages`).
    pub strip_slots: Vec<usize>,
    /// Live standard windows that matched nothing; they join the active stage.
    pub unmatched: Vec<WindowId>,
}

/// Which frames the windows [`Model::launch_with`] places take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchFrames {
    /// Confident matches their saved frames; the active stage's and pinned windows are
    /// restored there (a launch that restores frames).
    Saved,
    /// Their live frames (a live frame in a park zone always falls back to the saved one).
    Live,
    /// Confident matches their saved frames, a shown one moved there; the rest untouched (an
    /// adoption's windows).
    Matched,
}

/// What the platform knows at launch about recency, for windows no saved stage claims.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LaunchHints {
    /// Live windows front to back (the window list's z-order).
    pub z_order: Vec<WindowId>,
    /// The frontmost app, if known.
    pub frontmost: Option<Pid>,
}

/// How many live standard windows match a saved standard window of `space` exactly (the
/// same window id, else the same bundle ID and title; each window on either side used once):
/// the evidence that `space` is on screen.
pub fn exact_matches(space: &SavedSpace, live: &[WindowMeta]) -> usize {
    let saved: Vec<&SavedWindow> = space
        .stages
        .iter()
        .flat_map(|s| s.windows.iter())
        .filter(|w| w.role == SavedRole::Standard)
        .collect();
    let standard: Vec<&WindowMeta> = live.iter().filter(|l| l.role == WindowRole::Standard).collect();
    let mut used: HashSet<WindowId> = HashSet::new();
    let mut matched = vec![false; saved.len()];
    let passes: [fn(&SavedWindow, &WindowMeta) -> bool; 2] = [SavedWindow::is_window, SavedWindow::same_title];
    for pass in passes {
        for (done, sw) in matched.iter_mut().zip(&saved) {
            if *done {
                continue;
            }
            if let Some(l) = standard.iter().find(|l| !used.contains(&l.id) && pass(sw, l)) {
                used.insert(l.id);
                *done = true;
            }
        }
    }
    matched.iter().filter(|m| **m).count()
}

/// How many live standard windows match a saved standard window of `space` by bundle ID
/// alone (each saved window used once): weak evidence, for the launch fallback.
pub fn bundle_matches(space: &SavedSpace, live: &[WindowMeta]) -> usize {
    let mut used: HashSet<WindowId> = HashSet::new();
    let mut n = 0;
    for sw in space
        .stages
        .iter()
        .flat_map(|s| s.windows.iter())
        .filter(|w| w.role == SavedRole::Standard)
    {
        if let Some(l) = live.iter().find(|l| {
            l.role == WindowRole::Standard && !used.contains(&l.id) && !l.bundle_id.is_empty() && l.bundle_id == sw.bundle_id
        }) {
            used.insert(l.id);
            n += 1;
        }
    }
    n
}

/// How many live standard windows match a saved standard window of `space` with
/// confidence (strong or medium, unambiguous between its stages; each window on either side
/// used once): the evidence a restore takes that `space` is on screen. `parked`: a live
/// window's frame is in a park zone (it says nothing then).
pub fn confident_matches(space: &SavedSpace, live: &[WindowMeta], parked: impl Fn(&WindowMeta) -> bool) -> usize {
    let saved: Vec<Saved> = space
        .stages
        .iter()
        .enumerate()
        .flat_map(|(i, s)| s.windows.iter().filter(|w| w.role == SavedRole::Standard).map(move |w| w.scorer_key(i as u64)))
        .collect();
    let standard: Vec<&WindowMeta> = live.iter().filter(|l| l.role == WindowRole::Standard).collect();
    let keys: Vec<Live> = standard.iter().map(|m| live_key(m, parked(m))).collect();
    assign(&saved, &keys, LowRule::default()).iter().flatten().filter(|p| p.grade.confident()).count()
}

/// Places live standard windows into one saved Space's stages ([`rematch_on`] with every
/// live frame taken as telling).
pub fn rematch(state: &SavedSpace, live: &[WindowMeta]) -> Assignment {
    rematch_on(state, live, |_| false)
}

/// Places live standard windows into one saved Space's stages with the scored matcher
/// ([`assign`]): strong and medium matches into their saved stages; a low one only into the
/// saved active stage (never parked), its saved window still pending; each live window used
/// at most once. Saved standard windows left over are pending in their stages. Child windows
/// on either side are left out; they follow their parents. `parked`: a live window's frame
/// is in a park zone (its frame says nothing).
pub fn rematch_on(state: &SavedSpace, live: &[WindowMeta], parked: impl Fn(&WindowMeta) -> bool) -> Assignment {
    let standard: Vec<&WindowMeta> = live
        .iter()
        .filter(|m| m.role == WindowRole::Standard)
        .collect();
    let mut saved: Vec<(usize, &SavedWindow)> = Vec::new();
    for (i, st) in state.stages.iter().enumerate() {
        let mut ws: Vec<&SavedWindow> = st
            .windows
            .iter()
            .filter(|w| w.role == SavedRole::Standard)
            .collect();
        ws.sort_by_key(|w| w.z);
        saved.extend(ws.into_iter().map(|w| (i, w)));
    }
    let keys: Vec<Saved> = saved.iter().map(|(i, w)| w.scorer_key(*i as u64)).collect();
    let lives: Vec<Live> = standard.iter().map(|m| live_key(m, parked(m))).collect();
    let low = LowRule { shown: state.active_index.filter(|&i| i < state.stages.len()).map(|i| i as u64) };
    let picks = assign(&keys, &lives, low);
    // Saved index -> (live window, grade).
    let mut matched: Vec<Option<(WindowId, Grade)>> = vec![None; saved.len()];
    for (l, pick) in standard.iter().zip(&picks) {
        if let Some(p) = pick {
            matched[p.saved] = Some((l.id, p.grade));
        }
    }
    let mut stages: Vec<AssignedStage> = state
        .stages
        .iter()
        .map(|s| AssignedStage {
            name: s.name.clone(),
            windows: Vec::new(),
            pending: Vec::new(),
            last_active: s.last_active,
        })
        .collect();
    for ((stage, sw), m) in saved.iter().zip(&matched) {
        match m {
            Some((id, grade)) => {
                let sure = grade.confident();
                stages[*stage].windows.push(AssignedWindow {
                    id: *id,
                    frame: sw.frame,
                    key: sw.key,
                    pinned: sw.pinned && sure,
                    original: sw.original.filter(|_| sure),
                    grade: *grade,
                    z: sw.z,
                });
                if !sure {
                    // Not sure it is that window: the saved one is still awaited.
                    stages[*stage].pending.push((*sw).clone());
                }
            }
            None => stages[*stage].pending.push((*sw).clone()),
        }
    }
    let used: HashSet<WindowId> = matched.iter().flatten().map(|(id, _)| *id).collect();
    let active_index = state.active_index.filter(|&i| i < stages.len());
    Assignment {
        strip_slots: state
            .strip_slots
            .iter()
            .copied()
            .filter(|&i| i < stages.len())
            .collect(),
        stages,
        active_index,
        unmatched: standard
            .iter()
            .filter(|l| !used.contains(&l.id))
            .map(|l| l.id)
            .collect(),
    }
}

impl Model {
    /// Snapshot for `state.json`: every Space with windows or pending windows, the shown one
    /// flagged `current`, plus the saved Spaces not visited yet this run (untouched). Pinned
    /// windows are saved with their Space's active stage, flagged `pinned`; children are saved
    /// with their stage after its members; pending windows after those. The window server
    /// session and the times are the platform's to stamp ([`StateFile::session`],
    /// [`StateFile::stamp`]).
    pub fn to_state(&self) -> StateFile {
        let mut spaces: Vec<SavedSpace> = self
            .spaces
            .iter()
            .filter(|space| !self.space_is_empty(space.id) || self.space_has_quit(space.id))
            .map(|space| self.save_space(space.id))
            .collect();
        spaces.extend(self.leftovers.iter().cloned().map(|mut l| {
            l.current = false;
            // Stages whose windows went to another Desktop (the pool after a restart): no
            // stage with nothing in it is written.
            let gone: Vec<usize> = (0..l.stages.len())
                .filter(|&i| l.stages[i].windows.is_empty() && self.pooled_out.contains(&(l.id, i)))
                .collect();
            l.remove_stages(&gone);
            l
        }));
        let main = self.main_display().frame;
        StateFile {
            version: STATE_VERSION,
            session: None,
            saved_at: None,
            display: Some(SavedDisplay { w: main.w, h: main.h, key: None }),
            spaces,
        }
    }

    /// One Space as `state.json` keeps it (see [`Model::to_state`]).
    pub(crate) fn save_space(&self, space: SpaceId) -> SavedSpace {
        let save = |id: WindowId, z: usize, key: bool, pinned: bool| -> Option<SavedWindow> {
            let w = self.windows.get(&id)?;
            let fp = fingerprint(&w.meta.title);
            Some(SavedWindow {
                bundle_id: w.meta.bundle_id.clone(),
                title: w.meta.title.clone(),
                frame: w.restore,
                z,
                key,
                pinned,
                role: match w.meta.role {
                    WindowRole::Standard => SavedRole::Standard,
                    WindowRole::Child { .. } => SavedRole::Child,
                },
                original: w.original,
                window: Some(id.0),
                fp: (!fp.is_empty()).then_some(fp),
                pending: None,
                quit_at: None,
            })
        };
        let here: Vec<&crate::engine::model::Stage> = self.stages_in(space).collect();
        let active = self.active_stage_in(space);
        let active_index = here.iter().position(|s| Some(s.id) == active);
        let mut stages: Vec<SavedStage> = here
            .iter()
            .map(|s| {
                let mut windows: Vec<SavedWindow> = s
                    .windows
                    .iter()
                    .enumerate()
                    .filter_map(|(z, &id)| save(id, z, s.key == Some(id), false))
                    .collect();
                let base = windows.len();
                windows.extend(
                    self.children_in(Home::Stage(s.id))
                        .into_iter()
                        .enumerate()
                        .filter_map(|(i, id)| save(id, base + i, false, false)),
                );
                let base = windows.len();
                let has_key = s.key.is_some_and(|k| s.windows.contains(&k));
                windows.extend(self.pending_of(s.id).iter().enumerate().map(|(i, w)| SavedWindow {
                    z: base + i,
                    key: w.key && !has_key,
                    ..w.clone()
                }));
                SavedStage {
                    name: s.name.clone(),
                    windows,
                    last_active: Some(s.last_active),
                }
            })
            .collect();
        if let Some(i) = active_index {
            let mut pinned = self.pinned();
            pinned.extend(self.children_in(Home::Pinned));
            pinned.retain(|id| self.windows.get(id).is_some_and(|w| w.space == space));
            let base = stages[i].windows.len();
            stages[i].windows.extend(
                pinned
                    .into_iter()
                    .enumerate()
                    .filter_map(|(k, id)| save(id, base + k, false, true)),
            );
        }
        // The strip, with each pending stage (no card) in the place its card had.
        let slots = self.slots.get(&space).cloned().unwrap_or_default();
        let order = self.slot_order.get(&space).map_or(slots.clone(), |saved| {
            merge_order(&slots, saved, |s| here.iter().any(|h| h.id == s) && Some(s) != active && self.has_pending(s))
        });
        let strip_slots = order.iter().filter_map(|id| here.iter().position(|s| s.id == *id)).collect();
        // Windows whose app quit a moment ago, in their stages (round 8H).
        let ids: Vec<StageId> = here.iter().map(|s| s.id).collect();
        let (stages, active_index, strip_slots) = self.with_quit(space, &ids, stages, active_index, strip_slots);
        SavedSpace {
            id: space.0,
            current: self.current.values().any(|&c| c == space),
            stages,
            active_index,
            strip_slots,
            last_seen: None,
        }
    }

    /// Rebuilds stages from an [`Assignment`] on launch, with no recency hints (see
    /// [`Model::apply_launch`]).
    pub fn apply_assignment(
        &mut self,
        live: &[WindowMeta],
        assignment: &Assignment,
        restore_frames: bool,
    ) -> Vec<Action> {
        self.apply_launch(live, assignment, restore_frames, &LaunchHints::default())
    }

    /// Rebuilds stages from an [`Assignment`] on launch, on the Space the main display shows.
    /// `live` lists every live window there (children too; they follow their parents). A
    /// saved stage's windows not on screen are kept pending in it (no card until one comes,
    /// [`crate::engine::restore`]); saved stages with neither a matched nor a pending window
    /// are dropped unless active or declared. Saved recency (`last_active`) is kept.
    ///
    /// Standard windows no saved stage claims (every window on a first start, without a
    /// `state.json`) are placed the way Apple's Stage Manager does when it is turned on: an
    /// app that already has a saved stage takes its extra windows into that stage (the active
    /// one if it holds the app); every other app gets a stage of its own holding all its
    /// windows ([`AppWindows::AllAtOnce`]) or one stage per window
    /// ([`AppWindows::OneAtATime`]). The frontmost app ([`LaunchHints::frontmost`], else the
    /// app of the frontmost window) comes first and takes an empty active stage; when no
    /// saved stage is active with windows, its stage becomes active. The others enter the
    /// strip most recently used first, by the window list's z-order.
    ///
    /// Windows of the Space's inactive stages are parked; with `restore_frames` the active
    /// stage's and pinned windows get their saved frames, else they keep their live frames (a
    /// live frame in a park zone always falls back to the saved one). Windows already known
    /// are skipped. Other Spaces are left alone.
    pub fn apply_launch(
        &mut self,
        live: &[WindowMeta],
        assignment: &Assignment,
        restore_frames: bool,
        hints: &LaunchHints,
    ) -> Vec<Action> {
        let frames = if restore_frames { LaunchFrames::Saved } else { LaunchFrames::Live };
        self.launch_with(live, assignment, frames, hints, None)
    }

    /// [`Model::apply_launch`] with the frames `frames` says; `origin`: the saved Desktop the
    /// assignment's stages come from, when one does (each stage built is noted as standing
    /// for its saved stage, for the pool after a restart, `pool.rs`).
    pub(crate) fn launch_with(
        &mut self,
        live: &[WindowMeta],
        assignment: &Assignment,
        frames: LaunchFrames,
        hints: &LaunchHints,
        origin: Option<u64>,
    ) -> Vec<Action> {
        let restore_frames = frames == LaunchFrames::Saved;
        // Saved frames only on a main display of the saved size (else the live ones).
        let matched_frames = frames != LaunchFrames::Live && self.saved_frames_fit();
        let mut actions = Vec::new();
        let metas: HashMap<WindowId, &WindowMeta> = live.iter().map(|m| (m.id, m)).collect();
        let space = self.current_space(self.main_display().id);
        let mut built: Vec<Option<StageId>> = vec![None; assignment.stages.len()];
        let mut placed: HashSet<WindowId> = HashSet::new();
        let saved_clock = assignment.stages.iter().filter_map(|s| s.last_active).max().unwrap_or(0);
        self.clock = self.clock.max(saved_clock);
        // Apps with windows here are running: the entries kept from their quit go (round 8H).
        for m in live.iter().filter(|m| m.role == WindowRole::Standard) {
            self.forget_quit_of(&m.bundle_id);
        }

        for (i, st) in assignment.stages.iter().enumerate() {
            let members: Vec<&AssignedWindow> = st
                .windows
                .iter()
                .filter(|w| {
                    metas.contains_key(&w.id)
                        && !self.windows.contains_key(&w.id)
                        && placed.insert(w.id)
                })
                .collect();
            let declared = st.name.as_deref().is_some_and(|n| self.is_declared(n));
            let standing: Vec<&&AssignedWindow> = members.iter().filter(|w| !w.pinned).collect();
            let pending = self.mark_pending(&st.pending);
            if standing.is_empty() && pending.is_empty() && Some(i) != assignment.active_index && !declared {
                for w in members.iter().filter(|w| w.pinned) {
                    self.insert_restored(metas[&w.id], w, Home::Pinned, matched_frames);
                }
                continue;
            }
            let sid = match st.name.as_deref().and_then(|n| self.find_named(space, n)) {
                Some(s) if !built.contains(&Some(s)) => s,
                _ => self.create_stage(space, st.name.clone()),
            };
            built[i] = Some(sid);
            if let Some(id) = origin {
                self.kin.insert(sid, super::model::Kin::Saved(id, i));
            }
            for w in &members {
                let home = if w.pinned {
                    Home::Pinned
                } else {
                    Home::Stage(sid)
                };
                self.insert_restored(metas[&w.id], w, home, matched_frames);
            }
            for w in &members {
                self.restored.insert(w.id, super::model::Restored { grade: w.grade, z: w.z });
            }
            if !pending.is_empty() {
                self.pending.entry(sid).or_default().extend(pending);
            }
            let ids: Vec<WindowId> = standing.iter().map(|w| w.id).collect();
            let key = standing
                .iter()
                .find(|w| w.key)
                .map(|w| w.id)
                .or(ids.first().copied());
            let t = match st.last_active {
                Some(t) => t,
                None => self.tick(),
            };
            if let Some(stage) = self.stage_mut(sid) {
                stage.windows.extend(ids);
                stage.key = key;
                stage.last_active = t;
            }
        }

        if let Some(sid) = assignment.active_index.and_then(|i| built[i]) {
            let previous = self.active.insert(space, sid);
            let t = self.tick();
            if let Some(stage) = self.stage_mut(sid) {
                stage.last_active = t;
            }
            if let Some(p) = previous.filter(|&p| p != sid) {
                self.drop_if_dead(p, &mut actions);
            }
        }

        self.place_unmatched(space, &metas, assignment, &placed, hints);

        // Every window of every stage clear of the strip, so restored frames already are.
        let ids: Vec<WindowId> = live.iter().map(|m| m.id).collect();
        let mut fitted: Vec<WindowId> = ids.into_iter().filter(|&id| self.fit_window(id)).collect();

        let stage_ids: Vec<StageId> = self.stages_in(space).map(|s| s.id).collect();
        for s in stage_ids {
            if self.is_active(s) {
                if restore_frames {
                    self.show_stage(s, &mut actions);
                } else {
                    self.unstick(s, &mut actions);
                }
            } else {
                self.park_stage(s, &mut actions);
            }
        }
        if restore_frames {
            for id in self.pinned() {
                if self.windows.get(&id).is_some_and(|w| w.space == space) {
                    self.restore_window(id, &mut actions);
                }
            }
        }
        if frames == LaunchFrames::Matched {
            // A window shown at another frame than the saved one it took goes there.
            fitted.extend(live.iter().filter(|m| self.windows.get(&m.id).is_some_and(|w| !same_frame(w.restore, m.frame))).map(|m| m.id));
        }
        for id in fitted {
            let shown = self.windows.get(&id).filter(|w| !w.parked).map(|w| w.restore);
            let done = actions
                .iter()
                .any(|a| matches!(a, Action::SetFrame { id: x, .. } if *x == id));
            if let Some(frame) = shown
                && !done
            {
                actions.push(Action::SetFrame { id, frame });
            }
        }

        for meta in live
            .iter()
            .filter(|m| matches!(m.role, WindowRole::Child { .. }))
        {
            let child_actions =
                self.add_window(meta.clone(), &Decision::default(), &Observation::default());
            actions.extend(
                child_actions
                    .into_iter()
                    .filter(|a| *a != Action::StateChanged),
            );
        }

        let slots: Vec<StageId> = assignment
            .strip_slots
            .iter()
            .filter_map(|&i| built.get(i).copied().flatten())
            .collect();
        if slots.iter().any(|&s| self.has_pending(s)) {
            // Stages still pending have no card yet; when one comes, it takes its saved place.
            self.slot_order.insert(space, slots.clone());
        }
        // Slots of pending stages only (an adoption) never replace the strip shown: no card
        // of theirs exists yet, and the saved order above places them as they come.
        let cards = slots.iter().any(|&s| self.stage(s).is_some_and(|st| !st.windows.is_empty()));
        if (!slots.is_empty() && cards) || !self.slots.contains_key(&space) {
            self.slots.insert(space, slots);
        }
        self.note_front_waits(space);
        self.normalize_slots();
        actions.push(Action::StateChanged);
        actions
    }

    /// Places standard windows no saved stage claims (see [`Model::apply_launch`]).
    fn place_unmatched(
        &mut self,
        space: SpaceId,
        metas: &HashMap<WindowId, &WindowMeta>,
        assignment: &Assignment,
        placed: &HashSet<WindowId>,
        hints: &LaunchHints,
    ) {
        let z = |id: WindowId| {
            hints
                .z_order
                .iter()
                .position(|&x| x == id)
                .unwrap_or(usize::MAX)
        };
        let mut pending: Vec<&WindowMeta> = assignment
            .unmatched
            .iter()
            .filter_map(|id| metas.get(id).copied())
            .filter(|m| !self.windows.contains_key(&m.id) && !placed.contains(&m.id))
            .collect();
        pending.sort_by_key(|m| z(m.id));
        // Apps in recency order: the frontmost app first, then by their frontmost window.
        let mut apps: Vec<(Pid, Vec<&WindowMeta>)> = Vec::new();
        for m in pending {
            match apps.iter_mut().find(|(p, _)| *p == m.pid) {
                Some((_, ws)) => ws.push(m),
                None => apps.push((m.pid, vec![m])),
            }
        }
        if let Some(front) = hints.frontmost
            && let Some(i) = apps.iter().position(|(p, _)| *p == front)
        {
            let app = apps.remove(i);
            apps.insert(0, app);
        }
        let one_at_a_time = self.options.settings.app_windows == AppWindows::OneAtATime;
        let mut fresh: Vec<StageId> = Vec::new();
        for (pid, windows) in apps {
            let existing = self.app_stage(pid, space);
            let units: Vec<Vec<&WindowMeta>> = if existing.is_none() && one_at_a_time {
                windows.into_iter().map(|w| vec![w]).collect()
            } else {
                vec![windows]
            };
            for unit in units {
                let stage = match existing {
                    Some(s) => s,
                    None => {
                        let active = self.ensure_active(space);
                        let reusable = self.stage(active).is_some_and(|s| {
                            s.windows.is_empty() && s.name.is_none() && !fresh.contains(&active)
                        }) && !self.has_pending(active);
                        let s = if reusable {
                            active
                        } else {
                            self.create_stage(space, None)
                        };
                        fresh.push(s);
                        s
                    }
                };
                for meta in unit {
                    self.insert_unmatched(meta, stage, space);
                }
            }
        }
        // Recency: the first fresh stage is the most recent after the active one.
        for &s in fresh.iter().rev() {
            let t = self.tick();
            if let Some(st) = self.stage_mut(s) {
                st.last_active = t;
            }
        }
        let active = self.ensure_active(space);
        // A saved active stage whose windows are still to come keeps its place in a restore
        // (it is the stage the user left in front); else an empty one gives way.
        let waits = self.restore.after_restart_window() && self.has_pending(active);
        if self.stage(active).is_some_and(|s| s.windows.is_empty())
            && !waits
            && let Some(&first) = fresh.first()
            && first != active
        {
            self.active.insert(space, first);
            let mut ignored = Vec::new();
            self.drop_if_dead(active, &mut ignored);
        }
        let active = self.ensure_active(space);
        let t = self.tick();
        if let Some(st) = self.stage_mut(active) {
            st.last_active = t;
        }
    }

    /// The stage holding app `pid`'s windows on `space`: the active one if it does, else
    /// the most recently active.
    fn app_stage(&self, pid: Pid, space: SpaceId) -> Option<StageId> {
        let holds = |s: &&crate::engine::model::Stage| {
            s.space == space
                && s.windows
                    .iter()
                    .any(|w| self.windows.get(w).is_some_and(|w| w.meta.pid == pid))
        };
        let active = self.active_stage_in(space);
        self.stages
            .iter()
            .filter(holds)
            .max_by_key(|s| (Some(s.id) == active, s.last_active))
            .map(|s| s.id)
    }

    fn insert_unmatched(&mut self, meta: &WindowMeta, stage: StageId, space: SpaceId) {
        let display = self.known_display(meta.display);
        let mut meta = meta.clone();
        meta.display = display;
        let id = meta.id;
        let restore = self.safe_restore(meta.frame, display);
        self.windows.insert(
            id,
            WindowState {
                meta,
                home: Home::Stage(stage),
                restore,
                parked: false,
                original: None,
                space,
                sticky: false,
            },
        );
        if let Some(s) = self.stage_mut(stage) {
            s.windows.push(id);
            if s.key.is_none() {
                s.key = Some(id);
            }
        }
    }

    /// Restores members of an active stage whose live frame is in a park zone (left there by a
    /// crash), since they would otherwise stay hidden while counted as visible.
    fn unstick(&mut self, stage: StageId, actions: &mut Vec<Action>) {
        let Some(s) = self.stage(stage) else { return };
        let mut ids = s.windows.clone();
        ids.extend(self.children_in(Home::Stage(stage)));
        for id in ids {
            let stuck = self
                .windows
                .get(&id)
                .is_some_and(|w| self.display(w.meta.display).parks(w.meta.frame));
            if stuck {
                self.restore_window(id, actions);
            }
        }
    }

    fn insert_restored(
        &mut self,
        meta: &WindowMeta,
        saved: &AssignedWindow,
        home: Home,
        restore_frames: bool,
    ) {
        let display = self.known_display(meta.display);
        let live_parked = self.display(display).parks(meta.frame);
        let base = if (restore_frames && saved.grade.confident()) || live_parked || meta.frame.is_degenerate() {
            saved.frame
        } else {
            meta.frame
        };
        let restore = self.safe_restore(base, display);
        // The saved pre-fit frame still applies while the window is where we fitted it.
        let original = saved.original.filter(|_| same_frame(base, saved.frame));
        let space = self.current_space(display);
        let mut meta = meta.clone();
        meta.display = display;
        self.windows.insert(
            meta.id,
            WindowState {
                meta,
                home,
                restore,
                parked: false,
                original,
                space,
                sticky: false,
            },
        );
    }
}

#[cfg(test)]
mod tests;
