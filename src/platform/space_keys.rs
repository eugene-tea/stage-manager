//! Test support: the Desktop switch keys, posted the way a person types them.
//!
//! macOS switches to the neighbouring Desktop on "Move left a space" / "Move right a space"
//! (Keyboard Shortcuts → Mission Control; symbolic hotkeys 79 and 81, Control-← and
//! Control-→ by default). The hands-free Spaces check (`ctl debug-space`, and the one-shot
//! `stage-manager debug-post-space` for moments when the app is not running) posts that key
//! at the HID event tap from the app, which has Accessibility. Public API only: the key is
//! read from the user's own `com.apple.symbolichotkeys` settings (so a remapped shortcut is
//! honoured) and posted with `CGEventPost`; nothing asks the window server for Spaces.

use std::ptr::NonNull;
use std::time::Duration;

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFPreferencesAppSynchronize, CFPreferencesCopyAppValue, CFRetained,
    CFString, CFType,
};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation};
use serde::Serialize;

/// Which neighbour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// The Desktop to the left ("Move left a space").
    Left,
    /// The Desktop to the right ("Move right a space").
    Right,
}

impl Direction {
    /// `left` or `right`.
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }

    /// `left` or `right`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    /// The symbolic hotkey's id in `com.apple.symbolichotkeys`.
    pub fn hotkey_id(self) -> u32 {
        match self {
            Self::Left => 79,
            Self::Right => 81,
        }
    }

    /// The arrow's virtual key code (the default shortcut's key).
    fn default_keycode(self) -> u16 {
        match self {
            Self::Left => 123,
            Self::Right => 124,
        }
    }
}

/// Control plus Fn, as macOS stores the default shortcut's modifiers (arrow keys carry the
/// Fn flag).
pub const DEFAULT_MODIFIERS: u64 = 0x84_0000;

/// The Control modifier bit (the same in `NSEvent` and `CGEvent` flags).
const CONTROL: u64 = 0x4_0000;

/// The key code of the left Control key.
const CONTROL_KEY: u16 = 59;

/// A shortcut as the user's settings have it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceKey {
    /// Virtual key code.
    pub keycode: u16,
    /// Modifier flags (`CGEventFlags` bits).
    pub modifiers: u64,
    /// Turned on in Keyboard Shortcuts (absent means on).
    pub enabled: bool,
    /// True when the settings named the key; false for the macOS default.
    pub from_settings: bool,
}

impl SpaceKey {
    /// From a hotkey entry's `enabled` and `value.parameters` (`[character, keycode,
    /// modifiers]`); anything missing or malformed is the macOS default.
    pub fn from_parameters(direction: Direction, enabled: Option<bool>, parameters: Option<&[i64]>) -> Self {
        let named = parameters.and_then(|p| match p {
            [_, code, mods] if (0..=0xFFFF).contains(code) && *mods >= 0 => Some((*code as u16, *mods as u64)),
            _ => None,
        });
        Self {
            keycode: named.map_or(direction.default_keycode(), |n| n.0),
            modifiers: named.map_or(DEFAULT_MODIFIERS, |n| n.1),
            enabled: enabled.unwrap_or(true),
            from_settings: named.is_some(),
        }
    }

    /// Reads the shortcut from `com.apple.symbolichotkeys`.
    pub fn read(direction: Direction) -> Self {
        let (enabled, parameters) = read_entry(direction.hotkey_id());
        Self::from_parameters(direction, enabled, parameters.as_deref())
    }

    /// Short description for replies and logs (`ctrl+fn+124`).
    pub fn describe(&self) -> String {
        let names = [(0x2_0000, "shift"), (CONTROL, "ctrl"), (0x8_0000, "alt"), (0x10_0000, "cmd"), (0x80_0000, "fn")];
        let mut parts: Vec<String> =
            names.iter().filter(|(bit, _)| self.modifiers & bit != 0).map(|(_, n)| n.to_string()).collect();
        parts.push(self.keycode.to_string());
        parts.join("+")
    }
}

