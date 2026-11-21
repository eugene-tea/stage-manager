//! The cause ledger: what our own actions expect, and what the user did, so a notice can be
//! told ours from the user's by the order of events (review 11-21, R1).
//!
//! macOS never says who caused a notice. Until this round a focus or a move of a window was
//! "ours" when anything of ours had touched that window within 700 ms, whatever the action
//! and however late the thread got to the notice. Here each action of ours says what it
//! expects, and a notice is matched by kind and content:
//!
//! - **A focus** ([`Causes::expect_focus`], [`Causes::expect_raise`], [`Causes::focus_heard`])
//!   is expected of a window we focused or raised, never of one we only moved or hid. An
//!   app tells its focus changes in the order they happen, so what we asked of one app is
//!   kept in that order: a focus notice of a window is ours when it answers the earliest
//!   request for that window still open, however late it comes, and the requests before
//!   that one are over (answered, or never to be). Nothing of the user's that could have
//!   caused it may have come between (a press on that window, on anything not known, or a
//!   ⌘ key); one more notice is expected when its app's activation is awaited too. A press
//!   on the window itself makes its next focus the user's at once. [`EXPECT_MAX`] only
//!   expires a request never answered.
//! - **A move** ([`Causes::expect_move`], [`Causes::move_heard`]) is ours when the window is
//!   where our last set put it (content: however late, however many notices), or when it is
//!   the first answer to a set of ours not yet answered with no button down (the app or
//!   macOS clamped it: that frame is then where our set put it). A move while a button is
//!   down that leaves the window elsewhere is never ours: a drag right after our show is a
//!   drag.
//! - **An activation** ([`Causes::touch_app`], [`Causes::is_app_echo`]) is ours within the
//!   echo window of our `AXFrontmost`, or when it is the first reported for an app we made
//!   frontmost with nothing of the user's between: the rule the other two were modelled on.
//!
//! The ledger also keeps what the same question needs later, in one place (until round
//! r4-causes four records of the controller's own):
//!
//! - **Finder's echo of a press on the desktop** ([`Causes::desktop_pressed`],
//!   [`Causes::desktop_heard`]; [`super::desktop_echo`]): the user's press, and the first
//!   activation and focus of Finder it expects.
//! - **The windows the user (or their app) focused since we last hid them**
//!   ([`Causes::focus_not_ours`], [`Causes::focused_since_hidden`]): a focus judged not ours
//!   stays on record until our next hide of its window, for the check that adopts a parked
//!   window the user went to.
//! - **The windows our batches showed, and when** ([`Causes::showed`],
//!   [`Causes::shown_within`]): a Space change's arrival counts those just shown as carried
//!   by our switch (a time still, the arrival's own).
//!
//! What stays a time, and why:
//!
//! - The echo window (`[windows] focus_echo_ms`) still answers for a focus notice **beyond**
//!   those expected of a window we focused (an app may tell its focus more often than
//!   counted here; within the window of our own focus it is still ours, as before), for a
//!   run of an app's own adjustments after a set of ours with no button down (no event ends
//!   such a run), and for [`Causes::wrote_recently`]: "the window server's list may not show
//!   our write yet", asked by the checks that read that list.
//! - [`EXPECT_MAX`] and [`LATE_ACTIVATION_MAX`], the caps.
//!
//! Each time decision and each expectation's end is noted ([`super::margin`]): a log tells
//! how often the echo window was what decided.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::desktop_echo::{Cause, DesktopEcho, Told};
use super::geometry::Rect;
use super::margin::{self, End};

/// How long an app we made frontmost may take to report its activation and still have it
/// count as ours ([`Causes::is_app_echo`]): a busy app (Electron under load, an app answering
/// Accessibility at the 0.25 s timeout) can report it well after the echo window
/// (`[windows] focus_echo_ms`).
pub const LATE_ACTIVATION_MAX: Duration = Duration::from_secs(5);

/// How long a focus or a move we asked for may take to be reported and still be the answer
/// to our request: the cap of an expectation never met, and nothing else. Generous: a
/// request left open costs little (a later request to the same app answered, or any press
/// that could cause a focus, ends it), while a busy Mac reports a focus many seconds late
/// (the bench at load 10: 12 s), and that late notice taken for the user's switches stages
/// back, whose own focus is then told late in turn.
pub const EXPECT_MAX: Duration = Duration::from_secs(30);

/// A press the input monitor heard is the session's last press when the session's clock
/// puts that one no later than this after it (two clocks, read at two moments).
const HEARD_SLACK: Duration = Duration::from_millis(100);

/// Two frames a point apart or less are the same place (a frame read back is rounded).
const SAME_FRAME: f64 = 1.0;

fn same_frame(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= SAME_FRAME && (a.y - b.y).abs() <= SAME_FRAME && (a.w - b.w).abs() <= SAME_FRAME && (a.h - b.h).abs() <= SAME_FRAME
}

/// Where a press of the user's landed, as far as it is known ([`Causes::pressed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressed {
    /// On this window (the frontmost one under the pointer in the window list read for the
    /// press).
    Window(u32),
    /// Somewhere else or not known (the Dock, the menu bar, the wallpaper, a ⌘ key): it may
    /// have caused a focus of any window.
    Elsewhere,
}

