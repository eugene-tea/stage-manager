//! A window whose Accessibility handle died (round stale-handles), decided without touching
//! anything.
//!
//! The owner's 11-04: Music launched, answered `CannotComplete` four times, and at its first
//! answer we kept its window's element (07:43:09). Its first move ever, a park 108 s later,
//! failed with `kAXErrorInvalidUIElement` (-25202), and so did every move after it (2109 lines
//! that day): the window never parked and stayed over the next stage, and once he closed it
//! (Music keeps a closed main window, ordered out, to reuse) its stage kept counting it, its
//! card showing the old picture and every switch there flying in a ghost. No destroyed
//! notification came. The element went stale long before any move: at Music's first
//! activation (07:43:45) its window left the screen and came back, a moment at another size;
//! from 07:43:47 every read through our element answered -25202, and at 07:43:50 Music's focus
//! named an element matching none we tracked while the window (the same window-server id)
//! still was. So Music ordered the window out and in and handed out a new element for it;
//! why no destroyed notification came for the old one is not known.
//!
//! A handle is **dead** when a call through it answers `kAXErrorInvalidUIElement`. The
//! window is then looked up afresh by its id in its app's `AXWindows` ([`Lookup`]) and judged
//! ([`judge`]):
//!
//! - **listed afresh**: its handle is replaced and the move tried once more with it
//!   ([`Judgement::Refresh`]). A minimized window is listed (`AXWindows` names an app's
//!   minimized windows), so it is always kept this way;
//! - **its app did not answer**: kept, looked at again later;
//! - **not listed, and on screen**: kept (never drop a visible window), looked at again;
//! - **not listed, off screen or not listed by the window server either**: closed, unless it
//!   may be out of reach for a reason: on a Desktop not shown (`AXWindows` lists only the
//!   current Space's windows for most apps), its app hidden, or a hidden tab (8J: off screen
//!   beside a same-size window of its app, [`super::hidden_tab::may_be_hidden_tab`]); those
//!   are kept ([`Keep`]).
//!
//! A window judged closed is let go as a destroyed notification would have: out of its
//! stage, its card and the saved state, logged once. Never on one look ([`Sightings`]): a
//! second look at least [`LOOK_APART`] later must judge it closed too (a Desktop slide's list
//! before its notice shows the Desktop's windows off screen). A closed window that comes back
//! (Music's Window ▸ Music orders the same window in) is a new window then.
//!
//! A window's app is looked up at most once per [`LOOK_AGAIN`] ([`Looks`]): a window that
//! refuses every move costs about one lookup a second, not one per move.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Two looks judging a dead handle's window closed at least this far apart let it go.
pub const LOOK_APART: Duration = Duration::from_millis(700);

/// A window's app is asked for its window list at most this often (under the Space check's
/// second, so each of its looks may ask).
pub const LOOK_AGAIN: Duration = Duration::from_millis(800);

/// What the window's app said when asked for its windows (`AXWindows`), by window id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// The app lists a window with this id: a fresh handle.
    Listed,
    /// The app answered, and lists no window with this id.
    NotListed,
    /// The app did not answer (busy, launching), or a window's id could not be read.
    NoAnswer,
}

/// What the window server says of the window now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Server {
    /// Listed on screen.
    OnScreen,
    /// Listed off screen (ordered out: closed and kept by its app, minimized, hidden, on a
    /// Desktop not shown, a hidden tab).
    OffScreen,
    /// Not listed at all.
    Unlisted,
}

/// What is known of a window whose handle died, at one look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// Its app's answer.
    pub lookup: Lookup,
    /// The window server's.
    pub server: Server,
    /// The model has it on a Desktop not shown.
    pub other_space: bool,
    /// Its app is hidden (by the user or by us), or whether it is cannot be told.
    pub app_hidden: bool,
    /// It may be a hidden tab: off screen beside a same-size window of its app (8J).
    pub maybe_tab: bool,
}

/// Why a window whose handle died is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// Its app did not answer.
    NoAnswer,
    /// On screen: never dropped.
    OnScreen,
    /// On a Desktop not shown.
    OtherSpace,
    /// Its app is hidden.
    AppHidden,
    /// Perhaps a hidden tab.
    MaybeTab,
}

impl Keep {
    /// The reason in words, for the log.
    pub fn words(self) -> &'static str {
        match self {
            Self::NoAnswer => "its app did not answer",
            Self::OnScreen => "it is on screen",
            Self::OtherSpace => "its Desktop is not shown",
            Self::AppHidden => "its app is hidden",
            Self::MaybeTab => "it may be a hidden tab",
        }
    }
}

/// What to do with a window whose handle died.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Judgement {
    /// Its app lists it afresh: take the new handle and try again.
    Refresh,
    /// Leave it as it is; look again later.
    Keep(Keep),
    /// Closed: let it go (once a second look agrees, [`Sightings`]).
    Gone,
}

/// The judgement for a window whose handle died (module docs).
pub fn judge(f: Facts) -> Judgement {
    match f.lookup {
        Lookup::Listed => return Judgement::Refresh,
        Lookup::NoAnswer => return Judgement::Keep(Keep::NoAnswer),
        Lookup::NotListed => {}
    }
    if f.server == Server::OnScreen {
        Judgement::Keep(Keep::OnScreen)
    } else if f.other_space {
        Judgement::Keep(Keep::OtherSpace)
    } else if f.app_hidden {
        Judgement::Keep(Keep::AppHidden)
    } else if f.maybe_tab {
        Judgement::Keep(Keep::MaybeTab)
    } else {
        Judgement::Gone
    }
}

/// When each window's app was last asked for its windows ([`LOOK_AGAIN`]).
#[derive(Debug, Clone, Default)]
pub struct Looks {
    last: HashMap<u32, Instant>,
}

