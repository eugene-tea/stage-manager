//! The menu bar status item and its menu.
//!
//! **The desktop notice** (round desktop-notice): after a restart, the windows macOS reopened on
//! another Desktop than their own are listed in a section of the menu, above About: a header,
//! one entry per window (the app and title; below it where the window belongs and where it
//! is), then Dismiss. While any is listed the icon carries one passive mark
//! ([`crate::ui::glyph::marked_shapes`]). Nothing pops up: the login is busy with apps
//! opening, and a list that waits in the menu until it is done is seen at the next look at the
//! menu bar. A click on an entry brings its window forward; an entry leaves by itself once its
//! window shows on its own Desktop ([`crate::engine::desktop_notice`]).
//!
//! Menu items call a small Objective-C target ([`MenuTarget`]) that forwards each click as a
//! [`MenuAction`] to the handler installed with [`set_action_handler`]. Main thread only.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};

use crate::platform::permissions::{self, Approval, PermissionState};
use crate::ui::glyph;

/// What a menu click asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// Ask macOS for an approval and open its Privacy pane.
    Allow(Approval),
    /// The standard About panel.
    About,
    /// A manual update check.
    CheckForUpdates,
    /// The Settings window.
    Settings,
    /// Write a problem mark into the day's log (shown while the log is verbose).
    MarkProblem,
    /// Turn Apple's Stage Manager off (the user's click; never without one).
    TurnOffAppleStageManager,
    /// Bring a window the desktop notice lists forward (its window id).
    ShowNoticed(u32),
    /// Dismiss the desktop notice's list.
    DismissNotice,
    Quit,
}

/// The installed receiver of menu clicks.
type ActionHandler = Box<dyn Fn(MenuAction)>;

thread_local! {
    static HANDLER: RefCell<Option<ActionHandler>> = const { RefCell::new(None) };
}

/// Installs the function that receives menu clicks (main thread).
pub fn set_action_handler(handler: impl Fn(MenuAction) + 'static) {
    HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(handler)));
}

/// Hands `action` to the installed handler (also used by the app's main menu).
pub fn dispatch(action: MenuAction) {
    // Take the handler out while it runs so a handler that re-enters the menu cannot hit a
    // RefCell borrow panic inside an Objective-C callback.
    let handler = HANDLER.with(|h| h.borrow_mut().take());
    if let Some(handler) = handler {
        handler(action);
        HANDLER.with(|h| {
            let mut slot = h.borrow_mut();
            if slot.is_none() {
                *slot = Some(handler);
            }
        });
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; MenuTarget does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "SMMenuTarget"]
    pub struct MenuTarget;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        // SAFETY (all methods): action signature `- (void)name:(id)sender`.
        #[unsafe(method(allowAccessibility:))]
        fn allow_accessibility(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::Allow(Approval::Accessibility));
        }

        #[unsafe(method(allowScreenRecording:))]
        fn allow_screen_recording(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::Allow(Approval::ScreenRecording));
        }

        #[unsafe(method(about:))]
        fn about(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::About);
        }

        #[unsafe(method(checkForUpdates:))]
        fn check_for_updates(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::CheckForUpdates);
        }

        #[unsafe(method(settings:))]
        fn settings(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::Settings);
        }

        #[unsafe(method(markProblem:))]
        fn mark_problem(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::MarkProblem);
        }

        #[unsafe(method(turnOffAppleStageManager:))]
        fn turn_off_apple(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::TurnOffAppleStageManager);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::Quit);
        }

        #[unsafe(method(showNoticed:))]
        fn show_noticed(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            // SAFETY: the sender of this action is one of our NSMenuItems; `tag` returns an
            // NSInteger, the window id it was given.
            let tag: isize = unsafe { msg_send![sender, tag] };
            if let Ok(id) = u32::try_from(tag) {
                dispatch(MenuAction::ShowNoticed(id));
            }
        }

        #[unsafe(method(dismissNotice:))]
        fn dismiss_notice(&self, _sender: Option<&AnyObject>) {
            dispatch(MenuAction::DismissNotice);
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// What the menu shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuState {
    pub permissions: PermissionState,
    /// Window pictures are on (`[strip] capture`): Screen Recording is asked for.
    pub capture_on: bool,
    /// macOS refused window captures; "Allow Screen Recording…" shows and retries them.
    pub capture_refused: bool,
    /// Apple's Stage Manager is on (`GloballyEnabled = 1`).
    pub apple_stage_manager_on: bool,
    /// config.toml failed to load (the last good config stays in force).
    pub config_error: Option<String>,
    /// Show Mark a Problem (the log is verbose: `[log] level` debug or trace).
    pub mark_shown: bool,
    /// When the last problem mark was written (`HH:MM:SS`).
    pub last_mark: Option<String>,
    /// The desktop notice's entries (empty: no section, no mark).
    pub notice: Vec<NoticeLine>,
}

