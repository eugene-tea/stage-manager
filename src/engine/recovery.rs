//! The recovery scan's decisions: which live windows are stranded in a park zone and where
//! each goes back to.
//!
//! The platform runs the scan at launch, in the guardian process after a crash, and from the
//! panic hook when the registry is unavailable. It reads every window's frame through
//! Accessibility; this module decides, without touching anything:
//!
//! - a window is **stranded** when it sits in the main display's park zone at one of the
//!   corners that count ([`ScanDisplays::corners`]; [`super::park::is_parked_in_at`]: the full frame's or
//!   the visible frame's, since macOS keeps a parked window above a bottom Dock) and does not
//!   show on another display either (so a window on a second display to the bottom-right of
//!   the main one is left alone);
//! - a stranded window goes back to the frame a crashed copy registered for it (its parked
//!   file, by window id: exact, and fresher than any save; S4 of the Fable review of 10-29),
//!   else its frame from `state.json` (any Space; the same window id first, then exact
//!   bundle ID + title, then bundle ID only when one candidate is left, each saved window
//!   used once: [`super::window_match`]; a child only as the very window, by its id), else
//!   it is centered; the result is always clamped onto the visible frame and never in a park
//!   zone; a saved window we had fitted clear of the strip goes back to its pre-fit frame
//!   instead; `state.json` frames only on a main display of the size they were saved on
//!   (S6, as a restore uses them);
//! - a window that is not stranded but still sits exactly where we fitted it (its saved
//!   window matched, live frame equal to the saved one) goes back to its saved pre-fit frame, so
//!   a crash never leaves a window narrowed.
//!
//! The guardian's watch after its scan ([`super::guard_watch`]) works from the windows the
//! crashed app had registered (the parked file, [`Registered`]) and the window server's list
//! on every Space ([`Listed`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::geometry::Rect;
use super::park::{Corner, PARK_MARGIN, Slivers, choose_corner, clamp_to_visible, is_parked_in_at_with};
use super::persist::{SavedRole, SavedWindow, StateFile};
use super::stage_area::same_frame;
use super::window_match::{Key, match_windows};

/// A window the scan saw.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveWindow {
    /// Its `CGWindowID`, when known: matched first against the saved window ids.
    pub id: Option<u32>,
    /// The owning app's bundle ID (empty when unknown).
    pub bundle_id: String,
    /// The window title.
    pub title: String,
    /// The live frame.
    pub frame: Rect,
}

/// The main display as the scan sees it, plus every other display's frame.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanDisplays {
    /// The main display's full frame.
    pub main: Rect,
    /// The main display's visible frame (no menu bar, no Dock).
    pub visible: Rect,
    /// Frames of the other displays.
    pub others: Vec<Rect>,
    /// The park corners that count: bottom-right (Apple's, the default), the arrangement's
    /// choice now ([`choose_corner`]) and every corner the app used this run (its parked
    /// file), sorted, no duplicates.
    pub corners: Vec<Corner>,
}

impl ScanDisplays {
    /// The main display (`main`, `visible`), the `others`, and the corners the app used
    /// (`used`), with bottom-right and the arrangement's corner always counted.
    pub fn new(main: Rect, visible: Rect, others: Vec<Rect>, used: impl IntoIterator<Item = Corner>) -> Self {
        let mut corners = vec![Corner::BottomRight, choose_corner(main, visible, &others).corner];
        corners.extend(used);
        corners.sort();
        corners.dedup();
        Self { main, visible, others, corners }
    }

    /// True when `frame` is in the main display's park zone at any corner that counts
    /// ([`super::park::is_parked_in_at`], slivers included), whatever shows on other
    /// displays. For frames we put windows at (never a sliver) and windows we know.
    pub fn parks(&self, frame: Rect) -> bool {
        self.parks_with(frame, Slivers::Count)
    }

    /// [`Self::parks`], counting slivers or not ([`Slivers`]).
    pub fn parks_with(&self, frame: Rect, slivers: Slivers) -> bool {
        self.corners.iter().any(|c| is_parked_in_at_with(frame, self.main, self.visible, *c, slivers))
    }
}

