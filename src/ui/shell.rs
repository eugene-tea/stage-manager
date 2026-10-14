//! The app's shell: everything around window management that a Mac app is expected to do.
//! About, Settings, the welcome, update checks, launch at login, the approvals, and Apple's
//! Stage Manager (stand down, offer to turn it off). Main thread only.
//!
//! The main thread's poll ([`poll`], every second) and config reloads ([`config_changed`])
//! keep it current; menu clicks arrive through [`menu`]. Apple's settings have one source:
//! only the app's poll calls `refresh()` in `native_settings` (it consumes a change); after a
//! write the shell asks for a poll at once ([`Hooks::poll_now`]), so the app, the WM thread,
//! the menu and this window all learn the new value from the same reading. Calls back into the app (the menu
//! bar icon, the menu) go through plain function pointers dispatched after the current
//! event, so they never run inside the app state's borrow.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString, NSWorkspace};
use objc2_foundation::{MainThreadMarker, NSArray, NSBundle, NSString, NSTimer, NSURL};

use super::alert::{self, AlertSpec};
use super::app_menu::{self, AppEvent};
use super::settings_window::{self, Change, SettingsValues};
use super::status_menu::MenuAction;
use super::welcome::{self, WelcomeAction};
use super::about;
use crate::app_state::{AppState, now_secs};
use crate::config::schema::REFERENCE_URL;
use crate::config::{self, Config, FitMode, edit};
use crate::platform::login_item;
use crate::platform::native_settings::{self, RawSettings};
use crate::platform::permissions::{self, Approval, PermissionState};
use crate::platform::wm_thread::{WmCommand, WmHandle};
use crate::platform::{capture, paths, update_check};
use crate::updates::{self, Outcome, Release};

/// The agent notes on GitHub (the "Copy Instructions for Agents" prompt points here).
pub const AGENTS_URL: &str = "https://github.com/eugene-tea/stage-manager/blob/HEAD/docs/AGENTS.md";
/// System Settings → Desktop & Dock (Stage Manager's own settings).
pub const DESKTOP_DOCK_URL: &str = "x-apple.systempreferences:com.apple.Desktop-Settings.extension";
/// The first automatic update check waits this long after launch (not at login's busiest).
const FIRST_CHECK_DELAY: f64 = 30.0;
/// How often an automatic check is considered (it runs when a day has passed).
const CHECK_TICK: f64 = 3600.0;
/// How long a turn-off of Apple's Stage Manager has to show before the fallback.
const TURN_OFF_WAIT: f64 = 2.0;

/// Calls into the app, run after the current event.
#[derive(Clone, Copy)]
pub struct Hooks {
    /// Show or hide the menu bar icon.
    pub set_menu_icon: fn(bool),
    /// Redraw the status menu.
    pub refresh_menu: fn(),
    /// Run the app's poll now (Apple's settings were written).
    pub poll_now: fn(),
}

