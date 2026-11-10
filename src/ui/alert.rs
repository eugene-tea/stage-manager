//! Apple-style alerts that never block: a standard `NSAlert`, shown either as an ordinary
//! window or as a sheet on the window whose action raised it, its answer going to a Rust
//! closure as the index of the button clicked.
//!
//! `runModal` would stop the main run loop's default mode (the 1 s poll, the strip's timers)
//! for as long as an alert waits, and an automatic update alert may wait for hours. So a
//! free-standing alert's own window is shown instead: the first button is the default
//! (Return), a button titled like "Later" or "Not Now" answers Escape.
//!
//! An alert about something started in a window (the Settings window's Check for Updates…, a
//! setting that can't be saved) is a sheet on that window, as Apple's HIG asks (round
//! settings-sheet): `beginSheetModalForWindow:completionHandler:`, which runs no modal loop
//! either; the completion handler carries the answer. [`place`] chooses: a sheet only while
//! that window is on screen and carries no other sheet of ours (two never stack: the second
//! shows free-standing). A sheet whose window closes ends with its cancel answer.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_app_kit::{NSAlert, NSAlertStyle, NSModalResponse, NSWindow, NSWindowWillCloseNotification};
use objc2_foundation::{MainThreadMarker, NSNotification, NSNotificationCenter, NSOperationQueue, NSString};

use super::form::{self, ActionTarget};

/// The code a sheet ends with when an alert under the same key replaces it (no answer).
const ENDED_REPLACED: NSModalResponse = -1000; // NSModalResponseStop
/// The code a sheet ends with when its window closes (the cancel answer).
const ENDED_WINDOW_CLOSED: NSModalResponse = -1001; // NSModalResponseAbort
/// `NSAlertFirstButtonReturn`: button `i` ends a sheet with this plus `i`.
const FIRST_BUTTON: NSModalResponse = 1000;

/// Where an alert shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A sheet on the window the action came from.
    Sheet,
    /// An ordinary alert window, centred on the screen.
    Free,
}

/// Where an alert shows: a sheet when the action came from a window (`from_window`), that
/// window is on screen (`window_up`), and it carries no other sheet of ours (`sheet_up`);
/// free-standing otherwise (an automatic notice, the menu bar, a window closed meanwhile, a
/// second alert while a sheet waits).
pub fn place(from_window: bool, window_up: bool, sheet_up: bool) -> Place {
    if from_window && window_up && !sheet_up { Place::Sheet } else { Place::Free }
}

/// The answer a sheet's end code means: the button clicked (Return and Escape click theirs),
/// the cancel button when the window closed under it, nothing when it was replaced.
fn answer(code: NSModalResponse, cancel: Option<usize>) -> Option<usize> {
    match code {
        ENDED_WINDOW_CLOSED => cancel,
        c if c >= FIRST_BUTTON => usize::try_from(c - FIRST_BUTTON).ok(),
        _ => None,
    }
}

/// A sheet's window and the observer of its closing.
struct Sheet {
    parent: Retained<NSWindow>,
    observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Drop for Sheet {
    fn drop(&mut self) {
        // SAFETY: the token came from the default center's addObserverForName.
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(self.observer.as_ref()) };
    }
}

/// One shown alert.
struct Shown {
    id: u64,
    alert: Retained<NSAlert>,
    /// The free-standing alert's button target (a sheet's buttons keep the alert's own).
    _target: Option<Retained<ActionTarget>>,
    key: &'static str,
    sheet: Option<Sheet>,
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

/// The alert with its texts and buttons (the one under the same key closed), and its number.
fn make(mtm: MainThreadMarker, spec: &AlertSpec) -> (Retained<NSAlert>, u64) {
    close(spec.key);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&spec.title));
    alert.setInformativeText(&NSString::from_str(&spec.text));
    alert.setAlertStyle(if spec.warning { NSAlertStyle::Warning } else { NSAlertStyle::Informational });
    for (i, title) in spec.buttons.iter().enumerate() {
        let b = alert.addButtonWithTitle(&NSString::from_str(title));
        if spec.cancel == Some(i) {
            b.setKeyEquivalent(&NSString::from_str("\u{1b}"));
        }
    }
    // Never brought back by macOS's reopen at login (round reopen-list).
    alert.window().setRestorable(false);
    let id = NEXT.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    (alert, id)
}

