//! New-window rules: which stage a new window joins and where it is placed.
//!
//! Rules come from `config.toml`. They are checked in order and the first match wins; a rule
//! that leaves out its action or placement takes the configured default for it.

use std::fmt;

use regex::Regex;

/// What to do with a new standard window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RuleAction {
    /// Apple's default (`"native"`, the default): a window of an app already in the active
    /// stage joins it; a window of an app whose windows sit in a parked stage joins that
    /// stage and switches to it; a window of an app with no managed windows opens a new
    /// stage and switches to it. See `Model::add_window`.
    #[default]
    Native,
    /// Join the active stage (`"current"`).
    Current,
    /// Open a new stage and switch to it (`"new"`).
    New,
    /// Join the named stage, creating it if missing (`"stage:<name>"`).
    Stage(String),
    /// Pin the window: visible on every stage, never parked (`"float"`).
    Float,
    /// Leave the window unmanaged (`"ignore"`).
    Ignore,
}

impl RuleAction {
    /// Parses `"native" | "current" | "new" | "stage:<name>" | "float" | "ignore"`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let t = text.trim();
        match t {
            "native" => Ok(Self::Native),
            "current" => Ok(Self::Current),
            "new" => Ok(Self::New),
            "float" => Ok(Self::Float),
            "ignore" => Ok(Self::Ignore),
            _ => match t.strip_prefix("stage:") {
                Some(name) if !name.trim().is_empty() => Ok(Self::Stage(name.trim().to_string())),
                Some(_) => Err(format!(
                    "action {text:?}: a stage name is missing after \"stage:\""
                )),
                None => Err(format!(
                    "action {text:?}: expected native, current, new, stage:<name>, float or ignore"
                )),
            },
        }
    }
}

impl fmt::Display for RuleAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native => f.write_str("native"),
            Self::Current => f.write_str("current"),
            Self::New => f.write_str("new"),
            Self::Stage(name) => write!(f, "stage:{name}"),
            Self::Float => f.write_str("float"),
            Self::Ignore => f.write_str("ignore"),
        }
    }
}

/// Where a new window goes on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// Keep the frame the app chose (`"as-is"`, the default).
    #[default]
    AsIs,
    /// Center on the display at the current size (`"center"`).
    Center,
    /// Offset from the frontmost window of the stage (`"cascade"`).
    Cascade,
}

impl Placement {
    /// Parses `"as-is" | "center" | "cascade"`.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "as-is" => Ok(Self::AsIs),
            "center" => Ok(Self::Center),
            "cascade" => Ok(Self::Cascade),
            _ => Err(format!(
                "placement {text:?}: expected as-is, center or cascade"
            )),
        }
    }
}

impl fmt::Display for Placement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AsIs => "as-is",
            Self::Center => "center",
            Self::Cascade => "cascade",
        })
    }
}

/// The outcome of the rules for one window: the stage action and the placement.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Decision {
    /// Which stage the window joins.
    pub action: RuleAction,
    /// Where it goes on screen.
    pub placement: Placement,
    /// With `native`: a newly opened app's window (its app has no managed standard window)
    /// joins the active stage instead of a stage of its own (`[stages] new_apps_join_current`).
    /// The model still sends a window matching a saved one back to its saved stage, and
    /// ignores this while the restore window is open (`Model::add_window`).
    pub new_app_joins_current: bool,
}

/// What Shift did to a new standard window's decision ([`Decision::for_new_window`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftEffect {
    /// Nothing (not held, or not counted for this window).
    None,
    /// `[sugar] shift_brings_here`: the window joins the current stage.
    BringsHere,
    /// With `[stages] new_apps_join_current` on, a newly opened app gets a stage of its own
    /// (Apple's rule, once).
    OwnStage,
}

impl Decision {
    /// The decision for a new standard window after `[stages] new_apps_join_current`
    /// (`join_current`) and Shift (`shift`: held within its window, with
    /// `[sugar] shift_brings_here` on, for an action [`RuleAction::shift_counts`]).
    /// `new_app`: the window's app has no managed standard window yet.
    ///
    /// The key changes only Apple's rule (`native`) for a newly opened app: its window joins
    /// the active stage; Shift then gives it a stage of its own instead (the key inverted).
    /// Every other case is as without the key: Shift brings the window into the current
    /// stage. A rule's or `[defaults]`'s other action wins over the key, as over Shift.
    pub fn for_new_window(mut self, new_app: bool, join_current: bool, shift: bool) -> (Self, ShiftEffect) {
        if join_current && new_app && self.action == RuleAction::Native {
            if shift {
                return (self, ShiftEffect::OwnStage);
            }
            self.new_app_joins_current = true;
            return (self, ShiftEffect::None);
        }
        if shift && self.action.shift_counts() {
            self.action = RuleAction::Current;
            return (self, ShiftEffect::BringsHere);
        }
        (self, ShiftEffect::None)
    }
}

impl RuleAction {
    /// Whether Shift held while the window opens changes this action (`native`, `new`,
    /// `stage:<name>`; never `current`, `float` or `ignore`).
    pub fn shift_counts(&self) -> bool {
        matches!(self, Self::Native | Self::New | Self::Stage(_))
    }
}