/// What a park test knows about the window it looks at (the review of 6X, L3 and L4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Known {
    /// Any app's window: a frame clearly in the park zone, no thin sliver
    /// ([`Slivers::Ignored`]: it may be a user's window pushed off the edge), and no helper's
    /// tiny window ([`MIN_STRANDED_SIDE`]).
    Unknown,
    /// A window of an app we manage (one with a window in `state.json`): a sliver counts too.
    Managed,
    /// A window we registered (parked or fitted: the registry, the parked file): a sliver
    /// counts and no size is too small (a small window we parked is still ours).
    Registered,
}

impl Known {
    fn slivers(self) -> Slivers {
        if self == Self::Unknown { Slivers::Ignored } else { Slivers::Count }
    }
}

/// Windows smaller than this on either side are never stranded: no window we park is that
/// small, and helper apps keep invisible ones (1×1 and the like) that would make
/// every safety path wait for them (6X).
pub const MIN_STRANDED_SIDE: f64 = 48.0;

/// True when `frame` is in the main display's park zone and not meaningfully on any other
/// display (it shows at most [`PARK_MARGIN`] points on both axes there), as far as what is
/// `known` of its window allows ([`Known`]: slivers, and helpers' tiny windows,
/// [`MIN_STRANDED_SIDE`]).
pub fn is_stranded(frame: Rect, displays: &ScanDisplays, known: Known) -> bool {
    let tiny = frame.w < MIN_STRANDED_SIDE || frame.h < MIN_STRANDED_SIDE;
    if (tiny && known != Known::Registered) || !displays.parks_with(frame, known.slivers()) {
        return false;
    }
    !displays.others.iter().any(|d| {
        frame
            .intersect(d)
            .is_some_and(|seen| seen.w > PARK_MARGIN || seen.h > PARK_MARGIN)
    })
}

/// Where a frame the scan moves a window to comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameSource {
    /// The frame a crashed copy registered for it (its parked file).
    Registered,
    /// Its window's frame in `state.json` (or its pre-fit frame there).
    Saved,
    /// No saved window matched: centered on the visible frame.
    Centered,
}

impl FrameSource {
    /// A short name for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Saved => "state.json",
            Self::Centered => "centered",
        }
    }
}

/// For every stranded window in `windows`, its index and the frame to move it to.
pub fn plan(
    windows: &[LiveWindow],
    state: Option<&StateFile>,
    displays: &ScanDisplays,
) -> Vec<(usize, Rect)> {
    plan_sourced(windows, state, displays).into_iter().map(|(i, f, _)| (i, f)).collect()
}

/// [`plan_sourced`] with no registered frames.
pub fn plan_sourced(
    windows: &[LiveWindow],
    state: Option<&StateFile>,
    displays: &ScanDisplays,
) -> Vec<(usize, Rect, FrameSource)> {
    plan_with(windows, &BTreeMap::new(), state, displays)
}