/// A focus we asked of an app (a focus or a raise of ours).
#[derive(Clone, Copy, Debug)]
struct FocusExpected {
    /// The window.
    window: u32,
    /// When we asked last.
    asked: Instant,
    /// The focus notices still expected of it.
    left: u8,
    /// The user pressed on the window since: its focus is theirs.
    pressed: bool,
}

/// A frame we set.
#[derive(Clone, Copy, Debug)]
struct MoveExpected {
    /// Where our last set put the window: the frame asked, or the one its first answer
    /// showed (clamped).
    frame: Rect,
    /// When we set it.
    asked: Instant,
    /// No move notice answered it yet.
    unmet: bool,
}

/// The cause ledger (module docs).
#[derive(Debug)]
pub struct Causes {
    /// Every window we wrote to (moved, hid, raised, focused), and when.
    wrote: HashMap<u32, Instant>,
    /// What we asked each app to focus, oldest first.
    focus: HashMap<i32, Vec<FocusExpected>>,
    moves: HashMap<u32, MoveExpected>,
    apps: HashMap<i32, Instant>,
    /// Apps we made frontmost whose activation has not been reported yet.
    awaiting: HashMap<i32, Instant>,
    /// The app whose activation was reported last (ours or not): frontmost as far as we know.
    /// A focus on it awaits no activation, since macOS reports none for an app already in
    /// front.
    front: Option<i32>,
    /// The last press of the user's the input monitor heard, and when.
    last_press: Option<(Pressed, Instant)>,
    /// The last press on the desktop, and what Finder still owes it.
    desktop: DesktopEcho,
    /// The windows a focus not ours named since we last hid them, and when.
    users_focus: HashMap<u32, Instant>,
    /// The windows a batch of ours showed (set a frame for), and when.
    shown: HashMap<u32, Instant>,
    /// Each app's focused window as we believe it is, or will be once the app has done what
    /// we asked: the window we last asked it to focus, or the one a focus notice not ours
    /// named since.
    believed: HashMap<i32, u32>,
    window: Duration,
}

impl Default for Causes {
    fn default() -> Self {
        // The echo window: `[windows] focus_echo_ms` as the config has it by default (set from
        // the config in force at the controller's start, [`Self::set_window`]).
        let window = Duration::from_millis(crate::config::Config::default().windows.focus_echo_ms);
        Self {
            wrote: HashMap::new(),
            focus: HashMap::new(),
            moves: HashMap::new(),
            apps: HashMap::new(),
            awaiting: HashMap::new(),
            front: None,
            last_press: None,
            desktop: DesktopEcho::default(),
            users_focus: HashMap::new(),
            shown: HashMap::new(),
            believed: HashMap::new(),
            window,
        }
    }
}

impl Causes {
    /// Sets the echo window.
    pub fn set_window(&mut self, window: Duration) {
        self.window = window;
    }

    // ----- what we did --------------------------------------------------------------------

    /// We just wrote to this window (moved, hid, raised or focused it).
    pub fn wrote(&mut self, id: u32, now: Instant) {
        self.wrote.insert(id, now);
    }

    /// We just set this window's frame to `frame` (a show, a park). `tracked`: its frame as
    /// last read; already there, no move notice is expected of the set.
    pub fn expect_move(&mut self, id: u32, frame: Rect, tracked: Option<Rect>, now: Instant) {
        self.wrote(id, now);
        let unmet = !tracked.is_some_and(|t| same_frame(t, frame));
        self.moves.insert(id, MoveExpected { frame, asked: now, unmet });
    }

    /// `notices` focus notices are expected of window `id` of app `pid`, asked now: one
    /// request with the one before it when that was for the same window (a switch focuses
    /// its key window at once and again at the landing).
    /// A batch of ours just showed this window (set its frame): on record for the arrival of
    /// a Space change ([`Self::shown_within`]).
    pub fn showed(&mut self, id: u32, now: Instant) {
        self.shown.insert(id, now);
    }

    /// The windows our batches showed less than `limit` before `at` (time window `name`,
    /// each noted); the others are forgotten.
    pub fn shown_within(&mut self, at: Instant, limit: Duration, name: &'static str) -> Vec<u32> {
        self.shown.retain(|_, shown| margin::within(name, at.saturating_duration_since(*shown), limit));
        self.shown.keys().copied().collect()
    }

    /// We just hid this window: a focus of it before this is not the user's choice about
    /// where it is now.
    pub fn hid(&mut self, id: u32) {
        self.users_focus.remove(&id);
    }

    /// A focus of window `id` was judged not ours (the user's, or its app's own): on record
    /// until our next hide of it.
    pub fn focus_not_ours(&mut self, id: u32, now: Instant) {
        self.users_focus.insert(id, now);
    }

    /// True when the user (or its app) focused window `id` since we last hid it.
    pub fn focused_since_hidden(&self, id: u32) -> bool {
        self.users_focus.contains_key(&id)
    }

    fn expect(&mut self, id: u32, pid: i32, notices: u8, now: Instant) {
        let asked = self.focus.entry(pid).or_default();
        // What an earlier request for this window left closed (answered, or ended by a
        // press of the user's on it) says nothing of this one.
        let last = asked.len().saturating_sub(1);
        let mut i = 0;
        asked.retain(|e| {
            i += 1;
            i - 1 == last || e.window != id || e.left > 0
        });
        match asked.last_mut().filter(|e| e.window == id && now.saturating_duration_since(e.asked) < EXPECT_MAX) {
            Some(e) => *e = FocusExpected { window: id, asked: now, left: e.left.max(notices), pressed: false },
            None => asked.push(FocusExpected { window: id, asked: now, left: notices, pressed: false }),
        }
    }

