//! Every configuration key in one table: its section, type, valid range, built-in default
//! (Apple's value where Apple has one) and a one-line explanation.
//!
//! The table is the single source for parsing and validation ([`super::Config::parse`]),
//! for the commented file written on first run ([`default_file`]), for
//! `stage-manager config --print-defaults` and `--explain <key>`, and for the configuration
//! reference in `README.md` ([`markdown_reference`]; a test keeps it in sync).

use std::fmt::Write as _;

/// A key's type and valid values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// `true` or `false`.
    Bool,
    /// A number in `min..=max` (integers are accepted).
    Float { min: f64, max: f64 },
    /// A whole number in `min..=max`.
    Int { min: i64, max: i64 },
    /// A number in `min..=max`, or the string `"auto"` (a value computed from the display).
    FloatOrAuto { min: f64, max: f64 },
    /// One of these strings.
    Choice(&'static [&'static str]),
    /// A color: `"#rrggbb"` or `"#rrggbbaa"`.
    Color,
    /// A hotkey such as `"ctrl+alt+n"`, or `""` for none.
    Hotkey,
    /// A numbered hotkey template with `{n}`, or `""` for none.
    NumberedHotkey,
    /// A list of stage names.
    Names,
    /// A rule action (`native`, `current`, `new`, `stage:<name>`, `float`, `ignore`).
    Action,
    /// A placement (`as-is`, `center`, `cascade`).
    Placement,
}

impl Kind {
    /// How the type reads in docs.
    pub fn describe(&self) -> String {
        match self {
            Kind::Bool => "true or false".into(),
            Kind::Float { min, max } => format!("number, {min} to {max}"),
            Kind::Int { min, max } => format!("whole number, {min} to {max}"),
            Kind::FloatOrAuto { min, max } => format!("\"auto\" or a number, {min} to {max}"),
            Kind::Choice(c) => c.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(", "),
            Kind::Color => "color \"#rrggbb\" or \"#rrggbbaa\"".into(),
            Kind::Hotkey => "hotkey such as \"ctrl+alt+n\", or \"\" for none".into(),
            Kind::NumberedHotkey => "hotkey with {n} for the digits 1-9, or \"\" for none".into(),
            Kind::Names => "list of names".into(),
            Kind::Action => "\"native\", \"current\", \"new\", \"stage:<name>\", \"float\", \"ignore\"".into(),
            Kind::Placement => "\"as-is\", \"center\", \"cascade\"".into(),
        }
    }
}

/// When a changed value takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    /// Within a second of saving the file.
    Live,
    /// At the next launch.
    Restart,
}

/// One configuration key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyDef {
    /// The `[section]` it lives in.
    pub section: &'static str,
    /// The key's name.
    pub key: &'static str,
    /// Type and range.
    pub kind: Kind,
    /// The built-in default as a TOML literal.
    pub default: &'static str,
    /// One line: what it does (and Apple's value when the default is Apple's).
    pub doc: &'static str,
    /// When a change applies.
    pub applies: Applies,
}

impl KeyDef {
    /// `section.key`.
    pub fn path(&self) -> String {
        format!("{}.{}", self.section, self.key)
    }
}

/// One `[section]` with its heading comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionDef {
    /// Its name.
    pub name: &'static str,
    /// Comment lines written above its keys.
    pub doc: &'static [&'static str],
}

const fn k(section: &'static str, key: &'static str, kind: Kind, default: &'static str, doc: &'static str) -> KeyDef {
    KeyDef { section, key, kind, default, doc, applies: Applies::Live }
}

const fn f(min: f64, max: f64) -> Kind {
    Kind::Float { min, max }
}

const fn i(min: i64, max: i64) -> Kind {
    Kind::Int { min, max }
}

