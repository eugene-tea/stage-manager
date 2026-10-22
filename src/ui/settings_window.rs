//! The Settings window, in the shape of a Mac app's Settings (Safari, Mail, Xcode): a toolbar
//! of panes (`NSWindowToolbarStyle::Preference`, an SF Symbol and a label each), one pane
//! shown at a time, the window titled after it. Each pane is a grouped form in the look of
//! System Settings ([`form`]).
//!
//! - **General:** launch at login, the menu bar icon, automatic update checks, installing
//!   updates automatically, the version with Check for Updates….
//! - **Stage Manager** (Apple's own Customize sheet): whether Apple's Stage Manager is on
//!   (with Turn Off), "Show recent apps", "Show windows from an application", and "Click
//!   wallpaper to reveal desktop" (read only; its button opens Desktop & Dock).
//! - **Stages:** stages back after a restart, new apps in the current stage, Shift brings apps
//!   here, merge by dropping a card on a card.
//! - **Windows:** keeping windows clear of the strip, animation speed, keyboard shortcuts.
//! - **Advanced:** the config file (open, reveal) and the note that an AI agent is the
//!   easiest way to change anything else, with Copy Instructions for Agents.
//!
//! The toolbar is built by hand (an `NSToolbar` with a delegate, the panes swapped as the
//! window's content view) rather than with `NSTabViewController`: every pane is built once and
//! kept, so [`apply`] refreshes the hidden ones too; the height animates to each pane with the
//! top-left edge kept, the width fixed; and no view controller subclass is needed from Rust.
//! A pane taller than the screen allows scrolls; the others have no scroll view.
//!
//! The window shows [`SettingsValues`] and reports each user change as a [`Change`] to the
//! closure it was opened with; the caller writes it (config file, macOS setting, login item,
//! the pane shown last in the app's own state) and calls [`apply`] with fresh values. Changes
//! made elsewhere (an agent editing the file) arrive the same way. Main thread only.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSControlStateValueOn, NSImage, NSPopUpButton, NSScreen, NSScrollView,
    NSSlider, NSSwitch, NSTextAlignment, NSTickMarkPosition, NSToolbar, NSToolbarDelegate, NSToolbarDisplayMode,
    NSToolbarItem, NSView, NSWindow, NSWindowStyleMask, NSWindowToolbarStyle,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSPoint, NSRect, NSSize, NSString, NSTimer};

use super::form::{self, ActionTarget, FlippedView, Form, Row};
use crate::config::{AppConfig, Config, FitMode};
use crate::platform::login_item::LoginStatus;
use crate::platform::native_settings::RawSettings;

/// The window's width (every pane's).
const WIDTH: f64 = 600.0;

/// The speeds the slider stops at, slowest first; the middle one is Apple's (1).
pub const SPEEDS: [f64; 9] = [0.25, 0.33, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];

/// The slider stop nearest to `speed` (by ratio).
pub fn speed_stop(speed: f64) -> usize {
    let s = speed.clamp(SPEEDS[0], SPEEDS[SPEEDS.len() - 1]);
    let mut best = 0;
    for (i, v) in SPEEDS.iter().enumerate() {
        if (s / v).ln().abs() < (s / SPEEDS[best]).ln().abs() {
            best = i;
        }
    }
    best
}

/// One pane of the window, in toolbar order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Pane {
    #[default]
    General,
    StageManager,
    Stages,
    Windows,
    Advanced,
}

impl Pane {
    /// Every pane, in toolbar order.
    pub const ALL: [Pane; 5] = [Pane::General, Pane::StageManager, Pane::Stages, Pane::Windows, Pane::Advanced];

    /// Its toolbar item identifier, also how the app's state remembers it.
    pub fn id(self) -> &'static str {
        match self {
            Pane::General => "general",
            Pane::StageManager => "stage-manager",
            Pane::Stages => "stages",
            Pane::Windows => "windows",
            Pane::Advanced => "advanced",
        }
    }

    /// The pane with identifier `id`.
    pub fn from_id(id: &str) -> Option<Pane> {
        Pane::ALL.into_iter().find(|p| p.id() == id)
    }

    /// Its toolbar label, also the window's title while it shows.
    pub fn title(self) -> &'static str {
        match self {
            Pane::General => "General",
            Pane::StageManager => "Stage Manager",
            Pane::Stages => "Stages",
            Pane::Windows => "Windows",
            Pane::Advanced => "Advanced",
        }
    }

    /// Its toolbar symbol (SF Symbols, all on macOS 14).
    pub fn symbol(self) -> &'static str {
        match self {
            Pane::General => "gearshape",
            Pane::StageManager => "rectangle.3.group",
            Pane::Stages => "square.stack.3d.up",
            Pane::Windows => "macwindow",
            Pane::Advanced => "gearshape.2",
        }
    }

    /// The tags of the controls it holds, top to bottom (each control on exactly one pane).
    pub fn tags(self) -> &'static [isize] {
        use tag::*;
        match self {
            Pane::General => &[LOGIN, LOGIN_ITEMS, ICON, UPDATES, INSTALL, CHECK],
            Pane::StageManager => &[TURN_OFF, RECENT, GROUPING, DESKTOP_DOCK],
            Pane::Stages => &[RESTORE, NEW_APPS, SHIFT, MERGE],
            Pane::Windows => &[FIT, SPEED, HOTKEYS],
            Pane::Advanced => &[REVEAL, OPEN, COPY],
        }
    }

    /// The pane's toolbar item tag.
    fn item_tag(self) -> isize {
        tag::PANE + Pane::ALL.iter().position(|p| *p == self).unwrap_or(0) as isize
    }

    /// The pane a toolbar item's tag names.
    fn of_item_tag(t: isize) -> Option<Pane> {
        usize::try_from(t - tag::PANE).ok().and_then(|i| Pane::ALL.get(i).copied())
    }
}