/// [`plan`], with where each frame comes from. `registered`: the frames a crashed copy
/// registered (its parked file), by window id, for windows of the process that registered
/// them (the caller checks): a stranded window named there goes back to that frame, a
/// window we parked whatever its size or sliver (S4). Saved windows are matched by
/// [`match_windows`]: the same window id first (ids in `state` must be this session's, as
/// [`StateFile::for_session`] leaves them), then the app and exact title, then the app alone
/// only when one candidate is left on both sides; every live window takes part, so a window
/// still on screen keeps its own saved frame. A saved child (a dialog, a sheet) is matched
/// only as the very window, by its id: its title says little, and it never takes a standard
/// window's place in the app-alone pass (S1). Saved frames apply only on a main display of
/// the size they were saved on ([`StateFile::frames_fit`], S6): else a stranded window is
/// centered at its own size, as a restore leaves windows at the frames their apps give them.
pub fn plan_with(
    windows: &[LiveWindow],
    registered: &BTreeMap<u32, Rect>,
    state: Option<&StateFile>,
    displays: &ScanDisplays,
) -> Vec<(usize, Rect, FrameSource)> {
    let all: Vec<&SavedWindow> = state.map(|s| s.all_stages().flat_map(|st| st.windows.iter()).collect()).unwrap_or_default();
    let (saved, children): (Vec<&SavedWindow>, Vec<&SavedWindow>) = all.into_iter().partition(|w| w.role == SavedRole::Standard);
    let frames_fit = state.is_none_or(|s| s.frames_fit(displays.main));
    // An app with a window in state.json is one we manage: a sliver of its counts (L3); a
    // window a crashed copy registered is ours (any size, a sliver).
    let known = |w: &LiveWindow| {
        if w.id.is_some_and(|id| registered.contains_key(&id)) {
            Known::Registered
        } else if !w.bundle_id.is_empty() && saved.iter().any(|s| s.bundle_id == w.bundle_id) {
            Known::Managed
        } else {
            Known::Unknown
        }
    };
    let stranded: Vec<bool> = windows.iter().map(|w| is_stranded(w.frame, displays, known(w))).collect();
    let live_keys: Vec<Key> = windows.iter().map(|w| Key { id: w.id, bundle: &w.bundle_id, title: &w.title }).collect();
    // Children first, by id only: no title pass (no title given), no app-alone pass.
    let child_keys: Vec<Key> = children.iter().map(|s| Key { id: s.window, bundle: &s.bundle_id, title: "" }).collect();
    let as_child = match_windows(&child_keys, &live_keys, |_| false);
    // A live window found as a saved child is that child: it takes part in no other pass.
    let rest_keys: Vec<Key> = live_keys
        .iter()
        .zip(&as_child)
        .map(|(k, c)| if c.is_some() { Key { id: None, bundle: "", title: "" } } else { *k })
        .collect();
    let saved_keys: Vec<Key> = saved.iter().map(|s| Key { id: s.window, bundle: &s.bundle_id, title: &s.title }).collect();
    let matched = match_windows(&saved_keys, &rest_keys, |i| stranded[i]);
    let mut out = Vec::new();
    for (i, w) in windows.iter().enumerate() {
        let found = matched[i].map(|(k, _)| saved[k]).or_else(|| as_child[i].map(|(k, _)| children[k]));
        if stranded[i] {
            let own = w.id.and_then(|id| registered.get(&id));
            let (frame, source) = match (own, found) {
                (Some(r), _) => (*r, FrameSource::Registered),
                (None, Some(s)) if frames_fit => (s.original.unwrap_or(s.frame), FrameSource::Saved),
                (None, _) => (w.frame, FrameSource::Centered),
            };
            out.push((i, safe_restore(frame, displays), source));
        } else if let Some(s) = found
            && let Some(o) = s.original
            && same_frame(w.frame, s.frame)
        {
            // Still exactly where we fitted it: back to its pre-fit frame.
            let back = if displays.parks(o) { clamp_to_visible(o, displays.visible) } else { o };
            out.push((i, back, FrameSource::Saved));
        }
    }
    out
}

/// A window the app had in its safety registry (parked, or fitted clear of the strip), as
/// written to the parked file for the guardian.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Registered {
    /// `CGWindowID`.
    pub id: u32,
    /// The owning process.
    pub pid: i32,
    /// Where it goes back (already clamped onto the visible display when registered).
    pub restore: Rect,
    /// A child window (dialog, sheet, panel).
    pub child: bool,
    /// A shown window we fitted clear of the strip: `restore` is its exact pre-fit frame, and
    /// it is back only once there (or once the guardian has moved it there).
    #[serde(default)]
    pub fitted: bool,
    /// Taken from a guardian not signed like this app (the Codex review of 10-21, R4;
    /// [`super::handoff::vet`]): nothing vouches that we parked it, so it is any app's window
    /// to the park test (no sliver, no tiny size), never fitted, and reached only while the
    /// window server lists it on screen in the park zone. False in files of earlier versions.
    #[serde(default)]
    pub unvouched: bool,
}