/// The sections, in file order.
pub const SECTIONS: &[SectionDef] = &[
    SectionDef {
        name: "strip",
        doc: &["The strip of recent stages on the left. Lengths are points."],
    },
    SectionDef {
        name: "cards",
        doc: &[
            "Each card is its stage's windows drawn small and turned in perspective, as Apple does:",
            "every window is scaled by one factor (`full_height` over the screen's visible height),",
            "turned about its left edge, and seen from a point at the screen's vertical middle.",
        ],
    },
    SectionDef { name: "icons", doc: &["The app icon over each card's bottom-left corner."] },
    SectionDef { name: "motion", doc: &["Cards and the strip move on one spring (Apple's, measured)."] },
    SectionDef { name: "reveal", doc: &["When the strip hides and comes back."] },
    SectionDef {
        name: "drag",
        doc: &["Dragging a card out of the strip or within it, and dragging a window onto the strip."],
    },
    SectionDef {
        name: "animation",
        doc: &[
            "Stage switches move as Apple's do: incoming windows grow from their card, outgoing ones",
            "shrink into their slot. Live windows cannot be scaled, so snapshots move in a",
            "click-through layer above everything while the real windows jump behind them.",
        ],
    },
    SectionDef { name: "windows", doc: &["How windows are treated."] },
    SectionDef {
        name: "sugar",
        doc: &["Extras that ride on gestures you already make; invisible until used."],
    },
    SectionDef {
        name: "hotkeys",
        doc: &[
            "Global hotkeys (an extension; Apple's Stage Manager has none). Off by default because",
            "they take key combinations other apps may use. Modifiers: ctrl, alt (option), shift, cmd.",
        ],
    },
    SectionDef { name: "stages", doc: &[] },
    SectionDef { name: "defaults", doc: &["What a new window does when no [[rule]] matches."] },
    SectionDef {
        name: "log",
        doc: &[
            "The daily log: ~/Library/Logs/Stage Manager/YYYY-MM-DD.log, local times. It never",
            "leaves this Mac; window titles may appear in it. RUST_LOG overrides `level`.",
        ],
    },
];

