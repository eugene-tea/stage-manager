//! A launch as the login session begins (round early-start: the launchd agent starts Stage
//! Manager before loginwindow reopens the apps of the last session). The Dock, Finder, the
//! menu bar and the second screen may not be there yet; apps open and their windows show one
//! by one after the launch.
//!
//! Most of the launch waits already: the Dock's order and bar are read again every 2 s and
//! after each app launch, Finder's desktop icons are retried (at most 4 s apart), the
//! displays are polled every second, and a Desktop seen empty after a restart is provisional
//! until windows that match a saved one show. Windows that show after the launch go to their
//! saved stages through the restore window, whose clocks count from the login as well as
//! from the launch ([`crate::engine::restore::Settling`]).
//!
//! What this module adds is the screens: the stages are built from the screens known at the
//! launch, and a saved Desktop whose screen is not connected forms on the one that is (the
//! 10-23 login on the laptop alone). So an early launch waits for the screens a moment
//! ([`screens_wait`]): until the platform has read the displays at all (at most
//! [`FIRST_READING_WAIT`]), and, within [`EARLY`] of the login, until every screen
//! `state.json` saved stages on is connected (at most [`SCREENS_WAIT`] after the launch: a
//! lid closed at login never connects its screen).

use std::time::Duration;

use super::displays::ScreenKey;
use super::persist::StateFile;

/// A launch this soon after the login is the session's start: screens may still come.
pub const EARLY: Duration = Duration::from_secs(60);

/// How long an early launch waits for a screen `state.json` saved stages on.
pub const SCREENS_WAIT: Duration = Duration::from_secs(10);

/// How long any launch waits for the platform's first reading of the displays.
pub const FIRST_READING_WAIT: Duration = Duration::from_secs(5);

/// Why the launch's stages wait for the screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreensWait {
    /// No reading of the displays yet.
    NoReading,
    /// This many screens with saved stages are not connected yet.
    Missing(usize),
}

impl ScreensWait {
    /// Its words for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::NoReading => "the displays are not read yet",
            Self::Missing(_) => "a screen the stages were saved on is not connected yet",
        }
    }
}

/// Whether the launch's stages wait for the screens (module docs): `read`, the displays were
/// read; `missing`, screens with saved stages not connected ([`missing_screens`]);
/// `since_login`, the login's age at the launch; `waited`, since the launch first tried to
/// build; `main_only`, `[displays] main_only` (other screens are not managed).
pub fn screens_wait(read: bool, missing: usize, since_login: Option<Duration>, waited: Duration, main_only: bool) -> Option<ScreensWait> {
    if !read {
        return (waited < FIRST_READING_WAIT).then_some(ScreensWait::NoReading);
    }
    let early = since_login.is_some_and(|d| d <= EARLY);
    (missing > 0 && early && !main_only && waited < SCREENS_WAIT).then_some(ScreensWait::Missing(missing))
}

/// How many screens `state` saved stages on (a Desktop with at least one stage, its screen
/// known) are not among `connected`.
pub fn missing_screens(state: &StateFile, connected: &[ScreenKey]) -> usize {
    let mut saved: Vec<ScreenKey> = state
        .spaces
        .iter()
        .filter(|s| !s.stages.is_empty())
        .filter_map(|s| s.screen.as_ref().and_then(|k| ScreenKey::parse(&k.key)))
        .filter(ScreenKey::known)
        .collect();
    saved.sort();
    saved.dedup();
    saved.iter().filter(|k| !connected.contains(k)).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::persist::{SavedScreen, SavedSpace, SavedStage};

    const LAPTOP: ScreenKey = ScreenKey::new(0x610, 0xa05c, 0xfd62);
    const STUDIO: ScreenKey = ScreenKey::new(0x610, 0xae3a, 0x1e22);

    fn space(id: u64, key: ScreenKey, stages: usize) -> SavedSpace {
        SavedSpace {
            id,
            screen: Some(SavedScreen { key: key.text(), w: 1800.0, h: 1169.0, ..SavedScreen::default() }),
            stages: vec![SavedStage::default(); stages],
            ..SavedSpace::default()
        }
    }

    fn state(spaces: Vec<SavedSpace>) -> StateFile {
        StateFile { spaces, ..StateFile::single(SavedSpace::default()) }
    }

    #[test]
    fn the_screens_with_saved_stages_that_are_not_connected() {
        let s = state(vec![space(1, STUDIO, 2), space(2, STUDIO, 1), space(3, LAPTOP, 1)]);
        assert_eq!(missing_screens(&s, &[LAPTOP]), 1, "the 5K, once for its two Desktops");
        assert_eq!(missing_screens(&s, &[LAPTOP, STUDIO]), 0);
        assert_eq!(missing_screens(&s, &[]), 2);
        let empty = state(vec![space(1, STUDIO, 0), space(3, LAPTOP, 1)]);
        assert_eq!(missing_screens(&empty, &[LAPTOP]), 0, "a Desktop with no stages waits for nothing");
        let unnamed = state(vec![SavedSpace { id: 1, stages: vec![SavedStage::default()], ..SavedSpace::default() }]);
        assert_eq!(missing_screens(&unnamed, &[LAPTOP]), 0, "a file from before round displays names no screen");
    }

    #[test]
    fn an_early_launch_waits_for_a_saved_screen_a_moment() {
        let s = Duration::from_secs;
        let login = Some(s(2));
        assert_eq!(screens_wait(true, 1, login, s(0), false), Some(ScreensWait::Missing(1)), "the 5K not listed yet");
        assert_eq!(screens_wait(true, 1, login, s(9), false), Some(ScreensWait::Missing(1)));
        assert_eq!(screens_wait(true, 1, login, SCREENS_WAIT, false), None, "a lid closed at login: built on the screen there is");
        assert_eq!(screens_wait(true, 0, login, s(0), false), None, "every saved screen is here");
        assert_eq!(screens_wait(true, 1, Some(s(600)), s(0), false), None, "a relaunch later in the session: the screen is gone, not coming");
        assert_eq!(screens_wait(true, 1, None, s(0), false), None, "no login time: no wait");
        assert_eq!(screens_wait(true, 1, login, s(0), true), None, "main_only manages one screen");
    }

    #[test]
    fn any_launch_waits_for_the_first_reading_of_the_displays_a_moment() {
        let s = Duration::from_secs;
        assert_eq!(screens_wait(false, 0, None, s(0), false), Some(ScreensWait::NoReading));
        assert_eq!(screens_wait(false, 0, Some(s(600)), s(4), true), Some(ScreensWait::NoReading));
        assert_eq!(screens_wait(false, 0, None, FIRST_READING_WAIT, false), None, "then the window server's main display stands in");
    }
}
