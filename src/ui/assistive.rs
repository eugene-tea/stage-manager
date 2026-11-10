//! Whether an assistive app that finds things by the pointer is on: VoiceOver or Switch
//! Control (`NSWorkspace`'s `voiceOverEnabled`, `switchControlEnabled`), read at launch and
//! followed through key-value observing.
//!
//! While one is on, the strip's cards can be hit tested where they rest whatever the pointer
//! does ([`crate::ui::strip_reach`]), and a card's front window retitled is presented again so
//! its label follows. The value is kept in an atomic; the change callback runs on the main
//! thread.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, define_class, msg_send};
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSKeyValueObservingOptions, NSObject, NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSString, ns_string};

/// VoiceOver or Switch Control, as last read.
static ON: AtomicBool = AtomicBool::new(false);

/// The observer (kept for the life of the app) and what runs after a change.
type Watch = (Retained<Observer>, Box<dyn Fn()>);

thread_local! {
    /// The watch, on the main thread.
    static WATCH: RefCell<Option<Watch>> = const { RefCell::new(None) };
}

/// True while VoiceOver or Switch Control is on.
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; Observer does not implement Drop.
    #[unsafe(super = NSObject)]
    #[name = "SMAssistiveObserver"]
    struct Observer;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for Observer {}

    impl Observer {
        // SAFETY: the signature matches NSKeyValueObserving's; nothing passed in is kept.
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(&self, _key: Option<&NSString>, _object: Option<&AnyObject>, _change: Option<&AnyObject>, _context: *mut c_void) {
            // KVO may call on any thread: read and act on the main one.
            DispatchQueue::main().exec_async(|| {
                if read() {
                    WATCH.with(|w| {
                        if let Some((_, changed)) = w.borrow().as_ref() {
                            changed();
                        }
                    });
                }
            });
        }
    }
);

impl Observer {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: NSObject's designated initializer on a fresh instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// Reads the settings now and follows their changes; `changed` runs on the main thread after
/// each change. Call once, on the main thread.
pub fn install(changed: impl Fn() + 'static) {
    read();
    let observer = Observer::new();
    let workspace = NSWorkspace::sharedWorkspace();
    for key in [ns_string!("voiceOverEnabled"), ns_string!("switchControlEnabled")] {
        // SAFETY: the observer lives for the app's life (kept below) and is never removed;
        // NSWorkspace documents both properties as key-value observable; no context.
        unsafe { workspace.addObserver_forKeyPath_options_context(&observer, key, NSKeyValueObservingOptions::empty(), std::ptr::null_mut()) };
    }
    WATCH.with(|w| *w.borrow_mut() = Some((observer, Box::new(changed))));
}

/// Reads macOS's settings into the atomic; true when it changed (logged).
fn read() -> bool {
    let workspace = NSWorkspace::sharedWorkspace();
    let (voice_over, switch_control) = (workspace.isVoiceOverEnabled(), workspace.isSwitchControlEnabled());
    let now = voice_over || switch_control;
    let changed = ON.swap(now, Ordering::Relaxed) != now;
    if changed {
        tracing::info!(voice_over, switch_control, "assistive apps: the strip's cards hit-testable where they rest while on");
    }
    changed
}
