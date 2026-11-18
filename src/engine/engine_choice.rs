//! Which window engine runs (docs/ARCHITECTURE.md, "Window engines"): decided on every Mac
//! at every launch from the config, the launch probe and two switches, never from a list of
//! macOS versions; and the checks while running that end a run of the Spaces engine early.
//! Pure: the platform runs the probe ([`crate::platform::spaces::probe`]) and installs the
//! engine ([`crate::platform::window_engine::choose_at_launch`]).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::recovery::EngineKind;

/// `[windows] engine`: which engines the app may run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConfigEngine {
    /// The window server's Spaces where the launch probe says they work, else the
    /// Accessibility corner.
    #[default]
    Auto,
    /// The Accessibility corner only; the probe is not run.
    Accessibility,
}

impl ConfigEngine {
    /// The config's word for it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Accessibility => "accessibility",
        }
    }
}

/// What the launch probe found: whether a trial Space with a window of the app's own worked,
/// why not when not, and how long it took.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProbeVerdict {
    /// Every step read back as asked.
    pub ok: bool,
    /// The step that failed, or "a trial Space with an own window works".
    pub reason: String,
    /// The probe's time, start to end, in milliseconds.
    pub ms: f64,
    /// Each step's milliseconds ("calls 2.1, desktops 0.3, panel 12.0, ..."), for the log.
    pub steps: String,
}

/// What was asked of the launch, besides the probe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Asked {
    /// `[windows] engine`.
    pub config: ConfigEngine,
    /// The hidden opt-in (`STAGE_MANAGER_SPACES=1`): until the Spaces engine is measured
    /// (1.9, S6), `auto` runs it only with this set.
    pub opted_in: bool,
    /// The live suite's switch (`STAGE_MANAGER_ENGINE`, honoured with `STAGE_MANAGER_ONLY`
    /// only): the engine a test run asks for, ahead of the config and the opt-in.
    pub test: Option<EngineKind>,
}

impl Asked {
    /// True when the launch probe runs: always, unless the config or the test switch asks
    /// for the Corner engine (the way out never touches the window server's Space calls).
    pub fn probes(&self) -> bool {
        match self.test {
            Some(EngineKind::Corner | EngineKind::Apple) => false,
            Some(EngineKind::Spaces) => true,
            None => self.config == ConfigEngine::Auto,
        }
    }
}

/// The engine a launch runs and why, for the log and `ctl state`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Choice {
    /// The engine.
    pub kind: EngineKind,
    /// Why, in a few words.
    pub why: String,
    /// The launch probe's verdict (`None`: not run).
    pub probe: Option<ProbeVerdict>,
    /// Set once a check while running fell back to the Corner engine: why.
    pub fallback: Option<String>,
}

/// The engine for what was `asked` and what the probe found (`None` when it was not run).
pub fn choose(asked: Asked, probe: Option<ProbeVerdict>) -> Choice {
    let corner = |why: String, probe: Option<ProbeVerdict>| Choice { kind: EngineKind::Corner, why, probe, fallback: None };
    let wants_spaces = match asked.test {
        // The Apple mode is no launch's choice (it follows Apple's Stage Manager while it is on).
        Some(EngineKind::Corner | EngineKind::Apple) => return corner("the test switch asks for it".into(), probe),
        Some(EngineKind::Spaces) => "the test switch asks for it",
        None if asked.config == ConfigEngine::Accessibility => return corner("[windows] engine = \"accessibility\"".into(), probe),
        None if !asked.opted_in => {
            return corner("auto: the Spaces engine runs only with STAGE_MANAGER_SPACES=1 until it is measured".into(), probe);
        }
        None => "auto, STAGE_MANAGER_SPACES=1",
    };
    match probe {
        Some(p) if p.ok => Choice { kind: EngineKind::Spaces, why: format!("{wants_spaces}; the launch probe passed"), probe: Some(p), fallback: None },
        Some(p) => corner(format!("the launch probe failed: {}", p.reason), Some(p)),
        None => corner("the launch probe did not run".into(), None),
    }
}

/// How many windows not where a Space call put them, each set right by the call's one retry,
/// end a run of the Spaces engine within [`STRIKE_WINDOW`]. Measured: no miss in the spike's
/// 80 rounds, the engine probe's 20 rounds of 3 windows, or the live suite's Spaces passes
/// (11-14), so one is already unusual; three in a minute is a window server that refuses
/// often enough for a switch to show a stray window, and the Corner engine does better then.
pub const STRIKES: usize = 3;

/// The time [`STRIKES`] must fall within: a minute holds a few dozen switches at a fast
/// pace, and a miss from a moment of window-server load an hour ago says nothing now.
pub const STRIKE_WINDOW: Duration = Duration::from_secs(60);

