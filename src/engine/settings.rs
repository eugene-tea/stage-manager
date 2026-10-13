//! macOS's own Stage Manager settings, interpreted.
//!
//! Apple's `com.apple.WindowManager` preferences are the source of truth wherever they exist.
//! The platform reads the raw values (with `CFPreferencesCopyAppValue`, re-read on change) and
//! hands them over as a map; [`NativeSettings::from_values`] turns them into behavior. Missing
//! keys take Apple's defaults. `config.toml` never repeats any of these.
//!
//! **Click wallpaper to reveal desktop** (System Settings → Desktop & Dock): Apple's guide
//! ("Change Desktop & Dock settings on Mac", macOS 27) offers "Always" (clicking the
//! wallpaper moves all windows out of the way) and "Only in Stage Manager". Since Sonoma the
//! setting ships as "Always", and `EnableStandardClickToShowDesktop` is written only once the
//! user picks a value (`false` = Only in Stage Manager), so an absent key means Always: macOS
//! itself moves every window aside on a wallpaper click while Apple's Stage Manager is off.
//!
//! | Key | Raw values | Field |
//! |---|---|---|
//! | `AppWindowGroupingBehavior` | 1 = all at once (default), 0 = one at a time | [`NativeSettings::app_windows`] |
//! | `AutoHide` | 1 = hide recent apps (strip auto-hides), 0 = show (default) | [`NativeSettings::hide_recent_apps`] |
//! | `HideDesktop` | 1 = hide desktop items, 0 = show (default) | [`NativeSettings::hide_desktop_items`] |
//! | `GloballyEnabled` | 1 = Apple's Stage Manager is on, 0 = off (default) | [`NativeSettings::stage_manager_enabled`] |
//! | `EnableStandardClickToShowDesktop` | 1 or absent = Always (default), 0 = Only in Stage Manager | [`NativeSettings::click_wallpaper_reveals_desktop`], [`NativeSettings::macos_reveals_desktop_outside_stage_manager`] |
//! | `StageManagerHideWidgets` | 1 = hide widgets in Stage Manager | [`NativeSettings::hide_widgets_in_stage_manager`] |
//! | `StandardHideWidgets` | 1 = hide widgets on the desktop | [`NativeSettings::hide_widgets_standard`] |

use std::collections::HashMap;

/// A raw preference value as the platform read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefValue {
    /// A numeric value (`CFNumber`).
    Int(i64),
    /// A boolean value (`CFBoolean`).
    Bool(bool),
}

impl PrefValue {
    /// The value as a boolean: any non-zero number is true.
    pub fn as_bool(self) -> bool {
        match self {
            PrefValue::Int(n) => n != 0,
            PrefValue::Bool(b) => b,
        }
    }
}

/// The preference domain the keys live in.
pub const DOMAIN: &str = "com.apple.WindowManager";

/// Every key [`NativeSettings::from_values`] reads, so the platform knows what to fetch.
pub const KEYS: &[&str] = &[
    "AppWindowGroupingBehavior",
    "AutoHide",
    "HideDesktop",
    "GloballyEnabled",
    "EnableStandardClickToShowDesktop",
    "StageManagerHideWidgets",
    "StandardHideWidgets",
];

/// "Show windows from an application" in System Settings → Desktop & Dock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppWindows {
    /// All of an app's windows show together (`AppWindowGroupingBehavior = 1`, Apple's
    /// default). Activating an app that already has a window in the active stage does nothing.
    #[default]
    AllAtOnce,
    /// Only the app's most recent window shows (`AppWindowGroupingBehavior = 0`). Activating
    /// an app follows its focused window to that window's stage even when another window of
    /// the app is in the active stage.
    OneAtATime,
}

/// Apple's Stage Manager settings, interpreted for this app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeSettings {
    /// How an app's windows group when the app is activated (`AppWindowGroupingBehavior`).
    pub app_windows: AppWindows,
    /// "Show recent apps in Stage Manager" turned off (`AutoHide = 1`): the strip stays hidden
    /// until the pointer reaches the left edge.
    pub hide_recent_apps: bool,
    /// "Show items … in Stage Manager" turned off (`HideDesktop = 1`). Read and reported; the
    /// app does not hide Finder's desktop items.
    pub hide_desktop_items: bool,
    /// Click wallpaper to reveal desktop. Apple's choices are "Always" and "Only in Stage
    /// Manager"; this app is the stage manager, so both mean yes and this is always true.
    pub click_wallpaper_reveals_desktop: bool,
    /// `EnableStandardClickToShowDesktop` 1 or absent ("Always", Apple's default): macOS
    /// itself reveals the desktop on a wallpaper click outside Apple's Stage Manager, so our
    /// own reveal must stay off (both at once would fight over the windows). Only with "Only
    /// in Stage Manager" (0) is the wallpaper click ours to answer.
    pub macos_reveals_desktop_outside_stage_manager: bool,
    /// Apple's own Stage Manager is on (`GloballyEnabled = 1`); both cannot run, so warn.
    pub stage_manager_enabled: bool,
    /// Widgets hidden in Stage Manager (`StageManagerHideWidgets = 1`). Not applicable; kept
    /// for display.
    pub hide_widgets_in_stage_manager: bool,
    /// Widgets hidden on the desktop (`StandardHideWidgets = 1`). Not applicable; kept for
    /// display.
    pub hide_widgets_standard: bool,
}

