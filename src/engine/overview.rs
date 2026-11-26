//! Mission Control, App Exposé and Show Desktop, told from the window list; and the two
//! guards that keep them from being read as a Space change.
//!
//! While one of them shows, and for a moment while it animates out, the window server lists
//! windows on screen that the shown Desktop does not hold: App Exposé lists the app's
//! windows of other Desktops, Mission Control lists every window moved into its layout, Show
//! Desktop slides the Desktop's windows off the display (measured 10-12: on Desktop 2, App
//! Exposé then Escape listed three of Desktop 1's windows on screen; the once-a-second check
//! took that for a Desktop change and recognized Desktop 2 as Desktop 1, merging their
//! stages).
//!
//! **Telling them** ([`overview_in`]): each one puts a window of macOS's own over the whole
//! display above the windows' layer, measured on macOS 27 with Apple's Stage Manager off:
//! Mission Control and App Exposé a `Dock` window at layer 20 and a `WindowManager` one at
//! layer 19 (the `WindowManager` one goes first when it closes, the `Dock` one ~0.5 s after
//! Escape, when the windows are back in place); Show Desktop a `WindowManager` window at
//! layer 18 for as long as the desktop is revealed. The `Dock` window alone is not one of
//! them: an auto-hiding Dock lists the same full-display layer-20 window for as long as the
//! pointer brings it up (measured 10-12 on the owner's Mac: 2.9 s while the pointer rested
//! on the bottom edge, no `WindowManager` window with it; the owner's Dock clicks to another Desktop
//! were read as Mission Control, round 6R). So only `WindowManager`'s window begins one; the
//! `Dock` window alone only carries a Mission Control already shown through its exit
//! ([`OverviewWatch::see`]). With none of them up, neither process lists a window covering
//! the display above layer 0. Owner names are process names, not localized.
//!
//! **Freezing** ([`OverviewWatch`]): while one shows and for [`OVERVIEW_SETTLE`] after it
//! closes, nothing infers a Space from the window list (no recognizing the Space again, no
//! window counted gone or moved, no window placed). A Space change notified meanwhile (the
//! user clicked another Desktop in Mission Control) stays authoritative: it is handled once
//! the overview has closed and [`OVERVIEW_ARRIVAL_SETTLE`] has passed, when the list shows the
//! Desktop that macOS settled on.
//!
//! **Expecting** ([`OverviewWatch::expect_show_desktop`]): a wallpaper click macOS answers
//! with Show Desktop is known before the overview is listed (~0.7 s after the click): the
//! corner patch drawn from the file's picture hides at the click (one drawn from a still of the
//! desktop stays: [`super::show_desktop`]). The expectation lasts [`REVEAL_EXPECTED`], a
//! timer, since no event says macOS showed nothing, and ends when Show Desktop is listed.
//!
//! **The window server's notice** ([`OverviewWatch::told`], round r5-notices; under every
//! engine of ours since round looks-forward): type 1508 comes
//! at Mission Control's and App Exposé's open (~40 ms after the key) and close (~32 ms; the
//! list shows its window gone ~320 ms after, the Dock's cover ~610 ms), whatever opened them.
//! It is a trigger for a look, never a fact: the look at it finds an overview opened by a
//! gesture or a hot corner within a few ms instead of at the tick ([`OVERVIEW_LOOK`]). One heard
//! while an overview shows ends the settle after its close early: the first look that then
//! finds macOS's own window and the Dock's cover both gone ends the freeze
//! ([`OverviewWatch::settled_by_notice`]); without it, [`OVERVIEW_SETTLE`] and
//! [`OVERVIEW_ARRIVAL_SETTLE`] run as before, and stay the caps.
//!
//! **Confirming** ([`Persisting`]): evidence from the window list (windows of another Space
//! on screen) counts only when the same evidence holds at two checks at least a settle apart,
//! never from one listing.

use std::time::{Duration, Instant};

use super::fullscreen::covers_display;
use super::geometry::Rect;

/// Space inference stays frozen this long after an overview closes (the list may still show
/// windows on their way back).
pub const OVERVIEW_SETTLE: Duration = Duration::from_secs(1);

