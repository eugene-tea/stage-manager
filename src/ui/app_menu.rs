//! The app delegate and the (hidden) main menu.
//!
//! - **Launched:** `applicationDidFinishLaunching:` reads AppKit's open event (whether it
//!   marks a login item's launch) for the launch's line (round reopen-list).
//! - **Reopen:** opening Stage Manager again while it runs (Finder, Spotlight, Launchpad,
//!   `open`) reaches `applicationShouldHandleReopen:hasVisibleWindows:`: the app shows its
//!   Settings window, the way back when the menu bar icon is hidden.
//! - **Terminate:** a quit from elsewhere (logout, `osascript … quit`) goes through the app's
//!   own Quit, which brings every parked window back first and exits (after a logout's head
//!   start, `willPowerOffNotification`, the windows are back already: it only exits). A
//!   logout is never cancelled: when the Quit cannot run at once (the app's state is busy, or
//!   a quit is already under way) the answer is "later", the Quit is tried again every
//!   200 ms, and after 5 s the registry unparks and macOS is told to go on. Its quit event's
//!   reason (`keyAEQuitReason`: logout, restart, shutdown) is logged and kept
//!   ([`system_quit_reason`]); the user's own Quit (the menus' item, ⌘Q) is kept apart
//!   ([`user_quit`]): a cancelled logout opens the app again after the one, never the other
//!   (round logout-cancel-relaunch).
//! - **Main menu:** an accessory app shows no menu bar, but its main menu's key equivalents
//!   still work while one of its windows is key: ⌘, (Settings), ⌘Q (Quit, the safe one),
//!   ⌘W and ⌘M, and the edit keys for text.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use dispatch2::{DispatchQueue, DispatchTime};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationDelegate, NSApplicationTerminateReply, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSString};

use super::status_menu::{self, MenuAction};

/// What the system asks of the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppEvent {
    /// AppKit finished launching; whether its open event marked a login item's launch
    /// (`None`: no open event).
    Launched(Option<bool>),
    /// The app was opened again while running.
    Reopen,
}

type Handler = Box<dyn Fn(AppEvent)>;

thread_local! {
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
    static KEEP: RefCell<Option<Retained<AppDelegate>>> = const { RefCell::new(None) };
}

/// The system quit event's `keyAEQuitReason` (a FourCharCode; 0: none, or no system quit).
static QUIT_REASON: AtomicU32 = AtomicU32::new(0);

/// Set when the user chose Quit (a menu's item, ⌘Q).
static USER_QUIT: AtomicBool = AtomicBool::new(false);

/// The system quit event's reason (`keyAEQuitReason`), when one came with a reason.
pub fn system_quit_reason() -> Option<u32> {
    Some(QUIT_REASON.load(Ordering::SeqCst)).filter(|r| *r != 0)
}

/// Whether the user chose Quit (a menu's item, ⌘Q), not the system.
pub fn user_quit() -> bool {
    USER_QUIT.load(Ordering::SeqCst)
}

/// The user chose Quit (a menu's item, ⌘Q).
pub fn note_user_quit() {
    USER_QUIT.store(true, Ordering::SeqCst);
}

/// The current Apple event's `keyAEQuitReason` attribute (AERegistry.h, `why?`), 0 when there
/// is none. Call inside `applicationShouldTerminate:`, while the quit event is current.
fn quit_reason() -> u32 {
    use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager};
    /// `keyAEQuitReason`, AERegistry.h.
    const QUIT_REASON_KEY: u32 = u32::from_be_bytes(*b"why?");
    let Some(event) = NSAppleEventManager::sharedAppleEventManager().currentAppleEvent() else { return 0 };
    // SAFETY: `-[NSAppleEventDescriptor attributeDescriptorForKeyword:]` takes an AEKeyword (a
    // FourCharCode, u32) and returns a descriptor or nil (objc2 binds it only with
    // CoreServices' types).
    let why: Option<Retained<NSAppleEventDescriptor>> = unsafe { msg_send![&*event, attributeDescriptorForKeyword: QUIT_REASON_KEY] };
    why.map_or(0, |w| w.typeCodeValue())
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
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _note: &NSNotification) {
            // The open event is AppKit's current Apple event only now.
            send(AppEvent::Launched(crate::platform::login_item::open_event_login_item()));
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            // The shell says whether Settings shows (not moments after the login).
            send(AppEvent::Reopen);
            false
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            let reason = quit_reason();
            QUIT_REASON.store(reason, Ordering::SeqCst);
            tracing::info!(reason = %crate::engine::logout_watch::four_cc(reason), "asked to quit by the system");
            // The app's Quit unparks every window and exits; it returns only when it could
            // not run now (or one is under way): answer later and keep trying.
            status_menu::dispatch(MenuAction::Quit);
            tracing::warn!("the quit could not run at once; trying again (the system waits)");
            retry_quit(1);
            NSApplicationTerminateReply::TerminateLater
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
            note_user_quit();
            status_menu::dispatch(MenuAction::Quit);
        }
    }
);

/// How often and how long a deferred system quit is tried.
const QUIT_RETRY: Duration = Duration::from_millis(200);
const QUIT_ATTEMPTS: u32 = 25;

/// Tries the app's Quit again after [`QUIT_RETRY`] (on the main queue, which runs while
/// AppKit waits for the answer); after [`QUIT_ATTEMPTS`] the registry unparks and macOS goes
/// on with the logout (the guardian scans after the exit).
fn retry_quit(attempt: u32) {
    let when = DispatchTime::try_from(QUIT_RETRY).unwrap_or(DispatchTime::NOW);
    let _ = DispatchQueue::main().after(when, move || {
        status_menu::dispatch(MenuAction::Quit);
        if attempt < QUIT_ATTEMPTS {
            retry_quit(attempt + 1);
            return;
        }
        tracing::error!("the quit never ran; unparking from the registry and letting the system quit");
        crate::platform::safety::emergency_unpark("terminate");
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).replyToApplicationShouldTerminate(true);
        }
    });
}

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
