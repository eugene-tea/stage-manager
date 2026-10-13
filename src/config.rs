//! Configuration: TOML at `~/.config/stage-manager/config.toml`.
//!
//! Out of the box the app looks and behaves like Apple's Stage Manager; every extension is
//! off until the file turns it on. Every key, with its type, range, default and a one-line
//! explanation, lives in [`schema::KEYS`]; the built-in defaults live in code, and the file
//! written on first run ([`schema::default_file`]) holds no live values (every key is
//! commented out), so new defaults reach existing users. Apple's own Stage Manager settings
//! are read from `com.apple.WindowManager` (see [`crate::engine::settings`]) and never
//! repeated here.
//!
//! Unknown keys and out-of-range numbers produce warnings (numbers are clamped); a value of
//! the wrong type is an error, and a bad file keeps the previous config.

pub mod hotkey;
pub mod schema;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::engine::model::{ModelOptions, StripOptions, StripOrder};
use crate::engine::rules::{Decision, Placement, Rule, RuleAction, RuleSet};
use crate::engine::settings::NativeSettings;

pub use hotkey::{Hotkey, HotkeyCommand, HotkeyConfig, Key, Mods, NumberedHotkey};
use schema::{KEYS, Kind, RETIRED, RULE_KEYS, SECTIONS};

/// The file written on first run: every key commented out with its default.
pub fn default_file() -> String {
    schema::default_file()
}

/// A number, or "auto" (computed from the display).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Auto {
    /// Computed from the display.
    Auto,
    /// This value.
    Value(f64),
}

impl Auto {
    /// The value, or `auto` when automatic.
    pub fn or(self, auto: f64) -> f64 {
        match self {
            Auto::Auto => auto,
            Auto::Value(v) => v,
        }
    }
}

/// An sRGB color with alpha, each channel 0..=1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    /// Parses `#rrggbb` or `#rrggbbaa`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let hex = text.strip_prefix('#').ok_or_else(|| format!("{text:?} is not a #rrggbb color"))?;
        if !(hex.len() == 6 || hex.len() == 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("{text:?} is not a #rrggbb or #rrggbbaa color"));
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map(|b| f64::from(b) / 255.0).unwrap_or(0.0);
        Ok(Self { r: byte(0), g: byte(2), b: byte(4), a: if hex.len() == 8 { byte(6) } else { 1.0 } })
    }
}

/// How the strip's card count is chosen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapacityRule {
    /// A fixed count; 0 counts from the visible height.
    pub fixed: usize,
    /// Room per card.
    pub pitch: f64,
    /// Room kept free above and below.
    pub margin: f64,
    /// Bounds of the counted value.
    pub min: usize,
    pub max: usize,
}

impl CapacityRule {
    /// The card count for a visible height, with strip lengths multiplied by `k`.
    pub fn for_height(&self, visible_height: f64, k: f64) -> usize {
        if self.fixed > 0 {
            return self.fixed;
        }
        let (lo, hi) = (self.min.min(self.max), self.max.max(self.min));
        let n = ((visible_height - 2.0 * self.margin * k) / (self.pitch * k)).floor();
        if !n.is_finite() || n < lo as f64 { lo } else { (n as usize).min(hi) }
    }
}

/// The `[strip]` keys the engine uses.
#[derive(Debug, Clone, PartialEq)]
pub struct StripConfig {
    /// Card order.
    pub order: StripOrder,
    /// Show the active stage as a card (fixed order only).
    pub show_active: bool,
    /// Hide the strip while a wallpaper click shows the desktop (an option; Apple keeps it
    /// shown, as the way back to a stage). It comes back on any window activation.
    pub hide_while_desktop_shown: bool,
    /// How many cards show.
    pub capacity: CapacityRule,
    /// Thumbnail refresh interval.
    pub thumbnail_interval_ms: u64,
    /// Capture window pictures (thumbnails, switch snapshots).
    pub capture: bool,
}

/// `[cards]`: the look of each card.
#[derive(Debug, Clone, PartialEq)]
pub struct CardStyle {
    pub x: f64,
    pub full_height: f64,
    pub min_height: f64,
    pub max_width: f64,
    pub gap: f64,
    pub tilt: Auto,
    pub perspective: Auto,
    pub perspective_x: f64,
    pub perspective_y: f64,
    pub corner_radius: f64,
    pub border_width: f64,
    pub border_color: Rgba,
    pub shadow_radius: f64,
    pub shadow_opacity: f64,
    pub shadow_offset_x: f64,
    pub shadow_offset_y: f64,
    pub shadow_color: Rgba,
    pub max_windows: usize,
    pub stack_dx: f64,
    pub behind_scale: f64,
    pub second_app_dx: f64,
    pub second_app_dy: f64,
    pub hover_scale: f64,
    pub hover_ms: u64,
    /// A pressed card's size, of its resting one.
    pub press_scale: f64,
    pub press_ms: u64,
    pub fade_ms: u64,
}

/// `[icons]`.
#[derive(Debug, Clone, PartialEq)]
pub struct IconStyle {
    pub size: f64,
    pub center_dx: f64,
    pub center_dy: f64,
    pub step: f64,
    pub max: usize,
    pub shadow_radius: f64,
    pub shadow_opacity: f64,
    pub shadow_offset_y: f64,
}

/// `[motion]`.
#[derive(Debug, Clone, PartialEq)]
pub struct MotionStyle {
    /// The cards' glide.
    pub spring_stiffness: f64,
    pub spring_damping: f64,
    pub spring_mass: f64,
    /// The strip's slide.
    pub slide_stiffness: f64,
    pub slide_damping: f64,
    pub slide_mass: f64,
    pub slide_distance: f64,
}

/// `[reveal]`.
#[derive(Debug, Clone, PartialEq)]
pub struct RevealStyle {
    pub edge_width: f64,
    pub delay_ms: u64,
    pub hide_delay_ms: u64,
    pub poll_ms: u64,
    pub edge_tab: bool,
    pub launch_reveal: bool,
    pub launch_reveal_ms: u64,
}