/// The Spaces engine's health while running: misses a retry set right, in a sliding window
/// of time, and the one miss a retry did not.
#[derive(Debug, Clone, Default)]
pub struct Health {
    strikes: VecDeque<Instant>,
    failed: Option<String>,
}

impl Health {
    /// A window was not where the call put it, and the call's retry put it there: true when
    /// that makes [`STRIKES`] within [`STRIKE_WINDOW`] (the engine has failed from then on).
    pub fn strike(&mut self, now: Instant, what: &str) -> bool {
        while self.strikes.front().is_some_and(|t| now.duration_since(*t) >= STRIKE_WINDOW) {
            self.strikes.pop_front();
        }
        self.strikes.push_back(now);
        if self.strikes.len() >= STRIKES && self.failed.is_none() {
            self.failed = Some(format!("{STRIKES} windows not where a Space call put them within {} s (the last: {what})", STRIKE_WINDOW.as_secs()));
        }
        self.failed.is_some()
    }

    /// A window was not where the call put it after the call's retry: the engine has failed.
    pub fn fail(&mut self, why: String) {
        if self.failed.is_none() {
            self.failed = Some(why);
        }
    }

    /// Why the engine has failed, once it has.
    pub fn failed(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    /// The misses a retry set right within the window, as of `now`.
    pub fn strikes(&self, now: Instant) -> usize {
        self.strikes.iter().filter(|t| now.duration_since(**t) < STRIKE_WINDOW).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passed() -> Option<ProbeVerdict> {
        Some(ProbeVerdict { ok: true, reason: "a trial Space with an own window works".into(), ms: 12.0, steps: String::new() })
    }

    fn failed() -> Option<ProbeVerdict> {
        Some(ProbeVerdict { ok: false, reason: "the trial window stayed on its Desktop".into(), ms: 60.0, steps: String::new() })
    }

    #[test]
    fn auto_runs_the_corner_engine_until_the_opt_in_and_still_probes() {
        let asked = Asked::default();
        assert!(asked.probes(), "the verdict is logged at every launch");
        let c = choose(asked, passed());
        assert_eq!(c.kind, EngineKind::Corner);
        assert!(c.why.contains("STAGE_MANAGER_SPACES"), "{}", c.why);
        assert_eq!(c.probe, passed(), "the verdict is kept for ctl state");
    }

    #[test]
    fn auto_with_the_opt_in_runs_spaces_only_when_the_probe_passes() {
        let asked = Asked { opted_in: true, ..Asked::default() };
        assert_eq!(choose(asked, passed()).kind, EngineKind::Spaces);
        let c = choose(asked, failed());
        assert_eq!(c.kind, EngineKind::Corner);
        assert!(c.why.contains("the trial window stayed on its Desktop"), "the reason is said: {}", c.why);
        assert_eq!(choose(asked, None).kind, EngineKind::Corner);
    }

    #[test]
    fn accessibility_is_the_way_out_and_runs_no_probe() {
        let asked = Asked { config: ConfigEngine::Accessibility, opted_in: true, test: None };
        assert!(!asked.probes());
        let c = choose(asked, None);
        assert_eq!(c.kind, EngineKind::Corner);
        assert!(c.why.contains("accessibility"));
    }

    #[test]
    fn the_test_switch_comes_first_but_spaces_still_needs_the_probe() {
        let spaces = Asked { config: ConfigEngine::Accessibility, opted_in: false, test: Some(EngineKind::Spaces) };
        assert!(spaces.probes());
        assert_eq!(choose(spaces, passed()).kind, EngineKind::Spaces);
        assert_eq!(choose(spaces, failed()).kind, EngineKind::Corner);
        let corner = Asked { opted_in: true, test: Some(EngineKind::Corner), ..Asked::default() };
        assert!(!corner.probes());
        assert_eq!(choose(corner, None).kind, EngineKind::Corner);
    }

    #[test]
    fn strikes_fail_the_engine_only_close_together() {
        let t = Instant::now();
        let mut h = Health::default();
        assert!(!h.strike(t, "a"));
        assert!(!h.strike(t + Duration::from_secs(30), "b"));
        assert!(!h.strike(t + Duration::from_secs(61), "c"), "the first is out of the window");
        assert_eq!(h.strikes(t + Duration::from_secs(61)), 2);
        assert!(h.strike(t + Duration::from_secs(62), "d"));
        assert!(h.failed().is_some_and(|w| w.contains("the last: d")));
    }

    #[test]
    fn one_miss_a_retry_does_not_set_right_fails_the_engine_and_the_first_reason_stays() {
        let mut h = Health::default();
        h.fail("first".into());
        h.fail("second".into());
        assert_eq!(h.failed(), Some("first"));
    }
}
