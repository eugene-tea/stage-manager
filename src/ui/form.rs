//! A small AppKit toolkit for the app's own windows (Settings, the welcome): a form of groups,
//! each a column of rows with a title, an optional grey explanation and a control at the
//! trailing edge. Two looks ([`Style`]): grouped, System Settings' rounded boxes with
//! hairlines between rows (the welcome), and plain, rows straight on the window background as
//! in the toolbar-tab settings windows of Safari, Finder, Mail and Xcode (Settings, whose tabs
//! already group things). Standard controls, system fonts and dynamic system colors only, so
//! light and dark both come from AppKit.
//!
//! Layout is by frames in a flipped view (top-down), computed once per build; where a row's
//! parts go is pure math ([`row_layout`]). Controls send their action to an [`ActionTarget`],
//! which hands the control's tag to a Rust closure. [`render_png`] draws a window into a PNG
//! in-process (`cacheDisplayInRect`), never from the screen, for the preview mode.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{ClassType, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSBitmapImageFileType,
    NSBox, NSBoxType, NSButton, NSColor, NSControl, NSControlSize, NSControlStateValueOff, NSControlStateValueOn, NSFont,
    NSFontWeightRegular, NSFontWeightSemibold, NSImage, NSImageSymbolConfiguration, NSImageView, NSLineBreakMode,
    NSAccessibility, NSPopUpButton, NSSwitch, NSTextAlignment, NSTextField, NSTitlePosition, NSView, NSWindow,
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
    // Decoration beside words that say the same: VoiceOver skips it.
    view.setAccessibilityElement(false);
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

/// How a form draws its groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    /// System Settings: each group a rounded box, its rows split by hairlines.
    #[default]
    Grouped,
    /// A toolbar-tab settings window (Safari, Finder, Xcode): rows straight on the window
    /// background, no box, no fill, no border, no hairlines; space between groups instead.
    Plain,
}

/// The spacing a [`Style`] lays rows out with, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Window edge to the group: the box (grouped) or the rows' text and controls (plain).
    pub margin: f64,
    /// Group edge to a row's content, on either side.
    pub pad_x: f64,
    /// Space above and below a row's title block.
    pub pad_y: f64,
    /// A row's least height (a one-line title with a switch).
    pub row_min: f64,
    /// Each group in a rounded box with a fill and a border.
    pub boxed: bool,
    /// Hairlines between rows.
    pub hairlines: bool,
    /// Space after a group.
    pub group_gap: f64,
    /// Section titles and footnotes, in from the margin.
    pub note_inset: f64,
    /// Space under the last thing; a group's own gap gives way to it.
    pub bottom: f64,
}

impl Style {
    /// Its spacing. Plain keeps every row's text and controls where grouped puts them (32
    /// points from the window's edges), so only the box goes.
    pub fn metrics(self) -> Metrics {
        match self {
            Style::Grouped => Metrics { margin: 20.0, pad_x: 12.0, pad_y: 9.0, row_min: 40.0, boxed: true, hairlines: true, group_gap: 8.0, note_inset: 2.0, bottom: 12.0 },
            Style::Plain => Metrics { margin: 32.0, pad_x: 0.0, pad_y: 9.0, row_min: 40.0, boxed: false, hairlines: false, group_gap: 14.0, note_inset: 0.0, bottom: 20.0 },
        }
    }
}

/// A frame in a flipped (top-down) view: x, y, width, height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Frame {
    fn rect(self) -> NSRect {
        NSRect::new(NSPoint::new(self.x, self.y), NSSize::new(self.w, self.h))
    }
}

/// The sizes of one row's parts, measured before layout.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RowSizes {
    /// The leading icon's size, if any.
    pub icon: Option<(f64, f64)>,
    /// The trailing controls' sizes, left to right.
    pub trailing: Vec<(f64, f64)>,
    /// The title's height at the text width.
    pub title_h: f64,
    /// The detail's height at the text width, if any.
    pub detail_h: Option<f64>,
    /// The full-width view under the title block, its height.
    pub below_h: Option<f64>,
}

/// Where one row's parts go, relative to its group's top-left.
#[derive(Debug, Clone, PartialEq)]
pub struct RowLayout {
    pub title: Frame,
    pub detail: Option<Frame>,
    pub icon: Option<Frame>,
    /// The trailing controls, left to right.
    pub trailing: Vec<Frame>,
    pub below: Option<Frame>,
    /// The row's height (the next row starts there).
    pub height: f64,
}

/// Space between trailing controls, and between the text and the first of them.
const GAP: f64 = 8.0;
/// Icon to text.
const ICON_GAP: f64 = 8.0;
/// The narrowest text column.
const TEXT_MIN: f64 = 80.0;

