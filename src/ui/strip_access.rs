//! The strip for VoiceOver and other assistive apps: the cards are Core Animation layers,
//! which the accessibility tree never sees, so the strip's view lists one element per card.
//!
//! - **Elements** ([`CardElement`], an `NSAccessibilityElement`): role button, labelled as
//!   Apple's card is, with its front window's title (its app's name when untitled), help
//!   saying what pressing does, framed where the card rests, in Cocoa screen coordinates
//!   ([`screen_frame`]; AppKit hands clients top-left ones). Set as the frame in the flipped
//!   view's space it came out upside down (11-09: a top card read y 1057 on a 1440 pt
//!   screen), so the element answers `accessibilityFrame` itself. Its actions are Apple's: `AXPress` switches
//!   as a click does, `AXAddToStage` adds the card's front window to the stage in front, as a
//!   Shift-click does. Each app also gets a custom action that shows all its windows, as a
//!   click on its icon does.
//! - **The list** follows the cards: [`items`] builds it from what the strip lays out (a
//!   pure function, unit tested) and [`Access::update`] reuses each card's element while its
//!   stage stays, so VoiceOver keeps its place. While the strip is out of sight the list is
//!   empty, as nothing can be pressed there.
//! - **Reaching them**: the strip's pane lets the mouse through off the cards, so a hit test
//!   from outside (VoiceOver's, the pointer elsewhere) misses it; while an assistive app is on
//!   [`crate::ui::strip_reach`] puts a catching window under each card.
//! - Main thread only, as the strip.

use std::cell::Cell;
use std::collections::HashMap;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityActionDescription, NSAccessibilityButton, NSAccessibilityButtonRole, NSAccessibilityCustomAction,
    NSAccessibilityElement, NSAccessibilityElementProtocol, NSAccessibilityGroupRole, NSAccessibilityLayoutChangedNotification,
    NSAccessibilityPostNotification, NSAccessibilityPressAction, NSView,
};
use objc2_foundation::{NSArray, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::engine::geometry::Rect;
use crate::ui::strip_model::{CardSnapshot, StripEvent};

/// Apple's action on a card for Shift-click's add (its strip's buttons list it beside
/// `AXPress`).
pub const ADD_TO_STAGE: &str = "AXAddToStage";

/// What one card says to assistive apps, and where it is.
#[derive(Clone, Debug, PartialEq)]
pub struct CardItem {
    /// The card's stage.
    pub stage: u64,
    /// What VoiceOver reads first: the front window's title ([`card_label`]).
    pub label: String,
    /// What pressing it does.
    pub help: String,
    /// It offers [`ADD_TO_STAGE`]: a card of a stage not in front.
    pub adds: bool,
    /// Where the card rests, in Cocoa screen coordinates (bottom-left origin, the main
    /// screen's): [`screen_frame`].
    pub frame: Rect,
    /// Its apps, in icon order: (pid, name) for the "show all windows" actions.
    pub apps: Vec<(i32, String)>,
}

/// The card's label, as Apple's: its front window's title, else (untitled) its app's name
/// `front_app`; a card of several apps reads its front window's alone, as Apple's does.
/// Apple's has no stage number; a stage given a name (ours alone) says it first ("Mail:
/// Inbox"), and with nothing known the card says its ⌃⌥N number ("Stage 3"). The current
/// stage (a card shown for it, `[strip] active = "card"`) says so.
pub fn card_label(card: &CardSnapshot, front_app: Option<&str>) -> String {
    let name = card.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let title = card.title.trim();
    let what = if title.is_empty() { front_app.map(str::trim).unwrap_or_default() } else { title };
    let mut label = match (name, what.is_empty()) {
        (Some(name), false) => format!("{name}: {what}"),
        (Some(name), true) => name.to_string(),
        (None, false) => what.to_string(),
        (None, true) => format!("Stage {}", card.number),
    };
    if card.active {
        label.push_str(", current stage");
    }
    label
}

/// The card's help text: what pressing it does.
pub fn card_help(card: &CardSnapshot) -> String {
    if card.active { "The stage shown now.".to_string() } else { "Switches to this stage.".to_string() }
}

/// Where a card at `f` (the strip view's coordinates: top-left origin, flipped) is on screen,
/// in Cocoa screen coordinates (bottom-left origin at the main screen's bottom left), its
/// pane's panel being at `panel` (Cocoa screen coordinates; the view fills it).
pub fn screen_frame(panel: Rect, f: Rect) -> Rect {
    Rect::new(panel.x + f.x, panel.y + panel.h - (f.y + f.h), f.w, f.h)
}

/// The elements for `cards` (as the strip shows them) at `frames` (each card's resting frame
/// by stage, view coordinates) in the panel at `panel` (Cocoa screen coordinates), top to
/// bottom; `name` gives an app's name by pid. Cards with no frame (the drop gap, the active
/// stage's empty slot, a card not laid out) are left out.
pub fn items(cards: &[CardSnapshot], frames: &[(u64, Rect)], panel: Rect, name: impl Fn(i32) -> Option<String>) -> Vec<CardItem> {
    frames
        .iter()
        .filter_map(|(stage, frame)| {
            let card = cards.iter().find(|c| c.stage == *stage && !c.gap)?;
            let apps: Vec<(i32, String)> = card.apps.iter().filter_map(|pid| name(*pid).map(|n| (*pid, n))).collect();
            // The front window's app (the first icon), named or not.
            let front = card.apps.first().and_then(|pid| apps.iter().find(|(p, _)| p == pid)).map(|(_, n)| n.as_str());
            let label = card_label(card, front);
            Some(CardItem { stage: card.stage, label, help: card_help(card), adds: !card.active, frame: screen_frame(panel, *frame), apps })
        })
        .collect()
}

/// The custom action's name for showing all of `app`'s windows (a click on its icon).
pub fn app_action_name(app: &str) -> String {
    format!("Show all {app} windows")
}

/// What an assistive app asked of a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// `AXPress`: a click.
    Card,
    /// [`ADD_TO_STAGE`]: a Shift-click.
    Add,
    /// An app's custom action: a click on its icon.
    App(i32),
}

