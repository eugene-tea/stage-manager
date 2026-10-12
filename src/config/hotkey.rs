//! Hotkeys as pure data: key names and modifiers, no macOS key codes (the platform maps them).
//!
//! Text form: modifiers and a key joined by `+`, case-insensitive, for example
//! `"ctrl+alt+n"` or `"cmd+shift+f5"`. Modifiers: `ctrl`/`control`/`⌃`, `alt`/`opt`/`option`/`⌥`,
//! `shift`/`⇧`, `cmd`/`command`/`⌘`. A numbered hotkey uses `{n}` as its key and expands to the
//! digits 1–9. An empty string or `"none"` disables a hotkey.

use std::fmt;

/// Modifier keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    /// Control (⌃).
    pub ctrl: bool,
    /// Option (⌥).
    pub alt: bool,
    /// Shift (⇧).
    pub shift: bool,
    /// Command (⌘).
    pub cmd: bool,
}

impl Mods {
    /// True when no modifier is set.
    pub fn is_empty(&self) -> bool {
        !(self.ctrl || self.alt || self.shift || self.cmd)
    }
}

/// A key by name (US layout positions; the platform maps names to virtual key codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// `a`–`z`, stored lowercase.
    Letter(char),
    /// `0`–`9` on the main row.
    Digit(u8),
    /// `f1`–`f20`.
    F(u8),
    /// `space`.
    Space,
    /// `return` (or `enter`).
    Return,
    /// `tab`.
    Tab,
    /// `escape` (or `esc`).
    Escape,
    /// `delete` (or `backspace`).
    Delete,
    /// `forwarddelete`.
    ForwardDelete,
    /// `left`.
    Left,
    /// `right`.
    Right,
    /// `up`.
    Up,
    /// `down`.
    Down,
    /// `home`.
    Home,
    /// `end`.
    End,
    /// `pageup`.
    PageUp,
    /// `pagedown`.
    PageDown,
    /// `minus` or `-`.
    Minus,
    /// `equal` or `=`.
    Equal,
    /// `leftbracket` or `[`.
    LeftBracket,
    /// `rightbracket` or `]`.
    RightBracket,
    /// `backslash` or `\`.
    Backslash,
    /// `semicolon` or `;`.
    Semicolon,
    /// `quote` or `'`.
    Quote,
    /// `comma` or `,`.
    Comma,
    /// `period` or `.`.
    Period,
    /// `slash` or `/`.
    Slash,
    /// `grave` or `` ` ``.
    Grave,
}

const NAMED: &[(&str, Key)] = &[
    ("space", Key::Space),
    ("return", Key::Return),
    ("enter", Key::Return),
    ("tab", Key::Tab),
    ("escape", Key::Escape),
    ("esc", Key::Escape),
    ("delete", Key::Delete),
    ("backspace", Key::Delete),
    ("forwarddelete", Key::ForwardDelete),
    ("left", Key::Left),
    ("right", Key::Right),
    ("up", Key::Up),
    ("down", Key::Down),
    ("home", Key::Home),
    ("end", Key::End),
    ("pageup", Key::PageUp),
    ("pagedown", Key::PageDown),
    ("minus", Key::Minus),
    ("-", Key::Minus),
    ("equal", Key::Equal),
    ("=", Key::Equal),
    ("leftbracket", Key::LeftBracket),
    ("[", Key::LeftBracket),
    ("rightbracket", Key::RightBracket),
    ("]", Key::RightBracket),
    ("backslash", Key::Backslash),
    ("\\", Key::Backslash),
    ("semicolon", Key::Semicolon),
    (";", Key::Semicolon),
    ("quote", Key::Quote),
    ("'", Key::Quote),
    ("comma", Key::Comma),
    (",", Key::Comma),
    ("period", Key::Period),
    (".", Key::Period),
    ("slash", Key::Slash),
    ("/", Key::Slash),
    ("grave", Key::Grave),
    ("`", Key::Grave),
];

