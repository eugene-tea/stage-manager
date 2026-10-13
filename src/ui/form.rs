//! A small AppKit toolkit for the app's own windows (Settings, the welcome), in the look of
//! System Settings: a grouped form of rounded boxes, each a column of rows with a title, an
//! optional grey explanation and a control at the trailing edge. Standard controls, system
//! fonts and dynamic system colors only, so light and dark both come from AppKit.
//!
//! Layout is by frames in a flipped view (top-down), computed once per build. Controls send
//! their action to an [`ActionTarget`], which hands the control's tag to a Rust closure.
//! [`render_png`] draws a window into a PNG in-process (`cacheDisplayInRect`), never from
//! the screen, for the preview mode.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSBitmapImageFileType,
    NSBox, NSBoxType, NSButton, NSColor, NSControl, NSControlSize, NSControlStateValueOff, NSControlStateValueOn, NSFont,
    NSFontWeightRegular, NSFontWeightSemibold, NSImage, NSImageSymbolConfiguration, NSImageView, NSLineBreakMode,
    NSPopUpButton, NSSwitch, NSTextAlignment, NSTextField, NSTitlePosition, NSView, NSWindow,
};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

define_class!(
    // SAFETY: NSView has no subclassing requirements; FlippedView does not implement Drop.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[name = "SMFormFlippedView"]
    pub struct FlippedView;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for FlippedView {}

    impl FlippedView {
        // SAFETY: signature matches NSView's `isFlipped`.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl FlippedView {
    /// A top-down view of `size`.
    pub fn new(mtm: MainThreadMarker, size: NSSize) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's designated initializer on a freshly allocated instance.
        unsafe { msg_send![super(this), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)] }
    }
}

/// The closure an [`ActionTarget`] calls with the sender's tag.
pub type ActionFn = Rc<dyn Fn(isize)>;

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the ivar is plain Rust data.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "SMFormActionTarget"]
    #[ivars = RefCell<Option<ActionFn>>]
    pub struct ActionTarget;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for ActionTarget {}

    impl ActionTarget {
        // SAFETY: action signature `- (void)act:(id)sender`.
        #[unsafe(method(act:))]
        fn act(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            // SAFETY: every sender is an NSControl (or NSMenuItem), both answer `tag`.
            let tag: isize = unsafe { msg_send![sender, tag] };
            let f = self.ivars().borrow().clone();
            if let Some(f) = f {
                f(tag);
            }
        }
    }
);

impl ActionTarget {
    /// A target that calls `f` with each sender's tag.
    pub fn new(mtm: MainThreadMarker, f: impl Fn(isize) + 'static) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(Some(Rc::new(f))));
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }

    /// The action every control sends.
    pub fn action() -> Sel {
        sel!(act:)
    }
}

/// Wires `control` to `target` with `tag`.
pub fn wire(control: &NSControl, target: &ActionTarget, tag: isize) {
    // SAFETY: the target outlives the window's controls (the window owner keeps both).
    unsafe {
        control.setTarget(Some(target));
        control.setAction(Some(ActionTarget::action()));
    }
    control.setTag(tag);
}

/// A one-line label.
pub fn label(mtm: MainThreadMarker, text: &str, size: f64, semibold: bool) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    // SAFETY: the weight constants are AppKit's own.
    let weight = unsafe { if semibold { NSFontWeightSemibold } else { NSFontWeightRegular } };
    l.setFont(Some(&NSFont::systemFontOfSize_weight(size, weight)));
    l.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    l
}

/// A wrapping label `width` wide; returns it and its height.
pub fn wrapping(mtm: MainThreadMarker, text: &str, width: f64, size: f64, color: &NSColor) -> (Retained<NSTextField>, f64) {
    let l = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    l.setFont(Some(&NSFont::systemFontOfSize(size)));
    l.setTextColor(Some(color));
    l.setSelectable(false);
    l.setPreferredMaxLayoutWidth(width);
    let h = height_of(&l, width);
    l.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, h)));
    (l, h)
}

