//! The app delegate and the (hidden) main menu.
//!
//! - **Reopen:** opening Stage Manager again while it runs (Finder, Spotlight, Launchpad,
//!   `open`) reaches `applicationShouldHandleReopen:hasVisibleWindows:`: the app shows its
//!   Settings window, the way back when the menu bar icon is hidden.
//! - **Terminate:** a quit from elsewhere (logout, `osascript … quit`) goes through the app's
//!   own Quit, which brings every parked window back first.
//! - **Main menu:** an accessory app shows no menu bar, but its main menu's key equivalents
//!   still work while one of its windows is key: ⌘, (Settings), ⌘Q (Quit, the safe one),
//!   ⌘W and ⌘M, and the edit keys for text.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationDelegate, NSApplicationTerminateReply, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};

use super::status_menu::{self, MenuAction};

/// What the system asks of the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppEvent {
    /// The app was opened again while running.
    Reopen,
}

type Handler = Box<dyn Fn(AppEvent)>;

thread_local! {
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
    static KEEP: RefCell<Option<Retained<AppDelegate>>> = const { RefCell::new(None) };
}

fn send(event: AppEvent) {
    let h = HANDLER.with(|h| h.borrow_mut().take());
    if let Some(h) = h {
        h(event);
        HANDLER.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some(h);
            }
        });
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; AppDelegate does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "SMAppDelegate"]
    pub struct AppDelegate;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for AppDelegate {}

    // SAFETY: the methods below match NSApplicationDelegate's signatures.
    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            tracing::info!("opened again while running: showing Settings");
            send(AppEvent::Reopen);
            false
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            tracing::info!("asked to quit by the system");
            // The app's Quit unparks every window and exits; if it returns, nothing quits.
            status_menu::dispatch(MenuAction::Quit);
            NSApplicationTerminateReply::TerminateCancel
        }
    }

    impl AppDelegate {
        // SAFETY (all methods): action signature `- (void)name:(id)sender`.
        #[unsafe(method(about:))]
        fn about(&self, _sender: Option<&AnyObject>) {
            status_menu::dispatch(MenuAction::About);
        }

        #[unsafe(method(settings:))]
        fn settings(&self, _sender: Option<&AnyObject>) {
            status_menu::dispatch(MenuAction::Settings);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            status_menu::dispatch(MenuAction::Quit);
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

fn item(mtm: MainThreadMarker, title: &str, action: Sel, key: &str, target: Option<&AnyObject>) -> Retained<NSMenuItem> {
    // SAFETY: valid title and key; the selector is the target's (or a responder's).
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(action), &NSString::from_str(key))
    };
    // SAFETY: the delegate lives for the whole app (KEEP); `None` is the responder chain.
    unsafe { item.setTarget(target) };
    item
}

fn submenu(mtm: MainThreadMarker, title: &str, items: Vec<Retained<NSMenuItem>>) -> Retained<NSMenuItem> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    for i in &items {
        menu.addItem(i);
    }
    let holder = NSMenuItem::new(mtm);
    holder.setSubmenu(Some(&menu));
    holder
}

/// Installs the delegate and the main menu; `on_event` gets reopens.
pub fn install(mtm: MainThreadMarker, on_event: impl Fn(AppEvent) + 'static) {
    HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(on_event)));
    let app = NSApplication::sharedApplication(mtm);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let t: Option<&AnyObject> = Some(&delegate);
    let main = NSMenu::new(mtm);
    main.addItem(&submenu(
        mtm,
        "Stage Manager",
        vec![
            item(mtm, "About Stage Manager", sel!(about:), "", t),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Settings…", sel!(settings:), ",", t),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Quit Stage Manager", sel!(quit:), "q", t),
        ],
    ));
    let redo = item(mtm, "Redo", sel!(redo:), "z", None);
    redo.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
    main.addItem(&submenu(
        mtm,
        "Edit",
        vec![
            item(mtm, "Undo", sel!(undo:), "z", None),
            redo,
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Cut", sel!(cut:), "x", None),
            item(mtm, "Copy", sel!(copy:), "c", None),
            item(mtm, "Paste", sel!(paste:), "v", None),
            item(mtm, "Select All", sel!(selectAll:), "a", None),
        ],
    ));
    main.addItem(&submenu(
        mtm,
        "Window",
        vec![item(mtm, "Minimize", sel!(performMiniaturize:), "m", None), item(mtm, "Close", sel!(performClose:), "w", None)],
    ));
    app.setMainMenu(Some(&main));
    KEEP.with(|k| *k.borrow_mut() = Some(delegate));
}