/// What the window shows.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsValues {
    /// "1.0.0 (123)".
    pub version: String,
    pub login: LoginStatus,
    pub app: AppConfig,
    /// Apple's Stage Manager is on (we stand down).
    pub apple_on: bool,
    /// Apple's "Show recent apps in Stage Manager" (`AutoHide = 0`).
    pub show_recent_apps: bool,
    /// Apple's "Show windows from an application: All at Once" (`AppWindowGroupingBehavior = 1`).
    pub all_at_once: bool,
    /// Apple's "Click wallpaper to reveal desktop: Only in Stage Manager".
    pub wallpaper_only_in_stage_manager: bool,
    /// `[stages] restore_after_restart`.
    pub restore_after_restart: bool,
    /// `[stages] new_apps_join_current`.
    pub new_apps_join_current: bool,
    pub merge_on_drop: bool,
    pub shift_brings_here: bool,
    pub fit: FitMode,
    pub hotkeys: bool,
    /// `[animation] speed` (1 = Apple's).
    pub speed: f64,
    /// Anything animates (`enabled` and a speed above 0).
    pub animations_on: bool,
    /// The config file, `~` for the home folder.
    pub config_path: String,
    /// The config file's error, if it has one.
    pub config_error: Option<String>,
}

impl SettingsValues {
    /// The values from the config in force, Apple's settings, the login item and the file.
    pub fn new(config: &Config, raw: &RawSettings, login: LoginStatus, config_path: String, config_error: Option<String>, version: String) -> Self {
        let flag = |k: &str, default: i64| raw.get(k).copied().unwrap_or(default);
        Self {
            version,
            login,
            app: config.app,
            apple_on: flag("GloballyEnabled", 0) != 0,
            show_recent_apps: flag("AutoHide", 0) == 0,
            all_at_once: flag("AppWindowGroupingBehavior", 1) != 0,
            wallpaper_only_in_stage_manager: flag("EnableStandardClickToShowDesktop", 1) == 0,
            restore_after_restart: config.stages.restore_after_restart,
            new_apps_join_current: config.stages.new_apps_join_current,
            merge_on_drop: config.style.merge_on_drop,
            shift_brings_here: config.sugar.shift_brings_here,
            fit: config.windows.fit,
            hotkeys: config.hotkeys.enabled,
            speed: config.style.animation.speed,
            animations_on: config.style.animation.animates(),
            config_path,
            config_error,
        }
    }

    /// What decides the panes' rows (a change rebuilds them; anything else updates in place).
    fn shape(&self) -> (u8, bool, bool, bool, bool) {
        let login = match self.login {
            LoginStatus::On => 0,
            LoginStatus::NeedsApproval => 1,
            LoginStatus::Off => 2,
            LoginStatus::Unavailable(_) => 3,
        };
        // The Shift row says what Shift does, which the new-apps switch flips.
        (login, self.apple_on, self.config_error.is_some(), self.animations_on, self.new_apps_join_current)
    }
}

/// A change the user made.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    LaunchAtLogin(bool),
    OpenLoginItems,
    MenuBarIcon(bool),
    AutoUpdates(bool),
    InstallUpdates(bool),
    CheckNow,
    TurnOffApple,
    ShowRecentApps(bool),
    AllAtOnce(bool),
    OpenDesktopAndDock,
    RestoreAfterRestart(bool),
    NewAppsJoinCurrent(bool),
    MergeOnDrop(bool),
    ShiftBringsHere(bool),
    Fit(FitMode),
    Hotkeys(bool),
    Speed(f64),
    OpenConfig,
    RevealConfig,
    CopyAgentInstructions,
    /// Another pane was chosen (remembered for the next opening).
    Pane(Pane),
}

mod tag {
    pub const LOGIN: isize = 1;
    pub const LOGIN_ITEMS: isize = 2;
    pub const ICON: isize = 3;
    pub const UPDATES: isize = 4;
    pub const CHECK: isize = 5;
    pub const TURN_OFF: isize = 6;
    pub const RECENT: isize = 7;
    pub const GROUPING: isize = 8;
    pub const DESKTOP_DOCK: isize = 9;
    pub const MERGE: isize = 10;
    pub const SHIFT: isize = 11;
    pub const FIT: isize = 12;
    pub const HOTKEYS: isize = 13;
    pub const SPEED: isize = 14;
    pub const OPEN: isize = 15;
    pub const REVEAL: isize = 16;
    pub const COPY: isize = 17;
    pub const INSTALL: isize = 18;
    pub const RESTORE: isize = 19;
    pub const NEW_APPS: isize = 20;
    /// The toolbar items: `PANE + n` for the nth pane.
    pub const PANE: isize = 100;
    /// Every control's tag.
    #[cfg(test)]
    pub const ALL: [isize; 20] = [
        LOGIN, LOGIN_ITEMS, ICON, UPDATES, CHECK, TURN_OFF, RECENT, GROUPING, DESKTOP_DOCK, MERGE, SHIFT, FIT, HOTKEYS, SPEED,
        OPEN, REVEAL, COPY, INSTALL, RESTORE, NEW_APPS,
    ];
}