/// A Space change notified while an overview showed is handled this long after it closes
/// (measured: the windows are in place when the last of its windows goes).
pub const OVERVIEW_ARRIVAL_SETTLE: Duration = Duration::from_millis(300);

/// An overview listed without a break for this long is taken for something else (a window of
/// macOS's we do not know): it no longer freezes anything, so the app can never stay frozen.
pub const OVERVIEW_STUCK: Duration = Duration::from_secs(300);

/// The Dock's cover alone carries a Mission Control at most this long after WindowManager's
/// window was last listed (measured: its exit lists the Dock's ~0.5 s after Escape; longer,
/// the pointer is holding the Dock up).
pub const OVERVIEW_EXIT_CARRY: Duration = Duration::from_millis(1500);

/// The longest an overview opened by a gesture, a hot corner or the Dock's item waits for a
/// look with no notice and no key heard: the tick at rest ([`super::tick_pace::REST`]).
pub const OVERVIEW_LOOK: Duration = super::tick_pace::REST;

/// Windows of another Space on screen count only when seen at two checks this far apart.
pub const FOREIGN_SETTLE: Duration = Duration::from_secs(1);

/// How long a wallpaper click that macOS answers with Show Desktop counts as the overview
/// before it is listed (seen ~0.7 s after the click); once listed, the overview itself holds.
pub const REVEAL_EXPECTED: Duration = Duration::from_millis(1500);

/// The Dock's window level (`kCGDockWindowLevel`): Mission Control's and App Exposé's backdrop.
pub const DOCK_LAYER: i64 = 20;
/// WindowManager's layer while Mission Control or App Exposé shows.
const EXPOSE_LAYER: i64 = 19;
/// WindowManager's layer while Show Desktop reveals the desktop.
pub const SHOW_DESKTOP_LAYER: i64 = 18;

/// What macOS shows over the Desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overview {
    /// Mission Control or App Exposé.
    MissionControl,
    /// Show Desktop (every window slid aside).
    ShowDesktop,
}

impl Overview {
    /// Its name for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::MissionControl => "Mission Control or App Exposé",
            Self::ShowDesktop => "Show Desktop",
        }
    }
}

/// One window of the window server's list, as far as telling an overview needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sighting<'a> {
    /// The owning process's name.
    pub owner: &'a str,
    /// Its window layer (0 for app windows).
    pub layer: i64,
    /// Its bounds, top-left origin.
    pub bounds: Rect,
    /// Listed on screen.
    pub onscreen: bool,
}

/// What one look at the window list shows over `display` ([`overview_in`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sight {
    /// An overview `WindowManager`'s own window says shows.
    pub overview: Option<Overview>,
    /// The Dock's full-display window is listed: Mission Control or App Exposé on its way out
    /// when one showed, else just the Dock brought up (auto-hide), which is no overview.
    pub dock_cover: bool,
}

impl From<Option<Overview>> for Sight {
    fn from(overview: Option<Overview>) -> Self {
        Self { overview, dock_cover: false }
    }
}

/// What the window list shows on `display` (the main display's frame).
pub fn overview_in<'a>(list: impl IntoIterator<Item = Sighting<'a>>, display: Rect) -> Sight {
    let mut sight = Sight::default();
    for w in list {
        if !w.onscreen || !covers_display(w.bounds, display) {
            continue;
        }
        match (w.owner, w.layer) {
            ("WindowManager", EXPOSE_LAYER) => sight.overview = Some(Overview::MissionControl),
            ("WindowManager", SHOW_DESKTOP_LAYER) if sight.overview.is_none() => sight.overview = Some(Overview::ShowDesktop),
            ("Dock", DOCK_LAYER) => sight.dock_cover = true,
            _ => {}
        }
    }
    sight
}

/// What changed at a look ([`OverviewWatch::see`]), for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Nothing new.
    None,
    /// An overview shows now (or another one replaced it).
    Began(Overview),
    /// The overview closed.
    Ended(Overview),
    /// Listed for [`OVERVIEW_STUCK`] without a break: no longer trusted.
    Stuck(Overview),
}