impl Key {
    /// Parses a key name (lowercase or not).
    pub fn parse(name: &str) -> Result<Key, String> {
        let n = name.trim().to_lowercase();
        let mut chars = n.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_lowercase() {
                return Ok(Key::Letter(c));
            }
            if let Some(d) = c.to_digit(10) {
                return Ok(Key::Digit(d as u8));
            }
        }
        if let Some(num) = n.strip_prefix('f')
            && let Ok(k) = num.parse::<u8>()
            && (1..=20).contains(&k)
        {
            return Ok(Key::F(k));
        }
        NAMED
            .iter()
            .find(|(s, _)| *s == n)
            .map(|(_, k)| *k)
            .ok_or_else(|| format!("unknown key {name:?}"))
    }

    /// The canonical name.
    pub fn name(&self) -> String {
        match self {
            Key::Letter(c) => c.to_string(),
            Key::Digit(d) => d.to_string(),
            Key::F(k) => format!("f{k}"),
            other => NAMED
                .iter()
                .find(|(_, k)| k == other)
                .map(|(s, _)| s.to_string())
                .unwrap_or_default(),
        }
    }
}

/// A key with modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hotkey {
    /// The key.
    pub key: Key,
    /// The modifiers.
    pub mods: Mods,
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.mods.ctrl, "ctrl+"),
            (self.mods.alt, "alt+"),
            (self.mods.shift, "shift+"),
            (self.mods.cmd, "cmd+"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        f.write_str(&self.key.name())
    }
}

/// A hotkey for stages 1–9: the modifiers plus each digit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NumberedHotkey {
    /// The modifiers shared by all nine keys.
    pub mods: Mods,
}

impl NumberedHotkey {
    /// The hotkey for stage `n` (1–9).
    pub fn for_number(&self, n: u8) -> Hotkey {
        Hotkey {
            key: Key::Digit(n),
            mods: self.mods,
        }
    }
}

/// What a hotkey does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyCommand {
    /// Switch to stage N.
    SwitchTo(usize),
    /// Send the focused window to stage N.
    SendTo(usize),
    /// Send the focused window to a new stage.
    NewStage,
    /// Pin or unpin the focused window.
    TogglePin,
    /// Show or hide the strip.
    ToggleStrip,
}

/// The `[hotkeys]` section; `None` disables a binding. Nothing is registered unless
/// `enabled` (Apple's Stage Manager has no hotkeys, and these take combinations other apps
/// may use).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyConfig {
    /// Register the bindings at all (default off).
    pub enabled: bool,
    /// Switch to stage N (default `ctrl+alt+{n}`).
    pub switch: Option<NumberedHotkey>,
    /// Send the focused window to stage N (default `ctrl+alt+shift+{n}`).
    pub send: Option<NumberedHotkey>,
    /// Send the focused window to a new stage (default `ctrl+alt+n`).
    pub new_stage: Option<Hotkey>,
    /// Toggle pin (default `ctrl+alt+p`).
    pub toggle_pin: Option<Hotkey>,
    /// Toggle the strip (default `ctrl+alt+s`).
    pub toggle_strip: Option<Hotkey>,
}

const CTRL_ALT: Mods = Mods {
    ctrl: true,
    alt: true,
    shift: false,
    cmd: false,
};

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            switch: Some(NumberedHotkey { mods: CTRL_ALT }),
            send: Some(NumberedHotkey {
                mods: Mods {
                    shift: true,
                    ..CTRL_ALT
                },
            }),
            new_stage: Some(Hotkey {
                key: Key::Letter('n'),
                mods: CTRL_ALT,
            }),
            toggle_pin: Some(Hotkey {
                key: Key::Letter('p'),
                mods: CTRL_ALT,
            }),
            toggle_strip: Some(Hotkey {
                key: Key::Letter('s'),
                mods: CTRL_ALT,
            }),
        }
    }
}