/// The tag of the control (or toolbar item) that reports `change`: every change has one, so
/// every change is reachable from a pane (tested; a new change without one does not compile).
#[cfg(test)]
fn tag_of(change: &Change) -> isize {
    match change {
        Change::LaunchAtLogin(_) => tag::LOGIN,
        Change::OpenLoginItems => tag::LOGIN_ITEMS,
        Change::MenuBarIcon(_) => tag::ICON,
        Change::AutoUpdates(_) => tag::UPDATES,
        Change::InstallUpdates(_) => tag::INSTALL,
        Change::CheckNow => tag::CHECK,
        Change::TurnOffApple => tag::TURN_OFF,
        Change::ShowRecentApps(_) => tag::RECENT,
        Change::AllAtOnce(_) => tag::GROUPING,
        Change::OpenDesktopAndDock => tag::DESKTOP_DOCK,
        Change::RestoreAfterRestart(_) => tag::RESTORE,
        Change::NewAppsJoinCurrent(_) => tag::NEW_APPS,
        Change::MergeOnDrop(_) => tag::MERGE,
        Change::ShiftBringsHere(_) => tag::SHIFT,
        Change::Fit(_) => tag::FIT,
        Change::Hotkeys(_) => tag::HOTKEYS,
        Change::Speed(_) => tag::SPEED,
        Change::OpenConfig => tag::OPEN,
        Change::RevealConfig => tag::REVEAL,
        Change::CopyAgentInstructions => tag::COPY,
        Change::Pane(p) => p.item_tag(),
    }
}

const FIT_TITLES: [&str; 3] = ["Off", "Move Windows", "Move and Narrow"];
const FIT_MODES: [FitMode; 3] = [FitMode::Off, FitMode::Move, FitMode::Narrow];

/// The footnote of the panes of extras (not in Apple's Stage Manager).
const EXTRAS_NOTE: &str = "Not in Apple's Stage Manager. Each stays out of the way until you use it.";

/// The controls whose state is read and updated, on every pane.
struct Controls {
    login: Retained<NSSwitch>,
    icon: Retained<NSSwitch>,
    updates: Retained<NSSwitch>,
    install: Retained<NSSwitch>,
    recent: Retained<NSSwitch>,
    grouping: Retained<NSPopUpButton>,
    restore: Retained<NSSwitch>,
    new_apps: Retained<NSSwitch>,
    merge: Retained<NSSwitch>,
    shift: Retained<NSSwitch>,
    fit: Retained<NSPopUpButton>,
    hotkeys: Retained<NSSwitch>,
    speed: Retained<NSSlider>,
    copy: Retained<NSButton>,
}

/// One built pane: the view set as the window's content (the form, or a scroll view around
/// one taller than the screen allows) and its content height.
struct PaneView {
    pane: Pane,
    view: Retained<NSView>,
    height: f64,
}

/// The open window.
struct Open {
    window: Retained<NSWindow>,
    toolbar: Retained<NSToolbar>,
    _delegate: Retained<ToolbarDelegate>,
    target: Retained<ActionTarget>,
    panes: Vec<PaneView>,
    /// The pane shown.
    pane: Pane,
    controls: Controls,
    shown: SettingsValues,
}

/// Receives the user's changes.
type ChangeFn = Rc<dyn Fn(Change)>;

thread_local! {
    static OPEN: RefCell<Option<Open>> = const { RefCell::new(None) };
    static HANDLER: RefCell<Option<ChangeFn>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the ivar is plain Rust data and the
    // class does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "SMSettingsToolbarDelegate"]
    #[ivars = Retained<ActionTarget>]
    struct ToolbarDelegate;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for ToolbarDelegate {}

    // SAFETY: the methods below match NSToolbarDelegate's signatures.
    unsafe impl NSToolbarDelegate for ToolbarDelegate {
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn item_for(&self, _toolbar: &NSToolbar, id: &NSString, _inserted: bool) -> Option<Retained<NSToolbarItem>> {
            Pane::from_id(&id.to_string()).map(|pane| toolbar_item(self.mtm(), pane, self.ivars()))
        }

        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn default_ids(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            pane_ids()
        }

        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn allowed_ids(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            pane_ids()
        }

        #[unsafe(method_id(toolbarSelectableItemIdentifiers:))]
        fn selectable_ids(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            pane_ids()
        }
    }
);

impl ToolbarDelegate {
    fn new(mtm: MainThreadMarker, target: Retained<ActionTarget>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(target);
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// Every pane's toolbar identifier, in order.
fn pane_ids() -> Retained<NSArray<NSString>> {
    let ids: Vec<Retained<NSString>> = Pane::ALL.iter().map(|p| NSString::from_str(p.id())).collect();
    NSArray::from_retained_slice(&ids)
}

/// A pane's toolbar item: its symbol and label; a click goes to `target` with its tag.
fn toolbar_item(mtm: MainThreadMarker, pane: Pane, target: &ActionTarget) -> Retained<NSToolbarItem> {
    let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), &NSString::from_str(pane.id()));
    let label = NSString::from_str(pane.title());
    item.setLabel(&label);
    item.setPaletteLabel(&label);
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(pane.symbol()), Some(&label));
    if image.is_none() {
        tracing::debug!(symbol = pane.symbol(), "settings: no such symbol");
    }
    item.setImage(image.as_deref());
    item.setTag(pane.item_tag());
    // SAFETY: the target outlives the toolbar (the open window keeps both); `act:` is its action.
    unsafe {
        item.setTarget(Some(target));
        item.setAction(Some(ActionTarget::action()));
    }
    item
}

fn on(s: &NSSwitch) -> bool {
    s.state() == NSControlStateValueOn
}

fn set_switch(s: &NSSwitch, value: bool) {
    s.setState(if value { NSControlStateValueOn } else { objc2_app_kit::NSControlStateValueOff });
}

