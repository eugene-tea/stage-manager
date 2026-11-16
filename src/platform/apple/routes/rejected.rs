//! The routes tried and rejected (docs/STRATEGIES.md, "Tried and rejected"), kept as
//! tripwires: reachable only through `ctl apple try` (with `STAGE_MANAGER_TEST=1`), and the
//! live check (`scripts/test_apple.py rejected`) asserts each still does nothing, so a macOS
//! update that makes one work turns the check red and says so.
//!
//! - [`ax_action`] `AXPress` / `AXAddToStage` on a strip item: success returned, nothing
//!   happens (11-16, macOS 27.2).
//! - [`pid_click`]: a Shift-click posted to WindowManager's pid only (`CGEventPostToPid`):
//!   ignored.
//! - [`shift_focus`]: a real Shift held around the focus route: it switches, it does not add.

use std::time::Duration;

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton};

use crate::engine::geometry::Point;
use crate::platform::apple::strip_ax;
use crate::platform::ax::AxElement;
use crate::platform::controller::ax_focus;

/// `action` (`AXPress` or `AXAddToStage`) on the strip item of WindowManager `manager` that
/// holds window `window`.
pub fn ax_action(manager: i32, window: u32, action: &str) -> Result<(), String> {
    strip_ax::perform(manager, window, action)
}

/// A Shift-click at `at` posted to WindowManager `manager`'s pid alone, the pointer never
/// moved.
pub fn pid_click(manager: i32, at: Point) -> Result<(), String> {
    for kind in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
        let event = CGEvent::new_mouse_event(None, kind, CGPoint { x: at.x, y: at.y }, CGMouseButton::Left).ok_or("cannot create a mouse event")?;
        CGEvent::set_flags(Some(&event), CGEventFlags::MaskShift);
        CGEvent::post_to_pid(manager, Some(&event));
    }
    Ok(())
}

/// The focus route on `element` (app `pid`) with a real Shift key held around it.
pub fn shift_focus(element: &AxElement, pid: i32) -> Result<(), String> {
    const SHIFT_KEY: u16 = 56;
    let key = |down: bool| {
        if let Some(e) = CGEvent::new_keyboard_event(None, SHIFT_KEY, down) {
            CGEvent::set_flags(Some(&e), if down { CGEventFlags::MaskShift } else { CGEventFlags::empty() });
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
        }
    };
    key(true);
    std::thread::sleep(Duration::from_millis(12));
    let focused = ax_focus(element, pid).map_err(|e| e.to_string());
    std::thread::sleep(Duration::from_millis(12));
    key(false);
    focused
}
