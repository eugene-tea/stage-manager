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
//!
//! **A tab selected** (round tab-lost, [`tabs_selected`]): the same signature read the other
//! way. A managed window the model holds on a Desktop shown, now listed off screen, whose
//! exact frame ([`same_place`]) a window of its app not managed takes on screen, is a tab no
//! longer selected beside the tab selected: the stage takes the tab selected in its place
//! ([`crate::engine::model::Model::tab_selected`]), never "left this Space". Its stage may be
//! hidden (round tab-quit): a group parked in the corner whose other tab is selected shows
//! that tab at the park spot (a sliver on screen), the tab left off screen at the same frame;
//! the tab selected is then hidden with its stage and goes back to the group's frame.
//!
//! **Under an engine that hides off screen** (the review of 1.9, C-1): the Spaces engine's
//! hidden window is listed off screen at its own frame, the tab left's look, so a new window
//! its app opens at that frame (a screen-filling window, a cascade clamped to the screen, a
//! fit) showed the signature of a tab switch: it vanished into the hidden stage and the
//! hidden window came out. There the tab left must also be gone from its app's Accessibility
//! list, as a tab ordered out is ([`tabs_selected`]'s `still_listed`); a window in a stage
//! Space stays listed there (measured for the launch hold's Space, `probe-spaces sources
//! --hold-ms`). Under the Corner engine a hidden window is a sliver on screen, so off screen
//! is evidence already.

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

/// True when `a` and `b` are the same frame: same size and same origin, each within
/// [`SAME_SIZE_SLACK`].
pub fn same_place(a: Rect, b: Rect) -> bool {
    same_size(a, b) && (a.x - b.x).abs() <= SAME_SIZE_SLACK && (a.y - b.y).abs() <= SAME_SIZE_SLACK
}

/// A window tab selected (round tab-lost): the tab `selected` now shows where the tab `left`
/// showed, and `left` became a hidden tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabSelected {
    /// The managed window that went off screen: the tab no longer selected.
    pub left: u32,
    /// The window on screen in its place: the tab selected now.
    pub selected: u32,
}