/// Every key, grouped by section in [`SECTIONS`] order.
pub const KEYS: &[KeyDef] = &[
    // [strip]
    k("strip", "order", Kind::Choice(&["native", "fixed"]), "\"native\"",
        "Card order: \"native\" is Apple's (the stage you leave takes the clicked card's slot); \"fixed\" is creation order."),
    k("strip", "level", Kind::Choice(&["below_windows", "above_windows"]), "\"below_windows\"",
        "Where the strip sits: \"below_windows\" is Apple's (a window moved over it covers it); \"above_windows\" keeps it over windows."),
    k("strip", "reorder", Kind::Bool, "true",
        "Drag a card up or down within the strip to give it another slot (an extension; Apple's cannot, invisible until used)."),
    k("strip", "show_active", Kind::Bool, "false", "Also show the active stage as a card (fixed order only; Apple never does)."),
    k("strip", "show_numbers", Kind::Bool, "false", "Draw each card's stage number (for the hotkeys). Apple shows none."),
    k("strip", "show_names", Kind::Bool, "false", "Draw stage names under cards. Apple shows none."),
    k("strip", "width", f(60.0, 800.0), "200.0",
        "Width of the strip column: the panel, and the area a window must cover to hide the strip."),
    k("strip", "capacity", i(0, 30), "0",
        "Most cards shown; 0 counts them from the screen's visible height (Apple: 5 at 1130 pt, 6 at 1410 pt)."),
    k("strip", "capacity_pitch", f(20.0, 2000.0), "190.0", "With capacity = 0: room per card, in points of visible height."),
    k("strip", "capacity_margin", f(0.0, 2000.0), "60.0", "With capacity = 0: visible height kept free above and below the cards."),
    k("strip", "capacity_min", i(1, 30), "3", "With capacity = 0: never fewer cards than this."),
    k("strip", "capacity_max", i(1, 30), "9", "With capacity = 0: never more cards than this."),
    k("strip", "scale", f(0.25, 4.0), "1.0", "Multiplies every strip length (cards, gaps, icons, width). Apple: 1."),
    k("strip", "scale_to_display", Kind::Bool, "false",
        "Also shrink strip lengths on screens smaller than 2560 x 1410 pt (by area, at least half). Apple does not."),
    k("strip", "thumbnail_interval_ms", i(250, 60_000), "1500", "How often thumbnails are refreshed, in milliseconds."),
    k("strip", "thumbnail_resolution", f(0.5, 4.0), "2.0", "Thumbnails are captured at this multiple of their drawn size."),
    k("strip", "capture", Kind::Bool, "true",
        "Capture pictures of windows (Screen Recording) for card thumbnails and switch animations; false draws plain cards and ghosts and never uses screen capture."),
    // [cards]
    k("cards", "x", f(-100.0, 400.0), "15.0", "Left edge of every card, from the screen's left edge."),
    k("cards", "full_height", f(10.0, 1000.0), "161.0",
        "A window as tall as the visible screen becomes a card this tall; every window gets the same scale."),
    k("cards", "min_height", f(0.0, 1000.0), "97.0", "Small windows are drawn at least this tall."),
    k("cards", "max_width", f(0.0, 2000.0), "0.0", "Widest card before the turn; 0 = as wide as a card of the whole screen."),
    k("cards", "gap", f(0.0, 500.0), "29.5", "Space between cards. The column is centered on the visible screen."),
    k("cards", "tilt", Kind::FloatOrAuto { min: 0.0, max: 85.0 }, "\"auto\"",
        "Turn of each card in degrees; \"auto\" is Apple's for the screen (44.2 at 1800 pt wide, 47.9 at 2560)."),
    k("cards", "perspective", Kind::FloatOrAuto { min: 50.0, max: 50_000.0 }, "\"auto\"",
        "Eye distance in points; \"auto\" is Apple's for the screen (1346 at 1800 pt wide, 2260 at 2560)."),
    k("cards", "perspective_x", f(-3000.0, 3000.0), "15.0", "The eye's horizontal position, from the screen's left edge."),
    k("cards", "perspective_y", f(-3000.0, 3000.0), "0.0", "The eye's height, from the screen's vertical middle (down is positive)."),
    k("cards", "corner_radius", f(0.0, 60.0), "4.0", "Corner radius of each window's thumbnail."),
    k("cards", "border_width", f(0.0, 10.0), "0.5", "Width of the thin edge around each thumbnail."),
    k("cards", "border_color", Kind::Color, "\"#ffffff2e\"", "Color of that edge."),
    k("cards", "shadow_radius", f(0.0, 100.0), "4.0", "Blur radius of each thumbnail's shadow."),
    k("cards", "shadow_opacity", f(0.0, 1.0), "0.4", "Opacity of that shadow."),
    k("cards", "shadow_offset_x", f(-100.0, 100.0), "0.0", "Horizontal offset of that shadow."),
    k("cards", "shadow_offset_y", f(-100.0, 100.0), "2.0", "Vertical offset of that shadow (down is positive)."),
    k("cards", "shadow_color", Kind::Color, "\"#000000\"", "Color of that shadow."),
    k("cards", "max_windows", i(1, 8), "3", "Most windows drawn on one card."),
    k("cards", "stack_dx", f(-200.0, 200.0), "17.0", "Windows of one app step this far right toward the front one (the one behind peeks out on the left)."),
    k("cards", "behind_scale", f(0.3, 1.0), "1.0", "Each window behind another of the same app is drawn this much smaller."),
    k("cards", "second_app_dx", f(-300.0, 300.0), "68.0", "A second app's window sits this far right of the first."),
    k("cards", "second_app_dy", f(-300.0, 300.0), "14.0", "...and this far up."),
    k("cards", "hover_scale", f(1.0, 2.0), "1.06", "The card under the pointer grows by this factor, about its center."),
    k("cards", "hover_ms", i(0, 3000), "200", "How long the hover growth takes, in milliseconds."),
    k("cards", "fade_ms", i(0, 3000), "200", "How long a new card takes to fade in, in milliseconds."),
    // [icons]
    k("icons", "size", f(0.0, 300.0), "38.0", "Size of the app icon image (macOS icons carry a margin; about 31 pt shows). 0 hides icons."),
    k("icons", "center_dx", f(-300.0, 300.0), "10.5", "The icon's center, right of the card's bottom-left corner (its leftmost window's)."),
    k("icons", "center_dy", f(-300.0, 300.0), "-10.5", "The icon's center, below the card's bottom-left corner (up is negative)."),
    k("icons", "step", f(0.0, 300.0), "36.0", "A second app's icon sits this far right of the first."),
    k("icons", "max", i(0, 10), "3", "Most icons on one card."),
    k("icons", "shadow_radius", f(0.0, 100.0), "4.0", "Blur radius of the icon's shadow."),
    k("icons", "shadow_opacity", f(0.0, 1.0), "0.3", "Opacity of the icon's shadow."),
    k("icons", "shadow_offset_y", f(-100.0, 100.0), "2.0", "Vertical offset of the icon's shadow (down is positive)."),
    // [motion]
    k("motion", "spring_stiffness", f(1.0, 10_000.0), "240.0", "Spring stiffness for cards moving and the strip sliding."),
    k("motion", "spring_damping", f(0.0, 1000.0), "31.0", "Spring damping (31 with stiffness 240 and mass 1: no overshoot)."),
    k("motion", "spring_mass", f(0.01, 100.0), "1.0", "Spring mass."),
    k("motion", "slide_distance", f(0.0, 3000.0), "286.0", "How far left the cards slide when the strip hides."),
    // [reveal]
    k("reveal", "edge_width", f(0.0, 100.0), "2.0", "The pointer within this many points of the left edge counts as at the edge."),
    k("reveal", "delay_ms", i(0, 10_000), "250", "Resting at the left edge this long brings a hidden strip back."),
    k("reveal", "hide_delay_ms", i(0, 10_000), "500", "A strip brought back hides again this long after the pointer leaves it."),
    k("reveal", "poll_ms", i(10, 1000), "60", "How often the pointer is checked while the strip is hidden."),
    k("reveal", "edge_tab", Kind::Bool, "false", "Show a slim tab at the left edge while a window hides the strip (an extension)."),
    k("reveal", "launch_reveal", Kind::Bool, "false", "Show a covered strip for a moment at launch (an extension)."),
    k("reveal", "launch_reveal_ms", i(0, 60_000), "3000", "How long that launch showing lasts."),
    // [drag]
    k("drag", "threshold", f(0.0, 100.0), "4.0", "A press that moves farther than this (points) drags the card; less is a click."),
    k("drag", "grow_distance", f(1.0, 3000.0), "260.0", "Past the strip, the dragged card grows to its window's size over this distance."),
    k("drag", "lifted_opacity", f(0.0, 1.0), "0.3", "Opacity of a card in the strip while it is dragged out."),
    k("drag", "ghost_opacity", f(0.0, 1.0), "0.92", "Opacity of the dragged copy of the card, and of a window's card while it is dragged over the strip."),
    // [animation]
    k("animation", "enabled", Kind::Bool, "true",
        "Animate switches, the desktop reveal, Cmd-M and drops as Apple does. false: windows jump."),
    k("animation", "spring_stiffness", f(1.0, 10_000.0), "240.0", "Spring stiffness of the windows' motion (Apple's: 99 % by about 430 ms)."),
    k("animation", "spring_damping", f(0.0, 1000.0), "31.0", "Spring damping (31 with stiffness 240 and mass 1: critically damped, no overshoot)."),
    k("animation", "spring_mass", f(0.01, 100.0), "1.0", "Spring mass."),
    k("animation", "unpark_at", f(0.5, 1.0), "0.99",
        "An incoming window takes its place behind its growing snapshot once the motion is this far along (0.99 = 99 %, about 430 ms)."),
    k("animation", "cover_ms", i(0, 500), "20",
        "Outgoing windows leave this long after their snapshots are drawn over them, so the snapshots are on screen first."),
    k("animation", "corner_radius", f(0.0, 60.0), "0.0",
        "Corner radius of a snapshot at full size; 0 keeps the window's own shape (the card's radius is [cards] corner_radius)."),
    k("animation", "shadow_radius", f(0.0, 100.0), "24.0", "Blur radius of a moving snapshot's shadow."),
    k("animation", "shadow_opacity", f(0.0, 1.0), "0.35", "Opacity of that shadow."),
    k("animation", "shadow_offset_y", f(-100.0, 100.0), "10.0", "Vertical offset of that shadow (down is positive)."),
    k("animation", "snapshot_interval_ms", i(0, 60_000), "2000",
        "How often full-size snapshots of the shown windows are refreshed for the next switch; 0 = only as the pointer nears the strip."),
    k("animation", "snapshot_cache_mb", i(16, 8192), "512", "Most memory kept for full-size window snapshots; the oldest go first."),
    // [windows]
    k("windows", "fit", Kind::Choice(&["off", "move", "narrow"]), "\"off\"",
        "Keep placed windows clear of the strip: \"move\" right just enough, \"narrow\" also too-wide ones. Apple: \"off\"."),
    k("windows", "fit_margin", f(0.0, 500.0), "16.0", "With fit: room kept between the strip column and the windows."),
    k("windows", "minimize_to_strip", Kind::Bool, "true",
        "Cmd-M (minimize) sends the window to the strip as a card of its own, as Apple does, instead of the Dock."),
    k("windows", "focus_echo_ms", i(0, 10_000), "700",
        "Focus changes this soon after we moved a window are taken as our own and never switch stages."),
    // [sugar]
    k("sugar", "shift_brings_here", Kind::Bool, "true",
        "Hold Shift while opening or activating an app (Dock, Finder, Spotlight Shift-Return, Launchpad) to bring it here."),
    k("sugar", "launch_window_ms", i(0, 60_000), "10000", "A newly opened app's first window counts a Shift held this long before."),
    k("sugar", "activation_window_ms", i(0, 60_000), "2000", "An activation counts a Shift held this long before."),
    // [hotkeys]
    k("hotkeys", "enabled", Kind::Bool, "false", "Register the hotkeys below."),
    k("hotkeys", "switch", Kind::NumberedHotkey, "\"ctrl+alt+{n}\"", "Switch to stage N."),
    k("hotkeys", "send", Kind::NumberedHotkey, "\"ctrl+alt+shift+{n}\"", "Send the focused window to stage N (N = count + 1 makes a stage)."),
    k("hotkeys", "new_stage", Kind::Hotkey, "\"ctrl+alt+n\"", "Send the focused window to a new stage."),
    k("hotkeys", "toggle_pin", Kind::Hotkey, "\"ctrl+alt+p\"", "Pin or unpin the focused window (shown on every stage)."),
    k("hotkeys", "toggle_strip", Kind::Hotkey, "\"ctrl+alt+s\"", "Show or hide the strip."),
    // [stages]
    k("stages", "names", Kind::Names, "[]",
        "Named stages created at launch in this order (hotkey numbers 1, 2, ...); they stay when empty."),
    // [defaults]
    k("defaults", "action", Kind::Action, "\"native\"",
        "\"native\" is Apple's: a window joins its app's stage, a newly opened app gets its own."),
    k("defaults", "placement", Kind::Placement, "\"as-is\"",
        "Where it goes: \"as-is\", \"center\", or \"cascade\" (offset from the front window)."),
    // [log]
    k("log", "level", Kind::Choice(&["error", "warn", "info", "debug", "trace"]), "\"debug\"",
        "How much the log keeps: \"debug\" (the trial default) records every decision, \"info\" only the main ones."),
    k("log", "days", i(1, 365), "14", "Day files older than this many days are deleted."),
    k("log", "max_mb_per_day", i(1, 10_000), "100",
        "A day's file this big takes only warnings, errors, marks and counts from then on."),
    k("log", "max_mb_total", i(1, 100_000), "500", "The log folder's cap: the oldest days go first, never today's."),
];

