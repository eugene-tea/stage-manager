//! SkyLight's Space calls, loaded at run time: the C functions with `dlopen`/`dlsym`, the
//! bridged operations (`SLSBridged…Operation`) as Objective-C classes looked up by name and
//! sent through `objc_msgSend` (also found with `dlsym`). Nothing is linked. Two routes:
//! [`Route::Classic`] (`SLSSpaceCreate`, `SLSShowSpaces`, …) and [`Route::Bridged`] (the
//! operation classes, performed with `-performWithWMBridgeDelegate`, which needs AppKit
//! loaded). The signatures come from public sources only (yabai, SketchyBar, a public header
//! dump); the Spaces engine ([`super::engine`]) and the probe
//! ([`crate::platform::spaces_probe`]) both call through here.
//!
//! What the spike measured (macOS 27.2): only the bridged add moves another app's window; a
//! classic Space belongs to the connection that made it and ends with it, its windows back
//! on the Desktop shown; return codes mean nothing (read back instead).

use std::ffi::{CStr, c_void};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::sel;
use objc2_core_foundation::CGAffineTransform;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use crate::engine::model::desks::Desk;

/// The SkyLight framework's binary.
pub(crate) const SKYLIGHT: &CStr = c"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";

/// `SLSCopySpaces`/`SLSCopySpacesForWindows` mask: every kind of Space (synara lists
/// orphans with 0xf; yabai and sunlight pass 0x7, which leaves the probe's Spaces out).
pub(crate) const ALL_SPACES_MASK: i32 = 0xf;

/// `SLSSpaceAddWindowsAndRemoveFromSpaces` options as SketchyBar and sunlight pass them.
pub(crate) const ADD_REMOVE_OPTIONS: i32 = 7;

pub(crate) type FnMain = unsafe extern "C" fn() -> i32;
pub(crate) type FnCopyByCid = unsafe extern "C" fn(i32) -> *mut c_void;
pub(crate) type FnCopySpaces = unsafe extern "C" fn(i32, i32) -> *mut c_void;
pub(crate) type FnCopySpacesForWindows = unsafe extern "C" fn(i32, i32, *const c_void) -> *mut c_void;
pub(crate) type FnSpaceCreate = unsafe extern "C" fn(i32, i32, i32) -> u64;
pub(crate) type FnSpaceDestroy = unsafe extern "C" fn(i32, u64) -> i32;
pub(crate) type FnSpaceLevel = unsafe extern "C" fn(i32, u64, i32) -> i32;
/// `int SLSSpaceGetAbsoluteLevel(cid, sid)` (CGSSpace.h as Penc and BetterSwiftAX carry it).
pub(crate) type FnSpaceGetLevel = unsafe extern "C" fn(i32, u64) -> i32;
pub(crate) type FnSpaceList = unsafe extern "C" fn(i32, *const c_void) -> i32;
pub(crate) type FnAddRemove = unsafe extern "C" fn(i32, u64, *const c_void, i32) -> i32;
pub(crate) type FnMoveManaged = unsafe extern "C" fn(i32, *const c_void, u64) -> i32;
pub(crate) type FnSetTransform = unsafe extern "C" fn(i32, u64, *const CGAffineTransform, i32) -> i32;
pub(crate) type FnGetTransform = unsafe extern "C" fn(i32, u64, *mut i32) -> CGAffineTransform;
pub(crate) type FnSpaceType = unsafe extern "C" fn(i32, u64) -> i32;
pub(crate) type FnActiveSpace = unsafe extern "C" fn(i32) -> u64;

/// One screen's entry of `SLSCopyManagedDisplaySpaces`: its identifier, the Space it shows
/// and that Space's type, its Desktops (type 0) with their uuids, and its other Spaces (a
/// full-screen app's, a tiled pair's) with their types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScreenDesks {
    /// The screen's "Display Identifier": its uuid, or `Main` (macOS names the main screen so
    /// in some setups; [`crate::platform::controller`]'s listing matches either).
    pub screen: String,
    /// The Space it shows ("Current Space"); 0 when the entry names none.
    pub current: u64,
    /// The type of the Space it shows: 0 a Desktop, else a full-screen app's (or another
    /// kind); -1 when the entry does not say.
    pub current_kind: i64,
    /// Its Desktops (type 0), in the window server's order.
    pub desks: Vec<Desk>,
    /// Its other Spaces, with their types (-1 when unsaid), in the window server's order.
    pub others: Vec<(u64, i64)>,
}

