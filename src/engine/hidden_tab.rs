//! Hidden tabs (8J), decided without touching anything: a window the safety paths must
//! never wait for.
//!
//! macOS window tabs are separate window-server windows. The tab not selected is ordered
//! out: listed off screen, not listed through Accessibility by its app, and it keeps its
//! last frame until it is selected again, when it takes its group's frame. So a hidden tab
//! can sit in the park zone (the park spot of an earlier run, or of its group's selected
//! tab when it was parked and another tab was then selected) and nothing can move it, nor
//! needs to. The owner's quit on 10-16: Terminal's hidden tabs 83 and 84 at the park spot
//! (off screen, 1497×1045) beside the selected tab 86 of the same size on another Desktop
//! left the exit unclean, and the guardian waited for them forever.
//!
//! A window is a **hidden tab** ([`is_hidden_tab`]) when it is on layer 0, not on screen,
//! not listed through Accessibility by its app (an app that does not answer never has one:
//! today's caution stays), not a window we know as real ([`Candidate::known_real`]: one the
//! model manages, one we parked and still track), and its app has another window of the
//! same size ([`SAME_SIZE_SLACK`]) that Accessibility lists or that we know (the group's
//! selected tab, shown or managed).

use super::geometry::Rect;

/// Two windows are the same size when neither side differs by more than this (points).
pub const SAME_SIZE_SLACK: f64 = 2.0;

/// A window as the window server lists it (any Space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabListed {
    /// `CGWindowID`.
    pub id: u32,
    /// The owning process.
    pub pid: i32,
    /// The window layer (normal windows: 0).
    pub layer: i64,
    /// On screen now.
    pub onscreen: bool,
    /// Its frame.
    pub bounds: Rect,
}

/// What is known of the window looked at, beyond the window server's list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// A window we know as a real one: the model manages it, or we parked it and track it
    /// still (a registered window the model has not let go). Never a hidden tab.
    pub known_real: bool,
}

/// True when `a` and `b` differ in size by at most [`SAME_SIZE_SLACK`] on each side.
pub fn same_size(a: Rect, b: Rect) -> bool {
    (a.w - b.w).abs() <= SAME_SIZE_SLACK && (a.h - b.h).abs() <= SAME_SIZE_SLACK
}

/// The cheap part of [`is_hidden_tab`], before any Accessibility call: layer 0, off screen,
/// not known as real, and its app has another layer-0 window of the same size in `listed`.
pub fn may_be_hidden_tab(w: &TabListed, listed: &[TabListed], candidate: Candidate) -> bool {
    w.layer == 0 && !w.onscreen && !candidate.known_real && siblings(w, listed).next().is_some()
}

/// True when `w` is a hidden tab (module docs): `ax` is its app's window ids as
/// Accessibility lists them (`None`: the app did not answer); `known(id)` says whether we
/// know window `id` as a real window (managed, registered, saved in `state.json` this
/// session), which makes a sibling the group's selected tab even off the Desktop shown.
pub fn is_hidden_tab(w: &TabListed, listed: &[TabListed], candidate: Candidate, ax: Option<&[u32]>, known: impl Fn(u32) -> bool) -> bool {
    let Some(ax) = ax else { return false };
    may_be_hidden_tab(w, listed, candidate) && !ax.contains(&w.id) && siblings(w, listed).any(|s| ax.contains(&s.id) || known(s.id))
}

