//! The slide watch on the WM thread ([`crate::engine::slide_watch`]): what a Dock press or
//! ⌘Tab's switcher put home is noted, each Dock click and ⌘Tab release under the Spaces engine
//! is watched for a Desktop slide nobody asked for, and its outcome is written as one
//! `slide-watch: outcome` line; a `slide-watch: summary` line follows each hour with activity,
//! and at the quit. Only lines and counters: nothing here changes what the app does, and
//! nothing is polled (the Desktop changes are the ones the controller recognizes anyway, the
//! outcome is written at the next tick, input or Desktop change once due).
//!
//! Under the Corner engine nothing is watched or written (macOS counts a parked window on its
//! Desktop: the slide this watches for does not arise).

use crate::platform::clock;
use std::time::Instant;

use serde_json::Value;

use super::Controller;
use crate::engine::model::{Model, SpaceId};
use crate::engine::slide_watch::{Person, Start, Tally, Via, Watch};

/// The watch's state: the facts of a Dock press waiting for its release, ⌘Tab's last lend,
/// the activation watched, and the counts.
#[derive(Debug, Default)]
pub(super) struct SlideWatch {
    /// A Dock press's facts, until its release (a click starts the watch).
    pressed: Option<Start>,
    /// ⌘Tab's last lend for the switcher: the app, whether its park took, what it took,
    /// and why it did not.
    lend: Option<(i32, Option<bool>, u64, Option<&'static str>)>,
    /// The activation watched.
    watch: Option<Watch>,
    /// The counts since the launch.
    tally: Tally,
}

/// What a lend did, as the watch notes it: `home` (lent), `parked` (the park asked took;
/// `None`: none asked), `asked` (a park was asked), the lend's park time.
#[derive(Debug, Clone, Copy)]
pub(super) struct LendNote {
    pub(super) home: bool,
    pub(super) parked: Option<bool>,
    pub(super) asked: bool,
    pub(super) park_ms: u64,
}

impl LendNote {
    /// `parked` as the line says it: `n/a` when nothing was put home and no park asked.
    fn parked(self) -> Option<bool> {
        if self.home || self.asked { Some(self.parked.unwrap_or(false)) } else { None }
    }

    /// Why the park was skipped or did not take, or the lend was not made.
    fn why(self) -> Option<&'static str> {
        match (self.home, self.asked, self.parked) {
            (true, false, _) => Some("no park asked: the window is not known to the observer"),
            (true, true, Some(false)) => Some("the park did not take in time: put home at its frame"),
            (false, true, Some(false)) => Some("the park did not take: nothing put home"),
            (false, _, _) => Some("the window could not be put home"),
            _ => None,
        }
    }
}

/// True when app `pid` has a window on Desktop `space` in the model.
fn app_on(m: &Model, pid: i32, space: SpaceId) -> bool {
    m.windows().any(|w| w.meta.pid == pid && w.space == space)
}

impl Controller {
    /// True under the Spaces engine with a model (the only engine watched).
    fn slide_watched(&self) -> bool {
        self.model.as_ref().is_some_and(|m| !m.macos_counts_hidden())
    }

    /// A Dock press handled: its facts kept for the release. `lend`: what the press put home
    /// (`None`: nothing was to be put home), `predicted`: the Desktop predicted for the click.
    pub(super) fn slide_watch_press(&mut self, bundle: &str, pid: i32, lend: Option<LendNote>, predicted: Option<u64>) {
        if !self.slide_watched() {
            return;
        }
        let Some(m) = self.model.as_ref() else { return };
        let before = m.current_space(m.main_display().id);
        self.slide_watch.pressed = Some(Start {
            via: Via::Dock,
            bundle: Some(bundle.to_string()),
            pid,
            put_home: lend.is_some_and(|l| l.home),
            parked: lend.and_then(LendNote::parked),
            park_ms: lend.filter(|l| l.asked).map(|l| l.park_ms),
            why: lend.and_then(LendNote::why),
            press_to_release_ms: None,
            desktop_before: before.0,
            predicted,
            app_here: app_on(m, pid, before),
        });
    }

    /// A Dock press begins: the facts of one before it go.
    pub(super) fn slide_watch_pressing(&mut self) {
        self.slide_watch.pressed = None;
    }

    /// A Dock release: a click (`click`) starts the watch of what its press noted,
    /// `press_to_release_ms` after the press; anything else drops it.
    pub(super) fn slide_watch_released(&mut self, click: bool, press_to_release_ms: u64, now: Instant) {
        let Some(mut start) = self.slide_watch.pressed.take() else { return };
        if click {
            start.press_to_release_ms = Some(press_to_release_ms);
            self.slide_watch_begin(start, now);
        }
    }

    /// ⌘Tab's switcher put (or did not put) a window of app `pid` home.
    pub(super) fn slide_watch_lend(&mut self, pid: i32, lend: LendNote) {
        if self.slide_watched() {
            self.slide_watch.lend = Some((pid, lend.parked(), lend.park_ms, lend.why()));
        }
    }

