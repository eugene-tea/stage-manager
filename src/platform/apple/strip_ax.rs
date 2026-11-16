//! Apple's strip as WindowManager's Accessibility shows it, read only (two probes, 11-16,
//! macOS 27.2): under WindowManager's application element an `AXGroup` `sm.space` per
//! screen, in it the `AXList` `sm.strip`, in that one `AXButton` per item, each with
//! `AXFrame` and `AXWindowsIDs` (the `CGWindowID`s of that stage). Only the items the strip shows are listed; the current
//! stage is not; after every change the strip reads empty for about 650 ms. The items'
//! titles and descriptions (window titles) are never read, stored or logged. Their actions
//! (`AXPress`, `AXAddToStage`) return success and do nothing: never used here.
//!
//! Modelled on [`super::super::switcher_ax`]: every call bounded by [`STRIP_AX_SECONDS`], the
//! first call left unanswered ends the read.

use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;

use crate::engine::apple::mirror::{Item, Reading};
use crate::platform::ax::{AxElement, AxError, AxResult};

/// WindowManager's bundle id.
pub const BUNDLE: &str = "com.apple.WindowManager";

/// Each Accessibility call to WindowManager (seconds): a read every 2 s at rest, never one
/// that holds the WM thread up.
pub const STRIP_AX_SECONDS: f32 = 0.05;

/// `AXIdentifier`.
const IDENTIFIER: &str = "AXIdentifier";
/// `AXFrame`.
const FRAME: &str = "AXFrame";
/// `AXWindowsIDs`, WindowManager's own.
const WINDOWS_IDS: &str = "AXWindowsIDs";
/// The screen's group.
const SPACE_ID: &str = "sm.space";
/// The strip's list.
const STRIP_ID: &str = "sm.strip";

/// WindowManager's pid, when it runs.
pub fn manager_pid() -> Option<i32> {
    let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(BUNDLE));
    apps.iter().next().map(|a| a.processIdentifier())
}

/// The strips of every screen (`sm.space`, in WindowManager's order: the main screen first)
/// of WindowManager `pid`, read now; `None` when WindowManager does not answer.
pub fn read(pid: i32) -> Option<Vec<Reading>> {
    let app = AxElement::application(pid);
    let spaces = match app.bounded(STRIP_AX_SECONDS, AxElement::children) {
        Ok(c) => c,
        Err(e) => {
            tracing::trace!("apple mode: WindowManager's Accessibility did not answer: {e}");
            return None;
        }
    };
    let mut out = Vec::new();
    for space in spaces {
        match identified(&space) {
            Ok(id) if id == SPACE_ID => {}
            Ok(_) => continue,
            Err(AxError::CANNOT_COMPLETE) => return None,
            Err(_) => continue,
        }
        match strip_of(&space) {
            Ok(r) => out.push(r),
            Err(AxError::CANNOT_COMPLETE) => return None,
            Err(_) => out.push(Reading::default()),
        }
    }
    Some(out)
}

/// `element`'s `AXIdentifier`.
fn identified(element: &AxElement) -> AxResult<String> {
    element.bounded(STRIP_AX_SECONDS, |e| e.text(IDENTIFIER))
}

/// The strip of screen group `space`: its `sm.strip`'s items.
fn strip_of(space: &AxElement) -> AxResult<Reading> {
    let t = STRIP_AX_SECONDS;
    for child in space.bounded(t, AxElement::children)? {
        if identified(&child).ok().as_deref() != Some(STRIP_ID) {
            continue;
        }
        let mut items = Vec::new();
        for item in child.bounded(t, AxElement::children)? {
            let frame = item.bounded(t, |e| e.rect(FRAME)).unwrap_or_default();
            let windows = item.bounded(t, |e| e.numbers(WINDOWS_IDS)).unwrap_or_default();
            items.push(Item { frame, windows: windows.into_iter().filter_map(|n| u32::try_from(n).ok()).collect() });
        }
        return Ok(Reading { items });
    }
    Ok(Reading::default())
}