/// The parked file the app keeps for its guardian: the registered windows and the main
/// display's visible frame (the guardian runs without AppKit's screens; with a Dock at the
/// bottom the park zone is measured against the visible frame, so a guess would miss parked
/// windows).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ParkedFile {
    /// The main display's visible frame when written.
    pub visible: Option<Rect>,
    /// The registered windows.
    pub windows: Vec<Registered>,
    /// Every park corner the app used this run ([`ScanDisplays::corners`]); empty in files
    /// of earlier versions (bottom-right only).
    #[serde(default)]
    pub corners: Vec<Corner>,
    /// Apps we hid (`[windows] hide_parked_apps`): the guardian unhides each one still
    /// running under the same bundle id. Empty in files of earlier versions.
    #[serde(default)]
    pub hidden: Vec<HiddenApp>,
}

/// An app we hid, as the parked file records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiddenApp {
    /// Its process id.
    pub pid: i32,
    /// Its bundle id (a pid reused by another app is not unhidden).
    pub bundle: String,
}

impl ParkedFile {
    /// Reads the file's JSON; also the older form, a bare list of windows.
    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Any {
            File(ParkedFile),
            List(Vec<Registered>),
        }
        Ok(match serde_json::from_slice(bytes)? {
            Any::File(f) => f,
            Any::List(windows) => Self { visible: None, windows, corners: Vec::new(), hidden: Vec::new() },
        })
    }
}

/// A window the window server lists (on any Space), as the guardian sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listed {
    /// `CGWindowID`.
    pub id: u32,
    /// The owning process.
    pub pid: i32,
    /// Its frame (the window server keeps another Space's windows' frames).
    pub bounds: Rect,
    /// On screen now: its Space shows.
    pub onscreen: bool,
    /// Its app may be managed (`STAGE_MANAGER_ONLY`, exclusions).
    pub allowed: bool,
}