impl ScreenDesks {
    /// Every Space it lists, Desktops first, with its type.
    pub fn spaces(&self) -> impl Iterator<Item = (u64, i64)> + '_ {
        self.desks.iter().map(|d| (d.id, 0)).chain(self.others.iter().copied())
    }

    /// Every Space it lists (Desktops first).
    pub fn every(&self) -> impl Iterator<Item = u64> + '_ {
        self.spaces().map(|(id, _)| id)
    }

    /// The entry as one dictionary of `SLSCopyManagedDisplaySpaces` holds it.
    pub fn parse(d: &NSDictionary) -> Self {
        let screen = get(d, "Display Identifier").and_then(|v| v.downcast::<NSString>().ok()).map(|s| s.to_string()).unwrap_or_default();
        let shown = get(d, "Current Space").and_then(|v| v.downcast::<NSDictionary>().ok());
        let current = shown.as_ref().and_then(|c| number(c, "id64")).unwrap_or(0);
        let kind = |s: &NSDictionary| number(s, "type").map_or(-1, |t| t as i64);
        let mut out = Self { screen, current, current_kind: -1, ..Self::default() };
        if let Some(list) = get(d, "Spaces").and_then(|v| v.downcast::<NSArray>().ok()) {
            for s in list.iter() {
                let Ok(s) = s.downcast::<NSDictionary>() else { continue };
                let id = number(&s, "id64").unwrap_or(0);
                match kind(&s) {
                    0 => {
                        let uuid = get(&s, "uuid").and_then(|v| v.downcast::<NSString>().ok()).map(|u| u.to_string()).unwrap_or_default();
                        out.desks.push(Desk { id, uuid });
                    }
                    k => out.others.push((id, k)),
                }
            }
        }
        // The Current Space's own type, else the type of the Space listed under its id.
        out.current_kind = shown
            .as_ref()
            .and_then(|c| number(c, "type").map(|t| t as i64))
            .or_else(|| out.spaces().find(|(id, _)| *id == current).map(|(_, k)| k))
            .unwrap_or(-1);
        out
    }
}

/// The C functions, each `None` when SkyLight does not export it.
pub(crate) struct Sky {
    pub(crate) cid: i32,
    pub(crate) copy_managed: Option<FnCopyByCid>,
    // Loaded in every run, as the rest (the names loaded stay the same); read by the probes only.
    #[cfg_attr(not(feature = "probes"), allow(dead_code))]
    pub(crate) copy_spaces: Option<FnCopySpaces>,
    pub(crate) spaces_for_windows: Option<FnCopySpacesForWindows>,
    pub(crate) create: Option<FnSpaceCreate>,
    pub(crate) destroy: Option<FnSpaceDestroy>,
    pub(crate) level: Option<FnSpaceLevel>,
    pub(crate) get_level: Option<FnSpaceGetLevel>,
    pub(crate) show: Option<FnSpaceList>,
    pub(crate) hide: Option<FnSpaceList>,
    pub(crate) add_remove: Option<FnAddRemove>,
    // Loaded in every run, as the rest (the names loaded stay the same); read by the probes only.
    #[cfg_attr(not(feature = "probes"), allow(dead_code))]
    pub(crate) move_managed: Option<FnMoveManaged>,
    pub(crate) set_transform: Option<FnSetTransform>,
    pub(crate) get_transform: Option<FnGetTransform>,
    // Loaded in every run, as the rest (the names loaded stay the same); read by the probes only.
    #[cfg_attr(not(feature = "probes"), allow(dead_code))]
    pub(crate) space_type: Option<FnSpaceType>,
    pub(crate) active: Option<FnActiveSpace>,
    pub(crate) msg_send: *const c_void,
    pub(crate) names: Vec<(&'static str, bool)>,
}

// SAFETY: `Sky` holds function addresses of a loaded system framework, a connection id and
// the address of `objc_msgSend`: plain values, never written after `open`, and the functions
// are the window server's client calls, which take the connection id and lock for themselves.
unsafe impl Send for Sky {}
// SAFETY: as above; nothing in it changes behind a shared reference.
unsafe impl Sync for Sky {}

/// The function `name` from `handle`, as the type the caller names.
///
/// # Safety
/// `F` must be an `extern "C"` function pointer type matching the symbol's real signature.
pub(crate) unsafe fn load<F: Copy>(handle: *mut c_void, name: &'static CStr, names: &mut Vec<(&'static str, bool)>) -> Option<F> {
    // SAFETY: `handle` is a live dlopen handle (or RTLD_DEFAULT) and `name` a C string.
    let p = unsafe { libc::dlsym(handle, name.as_ptr()) };
    names.push((name.to_str().unwrap_or("?"), !p.is_null()));
    if p.is_null() {
        return None;
    }
    // SAFETY: a non-null symbol address, read as the function pointer type the caller states
    // (same size as a data pointer on Apple platforms).
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&p) })
}