/// `enabled` and `value.parameters` of one symbolic hotkey, as far as they are there.
fn read_entry(id: u32) -> (Option<bool>, Option<Vec<i64>>) {
    let domain = CFString::from_static_str("com.apple.symbolichotkeys");
    CFPreferencesAppSynchronize(&domain);
    let Some(all) = CFPreferencesCopyAppValue(&CFString::from_static_str("AppleSymbolicHotKeys"), &domain) else {
        return (None, None);
    };
    let Some(entry) = dictionary(&all).and_then(|d| d.get(&CFString::from_str(&id.to_string()))) else {
        return (None, None);
    };
    let Some(entry) = dictionary(&entry) else { return (None, None) };
    let enabled = entry.get(&CFString::from_static_str("enabled")).and_then(|v| boolean(&v));
    let parameters = entry
        .get(&CFString::from_static_str("value"))
        .and_then(|v| dictionary(&v))
        .and_then(|v| v.get(&CFString::from_static_str("parameters")))
        .and_then(|p| p.downcast::<CFArray>().ok())
        .map(|array| {
            // SAFETY: a plist array holds CF objects; each item is type-checked below.
            let array: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(array) };
            array.iter().filter_map(|n| n.downcast_ref::<CFNumber>().and_then(|n| n.as_i64())).collect()
        });
    (enabled, parameters)
}

fn dictionary(value: &CFType) -> Option<CFRetained<CFDictionary<CFString, CFType>>> {
    let dict = value.downcast_ref::<CFDictionary>()?;
    // SAFETY: `dict` is a live dictionary (retained for the result); a preferences
    // dictionary has string keys and CF object values, each type-checked where it is read.
    Some(unsafe { CFRetained::cast_unchecked(CFRetained::retain(NonNull::from(dict))) })
}

fn boolean(value: &CFType) -> Option<bool> {
    if let Some(b) = value.downcast_ref::<CFBoolean>() {
        return Some(b.as_bool());
    }
    value.downcast_ref::<CFNumber>().and_then(|n| n.as_i64()).map(|n| n != 0)
}

fn post_key(keycode: u16, down: bool, flags: u64) -> Result<(), String> {
    let event = CGEvent::new_keyboard_event(None, keycode, down).ok_or("cannot create a key event")?;
    CGEvent::set_flags(Some(&event), CGEventFlags(flags));
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

/// Posts the shortcut at the HID tap: Control down, the key down and up with the shortcut's
/// modifiers, Control up with no flags (so the session's modifier state is clean after).
/// `numpad` adds the numeric-pad flag a hardware arrow key carries.
pub fn post(key: &SpaceKey, numpad: bool) -> Result<(), String> {
    if !key.enabled {
        return Err(format!("the shortcut {} is turned off in Keyboard Shortcuts", key.describe()));
    }
    let flags = key.modifiers | if numpad { CGEventFlags::MaskNumericPad.0 } else { 0 };
    let pause = Duration::from_millis(30);
    let control = key.modifiers & CONTROL != 0;
    if control {
        post_key(CONTROL_KEY, true, CONTROL)?;
        std::thread::sleep(pause);
    }
    let result = post_key(key.keycode, true, flags).and_then(|_| {
        std::thread::sleep(pause);
        post_key(key.keycode, false, flags)
    });
    std::thread::sleep(pause);
    if control {
        post_key(CONTROL_KEY, false, 0)?;
    }
    result
}

/// The Space shown as the engine sees it (`ctl space`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SpaceView {
    /// The engine's synthetic id of the Space shown.
    pub space: u64,
    /// A full-screen app's Space shows.
    pub fullscreen: bool,
    /// A change waits for a full-screen animation to settle.
    pub settling: bool,
    /// Space changes handled since launch.
    pub changes: u64,
}