/// The action named `action` on a card that offers [`ADD_TO_STAGE`] or not (`adds`).
pub fn press_for(action: &str, adds: bool) -> Option<Press> {
    match action {
        "AXPress" => Some(Press::Card),
        ADD_TO_STAGE if adds => Some(Press::Add),
        _ => None,
    }
}

/// The action names a card lists, `AXPress` first, as Apple's.
pub fn action_names(adds: bool) -> Vec<&'static str> {
    if adds { vec!["AXPress", ADD_TO_STAGE] } else { vec!["AXPress"] }
}

/// The event `press` on `stage`'s card sends: what the click it stands for sends.
pub fn event_for(stage: u64, press: Press) -> StripEvent {
    match press {
        Press::Card => StripEvent::CardClicked { stage, shift: false },
        Press::Add => StripEvent::CardClicked { stage, shift: true },
        Press::App(pid) => StripEvent::IconClicked { stage, pid },
    }
}

/// Where a pressed card or app action goes: (strip key, stage, what was asked).
pub type PressSink = fn(u32, u64, Press);

/// One card's element: its strip and stage, whether it adds, and where a press goes.
pub struct CardTarget {
    strip: Cell<u32>,
    stage: Cell<u64>,
    adds: Cell<bool>,
    /// Where it is, Cocoa screen coordinates.
    frame: Cell<NSRect>,
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
            self.ask(Press::Card)
        }
    }

    impl CardElement {
        // Its frame in screen coordinates, as the protocol asks (AppKit flips it for clients).
        // SAFETY: the signature matches NSAccessibilityElement's.
        #[unsafe(method(accessibilityFrame))]
        fn accessibility_frame(&self) -> NSRect {
            self.ivars().frame.get()
        }

        // The actions by name, as Apple's card lists them (`AXPress`, `AXAddToStage`): the
        // protocol methods have no way to add a named action of one's own, so these three
        // (NSAccessibility's informal protocol, still what AppKit answers clients from) say
        // them. SAFETY (all three): signatures match NSObject's NSAccessibility methods.
        #[unsafe(method_id(accessibilityActionNames))]
        fn action_names(&self) -> Retained<NSArray<NSString>> {
            let names: Vec<Retained<NSString>> = action_names(self.ivars().adds.get()).into_iter().map(NSString::from_str).collect();
            NSArray::from_retained_slice(&names)
        }

        #[unsafe(method_id(accessibilityActionDescription:))]
        fn action_description(&self, action: &NSString) -> Option<Retained<NSString>> {
            match press_for(&action.to_string(), self.ivars().adds.get()) {
                Some(Press::Add) => Some(NSString::from_str("add to stage")),
                // SAFETY: a framework constant; AppKit's own description of the press.
                Some(_) => NSAccessibilityActionDescription(unsafe { NSAccessibilityPressAction }),
                None => None,
            }
        }

        #[unsafe(method(accessibilityPerformAction:))]
        fn perform_action(&self, action: &NSString) {
            if let Some(press) = press_for(&action.to_string(), self.ivars().adds.get()) {
                self.ask(press);
            }
        }
    }
);

