//! The window server's view: `CGWindowListCopyWindowInfo` front to back, exclusions and
//! z-order.
//!
//! **One read per wake** (round sm-tick): the WM tick's duties that read the list (the
//! closed windows' sweep, the Space check, the engine's upkeep) share one read of it while a
//! [`Wake`] is open ([`wake_list`], [`wake_on_screen`]), until a window write of ours
//! ([`note_write`]: every Accessibility set and action, the Spaces engine's moves) makes it
//! stale: a duty after a write reads afresh, as each did before. Outside a wake every read is
//! fresh.
//!
//! **Windows in a transformed Space** (round sm-landing): while a switch's incoming window
//! flies its last stretch through its Space's transform, the window server lists its drawn
//! bounds; every read here reports its frame instead ([`set_transformed`]), so nothing reads
//! a window mid-flight as moved or resized.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFIndex, CFNumber, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    CGWindowListCopyWindowInfo, CGWindowListCreate, CGWindowListCreateDescriptionFromArray, CGWindowListOption, kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds,
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
    joined(on_screen())
}

/// The on-screen list `on` (front to back) followed by every other window the "all" query
/// lists ([`list_windows`]).
fn joined(mut on: Vec<CgWindow>) -> Vec<CgWindow> {
    let seen: HashSet<u32> = on.iter().map(|w| w.id).collect();
    on.extend(
        query(CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements)
            .into_iter()
            .filter(|w| !seen.contains(&w.id)),
    );
    on
}

/// Windows drawn in a transformed Space now, each with its frame ([`set_transformed`]).
static TRANSFORMED: Mutex<Vec<(u32, Rect)>> = Mutex::new(Vec::new());
/// How many there are (the reads skip the lock while none is).
static TRANSFORMED_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Window `id` is drawn in a transformed Space from now on, its frame `frame` (`Some`), or is
/// no longer (`None`): the reads report `frame` for it meanwhile (any thread).
pub fn set_transformed(id: u32, frame: Option<Rect>) {
    let mut t = TRANSFORMED.lock().unwrap_or_else(|p| p.into_inner());
    t.retain(|(w, _)| *w != id);
    if let Some(f) = frame {
        t.push((id, f));
    }
    TRANSFORMED_COUNT.store(t.len(), Ordering::Relaxed);
}

/// The bounds to report for window `id` listed at `listed`: its frame while it is drawn in a
/// transformed Space ([`set_transformed`]), else as listed.
fn reported(id: u32, listed: Rect) -> Rect {
    if TRANSFORMED_COUNT.load(Ordering::Relaxed) == 0 {
        return listed;
    }
    let t = TRANSFORMED.lock().unwrap_or_else(|p| p.into_inner());
    t.iter().find(|(w, _)| *w == id).map_or(listed, |(_, f)| *f)
}

/// Window writes of ours so far ([`note_write`]).
static WRITES: AtomicU64 = AtomicU64::new(0);

/// A window write of ours (any thread): a list read in the open [`Wake`] before it is stale.
pub fn note_write() {
    WRITES.fetch_add(1, Ordering::Relaxed);
}

/// The lists read in the open wake, and the writes counted when they were read.
#[derive(Default)]
struct WakeLists {
    writes: u64,
    on: Option<Rc<Vec<CgWindow>>>,
    full: Option<Rc<Vec<CgWindow>>>,
}

impl WakeLists {
    /// Forgets what was read before a write of ours.
    fn fresh(&mut self) -> &mut Self {
        let writes = WRITES.load(Ordering::Relaxed);
        if writes != self.writes {
            *self = Self { writes, ..Self::default() };
        }
        self
    }
}

thread_local! {
    /// The open wake's lists on this thread ([`wake`]); `None` outside one.
    static WAKE: RefCell<Option<WakeLists>> = const { RefCell::new(None) };
}

/// An open wake ([`wake`]): its reads are shared until it is dropped.
pub struct Wake(());

impl Drop for Wake {
    fn drop(&mut self) {
        WAKE.with(|w| *w.borrow_mut() = None);
    }
}

/// Opens a wake on this thread (the WM tick): [`wake_list`] and [`wake_on_screen`] read the
/// window server once each until a write of ours, then afresh.
pub fn wake() -> Wake {
    WAKE.with(|w| *w.borrow_mut() = Some(WakeLists { writes: WRITES.load(Ordering::Relaxed), ..WakeLists::default() }));
    Wake(())
}