/// A restore frame clamped onto the visible display and never in a park zone.
pub fn safe_restore(frame: Rect, displays: &ScanDisplays) -> Rect {
    let mut f = clamp_to_visible(frame, displays.visible);
    if displays.parks(f) {
        f = clamp_to_visible(Rect::new(f64::NAN, 0.0, 0.0, 0.0), displays.visible);
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Size;
    use crate::engine::park::{MACOS_KEEPS_TITLE, is_parked, park_frame};
    use crate::engine::persist::{SavedSpace, SavedStage, SavedWindow};

    const MAIN: Rect = Rect::new(0.0, 0.0, 1440.0, 900.0);
    const VISIBLE: Rect = Rect::new(0.0, 25.0, 1440.0, 875.0);

    fn displays() -> ScanDisplays {
        ScanDisplays {
            corners: vec![Corner::BottomRight],
            main: MAIN,
            visible: VISIBLE,
            others: vec![],
        }
    }

    fn live(bundle: &str, title: &str, frame: Rect) -> LiveWindow {
        LiveWindow {
            id: None,
            bundle_id: bundle.into(),
            title: title.into(),
            frame,
        }
    }

    fn saved(bundle: &str, title: &str, frame: Rect) -> SavedWindow {
        SavedWindow {
            bundle_id: bundle.into(),
            title: title.into(),
            frame,
            z: 0,
            key: false,
            pinned: false,
            role: SavedRole::Standard,
            original: None,
            window: None,
            ..Default::default()
        }
    }

    fn state(windows: Vec<SavedWindow>) -> StateFile {
        StateFile::single(SavedSpace {
            id: 1,
            current: true,
            stages: vec![SavedStage {
                name: None,
                windows,
                ..Default::default()
            }],
            active_index: Some(0),
            ..Default::default()
        })
    }

    fn parked(w: f64, h: f64) -> Rect {
        park_frame(Size::new(w, h), MAIN)
    }

    #[test]
    fn fitted_windows_go_back_to_their_pre_fit_frames() {
        let fitted = Rect::new(216.0, 60.0, 1200.0, 700.0);
        let original = Rect::new(20.0, 60.0, 1400.0, 700.0);
        let mut a = saved("com.example.a", "Wide", fitted);
        a.original = Some(original);
        let mut b = saved("com.example.a", "Moved since", fitted);
        b.original = Some(original);
        let mut c = saved("com.example.a", "Parked wide", Rect::new(216.0, 100.0, 1200.0, 600.0));
        c.original = Some(Rect::new(10.0, 100.0, 1400.0, 600.0));
        let st = state(vec![a, b, c]);
        let ws = [
            live("com.example.a", "Wide", fitted),
            live("com.example.a", "Moved since", Rect::new(300.0, 80.0, 1000.0, 700.0)),
            live("com.example.a", "Parked wide", parked(1200.0, 600.0)),
        ];
        let mut p = plan(&ws, Some(&st), &displays());
        p.sort_by_key(|x| x.0);
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!(p[0], (0, original), "exactly the pre-fit frame");
        assert_eq!(p[1].0, 2);
        assert_eq!((p[1].1.w, p[1].1.h), (1400.0, 600.0), "a parked one comes back at its pre-fit size");
    }

    #[test]
    fn only_parked_windows_move() {
        let ws = [
            live("com.example.a", "Notes", Rect::new(100.0, 100.0, 500.0, 400.0)),
            live("com.example.a", "Draft", parked(500.0, 400.0)),
        ];
        let p = plan(&ws, None, &displays());
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].0, 1);
        assert_eq!(p[0].1, Rect::new(470.0, 263.0, 500.0, 400.0), "centered");
    }

    #[test]
    fn saved_frames_win_exact_title_first() {
        let st = state(vec![
            saved("com.example.a", "Other", Rect::new(10.0, 40.0, 300.0, 200.0)),
            saved("com.example.a", "Draft", Rect::new(200.0, 150.0, 500.0, 400.0)),
        ]);
        let ws = [
            live("com.example.a", "Draft", parked(500.0, 400.0)),
            live("com.example.a", "Renamed", parked(300.0, 200.0)),
            live("com.example.b", "Lone", parked(300.0, 200.0)),
        ];
        let p = plan(&ws, Some(&st), &displays());
        assert_eq!(p[0], (0, Rect::new(200.0, 150.0, 500.0, 400.0)));
        assert_eq!(
            p[1],
            (1, Rect::new(10.0, 40.0, 300.0, 200.0)),
            "bundle-only fallback"
        );
        assert_eq!(p[2].0, 2);
        assert!(!is_parked(p[2].1, MAIN));
    }

    /// S1 (the Fable review of 10-29): a parked dialog never matched (standard windows only)
    /// and was always centered. Saved as a child with its id (this session's: a quit, a
    /// crash), it is found as the very window; a child never matches by title, nor takes
    /// the app-alone pass from a standard window, nor is a standard window taken for one.
    #[test]
    fn a_parked_child_is_found_as_the_very_window() {
        let dialog_at = Rect::new(500.0, 300.0, 420.0, 180.0);
        let doc_at = Rect::new(200.0, 150.0, 900.0, 600.0);
        let mut dialog = saved("com.example.a", "Save", dialog_at);
        dialog.role = SavedRole::Child;
        dialog.window = Some(77);
        let mut untitled = saved("com.example.a", "", Rect::new(40.0, 60.0, 300.0, 200.0));
        untitled.role = SavedRole::Child;
        let st = state(vec![saved("com.example.a", "Report", doc_at), dialog, untitled]);
        let mut d = live("com.example.a", "Save", parked(420.0, 180.0));
        d.id = Some(77);
        let p = plan_sourced(&[d.clone()], Some(&st), &displays());
        assert_eq!(p, vec![(0, dialog_at, FrameSource::Saved)], "its own saved frame, not the center");
        // Another dialog with the same title and no saved id: never the child's frame by
        // title (and two of the app's windows want the one standard entry: neither takes it).
        let other = live("com.example.a", "Save", parked(420.0, 180.0));
        let doc = live("com.example.a", "Renamed", parked(900.0, 600.0));
        let p = plan_sourced(&[other, doc], Some(&st), &displays());
        assert_eq!(p.iter().map(|x| x.2).collect::<Vec<_>>(), [FrameSource::Centered, FrameSource::Centered], "{p:?}");
        // The very dialog beside its document: each its own, the document by app alone (the
        // dialog found as a child no longer wants the standard entry).
        let p = plan_sourced(&[d, live("com.example.a", "Renamed", parked(900.0, 600.0))], Some(&st), &displays());
        assert_eq!(p, vec![(0, dialog_at, FrameSource::Saved), (1, doc_at, FrameSource::Saved)], "each its own");
    }

    /// S4 (the Fable review of 10-29): after a crash the scan used `state.json` (a save up to
    /// a debounce old, a retitled window by app alone, a child never) and centered what it
    /// could not match. A window the crashed copy registered goes back to its registered
    /// frame first, by id, however small or thin a sliver of it shows.
    #[test]
    fn a_registered_window_goes_back_to_its_registered_frame_first() {
        let reg_at = Rect::new(120.0, 90.0, 800.0, 500.0);
        let mut saved_w = saved("com.example.a", "Draft", Rect::new(10.0, 40.0, 800.0, 500.0));
        saved_w.window = Some(5);
        let st = state(vec![saved_w]);
        let mut a = live("com.example.a", "Draft", parked(800.0, 500.0));
        a.id = Some(5);
        let mut b = live("com.example.b", "", Rect::new(1439.0, 899.0, 30.0, 30.0));
        b.id = Some(6);
        let registered = BTreeMap::from([(5, reg_at), (6, Rect::new(300.0, 300.0, 30.0, 30.0))]);
        let p = plan_with(&[a.clone(), b], &registered, Some(&st), &displays());
        assert_eq!(p, vec![(0, reg_at, FrameSource::Registered), (1, Rect::new(300.0, 300.0, 30.0, 30.0), FrameSource::Registered)]);
        assert_eq!(plan_sourced(&[a], Some(&st), &displays())[0].2, FrameSource::Saved, "without it: state.json");
    }

    /// S6 (the Fable review of 10-29): `state.json` frames saved on a main display of another
    /// size are not used by the scan (as a restore does not use them); the stranded window is
    /// centered at its own size. A file that does not say keeps them.
    #[test]
    fn saved_frames_of_another_display_size_are_not_used() {
        let at = Rect::new(1700.0, 900.0, 500.0, 400.0);
        let mut st = state(vec![saved("com.example.a", "Draft", at)]);
        st.display = Some(crate::engine::persist::SavedDisplay { w: 2560.0, h: 1440.0, key: None });
        let ws = [live("com.example.a", "Draft", parked(500.0, 400.0))];
        let p = plan_sourced(&ws, Some(&st), &displays());
        assert_eq!(p, vec![(0, Rect::new(470.0, 263.0, 500.0, 400.0), FrameSource::Centered)]);
        st.display = Some(crate::engine::persist::SavedDisplay { w: 1440.2, h: 899.8, key: None });
        assert_eq!(plan_sourced(&ws, Some(&st), &displays())[0].2, FrameSource::Saved, "the same size, rounded");
        st.display = None;
        assert_eq!(plan_sourced(&ws, Some(&st), &displays())[0].2, FrameSource::Saved, "a file that does not say");
    }

    /// Grok High 2 (1.1.0): two stranded windows of one app whose titles changed keep their
    /// own frames when their ids were saved (this session's), whatever the saved order.
    #[test]
    fn stranded_windows_are_found_by_window_id_first() {
        let (a, b) = (Rect::new(10.0, 40.0, 300.0, 200.0), Rect::new(200.0, 150.0, 500.0, 400.0));
        let mut first = saved("com.example.a", "Draft", a);
        first.window = Some(41);
        let mut second = saved("com.example.a", "Notes", b);
        second.window = Some(42);
        let st = state(vec![first, second]);
        let mut x = live("com.example.a", "Renamed", parked(500.0, 400.0));
        x.id = Some(42);
        let mut y = live("com.example.a", "", parked(300.0, 200.0));
        y.id = Some(41);
        let p = plan_sourced(&[x, y], Some(&st), &displays());
        assert_eq!(p, vec![(0, b, FrameSource::Saved), (1, a, FrameSource::Saved)]);
    }

    /// Grok High 2: without ids or titles to tell them apart, two stranded windows of one app
    /// are centered, never handed each other's (or a window on screen's) frame.
    #[test]
    fn app_alone_never_guesses_between_windows() {
        let (a, b) = (Rect::new(10.0, 40.0, 300.0, 200.0), Rect::new(200.0, 150.0, 500.0, 400.0));
        let st = state(vec![saved("com.example.a", "Draft", a), saved("com.example.a", "Notes", b)]);
        let ws = [live("com.example.a", "One", parked(500.0, 400.0)), live("com.example.a", "Two", parked(300.0, 200.0))];
        let p = plan_sourced(&ws, Some(&st), &displays());
        assert!(p.iter().all(|x| x.2 == FrameSource::Centered), "{p:?}");
        // One window on screen holds its own entry (by title): the stranded one has the one
        // candidate left, its own.
        let ws = [live("com.example.a", "Draft", a), live("com.example.a", "Renamed", parked(500.0, 400.0))];
        let p = plan_sourced(&ws, Some(&st), &displays());
        assert_eq!(p, vec![(1, b, FrameSource::Saved)]);
        // One window on screen not in state.json at all: two saved entries left, centered.
        let ws = [live("com.example.a", "New", Rect::new(50.0, 60.0, 400.0, 300.0)), live("com.example.a", "Renamed", parked(500.0, 400.0))];
        let p = plan_sourced(&ws, Some(&st), &displays());
        assert_eq!(p.iter().map(|x| (x.0, x.2)).collect::<Vec<_>>(), vec![(1, FrameSource::Centered)]);
    }

    #[test]
    fn a_window_on_a_second_display_is_not_stranded() {
        let mut d = displays();
        d.others.push(Rect::new(1440.0, 600.0, 1920.0, 1080.0));
        let on_second = Rect::new(1500.0, 950.0, 800.0, 600.0);
        assert!(is_parked(on_second, MAIN));
        assert!(!is_stranded(on_second, &d, Known::Unknown));
        assert!(is_stranded(parked(400.0, 300.0), &displays(), Known::Unknown));
    }

    /// 6X: a helper's tiny window in the corner is never stranded (no window we park is that
    /// small); a real one there is.
    #[test]
    fn tiny_windows_are_never_stranded() {
        let d = displays();
        assert!(!is_stranded(Rect::new(1439.0, 899.0, 1.0, 1.0), &d, Known::Unknown));
        assert!(!is_stranded(Rect::new(1439.0, 868.0, 40.0, 40.0), &d, Known::Unknown));
        assert!(is_stranded(Rect::new(1439.0, 868.0, 300.0, 200.0), &d, Known::Unknown));
        let p = plan(&[live("com.example.helper", "", Rect::new(1439.0, 899.0, 1.0, 1.0))], None, &d);
        assert!(p.is_empty(), "the scan leaves it alone");
    }

    /// The review of 6X, L3 and L4: a sliver counts for a window of an app we manage or one
    /// we registered, not for any app's; a small window we registered is still ours.
    #[test]
    fn what_is_known_of_a_window_decides_slivers_and_size() {
        let d = displays();
        let sliver = Rect::new(1439.0, 820.0, 600.0, 392.0);
        assert!(!is_stranded(sliver, &d, Known::Unknown), "maybe a user's window pushed off the edge");
        assert!(is_stranded(sliver, &d, Known::Managed));
        assert!(is_stranded(sliver, &d, Known::Registered));
        let small = Rect::new(1439.0, 868.0, 40.0, 30.0);
        assert!(!is_stranded(small, &d, Known::Unknown));
        assert!(!is_stranded(small, &d, Known::Managed));
        assert!(is_stranded(small, &d, Known::Registered));
        // The scan: the sliver of an app in state.json comes back, another app's stays.
        let st = state(vec![saved("com.example.a", "Draft", Rect::new(100.0, 100.0, 600.0, 392.0))]);
        let p = plan(&[live("com.example.a", "Other", sliver), live("com.example.b", "Draft", sliver)], Some(&st), &d);
        assert_eq!(p.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![0]);
    }

    /// The review's S1: a 76 pt Dock at the bottom; macOS keeps the parked window's title bar
    /// above it. The scan, the guardian and the clean-quit check must all see it parked.
    #[test]
    fn a_window_parked_above_a_bottom_dock_is_stranded() {
        let d = ScanDisplays::new(MAIN, Rect::new(0.0, 25.0, 1440.0, 799.0), vec![], []);
        let at = Rect::new(1380.0, d.visible.max_y() - MACOS_KEEPS_TITLE, 600.0, 392.0);
        assert!(!is_parked(at, MAIN), "the full frame alone misses it");
        assert!(is_stranded(at, &d, Known::Unknown));
        let p = plan(&[live("com.example.a", "Draft", at)], None, &d);
        assert_eq!(p.len(), 1);
        assert!(!d.parks(p[0].1) && d.visible.contains_rect(&p[0].1), "{:?}", p[0].1);
        // Same Dock on the right: macOS may or may not clamp x against it.
        let right = ScanDisplays::new(MAIN, Rect::new(0.0, 25.0, 1360.0, 875.0), vec![], []);
        for x in [1439.0, 1359.0] {
            assert!(is_stranded(Rect::new(x, 900.0 - MACOS_KEEPS_TITLE, 600.0, 392.0), &right, Known::Unknown));
        }
    }

    #[test]
    fn the_parked_file_round_trips_and_reads_the_older_list() {
        let f = ParkedFile {
            visible: Some(Rect::new(0.0, 25.0, 1440.0, 799.0)),
            windows: vec![Registered { id: 4, pid: 9, restore: Rect::new(10.0, 40.0, 300.0, 200.0), child: false, fitted: true, unvouched: false }],
            corners: vec![Corner::BottomLeft],
            hidden: vec![HiddenApp { pid: 12, bundle: "com.example.a".into() }],
        };
        let json = serde_json::to_vec(&f).unwrap_or_default();
        assert_eq!(ParkedFile::from_json(&json).ok(), Some(f.clone()));
        let old = serde_json::to_vec(&f.windows).unwrap_or_default();
        assert_eq!(ParkedFile::from_json(&old).ok(), Some(ParkedFile { visible: None, corners: vec![], hidden: vec![], ..f.clone() }));
        // A file of the version before corners: bottom-right only.
        let before = br#"{"visible":null,"windows":[]}"#;
        assert_eq!(ParkedFile::from_json(before).ok().map(|f| f.corners), Some(vec![]));
        assert!(serde_json::to_string(&f).unwrap_or_default().contains(r#""corners":["bottom-left"]"#));
        assert!(ParkedFile::from_json(b"{").is_err());
    }

    #[test]
    fn results_are_clamped_and_never_parked() {
        let st = state(vec![saved(
            "com.example.a",
            "Huge",
            Rect::new(-500.0, -500.0, 5000.0, 5000.0),
        )]);
        let ws = [live("com.example.a", "Huge", parked(800.0, 600.0))];
        let p = plan(&ws, Some(&st), &displays());
        assert_eq!(p[0].1, VISIBLE);
        let bad = state(vec![saved("com.example.a", "X", parked(400.0, 300.0))]);
        let ws = [live("com.example.a", "X", parked(400.0, 300.0))];
        let p = plan(&ws, Some(&bad), &displays());
        assert!(!is_parked(p[0].1, MAIN) && VISIBLE.contains_rect(&p[0].1));
    }
}