    /// True when app `pid`'s focused window, as we believe it ([`Self::believed`]), is
    /// another than `id`: making `id` its key window changes it, and the app will say so. An
    /// app we know nothing of yet counts as unchanged: a request left open with nothing to
    /// answer it would take the app's own next focus of that window for ours, while one
    /// expectation too few leaves a notice to the echo window and the gates after it, as
    /// before.
    fn changes_focus(&self, id: u32, pid: i32) -> bool {
        self.believed.get(&pid).is_some_and(|w| *w != id)
    }

    /// We just raised this window of app `pid`: when its app is in front (as far as we know)
    /// the raise makes it the app's key window, and one focus notice is expected of it.
    pub fn expect_raise(&mut self, id: u32, pid: i32, now: Instant) {
        self.wrote(id, now);
        let in_front = self.front == Some(pid);
        let becomes_key = in_front && self.changes_focus(id, pid);
        self.expect(id, pid, u8::from(becomes_key), now);
        if in_front {
            self.believed.insert(pid, id);
        }
    }

    /// We just focused this window of app `pid` (made it main and its app frontmost): a
    /// focus notice is expected when it was not the app's focused window (as its last notice
    /// said), and one more with its app's activation when another app was in front.
    pub fn expect_focus(&mut self, id: u32, pid: i32, now: Instant) {
        self.wrote(id, now);
        let changes = self.changes_focus(id, pid);
        let activates = self.front != Some(pid);
        self.expect(id, pid, u8::from(changes) + u8::from(activates), now);
        self.believed.insert(pid, id);
        self.touch_app(pid, now);
    }

    /// We just made this app frontmost. Its activation is awaited only when another app was
    /// in front: a switch focuses its key window twice (at once, then again at the landing),
    /// and the second focus, after the activation was reported, left one awaited that never
    /// came, so macOS's own activation of that app up to 5 s later was taken for ours
    /// (11-09 suite: the front app quit, macOS activated the fixture 4.4 s after the
    /// landing, and its stage never came).
    pub fn touch_app(&mut self, pid: i32, now: Instant) {
        self.apps.insert(pid, now);
        if self.front != Some(pid) {
            self.awaiting.insert(pid, now);
        }
    }

    // ----- what the user did ----------------------------------------------------------------

    /// The user pressed a mouse button `on`: a focus that follows it on the window pressed
    /// is theirs; a press elsewhere (or a ⌘ key, [`Pressed::Elsewhere`]) may have caused any
    /// focus, so none is ours by order any more.
    pub fn pressed(&mut self, on: Pressed, now: Instant) {
        self.last_press = Some((on, now));
        let refute = |e: &mut FocusExpected| {
            if e.left > 0 {
                margin::wait("EXPECTED_FOCUS", now.saturating_duration_since(e.asked), EXPECT_MAX, End::Refuted);
            }
            e.left = 0;
        };
        let all = self.focus.values_mut().flatten();
        match on {
            Pressed::Window(id) => all.filter(|e| e.window == id).for_each(|e| {
                refute(e);
                e.pressed = true;
            }),
            Pressed::Elsewhere => all.for_each(refute),
        }
    }

    /// Whether the user's last press was on window `id`, when that is known: `last` is when
    /// the session's last press of a button or key came (its own clock, which also counts
    /// keys and clicks on our own windows, unheard by the monitor). `Some(true)`: the press
    /// heard last is that one and was on this window; `Some(false)`: on another window;
    /// `None`: not known (a key, a press elsewhere, one never heard): time is all there is.
    pub fn pressed_on(&self, id: u32, last: Option<Instant>) -> Option<bool> {
        let (on, heard) = self.last_press?;
        if last? > heard + HEARD_SLACK {
            return None;
        }
        match on {
            Pressed::Window(w) => Some(w == id),
            Pressed::Elsewhere => None,
        }
    }

    /// The user pressed the desktop (the wallpaper or a desktop icon): Finder's first
    /// activation and first window focus that follow are the press's echo
    /// ([`DesktopEcho`]).
    pub fn desktop_pressed(&mut self, now: Instant) {
        self.desktop.desktop_pressed(now);
    }

    /// A left mouse-up: a desktop press still down is released now.
    pub fn released(&mut self, now: Instant) {
        self.desktop.released(now);
    }

    /// The user did something else than press the desktop (a mouse-down off it, a key
    /// press): what Finder does next is no echo. True when an echo was pending.
    pub fn off_the_desktop(&mut self) -> bool {
        self.desktop.user_acted()
    }

    /// App `bundle_id` told `told`, received at `at`: true when it is Finder's echo of the
    /// last desktop press ([`DesktopEcho::heard`]). Call it once per notice.
    pub fn desktop_heard(&mut self, bundle_id: Option<&str>, told: Told, at: Instant) -> bool {
        self.desktop.heard(bundle_id, told, at) == Cause::WallpaperEcho
    }