/// `[drag]`.
#[derive(Debug, Clone, PartialEq)]
pub struct DragStyle {
    pub threshold: f64,
    /// The middle fraction of a card's height that takes a dropped card (a merge).
    pub merge_zone: f64,
    pub grow_distance: f64,
    pub lifted_opacity: f64,
    pub ghost_opacity: f64,
    /// A card dropped onto another shrinks into it and fades this long.
    pub merge_ms: u64,
}

/// `[animation]`: switches drawn as moving snapshots.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationStyle {
    pub enabled: bool,
    /// Multiplies the speed of every animation; 0 = none.
    pub speed: f64,
    pub spring_stiffness: f64,
    pub spring_damping: f64,
    pub spring_mass: f64,
    pub unpark_at: f64,
    pub cover_ms: u64,
    pub corner_radius: f64,
    pub shadow_radius: f64,
    pub shadow_opacity: f64,
    pub shadow_offset_y: f64,
    pub snapshot_interval_ms: u64,
    pub snapshot_cache_mb: u64,
    /// An activation of an app in another stage (Cmd-Tab, Dock, a click on its sliver) grows
    /// its windows from their card like a card click; off (the default), they come to their
    /// frames at once, since macOS has already raised them in the park corner.
    pub activation_grows: bool,
}

/// `[strip] level`: where the strip sits among windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StripLevel {
    /// Apple's: below normal windows, so a window moved over it covers it.
    #[default]
    BelowWindows,
    /// Over every normal window.
    AboveWindows,
}

/// Everything the strip panel draws with; changes apply live.
#[derive(Debug, Clone, PartialEq)]
pub struct StripStyle {
    /// Below or above windows.
    pub level: StripLevel,
    /// Cards can be dragged to another slot.
    pub reorder: bool,
    /// A card dropped onto another card merges the two stages.
    pub merge_on_drop: bool,
    /// Column width.
    pub width: f64,
    /// Multiplies every strip length.
    pub scale: f64,
    /// Also shrink by display area.
    pub scale_to_display: bool,
    /// Number badges.
    pub show_numbers: bool,
    /// Stage names.
    pub show_names: bool,
    /// Thumbnail capture resolution (multiple of the drawn size).
    pub thumbnail_resolution: f64,
    pub cards: CardStyle,
    pub icons: IconStyle,
    pub motion: MotionStyle,
    pub reveal: RevealStyle,
    pub drag: DragStyle,
    pub animation: AnimationStyle,
}

impl Default for StripStyle {
    fn default() -> Self {
        Config::default().style
    }
}

/// The slowest `[animation] speed` other than 0 (none).
pub const MIN_SPEED: f64 = 0.25;

impl AnimationStyle {
    /// `[animation] speed` as applied: 0 when `enabled = false` or `speed = 0` (nothing
    /// animates), else at least [`MIN_SPEED`].
    pub fn speed(&self) -> f64 {
        if !self.enabled || self.speed <= 0.0 { 0.0 } else { self.speed.max(MIN_SPEED) }
    }

    /// True when anything animates.
    pub fn animates(&self) -> bool {
        self.speed() > 0.0
    }

    /// A duration of `ms` milliseconds at this speed, in seconds (0 when nothing animates).
    pub fn seconds(&self, ms: u64) -> f64 {
        let speed = self.speed();
        if speed <= 0.0 { 0.0 } else { ms as f64 / 1000.0 / speed }
    }
}

/// `[windows] fit`: keeping placed windows clear of the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FitMode {
    /// Apple's: windows keep their exact frames.
    #[default]
    Off,
    /// Move right just enough.
    Move,
    /// Move, and narrow a window too wide for the room.
    Narrow,
}

/// `[windows]`.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowsConfig {
    pub fit: FitMode,
    pub fit_margin: f64,
    pub minimize_to_strip: bool,
    pub focus_echo_ms: u64,
    /// A press on a parked window's sliver does nothing (Apple's parked windows cannot be
    /// grabbed); off, it reaches the app and its stage comes on release.
    pub inert_slivers: bool,
    /// Points of a parked window asked to stay inside the corner's side.
    pub park_inside: f64,
    /// How far below the display's bottom a parked window's top is asked.
    pub park_drop: f64,
    /// Apps whose every window sits in inactive stages are hidden instead of parked in view
    /// (a prototype).
    pub hide_parked_apps: bool,
    /// A live picture of the desktop over the parked windows' slivers (a prototype).
    pub corner_patch: bool,
    /// How far past a sliver the patch reaches (its shadow).
    pub corner_patch_reach: f64,
    /// Most frames a second of the patch's live picture.
    pub corner_patch_fps: u32,
}

/// The `[sugar]` section: shortcuts that ride on gestures people already make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SugarConfig {
    /// Shift held while opening an app, on a Dock click or on a card click brings windows
    /// into the current stage instead of switching.
    pub shift_brings_here: bool,
    /// A newly opened app's first window counts a Shift held this long before.
    pub launch_window_ms: u64,
    /// An activation counts a Shift held this long before.
    pub activation_window_ms: u64,
}

/// The `[stages]` section.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StagesConfig {
    /// Named stages created at launch, in order.
    pub names: Vec<String>,
}

/// `[log] level`: how much the daily log keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogLevel {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// The main events.
    Info,
    /// Every decision the app makes (the trial default).
    #[default]
    Debug,
    /// Also per-move and per-frame detail.
    Trace,
}

impl LogLevel {
    /// The level named in the config (`"debug"`, …); anything else is the default.
    pub fn parse(text: &str) -> Self {
        match text {
            "error" => Self::Error,
            "warn" => Self::Warn,
            "info" => Self::Info,
            "trace" => Self::Trace,
            _ => Self::Debug,
        }
    }

    /// The filter directives it stands for (`RUST_LOG` syntax): other crates stay at
    /// `info` while ours are at `debug` or `trace`.
    pub fn directives(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "info,stage_manager=debug",
            Self::Trace => "info,stage_manager=trace",
        }
    }
}

/// `[log]`: the daily log's level and limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogConfig {
    /// How much it keeps.
    pub level: LogLevel,
    /// Day files kept (today included).
    pub days: u32,
    /// A day's file cap, in MB.
    pub max_mb_per_day: u64,
    /// The folder's cap, in MB.
    pub max_mb_total: u64,
}

impl Default for LogConfig {
    fn default() -> Self {
        Config::default().log
    }
}