/// A text field's height at `width`.
pub fn height_of(field: &NSTextField, width: f64) -> f64 {
    field.sizeThatFits(NSSize::new(width, 10_000.0)).height.ceil()
}

/// A switch, on or off.
pub fn switch(mtm: MainThreadMarker, on: bool, target: &ActionTarget, tag: isize) -> Retained<NSSwitch> {
    let s = NSSwitch::new(mtm);
    s.setControlSize(NSControlSize::Mini);
    s.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    s.sizeToFit();
    wire(&s, target, tag);
    s
}

/// A push button sized to its title.
pub fn button(mtm: MainThreadMarker, title: &str, target: &ActionTarget, tag: isize) -> Retained<NSButton> {
    // SAFETY: the action is implemented by `target`, which the caller keeps alive.
    let b = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(ActionTarget::action()), mtm) };
    b.setTag(tag);
    b.setControlSize(NSControlSize::Regular);
    b.sizeToFit();
    b
}

/// A pop-up button with `items`, `selected` chosen.
pub fn popup(mtm: MainThreadMarker, items: &[&str], selected: usize, target: &ActionTarget, tag: isize) -> Retained<NSPopUpButton> {
    let p = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    for i in items {
        p.addItemWithTitle(&NSString::from_str(i));
    }
    p.selectItemAtIndex(selected as isize);
    p.sizeToFit();
    wire(&p, target, tag);
    p
}

/// An SF Symbol image view of `points` size, tinted `color` (template otherwise).
pub fn symbol(mtm: MainThreadMarker, name: &str, points: f64, color: Option<&NSColor>) -> Retained<NSImageView> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)
        .unwrap_or_default();
    let view = NSImageView::imageViewWithImage(&image, mtm);
    // SAFETY: the weight constant is AppKit's own.
    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(points, unsafe { NSFontWeightRegular });
    view.setSymbolConfiguration(Some(&config));
    if let Some(c) = color {
        view.setContentTintColor(Some(c));
    }
    view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(points + 8.0, points + 8.0)));
    view
}

/// One row of a group.
pub struct Row {
    /// The leading title.
    pub title: String,
    /// Grey text under the title.
    pub detail: Option<String>,
    /// The detail in this color instead of grey (errors).
    pub detail_color: Option<Retained<NSColor>>,
    /// A leading icon.
    pub icon: Option<Retained<NSView>>,
    /// Controls at the trailing edge, left to right.
    pub trailing: Vec<Retained<NSView>>,
    /// A full-width view under the title block, with its height; its width is set to the
    /// row's inner width.
    pub below: Option<(Retained<NSView>, f64)>,
}

impl Row {
    /// A row with a title and one trailing control.
    pub fn new(title: &str, control: Option<Retained<NSView>>) -> Self {
        Self { title: title.into(), detail: None, detail_color: None, icon: None, trailing: control.into_iter().collect(), below: None }
    }

    /// With grey text under the title.
    pub fn detail(mut self, text: impl Into<String>) -> Self {
        self.detail = Some(text.into());
        self
    }
}

/// Builds a grouped form top-down.
pub struct Form {
    mtm: MainThreadMarker,
    /// The form's width.
    pub width: f64,
    /// Left and right margin around groups.
    pub margin: f64,
    view: Retained<FlippedView>,
    y: f64,
    /// The detail labels made, by row title (for in-place updates).
    pub details: Vec<(String, Retained<NSTextField>)>,
}

const ROW_PAD_X: f64 = 12.0;
const ROW_PAD_Y: f64 = 9.0;
const ROW_MIN: f64 = 40.0;
const GAP: f64 = 8.0;

impl Form {
    /// An empty form `width` wide.
    pub fn new(mtm: MainThreadMarker, width: f64) -> Self {
        Self { mtm, width, margin: 20.0, view: FlippedView::new(mtm, NSSize::new(width, 10.0)), y: 20.0, details: Vec::new() }
    }

    /// The content width inside the margins.
    pub fn inner(&self) -> f64 {
        self.width - 2.0 * self.margin
    }