    /// The user may have switched apps (a click, a ⌘ key press, another app activated on
    /// purpose): an activation reported from now on beyond the echo window is theirs, not a
    /// late echo.
    pub fn user_switched(&mut self) {
        self.awaiting.clear();
    }

    // ----- what macOS tells -----------------------------------------------------------------

    /// A focus notice of window `id` (of app `pid`; `None`: not tracked, nothing was asked of
    /// it), received at `at`: true when it is the answer to a focus or raise of ours (module
    /// docs). Call it once per notice: it consumes what it matches.
    pub fn focus_heard(&mut self, id: u32, pid: Option<i32>, at: Instant) -> bool {
        let Some(pid) = pid else { return false };
        let ours = self.answers(id, pid, at);
        if !ours {
            // The app's own change, or the user's: where its focus is now.
            self.believed.insert(pid, id);
        }
        ours
    }

    fn answers(&mut self, id: u32, pid: i32, at: Instant) -> bool {
        let window = self.window;
        let Some(asked) = self.focus.get_mut(&pid) else { return false };
        if asked.iter().any(|e| e.window == id && e.pressed) {
            // The user pressed on it since we asked: theirs.
            return false;
        }
        let open = |e: &FocusExpected| e.window == id && e.left > 0 && at.saturating_duration_since(e.asked) < EXPECT_MAX;
        if let Some(i) = asked.iter().position(open) {
            // By order: the answer to the earliest request for it still open, however late.
            // The app tells its focus in order, so what we asked of it before is over.
            let age = margin::age("EXPECTED_FOCUS", at, asked[i].asked);
            asked[i].left -= 1;
            margin::wait("EXPECTED_FOCUS", age, EXPECT_MAX, End::Met);
            for e in asked.drain(..i).filter(|e| e.left > 0) {
                margin::wait("EXPECTED_FOCUS", at.saturating_duration_since(e.asked), EXPECT_MAX, End::Cap);
            }
            return true;
        }
        // By time, as before: one more than expected, within the echo window of our focus.
        let Some(last) = asked.iter().rev().find(|e| e.window == id) else { return false };
        let age = margin::age("ECHO_WINDOW", at, last.asked);
        age < window.saturating_mul(2) && margin::within("ECHO_WINDOW", age, window)
    }

    /// A move or resize notice of window `id`, received at `at`, the window now at `frame`
    /// (`None`: not known), `button_down`: the left button is: true when it is the answer to
    /// a frame of ours (module docs).
    pub fn move_heard(&mut self, id: u32, frame: Option<Rect>, button_down: bool, at: Instant) -> bool {
        let window = self.window;
        let Some(e) = self.moves.get_mut(&id) else { return false };
        let age = margin::age("EXPECTED_MOVE", at, e.asked);
        if frame.is_some_and(|f| same_frame(f, e.frame)) {
            // By content: the window is where our set put it.
            if std::mem::take(&mut e.unmet) {
                margin::wait("EXPECTED_MOVE", age, EXPECT_MAX, End::Met);
            }
            return true;
        }
        if button_down {
            // Under the user's hand, and not where we put it: theirs. Where we put it is
            // no longer where it is.
            self.moves.remove(&id);
            return false;
        }
        if e.unmet && age < EXPECT_MAX {
            // By order: the first answer to our set, clamped by the app or by macOS.
            e.unmet = false;
            if let Some(f) = frame {
                e.frame = f;
            }
            margin::wait("EXPECTED_MOVE", age, EXPECT_MAX, End::Met);
            return true;
        }
        // By time, as before: an app still adjusting after our set. (A set long answered
        // is no time decision: nothing to note.)
        let ours = age < window.saturating_mul(2) && margin::within("ECHO_WINDOW", age, window);
        match (ours, frame) {
            (true, Some(f)) => e.frame = f,
            (true, None) => {}
            (false, _) => {
                self.moves.remove(&id);
            }
        }
        ours
    }

    /// True when an activation of `pid` is probably our own: within the echo window, or
    /// the first activation reported since we made it frontmost with no user input between.
    /// Call it once per reported activation: it consumes the awaited one.
    ///
    /// An awaited one it consumes also ends the awaits of the apps we made frontmost more
    /// than an echo window before it: our later switch made this app frontmost and macOS
    /// says so, so an older request is answered by now or never will be (11-14, the live
    /// landing: the fixture's activation never came, Alpha's switch 1.8 s later was reported,
    /// and the user's activation of the fixture 1.3 s after that was taken for the late
    /// echo). Quick switches keep theirs: within the echo window of each other the first
    /// app may still be answering, and one switch focuses one window.
    pub fn is_app_echo(&mut self, pid: i32, now: Instant) -> bool {
        self.front = Some(pid);
        let asked = self.awaiting.remove(&pid);
        if let Some(at) = asked {
            let window = self.window;
            self.awaiting.retain(|_, t| at.saturating_duration_since(*t) < window);
        }
        let late = asked.is_some_and(|t| margin::within("LATE_ACTIVATION_MAX", margin::age("LATE_ACTIVATION_MAX", now, t), LATE_ACTIVATION_MAX));
        let window = self.window;
        late || self.apps.get(&pid).is_some_and(|t| margin::within("ECHO_WINDOW", margin::age("ECHO_WINDOW", now, *t), window))
    }