/// Which apps a rule applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMatch {
    /// Every app (`app = "*"`).
    Any,
    /// One bundle ID, compared exactly.
    Exact(String),
}

/// One `[[rule]]` entry.
#[derive(Debug, Clone)]
pub struct Rule {
    /// The app the rule applies to.
    pub app: AppMatch,
    /// An optional regex the window title must match (unanchored; use `^…$` to anchor).
    pub title: Option<Regex>,
    /// The action, or `None` for the default action.
    pub action: Option<RuleAction>,
    /// The placement, or `None` for the default placement.
    pub placement: Option<Placement>,
}

impl PartialEq for Rule {
    fn eq(&self, other: &Self) -> bool {
        self.app == other.app
            && self.title.as_ref().map(Regex::as_str) == other.title.as_ref().map(Regex::as_str)
            && self.action == other.action
            && self.placement == other.placement
    }
}

impl Rule {
    /// Builds a rule from config text: `app` is a bundle ID or `"*"`, `title` a regex.
    pub fn parse(
        app: &str,
        title: Option<&str>,
        action: Option<&str>,
        placement: Option<&str>,
    ) -> Result<Self, String> {
        let app = match app.trim() {
            "" => return Err("rule: app is empty (use \"*\" for every app)".to_string()),
            "*" => AppMatch::Any,
            id => AppMatch::Exact(id.to_string()),
        };
        let title = title
            .map(|t| Regex::new(t).map_err(|e| format!("rule title regex {t:?}: {e}")))
            .transpose()?;
        Ok(Self {
            app,
            title,
            action: action.map(RuleAction::parse).transpose()?,
            placement: placement.map(Placement::parse).transpose()?,
        })
    }

    /// True when the rule applies to a window of `bundle_id` titled `title`.
    pub fn matches(&self, bundle_id: &str, title: &str) -> bool {
        let app_ok = match &self.app {
            AppMatch::Any => true,
            AppMatch::Exact(id) => id == bundle_id,
        };
        app_ok && self.title.as_ref().is_none_or(|re| re.is_match(title))
    }
}

/// The ordered rules plus the defaults; the first matching rule wins.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RuleSet {
    /// Rules in config order.
    pub rules: Vec<Rule>,
    /// Used when no rule matches, and for the parts a matching rule leaves out.
    pub defaults: Decision,
}

impl RuleSet {
    /// Builds a rule set.
    pub fn new(rules: Vec<Rule>, defaults: Decision) -> Self {
        Self { rules, defaults }
    }