/// The change a control with `tag` reports, read from its state.
fn change_for(c: &Controls, t: isize) -> Option<Change> {
    Some(match t {
        tag::LOGIN => Change::LaunchAtLogin(on(&c.login)),
        tag::LOGIN_ITEMS => Change::OpenLoginItems,
        tag::ICON => Change::MenuBarIcon(on(&c.icon)),
        tag::UPDATES => Change::AutoUpdates(on(&c.updates)),
        tag::INSTALL => Change::InstallUpdates(on(&c.install)),
        tag::CHECK => Change::CheckNow,
        tag::TURN_OFF => Change::TurnOffApple,
        tag::RECENT => Change::ShowRecentApps(on(&c.recent)),
        tag::GROUPING => Change::AllAtOnce(c.grouping.indexOfSelectedItem() == 0),
        tag::DESKTOP_DOCK => Change::OpenDesktopAndDock,
        tag::RESTORE => Change::RestoreAfterRestart(on(&c.restore)),
        tag::NEW_APPS => Change::NewAppsJoinCurrent(on(&c.new_apps)),
        tag::MERGE => Change::MergeOnDrop(on(&c.merge)),
        tag::SHIFT => Change::ShiftBringsHere(on(&c.shift)),
        tag::FIT => Change::Fit(FIT_MODES[(c.fit.indexOfSelectedItem().max(0) as usize).min(2)]),
        tag::HOTKEYS => Change::Hotkeys(on(&c.hotkeys)),
        tag::SPEED => Change::Speed(SPEEDS[(c.speed.doubleValue().round().max(0.0) as usize).min(SPEEDS.len() - 1)]),
        tag::OPEN => Change::OpenConfig,
        tag::REVEAL => Change::RevealConfig,
        tag::COPY => Change::CopyAgentInstructions,
        _ => return None,
    })
}

fn clicked(t: isize) {
    let change = match Pane::of_item_tag(t) {
        Some(pane) => {
            // A toolbar item: show its pane (after the click, outside every borrow).
            dispatch2::DispatchQueue::main().exec_async(move || select(pane, true));
            Some(Change::Pane(pane))
        }
        None => OPEN.with(|o| o.borrow().as_ref().and_then(|o| change_for(&o.controls, t))),
    };
    let handler = HANDLER.with(|h| h.borrow().clone());
    if let (Some(change), Some(handler)) = (change, handler) {
        tracing::debug!(?change, "settings: changed");
        handler(change);
    }
}

/// The login switch: the login item's state, or the config's wish where this copy cannot
/// register (shown disabled).
fn login_on(v: &SettingsValues) -> bool {
    match v.login {
        LoginStatus::Unavailable(_) => v.app.launch_at_login,
        other => other.shows_on(),
    }
}

/// Any view as a plain `NSView`.
fn view<T: AsRef<NSView>>(v: &T) -> Retained<NSView> {
    v.as_ref().retain()
}

/// The speed slider with Slower · Native · Faster under it.
fn speed_view(mtm: MainThreadMarker, width: f64, stop: usize, enabled: bool, target: &ActionTarget) -> (Retained<NSView>, Retained<NSSlider>, f64) {
    let view = FlippedView::new(mtm, NSSize::new(width, 40.0));
    let slider = NSSlider::new(mtm);
    slider.setMinValue(0.0);
    slider.setMaxValue((SPEEDS.len() - 1) as f64);
    slider.setNumberOfTickMarks(SPEEDS.len() as isize);
    slider.setAllowsTickMarkValuesOnly(true);
    slider.setTickMarkPosition(NSTickMarkPosition::Below);
    slider.setContinuous(false);
    slider.setDoubleValue(stop as f64);
    slider.setEnabled(enabled);
    form::wire(&slider, target, tag::SPEED);
    slider.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 24.0)));
    view.addSubview(&slider);
    let grey = NSColor::secondaryLabelColor();
    for (text, align, x) in [
        ("Slower", NSTextAlignment::Left, 0.0),
        ("Native", NSTextAlignment::Center, width / 2.0 - 60.0),
        ("Faster", NSTextAlignment::Right, width - 120.0),
    ] {
        let l = form::label(mtm, text, 11.0, false);
        l.setTextColor(Some(&grey));
        l.setAlignment(align);
        l.setFrame(NSRect::new(NSPoint::new(x, 24.0), NSSize::new(120.0, 14.0)));
        view.addSubview(&l);
    }
    (view.into_super(), slider, 40.0)
}

/// Makes the controls of one pane: a switch, a button or a pop-up wired to the window's
/// target, each checked (in debug builds) against the pane's [`Pane::tags`].
struct Maker<'a> {
    mtm: MainThreadMarker,
    target: &'a ActionTarget,
    pane: Pane,
}

impl Maker<'_> {
    fn ours(&self, t: isize) {
        debug_assert!(self.pane.tags().contains(&t), "tag {t} is not on the {:?} pane", self.pane);
    }

    fn switch(&self, value: bool, t: isize) -> Retained<NSSwitch> {
        self.ours(t);
        form::switch(self.mtm, value, self.target, t)
    }

    fn button(&self, title: &str, t: isize) -> Retained<NSButton> {
        self.ours(t);
        form::button(self.mtm, title, self.target, t)
    }

    fn popup(&self, items: &[&str], selected: usize, t: isize) -> Retained<NSPopUpButton> {
        self.ours(t);
        form::popup(self.mtm, items, selected, self.target, t)
    }
}