/// The app's other layer-0 windows of the same size.
fn siblings<'a>(w: &'a TabListed, listed: &'a [TabListed]) -> impl Iterator<Item = &'a TabListed> {
    listed.iter().filter(move |s| s.id != w.id && s.pid == w.pid && s.layer == 0 && same_size(s.bounds, w.bounds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::recovery::{Known, ScanDisplays, is_stranded};

    const MAIN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    const VISIBLE: Rect = Rect::new(0.0, 30.0, 2560.0, 1410.0);
    const TERMINAL: i32 = 501;

    fn displays() -> ScanDisplays {
        ScanDisplays::new(MAIN, VISIBLE, vec![], [])
    }

    fn win(id: u32, x: f64, y: f64, w: f64, h: f64, onscreen: bool) -> TabListed {
        TabListed { id, pid: TERMINAL, layer: 0, onscreen, bounds: Rect::new(x, y, w, h) }
    }

    /// The owner's Terminal on 10-16 (no titles): two tab groups. 86 is the selected tab of
    /// 83/84, on another Desktop; 85 the selected tab of 80–82, parked in an inactive stage
    /// on the Desktop shown. Plus a lone window parked on another Desktop (90).
    fn listing() -> Vec<TabListed> {
        vec![
            win(85, 2559.0, 1408.0, 1357.0, 955.0, true),
            win(86, 532.0, 213.0, 1497.0, 1045.0, false),
            win(83, 2559.0, 1408.0, 1497.0, 1045.0, false),
            win(84, 2559.0, 1408.0, 1497.0, 1045.0, false),
            win(80, 564.0, 181.0, 1357.0, 955.0, false),
            win(81, 564.0, 181.0, 1357.0, 955.0, false),
            win(82, 564.0, 181.0, 1357.0, 955.0, false),
            win(90, 2559.0, 1408.0, 900.0, 600.0, false),
        ]
    }

    /// Accessibility lists the Desktop shown only: 85.
    const AX: [u32; 1] = [85];

    /// The model manages 85 and 86 (86 on the Desktop not shown).
    fn managed(id: u32) -> bool {
        [85, 86].contains(&id)
    }

    const UNKNOWN: Candidate = Candidate { known_real: false };

    fn tab(id: u32, ax: Option<&[u32]>, known: impl Fn(u32) -> bool) -> bool {
        let l = listing();
        let w = l.iter().find(|w| w.id == id).copied().expect("listed");
        is_hidden_tab(&w, &l, UNKNOWN, ax, known)
    }

    #[test]
    fn the_10_16_terminal_tabs_are_hidden_tabs() {
        assert!(tab(83, Some(&AX), managed), "its selected tab 86 is managed");
        assert!(tab(84, Some(&AX), managed));
        // 80–82 too (out of the park zone: no safety path asks).
        assert!(tab(80, Some(&AX), managed), "85, its selected tab, is listed through Accessibility");
    }

    #[test]
    fn a_real_parked_window_is_never_a_hidden_tab() {
        // 85: on screen (its Desktop shows), listed through Accessibility.
        assert!(!tab(85, Some(&AX), managed));
        assert!(!tab(85, Some(&[]), |_| false), "on screen: never");
    }

    #[test]
    fn a_lone_window_on_another_desktop_is_not_a_tab() {
        assert!(!tab(90, Some(&AX), managed), "no window of its size");
        assert!(!tab(90, Some(&AX), |_| true));
    }

    #[test]
    fn an_app_that_does_not_answer_has_no_hidden_tab() {
        assert!(!tab(83, None, managed));
        assert!(!tab(83, None, |_| true));
    }

    #[test]
    fn a_sibling_must_be_listed_through_accessibility_or_known() {
        // 86 neither shown nor known: 83 and 84 only have each other, ordered out alike.
        assert!(!tab(83, Some(&AX), |_| false));
        // Once 86's Desktop shows, Accessibility lists it.
        assert!(tab(83, Some(&[86]), |_| false));
        // A window Accessibility lists is never one.
        assert!(!tab(83, Some(&[83, 86]), managed));
    }

    #[test]
    fn a_window_known_as_real_is_never_a_hidden_tab() {
        // Two real windows of the same size parked on another Desktop, both managed: each
        // is the other's same-size sibling, and neither is a tab.
        let l = vec![win(1, 2559.0, 1408.0, 800.0, 600.0, false), win(2, 2559.0, 1408.0, 800.0, 600.0, false)];
        let real = Candidate { known_real: true };
        assert!(!is_hidden_tab(&l[0], &l, real, Some(&[]), |_| true));
        assert!(!may_be_hidden_tab(&l[1], &l, real));
    }

    #[test]
    fn sizes_match_within_two_points_and_only_in_the_same_app() {
        let l = vec![win(1, 2559.0, 1408.0, 800.0, 600.0, false), win(2, 10.0, 10.0, 802.0, 598.0, true)];
        assert!(is_hidden_tab(&l[0], &l, UNKNOWN, Some(&[2]), |_| false));
        let l = vec![win(1, 2559.0, 1408.0, 800.0, 600.0, false), win(2, 10.0, 10.0, 803.0, 600.0, true)];
        assert!(!is_hidden_tab(&l[0], &l, UNKNOWN, Some(&[2]), |_| false), "3 pt wider");
        let mut other = win(2, 10.0, 10.0, 800.0, 600.0, true);
        other.pid = 7;
        let l = vec![win(1, 2559.0, 1408.0, 800.0, 600.0, false), other];
        assert!(!is_hidden_tab(&l[0], &l, UNKNOWN, Some(&[2]), |_| true), "another app's window");
        let mut panel = win(2, 10.0, 10.0, 800.0, 600.0, true);
        panel.layer = 3;
        let l = vec![win(1, 2559.0, 1408.0, 800.0, 600.0, false), panel];
        assert!(!is_hidden_tab(&l[0], &l, UNKNOWN, Some(&[2]), |_| true), "not a layer-0 window");
    }

    /// The quit's still-parked check on the 10-16 listing: 83 and 84 are in the park zone (a
    /// sliver of an app we manage counts), and they are hidden tabs, so nothing is left: the
    /// quit reports every window back. The lone window 90 still counts.
    #[test]
    fn the_quit_reports_all_back_on_the_10_16_listing() {
        let d = displays();
        let l = listing();
        let stranded: Vec<u32> = l.iter().filter(|w| w.layer == 0 && is_stranded(w.bounds, &d, Known::Managed)).map(|w| w.id).collect();
        assert!(stranded.contains(&83) && stranded.contains(&84), "{stranded:?}");
        let left: Vec<u32> = stranded
            .into_iter()
            .filter(|id| *id != 85) // shown and parked in an inactive stage: the unpark brings it back
            .filter(|id| {
                let w = l.iter().find(|w| w.id == *id).copied().expect("listed");
                !is_hidden_tab(&w, &l, UNKNOWN, Some(&AX), managed)
            })
            .collect();
        assert_eq!(left, vec![90], "only the lone window still parked on another Desktop is left");
        let without_90: Vec<TabListed> = l.into_iter().filter(|w| w.id != 90).collect();
        let left: Vec<u32> = without_90
            .iter()
            .filter(|w| w.id != 85 && is_stranded(w.bounds, &d, Known::Managed))
            .filter(|w| !is_hidden_tab(w, &without_90, UNKNOWN, Some(&AX), managed))
            .map(|w| w.id)
            .collect();
        assert!(left.is_empty(), "the 10-16 quit: every window back ({left:?})");
    }
}
