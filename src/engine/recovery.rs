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
//! - a stranded window goes back to its frame from `state.json` (exact bundle ID + title
//!   first, then bundle ID only, each saved window used once), else it is centered; the
//!   result is always clamped onto the visible frame and never in a park zone; a saved
//!   window we had fitted clear of the strip goes back to its pre-fit frame instead;
//! - a window that is not stranded but still sits exactly where we fitted it (exact bundle ID
//!   and title, live frame equal to the saved one) goes back to its saved pre-fit frame, so
//!   a crash never leaves a window narrowed.

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
            s.stages
                .iter()
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
            let base = saved.unwrap_or(windows[i].frame);
            let mut frame = clamp_to_visible(base, displays.visible);
            if is_parked(frame, displays.main) {
                frame = clamp_to_visible(Rect::new(f64::NAN, 0.0, 0.0, 0.0), displays.visible);
            }
            (i, frame)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Size;
    use crate::engine::park::park_frame;
    use crate::engine::persist::{STATE_VERSION, SavedStage, SavedWindow};

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
        StateFile {
            version: STATE_VERSION,
            stages: vec![SavedStage {
                name: None,
                windows,
            }],
            active_index: Some(0),
            strip_slots: vec![],
        }
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
}