    /// ⌘ let go with the switcher up on app `pid` (`lent`: a window of it is home for the
    /// switcher now; `since_tab_ms`: from the first Tab; `activated`: an app already activated
    /// since the first Tab, as a click in the switcher does before ⌘'s release): the watch
    /// starts.
    pub(super) fn slide_watch_cmd_tab(
        &mut self,
        ob: &crate::platform::observe::Observer,
        pid: i32,
        lent: bool,
        since_tab_ms: Option<u64>,
        activated: Option<i32>,
        now: Instant,
    ) {
        let noted = self.slide_watch.lend.take().filter(|(p, ..)| *p == pid);
        if !self.slide_watched() {
            return;
        }
        let Some(m) = self.model.as_ref() else { return };
        let before = m.current_space(m.main_display().id);
        let start = Start {
            via: Via::CmdTab,
            bundle: ob.bundle_id(pid).map(str::to_string),
            pid,
            put_home: lent,
            parked: noted.and_then(|(_, parked, ..)| parked).filter(|_| lent),
            park_ms: noted.map(|(_, _, ms, _)| ms).filter(|_| lent),
            why: noted.and_then(|(.., why)| why),
            press_to_release_ms: since_tab_ms,
            desktop_before: before.0,
            predicted: None,
            app_here: app_on(m, pid, before),
        };
        self.slide_watch_begin(start, now);
        if let Some(activated) = activated {
            self.slide_watch_activated(ob, activated);
        }
    }

    /// A new activation watched; one still watched is written first with what it knows.
    fn slide_watch_begin(&mut self, start: Start, now: Instant) {
        if let Some(old) = self.slide_watch.watch.take() {
            self.slide_watch_write(old, now);
        }
        self.slide_watch.watch = Some(Watch::new(start, now));
    }

    /// A Space change notice.
    pub(super) fn slide_watch_notice(&mut self, now: Instant) {
        if let Some(w) = self.slide_watch.watch.as_mut() {
            w.notice(now);
        }
    }

    /// The main screen's Desktop recognized, `from` → `to`. It is where the activation was
    /// meant to go when a Desktop was arranged ahead there, the press predicted it, or the
    /// app had no window on the Desktop left and has one there (macOS's own rule).
    pub(super) fn slide_watch_arrived(&mut self, from: SpaceId, to: SpaceId) {
        let now = clock::now();
        let ahead = self.ahead_space();
        let Some(w) = self.slide_watch.watch.as_ref() else { return };
        let s = w.start();
        let there = self.model.as_ref().is_some_and(|m| app_on(m, w.pid(), to));
        let expected = ahead == Some(to) || s.predicted == Some(to.0) || (!s.app_here && there);
        if let Some(w) = self.slide_watch.watch.as_mut() {
            w.arrived(now, from.0, to.0, expected);
        }
        self.slide_watch_step(now);
    }

    /// The person did `what` (a ⌃-arrow key, Mission Control showing).
    pub(super) fn slide_watch_person(&mut self, what: Person) {
        if let Some(w) = self.slide_watch.watch.as_mut() {
            w.person(clock::now(), what);
        }
    }

    /// App `pid` activated.
    pub(super) fn slide_watch_activated(&mut self, ob: &crate::platform::observe::Observer, pid: i32) {
        if let Some(w) = self.slide_watch.watch.as_mut() {
            w.activated(pid, ob.bundle_id(pid));
        }
    }

    /// A switch found incoming windows `shown` already on screen (the safety net): noted when
    /// one of them is the watched app's.
    pub(super) fn slide_watch_shown_early(&mut self, shown: &[u32]) {
        let Some(pid) = self.slide_watch.watch.as_ref().map(Watch::pid) else { return };
        let ours = self
            .model
            .as_ref()
            .is_some_and(|m| shown.iter().any(|id| m.window(crate::engine::model::WindowId(*id)).is_some_and(|w| w.meta.pid == pid)));
        if ours && let Some(w) = self.slide_watch.watch.as_mut() {
            w.shown_early();
        }
    }

    /// The tick (and input, and a Desktop recognized): a watch whose outcome is known is
    /// written; the hourly summary when due.
    pub(super) fn slide_watch_step(&mut self, now: Instant) {
        if self.slide_watch.watch.as_ref().is_some_and(|w| w.due(now))
            && let Some(w) = self.slide_watch.watch.take()
        {
            self.slide_watch_write(w, now);
        }
        if self.slide_watch.tally.summary_due(now) {
            let line = self.slide_watch.tally.summary_line("hour", now);
            tracing::info!("slide-watch: summary {line}");
        }
    }

    /// The quit (every window unparked): the watch still open is written, then a summary when
    /// there was activity since the last.
    pub(super) fn slide_watch_quit(&mut self) {
        let now = clock::now();
        if let Some(w) = self.slide_watch.watch.take() {
            self.slide_watch_write(w, now);
        }
        if self.slide_watch.tally.active() {
            let line = self.slide_watch.tally.summary_line("quit", now);
            tracing::info!("slide-watch: summary {line}");
        }
    }

    fn slide_watch_write(&mut self, w: Watch, now: Instant) {
        tracing::info!("slide-watch: outcome {}", w.line());
        self.slide_watch.tally.add(&w, now);
    }

    /// `ctl state`'s `slide_watch`: the counts since the launch, by `via`.
    pub(super) fn slide_watch_json(&self) -> Value {
        self.slide_watch.tally.json()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lends_park_says_n_a_only_when_nothing_was_put_home_or_asked() {
        let note = |home, parked, asked| LendNote { home, parked, asked, park_ms: 9 };
        assert_eq!(note(true, Some(true), true).parked(), Some(true));
        assert_eq!(note(true, Some(true), true).why(), None);
        assert_eq!(note(true, Some(false), true).parked(), Some(false));
        assert!(note(true, Some(false), true).why().is_some_and(|w| w.contains("did not take")));
        assert_eq!(note(true, None, false).parked(), Some(false), "put home with no park asked");
        assert_eq!(note(false, None, false).parked(), None);
        assert!(note(false, Some(false), true).why().is_some_and(|w| w.contains("nothing put home")));
    }
}
