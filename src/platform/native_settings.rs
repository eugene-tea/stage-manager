//! Apple's Stage Manager settings (`com.apple.WindowManager`), the source of truth wherever
//! they exist, and the one Dock setting that decides whether a Dock click changes Desktops
//! (`com.apple.dock` `workspaces-auto-swoosh`). Read with `CFPreferencesAppSynchronize` +
//! `CFPreferencesCopyAppValue` (never written), re-read every 2 s by the main thread, and
//! exposed as raw `key → i64` values for the engine to interpret.

use std::collections::BTreeMap;
use std::sync::RwLock;

use objc2_core_foundation::{
    CFBoolean, CFNumber, CFPreferencesAppSynchronize, CFPreferencesCopyAppValue, CFString,
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
/// with open windows for the application").
pub const DOCK_KEYS: [&str; 1] = [crate::engine::settings::SWITCH_TO_APP_SPACE];

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
    fn reading_the_real_domain_returns_known_keys_only() {
        let raw = read();
        assert!(raw.keys().all(|k| KEYS.contains(&k.as_str()) || DOCK_KEYS.contains(&k.as_str())));
    }
}