/// Follows overviews across looks at the window list and says when Space inference waits.
#[derive(Debug, Clone, Copy, Default)]
pub struct OverviewWatch {
    /// The overview listed at the last look and since when.
    showing: Option<(Overview, Instant)>,
    /// When the shown overview's own window (WindowManager's) was last listed.
    told_at: Option<Instant>,
    /// When the last one was seen gone.
    ended: Option<Instant>,
    /// The current one was listed too long to be trusted.
    stuck: bool,
    /// When Show Desktop was last expected from a wallpaper click ([`Self::expect_show_desktop`]).
    expected: Option<Instant>,
    /// When the window list was last looked at.
    looked: Option<Instant>,
    /// The look before the one the shown overview began at was this long before it: it began
    /// within that much ([`Self::found_within`]).
    found_within: Option<Duration>,
    /// When the window server's notice (1508) last came while an overview showed
    /// ([`Self::told`]): the close's, or the open's own when the poll found it first.
    told: Option<Instant>,
    /// The last such notice, kept past the close (`ctl space`).
    last_told: Option<Instant>,
    /// The look being made was brought by a notice ([`Self::look_at_notice`]).
    at_notice: bool,
    /// The last close was found by a look after a notice, with macOS's own window and the
    /// Dock's cover both gone: no settle follows ([`Self::settled_by_notice`]).
    quick: bool,
}

impl OverviewWatch {
    /// Takes what a look at the window list at `now` shows. The Dock's cover alone keeps a
    /// Mission Control (or App Exposé) that showed going until it goes too (their exit); it
    /// never begins one (an auto-hiding Dock brought up lists the same window).
    pub fn see(&mut self, sight: impl Into<Sight>, now: Instant) -> Change {
        let sight = sight.into();
        let gap = self.looked.map(|t| now.saturating_duration_since(t));
        self.looked = Some(now);
        if sight.overview.is_some() {
            self.told_at = Some(now);
        }
        let carried = self.told_at.is_some_and(|t| now.duration_since(t) < OVERVIEW_EXIT_CARRY);
        let carrying = sight.overview.is_none() && matches!(self.showing, Some((Overview::MissionControl, _)));
        if sight.overview.is_none()
            && sight.dock_cover
            && !carried
            && let (Some((Overview::MissionControl, _)), Some(t)) = (self.showing, self.told_at)
        {
            // The Dock's cover still listed, Mission Control's own windows gone longer than the
            // carry: it is taken as closed now, by the carry's cap.
            crate::engine::margin::wait("OVERVIEW_EXIT_CARRY", now.duration_since(t), OVERVIEW_EXIT_CARRY, crate::engine::margin::End::Cap);
        }
        let seen = match (sight.overview, self.showing) {
            (None, Some((Overview::MissionControl, _))) if sight.dock_cover && carried => Some(Overview::MissionControl),
            (seen, _) => seen,
        };
        match (seen, self.showing) {
            (Some(kind), Some((was, since))) if kind == was => {
                if !self.stuck && now.duration_since(since) >= OVERVIEW_STUCK {
                    self.stuck = true;
                    return Change::Stuck(kind);
                }
                Change::None
            }
            (Some(kind), _) => {
                self.showing = Some((kind, now));
                self.found_within = gap;
                self.stuck = false;
                self.ended = None;
                self.told = None;
                self.quick = false;
                // How the wait for a look ended: at a notice's look, or at the poll's.
                let within = gap.unwrap_or_default();
                if self.at_notice {
                    crate::engine::margin::wait_noticed("OVERVIEW_LOOK", within, OVERVIEW_LOOK);
                } else {
                    crate::engine::margin::wait("OVERVIEW_LOOK", within, OVERVIEW_LOOK, crate::engine::margin::End::Met);
                }
                if kind == Overview::ShowDesktop {
                    // The expectation is met: the overview holds on its own from here, and its
                    // close is not held back by a click's timer.
                    self.expected = None;
                }
                Change::Began(kind)
            }
            (None, Some((was, _))) => {
                self.showing = None;
                self.stuck = false;
                self.ended = Some(now);
                // macOS's own window and the Dock's cover both gone at a look after the
                // close's notice: the windows are in place, no settle follows.
                self.quick = self.told.take().is_some() && !sight.dock_cover;
                if self.quick {
                    crate::engine::margin::wait_noticed("OVERVIEW_SETTLE", Duration::ZERO, OVERVIEW_SETTLE);
                } else {
                    crate::engine::margin::wait("OVERVIEW_SETTLE", OVERVIEW_SETTLE, OVERVIEW_SETTLE, crate::engine::margin::End::Cap);
                }
                if carrying && !sight.dock_cover && let Some(t) = self.told_at {
                    // The Dock's cover went within the carry: met, at a look the notice
                    // brought forward when one came.
                    let gone = now.saturating_duration_since(t);
                    if self.quick {
                        crate::engine::margin::wait_noticed("OVERVIEW_EXIT_CARRY", gone, OVERVIEW_EXIT_CARRY);
                    } else {
                        crate::engine::margin::wait("OVERVIEW_EXIT_CARRY", gone, OVERVIEW_EXIT_CARRY, crate::engine::margin::End::Met);
                    }
                }
                Change::Ended(was)
            }
            (None, None) => Change::None,
        }
    }

