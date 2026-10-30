//! Apple-style alerts that never block: a standard `NSAlert`, laid out and shown as an
//! ordinary window, its buttons sending their index to a Rust closure.
//!
//! `runModal` would stop the main run loop's default mode (the 1 s poll, the strip's timers)
//! for as long as an alert waits, and an automatic update alert may wait for hours. So the
//! alert's own window is shown instead: the first button is the default (Return), a button
//! titled like "Later" or "Not Now" answers Escape.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2_app_kit::{NSAlert, NSAlertStyle, NSWindow};
use objc2_foundation::{MainThreadMarker, NSString};

use super::form::{self, ActionTarget};

/// One shown alert.
struct Shown {
    id: u64,
    alert: Retained<NSAlert>,
    _target: Retained<ActionTarget>,
    key: &'static str,
}

thread_local! {
    static SHOWN: RefCell<Vec<Shown>> = const { RefCell::new(Vec::new()) };
    static NEXT: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    /// Alerts closed, kept until the main queue's next turn: one closed from its own button
    /// (an alert that shows the next under the same key) is still running that button's
    /// action, whose target must outlive it (the review's U11).
    static CLOSED: RefCell<Vec<Shown>> = const { RefCell::new(Vec::new()) };
}

/// What an alert says and offers.
pub struct AlertSpec {
    /// Alerts with the same key replace each other (one update alert at a time).
    pub key: &'static str,
    pub title: String,
    pub text: String,
    /// Button titles, the default first.
    pub buttons: Vec<String>,
    /// The button Escape answers, if any.
    pub cancel: Option<usize>,
    /// A warning (caution icon) instead of an informational alert.
    pub warning: bool,
    /// Activate the app (the user asked); false shows it above other windows without taking
    /// the keyboard focus (an automatic notice).
    pub activate: bool,
}

/// Builds the alert and its laid-out window, without showing it.
pub fn build(mtm: MainThreadMarker, spec: AlertSpec, on_button: impl Fn(usize) + 'static) -> Retained<NSWindow> {
    close(spec.key);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&spec.title));
    alert.setInformativeText(&NSString::from_str(&spec.text));
    alert.setAlertStyle(if spec.warning { NSAlertStyle::Warning } else { NSAlertStyle::Informational });
    let key = spec.key;
    let id = NEXT.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    let target = ActionTarget::new(mtm, move |tag| {
        // The window goes now; the alert and this target after the click is handled.
        if let Some(w) = SHOWN.with(|s| s.borrow().iter().find(|a| a.id == id).map(|a| a.alert.window())) {
            w.orderOut(None);
        }
        dispatch2::DispatchQueue::main().exec_async(move || drop_where(|a| a.id == id));
        on_button(tag as usize);
    });
    for (i, title) in spec.buttons.iter().enumerate() {
        let b = alert.addButtonWithTitle(&NSString::from_str(title));
        form::wire(&b, &target, i as isize);
        if spec.cancel == Some(i) {
            b.setKeyEquivalent(&NSString::from_str("\u{1b}"));
        }
    }
    alert.layout();
    let window = alert.window();
    SHOWN.with(|s| s.borrow_mut().push(Shown { id, alert, _target: target, key }));
    window
}

/// Shows an alert in front of every app; `on_button` gets the index of the button clicked.
pub fn show(mtm: MainThreadMarker, spec: AlertSpec, on_button: impl Fn(usize) + 'static) {
    let activate = spec.activate;
    let window = build(mtm, spec, on_button);
    window.center();
    if activate {
        form::bring_to_front(mtm, &window);
    } else {
        window.orderFrontRegardless();
    }
}

/// Closes the alert shown under `key`, if any.
pub fn close(key: &str) {
    drop_where(|a| a.key == key);
}

fn drop_where(pred: impl Fn(&Shown) -> bool) {
    let gone: Vec<Shown> = SHOWN.with(|s| {
        let mut list = s.borrow_mut();
        let (gone, kept): (Vec<Shown>, Vec<Shown>) = list.drain(..).partition(|a| pred(a));
        *list = kept;
        gone
    });
    if gone.is_empty() {
        return;
    }
    for a in &gone {
        a.alert.window().orderOut(None);
    }
    // Released on the next turn, never inside a button's own action.
    CLOSED.with(|c| c.borrow_mut().extend(gone));
    dispatch2::DispatchQueue::main().exec_async(|| {
        let _released = CLOSED.with(|c| std::mem::take(&mut *c.borrow_mut()));
    });
}
