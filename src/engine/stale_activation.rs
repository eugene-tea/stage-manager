//! A late activation never switches stages back (round stale-activations).
//!
//! macOS tells us an app became frontmost through NSWorkspace's notice, which the main thread
//! stamps when it gets it ([`crate::platform::apps::AppEvent::Activated`]) and the WM thread
//! decides on later. Under load that gap grows to seconds (the owner's 11-03 14:09:57: the Mac's load
//! at ~110, our notices 1–2.6 s late): every late activation was taken as the owner's, each switch
//! superseded the one before and brought its own late activation, and windows flew back and
//! forth between two stages for 10 s.
//!
//! An activation acts only if it is still true and still the owner's when we decide ([`judge`]):
//!
//! 1. **Older than our switch**: it fired (the main thread's stamp) before the latest switch we
//!    began (a card click, a hotkey, an activation or focus decided, a drop): that switch is
//!    newer than it and already decided where the user is. Our own activations happen inside
//!    our switches (the early focus at their start), so their late echoes fall here too.
//! 2. **Not frontmost**: another app is frontmost now (`NSWorkspace.frontmostApplication`, the
//!    main thread's view, updated before the notice is posted): something newer happened, and
//!    its own notice is on its way.
//!
//! Neither drops an activation of the owner's that is the newest thing that happened: its app stays
//! frontmost until something newer comes, and nothing we began came after it, since every
//! switch we begin from an activation passed rule 2 first (no newer activation had fired
//! then). A ⌘Tab, a Dock click or a click on a window under load still switches, late.

use std::time::{Duration, Instant};

use super::StageId;

/// Why an activation notice starts no switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stale {
    /// It fired `by` before the latest switch we began.
    BeforeOurSwitch {
        /// How long before that switch's start it fired.
        by: Duration,
    },
    /// App `front` is frontmost now, not the activated one.
    NotFrontmost {
        /// The frontmost app's pid.
        front: i32,
    },
}

impl Stale {
    /// The reason, for the log.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::BeforeOurSwitch { .. } => "it fired before a switch we began since",
            Self::NotFrontmost { .. } => "another app is frontmost now",
        }
    }
}

/// Whether app `pid`'s activation, stamped `fired`, is stale now: `our_switch` is when the
/// latest switch we began started, `frontmost` the frontmost app's pid now (`None`: unknown,
/// which never drops one).
pub fn judge(pid: i32, fired: Instant, our_switch: Option<Instant>, frontmost: Option<i32>) -> Option<Stale> {
    if let Some(began) = our_switch
        && fired < began
    {
        return Some(Stale::BeforeOurSwitch { by: began.duration_since(fired) });
    }
    match frontmost {
        Some(front) if front != pid => Some(Stale::NotFrontmost { front }),
        _ => None,
    }
}

