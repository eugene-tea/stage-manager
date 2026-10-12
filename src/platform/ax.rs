//! Accessibility wrappers: [`AxElement`] over `AXUIElement`, and [`AxObserver`] per app.
//!
//! Every call returns `Result<_, AxError>` and never panics. Call these on the WM thread
//! only: AX messaging blocks for up to the messaging timeout on a hung app.

use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;

use objc2_application_services::{AXError as RawAxError, AXObserver, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFRetained, CFRunLoop, CFRunLoopSource, CFString, CFType, CGPoint, CGSize,
    kCFRunLoopDefaultMode,
};

use super::geometry::{Point, Rect, Size};

/// Attribute, action and value names used by the app.
pub mod names {
    pub const ROLE: &str = "AXRole";
    pub const SUBROLE: &str = "AXSubrole";
    pub const TITLE: &str = "AXTitle";
    pub const POSITION: &str = "AXPosition";
    pub const SIZE: &str = "AXSize";
    pub const WINDOWS: &str = "AXWindows";
    pub const FOCUSED_WINDOW: &str = "AXFocusedWindow";
    pub const MAIN: &str = "AXMain";
    pub const MINIMIZED: &str = "AXMinimized";
    pub const FULLSCREEN: &str = "AXFullScreen";
    pub const FRONTMOST: &str = "AXFrontmost";
    pub const RAISE: &str = "AXRaise";

    pub const ROLE_WINDOW: &str = "AXWindow";
    pub const SUBROLE_STANDARD: &str = "AXStandardWindow";
}

/// AX notification names observed by the app.
pub mod notifications {
    pub const WINDOW_CREATED: &str = "AXWindowCreated";
    pub const FOCUSED_WINDOW_CHANGED: &str = "AXFocusedWindowChanged";
    pub const APPLICATION_ACTIVATED: &str = "AXApplicationActivated";
    pub const ELEMENT_DESTROYED: &str = "AXUIElementDestroyed";
    pub const WINDOW_MOVED: &str = "AXWindowMoved";
    pub const WINDOW_RESIZED: &str = "AXWindowResized";
    pub const WINDOW_MINIATURIZED: &str = "AXWindowMiniaturized";

    /// Registered on each application element.
    pub const APP_LEVEL: [&str; 3] = [WINDOW_CREATED, FOCUSED_WINDOW_CHANGED, APPLICATION_ACTIVATED];
    /// Registered on each tracked window element.
    pub const WINDOW_LEVEL: [&str; 4] =
        [ELEMENT_DESTROYED, WINDOW_MOVED, WINDOW_RESIZED, WINDOW_MINIATURIZED];
}

/// An Accessibility failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxError {
    /// The API returned this `AXError` code.
    Api(i32),
    /// The attribute exists but holds an unexpected type.
    WrongType(&'static str),
    /// A value could not be created or decoded.
    Value,
}

impl AxError {
    /// `kAXErrorAPIDisabled`: the process is not trusted for Accessibility.
    pub const API_DISABLED: Self = Self::Api(-25211);
    /// `kAXErrorNoValue`: the attribute has no value.
    pub const NO_VALUE: Self = Self::Api(-25212);
    /// `kAXErrorCannotComplete`: messaging failed (hung app, app still launching).
    pub const CANNOT_COMPLETE: Self = Self::Api(-25204);
    /// `kAXErrorAttributeUnsupported`.
    pub const ATTRIBUTE_UNSUPPORTED: Self = Self::Api(-25205);
    /// `kAXErrorNotificationAlreadyRegistered`.
    pub const ALREADY_REGISTERED: Self = Self::Api(-25209);

    fn check(code: RawAxError) -> Result<(), AxError> {
        if code == RawAxError::Success { Ok(()) } else { Err(Self::Api(code.0)) }
    }
}

impl fmt::Display for AxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(code) => {
                let name = match code {
                    -25200 => "Failure",
                    -25201 => "IllegalArgument",
                    -25202 => "InvalidUIElement",
                    -25203 => "InvalidUIElementObserver",
                    -25204 => "CannotComplete",
                    -25205 => "AttributeUnsupported",
                    -25206 => "ActionUnsupported",
                    -25207 => "NotificationUnsupported",
                    -25208 => "NotImplemented",
                    -25209 => "NotificationAlreadyRegistered",
                    -25210 => "NotificationNotRegistered",
                    -25211 => "APIDisabled",
                    -25212 => "NoValue",
                    _ => "Unknown",
                };
                write!(f, "AX error {code} ({name})")
            }
            Self::WrongType(attr) => write!(f, "AX attribute {attr} has an unexpected type"),
            Self::Value => write!(f, "AX value could not be converted"),
        }
    }
}