fn log_config(v: &Values) -> LogConfig {
    LogConfig {
        level: LogLevel::parse(v.text("log.level")),
        days: v.int("log.days").clamp(1, 365) as u32,
        max_mb_per_day: v.uint("log.max_mb_per_day"),
        max_mb_total: v.uint("log.max_mb_total"),
    }
}

/// The whole configuration, validated.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Strip order and capacity.
    pub strip: StripConfig,
    /// How the strip looks and moves.
    pub style: StripStyle,
    /// Window treatment.
    pub windows: WindowsConfig,
    /// New-window rules with the `[defaults]` decision.
    pub rules: RuleSet,
    /// Hotkeys (an extension; off unless `[hotkeys] enabled = true`).
    pub hotkeys: HotkeyConfig,
    /// Named stages.
    pub stages: StagesConfig,
    /// Modifier sugar.
    pub sugar: SugarConfig,
    /// The daily log.
    pub log: LogConfig,
}

impl Default for Config {
    fn default() -> Self {
        match Config::parse("") {
            Ok(p) => p.config,
            // The built-in defaults are static and tested; this cannot happen.
            Err(e) => panic!("built-in config defaults are invalid: {e}"),
        }
    }
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

/// Validated values by `section.key`, defaults filled in.
struct Values(BTreeMap<String, toml::Value>);

impl Values {
    fn get(&self, path: &str) -> &toml::Value {
        match self.0.get(path) {
            Some(v) => v,
            None => panic!("config key {path} is not in the schema"),
        }
    }
    fn num(&self, path: &str) -> f64 {
        let v = self.get(path);
        v.as_float().or_else(|| v.as_integer().map(|i| i as f64)).unwrap_or(0.0)
    }
    fn int(&self, path: &str) -> i64 {
        self.get(path).as_integer().unwrap_or(0)
    }
    fn uint(&self, path: &str) -> u64 {
        self.int(path).max(0) as u64
    }
    fn count(&self, path: &str) -> usize {
        self.int(path).max(0) as usize
    }
    fn flag(&self, path: &str) -> bool {
        self.get(path).as_bool().unwrap_or(false)
    }
    fn text(&self, path: &str) -> &str {
        self.get(path).as_str().unwrap_or("")
    }
    fn auto(&self, path: &str) -> Auto {
        match self.get(path).as_str() {
            Some(_) => Auto::Auto,
            None => Auto::Value(self.num(path)),
        }
    }
    fn color(&self, path: &str) -> Rgba {
        Rgba::parse(self.text(path)).unwrap_or(Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 })
    }
}

/// Checks one value against its kind; numbers out of range are clamped with a warning.
fn validate(d: &schema::KeyDef, v: &toml::Value, warnings: &mut Vec<String>) -> Result<toml::Value, String> {
    let path = d.path();
    let ty = || format!("[{}] {}: expected {}", d.section, d.key, d.kind.describe());
    let clamp_f = |x: f64, min: f64, max: f64, warnings: &mut Vec<String>| {
        if x < min || x > max || !x.is_finite() {
            let c = if x.is_finite() { x.clamp(min, max) } else { min };
            warnings.push(format!("{path} = {x} is outside {min}..{max}; using {c}"));
            c
        } else {
            x
        }
    };
    let number = |v: &toml::Value| v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
    Ok(match d.kind {
        Kind::Bool => toml::Value::Boolean(v.as_bool().ok_or_else(ty)?),
        Kind::Float { min, max } => toml::Value::Float(clamp_f(number(v).ok_or_else(ty)?, min, max, warnings)),
        Kind::Int { min, max } => {
            let x = v.as_integer().ok_or_else(ty)?;
            if x < min || x > max {
                let c = x.clamp(min, max);
                warnings.push(format!("{path} = {x} is outside {min}..{max}; using {c}"));
                toml::Value::Integer(c)
            } else {
                toml::Value::Integer(x)
            }
        }
        Kind::FloatOrAuto { min, max } => match (v.as_str(), number(v)) {
            (Some("auto"), _) => toml::Value::String("auto".into()),
            (None, Some(x)) => toml::Value::Float(clamp_f(x, min, max, warnings)),
            _ => return Err(ty()),
        },
        Kind::Choice(choices) => {
            let s = v.as_str().ok_or_else(ty)?;
            if !choices.contains(&s) {
                return Err(format!("[{}] {} {s:?}: expected {}", d.section, d.key, d.kind.describe()));
            }
            v.clone()
        }
        Kind::Color => {
            Rgba::parse(v.as_str().ok_or_else(ty)?).map_err(|e| format!("[{}] {}: {e}", d.section, d.key))?;
            v.clone()
        }
        Kind::Hotkey => {
            hotkey::parse_hotkey(v.as_str().ok_or_else(ty)?)?;
            v.clone()
        }
        Kind::NumberedHotkey => {
            hotkey::parse_numbered(v.as_str().ok_or_else(ty)?)?;
            v.clone()
        }
        Kind::Names => {
            let items = v.as_array().ok_or_else(ty)?;
            if items.iter().any(|i| !i.is_str()) {
                return Err(ty());
            }
            v.clone()
        }
        Kind::Action => {
            RuleAction::parse(v.as_str().ok_or_else(ty)?)?;
            v.clone()
        }
        Kind::Placement => {
            Placement::parse(v.as_str().ok_or_else(ty)?)?;
            v.clone()
        }
    })
}

fn default_values() -> BTreeMap<String, toml::Value> {
    KEYS.iter()
        .map(|d| {
            let doc = format!("v = {}", d.default);
            let value = toml::from_str::<toml::Table>(&doc).ok().and_then(|mut t| t.remove("v"));
            match value {
                Some(v) => (d.path(), v),
                None => panic!("schema default for {} is not TOML: {}", d.path(), d.default),
            }
        })
        .collect()
}

