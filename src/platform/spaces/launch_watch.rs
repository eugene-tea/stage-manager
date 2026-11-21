//! A just-opened app's new windows held before they show (round sm-placenew): under the
//! Spaces engine a newly opened app's first window never shows over the stage being left.
//!
//! What was measured (11-14, `probe-spaces sources`, 16 launches of the fixture, TextEdit,
//! Calculator and Stickies): the window server lists an app's new window, off screen, on no
//! Space and 0x0, 26 to 650 ms before it first shows; it joins the Desktop only as it shows.
//! Accessibility answers the app's first question within about 15 ms of that moment (the
//! app's main thread is busy until it orders the window in), and the window server's notices
//! (`SLSRegisterConnectionNotifyProc`, `SLSRegisterNotifyProc`, every type to 2000) brought
//! this process nothing. So the window list is the one early source: from the app's launch
//! notice, this watch reads the ids of every window ([`windows::every_window_id`], a tenth
//! of a full list's cost) every [`EVERY`], on a thread of its own, and puts each new window
//! of the app that is still on no Space into the Space where new windows wait
//! ([`SpacesEngine::hold_new`]), never shown. The app orders it in there; Accessibility then
//! lists it, the controller finds it and places it at once ([`WindowEngine::about_to_show`]),
//! its stage's switch hides the stage left behind, and only then is the window let onto the
//! Desktop ([`WindowEngine::let_new_out`]), or into its stage's Space when it joins a stage
//! not shown.
//!
//! A window not placed within [`NOT_PLACED_AFTER`] of showing in its Space (its app does
//! not answer Accessibility yet, or it is not a window anyone places) is let out as it
//! would have shown; one its app never orders in (helper windows: every app measured makes
//! four or five) is let out when the watch ends. The watch ends [`WATCH_FOR`] after the
//! launch notice (the observer's own "just opened" time), once nothing waits. A window
//! created and ordered in within one read (Stickies' second note, once) shows unheld: counted
//! (`new_missed`), placed as before.
//!
//! The window server's notice of a window's creation (type 811, 3-11 ms after the cause in
//! `probe-spaces cgs`, 11-21: [`super::notices`]) would wake the watch for a look at once
//! ([`nudge`]), but the app has never heard it: in the live `launch` phase (11-21, 20
//! launches) no 811, 815 or 816 came and every one of the 100 windows held was found by the
//! read every [`EVERY`] (the probe also asked `SLSRequestNotificationsForWindows` for its
//! fixtures' windows; the app does not). So the watch rests on its read; each window held is
//! noted by which look found it (`margin` `launch_watch::EVERY`: `noticed`, else the read's).
//!
//! [`WindowEngine::about_to_show`]: crate::platform::window_engine::WindowEngine::about_to_show
//! [`WindowEngine::let_new_out`]: crate::platform::window_engine::WindowEngine::let_new_out

use crate::platform::clock;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::engine::SpacesEngine;
use crate::engine::observe_retry::LAUNCH_FOR;
use crate::platform::windows;

/// How often the window ids are read while an app has just launched: the shortest lead
/// measured from a new window's first listing to its first frame on screen was 23 ms (the
/// fixture), and a hold takes about 1.5 ms.
pub const EVERY: Duration = Duration::from_millis(4);

/// How long after its launch notice an app's new windows are watched for.
pub const WATCH_FOR: Duration = LAUNCH_FOR;

/// A held window shown in its Space (its app ordered it in) and not placed this long after
/// is let out as it would have shown.
pub const NOT_PLACED_AFTER: Duration = Duration::from_secs(1);

/// What the first look at one of the app's new windows decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstLook {
    /// Created and not ordered in yet: held.
    Hold,
    /// On screen already: too late to hold.
    Missed,
    /// Not a window to hold (another layer, or already on a Space: minimized, another
    /// Desktop's).
    Leave,
}

