//! The Settings window: one pane in the look of System Settings, a grouped form.
//!
//! - **General:** launch at login, the menu bar icon, automatic update checks, installing
//!   updates automatically, the version with Check for Updates….
//! - **Stage Manager** (Apple's own Customize sheet): whether Apple's Stage Manager is on
//!   (with Turn Off), "Show recent apps", "Show windows from an application", and "Click
//!   wallpaper to reveal desktop" (read only; its button opens Desktop & Dock).
//! - **Extras:** merge by dropping a card on a card, Shift brings apps here, keeping windows
//!   clear of the strip, keyboard shortcuts, animation speed.
//! - **Advanced:** the config file (open, reveal) and the note that an AI agent is the
//!   easiest way to change anything else, with Copy Instructions for Agents.
//!
//! The window shows [`SettingsValues`] and reports each user change as a [`Change`] to the
//! closure it was opened with; the caller writes it (config file, macOS setting, login item)
//! and calls [`apply`] with fresh values. Changes made elsewhere (an agent editing the file)
//! arrive the same way. Main thread only.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{MainThreadOnly, Message};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSControlStateValueOn, NSPopUpButton, NSScreen, NSScrollView, NSSlider,
    NSSwitch, NSTextAlignment, NSTickMarkPosition, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString, NSTimer};

use super::form::{self, ActionTarget, FlippedView, Form, Row};
use crate::config::{AppConfig, Config, FitMode};
use crate::platform::login_item::LoginStatus;
use crate::platform::native_settings::RawSettings;

/// The form's width.
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

    /// What decides the form's rows (a change rebuilds it; anything else updates in place).
    fn shape(&self) -> (u8, bool, bool, bool) {
        let login = match self.login {
            LoginStatus::On => 0,
            LoginStatus::NeedsApproval => 1,
            LoginStatus::Off => 2,
            LoginStatus::Unavailable(_) => 3,
        };
        (login, self.apple_on, self.config_error.is_some(), self.animations_on)
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
    MergeOnDrop(bool),
    ShiftBringsHere(bool),
    Fit(FitMode),
    Hotkeys(bool),
    Speed(f64),
    OpenConfig,
    RevealConfig,
    CopyAgentInstructions,
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
}

const FIT_TITLES: [&str; 3] = ["Off", "Move Windows", "Move and Narrow"];
const FIT_MODES: [FitMode; 3] = [FitMode::Off, FitMode::Move, FitMode::Narrow];

/// The controls whose state is read and updated.
struct Controls {
    login: Retained<NSSwitch>,
    icon: Retained<NSSwitch>,
    updates: Retained<NSSwitch>,
    install: Retained<NSSwitch>,
    recent: Retained<NSSwitch>,
    grouping: Retained<NSPopUpButton>,
    merge: Retained<NSSwitch>,
    shift: Retained<NSSwitch>,
    fit: Retained<NSPopUpButton>,
    hotkeys: Retained<NSSwitch>,
    speed: Retained<NSSlider>,
    copy: Retained<NSButton>,
}

/// The open window.
struct Open {
    window: Retained<NSWindow>,
    scroll: Retained<NSScrollView>,
    _target: Retained<ActionTarget>,
    controls: Controls,
    shown: SettingsValues,
}

/// Receives the user's changes.
type ChangeFn = Rc<dyn Fn(Change)>;

