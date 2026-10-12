//! The recovery scan's decisions: which live windows are stranded in a park zone and where
//! each goes back to.
//!
//! The platform runs the scan at launch, in the guardian process after a crash, and from the
//! panic hook when the registry is unavailable. It reads every window's frame through
//! Accessibility; this module decides, without touching anything:
//!
//! - a window is **stranded** when it sits in the main display's park zone
//!   ([`is_parked`]) and does not show on another display either (so a window on a second
//!   display to the bottom-right of the main one is left alone);
//! - a stranded window goes back to its frame from `state.json` (any Space; exact bundle ID + title
//!   first, then bundle ID only, each saved window used once), else it is centered; the
//!   result is always clamped onto the visible frame and never in a park zone; a saved
//!   window we had fitted clear of the strip goes back to its pre-fit frame instead;
//! - a window that is not stranded but still sits exactly where we fitted it (exact bundle ID
//!   and title, live frame equal to the saved one) goes back to its saved pre-fit frame, so
//!   a crash never leaves a window narrowed.
//!
//! The guardian also needs to know which windows still wait after its scan
//! ([`stranded_windows`]): windows the crashed app had registered as parked (the parked
//! file, [`Registered`]) and any allowed app's window, still in the park zone, on any Space.
//! A window on a Space not shown is out of Accessibility's reach for most apps; it can only
//! be moved once its Space shows (the window server lists it on screen again).

use serde::{Deserialize, Serialize};

use super::geometry::Rect;
use super::park::{PARK_MARGIN, clamp_to_visible, is_parked};
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
}

/// True when `frame` is in the main display's park zone and not meaningfully on any other
/// display (it shows at most [`PARK_MARGIN`] points on both axes there).
pub fn is_stranded(frame: Rect, displays: &ScanDisplays) -> bool {
    if !is_parked(frame, displays.main) {
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
            let back = if is_parked(*o, displays.main) { clamp_to_visible(*o, displays.visible) } else { *o };
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

/// A window still stranded in the park zone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stranded {
    /// `CGWindowID`.
    pub id: u32,
    /// The owning process.
    pub pid: i32,
    /// On screen now (its Space shows): Accessibility can reach it.
    pub shown: bool,
    /// Where it goes back: the registered frame, made safe ([`safe_restore`]); `None` for a
    /// window the app had not registered (the recovery scan decides).
    pub restore: Option<Rect>,
}

/// Every layer-0 window in `listed` that is stranded: registered ones (whatever their app)
/// and those of allowed apps. A registered window no longer listed was closed: it waits for
/// nothing.
pub fn stranded_windows(listed: &[Listed], registered: &[Registered], displays: &ScanDisplays) -> Vec<Stranded> {
    listed
        .iter()
        .filter(|w| is_stranded(w.bounds, displays))
        .filter_map(|w| {
            let reg = registered.iter().find(|r| r.id == w.id);
            (reg.is_some() || w.allowed).then(|| Stranded {
                id: w.id,
                pid: w.pid,
                shown: w.onscreen,
                restore: reg.map(|r| safe_restore(r.restore, displays)),
            })
        })
        .collect()
}

/// A restore frame clamped onto the visible display and never in a park zone.
pub fn safe_restore(frame: Rect, displays: &ScanDisplays) -> Rect {
    let mut f = clamp_to_visible(frame, displays.visible);
    if is_parked(f, displays.main) {
        f = clamp_to_visible(Rect::new(f64::NAN, 0.0, 0.0, 0.0), displays.visible);
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Size;
    use crate::engine::park::park_frame;
    use crate::engine::persist::{SavedSpace, SavedStage, SavedWindow};

    const MAIN: Rect = Rect::new(0.0, 0.0, 1440.0, 900.0);
    const VISIBLE: Rect = Rect::new(0.0, 25.0, 1440.0, 875.0);

    fn displays() -> ScanDisplays {
        ScanDisplays {
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

    fn listed(id: u32, bounds: Rect, onscreen: bool, allowed: bool) -> Listed {
        Listed { id, pid: 7, bounds, onscreen, allowed }
    }

    #[test]
    fn registered_windows_wait_until_they_leave_the_park_zone() {
        let restore = Rect::new(300.0, 200.0, 600.0, 400.0);
        let reg = [Registered { id: 1, pid: 7, restore, child: false }];
        // Parked on a Space not shown: waits, not shown, with its registered frame.
        let away = [listed(1, parked(600.0, 400.0), false, false)];
        assert_eq!(
            stranded_windows(&away, &reg, &displays()),
            vec![Stranded { id: 1, pid: 7, shown: false, restore: Some(restore) }],
            "a registered window counts even when its app is not on the allowed list"
        );
        // Its Space shows: still waiting, now reachable.
        let here = [listed(1, parked(600.0, 400.0), true, false)];
        assert!(stranded_windows(&here, &reg, &displays())[0].shown);
        // Back on screen, or closed: nothing waits.
        assert!(stranded_windows(&[listed(1, restore, true, true)], &reg, &displays()).is_empty());
        assert!(stranded_windows(&[], &reg, &displays()).is_empty());
    }

    #[test]
    fn unregistered_windows_wait_only_for_allowed_apps() {
        let ws = [listed(2, parked(400.0, 300.0), false, true), listed(3, parked(400.0, 300.0), false, false)];
        let s = stranded_windows(&ws, &[], &displays());
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].id, s[0].restore), (2, None));
    }

    #[test]
    fn registered_restore_frames_are_made_safe() {
        let reg = [Registered { id: 1, pid: 7, restore: parked(400.0, 300.0), child: false }];
        let s = stranded_windows(&[listed(1, parked(400.0, 300.0), true, true)], &reg, &displays());
        let f = s[0].restore.unwrap_or_default();
        assert!(!is_parked(f, MAIN) && VISIBLE.contains_rect(&f), "{f:?}");
    }
}