/// The first look at a new window of the app: its layer, whether it is on screen, whether
/// the window server lists it on any Space.
pub fn first_look(layer: i64, onscreen: bool, on_a_space: bool) -> FirstLook {
    match (layer, onscreen, on_a_space) {
        (0, true, _) => FirstLook::Missed,
        (0, false, false) => FirstLook::Hold,
        _ => FirstLook::Leave,
    }
}

/// Why a held window not placed yet is let out now, if it is: shown in its Space at
/// `ordered` and not placed within [`NOT_PLACED_AFTER`]; never ordered in by the time the
/// watch is over (`watching` false).
pub fn let_out_unplaced(ordered: Option<Instant>, watching: bool, now: Instant) -> Option<&'static str> {
    match ordered {
        Some(t) if now.saturating_duration_since(t) >= NOT_PLACED_AFTER => {
            crate::engine::margin::wait("NOT_PLACED_AFTER", now.saturating_duration_since(t), NOT_PLACED_AFTER, crate::engine::margin::End::Cap);
            Some("not placed in time")
        }
        None if !watching => Some("never shown by its app"),
        _ => None,
    }
}

/// The apps being watched now (a second launch notice for one starts no second watch).
static WATCHED: Mutex<BTreeSet<i32>> = Mutex::new(BTreeSet::new());

/// How many window-creation notices came ([`nudge`]), and the watches' wake-up.
static NUDGES: Mutex<u64> = Mutex::new(0);
static NUDGED: Condvar = Condvar::new();

/// True while an app's launch is watched.
pub fn watching() -> bool {
    WATCHED.lock().is_ok_and(|w| !w.is_empty())
}

/// A window was created (the window server's notice): every watch looks now.
pub fn nudge() {
    if let Ok(mut n) = NUDGES.lock() {
        *n += 1;
        NUDGED.notify_all();
    }
}

/// Waits [`EVERY`] or until a [`nudge`] after the one `seen` counts; true when a nudge ended it.
fn pause(seen: &mut u64) -> bool {
    let Ok(n) = NUDGES.lock() else {
        std::thread::sleep(EVERY);
        return false;
    };
    let (n, _) = NUDGED.wait_timeout_while(n, EVERY, |n| *n == *seen).unwrap_or_else(std::sync::PoisonError::into_inner);
    let woke = *n != *seen;
    *seen = *n;
    woke
}

