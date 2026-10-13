//! The recovery scan's decisions: which live windows are stranded in a park zone and where
//! each goes back to.
//!
//! The platform runs the scan at launch, in the guardian process after a crash, and from the
//! panic hook when the registry is unavailable. It reads every window's frame through
//! Accessibility; this module decides, without touching anything:
//!
//! - a window is **stranded** when it sits in the main display's park zone at one of the
//!   corners that count ([`ScanDisplays::corners`]; [`is_parked_in_at`]: the full frame's or
//!   the visible frame's, since macOS keeps a parked window above a bottom Dock) and does not
//!   show on another display either (so a window on a second display to the bottom-right of
//!   the main one is left alone);
//! - a stranded window goes back to its frame from `state.json` (any Space; exact bundle ID + title
//!   first, then bundle ID only, each saved window used once), else it is centered; the
//!   result is always clamped onto the visible frame and never in a park zone; a saved
//!   window we had fitted clear of the strip goes back to its pre-fit frame instead;
//! - a window that is not stranded but still sits exactly where we fitted it (exact bundle ID
//!   and title, live frame equal to the saved one) goes back to its saved pre-fit frame, so
//!   a crash never leaves a window narrowed.
//!
//! The guardian's watch after its scan ([`super::guard_watch`]) works from the windows the
//! crashed app had registered (the parked file, [`Registered`]) and the window server's list
//! on every Space ([`Listed`]).

use serde::{Deserialize, Serialize};

use super::geometry::Rect;
use super::park::{Corner, PARK_MARGIN, choose_corner, clamp_to_visible, is_parked_in_at};
use super::persist::{SavedRole, StateFile};
use super::stage_area::same_frame;

/// A window the scan saw.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveWindow {
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
    /// ([`is_parked_in_at`]), whatever shows on other displays.
    pub fn parks(&self, frame: Rect) -> bool {
        self.corners.iter().any(|c| is_parked_in_at(frame, self.main, self.visible, *c))
    }
}

/// True when `frame` is in the main display's park zone and not meaningfully on any other
/// display (it shows at most [`PARK_MARGIN`] points on both axes there).
pub fn is_stranded(frame: Rect, displays: &ScanDisplays) -> bool {
    if !displays.parks(frame) {
        return false;
    }
    !displays.others.iter().any(|d| {
        frame
            .intersect(d)
            .is_some_and(|seen| seen.w > PARK_MARGIN || seen.h > PARK_MARGIN)
    })
}

/// For every stranded window in `windows`, its index and the frame to move it to.
pub fn plan(
    windows: &[LiveWindow],
    state: Option<&StateFile>,
    displays: &ScanDisplays,
) -> Vec<(usize, Rect)> {
    let saved: Vec<(&str, &str, Rect, Option<Rect>)> = state
        .map(|s| {
            s.all_stages()
                .flat_map(|st| st.windows.iter())
                .filter(|w| w.role == SavedRole::Standard)
                .map(|w| (w.bundle_id.as_str(), w.title.as_str(), w.frame, w.original))
                .collect()
        })
        .unwrap_or_default();
    let stranded: Vec<usize> = windows
        .iter()
        .enumerate()
        .filter(|(_, w)| is_stranded(w.frame, displays))
        .map(|(i, _)| i)
        .collect();
    let mut used = vec![false; saved.len()];
    let mut target: Vec<Option<Rect>> = vec![None; stranded.len()];
    let passes: [fn(&LiveWindow, &str, &str) -> bool; 2] = [
        |w, b, t| w.bundle_id == b && w.title == t,
        |w, b, _| !b.is_empty() && w.bundle_id == b,
    ];
    for pass in passes {
        for (slot, &i) in target.iter_mut().zip(&stranded) {
            if slot.is_some() {
                continue;
            }
            let w = &windows[i];
            if let Some(k) = saved
                .iter()
                .enumerate()
                .position(|(k, (b, t, _, _))| !used[k] && pass(w, b, t))
            {
                used[k] = true;
                *slot = Some(saved[k].3.unwrap_or(saved[k].2));
            }
        }
    }
    let mut out: Vec<(usize, Rect)> = stranded
        .iter()
        .zip(target)
        .map(|(&i, saved)| {
            (i, safe_restore(saved.unwrap_or(windows[i].frame), displays))
        })
        .collect();
    for (i, w) in windows.iter().enumerate() {
        if stranded.contains(&i) {
            continue;
        }
        let found = saved.iter().enumerate().find(|(k, (b, t, f, o))| {
            !used[*k] && o.is_some() && w.bundle_id == *b && w.title == *t && same_frame(w.frame, *f)
        });
        if let Some((k, (_, _, _, Some(o)))) = found {
            used[k] = true;
            let back = if displays.parks(*o) { clamp_to_visible(*o, displays.visible) } else { *o };
            out.push((i, back));
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
        }
    }

    fn state(windows: Vec<SavedWindow>) -> StateFile {
        StateFile::single(SavedSpace {
            id: 1,
            current: true,
            stages: vec![SavedStage {
                name: None,
                windows,
            }],
            active_index: Some(0),
            strip_slots: vec![],
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

    #[test]
    fn a_window_on_a_second_display_is_not_stranded() {
        let mut d = displays();
        d.others.push(Rect::new(1440.0, 600.0, 1920.0, 1080.0));
        let on_second = Rect::new(1500.0, 950.0, 800.0, 600.0);
        assert!(is_parked(on_second, MAIN));
        assert!(!is_stranded(on_second, &d));
        assert!(is_stranded(parked(400.0, 300.0), &displays()));
    }

    /// The review's S1: a 76 pt Dock at the bottom; macOS keeps the parked window's title bar
    /// above it. The scan, the guardian and the clean-quit check must all see it parked.
    #[test]
    fn a_window_parked_above_a_bottom_dock_is_stranded() {
        let d = ScanDisplays::new(MAIN, Rect::new(0.0, 25.0, 1440.0, 799.0), vec![], []);
        let at = Rect::new(1439.0, d.visible.max_y() - MACOS_KEEPS_TITLE, 600.0, 392.0);
        assert!(!is_parked(at, MAIN), "the full frame alone misses it");
        assert!(is_stranded(at, &d));
        let p = plan(&[live("com.example.a", "Draft", at)], None, &d);
        assert_eq!(p.len(), 1);
        assert!(!d.parks(p[0].1) && d.visible.contains_rect(&p[0].1), "{:?}", p[0].1);
        // Same Dock on the right: macOS may or may not clamp x against it.
        let right = ScanDisplays::new(MAIN, Rect::new(0.0, 25.0, 1360.0, 875.0), vec![], []);
        for x in [1439.0, 1359.0] {
            assert!(is_stranded(Rect::new(x, 900.0 - MACOS_KEEPS_TITLE, 600.0, 392.0), &right));
        }
    }

    #[test]
    fn the_parked_file_round_trips_and_reads_the_older_list() {
        let f = ParkedFile {
            visible: Some(Rect::new(0.0, 25.0, 1440.0, 799.0)),
            windows: vec![Registered { id: 4, pid: 9, restore: Rect::new(10.0, 40.0, 300.0, 200.0), child: false, fitted: true }],
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
