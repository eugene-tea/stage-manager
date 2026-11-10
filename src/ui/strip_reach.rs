//! The strip's cards where an assistive app looks for them: a catching window under each card
//! while VoiceOver or Switch Control is on ([`crate::ui::assistive`]).
//!
//! **Why.** A system-wide Accessibility hit test (`AXUIElementCopyElementAtPosition`, how
//! VoiceOver finds what is under the pointer or a point it was given) asks the window server
//! for the window there, and the window server passes over a window that ignores the mouse,
//! as it does for a click. The strip's pane lets the mouse through everywhere but on a card,
//! and decides that by where the pointer is (`Strip::update_pass_through`: one flag per
//! window). Found live 11-09: with the pointer anywhere else (VoiceOver's own cursor never
//! moves it), the hit test at a card's middle answered Finder's desktop beneath.
//!
//! **How.** While an assistive app is on, each card the strip lists for VoiceOver
//! ([`crate::ui::strip_access`]) gets a borderless, transparent, non-activating panel of its
//! own, exactly where the card rests, as a child window just below the pane: it takes the
//! mouse whatever the pointer does, so the window server finds it there; its view answers the
//! hit test with the card's element (whose parent stays the strip's "Stages" group). Only the
//! cards' rests are covered: the empty column and the gaps between cards still let clicks
//! through to the desktop or the window below. A window over the strip still covers its cards
//! (the panels sit at the pane's level, below windows). The pointer's events on one go to the
//! strip as the pane's own would ([`Mouse`]), so hovering, clicking and dragging a card work
//! the same; once the pointer is on a card the pane above takes the mouse again.
//!
//! Main thread only, as the strip.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSEventModifierFlags, NSPanel, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindowOrderingMode,
    NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSSize};

use crate::engine::geometry::Rect;
use crate::ui::panels;
use crate::ui::strip_access::CardElement;

/// Where each card's catching window goes, Cocoa screen coordinates: `frames` (each card's
/// rest by stage, the strip view's top-left coordinates) in the pane's panel at `panel`
/// (Cocoa screen coordinates); none unless `on` (an assistive app on, the strip shown).
pub fn targets(on: bool, panel: Rect, frames: &[(u64, Rect)]) -> Vec<(u64, Rect)> {
    if !on {
        return Vec::new();
    }
    frames
        .iter()
        .filter(|(_, f)| f.w > 0.0 && f.h > 0.0)
        .map(|(stage, f)| (*stage, Rect::new(panel.x + f.x, panel.y + panel.h - (f.y + f.h), f.w, f.h)))
        .collect()
}

/// The pointer on a card's catching window (`NSPoint`s in Cocoa screen coordinates): what
/// the strip's own view would have heard.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mouse {
    /// It moved onto or over the card.
    Moved(NSPoint),
    /// The button went down there.
    Down(NSPoint),
    /// It is dragged with the button down.
    Dragged,
    /// The button came up there, with these modifiers held.
    Up(NSPoint, NSEventModifierFlags),
}

/// Where the catching windows' events go: (strip key, the event).
pub type MouseSink = fn(u32, Mouse);

/// A catching window's view: its strip, its card's element and where its events go.
pub struct ReachTarget {
    strip: Cell<u32>,
    element: RefCell<Option<Retained<CardElement>>>,
    sink: Cell<Option<MouseSink>>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements; ReachView does not implement Drop.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[name = "SMStripReachView"]
    #[ivars = ReachTarget]
    pub struct ReachView;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for ReachView {}

    impl ReachView {
        // SAFETY (all methods): signatures match NSView / NSResponder / NSAccessibility.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.send(Mouse::Down(self.screen_point(event)));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _event: &NSEvent) {
            self.send(Mouse::Dragged);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.send(Mouse::Up(self.screen_point(event), event.modifierFlags()));
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.send(Mouse::Moved(self.screen_point(event)));
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, event: &NSEvent) {
            self.send(Mouse::Moved(self.screen_point(event)));
        }

        // The pointer leaving is not sent: it leaves for the pane above (which took the mouse
        // on the card) or for what the global monitor hears.

        // The hit test anywhere on it: its card's element.
        #[unsafe(method(accessibilityHitTest:))]
        fn accessibility_hit_test(&self, point: NSPoint) -> *mut AnyObject {
            match self.ivars().element.borrow().clone() {
                Some(element) => Retained::autorelease_return(Retained::into_super(Retained::into_super(Retained::into_super(element)))),
                // SAFETY: NSView's own hit test, same signature; an autoreleased object or nil.
                None => unsafe { msg_send![super(self), accessibilityHitTest: point] },
            }
        }
    }
);

impl ReachView {
    fn new(mtm: MainThreadMarker, frame: NSRect, strip: u32, sink: MouseSink) -> Retained<Self> {
        let target = ReachTarget { strip: Cell::new(strip), element: RefCell::new(None), sink: Cell::new(Some(sink)) };
        let this = Self::alloc(mtm).set_ivars(target);
        // SAFETY: NSView's designated initializer on a fresh instance.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn send(&self, mouse: Mouse) {
        let t = self.ivars();
        if let Some(sink) = t.sink.get() {
            sink(t.strip.get(), mouse);
        }
    }