/// Starts watching app `pid`'s new windows on a thread of its own.
pub fn start(engine: Arc<SpacesEngine>, pid: i32) {
    // On a fake desk no thread watches: a new app's windows show as their app orders them in.
    if cfg!(test) && crate::platform::fake_active() {
        return;
    }
    let fresh = WATCHED.lock().map(|mut w| w.insert(pid)).unwrap_or(false);
    if !fresh {
        return;
    }
    let spawned = std::thread::Builder::new().name("sm-launch-watch".into()).spawn(move || {
        watch(&engine, pid);
        if let Ok(mut w) = WATCHED.lock() {
            w.remove(&pid);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(pid, "launch watch: no thread: {e}");
        if let Ok(mut w) = WATCHED.lock() {
            w.remove(&pid);
        }
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The watch itself (see the module docs).
fn watch(engine: &SpacesEngine, pid: i32) {
    let began = clock::now();
    // Made ahead of the app's first window (a Space is made in a few ms).
    let _ = engine.prepare_new();
    let mut others: HashSet<u32> = HashSet::new();
    let mut looked: HashSet<u32> = HashSet::new();
    let mut waiting: BTreeMap<u32, Option<Instant>> = BTreeMap::new();
    let (mut held, mut missed, mut left_alone, mut placed, mut unplaced) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut first_held_ms: Option<u64> = None;
    let mut nudges = NUDGES.lock().map(|n| *n).unwrap_or(0);
    // Whether the look now was brought by a creation notice, and when the last look was.
    let (mut by_notice, mut held_by_notice, mut last_look) = (false, 0u32, began);
    loop {
        let now = clock::now();
        let watching = now.duration_since(began) < WATCH_FOR && alive(pid);
        if !watching && waiting.is_empty() {
            break;
        }
        if watching {
            let fresh: Vec<u32> = windows::every_window_id().into_iter().filter(|id| !others.contains(id) && !looked.contains(id)).collect();
            for w in windows::describe(&fresh) {
                if w.pid != pid {
                    others.insert(w.id);
                    continue;
                }
                looked.insert(w.id);
                let on_a_space = !engine.spaces_of(w.id).is_empty();
                match first_look(w.layer, w.onscreen, on_a_space) {
                    FirstLook::Hold => {
                        if engine.hold_new(w.id) {
                            held += 1;
                            let since = now.saturating_duration_since(last_look);
                            if by_notice {
                                held_by_notice += 1;
                                crate::engine::margin::wait_noticed("launch_watch::EVERY", since, EVERY);
                            } else {
                                crate::engine::margin::wait("launch_watch::EVERY", since, EVERY, crate::engine::margin::End::Met);
                            }
                            first_held_ms.get_or_insert(began.elapsed().as_millis() as u64);
                            waiting.insert(w.id, None);
                        }
                    }
                    FirstLook::Missed => {
                        missed += 1;
                        engine.missed_new();
                        tracing::debug!(pid, id = w.id, "launch watch: a new window was on screen before it could be held");
                    }
                    FirstLook::Leave => left_alone += 1,
                }
            }
        }
        if !waiting.is_empty() {
            let ids: Vec<u32> = waiting.keys().copied().collect();
            let listed = windows::describe(&ids);
            let now = clock::now();
            for id in ids {
                if !engine.waits_new(id) {
                    // Placed by the controller (let out, or into its stage's Space).
                    placed += 1;
                    waiting.remove(&id);
                    continue;
                }
                let Some(w) = listed.iter().find(|w| w.id == id) else {
                    // Gone: the engine forgets it at its next tidy.
                    waiting.remove(&id);
                    continue;
                };
                let ordered = waiting.entry(id).or_insert(None);
                if ordered.is_none() && w.bounds.w > 0.0 && w.bounds.h > 0.0 {
                    *ordered = Some(now);
                }
                if let Some(why) = let_out_unplaced(*ordered, watching, now) {
                    engine.let_new(id, false, why);
                    unplaced += 1;
                    waiting.remove(&id);
                }
            }
        }
        last_look = now;
        by_notice = pause(&mut nudges);
    }
    tracing::info!(
        pid,
        held,
        held_by_notice,
        placed,
        unplaced,
        missed,
        left_alone,
        first_held_ms,
        watched_ms = began.elapsed().as_millis() as u64,
        "launch watch: a just-opened app's new windows held until placed (unplaced: never shown by the app, or not placed in time)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_window_not_ordered_in_yet_is_held() {
        assert_eq!(first_look(0, false, false), FirstLook::Hold, "created, on no Space: held");
        assert_eq!(first_look(0, true, true), FirstLook::Missed, "on screen already");
        assert_eq!(first_look(0, true, false), FirstLook::Missed);
        assert_eq!(first_look(0, false, true), FirstLook::Leave, "on a Space and off screen: minimized or another Desktop's");
        assert_eq!(first_look(3, false, false), FirstLook::Leave, "a floating panel or a menu");
    }

    #[test]
    fn a_held_window_goes_out_unplaced_only_when_its_time_is_up() {
        let t = Instant::now();
        assert_eq!(let_out_unplaced(None, true, t), None, "not ordered in, still watching: waits");
        assert_eq!(let_out_unplaced(Some(t), true, t + Duration::from_millis(999)), None);
        assert_eq!(let_out_unplaced(Some(t), true, t + NOT_PLACED_AFTER), Some("not placed in time"));
        assert_eq!(let_out_unplaced(Some(t), false, t + Duration::from_millis(10)), None, "shown in its Space: its full time even past the watch");
        assert_eq!(let_out_unplaced(None, false, t), Some("never shown by its app"));
    }
}
