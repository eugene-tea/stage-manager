//! The window server's view: `CGWindowListCopyWindowInfo` front to back, exclusions and
//! z-order.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;

use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFIndex, CFNumber, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    CGWindowListCopyWindowInfo, CGWindowListCreateDescriptionFromArray, CGWindowListOption, kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds,
    kCGWindowIsOnscreen, kCGWindowLayer, kCGWindowName, kCGWindowNumber, kCGWindowOwnerName,
    kCGWindowOwnerPID,
};
use serde::Serialize;

use crate::engine::geometry::Rect;

/// Apps whose windows are never managed.
pub const EXCLUDED_BUNDLE_IDS: [&str; 5] = [
    "com.apple.dock",
    "com.apple.WindowManager",
    "com.apple.controlcenter",
    "com.apple.notificationcenterui",
    "com.apple.Spotlight",
];

/// Finder: its desktop window is excluded (it sits below layer 0 and is also filtered by
/// `kCGWindowListExcludeDesktopElements`).
pub const FINDER_BUNDLE_ID: &str = "com.apple.finder";

/// One window as the window server reports it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CgWindow {
    /// `CGWindowID`.
    pub id: u32,
    /// Owning process.
    pub pid: i32,
    /// Owning app's name (may be empty).
    pub owner: String,
    /// Window name; only present with Screen Recording granted.
    pub name: Option<String>,
    /// Window layer; normal app windows are at 0.
    pub layer: i64,
    /// Bounds, top-left origin.
    pub bounds: Rect,
    /// Whether the window is onscreen.
    pub onscreen: bool,
    /// Window alpha (0 = fully transparent).
    pub alpha: f64,
}

/// Every window, desktop elements excluded: the onscreen ones front to back, then the
/// offscreen ones.
///
/// Two queries, because only the onscreen-only list is ordered front to back: the "all"
/// list puts windows created later (our own strip panels, say) after older windows they
/// cover, which once made a test's safety check see a covered window as topmost.
pub fn list_windows() -> Vec<CgWindow> {
    let mut out = query(CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements);
    let seen: HashSet<u32> = out.iter().map(|w| w.id).collect();
    out.extend(
        query(CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements)
            .into_iter()
            .filter(|w| !seen.contains(&w.id)),
    );
    out
}

/// The windows `ids` as the window server describes them now, on screen or not, on any
/// Desktop (`CGWindowListCreateDescriptionFromArray`): one query for a few windows, no walk
/// of every window as [`list_windows`] does. An id the window server does not know is left
/// out.
pub fn describe(ids: &[u32]) -> Vec<CgWindow> {
    if ids.is_empty() {
        return Vec::new();
    }
    // The array holds the CGWindowIDs themselves as its values (no callbacks), as the call
    // expects.
    let mut values: Vec<*const c_void> = ids.iter().map(|&id| id as usize as *const c_void).collect();
    let Ok(count) = CFIndex::try_from(values.len()) else { return Vec::new() };
    // SAFETY: `values` holds `count` pointer-sized values and outlives the call; null
    // callbacks make CF store them as they are, never retained or released.
    let Some(array) = (unsafe { CFArray::new(None, values.as_mut_ptr(), count, std::ptr::null()) }) else {
        tracing::warn!("CFArrayCreate returned nothing for a window description");
        return Vec::new();
    };
    // SAFETY: an array of CGWindowIDs, as the call is documented to take.
    let Some(described) = (unsafe { CGWindowListCreateDescriptionFromArray(Some(&array)) }) else {
        return Vec::new();
    };
    // SAFETY: the description is documented as an array of CFDictionary; items are
    // type-checked below.
    let described: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(described) };
    described.iter().filter_map(|item| parse_window(&item)).collect()
}

fn query(options: CGWindowListOption) -> Vec<CgWindow> {
    let Some(array) = CGWindowListCopyWindowInfo(options, kCGNullWindowID) else {
        tracing::warn!("CGWindowListCopyWindowInfo returned nothing");
        return Vec::new();
    };
    // SAFETY: the window list is documented as an array of CFDictionary; items are
    // type-checked below.
    let array: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(array) };
    array.iter().filter_map(|item| parse_window(&item)).collect()
}

fn parse_window(item: &CFType) -> Option<CgWindow> {
    // SAFETY: window-info dictionaries map CFString keys to CF values.
    let dict = unsafe { as_string_keyed(item.downcast_ref::<CFDictionary>()?) };
    // SAFETY: the kCGWindow* keys are valid framework statics.
    let (k_id, k_pid, k_owner, k_name, k_layer, k_bounds, k_onscreen, k_alpha) = unsafe {
        (
            kCGWindowNumber,
            kCGWindowOwnerPID,
            kCGWindowOwnerName,
            kCGWindowName,
            kCGWindowLayer,
            kCGWindowBounds,
            kCGWindowIsOnscreen,
            kCGWindowAlpha,
        )
    };
    let number = |k: &CFString| dict.get(k).and_then(|v| v.downcast_ref::<CFNumber>().and_then(|n| n.as_i64()));
    let string = |k: &CFString| dict.get(k).and_then(|v| v.downcast_ref::<CFString>().map(|s| s.to_string()));
    let id = u32::try_from(number(k_id)?).ok()?;
    let pid = i32::try_from(number(k_pid)?).ok()?;
    let onscreen = dict.get(k_onscreen).and_then(|v| v.downcast_ref::<CFBoolean>().map(|b| b.as_bool()));
    let bounds = dict.get(k_bounds).and_then(|v| parse_bounds(&v)).unwrap_or_default();
    Some(CgWindow {
        id,
        pid,
        owner: string(k_owner).unwrap_or_default(),
        name: string(k_name),
        layer: number(k_layer).unwrap_or(0),
        bounds,
        onscreen: onscreen.unwrap_or(false),
        alpha: dict.get(k_alpha).and_then(|v| v.downcast_ref::<CFNumber>().and_then(|n| n.as_f64())).unwrap_or(1.0),
    })
}