impl Default for NativeSettings {
    /// Apple's defaults, as if every key were absent.
    fn default() -> Self {
        Self::from_values(&HashMap::new())
    }
}

impl NativeSettings {
    /// Interprets raw `com.apple.WindowManager` values; missing keys take Apple's defaults.
    pub fn from_values(values: &HashMap<String, PrefValue>) -> Self {
        let flag = |key: &str| values.get(key).is_some_and(|v| v.as_bool());
        let app_windows = match values.get("AppWindowGroupingBehavior") {
            Some(v) if !v.as_bool() => AppWindows::OneAtATime,
            _ => AppWindows::AllAtOnce,
        };
        Self {
            app_windows,
            hide_recent_apps: flag("AutoHide"),
            hide_desktop_items: flag("HideDesktop"),
            click_wallpaper_reveals_desktop: true,
            macos_reveals_desktop_outside_stage_manager: values
                .get("EnableStandardClickToShowDesktop")
                .is_none_or(|v| v.as_bool()),
            stage_manager_enabled: flag("GloballyEnabled"),
            hide_widgets_in_stage_manager: flag("StageManagerHideWidgets"),
            hide_widgets_standard: flag("StandardHideWidgets"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, PrefValue)]) -> HashMap<String, PrefValue> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn missing_keys_take_apple_defaults() {
        let s = NativeSettings::default();
        assert_eq!(s.app_windows, AppWindows::AllAtOnce);
        assert!(!s.hide_recent_apps);
        assert!(!s.hide_desktop_items);
        assert!(s.click_wallpaper_reveals_desktop);
        assert!(
            s.macos_reveals_desktop_outside_stage_manager,
            "absent = Always (Apple's default since Sonoma): macOS reveals on its own"
        );
        assert!(!s.stage_manager_enabled);
    }

    #[test]
    fn his_mac_values() {
        let s = NativeSettings::from_values(&values(&[
            ("AppWindowGroupingBehavior", PrefValue::Int(1)),
            ("AutoHide", PrefValue::Int(0)),
            ("HideDesktop", PrefValue::Int(0)),
            ("GloballyEnabled", PrefValue::Bool(true)),
            ("StageManagerHideWidgets", PrefValue::Int(1)),
        ]));
        assert_eq!(s.app_windows, AppWindows::AllAtOnce);
        assert!(!s.hide_recent_apps);
        assert!(s.stage_manager_enabled);
        assert!(s.hide_widgets_in_stage_manager);
        assert!(!s.hide_widgets_standard);
        assert!(s.macos_reveals_desktop_outside_stage_manager, "his key is absent: Always");
    }

    #[test]
    fn one_at_a_time_and_hidden_strip() {
        let s = NativeSettings::from_values(&values(&[
            ("AppWindowGroupingBehavior", PrefValue::Int(0)),
            ("AutoHide", PrefValue::Bool(true)),
            ("HideDesktop", PrefValue::Int(1)),
            ("EnableStandardClickToShowDesktop", PrefValue::Int(1)),
        ]));
        assert_eq!(s.app_windows, AppWindows::OneAtATime);
        assert!(s.hide_recent_apps);
        assert!(s.hide_desktop_items);
        assert!(s.click_wallpaper_reveals_desktop);
        assert!(s.macos_reveals_desktop_outside_stage_manager);
    }

    #[test]
    fn only_in_stage_manager_leaves_the_wallpaper_click_to_us() {
        let s = NativeSettings::from_values(&values(&[(
            "EnableStandardClickToShowDesktop",
            PrefValue::Int(0),
        )]));
        assert!(s.click_wallpaper_reveals_desktop);
        assert!(!s.macos_reveals_desktop_outside_stage_manager);
        let s = NativeSettings::from_values(&values(&[("EnableStandardClickToShowDesktop", PrefValue::Bool(false))]));
        assert!(!s.macos_reveals_desktop_outside_stage_manager, "written as a boolean by System Settings");
        let s = NativeSettings::from_values(&values(&[("EnableStandardClickToShowDesktop", PrefValue::Bool(true))]));
        assert!(s.macos_reveals_desktop_outside_stage_manager);
    }
}
