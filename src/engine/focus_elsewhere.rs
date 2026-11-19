//! An app's own focus on its window of another Desktop while a switch to its stage here plays.
//!
//! A focus on a window the switch brings in changes nothing: the switch lands as planned, its
//! picture unveiling the window at its frame ([`focus_on_switch`]).
//!
//! A focus held until its window's Desktop is recognized (on another Desktop, behind a
//! full-screen app, or not placed yet) leaves a running switch to land, unless a click or key
//! press came after the switch began ([`focus_on_switch`]). 11-19: a link opened Safari, the
//! switch to its stage began, and 20 ms in Safari focused its window on another Desktop; cut
//! short, the stage's window showed at once and its flying picture landed on it 0.6 s later.
//!
//! Landing, the switch keeps none of the keyboard it planned ([`without_keyboard`]): no focus,
//! and no raise of that app's windows. 11-19 13:13 (a link clicked in Mail on Desktop 4):
//! Safari activated, the switch to its stage here began with the keyboard planned for the
//! window's show, and Safari opened the link in its window of Desktop 1 and focused it; macOS
//! went there. The switch's focus was withdrawn, but its landing still raised Safari's window
//! on Desktop 4: an `AXRaise` of a window of the app in front makes it the app's key window,
//! and macOS came back to Desktop 4, away from the link. The windows still come to their
//! frames: the stage stays shown on its Desktop, unseen until the owner goes back there.
//!
//! Whatever else of ours still raises or focuses a window of that app off the Desktop it went
//! to (a landing's focus after the window's stretch, an order check's raise again) is checked
//! as it is given ([`WentElsewhere::withholds`]), until a click or key press, or
//! [`WENT_ELSEWHERE_MAX`].

use std::time::{Duration, Instant};

use super::model::{Action, Pid, SpaceId, WindowId};

/// How long an app's own focus elsewhere keeps our raises and focuses of its other windows
/// back: a switch's landing comes within its unveil backstop, a landing focus after the
/// window's stretch a moment later.
pub const WENT_ELSEWHERE_MAX: Duration = Duration::from_secs(3);

/// What a window's focus does to a running switch ([`focus_on_switch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusOnSwitch {
    /// Nothing: the focus is on a window the switch brings in, which lands as planned, behind
    /// its picture, with the switch's own focus.
    LandAsPlanned,
    /// Its windows to their frames at once, its own focus dropped.
    Supersede,
    /// Its windows land behind their pictures as planned, without its keyboard
    /// ([`without_keyboard`]).
    LandWithoutFocus,
}

/// Whether a window's focus cuts a running switch short: its windows to their frames at once,
/// its own focus dropped. A focus on a window the switch brings in (`arriving`) changes
/// nothing: the switch already does what it asks. 11-19 13:17: a link opened Safari, the
/// switch to its stage began, and Safari focused that stage's window 11 ms in; cut short,
/// the window showed at its frame at once while its picture grew from the card over it for
/// 0.64 s, a zoom onto a window already there. A focus held until its window's Desktop is
/// recognized (`held`) leaves the switch to land without its keyboard (the app's focus wins:
/// ours at the landing would pull macOS back from the Desktop the app went to), unless a
/// click or key press came after the switch began (`input_since_switch`: ⌘Tab or a Dock
/// click mid-switch).
pub fn focus_on_switch(arriving: bool, held: bool, input_since_switch: bool) -> FocusOnSwitch {
    if arriving {
        FocusOnSwitch::LandAsPlanned
    } else if !held || input_since_switch {
        FocusOnSwitch::Supersede
    } else {
        FocusOnSwitch::LandWithoutFocus
    }
}

/// What [`without_keyboard`] took out of a switch's batch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Withdrawn {
    /// The windows whose focus went.
    pub focus: Vec<u32>,
    /// The windows of the app that went elsewhere whose raise went.
    pub raises: Vec<u32>,
}