/// The `[[rule]]` keys (an array of tables, documented by example in the file).
pub const RULE_KEYS: &[&str] = &["app", "title", "action", "placement"];

/// Keys from earlier versions that are no longer read, with what replaced them.
pub const RETIRED: &[(&str, &str, &str)] = &[(
    "stages",
    "fit_windows",
    "it is ignored; [windows] fit = \"off\", \"move\" or \"narrow\" replaced it",
)];

/// A key by `section.key` path.
pub fn find(path: &str) -> Option<&'static KeyDef> {
    let (section, key) = path.split_once('.')?;
    KEYS.iter().find(|d| d.section == section && d.key == key)
}

/// The keys of one section, in order.
pub fn keys_of(section: &str) -> impl Iterator<Item = &'static KeyDef> + '_ {
    KEYS.iter().filter(move |d| d.section == section)
}

const HEADER: &str = "\
# Stage Manager configuration.
#
# Out of the box Stage Manager looks and behaves like Apple's Stage Manager. Every key below
# is commented out and shows its built-in default; uncomment a line to change it. Settings
# macOS already has (\"Show recent apps\", \"Show windows from an application\", \"Click
# wallpaper to reveal desktop\") are read live from System Settings > Desktop & Dock and are
# not repeated here. Saved changes apply within a second.
#
# `stage-manager config --explain <section.key>` explains one key;
# `stage-manager config --print-defaults` prints this file.
";