thread_local! {
    static OPEN: RefCell<Option<Open>> = const { RefCell::new(None) };
    static HANDLER: RefCell<Option<ChangeFn>> = const { RefCell::new(None) };
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
    let change = OPEN.with(|o| o.borrow().as_ref().and_then(|o| change_for(&o.controls, t)));
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

/// Builds the form for `v`; returns its view, height and controls.
fn build(mtm: MainThreadMarker, v: &SettingsValues, target: &ActionTarget) -> (Retained<FlippedView>, f64, Controls) {
    let mut f = Form::new(mtm, WIDTH);
    let sw = |value: bool, t: isize| form::switch(mtm, value, target, t);
    let btn = |title: &str, t: isize| form::button(mtm, title, target, t);

    // General
    f.header("General");
    let login = sw(login_on(v), tag::LOGIN);
    let mut login_row = Row::new("Launch at login", None);
    match v.login {
        LoginStatus::NeedsApproval => {
            login_row = login_row.detail("Allow Stage Manager in System Settings → General → Login Items.");
            login_row.trailing.push(view(&btn("Login Items…", tag::LOGIN_ITEMS)));
        }
        LoginStatus::Unavailable(_) => {
            login.setEnabled(false);
            login_row = login_row.detail("Available when Stage Manager runs from Applications.");
        }
        LoginStatus::On | LoginStatus::Off => {}
    }
    login_row.trailing.push(view(&login));
    let icon = sw(v.app.show_menu_bar_icon, tag::ICON);
    let updates = sw(v.app.check_for_updates, tag::UPDATES);
    let install = sw(v.app.install_updates, tag::INSTALL);
    install.setEnabled(v.app.check_for_updates);
    f.group(vec![
        login_row,
        Row::new("Show menu bar icon", Some(view(&icon)))
            .detail("To open Settings again, open Stage Manager from Applications."),
        Row::new("Check for updates automatically", Some(view(&updates)))
            .detail("Once a day, from GitHub. Nothing about you or this Mac is sent."),
        Row::new("Install updates automatically", Some(view(&install)))
            .detail("While you're away from the Mac; otherwise Stage Manager asks first. Your windows come back before it relaunches."),
        Row::new(&format!("Stage Manager {}", v.version), Some(view(&btn("Check for Updates…", tag::CHECK)))),
    ]);
    f.space(12.0);

    // Stage Manager (Apple's Customize sheet)
    f.header("Stage Manager");
    let mut apple = Row::new("Apple's Stage Manager", None);
    if v.apple_on {
        apple = apple.detail("On. This app stands down until Apple's is off: it arranges nothing and shows no strip.");
        apple.trailing.push(view(&btn("Turn Off", tag::TURN_OFF)));
    } else {
        apple = apple.detail("Off. This app takes its place.");
    }
    let recent = sw(v.show_recent_apps, tag::RECENT);
    let grouping = form::popup(mtm, &["All at Once", "One at a Time"], if v.all_at_once { 0 } else { 1 }, target, tag::GROUPING);
    let wallpaper = if v.wallpaper_only_in_stage_manager { "Only in Stage Manager" } else { "Always" };
    f.group(vec![
        apple,
        Row::new("Show recent apps in Stage Manager", Some(view(&recent))),
        Row::new("Show windows from an application", Some(view(&grouping))),
        Row::new("Click wallpaper to reveal desktop", Some(view(&btn("Desktop & Dock Settings…", tag::DESKTOP_DOCK))))
            .detail(format!("{wallpaper}. Set in System Settings → Desktop & Dock.")),
    ]);
    f.footnote("These are macOS's own Stage Manager settings, shared with System Settings → Desktop & Dock.");
    f.space(12.0);

    // Extras
    f.header("Extras");
    let merge = sw(v.merge_on_drop, tag::MERGE);
    let shift = sw(v.shift_brings_here, tag::SHIFT);
    let fit_index = FIT_MODES.iter().position(|m| *m == v.fit).unwrap_or(0);
    let fit = form::popup(mtm, &FIT_TITLES, fit_index, target, tag::FIT);
    let hotkeys = sw(v.hotkeys, tag::HOTKEYS);
    let inner = f.inner() - 24.0;
    let (speed_below, speed, speed_h) = speed_view(mtm, inner, speed_stop(v.speed), v.animations_on, target);
    let mut speed_row = Row::new("Animation speed", None);
    if !v.animations_on {
        speed_row = speed_row.detail("Animations are off in the config file ([animation] enabled = false or speed = 0).");
    }
    speed_row.below = Some((speed_below, speed_h));
    f.group(vec![
        Row::new("Merge stages by dropping a card on a card", Some(view(&merge))),
        Row::new("Hold Shift to bring apps to the current stage", Some(view(&shift)))
            .detail("While opening an app, clicking it in the Dock, or clicking a card."),
        Row::new("Keep windows clear of the strip", Some(view(&fit)))
            .detail("Off is Apple's: every window keeps its own frame."),
        Row::new("Keyboard shortcuts", Some(view(&hotkeys)))
            .detail("⌃⌥1–9 switch stages, ⌃⌥⇧1–9 send the front window, ⌃⌥N new stage, ⌃⌥P pin, ⌃⌥S strip (as set in the config file)."),
        speed_row,
    ]);
    f.footnote("Not in Apple's Stage Manager. Each stays out of the way until you use it.");
    f.space(12.0);

    // Advanced
    f.header("Advanced");
    let mut file_row = Row::new("Every option lives in a config file.", None).detail(v.config_path.clone());
    file_row.trailing.push(view(&btn("Reveal in Finder", tag::REVEAL)));
    file_row.trailing.push(view(&btn("Open Config File…", tag::OPEN)));
    let mut rows = vec![file_row];
    if let Some(e) = &v.config_error {
        let mut r = Row::new("The config file has an error; the last good settings stay in force.", None).detail(e.clone());
        r.detail_color = Some(NSColor::systemRedColor());
        rows.push(r);
    }
    let copy = btn("Copy Instructions for Agents", tag::COPY);
    rows.push(
        Row::new("Ask an AI agent", Some(view(&copy)))
            .detail("The easiest way to change anything else is to ask an AI agent (Claude Code, Codex, …) to do it."),
    );
    f.group(rows);
    f.footnote("Changes apply at once and are saved in the config file, keeping its comments.");

    let (view, height) = f.finish();
    (view, height, Controls { login, icon, updates, install, recent, grouping, merge, shift, fit, hotkeys, speed, copy })
}

/// Opens the window (or brings it to the front) with `values`; each change goes to
/// `on_change`.
pub fn show(mtm: MainThreadMarker, values: SettingsValues, on_change: impl Fn(Change) + 'static) {
    HANDLER.with(|h| *h.borrow_mut() = Some(Rc::new(on_change)));
    let existing = OPEN.with(|o| o.borrow().as_ref().map(|o| o.window.clone()));
    if let Some(window) = existing {
        apply(values);
        form::bring_to_front(mtm, &window);
        return;
    }
    let window = create(mtm, values);
    window.center();
    form::bring_to_front(mtm, &window);
}

/// Creates the window (not shown) with its change handler; the preview draws it.
pub fn show_with_handler(mtm: MainThreadMarker, values: SettingsValues, on_change: impl Fn(Change) + 'static) -> Retained<NSWindow> {
    HANDLER.with(|h| *h.borrow_mut() = Some(Rc::new(on_change)));
    create(mtm, values)
}

/// Creates the window (not shown) and makes it the open one.
pub fn create(mtm: MainThreadMarker, values: SettingsValues) -> Retained<NSWindow> {
    let target = ActionTarget::new(mtm, clicked);
    let (view, height, controls) = build(mtm, &values, &target);
    let max = NSScreen::mainScreen(mtm).map_or(800.0, |s| s.visibleFrame().size.height * 0.85);
    let content = NSSize::new(WIDTH, height.min(max));
    // SAFETY: a fresh allocation, a valid frame and style; released only by us.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), content),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is owned by OPEN until closed; AppKit must not release it on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("Stage Manager Settings"));
    window.setTabbingMode(objc2_app_kit::NSWindowTabbingMode::Disallowed);
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), content));
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setDrawsBackground(false);
    scroll.setDocumentView(Some(&view));
    window.setContentView(Some(&scroll));
    OPEN.with(|o| *o.borrow_mut() = Some(Open { window: window.clone(), scroll, _target: target, controls, shown: values }));
    window
}

