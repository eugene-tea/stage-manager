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
//! today's caution stays), and its app has another window of the same size
//! ([`SAME_SIZE_SLACK`]) that vouches for it: the group's selected tab.
//!
//! **Only by its own signature** (the review of 8J): a real window judged a tab would stay
//! parked with every safety path blind to it, while a tab judged real only keeps a guardian
//! waiting. So what may be judged at all is narrow ([`Candidate`]):
//! - a window we know as real ([`Candidate::Real`]: the model's, one we parked and track,
//!   one the model let go because it left its Space, was hidden or minimized, or for any
//!   reason but the tab's) never is;
//! - a parked window the model let go because Accessibility reported it **destroyed while
//!   its app still listed it in the window server** ([`Candidate::Destroyed`]: how macOS
//!   reports a tab no longer selected) is, beside a same-size window Accessibility lists now
//!   or that the model manages and has not parked;
//! - a window found in the park zone that we never registered nor manage
//!   ([`Candidate::Unknown`]: the owner's 10-16 case) is only beside a same-size window
//!   Accessibility lists now. Saved ids (`state.json`) never vouch: two real windows of the
//!   same size on a Desktop not shown would vouch for each other. When the group's selected
//!   tab is on a Desktop not shown, its hidden tabs wait until that Desktop shows: the safe
//!   failure.

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

/// What may be judged of the window looked at (module docs), beyond the window server's list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Candidate {
    /// A window we know as real: the model manages it, we parked it and track it still, or
    /// the model let it go for any reason but the tab's signature. Never a hidden tab.
    Real,
    /// A parked window the model let go because Accessibility reported it destroyed while
    /// its app still listed it in the window server (a tab no longer selected): a same-size
    /// window Accessibility lists, or one the model manages and has not parked, vouches.
    Destroyed,
    /// A window found in the park zone that we never registered nor manage: only a same-size
    /// window Accessibility lists now vouches.
    Unknown,
}

/// True when `a` and `b` differ in size by at most [`SAME_SIZE_SLACK`] on each side.
pub fn same_size(a: Rect, b: Rect) -> bool {
    (a.w - b.w).abs() <= SAME_SIZE_SLACK && (a.h - b.h).abs() <= SAME_SIZE_SLACK
}

/// The cheap part of [`is_hidden_tab`], before any Accessibility call: layer 0, off screen,
/// not known as real, and its app has another layer-0 window of the same size in `listed`.
pub fn may_be_hidden_tab(w: &TabListed, listed: &[TabListed], candidate: Candidate) -> bool {
    w.layer == 0 && !w.onscreen && candidate != Candidate::Real && siblings(w, listed).next().is_some()
}