impl Config {
    /// Parses and validates config text.
    pub fn parse(text: &str) -> Result<Parsed, ConfigError> {
        let err = |e: String| ConfigError(format!("config.toml: {e}"));
        let table: toml::Table = toml::from_str(text).map_err(|e| ConfigError(format!("config.toml: {e}")))?;
        let mut warnings = Vec::new();
        let mut values = default_values();
        let mut raw_rules: Vec<toml::Table> = Vec::new();
        for (section, value) in &table {
            if section == "rule" {
                let items = value.as_array().ok_or_else(|| err("[[rule]] must be an array of tables".into()))?;
                for item in items {
                    let t = item.as_table().ok_or_else(|| err("[[rule]] must be an array of tables".into()))?;
                    for key in t.keys() {
                        if !RULE_KEYS.contains(&key.as_str()) {
                            warnings.push(format!("unknown key {key:?} in [[rule]] ignored"));
                        }
                    }
                    raw_rules.push(t.clone());
                }
                continue;
            }
            if !SECTIONS.iter().any(|s| s.name == section) {
                warnings.push(format!("unknown section [{section}] ignored"));
                continue;
            }
            let t = value.as_table().ok_or_else(|| err(format!("[{section}] must be a table")))?;
            for (key, v) in t {
                if let Some((_, _, why)) = RETIRED.iter().find(|(s, k, _)| s == section && k == key) {
                    warnings.push(format!("[{section}] {key}: {why}"));
                    continue;
                }
                let Some(d) = KEYS.iter().find(|d| d.section == section && d.key == key) else {
                    warnings.push(format!("unknown key {key:?} in [{section}] ignored"));
                    continue;
                };
                let checked = validate(d, v, &mut warnings).map_err(err)?;
                values.insert(d.path(), checked);
            }
        }
        let v = Values(values);

        let rules = raw_rules
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let s = |k: &str| t.get(k).map(|v| v.as_str().ok_or(format!("[[rule]] #{}: {k} must be a string", i + 1)));
                let app = s("app").ok_or(format!("[[rule]] #{}: missing app", i + 1))??;
                let title = s("title").transpose()?;
                let action = s("action").transpose()?;
                let placement = s("placement").transpose()?;
                Rule::parse(app, title, action, placement).map_err(|e| format!("[[rule]] #{}: {e}", i + 1))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        let defaults = Decision {
            action: RuleAction::parse(v.text("defaults.action")).map_err(err)?,
            placement: Placement::parse(v.text("defaults.placement")).map_err(err)?,
        };

        let numbered = |p: &str| hotkey::parse_numbered(v.text(p)).map_err(err);
        let fixed = |p: &str| hotkey::parse_hotkey(v.text(p)).map_err(err);
        let hotkeys = HotkeyConfig {
            enabled: v.flag("hotkeys.enabled"),
            switch: numbered("hotkeys.switch")?,
            send: numbered("hotkeys.send")?,
            new_stage: fixed("hotkeys.new_stage")?,
            toggle_pin: fixed("hotkeys.toggle_pin")?,
            toggle_strip: fixed("hotkeys.toggle_strip")?,
        };
        if hotkeys.enabled {
            warnings.extend(hotkeys.conflicts());
        }

        let mut names: Vec<String> = Vec::new();
        for n in v.get("stages.names").as_array().into_iter().flatten().filter_map(toml::Value::as_str) {
            let n = n.trim().to_string();
            if n.is_empty() {
                warnings.push("[stages] an empty name was ignored".to_string());
            } else if names.iter().any(|m| m.eq_ignore_ascii_case(&n)) {
                warnings.push(format!("[stages] duplicate name {n:?} ignored"));
            } else {
                names.push(n);
            }
        }

        let speed = v.num("animation.speed");
        if speed > 0.0 && speed < MIN_SPEED {
            warnings.push(format!("animation.speed = {speed} is outside {MIN_SPEED}..4 (0 = none); using {MIN_SPEED}"));
        }

        let config = Config {
            strip: StripConfig {
                order: if v.text("strip.order") == "fixed" { StripOrder::Fixed } else { StripOrder::Native },
                show_active: v.flag("strip.show_active"),
                hide_while_desktop_shown: v.flag("strip.hide_while_desktop_shown"),
                capacity: CapacityRule {
                    fixed: v.count("strip.capacity"),
                    pitch: v.num("strip.capacity_pitch"),
                    margin: v.num("strip.capacity_margin"),
                    min: v.count("strip.capacity_min"),
                    max: v.count("strip.capacity_max"),
                },
                thumbnail_interval_ms: v.uint("strip.thumbnail_interval_ms"),
                capture: v.flag("strip.capture"),
            },
            style: StripStyle {
                level: match v.text("strip.level") {
                    "above_windows" => StripLevel::AboveWindows,
                    _ => StripLevel::BelowWindows,
                },
                reorder: v.flag("strip.reorder"),
                merge_on_drop: v.flag("strip.merge_on_drop"),
                width: v.num("strip.width"),
                scale: v.num("strip.scale"),
                scale_to_display: v.flag("strip.scale_to_display"),
                show_numbers: v.flag("strip.show_numbers"),
                show_names: v.flag("strip.show_names"),
                thumbnail_resolution: v.num("strip.thumbnail_resolution"),
                cards: CardStyle {
                    x: v.num("cards.x"),
                    full_height: v.num("cards.full_height"),
                    min_height: v.num("cards.min_height"),
                    max_width: v.num("cards.max_width"),
                    gap: v.num("cards.gap"),
                    tilt: v.auto("cards.tilt"),
                    perspective: v.auto("cards.perspective"),
                    perspective_x: v.num("cards.perspective_x"),
                    perspective_y: v.num("cards.perspective_y"),
                    corner_radius: v.num("cards.corner_radius"),
                    border_width: v.num("cards.border_width"),
                    border_color: v.color("cards.border_color"),
                    shadow_radius: v.num("cards.shadow_radius"),
                    shadow_opacity: v.num("cards.shadow_opacity"),
                    shadow_offset_x: v.num("cards.shadow_offset_x"),
                    shadow_offset_y: v.num("cards.shadow_offset_y"),
                    shadow_color: v.color("cards.shadow_color"),
                    max_windows: v.count("cards.max_windows"),
                    stack_dx: v.num("cards.stack_dx"),
                    behind_scale: v.num("cards.behind_scale"),
                    second_app_dx: v.num("cards.second_app_dx"),
                    second_app_dy: v.num("cards.second_app_dy"),
                    hover_scale: v.num("cards.hover_scale"),
                    hover_ms: v.uint("cards.hover_ms"),
                    press_scale: v.num("cards.press_scale"),
                    press_ms: v.uint("cards.press_ms"),
                    fade_ms: v.uint("cards.fade_ms"),
                },
                icons: IconStyle {
                    size: v.num("icons.size"),
                    center_dx: v.num("icons.center_dx"),
                    center_dy: v.num("icons.center_dy"),
                    step: v.num("icons.step"),
                    max: v.count("icons.max"),
                    shadow_radius: v.num("icons.shadow_radius"),
                    shadow_opacity: v.num("icons.shadow_opacity"),
                    shadow_offset_y: v.num("icons.shadow_offset_y"),
                },
                motion: MotionStyle {
                    spring_stiffness: v.num("motion.spring_stiffness"),
                    spring_damping: v.num("motion.spring_damping"),
                    spring_mass: v.num("motion.spring_mass"),
                    slide_stiffness: v.num("motion.slide_stiffness"),
                    slide_damping: v.num("motion.slide_damping"),
                    slide_mass: v.num("motion.slide_mass"),
                    slide_distance: v.num("motion.slide_distance"),
                },
                reveal: RevealStyle {
                    edge_width: v.num("reveal.edge_width"),
                    delay_ms: v.uint("reveal.delay_ms"),
                    hide_delay_ms: v.uint("reveal.hide_delay_ms"),
                    poll_ms: v.uint("reveal.poll_ms"),
                    edge_tab: v.flag("reveal.edge_tab"),
                    launch_reveal: v.flag("reveal.launch_reveal"),
                    launch_reveal_ms: v.uint("reveal.launch_reveal_ms"),
                },
                drag: DragStyle {
                    threshold: v.num("drag.threshold"),
                    merge_zone: v.num("drag.merge_zone"),
                    grow_distance: v.num("drag.grow_distance"),
                    lifted_opacity: v.num("drag.lifted_opacity"),
                    ghost_opacity: v.num("drag.ghost_opacity"),
                    merge_ms: v.uint("drag.merge_ms"),
                },
                animation: AnimationStyle {
                    enabled: v.flag("animation.enabled"),
                    speed: v.num("animation.speed"),
                    spring_stiffness: v.num("animation.spring_stiffness"),
                    spring_damping: v.num("animation.spring_damping"),
                    spring_mass: v.num("animation.spring_mass"),
                    unpark_at: v.num("animation.unpark_at"),
                    cover_ms: v.uint("animation.cover_ms"),
                    corner_radius: v.num("animation.corner_radius"),
                    shadow_radius: v.num("animation.shadow_radius"),
                    shadow_opacity: v.num("animation.shadow_opacity"),
                    shadow_offset_y: v.num("animation.shadow_offset_y"),
                    snapshot_interval_ms: v.uint("animation.snapshot_interval_ms"),
                    snapshot_cache_mb: v.uint("animation.snapshot_cache_mb"),
                    activation_grows: v.flag("animation.activation_grows"),
                },
            },
            windows: WindowsConfig {
                fit: match v.text("windows.fit") {
                    "move" => FitMode::Move,
                    "narrow" => FitMode::Narrow,
                    _ => FitMode::Off,
                },
                fit_margin: v.num("windows.fit_margin"),
                minimize_to_strip: v.flag("windows.minimize_to_strip"),
                focus_echo_ms: v.uint("windows.focus_echo_ms"),
                inert_slivers: v.flag("windows.inert_slivers"),
                park_inside: v.num("windows.park_inside"),
                park_drop: v.num("windows.park_drop"),
                hide_parked_apps: v.flag("windows.hide_parked_apps"),
                corner_patch: v.flag("windows.corner_patch"),
                corner_patch_reach: v.num("windows.corner_patch_reach"),
                corner_patch_fps: v.uint("windows.corner_patch_fps") as u32,
            },
            rules: RuleSet::new(rules, defaults),
            hotkeys,
            stages: StagesConfig { names },
            sugar: SugarConfig {
                shift_brings_here: v.flag("sugar.shift_brings_here"),
                launch_window_ms: v.uint("sugar.launch_window_ms"),
                activation_window_ms: v.uint("sugar.activation_window_ms"),
            },
            log: log_config(&v),
        };
        Ok(Parsed { config, warnings })
    }