struct Shell {
    mtm: MainThreadMarker,
    wm: WmHandle,
    hooks: Hooks,
    config: Config,
    config_error: Option<String>,
    state: AppState,
    permissions: PermissionState,
    raw: RawSettings,
    capture_refused: bool,
    /// The update check in flight: its number, and whether a user asked for it (its result
    /// shows then).
    update: Option<(u64, bool)>,
    update_serial: u64,
    _timers: Vec<Retained<NSTimer>>,
}

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Shell) -> R) -> Option<R> {
    SHELL.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// Runs `f` after the current event (outside every borrow).
fn later(f: impl FnOnce() + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(f);
}

/// Whether automatic, unasked things (update checks, the welcome) may happen: not in test
/// runs (`STAGE_MANAGER_NO_PROMPT`, `STAGE_MANAGER_HOME`).
pub fn unattended_allowed() -> bool {
    permissions::prompts_allowed() && paths::custom_home().is_none()
}

/// The config file as shown (`~` for the home folder).
pub fn display_path(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The prompt "Copy Instructions for Agents" puts on the clipboard.
pub fn agent_instructions(config_path: &str, binary: &str) -> String {
    format!(
        "Please change a setting of Stage Manager, the macOS app by eugene-tea, for me.\n\
         \n\
         - Read the notes for agents first: {AGENTS_URL}\n\
         - The config file is {config_path} (TOML; every key is listed there, commented out with its default).\n\
         - Every key with its values and default: {REFERENCE_URL}\n\
         - After editing, check it: \"{binary}\" config check\n\
         - The app reloads the file within a second; do not restart it, edit state.json or move windows.\n\
         \n\
         What I want: "
    )
}

/// The running binary (for the agent prompt).
fn binary_path() -> String {
    NSBundle::mainBundle().executablePath().map_or_else(|| "stage-manager".to_string(), |p| p.to_string())
}

/// Opens a URL with the default handler.
pub fn open_url(url: &str) {
    match NSURL::URLWithString(&NSString::from_str(url)) {
        Some(u) => {
            if !NSWorkspace::sharedWorkspace().openURL(&u) {
                tracing::warn!("could not open {url}");
            }
        }
        None => tracing::warn!("bad URL {url}"),
    }
}

/// Opens config.toml in the default text editor (writing the default file first if missing).
pub fn open_config() {
    let path = paths::config_file();
    if !path.exists() {
        let written = std::fs::create_dir_all(paths::config_dir()).and_then(|_| std::fs::write(&path, config::default_file()));
        if let Err(e) = written {
            tracing::warn!("cannot create {}: {e}", path.display());
            return;
        }
    }
    if let Err(e) = std::process::Command::new("/usr/bin/open").arg("-t").arg(&path).status() {
        tracing::warn!("cannot open {}: {e}", path.display());
    }
}

fn reveal_config() {
    let path = paths::config_file();
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
}

/// The config file's parse, or `None` when it does not load (the last good one stays).
fn read_config() -> Option<Config> {
    let text = std::fs::read_to_string(paths::config_file()).unwrap_or_default();
    Config::parse(&text).ok().map(|p| p.config)
}

/// Starts the shell. Call once the status menu exists.
pub fn install(mtm: MainThreadMarker, wm: WmHandle, hooks: Hooks, permissions: PermissionState, raw: RawSettings) {
    let config = read_config().unwrap_or_default();
    let state = AppState::load(&paths::app_state_file());
    app_menu::install(mtm, |event| match event {
        AppEvent::Reopen => show_settings(),
    });
    let shell = Shell {
        mtm,
        wm,
        hooks,
        config,
        config_error: None,
        state,
        permissions,
        raw,
        capture_refused: false,
        update: None,
        update_serial: 0,
        _timers: Vec::new(),
    };
    SHELL.with(|s| *s.borrow_mut() = Some(shell));
    with(Shell::launch);
}

impl Shell {
    fn capture_on(&self) -> bool {
        self.config.strip.capture && std::env::var_os("STAGE_MANAGER_NO_CAPTURE").is_none()
    }

    fn apple_on(&self) -> bool {
        native_settings::apple_stage_manager_enabled(&self.raw)
    }

    fn save_state(&self) {
        self.state.save(&paths::app_state_file());
    }

    /// The launch: the menu bar icon, the welcome or the Apple prompt, the login item's
    /// first registration, and the update timers.
    fn launch(&mut self) {
        let hooks = self.hooks;
        let icon = self.config.app.show_menu_bar_icon;
        later(move || (hooks.set_menu_icon)(icon));
        let first = !self.state.welcomed;
        let prompts = permissions::prompts_allowed() && paths::custom_home().is_none();
        if welcome::due(first, &self.permissions, self.apple_on(), prompts) {
            tracing::info!(first, accessibility = self.permissions.accessibility, apple = self.apple_on(), "showing the welcome");
            self.show_welcome();
            if first {
                self.state.welcomed = true;
                self.save_state();
            }
        } else if first && !prompts {
            tracing::debug!("first launch in a test run: no welcome");
        }
        if !self.state.login_item_set_up {
            match login_item::refusal() {
                None => {
                    if self.config.app.launch_at_login {
                        let _ = login_item::set(true);
                    }
                    self.state.login_item_set_up = true;
                    self.save_state();
                }
                Some(why) => tracing::debug!("login item not set up: {why}"),
            }
        }
        let first_check = RcBlock::new(|_t: NonNull<NSTimer>| auto_check());
        let tick = RcBlock::new(|_t: NonNull<NSTimer>| auto_check());
        // SAFETY: the blocks run on the main run loop, which keeps the timers; we keep them too.
        unsafe {
            self._timers.push(NSTimer::scheduledTimerWithTimeInterval_repeats_block(FIRST_CHECK_DELAY, false, &first_check));
            self._timers.push(NSTimer::scheduledTimerWithTimeInterval_repeats_block(CHECK_TICK, true, &tick));
        }
    }

    fn welcome_rows(&self, apple_row: bool) -> Vec<welcome::WelcomeRow> {
        welcome::rows(&self.permissions, self.capture_on(), apple_row, self.apple_on())
    }

    fn show_welcome(&self) {
        let rows = self.welcome_rows(self.apple_on());
        welcome::show(self.mtm, rows, |action| {
            later(move || match action {
                WelcomeAction::Allow(a) => allow(a),
                WelcomeAction::TurnOffApple => turn_off_apple(),
                WelcomeAction::OpenSettings => show_settings(),
                WelcomeAction::Done => {}
            })
        });
    }

    fn settings_values(&self) -> SettingsValues {
        let (version, build) = about::versions();
        let version = match build {
            Some(b) => format!("{version} ({b})"),
            None => version,
        };
        SettingsValues::new(
            &self.config,
            &self.raw,
            login_item::status(),
            display_path(&paths::config_file()),
            self.config_error.clone(),
            version,
        )
    }

    fn refresh_settings(&self) {
        if settings_window::is_open() {
            settings_window::apply(self.settings_values());
        }
    }

    /// Writes `path = literal` into config.toml and asks for a reload; a failure says so.
    fn write_config(&mut self, path: &str, literal: &str) {
        match edit::set_in_file(&paths::config_file(), path, literal) {
            Ok(changed) => {
                tracing::info!(key = path, value = literal, changed, "settings: config written");
                if let Some(c) = read_config() {
                    self.config = c;
                }
                self.wm.send(WmCommand::ReloadConfig);
            }
            Err(e) => {
                tracing::warn!(key = path, "settings: cannot write the config: {e}");
                alert::show(
                    self.mtm,
                    AlertSpec {
                        key: "config-write",
                        title: "Stage Manager couldn't save this setting".into(),
                        text: format!("{e}\n\nFix the config file (Settings → Advanced → Open Config File…) and try again."),
                        buttons: vec!["OK".into()],
                        cancel: Some(0),
                        warning: true,
                        activate: true,
                    },
                    |_| {},
                );
            }
        }
        self.refresh_settings();
    }

    /// Writes one of Apple's Stage Manager settings (the user's click in Settings), then
    /// asks the app's poll to read it at once: the one reader (R8 V1).
    fn write_apple(&mut self, key: &str, value: i64) {
        if let Err(e) = native_settings::write(key, value) {
            tracing::warn!(key, "cannot change the macOS setting: {e}");
        }
        let hooks = self.hooks;
        later(move || (hooks.poll_now)());
    }

    fn on_change(&mut self, change: Change) {
        match change {
            Change::LaunchAtLogin(on) => {
                self.write_config("app.launch_at_login", if on { "true" } else { "false" });
                let _ = login_item::set(on);
                self.refresh_settings();
            }
            Change::OpenLoginItems => login_item::open_login_items(),
            Change::MenuBarIcon(on) => {
                self.write_config("app.show_menu_bar_icon", if on { "true" } else { "false" });
                let hooks = self.hooks;
                later(move || (hooks.set_menu_icon)(on));
            }
            Change::AutoUpdates(on) => self.write_config("app.check_for_updates", if on { "true" } else { "false" }),
            Change::CheckNow => later(|| check(true)),
            Change::TurnOffApple => later(turn_off_apple),
            Change::ShowRecentApps(on) => self.write_apple("AutoHide", i64::from(!on)),
            Change::AllAtOnce(all) => self.write_apple("AppWindowGroupingBehavior", i64::from(all)),
            Change::OpenDesktopAndDock => open_url(DESKTOP_DOCK_URL),
            Change::MergeOnDrop(on) => self.write_config("strip.merge_on_drop", if on { "true" } else { "false" }),
            Change::ShiftBringsHere(on) => self.write_config("sugar.shift_brings_here", if on { "true" } else { "false" }),
            Change::Fit(mode) => {
                let v = match mode {
                    FitMode::Off => "\"off\"",
                    FitMode::Move => "\"move\"",
                    FitMode::Narrow => "\"narrow\"",
                };
                self.write_config("windows.fit", v);
            }
            Change::Hotkeys(on) => self.write_config("hotkeys.enabled", if on { "true" } else { "false" }),
            Change::Speed(speed) => self.write_config("animation.speed", &format!("{speed:?}")),
            Change::OpenConfig => open_config(),
            Change::RevealConfig => reveal_config(),
            Change::CopyAgentInstructions => {
                let text = agent_instructions(&paths::config_file().display().to_string(), &binary_path());
                let pb = NSPasteboard::generalPasteboard();
                pb.clearContents();
                // SAFETY: AppKit's own pasteboard type constant.
                pb.setString_forType(&NSString::from_str(&text), unsafe { NSPasteboardTypeString });
                tracing::info!("settings: agent instructions copied");
                settings_window::confirm_copied();
            }
        }
    }
}

/// Shows the Settings window in front (menu, ⌘,, a second launch, the welcome).
pub fn show_settings() {
    let Some((mtm, values)) = with(|s| (s.mtm, s.settings_values())) else { return };
    settings_window::show(mtm, values, |change| {
        later(move || {
            with(|s| s.on_change(change));
        })
    });
}

/// A status menu click the shell handles.
pub fn menu(action: MenuAction) {
    match action {
        MenuAction::About => {
            if let Some(mtm) = MainThreadMarker::new() {
                about::show(mtm);
            }
        }
        MenuAction::CheckForUpdates => check(true),
        MenuAction::Settings => show_settings(),
        MenuAction::Allow(a) => allow(a),
        MenuAction::TurnOffAppleStageManager => turn_off_apple(),
        MenuAction::MarkProblem | MenuAction::Quit => {}
    }
}

/// Asks macOS for an approval and opens its pane (a user's click).
pub fn allow(a: Approval) {
    match a {
        Approval::Accessibility => {
            // Lists the app in the pane (and shows macOS's own prompt), then opens it.
            permissions::check_accessibility(true);
        }
        Approval::ScreenRecording => {
            // Refused captures try again now.
            capture::retry_now();
            permissions::request_screen_recording();
        }
    }
    open_url(a.pane().url());
}

/// The main thread's poll: approvals, Apple's settings, refused captures.
pub fn poll(permissions: PermissionState, raw: &RawSettings, capture_refused: bool) {
    let Some((changed, apple_rows)) = with(|s| {
        let changed = s.permissions != permissions || s.raw != *raw || s.capture_refused != capture_refused;
        if changed {
            s.permissions = permissions;
            s.raw = raw.clone();
            s.capture_refused = capture_refused;
        }
        (changed, welcome::has_apple_row())
    }) else {
        return;
    };
    if changed {
        if welcome::is_open()
            && let Some(rows) = with(|s| s.welcome_rows(apple_rows))
        {
            welcome::update(rows);
        }
        with(|s| s.refresh_settings());
    }
}

/// The config was reloaded (by the WM thread): fresh `[app]` keys and Settings values.
pub fn config_changed(error: Option<String>) {
    let Some(config) = read_config() else {
        with(|s| {
            s.config_error = error;
            s.refresh_settings();
        });
        return;
    };
    with(|s| {
        let before = s.config.app;
        s.config_error = error;
        s.config = config;
        let after = s.config.app;
        if before.show_menu_bar_icon != after.show_menu_bar_icon {
            let hooks = s.hooks;
            later(move || (hooks.set_menu_icon)(after.show_menu_bar_icon));
        }
        if before.launch_at_login != after.launch_at_login && login_item::refusal().is_none() {
            // An edit of the file (an agent's) turns the login item on or off too.
            let _ = login_item::set(after.launch_at_login);
        }
        s.refresh_settings();
    });
}

/// Whether window pictures are on (Screen Recording is asked for), for the menu.
pub fn capture_on() -> bool {
    with(|s| s.capture_on()).unwrap_or(true)
}

/// An automatic update check, when allowed, on, and a day has passed.
fn auto_check() {
    let due = with(|s| s.config.app.check_for_updates && updates::due(s.state.last_update_check, now_secs())).unwrap_or(false);
    if due && unattended_allowed() {
        check(false);
    }
}

/// How long a check may stay unanswered before it counts as failed (NSURLSession's own
/// timeout is 20 s; a completion that never comes must not wedge every later check).
const CHECK_WATCHDOG: Duration = Duration::from_secs(25);

/// Checks GitHub for a newer release; `manual` checks always answer. A manual check while
/// one runs makes that one's result show.
pub fn check(manual: bool) {
    let started = with(|s| match s.update.as_mut() {
        Some((_, asked)) => {
            *asked |= manual;
            None
        }
        None => {
            s.update_serial += 1;
            s.update = Some((s.update_serial, manual));
            Some(s.update_serial)
        }
    })
    .flatten();
    let Some(serial) = started else {
        tracing::debug!(manual, "an update check is already running; its result will show");
        return;
    };
    tracing::info!(manual, "checking for updates");
    update_check::check(move |outcome| later(move || on_checked(serial, outcome)));
    let when = dispatch2::DispatchTime::try_from(CHECK_WATCHDOG).unwrap_or(dispatch2::DispatchTime::NOW);
    let _ = dispatch2::DispatchQueue::main().after(when, move || on_checked(serial, Outcome::Failed("GitHub did not answer".into())));
}

/// The check numbered `serial` ended (its answer, or the watchdog: whichever comes first).
fn on_checked(serial: u64, outcome: Outcome) {
    let Some(Some((mtm, skipped, manual))) = with(|s| {
        let (running, manual) = s.update?;
        if running != serial {
            return None;
        }
        s.update = None;
        if !matches!(outcome, Outcome::Failed(_)) {
            s.state.last_update_check = Some(now_secs());
            s.save_state();
        }
        Some((s.mtm, s.state.skipped_version.clone(), manual))
    }) else {
        return;
    };
    let current = updates::current();
    match outcome {
        Outcome::Available(release) => {
            tracing::info!(version = %release.version, manual, "an update is available");
            if manual || updates::worth_telling(&release, skipped.as_deref()) {
                offer(mtm, release, &current, manual);
            }
        }
        Outcome::UpToDate => {
            tracing::info!(manual, "up to date");
            if manual {
                let (title, text) = updates::up_to_date_text(&current);
                alert::show(mtm, AlertSpec { key: "update", title, text, buttons: vec!["OK".into()], cancel: Some(0), warning: false, activate: true }, |_| {});
            }
        }
        Outcome::Failed(e) => {
            if manual {
                tracing::warn!("update check failed: {e}");
                alert::show(
                    mtm,
                    AlertSpec {
                        key: "update",
                        title: "Stage Manager can't check for updates right now".into(),
                        text: "Check your internet connection and try again later.".into(),
                        buttons: vec!["OK".into()],
                        cancel: Some(0),
                        warning: false,
                        activate: true,
                    },
                    |_| {},
                );
            } else {
                tracing::info!("automatic update check failed: {e}");
            }
        }
    }
}

/// The update alert's spec (also the preview's).
pub fn offer_spec(release: &Release, current: &updates::Version) -> AlertSpec {
    let (title, text) = updates::available_text(release, current);
    AlertSpec {
        key: "update",
        title,
        text,
        buttons: vec!["Download".into(), "Later".into(), "Skip This Version".into()],
        cancel: Some(1),
        warning: false,
        activate: true,
    }
}

/// Offers `release`. A user's check brings the alert to the front; an automatic one never
/// takes the focus from what the user is doing (R8 V2): the alert comes up above other
/// windows without activating the app, and keyboard focus stays where it was.
fn offer(mtm: MainThreadMarker, release: Release, current: &updates::Version, manual: bool) {
    let mut spec = offer_spec(&release, current);
    spec.activate = manual;
    alert::show(mtm, spec, move |button| match button {
        0 => open_url(release.download_url()),
        2 => {
            let v = release.version.to_string();
            tracing::info!(version = %v, "update skipped");
            later(move || {
                with(|s| {
                    s.state.skipped_version = Some(v);
                    s.save_state();
                });
            });
        }
        _ => {}
    });
}

/// Turns Apple's Stage Manager off (the user's click), then checks within about 2 s that it
/// stopped: `GloballyEnabled` reads back 0 and `WindowManager` shows no more windows than
/// before. Otherwise System Settings → Desktop & Dock opens with a one-line hint.
pub fn turn_off_apple() {
    let before = native_settings::window_manager_windows();
    let written = native_settings::turn_off_apple_stage_manager();
    if let Err(e) = &written {
        tracing::warn!("cannot turn Apple's Stage Manager off: {e}");
    }
    let check = RcBlock::new(move |_t: NonNull<NSTimer>| {
        let raw = native_settings::read();
        let off = !native_settings::apple_stage_manager_enabled(&raw);
        let now = native_settings::window_manager_windows();
        let stopped = off && (now == 0 || now < before);
        tracing::info!(off, wm_windows_before = before, wm_windows_now = now, stopped, "turning Apple's Stage Manager off");
        // The app's poll reads the change (the one reader): app, WM thread, menu, windows.
        if let Some(h) = with(|s| s.hooks) {
            later(move || (h.poll_now)());
        }
        if !stopped {
            open_url(DESKTOP_DOCK_URL);
            if let Some(mtm) = MainThreadMarker::new() {
                alert::show(
                    mtm,
                    AlertSpec {
                        key: "apple",
                        title: "Turn off Stage Manager in Desktop & Dock".into(),
                        text: "macOS kept Apple's Stage Manager on. In the Desktop & Dock settings that just opened, turn Stage Manager off.".into(),
                        buttons: vec!["OK".into()],
                        cancel: Some(0),
                        warning: false,
                        activate: true,
                    },
                    |_| {},
                );
            }
        }
    });
    let wait = if written.is_ok() { TURN_OFF_WAIT } else { 0.0 };
    // SAFETY: the block runs once on the main run loop, which keeps the timer.
    let _ = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(wait, false, &check) };
}

/// How long the turn-off waits before judging (for the docs and tests).
pub fn turn_off_wait() -> Duration {
    Duration::from_secs_f64(TURN_OFF_WAIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_prompt_points_at_the_notes_and_the_file() {
        let p = agent_instructions("/Users/me/.config/stage-manager/config.toml", "/Applications/Stage Manager.app/Contents/MacOS/stage-manager");
        assert!(p.contains(AGENTS_URL));
        assert!(p.contains(REFERENCE_URL));
        assert!(p.contains("/Users/me/.config/stage-manager/config.toml"));
        assert!(p.contains("\"/Applications/Stage Manager.app/Contents/MacOS/stage-manager\" config check"));
        assert!(p.contains("state.json"));
        assert!(p.trim_end().ends_with("What I want:"));
    }

    #[test]
    fn paths_show_the_home_folder_as_a_tilde() {
        if let Some(home) = std::env::var_os("HOME") {
            let p = PathBuf::from(home).join(".config/stage-manager/config.toml");
            assert_eq!(display_path(&p), "~/.config/stage-manager/config.toml");
        }
        assert_eq!(display_path(Path::new("/tmp/x.toml")), "/tmp/x.toml");
    }

    /// R8 V1: Apple's settings have one reader, the app's poll. A second caller of
    /// `refresh()` consumes changes the poll then never sees, and stale copies flip
    /// Settings' switches back.
    #[test]
    fn only_the_apps_poll_reads_apples_settings_for_changes() {
        let needle = concat!("native_settings::", "refresh(");
        for (name, text) in [
            ("shell.rs", include_str!("shell.rs")),
            ("settings_window.rs", include_str!("settings_window.rs")),
            ("welcome.rs", include_str!("welcome.rs")),
            ("preview.rs", include_str!("preview.rs")),
            ("status_menu.rs", include_str!("status_menu.rs")),
        ] {
            assert!(!text.contains(needle), "{name} must not call refresh(); ask the app's poll (Hooks::poll_now)");
        }
        let main = include_str!("../main.rs");
        assert_eq!(main.matches(needle).count(), 2, "the poll and the launch's first reading");
    }

    #[test]
    fn the_prompts_say_what_the_owner_asked() {
        let r = updates::parse_release(r#"{"tag_name":"v1.1.0","html_url":"https://github.com/eugene-tea/stage-manager/releases/tag/v1.1.0"}"#).expect("parses");
        let o = offer_spec(&r, &updates::Version::parse("1.0.0").expect("version"));
        assert_eq!(o.buttons, vec!["Download", "Later", "Skip This Version"]);
        assert!(o.text.contains("Stage Manager 1.1.0 is available — you have 1.0.0"));
        assert!(o.activate, "the spec activates; an automatic offer turns it off");
        assert_eq!(turn_off_wait(), Duration::from_secs(2));
    }
}