    /// Adds `view` at the current position (`x` from the left), advancing by its height.
    pub fn place(&mut self, view: &NSView, x: f64, height: f64) {
        let size = view.frame().size;
        view.setFrame(NSRect::new(NSPoint::new(x, self.y), NSSize::new(size.width, height)));
        self.view.addSubview(view);
        self.y += height;
    }

    /// Vertical space.
    pub fn space(&mut self, points: f64) {
        self.y += points;
    }

    /// A section title above a group.
    pub fn header(&mut self, text: &str) {
        let l = label(self.mtm, text, 13.0, true);
        l.sizeToFit();
        let h = l.frame().size.height;
        self.place(&l, self.margin + 2.0, h);
        self.y += 6.0;
    }

    /// Grey text under a group.
    pub fn footnote(&mut self, text: &str) {
        self.y += 6.0;
        let (l, h) = wrapping(self.mtm, text, self.inner() - 4.0, 11.0, &NSColor::secondaryLabelColor());
        self.place(&l, self.margin + 2.0, h);
    }

    /// A rounded group of `rows`, separated by hairlines.
    pub fn group(&mut self, rows: Vec<Row>) {
        let mtm = self.mtm;
        let width = self.inner();
        let content = FlippedView::new(mtm, NSSize::new(width, 10.0));
        let mut y = 0.0;
        let count = rows.len();
        for (n, row) in rows.into_iter().enumerate() {
            let h = self.row(&content, row, y, width);
            y += h;
            if n + 1 < count {
                let line = NSBox::new(mtm);
                line.setBoxType(NSBoxType::Separator);
                line.setFrame(NSRect::new(NSPoint::new(ROW_PAD_X, y), NSSize::new(width - 2.0 * ROW_PAD_X, 1.0)));
                content.addSubview(&line);
            }
        }
        content.setFrameSize(NSSize::new(width, y));
        let group = NSBox::new(mtm);
        group.setBoxType(NSBoxType::Custom);
        group.setTitlePosition(NSTitlePosition::NoTitle);
        group.setContentViewMargins(NSSize::new(0.0, 0.0));
        group.setCornerRadius(10.0);
        group.setBorderWidth(1.0);
        group.setBorderColor(&NSColor::quinaryLabelColor());
        group.setFillColor(&NSColor::quinarySystemFillColor());
        group.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, y)));
        group.setContentView(Some(&content));
        self.place(&group, self.margin, y);
        self.y += GAP;
    }

    /// Lays one row out in `content` at `top`; returns its height.
    fn row(&mut self, content: &FlippedView, row: Row, top: f64, width: f64) -> f64 {
        let mtm = self.mtm;
        let mut x_right = width - ROW_PAD_X;
        let mut trailing_h: f64 = 0.0;
        let mut trailing_frames = Vec::new();
        for control in row.trailing.iter().rev() {
            let size = control.frame().size;
            x_right -= size.width;
            trailing_frames.push((control.clone(), x_right, size));
            trailing_h = trailing_h.max(size.height);
            x_right -= GAP;
        }
        let mut x_left = ROW_PAD_X;
        let mut icon_size = NSSize::new(0.0, 0.0);
        if let Some(icon) = &row.icon {
            icon_size = icon.frame().size;
            x_left += icon_size.width + 8.0;
        }
        let text_w = (x_right - x_left).max(80.0);
        let title = label(mtm, &row.title, 13.0, false);
        title.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
        title.setPreferredMaxLayoutWidth(text_w);
        let title_h = height_of(&title, text_w);
        let detail = row.detail.as_deref().map(|d| {
            let color = row.detail_color.clone().unwrap_or_else(NSColor::secondaryLabelColor);
            wrapping(mtm, d, text_w, 11.0, &color)
        });
        let text_h = title_h + detail.as_ref().map_or(0.0, |(_, h)| 2.0 + h);
        let block = text_h.max(trailing_h).max(icon_size.height);
        let head = (block + 2.0 * ROW_PAD_Y).max(ROW_MIN);
        let mid = top + head / 2.0;
        let mut ty = mid - text_h / 2.0;
        title.setFrame(NSRect::new(NSPoint::new(x_left, ty), NSSize::new(text_w, title_h)));
        content.addSubview(&title);
        ty += title_h + 2.0;
        if let Some((d, h)) = detail {
            d.setFrame(NSRect::new(NSPoint::new(x_left, ty), NSSize::new(text_w, h)));
            content.addSubview(&d);
            self.details.push((row.title.clone(), d));
        }
        if let Some(icon) = &row.icon {
            icon.setFrameOrigin(NSPoint::new(ROW_PAD_X, mid - icon_size.height / 2.0));
            content.addSubview(icon);
        }
        for (control, x, size) in trailing_frames {
            control.setFrame(NSRect::new(NSPoint::new(x, (mid - size.height / 2.0).round()), size));
            content.addSubview(&control);
        }
        let mut height = head;
        if let Some((below, h)) = row.below {
            below.setFrame(NSRect::new(NSPoint::new(ROW_PAD_X, top + head - ROW_PAD_Y + 2.0), NSSize::new(width - 2.0 * ROW_PAD_X, h)));
            content.addSubview(&below);
            height += h;
        }
        height
    }

    /// The finished view (its height includes a bottom margin).
    pub fn finish(self) -> (Retained<FlippedView>, f64) {
        let h = self.y + 12.0;
        self.view.setFrameSize(NSSize::new(self.width, h));
        (self.view, h)
    }
}