const RULES_DOC: &str = "\
# Rules for new windows, checked in order; the first match wins. `app` is a bundle ID
# (exact) or \"*\"; `title` is an optional regular expression. Leave out `action` or
# `placement` to use [defaults].
#
# [[rule]]
# app = \"com.example.chat\"
# action = \"stage:Chat\"
#
# [[rule]]
# app = \"com.example.terminal\"
# title = \"^Scratch\"
# action = \"float\"
# placement = \"center\"
";

/// The commented file written on first run and printed by `config --print-defaults`: every
/// key commented out with its default and a one-line explanation, so new built-in defaults
/// reach existing users.
pub fn default_file() -> String {
    let mut out = String::from(HEADER);
    for s in SECTIONS {
        out.push('\n');
        let _ = writeln!(out, "[{}]", s.name);
        for line in s.doc {
            let _ = writeln!(out, "# {line}");
        }
        for d in keys_of(s.name) {
            let _ = writeln!(out, "#");
            let _ = writeln!(out, "# {}", d.doc);
            let _ = writeln!(out, "# {} = {}", d.key, d.default);
        }
        if s.name == "defaults" {
            out.push('\n');
            out.push_str(RULES_DOC);
        }
    }
    out
}

/// `config --explain <key>`: the key's explanation, type, default and when it applies.
pub fn explain(path: &str) -> Option<String> {
    let d = find(path)?;
    Some(format!(
        "[{}] {}\n  {}\n  type: {}\n  default: {}\n  applies: {}\n",
        d.section,
        d.key,
        d.doc,
        d.kind.describe(),
        d.default,
        match d.applies {
            Applies::Live => "within a second of saving",
            Applies::Restart => "at the next launch",
        }
    ))
}