impl Sky {
    /// Test only: no SkyLight behind it. On a thread with a fake desk
    /// ([`crate::platform::fake`]) each call is answered by the desk before it would need one.
    #[cfg(test)]
    pub(crate) fn hollow() -> Self {
        Self {
            cid: 0,
            copy_managed: None,
            copy_spaces: None,
            spaces_for_windows: None,
            create: None,
            destroy: None,
            level: None,
            get_level: None,
            show: None,
            hide: None,
            add_remove: None,
            move_managed: None,
            set_transform: None,
            get_transform: None,
            space_type: None,
            active: None,
            msg_send: std::ptr::null(),
            names: Vec::new(),
        }
    }

    pub(crate) fn open() -> Result<Self, String> {
        #[cfg(test)]
        if crate::platform::fake::active() {
            return Ok(Self::hollow());
        }
        // SAFETY: a system framework path; RTLD_NOW resolves everything at load.
        let h = unsafe { libc::dlopen(SKYLIGHT.as_ptr(), libc::RTLD_NOW) };
        if h.is_null() {
            return Err("SkyLight did not load".into());
        }
        let mut names = Vec::new();
        // SAFETY (every `load` below): the types follow the public signatures listed in the
        // module docs and the spike notes.
        let main: Option<FnMain> = unsafe { load(h, c"SLSMainConnectionID", &mut names) };
        let Some(main) = main else { return Err("SLSMainConnectionID is missing".into()) };
        // SAFETY: no arguments; returns this process's connection id.
        let cid = unsafe { main() };
        // SAFETY: as above.
        let msg_send = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"objc_msgSend".as_ptr()) };
        Ok(Self {
            cid,
            copy_managed: unsafe { load(h, c"SLSCopyManagedDisplaySpaces", &mut names) },
            copy_spaces: unsafe { load(h, c"SLSCopySpaces", &mut names) },
            spaces_for_windows: unsafe { load(h, c"SLSCopySpacesForWindows", &mut names) },
            create: unsafe { load(h, c"SLSSpaceCreate", &mut names) },
            destroy: unsafe { load(h, c"SLSSpaceDestroy", &mut names) },
            level: unsafe { load(h, c"SLSSpaceSetAbsoluteLevel", &mut names) },
            get_level: unsafe { load(h, c"SLSSpaceGetAbsoluteLevel", &mut names) },
            show: unsafe { load(h, c"SLSShowSpaces", &mut names) },
            hide: unsafe { load(h, c"SLSHideSpaces", &mut names) },
            add_remove: unsafe { load(h, c"SLSSpaceAddWindowsAndRemoveFromSpaces", &mut names) },
            move_managed: unsafe { load(h, c"SLSMoveWindowsToManagedSpace", &mut names) },
            set_transform: unsafe { load(h, c"SLSSpaceSetTransform", &mut names) },
            get_transform: unsafe { load(h, c"SLSSpaceGetTransform", &mut names) },
            space_type: unsafe { load(h, c"SLSSpaceGetType", &mut names) },
            active: unsafe { load(h, c"SLSGetActiveSpace", &mut names) },
            msg_send,
            names,
        })
    }

    /// Every screen's entry of `SLSCopyManagedDisplaySpaces` ([`ScreenDesks`]), in its order.
    pub(crate) fn displays(&self) -> Vec<ScreenDesks> {
        #[cfg(test)]
        if let Some(out) = crate::platform::fake::with(|d| d.sky_displays()) {
            return out;
        }
        let Some(f) = self.copy_managed else { return Vec::new() };
        // SAFETY: returns a +1 CFArray of CFDictionary (or null).
        let Some(list) = (unsafe { take_array(f(self.cid)) }) else { return Vec::new() };
        list.iter().filter_map(|d| d.downcast::<NSDictionary>().ok()).map(|d| ScreenDesks::parse(&d)).collect()
    }

    /// The main display's Desktop shown and its uuid.
    pub(crate) fn current(&self) -> (u64, String) {
        self.displays().into_iter().next().map(|d| (d.current, d.screen)).unwrap_or((0, String::new()))
    }

    /// The Space of the screen with the keyboard (`SLSGetActiveSpace`), when exported.
    pub(crate) fn active_space(&self) -> Option<u64> {
        #[cfg(test)]
        if let Some(out) = crate::platform::fake::with(|d| d.screens[0].shown) {
            return Some(out);
        }
        // SAFETY: plain query on this process's connection.
        self.active.map(|f| unsafe { f(self.cid) }).filter(|s| *s != 0)
    }

    #[cfg(feature = "probes")]
    /// Every Space the window server lists for this mask.
    pub(crate) fn all_spaces(&self, mask: i32) -> Vec<u64> {
        #[cfg(test)]
        if let Some(out) = crate::platform::fake::with(|d| d.sky_all_spaces(mask)) {
            return out;
        }
        let Some(f) = self.copy_spaces else { return Vec::new() };
        // SAFETY: returns a +1 CFArray of CFNumber (or null).
        unsafe { take_array(f(self.cid, mask)) }.map(|a| numbers(&a)).unwrap_or_default()
    }

    /// The Spaces each window is on.
    pub(crate) fn spaces_of(&self, ids: &[u32]) -> Vec<u64> {
        #[cfg(test)]
        if let Some(out) = crate::platform::fake::with(|d| d.sky_spaces_of(ids)) {
            return out;
        }
        let Some(f) = self.spaces_for_windows else { return Vec::new() };
        let arr = wid_array(ids);
        // SAFETY: a CFArray of window numbers; returns a +1 CFArray of CFNumber (or null).
        unsafe { take_array(f(self.cid, ALL_SPACES_MASK, Retained::as_ptr(&arr).cast())) }.map(|a| numbers(&a)).unwrap_or_default()
    }

    #[cfg(feature = "probes")]
    pub(crate) fn space_type(&self, sid: u64) -> Option<i32> {
        #[cfg(test)]
        if let Some(out) = crate::platform::fake::with(|d| if d.is_desktop(sid) { 0 } else { 2 }) {
            return Some(out);
        }
        // SAFETY: plain query.
        self.space_type.map(|f| unsafe { f(self.cid, sid) })
    }

    pub(crate) fn get_transform(&self, sid: u64) -> Option<[f64; 6]> {
        #[cfg(test)]
        if crate::platform::fake::active() {
            return Some([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        }
        let f = self.get_transform?;
        let mut options = 0i32;
        // SAFETY: returns the transform by value (indirect result); `options` is writable.
        let t = unsafe { f(self.cid, sid, &mut options) };
        Some([t.a, t.b, t.c, t.d, t.tx, t.ty])
    }

    /// The class `name`, when the runtime has it.
    pub(crate) fn class(name: &CStr) -> Option<&'static AnyClass> {
        AnyClass::get(name)
    }

    /// `[[cls alloc] <init>]` (`build` sends `init` with its arguments), performed and
    /// released; the result of `-performWithWMBridgeDelegate` (null for an asynchronous
    /// operation). An error when the class's instances do not answer `init` or the perform
    /// ([`answers`]).
    pub(crate) fn bridged(&self, class: &CStr, init: Sel, build: impl FnOnce(*mut AnyObject) -> *mut AnyObject) -> Result<*mut AnyObject, String> {
        let Some(cls) = Self::class(class) else { return Err(format!("{} is missing", class.to_str().unwrap_or("?"))) };
        answers(cls, init)?;
        if self.msg_send.is_null() {
            return Err("objc_msgSend is missing".into());
        }
        // SAFETY: `+alloc` on a class: (Class, SEL) -> id.
        let alloc: unsafe extern "C" fn(*const AnyClass, Sel) -> *mut AnyObject = unsafe { std::mem::transmute(self.msg_send) };
        // SAFETY: as typed above.
        let raw = unsafe { alloc(cls, sel!(alloc)) };
        let op = build(raw);
        if op.is_null() {
            return Err("the operation's initializer returned nil".into());
        }
        // SAFETY: `-performWithWMBridgeDelegate`: (id, SEL) -> id (void for the asynchronous
        // ones, read as a null-or-garbage pointer we only use for the synchronous ones).
        let perform: unsafe extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject = unsafe { std::mem::transmute(self.msg_send) };
        // SAFETY: `op` is a live, initialized operation.
        let result = unsafe { perform(op, sel!(performWithWMBridgeDelegate)) };
        // SAFETY: `op` came from alloc/init (+1); this balances it.
        drop(unsafe { Retained::from_raw(op) });
        Ok(result)
    }

    pub(crate) fn send_obj(&self) -> unsafe extern "C" fn(*mut AnyObject, Sel, *const c_void) -> *mut AnyObject {
        // SAFETY: (id, SEL, id) -> id.
        unsafe { std::mem::transmute(self.msg_send) }
    }

    pub(crate) fn create(&self, route: Route, managed: bool) -> Result<u64, String> {
        #[cfg(test)]
        if let Some(out) = crate::platform::fake::with(|d| d.sky_create()) {
            return Ok(out);
        }
        match route {
            Route::Classic => {
                let f = self.create.ok_or("SLSSpaceCreate is missing")?;
                // SAFETY: (cid, 1, 0) as SketchyBar and sunlight call it.
                Ok(unsafe { f(self.cid, 1, 0) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let (options, values): (u32, Retained<NSDictionary<NSString, AnyObject>>) = if managed {
                    let uuid = NSString::from_str(&self.current().1);
                    let zero = NSNumber::new_i32(0);
                    let keys = [&*NSString::from_str("type"), &*NSString::from_str("Display Identifier")];
                    let vals: [&AnyObject; 2] = [&zero, &uuid];
                    (0, NSDictionary::from_slices(&keys, &vals))
                } else {
                    (1, NSDictionary::new())
                };
                let ms = self.msg_send;
                let result = self.bridged(c"SLSBridgedSpaceCreateOperation", sel!(initWithOptions:values:), |raw| {
                    // SAFETY: `-initWithOptions:values:` (id, SEL, uint32, id) -> id.
                    let init: unsafe extern "C" fn(*mut AnyObject, Sel, u32, *const c_void) -> *mut AnyObject =
                        unsafe { std::mem::transmute(ms) };
                    // SAFETY: as typed; `values` lives across the call.
                    unsafe { init(raw, sel!(initWithOptions:values:), options, Retained::as_ptr(&values).cast()) }
                })?;
                if result.is_null() {
                    return Err("the create operation returned nil".into());
                }
                // SAFETY: `-spaceID` (id, SEL) -> uint64 on SLSBridgedWindowManagementOperationSpaceIDResult.
                let space_id: unsafe extern "C" fn(*mut AnyObject, Sel) -> u64 = unsafe { std::mem::transmute(ms) };
                // SAFETY: `result` is the live result object (autoreleased, inside this pool).
                let responds = unsafe { (*result).class().responds_to(sel!(spaceID)) };
                if !responds {
                    return Err(format!("the create operation returned a {}", unsafe { (*result).class().name().to_string_lossy() }));
                }
                // SAFETY: checked above that it answers -spaceID.
                Ok(unsafe { space_id(result, sel!(spaceID)) })
            }),
        }
    }

    pub(crate) fn destroy(&self, route: Route, sid: u64) -> Result<i32, String> {
        // Ending a Space that holds windows puts them on the Desktop shown.
        crate::platform::windows::note_write();
        #[cfg(test)]
        if crate::platform::fake::with(|d| d.sky_destroy(sid)).is_some() {
            return Ok(0);
        }
        match route {
            Route::Classic => {
                let f = self.destroy.ok_or("SLSSpaceDestroy is missing")?;
                // SAFETY: (cid, sid).
                Ok(unsafe { f(self.cid, sid) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let ms = self.msg_send;
                self.bridged(c"SLSBridgedSpaceDestroyOperation", sel!(initWithSpaceID:), |raw| {
                    // SAFETY: `-initWithSpaceID:` (id, SEL, uint64) -> id.
                    let init: unsafe extern "C" fn(*mut AnyObject, Sel, u64) -> *mut AnyObject = unsafe { std::mem::transmute(ms) };
                    // SAFETY: as typed.
                    unsafe { init(raw, sel!(initWithSpaceID:), sid) }
                })
                .map(|_| 0)
            }),
        }
    }

    /// Space `sid`'s level ([`Self::set_level`]), when exported.
    pub(crate) fn get_level(&self, sid: u64) -> Option<i32> {
        #[cfg(test)]
        if crate::platform::fake::active() {
            return Some(0);
        }
        // SAFETY: plain query on this process's connection.
        self.get_level.map(|f| unsafe { f(self.cid, sid) })
    }

    pub(crate) fn set_level(&self, route: Route, sid: u64, level: i32) -> Result<i32, String> {
        #[cfg(test)]
        if crate::platform::fake::active() {
            return Ok(0);
        }
        match route {
            Route::Classic => {
                let f = self.level.ok_or("SLSSpaceSetAbsoluteLevel is missing")?;
                // SAFETY: (cid, sid, level).
                Ok(unsafe { f(self.cid, sid, level) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let ms = self.msg_send;
                self.bridged(c"SLSBridgedSpaceSetAbsoluteLevelOperation", sel!(initWithSpaceID:level:), |raw| {
                    // SAFETY: `-initWithSpaceID:level:` (id, SEL, uint64, int) -> id.
                    let init: unsafe extern "C" fn(*mut AnyObject, Sel, u64, i32) -> *mut AnyObject = unsafe { std::mem::transmute(ms) };
                    // SAFETY: as typed.
                    unsafe { init(raw, sel!(initWithSpaceID:level:), sid, level) }
                })
                .map(|_| 0)
            }),
        }
    }

    pub(crate) fn show_hide(&self, route: Route, sids: &[u64], show: bool) -> Result<i32, String> {
        #[cfg(test)]
        if crate::platform::fake::active() {
            return Ok(0);
        }
        let arr = sid_array(sids);
        match route {
            Route::Classic => {
                let f = if show { self.show } else { self.hide }.ok_or("SLSShowSpaces/SLSHideSpaces is missing")?;
                // SAFETY: (cid, CFArray of Space ids).
                Ok(unsafe { f(self.cid, Retained::as_ptr(&arr).cast()) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let class = if show { c"SLSBridgedShowSpacesOperation" } else { c"SLSBridgedHideSpacesOperation" };
                let send = self.send_obj();
                self.bridged(class, sel!(initWithSpaces:), |raw| {
                    // SAFETY: `-initWithSpaces:` (id, SEL, NSArray) -> id; `arr` outlives it.
                    unsafe { send(raw, sel!(initWithSpaces:), Retained::as_ptr(&arr).cast()) }
                })
                .map(|_| 0)
            }),
        }
    }

    /// The windows into Space `sid`, out of the other Spaces `options` names (a mask of kinds
    /// of Space, as [`ALL_SPACES_MASK`] is).
    pub(crate) fn add_remove_from(&self, route: Route, sid: u64, ids: &[u32], options: i32) -> Result<i32, String> {
        crate::platform::windows::note_write();
        // Its notices are ours: they trigger nothing (round r5-notices).
        super::notices::expect(sid, ids);
        #[cfg(test)]
        if crate::platform::fake::with(|d| d.sky_add_remove(route == Route::Bridged, sid, ids, options)).is_some() {
            return Ok(0);
        }
        let arr = wid_array(ids);
        match route {
            Route::Classic => {
                let f = self.add_remove.ok_or("SLSSpaceAddWindowsAndRemoveFromSpaces is missing")?;
                // SAFETY: (cid, sid, CFArray of window ids, 7).
                Ok(unsafe { f(self.cid, sid, Retained::as_ptr(&arr).cast(), options) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let ms = self.msg_send;
                self.bridged(c"SLSBridgedSpaceAddWindowsAndRemoveFromSpacesOperation", sel!(initWithSpaceID:windows:options:), |raw| {
                    // SAFETY: `-initWithSpaceID:windows:options:` (id, SEL, uint64, NSArray, uint32) -> id.
                    let init: unsafe extern "C" fn(*mut AnyObject, Sel, u64, *const c_void, u32) -> *mut AnyObject =
                        unsafe { std::mem::transmute(ms) };
                    // SAFETY: as typed; `arr` outlives the call.
                    unsafe { init(raw, sel!(initWithSpaceID:windows:options:), sid, Retained::as_ptr(&arr).cast(), options as u32) }
                })
                .map(|_| 0)
            }),
        }
    }

    /// The windows into Space `sid`, out of every Desktop ([`ADD_REMOVE_OPTIONS`]).
    pub(crate) fn add_remove(&self, route: Route, sid: u64, ids: &[u32]) -> Result<i32, String> {
        self.add_remove_from(route, sid, ids, ADD_REMOVE_OPTIONS)
    }

    #[cfg(feature = "probes")]
    /// The windows onto the managed Space (a Desktop) `sid`.
    pub(crate) fn move_managed(&self, route: Route, ids: &[u32], sid: u64) -> Result<i32, String> {
        crate::platform::windows::note_write();
        super::notices::expect(sid, ids);
        #[cfg(test)]
        if crate::platform::fake::with(|d| d.sky_add_remove(route == Route::Bridged, sid, ids, 0xf)).is_some() {
            return Ok(0);
        }
        let arr = wid_array(ids);
        match route {
            Route::Classic => {
                let f = self.move_managed.ok_or("SLSMoveWindowsToManagedSpace is missing")?;
                // SAFETY: (cid, CFArray of window ids, sid).
                Ok(unsafe { f(self.cid, Retained::as_ptr(&arr).cast(), sid) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let ms = self.msg_send;
                self.bridged(c"SLSBridgedMoveWindowsToManagedSpaceOperation", sel!(initWithWindows:spaceID:), |raw| {
                    // SAFETY: `-initWithWindows:spaceID:` (id, SEL, NSArray, uint64) -> id.
                    let init: unsafe extern "C" fn(*mut AnyObject, Sel, *const c_void, u64) -> *mut AnyObject =
                        unsafe { std::mem::transmute(ms) };
                    // SAFETY: as typed; `arr` outlives the call.
                    unsafe { init(raw, sel!(initWithWindows:spaceID:), Retained::as_ptr(&arr).cast(), sid) }
                })
                .map(|_| 0)
            }),
        }
    }

    pub(crate) fn set_transform(&self, route: Route, sid: u64, t: CGAffineTransform) -> Result<i32, String> {
        #[cfg(test)]
        if crate::platform::fake::active() {
            return Ok(0);
        }
        match route {
            Route::Classic => {
                let f = self.set_transform.ok_or("SLSSpaceSetTransform is missing")?;
                // SAFETY: (cid, sid, &transform, options 0); on arm64 a 48-byte struct passes by
                // reference either way.
                Ok(unsafe { f(self.cid, sid, &t, 0) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let ms = self.msg_send;
                self.bridged(c"SLSBridgedSpaceSetTransformOperation", sel!(initWithSpaceID:transform:options:), |raw| {
                    // SAFETY: `-initWithSpaceID:transform:options:` (id, SEL, uint64,
                    // CGAffineTransform, uint32) -> id.
                    let init: unsafe extern "C" fn(*mut AnyObject, Sel, u64, CGAffineTransform, u32) -> *mut AnyObject =
                        unsafe { std::mem::transmute(ms) };
                    // SAFETY: as typed.
                    unsafe { init(raw, sel!(initWithSpaceID:transform:options:), sid, t, 0) }
                })
                .map(|_| 0)
            }),
        }
    }
}

/// Ok when instances of `cls` answer `init` and `-performWithWMBridgeDelegate`: sent to a
/// class that does not (a macOS that renamed or dropped one), `objc_msgSend` raises an
/// Objective-C exception, which unwinds into Rust through an `extern "C"` call and aborts
/// the app, at every launch's first landing (review A-9).
pub(crate) fn answers(cls: &AnyClass, init: Sel) -> Result<(), String> {
    for sel in [init, sel!(performWithWMBridgeDelegate)] {
        if !cls.responds_to(sel) {
            return Err(format!("{} does not answer {}", cls.name().to_string_lossy(), sel.name().to_string_lossy()));
        }
    }
    Ok(())
}

/// `dict[key]`.
pub(crate) fn get(dict: &NSDictionary, key: &str) -> Option<Retained<AnyObject>> {
    let k = NSString::from_str(key);
    dict.objectForKey(&k)
}

/// `dict[key]` as an unsigned number.
pub(crate) fn number(dict: &NSDictionary, key: &str) -> Option<u64> {
    get(dict, key).and_then(|v| v.downcast::<NSNumber>().ok()).map(|n| n.as_u64())
}

/// Takes a +1 CFArray as an `NSArray` (toll-free bridged).
///
/// # Safety
/// `p` must be null or a +1 reference to a CFArray.
pub(crate) unsafe fn take_array(p: *mut c_void) -> Option<Retained<NSArray>> {
    // SAFETY: the caller's contract; CFArray and NSArray are toll-free bridged.
    unsafe { Retained::from_raw(p.cast::<NSArray>()) }
}

pub(crate) fn numbers(a: &NSArray) -> Vec<u64> {
    a.iter().filter_map(|v| v.downcast::<NSNumber>().ok()).map(|n| n.as_u64()).collect()
}

pub(crate) fn wid_array(ids: &[u32]) -> Retained<NSArray<NSNumber>> {
    NSArray::from_retained_slice(&ids.iter().map(|&i| NSNumber::new_i32(i as i32)).collect::<Vec<_>>())
}

pub(crate) fn sid_array(ids: &[u64]) -> Retained<NSArray<NSNumber>> {
    NSArray::from_retained_slice(&ids.iter().map(|&i| NSNumber::new_u64(i)).collect::<Vec<_>>())
}

/// Which family of calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// `SLSSpaceCreate`, `SLSShowSpaces`, … (C functions).
    Classic,
    /// `SLSBridged…Operation` classes through `-performWithWMBridgeDelegate`.
    Bridged,
}

impl Route {
    #[cfg(feature = "probes")]
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Bridged => "bridged",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dictionary of `pairs`, as the window server's are read (`NSDictionary` of objects).
    fn dict(pairs: &[(&str, &AnyObject)]) -> Retained<NSDictionary> {
        let keys: Vec<Retained<NSString>> = pairs.iter().map(|(k, _)| NSString::from_str(k)).collect();
        let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let vals: Vec<&AnyObject> = pairs.iter().map(|(_, v)| *v).collect();
        let d = NSDictionary::from_slices(&keys, &vals);
        // SAFETY: an NSDictionary of NSString keys is an NSDictionary of objects.
        unsafe { Retained::cast_unchecked(d) }
    }

    fn space(id: u64, kind: i64, uuid: Option<&str>) -> Retained<NSDictionary> {
        let id = NSNumber::new_u64(id);
        let kind = NSNumber::new_i64(kind);
        let uuid = uuid.map(NSString::from_str);
        let mut pairs: Vec<(&str, &AnyObject)> = vec![("id64", &id), ("type", &kind)];
        if let Some(u) = &uuid {
            pairs.push(("uuid", u));
        }
        dict(&pairs)
    }

    /// One screen's entry as `SLSCopyManagedDisplaySpaces` gives it (design r3 §1): the first
    /// Desktop with an empty uuid, a full-screen app's Space (type 4) between two Desktops
    /// and shown, a Desktop with no uuid key at all.
    #[test]
    fn a_screens_entry_parses_into_its_desktops_and_other_spaces() {
        let spaces = [space(1, 0, Some("")), space(9, 4, None), space(4, 0, Some("6F1C-AB")), space(12, 0, None)];
        let refs: Vec<&NSDictionary> = spaces.iter().map(|s| &**s).collect();
        let list = NSArray::from_slice(&refs);
        let current = space(9, 4, None);
        let screen = NSString::from_str("37D8832A-2D66-02CA-B9F7-8F30A301B230");
        let entry = dict(&[("Display Identifier", &screen), ("Current Space", &current), ("Spaces", &list)]);
        let d = ScreenDesks::parse(&entry);
        assert_eq!(d.screen, "37D8832A-2D66-02CA-B9F7-8F30A301B230");
        assert_eq!((d.current, d.current_kind), (9, 4), "a full-screen app's Space shows");
        let desk = |id, uuid: &str| Desk { id, uuid: uuid.into() };
        assert_eq!(d.desks, vec![desk(1, ""), desk(4, "6F1C-AB"), desk(12, "")], "Desktops only, an empty uuid kept empty");
        assert_eq!(d.others, vec![(9, 4)]);
        assert_eq!(d.every().collect::<Vec<_>>(), vec![1, 4, 12, 9], "every Space, Desktops first");
        // The Current Space without its type: the type of the Space listed under its id.
        let current = dict(&[("id64", &*NSNumber::new_u64(4))]);
        let entry = dict(&[("Display Identifier", &*NSString::from_str("Main")), ("Current Space", &current), ("Spaces", &list)]);
        let d = ScreenDesks::parse(&entry);
        assert_eq!((d.screen.as_str(), d.current, d.current_kind), ("Main", 4, 0));
        // An entry with nothing in it.
        let d = ScreenDesks::parse(&dict(&[]));
        assert_eq!(d, ScreenDesks { current_kind: -1, ..ScreenDesks::default() });
    }

    /// Review A-9: a class whose instances do not answer the operation's initializer, or the
    /// perform, is refused before anything is sent (the send would raise and abort).
    #[test]
    fn a_class_that_does_not_answer_the_operation_is_refused() {
        let object = AnyClass::get(c"NSObject").expect("NSObject");
        let refused = answers(object, sel!(initWithSpaceID:)).expect_err("NSObject has no such initializer");
        assert!(refused.contains("initWithSpaceID:"), "{refused}");
        let refused = answers(object, sel!(init)).expect_err("nor the perform");
        assert!(refused.contains("performWithWMBridgeDelegate"), "{refused}");
    }
}
