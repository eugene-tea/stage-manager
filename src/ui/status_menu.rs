//! The menu bar status item and its menu.
//!
//! Menu items call a small Objective-C target ([`MenuTarget`]) that forwards each click as a
//! [`MenuAction`] to the handler installed with [`set_action_handler`]. Main thread only.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString, ns_string};

use crate::platform::permissions::PermissionState;
use crate::ui::glyph;

/// What a menu click asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    GrantAccessibility,
    GrantScreenRecording,
    ToggleStrip,
    ReloadConfig,
    OpenConfig,
    /// Write a problem mark into the day's log (shown while the log is verbose).
    MarkProblem,
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

        #[unsafe(method(markProblem:))]
        fn mark_problem(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::MarkProblem);
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuState {
    pub permissions: PermissionState,
    /// Apple's Stage Manager is on (`GloballyEnabled = 1`).
    pub apple_stage_manager_on: bool,
    pub strip_visible: bool,
    /// config.toml failed to load (the last good config stays in force).
    pub config_error: Option<String>,
    /// Show Mark a Problem (the log is verbose: the trial default).
    pub mark_shown: bool,
    /// When the last problem mark was written (`HH:MM:SS`).
    pub last_mark: Option<String>,
    /// macOS refused window captures; "Grant Screen Recording…" shows and retries them.
    pub capture_refused: bool,
}

/// The Mark a Problem item's title: when the last mark went in, if any.
pub fn mark_title(last: Option<&str>) -> String {
    match last {
        Some(at) => format!("Mark a Problem (last at {at})"),
        None => "Mark a Problem".to_string(),
    }
}

/// The menu's config line: hidden (`None`) while the config is fine.
pub fn config_line(error: Option<&str>) -> Option<String> {
    const MAX: usize = 90;
    let e = error?;
    let mut text = format!("Config error: {e}");
    if text.chars().count() > MAX {
        text = text.chars().take(MAX - 1).collect::<String>() + "…";
    }
    Some(text)
}

/// The status item, its menu and the items that change.
pub struct StatusMenu {
    _item: Retained<NSStatusItem>,
    _target: Retained<MenuTarget>,
    state_line: Retained<NSMenuItem>,
    config_line: Retained<NSMenuItem>,
    warning_line: Retained<NSMenuItem>,
    grant_accessibility: Retained<NSMenuItem>,
    grant_screen_recording: Retained<NSMenuItem>,
    toggle_strip: Retained<NSMenuItem>,
    mark: Retained<NSMenuItem>,
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
    /// Creates the status item with the app's glyph ([`glyph::status_image`]).
    pub fn new(mtm: MainThreadMarker) -> Self {
        let status_item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status_item.button(mtm) {
            button.setImage(Some(&glyph::status_image()));
            tracing::debug!(visible = status_item.isVisible(), "status item created");
        } else {
            tracing::warn!("status item has no button");
        }

        let target = MenuTarget::new(mtm);
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);

        let state_line = item(mtm, "", None, &target);
        state_line.setEnabled(false);
        let config_line = item(mtm, "", None, &target);
        config_line.setEnabled(false);
        config_line.setHidden(true);
        let warning_line = item(mtm, "Apple's Stage Manager is on — turn it off", None, &target);
        warning_line.setEnabled(false);
        let grant_accessibility = item(mtm, "Grant Accessibility…", Some(sel!(grantAccessibility:)), &target);
        let grant_screen_recording = item(mtm, "Grant Screen Recording…", Some(sel!(grantScreenRecording:)), &target);
        let toggle_strip = item(mtm, "Hide Strip", Some(sel!(toggleStrip:)), &target);
        let reload = item(mtm, "Reload Config", Some(sel!(reloadConfig:)), &target);
        let open = item(mtm, "Open Config", Some(sel!(openConfig:)), &target);
        let mark = item(mtm, &mark_title(None), Some(sel!(markProblem:)), &target);
        let quit = item(mtm, "Quit Stage Manager", Some(sel!(quit:)), &target);

        for it in [&state_line, &config_line, &warning_line, &grant_accessibility, &grant_screen_recording] {
            menu.addItem(it);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        for it in [&toggle_strip, &reload, &open, &mark] {
            menu.addItem(it);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&quit);
        status_item.setMenu(Some(&menu));

        Self {
            _item: status_item,
            _target: target,
            state_line,
            config_line,
            warning_line,
            grant_accessibility,
            grant_screen_recording,
            toggle_strip,
            mark,
        }
    }

    /// Shows `state`: the state line, the config error (only while there is one), the Apple
    /// Stage Manager warning, the grant items (only while missing), the strip toggle's title
    /// and Mark a Problem (while the log is verbose).
    pub fn update(&self, state: &MenuState) {
        self.state_line.setTitle(&NSString::from_str(&state.permissions.summary()));
        match config_line(state.config_error.as_deref()) {
            Some(text) => {
                self.config_line.setTitle(&NSString::from_str(&text));
                self.config_line.setHidden(false);
            }
            None => self.config_line.setHidden(true),
        }
        self.warning_line.setHidden(!state.apple_stage_manager_on);
        self.grant_accessibility.setHidden(state.permissions.accessibility);
        self.grant_screen_recording.setHidden(state.permissions.screen_recording && !state.capture_refused);
        let strip = if state.strip_visible { "Hide Strip" } else { "Show Strip" };
        self.toggle_strip.setTitle(&NSString::from_str(strip));
        self.mark.setHidden(!state.mark_shown);
        self.mark.setTitle(&NSString::from_str(&mark_title(state.last_mark.as_deref())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_line_shows_only_errors_and_stays_short() {
        assert_eq!(config_line(None), None);
        assert_eq!(config_line(Some("bad")).as_deref(), Some("Config error: bad"));
        let long = "x".repeat(300);
        let line = config_line(Some(&long)).unwrap_or_default();
        assert_eq!(line.chars().count(), 90);
        assert!(line.ends_with('…'));
    }

    #[test]
    fn the_mark_item_says_when_the_last_mark_went_in() {
        assert_eq!(mark_title(None), "Mark a Problem");
        assert_eq!(mark_title(Some("15:04:05")), "Mark a Problem (last at 15:04:05)");
    }
}