/// The General pane.
fn general(f: &mut Form, m: &Maker, v: &SettingsValues) -> (Retained<NSSwitch>, Retained<NSSwitch>, Retained<NSSwitch>, Retained<NSSwitch>) {
    let login = m.switch(login_on(v), tag::LOGIN);
    let mut login_row = Row::new("Launch at login", None);
    match v.login {
        LoginStatus::NeedsApproval => {
            login_row = login_row.detail("Allow Stage Manager in System Settings → General → Login Items.");
            login_row.trailing.push(view(&m.button("Login Items…", tag::LOGIN_ITEMS)));
        }
        LoginStatus::Unavailable(_) => {
            login.setEnabled(false);
            login_row = login_row.detail("Available when Stage Manager runs from Applications.");
        }
        LoginStatus::On | LoginStatus::Off => {}
    }
    login_row.trailing.push(view(&login));
    let icon = m.switch(v.app.show_menu_bar_icon, tag::ICON);
    let updates = m.switch(v.app.check_for_updates, tag::UPDATES);
    let install = m.switch(v.app.install_updates, tag::INSTALL);
    install.setEnabled(v.app.check_for_updates);
    f.group(vec![
        login_row,
        Row::new("Show menu bar icon", Some(view(&icon)))
            .detail("To open Settings again, open Stage Manager from Applications."),
        Row::new("Check for updates automatically", Some(view(&updates)))
            .detail("Once a day, from GitHub. Nothing about you or this Mac is sent."),
        Row::new("Install updates automatically", Some(view(&install)))
            .detail("While you're away from the Mac; otherwise Stage Manager asks first. Your windows come back before it relaunches."),
        Row::new(&format!("Stage Manager {}", v.version), Some(view(&m.button("Check for Updates…", tag::CHECK)))),
    ]);
    (login, icon, updates, install)
}

/// The Stage Manager pane: Apple's Customize sheet.
fn stage_manager(f: &mut Form, m: &Maker, v: &SettingsValues) -> (Retained<NSSwitch>, Retained<NSPopUpButton>) {
    let mut apple = Row::new("Apple's Stage Manager", None);
    if v.apple_on {
        apple = apple.detail("On. This app stands down until Apple's is off: it arranges nothing and shows no strip.");
        apple.trailing.push(view(&m.button("Turn Off", tag::TURN_OFF)));
    } else {
        apple = apple.detail("Off. This app takes its place.");
    }
    let recent = m.switch(v.show_recent_apps, tag::RECENT);
    let grouping = m.popup(&["All at Once", "One at a Time"], if v.all_at_once { 0 } else { 1 }, tag::GROUPING);
    let wallpaper = if v.wallpaper_only_in_stage_manager { "Only in Stage Manager" } else { "Always" };
    f.group(vec![
        apple,
        Row::new("Show recent apps in Stage Manager", Some(view(&recent))),
        Row::new("Show windows from an application", Some(view(&grouping))),
        Row::new("Click wallpaper to reveal desktop", Some(view(&m.button("Desktop & Dock Settings…", tag::DESKTOP_DOCK))))
            .detail(format!("{wallpaper}. Set in System Settings → Desktop & Dock.")),
    ]);
    f.footnote("These are macOS's own Stage Manager settings, shared with System Settings → Desktop & Dock.");
    (recent, grouping)
}

/// The Stages pane: where windows and apps go.
fn stages(f: &mut Form, m: &Maker, v: &SettingsValues) -> [Retained<NSSwitch>; 4] {
    let restore = m.switch(v.restore_after_restart, tag::RESTORE);
    let new_apps = m.switch(v.new_apps_join_current, tag::NEW_APPS);
    let shift = m.switch(v.shift_brings_here, tag::SHIFT);
    let merge = m.switch(v.merge_on_drop, tag::MERGE);
    f.group(vec![
        Row::new("Bring stages back after a restart", Some(view(&restore)))
            .detail("Windows go back to their stages as their apps reopen them. Off is Apple's: each app starts in a stage of its own. Applies at the next launch."),
        Row::new("Open new apps in the current stage", Some(view(&new_apps)))
            .detail("Off is Apple's: a newly opened app gets a stage of its own."),
        Row::new("Hold Shift to choose the stage", Some(view(&shift))).detail(if v.new_apps_join_current {
            "Opening an app with Shift gives it a stage of its own. Shift-clicking an app in the Dock or a card brings it to the current stage."
        } else {
            "Opening an app with Shift, Shift-clicking it in the Dock or Shift-clicking a card brings it to the current stage."
        }),
        Row::new("Merge stages by dropping a card on a card", Some(view(&merge))),
    ]);
    f.footnote(EXTRAS_NOTE);
    [restore, new_apps, shift, merge]
}

/// The Windows pane: how windows are placed and move.
fn windows(f: &mut Form, m: &Maker, v: &SettingsValues) -> (Retained<NSPopUpButton>, Retained<NSSlider>, Retained<NSSwitch>) {
    let fit_index = FIT_MODES.iter().position(|x| *x == v.fit).unwrap_or(0);
    let fit = m.popup(&FIT_TITLES, fit_index, tag::FIT);
    let hotkeys = m.switch(v.hotkeys, tag::HOTKEYS);
    m.ours(tag::SPEED);
    let (speed_below, speed, speed_h) = speed_view(m.mtm, f.inner() - 24.0, speed_stop(v.speed), v.animations_on, m.target);
    let mut speed_row = Row::new("Animation speed", None);
    if !v.animations_on {
        speed_row = speed_row.detail("Animations are off in the config file ([animation] enabled = false or speed = 0).");
    }
    speed_row.below = Some((speed_below, speed_h));
    f.group(vec![
        Row::new("Keep windows clear of the strip", Some(view(&fit)))
            .detail("Off is Apple's: every window keeps its own frame."),
        speed_row,
        Row::new("Keyboard shortcuts", Some(view(&hotkeys)))
            .detail("⌃⌥1–9 switch stages, ⌃⌥⇧1–9 send the front window, ⌃⌥N new stage, ⌃⌥P pin, ⌃⌥S strip (as set in the config file)."),
    ]);
    f.footnote(EXTRAS_NOTE);
    (fit, speed, hotkeys)
}