    // ----- "our write may not show yet" -------------------------------------------------------

    /// True when we wrote to window `id` within the echo window before `now`: the window
    /// server's list may not show the write yet (asked by the checks that read that list).
    pub fn wrote_recently(&self, id: u32, now: Instant) -> bool {
        self.wrote.get(&id).is_some_and(|t| now.duration_since(*t) < self.window)
    }

    /// [`Self::wrote_recently`] for a notice received at `at`, its margin noted: a notice
    /// stamped before our write counts as at it.
    pub fn wrote_before(&self, id: u32, at: Instant) -> bool {
        let Some(t) = self.wrote.get(&id) else { return false };
        margin::within("ECHO_WINDOW", margin::age("ECHO_WINDOW", at, *t), self.window)
    }

    /// Forgets old entries. Writes are kept up to [`LATE_ACTIVATION_MAX`], longer than the
    /// echo window they answer for: a notice handled late is judged by its receipt, which may
    /// be that much older than now. A focus expected and never told ends by its cap, noted.
    pub fn prune(&mut self, now: Instant) {
        let keep = self.window.max(LATE_ACTIVATION_MAX);
        self.wrote.retain(|_, t| now.duration_since(*t) < keep);
        self.apps.retain(|_, t| now.duration_since(*t) < keep);
        self.awaiting.retain(|_, t| now.saturating_duration_since(*t) < LATE_ACTIVATION_MAX);
        for asked in self.focus.values_mut() {
            asked.retain(|e| {
                let age = now.saturating_duration_since(e.asked);
                if age >= EXPECT_MAX && e.left > 0 {
                    margin::wait("EXPECTED_FOCUS", age, EXPECT_MAX, End::Cap);
                }
                // One answered stays while the echo window may still ask about it.
                age < if e.left > 0 { EXPECT_MAX } else { keep }
            });
        }
        self.focus.retain(|_, asked| !asked.is_empty());
        self.shown.retain(|_, t| now.saturating_duration_since(*t) < keep);
        // A frame we set stays the frame we set until the window moves elsewhere: only an
        // unanswered set ends by time.
        for e in self.moves.values_mut() {
            if e.unmet && now.saturating_duration_since(e.asked) >= EXPECT_MAX {
                e.unmet = false;
            }
        }
    }

