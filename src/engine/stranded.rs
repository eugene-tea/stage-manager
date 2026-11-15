//! Windows stranded on screen (round stage-lost), decided without touching anything.
//!
//! The owner's 11-15 07:35: two VS Code windows left their stage with no log line (taken for
//! windows on every Desktop after a slide away and back, [`crate::engine::model::Model::set_slide_carried`]).
//! They stayed on screen over every stage after: a card click brought the next stage in on
//! top of them, a drag of one onto the strip did nothing. Whatever lets a window go, one the
//! user sees in front with no stage of its own must not stay so: at a switch whose front
//! stage shows nothing, and at the user's own act on such a window (a drag onto the strip, a
//! send), it is taken into the stage in front first, and the switch or the act goes on as
//! for any managed window.
//!
//! **Stranded**: a standard window on a managed screen, listed on screen, that Accessibility
//! tracks, of an app the model manages another window of, that the model does not manage
//! and does not hold waiting (`unplaced`: those are offered at every Space check, with their
//! own reasons), no rule ignores, the engine is not placing as new, and that has been
//! tracked for [`SETTLED`] at least (a window just opened is the new-window path's: it may
//! not have been reported yet, and its app's rule decides where it goes).

use std::time::Duration;

/// How long a window must have been tracked before it counts as stranded (a new window's
/// own path places it well within this).
pub const SETTLED: Duration = Duration::from_secs(3);

/// What the platform knows of one window on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    pub id: u32,
    /// A standard window (not a dialog, sheet or panel).
    pub standard: bool,
    /// Listed on screen on a screen the app manages.
    pub on_managed_screen: bool,
    /// Managed by the model.
    pub managed: bool,
    /// Waiting with the windows not placed yet.
    pub waiting: bool,
    /// A rule says to leave it alone.
    pub ignored: bool,
    /// The engine holds it on its way in (a launch's new window).
    pub arriving: bool,
    /// Its app has another window the model manages.
    pub app_managed: bool,
    /// How long Accessibility has tracked it.
    pub tracked: Duration,
}

impl Seen {
    /// True when the window is stranded (see the module notes).
    pub fn stranded(&self) -> bool {
        self.standard
            && self.on_managed_screen
            && !self.managed
            && !self.waiting
            && !self.ignored
            && !self.arriving
            && self.app_managed
            && self.tracked >= SETTLED
    }
}

/// The stranded windows of `seen`, in its order.
pub fn stranded(seen: &[Seen]) -> Vec<u32> {
    seen.iter().filter(|s| s.stranded()).map(|s| s.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(id: u32) -> Seen {
        Seen {
            id,
            standard: true,
            on_managed_screen: true,
            managed: false,
            waiting: false,
            ignored: false,
            arriving: false,
            app_managed: true,
            tracked: Duration::from_secs(30),
        }
    }

    /// The 11-15 case: two windows of an app with a third window managed, on screen, known
    /// to Accessibility for minutes, managed no more.
    #[test]
    fn a_known_apps_window_on_screen_that_nothing_manages_is_stranded() {
        assert_eq!(stranded(&[editor(49841), editor(49983)]), vec![49841, 49983]);
    }

    #[test]
    fn windows_with_a_reason_are_not_stranded() {
        let cases = [
            Seen { managed: true, ..editor(1) },
            Seen { waiting: true, ..editor(2) },
            Seen { ignored: true, ..editor(3) },
            Seen { arriving: true, ..editor(4) },
            Seen { standard: false, ..editor(5) },
            Seen { on_managed_screen: false, ..editor(6) },
            Seen { app_managed: false, ..editor(7) },
            Seen { tracked: SETTLED - Duration::from_millis(1), ..editor(8) },
        ];
        assert!(stranded(&cases).is_empty(), "{cases:?}");
        assert!(Seen { tracked: SETTLED, ..editor(9) }.stranded(), "tracked long enough");
    }
}