/// The text column of a row `width` wide (the group's) with these trailing controls and
/// icon: its left edge and width.
pub fn text_column(m: &Metrics, width: f64, trailing: &[(f64, f64)], icon: Option<(f64, f64)>) -> (f64, f64) {
    let controls: f64 = trailing.iter().map(|(w, _)| w + GAP).sum();
    let left = m.pad_x + icon.map_or(0.0, |(w, _)| w + ICON_GAP);
    (left, (width - m.pad_x - controls - left).max(TEXT_MIN))
}

/// Lays out one row `width` wide at `top`: the title block and the controls centred on the
/// row's middle, the below view under the title block.
pub fn row_layout(m: &Metrics, width: f64, top: f64, s: &RowSizes) -> RowLayout {
    let (x_left, text_w) = text_column(m, width, &s.trailing, s.icon);
    let text_h = s.title_h + s.detail_h.map_or(0.0, |h| 2.0 + h);
    let trailing_h = s.trailing.iter().fold(0.0_f64, |a, (_, h)| a.max(*h));
    let block = text_h.max(trailing_h).max(s.icon.map_or(0.0, |(_, h)| h));
    let head = (block + 2.0 * m.pad_y).max(m.row_min);
    let mid = top + head / 2.0;
    let title = Frame { x: x_left, y: mid - text_h / 2.0, w: text_w, h: s.title_h };
    let detail = s.detail_h.map(|h| Frame { x: x_left, y: title.y + s.title_h + 2.0, w: text_w, h });
    let icon = s.icon.map(|(w, h)| Frame { x: m.pad_x, y: mid - h / 2.0, w, h });
    let mut x = width - m.pad_x - s.trailing.iter().map(|(w, _)| w).sum::<f64>() - GAP * s.trailing.len().saturating_sub(1) as f64;
    let mut trailing = Vec::new();
    for (w, h) in &s.trailing {
        trailing.push(Frame { x, y: (mid - h / 2.0).round(), w: *w, h: *h });
        x += w + GAP;
    }
    let below = s.below_h.map(|h| Frame { x: m.pad_x, y: top + head - m.pad_y + 2.0, w: width - 2.0 * m.pad_x, h });
    RowLayout { title, detail, icon, trailing, below, height: head + s.below_h.unwrap_or(0.0) }
}

/// A form's height with its content ending at `y`, of which `trailing_gap` is a last group's
/// gap with nothing after it: the bottom margin takes that gap's place.
pub fn finished_height(y: f64, trailing_gap: f64, m: &Metrics) -> f64 {
    y - trailing_gap + m.bottom
}

/// Builds a form top-down.
pub struct Form {
    mtm: MainThreadMarker,
    /// The form's width.
    pub width: f64,
    /// Left and right margin around groups.
    pub margin: f64,
    metrics: Metrics,
    view: Retained<FlippedView>,
    y: f64,
    /// The gap the last group left, while nothing came after it.
    trailing_gap: f64,
    /// The detail labels made, by row title (for in-place updates).
    pub details: Vec<(String, Retained<NSTextField>)>,
}

impl Form {
    /// An empty grouped form `width` wide.
    pub fn new(mtm: MainThreadMarker, width: f64) -> Self {
        Self::with_style(mtm, width, Style::Grouped)
    }

    /// An empty form `width` wide in `style`.
    pub fn with_style(mtm: MainThreadMarker, width: f64, style: Style) -> Self {
        let metrics = style.metrics();
        Self { mtm, width, margin: metrics.margin, metrics, view: FlippedView::new(mtm, NSSize::new(width, 10.0)), y: 20.0, trailing_gap: 0.0, details: Vec::new() }
    }

    /// The content width inside the margins.
    pub fn inner(&self) -> f64 {
        self.width - 2.0 * self.margin
    }

    /// The width of a row's content (a below view's).
    pub fn row_inner(&self) -> f64 {
        self.inner() - 2.0 * self.metrics.pad_x
    }

    /// Adds `view` at the current position (`x` from the left), advancing by its height.
    pub fn place(&mut self, view: &NSView, x: f64, height: f64) {
        let size = view.frame().size;
        view.setFrame(NSRect::new(NSPoint::new(x, self.y), NSSize::new(size.width, height)));
        self.view.addSubview(view);
        self.y += height;
        self.trailing_gap = 0.0;
    }

    /// Vertical space.
    pub fn space(&mut self, points: f64) {
        self.y += points;
        self.trailing_gap = 0.0;
    }