impl HotkeyConfig {
    /// Every binding the platform should register (none unless enabled), numbered ones
    /// expanded to 1–9.
    pub fn bindings(&self) -> Vec<(Hotkey, HotkeyCommand)> {
        if self.enabled { self.all_bindings() } else { Vec::new() }
    }

    /// Every configured binding, enabled or not, numbered ones expanded to 1–9.
    pub fn all_bindings(&self) -> Vec<(Hotkey, HotkeyCommand)> {
        let mut out = Vec::new();
        for n in 1..=9u8 {
            if let Some(s) = self.switch {
                out.push((s.for_number(n), HotkeyCommand::SwitchTo(n.into())));
            }
            if let Some(s) = self.send {
                out.push((s.for_number(n), HotkeyCommand::SendTo(n.into())));
            }
        }
        for (key, cmd) in [
            (self.new_stage, HotkeyCommand::NewStage),
            (self.toggle_pin, HotkeyCommand::TogglePin),
            (self.toggle_strip, HotkeyCommand::ToggleStrip),
        ] {
            if let Some(k) = key {
                out.push((k, cmd));
            }
        }
        out
    }

    /// Hotkeys bound to more than one command, as readable warnings.
    pub fn conflicts(&self) -> Vec<String> {
        let b = self.all_bindings();
        let mut out = Vec::new();
        for (i, (k, c)) in b.iter().enumerate() {
            if let Some((_, other)) = b[..i].iter().find(|(k2, _)| k2 == k) {
                out.push(format!(
                    "hotkey {k} is bound to both {other:?} and {c:?}; the first wins"
                ));
            }
        }
        out
    }
}

/// Either a fixed hotkey or a `{n}` template.
enum Parsed {
    Off,
    Key(Hotkey),
    Numbered(Mods),
}

fn parse_any(text: &str) -> Result<Parsed, String> {
    let t = text.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Ok(Parsed::Off);
    }
    let parts: Vec<&str> = t.split('+').map(str::trim).collect();
    // A trailing "+" means the key itself is "+", which is not supported; report clearly.
    if parts.iter().any(|p| p.is_empty()) {
        return Err(format!(
            "hotkey {text:?}: empty part (keys are joined with +)"
        ));
    }
    let (key, mod_names) = parts
        .split_last()
        .ok_or_else(|| format!("hotkey {text:?} is empty"))?;
    let mut mods = Mods::default();
    for m in mod_names {
        let slot = match m.to_lowercase().as_str() {
            "ctrl" | "control" | "⌃" => &mut mods.ctrl,
            "alt" | "opt" | "option" | "⌥" => &mut mods.alt,
            "shift" | "⇧" => &mut mods.shift,
            "cmd" | "command" | "⌘" => &mut mods.cmd,
            other => return Err(format!("hotkey {text:?}: unknown modifier {other:?}")),
        };
        *slot = true;
    }
    if *key == "{n}" {
        if mods.is_empty() {
            return Err(format!(
                "hotkey {text:?}: digits need at least one modifier"
            ));
        }
        return Ok(Parsed::Numbered(mods));
    }
    let key = Key::parse(key).map_err(|e| format!("hotkey {text:?}: {e}"))?;
    if mods.is_empty() && !matches!(key, Key::F(_)) {
        return Err(format!("hotkey {text:?}: needs at least one modifier"));
    }
    Ok(Parsed::Key(Hotkey { key, mods }))
}

/// Parses a fixed hotkey such as `"ctrl+alt+n"`; `Ok(None)` when disabled.
pub fn parse_hotkey(text: &str) -> Result<Option<Hotkey>, String> {
    match parse_any(text)? {
        Parsed::Off => Ok(None),
        Parsed::Key(k) => Ok(Some(k)),
        Parsed::Numbered(_) => Err(format!(
            "hotkey {text:?}: {{n}} is only for switch and send"
        )),
    }
}

