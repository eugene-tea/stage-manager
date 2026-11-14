//! `stage-manager probe-spaces …`: the Spaces spike (1.9, S0). Measures whether this process
//! can run stages as hidden window-server Spaces: a Space it creates, a fixture app's windows
//! moved into it, the Space hidden and shown, its transform stepped, a new window placed in it
//! before it shows, and what is left after a `kill -9`.
//!
//! Every call is loaded at run time: SkyLight's C functions with `dlopen`/`dlsym`, its bridged
//! operations (`SLSBridged…Operation`) as Objective-C classes looked up by name and sent
//! through `objc_msgSend` (also found with `dlsym`). Nothing new is linked. Two routes:
//! `classic` (`SLSSpaceCreate`, `SLSShowSpaces`, …) and `bridged` (the operation classes,
//! performed with `-performWithWMBridgeDelegate`, which needs AppKit loaded: the probe makes
//! itself an accessory `NSApplication` first). The signatures come from public sources only;
//! the spike's notes list each with its source.
//!
//! Refuses any bundle id but a fixture's (`dev.eugene.stagemanager.fixture…`) and touches only
//! that app's layer-0 windows. Every Space it makes is written to
//! `/tmp/sm-spaces-probe/spaces.txt` before windows go into it; `probe-spaces cleanup`
//! brings every fixture window back to the Desktop shown and destroys those Spaces.
//!
//! Routes per step: `--route R` (classic or bridged, default bridged) for create, show, hide,
//! level, transform and destroy; `--create R` and `--add R` override it for the create and
//! for putting windows into a Space. What the spike found (macOS 27.2): only the bridged add
//! moves another app's window, and a classic Space dies with its creator, so `--route classic
//! --add bridged` is the combination that brings windows back by itself after a `kill -9`.
//!
//! Subcommands (one JSON object on stdout each, `hide` and `place-new` one line per event):
//! - `read [BUNDLE]`: symbols and classes found, the Desktops, every Space, the recorded
//!   Spaces, the fixture's windows and their Spaces.
//! - `cycle BUNDLE [--back move|show] [--rounds N] [--level L] [--managed]`: create, then N
//!   rounds of hide and show (back to the Desktop, or the Space shown), timed to the window
//!   list with this process's CPU per call, then clean up.
//! - `hide BUNDLE [--hold S] [--show-space]`: hide the windows in a new Space (then show that
//!   Space) and hold S seconds (for a `kill -9` or the Mission Control checks), then bring
//!   them back.
//! - `transform BUNDLE [--steps N] [--scale K] [--shown] [--pace-ms MS] [--hold-ms MS]
//!   [no-shots]`: the windows in a new Space, its transform stepped from identity to a scale
//!   K, each step timed, read back and checked in the window list; screenshots of the halfway
//!   step and the last.
//! - `place-new BUNDLE [--for S]`: a hidden Space waits; every fixture window that appears in
//!   the next S seconds goes into it as soon as it is listed, on screen or not yet.
//! - `watch BUNDLE [--for S] [--every MS]`: prints each change of the fixture windows'
//!   on-screen flag and Spaces (after a `kill -9` of a holder).
//! - `space-op show|hide|destroy SPACE`: one call on a recorded Space (an orphan).
//! - `key mission-control|app-windows|escape|cmd-tab|left|right`: posts that shortcut.
//! - `dock-click BUNDLE`: presses the fixture's Dock item through Accessibility.
//! - `cleanup [BUNDLE]`: every fixture window back to the Desktop shown, every recorded Space
//!   shown and destroyed (both routes), the record emptied.

use std::ffi::{CStr, c_void};
use std::io::Write as _;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::sel;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSRunningApplication};
use objc2_core_foundation::{CFRunLoop, CGAffineTransform, kCFRunLoopDefaultMode};
use objc2_foundation::{MainThreadMarker, NSArray, NSDictionary, NSNumber, NSString};
use serde_json::{Value, json};

use super::park_probe::FIXTURE_PREFIX;
use super::windows::{self, CgWindow};

/// Where the probe keeps its record of the Spaces it made, and screenshots.
pub const PROBE_DIR: &str = "/tmp/sm-spaces-probe";

/// The SkyLight framework's binary.
const SKYLIGHT: &CStr = c"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";

/// `SLSCopySpaces`/`SLSCopySpacesForWindows` mask: every kind of Space (synara lists
/// orphans with 0xf; yabai and sunlight pass 0x7, which leaves the probe's Spaces out).
const ALL_SPACES_MASK: i32 = 0xf;

/// `SLSSpaceAddWindowsAndRemoveFromSpaces` options as SketchyBar and sunlight pass them.
const ADD_REMOVE_OPTIONS: i32 = 7;

type FnMain = unsafe extern "C" fn() -> i32;
type FnCopyByCid = unsafe extern "C" fn(i32) -> *mut c_void;
type FnCopySpaces = unsafe extern "C" fn(i32, i32) -> *mut c_void;
type FnCopySpacesForWindows = unsafe extern "C" fn(i32, i32, *const c_void) -> *mut c_void;
type FnSpaceCreate = unsafe extern "C" fn(i32, i32, i32) -> u64;
type FnSpaceDestroy = unsafe extern "C" fn(i32, u64) -> i32;
type FnSpaceLevel = unsafe extern "C" fn(i32, u64, i32) -> i32;
type FnSpaceList = unsafe extern "C" fn(i32, *const c_void) -> i32;
type FnAddRemove = unsafe extern "C" fn(i32, u64, *const c_void, i32) -> i32;
type FnMoveManaged = unsafe extern "C" fn(i32, *const c_void, u64) -> i32;
type FnSetTransform = unsafe extern "C" fn(i32, u64, *const CGAffineTransform, i32) -> i32;
type FnGetTransform = unsafe extern "C" fn(i32, u64, *mut i32) -> CGAffineTransform;
type FnSpaceType = unsafe extern "C" fn(i32, u64) -> i32;
type FnActiveSpace = unsafe extern "C" fn(i32) -> u64;

/// One display's Desktops: its uuid, the Space it shows, and each Space with its type.
type DisplaySpaces = (String, u64, Vec<(u64, i64)>);