    /// Window `id` is gone: nothing is expected of it.
    pub fn forget_window(&mut self, id: u32) {
        self.wrote.remove(&id);
        self.focus.values_mut().for_each(|asked| asked.retain(|e| e.window != id));
        self.moves.remove(&id);
        self.users_focus.remove(&id);
        self.shown.remove(&id);
        self.believed.retain(|_, w| *w != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;
    const AT: Rect = Rect { x: 240.0, y: 130.0, w: 700.0, h: 450.0 };
    const PARK: Rect = Rect { x: 1439.0, y: 793.0, w: 700.0, h: 450.0 };

    // ----- a focus --------------------------------------------------------------------------

    /// The 11-03 class: our focus of a window reported 1.2 s late, past the echo window, by
    /// a busy app. The first focus of it since we asked: ours. A second one is not.
    #[test]
    fn the_first_focus_after_ours_is_ours_however_late() {
        let t0 = Instant::now();
        let mut c = Causes { front: Some(42), ..Causes::default() };
        c.believed.insert(42, 1);
        c.expect_focus(7, 42, t0);
        assert!(c.focus_heard(7, Some(42), t0 + MS(1200)), "late, but the one we asked for");
        assert!(!c.focus_heard(7, Some(42), t0 + MS(1300)), "a second one, past the echo window, is not");
        assert!(!c.focus_heard(7, None, t0), "a window not tracked was asked nothing");
        // Past the cap it is nobody's answer.
        c.expect_focus(8, 42, t0);
        assert!(!c.focus_heard(8, Some(42), t0 + EXPECT_MAX));
    }

    /// The bench at load 10: two card clicks for two stages of one app, 0.5 s apart, each
    /// focus told 12 s late. The app tells them in the order we asked: both ours, and no
    /// stage comes back.
    #[test]
    fn an_app_answers_in_the_order_we_asked() {
        let t0 = Instant::now();
        let mut c = Causes { front: Some(42), ..Causes::default() };
        c.believed.insert(42, 1);
        c.expect_focus(17, 42, t0);
        c.expect_focus(11, 42, t0 + MS(480));
        assert!(c.focus_heard(17, Some(42), t0 + MS(12_000)));
        assert!(c.focus_heard(11, Some(42), t0 + MS(12_480)));
        assert!(!c.focus_heard(17, Some(42), t0 + MS(13_000)), "nothing more was asked for");
        // A request the app never answered (its focus was there already) is over once a
        // later one is: its window focused after that is the app's own, or the user's.
        c.expect_focus(17, 42, t0 + MS(20_000));
        c.expect_focus(11, 42, t0 + MS(21_000));
        assert!(c.focus_heard(11, Some(42), t0 + MS(21_020)));
        assert!(!c.focus_heard(17, Some(42), t0 + MS(24_000)));
        // The app's own focus of a window we asked nothing of leaves what we asked open.
        c.expect_focus(19, 42, t0 + MS(40_000));
        assert!(!c.focus_heard(5, Some(42), t0 + MS(40_500)));
        assert!(c.focus_heard(19, Some(42), t0 + MS(42_000)));
    }

    /// Its app behind: the focus is told once for the window made main and once more with
    /// the activation, in either order with the activation's own notice.
    #[test]
    fn a_focus_of_an_app_behind_expects_one_more_with_its_activation() {
        let t0 = Instant::now();
        let mut c = Causes { front: Some(9), ..Causes::default() };
        c.believed.insert(42, 1);
        c.expect_focus(7, 42, t0);
        assert!(c.focus_heard(7, Some(42), t0 + MS(900)));
        assert!(c.is_app_echo(42, t0 + MS(2000)));
        assert!(c.focus_heard(7, Some(42), t0 + MS(2010)));
        assert!(!c.focus_heard(7, Some(42), t0 + MS(2020)));
        // Already its app's focused window (its last notice said so): only the activation's.
        c.front = Some(9);
        c.expect_focus(7, 42, t0 + MS(3000));
        assert!(c.focus_heard(7, Some(42), t0 + MS(4500)));
        assert!(!c.focus_heard(7, Some(42), t0 + MS(4600)));
    }

    /// Kind: a hide or a move of ours expects no focus. The window parked 300 ms ago and
    /// focused now (its sliver clicked) is the user's; so is a window just shown.
    #[test]
    fn a_hide_or_a_move_expects_no_focus() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        c.expect_move(7, PARK, Some(AT), t0);
        c.wrote(8, t0);
        assert!(!c.focus_heard(7, Some(42), t0 + MS(300)));
        assert!(!c.focus_heard(8, Some(42), t0 + MS(300)));
        assert!(c.wrote_recently(7, t0 + MS(300)), "the list may not show the park yet all the same");
    }

    /// Within the echo window of our own focus a notice beyond those counted is still ours
    /// (an app may tell its focus more than once): time, as before.
    #[test]
    fn more_focus_notices_than_counted_are_ours_within_the_echo_window_of_our_focus() {
        let t0 = Instant::now();
        let mut c = Causes { front: Some(42), ..Causes::default() };
        c.believed.insert(42, 1);
        c.expect_focus(7, 42, t0);
        assert!(c.focus_heard(7, Some(42), t0 + MS(15)));
        assert!(c.focus_heard(7, Some(42), t0 + MS(40)), "told twice");
        assert!(!c.focus_heard(7, Some(42), t0 + MS(800)));
    }

    /// A press on the window itself: its next focus is the user's, even within the echo
    /// window of our raise of it (a click on a window 300 ms after our switch showed it).
    /// A press on another window leaves the expectation; one elsewhere ends the order's
    /// part and leaves the echo window's.
    #[test]
    fn a_press_on_the_window_makes_its_focus_the_users() {
        let t0 = Instant::now();
        let mut c = Causes { front: Some(42), ..Causes::default() };
        c.believed.insert(42, 7);
        c.expect_raise(8, 42, t0);
        c.pressed(Pressed::Window(8), t0 + MS(300));
        assert!(!c.focus_heard(8, Some(42), t0 + MS(320)));
        // Another window pressed: ours still, by order.
        c.expect_focus(7, 42, t0 + MS(1000));
        c.pressed(Pressed::Window(8), t0 + MS(1100));
        assert!(c.focus_heard(7, Some(42), t0 + MS(2500)));
        // A press on the Dock or a ⌘ key: the late one is no longer ours, a prompt one is.
        c.expect_focus(8, 42, t0 + MS(3000));
        c.pressed(Pressed::Elsewhere, t0 + MS(3100));
        assert!(c.focus_heard(8, Some(42), t0 + MS(3200)), "within the echo window of our focus");
        c.expect_focus(7, 42, t0 + MS(4000));
        c.pressed(Pressed::Elsewhere, t0 + MS(4100));
        assert!(!c.focus_heard(7, Some(42), t0 + MS(5500)), "⌘` back to it, or the late echo: not ours by order");
    }

    /// The user's own: the press heard last is told from a later one the monitor never
    /// heard (a key, a click on our own strip) by the session's clock.
    #[test]
    fn the_last_press_is_on_a_window_only_while_no_later_press_came() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        assert_eq!(c.pressed_on(7, Some(t0)), None, "none heard");
        c.pressed(Pressed::Window(7), t0);
        assert_eq!(c.pressed_on(7, Some(t0 + MS(3))), Some(true));
        assert_eq!(c.pressed_on(8, Some(t0 + MS(3))), Some(false), "the click was on another window");
        assert_eq!(c.pressed_on(7, Some(t0 + MS(900))), None, "a key since, or a click on our own strip");
        c.pressed(Pressed::Elsewhere, t0 + MS(2000));
        assert_eq!(c.pressed_on(7, Some(t0 + MS(2000))), None, "the Dock, the menu bar: not known");
    }

    /// A press on a window ends what was asked of it; a focus of ours asked after the press
    /// is expected afresh, also with other requests in between.
    #[test]
    fn a_request_after_a_press_on_the_window_is_expected_afresh() {
        let t0 = Instant::now();
        let mut c = Causes { front: Some(42), ..Causes::default() };
        c.believed.insert(42, 1);
        c.expect_focus(7, 42, t0);
        c.pressed(Pressed::Window(7), t0 + MS(100));
        assert!(!c.focus_heard(7, Some(42), t0 + MS(120)), "the click's own");
        c.expect_focus(8, 42, t0 + MS(1000));
        c.expect_focus(7, 42, t0 + MS(2000));
        assert!(c.focus_heard(8, Some(42), t0 + MS(3500)));
        assert!(c.focus_heard(7, Some(42), t0 + MS(3600)), "ours again: asked after the press");
    }

