//! The strip for VoiceOver and other assistive apps: the cards are Core Animation layers,
//! which the accessibility tree never sees, so the strip's view lists one element per card.
//!
//! - **Elements** ([`CardElement`], an `NSAccessibilityElement`): role button, labelled with
//!   the stage and its apps ("Stage 3: Safari, Notes", or the stage's name), help saying what
//!   pressing does, framed where the card rests in the view (AppKit turns that into screen
//!   coordinates as the panel moves). VoiceOver's press switches as a click does; each app
//!   gets a custom action that shows all its windows, as a click on its icon does.
//! - **The list** follows the cards: [`items`] builds it from what the strip lays out (a
//!   pure function, unit tested) and [`Access::update`] reuses each card's element while its
//!   stage stays, so VoiceOver keeps its place. While the strip is out of sight the list is
//!   empty, as nothing can be pressed there.
//! - Main thread only, as the strip.

use std::cell::Cell;
use std::collections::HashMap;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityButton, NSAccessibilityButtonRole, NSAccessibilityCustomAction, NSAccessibilityElement,
    NSAccessibilityElementProtocol, NSAccessibilityGroupRole, NSAccessibilityLayoutChangedNotification, NSAccessibilityPostNotification,
    NSView,
};
use objc2_foundation::{NSArray, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::engine::geometry::Rect;
use crate::ui::strip_model::CardSnapshot;

/// What one card says to assistive apps, and where it is.
#[derive(Clone, Debug, PartialEq)]
pub struct CardItem {
    /// The card's stage.
    pub stage: u64,
    /// What VoiceOver reads first: the stage and its apps.
    pub label: String,
    /// What pressing it does.
    pub help: String,
    /// Where the card rests, in the strip view's coordinates (top-left origin).
    pub frame: Rect,
    /// Its apps, in icon order: (pid, name) for the "show all windows" actions.
    pub apps: Vec<(i32, String)>,
}

/// The card's label: its name, else "Stage N" (its ⌃⌥N number); then its apps by name,
/// e.g. "Stage 3: Safari, Notes". An app whose name is unknown is left out; the current stage
/// (a card shown for it, `[strip] active = "card"`) says so.
pub fn card_label(card: &CardSnapshot, names: &[String]) -> String {
    let stage = match card.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => name.to_string(),
        None => format!("Stage {}", card.number),
    };
    let mut label = if names.is_empty() { stage } else { format!("{stage}: {}", names.join(", ")) };
    if card.active {
        label.push_str(", current stage");
    }
    label
}

/// The card's help text: what pressing it does.
pub fn card_help(card: &CardSnapshot) -> String {
    if card.active { "The stage shown now.".to_string() } else { "Switches to this stage.".to_string() }
}

/// The elements for `cards` (as the strip shows them) at `frames` (each card's resting frame
/// by stage, view coordinates), top to bottom; `name` gives an app's name by pid. Cards with no
/// frame (the drop gap, the active stage's empty slot, a card not laid out) are left out.
pub fn items(cards: &[CardSnapshot], frames: &[(u64, Rect)], name: impl Fn(i32) -> Option<String>) -> Vec<CardItem> {
    frames
        .iter()
        .filter_map(|(stage, frame)| {
            let card = cards.iter().find(|c| c.stage == *stage && !c.gap)?;
            let apps: Vec<(i32, String)> = card.apps.iter().filter_map(|pid| name(*pid).map(|n| (*pid, n))).collect();
            let names: Vec<String> = apps.iter().map(|(_, n)| n.clone()).collect();
            Some(CardItem { stage: card.stage, label: card_label(card, &names), help: card_help(card), frame: *frame, apps })
        })
        .collect()
}

/// The custom action's name for showing all of `app`'s windows (a click on its icon).
pub fn app_action_name(app: &str) -> String {
    format!("Show all {app} windows")
}

/// Where a pressed card or app action goes: (strip key, stage, the app's pid for an icon).
pub type PressSink = fn(u32, u64, Option<i32>);

/// One card's element: its strip and stage, and where a press goes.
pub struct CardTarget {
    strip: Cell<u32>,
    stage: Cell<u64>,
    sink: Cell<Option<PressSink>>,
}