    /// Strip options for the model; `capacity` is computed by the platform from the screen.
    pub fn strip_options(&self, capacity: usize) -> StripOptions {
        StripOptions { order: self.strip.order, show_active: self.strip.show_active, capacity }
    }

    /// Model options from this config plus Apple's settings and the strip capacity.
    pub fn model_options(&self, settings: NativeSettings, capacity: usize) -> ModelOptions {
        ModelOptions {
            settings,
            strip: self.strip_options(capacity),
            named_stages: self.stages.names.clone(),
            fit: crate::engine::stage_area::FitOptions::default(),
            park: crate::engine::park::ParkRequest { inside: self.windows.park_inside, drop: self.windows.park_drop },
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

/// What [`load_or_create`] did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    /// Read it as it was.
    Read,
    /// It was missing: wrote the commented default file.
    Created,
    /// It was an unedited default file of an earlier version: replaced it with today's.
    Upgraded,
}

/// Result of [`load_or_create`].
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    /// The config.
    pub config: Config,
    /// Warnings from parsing.
    pub warnings: Vec<String>,
    /// What happened to the file.
    pub action: FileAction,
}

/// True when `text` is a default file some earlier version wrote, never edited.
pub fn is_legacy_default(text: &str) -> bool {
    schema::LEGACY_DEFAULTS.contains(&text)
}

/// Loads `path`. A missing file is written with [`default_file`] first (and its directory);
/// a file byte-identical to an earlier version's default is replaced with it. Any other
/// file is left alone.
pub fn load_or_create(path: &Path) -> Result<Loaded, ConfigError> {
    let mut action = FileAction::Read;
    let write = |path: &Path| {
        std::fs::write(path, default_file()).map_err(|e| ConfigError(format!("cannot write {}: {e}", path.display())))
    };
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| ConfigError(format!("cannot create {}: {e}", dir.display())))?;
        }
        write(path)?;
        action = FileAction::Created;
    } else if std::fs::read_to_string(path).is_ok_and(|t| is_legacy_default(&t)) {
        write(path)?;
        action = FileAction::Upgraded;
    }
    let text = std::fs::read_to_string(path).map_err(|e| ConfigError(format!("cannot read {}: {e}", path.display())))?;
    let Parsed { config, warnings } = Config::parse(&text)?;
    Ok(Loaded { config, warnings, action })
}

