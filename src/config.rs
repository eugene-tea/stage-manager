//! Configuration: TOML at `~/.config/stage-manager/config.toml`.
//!
//! The file holds only what macOS lacks (rules, placement, hotkeys, named stages, strip
//! extensions). Apple's own Stage Manager settings are read from `com.apple.WindowManager`
//! (see [`crate::engine::settings`]) and never repeated here. The first run writes
//! [`DEFAULT_CONFIG`], which documents every option and parses to [`Config::default`].
//! Unknown keys produce warnings, not errors; a bad file keeps the previous config and
//! returns an error message for the log and the menu.

pub mod hotkey;

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::engine::model::{ModelOptions, StripOptions, StripOrder};
use crate::engine::rules::{Decision, Placement, Rule, RuleAction, RuleSet};
use crate::engine::settings::NativeSettings;

pub use hotkey::{Hotkey, HotkeyCommand, HotkeyConfig, Key, Mods, NumberedHotkey};

/// The file written on first run. Every uncommented value equals the built-in default.
pub const DEFAULT_CONFIG: &str = r#"# Stage Manager configuration.
#
# macOS settings come first: "Show recent apps", "Show windows from an application",
# "Click wallpaper to reveal desktop" and the rest are read live from
# System Settings → Desktop & Dock. This file only adds what macOS lacks.
# Changes are picked up automatically.

[strip]
# "native": Apple's slot-stable order (the stage you leave takes the clicked card's slot).
# "fixed": creation order, matching the hotkey numbers.
order = "native"
# Also show the active stage as a card (fixed order only; Apple never shows it).
show_active = false
# Show each card's hotkey number.
show_numbers = true
# How often thumbnails refresh, in milliseconds (minimum 250).
thumbnail_interval_ms = 1500

[defaults]
# What a new window does when no rule matches:
#   "current"       join the active stage
#   "new"           open a new stage and switch to it
#   "stage:<name>"  join the named stage (created if missing)
#   "float"         pin it: visible on every stage
#   "ignore"        leave it unmanaged
action = "current"
# Where it goes: "as-is", "center", or "cascade" (offset from the front window).
placement = "as-is"

# Rules for new windows, checked in order; the first match wins. `app` is a bundle ID
# (exact) or "*"; `title` is an optional regular expression. Leave out `action` or
# `placement` to use the defaults above.
#
# [[rule]]
# app = "com.example.chat"
# action = "stage:Chat"
#
# [[rule]]
# app = "com.example.terminal"
# title = "^Scratch"
# action = "float"
# placement = "center"
#
# [[rule]]
# app = "*"
# placement = "cascade"

[hotkeys]
# Modifiers: ctrl, alt (option), shift, cmd. "{n}" stands for the digits 1-9.
# An empty string disables a hotkey.
switch = "ctrl+alt+{n}"
send = "ctrl+alt+shift+{n}"
new_stage = "ctrl+alt+n"
toggle_pin = "ctrl+alt+p"
toggle_strip = "ctrl+alt+s"

[stages]
# Named stages created at launch, in this order (they take hotkey numbers 1, 2, ...)
# and stay even when empty. For example:
# names = ["Work", "Chat"]
names = []
"#;

/// Smallest accepted thumbnail interval.
pub const MIN_THUMBNAIL_INTERVAL_MS: u64 = 250;

/// The `[strip]` section: our extensions to Apple's strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripConfig {
    /// Card order.
    pub order: StripOrder,
    /// Show the active stage as a card (fixed order only).
    pub show_active: bool,
    /// Show hotkey numbers on cards.
    pub show_numbers: bool,
    /// Thumbnail refresh interval.
    pub thumbnail_interval_ms: u64,
}

impl Default for StripConfig {
    fn default() -> Self {
        Self {
            order: StripOrder::Native,
            show_active: false,
            show_numbers: true,
            thumbnail_interval_ms: 1500,
        }
    }
}

/// The `[stages]` section.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StagesConfig {
    /// Named stages created at launch, in order.
    pub names: Vec<String>,
}

