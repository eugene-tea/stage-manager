//! Which window engine runs (docs/ARCHITECTURE.md, "Window engines"): decided on every Mac
//! at every launch from the config, the launch probe and the test switch, never from a list of
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
    /// The Accessibility corner only; the probe is not run.
    #[default]
    Accessibility,
    /// The window server's Spaces where the launch probe passes, else the Accessibility
    /// corner (and for the rest of a run whose checks fail).
    Spaces,
}

impl ConfigEngine {
    /// The config's word for it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Accessibility => "accessibility",
            Self::Spaces => "spaces",
        }
    }

    /// The value for the config's `word` (`accessibility` for anything else: the schema
    /// has refused it already).
    pub fn parse(word: &str) -> Self {
        match word.trim() {
            "spaces" => Self::Spaces,
            _ => Self::Accessibility,
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
    /// The live suite's switch (`STAGE_MANAGER_ENGINE`, honoured with `STAGE_MANAGER_ONLY`
    /// only): the engine a test run asks for, ahead of the config.
    pub test: Option<EngineKind>,
}

impl Asked {
    /// True when the launch probe runs: when the config or the test switch asks for the
    /// Spaces engine (the Corner engine never touches the window server's Space calls).
    pub fn probes(&self) -> bool {
        match self.test {
            Some(EngineKind::Corner | EngineKind::Apple) => false,
            Some(EngineKind::Spaces) => true,
            None => self.config == ConfigEngine::Spaces,
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
        None => match asked.config {
            ConfigEngine::Accessibility => return corner("[windows] engine = \"accessibility\"".into(), probe),
            ConfigEngine::Spaces => "[windows] engine = \"spaces\"",
        },
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

    fn asked(config: ConfigEngine, test: Option<EngineKind>) -> Asked {
        Asked { config, test }
    }

    const CONFIGS: [ConfigEngine; 2] = [ConfigEngine::Accessibility, ConfigEngine::Spaces];

    #[test]
    fn the_config_words_parse_and_name_each_other() {
        for c in CONFIGS {
            assert_eq!(ConfigEngine::parse(c.name()), c);
        }
        assert_eq!(ConfigEngine::parse("nonsense"), ConfigEngine::Accessibility);
        assert_eq!(ConfigEngine::parse("auto"), ConfigEngine::Accessibility, "never released: no alias");
        assert_eq!(ConfigEngine::default(), ConfigEngine::Accessibility);
    }

    #[test]
    fn spaces_runs_the_spaces_engine_when_the_probe_passes() {
        let a = asked(ConfigEngine::Spaces, None);
        assert!(a.probes());
        let c = choose(a, passed());
        assert_eq!(c.kind, EngineKind::Spaces);
        assert!(c.why.contains("\"spaces\"") && c.why.contains("passed"), "{}", c.why);
        assert_eq!(c.fallback, None);
    }

    #[test]
    fn spaces_runs_the_corner_engine_when_the_probe_fails_and_says_why() {
        let a = asked(ConfigEngine::Spaces, None);
        let c = choose(a, failed());
        assert_eq!(c.kind, EngineKind::Corner);
        assert!(c.why.contains("the trial window stayed on its Desktop"), "the reason is said: {}", c.why);
        assert_eq!(c.probe, failed(), "the verdict is kept for ctl state");
        assert_eq!(choose(a, None).kind, EngineKind::Corner, "a probe that did not run is no pass");
    }

    #[test]
    fn accessibility_is_the_default_and_runs_no_probe() {
        let a = Asked::default();
        assert_eq!(a, asked(ConfigEngine::Accessibility, None));
        assert!(!a.probes());
        let c = choose(a, None);
        assert_eq!(c.kind, EngineKind::Corner);
        assert!(c.why.contains("accessibility"));
    }

    #[test]
    fn the_test_switch_comes_ahead_of_every_config_value() {
        for config in CONFIGS {
            let spaces = asked(config, Some(EngineKind::Spaces));
            assert!(spaces.probes(), "{config:?}");
            let c = choose(spaces, passed());
            assert_eq!(c.kind, EngineKind::Spaces, "{config:?}");
            assert!(c.why.contains("test switch"), "{}", c.why);
            assert_eq!(choose(spaces, failed()).kind, EngineKind::Corner, "{config:?}: Spaces still needs the probe");
            for kind in [EngineKind::Corner, EngineKind::Apple] {
                let other = asked(config, Some(kind));
                assert!(!other.probes(), "{config:?} {kind:?}");
                let c = choose(other, None);
                assert_eq!(c.kind, EngineKind::Corner, "{config:?} {kind:?}");
                assert!(c.why.contains("test switch"), "{}", c.why);
            }
        }
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