impl std::error::Error for AxError {}

/// Result of an Accessibility call.
pub type AxResult<T> = Result<T, AxError>;

unsafe extern "C" {
    /// Private but stable: the `CGWindowID` behind an AX window element. The one private
    /// symbol the project allows.
    fn _AXUIElementGetWindow(element: &AXUIElement, window_id: *mut u32) -> RawAxError;
}

/// An Accessibility element (application, window, or the system-wide element).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AxElement(CFRetained<AXUIElement>);

// SAFETY: AXUIElementRef is an immutable CF handle that the AX API accepts from any thread;
// the app still confines AX calls to the WM thread for blocking reasons, not safety.
unsafe impl Send for AxElement {}
// SAFETY: as above; the handle is never mutated through a shared reference.
unsafe impl Sync for AxElement {}

impl fmt::Debug for AxElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AxElement({:p})", CFRetained::as_ptr(&self.0))
    }
}

impl AxElement {
    /// The application element of `pid`.
    pub fn application(pid: libc::pid_t) -> Self {
        // SAFETY: creating an element for any pid is valid; a dead pid yields errors later.
        Self(unsafe { AXUIElement::new_application(pid) })
    }

    /// The system-wide element.
    pub fn system_wide() -> Self {
        // SAFETY: no preconditions.
        Self(unsafe { AXUIElement::new_system_wide() })
    }

    /// Wraps a borrowed element from a callback, retaining it.
    ///
    /// # Safety
    /// `ptr` must point at a live `AXUIElement`.
    pub unsafe fn from_borrowed(ptr: NonNull<AXUIElement>) -> Self {
        // SAFETY: the caller guarantees `ptr` is a live element; retain balances our drop.
        Self(unsafe { CFRetained::retain(ptr) })
    }

    /// The raw element.
    pub fn raw(&self) -> &AXUIElement {
        &self.0
    }

    /// Sets the messaging timeout; on the system-wide element it becomes the global default.
    pub fn set_messaging_timeout(&self, seconds: f32) -> AxResult<()> {
        // SAFETY: valid element; plain float argument.
        AxError::check(unsafe { self.0.set_messaging_timeout(seconds) })
    }

    /// The process id owning this element.
    pub fn pid(&self) -> AxResult<libc::pid_t> {
        let mut pid: libc::pid_t = 0;
        // SAFETY: `pid` is a valid out pointer.
        AxError::check(unsafe { self.0.pid(NonNull::from(&mut pid)) })?;
        Ok(pid)
    }

    /// Copies an attribute value.
    pub fn attribute(&self, name: &str) -> AxResult<CFRetained<CFType>> {
        let attr = CFString::from_str(name);
        let mut value: *const CFType = std::ptr::null();
        // SAFETY: `value` is a valid out pointer; on success it holds a +1 reference.
        let code = unsafe { self.0.copy_attribute_value(&attr, NonNull::from(&mut value)) };
        AxError::check(code)?;
        let ptr = NonNull::new(value.cast_mut()).ok_or(AxError::NO_VALUE)?;
        // SAFETY: the Copy rule hands us ownership of one reference.
        Ok(unsafe { CFRetained::from_raw(ptr) })
    }

    /// Sets an attribute value.
    pub fn set_attribute(&self, name: &str, value: &CFType) -> AxResult<()> {
        let attr = CFString::from_str(name);
        // SAFETY: valid element, attribute name and CF value.
        AxError::check(unsafe { self.0.set_attribute_value(&attr, value) })
    }

    /// Performs an action such as `AXRaise`.
    pub fn perform_action(&self, name: &str) -> AxResult<()> {
        let action = CFString::from_str(name);
        // SAFETY: valid element and action name.
        AxError::check(unsafe { self.0.perform_action(&action) })
    }

    fn string_attr(&self, name: &'static str) -> AxResult<String> {
        let v = self.attribute(name)?;
        v.downcast_ref::<CFString>().map(|s| s.to_string()).ok_or(AxError::WrongType(name))
    }

    fn bool_attr(&self, name: &'static str) -> AxResult<bool> {
        let v = self.attribute(name)?;
        v.downcast_ref::<CFBoolean>().map(|b| b.as_bool()).ok_or(AxError::WrongType(name))
    }

