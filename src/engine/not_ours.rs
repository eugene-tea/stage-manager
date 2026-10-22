//! Another app's window in the park zone that we never parked (round 8M), decided without
//! touching anything: the quit's still-parked check and the guardian leave it alone.
//!
//! Every quit of the owner's week logged ERROR "windows still parked" for windows that were
//! never ours: Terminal's hidden tabs 83 and 84 (off screen at the park spot, their selected
//! tab of the same size on another Desktop, so 8J's test could not vouch for them) and
//! Chrome's helper windows 116 and 117 (layer 0, 1516x139 and 1516x89 at 2658,1424, off
//! screen, never listed through Accessibility). The guardian then waited on them.
//!
//! A window found in the park zone is **not ours** ([`not_ours`]) when all of these hold:
//! - we never registered it (never parked or fitted it in this run) and the model does not
//!   manage it ([`Candidate::Unknown`]); a registered window always waits, 8J's rules apply to
//!   it unchanged;
//! - no `state.json` of this window server session names it (a window parked by an earlier
//!   run of this session, its parked file lost, still waits: the review of 8J's two same-size
//!   windows on a Desktop not shown);
//! - it is on layer 0 and off screen;
//! - its app answers Accessibility and does not list it. An app that does not answer proves
//!   nothing: its windows wait as before.
//!
//! A window we parked is always registered first (the registry is written before each batch
//! of moves), so a window that meets every rule is one macOS or its app put there: a hidden
//! tab, a helper's off-screen window, a window restored from an old park spot. Nothing we do
//! brought it there, and nothing waiting would bring it back.

use super::hidden_tab::{Candidate, TabListed};

/// True when `w`, a window found in the park zone, is another app's off-screen window that
/// is not ours (module docs). `candidate`: what is known of it ([`Candidate`]); `saved`: this
/// session's `state.json` names it; `ax`: its app's window ids as Accessibility lists them now
/// (`None`: the app did not answer).
pub fn not_ours(w: &TabListed, candidate: Candidate, saved: bool, ax: Option<&[u32]>) -> bool {
    candidate == Candidate::Unknown && !saved && w.layer == 0 && !w.onscreen && ax.is_some_and(|ax| !ax.contains(&w.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Rect;

    const TERMINAL: i32 = 501;
    const CHROME: i32 = 502;
    const APP: i32 = 503;

    fn win(id: u32, pid: i32, x: f64, y: f64, w: f64, h: f64, onscreen: bool) -> TabListed {
        TabListed { id, pid, layer: 0, onscreen, bounds: Rect::new(x, y, w, h) }
    }

    /// The 10-16 and 10-21 quits (no titles): Terminal's hidden tabs 83 and 84 at the park
    /// spot beside their selected tab 86 on another Desktop (Accessibility lists only the
    /// Desktop shown: 85), and Chrome's helper windows 116 and 117 off screen past the
    /// display's corner (Accessibility lists Chrome's real windows only). Never registered,
    /// never saved: none of them is ours.
    #[test]
    fn the_week_s_quit_listings_are_not_ours() {
        let terminal_ax = [85];
        for id in [83, 84] {
            let w = win(id, TERMINAL, 2559.0, 1408.0, 1497.0, 1045.0, false);
            assert!(not_ours(&w, Candidate::Unknown, false, Some(&terminal_ax)), "{id}");
        }
        let chrome_ax = [101, 102];
        let helpers = [win(116, CHROME, 2658.0, 1424.0, 1516.0, 139.0, false), win(117, CHROME, 2658.0, 1424.0, 1516.0, 89.0, false)];
        for w in &helpers {
            assert!(not_ours(w, Candidate::Unknown, false, Some(&chrome_ax)), "{}", w.id);
        }
    }

    /// A window we parked, registered, now on another Desktop (off screen, not listed through
    /// Accessibility): it waits, whatever else holds. So does one the model manages.
    #[test]
    fn a_registered_or_managed_parked_window_on_another_desktop_waits() {
        let w = win(1, APP, 2559.0, 1408.0, 800.0, 600.0, false);
        assert!(!not_ours(&w, Candidate::Real, false, Some(&[])));
        assert!(!not_ours(&w, Candidate::Destroyed, false, Some(&[])), "8J's rules for a registered one");
    }

    /// The review of 8J: two windows an earlier run of this session parked on a Desktop not
    /// shown, the parked file lost, both in `state.json`: they wait.
    #[test]
    fn a_window_saved_in_this_session_waits() {
        let w = win(21, APP, 2559.0, 1408.0, 800.0, 600.0, false);
        assert!(!not_ours(&w, Candidate::Unknown, true, Some(&[])));
    }

    #[test]
    fn on_screen_listed_or_unanswered_is_never_judged() {
        let off = win(7, APP, 2559.0, 1408.0, 800.0, 600.0, false);
        assert!(!not_ours(&off, Candidate::Unknown, false, None), "its app did not answer: it waits");
        assert!(!not_ours(&off, Candidate::Unknown, false, Some(&[7])), "listed through Accessibility: reachable");
        let shown = win(7, APP, 2559.0, 1408.0, 800.0, 600.0, true);
        assert!(!not_ours(&shown, Candidate::Unknown, false, Some(&[])), "on screen: the quit's leftovers bring it back");
        let raised = TabListed { layer: 3, ..off };
        assert!(!not_ours(&raised, Candidate::Unknown, false, Some(&[])), "layer 0 only");
    }
}
