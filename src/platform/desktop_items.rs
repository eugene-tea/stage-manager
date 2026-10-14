//! Finder's desktop icons, read through public Accessibility on a helper thread of their own
//! ([`start_read`]): the corner patch draws the desktop picture only, so it stays off while
//! an icon lies under it, or while where the icons lie is not known
//! ([`crate::engine::corner_patch::desktop_icons`], [`crate::engine::corner_patch::item_reach`]).
//!
//! Finder's application element lists one `AXScrollArea` per display's desktop, holding an
//! `AXGroup` (the icon view; scroll bars are its other children) whose `AXImage` children are
//! the icons (the icon's image frame; the label below it is not an element). Where Finder
//! keeps icon places on disk is not public, and neither is where desktop widgets sit
//! (WindowManager draws them in one full-screen window and lists no element for them), so
//! widgets are only logged as possibly covered.
//!
//! **Cost.** Every Accessibility message runs at Finder's short timeout
//! ([`FINDER_AX_TIMEOUT_SECONDS`], set on each element asked, as a timeout is an element's
//! own), the first failure ends the read, each icon costs one message for its role and frame,
//! and at most [`MAX_ICONS`] are read. The read runs on a thread of its own, never the WM
//! thread: a slow or hung Finder, or a crowded desktop, costs the WM thread nothing.

use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::{CFBoolean, CFNumber, CFPreferencesAppSynchronize, CFPreferencesCopyAppValue, CFString};
use objc2_foundation::NSString;
use std::sync::mpsc;

use super::ax::AxElement;
use super::windows::FINDER_BUNDLE_ID;
use crate::engine::corner_patch::{AxNode, DesktopIcons, MAX_ICONS, desktop_icons, finder_desktop_on, roles};

/// The Accessibility timeout for each message to Finder.
pub const FINDER_AX_TIMEOUT_SECONDS: f32 = 0.1;

/// Reads Finder's desktop icons now (any thread but the main one's hot paths: up to a
/// timeout per message). `None` when Finder does not answer (no Accessibility, a timeout, a
/// message failing); [`DesktopIcons::Unexpected`] when it answers with a tree other than
/// the one expected. No Finder running is unreadable too (`None`, tried again shortly), never
/// an empty desktop: Finder may be relaunching, and its icons are back once it is.
pub fn finder_icons() -> Option<DesktopIcons> {
    let finder = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(FINDER_BUNDLE_ID))
        .iter()
        .find(|a| !a.isTerminated())
        .map(|a| a.processIdentifier());
    let pid = finder?;
    let app = read_tree(&AxElement::application(pid))?;
    Some(desktop_icons(finder_desktop_on(create_desktop().as_deref()), &app))
}

/// Starts [`finder_icons`] on a thread of its own; the answer comes on the channel (none
/// when the thread could not start).
pub fn start_read() -> Option<mpsc::Receiver<Option<DesktopIcons>>> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("finder-desktop-icons".into()).spawn(move || {
        // A panic here must not take the app down (Fable M2): the icons are unknown, so the
        // patch stays off, and the read is tried again later.
        let read = super::safety::guarded("the read of Finder's desktop icons", finder_icons)
            .unwrap_or(Some(DesktopIcons::Unexpected("the read panicked")));
        let _ = tx.send(read);
    });
    match spawned {
        Ok(_) => Some(rx),
        Err(e) => {
            tracing::warn!("corner patch: cannot start a read of Finder's desktop icons: {e}");
            None
        }
    }
}

/// The part of Finder's tree [`desktop_icons`] needs: the application's children's roles,
/// each scroll area's children's roles, and each icon view's children's roles and frames
/// (unless there are more than [`MAX_ICONS`] in all). Every message bounded; the first
/// failure ends the read.
fn read_tree(finder: &AxElement) -> Option<Vec<AxNode>> {
    let t = FINDER_AX_TIMEOUT_SECONDS;
    let mut read = 0usize;
    let mut app = Vec::new();
    for child in finder.bounded(t, AxElement::children).ok()? {
        let role = child.bounded(t, AxElement::role).ok()?;
        if role != roles::SCROLL_AREA {
            app.push(AxNode::leaf(&role, None));
            continue;
        }
        let mut area = Vec::new();
        for part in child.bounded(t, AxElement::children).ok()? {
            let role = part.bounded(t, AxElement::role).ok()?;
            if role != roles::GROUP {
                area.push(AxNode::leaf(&role, None));
                continue;
            }
            let items = part.bounded(t, AxElement::children).ok()?;
            read += items.len();
            if read > MAX_ICONS {
                area.push(AxNode { count: items.len(), ..AxNode::new(&role, Vec::new()) });
                continue;
            }
            let mut icons = Vec::with_capacity(items.len());
            for item in items {
                let (role, frame) = item.bounded(t, AxElement::role_and_frame).ok()?;
                icons.push(AxNode::leaf(&role, frame));
            }
            area.push(AxNode::new(&role, icons));
        }
        app.push(AxNode::new(&role, area));
    }
    Some(app)
}

/// `com.apple.finder` `CreateDesktop` as text (absent: `None`).
fn create_desktop() -> Option<String> {
    let domain = CFString::from_static_str("com.apple.finder");
    let key = CFString::from_static_str("CreateDesktop");
    CFPreferencesAppSynchronize(&domain);
    let value = CFPreferencesCopyAppValue(&key, &domain)?;
    if let Some(b) = value.downcast_ref::<CFBoolean>() {
        return Some(b.as_bool().to_string());
    }
    if let Some(n) = value.downcast_ref::<CFNumber>() {
        return n.as_i64().map(|n| n.to_string());
    }
    value.downcast_ref::<CFString>().map(|s| s.to_string())
}