    /// How long before the look that found the overview shown the previous look was: it
    /// began on screen at most this much before it was found (`None`: the first look). A
    /// gesture, a hot corner and the Dock's item are found at the tick (2 s at rest).
    pub fn found_within(&self) -> Option<Duration> {
        self.found_within.filter(|_| self.showing.is_some())
    }

    /// The overview shown at the last look (not one listed too long to be trusted).
    pub fn showing(&self) -> Option<Overview> {
        self.showing.filter(|_| !self.stuck).map(|(k, _)| k)
    }

    /// How long ago the last overview closed, while none shows (`None`: one shows, or none
    /// closed since the launch).
    pub fn since_closed(&self, now: Instant) -> Option<Duration> {
        self.ended.filter(|_| self.showing().is_none()).map(|t| now.saturating_duration_since(t))
    }

    /// True while Space inference waits: an overview shows, or closed less than
    /// [`OVERVIEW_SETTLE`] ago (none after a close found at a look after its notice:
    /// [`Self::settled_by_notice`]).
    pub fn freezes(&self, now: Instant) -> bool {
        self.showing().is_some() || self.ended.is_some_and(|t| !self.quick && now.duration_since(t) < OVERVIEW_SETTLE)
    }

    /// True while a notified Space change waits: an overview shows, or closed less than
    /// [`OVERVIEW_ARRIVAL_SETTLE`] ago (none after a close found at a look after its notice).
    pub fn holds_arrival(&self, now: Instant) -> bool {
        self.showing().is_some() || self.ended.is_some_and(|t| !self.quick && now.duration_since(t) < OVERVIEW_ARRIVAL_SETTLE)
    }

    /// The window server's notice (1508) stamped `at`: noted when an overview shows (the
    /// close's, or the open's own heard after the poll found it). The caller looks next.
    pub fn told(&mut self, at: Instant) {
        if self.showing.is_some_and(|(_, since)| at >= since) {
            self.told = Some(at);
            self.last_told = Some(at);
        }
    }

    /// The looks from now until `false` are brought by a notice or another trigger (its key,
    /// the Dock's notice), not by the poll: for the margin's count.
    pub fn look_at_notice(&mut self, on: bool) {
        self.at_notice = on;
    }

    /// True while an overview shows and a notice came during it less than
    /// [`OVERVIEW_EXIT_CARRY`] ago: its close is looked for at every fast tick.
    pub fn closing_told(&self, now: Instant) -> bool {
        self.showing.is_some() && self.told.is_some_and(|t| now.saturating_duration_since(t) < OVERVIEW_EXIT_CARRY)
    }

    /// True when the last close was found at a look after a notice, with macOS's own window
    /// and the Dock's cover both gone: no settle followed it.
    pub fn settled_by_notice(&self) -> bool {
        self.ended.is_some() && self.quick
    }