impl CardElement {
    fn new(strip: u32, stage: u64, sink: PressSink) -> Retained<Self> {
        let target = CardTarget { strip: Cell::new(strip), stage: Cell::new(stage), adds: Cell::new(false), frame: Cell::new(NSRect::ZERO), sink: Cell::new(Some(sink)) };
        let this = Self::alloc().set_ivars(target);
        // SAFETY: NSObject's designated initializer on a fresh instance.
        unsafe { msg_send![super(this), init] }
    }

    /// Sends `press` on this card where presses go; false with nowhere to send it.
    fn ask(&self, press: Press) -> bool {
        let t = self.ivars();
        match t.sink.get() {
            Some(sink) => {
                sink(t.strip.get(), t.stage.get(), press);
                true
            }
            None => false,
        }
    }

    /// Says what `item` says, framed where it rests; its app actions renewed.
    fn show(&self, item: &CardItem, sink: PressSink) {
        self.setAccessibilityLabel(Some(&NSString::from_str(&item.label)));
        self.setAccessibilityHelp(Some(&NSString::from_str(&item.help)));
        let f = item.frame;
        self.ivars().frame.set(NSRect::new(NSPoint::new(f.x, f.y), NSSize::new(f.w, f.h)));
        self.ivars().adds.set(item.adds);
        let (strip, stage) = (self.ivars().strip.get(), item.stage);
        let actions: Vec<Retained<NSAccessibilityCustomAction>> = item
            .apps
            .iter()
            .map(|(pid, name)| {
                let pid = *pid;
                let handler = RcBlock::new(move || {
                    sink(strip, stage, Press::App(pid));
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
    /// The element of `stage`'s card, while the view lists it.
    pub fn element(&self, stage: u64) -> Option<&Retained<CardElement>> {
        self.shown.iter().any(|i| i.stage == stage).then(|| self.elements.get(&stage)).flatten()
    }

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

    fn titled(title: &str, c: CardSnapshot) -> CardSnapshot {
        CardSnapshot { title: title.to_string(), ..c }
    }

    fn names(pid: i32) -> Option<String> {
        match pid {
            1 => Some("Safari".to_string()),
            2 => Some("Notes".to_string()),
            _ => None,
        }
    }

    #[test]
    fn a_card_reads_its_front_windows_title_as_apples() {
        let c = titled("Quarterly plan \u{2014} Edited", card(7, 3, None, &[1, 2]));
        assert_eq!(card_label(&c, Some("Safari")), "Quarterly plan \u{2014} Edited", "the front window's alone: no number, no other app");
        assert_eq!(card_help(&c), "Switches to this stage.");
    }

    #[test]
    fn an_untitled_window_reads_its_apps_name() {
        let c = titled("  ", card(7, 3, None, &[1]));
        assert_eq!(card_label(&c, Some("Calculator")), "Calculator");
        assert_eq!(card_label(&c, None), "Stage 3", "nothing known: its \u{2303}\u{2325}N number");
    }

    #[test]
    fn a_named_stage_says_its_name_first() {
        let c = titled("Inbox", card(7, 3, Some("Mail"), &[1]));
        assert_eq!(card_label(&c, Some("Mail")), "Mail: Inbox");
        assert_eq!(card_label(&titled("", c.clone()), None), "Mail");
        let blank = card(7, 3, Some("  "), &[1]);
        assert_eq!(card_label(&blank, Some("Notes")), "Notes", "a blank name is no name");
    }

    #[test]
    fn the_current_stage_says_so() {
        let c = CardSnapshot { active: true, ..titled("Todo", card(1, 1, None, &[2])) };
        assert_eq!(card_label(&c, Some("Notes")), "Todo, current stage");
        assert_eq!(card_help(&c), "The stage shown now.");
    }

    /// A Cocoa screen rect as Accessibility's clients get it: top-left origin at the main
    /// screen's top left (`primary_h` its height), as AppKit converts `accessibilityFrame`.
    fn top_left(primary_h: f64, r: Rect) -> Rect {
        Rect::new(r.x, primary_h - (r.y + r.h), r.w, r.h)
    }

    #[test]
    fn a_cards_frame_is_where_it_is_on_either_screen() {
        // The 5K (2560x1440, main) and the laptop to its left at -1800,271 1800x1169 in
        // top-left coordinates: Cocoa (-1800, 1440 - 271 - 1169 = 0).
        let primary_h = 1440.0;
        for (screen_top_left, visible_inset_top, dock) in [(Rect::new(0.0, 0.0, 2560.0, 1440.0), 25.0, 0.0), (Rect::new(-1800.0, 271.0, 1800.0, 1169.0), 33.0, 60.0)] {
            // The panel spans the visible frame's height at the screen's left (as `Geometry`).
            let top = screen_top_left.y + visible_inset_top;
            let h = screen_top_left.h - visible_inset_top - dock;
            let panel = Rect::new(screen_top_left.x, primary_h - (top + h), 300.0, h);
            // A card 16 pt in and 300 pt down the column: globally there, top-left.
            let local = Rect::new(16.0, 300.0, 83.0, 113.0);
            let global = Rect::new(screen_top_left.x + 16.0, top + 300.0, 83.0, 113.0);
            assert_eq!(top_left(primary_h, screen_frame(panel, local)), global, "{screen_top_left:?}");
        }
    }

    #[test]
    fn a_card_offers_apples_press_and_add_to_stage() {
        assert_eq!(action_names(true), ["AXPress", "AXAddToStage"]);
        assert_eq!(action_names(false), ["AXPress"], "the stage in front has nothing to add to");
        assert_eq!(press_for("AXPress", true), Some(Press::Card));
        assert_eq!(press_for("AXAddToStage", true), Some(Press::Add));
        assert_eq!(press_for("AXAddToStage", false), None);
        assert_eq!(press_for("AXShowMenu", true), None);
    }

    #[test]
    fn each_action_sends_the_click_it_stands_for() {
        assert_eq!(event_for(4, Press::Card), StripEvent::CardClicked { stage: 4, shift: false });
        assert_eq!(event_for(4, Press::Add), StripEvent::CardClicked { stage: 4, shift: true }, "a Shift-click: the front window joins the stage in front");
        assert_eq!(event_for(4, Press::App(9)), StripEvent::IconClicked { stage: 4, pid: 9 });
    }

    #[test]
    fn the_list_follows_the_laid_out_cards_top_to_bottom() {
        let active = CardSnapshot { active: true, ..card(40, 4, None, &[2]) };
        let cards = vec![card(10, 1, None, &[9, 1]), titled("Draft", card(20, 2, Some("Writing"), &[2])), CardSnapshot { gap: true, ..card(30, 3, None, &[]) }, active];
        let frames = vec![
            (20, Rect::new(0.0, 10.0, 100.0, 80.0)),
            (10, Rect::new(0.0, 100.0, 100.0, 80.0)),
            (30, Rect::new(0.0, 200.0, 1.0, 1.0)),
            (99, Rect::new(0.0, 0.0, 1.0, 1.0)),
            (40, Rect::new(0.0, 300.0, 100.0, 80.0)),
        ];
        let list = items(&cards, &frames, Rect::new(0.0, 0.0, 300.0, 1000.0), names);
        let labels: Vec<&str> = list.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Writing: Draft", "Stage 1", "Notes, current stage"],
            "in the frames' order; the gap and a card not shown are left out; an untitled window of an unnamed app falls back to the number"
        );
        assert_eq!(list.iter().map(|i| i.adds).collect::<Vec<_>>(), [true, true, false], "the stage in front adds nothing");
        assert_eq!(list[1].frame, Rect::new(0.0, 820.0, 100.0, 80.0), "in screen coordinates: 1000 - (100 + 80)");
        assert_eq!(list[1].apps, vec![(1, "Safari".to_string())]);
        assert_eq!(app_action_name("Safari"), "Show all Safari windows");
    }
}