    fn set_bool_attr(&self, name: &'static str, value: bool) -> AxResult<()> {
        self.set_attribute(name, CFBoolean::new(value))
    }

    fn element_attr(&self, name: &'static str) -> AxResult<AxElement> {
        let v = self.attribute(name)?;
        v.downcast::<AXUIElement>().map(AxElement).map_err(|_| AxError::WrongType(name))
    }

    fn value_attr<T: Default>(&self, name: &'static str, kind: AXValueType) -> AxResult<T> {
        let v = self.attribute(name)?;
        let value = v.downcast_ref::<AXValue>().ok_or(AxError::WrongType(name))?;
        let mut out = T::default();
        // SAFETY: `out` has the C layout of `kind` (CGPoint/CGSize) and is writable.
        let ok = unsafe { value.value(kind, NonNull::from(&mut out).cast::<c_void>()) };
        if ok { Ok(out) } else { Err(AxError::Value) }
    }

    fn set_value_attr<T>(&self, name: &'static str, kind: AXValueType, value: &T) -> AxResult<()> {
        // SAFETY: `value` has the C layout of `kind` and outlives the call.
        let ax_value = unsafe { AXValue::new(kind, NonNull::from(value).cast::<c_void>()) }
            .ok_or(AxError::Value)?;
        self.set_attribute(name, &ax_value)
    }

    /// `AXRole`.
    pub fn role(&self) -> AxResult<String> {
        self.string_attr(names::ROLE)
    }

    /// `AXSubrole`.
    pub fn subrole(&self) -> AxResult<String> {
        self.string_attr(names::SUBROLE)
    }

    /// `AXTitle`.
    pub fn title(&self) -> AxResult<String> {
        self.string_attr(names::TITLE)
    }

    /// `AXPosition` (top-left origin).
    pub fn position(&self) -> AxResult<Point> {
        let p: CGPoint = self.value_attr(names::POSITION, AXValueType::CGPoint)?;
        Ok(Point { x: p.x, y: p.y })
    }

    /// `AXSize`.
    pub fn size(&self) -> AxResult<Size> {
        let s: CGSize = self.value_attr(names::SIZE, AXValueType::CGSize)?;
        Ok(Size { w: s.width, h: s.height })
    }

    /// Position and size together.
    pub fn frame(&self) -> AxResult<Rect> {
        Ok(Rect::from_parts(self.position()?, self.size()?))
    }

    /// `AXWindows` of an application element.
    pub fn windows(&self) -> AxResult<Vec<AxElement>> {
        let v = self.attribute(names::WINDOWS)?;
        let array = v.downcast::<CFArray>().map_err(|_| AxError::WrongType(names::WINDOWS))?;
        // SAFETY: AXWindows is documented as an array of AXUIElements; each item is
        // type-checked again below.
        let array: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(array) };
        Ok(array.iter().filter_map(|item| item.downcast::<AXUIElement>().ok().map(AxElement)).collect())
    }

    /// `AXFocusedWindow` of an application element.
    pub fn focused_window(&self) -> AxResult<AxElement> {
        self.element_attr(names::FOCUSED_WINDOW)
    }

    /// `AXMain` of a window.
    pub fn is_main(&self) -> AxResult<bool> {
        self.bool_attr(names::MAIN)
    }

    /// `AXMinimized` of a window.
    pub fn is_minimized(&self) -> AxResult<bool> {
        self.bool_attr(names::MINIMIZED)
    }

    /// `AXFullScreen` of a window; a window without the attribute is not fullscreen.
    pub fn is_fullscreen(&self) -> AxResult<bool> {
        match self.bool_attr(names::FULLSCREEN) {
            Err(e) if e == AxError::ATTRIBUTE_UNSUPPORTED || e == AxError::NO_VALUE => Ok(false),
            other => other,
        }
    }

    /// Sets `AXPosition`.
    pub fn set_position(&self, p: Point) -> AxResult<()> {
        self.set_value_attr(names::POSITION, AXValueType::CGPoint, &CGPoint { x: p.x, y: p.y })
    }

    /// Sets `AXSize`.
    pub fn set_size(&self, s: Size) -> AxResult<()> {
        self.set_value_attr(names::SIZE, AXValueType::CGSize, &CGSize { width: s.w, height: s.h })
    }

    /// Sets the frame: size, then position, then size again (a window that cannot grow past
    /// a display edge at its old position gets its full size once moved).
    pub fn set_frame(&self, r: Rect) -> AxResult<()> {
        let first = self.set_size(r.size());
        self.set_position(r.origin())?;
        self.set_size(r.size()).or(first)
    }

    /// `AXRaise` on a window.
    pub fn raise(&self) -> AxResult<()> {
        self.perform_action(names::RAISE)
    }

    /// Sets `AXMain` on a window.
    pub fn set_main(&self, main: bool) -> AxResult<()> {
        self.set_bool_attr(names::MAIN, main)
    }

    /// Sets `AXFrontmost` on an application element.
    pub fn set_frontmost(&self, frontmost: bool) -> AxResult<()> {
        self.set_bool_attr(names::FRONTMOST, frontmost)
    }

    /// The `CGWindowID` of a window element (via `_AXUIElementGetWindow`).
    pub fn window_id(&self) -> AxResult<u32> {
        let mut id: u32 = 0;
        // SAFETY: valid element and out pointer; the function only writes `id`.
        AxError::check(unsafe { _AXUIElementGetWindow(&self.0, &mut id) })?;
        if id == 0 { Err(AxError::NO_VALUE) } else { Ok(id) }
    }
}