    /// A section title above a group.
    pub fn header(&mut self, text: &str) {
        let l = label(self.mtm, text, 13.0, true);
        l.sizeToFit();
        let h = l.frame().size.height;
        self.place(&l, self.margin + self.metrics.note_inset, h);
        self.y += 6.0;
    }

    /// Grey text under a group.
    pub fn footnote(&mut self, text: &str) {
        self.y += 6.0;
        let inset = self.metrics.note_inset;
        let (l, h) = wrapping(self.mtm, text, self.inner() - 2.0 * inset, 11.0, &NSColor::secondaryLabelColor());
        self.place(&l, self.margin + inset, h);
    }

    /// A group of `rows`: a rounded box with hairlines between them (grouped), or the rows
    /// alone (plain).
    pub fn group(&mut self, rows: Vec<Row>) {
        let mtm = self.mtm;
        let m = self.metrics;
        let width = self.inner();
        let content = FlippedView::new(mtm, NSSize::new(width, 10.0));
        let mut y = 0.0;
        let count = rows.len();
        for (n, row) in rows.into_iter().enumerate() {
            let h = self.row(&content, row, y, width);
            y += h;
            if m.hairlines && n + 1 < count {
                let line = NSBox::new(mtm);
                line.setBoxType(NSBoxType::Separator);
                line.setFrame(NSRect::new(NSPoint::new(m.pad_x, y), NSSize::new(width - 2.0 * m.pad_x, 1.0)));
                content.addSubview(&line);
            }
        }
        content.setFrameSize(NSSize::new(width, y));
        if m.boxed {
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
        } else {
            self.place(&content, self.margin, y);
        }
        self.y += m.group_gap;
        self.trailing_gap = m.group_gap;
    }

    /// Lays one row out in `content` at `top`; returns its height.
    fn row(&mut self, content: &FlippedView, row: Row, top: f64, width: f64) -> f64 {
        let mtm = self.mtm;
        let m = self.metrics;
        let size_of = |v: &NSView| {
            let s = v.frame().size;
            (s.width, s.height)
        };
        let trailing: Vec<(f64, f64)> = row.trailing.iter().map(|c| size_of(c)).collect();
        let icon = row.icon.as_deref().map(size_of);
        let (_, text_w) = text_column(&m, width, &trailing, icon);
        let title = label(mtm, &row.title, 13.0, false);
        title.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
        title.setPreferredMaxLayoutWidth(text_w);
        let detail = row.detail.as_deref().map(|d| {
            let color = row.detail_color.clone().unwrap_or_else(NSColor::secondaryLabelColor);
            wrapping(mtm, d, text_w, 11.0, &color)
        });
        let sizes = RowSizes {
            icon,
            trailing,
            title_h: height_of(&title, text_w),
            detail_h: detail.as_ref().map(|(_, h)| *h),
            below_h: row.below.as_ref().map(|(_, h)| *h),
        };
        let at = row_layout(&m, width, top, &sizes);
        title.setFrame(at.title.rect());
        content.addSubview(&title);
        if let (Some((d, _)), Some(frame)) = (detail, at.detail) {
            d.setFrame(frame.rect());
            content.addSubview(&d);
            self.details.push((row.title.clone(), d));
        }
        if let (Some(icon), Some(frame)) = (&row.icon, at.icon) {
            icon.setFrameOrigin(NSPoint::new(frame.x, frame.y));
            content.addSubview(icon);
        }
        for (control, frame) in row.trailing.iter().zip(&at.trailing) {
            control.setFrame(frame.rect());
            content.addSubview(control);
            describe_control(control, &row.title, row.detail.as_deref());
        }
        if let (Some((below, _)), Some(frame)) = (row.below, at.below) {
            below.setFrame(frame.rect());
            content.addSubview(&below);
        }
        at.height
    }

    /// The finished view (its height includes a bottom margin).
    pub fn finish(self) -> (Retained<FlippedView>, f64) {
        let h = finished_height(self.y, self.trailing_gap, &self.metrics);
        self.view.setFrameSize(NSSize::new(self.width, h));
        (self.view, h)
    }
}