/// Shows fresh values: the controls change in place, or the form is rebuilt when its rows
/// change (login item state, Apple's Stage Manager, a config error, animations off).
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
    OPEN.with(|o| {
        let mut o = o.borrow_mut();
        let Some(open) = o.as_mut() else { return };
        let origin = open.scroll.contentView().bounds().origin;
        let (view, _h, controls) = build(mtm, &open.shown, &open._target);
        open.scroll.setDocumentView(Some(&view));
        open.scroll.contentView().scrollToPoint(origin);
        open.controls = controls;
        tracing::debug!("settings: rebuilt");
    });
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
    }

    #[test]
    fn only_row_changes_rebuild_the_form() {
        let config = Config::default();
        let base = SettingsValues::new(&config, &RawSettings::new(), LoginStatus::On, "~/x".into(), None, "1.0.0".into());
        let mut app = base.app;
        app.check_for_updates = false;
        app.install_updates = true;
        let toggled = SettingsValues { merge_on_drop: false, speed: 2.0, app, ..base.clone() };
        assert_eq!(base.shape(), toggled.shape(), "a switch or the slider updates in place");
        for other in [
            SettingsValues { login: LoginStatus::NeedsApproval, ..base.clone() },
            SettingsValues { apple_on: true, ..base.clone() },
            SettingsValues { config_error: Some("bad".into()), ..base.clone() },
            SettingsValues { animations_on: false, ..base.clone() },
        ] {
            assert_ne!(base.shape(), other.shape());
        }
    }
}