/// Builds the free-standing alert and its laid-out window, without showing it.
pub fn build(mtm: MainThreadMarker, spec: AlertSpec, on_button: impl Fn(usize) + 'static) -> Retained<NSWindow> {
    let (alert, id) = make(mtm, &spec);
    let target = ActionTarget::new(mtm, move |tag| {
        // The window goes now; the alert and this target after the click is handled.
        if let Some(w) = SHOWN.with(|s| s.borrow().iter().find(|a| a.id == id).map(|a| a.alert.window())) {
            w.orderOut(None);
        }
        dispatch2::DispatchQueue::main().exec_async(move || drop_where(|a| a.id == id));
        on_button(tag as usize);
    });
    for (i, b) in alert.buttons().iter().enumerate() {
        form::wire(&b, &target, i as isize);
    }
    alert.layout();
    let window = alert.window();
    SHOWN.with(|s| s.borrow_mut().push(Shown { id, alert, _target: Some(target), key: spec.key, sheet: None }));
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

/// Whether `window` carries a sheet of ours.
pub fn has_sheet(window: &NSWindow) -> bool {
    SHOWN.with(|s| s.borrow().iter().any(|a| a.sheet.as_ref().is_some_and(|sh| std::ptr::eq(&*sh.parent, window))))
}

/// Shows an alert as a sheet on `parent` (brought to the front first when the spec
/// activates); `on_button` gets the index of the button clicked, or the cancel button's
/// when `parent` closes under it. Callers choose with [`place`]: never on a window that
/// carries another sheet of ours.
pub fn show_sheet(mtm: MainThreadMarker, spec: AlertSpec, parent: &NSWindow, on_button: impl Fn(usize) + 'static) {
    let (alert, id) = make(mtm, &spec);
    let cancel = spec.cancel;
    let on_button = Rc::new(on_button);
    // The alert stays alive as long as AppKit keeps the handler (until the sheet is gone).
    let kept = alert.clone();
    let handler = RcBlock::new(move |code: NSModalResponse| {
        let _alert = &kept;
        let answer = answer(code, cancel);
        tracing::debug!(id, code, ?answer, "alert: sheet ended");
        // Off the list (a replaced one is already); released on the next turn.
        let ended: Vec<Shown> = SHOWN.with(|s| {
            let mut list = s.borrow_mut();
            let (ended, kept): (Vec<Shown>, Vec<Shown>) = list.drain(..).partition(|a| a.id == id);
            *list = kept;
            ended
        });
        release_next_turn(ended);
        if let Some(i) = answer {
            on_button(i);
        }
    });
    let closing = RcBlock::new(move |_note: NonNull<NSNotification>| {
        let Some((parent, sheet)) = SHOWN.with(|s| {
            s.borrow().iter().find(|a| a.id == id).and_then(|a| Some((a.sheet.as_ref()?.parent.clone(), a.alert.window())))
        }) else {
            return;
        };
        tracing::info!(id, "alert: its window closed; the sheet ends with its cancel answer");
        parent.endSheet_returnCode(&sheet, ENDED_WINDOW_CLOSED);
    });
    // SAFETY: a framework name static; the block runs on the main queue only, where the
    // alert list lives; the object is the live parent window.
    let observer = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(NSWindowWillCloseNotification),
            Some(parent.as_ref()),
            Some(&NSOperationQueue::mainQueue()),
            &closing,
        )
    };
    SHOWN.with(|s| {
        s.borrow_mut().push(Shown { id, alert: alert.clone(), _target: None, key: spec.key, sheet: Some(Sheet { parent: parent.retain(), observer }) })
    });
    if spec.activate {
        form::bring_to_front(mtm, parent);
    }
    alert.beginSheetModalForWindow_completionHandler(parent, Some(&handler));
}

/// Closes the alert shown under `key`, if any (a sheet ends with no answer).
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
        match &a.sheet {
            // Outside the list's borrow: the completion handler may run inside the call.
            Some(sheet) => sheet.parent.endSheet_returnCode(&a.alert.window(), ENDED_REPLACED),
            None => a.alert.window().orderOut(None),
        }
    }
    release_next_turn(gone);
}

/// Releases `gone` on the main queue's next turn, never inside a button's own action.
fn release_next_turn(gone: Vec<Shown>) {
    if gone.is_empty() {
        return;
    }
    CLOSED.with(|c| c.borrow_mut().extend(gone));
    dispatch2::DispatchQueue::main().exec_async(|| {
        let _released = CLOSED.with(|c| std::mem::take(&mut *c.borrow_mut()));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alert_from_a_window_on_screen_is_a_sheet_on_it() {
        assert_eq!(place(true, true, false), Place::Sheet);
    }

    #[test]
    fn everything_else_stands_free() {
        assert_eq!(place(false, true, false), Place::Free, "an automatic notice or the menu bar, Settings open");
        assert_eq!(place(false, false, false), Place::Free);
        assert_eq!(place(true, false, false), Place::Free, "Settings closed before the answer came");
        assert_eq!(place(true, true, true), Place::Free, "a sheet of ours waits there: never two stacked");
        assert_eq!(place(false, true, true), Place::Free);
    }

    #[test]
    fn a_sheet_answers_with_its_button_or_its_cancel_when_the_window_closes() {
        assert_eq!(answer(1000, Some(1)), Some(0), "the first button (Return)");
        assert_eq!(answer(1002, Some(1)), Some(2), "the third");
        assert_eq!(answer(1003, None), Some(3), "a fourth counts on from the third");
        assert_eq!(answer(ENDED_WINDOW_CLOSED, Some(1)), Some(1), "Later, when Settings closes");
        assert_eq!(answer(ENDED_WINDOW_CLOSED, None), None, "no cancel button: no answer, never the default");
        assert_eq!(answer(ENDED_REPLACED, Some(0)), None, "replaced under its key: no answer");
        assert_eq!(answer(0, Some(0)), None);
    }
}