/// Sets the whole app's appearance: `Some(true)` dark, `Some(false)` light, `None` the
/// system's (preview renders).
pub fn set_app_appearance(mtm: MainThreadMarker, dark: Option<bool>) {
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let appearance = dark.and_then(|d| {
        // SAFETY: AppKit's own appearance name constants.
        let name = unsafe { if d { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua } };
        NSAppearance::appearanceNamed(name)
    });
    app.setAppearance(appearance.as_deref());
}

/// Draws `window` (title bar included) into a PNG at `path`, in-process: the view hierarchy
/// renders itself into a bitmap (`cacheDisplayInRect`); the screen is never read.
pub fn render_png(window: &NSWindow, path: &Path) -> Result<(), String> {
    let content = window.contentView().ok_or("no content view")?;
    // The frame view (the content view's superview) holds the title bar too.
    // SAFETY: plain getter.
    let view: Retained<NSView> = unsafe { content.superview() }.unwrap_or(content);
    view.layoutSubtreeIfNeeded();
    view.displayIfNeeded();
    let bounds = view.bounds();
    let rep = view.bitmapImageRepForCachingDisplayInRect(bounds).ok_or("no bitmap")?;
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
    let props = NSDictionary::new();
    // SAFETY: an empty properties dictionary is valid for PNG.
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &props) }.ok_or("no PNG data")?;
    std::fs::write(path, data.to_vec()).map_err(|e| format!("{}: {e}", path.display()))
}

thread_local! {
    static LATER: RefCell<Vec<Box<dyn std::any::Any>>> = const { RefCell::new(Vec::new()) };
}

/// Keeps `thing` (a window, a target) alive until the current event is handled, then drops
/// it on the main thread: a control's target must outlive the action it is sending.
pub fn release_later(thing: impl std::any::Any) {
    LATER.with(|l| l.borrow_mut().push(Box::new(thing)));
    dispatch2::DispatchQueue::main().exec_async(|| {
        let gone = LATER.with(|l| std::mem::take(&mut *l.borrow_mut()));
        drop(gone);
    });
}

/// Centers text in a label.
pub fn centered(field: &NSTextField) {
    field.setAlignment(NSTextAlignment::Center);
}

/// Makes `window` the app's front window (an accessory app activates first).
pub fn bring_to_front(mtm: MainThreadMarker, window: &NSWindow) {
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.activate();
    window.makeKeyAndOrderFront(None);
    window.orderFrontRegardless();
}
