//! How many Desktops macOS keeps for each screen, read from the Dock's own Spaces settings
//! (`com.apple.spaces`, `SpacesDisplayConfiguration` → `Management Data` → `Monitors`), read
//! only: never written, never synchronized.
//!
//! Each monitor entry names its screen by `Display Identifier`: `Main` for the main screen,
//! else the screen's UUID (`CGDisplayCreateUUIDFromDisplayID`, public in ColorSync), and
//! lists its Spaces; a Desktop is a Space of type 0 (full-screen apps' Spaces have another
//! type). A screen not connected keeps an entry with no `Spaces` (its `Collapsed Space`
//! only), and is left out. The settings name no Space the app could match to what it sees
//! (no public API says which Space shows; the main screen's first Desktop has an empty
//! `uuid`), so the count is all the app takes: a screen with one Desktop is the one Desktop
//! saved on it after a restart ([`crate::engine::model::Model::set_desktop_counts`]).

use std::ptr::NonNull;

use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFPreferencesCopyAppValue, CFRetained, CFString, CFType, CFUUID};
use objc2_core_graphics::CGMainDisplayID;

use crate::engine::displays::ScreenKey;
use crate::engine::model::Display;

#[link(name = "ColorSync", kind = "framework")]
unsafe extern "C" {
    /// ColorSync (public): the UUID macOS keeps for a display, the one `com.apple.spaces`
    /// names it by. A +1 reference, or null.
    fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> Option<NonNull<CFUUID>>;
}

/// One monitor entry: its `Display Identifier` and how many Desktops it lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorDesktops {
    /// `Main`, or the screen's UUID.
    pub identifier: String,
    /// Its Spaces of type 0.
    pub desktops: u32,
}

/// A connected screen as the settings can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedScreen {
    /// Its identity ([`ScreenKey`]).
    pub key: ScreenKey,
    /// Its UUID, when ColorSync gives one.
    pub uuid: Option<String>,
    /// The main screen (`Main` in the settings).
    pub main: bool,
}

/// The Desktop count of each screen in `screens` that a monitor entry names (by its UUID,
/// or `Main` for the main screen). A screen named twice takes the entry by its UUID.
pub fn counts_for(monitors: &[MonitorDesktops], screens: &[NamedScreen]) -> Vec<(ScreenKey, u32)> {
    screens
        .iter()
        .filter(|s| s.key.known())
        .filter_map(|s| {
            let by_uuid = s.uuid.as_deref().and_then(|u| monitors.iter().find(|m| m.identifier.eq_ignore_ascii_case(u)));
            let by_main = || s.main.then(|| monitors.iter().find(|m| m.identifier == "Main")).flatten();
            by_uuid.or_else(by_main).map(|m| (s.key, m.desktops))
        })
        .collect()
}

/// The monitor entries of `com.apple.spaces` (empty when the settings are absent or not as
/// expected).
pub fn read_monitors() -> Vec<MonitorDesktops> {
    let domain = CFString::from_static_str("com.apple.spaces");
    let Some(all) = CFPreferencesCopyAppValue(&CFString::from_static_str("SpacesDisplayConfiguration"), &domain) else {
        return Vec::new();
    };
    let Some(monitors) = dictionary(&all)
        .and_then(|d| d.get(&CFString::from_static_str("Management Data")))
        .and_then(|v| dictionary(&v))
        .and_then(|d| d.get(&CFString::from_static_str("Monitors")))
        .and_then(|v| array(&v))
    else {
        return Vec::new();
    };
    monitors
        .iter()
        .filter_map(|m| {
            let m = dictionary(&m)?;
            let identifier = m.get(&CFString::from_static_str("Display Identifier"))?.downcast::<CFString>().ok()?.to_string();
            let spaces = m.get(&CFString::from_static_str("Spaces")).and_then(|v| array(&v))?;
            let desktops = spaces
                .iter()
                .filter_map(|s| dictionary(&s))
                .filter(|s| s.get(&CFString::from_static_str("type")).and_then(|t| t.downcast_ref::<CFNumber>().and_then(|n| n.as_i64())) == Some(0))
                .count() as u32;
            (desktops > 0).then_some(MonitorDesktops { identifier, desktops })
        })
        .collect()
}

/// The UUID macOS keeps for display `id` (upper case, as the settings write it).
pub fn display_uuid(id: u32) -> Option<String> {
    // SAFETY: a plain C call with a display id; the result is a +1 CFUUID or null, owned by
    // the `CFRetained` made from it.
    let uuid = unsafe { CGDisplayCreateUUIDFromDisplayID(id) }.map(|p| unsafe { CFRetained::from_raw(p) })?;
    CFUUID::new_string(None, Some(&uuid)).map(|s| s.to_string().to_ascii_uppercase())
}

/// How many Desktops macOS keeps for each of `displays` it names (read only; see the module
/// docs).
pub fn desktop_counts(displays: &[Display]) -> Vec<(ScreenKey, u32)> {
    let main = CGMainDisplayID();
    let screens: Vec<NamedScreen> =
        displays.iter().map(|d| NamedScreen { key: d.key, uuid: display_uuid(d.id.0), main: d.id.0 == main }).collect();
    counts_for(&read_monitors(), &screens)
}

fn dictionary(value: &CFType) -> Option<CFRetained<CFDictionary<CFString, CFType>>> {
    let dict = value.downcast_ref::<CFDictionary>()?;
    // SAFETY: `dict` is a live dictionary (retained for the result); a preferences
    // dictionary has string keys and CF object values, each type-checked where it is read.
    Some(unsafe { CFRetained::cast_unchecked(CFRetained::retain(NonNull::from(dict))) })
}

fn array(value: &CFType) -> Option<CFRetained<CFArray<CFType>>> {
    let list = value.downcast_ref::<CFArray>()?;
    // SAFETY: `list` is a live array (retained for the result); a plist array holds CF
    // objects, each type-checked where it is read.
    Some(unsafe { CFRetained::cast_unchecked(CFRetained::retain(NonNull::from(list))) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAPTOP: ScreenKey = ScreenKey::new(0x610, 0xa05c, 0xfd62);
    const STUDIO: ScreenKey = ScreenKey::new(0x610, 0xae3a, 0x1e22);

    fn monitor(identifier: &str, desktops: u32) -> MonitorDesktops {
        MonitorDesktops { identifier: identifier.into(), desktops }
    }

    #[test]
    fn each_connected_screen_takes_its_entry_by_uuid_or_as_main() {
        // The owner's desk 11-06: the 5K is Main with two Desktops, the laptop one, a screen not
        // connected left out (no Spaces, so no entry here).
        let monitors = [monitor("Main", 2), monitor("37D8832A-2D66-02CA-B9F7-8F30A301B230", 1)];
        let screens = [
            NamedScreen { key: STUDIO, uuid: Some("ADADCA6B-546E-47C6-99E8-DC6FE524E131".into()), main: true },
            NamedScreen { key: LAPTOP, uuid: Some("37d8832a-2d66-02ca-b9f7-8f30a301b230".into()), main: false },
        ];
        assert_eq!(counts_for(&monitors, &screens), vec![(STUDIO, 2), (LAPTOP, 1)]);
    }

    #[test]
    fn a_screen_no_entry_names_is_unknown() {
        let monitors = [monitor("Main", 3)];
        let screens = [
            NamedScreen { key: STUDIO, uuid: None, main: false },
            NamedScreen { key: ScreenKey::NONE, uuid: None, main: true },
        ];
        assert!(counts_for(&monitors, &screens).is_empty(), "not main, no UUID; no identity");
    }
}