/// The Advanced pane: the config file.
fn advanced(f: &mut Form, m: &Maker, v: &SettingsValues) -> Retained<NSButton> {
    let mut file_row = Row::new("Every option lives in a config file.", None).detail(v.config_path.clone());
    file_row.trailing.push(view(&m.button("Reveal in Finder", tag::REVEAL)));
    file_row.trailing.push(view(&m.button("Open Config File…", tag::OPEN)));
    let mut rows = vec![file_row];
    if let Some(e) = &v.config_error {
        let mut r = Row::new("The config file has an error; the last good settings stay in force.", None).detail(e.clone());
        r.detail_color = Some(NSColor::systemRedColor());
        rows.push(r);
    }
    let copy = m.button("Copy Instructions for Agents", tag::COPY);
    rows.push(
        Row::new("Ask an AI agent", Some(view(&copy)))
            .detail("The easiest way to change anything else is to ask an AI agent (Claude Code, Codex, …) to do it."),
    );
    f.group(rows);
    f.footnote("Changes apply at once and are saved in the config file, keeping its comments.");
    copy
}

/// The tallest content a pane shows without scrolling: most of the screen's visible height,
/// less the title bar and toolbar.
fn max_content(mtm: MainThreadMarker) -> f64 {
    NSScreen::mainScreen(mtm).map_or(700.0, |s| s.visibleFrame().size.height * 0.85 - 80.0).max(300.0)
}

/// Builds every pane for `v`; returns them and their controls.
fn build(mtm: MainThreadMarker, v: &SettingsValues, target: &ActionTarget) -> (Vec<PaneView>, Controls) {
    let max = max_content(mtm);
    let mut panes = Vec::new();
    let mut add = |pane: Pane, f: Form| {
        let (form_view, height) = f.finish();
        let view = if height <= max {
            form_view.into_super()
        } else {
            let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, max)));
            scroll.setHasVerticalScroller(true);
            scroll.setAutohidesScrollers(true);
            scroll.setDrawsBackground(false);
            scroll.setDocumentView(Some(&form_view));
            scroll.into_super()
        };
        panes.push(PaneView { pane, view, height: height.min(max) });
    };
    let maker = |pane: Pane| Maker { mtm, target, pane };

    let mut f = Form::new(mtm, WIDTH);
    let (login, icon, updates, install) = general(&mut f, &maker(Pane::General), v);
    add(Pane::General, f);
    let mut f = Form::new(mtm, WIDTH);
    let (recent, grouping) = stage_manager(&mut f, &maker(Pane::StageManager), v);
    add(Pane::StageManager, f);
    let mut f = Form::new(mtm, WIDTH);
    let [restore, new_apps, shift, merge] = stages(&mut f, &maker(Pane::Stages), v);
    add(Pane::Stages, f);
    let mut f = Form::new(mtm, WIDTH);
    let (fit, speed, hotkeys) = windows(&mut f, &maker(Pane::Windows), v);
    add(Pane::Windows, f);
    let mut f = Form::new(mtm, WIDTH);
    let copy = advanced(&mut f, &maker(Pane::Advanced), v);
    add(Pane::Advanced, f);

    let controls = Controls { login, icon, updates, install, recent, grouping, restore, new_apps, merge, shift, fit, hotkeys, speed, copy };
    (panes, controls)
}

/// Opens the window (or brings it to the front) with `values`, on `pane` when it opens;
/// each change goes to `on_change`.
pub fn show(mtm: MainThreadMarker, values: SettingsValues, pane: Pane, on_change: impl Fn(Change) + 'static) {
    HANDLER.with(|h| *h.borrow_mut() = Some(Rc::new(on_change)));
    let existing = OPEN.with(|o| o.borrow().as_ref().map(|o| o.window.clone()));
    if let Some(window) = existing {
        apply(values);
        form::bring_to_front(mtm, &window);
        return;
    }
    let window = create(mtm, values, pane);
    window.center();
    form::bring_to_front(mtm, &window);
}

/// Creates the window (not shown) on `pane` with its change handler; the preview draws it.
pub fn show_with_handler(mtm: MainThreadMarker, values: SettingsValues, pane: Pane, on_change: impl Fn(Change) + 'static) -> Retained<NSWindow> {
    HANDLER.with(|h| *h.borrow_mut() = Some(Rc::new(on_change)));
    create(mtm, values, pane)
}

/// Creates the window (not shown) on `pane` and makes it the open one.
pub fn create(mtm: MainThreadMarker, values: SettingsValues, pane: Pane) -> Retained<NSWindow> {
    let target = ActionTarget::new(mtm, clicked);
    let (panes, controls) = build(mtm, &values, &target);
    let height = panes.iter().find(|p| p.pane == pane).map_or(400.0, |p| p.height);
    // SAFETY: a fresh allocation, a valid frame and style; released only by us.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, height)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is owned by OPEN until closed; AppKit must not release it on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTabbingMode(objc2_app_kit::NSWindowTabbingMode::Disallowed);
    let delegate = ToolbarDelegate::new(mtm, target.clone());
    let toolbar = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), &NSString::from_str("StageManagerSettings"));
    toolbar.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*delegate)));
    toolbar.setDisplayMode(NSToolbarDisplayMode::IconAndLabel);
    toolbar.setAllowsUserCustomization(false);
    toolbar.setAutosavesConfiguration(false);
    window.setToolbar(Some(&toolbar));
    window.setToolbarStyle(NSWindowToolbarStyle::Preference);
    OPEN.with(|o| {
        *o.borrow_mut() = Some(Open {
            window: window.clone(),
            toolbar,
            _delegate: delegate,
            target,
            panes,
            pane,
            controls,
            shown: values,
        })
    });
    install_pane(pane, false);
    window
}