    /// The decision for a new window of `bundle_id` titled `title`.
    pub fn decide(&self, bundle_id: &str, title: &str) -> Decision {
        match self.rules.iter().find(|r| r.matches(bundle_id, title)) {
            Some(rule) => Decision {
                action: rule
                    .action
                    .clone()
                    .unwrap_or_else(|| self.defaults.action.clone()),
                placement: rule.placement.unwrap_or(self.defaults.placement),
                new_app_joins_current: false,
            },
            None => self.defaults.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(app: &str, title: Option<&str>, action: Option<&str>, placement: Option<&str>) -> Rule {
        Rule::parse(app, title, action, placement).expect("valid rule")
    }

    #[test]
    fn parses_actions() {
        assert_eq!(RuleAction::parse("native"), Ok(RuleAction::Native));
        assert_eq!(RuleAction::default(), RuleAction::Native);
        assert_eq!(RuleAction::Native.to_string(), "native");
        assert_eq!(RuleAction::parse("current"), Ok(RuleAction::Current));
        assert_eq!(RuleAction::parse("new"), Ok(RuleAction::New));
        assert_eq!(RuleAction::parse("float"), Ok(RuleAction::Float));
        assert_eq!(RuleAction::parse("ignore"), Ok(RuleAction::Ignore));
        assert_eq!(
            RuleAction::parse("stage: Chat"),
            Ok(RuleAction::Stage("Chat".to_string()))
        );
        assert!(RuleAction::parse("stage:").is_err());
        assert!(RuleAction::parse("tile").is_err());
        assert_eq!(RuleAction::Stage("Work".into()).to_string(), "stage:Work");
    }

    #[test]
    fn parses_placements() {
        assert_eq!(Placement::parse("as-is"), Ok(Placement::AsIs));
        assert_eq!(Placement::parse("center"), Ok(Placement::Center));
        assert_eq!(Placement::parse("cascade"), Ok(Placement::Cascade));
        assert!(Placement::parse("grid").is_err());
        assert_eq!(Placement::Cascade.to_string(), "cascade");
    }

    #[test]
    fn new_apps_join_current_and_shift() {
        let native = Decision::default();
        let with = |action: RuleAction| Decision { action, ..Decision::default() };
        let joined = Decision { new_app_joins_current: true, ..Decision::default() };
        // The key off: Apple's rule; Shift brings the window here.
        assert_eq!(native.clone().for_new_window(true, false, false), (native.clone(), ShiftEffect::None));
        assert_eq!(native.clone().for_new_window(true, false, true), (with(RuleAction::Current), ShiftEffect::BringsHere));
        // On: a newly opened app joins the current stage; Shift gives it a stage of its own.
        assert_eq!(native.clone().for_new_window(true, true, false), (joined, ShiftEffect::None));
        assert_eq!(native.clone().for_new_window(true, true, true), (native.clone(), ShiftEffect::OwnStage));
        // An app already in a stage: the key changes nothing; Shift still brings it here.
        assert_eq!(native.clone().for_new_window(false, true, false), (native.clone(), ShiftEffect::None));
        assert_eq!(native.clone().for_new_window(false, true, true), (with(RuleAction::Current), ShiftEffect::BringsHere));
        // A rule's (or [defaults]') own action wins over the key, as over Shift where Shift
        // never counted.
        for action in [RuleAction::New, RuleAction::Stage("Chat".into()), RuleAction::Current, RuleAction::Float] {
            assert_eq!(with(action.clone()).for_new_window(true, true, false), (with(action.clone()), ShiftEffect::None), "{action}");
        }
        assert_eq!(with(RuleAction::New).for_new_window(true, true, true), (with(RuleAction::Current), ShiftEffect::BringsHere));
        assert!(!RuleAction::Float.shift_counts() && !RuleAction::Ignore.shift_counts() && !RuleAction::Current.shift_counts());
        assert!(RuleAction::Native.shift_counts() && RuleAction::New.shift_counts() && RuleAction::Stage("x".into()).shift_counts());
    }

    #[test]
    fn first_match_wins() {
        let set = RuleSet::new(
            vec![
                rule("com.example.chat", None, Some("stage:Chat"), None),
                rule("com.example.chat", None, Some("float"), None),
            ],
            Decision::default(),
        );
        assert_eq!(
            set.decide("com.example.chat", "Inbox").action,
            RuleAction::Stage("Chat".into())
        );
    }

    #[test]
    fn no_match_takes_defaults() {
        let defaults = Decision {
            action: RuleAction::New,
            placement: Placement::Center,
            ..Decision::default()
        };
        let set = RuleSet::new(
            vec![rule("com.example.a", None, Some("float"), None)],
            defaults.clone(),
        );
        assert_eq!(set.decide("com.example.b", "x"), defaults);
    }

    #[test]
    fn missing_parts_fall_back_to_defaults() {
        let defaults = Decision {
            action: RuleAction::Current,
            placement: Placement::Cascade,
            ..Decision::default()
        };
        let set = RuleSet::new(
            vec![
                rule("com.example.a", None, None, Some("center")),
                rule("com.example.b", None, Some("new"), None),
            ],
            defaults,
        );
        assert_eq!(
            set.decide("com.example.a", ""),
            Decision {
                action: RuleAction::Current,
                placement: Placement::Center,
                ..Decision::default()
            }
        );
        assert_eq!(
            set.decide("com.example.b", ""),
            Decision {
                action: RuleAction::New,
                placement: Placement::Cascade,
                ..Decision::default()
            }
        );
    }

    #[test]
    fn title_regex_narrows_a_rule() {
        let set = RuleSet::new(
            vec![
                rule("com.example.term", Some("^Scratch"), Some("float"), None),
                rule("*", Some("(?i)picture in picture"), Some("float"), None),
                rule("com.example.term", None, Some("stage:Shell"), None),
            ],
            Decision::default(),
        );
        assert_eq!(
            set.decide("com.example.term", "Scratch pad").action,
            RuleAction::Float
        );
        assert_eq!(
            set.decide("com.example.term", "My Scratch").action,
            RuleAction::Stage("Shell".into())
        );
        assert_eq!(
            set.decide("com.example.video", "Picture in Picture").action,
            RuleAction::Float
        );
        assert_eq!(
            set.decide("com.example.video", "Movie").action,
            RuleAction::Native
        );
    }

    #[test]
    fn bundle_match_is_exact() {
        let set = RuleSet::new(
            vec![rule("com.example.app", None, Some("ignore"), None)],
            Decision::default(),
        );
        assert_eq!(set.decide("com.example.app", "").action, RuleAction::Ignore);
        assert_eq!(
            set.decide("com.example.app.helper", "").action,
            RuleAction::Native
        );
        assert_eq!(
            set.decide("COM.EXAMPLE.APP", "").action,
            RuleAction::Native
        );
    }

    #[test]
    fn bad_rules_are_errors() {
        assert!(Rule::parse("", None, None, None).is_err());
        assert!(Rule::parse("com.x", Some("(unclosed"), None, None).is_err());
        assert!(Rule::parse("com.x", None, Some("nope"), None).is_err());
        assert!(Rule::parse("com.x", None, None, Some("nope")).is_err());
    }

    #[test]
    fn rules_compare_by_regex_text() {
        let a = rule("*", Some("^a$"), None, None);
        let b = rule("*", Some("^a$"), None, None);
        let c = rule("*", Some("^b$"), None, None);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