    /// For `ctl space`: ms since the overview shown was found, since the last one closed, and
    /// since a notice came while one showed.
    pub fn ages_ms(&self, now: Instant) -> (Option<u64>, Option<u64>, Option<u64>) {
        let ms = |t: Instant| now.saturating_duration_since(t).as_millis() as u64;
        (self.showing.map(|(_, t)| ms(t)), self.ended.map(ms), self.last_told.map(ms))
    }

    /// A wallpaper click at `now` that macOS answers with Show Desktop: expected for
    /// [`REVEAL_EXPECTED`] unless Show Desktop shows already (the click closes it). True when
    /// it is.
    pub fn expect_show_desktop(&mut self, now: Instant) -> bool {
        if self.showing() == Some(Overview::ShowDesktop) {
            return false;
        }
        self.expected = Some(now);
        true
    }

    /// True while an overview shows, or Show Desktop is expected from a wallpaper click
    /// ([`Self::expect_show_desktop`]): the corner patch and the still stay away.
    pub fn showing_or_expected(&self, now: Instant) -> bool {
        self.showing().is_some() || self.expected.is_some_and(|t| now.duration_since(t) < REVEAL_EXPECTED)
    }

    /// True while Show Desktop is listed, or expected from a wallpaper click with no overview
    /// listed yet (round show-desktop-patch: the corner patch's plan and the fast tick,
    /// [`super::show_desktop`]). Not during Mission Control or App Exposé.
    pub fn show_desktop_up(&self, now: Instant) -> bool {
        match self.showing() {
            Some(Overview::ShowDesktop) => true,
            Some(Overview::MissionControl) => false,
            None => self.expected.is_some_and(|t| now.duration_since(t) < REVEAL_EXPECTED),
        }
    }

    /// True while macOS may have the windows somewhere other than their frames: Show Desktop
    /// pushes them aside (listed, also one listed too long to be trusted: the windows may
    /// still be aside; or expected from a wallpaper click), Mission Control and App Exposé
    /// list every window of the Desktop shrunk into their layout (shown, not one listed too
    /// long to be trusted), and for [`OVERVIEW_SETTLE`] after either closes the windows are
    /// on their way back. A frame read then is the overview's, never the window's own (11-09:
    /// a window pushed off the right edge was read at Show Desktop's close and kept there as
    /// its frame; 11-18: an activation from a pick in Mission Control read the front stage's
    /// windows at their places in its layout, a 1949x1288 window at 718x474, and the next
    /// switch to their stage brought them back at that size).
    pub fn moves_windows(&self, now: Instant) -> bool {
        self.showing.is_some_and(|(k, _)| k == Overview::ShowDesktop)
            || self.showing() == Some(Overview::MissionControl)
            || self.expected.is_some_and(|t| now.duration_since(t) < REVEAL_EXPECTED)
            || self.ended.is_some_and(|t| now.duration_since(t) < OVERVIEW_SETTLE)
    }

    /// True once, when an expectation's time is up at `now` with no Show Desktop listed since
    /// the click (the patch is planned again: it comes back if macOS showed none).
    pub fn expectation_over(&mut self, now: Instant) -> bool {
        if self.expected.is_some_and(|t| now.duration_since(t) >= REVEAL_EXPECTED) {
            self.expected = None;
            return true;
        }
        false
    }
}

/// Evidence (a set of window ids) that counts only once it held at two looks at least a
/// settle apart.
#[derive(Debug, Clone, Default)]
pub struct Persisting {
    /// The evidence first seen and when.
    pending: Option<(Vec<u32>, Instant)>,
}

impl Persisting {
    /// Takes the evidence seen at `now` (sorted ids; empty for none). True when the same
    /// evidence was first seen at least `settle` ago and held at every look since.
    pub fn observe(&mut self, seen: Vec<u32>, now: Instant, settle: Duration) -> bool {
        if seen.is_empty() {
            self.pending = None;
            return false;
        }
        match &self.pending {
            Some((was, since)) if *was == seen => now.duration_since(*since) >= settle,
            _ => {
                self.pending = Some((seen, now));
                false
            }
        }
    }

    /// Forgets the evidence (a Space change, a freeze: what comes next is looked at afresh).
    pub fn clear(&mut self) {
        self.pending = None;
    }

    /// True while evidence waits for its second look.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests;