/// The tabs selected in `listed` (the window server's list, every Desktop): each window of
/// `held` (managed standard windows of a Desktop shown, shown or hidden by the engine)
/// listed off screen whose place another window of its app, on screen on layer 0 and not
/// managed (`managed`), takes exactly ([`same_place`]). A window parked in the corner is on
/// screen as a sliver; once its other tab is selected there, it is off screen and the tab
/// selected is the sliver at the same frame. The other hidden tabs of a group are off screen
/// and never count. A tab not selected keeps its
/// last frame, and the tab selected takes its group's frame, which is that same frame: the
/// signature of a tab switch, read from one listing. Two candidates for one place, or one
/// window in two places, say nothing (left to the usual checks).
///
/// `still_listed(left)`, asked only of a tab left whose place is taken: true when its app's
/// Accessibility list still names it (a tab ordered out never is), or the app does not
/// answer; then nothing is told. The caller asks it of a window the engine hides off screen
/// at its own frame (module docs), and answers false for any other.
pub fn tabs_selected(
    listed: &[TabListed],
    held: &[u32],
    managed: impl Fn(u32) -> bool,
    mut still_listed: impl FnMut(&TabListed) -> bool,
) -> Vec<TabSelected> {
    let mut out: Vec<TabSelected> = Vec::new();
    for &id in held {
        let Some(left) = listed.iter().find(|w| w.id == id && w.layer == 0) else { continue };
        if left.onscreen {
            continue;
        }
        let mut places = listed
            .iter()
            .filter(|s| s.id != id && s.pid == left.pid && s.layer == 0 && s.onscreen && !managed(s.id) && same_place(s.bounds, left.bounds));
        let (Some(selected), None) = (places.next(), places.next()) else { continue };
        if still_listed(left) {
            continue;
        }
        out.push(TabSelected { left: id, selected: selected.id });
    }
    // A window selected for two tabs that left: neither is told.
    let twice: Vec<u32> =
        out.iter().map(|t| t.selected).filter(|s| out.iter().filter(|t| t.selected == *s).count() > 1).collect();
    out.retain(|t| !twice.contains(&t.selected));
    out
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

    /// The owner's 11-15 Terminal (no titles): the group's two tabs, 1406×985. 213 in a stage,
    /// shown; 212 selected in its place (213 listed off screen where it was).
    fn terminal_11_15(selected_212: bool) -> Vec<TabListed> {
        vec![
            win(213, 864.0, 215.0, 1406.0, 985.0, !selected_212),
            win(212, 864.0, 215.0, 1406.0, 985.0, selected_212),
            // Another Terminal window of the same size elsewhere, on screen and not managed.
            win(214, 774.0, 179.0, 1406.0, 985.0, true),
        ]
    }

    /// Round tab-lost: a tab selected is told by one listing, the tab it replaces off screen
    /// at its frame and the tab selected on screen at that same frame.
    #[test]
    fn a_tab_selected_takes_the_place_of_the_managed_tab_it_hides() {
        let l = terminal_11_15(true);
        let managed = |id: u32| id == 213;
        assert_eq!(tabs_selected(&l, &[213], managed, |_| false), vec![TabSelected { left: 213, selected: 212 }]);
        // Selected back: 212, now the managed one, leaves; 213 takes its place again.
        let l = terminal_11_15(false);
        assert_eq!(tabs_selected(&l, &[212], |id| id == 212, |_| false), vec![TabSelected { left: 212, selected: 213 }]);
    }

    #[test]
    fn a_managed_window_still_on_screen_or_without_a_window_in_its_place_is_no_tab_switch() {
        let l = terminal_11_15(false);
        assert!(tabs_selected(&l, &[213], |id| id == 213, |_| false).is_empty(), "213 still shows");
        // 213 off screen (moved to another Desktop), nothing at its frame: no tab.
        let l = vec![win(213, 864.0, 215.0, 1406.0, 985.0, false), win(214, 774.0, 179.0, 1406.0, 985.0, true)];
        assert!(tabs_selected(&l, &[213], |id| id == 213, |_| false).is_empty(), "same size elsewhere is not its place");
    }

    #[test]
    fn a_managed_window_in_the_place_another_apps_window_or_a_panel_says_nothing() {
        let l = terminal_11_15(true);
        assert!(tabs_selected(&l, &[213], |id| id == 213 || id == 212, |_| false).is_empty(), "212 managed already");
        let mut other = win(212, 864.0, 215.0, 1406.0, 985.0, true);
        other.pid = 7;
        let l = vec![win(213, 864.0, 215.0, 1406.0, 985.0, false), other];
        assert!(tabs_selected(&l, &[213], |id| id == 213, |_| false).is_empty(), "another app's window");
        let mut panel = win(212, 864.0, 215.0, 1406.0, 985.0, true);
        panel.layer = 3;
        let l = vec![win(213, 864.0, 215.0, 1406.0, 985.0, false), panel];
        assert!(tabs_selected(&l, &[213], |id| id == 213, |_| false).is_empty(), "not a layer-0 window");
    }

    #[test]
    fn two_candidates_for_one_place_or_one_window_for_two_places_say_nothing() {
        let l = vec![
            win(213, 864.0, 215.0, 1406.0, 985.0, false),
            win(212, 864.0, 215.0, 1406.0, 985.0, true),
            win(211, 865.0, 216.0, 1406.0, 985.0, true),
        ];
        assert!(tabs_selected(&l, &[213], |id| id == 213, |_| false).is_empty(), "two windows in its place");
        let l = vec![
            win(213, 864.0, 215.0, 1406.0, 985.0, false),
            win(215, 864.0, 215.0, 1406.0, 985.0, false),
            win(212, 864.0, 215.0, 1406.0, 985.0, true),
        ];
        assert!(tabs_selected(&l, &[213, 215], |id| id == 213 || id == 215, |_| false).is_empty(), "one window for two places");
    }

    /// Round tab-quit (the safety drill after the tabs phase, 11-15): the fixture's group
    /// parked in the corner (Fixture 1 selected, at the park spot, a sliver on screen; Fixture 3
    /// a hidden tab beside it), then Fixture 3 selected while parked. The model holds Fixture 1,
    /// parked: the tab selected at the park spot takes its place; a third tab of the group, off
    /// screen at the same spot, never counts; before the switch nothing is told.
    #[test]
    fn a_tab_selected_while_its_group_is_parked_takes_the_place_of_the_parked_tab() {
        let spot = |id, onscreen| win(id, 2559.0, 1408.0, 520.0, 392.0, onscreen);
        let before = vec![spot(52016, true), spot(52018, false), spot(52019, false), win(52017, 190.0, 148.0, 560.0, 422.0, true)];
        let managed = |id| id == 52016 || id == 52017;
        assert!(tabs_selected(&before, &[52016, 52017], managed, |_| false).is_empty(), "the parked tab is still the selected one");
        let after = vec![spot(52018, true), spot(52016, false), spot(52019, false), win(52017, 190.0, 148.0, 560.0, 422.0, true)];
        assert_eq!(tabs_selected(&after, &[52016, 52017], managed, |_| false), vec![TabSelected { left: 52016, selected: 52018 }]);
    }

    /// The review of 1.9, C-1: under the Spaces engine a hidden stage's window (A) is listed
    /// off screen at its own frame, the screen-filling one, and a new window of its app (B,
    /// Command-N, the cascade clamped to the screen) opens on screen at that frame. Its app
    /// still lists A through Accessibility: no tab was selected, B is a new window. A real tab
    /// switch there (A ordered out, no longer listed) is still told.
    #[test]
    fn a_new_window_at_a_hidden_windows_frame_is_no_tab_while_its_app_still_lists_the_hidden_one() {
        let full = |id, onscreen| win(id, 0.0, 30.0, 2560.0, 1410.0, onscreen);
        // A hidden in a stage Space; C, managed, shown in front at the same frame; B new.
        let l = vec![full(1, false), full(3, true), full(2, true)];
        let managed = |id| id == 1 || id == 3;
        let mut asked = Vec::new();
        let still = |t: &TabListed| {
            asked.push(t.id);
            true
        };
        assert!(tabs_selected(&l, &[1, 3], managed, still).is_empty(), "A still listed: B is a new window");
        assert_eq!(asked, vec![1], "asked only of the window whose place is taken");
        assert_eq!(tabs_selected(&l, &[1, 3], managed, |_| false), vec![TabSelected { left: 1, selected: 2 }], "A ordered out: a tab");
    }

    #[test]
    fn a_place_is_the_frame_within_two_points() {
        assert!(same_place(Rect::new(10.0, 10.0, 800.0, 600.0), Rect::new(12.0, 8.0, 801.0, 599.0)));
        assert!(!same_place(Rect::new(10.0, 10.0, 800.0, 600.0), Rect::new(13.0, 10.0, 800.0, 600.0)));
        assert!(!same_place(Rect::new(10.0, 10.0, 800.0, 600.0), Rect::new(10.0, 10.0, 803.0, 600.0)));
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