/// One window the desktop notice lists, as the menu shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoticeLine {
    /// The window's id (the entry's tag; a click brings it forward).
    pub window: u32,
    /// The entry's title: the app and the window's title.
    pub title: String,
    /// Its second line: where the window belongs and where it is.
    pub detail: String,
}

/// The desktop notice section's header.
pub const NOTICE_HEADER: &str = "Reopened on Another Desktop";

/// The item that dismisses the list.
pub const NOTICE_DISMISS: &str = "Dismiss";

/// One item of the desktop notice's section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeItem {
    /// [`NOTICE_HEADER`] (a section header).
    Header,
    /// The entry for line `.0`.
    Entry(usize),
    /// [`NOTICE_DISMISS`].
    Dismiss,
    /// The separator before About.
    Separator,
}

/// The desktop notice's section for `lines`, in menu order: nothing when none is listed.
pub fn notice_items(lines: &[NoticeLine]) -> Vec<NoticeItem> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut out = vec![NoticeItem::Header];
    out.extend((0..lines.len()).map(NoticeItem::Entry));
    out.extend([NoticeItem::Dismiss, NoticeItem::Separator]);
    out
}

/// An entry's title and second line: the detail as the item's subtitle where macOS has one
/// (14.4), else in the title after the window's.
pub fn entry_texts(line: &NoticeLine, subtitles: bool) -> (String, Option<String>) {
    if subtitles { (line.title.clone(), Some(line.detail.clone())) } else { (format!("{} ({})", line.title, line.detail), None) }
}

/// One entry of the menu, in Apple's menu-bar-app order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    /// "Stage Manager needs … access" (disabled), while it is missing.
    Needs(Approval),
    /// "Allow ……", while it is missing.
    Allow(Approval),
    /// "Turn Off Apple's Stage Manager…", while it is on (we stand down meanwhile).
    AppleWarning,
    /// The config error (disabled), while there is one.
    ConfigError,
    About,
    CheckForUpdates,
    Settings,
    MarkProblem,
    Quit,
    /// A separator; the number tells them apart.
    Separator(u8),
}

/// The menu's entries for `state`, in order: approvals and warnings at the top only while
/// they apply, then About, Check for Updates, Settings, Mark a Problem (while the log is
/// verbose) and Quit, with separators between the groups.
pub fn items(state: &MenuState) -> Vec<Item> {
    let mut out = Vec::new();
    for a in permissions::missing(&state.permissions, state.capture_on, state.capture_refused) {
        out.extend([Item::Needs(a), Item::Allow(a)]);
    }
    if state.apple_stage_manager_on {
        out.push(Item::AppleWarning);
    }
    if state.config_error.is_some() {
        out.push(Item::ConfigError);
    }
    if !out.is_empty() {
        out.push(Item::Separator(0));
    }
    out.extend([Item::About, Item::CheckForUpdates, Item::Separator(1), Item::Settings, Item::Separator(2)]);
    if state.mark_shown {
        out.extend([Item::MarkProblem, Item::Separator(3)]);
    }
    out.push(Item::Quit);
    out
}

/// Every entry that may show, in menu order.
fn all_items() -> Vec<Item> {
    let mut all = Vec::new();
    for a in Approval::ALL {
        all.extend([Item::Needs(a), Item::Allow(a)]);
    }
    all.extend([
        Item::AppleWarning,
        Item::ConfigError,
        Item::Separator(0),
        Item::About,
        Item::CheckForUpdates,
        Item::Separator(1),
        Item::Settings,
        Item::Separator(2),
        Item::MarkProblem,
        Item::Separator(3),
        Item::Quit,
    ]);
    all
}

/// The menu item while Apple's Stage Manager is on.
pub const APPLE_ITEM: &str = "Turn Off Apple's Stage Manager";

/// The Mark a Problem item's title: when the last mark went in, if any.
pub fn mark_title(last: Option<&str>) -> String {
    match last {
        Some(at) => format!("Mark a Problem (last at {at})"),
        None => "Mark a Problem".to_string(),
    }
}

/// The menu's config line: hidden (`None`) while the config is fine.
pub fn config_line(error: Option<&str>) -> Option<String> {
    const MAX: usize = 90;
    let e = error?;
    let mut text = format!("Config error: {e}");
    if text.chars().count() > MAX {
        text = text.chars().take(MAX - 1).collect::<String>() + "…";
    }
    Some(text)
}

/// The status item, its menu and its entries.
pub struct StatusMenu {
    item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    target: Retained<MenuTarget>,
    entries: Vec<(Item, Retained<NSMenuItem>)>,
    /// The desktop notice's lines shown and the items showing them.
    notice: RefCell<(Vec<NoticeLine>, Vec<Retained<NSMenuItem>>)>,
    /// The icon carries the desktop notice's mark.
    marked: Cell<bool>,
}