    /// A raise makes a window its app's key window only when that app is in front: behind,
    /// nothing is expected of it.
    #[test]
    fn a_raise_expects_a_focus_only_of_the_app_in_front() {
        let t0 = Instant::now();
        let mut c = Causes { front: Some(42), ..Causes::default() };
        c.believed.insert(42, 1);
        c.believed.insert(50, 2);
        c.expect_raise(7, 42, t0);
        assert!(c.focus_heard(7, Some(42), t0 + MS(1500)));
        c.expect_raise(8, 50, t0);
        assert!(!c.focus_heard(8, Some(50), t0 + MS(1500)));
    }

    // ----- a move ---------------------------------------------------------------------------

    /// Content: the window at the frame we set is our move however late its notices come
    /// and however many; the same window dragged elsewhere 200 ms after the show is the
    /// user's from the first notice.
    #[test]
    fn a_move_is_ours_by_where_the_window_is() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        c.expect_move(7, AT, Some(PARK), t0);
        assert!(c.move_heard(7, Some(AT), false, t0 + MS(10)));
        assert!(c.move_heard(7, Some(AT), false, t0 + MS(3000)), "the resize's notice, late");
        let dragged = Rect { x: AT.x + 24.0, ..AT };
        assert!(!c.move_heard(7, Some(dragged), true, t0 + MS(3100)));
        assert!(!c.move_heard(7, Some(AT), true, t0 + MS(3200)), "dragged back through it: no longer where we put it");
    }

    /// A drag right after a show: 200 ms after our set, its echo long handled.
    #[test]
    fn a_drag_right_after_our_show_is_a_drag() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        c.expect_move(7, AT, Some(PARK), t0);
        assert!(c.move_heard(7, Some(AT), false, t0 + MS(10)));
        assert!(!c.move_heard(7, Some(Rect { x: AT.x + 8.0, y: AT.y + 4.0, ..AT }), true, t0 + MS(260)));
    }

    /// The app clamps what we set: the first answer with no button down is ours, late or
    /// not, and that frame is then where our set put the window.
    #[test]
    fn the_first_answer_to_our_set_is_ours_though_clamped() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        let clamped = Rect { w: 720.0, ..AT };
        c.expect_move(7, AT, Some(PARK), t0);
        assert!(c.move_heard(7, Some(clamped), false, t0 + MS(1500)));
        assert!(c.move_heard(7, Some(clamped), false, t0 + MS(1600)), "its second notice: by content now");
        assert!(!c.move_heard(7, Some(Rect { x: 0.0, ..clamped }), false, t0 + MS(2400)), "a later move elsewhere is the app's or the user's");
        // A set that changes nothing expects no answer: the next move is not ours.
        c.expect_move(8, AT, Some(AT), t0);
        assert!(!c.move_heard(8, Some(clamped), false, t0 + MS(1500)));
        // An app still adjusting within the echo window of our set (no button): ours, as before.
        c.expect_move(9, AT, Some(PARK), t0);
        assert!(c.move_heard(9, Some(clamped), false, t0 + MS(20)));
        assert!(c.move_heard(9, Some(Rect { w: 740.0, ..AT }), false, t0 + MS(60)));
    }

    /// The review of 1.9, B-8: the windows our batches showed are kept only for a while,
    /// also with no Space change ever (one Desktop), and an arrival names those just shown.
    #[test]
    fn the_windows_we_showed_are_named_to_an_arrival_only_for_a_while() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        for i in 0..100 {
            c.showed(i, t0 + MS(u64::from(i) * 100));
            c.prune(t0 + MS(u64::from(i) * 100));
        }
        assert!(c.shown.len() <= 51, "pruned with no arrival ever: {}", c.shown.len());
        let mut carried = c.shown_within(t0 + MS(9_900), MS(1500), "SLIDE_CARRY");
        carried.sort_unstable();
        assert_eq!(carried, (85..100).collect::<Vec<u32>>(), "only those within the carry of the arrival");
    }

    /// A focus judged not ours stays on record until our next hide of its window.
    #[test]
    fn a_focus_not_ours_is_remembered_until_we_hide_its_window() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        c.focus_not_ours(7, t0);
        c.prune(t0 + Duration::from_secs(3600));
        assert!(c.focused_since_hidden(7));
        c.hid(7);
        assert!(!c.focused_since_hidden(7));
    }

    // ----- an activation --------------------------------------------------------------------

    #[test]
    fn an_echo_is_judged_by_when_its_notice_came_not_when_it_was_handled() {
        let t0 = Instant::now();
        let mut e = Causes::default();
        e.wrote(7, t0);
        e.touch_app(42, t0);
        e.user_switched();
        let fired = t0 + MS(200);
        let handled = fired + MS(600);
        e.prune(handled);
        assert!(!e.wrote_recently(7, handled), "measured at the handling, as before, it was the user's");
        assert!(e.wrote_recently(7, fired), "measured at the notice: ours");
        assert!(e.is_app_echo(42, fired), "the activation too, past the awaited one");
        assert!(!e.wrote_recently(7, t0 + e.window), "a notice that came past the window is not");
    }

    #[test]
    fn echoes_expire() {
        let mut e = Causes::default();
        let t0 = Instant::now();
        e.wrote(7, t0);
        e.touch_app(42, t0);
        assert!(e.wrote_recently(7, t0 + MS(100)));
        assert!(e.is_app_echo(42, t0 + MS(100)));
        assert!(!e.wrote_recently(8, t0));
        let echo = Duration::from_millis(crate::config::Config::default().windows.focus_echo_ms);
        assert!(!e.wrote_recently(7, t0 + echo));
        e.prune(t0 + echo);
        assert!(!e.wrote.is_empty() && !e.apps.is_empty(), "kept for a notice handled late");
        e.prune(t0 + LATE_ACTIVATION_MAX);
        assert!(e.wrote.is_empty() && e.apps.is_empty());
    }

    #[test]
    fn a_late_activation_after_a_second_quick_switch_is_still_ours() {
        let (slack, mail) = (10, 20);
        let mut e = Causes::default();
        let t0 = Instant::now();
        e.touch_app(slack, t0);
        e.touch_app(mail, t0 + MS(400));
        assert!(e.is_app_echo(mail, t0 + MS(450)));
        // Slack, busy, reports its activation 800 ms after our focus: past the echo window.
        assert!(e.is_app_echo(slack, t0 + MS(800)), "late, but the one we asked for");
        assert!(!e.is_app_echo(slack, t0 + MS(900)), "a second one is not");
    }

    #[test]
    fn a_landing_focus_after_the_activation_awaits_no_second_one() {
        // The 11-09 suite: a card click to the fixture's stage focuses its window at once
        // (46.981) and macOS reports the activation (46.996); the landing focuses it again
        // (~47.5). A card click to Alpha follows (49.8, its activation ours), then Alpha quits
        // and macOS activates the fixture (51.939): the user's, not an echo.
        let (fixture, alpha) = (67303, 67932);
        let mut e = Causes::default();
        let t0 = Instant::now();
        e.touch_app(fixture, t0);
        assert!(e.is_app_echo(fixture, t0 + MS(15)));
        e.touch_app(fixture, t0 + MS(560));
        e.touch_app(alpha, t0 + MS(2850));
        assert!(e.is_app_echo(alpha, t0 + MS(2910)));
        assert!(!e.is_app_echo(fixture, t0 + MS(4960)), "macOS's activation after the front app quit");
        // A landing focus that comes before the activation is reported still awaits it.
        let mut e = Causes::default();
        e.touch_app(alpha, t0);
        e.touch_app(fixture, t0 + MS(10));
        e.touch_app(fixture, t0 + MS(500));
        assert!(e.is_app_echo(fixture, t0 + MS(1500)), "late, but the one we asked for");
    }

    #[test]
    fn our_later_switch_ends_an_older_apps_late_echo() {
        // The suite on 9af41d3 (11-14, the live landing): the switch to the fixture's stage
        // focuses its window (14.578, again at the unveil 15.121) and no activation comes; a
        // switch to Alpha's new stage (16.880) is reported (16.937); the test's ⌘Tab-style
        // activation of the fixture (18.265) is the user's, not that late echo.
        let (fixture, alpha) = (85618, 86476);
        let mut e = Causes::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + MS(ms);
        e.touch_app(fixture, at(0));
        e.touch_app(fixture, at(543));
        e.touch_app(alpha, at(2302));
        assert!(e.is_app_echo(alpha, at(2359)));
        assert!(!e.is_app_echo(fixture, at(3687)), "the user's activation brings the fixture's stage");
        // Only an awaited activation ends the others': one reported within the echo window
        // of a focus that awaited none (the app was in front) leaves them.
        let mut e = Causes::default();
        e.touch_app(fixture, at(0));
        e.front = Some(alpha);
        e.touch_app(alpha, at(1000));
        assert!(e.is_app_echo(alpha, at(1050)));
        assert!(e.is_app_echo(fixture, at(1400)), "still the late echo we asked for");
        // Two apps focused by one batch (or two quick switches): the first one's late echo
        // stays ours after the second one's activation.
        let (mail, slack) = (20, 10);
        let mut e = Causes::default();
        e.touch_app(slack, at(0));
        e.touch_app(mail, at(5));
        assert!(e.is_app_echo(mail, at(30)));
        assert!(e.is_app_echo(slack, at(1500)), "busy, but the one we asked for");
    }

    #[test]
    fn a_user_switch_ends_a_pending_late_echo() {
        let slack = 10;
        let mut e = Causes::default();
        let t0 = Instant::now();
        e.touch_app(slack, t0);
        e.user_switched();
        assert!(!e.is_app_echo(slack, t0 + MS(900)), "a Dock click or ⌘Tab to it is the user's");
        assert!(e.is_app_echo(slack, t0 + MS(100)), "still within the echo window");
        // Never awaited forever: an app that was already frontmost never reports one.
        e.touch_app(slack, t0);
        e.prune(t0 + LATE_ACTIVATION_MAX);
        assert!(!e.is_app_echo(slack, t0 + LATE_ACTIVATION_MAX));
    }
}