define_class!(
    // SAFETY: NSAccessibilityElement has no subclassing requirements; CardElement does not
    // implement Drop.
    #[unsafe(super = NSAccessibilityElement)]
    #[name = "SMCardAccessibilityElement"]
    #[ivars = CardTarget]
    pub struct CardElement;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for CardElement {}

    // SAFETY: the superclass implements the element protocol.
    unsafe impl NSAccessibilityElementProtocol for CardElement {}

    // SAFETY: the signature matches NSAccessibilityButton; the label comes from the superclass's
    // storage (`setAccessibilityLabel`).
    unsafe impl NSAccessibilityButton for CardElement {
        #[unsafe(method(accessibilityPerformPress))]
        fn perform_press(&self) -> bool {
            let t = self.ivars();
            match t.sink.get() {
                Some(sink) => {
                    sink(t.strip.get(), t.stage.get(), None);
                    true
                }
                None => false,
            }
        }
    }
);

impl CardElement {
    fn new(strip: u32, stage: u64, sink: PressSink) -> Retained<Self> {
        let this = Self::alloc().set_ivars(CardTarget { strip: Cell::new(strip), stage: Cell::new(stage), sink: Cell::new(Some(sink)) });
        // SAFETY: NSObject's designated initializer on a fresh instance.
        unsafe { msg_send![super(this), init] }
    }

    /// Says what `item` says, framed where it rests; its app actions renewed.
    fn show(&self, item: &CardItem, sink: PressSink) {
        self.setAccessibilityLabel(Some(&NSString::from_str(&item.label)));
        self.setAccessibilityHelp(Some(&NSString::from_str(&item.help)));
        let f = item.frame;
        self.setAccessibilityFrameInParentSpace(NSRect::new(NSPoint::new(f.x, f.y), NSSize::new(f.w, f.h)));
        let (strip, stage) = (self.ivars().strip.get(), item.stage);
        let actions: Vec<Retained<NSAccessibilityCustomAction>> = item
            .apps
            .iter()
            .map(|(pid, name)| {
                let pid = *pid;
                let handler = RcBlock::new(move || {
                    sink(strip, stage, Some(pid));
                    Bool::YES
                });
                NSAccessibilityCustomAction::initWithName_handler(
                    NSAccessibilityCustomAction::alloc(),
                    &NSString::from_str(&app_action_name(name)),
                    Some(&handler),
                )
            })
            .collect();
        self.setAccessibilityCustomActions(Some(&NSArray::from_retained_slice(&actions)));
    }
}

/// The accessibility elements of one strip view (a pane's), kept per stage.
#[derive(Default)]
pub struct Access {
    elements: HashMap<u64, Retained<CardElement>>,
    /// What the view lists now.
    shown: Vec<CardItem>,
}

impl Access {
    /// Lists `items` as `view`'s children (top to bottom), each card keeping its element while
    /// its stage stays; nothing when the list is unchanged. `strip` is the strip's key, `sink`
    /// where presses go.
    pub fn update(&mut self, view: &NSView, strip: u32, items: Vec<CardItem>, sink: PressSink) {
        if items == self.shown {
            return;
        }
        self.elements.retain(|stage, _| items.iter().any(|i| i.stage == *stage));
        let mut children: Vec<Retained<AnyObject>> = Vec::with_capacity(items.len());
        for item in &items {
            let element = self.elements.entry(item.stage).or_insert_with(|| {
                let e = CardElement::new(strip, item.stage, sink);
                // SAFETY: a framework constant.
                e.setAccessibilityRole(Some(unsafe { NSAccessibilityButtonRole }));
                // SAFETY: the view outlives its elements (they go with its pane).
                unsafe { e.setAccessibilityParent(Some(view)) };
                e
            });
            element.show(item, sink);
            children.push(Retained::into_super(Retained::into_super(Retained::into_super(element.clone()))));
        }
        let children = NSArray::from_retained_slice(&children);
        // SAFETY: an array of accessibility elements, as the property holds.
        unsafe { view.setAccessibilityChildren(Some(&children)) };
        let changed_set = self.shown.len() != items.len() || self.shown.iter().zip(&items).any(|(a, b)| a.stage != b.stage);
        self.shown = items;
        if changed_set {
            let target: &AnyObject = view;
            // SAFETY: a live view and a framework notification name; main thread.
            unsafe { NSAccessibilityPostNotification(target, NSAccessibilityLayoutChangedNotification) };
        }
    }
}