fn item(mtm: MainThreadMarker, title: &str, action: Option<Sel>, key: &str, target: &MenuTarget) -> Retained<NSMenuItem> {
    // SAFETY: valid title, selector implemented by `target`, plain key equivalent.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    };
    if action.is_some() {
        // SAFETY: the target is retained by StatusMenu for as long as the menu exists.
        unsafe { item.setTarget(Some(target)) };
    }
    item
}

fn entry(mtm: MainThreadMarker, which: Item, target: &MenuTarget) -> Retained<NSMenuItem> {
    let disabled = |title: &str| {
        let i = item(mtm, title, None, "", target);
        i.setEnabled(false);
        i
    };
    match which {
        Item::Needs(a) => disabled(&a.needs_line()),
        Item::Allow(a) => {
            let action = match a {
                Approval::Accessibility => sel!(allowAccessibility:),
                Approval::ScreenRecording => sel!(allowScreenRecording:),
            };
            item(mtm, &a.allow_title(), Some(action), "", target)
        }
        Item::AppleWarning => item(mtm, APPLE_ITEM, Some(sel!(turnOffAppleStageManager:)), "", target),
        Item::ConfigError => disabled(""),
        Item::About => item(mtm, "About Stage Manager", Some(sel!(about:)), "", target),
        Item::CheckForUpdates => item(mtm, "Check for Updates…", Some(sel!(checkForUpdates:)), "", target),
        Item::Settings => item(mtm, "Settings…", Some(sel!(settings:)), ",", target),
        Item::MarkProblem => item(mtm, &mark_title(None), Some(sel!(markProblem:)), "", target),
        Item::Quit => item(mtm, "Quit Stage Manager", Some(sel!(quit:)), "q", target),
        Item::Separator(_) => NSMenuItem::separatorItem(mtm),
    }
}

impl StatusMenu {
    /// Creates the status item with the app's glyph ([`glyph::status_image`]).
    pub fn new(mtm: MainThreadMarker) -> Self {
        let status_item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status_item.button(mtm) {
            button.setImage(Some(&glyph::status_image()));
            tracing::debug!(visible = status_item.isVisible(), "status item created");
        } else {
            tracing::warn!("status item has no button");
        }

        let target = MenuTarget::new(mtm);
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let entries: Vec<(Item, Retained<NSMenuItem>)> = all_items().into_iter().map(|i| (i, entry(mtm, i, &target))).collect();
        for (_, it) in &entries {
            menu.addItem(it);
        }
        status_item.setMenu(Some(&menu));

        Self { item: status_item, menu, target, entries, notice: RefCell::new((Vec::new(), Vec::new())), marked: Cell::new(false) }
    }

    /// Shows or hides the menu bar icon (`[app] show_menu_bar_icon`).
    pub fn set_visible(&self, visible: bool) {
        if self.item.isVisible() != visible {
            self.item.setVisible(visible);
            tracing::info!(visible, "menu bar icon");
        }
    }

    /// Shows `state`: each entry of [`items`] shows, every other one hides.
    pub fn update(&self, state: &MenuState) {
        let shown = items(state);
        for (which, it) in &self.entries {
            it.setHidden(!shown.contains(which));
            match which {
                Item::ConfigError => {
                    if let Some(text) = config_line(state.config_error.as_deref()) {
                        it.setTitle(&NSString::from_str(&text));
                    }
                }
                Item::MarkProblem => it.setTitle(&NSString::from_str(&mark_title(state.last_mark.as_deref()))),
                _ => {}
            }
        }
        self.show_notice(&state.notice);
    }