impl SpaceView {
    /// From a `ctl space` reply.
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        if v["ok"] != true {
            return None;
        }
        Some(Self {
            space: v["space"].as_u64()?,
            fullscreen: v["fullscreen"].as_bool()?,
            settling: v["settling"].as_bool()?,
            changes: v["changes"].as_u64()?,
        })
    }

    /// True once the engine has handled a Space change since `before` and it has settled:
    /// a change was counted, or the Space or its full-screen state differ.
    pub fn moved_from(&self, before: &Self) -> bool {
        !self.settling
            && (self.changes > before.changes || self.space != before.space || self.fullscreen != before.fullscreen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_mean_the_macos_default() {
        let k = SpaceKey::from_parameters(Direction::Right, None, None);
        assert_eq!(k, SpaceKey { keycode: 124, modifiers: DEFAULT_MODIFIERS, enabled: true, from_settings: false });
        assert_eq!(k.describe(), "ctrl+fn+124");
        let k = SpaceKey::from_parameters(Direction::Left, Some(true), None);
        assert_eq!((k.keycode, k.enabled), (123, true));
    }

    #[test]
    fn named_parameters_win() {
        // Option-Shift-L, say.
        let k = SpaceKey::from_parameters(Direction::Left, Some(true), Some(&[108, 37, 0xA_0000]));
        assert_eq!((k.keycode, k.modifiers, k.from_settings), (37, 0xA_0000, true));
        assert_eq!(k.describe(), "shift+alt+37");
    }

    #[test]
    fn malformed_parameters_fall_back() {
        for p in [&[1_i64, 2][..], &[65535, -1, 0x84_0000], &[65535, 124, -5], &[]] {
            let k = SpaceKey::from_parameters(Direction::Right, None, Some(p));
            assert_eq!((k.keycode, k.modifiers, k.from_settings), (124, DEFAULT_MODIFIERS, false), "{p:?}");
        }
    }

    #[test]
    fn a_turned_off_shortcut_is_never_posted() {
        let k = SpaceKey::from_parameters(Direction::Right, Some(false), None);
        assert!(!k.enabled);
        assert!(post(&k, false).unwrap_err().contains("turned off"));
    }

    #[test]
    fn a_space_change_counts_once_settled() {
        let before = SpaceView { space: 1, fullscreen: false, settling: false, changes: 3 };
        assert!(!before.moved_from(&before), "nothing happened (the edge of the row of Desktops)");
        let settling = SpaceView { settling: true, changes: 4, ..before };
        assert!(!settling.moved_from(&before), "still settling");
        assert!(SpaceView { settling: false, ..settling }.moved_from(&before), "an empty Desktop to another keeps its id");
        assert!(SpaceView { space: 2, ..before }.moved_from(&before));
        assert!(SpaceView { fullscreen: true, ..before }.moved_from(&before));
    }

    #[test]
    fn space_views_come_from_ctl_replies() {
        let v = serde_json::json!({ "ok": true, "space": 2, "fullscreen": false, "settling": false, "changes": 7 });
        assert_eq!(
            SpaceView::from_json(&v),
            Some(SpaceView { space: 2, fullscreen: false, settling: false, changes: 7 })
        );
        assert_eq!(SpaceView::from_json(&serde_json::json!({ "ok": false, "error": "x" })), None);
        assert_eq!(SpaceView::from_json(&serde_json::json!({ "ok": true, "space": 2 })), None);
    }

    #[test]
    fn directions_parse() {
        assert_eq!(Direction::parse("left"), Some(Direction::Left));
        assert_eq!(Direction::parse("right").map(Direction::hotkey_id), Some(81));
        assert_eq!(Direction::parse("up"), None);
    }

    #[test]
    fn reading_the_real_settings_never_fails() {
        for d in [Direction::Left, Direction::Right] {
            let k = SpaceKey::read(d);
            assert!(k.keycode > 0);
        }
    }
}