/// Shows `pane` (a toolbar click): the window takes its title and height, animated, its
/// top-left edge kept.
fn select(pane: Pane, animate: bool) {
    let same = OPEN.with(|o| o.borrow().as_ref().is_none_or(|o| o.pane == pane && o.window.contentView().is_some()));
    if same {
        return;
    }
    install_pane(pane, animate);
}

/// Makes `pane` the window's content: its title, the toolbar's selection, its view and its
/// height. The resize runs outside the window's borrow (an animated one runs the event loop).
fn install_pane(pane: Pane, animate: bool) {
    let Some((window, toolbar, content, height)) = OPEN.with(|o| {
        let mut o = o.borrow_mut();
        let open = o.as_mut()?;
        let p = open.panes.iter().find(|p| p.pane == pane)?;
        open.pane = pane;
        Some((open.window.clone(), open.toolbar.clone(), p.view.clone(), p.height))
    }) else {
        return;
    };
    window.setTitle(&NSString::from_str(pane.title()));
    toolbar.setSelectedItemIdentifier(Some(&NSString::from_str(pane.id())));
    let frame = window.frame();
    let now = window.contentRectForFrameRect(frame).size.height;
    let grow = height - now;
    let target = NSRect::new(
        NSPoint::new(frame.origin.x, frame.origin.y - grow),
        NSSize::new(frame.size.width, frame.size.height + grow),
    );
    window.setContentView(Some(&content));
    if grow.abs() >= 0.5 {
        window.setFrame_display_animate(target, true, animate && window.isVisible());
    }
    tracing::debug!(pane = pane.id(), "settings: pane shown");
}

/// Shows fresh values on every pane, the hidden ones too: the controls change in place, or
/// the panes are rebuilt when their rows change (login item state, Apple's Stage Manager, a
/// config error, animations off).
pub fn apply(values: SettingsValues) {
    let rebuild = OPEN.with(|o| {
        let mut o = o.borrow_mut();
        let Some(open) = o.as_mut() else { return false };
        if open.shown == values {
            return false;
        }
        if open.shown.shape() != values.shape() || open.shown.version != values.version || open.shown.config_path != values.config_path {
            open.shown = values.clone();
            return true;
        }
        let c = &open.controls;
        set_switch(&c.login, login_on(&values));
        set_switch(&c.icon, values.app.show_menu_bar_icon);
        set_switch(&c.updates, values.app.check_for_updates);
        set_switch(&c.install, values.app.install_updates);
        c.install.setEnabled(values.app.check_for_updates);
        set_switch(&c.recent, values.show_recent_apps);
        c.grouping.selectItemAtIndex(if values.all_at_once { 0 } else { 1 });
        set_switch(&c.restore, values.restore_after_restart);
        set_switch(&c.new_apps, values.new_apps_join_current);
        set_switch(&c.merge, values.merge_on_drop);
        set_switch(&c.shift, values.shift_brings_here);
        c.fit.selectItemAtIndex(FIT_MODES.iter().position(|m| *m == values.fit).unwrap_or(0) as isize);
        set_switch(&c.hotkeys, values.hotkeys);
        c.speed.setDoubleValue(speed_stop(values.speed) as f64);
        let wallpaper_changed = open.shown.wallpaper_only_in_stage_manager != values.wallpaper_only_in_stage_manager
            || open.shown.config_error != values.config_error;
        open.shown = values;
        wallpaper_changed
    });
    if rebuild {
        // After the current event: the control that sent it is still on the stack.
        dispatch2::DispatchQueue::main().exec_async(|| {
            if let Some(mtm) = MainThreadMarker::new() {
                rebuild_now(mtm);
            }
        });
    }
}

fn rebuild_now(mtm: MainThreadMarker) {
    let pane = OPEN.with(|o| {
        let mut o = o.borrow_mut();
        let open = o.as_mut()?;
        let (panes, controls) = build(mtm, &open.shown, &open.target);
        // The old views go after the current event (a control of theirs may be sending).
        form::release_later(std::mem::replace(&mut open.panes, panes).into_iter().map(|p| p.view).collect::<Vec<_>>());
        open.controls = controls;
        tracing::debug!("settings: rebuilt");
        Some(open.pane)
    });
    if let Some(pane) = pane {
        install_pane(pane, false);
    }
}

/// Whether the window is open (shown or hidden).
pub fn is_open() -> bool {
    OPEN.with(|o| o.borrow().as_ref().is_some_and(|o| o.window.isVisible()))
}

