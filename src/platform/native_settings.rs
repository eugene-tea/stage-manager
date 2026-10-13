//! Apple's Stage Manager settings (`com.apple.WindowManager`), the source of truth wherever
//! they exist, and the one Dock setting that decides whether a Dock click changes Desktops
//! (`com.apple.dock` `workspaces-auto-swoosh`, never written). Read with
//! `CFPreferencesAppSynchronize` + `CFPreferencesCopyAppValue`, re-read every second by the
//! main thread, and exposed as raw `key → i64` values for the engine to interpret.
//!
//! Writes ([`write`]) happen only on a user's click in Settings or a prompt: "Show recent
//! apps" (`AutoHide`) and "Show windows from an application" (`AppWindowGroupingBehavior`),
//! the same keys Apple's own Customize sheet writes, and turning Apple's Stage Manager off
//! (`GloballyEnabled`). Never `EnableStandardClickToShowDesktop` (it changes macOS beyond
//! Stage Manager). Unit tests and test runs (`STAGE_MANAGER_HOME`) never write, unless
//! `SM_TEST_TOGGLE_APPLE_SM=1` (the owner's opt-in live test).

use std::collections::BTreeMap;
use std::sync::RwLock;

use objc2_core_foundation::{
    CFBoolean, CFNumber, CFPreferencesAppSynchronize, CFPreferencesCopyAppValue, CFPreferencesSetAppValue, CFString,
};

/// The preferences domain of Apple's Stage Manager.
pub const DOMAIN: &str = "com.apple.WindowManager";

/// `1` = Apple's Stage Manager is on (it must be off while this app runs).
pub const GLOBALLY_ENABLED: &str = "GloballyEnabled";

/// Every key read from [`DOMAIN`]. Absent keys are absent from [`RawSettings`].
pub const KEYS: [&str; 7] = [
    "AppWindowGroupingBehavior",
    "AutoHide",
    "HideDesktop",
    GLOBALLY_ENABLED,
    "StageManagerHideWidgets",
    "StandardHideWidgets",
    "EnableStandardClickToShowDesktop",
];

/// The Dock's domain, for [`DOCK_KEYS`].
pub const DOCK_DOMAIN: &str = "com.apple.dock";

/// Every key read from [`DOCK_DOMAIN`] ("When switching to an application, switch to a Space
/// with open windows for the application"; the hot corners' actions, for the inert slivers'
/// Quick Note corner, [`crate::engine::sliver::quick_note_at`]).
pub const DOCK_KEYS: [&str; 5] = [
    crate::engine::settings::SWITCH_TO_APP_SPACE,
    "wvous-tl-corner",
    "wvous-tr-corner",
    "wvous-bl-corner",
    "wvous-br-corner",
];

/// Raw values: key → integer (booleans as 0/1). A missing key means "macOS default".
pub type RawSettings = BTreeMap<String, i64>;

static CURRENT: RwLock<RawSettings> = RwLock::new(BTreeMap::new());

/// Reads every key in [`KEYS`] and [`DOCK_KEYS`] fresh from disk.
pub fn read() -> RawSettings {
    let mut out = RawSettings::new();
    read_domain(DOMAIN, &KEYS, &mut out);
    read_domain(DOCK_DOMAIN, &DOCK_KEYS, &mut out);
    out
}

fn read_domain(domain: &'static str, keys: &[&'static str], out: &mut RawSettings) {
    let cf_domain = CFString::from_static_str(domain);
    CFPreferencesAppSynchronize(&cf_domain);
    for &key in keys {
        let cf_key = CFString::from_static_str(key);
        let Some(value) = CFPreferencesCopyAppValue(&cf_key, &cf_domain) else { continue };
        if let Some(b) = value.downcast_ref::<CFBoolean>() {
            out.insert(key.to_string(), i64::from(b.as_bool()));
        } else if let Some(n) = value.downcast_ref::<CFNumber>() {
            if let Some(v) = n.as_i64() {
                out.insert(key.to_string(), v);
            }
        } else {
            tracing::debug!(domain, key, "preference value has an unexpected type; ignored");
        }
    }
}

/// The last values read by [`refresh`].
pub fn current() -> RawSettings {
    CURRENT.read().map(|g| g.clone()).unwrap_or_default()
}

/// Re-reads the settings; logs and returns the new values when anything changed.
pub fn refresh() -> Option<RawSettings> {
    let fresh = read();
    let Ok(mut guard) = CURRENT.write() else { return None };
    if *guard == fresh {
        return None;
    }
    for line in describe_changes(&guard, &fresh) {
        tracing::info!("macOS setting {line}");
    }
    *guard = fresh.clone();
    Some(fresh)
}

/// Whether Apple's own Stage Manager is on.
pub fn apple_stage_manager_enabled(settings: &RawSettings) -> bool {
    settings.get(GLOBALLY_ENABLED).copied().unwrap_or(0) != 0
}