/// A switch's held batch once `app` focused its window on another Desktop: every focus goes,
/// and every raise of `app`'s windows (`pid_of`: a window's app; one not known keeps its
/// raise). The moves stay, and the raises of other apps' windows (an `AXRaise` of an app not
/// in front reorders its window without activating it).
pub fn without_keyboard(actions: Vec<Action>, app: Pid, pid_of: impl Fn(WindowId) -> Option<Pid>) -> (Vec<Action>, Withdrawn) {
    let mut withdrawn = Withdrawn::default();
    let kept = actions
        .into_iter()
        .filter(|a| match *a {
            Action::Focus { id } => {
                withdrawn.focus.push(id.0);
                false
            }
            Action::Raise { id } if pid_of(id) == Some(app) => {
                withdrawn.raises.push(id.0);
                false
            }
            _ => true,
        })
        .collect();
    (kept, withdrawn)
}

/// An app's own focus on its window of another Desktop, mid-switch, with no click or key
/// press since the switch began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WentElsewhere {
    /// The app.
    pub pid: Pid,
    /// The window it focused.
    pub window: WindowId,
    /// That window's Space (`None`: not known yet).
    pub space: Option<SpaceId>,
    /// When the focus came.
    pub at: Instant,
}

impl WentElsewhere {
    /// Over once a mouse button or key was pressed after it (`since_press`: since the last
    /// press), or [`WENT_ELSEWHERE_MAX`] after it.
    pub fn over(&self, since_press: Duration, now: Instant) -> bool {
        let age = now.saturating_duration_since(self.at);
        age >= WENT_ELSEWHERE_MAX || since_press < age
    }