/// The configuration reference for README: one table row per key.
pub fn markdown_reference() -> String {
    let mut out = String::from("| Key | Default | Values | What it does |\n|---|---|---|---|\n");
    for d in KEYS {
        let _ = writeln!(
            out,
            "| `{}` | `{}` | {} | {} |",
            d.path(),
            d.default,
            d.kind.describe().replace('|', "\\|"),
            d.doc.replace('|', "\\|")
        );
    }
    out
}

/// Every default file an earlier version wrote, verbatim. A config file byte-identical to
/// one of them was never edited, so it is replaced with [`default_file`] at launch. When
/// [`default_file`] changes, its previous text is added here.
pub const LEGACY_DEFAULTS: &[&str] = &[
    include_str!("legacy/default-1.toml"),
    include_str!("legacy/default-2.toml"),
    include_str!("legacy/default-3.toml"),
    include_str!("legacy/default-4.toml"),
    include_str!("legacy/default-5.toml"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_belongs_to_a_known_section_and_is_unique() {
        for (n, d) in KEYS.iter().enumerate() {
            assert!(SECTIONS.iter().any(|s| s.name == d.section), "{}", d.path());
            assert!(!KEYS[..n].iter().any(|e| e.path() == d.path()), "duplicate {}", d.path());
            assert!(!d.doc.is_empty() && !d.doc.contains('\n'), "{}: one line", d.path());
        }
    }

    #[test]
    fn the_default_file_has_every_key_commented_out_and_nothing_live() {
        let text = default_file();
        for d in KEYS {
            assert!(text.contains(&format!("\n# {} = {}\n", d.key, d.default)), "{}", d.path());
        }
        let table: toml::Table = toml::from_str(&text).expect("parses");
        assert!(table.values().all(|v| v.as_table().is_some_and(|t| t.is_empty())), "no live values: {table:?}");
    }

    #[test]
    fn explain_names_type_default_and_doc() {
        let e = explain("windows.fit").expect("known");
        assert!(e.contains("\"off\"") && e.contains("type:") && e.contains("Apple"));
        assert!(explain("windows.nope").is_none());
        assert!(explain("nonsense").is_none());
    }

    #[test]
    fn legacy_defaults_are_distinct_from_the_current_file() {
        let now = default_file();
        assert!(LEGACY_DEFAULTS.iter().all(|t| *t != now));
        assert!(LEGACY_DEFAULTS.iter().all(|t| t.starts_with("# Stage Manager configuration.")));
    }
}
