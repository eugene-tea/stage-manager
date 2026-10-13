//! The welcome window: shown at the first launch, and at any launch while a required
//! approval is missing (or Apple's Stage Manager is on). Small and Apple-like: the app's
//! icon, a title, one row per approval (what it is for; a check mark once allowed, else an
//! Allow… button that opens the right System Settings pane), a row to turn Apple's Stage
//! Manager off while it is on, and Done. Rows update live as approvals land; once
//! everything is in place it says "All set".

use std::cell::RefCell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::{MainThreadOnly, Message};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSColor, NSImageView, NSView, NSWindow, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

use super::form::{self, ActionTarget, Form, Row};
use crate::platform::permissions::{Approval, PermissionState};

/// One row of the welcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WelcomeRow {
    /// An approval and whether it is granted.
    Approval(Approval, bool),
    /// Apple's Stage Manager, and whether it is off now.
    AppleOff(bool),
}

impl WelcomeRow {
    /// Whether the row is done.
    pub fn done(self) -> bool {
        match self {
            Self::Approval(_, granted) => granted,
            Self::AppleOff(off) => off,
        }
    }
}

/// The rows: Accessibility; Screen Recording while window pictures are on; Apple's Stage
/// Manager when it was on as the window opened (it stays, checked, once turned off).
pub fn rows(state: &PermissionState, capture_on: bool, apple_row: bool, apple_on: bool) -> Vec<WelcomeRow> {
    let mut out: Vec<WelcomeRow> = Approval::ALL
        .into_iter()
        .filter(|a| *a == Approval::Accessibility || capture_on)
        .map(|a| WelcomeRow::Approval(a, a.granted(state)))
        .collect();
    if apple_row {
        out.push(WelcomeRow::AppleOff(!apple_on));
    }
    out
}

/// Everything in place.
pub fn all_set(rows: &[WelcomeRow]) -> bool {
    rows.iter().all(|r| r.done())
}

/// Whether the welcome shows at launch: the first launch, a missing required approval, or
/// Apple's Stage Manager on (never under `STAGE_MANAGER_NO_PROMPT`).
pub fn due(first_launch: bool, state: &PermissionState, apple_on: bool, prompts_allowed: bool) -> bool {
    prompts_allowed && (first_launch || !state.accessibility || apple_on)
}

/// A click in the welcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WelcomeAction {
    Allow(Approval),
    TurnOffApple,
    OpenSettings,
    Done,
}

const WIDTH: f64 = 500.0;

mod tag {
    pub const ACCESSIBILITY: isize = 1;
    pub const SCREEN: isize = 2;
    pub const APPLE: isize = 3;
    pub const SETTINGS: isize = 4;
    pub const DONE: isize = 5;
}

struct Open {
    window: Retained<NSWindow>,
    _target: Retained<ActionTarget>,
    rows: Vec<WelcomeRow>,
}

/// Receives the welcome's clicks.
type ActionFn = Rc<dyn Fn(WelcomeAction)>;

thread_local! {
    static OPEN: RefCell<Option<Open>> = const { RefCell::new(None) };
    static HANDLER: RefCell<Option<ActionFn>> = const { RefCell::new(None) };
}

fn clicked(t: isize) {
    let action = match t {
        tag::ACCESSIBILITY => WelcomeAction::Allow(Approval::Accessibility),
        tag::SCREEN => WelcomeAction::Allow(Approval::ScreenRecording),
        tag::APPLE => WelcomeAction::TurnOffApple,
        tag::SETTINGS => WelcomeAction::OpenSettings,
        _ => WelcomeAction::Done,
    };
    if action == WelcomeAction::Done || action == WelcomeAction::OpenSettings {
        close();
    }
    if let Some(h) = HANDLER.with(|h| h.borrow().clone()) {
        h(action);
    }
}

fn view<T: AsRef<NSView>>(v: &T) -> Retained<NSView> {
    v.as_ref().retain()
}

/// A green check mark with "Allowed" (or another word).
fn done_mark(mtm: MainThreadMarker, word: &str) -> Retained<NSView> {
    let holder = form::FlippedView::new(mtm, NSSize::new(84.0, 22.0));
    let check = form::symbol(mtm, "checkmark.circle.fill", 15.0, Some(&NSColor::systemGreenColor()));
    check.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(22.0, 22.0)));
    holder.addSubview(&check);
    let l = form::label(mtm, word, 13.0, false);
    l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    l.setFrame(NSRect::new(NSPoint::new(24.0, 3.0), NSSize::new(60.0, 17.0)));
    holder.addSubview(&l);
    holder.into_super()
}