/// True when `w` is a hidden tab (module docs): `ax` is its app's window ids as
/// Accessibility lists them now (`None`: the app did not answer); `shown_managed(id)` says
/// whether the model manages window `id` and has not parked it, which vouches only for a
/// [`Candidate::Destroyed`] window (its group's selected tab may be on a Desktop not shown).
pub fn is_hidden_tab(w: &TabListed, listed: &[TabListed], candidate: Candidate, ax: Option<&[u32]>, shown_managed: impl Fn(u32) -> bool) -> bool {
    let Some(ax) = ax else { return false };
    let vouches = |id: u32| ax.contains(&id) || (candidate == Candidate::Destroyed && shown_managed(id));
    may_be_hidden_tab(w, listed, candidate) && !ax.contains(&w.id) && siblings(w, listed).any(|s| vouches(s.id))
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

    /// The model manages 85 and 86 (86 on the Desktop not shown) and has parked neither in
    /// its own eyes but 85 (parked in an inactive stage).
    fn shown_managed(id: u32) -> bool {
        id == 86
    }

    fn judged(id: u32, candidate: Candidate, ax: Option<&[u32]>, shown: impl Fn(u32) -> bool) -> bool {
        let l = listing();
        let w = l.iter().find(|w| w.id == id).copied().expect("listed");
        is_hidden_tab(&w, &l, candidate, ax, shown)
    }

    fn tab(id: u32, ax: Option<&[u32]>, shown: impl Fn(u32) -> bool) -> bool {
        judged(id, Candidate::Unknown, ax, shown)
    }

    /// The 10-16 case: 83 and 84 were never registered. Only a same-size window
    /// Accessibility lists now vouches: 86, once its Desktop shows. While it does not, they
    /// wait (the safe failure); the model managing 86 does not vouch for an unknown window.
    #[test]
    fn the_10_16_terminal_tabs_are_hidden_tabs_once_their_selected_tab_is_listed() {
        assert!(!tab(83, Some(&AX), shown_managed), "86 not listed through Accessibility: 83 waits");
        assert!(!tab(84, Some(&AX), |_| true));
        assert!(tab(83, Some(&[86]), |_| false), "86's Desktop shows: Accessibility lists it");
        assert!(tab(84, Some(&[86]), |_| false));
        // 80–82 beside 85, listed through Accessibility (out of the park zone: no safety path asks).
        assert!(tab(80, Some(&AX), |_| false));
    }

    /// A parked tab the model let go because Accessibility reported it destroyed while the
    /// window server still listed it: its group's selected tab vouches when Accessibility
    /// lists it or the model manages it unparked (on a Desktop not shown, say), never when the
    /// model has it parked.
    #[test]
    fn a_tab_reported_destroyed_is_vouched_for_by_its_shown_selected_tab() {
        assert!(judged(83, Candidate::Destroyed, Some(&AX), shown_managed), "86 managed, not parked");
        assert!(judged(83, Candidate::Destroyed, Some(&[86]), |_| false));
        assert!(!judged(83, Candidate::Destroyed, Some(&AX), |_| false), "nothing vouches");
        assert!(!judged(83, Candidate::Destroyed, Some(&[83, 86]), shown_managed), "listed itself: no tab");
    }

    #[test]
    fn a_real_parked_window_is_never_a_hidden_tab() {
        // 85: on screen (its Desktop shows), listed through Accessibility.
        assert!(!tab(85, Some(&AX), shown_managed));
        assert!(!tab(85, Some(&[]), |_| false), "on screen: never");
    }

    #[test]
    fn a_lone_window_on_another_desktop_is_not_a_tab() {
        assert!(!tab(90, Some(&AX), shown_managed), "no window of its size");
        assert!(!judged(90, Candidate::Destroyed, Some(&AX), |_| true));
    }

    #[test]
    fn an_app_that_does_not_answer_has_no_hidden_tab() {
        assert!(!tab(83, None, shown_managed));
        assert!(!judged(83, Candidate::Destroyed, None, |_| true));
    }

    /// The review of 8J, High: two default-size windows, one parked, which the user drags to
    /// another Desktop in Mission Control. The model lets it go (it left the Space): it stays
    /// real, whatever its same-size sibling, shown and listed through Accessibility, says.
    #[test]
    fn a_parked_window_that_left_its_space_beside_a_same_size_sibling_is_real() {
        let l = vec![win(1, 2559.0, 1408.0, 800.0, 600.0, false), win(2, 300.0, 200.0, 800.0, 600.0, true)];
        assert!(!is_hidden_tab(&l[0], &l, Candidate::Real, Some(&[2]), |_| true));
        assert!(!may_be_hidden_tab(&l[0], &l, Candidate::Real));
    }

    /// The review of 8J, Medium: saved ids never vouch. Two real windows of the same size
    /// parked on a Desktop not shown, the parked file lost (unknown to the guardian), both
    /// saved in `state.json`: Accessibility lists neither, so neither is a tab; both wait.
    #[test]
    fn two_same_size_windows_on_another_desktop_never_vouch_for_each_other() {
        let l = vec![win(1, 2559.0, 1408.0, 800.0, 600.0, false), win(2, 2559.0, 1408.0, 800.0, 600.0, false)];
        for w in &l {
            assert!(!is_hidden_tab(w, &l, Candidate::Unknown, Some(&[]), |_| true), "only Accessibility vouches for an unknown window");
            assert!(!is_hidden_tab(w, &l, Candidate::Destroyed, Some(&[]), |_| false));
            assert!(!is_hidden_tab(w, &l, Candidate::Real, Some(&[]), |_| true));
        }
    }

    #[test]
    fn sizes_match_within_two_points_and_only_in_the_same_app() {
        let u = Candidate::Unknown;
        let l = vec![win(1, 2559.0, 1408.0, 800.0, 600.0, false), win(2, 10.0, 10.0, 802.0, 598.0, true)];
        assert!(is_hidden_tab(&l[0], &l, u, Some(&[2]), |_| false));
        let l = vec![win(1, 2559.0, 1408.0, 800.0, 600.0, false), win(2, 10.0, 10.0, 803.0, 600.0, true)];
        assert!(!is_hidden_tab(&l[0], &l, u, Some(&[2]), |_| false), "3 pt wider");
        let mut other = win(2, 10.0, 10.0, 800.0, 600.0, true);
        other.pid = 7;
        let l = vec![win(1, 2559.0, 1408.0, 800.0, 600.0, false), other];
        assert!(!is_hidden_tab(&l[0], &l, u, Some(&[2]), |_| true), "another app's window");
        let mut panel = win(2, 10.0, 10.0, 800.0, 600.0, true);
        panel.layer = 3;
        let l = vec![win(1, 2559.0, 1408.0, 800.0, 600.0, false), panel];
        assert!(!is_hidden_tab(&l[0], &l, u, Some(&[2]), |_| true), "not a layer-0 window");
    }

    /// The windows the quit's still-parked check leaves on the 10-16 listing (85, shown and
    /// parked in an inactive stage, is brought back by the unpark): `candidate` for 83/84,
    /// Accessibility listing `ax`.
    fn left_at_quit(l: &[TabListed], candidate: Candidate, ax: &[u32]) -> Vec<u32> {
        let d = displays();
        l.iter()
            .filter(|w| w.layer == 0 && w.id != 85 && is_stranded(w.bounds, &d, Known::Managed))
            .filter(|w| {
                let c = if [83, 84].contains(&w.id) { candidate } else { Candidate::Real };
                !is_hidden_tab(w, l, c, Some(ax), shown_managed)
            })
            .map(|w| w.id)
            .collect()
    }

    /// The quit's still-parked check on the 10-16 listing: 83 and 84 are in the park zone (a
    /// sliver of an app we manage counts). Never registered, they are let go once 86's
    /// Desktop shows (Accessibility lists it), and wait before; reported destroyed by a run
    /// that parked them, 86 managed and not parked vouches. The lone window 90 still counts.
    #[test]
    fn the_quit_on_the_10_16_listing() {
        let l = listing();
        assert_eq!(left_at_quit(&l, Candidate::Unknown, &AX), vec![83, 84, 90], "86's Desktop not shown: the tabs wait");
        assert_eq!(left_at_quit(&l, Candidate::Unknown, &[86]), vec![90], "86's Desktop shows");
        assert_eq!(left_at_quit(&l, Candidate::Destroyed, &AX), vec![90]);
        let without_90: Vec<TabListed> = l.into_iter().filter(|w| w.id != 90).collect();
        assert!(left_at_quit(&without_90, Candidate::Unknown, &[86]).is_empty(), "every window back");
        assert!(left_at_quit(&without_90, Candidate::Destroyed, &AX).is_empty(), "every window back");
    }
}