    /// The event's location in Cocoa screen coordinates.
    fn screen_point(&self, event: &NSEvent) -> NSPoint {
        self.window().map_or_else(NSEvent::mouseLocation, |w| w.convertPointToScreen(event.locationInWindow()))
    }
}

/// One card's catching window.
struct Target {
    panel: Retained<NSPanel>,
    view: Retained<ReachView>,
    frame: Rect,
}

impl Target {
    fn new(mtm: MainThreadMarker, parent: &NSPanel, strip: u32, frame: Rect, sink: MouseSink) -> Self {
        let rect = cocoa(frame);
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setLevel(parent.level());
        panel.setCollectionBehavior(panels::desktop_panel());
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setAcceptsMouseMovedEvents(true);
        // Said outright: a transparent window left to its default lets the mouse through where
        // nothing is drawn, and nothing is drawn here.
        panel.setIgnoresMouseEvents(false);
        // SAFETY: the panel is owned by its target and closed by it.
        unsafe { panel.setReleasedWhenClosed(false) };
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), rect.size);
        let view = ReachView::new(mtm, bounds, strip, sink);
        let options = NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the view owns the tracking area and outlives it; no user info.
        let area = unsafe { NSTrackingArea::initWithRect_options_owner_userInfo(NSTrackingArea::alloc(), bounds, options, Some(&view), None) };
        view.addTrackingArea(&area);
        panel.setContentView(Some(&view));
        // A child just below the pane: on its Desktop, ordered in and out with it.
        // SAFETY: both windows are live; the child is taken off before it closes.
        unsafe { parent.addChildWindow_ordered(&panel, NSWindowOrderingMode::Below) };
        Self { panel, view, frame }
    }

    fn close(&self, parent: &NSPanel) {
        parent.removeChildWindow(&self.panel);
        self.panel.orderOut(None);
        self.panel.close();
    }
}

/// A Cocoa rect from `r` (already in Cocoa screen coordinates).
fn cocoa(r: Rect) -> NSRect {
    NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.w, r.h))
}

/// The catching windows of one pane, kept per stage.
#[derive(Default)]
pub struct Reach {
    targets: HashMap<u64, Target>,
}

impl Reach {
    /// Puts a catching window at each of `wanted` (stage, Cocoa screen frame, the card's
    /// element) under `parent` (the pane's panel), keeping each stage's while it stays and
    /// closing the rest. `strip` is the strip's key, `sink` where the pointer's events go.
    pub fn update(&mut self, mtm: MainThreadMarker, parent: &NSPanel, strip: u32, wanted: Vec<(u64, Rect, Retained<CardElement>)>, sink: MouseSink) {
        let gone: Vec<u64> = self.targets.keys().copied().filter(|s| !wanted.iter().any(|(w, _, _)| w == s)).collect();
        for stage in gone {
            if let Some(t) = self.targets.remove(&stage) {
                t.close(parent);
            }
        }
        if self.targets.is_empty() && wanted.is_empty() {
            return;
        }
        for (stage, frame, element) in wanted {
            let target = self.targets.entry(stage).or_insert_with(|| Target::new(mtm, parent, strip, frame, sink));
            if target.frame != frame {
                target.frame = frame;
                target.panel.setFrame_display(cocoa(frame), false);
            }
            let held = target.view.ivars().element.borrow().as_ref().is_some_and(|e| Retained::as_ptr(e) == Retained::as_ptr(&element));
            if !held {
                *target.view.ivars().element.borrow_mut() = Some(element);
            }
        }
        tracing::trace!(cards = self.targets.len(), "strip reach: catching windows under the cards");
    }

    /// Closes every catching window.
    pub fn clear(&mut self, parent: &NSPanel) {
        for (_, t) in self.targets.drain() {
            t.close(parent);
        }
    }

    /// Follows the pane's level (above or below windows), staying just under it.
    pub fn set_level(&self, level: isize) {
        for t in self.targets.values() {
            t.panel.setLevel(level);
        }
    }
}

impl Drop for Reach {
    fn drop(&mut self) {
        for t in self.targets.values() {
            t.panel.orderOut(None);
            t.panel.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_card_is_covered_where_it_rests_in_screen_coordinates() {
        // A panel at x 0, from y 25 up to 1440 (Cocoa: bottom-left origin).
        let panel = Rect::new(0.0, 25.0, 300.0, 1415.0);
        let frames = vec![(7, Rect::new(16.0, 100.0, 212.0, 138.0)), (8, Rect::new(16.0, 260.0, 212.0, 138.0))];
        let t = targets(true, panel, &frames);
        assert_eq!(t[0], (7, Rect::new(16.0, 25.0 + 1415.0 - 238.0, 212.0, 138.0)), "flipped: the top card highest on screen");
        assert_eq!(t[1].1.y + t[1].1.h, t[0].1.y - 22.0, "the gap between them stays uncovered");
    }

    #[test]
    fn nothing_is_covered_without_an_assistive_app_or_a_card() {
        let panel = Rect::new(0.0, 0.0, 300.0, 1000.0);
        assert!(targets(false, panel, &[(7, Rect::new(16.0, 100.0, 212.0, 138.0))]).is_empty(), "the pane's own rules alone");
        assert!(targets(true, panel, &[(7, Rect::new(16.0, 100.0, 0.0, 138.0))]).is_empty(), "a card with no size");
        assert!(targets(true, panel, &[]).is_empty());
    }
}