fn build(mtm: MainThreadMarker, rows: &[WelcomeRow], target: &ActionTarget) -> (Retained<form::FlippedView>, f64) {
    let mut f = Form::new(mtm, WIDTH);
    f.margin = 28.0;
    f.space(18.0);
    let app = NSApplication::sharedApplication(mtm);
    if let Some(icon) = app.applicationIconImage() {
        let iv = NSImageView::imageViewWithImage(&icon, mtm);
        iv.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(72.0, 72.0)));
        f.place(&iv, (WIDTH - 72.0) / 2.0, 72.0);
        f.space(10.0);
    }
    let set = all_set(rows);
    let title = form::label(mtm, if set { "Stage Manager is all set" } else { "Welcome to Stage Manager" }, 20.0, true);
    form::centered(&title);
    title.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(f.inner(), 26.0)));
    f.place(&title, f.margin, 26.0);
    f.space(6.0);
    let subtitle = if set {
        "Everything it needs is in place. Your windows now arrange into stages, as with Apple's Stage Manager."
    } else {
        "Stage Manager needs a few things from you before it can arrange your windows. Each can be changed later in System Settings."
    };
    let (sub, h) = form::wrapping(mtm, subtitle, f.inner(), 13.0, &NSColor::secondaryLabelColor());
    form::centered(&sub);
    f.place(&sub, f.margin, h);
    f.space(16.0);
    let mut group = Vec::new();
    for r in rows {
        let (title, detail, symbol, button, t) = match *r {
            WelcomeRow::Approval(a, _) => {
                let t = if a == Approval::Accessibility { tag::ACCESSIBILITY } else { tag::SCREEN };
                let detail = if a.required() { a.why().to_string() } else { format!("{} Optional: {}", a.why(), a.without().to_lowercase()) };
                (a.name().to_string(), detail, a.symbol(), "Allow…", t)
            }
            WelcomeRow::AppleOff(_) => (
                "Apple's Stage Manager".to_string(),
                "Stage Manager takes over from it, so Apple's must be off.".to_string(),
                "macwindow.on.rectangle",
                "Turn Off",
                tag::APPLE,
            ),
        };
        let mut row = Row::new(&title, None).detail(detail);
        row.icon = Some(view(&form::symbol(mtm, symbol, 20.0, Some(&NSColor::controlAccentColor()))));
        let trailing = if r.done() {
            done_mark(mtm, if matches!(r, WelcomeRow::AppleOff(_)) { "Off" } else { "Allowed" })
        } else {
            view(&form::button(mtm, button, target, t))
        };
        row.trailing.push(trailing);
        group.push(row);
    }
    f.group(group);
    f.space(4.0);
    f.footnote("Stage Manager lives in the menu bar: click its icon for Settings. Without the icon, open Stage Manager again from Applications.");
    f.space(14.0);
    // Buttons: Open Settings… on the left, Done (or Later) on the right.
    let settings = form::button(mtm, "Open Settings…", target, tag::SETTINGS);
    let done = form::button(mtm, if set { "Done" } else { "Later" }, target, tag::DONE);
    if set {
        done.setKeyEquivalent(&NSString::from_str("\r"));
    } else {
        done.setKeyEquivalent(&NSString::from_str("\u{1b}"));
    }
    let size = done.frame().size;
    let bar = form::FlippedView::new(mtm, NSSize::new(f.inner(), size.height));
    settings.setFrameOrigin(NSPoint::new(0.0, 0.0));
    done.setFrameOrigin(NSPoint::new(f.inner() - size.width, 0.0));
    bar.addSubview(&settings);
    bar.addSubview(&done);
    f.place(&bar, f.margin, size.height);
    f.space(8.0);
    f.finish()
}

fn make_window(mtm: MainThreadMarker, size: NSSize) -> Retained<NSWindow> {
    // SAFETY: a fresh allocation, a valid frame and style; released only by us.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), size),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::FullSizeContentView,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is owned by OPEN until closed.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("Welcome to Stage Manager"));
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setTitlebarAppearsTransparent(true);
    window.setMovableByWindowBackground(true);
    window
}