/// Views an untyped dictionary as `CFString → CFType`.
///
/// # Safety
/// Every key must be a `CFString` (values are type-checked by the callers).
unsafe fn as_string_keyed(dict: &CFDictionary) -> &CFDictionary<CFString, CFType> {
    // SAFETY: the generic parameters are phantom; the layout is the same CF object.
    unsafe { &*(dict as *const CFDictionary).cast::<CFDictionary<CFString, CFType>>() }
}

fn parse_bounds(value: &CFType) -> Option<Rect> {
    // SAFETY: bounds dictionaries map CFString keys (X, Y, Width, Height) to CFNumbers.
    let dict = unsafe { as_string_keyed(value.downcast_ref::<CFDictionary>()?) };
    let num = |k: &'static str| {
        dict.get(&CFString::from_static_str(k)).and_then(|v| v.downcast_ref::<CFNumber>().and_then(|n| n.as_f64()))
    };
    Some(Rect { x: num("X")?, y: num("Y")?, w: num("Width")?, h: num("Height")? })
}

/// What [`is_candidate`] excludes besides the layer rule.
#[derive(Clone, Debug, Default)]
pub struct Exclusions {
    /// Our own process.
    pub own_pid: i32,
    /// Processes of [`EXCLUDED_BUNDLE_IDS`].
    pub excluded_pids: HashSet<i32>,
}

/// Whether a window-server window may be managed: layer 0, not ours, not an excluded app.
pub fn is_candidate(w: &CgWindow, ex: &Exclusions) -> bool {
    w.layer == 0 && w.pid != ex.own_pid && !ex.excluded_pids.contains(&w.pid)
}

/// Whether a bundle id is on the exclusion list.
pub fn is_excluded_bundle(bundle_id: &str) -> bool {
    EXCLUDED_BUNDLE_IDS.contains(&bundle_id)
}

/// Z index of every onscreen layer-0 window: 0 is frontmost.
pub fn z_order(list: &[CgWindow]) -> HashMap<u32, usize> {
    list.iter()
        .filter(|w| w.onscreen && w.layer == 0)
        .enumerate()
        .map(|(z, w)| (w.id, z))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: u32, pid: i32, layer: i64, onscreen: bool) -> CgWindow {
        CgWindow { id, pid, owner: String::new(), name: None, layer, bounds: Rect::default(), onscreen, alpha: 1.0 }
    }

    #[test]
    fn z_order_counts_onscreen_layer_zero_front_to_back() {
        let list = [w(10, 1, 25, true), w(11, 1, 0, true), w(12, 2, 0, false), w(13, 2, 0, true)];
        let z = z_order(&list);
        assert_eq!(z.get(&11), Some(&0));
        assert_eq!(z.get(&13), Some(&1));
        assert!(!z.contains_key(&10));
        assert!(!z.contains_key(&12));
    }

    #[test]
    fn candidates_exclude_own_pid_layers_and_listed_apps() {
        let ex = Exclusions { own_pid: 7, excluded_pids: [9].into_iter().collect() };
        assert!(is_candidate(&w(1, 3, 0, true), &ex));
        assert!(!is_candidate(&w(1, 7, 0, true), &ex));
        assert!(!is_candidate(&w(1, 9, 0, true), &ex));
        assert!(!is_candidate(&w(1, 3, 3, true), &ex));
        assert!(is_excluded_bundle("com.apple.dock"));
        assert!(!is_excluded_bundle(FINDER_BUNDLE_ID));
    }

    #[test]
    fn listing_the_real_window_server_does_not_panic() {
        let list = list_windows();
        assert!(list.iter().all(|w| w.id != 0));
    }

    #[test]
    fn describing_windows_by_id_reads_only_those() {
        assert!(describe(&[]).is_empty());
        let list = list_windows();
        let ids: Vec<u32> = list.iter().take(3).map(|w| w.id).collect();
        let described = describe(&ids);
        assert!(described.iter().all(|w| ids.contains(&w.id)), "only the windows asked");
        for w in &described {
            let listed = list.iter().find(|l| l.id == w.id).expect("listed");
            assert_eq!(w.pid, listed.pid, "window {}: the same owner", w.id);
        }
        assert!(describe(&[u32::MAX]).is_empty(), "an id the window server does not know");
    }
}