    /// Shows the desktop notice's `lines`: its section rebuilt above About when they changed,
    /// the icon's mark while any is listed.
    fn show_notice(&self, lines: &[NoticeLine]) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let marked = !lines.is_empty();
        if self.marked.replace(marked) != marked
            && let Some(button) = self.item.button(mtm)
        {
            let image = if marked { glyph::status_image_marked() } else { glyph::status_image() };
            button.setImage(Some(&image));
            tracing::debug!(marked, "menu bar icon: the desktop notice's mark");
        }
        let mut shown = self.notice.borrow_mut();
        if shown.0 == lines {
            return;
        }
        for it in shown.1.drain(..) {
            self.menu.removeItem(&it);
        }
        let about = self.entries.iter().find(|(i, _)| *i == Item::About).map(|(_, it)| it);
        let first = about.map_or(0, |it| self.menu.indexOfItem(it).max(0));
        for (at, which) in (first..).zip(notice_items(lines)) {
            let it = match which {
                NoticeItem::Header => NSMenuItem::sectionHeaderWithTitle(&NSString::from_str(NOTICE_HEADER), mtm),
                NoticeItem::Entry(i) => {
                    let it = item(mtm, "", Some(sel!(showNoticed:)), "", &self.target);
                    // `subtitle` came with macOS 14.4; the app runs from 14.0.
                    let (title, subtitle) = entry_texts(&lines[i], it.respondsToSelector(sel!(setSubtitle:)));
                    it.setTitle(&NSString::from_str(&title));
                    it.setTag(lines[i].window as isize);
                    if let Some(sub) = subtitle {
                        it.setSubtitle(Some(&NSString::from_str(&sub)));
                    }
                    it
                }
                NoticeItem::Dismiss => item(mtm, NOTICE_DISMISS, Some(sel!(dismissNotice:)), "", &self.target),
                NoticeItem::Separator => NSMenuItem::separatorItem(mtm),
            };
            self.menu.insertItem_atIndex(&it, at);
            shown.1.push(it);
        }
        shown.0 = lines.to_vec();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_line_shows_only_errors_and_stays_short() {
        assert_eq!(config_line(None), None);
        assert_eq!(config_line(Some("bad")).as_deref(), Some("Config error: bad"));
        let long = "x".repeat(300);
        let line = config_line(Some(&long)).unwrap_or_default();
        assert_eq!(line.chars().count(), 90);
        assert!(line.ends_with('…'));
    }

    fn state(a: bool, r: bool) -> MenuState {
        MenuState { permissions: PermissionState { accessibility: a, screen_recording: r }, capture_on: true, ..MenuState::default() }
    }

    #[test]
    fn with_every_approval_the_menu_is_apples_classic_one() {
        use Item::*;
        assert_eq!(items(&state(true, true)), vec![About, CheckForUpdates, Separator(1), Settings, Separator(2), Quit]);
        let verbose = MenuState { mark_shown: true, ..state(true, true) };
        assert_eq!(
            items(&verbose),
            vec![About, CheckForUpdates, Separator(1), Settings, Separator(2), MarkProblem, Separator(3), Quit]
        );
    }

    #[test]
    fn missing_approvals_show_at_the_top_until_granted() {
        use Item::*;
        let a = Approval::Accessibility;
        let r = Approval::ScreenRecording;
        assert_eq!(&items(&state(false, true))[..3], &[Needs(a), Allow(a), Separator(0)]);
        assert_eq!(&items(&state(true, false))[..3], &[Needs(r), Allow(r), Separator(0)]);
        assert_eq!(&items(&state(false, false))[..5], &[Needs(a), Allow(a), Needs(r), Allow(r), Separator(0)]);
        let no_pictures = MenuState { capture_on: false, ..state(true, false) };
        assert_eq!(items(&no_pictures)[0], About, "Screen Recording is not asked for without window pictures");
        let refused = MenuState { capture_refused: true, ..state(true, true) };
        assert_eq!(items(&refused)[..2], [Needs(r), Allow(r)], "macOS refused captures: ask again");
    }

    #[test]
    fn warnings_join_the_top_group() {
        use Item::*;
        let s = MenuState { apple_stage_manager_on: true, config_error: Some("bad".into()), ..state(true, true) };
        assert_eq!(&items(&s)[..4], &[AppleWarning, ConfigError, Separator(0), About]);
    }

    #[test]
    fn every_shown_item_exists_in_menu_order() {
        let all = all_items();
        let s = MenuState { apple_stage_manager_on: true, mark_shown: true, ..state(false, false) };
        let shown = items(&s);
        let positions: Vec<usize> = shown.iter().map(|i| all.iter().position(|a| a == i).expect("built")).collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{shown:?}");
    }

    fn line(n: u32) -> NoticeLine {
        NoticeLine { window: n, title: format!("Git — repo {n}"), detail: "Belongs on the Desktop with Term; now on this Desktop".into() }
    }

    #[test]
    fn the_desktop_notice_is_a_section_only_while_windows_are_listed() {
        assert!(notice_items(&[]).is_empty(), "nothing listed: the menu as it was");
        use NoticeItem::*;
        assert_eq!(notice_items(&[line(1), line(2)]), vec![Header, Entry(0), Entry(1), Dismiss, Separator]);
    }

    #[test]
    fn an_entry_puts_where_its_window_belongs_under_it_or_after_it() {
        let l = line(7);
        assert_eq!(entry_texts(&l, true), (l.title.clone(), Some(l.detail.clone())));
        assert_eq!(entry_texts(&l, false).0, "Git — repo 7 (Belongs on the Desktop with Term; now on this Desktop)");
    }

    #[test]
    fn the_mark_item_says_when_the_last_mark_went_in() {
        assert_eq!(mark_title(None), "Mark a Problem");
        assert_eq!(mark_title(Some("15:04:05")), "Mark a Problem (last at 15:04:05)");
    }
}