/// Creates the window for `rows` (not shown); `on_action` gets the clicks.
pub fn create(mtm: MainThreadMarker, rows: Vec<WelcomeRow>, on_action: impl Fn(WelcomeAction) + 'static) -> Retained<NSWindow> {
    HANDLER.with(|h| *h.borrow_mut() = Some(Rc::new(on_action)));
    close();
    let target = ActionTarget::new(mtm, clicked);
    let (view, height) = build(mtm, &rows, &target);
    let window = make_window(mtm, NSSize::new(WIDTH, height));
    window.setContentView(Some(&view));
    OPEN.with(|o| *o.borrow_mut() = Some(Open { window: window.clone(), _target: target, rows }));
    window
}

/// Shows the welcome in front.
pub fn show(mtm: MainThreadMarker, rows: Vec<WelcomeRow>, on_action: impl Fn(WelcomeAction) + 'static) {
    let window = create(mtm, rows, on_action);
    window.center();
    form::bring_to_front(mtm, &window);
}

/// Fresh rows (an approval landed, Apple's turned off): redrawn when they changed.
pub fn update(rows: Vec<WelcomeRow>) {
    let changed = OPEN.with(|o| o.borrow().as_ref().is_some_and(|o| o.rows != rows));
    if !changed {
        return;
    }
    // After the current event: a button that sent it may be on the stack.
    dispatch2::DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else { return };
        OPEN.with(|o| {
            let mut o = o.borrow_mut();
            let Some(open) = o.as_mut() else { return };
            let (view, height) = build(mtm, &rows, &open._target);
            let mut frame = open.window.frame();
            let content = open.window.contentRectForFrameRect(frame);
            let dh = height - content.size.height;
            frame.origin.y -= dh;
            frame.size.height += dh;
            open.window.setContentView(Some(&view));
            open.window.setFrame_display(frame, true);
            if all_set(&rows) {
                tracing::info!("welcome: all set");
            }
            open.rows = rows;
        });
    });
}

/// Whether the welcome is showing.
pub fn is_open() -> bool {
    OPEN.with(|o| o.borrow().as_ref().is_some_and(|o| o.window.isVisible()))
}

/// Whether the open welcome has the Apple's Stage Manager row.
pub fn has_apple_row() -> bool {
    OPEN.with(|o| o.borrow().as_ref().is_some_and(|o| o.rows.iter().any(|r| matches!(r, WelcomeRow::AppleOff(_)))))
}

/// Closes the welcome.
pub fn close() {
    let open = OPEN.with(|o| o.borrow_mut().take());
    if let Some(open) = open {
        open.window.orderOut(None);
        // The target may be sending this click: let it go after the event.
        form::release_later(open);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: bool, r: bool) -> PermissionState {
        PermissionState { accessibility: a, screen_recording: r }
    }

    #[test]
    fn rows_follow_the_approvals_and_apples_stage_manager() {
        use WelcomeRow::*;
        assert_eq!(rows(&p(false, false), true, false, false), vec![Approval(super::Approval::Accessibility, false), Approval(super::Approval::ScreenRecording, false)]);
        assert_eq!(rows(&p(true, false), false, false, false), vec![Approval(super::Approval::Accessibility, true)], "no pictures, no Screen Recording row");
        let with_apple = rows(&p(true, true), true, true, true);
        assert_eq!(with_apple.last(), Some(&AppleOff(false)));
        assert!(!all_set(&with_apple), "Apple's still on");
        let turned_off = rows(&p(true, true), true, true, false);
        assert_eq!(turned_off.last(), Some(&AppleOff(true)), "the row stays, checked");
        assert!(all_set(&turned_off));
        assert!(!all_set(&rows(&p(true, false), true, false, false)));
    }

    #[test]
    fn the_welcome_shows_at_first_launch_and_while_something_required_is_missing() {
        assert!(due(true, &p(true, true), false, true), "first launch, even with everything granted");
        assert!(due(false, &p(false, true), false, true), "Accessibility missing");
        assert!(!due(false, &p(true, false), false, true), "Screen Recording is optional: the menu asks");
        assert!(due(false, &p(true, true), true, true), "Apple's Stage Manager on");
        assert!(!due(false, &p(true, true), false, true));
        assert!(!due(true, &p(false, false), true, false), "never in test runs (STAGE_MANAGER_NO_PROMPT)");
    }
}