/// What VoiceOver reads for a row's control: a switch or a pop-up has no words of its own,
/// so it takes the row's title (System Settings' rows read so); the row's grey explanation is
/// every control's help. A button keeps its own title.
pub fn describe_control(control: &NSView, title: &str, detail: Option<&str>) {
    if control.isKindOfClass(NSSwitch::class()) || control.isKindOfClass(NSPopUpButton::class()) {
        control.setAccessibilityLabel(Some(&NSString::from_str(title)));
    }
    if let Some(d) = detail {
        control.setAccessibilityHelp(Some(&NSString::from_str(d)));
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

/// Makes `window` the app's front window (an accessory app activates first), on the Desktop
/// shown.
pub fn bring_to_front(mtm: MainThreadMarker, window: &NSWindow) {
    window.setCollectionBehavior(super::panels::app_window());
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.activate();
    window.makeKeyAndOrderFront(None);
    window.orderFrontRegardless();
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROUPED: Metrics = Metrics { margin: 20.0, pad_x: 12.0, pad_y: 9.0, row_min: 40.0, boxed: true, hairlines: true, group_gap: 8.0, note_inset: 2.0, bottom: 12.0 };

    #[test]
    fn plain_drops_the_box_and_the_hairlines_but_keeps_the_rows_where_they_were() {
        let (grouped, plain) = (Style::Grouped.metrics(), Style::Plain.metrics());
        assert_eq!(grouped, GROUPED, "the welcome keeps its look");
        assert!(!plain.boxed && !plain.hairlines);
        assert_eq!(plain.margin + plain.pad_x, grouped.margin + grouped.pad_x, "the text column starts where it did");
        assert_eq!((plain.pad_y, plain.row_min), (grouped.pad_y, grouped.row_min), "rows as tall as before");
        assert_eq!(Style::default(), Style::Grouped);
    }

    #[test]
    fn the_text_column_ends_a_gap_before_the_trailing_controls() {
        let m = Style::Plain.metrics();
        assert_eq!(text_column(&m, 536.0, &[], None), (0.0, 536.0), "no control: the whole width");
        assert_eq!(text_column(&m, 536.0, &[(40.0, 20.0)], None), (0.0, 536.0 - 40.0 - GAP));
        assert_eq!(text_column(&GROUPED, 560.0, &[(40.0, 20.0)], Some((28.0, 28.0))), (12.0 + 28.0 + ICON_GAP, 560.0 - 12.0 - 48.0 - 48.0));
        assert_eq!(text_column(&m, 200.0, &[(300.0, 20.0)], None).1, TEXT_MIN, "never narrower than the least");
    }

    #[test]
    fn a_row_centres_its_text_and_controls_on_its_middle() {
        let m = Style::Plain.metrics();
        let one_line = RowSizes { trailing: vec![(40.0, 20.0)], title_h: 16.0, ..Default::default() };
        let at = row_layout(&m, 536.0, 100.0, &one_line);
        assert_eq!(at.height, m.row_min, "a one-line row is the least height");
        assert_eq!(at.title, Frame { x: 0.0, y: 112.0, w: 488.0, h: 16.0 });
        assert_eq!(at.trailing, [Frame { x: 496.0, y: 110.0, w: 40.0, h: 20.0 }], "flush with the trailing edge");

        let two = RowSizes { trailing: vec![(100.0, 24.0), (120.0, 24.0)], title_h: 16.0, detail_h: Some(28.0), ..Default::default() };
        let at = row_layout(&m, 536.0, 0.0, &two);
        assert_eq!(at.height, 16.0 + 2.0 + 28.0 + 2.0 * m.pad_y, "taller than the least: text plus padding");
        assert_eq!(at.detail, Some(Frame { x: 0.0, y: m.pad_y + 18.0, w: 536.0 - 236.0, h: 28.0 }));
        assert_eq!(at.trailing[0].x + 100.0 + GAP, at.trailing[1].x, "left to right, a gap apart");
        assert_eq!(at.trailing[1].x + 120.0, 536.0);
    }

    #[test]
    fn a_below_view_spans_the_row_under_its_title() {
        let m = Style::Plain.metrics();
        let s = RowSizes { title_h: 16.0, below_h: Some(40.0), ..Default::default() };
        let at = row_layout(&m, 536.0, 0.0, &s);
        assert_eq!(at.below, Some(Frame { x: 0.0, y: 40.0 - m.pad_y + 2.0, w: 536.0, h: 40.0 }));
        assert_eq!(at.height, 80.0);
        let at = row_layout(&GROUPED, 560.0, 0.0, &s);
        assert_eq!(at.below.map(|f| (f.x, f.w)), Some((12.0, 536.0)), "inside the box's padding");
    }

    #[test]
    fn the_bottom_margin_takes_a_last_groups_gap() {
        let plain = Style::Plain.metrics();
        assert_eq!(finished_height(300.0, plain.group_gap, &plain), 300.0 - plain.group_gap + plain.bottom);
        assert_eq!(finished_height(300.0, 0.0, &plain), 320.0, "after a footnote");
        assert_eq!(finished_height(300.0, 0.0, &GROUPED), 312.0, "the welcome as before");
    }
}