/// The whole configuration, validated.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    /// Strip extensions.
    pub strip: StripConfig,
    /// New-window rules with the `[defaults]` decision.
    pub rules: RuleSet,
    /// Hotkeys.
    pub hotkeys: HotkeyConfig,
    /// Named stages.
    pub stages: StagesConfig,
}

/// A config problem that stops a file from loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// A parsed config with any warnings (unknown keys, clamped values, hotkey conflicts).
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// The config.
    pub config: Config,
    /// Human-readable warnings.
    pub warnings: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Raw {
    strip: RawStrip,
    defaults: RawDefaults,
    rule: Vec<RawRule>,
    hotkeys: RawHotkeys,
    stages: RawStages,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawStrip {
    order: Option<String>,
    show_active: Option<bool>,
    show_numbers: Option<bool>,
    thumbnail_interval_ms: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawDefaults {
    action: Option<String>,
    placement: Option<String>,
}

#[derive(Deserialize)]
struct RawRule {
    app: String,
    title: Option<String>,
    action: Option<String>,
    placement: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawHotkeys {
    switch: Option<String>,
    send: Option<String>,
    new_stage: Option<String>,
    toggle_pin: Option<String>,
    toggle_strip: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawStages {
    names: Option<Vec<String>>,
}

const KNOWN: &[(&str, &[&str])] = &[
    (
        "strip",
        &[
            "order",
            "show_active",
            "show_numbers",
            "thumbnail_interval_ms",
        ],
    ),
    ("defaults", &["action", "placement"]),
    ("rule", &["app", "title", "action", "placement"]),
    (
        "hotkeys",
        &["switch", "send", "new_stage", "toggle_pin", "toggle_strip"],
    ),
    ("stages", &["names"]),
];

fn unknown_keys(table: &toml::Table) -> Vec<String> {
    let mut out = Vec::new();
    for (section, value) in table {
        let Some((_, keys)) = KNOWN.iter().find(|(s, _)| s == section) else {
            out.push(format!("unknown section [{section}] ignored"));
            continue;
        };
        let tables: Vec<&toml::Table> = match value {
            toml::Value::Table(t) => vec![t],
            toml::Value::Array(items) => items.iter().filter_map(toml::Value::as_table).collect(),
            _ => Vec::new(),
        };
        for t in tables {
            for key in t.keys() {
                if !keys.contains(&key.as_str()) {
                    out.push(format!("unknown key {key:?} in [{section}] ignored"));
                }
            }
        }
    }
    out
}

impl Config {
    /// Parses and validates config text.
    pub fn parse(text: &str) -> Result<Parsed, ConfigError> {
        let table: toml::Table =
            toml::from_str(text).map_err(|e| ConfigError(format!("config.toml: {e}")))?;
        let mut warnings = unknown_keys(&table);
        let raw: Raw =
            toml::from_str(text).map_err(|e| ConfigError(format!("config.toml: {e}")))?;
        let err = |e: String| ConfigError(format!("config.toml: {e}"));

        let defaults_strip = StripConfig::default();
        let order = match raw.strip.order.as_deref() {
            None | Some("native") => StripOrder::Native,
            Some("fixed") => StripOrder::Fixed,
            Some(other) => {
                return Err(err(format!(
                    "[strip] order {other:?}: expected native or fixed"
                )));
            }
        };
        let mut interval = raw
            .strip
            .thumbnail_interval_ms
            .unwrap_or(defaults_strip.thumbnail_interval_ms);
        if interval < MIN_THUMBNAIL_INTERVAL_MS {
            warnings.push(format!(
                "[strip] thumbnail_interval_ms {interval} is below {MIN_THUMBNAIL_INTERVAL_MS}; using {MIN_THUMBNAIL_INTERVAL_MS}"
            ));
            interval = MIN_THUMBNAIL_INTERVAL_MS;
        }
        let strip = StripConfig {
            order,
            show_active: raw.strip.show_active.unwrap_or(defaults_strip.show_active),
            show_numbers: raw
                .strip
                .show_numbers
                .unwrap_or(defaults_strip.show_numbers),
            thumbnail_interval_ms: interval,
        };

        let defaults = Decision {
            action: raw
                .defaults
                .action
                .as_deref()
                .map(RuleAction::parse)
                .transpose()
                .map_err(err)?
                .unwrap_or_default(),
            placement: raw
                .defaults
                .placement
                .as_deref()
                .map(Placement::parse)
                .transpose()
                .map_err(err)?
                .unwrap_or_default(),
        };
        let rules = raw
            .rule
            .iter()
            .enumerate()
            .map(|(i, r)| {
                Rule::parse(
                    &r.app,
                    r.title.as_deref(),
                    r.action.as_deref(),
                    r.placement.as_deref(),
                )
                .map_err(|e| err(format!("[[rule]] #{}: {e}", i + 1)))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let base = HotkeyConfig::default();
        let numbered = |text: Option<&String>, fallback| match text {
            Some(t) => hotkey::parse_numbered(t).map_err(err),
            None => Ok(fallback),
        };
        let fixed = |text: Option<&String>, fallback| match text {
            Some(t) => hotkey::parse_hotkey(t).map_err(err),
            None => Ok(fallback),
        };
        let hotkeys = HotkeyConfig {
            switch: numbered(raw.hotkeys.switch.as_ref(), base.switch)?,
            send: numbered(raw.hotkeys.send.as_ref(), base.send)?,
            new_stage: fixed(raw.hotkeys.new_stage.as_ref(), base.new_stage)?,
            toggle_pin: fixed(raw.hotkeys.toggle_pin.as_ref(), base.toggle_pin)?,
            toggle_strip: fixed(raw.hotkeys.toggle_strip.as_ref(), base.toggle_strip)?,
        };
        warnings.extend(hotkeys.conflicts());

        let mut names: Vec<String> = Vec::new();
        for n in raw.stages.names.unwrap_or_default() {
            let n = n.trim().to_string();
            if n.is_empty() {
                warnings.push("[stages] an empty name was ignored".to_string());
            } else if names.iter().any(|m| m.eq_ignore_ascii_case(&n)) {
                warnings.push(format!("[stages] duplicate name {n:?} ignored"));
            } else {
                names.push(n);
            }
        }

        Ok(Parsed {
            config: Config {
                strip,
                rules: RuleSet::new(rules, defaults),
                hotkeys,
                stages: StagesConfig { names },
            },
            warnings,
        })
    }

    /// Strip options for the model; `capacity` is computed by the platform from the screen.
    pub fn strip_options(&self, capacity: usize) -> StripOptions {
        StripOptions {
            order: self.strip.order,
            show_active: self.strip.show_active,
            capacity,
        }
    }

    /// Model options from this config plus Apple's settings and the strip capacity.
    pub fn model_options(&self, settings: NativeSettings, capacity: usize) -> ModelOptions {
        ModelOptions {
            settings,
            strip: self.strip_options(capacity),
            named_stages: self.stages.names.clone(),
        }
    }
}

/// The environment variable that moves every file of the app (config, state, ctl socket,
/// log) into one directory, so test runs never touch the real ones.
pub const HOME_ENV: &str = "STAGE_MANAGER_HOME";

/// `$STAGE_MANAGER_HOME` when set (and not empty), else `~/.config/stage-manager`.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(HOME_ENV).filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("stage-manager"))
}

/// `config.toml` in [`config_dir`].
pub fn default_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// `state.json` in [`config_dir`].
pub fn state_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("state.json"))
}

/// Result of [`load_or_create`].
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    /// The config.
    pub config: Config,
    /// Warnings from parsing.
    pub warnings: Vec<String>,
    /// True when the default file was written by this call.
    pub created: bool,
}

/// Loads `path`, writing [`DEFAULT_CONFIG`] there first (and its directory) if it is missing.
pub fn load_or_create(path: &Path) -> Result<Loaded, ConfigError> {
    let mut created = false;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| ConfigError(format!("cannot create {}: {e}", dir.display())))?;
        }
        std::fs::write(path, DEFAULT_CONFIG)
            .map_err(|e| ConfigError(format!("cannot write {}: {e}", path.display())))?;
        created = true;
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError(format!("cannot read {}: {e}", path.display())))?;
    let Parsed { config, warnings } = Config::parse(&text)?;
    Ok(Loaded {
        config,
        warnings,
        created,
    })
}