/// [`list_windows`], read once per wake (the on-screen part shared with
/// [`wake_on_screen`]); fresh outside one.
pub fn wake_list() -> Rc<Vec<CgWindow>> {
    WAKE.with(|w| {
        let mut w = w.borrow_mut();
        let Some(lists) = w.as_mut() else { return Rc::new(list_windows()) };
        let lists = lists.fresh();
        if let Some(full) = &lists.full {
            return full.clone();
        }
        let on = lists.on.get_or_insert_with(|| Rc::new(on_screen())).as_ref().clone();
        let full = Rc::new(joined(on));
        lists.full = Some(full.clone());
        full
    })
}

/// [`on_screen`], read once per wake (the on-screen query [`wake_list`] made, when it came
/// first); fresh outside one.
pub fn wake_on_screen() -> Rc<Vec<CgWindow>> {
    WAKE.with(|w| {
        let mut w = w.borrow_mut();
        let Some(lists) = w.as_mut() else { return Rc::new(on_screen()) };
        lists.fresh().on.get_or_insert_with(|| Rc::new(on_screen())).clone()
    })
}

/// Every window, on screen or not, on any Space (desktop elements excluded), in the window
/// server's order: one query (the Spaces probe's watch for windows that are not shown yet).
pub fn every_window() -> Vec<CgWindow> {
    query(CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements)
}

/// The ids of every window, on screen or not, on any Space (desktop elements excluded):
/// `CGWindowListCreate`, no description built, a fraction of [`every_window`]'s cost (a
/// launch's watch for a new window reads it every few ms,
/// [`super::spaces::launch_watch`]).
pub fn every_window_id() -> Vec<u32> {
    let Some(array) = CGWindowListCreate(CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements, kCGNullWindowID) else {
        return Vec::new();
    };
    (0..array.count())
        .filter_map(|i| {
            // SAFETY: `i` is within the array; its values are the CGWindowIDs themselves
            // (no callbacks), as the call documents.
            let v = unsafe { array.value_at_index(i) };
            u32::try_from(v as usize).ok()
        })
        .collect()
}

/// The on-screen windows only (desktop elements excluded), front to back: one query.
pub fn on_screen() -> Vec<CgWindow> {
    query(CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements)
}

/// The on-screen windows front to back with the desktop's own among them (the desktop
/// picture, Finder's icons, the window server's backdrop, all far below layer 0): one query.
/// Only the test's desktop clicks need those listed ([`super::input::desktop_click`]: where
/// the desktop is topmost); every other reader leaves them out.
pub fn on_screen_with_desktop() -> Vec<CgWindow> {
    query(CGWindowListOption::OptionOnScreenOnly)
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
    let bounds = reported(id, dict.get(k_bounds).and_then(|v| parse_bounds(&v)).unwrap_or_default());
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

    /// A window flying in a transformed Space reads at its frame; others as listed; once it
    /// lands, as listed again.
    #[test]
    fn a_window_in_a_transformed_space_reads_at_its_frame() {
        let frame = Rect::new(100.0, 80.0, 1200.0, 800.0);
        let drawn = Rect::new(400.0, 300.0, 600.0, 400.0);
        set_transformed(4_000_001, Some(frame));
        assert_eq!(reported(4_000_001, drawn), frame);
        assert_eq!(reported(4_000_002, drawn), drawn);
        set_transformed(4_000_001, None);
        assert_eq!(reported(4_000_001, drawn), drawn);
    }

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

    /// Round sm-tick: inside a wake the tick's duties share one read of each list, the
    /// on-screen query shared between the two, until a window write of ours; outside a wake
    /// every read is fresh.
    #[test]
    fn a_wake_reads_each_list_once_until_a_write_of_ours() {
        let a = wake_on_screen();
        assert!(!Rc::ptr_eq(&a, &wake_on_screen()), "outside a wake: read afresh");
        let wake = wake();
        let full = wake_list();
        let on = wake_on_screen();
        assert!(Rc::ptr_eq(&full, &wake_list()), "the complete list, read once");
        assert!(Rc::ptr_eq(&on, &wake_on_screen()), "the on-screen list, read once");
        assert!(full.starts_with(&on), "the complete list starts with the same on-screen read");
        note_write();
        assert!(!Rc::ptr_eq(&full, &wake_list()), "after a write of ours: read afresh");
        assert!(!Rc::ptr_eq(&on, &wake_on_screen()));
        let again = wake_on_screen();
        assert!(Rc::ptr_eq(&again, &wake_on_screen()), "then shared again");
        drop(wake);
        assert!(!Rc::ptr_eq(&again, &wake_on_screen()), "the wake over: read afresh");
    }
}