/// "Copied" on the copy button for a moment.
pub fn confirm_copied() {
    let Some(button) = OPEN.with(|o| o.borrow().as_ref().map(|o| o.controls.copy.clone())) else { return };
    let title = button.title();
    button.setTitle(&NSString::from_str("Copied"));
    let restore = RcBlock::new(move |_t: NonNull<NSTimer>| button.setTitle(&title));
    // SAFETY: the block runs once on the main run loop, which keeps the timer.
    let _ = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.5, false, &restore) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slider_stops_at_apples_speed_in_the_middle() {
        assert_eq!(SPEEDS[SPEEDS.len() / 2], 1.0, "Native in the middle");
        assert_eq!(speed_stop(1.0), 4);
        assert_eq!(speed_stop(2.0), 6);
        assert_eq!(speed_stop(0.25), 0);
        assert_eq!(speed_stop(4.0), 8);
        assert_eq!(speed_stop(9.0), 8, "clamped");
        assert_eq!(speed_stop(1.2), 4, "nearest by ratio: 1.2 is closer to 1 than to 1.5");
        assert_eq!(speed_stop(1.3), 5);
        for (i, s) in SPEEDS.iter().enumerate() {
            assert_eq!(speed_stop(*s), i, "every stop maps to itself");
        }
    }

    #[test]
    fn values_read_apples_keys_as_its_settings_do() {
        let config = Config::default();
        let raw: RawSettings = RawSettings::new();
        let v = SettingsValues::new(&config, &raw, LoginStatus::Off, "~/x".into(), None, "1.0.0".into());
        assert!(v.show_recent_apps && v.all_at_once && !v.wallpaper_only_in_stage_manager && !v.apple_on, "macOS defaults");
        let raw: RawSettings = [("AutoHide", 1), ("AppWindowGroupingBehavior", 0), ("EnableStandardClickToShowDesktop", 0), ("GloballyEnabled", 1)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let v = SettingsValues::new(&config, &raw, LoginStatus::Off, "~/x".into(), None, "1.0.0".into());
        assert!(!v.show_recent_apps && !v.all_at_once && v.wallpaper_only_in_stage_manager && v.apple_on);
        assert!(v.merge_on_drop && v.shift_brings_here && v.fit == FitMode::Off && !v.hotkeys && v.speed == 1.0 && v.animations_on);
        assert!(v.restore_after_restart, "stages come back after a restart by default");
        assert!(!v.new_apps_join_current, "Apple's: a newly opened app gets a stage of its own");
    }

    #[test]
    fn only_row_changes_rebuild_the_form() {
        let config = Config::default();
        let base = SettingsValues::new(&config, &RawSettings::new(), LoginStatus::On, "~/x".into(), None, "1.0.0".into());
        let mut app = base.app;
        app.check_for_updates = false;
        app.install_updates = true;
        let toggled = SettingsValues {
            merge_on_drop: false,
            restore_after_restart: false,
            speed: 2.0,
            app,
            ..base.clone()
        };
        assert_eq!(base.shape(), toggled.shape(), "a switch or the slider updates in place");
        for other in [
            SettingsValues { login: LoginStatus::NeedsApproval, ..base.clone() },
            SettingsValues { apple_on: true, ..base.clone() },
            SettingsValues { config_error: Some("bad".into()), ..base.clone() },
            SettingsValues { animations_on: false, ..base.clone() },
            SettingsValues { new_apps_join_current: true, ..base.clone() },
        ] {
            assert_ne!(base.shape(), other.shape());
        }
    }

    #[test]
    fn the_panes_in_order_with_their_rows() {
        let titles: Vec<_> = Pane::ALL.iter().map(|p| p.title()).collect();
        assert_eq!(titles, ["General", "Stage Manager", "Stages", "Windows", "Advanced"]);
        assert_eq!(Pane::default(), Pane::General, "a first opening shows General");
        assert_eq!(Pane::Stages.tags(), [tag::RESTORE, tag::NEW_APPS, tag::SHIFT, tag::MERGE]);
        assert_eq!(Pane::Windows.tags(), [tag::FIT, tag::SPEED, tag::HOTKEYS]);
        assert!(Pane::StageManager.tags().contains(&tag::GROUPING) && Pane::StageManager.tags().contains(&tag::RECENT));
        assert!(Pane::Advanced.tags().contains(&tag::OPEN) && Pane::General.tags().contains(&tag::CHECK));
        for p in Pane::ALL {
            assert_eq!(Pane::from_id(p.id()), Some(p), "the remembered id reads back");
            assert_eq!(Pane::of_item_tag(p.item_tag()), Some(p));
            assert!(!p.symbol().is_empty());
        }
        assert_eq!(Pane::from_id("extras"), None);
        assert_eq!(Pane::of_item_tag(tag::PANE + 5), None);
        assert_eq!(Pane::of_item_tag(tag::LOGIN), None, "a control's tag is no pane");
    }

    #[test]
    fn every_control_sits_on_exactly_one_pane() {
        for t in tag::ALL {
            let on: Vec<_> = Pane::ALL.into_iter().filter(|p| p.tags().contains(&t)).collect();
            assert_eq!(on.len(), 1, "tag {t} on {on:?}");
        }
        let all: usize = Pane::ALL.iter().map(|p| p.tags().len()).sum();
        assert_eq!(all, tag::ALL.len(), "no pane lists a tag that is not a control");
    }

    #[test]
    fn every_change_is_still_reachable_from_a_pane() {
        // `tag_of` matches every `Change`, so a new change without a control does not compile.
        let changes = [
            Change::LaunchAtLogin(true),
            Change::OpenLoginItems,
            Change::MenuBarIcon(true),
            Change::AutoUpdates(true),
            Change::InstallUpdates(true),
            Change::CheckNow,
            Change::TurnOffApple,
            Change::ShowRecentApps(true),
            Change::AllAtOnce(true),
            Change::OpenDesktopAndDock,
            Change::RestoreAfterRestart(true),
            Change::NewAppsJoinCurrent(true),
            Change::MergeOnDrop(true),
            Change::ShiftBringsHere(true),
            Change::Fit(FitMode::Move),
            Change::Hotkeys(true),
            Change::Speed(2.0),
            Change::OpenConfig,
            Change::RevealConfig,
            Change::CopyAgentInstructions,
        ];
        assert_eq!(changes.len(), tag::ALL.len(), "one change per control");
        for c in changes {
            let t = tag_of(&c);
            assert!(Pane::ALL.iter().any(|p| p.tags().contains(&t)), "{c:?} has no pane");
        }
        for p in Pane::ALL {
            assert_eq!(Pane::of_item_tag(tag_of(&Change::Pane(p))), Some(p), "the toolbar reaches {p:?}");
        }
    }
}