/// The C functions, each `None` when SkyLight does not export it.
struct Sky {
    cid: i32,
    copy_managed: Option<FnCopyByCid>,
    copy_spaces: Option<FnCopySpaces>,
    spaces_for_windows: Option<FnCopySpacesForWindows>,
    create: Option<FnSpaceCreate>,
    destroy: Option<FnSpaceDestroy>,
    level: Option<FnSpaceLevel>,
    show: Option<FnSpaceList>,
    hide: Option<FnSpaceList>,
    add_remove: Option<FnAddRemove>,
    move_managed: Option<FnMoveManaged>,
    set_transform: Option<FnSetTransform>,
    get_transform: Option<FnGetTransform>,
    space_type: Option<FnSpaceType>,
    active: Option<FnActiveSpace>,
    msg_send: *const c_void,
    names: Vec<(&'static str, bool)>,
}

/// The function `name` from `handle`, as the type the caller names.
///
/// # Safety
/// `F` must be an `extern "C"` function pointer type matching the symbol's real signature.
unsafe fn load<F: Copy>(handle: *mut c_void, name: &'static CStr, names: &mut Vec<(&'static str, bool)>) -> Option<F> {
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
    fn open() -> Result<Self, String> {
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

    /// The Desktops per display: (display uuid, current Space, [(Space, type)]).
    fn displays(&self) -> Vec<DisplaySpaces> {
        let Some(f) = self.copy_managed else { return Vec::new() };
        // SAFETY: returns a +1 CFArray of CFDictionary (or null).
        let Some(list) = (unsafe { take_array(f(self.cid)) }) else { return Vec::new() };
        let mut out = Vec::new();
        for d in list.iter() {
            let Ok(d) = d.downcast::<NSDictionary>() else { continue };
            let uuid = get(&d, "Display Identifier").and_then(|v| v.downcast::<NSString>().ok()).map(|s| s.to_string()).unwrap_or_default();
            let current = get(&d, "Current Space")
                .and_then(|v| v.downcast::<NSDictionary>().ok())
                .and_then(|c| number(&c, "id64"))
                .unwrap_or(0);
            let mut spaces = Vec::new();
            if let Some(list) = get(&d, "Spaces").and_then(|v| v.downcast::<NSArray>().ok()) {
                for s in list.iter() {
                    if let Ok(s) = s.downcast::<NSDictionary>() {
                        spaces.push((number(&s, "id64").unwrap_or(0), number(&s, "type").map_or(-1, |t| t as i64)));
                    }
                }
            }
            out.push((uuid, current, spaces));
        }
        out
    }

    /// The main display's Desktop shown and its uuid.
    fn current(&self) -> (u64, String) {
        self.displays().into_iter().next().map(|(u, c, _)| (c, u)).unwrap_or((0, String::new()))
    }

    /// Every Space the window server lists for this mask.
    fn all_spaces(&self, mask: i32) -> Vec<u64> {
        let Some(f) = self.copy_spaces else { return Vec::new() };
        // SAFETY: returns a +1 CFArray of CFNumber (or null).
        unsafe { take_array(f(self.cid, mask)) }.map(|a| numbers(&a)).unwrap_or_default()
    }

    /// The Spaces each window is on.
    fn spaces_of(&self, ids: &[u32]) -> Vec<u64> {
        let Some(f) = self.spaces_for_windows else { return Vec::new() };
        let arr = wid_array(ids);
        // SAFETY: a CFArray of window numbers; returns a +1 CFArray of CFNumber (or null).
        unsafe { take_array(f(self.cid, ALL_SPACES_MASK, Retained::as_ptr(&arr).cast())) }.map(|a| numbers(&a)).unwrap_or_default()
    }

    fn space_type(&self, sid: u64) -> Option<i32> {
        // SAFETY: plain query.
        self.space_type.map(|f| unsafe { f(self.cid, sid) })
    }

    fn get_transform(&self, sid: u64) -> Option<[f64; 6]> {
        let f = self.get_transform?;
        let mut options = 0i32;
        // SAFETY: returns the transform by value (indirect result); `options` is writable.
        let t = unsafe { f(self.cid, sid, &mut options) };
        Some([t.a, t.b, t.c, t.d, t.tx, t.ty])
    }

    /// The class `name`, when the runtime has it.
    fn class(name: &CStr) -> Option<&'static AnyClass> {
        AnyClass::get(name)
    }

    /// `[[cls alloc] <sel>]` with one object argument, performed and released; the result of
    /// `-performWithWMBridgeDelegate` (null for an asynchronous operation).
    fn bridged(&self, class: &CStr, build: impl FnOnce(*mut AnyObject) -> *mut AnyObject) -> Result<*mut AnyObject, String> {
        let Some(cls) = Self::class(class) else { return Err(format!("{} is missing", class.to_str().unwrap_or("?"))) };
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

    fn send_obj(&self) -> unsafe extern "C" fn(*mut AnyObject, Sel, *const c_void) -> *mut AnyObject {
        // SAFETY: (id, SEL, id) -> id.
        unsafe { std::mem::transmute(self.msg_send) }
    }

    fn create(&self, route: Route, managed: bool) -> Result<u64, String> {
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
                let result = self.bridged(c"SLSBridgedSpaceCreateOperation", |raw| {
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

    fn destroy(&self, route: Route, sid: u64) -> Result<i32, String> {
        match route {
            Route::Classic => {
                let f = self.destroy.ok_or("SLSSpaceDestroy is missing")?;
                // SAFETY: (cid, sid).
                Ok(unsafe { f(self.cid, sid) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let ms = self.msg_send;
                self.bridged(c"SLSBridgedSpaceDestroyOperation", |raw| {
                    // SAFETY: `-initWithSpaceID:` (id, SEL, uint64) -> id.
                    let init: unsafe extern "C" fn(*mut AnyObject, Sel, u64) -> *mut AnyObject = unsafe { std::mem::transmute(ms) };
                    // SAFETY: as typed.
                    unsafe { init(raw, sel!(initWithSpaceID:), sid) }
                })
                .map(|_| 0)
            }),
        }
    }

    fn set_level(&self, route: Route, sid: u64, level: i32) -> Result<i32, String> {
        match route {
            Route::Classic => {
                let f = self.level.ok_or("SLSSpaceSetAbsoluteLevel is missing")?;
                // SAFETY: (cid, sid, level).
                Ok(unsafe { f(self.cid, sid, level) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let ms = self.msg_send;
                self.bridged(c"SLSBridgedSpaceSetAbsoluteLevelOperation", |raw| {
                    // SAFETY: `-initWithSpaceID:level:` (id, SEL, uint64, int) -> id.
                    let init: unsafe extern "C" fn(*mut AnyObject, Sel, u64, i32) -> *mut AnyObject = unsafe { std::mem::transmute(ms) };
                    // SAFETY: as typed.
                    unsafe { init(raw, sel!(initWithSpaceID:level:), sid, level) }
                })
                .map(|_| 0)
            }),
        }
    }

    fn show_hide(&self, route: Route, sids: &[u64], show: bool) -> Result<i32, String> {
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
                self.bridged(class, |raw| {
                    // SAFETY: `-initWithSpaces:` (id, SEL, NSArray) -> id; `arr` outlives it.
                    unsafe { send(raw, sel!(initWithSpaces:), Retained::as_ptr(&arr).cast()) }
                })
                .map(|_| 0)
            }),
        }
    }

    /// The windows into Space `sid`, out of every other Space.
    fn add_remove(&self, route: Route, sid: u64, ids: &[u32]) -> Result<i32, String> {
        let arr = wid_array(ids);
        match route {
            Route::Classic => {
                let f = self.add_remove.ok_or("SLSSpaceAddWindowsAndRemoveFromSpaces is missing")?;
                // SAFETY: (cid, sid, CFArray of window ids, 7).
                Ok(unsafe { f(self.cid, sid, Retained::as_ptr(&arr).cast(), ADD_REMOVE_OPTIONS) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let ms = self.msg_send;
                self.bridged(c"SLSBridgedSpaceAddWindowsAndRemoveFromSpacesOperation", |raw| {
                    // SAFETY: `-initWithSpaceID:windows:options:` (id, SEL, uint64, NSArray, uint32) -> id.
                    let init: unsafe extern "C" fn(*mut AnyObject, Sel, u64, *const c_void, u32) -> *mut AnyObject =
                        unsafe { std::mem::transmute(ms) };
                    // SAFETY: as typed; `arr` outlives the call.
                    unsafe { init(raw, sel!(initWithSpaceID:windows:options:), sid, Retained::as_ptr(&arr).cast(), ADD_REMOVE_OPTIONS as u32) }
                })
                .map(|_| 0)
            }),
        }
    }

    /// The windows onto the managed Space (a Desktop) `sid`.
    fn move_managed(&self, route: Route, ids: &[u32], sid: u64) -> Result<i32, String> {
        let arr = wid_array(ids);
        match route {
            Route::Classic => {
                let f = self.move_managed.ok_or("SLSMoveWindowsToManagedSpace is missing")?;
                // SAFETY: (cid, CFArray of window ids, sid).
                Ok(unsafe { f(self.cid, Retained::as_ptr(&arr).cast(), sid) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let ms = self.msg_send;
                self.bridged(c"SLSBridgedMoveWindowsToManagedSpaceOperation", |raw| {
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

    fn set_transform(&self, route: Route, sid: u64, t: CGAffineTransform) -> Result<i32, String> {
        match route {
            Route::Classic => {
                let f = self.set_transform.ok_or("SLSSpaceSetTransform is missing")?;
                // SAFETY: (cid, sid, &transform, options 0); on arm64 a 48-byte struct passes by
                // reference either way.
                Ok(unsafe { f(self.cid, sid, &t, 0) })
            }
            Route::Bridged => autoreleasepool(|_| {
                let ms = self.msg_send;
                self.bridged(c"SLSBridgedSpaceSetTransformOperation", |raw| {
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

/// `dict[key]`.
fn get(dict: &NSDictionary, key: &str) -> Option<Retained<AnyObject>> {
    let k = NSString::from_str(key);
    dict.objectForKey(&k)
}

/// `dict[key]` as an unsigned number.
fn number(dict: &NSDictionary, key: &str) -> Option<u64> {
    get(dict, key).and_then(|v| v.downcast::<NSNumber>().ok()).map(|n| n.as_u64())
}

/// Takes a +1 CFArray as an `NSArray` (toll-free bridged).
///
/// # Safety
/// `p` must be null or a +1 reference to a CFArray.
unsafe fn take_array(p: *mut c_void) -> Option<Retained<NSArray>> {
    // SAFETY: the caller's contract; CFArray and NSArray are toll-free bridged.
    unsafe { Retained::from_raw(p.cast::<NSArray>()) }
}

fn numbers(a: &NSArray) -> Vec<u64> {
    a.iter().filter_map(|v| v.downcast::<NSNumber>().ok()).map(|n| n.as_u64()).collect()
}

fn wid_array(ids: &[u32]) -> Retained<NSArray<NSNumber>> {
    NSArray::from_retained_slice(&ids.iter().map(|&i| NSNumber::new_i32(i as i32)).collect::<Vec<_>>())
}

fn sid_array(ids: &[u64]) -> Retained<NSArray<NSNumber>> {
    NSArray::from_retained_slice(&ids.iter().map(|&i| NSNumber::new_u64(i)).collect::<Vec<_>>())
}

/// Which family of calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// `SLSSpaceCreate`, `SLSShowSpaces`, … (C functions).
    Classic,
    /// `SLSBridged…Operation` classes through `-performWithWMBridgeDelegate`.
    Bridged,
}

impl Route {
    fn name(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Bridged => "bridged",
        }
    }
}

/// Parsed options shared by the subcommands.
struct Opts {
    bundle: Option<String>,
    route: Route,
    create_route: Option<Route>,
    add_route: Option<Route>,
    show_space: bool,
    back_show: bool,
    rounds: usize,
    level: Option<i32>,
    managed: bool,
    hold: f64,
    steps: usize,
    scale: f64,
    shown: bool,
    pace_ms: f64,
    hold_ms: u64,
    every_ms: u64,
    words: Vec<String>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        bundle: None,
        route: Route::Bridged,
        create_route: None,
        add_route: None,
        show_space: false,
        back_show: false,
        rounds: 5,
        level: None,
        managed: false,
        hold: 20.0,
        steps: 120,
        scale: 0.5,
        shown: false,
        pace_ms: 0.0,
        hold_ms: 300,
        every_ms: 2,
        words: Vec::new(),
    };
    let mut it = args.iter();
    let num = |v: Option<&String>, what: &str| v.and_then(|s| s.parse::<f64>().ok()).ok_or(format!("{what} takes a number"));
    while let Some(a) = it.next() {
        match a.as_str() {
            "--create" => {
                o.create_route = match it.next().map(String::as_str) {
                    Some("classic") => Some(Route::Classic),
                    Some("bridged") => Some(Route::Bridged),
                    _ => return Err("--create takes classic or bridged".into()),
                }
            }
            "--show-space" => o.show_space = true,
            "--add" => {
                o.add_route = match it.next().map(String::as_str) {
                    Some("classic") => Some(Route::Classic),
                    Some("bridged") => Some(Route::Bridged),
                    _ => return Err("--add takes classic or bridged".into()),
                }
            }
            "--route" => {
                o.route = match it.next().map(String::as_str) {
                    Some("classic") => Route::Classic,
                    Some("bridged") => Route::Bridged,
                    _ => return Err("--route takes classic or bridged".into()),
                }
            }
            "--back" => o.back_show = it.next().map(String::as_str) == Some("show"),
            "--rounds" => o.rounds = num(it.next(), "--rounds")? as usize,
            "--level" => o.level = Some(num(it.next(), "--level")? as i32),
            "--managed" => o.managed = true,
            "--hold" | "--for" => o.hold = num(it.next(), "--hold")?,
            "--steps" => o.steps = num(it.next(), "--steps")?.max(1.0) as usize,
            "--scale" => o.scale = num(it.next(), "--scale")?,
            "--shown" => o.shown = true,
            "--pace-ms" => o.pace_ms = num(it.next(), "--pace-ms")?,
            "--hold-ms" => o.hold_ms = num(it.next(), "--hold-ms")? as u64,
            "--every" => o.every_ms = num(it.next(), "--every")?.max(1.0) as u64,
            b if b.starts_with("dev.") && o.bundle.is_none() => o.bundle = Some(b.to_string()),
            w => o.words.push(w.to_string()),
        }
    }
    if let Some(b) = &o.bundle
        && !b.starts_with(FIXTURE_PREFIX)
    {
        return Err("probe-spaces moves windows: only a fixture app's (dev.eugene.stagemanager.fixture…)".into());
    }
    Ok(o)
}

fn print(v: Value) {
    println!("{v}");
    let _ = std::io::stdout().flush();
}

fn fail(code: u8, error: &str) -> ExitCode {
    print(json!({ "ok": false, "error": error }));
    ExitCode::from(code)
}

/// Runs the probe (see the module docs).
pub fn run(args: &[String]) -> ExitCode {
    let Some((cmd, rest)) = args.split_first() else {
        return fail(2, "usage: stage-manager probe-spaces read|cycle|hide|transform|place-new|watch|key|dock-click|cleanup …");
    };
    let o = match parse(rest) {
        Ok(o) => o,
        Err(e) => return fail(2, &e),
    };
    let Some(mtm) = MainThreadMarker::new() else { return fail(4, "not on the main thread") };
    // The bridge's delegate is AppKit's window coordinator: load AppKit as an accessory app
    // (no Dock icon, no menu bar).
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let sky = match Sky::open() {
        Ok(s) => s,
        Err(e) => return fail(3, &e),
    };
    let _ = std::fs::create_dir_all(PROBE_DIR);
    let need_bundle = || o.bundle.clone().ok_or("this subcommand takes a fixture bundle id".to_string());
    let result = match cmd.as_str() {
        "read" => Ok(read(&sky, o.bundle.as_deref())),
        "cycle" => need_bundle().and_then(|b| cycle(&sky, &b, &o)),
        "hide" => need_bundle().and_then(|b| hide_hold(&sky, &b, &o)),
        "transform" => need_bundle().and_then(|b| transform(&sky, &b, &o)),
        "place-new" => need_bundle().and_then(|b| place_new(&sky, &b, &o)),
        "watch" => need_bundle().map(|b| watch(&sky, &b, &o)),
        "key" => key(&o),
        "dock-click" => need_bundle().and_then(|b| dock_click(&b)),
        "cleanup" => Ok(cleanup(&sky, o.bundle.as_deref())),
        "space-op" => space_op(&sky, &o),
        _ => Err(format!("unknown subcommand {cmd}")),
    };
    match result {
        Ok(v) => {
            print(v);
            ExitCode::SUCCESS
        }
        Err(e) => {
            // Whatever failed, nothing of the fixture's stays hidden.
            let c = cleanup(&sky, o.bundle.as_deref());
            print(json!({ "ok": false, "error": e, "cleanup": c }));
            ExitCode::FAILURE
        }
    }
}

/// The fixture's processes.
fn fixture_pids(bundle: &str) -> Vec<i32> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle))
        .iter()
        .map(|a| a.processIdentifier())
        .collect()
}

/// Every fixture process's pid (any fixture bundle id).
fn all_fixture_pids() -> Vec<i32> {
    objc2_app_kit::NSWorkspace::sharedWorkspace()
        .runningApplications()
        .iter()
        .filter(|a| a.bundleIdentifier().is_some_and(|b| b.to_string().starts_with(FIXTURE_PREFIX)))
        .map(|a| a.processIdentifier())
        .collect()
}

/// The fixture's layer-0 windows that are on some Space, as the window server lists them
/// (the fixture's never-shown helper windows are on none and are left out).
fn fixture_windows(sky: &Sky, pids: &[i32]) -> Vec<CgWindow> {
    windows::list_windows()
        .into_iter()
        .filter(|w| pids.contains(&w.pid) && w.layer == 0 && (w.bounds.h > 50.0 || !sky.spaces_of(&[w.id]).is_empty()))
        .collect()
}

fn ids_of(list: &[CgWindow]) -> Vec<u32> {
    list.iter().map(|w| w.id).collect()
}

/// Lets the run loop turn for `d` (the bridge may answer on it).
fn pump(d: Duration) {
    // SAFETY: the framework's own constant.
    let mode = unsafe { kCFRunLoopDefaultMode };
    CFRunLoop::run_in_mode(mode, d.as_secs_f64(), false);
}

/// Polls the window list until every window's on-screen flag is `want`; the ms it took.
fn wait_onscreen(ids: &[u32], want: bool, limit: Duration) -> (Option<f64>, Vec<CgWindow>) {
    let t0 = Instant::now();
    loop {
        let list = windows::describe(ids);
        if list.len() == ids.len() && list.iter().all(|w| w.onscreen == want) {
            return (Some(ms(t0)), list);
        }
        if t0.elapsed() > limit {
            return (None, list);
        }
        pump(Duration::from_micros(250));
    }
}

fn ms(t0: Instant) -> f64 {
    (t0.elapsed().as_secs_f64() * 1e6).round() / 1e3
}

/// This process's CPU time (user + system), in ms.
fn cpu_ms() -> f64 {
    // SAFETY: an all-zero rusage is a valid out value.
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: RUSAGE_SELF with a writable struct.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let t = |v: libc::timeval| v.tv_sec as f64 * 1e3 + v.tv_usec as f64 / 1e3;
    t(u.ru_utime) + t(u.ru_stime)
}

fn frames(list: &[CgWindow]) -> Value {
    json!(list.iter().map(|w| json!({ "id": w.id, "onscreen": w.onscreen, "bounds": w.bounds, "alpha": w.alpha })).collect::<Vec<_>>())
}

// The record of the Spaces the probe made.

fn record_path() -> std::path::PathBuf {
    std::path::Path::new(PROBE_DIR).join("spaces.txt")
}

fn recorded() -> Vec<u64> {
    std::fs::read_to_string(record_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_whitespace().next()?.parse().ok())
        .collect()
}

fn record(sid: u64, route: Route) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(record_path()) {
        let _ = writeln!(f, "{sid} {} pid={}", route.name(), std::process::id());
    }
}

fn unrecord(sid: u64) {
    let keep: Vec<String> = std::fs::read_to_string(record_path())
        .unwrap_or_default()
        .lines()
        .filter(|l| l.split_whitespace().next().and_then(|s| s.parse::<u64>().ok()) != Some(sid))
        .map(String::from)
        .collect();
    let _ = std::fs::write(record_path(), if keep.is_empty() { String::new() } else { keep.join("\n") + "\n" });
}

/// A new Space by `route`, recorded, at `level` when given.
fn new_space(sky: &Sky, o: &Opts) -> Result<(u64, Value), String> {
    let t0 = Instant::now();
    let sid = sky.create(o.create_route.unwrap_or(o.route), o.managed)?;
    let create_ms = ms(t0);
    if sid == 0 {
        return Err("the create call returned Space 0".into());
    }
    record(sid, o.create_route.unwrap_or(o.route));
    let level = o.level.map(|l| sky.set_level(o.route, sid, l));
    let info = json!({
        "space": sid,
        "create_ms": create_ms,
        "type": sky.space_type(sid),
        "listed_all": sky.all_spaces(0xf).contains(&sid),
        "managed": sky.displays().iter().any(|d| d.2.iter().any(|s| s.0 == sid)),
        "level": level.map(|r| format!("{r:?}")),
    });
    Ok((sid, info))
}

/// Windows back to the Desktop shown, the Space shown and destroyed, both routes when needed.
fn tear_down(sky: &Sky, sid: u64, ids: &[u32], route: Route) -> Value {
    let (current, _) = sky.current();
    let mut steps = Vec::new();
    for r in [route, other(route)] {
        let still: Vec<u32> = windows::describe(ids).iter().filter(|w| !w.onscreen).map(|w| w.id).collect();
        if still.is_empty() {
            break;
        }
        steps.push(json!({ "route": r.name(), "move_back": format!("{:?}", sky.move_managed(r, &still, current)) }));
        let _ = wait_onscreen(&still, true, Duration::from_millis(500));
        let still: Vec<u32> = windows::describe(ids).iter().filter(|w| !w.onscreen).map(|w| w.id).collect();
        if !still.is_empty() {
            steps.push(json!({ "route": r.name(), "add_back": format!("{:?}", sky.add_remove(r, current, &still)) }));
            let _ = wait_onscreen(&still, true, Duration::from_millis(500));
        }
    }
    let _ = sky.show_hide(route, &[sid], true);
    let mut destroyed = false;
    for r in [route, other(route)] {
        let d = sky.destroy(r, sid);
        pump(Duration::from_millis(150));
        destroyed = !sky.all_spaces(0xf).contains(&sid);
        steps.push(json!({ "route": r.name(), "destroy": format!("{d:?}"), "gone": destroyed }));
        if destroyed {
            break;
        }
    }
    if destroyed {
        unrecord(sid);
    }
    let after = windows::describe(ids);
    json!({ "steps": steps, "destroyed": destroyed, "all_onscreen": after.iter().all(|w| w.onscreen), "frames": frames(&after) })
}

fn other(r: Route) -> Route {
    match r {
        Route::Classic => Route::Bridged,
        Route::Bridged => Route::Classic,
    }
}

fn read(sky: &Sky, bundle: Option<&str>) -> Value {
    let classes = [
        c"SLSBridgedSpaceCreateOperation",
        c"SLSBridgedSpaceDestroyOperation",
        c"SLSBridgedSpaceAddWindowsAndRemoveFromSpacesOperation",
        c"SLSBridgedHideSpacesOperation",
        c"SLSBridgedShowSpacesOperation",
        c"SLSBridgedSpaceSetTransformOperation",
        c"SLSBridgedSpaceSetAbsoluteLevelOperation",
        c"SLSBridgedMoveWindowsToManagedSpaceOperation",
    ];
    let windows = bundle.map(|b| {
        let list = fixture_windows(sky, &fixture_pids(b));
        list.iter().map(|w| json!({ "id": w.id, "onscreen": w.onscreen, "bounds": w.bounds, "spaces": sky.spaces_of(&[w.id]) })).collect::<Vec<_>>()
    });
    json!({
        "ok": true,
        "cid": sky.cid,
        "all_spaces": sky.all_spaces(ALL_SPACES_MASK),
        "symbols": sky.names.iter().map(|(n, f)| json!({ "name": n, "found": f })).collect::<Vec<_>>(),
        "classes": classes.iter().map(|c| json!({ "name": c.to_str().unwrap_or("?"), "found": Sky::class(c).is_some() })).collect::<Vec<_>>(),
        "active": sky.active.map(|f| unsafe { f(sky.cid) }), // SAFETY: plain query.
        "displays": sky.displays().iter().map(|(u, c, s)| json!({ "uuid": u, "current": c, "spaces": s })).collect::<Vec<_>>(),
        "recorded": recorded().iter().map(|&s| json!({ "space": s, "type": sky.space_type(s), "exists": sky.all_spaces(0xf).contains(&s) })).collect::<Vec<_>>(),
        "windows": windows,
    })
}

/// Q1 and Q6: hide and show rounds, timed to the window list.
fn cycle(sky: &Sky, bundle: &str, o: &Opts) -> Result<Value, String> {
    let list = fixture_windows(sky, &fixture_pids(bundle));
    let ids = ids_of(&list);
    if ids.is_empty() {
        return Err("the fixture has no windows".into());
    }
    if list.iter().any(|w| !w.onscreen) {
        return Err("a fixture window is not on screen before the run".into());
    }
    let before = frames(&list);
    let (current, _) = sky.current();
    let (sid, info) = new_space(sky, o)?;
    let mut rounds = Vec::new();
    let mut hidden_first = Value::Null;
    for i in 0..o.rounds {
        // Hide: into our Space (and the Space hidden; a new Space starts hidden).
        let c0 = cpu_ms();
        let t0 = Instant::now();
        let r = if o.back_show && i > 0 { sky.show_hide(o.route, &[sid], false) } else { sky.add_remove(o.add_route.unwrap_or(o.route), sid, &ids) };
        let call_ms = ms(t0);
        let call_cpu = cpu_ms() - c0;
        let (hide_ms, hidden) = wait_onscreen(&ids, false, Duration::from_secs(2));
        let spaces_hidden = sky.spaces_of(&ids);
        if i == 0 {
            hidden_first = json!({ "frames": frames(&hidden), "spaces": spaces_hidden, "listed": hidden.len() });
        }
        pump(Duration::from_millis(100));
        // Show: back to the Desktop, or our Space shown.
        let c1 = cpu_ms();
        let t1 = Instant::now();
        let s = if o.back_show { sky.show_hide(o.route, &[sid], true) } else { sky.add_remove(o.add_route.unwrap_or(o.route), current, &ids) };
        let show_call_ms = ms(t1);
        let show_cpu = cpu_ms() - c1;
        let (show_ms, shown) = wait_onscreen(&ids, true, Duration::from_secs(2));
        let kept = shown.iter().all(|w| list.iter().any(|b| b.id == w.id && b.bounds == w.bounds));
        rounds.push(json!({
            "hide": { "result": format!("{r:?}"), "call_ms": call_ms, "cpu_ms": call_cpu, "to_list_ms": hide_ms },
            "show": { "result": format!("{s:?}"), "call_ms": show_call_ms, "cpu_ms": show_cpu, "to_list_ms": show_ms, "frames_kept": kept },
            "spaces_shown": sky.spaces_of(&ids),
        }));
        if hide_ms.is_none() || show_ms.is_none() {
            break;
        }
        pump(Duration::from_millis(100));
    }
    let down = tear_down(sky, sid, &ids, o.route);
    Ok(json!({
        "ok": true, "route": o.route.name(), "back": if o.back_show { "show" } else { "move" },
        "desktop": current, "space": info, "windows": ids, "before": before, "hidden_first": hidden_first,
        "rounds": rounds, "teardown": down,
    }))
}

/// Q2 and Q5: hides the windows and holds.
fn hide_hold(sky: &Sky, bundle: &str, o: &Opts) -> Result<Value, String> {
    let list = fixture_windows(sky, &fixture_pids(bundle));
    let ids = ids_of(&list);
    if ids.is_empty() {
        return Err("the fixture has no windows".into());
    }
    let (sid, info) = new_space(sky, o)?;
    let t0 = Instant::now();
    let r = sky.add_remove(o.add_route.unwrap_or(o.route), sid, &ids);
    let (hide_ms, hidden) = wait_onscreen(&ids, false, Duration::from_secs(2));
    print(json!({
        "event": "hidden", "pid": std::process::id(), "space": info, "windows": ids, "call": format!("{r:?}"),
        "call_ms": ms(t0), "to_list_ms": hide_ms, "frames": frames(&hidden), "spaces": sky.spaces_of(&ids),
    }));
    let mut hidden = hidden;
    if o.show_space {
        let t1 = Instant::now();
        let r = sky.show_hide(o.route, &[sid], true);
        let (t, now) = wait_onscreen(&ids, true, Duration::from_secs(2));
        print(json!({ "event": "space shown", "call": format!("{r:?}"), "call_ms": ms(t1), "to_list_ms": t, "frames": frames(&now) }));
        hidden = now;
    }
    let until = Instant::now() + Duration::from_secs_f64(o.hold);
    let mut last: Vec<(u32, bool)> = hidden.iter().map(|w| (w.id, w.onscreen)).collect();
    while Instant::now() < until {
        pump(Duration::from_millis(20));
        let now = windows::describe(&ids);
        let state: Vec<(u32, bool)> = now.iter().map(|w| (w.id, w.onscreen)).collect();
        if state != last {
            print(json!({ "event": "changed", "t_ms": ms(t0), "frames": frames(&now), "spaces": sky.spaces_of(&ids) }));
            last = state;
        }
    }
    let down = tear_down(sky, sid, &ids, o.route);
    Ok(json!({ "ok": true, "event": "done", "teardown": down }))
}

/// Q3: the transform stepped on a Space holding the live windows.
fn transform(sky: &Sky, bundle: &str, o: &Opts) -> Result<Value, String> {
    let list = fixture_windows(sky, &fixture_pids(bundle));
    let ids = ids_of(&list);
    if ids.is_empty() {
        return Err("the fixture has no windows".into());
    }
    let (sid, info) = new_space(sky, o)?;
    let add = sky.add_remove(o.add_route.unwrap_or(o.route), sid, &ids);
    let _ = wait_onscreen(&ids, false, Duration::from_secs(1));
    let shown = if o.shown {
        let r = sky.show_hide(o.route, &[sid], true);
        let (t, _) = wait_onscreen(&ids, true, Duration::from_secs(1));
        json!({ "result": format!("{r:?}"), "to_list_ms": t })
    } else {
        Value::Null
    };
    let no_shots = o.words.iter().any(|w| w == "no-shots");
    let shot = |name: &str| {
        if no_shots {
            return None;
        }
        let path = format!("{PROBE_DIR}/{name}.png");
        let ok = std::process::Command::new("/usr/sbin/screencapture").args(["-x", &path]).status().is_ok_and(|s| s.success());
        if ok { Some(path) } else { None }
    };
    let before_shot = shot(&format!("transform-{}-before", o.route.name()));
    let initial = sky.get_transform(sid);
    let mut call_ms = Vec::new();
    let mut readback_ok = 0usize;
    let mut listed_ok = 0usize;
    let mut half_shot = None;
    let c0 = cpu_ms();
    let t0 = Instant::now();
    for i in 1..=o.steps {
        let k = 1.0 + (o.scale - 1.0) * i as f64 / o.steps as f64;
        // Scale about the top-left corner, moved right and down by up to 200 points.
        let off = 200.0 * i as f64 / o.steps as f64;
        let t = CGAffineTransform { a: k, b: 0.0, c: 0.0, d: k, tx: off, ty: off };
        let s = Instant::now();
        let _ = sky.set_transform(o.route, sid, t);
        call_ms.push(ms(s));
        if let Some(r) = sky.get_transform(sid)
            && (r[0] - k).abs() < 1e-6
        {
            readback_ok += 1;
        }
        // The window list reports the transformed size: did this step reach the server?
        if let (Some(first), Some(w0)) = (windows::describe(&ids[..1]).first(), list.first())
            && (first.bounds.w - w0.bounds.w / k).abs() < 2.0
        {
            listed_ok += 1;
        }
        if i == o.steps / 2 {
            pump(Duration::from_millis(o.hold_ms));
            half_shot = shot(&format!("transform-{}-half", o.route.name()));
        }
        if o.pace_ms > 0.0 {
            let due = Duration::from_secs_f64(o.pace_ms * i as f64 / 1e3);
            while t0.elapsed() < due {
                pump(Duration::from_micros(200));
            }
        }
    }
    let total = ms(t0);
    let cpu = cpu_ms() - c0;
    pump(Duration::from_millis(o.hold_ms));
    let end_shot = shot(&format!("transform-{}-end", o.route.name()));
    let listed = frames(&windows::describe(&ids));
    let _ = sky.set_transform(o.route, sid, CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 });
    let mut sorted = call_ms.clone();
    sorted.sort_by(f64::total_cmp);
    let pct = |p: f64| sorted.get(((sorted.len() as f64 - 1.0) * p).round() as usize).copied();
    let down = tear_down(sky, sid, &ids, o.route);
    Ok(json!({
        "ok": true, "route": o.route.name(), "space": info, "add": format!("{add:?}"), "shown": shown,
        "initial_transform": initial, "final_readback": sky.get_transform(sid),
        "steps": o.steps, "readback_matched": readback_ok, "listed_matched": listed_ok, "total_ms": total, "cpu_ms": cpu,
        "steps_per_s": (o.steps as f64 / (total / 1e3)).round(), "call_ms_p50": pct(0.5), "call_ms_p95": pct(0.95), "call_ms_max": pct(1.0),
        "listed_while_transformed": listed, "shots": [before_shot, half_shot, end_shot], "teardown": down,
    }))
}

/// Q4: new fixture windows into a hidden Space as soon as they are listed.
fn place_new(sky: &Sky, bundle: &str, o: &Opts) -> Result<Value, String> {
    let old: Vec<u32> = windows::list_windows().iter().filter(|w| w.layer == 0).map(|w| w.id).collect();
    let (sid, info) = new_space(sky, o)?;
    print(json!({ "event": "waiting", "space": info, "old": old.len() }));
    let t0 = Instant::now();
    let until = t0 + Duration::from_secs_f64(o.hold);
    // Per placed window: id, when first listed, on screen when listed, call ms, when first
    // listed off screen, on-screen samples after the call, samples.
    struct Placed {
        id: u32,
        listed: Instant,
        on_when_listed: bool,
        bounds: crate::engine::geometry::Rect,
        call: String,
        call_ms: f64,
        off_ms: Option<f64>,
        on_after: usize,
        samples: usize,
    }
    let mut placed: Vec<Placed> = Vec::new();
    let mut pids = fixture_pids(bundle);
    let mut last_pids = Instant::now();
    while Instant::now() < until {
        if last_pids.elapsed() > Duration::from_millis(20) {
            pids = fixture_pids(bundle);
            last_pids = Instant::now();
        }
        // One query of every window (on screen or not): a window may be listed before it shows.
        let all = windows::every_window();
        let now = Instant::now();
        for w in &all {
            if !pids.contains(&w.pid) || w.layer != 0 || w.bounds.h <= 50.0 || old.contains(&w.id) {
                continue;
            }
            if let Some(p) = placed.iter_mut().find(|p| p.id == w.id) {
                if now.duration_since(p.listed) < Duration::from_millis(1500) {
                    p.samples += 1;
                    if w.onscreen {
                        p.on_after += 1;
                    } else if p.off_ms.is_none() {
                        p.off_ms = Some((now.duration_since(p.listed).as_secs_f64() * 1e6).round() / 1e3);
                    }
                }
                continue;
            }
            let c = Instant::now();
            let r = sky.add_remove(o.add_route.unwrap_or(o.route), sid, &[w.id]);
            placed.push(Placed {
                id: w.id,
                listed: now,
                on_when_listed: w.onscreen,
                bounds: w.bounds,
                call: format!("{r:?}"),
                call_ms: ms(c),
                off_ms: None,
                on_after: 0,
                samples: 0,
            });
        }
        pump(Duration::from_micros(250));
    }
    let ids: Vec<u32> = placed.iter().map(|p| p.id).collect();
    let events: Vec<Value> = placed
        .iter()
        .map(|p| {
            json!({
                "id": p.id, "t_ms": (p.listed.duration_since(t0).as_secs_f64() * 1e3).round(), "onscreen_when_listed": p.on_when_listed,
                "bounds_when_listed": p.bounds, "call": p.call, "call_ms": p.call_ms, "listed_to_offscreen_ms": p.off_ms,
                "onscreen_samples_after": p.on_after, "samples": p.samples, "spaces": sky.spaces_of(&[p.id]),
            })
        })
        .collect();
    let down = tear_down(sky, sid, &ids, o.route);
    Ok(json!({ "ok": true, "event": "done", "placed": events, "teardown": down }))
}

/// Prints each change of the fixture windows' state for a while.
fn watch(sky: &Sky, bundle: &str, o: &Opts) -> Value {
    let t0 = Instant::now();
    let until = t0 + Duration::from_secs_f64(o.hold);
    let mut last = Value::Null;
    let mut changes = 0;
    while Instant::now() < until {
        let list = fixture_windows(sky, &fixture_pids(bundle));
        let state = json!(list.iter().map(|w| json!([w.id, w.onscreen, sky.spaces_of(&[w.id])])).collect::<Vec<_>>());
        if state != last {
            let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
            print(json!({ "event": "state", "t_ms": ms(t0), "epoch": epoch, "windows": state }));
            last = state;
            changes += 1;
        }
        pump(Duration::from_millis(o.every_ms));
    }
    json!({ "ok": true, "changes": changes })
}

fn key(o: &Opts) -> Result<Value, String> {
    use super::space_keys::{self, Direction, ESCAPE_KEY, Expose, SpaceKey};
    let word = o.words.first().map(String::as_str).unwrap_or("");
    let r = match word {
        "escape" => space_keys::post_plain(ESCAPE_KEY),
        // ⌘Tab once: to the app used before the front one.
        "cmd-tab" => space_keys::post_with_command(48),
        w => {
            if let Some(d) = Direction::parse(w) {
                space_keys::post(&SpaceKey::read(d), false)
            } else if let Some(e) = Expose::parse(w) {
                space_keys::post(&SpaceKey::read_expose(e), false)
            } else {
                return Err("key takes mission-control, app-windows, escape, cmd-tab, left or right".into());
            }
        }
    };
    r.map(|_| json!({ "ok": true, "key": word, "accessibility": super::permissions::accessibility_trusted() }))
}

fn dock_click(bundle: &str) -> Result<Value, String> {
    let dock = super::dock::dock_pid().ok_or("no Dock")?;
    let (item, info) = super::dock::item_for_bundle(dock, bundle).ok_or("the fixture has no Dock item")?;
    item.perform_action("AXPress").map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true, "pressed": info.path }))
}

/// `space-op show|hide|destroy SPACE`: one call on a Space the probe recorded (an orphan a
/// killed holder left), timed, with the fixture windows' state after.
fn space_op(sky: &Sky, o: &Opts) -> Result<Value, String> {
    let (Some(op), Some(sid)) = (o.words.first(), o.words.get(1).and_then(|s| s.parse::<u64>().ok())) else {
        return Err("space-op takes show|hide|destroy and a Space id".into());
    };
    if !recorded().contains(&sid) {
        return Err("space-op acts only on a Space the probe recorded".into());
    }
    let ids = ids_of(&fixture_windows(sky, &all_fixture_pids()));
    let t0 = Instant::now();
    let r = match op.as_str() {
        "show" => sky.show_hide(o.route, &[sid], true),
        "hide" => sky.show_hide(o.route, &[sid], false),
        "destroy" => sky.destroy(o.route, sid),
        _ => return Err("space-op takes show|hide|destroy".into()),
    };
    let call = ms(t0);
    let want = op != "hide";
    let (t, now) = wait_onscreen(&ids, want, Duration::from_secs(2));
    pump(Duration::from_millis(150));
    let exists = sky.all_spaces(ALL_SPACES_MASK).contains(&sid);
    if !exists {
        unrecord(sid);
    }
    Ok(json!({
        "ok": true, "op": op, "route": o.route.name(), "space": sid, "result": format!("{r:?}"), "call_ms": call,
        "to_list_ms": t, "exists_after": exists, "frames": frames(&now),
        "spaces": ids.iter().map(|&i| json!([i, sky.spaces_of(&[i])])).collect::<Vec<_>>(),
    }))
}

/// Every fixture window back to the Desktop shown; every recorded Space shown and destroyed.
fn cleanup(sky: &Sky, bundle: Option<&str>) -> Value {
    let pids = match bundle {
        Some(b) => fixture_pids(b),
        None => all_fixture_pids(),
    };
    let list = fixture_windows(sky, &pids);
    let ids = ids_of(&list);
    let (current, _) = sky.current();
    let mut steps = Vec::new();
    let off: Vec<u32> = list.iter().filter(|w| !w.onscreen).map(|w| w.id).collect();
    if !off.is_empty() {
        for r in [Route::Bridged, Route::Classic] {
            let still: Vec<u32> = windows::describe(&off).iter().filter(|w| !w.onscreen).map(|w| w.id).collect();
            if still.is_empty() {
                break;
            }
            steps.push(json!({ "route": r.name(), "move": format!("{:?}", sky.move_managed(r, &still, current)) }));
            let _ = wait_onscreen(&still, true, Duration::from_millis(500));
            let still: Vec<u32> = windows::describe(&off).iter().filter(|w| !w.onscreen).map(|w| w.id).collect();
            if !still.is_empty() {
                steps.push(json!({ "route": r.name(), "add": format!("{:?}", sky.add_remove(r, current, &still)) }));
                let _ = wait_onscreen(&still, true, Duration::from_millis(500));
            }
        }
    }
    let mut spaces = Vec::new();
    for sid in recorded() {
        let mut gone = !sky.all_spaces(0xf).contains(&sid);
        if !gone {
            let _ = sky.show_hide(Route::Bridged, &[sid], true);
            let _ = sky.show_hide(Route::Classic, &[sid], true);
            for r in [Route::Bridged, Route::Classic] {
                let _ = sky.destroy(r, sid);
                pump(Duration::from_millis(150));
                gone = !sky.all_spaces(0xf).contains(&sid);
                if gone {
                    break;
                }
            }
        }
        if gone {
            unrecord(sid);
        }
        spaces.push(json!({ "space": sid, "gone": gone }));
    }
    let after = windows::describe(&ids);
    json!({
        "ok": after.iter().all(|w| w.onscreen),
        "windows": frames(&after),
        "minimized_or_hidden_note": "a fixture window may be off screen for its own reasons (minimized): check frames",
        "steps": steps,
        "spaces": spaces,
    })
}