/// Whether a window focus that would switch stages is stale: its window is in stage `was`,
/// and its app's focused window now is in another known stage `is` (asked at the decision,
/// since a focus notice carries no stamp). Unknown either way, it acts.
pub fn focus_moved_on(was: Option<StageId>, is: Option<StageId>) -> bool {
    matches!((was, is), (Some(was), Some(is)) if was != is)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn a_focus_whose_app_moved_on_to_another_stage_is_stale() {
        assert!(focus_moved_on(Some(StageId(1)), Some(StageId(2))));
        assert!(!focus_moved_on(Some(StageId(1)), Some(StageId(1))));
        assert!(!focus_moved_on(Some(StageId(1)), None));
        assert!(!focus_moved_on(None, Some(StageId(2))));
    }

    const SAFARI: i32 = 1525;
    const CHROME: i32 = 1528;
    const SLACK: i32 = 1529;
    const FORK: i32 = 1532;
    const CODE: i32 = 18401;

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn a_late_echo_of_our_own_activation_is_dropped() {
        // We switched to Safari's stage at 0 (activating Safari); he clicked Chrome's card at
        // 500 (our switch, Chrome activated); Safari's notice of 100 comes at 2000.
        let t0 = Instant::now();
        let v = judge(SAFARI, ms(t0, 100), Some(ms(t0, 500)), Some(CHROME));
        assert_eq!(v, Some(Stale::BeforeOurSwitch { by: Duration::from_millis(400) }));
    }

    #[test]
    fn a_late_activation_of_another_app_still_frontmost_acts() {
        // Our switch at 0; the owner's ⌘Tab to Chrome at 300 reaches us 2.5 s later: still frontmost.
        let t0 = Instant::now();
        assert_eq!(judge(CHROME, ms(t0, 300), Some(t0), Some(CHROME)), None);
        // Nothing known about the front (a failed read): it acts.
        assert_eq!(judge(CHROME, ms(t0, 300), Some(t0), None), None);
        // No switch of ours yet.
        assert_eq!(judge(CHROME, ms(t0, 300), None, Some(CHROME)), None);
    }

    #[test]
    fn one_superseded_by_a_newer_click_is_dropped() {
        // Chrome activated at 1000; the owner's card click at 1500 began our switch (Slack's stage);
        // Chrome's notice comes at 2500.
        let t0 = Instant::now();
        let v = judge(CHROME, ms(t0, 1000), Some(ms(t0, 1500)), Some(SLACK));
        assert!(matches!(v, Some(Stale::BeforeOurSwitch { .. })));
    }

    #[test]
    fn one_no_longer_frontmost_is_dropped() {
        let t0 = Instant::now();
        assert_eq!(judge(CODE, ms(t0, 2000), Some(t0), Some(CHROME)), Some(Stale::NotFrontmost { front: CHROME }));
    }

    /// A notice: the app, when the main thread stamped it, when the WM thread decided it
    /// (ms since 14:09:50.000).
    struct Notice {
        pid: i32,
        fired: u64,
        decided: u64,
    }

    /// The controller's decisions, replayed: `notices` in the order they reached us, `clicks`
    /// (when a card click began our switch, and the stage's app). The frontmost app at a
    /// decision is the main thread's view then: the latest of the notices stamped by then and
    /// our own activations (each switch of ours activates its stage's app as it begins).
    /// Returns the switches made, as (when, app).
    fn replay(notices: &[Notice], clicks: &[(u64, i32)], stage_of: &HashMap<i32, u32>, gate: bool) -> Vec<(u64, i32)> {
        let t0 = Instant::now();
        let mut fronts: Vec<(u64, i32)> = notices.iter().map(|n| (n.fired, n.pid)).collect();
        let mut events: Vec<(u64, Option<&Notice>, i32)> = notices.iter().map(|n| (n.decided, Some(n), n.pid)).collect();
        events.extend(clicks.iter().map(|&(t, pid)| (t, None, pid)));
        events.sort_by_key(|e| e.0);
        let mut active = None;
        let mut our_switch: Option<u64> = None;
        let mut switches = Vec::new();
        for (t, notice, pid) in events {
            let front = |fronts: &[(u64, i32)]| fronts.iter().filter(|f| f.0 <= t).max_by_key(|f| f.0).map(|f| f.1);
            if let Some(n) = notice {
                if gate && judge(pid, ms(t0, n.fired), our_switch.map(|s| ms(t0, s)), front(&fronts)).is_some() {
                    continue;
                }
                if active == stage_of.get(&pid).copied() {
                    continue;
                }
                switches.push((t, pid));
            }
            // A switch of ours begins: its stage shown, its app activated.
            active = stage_of.get(&pid).copied();
            our_switch = Some(t);
            fronts.push((t, pid));
        }
        switches
    }

    /// A replay's input: the notices, the card clicks (when, the stage's app), each app's stage.
    type Replay = (Vec<Notice>, Vec<(u64, i32)>, HashMap<i32, u32>);

    /// The owner's 11-03 14:09:52–14:10:09 (app[17282]): card clicks for stages 5, 4 and 1, then
    /// every notice as it came (`queued_ms` gives when it fired).
    fn boomerang() -> Replay {
        let stage_of: HashMap<i32, u32> = [(CHROME, 8), (CODE, 5), (SAFARI, 4), (SLACK, 1), (FORK, 6)].into_iter().collect();
        let clicks = vec![(2357, CODE), (4855, SAFARI), (5101, SLACK)];
        let seen: [(i32, u64, u64); 19] = [
            (CODE, 2609, 43),
            (SAFARI, 5332, 395),
            (SLACK, 5677, 470),
            (SAFARI, 7475, 1462),
            (CODE, 8113, 1251),
            (SAFARI, 8504, 409),
            (CHROME, 8827, 701),
            (CODE, 9382, 971),
            (CHROME, 10072, 945),
            (CODE, 11484, 1708),
            (FORK, 11910, 1880),
            (CHROME, 12870, 1475),
            (CODE, 13263, 1438),
            (FORK, 14204, 1498),
            (CODE, 15455, 2460),
            (CHROME, 15714, 2600),
            (CODE, 16354, 2590),
            (CHROME, 18778, 2524),
            (CODE, 19083, 2212),
        ];
        let notices = seen.iter().map(|&(pid, came, queued)| Notice { pid, fired: came - queued, decided: came }).collect();
        (notices, clicks, stage_of)
    }

    #[test]
    fn the_boomerang_of_11_03_replayed_ends_in_at_most_one_switch_after_his_last_click() {
        let (notices, clicks, stage_of) = boomerang();
        let last_click = clicks.last().map(|c| c.0).unwrap_or_default();
        // Without the rule, the replay switches back and forth as the app did.
        let before = replay(&notices, &clicks, &stage_of, false);
        assert!(before.iter().filter(|s| s.0 > last_click).count() >= 10, "{before:?}");
        // With it: at most one switch after the owner's last click.
        let after = replay(&notices, &clicks, &stage_of, true);
        let late: Vec<_> = after.iter().filter(|s| s.0 > last_click).collect();
        assert!(late.len() <= 1, "{late:?}");
    }

    #[test]
    fn his_activation_under_load_still_switches_late() {
        // A card click at 0, then the owner's ⌘Tab to Fork at 3000 that reaches us 2.4 s late with
        // nothing newer: it switches.
        let stage_of: HashMap<i32, u32> = [(CODE, 5), (FORK, 6)].into_iter().collect();
        let notices = [Notice { pid: FORK, fired: 3000, decided: 5400 }];
        let switches = replay(&notices, &[(0, CODE)], &stage_of, true);
        assert_eq!(switches, vec![(5400, FORK)]);
    }
}