/// The config in force plus where it came from. A failed reload keeps the previous config.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
    config: Config,
}

impl ConfigStore {
    /// Opens `path`, creating the default file if missing. A bad file yields the built-in
    /// defaults plus the error. Warnings come back for logging.
    pub fn open(path: PathBuf) -> (Self, Result<Vec<String>, ConfigError>) {
        match load_or_create(&path) {
            Ok(l) => (
                Self {
                    path,
                    config: l.config,
                },
                Ok(l.warnings),
            ),
            Err(e) => (
                Self {
                    path,
                    config: Config::default(),
                },
                Err(e),
            ),
        }
    }

    /// Re-reads the file. On error the previous config stays in force and the error returns.
    pub fn reload(&mut self) -> Result<Vec<String>, ConfigError> {
        let text = std::fs::read_to_string(&self.path)
            .map_err(|e| ConfigError(format!("cannot read {}: {e}", self.path.display())))?;
        let Parsed { config, warnings } = Config::parse(&text)?;
        self.config = config;
        Ok(warnings)
    }

    /// The config in force.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::rules::AppMatch;

    fn parse(text: &str) -> Parsed {
        Config::parse(text).expect("parses")
    }

    #[test]
    fn default_file_parses_to_the_defaults_without_warnings() {
        let p = parse(DEFAULT_CONFIG);
        assert_eq!(p.config, Config::default());
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }

    #[test]
    fn default_file_examples_parse_when_uncommented() {
        let uncommented: String = DEFAULT_CONFIG
            .lines()
            .filter(|l| *l != "names = []")
            .map(|l| match l.strip_prefix("# ") {
                Some(rest) if rest.starts_with("[[rule]]") || rest.contains(" = ") => rest,
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let p = parse(&uncommented);
        assert_eq!(p.config.rules.rules.len(), 3);
        assert_eq!(p.config.stages.names, ["Work", "Chat"]);
    }

    #[test]
    fn readme_shows_the_default_file_verbatim() {
        let readme = include_str!("../README.md");
        assert!(
            readme.contains(&format!("```toml\n{DEFAULT_CONFIG}```")),
            "README.md's config reference is out of date: paste DEFAULT_CONFIG into it"
        );
    }

    #[test]
    fn empty_file_is_all_defaults() {
        assert_eq!(parse("").config, Config::default());
    }

    #[test]
    fn full_config() {
        let p = parse(
            r#"
[strip]
order = "fixed"
show_active = true
show_numbers = false
thumbnail_interval_ms = 2000

[defaults]
action = "new"
placement = "cascade"

[[rule]]
app = "com.example.chat"
action = "stage:Chat"

[[rule]]
app = "*"
title = "(?i)picture in picture"
action = "float"
placement = "center"

[hotkeys]
switch = "cmd+alt+{n}"
toggle_strip = ""

[stages]
names = ["Work", "Chat", "work", " "]
"#,
        );
        let c = p.config;
        assert_eq!(
            c.strip,
            StripConfig {
                order: StripOrder::Fixed,
                show_active: true,
                show_numbers: false,
                thumbnail_interval_ms: 2000
            }
        );
        assert_eq!(
            c.rules.defaults,
            Decision {
                action: RuleAction::New,
                placement: Placement::Cascade
            }
        );
        assert_eq!(c.rules.rules.len(), 2);
        assert_eq!(c.rules.rules[1].app, AppMatch::Any);
        assert_eq!(
            c.rules.decide("com.example.chat", "x").action,
            RuleAction::Stage("Chat".into())
        );
        assert_eq!(
            c.rules.decide("com.example.chat", "x").placement,
            Placement::Cascade
        );
        assert_eq!(
            c.rules
                .decide("com.example.video", "Picture in Picture")
                .action,
            RuleAction::Float
        );
        assert!(
            c.hotkeys
                .switch
                .is_some_and(|s| s.mods.cmd && s.mods.alt && !s.mods.ctrl)
        );
        assert_eq!(c.hotkeys.toggle_strip, None);
        assert_eq!(
            c.hotkeys.new_stage,
            HotkeyConfig::default().new_stage,
            "unset keeps default"
        );
        assert_eq!(c.stages.names, ["Work", "Chat"]);
        assert_eq!(p.warnings.len(), 2, "{:?}", p.warnings);
    }

    #[test]
    fn unknown_keys_warn_and_do_not_fail() {
        let p = parse(
            r#"
[strip]
order = "native"
position = "left"

[[rule]]
app = "com.example.a"
when = "always"

[gestures]
swipe = true
"#,
        );
        assert_eq!(p.config.strip, StripConfig::default());
        assert_eq!(p.warnings.len(), 3, "{:?}", p.warnings);
        assert!(p.warnings.iter().any(|w| w.contains("position")));
        assert!(p.warnings.iter().any(|w| w.contains("[gestures]")));
    }

    #[test]
    fn bad_values_are_errors() {
        for text in [
            "[strip]\norder = \"recent\"",
            "[strip]\nshow_numbers = \"yes\"",
            "[defaults]\naction = \"tile\"",
            "[defaults]\nplacement = \"grid\"",
            "[[rule]]\ntitle = \"x\"",
            "[[rule]]\napp = \"a\"\ntitle = \"(\"",
            "[hotkeys]\nswitch = \"ctrl+alt+s\"",
            "[hotkeys]\nnew_stage = \"ctrl+alt+{n}\"",
            "[hotkeys]\ntoggle_pin = \"p\"",
            "strip = 5",
            "not toml at all [",
        ] {
            assert!(Config::parse(text).is_err(), "should fail: {text}");
        }
    }

    #[test]
    fn low_thumbnail_interval_is_clamped_with_a_warning() {
        let p = parse("[strip]\nthumbnail_interval_ms = 10");
        assert_eq!(
            p.config.strip.thumbnail_interval_ms,
            MIN_THUMBNAIL_INTERVAL_MS
        );
        assert_eq!(p.warnings.len(), 1);
    }

    #[test]
    fn hotkey_conflicts_warn() {
        let p = parse("[hotkeys]\ntoggle_pin = \"ctrl+alt+n\"");
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
    }

    #[test]
    fn model_options_carry_config_and_settings() {
        let p = parse("[strip]\norder = \"fixed\"\n[stages]\nnames = [\"Work\"]");
        let o = p.config.model_options(NativeSettings::default(), 5);
        assert_eq!(
            o.strip,
            StripOptions {
                order: StripOrder::Fixed,
                show_active: false,
                capacity: 5
            }
        );
        assert_eq!(o.named_stages, ["Work"]);
    }

    #[test]
    fn first_run_writes_the_default_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("config.toml");
        let l = load_or_create(&path).expect("loads");
        assert!(l.created);
        assert_eq!(
            std::fs::read_to_string(&path).expect("written"),
            DEFAULT_CONFIG
        );
        assert_eq!(l.config, Config::default());
        let again = load_or_create(&path).expect("loads");
        assert!(!again.created);
    }

    #[test]
    fn a_bad_reload_keeps_the_previous_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[strip]\norder = \"fixed\"").expect("write");
        let (mut store, first) = ConfigStore::open(path.clone());
        assert_eq!(first, Ok(vec![]));
        assert_eq!(store.config().strip.order, StripOrder::Fixed);
        std::fs::write(&path, "[strip]\norder = \"sideways\"").expect("write");
        let e = store.reload().expect_err("bad file");
        assert!(e.to_string().contains("sideways"));
        assert_eq!(store.config().strip.order, StripOrder::Fixed);
        std::fs::write(&path, "[strip]\norder = \"native\"\nbogus = 1").expect("write");
        assert_eq!(store.reload().expect("good").len(), 1);
        assert_eq!(store.config().strip.order, StripOrder::Native);
        assert_eq!(store.path(), path.as_path());
    }

    #[test]
    fn a_bad_file_at_launch_falls_back_to_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[[rule]]\napp = 3").expect("write");
        let (store, result) = ConfigStore::open(path);
        assert!(result.is_err());
        assert_eq!(store.config(), &Config::default());
    }
}