/// The keys [`write`] may change.
pub const WRITABLE: [&str; 3] = ["AutoHide", "AppWindowGroupingBehavior", GLOBALLY_ENABLED];

/// The owner's opt-in for a live test that toggles Apple's Stage Manager.
pub const TEST_TOGGLE_ENV: &str = "SM_TEST_TOGGLE_APPLE_SM";

/// Why this process may not write `com.apple.WindowManager`, or `None` when it may.
pub fn write_refusal() -> Option<&'static str> {
    if cfg!(test) {
        return Some("unit tests never write com.apple.WindowManager");
    }
    if super::paths::custom_home().is_some() && std::env::var_os(TEST_TOGGLE_ENV).is_none() {
        return Some("a test run (STAGE_MANAGER_HOME) never writes com.apple.WindowManager");
    }
    None
}

/// Writes one of [`WRITABLE`] (`AutoHide` and `GloballyEnabled` as booleans,
/// `AppWindowGroupingBehavior` as a number), synchronizes, and re-reads. Only on a user's
/// click.
pub fn write(key: &str, value: i64) -> Result<(), String> {
    if !WRITABLE.contains(&key) {
        return Err(format!("{key} is not written by Stage Manager"));
    }
    if let Some(why) = write_refusal() {
        return Err(why.to_string());
    }
    let domain = CFString::from_static_str(DOMAIN);
    let cf_key = CFString::from_str(key);
    if key == "AppWindowGroupingBehavior" {
        let n = CFNumber::new_i64(value);
        // SAFETY: a CFNumber is a property list value; key and domain are valid strings.
        unsafe { CFPreferencesSetAppValue(&cf_key, Some(&n), &domain) };
    } else {
        let b = CFBoolean::new(value != 0);
        // SAFETY: a CFBoolean is a property list value; key and domain are valid strings.
        unsafe { CFPreferencesSetAppValue(&cf_key, Some(b), &domain) };
    }
    if !CFPreferencesAppSynchronize(&domain) {
        return Err("macOS did not save the setting".into());
    }
    tracing::info!(key, value, "macOS setting written (the user's click)");
    Ok(())
}

/// Turns Apple's Stage Manager off (`GloballyEnabled = false`), on the user's click only.
pub fn turn_off_apple_stage_manager() -> Result<(), String> {
    write(GLOBALLY_ENABLED, 0)
}

/// On-screen windows of Apple's `WindowManager` process (layer 0 or above): its strip's
/// cards and their icons while Apple's Stage Manager runs. The public signal that it has
/// really stopped is this count falling to what it is with Stage Manager off (none outside
/// a wallpaper-click reveal) together with `GloballyEnabled` reading back 0.
pub fn window_manager_windows() -> usize {
    super::windows::list_windows().iter().filter(|w| w.onscreen && w.owner == "WindowManager").count()
}

/// Human-readable changes between two readings, one line per key (`Key: old → new`).
pub fn describe_changes(old: &RawSettings, new: &RawSettings) -> Vec<String> {
    let show = |v: Option<&i64>| v.map_or_else(|| "unset".to_string(), |v| v.to_string());
    KEYS.iter()
        .chain(DOCK_KEYS.iter())
        .filter(|k| old.get(**k) != new.get(**k))
        .map(|k| format!("{k}: {} → {}", show(old.get(*k)), show(new.get(*k))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(pairs: &[(&str, i64)]) -> RawSettings {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn changes_list_only_moved_keys() {
        let old = s(&[("AutoHide", 0), ("HideDesktop", 1)]);
        let new = s(&[("AutoHide", 1), ("HideDesktop", 1), (GLOBALLY_ENABLED, 0)]);
        assert_eq!(describe_changes(&old, &new), vec!["AutoHide: 0 → 1", "GloballyEnabled: unset → 0"]);
        assert!(describe_changes(&new, &new).is_empty());
    }

    #[test]
    fn globally_enabled_means_apple_stage_manager_is_on() {
        assert!(apple_stage_manager_enabled(&s(&[(GLOBALLY_ENABLED, 1)])));
        assert!(!apple_stage_manager_enabled(&s(&[(GLOBALLY_ENABLED, 0)])));
        assert!(!apple_stage_manager_enabled(&RawSettings::new()));
    }

    #[test]
    fn unit_tests_never_write_apples_settings() {
        assert!(write_refusal().is_some());
        assert!(write("AutoHide", 1).is_err());
        assert!(turn_off_apple_stage_manager().is_err());
        assert!(write("EnableStandardClickToShowDesktop", 0).is_err(), "never written");
    }

    #[test]
    fn reading_the_real_domain_returns_known_keys_only() {
        let raw = read();
        assert!(raw.keys().all(|k| KEYS.contains(&k.as_str()) || DOCK_KEYS.contains(&k.as_str())));
    }
}