/// Parses a numbered hotkey such as `"ctrl+alt+{n}"`; `Ok(None)` when disabled.
pub fn parse_numbered(text: &str) -> Result<Option<NumberedHotkey>, String> {
    match parse_any(text)? {
        Parsed::Off => Ok(None),
        Parsed::Numbered(mods) => Ok(Some(NumberedHotkey { mods })),
        Parsed::Key(_) => Err(format!("hotkey {text:?}: must end in +{{n}}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_keys() {
        let k = parse_hotkey("Ctrl+Option+N").expect("ok").expect("on");
        assert_eq!(
            k,
            Hotkey {
                key: Key::Letter('n'),
                mods: CTRL_ALT
            }
        );
        let k = parse_hotkey("⌘+⇧+f5").expect("ok").expect("on");
        assert!(k.mods.cmd && k.mods.shift && !k.mods.ctrl);
        assert_eq!(k.key, Key::F(5));
        assert_eq!(
            parse_hotkey("cmd+`").expect("ok").map(|k| k.key),
            Some(Key::Grave)
        );
        assert_eq!(
            parse_hotkey("alt+space").expect("ok").map(|k| k.key),
            Some(Key::Space)
        );
    }

    #[test]
    fn numbered_templates() {
        let n = parse_numbered("ctrl+alt+shift+{n}")
            .expect("ok")
            .expect("on");
        assert_eq!(
            n.for_number(3),
            Hotkey {
                key: Key::Digit(3),
                mods: Mods {
                    shift: true,
                    ..CTRL_ALT
                }
            }
        );
        assert!(parse_numbered("ctrl+alt+n").is_err());
        assert!(parse_hotkey("ctrl+{n}").is_err());
        assert!(parse_numbered("{n}").is_err());
    }

    #[test]
    fn disabled_and_bad_hotkeys() {
        assert_eq!(parse_hotkey("").expect("ok"), None);
        assert_eq!(parse_numbered("none").expect("ok"), None);
        assert!(parse_hotkey("ctrl+hyper+a").is_err());
        assert!(parse_hotkey("ctrl+banana").is_err());
        assert!(
            parse_hotkey("a").is_err(),
            "a bare letter would steal typing"
        );
        assert!(
            parse_hotkey("f13").expect("ok").is_some(),
            "bare function keys are fine"
        );
        assert!(parse_hotkey("ctrl++").is_err());
        assert!(parse_hotkey("f21").is_err());
    }

    #[test]
    fn display_round_trips() {
        for text in [
            "ctrl+alt+n",
            "alt+shift+cmd+f12",
            "ctrl+leftbracket",
            "cmd+7",
        ] {
            let k = parse_hotkey(text).expect("ok").expect("on");
            assert_eq!(parse_hotkey(&k.to_string()).expect("ok"), Some(k));
        }
        assert_eq!(
            parse_hotkey("ctrl+[").expect("ok").expect("on").to_string(),
            "ctrl+leftbracket"
        );
    }

    #[test]
    fn default_bindings_expand_digits() {
        let b = HotkeyConfig::default().all_bindings();
        assert!(HotkeyConfig::default().bindings().is_empty(), "off by default");
        assert_eq!(b.len(), 9 * 2 + 3);
        assert!(b.contains(&(
            Hotkey {
                key: Key::Digit(9),
                mods: CTRL_ALT
            },
            HotkeyCommand::SwitchTo(9)
        )));
        assert!(HotkeyConfig::default().conflicts().is_empty());
    }

    #[test]
    fn conflicts_are_reported() {
        let cfg = HotkeyConfig {
            toggle_pin: Some(Hotkey {
                key: Key::Digit(1),
                mods: CTRL_ALT,
            }),
            ..HotkeyConfig::default()
        };
        assert_eq!(cfg.conflicts().len(), 1);
    }
}