impl Looks {
    /// Whether window `id`'s app may be asked now; when it may, the look is counted.
    pub fn may_look(&mut self, id: u32, now: Instant) -> bool {
        match self.last.get(&id) {
            Some(&t) if now.saturating_duration_since(t) < LOOK_AGAIN => false,
            _ => {
                self.last.insert(id, now);
                true
            }
        }
    }

    /// Forgets windows `keep` says no to (let go).
    pub fn retain(&mut self, keep: impl Fn(u32) -> bool) {
        self.last.retain(|id, _| keep(*id));
    }

    /// Forgets looks older than [`LOOK_AGAIN`]: they hold nothing back any more.
    pub fn prune(&mut self, now: Instant) {
        self.last.retain(|_, t| now.saturating_duration_since(*t) < LOOK_AGAIN);
    }
}

/// Windows judged closed at one look, with when: a second look [`LOOK_APART`] later lets
/// them go.
#[derive(Debug, Clone, Default)]
pub struct Sightings {
    first: HashMap<u32, Instant>,
}

impl Sightings {
    /// Window `id` judged closed at `now`: true when an earlier look at least
    /// [`LOOK_APART`] ago judged so too (it is then forgotten here: the caller lets it go).
    pub fn closed(&mut self, id: u32, now: Instant) -> bool {
        match self.first.get(&id) {
            Some(&t) if now.saturating_duration_since(t) >= LOOK_APART => {
                self.first.remove(&id);
                true
            }
            Some(_) => false,
            None => {
                self.first.insert(id, now);
                false
            }
        }
    }

    /// Window `id` judged anything but closed: its streak starts over.
    pub fn clear(&mut self, id: u32) {
        self.first.remove(&id);
    }

    /// Keeps the streaks of windows `keep` says yes to (still candidates).
    pub fn retain(&mut self, keep: impl Fn(u32) -> bool) {
        self.first.retain(|id, _| keep(*id));
    }

    /// Windows judged closed once, waiting for a second look (for `ctl state`).
    pub fn waiting(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.first.keys().copied().collect();
        v.sort_unstable();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Music's window on 11-04 after he closed it: its app lists no window, the window server
    /// lists it off screen, on the Desktop shown, Music not hidden, no window of its size.
    fn closed() -> Facts {
        Facts { lookup: Lookup::NotListed, server: Server::OffScreen, other_space: false, app_hidden: false, maybe_tab: false }
    }

    #[test]
    fn a_dead_handle_whose_window_is_unlisted_and_off_screen_is_gone() {
        assert_eq!(judge(closed()), Judgement::Gone);
        assert_eq!(judge(Facts { server: Server::Unlisted, ..closed() }), Judgement::Gone, "not listed anywhere: closed");
    }

    #[test]
    fn a_dead_handle_whose_window_is_on_screen_is_kept() {
        assert_eq!(judge(Facts { server: Server::OnScreen, ..closed() }), Judgement::Keep(Keep::OnScreen));
    }

    #[test]
    fn a_dead_handle_whose_window_is_listed_afresh_is_refreshed() {
        for server in [Server::OnScreen, Server::OffScreen, Server::Unlisted] {
            assert_eq!(judge(Facts { lookup: Lookup::Listed, server, ..closed() }), Judgement::Refresh);
        }
        // A minimized window: off screen, and listed by its app.
        assert_eq!(judge(Facts { lookup: Lookup::Listed, ..closed() }), Judgement::Refresh, "minimized: refreshed, never let go");
    }

    #[test]
    fn a_window_out_of_reach_for_a_reason_is_kept() {
        assert_eq!(judge(Facts { other_space: true, ..closed() }), Judgement::Keep(Keep::OtherSpace));
        assert_eq!(judge(Facts { app_hidden: true, ..closed() }), Judgement::Keep(Keep::AppHidden));
        assert_eq!(judge(Facts { maybe_tab: true, ..closed() }), Judgement::Keep(Keep::MaybeTab));
        assert_eq!(judge(Facts { lookup: Lookup::NoAnswer, ..closed() }), Judgement::Keep(Keep::NoAnswer));
    }

    #[test]
    fn a_window_is_let_go_only_on_a_second_look_apart() {
        let t0 = Instant::now();
        let mut s = Sightings::default();
        assert!(!s.closed(9460, t0), "one look never closes");
        assert!(!s.closed(9460, t0 + Duration::from_millis(300)), "too soon");
        assert_eq!(s.waiting(), vec![9460]);
        assert!(s.closed(9460, t0 + LOOK_APART), "a second look apart");
        assert!(s.waiting().is_empty(), "forgotten once closed");
        assert!(!s.closed(9460, t0 + LOOK_APART * 2), "a new streak");
        s.clear(9460);
        assert!(!s.closed(9460, t0 + LOOK_APART * 4), "a look in between starts it over");
        s.retain(|_| false);
        assert!(s.waiting().is_empty());
    }

    #[test]
    fn an_app_is_asked_at_most_once_per_look_again_per_window() {
        let t0 = Instant::now();
        let mut l = Looks::default();
        assert!(l.may_look(9460, t0));
        assert!(!l.may_look(9460, t0 + Duration::from_millis(500)), "each failed move would otherwise ask");
        assert!(l.may_look(1779, t0), "another window has its own");
        assert!(l.may_look(9460, t0 + LOOK_AGAIN));
        l.retain(|_| false);
        assert!(l.may_look(9460, t0 + LOOK_AGAIN), "forgotten: asked at once");
        l.prune(t0 + LOOK_AGAIN * 3);
        assert!(l.last.is_empty(), "old looks pruned");
    }
}