/// The C callback type of an observer.
pub type ObserverCallback =
    unsafe extern "C-unwind" fn(NonNull<AXObserver>, NonNull<AXUIElement>, NonNull<CFString>, *mut c_void);

/// An `AXObserver` for one process, attached to the run loop it was created on.
pub struct AxObserver {
    observer: CFRetained<AXObserver>,
    source: CFRetained<CFRunLoopSource>,
    run_loop: CFRetained<CFRunLoop>,
    pid: libc::pid_t,
}

impl fmt::Debug for AxObserver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AxObserver(pid {})", self.pid)
    }
}

impl AxObserver {
    /// Creates an observer for `pid` and adds its source to the current thread's run loop
    /// (default mode).
    pub fn new(pid: libc::pid_t, callback: ObserverCallback) -> AxResult<Self> {
        let mut raw: *mut AXObserver = std::ptr::null_mut();
        // SAFETY: `raw` is a valid out pointer; the callback has the documented signature.
        let code = unsafe { AXObserver::create(pid, Some(callback), NonNull::from(&mut raw)) };
        AxError::check(code)?;
        let ptr = NonNull::new(raw).ok_or(AxError::Value)?;
        // SAFETY: AXObserverCreate follows the Create rule (+1 reference).
        let observer = unsafe { CFRetained::from_raw(ptr) };
        // SAFETY: valid observer.
        let source = unsafe { observer.run_loop_source() };
        let run_loop = CFRunLoop::current().ok_or(AxError::Value)?;
        // SAFETY: the default-mode constant is a valid static.
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
        Ok(Self { observer, source, run_loop, pid })
    }

    /// The observed process.
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// Registers `notification` on `element`; `refcon` (an opaque integer, never
    /// dereferenced) is handed back to the callback.
    pub fn add(&self, element: &AxElement, notification: &str, refcon: usize) -> AxResult<()> {
        let name = CFString::from_str(notification);
        let refcon = refcon as *mut c_void;
        // SAFETY: valid observer, element and name; refcon is an opaque integer for us.
        let code = unsafe { self.observer.add_notification(element.raw(), &name, refcon) };
        match AxError::check(code) {
            Err(e) if e == AxError::ALREADY_REGISTERED => Ok(()),
            other => other,
        }
    }

    /// Unregisters `notification` on `element`.
    pub fn remove(&self, element: &AxElement, notification: &str) -> AxResult<()> {
        let name = CFString::from_str(notification);
        // SAFETY: valid observer, element and name.
        AxError::check(unsafe { self.observer.remove_notification(element.raw(), &name) })
    }
}

impl Drop for AxObserver {
    fn drop(&mut self) {
        // SAFETY: the default-mode constant is a valid static; removing an absent source is
        // a no-op.
        self.run_loop.remove_source(Some(&self.source), unsafe { kCFRunLoopDefaultMode });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_name_their_codes() {
        assert_eq!(AxError::API_DISABLED.to_string(), "AX error -25211 (APIDisabled)");
        assert_eq!(AxError::Api(-1).to_string(), "AX error -1 (Unknown)");
        assert!(AxError::WrongType("AXRole").to_string().contains("AXRole"));
    }

    #[test]
    fn notification_sets_are_disjoint() {
        for n in notifications::APP_LEVEL {
            assert!(!notifications::WINDOW_LEVEL.contains(&n));
        }
    }
}