/// The child of `view` (the strip's) at `p` (screen coordinates, Cocoa): the card's element
/// there, if any (its children are the cards' elements, [`Access::update`]).
pub fn hit(view: &NSView, p: NSPoint) -> Option<Retained<AnyObject>> {
    let children = view.accessibilityChildren()?;
    children.iter().find(|child| {
        // SAFETY: every child is an accessibility element (an NSAccessibilityElement).
        let f: NSRect = unsafe { msg_send![&**child, accessibilityFrame] };
        p.x >= f.origin.x && p.x < f.origin.x + f.size.width && p.y >= f.origin.y && p.y < f.origin.y + f.size.height
    })
}

/// Makes `view` (the strip's) a group VoiceOver can enter: it holds the cards' elements.
pub fn describe_strip_view(view: &NSView) {
    view.setAccessibilityElement(true);
    // SAFETY: a framework constant.
    view.setAccessibilityRole(Some(unsafe { NSAccessibilityGroupRole }));
    view.setAccessibilityLabel(Some(&NSString::from_str("Stages")));
}

/// Makes `view` (the edge tab's) a button VoiceOver reads; its press reveals the strip.
pub fn describe_edge_tab(view: &NSView) {
    view.setAccessibilityElement(true);
    // SAFETY: a framework constant.
    view.setAccessibilityRole(Some(unsafe { NSAccessibilityButtonRole }));
    view.setAccessibilityLabel(Some(&NSString::from_str("Stages")));
    view.setAccessibilityHelp(Some(&NSString::from_str("Shows the strip over the window covering it.")));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(stage: u64, number: usize, name: Option<&str>, apps: &[i32]) -> CardSnapshot {
        CardSnapshot { stage, number, name: name.map(str::to_string), apps: apps.to_vec(), ..Default::default() }
    }

    fn names(pid: i32) -> Option<String> {
        match pid {
            1 => Some("Safari".to_string()),
            2 => Some("Notes".to_string()),
            _ => None,
        }
    }

    #[test]
    fn a_card_reads_its_number_and_apps() {
        let c = card(7, 3, None, &[1, 2]);
        assert_eq!(card_label(&c, &["Safari".into(), "Notes".into()]), "Stage 3: Safari, Notes");
        assert_eq!(card_label(&c, &[]), "Stage 3");
        assert_eq!(card_help(&c), "Switches to this stage.");
    }

    #[test]
    fn a_named_stage_reads_its_name() {
        let c = card(7, 3, Some("Mail"), &[1]);
        assert_eq!(card_label(&c, &["Safari".into()]), "Mail: Safari");
        let blank = card(7, 3, Some("  "), &[1]);
        assert_eq!(card_label(&blank, &[]), "Stage 3", "a blank name is no name");
    }

    #[test]
    fn the_current_stage_says_so() {
        let c = CardSnapshot { active: true, ..card(1, 1, None, &[2]) };
        assert_eq!(card_label(&c, &["Notes".into()]), "Stage 1: Notes, current stage");
        assert_eq!(card_help(&c), "The stage shown now.");
    }

    #[test]
    fn the_list_follows_the_laid_out_cards_top_to_bottom() {
        let cards = vec![card(10, 1, None, &[1, 9]), card(20, 2, Some("Writing"), &[2]), CardSnapshot { gap: true, ..card(30, 3, None, &[]) }];
        let frames = vec![(20, Rect::new(0.0, 10.0, 100.0, 80.0)), (10, Rect::new(0.0, 100.0, 100.0, 80.0)), (30, Rect::new(0.0, 200.0, 1.0, 1.0)), (99, Rect::new(0.0, 0.0, 1.0, 1.0))];
        let list = items(&cards, &frames, names);
        let labels: Vec<&str> = list.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Writing: Notes", "Stage 1: Safari"], "in the frames' order; the gap and a card not shown are left out, and an unnamed app");
        assert_eq!(list[1].frame, Rect::new(0.0, 100.0, 100.0, 80.0));
        assert_eq!(list[1].apps, vec![(1, "Safari".to_string())]);
        assert_eq!(app_action_name("Safari"), "Show all Safari windows");
    }
}
