//! The menu bar status item and its menu.
//!
//! Menu items call a small Objective-C target ([`MenuTarget`]) that forwards each click as a
//! [`MenuAction`] to the handler installed with [`set_action_handler`]. Main thread only.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString, ns_string};

use crate::platform::permissions::PermissionState;

/// What a menu click asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    GrantAccessibility,
    GrantScreenRecording,
    ToggleStrip,
    ReloadConfig,
    OpenConfig,
    Quit,
}

/// The installed receiver of menu clicks.
type ActionHandler = Box<dyn Fn(MenuAction)>;

thread_local! {
    static HANDLER: RefCell<Option<ActionHandler>> = const { RefCell::new(None) };
}

/// Installs the function that receives menu clicks (main thread).
pub fn set_action_handler(handler: impl Fn(MenuAction) + 'static) {
    HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(handler)));
}

fn dispatch(action: MenuAction) {
    // Take the handler out while it runs so a handler that re-enters the menu cannot hit a
    // RefCell borrow panic inside an Objective-C callback.
    let handler = HANDLER.with(|h| h.borrow_mut().take());
    if let Some(handler) = handler {
        handler(action);
        HANDLER.with(|h| {
            let mut slot = h.borrow_mut();
            if slot.is_none() {
                *slot = Some(handler);
            }
        });
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; MenuTarget does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "SMMenuTarget"]
    pub struct MenuTarget;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        // SAFETY (all methods): action signature `- (void)name:(id)sender`.
        #[unsafe(method(grantAccessibility:))]
        fn grant_accessibility(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::GrantAccessibility);
        }

        #[unsafe(method(grantScreenRecording:))]
        fn grant_screen_recording(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::GrantScreenRecording);
        }

        #[unsafe(method(toggleStrip:))]
        fn toggle_strip(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::ToggleStrip);
        }

        #[unsafe(method(reloadConfig:))]
        fn reload_config(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::ReloadConfig);
        }

        #[unsafe(method(openConfig:))]
        fn open_config(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::OpenConfig);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::Quit);
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// What the menu shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuState {
    pub permissions: PermissionState,
    /// Apple's Stage Manager is on (`GloballyEnabled = 1`).
    pub apple_stage_manager_on: bool,
    pub strip_visible: bool,
}

/// The status item, its menu and the items that change.
pub struct StatusMenu {
    _item: Retained<NSStatusItem>,
    _target: Retained<MenuTarget>,
    state_line: Retained<NSMenuItem>,
    warning_line: Retained<NSMenuItem>,
    grant_accessibility: Retained<NSMenuItem>,
    grant_screen_recording: Retained<NSMenuItem>,
    toggle_strip: Retained<NSMenuItem>,
}

fn item(mtm: MainThreadMarker, title: &str, action: Option<Sel>, target: &MenuTarget) -> Retained<NSMenuItem> {
    // SAFETY: valid title, selector implemented by `target`, empty key equivalent.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), action, ns_string!(""))
    };
    if action.is_some() {
        // SAFETY: the target is retained by StatusMenu for as long as the menu exists.
        unsafe { item.setTarget(Some(target)) };
    }
    item
}

impl StatusMenu {
    /// Creates the status item with the `rectangle.stack` symbol (text title as fallback).
    pub fn new(mtm: MainThreadMarker) -> Self {
        let status_item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status_item.button(mtm) {
            let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("rectangle.stack"),
                Some(ns_string!("Stage Manager")),
            );
            match image {
                Some(image) => {
                    image.setTemplate(true);
                    button.setImage(Some(&image));
                }
                None => button.setTitle(ns_string!("SM")),
            }
            tracing::debug!(symbol = button.image().is_some(), visible = status_item.isVisible(), "status item created");
        } else {
            tracing::warn!("status item has no button");
        }

        let target = MenuTarget::new(mtm);
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);

        let state_line = item(mtm, "", None, &target);
        state_line.setEnabled(false);
        let warning_line = item(mtm, "Apple's Stage Manager is on — turn it off", None, &target);
        warning_line.setEnabled(false);
        let grant_accessibility = item(mtm, "Grant Accessibility…", Some(sel!(grantAccessibility:)), &target);
        let grant_screen_recording = item(mtm, "Grant Screen Recording…", Some(sel!(grantScreenRecording:)), &target);
        let toggle_strip = item(mtm, "Hide Strip", Some(sel!(toggleStrip:)), &target);
        let reload = item(mtm, "Reload Config", Some(sel!(reloadConfig:)), &target);
        let open = item(mtm, "Open Config", Some(sel!(openConfig:)), &target);
        let quit = item(mtm, "Quit Stage Manager", Some(sel!(quit:)), &target);

        for it in [&state_line, &warning_line, &grant_accessibility, &grant_screen_recording] {
            menu.addItem(it);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        for it in [&toggle_strip, &reload, &open] {
            menu.addItem(it);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&quit);
        status_item.setMenu(Some(&menu));

        Self {
            _item: status_item,
            _target: target,
            state_line,
            warning_line,
            grant_accessibility,
            grant_screen_recording,
            toggle_strip,
        }
    }

    /// Shows `state`: the state line, the Apple Stage Manager warning, the grant items
    /// (only while missing) and the strip toggle's title.
    pub fn update(&self, state: &MenuState) {
        self.state_line.setTitle(&NSString::from_str(&state.permissions.summary()));
        self.warning_line.setHidden(!state.apple_stage_manager_on);
        self.grant_accessibility.setHidden(state.permissions.accessibility);
        self.grant_screen_recording.setHidden(state.permissions.screen_recording);
        let strip = if state.strip_visible { "Hide Strip" } else { "Show Strip" };
        self.toggle_strip.setTitle(&NSString::from_str(strip));
    }
}