/// The config in force plus where it came from. A failed reload keeps the previous config.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
    config: Config,
}

impl ConfigStore {
    /// Opens `path`, creating (or upgrading an unedited) default file. A bad file yields the
    /// built-in defaults plus the error. Warnings and what happened to the file come back
    /// for logging.
    pub fn open(path: PathBuf) -> (Self, Result<(Vec<String>, FileAction), ConfigError>) {
        match load_or_create(&path) {
            Ok(l) => (Self { path, config: l.config }, Ok((l.warnings, l.action))),
            Err(e) => (Self { path, config: Config::default() }, Err(e)),
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

/// `stage-manager config ...`: `--print-defaults`, `--explain <section.key>`,
/// `--reference` (the guide's table), `--check [file]`. Returns the exit code and output.
pub fn command(args: &[String]) -> (i32, String) {
    match args.first().map(String::as_str) {
        Some("--print-defaults") => (0, default_file()),
        Some("--reference") => (0, schema::markdown_reference()),
        Some("--explain") => match args.get(1) {
            Some(key) => match schema::explain(key) {
                Some(text) => (0, text),
                None => {
                    let close: Vec<String> = KEYS
                        .iter()
                        .map(|d| d.path())
                        .filter(|p| p.contains(key.as_str()) || key.contains(p.split('.').next_back().unwrap_or("")))
                        .collect();
                    let hint = if close.is_empty() { String::new() } else { format!(" (did you mean {}?)", close.join(", ")) };
                    (1, format!("unknown key {key:?}{hint}; keys are section.key, for example windows.fit\n"))
                }
            },
            None => (2, "usage: stage-manager config --explain <section.key>\n".into()),
        },
        Some("--check") => {
            let path = args.get(1).map(PathBuf::from).or_else(default_path);
            let Some(path) = path else { return (2, "no config path\n".into()) };
            match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| Config::parse(&t).map_err(|e| e.0)) {
                Ok(p) => {
                    let mut out = format!("{}: ok\n", path.display());
                    for w in p.warnings {
                        out.push_str(&format!("warning: {w}\n"));
                    }
                    (0, out)
                }
                Err(e) => (1, format!("{e}\n")),
            }
        }
        _ => (
            2,
            "usage: stage-manager config --print-defaults | --explain <section.key> | --reference | --check [file]\n".into(),
        ),
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
    fn the_default_file_parses_to_the_defaults_without_warnings() {
        let p = parse(&default_file());
        assert_eq!(p.config, Config::default());
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }

    #[test]
    fn every_key_uncommented_with_its_default_parses_to_the_defaults() {
        let uncommented: String = default_file()
            .lines()
            .map(|l| match l.strip_prefix("# ") {
                Some(rest) if KEYS.iter().any(|d| rest == format!("{} = {}", d.key, d.default)) => rest,
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let p = parse(&uncommented);
        assert_eq!(p.config, Config::default());
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }

    #[test]
    fn the_rule_examples_parse_when_uncommented() {
        let uncommented: String = default_file()
            .lines()
            .skip_while(|l| !l.starts_with("# Rules for new windows"))
            .map(|l| match l.strip_prefix("# ") {
                Some(rest) if rest.starts_with("[[rule]]") || rest.contains(" = ") => rest,
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse(&uncommented).config.rules.rules.len(), 2);
    }

    #[test]
    fn the_defaults_are_apples() {
        let c = Config::default();
        assert_eq!(c.windows.fit, FitMode::Off, "windows keep their exact frames");
        assert!(!c.style.reveal.edge_tab && !c.style.reveal.launch_reveal);
        assert!(!c.style.show_numbers && !c.style.show_names);
        assert!(!c.hotkeys.enabled && c.hotkeys.bindings().is_empty(), "no global hotkeys");
        assert!(!c.style.scale_to_display && c.style.scale == 1.0);
        assert!(c.sugar.shift_brings_here, "invisible until Shift is held");
        assert!(c.windows.minimize_to_strip);
        assert_eq!(c.style.level, StripLevel::BelowWindows, "a window moved over the strip covers it");
        assert!(c.style.reorder, "invisible until a card is dragged");
        assert!(c.style.merge_on_drop, "invisible until a card is dropped on a card (the owner's word, 10-12)");
        assert_eq!(c.style.drag.merge_zone, 0.5);
        assert!(!c.strip.hide_while_desktop_shown, "the strip stays shown while the desktop is revealed");
        let a = &c.style.animation;
        assert!(a.enabled && a.speed == 1.0 && a.speed() == 1.0, "every animation at Apple's speed");
        assert_eq!((a.spring_stiffness, a.spring_damping, a.spring_mass), (272.0, 33.0, 1.0), "Apple's switch, measured 10-12");
        let m = &c.style.motion;
        assert_eq!((m.spring_stiffness, m.spring_damping, m.spring_mass), (289.0, 34.0, 1.0), "Apple's card glide");
        assert_eq!((m.slide_stiffness, m.slide_damping, m.slide_mass), (282.0, 33.6, 1.0), "Apple's slide");
        let k = &c.style.cards;
        assert_eq!((k.hover_scale, k.hover_ms, k.press_scale, k.press_ms), (1.06, 210, 0.987, 100), "Apple's hover and press");
        assert_eq!(c.strip.capacity.for_height(1130.0, 1.0), 5, "his laptop");
        assert_eq!(c.strip.capacity.for_height(1410.0, 1.0), 6, "5K");
    }

    #[test]
    fn the_log_section_has_the_trial_defaults_and_parses() {
        let c = Config::default().log;
        assert_eq!(c, LogConfig { level: LogLevel::Debug, days: 14, max_mb_per_day: 100, max_mb_total: 500 });
        assert_eq!(c.level.directives(), "info,stage_manager=debug");
        let p = parse("[log]\nlevel = \"info\"\ndays = 3\nmax_mb_per_day = 5\nmax_mb_total = 0\n");
        assert_eq!(p.config.log, LogConfig { level: LogLevel::Info, days: 3, max_mb_per_day: 5, max_mb_total: 1 });
        assert_eq!(p.warnings.len(), 1, "max_mb_total clamped: {:?}", p.warnings);
        assert!(Config::parse("[log]\nlevel = \"loud\"\n").is_err());
    }

    #[test]
    fn animation_speed_scales_or_stops_everything() {
        let speed = |text: &str| parse(text).config.style.animation;
        let a = speed("[animation]\nspeed = 2\n");
        assert_eq!((a.speed(), a.seconds(210)), (2.0, 0.105));
        let a = speed("[animation]\nspeed = 0.5\n");
        assert_eq!((a.speed(), a.seconds(100)), (0.5, 0.2));
        let a = speed("[animation]\nspeed = 0\n");
        assert!(!a.animates() && a.seconds(210) == 0.0, "0 is no animation");
        let p = parse("[animation]\nspeed = 0.1\n");
        assert_eq!(p.config.style.animation.speed(), MIN_SPEED, "the slowest other than none");
        assert_eq!(p.warnings.len(), 1, "raised with a warning: {:?}", p.warnings);
        assert!(p.warnings[0].contains("animation.speed = 0.1") && p.warnings[0].contains("using 0.25"), "{:?}", p.warnings);
        assert!(parse("[animation]\nspeed = 0.25\n").warnings.is_empty(), "the slowest itself is fine");
        assert!(parse("[animation]\nspeed = 0\n").warnings.is_empty(), "0 (none) is fine");
        let p = parse("[animation]\nspeed = 9\n");
        assert_eq!(p.config.style.animation.speed(), 4.0);
        assert_eq!(p.warnings.len(), 1, "clamped with a warning: {:?}", p.warnings);
        let a = speed("[animation]\nenabled = false\nspeed = 2\n");
        assert!(!a.animates(), "enabled = false wins");
    }

    /// Every animation's timing ([`schema`] keys of the guide's "Animations" table).
    const ANIMATION_KEYS: &[&str] = &[
        "animation.enabled",
        "animation.speed",
        "animation.spring_stiffness",
        "animation.spring_damping",
        "animation.spring_mass",
        "animation.unpark_at",
        "motion.spring_stiffness",
        "motion.spring_damping",
        "motion.spring_mass",
        "motion.slide_stiffness",
        "motion.slide_damping",
        "motion.slide_mass",
        "motion.slide_distance",
        "cards.hover_ms",
        "cards.hover_scale",
        "cards.press_ms",
        "cards.press_scale",
        "cards.fade_ms",
        "drag.merge_ms",
    ];

    #[test]
    fn every_animation_key_reaches_the_strip_live() {
        // A reload hands the strip a new style whenever it differs (the controller's
        // `reload_config`), and every motion reads it at its start: each key must be part of
        // the style.
        let base = Config::default().style;
        for path in ANIMATION_KEYS {
            let d = schema::find(path).unwrap_or_else(|| panic!("{path} is a key"));
            let other = match d.kind {
                Kind::Bool => (d.default != "true").to_string(),
                Kind::Float { min, max } => format!("{:?}", if d.default.parse::<f64>().ok() == Some(max) { min } else { max }),
                Kind::Int { min, max } => (if d.default.parse::<i64>().ok() == Some(max) { min } else { max }).to_string(),
                _ => panic!("{path}: not a timing"),
            };
            let text = format!("[{}]\n{} = {other}\n", d.section, d.key);
            assert_ne!(parse(&text).config.style, base, "{path} = {other} changes the strip's style");
        }
    }

    #[test]
    fn readme_has_the_generated_reference() {
        let readme = include_str!("../docs/GUIDE.md");
        assert!(
            readme.contains(&schema::markdown_reference()),
            "docs/GUIDE.md's configuration reference is out of date: paste `stage-manager config --reference`"
        );
    }

    #[test]
    fn empty_file_is_all_defaults() {
        assert_eq!(parse("").config, Config::default());
    }

    #[test]
    fn full_config() {
        let p = parse(
            r##"
[strip]
order = "fixed"
level = "above_windows"
reorder = false
merge_on_drop = false
show_active = true
show_numbers = true
hide_while_desktop_shown = true
thumbnail_interval_ms = 2000
capacity = 4

[cards]
tilt = 30
perspective = 900.5
shadow_color = "#11223380"

[windows]
fit = "narrow"
minimize_to_strip = false

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
enabled = true
switch = "cmd+alt+{n}"
toggle_strip = ""

[stages]
names = ["Work", "Chat", "work", " "]

[sugar]
shift_brings_here = false

[animation]
enabled = false
unpark_at = 0.9
"##,
        );
        let c = p.config;
        assert_eq!(c.style.level, StripLevel::AboveWindows);
        assert!(!c.style.reorder && !c.style.merge_on_drop);
        assert!(!c.style.animation.enabled && c.style.animation.unpark_at == 0.9);
        assert_eq!(c.strip.order, StripOrder::Fixed);
        assert!(c.strip.show_active && c.style.show_numbers && c.strip.hide_while_desktop_shown);
        assert_eq!(c.strip.thumbnail_interval_ms, 2000);
        assert_eq!(c.strip.capacity.for_height(1410.0, 1.0), 4);
        assert_eq!(c.style.cards.tilt, Auto::Value(30.0));
        assert_eq!(c.style.cards.perspective, Auto::Value(900.5));
        assert!((c.style.cards.shadow_color.a - 128.0 / 255.0).abs() < 1e-9);
        assert_eq!(c.windows.fit, FitMode::Narrow);
        assert!(!c.windows.minimize_to_strip);
        assert_eq!(c.rules.defaults, Decision { action: RuleAction::New, placement: Placement::Cascade });
        assert_eq!(c.rules.rules.len(), 2);
        assert_eq!(c.rules.rules[1].app, AppMatch::Any);
        assert_eq!(c.rules.decide("com.example.chat", "x").action, RuleAction::Stage("Chat".into()));
        assert_eq!(c.rules.decide("com.example.video", "Picture in Picture").action, RuleAction::Float);
        assert!(c.hotkeys.enabled);
        assert!(c.hotkeys.switch.is_some_and(|s| s.mods.cmd && s.mods.alt && !s.mods.ctrl));
        assert_eq!(c.hotkeys.toggle_strip, None);
        assert_eq!(c.hotkeys.new_stage, HotkeyConfig::default().new_stage, "unset keeps default");
        assert_eq!(c.stages.names, ["Work", "Chat"]);
        assert!(!c.sugar.shift_brings_here);
        assert_eq!(p.warnings.len(), 2, "{:?}", p.warnings);
    }

    #[test]
    fn unknown_and_retired_keys_warn_and_do_not_fail() {
        let p = parse(
            r#"
[strip]
order = "native"
position = "left"

[stages]
fit_windows = true

[[rule]]
app = "com.example.a"
when = "always"

[gestures]
swipe = true
"#,
        );
        assert_eq!(p.config.strip, Config::default().strip);
        assert_eq!(p.config.windows, Config::default().windows);
        assert_eq!(p.warnings.len(), 4, "{:?}", p.warnings);
        assert!(p.warnings.iter().any(|w| w.contains("position")));
        assert!(p.warnings.iter().any(|w| w.contains("[gestures]")));
        assert!(p.warnings.iter().any(|w| w.contains("fit_windows") && w.contains("[windows] fit")));
    }

    #[test]
    fn bad_values_are_errors() {
        for text in [
            "[strip]\norder = \"recent\"",
            "[strip]\nshow_numbers = \"yes\"",
            "[strip]\ncapacity = 2.5",
            "[cards]\ntilt = \"steep\"",
            "[cards]\nborder_color = \"white\"",
            "[windows]\nfit = \"tile\"",
            "[defaults]\naction = \"tile\"",
            "[defaults]\nplacement = \"grid\"",
            "[[rule]]\ntitle = \"x\"",
            "[[rule]]\napp = \"a\"\ntitle = \"(\"",
            "[hotkeys]\nswitch = \"ctrl+alt+s\"",
            "[hotkeys]\nnew_stage = \"ctrl+alt+{n}\"",
            "[hotkeys]\ntoggle_pin = \"p\"",
            "[sugar]\nshift_brings_here = \"yes\"",
            "[stages]\nnames = [1]",
            "strip = 5",
            "not toml at all [",
        ] {
            assert!(Config::parse(text).is_err(), "should fail: {text}");
        }
    }

    #[test]
    fn out_of_range_numbers_are_clamped_with_a_warning() {
        let p = parse("[strip]\nthumbnail_interval_ms = 10\n[cards]\nhover_scale = 9.0");
        assert_eq!(p.config.strip.thumbnail_interval_ms, 250);
        assert_eq!(p.config.style.cards.hover_scale, 2.0);
        assert_eq!(p.warnings.len(), 2, "{:?}", p.warnings);
    }

    #[test]
    fn hotkey_conflicts_warn_only_when_enabled() {
        assert_eq!(parse("[hotkeys]\ntoggle_pin = \"ctrl+alt+n\"").warnings.len(), 0);
        let p = parse("[hotkeys]\nenabled = true\ntoggle_pin = \"ctrl+alt+n\"");
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
    }

    #[test]
    fn capacity_counts_from_the_height() {
        let c = Config::default().strip.capacity;
        assert_eq!(c.for_height(875.0, 1.0), 3, "small screens keep three");
        assert_eq!(c.for_height(2500.0, 1.0), 9);
        assert_eq!(c.for_height(f64::NAN, 1.0), 3);
        assert_eq!(c.for_height(1130.0, 0.5), 9, "smaller cards fit more");
    }

    #[test]
    fn model_options_carry_config_and_settings() {
        let p = parse("[strip]\norder = \"fixed\"\n[stages]\nnames = [\"Work\"]");
        let o = p.config.model_options(NativeSettings::default(), 5);
        assert_eq!(o.strip, StripOptions { order: StripOrder::Fixed, show_active: false, capacity: 5 });
        assert_eq!(o.named_stages, ["Work"]);
    }

    #[test]
    fn first_run_writes_the_commented_file_and_old_defaults_are_upgraded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("config.toml");
        let l = load_or_create(&path).expect("loads");
        assert_eq!(l.action, FileAction::Created);
        assert_eq!(std::fs::read_to_string(&path).expect("written"), default_file());
        assert_eq!(l.config, Config::default());
        assert_eq!(load_or_create(&path).expect("loads").action, FileAction::Read);

        for old in schema::LEGACY_DEFAULTS {
            std::fs::write(&path, old).expect("write");
            let l = load_or_create(&path).expect("loads");
            assert_eq!(l.action, FileAction::Upgraded);
            assert_eq!(std::fs::read_to_string(&path).expect("read"), default_file());
            assert_eq!(l.config, Config::default());
        }
        // An edited file is left alone, even one that differs by a byte.
        let edited = format!("{} ", schema::LEGACY_DEFAULTS[2]);
        std::fs::write(&path, &edited).expect("write");
        let l = load_or_create(&path).expect("loads");
        assert_eq!(l.action, FileAction::Read);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), edited);
        assert!(l.config.style.show_numbers, "its old values stay");
    }

    #[test]
    fn a_bad_reload_keeps_the_previous_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[strip]\norder = \"fixed\"").expect("write");
        let (mut store, first) = ConfigStore::open(path.clone());
        assert_eq!(first, Ok((vec![], FileAction::Read)));
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

    #[test]
    fn the_config_command_prints_explains_and_checks() {
        let (code, out) = command(&["--print-defaults".into()]);
        assert_eq!((code, out), (0, default_file()));
        let (code, out) = command(&["--explain".into(), "cards.tilt".into()]);
        assert_eq!(code, 0);
        assert!(out.contains("auto"));
        let (code, out) = command(&["--explain".into(), "tilt".into()]);
        assert_eq!(code, 1);
        assert!(out.contains("cards.tilt"), "{out}");
        assert_eq!(command(&[]).0, 2);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "[windows]\nfit = \"move\"\nx = 1").expect("write");
        let (code, out) = command(&["--check".into(), path.display().to_string()]);
        assert_eq!(code, 0);
        assert!(out.contains("warning"));
    }
}