    /// Whether a raise or focus of ours on `target` (of app `pid`, on Space `space`) is
    /// withheld: a window of the app, not the one it focused, off the Space it went to (any
    /// other window of the app while that Space is not known).
    pub fn withholds(&self, target: WindowId, pid: Pid, space: Option<SpaceId>, since_press: Duration, now: Instant) -> bool {
        !self.over(since_press, now) && pid == self.pid && target != self.window && (self.space.is_none() || space != self.space)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Rect;

    const SAFARI: Pid = 771;
    const MAIL: Pid = 600;

    /// 11-19: an app's own focus of its window on another Desktop, 20 ms into a switch to its
    /// stage here, showed the stage's window at once under its flying picture. A held focus
    /// lets the switch land; a click or key press since the switch began still cuts it short.
    #[test]
    fn a_held_focus_cuts_a_switch_short_only_after_input() {
        use FocusOnSwitch::*;
        assert_eq!(focus_on_switch(false, true, false), LandWithoutFocus, "the app's own focus elsewhere: the switch lands");
        assert_eq!(focus_on_switch(false, true, true), Supersede, "⌘Tab or a Dock click to another Desktop's window, mid-switch");
        assert_eq!(focus_on_switch(false, false, false), Supersede, "a window of this Desktop focused: its stage comes now");
        assert_eq!(focus_on_switch(false, false, true), Supersede);
    }

    /// 11-19 13:17, in the order the events came: Safari activated, the switch to its stage
    /// (window 150) began, its focus waiting for the window's show, and Safari focused 150
    /// itself 11 ms in. Superseded, 150 showed at its frame at once while its picture grew
    /// over it from the card for 0.64 s. A focus on a window the switch brings in lets it land
    /// as planned, its picture unveiling the window at its frame, whatever came before.
    #[test]
    fn a_focus_on_a_window_the_switch_brings_in_lets_it_land_as_planned() {
        use FocusOnSwitch::*;
        assert_eq!(focus_on_switch(true, false, false), LandAsPlanned, "the app's own focus of the window coming in");
        assert_eq!(focus_on_switch(true, false, true), LandAsPlanned, "⌘` onto it: it comes all the same");
        assert_eq!(focus_on_switch(true, true, false), LandAsPlanned);
    }

    /// 11-19 13:13, in the order the events came: Safari activated with Mail's stage in front
    /// on Desktop 4 (space 2); the switch brought Safari's stage 17 (window 150) in, its focus
    /// waiting for the window's show; Safari focused its window 151 on Desktop 1 (space 16)
    /// 33 ms later. The landing's raise of 150 took macOS back to Desktop 4. Now the switch
    /// lands with its moves and nothing that makes 150 Safari's key window, and nothing of
    /// ours raises or focuses 150 after, while 151's Desktop gets its stage.
    #[test]
    fn a_link_opened_in_the_apps_window_of_another_desktop_keeps_macos_there() {
        let t0 = Instant::now();
        let frame = Rect::new(383.0, 82.0, 1845.0, 1314.0);
        let (w150, w151, sheet, mail_w) = (WindowId(150), WindowId(151), WindowId(152), WindowId(126));
        let pid_of = |w: WindowId| match w.0 {
            150..=152 => Some(SAFARI),
            126 => Some(MAIL),
            _ => None,
        };
        // .163, .243: the switch's batch, its focus held for the window's show.
        let pending = vec![
            Action::SetFrame { id: w150, frame },
            Action::SetFrame { id: sheet, frame },
            Action::Raise { id: mail_w },
            Action::Raise { id: w150 },
            Action::Raise { id: sheet },
            Action::Focus { id: w150 },
        ];
        // .276: Safari's own focus of 151, held for its Desktop, no click or key since the switch.
        let at = t0 + Duration::from_millis(276);
        assert_eq!(focus_on_switch(false, true, false), FocusOnSwitch::LandWithoutFocus);
        let (kept, withdrawn) = without_keyboard(pending, SAFARI, pid_of);
        assert_eq!(kept, vec![Action::SetFrame { id: w150, frame }, Action::SetFrame { id: sheet, frame }, Action::Raise { id: mail_w }]);
        assert_eq!(withdrawn, Withdrawn { focus: vec![150], raises: vec![150, 152] });
        let went = WentElsewhere { pid: SAFARI, window: w151, space: Some(SpaceId(16)), at };
        // .555, the landing; a landing focus or an order check after it: 150 stays alone.
        let quiet = Duration::from_secs(60);
        let landing = at + Duration::from_millis(280);
        assert!(went.withholds(w150, SAFARI, Some(SpaceId(2)), quiet, landing), "Safari's window of Desktop 4");
        assert!(!went.withholds(mail_w, MAIL, Some(SpaceId(2)), quiet, landing), "another app's window: no activation");
        // .610, Desktop 1 recognized: 151's stage comes in front there, with its raise and focus.
        let arrival = at + Duration::from_millis(340);
        assert!(!went.withholds(w151, SAFARI, Some(SpaceId(16)), quiet, arrival), "the window the app focused");
        assert!(!went.withholds(WindowId(153), SAFARI, Some(SpaceId(16)), quiet, arrival), "its stage's other windows");
        // The owner clicks or types after it, or it is old: ours act again.
        let later = at + Duration::from_millis(900);
        assert!(!went.withholds(w150, SAFARI, Some(SpaceId(2)), Duration::from_millis(100), later), "a click or key since");
        assert!(went.withholds(w150, SAFARI, Some(SpaceId(2)), Duration::from_millis(1000), later), "the last press before it");
        assert!(!went.withholds(w150, SAFARI, Some(SpaceId(2)), quiet, at + WENT_ELSEWHERE_MAX));
    }

    /// The window the app focused not known yet (no Space): every other window of the app is
    /// withheld, and a window of no known Space too.
    #[test]
    fn a_focus_on_a_window_not_known_withholds_all_the_apps_others() {
        let at = Instant::now();
        let went = WentElsewhere { pid: SAFARI, window: WindowId(151), space: None, at };
        let quiet = Duration::from_secs(60);
        assert!(went.withholds(WindowId(150), SAFARI, Some(SpaceId(2)), quiet, at));
        assert!(!went.withholds(WindowId(151), SAFARI, None, quiet, at));
        let known = WentElsewhere { space: Some(SpaceId(16)), ..went };
        assert!(known.withholds(WindowId(150), SAFARI, None, quiet, at));
    }

    /// A raise of a window whose app is not known keeps its place; a batch with no focus and no
    /// raise of the app is left whole.
    #[test]
    fn without_keyboard_leaves_the_rest_whole() {
        let frame = Rect::new(0.0, 0.0, 10.0, 10.0);
        let batch = vec![Action::SetFrame { id: WindowId(9), frame }, Action::Raise { id: WindowId(9) }, Action::StateChanged];
        let (kept, withdrawn) = without_keyboard(batch.clone(), SAFARI, |_| None);
        assert_eq!(kept, batch);
        assert_eq!(withdrawn, Withdrawn::default());
    }
}
